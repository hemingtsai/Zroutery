import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  BALANCE_PRESETS,
  defaultProbe,
  errorText,
  money,
  previewId,
  slugify,
  virtualId,
  TIERS,
  type Account,
  type AppConfig,
  type BalanceStatus,
  type CcProviderDraft,
  type CcSwitchPreview,
  type DiscoveredModel,
  type Maintenance,
  type ModelTier,
  type Provider,
  type ProviderClientProfile,
  type ProviderKind,
  type Snapshot,
  tierName,
} from "../api";
import { describeBackendError } from "../i18n";
import {
  Badge,
  Button,
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
  return trimmed.startsWith("/v1") ? `${base}${trimmed}` : `${base}/v1${trimmed}`;
}

/**
 * Mirrors the `client_profile` arm of `ProviderConfig::new`.
 *
 * The OpenCode arm is not a preference: `config.rs` rejects any OpenCode
 * provider whose configured profile is not `opencode_cli`, so creating or
 * re-kinding one without it produces a config that will not validate.
 */
function defaultClientProfile(kind: ProviderKind): ProviderClientProfile {
  switch (kind) {
    case "anthropic":
      return "claude_code";
    case "opencode_zen_free":
    case "opencode_console":
      return "opencode_cli";
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
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
}) {
  const { config, keys, balances } = snapshot;
  const { t, plural } = useI18n();
  const notify = useToast();
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);

  const [ccPreview, setCcPreview] = useState<CcSwitchPreview | null>(null);
  const [ccLoading, setCcLoading] = useState(false);
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
                onClick={() => void run(api.refreshBalances)}
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

      {config.providers.length === 0 ? (
        <div className="empty-state">
          <p>{t("providers.empty")}</p>
          <p className="muted">{t("providers.empty_hint")}</p>
        </div>
      ) : (
        <div className="list">
          {config.providers.map((p) => (
            <ProviderRow
              key={p.id}
              id={p.id}
              name={p.name}
              baseUrl={p.base_url}
              kind={p.kind}
              modelCount={modelCounts.get(p.id) ?? 0}
              status={!p.enabled ? "off" : keys[p.id] || p.key_ref === "" ? "ok" : "warn"}
              selected={openId === p.id}
              showBalance={hasBalanceSource(p, snapshot)}
              balance={balances[p.id]}
              onOpen={openProvider}
            />
          ))}
        </div>
      )}

      {/* The account probe moved to the Accounts page: it is a runtime reading
          over every provider's accounts, not a property of this page's list, and
          the drawer never had a good place to put a cross-provider report. */}

      <AddProviderForm
        providers={config.providers}
        save={save}
        busy={busy}
        opencodeAvailable={snapshot.opencode_available}
        onAdded={openProvider}
      />

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
                  <th>{t("cc.import")}</th>
                  <th>{t("field.name")}</th>
                  <th>{t("providers.endpoint")}</th>
                  <th>{t("nav.models")}</th>
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
  onOpen: (id: string) => void;
}) {
  const { t } = useI18n();
  return (
    <button type="button"
      className={`list-row ${selected ? "selected" : ""}`}
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
        status === "off"
          ? t("status.disabled")
          : status === "warn"
            ? t("status.no_key")
            : t("status.enabled")
      }`}
    >
      <StatusDot
        tone={status}
        state={
          status === "off"
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
 * The add-a-provider form, with its own drafts.
 *
 * The drafts used to live in the page above, so typing a provider name
 * re-rendered the whole list — and every row of it re-counted the models it
 * owns, which is a walk over every model in the configuration. Nothing above
 * this form reads a draft, so the form owns them now and a keystroke costs
 * one input.
 *
 * The form is still the same form: the same fields, in the same order, with
 * the same names, and the provider it adds still opens its own drawer.
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

  const addProvider = () => {
    const name = newName.trim();
    if (!name) return;
    const id = slugify(name);
    if (providers.some((p) => p.id === id)) {
      setNameError(t("providers.duplicate", { id }));
      return;
    }
    void save((cfg) => {
      if (cfg.providers.some((p) => p.id === id)) {
        setNameError(t("providers.duplicate", { id }));
        return null;
      }
      const next = structuredClone(cfg);
      next.providers.push({
        id,
        name: name.trim(),
        kind: newKind,
        base_url: newBaseUrl.trim() || defaultBaseUrl(newKind),
        key_ref: `provider:${id}`,
        extra_headers: {},
        client_profile: defaultClientProfile(newKind),
        bearer_auth: false,
        enabled: true,
        timeout_secs: 600,
        connect_timeout_secs: 15,
        anthropic_version: null,
        balance: { preset: "none", custom: null },
        decision: { path: "/v1/systemone", threshold: 0.85 },
        quirks: {
          use_max_completion_tokens: false,
          drop_temperature: false,
          drop_top_p: false,
          drop_stop: false,
          stream_usage: true,
          system_as_developer: false,
          send_reasoning_effort: false,
        },
      });
      return next;
    });
    setNewName("");
    setNewBaseUrl("");
    setNameError(null);
    onAdded(id);
  };

  return (
    <Section title={t("providers.add_section")} hint={t("providers.add_hint")}>
      <div className="controls">
        <Field label={t("field.name")} danger={Boolean(nameError)} hint={nameError ?? undefined}>
          <input
            value={newName}
            placeholder="DeepSeek"
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
          <Button kind="primary" onClick={addProvider} disabled={busy || !newName.trim()}>
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
 * filled by the reconciler. Requests do not route by account (the serving path
 * resolves one credential per provider), which the hint says rather than hides:
 * what declaring an account buys is probing, per-account balances and check-in.
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
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);

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
        enabled: true,
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
    const ok = await run(() => api.setAccountKey(provider.id, account_id, key));
    if (ok) {
      setKeyDraft("");
      setKeyFor(null);
    }
  };

  return (
    <Section title={t("accounts.title")} hint={t("accounts.title_hint")}>
      {accounts.length === 0 ? (
        <Empty>{t("accounts.empty")}</Empty>
      ) : (
        <div className="list">
          {accounts.map((a) => (
            <div className="account-decl" key={a.account_id}>
              <div className="row-main">
                <span className="row-title mono">{a.account_id}</span>
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
                      void storeKey(a.account_id);
                    }}
                  />
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
                {a.maintenance.checkin_enabled && (
                  <>
                    <TextField
                      label={t("accounts.checkin_path")}
                      hint={t("accounts.checkin_path_hint")}
                      value={a.maintenance.checkin_path ?? ""}
                      placeholder="/checkin"
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
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  onClose: () => void;
  /** Mutates the provider being committed; see `Providers.update`. */
  onUpdate: (mutate: (provider: Provider) => void) => void;
  onRemove: () => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const hasKey = snapshot.keys[provider.id] ?? false;
  const [discovered, setDiscovered] = useState<DiscoveredModel[] | null>(null);
  const [discovering, setDiscovering] = useState(false);
  const [checkingBalance, setCheckingBalance] = useState(false);
  // Hoisted so the type is narrowed once rather than at two separate index
  // reads, and so the translation has one string to work with.
  const balanceError = snapshot.balances[provider.id]?.error ?? null;
  // What the chosen probe asks for, said under the dropdown.
  const presetHint = (() => {
    const preset = BALANCE_PRESETS.find((p) => p.id === provider.balance.preset);
    return preset ? t(preset.hintKey as Parameters<typeof t>[0]) : null;
  })();
  // The tier each catalogue row is about to be added as. Keys are the discovered
  // model ids; "" means "no class yet" — the model lands unclassified and the
  // Models page banner keeps counting it until a class is assigned.
  const [tierChoice, setTierChoice] = useState<Record<string, ModelTier | "none">>({});
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  // The free tier's catalogue is the whole Zen list, paid models included, so
  // this defaults on for that one kind: the operator is looking for the free
  // models and the paid ones can only fail on an unauthenticated provider.
  const [freeOnly, setFreeOnly] = useState(provider.kind === "opencode_zen_free");
  // Which catalogue request is allowed to answer. Switching providers (or
  // closing the drawer) bumps it, so a slow response for the provider that was
  // open a moment ago cannot land in the current one's list.
  const discoveryGeneration = useRef(0);
  useEffect(
    () => () => {
      discoveryGeneration.current += 1;
    },
    [],
  );

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

  /**
   * The provider's own models, and the names they answer to.
   *
   * The catalogue table asks "is this model already added?" once per
   * published model, and the answer used to be a scan of the provider's own
   * list: one lookup per row, so one render cost O(published x configured).
   * A set answers it in one step, and the two answers are the same.
   */
  const models = useMemo(
    () => snapshot.config.models.filter((m) => m.provider_id === providerId),
    [snapshot.config.models, providerId],
  );
  const configuredNames = useMemo(
    () => new Set(models.map((m) => m.upstream_model)),
    [models],
  );

  /**
   * The tier menus, once.
   *
   * Both tables offer the same tiers under the same naming style, and each
   * row used to build the menu for itself: `virtualId` allocates its own
   * lookup table on every call, so a catalogue of six hundred models built
   * six hundred copies of it, twice, on every render of the drawer. One
   * array per table, rebuilt when the naming style changes, is the whole
   * menu.
   */
  const namingStyle = snapshot.config.routing.naming_style;
  const modelTierOptions = useMemo<{ value: ModelTier | "none"; label: string }[]>(
    () => [
      { value: "none", label: t("models.no_tier") },
      ...TIERS.map((c) => ({ value: c, label: virtualId(c, namingStyle) })),
    ],
    [namingStyle, t],
  );
  const catalogueTierOptions = useMemo<{ value: ModelTier | "none"; label: string }[]>(
    () => [
      { value: "none", label: t("models.no_tier") },
      ...TIERS.map((c) => ({ value: c, label: virtualId(c, namingStyle) })),
    ],
    [namingStyle, t],
  );

  /**
   * What the catalogue table shows.
   *
   * The filter reads `free` off the entry rather than inferring it from a price:
   * a source that publishes no price for a genuinely free model leaves
   * `pricing` null, so "no price" and "free" are not the same test.
   */
  const visibleDiscovered = useMemo(
    () => (discovered ?? []).filter((d) => !freeOnly || d.free),
    [discovered, freeOnly],
  );

  /** The catalogue by name, so a row can be added without a second search. */
  const discoveredById = useMemo(
    () => new Map((discovered ?? []).map((d) => [d.id, d])),
    [discovered],
  );

  const discover = async () => {
    const generation = ++discoveryGeneration.current;
    setDiscovering(true);
    try {
      const ids = await api.fetchModels(provider);
      if (generation !== discoveryGeneration.current) return;
      setDiscovered(ids);
      if (ids.length === 0) notify("error", t("providers.empty_catalogue", { name: provider.name }));
    } catch (e) {
      if (generation !== discoveryGeneration.current) return;
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      if (generation === discoveryGeneration.current) setDiscovering(false);
    }
  };

  const addDiscovered = useCallback(
    (id: string, tier: ModelTier | "none") => {
      const model = discoveredById.get(id);
      if (!model) return;
      // Adding a model is a config mutation; the provider page reaches into the
      // same save pipeline every other page uses. The tier rides along so an
      // operator who knows the class does not have to visit the Models page to
      // finish the sentence.
      void save((cfg) => {
        if (
          cfg.models.some(
            (m) => m.provider_id === providerId && m.upstream_model === model.id,
          )
        ) {
          notify("error", t("models.duplicate", { model: model.id }));
          return null;
        }
        const next = structuredClone(cfg);
        next.models.push({
          provider_id: providerId,
          upstream_model: model.id,
          tier: tier === "none" ? null : tier,
          priority: 0,
          weight: 1,
          enabled: true,
          capabilities: {
            vision: false,
            tools: true,
            thinking: false,
            structured_output: false,
            audio: false,
            video: false,
            files: false,
          },
          display_name: null,
          aliases: [],
          max_output_tokens: null,
          pricing: model.pricing,
        });
        return next;
      });
    },
    [save, providerId, discoveredById, notify, t],
  );

  /**
   * The tier a catalogue row is about to be added as.
   *
   * The choice lives in a map keyed by model id, so a row that has not been
   * touched reads "" — "no class yet", which lands the model unclassified —
   * and the Models page banner keeps counting it until a class is assigned.
   */
  const chooseTier = useCallback((id: string, tier: ModelTier | "none") => {
    setTierChoice((current) => ({ ...current, [id]: tier }));
  }, []);

  /**
   * The tier of a model this provider already has.
   *
   * Models live at the config level, not on the provider, so the class edit
   * is its own save rather than a provider-field mutation.
   */
  const setModelTier = useCallback(
    (upstreamModel: string, next: ModelTier | "none") => {
      void save((cfg) => {
        const nextCfg = structuredClone(cfg);
        const entry = nextCfg.models.find(
          (e) => e.provider_id === providerId && e.upstream_model === upstreamModel,
        );
        if (!entry) return null;
        entry.tier = next === "none" ? null : next;
        return nextCfg;
      });
    },
    [save, providerId],
  );

  /** Remove one configured model, with the confirmation the list row uses. */
  const removeModel = useCallback(
    (upstreamModel: string) => {
      const model = models.find((m) => m.upstream_model === upstreamModel);
      if (!model) return;
      setConfirm({
        title: t("confirm.remove_model"),
        body: t("confirm.remove_model_body"),
        confirmLabel: t("models.remove"),
        danger: true,
        onConfirm: () =>
          void save((cfg) => {
            const index = cfg.models.findIndex(
              (m) => m.provider_id === providerId && m.upstream_model === upstreamModel,
            );
            if (index < 0) return null;
            const next = structuredClone(cfg);
            // The Auto Mode pool names models by exposed id or alias; deleting the
            // model deletes its pool membership with it, so the reference cannot
            // dangle behind the removal.
            const gone = [
              previewId(providerId, model.upstream_model),
              ...model.aliases,
            ];
            next.models.splice(index, 1);
            next.classifier.candidates = next.classifier.candidates.filter(
              (c) => !gone.includes(c.model),
            );
            return next;
          }),
      });
    },
    [models, providerId, save, t],
  );

  return (
    <Drawer
      title={
        <>
          <StatusDot tone={provider.enabled ? (hasKey || provider.key_ref === "" ? "ok" : "warn") : "off"} />
          {provider.name}
        </>
      }
      onClose={onClose}
    >
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
            ) : provider.key_ref === "" ? (
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

      {!hasKey && provider.key_ref !== "" && (
        <ApiKeyField providerId={providerId} busy={busy} run={run} />
      )}

      <Section title={t("providers.models_section")} hint={t("providers.models_hint")}>
        {models.length > 0 ? (
          <table className="table">
            <thead>
              <tr>
                <th>{t("field.model_name")}</th>
                <th>{t("models.tier")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {models.map((m) => (
                <ConfiguredModelRow
                  key={m.upstream_model}
                  providerId={providerId}
                  upstreamModel={m.upstream_model}
                  tier={m.tier ?? "none"}
                  options={modelTierOptions}
                  disabled={busy}
                  tierLocked={provider.kind === "decision"}
                  onTier={setModelTier}
                  onRemove={removeModel}
                />
              ))}
            </tbody>
          </table>
        ) : (
          <Empty>{t("providers.models_none")}</Empty>
        )}
        {/* A decision endpoint publishes no catalogue: it answers questions, it
            does not serve models. So there is nothing to fetch, and the model is
            declared by hand on the models page. */}
        {provider.kind === "decision" ? (
          <>
            {/* The tier is not this model's to choose — the router excludes a
                decision model from every tier pool — and a control that refuses
                for no visible reason is a control that looks broken. */}
            <p className="field-hint">{t("models.tier_decision_hint")}</p>
            <p className="field-hint">{t("providers.decision_no_catalogue")}</p>
          </>
        ) : (
          <div className="row gap wrap">
            <Button
              kind="ghost"
              ariaLabel={t("providers.fetch")}
              onClick={() => void discover()}
              disabled={busy || discovering}
            >
              {discovering ? t("providers.discovering") : t("providers.fetch")}
            </Button>
          </div>
        )}
        {discovered && discovered.length > 0 && (
          <>
            <div className="controls">
              <Toggle
                label={t("providers.free_only")}
                hint={t("providers.free_only_hint")}
                checked={freeOnly}
                onChange={setFreeOnly}
              />
            </div>
            <p className="muted">
              {freeOnly
                ? t("providers.catalogue_shown", {
                    shown: visibleDiscovered.length,
                    total: discovered.length,
                  })
                : t("providers.catalogue_total", { total: discovered.length })}
            </p>
            {visibleDiscovered.length > 0 ? (
              <table className="table">
                <thead>
                  <tr>
                    <th>{t("field.model_name")}</th>
                    <th>{t("models.price")}</th>
                    <th>{t("models.tier")}</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {visibleDiscovered.map((d) => (
                    <DiscoveredModelRow
                      key={d.id}
                      id={d.id}
                      pricing={d.pricing}
                      free={d.free}
                      tier={tierChoice[d.id] ?? "none"}
                      options={catalogueTierOptions}
                      added={configuredNames.has(d.id)}
                      onTier={chooseTier}
                      onAdd={addDiscovered}
                    />
                  ))}
                </tbody>
              </table>
            ) : (
              <div className="empty-state">
                <p>{t("providers.catalogue_no_free", { total: discovered.length })}</p>
              </div>
            )}
          </>
        )}
        <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
      </Section>

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
          {provider.kind === "anthropic" && (
            <TextField
              label={t("providers.f_version")}
              hint={t("providers.f_version_hint")}
              value={provider.anthropic_version ?? ""}
              placeholder="2023-06-01"
              onCommit={(v) =>
                onUpdate((p) => {
                  p.anthropic_version = v || null;
                })
              }
            />
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
                void run(() => api.refreshBalance(provider.id)).finally(() =>
                  setCheckingBalance(false),
                );
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

      {/* A decision endpoint is asked a question, not a chat, so it has settings
          of its own: where the decision endpoint lives, and how sure the answer
          has to be before the Auto Mode acts on it. */}
      {provider.kind === "decision" && (
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
        </div>
      </Section>

      {/*
        Account declarations are offered only when the build actually contains
        the account subsystem, for the same reason the maintenance section
        below asks first: a command that was never registered looks exactly
        like one that failed.
      */}
      {snapshot.account_available && (
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

/**
 * One model this provider already has, memoised.
 *
 * A row of this table carries a select, and a select is the most expensive
 * thing in the drawer. The row therefore takes the menu it offers, the tier
 * it currently holds and two stable callbacks — so a change to one row's
 * tier, or the confirmation that opens when a row is removed, leaves every
 * other row's select untouched.
 *
 * The markup is the row's own three cells, unchanged.
 */
const ConfiguredModelRow = memo(function ConfiguredModelRow({
  providerId,
  upstreamModel,
  tier,
  options,
  disabled,
  tierLocked,
  onTier,
  onRemove,
}: {
  providerId: string;
  upstreamModel: string;
  tier: ModelTier | "none";
  options: { value: ModelTier | "none"; label: string }[];
  disabled: boolean;
  /** Whether the tier is not this model's to choose.
   *
   * A decision model answers the Auto Mode's questions and cannot answer a
   * request that wants prose, so a tier would be a promise the router cannot
   * keep. The control is disabled rather than hidden, so that the row still
   * shows what the model is and where the tier would have gone. */
  tierLocked: boolean;
  onTier: (upstreamModel: string, tier: ModelTier | "none") => void;
  onRemove: (upstreamModel: string) => void;
}) {
  const { t } = useI18n();
  return (
    <tr>
      <td className="mono">{previewId(providerId, upstreamModel)}</td>
      <td>
        <Select
          ariaLabel={t("models.tier_for", { model: previewId(providerId, upstreamModel) })}
          value={tier}
          disabled={disabled || tierLocked}
          onChange={(next) => onTier(upstreamModel, next)}
          options={options}
        />
      </td>
      <td>
        <Button
          kind="ghost"
          ariaLabel={t("models.remove")}
          disabled={disabled}
          onClick={() => onRemove(upstreamModel)}
        >
          {t("common.remove")}
        </Button>
      </td>
    </tr>
  );
});

/**
 * One model the provider publishes, memoised.
 *
 * The catalogue is the largest thing in the drawer: a gateway can publish
 * several hundred models, and every one of them carries a select. The row
 * takes its price, its free flag, the tier it is about to be added as and
 * whether it is already configured — all of them values that change on their
 * own — so choosing a tier in one row does not re-render the other five
 * hundred.
 *
 * The markup is the row's own four cells, unchanged.
 */
const DiscoveredModelRow = memo(function DiscoveredModelRow({
  id,
  pricing,
  free,
  tier,
  options,
  added,
  onTier,
  onAdd,
}: {
  id: string;
  pricing: DiscoveredModel["pricing"];
  free: boolean;
  tier: ModelTier | "none";
  options: { value: ModelTier | "none"; label: string }[];
  added: boolean;
  onTier: (id: string, tier: ModelTier | "none") => void;
  onAdd: (id: string, tier: ModelTier | "none") => void;
}) {
  const { t } = useI18n();
  return (
    <tr>
      <td className="mono">
        {id} {free && <Badge tone="ok">{t("providers.free_badge")}</Badge>}
      </td>
      <td className="muted">
        {pricing
          ? `${pricing.input_per_mtok} / ${pricing.output_per_mtok} ${pricing.currency}`
          : "—"}
      </td>
      <td>
        <Select
          ariaLabel={t("models.tier_for", { model: id })}
          value={tier}
          onChange={(next) => onTier(id, next)}
          options={options}
        />
      </td>
      <td>
        <Button kind="ghost" onClick={() => onAdd(id, tier)} disabled={added}>
          {t("cc.add")}
        </Button>
      </td>
    </tr>
  );
});
