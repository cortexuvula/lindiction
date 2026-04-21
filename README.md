# Lindiction

Push-to-talk voice dictation for Linux. Hold a hotkey, speak, release — transcribed text appears at the cursor.

MVP scope: Ubuntu 24.04 / X11 / GNOME. Wayland, system tray, systemd service, `.deb` packaging, and VAD endpointing are v0.2+.

## Requirements

- Ubuntu 24.04 LTS, X11 session (`echo $XDG_SESSION_TYPE` returns `x11`)
- PipeWire with PulseAudio compatibility (default on modern Ubuntu)
- Rust toolchain (install via [rustup](https://rustup.rs/))
- A working microphone

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
    libclang-dev libasound2-dev libpulse-dev libdbus-1-dev curl
```

Build:

```bash
cargo build --release
```

First build takes several minutes (compiles whisper.cpp from source). First run auto-downloads the model; no manual `curl` step needed.

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

### Tray menu

When the daemon is running, a microphone icon appears in the system tray. Left-click it (or right-click, depending on your desktop) to open the menu:

- **Open config…** — opens `~/.config/lindiction/config.toml` in your default text editor, creating an empty file if it doesn't exist yet. Save the file and restart the daemon to pick up changes.
- **About Lindiction** — shows a short desktop notification with the current version, license, and project URL.
- **Help** — opens [this repository](https://github.com/cortexuvula/lindiction) in your default browser.
- **Quit** — exits the daemon cleanly (same as Ctrl-C in the daemon's terminal).

The tray icon also changes color to reflect daemon state: dim microphone (idle), red dot (recording), refresh spinner (transcribing).

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

The unit file is installed by the `.deb` at `/lib/systemd/user/lindiction.service`. If you built from source, copy it yourself:

```bash
mkdir -p ~/.config/systemd/user
cp systemd/lindiction.service ~/.config/systemd/user/
```

## Configuration

Lindiction reads an optional TOML file at `~/.config/lindiction/config.toml` (or `$XDG_CONFIG_HOME/lindiction/config.toml`). If the file is absent, the built-in defaults apply. Unknown fields are rejected at startup.

Precedence for the model path: `--model` CLI flag > `LINDICTION_MODEL` env var > `[model].path` in TOML > default (`models/ggml-tiny.en.bin`).

### Full schema with defaults

```toml
[hotkey]
# Hotkey binding: "+"-separated, case-insensitive.
# Modifiers: ctrl (alias: control), alt, shift, super (alias: meta).
# Keys: letters a-z, digits 0-9, space, enter (alias: return), tab,
#       escape (alias: esc), backspace, f1-f24, arrow keys
#       (up, down, left, right).
binding = "ctrl+alt+space"

[model]
# Path to GGML whisper model file.
path = "models/ggml-tiny.en.bin"

[postprocess]
# Remove common filler words before injection (case-insensitive, word-boundary).
remove_fillers = true
filler_words = ["um", "uh", "ah", "like", "you know", "so", "basically"]

# Uppercase the first letter of the utterance and of each sentence
# that follows `. `, `? `, or `! `.
capitalize_sentences = true

# Append a `.` if the final character is not `.`, `?`, or `!`.
ensure_trailing_period = true
```

### Opt out of postprocessing

To get raw whisper output (v0.1 behaviour), set all three toggles to `false`:

```toml
[postprocess]
remove_fillers = false
capitalize_sentences = false
ensure_trailing_period = false
```

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

## Troubleshooting

**"Model not found"** — first launch auto-downloads the default model. If it fails (network issue, etc.), rerun the daemon to retry. To use an existing local model, pass `--model /path/to/model.bin` or set `LINDICTION_MODEL=/path/to/model.bin`.

**"xdotool not found"** — `sudo apt install xdotool`.

**"No audio input device"** — run `pactl list sources short`. If empty, your PipeWire/PulseAudio is not running. Log out and back in, or `systemctl --user restart pipewire`.

**"Hotkey registration failed"** — another application is bound to your configured hotkey. Close it, or change `[hotkey].binding` in your config file.

**"Config parse error" / "Unknown config field"** — the TOML file at `~/.config/lindiction/config.toml` has a syntax error or uses a field name that is not part of the current schema. Check it against the schema in the Configuration section above, or delete the file to fall back to defaults.

**"Invalid hotkey binding"** — the `[hotkey] binding` value could not be parsed. Valid modifiers are `ctrl` (alias `control`), `alt`, `shift`, `super` (alias `meta`). Valid keys are letters `a`–`z`, digits `0`–`9`, `space`, `enter` (alias `return`), `tab`, `escape` (alias `esc`), `backspace`, `f1`–`f24`, and arrow keys (`up`, `down`, `left`, `right`). Example bindings: `"ctrl+alt+space"`, `"f12"`, `"super+shift+d"`.

**Text appears in the wrong window** — `xdotool type` types into the currently-focused window. Focus the target window before releasing the hotkey.

**Transcriptions are gibberish** — the `tiny.en` model is fast but imprecise. Switch to `ggml-base.en.bin` via `--model`.

**"curl exited with…" on first run** — the auto-download failed. Check your network, then relaunch. The partial download is automatically cleaned up. As a manual fallback:

```bash
mkdir -p ~/.local/share/lindiction/models
curl -L -o ~/.local/share/lindiction/models/ggml-tiny.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
```

**Tray icon doesn't appear** — on Ubuntu 24.04 GNOME, the AppIndicator extension is pre-installed and active. On vanilla upstream GNOME, install and enable it:

```bash
sudo apt install gnome-shell-extension-appindicator
# then enable "Ubuntu AppIndicators" in the Extensions app, or via:
gnome-extensions enable ubuntu-appindicators@ubuntu.com
```

The daemon runs fine without a tray icon — the hotkey still works.

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
