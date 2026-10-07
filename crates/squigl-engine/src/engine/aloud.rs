//! Reading aloud (roadmap Phase 8): the results said by the [`Voice`] a sentence
//! at a time -- one reading, or the page: every block read in order and said as
//! it lands -- with pause (between sentences), skipping between results, and each
//! new reading said as it arrives when `[speech].speak_new` is on. Runs in
//! [`Engine::handle`] and [`Engine::pump`] like the rest of the engine; the voice
//! calls the waker when an utterance ends.

use super::{Engine, Level, Reply};
use crate::speech::{utterances, Utterance, Voice, VoiceInfo};
use std::collections::VecDeque;

/// Read-aloud's half of the engine's state.
#[derive(Default)]
pub(super) struct Aloud {
    voice: Option<Box<dyn Voice>>,
    /// Asked of the voice once (a system may list thousands).
    voices: Option<Vec<VoiceInfo>>,
    next_id: u64,
    /// The result being said, and its sentences still to say.
    result: Option<usize>,
    sentences: VecDeque<Utterance>,
    /// The sentence being said, by utterance id.
    saying: Option<(u64, Utterance)>,
    paused: bool,
    /// Going on to the next result when this one is said (the page read aloud),
    /// waiting for it while reads are still to come.
    follow: bool,
    /// The result to say next when following.
    next: Option<usize>,
    /// The results list these positions are of ([`Engine::results_generation`]).
    generation: u64,
    /// Results already offered to `speak_new`.
    heard: usize,
}

impl Aloud {
    pub(super) fn new(voice: Option<Box<dyn Voice>>) -> Self {
        Self {
            voice,
            ..Self::default()
        }
    }
}

/// What is being said: which result, and where the sentence is in its first
/// reading's text (UTF-16 units).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Speaking {
    pub result: usize,
    pub start: usize,
    pub end: usize,
}

/// Reading aloud, for a front end.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct SpeechSlice {
    /// There is a voice (a build with speech, and the system's speech working).
    pub available: bool,
    pub speaking: Option<Speaking>,
    pub paused: bool,
    /// The page is being read aloud: what is still to be read is said as it lands.
    pub following: bool,
}

impl Engine {
    /// Builds the voice (once) and puts `[speech]` in force.
    pub(super) fn configure_voice(&mut self) {
        let speech = self.config().speech.clone();
        let Some(voice) = &mut self.aloud.voice else {
            return;
        };
        if self.aloud.voices.is_none() {
            self.aloud.voices = Some(voice.voices());
        }
        if let Err(e) = voice.configure(speech.voice.as_deref(), speech.rate) {
            self.notice(Level::Warning, format!("speech settings: {e:#}"));
        }
    }

    /// The voices there are, for the voices slice (sent once).
    pub(super) fn voices(&self) -> Vec<VoiceInfo> {
        self.aloud.voices.clone().unwrap_or_default()
    }

    pub(super) fn speech_slice(&self) -> SpeechSlice {
        let a = &self.aloud;
        SpeechSlice {
            available: a.voice.is_some(),
            speaking: a.result.and_then(|result| {
                let u = a
                    .saying
                    .as_ref()
                    .map(|(_, u)| u)
                    .or_else(|| a.sentences.front())?;
                Some(Speaking {
                    result,
                    start: u.start,
                    end: u.end,
                })
            }),
            paused: a.paused,
            following: a.follow,
        }
    }

    /// The voice's own model, if it has one to download.
    pub(super) fn voice_model(&self) -> Option<(String, crate::model::ModelPhase)> {
        self.aloud.voice.as_ref().and_then(|v| v.model())
    }

    /// Prepares or cancels the voice's model called `name`; false if it has none.
    pub(super) fn voice_model_command(&self, name: &str, prepare: bool) -> bool {
        let Some(v) = &self.aloud.voice else {
            return false;
        };
        if v.model().is_none_or(|(n, _)| n != name) {
            return false;
        }
        if prepare {
            v.prepare_model();
        } else {
            v.cancel_model();
        }
        true
    }

    fn has_voice(&mut self) -> bool {
        if self.aloud.voice.is_none() {
            self.notice(Level::Warning, "reading aloud is not available here");
            return false;
        }
        true
    }

    /// Says result `index` (`None`: the newest, or the first of a page's blocks).
    pub(super) fn speak(&mut self, index: Option<usize>) -> Reply {
        if !self.has_voice() {
            return Reply::Unchanged;
        }
        let n = self.results().len();
        let index = index.unwrap_or(if self.results_in_page_order() {
            0
        } else {
            n.saturating_sub(1)
        });
        if index >= n {
            self.notice(Level::Info, "nothing to read aloud yet");
            return Reply::Unchanged;
        }
        self.stop_voice();
        self.aloud.follow = false;
        self.start_result(index);
        Reply::Done
    }

    /// Reads the page aloud: every block read in order, each said as it lands --
    /// or, when the page's blocks are already read, said from the first.
    pub(super) fn read_aloud(&mut self) -> Reply {
        if !self.has_voice() {
            return Reply::Unchanged;
        }
        self.stop_voice();
        if self.results_in_page_order() && !self.results().is_empty() && !self.reads_busy() {
            self.aloud.follow = true;
            self.start_result(0);
            return Reply::Done;
        }
        if !self.block_mode() && self.set_block_mode(true) == Reply::Unchanged {
            // Not ready: set_block_mode said why.
            return Reply::Unchanged;
        }
        if self.read_all() == Reply::Unchanged {
            return Reply::Unchanged;
        }
        self.aloud.follow = true;
        self.aloud.next = Some(0);
        self.aloud.generation = self.results_generation();
        Reply::Done
    }

    pub(super) fn stop_speaking(&mut self) -> Reply {
        let busy = self.aloud.result.is_some() || self.aloud.follow;
        self.stop_voice();
        if busy {
            Reply::Done
        } else {
            Reply::Unchanged
        }
    }

    /// Holds the voice mid-word where it can; else stops it, to say the sentence
    /// again from its start on resuming.
    pub(super) fn pause_speaking(&mut self) -> Reply {
        if self.aloud.saying.is_none() || self.aloud.paused {
            return Reply::Unchanged;
        }
        if self.aloud.voice.as_mut().is_some_and(|v| v.pause()) {
            self.aloud.paused = true;
            return Reply::Done;
        }
        let Some((_, sentence)) = self.aloud.saying.take() else {
            return Reply::Unchanged;
        };
        self.aloud.sentences.push_front(sentence);
        self.aloud.paused = true;
        if let Some(v) = &mut self.aloud.voice {
            v.stop();
        }
        Reply::Done
    }

    pub(super) fn resume_speaking(&mut self) -> Reply {
        if !self.aloud.paused {
            return Reply::Unchanged;
        }
        self.aloud.paused = false;
        // Held mid-word: it goes on. Else the sentence is said again.
        if self.aloud.saying.is_some() && self.aloud.voice.as_mut().is_some_and(|v| v.resume()) {
            return Reply::Done;
        }
        self.say_next();
        Reply::Done
    }

    /// The next (`1`) or previous (`-1`) result, from its start.
    pub(super) fn skip_speech(&mut self, delta: i32) -> Reply {
        let Some(current) = self.aloud.result else {
            return Reply::Unchanged;
        };
        let target = current as i64 + delta as i64;
        if target < 0 || target as usize >= self.results().len() {
            return Reply::Unchanged;
        }
        let follow = self.aloud.follow;
        self.stop_voice();
        self.aloud.follow = follow;
        self.start_result(target as usize);
        Reply::Done
    }

    /// Stops the voice and forgets what was to be said.
    fn stop_voice(&mut self) {
        let generation = self.results_generation();
        let a = &mut self.aloud;
        if a.saying.take().is_some() {
            if let Some(v) = &mut a.voice {
                v.stop();
            }
        }
        a.sentences.clear();
        a.result = None;
        a.paused = false;
        a.follow = false;
        a.next = None;
        a.generation = generation;
    }

    /// Loads result `index`'s sentences and starts saying them.
    fn start_result(&mut self, index: usize) {
        let sentences = match self.results().get(index).map(|r| &r.result) {
            Some(Ok(t)) => match t.readings.first() {
                Some(reading) => {
                    let voice = self.aloud.voice.as_ref();
                    utterances(&reading.text, |m| voice.and_then(|v| v.maths(m)))
                }
                None => vec![said("No answer.")],
            },
            Some(Err(_)) => vec![said("Could not read it.")],
            None => Vec::new(),
        };
        let generation = self.results_generation();
        let a = &mut self.aloud;
        a.result = Some(index);
        a.sentences = sentences.into();
        a.next = a.follow.then_some(index + 1);
        a.generation = generation;
        a.paused = false;
        self.say_next();
    }

    /// Says the next sentence, or goes on to the next result when following.
    fn say_next(&mut self) {
        if self.aloud.paused {
            return;
        }
        if let Some(sentence) = self.aloud.sentences.pop_front() {
            self.aloud.next_id += 1;
            let id = self.aloud.next_id;
            let said = match &mut self.aloud.voice {
                Some(v) => v.say(id, &sentence.text),
                None => Ok(()),
            };
            match said {
                Ok(()) => {
                    self.aloud.saying = Some((id, sentence));
                    // The next sentence made while this one is said.
                    if let (Some(v), Some(next)) =
                        (&mut self.aloud.voice, self.aloud.sentences.front())
                    {
                        v.prepare(&next.text);
                    }
                }
                Err(e) => {
                    self.notice(Level::Warning, format!("could not speak: {e:#}"));
                    self.stop_voice();
                }
            }
            return;
        }
        // This result is said.
        match self.aloud.next {
            Some(n) if n < self.results().len() => self.start_result(n),
            // Still being read: wait for it.
            Some(_) if self.reads_busy() => self.aloud.result = None,
            _ => self.stop_voice(),
        }
    }

    /// What happened since the last pump: an utterance ended, a result to go on
    /// to landed, the list was emptied, a new reading to say.
    pub(super) fn pump_speech(&mut self) {
        if self.aloud.voice.is_none() {
            return;
        }
        let generation = self.results_generation();
        if generation != self.aloud.generation {
            // A new list: what was being said is gone -- unless the page is being
            // read aloud and has not started (its "read all" emptied the list).
            let waiting =
                self.aloud.follow && self.aloud.next == Some(0) && self.aloud.result.is_none();
            if !waiting {
                self.stop_voice();
            }
            self.aloud.generation = generation;
            self.aloud.heard = 0;
        }
        let ended = self.aloud.voice.as_ref().map_or(0, |v| v.ended());
        if self
            .aloud
            .saying
            .as_ref()
            .is_some_and(|(id, _)| ended >= *id)
        {
            self.aloud.saying = None;
            self.say_next();
        }
        // Following and waiting for the next result.
        if self.aloud.saying.is_none() && self.aloud.result.is_none() && self.aloud.follow {
            match self.aloud.next {
                Some(n) if n < self.results().len() => self.start_result(n),
                _ if !self.reads_busy() => self.stop_voice(),
                _ => {}
            }
        }
        // A new reading, said as it arrives.
        let n = self.results().len();
        if n > self.aloud.heard {
            let idle = self.aloud.result.is_none() && !self.aloud.follow;
            if idle && self.config().speech.speak_new {
                self.start_result(n - 1);
            }
            self.aloud.heard = n;
        }
    }
}

fn said(text: &str) -> Utterance {
    Utterance {
        text: text.into(),
        start: 0,
        end: 0,
    }
}
