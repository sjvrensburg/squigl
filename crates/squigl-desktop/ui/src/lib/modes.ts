// The display modes, named for people (squigl_engine::display::DisplayMode).
import type { DisplayMode } from "./engine";

export const MODES: [DisplayMode, string][] = [
  ["normal", "Normal colours"],
  ["grey", "Black on white"],
  ["inverted", "Inverted colours"],
  ["yellow-on-black", "Yellow on black"],
  ["white-on-black", "White on black"],
  ["black-on-yellow", "Black on yellow"],
  ["custom", "My own colours"],
];

export function modeLabel(mode: DisplayMode): string {
  return MODES.find(([m]) => m === mode)?.[1] ?? mode;
}
