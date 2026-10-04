import { describe, expect, it } from "vitest";
import { PROBLEMS } from "./engine";
import { guidance } from "./guidance";

describe("guidance", () => {
  it("has a title and steps for every problem the engine reports", () => {
    for (const p of PROBLEMS) {
      for (const platform of ["linux", "windows", "mac"] as const) {
        const g = guidance(p, platform);
        expect(g.title.length, p).toBeGreaterThan(0);
        expect(g.steps.length, p).toBeGreaterThan(0);
      }
    }
  });

  it("tells each platform where adb comes from", () => {
    expect(guidance("adb-missing", "linux").steps[0]).toContain("android-tools");
    expect(guidance("adb-missing", "windows").steps[0]).toContain("Platform-Tools");
    expect(guidance("adb-missing", "windows").steps[1]).toContain("SQUIGL_ADB");
  });
});
