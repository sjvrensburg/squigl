// The magnifier's keyboard shortcuts: the actions, their default keys, and the
// table the settings make of them. The settings file keeps only keys moved from
// their defaults ([desktop].shortcuts) and whether single-key shortcuts are on at
// all (WCAG 2.1.4: they can be turned off).

export type Action =
  | "freeze"
  | "rotate-cw"
  | "rotate-ccw"
  | "zoom-in"
  | "zoom-out"
  | "zoom-reset"
  | "pan-left"
  | "pan-right"
  | "pan-up"
  | "pan-down"
  | "next-mode"
  | "reading-line"
  | "camera-zoom-in"
  | "camera-zoom-out"
  | "torch"
  | "fullscreen"
  | "settings";

export const ACTIONS: { action: Action; label: string; key: string }[] = [
  { action: "freeze", label: "Freeze or go live", key: " " },
  { action: "rotate-cw", label: "Rotate right", key: "r" },
  { action: "rotate-ccw", label: "Rotate left", key: "R" },
  { action: "zoom-in", label: "Magnify more", key: "+" },
  { action: "zoom-out", label: "Magnify less", key: "-" },
  { action: "zoom-reset", label: "Whole picture", key: "0" },
  { action: "pan-left", label: "Move left", key: "ArrowLeft" },
  { action: "pan-right", label: "Move right", key: "ArrowRight" },
  { action: "pan-up", label: "Move up", key: "ArrowUp" },
  { action: "pan-down", label: "Move down", key: "ArrowDown" },
  { action: "next-mode", label: "Next colours", key: "m" },
  { action: "reading-line", label: "Reading line", key: "l" },
  { action: "camera-zoom-in", label: "Camera zoom in", key: "]" },
  { action: "camera-zoom-out", label: "Camera zoom out", key: "[" },
  { action: "torch", label: "Torch on or off", key: "t" },
  { action: "fullscreen", label: "Full screen", key: "f" },
  { action: "settings", label: "Settings", key: "," },
];

/** Whether a key is a printing character, which single-key shortcuts may switch off. */
export function isCharacterKey(key: string): boolean {
  return [...key].length === 1;
}

/** How a key is shown: "Space" for " ", else as it is. */
export function keyLabel(key: string): string {
  if (key === " ") return "Space";
  if (key.startsWith("Arrow")) return key.slice(5) + " arrow";
  if (isCharacterKey(key)) {
    // A capital letter is the letter with Shift.
    return key !== key.toLowerCase() ? `Shift+${key}` : key.toUpperCase();
  }
  return key;
}

export interface ShortcutSettings {
  single_key_shortcuts: boolean;
  shortcuts: Record<string, string>;
}

/** Key to action, with the overrides applied; character keys left out when they are off. */
export function table(settings: ShortcutSettings): Map<string, Action> {
  const keys = new Map<string, Action>();
  for (const { action, key } of ACTIONS) {
    const k = settings.shortcuts[action] ?? key;
    if (!settings.single_key_shortcuts && isCharacterKey(k)) continue;
    keys.set(k, action);
  }
  return keys;
}

/** The key each action has now (after overrides). */
export function keyFor(action: Action, settings: ShortcutSettings): string {
  return settings.shortcuts[action] ?? ACTIONS.find((a) => a.action === action)!.key;
}

/** The overrides after giving `action` the key `key`: the key leaves any other action, and a key back at its default is not stored. */
export function rebind(settings: ShortcutSettings, action: Action, key: string): Record<string, string> {
  const next: Record<string, string> = {};
  for (const a of ACTIONS) {
    let k = a.action === action ? key : keyFor(a.action, settings);
    if (a.action !== action && k === key) k = ""; // taken: the other action loses it
    if (k !== a.key) next[a.action] = k;
  }
  return next;
}
