import { memo, useCallback, useMemo, useState, type ReactNode } from "react";
import {
  TIERS,
  emptyPricing,
  modelRows,
  previewId,
  priceText,
  virtualId,
  tierName,
  type AppConfig,
  type ModelTier,
  type ModelEntry,
  type ModelRow,
  type NamingStyle,
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
  /*
   * The list, and everything derived from it, is computed here rather than in
   * the render body because it is the only unbounded thing on the page and the
   * add-a-model field below it holds a draft that changes on every keystroke.
   *
   * `modelRows` reads only these two arrays, and `exposed_ids` is parallel to
   * `config.models`, so keying on the pair is the whole input: a save that
   * replaces either rebuilds the list, and nothing else has to.
   */
  const rows = useMemo(
    () => modelRows(snapshot),
    [snapshot.config.models, snapshot.exposed_ids],
  );
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [draft, setDraft] = useState({ provider_id: config.providers[0]?.id ?? "", upstream_model: "" });

  /*
   * Two index lookups instead of two linear scans.
   *
   * Every row used to ask `config.providers.find` and `health.find` its own
   * question, so one render cost O(rows x (providers + health)) — and because
   * the draft field below the list re-renders the page, that was the price of
   * typing a model name. `Map` makes it O(providers + health) to build.
   */
  const providerById = useMemo(
    () => new Map(config.providers.map((p) => [p.id, p])),
    [config.providers],
  );
  const healthById = useMemo(() => new Map(health.map((h) => [h.model_id, h])), [health]);

  // A decision model has no tier by design; see `AppConfig::unclassified_models`.
  const unclassified = useMemo(
    () =>
      rows.filter(
        (r) =>
          r.model.tier === null &&
          providerById.get(r.model.provider_id)?.kind !== "decision",
      ),
    [rows, providerById],
  );
  const cooling = useMemo(
    () => new Set(health.filter((h) => h.cooldown_remaining_secs > 0).map((h) => h.model_id)),
    [health],
  );

  /**
   * One row's worth of text, resolved while the configuration produced it.
   *
   * Joining the sub-line here rather than in the row keeps the strings out of
   * the per-keystroke path entirely; the characters are the same either way, so
   * the rendered line is identical.
   */
  const listView = useMemo(
    () =>
      rows.map((r) => {
        const provider = providerById.get(r.model.provider_id);
        const healthRow = healthById.get(r.id);
        const status: ListTone =
          !r.model.enabled || provider?.enabled === false
            ? "off"
            : cooling.has(r.id)
              ? "warn"
              : "ok";
        return {
          id: r.id,
          status,
          selected: openId === r.id,
          providerLine:
            (provider?.name ?? t("models.missing_provider")) +
            (provider?.enabled === false ? ` ${t("providers.provider_off")}` : "") +
            (r.model.tier ? ` · ${tierName(r.model.tier, t)}` : ` · ${t("models.no_tier")}`),
          price: r.model.pricing ? priceText(r.model.pricing) : t("common.dash"),
          latency: healthRow ? ms(healthRow.avg_latency_ms) : t("common.dash"),
        };
      }),
    [rows, providerById, healthById, cooling, openId, t],
  );

  const openRow = useCallback((id: string) => setOpenId(id), []);

  /**
   * Apply an edit to the model the save is actually about.
   *
   * `mutate` receives the model from the config being committed, not one
   * captured when the control was rendered: a save queued behind an in-flight
   * one must merge into that save's result. A callback that spread the
   * rendered model would otherwise carry the whole nested `capabilities` or
   * `pricing` object back and undo the edit that just landed.
   */
  const update = (id: string, mutate: (model: ModelEntry) => void) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const model = next.models.find(
        (m) => previewId(m.provider_id, m.upstream_model) === id,
      );
      if (!model) return null;
      mutate(model);
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
      // The Auto Mode pool names models by exposed id or alias. Deleting the
      // model deletes its pool membership with it — leaving the candidate would
      // dangle it, and a dangling candidate used to block every save until it
      // was hunted down by hand.
      const gone = [id, ...next.models[index].aliases];
      next.models.splice(index, 1);
      next.classifier.candidates = next.classifier.candidates.filter(
        (c) => !gone.includes(c.model),
      );
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
          {listView.map((row) => (
            <ModelListRow key={row.id} {...row} onOpen={openRow} />
          ))}
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
          namingStyle={config.routing.naming_style}
          onClose={() => setOpenId(null)}
          onUpdate={(mutate) => update(open.id, mutate)}
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

/** The status vocabulary a list row can draw. Mirrors `StatusDot`'s prop. */
type ListTone = "ok" | "warn" | "danger" | "off";

/**
 * One row of the model list, memoised.
 *
 * This is the only element on the page that appears once per configured model,
 * so it is the only one worth stopping: it takes every string it prints already
 * resolved and only two changing values of its own (`selected`, and the status
 * that depends on it), both computed by the list above. With the add-a-model
 * field holding a draft that changes on every keystroke, a row that re-rendered
 * with its parent turned four hundred rows of DOM into the price of typing a
 * model name.
 *
 * The markup is the row's own button, unchanged — a component boundary is not
 * a node, so the list draws exactly what it drew before.
 */
const ModelListRow = memo(function ModelListRow({
  id,
  status,
  providerLine,
  price,
  latency,
  selected,
  onOpen,
}: {
  id: string;
  status: ListTone;
  providerLine: string;
  price: string;
  latency: string;
  selected: boolean;
  onOpen: (id: string) => void;
}) {
  const { t } = useI18n();
  return (
    <button type="button"
      className={`list-row ${selected ? "selected" : ""}`}
      onClick={() => onOpen(id)}
      /*
       * The name carries the state as well, because the row's own content is
       * replaced by it in the accessibility tree: the dot beside the model is
       * a colour, and a colour is invisible to a screen reader. Without this
       * the row announced only "Open <id>", and whether the model was enabled
       * was nowhere in the sentence.
       */
      aria-label={`${t("models.open_row", { id })} — ${
        status === "off" ? t("status.disabled") : t("status.enabled")
      }`}
    >
      <StatusDot
        tone={status}
        state={status === "off" ? t("status.disabled") : t("status.enabled")}
      />
      <div className="row-main">
        {/*
         * A model id can be long, and an id that is longer than the row makes
         * the row wider than the page. The text is clipped rather than
         * truncated away: it is still in the document, and the row's own
         * accessible name carries it in full, so nothing is lost but the
         * pixels.
         */}
        <span className="row-title mono truncate" title={id}>
          {id}
        </span>
        <span className="row-sub">{providerLine}</span>
      </div>
      <span className="col-num">{price}</span>
      <span className="col-num">{latency}</span>
    </button>
  );
});

/**
 * One model's whole story. "Used for" answers where it sits in routing;
 * performance is what the proxy has observed, not a promise.
 */
function ModelDrawer({
  row,
  provider,
  health,
  busy,
  namingStyle,
  onClose,
  onUpdate,
  onRemove,
}: {
  row: ModelRow;
  provider: Provider | undefined;
  health: Snapshot["health"][number] | undefined;
  busy: boolean;
  namingStyle: NamingStyle;
  onClose: () => void;
  /** Receives the model being committed, so a nested object is merged onto
   *  the latest one rather than a copy captured at render time. */
  onUpdate: (mutate: (model: ModelEntry) => void) => void;
  onRemove: () => void;
}) {
  const m = row.model;
  const { t } = useI18n();
  const status = !m.enabled || provider?.enabled === false ? "off" : "ok";
  // A decision model answers the Auto Mode's questions and cannot answer a
  // request that wants prose, so a tier would be a promise the router cannot
  // keep. The control is disabled rather than hidden, and the reason is beside
  // it — a hidden control would leave the row looking like an oversight.
  const tierLocked = provider?.kind === "decision";

  // The rows are built before the markup so that the explanation of a locked
  // tier can be *added* rather than spliced in: a conditional inside the
  // array literal is a tuple the compiler cannot type.
  const rows: [ReactNode, ReactNode][] = [
    [t("models.provider"), provider?.name ?? t("models.missing_provider")],
    [t("models.upstream"), <span className="mono">{m.upstream_model}</span>],
    [
      t("models.tier"),
      <Select<ModelTier | "none">
        ariaLabel={t("models.tier_for", { model: m.upstream_model })}
        value={m.tier ?? "none"}
        disabled={busy || tierLocked}
        onChange={(next) =>
          onUpdate((model) => {
            model.tier = next === "none" ? null : next;
          })
        }
        options={[
          { value: "none", label: "—" },
          ...TIERS.map((c) => ({ value: c, label: virtualId(c, namingStyle) })),
        ]}
      />,
    ],
    [
      t("common.enabled"),
      <Toggle
        label=""
        // The visible label is empty because the row beside it already
        // says "Enabled" — which left the switch itself with no name at
        // all, announcing as "checkbox, unchecked" and nothing more.
        ariaLabel={t("common.enabled")}
        checked={m.enabled}
        onChange={(enabled) =>
          onUpdate((model) => {
            model.enabled = enabled;
          })
        }
      />,
    ],
    [
      t("models.aliases"),
      m.aliases.length ? <span className="mono">{m.aliases.join(", ")}</span> : t("common.dash"),
    ],
  ];
  if (tierLocked) {
    rows.push([
      t("models.tier_decision"),
      <span className="muted">{t("models.tier_decision_hint")}</span>,
    ]);
  }

  return (
    <Drawer title={<><StatusDot tone={status} /> <span className="mono">{row.id}</span></>} onClose={onClose}>
      <KeyValue rows={rows} />

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
            onCommit={(v) =>
              v.trim() &&
              onUpdate((model) => {
                model.upstream_model = v;
              })
            }
          />
          <TextField
            label={t("models.f_display")}
            hint={t("models.f_display_hint")}
            value={m.display_name ?? ""}
            placeholder={m.upstream_model}
            onCommit={(v) =>
              onUpdate((model) => {
                model.display_name = v || null;
              })
            }
          />
          <NumberField
            label={t("models.f_priority")}
            hint={t("models.f_priority_hint")}
            min={0}
            integer
            value={m.priority}
            onCommit={(priority) =>
              onUpdate((model) => {
                model.priority = priority ?? 0;
              })
            }
          />
          <NumberField
            label={t("models.f_weight")}
            hint={t("models.f_weight_hint")}
            min={1}
            integer
            value={m.weight}
            onCommit={(weight) =>
              onUpdate((model) => {
                model.weight = weight ?? 1;
              })
            }
          />
          <NumberField
            label={t("models.f_max_tokens")}
            hint={t("models.f_max_tokens_hint")}
            min={1}
            placeholder={t("models.price_unlimited")}
            integer
            value={m.max_output_tokens}
            onCommit={(max_output_tokens) =>
              onUpdate((model) => {
                model.max_output_tokens = max_output_tokens;
              })
            }
          />
          <TextField
            label={t("models.aliases")}
            hint={t("models.f_aliases_hint")}
            value={m.aliases.join(", ")}
            onCommit={(v) =>
              onUpdate((model) => {
                model.aliases = v
                  .split(",")
                  .map((a) => a.trim())
                  .filter(Boolean);
              })
            }
            wide
          />
        </div>
        <div className="grid-two">
          <Toggle
            label={t("models.tool_use")}
            ariaLabel={t("models.tool_use")}
            checked={m.capabilities.tools}
            onChange={(v) =>
              onUpdate((model) => {
                model.capabilities.tools = v;
              })
            }
          />
          <Toggle
            label={t("models.vision")}
            ariaLabel={t("models.vision")}
            checked={m.capabilities.vision}
            onChange={(v) =>
              onUpdate((model) => {
                model.capabilities.vision = v;
              })
            }
          />
          <Toggle
            label={t("models.thinking")}
            ariaLabel={t("models.thinking")}
            checked={m.capabilities.thinking}
            onChange={(v) =>
              onUpdate((model) => {
                model.capabilities.thinking = v;
              })
            }
          />
        </div>
      </Section>

      <Section title={t("models.price")} hint={t("models.price_hint")}>
        <PriceFields
          pricing={m.pricing}
          onChange={(mutate) => onUpdate(mutate)}
        />
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
  /** Mutates the model being committed; see `Models.update`. */
  onChange: (mutate: (model: ModelEntry) => void) => void;
}) {
  const { t } = useI18n();
  const current = pricing ?? emptyPricing();

  /**
   * One field of the price, applied to the price as it is in the config being
   * committed. Rebuilding the object from the rendered `current` would bring
   * back the other three fields as they were before an in-flight save.
   */
  const patch = (change: Partial<Pricing>) => {
    onChange((model) => {
      const previous = model.pricing;
      const next = { ...(previous ?? emptyPricing()), ...change };
      // Naming a currency on its own does not make a model priced; the price
      // exists once one of the numbers does.
      model.pricing = change.currency !== undefined
        ? isPriced(next) ? next : previous
        : isPriced(next) ? next : null;
    });
  };

  return (
    <div className="controls">
      <TextField
        label={t("field.currency")}
        ariaLabel={t("field.currency")}
        hint={t("field.currency_hint")}
        value={current.currency}
        onCommit={(currency) => patch({ currency: currency.trim().toUpperCase() || "USD" })}
      />
      <NumberField
        label={t("field.input")}
        ariaLabel={t("models.input_price")}
        hint={t("field.input_hint")}
        min={0}
        value={current.input_per_mtok}
        onCommit={(v) => patch({ input_per_mtok: v ?? 0 })}
      />
      <NumberField
        label={t("field.output")}
        ariaLabel={t("models.output_price")}
        hint={t("field.output_hint")}
        min={0}
        value={current.output_per_mtok}
        onCommit={(v) => patch({ output_per_mtok: v ?? 0 })}
      />
      <NumberField
        label={t("models.cache_read")}
        hint={t("models.cache_read_hint")}
        min={0}
        placeholder={t("models.price_same_as_input")}
        value={current.cache_read_per_mtok}
        onCommit={(v) => patch({ cache_read_per_mtok: v })}
      />
      <NumberField
        label={t("models.cache_write")}
        hint={t("models.cache_write_hint")}
        min={0}
        placeholder={t("models.price_not_billed")}
        value={current.cache_write_per_mtok}
        onCommit={(v) => patch({ cache_write_per_mtok: v })}
      />
      {pricing && (
        <div className="field-actions">
          <Button
            kind="ghost"
            onClick={() =>
              onChange((model) => {
                model.pricing = null;
              })
            }
          >
            {t("models.clear_price")}
          </Button>
        </div>
      )}
    </div>
  );
}

/** Whether these numbers mean the model is priced at all. */
function isPriced(pricing: Pricing): boolean {
  return (
    pricing.input_per_mtok > 0 ||
    pricing.output_per_mtok > 0 ||
    pricing.cache_read_per_mtok !== null ||
    pricing.cache_write_per_mtok !== null
  );
}
