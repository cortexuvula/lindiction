use crate::autostart;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Visual states that the tray icon can display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Idle,
    Recording,
    Processing,
    Paused,
}

impl TrayEvent {
    /// Freedesktop theme-icon name for this state. These live in every
    /// modern icon theme; we avoid shipping our own PNG assets in v0.3.
    pub fn icon_name(self) -> &'static str {
        match self {
            TrayEvent::Idle => "audio-input-microphone",
            TrayEvent::Recording => "media-record",
            TrayEvent::Processing => "view-refresh",
            TrayEvent::Paused => "media-playback-pause",
        }
    }

    /// Short human-readable tool-tip shown on hover. Distinct per state
    /// so assistive tech / status inspections can distinguish idle from
    /// recording without relying on color alone.
    pub fn tooltip(self) -> &'static str {
        match self {
            TrayEvent::Idle => "Lindiction — idle",
            TrayEvent::Recording => "Lindiction — recording",
            TrayEvent::Processing => "Lindiction — transcribing",
            TrayEvent::Paused => "Lindiction — paused",
        }
    }
}

/// Commands the tray menu sends to the App's event loop. Replaces the
/// former unit-typed shutdown channel so a single receiver can handle
/// every user-initiated action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCmd {
    /// Flip the paused state. The App loop owns the authoritative bool;
    /// the tray mirrors it locally for checkbox rendering.
    TogglePause,
    /// Graceful shutdown followed by exec-replace with the current binary
    /// (handled by `main.rs`). Used to pick up config changes without
    /// asking the user to relaunch.
    Restart,
    /// Graceful shutdown. Process exits normally.
    Quit,
}

/// Internal ksni tray implementation. The `state` field is mutated
/// from an async task via `ksni::Handle::update`.
///
/// `autostart_enabled`, `autostart_supported`, and `paused` are captured
/// at startup and updated in place when the user clicks the relevant
/// menu item. ksni diffs the menu on every `update_menu` call and emits
/// a live DBus property update, so checkmarks flip in an open menu
/// without rebuilding.
///
/// `paused` is only a UI cache — the App loop owns the authoritative
/// state. If the click's command-channel send fails (receiver dropped),
/// we leave the cache untouched so the checkbox never lies.
struct LindictionTray {
    state: TrayEvent,
    control_tx: mpsc::UnboundedSender<ControlCmd>,
    autostart_enabled: bool,
    autostart_supported: bool,
    paused: bool,
}

impl ksni::Tray for LindictionTray {
    fn title(&self) -> String {
        "Lindiction".to_string()
    }

    fn id(&self) -> String {
        "lindiction".to_string()
    }

    fn icon_name(&self) -> String {
        self.state.icon_name().to_string()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.state.tooltip().to_string(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};
        // Order: the two most-frequent toggles on top (Pause, Open config),
        // then Auto-start (rare toggle), then info items, then the two
        // lifecycle actions at the bottom (Restart, Quit).
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            CheckmarkItem {
                label: "Pause".into(),
                checked: self.paused,
                activate: Box::new(|this: &mut Self| toggle_pause(this)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open config\u{2026}".into(),
                activate: Box::new(|_: &mut Self| open_config()),
                ..Default::default()
            }
            .into(),
        ];

        // Only surface the autostart toggle when systemctl --user is usable.
        // On non-systemd distros or headless SSH sessions without linger,
        // showing a greyed-out checkbox would be more confusing than hiding.
        if self.autostart_supported {
            items.push(
                CheckmarkItem {
                    label: "Auto-start on login".into(),
                    checked: self.autostart_enabled,
                    activate: Box::new(|this: &mut Self| toggle_autostart(this)),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.extend([
            ksni::MenuItem::Separator,
            StandardItem {
                label: "About Lindiction".into(),
                activate: Box::new(|_: &mut Self| about()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Help".into(),
                activate: Box::new(|_: &mut Self| help()),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Restart".into(),
                activate: Box::new(|this: &mut Self| {
                    // Restart reloads config via exec-replace. If the channel
                    // is closed, the App loop has already exited; nothing to do.
                    let _ = this.control_tx.send(ControlCmd::Restart);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.control_tx.send(ControlCmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

/// Public façade around the ksni service. Lives for the duration of the
/// daemon. Dropping it calls `Handle::shutdown()` in the `Drop` impl,
/// which stops the ksni background thread and releases the DBus name.
pub struct TrayManager {
    state_tx: mpsc::UnboundedSender<TrayEvent>,
    control_rx: mpsc::UnboundedReceiver<ControlCmd>,
    handle: ksni::Handle<LindictionTray>,
}

impl TrayManager {
    /// Register the tray icon on the session bus. Returns a manager that
    /// the main app uses to push state events and listen for user-driven
    /// control commands (Pause/Restart/Quit).
    ///
    /// Non-fatal on failure: if the tray cannot be registered (e.g. no
    /// DBus session, or a StatusNotifier host is not present), this
    /// returns a manager whose `set_state` is a no-op and whose control
    /// channel never fires. The daemon still works via hotkey.
    pub fn start() -> Self {
        let (state_tx, mut state_rx) = mpsc::unbounded_channel::<TrayEvent>();
        // Unbounded so fast user clicks never drop a toggle. The receiver
        // lives on the main event loop, which drains promptly.
        let (control_tx, control_rx) = mpsc::unbounded_channel::<ControlCmd>();

        // Snapshot autostart state at startup. Live user-initiated toggles
        // update the cached field directly; external `systemctl` invocations
        // won't refresh until daemon restart, which is an acceptable trade
        // for not polling subprocesses on a timer.
        let autostart_supported = autostart::is_supported();
        let autostart_enabled = autostart_supported && autostart::status().is_enabled();

        let tray = LindictionTray {
            state: TrayEvent::Idle,
            control_tx,
            autostart_enabled,
            autostart_supported,
            // Pause always starts off on a fresh launch — ephemeral state by design.
            paused: false,
        };

        // ksni 0.2.2's spawn() calls self.run().unwrap() in a background
        // std::thread. If the DBus session bus is unavailable, run() returns
        // Err and the thread panics. Because that panic is on a non-main
        // thread, the process survives — but the panic message prints to
        // stderr (not to tracing). The Handle's internal Mutex is not held
        // at panic time (failure occurs before the first lock), so it is
        // not poisoned; subsequent handle.update() calls silently no-op.
        // Net effect: daemon continues working via hotkey even without a
        // tray icon, at the cost of a one-time stderr panic message.
        let service = ksni::TrayService::new(tray);
        let handle = service.handle();
        service.spawn();

        info!("tray service spawned");

        // Bridge the mpsc<TrayEvent> channel into ksni's Handle::update calls.
        let handle_bridge = handle.clone();
        tokio::spawn(async move {
            while let Some(event) = state_rx.recv().await {
                debug!(?event, "tray state update");
                handle_bridge.update(|t| t.state = event);
            }
            debug!("tray state channel closed; exiting bridge task");
        });

        Self {
            state_tx,
            control_rx,
            handle,
        }
    }

    /// Non-blocking. Safe to call from any thread or async context.
    /// Events are queued on an unbounded channel and applied in order
    /// by a background tokio task.
    pub fn set_state(&self, event: TrayEvent) {
        if self.state_tx.send(event).is_err() {
            warn!("tray bridge task has exited; state update dropped");
        }
    }

    /// Main app awaits this to learn when the user picked Pause, Restart,
    /// or Quit from the tray menu.
    pub fn control_signal(&mut self) -> &mut mpsc::UnboundedReceiver<ControlCmd> {
        &mut self.control_rx
    }

    /// Reflect the authoritative paused state back into the tray so the
    /// Pause checkbox stays truthful even if, later, the app wants to
    /// change paused state from somewhere other than a user click.
    ///
    /// Currently only used by App::run when it wants to force the tray's
    /// local cache back in sync (e.g., after a no-op toggle). Keeps the
    /// UI-cache-only invariant documented on LindictionTray honest.
    pub fn set_paused(&self, paused: bool) {
        self.handle.update(|t| t.paused = paused);
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        // Tell the ksni background thread to stop its polling loop and
        // release the DBus name cleanly.
        self.handle.shutdown();
    }
}

/// Handle a click on the "Pause" checkbox. Sends `ControlCmd::TogglePause`
/// to the App loop; the cached `paused` bool is only flipped on successful
/// send so the checkbox never shows a state the daemon can't honor.
///
/// The App loop is the authoritative owner of the pause state — it gates
/// hotkey press/release on the bool and updates the tray icon. This
/// helper only updates the tray's own rendering cache.
fn toggle_pause(this: &mut LindictionTray) {
    if this.control_tx.send(ControlCmd::TogglePause).is_err() {
        warn!("control channel closed; pause toggle dropped");
        return;
    }
    this.paused = !this.paused;
    debug!(paused = this.paused, "pause toggled from tray");
}

/// Handle a click on the "Auto-start on login" checkbox. Flips the cached
/// state optimistically, shells out to `systemctl --user enable|disable`,
/// and on failure reverts the cache and shows a desktop notification.
///
/// Runs synchronously on the ksni thread. Typical `systemctl --user enable`
/// returns in well under 200 ms on a healthy session, so blocking here is
/// fine; a longer pause would indicate a genuine systemd problem the user
/// should learn about immediately rather than a latency hazard to hide.
fn toggle_autostart(this: &mut LindictionTray) {
    let desired = !this.autostart_enabled;
    let result = if desired {
        autostart::enable()
    } else {
        autostart::disable()
    };
    match result {
        Ok(()) => {
            this.autostart_enabled = desired;
            info!(enabled = desired, "autostart toggled via tray");
        }
        Err(e) => {
            warn!(error = %e, desired, "autostart toggle failed");
            notify_warn(&format!(
                "Could not {} auto-start: {e}",
                if desired { "enable" } else { "disable" }
            ));
        }
    }
}

/// Open the TOML config file in the user's default editor, creating an
/// empty file if it doesn't exist yet. Empty TOML is valid — it parses
/// to all defaults thanks to `#[serde(default)]` on every config struct.
fn open_config() {
    let Some(path) = crate::config::config_file_path() else {
        warn!("could not resolve XDG config path; Open config has no target");
        return;
    };

    if let Err(e) = ensure_config_file_exists(&path) {
        warn!(error = %e, path = %path.display(), "could not create config file");
        notify_warn(&format!("Could not create {}: {}", path.display(), e));
        return;
    }

    // `xdg-open`'s exit code is unreliable across distros (some backends
    // return non-zero even on success). We only flag spawn failures
    // (Err from `.status()`), not non-zero exits.
    if let Err(e) = std::process::Command::new("xdg-open").arg(&path).status() {
        warn!(error = %e, "xdg-open failed");
        notify_warn(&format!("xdg-open failed: {e}"));
    }
}

/// Create the config file at `path` if missing. No-op if the file already
/// exists. Creates all parent directories as needed. Written empty — the
/// user's editor (or their subsequent config authoring) fills it in.
fn ensure_config_file_exists(path: &std::path::Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, "")?;
    info!(path = %path.display(), "created empty config file");
    Ok(())
}

/// Show a desktop notification with version, license, and the repo URL.
/// Best-effort: if the notification subsystem is unavailable, silently no-op.
fn about() {
    let _ = notify_rust::Notification::new()
        .appname("Lindiction")
        .summary(&format!("Lindiction v{}", env!("CARGO_PKG_VERSION")))
        .body(
            "Push-to-talk voice dictation for Linux.\n\
             MIT licensed.\n\
             https://github.com/cortexuvula/lindiction",
        )
        .icon("audio-input-microphone")
        .timeout(notify_rust::Timeout::Milliseconds(6000))
        .show();
}

/// Open the project's GitHub page in the user's default browser.
fn help() {
    const REPO_URL: &str = "https://github.com/cortexuvula/lindiction";
    // `xdg-open`'s exit code is unreliable across distros — only flag spawn
    // failures (Err from `.status()`), not non-zero exits.
    if let Err(e) = std::process::Command::new("xdg-open")
        .arg(REPO_URL)
        .status()
    {
        warn!(error = %e, "xdg-open failed");
        notify_warn(&format!("xdg-open failed: {e}"));
    }
}

/// Best-effort warning notification. Used when a menu action fails in a way
/// the user would want to know about (e.g. xdg-open not installed).
fn notify_warn(msg: &str) {
    let _ = notify_rust::Notification::new()
        .appname("Lindiction")
        .summary("Lindiction — action failed")
        .body(msg)
        .icon("dialog-warning")
        .timeout(notify_rust::Timeout::Milliseconds(5000))
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lindiction-tray-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn ensure_config_file_creates_empty_when_missing() {
        let dir = fresh_temp_dir("create-empty");
        let path = dir.join("lindiction").join("config.toml");
        assert!(!path.exists(), "precondition: path must not exist");

        super::ensure_config_file_exists(&path).expect("should create");
        assert!(path.exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_config_file_leaves_existing_unchanged() {
        let dir = fresh_temp_dir("leave-existing");
        let config_dir = dir.join("lindiction");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("config.toml");
        let existing_content = "[hotkey]\nbinding = \"f9\"\n";
        std::fs::write(&path, existing_content).unwrap();

        super::ensure_config_file_exists(&path).expect("should no-op");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), existing_content);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_config_file_creates_nested_parents() {
        let dir = fresh_temp_dir("nested-parents");
        let path = dir.join("a").join("b").join("c").join("config.toml");
        assert!(!path.exists());

        super::ensure_config_file_exists(&path).expect("should create nested");
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn icon_name_is_distinct_per_state() {
        let names: Vec<&str> = [
            TrayEvent::Idle,
            TrayEvent::Recording,
            TrayEvent::Processing,
            TrayEvent::Paused,
        ]
        .iter()
        .map(|e| e.icon_name())
        .collect();
        assert_eq!(
            names,
            [
                "audio-input-microphone",
                "media-record",
                "view-refresh",
                "media-playback-pause",
            ]
        );
        // All distinct — if any ever collide, two states would render the
        // same icon and become indistinguishable at a glance.
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "icon names must all be unique");
    }

    #[test]
    fn tooltip_is_distinct_per_state() {
        let all = [
            TrayEvent::Idle,
            TrayEvent::Recording,
            TrayEvent::Processing,
            TrayEvent::Paused,
        ];
        let mut tips: Vec<&str> = all.iter().map(|e| e.tooltip()).collect();
        tips.sort();
        tips.dedup();
        assert_eq!(tips.len(), all.len(), "tooltips must all be unique");
        assert!(TrayEvent::Idle.tooltip().contains("idle"));
        assert!(TrayEvent::Recording.tooltip().contains("recording"));
        assert!(TrayEvent::Paused.tooltip().contains("paused"));
    }

    #[test]
    fn tray_event_is_copy_and_eq() {
        let e = TrayEvent::Recording;
        let f = e; // Copy
        assert_eq!(e, f);
        assert_ne!(TrayEvent::Idle, TrayEvent::Recording);
    }
}
