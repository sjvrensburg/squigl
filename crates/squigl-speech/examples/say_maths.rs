//! Text in, the text as read aloud out: `$…$`, `$$…$$`, `\(…\)` and `\[…\]` maths in
//! words (MathCAT, ClearSpeak), exactly as squigl reads a reading. A filter for other
//! programs that hand text to a voice (halo-prep's podcast re-voicer calls it).
//!
//! One JSON string per line on stdin, one per line on stdout, so text holding line
//! breaks survives:
//!
//! ```text
//! echo '"Sample $2^m$ tokens."' | cargo run --release -p squigl-speech --example say_maths
//! "Sample 2 to the m-th power tokens."
//! ```

use squigl_speech::Maths;
use std::io::{BufRead, Write};

fn main() -> anyhow::Result<()> {
    let maths = Maths::start()?;
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        // A blank line is no JSON string; answer it with an empty one, so a
        // caller waiting for each answer keeps its lockstep.
        if line.trim().is_empty() {
            writeln!(out, "\"\"")?;
            out.flush()?;
            continue;
        }
        let text: String = serde_json::from_str(&line)?;
        let said: Vec<String> = squigl_engine::speech::utterances(&text, |m| maths.say(m))
            .into_iter()
            .map(|u| u.text)
            .collect();
        writeln!(out, "{}", serde_json::to_string(&said.join(" "))?)?;
        // A caller waits for each answer before sending the next line.
        out.flush()?;
    }
    Ok(())
}
