import type { MouseEvent as ReactMouseEvent } from "react";
import {
  api,
  applyTierOrder,
  isSideKind,
  modelRows,
  modelName,
  money,
  ms,
  tierMembers,
  TIERS,
  virtualId,
  type AppConfig,
  type BalanceStatus,
  type ModelTier,
  type RoutingStrategy,
  type Snapshot,
} from "../api";
import { StatusDot, useToast } from "../components";
import { useI18n, type TKey } from "../i18n";
import { useMode, type UiMode } from "../mode";
import { RoutePool } from "./Routing";

/**
 * Overview answers three questions and stops: what is connected, where
 * requests go, what happened just now. No hero copy, no counters wall — the
 * information is the layout, set in type and separated by rules.
 *
 * The routing picture is client-agnostic: "Default" and "Auto review" are
 * route purposes, not applications. Which client asked is Activity's
 * business — there it is data, here it would be an assumption.
 *
 * Simple mode turns the first two answers into one workbench: the tier rows
 * become the routing page's own pools (a drag here rewrites the same priority
 * the router reads), and each enabled provider answers with its balance and
 * model count, its row a door to the page that edits it — never an editor
 * itself. Geek mode renders exactly what it always did; the mode filters
 * visibility and changes no behaviour.
 */

/** The words the routing page puts beside a tier, reused for the home rows. */
const TIER_HINT: Record<ModelTier, TKey> = {
  fast: "tier.hint.fast",
  standard: "tier.hint.standard",
  reasoning: "tier.hint.reasoning",
  frontier: "tier.hint.frontier",
};

/** Strategy names for the honest order note under the home pools. */
const STRATEGY_LABEL: Record<RoutingStrategy, TKey> = {
  priority: "strategy.priority",
  weighted_random: "strategy.weighted_random",
  round_robin: "strategy.round_robin",
  lowest_latency: "strategy.lowest_latency",
  balanced: "strategy.balanced",
};

export default function Overview({
  snapshot,
  onNavigate,
  save,
  busy,
  onModeChange,
}: {
  snapshot: Snapshot;
  onNavigate: (page: "models" | "providers" | "routing" | "activity" | "settings") => void;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  /** The queue's read half, part of the page contract like every content page; the overview's writes are saves. */
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
  /** The shell's mode switch, for the add-models hand-off (simple has no models page). */
  onModeChange: (mode: UiMode) => void;
}) {
  const { config, server, recent } = snapshot;
  const { t } = useI18n();
  const notify = useToast();
  const mode = useMode();
  const simple = mode === "simple";

  const rows = modelRows(snapshot);
  const providers = config.providers.filter((p) => p.enabled);
  const models = rows.filter(
    (r) => r.model.enabled && providers.some((p) => p.id === r.model.provider_id && p.enabled),
  );

  const tierRoutes = TIERS
    .map((tier) => ({ tier, members: tierMembers(rows, config.providers, tier) }))
    .filter((r) => r.members.length > 0);

  const reviewCandidates = config.classifier.enabled
    ? config.classifier.candidates
        .filter((c) => c.enabled)
        .map((c) => rows.find((r) => r.id === c.model || r.model.aliases.includes(c.model)))
        .filter((r): r is (typeof rows)[number] => Boolean(r))
    : [];

  /*
   * The pool's live furniture, the same joins the routing page makes: which
   * model is cooling, what a provider is called, whose wallet the router has
   * latched shut. Recomputed per render because the snapshot under them moves
   * every poll.
   */
  const cooling = new Set(
    snapshot.health.filter((h) => h.cooldown_remaining_secs > 0).map((h) => h.model_id),
  );
  const providerById = new Map(config.providers.map((p) => [p.id, p]));
  const walletHeld = new Set(snapshot.wallet_holds.map((h) => h.provider_id));

  /**
   * The routing page's own writer, verbatim: a drop hands over the full
   * desired order and the save renumbers that tier's priorities, so the home
   * page and the routing page can never disagree about what order means.
   */
  const reorderTier = (
    tier: ModelTier,
    order: { provider_id: string; upstream_model: string }[],
  ) => {
    void save((cfg) => {
      const tierModels = applyTierOrder(cfg.models, tier, order);
      if (!tierModels) return null;
      return { ...cfg, models: tierModels };
    });
  };

  /*
   * The provider row's pointer target. The name and the add-models action are
   * controls with clicks of their own; only the row's remainder navigates —
   * the same stand-down the activity ledger's rows observe.
   */
  const openProviders = (event: ReactMouseEvent<HTMLDivElement>) => {
    if ((event.target as HTMLElement).closest("button, a")) return;
    onNavigate("providers");
  };

  return (
    <>
      <div className="page-head">
        <p className="lede hang">
          <StatusDot tone={server.running ? "ok" : "danger"} />
          {server.running ? (
            <>
              {t("overview.gateway")}{" "}
              <span className="mono">{server.host}:{server.port}</span>
            </>
          ) : (
            t("overview.gateway_stopped")
          )}
          <span className="stat-inline">
            {t("overview.counts", { models: models.length, providers: providers.length })}
          </span>
        </p>
        <div className="page-head-actions text-actions">
          <button type="button"
            className="linky"
            onClick={() => api.copy(`http://${server.host}:${server.port}`).then(() => notify("ok", t("toast.copied")))}
          >
            {t("action.copy_base_url")}
          </button>
        </div>
      </div>

      <section className="section">
        <div className="section-head">
          <span className="section-title">{t("nav.routing")}</span>
          {/*
           * The route purposes' detail lives on the routing page, which simple
           * mode does not advertise — the pools below are the whole of what a
           * first user can act on, so the door that leaves the short nav is
           * not drawn either.
           */}
          {mode === "geek" && (
            <button type="button" className="linky" onClick={() => onNavigate("routing")}>
              {t("overview.configure")}
            </button>
          )}
        </div>

        {simple ? (
          <>
            <div className="flow">
              {tierRoutes.length === 0 ? (
                <span className="flow-empty">{t("route.no_tier")}</span>
              ) : (
                tierRoutes.map(({ tier, members }) => (
                  <div className="flow-row" key={tier}>
                    <div className="flow-kind">
                      <span className="flow-kind-name mono">{virtualId(tier, config.routing.naming_style)}</span>
                      <span className="flow-kind-hint">{t(TIER_HINT[tier])}</span>
                    </div>
                    <div className="flow-routes">
                      <RoutePool
                        tier={tier}
                        members={members}
                        firstLabel={t("routing.request")}
                        cooling={cooling}
                        providerName={(id: string) => providerById.get(id)?.name}
                        busy={busy}
                        onReorder={reorderTier}
                      />
                    </div>
                  </div>
                ))
              )}
            </div>
            {config.routing.strategy !== "priority" && (
              <p className="field-hint">
                {t("routing.strategy_order_note", {
                  strategy: t(STRATEGY_LABEL[config.routing.strategy]),
                })}
              </p>
            )}
          </>
        ) : (
          <div className="flow">
            <div className="flow-row">
              <div className="flow-kind">
                <span className="flow-kind-name">{t("route.default")}</span>
                <span className="flow-kind-hint">{t("route.default_hint")}</span>
              </div>
              <div className="flow-routes">
                {tierRoutes.length === 0 ? (
                  <span className="flow-empty">{t("route.no_tier")}</span>
                ) : (
                  tierRoutes.map(({ tier, members }) => (
                    <span className="flow-route" key={tier}>
                      <span className="flow-src mono">{virtualId(tier, config.routing.naming_style)}</span>
                      <span className="flow-arrow" aria-hidden>{"→"}</span>
                      <span className="flow-target">
                        {modelName(members[0].model)}
                        <span className="flow-provider">
                          {providers.find((p) => p.id === members[0].model.provider_id)?.name}
                        </span>
                        {members.length > 1 && (
                          <span className="flow-meta">+{members.length - 1}</span>
                        )}
                      </span>
                    </span>
                  ))
                )}
              </div>
            </div>

            <div className="flow-row">
              <div className="flow-kind">
                <span className="flow-kind-name">{t("route.auto_review")}</span>
                <span className="flow-kind-hint">
                  {config.classifier.enabled
                    ? t("route.review_hint_on")
                    : t("route.review_hint_off")}
                </span>
              </div>
              <div className="flow-routes">
                {config.classifier.enabled ? (
                  reviewCandidates.length > 0 ? (
                    <span className="flow-route">
                      <span className="flow-src">{t("route.check")}</span>
                      <span className="flow-arrow" aria-hidden>{"→"}</span>
                      <span className="flow-target">
                        {modelName(reviewCandidates[0].model)}
                        <span className="flow-provider">
                          {
                            providers.find((p) => p.id === reviewCandidates[0].model.provider_id)
                              ?.name
                          }
                        </span>
                        {reviewCandidates.length > 1 && (
                          <span className="flow-meta">+{reviewCandidates.length - 1}</span>
                        )}
                      </span>
                    </span>
                  ) : (
                    <span className="flow-empty">{t("route.no_candidate")}</span>
                  )
                ) : (
                  <span className="flow-empty">{t("route.off")}</span>
                )}
              </div>
            </div>
          </div>
        )}
      </section>

      {/*
       * The providers, home-sized: who is connected, what they have left, how
       * many models they hold. Read-only on purpose — enabling, editing and
       * balance administration stay on the providers page, and a row is a
       * door there, not a copy of it. A latched empty wallet rides inline as
       * the red word it is; the holds' release lever stays where it was.
       */}
      {simple && providers.length > 0 && (
        <section className="section">
          <div className="section-head">
            <span className="section-title">{t("task.providers")}</span>
          </div>
          <div className="list">
            {providers.map((provider) => {
              const modelCount = config.models.filter((m) => m.provider_id === provider.id).length;
              return (
                <div className="list-row" key={provider.id} onClick={openProviders}>
                  <StatusDot tone="ok" state={t("status.enabled")} />
                  <div className="row-main">
                    <span className="row-title">
                      <button
                        type="button"
                        className="ledger-name"
                        aria-label={t("providers.open_row", { name: provider.name })}
                        onClick={() => onNavigate("providers")}
                      >
                        {provider.name}
                      </button>
                      {walletHeld.has(provider.id) && (
                        <StatusDot tone="danger" label={t("providers.wallet_empty")} />
                      )}
                    </span>
                    <span className="row-sub">
                      <span className="mono">{provider.base_url}</span>
                    </span>
                  </div>
                  <BalanceCell status={snapshot.balances[provider.id]} />
                  <span className="col-num">{t("count.models", { n: modelCount })}</span>
                  {/*
                   * The honest gap. Simple mode has no models page, and a
                   * provider with no models is a dead end the home page must
                   * not paper over: the row hands the reader across the mode
                   * line explicitly — a switch they made, one click to undo.
                   */}
                  {modelCount === 0 && (
                    <button
                      type="button"
                      className="linky"
                      onClick={() => {
                        onModeChange("geek");
                        onNavigate("models");
                      }}
                    >
                      {t("overview.add_models")}
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      )}

      {providers.length === 0 ? (
        <div className="empty-state">
          <p>{t("overview.no_providers")}</p>
          <button type="button" className="linky" onClick={() => onNavigate("providers")}>
            {t("overview.add_provider")}
          </button>
        </div>
      ) : (
        <section className="section">
          <div className="section-head">
            <span className="section-title">{t("overview.recent")}</span>
            <button type="button" className="linky" onClick={() => onNavigate("activity")}>
              {t("overview.all_activity")}
            </button>
          </div>
          {recent.length === 0 ? (
            <p className="empty">{t("overview.no_requests")}</p>
          ) : (
            <div className="timeline">
              {recent.slice(0, 5).map((r) => (
                <span className="timeline-item" key={r.id} style={{ cursor: "default" }}>
                  <span className="timeline-time">
                    {new Date(r.at).toLocaleTimeString([], { hour12: false })}
                  </span>
                  <span className="timeline-kind">
                    {isSideKind(r.kind) ? t("kind.review") : t("kind.main")}
                  </span>
                  <span className="timeline-main-cell">
                    <StatusDot
                      tone={r.ok ? "ok" : "danger"}
                      state={r.ok ? t("status.succeeded") : t("status.failed")}
                    />
                    <span className="timeline-model">
                      {r.model_name ?? (rows.find((row) => row.id === r.resolved_model) ? modelName(rows.find((row) => row.id === r.resolved_model)!.model) : r.resolved_model ?? r.requested_model)}
                    </span>
                    {r.provider_name && (
                      <span className="timeline-provider">{r.provider_name}</span>
                    )}
                  </span>
                  <span className="timeline-meta">
                    <span>{ms(r.latency_ms)}</span>
                  </span>
                </span>
              ))}
            </div>
          )}
        </section>
      )}
    </>
  );
}

/**
 * One provider's last balance reading, home-sized: what is left, or that the
 * check failed. The full error rides on hover; the wording and the branches
 * are the providers page's BalanceChip's, so the two lists cannot report the
 * same check with different words.
 */
function BalanceCell({ status }: { status: BalanceStatus | undefined }) {
  const { t } = useI18n();
  if (!status) {
    return <span className="col-num">—</span>;
  }
  if (status.aggregate) {
    return (
      <span className="col-num">
        {Object.values(status.aggregate.by_currency)
          .map((balance) => money(balance.currency, balance.remaining ?? 0))
          .join(" · ") || t("common.unknown")}
      </span>
    );
  }
  if (status.error) {
    return (
      <span className="col-num" title={status.error}>
        {t("providers.balance_failed")}
      </span>
    );
  }
  if (status.balance) {
    const amount = status.balance.remaining ?? status.balance.total;
    return (
      <span className="col-num">{amount !== null ? money(status.balance.currency, amount) : "—"}</span>
    );
  }
  return <span className="col-num">—</span>;
}
