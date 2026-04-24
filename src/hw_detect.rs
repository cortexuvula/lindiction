//! Hardware capability probing for `model_choice::recommend`.
//!
//! Linux-only (parses `/proc/meminfo`). The main ergonomic concern is
//! that we never panic or bail from here — an unreadable `/proc/meminfo`
//! produces a profile with zero RAM, which `model_choice::recommend`
//! interprets as "use the safe default". Failure to probe must not
//! block the daemon from starting; the config-layer fallback to
//! small.en keeps everything working.

use std::fs;
use std::process::Command;
use tracing::debug;

/// Snapshot of the host's compute capabilities, taken at daemon start.
/// `gpu` is `Some` when a GPU probe (nvidia-smi, vulkaninfo, or rocm-smi)
/// succeeded; `None` means no usable GPU was found.
#[derive(Debug, Clone)]
pub struct HardwareProfile {
    pub total_ram_bytes: u64,
    pub available_ram_bytes: u64,
    pub cpu_threads: usize,
    pub gpu: Option<GpuProfile>,
}

/// Which backend API the detected GPU is usable through. Matches the
/// `COMPILED_BACKEND` constant from `lib.rs` so reconciliation logging
/// can string-compare directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    Cuda,
    Vulkan,
    Rocm,
}

impl GpuBackend {
    /// Tag that matches `crate::COMPILED_BACKEND` string values
    /// ("cuda", "vulkan", "hipblas").
    pub fn tag(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            // Note: COMPILED_BACKEND uses "hipblas" (the whisper-rs feature
            // name), not "rocm". Keep them aligned so `probe.backend.tag() ==
            // COMPILED_BACKEND` returns true when they match.
            Self::Rocm => "hipblas",
        }
    }
}

/// Concrete info about a detected GPU. `vram_bytes` is "total" VRAM as
/// reported by the probe; free-VRAM isn't worth probing separately since
/// the daemon is the dominant GPU consumer on a user's workstation.
#[derive(Debug, Clone)]
pub struct GpuProfile {
    pub backend: GpuBackend,
    pub device_name: String,
    pub vram_bytes: u64,
}

/// Probe the host. Never panics; fields fall back to 0 / 1 on unknown.
pub fn detect() -> HardwareProfile {
    let (total_ram_bytes, available_ram_bytes) =
        parse_proc_meminfo().unwrap_or((0, 0));
    let cpu_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let profile = HardwareProfile {
        total_ram_bytes,
        available_ram_bytes,
        cpu_threads,
        gpu: detect_gpu(),
    };
    debug!(?profile, "hardware profile detected");
    profile
}

/// Try each GPU probe in order: CUDA (nvidia-smi), Vulkan (vulkaninfo),
/// ROCm (rocm-smi). Return the first success. This order matters when
/// a host has multiple backends available (e.g., NVIDIA with the Vulkan
/// ICD installed) — CUDA wins because NVIDIA's own tooling gives the
/// most reliable info.
fn detect_gpu() -> Option<GpuProfile> {
    probe_nvidia_smi()
        .or_else(probe_vulkaninfo)
        .or_else(probe_rocm_smi)
}

/// Probe via `nvidia-smi`. Returns None if the tool isn't installed,
/// exits non-zero, or prints unparseable output.
fn probe_nvidia_smi() -> Option<GpuProfile> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&out.stdout).ok()?;
    parse_nvidia_smi(stdout)
}

/// Parse `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`
/// output. Takes the first line (first GPU) only. `memory.total` is in MiB
/// because we passed `nounits`; multiply by 1 MiB for bytes.
fn parse_nvidia_smi(stdout: &str) -> Option<GpuProfile> {
    let first = stdout.lines().find(|l| !l.trim().is_empty())?;
    let (name, mib) = first.split_once(',')?;
    let name = name.trim();
    let mib: u64 = mib.trim().parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some(GpuProfile {
        backend: GpuBackend::Cuda,
        device_name: name.to_string(),
        vram_bytes: mib * 1024 * 1024,
    })
}

/// Probe via `vulkaninfo --summary`. Returns None if the tool isn't
/// installed, exits non-zero, or prints unparseable output.
fn probe_vulkaninfo() -> Option<GpuProfile> {
    let out = Command::new("vulkaninfo").arg("--summary").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&out.stdout).ok()?;
    parse_vulkaninfo_summary(stdout)
}

/// Parse `vulkaninfo --summary` output. Skips any software rasterizer
/// devices (PHYSICAL_DEVICE_TYPE_CPU, e.g., lavapipe) and returns the
/// first real GPU.
///
/// Limitation: `--summary` does not report memory heap sizes, so we use
/// a conservative 4 GiB default for `vram_bytes`. Parsing the full
/// (non-summary) `vulkaninfo` output would give exact heap sizes but at
/// the cost of ~3000 lines of text to scan; for model-selection purposes
/// the conservative default is enough.
fn parse_vulkaninfo_summary(stdout: &str) -> Option<GpuProfile> {
    const CONSERVATIVE_VRAM: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

    // Split into device blocks by `GPU<n>:` headers. A line that begins
    // (after trimming) with `GPU` and ends with `:` and whose middle is
    // all digits starts a new block.
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in stdout.lines() {
        if is_gpu_header(line) {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            current = Some(Vec::new());
            continue;
        }
        if let Some(block) = current.as_mut() {
            block.push(line);
        }
    }
    if let Some(block) = current.take() {
        blocks.push(block);
    }

    for block in blocks {
        let mut device_type: Option<&str> = None;
        let mut device_name: Option<String> = None;
        for line in &block {
            if let Some(v) = extract_kv(line, "deviceType") {
                device_type = Some(v);
            } else if let Some(v) = extract_kv(line, "deviceName") {
                device_name = Some(v.to_string());
            }
        }
        let dtype = device_type?;
        if dtype == "PHYSICAL_DEVICE_TYPE_CPU" {
            continue;
        }
        let name = device_name?;
        return Some(GpuProfile {
            backend: GpuBackend::Vulkan,
            device_name: name,
            vram_bytes: CONSERVATIVE_VRAM,
        });
    }
    None
}

/// Returns true for a `GPU<digits>:` header line (after trim).
fn is_gpu_header(line: &str) -> bool {
    let t = line.trim();
    let Some(rest) = t.strip_prefix("GPU") else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(':') else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// Extracts the RHS of a `key = value` line (with any indentation).
/// Returns None if the key doesn't match or the `=` is missing.
fn extract_kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let t = line.trim_start();
    let rest = t.strip_prefix(key)?;
    // Require that the next non-whitespace char sequence starts with `=`.
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    Some(rest.trim())
}

/// Probe via `rocm-smi --showproductname --showmeminfo vram --csv`.
/// Returns None if the tool isn't installed, exits non-zero, or prints
/// unparseable output. Requires rocm-smi 5.0+ for the CSV format;
/// older plaintext output is treated as a parse failure.
fn probe_rocm_smi() -> Option<GpuProfile> {
    let out = Command::new("rocm-smi")
        .args(["--showproductname", "--showmeminfo", "vram", "--csv"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&out.stdout).ok()?;
    parse_rocm_smi_csv(stdout)
}

/// Parse rocm-smi CSV output. Header row is expected; the first data
/// row yields the GPU info. Column positions are resolved from the
/// header so the parser tolerates minor column-order changes.
fn parse_rocm_smi_csv(stdout: &str) -> Option<GpuProfile> {
    let mut lines = stdout.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next()?;
    let cols: Vec<&str> = header.split(',').map(|s| s.trim()).collect();
    let name_idx = cols.iter().position(|c| *c == "Card series")?;
    let vram_idx = cols
        .iter()
        .position(|c| *c == "VRAM Total Memory (B)")?;
    let data = lines.next()?;
    let cells: Vec<&str> = data.split(',').map(|s| s.trim()).collect();
    let name = cells.get(name_idx)?.to_string();
    let vram: u64 = cells.get(vram_idx)?.parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some(GpuProfile {
        backend: GpuBackend::Rocm,
        device_name: name,
        vram_bytes: vram,
    })
}

fn parse_proc_meminfo() -> Option<(u64, u64)> {
    let s = fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_str(&s)
}

/// Extracted for testability. Returns (total, available) in bytes.
/// Both lines must be present, in either order.
fn parse_meminfo_str(s: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut available = None;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse_kb_line(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = parse_kb_line(rest);
        }
    }
    Some((total?, available?))
}

/// Parse a `/proc/meminfo` value line like `"       12345 kB"`. Returns
/// bytes. Only `kB` is handled because that's the only unit `/proc/meminfo`
/// actually uses; anything else returns None.
fn parse_kb_line(s: &str) -> Option<u64> {
    let trimmed = s.trim();
    let kb_str = trimmed.strip_suffix("kB")?.trim();
    kb_str.parse::<u64>().ok().map(|kb| kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_representative_meminfo_sample() {
        // Abridged real /proc/meminfo. Mixed-whitespace is intentional.
        let sample = "\
MemTotal:       16384000 kB
MemFree:         2048000 kB
MemAvailable:   12288000 kB
Buffers:          256000 kB
Cached:          4096000 kB
";
        let (total, avail) = parse_meminfo_str(sample).expect("must parse");
        assert_eq!(total, 16_384_000 * 1024);
        assert_eq!(avail, 12_288_000 * 1024);
    }

    #[test]
    fn missing_memavailable_rejects() {
        let sample = "\
MemTotal:       16384000 kB
MemFree:         2048000 kB
";
        assert!(parse_meminfo_str(sample).is_none());
    }

    #[test]
    fn missing_memtotal_rejects() {
        let sample = "MemAvailable:   12288000 kB\n";
        assert!(parse_meminfo_str(sample).is_none());
    }

    #[test]
    fn empty_input_rejects() {
        assert!(parse_meminfo_str("").is_none());
    }

    #[test]
    fn parse_kb_line_handles_various_spacing() {
        assert_eq!(parse_kb_line("   12345 kB").unwrap(), 12_345 * 1024);
        assert_eq!(parse_kb_line("0 kB").unwrap(), 0);
        assert_eq!(parse_kb_line("\t100 kB\t").unwrap(), 100 * 1024);
    }

    #[test]
    fn parse_kb_line_rejects_unknown_units() {
        assert!(parse_kb_line("   12345 MB").is_none());
        assert!(parse_kb_line("12345").is_none());
    }

    #[test]
    fn detect_never_panics_and_returns_something() {
        // On Linux this should populate real values; on any OS it must
        // at least not panic and must return non-zero cpu_threads (since
        // the test runner itself is using at least one thread). The `gpu`
        // field may be Some or None depending on host — don't assert it.
        let p = detect();
        assert!(p.cpu_threads >= 1);
    }

    #[test]
    fn parses_nvidia_smi_single_gpu() {
        let out = "NVIDIA GeForce RTX 3060, 12288\n";
        let p = parse_nvidia_smi(out).expect("must parse");
        assert_eq!(p.backend, GpuBackend::Cuda);
        assert_eq!(p.device_name, "NVIDIA GeForce RTX 3060");
        assert_eq!(p.vram_bytes, 12288 * 1024 * 1024);
    }

    #[test]
    fn parses_nvidia_smi_takes_first_gpu() {
        let out = "\
NVIDIA GeForce RTX 3060, 12288
Tesla T4, 15360
";
        let p = parse_nvidia_smi(out).expect("must parse");
        assert_eq!(p.device_name, "NVIDIA GeForce RTX 3060");
        assert_eq!(p.vram_bytes, 12288 * 1024 * 1024);
    }

    #[test]
    fn nvidia_smi_empty_output_rejects() {
        assert!(parse_nvidia_smi("").is_none());
    }

    #[test]
    fn nvidia_smi_malformed_row_rejects() {
        assert!(parse_nvidia_smi("just some garbage with no commas").is_none());
        assert!(parse_nvidia_smi("RTX, not-a-number\n").is_none());
    }

    #[test]
    fn parses_vulkaninfo_summary_discrete_gpu() {
        // Abridged real --summary output. Key structural bits: GPU0: header,
        // indented key=value lines, deviceName, deviceType.
        let out = "\
==========
VULKANINFO
==========

Instance Extensions:
====================
... (omitted)

Devices:
========
GPU0:
    apiVersion         = 1.3.260
    driverVersion      = 535.104.12.0
    vendorID           = 0x10DE
    deviceID           = 0x2504
    deviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
    deviceName         = NVIDIA GeForce RTX 3060
    driverID           = DRIVER_ID_NVIDIA_PROPRIETARY
";
        let p = parse_vulkaninfo_summary(out).expect("must parse");
        assert_eq!(p.backend, GpuBackend::Vulkan);
        assert_eq!(p.device_name, "NVIDIA GeForce RTX 3060");
        assert_eq!(p.vram_bytes, 4 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parses_vulkaninfo_summary_skips_cpu_rasterizer() {
        // lavapipe (software Vulkan) reports as PHYSICAL_DEVICE_TYPE_CPU.
        // We should skip it and try the next GPU. If the only device is a
        // CPU rasterizer, return None — no real GPU.
        let cpu_only = "\
Devices:
========
GPU0:
    deviceType         = PHYSICAL_DEVICE_TYPE_CPU
    deviceName         = llvmpipe (LLVM 15.0.6, 256 bits)
";
        assert!(parse_vulkaninfo_summary(cpu_only).is_none());

        let cpu_then_gpu = "\
Devices:
========
GPU0:
    deviceType         = PHYSICAL_DEVICE_TYPE_CPU
    deviceName         = llvmpipe
GPU1:
    deviceType         = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU
    deviceName         = Intel(R) Iris(R) Xe Graphics
";
        let p = parse_vulkaninfo_summary(cpu_then_gpu).expect("must skip CPU and pick GPU");
        assert_eq!(p.device_name, "Intel(R) Iris(R) Xe Graphics");
    }

    #[test]
    fn vulkaninfo_empty_output_rejects() {
        assert!(parse_vulkaninfo_summary("").is_none());
    }

    #[test]
    fn parses_rocm_smi_csv() {
        let out = "\
device,Card series,VRAM Total Memory (B),VRAM Total Used Memory (B)
card0,Navi 22 [Radeon RX 6700 XT],12884901888,1234567
";
        let p = parse_rocm_smi_csv(out).expect("must parse");
        assert_eq!(p.backend, GpuBackend::Rocm);
        assert_eq!(p.device_name, "Navi 22 [Radeon RX 6700 XT]");
        assert_eq!(p.vram_bytes, 12_884_901_888);
    }

    #[test]
    fn rocm_smi_header_only_rejects() {
        // No data rows after the header → no GPU to report.
        let out = "device,Card series,VRAM Total Memory (B),VRAM Total Used Memory (B)\n";
        assert!(parse_rocm_smi_csv(out).is_none());
    }

    #[test]
    fn rocm_smi_empty_rejects() {
        assert!(parse_rocm_smi_csv("").is_none());
    }

    #[test]
    fn gpu_backend_tag_matches_compiled_backend() {
        // The tag() values must align with COMPILED_BACKEND's string values
        // so reconciliation logging (Task 14) can string-compare directly.
        assert_eq!(GpuBackend::Cuda.tag(), "cuda");
        assert_eq!(GpuBackend::Vulkan.tag(), "vulkan");
        assert_eq!(GpuBackend::Rocm.tag(), "hipblas"); // whisper-rs feature name
    }
}
