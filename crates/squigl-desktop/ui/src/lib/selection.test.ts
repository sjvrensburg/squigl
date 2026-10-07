import { describe, expect, it } from "vitest";
import { blockAt, boxBetween, clampPoint, moved, outline, role, scaled, toCanvas, toView } from "./selection";
import type { Block } from "./engine";

const block = (label: string, x: number, y: number, w: number, h: number): Block => ({
  label,
  score: 0.9,
  rect: { x, y, w, h },
  quad: null,
});

describe("selection", () => {
  it("maps canvas pixels to view pixels and back through the placement", () => {
    // Magnified 4 device pixels per view pixel, at a device pixel ratio of 2, from (100, 50).
    const p = { origin: [100, 50] as [number, number], scale: 4 };
    expect(toView(p, 2, [20, 10])).toEqual([110, 55]);
    expect(toCanvas(p, 2, [110, 55])).toEqual([20, 10]);
  });

  it("makes a box of a drag, and a click of a short one", () => {
    expect(boxBetween([50, 40], [10, 10])).toEqual({ x: 10, y: 10, w: 40, h: 30 });
    expect(boxBetween([10, 10], [14, 60])).toBeNull();
    expect(clampPoint([-5, 900.4], [640, 480])).toEqual([0, 480]);
  });

  it("moves and scales a box within the view", () => {
    const box = { x: 10, y: 10, w: 100, h: 50 };
    expect(moved(box, -50, 600, [640, 480])).toEqual({ x: 0, y: 430, w: 100, h: 50 });
    expect(scaled(box, 2, [640, 480])).toEqual({ x: 0, y: 0, w: 200, h: 100 });
    expect(scaled(box, 0.01, [640, 480]).w).toBe(8);
  });

  it("picks the smallest block under a point", () => {
    const blocks = [block("text", 0, 0, 200, 200), block("display_formula", 50, 50, 20, 20)];
    expect(blockAt(blocks, [55, 55])).toBe(1);
    expect(blockAt(blocks, [150, 150])).toBe(0);
    expect(blockAt(blocks, [300, 300])).toBeNull();
    expect(role(blocks[1]!.label)).toBe("formula");
    expect(role("paragraph_title")).toBe("text");
  });

  it("outlines a block's quad, or the rectangle's corners", () => {
    expect(outline({ rect: { x: 1, y: 2, w: 3, h: 4 }, quad: null })).toEqual([
      [1, 2],
      [4, 2],
      [4, 6],
      [1, 6],
    ]);
  });
});
