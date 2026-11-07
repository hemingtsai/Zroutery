import { afterEach, describe, expect, it, vi } from "vitest";
import { applyTheme, loadThemePref, resolveTheme, storeThemePref } from "./theme";

describe("theme preference", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("persists explicit choices for a later shell mount", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    });
    expect(loadThemePref()).toBe("system");
    storeThemePref("dark");
    expect(loadThemePref()).toBe("dark");
    storeThemePref("light");
    expect(loadThemePref()).toBe("light");
  });

  it("resolves and applies the persisted system preference", () => {
    const root = { dataset: {} as Record<string, string> };
    vi.stubGlobal("document", { documentElement: root });
    vi.stubGlobal("window", { matchMedia: () => ({ matches: true }) });
    expect(resolveTheme("system")).toBe("dark");
    applyTheme("light");
    expect(root.dataset.theme).toBe("light");
  });
});
