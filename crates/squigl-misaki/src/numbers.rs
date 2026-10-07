//! English numbers in words, as Python's `num2words` writes them (Misaki splits its
//! output at anything not a letter and drops "and", so only the words matter):
//! cardinals, ordinals, years and decimals.

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const SCALES: [&str; 7] = [
    "",
    "thousand",
    "million",
    "billion",
    "trillion",
    "quadrillion",
    "quintillion",
];

/// Below a thousand: "two hundred and thirty-four".
fn below_thousand(n: u64) -> String {
    let (h, rest) = (n / 100, n % 100);
    let tail = match rest {
        0 => String::new(),
        1..=19 => ONES[rest as usize].to_string(),
        _ if rest % 10 == 0 => TENS[(rest / 10) as usize].to_string(),
        _ => format!(
            "{}-{}",
            TENS[(rest / 10) as usize],
            ONES[(rest % 10) as usize]
        ),
    };
    match (h, tail.is_empty()) {
        (0, _) => tail,
        (_, true) => format!("{} hundred", ONES[h as usize]),
        (_, false) => format!("{} hundred and {tail}", ONES[h as usize]),
    }
}

/// "one thousand, two hundred and thirty-four".
pub fn cardinal(n: u64) -> String {
    if n == 0 {
        return "zero".into();
    }
    let mut groups = Vec::new();
    let mut rest = n;
    while rest > 0 {
        groups.push(rest % 1000);
        rest /= 1000;
    }
    let mut parts = Vec::new();
    for (i, &g) in groups.iter().enumerate().rev() {
        if g == 0 {
            continue;
        }
        let words = below_thousand(g);
        parts.push(if i == 0 {
            words
        } else {
            format!("{words} {}", SCALES[i.min(SCALES.len() - 1)])
        });
    }
    // num2words: "and" before a last group under a hundred ("one thousand and
    // five"), commas between the rest. (Misaki drops "and" in the end.)
    let last_small = groups[0] > 0 && groups[0] < 100 && parts.len() > 1;
    if last_small {
        let last = parts.pop().expect("more than one");
        format!("{} and {last}", parts.join(", "))
    } else {
        parts.join(", ")
    }
}

/// "first", "twenty-second", "one hundredth".
pub fn ordinal(n: u64) -> String {
    let words = cardinal(n);
    let split = words.rfind([' ', '-']).map_or(0, |i| i + 1);
    let (head, last) = words.split_at(split);
    let last = match last {
        "one" => "first".to_string(),
        "two" => "second".into(),
        "three" => "third".into(),
        "five" => "fifth".into(),
        "eight" => "eighth".into(),
        "nine" => "ninth".into(),
        "twelve" => "twelfth".into(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    format!("{head}{last}")
}

/// A year: "nineteen eighty-four", "nineteen oh-five", "nineteen hundred"; "two
/// thousand and five" (a cardinal) where num2words reads it as one.
pub fn year(n: u64) -> String {
    let (high, low) = (n / 100, n % 100);
    if high == 0 || (high % 10 == 0 && low < 10) || high >= 100 {
        return cardinal(n);
    }
    let low = match low {
        0 => "hundred".to_string(),
        1..=9 => format!("oh-{}", cardinal(low)),
        _ => cardinal(low),
    };
    format!("{} {low}", cardinal(high))
}

/// A decimal written out digit by digit after the point: "three point one four".
/// `text` is the number as written ("3.14"); `None` if it is not one.
pub fn decimal(text: &str) -> Option<String> {
    let (whole, frac) = text.split_once('.')?;
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut out = cardinal(whole);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        return Some(out);
    }
    out.push_str(" point");
    for d in frac.chars() {
        out.push(' ');
        out.push_str(ONES[d.to_digit(10)? as usize]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_num2words_writes_them() {
        assert_eq!(cardinal(0), "zero");
        assert_eq!(cardinal(42), "forty-two");
        assert_eq!(cardinal(105), "one hundred and five");
        assert_eq!(cardinal(1234), "one thousand, two hundred and thirty-four");
        assert_eq!(cardinal(3_000_000), "three million");
        assert_eq!(ordinal(21), "twenty-first");
        assert_eq!(ordinal(12), "twelfth");
        assert_eq!(ordinal(30), "thirtieth");
        assert_eq!(ordinal(100), "one hundredth");
        assert_eq!(year(1984), "nineteen eighty-four");
        assert_eq!(year(1905), "nineteen oh-five");
        assert_eq!(year(1900), "nineteen hundred");
        assert_eq!(year(2005), "two thousand and five");
        assert_eq!(decimal("3.14").unwrap(), "three point one four");
        assert_eq!(decimal("3.50").unwrap(), "three point five");
    }
}
