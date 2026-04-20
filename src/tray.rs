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
        vec![StandardItem {
            label: "Quit".into(),
            activate: Box::new(|this: &mut Self| {
                let _ = this.shutdown_tx.try_send(());
            }),
            ..Default::default()
        }
        .into()]
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

#[cfg(test)]
mod tests {
    use super::*;

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
