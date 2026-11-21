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

  it("a disabled recipe takes over the host's disabled fade with full opacity", () => {
    const theme = structuredClone(base);
    (theme.modes.light.recipes as unknown[]).push({
      part: "button",
      when: { interaction: "rest", disabled: true },
      fill: { role: "text" },
      foreground: { role: "bg" },
    });
    const css = compileTheme(theme);
    expect(css).toContain(".btn:disabled:not([data-host-security]");
    expect(css).toContain("opacity:1");
  });

  it("tonal roles ride a baseline literal under the accent-derived relative color", () => {
    const css = compileTheme(md3);
    expect(css).toContain(
      "--primary-container:#eaddff;--primary-container:oklch(from var(--system-accent,#eaddff) 0.918 min(c,0.048) h)",
    );
    expect(css).toContain(
      "--on-secondary-container:#1d192b;--on-secondary-container:oklch(from var(--system-accent,#1d192b) 0.227 min(c,0.035) h)",
    );
    for (const mutate of [
      (colors: Record<string, unknown>) => { colors["primary-container"] = { tonal: { of: "hue", lightness: 0.9, chroma: 0.05, fallback: { hex: "#eaddff" } } }; },
      (colors: Record<string, unknown>) => { colors["primary-container"] = { tonal: { of: "accent", lightness: 1.2, chroma: 0.05, fallback: { hex: "#eaddff" } } }; },
      (colors: Record<string, unknown>) => { colors["primary-container"] = { tonal: { of: "accent", lightness: 0.9, chroma: 0.5, fallback: { hex: "#eaddff" } } }; },
      (colors: Record<string, unknown>) => { colors["primary-container"] = { tonal: { of: "accent", lightness: 0.9, chroma: 0.05 } }; },
    ]) {
      const theme = structuredClone(md3);
      mutate(theme.modes.light.colors as Record<string, unknown>);
      expect(() => compileTheme(theme)).toThrow("invalid_theme");
    }
  });

  it("retunes the host motion slots from the closed curve set", () => {
    const css = compileTheme(md3);
    expect(css).toContain(
      "--t-fast:150ms;--t-page:250ms;--t-drawer:300ms;--ease:cubic-bezier(0.2,0,0,1)",
    );
    for (const bad of [
      { fast_ms: 10, page_ms: 250, drawer_ms: 300, ease: "standard" },
      { fast_ms: 150, page_ms: 250, drawer_ms: 700, ease: "standard" },
      { fast_ms: 150, page_ms: 250, drawer_ms: 300, ease: "spring" },
    ]) {
      const broken = structuredClone(md3) as Record<string, unknown>;
      broken.motion = bad;
      expect(() => compileTheme(broken)).toThrow("invalid_theme");
    }
  });

  it("the nav icon slot turns on only where a skin styles it", () => {
    for (const theme of [md3, fluent]) {
      const css = compileTheme(theme);
      expect(css).toContain(".nav-item .nav-icon:not([data-host-security]");
      expect(css).toContain("display:inline-flex");
    }
  });

  it("the md3 rail stacks the nav item and wraps the icon slot in the indicator", () => {
    const css = compileTheme(md3);
    expect(css).toContain("display:inline-flex;flex-direction:column;align-items:center;text-align:center;gap:6px");
    // The active pill lands on the icon slot via the ancestor's state.
    expect(css).toContain(".nav-item.active .nav-icon:not(");
    expect(css).toContain("background:var(--secondary-container)");
    // The count badge loses its horizontal push under the rail.
    expect(css).toContain(".nav-item:not([data-host-security],[data-host-security] *) .nav-count{margin:0}");
    // Fluent keeps the horizontal drawer morphology: no rail layout.
    expect(compileTheme(fluent)).not.toContain("flex-direction:column;align-items:center;text-align:center");
  });

  it("the nav indicator morph is pack-declared, not host-assumed", () => {
    const md3Css = compileTheme(md3);
    expect(md3Css).toContain("--nav-indicator-morph:1");
    expect(md3Css).toContain(".nav-item.active:not([data-host-security],[data-host-security] *) .nav-icon{view-transition-name:nav-indicator}");
    expect(compileTheme(fluent)).not.toContain("--nav-indicator-morph");
    const broken = structuredClone(md3);
    broken.modes.light.recipes[0].decoration = "nav_indicator_morph";
    expect(() => compileTheme(broken)).toThrow("invalid_theme");
  });

  it("rejects the rail layout outside the navigation item", () => {
    const broken = structuredClone(md3);
    broken.modes.light.recipes[0].layout = "nav_rail";
    expect(() => compileTheme(broken)).toThrow("invalid_theme");
  });
});
