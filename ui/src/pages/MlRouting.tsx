import { useCallback, useEffect, useState } from "react";
import {
  api,
  errorText,
  type MlPromotionRound,
  type MlPromotionRoundStatus,
  type MlRollback,
  type MlStatus,
  type MlTraceClear,
  type MlTraceInfo,
  type PromotionVerdict,
  type ShadowAnalysisStatus,
} from "../api";
import {
  Badge,
  Banner,
  Button,
  ConfirmDialog,
  Empty,
  KeyValue,
  Section,
  StatusDot,
  useToast,
  num,
  type ConfirmRequest,
} from "../components";
import { useI18n } from "../i18n";

/** How many records an on-demand replay reads. */
const REPLAY_LIMIT = 5_000;

/** How often the counters refresh while the panel is on screen. */
const POLL_MS = 2_000;

/**
 * What the learned model is doing right now, and what an operator can do about
 * it.
 *
 * The panel is shaped around the questions an operator actually asks, in the
 * order they ask them:
 *
 *  1. Is anything learned in the routing path at all?
 *  2. If so, which model, and who authorised it?
 *  3. On what evidence, and did every criterion hold?
 *  4. What has it done since — ranked, fell back, explored?
 *  5. What would it have done differently?
 *  6. If it is wrong, how do I take it out?
 *
 * Question 4 is counters, not quality. A count of collected samples says
 * collection is happening; it says nothing about whether the model is any good,
 * and the panel does not let the two be read as one. The quality claims on
 * screen are the ones the gate actually made, with the criterion that decided
 * each, and the replay's measured numbers with the count they were measured over.
 *
 * The rollback is here because it is the reason the gate is trustworthy. A model
 * that turns out to be wrong has to be removable by the person who can see that
 * it is wrong, without stopping the proxy or editing a file.
 */
export default function MlRouting({
  available,
  onStatus,
}: {
  /** Whether this build has a learning stack, from the snapshot. */
  available: boolean;
  /** Hands a freshly read status up, so the page can stop showing stale facts. */
  onStatus?: (status: MlStatus) => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const [status, setStatus] = useState<MlStatus | null>(null);
  const [replay, setReplay] = useState<ShadowAnalysisStatus | null>(null);
  const [replaying, setReplaying] = useState(false);
  const [rollingBack, setRollingBack] = useState(false);
  const [fault, setFault] = useState<string | null>(null);
  const [round, setRound] = useState<MlPromotionRoundStatus | null>(null);
  const [roundRunning, setRoundRunning] = useState(false);
  const [log, setLog] = useState<MlTraceInfo | null>(null);
  const [clearing, setClearing] = useState(false);
  const [confirmClear, setConfirmClear] = useState<ConfirmRequest | null>(null);

  const read = useCallback(async () => {
    if (!available) return;
    try {
      const next = await api.mlStatus();
      setStatus(next);
      onStatus?.(next);
      setFault(null);
    } catch (e) {
      // A failure here is a failure, not "no ML stack": the absence was already
      // declared in the snapshot, so there is nothing left to explain away.
      setFault(errorText(e));
    }
  }, [available, onStatus]);

  /**
   * How much history is on disk.
   *
   * Read on demand rather than on the status poll: this streams a count off a
   * file that grows with traffic, and the counters next to it are already on the
   * status document, so polling it every two seconds would buy nothing.
   */
  const readLog = useCallback(async () => {
    if (!available) return;
    try {
      setLog(await api.mlTraces());
    } catch (e) {
      setFault(errorText(e));
    }
  }, [available]);

  useEffect(() => {
    void read();
    if (!available) return;
    const timer = setInterval(() => void read(), POLL_MS);
    return () => clearInterval(timer);
  }, [read, available]);

  useEffect(() => {
    if (!available) return;
    void readLog();
  }, [available, readLog]);

  const runReplay = useCallback(async () => {
    setReplaying(true);
    setFault(null);
    try {
      setReplay(await api.mlShadow(REPLAY_LIMIT));
    } catch (e) {
      setFault(errorText(e));
    } finally {
      setReplaying(false);
    }
  }, []);

  const rollBack = useCallback(async () => {
    setRollingBack(true);
    setFault(null);
    try {
      const outcome: MlRollback = await api.rollbackMlModel();
      // The status that came back with the outcome is the one to believe: it was
      // read after the pointer moved, so it describes the process as it is now.
      setStatus(outcome.status);
      onStatus?.(outcome.status);
      // A replay of a model that is no longer serving would answer a question
      // about traffic the current model did not rank, so the stale answer goes.
      setReplay(null);
    } catch (e) {
      setFault(errorText(e));
    } finally {
      setRollingBack(false);
    }
  }, [onStatus]);

  /**
   * Judge a candidate, or install one the gate already authorised.
   *
   * The two are one function with a flag rather than two buttons on purpose:
   * the endpoint's own contract is that judging installs nothing and only an
   * explicit install changes what serves traffic, so the panel asks for exactly
   * one of the two each time and never infers the second.
   */
  const runRound = useCallback(async (install: boolean) => {
    setRoundRunning(true);
    setFault(null);
    try {
      const outcome: MlPromotionRound = await api.mlPromote(install);
      setRound(outcome.round);
      // The status that came back with the round was read after the pointer
      // moved, so it describes the process as it is now.
      setStatus(outcome.status);
      onStatus?.(outcome.status);
      // A replay of a withdrawn model answers a question about traffic the
      // current model did not rank.
      setReplay(null);
      void readLog();
    } catch (e) {
      setFault(errorText(e));
    } finally {
      setRoundRunning(false);
    }
  }, [onStatus, readLog]);

  /**
   * Discard the operator's own history, and say what that costs.
   *
   * The consequence is not a footnote: the next round trains only on records
   * written from here on, so this changes what the next model learns. It is said
   * before the click, in the dialog, and again after it — because a panel that
   * only said it once would be relying on the operator having read it.
   */
  const clearLog = useCallback(async () => {
    setClearing(true);
    setFault(null);
    try {
      const outcome: MlTraceClear = await api.clearMlTraces();
      notify(
        outcome.error ? "error" : "ok",
        outcome.error
          ? outcome.error
          : outcome.cleared
            ? t("ml.log_cleared", { bytes: bytesText(outcome.removed_bytes) })
            : t("ml.log_clear_nothing"),
      );
      await readLog();
    } catch (e) {
      setFault(errorText(e));
    } finally {
      setClearing(false);
    }
  }, [notify, readLog, t]);

  if (!available) {
    return (
      <Section title={t("ml.title")} hint={t("ml.hint")}>
        <div className="row" style={{ gap: 8, alignItems: "center" }}>
          <StatusDot tone="off" />
          <span className="muted">{t("ml.unavailable")}</span>
        </div>
      </Section>
    );
  }

  if (!status) {
    return (
      <Section title={t("ml.title")} hint={t("ml.hint")}>
        <p className="muted">{fault ?? t("common.loading")}</p>
      </Section>
    );
  }

  const active = status.active;
  const serving = status.routing_enabled && active !== null;
  const decision = status.active_decision;
  const tone = serving ? "ok" : active ? "warn" : "off";
  // A rollback needs somewhere to go. One promotion means there is no earlier
  // model, and offering a button that can only ever be refused is how an operator
  // learns to distrust the panel.
  const canRollBack = status.history.some((entry) => entry.action === "promote") && active !== null;
  // Configured candidates the router has never served. With exploration off
  // these can never be served at all, which is a defect in the configuration
  // rather than a cold start — so the distinction is made here and drives
  // whether this is a quiet row or a warning. The backend's own verdict is not
  // reimplemented: `exploration_probability <= 0` is the same condition
  // `MlStatus::blind_spots_are_permanent` uses, and
  // `blind_spot_warning_matches_what_exploration_actually_does` keeps the two
  // honest against `explore`'s real behaviour.
  const blind = status.blind_candidates ?? [];
  const blindIsPermanent = blind.length > 0 && status.exploration_probability <= 0;

  // An ML stack that is switched on and learning nothing.
  //
  // The condition is the one §E13 of the closed-loop report measured, read
  // live: the stack is on, shadow evaluation is off, so `evaluate` returns
  // `None`, no request retains the decision-time input a sample is built from,
  // and every request is discarded at the ingestion boundary. Nothing in the
  // configuration, the status document or the promotion endpoint declares the
  // prerequisite, and every endpoint stays healthy throughout — so the counters
  // in `KeyValue` are the only place this state is visible, and a panel that
  // shows "Collected: 0 samples" without saying why is exactly the confusion the
  // report says to refuse.
  //
  // `no_decision_time_input > 0` rather than `samples === 0`: a fresh install
  // has collected nothing and has discarded nothing, which is not this fault.
  const learningBlocked =
    status.routing_enabled &&
    !status.shadow.enabled &&
    status.dataset.no_decision_time_input > 0;

  return (
    <>
      <Section title={t("ml.title")} hint={t("ml.hint")}>
      <div className="row" style={{ gap: 8, alignItems: "center", marginBottom: 12 }}>
        <StatusDot tone={tone} />
        <span>{t(serving ? "ml.serving" : active ? "ml.inert" : "ml.off")}</span>
        {status.exploration_probability > 0 && (
          <Badge tone="warn">
            {t("ml.exploring", { pct: Math.round(status.exploration_probability * 100) })}
          </Badge>
        )}
      </div>

      {fault && (
        <p className="ml-fault" role="alert">
          {fault}
        </p>
      )}

      {blindIsPermanent && (
        <p className="ml-fault" role="alert">
          {t("ml.blind_spot_warning", {
            names: blind.map((b) => `${b.provider_id}/${b.model_id}`).join(", "),
          })}
        </p>
      )}

      {/* -- The silent failure: on, collecting nothing, everything healthy. -- */}
      {learningBlocked && (
        <Banner tone="warn" actions={<Badge tone="warn">{t("ml.learning_blocked_short")}</Badge>}>
          {t("ml.learning_blocked", { n: num(status.dataset.no_decision_time_input) })}
        </Banner>
      )}

      {!status.durable_state && <p className="muted">{t("ml.no_state_dir")}</p>}

      <KeyValue
        rows={
          [
            [
              t("ml.installed"),
              active ? (
                <>
                  {active.model_id} <code>{active.commit_id.slice(0, 8)}</code>
                </>
              ) : (
                t("ml.none")
              ),
            ],
            active && [
              t("ml.gate"),
              <>
                {t("ml.baseline", { name: active.required_baseline })} ·{" "}
                <code>{active.dataset_fingerprint.slice(0, 8)}</code> ·{" "}
                {t("ml.paired", { n: active.paired_requests })}
              </>,
            ],
            active && [t("ml.holdout_loss"), active.holdout_loss.toFixed(4)],
            learningBlocked && [
              t("ml.ingested"),
              <>
                {num(status.dataset.ingested)}{" "}
                <span className="muted">
                  · {num(status.dataset.no_decision_time_input)}{" "}
                  {t("ml.dropped_no_decision")}
                </span>
              </>,
            ],
            [t("ml.ranking"), `${status.routing.rankings} / ${status.routing.fallbacks}`],
            [t("ml.explored"), `${status.routing.explorations} / ${status.routing.rankings}`],
            status.routing.blind_explorations > 0 && [
              t("ml.blind_explored"),
              `${status.routing.blind_explorations} ${t("ml.with_no_model")}`,
            ],
            blind.length > 0 && [
              t("ml.never_tried"),
              <>
                {t("ml.blind_spot_count", { n: blind.length })}
                <StatusDot tone={blindIsPermanent ? "warn" : "ok"} />{" "}
                <div className="muted">
                  {blind.map((b) => `${b.provider_id}/${b.model_id}`).join(", ")}
                </div>
              </>,
            ],
            [
              t("ml.collected"),
              `${count(status.dataset.samples)} ${t("ml.samples")} · ${
                status.traces?.appended ?? 0
              } ${t("ml.traces")}`,
            ],
          ].filter(Boolean) as [React.ReactNode, React.ReactNode][]
        }
      />

      {decision && (
        <details style={{ marginTop: 12 }}>
          <summary>{t("ml.criteria")}</summary>
          <ul className="ml-criteria">
            {decision.criteria.map((criterion) => (
              <li key={criterion.name}>
                <StatusDot tone={criterion.held ? "ok" : "warn"} />
                <code>{criterion.name}</code>
                <span className="muted"> {criterion.reason}</span>
              </li>
            ))}
          </ul>
        </details>
      )}

      {/* -- Replay: what would the serving model have done differently? ------- */}
      <div className="row" style={{ gap: 8, marginTop: 12 }}>
        <Button onClick={() => void runReplay()} disabled={replaying}>
          {replaying ? t("ml.replaying") : t("ml.replay")}
        </Button>
        {canRollBack && (
          <Button onClick={() => void rollBack()} disabled={rollingBack}>
            {rollingBack ? t("ml.rolling_back") : t("ml.rollback_action")}
          </Button>
        )}
      </div>

      {replay && <Replay verdict={replay} />}

      {status.history.length > 0 && (
        <details style={{ marginTop: 8 }}>
          <summary>{t("ml.history")}</summary>
          <ul className="ml-criteria">
            {status.history
              .slice()
              .reverse()
              .map((entry, index) => (
                <li key={`${entry.at}-${index}`}>
                  <Badge tone={entry.action === "rollback" ? "warn" : "neutral"}>
                    {entry.action === "rollback" ? t("ml.rolled_back") : t("ml.promoted")}
                  </Badge>{" "}
                  <code>{entry.commit_id.slice(0, 8)}</code>{" "}
                  <span className="muted">{when(entry.at)}</span>
                </li>
              ))}
          </ul>
        </details>
      )}
      </Section>

      <Round
        round={round}
        running={roundRunning}
        onJudge={() => void runRound(false)}
        onInstall={() => void runRound(true)}
      />

      <Log
        log={log}
        clearing={clearing}
        onClear={() =>
          setConfirmClear({
            title: t("confirm.clear_traces"),
            body: t("confirm.clear_traces_body", { n: num(log?.records ?? 0) }),
            confirmLabel: t("common.delete"),
            danger: true,
            onConfirm: () => void clearLog(),
          })
        }
      />

      <ConfirmDialog request={confirmClear} onClose={() => setConfirmClear(null)} />
    </>
  );
}

/**
 * One promotion round: what the gate decided, and whether it is serving.
 *
 * Judging and installing are two buttons because the backend makes them two
 * acts. The install button appears only once a round has come back authorised
 * — offering it before would be offering an action the gate can refuse, and an
 * operator who learns to press buttons that do nothing learns to distrust the
 * panel.
 *
 * `policy_choices` is rendered even when the verdict is favourable, because it
 * is the number that says whether the candidate learned a ranking at all: one
 * candidate named on every request is not a learned ranking, and a promoted
 * model with that shape is the finding this panel exists to make visible.
 */
function Round({
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
function RoundResult({ round }: { round: MlPromotionRoundStatus }) {
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
      {round.error && (
        <p className="ml-fault" role="alert">
          {t("ml.round_error")}: {round.error}
        </p>
      )}
    </div>
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
function VerdictBadge({ verdict }: { verdict: PromotionVerdict }) {
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
function Log({
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
function bytesText(value: number): string {
  return num(value);
}

/**
 * The replay's numbers, or why there are none.
 *
 * The unmeasured case is shown rather than hidden. A body where the alternative
 * was never tried has no answer about the alternative, and a panel that rendered
 * a zero there would be claiming a measurement nobody made.
 */
function Replay({ verdict }: { verdict: ShadowAnalysisStatus }) {
  const { t } = useI18n();
  const analysis = verdict.analysis;

  if (!analysis) {
    return (
      <div className="ml-replay">
        <p className="muted">
          {verdict.reason ?? t("ml.replay_none")}
          {verdict.traces_read > 0 && ` (${verdict.traces_read} ${t("ml.traces")})`}
        </p>
      </div>
    );
  }

  const gaps = Object.entries(analysis.gaps).filter(([, count]) => count > 0);

  return (
    <div className="ml-replay">
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
function count(value: number): string {
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
function when(seconds: number): string {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString();
}
