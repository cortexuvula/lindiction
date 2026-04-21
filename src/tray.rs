use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Three visual states that the tray icon can display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Idle,
    Recording,
    Processing,
}

impl TrayEvent {
    /// Freedesktop theme-icon name for this state. These live in every
    /// modern icon theme; we avoid shipping our own PNG assets in v0.3.
    pub fn icon_name(self) -> &'static str {
        match self {
            TrayEvent::Idle => "audio-input-microphone",
            TrayEvent::Recording => "media-record",
            TrayEvent::Processing => "view-refresh",
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
        }
    }
}

/// Internal ksni tray implementation. The `state` field is mutated
/// from an async task via `ksni::Handle::update`.
struct LindictionTray {
    state: TrayEvent,
    shutdown_tx: mpsc::Sender<()>,
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
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open config\u{2026}".into(),
                activate: Box::new(|_: &mut Self| open_config()),
                ..Default::default()
            }
            .into(),
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
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.shutdown_tx.try_send(());
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Public façade around the ksni service. Lives for the duration of the
/// daemon. Dropping it calls `Handle::shutdown()` in the `Drop` impl,
/// which stops the ksni background thread and releases the DBus name.
pub struct TrayManager {
    state_tx: mpsc::UnboundedSender<TrayEvent>,
    shutdown_rx: mpsc::Receiver<()>,
    handle: ksni::Handle<LindictionTray>,
}

impl TrayManager {
    /// Register the tray icon on the session bus. Returns a manager that
    /// the main app uses to push state events and listen for Quit.
    ///
    /// Non-fatal on failure: if the tray cannot be registered (e.g. no
    /// DBus session, or a StatusNotifier host is not present), this
    /// returns a manager whose `set_state` is a no-op and whose
    /// shutdown channel never fires. The daemon still works via hotkey.
    pub fn start() -> Self {
        let (state_tx, mut state_rx) = mpsc::unbounded_channel::<TrayEvent>();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

        let tray = LindictionTray {
            state: TrayEvent::Idle,
            shutdown_tx,
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
            shutdown_rx,
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

    /// Main app awaits this to learn when the user picked Quit from
    /// the tray menu.
    pub fn shutdown_signal(&mut self) -> &mut mpsc::Receiver<()> {
        &mut self.shutdown_rx
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        // Tell the ksni background thread to stop its polling loop and
        // release the DBus name cleanly.
        self.handle.shutdown();
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
        let names: Vec<&str> = [TrayEvent::Idle, TrayEvent::Recording, TrayEvent::Processing]
            .iter()
            .map(|e| e.icon_name())
            .collect();
        assert_eq!(
            names,
            ["audio-input-microphone", "media-record", "view-refresh"]
        );
    }

    #[test]
    fn tooltip_is_distinct_per_state() {
        assert_ne!(TrayEvent::Idle.tooltip(), TrayEvent::Recording.tooltip());
        assert_ne!(
            TrayEvent::Recording.tooltip(),
            TrayEvent::Processing.tooltip()
        );
        assert!(TrayEvent::Idle.tooltip().contains("idle"));
        assert!(TrayEvent::Recording.tooltip().contains("recording"));
    }

    #[test]
    fn tray_event_is_copy_and_eq() {
        let e = TrayEvent::Recording;
        let f = e; // Copy
        assert_eq!(e, f);
        assert_ne!(TrayEvent::Idle, TrayEvent::Recording);
    }
}
