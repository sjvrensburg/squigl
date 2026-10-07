// A reading's maths, shown as MathML: the engine's text and maths runs
// (squigl_engine::math::Part, by hand), the reading's confidence marks laid over
// them -- a formula is marked as a whole, by its least sure token, as the egui
// window shades it -- and the MathML rebuilt from MathML elements only before it
// goes into the page (it comes from a model's output).
import { confidence, type Confidence, type Reading, type Span, type Thresholds } from "./reading";

export type Part =
  | { kind: "text"; text: string; start: number; end: number }
  | { kind: "math"; source: string; display: boolean; mathml: string | null; start: number; end: number };

export type Run =
  | { kind: "text"; spans: Span[] }
  | { kind: "math"; source: string; display: boolean; mathml: string | null; confidence: Confidence; alternates: string[] };

const WORSE: Record<Confidence, number> = { steady: 0, wavering: 1, hesitant: 2 };

/**
 * The reading as runs to show: its text runs split by confidence (neighbouring
 * steady tokens joined), its maths runs marked by their least sure token. Without
 * tokens that rebuild the text exactly, everything is steady.
 */
export function runs(r: Reading, t: Thresholds, parts: Part[]): Run[] {
  const tokens = r.tokens?.length && r.tokens.map((k) => k.text).join("") === r.text ? r.tokens : null;
  // Each token's place in the text, in UTF-16 units (a JS string's).
  const placed: { start: number; end: number; c: Confidence; alternates: string[] }[] = [];
  let at = 0;
  for (const k of tokens ?? []) {
    placed.push({
      start: at,
      end: at + k.text.length,
      c: confidence(k.prob, t),
      alternates: k.alternates.map((a) => a.text),
    });
    at += k.text.length;
  }
  const out = parts.map((p): Run => {
    const over = placed.filter((k) => k.start < p.end && k.end > p.start);
    if (p.kind === "math") {
      const worst = over.reduce<(typeof placed)[number] | null>(
        (w, k) => (!w || WORSE[k.c] > WORSE[w.c] ? k : w),
        null,
      );
      const c = worst?.c ?? "steady";
      return { ...p, confidence: c, alternates: c === "steady" ? [] : (worst?.alternates ?? []) };
    }
    if (!tokens) return { kind: "text", spans: [{ text: p.text, confidence: "steady", alternates: [] }] };
    const spans: Span[] = [];
    for (const k of over) {
      const text = r.text.slice(Math.max(k.start, p.start), Math.min(k.end, p.end));
      const last = spans[spans.length - 1];
      if (last && k.c === "steady" && last.confidence === "steady") last.text += text;
      else spans.push({ text, confidence: k.c, alternates: k.alternates });
    }
    return { kind: "text", spans };
  });
  // A display formula is a block of its own: the line breaks around it in the
  // source would be blank lines beside it.
  out.forEach((run, i) => {
    if (run.kind !== "math" || !run.display) return;
    const before = out[i - 1];
    const after = out[i + 1];
    const last = before?.kind === "text" ? before.spans[before.spans.length - 1] : undefined;
    if (last) last.text = last.text.replace(/\n$/, "");
    const first = after?.kind === "text" ? after.spans[0] : undefined;
    if (first) first.text = first.text.replace(/^\n/, "");
  });
  return out;
}

/** How many runs were uncertain (words, and formulas as a whole), for a summary a screen reader can say. */
export function uncertainRuns(rs: Run[]): number {
  let n = 0;
  for (const r of rs) {
    if (r.kind === "math") n += r.confidence !== "steady" ? 1 : 0;
    else n += r.spans.filter((s) => s.confidence !== "steady" && s.text.trim()).length;
  }
  return n;
}

const MATHML_NS = "http://www.w3.org/1998/Math/MathML";

/** The MathML Core elements a converter writes; anything else is dropped. */
const ELEMENTS = new Set([
  "math", "mrow", "mi", "mn", "mo", "ms", "mtext", "mspace", "msub", "msup", "msubsup",
  "munder", "mover", "munderover", "mfrac", "msqrt", "mroot", "mtable", "mtr", "mtd",
  "mstyle", "mpadded", "mphantom", "merror", "semantics", "mprescripts", "mmultiscripts", "none",
]);

/** Presentation attributes only: no events, links or anything that loads. */
const ATTRIBUTES = new Set([
  "display", "displaystyle", "scriptlevel", "mathvariant", "lspace", "rspace", "stretchy",
  "symmetric", "largeop", "movablelimits", "accent", "accentunder", "form", "fence",
  "separator", "width", "height", "depth", "voffset", "linethickness", "columnspan", "rowspan",
  "columnalign", "rowalign", "minsize", "maxsize", "style",
]);

function copy(from: Element, doc: Document): Element | null {
  const name = from.localName;
  if (!ELEMENTS.has(name)) return null;
  const to = doc.createElementNS(MATHML_NS, name);
  for (const a of from.attributes) {
    if (!ATTRIBUTES.has(a.name)) continue;
    // Inline style is layout (math-core aligns table cells with it), never a URL.
    if (a.name === "style" && /url\(|@import|expression/i.test(a.value)) continue;
    to.setAttribute(a.name, a.value);
  }
  for (const child of from.childNodes) {
    if (child.nodeType === Node.TEXT_NODE) to.appendChild(doc.createTextNode(child.textContent ?? ""));
    else if (child.nodeType === Node.ELEMENT_NODE) {
      const c = copy(child as Element, doc);
      if (c) to.appendChild(c);
    }
  }
  return to;
}

/** `mathml` rebuilt from MathML elements and presentation attributes alone; null if it holds no `<math>`. */
export function sanitize(mathml: string, doc: Document = document): Element | null {
  const parsed = new DOMParser().parseFromString(mathml, "text/html");
  const math = parsed.body.querySelector("math");
  return math && math.namespaceURI === MATHML_NS ? copy(math, doc) : null;
}
