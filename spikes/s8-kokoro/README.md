# Spike S8: Kokoro as the reading-aloud voice

The system voice that Phase 8d reads aloud with (speech-dispatcher, which on Linux is
espeak-ng) phrases maths well through MathCAT, but it is hard to listen to. This
spike asks whether Kokoro-82M, an Apache-2.0 open-weights text-to-speech model, can
replace it. It covers:

- how well each voice is understood;
- how fast Kokoro runs, and which export to use;
- how text becomes Kokoro's phonemes without GPL code;
- whether it can run beside GLM-OCR in one process;
- playback.

## Running it

```
# Kokoro (onnx-community/Kokoro-82M-v1.0-ONNX @ 1939ad2a8e) into $KOKORO_DIR:
#   onnx/model*.onnx, voices/*.bin, and config.json from hexgrad/Kokoro-82M (the vocabulary)
export KOKORO_DIR=/path/to/kokoro
cargo run --release -- sentences        # the recorded readings as squigl would say them -> sentences.txt
python phonemize.py                     # Misaki (git, with its neural fallback) -> phonemes.txt
cargo run --release -- speak model af_heart am_michael    # -> out/*.wav, and timings
THREADS=4 taskset -c 0-3 cargo run --release -- speak model_fp16 af_heart
python score.py                         # Whisper base.en word error rates, Kokoro against espeak-ng
python export_g2p.py                    # Misaki's fallback as ONNX, checked against PyTorch
cargo run --release -- devices          # where playback would go (no sound)
```

`sentences.txt` is every sentence of `crates/squigl-models/testdata/handwriting` as
`speech::utterances` splits it, with the maths in MathCAT's ClearSpeak words. There
are 39 sentences. The Python side ran in a venv with `misaki` installed from git;
the release on PyPI (0.9.4) has no neural fallback.

## Findings (2026-10-07, on the 20-thread Ubuntu box)

**Understood far better.** Whisper base.en stands in for a listener. Word error
rates against the text each voice was given:

| voice | text | maths sentences |
|---|---|---|
| Kokoro fp32, `af_heart` | 15.5% | 16.3% |
| Kokoro fp16 | 15.5% | 22.0% |
| Kokoro 8-bit (`model_quantized`) | 14.0% | 16.6% |
| espeak-ng (what speech-dispatcher uses) | **38.9%** | **25.4%** |

Some of Kokoro's errors are Whisper's own. For example, "homogamous" came back as
"homogenous", and the reading itself was wrong there. The difference from espeak-ng
is the point.

**Fast enough on the CPU.** Real-time factor (time to make it ÷ length of the
speech), over all 39 sentences:

| export | size | 20 threads | 4 threads |
|---|---|---|---|
| `model` (fp32) | 326 MB | 0.23 | 0.39 |
| `model_fp16` | 163 MB | 0.23 | 0.38 |
| `model_quantized` (8-bit) | 92 MB | 0.65 | 0.78 |
| `model_q8f16` | 86 MB | **segfaults** in ONNX Runtime 1.30's CPU kernels | |

- A short sentence takes 0.35–0.8 s to make on 20 threads.
- WebGPU is no faster here: 0.23.
- Making the next sentence while one plays hides the delay after the first.

**Which export:** fp16 is as fast as fp32 at half the size, but understood worse on
maths. 8-bit is smallest that works, but only just faster than real time on four
threads. A person should listen before choosing (`out/`).

**The voice files** are 510 × 256 float32 (510 KB each). The style is the row for
the input's phoneme count. The input is at most 510 phonemes, so a long maths
sentence is cut at MathCAT's pauses (`; `, then `, `); the derivative formula was
one such sentence.

**Kokoro needs Misaki's phonemes; misaki-rs is out.**

- misaki-rs is MIT, but it replaced 64,566 of the 90,201 entries in Misaki's US gold
  dictionary with pronunciations it says it made with eSpeak. Its silver dictionary
  grew from 93k to 300k entries.
- They are in a different phoneme style ("aardvark": Misaki `ˈɑɹdvˌɑɹk`, misaki-rs
  `ˈɑː‍ɹdvɑː‍ɹk`). Kokoro was trained on Misaki's own.
- Their link to espeak-ng's GPL-3 data is unclear.
- Its own README defers the data's licence to Misaki's.

**Misaki itself can be ported.** It is Apache-2.0, data included.

- **The English logic** is one 738-line file, `en.py`. Its US dictionaries are 3 MB
  of JSON each, about 1.4 MB together gzipped.
- **Part-of-speech tagging** is spaCy, which a port cannot bring. It was measured on
  5,435 words of plain English (two licence texts):
  - With no tags, 6.1% of words come out differently. Nearly all are "a" (`ˈA`, the
    letter's name, not `ɐ`), "in", "to", "the" and "that" losing their weak forms.
  - A list of function words with their usual tag, everything else a noun, brings
    that to **1.0%**.
  - What is left is noun/verb pairs ("use", "produce", "permit"), which word-order
    rules can mostly cover.
  - So the port needs no spaCy: a word list and a few rules.
- **Words in no dictionary** go to Misaki's own neural fallback,
  `PeterReid/graphemes_to_phonemes_en_us`, not espeak.
  - It is a one-layer BART: 751k parameters, Apache-2.0.
  - `export_g2p.py` exports it as two ONNX graphs, encoder 1.4 MB and decoder
    1.7 MB. Greedy decoding through ONNX Runtime matches PyTorch on all 12 test
    words, at about 10 ms a word.
  - It does well on misreadings: "Imfume" → `ɪmfjˈum`, "Ardocummed" → `ˈɑɹdəkəmd`,
    "childi" → `ʧˈIldi`, "sklearn" → `sklˈɪɹn`.
- **One Misaki quirk to fix:** `=` comes out as "x" (`ˈɛks`), and "IG=" is garbled.
  squigl should say symbols in words before the G2P.

**Beside GLM-OCR.** Kokoro ran on the CPU outside `RUNTIME`, in a loop, while
GLM-OCR read every handwriting sample three times on WebGPU under it, in one process
with one ONNX Runtime environment. There were 51 reads and 72 Kokoro runs in 163 s,
and no crash. The WebGPU concurrency bug (onnxruntime#32561) is between WebGPU
sessions, so a CPU-only Kokoro need not wait for a read. That matters for reading a
page aloud, where speaking and reading overlap. Kokoro stayed faster than real time
throughout.

**Playback.** cpal 0.16 builds (ALSA headers on Linux, `libasound2-dev`) and finds
the default output, 44.1 kHz stereo here. Kokoro's 24 kHz mono needs resampling. With
its own playback, squigl can pause mid-word, which the system voices cannot do
everywhere.

## What it would take

A `squigl-models` voice, "Kokoro", behind the engine's `speech::Voice`:

- **Downloaded on consent** like the other models (pinned revision, SHA-256): the
  model export plus a few voices, plus the G2P fallback (3 MB) and Misaki's US
  dictionaries (gzipped, about 1.4 MB).
- **Misaki's `en.py` ported to Rust:**
  - dictionaries;
  - the suffix, number and stress rules (num2words → the `num2words` crate);
  - a function-word tagger;
  - the ONNX fallback.
- **Synthesis on the CPU** a sentence ahead, and playback through cpal with
  resampling.
- **The system voice stays** as the fallback, and for people who prefer it.

**Still for a person:**

- listen to `out/` and choose the export and a default voice;
- how it sounds and how fast it is on Windows and macOS.
