import { useCallback, useEffect, useState } from "react";
import {
  api,
  costText,
  isSideKind,
  totalsText,
  type RequestKind,
  type RequestRecord,
  type Snapshot,
} from "../api";
import {
  Badge,
  Button,
  ConfirmDialog,
  Drawer,
  KeyValue,
  PageHead,
  Section,
  StatusDot,
  Tabs,
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
  const [tab, setTab] = useState<"usage" | "log">("usage");
  /*
   * The internal log, here rather than on the settings page it used to live on.
   *
   * It was the one observability surface not on this page, which is otherwise
   * entirely observability — health, spend, timing, usage. What a reader wants
   * from a log is the same thing they came here for, and sending them to
   * "Advanced" on a settings page to find it is what makes a working feature
   * look absent.
   *
   * `null` means not following, which is the distinction the button draws. It is
   * not an empty log: a log that cannot be read arrives as `[]` from `loadLogs`,
   * so "stopped following" and "there is nothing logged" never render the same.
   */
  const [logs, setLogs] = useState<string[] | null>(null);
  const [history, setHistory] = useState<"export" | "import" | null>(null);
  const [archive, setArchive] = useState("");
  const [historyError, setHistoryError] = useState<string | null>(null);

  const openExport = async () => {
    setHistoryError(null);
    try {
      setArchive(await api.exportUsageHistory());
      setHistory("export");
    } catch (error) {
      setHistoryError(String(error));
    }
  };

  const openImport = () => {
    setArchive("");
    setHistoryError(null);
    setHistory("import");
  };

  const importArchive = async () => {
    if (!archive.trim()) return;
    const ok = await run(() => api.importUsageHistory(archive));
    if (ok) {
      setHistory(null);
      setArchive("");
    }
  };

  const clearClient = (client_id: string, client_name: string) =>
    setConfirm({
      title: t("activity.clear_client"),
      body: t("activity.clear_client_body", { name: client_name || client_id }),
      confirmLabel: t("activity.clear_client"),
      danger: true,
      onConfirm: () => void run(() => api.clearClientUsage(client_id)),
    });

  const loadLogs = useCallback(async () => {
    try {
      setLogs(await api.logs());
    } catch {
      setLogs([]);
    }
  }, []);

  useEffect(() => {
    if (logs === null) return;
    const timer = setInterval(loadLogs, 1500);
    return () => clearInterval(timer);
  }, [logs !== null, loadLogs]);

  const successRate =
    summary.requests > 0
      ? Math.round(((summary.requests - summary.failures) / summary.requests) * 1000) / 10
      : null;

  /*
   * Nothing has happened yet — no requests, no health, no spend. The page's
   * opening line already says so and says what to do; the three sections
   * below it used to repeat the sentence twice more and park an em dash under
   * the word "Spend". Each section is omitted when it has nothing of its own
   * to add, and "Reset" — a destructive action over numbers that do not
   * exist — goes with them.
   */
  const idle = summary.requests === 0 && recent.length === 0 && health.length === 0;

  const classifier = (() => {
    // Every side kind is Auto review traffic, whatever protocol it was asked in.
    const sides = summary.per_kind.filter((k) => isSideKind(k.kind as RequestKind));
    if (sides.length === 0) return undefined;
    return {
      kind: "auto_mode",
      requests: sides.reduce((n, k) => n + k.requests, 0),
      failures: sides.reduce((n, k) => n + k.failures, 0),
      avg_latency_ms: sides.length
        ? sides.reduce((n, k) => n + k.avg_latency_ms, 0) / sides.length
        : 0,
    };
  })();
  const open = recent.find((r) => r.id === openId) ?? null;

  const logPane = (
    <Section title={t("activity.log_section")} hint={t("activity.log_hint")}>
      {logs === null ? (
        <Button kind="ghost" onClick={loadLogs}>
          {t("activity.show_log")}
        </Button>
      ) : (
        <>
          <pre className="log-view">{logs.join("\n")}</pre>
          <Button kind="ghost" onClick={() => setLogs(null)}>
            {t("activity.stop_follow")}
          </Button>
        </>
      )}
    </Section>
  );

  const usagePane = (
    <>
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

      {!idle && (
        <Section title={t("activity.history")} hint={t("activity.history_hint")}>
          <div className="controls">
            <Button kind="ghost" onClick={() => void openExport()}>
              {t("activity.export_history")}
            </Button>
            <Button kind="ghost" onClick={openImport}>
              {t("activity.import_history")}
            </Button>
          </div>
          {historyError && <p className="field-error">{historyError}</p>}
          {summary.per_client.length > 0 && (
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th scope="col">{t("activity.client")}</th>
                    <th scope="col">{t("models.requests")}</th>
                    <th scope="col">{t("field.input")} / {t("field.output")}</th>
                    <th scope="col">{t("activity.spend")}</th>
                    <th scope="col">{t("common.actions")}</th>
                  </tr>
                </thead>
                <tbody>
                  {summary.per_client.map((client) => (
                    <tr key={client.client_id}>
                      <td>
                        <span className="mono">{client.client_name || client.client_id}</span>
                        <span className="muted"> · {client.client_id}</span>
                      </td>
                      <td className="mono">
                        {num(client.requests)}
                        {client.failures > 0 && (
                          <Badge tone="danger">{t("activity.n_failed", { n: client.failures })}</Badge>
                        )}
                      </td>
                      <td className="muted mono">
                        {num(client.input_tokens)} / {num(client.output_tokens)}
                      </td>
                      <td className="mono">{totalsText(client.cost)}</td>
                      <td>
                        <Button
                          kind="ghost"
                          ariaLabel={t("activity.clear_client_named", {
                            name: client.client_name || client.client_id,
                          })}
                          onClick={() => clearClient(client.client_id, client.client_name)}
                        >
                          {t("activity.clear_client")}
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Section>
      )}

      {!idle && (
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
                  <Button
                    kind="ghost"
                    // Every row's button is the same word, so the name carries
                    // the model it clears the health of. Without it, a list of
                    // twenty rows presents twenty buttons all called "Clear".
                    ariaLabel={t("activity.clear_health", { id: h.model_id })}
                    onClick={() => run(() => api.resetHealth(h.model_id))}
                  >
                    {t("common.clear")}
                  </Button>
                </div>
              ))}
            </div>
          )}
        </Section>
      )}

      {!idle && (
        <Section title={t("activity.spend")} hint={t("activity.spend_hint")}>
          <div className="stats">
            <div>
              <strong>{totalsText(summary.cost)}</strong>
              <span>{t("activity.estimated")}</span>
            </div>
          </div>
          {summary.per_model.some((m) => Object.keys(m.cost).length > 0) && (
            <div className="table-scroll">
              <table className="table">
              <thead>
                <tr>
                  <th scope="col">{t("field.model_name")}</th>
                  <th scope="col">{t("models.requests")}</th>
                  <th scope="col">{t("field.input")} / {t("field.output")}</th>
                  <th scope="col">{t("activity.spend")}</th>
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
            </div>
          )}
        </Section>
      )}
    </>
  );

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
        // Nothing to reset while nothing has been counted; a destructive
        // button that can only clear zeros teaches doubt about every other.
        actions={
          summary.requests === 0 ? undefined : (
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
          )
        }
      />

      {/*
        The drawer is deliberately outside the strip. It is opened from a row in
        the usage pane, so it belongs to the page rather than to whichever pane
        happens to be showing — inside, switching to the log would close the
        request the reader was reading.
      */}
      <Tabs
        value={tab}
        onChange={(next) => setTab(next as "usage" | "log")}
        ariaLabel={t("activity.tab_group")}
        tabs={[
          { value: "usage", label: t("activity.tab_usage"), content: usagePane },
          { value: "log", label: t("activity.tab_log"), content: logPane },
        ]}
      />

      {history && (
        <Drawer
          title={history === "export" ? t("activity.export_history") : t("activity.import_history")}
          onClose={() => setHistory(null)}
        >
          <p className="field-hint">
            {history === "export" ? t("activity.export_hint") : t("activity.import_hint")}
          </p>
          <textarea
            className="text-input"
            style={{ width: "100%", minHeight: 280, fontFamily: "var(--font-mono)" }}
            value={archive}
            readOnly={history === "export"}
            placeholder={history === "import" ? t("activity.import_placeholder") : undefined}
            onChange={(event) => setArchive(event.target.value)}
          />
          <div className="controls" style={{ marginTop: 12 }}>
            {history === "export" ? (
              <Button onClick={() => void api.copy(archive)}>{t("common.copy")}</Button>
            ) : (
              <Button onClick={() => void importArchive()} disabled={!archive.trim()}>
                {t("activity.import_history")}
              </Button>
            )}
          </div>
        </Drawer>
      )}

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
  const isAuto = isSideKind(record.kind);
  const { t } = useI18n();
  return (
    <button type="button" className="timeline-item" onClick={onOpen}>
      <span className="timeline-time">
        {new Date(record.at).toLocaleTimeString([], { hour12: false })}
      </span>
      <span className={`timeline-kind ${isAuto ? "auto-mode" : ""}`}>
        {isAuto ? t("kind.review") : t("kind.main")}
      </span>
      <span className="timeline-main-cell">
        {/*
         * The dot is the row's only sign of whether the request worked — the
         * row carries the model, the provider and the latency, and nothing
         * else said "this one failed". A dot is a colour, and a colour is
         * invisible to a screen reader, so the outcome is said in words here.
         */}
        <StatusDot
          tone={record.ok ? "ok" : "danger"}
          state={record.ok ? t("status.succeeded") : t("status.failed")}
        />
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
          <StatusDot
            tone={record.ok ? "ok" : "danger"}
            state={record.ok ? t("status.succeeded") : t("status.failed")}
          />
          {isSideKind(record.kind) ? t("activity.review") : t("activity.request")}
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
            isSideKind(record.kind) ? t("kind.check_kv") : t("kind.main_kv"),
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
