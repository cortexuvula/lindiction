pub mod app;
pub mod audio;
pub mod autostart;
pub mod config;
pub mod hotkey;
pub mod hw_detect;
pub mod inject;
pub mod mic_select;
pub mod model_choice;
pub mod model_download;
pub mod postprocess;
pub mod preroll;
pub mod replace;
pub mod stt;
pub mod tray;
pub mod update;

use crate::hw_detect::HardwareProfile;

/// Which GPU backend was compiled in at build time, if any.
/// Reflects the `cuda` / `vulkan` / `hipblas` Cargo feature flags.
/// Produced at build time so runtime code (hw_detect reconciliation
/// logging) can compare against the detected hardware without re-deriving.
pub const COMPILED_BACKEND: &str = {
    if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else if cfg!(feature = "hipblas") {
        "hipblas"
    } else {
        "cpu"
    }
};

/// Result of comparing the detected GPU against the compiled backend.
/// `&'static` strings are fine because we never mutate these beyond
/// construction; the daemon lifecycle doesn't change backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendReconciliation {
    /// CPU-only build running on a host with no detected GPU — matching.
    CpuBuildNoGpu,
    /// GPU build AND detected GPU backend tag equals COMPILED_BACKEND.
    GpuBuildMatchesGpu,
    /// CPU build, but a usable GPU was detected. Opportunity cost;
    /// daemon still works but misses the GPU. Recommend recompiling
    /// with the right --features flag.
    CpuBuildWithGpu,
    /// GPU build for X, but the detected GPU speaks Y (or no GPU was
    /// detected at all). Daemon still runs via CPU fallback in whisper.cpp
    /// but loaded GPU libs unused.
    GpuBuildWithoutMatchingGpu,
}

/// Compute reconciliation from the compiled-in backend and the probed
/// profile. Pure; no I/O. See `BackendReconciliation` variants for cases.
pub fn reconcile_backend(hw: &HardwareProfile) -> BackendReconciliation {
    let compiled_is_gpu = COMPILED_BACKEND != "cpu";
    let detected_match = hw
        .gpu
        .as_ref()
        .map(|g| g.backend.tag() == COMPILED_BACKEND)
        .unwrap_or(false);
    match (compiled_is_gpu, hw.gpu.as_ref(), detected_match) {
        (false, None, _) => BackendReconciliation::CpuBuildNoGpu,
        (false, Some(_), _) => BackendReconciliation::CpuBuildWithGpu,
        (true, _, true) => BackendReconciliation::GpuBuildMatchesGpu,
        (true, _, false) => BackendReconciliation::GpuBuildWithoutMatchingGpu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw_detect::{GpuBackend, GpuProfile, HardwareProfile};

    fn hw_no_gpu() -> HardwareProfile {
        HardwareProfile {
            total_ram_bytes: 8_000_000_000,
            available_ram_bytes: 8_000_000_000,
            cpu_threads: 4,
            gpu: None,
        }
    }

    fn hw_with_gpu(backend: GpuBackend) -> HardwareProfile {
        HardwareProfile {
            total_ram_bytes: 16_000_000_000,
            available_ram_bytes: 16_000_000_000,
            cpu_threads: 8,
            gpu: Some(GpuProfile {
                backend,
                device_name: "Test GPU".into(),
                vram_bytes: 8_000_000_000,
            }),
        }
    }

    #[test]
    fn cpu_build_no_gpu_reconciles_cleanly() {
        // On a default cargo build, COMPILED_BACKEND == "cpu".
        if COMPILED_BACKEND != "cpu" {
            return;
        } // only meaningful on the default build
        assert_eq!(
            reconcile_backend(&hw_no_gpu()),
            BackendReconciliation::CpuBuildNoGpu
        );
    }

    #[test]
    fn cpu_build_with_gpu_warns() {
        if COMPILED_BACKEND != "cpu" {
            return;
        }
        for backend in [GpuBackend::Cuda, GpuBackend::Vulkan, GpuBackend::Rocm] {
            assert_eq!(
                reconcile_backend(&hw_with_gpu(backend)),
                BackendReconciliation::CpuBuildWithGpu,
                "should warn when GPU detected on CPU build (got backend {:?})",
                backend
            );
        }
    }

    #[test]
    fn gpu_build_matches_gpu() {
        // Reverse case: only meaningful when building with a GPU feature.
        if COMPILED_BACKEND == "cpu" {
            return;
        }
        let backend = match COMPILED_BACKEND {
            "cuda" => GpuBackend::Cuda,
            "vulkan" => GpuBackend::Vulkan,
            "hipblas" => GpuBackend::Rocm,
            _ => unreachable!(),
        };
        assert_eq!(
            reconcile_backend(&hw_with_gpu(backend)),
            BackendReconciliation::GpuBuildMatchesGpu
        );
    }

    #[test]
    fn gpu_build_mismatched_gpu() {
        if COMPILED_BACKEND == "cpu" {
            return;
        }
        let wrong_backend = match COMPILED_BACKEND {
            "cuda" => GpuBackend::Vulkan,
            "vulkan" => GpuBackend::Rocm,
            "hipblas" => GpuBackend::Cuda,
            _ => unreachable!(),
        };
        assert_eq!(
            reconcile_backend(&hw_with_gpu(wrong_backend)),
            BackendReconciliation::GpuBuildWithoutMatchingGpu
        );
    }

    #[test]
    fn gpu_build_no_gpu_detected() {
        if COMPILED_BACKEND == "cpu" {
            return;
        }
        assert_eq!(
            reconcile_backend(&hw_no_gpu()),
            BackendReconciliation::GpuBuildWithoutMatchingGpu
        );
    }
}
