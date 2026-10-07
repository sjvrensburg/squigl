import { describe, expect, it } from "vitest";
import { brushInView, coverStrokes, farEnough } from "./erase";
import type { Stroke } from "./engine";

/** Distance from `p` to the nearest point of a stroke's path. */
function reach(s: Stroke, p: [number, number]): number {
  const [a, b] = [s.points[0]!, s.points[s.points.length - 1]!];
  const [dx, dy] = [b[0] - a[0], b[1] - a[1]];
  const len2 = dx * dx + dy * dy;
  const t = len2 ? Math.min(1, Math.max(0, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2)) : 0;
  return Math.hypot(p[0] - (a[0] + t * dx), p[1] - (a[1] + t * dy));
}

const covered = (strokes: Stroke[], p: [number, number]) => strokes.some((s) => reach(s, p) <= s.radius + 1e-6);

describe("erasing", () => {
  it("sizes the brush in view pixels", () => {
    expect(brushInView(12, { origin: [0, 0], scale: 4 }, 2)).toBe(6);
    expect(farEnough([0, 0], [1, 0], 8)).toBe(false);
    expect(farEnough([0, 0], [2, 0], 8)).toBe(true);
  });

  it("covers a box, spilling over by little", () => {
    const box: [number, number][] = [
      [10, 20],
      [110, 20],
      [110, 70],
      [10, 70],
    ];
    const strokes = coverStrokes(box);
    expect(strokes.length).toBe(5);
    for (let x = 10; x <= 110; x += 2.5)
      for (let y = 20; y <= 70; y += 2.5) expect(covered(strokes, [x, y]), `${x},${y}`).toBe(true);
    // Nothing beyond a fifth of a row (10 px) outside it.
    expect(covered(strokes, [10, 17.5])).toBe(false);
    expect(covered(strokes, [7.5, 45])).toBe(false);
  });

  it("covers a tilted quad and a box narrower than a row", () => {
    const quad: [number, number][] = [
      [20, 0],
      [100, 20],
      [90, 60],
      [10, 40],
    ];
    const strokes = coverStrokes(quad);
    for (const p of [
      [20, 1],
      [99, 20],
      [90, 59],
      [11, 40],
      [55, 30],
    ] as [number, number][])
      expect(covered(strokes, p), `${p}`).toBe(true);
    const thin = coverStrokes([
      [0, 0],
      [4, 0],
      [4, 30],
      [0, 30],
    ]);
    expect(thin.every((s) => s.points.length === 1)).toBe(true);
    expect(covered(thin, [0, 0])).toBe(true);
    expect(coverStrokes([[0, 0], [5, 0], [5, 0], [0, 0]])).toEqual([]);
  });
});
