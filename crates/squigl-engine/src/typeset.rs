//! Typesetting a reading: the [`Typesetter`] a front end may have (the egui window's
//! is `squigl_math::Renderer`, on Typst), and the text and tint spans handed to it.

use crate::config::UiConfig;
use crate::transcribe::{Confidence, Reading};

/// A byte range of the text handed to [`Typesetter::render`], to be tinted by
/// hesitation. Disjoint and given in byte order; `severity` breaks a tie when a
/// LaTeX maths segment is shaded as a whole and more than one span falls inside
/// it (higher wins).
#[derive(Debug)]
pub struct TintSpan {
    pub start: usize,
    pub end: usize,
    pub color: [u8; 4],
    pub severity: u8,
}

/// Typesets a reading (LaTeX maths and all) into pixels.
pub trait Typesetter: Send + Sync {
    /// `width_pt` points wide, text `size_pt`, `scale` pixels per point, in `rgb`.
    /// `spans` tints hesitant/wavering tokens -- plain text by `#highlight`, a
    /// LaTeX maths segment as a whole by its worst overlapping span.
    fn render(
        &self,
        text: &str,
        spans: &[TintSpan],
        width_pt: f32,
        size_pt: f32,
        scale: f32,
        rgb: [u8; 3],
    ) -> anyhow::Result<(image::RgbaImage, f32)>;
}

/// The text to typeset for a reading, and the tint spans over it: the tokens'
/// concatenation when they are valid (so span byte offsets line up exactly; see
/// [`Reading::tokens_if_valid`]) and one span per token `tint` gives a colour (sRGBA,
/// unmultiplied), else the reading's own text and no spans. The colours are the
/// front end's.
pub fn typeset_source(
    r: &Reading,
    ui_cfg: &UiConfig,
    tint: impl Fn(Confidence) -> Option<[u8; 4]>,
) -> (String, Vec<TintSpan>) {
    let Some(tokens) = r.tokens_if_valid() else {
        return (r.text.clone(), Vec::new());
    };
    let mut text = String::new();
    let mut spans = Vec::new();
    for tok in tokens {
        let start = text.len();
        text.push_str(&tok.text);
        let end = text.len();
        let confidence = tok.confidence(ui_cfg);
        if let Some(color) = tint(confidence) {
            spans.push(TintSpan {
                start,
                end,
                color,
                severity: match confidence {
                    Confidence::Hesitant => 2,
                    Confidence::Wavering => 1,
                    Confidence::Steady => 0,
                },
            });
        }
    }
    (text, spans)
}
