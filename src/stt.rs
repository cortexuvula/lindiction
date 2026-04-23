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
    beam_size: i32,
    initial_prompt: String,
}

impl SttEngine {
    pub fn load(model_path: &Path, beam_size: u32, initial_prompt: String) -> Result<Self> {
        if !model_path.exists() {
            anyhow::bail!(
                "Model not found: {}. Download with:\n  \
                 curl -L -o ~/.local/share/lindiction/models/ggml-small.en.bin \
                 https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
                model_path.display()
            );
        }
        info!(path = %model_path.display(), beam_size, prompt_len = initial_prompt.len(), "loading whisper model");
        let ctx = WhisperContext::new_with_params(
            model_path
                .to_str()
                .context("model path is not valid UTF-8")?,
            WhisperContextParameters::default(),
        )
        .with_context(|| {
            format!(
                "Failed to load model at {}. File may be corrupt; re-download.",
                model_path.display()
            )
        })?;
        // Clamp to i32 to match whisper-rs's c_int; 1 floor avoids the
        // pathological "beam_size = 0" configuration that would return
        // no tokens at all.
        let beam_size = beam_size.clamp(1, i32::MAX as u32) as i32;
        Ok(Self {
            ctx,
            beam_size,
            initial_prompt,
        })
    }

    /// Transcribe a 16 kHz mono f32 buffer. Blocking; call from `spawn_blocking`.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String> {
        if audio.is_empty() {
            return Ok(String::new());
        }
        // FIXME(v0.2): create_state allocates ~180 MB of compute buffers per call.
        // Fine for PTT (one allocation per utterance, freed after), but a
        // persistent WhisperState would be better for a future streaming mode.
        let mut state = self.ctx.create_state().context("creating whisper state")?;

        let strategy = if self.beam_size > 1 {
            SamplingStrategy::BeamSearch {
                beam_size: self.beam_size,
                // whisper.cpp currently ignores `patience`; 1.0 is the
                // conventional "no adjustment" value.
                patience: 1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        // Hardcoded to English — the default small.en model is English-only.
        // Switching to a multilingual model (ggml-small.bin, etc.) without
        // removing this would force transcription to English regardless of
        // what language was spoken.
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Suppress "..." / "[ ]" hallucinations whisper otherwise emits on
        // silence or noise.
        params.set_suppress_blank(true);
        params.set_suppress_non_speech_tokens(true);
        // Threshold above which whisper declares a segment to be non-speech
        // and emits nothing. 0.6 is the whisper.cpp default but setting
        // explicitly guards against upstream default drift.
        params.set_no_speech_thold(0.6);
        // Each utterance is a fresh create_state()+full() call, so there's
        // no between-utterance context leak to worry about. But *inside*
        // one call, whisper segments audio longer than ~30 s and by default
        // feeds the previous segment's tokens back in as context — which
        // can compound errors on the second half of a long dictation.
        // Disabling it makes each internal segment a clean slate.
        params.set_no_context(true);
        // Deterministic decoding. For beam search this matters only when
        // whisper falls back from beam → sampling on low-confidence
        // segments; 0.0 says "stick with the most probable token" on that
        // fallback too. For greedy (beam_size=1) it's the default.
        params.set_temperature(0.0);
        // Empty initial_prompt means "no bias" — skip the call rather than
        // pushing an empty string through FFI.
        if !self.initial_prompt.is_empty() {
            params.set_initial_prompt(&self.initial_prompt);
        }

        state
            .full(params, audio)
            .context("whisper inference failed")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut out = String::new();
        for i in 0..n {
            out.push_str(&state.full_get_segment_text(i).context("segment text")?);
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
        assert!(!is_non_speech_marker("[hello]")); // lowercase
        assert!(!is_non_speech_marker("[123]")); // digits
        assert!(!is_non_speech_marker("Hello world.")); // no brackets
        assert!(!is_non_speech_marker("[BLANK_AUDIO] extra")); // trailing content
        assert!(!is_non_speech_marker("[]")); // empty brackets
        assert!(!is_non_speech_marker("")); // empty string
    }
}
