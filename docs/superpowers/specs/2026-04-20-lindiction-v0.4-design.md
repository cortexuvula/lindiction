# Lindiction v0.4 — Design

**Date:** 2026-04-20
**Status:** Approved, pre-implementation
**Predecessor:** `2026-04-20-lindiction-v0.3-design.md` (v0.3, tagged `v0.3.0`)

## Overview

Lindiction v0.4 expands the system tray menu from a single `Quit` item to four: `Open config…`, `About Lindiction`, `Help`, and `Quit`. Each new item is a pure tray-UI action — no changes to the daemon's state machine, hotkey wiring, or dictation pipeline. The dictation pipeline (cpal → whisper-rs → xdotool) is untouched.

Scope is deliberately narrow. Category-B menu items (Pause/Resume, Reload config, Show log) that would require daemon state changes or new infrastructure are explicitly deferred — see non-goals.

## Scope

### In scope (v0.4)

- **Open config…** — creates `~/.config/lindiction/config.toml` if missing (with an empty body, valid TOML that parses to all defaults), then shells out `xdg-open <path>`.
- **About Lindiction** — shows a 6-second desktop notification with version, one-line description, MIT license note, and the GitHub URL. Uses `notify-rust` (d/dbus-rs feature to match ksni's backend — no new C deps).
- **Help** — shells out `xdg-open https://github.com/cortexuvula/lindiction` to open the repo in the user's default browser.
- **Separator** between the three new items and Quit, if ksni 0.2's `MenuItem` supports it. If it doesn't, the separator is silently dropped — the four items stay in a flat list.
- **`config::config_file_path()` helper** — public function at module level that resolves `$XDG_CONFIG_HOME/lindiction/config.toml`. Refactored from the existing private `Config::config_path` to be callable from `tray.rs`.
- **README** documents the new menu items under the Run section.

### Explicit non-goals (deferred)

- Pause/Resume — would require a `paused: bool` in the `app.rs` select-loop Press arm. Probably v0.5.
- Reload config — would require hot-reregistering the global hotkey (singleton-receiver constraint of `global-hotkey`) and rebuilding the `Postprocessor`. Non-trivial. Probably v0.5+.
- Show log — requires adding persistent file logging (currently we log to stderr / journald only). Out of scope unless users actually ask for it.
- Submenus of any kind.
- Custom icons beyond the existing theme-name icons.
- GUI dialogs beyond desktop notifications.
- Hot-reload of the tray menu structure at runtime.

## Architecture

No new modules. Three new menu items are added to the existing `src/tray.rs` module's `LindictionTray::menu()` impl. One new public function in `src/config.rs`. One new dependency.

### New dependency

```toml
notify-rust = { version = "4", default-features = false, features = ["d"] }
```

- `default-features = false` disables the default `z` (zbus) feature.
- `features = ["d"]` selects the dbus-rs backend, which matches what `ksni = "0.2"` already pulls. Zero new transitive C dependencies.

### Modified files

```
Cargo.toml        # +notify-rust dep
src/config.rs     # refactor Config::config_path → pub fn config_file_path (module level)
src/tray.rs       # add 3 menu items + their closure handlers
README.md         # document menu items in the Run section
```

### Unchanged from v0.3

- Dictation pipeline, hotkey subsystem, audio capture, postprocess, inject, model download — all unmodified.
- `TrayManager` public API — unchanged. The only public-surface change is internal to `LindictionTray::menu()`.
- `TrayEvent` enum — unchanged.
- systemd unit, `.deb` metadata — unchanged.

## `config::config_file_path()` refactor

In `src/config.rs`, the current code has a private method:

```rust
impl Config {
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("lindiction").join("config.toml"))
    }
}
```

Refactor to a module-level public function:

```rust
/// Path to the TOML config file: `$XDG_CONFIG_HOME/lindiction/config.toml`
/// (typically `~/.config/lindiction/config.toml`). Returns `None` only
/// when neither `$XDG_CONFIG_HOME` nor `$HOME` is set — essentially
/// impossible in practice.
pub fn config_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("lindiction").join("config.toml"))
}
```

Update the existing caller `Config::from_xdg_file` to call `config_file_path()` instead of `Self::config_path()`. Delete the old private method. No behavior change.

## Menu extension

`LindictionTray::menu()` currently returns `vec![StandardItem { label: "Quit", ... }]`. Replace with:

```rust
fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
    use ksni::menu::StandardItem;
    vec![
        StandardItem {
            label: "Open config…".into(),
            activate: Box::new(|_: &mut Self| open_config()),
            ..Default::default()
        }
        .into(),
        StandardItem {
            label: "About Lindiction".into(),
            activate: Box::new(|_: &mut Self| about()),
            ..Default::default()
        }
        .into(),
        StandardItem {
            label: "Help".into(),
            activate: Box::new(|_: &mut Self| help()),
            ..Default::default()
        }
        .into(),
        // Separator (if supported in ksni 0.2; else this line is omitted)
        // ksni::MenuItem::Separator,
        StandardItem {
            label: "Quit".into(),
            activate: Box::new(|this: &mut Self| {
                let _ = this.shutdown_tx.try_send(());
            }),
            ..Default::default()
        }
        .into(),
    ]
}
```

The three new closures dispatch to free functions `open_config()`, `about()`, and `help()` defined at module level below the `LindictionTray` impl. They don't capture `LindictionTray` state (unlike Quit, which needs `shutdown_tx`), so they're plain `Fn()`. Easier to test and reason about.

**Separator caveat:** ksni 0.2 may or may not expose a `MenuItem::Separator` variant. If `MenuItem` is an enum with a `Separator` variant, include it between Help and Quit. If it's a struct (no separator variant), ship a flat list. The implementer confirms which by checking `~/.cargo/registry/src/*/ksni-0.2*/src/menu.rs` at task start. Flat-list fallback is acceptable.

## Menu action implementations

Three free functions at the bottom of `src/tray.rs` (after the existing impls, before the test module):

### `open_config()`

```rust
fn open_config() {
    let Some(path) = crate::config::config_file_path() else {
        warn!("could not resolve XDG config path; Open config has no target");
        return;
    };

    // Ensure the file exists — empty TOML is valid (parses to all defaults).
    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(error = %e, path = %parent.display(), "could not create config dir");
                notify_warn(&format!("Could not create {}: {}", parent.display(), e));
                return;
            }
        }
        if let Err(e) = std::fs::write(&path, "") {
            warn!(error = %e, path = %path.display(), "could not create config file");
            notify_warn(&format!("Could not create {}: {}", path.display(), e));
            return;
        }
        info!(path = %path.display(), "created empty config file");
    }

    if let Err(e) = std::process::Command::new("xdg-open").arg(&path).status() {
        warn!(error = %e, "xdg-open failed");
        notify_warn(&format!("xdg-open failed: {e}"));
    }
}
```

### `about()`

```rust
fn about() {
    let _ = notify_rust::Notification::new()
        .appname("Lindiction")
        .summary(&format!("Lindiction v{}", env!("CARGO_PKG_VERSION")))
        .body(
            "Push-to-talk voice dictation for Linux.\n\
             MIT licensed.\n\
             https://github.com/cortexuvula/lindiction",
        )
        .icon("audio-input-microphone")
        .timeout(notify_rust::Timeout::Milliseconds(6000))
        .show();
    // Ignored Result: if notifications fail, nothing is worse than not showing.
}
```

### `help()`

```rust
fn help() {
    const REPO_URL: &str = "https://github.com/cortexuvula/lindiction";
    if let Err(e) = std::process::Command::new("xdg-open").arg(REPO_URL).status() {
        warn!(error = %e, "xdg-open failed");
        notify_warn(&format!("xdg-open failed: {e}"));
    }
}
```

### `notify_warn(msg)` helper

Shared between `open_config` and `help`:

```rust
/// Send a best-effort warning notification. Used when a menu action fails
/// in a way the user would want to know about (e.g. xdg-open missing).
fn notify_warn(msg: &str) {
    let _ = notify_rust::Notification::new()
        .appname("Lindiction")
        .summary("Lindiction — action failed")
        .body(msg)
        .icon("dialog-warning")
        .timeout(notify_rust::Timeout::Milliseconds(5000))
        .show();
}
```

## Error handling

All three menu actions follow the same contract: **try, log on failure, notify on user-facing failures, never propagate**. ksni's menu-activate closure signature is `Box<dyn Fn(&mut Self) + Send + Sync>`; it can't return `Result`. Failures don't crash the daemon.

Specific cases:
- `xdg-open` not installed (vanishingly rare on desktop Linux) → warn log + desktop notification.
- Notifications subsystem unavailable → silent no-op. The `Result` from `notify_rust::Notification::show()` is ignored because there's no meaningful fallback.
- Config-path resolution fails → warn log. No notification (if the notification system is also broken, the user has bigger problems).
- Creating an empty config file fails (permission, disk full, etc.) → warn log + notification.

## Testing

### Unit

One new test in `src/config.rs`:

```rust
#[test]
fn config_file_path_ends_correctly() {
    let p = super::config_file_path();
    if let Some(p) = p {
        assert!(p.ends_with("lindiction/config.toml"), "got {}", p.display());
    }
    // If None, the environment has no $HOME / $XDG_CONFIG_HOME — acceptable.
}
```

No unit tests for `open_config`, `about`, or `help` — they shell out / hit DBus, and mocking those without adding infrastructure isn't worth it. Manual E2E covers them.

### Manual E2E (added to Task 10's test plan)

1. Click tray icon → the menu now shows 4 items (or 3 + separator + Quit), not 1.
2. Click "Open config…" with no existing config file → `~/.config/lindiction/config.toml` is created (empty), and the user's default text editor opens it. Verify `ls -lh ~/.config/lindiction/config.toml` shows a 0-byte file.
3. Click "Open config…" with an existing config file → no overwrite; editor opens the existing file.
4. Click "About Lindiction" → a desktop notification pops up with `Lindiction v0.4.0`, the description, and the GitHub URL. Dismisses after 6 seconds.
5. Click "Help" → default browser opens `https://github.com/cortexuvula/lindiction`.
6. Click "Quit" → daemon exits cleanly (unchanged from v0.3).

### CI

Unchanged from v0.3. The new `notify-rust` dep pulls no new system deps; `libdbus-1-dev` (already in both workflows) covers it.

## Migration notes

Zero migration required. Existing config files work unchanged. Existing systemd units work unchanged. Only user-visible change: the tray menu has more items now.

## Day-by-day (≤2 days)

### Day 1 — Implement

- `config::config_file_path()` refactor + update existing caller + 1 unit test.
- Add `notify-rust` dep.
- Extend `tray.rs` with three menu items, their closures, and the shared `notify_warn` helper.
- Optional: add `MenuItem::Separator` between Help and Quit if ksni 0.2 supports it.
- Manual smoke: launch daemon, click through the menu, verify each action.

**Exit criterion:** `cargo test --lib` green, `cargo fmt --check` clean, `cargo clippy -D warnings` clean, all four menu items work manually.

### Day 2 — Ship

- Update README Run section to document the menu.
- Bump `Cargo.toml` version to 0.4.0.
- Commit, merge to main, tag `v0.4.0`, push.
- Verify CI + Release workflows green; `.deb` + `.tar.gz` assets attached to the release.

**Exit criterion:** v0.4.0 live on GitHub with both release artifacts; local install via `.deb` produces a daemon whose tray menu has 4 items.

## Open questions — resolved during design

All decisions locked during brainstorming:

- **Menu item scope**: Open config + About + Help (Category A). Pause/Resume, Reload, Show log all deferred.
- **notify-send vs notify-rust**: notify-rust with `d` (dbus-rs) feature, because we already have dbus-rs in the tree via ksni. Zero net-new C deps. Also avoids depending on `notify-send`'s system binary.
- **"About" UX**: desktop notification, not a GUI dialog.
- **Empty config file on "Open config"**: yes — empty TOML parses to all defaults, so creating an empty file is always safe.
- **Separator**: include if ksni API permits; silently drop if not. Not worth a task iteration over.
