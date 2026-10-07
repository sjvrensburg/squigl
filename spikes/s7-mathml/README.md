# Spike S7: LaTeX to MathML, and maths speech

Roadmap spike S7 asks which converter Phase 8 should use for showing readings'
maths as MathML instead of Typst images, and how MathCAT (ClearSpeak) speaks that
MathML.

The candidates:

- `pulldown-latex` 0.8 (Rust);
- `math-core` 0.8.2 (Rust; formerly `latex2mmlc`);
- Temml 0.11 (JavaScript).

## Running it

```
cargo run --release      # writes corpus.json, report.html, speech.json
npm ci && node temml.mjs && cargo run --release   # adds the Temml column
WebKitWebDriver --port=4445 & node shot.mjs                 # report-N.png, in WebKitGTK's MiniBrowser
```

The corpus is every `$…$`/`$$…$$` in the recorded readings in
`crates/squigl-models/testdata/handwriting`, plus `corpus.tex`. `corpus.tex` holds
what a handwriting model is likely to emit, including unknown commands (`\softmax`,
`\Var`) and three inputs broken the way models break them. There are 71 expressions
in all. Each converter's MathML is spoken by MathCAT 0.7.7-alpha.1, built with
`include-zip`, in ClearSpeak.

## Findings (2026-10-07)

**Pick `math-core`, in `squigl-engine`.** It gives one MathML for both front ends
and for MathCAT, so the egui window and read-aloud get the same maths as the
desktop page.

**Errors:** pulldown-latex 5, math-core 5, Temml 3.

- The 5 are the 2 unknown commands and the 3 broken inputs. Temml does not fail on
  unknown commands; it shows them as red source text.
- **math-core with retries:** when it reports an unknown command, define that
  command as `\operatorname{…}` and convert again. This is what squigl-math does
  with MiTeX. Only the 3 broken inputs then fail.
- **Broken input** (an unclosed `{`, a `\left(` with no `\right`, `x^`) fails in
  every converter. Temml *throws* on `x^` even with `throwOnError: false`. Such a
  reading is shown as its source text.

**pulldown-latex is out.**

- Its MathML for the recorded entropy formula is malformed: MathCAT rejects it
  with "msub should have 2 children", and it renders wrongly.
- It writes `\operatorname*{argmax}` out as source text.
- It maps `\mu` to U+00B5, the micro sign, so MathCAT says "micro".
- It speaks `\|w\|` as "is parallel to".

**Temml** renders as well as math-core, and its MathML is spoken about as well.
But it is JavaScript, so it would serve only the webview. It also crashes on one
broken input.

**Speed:** about 11 µs per expression for either Rust converter (all 71 in 0.8
ms), against about 57 µs for Temml in Node.

**What math-core needs from us** (all in `src/main.rs`, as they would go into the
engine):

- **`for_webkit`:**
  - **The problem:** math-core writes `\log_2`, `\sin^2` and similar as an `<mo>`
    base with its own spacing inside the `<msub>`/`<msup>`. WebKitGTK puts that
    space between the base and its script ("log ₂p", "sin ²θ"). MathML Core has
    it around the whole embellished operator.
  - **The fix:** the base becomes an `<mi>`, with an `<mspace>` of the same width
    before and after the whole. That renders right in MiniBrowser (WebKitGTK
    4.1), and the speech is unchanged.
- **`speakable`**, for MathCAT only:
  - Drop the variation selectors (U+FE00/U+FE01) that math-core puts on
    `\mathcal` letters. Without this, "script cap L" was read out as the bare
    character.
  - Write the vector arrow as U+2192, not the combining U+20D7. "v with right
    arrow above embellishment above" becomes "vector v".
  - Drop the invisible separator math-core puts after a `cases` table. "Open
    brace; array of; row 1; column 1…" becomes "2 cases; case 1…".

**ClearSpeak's speech** is good on the readings themselves. Examples:

- "the log base 2, of p sub i";
- "the fraction with numerator; partial derivative cap l; and denominator partial
  derivative g hat";
- "the 2 by 2 identity matrix";
- "n choose k".

A few oddities are MathCAT's own and do not depend on the converter:

- "eigh" for the variable *a*;
- Greek runs with no spaces spoken as one word;
- `\sim` as "varies with".

**Cost:** the spike's release binary is 7.7 MB, with MathCAT, its zipped rules and
both converters included.

**Still open (tester):**

- MathCAT's speech through the `tts` crate on each OS: speech-dispatcher, WinRT
  and AVSpeech, and how each voice says "cap e", "sub" and the like.
- math-core's rendering in WebView2 (Chromium) and WKWebView (macOS), with and
  without `for_webkit`.
