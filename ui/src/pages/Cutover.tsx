/**
 * The cutover page: moving an external client's configuration onto this
 * gateway as a typed plan, and putting the file back when it goes wrong.
 *
 * # What a cutover is, and why it is not the takeover page
 *
 * Both write a client's configuration to point at this gateway. They are not
 * the same tool, and the difference is what makes this page worth having:
 *
 * - **Takeover** writes managed *fields* and records their previous values. It
 *   is the better tool for putting a client on the gateway and taking it off
 *   again, and this page deliberately offers no second set of buttons for it.
 * - **A cutover** writes the whole *file* from a staged copy, having captured
 *   the original byte for byte first. It is the only one of the two whose undo
 *   survives the process dying mid-run, because the record and the snapshot are
 *   on disk rather than in memory.
 *
 * So the case this page answers is a run that failed or was interrupted, and
 * the question of whether the user's file came back. That is why the rollback
 * button is the centre of the page rather than an afterthought.
 *
 * # Why the state and the snapshot are rendered apart
 *
 * `interrupted` and `has_snapshot` are separate readings and conflating them
 * produces the one button that must not exist: a migration interrupted with no
 * snapshot on disk has no way back, and offering rollback there would offer a
 * control that cannot help. Every claim below is conditional on what the record
 * actually says.
 */
import { useCallback, useEffect, useState } from "react";
import { useI18n, describeBackendError, type TKey } from "../i18n";
import { Badge, Button, ConfirmDialog, Empty, Section, useToast } from "../components";
import type { ConfirmRequest } from "../components";
import {
  api,
  errorText,
  type MigrationPreview,
  type MigrationResult,
  type MigrationStateName,
  type MigrationStatus,
  type TakeoverAgent,
} from "../api";

const AGENTS: TakeoverAgent[] = ["claude", "codex", "opencode", "gemini"];

/** Badge tone per migration state. */
function stateTone(state: MigrationStateName) {
  switch (state) {
    case "completed":
      return "ok" as const;
    case "failed":
      return "danger" as const;
    case "rolled_back":
      return "standard" as const;
    case "prepared":
    case "verified":
    case "switched":
      // The three that mean work started and did not finish. Distinct from
      // `failed` because nothing has gone *wrong* yet — but something is
      // unfinished, and saying so in a calmer tone is not the same as saying it
      // in a warning.
      return "warn" as const;
    default:
      return "neutral" as const;
  }
}

/** Whether a rollback is offered, and why it is not when it is not. */
function rollbackAvailability(status: MigrationStatus): { allowed: boolean; reason?: string } {
  if (status.state !== "failed") return { allowed: false, reason: "state" };
  if (!status.has_snapshot) return { allowed: false, reason: "no_snapshot" };
  return { allowed: true };
}

export default function Cutover() {
  const { t } = useI18n();
  const notify = useToast();

  const [status, setStatus] = useState<MigrationStatus | null>(null);
  const [previews, setPreviews] = useState<MigrationPreview[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [acting, setActing] = useState<TakeoverAgent | null>(null);
  const [rollingBack, setRollingBack] = useState(false);
  const [result, setResult] = useState<MigrationResult | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);

  const load = useCallback(async () => {
    try {
      const [nextStatus, nextPreviews] = await Promise.all([
        api.migrationStatus(),
        api.migrationPreviews(),
      ]);
      setStatus(nextStatus);
      const order = new Map(AGENTS.map((agent, index) => [agent, index]));
      setPreviews(
        [...nextPreviews].sort((a, b) => (order.get(a.agent) ?? 99) - (order.get(b.agent) ?? 99)),
      );
      setLoadError(null);
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
      // The empty list is kept, but the failure is remembered as one. A page
      // that cannot read the record must not render as though there were
      // nothing to read: "this build knows no external agent" and "the call
      // failed" are different facts, and only one of them is true here.
      setStatus(null);
      setPreviews([]);
      setLoadError(errorText(e));
    }
  }, [notify]);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * Apply one client's cutover.
   *
   * The result is kept on the page rather than only toasted: a failed cutover
   * is the moment the user most needs to read which step stopped it and what
   * the snapshot holds, and a toast that disappears is the wrong container for
   * that.
   */
  const apply = async (agent: TakeoverAgent, fingerprint: string) => {
    setActing(agent);
    try {
      const outcome = await api.migrationApply(agent, fingerprint);
      setResult(outcome);
      if (outcome.errors.length === 0) {
        notify("ok", t("cutover.toast_applied", { agent: t(`cutover.agent.${agent}` as TKey) }));
      } else {
        /*
         * A failed run used to say nothing at all.
         *
         * The toast was for the success case only, so the report — which is
         * on the page, in a region a screen reader does not hear — was the only
         * account of what went wrong. And the report is the one thing the
         * operator needs: the run failed, here is the step, here is the
         * reason, and the rollback below is the way back.
         */
        notify("error", t("cutover.toast_failed", { n: outcome.errors.length }));
      }
      await load();
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setActing(null);
    }
  };

  /** Put the file back, reporting honestly what actually came back. */
  const rollback = async () => {
    setRollingBack(true);
    try {
      const outcome = await api.migrationRollback();
      setResult(outcome);
      // `rolled_back` is the field to branch on, not `state`: the state machine
      // records that a transition happened, and a restore that put nothing back
      // advanced it anyway.
      if (outcome.rolled_back) {
        notify("ok", t("cutover.toast_rolled_back", { n: outcome.restored_files.length }));
      } else {
        notify("error", t("cutover.toast_nothing_restored"));
      }
      await load();
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setRollingBack(false);
    }
  };

  const rollbackState = status ? rollbackAvailability(status) : null;
  // The record admits one run, so the plans are informational afterwards. The
  // refusal is stated in the page rather than left as a dead button.
  const runnable = status?.state === "detected";

  return (
    <>
      <Section title={t("cutover.state.section")}>
        {status === null ? (
          <Empty>{t("common.loading")}</Empty>
        ) : (
          <RecordRow
            status={status}
            rollingBack={rollingBack}
            rollbackAllowed={rollbackState?.allowed ?? false}
            rollbackRefusal={rollbackRefusalReason(t, rollbackState)}
            onRollback={() =>
              setConfirm({
                title: t("cutover.confirm_rollback_title"),
                body: t("cutover.confirm_rollback_body", {
                  n: status.snapshot_files.length,
                }),
                confirmLabel: t("cutover.rollback"),
                danger: true,
                onConfirm: () => void rollback(),
              })
            }
          />
        )}
      </Section>

      {result && <ResultPanel result={result} />}

      <Section title={t("cutover.plans.section")} hint={t("cutover.plans.hint")}>
        {loadError ? (
          <Empty>{t("cutover.load_failed", { err: loadError })}</Empty>
        ) : previews === null ? (
          <Empty>{t("common.loading")}</Empty>
        ) : previews.length === 0 ? (
          <Empty>{t("cutover.none")}</Empty>
        ) : (
          <div className="list">
            {previews.map((preview) => (
              <PlanRow
                key={preview.agent}
                preview={preview}
                running={acting === preview.agent}
                runnable={runnable}
                onRun={() =>
                  setConfirm({
                    title: t("cutover.confirm_run_title", {
                      agent: t(`cutover.agent.${preview.agent}` as TKey),
                    }),
                    body: t("cutover.confirm_run_body", { n: preview.steps.length }),
                    confirmLabel: t("cutover.run"),
                    danger: true,
                    onConfirm: () => void apply(preview.agent, preview.fingerprint),
                  })
                }
              />
            ))}
          </div>
        )}
      </Section>

      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    </>
  );
}

/** Why rollback is not offered, in the page's own words. */
function rollbackRefusalReason(
  t: ReturnType<typeof useI18n>["t"],
  availability: { allowed: boolean; reason?: string } | null,
): string | null {
  if (!availability || availability.allowed) return null;
  if (availability.reason === "no_snapshot") return t("cutover.snapshot.none");
  return t("cutover.rollback_unavailable");
}

/** Where the record stands, what it holds, and the way back if there is one. */
function RecordRow({
  status,
  rollingBack,
  rollbackAllowed,
  rollbackRefusal,
  onRollback,
}: {
  status: MigrationStatus;
  rollingBack: boolean;
  rollbackAllowed: boolean;
  rollbackRefusal: string | null;
  onRollback: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className="list-row">
      <div className="row-main">
        <Badge tone={stateTone(status.state)}>{t(`cutover.state.${status.state}` as TKey)}</Badge>
        <p className="row-sub">{t(`cutover.state.hint.${status.state}` as TKey)}</p>
        <p className="row-sub muted">
          {status.has_snapshot
            ? t("cutover.snapshot.files", { n: status.snapshot_files.length })
            : t("cutover.snapshot.none")}
        </p>
        {!status.durable && <p className="row-sub warn-text">{t("cutover.not_durable")}</p>}
        {status.persist_error && (
          <p className="row-sub warn-text">
            {t("cutover.persist_error", { err: status.persist_error })}
          </p>
        )}
      </div>
      {rollbackAllowed ? (
        <div className="controls">
          <Button kind="danger" disabled={rollingBack} onClick={onRollback}>
            {rollingBack ? t("cutover.rolling_back") : t("cutover.rollback")}
          </Button>
        </div>
      ) : rollbackRefusal ? (
        <div className="row-main">
          <span className="row-sub muted">{rollbackRefusal}</span>
        </div>
      ) : null}
    </div>
  );
}

/** One client's plan: the steps, and the button that runs them. */
function PlanRow({
  preview,
  running,
  runnable,
  onRun,
}: {
  preview: MigrationPreview;
  running: boolean;
  runnable: boolean;
  onRun: () => void;
}) {
  const { t } = useI18n();
  const agentName = t(`cutover.agent.${preview.agent}` as TKey);
  const applicable = preview.blocked_reason === null && preview.steps.length > 0;

  return (
    <div className="list-row">
      <div className="row-main">
        <span className="row-title mono">{agentName}</span>
        {preview.config_path && (
          <span className="row-sub mono muted">{preview.config_path}</span>
        )}
        {preview.blocked_reason ? (
          <p className="row-sub warn-text">{preview.blocked_reason}</p>
        ) : (
          <StepsTable steps={preview.steps} />
        )}
        {applicable && !runnable && <p className="row-sub muted">{t("cutover.already_run")}</p>}
      </div>
      {applicable && (
        <div className="controls">
          <Button
            kind="danger"
            disabled={running || !runnable}
            onClick={onRun}
            title={runnable ? undefined : t("cutover.already_run")}
          >
            {running ? t("cutover.running") : t("cutover.run")}
          </Button>
        </div>
      )}
    </div>
  );
}

/** What the plan would do, in order, with what a rollback could put back. */
function StepsTable({ steps }: { steps: MigrationPreview["steps"] }) {
  const { t } = useI18n();
  return (
    <table className="table">
      <thead>
        <tr>
          <th scope="col">{t("cutover.table.undo")}</th>
          <th scope="col">{t("cutover.table.step")}</th>
        </tr>
      </thead>
      <tbody>
        {steps.map((step, index) => (
          <tr key={`${step.kind}-${index}`}>
            <td className="muted">
              {step.reversible ? t("cutover.step.reversible") : t("cutover.step.not_reversible")}
            </td>
            <td>{stepSentence(t, step)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * One step, in the reader's language.
 *
 * The plan sends the values its sentence is built from, because a sentence
 * that names a client, a port, a file or an endpoint cannot be translated
 * without them. The backend's own English sentence is the fallback for a kind
 * this build has no translation for — so a plan that grows a step shows its
 * English sentence rather than an empty cell.
 */
function stepSentence(
  t: (key: TKey, params?: Record<string, string | number>) => string,
  step: MigrationPreview["steps"][number],
): string {
  const params = step.params;
  switch (step.kind) {
    case "stop_external":
      return t("cutover.step.stop_external", { name: params.subject ?? "" });
    case "start_zroutery":
      return t("cutover.step.start_zroutery", { port: params.port ?? "" });
    case "copy_config":
      return t("cutover.step.copy_config", { path: params.path ?? "" });
    case "verify_endpoint":
      return t("cutover.step.verify_endpoint", { url: params.url ?? "" });
    default:
      return step.description;
  }
}

/**
 * What the last run produced.
 *
 * `rolled_back` and `state` are rendered as two separate claims on purpose. A
 * rollback that restored nothing still advanced the state machine, so a panel
 * reading only `state` would report a restore that never happened — which is
 * the defect the backend documents at length and the reason this panel does not
 * make the same shortcut.
 */
function ResultPanel({ result }: { result: MigrationResult }) {
  const { t } = useI18n();
  const tone =
    result.errors.length > 0 || (result.state === "rolled_back" && !result.rolled_back)
      ? "danger"
      : result.state === "rolled_back"
        ? "ok"
        : result.errors.length === 0
          ? "ok"
          : "warn";

  return (
    <Section title={t("cutover.result.title")} live>
      <div className="list-row">
        <div className="row-main">
          <Badge tone={tone}>{t(`cutover.state.${result.state}` as TKey)}</Badge>
          <p className="row-sub">
            {t("cutover.result.steps", {
              done: result.steps_completed,
              total: result.steps_total,
            })}
            <span className="muted"> · {t("cutover.result.duration", { ms: result.duration_ms })}</span>
          </p>

          {result.errors.length === 0 && result.state === "completed" && (
            <p className="row-sub">{t("cutover.result.ok")}</p>
          )}

          {result.state === "rolled_back" && result.rolled_back && (
            <p className="row-sub">{t("cutover.result.restored", { n: result.restored_files.length })}</p>
          )}
          {result.state === "rolled_back" && !result.rolled_back && (
            <p className="row-sub warn-text">
              {result.restored_files.length === 0
                ? t("cutover.result.rolled_back_nothing")
                : t("cutover.result.restore_failed", { n: result.failed_restores.length })}
            </p>
          )}

          {result.errors.map((message, index) => (
            <p key={index} className="row-sub warn-text">
              <strong>{t("cutover.result.errors")}:</strong> {describeBackendError(t, message)}
            </p>
          ))}
          {result.warnings.map((message, index) => (
            <p key={index} className="row-sub muted">
              <strong>{t("cutover.result.warnings")}:</strong> {describeBackendError(t, message)}
            </p>
          ))}
        </div>
      </div>
    </Section>
  );
}