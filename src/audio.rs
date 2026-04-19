use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// Owns the underlying `cpal::Stream`; dropping this struct stops capture.
///
/// Note: `cpal::Stream` is `!Send` on Linux (ALSA/PipeWire backends), which
/// makes `AudioStream` `!Send`. It must not be held across `await` points
/// in a `tokio::spawn`'d future. It is safe to hold in the top-level
/// `#[tokio::main]` task.
pub struct AudioStream {
    _stream: cpal::Stream,
}

pub fn start_capture(
    sample_rate: u32,
    channels: u16,
) -> Result<(AudioStream, mpsc::UnboundedReceiver<Vec<f32>>)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default audio input device — check `pactl list sources short`"))?;

    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    info!(device = %device_name, "opening input device");

    let stream_config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // FIXME(v0.2): cpal does not report the actually-negotiated sample rate after
    // `build_input_stream` returns — the backend may silently resample or ignore
    // the request. If Whisper produces garbage output at nominally "16 kHz",
    // check `device.default_input_config()?.sample_rate()` against `sample_rate`.
    debug!(
        sample_rate = stream_config.sample_rate.0,
        channels = stream_config.channels,
        "stream config requested"
    );

    let (tx, rx) = mpsc::unbounded_channel::<Vec<f32>>();

    let stream = device
        .build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Drop the data if the receiver has been closed. Do not
                // block inside the audio callback.
                if tx.send(data.to_vec()).is_err() {
                    debug!("audio receiver dropped; stopping send");
                }
            },
            |err| error!(%err, "cpal stream error"),
            None,
        )
        .context("failed to build cpal input stream")?;

    stream.play().context("failed to start cpal stream")?;

    Ok((AudioStream { _stream: stream }, rx))
}
