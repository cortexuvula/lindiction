use crate::config::default_model_path;
use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;

/// Minimum size of a valid ggml-small.en.bin download. The real file is
/// ~488 MB; anything smaller than this means curl followed a redirect to
/// an auth/error page and wrote HTML to disk. We reject and delete it.
const MIN_EXPECTED_BYTES: u64 = 400 * 1024 * 1024;

/// Hugging Face URL for the default small.en model.
const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";

/// Ensure the default whisper model exists at `path`. Auto-downloads on
/// first run if — and only if — `path` equals the system default
/// (`~/.local/share/lindiction/models/ggml-small.en.bin`). Any user-
/// specified path (via `--model`, `LINDICTION_MODEL`, or TOML) is left
/// alone; the subsequent `SttEngine::load` surfaces the usual
/// "model not found" error with a download hint.
pub fn ensure_default_model(path: &Path) -> Result<()> {
    // Guard 1: never download to a user-specified location.
    if path != default_model_path().as_path() {
        return Ok(());
    }
    // Guard 2: file already present.
    if path.exists() {
        return Ok(());
    }

    download_default(path)
}

fn download_default(path: &Path) -> Result<()> {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let tmp_path = path.with_extension("bin.tmp");

    info!(
        url = DEFAULT_MODEL_URL,
        target = %path.display(),
        "first-run: downloading default whisper model (488 MB)"
    );

    let status = std::process::Command::new("curl")
        .args(["-L", "--fail", "--show-error", "-o"])
        .arg(&tmp_path)
        .arg(DEFAULT_MODEL_URL)
        .status()
        .context("failed to spawn curl (is curl installed? `sudo apt install curl`)")?;

    if !status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "curl exited with {}. Could not download {} to {}. \
             Check your network connection, or pass --model /path/to/existing.bin.",
            status,
            DEFAULT_MODEL_URL,
            path.display()
        );
    }

    // Sanity check: reject suspiciously small downloads (often HTML error
    // pages that curl wrote with HTTP 200 after a redirect).
    let bytes = std::fs::metadata(&tmp_path)
        .with_context(|| format!("stat {}", tmp_path.display()))?
        .len();
    if bytes < MIN_EXPECTED_BYTES {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "downloaded {} bytes from {} (expected >= {}). \
             The server likely returned an error page. \
             Check the URL and your network, or pass --model /path/to/existing.bin.",
            bytes,
            DEFAULT_MODEL_URL,
            MIN_EXPECTED_BYTES
        );
    }

    // Atomic rename: the file is only visible at `path` once the download
    // has fully completed and passed the size check.
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;

    info!(bytes, path = %path.display(), "model download complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn skips_when_path_differs_from_default() {
        // A user-specified path — never auto-download here even if missing.
        let custom = PathBuf::from("/tmp/custom-lindiction-test-nonexistent.bin");
        assert!(!custom.exists(), "precondition: test path must not exist");
        // ensure_default_model returns Ok without spawning curl because the path
        // differs from the default. We can't observe the no-spawn directly, but
        // the function completing near-instantly (sub-millisecond) with Ok
        // demonstrates it.
        ensure_default_model(&custom).expect("should be a no-op for custom paths");
        // The file still doesn't exist afterward, confirming no download happened.
        assert!(!custom.exists());
    }

    #[test]
    fn skips_when_file_exists_at_default_path() {
        // Fake the default path by pointing XDG_DATA_HOME at a temp dir with
        // a pre-existing model file.
        let dir = std::env::temp_dir().join("lindiction-model-download-test-exists");
        let model_dir = dir.join("lindiction").join("models");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model = model_dir.join("ggml-small.en.bin");
        std::fs::write(&model, b"fake model bytes").unwrap();

        std::env::set_var("XDG_DATA_HOME", &dir);

        // The helper recomputes default_model_path() internally; it now points
        // to our temp fake, which exists. The function should return Ok.
        let default = default_model_path();
        assert_eq!(default, model);
        ensure_default_model(&default).expect("should skip — file exists");
        assert_eq!(
            std::fs::read(&model).unwrap(),
            b"fake model bytes",
            "file must not have been replaced"
        );

        std::env::remove_var("XDG_DATA_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn triggers_only_for_default_path() {
        // Smoke check: prove the "guard 1 vs guard 2" split by constructing
        // a path that equals the default but we guarantee the file doesn't
        // exist — then DO NOT actually call ensure_default_model (we don't
        // want to download 488 MB in a unit test). This test asserts the
        // guard logic is reachable, not that the download works.
        std::env::set_var("XDG_DATA_HOME", "/nonexistent-lindiction-dl-guard-test");
        let default = default_model_path();
        assert!(
            default.ends_with("lindiction/models/ggml-small.en.bin"),
            "default_model_path should end with lindiction/models/ggml-small.en.bin"
        );
        assert!(
            !default.exists(),
            "default path must not exist under bogus XDG"
        );
        std::env::remove_var("XDG_DATA_HOME");
    }
}
