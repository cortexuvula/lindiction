//! Manage the lindiction systemd user unit: enable/disable auto-start on
//! login, report current status. Shells out to `systemctl --user` rather
//! than linking against libsystemd; matches the project's philosophy of
//! using stable CLI tools as the integration surface (see `inject.rs`).
//!
//! Semantics of "auto-start" here means auto-start on graphical login — the
//! unit is `WantedBy=default.target` in the user systemd instance, so it
//! starts when the user session comes up, not at system boot.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, info, warn};

/// The systemd user unit name. Must match the filename shipped in the `.deb`
/// and the name embedded by `UNIT_TEMPLATE`.
pub const UNIT_NAME: &str = "lindiction.service";

/// Result of `systemctl --user is-enabled lindiction.service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Unit is enabled — will auto-start on login.
    Enabled,
    /// Unit is installed but not enabled.
    Disabled,
    /// Unit file is not loadable from any systemd search path.
    NotInstalled,
    /// systemctl is missing from PATH.
    SystemctlMissing,
    /// Some other state (`static`, `masked`, `alias`, …) or an unexpected
    /// stdout we couldn't classify. The raw trimmed stdout is preserved so
    /// error messages can surface it.
    Other(String),
}

impl Status {
    pub fn is_enabled(&self) -> bool {
        matches!(self, Status::Enabled)
    }

    /// Human-friendly one-liner for CLI output and tooltips.
    pub fn describe(&self) -> String {
        match self {
            Status::Enabled => "enabled — lindiction will start automatically on login".into(),
            Status::Disabled => "disabled — lindiction will not start automatically".into(),
            Status::NotInstalled => {
                format!("not installed — {UNIT_NAME} was not found in any systemd search path")
            }
            Status::SystemctlMissing => {
                "systemctl not found on PATH; cannot manage autostart".into()
            }
            Status::Other(s) => format!("unexpected state: {s}"),
        }
    }
}

/// True iff systemctl is present AND the `--user` instance responds. The
/// tray uses this to decide whether to show the autostart checkbox at all.
pub fn is_supported() -> bool {
    if which::which("systemctl").is_err() {
        return false;
    }
    // Cheap smoke-test: `systemctl --user list-units --no-legend --type=service -q`
    // returns 0 when the user manager is reachable. If there's no user bus
    // (e.g. headless SSH without linger), this returns non-zero.
    Command::new("systemctl")
        .args(["--user", "--no-pager", "list-units", "--type=service", "-q"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Query the current enablement state. Non-fatal: errors are mapped to a
/// `Status` variant so callers (tray, CLI) can display a useful state.
pub fn status() -> Status {
    if which::which("systemctl").is_err() {
        return Status::SystemctlMissing;
    }
    let out = match Command::new("systemctl")
        .args(["--user", "is-enabled", UNIT_NAME])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "systemctl is-enabled failed to spawn");
            return Status::SystemctlMissing;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    parse_is_enabled_output(&stdout, &stderr, out.status.success())
}

/// Pure parse step extracted for testability. `systemctl is-enabled` has
/// several quirks worth encoding:
///   - "enabled" is exit 0; everything else is non-zero (including "static"
///     and "disabled"), so we can't rely on exit code alone.
///   - An absent unit produces empty stdout and an error on stderr like
///     `Failed to get unit file state for <name>: No such file or directory`.
fn parse_is_enabled_output(stdout: &str, stderr: &str, success: bool) -> Status {
    match stdout {
        "enabled" | "enabled-runtime" => Status::Enabled,
        "disabled" => Status::Disabled,
        "" => {
            // Empty stdout generally means the unit couldn't be loaded.
            // Older systemd prints "Failed to get unit file state" on stderr;
            // newer prints "No such file or directory". Match on either.
            if stderr.contains("No such file")
                || stderr.contains("not found")
                || stderr.contains("Failed to get unit file state")
            {
                Status::NotInstalled
            } else if success {
                // Extremely unusual — 0 exit with no output. Treat as Other.
                Status::Other(String::new())
            } else {
                Status::Other(stderr.to_string())
            }
        }
        other => Status::Other(other.to_string()),
    }
}

/// Enable auto-start on login. If the unit is not loadable by the user
/// systemd instance, a unit file is generated at
/// `~/.config/systemd/user/lindiction.service` pointing at the currently
/// running executable — this makes `cargo install` / source builds work
/// without any extra steps.
pub fn enable() -> Result<()> {
    ensure_systemctl()?;
    ensure_unit_loadable()?;
    let out = Command::new("systemctl")
        .args(["--user", "enable", UNIT_NAME])
        .output()
        .context("spawning `systemctl --user enable`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "systemctl --user enable {UNIT_NAME} failed: {}",
            stderr.trim()
        ));
    }
    info!(unit = UNIT_NAME, "autostart enabled");
    Ok(())
}

/// Disable auto-start on login. If the unit is not installed, treat as a
/// no-op (nothing to disable) rather than an error — matches the user's
/// intent of "make sure this is off".
pub fn disable() -> Result<()> {
    ensure_systemctl()?;
    if matches!(status(), Status::NotInstalled) {
        debug!(
            unit = UNIT_NAME,
            "already not installed; disable is a no-op"
        );
        return Ok(());
    }
    let out = Command::new("systemctl")
        .args(["--user", "disable", UNIT_NAME])
        .output()
        .context("spawning `systemctl --user disable`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "systemctl --user disable {UNIT_NAME} failed: {}",
            stderr.trim()
        ));
    }
    info!(unit = UNIT_NAME, "autostart disabled");
    Ok(())
}

fn ensure_systemctl() -> Result<()> {
    which::which("systemctl")
        .map(|_| ())
        .map_err(|_| anyhow!("systemctl not found on PATH; cannot manage autostart"))
}

/// Make sure the user systemd instance can load the unit. Called before
/// `enable`. Three code paths:
///   1. Unit is already loadable (system-wide `.deb` install or a previously
///      generated user unit): do nothing.
///   2. Unit is not loadable AND no user-local file exists: generate one
///      targeting `current_exe()` and `daemon-reload`.
///   3. Unit is not loadable but a user-local file already exists: leave it
///      alone (the user may be mid-edit) and surface a clear error.
fn ensure_unit_loadable() -> Result<()> {
    if !matches!(status(), Status::NotInstalled) {
        return Ok(());
    }
    let user_unit = user_unit_path()
        .ok_or_else(|| anyhow!("could not resolve XDG config directory for systemd user unit"))?;
    if user_unit.exists() {
        return Err(anyhow!(
            "unit file exists at {} but systemd can't load it — check its contents or run `systemctl --user daemon-reload`",
            user_unit.display()
        ));
    }
    let exe = std::env::current_exe().context("determining current executable path")?;
    let contents = generate_unit_contents(&exe);
    if let Some(parent) = user_unit.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // create_new: never clobber an existing file. Defense in depth given
    // the exists() check above — `std::fs::write` would otherwise silently
    // overwrite, which we don't want if there's a TOCTOU race.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&user_unit)
        .with_context(|| format!("creating {}", user_unit.display()))?;
    f.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", user_unit.display()))?;
    info!(path = %user_unit.display(), exe = %exe.display(), "generated systemd user unit");

    // Make systemd pick up the new file before we try to enable it.
    let out = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .context("spawning `systemctl --user daemon-reload`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "systemctl --user daemon-reload failed: {}",
            stderr.trim()
        ));
    }
    Ok(())
}

/// Where we write the generated unit file (only used when no system-wide
/// unit is present). Returns None in the essentially-impossible case that
/// neither `$XDG_CONFIG_HOME` nor `$HOME` is set.
fn user_unit_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("systemd").join("user").join(UNIT_NAME))
}

/// Produce a systemd unit file targeting `exe`. Kept as a pure function so
/// it's trivially unit-testable. Mirrors `systemd/lindiction.service` from
/// the repo with the ExecStart substituted.
fn generate_unit_contents(exe: &std::path::Path) -> String {
    format!(
        "[Unit]\n\
         Description=Lindiction voice dictation\n\
         After=graphical-session.target sound.target\n\
         PartOf=graphical-session.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe}\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         Environment=RUST_LOG=lindiction=info\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_enabled() {
        assert_eq!(
            parse_is_enabled_output("enabled", "", true),
            Status::Enabled
        );
        assert_eq!(
            parse_is_enabled_output("enabled-runtime", "", true),
            Status::Enabled
        );
    }

    #[test]
    fn parse_disabled() {
        // is-enabled returns exit-1 for "disabled" — success=false is the
        // realistic case we must handle.
        assert_eq!(
            parse_is_enabled_output("disabled", "", false),
            Status::Disabled
        );
    }

    #[test]
    fn parse_not_installed_from_stderr() {
        assert_eq!(
            parse_is_enabled_output(
                "",
                "Failed to get unit file state for lindiction.service: No such file or directory",
                false
            ),
            Status::NotInstalled
        );
        // Newer systemd: shorter message on stderr.
        assert_eq!(
            parse_is_enabled_output("", "Unit lindiction.service not found.", false),
            Status::NotInstalled
        );
    }

    #[test]
    fn parse_other_state_preserves_text() {
        assert_eq!(
            parse_is_enabled_output("static", "", true),
            Status::Other("static".into())
        );
        assert_eq!(
            parse_is_enabled_output("masked", "", false),
            Status::Other("masked".into())
        );
    }

    #[test]
    fn status_describe_covers_all_variants() {
        // Smoke test: every variant produces a non-empty human description.
        for s in [
            Status::Enabled,
            Status::Disabled,
            Status::NotInstalled,
            Status::SystemctlMissing,
            Status::Other("static".into()),
        ] {
            assert!(!s.describe().is_empty());
        }
    }

    #[test]
    fn generated_unit_contains_exe_path() {
        let contents = generate_unit_contents(Path::new("/home/alice/.cargo/bin/lindiction"));
        assert!(contents.contains("ExecStart=/home/alice/.cargo/bin/lindiction\n"));
        assert!(contents.contains("[Unit]"));
        assert!(contents.contains("[Service]"));
        assert!(contents.contains("[Install]"));
        assert!(contents.contains("WantedBy=default.target"));
    }

    #[test]
    fn generated_unit_matches_ship_template_shape() {
        // Keep feature-parity with systemd/lindiction.service. If the ship
        // file changes meaningfully, this test should be updated in lockstep.
        let contents = generate_unit_contents(Path::new("/usr/bin/lindiction"));
        let ship_file = include_str!("../systemd/lindiction.service");
        for key_line in [
            "Description=Lindiction voice dictation",
            "After=graphical-session.target sound.target",
            "PartOf=graphical-session.target",
            "Type=simple",
            "Restart=on-failure",
            "RestartSec=3",
            "Environment=RUST_LOG=lindiction=info",
            "WantedBy=default.target",
        ] {
            assert!(
                contents.contains(key_line),
                "generated unit missing: {key_line}"
            );
            assert!(
                ship_file.contains(key_line),
                "ship file missing: {key_line} (update autostart.rs together)"
            );
        }
    }

    #[test]
    fn user_unit_path_ends_correctly() {
        if let Some(p) = user_unit_path() {
            assert!(
                p.ends_with("systemd/user/lindiction.service"),
                "got {}",
                p.display()
            );
        }
    }
}
