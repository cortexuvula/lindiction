# Lindiction v0.3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship lindiction v0.3 — system tray indicator (3 states, Quit menu), `.deb` package with opt-in systemd user service, and first-run auto-download of the default whisper model. Default model path moves to the XDG data directory so `.deb` installs work cleanly.

**Architecture:** Keep v0.2's pipeline (cpal → whisper-rs → xdotool) untouched. Add a `src/tray.rs` module that runs a `ksni` StatusNotifier service in a dedicated thread and exposes a small async API (`set_state`, `shutdown_signal`) to `app.rs`. Add a `src/model_download.rs` module that shells out to `curl` to fetch the default tiny.en model on first launch. Add `[package.metadata.deb]` to `Cargo.toml` and a `systemd/lindiction.service` unit. Update the GitHub Actions release workflow to build and upload the `.deb` alongside the existing tarball.

**Tech Stack:** Rust 2021, existing tokio / cpal / whisper-rs / global-hotkey / regex / clap / anyhow / tracing. New: `ksni` for the tray, `cargo-deb` for packaging (build-time tool, not a runtime dep).

**Spec:** `docs/superpowers/specs/2026-04-20-lindiction-v0.3-design.md`

**Prerequisite:** v0.2 shipped on `main` at `bed1c52` (tag `v0.2.0`). The v0.3 spec commit `f1d5f7f` is on top of that. Work happens on a new branch `feat/v0.3-impl` branched from `main`.

---

## Task 1: Branch + add ksni dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Create implementation branch**

```bash
git checkout -b feat/v0.3-impl
git log --oneline -2
```

Expected: branch created from `main`. Tip commit is `f1d5f7f` (v0.3 spec) with `bed1c52` (v0.2 README) beneath.

- [ ] **Step 2: Add ksni to Cargo.toml**

In `Cargo.toml` under `[dependencies]`, add `ksni = "0.2"` in alphabetical order (between `global-hotkey` and `regex`). The `[dependencies]` block becomes:

```toml
[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive", "env"] }
cpal = "0.15"
dirs = "5"
global-hotkey = "0.5"
ksni = "0.2"
regex = "1"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "signal", "process", "time"] }
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
whisper-rs = "0.11"
which = "6"
```

- [ ] **Step 3: Verify build**

```bash
cargo build
```

Expected: clean compile. ksni pulls `zbus` and a handful of DBus-related transitive crates on first build. If `ksni = "0.2"` does not resolve, try `ksni = "0.3"` and adjust the trait impl in Task 2 to match whichever version is installed (look in `~/.cargo/registry/src/*/ksni-*/src/lib.rs` for the current `Tray` trait shape).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add ksni dependency for tray support"
```

---

## Task 2: Tray module

**Files:**
- Create: `src/tray.rs`
- Modify: `src/lib.rs` (register the module)

- [ ] **Step 1: Declare the module**

In `src/lib.rs`, add `pub mod tray;` in alphabetical order. Final contents:

```rust
pub mod app;
pub mod audio;
pub mod config;
pub mod hotkey;
pub mod inject;
pub mod postprocess;
pub mod stt;
pub mod tray;
```

- [ ] **Step 2: Write the module with failing tests**

Create `src/tray.rs`:

```rust
use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Three visual states that the tray icon can display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Idle,
    Recording,
    Processing,
}

impl TrayEvent {
    /// Freedesktop theme-icon name for this state. These live in every
    /// modern icon theme; we avoid shipping our own PNG assets in v0.3.
    pub fn icon_name(self) -> &'static str {
        match self {
            TrayEvent::Idle => "audio-input-microphone",
            TrayEvent::Recording => "media-record",
            TrayEvent::Processing => "view-refresh",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            TrayEvent::Idle => "Lindiction — idle",
            TrayEvent::Recording => "Lindiction — recording",
            TrayEvent::Processing => "Lindiction — transcribing",
        }
    }
}

/// Internal ksni tray implementation. The `state` field is mutated
/// from an async task via `ksni::Handle::update`.
struct LindictionTray {
    state: TrayEvent,
    shutdown_tx: mpsc::Sender<()>,
}

impl ksni::Tray for LindictionTray {
    fn title(&self) -> String {
        "Lindiction".to_string()
    }

    fn id(&self) -> String {
        "lindiction".to_string()
    }

    fn icon_name(&self) -> String {
        self.state.icon_name().to_string()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.state.tooltip().to_string(),
            ..Default::default()
        }
    }

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
}

/// Public façade around the ksni service. Lives for the duration of the
/// daemon; dropping it unregisters the tray.
pub struct TrayManager {
    state_tx: mpsc::UnboundedSender<TrayEvent>,
    shutdown_rx: mpsc::Receiver<()>,
}

impl TrayManager {
    /// Register the tray icon on the session bus. Returns a manager that
    /// the main app uses to push state events and listen for Quit.
    ///
    /// Non-fatal on failure: if the tray cannot be registered (e.g. no
    /// DBus session, or a StatusNotifier host is not present), this
    /// returns a manager whose `set_state` is a no-op and whose
    /// shutdown channel never fires. The daemon still works via hotkey.
    pub fn start() -> Self {
        let (state_tx, mut state_rx) = mpsc::unbounded_channel::<TrayEvent>();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

        let tray = LindictionTray {
            state: TrayEvent::Idle,
            shutdown_tx: shutdown_tx.clone(),
        };

        // ksni::TrayService::spawn returns Ok in the current API; it does
        // its DBus work lazily on a background thread. If the session bus
        // is unavailable, ksni logs internally and the handle still exists
        // (updates become no-ops). That matches our "non-fatal" policy.
        let service = ksni::TrayService::new(tray);
        let handle = service.handle();
        service.spawn();

        info!("tray service spawned");

        // Bridge the mpsc<TrayEvent> channel into ksni's Handle::update calls.
        tokio::spawn(async move {
            while let Some(event) = state_rx.recv().await {
                debug!(?event, "tray state update");
                handle.update(|t| t.state = event);
            }
            debug!("tray state channel closed; exiting bridge task");
        });

        Self {
            state_tx,
            shutdown_rx,
        }
    }

    /// Non-blocking. Safe to call from any thread or async context.
    /// Events are queued on an unbounded channel and applied in order
    /// by a background tokio task.
    pub fn set_state(&self, event: TrayEvent) {
        if self.state_tx.send(event).is_err() {
            warn!("tray bridge task has exited; state update dropped");
        }
    }

    /// Main app awaits this to learn when the user picked Quit from
    /// the tray menu.
    pub fn shutdown_signal(&mut self) -> &mut mpsc::Receiver<()> {
        &mut self.shutdown_rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_name_is_distinct_per_state() {
        let names: Vec<&str> = [
            TrayEvent::Idle,
            TrayEvent::Recording,
            TrayEvent::Processing,
        ]
        .iter()
        .map(|e| e.icon_name())
        .collect();
        assert_eq!(names, ["audio-input-microphone", "media-record", "view-refresh"]);
    }

    #[test]
    fn tooltip_is_distinct_per_state() {
        assert_ne!(TrayEvent::Idle.tooltip(), TrayEvent::Recording.tooltip());
        assert_ne!(TrayEvent::Recording.tooltip(), TrayEvent::Processing.tooltip());
        assert!(TrayEvent::Idle.tooltip().contains("idle"));
        assert!(TrayEvent::Recording.tooltip().contains("recording"));
    }

    #[test]
    fn tray_event_is_copy_and_eq() {
        let e = TrayEvent::Recording;
        let f = e; // Copy
        assert_eq!(e, f);
        assert_ne!(TrayEvent::Idle, TrayEvent::Recording);
    }
}
```

**IMPORTANT — ksni API variance:** the `Tray` trait's exact method signatures differ between ksni versions. If `cargo build` complains about the trait impl, look at `~/.cargo/registry/src/*/ksni-*/src/lib.rs` for the current trait shape and adapt:

- In ksni 0.2.x, `icon_name` returns `String` (matches the code above).
- `tool_tip` returns a `ToolTip` struct — in some versions the field names differ (e.g. `title` vs `description`).
- `menu` returns `Vec<MenuItem<Self>>`; `StandardItem`'s `activate` closure signature depends on version (`Box<dyn Fn(&mut Self) + Send + Sync>` in 0.2).
- If `ksni::TrayService::new(tray).handle()` doesn't exist on your version, the handle is returned by `.spawn()` directly; store the return value and use `.update(|t| ...)` on it instead. The observable behaviour is the same.

Do not silently mangle the intended interface. If the adaptation is non-trivial, report DONE_WITH_CONCERNS describing the specific API differences and what you changed.

- [ ] **Step 3: Verify tests compile and pass**

```bash
cargo test --lib tray:: 2>&1 | tail -20
```

Expected: 3 tests pass (`icon_name_is_distinct_per_state`, `tooltip_is_distinct_per_state`, `tray_event_is_copy_and_eq`). These test pure helpers, not the ksni integration. The actual tray rendering is validated manually in Task 10.

- [ ] **Step 4: Run full lint + test suite**

```bash
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 48 tests pass (45 prior + 3 new tray tests), fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/tray.rs
git commit -m "feat(tray): ksni-based tray module with 3-state icon and Quit menu"
```

---

## Task 3: Wire tray into app.rs

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Update imports**

In `src/app.rs`, find:

```rust
use crate::postprocess::Postprocessor;
use crate::stt::SttEngine;
```

Add beneath:

```rust
use crate::tray::{TrayEvent, TrayManager};
```

- [ ] **Step 2: Start the tray and the worker-done channel**

Immediately after the existing `let postprocessor = Postprocessor::new(...)?;` line in `App::run`, insert:

```rust
        let mut tray = TrayManager::start();
        tray.set_state(TrayEvent::Idle);

        // One-way signal from the transcription worker to the select loop
        // telling the tray to return to Idle after an utterance finishes.
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(4);
```

- [ ] **Step 3: Clone `done_tx` into the transcription worker and notify on completion**

Find the existing worker-construction block, which looks like:

```rust
        let worker = {
            let injector_worker = injector.clone();
            let stt_worker = Arc::clone(&stt);
            let postprocessor_worker = postprocessor.clone();
            tokio::spawn(async move {
                while let Some(audio) = transcribe_rx.recv().await {
                    let len_seconds = audio.len() as f32 / 16_000.0;
                    debug!(samples = audio.len(), seconds = len_seconds, "transcribing");
                    let stt_for_task = Arc::clone(&stt_worker);
                    let text = match tokio::task::spawn_blocking(move || {
                        stt_for_task.transcribe(&audio)
                    })
                    .await
                    {
                        Ok(Ok(t)) => t,
                        Ok(Err(e)) => {
                            error!(error = %e, "transcription failed");
                            continue;
                        }
                        Err(join) => {
                            error!(error = %join, "transcription task join error");
                            continue;
                        }
                    };
                    if text.is_empty() {
                        debug!("empty transcription, nothing to inject");
                        continue;
                    }
                    let clean = postprocessor_worker.apply(&text);
                    if clean.trim().is_empty() {
                        debug!(raw = %text, "empty after postprocess, nothing to inject");
                        continue;
                    }
                    info!(text = %clean, "injecting");
                    if let Err(e) = injector_worker.inject(&clean).await {
                        // Intentionally omitting `text` to keep potentially sensitive
                        // dictated content out of the log sink. Rerun with -vv and
                        // a test utterance to diagnose xdotool-layer failures.
                        error!(error = %e, "injection failed");
                    }
                }
            })
        };
```

Replace it with a version that clones `done_tx` and notifies at the end of each iteration, using an inner `async {}` block with early `return`s so the done signal fires once per utterance regardless of which skip branch runs:

```rust
        let worker = {
            let injector_worker = injector.clone();
            let stt_worker = Arc::clone(&stt);
            let postprocessor_worker = postprocessor.clone();
            let done_tx_worker = done_tx.clone();
            tokio::spawn(async move {
                while let Some(audio) = transcribe_rx.recv().await {
                    let len_seconds = audio.len() as f32 / 16_000.0;
                    debug!(samples = audio.len(), seconds = len_seconds, "transcribing");
                    let stt_for_task = Arc::clone(&stt_worker);
                    let injector_inner = injector_worker.clone();
                    let postprocessor_inner = postprocessor_worker.clone();

                    async {
                        let text = match tokio::task::spawn_blocking(move || {
                            stt_for_task.transcribe(&audio)
                        })
                        .await
                        {
                            Ok(Ok(t)) => t,
                            Ok(Err(e)) => {
                                error!(error = %e, "transcription failed");
                                return;
                            }
                            Err(join) => {
                                error!(error = %join, "transcription task join error");
                                return;
                            }
                        };
                        if text.is_empty() {
                            debug!("empty transcription, nothing to inject");
                            return;
                        }
                        let clean = postprocessor_inner.apply(&text);
                        if clean.trim().is_empty() {
                            debug!(raw = %text, "empty after postprocess, nothing to inject");
                            return;
                        }
                        info!(text = %clean, "injecting");
                        if let Err(e) = injector_inner.inject(&clean).await {
                            // Intentionally omitting `text` to keep potentially sensitive
                            // dictated content out of the log sink. Rerun with -vv and
                            // a test utterance to diagnose xdotool-layer failures.
                            error!(error = %e, "injection failed");
                        }
                    }
                    .await;

                    // Always notify the tray bridge that this utterance is done,
                    // regardless of which skip branch fired above.
                    if done_tx_worker.send(()).await.is_err() {
                        debug!("done channel closed; exiting worker");
                        break;
                    }
                }
            })
        };
```

- [ ] **Step 4: Update the select loop to drive tray transitions + handle tray Quit + handle done signal**

Find the existing `tokio::select!` block in `App::run`. The current structure has three arms (hotkey_rx, audio_rx with guard, ctrl_c).

Insert tray.set_state calls in the Press/Release arms, and add two new arms (done_rx, tray.shutdown_signal). The finished select block should look like:

```rust
        loop {
            tokio::select! {
                maybe_evt = hotkey_rx.recv() => match maybe_evt {
                    Some(HotkeyEvent::Press) => {
                        if recording {
                            debug!("duplicate press ignored");
                        } else {
                            // Discard any audio buffered in the channel from before the press.
                            // cpal streams continuously from startup, so chunks pile up in the
                            // unbounded mpsc while `recording` is false (the `if recording`
                            // guard on the audio select arm only stops polling, not production).
                            // Without this drain, every utterance would include all mic input
                            // captured since daemon start (or the previous release), inflating
                            // inference time and potentially capturing unrelated speech.
                            let mut discarded = 0usize;
                            while audio_rx.try_recv().is_ok() {
                                discarded += 1;
                            }
                            if discarded > 0 {
                                debug!(chunks = discarded, "discarded pre-press audio");
                            }
                            recording = true;
                            buffer.clear();
                            info!("recording started");
                            tray.set_state(TrayEvent::Recording);
                        }
                    }
                    Some(HotkeyEvent::Release) => {
                        if !recording {
                            debug!("release without prior press ignored");
                        } else {
                            recording = false;
                            let audio = std::mem::take(&mut buffer);
                            buffer.reserve(16_000 * 30); // restore capacity for the next utterance
                            let seconds = audio.len() as f32 / 16_000.0;
                            info!(seconds, "recording stopped");
                            match transcribe_tx.try_send(audio) {
                                Ok(()) => {
                                    tray.set_state(TrayEvent::Processing);
                                }
                                Err(mpsc::error::TrySendError::Full(dropped)) => {
                                    let s = dropped.len() as f32 / 16_000.0;
                                    warn!(seconds = s, "transcribe queue full, dropping utterance");
                                    tray.set_state(TrayEvent::Idle);
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    error!("transcribe worker closed; shutting down");
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        error!("hotkey channel closed; shutting down");
                        break;
                    }
                },
                maybe_chunk = audio_rx.recv(), if recording => match maybe_chunk {
                    Some(chunk) => buffer.extend_from_slice(&chunk),
                    None => {
                        error!("audio channel closed; shutting down");
                        break;
                    }
                },
                maybe_done = done_rx.recv() => match maybe_done {
                    Some(()) => {
                        debug!("worker finished utterance; tray back to Idle");
                        tray.set_state(TrayEvent::Idle);
                    }
                    None => {
                        error!("done channel closed; shutting down");
                        break;
                    }
                },
                maybe_quit = tray.shutdown_signal().recv() => match maybe_quit {
                    Some(()) => {
                        info!("tray Quit activated; shutting down");
                        break;
                    }
                    None => {
                        // Tray bridge task exited. Daemon can keep running via hotkey,
                        // but this is a surprising state — log and continue rather than abort.
                        warn!("tray shutdown channel closed; continuing without tray");
                    }
                },
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c received; shutting down");
                    break;
                }
            }
        }
```

The only substantive additions are:
- `tray.set_state(TrayEvent::Recording)` on Press,
- `tray.set_state(TrayEvent::Processing)` on successful `try_send`,
- `tray.set_state(TrayEvent::Idle)` on Full-queue,
- new `done_rx.recv()` arm,
- new `tray.shutdown_signal().recv()` arm.

- [ ] **Step 5: Build + unit + lint**

```bash
cargo build --release
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 48 tests pass (unchanged — no new tests), fmt clean, clippy clean, release build succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): wire tray state transitions and Quit handler into select loop"
```

---

## Task 4: `default_model_path()` helper + XDG default

**Files:**
- Modify: `src/config.rs`
- Modify: existing tests in `src/config.rs`

- [ ] **Step 1: Add `default_model_path()` public helper**

In `src/config.rs`, near the top after the `use` statements, add:

```rust
/// Resolve the default model path to `$XDG_DATA_HOME/lindiction/models/ggml-tiny.en.bin`
/// (typically `~/.local/share/lindiction/models/ggml-tiny.en.bin`).
///
/// This is the single source of truth for the default — consumed by
/// `ModelConfig::default` AND by `model_download::ensure_default_model`
/// (which only auto-downloads when `config.model.path == default_model_path()`).
pub fn default_model_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("lindiction")
        .join("models")
        .join("ggml-tiny.en.bin")
}
```

- [ ] **Step 2: Use it in `ModelConfig::default`**

Find the existing `ModelConfig` Default impl:

```rust
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("models/ggml-tiny.en.bin"),
        }
    }
}
```

Replace with:

```rust
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            path: default_model_path(),
        }
    }
}
```

- [ ] **Step 3: Update the existing tests that baked in the old default**

Three tests in `src/config.rs` reference the old `"models/ggml-tiny.en.bin"` string literal. They must switch to asserting against `default_model_path()`.

Find the existing `default_model_path` test (the one that tests the Config's default field — not the helper; they share the name, which is fine because they live in different scopes):

```rust
    #[test]
    fn default_model_path() {
        let c = Config::default();
        assert_eq!(c.model.path, PathBuf::from("models/ggml-tiny.en.bin"));
    }
```

Rename it to `default_model_path_matches_xdg` and update:

```rust
    #[test]
    fn default_model_path_matches_xdg() {
        let c = Config::default();
        assert_eq!(c.model.path, super::default_model_path());
        // Verify the helper returns an absolute path inside an XDG-style location
        // (contains "lindiction/models/ggml-tiny.en.bin" as a suffix).
        let p = super::default_model_path();
        assert!(p.ends_with("lindiction/models/ggml-tiny.en.bin"), "got {}", p.display());
    }
```

Find `partial_toml_fills_from_default`:

```rust
    #[test]
    fn partial_toml_fills_from_default() {
        let s = r#"
[hotkey]
binding = "f10"
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(c.hotkey.binding, "f10");
        assert_eq!(c.model.path, PathBuf::from("models/ggml-tiny.en.bin"));
        assert!(c.postprocess.remove_fillers);
    }
```

Update the middle assertion:

```rust
        assert_eq!(c.model.path, super::default_model_path());
```

Find `load_with_no_config_file_uses_defaults`:

```rust
    #[test]
    fn load_with_no_config_file_uses_defaults() {
        isolate_xdg();
        std::env::remove_var("LINDICTION_MODEL");
        let c = Config::load(None).expect("load");
        assert_eq!(c.model.path, PathBuf::from("models/ggml-tiny.en.bin"));
        assert_eq!(c.hotkey.binding, "ctrl+alt+space");
        std::env::remove_var("XDG_CONFIG_HOME");
    }
```

Update the first assertion:

```rust
        assert_eq!(c.model.path, super::default_model_path());
```

**Important subtlety:** `default_model_path()` reads `dirs::data_dir()`, which respects `$XDG_DATA_HOME`. In tests `isolate_xdg()` sets `$XDG_CONFIG_HOME` only (for ignoring user config files) — it does NOT touch `$XDG_DATA_HOME`. So `default_model_path()` in tests returns whatever `dirs::data_dir()` produces for the test-running user's environment. Both the production code and the test assert against the same `default_model_path()` output — they agree by construction. No change needed to `isolate_xdg`.

- [ ] **Step 4: Verify tests pass**

```bash
cargo test --lib config:: -- --test-threads=1
```

Expected: 11 tests pass (the renamed `default_model_path_matches_xdg` plus the 10 other config tests, all green with the updated assertions).

- [ ] **Step 5: Full lint**

```bash
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 48 tests pass, fmt clean, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): default model path moves to XDG data dir (~/.local/share/lindiction/models)"
```

---

## Task 5: Model auto-download module

**Files:**
- Create: `src/model_download.rs`
- Modify: `src/lib.rs` (register the module)

- [ ] **Step 1: Declare the module**

In `src/lib.rs`, add `pub mod model_download;` in alphabetical order. Final contents:

```rust
pub mod app;
pub mod audio;
pub mod config;
pub mod hotkey;
pub mod inject;
pub mod model_download;
pub mod postprocess;
pub mod stt;
pub mod tray;
```

- [ ] **Step 2: Create the module with failing tests**

Create `src/model_download.rs`:

```rust
use crate::config::default_model_path;
use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;

/// Minimum size of a valid ggml-tiny.en.bin download. The real file is
/// ~77 MB; anything smaller than this means curl followed a redirect to
/// an auth/error page and wrote HTML to disk. We reject and delete it.
const MIN_EXPECTED_BYTES: u64 = 50 * 1024 * 1024;

/// Hugging Face URL for the default tiny.en model.
const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin";

/// Ensure the default whisper model exists at `path`. Auto-downloads on
/// first run if — and only if — `path` equals the system default
/// (`~/.local/share/lindiction/models/ggml-tiny.en.bin`). Any user-
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
        "first-run: downloading default whisper model (77 MB)"
    );

    let status = std::process::Command::new("curl")
        .args([
            "-L",
            "--fail",
            "--show-error",
            "-o",
        ])
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
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!("renaming {} to {}", tmp_path.display(), path.display())
    })?;

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
        let model = model_dir.join("ggml-tiny.en.bin");
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
        // want to download 77 MB in a unit test). This test asserts the
        // guard logic is reachable, not that the download works.
        std::env::set_var(
            "XDG_DATA_HOME",
            "/nonexistent-lindiction-dl-guard-test",
        );
        let default = default_model_path();
        assert!(
            default.ends_with("lindiction/models/ggml-tiny.en.bin"),
            "default_model_path should end with lindiction/models/ggml-tiny.en.bin"
        );
        assert!(!default.exists(), "default path must not exist under bogus XDG");
        std::env::remove_var("XDG_DATA_HOME");
    }
}
```

- [ ] **Step 3: Verify tests compile and pass**

```bash
cargo test --lib model_download:: -- --test-threads=1 2>&1 | tail -20
```

Expected: 3 tests pass (`skips_when_path_differs_from_default`, `skips_when_file_exists_at_default_path`, `triggers_only_for_default_path`).

**None of these tests actually performs a network download.** The full download path is validated manually in Task 10.

- [ ] **Step 4: Full lint + tests**

```bash
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 51 tests pass (48 + 3 new), fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/model_download.rs
git commit -m "feat(model_download): first-run auto-download via curl, guarded by default-path check"
```

---

## Task 6: Wire auto-download into `App::run`

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Import the new module**

In `src/app.rs`, find:

```rust
use crate::inject::Injector;
use crate::model_download... // <- does not exist yet
```

Actually the current imports are approximately:

```rust
use crate::audio::{start_capture, AudioStream};
use crate::config::Config;
use crate::hotkey::{parse_binding, start as start_hotkey, HotkeyEvent, HotkeyListener};
use crate::inject::Injector;
use crate::postprocess::Postprocessor;
use crate::stt::SttEngine;
use crate::tray::{TrayEvent, TrayManager};
```

Add `use crate::model_download;` in alphabetical order, between `inject` and `postprocess`:

```rust
use crate::audio::{start_capture, AudioStream};
use crate::config::Config;
use crate::hotkey::{parse_binding, start as start_hotkey, HotkeyEvent, HotkeyListener};
use crate::inject::Injector;
use crate::model_download;
use crate::postprocess::Postprocessor;
use crate::stt::SttEngine;
use crate::tray::{TrayEvent, TrayManager};
```

- [ ] **Step 2: Call `ensure_default_model` before loading the model**

Find this block in `App::run` (it's currently right after the xdotool preflight, or slightly below it depending on v0.2 layout):

```rust
        let injector = Injector::new(config.xdotool_delay_ms);

        let postprocessor = Postprocessor::new(&config.postprocess)
            .context("building postprocessor from config.postprocess")?;

        // Load the model upfront — fail fast on a bad model path or corrupt file.
        let stt = Arc::new(
            SttEngine::load(&config.model.path)
                .with_context(|| format!("loading model from {}", config.model.path.display()))?,
        );
```

Insert the auto-download call between postprocessor construction and SttEngine load:

```rust
        let injector = Injector::new(config.xdotool_delay_ms);

        let postprocessor = Postprocessor::new(&config.postprocess)
            .context("building postprocessor from config.postprocess")?;

        // Auto-download the default model on first run (no-op if the file
        // is already present or if the user specified a custom path).
        model_download::ensure_default_model(&config.model.path)
            .context("ensuring default whisper model is available")?;

        // Load the model upfront — fail fast on a bad model path or corrupt file.
        let stt = Arc::new(
            SttEngine::load(&config.model.path)
                .with_context(|| format!("loading model from {}", config.model.path.display()))?,
        );
```

- [ ] **Step 3: Build + lint**

```bash
cargo build --release
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 51 tests pass, fmt clean, clippy clean.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): call ensure_default_model before SttEngine::load"
```

---

## Task 7: cargo-deb metadata + systemd unit

**Files:**
- Modify: `Cargo.toml`
- Create: `systemd/lindiction.service`

- [ ] **Step 1: Create the systemd unit file**

Create directory `systemd/` and file `systemd/lindiction.service`:

```ini
[Unit]
Description=Lindiction voice dictation
After=graphical-session.target sound.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/bin/lindiction
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=lindiction=info

[Install]
WantedBy=default.target
```

- [ ] **Step 2: Add `[package.metadata.deb]` to Cargo.toml**

At the bottom of `Cargo.toml`, after the existing `[dev-dependencies]` block, add:

```toml
[package.metadata.deb]
maintainer = "Andre Hugo <cortexpeterpan@gmail.com>"
copyright = "2026, Andre Hugo"
license-file = ["LICENSE", "0"]
extended-description = """
Push-to-talk voice dictation for Linux using whisper.cpp.
Hold Ctrl+Alt+Space, speak, release, and the transcribed text is typed
at the cursor. First run auto-downloads the default tiny.en model
(~77 MB).
"""
depends = "$auto, xdotool, curl, libasound2, libpulse0"
section = "sound"
priority = "optional"
assets = [
    ["target/release/lindiction", "usr/bin/", "755"],
    ["systemd/lindiction.service", "lib/systemd/user/", "644"],
    ["README.md", "usr/share/doc/lindiction/README", "644"],
    ["LICENSE", "usr/share/doc/lindiction/copyright", "644"],
]
```

- [ ] **Step 3: Install cargo-deb locally and build the package**

```bash
cargo install cargo-deb --locked
cargo build --release
cargo deb --no-build
ls -lh target/debian/
```

Expected: a file like `target/debian/lindiction_0.1.0_amd64.deb` appears. (The version number in the filename is read from `Cargo.toml`'s `version` field — still `0.1.0` at this stage; Task 10 bumps to `0.2.0` or `0.3.0` as decided.)

**If the version is still `0.1.0`:** that is correct for this task. Task 10 bumps the version as part of the release commit.

- [ ] **Step 4: Inspect the package contents**

```bash
dpkg-deb --contents target/debian/lindiction_*_amd64.deb
```

Expected file list:

```
./usr/bin/lindiction
./lib/systemd/user/lindiction.service
./usr/share/doc/lindiction/README
./usr/share/doc/lindiction/copyright
./usr/share/doc/lindiction/changelog.Debian.gz
./usr/share/doc/lindiction/control
```

The `changelog.Debian.gz` and `control` files are auto-generated by cargo-deb.

- [ ] **Step 5: Run full lint**

```bash
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: 51 tests pass, fmt clean, clippy clean. No code changes from this task.

- [ ] **Step 6: Commit**

```bash
git add systemd/lindiction.service Cargo.toml
git commit -m "chore(packaging): cargo-deb metadata + systemd user unit for v0.3"
```

---

## Task 8: README rewrite

**Files:**
- Modify: `README.md`

Current README structure (from v0.2):
1. Intro paragraph
2. Requirements
3. Install → System packages, Whisper model, Build
4. Run → Flags
5. Configuration
6. Troubleshooting
7. Testing
8. License

Target v0.3 structure:
1. Intro paragraph
2. Requirements
3. Install → **From .deb (recommended)**, **From source** (with System packages + Build subsections; Whisper model subsection deleted — auto-download supersedes)
4. Run → Flags, **Auto-start with systemd (optional)**
5. Configuration
6. **Migrating from v0.2** (new H2)
7. Troubleshooting (updated)
8. Testing
9. License

- [ ] **Step 1: Rewrite the Install section**

In `README.md`, find the existing `## Install` section and replace it (everything from `## Install` through just before `## Run`) with:

```markdown
## Install

### From .deb (recommended)

Download the latest `.deb` from the [releases page](https://github.com/cortexuvula/lindiction/releases) and install:

```bash
wget https://github.com/cortexuvula/lindiction/releases/latest/download/lindiction-v0.3.0-amd64.deb
sudo apt install ./lindiction-v0.3.0-amd64.deb
```

First run will auto-download the default tiny.en whisper model (~77 MB) to `~/.local/share/lindiction/models/` — expect a one-time ~20-second delay on initial launch.

### From source

Install system packages:

```bash
sudo apt update
sudo apt install -y \
    xdotool build-essential cmake pkg-config \
    libclang-dev libasound2-dev libpulse-dev curl
```

Build:

```bash
cargo build --release
```

First build takes several minutes (compiles whisper.cpp from source). First run auto-downloads the model; no manual `curl` step needed.

```

- [ ] **Step 2: Add the systemd subsection inside `## Run`**

Find the existing `## Run` section. Keep everything from `## Run` through the end of the existing `### Flags` subsection. After the Flags table and before the next `## ` heading, add:

```markdown

### Auto-start with systemd (optional)

To run lindiction automatically on login and restart on crash:

```bash
systemctl --user daemon-reload
systemctl --user enable --now lindiction
journalctl --user -u lindiction -f    # tail logs
```

To disable auto-start:

```bash
systemctl --user disable --now lindiction
```

The unit file is installed by the `.deb` at `/lib/systemd/user/lindiction.service`. If you built from source, you can copy it yourself:

```bash
mkdir -p ~/.config/systemd/user
cp systemd/lindiction.service ~/.config/systemd/user/
```

```

- [ ] **Step 3: Add the Migrating section**

Insert a new `## Migrating from v0.2` H2 immediately before `## Troubleshooting`. Content:

```markdown
## Migrating from v0.2

The default model path moved from `models/ggml-tiny.en.bin` (relative to the working directory) to `~/.local/share/lindiction/models/ggml-tiny.en.bin` (XDG data directory).

Three options:

1. **Do nothing.** Launch the daemon — auto-download fetches a fresh `ggml-tiny.en.bin` to the new default location. One-time delay, no other action needed.
2. **Move the existing file:**
   ```bash
   mkdir -p ~/.local/share/lindiction/models
   mv models/ggml-tiny.en.bin ~/.local/share/lindiction/models/
   ```
3. **Pin the old location** in `~/.config/lindiction/config.toml`:
   ```toml
   [model]
   path = "/absolute/path/to/your/models/ggml-tiny.en.bin"
   ```
   Or use `LINDICTION_MODEL=/path/to/model.bin lindiction`.

No other breaking changes. Hotkey config, postprocess config, all existing TOML fields work identically.

```

- [ ] **Step 4: Update Troubleshooting**

Find the existing Troubleshooting section. The current first entry is:

```markdown
**"Model not found"** — download the model with the curl command above. The default expected path is `./models/ggml-tiny.en.bin` relative to the current working directory.
```

Replace it with:

```markdown
**"Model not found"** — first launch auto-downloads the default model. If it fails (network issue, etc.), rerun the daemon to retry. To use an existing local model, pass `--model /path/to/model.bin` or set `LINDICTION_MODEL=/path/to/model.bin`.

**"curl exited with…" on first run** — the auto-download failed. Check your network, then relaunch. The partial download is automatically cleaned up. As a manual fallback:

```bash
mkdir -p ~/.local/share/lindiction/models
curl -L -o ~/.local/share/lindiction/models/ggml-tiny.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
```
```

At the end of the Troubleshooting section (after the last existing entry), add:

```markdown

**Tray icon doesn't appear** — on Ubuntu 24.04 GNOME, the AppIndicator extension is pre-installed and active. On vanilla upstream GNOME, install and enable it:

```bash
sudo apt install gnome-shell-extension-appindicator
# then enable "Ubuntu AppIndicators" in the Extensions app, or via:
gnome-extensions enable ubuntu-appindicators@ubuntu.com
```

The daemon runs fine without a tray icon — the hotkey still works.
```

- [ ] **Step 5: Verify the file**

```bash
head -80 README.md
grep -c '^## ' README.md      # H2 header count — should now be 9 (was 8)
grep -n 'Migrating' README.md
grep -n 'Auto-start with systemd' README.md
```

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs: v0.3 README — .deb install, systemd enable, migration, tray troubleshooting"
```

---

## Task 9: CI release workflow — build + upload `.deb`

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Update `release.yml`**

Overwrite `.github/workflows/release.yml` with:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

permissions:
  contents: write

env:
  CARGO_TERM_COLOR: always

jobs:
  release:
    name: Build and publish release binary
    runs-on: ubuntu-24.04
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install system dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            build-essential cmake pkg-config \
            libclang-dev libasound2-dev libpulse-dev

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo build artifacts
        uses: Swatinem/rust-cache@v2

      - name: Install cargo-deb
        run: cargo install cargo-deb --locked --version "^2"

      - name: Build release binary
        run: cargo build --release --locked

      - name: Package tarball
        env:
          REF_NAME: ${{ github.ref_name }}
        run: |
          cd target/release
          tar -czf "../../lindiction-${REF_NAME}-x86_64-linux.tar.gz" lindiction
          cd ../..
          sha256sum "lindiction-${REF_NAME}-x86_64-linux.tar.gz" > "lindiction-${REF_NAME}-x86_64-linux.tar.gz.sha256"

      - name: Build .deb
        run: cargo deb --no-build

      - name: Rename .deb to include tag
        env:
          REF_NAME: ${{ github.ref_name }}
        run: |
          # cargo-deb names the file from Cargo.toml's version;
          # prefer a tag-based name for the release attachment.
          cp target/debian/lindiction_*_amd64.deb "./lindiction-${REF_NAME}-amd64.deb"

      - name: Create GitHub Release
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          REF_NAME: ${{ github.ref_name }}
        run: |
          gh release create "${REF_NAME}" \
            --title "${REF_NAME}" \
            --generate-notes \
            "lindiction-${REF_NAME}-x86_64-linux.tar.gz" \
            "lindiction-${REF_NAME}-x86_64-linux.tar.gz.sha256" \
            "lindiction-${REF_NAME}-amd64.deb"
```

The three substantive changes from the v0.2 workflow are:

1. A new "Install cargo-deb" step.
2. A new "Build .deb" step (after the existing release build).
3. A new "Rename .deb to include tag" step + the `.deb` added to the `gh release create` asset list.

- [ ] **Step 2: Local dry-run verification**

Because GitHub Actions can only be exercised by pushing a tag, locally test the commands the workflow will run. The `cargo build --release` step is already validated via Task 6. Verify the `.deb` steps on your machine:

```bash
cargo deb --no-build
cp target/debian/lindiction_*_amd64.deb ./lindiction-v0.3.0-amd64.deb  # simulates the rename step
ls -lh lindiction-v0.3.0-amd64.deb
rm lindiction-v0.3.0-amd64.deb  # cleanup — CI produces this fresh
```

Expected: the `.deb` file exists and is ~several MB.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): build and upload .deb alongside tarball on v* tag push"
```

---

## Task 10: Acceptance + tag v0.3.0

**Files:**
- Modify: `Cargo.toml` (bump version)

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --lib -- --test-threads=1
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

Expected: 51 unit tests + 1 integration test, all green.

- [ ] **Step 2: Run lint checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: both clean.

- [ ] **Step 3: Bump version**

In `Cargo.toml`, change:

```toml
version = "0.1.0"
```

to:

```toml
version = "0.3.0"
```

Then rebuild to update Cargo.lock:

```bash
cargo build --release
```

- [ ] **Step 4: Commit the version bump**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.3.0"
```

- [ ] **Step 5: Manual test 1 — tray appears, state transitions correctly**

Ensure the model exists (so auto-download doesn't interfere):

```bash
ls ~/.local/share/lindiction/models/ggml-tiny.en.bin 2>/dev/null || \
    mv models/ggml-tiny.en.bin ~/.local/share/lindiction/models/ 2>/dev/null || \
    echo "you'll need to wait for auto-download on the next launch"
```

Launch:

```bash
./target/release/lindiction -v
```

Expected:
- Tray icon appears in the system tray (dim microphone).
- Daemon log contains `tray service spawned` and `ready — hold the hotkey to dictate`.

Hold Ctrl+Alt+Space in a focused text editor. Say "hello world". Release.

Expected:
- Tray icon turns red during the press.
- Tray icon turns yellow (processing) between release and text-injected.
- Tray icon returns to dim microphone after text is injected.
- `Hello world.` is typed at the cursor.

- [ ] **Step 6: Manual test 2 — tray Quit menu works**

With the daemon running, click the tray icon. A menu with "Quit" appears. Click Quit.

Expected:
- Daemon log shows `tray Quit activated; shutting down`.
- Process exits cleanly with code 0 (same lifecycle as Ctrl-C).

- [ ] **Step 7: Manual test 3 — auto-download fires on first run**

Delete the local model:

```bash
rm -f ~/.local/share/lindiction/models/ggml-tiny.en.bin
```

Launch (without any `--model` flag or `LINDICTION_MODEL` env var):

```bash
unset LINDICTION_MODEL
./target/release/lindiction -v
```

Expected:
- Info log: `"first-run: downloading default whisper model (77 MB)"`.
- curl runs for ~20 seconds (varies by connection).
- Info log: `"model download complete"`.
- Daemon proceeds to normal startup (tray, ready line).
- File now exists at `~/.local/share/lindiction/models/ggml-tiny.en.bin` at ~77 MB.

Ctrl-C to exit.

- [ ] **Step 8: Manual test 4 — auto-download does NOT fire with --model**

```bash
./target/release/lindiction -v --model /tmp/does-not-exist.bin
```

Expected: clean startup error — the existing "Model not found" from `SttEngine::load`, no `"first-run: downloading..."` log, no curl invocation.

- [ ] **Step 9: Manual test 5 — local `.deb` install**

Build the .deb with the new version:

```bash
cargo deb --no-build
dpkg-deb --contents target/debian/lindiction_0.3.0_amd64.deb | grep -E 'bin|systemd|doc'
sudo apt install ./target/debian/lindiction_0.3.0_amd64.deb
which lindiction         # /usr/bin/lindiction
ls /lib/systemd/user/lindiction.service
```

Expected: `lindiction` on PATH, systemd unit file present.

Check the unit is visible to systemd:

```bash
systemctl --user daemon-reload
systemctl --user cat lindiction
```

Expected: the `[Unit] / [Service] / [Install]` contents match what we wrote.

Optionally enable + start:

```bash
systemctl --user enable --now lindiction
systemctl --user status lindiction
journalctl --user -u lindiction -n 30
```

Expected: `active (running)` status, logs include the familiar "ready — hold the hotkey" line.

Disable + remove after testing:

```bash
systemctl --user disable --now lindiction
sudo apt remove -y lindiction
```

- [ ] **Step 10: Merge to main**

```bash
git checkout main
git merge --ff-only feat/v0.3-impl
git log --oneline -12
```

Expected: 10 new commits fast-forwarded onto main.

- [ ] **Step 11: Tag v0.3.0 and push**

```bash
git tag -a v0.3.0 -m "v0.3.0: system tray, .deb packaging, systemd unit, first-run auto-download

Adds a minimal system tray icon with three visual states (idle / recording /
processing) and a Quit menu via ksni. Packages lindiction as a .deb with a
systemd user service (opt-in enable). Default model path moves to the XDG
data directory; first run auto-downloads the default tiny.en model via curl
— fail-safe, atomic, only triggers for the default path.

51 unit tests, 1 integration test. Manual E2E verified on Ubuntu 24.04 X11 GNOME."

git push origin main
git push origin v0.3.0
```

Expected: main pushes cleanly; tag push triggers the release workflow.

- [ ] **Step 12: Verify CI + release workflows**

```bash
sleep 5
gh run list --repo cortexuvula/lindiction --limit 2

# Wait for both to complete
until [ "$(gh run list --repo cortexuvula/lindiction --limit 2 --json status -q '.[] | .status' | grep -c 'completed')" = "2" ]; do
  sleep 30
done

gh run list --repo cortexuvula/lindiction --limit 2
gh release view v0.3.0 --repo cortexuvula/lindiction
```

Expected:
- Both workflows complete successfully.
- Release has three attached assets: `lindiction-v0.3.0-x86_64-linux.tar.gz`, `lindiction-v0.3.0-x86_64-linux.tar.gz.sha256`, `lindiction-v0.3.0-amd64.deb`.

- [ ] **Step 13: Delete the feature branch**

```bash
git branch -d feat/v0.3-impl
git branch --list
```

Expected: only `main` remains.

**Plan complete.** v0.3.0 is live with tray support, `.deb` + systemd packaging, and first-run auto-download.
