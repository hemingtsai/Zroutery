/** One row of the request timeline: outcome, the three model names, usage. */
import { costText, isSideKind, ms, tps, type RequestRecord } from "../../api";
import { useI18n } from "../../i18n";
import { StatusDot } from "../../components";

export function TimelineRow({
  record,
  onOpen,
}: {
  record: RequestRecord;
  onOpen: () => void;
}) {
  const isAuto = isSideKind(record.kind);
  const { t } = useI18n();
  const speed = tps(record);
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
        <span className="timeline-model">{record.model_name ?? record.resolved_model ?? record.requested_model}</span>
        {record.provider_name && <span className="timeline-provider">{record.provider_name}</span>}
        {record.usage_source && (
          <span className="timeline-provider">
            {t("activity.source_tag", {
              source: t(record.usage_source === "8788" ? "activity.source_standby" : "activity.source_local"),
            })}
          </span>
        )}
        {record.attempts > 1 && (
          <span className="timeline-provider">{t("activity.tries", { n: record.attempts })}</span>
        )}
      </span>
      {/*
       * First token and generation speed sit beside the latency because they
       * are timing figures too. Each renders only when the record can say it
       * honestly — the same convention as the cost cell: a figure the data
       * does not support renders nothing, not a dash or a zero (cc-switch's
       * rule that a missing first-token time means "no speed", never 0). Both
       * are ok-gated like cost: a failed row's meta reads its status code,
       * not performance numbers.
       */}
      <span className="timeline-meta">
        <span>{ms(record.latency_ms)}</span>
        {record.ok && record.stream && record.ttft_ms !== null && (
          <span>{t("activity.ttft", { v: ms(record.ttft_ms) })}</span>
        )}
        {record.ok && speed !== null && <span>{Math.round(speed)} tok/s</span>}
        {record.ok ? (
          record.cost ? <span>{costText(record.cost)}</span> : null
        ) : (
          <span>{record.status}</span>
        )}
      </span>
    </button>
  );
}
