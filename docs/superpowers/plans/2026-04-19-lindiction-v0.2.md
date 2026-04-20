# Lindiction v0.2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship lindiction v0.2 — TOML config at `~/.config/lindiction/config.toml`, configurable hotkey binding via string parsing, and a postprocess pipeline (filler removal + sentence capitalization + trailing period) with per-feature toggles all default-on.

**Architecture:** Keep the v0.1 pipeline (cpal → whisper-rs → xdotool) unchanged. The `Config` struct is rewritten into nested sections (`[hotkey]`, `[model]`, `[postprocess]`) that mirror the TOML schema, loaded through `Config::load(cli_model)` with precedence CLI > env > TOML > default. Hotkey binding becomes a string parsed into `(Modifiers, Code)` at startup; `hotkey::start` takes the parsed pair rather than hardcoding. A new `src/postprocess.rs` module sits between the transcribe and inject steps in `app.rs`.

**Tech Stack:** Rust 2021, existing tokio / cpal / whisper-rs / global-hotkey / clap / anyhow / tracing. New deps: `toml`, `serde` (derive), `regex`, `dirs`.

**Spec:** `docs/superpowers/specs/2026-04-19-lindiction-v0.2-design.md`

**Prerequisite:** v0.1 shipped on `main` at tip `c8d6c9b` (after the v0.2 spec commit). Work happens on a new branch `feat/v0.2-impl` branched from `main`.

---

## Task 1: Config rewrite + TOML loading

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/config.rs` (full rewrite)
- Modify: `src/main.rs` (call-site update)
- Modify: `src/app.rs` (call-site update)

This task is larger than typical because the Config shape changes; every call site and every existing test must update together to keep the tree compilable.

- [ ] **Step 1: Switch to implementation branch**

```bash
git checkout -b feat/v0.2-impl
git log --oneline -2
```

Expected: branch created from `main` (tip commit has subject `docs: add v0.2 design spec`).

- [ ] **Step 2: Add dependencies**

Modify `Cargo.toml`'s `[dependencies]` to include:

```toml
dirs = "5"
regex = "1"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
```

Final `[dependencies]` section should look like:

```toml
[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive", "env"] }
cpal = "0.15"
dirs = "5"
global-hotkey = "0.5"
regex = "1"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "signal", "process", "time"] }
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
whisper-rs = "0.11"
which = "6"
```

`[dev-dependencies]` stays as-is (`hound = "3.5"` only).

- [ ] **Step 3: Rewrite `src/config.rs`**

Overwrite `src/config.rs` with:

```rust
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub model: ModelConfig,
    pub postprocess: PostprocessConfig,
    #[serde(skip)]
    pub sample_rate: u32,
    #[serde(skip)]
    pub channels: u16,
    #[serde(skip)]
    pub xdotool_delay_ms: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub binding: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostprocessConfig {
    pub remove_fillers: bool,
    pub filler_words: Vec<String>,
    pub capitalize_sentences: bool,
    pub ensure_trailing_period: bool,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            binding: "ctrl+alt+space".to_string(),
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("models/ggml-tiny.en.bin"),
        }
    }
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            filler_words: [
                "um",
                "uh",
                "ah",
                "like",
                "you know",
                "so",
                "basically",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            capitalize_sentences: true,
            ensure_trailing_period: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: HotkeyConfig::default(),
            model: ModelConfig::default(),
            postprocess: PostprocessConfig::default(),
            sample_rate: 16_000,
            channels: 1,
            xdotool_delay_ms: 5,
        }
    }
}

impl Config {
    /// Load config by merging: defaults → TOML file → env override → CLI override.
    /// `cli_model` is `Some(path)` when the user passed `--model <PATH>`.
    pub fn load(cli_model: Option<PathBuf>) -> Result<Self> {
        let mut config = Self::from_xdg_file()?;

        if let Ok(env_path) = std::env::var("LINDICTION_MODEL") {
            config.model.path = PathBuf::from(env_path);
        }
        if let Some(cli_path) = cli_model {
            config.model.path = cli_path;
        }

        Ok(config)
    }

    fn from_xdg_file() -> Result<Self> {
        let Some(path) = Self::config_path() else {
            warn!("could not resolve XDG config directory; using defaults");
            return Ok(Self::default());
        };
        if !path.exists() {
            debug!(path = %path.display(), "no config file; using defaults");
            return Ok(Self::default());
        }
        info!(path = %path.display(), "loading config");
        let s = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&s).with_context(|| {
            format!(
                "parsing {}. See the Configuration section of the README.",
                path.display()
            )
        })
    }

    /// `~/.config/lindiction/config.toml` (or `$XDG_CONFIG_HOME/lindiction/config.toml`).
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("lindiction").join("config.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All precedence tests touch process-global env vars. Run under
    // `cargo test -- --test-threads=1` to avoid races.

    fn isolate_xdg() {
        // Force XDG_CONFIG_HOME to a path that cannot exist so no user
        // config file leaks into precedence tests.
        std::env::set_var("XDG_CONFIG_HOME", "/nonexistent-lindiction-test");
    }

    #[test]
    fn default_model_path() {
        let c = Config::default();
        assert_eq!(c.model.path, PathBuf::from("models/ggml-tiny.en.bin"));
    }

    #[test]
    fn default_hotkey_binding() {
        let c = Config::default();
        assert_eq!(c.hotkey.binding, "ctrl+alt+space");
    }

    #[test]
    fn default_postprocess_toggles_all_on() {
        let c = Config::default();
        assert!(c.postprocess.remove_fillers);
        assert!(c.postprocess.capitalize_sentences);
        assert!(c.postprocess.ensure_trailing_period);
        assert!(!c.postprocess.filler_words.is_empty());
    }

    #[test]
    fn default_runtime_fields() {
        let c = Config::default();
        assert_eq!(c.sample_rate, 16_000);
        assert_eq!(c.channels, 1);
        assert_eq!(c.xdotool_delay_ms, 5);
    }

    #[test]
    fn parses_full_toml() {
        let s = r#"
[hotkey]
binding = "f12"

[model]
path = "/tmp/custom.bin"

[postprocess]
remove_fillers = false
filler_words = ["hmm"]
capitalize_sentences = false
ensure_trailing_period = false
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(c.hotkey.binding, "f12");
        assert_eq!(c.model.path, PathBuf::from("/tmp/custom.bin"));
        assert!(!c.postprocess.remove_fillers);
        assert_eq!(c.postprocess.filler_words, vec!["hmm".to_string()]);
        assert!(!c.postprocess.capitalize_sentences);
        assert!(!c.postprocess.ensure_trailing_period);
    }

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

    #[test]
    fn unknown_field_is_rejected() {
        let s = r#"
[hotkey]
binding = "ctrl+alt+space"
nonsense = true
"#;
        let err = toml::from_str::<Config>(s).expect_err("should reject unknown field");
        let msg = format!("{}", err);
        assert!(
            msg.contains("nonsense") || msg.contains("unknown field"),
            "error should mention the unknown field; got: {msg}"
        );
    }

    #[test]
    fn load_with_no_config_file_uses_defaults() {
        isolate_xdg();
        std::env::remove_var("LINDICTION_MODEL");
        let c = Config::load(None).expect("load");
        assert_eq!(c.model.path, PathBuf::from("models/ggml-tiny.en.bin"));
        assert_eq!(c.hotkey.binding, "ctrl+alt+space");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn env_model_beats_default() {
        isolate_xdg();
        std::env::set_var("LINDICTION_MODEL", "/from/env.bin");
        let c = Config::load(None).expect("load");
        assert_eq!(c.model.path, PathBuf::from("/from/env.bin"));
        std::env::remove_var("LINDICTION_MODEL");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn cli_model_beats_env() {
        isolate_xdg();
        std::env::set_var("LINDICTION_MODEL", "/from/env.bin");
        let c = Config::load(Some(PathBuf::from("/from/cli.bin"))).expect("load");
        assert_eq!(c.model.path, PathBuf::from("/from/cli.bin"));
        std::env::remove_var("LINDICTION_MODEL");
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
```

- [ ] **Step 4: Update `src/main.rs`**

The CLI already exposes `cli.model: Option<PathBuf>`. Change the call site from the v0.1 pattern `Config::load().with_model_path(m)` to `Config::load(cli.model)?`. Overwrite `src/main.rs`:

```rust
use anyhow::Result;
use clap::Parser;
use lindiction::app::App;
use lindiction::config::Config;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Lindiction — push-to-talk voice dictation for Linux.
///
/// Hold Ctrl+Alt+Space (or your configured binding) to record. Release to
/// transcribe and inject the text at the cursor.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to GGML whisper model file (overrides TOML config and env var)
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

    let config = Config::load(cli.model)?;

    App::run(config).await
}
```

Note: clap's `#[arg(long, env = "LINDICTION_MODEL")]` gives CLI and env both a crack at populating `cli.model`. By the time `Config::load(cli.model)` runs, the `env` override has already been applied by clap — so the precedence inside `Config::load` is subtly different from the spec's wording. For the user experience it's the same (CLI beats env beats TOML), but the TEST for env-beats-default would fail if clap injected the env var through this path. Because the unit tests call `Config::load(None)` directly (not via clap), they remain valid. Leave this dual-path arrangement alone — it mirrors v0.1 and is what the v0.1 test already proved.

- [ ] **Step 5: Update `src/app.rs`**

The v0.1 `App::run` reads `config.model_path`. Change every use to `config.model.path`. Specifically, in `App::run`:

Find this line:
```rust
        let stt = Arc::new(
            SttEngine::load(&config.model_path)
                .with_context(|| format!("loading model from {}", config.model_path.display()))?,
        );
```

Replace with:
```rust
        let stt = Arc::new(
            SttEngine::load(&config.model.path)
                .with_context(|| format!("loading model from {}", config.model.path.display()))?,
        );
```

No other lines in `app.rs` reference `config.model_path`.

- [ ] **Step 6: Run all tests**

```bash
cargo test --lib -- --test-threads=1
```

Expected output:
- Config tests all pass (10 tests: default_model_path, default_hotkey_binding, default_postprocess_toggles_all_on, default_runtime_fields, parses_full_toml, partial_toml_fills_from_default, unknown_field_is_rejected, load_with_no_config_file_uses_defaults, env_model_beats_default, cli_model_beats_env).
- Inject tests all pass (5).
- Stt tests all pass (2).
- Hotkey module has no tests yet.
- Total: 17 passed.

If any test fails, fix before committing. Do NOT amend a bad commit later — fix now.

- [ ] **Step 7: Build release + gated integration test**

```bash
cargo build --release
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

Expected: both green. Integration test still passes because whisper pipeline is unchanged.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/config.rs src/main.rs src/app.rs
git commit -m "feat(config): TOML config with precedence CLI > env > TOML > default"
```

---

## Task 2: Hotkey string parser

**Files:**
- Modify: `src/hotkey.rs` (add `parse_binding` and helper fns; no change to `start` yet)

- [ ] **Step 1: Write failing unit tests**

Append to the end of `src/hotkey.rs` (above the existing `#[cfg(test)]` block if any — `hotkey.rs` had no tests in v0.1, so add a new `#[cfg(test)] mod tests` block at the very end of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::{Code, Modifiers};

    #[test]
    fn parse_canonical_ctrl_alt_space() {
        let (mods, code) = parse_binding("ctrl+alt+space").expect("parse");
        assert_eq!(mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(code, Code::Space);
    }

    #[test]
    fn parse_is_case_insensitive() {
        let (mods, code) = parse_binding("CTRL+Alt+SPACE").expect("parse");
        assert_eq!(mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(code, Code::Space);
    }

    #[test]
    fn parse_single_fn_key_no_modifiers() {
        let (mods, code) = parse_binding("f12").expect("parse");
        assert_eq!(mods, Modifiers::empty());
        assert_eq!(code, Code::F12);
    }

    #[test]
    fn parse_meta_alias_for_super() {
        let (mods, code) = parse_binding("meta+k").expect("parse");
        assert_eq!(mods, Modifiers::META);
        assert_eq!(code, Code::KeyK);
    }

    #[test]
    fn parse_digit_and_arrow() {
        let (_, code) = parse_binding("ctrl+7").expect("parse");
        assert_eq!(code, Code::Digit7);
        let (_, code) = parse_binding("alt+right").expect("parse");
        assert_eq!(code, Code::ArrowRight);
    }

    #[test]
    fn parse_unknown_modifier_errors() {
        let err = parse_binding("nope+space").expect_err("should fail");
        let msg = format!("{err}");
        assert!(msg.to_lowercase().contains("modifier"), "msg was: {msg}");
    }

    #[test]
    fn parse_unknown_key_errors() {
        let err = parse_binding("ctrl+nonsense").expect_err("should fail");
        let msg = format!("{err}");
        assert!(msg.to_lowercase().contains("key"), "msg was: {msg}");
    }

    #[test]
    fn parse_empty_string_errors() {
        assert!(parse_binding("").is_err());
    }
}
```

- [ ] **Step 2: Verify tests fail**

```bash
cargo test --lib hotkey::tests:: 2>&1 | tail -20
```

Expected: compile error "cannot find function `parse_binding` in this scope".

- [ ] **Step 3: Implement `parse_binding`**

Insert the following at the end of `src/hotkey.rs`, immediately BEFORE the `#[cfg(test)] mod tests { ... }` block:

```rust
/// Parse a binding string like `"ctrl+alt+space"` into `(Modifiers, Code)`
/// for `global_hotkey::hotkey::HotKey::new`. Tokens are `+`-separated;
/// the last token is the key, earlier tokens are modifiers. Case-insensitive.
pub fn parse_binding(s: &str) -> Result<(Modifiers, Code)> {
    let tokens: Vec<&str> = s.split('+').map(str::trim).collect();
    if tokens.is_empty() || tokens.iter().any(|t| t.is_empty()) {
        anyhow::bail!("empty hotkey binding or empty `+`-separated token in `{s}`");
    }
    let (key_token, mod_tokens) = tokens.split_last().expect("non-empty verified above");
    let mut modifiers = Modifiers::empty();
    for m in mod_tokens {
        modifiers |= parse_modifier_token(&m.to_lowercase())?;
    }
    let code = parse_key_token(&key_token.to_lowercase())?;
    Ok((modifiers, code))
}

fn parse_modifier_token(s: &str) -> Result<Modifiers> {
    match s {
        "ctrl" | "control" => Ok(Modifiers::CONTROL),
        "alt" => Ok(Modifiers::ALT),
        "shift" => Ok(Modifiers::SHIFT),
        "super" | "meta" => Ok(Modifiers::META),
        _ => anyhow::bail!(
            "Unknown hotkey modifier `{s}`. Valid modifiers: ctrl, alt, shift, super (alias: meta)."
        ),
    }
}

fn parse_key_token(s: &str) -> Result<Code> {
    const LETTERS: [Code; 26] = [
        Code::KeyA, Code::KeyB, Code::KeyC, Code::KeyD, Code::KeyE, Code::KeyF,
        Code::KeyG, Code::KeyH, Code::KeyI, Code::KeyJ, Code::KeyK, Code::KeyL,
        Code::KeyM, Code::KeyN, Code::KeyO, Code::KeyP, Code::KeyQ, Code::KeyR,
        Code::KeyS, Code::KeyT, Code::KeyU, Code::KeyV, Code::KeyW, Code::KeyX,
        Code::KeyY, Code::KeyZ,
    ];
    const DIGITS: [Code; 10] = [
        Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4,
        Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9,
    ];
    const FKEYS: [Code; 24] = [
        Code::F1,  Code::F2,  Code::F3,  Code::F4,  Code::F5,  Code::F6,
        Code::F7,  Code::F8,  Code::F9,  Code::F10, Code::F11, Code::F12,
        Code::F13, Code::F14, Code::F15, Code::F16, Code::F17, Code::F18,
        Code::F19, Code::F20, Code::F21, Code::F22, Code::F23, Code::F24,
    ];

    // Single-character letters and digits
    if s.len() == 1 {
        let c = s.chars().next().unwrap();
        if c.is_ascii_lowercase() {
            return Ok(LETTERS[(c as u8 - b'a') as usize]);
        }
        if c.is_ascii_digit() {
            return Ok(DIGITS[(c as u8 - b'0') as usize]);
        }
    }

    // F-keys: "f1".."f24"
    if let Some(n_str) = s.strip_prefix('f') {
        if let Ok(n) = n_str.parse::<usize>() {
            if (1..=24).contains(&n) {
                return Ok(FKEYS[n - 1]);
            }
        }
    }

    match s {
        "space" => Ok(Code::Space),
        "enter" | "return" => Ok(Code::Enter),
        "tab" => Ok(Code::Tab),
        "escape" | "esc" => Ok(Code::Escape),
        "backspace" => Ok(Code::Backspace),
        "up" => Ok(Code::ArrowUp),
        "down" => Ok(Code::ArrowDown),
        "left" => Ok(Code::ArrowLeft),
        "right" => Ok(Code::ArrowRight),
        _ => anyhow::bail!(
            "Unknown hotkey key `{s}`. Valid keys: letters a-z, digits 0-9, space, enter, \
             tab, escape, backspace, f1-f24, arrow keys (up, down, left, right)."
        ),
    }
}
```

- [ ] **Step 4: Verify tests pass**

```bash
cargo test --lib hotkey::tests:: 2>&1 | tail -15
```

Expected: 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/hotkey.rs
git commit -m "feat(hotkey): parse_binding converts string like 'ctrl+alt+space' to (Modifiers, Code)"
```

---

## Task 3: Wire configurable hotkey through

**Files:**
- Modify: `src/hotkey.rs` (change `start` signature)
- Modify: `src/app.rs` (parse binding from config, pass to `start`)

- [ ] **Step 1: Change `start`'s signature**

In `src/hotkey.rs`, find the current `pub fn start()` definition (returns `Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)>` and constructs `HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space)` internally).

Replace the function body's first three lines — which currently look like:

```rust
pub fn start() -> Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)> {
    let manager = GlobalHotKeyManager::new().context("creating GlobalHotKeyManager")?;
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
```

With:

```rust
pub fn start(
    modifiers: Modifiers,
    code: Code,
) -> Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)> {
    let manager = GlobalHotKeyManager::new().context("creating GlobalHotKeyManager")?;
    let hotkey = HotKey::new(Some(modifiers), Code::from(code));
```

Wait — `HotKey::new` already takes `Code` directly. Simpler form; replace with:

```rust
pub fn start(
    modifiers: Modifiers,
    code: Code,
) -> Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)> {
    let manager = GlobalHotKeyManager::new().context("creating GlobalHotKeyManager")?;
    let hotkey = HotKey::new(Some(modifiers), code);
```

Also update the `info!(hotkey_id, "registered Ctrl+Alt+Space");` line: replace with:

```rust
    info!(hotkey_id, ?modifiers, ?code, "registered hotkey");
```

Rest of the function body is unchanged.

- [ ] **Step 2: Update `src/app.rs` to parse and pass the binding**

In `src/app.rs`, find this line:

```rust
use crate::hotkey::{start as start_hotkey, HotkeyEvent, HotkeyListener};
```

Extend it to also bring in `parse_binding`:

```rust
use crate::hotkey::{parse_binding, start as start_hotkey, HotkeyEvent, HotkeyListener};
```

Then find the line:

```rust
        let (_hotkey_listener, mut hotkey_rx): (HotkeyListener, _) = start_hotkey()?;
```

Replace with:

```rust
        let (modifiers, code) = parse_binding(&config.hotkey.binding).with_context(|| {
            format!(
                "parsing hotkey binding `{}` from config",
                config.hotkey.binding
            )
        })?;
        let (_hotkey_listener, mut hotkey_rx): (HotkeyListener, _) = start_hotkey(modifiers, code)?;
```

- [ ] **Step 3: Update the ready-message**

Below the hotkey startup, find:

```rust
        info!("ready — hold Ctrl+Alt+Space to dictate");
```

Replace with:

```rust
        info!(hotkey = %config.hotkey.binding, "ready — hold the hotkey to dictate");
```

- [ ] **Step 4: Build**

```bash
cargo build --release
```

Expected: clean compile.

- [ ] **Step 5: Run full unit tests**

```bash
cargo test --lib -- --test-threads=1
```

Expected: 25 passed (10 config + 5 inject + 2 stt + 8 hotkey). If any test fails, fix before committing.

- [ ] **Step 6: Manual smoke — default binding still works**

Record that you will test this in Task 8 (manual E2E). For this commit, no daemon run is required — the unit tests cover parse correctness and the code path is identical in shape to v0.1.

- [ ] **Step 7: Commit**

```bash
git add src/hotkey.rs src/app.rs
git commit -m "feat(hotkey): route config.hotkey.binding through parse_binding into start()"
```

---

## Task 4: Postprocess module

**Files:**
- Create: `src/postprocess.rs`
- Modify: `src/lib.rs` (add `pub mod postprocess;`)

- [ ] **Step 1: Declare the module**

In `src/lib.rs`, add `pub mod postprocess;` in alphabetical order. Final contents:

```rust
pub mod app;
pub mod audio;
pub mod config;
pub mod hotkey;
pub mod inject;
pub mod postprocess;
pub mod stt;
```

- [ ] **Step 2: Write failing tests**

Create `src/postprocess.rs` with tests at the bottom. Full file contents (we'll stub the impl for now, fill it in Step 4):

```rust
use crate::config::PostprocessConfig;
use anyhow::{Context, Result};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct Postprocessor {
    filler_regex: Option<Regex>,
    collapse_whitespace: Regex,
    capitalize_sentences: bool,
    ensure_trailing_period: bool,
}

impl Postprocessor {
    pub fn new(cfg: &PostprocessConfig) -> Result<Self> {
        unimplemented!()
    }

    pub fn apply(&self, text: &str) -> String {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PostprocessConfig;

    fn default_cfg() -> PostprocessConfig {
        PostprocessConfig::default()
    }

    fn raw_cfg() -> PostprocessConfig {
        PostprocessConfig {
            remove_fillers: false,
            filler_words: vec![],
            capitalize_sentences: false,
            ensure_trailing_period: false,
        }
    }

    #[test]
    fn trims_whitespace_and_adds_period() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("  hello  "), "Hello.");
    }

    #[test]
    fn removes_fillers() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("um hello uh world"), "Hello world.");
    }

    #[test]
    fn capitalizes_first_letter() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello"), "Hello.");
    }

    #[test]
    fn capitalizes_after_sentence_punctuation() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello. world"), "Hello. World.");
    }

    #[test]
    fn idempotent_trailing_period() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello."), "Hello.");
    }

    #[test]
    fn preserves_question_and_exclamation_endings() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello?"), "Hello?");
        assert_eq!(p.apply("hello!"), "Hello!");
    }

    #[test]
    fn all_toggles_off_returns_trimmed_input() {
        let p = Postprocessor::new(&raw_cfg()).unwrap();
        assert_eq!(p.apply("  um hello  "), "um hello");
    }

    #[test]
    fn all_filler_input_returns_empty() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("um uh"), "");
    }

    #[test]
    fn preserves_all_caps_mid_sentence() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("HELLO WORLD"), "HELLO WORLD.");
    }

    #[test]
    fn filler_removal_is_case_insensitive() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("UM hello UH world"), "Hello world.");
    }

    #[test]
    fn filler_removal_respects_word_boundary() {
        // "umbrella" starts with "um" but shouldn't be stripped.
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("umbrella"), "Umbrella.");
    }

    #[test]
    fn multi_word_filler_is_removed() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("you know hello"), "Hello.");
    }
}
```

- [ ] **Step 3: Verify tests compile and fail**

```bash
cargo test --lib postprocess:: 2>&1 | tail -20
```

Expected: tests compile but panic at `unimplemented!()` — all 12 fail.

- [ ] **Step 4: Implement `Postprocessor`**

Replace the stub `impl Postprocessor` block with:

```rust
impl Postprocessor {
    pub fn new(cfg: &PostprocessConfig) -> Result<Self> {
        let filler_regex = if cfg.remove_fillers && !cfg.filler_words.is_empty() {
            let escaped: Vec<String> =
                cfg.filler_words.iter().map(|w| regex::escape(w)).collect();
            let pattern = format!("(?i)\\b({})\\b", escaped.join("|"));
            Some(Regex::new(&pattern).context("compiling filler-words regex")?)
        } else {
            None
        };
        let collapse_whitespace = Regex::new(r"\s+").expect("static regex compiles");
        Ok(Self {
            filler_regex,
            collapse_whitespace,
            capitalize_sentences: cfg.capitalize_sentences,
            ensure_trailing_period: cfg.ensure_trailing_period,
        })
    }

    pub fn apply(&self, text: &str) -> String {
        // 1. Trim.
        let mut s = text.trim().to_string();

        // 2. Remove filler words if configured.
        if let Some(re) = &self.filler_regex {
            s = re.replace_all(&s, "").to_string();
        }

        // 3. Collapse runs of whitespace into single spaces and re-trim.
        s = self
            .collapse_whitespace
            .replace_all(&s, " ")
            .trim()
            .to_string();

        // 4. Capitalize sentence-initial letters.
        if self.capitalize_sentences {
            s = capitalize_sentences(&s);
        }

        // 5. Append trailing period if none of `. ? !` terminate.
        if self.ensure_trailing_period && !s.is_empty() {
            let last = s.chars().last().expect("non-empty");
            if !matches!(last, '.' | '?' | '!') {
                s.push('.');
            }
        }

        s
    }
}

fn capitalize_sentences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if capitalize_next && c.is_ascii_alphabetic() {
            out.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '?' | '!') {
                capitalize_next = true;
            } else if !c.is_whitespace() {
                capitalize_next = false;
            }
        }
    }
    out
}
```

Note: remove the `unimplemented!()` panics by replacing the entire `impl Postprocessor` block.

- [ ] **Step 5: Verify tests pass**

```bash
cargo test --lib postprocess:: 2>&1 | tail -20
```

Expected: 12 tests pass. If any test fails, trace through the logic — the test case is authoritative, not the impl.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/postprocess.rs
git commit -m "feat(postprocess): Postprocessor with filler removal, capitalization, trailing period"
```

---

## Task 5: Wire postprocess into the transcription worker

**Files:**
- Modify: `src/app.rs` (insert postprocess step between transcribe and inject)

- [ ] **Step 1: Update imports**

In `src/app.rs`, find:

```rust
use crate::inject::Injector;
use crate::stt::SttEngine;
```

Add beneath:

```rust
use crate::postprocess::Postprocessor;
```

- [ ] **Step 2: Build `Postprocessor` in `App::run` and clone into the worker**

After the existing `let injector = Injector::new(config.xdotool_delay_ms);` line, add:

```rust
        let postprocessor = Postprocessor::new(&config.postprocess)
            .context("building postprocessor from config.postprocess")?;
```

Then, inside the `let worker = { ... }` closure-binding block, find:

```rust
        let worker = {
            let injector_worker = injector.clone();
            let stt_worker = Arc::clone(&stt);
```

Extend to clone the postprocessor:

```rust
        let worker = {
            let injector_worker = injector.clone();
            let stt_worker = Arc::clone(&stt);
            let postprocessor_worker = postprocessor.clone();
```

- [ ] **Step 3: Insert the postprocess step**

Inside the worker's transcription loop, after the existing match that binds `text` from `spawn_blocking`:

```rust
                    if text.is_empty() {
                        debug!("empty transcription, nothing to inject");
                        continue;
                    }
                    info!(text = %text, "injecting");
                    if let Err(e) = injector_worker.inject(&text).await {
```

Replace the block from `if text.is_empty()` down to (but not including) the `if let Err(e) = injector_worker.inject(&text).await {` line:

```rust
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
```

The only substantive changes are the new `let clean = ...` and the empty-after-postprocess check, and swapping `&text` for `&clean` in the inject call and the info log.

- [ ] **Step 4: Build + unit tests**

```bash
cargo build --release
cargo test --lib -- --test-threads=1
```

Expected: compile clean, all 37 tests pass (10 config + 5 inject + 2 stt + 8 hotkey + 12 postprocess).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): insert postprocessor between transcribe and inject"
```

---

## Task 6: README Configuration section

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add the Configuration section**

In `README.md`, between the `## Run` section's `Flags` subsection and the `## Troubleshooting` section, insert a new `## Configuration` section:

Find this line in README.md:

```markdown
## Troubleshooting
```

Replace with:

```markdown
## Configuration

Lindiction reads an optional TOML file at `~/.config/lindiction/config.toml` (or `$XDG_CONFIG_HOME/lindiction/config.toml`). If the file is absent, the built-in defaults apply. Unknown fields are rejected at startup.

Precedence for the model path: `--model` CLI flag > `LINDICTION_MODEL` env var > `[model].path` in TOML > default (`models/ggml-tiny.en.bin`).

### Full schema with defaults

```toml
[hotkey]
# Hotkey binding: "+"-separated, case-insensitive.
# Modifiers: ctrl, alt, shift, super (alias: meta).
# Keys: letters a-z, digits 0-9, space, enter, tab, escape, backspace,
#       f1-f24, arrow keys (up, down, left, right).
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

## Troubleshooting
```

Leave the existing Troubleshooting content unchanged below.

- [ ] **Step 2: Add a "Configuration error" troubleshooting entry**

Within the `## Troubleshooting` section, after the existing `**"Hotkey registration failed"** — ...` paragraph, insert:

```markdown
**"Config parse error" / "Unknown config field"** — the TOML file at `~/.config/lindiction/config.toml` has a syntax error or uses a field name that is not part of the current schema. Check it against the schema in the Configuration section above, or delete the file to fall back to defaults.

**"Invalid hotkey binding"** — the `[hotkey] binding` value could not be parsed. Valid modifiers are `ctrl`, `alt`, `shift`, `super` (alias `meta`). Valid keys are letters `a`–`z`, digits `0`–`9`, `space`, `enter`, `tab`, `escape`, `backspace`, `f1`–`f24`, and arrow keys (`up`, `down`, `left`, `right`). Example bindings: `"ctrl+alt+space"`, `"f12"`, `"super+shift+d"`.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add Configuration section and two new troubleshooting entries"
```

---

## Task 7: Acceptance + tag v0.2.0

**Files:** none modified. Manual verification + release.

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --lib -- --test-threads=1
LINDICTION_MODEL=models/ggml-tiny.en.bin cargo test --test integration_stt -- --nocapture
```

Expected: 37 unit tests green + 1 integration test green.

- [ ] **Step 2: Run lint checks (same as CI)**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --no-deps -- -D warnings
```

Expected: both green. If clippy surfaces new warnings, fix them in a separate commit subject `style: address new v0.2 clippy warnings` before continuing.

- [ ] **Step 3: Manual test 1 — default hotkey and default postprocess**

Build release: `cargo build --release`. Launch: `LINDICTION_MODEL=models/ggml-tiny.en.bin ./target/release/lindiction -v`.

Focus a text editor. Hold Ctrl+Alt+Space, say "um hello world", release.

Expected: text typed at cursor is `Hello world.` (filler removed, capitalized, trailing period).

- [ ] **Step 4: Manual test 2 — custom hotkey via config**

Kill the daemon. Write `~/.config/lindiction/config.toml`:

```toml
[hotkey]
binding = "ctrl+alt+d"
```

Re-launch. Daemon log should show `hotkey="ctrl+alt+d"` in the "ready" line.

Verify Ctrl+Alt+Space does NOTHING. Verify Ctrl+Alt+D triggers a recording.

- [ ] **Step 5: Manual test 3 — raw-output opt-out**

Kill the daemon. Overwrite `~/.config/lindiction/config.toml`:

```toml
[postprocess]
remove_fillers = false
capitalize_sentences = false
ensure_trailing_period = false
```

Re-launch. Record "um hello world".

Expected: text is `um hello world` (or whatever raw whisper returns, without transforms).

- [ ] **Step 6: Manual test 4 — malformed TOML surfaces error**

Kill the daemon. Overwrite the config file with:

```toml
[hotkey
binding = "ctrl+alt+space"
```

(Missing `]` on the section header.)

Launch the daemon. Expected: process exits with a stderr message containing `~/.config/lindiction/config.toml`, a line/column, and a pointer to the README.

- [ ] **Step 7: Manual test 5 — unknown field**

Overwrite config:

```toml
[hotkey]
binding = "ctrl+alt+space"
nonsense = true
```

Launch. Expected: process exits with an error mentioning `nonsense` and the `deny_unknown_fields` rejection.

- [ ] **Step 8: Clean up the test config**

```bash
rm -f ~/.config/lindiction/config.toml
```

- [ ] **Step 9: Merge to main**

```bash
git checkout main
git merge --ff-only feat/v0.2-impl
git log --oneline -10
```

Expected: 6 new commits from v0.2 fast-forward onto main.

- [ ] **Step 10: Tag v0.2.0 and push**

```bash
git tag -a v0.2.0 -m "v0.2.0: TOML config, configurable hotkey, postprocess pipeline

Adds optional ~/.config/lindiction/config.toml with three sections
(hotkey, model, postprocess). Hotkey binding is configurable via
string parsing ('ctrl+alt+space', 'f12', etc.). Postprocess removes
filler words, capitalizes sentence-initial letters, and ensures a
trailing period — all default-on, each individually toggleable."

git push origin main
git push origin v0.2.0
```

Expected: main pushes cleanly; tag push triggers the release workflow.

- [ ] **Step 11: Verify release workflow runs**

```bash
sleep 5
gh run list --repo cortexuvula/lindiction --limit 2
```

Wait for both workflows to complete:

```bash
until gh run list --repo cortexuvula/lindiction --limit 2 --json status -q '.[] | .status' | grep -qvE '^completed$'; do
  sleep 30
done
gh run list --repo cortexuvula/lindiction --limit 2
gh release view v0.2.0 --repo cortexuvula/lindiction
```

Expected: both CI and Release workflows complete successfully. The release has `lindiction-v0.2.0-x86_64-linux.tar.gz` and its `.sha256` attached.

- [ ] **Step 12: Delete the feature branch (local + remote if pushed)**

```bash
git branch -d feat/v0.2-impl
# the branch was never pushed to origin (we pushed main directly), so nothing to delete remotely
git branch --list
```

Expected: `feat/v0.2-impl` is gone; only `main` remains.

**Plan complete.** v0.2.0 is live on GitHub with an attached release binary.
