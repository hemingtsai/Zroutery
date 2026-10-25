import { describe, expect, it } from "vitest";
import type { Provider } from "./api";
import { hasChatCredential } from "./providerCredential";

/**
 * A minimal provider; each test states only the fields the rule reads, so a
 * change to the account or key shape fails here rather than passing with a
 * default that happens to match.
 */
function provider(overrides: Partial<Provider> = {}): Provider {
  return {
    id: "relay",
    name: "relay",
    kind: "openai_compatible",
    base_url: "https://relay.example/v1",
    key_ref: "provider:relay",
    enabled: true,
    accounts: [],
    ...overrides,
  } as Provider;
}

const inference = (key_ref: string) => ({
  id: "1",
  key_ref,
  enabled: true,
  priority: 0,
});

describe("hasChatCredential", () => {
  it("counts the provider's own key", () => {
    expect(hasChatCredential(provider(), { relay: true })).toBe(true);
  });

  it("counts a stored inference key on any account", () => {
    const p = provider({
      accounts: [
        {
          account_id: "354394",
          key_ref: "provider:relay:account:354394",
          enabled: true,
          maintenance: {} as never,
          inference_keys: [inference("provider:relay:account:354394:inference:1")],
        },
      ],
    });
    // The panel token is stored too, and on its own would not answer either.
    expect(
      hasChatCredential(p, {
        "provider:relay:account:354394": true,
        "provider:relay:account:354394:inference:1": true,
      }),
    ).toBe(true);
  });

  it("does not count an account's panel token as a chat credential", () => {
    const p = provider({
      accounts: [
        {
          account_id: "354394",
          key_ref: "provider:relay:account:354394",
          enabled: true,
          maintenance: {} as never,
          inference_keys: [],
        },
      ],
    });
    expect(hasChatCredential(p, { "provider:relay:account:354394": true })).toBe(
      false,
    );
  });

  it("treats a provider that declares no reference as complete", () => {
    expect(hasChatCredential(provider({ key_ref: "" }), {})).toBe(true);
  });
});
