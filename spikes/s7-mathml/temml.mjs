// Temml's MathML for every expression in corpus.json (written by the Rust side),
// into temml.json, for the report's third column.
import { readFileSync, writeFileSync } from "node:fs";
import temml from "temml";

const corpus = JSON.parse(readFileSync("corpus.json", "utf8"));
const out = corpus.map((latex) => {
  try {
    return { mathml: temml.renderToString(latex, { displayMode: true, throwOnError: true }), error: null };
  } catch (e) {
    let mathml = "";
    try {
      mathml = temml.renderToString(latex, { displayMode: true, throwOnError: false });
    } catch (crash) {
      // Even throwOnError: false can throw (an internal TypeError on "x^").
      return { mathml: "", error: `crashed: ${crash}` };
    }
    return { mathml, error: String(e.message ?? e) };
  }
});
writeFileSync("temml.json", JSON.stringify(out));
const t0 = performance.now();
for (const latex of corpus) {
  try {
    temml.renderToString(latex, { displayMode: true, throwOnError: false });
  } catch {}
}
console.log(`Temml: ${out.filter((o) => o.error).length} errors, ${((performance.now() - t0) * 1000).toFixed(0)} us in all`);
