//! Spike S8: Kokoro-82M through ONNX Runtime.
//!
//! `sentences` writes the recorded readings as the app would say them (squigl's
//! `speech::utterances`, maths in words by MathCAT) to sentences.txt; Misaki (Python,
//! `phonemize.py`) turns them into phonemes.txt; `speak MODEL VOICE...` synthesises
//! each line into out/MODEL-VOICE-N.wav and prints how long each took against the
//! audio's length.

use anyhow::{bail, Context, Result};
use libmathcat::interface as mathcat;
use ndarray::{Array1, Array2};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::time::Instant;

const SAMPLE_RATE: u32 = 24_000;
/// Where the model and voices were downloaded (see the README).
const DIR: &str = env!("KOKORO_DIR");

fn sentences() -> Result<()> {
    mathcat::set_rules_dir("Rules").map_err(|e| anyhow::anyhow!("{e}"))?;
    mathcat::set_preference("SpeechStyle", "ClearSpeak").map_err(|e| anyhow::anyhow!("{e}"))?;
    let dir = "../../crates/squigl-models/testdata/handwriting";
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".cpu.txt"))
        .collect();
    names.sort();
    let mut out = String::new();
    for p in names {
        let text = std::fs::read_to_string(&p)?;
        let said = squigl_engine::speech::utterances(&text, |m| {
            mathcat::set_mathml(m).ok()?;
            mathcat::get_spoken_text().ok()
        });
        for u in said {
            out.push_str(&u.text);
            out.push('\n');
        }
    }
    std::fs::write("sentences.txt", out)?;
    Ok(())
}

/// Kokoro's phoneme vocabulary (hexgrad/Kokoro-82M config.json).
fn vocab() -> Result<HashMap<char, i64>> {
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{DIR}/config.json"))?)?;
    Ok(config["vocab"]
        .as_object()
        .context("vocab")?
        .iter()
        .map(|(k, v)| (k.chars().next().unwrap(), v.as_i64().unwrap()))
        .collect())
}

/// A voice: 510 style vectors of 256, one per input length.
fn voice(name: &str) -> Result<Vec<f32>> {
    let bytes = std::fs::read(format!("{DIR}/voices/{name}.bin"))?;
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect())
}

fn chunks(line: &str, max: usize) -> Vec<String> {
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
    vec![line.chars().take(max).collect()]
}

fn speak(model: &str, voices: &[String], webgpu: bool) -> Result<()> {
    let vocab = vocab()?;
    let env = ort::init().build().map_err(|e| anyhow::anyhow!("{e}"))?;
    let providers = if webgpu {
        vec![ort::ep::WebGPU::default().build().error_on_failure()]
    } else {
        vec![ort::ep::CPU::default().build()]
    };
    let t = Instant::now();
    let threads: usize = std::env::var("THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    let mut session = Session::builder(&env)?
        .with_intra_threads(threads)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_execution_providers(providers)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(format!("{DIR}/onnx/{model}.onnx"))?;
    eprintln!("{model}: loaded in {:.2} s", t.elapsed().as_secs_f32());
    // Kokoro takes at most 510 phonemes: a longer line is cut at MathCAT's pauses
    // ("; "), then commas, into pieces under 400.
    // Named by sentence and piece: out/MODEL-VOICE-SS-P.wav.
    let lines: Vec<(String, String)> = std::fs::read_to_string("phonemes.txt")?
        .lines()
        .enumerate()
        .flat_map(|(i, l)| {
            chunks(l, 400)
                .into_iter()
                .enumerate()
                .map(move |(p, c)| (format!("{i:02}-{p}"), c))
        })
        .collect();
    std::fs::create_dir_all("out")?;
    for v in voices {
        let style = voice(v)?;
        let (mut work, mut audio) = (0.0, 0.0);
        for (i, line) in lines.iter() {
            let mut ids: Vec<i64> = vec![0];
            let mut dropped = String::new();
            for c in line.chars() {
                match vocab.get(&c) {
                    Some(&id) => ids.push(id),
                    None => dropped.push(c),
                }
            }
            ids.push(0);
            if !dropped.is_empty() {
                eprintln!("  line {i}: not in the vocabulary: {dropped:?}");
            }
            let n = ids.len() - 2;
            if n >= 510 {
                bail!("line {i} too long");
            }
            let s = Array2::from_shape_vec((1, 256), style[n * 256..(n + 1) * 256].to_vec())?;
            let t = Instant::now();
            let outputs = session.run(ort::inputs![
                "input_ids" => Tensor::from_array(Array2::from_shape_vec((1, ids.len()), ids)?)?,
                "style" => Tensor::from_array(s)?,
                "speed" => Tensor::from_array(Array1::from_vec(vec![1.0f32]))?,
            ])?;
            let took = t.elapsed().as_secs_f32();
            let wave = outputs[0].try_extract_array::<f32>()?;
            let secs = wave.len() as f32 / SAMPLE_RATE as f32;
            work += took;
            audio += secs;
            let mut w = hound::WavWriter::create(
                format!("out/{model}-{v}-{i}.wav"),
                hound::WavSpec {
                    channels: 1,
                    sample_rate: SAMPLE_RATE,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )?;
            for &x in wave.iter() {
                w.write_sample((x.clamp(-1.0, 1.0) * 32767.0) as i16)?;
            }
            w.finalize()?;
            eprintln!("  {v} {i}: {took:.2} s for {secs:.2} s of speech");
        }
        println!(
            "{model} {v}{}: {work:.1} s for {audio:.1} s of speech (real-time factor {:.2})",
            if webgpu { " webgpu" } else { "" },
            work / audio
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sentences") => sentences(),
        Some("devices") => {
            // What playback would go to (no sound).
            use cpal::traits::{DeviceTrait, HostTrait};
            let host = cpal::default_host();
            let device = host.default_output_device().context("no output device")?;
            let config = device.default_output_config()?;
            println!(
                "{:?}: {:?} at {} Hz, {} channels",
                host.id(),
                device.name()?,
                config.sample_rate().0,
                config.channels()
            );
            Ok(())
        }
        Some("speak") => {
            let webgpu = args.iter().any(|a| a == "--webgpu");
            let rest: Vec<String> = args[2..]
                .iter()
                .filter(|a| *a != "--webgpu")
                .cloned()
                .collect();
            speak(&args[1], &rest, webgpu)
        }
        _ => bail!("sentences | speak MODEL VOICE... [--webgpu]"),
    }
}
