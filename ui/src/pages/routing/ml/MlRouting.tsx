/**
 * The learned-routing (ML) tab: the self-polling panel with its promotion
 * rounds, gate criteria, replay, trace log and collection config; the
 * presentational panels live in `./panels`.
 */
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
  type ShadowAnalysisStatus,
} from "../../../api";
import {
  Badge,
  Banner,
  Button,
  ConfirmDialog,
  Disclosure,
  KeyValue,
  Section,
  StatusDot,
  useToast,
  num,
  type ConfirmRequest,
} from "../../../components";
import { useI18n } from "../../../i18n";


import {
  CollectionConfig,
  GateCriteria,
  Log,
  Replay,
  Round,
  bytesText,
  count,
  when,
} from "./panels";
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
  snapshot,
  save,
}: {
  /** Whether this build has a learning stack, from the snapshot. */
  available: boolean;
  /** Hands a freshly read status up, so the page can stop showing stale facts. */
  onStatus?: (status: MlStatus) => void;
  snapshot?: import("../../../api").Snapshot;
  save?: (mutate: (config: import("../../../api").AppConfig) => import("../../../api").AppConfig | null) => Promise<boolean>;
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
    // The status document walks every candidate, so a panel left open in a
    // background window pays that cost for a screen nobody is looking at. Skip
    // the tick while hidden, and refresh once when the window comes back.
    const timer = setInterval(() => {
      if (document.visibilityState === "hidden") return;
      void read();
    }, POLL_MS);
    const onVisibility = () => {
      if (document.visibilityState === "visible") void read();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
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

      {/* Detailed collection config — the 3 knobs that were invisible and caused 0 samples */}
      {snapshot && save && (
        <CollectionConfig snapshot={snapshot} save={save} status={status} />
      )}

      {/*
        The blind-spot warning is advice, not a failure, so it renders as a warn
        banner and says only what to do. It used to interpolate every blind
        model id into a danger-toned paragraph — with a real catalogue that was
        thirty-one ids across six red lines, announced in full to a screen
        reader, for what is one sentence of advice. The names live in the
        disclosure beside the counters below.
      */}
      {blindIsPermanent && <Banner tone="warn">{t("ml.blind_spot_warning")}</Banner>}

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
            [
              t("ml.collected"),
              `${count(status.dataset.samples)} ${t("ml.samples")} · ${
                status.traces?.appended ?? 0
              } ${t("ml.traces")}`,
            ],
          ].filter(Boolean) as [React.ReactNode, React.ReactNode][]
        }
      />

      {/*
        The blind candidates, folded. The count is the fact a reader scans for;
        the thirty-one ids behind it are the detail they asked for, and a
        detail asked for is exactly what the disclosure is for. `defaultOpen`
        stays false even at one entry: the row reads the same whatever the
        size of the pool, and a list that opens itself is a list the reader
        has to close.
      */}
      {blind.length > 0 && (
        <Disclosure
          title={t("ml.never_tried")}
          count={t("ml.blind_spot_count", { n: blind.length })}
        >
          <ul className="ml-blind">
            {blind.map((b) => (
              <li key={`${b.provider_id}/${b.model_id}`}>
                <code>
                  {b.provider_id}/{b.model_id}
                </code>
              </li>
            ))}
          </ul>
        </Disclosure>
      )}

      {decision && <GateCriteria decision={decision} />}

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
        <Disclosure title={t("ml.history")} count={status.history.length}>
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
        </Disclosure>
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
