// The selection on the picture, in view pixels (the frame as rotated), and the
// mapping between those and the canvas: a frame header's placement puts view pixel
// p at canvas device pixel (p - origin) * scale. The engine keeps the selection;
// these are the gestures' arithmetic.
import type { Block, Crop, Quad, Selection } from "./engine";

export interface Placement {
  origin: [number, number];
  scale: number;
}

/** A CSS pixel on the canvas (from its top-left) as a view point, unclamped. */
export function toView(p: Placement, dpr: number, [x, y]: [number, number]): [number, number] {
  return [p.origin[0] + (x * dpr) / p.scale, p.origin[1] + (y * dpr) / p.scale];
}

/** A view point as CSS pixels on the canvas. */
export function toCanvas(p: Placement, dpr: number, [x, y]: [number, number]): [number, number] {
  return [((x - p.origin[0]) * p.scale) / dpr, ((y - p.origin[1]) * p.scale) / dpr];
}

/** A point kept within a `view`-sized frame, in whole pixels. */
export function clampPoint([x, y]: [number, number], [vw, vh]: [number, number]): [number, number] {
  return [Math.round(Math.min(Math.max(x, 0), vw)), Math.round(Math.min(Math.max(y, 0), vh))];
}

/** Drags shorter than this (view pixels, either way) are a click, not a box. */
export const MIN_BOX = 8;

/** The box between two view points, or null if it is too small to be one. */
export function boxBetween(a: [number, number], b: [number, number]): Crop | null {
  const [x0, x1] = [Math.min(a[0], b[0]), Math.max(a[0], b[0])];
  const [y0, y1] = [Math.min(a[1], b[1]), Math.max(a[1], b[1])];
  if (x1 - x0 < MIN_BOX || y1 - y0 < MIN_BOX) return null;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/** `box` moved by (dx, dy), kept inside the view. */
export function moved(box: Crop, dx: number, dy: number, [vw, vh]: [number, number]): Crop {
  const x = Math.min(Math.max(box.x + Math.round(dx), 0), Math.max(vw - box.w, 0));
  const y = Math.min(Math.max(box.y + Math.round(dy), 0), Math.max(vh - box.h, 0));
  return { ...box, x, y };
}

/** `box` scaled about its centre by `factor`, kept inside the view and at least MIN_BOX. */
export function scaled(box: Crop, factor: number, [vw, vh]: [number, number]): Crop {
  const w = Math.min(Math.max(Math.round(box.w * factor), MIN_BOX), vw);
  const h = Math.min(Math.max(Math.round(box.h * factor), MIN_BOX), vh);
  const cx = box.x + box.w / 2;
  const cy = box.y + box.h / 2;
  return moved({ x: Math.round(cx - w / 2), y: Math.round(cy - h / 2), w, h }, 0, 0, [vw, vh]);
}

/** A rectangle's corners as a quad, clockwise from the top-left. */
export function corners(r: Crop): Quad {
  return [
    [r.x, r.y],
    [r.x + r.w, r.y],
    [r.x + r.w, r.y + r.h],
    [r.x, r.y + r.h],
  ];
}

/** The selection's outline: its quad if it has one, else its rectangle's corners. */
export function outline(s: Selection): Quad {
  return s.quad ?? corners(s.rect);
}

export function contains(r: Crop, [x, y]: [number, number]): boolean {
  return x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
}

/** The smallest block under a view point, as the engine picks one. */
export function blockAt(blocks: Block[], at: [number, number]): number | null {
  let best: number | null = null;
  blocks.forEach((b, i) => {
    if (!contains(b.rect, at)) return;
    if (best === null || b.rect.w * b.rect.h < blocks[best]!.rect.w * blocks[best]!.rect.h) best = i;
  });
  return best;
}

/** A block's kind as the engine folds it: what colour and words it gets. */
export function role(label: string): "text" | "formula" | "figure" | "other" {
  if (label === "display_formula" || label === "inline_formula") return "formula";
  if (["image", "chart", "table", "seal", "header_image", "footer_image"].includes(label)) return "figure";
  if (["number", "formula_number", "header", "footer"].includes(label)) return "other";
  return "text";
}
