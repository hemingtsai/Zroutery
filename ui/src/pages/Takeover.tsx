/**
 * The takeover page: which external agents point at Zroutery, and who owns
 * the fields that say so.
 *
 * # What a takeover is, in one paragraph
 *
 * Zroutery writes its local gateway address and token into a client's own
 * configuration file (Claude Code's `~/.claude/settings.json`, Codex's
 * `~/.codex/config.toml`, OpenCode's `opencode.json`), records what it
 * replaced, and restores those values on release. The client keeps its own
 * model choice; only the fields Zroutery wrote are managed.
 *
 * # Why every backend state gets its own rendering
 *
 * The nine states are nine different truths, and four of them need a decision
 * from this page: `ready` shows the field-level diff and adopts in one click
 * (the diff on the row *is* the preview — confirming the same two fields a
 * second time taught the reader to click through), `conflict` offers the way
 * back to managed values and — on release — asks which side of each
 * divergent field wins, `degraded` explains what is not yet proven, and
 * `released` offers the way back in. Only release, the destructive act,
 * confirms. Collapsing the states into held/not-held would hide exactly the
 * moments a user must decide something, which is the failure mode this
 * subsystem exists to prevent.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n, describeBackendError, type TKey } from "../i18n";
import { Badge, Button, ConfirmDialog, Disclosure, Drawer, Empty, Field, Section, Segment, Select, Toggle, useToast } from "../components";
import type { ConfirmRequest } from "../components";
import {
  api,
  errorText,
  type FieldConflict,
  type ProposedChange,
  type ResolutionReport,
  type TakeoverAgent,
  type TakeoverStatus,
  type TakeoverView,
  type ClientSetupOptions,
  type ClientSetupPreview,
} from "../api";
import { SaveFeedbackReset } from "../saveQueue";

const AGENTS: TakeoverAgent[] = ["claude", "codex", "opencode", "gemini"];
const MENU_AGENTS: TakeoverAgent[] = ["claude", "codex", "opencode"];

/** The four usage tiers, with their i18n keys spelled out.
 *
 * A template literal — `tier.${tier}` — once compiled to a key that does not
 * exist in any dictionary, and the `as TKey` cast swallowed the one proof tsc
 * could have given; `t()` returned undefined and the Radix select portalled
 * that undefined into its trigger, so the "默认用途档位" control went visually
 * dead the moment a tier was chosen. An explicit record lets the compiler
 * check every key, and there is no cast to silently widen.
 */
const TIERS = ["fast", "standard", "reasoning", "frontier"] as const;
const TIER_LABEL: Record<(typeof TIERS)[number], TKey> = {
  fast: "tier.name.fast",
  standard: "tier.name.standard",
  reasoning: "tier.name.reasoning",
  frontier: "tier.name.frontier",
};

/** A config value as a reviewable fragment: JSON for anything but a string. */
function valueText(value: unknown): string {
  if (value === null || value === undefined) return "—";
  const text = typeof value === "string" ? value : JSON.stringify(value);
  return text.length > 48 ? `${text.slice(0, 45)}…` : text;
}

/** Badge tone per state; the neutral default keeps unknown states visible. */
function stateTone(status: TakeoverStatus) {
  switch (status.state) {
    case "active":
      return "ok" as const;
    case "ready":
      return "standard" as const;
    case "conflict":
    case "error":
      return "danger" as const;
    case "degraded":
    case "available":
      return "warn" as const;
    default:
      return "neutral" as const;
  }
}

export default function Takeover() {
  const { t } = useI18n();
  const notify = useToast();

  const [views, setViews] = useState<TakeoverView[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  // A per-row in-flight marker, not a page-wide lock: one client's adopt must
  // not freeze the other three rows. Each button answers only for its own
  // agent; the 3s poll still pauses while anything is in flight, which keeps
  // a landing response from being overwritten mid-action by a stale poll.
  const [acting, setActing] = useState<TakeoverAgent | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [setup, setSetup] = useState<ClientSetupOptions>({ default_class: null, standby: false });
  const [preview, setPreview] = useState<ClientSetupPreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [setupError, setSetupError] = useState<string | null>(null);
  const setupVersion = useRef(0);
  // The native-menu sync lives in a per-client drawer now: the row is the one
  // place a client's actions live, so its menu sync opens from the row rather
  // than from a top form that made the reader pick the client all over again.
  const [setupAgent, setSetupAgent] = useState<TakeoverAgent | null>(null);
  const openSetup = (agent: TakeoverAgent) => {
    setupVersion.current += 1;
    setSetupAgent(agent);
    setSetup({ default_class: null, standby: false });
    setPreview(null);
    setSetupError(null);
  };
  const closeSetup = () => {
    setupVersion.current += 1;
    setSetupAgent(null);
    setPreview(null);
    setPreviewing(false);
    setSetupError(null);
  };
  // The drawer previews itself. The preview call is read-only and cheap, and
  // the reference grammar (cc-switch, magpie) is one decision, not
  // preview-then-decide: the diff re-derives the moment the drawer opens and
  // whenever an option changes, instead of a mandatory "preview" button whose
  // result every option tweak silently invalidated. Each preview is
  // versioned; only the newest option set may land.
  useEffect(() => {
    if (!setupAgent) return;
    const version = ++setupVersion.current;
    setPreviewing(true);
    const timer = setTimeout(() => {
      api.clientSetupPreview(setupAgent, setup)
        .then((next) => {
          if (version !== setupVersion.current) return;
          setPreview(next);
          setSetupError(null);
        })
        .catch((error) => {
          if (version !== setupVersion.current) return;
          setPreview(null);
          setSetupError(describeBackendError(t, errorText(error)));
        })
        .finally(() => {
          if (version === setupVersion.current) setPreviewing(false);
        });
    }, 200);
    return () => clearTimeout(timer);
  }, [setupAgent, setup, t]);
  const applySetup = async () => {
    if (!setupAgent || !preview || acting === setupAgent) return;
    const agent = setupAgent;
    const fingerprint = preview.proposal.fingerprint;
    const version = setupVersion.current;
    setActing(agent);
    setSetupError(null);
    try {
      await api.clientSetupApply(agent, setup, fingerprint);
      if (version === setupVersion.current) closeSetup();
      notify("ok", `${t("client_setup.toast_applied", { agent: t(`takeover.agent.${agent}`) })} ${t(`takeover.restart.${agent}`)}`);
      await load();
    } catch (error) {
      if (version === setupVersion.current) setSetupError(describeBackendError(t, errorText(error)));
    }
    finally { setActing(null); }
  };
  // Per-agent release strategy, preselected to the safe default: a release
  // that hits a conflict keeps what the user changed unless they say otherwise.
  const [strategies, setStrategies] = useState<
    Record<string, ResolutionReport["strategy"]>
  >({});

  const load = useCallback(async (quiet = false) => {
    try {
      const next = await api.takeoverStatus();
      // Reorder to the page's fixed presentation order, whatever the backend
      // enumerated; an agent added server-side still shows up (appended).
      const order = new Map(AGENTS.map((agent, index) => [agent, index]));
      setViews([...next].sort((a, b) => (order.get(a.agent) ?? 99) - (order.get(b.agent) ?? 99)));
      setLoadError(null);
    } catch (e) {
      if (quiet) return;
      notify("error", describeBackendError(t, errorText(e)));
      // The empty list is kept, but the failure is remembered as one. A page
      // that could not read the takeover record must not render as though
      // there were no external agents: "this build knows no external agent" and
      // "the call failed" are different facts, and only one of them is true.
      setViews([]);
      setLoadError(errorText(e));
    }
  }, [notify, t]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (acting) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      await load(true);
      if (!cancelled) timer = setTimeout(refresh, 3000);
    };
    timer = setTimeout(refresh, 3000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [load, acting]);

  /** Adopt in one step. The row's inline diff is the preview, and the
   * fingerprint guards the read→click gap; the reference projects confirm
   * only the destructive act (release), so adoption does not ask twice about
   * the same two fields. The toast carries the restart verb this client
   * needs — a written config is not yet an effect. */
  const adopt = async (agent: TakeoverAgent, fingerprint: string | null) => {
    setActing(agent);
    try {
      await api.takeoverAdopt(agent, fingerprint);
      notify("ok", `${t("takeover.toast_adopted", { agent: t(`takeover.agent.${agent}`) })} ${t(`takeover.restart.${agent}`)}`);
      await load();
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setActing(null);
    }
  };

  /** Release after confirmation, then show what was decided, per field. */
  const release = async (agent: TakeoverAgent) => {
    const strategy = strategies[agent] ?? "keep_external";
    setActing(agent);
    try {
      const view = await api.takeoverRelease(agent, strategy);
      const parts: string[] = [];
      if (view.report.kept_external.length > 0) {
        parts.push(t("takeover.report_kept", { n: view.report.kept_external.length }));
      }
      if (view.report.overwritten.length > 0) {
        parts.push(t("takeover.report_overwritten", { n: view.report.overwritten.length }));
      }
      if (view.report.skipped.length > 0) {
        parts.push(t("takeover.report_skipped", { n: view.report.skipped.length }));
      }
      notify("ok", `${t("takeover.toast_released")}${parts.length ? ` · ${parts.join(" · ")}` : ""}`);
      await load();
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setActing(null);
    }
  };

  /** Release is the one destructive act, so it is the one that confirms — and
   * the confirm carries the choice that used to be a bare unlabelled dropdown
   * on the row: what each field the client changed since should become. */
  const releaseRequest = (view: TakeoverView): ConfirmRequest => {
    const agentName = t(`takeover.agent.${view.agent}` as TKey);
    return {
      title: t("takeover.confirm_release_title", { agent: agentName }),
      body: (
        <>
          {view.status.state !== "error" && "config_path" in view.status && (
            <p className="confirm-path mono">{view.status.config_path}</p>
          )}
          <p className="confirm-note">{t("takeover.confirm_release_body", { agent: agentName })}</p>
          <Segment<ResolutionReport["strategy"]>
            ariaLabel={t("takeover.strategy.label")}
            value={strategies[view.agent] ?? "keep_external"}
            onChange={(next) => setStrategies((current) => ({ ...current, [view.agent]: next }))}
            options={[
              { value: "keep_external", label: t("takeover.strategy.keep_external") },
              { value: "overwrite_with_managed", label: t("takeover.strategy.overwrite") },
              { value: "skip", label: t("takeover.strategy.skip") },
            ]}
          />
        </>
      ),
      confirmLabel: t("takeover.release"),
      danger: true,
      onConfirm: () => void release(view.agent),
    };
  };

  return (
    <>
      <Section title={t("takeover.section")} hint={t("takeover.section_hint")}>
        {loadError ? (
          <Empty>{t("takeover.load_failed", { err: loadError })}</Empty>
        ) : views === null ? (
          <Empty>{t("common.loading")}</Empty>
        ) : views.length === 0 ? (
          <Empty>{t("takeover.none")}</Empty>
        ) : (
          <div className="list">
            {views.map((view) => (
              <AgentRow
                key={view.agent}
                view={view}
                actingAgent={acting}
                onSetupMenu={MENU_AGENTS.includes(view.agent) ? () => openSetup(view.agent) : undefined}
                onAdopt={(fingerprint) => void adopt(view.agent, fingerprint)}
                onRelease={() => setConfirm(releaseRequest(view))}
                onRefresh={() => void load()}
              />
            ))}
          </div>
        )}
      </Section>
      {setupAgent && (
        <Drawer
          title={`${t("takeover.setup_drawer")} · ${t(`takeover.agent.${setupAgent}`)}`}
          onClose={closeSetup}
        >
          {/* The sync menu is a client task, not a configuration edit: it
              opens with no verdict inherited from any earlier surface. */}
          <SaveFeedbackReset />
          <p className="section-hint">{t("client_setup.hint")}</p>
          <div className="controls">
            <Field label={t("client_setup.default")}>
              <Select
                value={setup.default_class ?? "keep"}
                ariaLabel={t("client_setup.default")}
                disabled={acting === setupAgent}
                options={[
                  { value: "keep", label: t("client_setup.keep") },
                  ...TIERS.map((tier) => ({
                    value: `${tier}-class`, label: t(TIER_LABEL[tier]),
                  })),
                ]}
                onChange={(value) =>
                  setSetup({ ...setup, default_class: value === "keep" ? null : value })
                }
              />
            </Field>
          </div>
          <Toggle
            label={t("client_setup.standby")}
            checked={setup.standby}
            disabled={acting === setupAgent}
            onChange={(standby) => setSetup({ ...setup, standby })}
          />
          {setupError && <p className="field-error" role="alert">{setupError}</p>}
          {previewing && <p className="field-hint" role="status">{t("client_setup.previewing")}</p>}
          {preview && <>
            <p className="field-hint">{preview.gateway_url} · {preview.config_path}</p>
            <div className="table-scroll">
              <table className="table">
                <thead><tr>
                  <th scope="col">{t("client_setup.pool")}</th>
                  <th scope="col">{t("models.effort_levels")}</th>
                  <th scope="col">{t("client_setup.evidence")}</th>
                </tr></thead>
                <tbody>{preview.classes.map((entry) => <tr key={entry.model}>
                  <td>{entry.display_name}</td>
                  <td>{entry.effort_levels.join(" / ") || t("common.unknown")}</td>
                  <td>{entry.capability_source}</td>
                </tr>)}</tbody>
              </table>
            </div>
            {preview.unavailable_classes.length > 0 && (
              <p className="field-hint">{t("client_setup.unavailable", { ids: preview.unavailable_classes.join(", ") })}</p>
            )}
            {preview.notes.map((note) => <p className="field-hint" key={note}>{note}</p>)}
            <Disclosure title={t("takeover.hint.ready", { n: preview.proposal.changes.length })} defaultOpen>
              <div className="table-scroll"><ChangesTable rows={preview.proposal.changes} kind="proposal" /></div>
            </Disclosure>
          </>}
          {/* The sync's one press lives outside the preview block and simply
              waits, disabled, for the auto-preview to land — a control that
              appears only after a fetch would let the drawer's geometry
              depend on a request's latency, and read as nothing to press. */}
          <div className="controls">
            <Button
              disabled={!preview || previewing || acting === setupAgent}
              kind="primary"
              onClick={() => void applySetup()}
            >
              {acting === setupAgent ? t("takeover.working") : t("client_setup.apply")}
            </Button>
          </div>
        </Drawer>
      )}
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    </>
  );
}

/** One agent's whole reading, with the action its state allows.
 *
 * One action strip for every state — the sync button used to be spelled out
 * in three separate state branches and drifted copy-paste-wise. The button
 * set is derived from the state; `actingAgent` locks only this row, so a
 * slow adopt on Codex does not freeze the other three clients.
 */
function AgentRow({
  view,
  actingAgent,
  onAdopt,
  onRelease,
  onRefresh,
  onSetupMenu,
}: {
  view: TakeoverView;
  actingAgent: TakeoverAgent | null;
  onAdopt: (fingerprint: string | null) => void;
  onRelease: () => void;
  /** The way out of the dead ends: not_detected, error and available rows
   * had no action at all, and the only refresh was a hidden 3-second poll. */
  onRefresh: () => void;
  /** Present only for agents the native menu can be written to; the row is
   * the one place that client's actions live, takeover and menu alike. */
  onSetupMenu?: () => void;
}) {
  const { t } = useI18n();
  const status = view.status;
  const agentName = t(`takeover.agent.${view.agent}` as TKey);
  const stateLabel = t(`takeover.state.${status.state}` as TKey);
  const acting = actingAgent === view.agent;
  // Every state with a path to show: where the managed config lives. For
  // not_detected the searched list replaces it — that list is the only thing
  // that lets the user fix the detection themselves.
  const path =
    "config_path" in status
      ? status.config_path
      : status.state === "not_detected"
        ? status.searched.join(" · ")
        : null;

  const menuReachable =
    status.state !== "not_detected" &&
    status.state !== "error" &&
    status.state !== "available" &&
    status.state !== "unsupported";
  // Ready adopts with the previewed fingerprint; released and conflict
  // re-adopt without one (the backend recomputes). On conflict this is the
  // "apply again" pill the reference projects put on drifted state — the
  // way back to managed values that the row previously did not offer at all.
  const canAdopt =
    status.state === "ready" ||
    status.state === "released" ||
    status.state === "conflict";
  const canRelease = holdsOwnership(status);
  const canRefresh =
    status.state === "not_detected" ||
    status.state === "error" ||
    status.state === "available";
  const anyAction = Boolean(onSetupMenu && menuReachable) || canAdopt || canRelease || canRefresh;

  return (
    <div className="list-row" data-agent={view.agent}>
      <div className="row-main">
        <span className="row-title mono">{agentName}</span>
        <Badge tone={stateTone(status)}>{stateLabel}</Badge>
        {path && <span className="row-sub mono muted">{path}</span>}
        <StatusDetail status={status} agentName={agentName} />
      </div>
      {anyAction && (
        <div className="controls">
          {onSetupMenu && menuReachable && (
            <Button kind="ghost" disabled={acting} onClick={onSetupMenu}>
              {t("takeover.sync_menu")}
            </Button>
          )}
          {canRefresh && (
            <Button kind="ghost" disabled={acting} onClick={onRefresh}>
              {t("takeover.refresh")}
            </Button>
          )}
          {canAdopt && (
            <Button
              kind="primary"
              disabled={acting}
              onClick={() => onAdopt(status.state === "ready" ? status.proposal.fingerprint : null)}
            >
              {acting ? t("takeover.working") : status.state === "ready" ? t("takeover.adopt") : t("takeover.adopt_again")}
            </Button>
          )}
          {canRelease && (
            <Button kind="danger" disabled={acting} onClick={onRelease}>
              {acting ? t("takeover.working") : t("takeover.release")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

/** Whether Zroutery currently holds this client's managed fields. */
function holdsOwnership(status: TakeoverStatus): boolean {
  return (
    status.state === "active" ||
    status.state === "degraded" ||
    status.state === "conflict"
  );
}

/** The state's own explanation, and its field-level detail where it has any. */
function StatusDetail({
  status,
  agentName,
}: {
  status: TakeoverStatus;
  agentName: string;
}) {
  const { t } = useI18n();

  switch (status.state) {
    case "unsupported":
      // The only unsupported client is Gemini CLI, and the reason is the same
      // every time: its endpoint is an environment variable no manifest can
      // restore. Stated in the page's own words, not the backend's.
      return <p className="row-sub">{t("takeover.hint.unsupported")}</p>;

    case "not_detected":
      // The localized hint already states the finding in the page's own
      // words; the backend's raw English reason (e.g. "no OpenCode
      // configuration found") would repeat it verbatim after the
      // translation, so it rides only in the hover title.
      return (
        <p className="row-sub" title={status.reason}>
          {t("takeover.hint.not_detected", { agent: agentName })}
        </p>
      );

    case "available":
      return (
        <p className="row-sub">
          <KnownBlocker blocker={status.blocker} />
        </p>
      );

    case "ready":
      return (
        <>
          <p className="row-sub">{t("takeover.hint.ready", { n: status.proposal.changes.length })}</p>
          <div className="table-scroll">
            <ChangesTable rows={status.proposal.changes} kind="proposal" />
          </div>
          <p className="row-sub muted">{t("takeover.confirm_inline", { agent: agentName })}</p>
        </>
      );

    case "active":
      return (
        <p className="row-sub">
          {t("takeover.hint.active")}
          <span className="muted"> · {t("takeover.generation", { n: status.generation })}</span>
        </p>
      );

    case "degraded":
      return (
        <>
          <p className="row-sub">
            {t("takeover.hint.degraded")}
            <span className="muted"> · {t("takeover.generation", { n: status.generation })}</span>
          </p>
          {status.reasons.map((reason, index) => (
            <p key={index} className="row-sub warn-text">
              <KnownReason reason={reason} />
            </p>
          ))}
        </>
      );

    case "conflict":
      return (
        <>
          <p className="row-sub warn-text">{t("takeover.hint.conflict", { n: status.conflicts.length })}</p>
          <div className="table-scroll">
            <ConflictTable conflicts={status.conflicts} />
          </div>
        </>
      );

    case "released":
      return (
        <p className="row-sub">
          {t("takeover.hint.released")}
          <span className="muted"> · {t("takeover.generation", { n: status.generation })}</span>
        </p>
      );

    case "error":
      // Through the same translation pass the degraded reasons take; only a
      // reason no rule knows falls back to the backend's own English.
      return <p className="row-sub warn-text"><KnownReason reason={status.reason} /></p>;

    default:
      return null;
  }
}

/** The two blockers Zroutery itself can be in, in the page's own words. */
function KnownBlocker({ blocker }: { blocker: string }) {
  const { t } = useI18n();
  if (/token/i.test(blocker)) return <>{t("takeover.blocker.no_token")}</>;
  if (/model classes|tier/i.test(blocker)) return <>{t("takeover.blocker.no_classes")}</>;
  return <>{blocker}</>;
}

/** The two honest reasons a held takeover is not `active`, translated. */
function KnownReason({ reason }: { reason: string }) {
  const { t } = useI18n();
  if (/not been verified|no request from this client/i.test(reason)) return <>{t("takeover.reason.unverified")}</>;
  if (/token has changed/i.test(reason)) return <>{t("takeover.reason.token_changed")}</>;
  return <>{reason}</>;
}

/** What an adoption would change, per field. Shown before it can happen. */
function ChangesTable({
  rows,
  kind,
}: {
  rows: ProposedChange[];
  kind: "proposal" | "conflict";
}) {
  const { t } = useI18n();
  return (
    <table className="table">
      <thead>
        <tr>
          <th scope="col">{t("takeover.table.field")}</th>
          <th scope="col">{t("takeover.table.current")}</th>
          <th scope="col">{kind === "proposal" ? t("takeover.table.next") : t("takeover.table.managed")}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.field_path}>
            <td className="mono">{row.field_path}</td>
            <td className="mono muted">{valueText(row.current)}</td>
            <td className="mono">{valueText(row.managed)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** A field someone changed behind the takeover, with both sides. */
function ConflictTable({ conflicts }: { conflicts: FieldConflict[] }) {
  const { t } = useI18n();
  return (
    <table className="table">
      <thead>
        <tr>
          <th scope="col">{t("takeover.table.field")}</th>
          <th scope="col">{t("takeover.table.managed")}</th>
          <th scope="col">{t("takeover.table.current")}</th>
        </tr>
      </thead>
      <tbody>
        {conflicts.map((conflict) => (
          <tr key={conflict.field_path}>
            <td className="mono">{conflict.field_path}</td>
            <td className="mono muted">{valueText(conflict.last_applied)}</td>
            <td className="mono warn-text">{valueText(conflict.current_external)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
