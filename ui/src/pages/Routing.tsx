import { useState } from "react";
import {
  TIERS,
  tierMembers,
  modelRows,
  virtualId,
  type AppConfig,
  type ClassifierCandidate,
  type Election,
  type ModelTier,
  type ModelRow,
  type RoutingStrategy,
  type Snapshot,
} from "../api";
import { api } from "../api";
import {
  Badge,
  Button,
  CompactNumber,
  Drawer,
  Field,
  KeyValue,
  NumberField,
  PageHead,
  Section,
  Select,
  StatusDot,
  Toggle,
  ms,
} from "../components";
import { useI18n } from "../i18n";

/**
 * Routing shows relationships first and configuration second. The page is
 * the flow — what a request of each purpose reaches — and every knob lives
 * behind Edit, because "how it is configured" is only interesting once
 * "where it goes" is already visible.
 *
 * Two purposes exist today: Default (every ordinary request) and Auto Mode
 * (permission side queries). Applications are a future dimension and are
 * deliberately absent until the backend can mean them.
 */
export default function Routing({
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
  const { config, health } = snapshot;
  const { t } = useI18n();
  const rows = modelRows(snapshot);
  const [edit, setEdit] = useState<"default" | "auto" | null>(null);

  const cooling = new Set(
    health.filter((h) => h.cooldown_remaining_secs > 0).map((h) => h.model_id),
  );

  const tierRoutes = TIERS
    .map((tier) => ({ tier, members: tierMembers(rows, config.providers, tier) }));

  const classifierCandidates = config.classifier.enabled
    ? config.classifier.candidates
        .filter((c) => c.enabled)
        .map((c) => rows.find((r) => r.id === c.model || r.model.aliases.includes(c.model)))
        .filter((r): r is ModelRow => Boolean(r))
    : [];

  return (
    <>
      <PageHead lede={t("routing.lede")} />

      <Section
        title={t("route.default")}
        hint={t("routing.default_hint")}
        actions={
          <button className="linky" onClick={() => setEdit("default")}>
            {t("routing.edit")}
          </button>
        }
      >
        <div className="list">
          <div className="flow">
            {tierRoutes.map(({ tier, members }) => (
              <div className="flow-row" key={tier}>
                <div className="flow-kind">
                  <span className="flow-kind-name mono">{virtualId(tier)}</span>
                  <span className="flow-kind-hint">{t(TIER_HINT_KEY[tier])}</span>
                </div>
                <div className="flow-routes">
                  {members.length === 0 ? (
                    <span className="flow-empty">
                      {t("routing.tier_empty", { id: virtualId(tier) })}
                    </span>
                  ) : (
                    members.map((r, i) => (
                      <span className="flow-route" key={r.id}>
                        {i === 0 ? (
                          <span className="flow-src">{i === 0 ? t("routing.request") : ""}</span>
                        ) : (
                          <span className="flow-src muted">{t("routing.fallback_n", { n: i })}</span>
                        )}
                        <span className="flow-arrow" aria-hidden>{"→"}</span>
                        <span className="flow-target">
                          <StatusDot tone={cooling.has(r.id) ? "warn" : "ok"} />
                          {r.id}
                          <span className="flow-provider">
                            {config.providers.find((p) => p.id === r.model.provider_id)?.name}
                          </span>
                        </span>
                      </span>
                    ))
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      </Section>

      <Section
        title={t("route.auto_review")}
        hint={config.classifier.enabled
          ? t("routing.review_hint_on")
          : t("routing.review_hint_off")}
        actions={
          <button className="linky" onClick={() => setEdit("auto")}>
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
                    classifierCandidates.map((r, i) => (
                      <span className="flow-route" key={r.id}>
                        {i === 0 ? (
                          <span className="flow-src">{t("routing.verdict")}</span>
                        ) : (
                          <span className="flow-src muted">{t("routing.fallback_n", { n: i })}</span>
                        )}
                        <span className="flow-arrow" aria-hidden>{"→"}</span>
                        <span className="flow-target">
                          <StatusDot tone={cooling.has(r.id) ? "warn" : "ok"} />
                          {r.id}
                          <span className="flow-provider">
                            {config.providers.find((p) => p.id === r.model.provider_id)?.name}
                          </span>
                        </span>
                      </span>
                    ))
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
          onElection={() => run(api.runElection)}
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

const TIER_HINT_KEY: Record<ModelTier, "tier.hint.fast" | "tier.hint.standard" | "tier.hint.reasoning" | "tier.hint.frontier"> = {
  fast: "tier.hint.fast",
  standard: "tier.hint.standard",
  reasoning: "tier.hint.reasoning",
  frontier: "tier.hint.frontier",
};

/**
 * How Default routing is configured. Strategy, failover, and what happens to
 * a model id nobody recognises.
 */
function DefaultDrawer({
  snapshot,
  busy,
  onClose,
  onSave,
  onElection,
}: {
  snapshot: Snapshot;
  busy: boolean;
  onClose: () => void;
  onSave: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  onElection: () => Promise<boolean>;
}) {
  const routing = snapshot.config.routing;
  const { t } = useI18n();

  const patch = (mutate: Partial<AppConfig["routing"]>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.routing, mutate);
      return next;
    });
  };

  return (
    <Drawer title={t("routing.default_drawer")} onClose={onClose}>
      <div className="controls">
        <Field label={t("field.strategy")}>
          <Select<RoutingStrategy>
            ariaLabel={t("field.strategy")}
            value={routing.strategy}
            disabled={busy}
            onChange={(strategy) => patch({ strategy })}
            options={[
              { value: "priority", label: t("strategy.priority") },
              { value: "weighted_random", label: t("strategy.weighted_random") },
              { value: "round_robin", label: t("strategy.round_robin") },
              { value: "lowest_latency", label: t("strategy.lowest_latency") },
              { value: "balanced", label: t("strategy.balanced") },
            ]}
          />
        </Field>
        <NumberField
          label={t("field.max_attempts")}
          hint={t("field.max_attempts_hint")}
          min={1}
          max={10}
          integer
          value={routing.max_attempts}
          onCommit={(max_attempts) => patch({ max_attempts: max_attempts ?? 3 })}
        />
        <Field label={t("field.unknown_ids")}>
          <Select<ModelTier | "unset">
            ariaLabel={t("field.unknown_ids")}
            value={routing.unknown_model_fallback ?? "unset"}
            onChange={(next) =>
              patch({ unknown_model_fallback: next === "unset" ? null : next })
            }
            options={[
              { value: "unset", label: t("routing.unknown_404") },
              ...TIERS.map((c) => ({
                value: c as ModelTier,
                label: t("routing.unknown_serve", { id: virtualId(c) }),
              })),
            ]}
          />
        </Field>
      </div>
      <div className="grid-two">
        <Toggle
          label={t("routing.failover")}
          hint={t("routing.failover_hint")}
          checked={routing.failover}
          onChange={(failover) => patch({ failover })}
        />
        <Toggle
          label={t("routing.claude_names")}
          hint={t("routing.claude_names_hint")}
          checked={routing.match_claude_names}
          onChange={(match_claude_names) => patch({ match_claude_names })}
        />
      </div>
      <p className="field-hint">{t("routing.aliases_note")}</p>

      {routing.strategy === "balanced" && (
        <Section
          title={t("routing.election")}
          hint={t("routing.election_hint")}
          actions={
            <Button kind="ghost" onClick={onElection} disabled={busy}>
              {t("routing.rerun")}
            </Button>
          }
        >
          <Toggle
            label={t("routing.elect_on_start")}
            checked={routing.elect_on_start}
            onChange={(elect_on_start) => patch({ elect_on_start })}
          />
          <ElectionResult election={snapshot.election} />
        </Section>
      )}
    </Drawer>
  );
}

/** What the last election decided, per tier, with the numbers behind it. */
function ElectionResult({ election }: { election: Election | null }) {
  const { t } = useI18n();
  if (!election) {
    return <p className="empty">{t("routing.no_election")}</p>;
  }
  const tierOutcomes = TIERS.map((tier) => election.tiers[tier]).filter(
    (c): c is NonNullable<typeof c> => c !== undefined,
  );
  if (tierOutcomes.length === 0) return <p className="empty">{t("routing.no_election_tiers")}</p>;
  return (
    <>
      {tierOutcomes.map((outcome) => (
        <div key={outcome.tier}>
          <div className="row gap" style={{ marginBottom: 4 }}>
            <span className="mono">{virtualId(outcome.tier)}</span>
            {outcome.note && <span className="muted">{outcome.note}</span>}
          </div>
          <table className="table">
            <tbody>
              {outcome.ranked.map((r, place) => (
                <tr key={r.model_id} className={r.score === null ? "row-warn" : ""}>
                  <td>
                    {place === 0 && r.score !== null ? <Badge tone="ok">{t("routing.primary")}</Badge> : ""}{" "}
                    <span className="mono">{r.model_id}</span>
                  </td>
                  <td className="muted mono">{ms(r.latency_ms)}</td>
                  <td className="muted">{r.note ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ))}
    </>
  );
}

/**
 * Auto Mode as a product surface: no "classifier" jargon, no candidate
 * plumbing visible — a switch, a pool, and where it fails over.
 */
function AutoModeDrawer({
  snapshot,
  busy,
  onClose,
  onSave,
}: {
  snapshot: Snapshot;
  busy: boolean;
  onClose: () => void;
  onSave: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
}) {
  const classifier = snapshot.config.classifier;
  const { t } = useI18n();
  const rows = modelRows(snapshot);

  const patch = (mutate: Partial<AppConfig["classifier"]>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.classifier, mutate);
      return next;
    });
  };

  const patchCandidate = (model: string, mutate: Partial<ClassifierCandidate>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      const candidate = next.classifier.candidates.find((c) => c.model === model);
      if (!candidate) return null;
      Object.assign(candidate, mutate);
      return next;
    });
  };

  const removeCandidate = (model: string) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      const index = next.classifier.candidates.findIndex((c) => c.model === model);
      if (index < 0) return null;
      next.classifier.candidates.splice(index, 1);
      return next;
    });
  };

  const moveCandidate = (model: string, delta: -1 | 1) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      const candidates = next.classifier.candidates;
      const index = candidates.findIndex((c) => c.model === model);
      const swap = index + delta;
      if (index < 0 || swap < 0 || swap >= candidates.length) return null;
      [candidates[index], candidates[swap]] = [candidates[swap], candidates[index]];
      return next;
    });
  };

  const addCandidate = (model: string) => {
    if (!model) return;
    void onSave((cfg) => {
      if (cfg.classifier.candidates.some((c) => c.model === model)) return null;
      const next = structuredClone(cfg);
      next.classifier.candidates.push({
        model,
        priority: next.classifier.candidates.reduce((max, c) => Math.max(max, c.priority), 0) + 10,
        enabled: true,
      });
      return next;
    });
  };

  const available = rows.filter((r) => !classifier.candidates.some((c) => c.model === r.id));

  return (
    <Drawer title={t("route.auto_review")} onClose={onClose}>
      <Toggle
        label={t("routing.route_review")}
        hint={t("routing.route_review_hint")}
        checked={classifier.enabled}
        onChange={(enabled) => patch({ enabled })}
      />

      <div className="controls">
        <Field label={t("field.strategy")}>
          <Select<RoutingStrategy>
            ariaLabel={t("field.strategy")}
            value={classifier.strategy}
            disabled={busy}
            onChange={(strategy) => patch({ strategy })}
            options={[
              { value: "priority", label: t("strategy.priority") },
              { value: "weighted_random", label: t("strategy.weighted_random") },
              { value: "round_robin", label: t("strategy.round_robin") },
              { value: "lowest_latency", label: t("strategy.lowest_latency") },
            ]}
          />
        </Field>
        <NumberField
          label={t("field.max_attempts")}
          min={1}
          max={10}
          integer
          value={classifier.max_attempts}
          onCommit={(max_attempts) => patch({ max_attempts: max_attempts ?? 2 })}
        />
      </div>
      <Toggle
        label={t("routing.review_failover")}
        hint={t("routing.review_failover_hint")}
        checked={classifier.failover}
        onChange={(failover) => patch({ failover })}
      />

      <Section title={t("routing.pool")} hint={t("routing.pool_hint")}>
        {classifier.candidates.length === 0 ? (
          <p className="empty">{t("routing.no_candidates")}</p>
        ) : (
          <table className="table">
            <tbody>
              {classifier.candidates.map((c, i) => {
                const known = rows.some((r) => r.id === c.model || r.model.aliases.includes(c.model));
                return (
                  <tr key={c.model} className={known ? "" : "row-warn"}>
                    <td className="mono">
                      {c.model}
                      {!known && (
                        <>
                          {" "}
                          <Badge tone="warn">{t("routing.not_configured")}</Badge>
                        </>
                      )}
                    </td>
                    <td>
                      <CompactNumber
                        ariaLabel={`Priority for ${c.model}`}
                        value={c.priority}
                        min={0}
                        integer
                        onCommit={(v) =>
                          patchCandidate(c.model, { priority: v ?? 0 })
                        }
                      />
                    </td>
                    <td>
                      <input
                        type="checkbox"
                        aria-label={`Enable ${c.model}`}
                        checked={c.enabled}
                        onChange={(e) => patchCandidate(c.model, { enabled: e.currentTarget.checked })}
                      />
                    </td>
                    <td>
                      <div className="row gap">
                        <Button kind="ghost" disabled={i === 0} onClick={() => moveCandidate(c.model, -1)}>
                          ↑
                        </Button>
                        <Button
                          kind="ghost"
                          disabled={i === classifier.candidates.length - 1}
                          onClick={() => moveCandidate(c.model, 1)}
                        >
                          ↓
                        </Button>
                        <Button kind="ghost" onClick={() => removeCandidate(c.model)}>
                          {t("common.remove")}
                        </Button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {available.length > 0 && (
          <div className="controls">
            <Select
              ariaLabel={t("routing.add_model")}
              value={null}
              placeholder={t("routing.add_model")}
              onChange={(model) => addCandidate(model)}
              options={available.map((r) => ({ value: r.id, label: r.id }))}
            />
          </div>
        )}
      </Section>

      <Section title={t("routing.behaviour")}>
        <KeyValue
          rows={[
            [t("routing.detection"), t("routing.automatic")],
            [
              t("routing.verdict_parsing"),
              <span>
                <code>{t("routing.verdict_only")}</code>
              </span>,
            ],
            [t("routing.no_verdict"), t("routing.fails_closed")],
          ]}
        />
        <p className="field-hint">{t("routing.fail_closed_note")}</p>
      </Section>
    </Drawer>
  );
}
