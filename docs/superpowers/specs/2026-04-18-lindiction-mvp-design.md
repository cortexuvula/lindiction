# Lindiction MVP — Design

**Date:** 2026-04-18
**Status:** Approved, pre-implementation
**Supersedes for v0.1:** the broader 4-week scope in `IMPLEMENTATION_PLAN.md` (which remains the reference for v0.2+)

## Overview

Lindiction MVP is a single-binary Rust push-to-talk dictation tool for Linux. Press and hold a hotkey, speak, release — transcribed text appears at the cursor. Target environment for v0.1 is the developer's machine: **Ubuntu 24.04, X11, GNOME, PipeWire (via PulseAudio compatibility layer)**.

The MVP exists to prove the three highest-uncertainty components (audio capture via cpal, whisper.cpp via whisper-rs, global hotkey on X11) work end-to-end on the target machine. Everything the broader `IMPLEMENTATION_PLAN.md` adds on top of that core — tray, packaging, systemd, IPC/CLI split, VAD endpointing, fallback injector chain, Wayland support, model auto-download, TOML config — is explicitly deferred to v0.2+.

## Scope

### In scope (v0.1 MVP)

- Single-binary `lindiction` (no daemon/CLI split).
- Global hotkey **Ctrl+Alt+Space**, push-to-hold semantics only.
- Audio capture at 16 kHz mono f32 via cpal.
- Transcription via whisper-rs with `ggml-tiny.en.bin` as the default model.
- Text injection via `xdotool type` (shell-out).
- CLI flags for overrides: `--model`, `--verbose`. (Hotkey is hardcoded in MVP; no flag.)
- Tracing-based logging to stderr.
- One integration test (transcribe a fixture WAV).
- README covering install, run, and troubleshooting.

### Explicit non-goals (deferred to v0.2+)

- System tray icon, `.deb` packaging, systemd user service.
- IPC Unix socket + separate CLI client.
- VAD endpointing (state machine, hangover).
- Post-processing (filler removal, capitalization, trim).
- Fallback injection chain — MVP ships xdotool only.
- Wayland support (no `wtype`, no `ydotool`).
- Toggle mode — MVP is PTT only.
- Automatic model download.
- TOML config file.

### Constraints inherited from the environment

- PipeWire via PulseAudio compat — cpal's PulseAudio backend is known working against this shim.
- X11 session — can use `XGrabKey`-based global hotkey without needing the user in the `input` group or running `ydotoold`.
- `xdotool` is a standard Ubuntu package (`sudo apt install xdotool`).

## Architecture

### High-level

Single async Rust process. One `tokio::select!` main loop in `app.rs` multiplexes three event sources:

1. **Hotkey events** from a background thread (driven by the chosen hotkey crate's event model).
2. **Audio frames** from the cpal callback thread.
3. **Shutdown signal** (`Ctrl+C`).

Whisper inference is blocking CPU work (100–800 ms depending on model and utterance length). It runs on a `tokio::task::spawn_blocking` worker so the select loop never stalls. The completed transcription is then injected via `xdotool` (a short-running async `tokio::process::Command`).

### Data flow

```
  Microphone                                                          
      │                                                               
      ▼                                                               
  cpal::Stream ──[Vec<f32>, mono 16 kHz]──► mpsc audio_tx             
                                                                      
  X11 XGrabKey thread ──[Press/Release]──► mpsc hotkey_tx             
                                                                      
  ┌────────────────────────────────────┐                              
  │  App::run  (tokio::select!)        │                              
  │                                    │                              
  │  Press   → recording = true        │                              
  │            buffer.clear()          │                              
  │  frame   → buffer.extend(&chunk)   │                              
  │  Release → transcribe_tx           │                              
  │            .try_send(mem::take(&mut buffer))                      
  └──────────────┬─────────────────────┘                              
                 │ Vec<f32>                                           
                 ▼                                                    
  ┌────────────────────────────────────┐                              
  │  Transcription worker (tokio task) │                              
  │  owns SttEngine                    │                              
  │                                    │                              
  │  loop {                            │                              
  │    audio = rx.recv().await;        │                              
  │    text  = spawn_blocking(||       │                              
  │              stt.transcribe(audio)).await;                        
  │    injector.inject(&text).await;   │                              
  │  }                                 │                              
  └────────────────────────────────────┘                              
```

Deliberate design choices:

- **`std::mem::take(&mut buffer)` on release.** Zero-copy handoff of the audio `Vec<f32>` to the transcription worker; buffer is reset to empty and ready for the next press.
- **Single dedicated transcription worker task** fed by a bounded mpsc channel (capacity 4). Every release `try_send`s the audio buffer to the worker, which transcribes and injects *serially*. Guarantees utterances appear at the cursor in press-order — back-to-back presses never race. If the channel is full (4 backlog, which would require pressing faster than whisper can transcribe for several seconds), the latest utterance is dropped with a `warn!` log. The select loop itself is never blocked by whisper.
- **Whisper context owned exclusively by the transcription worker.** No `Mutex` needed — only one task ever touches it. Model load cost (~hundreds of ms) is paid once at startup, not per utterance. `SttEngine::transcribe` itself is called inside `spawn_blocking` so whisper's CPU-bound work doesn't block the worker's async executor slot.

### Module responsibilities

| Module | Role | Key types |
|---|---|---|
| `main.rs` | clap arg parsing, `tracing_subscriber` init, constructs `App`, runs `App::run()`. | `Cli` (clap derive) |
| `config.rs` | Hardcoded defaults. Env-var overrides for MVP (`LINDICTION_MODEL`). Resolved once at startup. | `Config` |
| `audio.rs` | Opens default cpal input at 16 kHz mono f32. Owns the `cpal::Stream`. Produces `Vec<f32>` chunks on an `mpsc::UnboundedSender`. | `AudioCapture`, `AudioStream` |
| `hotkey.rs` | Registers the global hotkey on X11. Emits `HotkeyEvent::Press`/`Release` on an `mpsc::Sender`. | `HotkeyListener`, `HotkeyEvent` |
| `stt.rs` | Loads GGML model once. `transcribe(&[f32]) -> Result<String>`. Synchronous; no async awareness. | `SttEngine` |
| `inject.rs` | `inject(&str)` spawns `xdotool type --clearmodifiers --delay 5 -- "$text"` via `tokio::process::Command`. | `Injector` |
| `app.rs` | Owns the select loop and spawns the transcription worker. Holds audio_rx, hotkey_rx, `transcribe_tx`. | `App` |

Each module is independently testable: `audio` produces frames to a channel (mockable), `stt` takes a `&[f32]` and returns a `String` (fixture-driven), `inject` takes a `&str` (can inspect the built xdotool command). The only integration point is `app.rs`.

## Concurrency model

Three threads involved:

1. **cpal audio callback thread** — driven by the cpal backend. Runs the closure passed to `build_input_stream`. Must not block. All it does is `audio_tx.send(data.to_vec())` and return.
2. **Hotkey event thread** — implementation depends on the chosen crate (see "Hotkey crate spike" below). It calls `hotkey_tx.blocking_send(...)` to hand events to tokio.
3. **Tokio multi-thread runtime** — one worker per CPU core by default. The main task runs the select loop; `tokio::spawn`'d transcription tasks run whisper via `spawn_blocking`.

### Back-pressure and channel sizing

- `audio_tx`: **unbounded** mpsc. Each chunk is ~1024 samples = 8 KB. At 16 kHz with 1024-sample chunks, that's ~16 sends/sec — negligible volume. Unbounded keeps the cpal callback non-blocking.
- `hotkey_tx`: **bounded, cap 32**. Hotkey events are extremely low-volume; 32 is overkill. Bounded just to surface any bug that produces event floods.

## Error handling

### Convention

- Domain errors (audio device missing, model not found, injection failed) use `anyhow::Result<T>` with `.context(...)` for readable chains.
- `thiserror`-typed errors are not needed for MVP — single binary, no library boundary to expose a stable error enum across.

### Startup errors — fatal, exit with actionable message

| Failure | User sees | Exit code |
|---|---|---|
| No default input device | "No audio input device found. Check `pactl list sources short`." | 1 |
| Model path doesn't exist | "Model not found: {path}. Download with: `curl -L -o models/ggml-tiny.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin`" | 1 |
| Whisper model load fails | "Failed to load model at {path}: {err}. File may be corrupt; re-download." | 1 |
| xdotool not in PATH | "xdotool not found. Install: `sudo apt install xdotool`." (checked at startup, not first use) | 1 |
| Hotkey registration fails | "Hotkey registration failed: {err}. Is another app bound to Ctrl+Alt+Space?" | 1 |

### Runtime errors — log + continue

| Failure | Behavior |
|---|---|
| Transcription returns an error | `tracing::error!("transcription failed: {err}")`; drop utterance; no text injected. |
| xdotool exits non-zero | Log stderr; drop utterance. |
| Transcribe queue full on release (4 backlogged) | `warn!("transcribe queue full, dropping {}s utterance")`; drop. Requires pressing faster than whisper can transcribe for several presses in a row — unlikely in normal use. |
| cpal stream error callback fires | Log; for MVP, do not attempt reconnect — user must restart. (Reconnect is v0.2.) |
| Release received without prior Press | Ignore; no-op. |
| Press received while already recording (e.g. OS auto-repeat) | Ignore the duplicate press; keep existing buffer. |

## Dependencies

```toml
[package]
name = "lindiction"
version = "0.1.0"
edition = "2021"

[dependencies]
cpal = "0.15"                                  # verify at spike; PulseAudio backend required
whisper-rs = "0.11"                            # latest stable on crates.io at spike time
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "signal", "process"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
clap = { version = "4", features = ["derive"] }

# Hotkey crate: chosen on day 1 from the following candidates, in order of preference:
#   1. global-hotkey         — Tauri's crate, most mature
#   2. livesplit-hotkey      — simpler, minimal deps
#   3. x11 (direct bindings) — fallback if neither crate integrates cleanly with tokio
```

All versions above are starting pins — day-1 spike updates them to the latest compatible releases.

## CLI surface

```
lindiction [OPTIONS]

Push-to-talk voice dictation. Hold Ctrl+Alt+Space to record; release to transcribe.

Options:
  --model <PATH>          Path to GGML model file
                          [default: ./models/ggml-tiny.en.bin]
                          [env: LINDICTION_MODEL]
  -v, --verbose           Enable debug logging (-vv for trace)
  -h, --help              Print help
  -V, --version           Print version
```

Hotkey is hardcoded to Ctrl+Alt+Space in MVP. Making it configurable is a v0.2 task (requires a parser for key names, evdev/X11 keysym mapping table, and probably a TOML config to hold the choice).

## Testing strategy

### Unit tests

- `config::test_defaults` — verify default paths and the env-var override for `LINDICTION_MODEL`.
- `inject::test_xdotool_args` — verify the argv built for `xdotool type` escapes correctly (in particular, verify `--` separator is always present so text starting with `-` is not interpreted as a flag).

### Integration tests

- `tests/integration_stt.rs` — loads `tests/fixtures/hello.wav` (a ~2-second "hello world" clip recorded at 16 kHz mono), transcribes it, asserts output (lowercased, trimmed) contains `"hello"`. **Gated** on `LINDICTION_MODEL` env var pointing to a real model file; otherwise skipped with `println!` note. This keeps `cargo test` runnable without requiring the 75 MB model in CI or on first clone.

### Manual test plan (for each RC of the MVP)

1. Hold hotkey in a terminal, say "hello world", release → text appears.
2. Hold hotkey, say "one two three", release; immediately hold again and say "four five six" before (1)'s transcription has finished → both transcriptions appear in order.
3. Rapid press-release with no speech → graceful (empty or near-empty transcription; no panic).
4. Release hotkey without having pressed → no-op.
5. 30-second utterance → transcribes, takes perceptibly longer, no crash.
6. Uninstall xdotool and start daemon → fails at startup with the documented message.

## File layout

```
lindiction/
├── Cargo.toml
├── Cargo.lock
├── README.md                            # install + run + troubleshooting
├── models/
│   └── .gitkeep                         # .gitignore the .bin files
├── tests/
│   ├── integration_stt.rs
│   └── fixtures/
│       └── hello.wav                    # checked in; ~50 KB
├── docs/
│   └── superpowers/specs/
│       └── 2026-04-18-lindiction-mvp-design.md  # this file
└── src/
    ├── main.rs
    ├── app.rs
    ├── config.rs
    ├── audio.rs
    ├── hotkey.rs
    ├── stt.rs
    └── inject.rs
```

## Day-by-day schedule (≤1 week)

### Day 1 — Spike + dep lock-in

Goal: prove all five risky components work on the target machine, independently, in the simplest possible form. No integration, no modules, no glue.

0. **Prep** (can be done in parallel with any other step):
   - `sudo apt install xdotool build-essential cmake pkg-config libclang-dev libasound2-dev`.
   - Download the model: `mkdir -p models && curl -L -o models/ggml-tiny.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin`.
   - Record the test fixture: `arecord -f S16_LE -r 16000 -c 1 -d 2 tests/fixtures/hello.wav` (say "hello world" during the 2-second window), or grab an existing 16 kHz mono "hello world" clip.
1. `cargo new lindiction`, `git init`, initial commit.
2. 30-line cpal smoke test: open default input at 16 kHz mono f32, compute RMS of incoming frames, print once per second for 5 seconds, exit. Verifies the cpal PulseAudio backend talks to PipeWire's compat layer on this machine.
3. Add `whisper-rs` as a dep. Write a smoke test that loads `models/ggml-tiny.en.bin` and transcribes `tests/fixtures/hello.wav`. Verifies the whisper.cpp C++ build toolchain is happy on Ubuntu 24.04 (first build can take minutes; do this before being blocked on it).
4. Add the chosen hotkey crate (start with `global-hotkey`). Register Ctrl+Alt+Space, print `"PRESS"`/`"RELEASE"` on events. Confirm press/release both fire (some crates only emit press — this is the make-or-break check for PTT). Fall back to `livesplit-hotkey` or direct `x11` bindings if needed.
5. Shell out to `xdotool type -- "hello"`. Confirm text appears in a focused terminal.

**Exit criterion:** all five spikes pass. Pin versions in a shared `Cargo.toml`.

### Day 2 — Wire it up

Write the six modules (`config.rs`, `audio.rs`, `hotkey.rs`, `stt.rs`, `inject.rs`, `app.rs`) and `main.rs`. Structure follows spec. First end-to-end dictation.

**Exit criterion:** hold hotkey, say "hello world", release → text types at cursor.

### Day 3 — Polish

- clap CLI flags, `--version`, `--help`.
- Error messages match the table in this spec.
- Tracing logs at `info` for lifecycle, `debug` for frame counts and timings.
- README with install, run, troubleshooting.

**Exit criterion:** a fresh user can clone, install deps, and dictate in ≤10 minutes following only the README.

### Day 4 — Integration test + edge cases

- `tests/integration_stt.rs`.
- Manual test plan items 1–6.
- Fix any bugs found.

**Exit criterion:** `cargo test` green; all manual tests pass.

### Day 5 — Slack / optional

- Swap to `ggml-base.en.bin`. Benchmark latency (press release → text typed) for 3-second, 10-second, and 30-second utterances.
- Document latency numbers in README.
- Decide which model to default to in v0.1 release.

## Spike targets (open questions for day 1)

These are the knowns unknowns the design assumes will resolve during day 1:

1. **Exact `cpal` version** — the version on crates.io that has a working PulseAudio backend on Ubuntu 24.04. Current guess: 0.15.x. Update pin after spike.
2. **Exact `whisper-rs` version** — pin to the latest stable on crates.io at spike time. Confirm the `whisper.cpp` submodule it vendors builds cleanly on gcc/clang present in Ubuntu 24.04.
3. **Hotkey crate** — whether `global-hotkey` cleanly coexists with a tokio runtime (some X11 crates insist on owning the main thread). Fallback order documented above.
4. **xdotool typing edge cases** — whether `xdotool type --` handles arbitrary Unicode punctuation correctly on this machine (smart quotes, em dashes, etc.), or if we need to pre-sanitize. Findings captured in a follow-up note, not the code, unless a fix is trivial.

## Future (v0.2+ — not part of this MVP)

The broader `IMPLEMENTATION_PLAN.md` is the v0.2+ roadmap. Order of likely work after MVP ships:

1. TOML config at `~/.config/lindiction/config.toml`.
2. Post-processing: trim, filler removal, capitalize.
3. Additional injection methods and fallback chain (to support Wayland).
4. VAD endpointing for toggle mode.
5. System tray indicator.
6. Model auto-download on first run.
7. IPC/CLI split.
8. `.deb` packaging + systemd user service.
