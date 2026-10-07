//! What spaCy does for Misaki, roughly: text into tokens (punctuation off the ends
//! of words) and a Penn Treebank tag for each. Misaki needs the tag only where it
//! changes how a word is said -- "a" as a determiner (`ɐ`), "the", "to" and "in"
//! weak, the 790 dictionary words said differently as noun or verb ("use",
//! "object", "present"), "read" and "used" in the past, "that" as a determiner --
//! so function words come from a list and the rest are guessed from their
//! neighbours.

/// The tag each function word usually has.
fn closed(word: &str) -> Option<&'static str> {
    Some(match word {
        "a" | "an" | "the" | "this" | "these" | "those" | "each" | "every" | "some" | "any"
        | "no" | "another" | "either" | "neither" | "all" | "both" => "DT",
        "of" | "in" | "on" | "at" | "by" | "for" | "with" | "from" | "about" | "into" | "over"
        | "after" | "before" | "under" | "between" | "through" | "during" | "without"
        | "within" | "against" | "among" | "upon" | "around" | "across" | "behind" | "beyond"
        | "toward" | "towards" | "since" | "until" | "unless" | "because" | "although"
        | "though" | "whereas" | "while" | "if" | "whether" | "than" | "as" | "like" | "per"
        | "via" | "near" | "onto" | "inside" | "outside" | "below" | "above" | "except" => "IN",
        "to" => "TO",
        "and" | "or" | "but" | "nor" | "yet" | "plus" => "CC",
        "i" | "you" | "he" | "she" | "it" | "we" | "they" | "me" | "him" | "us" | "them"
        | "myself" | "yourself" | "himself" | "herself" | "itself" | "ourselves" | "themselves"
        | "one" => "PRP",
        "my" | "your" | "his" | "her" | "its" | "our" | "their" => "PRP$",
        "which" | "whatever" | "whichever" => "WDT",
        "who" | "whom" | "what" | "whoever" => "WP",
        "whose" => "WP$",
        "when" | "where" | "why" | "how" => "WRB",
        "can" | "could" | "may" | "might" | "must" | "shall" | "should" | "will" | "would"
        | "can't" | "couldn't" | "won't" | "wouldn't" | "shouldn't" | "mustn't" => "MD",
        "is" | "has" | "does" | "isn't" | "hasn't" | "doesn't" => "VBZ",
        "are" | "am" | "have" | "do" | "aren't" | "haven't" | "don't" => "VBP",
        "was" | "were" | "had" | "did" | "wasn't" | "weren't" | "hadn't" | "didn't" => "VBD",
        "be" => "VB",
        "been" => "VBN",
        "being" => "VBG",
        "not" | "n't" | "very" | "also" | "just" | "only" | "even" | "still" | "already"
        | "never" | "always" | "often" | "too" | "quite" | "rather" | "really" | "then" | "now"
        | "here" | "there" | "so" | "again" | "almost" | "perhaps" | "however" => "RB",
        "up" | "out" | "off" | "down" | "away" | "back" => "RP",
        "once" => "IN",
        "first" | "last" | "next" | "other" | "same" | "own" | "new" | "old" | "few" | "many"
        | "more" | "most" | "less" | "such" | "second" | "third" => "JJ",
        _ => return None,
    })
}

const PREFIXES: &str = "\"“‘'([{$£€#*";
const SUFFIXES: &str = "\"”’')]},;:!?%…*";

/// The text as (token, whitespace after it): split at spaces, punctuation peeled
/// off both ends of each word ("(child" → "(", "child"), a sentence's full stop
/// off its last word but not off an abbreviation ("e.g.", "U.S.A.").
pub fn tokenize(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for (wi, word) in words.iter().enumerate() {
        let space = if wi + 1 < words.len() { " " } else { "" };
        let mut rest: &str = word;
        let mut pieces: Vec<String> = Vec::new();
        while let Some(c) = rest.chars().next().filter(|c| PREFIXES.contains(*c)) {
            // A lone apostrophe word ("'s", "'cause") keeps it.
            if (c == '\'' || c == '‘') && rest.chars().nth(1).is_some_and(char::is_alphabetic) {
                break;
            }
            pieces.push(c.to_string());
            rest = &rest[c.len_utf8()..];
        }
        let mut tail: Vec<String> = Vec::new();
        while rest.chars().count() > 1 {
            if rest.len() > 3 {
                if let Some(stem) = rest.strip_suffix("...") {
                    tail.push("...".into());
                    rest = stem;
                    continue;
                }
            }
            let c = rest.chars().last().expect("not empty");
            let stem = &rest[..rest.len() - c.len_utf8()];
            // An abbreviation keeps its full stop: letters in ones and twos between
            // the dots ("e.g.", "U.S.A."), as Misaki spells them.
            let abbreviation = c == '.'
                && stem.contains('.')
                && stem.split('.').all(|p| {
                    !p.is_empty() && p.chars().count() < 3 && p.chars().all(char::is_alphabetic)
                });
            if !(SUFFIXES.contains(c) || (c == '.' && !abbreviation)) {
                break;
            }
            // An apostrophe ending a plural possessive ("parents'") stays.
            if (c == '\'' || c == '’') && stem.ends_with('s') {
                break;
            }
            tail.push(c.to_string());
            rest = stem;
        }
        if !rest.is_empty() {
            pieces.push(rest.to_string());
        }
        pieces.extend(tail.into_iter().rev());
        let n = pieces.len();
        for (i, p) in pieces.into_iter().enumerate() {
            out.push((
                p,
                if i + 1 == n {
                    space.to_string()
                } else {
                    String::new()
                },
            ));
        }
    }
    out
}

fn punctuation_tag(token: &str, opening: bool) -> Option<&'static str> {
    Some(match token {
        "." | "!" | "?" | "..." | "…" => ".",
        "," => ",",
        ":" | ";" | "-" | "–" | "—" | "--" => ":",
        "(" | "[" | "{" => "-LRB-",
        ")" | "]" | "}" => "-RRB-",
        "“" | "‘" => "``",
        "”" | "’" => "''",
        "\"" | "'" => {
            if opening {
                "``"
            } else {
                "''"
            }
        }
        "$" | "£" | "€" => "$",
        "#" => "#",
        "&" => "CC",
        "%" => "NN",
        _ if !token.chars().any(|c| c.is_alphanumeric()) => "SYM",
        _ => return None,
    })
}

fn is_sentence_start(prev: Option<&str>) -> bool {
    matches!(prev, None | Some("." | ":" | "``" | "-LRB-"))
}

/// Penn tags for `tokens` (from [`tokenize`]): (text, tag, whitespace).
pub fn tag(tokens: &[(String, String)]) -> Vec<(String, String, String)> {
    let mut tags: Vec<String> = Vec::with_capacity(tokens.len());
    for (i, (text, _)) in tokens.iter().enumerate() {
        let prev_tag = tags.last().map(String::as_str);
        let prev_word = i.checked_sub(1).map(|j| tokens[j].0.to_lowercase());
        let next = tokens.get(i + 1).map(|(t, _)| t.as_str());
        let opening = i == 0 || !tokens[i - 1].1.is_empty();
        let lower = text.to_lowercase();
        let tag: &str = if let Some(t) = punctuation_tag(text, opening) {
            t
        } else if text.chars().any(|c| c.is_ascii_digit())
            && !text
                .chars()
                .any(|c| c.is_alphabetic() && !c.is_ascii_lowercase())
        {
            "CD"
        } else if lower == "that" {
            // A determiner (stressed) before a noun or alone; else "that" joins.
            let next_ends = next.is_none_or(|n| punctuation_tag(n, false).is_some());
            let after_noun = prev_tag.is_some_and(|t| t.starts_with("NN") || t == "-RRB-")
                || prev_word.as_deref().is_some_and(|w| {
                    matches!(
                        w,
                        "one" | "those" | "something" | "anything" | "everything" | "nothing"
                    )
                });
            let next_starts_clause = next
                .and_then(|n| closed(&n.to_lowercase()))
                .is_some_and(|t| matches!(t, "PRP" | "DT" | "PRP$"));
            if next_starts_clause {
                // "I know that you…": joining a clause, unstressed.
                "IN"
            } else if after_noun && !next_ends {
                "WDT"
            } else {
                // A determiner or pronoun: "that one", "I know that", "That is".
                "DT"
            }
        } else if text.chars().filter(|c| c.is_alphabetic()).count() >= 2
            && text
                .chars()
                .filter(|c| c.is_alphabetic())
                .all(char::is_uppercase)
            && !matches!(text.as_str(), "I")
        {
            // An acronym, even one spelling a word ("US", "IT").
            "NNP"
        } else if let Some(t) = closed(&lower) {
            // "I" only as the pronoun; a capital "A" may be a letter.
            if text == "A" && next.is_some_and(|n| n.starts_with(char::is_uppercase)) {
                "NNP"
            } else {
                t
            }
        } else {
            let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
            let all_caps = letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase());
            let capital = text.starts_with(char::is_uppercase);
            let start = is_sentence_start(prev_tag);
            let after = |ts: &[&str]| prev_tag.is_some_and(|p| ts.contains(&p));
            let next_tag = next.and_then(|n| closed(&n.to_lowercase()));
            if all_caps || (capital && !start) {
                "NNP"
            } else if after(&["TO", "MD"])
                || prev_word.as_deref().is_some_and(|w| {
                    matches!(
                        w,
                        "do" | "does" | "did" | "don't" | "doesn't" | "didn't" | "please" | "let"
                    )
                })
            {
                "VB"
            } else if prev_word
                .as_deref()
                .is_some_and(|w| matches!(w, "i" | "you" | "we" | "they"))
            {
                if lower.ends_with("ed") {
                    "VBD"
                } else {
                    "VBP"
                }
            } else if prev_word
                .as_deref()
                .is_some_and(|w| matches!(w, "he" | "she" | "it"))
                && lower.ends_with('s')
            {
                "VBZ"
            } else if lower.ends_with("ing") && lower.len() > 4 {
                "VBG"
            } else if lower.ends_with("ly") && lower.len() > 4 {
                "RB"
            } else if lower == "read"
                && prev_word.as_deref().is_some_and(|w| {
                    matches!(
                        w,
                        "has"
                            | "have"
                            | "had"
                            | "was"
                            | "were"
                            | "is"
                            | "are"
                            | "be"
                            | "been"
                            | "being"
                    )
                })
            {
                "VBN"
            } else if lower.ends_with("ed") && lower.len() > 3 {
                if prev_word.as_deref().is_some_and(|w| {
                    matches!(
                        w,
                        "has"
                            | "have"
                            | "had"
                            | "was"
                            | "were"
                            | "is"
                            | "are"
                            | "be"
                            | "been"
                            | "being"
                            | "get"
                            | "got"
                    )
                }) {
                    "VBN"
                } else if after(&["DT", "PRP$", "IN"]) {
                    "JJ"
                } else {
                    "VBD"
                }
            } else if start && matches!(next_tag, Some("DT" | "PRP$")) {
                // An instruction: "Read the record".
                "VB"
            } else if after(&["DT", "PRP$", "JJ", "IN", "CD", "POS"]) || start {
                if lower.ends_with('s') && !lower.ends_with("ss") {
                    "NNS"
                } else {
                    "NN"
                }
            } else if lower == "read" && after(&["NN", "NNS", "NNP", "PRP"]) {
                "VBD"
            } else if after(&["NN", "NNS", "NNP", "PRP"])
                && (matches!(next_tag, Some("DT" | "PRP$" | "PRP"))
                    || next.is_some_and(|n| {
                        n.contains('-')
                            || n.starts_with(|c: char| c.is_uppercase() || c.is_ascii_digit())
                    }))
            {
                // Between a subject and what follows: a verb ("squigl uses X",
                // "GLM-OCR read every sample").
                if lower == "read" {
                    "VBD"
                } else if lower.ends_with('s') {
                    "VBZ"
                } else {
                    "VBP"
                }
            } else if lower.ends_with('s') && !lower.ends_with("ss") {
                "NNS"
            } else {
                "NN"
            }
        };
        tags.push(tag.to_string());
    }
    tokens
        .iter()
        .zip(tags)
        .map(|((t, w), tag)| (t.clone(), tag, w.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        tokenize(s).into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn punctuation_comes_off_words_but_not_abbreviations() {
        assert_eq!(
            texts("E (parallel node) ends."),
            ["E", "(", "parallel", "node", ")", "ends", "."]
        );
        assert_eq!(
            texts("I.e., the U.S.A. features..."),
            ["I.e.", ",", "the", "U.S.A.", "features", "..."]
        );
        assert_eq!(
            texts("It costs $3.50, about 40%."),
            ["It", "costs", "$", "3.50", ",", "about", "40", "%", "."]
        );
        assert_eq!(
            texts("\"Don't,\" she said."),
            ["\"", "Don't", ",", "\"", "she", "said", "."]
        );
    }

    #[test]
    fn tags_where_saying_depends_on_them() {
        let tags =
            |s: &str| -> Vec<String> { tag(&tokenize(s)).into_iter().map(|(_, t, _)| t).collect() };
        assert_eq!(tags("I use a pen"), ["PRP", "VBP", "DT", "NN"]);
        assert_eq!(tags("the use of it"), ["DT", "NN", "IN", "PRP"]);
        assert_eq!(tags("Read the record."), ["VB", "DT", "NN", "."]);
        assert_eq!(tags("I know that."), ["PRP", "VBP", "DT", "."]);
        assert_eq!(tags("data that splits"), ["NN", "WDT", "NNS"]);
    }
}
