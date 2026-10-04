//! Reading a crop: the [`Transcriber`] trait and the remote backends, and the
//! backend entries of the config file ([`crate::config`]).
//!
//! The rules come from halo-workbench's hint tool, which this replaces: several
//! readings are shown as several readings (grouped by identical text, counted, never
//! merged or ranked across backends), an empty answer is reported rather than hidden,
//! and the prompts are the ones every measurement behind the model choice was taken
//! with -- change them and the comparison is void.

use crate::config::UiConfig;
use crate::model::ModelPhase;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Verbatim from halo-workbench `app/handwriting.py`.
pub const CROP_PROMPT: &str =
    "This is a crop from a handwritten student's answer. Write out exactly what is \
     written in it, character for character. Do not correct spelling, do not complete \
     an abbreviation, and do not explain. If part of it is struck out, show that. \
     If you cannot read it, say UNREADABLE.";

/// For a block the layout model called a formula.
pub const FORMULA_PROMPT: &str =
    "This is a crop of a handwritten mathematical expression from a student's answer. \
     Write it out exactly as LaTeX between $ signs, symbol for symbol. Do not simplify, \
     correct or complete it, and do not explain. If part of it is struck out, leave it \
     out. If you cannot read it, say UNREADABLE.";

/// Verbatim from halo-workbench `app/handwriting.py`.
pub const PAGE_PROMPT: &str =
    "Transcribe the handwritten text in the attached image of a student's answer \
     to a test question. Provide only the word for word transcription and do not \
     correct or infer meaning.";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One word or line the operator boxed: read it character for character.
    Crop,
    /// A block the layout model called a formula: read it as maths.
    Formula,
    /// A whole page.
    Page,
}

impl Mode {
    /// The prompt as shipped (the one every measurement was taken with).
    pub fn default_prompt(self) -> &'static str {
        match self {
            Mode::Crop => CROP_PROMPT,
            Mode::Formula => FORMULA_PROMPT,
            Mode::Page => PAGE_PROMPT,
        }
    }

    /// The hint API knows crops and pages; a formula is a crop to it.
    fn api_name(self) -> &'static str {
        match self {
            Mode::Crop | Mode::Formula => "crop",
            Mode::Page => "page",
        }
    }
}

/// One alternate the model considered instead of the token it went with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenAlt {
    pub text: String,
    pub prob: f32,
}

/// One output token with its probability and runner-up alternates, in the
/// workbench's sense (`app/handwriting.py::token_confidences()`): >= 0.92 is
/// "steady", >= 0.6 "wavering", below that "hesitant". Never merged or voted --
/// each backend that can report tokens reports its own.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Token {
    pub text: String,
    pub prob: f32,
    /// Up to 3 runner-up tokens the model gave lower probability, most likely first.
    pub alternates: Vec<TokenAlt>,
}

/// One distinct answer and how many of the samples gave it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub text: String,
    pub count: u32,
    /// The generation hit its token limit: the text may be cut short.
    pub truncated: bool,
    /// Per-token probabilities, when the backend can report them (today: the
    /// built-in GLM-OCR). `None` when the backend does not support it.
    pub tokens: Option<Vec<Token>>,
}

impl Reading {
    /// `tokens`, but only when they reconstruct the shown text -- a backend's
    /// per-token decode can drift from its whole-text decode (BPE merges split
    /// across a token boundary), and a wrong overlay is worse than none.
    pub fn tokens_if_valid(&self) -> Option<&[Token]> {
        let tokens = self.tokens.as_deref()?;
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        (joined.split_whitespace().collect::<Vec<_>>().join(" ")
            == self.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .then_some(tokens)
    }
}

/// The workbench's three-way confidence bucket for one token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Steady,
    Wavering,
    Hesitant,
}

impl Token {
    pub fn confidence(&self, cfg: &UiConfig) -> Confidence {
        if self.prob >= cfg.steady_threshold {
            Confidence::Steady
        } else if self.prob >= cfg.wavering_threshold {
            Confidence::Wavering
        } else {
            Confidence::Hesitant
        }
    }
}

/// What one backend said about one image.
#[derive(Debug, Clone, Serialize)]
pub struct Transcription {
    pub backend: String,
    pub readings: Vec<Reading>,
    /// Samples that came back with no text at all -- a reported result, not an error.
    pub silent: u32,
    pub samples: u32,
    pub elapsed: Duration,
}

/// Something that reads an image. `png` is the encoded crop; implementations run on
/// a worker thread and may block.
pub trait Transcriber: Send + Sync {
    fn name(&self) -> &str;
    /// A line for the window about the backend's own state (downloading, loading,
    /// unavailable); `None` when there is nothing to say.
    fn status(&self) -> Option<String> {
        None
    }
    /// Where a built-in model is in getting ready; `None` for a backend with nothing
    /// to prepare (a remote one).
    fn phase(&self) -> Option<ModelPhase> {
        None
    }
    /// Gets ready -- finds, downloads if need be, and loads -- unless it is already
    /// ready or on its way. Returns at once; [`phase`](Self::phase) follows it.
    fn prepare(&self) {}
    /// Abandons a download in progress (the model goes back to not installed).
    fn cancel_prepare(&self) {}
    /// `prompt` is the instruction for `mode` (the user's, or the default); a
    /// backend that sets its own prompt (the hint API) ignores it. `capture_px` is
    /// the size of the whole frame the crop was cut from, for backends that warn
    /// about low-resolution captures.
    fn read(
        &self,
        png: &[u8],
        mode: Mode,
        prompt: &str,
        capture_px: (u32, u32),
    ) -> Result<Transcription>;
}

// ---------------------------------------------------------------------------
// Configuration

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BackendConfig {
    /// Any OpenAI-compatible chat endpoint with image input: llama-server, Ollama,
    /// vLLM, OpenAI itself. `base_url` is everything before `/chat/completions`.
    OpenAi {
        name: String,
        base_url: String,
        model: String,
        #[serde(default)]
        api_key: Option<String>,
        /// How many times to ask; more than one samples at `temperature` to show
        /// how sure the model is.
        #[serde(default = "one")]
        samples: u32,
        #[serde(default = "default_temperature")]
        temperature: f32,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
    /// halo-workbench's `/hint/read`: it picks the model (`member`), starts it on
    /// demand, and does the sampling itself.
    HintApi {
        name: String,
        /// The workbench root, e.g. `http://127.0.0.1:8093`.
        base_url: String,
        member: String,
        #[serde(default = "default_hint_samples")]
        samples: u32,
    },
    /// The bundled GLM-OCR on ONNX Runtime (needs the `local-model` build feature).
    /// Greedy decoding: one reading per request.
    Local {
        name: String,
        #[serde(default)]
        device: LocalDevice,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
        /// Ceiling on image tokens (the image is downscaled to fit). A whole page
        /// at the model's default budget lost the GPU; 2048 is the workbench's
        /// measured setting.
        #[serde(default = "default_max_image_tokens")]
        max_image_tokens: u32,
    },
}

/// Which device the built-in models try. Part of the config whether or not the
/// front end was built with them, so one config file serves every build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalDevice {
    /// WebGPU, falling back to CPU if the provider cannot be set up.
    #[default]
    Auto,
    Webgpu,
    Cpu,
}

fn one() -> u32 {
    1
}
fn default_temperature() -> f32 {
    0.7
}
fn default_max_tokens() -> u32 {
    1024
}
fn default_hint_samples() -> u32 {
    3
}
fn default_max_image_tokens() -> u32 {
    2048
}

impl BackendConfig {
    /// Builds the backends that are plain HTTP clients. `None` for
    /// [`BackendConfig::Local`]: the built-in model lives outside this crate, so a
    /// front end builds that one itself (and says why when it was built without it).
    pub fn build(&self) -> Option<Box<dyn Transcriber>> {
        Some(match self.clone() {
            BackendConfig::OpenAi {
                name,
                base_url,
                model,
                api_key,
                samples,
                temperature,
                max_tokens,
            } => Box::new(OpenAiBackend {
                name,
                base_url: base_url.trim_end_matches('/').to_string(),
                model,
                api_key,
                samples: samples.max(1),
                temperature,
                max_tokens,
                use_max_completion_tokens: AtomicBool::new(false),
            }),
            BackendConfig::HintApi {
                name,
                base_url,
                member,
                samples,
            } => Box::new(HintApiBackend {
                name,
                base_url: base_url.trim_end_matches('/').to_string(),
                member,
                samples: samples.max(1),
            }),
            BackendConfig::Local { .. } => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible

struct OpenAiBackend {
    name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    samples: u32,
    temperature: f32,
    max_tokens: u32,
    /// Some models (OpenAI's `gpt-5` family, as of 2026) reject `max_tokens` and
    /// want `max_completion_tokens` instead; discovered from the first request's
    /// error and remembered so later requests (more samples, later reads) do not
    /// pay for a failed attempt again.
    use_max_completion_tokens: AtomicBool,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn data_url(png: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

impl Transcriber for OpenAiBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(
        &self,
        png: &[u8],
        _mode: Mode,
        prompt: &str,
        _capture_px: (u32, u32),
    ) -> Result<Transcription> {
        let started = Instant::now();
        // One sample is a greedy read; spread is only asked for when sampling.
        let temperature = if self.samples == 1 {
            0.0
        } else {
            self.temperature
        };
        let agent = agent();
        let url = format!("{}/chat/completions", self.base_url);
        let mut samples = Vec::new();
        for _ in 0..self.samples {
            let payload = self.send_one(&agent, &url, temperature, prompt, png)?;
            samples.push(parse_choice(&payload)?);
        }
        Ok(group(&self.name, samples, started.elapsed()))
    }
}

impl OpenAiBackend {
    /// One request body: `logprobs`/`top_logprobs` for hesitation tinting (a
    /// server that does not support them either ignores the fields or omits
    /// `logprobs` from its response, both handled by `parse_logprobs_tokens`
    /// returning `None`), and `max_tokens` or `max_completion_tokens` depending
    /// on `use_completion_tokens`.
    fn body(
        &self,
        temperature: f32,
        prompt: &str,
        png: &[u8],
        use_completion_tokens: bool,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "temperature": temperature,
            "logprobs": true,
            "top_logprobs": 3,
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": data_url(png)}},
                {"type": "text", "text": prompt},
            ]}],
        });
        let field = if use_completion_tokens {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[field] = serde_json::json!(self.max_tokens);
        body
    }

    /// Posts one request, retrying once with `max_completion_tokens` in place of
    /// `max_tokens` if the server rejects the latter the way OpenAI's `gpt-5`
    /// family does (as of 2026) -- remembered on `self` so later requests, in
    /// this read and later ones, do not pay for the failed attempt again.
    fn send_one(
        &self,
        agent: &ureq::Agent,
        url: &str,
        temperature: f32,
        prompt: &str,
        png: &[u8],
    ) -> Result<serde_json::Value> {
        let use_completion = self.use_max_completion_tokens.load(Ordering::Relaxed);
        let body = self.body(temperature, prompt, png, use_completion);
        let (status, payload) = self.send(agent, url, &body)?;
        if status < 300 {
            return Ok(payload);
        }
        if !use_completion && rejects_max_tokens(&payload) {
            self.use_max_completion_tokens
                .store(true, Ordering::Relaxed);
            let body = self.body(temperature, prompt, png, true);
            let (status, payload) = self.send(agent, url, &body)?;
            if status < 300 {
                return Ok(payload);
            }
            return Err(response_error(&self.base_url, status, &payload));
        }
        Err(response_error(&self.base_url, status, &payload))
    }

    fn send(
        &self,
        agent: &ureq::Agent,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value)> {
        let mut request = agent.post(url);
        if let Some(key) = &self.api_key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let mut response = request
            .send_json(body)
            .with_context(|| format!("{} did not answer", self.base_url))?;
        let status = response.status().as_u16();
        let payload: serde_json::Value = response
            .body_mut()
            .read_json()
            .with_context(|| format!("{}: unreadable response (HTTP {status})", self.base_url))?;
        Ok((status, payload))
    }
}

/// Whether an error response is OpenAI's own "use max_completion_tokens instead"
/// rejection -- a structured error (`code`/`param`), not a message-text match, so
/// it only fires on the specific, documented shape rather than guessing from
/// prose.
fn rejects_max_tokens(payload: &serde_json::Value) -> bool {
    payload.pointer("/error/param").and_then(|v| v.as_str()) == Some("max_tokens")
        && payload.pointer("/error/code").and_then(|v| v.as_str()) == Some("unsupported_parameter")
}

fn response_error(base_url: &str, status: u16, payload: &serde_json::Value) -> anyhow::Error {
    let detail = payload
        .pointer("/error/message")
        .or_else(|| payload.get("error"))
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_default();
    anyhow!("{base_url} answered HTTP {status} {detail}")
}

struct Sample {
    text: String,
    truncated: bool,
    tokens: Option<Vec<Token>>,
}

fn parse_choice(payload: &serde_json::Value) -> Result<Sample> {
    let choice = payload
        .pointer("/choices/0")
        .ok_or_else(|| anyhow!("response has no choices"))?;
    let text = choice
        .pointer("/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let truncated = choice.get("finish_reason").and_then(|f| f.as_str()) == Some("length");
    let tokens = parse_logprobs_tokens(choice);
    Ok(Sample {
        text,
        truncated,
        tokens,
    })
}

/// Bytes of one token, appended to a running buffer and decoded as far as valid
/// UTF-8 allows -- a multi-byte character can be split across adjacent tokens, so
/// decoding each token's bytes on their own can produce replacement characters
/// where a whole character has not arrived yet.
fn decode_incremental(buf: &mut Vec<u8>) -> String {
    match std::str::from_utf8(buf) {
        Ok(s) => {
            let s = s.to_string();
            buf.clear();
            s
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            let s = std::str::from_utf8(&buf[..valid_up_to])
                .expect("valid_up_to bounds valid UTF-8")
                .to_string();
            buf.drain(..valid_up_to);
            s
        }
    }
}

/// `token`'s `bytes`, or its `token` string when the server omitted `bytes`.
fn logprob_bytes(entry: &serde_json::Value) -> Vec<u8> {
    entry
        .get("bytes")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64())
                .map(|v| v as u8)
                .collect()
        })
        .unwrap_or_else(|| {
            entry
                .get("token")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .as_bytes()
                .to_vec()
        })
}

/// Per-token probabilities from an OpenAI-style `logprobs.content`
/// (`{token, logprob, bytes, top_logprobs: [{token, logprob, bytes}, ...]}` per
/// entry); `None` when the server did not return one (an older or non-conforming
/// endpoint).
fn parse_logprobs_tokens(choice: &serde_json::Value) -> Option<Vec<Token>> {
    let content = choice.pointer("/logprobs/content")?.as_array()?;
    if content.is_empty() {
        return None;
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut tokens = Vec::with_capacity(content.len());
    for entry in content {
        buf.extend(logprob_bytes(entry));
        let text = decode_incremental(&mut buf);
        let prob = entry
            .get("logprob")
            .and_then(|v| v.as_f64())
            .map(|lp| lp.exp() as f32)
            .unwrap_or(0.0);
        let alternates = entry
            .get("top_logprobs")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|alt| {
                        let alt_prob = alt.get("logprob").and_then(|v| v.as_f64())?.exp() as f32;
                        // An alternate is a candidate the model did not go with, not
                        // part of the actual byte stream, so it is decoded on its
                        // own rather than through the running buffer above.
                        Some(TokenAlt {
                            text: String::from_utf8_lossy(&logprob_bytes(alt)).into_owned(),
                            prob: alt_prob,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        tokens.push(Token {
            text,
            prob,
            alternates,
        });
    }
    Some(tokens)
}

/// Whitespace-insensitive key, as the workbench's `normalise()`: two samples that
/// differ only in spacing are one reading.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Distinct readings with their support, most-supported first, ties in first-seen
/// order; empty answers counted separately as `silent`.
fn group(backend: &str, samples: Vec<Sample>, elapsed: Duration) -> Transcription {
    let mut readings: Vec<Reading> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut silent = 0;
    let total = samples.len() as u32;
    for s in samples {
        if s.text.is_empty() {
            silent += 1;
            continue;
        }
        let key = normalise(&s.text);
        match keys.iter().position(|k| *k == key) {
            Some(i) => {
                readings[i].count += 1;
                readings[i].truncated |= s.truncated;
            }
            None => {
                keys.push(key);
                readings.push(Reading {
                    text: s.text,
                    count: 1,
                    truncated: s.truncated,
                    tokens: s.tokens,
                });
            }
        }
    }
    // Stable sort keeps first-seen order among equals.
    readings.sort_by_key(|r| std::cmp::Reverse(r.count));
    Transcription {
        backend: backend.to_string(),
        readings,
        silent,
        samples: total,
        elapsed,
    }
}

// ---------------------------------------------------------------------------
// halo-workbench /hint/read

struct HintApiBackend {
    name: String,
    base_url: String,
    member: String,
    samples: u32,
}

impl Transcriber for HintApiBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(
        &self,
        png: &[u8],
        mode: Mode,
        _prompt: &str,
        capture_px: (u32, u32),
    ) -> Result<Transcription> {
        let started = Instant::now();
        let body = serde_json::json!({
            "member": self.member,
            "mode": mode.api_name(),
            "samples": self.samples,
            "image": data_url(png),
            "capture_px": {"width": capture_px.0, "height": capture_px.1},
        });
        let url = format!("{}/hint/read", self.base_url);
        let mut response = agent()
            .post(&url)
            .send_json(&body)
            .with_context(|| format!("{} did not answer", self.base_url))?;
        let status = response.status().as_u16();
        let payload: serde_json::Value = response
            .body_mut()
            .read_json()
            .with_context(|| format!("{}: unreadable response (HTTP {status})", self.base_url))?;
        if status >= 300 {
            let detail = payload.get("error").and_then(|e| e.as_str()).unwrap_or("");
            bail!("{} answered HTTP {status}: {detail}", self.base_url);
        }
        parse_hint_response(&self.name, &payload, started.elapsed())
    }
}

fn parse_hint_response(
    backend: &str,
    payload: &serde_json::Value,
    elapsed: Duration,
) -> Result<Transcription> {
    let readings = payload
        .get("readings")
        .and_then(|r| r.as_array())
        .ok_or_else(|| anyhow!("hint response has no readings"))?
        .iter()
        .map(|r| Reading {
            text: r
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            count: r.get("count").and_then(|c| c.as_u64()).unwrap_or(1) as u32,
            truncated: r
                .get("truncated")
                .and_then(|t| t.as_bool())
                .unwrap_or(false),
            // TODO: the hint API can report per-token confidences too; not parsed yet.
            tokens: None,
        })
        .collect();
    let samples = payload
        .get("samples")
        .and_then(|s| s.as_array())
        .map(|s| s.len() as u32)
        .unwrap_or(0);
    let silent = payload
        .get("silent_count")
        .and_then(|s| s.as_u64())
        .unwrap_or(0) as u32;
    Ok(Transcription {
        backend: backend.to_string(),
        readings,
        silent,
        samples,
        elapsed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Sample {
        Sample {
            text: text.into(),
            truncated: false,
            tokens: None,
        }
    }

    #[test]
    fn grouping_counts_support_and_keeps_first_seen_order() {
        let t = group(
            "b",
            vec![s("the"), s("tho"), s("the "), s(""), s("  the")],
            Duration::ZERO,
        );
        assert_eq!(t.samples, 5);
        assert_eq!(t.silent, 1);
        assert_eq!(
            t.readings,
            vec![
                Reading {
                    text: "the".into(),
                    count: 3,
                    truncated: false,
                    tokens: None
                },
                Reading {
                    text: "tho".into(),
                    count: 1,
                    truncated: false,
                    tokens: None
                },
            ]
        );
        // Ties stay in first-seen order rather than being ranked.
        let t = group("b", vec![s("b"), s("a")], Duration::ZERO);
        assert_eq!(t.readings[0].text, "b");
    }

    #[test]
    fn recognises_the_max_tokens_rejection() {
        let v = serde_json::json!({"error": {
            "message": "Unsupported parameter: 'max_tokens' is not supported with \
                         this model. Use 'max_completion_tokens' instead.",
            "type": "invalid_request_error",
            "param": "max_tokens",
            "code": "unsupported_parameter",
        }});
        assert!(rejects_max_tokens(&v));
        // A different unsupported-parameter error must not trigger the retry.
        let other = serde_json::json!({"error": {
            "param": "temperature",
            "code": "unsupported_parameter",
        }});
        assert!(!rejects_max_tokens(&other));
        assert!(!rejects_max_tokens(&serde_json::json!({})));
    }

    #[test]
    fn parses_openai_choice_and_length_stop() {
        let v = serde_json::json!({"choices": [{"message": {"content": " x^2 "}, "finish_reason": "length"}]});
        let c = parse_choice(&v).unwrap();
        assert_eq!(c.text, "x^2");
        assert!(c.truncated);
        assert!(c.tokens.is_none());
        assert!(parse_choice(&serde_json::json!({"choices": []})).is_err());
    }

    #[test]
    fn parses_openai_logprobs_with_split_multibyte_char() {
        // "µ" (U+00B5, 2 bytes in UTF-8: 0xC2 0xB5) split across two tokens, as a
        // BPE tokenizer can do; the incremental decoder should still reconstruct it
        // rather than emit a replacement character on the first token.
        let v = serde_json::json!({"choices": [{
            "message": {"content": "µm"},
            "logprobs": {"content": [
                {"token": "\u{FFFD}", "logprob": -0.1, "bytes": [0xC2],
                 "top_logprobs": [{"token": "\u{FFFD}", "logprob": -0.1, "bytes": [0xC2]}]},
                {"token": "\u{FFFD}m", "logprob": -0.2, "bytes": [0xB5, b'm']},
            ]},
        }]});
        let c = parse_choice(&v).unwrap();
        let tokens = c.tokens.unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "");
        assert_eq!(tokens[1].text, "µm");
        assert!((tokens[0].prob - (-0.1_f64).exp() as f32).abs() < 1e-6);
    }

    #[test]
    fn parses_hint_response() {
        let v = serde_json::json!({
            "readings": [{"text": "mitochondria", "count": 2, "truncated": false}],
            "samples": [{}, {}, {}],
            "silent_count": 1,
        });
        let t = parse_hint_response("w", &v, Duration::ZERO).unwrap();
        assert_eq!(t.readings.len(), 1);
        assert_eq!(t.readings[0].count, 2);
        assert_eq!((t.samples, t.silent), (3, 1));
    }
}
