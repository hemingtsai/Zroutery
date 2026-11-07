import {
  Fragment,
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  api,
  BALANCE_PRESETS,
  defaultProbe,
  emptyProvider,
  errorText,
  money,
  slugify,
  type Account,
  type BindingPreview,
  type AppConfig,
  type BalanceStatus,
  type CcProviderDraft,
  type CcSwitchPreview,
  type Maintenance,
  type Provider,
  type ProviderClientProfile,
  type ProviderKind,
  type Snapshot,
  tierName,
} from "../api";
import { describeBackendError } from "../i18n";
import {
  Badge,
  Banner,
  Button,
  CompactNumber,
  ConfirmDialog,
  Drawer,
  Empty,
  Field,
  KeyValue,
  NumberField,
  PageHead,
  Section,
  Segment,
  Select,
  StatusDot,
  TextField,
  Toggle,
  useToast,
  type ConfirmRequest,
} from "../components";
import { useI18n } from "../i18n";
import { useMode } from "../mode";
import { groupProviders } from "../providerGroups";
import { hasChatCredential } from "../providerCredential";
import { SaveFeedbackReset, type TaskKind } from "../saveQueue";
import NewApiPanel from "./NewApiPanel";
import { revealAddFormLanding } from "./Models";
import { AccountBindingReview } from "../AccountBindingReview";

type KindLabelKey =
  | "providers.openai_dialect"
  | "providers.anthropic_dialect"
  | "providers.opencode_zen_dialect"
  | "providers.opencode_console_dialect"
  | "providers.decision_dialect";

type ProfileLabelKey =
  | "providers.profile_auto"
  | "providers.profile_native"
  | "providers.profile_claude_code"
  | "providers.profile_codex"
  | "providers.profile_opencode_cli";

const KINDS: { id: ProviderKind; labelKey: KindLabelKey }[] = [
  { id: "openai_compatible", labelKey: "providers.openai_dialect" },
  { id: "anthropic", labelKey: "providers.anthropic_dialect" },
  { id: "opencode_zen_free", labelKey: "providers.opencode_zen_dialect" },
  { id: "opencode_console", labelKey: "providers.opencode_console_dialect" },
  { id: "decision", labelKey: "providers.decision_dialect" },
];

/**
 * The provider kinds this build can actually serve.
 *
 * The two OpenCode kinds are configuration rather than code, so they survive
 * every build — but the request shaping that makes OpenCode's gateway accept a
 * request does not, and the gateway checks a fixed fingerprint and rejects
 * anything else. Offering them in a build that cannot shape the request is
 * offering a provider whose every request is refused, so they are offered only
 * where they work.
 */
function kindsFor(opencodeAvailable: boolean): { id: ProviderKind; labelKey: KindLabelKey }[] {
  if (opencodeAvailable) return KINDS;
  return KINDS.filter((k) => k.id !== "opencode_zen_free" && k.id !== "opencode_console");
}

const CLIENT_PROFILES: { id: ProviderClientProfile; labelKey: ProfileLabelKey }[] = [
  { id: "auto", labelKey: "providers.profile_auto" },
  { id: "native", labelKey: "providers.profile_native" },
  { id: "claude_code", labelKey: "providers.profile_claude_code" },
  { id: "codex", labelKey: "providers.profile_codex" },
  { id: "opencode_cli", labelKey: "providers.profile_opencode_cli" },
];

/**
 * The client profiles this build can actually shape a request with.
 *
 * The `opencode_cli` profile exists to make a request look like the OpenCode
 * CLI, which is what OpenCode's gateway accepts. Without the shaping behind
 * it, a request wearing that identity is refused — so the profile is offered
 * only where the shaping is.
 */
function clientProfilesFor(opencodeAvailable: boolean): { id: ProviderClientProfile; labelKey: ProfileLabelKey }[] {
  if (opencodeAvailable) return CLIENT_PROFILES;
  return CLIENT_PROFILES.filter((c) => c.id !== "opencode_cli");
}

/**
 * Whether this kind targets OpenCode's gateway.
 *
 * Mirrors `ProviderKind::is_opencode`. Such a provider speaks the OpenAI
 * dialect but is not an OpenAI-compatible endpoint: the gateway checks a fixed
 * fingerprint and rejects anything else, which is why both the profile and the
 * discovery surface behave differently for these two kinds.
 */
function isOpencodeKind(kind: ProviderKind): boolean {
  return kind === "opencode_zen_free" || kind === "opencode_console";
}

/**
 * Whether this kind answers only with a stored key.
 *
 * The Zen free gateway identifies its clients by a fixed fingerprint and sells
 * nothing, so it issues no keys: a free provider with the default
 * `provider:{id}` reference is *complete*, not missing a credential. The
 * reference-based check alone called every free provider "warn" — a yellow dot
 * with no action anywhere that could clear it, on the one kind of provider
 * that needs nothing.
 */
function needsKey(kind: ProviderKind): boolean {
  return kind !== "opencode_zen_free";
}

/** Mirrors `ProviderKind::default_base_url` in `zroutery-core::config`. */
function defaultBaseUrl(kind: ProviderKind): string {
  switch (kind) {
    case "anthropic":
      return "https://api.anthropic.com";
    case "opencode_zen_free":
      return "https://opencode.ai/zen";
    case "opencode_console":
      return "https://opencode.ai/inference/openai/v1";
    case "decision":
      return "https://api.typesafe.ai";
    default:
      return "https://api.openai.com/v1";
  }
}

/**
 * A readable provider name derived from a pasted endpoint.
 *
 * The add form accepts a URL alone because the endpoint is the thing an
 * operator has; making them invent a display name first is a step that adds
 * nothing. `https://ps.air-outer.com/v1` becomes `ps.air-outer`.
 */
function nameFromBaseUrl(url: string): string {
  try {
    const host = new URL(url.trim()).hostname.toLowerCase();
    return host.replace(/^www\./, "") || "";
  } catch {
    return "";
  }
}

/**
 * The url a decision provider is asked at, mirroring `DecisionConfig::url` in
 * `zroutery-core::config`.
 *
 * The base is used as it stands and `/v1` is added to a path that does not
 * already begin with it, so `/v1/systemone` and `/systemone` name the same
 * endpoint — and a path that starts with neither still names one, just not the
 * one the operator may have meant. The drawer shows the result, because the
 * *Endpoint* row is where an operator checks the address and this is the
 * address the request goes to.
 */
function decisionUrl(baseUrl: string, path: string): string {
  const base = baseUrl.replace(/\/+$/, "");
  const trimmed = path.trim();
  if (trimmed === "") return base;
  const segments = trimmed.split("/").filter(Boolean);
  const baseSegments = base.split("/").filter(Boolean);
  if (segments.length && segments.every((part, index) => part === baseSegments[baseSegments.length - segments.length + index])) return base;
  const suffix = `/${trimmed.replace(/^\/+/, "")}`;
  if (base.endsWith("/v1")) return `${base}${suffix.replace(/^\/v1\//, "/")}`;
  return suffix === "/v1" || suffix.startsWith("/v1/") ? `${base}${suffix}` : `${base}/v1${suffix}`;
}

/**
 * Mirrors the `client_profile` arm of `ProviderConfig::new`.
 *
 * The OpenCode arm is not a preference: `config.rs` rejects any OpenCode
 * provider whose configured profile is not `opencode_cli`, so creating or
 * re-kinding one without it produces a config that will not validate.
 *
 * A decision endpoint is `native` for the same kind of reason — not a
 * validation rule but a fact about the request. The decision body is not a
 * chat, so there is no client identity to impersonate, and `ProviderConfig::new`
 * files it as native. Offering `auto` here would let the dashboard create a
 * provider whose identity differs from the one the backend's own constructor
 * would have given it.
 */
function defaultClientProfile(kind: ProviderKind): ProviderClientProfile {
  switch (kind) {
    case "anthropic":
      return "claude_code";
    case "opencode_zen_free":
    case "opencode_console":
      return "opencode_cli";
    case "decision":
      return "native";
    default:
      return "auto";
  }
}

/** The label `KINDS` gives a kind, for the places that show one directly. */
function kindLabelKey(kind: ProviderKind): KindLabelKey {
  return KINDS.find((k) => k.id === kind)?.labelKey ?? "providers.openai_dialect";
}

/**
 * The provider list. A provider is a connection to one upstream — endpoint,
 * credential, dialect — and the drawer is where it lives in full. Models
 * belong to a provider but have their own page; this drawer only lists them.
 */
export default function Providers({
  snapshot,
  save,
  run,
  busy,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>, kind?: TaskKind) => Promise<boolean>;
  busy: boolean;
}) {
  const { config, keys, balances } = snapshot;
  const { t, plural } = useI18n();
  const notify = useToast();
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);

  const [ccPreview, setCcPreview] = useState<CcSwitchPreview | null>(null);
  const [ccLoading, setCcLoading] = useState(false);
  // The add form's folded state. Opened from the page head; closes itself
  // when a provider is added, because the drawer that opens over it is the
  // continuation of that task.
  const [adding, setAdding] = useState(false);
  /*
   * …and when it opens, the reader is handed it: the landing under the head
   * is brought into the viewport (a no-op from the top, where the trigger
   * put them) and the focus lands in the form's first field. The behavior
   * and its order live in `revealAddFormLanding`, shared with the model
   * page's entry; StrictMode's double effect just lands the same focus twice.
   */
  const addLandingRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!adding) return;
    revealAddFormLanding(addLandingRef.current, "input");
  }, [adding]);
  const [ccSelected, setCcSelected] = useState<Record<string, boolean>>({});
  const isMounted = useRef(true);
  useEffect(() => () => { isMounted.current = false; }, []);

  /**
   * How many models each provider owns.
   *
   * Every row of the list used to ask the model list this question on its
   * own — one scan of the whole model list per row — so a single render cost
   * O(providers x models), and the number it wanted was a count. Counting
   * once and looking the answer up makes the render O(models), and the number
   * on the row is the same either way.
   */
  const modelCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const model of config.models) {
      counts.set(model.provider_id, (counts.get(model.provider_id) ?? 0) + 1);
    }
    return counts;
  }, [config.models]);

  /*
   * The list, grouped by vendor.
   *
   * `groupProviders` owns the rule and its tests; this page only draws the
   * result, so a change to what counts as the same vendor cannot be made here
   * without the case that motivated it.
   */
  const grouped = useMemo(() => groupProviders(config.providers), [config.providers]);

  /**
   * Opening a provider, and nothing else.
   *
   * The list is drawn one memoised row at a time, so the callback a row is
   * given has to be the same function every render: a new one would make
   * every row look changed and re-render the whole list to open a drawer.
   */
  const openProvider = useCallback((id: string) => setOpenId(id), []);

  /**
   * Apply an edit to the provider the save is actually about.
   *
   * `mutate` receives the provider from the config being committed rather
   * than one captured at render time, so a save queued behind an in-flight
   * save merges into that save's result instead of carrying a whole stale
   * `quirks`/`balance` object back over it.
   */
  const update = (id: string, mutate: (provider: Provider) => void) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const provider = next.providers.find((p) => p.id === id);
      if (!provider) return null;
      mutate(provider);
      return next;
    });
  };

  /**
   * Remove a provider, its models and its credential in one backend call.
   *
   * Saving the configuration first and clearing the key afterwards lost the
   * provider's real `key_ref` — the clear could only guess `provider:{id}`, so
   * a custom reference stayed in the credential store. The backend reads the
   * reference before changing the configuration and refuses to delete one
   * another provider still uses.
   */
  const removeProvider = async (id: string) => {
    const models = config.models.filter((m) => m.provider_id === id);
    const ok = await run(() => api.removeProvider(id));
    if (!ok || !isMounted.current) return;
    setOpenId(null);
    if (models.length) {
      notify("ok", t("providers.removed_models_notice", { n: models.length }));
    }
  };

  // ------------------------------------------------------- CC Switch import

  const loadCcPreview = async () => {
    setCcLoading(true);
    try {
      const preview = await api.ccswitchPreview();
      setCcPreview(preview);
      const selection: Record<string, boolean> = {};
      for (const p of preview.providers) {
        selection[p.source_id] = !p.already_imported;
      }
      setCcSelected(selection);
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setCcLoading(false);
    }
  };

  /**
   * Refresh every balance, as an observation.
   *
   * The readings travel through the same serial chain as a save — an
   * observation applying an older document over a fresher write would roll
   * the rebase point back — but the words are the observation's own: the
   * readings are current, and nothing here claims a configuration was saved.
   * A per-provider failure arrives inside the snapshot and is said beside that
   * provider's balance; a rejection means the refresh itself could not run,
   * and the last readings stand untouched.
   */
  const refreshAllBalances = async () => {
    const ok = await run(api.refreshBalances, "observe");
    if (ok) notify("ok", t("toast.balances_refreshed"));
    else notify("error", t("toast.balances_refresh_failed"));
  };

  const importSelected = async () => {
    const ids = Object.entries(ccSelected)
      .filter(([, on]) => on)
      .map(([id]) => id);
    if (ids.length === 0) return;
    const ok = await run(() => api.ccswitchImport(ids));
    if (!ok || !isMounted.current) return;
    notify("ok", t("cc.imported_notice", { n: ids.length }));
    await loadCcPreview();
  };

  const open = config.providers.find((p) => p.id === openId) ?? null;
  const openModelCount = open ? (modelCounts.get(open.id) ?? 0) : 0;

  /*
   * Whether any provider has a balance to ask for at all.
   *
   * The gate for the refresh-all button. A provider with `preset: "none"` has
   * nothing to ask for, so on an installation where every provider is like that
   * the button would press, spend a request per provider, and change nothing —
   * which is the same "control this build cannot serve" problem the account
   * section below guards against, in the other direction.
   *
   * A provider that *declares accounts* is counted too, because the balance of
   * a panel-backed provider does not come from the relay: it comes from the
   * panel, read with the user's access token. The relay publishes nothing.
   */
  const anyBalance = config.providers.some((p) => hasBalanceSource(p, snapshot));

  /*
   * Providers the wallet check has paused. The hold is a property of the
   * provider, so it is marked on its own row and explained in its drawer; it
   * used to be a second list above the first, printing the same two providers
   * again under a heading whose only new content was the release button.
   */
  const heldProviders = useMemo(
    () => new Set(snapshot.wallet_holds.map((hold) => hold.provider_id)),
    [snapshot.wallet_holds],
  );

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      <PageHead
        lede={
          config.providers.length === 0
            ? t("providers.lede_none")
            : t("count.providers", { n: config.providers.length })
        }
        actions={
          <>
            {/*
             * Adding is the page's primary verb, so its entry is the page's
             * first action and the form it opens is collapsed until asked
             * for — a permanently expanded three-field form under the list
             * was furniture nobody had asked to sit on. cc-switch puts its
             * add entry in the header and opens a wizard; here the form
             * unfolds directly under this head, above the list — the old
             * berth at the bottom surfaced it a whole list-length below the
             * button that opened it, and the landing of an unfolded form is
             * wherever it is mounted. The layout harness expands it the same
             * way a reader would: by clicking this link.
             */}
            <button type="button"
              className="linky"
              onClick={() => setAdding((open) => !open)}
            >
              {adding ? t("common.collapse") : t("providers.add")}
            </button>
            {/*
              `refresh_balances` asks every provider for its balance and probes
              every account, then answers with one snapshot — so the chips on this
              page and the figures in each drawer go current without opening any of
              them. The account probe below asks the same accounts but answers with
              the per-account outcome, which is the only way to learn *which* one is
              unsupported. They overlap deliberately: one refreshes, one diagnoses.
            */}
            {anyBalance && (
              <button type="button"
                className="linky"
                onClick={() => void refreshAllBalances()}
                disabled={busy}
              >
                {t("providers.refresh_all")}
              </button>
            )}
            <button type="button" className="linky" onClick={loadCcPreview} disabled={ccLoading || busy}>
              {ccLoading ? t("providers.reading_cc") : t("providers.import_cc")}
            </button>
          </>
        }
      />

      {/*
       * The form unfolds from the page head's entry — and it unfolds here,
       * above the list, because the landing of an unfolded form is wherever
       * it is mounted: at the bottom it surfaced a whole list-length below
       * the button that opened it. Opening hands the focus to the form's
       * first field (the effect beside `adding`); the layout harness expands
       * it the same way a reader would, by clicking the head link.
       */}
      {adding && (
        <div ref={addLandingRef}>
          <AddProviderForm
            providers={config.providers}
            save={save}
            busy={busy}
            opencodeAvailable={snapshot.opencode_available}
            onAdded={(id) => {
              setAdding(false);
              openProvider(id);
            }}
          />
        </div>
      )}

      {config.providers.length === 0 ? (
        <div className="empty-state">
          <p>{t("providers.empty")}</p>
          <p className="muted">{t("providers.empty_hint")}</p>
        </div>
      ) : (
        <div className="list">
          {grouped.top.map((p) => (
            <Fragment key={p.id}>
              <ProviderRow
                id={p.id}
                name={p.name}
                baseUrl={p.base_url}
                kind={p.kind}
                modelCount={modelCounts.get(p.id) ?? 0}
                status={
                  !p.enabled
                    ? "off"
                    : !needsKey(p.kind) || hasChatCredential(p, keys)
                      ? "ok"
                      : "warn"
                }
                selected={openId === p.id}
                showBalance={hasBalanceSource(p, snapshot)}
                balance={balances[p.id]}
                held={heldProviders.has(p.id)}
                onOpen={openProvider}
              />
              {(grouped.nested.get(p.id) ?? []).map((d) => (
                <ProviderRow
                  key={d.id}
                  id={d.id}
                  name={d.name}
                  baseUrl={d.base_url}
                  kind={d.kind}
                  modelCount={modelCounts.get(d.id) ?? 0}
                  status={
                    !d.enabled
                      ? "off"
                      : !needsKey(d.kind) || hasChatCredential(d, keys)
                        ? "ok"
                        : "warn"
                  }
                  selected={openId === d.id}
                  showBalance={hasBalanceSource(d, snapshot)}
                  balance={balances[d.id]}
                  held={heldProviders.has(d.id)}
                  nested
                  onOpen={openProvider}
                />
              ))}
            </Fragment>
          ))}
        </div>
      )}

      {/* The account probe moved to the Accounts page: it is a runtime reading
          over every provider's accounts, not a property of this page's list, and
          the drawer never had a good place to put a cross-provider report. */}

      {ccPreview && (
        <Section
          title={t("providers.import_cc")}
          hint={ccPreview.source ? t("cc.source_hint", { path: ccPreview.source }) : undefined}
          actions={
            <>
              <Button kind="ghost" onClick={loadCcPreview} disabled={ccLoading || busy}>
                {t("cc.reload")}
              </Button>
              <Button
                kind="primary"
                onClick={importSelected}
                disabled={busy || !Object.values(ccSelected).some(Boolean)}
              >
                {t("cc.import")}
              </Button>
            </>
          }
        >
          {ccPreview.providers.length === 0 ? (
            <Empty>{t("cc.empty")}</Empty>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">{t("cc.import")}</th>
                  <th scope="col">{t("field.name")}</th>
                  <th scope="col">{t("providers.endpoint")}</th>
                  <th scope="col">{t("nav.models")}</th>
                </tr>
              </thead>
              <tbody>
                {ccPreview.providers.map((p) => (
                  <CcSwitchRow
                    key={p.source_id}
                    draft={p}
                    checked={ccSelected[p.source_id] ?? false}
                    onSelect={(id, on) => setCcSelected({ ...ccSelected, [id]: on })}
                  />
                ))}
              </tbody>
            </table>
          )}
        </Section>
      )}

      {open && (
        <ProviderDrawer
          key={open.id}
          provider={open}
          snapshot={snapshot}
          busy={busy}
          save={save}
          run={run}
          onClose={() => setOpenId(null)}
          onUpdate={(mutate) => update(open.id, mutate)}
          onRemove={() =>
            setConfirm({
              title: t("confirm.remove_provider"),
              body: plural(openModelCount, "confirm.remove_provider_body"),
              confirmLabel: t("providers.remove"),
              danger: true,
              onConfirm: () => void removeProvider(open.id),
            })
          }
        />
      )}
    </>
  );
}

/**
 * One provider in the list, memoised.
 *
 * The list is the only thing on this page that grows with the configuration,
 * and the row is the only thing in it that is drawn once per provider. It
 * takes every string it prints already resolved — the name, the endpoint, the
 * model count, the dialect — plus the values that change on their own: the
 * status dot, whether it is the open provider, and the balance reading
 * beside it.
 *
 * The markup is the row's own button, unchanged: a component boundary is not
 * a node, so the list draws exactly what it drew before.
 */
const ProviderRow = memo(function ProviderRow({
  id,
  name,
  baseUrl,
  kind,
  modelCount,
  status,
  selected,
  showBalance,
  balance,
  held,
  nested,
  onOpen,
}: {
  id: string;
  name: string;
  baseUrl: string;
  kind: ProviderKind;
  modelCount: number;
  status: "ok" | "warn" | "danger" | "off";
  selected: boolean;
  showBalance: boolean;
  balance: BalanceStatus | undefined;
  /** The wallet check paused this provider; the drawer carries the release. */
  held?: boolean;
  /** Rendered under the ordinary provider it shares a host with. */
  nested?: boolean;
  onOpen: (id: string) => void;
}) {
  const { t } = useI18n();
  return (
    <button type="button"
      className={`list-row ${nested ? "row-nested" : ""} ${selected ? "selected" : ""}`}
      onClick={() => onOpen(id)}
      /*
       * The name carries the state as well, because the row's own content is
       * replaced by it in the accessibility tree: the dot beside the provider
       * is a colour, and a colour is invisible to a screen reader. Without
       * this the row announced only "Open <name>", and whether the provider
       * was switched on — or was on with no credential stored — was nowhere
       * in the sentence.
       */
      aria-label={`${t("providers.open_row", { name })} — ${
        held
          ? t("status.wallet_held")
          : status === "off"
            ? t("status.disabled")
            : status === "warn"
              ? t("status.no_key")
              : t("status.enabled")
      }`}
    >
      <StatusDot
        tone={held ? "danger" : status}
        state={
          held
            ? t("status.wallet_held")
            : status === "off"
              ? t("status.disabled")
              : status === "warn"
                ? t("status.no_key")
                : t("status.enabled")
        }
      />
      <div className="row-main">
        <span className="row-title truncate" title={name}>
          {name}
        </span>
        <span className="row-sub">
          <span className="mono">{baseUrl}</span>
          {modelCount > 0 && ` · ${t("count.models", { n: modelCount })}`}
          {` · ${t(kindLabelKey(kind))}`}
        </span>
      </div>
      {showBalance && <BalanceChip status={balance} />}
    </button>
  );
});

/**
 * One submit intent's draft: what the three fields hold when it is made.
 */
export type AddProviderDraft = {
  name: string;
  baseUrl: string;
  kind: ProviderKind;
};

/**
 * How a submit intent ended.
 *
 * `added` is the only outcome that may clear the draft, fold the form and
 * open the new provider's drawer — the failure the review's O5 recorded was
 * exactly those effects running on a save nobody waited for. `duplicate`
 * covers both a name already on the page and the race where a queued save
 * lands on a document that has the id by now: the form stays open, and the
 * drawer of the *existing* provider must not open over it. `write_failed` is
 * a save the backend refused or the queue dropped — including the stale
 * rejection when the configuration changed underneath the save — and the
 * draft is kept as typed for a retry.
 */
export type AddProviderOutcome =
  | { kind: "added"; id: string }
  | { kind: "duplicate"; id: string }
  | { kind: "name_required" }
  | { kind: "write_failed" }
  | { kind: "ignored" };

/**
 * The add form's submission as one unit, the way `saveQueue` is.
 *
 * `run` turns one intent into at most one `save` call and reports how it
 * ended; the form applies the outcome to its state. A second intent while one
 * is still in flight is ignored — one press, one write, never a queue of
 * duplicates. Extracted from the component so the failure paths stay testable
 * without a DOM.
 */
export function createAddProviderSubmission(
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>,
) {
  let inFlight = false;
  return {
    /** Whether an intent is mid-save; the form's button waits on this. */
    inFlight: () => inFlight,
    async run(
      draft: AddProviderDraft,
      providers: Provider[],
    ): Promise<AddProviderOutcome> {
      if (inFlight) return { kind: "ignored" };
      const name = draft.name.trim() || nameFromBaseUrl(draft.baseUrl);
      if (!name) return { kind: "name_required" };
      const id = slugify(name);
      if (providers.some((p) => p.id === id)) return { kind: "duplicate", id };
      inFlight = true;
      try {
        // `committed` separates "the queue ran and the write landed" from
        // "the queue ran and found the provider already there": a mutate that
        // returns null also resolves true, and only the first is an add.
        let committed = false;
        const ok = await save((cfg) => {
          // Re-checked against the document being committed: a save that
          // queues behind another one rebases onto the newer configuration,
          // and the id may have been taken in between.
          if (cfg.providers.some((p) => p.id === id)) return null;
          const next = structuredClone(cfg);
          next.providers.push(
            emptyProvider(
              id,
              name,
              draft.kind,
              draft.baseUrl.trim() || defaultBaseUrl(draft.kind),
            ),
          );
          committed = true;
          return next;
        });
        if (!ok) return { kind: "write_failed" };
        if (!committed) return { kind: "duplicate", id };
        return { kind: "added", id };
      } finally {
        inFlight = false;
      }
    },
  };
}

/**
 * The add-a-provider form, with its own drafts.
 *
 * The drafts used to live in the page above, so typing a provider name
 * re-rendered the whole list — and every row of it re-counted the models it
 * owns, which is a walk over every model in the configuration. Nothing above
 * this form reads a draft, so the form owns them now and a keystroke costs
 * one input.
 *
 * The form is still the same form: the same fields, in the same order, with
 * the same names, and the provider it adds still opens its own drawer. What
 * changed is when: the drawer opens only after the save that added the
 * provider has landed, and a refused save leaves the form and its draft
 * standing for a retry.
 */
function AddProviderForm({
  providers,
  save,
  busy,
  opencodeAvailable,
  onAdded,
}: {
  providers: Provider[];
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
  /** Whether this build can talk to OpenCode's gateway at all. */
  opencodeAvailable: boolean;
  /** The provider that was added, so the page can open its drawer. */
  onAdded: (id: string) => void;
}) {
  const { t } = useI18n();
  const [newName, setNewName] = useState("");
  const [newKind, setNewKind] = useState<ProviderKind>("openai_compatible");
  const [newBaseUrl, setNewBaseUrl] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  // The form folds the moment its work ends, so a save that lands after the
  // fold — or after the page itself is gone — must not touch state that no
  // longer exists.
  const mounted = useRef(true);
  useEffect(() => {
    // StrictMode's mount–cleanup–mount reruns this pair on the same ref.
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const submission = useMemo(() => createAddProviderSubmission(save), [save]);

  const addProvider = async () => {
    // One intent, one write: while a save is in flight the press is ignored,
    // not queued behind itself.
    if (submission.inFlight()) return;
    setSaveError(null);
    setSubmitting(true);
    try {
      const outcome = await submission.run(
        { name: newName, baseUrl: newBaseUrl, kind: newKind },
        providers,
      );
      if (!mounted.current) return;
      if (outcome.kind === "added") {
        // A real add only. The drawer that opens over this form is where the
        // credential is filled in next, so the draft is cleared and the new
        // provider's own drawer opens on it.
        setNewName("");
        setNewBaseUrl("");
        setNameError(null);
        setSaveError(null);
        onAdded(outcome.id);
      } else if (outcome.kind === "duplicate") {
        setNameError(t("providers.duplicate", { id: outcome.id }));
      } else if (outcome.kind === "name_required") {
        setNameError(t("providers.name_required"));
      } else if (outcome.kind === "write_failed") {
        // The write was refused or dropped; the draft stays as typed for a
        // retry. The queue's own error is already on the window — this line
        // ties the refusal to the form that made the attempt.
        setSaveError(t("providers.add_failed"));
      }
      // `ignored`: another intent is mid-save, and this press made no write.
    } finally {
      if (mounted.current) setSubmitting(false);
    }
  };

  return (
    <Section title={t("providers.add_section")} hint={t("providers.add_hint")}>
      {saveError && <Banner tone="danger">{saveError}</Banner>}
      <div className="controls">
        <Field label={t("field.name")} danger={Boolean(nameError)} hint={nameError ?? t("providers.name_optional")}>
          <input
            value={newName}
            placeholder={t("providers.name_placeholder")}
            className={nameError ? "input-error" : undefined}
            onChange={(e) => {
              setNewName(e.currentTarget.value);
              setNameError(null);
            }}
            onKeyDown={(e) => e.key === "Enter" && addProvider()}
          />
        </Field>
        <Field label={t("field.api_dialect")}>
          <Segment
            ariaLabel={t("field.api_dialect")}
            value={newKind}
            onChange={(kind) => setNewKind(kind)}
            options={kindsFor(opencodeAvailable).map((k) => ({ value: k.id, label: t(k.labelKey) }))}
          />
        </Field>
        <Field label={t("field.base_url")} hint={t("field.base_url_hint")}>
          <input
            value={newBaseUrl}
            placeholder={defaultBaseUrl(newKind)}
            onChange={(e) => setNewBaseUrl(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && addProvider()}
          />
        </Field>
        <div className="field-actions">
          <Button
            kind="primary"
            onClick={addProvider}
            disabled={busy || submitting || (!newName.trim() && !newBaseUrl.trim())}
          >
            {t("providers.add")}
          </Button>
        </div>
      </div>
    </Section>
  );
}

function CcSwitchRow({
  draft,
  checked,
  onSelect,
}: {
  draft: CcProviderDraft;
  checked: boolean;
  onSelect: (id: string, on: boolean) => void;
}) {
  const { t, lang } = useI18n();
  // The backend decides this with the same rule the import command applies, so
  // a disabled row really is one the import would skip.
  const alreadyId = draft.already_imported ? draft.target_id : null;
  return (
    <tr className={draft.already_imported ? "row-warn" : ""}>
      <td>
        <input
          type="checkbox"
          aria-label={t("cc.import_row", { name: draft.name })}
          checked={checked}
          disabled={draft.already_imported}
          title={alreadyId ? `${t("cc.already")} (${alreadyId})` : undefined}
          onChange={(e) => onSelect(draft.source_id, e.currentTarget.checked)}
        />
      </td>
      <td>
        {draft.name}
        {draft.is_current && (
          <>
            {" "}
            <Badge tone="ok">{t("cc.active")}</Badge>
          </>
        )}
        {draft.already_imported && (
          <>
            {" "}
            <Badge tone="neutral">{t("cc.already")}</Badge>
          </>
        )}
      </td>
      <td className="muted mono">{draft.base_url}</td>
      <td className="muted">
        {draft.models.length === 0
          ? "—"
          : draft.models
              .map((m) =>
                m.tier
                  ? // The brackets are full-width in Chinese and half-width in
                    // English, because a full-width bracket in an English
                    // sentence is a Chinese mark in the wrong place.
                    lang === "zh"
                    ? `${m.upstream_model}（${tierName(m.tier, t)}）`
                    : `${m.upstream_model} (${tierName(m.tier, t)})`
                  : m.upstream_model,
              )
              .join(", ")}
      </td>
    </tr>
  );
}

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
function hasBalanceSource(p: Provider, snapshot: Snapshot): boolean {
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
function hasPanelBalance(p: Provider, snapshot: Snapshot): boolean {
  if (!snapshot.account_available) return false;
  return (p.accounts ?? []).some((a) => a.enabled);
}

/** Balance state as one quiet chip: what is left, or that the check failed. */
function BalanceChip({ status }: { status: BalanceStatus | undefined }) {
  const { t } = useI18n();
  if (!status) return <span className="muted">—</span>;
  if (status.aggregate) {
    return <span className="muted">
      {Object.values(status.aggregate.by_currency).map((balance) => money(balance.currency, balance.remaining ?? 0)).join(" · ") || t("common.unknown")}
      {status.aggregate.partial && <Badge tone="warn">{t("binding.partial", { ok: status.aggregate.succeeded, n: status.aggregate.participating })}</Badge>}
    </span>;
  }
  if (status.error) return <span className="muted">{t("providers.balance_failed")}</span>;
  if (status.balance) {
    const amount = status.balance.remaining ?? status.balance.total;
    return (
      <span className="muted mono">
        {amount !== null ? money(status.balance.currency, amount) : ""}
      </span>
    );
  }
  return null;
}

/** A fresh declaration: nothing observed, nothing scheduled, provider's key. */
function blankMaintenance(): Maintenance {
  return {
    checkin_enabled: false,
    checkin_interval_secs: null,
    checkin_path: null,
    checkin_trigger_selector: null,
    login_path: null,
    panel_url: null,
    new_api_user: null,
    browser_executable: "",
  };
}

/**
 * Declare, credential and maintain one provider's accounts.
 *
 * What this section edits is the *declaration*: the configuration that says an
 * account exists, where its credential lives and what maintenance was asked
 * for. It never shows an observation — quota, balances and the last check-in
 * are runtime readings, and they render in the maintenance section below,
 * filled by the reconciler.
 *
 * Two credentials are kept apart on purpose. The panel token (`key_ref`) probes
 * balances and signs in; it is never sent as chat traffic. `inference_keys` is
 * the pool the routing path may actually send upstream, and a declared account
 * with an empty pool does not fall back to the panel token or the provider key.
 *
 * Keys are written through the backend command, never through the config save:
 * the dashboard only ever learns whether a secret exists, the same discipline
 * the provider key field follows.
 */
function AccountSection({
  provider,
  snapshot,
  busy,
  save,
  run,
  onUpdate,
}: {
  provider: Provider;
  snapshot: Snapshot;
  busy: boolean;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  onUpdate: (mutate: (provider: Provider) => void) => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const accounts = provider.accounts ?? [];
  const [newId, setNewId] = useState("");
  const [newKey, setNewKey] = useState("");
  const [keyFor, setKeyFor] = useState<string | null>(null);
  const [keyDraft, setKeyDraft] = useState("");
  const [userHint, setUserHint] = useState("");
  const [bindingPreview, setBindingPreview] = useState<BindingPreview | null>(null);
  const [newInference, setNewInference] = useState<
    Record<string, { id: string; secret: string; note: string }>
  >({});
  const [inferenceKeyFor, setInferenceKeyFor] = useState<string | null>(null);
  const [inferenceDraft, setInferenceDraft] = useState("");
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [dragFrom, setDragFrom] = useState<{ accountId: string; keyId: string } | null>(null);
  const [dropAt, setDropAt] = useState<{ accountId: string; index: number } | null>(null);
  const [reordering, setReordering] = useState<Record<string, boolean>>({});
  const reorderPending = useRef(new Set<string>());
  const inferenceWriteChains = useRef(new Map<string, Promise<boolean>>());
  const inferenceRows = useRef(new Map<string, HTMLDivElement>());

  /** A reference of its own — the account does not resolve to the provider key. */
  const hasOwnRef = (a: Account) =>
    a.key_ref !== "" && a.key_ref !== provider.key_ref;
  const keyStored = (a: Account) =>
    hasOwnRef(a) && (snapshot.keys[a.key_ref] ?? false);

  /** Edit one declaration in place; the id is the key within this provider. */
  const edit = (account_id: string, mutate: (a: Account) => void) =>
    onUpdate((p) => {
      const declaration = p.accounts?.find((a) => a.account_id === account_id);
      if (declaration) mutate(declaration);
    });

  const addAccount = async () => {
    const id = newId.trim();
    const key = newKey.trim();
    if (!id) return;
    if (accounts.some((a) => a.account_id === id)) {
      notify("error", t("accounts.duplicate", { id }));
      return;
    }
    const ok = await save((cfg) => {
      const next = structuredClone(cfg);
      const target = next.providers.find((p) => p.id === provider.id);
      if (!target) return null;
      target.accounts = target.accounts ?? [];
      // Re-checked here rather than trusted from the render: a save that
      // queues behind another one rebases onto the newer document.
      if (target.accounts.some((a) => a.account_id === id)) return null;
      target.accounts.push({
        account_id: id,
        key_ref: "",
        probe_kind: "none",
        enabled: true,
        inference_keys: [],
        maintenance: blankMaintenance(),
      });
      return next;
    });
    if (!ok) return;
    setNewId("");
    setNewKey("");
    if (key) {
      // The declaration exists only after the save landed, so the key is
      // stored as a second step against the saved configuration.
      const stored = await run(() => api.setAccountKey(provider.id, id, key));
      if (!stored) notify("error", t("accounts.key_store_failed"));
    }
  };

  const storeKey = async (account_id: string) => {
    const key = keyDraft.trim();
    if (!key) return;
    try {
      setBindingPreview(await api.stageAccountKey(provider.id, account_id, key, userHint.trim() || undefined));
      setKeyDraft("");
    } catch (error) { notify("error", describeBackendError(t, errorText(error))); }
  };

  const inferenceDraftFor = (account_id: string) =>
    newInference[account_id] ?? { id: "", secret: "", note: "" };

  const editNewInference = (
    account_id: string,
    patch: Partial<{ id: string; secret: string; note: string }>,
  ) =>
    setNewInference((current) => ({
      ...current,
      [account_id]: {
        ...(current[account_id] ?? { id: "", secret: "", note: "" }),
        ...patch,
      },
    }));

  /**
   * The command edits saved configuration, so it runs without a config save:
   * an enable/priority change must not rewrite the declaration the user is
   * still editing.
   */
  const writeInference = async (
    account_id: string,
    key_id: string,
    options: { api_key?: string; priority?: number; enabled?: boolean; note?: string },
  ) => {
    const previous = inferenceWriteChains.current.get(account_id) ?? Promise.resolve(true);
    const pending = previous
      .then((previousOk) =>
        previousOk
          ? run(() => api.setAccountInferenceKey(provider.id, account_id, key_id, options))
          : false,
      );
    inferenceWriteChains.current.set(account_id, pending);
    const ok = await pending;
    if (inferenceWriteChains.current.get(account_id) === pending) {
      inferenceWriteChains.current.delete(account_id);
    }
    if (!ok) notify("error", t("accounts.inference_store_failed"));
    return ok;
  };

  const reorderInference = async (
    account_id: string,
    order: (typeof accounts)[number]["inference_keys"],
  ) => {
    if (reorderPending.current.has(account_id)) return;
    reorderPending.current.add(account_id);
    setReordering((current) => ({ ...current, [account_id]: true }));
    try {
      // The command changes persisted config. Await every write so each one
      // rebases on the snapshot committed by the previous key's update.
      for (const [index, key] of order.entries()) {
        const priority = index * 10;
        if (!(await writeInference(account_id, key.id, { priority }))) return;
      }
    } finally {
      reorderPending.current.delete(account_id);
      setReordering((current) => ({ ...current, [account_id]: false }));
    }
  };

  const moveInference = (
    account_id: string,
    order: typeof accounts[number]["inference_keys"],
    from: number,
    delta: -1 | 1,
  ) => {
    const to = from + delta;
    if (to < 0 || to >= order.length || reorderPending.current.has(account_id)) return;
    const next = [...order];
    [next[from], next[to]] = [next[to], next[from]];
    void reorderInference(account_id, next);
  };

  const startInferenceDrag = (
    account_id: string,
    ordered: (typeof accounts)[number]["inference_keys"],
    from: number,
  ) => (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (busy || reorderPending.current.has(account_id) || event.button !== 0) return;
    const startY = event.clientY;
    let started = false;
    const hitIndex = (y: number) => {
      for (let index = 0; index < ordered.length; index += 1) {
        const row = inferenceRows.current.get(`${account_id}:${ordered[index].id}`);
        if (!row) continue;
        const rect = row.getBoundingClientRect();
        if (y < rect.top + rect.height / 2) return index;
      }
      return ordered.length;
    };
    const finish = (event: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      if (!started) return;
      const to = hitIndex(event.clientY);
      setDragFrom(null);
      setDropAt(null);
      if (to === from || to === from + 1) return;
      const next = [...ordered];
      const [key] = next.splice(from, 1);
      next.splice(to > from ? to - 1 : to, 0, key);
      void reorderInference(account_id, next);
    };
    const cancel = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      setDragFrom(null);
      setDropAt(null);
    };
    const move = (pointer: PointerEvent) => {
      if (!started) {
        if (Math.abs(pointer.clientY - startY) < 4) return;
        started = true;
        setDragFrom({ accountId: account_id, keyId: ordered[from].id });
      }
      pointer.preventDefault();
      setDropAt({ accountId: account_id, index: hitIndex(pointer.clientY) });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", cancel);
  };

  const addInference = async (account_id: string) => {
    const draft = inferenceDraftFor(account_id);
    const id = draft.id.trim();
    if (!id) return;
    const account = accounts.find((a) => a.account_id === account_id);
    if (account?.inference_keys.some((key) => key.id === id)) {
      notify("error", t("accounts.inference_duplicate", { id }));
      return;
    }
    const ok = await writeInference(account_id, id, {
      api_key: draft.secret.trim(),
      note: draft.note.trim(),
    });
    if (ok) editNewInference(account_id, { id: "", secret: "", note: "" });
  };

  const replaceInference = async (account_id: string, key_id: string) => {
    const secret = inferenceDraft.trim();
    if (!secret) return;
    const ok = await writeInference(account_id, key_id, { api_key: secret });
    if (ok) {
      setInferenceDraft("");
      setInferenceKeyFor(null);
    }
  };

  return (
    <Section title={t("accounts.title")} hint={t("accounts.title_hint")}>
      {bindingPreview && <AccountBindingReview preview={bindingPreview} busy={busy}
        publish={(confirmed) => void run(() => api.publishAccountKey(bindingPreview.publication_id, confirmed)).then((ok) => {
          if (ok) { setBindingPreview(null); setKeyFor(null); }
        })}
        discard={() => void api.discardAccountKey(bindingPreview.publication_id).then(() => setBindingPreview(null)).catch((error) => notify("error", errorText(error)))} />}
      {accounts.length === 0 ? (
        <Empty>{t("accounts.empty")}</Empty>
      ) : (
        <div className="list">
          {accounts.map((a) => (
            <div className="account-decl" key={a.account_id}>
              <div className="row-main">
                <span className="row-title mono">{a.account_id}</span>
                {a.binding && <Badge tone={a.binding.verification === "verified" ? "ok" : "warn"}>
                  {t(a.binding.verification === "verified" ? "binding.verified" : "binding.pending")}
                  {a.binding.identity && ` · ${a.binding.identity.username ?? a.binding.identity.user_id} · ${a.binding.identity.group ?? t("common.unknown")}`}
                </Badge>}
                {hasOwnRef(a) ? (
                  keyStored(a) ? (
                    <Badge tone="ok">{t("accounts.own_key")}</Badge>
                  ) : (
                    <Badge tone="warn">{t("accounts.no_key")}</Badge>
                  )
                ) : (
                  <Badge>{t("accounts.follows_provider")}</Badge>
                )}
                <div className="field-actions">
                  <Button
                    kind="ghost"
                    disabled={busy}
                    onClick={() => {
                      setKeyFor(keyFor === a.account_id ? null : a.account_id);
                      setKeyDraft("");
                    }}
                  >
                    {t("accounts.set_key")}
                  </Button>
                  {hasOwnRef(a) && (
                    <Button
                      kind="ghost"
                      disabled={busy}
                      onClick={() =>
                        setConfirm({
                          title: t("accounts.clear_key_title", { id: a.account_id }),
                          body: t("accounts.clear_key_body"),
                          confirmLabel: t("accounts.clear_key"),
                          danger: true,
                          onConfirm: () =>
                            void run(() =>
                              api.clearAccountKey(provider.id, a.account_id),
                            ),
                        })
                      }
                    >
                      {t("accounts.clear_key")}
                    </Button>
                  )}
                  <Button
                    kind="ghost"
                    disabled={busy}
                    onClick={() =>
                      setConfirm({
                        title: t("accounts.remove_title", { id: a.account_id }),
                        body: t("accounts.remove_body"),
                        confirmLabel: t("common.delete"),
                        danger: true,
                        onConfirm: () =>
                          void run(() => api.removeAccount(provider.id, a.account_id)),
                      })
                    }
                  >
                    {t("common.delete")}
                  </Button>
                </div>
              </div>
              {keyFor === a.account_id && (
                <div className="controls">
                  <TextField
                    label={t("accounts.key_label")}
                    hint={t("accounts.key_hint")}
                    value={keyDraft}
                    password
                    onCommit={(v) => {
                      setKeyDraft(v);
                    }}
                  />
                  <TextField label={t("binding.user_hint")} value={userHint} onCommit={setUserHint} />
                  <div className="field-actions">
                    <Button
                      kind="primary"
                      disabled={busy || !keyDraft.trim()}
                      onClick={() => void storeKey(a.account_id)}
                    >
                      {t("accounts.set_key")}
                    </Button>
                  </div>
                </div>
              )}
              <Field label={t("binding.probe_kind")}><Select ariaLabel={t("binding.probe_kind")} value={a.probe_kind ?? "new_api"}
                options={[{ value: "none", label: t("binding.probe_none") }, { value: "new_api", label: t("binding.probe_newapi") }]}
                onChange={(kind) => edit(a.account_id, (account) => { account.probe_kind = kind as "none" | "new_api"; })} /></Field>
              {/*
                * Chat credentials, separate from the panel token above. The
                * list is edited by command rather than by the config save, so
                * a toggle here cannot race the declaration this drawer holds.
                */}
              <div className="inference-pool">
                <div className="pool-head">
                  <span className="pool-title">{t("accounts.inference_title")}</span>
                  <span className="field-hint">{t("accounts.inference_hint")}</span>
                </div>
                {a.inference_keys.length === 0 ? (
                  <Empty>{t("accounts.inference_empty")}</Empty>
                ) : (
                  <div className="key-list">
                    {[...a.inference_keys]
                      .sort(
                        (left, right) =>
                          left.priority - right.priority || left.id.localeCompare(right.id),
                      )
                      .map((key, index, ordered) => {
                      const slot = `${a.account_id}:${key.id}`;
                      const stored = snapshot.keys[key.key_ref] ?? false;
                      const dragging = dragFrom?.accountId === a.account_id && dragFrom.keyId === key.id;
                      const dragIndex = dragFrom?.accountId === a.account_id
                        ? ordered.findIndex((item) => item.id === dragFrom.keyId)
                        : -1;
                      const dropBefore = dropAt?.accountId === a.account_id && dropAt.index === index && dropAt.index !== dragIndex && dropAt.index !== dragIndex + 1;
                      const dropAfter = dropAt?.accountId === a.account_id && dropAt.index === ordered.length && index === ordered.length - 1 && dragIndex !== ordered.length - 1;
                      return (
                        <div
                          className={`key-row ${dragging ? "key-row-dragging" : ""} ${dropBefore ? "key-row-drop-before" : ""} ${dropAfter ? "key-row-drop-after" : ""}`}
                          data-inference-key-id={key.id}
                          key={key.id}
                          ref={(row) => {
                            const rowId = `${a.account_id}:${key.id}`;
                            if (row) inferenceRows.current.set(rowId, row);
                            else inferenceRows.current.delete(rowId);
                          }}
                        >
                          <button
                            type="button"
                            className="inference-drag-handle"
                            aria-label={t("routing.drag_hint")}
                            title={t("routing.drag_hint")}
                            disabled={busy || reordering[a.account_id] || ordered.length < 2}
                            onPointerDown={startInferenceDrag(a.account_id, ordered, index)}
                          >
                            ⋮⋮
                          </button>
                          <div className="key-head">
                            <span className="key-id mono">{key.id}</span>
                            <input
                              className="text-input key-note"
                              style={{ maxWidth: 220 }}
                              defaultValue={key.note}
                              placeholder={t("accounts.inference_note_placeholder")}
                              aria-label={t("accounts.inference_note")}
                              onBlur={(event) => {
                                const note = event.currentTarget.value.trim();
                                if (note !== key.note) {
                                  void writeInference(a.account_id, key.id, { note });
                                }
                              }}
                            />
                            {stored ? (
                              <Badge tone="ok">{t("accounts.inference_stored")}</Badge>
                            ) : (
                              <Badge tone="warn">{t("accounts.inference_missing")}</Badge>
                            )}
                          </div>
                          <div className="key-controls">
                            <Toggle
                              label={t("common.enabled")}
                              checked={key.enabled}
                              onChange={(enabled) =>
                                void writeInference(a.account_id, key.id, { enabled })
                              }
                            />
                            <label
                              className="key-priority"
                              title={t("accounts.inference_priority_hint")}
                            >
                              <span>{t("accounts.inference_priority_label")}</span>
                              <CompactNumber
                                integer
                                min={0}
                                value={key.priority}
                                ariaLabel={t("accounts.inference_priority_label")}
                                onCommit={(v) =>
                                  void writeInference(a.account_id, key.id, {
                                    priority: v ?? 0,
                                  })
                                }
                              />
                            </label>
                          </div>
                          <div className="key-actions">
                              <button
                                type="button"
                                className="btn btn-ghost"
                                aria-label={t("routing.move_up", { model: key.id })}
                                disabled={busy || reordering[a.account_id] || index === 0}
                                onClick={() => moveInference(a.account_id, ordered, index, -1)}
                                onKeyDown={(event) => {
                                  if (event.key === "Enter" || event.key === " ") {
                                    event.preventDefault();
                                    moveInference(a.account_id, ordered, index, -1);
                                  }
                                }}
                              >
                                ↑
                              </button>
                              <button
                                type="button"
                                className="btn btn-ghost"
                                aria-label={t("routing.move_down", { model: key.id })}
                                disabled={busy || reordering[a.account_id] || index === ordered.length - 1}
                                onClick={() => moveInference(a.account_id, ordered, index, 1)}
                                onKeyDown={(event) => {
                                  if (event.key === "Enter" || event.key === " ") {
                                    event.preventDefault();
                                    moveInference(a.account_id, ordered, index, 1);
                                  }
                                }}
                              >
                                ↓
                              </button>
                            <Button
                              kind="ghost"
                              disabled={busy}
                              onClick={() => {
                                setInferenceKeyFor(
                                  inferenceKeyFor === slot ? null : slot,
                                );
                                setInferenceDraft("");
                              }}
                            >
                              {t("accounts.inference_replace")}
                            </Button>
                            <Button
                              kind="ghost"
                              disabled={busy}
                              onClick={() =>
                                setConfirm({
                                  title: t("accounts.inference_remove_title", {
                                    id: key.id,
                                  }),
                                  body: t("accounts.inference_remove_body"),
                                  confirmLabel: t("common.delete"),
                                  danger: true,
                                  onConfirm: () =>
                                    void run(() =>
                                      api.removeAccountInferenceKey(
                                        provider.id,
                                        a.account_id,
                                        key.id,
                                      ),
                                    ),
                                })
                              }
                            >
                              {t("common.delete")}
                            </Button>
                          </div>
                          {inferenceKeyFor === slot && (
                            <div className="key-replace">
                              <TextField
                                label={t("accounts.inference_key_label")}
                                hint={t("accounts.inference_key_hint")}
                                value={inferenceDraft}
                                password
                                onCommit={(v) => setInferenceDraft(v)}
                              />
                              <div className="field-actions">
                                <Button
                                  kind="primary"
                                  disabled={busy || !inferenceDraft.trim()}
                                  onClick={() =>
                                    void replaceInference(a.account_id, key.id)
                                  }
                                >
                                  {t("common.save")}
                                </Button>
                              </div>
                            </div>
                          )}
                        </div>
                      );
                      })}
                  </div>
                )}
                <div className="controls">
                  <TextField
                    label={t("accounts.inference_new_id")}
                    hint={t("accounts.inference_new_id_hint")}
                    value={inferenceDraftFor(a.account_id).id}
                    onCommit={(v) => editNewInference(a.account_id, { id: v })}
                  />
                  <TextField
                    label={t("accounts.inference_new_secret")}
                    hint={t("accounts.inference_key_hint")}
                    value={inferenceDraftFor(a.account_id).secret}
                    password
                    onCommit={(v) => editNewInference(a.account_id, { secret: v })}
                  />
                  <TextField
                    label={t("accounts.inference_note")}
                    value={inferenceDraftFor(a.account_id).note}
                    onCommit={(v) => editNewInference(a.account_id, { note: v })}
                  />
                  <div className="field-actions">
                    <Button
                      kind="primary"
                      disabled={
                        busy ||
                        !inferenceDraftFor(a.account_id).id.trim() ||
                        !inferenceDraftFor(a.account_id).secret.trim()
                      }
                      onClick={() => void addInference(a.account_id)}
                    >
                      {t("accounts.inference_add")}
                    </Button>
                  </div>
                </div>
              </div>
              <div className="controls">
                <Toggle
                  label={t("common.enabled")}
                  checked={a.enabled}
                  onChange={(enabled) => edit(a.account_id, (d) => { d.enabled = enabled; })}
                />
                <Toggle
                  label={t("accounts.checkin")}
                  checked={a.maintenance.checkin_enabled}
                  onChange={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.checkin_enabled = v;
                      if (v) {
                        // NewAPI's dominant layout. Filled only when the field
                        // is empty so an operator's correction is never
                        // overwritten by turning the toggle off and on again.
                        d.maintenance.checkin_path ??= "/console/personal";
                        d.maintenance.checkin_trigger_selector ??= "button.checkin";
                        d.maintenance.login_path ??= "/sign-in";
                      }
                    })
                  }
                />
                <NumberField
                  label={t("accounts.interval")}
                  hint={t("accounts.interval_hint")}
                  min={1}
                  integer
                  value={a.maintenance.checkin_interval_secs}
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.checkin_interval_secs = v;
                    })
                  }
                />
                <TextField
                  label={t("accounts.panel_url")}
                  hint={t("accounts.panel_url_hint")}
                  value={a.maintenance.panel_url ?? ""}
                  placeholder="https://panel.example"
                  wide
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.panel_url = v.trim() || null;
                    })
                  }
                />
                <TextField
                  label={t("accounts.new_api_user")}
                  hint={t("accounts.new_api_user_hint")}
                  value={a.maintenance.new_api_user ?? ""}
                  placeholder="354394"
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.new_api_user = v.trim() || null;
                    })
                  }
                />
                <TextField
                  label={t("accounts.login_path")}
                  hint={t("accounts.login_path_hint")}
                  value={a.maintenance.login_path ?? ""}
                  placeholder="/sign-in"
                  wide
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.login_path = v.trim() || null;
                    })
                  }
                />
                {a.maintenance.checkin_enabled && (
                  <>
                    <TextField
                      label={t("accounts.checkin_path")}
                      hint={t("accounts.checkin_path_hint")}
                      value={a.maintenance.checkin_path ?? ""}
                      placeholder="/console/personal"
                      wide
                      onCommit={(v) =>
                        edit(a.account_id, (d) => {
                          d.maintenance.checkin_path = v.trim() || null;
                        })
                      }
                    />
                    <TextField
                      label={t("accounts.checkin_selector")}
                      hint={t("accounts.checkin_selector_hint")}
                      value={a.maintenance.checkin_trigger_selector ?? ""}
                      wide
                      onCommit={(v) =>
                        edit(a.account_id, (d) => {
                          d.maintenance.checkin_trigger_selector = v.trim() || null;
                        })
                      }
                    />
                  </>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
      <div className="controls">
        <TextField
          label={t("accounts.id_label")}
          hint={t("accounts.id_hint")}
          value={newId}
          onCommit={(v) => setNewId(v)}
        />
        <TextField
          label={t("accounts.key_label_add")}
          hint={t("accounts.key_hint_add")}
          value={newKey}
          password
          onCommit={(v) => setNewKey(v)}
        />
        <div className="field-actions">
          <Button
            kind="primary"
            disabled={busy || !newId.trim()}
            onClick={() => void addAccount()}
          >
            {t("accounts.add")}
          </Button>
        </div>
      </div>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    </Section>
  );
}

/**
 * The credential field, holding its own draft.
 *
 * The draft used to live in `ProviderDrawer`, one component above a catalogue
 * table that can be as long as the provider's own model list — a gateway
 * publishing several hundred models puts several hundred Radix selects in that
 * table. Typing a 40-character key therefore re-rendered all of them forty
 * times, and it measured: 325 ms a keystroke against a 600-row catalogue, where
 * a keystroke into a field that owned its own draft costs one frame. Nothing
 * above this field reads the draft, so the drawer no longer re-renders while a
 * key is being typed.
 *
 * Not [`TextField`]: that commits on blur, and a credential must never reach
 * the store half-written because focus moved.
 */
function ApiKeyField({
  providerId,
  busy,
  run,
}: {
  providerId: string;
  busy: boolean;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState("");

  const saveKey = async () => {
    const value = draft.trim();
    if (!value) return;
    const ok = await run(() => api.setKey(providerId, value));
    if (ok) setDraft("");
  };

  return (
    <Section title={t("providers.api_key")} hint={t("providers.api_key_hint")}>
      <div className="controls">
        <input
          type="password"
          aria-label={t("providers.api_key")}
          autoComplete="off"
          placeholder="sk-…"
          value={draft}
          onChange={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && void saveKey()}
        />
        <Button kind="primary" onClick={saveKey} disabled={busy || !draft.trim()}>
          {t("providers.save_key")}
        </Button>
      </div>
    </Section>
  );
}

/**
 * One provider in full: credential, endpoint, dialect quirks and the models
 * it offers. Saving a key never round-trips the config — it goes to the
 * credential store alone, and the drawer shows only whether one exists.
 */
function ProviderDrawer({
  provider,
  snapshot,
  busy,
  save,
  run,
  onClose,
  onUpdate,
  onRemove,
}: {
  provider: Provider;
  snapshot: Snapshot;
  busy: boolean;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>, kind?: TaskKind) => Promise<boolean>;
  onClose: () => void;
  /** Mutates the provider being committed; see `Providers.update`. */
  onUpdate: (mutate: (provider: Provider) => void) => void;
  onRemove: () => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  // Simple mode collapses the drawer to its facts: what this provider is, how
  // it authenticates, whether it is on. The panels, the probe, the quirks and
  // the account declarations keep existing and keep running — the mode only
  // stops drawing them, and geek mode draws every one again.
  const geek = useMode() === "geek";
  const hasKey = snapshot.keys[provider.id] ?? false;
  // Chat credentials an account declares: the auth row would otherwise say
  // "no key" beside a green dot, because the provider's own reference is empty
  // while the account's inference key is what requests are signed with.
  const accountKeys = (provider.accounts ?? []).reduce(
    (n, account) =>
      n +
      account.inference_keys.filter((key) => snapshot.keys[key.key_ref] ?? false)
        .length,
    0,
  );
  const [checkingBalance, setCheckingBalance] = useState(false);
  // Hoisted so the type is narrowed once rather than at two separate index
  // reads, and so the translation has one string to work with.
  const balanceError = snapshot.balances[provider.id]?.error ?? null;
  // What the chosen probe asks for, said under the dropdown.
  const presetHint = (() => {
    const preset = BALANCE_PRESETS.find((p) => p.id === provider.balance.preset);
    return preset ? t(preset.hintKey as Parameters<typeof t>[0]) : null;
  })();

  /**
   * The provider this instance edits, frozen at mount.
   *
   * The parent keys the drawer by provider id, so a switch remounts it and the
   * key draft never travels to the next provider; reading the id here in the
   * same render keeps the two guarantees in one place.
   */
  const providerId = provider.id;

  /**
   * The address this provider is called at.
   *
   * A relay's base url is its endpoint. A decision endpoint's is the base url
   * plus the path it answers on, and the summary's *Endpoint* row has to say
   * the address the request goes to rather than the base it starts from.
   */
  const endpoint =
    provider.kind === "decision"
      ? decisionUrl(provider.base_url, provider.decision.path)
      : provider.base_url;

  /** How many models this provider contributes, for the summary row. */
  const models = useMemo(
    () => snapshot.config.models.filter((m) => m.provider_id === providerId),
    [snapshot.config.models, providerId],
  );

  /** Wallet holds for this provider, one per paused account or key. */
  const holds = snapshot.wallet_holds.filter((hold) => hold.provider_id === providerId);

  return (
      <Drawer
        title={
          <>
            <StatusDot
              tone={
                holds.length > 0
                  ? "danger"
                  : provider.enabled
                    ? hasKey ||
                      accountKeys > 0 ||
                      provider.key_ref === "" ||
                      !needsKey(provider.kind)
                      ? "ok"
                      : "warn"
                    : "off"
              }
            />
            {provider.name}
          </>
        }
        onClose={onClose}
      >
      {/* A drawer opened for one provider starts with no verdict inherited
          from the surface before it — not from the startup read, not from
          another provider's last save. */}
      <SaveFeedbackReset />
      <KeyValue
        rows={[
          [t("providers.endpoint"), <span className="mono">{endpoint}</span>],
          [t("providers.dialect"), t(kindLabelKey(provider.kind))],
          [
            t("providers.auth"),
            hasKey ? (
              <span className="row gap">
                <span className="mono">••••••••••••</span>
                <Button kind="ghost" onClick={() => void run(() => api.clearKey(provider.id))}>
                  {t("common.remove")}
                </Button>
              </span>
            ) : accountKeys > 0 ? (
              t("providers.auth_account_key")
            ) : provider.key_ref === "" || !needsKey(provider.kind) ? (
              t("providers.no_cred")
            ) : (
              t("providers.no_key")
            ),
          ],
          [
            t("providers.models_section"),
            models.length
              ? t("providers.models_count", { n: models.length })
              : t("providers.models_none"),
          ],
        ]}
      />

      {/*
        The hold lives where the release button does. On the page it was a
        second list of the same providers, sorted by nothing and offering the
        one action the row itself could not; here the subject is already named
        by the drawer, so the block says what happened and how to undo it.
      */}
      {holds.length > 0 && (
        <Section title={t("providers.wallet_holds")} hint={t("providers.wallet_holds_hint")}>
          <div className="list">
            {holds.map((hold) => {
              const scope = [hold.account_id, hold.key_id].filter(Boolean).join(" / ");
              return (
                <div
                  className="list-row"
                  key={`${hold.provider_id}:${hold.account_id ?? ""}:${hold.key_id ?? ""}`}
                  style={{ cursor: "default" }}
                >
                  <StatusDot tone="danger" state={t("providers.wallet_empty")} />
                  <div className="row-main">
                    <span className="row-title">
                      {t("providers.wallet_empty")}
                      {scope && <span className="muted mono"> · {scope}</span>}
                    </span>
                    <span className="row-sub">
                      {t("providers.wallet_held_since", {
                        time: new Date(hold.held_at * 1000).toLocaleString(),
                      })}
                    </span>
                  </div>
                  <Button
                    kind="ghost"
                    disabled={busy}
                    onClick={() =>
                      // Releasing a hold is a router runtime action, not a
                      // configuration write.
                      void run(
                        () =>
                          api.releaseWalletHold(hold.provider_id, hold.account_id, hold.key_id),
                        "observe",
                      )
                    }
                  >
                    {t("providers.wallet_release")}
                  </Button>
                </div>
              );
            })}
          </div>
        </Section>
      )}
      {/* A free provider has nothing to paste: the field asks for a credential
          the kind cannot use, and the row beside it already says so. */}
      {!hasKey && needsKey(provider.kind) && provider.key_ref !== "" && (
        <ApiKeyField providerId={providerId} busy={busy} run={run} />
      )}

      {geek && snapshot.newapi_available && (
        <NewApiPanel provider={provider} models={models} busy={busy} run={run} />
      )}

      {geek ? (
      <Section title={t("providers.configuration")}>
        <div className="controls">
          <TextField
            label={t("field.name")}
            value={provider.name}
            onCommit={(name) =>
              name.trim() &&
              onUpdate((p) => {
                p.name = name;
              })
            }
          />
          <TextField
            label={t("field.base_url")}
            hint={t("field.base_url_hint")}
            value={provider.base_url}
            onCommit={(base_url) =>
              base_url.trim() &&
              onUpdate((p) => {
                p.base_url = base_url;
              })
            }
            wide
          />
          <Field label={t("field.api_dialect")}>
            <Segment
              ariaLabel={t("field.api_dialect")}
              value={provider.kind}
              onChange={(kind) =>
                onUpdate((p) => {
                  const previousDefault = defaultBaseUrl(p.kind);
                  p.kind = kind;
                  // The profile is not free alongside the kind: config.rs
                  // refuses an OpenCode provider that is not `opencode_cli`,
                  // and Claude Code is only the sensible pairing for an
                  // Anthropic endpoint. Re-derive it so switching kind in the
                  // drawer cannot leave a config that will not validate.
                  p.client_profile = defaultClientProfile(kind);
                  // Follow the new endpoint only if the base URL is still the
                  // one the old kind defaulted to; a hand-edited one is left
                  // alone.
                  if (p.base_url.trim() === previousDefault) {
                    p.base_url = defaultBaseUrl(kind);
                  }
                })
              }
              options={kindsFor(snapshot.opencode_available).map((k) => ({ value: k.id, label: t(k.labelKey) }))}
            />
          </Field>
          {isOpencodeKind(provider.kind) || provider.kind === "openai_compatible" ? (
            <Field label={t("providers.api_surface")} hint={t("providers.api_surface_hint")}>
              <Segment
                ariaLabel={t("providers.api_surface")}
                value={provider.api_surface}
                onChange={(surface) =>
                  onUpdate((p) => {
                    p.api_surface = surface;
                  })
                }
                options={[
                  { value: "chat", label: t("providers.surface_chat") },
                  { value: "responses", label: t("providers.surface_responses") },
                ]}
              />
            </Field>
          ) : null}
          <NumberField
            label={t("field.timeout")}
            hint={t("field.timeout_hint")}
            min={1}
            integer
            value={provider.timeout_secs}
            onCommit={(timeout_secs) =>
              onUpdate((p) => {
                p.timeout_secs = timeout_secs ?? 600;
              })
            }
          />
          <NumberField
            label={t("field.connect_timeout")}
            min={1}
            integer
            value={provider.connect_timeout_secs}
            onCommit={(connect_timeout_secs) =>
              onUpdate((p) => {
                p.connect_timeout_secs = connect_timeout_secs ?? 15;
              })
            }
          />
          <Field label={t("providers.proxy_policy")} hint={t("providers.proxy_policy_hint")}>
            <Select
              ariaLabel={t("providers.proxy_policy")}
              value={provider.bypass_proxy == null ? "inherit" : provider.bypass_proxy ? "direct" : "system"}
              onChange={(value) => onUpdate((p) => {
                p.bypass_proxy = value === "inherit" ? null : value === "direct";
              })}
              options={[
                { value: "inherit", label: t("providers.proxy_inherit") },
                { value: "direct", label: t("providers.proxy_direct") },
                { value: "system", label: t("providers.proxy_system") },
              ]}
            />
          </Field>
          {provider.kind === "anthropic" && (
            <>
              {/*
                The dated versions are mutually exclusive wire contracts, not
                free-text: 2026-10-21 removes the sampling parameters and
                confines documents, and an arbitrary string the upstream has
                never met answers 400 for a reason the operator cannot see. Two
                segments — the three-year baseline and the 2026 revision — so
                the choice can only name a shape the encoder actually
                implements.
              */}
              <Field label={t("providers.f_version")} hint={t("providers.f_version_hint")}>
                <Segment
                  ariaLabel={t("providers.f_version")}
                  value={provider.anthropic_version ?? "2023-06-01"}
                  onChange={(version) =>
                    onUpdate((p) => {
                      p.anthropic_version = version === "2023-06-01" ? null : version;
                    })
                  }
                  options={[
                    { value: "2023-06-01", label: "2023-06-01" },
                    { value: "2026-10-21", label: "2026-10-21" },
                  ]}
                />
              </Field>
              <TextField
                label={t("providers.f_beta")}
                hint={t("providers.f_beta_hint")}
                value={provider.extra_headers["anthropic-beta"] ?? ""}
                placeholder="context-management-2025-06-27"
                onCommit={(v) =>
                  onUpdate((p) => {
                    const trimmed = v.trim();
                    if (trimmed) p.extra_headers["anthropic-beta"] = trimmed;
                    else delete p.extra_headers["anthropic-beta"];
                  })
                }
              />
            </>
          )}
        </div>
        <div className="grid-two">
          <Toggle
            label={t("common.enabled")}
            checked={provider.enabled}
            onChange={(enabled) =>
              onUpdate((p) => {
                p.enabled = enabled;
              })
            }
          />
          <Field
            label={t("providers.client_profile")}
            hint={
              isOpencodeKind(provider.kind)
                ? t("providers.client_profile_locked_hint")
                : t("providers.client_profile_hint")
            }
          >
            <Select
              ariaLabel={t("providers.client_profile")}
              value={provider.client_profile ?? "auto"}
              // The OpenCode gateway checks a fixed fingerprint, so config.rs
              // refuses any other profile on these kinds. Offering the others
              // here would only offer a choice that fails to save.
              disabled={isOpencodeKind(provider.kind)}
              onChange={(client_profile) =>
                onUpdate((p) => {
                  p.client_profile = client_profile;
                })
              }
              options={
                isOpencodeKind(provider.kind)
                  ? CLIENT_PROFILES.filter((c) => c.id === "opencode_cli").map((c) => ({
                      value: c.id,
                      label: t(c.labelKey),
                    }))
                  : clientProfilesFor(snapshot.opencode_available).map((c) => ({ value: c.id, label: t(c.labelKey) }))
              }
            />
          </Field>
          {provider.kind === "anthropic" && (
            <Toggle
              label={t("providers.bearer_auth")}
              hint={t("providers.bearer_auth_hint")}
              checked={provider.bearer_auth}
              onChange={(bearer_auth) =>
                onUpdate((p) => {
                  p.bearer_auth = bearer_auth;
                })
              }
            />
          )}
        </div>
      </Section>
      ) : (
        /* Simple mode keeps the one switch that decides whether this provider
           serves traffic, and nothing else from the configuration block. */
        <div className="grid-two">
          <Toggle
            label={t("common.enabled")}
            checked={provider.enabled}
            onChange={(enabled) =>
              onUpdate((p) => {
                p.enabled = enabled;
              })
            }
          />
        </div>
      )}

      {geek && (
      <Section
        title={t("providers.balance")}
        hint={
          hasPanelBalance(provider, snapshot)
            ? t("providers.balance_hint_panel")
            : provider.balance.preset !== "none"
              ? t("providers.balance_hint")
              : t("providers.balance_hint_none")
        }
      >
        <div className="controls">
          <Field label={t("field.probe")}>
            <Select
              ariaLabel={t("field.probe")}
              value={provider.balance.preset}
              onChange={(preset) =>
                onUpdate((p) => {
                  p.balance = {
                    preset,
                    custom:
                      preset === "custom" ? p.balance.custom ?? defaultProbe() : null,
                  };
                })
              }
              options={BALANCE_PRESETS.map((p) => ({
                value: p.id,
                label: t(p.labelKey as Parameters<typeof t>[0]),
              }))}
            />
          </Field>
          {/*
            * What the chosen probe asks for. The data was in the table all
            * along and nothing showed it, so the operator had to know that
            * "DeepSeek" meant "/user/balance" by recognising the provider.
            */}
          {presetHint && <p className="field-hint">{presetHint}</p>}
          <div className="field-actions">
            <Button
              kind="ghost"
              onClick={() => {
                setCheckingBalance(true);
                void run(() => api.refreshBalance(provider.id), "observe")
                  .then((ok) => {
                    // The probe's own failure is recorded in the snapshot and
                    // rendered beside the balance below, with the last reading
                    // kept; a rejection means the check never ran at all.
                    if (!ok) notify("error", t("toast.balances_refresh_failed"));
                  })
                  .finally(() => setCheckingBalance(false));
              }}
              disabled={busy || checkingBalance || !hasBalanceSource(provider, snapshot)}
              title={
                hasBalanceSource(provider, snapshot)
                  ? undefined
                  : t("providers.balance_no_source")
              }
            >
              {checkingBalance ? t("providers.checking") : t("providers.check_now")}
            </Button>
          </div>
        </div>
        {balanceError && (
          <p className="field-hint">{describeBackendError(t, balanceError)}</p>
        )}
      </Section>
      )}

      {/* A decision endpoint is asked a question, not a chat, so it has settings
          of its own: where the decision endpoint lives, and how sure the answer
          has to be before the Auto Mode acts on it. */}
      {geek && provider.kind === "decision" && (
        <Section
          title={t("providers.decision")}
          hint={t("providers.decision_hint")}
        >
          <div className="grid-two">
            <Field label={t("providers.decision_path")} hint={t("providers.decision_path_hint")}>
              <input
                value={provider.decision.path}
                onChange={(e) =>
                  onUpdate((p) => {
                    p.decision.path = e.currentTarget.value;
                  })
                }
              />
            </Field>
            <Field
              label={t("providers.decision_threshold")}
              hint={t("providers.decision_threshold_hint")}
            >
              <input
                type="number"
                min={0.5}
                max={1}
                step={0.05}
                value={provider.decision.threshold}
                onChange={(e) =>
                  onUpdate((p) => {
                    const parsed = Number(e.currentTarget.value);
                    p.decision.threshold = Number.isFinite(parsed) ? parsed : 0.85;
                  })
                }
              />
            </Field>
          </div>
        </Section>
      )}

      {geek && (
      <Section title={t("providers.compatibility")} hint={t("providers.compatibility_hint")}>
        <div className="grid-two">
          <Toggle
            label={t("quirk.max_completion_tokens")}
            checked={provider.quirks.use_max_completion_tokens}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.use_max_completion_tokens = v;
              })
            }
          />
          <Toggle
            label={t("quirk.drop_temperature")}
            checked={provider.quirks.drop_temperature}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.drop_temperature = v;
              })
            }
          />
          <Toggle
            label={t("quirk.drop_top_p")}
            checked={provider.quirks.drop_top_p}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.drop_top_p = v;
              })
            }
          />
          <Toggle
            label={t("quirk.drop_stop")}
            hint={t("quirk.drop_stop_hint")}
            checked={provider.quirks.drop_stop}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.drop_stop = v;
              })
            }
          />
          <Toggle
            label={t("quirk.system_as_developer")}
            checked={provider.quirks.system_as_developer}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.system_as_developer = v;
              })
            }
          />
          <Toggle
            label={t("quirk.reasoning_effort")}
            checked={provider.quirks.send_reasoning_effort}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.send_reasoning_effort = v;
              })
            }
          />
          <div className="grid-span">
            <Toggle
              label={t("quirk.portable_history")}
              hint={t("quirk.portable_history_hint")}
              checked={provider.quirks.portable_history}
              onChange={(v) =>
                onUpdate((p) => {
                  p.quirks.portable_history = v;
                })
              }
            />
          </div>
        </div>
      </Section>
      )}

      {/*
        Account declarations are offered only when the build actually contains
        the account subsystem, for the same reason the maintenance section
        below asks first: a command that was never registered looks exactly
        like one that failed.
      */}
      {geek && snapshot.account_available && (
        <AccountSection
          provider={provider}
          snapshot={snapshot}
          busy={busy}
          save={save}
          run={run}
          onUpdate={onUpdate}
        />
      )}

      {/* Account maintenance — the runtime face — moved to the Accounts page.
          This drawer keeps the declaration, which is the part that changes when
          the user edits configuration rather than when the account is used. */}

      <div>
        <Button kind="danger" disabled={busy} onClick={onRemove}>
          {t("providers.remove")}
        </Button>
      </div>
    </Drawer>
  );
}

