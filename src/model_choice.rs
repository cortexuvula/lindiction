//! Decide which Whisper model to run based on a `HardwareProfile`.
//!
//! Intentionally conservative. The budget heuristic (60% of available
//! RAM) leaves room for the rest of the system and avoids swapping
//! under load. `SmallEn` is the safe fallback when detection returns
//! zero (i.e. we couldn't read `/proc/meminfo`) — it's what the daemon
//! shipped with before auto-selection existed, so keeping it as the
//! fallback means a detection failure never regresses the prior
//! behavior.

use crate::hw_detect::HardwareProfile;

/// English-only GGML whisper models we support auto-selecting across.
/// Other model families (multilingual, distil, etc.) are reachable via
/// explicit `--model` / `LINDICTION_MODEL` / `config.toml [model].path`
/// overrides but are not candidates for auto-selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelId {
    TinyEn,
    BaseEn,
    SmallEn,
    MediumEn,
    /// English-optimized "turbo" distillation of large-v3. ~1.6 GB on
    /// disk, ~5 GB runtime RAM on CPU, ~2 GB on GPU. Best English WER
    /// at a fraction of large-v3's compute cost. Only a sensible choice
    /// when the daemon is compiled with a GPU backend AND the detected
    /// GPU has enough VRAM.
    LargeV3Turbo,
}

impl ModelId {
    /// The ggml bin filename that the downloader drops into XDG data dir.
    pub fn filename(self) -> &'static str {
        match self {
            Self::TinyEn => "ggml-tiny.en.bin",
            Self::BaseEn => "ggml-base.en.bin",
            Self::SmallEn => "ggml-small.en.bin",
            Self::MediumEn => "ggml-medium.en.bin",
            Self::LargeV3Turbo => "ggml-large-v3-turbo.bin",
        }
    }

    /// Canonical download URL on Hugging Face.
    pub fn download_url(self) -> &'static str {
        match self {
            Self::TinyEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
            Self::BaseEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
            Self::SmallEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
            Self::MediumEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
            Self::LargeV3Turbo => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        }
    }

    /// Approximate on-disk size (bytes). Used to sanity-check downloads
    /// (reject an HTML error page that curl wrote with 200). Figures are
    /// from current Hugging Face release sizes; they drift ±5% over time.
    pub fn approx_download_bytes(self) -> u64 {
        match self {
            Self::TinyEn => 75_000_000,
            Self::BaseEn => 142_000_000,
            Self::SmallEn => 488_000_000,
            Self::MediumEn => 1_500_000_000,
            Self::LargeV3Turbo => 1_600_000_000,
        }
    }

    /// Approximate runtime RAM footprint: model + whisper working state
    /// + beam-search buffers. Empirically ~2x the on-disk size for CPU
    /// beam_size=5. Used by `recommend` to budget against available RAM.
    pub fn approx_runtime_ram_bytes(self) -> u64 {
        self.approx_download_bytes() * 2
    }

    /// Approximate VRAM footprint when running on a GPU backend.
    /// Noticeably smaller than CPU runtime RAM because GPU kernels
    /// use less scratch memory per token; the working set is
    /// dominated by the model weights plus a small activation
    /// buffer. Used by `recommend` to budget against GPU VRAM.
    pub fn approx_runtime_vram_bytes(self) -> u64 {
        // 1.3× on-disk size covers weights + activations; empirically
        // within the observed footprint for whisper.cpp's CUDA backend.
        (self.approx_download_bytes() as f64 * 1.3) as u64
    }

    /// Human-readable tag for logging. Stable — do not change; external
    /// logs may key on these strings.
    pub fn tag(self) -> &'static str {
        match self {
            Self::TinyEn => "tiny.en",
            Self::BaseEn => "base.en",
            Self::SmallEn => "small.en",
            Self::MediumEn => "medium.en",
            Self::LargeV3Turbo => "large-v3-turbo",
        }
    }
}

/// Pick the largest model that fits the host's current compute budget.
///
/// Path A (GPU): if `hw.gpu` is Some AND its backend tag matches
/// `crate::COMPILED_BACKEND`, budget 80% of `vram_bytes` against each
/// model's `approx_runtime_vram_bytes`. Consider LargeV3Turbo first
/// (best English accuracy), then Medium/Small/Base/Tiny as fallbacks.
///
/// Path B (CPU): otherwise (no GPU, or GPU backend doesn't match
/// what's compiled in), use the prior CPU-RAM logic — 60% of
/// `available_ram_bytes` against `approx_runtime_ram_bytes`.
///
/// Path C (unknown): if both `available_ram_bytes == 0` AND we're not
/// on the GPU path, fall back to `SmallEn` — preserves the
/// pre-auto-select behavior and avoids regressing existing users.
pub fn recommend(hw: &HardwareProfile) -> ModelId {
    // Path A — GPU available and matches the compiled backend.
    if let Some(gpu) = &hw.gpu {
        if gpu.backend.tag() == crate::COMPILED_BACKEND {
            let budget = gpu.vram_bytes * 80 / 100;
            for candidate in [
                ModelId::LargeV3Turbo,
                ModelId::MediumEn,
                ModelId::SmallEn,
                ModelId::BaseEn,
            ] {
                if candidate.approx_runtime_vram_bytes() <= budget {
                    return candidate;
                }
            }
            return ModelId::TinyEn;
        }
    }
    // Path C — no RAM data means no reliable way to budget.
    if hw.available_ram_bytes == 0 {
        return ModelId::SmallEn;
    }
    // Path B — CPU.
    let budget = hw.available_ram_bytes * 60 / 100;
    for candidate in [ModelId::MediumEn, ModelId::SmallEn, ModelId::BaseEn] {
        if candidate.approx_runtime_ram_bytes() <= budget {
            return candidate;
        }
    }
    ModelId::TinyEn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw_detect::{GpuBackend, GpuProfile};

    fn profile_with_available(available: u64) -> HardwareProfile {
        HardwareProfile {
            total_ram_bytes: available,
            available_ram_bytes: available,
            cpu_threads: 4,
            gpu: None,
        }
    }

    fn profile_with_gpu(vram_bytes: u64, backend: GpuBackend) -> HardwareProfile {
        HardwareProfile {
            total_ram_bytes: 16_000_000_000,
            available_ram_bytes: 16_000_000_000,
            cpu_threads: 8,
            gpu: Some(GpuProfile {
                backend,
                device_name: "Test GPU".into(),
                vram_bytes,
            }),
        }
    }

    #[test]
    fn zero_available_falls_back_to_small() {
        // No RAM probe data → keep the pre-auto-select default.
        assert_eq!(
            recommend(&profile_with_available(0)),
            ModelId::SmallEn
        );
    }

    #[test]
    fn sub_base_budget_falls_back_to_tiny() {
        // ~200 MB available → 120 MB budget. tiny.en's ~150 MB runtime
        // doesn't fit either, but it's still the smallest option; use it
        // as the last resort.
        assert_eq!(
            recommend(&profile_with_available(200_000_000)),
            ModelId::TinyEn
        );
    }

    #[test]
    fn modest_system_picks_base() {
        // 512 MB available → 307 MB budget. base.en ~284 MB fits; small
        // ~976 MB does not.
        assert_eq!(
            recommend(&profile_with_available(512_000_000)),
            ModelId::BaseEn
        );
    }

    #[test]
    fn typical_laptop_picks_small() {
        // 4 GB available → 2.4 GB budget. small.en ~976 MB fits, medium
        // ~3 GB does not.
        assert_eq!(
            recommend(&profile_with_available(4_000_000_000)),
            ModelId::SmallEn
        );
    }

    #[test]
    fn beefy_workstation_picks_medium() {
        // 16 GB available → 9.6 GB budget. medium.en ~3 GB comfortably fits.
        assert_eq!(
            recommend(&profile_with_available(16_000_000_000)),
            ModelId::MediumEn
        );
    }

    #[test]
    fn filename_and_tag_are_consistent() {
        // tag() and filename() should reference the same model identity.
        for m in [
            ModelId::TinyEn,
            ModelId::BaseEn,
            ModelId::SmallEn,
            ModelId::MediumEn,
            ModelId::LargeV3Turbo,
        ] {
            assert!(m.filename().contains(m.tag()), "filename {} should contain tag {}", m.filename(), m.tag());
        }
    }

    #[test]
    fn download_urls_are_all_https() {
        for m in [
            ModelId::TinyEn,
            ModelId::BaseEn,
            ModelId::SmallEn,
            ModelId::MediumEn,
            ModelId::LargeV3Turbo,
        ] {
            assert!(m.download_url().starts_with("https://"), "non-HTTPS url: {}", m.download_url());
        }
    }

    #[test]
    fn runtime_ram_larger_than_download() {
        for m in [
            ModelId::TinyEn,
            ModelId::BaseEn,
            ModelId::SmallEn,
            ModelId::MediumEn,
            ModelId::LargeV3Turbo,
        ] {
            assert!(
                m.approx_runtime_ram_bytes() > m.approx_download_bytes(),
                "runtime RAM must exceed on-disk size for {}",
                m.tag()
            );
        }
    }

    #[test]
    fn gpu_path_ignored_when_backend_mismatches_compiled() {
        // On a CPU-only build (COMPILED_BACKEND == "cpu"), a detected CUDA
        // GPU must not pull us onto the GPU path — the daemon can't use the
        // GPU even if it's there. Fall back to CPU-RAM budgeting.
        let hw = profile_with_gpu(24_000_000_000, GpuBackend::Cuda);
        let chosen = recommend(&hw);
        // With 16 GB RAM available and the CPU budget → MediumEn fits.
        // The key assertion is that LargeV3Turbo is NOT chosen (it would
        // be on the GPU path), since CPU budget doesn't accommodate it
        // OR more importantly because we took the CPU path.
        assert_ne!(chosen, ModelId::LargeV3Turbo, "GPU-only model must not be picked on CPU build");
    }

    #[test]
    fn gpu_path_picks_large_when_vram_ample() {
        // This test only passes when cargo test runs with a GPU feature
        // enabled (cuda / vulkan / hipblas) so that COMPILED_BACKEND
        // matches the detected backend. On a default CPU build, this test
        // takes the CPU fallback path, which is fine — the assertion is
        // guarded on COMPILED_BACKEND.
        if crate::COMPILED_BACKEND == "cpu" {
            eprintln!("skipping GPU path test: COMPILED_BACKEND is cpu");
            return;
        }
        let backend = match crate::COMPILED_BACKEND {
            "cuda" => GpuBackend::Cuda,
            "vulkan" => GpuBackend::Vulkan,
            "hipblas" => GpuBackend::Rocm,
            _ => unreachable!("COMPILED_BACKEND const guards this"),
        };
        let hw = profile_with_gpu(24_000_000_000, backend);
        assert_eq!(recommend(&hw), ModelId::LargeV3Turbo);
    }

    #[test]
    fn gpu_path_picks_medium_with_moderate_vram() {
        if crate::COMPILED_BACKEND == "cpu" {
            return;
        }
        let backend = match crate::COMPILED_BACKEND {
            "cuda" => GpuBackend::Cuda,
            "vulkan" => GpuBackend::Vulkan,
            "hipblas" => GpuBackend::Rocm,
            _ => unreachable!(),
        };
        // At 2.5 GB VRAM × 80% = 2.0 GB budget: Medium (~1.95 GB) fits,
        // turbo (~2.08 GB) does not.
        let hw = profile_with_gpu(2_500_000_000, backend);
        assert_eq!(recommend(&hw), ModelId::MediumEn);
    }

    #[test]
    fn large_v3_turbo_metadata() {
        assert!(ModelId::LargeV3Turbo.filename().contains("large-v3-turbo"));
        assert!(ModelId::LargeV3Turbo.download_url().contains("large-v3-turbo"));
        assert_eq!(ModelId::LargeV3Turbo.tag(), "large-v3-turbo");
        assert!(ModelId::LargeV3Turbo.approx_download_bytes() > 1_000_000_000);
        // VRAM runtime is smaller than CPU runtime RAM.
        assert!(
            ModelId::LargeV3Turbo.approx_runtime_vram_bytes()
                < ModelId::LargeV3Turbo.approx_runtime_ram_bytes()
        );
    }

    #[test]
    fn all_models_have_vram_smaller_than_ram() {
        // GPU runs use less scratch memory than CPU for whisper.cpp; a
        // runtime VRAM > runtime RAM for any model would be a config error.
        for m in [
            ModelId::TinyEn,
            ModelId::BaseEn,
            ModelId::SmallEn,
            ModelId::MediumEn,
            ModelId::LargeV3Turbo,
        ] {
            assert!(
                m.approx_runtime_vram_bytes() < m.approx_runtime_ram_bytes(),
                "VRAM estimate must be below RAM estimate for {}",
                m.tag()
            );
        }
    }
}
