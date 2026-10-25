import type { Provider } from "./api";

/**
 * Whether any credential that can actually reach a model exists.
 *
 * The provider row's dot and the drawer's status used to answer only for
 * `provider.key_ref`, so a provider whose chat runs on an account's inference
 * key showed the yellow "no key" dot while chat worked: the account's key is a
 * chat credential and was invisible to that check. An account's *panel* token
 * is deliberately not counted — it probes balances and signs in, and is never
 * sent upstream as chat traffic.
 *
 * An empty reference means the provider declares no credential at all (a free
 * gateway), which is answered here so callers do not repeat the check.
 */
export function hasChatCredential(
  provider: Provider,
  keys: Record<string, boolean>,
): boolean {
  if (keys[provider.id] || provider.key_ref === "") return true;
  return (provider.accounts ?? []).some((account) =>
    account.inference_keys.some((key) => keys[key.key_ref] ?? false),
  );
}
