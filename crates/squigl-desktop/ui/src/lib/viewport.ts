// The magnifier's view state and how keys and the wheel move it. Pure functions,
// so they can be tested without a window.
import type { Rotation } from "./engine";

export interface View {
  centre: [number, number];
  magnification: number;
}

/** Each zoom step multiplies or divides the magnification by this. */
export const ZOOM_FACTOR = 1.25;

export function clampMagnification(m: number, max: number): number {
  return Math.min(Math.max(m, 1), max);
}

/** `steps` zoom steps in (positive) or out, keeping the centre. */
export function zoom(view: View, steps: number, max: number): View {
  return {
    ...view,
    magnification: clampMagnification(view.magnification * ZOOM_FACTOR ** steps, max),
  };
}

/**
 * Moves the centre by a fraction of what is on screen: `dx` of 1 moves it one
 * screen-width right. At 1x nothing moves (the whole view is shown); the centre is
 * kept where the view can still fill the screen.
 */
export function pan(view: View, dx: number, dy: number): View {
  const shown = 1 / view.magnification;
  const clamp = (c: number) => Math.min(Math.max(c, shown / 2), 1 - shown / 2);
  return {
    ...view,
    centre: [clamp(view.centre[0] + dx * shown), clamp(view.centre[1] + dy * shown)],
  };
}

export const ROTATIONS: Rotation[] = ["none", "cw90", "cw180", "cw270"];

export function turn(rotation: Rotation, quarterTurnsCw: number): Rotation {
  const i = ROTATIONS.indexOf(rotation);
  return ROTATIONS[(((i + quarterTurnsCw) % 4) + 4) % 4] as Rotation;
}

/**
 * The texture coordinates of the drawn quad's corners (top-left, top-right,
 * bottom-left, bottom-right on screen) for planes in the frame's own orientation
 * shown turned clockwise by `rotation`.
 */
export function cornerUvs(rotation: Rotation): [number, number][] {
  switch (rotation) {
    case "none":
      return [[0, 0], [1, 0], [0, 1], [1, 1]];
    case "cw90":
      return [[0, 1], [0, 0], [1, 1], [1, 0]];
    case "cw180":
      return [[1, 1], [0, 1], [1, 0], [0, 0]];
    case "cw270":
      return [[1, 0], [1, 1], [0, 0], [0, 1]];
  }
}
