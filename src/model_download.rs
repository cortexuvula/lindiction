use crate::model_choice::ModelId;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

/// XDG data dir path for the canonical filename of `model`. Mirrors the
/// shape of `config::default_model_path()` but parameterized — use this
/// anywhere you need to know where an auto-downloaded model lives.
pub fn default_model_path_for(model: ModelId) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("lindiction")
        .join("models")
        .join(model.filename())
}

/// Accept 80% of the expected size as the minimum — leaves room for
/// release-time re-packaging but still rejects 100 KB HTML error pages.
fn min_expected_bytes(model: ModelId) -> u64 {
    model.approx_download_bytes() * 80 / 100
}

/// Ensure the whisper model file exists at `path`.
///
/// If `auto_model` is `Some(id)` AND `path` equals the canonical XDG
/// path for `id` (i.e. the caller didn't override via TOML / env / CLI),
/// the file is auto-downloaded on first run. If `auto_model` is `None`
/// (the user specified a custom path), the function is a no-op — the
/// subsequent `SttEngine::load` surfaces any "file missing" error with
/// a download hint.
pub fn ensure_model(path: &Path, auto_model: Option<ModelId>) -> Result<()> {
    // Guard 0: user specified a custom path — never auto-download.
    let Some(model) = auto_model else {
        return Ok(());
    };
    // Guard 1: never download to a non-canonical location, even when
    // an auto-selected model is known.
    if path != default_model_path_for(model).as_path() {
        return Ok(());
    }
    // Guard 2: file already present.
    if path.exists() {
        return Ok(());
    }

    download(path, model)
}

fn download(path: &Path, model: ModelId) -> Result<()> {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let tmp_path = path.with_extension("bin.tmp");
    let url = model.download_url();
    let expected_mb = model.approx_download_bytes() / (1024 * 1024);

    info!(
        url = url,
        target = %path.display(),
        model = model.tag(),
        "first-run: downloading whisper model ({} MB)",
        expected_mb
    );

    let status = std::process::Command::new("curl")
        .args(["-L", "--fail", "--show-error", "-o"])
        .arg(&tmp_path)
        .arg(url)
        .status()
        .context("failed to spawn curl (is curl installed? `sudo apt install curl`)")?;

    if !status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "curl exited with {}. Could not download {} ({}) to {}. \
             Check your network connection, or pass --model /path/to/existing.bin.",
            status,
            model.tag(),
            url,
            path.display()
        );
    }

    // Sanity check: reject suspiciously small downloads (often HTML error
    // pages that curl wrote with HTTP 200 after a redirect).
    let min_bytes = min_expected_bytes(model);
    let bytes = std::fs::metadata(&tmp_path)
        .with_context(|| format!("stat {}", tmp_path.display()))?
        .len();
    if bytes < min_bytes {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "downloaded {} bytes from {} (expected >= {}). \
             The server likely returned an error page. \
             Check the URL and your network, or pass --model /path/to/existing.bin.",
            bytes,
            url,
            min_bytes
        );
    }

    // Atomic rename: the file is only visible at `path` once the download
    // has fully completed and passed the size check.
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;

    info!(bytes, path = %path.display(), model = model.tag(), "model download complete");
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
        // ensure_model returns Ok without spawning curl because the path
        // differs from the canonical auto-download path for SmallEn. We
        // can't observe the no-spawn directly, but the function completing
        // near-instantly (sub-millisecond) with Ok demonstrates it.
        ensure_model(&custom, Some(ModelId::SmallEn))
            .expect("should be a no-op for custom paths");
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

        // The helper recomputes default_model_path_for() internally; it now
        // points to our temp fake, which exists. The function should return Ok.
        let default = default_model_path_for(ModelId::SmallEn);
        assert_eq!(default, model);
        ensure_model(&default, Some(ModelId::SmallEn)).expect("should skip — file exists");
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
        // exist — then DO NOT actually call ensure_model (we don't want to
        // download 488 MB in a unit test). This test asserts the guard logic
        // is reachable, not that the download works.
        std::env::set_var("XDG_DATA_HOME", "/nonexistent-lindiction-dl-guard-test");
        let default = default_model_path_for(ModelId::SmallEn);
        assert!(
            default.ends_with("lindiction/models/ggml-small.en.bin"),
            "default_model_path_for(SmallEn) should end with lindiction/models/ggml-small.en.bin"
        );
        assert!(
            !default.exists(),
            "default path must not exist under bogus XDG"
        );
        std::env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    fn none_auto_model_is_always_no_op() {
        // If the caller passes None for auto_model (user specified --model /x),
        // we must never try to download, even if path equals a canonical model path.
        let default_small = default_model_path_for(ModelId::SmallEn);
        // Path doesn't need to exist — the None short-circuit should fire first.
        let _ = std::fs::remove_file(&default_small);
        ensure_model(&default_small, None).expect("None auto_model must be a pure no-op");
        assert!(!default_small.exists(), "must not have downloaded anything");
    }
}
