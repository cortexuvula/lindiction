use crate::hw_detect;
use crate::model_choice::{self, ModelId};
use crate::model_download;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info, warn};

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
    /// Populated by `Config::load` when the model path was resolved via
    /// hardware-based auto-selection (i.e. not explicitly set via CLI,
    /// env, or TOML). Not serialized — purely runtime state consumed by
    /// `model_download::ensure_model` to gate first-run download.
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
    /// Override the system default input device. None (the default)
    /// uses whatever the audio server reports as the user's default
    /// source — picks up changes from `pactl set-default-source` /
    /// WirePlumber metadata automatically. Some(name) pins to a
    /// specific cpal-level device name (whatever `Device::name()`
    /// returned at enumeration time). Set via the tray menu's
    /// "Microphone" submenu, or hand-edited.
    pub device: Option<String>,
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
        // Empty PathBuf is the "not specified" sentinel. Config::load
        // turns it into a concrete path via one of (CLI, env, TOML,
        // hardware-auto). Users writing `path = ""` in TOML would
        // also land here — that's not a useful config, and treating
        // it as "not specified" is more charitable than erroring.
        Self {
            path: PathBuf::new(),
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            preroll_ms: 300,
            device: None,
        }
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
    /// Resolve the full runtime config. Model-path precedence is:
    ///   1. `--model` CLI flag (`cli_model`)
    ///   2. `LINDICTION_MODEL` env var
    ///   3. `[model].path` in TOML
    ///   4. Hardware-based auto-selection via `hw_detect` + `model_choice`
    ///
    /// Only (4) sets `auto_selected_model`; the first three are treated as
    /// explicit user choices and `model_download::ensure_model` will skip
    /// auto-download for them.
    pub fn load(cli_model: Option<PathBuf>) -> Result<Self> {
        let mut config = Self::from_xdg_file()?;
        // Probe hardware once up front. Used by precedence-4 auto-
        // selection and by reconciliation logging at the end. Calling
        // `hw_detect::detect()` is cheap (a few subprocess spawns at
        // most) but still wasteful to repeat.
        let hw = hw_detect::detect();

        // Resolve model path by precedence. Only precedence 4 sets
        // `auto_selected_model`; the first three are explicit user
        // choices and `model_download::ensure_model` skips auto-download
        // for them.
        if let Some(p) = cli_model {
            // Precedence 1: CLI.
            config.model.path = p;
            config.auto_selected_model = None;
        } else if let Ok(p) = std::env::var("LINDICTION_MODEL") {
            // Precedence 2: env var. An empty env var falls through.
            if !p.is_empty() {
                config.model.path = PathBuf::from(p);
                config.auto_selected_model = None;
            }
        }
        // Precedence 3 (TOML explicit path) is already reflected in
        // `config.model.path` from `from_xdg_file`. Precedence 4: if
        // nothing above set a path, fall back to hardware auto-select.
        if config.model.path.as_os_str().is_empty() {
            let chosen = model_choice::recommend(&hw);
            info!(
                profile = ?hw,
                chosen = chosen.tag(),
                "auto-selected whisper model based on hardware"
            );
            config.model.path = model_download::default_model_path_for(chosen);
            config.auto_selected_model = Some(chosen);
        }

        // Log reconciliation regardless of how model path was resolved —
        // a user who set --model explicitly still benefits from knowing
        // whether their compiled backend matches their hardware.
        match crate::reconcile_backend(&hw) {
            crate::BackendReconciliation::CpuBuildNoGpu => {
                debug!(
                    backend = crate::COMPILED_BACKEND,
                    "cpu build on cpu host — nothing to reconcile"
                );
            }
            crate::BackendReconciliation::GpuBuildMatchesGpu => {
                info!(
                    backend = crate::COMPILED_BACKEND,
                    "gpu build matches detected gpu"
                );
            }
            crate::BackendReconciliation::CpuBuildWithGpu => {
                warn!(
                    detected = ?hw.gpu,
                    "running on cpu-only build but a gpu was detected; \
                     rebuild with `cargo build --release --features cuda` (or vulkan / hipblas) \
                     to use the gpu"
                );
            }
            crate::BackendReconciliation::GpuBuildWithoutMatchingGpu => {
                warn!(
                    compiled = crate::COMPILED_BACKEND,
                    detected = ?hw.gpu,
                    "binary was built for a gpu backend that doesn't match the detected hardware; \
                     the daemon will still run but GPU is unused. Rebuild with the matching feature flag."
                );
            }
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
        // ModelConfig::default().path is now the empty-PathBuf sentinel
        // meaning "not specified"; Config::load resolves it via
        // CLI > env > TOML > hardware-auto.
        let c = Config::default();
        assert!(
            c.model.path.as_os_str().is_empty(),
            "expected empty-path sentinel, got {}",
            c.model.path.display()
        );
        // The canonical XDG path for SmallEn still lives in the models
        // subdir — that's what model_download::default_model_path_for
        // returns and what hardware-auto falls back to when the recommender
        // picks SmallEn.
        let p = crate::model_download::default_model_path_for(ModelId::SmallEn);
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
        assert!(c.audio.device.is_none());
    }

    #[test]
    fn audio_device_override_parses() {
        let s = r#"
[audio]
device = "alsa_input.pci-0000_08_00.4.analog-stereo"
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(
            c.audio.device.as_deref(),
            Some("alsa_input.pci-0000_08_00.4.analog-stereo")
        );
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
        // TOML that doesn't name [model] leaves ModelConfig::default()'s
        // empty-PathBuf sentinel in place; resolution into a concrete
        // path only happens inside Config::load.
        let s = r#"
[hotkey]
binding = "f10"
"#;
        let c: Config = toml::from_str(s).expect("parse");
        assert_eq!(c.hotkey.binding, "f10");
        assert!(
            c.model.path.as_os_str().is_empty(),
            "expected empty-path sentinel before Config::load runs; got {}",
            c.model.path.display()
        );
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
        // With no CLI / env / TOML, hardware auto-selection fires. The
        // actual model size depends on the test host's RAM/cores — so
        // only verify that auto-selection populated the field and that
        // the chosen path looks like a whisper ggml file.
        assert!(
            c.auto_selected_model.is_some(),
            "expected hardware auto-selection to fire"
        );
        let fname = c
            .model
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert!(
            fname.starts_with("ggml-"),
            "auto-selected path should be a ggml-*.bin file; got {}",
            c.model.path.display()
        );
        assert_eq!(c.hotkey.binding, "ctrl+alt+space");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn env_model_beats_default() {
        isolate_xdg();
        std::env::set_var("LINDICTION_MODEL", "/from/env.bin");
        let c = Config::load(None).expect("load");
        assert_eq!(c.model.path, PathBuf::from("/from/env.bin"));
        assert!(
            c.auto_selected_model.is_none(),
            "env-specified path must not auto-download"
        );
        std::env::remove_var("LINDICTION_MODEL");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn cli_model_beats_env() {
        isolate_xdg();
        std::env::set_var("LINDICTION_MODEL", "/from/env.bin");
        let c = Config::load(Some(PathBuf::from("/from/cli.bin"))).expect("load");
        assert_eq!(c.model.path, PathBuf::from("/from/cli.bin"));
        assert!(
            c.auto_selected_model.is_none(),
            "CLI-specified path must not auto-download"
        );
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
        assert!(
            c.auto_selected_model.is_none(),
            "TOML-specified path must not auto-download"
        );

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

    #[test]
    fn auto_selection_populates_auto_selected_model_when_nothing_set() {
        isolate_xdg();
        std::env::remove_var("LINDICTION_MODEL");
        let c = Config::load(None).expect("load");
        // Hardware auto-select must have fired — no CLI, no env, no TOML.
        assert!(
            c.auto_selected_model.is_some(),
            "expected auto_selected_model to be populated; got {:?}",
            c.auto_selected_model
        );
        // And the chosen path should live in the XDG models dir.
        let parent = c.model.path.parent().unwrap();
        assert!(
            parent.ends_with("lindiction/models"),
            "chosen path should live in XDG models dir; got {}",
            c.model.path.display()
        );
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn cli_path_clears_auto_selected_model() {
        isolate_xdg();
        std::env::remove_var("LINDICTION_MODEL");
        let c = Config::load(Some(PathBuf::from("/from/cli.bin"))).expect("load");
        assert_eq!(c.model.path, PathBuf::from("/from/cli.bin"));
        assert!(
            c.auto_selected_model.is_none(),
            "CLI-specified path must not auto-download anything"
        );
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn toml_path_clears_auto_selected_model() {
        // TOML with an explicit [model].path must not trigger auto-download.
        let dir = std::env::temp_dir().join("lindiction-config-auto-toml-path");
        let cfg_dir = dir.join("lindiction");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[model]
path = "/from/toml.bin"
"#,
        )
        .unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::remove_var("LINDICTION_MODEL");

        let c = Config::load(None).expect("load");
        assert_eq!(c.model.path, PathBuf::from("/from/toml.bin"));
        assert!(c.auto_selected_model.is_none());

        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
