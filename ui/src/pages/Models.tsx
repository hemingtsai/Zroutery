/**
 * The model list. One row per model: name, provider, class, one small status
 * dot. Everything else — pricing, capabilities, aliases, the knobs — lives in
 * the drawer, because a list is for choosing and a drawer is for knowing.
 *
 * The drawer, the list row, the catalogue import and the price formatting
 * live in `pages/models/`; this file keeps the page state and the save
 * handlers. The tested pure helpers are re-exported here so the test and
 * the provider page keep their import paths.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  type AppConfig,
  emptyModel,
  modelRows,
  modelName,
  ms,
  previewId,
  tierName,
  type DiscoveredModel,
  type ModelEntry,
  type ModelRow,
  type Snapshot,
} from "../api";
import { useI18n } from "../i18n";
import {
  Banner,
  Button,
  ConfirmDialog,
  num,
  PageHead,
  Section,
  Select,
  useToast,
  type ConfirmRequest,
} from "../components";
import { CatalogueImport } from "./models/CatalogueImport";
import { ModelDrawer } from "./models/ModelDrawer";
import { ModelListRow, type ListTone } from "./models/ModelListRow";
import { modelPriceText } from "./models/priceText";
import { revealAddFormLanding } from "./models/reveal";

/** Rows per page. The same page size the Activity log's pager (U8) cuts at. */
const MODEL_PAGE_SIZE = 50;



/**
 * Whether a capability set is still the shape an entry is created with.
 *
 * A sync fills capabilities only from this state: anything else is a decision
 * the user made, and models.dev is not allowed to overrule it.
 */
function isDefaultCapabilities(capabilities: ModelEntry["capabilities"]) {
  return (
    capabilities.tools &&
    !capabilities.vision &&
    !capabilities.thinking &&
    !capabilities.structured_output &&
    !capabilities.audio &&
    !capabilities.video &&
    !capabilities.files
  );
}

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
   *
   * The display order is the provider's name, then the model id — the order
   * the configuration file happens to store rows in is no order at all, and at
   * thirty-one models a list where one provider's models sit scattered across
   * the whole height is a list that has to be read end to end to answer "what
   * does OpenCode Zen expose". `numeric` keeps v4.10 after v4.9.
   */
  const rows = useMemo(() => {
    const list = modelRows(snapshot);
    const providerName = new Map(
      snapshot.config.providers.map((p) => [p.id, p.name.toLowerCase()]),
    );
    return [...list].sort(
      (a, b) =>
        (providerName.get(a.model.provider_id) ?? "").localeCompare(
          providerName.get(b.model.provider_id) ?? "",
        ) || modelName(a.model).localeCompare(modelName(b.model), undefined, { numeric: true }),
    );
  }, [snapshot.config.models, snapshot.config.providers, snapshot.exposed_ids]);
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  /*
   * The list is a page at a time, cut on the client — cc-switch's table shape
   * (its TablePagination: the data arrives whole, the pages are sliced in the
   * browser) and the Activity log's pager (U8) converted for the catalogue:
   * the same page size, the same three copy keys, the same hide-on-single-page.
   * The P2 baseline measured this page mounting 600 rows at once — a 235-308ms
   * hot switch, fifteen times the 16ms budget — and fifty rows land the same
   * interaction back inside a frame.
   *
   * Why pages rather than virtualization: virtualization buys an unbounded DOM
   * by unmounting everything off-screen, and the price is threefold here —
   * a windowing dependency (this repo adds none for a bounded table; the
   * charts are hand-drawn SVG for the same reason), measured row heights (rows
   * are content-driven: the sub-line wraps where it likes, and a virtualizer
   * needs fixed or measured heights to place its window), and a wounded
   * document (find-in-page, the accessibility tree and the search semantics
   * below all address the whole set, while a virtual list only ever presents
   * its window). A configured catalogue is bounded — hundreds of rows, not a
   * ledger that grows forever — so fifty real rows per page is the whole job,
   * and virtualization stays reserved for the genuinely infinite list.
   */
  const [page, setPage] = useState(0);
  const [draft, setDraft] = useState({ provider_id: config.providers[0]?.id ?? "", upstream_model: "" });
  const [syncing, setSyncing] = useState(false);
  // The add form unfolds from the page head; the list stays the subject.
  const [adding, setAdding] = useState(false);
  /*
   * …and when it opens, the reader is handed it: the landing under the head
   * is brought into the viewport (a no-op from the top, where the trigger
   * put them) and the focus lands in the form's first input. The behavior
   * and its order live in `revealAddFormLanding`; StrictMode's double effect
   * just lands the same focus twice.
   */
  const addLandingRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!adding) return;
    revealAddFormLanding(addLandingRef.current, "input");
  }, [adding]);

  /**
   * Fill configured models' missing metadata from models.dev.
   *
   * `fetch_provider_models` enriches every catalogue with models.dev, so this
   * reaches the same source an import does. Only unset fields are filled: a
   * context window, an output cap or an effort list the user already entered
   * is a correction, and a capability set that differs from the default shape
   * was edited by hand. Nothing is overwritten, which is what makes a later
   * sync safe to run again.
   */
  const syncMetadata = async () => {
    setSyncing(true);
    try {
      const byModel = new Map<string, DiscoveredModel>();
      const failures: string[] = [];
      for (const provider of config.providers) {
        if (provider.kind === "decision") continue;
        if (!config.models.some((m) => m.provider_id === provider.id)) continue;
        try {
          const found = await api.fetchModels(provider);
          for (const entry of found) {
            byModel.set(`${provider.id}\u0000${entry.id}`, entry);
          }
        } catch {
          failures.push(provider.name);
        }
      }

      let changed = 0;
      await save((current) => {
        const next = structuredClone(current);
        for (const model of next.models) {
          const found = byModel.get(`${model.provider_id}\u0000${model.upstream_model}`);
          if (!found) continue;
          let touched = false;
          if (model.context_window === null && found.context_window != null) {
            model.context_window = found.context_window;
            touched = true;
          }
          if (model.max_output_tokens === null && found.max_output != null) {
            model.max_output_tokens = found.max_output;
            touched = true;
          }
          if (
            (model.effort_levels ?? []).length === 0 &&
            !model.reasoning_capability?.manual &&
            found.effort_levels?.length
          ) {
            model.effort_levels = found.effort_levels;
            touched = true;
          }
          if (found.capabilities && isDefaultCapabilities(model.capabilities)) {
            model.capabilities = found.capabilities;
            touched = true;
          }
          if (touched) changed += 1;
        }
        return changed > 0 ? next : null;
      });

      if (changed > 0) notify("ok", t("models.synced", { n: changed }));
      else notify("ok", t("models.sync_none"));
      if (failures.length > 0) {
        notify("error", t("models.sync_failed", { names: failures.join(", ") }));
      }
    } finally {
      setSyncing(false);
    }
  };

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

  /**
   * Every name a model can be reached by, and which model owns it.
   *
   * The exposed id and the aliases share one namespace: validation refuses a
   * collision between them, so an add or a rename that would collide must be
   * refused before the save, where the operator can see which name is taken.
   */
  const takenNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const row of rows) {
      names.set(row.id, row.id);
      for (const alias of row.model.aliases) names.set(alias, row.id);
    }
    return names;
  }, [rows]);

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
        // The price line under the name (A5): `modelPriceText` owns the
        // states a reader must be able to tell apart — no price configured,
        // a configured zero, and the side of a one-sided price that was
        // never entered — plus the trimmed display and the raw `title`
        // behind it. Resolved here like the other strings, so the
        // per-keystroke render path stays free of it.
        const price = modelPriceText(r.model.pricing, t);
        return {
          id: r.id,
          name: modelName(r.model),
          status,
          selected: openId === r.id,
          providerLine:
            (provider?.name ?? t("models.missing_provider")) +
            (provider?.enabled === false ? ` ${t("providers.provider_off")}` : "") +
            (r.model.tier ? ` · ${tierName(r.model.tier, t)}` : ` · ${t("models.no_tier")}`),
          price: price.line,
          priceTitle: price.title,
          latency: healthRow ? ms(healthRow.avg_latency_ms) : "",
          // One lower-cased haystack per row, built in the same pass as the
          // other strings: the search box then costs a `includes` per row
          // instead of four field reads and a join on every keystroke.
          search: [
            r.id,
            r.model.upstream_model,
            r.model.display_name ?? "",
            provider?.name ?? "",
            ...r.model.aliases,
          ]
            .join(" ")
            .toLowerCase(),
        };
      }),
    [rows, providerById, healthById, cooling, openId, t],
  );

  /** The rows the search box lets through; empty query means every row. */
  const view = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return needle
      ? listView.filter((row) => row.search.includes(needle))
      : listView;
  }, [listView, query]);

  /*
   * The window: search filters the full set, and the page cuts the filtered
   * result — never the other way round, or a match on page 3 would be
   * invisible to a reader sitting on page 1. The page number is clamped
   * rather than reset when the set shrinks under it (a save removed models
   * while the reader sat near the end): the same snap-back the Activity pager
   * uses, so a page that runs past the end shows its last real page instead
   * of an empty table.
   */
  const pageCount = Math.max(1, Math.ceil(view.length / MODEL_PAGE_SIZE));
  const safePage = Math.min(page, pageCount - 1);
  const pageRows = view.slice(safePage * MODEL_PAGE_SIZE, (safePage + 1) * MODEL_PAGE_SIZE);

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
      // Vision is the other reference a removal used to leave dangling: the
      // configured model no longer exists, so the image path silently degraded
      // to the placeholder.
      if (next.vision.model !== null && gone.includes(next.vision.model)) {
        next.vision.model = null;
      }
      return next;
    });
    setOpenId(null);
  };

  /**
   * Rename a model, and move everything that named it by its exposed id.
   *
   * The upstream name *is* the identity: the exposed id is derived from it, so
   * a rename is a new model to every reference in the configuration. The Auto
   * Mode pool and the vision model are rewritten in the same save; an alias a
   * reference used does not change, because the alias belongs to the model.
   *
   * Returns the reason it could not be done, or null. The drawer keeps the
   * error visible under the field — a backend refusal arrives only after the
   * optimistic input has already changed.
   */
  const rename = async (row: ModelRow, raw: string): Promise<string | null> => {
    const next = raw.trim();
    if (!next) return t("models.rename_required");
    if (next === row.model.upstream_model) return null;
    const newId = previewId(row.model.provider_id, next);
    if (newId !== row.id && takenNames.has(newId)) {
      return t("models.name_taken", { id: newId });
    }

    let committed = false;
    const ok = await save((cfg) => {
      const model = cfg.models.find(
        (m) =>
          m.provider_id === row.model.provider_id &&
          m.upstream_model === row.model.upstream_model,
      );
      if (!model) return null;
      if (
        cfg.models.some(
          (m) => m !== model && m.aliases.includes(newId),
        )
      ) {
        return null;
      }
      model.upstream_model = next;
      for (const candidate of cfg.classifier.candidates) {
        if (candidate.model === row.id) candidate.model = newId;
      }
      if (cfg.vision.model === row.id) cfg.vision.model = newId;
      committed = true;
      return cfg;
    });
    if (!ok || !committed) return t("models.rename_failed");
    setOpenId(newId);
    return null;
  };

  const add = async () => {
    const upstream = draft.upstream_model.trim();
    const providerId = draft.provider_id;
    if (!providerId || !upstream) {
      notify("error", t("models.pick_provider_name"));
      return;
    }
    const id = previewId(providerId, upstream);
    if (takenNames.has(id)) {
      setNameError(
        config.models.some(
          (m) => m.provider_id === providerId && m.upstream_model === upstream,
        )
          ? t("models.duplicate", { model: upstream })
          : t("models.name_taken", { id }),
      );
      return;
    }
    let committed = false;
    const ok = await save((cfg) => {
      if (
        cfg.models.some(
          (m) => m.provider_id === providerId && m.upstream_model === upstream,
        )
      ) {
        return null;
      }
      const next = structuredClone(cfg);
      next.models.push(emptyModel(providerId, upstream));
      committed = true;
      return next;
    });
    if (ok && committed) {
      setDraft({ ...draft, upstream_model: "" });
      setNameError(null);
    } else if (ok) {
      // The queued save found the model already there: a race, not a crash.
      setNameError(t("models.duplicate", { model: upstream }));
    }
  };

  const open = rows.find((r) => r.id === openId) ?? null;

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      {unclassified.length > 0 && (
        <Banner tone="warn">
          {t("models.unclassified", {
            n: unclassified.length,
            ids: unclassified.map((r) => modelName(r.model)).join(", "),
          })}
        </Banner>
      )}

      <PageHead
        lede={
          rows.length === 0 ? t("models.lede_none") : t("models.exposed", { n: rows.length })
        }
        actions={
          <button type="button" className="linky" onClick={() => setAdding((open) => !open)}>
            {adding ? t("common.collapse") : t("models.add")}
          </button>
        }
      />

      {/*
       * The add form unfolds from the page head's action, the way cc-switch
       * opens its wizard from the header button: the list is the page's
       * subject, and a permanently expanded form under forty-four rows was
       * furniture before it was a tool. It mounts here, above that list —
       * the old berth under the pager surfaced it a whole page-length below
       * the button that opened it once the catalogue grew (the review's O2)
       * — so the landing is the head the reader is standing at, and the
       * effect beside `adding` hands the focus to the form's first input.
       * The layout harness unfolds it the same way a reader does, by
       * clicking the head link.
       */}
      {adding && (
        <div ref={addLandingRef}>
          <Section title={t("models.add_section")} hint={t("models.add_hint")}>
            <div className="controls">
              <Select
                ariaLabel={t("field.provider")}
                value={draft.provider_id || null}
                onChange={(provider_id) => setDraft({ ...draft, provider_id })}
                placeholder={t("field.provider")}
                options={config.providers.map((p) => ({
                  value: p.id,
                  // A decision model is added by name like any other model, but it
                  // is worth saying which providers answer that way: it is the
                  // difference between a chat entry and a typed-question one.
                  label:
                    p.kind === "decision"
                      ? `${p.name} · ${t("providers.decision_dialect")}`
                      : p.name,
                }))}
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
                onKeyDown={(e) => {
                  if (e.key === "Enter") void add();
                }}
              />
              <Button
                kind="primary"
                onClick={() => void add()}
                disabled={busy || !config.providers.length}
              >
                {t("models.add")}
              </Button>
            </div>
            {draft.upstream_model.trim() && draft.provider_id && (
              <p className="field-hint">
                {t("models.exposed_as", {
                  id: previewId(draft.provider_id, draft.upstream_model),
                })}
              </p>
            )}
            {providerById.get(draft.provider_id)?.kind === "decision" && (
              <p className="field-hint">{t("providers.decision_no_catalogue")}</p>
            )}
            {nameError && <p className="field-hint field-hint-danger">{nameError}</p>}
            {!config.providers.length && <p className="field-hint">{t("models.add_provider_first")}</p>}
            {draft.provider_id && (
              <CatalogueImport
                key={draft.provider_id}
                provider={providerById.get(draft.provider_id)}
                configured={
                  new Set(
                    config.models
                      .filter((m) => m.provider_id === draft.provider_id)
                      .map((m) => m.upstream_model),
                  )
                }
                save={save}
                busy={busy}
              />
            )}
          </Section>
        </div>
      )}

      {rows.length > 0 && (
        <div className="controls">
          <input
            aria-label={t("models.search")}
            placeholder={t("models.search")}
            value={query}
            onChange={(e) => {
              setQuery(e.currentTarget.value);
              // A page number is meaningless across a change of denominator:
              // the query just changed the set being paged, so start over.
              // (The rule the Activity pager applies to its filters.)
              setPage(0);
            }}
          />
          {/*
           * The showing hint and the pager both count, so they take turns:
           * the hint speaks only while a search is on AND the result fits on
           * one page. The moment a second page exists, the pager's own status
           * line ("1–50 of N") already announces the filtered total — two
           * different totals on one screen is arithmetic the reader should
           * not have to check. (The other split on the table — pager only
           * while not searching — was rejected: it would have forced search
           * results to render unpaginated, putting the 600-row cost this page
           * just shed behind the first keystroke of a broad query.)
           */}
          {query.trim() && pageCount === 1 && (
            <span className="muted">
              {t("models.showing", { shown: view.length, total: rows.length })}
            </span>
          )}
          <Button
            kind="ghost"
            disabled={busy || syncing}
            onClick={() => void syncMetadata()}
          >
            {syncing ? t("models.syncing") : t("models.sync")}
          </Button>
        </div>
      )}

      {rows.length === 0 ? (
        <div className="empty-state">
          <p>{t("models.empty")}</p>
          <p className="muted">{t("models.empty_hint")}</p>
        </div>
      ) : view.length === 0 ? (
        <p className="empty">{t("models.no_match", { query: query.trim() })}</p>
      ) : (
        <div className="list">
          {pageRows.map((row) => (
            <ModelListRow key={row.id} {...row} onOpen={openRow} />
          ))}
        </div>
      )}

      {/*
       * The pager, the Activity log's (U8) verbatim structure: one quiet line
       * under the rows — where you are, how many there are, one step each
       * way. It hides itself on a single page (a control for a choice nobody
       * has) and stays visible while a search is on, because a broad query can
       * match more than a page and a reader must be able to reach page two.
       * The copy keys are the log's three, reused so both pagers say the same
       * words in every language — hence the "activity." prefix on this page.
       */}
      {pageCount > 1 && (
        <div className="pager">
          <span className="muted">
            {t("activity.page_status", {
              from: safePage * MODEL_PAGE_SIZE + 1,
              to: Math.min((safePage + 1) * MODEL_PAGE_SIZE, view.length),
              total: num(view.length),
            })}
          </span>
          <div className="row gap">
            <button
              type="button"
              className="linky"
              disabled={safePage === 0}
              onClick={() => setPage(safePage - 1)}
            >
              {t("activity.prev_page")}
            </button>
            <button
              type="button"
              className="linky"
              disabled={safePage >= pageCount - 1}
              onClick={() => setPage(safePage + 1)}
            >
              {t("activity.next_page")}
            </button>
          </div>
        </div>
      )}

      {open && (
        <ModelDrawer
          row={open}
          provider={config.providers.find((p) => p.id === open.model.provider_id)}
          health={health.find((h) => h.model_id === open.id)}
          busy={busy}
          namingStyle={config.routing.naming_style}
          onClose={() => setOpenId(null)}
          onUpdate={(mutate) => update(open.id, mutate)}
          onRename={(next) => rename(open, next)}
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

// The tested helpers and the shared landing hand-off keep their old import

// path (the test file and the provider page both say `from "./Models"`).

export { formatPriceNumber, modelPriceText } from "./models/priceText";

export { revealAddFormLanding } from "./models/reveal";
