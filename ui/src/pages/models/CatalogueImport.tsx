/**
 * Bulk catalogue import: fetch a provider's models.dev listing, filter,
 * multi-select, and add the chosen models in one save.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import {
  TIERS,
  type AppConfig,
  api,
  contextText,
  emptyModel,
  errorText,
  virtualId,
  type DiscoveredModel,
  type ModelTier,
  type Provider,
} from "../../api";
import { describeBackendError, useI18n } from "../../i18n";
import { Badge, Button, Select, Toggle, useToast } from "../../components";

/**
 * Add several models from the selected provider's own catalogue.
 *
 * The provider drawer could already add one catalogue entry at a time, with a
 * tier chosen per row. This is the bulk path: search, filter, select, one tier
 * for the batch, one save. It reads the same `fetch_provider_models` command
 * and copies the same fields — prices, the context window, the output cap and
 * the capability flags — so an imported model is not a poorer entry than one
 * typed by hand.
 *
 * A decision provider has no catalogue to browse, so the control is absent
 * there rather than present and empty.
 */
export function CatalogueImport({
  provider,
  configured,
  save,
  busy,
}: {
  provider: Provider | undefined;
  /** Upstream names this provider already has. */
  configured: Set<string>;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const [open, setOpen] = useState(false);
  const [models, setModels] = useState<DiscoveredModel[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [freeOnly, setFreeOnly] = useState(provider?.kind === "opencode_zen_free");
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [tier, setTier] = useState<ModelTier | "none">("none");
  // Which catalogue request is allowed to answer; unmounting bumps it.
  const generation = useRef(0);
  useEffect(
    () => () => {
      generation.current += 1;
    },
    [],
  );

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return (models ?? []).filter(
      (d) =>
        (!freeOnly || d.free) &&
        (!needle || d.id.toLowerCase().includes(needle)),
    );
  }, [models, freeOnly, query]);

  if (!provider || provider.kind === "decision") return null;

  const load = async () => {
    const request = ++generation.current;
    setLoading(true);
    try {
      const found = await api.fetchModels(provider);
      if (request !== generation.current) return;
      setModels(found);
      if (found.length === 0) {
        notify("error", t("providers.empty_catalogue", { name: provider.name }));
      }
    } catch (e) {
      if (request !== generation.current) return;
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      if (request === generation.current) setLoading(false);
    }
  };

  const toggle = (id: string) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const chosenCount = visible.filter(
    (d) => selected.has(d.id) && !configured.has(d.id),
  ).length;

  const addSelected = async () => {
    const chosen = visible.filter(
      (d) => selected.has(d.id) && !configured.has(d.id),
    );
    if (chosen.length === 0) return;
    const ok = await save((cfg) => {
      const next = structuredClone(cfg);
      for (const entry of chosen) {
        if (
          next.models.some(
            (m) => m.provider_id === provider.id && m.upstream_model === entry.id,
          )
        ) {
          continue;
        }
        next.models.push(
          emptyModel(provider.id, entry.id, {
            tier: tier === "none" ? null : tier,
            pricing: entry.pricing,
            context_window: entry.context_window ?? null,
            max_output_tokens: entry.max_output ?? null,
            ...(entry.capabilities ? { capabilities: entry.capabilities } : {}),
            effort_levels: entry.effort_levels ?? [],
          }),
        );
      }
      return next;
    });
    if (ok) {
      setSelected(new Set());
      notify("ok", t("models.imported", { n: chosen.length }));
    }
  };

  return (
    <>
      <div className="controls">
        <Button
          kind="ghost"
          disabled={busy || loading}
          onClick={() => {
            const next = !open;
            setOpen(next);
            if (next && models === null) void load();
          }}
        >
          {open ? t("models.import_hide") : t("models.import_browse")}
        </Button>
        {loading && <span className="muted">{t("common.loading")}</span>}
      </div>
      {open && models !== null && (
        <>
          <div className="controls">
            <input
              aria-label={t("models.import_search")}
              placeholder={t("models.import_search")}
              value={query}
              onChange={(e) => setQuery(e.currentTarget.value)}
            />
            {provider.kind === "opencode_zen_free" && (
              <Toggle
                label={t("providers.free_only")}
                hint={t("providers.free_only_hint")}
                checked={freeOnly}
                onChange={setFreeOnly}
              />
            )}
            <Select<ModelTier | "none">
              ariaLabel={t("models.import_tier")}
              value={tier}
              onChange={setTier}
              options={[
                { value: "none", label: t("models.no_tier") },
                ...TIERS.map((c) => ({ value: c, label: virtualId(c) })),
              ]}
            />
            <Button
              kind="primary"
              disabled={busy || chosenCount === 0}
              onClick={() => void addSelected()}
            >
              {t("models.import_add", { n: chosenCount })}
            </Button>
          </div>
          <p className="muted">
            {freeOnly
              ? t("providers.catalogue_shown", {
                  shown: visible.length,
                  total: models.length,
                })
              : t("providers.catalogue_total", { total: models.length })}
          </p>
          {visible.length === 0 ? (
            <p className="empty">
              {freeOnly
                ? t("providers.catalogue_no_free", { total: models.length })
                : t("models.import_empty")}
            </p>
          ) : (
            <div className="list">
              {visible.map((entry) => (
                <label key={entry.id} className={`list-row ${configured.has(entry.id) ? "muted" : ""}`}>
                  <input
                    type="checkbox"
                    aria-label={entry.id}
                    checked={configured.has(entry.id) || selected.has(entry.id)}
                    disabled={configured.has(entry.id)}
                    onChange={() => toggle(entry.id)}
                  />
                  <span className="row-title mono truncate" title={entry.id}>
                    {entry.id}
                  </span>
                  {entry.context_window != null && (
                    <span className="muted mono">{contextText(entry.context_window)}</span>
                  )}
                  {entry.free && <Badge>{t("providers.free_badge")}</Badge>}
                  {configured.has(entry.id) && (
                    <Badge tone="ok">{t("models.import_added")}</Badge>
                  )}
                </label>
              ))}
            </div>
          )}
        </>
      )}
    </>
  );
}
