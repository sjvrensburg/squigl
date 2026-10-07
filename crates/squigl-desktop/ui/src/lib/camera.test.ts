import { describe, expect, it } from "vitest";
import { SLIDER_STEPS, fromSlider, toSlider, zoomBounds } from "./camera";
import type { StreamSlice } from "./engine";

const slice = (over: Partial<StreamSlice>): StreamSlice => ({
  source: { kind: "network" },
  status: { state: "streaming", width: 640, height: 480 },
  capabilities: { zoom: true, torch: true, facing: false },
  facing: "back",
  zoom: 1,
  zoom_applied: 1,
  zoom_range: [1, 8],
  torch: false,
  ...over,
});

describe("camera zoom", () => {
  it("offers a range only when the camera has one", () => {
    expect(zoomBounds(slice({}))).toEqual([1, 8]);
    expect(zoomBounds(null)).toBeNull();
    expect(zoomBounds(slice({ capabilities: { zoom: false, torch: true, facing: false } }))).toBeNull();
    expect(zoomBounds(slice({ zoom_range: null }))).toBeNull();
    expect(zoomBounds(slice({ zoom_range: [1, 1] }))).toBeNull();
  });

  it("starts the ADB phone's at 1x, and a paired phone's where it says", () => {
    expect(zoomBounds(slice({ zoom_range: [0.6, 10] }))).toEqual([0.6, 10]);
    expect(zoomBounds(slice({ source: { kind: "phone" }, zoom_range: [0.6, 10] }))).toEqual([1, 10]);
  });

  it("moves on a logarithmic scale, from end to end exactly", () => {
    const range: [number, number] = [1, 16];
    expect(toSlider(1, range)).toBe(0);
    expect(toSlider(16, range)).toBe(SLIDER_STEPS);
    // Halfway along 1-16 is 4x.
    expect(toSlider(4, range)).toBeCloseTo(SLIDER_STEPS / 2);
    expect(fromSlider(SLIDER_STEPS / 2, range)).toBe(4);
    expect(fromSlider(SLIDER_STEPS, [1, 8])).toBe(8);
    expect(fromSlider(0, [0.6, 8])).toBe(0.6);
    expect(fromSlider(toSlider(2.37, [1, 8]), [1, 8])).toBe(2.37);
    // A zoom outside the range sits at an end.
    expect(toSlider(50, [1, 8])).toBe(SLIDER_STEPS);
  });
});
