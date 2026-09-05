import { useState } from "react";
import { api, costText, totalsText, type RequestRecord, type Snapshot } from "../api";
import {
  Badge,
  Button,
  ConfirmDialog,
  Drawer,
  KeyValue,
  PageHead,
  Section,
  StatusDot,
  ms,
  num,
  type ConfirmRequest,
} from "../components";
import { useI18n } from "../i18n";

/**
 * Recent requests as a timeline, not a log viewer: what was asked, what
 * answered, how long, and — for Auto Mode traffic — the outcome. Clicking a
 * row opens the request's whole story in a drawer.
 */
export default function Activity({
  snapshot,
  run,
}: {
  snapshot: Snapshot;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
}) {
  const { summary, recent, health } = snapshot;
  const { t } = useI18n();
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);

  const successRate =
    summary.requests > 0
      ? Math.round(((summary.requests - summary.failures) / summary.requests) * 1000) / 10
      : null;

  const classifier = summary.per_kind.find((k) => k.kind === "auto_mode");
  const open = recent.find((r) => r.id === openId) ?? null;

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      <PageHead
        lede={
          summary.requests === 0
            ? t("activity.lede_none")
            : t("activity.lede", {
                requests: num(summary.requests),
                rate: successRate ?? 100,
                in: num(summary.input_tokens),
                out: num(summary.output_tokens),
              })
        }
        actions={
          <Button
            kind="ghost"
            onClick={() =>
              setConfirm({
                title: t("confirm.reset_stats"),
                body: t("confirm.reset_stats_body"),
                confirmLabel: t("activity.reset"),
                danger: true,
                onConfirm: () => void run(api.clearStats),
              })
            }
          >
            {t("activity.reset")}
          </Button>
        }
      />

      {classifier && classifier.requests > 0 && (
        <p className="section-hint">
          <StatusDot tone="ok" />{" "}
          {t("activity.review_line", {
            n: num(classifier.requests),
            failed: classifier.failures,
            latency: ms(classifier.avg_latency_ms),
          })}
        </p>
      )}

      <div className="list">
        <div className="timeline">
          {recent.length === 0 ? (
            <div style={{ padding: "14px 16px" }}>
              <p className="empty">{t("activity.nothing")}</p>
            </div>
          ) : (
            recent.map((r) => (
              <TimelineRow key={r.id} record={r} onOpen={() => setOpenId(r.id)} />
            ))
          )}
        </div>
      </div>

      <Section title={t("activity.health")} hint={t("activity.health_hint")}>
        {health.length === 0 ? (
          <p className="empty">{t("activity.nothing")}</p>
        ) : (
          <div className="list">
            {health.map((h) => (
              <div className="list-row" key={h.model_id} style={{ cursor: "default" }}>
                <StatusDot
                  tone={
                    h.cooldown_remaining_secs > 0
                      ? "warn"
                      : h.total_failure > h.total_success
                        ? "danger"
                        : "ok"
                  }
                />
                <div className="row-main">
                  <span className="row-title mono">{h.model_id}</span>
                  <span className="row-sub">
                    {t("activity.ok_failed", {
                      ok: num(h.total_success),
                      failed: num(h.total_failure),
                    })}
                    {h.last_error ? ` · ${h.last_error}` : ""}
                  </span>
                </div>
                <span className="muted mono">{ms(h.avg_latency_ms)}</span>
                {h.cooldown_remaining_secs > 0 && (
                  <Badge tone="warn">
                    {t("activity.cooling", { n: h.cooldown_remaining_secs })}
                  </Badge>
                )}
                <Button kind="ghost" onClick={() => run(() => api.resetHealth(h.model_id))}>
                  {t("common.clear")}
                </Button>
              </div>
            ))}
          </div>
        )}
      </Section>

      <Section title={t("activity.spend")} hint={t("activity.spend_hint")}>
        <div className="stats">
          <div>
            <strong>{totalsText(summary.cost)}</strong>
            <span>{t("activity.estimated")}</span>
          </div>
        </div>
        {summary.per_model.some((m) => Object.keys(m.cost).length > 0) && (
          <table className="table">
            <thead>
              <tr>
                <th>{t("field.model_name")}</th>
                <th>{t("models.requests")}</th>
                <th>{t("field.input")} / {t("field.output")}</th>
                <th>{t("activity.spend")}</th>
              </tr>
            </thead>
            <tbody>
              {summary.per_model
                .filter((m) => Object.keys(m.cost).length > 0)
                .map((m) => (
                  <tr key={m.model_id}>
                    <td className="mono">{m.model_id}</td>
                    <td className="mono">
                      {num(m.requests)}
                      {m.failures > 0 && (
                      <Badge tone="danger">{t("activity.n_failed", { n: m.failures })}</Badge>
                    )}
                    </td>
                    <td className="muted mono">
                      {num(m.input_tokens)} / {num(m.output_tokens)}
                    </td>
                    <td className="mono">{totalsText(m.cost)}</td>
                  </tr>
                ))}
            </tbody>
          </table>
        )}
      </Section>

      {open && <RequestDrawer record={open} onClose={() => setOpenId(null)} />}
    </>
  );
}

function TimelineRow({
  record,
  onOpen,
}: {
  record: RequestRecord;
  onOpen: () => void;
}) {
  const isAuto = record.kind === "auto_mode";
  const { t } = useI18n();
  return (
    <button className="timeline-item" onClick={onOpen}>
      <span className="timeline-time">
        {new Date(record.at).toLocaleTimeString([], { hour12: false })}
      </span>
      <span className={`timeline-kind ${isAuto ? "auto-mode" : ""}`}>
        {isAuto ? t("kind.review") : t("kind.main")}
      </span>
      <span className="timeline-main-cell">
        {record.ok ? (
          <StatusDot tone="ok" />
        ) : (
          <StatusDot tone="danger" />
        )}
        <span className="timeline-model">{record.resolved_model ?? record.requested_model}</span>
        {record.provider_name && <span className="timeline-provider">{record.provider_name}</span>}
        {record.attempts > 1 && (
          <span className="timeline-provider">{t("activity.tries", { n: record.attempts })}</span>
        )}
      </span>
      <span className="timeline-meta">
        <span>{ms(record.latency_ms)}</span>
        {record.ok ? (
          record.cost ? <span>{costText(record.cost)}</span> : null
        ) : (
          <span>{record.status}</span>
        )}
      </span>
    </button>
  );
}

/** One request's whole story: the three model names, the outcome, the usage. */
function RequestDrawer({ record, onClose }: { record: RequestRecord; onClose: () => void }) {
  const { t } = useI18n();
  return (
    <Drawer
      title={
        <>
          <StatusDot tone={record.ok ? "ok" : "danger"} />
          {record.kind === "auto_mode" ? t("activity.review") : t("activity.request")}
        </>
      }
      onClose={onClose}
    >
      <KeyValue
        rows={[
          [t("activity.asked_for"), <span className="mono">{record.requested_model}</span>],
          [
            t("activity.answered_by"),
            record.resolved_model ? (
              <span className="mono">{record.resolved_model}</span>
            ) : (
              t("common.dash")
            ),
          ],
          [t("models.provider"), record.provider_name ?? t("common.dash")],
          [
            t("activity.kind"),
            record.kind === "auto_mode" ? t("kind.check_kv") : t("kind.main_kv"),
          ],
          ["API", record.ingress],
          [
            t("activity.result"),
            record.ok ? (
              <Badge tone="ok">200</Badge>
            ) : (
              <Badge tone="danger">{record.status}</Badge>
            ),
          ],
          [t("activity.attempts"), <span className="mono">{record.attempts}</span>],
        ]}
      />

      {record.error && <p className="field-hint">{record.error}</p>}

      <Section title={t("activity.timing")}>
        <KeyValue
          rows={[
            [t("activity.total"), <span className="mono">{ms(record.latency_ms)}</span>],
            [
              t("activity.first_token"),
              record.stream ? (
                <span className="mono">{ms(record.ttft_ms)}</span>
              ) : (
                t("common.dash")
              ),
            ],
            [t("activity.streamed"), record.stream ? t("common.yes") : t("common.no")],
          ]}
        />
      </Section>

      <Section title={t("activity.usage")}>
        <KeyValue
          rows={[
            [t("activity.input_tokens"), <span className="mono">{num(record.usage.input_tokens)}</span>],
            [t("activity.output_tokens"), <span className="mono">{num(record.usage.output_tokens)}</span>],
            [t("activity.cache_read"), <span className="mono">{num(record.usage.cache_read_tokens)}</span>],
            [t("activity.cache_write"), <span className="mono">{num(record.usage.cache_write_tokens)}</span>],
            [
              t("activity.cost"),
              record.cost ? (
                <span className="mono">{costText(record.cost)}</span>
              ) : (
                t("activity.unpriced")
              ),
            ],
          ]}
        />
      </Section>
    </Drawer>
  );
}
