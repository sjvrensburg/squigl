//! The port against Misaki itself: `testdata/reference.tsv` is Python Misaki
//! (fba1236, spaCy's en_core_web_sm tagging it) on squigl's own prose, the
//! recorded handwriting readings as squigl says them, and lines of numbers,
//! money and words said two ways; `testdata/fallback.tsv` is its fallback network
//! on words in neither dictionary. Both need the data: `$SQUIGL_MODEL_DIR/misaki`
//! (us_gold.json, us_silver.json) and `$SQUIGL_MODEL_DIR/misaki-fallback`
//! (config.json, model.safetensors), as the Kokoro voice downloads them.

use squigl_misaki::{Fallback, G2p, Lexicon};
use std::path::PathBuf;

fn data(name: &str) -> PathBuf {
    let dir = std::env::var_os("SQUIGL_MODEL_DIR").expect("SQUIGL_MODEL_DIR");
    PathBuf::from(dir).join(name)
}

fn lines(file: &str) -> Vec<(String, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(file);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
#[ignore = "needs Misaki's fallback model in $SQUIGL_MODEL_DIR/misaki-fallback"]
fn the_fallback_reads_as_misakis_does() {
    let fallback = Fallback::load(&data("misaki-fallback")).unwrap();
    let mut wrong = Vec::new();
    for (word, want) in lines("fallback.tsv") {
        let got = fallback.phonemes(&word);
        if got != want {
            wrong.push(format!("{word}: want {want}, got {got}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
#[ignore = "needs Misaki's dictionaries and fallback in $SQUIGL_MODEL_DIR"]
fn sentences_come_out_as_misakis() {
    let dir = data("misaki");
    let lexicon = Lexicon::new(
        &std::fs::read_to_string(dir.join("us_gold.json")).unwrap(),
        &std::fs::read_to_string(dir.join("us_silver.json")).unwrap(),
    )
    .unwrap();
    let g2p = G2p::new(
        lexicon,
        Some(Fallback::load(&data("misaki-fallback")).unwrap()),
    );
    let (mut words, mut same, mut lines_same) = (0, 0, 0);
    let mut shown = 0;
    let reference = lines("reference.tsv");
    for (text, want) in &reference {
        let got = g2p.phonemes(text);
        if &got == want {
            lines_same += 1;
        }
        let (w, g): (Vec<&str>, Vec<&str>) = (want.split(' ').collect(), got.split(' ').collect());
        words += w.len();
        if w.len() == g.len() {
            same += w.iter().zip(&g).filter(|(a, b)| a == b).count();
        }
        if &got != want && shown < 40 {
            shown += 1;
            println!("{text}\n  want {want}\n  got  {got}");
        }
    }
    let rate = same as f64 / words as f64;
    println!(
        "{lines_same}/{} lines and {same}/{words} words ({:.1}%) as Misaki has them",
        reference.len(),
        100.0 * rate
    );
    assert!(rate > 0.98, "only {:.1}% of words agree", 100.0 * rate);
}
