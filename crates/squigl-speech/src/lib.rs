//! Reading aloud for squigl: [`Voices`] is the engine's [`Voice`], speaking with
//! Kokoro (squigl's own neural voice, downloaded when the person agrees; the
//! `kokoro` feature) or the system's speech ([`SystemVoice`]), and saying maths in
//! words by MathCAT ([`Maths`]) for either.

#[cfg(feature = "kokoro")]
mod kokoro;
mod maths;
mod system;

#[cfg(feature = "kokoro")]
pub use kokoro::KokoroVoice;
pub use maths::Maths;
pub use system::{system_rate, SystemVoice};

use anyhow::{bail, Result};
use squigl_engine::engine::Waker;
use squigl_engine::model::ModelPhase;
use squigl_engine::speech::{Voice, VoiceInfo};

/// Which voice said the newest utterance.
#[derive(Clone, Copy, PartialEq)]
enum Speaking {
    System,
    #[cfg(feature = "kokoro")]
    Kokoro,
}

/// The voices squigl has, as one: Kokoro once it is ready and chosen (or when no
/// voice is chosen -- it is the better one), the system's otherwise.
pub struct Voices {
    system: Option<SystemVoice>,
    #[cfg(feature = "kokoro")]
    kokoro: Option<KokoroVoice>,
    maths: Option<Maths>,
    /// `[speech].voice`.
    choice: Option<String>,
    speaking: Speaking,
}

impl Voices {
    /// The system's speech and MathCAT, and Kokoro when given its model.
    pub fn start(
        wake: Waker,
        #[cfg(feature = "kokoro")] kokoro: Option<
            std::sync::Arc<squigl_models::kokoro::KokoroService>,
        >,
    ) -> Result<Self> {
        log::debug!("starting the system's speech");
        let system = SystemVoice::start(wake.clone())
            .inspect_err(|e| log::warn!("no system speech: {e:#}"))
            .ok();
        log::debug!("system speech: {}", system.is_some());
        #[cfg(feature = "kokoro")]
        let kokoro = kokoro.and_then(|k| {
            KokoroVoice::start(k, wake.clone())
                .inspect_err(|e| log::warn!("no Kokoro voice: {e:#}"))
                .ok()
        });
        #[cfg(feature = "kokoro")]
        let none = system.is_none() && kokoro.is_none();
        #[cfg(not(feature = "kokoro"))]
        let none = system.is_none();
        if none {
            bail!("no way to speak on this system");
        }
        log::debug!("starting MathCAT");
        let maths = Maths::start()
            .inspect_err(|e| log::warn!("maths will be read as its source: MathCAT: {e:#}"))
            .ok();
        Ok(Self {
            system,
            #[cfg(feature = "kokoro")]
            kokoro,
            maths,
            choice: None,
            speaking: Speaking::System,
        })
    }

    /// The voice to speak with now.
    fn pick(&self) -> Speaking {
        #[cfg(feature = "kokoro")]
        {
            let chosen = self
                .choice
                .as_deref()
                .is_none_or(|c| c.starts_with("kokoro:"));
            if chosen && self.kokoro.as_ref().is_some_and(KokoroVoice::ready)
                || self.system.is_none()
            {
                return Speaking::Kokoro;
            }
        }
        Speaking::System
    }

    fn voice(&mut self, which: Speaking) -> Option<&mut dyn Voice> {
        match which {
            Speaking::System => self.system.as_mut().map(|v| v as &mut dyn Voice),
            #[cfg(feature = "kokoro")]
            Speaking::Kokoro => self.kokoro.as_mut().map(|v| v as &mut dyn Voice),
        }
    }

    fn each(&mut self, mut f: impl FnMut(&mut dyn Voice)) {
        if let Some(v) = &mut self.system {
            f(v);
        }
        #[cfg(feature = "kokoro")]
        if let Some(v) = &mut self.kokoro {
            f(v);
        }
    }
}

impl Voice for Voices {
    fn say(&mut self, id: u64, text: &str) -> Result<()> {
        let which = self.pick();
        // The other one stops: one voice at a time.
        if which != self.speaking {
            if let Some(v) = self.voice(self.speaking) {
                v.stop();
            }
        }
        self.speaking = which;
        match self.voice(which) {
            Some(v) => v.say(id, text),
            None => bail!("no voice"),
        }
    }

    fn stop(&mut self) {
        self.each(|v| v.stop());
    }

    fn ended(&self) -> u64 {
        // Utterance ids only grow, and one voice speaks at a time.
        let ended = self.system.as_ref().map_or(0, |v| v.ended());
        #[cfg(feature = "kokoro")]
        let ended = ended.max(self.kokoro.as_ref().map_or(0, |v| v.ended()));
        ended
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        let mut all = Vec::new();
        #[cfg(feature = "kokoro")]
        if self.kokoro.is_some() {
            all.extend(KokoroVoice::voice_list());
        }
        if let Some(v) = &self.system {
            all.extend(v.voices());
        }
        all
    }

    fn configure(&mut self, voice: Option<&str>, rate: f32) -> Result<()> {
        self.choice = voice.map(String::from);
        let system_voice = voice.filter(|v| !v.starts_with("kokoro:"));
        let mut result = Ok(());
        if let Some(v) = &mut self.system {
            result = v.configure(system_voice, rate);
        }
        #[cfg(feature = "kokoro")]
        if let Some(v) = &mut self.kokoro {
            v.configure(voice, rate)?;
        }
        result
    }

    fn maths(&self, mathml: &str) -> Option<String> {
        self.maths.as_ref()?.say(mathml)
    }

    fn prepare(&mut self, text: &str) {
        let which = self.pick();
        if let Some(v) = self.voice(which) {
            v.prepare(text);
        }
    }

    fn pause(&mut self) -> bool {
        let which = self.speaking;
        self.voice(which).is_some_and(|v| v.pause())
    }

    fn resume(&mut self) -> bool {
        let which = self.speaking;
        self.voice(which).is_some_and(|v| v.resume())
    }

    fn model(&self) -> Option<(String, ModelPhase)> {
        #[cfg(feature = "kokoro")]
        if let Some(v) = &self.kokoro {
            return v.model();
        }
        None
    }

    fn prepare_model(&self) {
        #[cfg(feature = "kokoro")]
        if let Some(v) = &self.kokoro {
            v.prepare_model();
        }
    }

    fn cancel_model(&self) {
        #[cfg(feature = "kokoro")]
        if let Some(v) = &self.kokoro {
            v.cancel_model();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Every voice starts (no sound): the system's, and Kokoro's output.
    #[test]
    #[ignore = "needs the system's speech and a sound output"]
    fn the_voices_start() {
        let t = std::time::Instant::now();
        #[cfg(feature = "kokoro")]
        let service = Some(Arc::new(squigl_models::kokoro::KokoroService::new(
            &squigl_engine::model::ModelContext {
                eager: false,
                notify: Arc::new(|| {}),
            },
        )));
        let voice = Voices::start(
            Arc::new(|| {}),
            #[cfg(feature = "kokoro")]
            service,
        )
        .unwrap();
        println!(
            "started in {:?}: {} voices, model {:?}",
            t.elapsed(),
            voice.voices().len(),
            voice.model()
        );
        assert!(voice.system.is_some());
        #[cfg(feature = "kokoro")]
        assert!(voice.kokoro.is_some());
    }

    /// Says a sentence and some maths, for a person to listen to, with the system
    /// voice (or Kokoro, with the `kokoro` feature and its model in
    /// `$SQUIGL_MODEL_DIR/kokoro`):
    /// `cargo test -p squigl-speech --release [--features kokoro] -- --ignored --nocapture listen`.
    #[test]
    #[ignore = "makes a sound"]
    fn listen() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let wake: Waker = Arc::new(move || {
            let _ = tx.lock().unwrap().send(());
        });
        #[cfg(feature = "kokoro")]
        let service = {
            let ctx = squigl_engine::model::ModelContext {
                eager: true,
                notify: Arc::new(|| {}),
            };
            let k = Arc::new(squigl_models::kokoro::KokoroService::new(&ctx));
            while !k.ready() && !matches!(k.phase(), ModelPhase::Failed { .. }) {
                std::thread::sleep(Duration::from_millis(100));
            }
            Some(k)
        };
        let mut voice = Voices::start(
            wake,
            #[cfg(feature = "kokoro")]
            service,
        )
        .unwrap();
        voice.configure(None, 1.0).unwrap();
        let text = squigl_engine::speech::utterances(
            "The entropy is $E=-\\sum_{i=1}^{k} p_{i} \\log_{2} p_{i}$ in bits. Recursively splits data in order to clarify the point.",
            |m| voice.maths(m),
        );
        for (i, u) in text.iter().enumerate() {
            println!("saying: {}", u.text);
            voice.say(i as u64 + 1, &u.text).unwrap();
            if let Some(next) = text.get(i + 1) {
                voice.prepare(&next.text);
            }
            while voice.ended() < i as u64 + 1 {
                rx.recv_timeout(Duration::from_secs(60))
                    .expect("the utterance ends");
            }
        }
    }
}
