# Lindiction MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the single-binary `lindiction` push-to-talk voice dictation tool per the approved MVP spec — hold Ctrl+Alt+Space, speak, release, transcription appears at the cursor on Ubuntu 24.04 / X11 / GNOME.

**Architecture:** One async Rust binary. Tokio `select!` main loop in `app.rs` multiplexes hotkey events (from a background thread driven by `global-hotkey`) and audio frames (from the cpal callback thread). On hotkey release, the accumulated `Vec<f32>` audio buffer is handed to a dedicated transcription worker task over a bounded mpsc channel. The worker calls `whisper-rs` inside `spawn_blocking`, then shells out to `xdotool type` to inject text at the cursor. No VAD, no postprocess, no tray, no packaging — those are v0.2+.

**Tech Stack:** Rust 2021, Tokio, `cpal`, `whisper-rs`, `global-hotkey`, `xdotool` (system package), `clap`, `anyhow`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-04-18-lindiction-mvp-design.md`

**Risk:** Task 6 (hotkey crate spike) is the highest-unknown in this plan. If `global-hotkey` on X11 doesn't cleanly emit both `Pressed` and `Released` events or doesn't coexist with tokio, **stop and revise this plan** with the next candidate (`livesplit-hotkey`, then direct `x11` bindings). Do not silently substitute without revising.

---

## Task 1: System prep

**Files:** none (system packages + downloads).

- [ ] **Step 1: Install system packages**

Run:
```bash
sudo apt update
sudo apt install -y xdotool build-essential cmake pkg-config libclang-dev libasound2-dev libpulse-dev curl
```

Expected: apt installs these without errors. `libclang-dev` and `cmake` are for the whisper.cpp C++ build. `libasound2-dev` and `libpulse-dev` are for cpal's Linux backends. `xdotool` is what we shell out to for injection.

- [ ] **Step 2: Verify xdotool works**

Open a text editor (e.g., `gedit` or a terminal). Focus it. Run in another terminal:
```bash
xdotool type --delay 5 -- "hello from xdotool"
```

Expected: the string `hello from xdotool` appears in the focused window.

- [ ] **Step 3: Download the Whisper tiny.en model**

Run:
```bash
mkdir -p models
curl -L -o models/ggml-tiny.en.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
ls -lh models/ggml-tiny.en.bin
```

Expected: a ~75 MB file appears at `models/ggml-tiny.en.bin`.

- [ ] **Step 4: Record the test fixture**

Run:
```bash
mkdir -p tests/fixtures
arecord -f S16_LE -r 16000 -c 1 -d 3 tests/fixtures/hello.wav
```

Say "hello world" clearly during the 3-second recording window. Play it back to confirm:
```bash
aplay tests/fixtures/hello.wav
```

Expected: playback contains your voice saying "hello world".

- [ ] **Step 5: Verify the fixture is NOT committed and model IS gitignored**

Run:
```bash
git status
```

Expected: `tests/fixtures/hello.wav` appears as untracked (we'll commit it in Task 2). `models/ggml-tiny.en.bin` does NOT appear — it's ignored by `.gitignore`.

- [ ] **Step 6: Commit the fixture**

Run:
```bash
git add tests/fixtures/hello.wav
git commit -m "chore: add hello.wav test fixture (16 kHz mono WAV, 'hello world')"
```

---

## Task 2: Cargo project skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `src/config.rs`, `src/audio.rs`, `src/hotkey.rs`, `src/stt.rs`, `src/inject.rs`, `src/app.rs` (empty stubs)

- [ ] **Step 1: Create `Cargo.toml`**

Write `Cargo.toml`:
```toml
[package]
name = "lindiction"
version = "0.1.0"
edition = "2021"
description = "Linux push-to-talk voice dictation"
license = "MIT"

[[bin]]
name = "lindiction"
path = "src/main.rs"

[lib]
name = "lindiction"
path = "src/lib.rs"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
cpal = "0.15"
global-hotkey = "0.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "signal", "process"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
whisper-rs = "0.11"

[dev-dependencies]
hound = "3.5"   # for reading WAV in integration tests
```

A library target is included alongside the binary so integration tests in `tests/` can import modules (e.g., `stt::SttEngine`). `main.rs` stays thin and imports from the library.

- [ ] **Step 2: Create module stubs**

Write `src/lib.rs`:
```rust
pub mod app;
pub mod audio;
pub mod config;
pub mod hotkey;
pub mod inject;
pub mod stt;
```

Write `src/main.rs`:
```rust
fn main() {
    println!("lindiction skeleton — replaced in Task 9");
}
```

Write each of `src/config.rs`, `src/audio.rs`, `src/hotkey.rs`, `src/stt.rs`, `src/inject.rs`, `src/app.rs` as empty files (or a single `// stub` comment).

- [ ] **Step 3: Verify the project builds**

Run:
```bash
cargo build
```

Expected: first build takes a while because whisper.cpp compiles from source (several minutes on first compile — grab coffee). Then `lindiction skeleton — replaced in Task 9` should run if you execute `cargo run`.

**If the build fails on whisper-rs:** stop. Inspect the error. Likely causes are missing `cmake`, `libclang-dev`, or an incompatible C++ toolchain version. Fix the system, do not substitute the crate.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/
git commit -m "chore: cargo skeleton with empty module stubs"
```

---

## Task 3: `config.rs` — Config struct (TDD)

**Files:**
- Create/modify: `src/config.rs`

- [ ] **Step 1: Write failing unit tests**

Overwrite `src/config.rs` with:
```rust
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub model_path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub xdotool_delay_ms: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_path() {
        // Ensure env var is unset for this test
        std::env::remove_var("LINDICTION_MODEL");
        let config = Config::load();
        assert_eq!(config.model_path, PathBuf::from("models/ggml-tiny.en.bin"));
    }

    #[test]
    fn test_env_override() {
        std::env::set_var("LINDICTION_MODEL", "/tmp/custom.bin");
        let config = Config::load();
        assert_eq!(config.model_path, PathBuf::from("/tmp/custom.bin"));
        std::env::remove_var("LINDICTION_MODEL");
    }

    #[test]
    fn test_audio_defaults() {
        let config = Config::load();
        assert_eq!(config.sample_rate, 16_000);
        assert_eq!(config.channels, 1);
    }

    #[test]
    fn test_xdotool_delay_default() {
        let config = Config::load();
        assert_eq!(config.xdotool_delay_ms, 5);
    }
}
```

- [ ] **Step 2: Verify tests fail**

Run:
```bash
cargo test --lib config::
```

Expected: compilation fails with "no function or associated item named `load`".

- [ ] **Step 3: Implement `Config::load`**

Add this impl block below the `Config` struct definition (above the `#[cfg(test)] mod tests` block):
```rust
impl Config {
    pub fn load() -> Self {
        let model_path = std::env::var("LINDICTION_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("models/ggml-tiny.en.bin"));
        Self {
            model_path,
            sample_rate: 16_000,
            channels: 1,
            xdotool_delay_ms: 5,
        }
    }

    pub fn with_model_path(mut self, path: PathBuf) -> Self {
        self.model_path = path;
        self
    }
}
```

The `with_model_path` builder method exists because the CLI layer in Task 9 will override the model path from `--model`, not only from env.

- [ ] **Step 4: Verify tests pass**

Run:
```bash
cargo test --lib config::
```

Expected: 4 tests pass.

**Note:** `cargo test` runs tests in parallel by default. `test_default_model_path` and `test_env_override` both touch the `LINDICTION_MODEL` env var, which is a process-global. If they run concurrently on the same process they can race. If you see flakiness, run:
```bash
cargo test --lib config:: -- --test-threads=1
```

The race isn't in the code under test — it's in the tests' use of env vars. For MVP this is acceptable; a future v0.2 refactor would inject env access instead of reading globals.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add Config with env-var override and defaults"
```

---

## Task 4: `inject.rs` — xdotool injector (TDD)

**Files:**
- Create/modify: `src/inject.rs`

- [ ] **Step 1: Write failing unit tests**

Overwrite `src/inject.rs` with:
```rust
use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Injector {
    delay_ms: u32,
}

impl Injector {
    pub fn new(delay_ms: u32) -> Self {
        Self { delay_ms }
    }

    fn build_args(&self, text: &str) -> Vec<String> {
        vec![
            "type".to_string(),
            "--clearmodifiers".to_string(),
            "--delay".to_string(),
            self.delay_ms.to_string(),
            "--".to_string(),
            text.to_string(),
        ]
    }

    pub async fn inject(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let status = tokio::process::Command::new("xdotool")
            .args(self.build_args(text))
            .status()
            .await
            .context("failed to spawn xdotool")?;
        if !status.success() {
            anyhow::bail!("xdotool exited with {}", status);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_args_shape() {
        let inj = Injector::new(5);
        let args = inj.build_args("hello");
        assert_eq!(
            args,
            vec!["type", "--clearmodifiers", "--delay", "5", "--", "hello"]
        );
    }

    #[test]
    fn test_build_args_separator_precedes_text() {
        // If text starts with `-`, it must appear AFTER the `--` separator
        // so xdotool doesn't interpret it as a flag.
        let inj = Injector::new(5);
        let args = inj.build_args("-flag-looking-text");
        let sep = args.iter().position(|a| a == "--").unwrap();
        let text = args.iter().position(|a| a == "-flag-looking-text").unwrap();
        assert!(sep < text, "separator must come before text");
    }

    #[test]
    fn test_build_args_custom_delay() {
        let inj = Injector::new(12);
        let args = inj.build_args("x");
        assert!(args.contains(&"12".to_string()));
    }

    #[test]
    fn test_build_args_empty_text() {
        let inj = Injector::new(5);
        let args = inj.build_args("");
        // empty string is still the last arg; inject() short-circuits, but build_args does not
        assert_eq!(args.last(), Some(&"".to_string()));
    }

    #[tokio::test]
    async fn test_inject_empty_is_noop() {
        // Should return Ok without spawning xdotool (so test passes even if xdotool is absent
        // in a hypothetical container environment; day-0 prep guarantees it's present otherwise).
        let inj = Injector::new(5);
        assert!(inj.inject("").await.is_ok());
    }
}
```

- [ ] **Step 2: Verify tests pass**

Run:
```bash
cargo test --lib inject::
```

Expected: 5 tests pass (we wrote the impl inline above because inject's impl is small enough that splitting it across commits adds no value).

- [ ] **Step 3: Smoke test — real xdotool call**

Create `examples/smoke_inject.rs`:
```rust
use lindiction::inject::Injector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let inj = Injector::new(5);
    inj.inject("hello from lindiction smoke_inject").await?;
    Ok(())
}
```

Focus a text editor or terminal. In another terminal, run:
```bash
cargo run --example smoke_inject
```

Expected: within a few seconds, `hello from lindiction smoke_inject` appears in the focused window.

- [ ] **Step 4: Commit**

```bash
git add src/inject.rs examples/smoke_inject.rs
git commit -m "feat(inject): Injector shelling out to xdotool type"
```

---

## Task 5: `audio.rs` — cpal wrapper

**Files:**
- Create/modify: `src/audio.rs`
- Create: `examples/smoke_audio.rs`

Unit-testing `audio.rs` is not practical — it needs a real microphone and cpal backend. We rely on the smoke binary and the eventual end-to-end test in Task 10.

- [ ] **Step 1: Implement `AudioCapture`**

Overwrite `src/audio.rs`:
```rust
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// Opens the default input device at 16 kHz mono f32 and produces
/// frames on an unbounded mpsc channel. The returned `AudioStream`
/// owns the cpal `Stream`; dropping it stops capture.
pub struct AudioStream {
    _stream: cpal::Stream,
}

pub fn start_capture(
    sample_rate: u32,
    channels: u16,
) -> Result<(AudioStream, mpsc::UnboundedReceiver<Vec<f32>>)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default audio input device — check `pactl list sources short`"))?;

    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    info!(device = %device_name, "opening input device");

    let stream_config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let (tx, rx) = mpsc::unbounded_channel::<Vec<f32>>();

    let stream = device
        .build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Drop the data if the receiver has been closed. Do not
                // block inside the audio callback.
                if tx.send(data.to_vec()).is_err() {
                    debug!("audio receiver dropped; stopping send");
                }
            },
            |err| error!(%err, "cpal stream error"),
            None,
        )
        .context("failed to build cpal input stream")?;

    stream.play().context("failed to start cpal stream")?;

    Ok((AudioStream { _stream: stream }, rx))
}
```

- [ ] **Step 2: Write the smoke example**

Create `examples/smoke_audio.rs`:
```rust
use lindiction::audio::start_capture;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let (_stream, mut rx) = start_capture(16_000, 1)?;
    let start = Instant::now();
    let mut chunks = 0usize;
    let mut samples = 0usize;

    while start.elapsed() < Duration::from_secs(5) {
        if let Some(chunk) = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .ok()
            .flatten()
        {
            chunks += 1;
            samples += chunk.len();
            let rms = (chunk.iter().map(|x| x * x).sum::<f32>() / chunk.len() as f32).sqrt();
            println!("chunk: {:4} samples  rms: {:.4}", chunk.len(), rms);
        }
    }

    println!(
        "captured {} chunks / {} samples in 5s (~{} Hz effective)",
        chunks,
        samples,
        samples / 5
    );
    Ok(())
}
```

- [ ] **Step 3: Run the smoke example and confirm audio flows**

Run:
```bash
cargo run --example smoke_audio
```

Speak at the microphone during the 5-second window.

Expected:
- `rms` values near 0.00 during silence.
- `rms` values > 0.01 while speaking.
- Effective sample rate close to 16 000 Hz (e.g., printed line shows ~16000).

**If no chunks arrive:** cpal is not opening the input. Check `pactl list sources short` — if no sources appear, PipeWire isn't running. If `pactl` looks fine but cpal still fails, check the cpal version in `Cargo.toml` against the latest on crates.io and bump.

- [ ] **Step 4: Commit**

```bash
git add src/audio.rs examples/smoke_audio.rs
git commit -m "feat(audio): cpal input stream wrapper producing f32 chunks"
```

---

## Task 6: `hotkey.rs` — global-hotkey wrapper

**Files:**
- Create/modify: `src/hotkey.rs`
- Create: `examples/smoke_hotkey.rs`

This task is the plan's highest-risk point. If Step 3 (smoke test) fails to emit both Press and Release, **stop** and bring the failure to the user; do not improvise.

- [ ] **Step 1: Implement `HotkeyListener`**

Overwrite `src/hotkey.rs`:
```rust
use anyhow::{Context, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use tokio::sync::mpsc;
use tracing::{debug, info};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Press,
    Release,
}

/// Registers Ctrl+Alt+Space as a global hotkey. The returned
/// receiver yields `Press` and `Release` events. The `GlobalHotKeyManager`
/// is held in a background std::thread that polls the crate's crossbeam
/// channel and forwards to our tokio channel.
pub struct HotkeyListener {
    _manager: GlobalHotKeyManager,
}

pub fn start() -> Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)> {
    let manager = GlobalHotKeyManager::new().context("creating GlobalHotKeyManager")?;
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
    let hotkey_id = hotkey.id();
    manager
        .register(hotkey)
        .context("Hotkey registration failed. Is another app bound to Ctrl+Alt+Space?")?;

    info!(hotkey_id, "registered Ctrl+Alt+Space");

    let (tx, rx) = mpsc::channel::<HotkeyEvent>(32);
    let crate_rx = GlobalHotKeyEvent::receiver();

    std::thread::Builder::new()
        .name("lindiction-hotkey".into())
        .spawn(move || {
            loop {
                match crate_rx.recv() {
                    Ok(event) => {
                        if event.id != hotkey_id {
                            continue;
                        }
                        let mapped = match event.state {
                            HotKeyState::Pressed => HotkeyEvent::Press,
                            HotKeyState::Released => HotkeyEvent::Release,
                        };
                        debug!(?mapped, "hotkey event");
                        if tx.blocking_send(mapped).is_err() {
                            break;
                        }
                    }
                    Err(_) => break, // channel closed
                }
            }
        })
        .context("spawning hotkey thread")?;

    Ok((HotkeyListener { _manager: manager }, rx))
}
```

- [ ] **Step 2: Write the smoke example**

Create `examples/smoke_hotkey.rs`:
```rust
use lindiction::hotkey::{start, HotkeyEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let (_lst, mut rx) = start()?;
    println!("Hold Ctrl+Alt+Space. Press Ctrl+C to exit.");

    loop {
        tokio::select! {
            Some(evt) = rx.recv() => match evt {
                HotkeyEvent::Press => println!("PRESS"),
                HotkeyEvent::Release => println!("RELEASE"),
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Run the smoke example and verify both events fire**

Run:
```bash
cargo run --example smoke_hotkey
```

Press Ctrl+Alt+Space briefly; then hold it for 2 seconds and release.

Expected output:
```
PRESS
RELEASE
PRESS
RELEASE
```

Each physical press produces one `PRESS` event and each release produces one `RELEASE` event. Holding does not produce repeat PRESS events (the crate de-dupes via the OS repeat suppression, or if it emits auto-repeats, that's acceptable — the app layer in Task 8 ignores duplicate presses).

**Failure mode — no RELEASE event:** `global-hotkey` on this platform only emits Pressed. Stop. Tell the user. Do not proceed. Next-step options: upgrade `global-hotkey` to a version that supports release on X11, swap to `livesplit-hotkey`, or use raw `x11` bindings. All three require revising this plan.

**Failure mode — manager requires main thread:** if `GlobalHotKeyManager::new()` panics or errors with a threading complaint, move `start()` to the main thread. For MVP, `main.rs` can call `hotkey::start()` first and then hand the receiver to `app::run`.

- [ ] **Step 4: Commit**

```bash
git add src/hotkey.rs examples/smoke_hotkey.rs
git commit -m "feat(hotkey): global-hotkey wrapper emitting Press/Release"
```

---

## Task 7: `stt.rs` — whisper-rs wrapper + integration test

**Files:**
- Create/modify: `src/stt.rs`
- Create: `tests/integration_stt.rs`

- [ ] **Step 1: Implement `SttEngine`**

Overwrite `src/stt.rs`:
```rust
use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct SttEngine {
    ctx: WhisperContext,
}

impl SttEngine {
    pub fn load(model_path: &Path) -> Result<Self> {
        if !model_path.exists() {
            anyhow::bail!(
                "Model not found: {}. Download with:\n  \
                 curl -L -o models/ggml-tiny.en.bin \
                 https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
                model_path.display()
            );
        }
        info!(path = %model_path.display(), "loading whisper model");
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().context("model path is not valid UTF-8")?,
            WhisperContextParameters::default(),
        )
        .with_context(|| {
            format!(
                "Failed to load model at {}. File may be corrupt; re-download.",
                model_path.display()
            )
        })?;
        Ok(Self { ctx })
    }

    /// Transcribe a 16 kHz mono f32 buffer. Blocking; call from `spawn_blocking`.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String> {
        if audio.is_empty() {
            return Ok(String::new());
        }
        let mut state = self
            .ctx
            .create_state()
            .context("creating whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, audio)
            .context("whisper inference failed")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut out = String::new();
        for i in 0..n {
            out.push_str(
                &state
                    .full_get_segment_text(i)
                    .context("segment text")?,
            );
        }
        Ok(out.trim().to_string())
    }
}
```

- [ ] **Step 2: Write the integration test**

Create `tests/integration_stt.rs`:
```rust
use lindiction::stt::SttEngine;
use std::path::PathBuf;

fn load_wav(path: &str) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("open fixture");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "fixture must be 16 kHz");
    assert_eq!(spec.channels, 1, "fixture must be mono");
    match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.expect("sample") as f32 / i16::MAX as f32)
            .collect(),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("sample"))
            .collect(),
    }
}

#[test]
fn transcribes_hello_world_fixture() {
    // Gate on env var so `cargo test` works in environments without a model file.
    let model = match std::env::var("LINDICTION_MODEL") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("skipping: set LINDICTION_MODEL to run this test");
            return;
        }
    };
    if !model.exists() {
        eprintln!("skipping: {} does not exist", model.display());
        return;
    }

    let engine = SttEngine::load(&model).expect("load model");
    let audio = load_wav("tests/fixtures/hello.wav");
    let text = engine.transcribe(&audio).expect("transcribe");
    let lc = text.to_lowercase();
    assert!(
        lc.contains("hello"),
        "expected transcript to contain 'hello', got: {text:?}"
    );
}
```

- [ ] **Step 3: Run the test with the model present**

Run:
```bash
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

Expected: test passes; transcript contains `hello`. On first run, whisper prints its own stderr noise (loading messages) — that's fine.

**If the test fails** with an output that doesn't contain "hello":
- Re-record `tests/fixtures/hello.wav` more clearly.
- Try `ggml-base.en.bin` instead of tiny (tiny sometimes mis-hears short clips). Change the model downloaded in Task 1 Step 3 and retry.

- [ ] **Step 4: Run the test without the env var to verify skip**

Run:
```bash
cargo test --test integration_stt -- --nocapture
```

Expected: test passes trivially with `skipping: set LINDICTION_MODEL to run this test` on stderr.

- [ ] **Step 5: Commit**

```bash
git add src/stt.rs tests/integration_stt.rs Cargo.toml Cargo.lock
git commit -m "feat(stt): whisper-rs wrapper and gated integration test"
```

---

## Task 8: `app.rs` — select loop + transcription worker

**Files:**
- Create/modify: `src/app.rs`

- [ ] **Step 1: Add the `which` crate for xdotool preflight**

Add to `Cargo.toml` under `[dependencies]`:
```toml
which = "6"
```

- [ ] **Step 2: Implement `App`**

Overwrite `src/app.rs`:
```rust
use crate::audio::{start_capture, AudioStream};
use crate::config::Config;
use crate::hotkey::{start as start_hotkey, HotkeyEvent, HotkeyListener};
use crate::inject::Injector;
use crate::stt::SttEngine;
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub struct App;

impl App {
    pub async fn run(config: Config) -> Result<()> {
        // Preflight: verify xdotool is present before we accept any audio.
        if which::which("xdotool").is_err() {
            anyhow::bail!("xdotool not found on PATH. Install: sudo apt install xdotool");
        }

        let injector = Injector::new(config.xdotool_delay_ms);

        // Load the model upfront — fail fast on a bad model path or corrupt file.
        let stt = Arc::new(
            SttEngine::load(&config.model_path)
                .with_context(|| format!("loading model from {}", config.model_path.display()))?,
        );

        // Transcription worker task: reads audio buffers from an mpsc channel
        // and transcribes serially (via spawn_blocking) before injecting.
        // Serial processing guarantees utterances are injected in press-order.
        let (transcribe_tx, mut transcribe_rx) = mpsc::channel::<Vec<f32>>(4);
        let worker = {
            let injector_worker = injector.clone();
            let stt_worker = Arc::clone(&stt);
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
                    info!(text = %text, "injecting");
                    if let Err(e) = injector_worker.inject(&text).await {
                        error!(error = %e, "injection failed");
                    }
                }
            })
        };

        // Hotkey stream
        let (_hotkey_listener, mut hotkey_rx): (HotkeyListener, _) = start_hotkey()?;

        // Audio stream
        let (_audio_stream, mut audio_rx): (AudioStream, _) =
            start_capture(config.sample_rate, config.channels)?;

        info!("ready — hold Ctrl+Alt+Space to dictate");

        let mut recording = false;
        let mut buffer: Vec<f32> = Vec::with_capacity(16_000 * 30);

        loop {
            tokio::select! {
                maybe_evt = hotkey_rx.recv() => match maybe_evt {
                    Some(HotkeyEvent::Press) => {
                        if recording {
                            debug!("duplicate press ignored");
                        } else {
                            recording = true;
                            buffer.clear();
                            info!("recording started");
                        }
                    }
                    Some(HotkeyEvent::Release) => {
                        if !recording {
                            debug!("release without prior press ignored");
                        } else {
                            recording = false;
                            let audio = std::mem::take(&mut buffer);
                            let seconds = audio.len() as f32 / 16_000.0;
                            info!(seconds, "recording stopped");
                            match transcribe_tx.try_send(audio) {
                                Ok(()) => {},
                                Err(mpsc::error::TrySendError::Full(dropped)) => {
                                    let s = dropped.len() as f32 / 16_000.0;
                                    warn!(seconds = s, "transcribe queue full, dropping utterance");
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
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c received; shutting down");
                    break;
                }
            }
        }

        // Drop the transcribe sender so the worker exits.
        drop(transcribe_tx);
        let _ = worker.await;
        Ok(())
    }
}
```

**Ownership note:** `stt` is wrapped in an `Arc<SttEngine>` so the transcription worker task can clone a handle for each `spawn_blocking` call (which needs `'static + Send`). `whisper-rs`'s `WhisperContext` is `Send + Sync` as of 0.11 — the `Arc` alone is enough, no `Mutex` needed. If `cargo build` surfaces a `!Sync` error on `SttEngine` (indicating the crate version doesn't promise `Sync`), wrap as `Arc<Mutex<SttEngine>>` and lock inside the `spawn_blocking` closure — no other code change required.

- [ ] **Step 3: Build — no tests yet, just confirm compilation**

Run:
```bash
cargo build
```

Expected: compiles cleanly. Warnings about unused `HotkeyListener` / `AudioStream` are OK — they own drop-guards whose purpose is to be held alive.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs Cargo.toml Cargo.lock
git commit -m "feat(app): select loop + transcription worker wiring"
```

---

## Task 9: `main.rs` — CLI entry point

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Implement CLI**

Overwrite `src/main.rs`:
```rust
use anyhow::Result;
use clap::Parser;
use lindiction::app::App;
use lindiction::config::Config;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Lindiction — push-to-talk voice dictation for Linux.
///
/// Hold Ctrl+Alt+Space to record. Release to transcribe and inject
/// the text at the cursor.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to GGML whisper model file
    #[arg(long, env = "LINDICTION_MODEL")]
    model: Option<PathBuf>,

    /// Verbose logging. -v = debug, -vv = trace
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("lindiction={level},warn")));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut config = Config::load();
    if let Some(m) = cli.model {
        config = config.with_model_path(m);
    }

    App::run(config).await
}
```

- [ ] **Step 2: Build and verify `--help`**

Run:
```bash
cargo build
./target/debug/lindiction --help
```

Expected output includes:
```
Lindiction — push-to-talk voice dictation for Linux.

Hold Ctrl+Alt+Space to record. Release to transcribe and inject
the text at the cursor.

Usage: lindiction [OPTIONS]

Options:
      --model <MODEL>  Path to GGML whisper model file [env: LINDICTION_MODEL=]
  -v, --verbose...     Verbose logging. -v = debug, -vv = trace
  -h, --help           Print help
  -V, --version        Print version
```

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): clap CLI + tracing init wiring App::run"
```

---

## Task 10: End-to-end verification

**Files:** none modified. This task is manual verification. If any check fails, fix the underlying module and re-run.

- [ ] **Step 1: Start the daemon**

Run in a terminal:
```bash
cargo run -- -v
```

Expected startup lines on stderr (exact format depends on tracing):
```
lindiction::audio: opening input device device=<your mic>
lindiction::hotkey: registered Ctrl+Alt+Space
lindiction::app: ready — hold Ctrl+Alt+Space to dictate
```

No panics, no "xdotool not found", no "model not found", no "no default audio input device".

- [ ] **Step 2: Manual test 1 — basic dictation**

Leave the daemon running. Focus a text editor or a browser's URL bar. Hold Ctrl+Alt+Space, say "hello world", release.

Expected: within ~1 second, `hello world` (or similar; punctuation optional) appears in the focused field. Daemon logs show `recording started`, `recording stopped seconds=~1.5`, `injecting text="hello world."`.

- [ ] **Step 3: Manual test 2 — longer utterance**

Hold Ctrl+Alt+Space, say "this is a test of the dictation system", release.

Expected: the sentence appears in the focused field. Latency is perceptibly higher than the 1-word case (whisper inference scales with audio length) — this is normal.

- [ ] **Step 4: Manual test 3 — back-to-back**

Press Ctrl+Alt+Space, say "first", release. Immediately press again, say "second", release, before the first transcription has finished.

Expected: `first` appears, then `second`. Never `second` before `first`. (This confirms the serial-worker design holds.)

- [ ] **Step 5: Manual test 4 — empty press**

Tap Ctrl+Alt+Space with no speech (very short press, silence).

Expected: no text injected. Logs show `empty transcription, nothing to inject` at debug level (so visible only with `-v`).

- [ ] **Step 6: Manual test 5 — ctrl-c shutdown**

Press Ctrl+C in the daemon terminal.

Expected: logs show `ctrl-c received; shutting down`, process exits cleanly with code 0.

- [ ] **Step 7: Manual test 6 — missing model**

Run:
```bash
cargo run -- --model /tmp/does-not-exist.bin
```

Expected: clean startup error on stderr mentioning `Model not found: /tmp/does-not-exist.bin` and a curl command. Exit code non-zero.

- [ ] **Step 8: Commit (no code changes — checkpoint)**

If any of Steps 2–7 required code fixes, they were committed at the point of the fix. If nothing was fixed, no commit is needed here; annotate the CHECKPOINT in your plan-execution notes instead.

---

## Task 11: README

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write the README**

Create `README.md`:
```markdown
# Lindiction

Push-to-talk voice dictation for Linux. Hold a hotkey, speak, release — transcribed text appears at the cursor.

MVP scope: Ubuntu 24.04 / X11 / GNOME. Wayland, system tray, systemd service, `.deb` packaging, and VAD endpointing are v0.2+.

## Requirements

- Ubuntu 24.04 LTS, X11 session (`echo $XDG_SESSION_TYPE` returns `x11`)
- PipeWire with PulseAudio compatibility (default on modern Ubuntu)
- Rust toolchain (install via [rustup](https://rustup.rs/))
- A working microphone

## Install

### System packages

```bash
sudo apt update
sudo apt install -y \
    xdotool build-essential cmake pkg-config \
    libclang-dev libasound2-dev libpulse-dev curl
```

### Whisper model

Download the `tiny.en` model (~75 MB, fast, English-only):

```bash
mkdir -p models
curl -L -o models/ggml-tiny.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
```

For better accuracy at higher latency, download `ggml-base.en.bin` (~142 MB) and pass it via `--model`.

### Build

```bash
cargo build --release
```

First build takes several minutes (compiles whisper.cpp from source).

## Run

```bash
./target/release/lindiction
```

Hold **Ctrl+Alt+Space**, speak, release. Transcribed text appears at the cursor in whichever app is focused.

Press Ctrl+C in the daemon terminal to exit.

### Flags

| Flag | Purpose |
|---|---|
| `--model <PATH>` | Override model path. Also configurable via env var `LINDICTION_MODEL`. |
| `-v` / `-vv` | Debug / trace logging. |
| `--help` | Print help. |
| `--version` | Print version. |

## Troubleshooting

**"Model not found"** — download the model with the curl command above. The default expected path is `./models/ggml-tiny.en.bin` relative to the current working directory.

**"xdotool not found"** — `sudo apt install xdotool`.

**"No audio input device"** — run `pactl list sources short`. If empty, your PipeWire/PulseAudio is not running. Log out and back in, or `systemctl --user restart pipewire`.

**"Hotkey registration failed"** — another application is bound to Ctrl+Alt+Space. Close it, or edit `src/hotkey.rs` to choose a different binding (v0.2 will make this configurable at runtime).

**Text appears in the wrong window** — `xdotool type` types into the currently-focused window. Focus the target window before releasing the hotkey.

**Transcriptions are gibberish** — the `tiny.en` model is fast but imprecise. Switch to `ggml-base.en.bin` via `--model`.

## Testing

```bash
# Unit tests
cargo test --lib

# Integration test (requires a downloaded model)
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

## License

MIT.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: add README with install, run, and troubleshooting"
```

---

## Task 12: Acceptance checkpoint

**Files:** none modified.

- [ ] **Step 1: Run the full test suite**

Run:
```bash
cargo test --lib
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

Expected: both green.

- [ ] **Step 2: Re-run manual tests 1–6 from Task 10**

Each should pass. If any has regressed, fix and commit before proceeding.

- [ ] **Step 3: Build release binary**

Run:
```bash
cargo build --release
./target/release/lindiction --version
```

Expected: prints `lindiction 0.1.0`.

- [ ] **Step 4: Verify the repo is clean**

Run:
```bash
git status
```

Expected: `nothing to commit, working tree clean`.

- [ ] **Step 5: Tag the MVP**

Run:
```bash
git tag -a v0.1.0-mvp -m "MVP: hotkey + cpal + whisper + xdotool end-to-end"
git log --oneline
```

Expected: tag is created on the latest commit; log shows the sequence of Task 1 through Task 11 commits.

**Plan complete.** Next roadmap item is v0.2, which starts with TOML config and postprocessing per `docs/superpowers/specs/2026-04-18-lindiction-mvp-design.md` section "Future".
