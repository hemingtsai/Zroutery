import type { Provider } from "./api";

/**
 * The provider list, with auxiliary decision endpoints nested under the vendor
 * they answer for.
 *
 * A decision provider is its own configuration entry — the router keeps a
 * separate pool for it — but it is not a separate *place*: the Zen decision
 * preset answers for the same vendor as the chat provider beside it, and a row
 * at the bottom of the list named after a provider already in it reads as a
 * duplicate.
 */
export type ProviderGroups = {
  /** Providers that own a row of their own. */
  top: Provider[];
  /** Decision providers by the id of the row they render under. */
  nested: Map<string, Provider[]>;
};

/**
 * The address a provider's base URL names, for grouping.
 *
 * The host rather than the full URL: the free Zen provider moved to
 * `https://opencode.ai/inference/openai/v1` while the decision endpoint stayed
 * at `https://opencode.ai/zen`, so comparing endpoints would break the pairing
 * the moment either side moved. The port stays part of the key, because two
 * relays on one machine are two vendors.
 */
function address(url: string): string {
  const trimmed = url.trim().replace(/\/+$/, "");
  if (trimmed === "") return "";
  try {
    return new URL(trimmed).host.toLowerCase();
  } catch {
    // A base URL the parser rejects (a bare host name, a template) still groups
    // with itself, which is better than every such provider becoming top-level.
    return trimmed.toLowerCase();
  }
}

export function groupProviders(providers: Provider[]): ProviderGroups {
  const hostOf = new Map<string, string>();
  for (const provider of providers) {
    const host = address(provider.base_url);
    if (provider.kind !== "decision" && host !== "" && !hostOf.has(host)) {
      hostOf.set(host, provider.id);
    }
  }

  const top: Provider[] = [];
  const nested = new Map<string, Provider[]>();
  for (const provider of providers) {
    if (provider.kind === "decision") {
      const host = hostOf.get(address(provider.base_url));
      if (host !== undefined) {
        const bucket = nested.get(host) ?? [];
        bucket.push(provider);
        nested.set(host, bucket);
        continue;
      }
    }
    top.push(provider);
  }
  return { top, nested };
}
