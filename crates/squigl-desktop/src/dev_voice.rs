//! `--dev-fake-voice`: a voice that says nothing, taking a moment over each
//! utterance, and keeps what it was given for `dev_spoken` -- reading aloud end to
//! end with no speech system (CI has none on Linux) and nothing to hear.

use squigl_engine::engine::Waker;
use squigl_engine::speech::{Voice, VoiceInfo};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long each utterance takes to "say".
const UTTERANCE: Duration = Duration::from_millis(400);

/// What the fake voice was given, in order.
#[derive(Clone, Default)]
pub struct DevSpoken(pub Arc<Mutex<Vec<String>>>);

pub struct FakeVoice {
    spoken: DevSpoken,
    ended: Arc<AtomicU64>,
    /// The newest utterance: one cut off by another does not end the newer.
    newest: Arc<AtomicU64>,
    wake: Waker,
}

impl FakeVoice {
    pub fn new(spoken: DevSpoken, wake: Waker) -> Self {
        Self {
            spoken,
            ended: Arc::default(),
            newest: Arc::default(),
            wake,
        }
    }
}

impl Voice for FakeVoice {
    fn say(&mut self, id: u64, text: &str) -> anyhow::Result<()> {
        self.spoken.0.lock().unwrap().push(text.into());
        self.newest.store(id, Ordering::Release);
        let (ended, newest, wake) = (
            Arc::clone(&self.ended),
            Arc::clone(&self.newest),
            Arc::clone(&self.wake),
        );
        std::thread::spawn(move || {
            std::thread::sleep(UTTERANCE);
            if newest.load(Ordering::Acquire) == id {
                ended.fetch_max(id, Ordering::AcqRel);
                wake();
            }
        });
        Ok(())
    }

    fn stop(&mut self) {
        self.ended
            .fetch_max(self.newest.load(Ordering::Acquire), Ordering::AcqRel);
        (self.wake)();
    }

    fn ended(&self) -> u64 {
        self.ended.load(Ordering::Acquire)
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        vec![VoiceInfo {
            id: "test".into(),
            name: "Test voice".into(),
            language: Some("en".into()),
        }]
    }

    fn configure(&mut self, _: Option<&str>, _: f32) -> anyhow::Result<()> {
        Ok(())
    }

    fn maths(&self, _: &str) -> Option<String> {
        Some("some maths".into())
    }
}
