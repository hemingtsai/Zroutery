/**
 * The usage-source selectors: what the local / standby choice means for
 * the records and the day ledger, and the cache-hit rate both KPI cells
 * and the charts quote.
 */
import type { RequestRecord, UsageDay, UsageSourceView, UsageSourcesView } from "../../api";

export function selectSourceSummary(sources: UsageSourcesView, source: "local" | "standby") {
  if (source === "standby" && sources.standby) return sources.standby.summary;
  return sources.local.summary;
}

export function taggedRecords(source: UsageSourceView, label: string) {
  return source.recent.map((record) => ({ ...record, usage_source: label }));
}

export function selectSourceRecords(
  sources: UsageSourcesView,
  source: "local" | "standby",
): RequestRecord[] {
  if (source === "standby" && sources.standby) return taggedRecords(sources.standby, "8788");
  return taggedRecords(sources.local, "local");
}

export function selectSourceDays(sources: UsageSourcesView, source: "local" | "standby"): UsageDay[] {
  if (source === "standby" && sources.standby) return sources.standby.days;
  return sources.local.days;
}

/**
 * Cache hit rate as a percent of the cache-inclusive input total: stats.rs
 * documents `cache_read_tokens` as a subset of `input_tokens`, never an
 * addition, so the denominator is the input figure itself. One decimal is
 * kept when it has one — a 99.6% hit must not round into a claimed-perfect
 * 100%, which cc-switch reserves for a true full read of the prompt.
 */
export function cacheHitRate(read: number, input: number): string {
  const pct = Math.round((read / input) * 1000) / 10;
  return `${Number.isInteger(pct) ? pct : pct.toFixed(1)}%`;
}
