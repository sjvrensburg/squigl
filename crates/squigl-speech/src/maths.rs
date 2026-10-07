//! Maths in words: MathCAT (ClearSpeak, its rules zipped into the binary) on a
//! thread of its own, since MathCAT keeps its state per thread; every voice asks
//! it.

use anyhow::{anyhow, Result};
use libmathcat::interface as mathcat;
use std::sync::mpsc::{self, Sender};

type Ask = (String, Sender<Option<String>>);

pub struct Maths {
    tx: Sender<Ask>,
}

fn start_mathcat() -> Result<()> {
    // The rules are zipped into the binary; the directory is only a name then.
    mathcat::set_rules_dir("Rules").map_err(|e| anyhow!("{e}"))?;
    mathcat::set_preference("Language", "en").map_err(|e| anyhow!("{e}"))?;
    mathcat::set_preference("SpeechStyle", "ClearSpeak").map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

fn in_words(mathml: &str) -> Option<String> {
    mathcat::set_mathml(mathml).ok()?;
    let words = mathcat::get_spoken_text().ok()?;
    Some(words).filter(|w| !w.trim().is_empty())
}

impl Maths {
    pub fn start() -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Ask>();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("mathcat".into())
            .spawn(move || {
                let started = start_mathcat();
                let ok = started.is_ok();
                let _ = ready_tx.send(started);
                if !ok {
                    return;
                }
                for (mathml, reply) in rx {
                    let _ = reply.send(in_words(&mathml));
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the MathCAT thread ended"))??;
        Ok(Self { tx })
    }

    /// MathML in words, `None` when MathCAT cannot say it.
    pub fn say(&self, mathml: &str) -> Option<String> {
        let (reply, answer) = mpsc::channel();
        self.tx.send((mathml.into(), reply)).ok()?;
        answer.recv().ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mathcat_says_maths_in_words() {
        let maths = Maths::start().unwrap();
        let mathml = squigl_engine::math::to_mathml("\\frac{1}{2} + \\vec{v}", false).unwrap();
        let words = maths.say(&squigl_engine::math::speakable(&mathml)).unwrap();
        assert!(
            words.contains("half") && words.contains("vector v"),
            "{words}"
        );
    }
}
