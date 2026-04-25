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

Each release ships three `.deb` variants on the [releases page](https://github.com/cortexuvula/lindiction/releases). Pick the one matching your hardware:

| Variant | Filename suffix | When to use |
|---|---|---|
| **CPU** (default) | `-amd64.deb` | Anything; works on every machine |
| **CUDA** | `-amd64-cuda.deb` | NVIDIA GPU with CUDA runtime installed |
| **Vulkan** | `-amd64-vulkan.deb` | Cross-vendor GPU (Intel Arc, AMD without ROCm, NVIDIA fallback) |

```bash
# CPU build (works on every machine):
wget https://github.com/cortexuvula/lindiction/releases/latest/download/lindiction-v0.8.1-amd64.deb
sudo apt install ./lindiction-v0.8.1-amd64.deb

# Or NVIDIA CUDA build:
wget https://github.com/cortexuvula/lindiction/releases/latest/download/lindiction-v0.8.1-amd64-cuda.deb
sudo apt install ./lindiction-v0.8.1-amd64-cuda.deb
```

The auto-updater preserves the chosen backend across versions starting from v0.8.1 — installing the CUDA `.deb` once means future updates also pull the CUDA variant. (The AMD ROCm `hipblas` build isn't published as a `.deb` yet; use a source build with `--features hipblas` if you need it.)

First run auto-downloads a Whisper model sized to your hardware (~75 MB to ~1.6 GB depending on RAM/VRAM) into `~/.local/share/lindiction/models/`. Expect a one-time delay of 30 s to a few minutes depending on your connection.

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

## Building with GPU support

Lindiction runs on CPU by default. To build with GPU acceleration, enable
one of three mutually-exclusive Cargo features:

```bash
# NVIDIA (needs CUDA runtime on target host):
cargo build --release --features cuda

# Cross-vendor Vulkan (needs libvulkan.so.1 — present on every modern
# Linux; covers Intel Arc, AMD without ROCm, NVIDIA as fallback):
cargo build --release --features vulkan

# AMD via ROCm:
cargo build --release --features hipblas
```

Only one GPU feature can be enabled at a time — they conflict at the
whisper.cpp level. The daemon logs the compiled backend at startup so
you can verify which build you're running.

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

### Subcommands

| Subcommand | Purpose |
|---|---|
| `lindiction autostart enable` | Enable auto-start on graphical login (systemd `--user` unit). |
| `lindiction autostart disable` | Disable auto-start on login. |
| `lindiction autostart status` | Print the current enabled/disabled state. |
| `lindiction replace add <from> <to>` | Add (or update) a word-fix entry. Case-insensitive `from` match. |
| `lindiction replace list` | Print all configured replacements. |
| `lindiction replace remove <from>` | Remove an entry. |
| `lindiction replace edit` | Open `config.toml` in `$EDITOR` for free-form editing. |

The `replace` family edits `~/.config/lindiction/config.toml` in place, preserving comments and formatting. After any change, restart the daemon (`systemctl --user restart lindiction`) to apply.

### Tray menu

When the daemon is running, a microphone icon appears in the system tray. Left-click it (or right-click, depending on your desktop) to open the menu:

- **Update to vX.Y.Z…** *(only when an update is available)* — downloads the latest release, verifies SHA256, installs via `pkexec apt install` (for `.deb` installs) or atomic rename (for `~/.cargo/bin` / `~/.local/bin` / `~/bin` installs), then auto-restarts into the new binary. See the **Auto-update** section below.
- **Pause** — checkbox that mutes the hotkey while keeping the daemon resident. Presses and releases are ignored until you uncheck it. If you pause mid-hold, the in-flight recording is discarded rather than transcribed on resume. Pause state is ephemeral — it does not persist across restart or login.
- **Open config…** — opens `~/.config/lindiction/config.toml` in your default text editor, creating an empty file if it doesn't exist yet. Save the file and click **Restart** below to pick up changes.
- **Auto-start on login** — checkbox that enables or disables the systemd user unit in place. Equivalent to `lindiction autostart enable|disable` below. Hidden when `systemctl --user` is unavailable.
- **Check for updates…** *(hidden when `[update] enabled = false`)* — force an immediate GitHub check and show the result as a notification.
- **About Lindiction** — shows a short desktop notification with the current version, license, and project URL.
- **Help** — opens [this repository](https://github.com/cortexuvula/lindiction) in your default browser.
- **Restart** — graceful shutdown followed by re-launching the daemon with the same arguments. The easiest way to apply config changes without logging out. Any in-flight transcription finishes before the restart. Under a systemd user unit this is invisible to systemd (PID and cgroup are preserved via `execve`).
- **Quit** — exits the daemon cleanly (same as Ctrl-C in the daemon's terminal).

The tray icon reflects daemon state: dim microphone (idle), red dot (recording), refresh spinner (transcribing), pause glyph (paused), or a software-update badge when a new release is available *and* the daemon is currently idle (the in-progress states take precedence over the badge so time-sensitive feedback stays visible).

### Auto-update

Lindiction periodically polls the [GitHub Releases API](https://api.github.com/repos/cortexuvula/lindiction/releases/latest) for a newer version. When one is found, the tray icon switches to a software-update badge and a new **Update to vX.Y.Z…** menu item appears. Clicking it downloads the correct artifact for your install (tarball or `.deb`), verifies its SHA256, installs it, and restarts the daemon automatically.

Two install flows:

- **.deb installs** (binary at `/usr/bin/lindiction`): `pkexec apt install <new.deb>`. A polkit dialog pops up showing the exact command — approve with your password. Keeps `dpkg` state clean.
- **Source / cargo installs** (binary in `~/.cargo/bin`, `~/.local/bin`, or `~/bin`): atomic rename in place. No password needed. The new binary takes effect on the subsequent auto-restart.

Development builds (anywhere else, e.g. `target/release/`) refuse auto-install. Rebuild from git or install via the release `.deb` instead.

#### Trust model

The update path trusts HTTPS and GitHub Releases. SHA256 verification catches bit-rot but does **not** defend against a compromised GitHub account pushing a malicious release. GPG signing is a planned follow-up. If that's not acceptable for your threat model, disable network checks entirely:

```toml
[update]
enabled = false
```

#### Config

```toml
[update]
# Check for new releases on GitHub. Set false to skip ALL network calls.
enabled = true
# How often to recheck while the daemon runs, in hours. 0 = startup only.
# A check is always performed once at daemon launch when enabled.
interval_hours = 6
```

### Auto-start on login

The easiest way to toggle auto-start is the tray checkbox above. From the command line:

```bash
lindiction autostart enable     # start automatically next login
lindiction autostart disable    # stop starting automatically
lindiction autostart status     # print the current state
```

"Auto-start on login" means the daemon starts when you log in to your graphical session — this is a systemd **user** unit (`WantedBy=default.target`), not a system service. The daemon needs your audio session and X display, neither of which exist before you log in.

The subcommand works whether you installed via `.deb` or built from source. On source builds, it writes a generated unit file to `~/.config/systemd/user/lindiction.service` pointing at the current binary, then `systemctl --user daemon-reload && enable`. On `.deb` installs, it uses the system-wide unit at `/lib/systemd/user/lindiction.service` as-is.

Tail the logs:

```bash
journalctl --user -u lindiction -f
```

The manual systemd invocation still works if you prefer it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now lindiction
systemctl --user disable --now lindiction
```

## Configuration

Lindiction reads an optional TOML file at `~/.config/lindiction/config.toml` (or `$XDG_CONFIG_HOME/lindiction/config.toml`). If the file is absent, the built-in defaults apply. Unknown fields are rejected at startup.

Precedence for the model path: `--model` CLI flag > `LINDICTION_MODEL` env var > `[model].path` in TOML > default (`~/.local/share/lindiction/models/ggml-small.en.bin`).

### Full schema with defaults

```toml
[hotkey]
# Hotkey binding: "+"-separated, case-insensitive.
# Modifiers: ctrl (alias: control), alt, shift, super (alias: meta).
# Keys: letters a-z, digits 0-9, space, enter (alias: return), tab,
#       escape (alias: esc), backspace, f1-f24, arrow keys
#       (up, down, left, right).
binding = "ctrl+alt+space"

[audio]
# Milliseconds of mic audio captured *before* the hotkey press to
# prepend to each utterance. Covers human reaction time between
# "start speaking" and "hotkey registered" — without it, the first
# phoneme of most utterances gets clipped. 0 disables.
preroll_ms = 300

[model]
# Path to GGML whisper model file.
path = "~/.local/share/lindiction/models/ggml-small.en.bin"

[stt]
# 1 = greedy (fastest); 5 = beam search (better accuracy, ~1.5-2x slower).
beam_size = 5
# Text primed into the decoder's context before each utterance.
# Use this to bias recognition toward names, jargon, and acronyms
# whisper would otherwise mishear. Empty disables. Keep short
# (~200 chars max).
# Example: "Andre, Claude, lindiction, Rust, tokio, Ubuntu."
initial_prompt = ""

[injection]
# "type" = per-character xdotool typing. Universal (works everywhere)
#          but some X setups silently drop keystrokes — usually spaces
#          — producing merged words like "atesttosee".
# "paste" = put text on clipboard (via xclip) and send paste_shortcut.
#           Atomic, fast, reliable, but clobbers the clipboard.
#           Requires `sudo apt install xclip`.
method = "type"
# Milliseconds between keystrokes when method = "type". Too low (below
# ~10) and the X server silently drops events. Raise to 25-30 on busy
# desktops. Ignored when method = "paste".
xdotool_delay_ms = 15
# Key combo sent when method = "paste". "ctrl+v" works for most GUI apps.
# Terminal emulators (gnome-terminal, konsole, alacritty, etc.) need
# "ctrl+shift+v". "shift+Insert" is a good X11-wide fallback. Passed
# verbatim to `xdotool key` — capitalize keysyms as xdotool expects
# (e.g. "Insert", "Return").
paste_shortcut = "ctrl+v"

[postprocess]
# Remove common filler words before injection (case-insensitive, word-boundary).
remove_fillers = true
filler_words = ["um", "uh", "ah", "like", "you know", "so", "basically"]

# Uppercase the first letter of the utterance and of each sentence
# that follows `. `, `? `, or `! `.
capitalize_sentences = true

# Append a `.` if the final character is not `.`, `?`, or `!`.
ensure_trailing_period = true

# Ordered [from, to] string pairs. Each `from` is matched case-insensitively
# with word boundaries (where possible — edges that aren't word chars, like
# "c++", get a one-sided boundary). Replacement is verbatim — spell the `to`
# exactly how you want it to appear. Runs after sentence-capitalize so your
# casing sticks even at sentence start. Later entries see earlier
# substitutions, so you can chain:
#   replacements = [["cloud", "Claude"], ["Claude code", "Claude Code"]]
replacements = []

[update]
# Check GitHub for new releases and badge the tray icon when found.
enabled = true
# Recheck interval in hours. 0 = startup only (always checks once at launch).
interval_hours = 6
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

**Transcriptions are inaccurate on proper nouns / jargon** — set `[stt].initial_prompt` in your config to a short comma-separated list of names and terms you dictate often (e.g. `"Andre, lindiction, tokio, Ubuntu"`). For further accuracy, upgrade to `ggml-medium.en.bin` (~1.5 GB) via `--model`; the default `small.en` already balances accuracy and latency well.

**First phoneme of each utterance is missing** — your `[audio].preroll_ms` is 0 or too low. Increase to 400–500 ms. This compensates for reaction time between starting to speak and the hotkey registering.

**Words appear glued together with spaces missing** (e.g. "atesttosee" instead of "a test to see") — `xdotool type` is dropping space keystrokes, *not* a whisper problem. Check the daemon log — if the `injecting text=…` line shows correct spaces but the typed output doesn't, first try raising `[injection].xdotool_delay_ms` to 25 or 30. If that still doesn't fix it (some xdotool builds are structurally unreliable), switch to clipboard paste: `sudo apt install xclip` and set `[injection].method = "paste"` in your config. Paste is atomic (one Ctrl+V), unaffected by per-character dropouts — but it clobbers your clipboard and won't work in terminals.

**"curl exited with…" on first run** — the auto-download failed. Check your network, then relaunch. The partial download is automatically cleaned up. As a manual fallback:

```bash
mkdir -p ~/.local/share/lindiction/models
curl -L -o ~/.local/share/lindiction/models/ggml-small.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin
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
LINDICTION_MODEL=~/.local/share/lindiction/models/ggml-small.en.bin cargo test --test integration_stt -- --nocapture
```

## License

MIT.
