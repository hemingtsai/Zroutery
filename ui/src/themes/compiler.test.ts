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

describe("v1.1 recipe surface", () => {
  const base = structuredClone(md3);
  (base.modes.light.recipes as unknown[]) = [];

  it("compiles the Fluent elevation gradient as a typed border composite", () => {
    const css = compileTheme(fluent);
    // The hover recipe fills with the padding-box layer and rings the element
    // with the xaml ControlElevationBorderBrush gradient in the border-box.
    expect(css).toContain(
      "background-image:linear-gradient(var(--hover),var(--hover)),linear-gradient(180deg,#00000029,#0000000f)",
    );
    expect(css).toContain("background-clip:padding-box,border-box");
    expect(css).toContain("border:1px solid transparent");
  });

  it("rejects a gradient without a fill and stroke plus gradient together", () => {
    const withGradient = structuredClone(base);
    (withGradient.modes.light.recipes as unknown[]).push({
      part: "button",
      when: { interaction: "hover" },
      stroke_gradient: { width: 1, from: { hex: "#000000" }, to: { hex: "#ffffff" } },
    });
    expect(() => compileTheme(withGradient)).toThrow("invalid_theme");
    const both = structuredClone(base);
    (both.modes.light.recipes as unknown[]).push({
      part: "button",
      when: { interaction: "hover" },
      fill: { hex: "#ffffff" },
      stroke: { width: 1, color: { hex: "#000000" } },
      stroke_gradient: { width: 1, from: { hex: "#000000" }, to: { hex: "#ffffff" } },
    });
    expect(() => compileTheme(both)).toThrow("invalid_theme");
  });

  it("puts a menu radio's checked state on the item, not its dot", () => {
    const theme = structuredClone(base);
    (theme.modes.light.recipes as unknown[]).push({
      part: "menu.radio",
      when: { interaction: "rest", checked: true },
      fill: { role: "primary" },
    });
    expect(compileTheme(theme)).toContain(
      ".menu-item[aria-checked=true] .menu-radio",
    );
  });

  it("covers Radix keyboard highlights with the menu hover recipes", () => {
    const theme = structuredClone(base);
    (theme.modes.light.recipes as unknown[]).push({
      part: "menu.item",
      when: { interaction: "hover" },
      fill: { role: "hover" },
    });
    const css = compileTheme(theme);
    expect(css).toContain(".menu-item:hover");
    expect(css).toContain(".menu-item[data-highlighted]");
  });
});
