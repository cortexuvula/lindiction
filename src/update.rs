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

const GITHUB_API_URL: &str = "https://api.github.com/repos/cortexuvula/lindiction/releases/latest";

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
    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).context("parsing current package version")?;
    let raw = fetch_latest_release_json().await?;
    let parsed: GithubRelease =
        serde_json::from_str(&raw).context("parsing GitHub release JSON")?;
    build_update_info(current, parsed)
}

/// Build the asset suffix for the running daemon's compiled backend.
/// CPU build → unchanged from v0.7.0 conventions ("-amd64.deb" /
/// "-x86_64-linux.tar.gz") so v0.7.0/v0.8.0 daemons keep finding the
/// right artifact in v0.8.1+ releases. GPU builds → backend-specific
/// suffix that won't match the cpu artifact (e.g. "-amd64-cuda.deb",
/// "-x86_64-linux-cuda.tar.gz").
fn pick_suffix(base_suffix: &str, backend: &str) -> String {
    if backend == "cpu" {
        return base_suffix.to_string();
    }
    // base_suffix is something like "-amd64.deb" or "-x86_64-linux.tar.gz".
    // Insert the backend tag before the final extension. We split at the
    // last `.` rather than hardcoding ".deb" / ".tar.gz" so future suffixes
    // (e.g. ".tar.zst") work without changes here.
    if let Some(dot) = base_suffix.rfind('.') {
        // For ".tar.gz", we want to split at the FIRST dot in ".tar.gz",
        // not the last (otherwise we'd produce "-x86_64-linux.tar-cuda.gz").
        // Detect the compound extension and adjust.
        let (stem, ext) = if base_suffix.ends_with(".tar.gz") {
            let cut = base_suffix.len() - ".tar.gz".len();
            (&base_suffix[..cut], ".tar.gz")
        } else {
            (&base_suffix[..dot], &base_suffix[dot..])
        };
        return format!("{stem}-{backend}{ext}");
    }
    // Suffix without an extension is unusual; fall back to plain.
    base_suffix.to_string()
}

/// Extract the trailing filename from an asset URL.
///
/// install_deb / install_tarball use this to keep the local download
/// filename in lock-step with the asset name embedded in the .sha256
/// sidecar. The release CI generates each sidecar with
/// `sha256sum FILENAME > FILENAME.sha256`, so the sidecar's content
/// is `<hash>  FILENAME`. If we save the downloaded artifact under
/// any other name, `sha256sum -c` looks for FILENAME on disk and
/// rejects the verification with "No such file or directory" — the
/// exact failure mode that broke v0.8.3 cuda → v0.9.0 auto-update.
fn basename_from_url(url: &str) -> Result<String> {
    // Strip any query string or fragment first; release URLs don't
    // have these today, but defensive against future format changes.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    // Look at the rightmost segment specifically — a URL ending in `/`
    // has no filename, even if there are non-empty segments earlier in
    // the path.
    let last = path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("url has no filename component: {url}"))?;
    Ok(last.to_string())
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
    let backend = crate::COMPILED_BACKEND;
    let deb_suffix = pick_suffix("-amd64.deb", backend);
    let tarball_suffix = pick_suffix("-x86_64-linux.tar.gz", backend);

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
    let deb = find_primary(&deb_suffix).ok_or_else(|| {
        anyhow!(
            "release {} is missing a .deb asset for backend `{backend}` \
             (looked for suffix `{deb_suffix}`)",
            parsed.tag_name
        )
    })?;
    let tarball = find_primary(&tarball_suffix).ok_or_else(|| {
        anyhow!(
            "release {} is missing a tarball asset for backend `{backend}` \
             (looked for suffix `{tarball_suffix}`)",
            parsed.tag_name
        )
    })?;
    let tarball_sha = find_sha(&tarball_suffix).ok_or_else(|| {
        anyhow!(
            "release {} is missing a tarball SHA256 sidecar for backend `{backend}`",
            parsed.tag_name
        )
    })?;
    let deb_sha = find_sha(&deb_suffix);
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

/// Temp directory guard. Creates the dir on construction, removes it on drop.
/// Replaces the former make_tmp_dir + cleanup_tmp pair so every early-return
/// via `?` in install_deb / install_tarball cleans up, not just the success
/// path.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let p = std::env::temp_dir().join(format!("lindiction-update-{}", std::process::id()));
        // Wipe any stale debris from a prior attempt so sha256sum -c can't
        // "verify" the wrong file.
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
        Ok(Self(p))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join<P: AsRef<Path>>(&self, name: P) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                error = %e,
                path = %self.0.display(),
                "failed to clean up update temp dir"
            ),
        }
    }
}

/// Staging-file guard. Removes the file on drop unless it has already been
/// renamed elsewhere (in which case remove_file returns ENOENT, which we
/// swallow). Used by the tarball install to clean up the
/// `.lindiction-update-$PID.tmp` staging copy if any step after its
/// creation fails before the atomic rename.
struct StagingFile(PathBuf);

impl StagingFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingFile {
    fn drop(&mut self) {
        // ENOENT is the normal success path (we renamed the file away).
        // Anything else is best-effort cleanup gone wrong; worth a warn so
        // the user can spot permission / FS errors instead of silently
        // littering their install directory.
        match std::fs::remove_file(&self.0) {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                error = %e,
                path = %self.0.display(),
                "failed to clean up staging file"
            ),
        }
    }
}

async fn install_deb(info: &UpdateInfo) -> Result<()> {
    let tmp = TempDir::new()?;
    // Derive local filenames from the asset URLs. The .sha256 sidecars
    // produced by the release CI contain the upstream asset filename
    // verbatim (`sha256sum lindiction-vX.Y.Z-amd64-cuda.deb > ....sha256`),
    // so the local download filename MUST match — otherwise sha256sum -c
    // looks for a name that doesn't exist on disk and rejects the
    // verification. Using the URL's basename keeps the two in sync
    // regardless of backend (cpu / cuda / vulkan / hipblas).
    let deb_filename = basename_from_url(&info.deb_url)
        .context("could not derive .deb filename from deb_url")?;
    let deb_path = tmp.join(&deb_filename);
    download(&info.deb_url, &deb_path).await?;
    if let Some(sha_url) = &info.deb_sha256_url {
        let sha_filename = format!("{deb_filename}.sha256");
        download(sha_url, &tmp.join(&sha_filename)).await?;
        verify_sha256(tmp.path(), &sha_filename)?;
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
    Ok(())
}

async fn install_tarball(info: &UpdateInfo) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new()?;
    // See install_deb for why local filenames must mirror the URL's
    // basename rather than being hand-formatted with a fixed suffix.
    let tarball_filename = basename_from_url(&info.tarball_url)
        .context("could not derive tarball filename from tarball_url")?;
    let sha_filename = format!("{tarball_filename}.sha256");
    let tarball_path = tmp.join(&tarball_filename);
    let sha_path = tmp.join(&sha_filename);
    download(&info.tarball_url, &tarball_path).await?;
    download(&info.tarball_sha256_url, &sha_path).await?;
    verify_sha256(tmp.path(), &sha_filename)?;

    // Extract. The release tarball is a flat archive containing only the
    // `lindiction` binary at its root.
    let status = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball_path)
        .arg("-C")
        .arg(tmp.path())
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
    let exe = std::env::current_exe().context("resolving current binary path for rename target")?;
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow!("current binary {} has no parent directory", exe.display()))?;
    let staging = StagingFile(parent.join(format!(".lindiction-update-{}.tmp", std::process::id())));
    std::fs::copy(&extracted, staging.path())
        .with_context(|| format!("copying new binary to {}", staging.path().display()))?;
    std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("setting +x on {}", staging.path().display()))?;
    // Atomic swap. At this point a crash leaves either the old or the new
    // binary in place — never a truncated file. `rename` is guaranteed
    // atomic within a single filesystem.
    std::fs::rename(staging.path(), &exe).with_context(|| {
        format!(
            "renaming {} -> {}; leaving staging file in place for inspection",
            staging.path().display(),
            exe.display()
        )
    })?;
    info!(path = %exe.display(), "tarball install complete");
    Ok(())
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
    use std::sync::Mutex;

    /// Serializes the guard tests so they don't race each other on the
    /// PID-keyed paths TempDir and StagingFile use. A panic in one test
    /// poisons the mutex; later tests pull the lock with `.unwrap_or_else(|p| p.into_inner())`
    /// so one failure doesn't cascade into spurious failures in siblings.
    static GUARD_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn guard_lock() -> std::sync::MutexGuard<'static, ()> {
        GUARD_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

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
        assert_eq!(
            info.tarball_sha256_url,
            "https://example.test/tarball.sha256"
        );
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

    #[test]
    fn temp_dir_creates_and_removes_on_drop() {
        let _lock = guard_lock();
        let path;
        {
            let guard = TempDir::new().expect("creating TempDir");
            path = guard.path().to_path_buf();
            assert!(path.is_dir(), "TempDir::new must create the dir");
        }
        assert!(!path.exists(), "TempDir must remove the dir on drop");
    }

    #[test]
    fn temp_dir_cleans_pre_existing_debris() {
        let _lock = guard_lock();
        // Simulate a leftover from a prior run.
        let expected = std::env::temp_dir().join(format!("lindiction-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&expected);
        std::fs::create_dir_all(&expected).unwrap();
        std::fs::write(expected.join("stale.txt"), b"old").unwrap();

        let guard = TempDir::new().expect("creating TempDir over debris");
        // After new(), the dir exists but the stale file does not.
        assert!(guard.path().is_dir());
        assert!(!guard.path().join("stale.txt").exists(), "new() should wipe the old dir");
        drop(guard);
        assert!(!expected.exists());
    }

    #[test]
    fn staging_file_drop_removes_file_if_still_present() {
        let _lock = guard_lock();
        let path = std::env::temp_dir().join(format!(
            "lindiction-staging-test-leftover-{}.tmp",
            std::process::id()
        ));
        std::fs::write(&path, b"contents").unwrap();
        assert!(path.exists());

        let guard = StagingFile(path.clone());
        drop(guard);

        assert!(!path.exists(), "StagingFile::drop must remove a still-present staging file");
    }

    #[test]
    fn staging_file_drop_tolerates_missing_file() {
        let _lock = guard_lock();

        // Simulate the success path: by the time drop runs, the file the
        // StagingFile was guarding has already been renamed away, so the
        // remove_file inside Drop will see ENOENT. Must not log a warning
        // or panic.
        let path = std::env::temp_dir().join(format!(
            "lindiction-staging-test-missing-{}.tmp",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        assert!(!path.exists(), "precondition: path must not exist");

        let guard = StagingFile(path.clone());
        drop(guard);

        assert!(!path.exists());
    }

    #[test]
    fn pick_suffix_cpu_backend_returns_unchanged() {
        // Backward-compatible behavior — what v0.7.0 / v0.8.0 daemons assume.
        assert_eq!(pick_suffix("-amd64.deb", "cpu"), "-amd64.deb");
        assert_eq!(
            pick_suffix("-x86_64-linux.tar.gz", "cpu"),
            "-x86_64-linux.tar.gz"
        );
    }

    #[test]
    fn pick_suffix_inserts_backend_before_simple_extension() {
        // .deb is a single-segment extension.
        assert_eq!(pick_suffix("-amd64.deb", "cuda"), "-amd64-cuda.deb");
        assert_eq!(pick_suffix("-amd64.deb", "vulkan"), "-amd64-vulkan.deb");
        assert_eq!(pick_suffix("-amd64.deb", "hipblas"), "-amd64-hipblas.deb");
    }

    #[test]
    fn pick_suffix_inserts_backend_before_compound_extension() {
        // .tar.gz is a compound extension — must NOT be split as ".tar" + ".gz".
        assert_eq!(
            pick_suffix("-x86_64-linux.tar.gz", "cuda"),
            "-x86_64-linux-cuda.tar.gz"
        );
        assert_eq!(
            pick_suffix("-x86_64-linux.tar.gz", "vulkan"),
            "-x86_64-linux-vulkan.tar.gz"
        );
    }

    #[test]
    fn pick_suffix_unknown_backend_passes_through() {
        // Defensive: if a future backend name appears, generate the
        // mechanical pattern; the asset just won't be found in the release
        // and the user gets a clear error.
        assert_eq!(pick_suffix("-amd64.deb", "future"), "-amd64-future.deb");
    }

    #[test]
    fn basename_from_url_extracts_cpu_filename() {
        let url = "https://github.com/cortexuvula/lindiction/releases/download/v0.9.0/lindiction-v0.9.0-amd64.deb";
        assert_eq!(
            basename_from_url(url).unwrap(),
            "lindiction-v0.9.0-amd64.deb"
        );
    }

    #[test]
    fn basename_from_url_extracts_cuda_filename() {
        // Regression test for the v0.8.3 bug: when the URL is the cuda
        // variant, the local filename MUST also be the cuda variant so
        // the sha256 sidecar's reference resolves on disk.
        let url = "https://github.com/cortexuvula/lindiction/releases/download/v0.9.0/lindiction-v0.9.0-amd64-cuda.deb";
        assert_eq!(
            basename_from_url(url).unwrap(),
            "lindiction-v0.9.0-amd64-cuda.deb"
        );
    }

    #[test]
    fn basename_from_url_extracts_tarball_compound_extension() {
        let url = "https://example.test/path/lindiction-v0.9.0-x86_64-linux-vulkan.tar.gz";
        assert_eq!(
            basename_from_url(url).unwrap(),
            "lindiction-v0.9.0-x86_64-linux-vulkan.tar.gz"
        );
    }

    #[test]
    fn basename_from_url_strips_query_string() {
        let url = "https://example.test/lindiction-v0.9.0-amd64-cuda.deb?token=abc&t=123";
        assert_eq!(
            basename_from_url(url).unwrap(),
            "lindiction-v0.9.0-amd64-cuda.deb"
        );
    }

    #[test]
    fn basename_from_url_rejects_trailing_slash() {
        // A URL ending in / has no filename — release asset URLs from
        // GitHub never look like this, but rejecting it is cheap insurance.
        assert!(basename_from_url("https://example.test/").is_err());
    }

    #[test]
    fn build_update_info_finds_cuda_assets_when_present() {
        // Simulates a v0.8.1 release with both cpu and cuda artifacts.
        // Construction is via canned JSON so we can drive the code path even
        // on a cpu build (we'd need to fake COMPILED_BACKEND to test the
        // cuda code path end-to-end, which we can't from a test). What this
        // test actually verifies is that the cpu daemon STILL picks the cpu
        // artifact even when the cuda artifact is present.
        let json = r#"{
            "tag_name": "v0.8.1",
            "html_url": "https://x",
            "assets": [
                {"name": "lindiction-v0.8.1-x86_64-linux.tar.gz",            "browser_download_url": "u_cpu_tar"},
                {"name": "lindiction-v0.8.1-x86_64-linux.tar.gz.sha256",     "browser_download_url": "u_cpu_tar_sha"},
                {"name": "lindiction-v0.8.1-amd64.deb",                       "browser_download_url": "u_cpu_deb"},
                {"name": "lindiction-v0.8.1-amd64.deb.sha256",                "browser_download_url": "u_cpu_deb_sha"},
                {"name": "lindiction-v0.8.1-x86_64-linux-cuda.tar.gz",        "browser_download_url": "u_cuda_tar"},
                {"name": "lindiction-v0.8.1-x86_64-linux-cuda.tar.gz.sha256", "browser_download_url": "u_cuda_tar_sha"},
                {"name": "lindiction-v0.8.1-amd64-cuda.deb",                  "browser_download_url": "u_cuda_deb"},
                {"name": "lindiction-v0.8.1-amd64-cuda.deb.sha256",           "browser_download_url": "u_cuda_deb_sha"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let info = build_update_info(Version::parse("0.7.0").unwrap(), parsed)
            .unwrap()
            .expect("0.7.0 should see 0.8.1 as newer");
        // On a CPU build, MUST select the unsuffixed cpu artifacts.
        assert_eq!(info.deb_url, "u_cpu_deb");
        assert_eq!(info.tarball_url, "u_cpu_tar");
        assert_eq!(info.deb_sha256_url.as_deref(), Some("u_cpu_deb_sha"));
        assert_eq!(info.tarball_sha256_url, "u_cpu_tar_sha");
    }

    #[test]
    fn build_update_info_legacy_release_still_works_for_cpu() {
        // A v0.7.0-style release (no GPU artifacts) must still resolve
        // cleanly for a cpu build. This is the backward-compat invariant.
        let json = r#"{
            "tag_name": "v0.8.1",
            "html_url": "https://x",
            "assets": [
                {"name": "lindiction-v0.8.1-x86_64-linux.tar.gz",        "browser_download_url": "u_tar"},
                {"name": "lindiction-v0.8.1-x86_64-linux.tar.gz.sha256", "browser_download_url": "u_tar_sha"},
                {"name": "lindiction-v0.8.1-amd64.deb",                  "browser_download_url": "u_deb"},
                {"name": "lindiction-v0.8.1-amd64.deb.sha256",           "browser_download_url": "u_deb_sha"}
            ]
        }"#;
        let parsed: GithubRelease = serde_json::from_str(json).unwrap();
        let info = build_update_info(Version::parse("0.7.0").unwrap(), parsed)
            .unwrap()
            .expect("0.7.0 should see 0.8.1 as newer");
        assert_eq!(info.deb_url, "u_deb");
        assert_eq!(info.tarball_url, "u_tar");
    }
}
