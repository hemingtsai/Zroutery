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
  type DiscoveredModel,
  type Provider,
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
  const { t } = useI18n();
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

  const update = (id: string, patch: Partial<Provider>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const provider = next.providers.find((p) => p.id === id);
      if (!provider) return null;
      Object.assign(provider, patch);
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
        impersonate_claude_code: newKind === "anthropic",
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

  const removeProvider = async (id: string) => {
    const models = config.models.filter((m) => m.provider_id === id);
    const ok = await save((cfg) => {
      const next = structuredClone(cfg);
      next.providers = next.providers.filter((p) => p.id !== id);
      next.models = next.models.filter((m) => m.provider_id !== id);
      return next;
    });
    if (!ok) return;
    await run(() => api.clearKey(id));
    if (!isMounted.current) return;
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
                aria-label={`Open ${p.name}`}
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
          provider={open}
          snapshot={snapshot}
          busy={busy}
          save={save}
          run={run}
          onClose={() => setOpenId(null)}
          onUpdate={(patch) => update(open.id, patch)}
          onRemove={() =>
            setConfirm({
              title: t("confirm.remove_provider"),
              body: t("confirm.remove_provider_body"),
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
  return (
    <tr className={draft.already_imported ? "row-warn" : ""}>
      <td>
        <input
          type="checkbox"
          aria-label={`Import ${draft.name}`}
          checked={checked}
          disabled={draft.already_imported}
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
              .map((m) => (m.class ? `${m.upstream_model} (${m.class})` : m.upstream_model))
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
  onUpdate: (patch: Partial<Provider>) => void;
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

  const saveKey = async () => {
    const value = keyDraft.trim();
    if (!value) return;
    const ok = await run(() => api.setKey(provider.id, value));
    if (ok) setKeyDraft("");
  };

  const discover = async () => {
    setDiscovering(true);
    try {
      const ids = await api.fetchModels(provider);
      setDiscovered(ids);
      if (ids.length === 0) notify("error", t("providers.empty_catalogue", { name: provider.name }));
    } catch (e) {
      notify("error", errorText(e));
    } finally {
      setDiscovering(false);
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
        class: null,
        priority: 0,
        weight: 1,
        enabled: true,
        supports_tools: true,
        supports_vision: false,
        supports_thinking: false,
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
          <Button kind="ghost" onClick={() => void discover()} disabled={busy || discovering}>
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
            onCommit={(name) => name.trim() && onUpdate({ name })}
          />
          <TextField
            label={t("field.base_url")}
            hint={t("field.base_url_hint")}
            value={provider.base_url}
            onCommit={(base_url) => base_url.trim() && onUpdate({ base_url })}
            wide
          />
          <Field label={t("field.api_dialect")}>
            <Segment
              ariaLabel={t("field.api_dialect")}
              value={provider.kind}
              onChange={(kind) => onUpdate({ kind })}
              options={KINDS.map((k) => ({ value: k.id, label: t(k.labelKey) }))}
            />
          </Field>
          <NumberField
            label={t("field.timeout")}
            hint={t("field.timeout_hint")}
            min={1}
            integer
            value={provider.timeout_secs}
            onCommit={(timeout_secs) => onUpdate({ timeout_secs: timeout_secs ?? 600 })}
          />
          <NumberField
            label={t("field.connect_timeout")}
            min={1}
            integer
            value={provider.connect_timeout_secs}
            onCommit={(connect_timeout_secs) =>
              onUpdate({ connect_timeout_secs: connect_timeout_secs ?? 15 })
            }
          />
          {provider.kind === "anthropic" && (
            <TextField
              label={t("providers.f_version")}
              hint={t("providers.f_version_hint")}
              value={provider.anthropic_version ?? ""}
              placeholder="2023-06-01"
              onCommit={(v) => onUpdate({ anthropic_version: v || null })}
            />
          )}
        </div>
        <div className="grid-two">
          <Toggle
            label={t("common.enabled")}
            checked={provider.enabled}
            onChange={(enabled) => onUpdate({ enabled })}
          />
          <Toggle
            label={t("providers.impersonate")}
            hint={t("providers.impersonate_hint")}
            checked={provider.impersonate_claude_code}
            onChange={(impersonate_claude_code) => onUpdate({ impersonate_claude_code })}
          />
          {provider.kind === "anthropic" && (
            <Toggle
              label={t("providers.bearer_auth")}
              hint={t("providers.bearer_auth_hint")}
              checked={provider.bearer_auth}
              onChange={(bearer_auth) => onUpdate({ bearer_auth })}
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
                onUpdate({
                  balance: {
                    preset,
                    custom:
                      preset === "custom" ? provider.balance.custom ?? defaultProbe() : null,
                  },
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
            onChange={(v) => onUpdate({ quirks: { ...provider.quirks, use_max_completion_tokens: v } })}
          />
          <Toggle
            label={t("quirk.drop_temperature")}
            checked={provider.quirks.drop_temperature}
            onChange={(v) => onUpdate({ quirks: { ...provider.quirks, drop_temperature: v } })}
          />
          <Toggle
            label={t("quirk.drop_top_p")}
            checked={provider.quirks.drop_top_p}
            onChange={(v) => onUpdate({ quirks: { ...provider.quirks, drop_top_p: v } })}
          />
          <Toggle
            label={t("quirk.drop_stop")}
            hint={t("quirk.drop_stop_hint")}
            checked={provider.quirks.drop_stop}
            onChange={(v) => onUpdate({ quirks: { ...provider.quirks, drop_stop: v } })}
          />
          <Toggle
            label={t("quirk.system_as_developer")}
            checked={provider.quirks.system_as_developer}
            onChange={(v) => onUpdate({ quirks: { ...provider.quirks, system_as_developer: v } })}
          />
          <Toggle
            label={t("quirk.reasoning_effort")}
            checked={provider.quirks.send_reasoning_effort}
            onChange={(v) => onUpdate({ quirks: { ...provider.quirks, send_reasoning_effort: v } })}
          />
        </div>
      </Section>

      <div>
        <Button kind="danger" disabled={busy} onClick={onRemove}>
          {t("providers.remove")}
        </Button>
      </div>
    </Drawer>
  );
}
