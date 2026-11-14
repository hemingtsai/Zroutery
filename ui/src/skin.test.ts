import { afterEach, describe, expect, it, vi } from "vitest";
import { applySkin, loadSkin, storeSkin } from "./skin";

describe("skin preference", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("defaults safely and persists a new choice for the next shell mount", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    });
    expect(loadSkin()).toBe("industrial");
    storeSkin("org.zroutery.theme.fluent");
    expect(loadSkin()).toBe("org.zroutery.theme.fluent");
    values.set("zroutery-skin", "unknown");
    expect(loadSkin()).toBe("org.zroutery.theme.fluent");
  });

  it("applies the skin to the root element and removes industrial state", () => {
    const root = { dataset: {} as Record<string, string> };
    vi.stubGlobal("document", { documentElement: root, fonts: { add: vi.fn() },
      getElementById: () => null, createElement: () => ({ id: "", textContent: "" }), head: { append: vi.fn() } });
    applySkin("org.zroutery.theme.md3");
    expect(root.dataset.pluginTheme).toBe("org.zroutery.theme.md3");
    applySkin("industrial");
    expect(root.dataset.skin).toBeUndefined();
    expect(root.dataset.pluginTheme).toBeUndefined();
  });
});
