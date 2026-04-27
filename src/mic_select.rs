//! CLI-side / tray-side management of the `[audio].device` entry in
//! `~/.config/lindiction/config.toml`.
//!
//! Same toml_edit-based approach as `src/replace.rs`: round-trips
//! through `DocumentMut` so user comments, blank lines, and field
//! ordering survive. Used by the tray's "Microphone" submenu to
//! persist the user's chosen input device without clobbering anything
//! else they've hand-edited.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::config::config_file_path;

/// Set or clear the audio.device override.
///
/// `Some(name)` writes `[audio].device = "name"`, creating the
/// `[audio]` section if absent. `None` removes the `device` key
/// entirely (so the daemon falls back to the system default on next
/// startup) and removes the `[audio]` section if the removal leaves
/// it empty.
///
/// Creates `~/.config/lindiction/config.toml` (and its parent dir) if
/// neither exists.
pub fn set_device(name: Option<&str>) -> Result<()> {
    let path = config_path()?;
    let mut doc = load_or_new(&path)?;
    apply_set_device(&mut doc, name);
    save(&path, &doc)?;
    Ok(())
}

/// Read the current `[audio].device` value, if any. Convenience for
/// the tray's menu render so it can mark the active item without
/// re-running `Config::load`.
pub fn current_device() -> Result<Option<String>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(read_device(&doc))
}

fn config_path() -> Result<PathBuf> {
    config_file_path().context("could not resolve XDG config directory")
}

/// Read existing TOML or seed a minimal document with an empty
/// `[audio]` section. Same pattern as `replace::load_or_new`.
fn load_or_new(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))
    } else {
        // Empty doc — apply_set_device will create the section as needed.
        Ok(DocumentMut::new())
    }
}

fn save(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Pure transform on the document. Extracted for unit-testability so
/// we can verify behavior without hitting the filesystem.
fn apply_set_device(doc: &mut DocumentMut, name: Option<&str>) {
    match name {
        Some(name) => {
            let audio = doc
                .entry("audio")
                .or_insert_with(|| Item::Table(Table::new()))
                .as_table_mut()
                .expect("audio is a table");
            audio["device"] = Item::Value(Value::from(name));
        }
        None => {
            // Remove the device key. If [audio] becomes empty, drop
            // the section entirely so the file doesn't accumulate
            // empty stanzas across set/clear cycles.
            if let Some(audio) = doc.get_mut("audio").and_then(|i| i.as_table_mut()) {
                audio.remove("device");
                if audio.is_empty() {
                    doc.remove("audio");
                }
            }
        }
    }
}

/// Read [audio].device if present and a string. Returns None for any
/// missing key, non-table [audio], or non-string device value.
fn read_device(doc: &DocumentMut) -> Option<String> {
    doc.get("audio")?
        .as_table()?
        .get("device")?
        .as_str()
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_config<F: FnOnce(&Path)>(f: F) {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let dir = std::env::temp_dir()
            .join(format!("lindiction-mic-select-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let cfg_path = dir.join("lindiction").join("config.toml");
        let _ = std::fs::remove_file(&cfg_path);
        f(&cfg_path);
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_set_device_creates_audio_section_when_absent() {
        let mut doc: DocumentMut = "".parse().unwrap();
        apply_set_device(&mut doc, Some("usbstream:CARD=C920"));
        let s = doc.to_string();
        assert!(s.contains("[audio]"), "missing section; got:\n{s}");
        assert!(
            s.contains(r#"device = "usbstream:CARD=C920""#),
            "missing key/value; got:\n{s}"
        );
    }

    #[test]
    fn apply_set_device_replaces_existing_value() {
        let starting = r#"
[audio]
preroll_ms = 500
device = "old-name"
"#;
        let mut doc: DocumentMut = starting.parse().unwrap();
        apply_set_device(&mut doc, Some("new-name"));
        let s = doc.to_string();
        assert!(s.contains(r#"device = "new-name""#));
        assert!(s.contains("preroll_ms = 500"), "preroll_ms must survive");
    }

    #[test]
    fn apply_set_device_none_removes_key_only() {
        // Removing device should NOT remove the rest of [audio].
        let starting = r#"
[audio]
preroll_ms = 500
device = "to-be-cleared"
"#;
        let mut doc: DocumentMut = starting.parse().unwrap();
        apply_set_device(&mut doc, None);
        let s = doc.to_string();
        assert!(!s.contains("device"), "device key should be gone");
        assert!(s.contains("preroll_ms = 500"), "preroll_ms must remain");
        assert!(s.contains("[audio]"), "[audio] section should remain");
    }

    #[test]
    fn apply_set_device_none_drops_empty_section() {
        // If device was the only thing in [audio], the section header
        // also goes away — keeps the file tidy across set/clear cycles.
        let starting = r#"
[audio]
device = "to-be-cleared"
"#;
        let mut doc: DocumentMut = starting.parse().unwrap();
        apply_set_device(&mut doc, None);
        let s = doc.to_string();
        assert!(!s.contains("device"));
        assert!(
            !s.contains("[audio]"),
            "empty [audio] section should be dropped; got:\n{s}"
        );
    }

    #[test]
    fn apply_set_device_none_no_op_when_already_absent() {
        let starting = r#"[hotkey]
binding = "ctrl+alt+space"
"#;
        let mut doc: DocumentMut = starting.parse().unwrap();
        apply_set_device(&mut doc, None);
        let s = doc.to_string();
        // File should be byte-identical to the input.
        assert_eq!(s, starting);
    }

    #[test]
    fn read_device_returns_none_when_missing() {
        let doc: DocumentMut = "[hotkey]\nbinding = \"f9\"\n".parse().unwrap();
        assert_eq!(read_device(&doc), None);
    }

    #[test]
    fn read_device_returns_value_when_present() {
        let doc: DocumentMut = "[audio]\ndevice = \"my-mic\"\n".parse().unwrap();
        assert_eq!(read_device(&doc).as_deref(), Some("my-mic"));
    }

    #[test]
    fn read_device_handles_non_string_value() {
        // A user who hand-edited and put device = 42 should not
        // panic our reader; we treat it as "no override".
        let doc: DocumentMut = "[audio]\ndevice = 42\n".parse().unwrap();
        assert_eq!(read_device(&doc), None);
    }

    #[test]
    fn set_device_creates_config_when_missing() {
        with_temp_config(|path| {
            assert!(!path.exists());
            set_device(Some("alsa_input.pci-test")).expect("set");
            assert!(path.exists());
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains(r#"device = "alsa_input.pci-test""#));
        });
    }

    #[test]
    fn set_then_clear_round_trip_preserves_user_comments() {
        with_temp_config(|path| {
            let existing = r#"# header
[hotkey]
binding = "f9"

[postprocess]
# keep
remove_fillers = true
"#;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, existing).unwrap();

            set_device(Some("usbstream:CARD=Foo")).unwrap();
            let after_set = std::fs::read_to_string(path).unwrap();
            assert!(after_set.contains("# header"));
            assert!(after_set.contains("# keep"));
            assert!(after_set.contains(r#"device = "usbstream:CARD=Foo""#));
            assert_eq!(
                current_device().unwrap().as_deref(),
                Some("usbstream:CARD=Foo")
            );

            set_device(None).unwrap();
            let after_clear = std::fs::read_to_string(path).unwrap();
            assert!(
                after_clear.contains("# header"),
                "comments must survive clear"
            );
            assert!(after_clear.contains("# keep"));
            assert!(!after_clear.contains("device"));
            assert!(current_device().unwrap().is_none());
        });
    }

    #[test]
    fn current_device_returns_none_for_missing_file() {
        with_temp_config(|path| {
            assert!(!path.exists());
            assert!(current_device().unwrap().is_none());
        });
    }
}
