import { useState } from "react";
import {
  CLASSES,
  emptyPricing,
  modelRows,
  previewId,
  priceText,
  virtualId,
  type AppConfig,
  type ModelClass,
  type ModelEntry,
  type ModelRow,
  type Pricing,
  type Provider,
  type Snapshot,
} from "../api";
import { useI18n } from "../i18n";
import {
  Badge,
  Banner,
  Button,
  ConfirmDialog,
  Drawer,
  KeyValue,
  NumberField,
  PageHead,
  Section,
  Select,
  StatusDot,
  TextField,
  Toggle,
  useToast,
  type ConfirmRequest,
  ms,
} from "../components";

/**
 * The model list. One row per model: name, provider, class, one small status
 * dot. Everything else — pricing, capabilities, aliases, the knobs — lives in
 * the drawer, because a list is for choosing and a drawer is for knowing.
 */
export default function Models({
  snapshot,
  save,
  busy,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
}) {
  const { config, health } = snapshot;
  const { t } = useI18n();
  const notify = useToast();
  const rows = modelRows(snapshot);
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [draft, setDraft] = useState({ provider_id: config.providers[0]?.id ?? "", upstream_model: "" });

  const unclassified = rows.filter((r) => r.model.class === null);
  const cooling = new Set(
    health.filter((h) => h.cooldown_remaining_secs > 0).map((h) => h.model_id),
  );

  const update = (id: string, patch: Partial<ModelEntry>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const model = next.models.find(
        (m) => previewId(m.provider_id, m.upstream_model) === id,
      );
      if (!model) return null;
      Object.assign(model, patch);
      return next;
    });
  };

  const remove = (id: string) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const index = next.models.findIndex(
        (m) => previewId(m.provider_id, m.upstream_model) === id,
      );
      if (index < 0) return null;
      next.models.splice(index, 1);
      return next;
    });
    setOpenId(null);
  };

  const add = () => {
    const upstream = draft.upstream_model.trim();
    const providerId = draft.provider_id;
    if (!providerId || !upstream) {
      notify("error", t("models.pick_provider_name"));
      return;
    }
    if (config.models.some((m) => m.provider_id === providerId && m.upstream_model === upstream)) {
      setNameError(t("models.duplicate", { model: upstream }));
      return;
    }
    void save((cfg) => {
      if (
        cfg.models.some((m) => m.provider_id === providerId && m.upstream_model === upstream)
      ) {
        setNameError(t("models.duplicate", { model: upstream }));
        return null;
      }
      const next = structuredClone(cfg);
      next.models.push({
        provider_id: providerId,
        upstream_model: upstream,
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
        pricing: null,
      });
      return next;
    });
    setDraft({ ...draft, upstream_model: "" });
    setNameError(null);
  };

  const open = rows.find((r) => r.id === openId) ?? null;

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      {unclassified.length > 0 && (
        <Banner tone="warn">
          {t("models.unclassified", {
            n: unclassified.length,
            ids: unclassified.map((r) => r.id).join(", "),
          })}
        </Banner>
      )}

      <PageHead
        lede={
          rows.length === 0 ? t("models.lede_none") : t("models.exposed", { n: rows.length })
        }
      />

      {rows.length === 0 ? (
        <div className="empty-state">
          <p>{t("models.empty")}</p>
          <p className="muted">{t("models.empty_hint")}</p>
        </div>
      ) : (
        <div className="list">
          {rows.map((r) => {
            const provider = config.providers.find((p) => p.id === r.model.provider_id);
            const status = !r.model.enabled || provider?.enabled === false
              ? "off"
              : cooling.has(r.id)
                ? "warn"
                : "ok";
            const healthRow = health.find((h) => h.model_id === r.id);
            return (
              <button
                key={r.id}
                className={`list-row ${openId === r.id ? "selected" : ""}`}
                onClick={() => setOpenId(r.id)}
                aria-label={t("models.open_row", { id: r.id })}
              >
                <StatusDot tone={status} />
                <div className="row-main">
                  <span className="row-title mono">{r.id}</span>
                  <span className="row-sub">
                    {provider?.name ?? t("models.missing_provider")}
                    {provider?.enabled === false && ` ${t("providers.provider_off")}`}
                    {r.model.class ? ` · ${r.model.class}` : ` · ${t("models.no_class")}`}
                  </span>
                </div>
                <span className="col-num">
                  {r.model.pricing ? priceText(r.model.pricing) : t("common.dash")}
                </span>
                <span className="col-num">{healthRow ? ms(healthRow.avg_latency_ms) : t("common.dash")}</span>
              </button>
            );
          })}
        </div>
      )}

      <Section title={t("models.add_section")} hint={t("models.add_hint")}>
        <div className="controls">
          <Select
            ariaLabel={t("field.provider")}
            value={draft.provider_id || null}
            onChange={(provider_id) => setDraft({ ...draft, provider_id })}
            placeholder={t("field.provider")}
            options={config.providers.map((p) => ({ value: p.id, label: p.name }))}
          />
          <input
            aria-label={t("field.model_name")}
            placeholder="deepseek-chat"
            className={nameError ? "input-error" : undefined}
            value={draft.upstream_model}
            onChange={(e) => {
              setDraft({ ...draft, upstream_model: e.currentTarget.value });
              setNameError(null);
            }}
            onKeyDown={(e) => e.key === "Enter" && add()}
          />
          <Button kind="primary" onClick={add} disabled={busy || !config.providers.length}>
            {t("models.add")}
          </Button>
        </div>
        {nameError && <p className="field-hint field-hint-danger">{nameError}</p>}
        {!config.providers.length && <p className="field-hint">{t("models.add_provider_first")}</p>}
      </Section>

      {open && (
        <ModelDrawer
          row={open}
          provider={config.providers.find((p) => p.id === open.model.provider_id)}
          health={health.find((h) => h.model_id === open.id)}
          busy={busy}
          onClose={() => setOpenId(null)}
          onUpdate={(patch) => update(open.id, patch)}
          onRemove={() =>
            setConfirm({
              title: t("confirm.remove_model"),
              body: t("confirm.remove_model_body"),
              confirmLabel: t("models.remove"),
              danger: true,
              onConfirm: () => remove(open.id),
            })
          }
        />
      )}
    </>
  );
}

/**
 * One model's whole story. "Used for" answers where it sits in routing;
 * performance is what the proxy has observed, not a promise.
 */
function ModelDrawer({
  row,
  provider,
  health,
  busy,
  onClose,
  onUpdate,
  onRemove,
}: {
  row: ModelRow;
  provider: Provider | undefined;
  health: Snapshot["health"][number] | undefined;
  busy: boolean;
  onClose: () => void;
  onUpdate: (patch: Partial<ModelEntry>) => void;
  onRemove: () => void;
}) {
  const m = row.model;
  const { t } = useI18n();
  const status = !m.enabled || provider?.enabled === false ? "off" : "ok";

  return (
    <Drawer title={<><StatusDot tone={status} /> <span className="mono">{row.id}</span></>} onClose={onClose}>
      <KeyValue
        rows={[
          [t("models.provider"), provider?.name ?? t("models.missing_provider")],
          [t("models.upstream"), <span className="mono">{m.upstream_model}</span>],
          [
            t("models.class"),
            <Select<ModelClass | "none">
              ariaLabel={t("models.class")}
              value={m.class ?? "none"}
              disabled={busy}
              onChange={(next) => onUpdate({ class: next === "none" ? null : next })}
              options={[
                { value: "none", label: "—" },
                ...CLASSES.map((c) => ({ value: c, label: virtualId(c) })),
              ]}
            />,
          ],
          [t("common.enabled"), <Toggle label="" checked={m.enabled} onChange={(enabled) => onUpdate({ enabled })} />],
          [t("models.aliases"), m.aliases.length ? <span className="mono">{m.aliases.join(", ")}</span> : t("common.dash")],
        ]}
      />

      <Section title={t("models.performance")}>
        {health ? (
          <KeyValue
            rows={[
              [t("models.avg_latency"), <span className="mono">{ms(health.avg_latency_ms)}</span>],
              [t("models.requests"), <span className="mono">{health.total_success + health.total_failure}</span>],
              [
                t("models.failures"),
                health.total_failure > 0 ? (
                  <Badge tone="danger">{health.total_failure}</Badge>
                ) : (
                  <span className="mono">0</span>
                ),
              ],
              [t("models.last_error"), health.last_error ?? t("common.dash")],
            ]}
          />
        ) : (
          <p className="empty">{t("models.no_traffic")}</p>
        )}
      </Section>

      <Section title={t("models.configuration")}>
        <div className="controls">
          <TextField
            label={t("field.model_name")}
            hint={t("models.f_model_name_hint")}
            value={m.upstream_model}
            onCommit={(v) => v.trim() && onUpdate({ upstream_model: v })}
          />
          <TextField
            label={t("models.f_display")}
            hint={t("models.f_display_hint")}
            value={m.display_name ?? ""}
            placeholder={m.upstream_model}
            onCommit={(v) => onUpdate({ display_name: v || null })}
          />
          <NumberField
            label={t("models.f_priority")}
            hint={t("models.f_priority_hint")}
            min={0}
            integer
            value={m.priority}
            onCommit={(priority) => onUpdate({ priority: priority ?? 0 })}
          />
          <NumberField
            label={t("models.f_weight")}
            hint={t("models.f_weight_hint")}
            min={1}
            integer
            value={m.weight}
            onCommit={(weight) => onUpdate({ weight: weight ?? 1 })}
          />
          <NumberField
            label={t("models.f_max_tokens")}
            hint={t("models.f_max_tokens_hint")}
            min={1}
            placeholder="unlimited"
            integer
            value={m.max_output_tokens}
            onCommit={(max_output_tokens) => onUpdate({ max_output_tokens })}
          />
          <TextField
            label={t("models.aliases")}
            hint={t("models.f_aliases_hint")}
            value={m.aliases.join(", ")}
            onCommit={(v) =>
              onUpdate({
                aliases: v.split(",").map((a) => a.trim()).filter(Boolean),
              })
            }
            wide
          />
        </div>
        <div className="grid-two">
          <Toggle label={t("models.tool_use")} checked={m.supports_tools} onChange={(v) => onUpdate({ supports_tools: v })} />
          <Toggle label={t("models.vision")} checked={m.supports_vision} onChange={(v) => onUpdate({ supports_vision: v })} />
          <Toggle
            label={t("models.thinking")}
            checked={m.supports_thinking}
            onChange={(v) => onUpdate({ supports_thinking: v })}
          />
        </div>
      </Section>

      <Section title={t("models.price")} hint={t("models.price_hint")}>
        <PriceFields pricing={m.pricing} onChange={(pricing) => onUpdate({ pricing })} />
      </Section>

      <div>
        <Button kind="danger" disabled={busy} onClick={onRemove}>
          {t("models.remove")}
        </Button>
      </div>
    </Drawer>
  );
}

/**
 * The four numbers that make up a price. Clearing input and output removes the
 * price entirely, which is how a model goes back to being unpriced.
 */
function PriceFields({
  pricing,
  onChange,
}: {
  pricing: Pricing | null;
  onChange: (pricing: Pricing | null) => void;
}) {
  const { t } = useI18n();
  const current = pricing ?? emptyPricing();

  const patch = (change: Partial<Pricing>) => {
    const next = { ...current, ...change };
    const priced =
      next.input_per_mtok > 0 ||
      next.output_per_mtok > 0 ||
      next.cache_read_per_mtok !== null ||
      next.cache_write_per_mtok !== null;
    onChange(priced ? next : null);
  };

  return (
    <div className="controls">
      <TextField
        label={t("field.currency")}
        hint={t("field.currency_hint")}
        value={current.currency}
        onCommit={(currency) => patch({ currency: currency.trim().toUpperCase() || "USD" })}
      />
      <NumberField
        label={t("field.input")}
        hint={t("field.input_hint")}
        min={0}
        value={current.input_per_mtok}
        onCommit={(v) => patch({ input_per_mtok: v ?? 0 })}
      />
      <NumberField
        label={t("field.output")}
        hint={t("field.output_hint")}
        min={0}
        value={current.output_per_mtok}
        onCommit={(v) => patch({ output_per_mtok: v ?? 0 })}
      />
      <NumberField
        label={t("models.cache_read")}
        hint={t("models.cache_read_hint")}
        min={0}
        placeholder="same as input"
        value={current.cache_read_per_mtok}
        onCommit={(v) => patch({ cache_read_per_mtok: v })}
      />
      <NumberField
        label={t("models.cache_write")}
        hint={t("models.cache_write_hint")}
        min={0}
        placeholder="not billed"
        value={current.cache_write_per_mtok}
        onCommit={(v) => patch({ cache_write_per_mtok: v })}
      />
      {pricing && (
        <div className="field-actions">
          <Button kind="ghost" onClick={() => onChange(null)}>
            {t("models.clear_price")}
          </Button>
        </div>
      )}
    </div>
  );
}
