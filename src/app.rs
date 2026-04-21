use crate::audio::{start_capture, AudioStream};
use crate::config::Config;
use crate::hotkey::{parse_binding, start as start_hotkey, HotkeyEvent, HotkeyListener};
use crate::inject::Injector;
use crate::model_download;
use crate::postprocess::Postprocessor;
use crate::stt::SttEngine;
use crate::tray::{ControlCmd, TrayEvent, TrayManager};
use crate::update::{self, UpdateInfo};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Result of an update check or install, as delivered to the App's select
/// loop via mpsc. Carrying the result through the loop (rather than firing
/// tray actions directly from the background task) keeps all state
/// mutations in one place.
///
/// `CheckResult` carries an `UpdateInfo` which is string-heavy; clippy
/// flags the size delta between variants. Messages flow at user-click
/// cadence (not per audio frame), so boxing isn't worth the indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum UpdateEvent {
    /// A check completed. Value is Some(info) if an update is available,
    /// None if we're already current. The `manual` flag distinguishes a
    /// user-clicked "Check for updates" from a periodic tick — the former
    /// always gets a result notification, the latter is silent on no-news.
    CheckResult {
        manual: bool,
        result: Result<Option<UpdateInfo>>,
    },
    /// An install job completed. Ok(()) triggers a Restart; Err shows a
    /// notification and leaves the badge visible so the user can retry.
    InstallResult(Result<()>),
}

/// Best-effort desktop notification. Runs on `spawn_blocking` because
/// `notify_rust::Notification::show()` is synchronous DBus and could
/// briefly stall the tokio scheduler otherwise. We don't care about the
/// result — if the notification daemon is unavailable, the tray state
/// change is the fallback signal.
fn notify(summary: &str, body: &str) {
    let summary = summary.to_string();
    let body = body.to_string();
    tokio::task::spawn_blocking(move || {
        let _ = notify_rust::Notification::new()
            .appname("Lindiction")
            .summary(&summary)
            .body(&body)
            .icon("audio-input-microphone")
            .timeout(notify_rust::Timeout::Milliseconds(5000))
            .show();
    });
}

/// How `App::run` wants the process to exit. `main` inspects this and
/// either returns normally (`Quit`) or exec-replaces the current binary
/// (`Restart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitAction {
    Quit,
    Restart,
}

pub struct App;

impl App {
    pub async fn run(config: Config) -> Result<ExitAction> {
        // Preflight: verify xdotool is present before we accept any audio.
        if which::which("xdotool").is_err() {
            anyhow::bail!("xdotool not found on PATH. Install: sudo apt install xdotool");
        }

        let injector = Injector::new(config.xdotool_delay_ms);
        let postprocessor = Postprocessor::new(&config.postprocess)
            .context("building postprocessor from config.postprocess")?;

        // Auto-download the default model on first run (no-op if the file
        // is already present or if the user specified a custom path).
        model_download::ensure_default_model(&config.model.path)
            .context("ensuring default whisper model is available")?;

        let mut tray = TrayManager::start(config.update.enabled);
        tray.set_state(TrayEvent::Idle);

        // One-way signal from the transcription worker to the select loop
        // telling the tray to return to Idle after an utterance finishes.
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(4);

        // Background update-check and install tasks write results back
        // to the select loop via this channel. Size 4 is plenty — checks
        // are rare and results always consumed promptly.
        let (update_evt_tx, mut update_evt_rx) = mpsc::channel::<UpdateEvent>(4);

        // Kick off the startup update check if enabled. Automatic, not
        // manual — silent on "already current."
        if config.update.enabled {
            let tx = update_evt_tx.clone();
            tokio::spawn(async move {
                let result = update::check().await;
                let _ = tx
                    .send(UpdateEvent::CheckResult {
                        manual: false,
                        result,
                    })
                    .await;
            });
        }

        // Periodic update check loop. Only spawned if interval_hours > 0
        // (0 meaning startup-only per config docs). The initial tick
        // fires immediately — we skip it because we just kicked off the
        // startup check above.
        if config.update.enabled && config.update.interval_hours > 0 {
            let tx = update_evt_tx.clone();
            let interval_secs = config.update.interval_hours.saturating_mul(3600);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
                tick.tick().await; // drain the immediately-ready first tick
                loop {
                    tick.tick().await;
                    let result = update::check().await;
                    if tx
                        .send(UpdateEvent::CheckResult {
                            manual: false,
                            result,
                        })
                        .await
                        .is_err()
                    {
                        break; // select loop exited; stop polling
                    }
                }
            });
        }

        // Load the model upfront — fail fast on a bad model path or corrupt file.
        let stt = Arc::new(
            SttEngine::load(&config.model.path)
                .with_context(|| format!("loading model from {}", config.model.path.display()))?,
        );

        // Transcription worker task: reads audio buffers from an mpsc channel
        // and transcribes serially (via spawn_blocking) before injecting.
        // Serial processing guarantees utterances are injected in press-order.
        let (transcribe_tx, mut transcribe_rx) = mpsc::channel::<Vec<f32>>(4);
        let worker = {
            let injector_worker = injector.clone();
            let stt_worker = Arc::clone(&stt);
            let postprocessor_worker = postprocessor.clone();
            let done_tx_worker = done_tx.clone();
            tokio::spawn(async move {
                while let Some(audio) = transcribe_rx.recv().await {
                    let len_seconds = audio.len() as f32 / 16_000.0;
                    debug!(samples = audio.len(), seconds = len_seconds, "transcribing");
                    let stt_for_task = Arc::clone(&stt_worker);
                    let injector_inner = injector_worker.clone();
                    let postprocessor_inner = postprocessor_worker.clone();

                    async {
                        let text = match tokio::task::spawn_blocking(move || {
                            stt_for_task.transcribe(&audio)
                        })
                        .await
                        {
                            Ok(Ok(t)) => t,
                            Ok(Err(e)) => {
                                error!(error = %e, "transcription failed");
                                return;
                            }
                            Err(join) => {
                                error!(error = %join, "transcription task join error");
                                return;
                            }
                        };
                        if text.is_empty() {
                            debug!("empty transcription, nothing to inject");
                            return;
                        }
                        let clean = postprocessor_inner.apply(&text);
                        if clean.trim().is_empty() {
                            debug!(raw = %text, "empty after postprocess, nothing to inject");
                            return;
                        }
                        info!(text = %clean, "injecting");
                        if let Err(e) = injector_inner.inject(&clean).await {
                            // Intentionally omitting `text` to keep potentially sensitive
                            // dictated content out of the log sink. Rerun with -vv and
                            // a test utterance to diagnose xdotool-layer failures.
                            error!(error = %e, "injection failed");
                        }
                    }
                    .await;

                    // Always notify the tray bridge that this utterance is done,
                    // regardless of which skip branch fired above.
                    if done_tx_worker.send(()).await.is_err() {
                        debug!("done channel closed; exiting worker");
                        break;
                    }
                }
            })
        };

        // Hotkey stream
        let (modifiers, code) = parse_binding(&config.hotkey.binding).with_context(|| {
            format!(
                "parsing hotkey binding `{}` from config",
                config.hotkey.binding
            )
        })?;
        let (_hotkey_listener, mut hotkey_rx): (HotkeyListener, _) = start_hotkey(modifiers, code)?;

        // Audio stream
        let (_audio_stream, mut audio_rx): (AudioStream, _) =
            start_capture(config.sample_rate, config.channels)?;

        info!(hotkey = %config.hotkey.binding, "ready — hold the hotkey to dictate");

        let mut recording = false;
        // Ephemeral by design — not persisted across restart. See README
        // "Auto-start on login" / Pause section for the rationale.
        let mut paused = false;
        // Tracks which exit reason, if any, broke the select loop. Default
        // is Quit — reassigned to Restart on the matching tray command.
        let mut exit_action = ExitAction::Quit;
        // After the tray's control channel closes (ksni thread gone / DBus
        // down), an unguarded `recv()` arm would fire `None` on every loop
        // iteration and burn CPU. This flag disables the arm after the
        // first close.
        let mut tray_control_open = true;
        // Latest update check result. Populated by the background checker
        // tasks, read when the user clicks "Update to vX.Y.Z…" so we know
        // which artifacts to fetch.
        let mut latest_update: Option<UpdateInfo> = None;
        // FIXME(v0.2): no upper bound on recording duration. A 5-minute hold
        // accumulates ~19 MB; a 30-minute stuck-hotkey scenario is 115 MB.
        // Consider a max-samples cap that auto-releases with a warn.
        let mut buffer: Vec<f32> = Vec::with_capacity(16_000 * 30);

        loop {
            tokio::select! {
                maybe_evt = hotkey_rx.recv() => match maybe_evt {
                    Some(HotkeyEvent::Press) => {
                        if paused {
                            debug!("press ignored while paused");
                        } else if recording {
                            debug!("duplicate press ignored");
                        } else {
                            // Discard any audio buffered in the channel from before the press.
                            // cpal streams continuously from startup, so chunks pile up in the
                            // unbounded mpsc while `recording` is false (the `if recording`
                            // guard on the audio select arm only stops polling, not production).
                            // Without this drain, every utterance would include all mic input
                            // captured since daemon start (or the previous release), inflating
                            // inference time and potentially capturing unrelated speech.
                            let mut discarded = 0usize;
                            while audio_rx.try_recv().is_ok() {
                                discarded += 1;
                            }
                            if discarded > 0 {
                                debug!(chunks = discarded, "discarded pre-press audio");
                            }
                            recording = true;
                            buffer.clear();
                            info!("recording started");
                            tray.set_state(TrayEvent::Recording);
                        }
                    }
                    Some(HotkeyEvent::Release) => {
                        if paused {
                            debug!("release ignored while paused");
                        } else if !recording {
                            debug!("release without prior press ignored");
                        } else {
                            recording = false;
                            let audio = std::mem::take(&mut buffer);
                            buffer.reserve(16_000 * 30); // restore capacity for the next utterance
                            let seconds = audio.len() as f32 / 16_000.0;
                            info!(seconds, "recording stopped");
                            match transcribe_tx.try_send(audio) {
                                Ok(()) => {
                                    tray.set_state(TrayEvent::Processing);
                                }
                                Err(mpsc::error::TrySendError::Full(dropped)) => {
                                    let s = dropped.len() as f32 / 16_000.0;
                                    warn!(seconds = s, "transcribe queue full, dropping utterance");
                                    tray.set_state(TrayEvent::Idle);
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    error!("transcribe worker closed; shutting down");
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        error!("hotkey channel closed; shutting down");
                        break;
                    }
                },
                maybe_chunk = audio_rx.recv(), if recording => match maybe_chunk {
                    Some(chunk) => buffer.extend_from_slice(&chunk),
                    None => {
                        error!("audio channel closed; shutting down");
                        break;
                    }
                },
                maybe_done = done_rx.recv() => match maybe_done {
                    Some(()) => {
                        // A worker finishing during pause would otherwise
                        // stomp the Paused icon with Idle. Keep the tray
                        // consistent with the authoritative `paused` bool.
                        if paused {
                            debug!("worker finished utterance while paused; keeping Paused icon");
                        } else {
                            debug!("worker finished utterance; tray back to Idle");
                            tray.set_state(TrayEvent::Idle);
                        }
                    }
                    None => {
                        error!("done channel closed; shutting down");
                        break;
                    }
                },
                maybe_cmd = tray.control_signal().recv(), if tray_control_open => match maybe_cmd {
                    Some(ControlCmd::Quit) => {
                        info!("tray Quit activated; shutting down");
                        exit_action = ExitAction::Quit;
                        break;
                    }
                    Some(ControlCmd::Restart) => {
                        info!("tray Restart activated; replacing process image after clean shutdown");
                        exit_action = ExitAction::Restart;
                        break;
                    }
                    Some(ControlCmd::CheckForUpdates) => {
                        let tx = update_evt_tx.clone();
                        tokio::spawn(async move {
                            let result = update::check().await;
                            let _ = tx
                                .send(UpdateEvent::CheckResult {
                                    manual: true,
                                    result,
                                })
                                .await;
                        });
                    }
                    Some(ControlCmd::InstallUpdate) => {
                        let Some(info) = latest_update.clone() else {
                            warn!("InstallUpdate clicked but no pending update; ignoring");
                            continue;
                        };
                        notify(
                            &format!("Downloading lindiction v{}…", info.latest),
                            "You'll be prompted to approve the install.",
                        );
                        let tx = update_evt_tx.clone();
                        tokio::spawn(async move {
                            let result = update::install(&info).await;
                            let _ = tx.send(UpdateEvent::InstallResult(result)).await;
                        });
                    }
                    Some(ControlCmd::TogglePause) => {
                        paused = !paused;
                        if paused {
                            // If the user paused mid-hold, drop the in-flight
                            // buffer rather than transcribing a partial utterance
                            // on resume. "Pause" implies "forget what was happening."
                            if recording {
                                recording = false;
                                buffer.clear();
                                warn!("paused mid-recording; discarding utterance");
                            }
                            info!("paused");
                            tray.set_state(TrayEvent::Paused);
                        } else {
                            info!("resumed");
                            tray.set_state(TrayEvent::Idle);
                        }
                    }
                    None => {
                        // Tray bridge task exited. Daemon can keep running via hotkey,
                        // but this is a surprising state — log, disable the arm so we
                        // don't hot-loop on closed-channel polls, and continue.
                        warn!("tray control channel closed; continuing without tray");
                        tray_control_open = false;
                    }
                },
                maybe_evt = update_evt_rx.recv() => match maybe_evt {
                    Some(UpdateEvent::CheckResult { manual, result }) => {
                        match result {
                            Ok(Some(info)) => {
                                info!(
                                    latest = %info.latest,
                                    current = %info.current,
                                    manual,
                                    "update available"
                                );
                                tray.set_update_available(Some(info.latest.clone()));
                                notify(
                                    &format!("Lindiction v{} available", info.latest),
                                    "Click the tray icon to install.",
                                );
                                latest_update = Some(info);
                            }
                            Ok(None) => {
                                debug!(manual, "no update available");
                                tray.set_update_available(None);
                                latest_update = None;
                                if manual {
                                    notify("Lindiction is up to date", "");
                                }
                            }
                            Err(e) => {
                                // Periodic failures (offline, rate-limited) are
                                // not worth bothering the user about; only the
                                // user-triggered path surfaces them.
                                if manual {
                                    warn!(error = %e, "manual update check failed");
                                    notify(
                                        "Update check failed",
                                        &format!("{e:#}"),
                                    );
                                } else {
                                    debug!(error = %e, "automatic update check failed; will retry on next tick");
                                }
                            }
                        }
                    }
                    Some(UpdateEvent::InstallResult(Ok(()))) => {
                        info!("update install succeeded; triggering Restart");
                        tray.set_update_available(None);
                        // `latest_update` is intentionally NOT cleared — we're
                        // about to break and drop the whole scope.
                        notify(
                            "Update installed",
                            "Lindiction is restarting into the new version.",
                        );
                        exit_action = ExitAction::Restart;
                        break;
                    }
                    Some(UpdateEvent::InstallResult(Err(e))) => {
                        error!(error = %e, "update install failed");
                        notify(
                            "Update failed",
                            &format!("{e:#}"),
                        );
                        // Keep the badge visible so the user can retry.
                    }
                    None => {
                        // All update senders were dropped. We hold one
                        // clone ourselves; this shouldn't happen during
                        // normal operation.
                        debug!("update event channel closed");
                    }
                },
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c received; shutting down");
                    break;
                }
            }
        }

        // Explicitly close the transcribe channel BEFORE awaiting the worker.
        // This is load-bearing: the worker's `while let Some(_) = rx.recv()` loop
        // only exits when all senders are dropped. If we removed this line,
        // NLL would drop `transcribe_tx` at end-of-scope — AFTER `worker.await` —
        // and worker.await would deadlock. Non-negotiable ordering.
        drop(transcribe_tx);
        // worker.await may block up to ~800 ms if a whisper spawn_blocking call
        // is in flight at shutdown. This is intentional: we let the current
        // inference finish rather than leaking a blocking thread. Restart
        // paths rely on this invariant — an execve replacement after an
        // incomplete inference would silently drop the utterance.
        let _ = worker.await;
        Ok(exit_action)
    }
}
