import { describe, expect, it } from "vitest";
import md3 from "../../../plugins/theme-md3/theme.json";
import fluent from "../../../plugins/theme-fluent/theme.json";
import { compileTheme } from "./compiler";
import { loadThemeRef } from "./preferences";

describe("data-only theme boundary", () => {
  it("compiles both full offline themes through the same renderer", () => {
    for (const theme of [md3, fluent]) {
      const css = compileTheme(theme);
      expect(css).toContain(".toggle input[type=checkbox]");
      expect(css).toContain(".section");
      expect(css).toContain("[data-theme=dark]");
      expect(css).not.toMatch(/data-skin|url\(|@import|md3|fluent/);
      expect(css).toContain(":not([data-host-security]");
    }
    expect(compileTheme(md3)).toContain(":hover:checked");
  });
  it("rejects markup, selector, layout and color escape hatches", () => {
    for (const mutate of [
      (theme: Record<string, unknown>) => { theme.css = "body {display:none}"; },
      (theme: Record<string, unknown>) => { theme.name = { en: "x", "zh-CN": "x" }; },
    ]) {
      const theme = structuredClone(md3) as Record<string, unknown>;
      mutate(theme);
      expect(() => compileTheme(theme)).toThrow("invalid_theme");
    }
    const theme = structuredClone(md3);
    theme.modes.light.recipes[0].part = "body";
    expect(() => compileTheme(theme)).toThrow("invalid_theme");
    const cycle = structuredClone(md3);
    (cycle.modes.light.colors as Record<string, unknown>).primary = { role: "primary" };
    expect(() => compileTheme(cycle)).toThrow("invalid_theme");
  });
  it("migrates legacy skin exactly once without modifying the light/dark axis", () => {
    const values = new Map([["zroutery-skin", "md3"], ["zroutery-theme", "dark"]]);
    const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
    expect(loadThemeRef(storage)).toBe("org.zroutery.theme.md3");
    values.set("zroutery-skin", "fluent");
    expect(loadThemeRef(storage)).toBe("org.zroutery.theme.md3");
    expect(values.get("zroutery-theme")).toBe("dark");
  });
});
