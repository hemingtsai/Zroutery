/**
 * The accounts page: the runtime face of every declared account.
 *
 * # Why this is its own page, and not the provider drawer
 *
 * The split is declaration versus observation, and it follows the grain the
 * backend already has. The provider drawer edits what an account *is* — that it
 * exists, where its credential lives, what maintenance was asked for — which is
 * provider-scoped configuration written through a config save. This page shows
 * what an account *is doing* — the probe reading, the check-in phase, why the
 * scheduler acted or did not — which is runtime state that changes while nobody
 * is editing anything. One page per concern: a drawer that mixed both asked the
 * reader to hold a configuration question and an operations question in the same
 * scroll, and neither one benefits from the other's company.
 *
 * # What is deliberately not here
 *
 * No secret crosses this boundary, ever. The key column reads the snapshot's
 * key-presence map — whether a credential exists — and nothing else, the same
 * discipline the provider key field follows. And requests do not route by
 * account: the serving path resolves one credential per provider, which the
 * lede says rather than hides.
 */
import { useState } from "react";
import { useI18n, describeBackendError, type TKey } from "../i18n";
import { Badge, Button, Empty, NumberField, Section, Toggle, useToast } from "../components";
import {
  api,
  errorText,
  type AccountBalance,
  type AccountEvent,
  type AccountSyncOutcome,
  type AppConfig,
  type CheckinView,
  type Maintenance,
  type Provider,
  type ReconcileReport,
  type Snapshot,
} from "../api";

/** A whole number of seconds, as a short relative phrase. */
function ago(unix_secs: number): string {
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unix_secs);
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

export default function Accounts({
  snapshot,
  save,
  run,
  busy,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
}) {
  const { t } = useI18n();
  const notify = useToast();

  // What the last account probe established, held as the backend's own report
  // rather than folded into a summary, because the four outcomes are four
  // different statements. See `ReconcileTable` below.
  const [report, setReport] = useState<ReconcileReport | null>(null);
  const [probing, setProbing] = useState(false);
  // The account an HTTP check-in is in flight for, so the one button shows
  // progress without freezing the rest of the page.
  const [checking, setChecking] = useState<string | null>(null);

  // Offered only when the build actually contains the account subsystem. A
  // command that was never registered looks exactly like one that failed, so the
  // absence is declared in the snapshot rather than discovered by pressing.
  if (!snapshot.account_available) {
    return <Empty>{t("accounts.empty_build")}</Empty>;
  }

  const withAccounts = (snapshot.config.providers ?? []).filter(
    (p) => (p.accounts ?? []).length > 0,
  );

  /**
   * Probe every declared, enabled account, in one call.
   *
   * One call covers every provider, which is why this sits at the top of the
   * page and not inside one provider's section. Nothing calls this on a timer or
   * at startup: probing costs a network request per account, so the button is
   * the policy about when.
   */
  const probeAccounts = async () => {
    setProbing(true);
    try {
      setReport(await api.reconcileAccounts());
      // The reconciler published into the account store, which is where the
      // snapshot's balances and per-account figures are read from. Re-read the
      // snapshot rather than patching it here, so the page cannot end up showing
      // a probe result beside balances from before it ran.
      await run(api.snapshot);
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setProbing(false);
    }
  };

  /**
   * Check one account in over the panel's own HTTP API — the stored credential
   * is the login, no browser.
   *
   * The outcome is surfaced with the backend's own phase wording, and the
   * snapshot is re-read so the row's balance reflects any reward that was
   * observed rather than the balance from before the check-in.
   */
  const checkInHttp = async (providerId: string, accountId: string) => {
    const key = `${providerId}/${accountId}`;
    setChecking(key);
    try {
      const view = await api.apiCheckin(providerId, accountId);
      // The backend's own phase wording, with the reward when one was observed.
      // An unrecognised phase renders as itself rather than as a neighbour.
      const label = PHASE_KEYS.has(view.phase)
        ? t(`account.phase.${view.phase}` as TKey)
        : view.phase;
      const reward =
        view.reward_amount !== null ? ` (+${view.reward_amount} ${view.reward_unit ?? ""})` : "";
      notify("ok", `${label}${reward}`);
      await run(api.snapshot);
    } catch (e) {
      notify("error", `${t("accounts.checkin_api_failed")}: ${describeBackendError(t, errorText(e))}`);
    } finally {
      setChecking(null);
    }
  };

  return (
    <>
      {withAccounts.length === 0 && <Empty>{t("accounts.none")}</Empty>}

      <Section
        title={t("accounts.probe_section")}
        hint={t("accounts.probe_hint")}
        actions={
          <Button
            kind="primary"
            onClick={() => void probeAccounts()}
            disabled={busy || probing}
          >
            {probing ? t("accounts.probing") : t("accounts.probe_now")}
          </Button>
        }
      >
        {report === null ? (
          <Empty>{t("accounts.probe_idle")}</Empty>
        ) : (
          <ReconcileTable report={report} />
        )}
      </Section>

      {withAccounts.map((provider) => (
        <Section
          key={provider.id}
          title={
            <>
              {provider.name}{" "}
              {!provider.enabled && (
                <span className="muted">· {t("accounts.provider_disabled")}</span>
              )}
            </>
          }
          hint={provider.base_url}
        >
          <div className="list">
            {(provider.accounts ?? []).map((account) => (
              <AccountRow
                key={account.account_id}
                provider={provider}
                accountId={account.account_id}
                maintenance={account.maintenance}
                snapshot={snapshot}
                checking={checking === `${provider.id}/${account.account_id}`}
                busy={busy}
                onDeclare={(mutate) =>
                  void save((cfg) => {
                    const next = structuredClone(cfg);
                    const target = next.providers.find((p) => p.id === provider.id);
                    const declaration = target?.accounts?.find(
                      (a) => a.account_id === account.account_id,
                    );
                    if (!declaration) return null;
                    mutate(declaration.maintenance);
                    return next;
                  })
                }
                onHttpCheckin={() => void checkInHttp(provider.id, account.account_id)}
                onBrowserStart={() =>
                  void startBrowser(
                    provider.id,
                    account.account_id,
                    account.maintenance,
                    notify,
                    t,
                  )
                }
                onBrowserResume={() =>
                  void resumeBrowser(provider.id, account.account_id, notify, t)
                }
                onBrowserCancel={() =>
                  void cancelBrowser(provider.id, account.account_id, notify, t)
                }
              />
            ))}
          </div>
        </Section>
      ))}

      <EventsSection events={snapshot.account_events ?? []} />
    </>
  );
}

/** One account's whole runtime reading, with the actions that act on it. */
function AccountRow({
  provider,
  accountId,
  maintenance,
  snapshot,
  checking,
  busy,
  onDeclare,
  onHttpCheckin,
  onBrowserStart,
  onBrowserResume,
  onBrowserCancel,
}: {
  provider: Provider;
  accountId: string;
  maintenance: Maintenance;
  snapshot: Snapshot;
  checking: boolean;
  busy: boolean;
  /** Edits this account's maintenance declaration through the save pipeline. */
  onDeclare: (mutate: (maintenance: Maintenance) => void) => void;
  onHttpCheckin: () => void;
  onBrowserStart: () => void;
  onBrowserResume: () => void;
  onBrowserCancel: () => void;
}) {
  const { t } = useI18n();
  const view = checkinFor(snapshot.checkin, provider.id, accountId);
  const balance = (snapshot.account_balances ?? []).find(
    (b) => b.provider_id === provider.id && b.account_id === accountId,
  );
  const paused = view.awaiting_user;

  // The backend publishes a closed phase vocabulary, and a template key is not a
  // key TypeScript can check. The fallback matters more than the cast: an
  // unrecognised phase must read as "unknown", never as a phase that implies a
  // reward or a success.
  const phaseLabel = PHASE_KEYS.has(view.phase)
    ? t(`account.phase.${view.phase}` as Parameters<typeof t>[0])
    : view.phase;

  // Whether this account's credential is its own. The keys map carries the
  // account's own reference only when it declares one, so "absent" means two
  // different things and the label says which: either the account shares the
  // provider's secret, or it declared its own and none is stored.
  const ownRef =
    accountKeyRef(provider, accountId) !== (provider.key_ref || "");
  const ownStored = ownRef && (snapshot.keys ?? {})[accountKeyRef(provider, accountId)];

  const status = balance?.status ?? "unprobed";
  const statusTone =
    status === "active"
      ? "ok"
      : status === "quota_exhausted" || status === "rate_limited"
        ? "warn"
        : status === "suspended" || status === "authentication_expired"
          ? "danger"
          : "neutral";

  return (
    <div className="account-maint">
      <div className="row-main">
        <span className="row-title mono">{accountId}</span>
        <Badge tone={statusTone}>
          {STATUS_KEYS.has(status)
            ? t(`accounts.status.${status}` as Parameters<typeof t>[0])
            : status}
        </Badge>
        <span className="row-sub muted">
          {ownRef
            ? ownStored
              ? t("accounts.own_key_stored")
              : t("accounts.own_key_missing")
            : t("accounts.shared_key")}
        </span>
        {balance && (
          <span className="row-sub mono">
            {t("account.balance")} {accountBalanceLabel(balance, t, ago)}
          </span>
        )}
        {/*
          The credential-type hint, shown exactly where it is actionable. The
          panel API authenticates with the site's *access token* — the string on
          the panel's personal-settings page — and an `sk-` relay key fails there
          with an answer that reads as "wrong password". Saying which is which
          here is the difference between a fixable declaration and a stale one.
        */}
        {status === "authentication_expired" && (
          <span className="row-sub warn-text">{t("accounts.auth_hint")}</span>
        )}
        {balance?.last_failure !== null && balance?.last_failure !== undefined && (
          <span className="row-sub warn-text">
            {t("accounts.last_failure")} {ago(balance.last_failure)}
          </span>
        )}
        <span className="row-sub">{phaseLabel}</span>
        {view.last_completed_at !== null && (
          <span className="row-sub muted">
            · {t("account.last_checkin")} {ago(view.last_completed_at)}
          </span>
        )}
        {view.failure && <span className="row-sub warn-text">{view.failure}</span>}
        {/*
          Why the scheduler is or is not acting, in its own words. Without this an
          operator who sets an interval sees a row that never changes and cannot
          tell a policy that is working from a policy that is doing nothing.
        */}
        <span className="row-sub muted">{scheduleLabel(view)}</span>
        {/*
          A reward is shown only when one was observed, and its source travels
          with it. A figure recovered from provider log text is weaker evidence
          than one the provider reported in a structured field, and printing them
          identically would claim more than the backend knows.
        */}
        {view.reward_amount !== null && (
          <span className="row-sub mono">
            {t("account.reward")} {view.reward_amount} {view.reward_unit ?? ""}
            {view.reward_source === "event_content" && (
              <span className="muted"> · {t("account.reward_weak_source")}</span>
            )}
            {view.reward_source === "balance_delta" && (
              <span className="muted"> · {t("account.reward_inferred")}</span>
            )}
          </span>
        )}
        {latestEventFor(snapshot.account_events, provider.id, accountId) && (
          <span className="row-sub mono">
            {eventLabel(latestEventFor(snapshot.account_events, provider.id, accountId)!, t)}
          </span>
        )}
      </div>

      <div className="controls">
        {paused ? (
          <>
            <Button kind="primary" disabled={busy} onClick={onBrowserResume}>
              {t("account.resume")}
            </Button>
            <Button kind="ghost" disabled={busy} onClick={onBrowserCancel}>
              {t("account.cancel")}
            </Button>
          </>
        ) : (
          <>
            {snapshot.api_checkin_available && (
              <Button
                kind="primary"
                disabled={busy || checking || !maintenance.checkin_enabled}
                title={t("accounts.checkin_api_hint")}
                onClick={onHttpCheckin}
              >
                {checking ? t("accounts.checking") : t("accounts.checkin_api")}
              </Button>
            )}
            {snapshot.account_maintenance_available && (
              <>
                <Button kind="ghost" disabled={busy || view.phase === "running"} onClick={onBrowserStart}>
                  {t("account.checkin")}
                </Button>
                {view.browser_held && (
                  <Button kind="ghost" disabled={busy} onClick={onBrowserCancel}>
                    {t("account.close_browser")}
                  </Button>
                )}
              </>
            )}
          </>
        )}
      </div>

      {/*
        The one declaration this runtime row needs, declared where it runs. The
        provider drawer owns the full declaration, but an operator reading "不会
        自动运行" should not have to hunt for the drawer to fix it: the switch
        and the interval are exactly the two lines the scheduler reads, so they
        are editable right here, through the same config save.
      */}
      <div className="controls">
        <Toggle
          label={t("accounts.auto_section")}
          checked={maintenance.checkin_enabled}
          onChange={(v) => onDeclare((m) => { m.checkin_enabled = v; })}
        />
        <NumberField
          label={t("accounts.interval")}
          hint={t("accounts.interval_hint")}
          min={1}
          integer
          value={maintenance.checkin_interval_secs}
          onCommit={(v) => onDeclare((m) => { m.checkin_interval_secs = v; })}
        />
      </div>
    </div>
  );
}

/** The reference one account's credential lives under, from the keys map's own convention. */
function accountKeyRef(provider: Provider, accountId: string): string {
  const account = (provider.accounts ?? []).find((a) => a.account_id === accountId);
  return account?.key_ref?.trim() ? account.key_ref : (provider.key_ref || "");
}

/** One observed thing that happened to an account's resources. */
function EventsSection({ events }: { events: AccountEvent[] }) {
  const { t } = useI18n();
  if (events.length === 0) {
    return (
      <Section title={t("accounts.events_section")} hint={t("accounts.events_hint")}>
        <Empty>{t("accounts.events_none")}</Empty>
      </Section>
    );
  }
  return (
    <Section title={t("accounts.events_section")} hint={t("accounts.events_hint")}>
      <table className="table">
        <thead>
          <tr>
            <th>{t("accounts.events_when")}</th>
            <th>{t("field.account")}</th>
            <th>{t("accounts.events_what")}</th>
          </tr>
        </thead>
        <tbody>
          {events.map((event, index) => (
            <tr key={`${event.provider_id}/${event.account_id}/${event.observed_at}/${index}`}>
              <td className="mono">{ago(event.observed_at)}</td>
              <td className="mono">
                {event.provider_id}/{event.account_id}
              </td>
              <td>{eventLabel(event, t)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Section>
  );
}

// ------------------------------------------------------------- moved helpers
// The probe table and the check-in row vocabulary moved here with the surface:
// the provider drawer keeps the declaration, and these render observations.

/** An empty report is its own sentence rather than an empty table. */
function ReconcileTable({ report }: { report: ReconcileReport }) {
  const { t } = useI18n();
  if (report.outcomes.length === 0) {
    return <Empty>{t("accounts.probe_none")}</Empty>;
  }
  return (
    <table className="table">
      <thead>
        <tr>
          <th>{t("field.provider")}</th>
          <th>{t("field.account")}</th>
          <th>{t("accounts.probe_outcome")}</th>
        </tr>
      </thead>
      <tbody>
        {report.outcomes.map(([providerId, accountId, outcome], index) => (
          <tr key={`${providerId}/${accountId}/${index}`}>
            <td className="mono">{providerId}</td>
            <td className="mono">{accountId}</td>
            <td>
              <ReconcileOutcome outcome={outcome} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * One account's outcome, in the backend's own terms.
 *
 * The four are kept apart rather than collapsed into one healthy/unhealthy pair:
 * `refreshed` is a reading, `unsupported` is an account nothing was able to ask,
 * `partially_reported` is a provider that declined to say something, and
 * `failed` is an account that was asked and refused. A failure's reason is
 * passed through the backend-error translator first, so the common refusals —
 * a missing or relay-key credential, a stale classifier candidate — read as
 * instructions rather than as internal vocabulary; whatever it cannot
 * translate prints as the backend said it.
 */
function ReconcileOutcome({ outcome }: { outcome: AccountSyncOutcome }) {
  const { t } = useI18n();
  if (outcome === "refreshed") {
    return <span className="row-sub">{t("account.sync.refreshed")}</span>;
  }
  if (outcome === "unsupported") {
    return <span className="row-sub">{t("account.sync.unsupported")}</span>;
  }
  if ("failed" in outcome) {
    return (
      <span className="row-sub warn-text">
        {t("account.sync.failed")} · {describeBackendError(t, outcome.failed.reason)}
      </span>
    );
  }
  // The missing fields are named rather than counted, because "partially
  // reported" without the list is a claim that something was withheld without
  // saying what.
  return (
    <span className="row-sub">
      {t("account.sync.partially_reported")} · {outcome.partially_reported.missing.join(", ")}
    </span>
  );
}

/** The check-in view for one account, or a neutral one when the snapshot carries none. */
function checkinFor(
  views: CheckinView[] | undefined,
  providerId: string,
  accountId: string,
): CheckinView {
  return (
    views?.find((v) => v.provider_id === providerId && v.account_id === accountId) ?? {
      provider_id: providerId,
      account_id: accountId,
      phase: "idle",
      browser_held: false,
      awaiting_user: false,
      failure: null,
      reward_amount: null,
      reward_unit: null,
      reward_source: null,
      last_completed_at: null,
      // "unknown" rather than a guess: the backend is the only thing that knows
      // this account's declaration and history.
      schedule: { decision: "unknown", next_due_at: null, last_settled_at: null },
    }
  );
}

/** One account's line: who it is, what it has left, and how well that is known. */
function accountBalanceLabel(
  balance: AccountBalance,
  t: (key: TKey, params?: Record<string, string | number>) => string,
  ago: (at: number) => string,
): string {
  if (balance.status === "unprobed") {
    // Not a balance of zero. Nobody has asked this account yet.
    return t("accounts.status.unprobed");
  }
  if (balance.remaining === null) {
    // The probe ended without a figure; the status says what is known, in the
    // panel's words rather than the wire vocabulary's.
    return STATUS_KEYS.has(balance.status)
      ? t(`accounts.status.${balance.status}` as Parameters<typeof t>[0])
      : balance.status;
  }
  const stale =
    balance.last_failure !== null
      ? ` · ${t("accounts.last_failure")} ${ago(balance.last_failure)}`
      : "";
  return `${t("account.balance_amount", {
    remaining: balance.remaining,
    unit: balance.unit ?? "",
    total: balance.total ?? t("common.unknown"),
  })}${stale}`;
}

/** The most recent observed event for one account, if there is one. */
function latestEventFor(
  events: AccountEvent[] | undefined,
  providerId: string,
  accountId: string,
): AccountEvent | undefined {
  let newest: AccountEvent | undefined;
  for (const event of events ?? []) {
    if (event.provider_id !== providerId || event.account_id !== accountId) continue;
    if (!newest || event.observed_at > newest.observed_at) newest = event;
  }
  return newest;
}

/** The effects the backend publishes, so an unknown one can be told apart. */
const EFFECT_KEYS = new Set(["credited", "debited", "no_change", "unobserved"]);
/** Where a figure was read from, strongest evidence first. */
const EVENT_SOURCE_KEYS = new Set([
  "operation_response",
  "provider_response",
  "account_state",
  "account_log",
]);

/**
 * One observed change, in words, with where it came from.
 *
 * The source is named rather than implied because the three are not equally
 * trustworthy: a structured field the provider returned is evidence, text
 * scraped from a localised log is a fallback.
 */
function eventLabel(
  event: AccountEvent,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): string {
  const effect = EFFECT_KEYS.has(event.effect)
    ? t(`account.effect.${event.effect}` as TKey)
    : event.effect;
  if ((event.effect === "credited" || event.effect === "debited") && event.amount !== null) {
    const sign = event.effect === "credited" ? "+" : "-";
    const source = EVENT_SOURCE_KEYS.has(event.source)
      ? t(`account.source.${event.source}` as TKey)
      : event.source;
    return `${sign}${event.amount} ${event.unit ?? ""} · ${source}`;
  }
  return effect;
}

/** The scheduler decisions the backend publishes. */
const SCHEDULE_KEYS = new Set([
  "disabled",
  "not_applicable",
  "needs_first_observation",
  "no_policy",
  "not_due",
  "due",
  "unknown",
]);

/** One sentence saying why the scheduler did or did not act on this account. */
function scheduleLabel(view: CheckinView): string {
  const { t } = useI18n();
  const decision = view.schedule?.decision ?? "unknown";
  const reason = SCHEDULE_KEYS.has(decision)
    ? t(`account.schedule.${decision}` as Parameters<typeof t>[0])
    : decision;
  const next = view.schedule?.next_due_at;
  if (next !== null && next !== undefined) {
    return `${reason} · ${t("account.next_run")} ${ago(next)}`;
  }
  return reason;
}

/** The phases the backend publishes, so an unknown one can be told apart. */
const PHASE_KEYS = new Set([
  "idle",
  "running",
  "waiting_for_user",
  "succeeded",
  "already_completed",
  "not_supported",
  "failed",
  "cancelled",
  "cancelling",
]);

/** The statuses the balance view publishes, so an unknown one can be told apart. */
const STATUS_KEYS = new Set([
  "active",
  "suspended",
  "quota_exhausted",
  "rate_limited",
  "authentication_expired",
  "unknown",
  "unprobed",
]);

// ------------------------------------------------------------ browser actions
// Same pre-flight checks the provider drawer applied, because the browser path
// is the same operation no matter which surface starts it: a declaration that
// cannot be acted on is refused with the name of the missing setting, here and
// not only in the backend, so the reader is told which line to fix.

async function startBrowser(
  providerId: string,
  accountId: string,
  maintenance: Maintenance,
  notify: ReturnType<typeof useToast>,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): Promise<void> {
  if (!maintenance.checkin_enabled) {
    notify("error", t("account.not_enabled"));
    return;
  }
  const missing = !maintenance.checkin_path?.trim()
    ? t("account.need_path")
    : !maintenance.checkin_trigger_selector?.trim()
      ? t("account.need_selector")
      : null;
  if (missing) {
    notify("error", missing);
    return;
  }
  try {
    await api.startCheckin(providerId, accountId);
  } catch (e) {
    notify("error", `${t("account.start_failed")}: ${errorText(e)}`);
  }
}

async function resumeBrowser(
  providerId: string,
  accountId: string,
  notify: ReturnType<typeof useToast>,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): Promise<void> {
  try {
    await api.resumeCheckin(providerId, accountId);
  } catch (e) {
    notify("error", `${t("account.resume_failed")}: ${errorText(e)}`);
  }
}

async function cancelBrowser(
  providerId: string,
  accountId: string,
  notify: ReturnType<typeof useToast>,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): Promise<void> {
  try {
    await api.cancelCheckin(providerId, accountId);
  } catch (e) {
    notify("error", `${t("account.cancel_failed")}: ${errorText(e)}`);
  }
}
