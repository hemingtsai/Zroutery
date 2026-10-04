/**
 * Typed bridge to the Rust side. The shapes mirror the serde output of
 * `zroutery-core` and `src-tauri`, so field names stay snake_case.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ModelTier = "fast" | "standard" | "reasoning" | "frontier";
/** @deprecated Use ModelTier. */
export type ModelClass = ModelTier;
export type ProviderKind = "anthropic" | "openai_compatible";
export type NamingStyle = "internal" | "anthropic" | "openai";
export type RoutingStrategy =
  | "priority"
  | "weighted_random"
  | "round_robin"
  | "lowest_latency"
  | "balanced";

export const TIERS: ModelTier[] = ["fast", "standard", "reasoning", "frontier"];
/** @deprecated Use TIERS. */
export const CLASSES = TIERS;

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

export interface ModelCapabilities {
  vision: boolean;
  tools: boolean;
  thinking: boolean;
  structured_output: boolean;
  audio: boolean;
  video: boolean;
  files: boolean;
}

/**
 * A model is identified by its provider plus the upstream name. The id clients
 * use is derived from that pair by the backend and arrives in
 * `Snapshot.exposed_ids`, so this side never re-implements the rule.
 */
export interface ModelEntry {
  provider_id: string;
  upstream_model: string;
  tier: ModelTier | null;
  priority: number;
  weight: number;
  enabled: boolean;
  capabilities: ModelCapabilities;
  display_name: string | null;
  aliases: string[];
  max_output_tokens: number | null;
  /** Entered by hand, like the tier. Without it a request is logged unpriced. */
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

/** Spacing between two generated candidate priorities. */
export const CANDIDATE_PRIORITY_STEP = 10;

export function nextCandidatePriority(candidates: ClassifierCandidate[]): number {
  return (
    candidates.reduce((max, c) => Math.max(max, c.priority), 0) + CANDIDATE_PRIORITY_STEP
  );
}

/**
 * The pool order a priority strategy would try.
 *
 * Rust sorts the pool by `priority`, ascending, not by the order the entries
 * happen to sit in the array, so anything that displays or renumbers the pool
 * has to use the same rule.
 */
export function orderCandidatesByPriority<T extends { priority: number; model: string }>(
  candidates: T[],
): T[] {
  return [...candidates].sort(
    (a, b) => a.priority - b.priority || (a.model < b.model ? -1 : a.model > b.model ? 1 : 0),
  );
}

/**
 * Move one candidate `delta` places and renumber the whole pool.
 *
 * Swapping two array elements only changes what the page draws: the router
 * still sorts by `priority`, so a move that is not written into the numbers is
 * a move the gateway never makes. The pool is renumbered from the array order
 * the user is looking at, which is what "up" means; renumbering whatever order
 * the priorities already implied would leave them exactly as they were.
 */
export function moveCandidatePriority(
  candidates: ClassifierCandidate[],
  model: string,
  delta: number,
): ClassifierCandidate[] | null {
  const index = candidates.findIndex((c) => c.model === model);
  const swap = index + delta;
  if (index < 0 || swap < 0 || swap >= candidates.length) return null;
  const next = candidates.map((c) => ({ ...c }));
  const moved = next[index];
  next[index] = next[swap];
  next[swap] = moved;
  return next.map((candidate, place) => ({
    ...candidate,
    priority: CANDIDATE_PRIORITY_STEP * (place + 1),
  }));
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
  unknown_model_fallback: ModelTier | null;
  client_aliases: Record<string, ModelTier>;
  match_claude_names: boolean;
  scoring: ScoringConfig;
  elect_on_start: boolean;
  naming_style: NamingStyle;
}

/** One model's place in its tier, with the numbers that put it there. */
export interface Ranked {
  model_id: string;
  /** Lower is better. `null` when the model did not answer its probe. */
  score: number | null;
  latency_ms: number | null;
  price: Cost | null;
  note: string | null;
}

export interface TierElection {
  tier: ModelTier;
  /** Best first. */
  ranked: Ranked[];
  /** Whether price took part in the scoring. */
  priced: boolean;
  /** Why price was left out, when it was. */
  note: string | null;
}
/** @deprecated Use TierElection. */
export type ClassElection = TierElection;

export interface Election {
  decided_at: string;
  scoring: ScoringConfig;
  tiers: Partial<Record<ModelTier, TierElection>>;
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
  | { kind: "tier"; tier: ModelTier };

export type OnExceeded = { action: "reject" } | { action: "degrade"; to: ModelTier };

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
  /**
   * Whether this build contains a learning stack.
   *
   * Declared rather than discovered. To the webview a command that was never
   * registered and a command that failed are the same event, so the panel asks
   * first and a failed call stays a failure instead of being read as "this build
   * has no ML".
   */
  ml_available: boolean;
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
  models: { upstream_model: string; tier: ModelTier | null }[];
  is_current: boolean;
}

/** A CC Switch provider plus what an import would do with it. */
export interface CcProviderDraft {
  source_id: string;
  name: string;
  base_url: string;
  models: { upstream_model: string; tier: ModelTier | null }[];
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

/**
 * What the learned model is doing, mirroring `zroutery_core::ml::MlStatus`.
 *
 * Deliberately shallow about quality: this document reports what is installed
 * and what has been counted, not whether the model is any good. That claim
 * lives in a promotion decision, and a dashboard that implied otherwise would
 * be the exact confusion the ML boundary exists to prevent.
 */
export type PromotionVerdict = "promoted" | "rejected" | "blocked";

export interface PromotedModelStatus {
  model_id: string;
  commit_id: string;
  verdict: PromotionVerdict;
  dataset_fingerprint: string;
  fitted_partition_fingerprint: string;
  gate_config_identity: string;
  required_baseline: string;
  paired_requests: number;
  holdout_loss: number;
  promoted_at: number;
}

export interface PromotionHistoryEntry {
  action: "promote" | "rollback";
  commit_id: string;
  gate_identity: string;
  verdict: PromotionVerdict;
  at: number;
  note: string;
}

export interface BlindCandidate {
  /** The exposed model id, the key the router records outcomes under. */
  model_id: string;
  /** The provider that would serve it. */
  provider_id: string;
}

export interface MlStatus {
  routing_enabled: boolean;
  durable_state: boolean;
  traces_open: boolean;
  model_store_open: boolean;
  active: PromotedModelStatus | null;
  active_decision: PromotionDecision | null;
  history: PromotionHistoryEntry[];
  routing: {
    rankings: number;
    fallbacks: number;
    /** Deliberate alternatives tried with a model attached. */
    explorations: number;
    /**
     * Deliberate alternatives tried with *no* model attached, purely to gather
     * evidence about a candidate the current plan never reached.
     *
     * Counted apart from `explorations` because they are different claims. One is
     * the model choosing to try something else; the other is the router admitting
     * it knows nothing and going to find out. Showing them as one number would
     * let the second be read as the first.
     */
    blind_explorations: number;
    attached: boolean;
  };
  exploration_probability: number;
  exploration_seed: number;
  /**
   * Configured candidates the router holds no evidence about.
   *
   * `exploration_probability` ships at 0, and exploration returns `Exploit` at
   * zero before it draws, so a candidate the deterministic plan never picks can
   * never be reached. Adding a provider then looks like adding a capability and
   * is in fact inert, which is why this is reported rather than left for the
   * reader to derive from the probability above.
   */
  blind_candidates: BlindCandidate[];
  dataset: {
    ingested: number;
    samples: number;
    no_decision_time_input: number;
    rejected: number;
    evicted_by_count: number;
    evicted_by_age: number;
    faults: number;
  };
  traces: {
    appended: number;
    nothing: number;
    refused: number;
    io_errors: number;
  } | null;
  shadow: { enabled: boolean; decisions_recorded: number; faults: number };
  read_at: number;
}

/** One named gate criterion, with the number it was decided on. */
export interface PromotionCriterion {
  name: string;
  held: boolean;
  measured: number | null;
  threshold: number | null;
  reason: string;
}

export interface PromotionDecision {
  verdict: PromotionVerdict;
  candidate_commit: string;
  model_id: string;
  learning_event_count: number;
  dataset_fingerprint: string;
  fitted_partition_fingerprint: string;
  holdout_loss: number;
  gate_config_identity: string;
  criteria: PromotionCriterion[];
  baseline: string;
  paired_requests: number;
  decided_at: number;
  revision: string | null;
}

/** Why a shadow record contributed nothing to the quality measurement. */
export type ShadowGap =
  | "alternative_unmeasured"
  | "agreement"
  | "no_alternative"
  | "no_candidates";

/**
 * What the serving model would have done with the traffic already served.
 *
 * Mirrors `zroutery_core::ml::ShadowAnalysis`. The `Option`-valued means are the
 * point: a body with nothing measured reports `null` rather than zero, because a
 * zero improvement and an unmeasured one are different facts and averaging them
 * together is how a dashboard ends up claiming a model is worth exactly nothing
 * on the strength of never having been tested.
 */
export interface ShadowAnalysis {
  dataset_fingerprint: string;
  records: number;
  decision_records: number;
  prediction_only_records: number;
  agreements: number;
  disagreements: number;
  disagreements_measured: number;
  /** 0.0 to 1.0. */
  agreement_rate: number;
  measured_alternative_rate: number;
  alternative_selections: Record<string, number>;
  mean_estimated_utility_delta: number | null;
  mean_observed_utility_delta: number | null;
  mean_regret: number | null;
  harmful_alternatives: number;
  helpful_alternatives: number;
  gaps: Record<ShadowGap, number>;
  produced_at: number;
}

/**
 * The result of asking for a replay.
 *
 * `analysis` and `reason` are separate on purpose: "no model is attached" and
 * "the model disagreed with production on nothing" are different answers, and an
 * operator who reads one as the other draws the wrong conclusion from both.
 */
export interface ShadowAnalysisStatus {
  /** The bound that was applied, not a claim about all the history there is. */
  traces_read: number;
  commit_id: string | null;
  reason: string | null;
  analysis: ShadowAnalysis | null;
}

/** What re-reading the durable pointer did, and the status that resulted. */
export interface MlRollback {
  outcome: ReloadOutcome;
  status: MlStatus;
}

export interface ReloadOutcome {
  attached: boolean;
  commit_id: string | null;
  error: string | null;
}

export const api = {
  snapshot: () => invoke<Snapshot>("get_snapshot"),
  activity: () => invoke<Activity>("get_activity"),
  logs: () => invoke<string[]>("get_logs"),
  /**
   * Live counters and the stored pointer. Read-only.
   *
   * Call only when `snapshot.ml_available` is true. A missing command and a
   * failing one are indistinguishable from here, so the absence is declared in
   * the snapshot rather than discovered by catching this call.
   */
  mlStatus: () => invoke<MlStatus>("get_ml_status"),
  /** Replay the serving model over recorded history. Bounded by `limit`. */
  mlShadow: (limit: number) => invoke<ShadowAnalysisStatus>("get_ml_shadow", { limit }),
  /**
   * Withdraw the serving model and return to the previously promoted one.
   *
   * Returns the outcome together with the freshly read status, so the dashboard
   * cannot render a state the process is not in.
   */
  rollbackMlModel: () => invoke<MlRollback>("rollback_ml_model"),
  saveConfig: (config: AppConfig) => invoke<Snapshot>("save_config", { config }),
  setKey: (provider_id: string, api_key: string) =>
    invoke<Snapshot>("set_provider_key", { providerId: provider_id, apiKey: api_key }),
  clearKey: (provider_id: string) =>
    invoke<Snapshot>("clear_provider_key", { providerId: provider_id }),
  /**
   * Remove a provider, its models and the credential only it used.
   *
   * One backend round trip: the key reference is read before the
   * configuration changes, so a custom `key_ref` is cleared rather than
   * guessed, and a reference another provider shares is kept.
   */
  removeProvider: (provider_id: string) =>
    invoke<Snapshot>("remove_provider", { providerId: provider_id }),
  fetchModels: (provider: Provider) =>
    invoke<DiscoveredModel[]>("fetch_provider_models", { provider }),
  refreshBalance: (provider_id: string) =>
    invoke<Snapshot>("refresh_balance", { providerId: provider_id }),
  refreshBalances: () => invoke<Snapshot>("refresh_balances"),
  /** Probes every tier member, so it costs one tiny request each. */
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
 * Tray actions that change the gateway state. The tray has no snapshot to
 * return and the window is very likely hidden when it is used, so the new
 * state is announced and every window re-reads it; without this the page
 * keeps drawing the state it loaded when it first opened. The name mirrors
 * `GATEWAY_STATE_EVENT` in `src-tauri/src/tray.rs`.
 */
export const GATEWAY_STATE_EVENT = "zroutery://gateway-state-changed";

export function onGatewayStateChanged(handler: () => void): Promise<UnlistenFn> {
  return listen(GATEWAY_STATE_EVENT, () => handler());
}

/**
 * Which gateway command the *current* state calls for.
 *
 * Reading this from a rendered snapshot misreads the state whenever the
 * gateway was started or stopped elsewhere — a stopped gateway would be
 * "stopped" again instead of started. Callers load the server status first and
 * ask this what to do with it.
 */
export function gatewayAction(status: Pick<ServerStatus, "running">): "start" | "stop" {
  return status.running ? "stop" : "start";
}

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

export function scopeLabel(scope: BudgetScope, style: NamingStyle = "internal"): string {
  switch (scope.kind) {
    case "global":
      return "everything";
    case "provider":
      return `provider ${scope.id}`;
    case "tier":
      return virtualId(scope.tier, style);
  }
}

export function periodLabel(period: BudgetPeriod): string {
  return period === "day" ? "today" : "this month";
}

/** The virtual model id a tier is exposed as. */
export function virtualId(tier: ModelTier, style: NamingStyle = "internal"): string {
  const map: Record<NamingStyle, Record<ModelTier, string>> = {
    internal: { fast: "fast-class", standard: "standard-class", reasoning: "reasoning-class", frontier: "frontier-class" },
    anthropic: { fast: "haiku-class", standard: "sonnet-class", reasoning: "opus-class", frontier: "fable-class" },
    openai: { fast: "luna-class", standard: "terra-class", reasoning: "sol-class", frontier: "astra-class" },
  };
  return map[style][tier];
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

/** Members of a tier in the order the router would try them. */
export function tierMembers(
  rows: ModelRow[],
  providers: Provider[],
  tier: ModelTier,
): ModelRow[] {
  return rows
    .filter((r) => r.model.tier === tier && r.model.enabled)
    .filter((r) => providers.find((p) => p.id === r.model.provider_id)?.enabled)
    .sort((a, b) => a.model.priority - b.model.priority || a.id.localeCompare(b.id));
}
/** @deprecated Use tierMembers. */
export const classMembers = tierMembers;

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
