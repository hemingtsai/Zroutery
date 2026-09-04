/**
 * Typed bridge to the Rust side. The shapes mirror the serde output of
 * `zroutery-core` and `src-tauri`, so field names stay snake_case.
 */
import { invoke } from "@tauri-apps/api/core";

export type ModelClass = "opus" | "sonnet" | "haiku";
export type ProviderKind = "anthropic" | "openai_compatible";
export type RoutingStrategy =
  | "priority"
  | "weighted_random"
  | "round_robin"
  | "lowest_latency"
  | "balanced";

export const CLASSES: ModelClass[] = ["opus", "sonnet", "haiku"];

/** What a provider charges for one model, per million tokens. */
export interface Pricing {
  currency: string;
  input_per_mtok: number;
  output_per_mtok: number;
  cache_read_per_mtok: number | null;
  cache_write_per_mtok: number | null;
}

export interface Cost {
  currency: string;
  amount: number;
}

/** Spend per currency; never summed across currencies. */
export type CostTotals = Record<string, number>;

export type BalancePreset =
  | "none"
  | "deep_seek"
  | "moonshot"
  | "silicon_flow"
  | "open_router"
  | "sub2api"
  | "custom";

export const BALANCE_PRESETS: { id: BalancePreset; label: string; hint: string }[] = [
  { id: "none", label: "Not supported", hint: "OpenAI and Anthropic publish no balance" },
  { id: "deep_seek", label: "DeepSeek", hint: "/user/balance" },
  { id: "moonshot", label: "Moonshot", hint: "/users/me/balance" },
  { id: "silicon_flow", label: "SiliconFlow", hint: "/user/info" },
  { id: "open_router", label: "OpenRouter", hint: "/credits" },
  {
    id: "sub2api",
    label: "Sub2API relay",
    hint: "/v1/usage — wallet, key quota or subscription",
  },
  { id: "custom", label: "Custom endpoint", hint: "your own path and JSON pointers" },
];

export interface BalanceProbe {
  path: string;
  remaining_pointer: string | null;
  total_pointer: string | null;
  used_pointer: string | null;
  currency_pointer: string | null;
  currency: string | null;
}

export interface BalanceConfig {
  preset: BalancePreset;
  custom: BalanceProbe | null;
}

export interface Balance {
  currency: string;
  remaining: number | null;
  total: number | null;
  used: number | null;
}

/** The last answer from a provider's balance endpoint. */
export interface BalanceStatus {
  checked_at: string;
  balance: Balance | null;
  error: string | null;
}

export interface ProviderQuirks {
  use_max_completion_tokens: boolean;
  drop_temperature: boolean;
  drop_top_p: boolean;
  drop_stop: boolean;
  stream_usage: boolean;
  system_as_developer: boolean;
  send_reasoning_effort: boolean;
}

export interface Provider {
  id: string;
  name: string;
  kind: ProviderKind;
  base_url: string;
  key_ref: string;
  extra_headers: Record<string, string>;
  /** Whether to impersonate Claude Code client (User-Agent, x-app, anthropic-beta headers
   * and system prompt identity line). */
  impersonate_claude_code: boolean;
  /** Also send the key as `Authorization: Bearer`, for Anthropic relays that
   * read the Bearer header instead of `x-api-key`. */
  bearer_auth: boolean;
  enabled: boolean;
  timeout_secs: number;
  connect_timeout_secs: number;
  anthropic_version: string | null;
  quirks: ProviderQuirks;
  balance: BalanceConfig;
}

/**
 * A model is identified by its provider plus the upstream name. The id clients
 * use is derived from that pair by the backend and arrives in
 * `Snapshot.exposed_ids`, so this side never re-implements the rule.
 */
export interface ModelEntry {
  provider_id: string;
  upstream_model: string;
  class: ModelClass | null;
  priority: number;
  weight: number;
  enabled: boolean;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_thinking: boolean;
  display_name: string | null;
  aliases: string[];
  max_output_tokens: number | null;
  /** Entered by hand, like the class. Without it a request is logged unpriced. */
  pricing: Pricing | null;
}

/** How `balanced` weighs the two axes, and the request it prices them against. */
export interface ScoringConfig {
  price_weight: number;
  latency_weight: number;
  reference_input_tokens: number;
  reference_output_tokens: number;
}

/** A structural fingerprint of one classifier stage. All present fields must match. */
export interface ClassifierSignature {
  name: string;
  max_tokens: number | null;
  temperature: number | null;
  stop_sequence: string | null;
  system_contains: string[];
}

export interface BuiltinDetectors {
  anthropic_beta: boolean;
  xml_classifier_signature: boolean;
  model_1m_signature: boolean;
}

export interface DetectionConfig {
  enabled: boolean;
  minimum_confidence: number;
  builtins: BuiltinDetectors;
  /** Extra fingerprints on top of the built-ins. */
  signatures: ClassifierSignature[];
}

/** One member of the classifier pool: an existing model id, plus its place. */
export interface ClassifierCandidate {
  /** Exposed id (or alias) of an existing model entry. */
  model: string;
  priority: number;
  enabled: boolean;
}

/** Routing policy for Auto Mode classifier side queries. */
export interface ClassifierConfig {
  enabled: boolean;
  strategy: RoutingStrategy;
  failover: boolean;
  max_attempts: number;
  candidates: ClassifierCandidate[];
  detection: DetectionConfig;
}

/** How the desktop app behaves as a resident process. */
export interface WindowBehavior {
  /** Launch Zroutery at OS login. */
  launch_on_login: boolean;
  /** Start without showing the main window; the tray is the only presence. */
  silent_start: boolean;
  /** Closing the window keeps the process and the gateway alive in the tray. */
  keep_in_tray: boolean;
}

/** Vision fallback: describing images for models that cannot see them. */
export interface VisionConfig {
  enabled: boolean;
  /** Exposed id of an existing model that can describe images. */
  model: string | null;
  /** What replaces an image when no description is possible. */
  placeholder: string;
}

/** What a request was for: the main conversation or a side query. */
export type RequestKind = "main" | "auto_mode";

export interface RoutingConfig {
  strategy: RoutingStrategy;
  failover: boolean;
  max_attempts: number;
  break_after_failures: number;
  cooldown_secs: number;
  unknown_model_fallback: ModelClass | null;
  client_aliases: Record<string, ModelClass>;
  match_claude_names: boolean;
  scoring: ScoringConfig;
  elect_on_start: boolean;
}

/** One model's place in its class, with the numbers that put it there. */
export interface Ranked {
  model_id: string;
  /** Lower is better. `null` when the model did not answer its probe. */
  score: number | null;
  latency_ms: number | null;
  price: Cost | null;
  note: string | null;
}

export interface ClassElection {
  class: ModelClass;
  /** Best first. */
  ranked: Ranked[];
  /** Whether price took part in the scoring. */
  priced: boolean;
  /** Why price was left out, when it was. */
  note: string | null;
}

export interface Election {
  decided_at: string;
  scoring: ScoringConfig;
  classes: Partial<Record<ModelClass, ClassElection>>;
}

export interface ServerConfig {
  host: string;
  port: number;
  require_auth: boolean;
  /** Always empty in snapshots; sending it back empty keeps the stored token. */
  auth_token: string;
  autostart: boolean;
  allow_cors: boolean;
  cors_origins: string[];
  max_body_mib: number;
  log_limit: number;
  /** Bypass the system proxy for upstream requests. */
  bypass_proxy: boolean;
}

export type BudgetPeriod = "day" | "month";

/** What a budget covers. Serialised with a `kind` tag by the backend. */
export type BudgetScope =
  | { kind: "global" }
  | { kind: "provider"; id: string }
  | { kind: "class"; class: ModelClass };

export type OnExceeded = { action: "reject" } | { action: "degrade"; to: ModelClass };

export interface Budget {
  id: string;
  scope: BudgetScope;
  period: BudgetPeriod;
  limit: Cost;
  on_exceeded: OnExceeded;
  enabled: boolean;
}

/** A budget with the spend counted against it. */
export interface BudgetStatus {
  budget: Budget;
  spent: Cost;
  /** Over 1.0 means the limit has been passed. */
  used: number;
}

export interface AppConfig {
  server: ServerConfig;
  routing: RoutingConfig;
  /** Auto Mode classifier routing; orthogonal to `routing`. */
  classifier: ClassifierConfig;
  /** Desktop application lifecycle. */
  window: WindowBehavior;
  /** Vision fallback for models that cannot see. */
  vision: VisionConfig;
  providers: Provider[];
  models: ModelEntry[];
  budgets: Budget[];
}

export interface ConfigIssue {
  severity: "error" | "warning";
  code: string;
  message: string;
  subject: string | null;
}

export interface ServerStatus {
  running: boolean;
  address: string | null;
  base_url: string | null;
  host: string;
  port: number;
  require_auth: boolean;
  /** `zr-…abcd`. The real token only arrives through `revealToken`. */
  token_hint: string;
  exposed: boolean;
}

export interface Usage {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
}

export interface RequestRecord {
  id: string;
  at: string;
  ingress: string;
  /** `main` or `auto_mode`. */
  kind: RequestKind;
  requested_model: string;
  resolved_model: string | null;
  provider_name: string | null;
  stream: boolean;
  status: number;
  ok: boolean;
  error: string | null;
  latency_ms: number;
  ttft_ms: number | null;
  usage: Usage;
  /** `null` means unpriced, not free. */
  cost: Cost | null;
  attempts: number;
}

export interface ModelHealth {
  model_id: string;
  consecutive_failures: number;
  total_success: number;
  total_failure: number;
  avg_latency_ms: number;
  cooldown_remaining_secs: number;
  last_error: string | null;
}

export interface ModelTotals {
  model_id: string;
  requests: number;
  failures: number;
  input_tokens: number;
  output_tokens: number;
  reasoning_tokens: number;
  cached_tokens: number;
  cost: CostTotals;
  avg_latency_ms: number;
}

/** Counters for one request kind, so classifier traffic is visible as itself. */
export interface KindTotals {
  kind: string;
  requests: number;
  failures: number;
  input_tokens: number;
  output_tokens: number;
  avg_latency_ms: number;
}

export interface StatsSummary {
  since: string;
  requests: number;
  failures: number;
  input_tokens: number;
  output_tokens: number;
  cost: CostTotals;
  per_model: ModelTotals[];
  per_kind: KindTotals[];
}

export interface Snapshot {
  config: AppConfig;
  /** Exposed id per entry of `config.models`, same order. */
  exposed_ids: string[];
  issues: ConfigIssue[];
  blocking: boolean;
  server: ServerStatus;
  keys: Record<string, boolean>;
  health: ModelHealth[];
  summary: StatsSummary;
  recent: RequestRecord[];
  warning: string | null;
  config_path: string;
  version: string;
  /** provider id -> last balance check. */
  balances: Record<string, BalanceStatus>;
  /** The last election, when one has been held this run. */
  election: Election | null;
  /** Every budget with what has been spent against it. */
  budgets: BudgetStatus[];
}

/** One entry of a provider's catalogue, with prices when it publishes them. */
export interface DiscoveredModel {
  id: string;
  pricing: Pricing | null;
}

/** A CC Switch provider reduced to what an import decision needs. */
export interface CcProvider {
  source_id: string;
  name: string;
  base_url: string;
  /** Present in the payload, never rendered. */
  api_key: string | null;
  models: { upstream_model: string; class: ModelClass | null }[];
  is_current: boolean;
}

/** A CC Switch provider plus what an import would do with it. */
export interface CcProviderDraft {
  source_id: string;
  name: string;
  base_url: string;
  api_key: string | null;
  models: { upstream_model: string; class: ModelClass | null }[];
  is_current: boolean;
  /** The Zroutery provider id this would get. */
  target_id: string;
  /** A provider with the same endpoint already exists. */
  already_imported: boolean;
}

export interface CcSwitchPreview {
  source: string;
  providers: CcProviderDraft[];
}

/** The counters the Activity tab polls for, without the configuration. */
export interface Activity {
  health: ModelHealth[];
  summary: StatsSummary;
  recent: RequestRecord[];
}

export const api = {
  snapshot: () => invoke<Snapshot>("get_snapshot"),
  activity: () => invoke<Activity>("get_activity"),
  logs: () => invoke<string[]>("get_logs"),
  saveConfig: (config: AppConfig) => invoke<Snapshot>("save_config", { config }),
  setKey: (provider_id: string, api_key: string) =>
    invoke<Snapshot>("set_provider_key", { providerId: provider_id, apiKey: api_key }),
  clearKey: (provider_id: string) =>
    invoke<Snapshot>("clear_provider_key", { providerId: provider_id }),
  fetchModels: (provider: Provider) =>
    invoke<DiscoveredModel[]>("fetch_provider_models", { provider }),
  refreshBalance: (provider_id: string) =>
    invoke<Snapshot>("refresh_balance", { providerId: provider_id }),
  refreshBalances: () => invoke<Snapshot>("refresh_balances"),
  /** Probes every class member, so it costs one tiny request each. */
  runElection: () => invoke<Snapshot>("run_election"),
  start: () => invoke<Snapshot>("start_proxy"),
  stop: () => invoke<Snapshot>("stop_proxy"),
  regenerateToken: () => invoke<Snapshot>("regenerate_token"),
  /** Explicit user action; everything else works off `token_hint`. */
  revealToken: () => invoke<string>("reveal_token"),
  /** Copies in Rust so the token never enters this side. */
  copyToken: () => invoke<void>("copy_token"),
  clearStats: () => invoke<Snapshot>("clear_stats"),
  resetHealth: (model_id: string) =>
    invoke<Snapshot>("reset_model_health", { modelId: model_id }),
  copy: (text: string) => invoke<void>("copy_text", { text }),
  hide: () => invoke<void>("hide_window"),
  quit: () => invoke<void>("quit_app"),
  /** What CC Switch has on this machine, without importing anything. */
  ccswitchPreview: () => invoke<CcSwitchPreview>("ccswitch_preview"),
  /** Import the selected providers; ids are CC Switch's own provider ids. */
  ccswitchImport: (ids: string[]) =>
    invoke<Snapshot>("ccswitch_import", { ids }),
};

/**
 * Readable text for anything thrown across the IPC boundary.
 *
 * Tauri rejects with a string, but a render bug can throw an Error or a plain
 * object, and `String(value)` turns those into `[object Object]`.
 */
export function errorText(value: unknown): string {
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.message;
  if (value && typeof value === "object") {
    const maybe = value as { message?: unknown; error?: unknown };
    if (typeof maybe.message === "string") return maybe.message;
    if (typeof maybe.error === "string") return maybe.error;
    try {
      return JSON.stringify(value);
    } catch {
      return "unknown error";
    }
  }
  return String(value ?? "unknown error");
}

/**
 * Money for humans: enough decimals to see a single cheap request, not so many
 * that a total becomes unreadable.
 */
export function money(currency: string, amount: number): string {
  const digits = Math.abs(amount) > 0 && Math.abs(amount) < 0.01 ? 6 : 2;
  return `${amount.toFixed(digits)} ${currency}`;
}

export function costText(cost: Cost | null): string {
  return cost ? money(cost.currency, cost.amount) : "—";
}

export function totalsText(totals: CostTotals): string {
  const entries = Object.entries(totals);
  if (entries.length === 0) return "—";
  return entries.map(([currency, amount]) => money(currency, amount)).join(" + ");
}

/** `2.75 in / 11.00 out` per million tokens. */
export function priceText(pricing: Pricing | null): string {
  if (!pricing) return "—";
  return `${pricing.input_per_mtok} / ${pricing.output_per_mtok} ${pricing.currency}`;
}

export function emptyPricing(currency = "USD"): Pricing {
  return {
    currency,
    input_per_mtok: 0,
    output_per_mtok: 0,
    cache_read_per_mtok: null,
    cache_write_per_mtok: null,
  };
}

export function defaultProbe(): BalanceProbe {
  return {
    path: "/user/balance",
    remaining_pointer: "/balance",
    total_pointer: null,
    used_pointer: null,
    currency_pointer: null,
    currency: null,
  };
}

export function scopeLabel(scope: BudgetScope): string {
  switch (scope.kind) {
    case "global":
      return "everything";
    case "provider":
      return `provider ${scope.id}`;
    case "class":
      return `${scope.class}-class`;
  }
}

export function periodLabel(period: BudgetPeriod): string {
  return period === "day" ? "today" : "this month";
}

/** The virtual model id a class is exposed as. */
export function virtualId(cls: ModelClass): string {
  return `${cls}-class`;
}

/** A configured model together with the id the backend exposes it as. */
export interface ModelRow {
  model: ModelEntry;
  id: string;
  /** Index into `config.models`, used when editing. */
  index: number;
}

export function modelRows(snapshot: Snapshot): ModelRow[] {
  return snapshot.config.models.map((model, index) => ({
    model,
    id: snapshot.exposed_ids[index] ?? `${model.provider_id}-${model.upstream_model}`,
    index,
  }));
}

/** Members of a class in the order the router would try them. */
export function classMembers(
  rows: ModelRow[],
  providers: Provider[],
  cls: ModelClass,
): ModelRow[] {
  return rows
    .filter((r) => r.model.class === cls && r.model.enabled)
    .filter((r) => providers.find((p) => p.id === r.model.provider_id)?.enabled)
    .sort((a, b) => a.model.priority - b.model.priority || a.id.localeCompare(b.id));
}

/**
 * Preview of the id a model will get. Display only: the backend derives the real
 * one, and `Snapshot.exposed_ids` is what gets rendered afterwards.
 */
export function previewId(providerId: string, upstreamModel: string): string {
  return `${providerId.trim()}-${upstreamModel.trim()}`.replace(/[^A-Za-z0-9._-]/g, "-");
}

export function slugify(value: string): string {
  return (
    value
      .toLowerCase()
      .replace(/[^a-z0-9._-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "provider"
  );
}
