//! Misaki's `Lexicon` (misaki/en.py, Apache-2.0), ported: the gold and silver
//! dictionaries, stress, the special cases, -s/-ed/-ing on known stems, and
//! numbers. Kept close to the original, function for function, so it can be
//! checked against it.

use crate::numbers;
use std::collections::HashMap;

pub(crate) const PRIMARY: char = 'ˈ';
pub(crate) const SECONDARY: char = 'ˌ';
const VOWELS: &str = "AIOQWYaiuæɑɒɔəɛɜɪʊʌᵻ";
pub(crate) const DIPHTHONGS: &str = "AIOQWYʤʧ";
const US_TAUS: &str = "AIOWYiuæɑəɛɪɹʊʌ";
const ORDINALS: [&str; 4] = ["st", "nd", "rd", "th"];

pub(crate) fn is_vowel(c: char) -> bool {
    VOWELS.contains(c)
}

/// One dictionary entry: phonemes, or phonemes by part of speech (a value of
/// `None` means "not this way": fall back to spelling).
#[derive(Debug, Clone)]
pub enum Entry {
    Plain(String),
    Tagged(HashMap<String, Option<String>>),
}

fn parse(json: &str) -> anyhow::Result<HashMap<String, Entry>> {
    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(json)?;
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let entry = match v {
            serde_json::Value::String(s) => Entry::Plain(s),
            serde_json::Value::Object(m) => Entry::Tagged(
                m.into_iter()
                    .map(|(t, p)| (t, p.as_str().map(String::from)))
                    .collect(),
            ),
            _ => continue,
        };
        out.insert(k, entry);
    }
    Ok(grow(out))
}

/// Python's `str.capitalize`.
pub(crate) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f
            .to_uppercase()
            .chain(c.as_str().to_lowercase().chars())
            .collect(),
        None => String::new(),
    }
}

pub(crate) fn is_lower(s: &str) -> bool {
    s == s.to_lowercase()
}

pub(crate) fn is_upper(s: &str) -> bool {
    s == s.to_uppercase()
}

/// Python's `str.isalpha`.
pub(crate) fn is_alpha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(char::is_alphabetic)
}

pub(crate) fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `grow_dictionary`: every lowercase word also capitalised, every capitalised
/// word also lowercase, the original entries winning.
fn grow(d: HashMap<String, Entry>) -> HashMap<String, Entry> {
    let mut e = HashMap::new();
    for (k, v) in &d {
        if k.chars().count() < 2 {
            continue;
        }
        if is_lower(k) {
            let cap = capitalize(k);
            if *k != cap {
                e.insert(cap, v.clone());
            }
        } else if *k == capitalize(&k.to_lowercase()) {
            e.insert(k.to_lowercase(), v.clone());
        }
    }
    e.extend(d);
    e
}

/// What follows a word: whether the next sound is a vowel (`None`: punctuation
/// or the end), and whether the next word is "to".
#[derive(Debug, Clone, Copy, Default)]
pub struct Ctx {
    pub future_vowel: Option<bool>,
    pub future_to: bool,
}

/// `apply_stress`, on `ps` with the stress asked for (Misaki's numbers: -2
/// removes stress, -1 and -0.5 demote, 0.5 to 2 promote).
pub(crate) fn apply_stress(ps: &str, stress: Option<f32>) -> String {
    let Some(stress) = stress else {
        return ps.to_string();
    };
    let has = |c: char| ps.contains(c);
    let any_stress = has(PRIMARY) || has(SECONDARY);
    let any_vowel = ps.chars().any(is_vowel);
    if stress < -1.0 {
        ps.replace([PRIMARY, SECONDARY], "")
    } else if stress == -1.0 || ((stress == 0.0 || stress == -0.5) && has(PRIMARY)) {
        ps.replace(SECONDARY, "")
            .replace(PRIMARY, &SECONDARY.to_string())
    } else if (stress == 0.0 || stress == 0.5 || stress == 1.0) && !any_stress {
        if !any_vowel {
            return ps.to_string();
        }
        restress(&format!("{SECONDARY}{ps}"))
    } else if stress >= 1.0 && !has(PRIMARY) && has(SECONDARY) {
        ps.replace(SECONDARY, &PRIMARY.to_string())
    } else if stress > 1.0 && !any_stress {
        if !any_vowel {
            return ps.to_string();
        }
        restress(&format!("{PRIMARY}{ps}"))
    } else {
        ps.to_string()
    }
}

/// Moves each stress mark to just before the next vowel.
fn restress(ps: &str) -> String {
    let chars: Vec<char> = ps.chars().collect();
    let mut keyed: Vec<(f64, char)> = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| (i as f64, c))
        .collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == PRIMARY || c == SECONDARY {
            if let Some(j) = (i..chars.len()).find(|&j| is_vowel(chars[j])) {
                keyed[i].0 = j as f64 - 0.5;
            }
        }
    }
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    keyed.into_iter().map(|(_, c)| c).collect()
}

pub(crate) fn stress_weight(ps: &str) -> usize {
    ps.chars()
        .map(|c| if DIPHTHONGS.contains(c) { 2 } else { 1 })
        .sum()
}

fn parent_tag(tag: Option<&str>) -> Option<String> {
    let tag = tag?;
    Some(
        if tag.starts_with("VB") {
            "VERB"
        } else if tag.starts_with("NN") {
            "NOUN"
        } else if tag.starts_with("ADV") || tag.starts_with("RB") {
            "ADV"
        } else if tag.starts_with("ADJ") || tag.starts_with("JJ") {
            "ADJ"
        } else {
            tag
        }
        .to_string(),
    )
}

fn in_lexicon_ords(c: char) -> bool {
    c == '\'' || c == '-' || c.is_ascii_alphabetic()
}

type Found = (Option<String>, Option<u8>);

pub struct Lexicon {
    golds: HashMap<String, Entry>,
    silvers: HashMap<String, Entry>,
}

impl Lexicon {
    /// American English, from Misaki's `us_gold.json` and `us_silver.json`.
    pub fn new(gold_json: &str, silver_json: &str) -> anyhow::Result<Self> {
        Ok(Self {
            golds: parse(gold_json)?,
            silvers: parse(silver_json)?,
        })
    }

    fn gold_str(&self, word: &str) -> Option<String> {
        match self.golds.get(word)? {
            Entry::Plain(s) => Some(s.clone()),
            Entry::Tagged(m) => m.get("DEFAULT").cloned().flatten(),
        }
    }

    fn get_nnp(&self, word: &str) -> Found {
        let mut ps = String::new();
        for c in word.chars().filter(|c| c.is_alphabetic()) {
            match self.gold_str(&c.to_uppercase().to_string()) {
                Some(p) => ps.push_str(&p),
                None => return (None, None),
            }
        }
        let ps = apply_stress(&ps, Some(0.0));
        let ps = match ps.rfind(SECONDARY) {
            Some(i) => format!("{}{PRIMARY}{}", &ps[..i], &ps[i + SECONDARY.len_utf8()..]),
            None => ps,
        };
        (Some(ps), Some(3))
    }

    fn get_special_case(&self, word: &str, tag: &str, stress: Option<f32>, ctx: Ctx) -> Found {
        const ADD: [(&str, &str); 2] = [(".", "dot"), ("/", "slash")];
        if tag == "ADD" {
            if let Some((_, w)) = ADD.iter().find(|(k, _)| *k == word) {
                return self.lookup(w, None, Some(-0.5), Some(ctx));
            }
        }
        if let Some(w) = symbol(word) {
            return self.lookup(w, None, None, Some(ctx));
        }
        let inner = word.trim_matches('.');
        if inner.contains('.')
            && is_alpha(&word.replace('.', ""))
            && word
                .split('.')
                .map(|p| p.chars().count())
                .max()
                .unwrap_or(0)
                < 3
        {
            return self.get_nnp(word);
        }
        match word {
            "a" | "A" => return (Some(if tag == "DT" { "ɐ" } else { "ˈA" }.into()), Some(4)),
            "am" | "Am" | "AM" => {
                if tag.starts_with("NN") {
                    return self.get_nnp(word);
                } else if ctx.future_vowel.is_none()
                    || word != "am"
                    || stress.is_some_and(|s| s > 0.0)
                {
                    return (self.gold_str("am"), Some(4));
                }
                return (Some("ɐm".into()), Some(4));
            }
            "an" | "An" | "AN" => {
                if word == "AN" && tag.starts_with("NN") {
                    return self.get_nnp(word);
                }
                return (Some("ɐn".into()), Some(4));
            }
            "I" if tag == "PRP" => return (Some(format!("{SECONDARY}I")), Some(4)),
            "by" | "By" | "BY" if parent_tag(Some(tag)).as_deref() == Some("ADV") => {
                return (Some("bˈI".into()), Some(4))
            }
            _ => {}
        }
        if matches!(word, "to" | "To") || (word == "TO" && matches!(tag, "TO" | "IN")) {
            let ps = match ctx.future_vowel {
                None => self.gold_str("to"),
                Some(false) => Some("tə".into()),
                Some(true) => Some("tʊ".into()),
            };
            return (ps, Some(4));
        }
        if matches!(word, "in" | "In") || (word == "IN" && tag != "NNP") {
            let stress = if ctx.future_vowel.is_none() || tag != "IN" {
                "ˈ"
            } else {
                ""
            };
            return (Some(format!("{stress}ɪn")), Some(4));
        }
        if matches!(word, "the" | "The") || (word == "THE" && tag == "DT") {
            return (
                Some(
                    if ctx.future_vowel == Some(true) {
                        "ði"
                    } else {
                        "ðə"
                    }
                    .into(),
                ),
                Some(4),
            );
        }
        if tag == "IN" && matches!(word.to_lowercase().as_str(), "vs" | "vs.") {
            return self.lookup("versus", None, None, Some(ctx));
        }
        if matches!(word, "used" | "Used" | "USED") {
            let key = if matches!(tag, "VBD" | "JJ") && ctx.future_to {
                "VBD"
            } else {
                "DEFAULT"
            };
            if let Some(Entry::Tagged(m)) = self.golds.get("used") {
                return (m.get(key).cloned().flatten(), Some(4));
            }
        }
        (None, None)
    }

    pub(crate) fn is_known(&self, word: &str) -> bool {
        if self.golds.contains_key(word)
            || symbol(word).is_some()
            || self.silvers.contains_key(word)
        {
            return true;
        }
        if !is_alpha(word) || !word.chars().all(in_lexicon_ords) {
            return false;
        }
        if word.chars().count() == 1 {
            return true;
        }
        if is_upper(word) && self.golds.contains_key(&word.to_lowercase()) {
            return true;
        }
        let rest: String = word.chars().skip(1).collect();
        is_upper(&rest)
    }

    pub(crate) fn lookup(
        &self,
        word: &str,
        tag: Option<&str>,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Found {
        let mut word = word.to_string();
        let mut is_nnp = false;
        if is_upper(&word) && !self.golds.contains_key(&word) {
            word = word.to_lowercase();
            is_nnp = tag == Some("NNP");
        }
        let (mut entry, mut rating) = (self.golds.get(&word), 4);
        if entry.is_none() && !is_nnp {
            entry = self.silvers.get(&word);
            rating = 3;
        }
        let ps: Option<String> = match entry {
            None => None,
            Some(Entry::Plain(s)) => Some(s.clone()),
            Some(Entry::Tagged(m)) => {
                let key = if ctx.is_some_and(|c| c.future_vowel.is_none()) && m.contains_key("None")
                {
                    Some("None".to_string())
                } else if tag.is_some_and(|t| m.contains_key(t)) {
                    tag.map(String::from)
                } else {
                    parent_tag(tag)
                };
                match key.and_then(|k| m.get(&k).cloned()) {
                    Some(v) => v,
                    None => m.get("DEFAULT").cloned().flatten(),
                }
            }
        };
        if ps.is_none() || (is_nnp && !ps.as_deref().is_some_and(|p| p.contains(PRIMARY))) {
            let (nnp, r) = self.get_nnp(&word);
            if nnp.is_some() {
                return (nnp, r);
            }
        }
        (ps.map(|p| apply_stress(&p, stress)), Some(rating))
    }

    fn s(stem: Option<String>) -> Option<String> {
        let stem = stem?;
        let last = stem.chars().last()?;
        Some(if "ptkfθ".contains(last) {
            stem + "s"
        } else if "szʃʒʧʤ".contains(last) {
            stem + "ᵻz"
        } else {
            stem + "z"
        })
    }

    fn stem_s(
        &self,
        word: &str,
        tag: Option<&str>,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Found {
        let n = word.chars().count();
        if n < 3 || !word.ends_with('s') {
            return (None, None);
        }
        let cut = |k: usize| word.chars().take(n - k).collect::<String>();
        let stem = if !word.ends_with("ss") && self.is_known(&cut(1)) {
            cut(1)
        } else if (word.ends_with("'s")
            || (n > 4 && word.ends_with("es") && !word.ends_with("ies")))
            && self.is_known(&cut(2))
        {
            cut(2)
        } else if n > 4 && word.ends_with("ies") && self.is_known(&(cut(3) + "y")) {
            cut(3) + "y"
        } else {
            return (None, None);
        };
        let (ps, rating) = self.lookup(&stem, tag, stress, ctx);
        (Self::s(ps), rating)
    }

    fn ed(stem: Option<String>) -> Option<String> {
        let stem = stem?;
        let chars: Vec<char> = stem.chars().collect();
        let last = *chars.last()?;
        Some(if "pkfθʃsʧ".contains(last) {
            stem + "t"
        } else if last == 'd' {
            stem + "ᵻd"
        } else if last != 't' {
            stem + "d"
        } else if chars.len() < 2 {
            stem + "ɪd"
        } else if US_TAUS.contains(chars[chars.len() - 2]) {
            chars[..chars.len() - 1].iter().collect::<String>() + "ɾᵻd"
        } else {
            stem + "ᵻd"
        })
    }

    fn stem_ed(
        &self,
        word: &str,
        tag: Option<&str>,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Found {
        let n = word.chars().count();
        if n < 4 || !word.ends_with('d') {
            return (None, None);
        }
        let cut = |k: usize| word.chars().take(n - k).collect::<String>();
        let stem = if !word.ends_with("dd") && self.is_known(&cut(1)) {
            cut(1)
        } else if n > 4 && word.ends_with("ed") && !word.ends_with("eed") && self.is_known(&cut(2))
        {
            cut(2)
        } else {
            return (None, None);
        };
        let (ps, rating) = self.lookup(&stem, tag, stress, ctx);
        (Self::ed(ps), rating)
    }

    fn ing(stem: Option<String>) -> Option<String> {
        let stem = stem?;
        let chars: Vec<char> = stem.chars().collect();
        if chars.is_empty() {
            return None;
        }
        if chars.len() > 1
            && chars[chars.len() - 1] == 't'
            && US_TAUS.contains(chars[chars.len() - 2])
        {
            return Some(chars[..chars.len() - 1].iter().collect::<String>() + "ɾɪŋ");
        }
        Some(stem + "ɪŋ")
    }

    fn stem_ing(
        &self,
        word: &str,
        tag: Option<&str>,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Found {
        let n = word.chars().count();
        if n < 5 || !word.ends_with("ing") {
            return (None, None);
        }
        let cut = |k: usize| word.chars().take(n - k).collect::<String>();
        let doubled = {
            let c: Vec<char> = word.chars().collect();
            (n >= 5 && c[n - 4] == c[n - 5] && "bcdgklmnprstvxz".contains(c[n - 4]))
                || word.ends_with("cking")
        };
        let stem = if n > 5 && self.is_known(&cut(3)) {
            cut(3)
        } else if self.is_known(&(cut(3) + "e")) {
            cut(3) + "e"
        } else if n > 5 && doubled && self.is_known(&cut(4)) {
            cut(4)
        } else {
            return (None, None);
        };
        let (ps, rating) = self.lookup(&stem, tag, stress, ctx);
        (Self::ing(ps), rating)
    }

    pub(crate) fn get_word(&self, word: &str, tag: &str, stress: Option<f32>, ctx: Ctx) -> Found {
        let (ps, rating) = self.get_special_case(word, tag, stress, ctx);
        if ps.is_some() {
            return (ps, rating);
        }
        let mut word = word.to_string();
        let wl = word.to_lowercase();
        let rest: String = word.chars().skip(1).collect();
        if word.chars().count() > 1
            && is_alpha(&word.replace('\'', ""))
            && word != wl
            && (tag != "NNP" || word.chars().count() > 7)
            && !self.golds.contains_key(&word)
            && !self.silvers.contains_key(&word)
            && (is_upper(&word) || is_lower(&rest))
            && (self.golds.contains_key(&wl)
                || self.silvers.contains_key(&wl)
                || [Self::stem_s, Self::stem_ed, Self::stem_ing]
                    .iter()
                    .any(|f| {
                        f(self, &wl, Some(tag), stress, Some(ctx))
                            .0
                            .is_some_and(|p| !p.is_empty())
                    }))
        {
            word = wl;
        }
        if self.is_known(&word) {
            return self.lookup(&word, Some(tag), stress, Some(ctx));
        }
        if let Some(stem) = word.strip_suffix("s'") {
            let w = format!("{stem}'s");
            if self.is_known(&w) {
                return self.lookup(&w, Some(tag), stress, Some(ctx));
            }
        }
        if let Some(stem) = word.strip_suffix('\'') {
            if self.is_known(stem) {
                return self.lookup(stem, Some(tag), stress, Some(ctx));
            }
        }
        let found = self.stem_s(&word, Some(tag), stress, Some(ctx));
        if found.0.is_some() {
            return found;
        }
        let found = self.stem_ed(&word, Some(tag), stress, Some(ctx));
        if found.0.is_some() {
            return found;
        }
        let found = self.stem_ing(&word, Some(tag), Some(stress.unwrap_or(0.5)), Some(ctx));
        if found.0.is_some() {
            return found;
        }
        (None, None)
    }

    fn is_currency(word: &str) -> bool {
        if !word.contains('.') {
            return true;
        }
        if word.matches('.').count() > 1 {
            return false;
        }
        let cents = word.split('.').nth(1).unwrap_or("");
        cents.chars().count() < 3
    }

    /// The words of a number (`num` as num2words writes it, or digits) appended to
    /// `out` as phonemes.
    fn extend_num(&self, num: &str, escape: bool, out: &mut Vec<(String, u8)>) {
        let words = if escape {
            num.to_string()
        } else {
            numbers::cardinal(num.parse().unwrap_or(0))
        };
        // Python's re.split on runs of non-letters (num2words' words never start or
        // end with one, so no empty pieces).
        for w in words
            .split(|c: char| !c.is_ascii_lowercase())
            .filter(|w| !w.is_empty())
        {
            if w == "and" {
                continue;
            }
            let (ps, r) = self.lookup(w, None, if w == "point" { Some(-2.0) } else { None }, None);
            out.push((ps.unwrap_or_default(), r.unwrap_or(3)));
        }
    }

    pub(crate) fn get_number(&self, word: &str, currency: Option<char>, is_head: bool) -> Found {
        let suffix_len = word
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_lowercase() || *c == '\'')
            .count();
        let split = word
            .char_indices()
            .rev()
            .nth(suffix_len.saturating_sub(1))
            .map(|(i, _)| i);
        let (word, suffix) = match (suffix_len, split) {
            (0, _) | (_, None) => (word, None),
            (_, Some(i)) => (&word[..i], Some(&word[i..])),
        };
        let mut result: Vec<(String, u8)> = Vec::new();
        let mut word = word;
        if let Some(rest) = word.strip_prefix('-') {
            let (ps, r) = self.lookup("minus", None, None, None);
            result.push((ps.unwrap_or_default(), r.unwrap_or(3)));
            word = rest;
        }
        let is_ordinal = suffix.is_some_and(|s| ORDINALS.contains(&s));
        let currency_name = currency.and_then(currency_units);
        if is_digits(word) && is_ordinal {
            self.extend_num(
                &numbers::ordinal(word.parse().unwrap_or(0)),
                true,
                &mut result,
            );
        } else if result.is_empty()
            && word.chars().count() == 4
            && currency_name.is_none()
            && is_digits(word)
        {
            self.extend_num(&numbers::year(word.parse().unwrap_or(0)), true, &mut result);
        } else if !is_head && !word.contains('.') {
            let num = word.replace(',', "");
            let digits: Vec<char> = num.chars().collect();
            if digits.first() == Some(&'0') || digits.len() > 3 {
                for d in &digits {
                    self.extend_num(&d.to_string(), false, &mut result);
                }
            } else if digits.len() == 3 && !num.ends_with("00") {
                self.extend_num(&digits[0].to_string(), false, &mut result);
                if digits[1] == '0' {
                    let (ps, r) = self.lookup("O", None, Some(-2.0), None);
                    result.push((ps.unwrap_or_default(), r.unwrap_or(3)));
                    self.extend_num(&digits[2].to_string(), false, &mut result);
                } else {
                    self.extend_num(&num[1..], false, &mut result);
                }
            } else {
                self.extend_num(&num, false, &mut result);
            }
        } else if word.matches('.').count() > 1 || !is_head {
            for num in word.replace(',', "").split('.') {
                if num.is_empty() {
                    continue;
                }
                let d: Vec<char> = num.chars().collect();
                if d[0] == '0' || (d.len() != 2 && d[1..].iter().any(|&c| c != '0')) {
                    for c in &d {
                        self.extend_num(&c.to_string(), false, &mut result);
                    }
                } else {
                    self.extend_num(num, false, &mut result);
                }
            }
        } else if let (Some((major, minor)), true) = (currency_name, Self::is_currency(word)) {
            let plain = word.replace(',', "");
            let mut parts = plain.split('.');
            let mut pairs: Vec<(u64, &str)> =
                vec![(parts.next().unwrap_or("").parse().unwrap_or(0), major)];
            if let Some(c) = parts.next() {
                pairs.push((c.parse().unwrap_or(0), minor));
            }
            if pairs.len() > 1 {
                if pairs[1].0 == 0 {
                    pairs.truncate(1);
                } else if pairs[0].0 == 0 {
                    pairs.remove(0);
                }
            }
            for (i, (num, unit)) in pairs.iter().enumerate() {
                if i > 0 {
                    let (ps, r) = self.lookup("and", None, None, None);
                    result.push((ps.unwrap_or_default(), r.unwrap_or(3)));
                }
                self.extend_num(&num.to_string(), false, &mut result);
                let (ps, r) = if *num != 1 && *unit != "pence" {
                    self.stem_s(&format!("{unit}s"), None, None, None)
                } else {
                    self.lookup(unit, None, None, None)
                };
                result.push((ps.unwrap_or_default(), r.unwrap_or(3)));
            }
        } else {
            let words = if is_digits(word) {
                numbers::cardinal(word.parse().unwrap_or(0))
            } else if !word.contains('.') {
                let n = word.replace(',', "").parse().unwrap_or(0);
                if is_ordinal {
                    numbers::ordinal(n)
                } else {
                    numbers::cardinal(n)
                }
            } else {
                let w = word.replace(',', "");
                if let Some(frac) = w.strip_prefix('.') {
                    let digits: Vec<String> = frac
                        .chars()
                        .filter_map(|c| c.to_digit(10))
                        .map(|d| numbers::cardinal(d as u64))
                        .collect();
                    format!("point {}", digits.join(" "))
                } else {
                    numbers::decimal(&w).unwrap_or_default()
                }
            };
            self.extend_num(&words, true, &mut result);
        }
        if result.is_empty() {
            return (None, None);
        }
        let ps = result
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let rating = result.iter().map(|(_, r)| *r).min();
        let ps = match suffix {
            Some("s" | "'s") => Self::s(Some(ps)),
            Some("ed" | "'d") => Self::ed(Some(ps)),
            Some("ing") => Self::ing(Some(ps)),
            _ => Some(ps),
        };
        (ps, rating)
    }

    pub(crate) fn append_currency(&self, ps: String, currency: Option<char>) -> String {
        match currency.and_then(currency_units) {
            Some((major, _)) => match self.stem_s(&format!("{major}s"), None, None, None).0 {
                Some(c) => format!("{ps} {c}"),
                None => ps,
            },
            None => ps,
        }
    }

    pub(crate) fn is_number(word: &str, is_head: bool) -> bool {
        if !word.chars().any(|c| c.is_ascii_digit()) {
            return false;
        }
        let mut w = word;
        for s in ["ing", "'d", "ed", "'s", "st", "nd", "rd", "th", "s"] {
            if let Some(stem) = w.strip_suffix(s) {
                w = stem;
                break;
            }
        }
        w.chars().enumerate().all(|(i, c)| {
            c.is_ascii_digit() || c == ',' || c == '.' || (is_head && i == 0 && c == '-')
        })
    }

    /// `Lexicon.__call__`: a token's phonemes and rating.
    pub(crate) fn token(
        &self,
        text: &str,
        tag: &str,
        token_stress: Option<f32>,
        currency: Option<char>,
        is_head: bool,
        ctx: Ctx,
    ) -> Found {
        use unicode_normalization::UnicodeNormalization;
        let word: String = text.replace(['\u{2018}', '\u{2019}'], "'").nfkc().collect();
        let stress = if is_lower(&word) {
            None
        } else if is_upper(&word) {
            Some(2.0)
        } else {
            Some(0.5)
        };
        let (ps, rating) = self.get_word(&word, tag, stress, ctx);
        if let Some(ps) = ps {
            return (
                Some(apply_stress(
                    &self.append_currency(ps, currency),
                    token_stress,
                )),
                rating,
            );
        }
        if Self::is_number(&word, is_head) {
            let (ps, rating) = self.get_number(&word, currency, is_head);
            return (ps.map(|p| apply_stress(&p, token_stress)), rating);
        }
        (None, None)
    }
}

/// `SYMBOLS`.
fn symbol(word: &str) -> Option<&'static str> {
    Some(match word {
        "%" => "percent",
        "&" => "and",
        "+" => "plus",
        "@" => "at",
        _ => return None,
    })
}

/// `CURRENCIES`: the major and minor unit.
pub(crate) fn currency_units(c: char) -> Option<(&'static str, &'static str)> {
    Some(match c {
        '$' => ("dollar", "cent"),
        '£' => ("pound", "pence"),
        '€' => ("euro", "cent"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stress_moves_as_misaki_moves_it() {
        assert_eq!(apply_stress("kæt", Some(2.0)), "kˈæt");
        assert_eq!(apply_stress("kˈæt", Some(-1.0)), "kˌæt");
        assert_eq!(apply_stress("kˈæt", Some(-2.0)), "kæt");
        assert_eq!(apply_stress("kˌæt", Some(1.0)), "kˈæt");
        assert_eq!(apply_stress("kæt", None), "kæt");
        assert_eq!(apply_stress("pst", Some(2.0)), "pst");
    }
}
