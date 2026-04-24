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
}

impl ModelId {
    /// The ggml bin filename that the downloader drops into XDG data dir.
    pub fn filename(self) -> &'static str {
        match self {
            Self::TinyEn => "ggml-tiny.en.bin",
            Self::BaseEn => "ggml-base.en.bin",
            Self::SmallEn => "ggml-small.en.bin",
            Self::MediumEn => "ggml-medium.en.bin",
        }
    }

    /// Canonical download URL on Hugging Face.
    pub fn download_url(self) -> &'static str {
        match self {
            Self::TinyEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
            Self::BaseEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
            Self::SmallEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
            Self::MediumEn => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
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
        }
    }

    /// Approximate runtime RAM footprint: model + whisper working state
    /// + beam-search buffers. Empirically ~2x the on-disk size for CPU
    /// beam_size=5. Used by `recommend` to budget against available RAM.
    pub fn approx_runtime_ram_bytes(self) -> u64 {
        self.approx_download_bytes() * 2
    }

    /// Human-readable tag for logging. Stable — do not change; external
    /// logs may key on these strings.
    pub fn tag(self) -> &'static str {
        match self {
            Self::TinyEn => "tiny.en",
            Self::BaseEn => "base.en",
            Self::SmallEn => "small.en",
            Self::MediumEn => "medium.en",
        }
    }
}

/// Pick the largest model that fits into 60% of the host's available RAM.
/// If hardware detection failed (available_ram_bytes == 0), returns
/// `SmallEn` — the pre-auto-select default — so a detection failure
/// never regresses runtime behavior.
pub fn recommend(hw: &HardwareProfile) -> ModelId {
    if hw.available_ram_bytes == 0 {
        return ModelId::SmallEn;
    }
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

    fn profile_with_available(available: u64) -> HardwareProfile {
        HardwareProfile {
            total_ram_bytes: available,
            available_ram_bytes: available,
            cpu_threads: 4,
            gpu: None,
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
        for m in [ModelId::TinyEn, ModelId::BaseEn, ModelId::SmallEn, ModelId::MediumEn] {
            assert!(m.filename().contains(m.tag()), "filename {} should contain tag {}", m.filename(), m.tag());
        }
    }

    #[test]
    fn download_urls_are_all_https() {
        for m in [ModelId::TinyEn, ModelId::BaseEn, ModelId::SmallEn, ModelId::MediumEn] {
            assert!(m.download_url().starts_with("https://"), "non-HTTPS url: {}", m.download_url());
        }
    }

    #[test]
    fn runtime_ram_larger_than_download() {
        for m in [ModelId::TinyEn, ModelId::BaseEn, ModelId::SmallEn, ModelId::MediumEn] {
            assert!(
                m.approx_runtime_ram_bytes() > m.approx_download_bytes(),
                "runtime RAM must exceed on-disk size for {}",
                m.tag()
            );
        }
    }
}
