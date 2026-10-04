import { describe, expect, it } from "vitest";
import { cornerUvs, pan, turn, zoom } from "./viewport";
import { parseFrame } from "./frames";

describe("zoom", () => {
  it("steps by a factor and stays within 1x and the maximum", () => {
    const v = { centre: [0.5, 0.5] as [number, number], magnification: 1 };
    expect(zoom(v, 1, 30).magnification).toBeCloseTo(1.25);
    expect(zoom(v, -3, 30).magnification).toBe(1);
    expect(zoom({ ...v, magnification: 28 }, 2, 30).magnification).toBe(30);
  });
});

describe("pan", () => {
  it("moves by a fraction of the screen and keeps the view filling it", () => {
    const v = { centre: [0.5, 0.5] as [number, number], magnification: 4 };
    // One screen at 4x is a quarter of the view.
    expect(pan(v, 1, 0).centre[0]).toBeCloseTo(0.75);
    // Not past where the right edge of the view meets the right of the screen.
    expect(pan(v, 10, 0).centre[0]).toBeCloseTo(0.875);
    expect(pan(v, 0, -10).centre[1]).toBeCloseTo(0.125);
    // At 1x the whole view is shown; there is nowhere to go.
    expect(pan({ ...v, magnification: 1 }, 1, 1).centre).toEqual([0.5, 0.5]);
  });
});

describe("rotation", () => {
  it("turns both ways round", () => {
    expect(turn("none", 1)).toBe("cw90");
    expect(turn("none", -1)).toBe("cw270");
    expect(turn("cw270", 1)).toBe("none");
  });

  it("puts the frame's top-left corner where the engine's rotation does", () => {
    // squigl_engine's render tests: clockwise, the source's top-left is the
    // view's top-right; counter-clockwise, its bottom-left.
    const topLeft = (r: Parameters<typeof cornerUvs>[0]) =>
      cornerUvs(r).findIndex(([u, v]) => u === 0 && v === 0);
    expect(topLeft("none")).toBe(0);
    expect(topLeft("cw90")).toBe(1);
    expect(topLeft("cw180")).toBe(3);
    expect(topLeft("cw270")).toBe(2);
  });
});

describe("parseFrame", () => {
  it("splits the header and the planes", () => {
    const header = {
      seq: 7, source_width: 4, source_height: 2, region: { x: 0, y: 0, w: 4, h: 2 },
      step: 1, rotation: "none", format: "yuv420", width: 3, height: 2,
      view_region: { x: 0, y: 0, w: 3, h: 2 }, view: [4, 2],
      placement: { origin: [0, 0], scale: 1 },
    };
    const json = new TextEncoder().encode(JSON.stringify(header));
    // 3x2 luma, then 2x1 chroma each.
    const planes = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    const buf = new Uint8Array(4 + json.length + planes.length);
    new DataView(buf.buffer).setUint32(0, json.length, true);
    buf.set(json, 4);
    buf.set(planes, 4 + json.length);
    const f = parseFrame(buf.buffer);
    expect(f.header.seq).toBe(7);
    expect([...f.y]).toEqual([1, 2, 3, 4, 5, 6]);
    expect([...f.u]).toEqual([7, 8]);
    expect([...f.v]).toEqual([9, 10]);
  });
});
