use crate::config::PostprocessConfig;
use anyhow::{Context, Result};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct Postprocessor {
    filler_regex: Option<Regex>,
    collapse_whitespace: Regex,
    space_before_terminal_punct: Regex,
    leading_stranded_punct: Regex,
    capitalize_sentences: bool,
    ensure_trailing_period: bool,
}

impl Postprocessor {
    pub fn new(cfg: &PostprocessConfig) -> Result<Self> {
        let filler_regex = if cfg.remove_fillers {
            let escaped: Vec<String> = cfg
                .filler_words
                .iter()
                .filter(|w| !w.trim().is_empty())
                .map(|w| regex::escape(w))
                .collect();
            if escaped.is_empty() {
                None
            } else {
                let pattern = format!("(?i)\\b({})\\b", escaped.join("|"));
                Some(Regex::new(&pattern).context("compiling filler-words regex")?)
            }
        } else {
            None
        };
        let collapse_whitespace = Regex::new(r"\s+").expect("static regex compiles");
        let space_before_terminal_punct = Regex::new(r"\s+([.?!])").expect("static regex compiles");
        let leading_stranded_punct = Regex::new(r"^[\s,;:]+").expect("static regex compiles");
        Ok(Self {
            filler_regex,
            collapse_whitespace,
            space_before_terminal_punct,
            leading_stranded_punct,
            capitalize_sentences: cfg.capitalize_sentences,
            ensure_trailing_period: cfg.ensure_trailing_period,
        })
    }

    pub fn apply(&self, text: &str) -> String {
        // 1. Trim.
        let mut s = text.trim().to_string();

        // 2. Remove filler words if configured.
        if let Some(re) = &self.filler_regex {
            s = re.replace_all(&s, "").to_string();
        }

        // 3. Collapse runs of whitespace into single spaces and re-trim.
        s = self
            .collapse_whitespace
            .replace_all(&s, " ")
            .trim()
            .to_string();

        // 3b. Strip whitespace between words and terminal punctuation — fixes
        // "hello uh." → "hello ." after filler removal.
        s = self
            .space_before_terminal_punct
            .replace_all(&s, "$1")
            .to_string();

        // 3c. Strip leading whitespace + stranded sentence-internal punctuation
        // (",;:") — fixes "Um, hello" → ", hello" → "hello" after filler removal.
        s = self.leading_stranded_punct.replace_all(&s, "").to_string();

        // 4. Capitalize sentence-initial letters.
        if self.capitalize_sentences {
            s = capitalize_sentences(&s);
        }

        // 5. Append trailing period if none of `. ? !` terminate.
        if self.ensure_trailing_period && !s.is_empty() {
            let last = s.chars().last().expect("non-empty");
            if !matches!(last, '.' | '?' | '!') {
                s.push('.');
            }
        }

        s
    }
}

fn capitalize_sentences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if capitalize_next && c.is_ascii_alphabetic() {
            out.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
            // Only re-arm on sentence-ending punctuation. Non-alphabetic
            // characters (commas, colons, whitespace) neither capitalize
            // nor disarm the flag — they pass through transparently so
            // the next alphabetic character gets capitalized.
            if matches!(c, '.' | '?' | '!') {
                capitalize_next = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PostprocessConfig;

    fn default_cfg() -> PostprocessConfig {
        PostprocessConfig::default()
    }

    fn raw_cfg() -> PostprocessConfig {
        PostprocessConfig {
            remove_fillers: false,
            filler_words: vec![],
            capitalize_sentences: false,
            ensure_trailing_period: false,
        }
    }

    #[test]
    fn trims_whitespace_and_adds_period() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("  hello  "), "Hello.");
    }

    #[test]
    fn removes_fillers() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("um hello uh world"), "Hello world.");
    }

    #[test]
    fn capitalizes_first_letter() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello"), "Hello.");
    }

    #[test]
    fn capitalizes_after_sentence_punctuation() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello. world"), "Hello. World.");
    }

    #[test]
    fn idempotent_trailing_period() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello."), "Hello.");
    }

    #[test]
    fn preserves_question_and_exclamation_endings() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello?"), "Hello?");
        assert_eq!(p.apply("hello!"), "Hello!");
    }

    #[test]
    fn all_toggles_off_returns_trimmed_input() {
        let p = Postprocessor::new(&raw_cfg()).unwrap();
        assert_eq!(p.apply("  um hello  "), "um hello");
    }

    #[test]
    fn all_filler_input_returns_empty() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("um uh"), "");
    }

    #[test]
    fn preserves_all_caps_mid_sentence() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("HELLO WORLD"), "HELLO WORLD.");
    }

    #[test]
    fn filler_removal_is_case_insensitive() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("UM hello UH world"), "Hello world.");
    }

    #[test]
    fn filler_removal_respects_word_boundary() {
        // "umbrella" starts with "um" but shouldn't be stripped.
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("umbrella"), "Umbrella.");
    }

    #[test]
    fn multi_word_filler_is_removed() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("you know hello"), "Hello.");
    }

    #[test]
    fn empty_filler_entries_are_ignored() {
        let cfg = PostprocessConfig {
            remove_fillers: true,
            filler_words: vec![
                "um".to_string(),
                "".to_string(),
                "   ".to_string(),
                "uh".to_string(),
            ],
            capitalize_sentences: true,
            ensure_trailing_period: true,
        };
        let p = Postprocessor::new(&cfg).unwrap();
        // Regex must not treat the empty/whitespace entries as matches.
        assert_eq!(p.apply("um hello uh world"), "Hello world.");
        // A plain "hello" must not be mangled by a phantom `\b()\b` match.
        assert_eq!(p.apply("hello"), "Hello.");
    }

    #[test]
    fn filler_before_terminal_punct_removes_extra_space() {
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("hello uh."), "Hello.");
        assert_eq!(p.apply("what um?"), "What?");
        assert_eq!(p.apply("go uh!"), "Go!");
    }

    #[test]
    fn filler_with_trailing_comma_is_cleaned() {
        // Whisper commonly outputs "Um, hello world." with a comma
        // attached to the filler. Both the filler AND the stranded
        // comma should be removed.
        let p = Postprocessor::new(&default_cfg()).unwrap();
        assert_eq!(p.apply("Um, hello world."), "Hello world.");
        assert_eq!(p.apply("uh, hello"), "Hello.");
        assert_eq!(p.apply("so; hello world"), "Hello world.");
    }

    #[test]
    fn capitalizes_past_leading_punctuation_without_filler_strip() {
        // Even with filler removal disabled, a raw leading comma
        // shouldn't prevent the first alpha from being capitalized.
        let cfg = PostprocessConfig {
            remove_fillers: false,
            filler_words: vec![],
            capitalize_sentences: true,
            ensure_trailing_period: true,
        };
        let p = Postprocessor::new(&cfg).unwrap();
        // Note: without leading-punct stripping, the comma stays,
        // but `h` is still capitalized by the fixed walker.
        // However `leading_stranded_punct` IS applied regardless
        // of `remove_fillers`, so the comma gets stripped here too.
        assert_eq!(p.apply(", hello world"), "Hello world.");
    }

    #[test]
    fn raw_mode_still_strips_leading_garbage() {
        // With all postprocess toggles off, leading/trailing whitespace
        // still gets trimmed (trim is unconditional), but commas stay.
        let cfg = PostprocessConfig {
            remove_fillers: false,
            filler_words: vec![],
            capitalize_sentences: false,
            ensure_trailing_period: false,
        };
        let p = Postprocessor::new(&cfg).unwrap();
        // leading_stranded_punct DOES fire even in raw mode — it's part
        // of the normal pipeline, not gated on any toggle. This is
        // intentional: leading "," from whisper is never desirable.
        assert_eq!(p.apply(", hello world"), "hello world");
    }
}
