import { useCallback, useEffect, useState } from "react";
import {
  api,
  errorText,
  type MlRollback,
  type MlStatus,
  type ShadowAnalysisStatus,
} from "../api";
import { Badge, Button, KeyValue, Section, StatusDot } from "../components";
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
  const [status, setStatus] = useState<MlStatus | null>(null);
  const [replay, setReplay] = useState<ShadowAnalysisStatus | null>(null);
  const [replaying, setReplaying] = useState(false);
  const [rollingBack, setRollingBack] = useState(false);
  const [fault, setFault] = useState<string | null>(null);

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

  useEffect(() => {
    void read();
    if (!available) return;
    const timer = setInterval(() => void read(), POLL_MS);
    return () => clearInterval(timer);
  }, [read, available]);

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

  return (
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
            [t("ml.ranking"), `${status.routing.rankings} / ${status.routing.fallbacks}`],
            [t("ml.explored"), `${status.routing.explorations} / ${status.routing.rankings}`],
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
  );
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
