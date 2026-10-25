import { describe, expect, it } from "vitest";
import type { Provider } from "./api";
import { groupProviders } from "./providerGroups";

/** The grouping reads three fields; the rest of the declaration is irrelevant. */
function provider(id: string, kind: Provider["kind"], base_url: string): Provider {
  return { id, kind, base_url } as unknown as Provider;
}

function ids(providers: Provider[]): string[] {
  return providers.map((p) => p.id);
}

describe("provider grouping", () => {
  it("nests the zen decision preset under the free provider on the same host", () => {
    // The regression this exists for: the two endpoints differ
    // (`/inference/openai/v1` and `/zen`), and matching the whole URL left the
    // decision row at the bottom of the list named after the provider above it.
    const zen = provider("opencode-zen", "opencode_zen_free", "https://opencode.ai/inference/openai/v1");
    const jev = provider("opencode-zen-jev", "decision", "https://opencode.ai/zen");

    const grouped = groupProviders([zen, jev]);

    expect(ids(grouped.top)).toEqual(["opencode-zen"]);
    expect(ids(grouped.nested.get("opencode-zen") ?? [])).toEqual(["opencode-zen-jev"]);
  });

  it("leaves a decision provider alone when no chat provider shares its host", () => {
    const relay = provider("relay", "openai_compatible", "https://relay.example/v1");
    const jev = provider("jev", "decision", "https://opencode.ai/zen");

    const grouped = groupProviders([relay, jev]);

    expect(ids(grouped.top)).toEqual(["relay", "jev"]);
    expect(grouped.nested.size).toBe(0);
  });

  it("still nests when the decision provider is declared first", () => {
    const jev = provider("jev", "decision", "https://opencode.ai/zen");
    const zen = provider("zen", "openai_compatible", "https://opencode.ai/v1");

    const grouped = groupProviders([jev, zen]);

    expect(ids(grouped.top)).toEqual(["zen"]);
    expect(ids(grouped.nested.get("zen") ?? [])).toEqual(["jev"]);
  });

  it("keeps the port in the key, because two relays on one machine are two vendors", () => {
    const near = provider("near", "openai_compatible", "http://127.0.0.1:7863/v1");
    const far = provider("far", "decision", "http://127.0.0.1:7864/systemone");

    const grouped = groupProviders([near, far]);

    expect(ids(grouped.top)).toEqual(["near", "far"]);
  });

  it("groups under the first chat provider on a host and never under a decision one", () => {
    const first = provider("first", "openai_compatible", "https://vendor.example/a");
    const second = provider("second", "anthropic", "https://vendor.example/b");
    const one = provider("one", "decision", "https://vendor.example/d1");
    const two = provider("two", "decision", "https://vendor.example/d2");

    const grouped = groupProviders([first, second, one, two]);

    expect(ids(grouped.top)).toEqual(["first", "second"]);
    expect(ids(grouped.nested.get("first") ?? [])).toEqual(["one", "two"]);
  });

  it("does not nest a decision provider that has no base url", () => {
    const zen = provider("zen", "opencode_zen_free", "https://opencode.ai/v1");
    const blank = provider("blank", "decision", "   ");

    const grouped = groupProviders([zen, blank]);

    expect(ids(grouped.top)).toEqual(["zen", "blank"]);
  });
});
