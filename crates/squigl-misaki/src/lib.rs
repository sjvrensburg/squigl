//! English text to Kokoro's phonemes: a Rust port of Misaki's English G2P
//! (<https://github.com/hexgrad/misaki>, `misaki/en.py`, Apache-2.0, at fba1236),
//! the phonemiser Kokoro was trained with -- American English only.
//!
//! What differs from Misaki: spaCy's tokenizer and tagger are replaced by a
//! tokenizer and a part-of-speech guesser of our own ([`tag`]) -- function words
//! from a list, the rest from the words around them -- which agree with spaCy on
//! nearly every word that changes how a word is said (spike S8: 1% of words differ
//! against 6% with no tags at all). Words in neither dictionary go to Misaki's own
//! neural fallback ([`Fallback`], a small BART run here in plain Rust), not
//! espeak. Misaki's link syntax for hand-set phonemes is not supported.

pub mod fallback;
mod lexicon;
mod numbers;
pub mod tag;

pub use fallback::Fallback;
pub use lexicon::Lexicon;

use lexicon::{apply_stress, currency_units, is_vowel, stress_weight, Ctx, PRIMARY};
use std::sync::OnceLock;

/// Punctuation Kokoro says as pauses.
const PUNCTS: &str = ";:,.!?—…\"“”";
const NON_QUOTE_PUNCTS: &str = ";:,.!?—…";
const SUBTOKEN_JUNKS: &str = "',-._‘’/";
const CONSONANTS: &str = "bdfhjklmnpstvwzðŋɡɹɾʃʒʤʧθ";

/// One token as Misaki carries it.
#[derive(Debug, Clone, Default)]
struct Token {
    text: String,
    tag: String,
    whitespace: String,
    phonemes: Option<String>,
    stress: Option<f32>,
    currency: Option<char>,
    prespace: bool,
    is_head: bool,
    alias: Option<String>,
}

/// A word: one token, or the pieces of a word written without spaces between
/// them ("x_1", "3)").
enum Word {
    One(Token),
    Many(Vec<Token>),
}

/// `merge_tokens`: pieces as one token (phonemes joined when `unk` is given).
fn merge(tokens: &[Token], unk: Option<&str>) -> Token {
    let last = tokens.last().expect("not empty");
    let mut text = String::new();
    for t in &tokens[..tokens.len() - 1] {
        text.push_str(&t.text);
        text.push_str(&t.whitespace);
    }
    text.push_str(&last.text);
    let weight = |t: &Token| -> usize {
        t.text
            .chars()
            .map(|c| {
                if lexicon::is_lower(&c.to_string()) {
                    1
                } else {
                    2
                }
            })
            .sum()
    };
    // Python's max: the first of the heaviest.
    let mut tag_from = &tokens[0];
    for t in tokens {
        if weight(t) > weight(tag_from) {
            tag_from = t;
        }
    }
    let stresses: Vec<f32> = tokens.iter().filter_map(|t| t.stress).collect();
    let stress = match stresses.split_first() {
        Some((first, rest)) if rest.iter().all(|s| s == first) => Some(*first),
        _ => None,
    };
    let phonemes = unk.map(|unk| {
        let mut ps = String::new();
        for t in tokens {
            if t.prespace
                && !ps.is_empty()
                && !ps.ends_with(char::is_whitespace)
                && t.phonemes.as_deref().is_some_and(|p| !p.is_empty())
            {
                ps.push(' ');
            }
            ps.push_str(t.phonemes.as_deref().unwrap_or(unk));
        }
        ps
    });
    Token {
        text,
        tag: tag_from.tag.clone(),
        whitespace: last.whitespace.clone(),
        phonemes,
        stress,
        currency: tokens.iter().filter_map(|t| t.currency).max(),
        prespace: tokens[0].prespace,
        is_head: tokens[0].is_head,
        alias: None,
    }
}

/// Misaki's `subtokenize`: a word cut at case changes, digits, hyphens and
/// apostrophes.
fn subtokenize(word: &str) -> Vec<String> {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        fancy_regex::Regex::new(
            r"^['‘’]+|\p{Lu}(?=\p{Lu}\p{Ll})|(?:^-)?(?:\d?[,.]?\d)+|[-_]+|['‘’]{2,}|\p{L}*?(?:['‘’]\p{L})*?\p{Ll}(?=\p{Lu})|\p{L}+(?:['‘’]\p{L})*|[^-_\p{L}'‘’\d]|['‘’]+$",
        )
        .expect("Misaki's pattern compiles")
    });
    re.find_iter(word)
        .filter_map(|m| m.ok().map(|m| m.as_str().to_string()))
        .collect()
}

fn punct_tag_phonemes(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "-LRB-" => "(",
        "-RRB-" => ")",
        "``" => "\u{201C}",
        "\"\"" | "''" => "\u{201D}",
        _ => return None,
    })
}

const PUNCT_TAGS: [&str; 11] = [
    ".", ",", "-LRB-", "-RRB-", "``", "\"\"", "''", ":", "$", "#", "NFP",
];

/// `retokenize`: tokens into words, punctuation and currency settled.
fn retokenize(tokens: Vec<Token>) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut currency: Option<char> = None;
    let n = tokens.len();
    for (i, token) in tokens.iter().enumerate() {
        let mut tks: Vec<Token> = if token.alias.is_none() && token.phonemes.is_none() {
            subtokenize(&token.text)
                .into_iter()
                .map(|t| Token {
                    text: t,
                    tag: token.tag.clone(),
                    whitespace: String::new(),
                    stress: token.stress,
                    is_head: true,
                    ..Token::default()
                })
                .collect()
        } else {
            vec![token.clone()]
        };
        if tks.is_empty() {
            tks.push(token.clone());
        }
        tks.last_mut().expect("not empty").whitespace = token.whitespace.clone();
        let m = tks.len();
        for j in 0..m {
            let (before, after) = (
                (j > 0).then(|| tks[j - 1].text.clone()),
                (j + 1 < m).then(|| tks[j + 1].text.clone()),
            );
            let tk = &mut tks[j];
            if tk.alias.is_some() || tk.phonemes.is_some() {
            } else if tk.tag == "$"
                && tk.text.chars().count() == 1
                && currency_units(tk.text.chars().next().unwrap()).is_some()
            {
                currency = tk.text.chars().next();
                tk.phonemes = Some(String::new());
            } else if tk.tag == ":" && (tk.text == "-" || tk.text == "–") {
                tk.phonemes = Some("—".into());
            } else if PUNCT_TAGS.contains(&tk.tag.as_str())
                && !tk
                    .text
                    .chars()
                    .all(|c| c.to_ascii_lowercase().is_ascii_lowercase())
            {
                tk.phonemes = Some(match punct_tag_phonemes(&tk.tag) {
                    Some(p) => p.into(),
                    None => tk.text.chars().filter(|c| PUNCTS.contains(*c)).collect(),
                });
            } else if currency.is_some() {
                if tk.tag != "CD" {
                    currency = None;
                } else if j + 1 == m && (i + 1 == n || tokens[i + 1].tag != "CD") {
                    tk.currency = currency;
                }
            } else if j > 0
                && j + 1 < m
                && tk.text == "2"
                && before
                    .as_deref()
                    .and_then(|b| b.chars().last())
                    .zip(after.as_deref().and_then(|a| a.chars().next()))
                    .is_some_and(|(a, b)| a.is_alphabetic() && b.is_alphabetic())
            {
                tk.alias = Some("to".into());
            }
        }
        for tk in tks {
            if tk.alias.is_some() || tk.phonemes.is_some() {
                words.push(Word::One(tk));
            } else if let Some(Word::Many(last)) = words.last_mut().filter(
                |w| matches!(w, Word::Many(l) if l.last().is_some_and(|t| t.whitespace.is_empty())),
            ) {
                let mut tk = tk;
                tk.is_head = false;
                last.push(tk);
            } else if !tk.whitespace.is_empty() {
                words.push(Word::One(tk));
            } else {
                words.push(Word::Many(vec![tk]));
            }
        }
    }
    words
        .into_iter()
        .map(|w| match w {
            Word::Many(mut v) if v.len() == 1 => Word::One(v.remove(0)),
            w => w,
        })
        .collect()
}

/// `token_context`: what the word just phonemised tells the one before it.
fn token_context(ctx: Ctx, ps: Option<&str>, token: &Token) -> Ctx {
    let mut vowel = ctx.future_vowel;
    if let Some(ps) = ps.filter(|p| !p.is_empty()) {
        if let Some(c) = ps
            .chars()
            .find(|&c| is_vowel(c) || CONSONANTS.contains(c) || NON_QUOTE_PUNCTS.contains(c))
        {
            vowel = if NON_QUOTE_PUNCTS.contains(c) {
                None
            } else {
                Some(is_vowel(c))
            };
        }
    }
    let future_to = matches!(token.text.as_str(), "to" | "To")
        || (token.text == "TO" && matches!(token.tag.as_str(), "TO" | "IN"));
    Ctx {
        future_vowel: vowel,
        future_to,
    }
}

/// `resolve_tokens`: a word's pieces spaced and their stress balanced.
fn resolve_tokens(tokens: &mut [Token]) {
    let mut text = String::new();
    for t in &tokens[..tokens.len() - 1] {
        text.push_str(&t.text);
        text.push_str(&t.whitespace);
    }
    text.push_str(&tokens[tokens.len() - 1].text);
    let kinds: std::collections::HashSet<u8> = text
        .chars()
        .filter(|c| !SUBTOKEN_JUNKS.contains(*c))
        .map(|c| {
            if c.is_alphabetic() {
                0
            } else if c.is_ascii_digit() {
                1
            } else {
                2
            }
        })
        .collect();
    let prespace = text.contains(' ') || text.contains('/') || kinds.len() > 1;
    let n = tokens.len();
    for (i, tk) in tokens.iter_mut().enumerate() {
        if tk.phonemes.is_none() {
            if i == n - 1
                && tk.text.chars().count() == 1
                && NON_QUOTE_PUNCTS.contains(tk.text.as_str())
            {
                tk.phonemes = Some(tk.text.clone());
            } else if tk.text.chars().all(|c| SUBTOKEN_JUNKS.contains(c)) {
                tk.phonemes = Some(String::new());
            }
        } else if i > 0 {
            tk.prespace = prespace;
        }
    }
    if prespace {
        return;
    }
    let mut indices: Vec<(bool, usize, usize)> = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let p = t.phonemes.as_deref().filter(|p| !p.is_empty())?;
            Some((p.contains(PRIMARY), stress_weight(p), i))
        })
        .collect();
    if indices.len() == 2 && tokens[indices[0].2].text.chars().count() == 1 {
        let i = indices[1].2;
        let p = tokens[i].phonemes.take().unwrap_or_default();
        tokens[i].phonemes = Some(apply_stress(&p, Some(-0.5)));
        return;
    }
    let primaries = indices.iter().filter(|x| x.0).count();
    if indices.len() < 2 || primaries <= indices.len().div_ceil(2) {
        return;
    }
    let half = indices.len() / 2;
    indices.sort();
    for &(_, _, i) in &indices[..half] {
        let p = tokens[i].phonemes.take().unwrap_or_default();
        tokens[i].phonemes = Some(apply_stress(&p, Some(-0.5)));
    }
}

/// The phonemiser: the dictionaries, and the fallback for words in neither.
pub struct G2p {
    lexicon: Lexicon,
    fallback: Option<Fallback>,
}

impl G2p {
    pub fn new(lexicon: Lexicon, fallback: Option<Fallback>) -> Self {
        Self { lexicon, fallback }
    }

    fn lexicon_token(&self, tk: &Token, ctx: Ctx) -> Option<String> {
        let text = tk.alias.as_deref().unwrap_or(&tk.text);
        self.lexicon
            .token(text, &tk.tag, tk.stress, tk.currency, tk.is_head, ctx)
            .0
    }

    fn fallback(&self, text: &str) -> Option<String> {
        self.fallback.as_ref().map(|f| f.phonemes(text))
    }

    /// `text` in Kokoro's phonemes: words separated as written, punctuation kept
    /// where Kokoro pauses for it. A word nothing could read is left out.
    pub fn phonemes(&self, text: &str) -> String {
        let tokens = tag::tag(&tag::tokenize(text.trim_start()));
        let tokens: Vec<Token> = tokens
            .into_iter()
            .map(|(text, tag, whitespace)| Token {
                text,
                tag,
                whitespace,
                is_head: true,
                ..Token::default()
            })
            .collect();
        let mut words = retokenize(tokens);
        let mut ctx = Ctx::default();
        for w in words.iter_mut().rev() {
            match w {
                Word::One(tk) => {
                    if tk.phonemes.is_none() {
                        tk.phonemes = self.lexicon_token(tk, ctx);
                    }
                    if tk.phonemes.is_none() {
                        tk.phonemes = self.fallback(&tk.text);
                    }
                    ctx = token_context(ctx, tk.phonemes.as_deref(), tk);
                }
                Word::Many(w) => {
                    let (mut left, mut right) = (0, w.len());
                    let mut should_fallback = false;
                    while left < right {
                        let settled = w[left..right]
                            .iter()
                            .any(|t| t.alias.is_some() || t.phonemes.is_some());
                        let merged = (!settled).then(|| merge(&w[left..right], None));
                        let ps = merged.as_ref().and_then(|m| self.lexicon_token(m, ctx));
                        if let (Some(ps), Some(m)) = (ps, &merged) {
                            w[left].phonemes = Some(ps.clone());
                            for x in &mut w[left + 1..right] {
                                x.phonemes = Some(String::new());
                            }
                            ctx = token_context(ctx, Some(&ps), m);
                            right = left;
                            left = 0;
                        } else if left + 1 < right {
                            left += 1;
                        } else {
                            right -= 1;
                            let tk = &mut w[right];
                            if tk.phonemes.is_none() {
                                if tk.text.chars().all(|c| SUBTOKEN_JUNKS.contains(c)) {
                                    tk.phonemes = Some(String::new());
                                } else if self.fallback.is_some() {
                                    should_fallback = true;
                                    break;
                                }
                            }
                            left = 0;
                        }
                    }
                    if should_fallback {
                        let merged = merge(w, None);
                        w[0].phonemes = self.fallback(&merged.text);
                        for x in &mut w[1..] {
                            x.phonemes = Some(String::new());
                        }
                    } else {
                        resolve_tokens(w);
                    }
                }
            }
        }
        let mut out = String::new();
        for w in words {
            let tk = match w {
                Word::One(t) => t,
                Word::Many(v) => merge(&v, Some("")),
            };
            if let Some(p) = &tk.phonemes {
                out.push_str(&p.replace('ɾ', "T").replace('ʔ', "t"));
            }
            out.push_str(&tk.whitespace);
        }
        out
    }
}
