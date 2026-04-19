# Lindiction — Detailed Rust Implementation Plan

**Project codename:** `lindiction`
**Goal:** System-wide, push-to-talk voice dictation for Linux Ubuntu — a Wispr Flow clone
**Language:** Rust (single static binary, low latency, good system integration)
**Target:** ~200 hours / 4 weeks (1 developer, full-time)
**Date:** 2026-04-18

---

## Table of Contents

1. [Project Structure](#1-project-structure)
2. [Crate Dependencies](#2-crate-dependencies)
3. [Module Breakdown](#3-module-breakdown)
4. [Audio Pipeline Data Flow](#4-audio-pipeline-data-flow)
5. [Error Handling Strategy](#5-error-handling-strategy)
6. [Configuration Design (TOML Schema)](#6-configuration-design)
7. [Build and Packaging](#7-build-and-packaging)
8. [Testing Strategy](#8-testing-strategy)
9. [Day-by-Day 4-Week Schedule](#9-day-by-day-4-week-schedule)
10. [Key Code Snippets](#10-key-code-snippets)

---

## 1. Project Structure

### Cargo Workspace Layout

```
lindiction/
├── Cargo.toml                  # Workspace root
├── Cargo.lock
├── config.toml.example         # Example config for users
├── README.md
├── LICENSE                     # MIT
├── ARCHITECTURE.md             # High-level architecture (existing)
├── IMPLEMENTATION_PLAN.md      # This document
│
├── crates/
│   ├── lindictd/               # Main daemon binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── lindiction-cli/         # CLI control client binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   └── lindiction-core/        # Shared core library (all logic lives here)
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── config.rs
│           ├── error.rs
│           ├── audio.rs
│           ├── vad.rs
│           ├── vad_state.rs
│           ├── stt.rs
│           ├── postprocess.rs
│           ├── injector.rs
│           ├── hotkey.rs
│           ├── ipc.rs
│           ├── tray.rs
│           └── model.rs
│
├── scripts/
│   ├── setup.sh                # Install deps + download model
│   ├── install-service.sh      # Install systemd user service
│   └── build-deb.sh            # Build .deb package
│
├── systemd/
│   └── lindictd.service        # systemd user service unit
│
├── models/                     # Gitignored; downloaded at setup
│   └── .gitkeep
│
└── tests/
    ├── integration_audio.rs
    ├── integration_vad.rs
    ├── integration_stt.rs
    ├── integration_inject.rs
    └── fixtures/
        └── hello.wav           # Test audio fixture
```

### Why a Cargo Workspace?

- `lindiction-core` is a library crate shared by both binaries.
- `lindictd` is the long-running daemon (~50 lines of main, delegates to core).
- `lindiction-cli` is a thin CLI client that talks to the daemon over Unix socket.
- Workspace lets us share dependencies, run `cargo test --workspace`, and build both binaries in one `cargo build --release`.

---

## 2. Crate Dependencies

### lindiction-core/Cargo.toml

```toml
[package]
name = "lindiction-core"
version = "0.1.0"
edition = "2021"

[dependencies]
# ── Audio ──────────────────────────────────────────────
cpal = "0.17"                   # Cross-platform audio I/O (PipeWire backend on Linux)

# ── Voice Activity Detection ──────────────────────────
earshot = "1.1"                 # Pure Rust VAD, 20x faster than Silero, no ONNX dependency
                                # 256-sample frames @ 16kHz, returns score ∈ [0,1]

# ── Speech-to-Text ────────────────────────────────────
whisper-rs = "0.16"             # Rust bindings for whisper.cpp (builds from source)
                                # Alternative: whisper-cpp-plus = "0.1" (has streaming PCM support)

# ── Text Injection ────────────────────────────────────
# No crate needed — we shell out to ydotool/wl-copy via std::process::Command
# This is intentional: injection is best handled by battle-tested system tools

# ── Hotkey Detection ──────────────────────────────────
evdev-shortcut = "0.2"          # Global hotkeys via evdev (display-server agnostic)
                                # Requires user in `input` group
evdev = "0.12"                  # Low-level evdev for fallback/advanced key handling

# ── Async Runtime ─────────────────────────────────────
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util", "sync", "macros", "signal"] }

# ── Config ────────────────────────────────────────────
serde = { version = "1", features = ["derive"] }
toml = "0.8"

# ── Error Handling ────────────────────────────────────
anyhow = "1"
thiserror = "1"

# ── Logging ───────────────────────────────────────────
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

# ── CLI ───────────────────────────────────────────────
clap = { version = "4", features = ["derive"] }

# ── System Tray ───────────────────────────────────────
tray-icon = "0.19"              # System tray icon (GTK on Linux)
muda = "0.15"                   # Tray menu items
notify-rust = "4"               # Desktop notifications

# ── Misc ──────────────────────────────────────────────
dirs = "6"                      # XDG directory paths
chrono = "0.4"                  # Timestamps for logs
```

### lindictd/Cargo.toml

```toml
[package]
name = "lindictd"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "lindictd"
path = "src/main.rs"

[dependencies]
lindiction-core = { path = "../lindiction-core" }
tokio = { version = "1", features = ["rt-multi-thread", "signal"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
```

### lindiction-cli/Cargo.toml

```toml
[package]
name = "lindiction-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "lindiction"
path = "src/main.rs"

[dependencies]
lindiction-core = { path = "../lindiction-core" }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util"] }
anyhow = "1"
```

### Why These Specific Crates?

| Concern | Chosen Crate | Why Not Alternatives |
|---------|-------------|---------------------|
| **Audio** | `cpal 0.17` | PipeWire backend landed in cpal master. `pipewire-rs` is lower-level and requires more boilerplate. cpal gives us a clean stream API. |
| **VAD** | `earshot 1.1` | 20x faster than Silero VAD, pure Rust (no ONNX Runtime ~8MB dep), 75KB binary footprint, `#![no_std]` capable. Silero-vad-rs requires `ort` crate + ONNX runtime. |
| **STT** | `whisper-rs 0.16` | Most mature whisper.cpp bindings, widely used in production. `whisper-cpp-plus` has streaming PCM support but pins to whisper.cpp v1.8.3 fork. We can add streaming later. |
| **Hotkey** | `evdev-shortcut 0.2` | Display-server agnostic (works on X11, Wayland, headless). Reads from `/dev/input/*` directly. Voxtype uses evdev too. |
| **Injection** | Shelling out | No Rust crate handles this well. ydotool is battle-tested by voxd, whisper-talk, Voxtype. We wrap it in a clean trait with fallback chain. |
|| **Tray** | `tray-icon 0.19` + `muda 0.15` | Most actively maintained system tray crate for Rust. Works with GTK on Linux. |

---

## 3. Module Breakdown

### `config.rs` — Configuration Management
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub audio: AudioConfig,
    pub stt: SttConfig,
    pub injection: InjectionConfig,
    pub postprocess: PostProcessConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub key: String,            // e.g., "rightshift", "f12"
    pub modifiers: Vec<String>, // e.g., ["ctrl", "alt"]
    pub mode: RecordMode,       // PushToTalk | Toggle
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub sample_rate: u32,       // default: 16000
    pub channels: u16,          // default: 1 (mono)
    pub device: Option<String>, // None = default input device
    pub buffer_size: u32,       // default: 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    pub model_path: PathBuf,    // e.g., ~/.local/share/lindiction/models/ggml-base.en.bin
    pub threads: u32,           // default: num_cpus / 2
    pub language: String,       // default: "en"
    pub translate: bool,        // default: false (translate to English)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionConfig {
    pub method: InjectionMethod, // Ydotool | Wtype | Clipboard | Auto
    pub type_delay_ms: u32,     // ms between keystrokes for ydotool, default: 5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessConfig {
    pub remove_fillers: bool,   // default: true
    pub filler_words: Vec<String>, // ["um", "uh", "like", "you know", "so"]
    pub capitalize_sentences: bool, // default: true
    pub trim_silence: bool,     // default: true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode { PushToTalk, Toggle }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InjectionMethod { Ydotool, Wtype, Clipboard, Auto }
```

**Responsibilities:**
- Load config from `~/.config/lindiction/config.toml` (XDG)
- Validate all fields, apply defaults for missing keys
- Watch config file for changes (optional, v0.2)

### `error.rs` — Error Types
```rust
#[derive(Debug, thiserror::Error)]
pub enum LindictionError {
    #[error("Audio device error: {0}")]
    AudioDevice(String),

    #[error("Audio capture failed: {0}")]
    AudioCapture(String),

    #[error("Whisper model not found: {path}")]
    ModelNotFound { path: PathBuf },

    #[error("Whisper inference failed: {0}")]
    Inference(String),

    #[error("VAD processing failed: {0}")]
    Vad(String),

    #[error("Text injection failed: {method}: {reason}")]
    Injection { method: String, reason: String },

    #[error("Hotkey capture failed: {0}")]
    Hotkey(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("IPC error: {0}")]
    Ipc(String),
}

pub type Result<T> = std::result::Result<T, LindictionError>;
```

### `audio.rs` — Audio Capture
```rust
pub struct AudioCapture {
    device: cpal::Device,
    config: cpal::StreamConfig,
    sample_rate: u32,
}

pub struct AudioStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Vec<f32>>,
    _stream: cpal::Stream,
}
```

**Responsibilities:**
- Enumerate input devices, select default or configured device
- Open a cpal input stream at 16kHz mono f32
- Push audio chunks (Vec<f32>) to a tokio channel
- Handle device errors gracefully (reconnect, notify user)

### `vad.rs` — Voice Activity Detection
```rust
pub struct VadDetector {
    engine: earshot::Vad,
    threshold: f32,           // default: 0.5
    min_speech_ms: u32,       // default: 250ms
    min_silence_ms: u32,      // default: 300ms
    hangover_ms: u32,         // default: 500ms (keep recording after brief silence)
}

#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    SpeechStart,
    SpeechEnd,
    Silence,
}
```

**Responsibilities:**
- Feed 256-sample frames (16ms @ 16kHz) to earshot VAD
- Maintain state machine: Idle → Speaking → Hangover → Idle
- Emit VadEvent::SpeechStart on first speech detection
- Emit VadEvent::SpeechEnd after sustained silence (post-hangover)

### `vad_state.rs` — VAD State Machine
```
         ┌──────────────────────┐
         │        Idle          │
         │  (collecting silence)│
         └──────────┬───────────┘
                    │ speech detected
                    ▼
         ┌──────────────────────┐
         │      Speaking        │
         │  (collecting audio)  │
         └──────────┬───────────┘
                    │ silence for min_silence_ms
                    ▼
         ┌──────────────────────┐
         │      Hangover        │
         │  (brief pause, keep  │
         │   recording)         │
         └──────────┬───────────┘
                    │ speech resumes → back to Speaking
                    │ hangover expires → emit SpeechEnd, go to Idle
                    ▼
         ┌──────────────────────┐
         │   Transcribe &       │
         │   Inject             │
         └──────────────────────┘
```

### `stt.rs` — Speech-to-Text
```rust
pub struct SttEngine {
    ctx: whisper_rs::WhisperContext,
    params: whisper_rs::FullParams<'static>,
}

impl SttEngine {
    pub fn new(model_path: &Path, threads: u32) -> Result<Self>;
    pub fn transcribe(&mut self, audio: &[f32]) -> Result<String>;
    pub fn transcribe_with_metadata(&mut self, audio: &[f32]) -> Result<TranscriptionResult>;
}

pub struct TranscriptionResult {
    pub text: String,
    pub segments: Vec<Segment>,
    pub language: String,
    pub duration_ms: u64,
}

pub struct Segment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub confidence: f32,
}
```

**Responsibilities:**
- Load whisper.cpp model (GGML format) at startup
- Transcribe f32 PCM audio (16kHz mono) to text
- Return structured result with segments and confidence
- Thread-safe (hold mutex around whisper context)

### `postprocess.rs` — Text Post-Processing
```rust
pub struct PostProcessor {
    config: PostProcessConfig,
    filler_pattern: Regex, // compiled from config.filler_words
}

impl PostProcessor {
    pub fn new(config: &PostProcessConfig) -> Self;
    pub fn process(&self, text: &str) -> String;
}
```

**Pipeline (applied in order):**
1. Strip leading/trailing whitespace
2. Remove filler words (case-insensitive regex: `\b(um|uh|like|you know)\b`)
3. Collapse multiple spaces
4. Capitalize first letter of each sentence
5. Ensure trailing period if missing

### `injector.rs` — Text Injection
```rust
#[async_trait]
pub trait TextInjector: Send + Sync {
    async fn inject(&self, text: &str) -> Result<()>;
    fn name(&self) -> &str;
}

pub struct YdotoolInjector { type_delay_ms: u32 }
pub struct WtypeInjector;
pub struct ClipboardInjector;

pub struct InjectorChain {
    methods: Vec<Box<dyn TextInjector>>,
}

impl InjectorChain {
    pub fn auto() -> Self; // Build fallback chain based on detected display server
    pub async fn inject(&self, text: &str) -> Result<()> {
        // Try each method in order, return on first success
        for method in &self.methods {
            match method.inject(text).await {
                Ok(()) => return Ok(()),
                Err(e) => warn!("{} failed: {}, trying next", method.name(), e),
            }
        }
        Err(LindictionError::Injection {
            method: "all".into(),
            reason: "all injection methods failed".into(),
        })
    }
}
```

**Fallback chain (Auto mode):**
1. `wtype` — Wayland-native, best Unicode support (try first on Wayland)
2. `ydotool` — Kernel-level, works everywhere (fallback or primary on X11)
3. `Clipboard` — `wl-copy` / `xclip` + Ctrl+V via ydotool (last resort)

### `hotkey.rs` — Global Hotkey Listener
```rust
pub struct HotkeyListener {
    shortcut: evdev_shortcut::ShortcutListener,
    key: Key,
    modifiers: Vec<Modifier>,
    mode: RecordMode,
}

pub enum HotkeyEvent {
    Press,   // key/button pressed
    Release, // key/button released
}

impl HotkeyListener {
    pub fn new(config: &HotkeyConfig) -> Result<Self>;
    pub fn listen(&mut self) -> Result<mpsc::Receiver<HotkeyEvent>>;
}
```

**Responsibilities:**
- Open evdev device for keyboard input
- Register configured hotkey (e.g., RightShift)
- Emit HotkeyEvent::Press / HotkeyEvent::Release to channel
- Handle permission errors (user not in `input` group → print setup instructions)

### `ipc.rs` — Daemon IPC
```rust
pub enum DaemonCommand {
    Start,
    Stop,
    Status,
    TranscribeFile { path: PathBuf },
    SetConfig { key: String, value: String },
}

pub enum DaemonResponse {
    Ok,
    Status { state: DaemonState, uptime: Duration },
    Transcription { text: String },
    Error { message: String },
}

pub struct IpcServer {
    listener: tokio::net::UnixListener,
    socket_path: PathBuf,
}
```

**Responsibilities:**
- Listen on Unix socket at `/run/user/{uid}/lindiction/lindictd.sock`
- Accept connections from CLI client
- Parse JSON commands, dispatch to daemon, return JSON responses

### `tray.rs` — System Tray
```rust
pub struct TrayManager {
    tray: tray-icon::TrayIcon,
    menu: muda::Menu,
}

#[derive(Clone, Copy)]
pub enum TrayState { Idle, Recording, Processing, Error }
```

**Responsibilities:**
- Create system tray icon with state indicator
- Menu: Start/Stop, Settings (open config file), About, Quit
- Update icon based on DaemonState (idle → green mic, recording → red mic)

### `model.rs` — Model Management
```rust
pub struct ModelManager {
    models_dir: PathBuf, // ~/.local/share/lindiction/models/
}

impl ModelManager {
    pub fn ensure_model(&self, name: &str) -> Result<PathBuf>;
    pub fn download_model(&self, name: &str) -> Result<PathBuf>;
    pub fn list_models(&self) -> Vec<ModelInfo>;
}
```

**Responsibilities:**
- Download whisper GGML models from HuggingFace on first run
- Validate model file integrity
- Cache models locally, never re-download unless corrupted

---

## 4. Audio Pipeline Data Flow

### End-to-End Flow

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                           AUDIO PIPELINE                                     │
│                                                                              │
│  Microphone                                                                  │
│      │                                                                       │
│      ▼                                                                       │
│  cpal::Stream ──[Vec<f32>, 1024 samples @ 16kHz]──► AudioCapture            │
│      │                                                (64ms chunks)           │
│      ▼                                                                       │
│  earshot::Vad ──[256 samples @ 16kHz]──► VadDetector                        │
│      │                                    (16ms frames, score ∈ [0,1])        │
│      │                                                                       │
│      ▼                                                                       │
│  VAD State Machine                                                          │
│  ┌─────────────────────────────────────────────────┐                         │
│  │  Idle ──speech──► Speaking ──silence──► Hangover │                         │
│  │                        │              │         │                         │
│  │                        │    speech     │         │                         │
│  │                        │◄──────────────┘         │                         │
│  │                        │                          │                         │
│  │                   timeout                    ◄───┘                        │
│  │                        │                                                   │
│  │                        ▼                                                   │
│  │                 Emit SpeechEnd                                             │
│  └─────────────────────────────────────────────────┘                         │
│      │                                                                       │
│      │  Collected audio: Vec<f32> (all speech frames, 16kHz mono)            │
│      ▼                                                                       │
│  SttEngine::transcribe()                                                    │
│  ┌─────────────────────────────────────────────────┐                         │
│  │  whisper_rs::WhisperContext::full()              │                         │
│  │  Input:  &[f32] audio (16kHz, mono)             │                         │
│  │  Output: String raw_text                         │                         │
│  │  Latency: ~200-800ms (base.en, CPU)             │                         │
│  └─────────────────────────────────────────────────┘                         │
│      │                                                                       │
│      │  raw_text: "Um so like I think we should, uh, deploy it?"            │
│      ▼                                                                       │
│  PostProcessor::process()                                                   │
│  ┌─────────────────────────────────────────────────┐                         │
│  │  1. Remove fillers → "I think we should deploy it?"│                      │
│  │  2. Capitalize sentences                         │                        │
│  │  3. Trim whitespace                              │                        │
│  └─────────────────────────────────────────────────┘                         │
│      │                                                                       │
│      │  cleaned_text: "I think we should deploy it?"                        │
│      ▼                                                                       │
│  InjectorChain::inject()                                                    │
│  ┌─────────────────────────────────────────────────┐                         │
│  │  Try wtype → ydotool → clipboard+paste           │                        │
│  │  Text appears at cursor in focused app            │                        │
│  └─────────────────────────────────────────────────┘                         │
│                                                                              │
│  Total latency target: <1.5s (MVP), <700ms (v1.0 goal)                     │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Latency Budget (MVP Target: <1.5s)

| Stage | Target | Notes |
|-------|--------|-------|
| VAD silence detection | 300ms | min_silence_ms before triggering SpeechEnd |
| Audio buffer flush | 50ms | Collect remaining frames |
| Whisper inference | 200-800ms | base.en model, CPU, depends on audio length |
| Post-processing | <10ms | Regex substitution, negligible |
| Text injection | 50-200ms | ydotool typing speed, depends on text length |
| **Total** | **~600-1360ms** | Within MVP target |

### Audio Data Format

All audio flows as `Vec<f32>` at 16kHz mono:
- cpal captures in `f32` format (native)
- earshot VAD expects `&[f32]` slices of 256 samples
- whisper-rs expects `&[f32]` at 16kHz
- No format conversion needed — clean pipeline

---

## 5. Error Handling Strategy

### Principles
1. **Domain errors** use `thiserror` — typed, exhaustive, matchable
2. **Top-level errors** use `anyhow` — contextual, chainable
3. **Non-fatal errors** log + continue (audio glitches, single injection failure)
4. **Fatal errors** log + exit gracefully (no model, no audio device, permission denied)

### Error Recovery Matrix

| Error | Fatal? | Recovery |
|-------|--------|----------|
| Audio device disconnected | No | Try reconnect after 2s, notify via tray |
| Whisper model not found | Yes | Print download instructions, exit |
| Whisper inference OOM | No | Log error, skip this utterance |
| ydotool not installed | No | Fall back to wtype → clipboard |
| evdev permission denied | Yes | Print `usermod -aG input` instructions |
| Config parse error | Yes | Use defaults + warn, or exit (strict mode) |
| IPC socket busy | Yes | Another instance running, exit |
| VAD error | No | Treat as silence, continue |

### Daemon Panic Handler
```rust
std::panic::set_hook(Box::new(|info| {
    error!("PANIC: {}", info);
    // Clean up IPC socket
    let _ = std::fs::remove_file(SOCKET_PATH);
    // Show desktop notification
    let _ = notify_rust::Notification::new()
        .summary("Lindiction crashed")
        .body(&format!("{}", info))
        .show();
}));
```

---

## 6. Configuration Design

### Default Config (`~/.config/lindiction/config.toml`)

```toml
# Lindiction Configuration
# Docs: https://github.com/andre-hugo/lindiction

[hotkey]
# Key to hold/press for dictation (evdev key name)
key = "rightshift"
# Modifier keys: ctrl, alt, shift, meta
modifiers = []
# Recording mode: "push_to_hold" or "toggle"
mode = "push_to_hold"

[audio]
# Sample rate in Hz (must be 16000 for Whisper)
sample_rate = 16000
# Number of channels (1 = mono, 2 = stereo)
channels = 1
# Audio device name (null = system default)
device = null
# Buffer size in samples
buffer_size = 1024

[stt]
# Path to Whisper GGML model file
model_path = "~/.local/share/lindiction/models/ggml-base.en.bin"
# Number of CPU threads for inference (0 = auto)
threads = 0
# Language code ("en", "auto", etc.)
language = "en"
# Translate non-English to English
translate = false

[vad]
# Speech detection threshold (0.0 - 1.0)
threshold = 0.5
# Minimum speech duration to start recording (ms)
min_speech_ms = 250
# Silence duration to end recording (ms)
min_silence_ms = 300
# Extra recording time after silence (ms, catches brief pauses)
hangover_ms = 500

[injection]
# Text injection method: "ydotool", "wtype", "clipboard", "auto"
method = "auto"
# Delay between keystrokes for ydotool (ms)
type_delay_ms = 5

[postprocess]
# Remove filler words from transcription
remove_fillers = true
# Filler words to remove (case-insensitive)
filler_words = ["um", "uh", "ah", "like", "you know", "so", "basically"]
# Capitalize first letter of sentences
capitalize_sentences = true
# Trim leading/trailing silence artifacts
trim_silence = true

[logging]
# Log level: "error", "warn", "info", "debug", "trace"
level = "info"
# Log file path (null = stderr only)
file = "~/.local/share/lindiction/lindiction.log"
```

---

## 7. Build and Packaging

### Build Commands

```bash
# Clone and build
git clone https://github.com/andre-hugo/lindiction.git
cd lindiction

# Build release binaries (both lindictd and lindiction CLI)
cargo build --release

# Binaries at:
# target/release/lindictd      (daemon)
# target/release/lindiction    (CLI client)

# Download whisper model
./scripts/setup.sh
```

### Setup Script (`scripts/setup.sh`)

```bash
#!/bin/bash
set -euo pipefail

MODELS_DIR="$HOME/.local/share/lindiction/models"
mkdir -p "$MODELS_DIR"

MODEL_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"
MODEL_PATH="$MODELS_DIR/ggml-base.en.bin"

if [ ! -f "$MODEL_PATH" ]; then
    echo "Downloading Whisper base.en model (142 MB)..."
    curl -L -o "$MODEL_PATH" "$MODEL_URL"
    echo "Model saved to $MODEL_PATH"
else
    echo "Model already exists at $MODEL_PATH"
fi

# Check dependencies
echo "Checking system dependencies..."
for cmd in ydotool; do
    if ! command -v $cmd &> /dev/null; then
        echo "WARNING: $cmd not found. Install it for text injection."
        echo "  Ubuntu: sudo apt install ydotool"
        echo "  Arch:   sudo pacman -S ydotool"
    fi
done

echo ""
echo "Setup complete! Next steps:"
echo "  1. Add yourself to input group: sudo usermod -aG input \$USER"
echo "  2. Log out and back in (for group change)"
echo "  3. Start daemon: lindictd"
echo "  4. Hold RightShift and speak!"
```

### systemd User Service (`systemd/lindictd.service`)

```ini
[Unit]
Description=Lindiction Voice Dictation Daemon
Documentation=https://github.com/andre-hugo/lindiction
After=graphical-session.target pipewire.service
Wants=pipewire.service

[Service]
Type=simple
ExecStart=%h/.local/bin/lindictd
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=info

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=%h/.config/lindiction %h/.local/share/lindiction /run/user/%U
PrivateTmp=true

[Install]
WantedBy=default.target
```

### .deb Packaging (via `cargo-deb`)

Add to `crates/lindictd/Cargo.toml`:
```toml
[package.metadata.deb]
name = "lindiction"
maintainer = "Andre Hugo <ahugo@pm.me>"
copyright = "2026, Andre Hugo"
license-file = ["LICENSE", "0"]
depends = "$auto, ydotool, libasound2, libpipewire-0.3-0"
section = "utils"
priority = "optional"
assets = [
    ["target/release/lindictd", "usr/bin/", "755"],
    ["target/release/lindiction", "usr/bin/", "755"],
    ["systemd/lindictd.service", "usr/lib/systemd/user/", "644"],
    ["config.toml.example", "etc/lindiction/config.toml.example", "644"],
]
```

Build:
```bash
cargo install cargo-deb
cargo deb -p lindictd
# Output: target/debian/lindiction_0.1.0_amd64.deb
```

---

## 8. Testing Strategy

### Unit Tests (per module)

| Module | Test | What It Verifies |
|--------|------|-----------------|
| `config` | `test_default_config` | Defaults applied correctly |
| `config` | `test_load_config` | TOML parsing, validation |
| `config` | `test_invalid_hotkey` | Error on unknown key name |
| `vad` | `test_silence_detection` | VAD stays Idle on silence |
| `vad` | `test_speech_detection` | VAD transitions Speaking on voice |
| `vad` | `test_hangover` | VAD waits hangover_ms before SpeechEnd |
| `postprocess` | `test_filler_removal` | "um hello" → "hello" |
| `postprocess` | `test_capitalize` | "hello world." → "Hello world." |
| `postprocess` | `test_no_change` | Clean text passes through unchanged |
| `injector` | `test_clipboard_fallback` | Clipboard injector writes to clipboard |
| `error` | `test_error_display` | Error messages are human-readable |

### Integration Tests (`tests/`)

| Test | What It Verifies |
|------|-----------------|
| `integration_audio` | cpal opens default device, captures audio, returns f32 samples |
| `integration_vad` | VAD processes audio file, detects speech segments |
| `integration_stt` | Whisper transcribes test WAV, returns expected text |
| `integration_inject` | ydotool types test string (requires running desktop) |

### Manual Test Plan

1. **Install and setup**
   - [ ] `cargo build --release` succeeds
   - [ ] `scripts/setup.sh` downloads model
   - [ ] `lindictd` starts without errors
   - [ ] System tray icon appears

2. **Basic dictation**
   - [ ] Hold RightShift → recording starts (tray icon turns red)
   - [ ] Speak "Hello world" → release → text appears at cursor
   - [ ] Works in terminal (Alacritty/GNOME Terminal)
   - [ ] Works in browser (Firefox/Chrome address bar)
   - [ ] Works in text editor (VS Code, gedit)

3. **Post-processing**
   - [ ] "Um hello uh world" → "Hello world."
   - [ ] "hello" → "Hello." (capitalize + period)
   - [ ] Long pause mid-sentence doesn't cut off text

4. **Injection methods**
   - [ ] ydotool types correctly
   - [ ] Fallback to clipboard works when ydotool fails
   - [ ] Works on Wayland (GNOME)
   - [ ] Works on X11 (Xfce)

5. **Error handling**
   - [ ] No model → clear error message
   - [ ] No microphone → clear error message
   - [ ] Not in input group → setup instructions printed

---

## 9. Day-by-Day 4-Week Schedule

### Week 1: Foundation (40 hours)

| Day | Hours | Tasks | Milestone |
|-----|-------|-------|-----------|
| Mon | 8 | Project setup: init Cargo workspace, CI, linting. Create all module stubs. Write config.rs with serde/TOML parsing. | `cargo build` passes |
| Tue | 8 | Implement `audio.rs`: cpal input stream at 16kHz mono f32. Test with `arecord` equivalent. Write `error.rs`. | Audio capture works, can log mic samples |
| Wed | 8 | Implement `hotkey.rs`: evdev-shortcut integration. Test RightShift detection. Handle permission errors gracefully. | Hotkey press/release events in terminal |
| Thu | 8 | Implement `ipc.rs`: Unix socket server + CLI client. JSON protocol for start/stop/status. | `lindiction status` returns daemon state |
| Fri | 8 | Wire up main daemon loop: hotkey → state machine (idle/recording) → audio capture. Log collected audio length. End-to-end hotkey→record→stop. | Can record audio on hotkey press |

### Week 2: Core Pipeline (40 hours)

| Day | Hours | Tasks | Milestone |
|-----|-------|-------|-----------|
| Mon | 8 | Integrate whisper-rs: download model, load in SttEngine, transcribe test WAV. | "Hello world" transcribes correctly from file |
| Tue | 8 | Implement `vad.rs`: earshot integration. Feed audio frames, get speech/silence scores. Test with recorded audio. | VAD detects speech segments |
| Wed | 8 | Implement `vad_state.rs`: state machine (Idle→Speaking→Hangover). Collect speech audio. Emit SpeechEnd with Vec<f32>. | State machine transitions correctly |
| Thu | 8 | Integrate full pipeline: hotkey → audio → VAD → collect speech → whisper transcribe → log result. First end-to-end dictation! | Speak → see transcription in logs |
| Fri | 8 | Implement `postprocess.rs`: filler removal, capitalize, trim. Test regex patterns. | "Um hello uh world" → "Hello world." |

### Week 3: Injection & Polish (40 hours)

| Day | Hours | Tasks | Milestone |
|-----|-------|-------|-----------|
| Mon | 8 | Implement `injector.rs`: YdotoolInjector (shell out to ydotool type). Test in terminal. | Text types at cursor via ydotool |
| Tue | 8 | Implement WtypeInjector + ClipboardInjector. Build InjectorChain with Auto mode. Test fallback. | Injection works with all 3 methods |
| Wed | 8 | Full pipeline end-to-end: hold hotkey → speak → transcribe → postprocess → inject at cursor. Fix bugs. | **First usable dictation!** |
| Thu | 8 | Implement `tray.rs`: tray-icon with recording state. Menu with start/stop/quit. | System tray shows recording indicator |
| Fri | 8 | Implement `model.rs`: auto-download model on first run. Model validation. | First run downloads model automatically |

### Week 4: Packaging & Release (40 hours)

| Day | Hours | Tasks | Milestone |
|-----|-------|-------|-----------|
| Mon | 8 | systemd service file. Setup script. Install script. Test `cargo install --path .` | Clean install workflow works |
| Tue | 8 | .deb packaging with cargo-deb. Test on fresh Ubuntu VM. | `.deb` installs cleanly |
| Wed | 8 | Write README.md with screenshots, installation, usage, troubleshooting. | README complete |
| Thu | 8 | Integration testing: all manual test plan items. Fix remaining bugs. | All manual tests pass |
| Fri | 8 | Performance tuning, logging cleanup, edge cases. Tag v0.1.0 release. | **v0.1.0 released!** |

---

## 10. Key Code Snippets

### Audio Capture Loop (cpal)

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

pub fn start_audio_capture(
    config: &AudioConfig,
) -> anyhow::Result<(cpal::Stream, mpsc::UnboundedReceiver<Vec<f32>>)> {
    let host = cpal::default_host();

    let device = match &config.device {
        Some(name) => host
            .input_devices()?
            .find(|d| d.name().ok().as_deref() == Some(name))
            .ok_or_else(|| anyhow!("Audio device '{}' not found", name))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("No default input device"))?,
    };

    let stream_config = cpal::StreamConfig {
        channels: config.channels,
        sample_rate: cpal::SampleRate(config.sample_rate),
        buffer_size: cpal::BufferSize::Fixed(config.buffer_size),
    };

    let (tx, rx) = mpsc::unbounded_channel::<Vec<f32>>();

    let stream = device.build_input_stream(
        &stream_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let _ = tx.send(data.to_vec());
        },
        |err| error!("Audio stream error: {}", err),
        None,
    )?;

    stream.play()?;
    Ok((stream, rx))
}
```

### VAD State Machine

```rust
use earshot::Vad;

pub struct VadStateMachine {
    vad: Vad,
    state: VadState,
    threshold: f32,
    min_speech_frames: usize,
    min_silence_frames: usize,
    hangover_frames: usize,
    frame_counter: usize,
    speech_buffer: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VadState {
    Idle,
    Speaking,
    Hangover,
}

pub enum VadOutput {
    Listening,           // Still in idle, no speech detected
    Recording(Vec<f32>), // Accumulating speech audio
    Complete(Vec<f32>),  // Speech ended, here's the audio
}

impl VadStateMachine {
    pub fn new(sample_rate: u32, threshold: f32) -> Self {
        Self {
            vad: Vad::new(sample_rate),
            state: VadState::Idle,
            threshold,
            min_speech_frames: 16,  // ~250ms at 16ms frames
            min_silence_frames: 19, // ~300ms
            hangover_frames: 31,    // ~500ms
            frame_counter: 0,
            speech_buffer: Vec::with_capacity(sample_rate as usize * 30), // 30s pre-alloc
        }
    }

    pub fn process_frame(&mut self, frame: &[f32]) -> VadOutput {
        let score = self.vad.is_speech(frame);
        let is_speech = score > self.threshold;

        match self.state {
            VadState::Idle => {
                if is_speech {
                    self.state = VadState::Speaking;
                    self.frame_counter = 1;
                    self.speech_buffer.clear();
                    self.speech_buffer.extend_from_slice(frame);
                    VadOutput::Recording(self.speech_buffer.clone())
                } else {
                    VadOutput::Listening
                }
            }
            VadState::Speaking => {
                self.speech_buffer.extend_from_slice(frame);
                if is_speech {
                    self.frame_counter = 0; // reset silence counter
                    VadOutput::Recording(self.speech_buffer.clone())
                } else {
                    self.frame_counter += 1;
                    if self.frame_counter >= self.min_silence_frames {
                        self.state = VadState::Hangover;
                        self.frame_counter = 0;
                    }
                    VadOutput::Recording(self.speech_buffer.clone())
                }
            }
            VadState::Hangover => {
                self.speech_buffer.extend_from_slice(frame);
                if is_speech {
                    // Speech resumed during hangover — go back to Speaking
                    self.state = VadState::Speaking;
                    self.frame_counter = 0;
                    VadOutput::Recording(self.speech_buffer.clone())
                } else {
                    self.frame_counter += 1;
                    if self.frame_counter >= self.hangover_frames {
                        // Hangover expired — finalize
                        self.state = VadState::Idle;
                        let audio = std::mem::take(&mut self.speech_buffer);
                        VadOutput::Complete(audio)
                    } else {
                        VadOutput::Recording(self.speech_buffer.clone())
                    }
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.state = VadState::Idle;
        self.frame_counter = 0;
        self.speech_buffer.clear();
    }
}
```

### Whisper Inference

```rust
use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};

pub struct SttEngine {
    ctx: WhisperContext,
    threads: u32,
}

impl SttEngine {
    pub fn new(model_path: &Path, threads: u32) -> anyhow::Result<Self> {
        if !model_path.exists() {
            anyhow::bail!("Model not found: {}. Run 'lindiction setup' to download.", model_path.display());
        }

        let ctx = WhisperContext::new_with_params(
            model_path.to_str().unwrap(),
            WhisperContextParameters::default(),
        ).map_err(|e| anyhow!("Failed to load Whisper model: {}", e))?;

        Ok(Self { ctx, threads })
    }

    pub fn transcribe(&mut self, audio: &[f32]) -> anyhow::Result<String> {
        let mut state = self.ctx.create_state()
            .map_err(|e| anyhow!("Failed to create Whisper state: {}", e))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads as i32);
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, audio)
            .map_err(|e| anyhow!("Whisper inference failed: {}", e))?;

        let num_segments = state.full_n_segments()
            .map_err(|e| anyhow!("Failed to get segment count: {}", e))?;

        let mut text = String::new();
        for i in 0..num_segments {
            let segment = state
                .full_get_segment_text(i)
                .map_err(|e| anyhow!("Failed to get segment text: {}", e))?;
            text.push_str(&segment);
        }

        Ok(text.trim().to_string())
    }
}
```

### Text Injection with Fallback Chain

```rust
use std::process::Command;
use async_trait::async_trait;

#[async_trait]
pub trait TextInjector: Send + Sync {
    async fn inject(&self, text: &str) -> anyhow::Result<()>;
    fn name(&self) -> &str;
}

pub struct YdotoolInjector { type_delay_ms: u32 }

impl YdotoolInjector {
    pub fn new(type_delay_ms: u32) -> Self { Self { type_delay_ms } }
}

#[async_trait]
impl TextInjector for YdotoolInjector {
    async fn inject(&self, text: &str) -> anyhow::Result<()> {
        let output = Command::new("ydotool")
            .args(["type", "--keydelay", &self.type_delay_ms.to_string(), "--", text])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ydotool failed: {}", stderr);
        }
        Ok(())
    }

    fn name(&self) -> &str { "ydotool" }
}

pub struct WtypeInjector;

#[async_trait]
impl TextInjector for WtypeInjector {
    async fn inject(&self, text: &str) -> anyhow::Result<()> {
        let output = Command::new("wtype")
            .arg(text)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("wtype failed: {}", stderr);
        }
        Ok(())
    }

    fn name(&self) -> &str { "wtype" }
}

pub struct ClipboardInjector;

#[async_trait]
impl TextInjector for ClipboardInjector {
    async fn inject(&self, text: &str) -> anyhow::Result<()> {
        // Copy to clipboard via wl-copy (Wayland) or xclip (X11)
        let copied = Command::new("wl-copy")
            .arg(text)
            .output()
            .ok()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !copied {
            Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(text.as_bytes())?;
                    child.wait()
                })?;
        }

        // Simulate Ctrl+V paste
        Command::new("ydotool")
            .args(["key", "ctrl+v"])
            .output()?;

        Ok(())
    }

    fn name(&self) -> &str { "clipboard" }
}

pub struct InjectorChain {
    methods: Vec<Box<dyn TextInjector>>,
}

impl InjectorChain {
    pub fn auto() -> Self {
        // Detect display server and build fallback chain
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok()
            || std::env::var("XDG_SESSION_TYPE").ok().as_deref() == Some("wayland");

        let methods: Vec<Box<dyn TextInjector>> = if is_wayland {
            vec![
                Box::new(WtypeInjector),
                Box::new(YdotoolInjector::new(5)),
                Box::new(ClipboardInjector),
            ]
        } else {
            vec![
                Box::new(YdotoolInjector::new(5)),
                Box::new(ClipboardInjector),
            ]
        };

        Self { methods }
    }

    pub async fn inject(&self, text: &str) -> anyhow::Result<()> {
        for method in &self.methods {
            match method.inject(text).await {
                Ok(()) => {
                    tracing::debug!("Injected via {}", method.name());
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("{} failed: {}, trying next", method.name(), e);
                }
            }
        }
        anyhow::bail!("All injection methods failed")
    }
}
```

### Hotkey Listener (evdev)

```rust
use evdev_shortcut::{ShortcutListener, Shortcut, Key, Modifier};
use tokio::sync::mpsc;

pub enum HotkeyEvent { Press, Release }

pub struct HotkeyListener {
    key: Key,
    modifiers: Vec<Modifier>,
}

impl HotkeyListener {
    pub fn new(config: &HotkeyConfig) -> anyhow::Result<Self> {
        let key = parse_key(&config.key)?;
        let modifiers: Vec<Modifier> = config.modifiers
            .iter()
            .map(|m| parse_modifier(m))
            .collect::<anyhow::Result<_>>()?;

        Ok(Self { key, modifiers })
    }

    pub fn listen(self) -> anyhow::Result<mpsc::Receiver<HotkeyEvent>> {
        let (tx, rx) = mpsc::channel(32);

        let shortcut = Shortcut::new(self.key, self.modifiers);
        let mut listener = ShortcutListener::new(vec![shortcut])?;

        std::thread::spawn(move || {
            loop {
                match listener.next() {
                    Some((_, true)) => { let _ = tx.blocking_send(HotkeyEvent::Press); }
                    Some((_, false)) => { let _ = tx.blocking_send(HotkeyEvent::Release); }
                    None => break,
                }
            }
        });

        Ok(rx)
    }
}

fn parse_key(s: &str) -> anyhow::Result<Key> {
    match s.to_lowercase().as_str() {
        "rightshift" => Ok(Key::KEY_RIGHTSHIFT),
        "leftshift" => Ok(Key::KEY_LEFTSHIFT),
        "rightctrl" => Ok(Key::KEY_RIGHTCTRL),
        "f12" => Ok(Key::KEY_F12),
        "capslock" => Ok(Key::KEY_CAPSLOCK),
        _ => anyhow::bail!("Unknown key: {}. See evdev Key enum for options.", s),
    }
}

fn parse_modifier(s: &str) -> anyhow::Result<Modifier> {
    match s.to_lowercase().as_str() {
        "ctrl" | "control" => Ok(Modifier::CTRL),
        "alt" => Ok(Modifier::ALT),
        "shift" => Ok(Modifier::SHIFT),
        "meta" | "super" => Ok(Modifier::META),
        _ => anyhow::bail!("Unknown modifier: {}", s),
    }
}
```

### Main Daemon Loop

```rust
use tokio::signal;
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("lindiction=info")
        .init();

    // Load config
    let config = Config::load()?;

    // Initialize components
    let mut stt = SttEngine::new(&config.stt.model_path, config.stt.threads)?;
    let mut vad = VadStateMachine::new(config.audio.sample_rate, config.vad.threshold);
    let postprocessor = PostProcessor::new(&config.postprocess);
    let injector = InjectorChain::auto();
    let (_audio_stream, mut audio_rx) = start_audio_capture(&config.audio)?;
    let mut hotkey_rx = HotkeyListener::new(&config.hotkey)?.listen()?;

    info!("Lindiction daemon started. Hold {} to dictate.", config.hotkey.key);

    let mut is_recording = false;

    loop {
        tokio::select! {
            // Hotkey events
            Some(event) = hotkey_rx.recv() => {
                match event {
                    HotkeyEvent::Press => {
                        if !is_recording {
                            is_recording = true;
                            vad.reset();
                            info!("🎤 Recording started");
                            // TODO: update tray state
                        }
                    }
                    HotkeyEvent::Release => {
                        if is_recording {
                            is_recording = false;
                            info!("🎤 Recording stopped, processing...");
                            // TODO: finalize any in-progress audio
                        }
                    }
                }
            }

            // Audio frames
            Some(audio_chunk) = audio_rx.recv(), if is_recording => {
                // Process audio through VAD in 256-sample frames
                for frame in audio_chunk.chunks(256) {
                    if frame.len() == 256 {
                        match vad.process_frame(frame) {
                            VadOutput::Complete(speech_audio) => {
                                info!("Speech detected ({} samples), transcribing...", speech_audio.len());

                                match stt.transcribe(&speech_audio) {
                                    Ok(raw_text) => {
                                        let cleaned = postprocessor.process(&raw_text);
                                        info!("Transcribed: {}", cleaned);

                                        if let Err(e) = injector.inject(&cleaned).await {
                                            error!("Injection failed: {}", e);
                                        }
                                    }
                                    Err(e) => error!("Transcription failed: {}", e),
                                }
                            }
                            _ => {} // Still recording or listening
                        }
                    }
                }
            }

            // Shutdown signal
            _ = signal::ctrl_c() => {
                info!("Shutting down...");
                break;
            }
        }
    }

    Ok(())
}
```

---

## Appendix: Competitive Landscape

| Project | Language | STT Backend | Injection | Wayland | Offline | License |
|---------|----------|-------------|-----------|---------|---------|---------|
| **voxd** | Rust | whisper-rs | ydotool | ✅ | ✅ | MIT |
| **waystt** | Rust | whisper-rs / OpenAI API | ydotool / wl-copy | ✅ | ✅ | GPL-3.0 |
| **whisper-talk** | Rust | whisper / OpenAI API | ydotool / clipboard | ✅ | ✅ | MIT |
| **Voxtype** | Rust | whisper.cpp / ONNX | wtype / ydotool / clipboard | ✅ | ✅ | MIT |
| **whisp-rs** | Rust | cloud API | system | ✅ | ❌ | MIT |
| **Speech Note** | C++/Qt | Whisper / Vosk | clipboard | ⚠️ | ✅ | GPL-3.0 |
| **Nerd Dictation** | Python | Vosk | xdotool / ydotool | ⚠️ | ✅ | MIT |
| **lindiction** | Rust | whisper.cpp | wtype / ydotool / clipboard | ✅ | ✅ | MIT |

**Key differentiator for lindiction:** We combine the best ideas from all projects — Rust performance, whisper.cpp accuracy, earshot VAD (no ONNX dep), robust 3-method injection fallback chain, and Wispr Flow-style AI post-processing.
