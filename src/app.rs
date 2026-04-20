use crate::audio::{start_capture, AudioStream};
use crate::config::Config;
use crate::hotkey::{parse_binding, start as start_hotkey, HotkeyEvent, HotkeyListener};
use crate::inject::Injector;
use crate::stt::SttEngine;
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub struct App;

impl App {
    pub async fn run(config: Config) -> Result<()> {
        // Preflight: verify xdotool is present before we accept any audio.
        if which::which("xdotool").is_err() {
            anyhow::bail!("xdotool not found on PATH. Install: sudo apt install xdotool");
        }

        let injector = Injector::new(config.xdotool_delay_ms);

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
            tokio::spawn(async move {
                while let Some(audio) = transcribe_rx.recv().await {
                    let len_seconds = audio.len() as f32 / 16_000.0;
                    debug!(samples = audio.len(), seconds = len_seconds, "transcribing");
                    let stt_for_task = Arc::clone(&stt_worker);
                    let text =
                        match tokio::task::spawn_blocking(move || stt_for_task.transcribe(&audio))
                            .await
                        {
                            Ok(Ok(t)) => t,
                            Ok(Err(e)) => {
                                error!(error = %e, "transcription failed");
                                continue;
                            }
                            Err(join) => {
                                error!(error = %join, "transcription task join error");
                                continue;
                            }
                        };
                    if text.is_empty() {
                        debug!("empty transcription, nothing to inject");
                        continue;
                    }
                    info!(text = %text, "injecting");
                    if let Err(e) = injector_worker.inject(&text).await {
                        // Intentionally omitting `text` to keep potentially sensitive
                        // dictated content out of the log sink. Rerun with -vv and
                        // a test utterance to diagnose xdotool-layer failures.
                        error!(error = %e, "injection failed");
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
        // FIXME(v0.2): no upper bound on recording duration. A 5-minute hold
        // accumulates ~19 MB; a 30-minute stuck-hotkey scenario is 115 MB.
        // Consider a max-samples cap that auto-releases with a warn.
        let mut buffer: Vec<f32> = Vec::with_capacity(16_000 * 30);

        loop {
            tokio::select! {
                maybe_evt = hotkey_rx.recv() => match maybe_evt {
                    Some(HotkeyEvent::Press) => {
                        if recording {
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
                        }
                    }
                    Some(HotkeyEvent::Release) => {
                        if !recording {
                            debug!("release without prior press ignored");
                        } else {
                            recording = false;
                            let audio = std::mem::take(&mut buffer);
                            buffer.reserve(16_000 * 30); // restore capacity for the next utterance
                            let seconds = audio.len() as f32 / 16_000.0;
                            info!(seconds, "recording stopped");
                            match transcribe_tx.try_send(audio) {
                                Ok(()) => {},
                                Err(mpsc::error::TrySendError::Full(dropped)) => {
                                    let s = dropped.len() as f32 / 16_000.0;
                                    warn!(seconds = s, "transcribe queue full, dropping utterance");
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
        // inference finish rather than leaking a blocking thread.
        let _ = worker.await;
        Ok(())
    }
}
