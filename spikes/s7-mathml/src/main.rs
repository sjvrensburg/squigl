//! Spike S7: LaTeX -> MathML with pulldown-latex and math-core (and Temml's output
//! from temml.mjs, when temml.json is there), each MathML spoken by MathCAT in
//! ClearSpeak. Writes report.html (every rendering side by side, with the speech)
//! and prints a summary.

use std::fmt::Write as _;
use std::time::Instant;

/// The $...$ / $$...$$ segments of a reading.
fn segments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('$') {
        let display = rest[start..].starts_with("$$");
        let delim = if display { "$$" } else { "$" };
        let body = &rest[start + delim.len()..];
        let Some(end) = body.find(delim) else { break };
        out.push(body[..end].trim().to_string());
        rest = &body[end + delim.len()..];
    }
    out
}

fn corpus() -> Vec<(String, String)> {
    let mut items = Vec::new();
    let dir = "../../crates/squigl-models/testdata/handwriting";
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".cpu.txt"))
        .collect();
    names.sort();
    for p in names {
        let text = std::fs::read_to_string(&p).unwrap();
        let name = p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".cpu.txt", "");
        for s in segments(&text) {
            items.push((format!("reading: {name}"), s));
        }
    }
    for line in std::fs::read_to_string("corpus.tex").unwrap().lines() {
        if !line.trim().is_empty() && !line.starts_with('#') {
            items.push(("corpus".into(), line.trim().to_string()));
        }
    }
    items
}

struct Outcome {
    mathml: Option<String>,
    error: Option<String>,
    micros: u128,
}

fn pulldown(latex: &str) -> Outcome {
    use pulldown_latex::{push_mathml, Parser, RenderConfig, Storage};
    let t = Instant::now();
    let storage = Storage::new();
    let events: Vec<_> = Parser::new(latex, &storage).collect();
    let error = events
        .iter()
        .find_map(|e| e.as_ref().err().map(|e| e.to_string()));
    let mut s = String::new();
    let parser = Parser::new(latex, &storage);
    push_mathml(&mut s, parser, RenderConfig::default()).unwrap();
    let micros = t.elapsed().as_micros();
    Outcome {
        mathml: Some(s),
        error,
        micros,
    }
}

/// Converts, defining each unknown command (models invent `\softmax`, `\Var`) as
/// an operator name and trying again, as squigl-math does with MiTeX.
fn lenient(latex: &str) -> Result<String, String> {
    let mut macros: Vec<(String, String)> = Vec::new();
    loop {
        let conv = math_core::LatexToMathML::new(math_core::MathCoreConfig {
            macros: macros.clone(),
            ..Default::default()
        })
        .unwrap();
        match conv.convert_with_local_state(latex, math_core::MathDisplay::Block) {
            Ok(r) => return Ok(r.mathml),
            Err(e) => {
                let what = format!("{e:?}");
                let name = latex.get(e.0.clone()).unwrap_or("");
                let ok_name = name.len() > 1
                    && name.starts_with('\\')
                    && name[1..].chars().all(|c| c.is_ascii_alphabetic());
                if what.contains("UnknownCommand") && ok_name && macros.len() < 8 {
                    let n = &name[1..];
                    macros.push((n.to_string(), format!("\\operatorname{{{n}}}")));
                } else {
                    return Err(what);
                }
            }
        }
    }
}

fn mathcore(conv: &math_core::LatexToMathML, latex: &str) -> Outcome {
    let t = Instant::now();
    let r = conv.convert_with_local_state(latex, math_core::MathDisplay::Block);
    let micros = t.elapsed().as_micros();
    match r {
        Ok(r) => Outcome {
            mathml: Some(for_webkit(&r.mathml)),
            error: None,
            micros,
        },
        Err(e) => Outcome {
            mathml: None,
            error: Some(format!("{e:?}")),
            micros,
        },
    }
}

/// math-core's MathML as MathCAT speaks it best: no variation selectors (\mathcal's
/// chancery form, which MathCAT reads out as the bare character), the vector
/// arrow as U+2192 (it knows that one), and no invisible separator after a
/// `cases` table (which hides that it is one).
fn speakable(mathml: &str) -> String {
    mathml
        .replace(['\u{FE00}', '\u{FE01}'], "")
        .replace(">\u{20D7}</mo>", ">\u{2192}</mo>")
        .replace("</mtable><mo>\u{2063}</mo>", "</mtable>")
}

/// math-core writes `\log_2` as an `<mo>` with its own right space as the base of an
/// `<msub>`; WebKit puts that space between the base and its script ("log ₂p")
/// instead of after the whole, as MathML Core's embellished operators have it.
/// Moves it out: the base becomes an `<mi>`, followed after the script by an
/// `<mspace>` of the same width.
fn for_webkit(mathml: &str) -> String {
    let mut out = String::with_capacity(mathml.len());
    let mut rest = mathml;
    let mut pending: Vec<(String, usize, String)> = Vec::new(); // (tag, depth, width)
    let mut depth: Vec<String> = Vec::new();
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let gt = rest[lt..]
            .find('>')
            .map(|i| lt + i + 1)
            .unwrap_or(rest.len());
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
            depth.pop();
            if let Some((t, d, w)) = pending.last() {
                if *t == name && *d == depth.len() {
                    if w != "0" {
                        out.push_str(&format!("<mspace width=\"{w}\"/>"));
                    }
                    pending.pop();
                }
            }
        } else if tag.ends_with("/>") {
            out.push_str(tag);
        } else {
            // <mo lspace="A" rspace="B">name</mo> as the base of a script element.
            let base = matches!(name.as_str(), "msub" | "msup" | "msubsup")
                .then(|| {
                    let rest = rest.strip_prefix("<mo lspace=\"")?;
                    let (l, rest) = rest.split_once("\" rspace=\"")?;
                    let (r, rest) = rest.split_once("\">")?;
                    let (text, rest) = rest.split_once("</mo>")?;
                    text.chars()
                        .all(|c| c.is_alphabetic())
                        .then(|| (l.to_string(), r.to_string(), text.to_string(), rest))
                })
                .flatten();
            if let Some((l, r, text, after)) = base {
                if l != "0" {
                    out.push_str(&format!("<mspace width=\"{l}\"/>"));
                }
                out.push_str(tag);
                out.push_str(&format!("<mi>{text}</mi>"));
                pending.push((name.clone(), depth.len(), r));
                depth.push(name);
                rest = after;
                continue;
            }
            out.push_str(tag);
            depth.push(name);
        }
    }
    out.push_str(rest);
    out
}

fn speak(mathml: &str) -> String {
    let mathml = &speakable(mathml);
    match libmathcat::interface::set_mathml(mathml) {
        Ok(_) => {
            libmathcat::interface::get_spoken_text().unwrap_or_else(|e| format!("(no speech: {e})"))
        }
        Err(e) => format!(
            "(MathCAT refused it: {})",
            e.to_string().lines().next().unwrap_or("")
        ),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn main() -> anyhow::Result<()> {
    libmathcat::interface::set_rules_dir("Rules").map_err(|e| anyhow::anyhow!("{e}"))?;
    libmathcat::interface::set_preference("Language", "en").map_err(|e| anyhow::anyhow!("{e}"))?;
    libmathcat::interface::set_preference("SpeechStyle", "ClearSpeak")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let conv = math_core::LatexToMathML::new(math_core::MathCoreConfig::default()).unwrap();
    let temml: Option<Vec<serde_json::Value>> = std::fs::read_to_string("temml.json")
        .ok()
        .map(|s| serde_json::from_str(&s).unwrap());
    let items = corpus();
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>S7 LaTeX to MathML</title>\n<style>body{font:16px system-ui;margin:1rem} table{table-layout:fixed;width:100%} th:nth-child(1){width:2rem} th:nth-child(2){width:14rem} td{overflow:hidden;overflow-wrap:anywhere} td,th{border:1px solid #999;padding:.4rem;vertical-align:top} table{border-collapse:collapse} code{font-size:12px} .err{color:#b00} .say{font-size:12px;color:#333} math{font-size:1.4em}</style>\n<table><tr><th>#</th><th>LaTeX</th><th>pulldown-latex</th><th>math-core</th><th>Temml</th></tr>\n",
    );
    let (mut pd_fail, mut mc_fail, mut tm_fail) = (0, 0, 0);
    let (mut pd_us, mut mc_us) = (0u128, 0u128);
    let mut speech = Vec::new();
    for (i, (from, latex)) in items.iter().enumerate() {
        let pd = pulldown(latex);
        let mc = mathcore(&conv, latex);
        pd_us += pd.micros;
        mc_us += mc.micros;
        pd_fail += pd.error.is_some() as usize;
        mc_fail += mc.error.is_some() as usize;
        let tm = temml.as_ref().map(|t| &t[i]);
        let tm_err = tm.and_then(|t| t["error"].as_str());
        tm_fail += tm_err.is_some() as usize;
        let cell = |o: &Outcome| {
            let mut c = String::new();
            if let Some(m) = &o.mathml {
                c += m;
                let _ = write!(c, "<div class=say>{}</div>", esc(&speak(m)));
            }
            if let Some(e) = &o.error {
                let _ = write!(c, "<div class=err>{}</div>", esc(e));
            }
            c
        };
        let tm_cell = match tm {
            None => String::new(),
            Some(t) => {
                let m = t["mathml"].as_str().unwrap_or("");
                let mut c = m.to_string();
                if !m.is_empty() {
                    let _ = write!(c, "<div class=say>{}</div>", esc(&speak(m)));
                }
                if let Some(e) = tm_err {
                    let _ = write!(c, "<div class=err>{}</div>", esc(e));
                }
                c
            }
        };
        speech.push(serde_json::json!({
            "latex": latex,
            "pulldown": pd.mathml.as_deref().map(speak),
            "mathcore": mc.mathml.as_deref().map(speak),
            "pulldown_error": pd.error,
            "mathcore_error": mc.error,
        }));
        let _ = writeln!(
            html,
            "<tr><td>{i}</td><td><code>{}</code><div class=say>{from}</div></td><td>{}</td><td>{}</td><td>{tm_cell}</td></tr>",
            esc(latex),
            cell(&pd),
            cell(&mc)
        );
    }
    html += "</table>\n";
    std::fs::write("report.html", html)?;
    std::fs::write("speech.json", serde_json::to_string_pretty(&speech)?)?;
    std::fs::write(
        "corpus.json",
        serde_json::to_string(&items.iter().map(|(_, l)| l).collect::<Vec<_>>())?,
    )?;
    let lenient_fail = items.iter().filter(|(_, l)| lenient(l).is_err()).count();
    println!("math-core with unknown commands as operator names: {lenient_fail} errors");
    println!(
        "{} expressions. Errors: pulldown-latex {pd_fail}, math-core {mc_fail}, Temml {}. Time: pulldown-latex {pd_us} us, math-core {mc_us} us in all.",
        items.len(),
        if temml.is_some() { tm_fail.to_string() } else { "(not run)".into() }
    );
    Ok(())
}
