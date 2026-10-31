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
 * from this page: `ready` asks for confirmation with the field-level diff,
 * `conflict` asks which side of each divergent field wins, `degraded` explains
 * what is not yet proven, and `released` offers the way back in. Collapsing
 * them into held/not-held would hide exactly the moments a user must decide
 * something, which is the failure mode this subsystem exists to prevent.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n, describeBackendError, type TKey } from "../i18n";
import { Badge, Button, ConfirmDialog, Disclosure, Drawer, Empty, Field, Section, Select, Toggle, useToast } from "../components";
import type { ConfirmRequest } from "../components";
import {
  api,
  errorText,
  type FieldConflict,
  type ProposedChange,
  type ResolutionReport,
  type TakeoverAgent,
  type TakeoverProposal,
  type TakeoverStatus,
  type TakeoverView,
  type ClientSetupOptions,
  type ClientSetupPreview,
} from "../api";

const AGENTS: TakeoverAgent[] = ["claude", "codex", "opencode", "gemini"];
const MENU_AGENTS: TakeoverAgent[] = ["claude", "codex", "opencode"];

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
  const [acting, setActing] = useState<TakeoverAgent | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [setup, setSetup] = useState<ClientSetupOptions>({ default_class: null, standby: false });
  const [preview, setPreview] = useState<ClientSetupPreview | null>(null);
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
    setSetupError(null);
  };
  const previewSetup = async () => {
    if (!setupAgent || acting) return;
    const version = ++setupVersion.current;
    setPreview(null);
    setSetupError(null);
    setActing(setupAgent);
    try {
      const next = await api.clientSetupPreview(setupAgent, setup);
      if (version === setupVersion.current) setPreview(next);
    } catch (error) {
      if (version === setupVersion.current) setSetupError(describeBackendError(t, errorText(error)));
    }
    finally { setActing(null); }
  };
  const applySetup = async () => {
    if (!preview || !setupAgent || acting) return;
    const version = setupVersion.current;
    setActing(setupAgent);
    setSetupError(null);
    try {
      await api.clientSetupApply(setupAgent, setup, preview.proposal.fingerprint);
      if (version === setupVersion.current) closeSetup();
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

  /** Adopt after confirmation. `fingerprint` guards the preview→click gap. */
  const adopt = async (agent: TakeoverAgent, fingerprint: string | null) => {
    setActing(agent);
    try {
      await api.takeoverAdopt(agent, fingerprint);
      notify("ok", t("takeover.toast_adopted", { agent: t(`takeover.agent.${agent}`) }));
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
                acting={acting !== null}
                strategy={strategies[view.agent] ?? "keep_external"}
                onStrategy={(next) =>
                  setStrategies((current) => ({ ...current, [view.agent]: next }))
                }
                onSetupMenu={MENU_AGENTS.includes(view.agent) ? () => openSetup(view.agent) : undefined}
                onAdopt={(proposal) =>
                  setConfirm({
                    title: t("takeover.confirm_adopt_title", {
                      agent: t(`takeover.agent.${view.agent}`),
                    }),
                    body: t("takeover.confirm_adopt_body", {
                      agent: t(`takeover.agent.${view.agent}`),
                      n: proposal.changes.length,
                    }),
                    confirmLabel: t("takeover.adopt"),
                    onConfirm: () => void adopt(view.agent, proposal.fingerprint),
                  })
                }
                onAdoptAgain={() =>
                  setConfirm({
                    title: t("takeover.confirm_adopt_title", {
                      agent: t(`takeover.agent.${view.agent}`),
                    }),
                    body: t("takeover.confirm_adopt_again_body", {
                      agent: t(`takeover.agent.${view.agent}`),
                    }),
                    confirmLabel: t("takeover.adopt_again"),
                    onConfirm: () => void adopt(view.agent, null),
                  })
                }
                onRelease={() =>
                  setConfirm({
                    title: t("takeover.confirm_release_title", {
                      agent: t(`takeover.agent.${view.agent}`),
                    }),
                    body: t("takeover.confirm_release_body", {
                      agent: t(`takeover.agent.${view.agent}`),
                    }),
                    confirmLabel: t("takeover.release"),
                    danger: true,
                    onConfirm: () => void release(view.agent),
                  })
                }
              />
            ))}
          </div>
        )}
      </Section>
      {setupAgent && (
        <Drawer
          title={`${t("takeover.sync_menu")} · ${t(`takeover.agent.${setupAgent}`)}`}
          onClose={closeSetup}
        >
          <p className="section-hint">{t("client_setup.hint")}</p>
          <div className="controls">
            <Field label={t("client_setup.default")}>
              <Select
                value={setup.default_class ?? "keep"}
                ariaLabel={t("client_setup.default")}
                disabled={acting !== null}
                options={[
                  { value: "keep", label: t("client_setup.keep") },
                  ...["fast", "standard", "reasoning", "frontier"].map((tier) => ({
                    value: `${tier}-class`, label: t(`tier.${tier}` as TKey),
                  })),
                ]}
                onChange={(value) => {
                  setSetup({ ...setup, default_class: value === "keep" ? null : value });
                  setPreview(null);
                }}
              />
            </Field>
            <div className="field-actions">
              <Button disabled={acting !== null} onClick={() => void previewSetup()}>
                {t("client_setup.preview")}
              </Button>
            </div>
          </div>
          <Toggle
            label={t("client_setup.standby")}
            checked={setup.standby}
            disabled={acting !== null}
            onChange={(standby) => { setSetup({ ...setup, standby }); setPreview(null); }}
          />
          {setupError && <p className="field-error" role="alert">{setupError}</p>}
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
            <div className="controls">
              <Button disabled={acting !== null} kind="primary" onClick={() => setConfirm({
                title: t("client_setup.apply"),
                body: t("client_setup.confirm"),
                confirmLabel: t("client_setup.apply"),
                onConfirm: () => void applySetup(),
              })}>
                {t("client_setup.apply")}
              </Button>
            </div>
          </>}
        </Drawer>
      )}
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    </>
  );
}

/** One agent's whole reading, with the action its state allows. */
function AgentRow({
  view,
  acting,
  strategy,
  onStrategy,
  onAdopt,
  onAdoptAgain,
  onRelease,
  onSetupMenu,
}: {
  view: TakeoverView;
  acting: boolean;
  strategy: ResolutionReport["strategy"];
  onStrategy: (next: ResolutionReport["strategy"]) => void;
  onAdopt: (proposal: TakeoverProposal) => void;
  onAdoptAgain: () => void;
  onRelease: () => void;
  /** Present only for agents the native menu can be written to; the row is
   * the one place that client's actions live, takeover and menu alike. */
  onSetupMenu?: () => void;
}) {
  const { t } = useI18n();
  const status = view.status;
  const agentName = t(`takeover.agent.${view.agent}` as TKey);
  const stateLabel = t(`takeover.state.${status.state}` as TKey);
  // Every state with a path to show: where the managed config lives. For
  // not_detected the searched list replaces it — that list is the only thing
  // that lets the user fix the detection themselves.
  const path =
    "config_path" in status
      ? status.config_path
      : status.state === "not_detected"
        ? status.searched.join(" · ")
        : null;

  return (
    <div className="list-row" data-agent={view.agent}>
      <div className="row-main">
        <span className="row-title mono">{agentName}</span>
        <Badge tone={stateTone(status)}>{stateLabel}</Badge>
        {path && <span className="row-sub mono muted">{path}</span>}
        <StatusDetail status={status} agentName={agentName} />
      </div>
      {(holdsOwnership(status)) && (
        <div className="controls">
          {onSetupMenu && (
            <Button kind="ghost" disabled={acting} onClick={onSetupMenu}>
              {t("takeover.sync_menu")}
            </Button>
          )}
          {status.state === "conflict" && (
            <Select
              ariaLabel={t("takeover.strategy.label")}
              value={strategy}
              disabled={acting}
              onChange={onStrategy}
              options={[
                { value: "keep_external", label: t("takeover.strategy.keep_external") },
                {
                  value: "overwrite_with_managed",
                  label: t("takeover.strategy.overwrite"),
                },
                { value: "skip", label: t("takeover.strategy.skip") },
              ]}
            />
          )}
          <Button kind="danger" disabled={acting} onClick={onRelease}>
            {acting ? t("takeover.working") : t("takeover.release")}
          </Button>
        </div>
      )}
      {status.state === "ready" && (
        <div className="controls">
          {onSetupMenu && (
            <Button kind="ghost" disabled={acting} onClick={onSetupMenu}>
              {t("takeover.sync_menu")}
            </Button>
          )}
          <Button kind="primary" disabled={acting} onClick={() => onAdopt(status.proposal)}>
            {acting ? t("takeover.working") : t("takeover.adopt")}
          </Button>
        </div>
      )}
      {status.state === "released" && (
        <div className="controls">
          {onSetupMenu && (
            <Button kind="ghost" disabled={acting} onClick={onSetupMenu}>
              {t("takeover.sync_menu")}
            </Button>
          )}
          <Button kind="primary" disabled={acting} onClick={onAdoptAgain}>
            {acting ? t("takeover.working") : t("takeover.adopt_again")}
          </Button>
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
          <ChangesTable rows={status.proposal.changes} kind="proposal" />
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
          <ConflictTable conflicts={status.conflicts} />
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
      return <p className="row-sub warn-text">{status.reason}</p>;

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
