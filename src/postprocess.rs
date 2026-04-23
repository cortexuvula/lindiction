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
    /// User-defined [from, to] replacements. Precompiled at construction
    /// so apply() stays cheap.
    replacements: Vec<(Regex, String)>,
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

        // Compile each user replacement to a case-insensitive regex.
        // Word boundaries are added only on edges that are actually word
        // characters — so "clod" → \bclod\b (no match inside
        // "clodhopper"), but "c++" → \bc\+\+ (no trailing \b, which
        // would never match after `+`). Empty `from` entries are silently
        // skipped: a `\b\b` regex matches every word boundary and would
        // turn the transcript into a wall of `to`s.
        let mut replacements: Vec<(Regex, String)> = Vec::with_capacity(cfg.replacements.len());
        for pair in &cfg.replacements {
            let from = pair[0].trim();
            if from.is_empty() {
                continue;
            }
            let escaped = regex::escape(from);
            let first = from.chars().next().expect("non-empty");
            let last = from.chars().last().expect("non-empty");
            let lead = if is_word_char(first) { "\\b" } else { "" };
            let trail = if is_word_char(last) { "\\b" } else { "" };
            let pattern = format!("(?i){}{}{}", lead, escaped, trail);
            let re = Regex::new(&pattern)
                .with_context(|| format!("compiling replacement regex for `{}`", from))?;
            replacements.push((re, pair[1].clone()));
        }

        Ok(Self {
            filler_regex,
            collapse_whitespace,
            space_before_terminal_punct,
            leading_stranded_punct,
            capitalize_sentences: cfg.capitalize_sentences,
            ensure_trailing_period: cfg.ensure_trailing_period,
            replacements,
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

        // 4b. Apply user replacements (case-insensitive, word-bounded).
        // Runs after capitalization so the replacement's casing is what
        // the user wrote in config — e.g. "clod" → "Claude" keeps the
        // capital C even at sentence start. Runs before trailing-period
        // so replacements that add/remove punctuation don't desync the
        // terminal-period check.
        for (re, to) in &self.replacements {
            s = re.replace_all(&s, to.as_str()).to_string();
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

/// `\b` in the regex crate matches between `\w` and `\W`, where `\w`
/// is ASCII `[A-Za-z0-9_]`. Mirror that exactly.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
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
            replacements: vec![],
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
            replacements: vec![],
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
            replacements: vec![],
        };
        let p = Postprocessor::new(&cfg).unwrap();
        // Note: without leading-punct stripping, the comma stays,
        // but `h` is still capitalized by the fixed walker.
        // However `leading_stranded_punct` IS applied regardless
        // of `remove_fillers`, so the comma gets stripped here too.
        assert_eq!(p.apply(", hello world"), "Hello world.");
    }

    fn cfg_with_replacements(pairs: &[(&str, &str)]) -> PostprocessConfig {
        PostprocessConfig {
            replacements: pairs
                .iter()
                .map(|(a, b)| [a.to_string(), b.to_string()])
                .collect(),
            ..PostprocessConfig::default()
        }
    }

    #[test]
    fn replacement_simple_word() {
        let p = Postprocessor::new(&cfg_with_replacements(&[("clod", "Claude")])).unwrap();
        assert_eq!(p.apply("ask clod to help"), "Ask Claude to help.");
    }

    #[test]
    fn replacement_is_case_insensitive_but_preserves_target_casing() {
        let p = Postprocessor::new(&cfg_with_replacements(&[("clod", "Claude")])).unwrap();
        // Matches "Clod" (capitalized by sentence-cap step) AND "CLOD",
        // but the output is always the verbatim "Claude" from config.
        assert_eq!(p.apply("clod says hi"), "Claude says hi.");
        assert_eq!(p.apply("CLOD says hi"), "Claude says hi.");
    }

    #[test]
    fn replacement_respects_word_boundary() {
        // "clod" should match the standalone word but not occurrences
        // inside longer words like "clodhopper".
        let p = Postprocessor::new(&cfg_with_replacements(&[("clod", "Claude")])).unwrap();
        assert_eq!(p.apply("clodhopper shoes"), "Clodhopper shoes.");
    }

    #[test]
    fn replacement_handles_multi_word_from() {
        let p =
            Postprocessor::new(&cfg_with_replacements(&[("lin dictation", "lindiction")])).unwrap();
        assert_eq!(p.apply("lin dictation is great"), "lindiction is great.");
    }

    #[test]
    fn replacement_escapes_regex_metachars_in_from() {
        // "c++" would otherwise be a regex error (unmatched quantifier).
        let p = Postprocessor::new(&cfg_with_replacements(&[("c++", "cpp")])).unwrap();
        assert_eq!(p.apply("i write c++ code"), "I write cpp code.");
    }

    #[test]
    fn replacement_empty_from_is_skipped() {
        // An empty `from` would compile to \b\b and match every word
        // boundary; we silently ignore those entries instead.
        let p = Postprocessor::new(&cfg_with_replacements(&[
            ("", "nope"),
            ("   ", "nope"),
            ("clod", "Claude"),
        ]))
        .unwrap();
        assert_eq!(p.apply("hello clod world"), "Hello Claude world.");
    }

    #[test]
    fn replacement_runs_in_order_with_chaining() {
        // Second replacement operates on the output of the first —
        // documented behavior.
        let p = Postprocessor::new(&cfg_with_replacements(&[
            ("alpha", "beta"),
            ("beta", "gamma"),
        ]))
        .unwrap();
        assert_eq!(p.apply("one alpha two"), "One gamma two.");
    }

    #[test]
    fn replacement_disabled_by_default() {
        let p = Postprocessor::new(&PostprocessConfig::default()).unwrap();
        assert_eq!(p.apply("clod says hi"), "Clod says hi.");
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
            replacements: vec![],
        };
        let p = Postprocessor::new(&cfg).unwrap();
        // leading_stranded_punct DOES fire even in raw mode — it's part
        // of the normal pipeline, not gated on any toggle. This is
        // intentional: leading "," from whisper is never desirable.
        assert_eq!(p.apply(", hello world"), "hello world");
    }
}
