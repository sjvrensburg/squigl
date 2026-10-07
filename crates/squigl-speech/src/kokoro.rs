//! Kokoro as a [`Voice`]: each sentence made by squigl-models' Kokoro (on the CPU)
//! on a thread of this voice's own, the next one made while this one plays, and
//! played through cpal at the output's own rate -- so it can be held mid-word,
//! which the system voices cannot do everywhere.

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use squigl_engine::engine::Waker;
use squigl_engine::model::ModelPhase;
use squigl_engine::speech::{Voice, VoiceInfo};
use squigl_models::kokoro::{KokoroService, DEFAULT_VOICE, SAMPLE_RATE, VOICES};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// How many sentences made ahead are kept.
const CACHE: usize = 8;

/// What the output plays: one utterance's samples at the output's rate.
#[derive(Default)]
struct Player {
    samples: Vec<f32>,
    pos: usize,
    id: u64,
    playing: bool,
    paused: bool,
}

enum Msg {
    Say(u64, String),
    Prepare(String),
    Configure(String, f32),
}

/// What the voice and its thread share.
struct Shared {
    player: Mutex<Player>,
    /// The utterance wanted now (0: none): a sentence made after it was stopped
    /// is not played.
    wanted: AtomicU64,
    ended: AtomicU64,
    wake: Waker,
}

impl Shared {
    fn end(&self, id: u64) {
        self.ended.fetch_max(id, Ordering::AcqRel);
        (self.wake)();
    }
}

pub struct KokoroVoice {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    service: Arc<KokoroService>,
}

/// Windowed-sinc resampling of `x` from `from` to `to` Hz (Lanczos, a = 8): an
/// utterance at a time, so no state to carry between calls.
fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    const A: f64 = 8.0;
    let ratio = to as f64 / from as f64;
    // Going down, the kernel widens to cut what the new rate cannot carry.
    let scale = ratio.min(1.0);
    let n = (x.len() as f64 * ratio).round() as usize;
    let sinc = |t: f64| {
        if t.abs() < 1e-9 {
            1.0
        } else {
            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
        }
    };
    (0..n)
        .map(|i| {
            let centre = i as f64 / ratio;
            let reach = A / scale;
            let lo = (centre - reach).ceil().max(0.0) as usize;
            let hi = ((centre + reach).floor() as usize).min(x.len() - 1);
            let (mut acc, mut norm) = (0.0, 0.0);
            for (j, &v) in x.iter().enumerate().take(hi + 1).skip(lo) {
                let t = (j as f64 - centre) * scale;
                let w = sinc(t) * sinc(t / A);
                acc += v as f64 * w;
                norm += w;
            }
            if norm.abs() > 1e-9 {
                (acc / norm) as f32
            } else {
                0.0
            }
        })
        .collect()
}

/// Fills an output buffer from the player: the utterance on every channel, or
/// silence; at its end the utterance is over.
fn fill<T: cpal::SizedSample + cpal::FromSample<f32>>(
    data: &mut [T],
    channels: usize,
    shared: &Shared,
) {
    let mut ended = None;
    {
        let mut p = shared.player.lock().unwrap();
        for frame in data.chunks_mut(channels) {
            let v = if p.playing && !p.paused {
                match p.samples.get(p.pos) {
                    Some(&s) => {
                        p.pos += 1;
                        s
                    }
                    None => {
                        p.playing = false;
                        ended = Some(p.id);
                        0.0
                    }
                }
            } else {
                0.0
            };
            for out in frame {
                *out = T::from_sample(v);
            }
        }
    }
    if let Some(id) = ended {
        shared.end(id);
    }
}

fn open_output(shared: Arc<Shared>) -> Result<(cpal::Stream, u32)> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no sound output")?;
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let (rate, channels) = (config.sample_rate.0, config.channels as usize);
    let err = |e| log::warn!("sound output: {e}");
    macro_rules! build {
        ($t:ty) => {{
            let s = Arc::clone(&shared);
            device.build_output_stream(
                &config,
                move |d: &mut [$t], _| fill(d, channels, &s),
                err,
                None,
            )?
        }};
    }
    let stream = match format {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::I32 => build!(i32),
        other => bail!("sound output in {other:?}"),
    };
    stream.play()?;
    Ok((stream, rate))
}

fn run(
    rx: Receiver<Msg>,
    shared: Arc<Shared>,
    service: Arc<KokoroService>,
    ready: Sender<Result<()>>,
) {
    let (stream, rate) = match open_output(Arc::clone(&shared)) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let _stream = stream;
    let (mut voice, mut speed) = (DEFAULT_VOICE.to_string(), 1.0);
    let mut made: HashMap<String, Vec<f32>> = HashMap::new();
    let make = |text: &str,
                voice: &str,
                speed: f32,
                made: &mut HashMap<String, Vec<f32>>|
     -> Option<Vec<f32>> {
        if let Some(s) = made.remove(text) {
            return Some(s);
        }
        match service.speak(text, voice, speed) {
            Ok(audio) => Some(resample(&audio, SAMPLE_RATE, rate)),
            Err(e) => {
                log::warn!("Kokoro: {e:#}");
                None
            }
        }
    };
    for msg in rx {
        match msg {
            Msg::Say(id, text) => {
                let samples = make(&text, &voice, speed, &mut made);
                if shared.wanted.load(Ordering::Acquire) != id {
                    // Stopped, or another sentence asked for, while it was made.
                    continue;
                }
                match samples {
                    Some(samples) => {
                        let mut p = shared.player.lock().unwrap();
                        *p = Player {
                            samples,
                            pos: 0,
                            id,
                            playing: true,
                            paused: false,
                        };
                    }
                    None => shared.end(id),
                }
            }
            Msg::Prepare(text) => {
                if !made.contains_key(&text) {
                    if let Some(s) = make(&text, &voice, speed, &mut made) {
                        if made.len() >= CACHE {
                            made.clear();
                        }
                        made.insert(text, s);
                    }
                }
            }
            Msg::Configure(v, s) => {
                if v != voice || s != speed {
                    made.clear();
                }
                voice = v;
                speed = s;
            }
        }
    }
}

impl KokoroVoice {
    /// Opens the sound output and starts the voice's thread; an error when there
    /// is no output to play to.
    pub fn start(service: Arc<KokoroService>, wake: Waker) -> Result<Self> {
        let shared = Arc::new(Shared {
            player: Mutex::default(),
            wanted: AtomicU64::new(0),
            ended: AtomicU64::new(0),
            wake,
        });
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (s, k) = (Arc::clone(&shared), Arc::clone(&service));
        std::thread::Builder::new()
            .name("kokoro".into())
            .spawn(move || run(rx, s, k, ready_tx))?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the Kokoro thread ended"))??;
        Ok(Self {
            tx,
            shared,
            service,
        })
    }

    pub fn ready(&self) -> bool {
        self.service.ready()
    }

    /// Kokoro's voices, by the ids `[speech].voice` holds ("kokoro:af_heart").
    pub fn voice_list() -> Vec<VoiceInfo> {
        VOICES
            .iter()
            .map(|(id, name)| VoiceInfo {
                id: format!("kokoro:{id}"),
                name: format!("Kokoro: {name}"),
                language: Some("en-US".into()),
            })
            .collect()
    }

    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }
}

impl Voice for KokoroVoice {
    fn say(&mut self, id: u64, text: &str) -> Result<()> {
        // Whatever was playing is cut off: it is over.
        let previous = {
            let mut p = self.shared.player.lock().unwrap();
            let was = p.playing.then_some(p.id);
            p.playing = false;
            was
        };
        if let Some(prev) = previous {
            self.shared.end(prev);
        }
        self.shared.wanted.store(id, Ordering::Release);
        self.send(Msg::Say(id, text.into()));
        Ok(())
    }

    fn stop(&mut self) {
        let wanted = self.shared.wanted.swap(0, Ordering::AcqRel);
        self.shared.player.lock().unwrap().playing = false;
        self.shared.end(wanted);
    }

    fn ended(&self) -> u64 {
        self.shared.ended.load(Ordering::Acquire)
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        Self::voice_list()
    }

    fn configure(&mut self, voice: Option<&str>, rate: f32) -> Result<()> {
        let id = voice
            .and_then(|v| v.strip_prefix("kokoro:"))
            .filter(|v| VOICES.iter().any(|(id, _)| id == v))
            .unwrap_or(DEFAULT_VOICE);
        self.send(Msg::Configure(id.into(), rate.clamp(0.5, 2.0)));
        Ok(())
    }

    fn maths(&self, _: &str) -> Option<String> {
        None
    }

    fn prepare(&mut self, text: &str) {
        self.send(Msg::Prepare(text.into()));
    }

    fn pause(&mut self) -> bool {
        let mut p = self.shared.player.lock().unwrap();
        p.paused = true;
        true
    }

    fn resume(&mut self) -> bool {
        self.shared.player.lock().unwrap().paused = false;
        true
    }

    fn model(&self) -> Option<(String, ModelPhase)> {
        Some((self.service.name().to_string(), self.service.phase()))
    }

    fn prepare_model(&self) {
        self.service.prepare();
    }

    fn cancel_model(&self) {
        self.service.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_a_tone_and_its_length() {
        // 440 Hz for a tenth of a second at 24 kHz, to 48 kHz and to 44.1 kHz.
        let tone: Vec<f32> = (0..2400)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 24_000.0).sin())
            .collect();
        for to in [48_000, 44_100] {
            let out = resample(&tone, 24_000, to);
            assert_eq!(out.len(), (2400.0 * to as f64 / 24_000.0).round() as usize);
            // Away from the ends, the same tone at the new rate.
            for i in (out.len() / 4..3 * out.len() / 4).step_by(37) {
                let want = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / to as f32).sin();
                assert!(
                    (out[i] - want).abs() < 0.01,
                    "{to} Hz, sample {i}: {} vs {want}",
                    out[i]
                );
            }
        }
    }
}
