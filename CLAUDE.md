# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

**Pre-implementation.** As of 2026-04-18 the repo contains only `IMPLEMENTATION_PLAN.md` — no Cargo workspace, no source, no tests, no git history. The plan describes a 4-week build of `lindiction`, a system-wide push-to-talk voice dictation daemon for Linux (Ubuntu, Wayland+X11) written in Rust. Treat the plan as the spec; if the user asks you to start coding, bootstrap the workspace layout it describes.

## Planned architecture (from IMPLEMENTATION_PLAN.md)

Three-crate Cargo workspace:

- `crates/lindictd/` — long-running daemon binary. `main.rs` is intentionally thin; it wires up components from core and runs the tokio `select!` loop.
- `crates/lindiction-cli/` — CLI client binary (installed as `lindiction`). Talks to the daemon over a Unix socket at `/run/user/{uid}/lindiction/lindictd.sock` using JSON.
- `crates/lindiction-core/` — shared library where all logic lives. Every module below is in this crate.

The daemon is one async event loop. `tokio::select!` multiplexes three sources: hotkey events (evdev thread → mpsc), audio frames (cpal callback → mpsc), and shutdown signals. This is the central control flow — changes to it ripple everywhere.

### Data pipeline (single `Vec<f32>` format end to end)

```
mic → cpal (16kHz mono f32, 1024-sample chunks)
    → earshot VAD (256-sample / 16ms frames, score ∈ [0,1])
    → VadStateMachine (Idle → Speaking → Hangover → Idle)
    → on Complete: whisper-rs transcribe
    → PostProcessor (filler removal, capitalize, trim)
    → InjectorChain (wtype → ydotool → clipboard+paste)
```

No format conversion anywhere — cpal, earshot, and whisper-rs all speak 16kHz mono `f32`. Preserve this invariant; if you introduce resampling or channel conversion, you've likely taken a wrong turn.

### VAD state machine is load-bearing

`vad_state.rs` holds the `Idle → Speaking → Hangover → Idle` state machine with three frame-count thresholds: `min_speech_frames` (~16), `min_silence_frames` (~19), `hangover_frames` (~31). Hangover exists so brief mid-sentence pauses don't cut the utterance. The state machine owns the `speech_buffer: Vec<f32>` that accumulates frames and is drained on `VadOutput::Complete`. When editing, preserve the invariant that `Complete` is emitted exactly once per utterance and resets `speech_buffer`.

### Injection is a fallback chain, not a single call

`InjectorChain::auto()` inspects `WAYLAND_DISPLAY` / `XDG_SESSION_TYPE` and builds an ordered `Vec<Box<dyn TextInjector>>`. On Wayland: `wtype → ydotool → clipboard`. On X11: `ydotool → clipboard`. `inject()` tries each in order and returns on first success. New injection methods should plug in as `TextInjector` impls rather than being special-cased. The plan intentionally shells out via `std::process::Command` instead of using a crate — don't replace this with a library dependency.

### Why specific crate choices (don't casually swap)

- `earshot` (VAD) — chosen over Silero specifically to avoid an ONNX Runtime dependency (~8 MB). Swapping to Silero is a ~8 MB binary footprint regression.
- `evdev-shortcut` (hotkey) — chosen because it's display-server agnostic (reads `/dev/input/*` directly). Requires the user to be in the `input` group; the daemon must surface this clearly on `EACCES`.
- `whisper-rs` (STT) — most mature whisper.cpp bindings. The plan notes `whisper-cpp-plus` has streaming PCM but pins to an older fork; streaming is v0.2+ work.

## Commands (once the workspace exists)

```bash
# Build both binaries
cargo build --release
# → target/release/lindictd (daemon)
# → target/release/lindiction (CLI client)

# Full test suite across workspace
cargo test --workspace

# Run a single test
cargo test --workspace test_filler_removal
cargo test -p lindiction-core --test integration_vad

# Download the Whisper base.en model (~142 MB) into ~/.local/share/lindiction/models/
./scripts/setup.sh

# Run the daemon in the foreground
RUST_LOG=lindiction=debug cargo run --release --bin lindictd

# .deb package (requires cargo-deb)
cargo deb -p lindictd
```

## Configuration and filesystem paths

- Config: `~/.config/lindiction/config.toml` (XDG). TOML schema defined in `config.rs`. Sections: `[hotkey] [audio] [stt] [vad] [injection] [postprocess] [logging]`. Defaults in the plan are authoritative.
- Models: `~/.local/share/lindiction/models/ggml-*.bin`. Downloaded on demand by `model.rs`.
- IPC socket: `/run/user/{uid}/lindiction/lindictd.sock`.
- Log: `~/.local/share/lindiction/lindiction.log`.

## Error handling convention

- `lindiction-core` defines typed errors in `error.rs` via `thiserror` (`LindictionError` enum, `Result<T>` alias). Use these for domain errors.
- Top-level / daemon code uses `anyhow` for contextual chaining.
- Error-recovery matrix (in the plan) decides fatal vs non-fatal: audio device disconnect → retry + notify; model missing → exit with download instructions; `EACCES` on evdev → print `usermod -aG input` instructions and exit; ydotool missing → fall through the injector chain silently.

## Permissions gotcha

Running the daemon requires the user to be in the `input` group (for evdev) and typically a running `ydotoold`. If you're debugging hotkey or injection failures, check these before anything else — they produce confusing permission errors that look like library bugs.

## Latency target

MVP budget is **<1.5s** end-to-end (hotkey release → text at cursor), v1.0 goal is <700ms. The dominant term is Whisper inference (200–800 ms on CPU with `base.en`). When profiling, measure this first.
