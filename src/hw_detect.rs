//! Hardware capability probing for `model_choice::recommend`.
//!
//! Linux-only (parses `/proc/meminfo`). The main ergonomic concern is
//! that we never panic or bail from here — an unreadable `/proc/meminfo`
//! produces a profile with zero RAM, which `model_choice::recommend`
//! interprets as "use the safe default". Failure to probe must not
//! block the daemon from starting; the config-layer fallback to
//! small.en keeps everything working.

use std::fs;
use tracing::debug;

/// Snapshot of the host's compute capabilities, taken at daemon start.
/// `gpu` is a placeholder for a future GPU-probing step; today it is
/// always `None`.
#[derive(Debug, Clone)]
pub struct HardwareProfile {
    pub total_ram_bytes: u64,
    pub available_ram_bytes: u64,
    pub cpu_threads: usize,
    pub gpu: Option<GpuProfile>,
}

/// Placeholder. Kept as a struct (not just `bool`) so that when GPU
/// support lands, downstream consumers won't need a signature change.
#[derive(Debug, Clone)]
pub struct GpuProfile {
    pub _reserved: (),
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
        gpu: None,
    };
    debug!(?profile, "hardware profile detected");
    profile
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
        // the test runner itself is using at least one thread).
        let p = detect();
        assert!(p.cpu_threads >= 1);
        assert!(p.gpu.is_none()); // placeholder, always None for now
    }
}
