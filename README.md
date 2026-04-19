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
```

```bash
# Integration test (requires a downloaded model)
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

## License

MIT.
