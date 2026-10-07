import { describe, expect, it } from "vitest";
import { describe as say, plainText, size, spans, uncertain, type Reading, type ReadResult } from "./reading";

const t = { steady_threshold: 0.92, wavering_threshold: 0.6 };
const tok = (text: string, prob: number) => ({ text, prob, alternates: [{ text: "?", prob: 0.1 }] });

describe("readings", () => {
  it("marks the words the model was unsure of, joining the sure ones", () => {
    const r: Reading = {
      text: "the cat sat",
      count: 1,
      truncated: false,
      tokens: [tok("the", 0.99), tok(" cat", 0.95), tok(" sat", 0.7)],
    };
    expect(spans(r, t)).toEqual([
      { text: "the cat", confidence: "steady", alternates: ["?"] },
      { text: " sat", confidence: "wavering", alternates: ["?"] },
    ]);
    expect(uncertain(spans(r, t))).toBe(1);
  });

  it("shows tokens that do not rebuild the text as plain text", () => {
    const r: Reading = { text: "abc", count: 1, truncated: false, tokens: [tok("ab", 0.1)] };
    expect(spans(r, t)).toEqual([{ text: "abc", confidence: "steady", alternates: [] }]);
    expect(spans({ ...r, tokens: null }, t)[0]!.confidence).toBe("steady");
  });

  it("copies a result as text", () => {
    const ok = (readings: Reading[], samples: number): ReadResult => ({
      label: null,
      result: {
        Ok: { backend: "b", readings, silent: 0, samples, elapsed: { secs: 1, nanos: 0 } },
      },
    });
    const one: Reading = { text: "x", count: 2, truncated: false, tokens: null };
    expect(plainText(ok([one], 1))).toBe("x");
    expect(plainText(ok([one, { ...one, text: "y", count: 1 }], 3))).toBe("[2/3] x\n[1/3] y");
    expect(plainText({ label: null, result: { Err: "no" } })).toBe("(failed: no)");
  });

  it("says where a model is", () => {
    expect(size(650_000_000)).toBe("650 MB");
    expect(say({ phase: "not-installed", size: 1_200_000_000 })).toBe("Not downloaded yet (1.2 GB)");
    expect(say({ phase: "downloading", done: 10e6, total: 650e6 })).toBe("Downloading: 10 MB of 650 MB");
  });
});
