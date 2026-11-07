/** The two balance predicates the page and the drawer share. */
import type { Provider, Snapshot } from "../../api";

/**
 * Whether this provider has a balance to ask for.
 *
 * Two sources, and a provider may have either or both:
 *
 * - It **publishes** a balance of its own, at a relay endpoint the probe
 *   names — DeepSeek's `/user/balance`, OpenRouter's `/credits`, and the
 *   rest of that family.
 * - It **declares accounts**, and the panel those accounts are users of holds
 *   the wallet. A NewAPI relay serves `/v1` and publishes no balance there,
 *   while the panel that owns the balance publishes one — to the **user's
 *   access token**, which is the credential an account carries rather than the
 *   relay key the provider carries. Reading it needs the account subsystem, so
 *   the second source only counts in a build that has one: offering a button
 *   that cannot work is the failure this avoids.
 *
 * The two are asked through the same command, so a provider with both is
 * answered by its own endpoint and the panel is the fallback.
 */
export function hasBalanceSource(p: Provider, snapshot: Snapshot): boolean {
  if (p.balance.preset !== "none") return true;
  return hasPanelBalance(p, snapshot);
}

/**
 * Whether this provider's balance comes from the panel rather than the relay.
 *
 * Split out from [`hasBalanceSource`] because the two answer different
 * questions: one asks "is there anything to ask for", the other asks "where
 * would the answer come from". The page's hint is the second question, and
 * giving a panel hint to a provider with no account would send the user to
 * declare one for a provider that publishes its own balance instead.
 */
export function hasPanelBalance(p: Provider, snapshot: Snapshot): boolean {
  if (!snapshot.account_available) return false;
  return (p.accounts ?? []).some((a) => a.enabled);
}
