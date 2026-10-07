//! The system's own speech through the `tts` crate: speech-dispatcher on Linux,
//! WinRT on Windows, AVSpeechSynthesizer on macOS. It lives on a thread of its
//! own, where the end of an utterance is noticed (from the speech system's
//! callback where it has one, else by asking) and passed on as a call of the
//! engine's waker.

use anyhow::{anyhow, Context, Result};
use squigl_engine::engine::Waker;
use squigl_engine::speech::{Voice, VoiceInfo};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often an utterance is checked on while one is being said.
const POLL: Duration = Duration::from_millis(100);
/// An utterance the speech system never reports as started (an empty one, say)
/// counts as over after this long.
const NEVER_STARTED: Duration = Duration::from_secs(3);

enum Msg {
    Say(u64, String),
    Stop,
    /// The speech system said an utterance ended.
    Ended,
    Configure(Option<String>, f32, Sender<Result<()>>),
}

/// The system's speech, run on its own thread.
pub struct SystemVoice {
    tx: Sender<Msg>,
    ended: Arc<AtomicU64>,
    voices: Vec<VoiceInfo>,
}

impl SystemVoice {
    /// Starts the speech thread; an error when the system has no speech (no
    /// speech-dispatcher running, say).
    pub fn start(wake: Waker) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let ended = Arc::new(AtomicU64::new(0));
        let (ended2, tx2) = (Arc::clone(&ended), tx.clone());
        std::thread::Builder::new()
            .name("speech".into())
            .spawn(move || run(rx, tx2, ended2, wake, ready_tx))?;
        let voices = ready_rx
            .recv()
            .map_err(|_| anyhow!("the speech thread ended"))??;
        Ok(Self { tx, ended, voices })
    }

    fn send(&self, msg: Msg) {
        // The thread ends only when this is dropped.
        let _ = self.tx.send(msg);
    }
}

impl Voice for SystemVoice {
    fn say(&mut self, id: u64, text: &str) -> Result<()> {
        self.send(Msg::Say(id, text.into()));
        Ok(())
    }

    fn stop(&mut self) {
        self.send(Msg::Stop);
    }

    fn ended(&self) -> u64 {
        self.ended.load(Ordering::Acquire)
    }

    /// The English ones: reading aloud is in English (MathCAT's rules, Kokoro), and
    /// speech-dispatcher lists every espeak variant of every language -- 13,000 of
    /// them here.
    fn voices(&self) -> Vec<VoiceInfo> {
        self.voices
            .iter()
            .filter(|v| {
                v.language
                    .as_deref()
                    .is_some_and(|l| l.to_lowercase().starts_with("en"))
            })
            .cloned()
            .collect()
    }

    fn configure(&mut self, voice: Option<&str>, rate: f32) -> Result<()> {
        let (reply, answer) = mpsc::channel();
        self.send(Msg::Configure(voice.map(String::from), rate, reply));
        answer
            .recv()
            .map_err(|_| anyhow!("the speech thread ended"))?
    }

    fn maths(&self, _: &str) -> Option<String> {
        // [`crate::Maths`] does maths, for every voice.
        None
    }
}

/// `rate` (a multiple of normal speed, 0.5 to 2) on the speech system's own scale:
/// normal at 1, its slowest at 0.5 and its fastest at 2, straight lines between.
pub fn system_rate(rate: f32, min: f32, normal: f32, max: f32) -> f32 {
    let rate = rate.clamp(0.5, 2.0);
    if rate >= 1.0 {
        normal + (max - normal) * (rate - 1.0)
    } else {
        normal - (normal - min) * (1.0 - rate) * 2.0
    }
}

fn voice_info(v: &tts::Voice) -> VoiceInfo {
    VoiceInfo {
        id: v.id(),
        name: v.name(),
        language: Some(v.language().to_string()).filter(|l| !l.is_empty()),
    }
}

fn run(
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    ended: Arc<AtomicU64>,
    wake: Waker,
    ready: Sender<Result<Vec<VoiceInfo>>>,
) {
    let mut tts = match tts::Tts::default().context("starting the system's speech") {
        Ok(t) => t,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let voices: Vec<tts::Voice> = tts.voices().unwrap_or_default();
    // Where the system calls back at an utterance's end, it is a nudge to look.
    let callbacks = tts.supported_features().utterance_callbacks;
    if callbacks {
        let nudge = tx.clone();
        let _ = tts.on_utterance_end(Some(Box::new(move |_| {
            let _ = nudge.send(Msg::Ended);
        })));
    }
    drop(tx);
    let _ = ready.send(Ok(voices.iter().map(voice_info).collect()));

    // The utterance being said: its id, when it was started, whether it has been
    // seen speaking.
    let mut current: Option<(u64, Instant, bool)> = None;
    let finish = |current: &mut Option<(u64, Instant, bool)>| {
        if let Some((id, ..)) = current.take() {
            ended.store(id, Ordering::Release);
            wake();
        }
    };
    loop {
        let msg = if current.is_some() {
            match rx.recv_timeout(POLL) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        };
        match msg {
            Some(Msg::Say(id, text)) => {
                // The one before is cut off: it is over.
                finish(&mut current);
                match tts.speak(text, true) {
                    Ok(_) => current = Some((id, Instant::now(), false)),
                    Err(e) => {
                        log::warn!("could not speak: {e}");
                        ended.store(id, Ordering::Release);
                        wake();
                    }
                }
            }
            Some(Msg::Stop) => {
                let _ = tts.stop();
                finish(&mut current);
            }
            Some(Msg::Configure(voice, rate, reply)) => {
                let result = (|| -> Result<()> {
                    let r = system_rate(rate, tts.min_rate(), tts.normal_rate(), tts.max_rate());
                    tts.set_rate(r).context("setting the rate")?;
                    if let Some(id) = voice {
                        let v = voices
                            .iter()
                            .find(|v| v.id() == id)
                            .with_context(|| format!("no voice called {id:?}"))?;
                        tts.set_voice(v).context("choosing the voice")?;
                    }
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Some(Msg::Ended) | None => {}
        }
        // Whatever woke the loop, see whether the utterance is still going.
        if let Some((_, started, seen)) = &mut current {
            let speaking = tts.is_speaking().unwrap_or(false);
            if speaking {
                *seen = true;
            } else if *seen || started.elapsed() > NEVER_STARTED {
                finish(&mut current);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_is_on_the_systems_scale() {
        // speech-dispatcher: -100..100, normal 0.
        assert_eq!(system_rate(1.0, -100.0, 0.0, 100.0), 0.0);
        assert_eq!(system_rate(2.0, -100.0, 0.0, 100.0), 100.0);
        assert_eq!(system_rate(0.5, -100.0, 0.0, 100.0), -100.0);
        assert_eq!(system_rate(1.5, -100.0, 0.0, 100.0), 50.0);
        // WinRT: 0.5..6, normal 1; out of range is held to it.
        assert_eq!(system_rate(9.0, 0.5, 1.0, 6.0), 6.0);
        assert_eq!(system_rate(0.75, 0.5, 1.0, 6.0), 0.75);
    }

    /// The system's speech starts and lists its voices (no sound).
    #[test]
    #[ignore = "needs the system's speech (speech-dispatcher running, on Linux)"]
    fn the_system_voice_starts() {
        let voice = SystemVoice::start(Arc::new(|| {})).unwrap();
        assert!(!voice.voices().is_empty());
    }
}
