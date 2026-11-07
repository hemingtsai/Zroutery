/**
 * The routing page: tier pools, vision fallback, rectifiers and Auto Mode,
 * as tabs; the drawers and the pool live in `pages/routing/`.
 */
import { useMemo, useState } from "react";
import {
  applyTierOrder,
  orderCandidatesByPriority,
  api,
  modelRows,
  tierMembers,
  TIERS,
  virtualId,
  type AppConfig,
  type ModelTier,
  type ModelRow,
  type RectifierConfig,
  type Snapshot,
} from "../../api";
import { useI18n } from "../../i18n";
import type { TaskKind } from "../../saveQueue";
import { CommitInput, PageHead, Section, Select, Tabs } from "../../components";
import { AutoModeDrawer } from "./AutoModeDrawer";
import { DEFAULT_RECTIFIER, RECTIFIERS, STRATEGY_LABEL, TIER_HINT_KEY, DefaultDrawer } from "./DefaultDrawer";
import { RoutePool } from "./RoutePool";
import MlRouting from "./ml/MlRouting";

export default function Routing({
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
  const { config, health } = snapshot;
  const { t } = useI18n();
  const [edit, setEdit] = useState<"default" | "auto" | null>(null);

  /*
   * The route picture, derived from the configuration rather than from the
   * render.
   *
   * The page's own state is which tab is open and which drawer is mounted,
   * and neither of those changes what a request reaches — but both change far
   * more often than the configuration does. Everything below is keyed on the
   * configuration, so switching tabs or opening a drawer costs one render of
   * the picture rather than another pass over the model list.
   */
  const rows = useMemo(
    () => modelRows(snapshot),
    [snapshot.config.models, snapshot.exposed_ids],
  );

  /**
   * The providers, by id.
   *
   * Every route in the flow asked the provider list for a name by scanning
   * it, and the vision fallback asked the same question once per model: one
   * render cost O(models x providers). One map answers both.
   */
  const providerById = useMemo(
    () => new Map(config.providers.map((p) => [p.id, p])),
    [config.providers],
  );

  const cooling = useMemo(
    () =>
      new Set(
        health.filter((h) => h.cooldown_remaining_secs > 0).map((h) => h.model_id),
      ),
    [health],
  );

  /**
   * A drag or an arrow press in a pool lands here as the full desired order.
   *
   * The write is a priority renumber (applyTierOrder), not an array move — the
   * pool order *is* the priority order everywhere else, so this is the one
   * place that decides it. A null from the helper means the config moved
   * under the gesture; "nothing to change" is the honest answer to a drop
   * that no longer names the models it was made on.
   */
  const reorderTier = (
    tier: ModelTier,
    order: { provider_id: string; upstream_model: string }[],
  ) => {
    void save((cfg) => {
      const models = applyTierOrder(cfg.models, tier, order);
      if (!models) return null;
      return { ...cfg, models };
    });
  };

  const tierRoutes = useMemo(
    () => TIERS.map((tier) => ({ tier, members: tierMembers(rows, config.providers, tier) })),
    [rows, config.providers],
  );

  const patchVision = (patch: Partial<AppConfig["vision"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.vision, patch);
      return next;
    });
  };

  /**
   * Rectifiers mutate one flag at a time and each toggle is its own save.
   *
   * Taking a mutator rather than a partial keeps a computed key honest: the
   * row table names the field, and `r[field] = checked` stays typed as the
   * booleans `RectifierConfig` declares. A document written before the
   * rectifiers existed carries no `rectifier` object at all, so the object is
   * created here with the backend's own defaults rather than read as absent.
   */
  const patchRectifier = (mutate: (rectifier: RectifierConfig) => void) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      next.routing.rectifier = {
        ...DEFAULT_RECTIFIER,
        ...next.routing.rectifier,
      };
      mutate(next.routing.rectifier);
      return next;
    });
  };

  /*
   * The models the vision fallback is allowed to choose from.
   *
   * Three conditions, not one: a model has to be enabled, its provider has to be
   * enabled, and it has to actually accept images. The third is the one that
   * matters here — offering a model that cannot see as the thing that describes
   * images would make the fallback substitute one failure for another.
   */
  const visionCapable = useMemo(
    () =>
      rows.filter(
        (r) =>
          r.model.enabled &&
          r.model.capabilities.vision &&
          providerById.get(r.model.provider_id)?.enabled,
      ),
    [rows, providerById],
  );

  // The pool in the order the router would try it. Every strategy pool is
  // ranked by priority, so the page follows the numbers rather than the array.
  const classifierCandidates = useMemo(() => {
    if (!config.classifier.enabled) return [];
    return orderCandidatesByPriority(
      config.classifier.candidates
        .filter((c) => c.enabled)
        .map((c) => ({
          priority: c.priority,
          model: c.model,
          row: rows.find((r) => r.id === c.model || r.model.aliases.includes(c.model)),
        }))
        .filter((c): c is typeof c & { row: ModelRow } => Boolean(c.row)),
    );
  }, [config.classifier.enabled, config.classifier.candidates, rows]);

  const [tab, setTab] = useState<"ml" | "main" | "auto">("main");

  // The two route bodies, hoisted so the tab strip below can place them
  // without the two subjects and the tab shell duplicating any markup.
  const mainRoutes = (
    <Section
      title={t("route.default")}
      hint={t("routing.default_hint")}
      actions={
        <button type="button" className="linky" onClick={() => setEdit("default")}>
          {t("routing.edit")}
        </button>
      }
    >
      <div className="list">
        <div className="flow">
          {tierRoutes.map(({ tier, members }) => (
            <div className="flow-row" key={tier}>
              <div className="flow-kind">
                <span className="flow-kind-name mono">{virtualId(tier, config.routing.naming_style)}</span>
                <span className="flow-kind-hint">{t(TIER_HINT_KEY[tier])}</span>
              </div>
              <div className="flow-routes">
                {members.length === 0 ? (
                  <span className="flow-empty">
                    {t("routing.tier_empty", { id: virtualId(tier, config.routing.naming_style) })}
                  </span>
                ) : (
                  <RoutePool
                    tier={tier}
                    members={members}
                    firstLabel={t("routing.request")}
                    cooling={cooling}
                    providerName={(id: string) => providerById.get(id)?.name}
                    busy={busy}
                    onReorder={reorderTier}
                  />
                )}
              </div>
            </div>
          ))}
        </div>
        {/*
         * The honest footnote for the pools above. Under Priority the order
         * shown is the order attempted. Under every other strategy the live
         * order belongs to the election or the cursor, and the numbers here
         * are the baseline those start from — a hand that reorders must know
         * which one it is moving.
         */}
        {config.routing.strategy !== "priority" && (
          <p className="field-hint">
            {t("routing.strategy_order_note", {
              strategy: t(STRATEGY_LABEL[config.routing.strategy]),
            })}
          </p>
        )}
      </div>
    </Section>
  );

  /*
   * Vision fallback, here rather than on the settings page.
   *
   * It is not an appearance or a machine preference: when the chosen model
   * cannot see, a different model is asked what the image contains and its
   * answer is sent in place of the image. That is a decision about what a
   * request contains, made before the request is routed, and it belongs on the
   * page whose subject is what happens to a request.
   */
  const visionFallback = (
    <Section title={t("vision.title")} hint={t("vision.hint")}>
      <div className="setting-row">
        <div className="setting-left">
          <span className="setting-title">{t("vision.enable")}</span>
        </div>
        <div className="setting-right">
          <input
            type="checkbox"
            aria-label={t("vision.enable")}
            checked={config.vision.enabled}
            onChange={(e) => patchVision({ enabled: e.currentTarget.checked })}
          />
        </div>
      </div>
      {config.vision.enabled && (
        <>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("vision.model")}</span>
              <span className="setting-desc">{t("vision.model_hint")}</span>
            </div>
            <div className="setting-right">
              <Select
                ariaLabel={t("vision.model")}
                value={config.vision.model}
                onChange={(model) => patchVision({ model: model || null })}
                placeholder="—"
                options={visionCapable.map((r) => ({ value: r.id, label: r.id }))}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("vision.placeholder")}</span>
              <span className="setting-desc">{t("vision.placeholder_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitInput
                value={config.vision.placeholder}
                ariaLabel={t("vision.placeholder")}
                onCommit={(placeholder) =>
                  placeholder.trim() && patchVision({ placeholder })
                }
              />
            </div>
          </div>
          {visionCapable.length === 0 && (
            <p className="field-hint">{t("vision.model_required")}</p>
          )}
        </>
      )}
    </Section>
  );

  /*
   * Rectifiers, on this page for the same reason as the vision fallback: they
   * decide what leaves for an upstream. Each is a second attempt at the same
   * provider with a repaired request, not a failover, so they read as repairs
   * rather than as routing.
   */
  const rectifier = {
    ...DEFAULT_RECTIFIER,
    ...config.routing.rectifier,
  };
  const rectifiers = (
    <Section title={t("rectifier.title")} hint={t("rectifier.hint")}>
      <div className="setting-row">
        <div className="setting-left">
          <span className="setting-title">{t("rectifier.enable")}</span>
          <span className="setting-desc">{t("rectifier.enable_hint")}</span>
        </div>
        <div className="setting-right">
          <input
            type="checkbox"
            aria-label={t("rectifier.enable")}
            checked={rectifier.enabled}
            onChange={(e) =>
              patchRectifier((r) => {
                r.enabled = e.currentTarget.checked;
              })
            }
          />
        </div>
      </div>
      {rectifier.enabled &&
        RECTIFIERS.map((row) => (
          <div className="setting-row" key={row.field}>
            <div className="setting-left">
              <span className="setting-title">{t(row.label)}</span>
              <span className="setting-desc">{t(row.hint)}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t(row.label)}
                checked={rectifier[row.field]}
                onChange={(e) =>
                  patchRectifier((r) => {
                    r[row.field] = e.currentTarget.checked;
                  })
                }
              />
            </div>
          </div>
        ))}
    </Section>
  );

  const autoReview = (
    <Section
      title={t("route.auto_review")}
      hint={config.classifier.enabled
        ? t("routing.review_hint_on")
        : t("routing.review_hint_off")}
      actions={
        <button type="button" className="linky" onClick={() => setEdit("auto")}>
          {t("routing.edit")}
        </button>
      }
    >
      <div className="list">
        <div className="flow">
          <div className="flow-row">
            <div className="flow-kind">
              <span className="flow-kind-name">{t("routing.side_query")}</span>
              <span className="flow-kind-hint">
                {config.classifier.enabled
                  ? t("route.review_hint_on")
                  : t("route.review_hint_off")}
              </span>
            </div>
            <div className="flow-routes">
              {config.classifier.enabled ? (
                classifierCandidates.length > 0 ? (
                  <RoutePool
                    members={classifierCandidates.map((c) => c.row)}
                    firstLabel={
                      config.classifier.strategy === "priority"
                        ? t("routing.verdict")
                        : t("routing.next")
                    }
                    cooling={cooling}
                    providerName={(id) => providerById.get(id)?.name}
                  />
                ) : (
                  <span className="flow-empty">{t("routing.review_no_candidate")}</span>
                )
              ) : (
                <span className="flow-empty">{t("routing.review_off")}</span>
              )}
            </div>
          </div>
        </div>
      </div>
    </Section>
  );

return (
    <>
      <PageHead lede={t("routing.lede")} />

      {/*
        Three subjects, one screen. The learned model, the ordinary tier routes
        and the classifier pool each answer a different question, and stacked
        they made a reader scroll past two of them to reach the third. As tabs
        each is one click from any other and the page stops growing downward.
        A build without the learning stack has no first tab, so it opens on the
        ordinary routes instead.
      */}
      <Tabs
        value={tab}
        onChange={(next) => setTab(next as "ml" | "main" | "auto")}
        ariaLabel={t("routing.tab_group")}
        tabs={[
          ...(snapshot.ml_available
            ? [
                {
                  value: "ml",
                  label: t("routing.tab_ml"),
                  content: <MlRouting available={snapshot.ml_available} snapshot={snapshot} save={save} />,
                },
              ]
            : []),
          {
            value: "main",
            label: t("routing.tab_main"),
            content: (
              <>
                {mainRoutes}
                {visionFallback}
                {rectifiers}
              </>
            ),
          },
          { value: "auto", label: t("routing.tab_auto"), content: autoReview },
        ]}
      />

      {edit === "default" && (
        <DefaultDrawer
          snapshot={snapshot}
          busy={busy}
          onClose={() => setEdit(null)}
          onSave={(mutate) =>
            save((cfg) => {
              const next = mutate(cfg);
              return next;
            })
          }
          // Probing the tier members is a runtime reading, not a
          // configuration write.
          onElection={() => run(api.runElection, "observe")}
        />
      )}
      {edit === "auto" && (
        <AutoModeDrawer
          snapshot={snapshot}
          busy={busy}
          onClose={() => setEdit(null)}
          onSave={(mutate) => save(mutate)}
        />
      )}
    </>
  );
}
