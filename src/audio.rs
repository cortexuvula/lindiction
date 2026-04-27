use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Decide whether `wanted_rate` Hz at `wanted_channels` channels is in
/// any of the device's supported config ranges.
///
/// Extracted as a pure function so we can unit-test the decision logic
/// without a real audio device. The real `start_capture` maps cpal's
/// SupportedStreamConfigRange into the (channels, min_rate, max_rate)
/// triples this function consumes.
fn rate_supported(configs: &[(u16, u32, u32)], wanted_rate: u32, wanted_channels: u16) -> bool {
    configs.iter().any(|&(channels, min_rate, max_rate)| {
        channels == wanted_channels && min_rate <= wanted_rate && wanted_rate <= max_rate
    })
}

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
    device_name: Option<&str>,
) -> Result<(AudioStream, mpsc::UnboundedReceiver<Vec<f32>>)> {
    let host = cpal::default_host();

    // If the user pinned a specific device, try to find it by exact
    // cpal-level name. On miss, warn and fall back to the system default
    // — pinning a now-absent USB mic shouldn't kill the daemon.
    let device = match device_name {
        Some(requested) => {
            let found = host
                .input_devices()
                .ok()
                .and_then(|mut iter| iter.find(|d| d.name().ok().as_deref() == Some(requested)));
            match found {
                Some(d) => d,
                None => {
                    warn!(
                        requested = requested,
                        "configured input device not found; falling back to system default"
                    );
                    host.default_input_device().ok_or_else(|| {
                        anyhow!(
                            "no default audio input device — check `pactl list sources short`"
                        )
                    })?
                }
            }
        }
        None => host.default_input_device().ok_or_else(|| {
            anyhow!("no default audio input device — check `pactl list sources short`")
        })?,
    };

    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    info!(device = %device_name, "opening input device");

    let supported: Vec<(u16, u32, u32)> = device
        .supported_input_configs()
        .context("enumerating supported input configs")?
        .map(|c| (c.channels(), c.min_sample_rate().0, c.max_sample_rate().0))
        .collect();

    if !rate_supported(&supported, sample_rate, channels) {
        let default_desc = device
            .default_input_config()
            .ok()
            .map(|c| format!("{} Hz @ {} channels", c.sample_rate().0, c.channels()))
            .unwrap_or_else(|| "<unavailable>".into());
        warn!(?supported, "audio device does not advertise the requested rate/channels");
        return Err(anyhow!(
            "audio device `{device_name}` does not support {sample_rate} Hz @ {channels} channel(s). \
             Device default: {default_desc}. Whisper requires 16 kHz mono — try selecting a \
             different input device (e.g. `pactl set-default-source <name>` on PulseAudio/PipeWire, \
             or picking a different device with `arecord -L` on ALSA-only systems)."
        ));
    }

    let stream_config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    info!(
        sample_rate = stream_config.sample_rate.0,
        channels = stream_config.channels,
        "stream config verified"
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

/// One row in the tray's "Microphone" submenu.
#[derive(Debug, Clone)]
pub struct InputDeviceInfo {
    /// cpal-level name. Stored verbatim in config.toml when the user
    /// picks this device. Treat as opaque — it's whatever ALSA / PA /
    /// PipeWire's underlying name turned out to be (e.g.
    /// "alsa_input.pci-0000_08_00.4.analog-stereo", "default",
    /// "pipewire", "usbstream:CARD=C920").
    pub name: String,
    /// True if this device matches the system default. Used to mark
    /// the System default menu item appropriately.
    pub is_system_default: bool,
}

/// Enumerate input devices with the heuristic filter applied.
/// Pure with respect to its return value (calls cpal once per
/// invocation); intended to be called when the tray menu opens.
/// Quiet on probe errors — devices that fail to name themselves are
/// dropped from the result.
pub fn list_input_devices() -> Vec<InputDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let Ok(iter) = host.input_devices() else {
        return Vec::new();
    };
    iter.filter_map(|d| {
        let name = d.name().ok()?;
        if !is_useful_input_name(&name) {
            return None;
        }
        let is_system_default = default_name.as_deref() == Some(name.as_str());
        Some(InputDeviceInfo {
            name,
            is_system_default,
        })
    })
    .collect()
}

/// Heuristic filter to keep only "interesting" input devices, hiding
/// the dozens of ALSA virtual aliases that cpal's Linux backend
/// surfaces. Public-but-test-only — exists so unit tests can pin down
/// the filter behavior.
fn is_useful_input_name(name: &str) -> bool {
    // Reject ALSA virtual / converter / format-shim devices.
    const REJECTED_PREFIXES: &[&str] = &[
        "null",
        "samplerate",
        "speexrate",
        "upmix",
        "vdownmix",
        "dmix:",
        "dsnoop:",
        "surround",
        "hdmi:",
        "modem:",
        "front:",
        "rear:",
        "center_lfe:",
        "side:",
        "iec958:",
        // Raw hw:/plughw: devices duplicate the higher-level entries
        // (e.g. usbstream:CARD=C920 vs hw:CARD=C920,DEV=0). Hide
        // the raw forms.
        "hw:",
        "plughw:",
    ];
    if REJECTED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return false;
    }
    // Specific exact-match rejects.
    !matches!(
        name,
        "front" | "sysdefault" | "samplerate" | "rear" | "center_lfe" | "side" | "iec958"
    )
}

#[cfg(test)]
mod tests {
    use super::is_useful_input_name;
    use super::rate_supported;

    #[test]
    fn supported_exact_match() {
        let configs = [(1u16, 8_000u32, 48_000u32)];
        assert!(rate_supported(&configs, 16_000, 1));
    }

    #[test]
    fn supported_upper_bound_inclusive() {
        let configs = [(1u16, 8_000u32, 16_000u32)];
        assert!(rate_supported(&configs, 16_000, 1));
    }

    #[test]
    fn supported_lower_bound_inclusive() {
        let configs = [(1u16, 16_000u32, 48_000u32)];
        assert!(rate_supported(&configs, 16_000, 1));
    }

    #[test]
    fn rejects_rate_outside_range() {
        // Device that only supports 44.1 / 48 kHz mono — common for Bluetooth
        // headsets that drop down to only SCO's 8/16 kHz in call mode but
        // report their hi-fi range in profile mode.
        let configs = [(1u16, 44_100u32, 48_000u32)];
        assert!(!rate_supported(&configs, 16_000, 1));
    }

    #[test]
    fn rejects_mismatched_channels() {
        // Stereo-only mic in the supported list — must not match a mono request.
        let configs = [(2u16, 8_000u32, 48_000u32)];
        assert!(!rate_supported(&configs, 16_000, 1));
    }

    #[test]
    fn matches_first_of_multiple_ranges() {
        // Device with two entries: one stereo, one mono. Mono entry is what
        // we care about for a mono request.
        let configs = [
            (2u16, 44_100u32, 48_000u32),
            (1u16, 8_000u32, 48_000u32),
        ];
        assert!(rate_supported(&configs, 16_000, 1));
    }

    #[test]
    fn empty_list_rejects() {
        assert!(!rate_supported(&[], 16_000, 1));
    }

    #[test]
    fn keeps_useful_names() {
        for name in [
            "default",
            "pulse",
            "pipewire",
            "alsa_input.pci-0000_08_00.4.analog-stereo",
            "alsa_input.usb-046d_HD_Pro_Webcam_C920_CAC719AF-02.analog-stereo",
            "usbstream:CARD=C920",
        ] {
            assert!(is_useful_input_name(name), "should keep: {name}");
        }
    }

    #[test]
    fn rejects_alsa_virtual_aliases() {
        for name in [
            "null",
            "samplerate",
            "upmix",
            "vdownmix",
            "dmix:CARD=Generic,DEV=0",
            "dsnoop:CARD=C920,DEV=0",
            "surround51:CARD=Generic",
            "hdmi:CARD=NVidia,DEV=0",
            "modem:CARD=Generic",
            "front:CARD=Generic,DEV=0",
            "iec958:CARD=NVidia,DEV=0",
            "hw:CARD=C920,DEV=0",
            "plughw:CARD=Generic,DEV=0",
            "front",
            "sysdefault",
            "iec958",
        ] {
            assert!(!is_useful_input_name(name), "should reject: {name}");
        }
    }
}
