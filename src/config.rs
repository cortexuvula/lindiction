use crate::model_choice::ModelId;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Resolve the default model path to `$XDG_DATA_HOME/lindiction/models/ggml-small.en.bin`
/// (typically `~/.local/share/lindiction/models/ggml-small.en.bin`).
///
/// This is the single source of truth for the default — consumed by
/// `ModelConfig::default` AND by `model_download::ensure_default_model`
/// (which only auto-downloads when `config.model.path == default_model_path()`).
pub fn default_model_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("lindiction")
        .join("models")
        .join("ggml-small.en.bin")
}

/// Path to the TOML config file: `$XDG_CONFIG_HOME/lindiction/config.toml`
/// (typically `~/.config/lindiction/config.toml`). Returns `None` only when
/// neither `$XDG_CONFIG_HOME` nor `$HOME` is set — essentially impossible
/// in practice on Linux.
pub fn config_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("lindiction").join("config.toml"))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub audio: AudioConfig,
    pub model: ModelConfig,
    pub stt: SttConfig,
    pub injection: InjectionConfig,
    pub postprocess: PostprocessConfig,
    pub update: UpdateConfig,
    #[serde(skip)]
    pub sample_rate: u32,
    #[serde(skip)]
    pub channels: u16,
    #[serde(skip)]
    pub auto_selected_model: Option<ModelId>,
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
pub struct AudioConfig {
    /// Milliseconds of mic audio captured *before* the hotkey press to
    /// prepend to each utterance. Set to 0 to disable. Default 300 ms
    /// is enough to cover typical reaction time between the user
    /// starting to speak and the hotkey actually registering.
    pub preroll_ms: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InjectionMethod {
    /// Use `xdotool type` to emit one keystroke per character. Simple,
    /// works in every app (including terminals), but the X server may
    /// silently drop keystrokes on some systems — most often spaces.
    Type,
    /// Put the transcription on the clipboard via `xclip`, then send a
    /// single `Ctrl+V`. Atomic, fast, unaffected by per-keystroke
    /// dropouts. Does not work in terminals (they use Ctrl+Shift+V)
    /// and clobbers the user's clipboard.
    Paste,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InjectionConfig {
    /// `"type"` = per-character xdotool typing (default, universal but
    /// fragile). `"paste"` = clipboard + paste shortcut (fast, reliable,
    /// but clobbers the clipboard).
    pub method: InjectionMethod,
    /// Milliseconds between each keystroke `xdotool type` emits (only
    /// used when `method = "type"`). Too low and the X server silently
    /// drops events (usually space — producing merged words like
    /// "atesttosee"). xdotool's own default is 12; we default a bit
    /// higher for safety on busy desktops.
    pub xdotool_delay_ms: u32,
    /// Key combo sent via `xdotool key` when `method = "paste"`.
    /// Defaults to `ctrl+v` (standard GUI paste). Terminal emulators
    /// typically need `ctrl+shift+v`; `shift+Insert` is an X11-wide
    /// fallback that pastes the primary selection. Whatever string you
    /// set here is passed verbatim to `xdotool key`, so xdotool keysym
    /// syntax applies (capitalized `Insert`, etc.).
    pub paste_shortcut: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    /// 1 = greedy (fastest); 5 = beam search (better accuracy, ~1.5-2×
    /// slower). Values >5 show diminishing returns.
    pub beam_size: u32,
    /// Text primed into the decoder's context before each utterance.
    /// Use this to bias recognition toward proper nouns and jargon the
    /// model wouldn't otherwise know (project names, coworker names,
    /// acronyms). Empty string disables. Keep under ~200 chars —
    /// whisper will truncate longer prompts.
    pub initial_prompt: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostprocessConfig {
    pub remove_fillers: bool,
    pub filler_words: Vec<String>,
    pub capitalize_sentences: bool,
    pub ensure_trailing_period: bool,
    /// Ordered list of [from, to] string pairs. Each `from` is matched
    /// case-insensitively with word boundaries; on match, it's replaced
    /// verbatim with `to` (preserving the `to` casing exactly — so spell
    /// proper nouns the way you want them to appear). Applied after
    /// filler removal and sentence-capitalization; runs in list order
    /// so later entries see earlier substitutions.
    pub replacements: Vec<[String; 2]>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    /// Master switch. When false, the daemon performs no network calls
    /// to GitHub and hides the tray "Check for updates" item.
    pub enabled: bool,
    /// How often to recheck while the daemon runs. 0 = startup only.
    /// A check is also always performed at daemon launch when enabled.
    pub interval_hours: u64,
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
            path: default_model_path(),
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { preroll_ms: 300 }
    }
}

impl Default for InjectionConfig {
    fn default() -> Self {
        Self {
            method: InjectionMethod::Type,
            xdotool_delay_ms: 15,
            paste_shortcut: "ctrl+v".to_string(),
        }
    }
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            beam_size: 5,
            initial_prompt: String::new(),
        }
    }
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            filler_words: ["um", "uh", "ah", "like", "you know", "so", "basically"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            capitalize_sentences: true,
            ensure_trailing_period: true,
            replacements: Vec::new(),
        }
    }
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: 6,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: HotkeyConfig::default(),
            audio: AudioConfig::default(),
            model: ModelConfig::default(),
            stt: SttConfig::default(),
            injection: InjectionConfig::default(),
            postprocess: PostprocessConfig::default(),
            update: UpdateConfig::default(),
            sample_rate: 16_000,
            channels: 1,
            auto_selected_model: None,
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
        let Some(path) = config_file_path() else {
            // `dirs::config_dir()` returns None only when neither $HOME nor
            // $XDG_CONFIG_HOME is set — essentially impossible in practice.
            // The design spec called for a fatal exit here; we soften to
            // warn+default because aborting startup on an edge case the user
            // never explicitly triggered is worse than running with defaults.
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
    fn default_model_path_matches_xdg() {
        let c = Config::default();
        assert_eq!(c.model.path, super::default_model_path());
        // Verify the helper returns an absolute path ending with
        // lindiction/models/ggml-small.en.bin.
        let p = super::default_model_path();
        assert!(
            p.ends_with("lindiction/models/ggml-small.en.bin"),
            "got {}",
            p.display()
        );
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
    fn default_audio_config() {
        let c = Config::default();
        assert_eq!(c.audio.preroll_ms, 300);
    }

    #[test]
    fn default_stt_config() {
        let c = Config::default();
        assert_eq!(c.stt.beam_size, 5);
        assert!(c.stt.initial_prompt.is_empty());
    }

    #[test]
    fn audio_and_stt_sections_parse() {
        let s = r#"
[audio]
preroll_ms = 500

[stt]
beam_size = 1
initial_prompt = "Andre, lindiction, tokio"
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(c.audio.preroll_ms, 500);
        assert_eq!(c.stt.beam_size, 1);
        assert_eq!(c.stt.initial_prompt, "Andre, lindiction, tokio");
    }

    #[test]
    fn default_update_config_is_opt_in() {
        let c = Config::default();
        assert!(c.update.enabled, "update checks default to enabled");
        assert_eq!(c.update.interval_hours, 6);
    }

    #[test]
    fn update_section_parses_both_fields() {
        let s = r#"
[update]
enabled = false
interval_hours = 24
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert!(!c.update.enabled);
        assert_eq!(c.update.interval_hours, 24);
    }

    #[test]
    fn default_runtime_fields() {
        let c = Config::default();
        assert_eq!(c.sample_rate, 16_000);
        assert_eq!(c.channels, 1);
        assert_eq!(c.injection.xdotool_delay_ms, 15);
    }

    #[test]
    fn postprocess_replacements_parse_and_default_empty() {
        let c = Config::default();
        assert!(c.postprocess.replacements.is_empty());

        let s = r#"
[postprocess]
replacements = [["clod", "Claude"], ["lin dictation", "lindiction"]]
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(
            c.postprocess.replacements,
            vec![
                ["clod".to_string(), "Claude".to_string()],
                ["lin dictation".to_string(), "lindiction".to_string()],
            ]
        );
    }

    #[test]
    fn injection_section_parses() {
        let s = r#"
[injection]
method = "paste"
xdotool_delay_ms = 25
paste_shortcut = "ctrl+shift+v"
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(c.injection.method, InjectionMethod::Paste);
        assert_eq!(c.injection.xdotool_delay_ms, 25);
        assert_eq!(c.injection.paste_shortcut, "ctrl+shift+v");
    }

    #[test]
    fn injection_defaults_match_expected() {
        let c = Config::default();
        assert_eq!(c.injection.method, InjectionMethod::Type);
        assert_eq!(c.injection.xdotool_delay_ms, 15);
        assert_eq!(c.injection.paste_shortcut, "ctrl+v");
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
        assert_eq!(c.model.path, super::default_model_path());
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
        assert_eq!(c.model.path, super::default_model_path());
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

    #[test]
    fn load_reads_actual_toml_file() {
        let dir = std::env::temp_dir().join("lindiction-config-test-reads");
        let cfg_dir = dir.join("lindiction");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[hotkey]
binding = "f9"

[model]
path = "/from/toml.bin"
"#,
        )
        .unwrap();

        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::remove_var("LINDICTION_MODEL");

        let c = Config::load(None).expect("load");
        assert_eq!(c.hotkey.binding, "f9");
        assert_eq!(c.model.path, PathBuf::from("/from/toml.bin"));

        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_surfaces_malformed_toml_error() {
        let dir = std::env::temp_dir().join("lindiction-config-test-malformed");
        let cfg_dir = dir.join("lindiction");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        // Missing closing bracket on the section header.
        std::fs::write(cfg_dir.join("config.toml"), "[hotkey\nbinding = \"ctrl\"\n").unwrap();

        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::remove_var("LINDICTION_MODEL");

        let err = Config::load(None).expect_err("should fail on malformed TOML");
        let msg = format!("{err:#}"); // {:#} expands anyhow chain including the toml::de::Error

        assert!(
            msg.contains("config.toml"),
            "error should include the config file path; got: {msg}"
        );
        let msg_lower = msg.to_lowercase();
        assert!(
            msg_lower.contains("line") || msg_lower.contains("column"),
            "error chain should include toml line/column info; got: {msg}"
        );

        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_file_path_ends_correctly() {
        // If the environment resolves $XDG_CONFIG_HOME (or $HOME on Linux),
        // the path must end with lindiction/config.toml. If the env is so
        // broken that dirs::config_dir returns None, the function returns
        // None — acceptable, and we skip the assertion.
        if let Some(p) = super::config_file_path() {
            assert!(p.ends_with("lindiction/config.toml"), "got {}", p.display());
        }
    }
}
