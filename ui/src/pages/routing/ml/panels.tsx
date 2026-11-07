/**
 * The learned-routing presentational panels: promotion rounds and their
 * verdicts, the gate-criteria table, the replay launcher, the trace log,
 * and the collection configuration form. Pure-ish: everything reaches the
 * backend through props the panel hands in.
 */

import {
  type MlPromotionRoundStatus,
  type PromotionCriterion,
  type PromotionDecision,
  type PromotionVerdict,
  type MlTraceInfo,
  type ShadowAnalysisStatus,
} from "../../../api";
import { useI18n, type TKey } from "../../../i18n";
import {
  Badge,
  Banner,
  Button,
  CommitInput,
  CommitNumber,
  Disclosure,
  Empty,
  KeyValue,
  Section,
  StatusDot,
  Toggle,
  num,
} from "../../../components";

export function Round({

  round,
  running,
  onJudge,
  onInstall,
}: {
  round: MlPromotionRoundStatus | null;
  running: boolean;
  onJudge: () => void;
  onInstall: () => void;
}) {
  const { t } = useI18n();
  const authorised = round?.decision?.verdict === "promoted";
  const choices = Object.entries(round?.policy_choices ?? {});

  return (
    <Section title={t("ml.round")} hint={t("ml.round_hint")}>
      <div className="row gap">
        <Button onClick={onJudge} disabled={running}>
          {running ? t("ml.round_judging") : t("ml.round_judge")}
        </Button>
        {authorised && (
          <Button kind="primary" onClick={onInstall} disabled={running} title={t("ml.round_install_hint")}>
            {running ? t("ml.round_installing") : t("ml.round_install")}
          </Button>
        )}
      </div>

      {round && <RoundResult round={round} />}

      {choices.length > 0 && (
        <>
          <div className="section-sub">{t("ml.round_choices")}</div>
          <KeyValue
            rows={choices.map(([name, times]) => [name, num(times)])}
          />
          {choices.length === 1 && <p className="ml-fault">{t("ml.round_single_choice")}</p>}
        </>
      )}
    </Section>
  );
}

/**
 * The verdict, the evidence behind it, and the routing state it left.
 *
 * A round that never got as far as a verdict reports the reason instead of an
 * empty table, because "there is nothing to judge" and "the gate refused" are
 * different answers and an operator who reads one as the other draws the wrong
 * conclusion from both.
 */
export function RoundResult({ round }: { round: MlPromotionRoundStatus }) {
  const { t } = useI18n();

  if (round.reason && !round.decision) {
    return <Empty>{`${t("ml.round_none")} ${round.reason}`}</Empty>;
  }

  return (
    <div className="ml-replay">
      <KeyValue
        rows={
          [
            [
              t("ml.verdict"),
              round.decision ? <VerdictBadge verdict={round.decision.verdict} /> : null,
            ],
            round.candidate_commit && [
              t("ml.round_candidate"),
              <code>{round.candidate_commit.slice(0, 8)}</code>,
            ],
            [t("ml.round_records"), num(round.traces_read)],
            [
              t("ml.round_serving_now"),
              round.installed ? (
                <>
                  <StatusDot tone="ok" /> <code>{round.installed.slice(0, 8)}</code>
                </>
              ) : (
                t("ml.round_deterministic")
              ),
            ],
          ].filter(Boolean) as [React.ReactNode, React.ReactNode][]
        }
      />
      {round.decision && <GateCriteria decision={round.decision} />}
      {round.error && (
        <p className="ml-fault" role="alert">
          {t("ml.round_error")}: {round.error}
        </p>
      )}
    </div>
  );
}

/**
 * Every criterion the gate decided on, with the number it decided with.
 *
 * Shared by the two places a decision is shown: what is serving now, and the
 * round that was just judged. They are the same document and the same question,
 * and a panel that answered it in one place and not the other sent an operator
 * asking "why was this refused?" to a screen that only said "blocked".
 *
 * The criteria that held are still listed rather than filtered out: a reader
 * checking a refusal against the whole gate needs to see that the gate asked
 * nine questions and not one.
 */
export function GateCriteria({ decision }: { decision: PromotionDecision }) {
  const { t } = useI18n();
  const held = decision.criteria.filter((criterion) => criterion.held).length;

  return (
    <Disclosure title={t("ml.criteria")} count={<>{held}/{decision.criteria.length}</>}>
      <ul className="ml-criteria">
        {decision.criteria.map((criterion) => (
          <li key={criterion.name}>
            <StatusDot tone={criterion.held ? "ok" : "warn"} />
            <span className="ml-criterion-name">{criterionName(criterion.name, t)}</span>
            {criterionValue(criterion, t) && (
              <span className="ml-criterion-value">{criterionValue(criterion, t)}</span>
            )}
            {/*
              The gate's own prose, in the gate's own language, and only where
              it failed. On a passing row it restates the two numbers already
              shown beside it, so it is dropped rather than translated — a
              criterion whose reason mattered would need the reason to come
              from the backend as structured fields, not as a format string.
            */}
            {!criterion.held && criterion.reason && (
              <span className="muted ml-criterion-reason">{criterion.reason}</span>
            )}
          </li>
        ))}
      </ul>
    </Disclosure>
  );
}

/**
 * The gate's verdict as one badge.
 *
 * `promoted` is the only tone that reads as settled; the two refusals are told
 * apart from each other because they mean different things — `rejected` is a
 * judgement about the model, `blocked` is a statement that there was not enough
 * evidence to judge one.
 */
export function VerdictBadge({ verdict }: { verdict: PromotionVerdict }) {
  const { t } = useI18n();
  const tone = verdict === "promoted" ? "ok" : verdict === "blocked" ? "neutral" : "warn";
  return (
    <Badge tone={tone}>
      {t(
        verdict === "promoted"
          ? "ml.verdict_promoted"
          : verdict === "blocked"
            ? "ml.verdict_blocked"
            : "ml.verdict_rejected",
      )}
    </Badge>
  );
}

/**
 * The durable evidence on disk, and the one destructive action on it.
 *
 * The size and the count are read through the backend rather than derived from
 * the status counters: `count` streams the file, and the promotion round trains
 * from all of it, so "how much history is there" is a real question rather than
 * a decoration.
 *
 * A count that could not be read renders as a fault, never as zero. Zero says
 * "nothing recorded", and reading a permission error as that is how an operator
 * concludes their history is empty when it is on disk and unreadable.
 */
export function Log({
  log,
  clearing,
  onClear,
}: {
  log: MlTraceInfo | null;
  clearing: boolean;
  onClear: () => void;
}) {
  const { t } = useI18n();

  return (
    <Section
      title={t("ml.log")}
      hint={t("ml.log_hint")}
      actions={
        <Button kind="ghost" onClick={onClear} disabled={clearing}>
          {clearing ? t("ml.log_clearing") : t("ml.log_clear")}
        </Button>
      }
    >
      {!log && <p className="muted">{t("common.loading")}</p>}

      {log?.error && (
        <p className="ml-fault" role="alert">
          {log.records === null ? t("ml.log_unreadable") : log.error}
        </p>
      )}

      {/* An uncounted log has no rows to show: the count is the fact, and there
          isn't one. Rendering zero here would say "nothing recorded". */}
      {log && log.records !== null && !log.error && (
        <>
          {log.records === 0 ? (
            <Empty>{t("ml.log_empty")}</Empty>
          ) : (
            <KeyValue
              rows={[
                [t("ml.log_records"), num(log.records)],
                [t("ml.log_size"), t("bytes.value", { n: num(log.bytes_on_disk) })],
                ...(log.path ? [[t("ml.log_path"), <code>{log.path}</code>]] : []),
                ...(log.counters
                  ? [
                      [t("ml.log_appended"), num(log.counters.appended)],
                      log.counters.refused > 0 && [
                        t("ml.log_refused"),
                        num(log.counters.refused),
                      ],
                      log.counters.io_errors > 0 && [
                        t("ml.log_io_errors"),
                        num(log.counters.io_errors),
                      ],
                    ].filter(Boolean)
                  : []),
              ] as [React.ReactNode, React.ReactNode][]}
            />
          )}
        </>
      )}
    </Section>
  );
}

/** A byte count, grouped so a large log stays readable. */
export function bytesText(value: number): string {
  return num(value);
}

/**
 * The replay's numbers, or why there are none.
 *
 * The unmeasured case is shown rather than hidden. A body where the alternative
 * was never tried has no answer about the alternative, and a panel that rendered
 * a zero there would be claiming a measurement nobody made.
 */
export function Replay({ verdict }: { verdict: ShadowAnalysisStatus }) {
  const { t } = useI18n();
  const analysis = verdict.analysis;

  /*
   * A live region, because the replay arrives on its own: the operator asks
   * for it, it reads the recorded traffic, and the answer appears. Without a
   * live region the verdict — which is the whole point of asking — is a silent
   * change further down the page.
   */
  if (!analysis) {
    return (
      <div className="ml-replay" role="status">
        <p className="muted">
          {verdict.reason ?? t("ml.replay_none")}
          {verdict.traces_read > 0 && ` (${verdict.traces_read} ${t("ml.traces")})`}
        </p>
      </div>
    );
  }

  const gaps = Object.entries(analysis.gaps).filter(([, count]) => count > 0);

  return (
    <div className="ml-replay" role="status">
      <KeyValue
        rows={[
          [t("ml.replay_records"), analysis.records],
          [t("ml.agreement"), pct(analysis.agreement_rate)],
          [
            t("ml.disagreements"),
            `${analysis.disagreements} / ${analysis.disagreements_measured} ${t("ml.measured")}`,
          ],
          [t("ml.regret"), number(analysis.mean_regret)],
          [t("ml.utility_delta"), number(analysis.mean_observed_utility_delta)],
          [
            t("ml.alternatives"),
            `${analysis.harmful_alternatives} ${t("ml.harmful")} · ${
              analysis.helpful_alternatives
            } ${t("ml.helpful")}`,
          ],
        ]}
      />
      {gaps.length > 0 && (
        <p className="muted" style={{ marginTop: 8 }}>
          {t("ml.gaps")} {gaps.map(([name, count]) => `${name} ${count}`).join(" · ")}
        </p>
      )}
      <p className="muted" style={{ marginTop: 8 }}>
        {t("ml.replay_bound", { n: verdict.traces_read })}
      </p>
    </div>
  );
}

/** A rate as a whole percentage. */
function pct(value: number): string {
  return `${Math.round(value * 1000) / 10}%`;
}

/** A count, grouped so a six-figure sample total stays readable. */
export function count(value: number): string {
  return value.toLocaleString();
}

/**
 * A signed mean, or "—".
 *
 * "—" is not zero. A body with nothing measured reports no number, and showing a
 * zero would say the model changed nothing rather than that nobody has found out.
 */
function number(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  const rounded = Math.round(value * 10_000) / 10_000;
  return rounded > 0 ? `+${rounded}` : String(rounded);
}

/** A unix timestamp as a short local date, or "—" for zero. */
export function when(seconds: number): string {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString();
}

/** The nine checks `PromotionGate::evaluate` can report, in evaluation order. */
const CRITERION_NAMES = new Set([
  "baseline_present",
  "paired_evidence",
  "holdout_quality",
  "routing_utility",
  "selection_safety",
  "success_regression",
  "cost_regression",
  "latency_regression",
  "dataset_identity",
]);

/**
 * A criterion's name, translated.
 *
 * `PromotionCriterion::name` is documented as a stable identifier so a decision
 * can be reasoned about by name, which makes it a translation key rather than
 * prose the backend happens to have written in English. The fallback is the
 * identifier itself: a criterion a newer backend adds has to stay visible, and
 * an untranslated `dataset_freshness` is a smaller problem than a blank row in
 * a list where every other row is named.
 */
function criterionName(
  name: string,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): string {
  return CRITERION_NAMES.has(name) ? t(`ml.criterion.${name}` as TKey) : name;
}

/**
 * The number the criterion was decided on, per ADR-0007's "every criterion with
 * the number it was decided on".
 *
 * Null for the two checks that are facts rather than measurements —
 * `baseline_present` and `dataset_identity` both pass `None` for both fields —
 * so the row simply carries no figure instead of showing a placeholder that
 * reads like a measurement of zero.
 */
function criterionValue(
  criterion: PromotionCriterion,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): string | null {
  if (criterion.measured === null || criterion.threshold === null) return null;
  return t("ml.criterion_value", {
    measured: formatMeasure(criterion.measured, criterion.name),
    threshold: formatMeasure(criterion.threshold, criterion.name),
  });
}

/**
 * The four-decimal default is wrong for two of the nine: an evidence count of
 * 1842 and a latency budget in milliseconds both read as noise with four
 * decimals on them. Grouped by criterion rather than by magnitude, because
 * whether a figure is a count or a measurement is a property of the check and
 * not something the number's own size can tell you.
 */
function formatMeasure(value: number, criterion: string): string {
  switch (criterion) {
    case "paired_evidence":
    case "success_regression":
    case "selection_safety":
      return String(Math.round(value));
    case "latency_regression":
      return `${value.toFixed(1)} ms`;
    case "holdout_quality":
      return value.toFixed(4);
    default:
      return value.toFixed(4);
  }
}

export function CollectionConfig({
  snapshot,
  save,
  status,
}: {
  snapshot: import("../../../api").Snapshot;
  save: (mutate: (config: import("../../../api").AppConfig) => import("../../../api").AppConfig | null) => Promise<boolean>;
  status: import("../../../api").MlStatus;
}) {
  const { t } = useI18n();
  const cfg = snapshot.config;
  const shadow = cfg.shadow;
  const ml = cfg.ml_routing;
  // Builds without `ml` feature have neither key — show explanation instead of controls
  if (!shadow && !ml) {
    return (
      <Section title={t("ml.config_title")} hint={t("ml.config_hint")}>
        <p className="muted">{t("ml.config_unavailable")}</p>
      </Section>
    );
  }
  const shadowOn = shadow?.enabled ?? false;
  const mlOn = ml?.enabled ?? false;
  const canCollect = shadowOn;
  const canServe = mlOn && !!status.durable_state;

  return (
    <Section title={t("ml.config_title")} hint={t("ml.config_hint")}>
      {!canCollect && (
        <Banner tone="warn">{t("ml.config_shadow_off_hint")}</Banner>
      )}
      {shadow && (
        <>
          <Toggle
            label={t("ml.shadow_enable")}
            hint={t("ml.shadow_enable_hint")}
            checked={shadow.enabled}
            onChange={(enabled) =>
              void save((c) => {
                const n = structuredClone(c);
                if (!n.shadow) n.shadow = { enabled: false, max_decisions: 10000, max_age_secs: 604800 };
                n.shadow.enabled = enabled;
                return n;
              })
            }
          />
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("ml.shadow_max_decisions")}</span>
              <span className="setting-desc">{t("ml.shadow_max_decisions_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={100}
                max={100000}
                integer
                value={shadow.max_decisions}
                ariaLabel={t("ml.shadow_max_decisions")}
                onCommit={(v) =>
                  void save((c) => {
                    const n = structuredClone(c);
                    if (!n.shadow) return null;
                    n.shadow.max_decisions = v ?? 10000;
                    return n;
                  })
                }
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("ml.shadow_max_age")}</span>
              <span className="setting-desc">{t("ml.shadow_max_age_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={3600}
                max={2592000}
                integer
                value={shadow.max_age_secs}
                ariaLabel={t("ml.shadow_max_age")}
                onCommit={(v) =>
                  void save((c) => {
                    const n = structuredClone(c);
                    if (!n.shadow) return null;
                    n.shadow.max_age_secs = v ?? 604800;
                    return n;
                  })
                }
              />
            </div>
          </div>
        </>
      )}
      {ml && (
        <>
          <Toggle
            label={t("ml.routing_enable")}
            hint={t("ml.routing_enable_hint")}
            checked={ml.enabled}
            onChange={(enabled) =>
              void save((c) => {
                const n = structuredClone(c);
                if (!n.ml_routing) n.ml_routing = { enabled: false, state_dir: "", exploration_probability: 0, exploration_seed: 42 };
                n.ml_routing.enabled = enabled;
                return n;
              })
            }
          />
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("ml.state_dir")}</span>
              <span className="setting-desc">{t("ml.state_dir_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitInput
                value={ml.state_dir}
                ariaLabel={t("ml.state_dir")}
                placeholder="ml"
                onCommit={(state_dir) =>
                  void save((c) => {
                    const n = structuredClone(c);
                    if (!n.ml_routing) return null;
                    n.ml_routing.state_dir = state_dir.trim();
                    return n;
                  })
                }
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("ml.exploration")}</span>
              <span className="setting-desc">{t("ml.exploration_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={0}
                max={1}
                value={ml.exploration_probability}
                ariaLabel={t("ml.exploration")}
                onCommit={(v) =>
                  void save((c) => {
                    const n = structuredClone(c);
                    if (!n.ml_routing) return null;
                    n.ml_routing.exploration_probability = Math.max(0, Math.min(1, v ?? 0));
                    return n;
                  })
                }
              />
            </div>
          </div>
          {!canServe && mlOn && (
            <p className="field-hint">{t("ml.serve_blocked_hint")}</p>
          )}
        </>
      )}
      <KeyValue
        rows={[
          [t("ml.config_effect"), canCollect ? t("ml.effect_collecting") : t("ml.effect_idle")],
          [t("ml.config_serve"), canServe ? t("ml.serve_active") : t("ml.serve_deterministic")],
        ]}
      />
    </Section>
  );
}
