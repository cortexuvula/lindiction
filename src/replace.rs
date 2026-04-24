//! CLI-side management of the `[postprocess].replacements` table in
//! `~/.config/lindiction/config.toml`.
//!
//! We use `toml_edit` rather than the `toml` crate's Deserialize +
//! Serialize path because the latter destroys user comments, blank
//! lines, and field ordering — the config file is meant to be
//! hand-editable, so every round-trip through the CLI must leave the
//! file looking like the user left it.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Value};

use crate::config::config_file_path;

/// Public Change outcome for `add` — distinguishes "added new" from
/// "overwrote existing" so the CLI can print something useful.
pub enum AddOutcome {
    Added,
    Updated { previous: String },
}

/// Append-or-overwrite a [from, to] entry in the replacements array.
/// If an entry with the same `from` (case-insensitive) already exists,
/// its `to` is updated in place; otherwise a new entry is appended.
pub fn add(from: &str, to: &str) -> Result<AddOutcome> {
    let path = config_path()?;
    let mut doc = load_or_new(&path)?;
    let array = replacements_array_mut(&mut doc);

    for i in 0..array.len() {
        if let Some((existing_from, existing_to)) = entry_at(array, i) {
            if existing_from.eq_ignore_ascii_case(from) {
                // Preserve the slot's leading whitespace/decor so the
                // update doesn't change the file's shape.
                let existing_prefix = array
                    .get(i)
                    .and_then(|v| v.decor().prefix())
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut new_val = pair_value(from, to);
                new_val.decor_mut().set_prefix(existing_prefix);
                array.replace_formatted(i, new_val);
                save(&path, &doc)?;
                return Ok(AddOutcome::Updated {
                    previous: existing_to,
                });
            }
        }
    }

    // Fresh append. Put the new entry on its own line with four spaces
    // of indent so multi-entry arrays stay readable after `replace add`.
    // toml_edit's default `push` inlines the value after the previous
    // entry's trailing comma, which glues everything onto one visual
    // line.
    let mut new_val = pair_value(from, to);
    new_val.decor_mut().set_prefix("\n    ");
    array.push_formatted(new_val);
    save(&path, &doc)?;
    Ok(AddOutcome::Added)
}

/// Remove the entry whose `from` matches (case-insensitive). Returns
/// the `to` value that was removed, or None if no match was found.
pub fn remove(from: &str) -> Result<Option<String>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let mut doc = load_or_new(&path)?;
    let array = replacements_array_mut(&mut doc);
    for i in 0..array.len() {
        if let Some((existing_from, existing_to)) = entry_at(array, i) {
            if existing_from.eq_ignore_ascii_case(from) {
                array.remove(i);
                save(&path, &doc)?;
                return Ok(Some(existing_to));
            }
        }
    }
    Ok(None)
}

/// Return all [from, to] pairs in file order.
pub fn list() -> Result<Vec<(String, String)>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let doc = load_or_new(&path)?;
    let mut out = Vec::new();
    if let Some(array) = find_replacements_array(&doc) {
        for i in 0..array.len() {
            if let Some(pair) = entry_at(array, i) {
                out.push(pair);
            }
        }
    }
    Ok(out)
}

/// Open `config.toml` in the user's `$EDITOR` (falling back to `nano`,
/// then `vi`) for free-form editing. Blocks until the editor exits.
pub fn edit_in_editor() -> Result<()> {
    let path = config_path()?;
    if !path.exists() {
        // Create an empty stub so the editor has something to open.
        let doc = load_or_new(&path)?;
        save(&path, &doc)?;
    }
    let argv = pick_editor_argv();
    // argv is guaranteed non-empty by pick_editor_argv's contract
    // (it always returns at least `vi`).
    let (program, flags) = argv.split_first().expect("pick_editor_argv returns non-empty");
    let status = std::process::Command::new(program)
        .args(flags)
        .arg(&path)
        .status()
        .with_context(|| format!("failed to spawn editor `{program}`"))?;
    if !status.success() {
        anyhow::bail!("editor `{program}` exited with {status}");
    }
    Ok(())
}

/// Pick the editor to spawn, returning it as program + args.
///
/// Handles the common case of `EDITOR="code --wait"` / `EDITOR="nvim -u NONE"`
/// where the env var holds more than just a program name. Without splitting,
/// `Command::new("code --wait")` would fail with ENOENT (no such file).
///
/// Matches git / cargo / make semantics: naive whitespace split, no shell
/// quoting support. `EDITOR='"my editor"'` with quoted-path-containing-spaces
/// isn't handled — bringing in `shell_words` just for that would be overkill
/// for a rare edge case no one has asked for.
fn pick_editor_argv() -> Vec<String> {
    if let Ok(e) = std::env::var("EDITOR") {
        let tokens: Vec<String> = e.split_whitespace().map(String::from).collect();
        if !tokens.is_empty() {
            return tokens;
        }
    }
    if which::which("nano").is_ok() {
        return vec!["nano".to_string()];
    }
    vec!["vi".to_string()]
}

fn config_path() -> Result<PathBuf> {
    config_file_path().context("could not resolve XDG config directory")
}

/// Read the config file if it exists, otherwise start from a minimal
/// document containing an empty `[postprocess].replacements` array.
/// The `[postprocess]` section is seeded so the user can see the shape
/// after their first `replace add`.
fn load_or_new(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))
    } else {
        // Starter content. toml_edit preserves this formatting on save.
        let seed = "[postprocess]\nreplacements = []\n";
        seed.parse::<DocumentMut>()
            .context("seeding a new config.toml")
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

/// Fetch `[postprocess].replacements` as a mutable `Array`, creating
/// either the section or the array as needed. Panics are impossible
/// because we always leave `replacements` set to an array before we
/// hand out the reference.
fn replacements_array_mut(doc: &mut DocumentMut) -> &mut Array {
    let postprocess = doc
        .entry("postprocess")
        .or_insert_with(|| Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .expect("postprocess is a table");
    let item = postprocess
        .entry("replacements")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())));
    // If the key already existed with a non-array value, overwrite.
    if !matches!(item, Item::Value(Value::Array(_))) {
        *item = Item::Value(Value::Array(Array::new()));
    }
    match item {
        Item::Value(Value::Array(arr)) => arr,
        _ => unreachable!("just set to array"),
    }
}

fn find_replacements_array(doc: &DocumentMut) -> Option<&Array> {
    doc.get("postprocess")?
        .as_table()?
        .get("replacements")?
        .as_value()?
        .as_array()
}

/// Extract a `[from, to]` pair from an array slot, or `None` if the
/// shape is wrong. We silently skip malformed entries in list/lookup
/// rather than erroring — the running daemon's config loader will
/// complain more loudly if the file is actually broken.
fn entry_at(array: &Array, i: usize) -> Option<(String, String)> {
    let inner = array.get(i)?.as_array()?;
    let from = inner.get(0)?.as_str()?.to_string();
    let to = inner.get(1)?.as_str()?.to_string();
    Some((from, to))
}

fn pair_value(from: &str, to: &str) -> Value {
    let mut inner = Array::new();
    inner.push(Value::from(from));
    inner.push(Value::from(to));
    Value::Array(inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // All tests swap $XDG_CONFIG_HOME to a temp dir so they operate on
    // an isolated config.toml. Env mutation is process-global, so
    // tests run serially under this mutex.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_config<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let dir =
            std::env::temp_dir().join(format!("lindiction-replace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let cfg_path = dir.join("lindiction").join("config.toml");
        let _ = std::fs::remove_file(&cfg_path);
        f(&cfg_path);
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_creates_config_when_missing() {
        with_temp_config(|path| {
            assert!(!path.exists());
            let outcome = add("clod", "Claude").expect("add");
            assert!(matches!(outcome, AddOutcome::Added));
            assert!(path.exists());
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("clod"));
            assert!(text.contains("Claude"));
        });
    }

    #[test]
    fn list_roundtrip() {
        with_temp_config(|_path| {
            add("clod", "Claude").unwrap();
            add("fire coding", "vibe coding").unwrap();
            let items = list().unwrap();
            assert_eq!(
                items,
                vec![
                    ("clod".to_string(), "Claude".to_string()),
                    ("fire coding".to_string(), "vibe coding".to_string()),
                ]
            );
        });
    }

    #[test]
    fn add_duplicate_updates_in_place() {
        with_temp_config(|_path| {
            add("clod", "Claude").unwrap();
            let outcome = add("clod", "Cloud").expect("second add");
            match outcome {
                AddOutcome::Updated { previous } => assert_eq!(previous, "Claude"),
                AddOutcome::Added => panic!("expected Updated"),
            }
            let items = list().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], ("clod".to_string(), "Cloud".to_string()));
        });
    }

    #[test]
    fn add_duplicate_case_insensitive() {
        with_temp_config(|_path| {
            add("clod", "Claude").unwrap();
            let outcome = add("CLOD", "Claude2").unwrap();
            assert!(matches!(outcome, AddOutcome::Updated { .. }));
            let items = list().unwrap();
            assert_eq!(items.len(), 1);
            // The NEW from ("CLOD") replaces the old one ("clod").
            assert_eq!(items[0], ("CLOD".to_string(), "Claude2".to_string()));
        });
    }

    #[test]
    fn remove_returns_previous_value() {
        with_temp_config(|_path| {
            add("clod", "Claude").unwrap();
            let removed = remove("clod").unwrap();
            assert_eq!(removed, Some("Claude".to_string()));
            assert!(list().unwrap().is_empty());
        });
    }

    #[test]
    fn remove_missing_returns_none() {
        with_temp_config(|_path| {
            let removed = remove("nonexistent").unwrap();
            assert_eq!(removed, None);
        });
    }

    #[test]
    fn add_preserves_existing_config_comments() {
        with_temp_config(|path| {
            let existing = r#"# user comment
[hotkey]
binding = "f9"

[postprocess]
# keep this comment
remove_fillers = true
replacements = [["old", "new"]]
"#;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, existing).unwrap();

            add("clod", "Claude").unwrap();

            let after = std::fs::read_to_string(path).unwrap();
            assert!(
                after.contains("# user comment"),
                "file should keep top comment; got:\n{after}"
            );
            assert!(
                after.contains("# keep this comment"),
                "file should keep inline comment; got:\n{after}"
            );
            assert!(after.contains("old"));
            assert!(after.contains("clod"));
            assert!(after.contains("binding = \"f9\""));
        });
    }

    #[test]
    fn editor_argv_single_word() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var("EDITOR", "vim");
        assert_eq!(pick_editor_argv(), vec!["vim".to_string()]);
        std::env::remove_var("EDITOR");
    }

    #[test]
    fn editor_argv_splits_on_whitespace() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var("EDITOR", "code --wait");
        assert_eq!(
            pick_editor_argv(),
            vec!["code".to_string(), "--wait".to_string()]
        );
        std::env::remove_var("EDITOR");
    }

    #[test]
    fn editor_argv_collapses_runs_of_whitespace() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var("EDITOR", "  nvim    -u   NONE  ");
        assert_eq!(
            pick_editor_argv(),
            vec!["nvim".to_string(), "-u".to_string(), "NONE".to_string()]
        );
        std::env::remove_var("EDITOR");
    }

    #[test]
    fn editor_argv_empty_or_blank_env_falls_back() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        // "" and "   " both should trigger the fallback, not an empty argv.
        for blank in ["", "   ", "\t\n"] {
            std::env::set_var("EDITOR", blank);
            let argv = pick_editor_argv();
            assert!(
                !argv.is_empty(),
                "argv must never be empty for EDITOR = {blank:?}"
            );
            let prog = argv.first().unwrap().as_str();
            assert!(
                prog == "nano" || prog == "vi",
                "fallback should be nano or vi; got {prog} for EDITOR = {blank:?}"
            );
        }
        std::env::remove_var("EDITOR");
    }

    #[test]
    fn editor_argv_unset_env_falls_back() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::remove_var("EDITOR");
        let argv = pick_editor_argv();
        assert!(!argv.is_empty());
        let prog = argv.first().unwrap().as_str();
        assert!(
            prog == "nano" || prog == "vi",
            "fallback should be nano or vi; got {prog}"
        );
    }
}
