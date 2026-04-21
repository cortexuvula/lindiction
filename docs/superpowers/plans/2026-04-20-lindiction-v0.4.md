# Lindiction v0.4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship lindiction v0.4 — extend the tray menu from a single Quit item to four items (Open config…, About Lindiction, Help, Quit) with a separator before Quit. No daemon state changes; dictation pipeline untouched.

**Architecture:** Three new menu items added to `src/tray.rs::LindictionTray::menu()`. Each dispatches to a free function (`open_config`, `about`, `help`) with a shared `notify_warn` helper for user-facing failures. Existing private `Config::config_path` method promotes to a module-level public function `config::config_file_path()` so `tray.rs` can resolve `~/.config/lindiction/config.toml`. One new dependency: `notify-rust` with the `d` (dbus-rs) feature — zero new C deps since ksni already pulls dbus-rs.

**Tech Stack:** Rust 2021, existing tokio / ksni / cpal / whisper-rs / dbus-rs (via ksni). New: `notify-rust` for desktop notifications. Existing `xdg-open` shell-out for file/URL opening.

**Spec:** `docs/superpowers/specs/2026-04-20-lindiction-v0.4-design.md`

**Prerequisite:** v0.3 shipped on `main` at tip `65f5746` (v0.4 spec commit). v0.3.0 is tagged. Work happens on a new branch `feat/v0.4-impl` branched from `main`.

---

## Task 1: Branch + `notify-rust` dependency

**Files:** Modify `Cargo.toml`.

- [ ] **Step 1: Create implementation branch**

```bash
git checkout -b feat/v0.4-impl
git log --oneline -2
```

Expected: branch created from `main`. Tip is `65f5746` (v0.4 spec) with `979a191` (v0.3.0 version bump) beneath.

- [ ] **Step 2: Add `notify-rust` to Cargo.toml**

In `Cargo.toml` under `[dependencies]`, add `notify-rust` in alphabetical order, between `ksni` and `regex`. The `[dependencies]` block becomes:

```toml
[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive", "env"] }
cpal = "0.15"
dirs = "5"
global-hotkey = "0.5"
ksni = "0.2"
notify-rust = { version = "4", default-features = false, features = ["d"] }
regex = "1"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "signal", "process", "time"] }
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
whisper-rs = "0.11"
which = "6"
```

`default-features = false` + `features = ["d"]` explicitly selects the dbus-rs backend, matching what ksni already pulls. Without this, notify-rust 4.x defaults to the zbus backend which would add ~20 transitive deps we don't need.

- [ ] **Step 3: Verify build**

```bash
cargo build
```

Expected: clean compile. notify-rust 4.x with the `d` feature pulls `dbus` (which ksni already has in the tree — minor version may differ but Cargo resolves to a compatible set).

**If `default-features = false, features = ["d"]` produces a missing-function error** (unusual — notify-rust's feature flags are stable across 4.x), fall back to `notify-rust = "4"` with default features and report DONE_WITH_CONCERNS describing the larger dep tree.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add notify-rust dependency for tray About notification"
```

---

## Task 2: `config::config_file_path()` refactor

**Files:** Modify `src/config.rs`.

- [ ] **Step 1: Write the failing test**

In `src/config.rs`'s `#[cfg(test)] mod tests { ... }` block, append a new test (just before the closing brace of the `mod tests` block):

```rust
    #[test]
    fn config_file_path_ends_correctly() {
        // If the environment resolves $XDG_CONFIG_HOME (or $HOME on Linux),
        // the path must end with lindiction/config.toml. If the env is so
        // broken that dirs::config_dir returns None, the function returns
        // None — acceptable, and we skip the assertion.
        if let Some(p) = super::config_file_path() {
            assert!(
                p.ends_with("lindiction/config.toml"),
                "got {}",
                p.display()
            );
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib config::tests::config_file_path_ends_correctly 2>&1 | tail -10
```

Expected: compile error `cannot find function \`config_file_path\` in module \`super\`` (or similar).

- [ ] **Step 3: Refactor `Config::config_path` into a module-level public function**

In `src/config.rs`, find the existing private method:

```rust
    /// `~/.config/lindiction/config.toml` (or `$XDG_CONFIG_HOME/lindiction/config.toml`).
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("lindiction").join("config.toml"))
    }
```

Delete it from `impl Config`. Then add a new module-level public function, placed right below the existing `pub fn default_model_path() -> PathBuf` helper (both are paths-related helpers at module scope):

```rust
/// Path to the TOML config file: `$XDG_CONFIG_HOME/lindiction/config.toml`
/// (typically `~/.config/lindiction/config.toml`). Returns `None` only when
/// neither `$XDG_CONFIG_HOME` nor `$HOME` is set — essentially impossible
/// in practice on Linux.
pub fn config_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("lindiction").join("config.toml"))
}
```

- [ ] **Step 4: Update the existing caller in `Config::from_xdg_file`**

Find this line in `Config::from_xdg_file`:

```rust
        let Some(path) = Self::config_path() else {
```

Change the path to the new public function:

```rust
        let Some(path) = config_file_path() else {
```

(`config_file_path` is in the same module so no `crate::` prefix is needed.)

- [ ] **Step 5: Run the full config test suite**

```bash
cargo test --lib config:: -- --test-threads=1 2>&1 | tail -15
```

Expected: 13 tests pass (12 pre-existing config tests + 1 new `config_file_path_ends_correctly`).

- [ ] **Step 6: Run full lint + tests**

```bash
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 52 tests pass (51 prior + 1 new), fmt clean, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add src/config.rs
git commit -m "refactor(config): promote config_path to module-level pub fn config_file_path"
```

---

## Task 3: Tray menu extension

**Files:** Modify `src/tray.rs`.

This is the task's biggest change. It adds a testable `ensure_config_file_exists` helper, three free functions for menu actions (`open_config`, `about`, `help`), a shared `notify_warn` helper, and updates `LindictionTray::menu()` to ship four menu items + a separator.

- [ ] **Step 1: Write failing tests for the file-creation helper**

At the top of `src/tray.rs`'s existing `#[cfg(test)] mod tests { ... }` block (just after `use super::*;`), append these new tests:

```rust
    use std::path::Path;

    fn fresh_temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lindiction-tray-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn ensure_config_file_creates_empty_when_missing() {
        let dir = fresh_temp_dir("create-empty");
        let path = dir.join("lindiction").join("config.toml");
        assert!(!path.exists(), "precondition: path must not exist");

        super::ensure_config_file_exists(&path).expect("should create");
        assert!(path.exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_config_file_leaves_existing_unchanged() {
        let dir = fresh_temp_dir("leave-existing");
        let config_dir = dir.join("lindiction");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("config.toml");
        let existing_content = "[hotkey]\nbinding = \"f9\"\n";
        std::fs::write(&path, existing_content).unwrap();

        super::ensure_config_file_exists(&path).expect("should no-op");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), existing_content);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_config_file_creates_nested_parents() {
        let dir = fresh_temp_dir("nested-parents");
        let path = dir.join("a").join("b").join("c").join("config.toml");
        assert!(!path.exists());

        super::ensure_config_file_exists(&path).expect("should create nested");
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
```

The `use std::path::Path;` line may duplicate an earlier import — if so, remove the duplicate (Rust will emit a warning otherwise and clippy will fail).

- [ ] **Step 2: Run to verify tests fail**

```bash
cargo test --lib tray::tests::ensure_config 2>&1 | tail -10
```

Expected: compile error `cannot find function \`ensure_config_file_exists\` in module \`super\``.

- [ ] **Step 3: Implement `ensure_config_file_exists` and the three menu-action free functions**

In `src/tray.rs`, find the end of the `impl TrayManager { ... }` block and the `impl Drop for TrayManager` block. After both, and BEFORE the `#[cfg(test)] mod tests { ... }` block, add these free functions and helpers:

```rust
/// Open the TOML config file in the user's default editor, creating an
/// empty file if it doesn't exist yet. Empty TOML is valid — it parses
/// to all defaults thanks to `#[serde(default)]` on every config struct.
fn open_config() {
    let Some(path) = crate::config::config_file_path() else {
        warn!("could not resolve XDG config path; Open config has no target");
        return;
    };

    if let Err(e) = ensure_config_file_exists(&path) {
        warn!(error = %e, path = %path.display(), "could not create config file");
        notify_warn(&format!("Could not create {}: {}", path.display(), e));
        return;
    }

    // `xdg-open`'s exit code is unreliable across distros (some backends
    // return non-zero even on success). We only flag spawn failures
    // (Err from `.status()`), not non-zero exits.
    if let Err(e) = std::process::Command::new("xdg-open").arg(&path).status() {
        warn!(error = %e, "xdg-open failed");
        notify_warn(&format!("xdg-open failed: {e}"));
    }
}

/// Create the config file at `path` if missing. No-op if the file already
/// exists. Creates all parent directories as needed. Written empty — the
/// user's editor (or their subsequent config authoring) fills it in.
fn ensure_config_file_exists(path: &std::path::Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, "")?;
    info!(path = %path.display(), "created empty config file");
    Ok(())
}

/// Show a desktop notification with version, license, and the repo URL.
/// Best-effort: if the notification subsystem is unavailable, silently no-op.
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
}

/// Open the project's GitHub page in the user's default browser.
fn help() {
    const REPO_URL: &str = "https://github.com/cortexuvula/lindiction";
    // `xdg-open`'s exit code is unreliable across distros — only flag spawn
    // failures (Err from `.status()`), not non-zero exits.
    if let Err(e) = std::process::Command::new("xdg-open").arg(REPO_URL).status() {
        warn!(error = %e, "xdg-open failed");
        notify_warn(&format!("xdg-open failed: {e}"));
    }
}

/// Best-effort warning notification. Used when a menu action fails in a way
/// the user would want to know about (e.g. xdg-open not installed).
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

- [ ] **Step 4: Run the file-creation tests to confirm green**

```bash
cargo test --lib tray::tests::ensure_config 2>&1 | tail -10
```

Expected: 3 tests pass (`ensure_config_file_creates_empty_when_missing`, `ensure_config_file_leaves_existing_unchanged`, `ensure_config_file_creates_nested_parents`).

- [ ] **Step 5: Wire the new items into `LindictionTray::menu()`**

Find the current `menu()` impl on `LindictionTray`. It currently looks like:

```rust
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![StandardItem {
            label: "Quit".into(),
            activate: Box::new(|this: &mut Self| {
                let _ = this.shutdown_tx.try_send(());
            }),
            ..Default::default()
        }
        .into()]
    }
```

Replace with the four-item menu plus a separator before Quit:

```rust
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open config\u{2026}".into(),
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
            ksni::MenuItem::Separator,
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

Notes:

- The ellipsis is `\u{2026}` (the character `…`). Writing the literal byte directly works too, but the escaped form keeps the source ASCII-safe.
- `ksni::MenuItem::Separator` is the expected variant name in ksni 0.2. If the compile fails with "no variant named `Separator`", look in `~/.cargo/registry/src/*/ksni-0.2*/src/menu.rs` for the actual variant name and adapt. If ksni 0.2 genuinely does not expose a separator variant, drop the `ksni::MenuItem::Separator` line entirely and ship a flat four-item list — log this as a DONE_WITH_CONCERNS note.

- [ ] **Step 6: Verify build and full test suite**

```bash
cargo build --release
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 55 tests pass (52 from Task 2 + 3 new tray file-creation tests), fmt clean, clippy clean, release build succeeds.

- [ ] **Step 7: Commit**

```bash
git add src/tray.rs
git commit -m "feat(tray): add Open config, About, and Help menu items"
```

---

## Task 4: README update

**Files:** Modify `README.md`.

- [ ] **Step 1: Locate the existing Run section**

The current `## Run` section from v0.3 has two subsections: `### Flags` and `### Auto-start with systemd (optional)`. v0.4 adds a third subsection documenting the tray menu.

- [ ] **Step 2: Insert the new subsection between Flags and Auto-start**

Find the end of the `### Flags` subsection (typically a table of flags or a `--help` summary). After that subsection and BEFORE the `### Auto-start with systemd (optional)` H3, insert:

```markdown

### Tray menu

When the daemon is running, a microphone icon appears in the system tray. Left-click it (or right-click, depending on your desktop) to open the menu:

- **Open config…** — opens `~/.config/lindiction/config.toml` in your default text editor, creating an empty file if it doesn't exist yet. Save the file and restart the daemon to pick up changes.
- **About Lindiction** — shows a short desktop notification with the current version, license, and project URL.
- **Help** — opens [this repository](https://github.com/cortexuvula/lindiction) in your default browser.
- **Quit** — exits the daemon cleanly (same as Ctrl-C in the daemon's terminal).

The tray icon also changes color to reflect daemon state: dim microphone (idle), red dot (recording), refresh spinner (transcribing).

```

- [ ] **Step 3: Verify section ordering**

```bash
grep -n '^##' README.md
```

Expected output (exact line numbers vary, but this ordering):

```
## Requirements
## Install
## Run
## Configuration
## Migrating from v0.2
## Troubleshooting
## Testing
## License
```

Within `## Run`, verify `grep -n '^### ' README.md` shows `### Flags`, then `### Tray menu`, then `### Auto-start with systemd (optional)` — in that order.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document the v0.4 tray menu items under Run"
```

---

## Task 5: Acceptance + tag v0.4.0

**Files:** Modify `Cargo.toml` (version bump).

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --lib -- --test-threads=1
LINDICTION_MODEL=~/.local/share/lindiction/models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

Expected: 55 unit tests + 1 integration test, all green.

If `~/.local/share/lindiction/models/ggml-tiny.en.bin` doesn't exist on the dev machine, the integration test is gated on the env var pointing to a real file — either download it first or run the integration test manually against a known model location.

- [ ] **Step 2: Run lint checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: both clean.

- [ ] **Step 3: Bump version**

In `Cargo.toml`, change:

```toml
version = "0.3.0"
```

to:

```toml
version = "0.4.0"
```

Then rebuild to update Cargo.lock:

```bash
cargo build --release
```

- [ ] **Step 4: Commit the version bump**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.4.0"
```

- [ ] **Step 5: Manual test 1 — menu has 4 items + separator**

Launch the daemon:

```bash
./target/release/lindiction -v
```

Click the tray icon. The menu should show:

1. Open config…
2. About Lindiction
3. Help
4. (separator line)
5. Quit

If the separator isn't visible, that's acceptable only if Task 3 Step 5 surfaced a compile-time error for `ksni::MenuItem::Separator` and fell back to a flat list. Otherwise it's a regression — fix before proceeding.

- [ ] **Step 6: Manual test 2 — Open config creates an empty file**

Kill the daemon (Ctrl-C in its terminal). Delete any existing config:

```bash
rm -f ~/.config/lindiction/config.toml
ls ~/.config/lindiction/ 2>/dev/null || echo "no config dir"
```

Relaunch the daemon. Click tray → Open config…

Expected:
- `~/.config/lindiction/config.toml` is created, empty (0 bytes), verify with `ls -l ~/.config/lindiction/config.toml`.
- User's default text editor opens with the empty file visible.
- Daemon log contains `created empty config file path=...`.

- [ ] **Step 7: Manual test 3 — Open config preserves existing file**

Close the editor. Write a non-trivial config:

```bash
cat > ~/.config/lindiction/config.toml <<'EOF'
[hotkey]
binding = "f9"
EOF
```

Click tray → Open config… again.

Expected:
- The editor re-opens with the `[hotkey]` content intact — no overwrite.
- `cat ~/.config/lindiction/config.toml` still shows `binding = "f9"`.

- [ ] **Step 8: Manual test 4 — About notification**

Click tray → About Lindiction.

Expected: a desktop notification appears with title `Lindiction v0.4.0`, body mentioning "Push-to-talk voice dictation", "MIT licensed", and `https://github.com/cortexuvula/lindiction`. Dismisses after ~6 seconds.

- [ ] **Step 9: Manual test 5 — Help opens the repo URL**

Click tray → Help.

Expected: default browser opens with `https://github.com/cortexuvula/lindiction`.

- [ ] **Step 10: Manual test 6 — Quit still works**

Click tray → Quit.

Expected:
- Daemon log: `tray Quit activated; shutting down`.
- Process exits with code 0.

- [ ] **Step 11: Clean up the test config**

```bash
rm -f ~/.config/lindiction/config.toml
```

- [ ] **Step 12: Merge to main**

```bash
git checkout main
git merge --ff-only feat/v0.4-impl
git log --oneline -8
```

Expected: 5 new commits from v0.4 (Task 1 branch + ksni dep was counted, actually Task 1 was notify-rust dep; Tasks 2, 3, 4, 5 plus the spec commit already on main). Final graph:

```
chore: bump version to 0.4.0
docs: document the v0.4 tray menu items under Run
feat(tray): add Open config, About, and Help menu items
refactor(config): promote config_path to module-level pub fn config_file_path
chore: add notify-rust dependency for tray About notification
docs: add v0.4 design spec
```

- [ ] **Step 13: Tag v0.4.0 and push**

```bash
git tag -a v0.4.0 -m "v0.4.0: tray menu items (Open config, About, Help) + Quit

Adds three new tray menu items above the existing Quit: Open config
(xdg-open ~/.config/lindiction/config.toml, creating empty if missing),
About Lindiction (desktop notification via notify-rust with version
and project URL), and Help (xdg-open the GitHub repo). A separator
divides the three action items from Quit. No daemon state changes;
dictation pipeline unchanged.

55 unit tests, 1 integration test. Manual E2E verified on Ubuntu 24.04
X11 GNOME: all four menu items work, empty-config creation preserves
existing files, notify-rust pulls no new C deps."

git push origin main
git push origin v0.4.0
```

Expected: main pushes cleanly; tag push triggers the release workflow.

- [ ] **Step 14: Verify CI + release workflows**

```bash
sleep 5
gh run list --repo cortexuvula/lindiction --limit 2

until [ "$(gh run list --repo cortexuvula/lindiction --limit 2 --json status -q '.[] | .status' | grep -c 'completed')" = "2" ]; do
  sleep 30
done

gh run list --repo cortexuvula/lindiction --limit 2
gh release view v0.4.0 --repo cortexuvula/lindiction
```

Expected:
- Both workflows complete successfully.
- Release has three attached assets: `lindiction-v0.4.0-x86_64-linux.tar.gz`, `lindiction-v0.4.0-x86_64-linux.tar.gz.sha256`, `lindiction-v0.4.0-amd64.deb`.

- [ ] **Step 15: Delete the feature branch**

```bash
git branch -d feat/v0.4-impl
git branch --list
```

Expected: only `main` remains.

**Plan complete.** v0.4.0 is live with the expanded tray menu.
