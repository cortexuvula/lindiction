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
pub fn start() -> Result<(HotkeyListener, mpsc::Receiver<HotkeyEvent>)> {
    let manager = GlobalHotKeyManager::new().context("creating GlobalHotKeyManager")?;
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
    let hotkey_id = hotkey.id();
    manager
        .register(hotkey)
        .context("Hotkey registration failed. Is another app bound to Ctrl+Alt+Space?")?;

    info!(hotkey_id, "registered Ctrl+Alt+Space");

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
