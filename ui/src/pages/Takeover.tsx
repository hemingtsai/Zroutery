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
import { useCallback, useEffect, useState } from "react";
import { useI18n, type TKey } from "../i18n";
import { Badge, Button, ConfirmDialog, Empty, Section, Select, useToast } from "../components";
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
} from "../api";

const AGENTS: TakeoverAgent[] = ["claude", "codex", "opencode", "gemini"];

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
  const [acting, setActing] = useState<TakeoverAgent | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  // Per-agent release strategy, preselected to the safe default: a release
  // that hits a conflict keeps what the user changed unless they say otherwise.
  const [strategies, setStrategies] = useState<
    Record<string, ResolutionReport["strategy"]>
  >({});

  const load = useCallback(async () => {
    try {
      const next = await api.takeoverStatus();
      // Reorder to the page's fixed presentation order, whatever the backend
      // enumerated; an agent added server-side still shows up (appended).
      const order = new Map(AGENTS.map((agent, index) => [agent, index]));
      setViews([...next].sort((a, b) => (order.get(a.agent) ?? 99) - (order.get(b.agent) ?? 99)));
    } catch (e) {
      notify("error", errorText(e));
      setViews([]);
    }
  }, [notify]);

  useEffect(() => {
    void load();
  }, [load]);

  /** Adopt after confirmation. `fingerprint` guards the preview→click gap. */
  const adopt = async (agent: TakeoverAgent, fingerprint: string | null) => {
    setActing(agent);
    try {
      await api.takeoverAdopt(agent, fingerprint);
      notify("ok", t("takeover.toast_adopted", { agent: t(`takeover.agent.${agent}`) }));
      await load();
    } catch (e) {
      notify("error", errorText(e));
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
      notify("error", errorText(e));
    } finally {
      setActing(null);
    }
  };

  return (
    <>
      <Section title={t("takeover.section")} hint={t("takeover.section_hint")}>
        {views === null ? (
          <Empty>{t("common.loading")}</Empty>
        ) : views.length === 0 ? (
          <Empty>{t("takeover.none")}</Empty>
        ) : (
          <div className="list">
            {views.map((view) => (
              <AgentRow
                key={view.agent}
                view={view}
                acting={acting === view.agent}
                strategy={strategies[view.agent] ?? "keep_external"}
                onStrategy={(next) =>
                  setStrategies((current) => ({ ...current, [view.agent]: next }))
                }
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
}: {
  view: TakeoverView;
  acting: boolean;
  strategy: ResolutionReport["strategy"];
  onStrategy: (next: ResolutionReport["strategy"]) => void;
  onAdopt: (proposal: TakeoverProposal) => void;
  onAdoptAgain: () => void;
  onRelease: () => void;
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
    <div className="list-row">
      <div className="row-main">
        <span className="row-title mono">{agentName}</span>
        <Badge tone={stateTone(status)}>{stateLabel}</Badge>
        {path && <span className="row-sub mono muted">{path}</span>}
        <StatusDetail status={status} agentName={agentName} />
      </div>
      {(holdsOwnership(status)) && (
        <div className="controls">
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
          <Button kind="primary" disabled={acting} onClick={() => onAdopt(status.proposal)}>
            {acting ? t("takeover.working") : t("takeover.adopt")}
          </Button>
        </div>
      )}
      {status.state === "released" && (
        <div className="controls">
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
      return (
        <p className="row-sub">
          {t("takeover.hint.not_detected", { agent: agentName })}
          {status.reason && <span className="muted"> · {status.reason}</span>}
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
  if (/not been verified/i.test(reason)) return <>{t("takeover.reason.unverified")}</>;
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
          <th>{t("takeover.table.field")}</th>
          <th>{t("takeover.table.current")}</th>
          <th>{kind === "proposal" ? t("takeover.table.next") : t("takeover.table.managed")}</th>
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
          <th>{t("takeover.table.field")}</th>
          <th>{t("takeover.table.managed")}</th>
          <th>{t("takeover.table.current")}</th>
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
