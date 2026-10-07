//! A reading's maths as MathML, for a front end that shows readings as text
//! (roadmap spike S7 chose `math-core`): the reading split into text and maths at
//! `$…$`, `$$…$$`, `\(…\)` and `\[…\]` (as `squigl-math` splits it for Typst), each
//! maths segment converted to MathML Core. A command nothing defines (models
//! invent `\softmax`, `\Var`) is taken as an operator name; maths that still fails
//! (an unclosed group, `x^`) stays as its source text.

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};

/// A run of a reading: text, or maths with its MathML. `start`/`end` are offsets
/// into the reading's text in UTF-16 code units (a web page's string indices),
/// covering the delimiters for maths.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Part {
    Text {
        text: String,
        start: usize,
        end: usize,
    },
    Math {
        /// The source, delimiters and all: what to show if there is no MathML.
        source: String,
        display: bool,
        /// `None` when it could not be converted.
        mathml: Option<String>,
        start: usize,
        end: usize,
    },
}

/// The next maths opener in `s`: its byte offset, the opener, its closer, and
/// whether it is display maths.
fn next_math(s: &str) -> Option<(usize, &'static str, &'static str, bool)> {
    let candidates: [(&str, &str, bool); 4] = [
        ("$$", "$$", true),
        ("\\[", "\\]", true),
        ("\\(", "\\)", false),
        ("$", "$", false),
    ];
    let mut best: Option<(usize, &'static str, &'static str, bool)> = None;
    for (open, close, display) in candidates {
        if let Some(i) = s.find(open) {
            if best.is_none_or(|b| i < b.0) {
                best = Some((i, open, close, display));
            }
        }
    }
    best
}

/// `text` as text and maths runs, in order; nothing is dropped, so the runs'
/// texts and sources put `text` back together.
pub fn parts(text: &str) -> Vec<Part> {
    let utf16 = |byte: usize| text[..byte].encode_utf16().count();
    let mut out = Vec::new();
    let push_text = |from: usize, to: usize, out: &mut Vec<Part>| {
        if from < to {
            out.push(Part::Text {
                text: text[from..to].to_string(),
                start: utf16(from),
                end: utf16(to),
            });
        }
    };
    let mut at = 0;
    let mut plain = 0; // where the pending text run began
    while let Some((i, open, close, display)) = next_math(&text[at..]) {
        let start = at + i;
        let body = start + open.len();
        let Some(len) = text[body..].find(close) else {
            break;
        };
        let end = body + len + close.len();
        let tex = &text[body..body + len];
        if tex.trim().is_empty() {
            at = end;
            continue;
        }
        push_text(plain, start, &mut out);
        out.push(Part::Math {
            source: text[start..end].to_string(),
            display,
            mathml: to_mathml(tex, display),
            start: utf16(start),
            end: utf16(end),
        });
        at = end;
        plain = end;
    }
    push_text(plain, text.len(), &mut out);
    out
}

/// The MathML for `latex`, or `None` when it is not maths math-core can read even
/// with its unknown commands taken as operator names.
pub fn to_mathml(latex: &str, display: bool) -> Option<String> {
    let display = if display {
        MathDisplay::Block
    } else {
        MathDisplay::Inline
    };
    let mut macros: Vec<(String, String)> = Vec::new();
    // Each pass defines one more unknown command; a reading has few.
    for _ in 0..8 {
        let converter = LatexToMathML::new(MathCoreConfig {
            macros: macros.clone(),
            ..Default::default()
        })
        .ok()?;
        match converter.convert_with_local_state(latex, display) {
            Ok(r) => return Some(for_webkit(&r.mathml)),
            Err(e) => {
                // The error's kind is private; its text and span say enough.
                let name = latex.get(e.0.clone()).unwrap_or("");
                let command = name.len() > 1
                    && name.starts_with('\\')
                    && name[1..].chars().all(|c| c.is_ascii_alphabetic());
                if !(command && format!("{e:?}").contains("UnknownCommand")) {
                    return None;
                }
                let n = &name[1..];
                macros.push((n.to_string(), format!("\\operatorname{{{n}}}")));
            }
        }
    }
    None
}

/// math-core writes `\log_2`, `\sin^2` and the like as an `<mo>` with spacing of
/// its own as the base of an `<msub>`/`<msup>`/`<msubsup>`; WebKit puts that space
/// between the base and its script ("log ₂p") rather than around the whole, as
/// MathML Core's embellished operators have it. This moves it out: the base
/// becomes an `<mi>`, the whole gets `<mspace>`s of the same widths around it.
fn for_webkit(mathml: &str) -> String {
    let mut out = String::with_capacity(mathml.len());
    let mut rest = mathml;
    // Elements open around the current position, and the script elements whose
    // close gets a space after: (name, depth at opening, width).
    let mut open: Vec<String> = Vec::new();
    let mut pending: Vec<(String, usize, String)> = Vec::new();
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let gt = rest[lt..].find('>').map_or(rest.len(), |i| lt + i + 1);
        let tag = &rest[lt..gt];
        rest = &rest[gt..];
        let name: String = tag
            .trim_start_matches("</")
            .trim_start_matches('<')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if tag.starts_with("</") {
            out.push_str(tag);
            open.pop();
            if pending
                .last()
                .is_some_and(|(n, d, _)| *n == name && *d == open.len())
            {
                let (_, _, width) = pending.pop().expect("just looked");
                if width != "0" {
                    out.push_str(&format!("<mspace width=\"{width}\"/>"));
                }
            }
            continue;
        }
        if tag.ends_with("/>") {
            out.push_str(tag);
            continue;
        }
        // <mo lspace="A" rspace="B">name</mo> as the base of a script element.
        let base = matches!(name.as_str(), "msub" | "msup" | "msubsup")
            .then(|| {
                let r = rest.strip_prefix("<mo lspace=\"")?;
                let (l, r) = r.split_once("\" rspace=\"")?;
                let (right, r) = r.split_once("\">")?;
                let (text, after) = r.split_once("</mo>")?;
                text.chars()
                    .all(char::is_alphabetic)
                    .then(|| (l.to_string(), right.to_string(), text.to_string(), after))
            })
            .flatten();
        match base {
            Some((left, right, text, after)) => {
                if left != "0" {
                    out.push_str(&format!("<mspace width=\"{left}\"/>"));
                }
                out.push_str(tag);
                out.push_str(&format!("<mi>{text}</mi>"));
                pending.push((name.clone(), open.len(), right));
                open.push(name);
                rest = after;
            }
            None => {
                out.push_str(tag);
                open.push(name);
            }
        }
    }
    out.push_str(rest);
    out
}

/// MathML as MathCAT speaks it best: no variation selectors (math-core's chancery
/// `\mathcal`, which MathCAT reads out as the bare character), the vector arrow as
/// U+2192 (which it knows), and no invisible separator after a `cases` table
/// (which hides that it is one).
pub fn speakable(mathml: &str) -> String {
    mathml
        .replace(['\u{FE00}', '\u{FE01}'], "")
        .replace(">\u{20D7}</mo>", ">\u{2192}</mo>")
        .replace("</mtable><mo>\u{2063}</mo>", "</mtable>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(parts: &[Part]) -> Vec<String> {
        parts
            .iter()
            .map(|p| match p {
                Part::Text { text, .. } => format!("text {text:?}"),
                Part::Math {
                    source,
                    display,
                    mathml,
                    ..
                } => format!(
                    "{} {source:?}{}",
                    if *display { "display" } else { "inline" },
                    if mathml.is_some() { "" } else { " (as text)" }
                ),
            })
            .collect()
    }

    #[test]
    fn splits_a_reading_into_text_and_maths() {
        let text = "Entropy $E=-\\sum_i p_i$ is \\(k>2\\), or\n$$ x^ $$\nand \\[a\\] costs $5";
        assert_eq!(
            kinds(&parts(text)),
            [
                "text \"Entropy \"",
                "inline \"$E=-\\\\sum_i p_i$\"",
                "text \" is \"",
                "inline \"\\\\(k>2\\\\)\"",
                "text \", or\\n\"",
                "display \"$$ x^ $$\" (as text)",
                "text \"\\nand \"",
                "display \"\\\\[a\\\\]\"",
                // An unclosed opener is text.
                "text \" costs $5\"",
            ]
        );
    }

    #[test]
    fn offsets_are_a_web_pages() {
        // "é" is one UTF-16 unit and two bytes; "𝑥" two units and four bytes.
        let p = parts("é𝑥 $y$!");
        assert_eq!(
            p[1],
            Part::Math {
                source: "$y$".into(),
                display: false,
                mathml: to_mathml("y", false),
                start: 4,
                end: 7,
            }
        );
        assert!(matches!(
            &p[2],
            Part::Text {
                start: 7,
                end: 8,
                ..
            }
        ));
    }

    #[test]
    fn unknown_commands_are_operator_names() {
        let m = to_mathml("\\softmax(z) + \\Var(X)", false).unwrap();
        assert!(m.contains(">softmax<") && m.contains(">Var<"), "{m}");
        assert_eq!(to_mathml("\\frac{1}{2", true), None);
        assert_eq!(to_mathml("\\left( x", true), None);
    }

    #[test]
    fn script_operators_keep_their_space_outside() {
        let m = to_mathml("\\log_2 p_i", false).unwrap();
        assert!(
            m.contains("<msub><mi>log</mi><mn>2</mn></msub><mspace width=\"0.1667em\"/>"),
            "{m}"
        );
        // One within another still closes in the right place.
        let m = to_mathml("\\sin^{\\log_2 x} y", false).unwrap();
        assert!(!m.contains("<mo lspace"), "{m}");
        assert_eq!(m.matches("<msup>").count(), m.matches("</msup>").count());
    }

    #[test]
    fn speech_gets_what_mathcat_knows() {
        let m = speakable(&to_mathml("\\mathcal{L} + \\vec{v}", false).unwrap());
        assert!(!m.contains('\u{FE00}') && m.contains('\u{2192}'), "{m}");
        let m = speakable(&to_mathml("\\begin{cases} 1 & x \\end{cases}", true).unwrap());
        assert!(!m.contains('\u{2063}'), "{m}");
    }
}
