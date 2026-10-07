//! Misaki's fallback for words in neither dictionary: `PeterReid/graphemes_to_phonemes_en_us`
//! (Apache-2.0), a one-layer BART (d_model 128, one head, 751k parameters) from
//! letters to phonemes, run here in plain Rust from its `model.safetensors` --
//! small enough that a word takes milliseconds without any runtime. Decoding is
//! greedy with the length cap Misaki's `generate` call gets from transformers'
//! defaults (20 tokens), so its readings are Misaki's.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;

const D: usize = 128;
const START: usize = 1;
const EOS: usize = 2;
const UNKNOWN: usize = 3;
/// transformers' default `max_length`, which Misaki's `generate()` call keeps.
const MAX_LENGTH: usize = 20;
/// BART's learned positions start at 2.
const POSITION_OFFSET: usize = 2;

struct Linear {
    w: Vec<f32>,
    b: Vec<f32>,
    out: usize,
    inp: usize,
}

impl Linear {
    fn apply(&self, x: &[f32]) -> Vec<f32> {
        (0..self.out)
            .map(|o| {
                let row = &self.w[o * self.inp..(o + 1) * self.inp];
                self.b[o] + row.iter().zip(x).map(|(a, b)| a * b).sum::<f32>()
            })
            .collect()
    }
}

struct Norm {
    w: Vec<f32>,
    b: Vec<f32>,
}

impl Norm {
    fn apply(&self, x: &[f32]) -> Vec<f32> {
        let n = x.len() as f32;
        let mean = x.iter().sum::<f32>() / n;
        let var = x.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n;
        let inv = 1.0 / (var + 1e-5).sqrt();
        x.iter()
            .enumerate()
            .map(|(i, v)| (v - mean) * inv * self.w[i] + self.b[i])
            .collect()
    }
}

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
}

impl Attention {
    /// One head over all of `keys`, or only up to each query's own place when
    /// `causal`.
    fn apply(&self, queries: &[Vec<f32>], keys: &[Vec<f32>], causal: bool) -> Vec<Vec<f32>> {
        let scale = 1.0 / (D as f32).sqrt();
        let k: Vec<Vec<f32>> = keys.iter().map(|x| self.k.apply(x)).collect();
        let v: Vec<Vec<f32>> = keys.iter().map(|x| self.v.apply(x)).collect();
        queries
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let q: Vec<f32> = self.q.apply(x).iter().map(|a| a * scale).collect();
                let n = if causal { i + 1 } else { k.len() };
                let scores: Vec<f32> = (0..n)
                    .map(|j| q.iter().zip(&k[j]).map(|(a, b)| a * b).sum())
                    .collect();
                let max = scores.iter().cloned().fold(f32::MIN, f32::max);
                let exps: Vec<f32> = scores.iter().map(|s| (s - max).exp()).collect();
                let total: f32 = exps.iter().sum();
                let mut mixed = vec![0.0; D];
                for (j, e) in exps.iter().enumerate() {
                    for (m, val) in mixed.iter_mut().zip(&v[j]) {
                        *m += e / total * val;
                    }
                }
                self.out.apply(&mixed)
            })
            .collect()
    }
}

fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + libm::erff(x / std::f32::consts::SQRT_2))
}

struct FeedForward {
    fc1: Linear,
    fc2: Linear,
}

impl FeedForward {
    fn apply(&self, x: &[f32]) -> Vec<f32> {
        let h: Vec<f32> = self.fc1.apply(x).into_iter().map(gelu).collect();
        self.fc2.apply(&h)
    }
}

fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

/// The fallback model, ready to phonemise words.
pub struct Fallback {
    graphemes: HashMap<char, usize>,
    phonemes: Vec<char>,
    shared: Vec<f32>,
    vocab: usize,
    final_bias: Vec<f32>,
    enc_pos: Vec<f32>,
    enc_norm_emb: Norm,
    enc_attn: Attention,
    enc_norm_attn: Norm,
    enc_ff: FeedForward,
    enc_norm_ff: Norm,
    dec_pos: Vec<f32>,
    dec_norm_emb: Norm,
    dec_attn: Attention,
    dec_norm_attn: Norm,
    dec_cross: Attention,
    dec_norm_cross: Norm,
    dec_ff: FeedForward,
    dec_norm_ff: Norm,
}

/// A tensor's shape and values.
type Tensor = (Vec<usize>, Vec<f32>);

/// The tensors of a `.safetensors` file (all float32 here), by name.
fn read_safetensors(path: &Path) -> Result<HashMap<String, Tensor>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let n = u64::from_le_bytes(bytes.get(..8).context("too short")?.try_into()?) as usize;
    let header: HashMap<String, serde_json::Value> =
        serde_json::from_slice(bytes.get(8..8 + n).context("bad header")?)?;
    let data = &bytes[8 + n..];
    let mut out = HashMap::new();
    for (name, info) in header {
        if name == "__metadata__" {
            continue;
        }
        if info["dtype"] != "F32" {
            bail!("{name} is {}, not F32", info["dtype"]);
        }
        let shape: Vec<usize> = info["shape"]
            .as_array()
            .context("shape")?
            .iter()
            .map(|d| d.as_u64().unwrap_or(0) as usize)
            .collect();
        let offsets = info["data_offsets"].as_array().context("offsets")?;
        let (a, b) = (
            offsets[0].as_u64().unwrap_or(0) as usize,
            offsets[1].as_u64().unwrap_or(0) as usize,
        );
        let raw = data.get(a..b).context("offsets out of range")?;
        let values = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().expect("four bytes")))
            .collect();
        out.insert(name, (shape, values));
    }
    Ok(out)
}

impl Fallback {
    /// From the model's directory: `config.json` (its letter and phoneme lists) and
    /// `model.safetensors`.
    pub fn load(dir: &Path) -> Result<Self> {
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        // A string (or a list of one-character strings), one symbol a character.
        let chars = |key: &str| -> Result<Vec<char>> {
            match &config[key] {
                serde_json::Value::String(s) => Ok(s.chars().collect()),
                serde_json::Value::Array(a) => a
                    .iter()
                    .map(|c| {
                        c.as_str()
                            .and_then(|s| s.chars().next())
                            .context("a character")
                    })
                    .collect(),
                _ => bail!("{key} missing from config.json"),
            }
        };
        let graphemes = chars("grapheme_chars")?
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c, i))
            .collect();
        let phonemes = chars("phoneme_chars")?;
        let mut t = read_safetensors(&dir.join("model.safetensors"))?;
        let mut take = |name: &str| -> Result<Tensor> {
            t.remove(name).with_context(|| format!("{name} missing"))
        };
        let mut linear = |name: &str| -> Result<Linear> {
            let (shape, w) = take(&format!("{name}.weight"))?;
            let (_, b) = take(&format!("{name}.bias"))?;
            Ok(Linear {
                w,
                b,
                out: shape[0],
                inp: shape[1],
            })
        };
        let attention =
            |p: &str, linear: &mut dyn FnMut(&str) -> Result<Linear>| -> Result<Attention> {
                Ok(Attention {
                    q: linear(&format!("{p}.q_proj"))?,
                    k: linear(&format!("{p}.k_proj"))?,
                    v: linear(&format!("{p}.v_proj"))?,
                    out: linear(&format!("{p}.out_proj"))?,
                })
            };
        let enc_attn = attention("model.encoder.layers.0.self_attn", &mut linear)?;
        let enc_ff = FeedForward {
            fc1: linear("model.encoder.layers.0.fc1")?,
            fc2: linear("model.encoder.layers.0.fc2")?,
        };
        let dec_attn = attention("model.decoder.layers.0.self_attn", &mut linear)?;
        let dec_cross = attention("model.decoder.layers.0.encoder_attn", &mut linear)?;
        let dec_ff = FeedForward {
            fc1: linear("model.decoder.layers.0.fc1")?,
            fc2: linear("model.decoder.layers.0.fc2")?,
        };
        let mut norm = |name: &str| -> Result<Norm> {
            Ok(Norm {
                w: take(&format!("{name}.weight"))?.1,
                b: take(&format!("{name}.bias"))?.1,
            })
        };
        let enc_norm_emb = norm("model.encoder.layernorm_embedding")?;
        let enc_norm_attn = norm("model.encoder.layers.0.self_attn_layer_norm")?;
        let enc_norm_ff = norm("model.encoder.layers.0.final_layer_norm")?;
        let dec_norm_emb = norm("model.decoder.layernorm_embedding")?;
        let dec_norm_attn = norm("model.decoder.layers.0.self_attn_layer_norm")?;
        let dec_norm_cross = norm("model.decoder.layers.0.encoder_attn_layer_norm")?;
        let dec_norm_ff = norm("model.decoder.layers.0.final_layer_norm")?;
        let (shape, shared) = take("model.shared.weight")?;
        Ok(Self {
            graphemes,
            phonemes,
            vocab: shape[0],
            shared,
            final_bias: take("final_logits_bias")?.1,
            enc_pos: take("model.encoder.embed_positions.weight")?.1,
            dec_pos: take("model.decoder.embed_positions.weight")?.1,
            enc_norm_emb,
            enc_attn,
            enc_norm_attn,
            enc_ff,
            enc_norm_ff,
            dec_norm_emb,
            dec_attn,
            dec_norm_attn,
            dec_cross,
            dec_norm_cross,
            dec_ff,
            dec_norm_ff,
        })
    }

    fn embed(&self, ids: &[usize], positions: &[f32], norm: &Norm) -> Vec<Vec<f32>> {
        ids.iter()
            .enumerate()
            .map(|(t, &id)| {
                let tok = &self.shared[id * D..(id + 1) * D];
                let pos = &positions[(t + POSITION_OFFSET) * D..(t + POSITION_OFFSET + 1) * D];
                norm.apply(&add(tok, pos))
            })
            .collect()
    }

    fn encode(&self, ids: &[usize]) -> Vec<Vec<f32>> {
        let h = self.embed(ids, &self.enc_pos, &self.enc_norm_emb);
        let a = self.enc_attn.apply(&h, &h, false);
        let h: Vec<Vec<f32>> = h
            .iter()
            .zip(&a)
            .map(|(x, y)| self.enc_norm_attn.apply(&add(x, y)))
            .collect();
        h.iter()
            .map(|x| self.enc_norm_ff.apply(&add(x, &self.enc_ff.apply(x))))
            .collect()
    }

    /// The next token's scores after `ids`, given the encoded word.
    fn next_logits(&self, ids: &[usize], enc: &[Vec<f32>]) -> Vec<f32> {
        let h = self.embed(ids, &self.dec_pos, &self.dec_norm_emb);
        let a = self.dec_attn.apply(&h, &h, true);
        let h: Vec<Vec<f32>> = h
            .iter()
            .zip(&a)
            .map(|(x, y)| self.dec_norm_attn.apply(&add(x, y)))
            .collect();
        let c = self.dec_cross.apply(&h, enc, false);
        let h: Vec<Vec<f32>> = h
            .iter()
            .zip(&c)
            .map(|(x, y)| self.dec_norm_cross.apply(&add(x, y)))
            .collect();
        let last = h.last().expect("at least the start token");
        let last = self.dec_norm_ff.apply(&add(last, &self.dec_ff.apply(last)));
        (0..self.vocab)
            .map(|v| {
                self.final_bias[v]
                    + self.shared[v * D..(v + 1) * D]
                        .iter()
                        .zip(&last)
                        .map(|(a, b)| a * b)
                        .sum::<f32>()
            })
            .collect()
    }

    /// `word`'s phonemes, as Misaki's fallback says them.
    pub fn phonemes(&self, word: &str) -> String {
        // Positions run out at 64 (start and end included).
        let max_letters = self.enc_pos.len() / D - POSITION_OFFSET - 2;
        let ids: Vec<usize> = std::iter::once(START)
            .chain(
                word.chars()
                    .take(max_letters)
                    .map(|c| *self.graphemes.get(&c).unwrap_or(&UNKNOWN)),
            )
            .chain(std::iter::once(EOS))
            .collect();
        let enc = self.encode(&ids);
        let mut out = vec![START];
        while out.len() < MAX_LENGTH {
            // transformers forces the end token as the last one.
            if out.len() == MAX_LENGTH - 1 {
                out.push(EOS);
                break;
            }
            let logits = self.next_logits(&out, &enc);
            let next = logits
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(EOS, |(i, _)| i);
            out.push(next);
            if next == EOS {
                break;
            }
        }
        out.iter()
            .filter(|&&t| t > UNKNOWN)
            .filter_map(|&t| self.phonemes.get(t))
            .collect()
    }
}
