// The camera's own zoom, as the toolbar's slider moves it: on a logarithmic scale,
// since ranges run from 8x to 100x and each step should look like the same change.

import type { StreamSlice } from "./engine";

/** The zoom range a slider offers, if the camera has one worth a slider. */
export function zoomBounds(stream: StreamSlice | null): [number, number] | null {
  if (!stream?.capabilities.zoom || !stream.zoom_range) return null;
  let [lo, hi] = stream.zoom_range;
  // The ADB phone's zoom steps up from 1x.
  if (stream.source.kind === "phone") lo = Math.max(lo, 1);
  return hi > lo ? [lo, hi] : null;
}

/** The slider's positions, end to end: an arrow key moves one. */
export const SLIDER_STEPS = 100;

/** The slider position (0 to SLIDER_STEPS) for a zoom ratio. */
export function toSlider(zoom: number, [lo, hi]: [number, number]): number {
  const at = (Math.log(zoom) - Math.log(lo)) / (Math.log(hi) - Math.log(lo));
  return Math.min(SLIDER_STEPS, Math.max(0, at * SLIDER_STEPS));
}

/** The zoom ratio at a slider position, to hundredths, kept within the range. */
export function fromSlider(position: number, [lo, hi]: [number, number]): number {
  const at = position / SLIDER_STEPS;
  const zoom = Math.round(Math.exp(Math.log(lo) + at * (Math.log(hi) - Math.log(lo))) * 100) / 100;
  return Math.min(hi, Math.max(lo, zoom));
}
