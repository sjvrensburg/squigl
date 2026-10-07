//! Kokoro-82M (hexgrad, Apache-2.0; onnx-community's fp16 export) as a voice:
//! text to phonemes by squigl-misaki (Misaki's English G2P, ported), phonemes to
//! 24 kHz speech here, on the CPU (spike S8: about four times real time on 20
//! threads, 2.5 on 4; and on the CPU it need not wait for a read on WebGPU, so
//! it does not take [`crate::RUNTIME`] to run).

use crate::lifecycle::{Lifecycle, OnDevice, State};
use crate::models;
use crate::Device;
use anyhow::{anyhow, bail, Context, Result};
use ndarray::{Array1, Array2};
use ort::session::Session;
use ort::value::Tensor;
use squigl_engine::model::{ModelContext, ModelPhase};
use squigl_misaki::{Fallback, G2p, Lexicon};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 24_000;

/// Phonemes Kokoro takes at once (its context, less the two ends).
const MAX_PHONEMES: usize = 510;
/// A longer sentence is cut, at MathCAT's pauses first, into pieces under this.
const PIECE: usize = 400;

/// The voices downloaded with the model: id, and a name for people.
pub const VOICES: [(&str, &str); 5] = [
    ("af_heart", "Heart (American English, female)"),
    ("af_bella", "Bella (American English, female)"),
    ("af_sarah", "Sarah (American English, female)"),
    ("am_michael", "Michael (American English, male)"),
    ("am_adam", "Adam (American English, male)"),
];
pub const DEFAULT_VOICE: &str = "af_heart";

/// Kokoro's phoneme vocabulary (hexgrad/Kokoro-82M `config.json`).
const VOCAB: [(char, i64); 114] = [
    (';', 1),
    (':', 2),
    (',', 3),
    ('.', 4),
    ('!', 5),
    ('?', 6),
    ('—', 9),
    ('…', 10),
    ('"', 11),
    ('(', 12),
    (')', 13),
    ('“', 14),
    ('”', 15),
    (' ', 16),
    ('̃', 17),
    ('ʣ', 18),
    ('ʥ', 19),
    ('ʦ', 20),
    ('ʨ', 21),
    ('ᵝ', 22),
    ('ꭧ', 23),
    ('A', 24),
    ('I', 25),
    ('O', 31),
    ('Q', 33),
    ('S', 35),
    ('T', 36),
    ('W', 39),
    ('Y', 41),
    ('ᵊ', 42),
    ('a', 43),
    ('b', 44),
    ('c', 45),
    ('d', 46),
    ('e', 47),
    ('f', 48),
    ('h', 50),
    ('i', 51),
    ('j', 52),
    ('k', 53),
    ('l', 54),
    ('m', 55),
    ('n', 56),
    ('o', 57),
    ('p', 58),
    ('q', 59),
    ('r', 60),
    ('s', 61),
    ('t', 62),
    ('u', 63),
    ('v', 64),
    ('w', 65),
    ('x', 66),
    ('y', 67),
    ('z', 68),
    ('ɑ', 69),
    ('ɐ', 70),
    ('ɒ', 71),
    ('æ', 72),
    ('β', 75),
    ('ɔ', 76),
    ('ɕ', 77),
    ('ç', 78),
    ('ɖ', 80),
    ('ð', 81),
    ('ʤ', 82),
    ('ə', 83),
    ('ɚ', 85),
    ('ɛ', 86),
    ('ɜ', 87),
    ('ɟ', 90),
    ('ɡ', 92),
    ('ɥ', 99),
    ('ɨ', 101),
    ('ɪ', 102),
    ('ʝ', 103),
    ('ɯ', 110),
    ('ɰ', 111),
    ('ŋ', 112),
    ('ɳ', 113),
    ('ɲ', 114),
    ('ɴ', 115),
    ('ø', 116),
    ('ɸ', 118),
    ('θ', 119),
    ('œ', 120),
    ('ɹ', 123),
    ('ɾ', 125),
    ('ɻ', 126),
    ('ʁ', 128),
    ('ɽ', 129),
    ('ʂ', 130),
    ('ʃ', 131),
    ('ʈ', 132),
    ('ʧ', 133),
    ('ʊ', 135),
    ('ʋ', 136),
    ('ʌ', 138),
    ('ɣ', 139),
    ('ɤ', 140),
    ('χ', 142),
    ('ʎ', 143),
    ('ʒ', 147),
    ('ʔ', 148),
    ('ˈ', 156),
    ('ˌ', 157),
    ('ː', 158),
    ('ʰ', 162),
    ('ʲ', 164),
    ('↓', 169),
    ('→', 171),
    ('↗', 172),
    ('↘', 173),
    ('ᵻ', 177),
];

/// Symbols squigl writes out before phonemising: Misaki reads `=` as "x".
fn spoken_symbols(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '=' => out.push_str(" equals "),
            '×' => out.push_str(" times "),
            '→' => out.push_str(" to "),
            '≈' => out.push_str(" about "),
            '≤' => out.push_str(" at most "),
            '≥' => out.push_str(" at least "),
            '<' => out.push_str(" less than "),
            '>' => out.push_str(" more than "),
            _ => out.push(c),
        }
    }
    out
}

/// `line` (phonemes) in pieces of at most `max`, cut at "; ", then ", ", then
/// spaces.
fn pieces(line: &str, max: usize) -> Vec<String> {
    if line.chars().count() <= max {
        return vec![line.to_string()];
    }
    for sep in ["; ", ", ", " "] {
        let mut out: Vec<String> = Vec::new();
        for piece in line.split_inclusive(sep) {
            match out.last_mut() {
                Some(last) if last.chars().count() + piece.chars().count() <= max => {
                    last.push_str(piece)
                }
                _ => out.push(piece.to_string()),
            }
        }
        if out.iter().all(|p| p.chars().count() <= max) {
            return out;
        }
    }
    line.chars()
        .collect::<Vec<_>>()
        .chunks(max)
        .map(|c| c.iter().collect())
        .collect()
}

/// The loaded model, its voices and the phonemiser.
pub struct Kokoro {
    session: Session,
    voices: HashMap<String, Vec<f32>>,
    vocab: HashMap<char, i64>,
    g2p: G2p,
}

impl OnDevice for Kokoro {
    fn device(&self) -> Device {
        Device::Cpu
    }
}

impl Kokoro {
    /// From the model's directory, as [`models::KOKORO`] lays it out.
    pub fn load(dir: &Path) -> Result<Self> {
        let read =
            |p: &str| std::fs::read_to_string(dir.join(p)).with_context(|| format!("reading {p}"));
        let g2p = G2p::new(
            Lexicon::new(
                &read("misaki/us_gold.json")?,
                &read("misaki/us_silver.json")?,
            )?,
            Some(Fallback::load(&dir.join("misaki-fallback"))?),
        );
        let mut voices = HashMap::new();
        for (id, _) in VOICES {
            let bytes = std::fs::read(dir.join(format!("voices/{id}.bin")))?;
            if bytes.len() != MAX_PHONEMES * 256 * 4 {
                bail!("voice {id} is {} bytes", bytes.len());
            }
            voices.insert(
                id.to_string(),
                bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().expect("four bytes")))
                    .collect(),
            );
        }
        let session = crate::open_session(&dir.join("onnx/model_fp16.onnx"), Device::Cpu)?;
        Ok(Self {
            session,
            voices,
            vocab: VOCAB.into_iter().collect(),
            g2p,
        })
    }

    /// `text` as Kokoro's phonemes.
    pub fn phonemes(&self, text: &str) -> String {
        self.g2p.phonemes(&spoken_symbols(text))
    }

    /// `text` said by `voice` at `speed` (1 is normal): mono samples at
    /// [`SAMPLE_RATE`].
    pub fn speak(&mut self, text: &str, voice: &str, speed: f32) -> Result<Vec<f32>> {
        let style = self
            .voices
            .get(voice)
            .or_else(|| self.voices.get(DEFAULT_VOICE))
            .ok_or_else(|| anyhow!("no voice {voice}"))?
            .clone();
        let phonemes = self.phonemes(text);
        let mut audio = Vec::new();
        for piece in pieces(phonemes.trim(), PIECE) {
            let mut ids: Vec<i64> = vec![0];
            ids.extend(piece.chars().filter_map(|c| self.vocab.get(&c).copied()));
            ids.push(0);
            let n = ids.len() - 2;
            if n == 0 {
                continue;
            }
            let s = Array2::from_shape_vec((1, 256), style[n * 256..(n + 1) * 256].to_vec())?;
            let outputs = self.session.run(ort::inputs![
                "input_ids" => Tensor::from_array(Array2::from_shape_vec((1, ids.len()), ids)?)?,
                "style" => Tensor::from_array(s)?,
                "speed" => Tensor::from_array(Array1::from_vec(vec![speed]))?,
            ])?;
            audio.extend(outputs[0].try_extract_array::<f32>()?.iter());
        }
        Ok(audio)
    }
}

/// Kokoro's life: downloaded on request (the person agrees), then loaded.
pub struct KokoroService {
    life: Arc<Lifecycle<Kokoro>>,
}

impl KokoroService {
    pub fn new(ctx: &ModelContext) -> Self {
        Self {
            life: Lifecycle::new(&models::KOKORO, "Kokoro", ctx, |dir, report| {
                report(ModelPhase::Loading {
                    device: Device::Cpu.name().to_string(),
                });
                Kokoro::load(dir)
            }),
        }
    }

    pub fn name(&self) -> &'static str {
        "Kokoro"
    }

    pub fn phase(&self) -> ModelPhase {
        self.life.phase.get()
    }

    pub fn prepare(&self) {
        self.life.prepare();
    }

    pub fn cancel(&self) {
        self.life.cancel();
    }

    pub fn ready(&self) -> bool {
        matches!(*self.life.state.lock().unwrap(), State::Ready(_))
    }

    /// As [`Kokoro::speak`]; an error when the model is not ready.
    pub fn speak(&self, text: &str, voice: &str, speed: f32) -> Result<Vec<f32>> {
        let mut state = self.life.state.lock().unwrap();
        match &mut *state {
            State::Ready(k) => k.speak(text, voice, speed),
            _ => Err(anyhow!("Kokoro is not ready: {}", self.life.not_ready())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_cut_at_pauses_and_symbols_said() {
        let line = format!("{}; {}", "a".repeat(300), "b".repeat(300));
        let p = pieces(&line, 400);
        assert_eq!(p.len(), 2);
        assert!(p.iter().all(|x| x.chars().count() <= 400));
        assert_eq!(spoken_symbols("a=b"), "a equals b");
        assert_eq!(VOCAB.len(), 114);
    }

    #[test]
    #[ignore = "needs the Kokoro model (~175 MB) in $SQUIGL_MODEL_DIR/kokoro"]
    fn kokoro_says_a_sentence() {
        let dir =
            std::path::PathBuf::from(std::env::var_os("SQUIGL_MODEL_DIR").unwrap()).join("kokoro");
        let mut k = Kokoro::load(&dir).unwrap();
        let p = k.phonemes("The entropy is 2 bits.");
        assert_eq!(p, "ði ˈɛntɹəpi ɪz tˈu bˈɪts.");
        let audio = k
            .speak("The entropy is two bits.", "af_heart", 1.0)
            .unwrap();
        let secs = audio.len() as f32 / SAMPLE_RATE as f32;
        let rms = (audio.iter().map(|x| x * x).sum::<f32>() / audio.len() as f32).sqrt();
        assert!(
            (1.0..4.0).contains(&secs) && rms > 0.01,
            "{secs} s, rms {rms}"
        );
    }
}
