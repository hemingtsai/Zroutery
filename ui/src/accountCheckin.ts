import type { CheckinView, Maintenance, Provider } from "./api";

/** Match the backend's exact relay-to-panel mapping, never a provider name. */
export function isLoginSettledCheckin(
  provider: Pick<Provider, "base_url">,
  maintenance: Pick<Maintenance, "panel_url">,
): boolean {
  const explicit = maintenance.panel_url?.trim();
  let root = (explicit || provider.base_url).trim().replace(/\/+$/, "");
  while (root.endsWith("/v1") || root.endsWith("/api")) {
    root = root.replace(/\/(?:v1|api)$/, "").replace(/\/+$/, "");
  }
  if (!explicit && root.toLowerCase() === "https://ps.air-outer.com") {
    root = "https://agentrouter.org";
  }
  try {
    const url = new URL(root);
    return url.origin === "https://agentrouter.org" && !url.username && !url.password;
  } catch {
    return false;
  }
}

export function browserCheckinMissing(
  provider: Pick<Provider, "base_url">,
  maintenance: Maintenance,
): "path" | "selector" | null {
  if (isLoginSettledCheckin(provider, maintenance)) return null;
  if (!maintenance.checkin_path?.trim()) return "path";
  return maintenance.checkin_trigger_selector?.trim() ? null : "selector";
}

/** One account a "check in all" press may start. */
export interface CheckinTarget {
  providerId: string;
  accountId: string;
}

/**
 * Which accounts a "check in all" press may start.
 *
 * Only browser check-ins are included; an HTTP check-in is a separate action
 * with its own credential path. Disabled providers and accounts are skipped,
 * as are declarations that cannot be acted on and any account already running
 * or waiting for a person — a bulk press must never replace or duplicate a
 * session that exists. Multi-account is therefore just this list: each account
 * owns its own isolated browser profile on the backend.
 */
export function checkinAllTargets(
  providers: Provider[],
  checkin: CheckinView[] | undefined,
): CheckinTarget[] {
  const phases = new Map(
    (checkin ?? []).map((view) => [
      `${view.provider_id}/${view.account_id}`,
      view.phase,
    ]),
  );
  return providers.flatMap((provider) => {
    if (!provider.enabled) return [];
    return (provider.accounts ?? []).flatMap((account) => {
      const { account_id: accountId, maintenance } = account;
      if (!maintenance.checkin_enabled) return [];
      if (browserCheckinMissing(provider, maintenance)) return [];
      const phase = phases.get(`${provider.id}/${accountId}`);
      if (phase === "running" || phase === "waiting_for_user") return [];
      return [{ providerId: provider.id, accountId }];
    });
  });
}
