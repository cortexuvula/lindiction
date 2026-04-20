# Lindiction v0.3 — Design

**Date:** 2026-04-20
**Status:** Approved, pre-implementation
**Predecessor:** `2026-04-19-lindiction-v0.2-design.md` (v0.2, tagged `v0.2.0`)

## Overview

Lindiction v0.3 adds three user-facing improvements to the shipped v0.2: a minimal system tray indicator, a `.deb` package with a systemd user-service unit, and first-run auto-download of the default whisper model. The dictation pipeline itself (cpal → whisper-rs → xdotool) stays untouched. Target platform remains Ubuntu 24.04 / X11 / GNOME.

These three pieces fit together under a single theme — *making v0.2 installable and self-contained for non-developers*. A user should be able to `sudo apt install ./lindiction_0.3.0_amd64.deb` and have a working dictation setup after a single `lindiction` invocation (which auto-downloads the model and launches).

## Scope

### In scope (v0.3)

- **System tray icon** via `ksni` with three visual states (idle / recording / processing) and a menu whose only item is Quit.
- **`.deb` package** via `cargo-deb`, including the binary, the systemd user unit, README, and LICENSE. No post-install hooks.
- **systemd user service** installed but *not* enabled. User opt-in via `systemctl --user enable --now lindiction`.
- **Model auto-download on first run** when the configured model path equals the default and the file is missing. Shells out to `curl`.
- **Model path default change** from `models/ggml-tiny.en.bin` (relative to CWD) to `${XDG_DATA_HOME:-~/.local/share}/lindiction/models/ggml-tiny.en.bin` (XDG-compliant absolute).
- **README updates** covering `.deb` install, systemd enable, migration from v0.2.
- **CI release workflow** produces a `.deb` artifact alongside the existing tarball, both attached to the GitHub release on `v*` tag push.

### Explicit non-goals (still deferred to v0.4+)

- Wayland support (injection fallback chain, Wayland hotkey path).
- VAD endpointing for toggle mode.
- IPC socket + separate CLI client.
- Streaming transcription.
- Multiple models (base.en, large, multilingual). Auto-download only fetches `ggml-tiny.en.bin`.
- Tray menu items beyond Quit. No "Open config", no "Show log", no "About".
- Left-click-toggles-recording (alternative UX to hotkey). Left-click opens the Quit menu only.
- Progress bar during auto-download. The download is "in flight" or "done" from the user's terminal — curl's own progress bar is TTY-dependent and breaks under systemd, so we log start+finish events and that's it.

## Architecture

### New dependencies

```toml
ksni = "0.3"   # StatusNotifier tray client, pure Rust, works from any thread
```

No new HTTP crate — auto-download shells out to `curl`, which is already in `apt install` docs and will become a `depends` entry in the `.deb`.

### New files

```
src/tray.rs                                          # new module
src/model_download.rs                                # new module
packaging/icons/lindiction-idle.png                  # 22×22 monochrome
packaging/icons/lindiction-recording.png             # 22×22 with a red accent
packaging/icons/lindiction-processing.png            # 22×22 with a yellow accent
systemd/lindiction.service                           # systemd user unit
docs/superpowers/specs/2026-04-20-lindiction-v0.3-design.md  # this file
```

### Modified files

```
Cargo.toml             # new deps + [package.metadata.deb] block
src/lib.rs             # declare two new modules
src/config.rs          # ModelConfig default → XDG data dir
src/app.rs             # wire tray + auto-download + worker→tray "done" channel
README.md              # Configuration, Install (.deb), Migrating from v0.2, Troubleshooting
.github/workflows/release.yml  # add cargo-deb step
```

## Architecture — tray

`ksni` is a pure-Rust StatusNotifier client. StatusNotifier is the freedesktop protocol underlying GNOME's AppIndicator, KDE's native tray, and XFCE's tray-plugin. On Ubuntu 24.04's Ubuntu-flavoured GNOME, the AppIndicator extension is pre-installed and active, so the tray icon renders out-of-box. On vanilla upstream GNOME, the user needs `gnome-shell-extension-appindicator` — documented in the README.

We chose `ksni` over the more widespread `tray-icon` crate specifically because `tray-icon` on Linux requires the calling thread to own a GTK event loop. That would force us to move the tokio runtime off the main thread, which would ripple through `#[tokio::main]` and break the current `App::run` shape. `ksni` has no such constraint — it uses zbus internally and integrates cleanly with tokio.

### `src/tray.rs` interface

```rust
pub enum TrayEvent {
    Idle,
    Recording,
    Processing,
}

pub struct TrayManager {
    state_tx: mpsc::Sender<TrayEvent>,
    shutdown_rx: mpsc::Receiver<()>,
    _service: ksni::Handle<LindictionTray>,
}

impl TrayManager {
    /// Construct and register the tray icon. Starts in Idle.
    pub fn start() -> Result<Self>;

    /// Push a state change to the tray. Never blocks — uses a small mpsc
    /// buffer so the caller (the select loop in app.rs) doesn't yield.
    pub fn set_state(&self, event: TrayEvent);

    /// The select loop awaits this to learn when the user picks Quit.
    pub fn shutdown_signal(&mut self) -> &mut mpsc::Receiver<()>;
}
```

Internally `LindictionTray` implements `ksni::Tray`:
- `icon_name()` — returns one of three theme-independent icon names bundled with the crate (or a custom `.png` from `packaging/icons/`; the simpler path is to ship the PNGs and use them via `icon_pixmap`).
- `title()` / `id()` — static `"lindiction"`.
- `menu()` — one `StandardItem` labeled `"Quit"`, which sends `()` on the shutdown channel.
- `activate()` (left-click) — opens the menu by default; we don't override.

The `ksni::TrayService::new(tray).spawn()` call returns a `Handle` that lives inside `TrayManager`. State updates go through the `state_tx` channel; a dedicated async task inside `start()` reads from `state_rx` and calls `handle.update(|tray| tray.current_icon = ...)` for each event. Dropping `TrayManager` unregisters the tray cleanly.

### State-machine wire-up in `app.rs`

Four touch points in the main loop:

| Where | State event |
|---|---|
| Startup, after tray starts | `Idle` (implicit — that's the initial state) |
| `HotkeyEvent::Press` arm | `Recording` |
| `HotkeyEvent::Release` arm (after `try_send` to transcribe channel) | `Processing` |
| New channel: worker → select loop "done" signal, received in a new select arm | `Idle` |

One new `done_tx: mpsc::Sender<()>` threaded through the transcription worker. After the worker finishes either the inject call (success or failure), it sends `()` on `done_tx`. The main select loop has a new arm:

```rust
Some(()) = done_rx.recv() => {
    tray.set_state(TrayEvent::Idle);
}
```

Plus a fifth arm for shutdown-from-tray:

```rust
Some(()) = tray.shutdown_signal().recv() => {
    info!("tray Quit; shutting down");
    break;
}
```

Ctrl-C and tray-Quit converge on the same exit path.

## Architecture — model auto-download

New module `src/model_download.rs`.

### `ensure_default_model(path: &Path) -> Result<()>`

Behaviour:

1. If `path != default_model_path()` (user-configured path), return `Ok(())` immediately. Never auto-download to a user-specified location.
2. If `path.exists()`, return `Ok(())`.
3. Otherwise: create parent directory (`~/.local/share/lindiction/models/`), then shell out:
   ```
   curl -L --fail --show-error -o <path>.tmp https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
   ```
4. On curl success: atomic rename `<path>.tmp → <path>`. Return Ok.
5. On curl failure: delete `<path>.tmp` (best-effort), return an error naming the URL, the target path, and a hint that `--model` can point to an existing local model.

### Integration in `App::run`

Insert between the xdotool preflight and `SttEngine::load`:

```rust
// Preflight: verify xdotool is on PATH.
if which::which("xdotool").is_err() { ... }

// v0.3 addition: auto-download model on first run (default path only).
model_download::ensure_default_model(&config.model.path)
    .with_context(|| format!("ensuring model at {}", config.model.path.display()))?;

// Load the model upfront...
let stt = Arc::new(SttEngine::load(&config.model.path)?);
```

`ensure_default_model` is synchronous and blocking (curl is a child process we wait on). App::run is `async`, so the call happens inline — which is fine because App::run runs on the tokio runtime's main task and no other work is happening yet (tray, hotkey, audio all start after). The ~20 seconds of download at startup are visible to the user on first run only.

### Logging

- Info: `"first-run: downloading default model (75 MB) from <URL> to <path>"`.
- Info after success: `"model download complete"`.
- Error: surfaces through anyhow's chain — curl's stderr appears in the outer error message via `--show-error`.

## Model path default change

In `src/config.rs`, `ModelConfig::default()` currently returns:

```rust
Self { path: PathBuf::from("models/ggml-tiny.en.bin") }
```

Change to:

```rust
Self { path: default_model_path() }
```

Where `default_model_path()` is a new helper:

```rust
pub fn default_model_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("lindiction")
        .join("models")
        .join("ggml-tiny.en.bin")
}
```

`dirs::data_dir()` on Linux returns `$XDG_DATA_HOME` or `~/.local/share`. The unlikely fallback to a relative `.local/share` path protects against a broken environment the same way v0.2's XDG-config lookup did.

`default_model_path()` is the single source of truth used by:
- `ModelConfig::default` (the Config default)
- `model_download::ensure_default_model` (the auto-download guard comparing `path == default`)

No other callers. Expose it as `pub` from `config` (or a new `paths` module) so `model_download` can import it.

## systemd unit

`systemd/lindiction.service`:

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

Chosen because:
- `After=graphical-session.target` ensures X11 + PipeWire are up before the daemon tries to grab a hotkey.
- `PartOf=graphical-session.target` ties lifecycle to the session — the service stops when the user logs out.
- `Restart=on-failure` handles crashes; `RestartSec=3` prevents tight loops.
- `Environment=RUST_LOG=lindiction=info` gives a reasonable default log level under systemd (no `-v` CLI flag in play).
- `WantedBy=default.target` makes `systemctl --user enable lindiction` attach to the user's default target.

Service runs as the installing user (no `User=` override — user-level units run as the invoking user by definition).

## `.deb` via `cargo-deb`

Add to `Cargo.toml`:

```toml
[package.metadata.deb]
maintainer = "Andre Hugo <cortexpeterpan@gmail.com>"
copyright = "2026, Andre Hugo"
license-file = ["LICENSE", "0"]
extended-description = "Push-to-talk voice dictation for Linux using whisper.cpp. Hold Ctrl+Alt+Space, speak, release, and the transcribed text is typed at the cursor."
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

`$auto` lets cargo-deb resolve runtime shared-library dependencies automatically (glibc, libgcc, etc.). Explicit `xdotool`, `curl`, `libasound2`, `libpulse0` cover the things we shell out to or dlopen.

No `maintainer-scripts` (no `postinst` / `prerm`) — installation is dumb-copy; the user explicitly enables the systemd unit when ready.

Generated package name: `lindiction_0.3.0_amd64.deb`.

## CI release workflow — add `.deb` step

Modify `.github/workflows/release.yml`:

1. After `cargo build --release`, add:
   ```yaml
   - name: Install cargo-deb
     run: cargo install cargo-deb --locked

   - name: Build .deb
     run: cargo deb --no-build
   ```
   `--no-build` reuses the `target/release/lindiction` from the previous step.

2. After the existing `tar -czf ... lindiction` step, also package the `.deb` (or just reference it at its standard `target/debian/lindiction_<ver>_amd64.deb` location):
   ```yaml
   - name: Copy .deb to release root
     env:
       REF_NAME: ${{ github.ref_name }}
     run: cp target/debian/lindiction_*_amd64.deb "./lindiction-${REF_NAME}-amd64.deb"
   ```

3. Include the `.deb` in the release upload:
   ```yaml
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

## README changes

Four sections to update or add:

1. **Rewrite `## Install`** to lead with the `.deb` path (primary), then the build-from-source path (secondary, for developers). The `.deb` path becomes the recommended install; the existing source-build section stays but loses the explicit `curl` model-download step (replaced by the note "first run auto-downloads the model — expect a one-time ~20s delay on initial launch"). The existing System packages subsection and Whisper model subsection collapse into this new structure.

2. **New `## Running`** subsection for systemd:
   ```markdown
   ### Auto-start with systemd (optional)

   To run lindiction automatically on login and restart on crash:

   systemctl --user daemon-reload
   systemctl --user enable --now lindiction
   journalctl --user -u lindiction -f    # tail logs

   To disable auto-start:

   systemctl --user disable --now lindiction
   ```

3. **New `## Migrating from v0.2`** section documenting the default model path change and the `.deb` install path.

4. **Update `## Troubleshooting`**:
   - Rewrite the "Model not found" entry: first-run should auto-download; the entry now describes what to do if curl fails (check network, use `--model` to point to an existing local file).
   - Add: "Tray icon doesn't appear — on vanilla GNOME, install `gnome-shell-extension-appindicator` and enable it in Extensions. On Ubuntu's GNOME flavour, it's on by default."

## Error handling

New v0.3 error paths:

| Failure | Behaviour |
|---|---|
| `ksni::TrayService` fails to register | Warn + continue without the tray. Daemon still works via hotkey. Tray absence is non-fatal. |
| `curl` not on PATH | Bail at `ensure_default_model` with "curl not found. Install with: sudo apt install curl". |
| Auto-download network failure | Bail with curl's stderr + URL + target path. Partial `.tmp` file is deleted. User can retry by rerunning the daemon, or point `--model` elsewhere. |
| Target model directory can't be created | Bail with the full path and the underlying IO error. |

The tray-absence degradation (warn+continue) is deliberate. Dictation should work without the tray — the tray is a nice-to-have, not load-bearing. If StatusNotifier isn't available (non-freedesktop systems, deeply customised GNOME without AppIndicator), hotkey + audio + whisper + xdotool still function.

## Testing

### Unit tests

- `tray::test_state_events_sequence` — push Idle/Recording/Processing through a mock `TrayManager`, assert the internal state-change counter or last-event matches what was sent. This tests the channel plumbing without requiring a real DBus session bus.
- `model_download::test_default_path_returns_xdg` — assert `default_model_path()` under a controlled `XDG_DATA_HOME` returns the expected joined path.
- `model_download::test_skips_when_file_exists` — create a fake file at the default path, call `ensure_default_model`, assert no `curl` is spawned (observable via the function completing near-instantly).
- `model_download::test_skips_for_non_default_path` — call with a different path, assert the function returns immediately without spawning curl.

### Integration tests

No new integration tests. The existing `integration_stt` still validates the whisper boundary.

### Manual test plan

1. Delete `~/.local/share/lindiction/models/ggml-tiny.en.bin` (or rename so the path is missing). Launch daemon. Expected: info-level log "downloading default model..." appears, curl runs, ~20 s later the daemon comes up normally.
2. Relaunch the daemon after model is present. Expected: no download log, straight to model loading.
3. Pass `--model /tmp/does-not-exist.bin`. Expected: the old clear "Model not found" error — NO auto-download.
4. Tray icon appears in the system tray on Ubuntu 24.04 GNOME. Color dims at startup.
5. Hold Ctrl+Alt+Space → icon turns red; release → icon turns yellow; after inject → icon returns to dim.
6. Click the tray icon → menu with Quit appears. Click Quit → daemon exits cleanly (same log output as Ctrl-C).
7. Build `.deb` locally: `cargo deb`. Install via `sudo apt install ./target/debian/lindiction_0.3.0_amd64.deb`. Verify `/usr/bin/lindiction` and `/lib/systemd/user/lindiction.service` exist. Remove: `sudo apt remove lindiction`.
8. After `.deb` install: `systemctl --user daemon-reload && systemctl --user enable --now lindiction`. Wait a few seconds. `journalctl --user -u lindiction -n 50` should show startup logs including the auto-download (if the model wasn't already present) and "ready — hold the hotkey to dictate".

## Migration notes (from v0.2)

Users upgrading from v0.2 source builds hit one breaking change: the default model path moves from `models/ggml-tiny.en.bin` (relative to CWD) to `~/.local/share/lindiction/models/ggml-tiny.en.bin`.

Three migration options, in order of simplicity:

1. **Do nothing.** Launch the daemon. Auto-download kicks in and fetches `ggml-tiny.en.bin` to the new default location. First-run only.
2. **Move the existing file:**
   ```bash
   mkdir -p ~/.local/share/lindiction/models
   mv models/ggml-tiny.en.bin ~/.local/share/lindiction/models/
   ```
3. **Pin the old location in config** (for repo developers who want `cargo run` to keep using the checked-out path):
   ```toml
   [model]
   path = "/home/you/Development/linux-dictation/models/ggml-tiny.en.bin"
   ```
   Or equivalently `LINDICTION_MODEL=models/ggml-tiny.en.bin cargo run`.

Nothing else in v0.2's config schema changes. Hotkey, postprocess, all existing toggles work identically.

## Day-by-day schedule (≤3 days)

### Day 1 — Tray module

- Create `src/tray.rs` with `TrayManager`, `TrayEvent`, `LindictionTray`.
- Create the three icon PNGs (22×22, monochrome for idle, red dot for recording, yellow dot for processing).
- TDD the state-event channel plumbing.
- Wire into `app.rs`: new `done_tx/rx`, new shutdown arm in select, state transitions on Press/Release/Done.
- Manual verify: tray icon appears, color changes, Quit exits cleanly.

**Exit criterion:** `cargo test --lib` green, `cargo run` shows a working tray icon on Ubuntu.

### Day 2 — Auto-download + model-path default + packaging

- Move `default_model_path()` helper into `config.rs` (or a new `paths` module).
- Change `ModelConfig::default` to use it.
- Create `src/model_download.rs` with `ensure_default_model`.
- Wire into `App::run` between xdotool preflight and SttEngine::load.
- Add `[package.metadata.deb]` block to `Cargo.toml`.
- Create `systemd/lindiction.service`.
- Rewrite README's Install, add Running/systemd section, add Migrating from v0.2, update Troubleshooting.
- Local `cargo deb` test: produces a `.deb` without errors.

**Exit criterion:** `cargo deb` succeeds; README renders with all four updated sections; manually verify first-run auto-download on a system where the model is absent.

### Day 3 — CI + release

- Update `.github/workflows/release.yml` to install cargo-deb, run `cargo deb --no-build`, and upload the `.deb` to the GitHub release.
- Tag `v0.3.0`. Push. Verify CI green, release has both `.tar.gz` and `.deb` assets.
- Manually install the `.deb` from the release on a local machine, confirm end-to-end behaviour (tray, hotkey, systemd enable).

**Exit criterion:** v0.3.0 release is live with three attached assets (tarball, sha256, .deb); manual install of the .deb works end-to-end.

## Open questions — resolved during design

All of the following were asked and resolved during brainstorming; captured here so future readers don't re-open them:

- **Tray library**: `ksni`, not `tray-icon` — main-thread/GTK constraint.
- **systemd service default state**: installed but NOT enabled. User opts in explicitly.
- **Auto-download safeguard**: only fires for the default model path. Never downloads to a user-specified path.
- **HTTP library**: shell out to `curl`, no new crate.
- **Default model path**: XDG data dir (`~/.local/share/lindiction/models/ggml-tiny.en.bin`).
- **Tray menu scope**: Quit only. No config/log/about items in v0.3.
- **Auto-download progress bar**: none; simple start/done logs at info level.
- **Tray absence handling**: non-fatal; daemon logs a warn and proceeds without a tray icon.
