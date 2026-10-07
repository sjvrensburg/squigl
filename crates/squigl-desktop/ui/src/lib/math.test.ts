import { describe, expect, it } from "vitest";
import { runs, uncertainRuns, type Part } from "./math";
import type { Reading } from "./reading";

const t = { steady_threshold: 0.92, wavering_threshold: 0.6 };
const tok = (text: string, prob: number) => ({ text, prob, alternates: [{ text: "?", prob: 0.1 }] });

// "is $k>2$ ok": text 0-3, maths 3-8, text 8-11.
const parts: Part[] = [
  { kind: "text", text: "is ", start: 0, end: 3 },
  { kind: "math", source: "$k>2$", display: false, mathml: "<math></math>", start: 3, end: 8 },
  { kind: "text", text: " ok", start: 8, end: 11 },
];

describe("maths runs", () => {
  it("marks a formula by its least sure token, and the text around it by its own", () => {
    const r: Reading = {
      text: "is $k>2$ ok",
      count: 1,
      truncated: false,
      tokens: [tok("is", 0.99), tok(" $k", 0.99), tok(">", 0.7), tok("2$", 0.3), tok(" o", 0.99), tok("k", 0.8)],
    };
    const out = runs(r, t, parts);
    expect(out[0]).toEqual({ kind: "text", spans: [{ text: "is ", confidence: "steady", alternates: ["?"] }] });
    expect(out[1]).toMatchObject({ kind: "math", source: "$k>2$", confidence: "hesitant", alternates: ["?"] });
    expect(out[2]).toEqual({
      kind: "text",
      spans: [
        { text: " o", confidence: "steady", alternates: ["?"] },
        { text: "k", confidence: "wavering", alternates: ["?"] },
      ],
    });
  });

  it("shows everything as steady without tokens that rebuild the text", () => {
    const r: Reading = { text: "is $k>2$ ok", count: 1, truncated: false, tokens: [tok("is", 0.1)] };
    const out = runs(r, t, parts);
    expect(out[0]).toEqual({ kind: "text", spans: [{ text: "is ", confidence: "steady", alternates: [] }] });
    expect(out[1]).toMatchObject({ confidence: "steady", alternates: [] });
  });

  it("sets a display formula apart without the blank lines around it, and counts what was unsure", () => {
    const r: Reading = { text: "a\n$$x$$\nb", count: 1, truncated: false, tokens: null };
    const out = runs(r, t, [
      { kind: "text", text: "a\n", start: 0, end: 2 },
      { kind: "math", source: "$$x$$", display: true, mathml: "<math></math>", start: 2, end: 7 },
      { kind: "text", text: "\nb", start: 7, end: 9 },
    ]);
    expect(out[0]).toEqual({ kind: "text", spans: [{ text: "a", confidence: "steady", alternates: [] }] });
    expect(out[2]).toEqual({ kind: "text", spans: [{ text: "b", confidence: "steady", alternates: [] }] });
    expect(uncertainRuns(out)).toBe(0);
  });

  it("marks the sentence being read aloud, in text and maths", () => {
    const r: Reading = { text: "is $k>2$ ok", count: 1, truncated: false, tokens: null };
    const out = runs(r, t, parts, [1, 5]);
    expect(out[0]).toEqual({
      kind: "text",
      spans: [
        { text: "i", confidence: "steady", alternates: [] },
        { text: "s ", confidence: "steady", alternates: [], current: true },
      ],
    });
    expect(out[1]).toMatchObject({ kind: "math", current: true });
    expect(out[2]).toEqual({ kind: "text", spans: [{ text: " ok", confidence: "steady", alternates: [] }] });
  });
});
