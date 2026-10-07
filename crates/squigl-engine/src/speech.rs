//! Reading aloud: the [`Voice`] a front end may have (squigl-speech's is the
//! system's speech through the `tts` crate, with maths in words by MathCAT), and a
//! reading turned into what it says -- a sentence at a time, so speech can be
//! paused between sentences (no system voice pauses mid-word everywhere), the
//! sentence being said can be shown, and maths is said in words rather than as
//! LaTeX.

use crate::math::{self, Part};

/// A voice the system offers.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VoiceInfo {
    /// What [`crate::config::SpeechConfig::voice`] holds.
    pub id: String,
    pub name: String,
    /// BCP 47, when the system says.
    pub language: Option<String>,
}

/// Speaks one utterance at a time. Implemented outside the engine (the system's
/// speech is a native library); `ended` is how the engine learns an utterance is
/// over, and the voice calls the engine's waker when it is.
pub trait Voice: Send {
    /// Starts saying `text`, cutting off anything being said.
    fn say(&mut self, id: u64, text: &str) -> anyhow::Result<()>;
    /// Stops at once; the utterance being said counts as ended.
    fn stop(&mut self);
    /// The id of the newest utterance that has ended (finished or stopped); 0 before
    /// any has.
    fn ended(&self) -> u64;
    fn voices(&self) -> Vec<VoiceInfo>;
    /// The voice (`None`: the system's default) and the rate, as a multiple of
    /// normal speed.
    fn configure(&mut self, voice: Option<&str>, rate: f32) -> anyhow::Result<()>;
    /// MathML in words, or `None` when the voice has no way to say maths.
    fn maths(&self, mathml: &str) -> Option<String>;
}

/// One sentence of a reading, as said.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub text: String,
    /// Where it is in the reading's text, in UTF-16 units (as [`math::Part`]).
    pub start: usize,
    pub end: usize,
}

/// `text` as sentences to say: split after `.`, `!` or `?` followed by a space, at
/// line breaks, and around display maths; maths said by `maths` (given MathML in
/// the form MathCAT reads best, [`math::speakable`]) or, failing that, its source
/// without the delimiters.
pub fn utterances(text: &str, maths: impl Fn(&str) -> Option<String>) -> Vec<Utterance> {
    let mut out: Vec<Utterance> = Vec::new();
    let mut said = String::new();
    let mut start: Option<usize> = None;
    let mut end = 0;
    let mut flush = |said: &mut String, start: &mut Option<usize>, end: usize| {
        let text = said.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(s) = start.take() {
            if !text.is_empty() {
                out.push(Utterance {
                    text,
                    start: s,
                    end,
                });
            }
        }
        said.clear();
    };
    for part in math::parts(text) {
        match part {
            Part::Text {
                text: t, start: s, ..
            } => {
                let mut at = s;
                let mut chars = t.chars().peekable();
                while let Some(c) = chars.next() {
                    let width = c.len_utf16();
                    if c == '\n' {
                        flush(&mut said, &mut start, end);
                        at += width;
                        continue;
                    }
                    if start.is_none() && !c.is_whitespace() {
                        start = Some(at);
                    }
                    said.push(c);
                    at += width;
                    if !c.is_whitespace() {
                        end = at;
                    }
                    let ends = matches!(c, '.' | '!' | '?')
                        && chars.peek().is_none_or(|n| n.is_whitespace());
                    if ends {
                        flush(&mut said, &mut start, end);
                    }
                }
            }
            Part::Math {
                source,
                display,
                mathml,
                start: s,
                end: e,
            } => {
                if display {
                    flush(&mut said, &mut start, end);
                }
                let words = mathml
                    .as_deref()
                    .and_then(|m| maths(&math::speakable(m)))
                    .unwrap_or_else(|| strip_delimiters(&source).to_string());
                start.get_or_insert(s);
                said.push(' ');
                said.push_str(&words);
                said.push(' ');
                end = e;
                if display {
                    flush(&mut said, &mut start, end);
                }
            }
        }
    }
    flush(&mut said, &mut start, end);
    out
}

fn strip_delimiters(source: &str) -> &str {
    for (open, close) in [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)"), ("$", "$")] {
        if let Some(inner) = source
            .strip_prefix(open)
            .and_then(|s| s.strip_suffix(close))
        {
            return inner.trim();
        }
    }
    source
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Vec<(String, usize, usize)> {
        utterances(text, |m| m.contains("<mfrac>").then(|| "one half".into()))
            .into_iter()
            .map(|u| (u.text, u.start, u.end))
            .collect()
    }

    #[test]
    fn a_reading_is_said_a_sentence_at_a_time() {
        assert_eq!(
            said("First one. Then e.g. 3.5 more!  Last\nline two"),
            [
                ("First one.".into(), 0, 10),
                ("Then e.g.".into(), 11, 20),
                ("3.5 more!".into(), 21, 30),
                ("Last".into(), 32, 36),
                ("line two".into(), 37, 45),
            ]
        );
    }

    #[test]
    fn maths_is_said_in_words_and_display_maths_on_its_own() {
        assert_eq!(
            said("Take $\\frac{1}{2}$ of it.\n$$\\frac{a}{b}$$ and $x^$ too"),
            [
                ("Take one half of it.".into(), 0, 25),
                ("one half".into(), 26, 41),
                ("and x^ too".into(), 42, 54),
            ]
        );
        assert!(said("  \n ").is_empty());
    }
}
