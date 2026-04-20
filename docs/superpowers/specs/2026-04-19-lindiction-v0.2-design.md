# Lindiction v0.2 — Design

**Date:** 2026-04-19
**Status:** Approved, pre-implementation
**Supersedes for v0.2:** nothing; this layers on top of the shipped v0.1 MVP.
**Predecessor:** `2026-04-18-lindiction-mvp-design.md` (v0.1, tagged `v0.1.0-mvp`)

## Overview

Lindiction v0.2 adds three user-facing improvements to the shipped MVP: a TOML configuration file, a configurable hotkey binding, and a post-processing pipeline (filler-word removal, sentence capitalization, trailing-period enforcement). The dictation pipeline itself (cpal → whisper-rs → xdotool) is unchanged. Target platform remains Ubuntu 24.04 / X11 / GNOME; cross-platform work is still v0.3+.

The release is scoped deliberately narrow. Every other item from the IMPLEMENTATION_PLAN backlog (Wayland, system tray, VAD endpointing, model auto-download, IPC/CLI split, `.deb` + systemd) stays deferred. Attempting any of those in v0.2 would require infrastructure the config+postprocess work does not need.

## Scope

### In scope (v0.2)

- TOML config file at `~/.config/lindiction/config.toml` with three sections: `[hotkey]`, `[model]`, `[postprocess]`. Config file is optional — absence falls back to defaults silently.
- Hotkey binding parsed from a config string like `"ctrl+alt+space"` at startup.
- Post-processing pipeline with individual toggles, all default-on: filler removal, sentence capitalization, trailing period enforcement.
- Four-level precedence for the model path: CLI `--model` > env `LINDICTION_MODEL` > TOML `[model] path` > hardcoded default.
- New unit tests covering config TOML parsing, hotkey string parsing, and each postprocess transform.
- README `Configuration` section documenting the schema.

### Explicit non-goals (still deferred to v0.3+)

- Wayland support, including `wtype` and `ydotool` injection fallback chain and any Wayland-compatible hotkey path.
- System tray indicator.
- VAD endpointing (state machine) for toggle mode.
- Automatic model download.
- IPC Unix socket + separate CLI client.
- `.deb` packaging + systemd user service.
- CLI flag for hotkey binding (config file is the only surface for v0.2).
- Streaming transcription.

### Behavioral change from v0.1

Users who dogfooded v0.1 get their output transformed by default starting v0.2: capitalized sentences, trailing period, filler words removed. Users who preferred the raw whisper output must explicitly opt out by writing a config file with the three postprocess toggles set to `false`. The README will call this out. There are no compile-time breaking changes; every existing v0.1 invocation still works without config.

## Architecture

### Repo-layout additions

```
src/postprocess.rs                          # new module
src/config.rs                               # expanded: Config, HotkeyConfig, ModelConfig, PostprocessConfig + TOML loading
src/hotkey.rs                               # extended with parse_binding(&str) -> Result<(Modifiers, Code)>
docs/superpowers/specs/2026-04-19-lindiction-v0.2-design.md  # this file
```

New dependencies:

- `toml = "0.8"` — TOML parsing.
- `serde = { version = "1", features = ["derive"] }` — derive `Deserialize` for the config structs.
- `regex = "1"` — filler-word regex in postprocess.
- `dirs = "5"` — XDG config directory lookup (`~/.config/lindiction/`).

### Config loading

`Config::load` takes an optional CLI model override and returns a fully-resolved `Config`:

```rust
pub fn load(cli_model: Option<PathBuf>) -> Result<Config>
```

Steps, in order:

1. Start with `Config::default()` (in-code defaults for every field).
2. If `~/.config/lindiction/config.toml` exists, read and parse it. On parse error, exit with a message that includes the file path, the offending line and column from `toml::de::Error`, and a pointer to the README's `Configuration` section.
3. If `LINDICTION_MODEL` env var is set, override `config.model.path`.
4. If `cli_model.is_some()`, override `config.model.path`.

Precedence ends up as CLI > env > TOML > default. Only `model.path` has a CLI/env path; all other fields are TOML-only for v0.2.

### Config struct shape

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub model: ModelConfig,
    pub postprocess: PostprocessConfig,
    #[serde(skip)] pub sample_rate: u32,
    #[serde(skip)] pub channels: u16,
    #[serde(skip)] pub xdotool_delay_ms: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub binding: String,   // default "ctrl+alt+space"
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub path: PathBuf,     // default "models/ggml-tiny.en.bin"
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostprocessConfig {
    pub remove_fillers: bool,         // default true
    pub filler_words: Vec<String>,    // default ["um", "uh", "ah", "like", "you know", "so", "basically"]
    pub capitalize_sentences: bool,   // default true
    pub ensure_trailing_period: bool, // default true
}
```

The runtime-only fields (`sample_rate`, `channels`, `xdotool_delay_ms`) stay hardcoded at 16 000 / 1 / 5 in `Default::default()` and are skipped during TOML deserialization. Moving them into TOML is v0.3 work if the underlying feature changes (e.g. configurable sample rate for different whisper models). `deny_unknown_fields` on every struct means typos in the TOML file produce a clear startup error rather than silently ignoring the user's intent.

### Hotkey string parser

Lives in `src/hotkey.rs` as:

```rust
pub fn parse_binding(s: &str) -> anyhow::Result<(Modifiers, Code)>
```

Algorithm:

1. Split on `+`.
2. Trim and lowercase each token.
3. Last token is the key; earlier tokens are modifiers.
4. Map modifiers to `global_hotkey::hotkey::Modifiers`: `ctrl` → `CONTROL`, `alt` → `ALT`, `shift` → `SHIFT`, `super` or `meta` → `META`. Unknown modifier → bail with a message listing the four valid modifier names.
5. Map key to `global_hotkey::hotkey::Code`. Supported keys (minimum set for v0.2):
   - Letters `a`–`z` → `KeyA`–`KeyZ`.
   - Digits `0`–`9` → `Digit0`–`Digit9`.
   - `space` → `Space`, `enter` → `Enter`, `tab` → `Tab`, `escape` → `Escape`, `backspace` → `Backspace`.
   - `f1`–`f24` → `F1`–`F24`.
   - `up`, `down`, `left`, `right` → arrow keys.
6. Unknown key → bail with a message listing the supported categories and a pointer to the README.

`start()` in `hotkey.rs` changes its signature to accept the parsed `(Modifiers, Code)` rather than hardcoding the binding. The singleton-receiver constraint on the `global-hotkey` crate (one call per process) persists from v0.1.

### Postprocess module

New file `src/postprocess.rs`:

```rust
#[derive(Debug, Clone)]
pub struct Postprocessor {
    filler_regex: Option<Regex>,       // None if remove_fillers is false
    capitalize_sentences: bool,
    ensure_trailing_period: bool,
}

impl Postprocessor {
    pub fn new(cfg: &PostprocessConfig) -> anyhow::Result<Self>;
    pub fn apply(&self, text: &str) -> String;
}
```

Construction compiles the filler regex exactly once. The pattern is `(?i)\b(word1|word2|…)\b`, where each word is `regex::escape`'d. If the user supplies an empty filler list with `remove_fillers = true`, the regex is `None` (nothing to match) and the removal step is a no-op.

`apply` pipeline, in order:

1. Trim leading/trailing whitespace.
2. If `filler_regex.is_some()`, regex-replace all matches with empty string.
3. Collapse runs of whitespace (space/tab/newline) into single spaces.
4. If `capitalize_sentences`, walk char-by-char: uppercase the first letter of the string, and uppercase the first ASCII letter following any `.`, `?`, or `!` plus whitespace. Non-ASCII-alphabetic first characters pass through untouched (Unicode-capitalization is a surprising rabbit hole for MVP scope).
5. If `ensure_trailing_period` and the result does not already end in `.`, `?`, or `!`, append `.`.
6. Re-trim — steps 2 and 4 can leave a leading space if the first token was a filler.

`apply` never fails. Empty output after processing (e.g. an utterance that was 100% filler words) is returned as empty — the existing empty-string guard in `app.rs` drops it before injection.

### Wire-up in `app.rs`

One insertion inside the transcription worker, between `spawn_blocking(transcribe)` and `inject`. The worker already holds a `Postprocessor` built once in `App::run` from `config.postprocess` and cloned into the closure alongside the injector:

```rust
let raw = ... /* whisper output */;
let clean = postprocessor.apply(&raw);
if clean.trim().is_empty() {
    debug!("empty after postprocess");
    continue;
}
info!(text = %clean, "injecting");
injector_worker.inject(&clean).await?;
```

The existing `[BLANK_AUDIO]` filter in `stt.rs` still runs and returns empty, which postprocess passes through unchanged.

## Error handling

All error paths bail at startup with actionable messages. Nothing in v0.2 introduces new runtime-error classes.

| Failure | User sees | Exit code |
|---|---|---|
| `~/.config/lindiction/config.toml` exists but is not valid TOML | "Config parse error at `~/.config/lindiction/config.toml:<line>:<col>`: <message>. See the Configuration section of the README." | 1 |
| TOML contains an unknown field | "Unknown config field `<name>` in `<section>`. See the Configuration section of the README for the current schema." | 1 |
| `hotkey.binding` cannot be parsed | "Invalid hotkey binding `<value>`: <reason>. Valid modifiers: ctrl, alt, shift, super. Valid keys: letters, digits, space, enter, tab, escape, backspace, f1–f24, arrow keys." | 1 |
| XDG config home resolution fails (unlikely — `dirs` crate handles env fallback) | "Could not determine config directory. Set `$XDG_CONFIG_HOME` or `$HOME`." | 1 |

Invalid regex from user-supplied `filler_words` cannot happen at runtime because we always quote each word with `regex::escape` before joining with `|`. That said, the `Postprocessor::new` constructor still returns `Result` — if a future refactor adds un-escaped user input, the error path is ready.

## Testing

### Unit tests

**`config::test_*`** (in `src/config.rs`):

- Default config matches the expected field values.
- Present TOML file parses correctly and overrides defaults.
- Missing TOML file falls back to defaults.
- Malformed TOML returns an error whose message contains the file path and `line`/`column`.
- Unknown field errors under `deny_unknown_fields`.
- `LINDICTION_MODEL` env var overrides `[model] path` from TOML.
- CLI model override beats env var.

**`hotkey::parse_binding_*`** (in `src/hotkey.rs`):

- Canonical case: `"ctrl+alt+space"` → `(CONTROL | ALT, Space)`.
- Case-insensitive: `"CTRL+Alt+SPACE"` works identically.
- Single key, no modifiers: `"f12"` → `(empty, F12)`.
- Alt-named modifier: `"meta+k"` → `(META, KeyK)`.
- Unknown modifier: `"foo+space"` → error mentioning valid modifier list.
- Unknown key: `"ctrl+alt+nonsense"` → error mentioning key categories.
- Empty string: error.

**`postprocess::test_*`** (in `src/postprocess.rs`):

- Trim: `"  hello  "` → `"Hello."`.
- Filler removal: `"um hello uh world"` → `"Hello world."`.
- Capitalize first letter: `"hello"` → `"Hello."`.
- Capitalize after punctuation: `"hello. world"` → `"Hello. World."`.
- Idempotent trailing period: `"hello."` → `"Hello."` (no double period).
- All toggles false: output equals trimmed input, no transformations applied.
- Empty after filler removal: all-filler input returns empty string.
- Preserves case of all-caps words mid-sentence: `"HELLO WORLD"` → `"HELLO WORLD."` (first letter already uppercase, remaining untouched).

### Integration tests

No new integration tests required. The existing `tests/integration_stt.rs` still validates the whisper boundary and is unaffected.

### Manual test plan

1. Start the daemon with no config file. Record an utterance with filler words — verify the output has them stripped and has a trailing period.
2. Write a config file with `remove_fillers = false`, `capitalize_sentences = false`, `ensure_trailing_period = false`. Record the same utterance — verify raw whisper output.
3. Write a config file with `hotkey.binding = "ctrl+alt+d"`. Restart daemon. Verify Ctrl+Alt+Space no longer triggers and Ctrl+Alt+D does.
4. Write a malformed config file (e.g. missing `]` on a section header). Restart — verify the error message is clear.
5. Write a config with an unknown field. Verify the unknown-field error.

## Migration path from v0.1

- Existing v0.1 binaries continue to run with no config file present. Behavior change: transcriptions now get postprocessed by default. Users who had `lindiction` producing raw whisper output and want to keep that must write a minimal config file disabling the three postprocess toggles.
- The `LINDICTION_MODEL` env var and the `--model` CLI flag continue to behave identically — they still override the model path.
- No breaking change to CLI surface; no removed flags.

## Day-by-day schedule (≤3 days)

### Day 1 — Config module

- Rewrite `src/config.rs` around the `Config` / `HotkeyConfig` / `ModelConfig` / `PostprocessConfig` struct tree.
- Add `toml`, `serde`, `regex`, `dirs` to `Cargo.toml`.
- Implement `Config::load(cli_model: Option<PathBuf>) -> Result<Config>`.
- Write the 7 config unit tests.
- Update `src/main.rs` to pass `cli.model` into `Config::load`.

**Exit criterion:** `cargo test --lib config::` green; existing integration test still green.

### Day 2 — Hotkey parser + postprocess

- Extend `src/hotkey.rs` with `parse_binding` and change `start` to take `(Modifiers, Code)`.
- Update `App::run` to call `parse_binding(&config.hotkey.binding)?` and pass the result to `hotkey::start`.
- Write the 7 hotkey-parser unit tests.
- Implement `src/postprocess.rs` with `Postprocessor::new` and `apply`.
- Write the 8 postprocess unit tests.
- Wire postprocess into `app.rs` between transcribe and inject.

**Exit criterion:** all unit tests green; `cargo build --release` clean; manual smoke test passes.

### Day 3 — Polish + release

- Update README with a `Configuration` section documenting the TOML schema and every default value.
- Run the 5 manual tests.
- Tag `v0.2.0` and push. CI should pass; release workflow should produce the new binary.

**Exit criterion:** tag pushed, release workflow green, release notes auto-generated.

## Open questions and deliberate omissions

- **Log the resolved config at startup?** Probably at `debug` level so `-v` shows which hotkey and model path won. Will add in the implementation pass if useful.
- **Validate regex-escape output?** The `regex` crate's error surface is narrow; if the escape/join produces an invalid pattern it would only be due to a `regex` bug. The `Postprocessor::new` `Result` is already in place for that remote case.
- **Sentence boundary under Unicode punctuation (e.g. `。`)?** Not supported in v0.2. Tiny.en is English-only; the boundary-walker checks only `.`, `?`, `!`. Revisit when multilingual models land in v0.3+.
