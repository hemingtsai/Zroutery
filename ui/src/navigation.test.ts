import { describe, expect, it } from "vitest";
import { routeForDeepLink, taskFor, TASK_PAGES } from "./navigation";

describe("six task doors", () => {
  it("preserves legacy sub-area addresses", () => {
    expect(TASK_PAGES).toHaveLength(6);
    for (const [link, page] of [
      ["zroutery://accounts", "accounts"], ["zroutery://providers/accounts", "accounts"],
      ["zroutery://routing", "routing"], ["zroutery://models/routing", "routing"],
      ["zroutery://cutover", "cutover"], ["zroutery://takeover/cutover", "cutover"],
    ] as const) expect(routeForDeepLink(link)).toBe(page);
    expect(taskFor("accounts")).toBe("providers");
    expect(taskFor("routing")).toBe("models");
    expect(taskFor("cutover")).toBe("takeover");
  });
  it("does not navigate for unknown or foreign links", () => {
    for (const value of ["https://providers", "zroutery://missing", "not a URL"]) expect(routeForDeepLink(value)).toBeNull();
  });
});
