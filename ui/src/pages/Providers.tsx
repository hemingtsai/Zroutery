import { useEffect, useRef, useState } from "react";
import {
  api,
  BALANCE_PRESETS,
  defaultProbe,
  errorText,
  money,
  previewId,
  slugify,
  type AppConfig,
  type BalanceStatus,
  type CcProviderDraft,
  type CcSwitchPreview,
  type CheckinView,
  type DiscoveredModel,
  type Maintenance,
  type Provider,
  type ProviderClientProfile,
  type ProviderKind,
  type Snapshot,
} from "../api";
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

const KINDS: { id: ProviderKind; labelKey: "providers.openai_dialect" | "providers.anthropic_dialect" }[] = [
  { id: "openai_compatible", labelKey: "providers.openai_dialect" },
  { id: "anthropic", labelKey: "providers.anthropic_dialect" },
];

const CLIENT_PROFILES: {
  id: ProviderClientProfile;
  labelKey:
    | "providers.profile_auto"
    | "providers.profile_native"
    | "providers.profile_claude_code"
    | "providers.profile_codex";
}[] = [
  { id: "auto", labelKey: "providers.profile_auto" },
  { id: "native", labelKey: "providers.profile_native" },
  { id: "claude_code", labelKey: "providers.profile_claude_code" },
  { id: "codex", labelKey: "providers.profile_codex" },
];

function defaultBaseUrl(kind: ProviderKind): string {
  return kind === "anthropic" ? "https://api.anthropic.com" : "https://api.openai.com/v1";
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
  const [nameError, setNameError] = useState<string | null>(null);

  const [newName, setNewName] = useState("");
  const [newKind, setNewKind] = useState<ProviderKind>("openai_compatible");
  const [newBaseUrl, setNewBaseUrl] = useState("");

  const [ccPreview, setCcPreview] = useState<CcSwitchPreview | null>(null);
  const [ccLoading, setCcLoading] = useState(false);
  const [ccSelected, setCcSelected] = useState<Record<string, boolean>>({});
  const isMounted = useRef(true);
  useEffect(() => () => { isMounted.current = false; }, []);

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

  const addProvider = () => {
    const name = newName.trim();
    if (!name) return;
    const id = slugify(name);
    if (config.providers.some((p) => p.id === id)) {
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
        client_profile: newKind === "anthropic" ? "claude_code" : "auto",
        bearer_auth: false,
        enabled: true,
        timeout_secs: 600,
        connect_timeout_secs: 15,
        anthropic_version: null,
        balance: { preset: "none", custom: null },
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
    setOpenId(id);
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
      notify("error", errorText(e));
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
  const openModelCount = open
    ? config.models.filter((m) => m.provider_id === open.id).length
    : 0;

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
          <button className="linky" onClick={loadCcPreview} disabled={ccLoading || busy}>
            {ccLoading ? t("providers.reading_cc") : t("providers.import_cc")}
          </button>
        }
      />

      {config.providers.length === 0 ? (
        <div className="empty-state">
          <p>{t("providers.empty")}</p>
          <p className="muted">{t("providers.empty_hint")}</p>
        </div>
      ) : (
        <div className="list">
          {config.providers.map((p) => {
            const models = config.models.filter((m) => m.provider_id === p.id);
            const status = !p.enabled ? "off" : keys[p.id] || p.key_ref === "" ? "ok" : "warn";
            return (
              <button
                key={p.id}
                className={`list-row ${openId === p.id ? "selected" : ""}`}
                onClick={() => setOpenId(p.id)}
                aria-label={t("providers.open_row", { name: p.name })}
              >
                <StatusDot tone={status} />
                <div className="row-main">
                  <span className="row-title">{p.name}</span>
                  <span className="row-sub">
                    <span className="mono">{p.base_url}</span>
                    {models.length > 0 && ` · ${t("count.models", { n: models.length })}`}
                    {p.kind === "anthropic"
                      ? ` · ${t("providers.anthropic_dialect")}`
                      : ` · ${t("providers.openai_dialect")}`}
                  </span>
                </div>
                {p.balance.preset !== "none" && (
                  <BalanceChip status={balances[p.id]} />
                )}
              </button>
            );
          })}
        </div>
      )}

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
              options={KINDS.map((k) => ({ value: k.id, label: t(k.labelKey) }))}
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

function CcSwitchRow({
  draft,
  checked,
  onSelect,
}: {
  draft: CcProviderDraft;
  checked: boolean;
  onSelect: (id: string, on: boolean) => void;
}) {
  const { t } = useI18n();
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
              .map((m) => (m.tier ? `${m.upstream_model} (${m.tier})` : m.upstream_model))
              .join(", ")}
      </td>
    </tr>
  );
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

/**
 * One account's resource maintenance: where it stands, when it was last
 * checked in, and the four things a user can do about it.
 *
 * The phase is rendered as a word rather than a dot on purpose. A dot would have
 * to collapse "never tried", "waiting for you", "already done today" and
 * "failed" into a colour, and three of those want different things from the
 * person looking at them.
 *
 * `resume` is a separate button from `start` and is only offered while a browser
 * is held. That is the whole contract of a paused check-in: the button continues
 * the browser already open, with the session that was halfway through solving a
 * challenge, and it must never quietly begin a new one.
 */
function AccountMaintenanceRow({
  view,
  busy,
  onStart,
  onResume,
  onCancel,
}: {
  view: CheckinView;
  busy: boolean;
  onStart: () => void;
  onResume: () => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const paused = view.awaiting_user;

  // The backend publishes a closed phase vocabulary, and a template key is not a
  // key TypeScript can check. The fallback matters more than the cast: an
  // unrecognised phase must read as "unknown", never as a phase that implies a
  // reward or a success.
  const phaseKey = `account.phase.${view.phase}` as const;
  const phaseLabel = PHASE_KEYS.has(view.phase)
    ? t(phaseKey as Parameters<typeof t>[0])
    : view.phase;

  return (
    <div className="account-maint">
      <div className="row-main">
        <span className="row-title mono">{view.account_id}</span>
        <span className="row-sub">
          {phaseLabel}
          {view.last_completed_at !== null && (
            <span className="muted"> · {t("account.last_checkin")} {ago(view.last_completed_at)}</span>
          )}
        </span>
        {view.failure && <span className="row-sub warn-text">{view.failure}</span>}
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
      </div>

      <div className="controls">
        {paused ? (
          <>
            <button className="primary" disabled={busy} onClick={onResume}>
              {t("account.resume")}
            </button>
            <button disabled={busy} onClick={onCancel}>
              {t("account.cancel")}
            </button>
          </>
        ) : (
          <>
            <button disabled={busy || view.phase === "running"} onClick={onStart}>
              {t("account.checkin")}
            </button>
            {view.browser_held && (
              <button disabled={busy} onClick={onCancel}>
                {t("account.close_browser")}
              </button>
            )}
          </>
        )}
      </div>
    </div>
  );
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
]);

/**
 * The check-in view for one account, or a neutral one when the snapshot carries
 * none.
 *
 * A fallback rather than nothing, because "the backend said nothing about this
 * account" and "this account is idle" must not render the same way: the first is
 * a gap in the bridge and the second is a fact about the account.
 */
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
    }
  );
}

/** A whole number of seconds, as a short relative phrase. */
function ago(unix_secs: number): string {

const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unix_secs);
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
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
  const models = snapshot.config.models.filter((m) => m.provider_id === provider.id);
  const { t } = useI18n();
  const notify = useToast();
  const hasKey = snapshot.keys[provider.id] ?? false;
  const [keyDraft, setKeyDraft] = useState("");
  const [discovered, setDiscovered] = useState<DiscoveredModel[] | null>(null);
  const [discovering, setDiscovering] = useState(false);
  const [checkingBalance, setCheckingBalance] = useState(false);
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

  const saveKey = async () => {
    const value = keyDraft.trim();
    if (!value) return;
    const ok = await run(() => api.setKey(providerId, value));
    if (ok) setKeyDraft("");
  };

  // Check-in reports its own outcome rather than round-tripping the snapshot, so
  // these three ask for a fresh view and let the next poll carry it. The failure
  // is surfaced with the backend's own wording: every message here names the
  // setting or the action that fixes it, and paraphrasing would throw that away.
  const start = async (id: string, account: string, maintenance: Maintenance) => {
    if (!maintenance.checkin_enabled) {
      notify("error", t("account.not_enabled"));
      return;
    }
    try {
      await api.startCheckin(id, account);
    } catch (e) {
      notify("error", `${t("account.start_failed")}: ${errorText(e)}`);
    }
  };
  const resume = async (id: string, account: string) => {
    try {
      await api.resumeCheckin(id, account);
    } catch (e) {
      notify("error", `${t("account.resume_failed")}: ${errorText(e)}`);
    }
  };
  const cancel = async (id: string, account: string) => {
    try {
      await api.cancelCheckin(id, account);
    } catch (e) {
      notify("error", `${t("account.cancel_failed")}: ${errorText(e)}`);
    }
  };

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
      notify("error", errorText(e));
    } finally {
      if (generation === discoveryGeneration.current) setDiscovering(false);
    }
  };

  const addDiscovered = (model: DiscoveredModel) => {
    // Adding a model is a config mutation; the provider page reaches into the
    // same save pipeline every other page uses.
    void save((cfg) => {
      if (
        cfg.models.some(
          (m) => m.provider_id === provider.id && m.upstream_model === model.id,
        )
      ) {
        notify("error", t("models.duplicate", { model: model.id }));
        return null;
      }
      const next = structuredClone(cfg);
      next.models.push({
        provider_id: provider.id,
        upstream_model: model.id,
        tier: null,
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
  };

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
          [t("providers.endpoint"), <span className="mono">{provider.base_url}</span>],
          [t("providers.dialect"), provider.kind === "anthropic" ? t("providers.anthropic_dialect") : t("providers.openai_dialect")],
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
        <Section title={t("providers.api_key")} hint={t("providers.api_key_hint")}>
          <div className="controls">
            <input
              type="password"
              autoComplete="off"
              placeholder="sk-…"
              value={keyDraft}
              onChange={(e) => setKeyDraft(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && void saveKey()}
            />
            <Button kind="primary" onClick={saveKey} disabled={busy || !keyDraft.trim()}>
              {t("providers.save_key")}
            </Button>
          </div>
        </Section>
      )}

      <Section title={t("providers.models_section")} hint={t("providers.models_hint")}>
        <div className="row gap wrap">
          {models.length > 0 && (
            <span className="row gap wrap">
              {models.map((m) => (
                <span key={m.upstream_model} className="chip chip-done">
                  <span className="mono">{previewId(provider.id, m.upstream_model)}</span>
                </span>
              ))}
            </span>
          )}
          <Button
            kind="ghost"
            ariaLabel="Fetch models"
            onClick={() => void discover()}
            disabled={busy || discovering}
          >
            {discovering ? t("providers.discovering") : t("providers.fetch")}
          </Button>
        </div>
        {discovered && discovered.length > 0 && (
          <table className="table">
            <thead>
              <tr>
                <th>{t("field.model_name")}</th>
                <th>{t("models.price")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {discovered.map((d) => (
                <tr key={d.id}>
                  <td className="mono">{d.id}</td>
                  <td className="muted">
                    {d.pricing
                      ? `${d.pricing.input_per_mtok} / ${d.pricing.output_per_mtok} ${d.pricing.currency}`
                      : "—"}
                  </td>
                  <td>
                    <Button
                      kind="ghost"
                      onClick={() => addDiscovered(d)}
                      disabled={models.some((m) => m.upstream_model === d.id)}
                    >
                      {t("cc.add")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
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
                  p.kind = kind;
                })
              }
              options={KINDS.map((k) => ({ value: k.id, label: t(k.labelKey) }))}
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
          <Field label={t("providers.client_profile")} hint={t("providers.client_profile_hint")}>
            <Select
              ariaLabel={t("providers.client_profile")}
              value={provider.client_profile ?? "auto"}
              onChange={(client_profile) =>
                onUpdate((p) => {
                  p.client_profile = client_profile;
                })
              }
              options={CLIENT_PROFILES.map((c) => ({ value: c.id, label: t(c.labelKey) }))}
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

      <Section title={t("providers.balance")} hint={t("providers.balance_hint")}>
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
              options={BALANCE_PRESETS.map((p) => ({ value: p.id, label: p.label }))}
            />
          </Field>
          <div className="field-actions">
            <Button
              kind="ghost"
              onClick={() => {
                setCheckingBalance(true);
                void run(() => api.refreshBalance(provider.id)).finally(() =>
                  setCheckingBalance(false),
                );
              }}
              disabled={busy || checkingBalance || provider.balance.preset === "none"}
            >
              {checkingBalance ? t("providers.checking") : t("providers.check_now")}
            </Button>
          </div>
        </div>
        {snapshot.balances[provider.id]?.error && (
          <p className="field-hint">{snapshot.balances[provider.id].error}</p>
        )}
      </Section>

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
        Account maintenance is offered only when the build actually contains it.
        Asking first is the discipline the rest of this panel follows: a command
        that was never registered looks exactly like one that failed, and a panel
        that probed for it would render "unavailable" on a build whose bridge is
        merely broken.
      */}
      {snapshot.account_maintenance_available && (provider.accounts ?? []).length > 0 && (
        <Section title={t("account.section")} hint={t("account.section_hint")}>
          <div className="list">
            {(provider.accounts ?? []).map((account) => {
              const view = checkinFor(snapshot.checkin, provider.id, account.account_id);
              return (
                <AccountMaintenanceRow
                  key={account.account_id}
                  view={view}
                  busy={busy}
                  onStart={() =>
                    void start(provider.id, account.account_id, account.maintenance)
                  }
                  onResume={() => void resume(provider.id, account.account_id)}
                  onCancel={() => void cancel(provider.id, account.account_id)}
                />
              );
            })}
          </div>
        </Section>
      )}

      <div>
        <Button kind="danger" disabled={busy} onClick={onRemove}>
          {t("providers.remove")}
        </Button>
      </div>
    </Drawer>
  );
}
