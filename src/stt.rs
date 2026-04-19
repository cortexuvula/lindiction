use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// whisper-rs 0.11 marks WhisperContext as both Send and Sync (see
// whisper_rs::whisper_ctx::{unsafe impl Send, unsafe impl Sync}). That makes
// SttEngine itself Send + Sync, so Arc<SttEngine> is enough — no Mutex — for
// the transcription worker in app.rs.
pub struct SttEngine {
    ctx: WhisperContext,
}

impl SttEngine {
    pub fn load(model_path: &Path) -> Result<Self> {
        if !model_path.exists() {
            anyhow::bail!(
                "Model not found: {}. Download with:\n  \
                 curl -L -o models/ggml-tiny.en.bin \
                 https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
                model_path.display()
            );
        }
        info!(path = %model_path.display(), "loading whisper model");
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().context("model path is not valid UTF-8")?,
            WhisperContextParameters::default(),
        )
        .with_context(|| {
            format!(
                "Failed to load model at {}. File may be corrupt; re-download.",
                model_path.display()
            )
        })?;
        Ok(Self { ctx })
    }

    /// Transcribe a 16 kHz mono f32 buffer. Blocking; call from `spawn_blocking`.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String> {
        if audio.is_empty() {
            return Ok(String::new());
        }
        // FIXME(v0.2): create_state allocates ~180 MB of compute buffers per call.
        // Fine for PTT (one allocation per utterance, freed after), but a
        // persistent WhisperState would be better for a future streaming mode.
        let mut state = self
            .ctx
            .create_state()
            .context("creating whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // FIXME(v0.2): language is hardcoded for the English-only tiny.en model.
        // A multilingual model (ggml-tiny.bin, ggml-base.bin, etc.) would ignore
        // the speaker's actual language. Move to Config when we ship a config file.
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, audio)
            .context("whisper inference failed")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut out = String::new();
        for i in 0..n {
            out.push_str(
                &state
                    .full_get_segment_text(i)
                    .context("segment text")?,
            );
        }
        let trimmed = out.trim();
        if is_non_speech_marker(trimmed) {
            return Ok(String::new());
        }
        Ok(trimmed.to_string())
    }
}

/// Whisper.cpp emits all-caps bracketed markers like `[BLANK_AUDIO]`,
/// `[SILENCE]`, `[NOISE]`, `[MUSIC]`, etc. when the audio contains no
/// intelligible speech. For dictation, we treat these as equivalent to
/// empty output so they never reach the injector.
fn is_non_speech_marker(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'[' || bytes[bytes.len() - 1] != b']' {
        return false;
    }
    // Interior must be entirely uppercase letters, underscores, or spaces.
    // Reject anything else (digits, mixed case, punctuation) — that's likely
    // real speech that happens to start with a bracket.
    s[1..s.len() - 1]
        .chars()
        .all(|c| c.is_ascii_uppercase() || c == '_' || c == ' ')
}

#[cfg(test)]
mod tests {
    use super::is_non_speech_marker;

    #[test]
    fn marker_filter_catches_known_whisper_sentinels() {
        assert!(is_non_speech_marker("[BLANK_AUDIO]"));
        assert!(is_non_speech_marker("[SILENCE]"));
        assert!(is_non_speech_marker("[NOISE]"));
        assert!(is_non_speech_marker("[MUSIC]"));
        assert!(is_non_speech_marker("[APPLAUSE]"));
        assert!(is_non_speech_marker("[INAUDIBLE SPEECH]"));
    }

    #[test]
    fn marker_filter_preserves_real_speech() {
        assert!(!is_non_speech_marker("[hello]"));             // lowercase
        assert!(!is_non_speech_marker("[123]"));                // digits
        assert!(!is_non_speech_marker("Hello world."));         // no brackets
        assert!(!is_non_speech_marker("[BLANK_AUDIO] extra"));  // trailing content
        assert!(!is_non_speech_marker("[]"));                   // empty brackets
        assert!(!is_non_speech_marker(""));                     // empty string
    }
}
