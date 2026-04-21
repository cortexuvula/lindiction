//! Auto-update from GitHub Releases.
//!
//! Queries `releases/latest`, parses the asset list, verifies SHA256, and
//! installs either via `pkexec apt install` (when the binary lives in a
//! system path and was installed from a .deb) or via an atomic in-place
//! rename (when the binary is user-writable in ~/.cargo/bin or similar).
//!
//! # Trust model
//!
//! - HTTPS + GitHub's integrity for transport.
//! - SHA256 sidecar catches bit-rot at rest; does NOT defend against a
//!   compromised GitHub account (the attacker would control the sha256
//!   file too). Real integrity needs GPG signing, deferred to a follow-up.
//! - Users who want to opt out entirely should set `[update] enabled = false`.
//!
//! # Shell-out philosophy
//!
//! Per CLAUDE.md, we intentionally shell out rather than pull in a Rust
//! HTTP/archive stack: `curl` for download, `sha256sum` for verification,
//! `tar` for extraction, `pkexec apt install` for privilege-escalated
//! install. Same pattern as `model_download.rs` and the injector chain.

use anyhow::{anyhow, Context, Result};
use semver::Version;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

const GITHUB_API_URL: &str =
    "https://api.github.com/repos/cortexuvula/lindiction/releases/latest";

fn user_agent() -> String {
    // GitHub API requires a User-Agent header. Identify ourselves so any
    // future rate-limit diagnostics from GitHub's side can see what hit them.
    format!("lindiction/{}", env!("CARGO_PKG_VERSION"))
}

/// Details of an available release that the current daemon is behind.
/// Produced by `check()` and consumed by `install()`.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current: Version,
    pub latest: Version,
    pub tag_name: String,
    pub html_url: String,
    pub deb_url: String,
    /// May be absent on releases published before v0.6.0 — handle gracefully
    /// by skipping .deb integrity verification with a warn log.
    pub deb_sha256_url: Option<String>,
    pub tarball_url: String,
    pub tarball_sha256_url: String,
}

/// How the running binary was installed. Drives which update strategy
/// `install()` uses, or whether to refuse (DevBuild).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// `/usr/bin/...` or `/opt/...` — needs root to overwrite.
    Deb,
    /// `~/.cargo/bin/...`, `~/.local/bin/...`, or `~/bin/...` — user-writable.
    Tarball,
    /// `target/release/...` or anywhere else — refuse to auto-update.
    DevBuild,
}

/// Resolve the current binary's install style. Returns `DevBuild` if the
/// path can't be determined — better to refuse than update the wrong file.
pub fn detect_install_method() -> InstallMethod {
    match std::env::current_exe() {
        Ok(p) => detect_install_method_for(&p),
        Err(_) => InstallMethod::DevBuild,
    }
}

fn detect_install_method_for(exe: &Path) -> InstallMethod {
    let s = exe.to_string_lossy();
    if s.starts_with("/usr/") || s.starts_with("/opt/") {
        return InstallMethod::Deb;
    }
    if let Some(home) = dirs::home_dir() {
        let home_s = home.to_string_lossy();
        // Three common user-local install locations. `.cargo/bin` is the
        // cargo-install target; `.local/bin` is XDG user bin; `~/bin` is
        // the classic POSIX user-bin path.
        let user_prefixes = [
            format!("{}/.cargo/bin/", home_s),
            format!("{}/.local/bin/", home_s),
            format!("{}/bin/", home_s),
        ];
        for prefix in &user_prefixes {
            if s.starts_with(prefix.as_str()) {
                return InstallMethod::Tarball;
            }
        }
    }
    InstallMethod::DevBuild
}

/// Query GitHub's releases API for the latest tag and compare against
/// our compiled-in version.
///
/// Return values:
/// - `Ok(Some(info))` — an update is available.
/// - `Ok(None)` — we're already on the latest (or newer, e.g. a dev build
///   with a bumped Cargo.toml that hasn't shipped yet).
/// - `Err(...)` — network or parse failure. Callers on periodic ticks
///   should log at debug and move on; callers on user-triggered checks
///   should surface the error to the user.
pub async fn check() -> Result<Option<UpdateInfo>> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("parsing current package version")?;
    let raw = fetch_latest_release_json().await?;
    let parsed: GithubRelease = serde_json::from_str(&raw)
        .context("parsing GitHub release JSON")?;
    build_update_info(current, parsed)
}

fn build_update_info(current: Version, parsed: GithubRelease) -> Result<Option<UpdateInfo>> {
    let latest_str = parsed.tag_name.trim_start_matches('v');
    let latest = Version::parse(latest_str)
        .with_context(|| format!("parsing latest tag `{}`", parsed.tag_name))?;
    if latest <= current {
        debug!(%current, %latest, "no update available");
        return Ok(None);
    }
    // Find the four assets we need. SHA256 sidecars are distinguished by
    // the `.sha256` suffix; exclude those from the primary-asset lookups.
    let find_primary = |suffix: &str| -> Option<&GithubAsset> {
        parsed
            .assets
            .iter()
            .find(|a| a.name.ends_with(suffix) && !a.name.ends_with(".sha256"))
    };
    let find_sha = |suffix: &str| -> Option<&GithubAsset> {
        let combined = format!("{suffix}.sha256");
        parsed.assets.iter().find(|a| a.name.ends_with(&combined))
    };
    let deb = find_primary("-amd64.deb")
        .ok_or_else(|| anyhow!("release {} is missing a .deb asset", parsed.tag_name))?;
    let tarball = find_primary("-x86_64-linux.tar.gz")
        .ok_or_else(|| anyhow!("release {} is missing a tarball asset", parsed.tag_name))?;
    let tarball_sha = find_sha("-x86_64-linux.tar.gz").ok_or_else(|| {
        anyhow!("release {} is missing a tarball .sha256 asset", parsed.tag_name)
    })?;
    let deb_sha = find_sha("-amd64.deb");
    Ok(Some(UpdateInfo {
        current,
        latest,
        tag_name: parsed.tag_name,
        html_url: parsed.html_url,
        deb_url: deb.browser_download_url.clone(),
        deb_sha256_url: deb_sha.map(|a| a.browser_download_url.clone()),
        tarball_url: tarball.browser_download_url.clone(),
        tarball_sha256_url: tarball_sha.browser_download_url.clone(),
    }))
}

async fn fetch_latest_release_json() -> Result<String> {
    let ua = user_agent();
    let output = tokio::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail", // non-zero exit on HTTP 4xx/5xx
            "--location",
            "--max-time",
            "15",
            "-H",
            &format!("User-Agent: {ua}"),
            "-H",
            "Accept: application/vnd.github+json",
            GITHUB_API_URL,
        ])
        .output()
        .await
        .context("spawning curl for GitHub API")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "GitHub API call failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    String::from_utf8(output.stdout).context("GitHub API response was not UTF-8")
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Install the new release. On success, the caller should trigger a
/// process Restart so the newly-installed binary starts running.
pub async fn install(info: &UpdateInfo) -> Result<()> {
    match detect_install_method() {
        InstallMethod::DevBuild => Err(anyhow!(
            "running a development build; auto-update is refused. \
             Rebuild from git, or install from the .deb on the release page."
        )),
        InstallMethod::Deb => install_deb(info).await,
        InstallMethod::Tarball => install_tarball(info).await,
    }
}

async fn install_deb(info: &UpdateInfo) -> Result<()> {
    let tmp = make_tmp_dir()?;
    let deb_filename = format!("lindiction-{}-amd64.deb", info.tag_name);
    let deb_path = tmp.join(&deb_filename);
    download(&info.deb_url, &deb_path).await?;
    if let Some(sha_url) = &info.deb_sha256_url {
        let sha_filename = format!("{deb_filename}.sha256");
        download(sha_url, &tmp.join(&sha_filename)).await?;
        verify_sha256(&tmp, &sha_filename)?;
    } else {
        warn!(
            tag = %info.tag_name,
            "release does not publish .deb SHA256; skipping integrity check"
        );
    }
    info!(path = %deb_path.display(), "running pkexec apt install");
    // pkexec shows a polkit dialog with the full command so the user can
    // see exactly what they're authorizing. -y keeps apt non-interactive
    // since we can't relay its prompts through the dialog.
    let status = tokio::process::Command::new("pkexec")
        .arg("apt")
        .arg("install")
        .arg("-y")
        .arg(&deb_path)
        .status()
        .await
        .context("spawning pkexec")?;
    if !status.success() {
        return Err(anyhow!(
            "pkexec apt install exited with {status} — dialog denied, \
             apt lock held, or dependency conflict"
        ));
    }
    cleanup_tmp(&tmp);
    Ok(())
}

async fn install_tarball(info: &UpdateInfo) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = make_tmp_dir()?;
    let tarball_filename = format!("lindiction-{}-x86_64-linux.tar.gz", info.tag_name);
    let sha_filename = format!("{tarball_filename}.sha256");
    let tarball_path = tmp.join(&tarball_filename);
    let sha_path = tmp.join(&sha_filename);
    download(&info.tarball_url, &tarball_path).await?;
    download(&info.tarball_sha256_url, &sha_path).await?;
    verify_sha256(&tmp, &sha_filename)?;

    // Extract. The release tarball is a flat archive containing only the
    // `lindiction` binary at its root.
    let status = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball_path)
        .arg("-C")
        .arg(&tmp)
        .status()
        .await
        .context("spawning tar")?;
    if !status.success() {
        return Err(anyhow!("tar exited with {status}"));
    }
    let extracted = tmp.join("lindiction");
    if !extracted.is_file() {
        return Err(anyhow!(
            "tarball did not contain a top-level `lindiction` binary"
        ));
    }

    // Stage the new binary in the SAME directory as the current one so
    // the final rename is atomic (same-filesystem guarantee). Then rename
    // over the original. If the parent directory is not user-writable,
    // we'd have been routed to Deb install instead — so permission here
    // should always succeed.
    let exe =
        std::env::current_exe().context("resolving current binary path for rename target")?;
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow!("current binary {} has no parent directory", exe.display()))?;
    let staging = parent.join(format!(".lindiction-update-{}.tmp", std::process::id()));
    std::fs::copy(&extracted, &staging)
        .with_context(|| format!("copying new binary to {}", staging.display()))?;
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("setting +x on {}", staging.display()))?;
    // Atomic swap. At this point a crash leaves either the old or the new
    // binary in place — never a truncated file. `rename` is guaranteed
    // atomic within a single filesystem.
    std::fs::rename(&staging, &exe).with_context(|| {
        format!(
            "renaming {} -> {}; leaving staging file in place for inspection",
            staging.display(),
            exe.display()
        )
    })?;
    info!(path = %exe.display(), "tarball install complete");
    cleanup_tmp(&tmp);
    Ok(())
}

fn make_tmp_dir() -> Result<PathBuf> {
    let p = std::env::temp_dir().join(format!("lindiction-update-{}", std::process::id()));
    // Remove-and-recreate: if a previous update attempt in the same daemon
    // session left debris, we don't want sha256sum -c to find the old file
    // and silently "verify" stale bytes.
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
    Ok(p)
}

fn cleanup_tmp(p: &Path) {
    if let Err(e) = std::fs::remove_dir_all(p) {
        warn!(error = %e, path = %p.display(), "failed to clean up update temp dir");
    }
}

async fn download(url: &str, dest: &Path) -> Result<()> {
    let ua = user_agent();
    let dest_str = dest
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF-8 destination path: {}", dest.display()))?;
    let status = tokio::process::Command::new("curl")
        .args([
            "-L", // follow redirects (GitHub redirects release assets to S3)
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "300", // 5 min ceiling for slow links downloading ~2 MB
            "-H",
            &format!("User-Agent: {ua}"),
            "-o",
            dest_str,
            url,
        ])
        .status()
        .await
        .context("spawning curl for download")?;
    if !status.success() {
        return Err(anyhow!("curl failed downloading {url}: exit {status}"));
    }
    Ok(())
}

/// Verify a SHA256 sidecar against its referenced file. The sidecar is
/// produced by `sha256sum FILENAME > FILENAME.sha256` in the release
/// workflow, so it contains a relative filename. We `cd` to the directory
/// containing both files so the lookup resolves.
fn verify_sha256(dir: &Path, sha_filename: &str) -> Result<()> {
    let status = std::process::Command::new("sha256sum")
        .arg("-c")
        .arg(sha_filename)
        .current_dir(dir)
        .status()
        .context("spawning sha256sum")?;
    if !status.success() {
        return Err(anyhow!(
            "SHA256 mismatch on {}; refusing to install",
            sha_filename
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_deb_from_system_paths() {
        assert_eq!(
            detect_install_method_for(Path::new("/usr/bin/lindiction")),
            InstallMethod::Deb
        );
        assert_eq!(
            detect_install_method_for(Path::new("/usr/local/bin/lindiction")),
            InstallMethod::Deb
        );
        assert_eq!(
            detect_install_method_for(Path::new("/opt/lindiction/lindiction")),
            InstallMethod::Deb
        );
    }

    #[test]
    fn detect_tarball_from_user_paths() {
        let Some(home) = dirs::home_dir() else {
            // No HOME in the test env — skip rather than fail.
            return;
        };
        for sub in [".cargo/bin", ".local/bin", "bin"] {
            let p = home.join(sub).join("lindiction");
            assert_eq!(
                detect_install_method_for(&p),
                InstallMethod::Tarball,
                "expected Tarball for {}",
                p.display()
            );
        }
    }

    #[test]
    fn detect_devbuild_from_target_release() {
        assert_eq!(
            detect_install_method_for(Path::new("/home/alice/proj/target/release/lindiction")),
            InstallMethod::DevBuild
        );
        assert_eq!(
            detect_install_method_for(Path::new("/tmp/random/lindiction")),
            InstallMethod::DevBuild
        );
    }

    #[test]
    fn parses_github_release_json_with_all_four_assets() {
        let json = r#"{
            "tag_name": "v0.6.0",
            "html_url": "https://github.com/cortexuvula/lindiction/releases/tag/v0.6.0",
            "assets": [
                {"name": "lindiction-v0.6.0-x86_64-linux.tar.gz",        "browser_download_url": "https://example.test/tarball"},
                {"name": "lindiction-v0.6.0-x86_64-linux.tar.gz.sha256", "browser_download_url": "https://example.test/tarball.sha256"},
                {"name": "lindiction-v0.6.0-amd64.deb",                  "browser_download_url": "https://example.test/deb"},
                {"name": "lindiction-v0.6.0-amd64.deb.sha256",           "browser_download_url": "https://example.test/deb.sha256"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let info = build_update_info(Version::parse("0.5.0").unwrap(), parsed)
            .unwrap()
            .expect("0.5.0 should see 0.6.0 as newer");
        assert_eq!(info.latest, Version::parse("0.6.0").unwrap());
        assert_eq!(info.tag_name, "v0.6.0");
        assert_eq!(info.deb_url, "https://example.test/deb");
        assert_eq!(
            info.deb_sha256_url.as_deref(),
            Some("https://example.test/deb.sha256")
        );
        assert_eq!(info.tarball_url, "https://example.test/tarball");
        assert_eq!(info.tarball_sha256_url, "https://example.test/tarball.sha256");
    }

    #[test]
    fn parses_legacy_release_without_deb_sha256() {
        // Releases published before v0.6.0 don't have a .deb.sha256. We
        // must still accept them and return None for the sha URL.
        let json = r#"{
            "tag_name": "v0.5.0",
            "html_url": "https://github.com/x/y/releases/tag/v0.5.0",
            "assets": [
                {"name": "lindiction-v0.5.0-x86_64-linux.tar.gz",        "browser_download_url": "https://example.test/tarball"},
                {"name": "lindiction-v0.5.0-x86_64-linux.tar.gz.sha256", "browser_download_url": "https://example.test/tarball.sha256"},
                {"name": "lindiction-v0.5.0-amd64.deb",                  "browser_download_url": "https://example.test/deb"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let info = build_update_info(Version::parse("0.4.0").unwrap(), parsed)
            .unwrap()
            .expect("0.4.0 should see 0.5.0 as newer");
        assert_eq!(info.deb_sha256_url, None);
    }

    #[test]
    fn same_version_returns_no_update() {
        let json = r#"{
            "tag_name": "v0.5.0",
            "html_url": "https://x",
            "assets": [
                {"name": "lindiction-v0.5.0-x86_64-linux.tar.gz",        "browser_download_url": "u"},
                {"name": "lindiction-v0.5.0-x86_64-linux.tar.gz.sha256", "browser_download_url": "u"},
                {"name": "lindiction-v0.5.0-amd64.deb",                  "browser_download_url": "u"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let res = build_update_info(Version::parse("0.5.0").unwrap(), parsed).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn newer_local_version_returns_no_update() {
        // Dev build with bumped Cargo.toml shouldn't offer a downgrade.
        let json = r#"{
            "tag_name": "v0.5.0",
            "html_url": "https://x",
            "assets": [
                {"name": "lindiction-v0.5.0-x86_64-linux.tar.gz",        "browser_download_url": "u"},
                {"name": "lindiction-v0.5.0-x86_64-linux.tar.gz.sha256", "browser_download_url": "u"},
                {"name": "lindiction-v0.5.0-amd64.deb",                  "browser_download_url": "u"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let res = build_update_info(Version::parse("0.6.0").unwrap(), parsed).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn missing_primary_asset_errors() {
        let json = r#"{
            "tag_name": "v0.9.9",
            "html_url": "https://x",
            "assets": [
                {"name": "lindiction-v0.9.9-x86_64-linux.tar.gz",        "browser_download_url": "u"},
                {"name": "lindiction-v0.9.9-x86_64-linux.tar.gz.sha256", "browser_download_url": "u"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        // No .deb at all — build_update_info must reject.
        let err = build_update_info(Version::parse("0.1.0").unwrap(), parsed).unwrap_err();
        assert!(
            format!("{err:#}").contains(".deb"),
            "error should mention the missing asset type"
        );
    }

    #[test]
    fn tag_name_v_prefix_is_stripped() {
        let json = r#"{
            "tag_name": "v1.2.3",
            "html_url": "https://x",
            "assets": [
                {"name": "lindiction-v1.2.3-x86_64-linux.tar.gz",        "browser_download_url": "u"},
                {"name": "lindiction-v1.2.3-x86_64-linux.tar.gz.sha256", "browser_download_url": "u"},
                {"name": "lindiction-v1.2.3-amd64.deb",                  "browser_download_url": "u"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let info = build_update_info(Version::parse("0.5.0").unwrap(), parsed)
            .unwrap()
            .unwrap();
        assert_eq!(info.latest, Version::parse("1.2.3").unwrap());
        // tag_name retains the `v` prefix because it's used for artifact filenames.
        assert_eq!(info.tag_name, "v1.2.3");
    }

    #[test]
    fn unparseable_tag_errors_gracefully() {
        let json = r#"{
            "tag_name": "not-a-version",
            "html_url": "https://x",
            "assets": []
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let err = build_update_info(Version::parse("0.5.0").unwrap(), parsed).unwrap_err();
        assert!(format!("{err:#}").contains("not-a-version"));
    }
}
