import { describe, expect, it } from "vitest";
import { applySystemAccent, SYSTEM_ACCENT_PROPERTIES } from "./accent";
import type { SystemAccent } from "./api";

function styleMock() {
  const values = new Map<string, string>();
  return {
    values,
    setProperty: (name: string, value: string): void => {
      values.set(name, value);
    },
    removeProperty: (name: string): string => {
      const previous = values.get(name) ?? "";
      values.delete(name);
      return previous;
    },
  };
}

describe("system accent CSS tokens", () => {
  it("publishes the native accent roles", () => {
    const style = styleMock();
    const accent: SystemAccent = {
      accent: "#0078d4",
      accent_soft: "rgba(0, 120, 212, 0.16)",
      accent_fg: "#ffffff",
    };
    applySystemAccent(accent, style);
    expect(style.values.get("--system-accent")).toBe(accent.accent);
    expect(style.values.get("--system-accent-soft")).toBe(accent.accent_soft);
    expect(style.values.get("--system-accent-fg")).toBe(accent.accent_fg);
  });

  it("removes native roles when the platform has no accent", () => {
    const style = styleMock();
    for (const property of SYSTEM_ACCENT_PROPERTIES) style.setProperty(property, "stale");
    applySystemAccent(null, style);
    expect([...style.values.keys()]).toEqual([]);
  });
});
