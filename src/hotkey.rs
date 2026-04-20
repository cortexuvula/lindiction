use anyhow::{Context, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use tokio::sync::mpsc;
use tracing::{debug, info};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Press,
    Release,
}

/// Registers Ctrl+Alt+Space as a global hotkey. The returned
/// receiver yields `Press` and `Release` events. The `GlobalHotKeyManager`
/// is held in a background std::thread that polls the crate's crossbeam
/// channel and forwards to our tokio channel.
pub struct HotkeyListener {
    _manager: GlobalHotKeyManager,
}

// NOTE: `GlobalHotKeyEvent::receiver()` returns a process-global singleton receiver.
// Calling `start()` more than once per process would cause two forwarding threads to
// race on the same underlying channel — events would be split non-deterministically
// between callers. Only call this function once per process.
pub fn start(
    modifiers: Modifiers,
    code: Code,
) -> Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)> {
    let manager = GlobalHotKeyManager::new().context("creating GlobalHotKeyManager")?;
    let hotkey = HotKey::new(Some(modifiers), code);
    let hotkey_id = hotkey.id();
    manager
        .register(hotkey)
        .context("Hotkey registration failed. Is another app bound to this binding?")?;

    info!(hotkey_id, ?modifiers, ?code, "registered hotkey");

    let (tx, rx) = mpsc::channel::<HotkeyEvent>(32);
    let crate_rx = GlobalHotKeyEvent::receiver();

    std::thread::Builder::new()
        .name("lindiction-hotkey".into())
        .spawn(move || {
            while let Ok(event) = crate_rx.recv() {
                // Only forward events for our registered hotkey; other hotkeys
                // in the process (none today, but possible in the future) share
                // the same singleton channel.
                if event.id != hotkey_id {
                    continue;
                }
                let mapped = match event.state {
                    HotKeyState::Pressed => HotkeyEvent::Press,
                    HotKeyState::Released => HotkeyEvent::Release,
                };
                debug!(?mapped, "hotkey event");
                if tx.blocking_send(mapped).is_err() {
                    break;
                }
            }
        })
        .context("spawning hotkey thread")?;

    Ok((HotkeyListener { _manager: manager }, rx))
}

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
            "Unknown hotkey modifier `{s}`. Valid modifiers: ctrl (alias: control), alt, shift, super (alias: meta)."
        ),
    }
}

fn parse_key_token(s: &str) -> Result<Code> {
    const LETTERS: [Code; 26] = [
        Code::KeyA,
        Code::KeyB,
        Code::KeyC,
        Code::KeyD,
        Code::KeyE,
        Code::KeyF,
        Code::KeyG,
        Code::KeyH,
        Code::KeyI,
        Code::KeyJ,
        Code::KeyK,
        Code::KeyL,
        Code::KeyM,
        Code::KeyN,
        Code::KeyO,
        Code::KeyP,
        Code::KeyQ,
        Code::KeyR,
        Code::KeyS,
        Code::KeyT,
        Code::KeyU,
        Code::KeyV,
        Code::KeyW,
        Code::KeyX,
        Code::KeyY,
        Code::KeyZ,
    ];
    const DIGITS: [Code; 10] = [
        Code::Digit0,
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
    ];
    const FKEYS: [Code; 24] = [
        Code::F1,
        Code::F2,
        Code::F3,
        Code::F4,
        Code::F5,
        Code::F6,
        Code::F7,
        Code::F8,
        Code::F9,
        Code::F10,
        Code::F11,
        Code::F12,
        Code::F13,
        Code::F14,
        Code::F15,
        Code::F16,
        Code::F17,
        Code::F18,
        Code::F19,
        Code::F20,
        Code::F21,
        Code::F22,
        Code::F23,
        Code::F24,
    ];

    // ASCII letters and digits are single bytes in UTF-8, so `s.len() == 1`
    // is correct for the fast path. Multi-byte chars (e.g. `é`) fall through
    // to the trailing `match s`, which will reject them with a helpful error.
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

    // F-keys: "f1".."f24" in canonical form only. "f01"/"f024" are rejected
    // so users don't accidentally get F1 when they meant to type something else.
    if let Some(n_str) = s.strip_prefix('f') {
        if !n_str.is_empty() && !n_str.starts_with('0') {
            if let Ok(n) = n_str.parse::<usize>() {
                if (1..=24).contains(&n) {
                    return Ok(FKEYS[n - 1]);
                }
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
            "Unknown hotkey key `{s}`. Valid keys: letters a-z, digits 0-9, space, \
             enter (alias: return), tab, escape (alias: esc), backspace, f1-f24, \
             arrow keys (up, down, left, right)."
        ),
    }
}

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

    #[test]
    fn parse_leading_zero_fkey_errors() {
        // "f01" and "f024" are not canonical — reject rather than silently
        // accept as F1 / F24.
        assert!(parse_binding("f01").is_err());
        assert!(parse_binding("f024").is_err());
        // "f1" and "f24" still work.
        assert!(parse_binding("f1").is_ok());
        assert!(parse_binding("f24").is_ok());
    }
}
