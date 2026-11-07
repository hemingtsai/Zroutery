/** One request's whole story: the three model names, the outcome, the usage. */
import { useEffect, useState, type ReactNode } from "react";
import { api, costText, isSideKind, ms, totalsText, type DeclaredIdentity, type RequestRecord, type UsageAttempt } from "../../api";
import { useI18n } from "../../i18n";
import { Badge, Drawer, KeyValue, num, Section, StatusDot } from "../../components";
import { cacheHitRate } from "./select";

/** One request's whole story: the three model names, the outcome, the usage. */
export function RequestDrawer({ record, onClose }: { record: RequestRecord; onClose: () => void }) {
  const { t } = useI18n();
  const [attempts, setAttempts] = useState<UsageAttempt[] | null>(null);
  const [attemptError, setAttemptError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    setAttempts(null);
    setAttemptError(null);
    if (record.usage_source && record.usage_source !== "local") {
      setAttempts([]);
      return;
    }
    api.usageAttempts(record.id).then((rows) => {
      if (active) setAttempts(rows);
    }).catch((error) => {
      if (active) setAttemptError(String(error));
    });
    return () => { active = false; };
  }, [record.id, record.usage_source]);
  const declaration = (value: DeclaredIdentity | undefined) => {
    switch (value?.state) {
      case "declared": return value.value;
      case "auto_only": return t("activity.model_auto_only", { value: value.value });
      case "conflicting": return t("activity.model_conflict", { value: value.value.join(" / ") });
      default: return t("activity.model_missing");
    }
  };
  return (
    <Drawer
      showSaveStatus={false}
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
          ...(record.usage_source
            ? [[t("activity.usage_source"), record.usage_source] as [string, ReactNode]]
            : []),
          [
            t("activity.kind"),
            isSideKind(record.kind) ? t("kind.check_kv") : t("kind.main_kv"),
          ],
          [t("activity.inbound_api"), record.ingress],
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

      <Section title={t("activity.attempt_route")}>
        {attemptError ? <p className="field-error" role="status">{attemptError}</p> :
          attempts === null ? <p className="field-hint">{t("common.working")}</p> :
          attempts.length === 0 ? <p className="field-hint">{t("activity.attempt_unavailable")}</p> :
          attempts.map((attempt, index) => (
            <div className="import-section" key={attempt.id}>
              <h3>{index + 1}. {attempt.execution.provider_name} · {attempt.state}</h3>
              <KeyValue rows={[
                [t("activity.outbound_api"), attempt.execution.upstream_protocol ?? t("activity.model_missing")],
                [t("activity.planned_model"), attempt.execution.upstream_model],
                [t("activity.declared_model"), declaration(attempt.declared?.model)],
                [t("activity.declared_version"), declaration(attempt.declared?.model_version)],
                [t("accounting.usage_evidence"), attempt.usage_evidence
                  ? t(`accounting.usage_${attempt.usage_evidence}`)
                  : t("activity.model_missing")],
                [t("activity.cost"), attempt.cost ? costText(attempt.cost) : t("activity.unpriced")],
              ]} />
            </div>
          ))}
        {attempts && attempts.length > 0 && <p className="field-hint">{t("activity.declaration_hint")}</p>}
      </Section>

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
            /*
             * A ratio needs a prompt to be a ratio over: with zero input
             * tokens there is no rate, so the row stays out entirely instead
             * of printing 0% — a request without prompt tokens did not miss
             * the cache, it had nothing to hit.
             */
            ...(record.usage.input_tokens > 0
              ? ([
                  [
                    t("activity.cache_hit_rate"),
                    <span className="mono" title={t("activity.cache_hit_hint")}>
                      {cacheHitRate(record.usage.cache_read_tokens, record.usage.input_tokens)}
                    </span>,
                  ],
                ] as [ReactNode, ReactNode][])
              : []),
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
      {record.accounting && (
        <Section title={t("accounting.title")}>
          <KeyValue rows={[
            [t("accounting.state"), t(`accounting.${record.accounting.persistence}`)],
            [t("accounting.known_cost"), totalsText(record.accounting.known_cost)],
            [t("accounting.unknown_cost"), num(record.accounting.unknown_cost_attempts)],
            [t("accounting.unknown_usage"), num(record.accounting.unknown_usage_attempts)],
            [t("accounting.identity"), [record.account_id, record.inference_key_id].filter(Boolean).join(" / ") || t("common.dash")],
          ]} />
          <p className="field-hint">{t("accounting.no_replay")}</p>
        </Section>
      )}
    </Drawer>
  );
}
