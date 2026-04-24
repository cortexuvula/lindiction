pub mod app;
pub mod audio;
pub mod autostart;
pub mod config;
pub mod hotkey;
pub mod hw_detect;
pub mod inject;
pub mod model_choice;
pub mod model_download;
pub mod postprocess;
pub mod preroll;
pub mod replace;
pub mod stt;
pub mod tray;
pub mod update;

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
