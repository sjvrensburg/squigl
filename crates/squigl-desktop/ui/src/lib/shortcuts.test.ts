import { describe, expect, it } from "vitest";
import { keyLabel, rebind, table } from "./shortcuts";

const defaults = { single_key_shortcuts: true, shortcuts: {} };

describe("shortcuts", () => {
  it("maps the default keys", () => {
    const t = table(defaults);
    expect(t.get(" ")).toBe("freeze");
    expect(t.get("R")).toBe("rotate-ccw");
    expect(t.get("ArrowLeft")).toBe("pan-left");
  });

  it("drops character keys when single-key shortcuts are off, but keeps the arrows", () => {
    const t = table({ ...defaults, single_key_shortcuts: false });
    expect(t.has(" ")).toBe(false);
    expect(t.has("m")).toBe(false);
    expect(t.get("ArrowUp")).toBe("pan-up");
  });

  it("moves a key to another action, which takes it from its old one", () => {
    const shortcuts = rebind(defaults, "freeze", "m");
    expect(shortcuts).toEqual({ freeze: "m", "next-mode": "" });
    const t = table({ ...defaults, shortcuts });
    expect(t.get("m")).toBe("freeze");
    expect(t.has(" ")).toBe(false);
    // Back to its default: not stored.
    const back = rebind({ ...defaults, shortcuts }, "freeze", " ");
    expect(back).toEqual({ "next-mode": "" });
  });

  it("names keys for people", () => {
    expect(keyLabel(" ")).toBe("Space");
    expect(keyLabel("r")).toBe("R");
    expect(keyLabel("R")).toBe("Shift+R");
    expect(keyLabel("ArrowLeft")).toBe("Left arrow");
    expect(keyLabel("+")).toBe("+");
  });
});
