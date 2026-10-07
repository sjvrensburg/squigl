// Erasing, as the page makes it: brush strokes in view pixels (the engine paints
// them over the capture before anything reads it, and shows the result), the
// brush's size between the screen and the view, and the strokes that erase a whole
// box -- the keyboard's way to erase, after picking the box with the block keys.
import type { Stroke } from "./engine";
import type { Placement } from "./selection";

/** The smallest and largest brush, in CSS pixels of radius. */
export const BRUSH_MIN = 4;
export const BRUSH_MAX = 60;

/** A brush of `css` CSS pixels' radius in view pixels, at the picture's placement. */
export function brushInView(css: number, placement: Placement, dpr: number): number {
  return (css * dpr) / placement.scale;
}

/** Whether the pointer has moved far enough from the stroke's last point to add one. */
export function farEnough(last: [number, number], p: [number, number], radius: number): boolean {
  return Math.hypot(p[0] - last[0], p[1] - last[1]) >= Math.max(1, radius / 4);
}

/** Where a convex polygon crosses the line at height `y`: its leftmost and rightmost x. */
function across(poly: [number, number][], y: number): [number, number] | null {
  let lo = Infinity;
  let hi = -Infinity;
  poly.forEach((a, i) => {
    const b = poly[(i + 1) % poly.length]!;
    if ((a[1] <= y && y <= b[1]) || (b[1] <= y && y <= a[1])) {
      const x = a[1] === b[1] ? [a[0], b[0]] : [a[0] + ((y - a[1]) / (b[1] - a[1])) * (b[0] - a[0])];
      for (const v of x) {
        lo = Math.min(lo, v);
        hi = Math.max(hi, v);
      }
    }
  });
  return lo <= hi ? [lo, hi] : null;
}

/**
 * Strokes covering a convex polygon (a box's corners, clockwise), in rows no taller
 * than `cell`: each row a line along its middle with the radius that reaches its
 * corners, so it spills past the polygon by at most a fifth of a row.
 */
export function coverStrokes(poly: [number, number][], cell = 12): Stroke[] {
  const ys = poly.map((p) => p[1]);
  const top = Math.min(...ys);
  const bottom = Math.max(...ys);
  if (!(bottom > top)) return [];
  const rows = Math.ceil((bottom - top) / cell);
  const s = (bottom - top) / rows;
  const strokes: Stroke[] = [];
  for (let k = 0; k < rows; k++) {
    const y0 = top + k * s;
    const y = y0 + s / 2;
    // The row's widest extent, from its top, middle and bottom edges.
    let lo = Infinity;
    let hi = -Infinity;
    for (const at of [y0 + 1e-3, y, y0 + s - 1e-3]) {
      const r = across(poly, at);
      if (r) {
        lo = Math.min(lo, r[0]);
        hi = Math.max(hi, r[1]);
      }
    }
    if (!(hi > lo)) continue;
    if (hi - lo <= s) {
      strokes.push({ points: [[(lo + hi) / 2, y]], radius: Math.hypot((hi - lo) / 2, s / 2) });
    } else {
      strokes.push({
        points: [
          [lo + s / 2, y],
          [hi - s / 2, y],
        ],
        radius: s / Math.SQRT2,
      });
    }
  }
  return strokes;
}
