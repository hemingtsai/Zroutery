import { describe, expect, it } from "vitest";
import { en, type TKey } from "./i18n/en";
import { zh } from "./i18n/zh";
import { zhTW } from "./i18n.zhTW";

/*
 * en may carry ICU-style plurals ("... | ...") and the CJK dictionaries
 * legally keep a single branch, so the parity contract is about the variables
 * a renderer can interpolate, not the branch structure: the union of {name}
 * placeholders across branches must be identical in all three dictionaries.
 * A missing placeholder renders the raw "{name}" — interpolate only
 * substitutes what is present — and an extra one silently stays literal too,
 * so either side of the drift is a visible product bug, not a style issue.
 */
const PLACEHOLDER = /\{[a-z_0-9]+\}/g;

function variables(value: string): string[] {
  return [...new Set(value.split("|").flatMap((branch) => branch.match(PLACEHOLDER) ?? []))].sort();
}

describe("i18n placeholder parity", () => {
  it("zh and zhTW interpolate exactly the variables en does", () => {
    const drifted: string[] = [];
    for (const key of Object.keys(en) as TKey[]) {
      const expected = variables(en[key]);
      for (const [name, dict] of [
        ["zh", zh],
        ["zhTW", zhTW],
      ] as const) {
        const actual = variables(dict[key]);
        if (actual.join(",") !== expected.join(",")) {
          drifted.push(`${name}:${key} (${expected.join(" ") || "none"} vs ${actual.join(" ") || "none"})`);
        }
      }
    }
    expect(drifted).toEqual([]);
  });
});
