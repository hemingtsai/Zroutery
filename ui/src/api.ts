/**
 * Typed bridge to the Rust side. The shapes mirror the serde output of
 * `zroutery-core` and `src-tauri`, so field names stay snake_case.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { TKey } from "./i18n";

export type ModelTier = "fast" | "standard" | "reasoning" | "frontier";
/** @deprecated Use ModelTier. */
export type ModelClass = ModelTier;
/** Mirrors the serde names of `ProviderKind` in `zroutery-core::config`. The
 * two `opencode_*` kinds are gateway targets rather than wire dialects: both
 * speak the OpenAI dialect and both lock their client identity to
 * `opencode_cli`. */
export type ProviderKind =
  | "anthropic"
  | "openai_compatible"
  | "opencode_zen_free"
  | "opencode_console";
/** Mirrors `ProviderClientProfile` in `zroutery-core::config`: the client
 * identity presented upstream, and which of that client's own headers are
 * forwarded with it. */
export type ProviderClientProfile =
  | "auto"
  | "native"
  | "claude_code"
  | "codex"
  | "opencode_cli";
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

/**
 * The balance probes a provider can be asked at.
 *
 * The labels are translation *keys*, not words. They used to be English
 * literals, which made the whole "Probe" dropdown English inside a Chinese
 * interface — the one dropdown whose options are all descriptions rather than
 * product names.
 *
 * `hint` says what the probe asks for, and it used to have no rendering path
 * at all: the data was there and nothing showed it. It is a key for the same
 * reason, and the page renders the chosen one.
 */
export const BALANCE_PRESETS: {
  id: BalancePreset;
  labelKey: string;
  hintKey: string;
}[] = [
  {
    id: "none",
    labelKey: "balance_preset.none",
    hintKey: "balance_hint.none",
  },
  { id: "deep_seek", labelKey: "balance_preset.deep_seek", hintKey: "balance_hint.deep_seek" },
  { id: "moonshot", labelKey: "balance_preset.moonshot", hintKey: "balance_hint.moonshot" },
  {
    id: "silicon_flow",
    labelKey: "balance_preset.silicon_flow",
    hintKey: "balance_hint.silicon_flow",
  },
  {
    id: "open_router",
    labelKey: "balance_preset.open_router",
    hintKey: "balance_hint.open_router",
  },
  {
    id: "sub2api",
    labelKey: "balance_preset.sub2api",
    hintKey: "balance_hint.sub2api",
  },
  {
    id: "custom",
    labelKey: "balance_preset.custom",
    hintKey: "balance_hint.custom",
  },
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
  /** The identity presented upstream. `claude_code` and `codex` also forward
   * that client's own headers; `native` forwards none. */
  client_profile: ProviderClientProfile;
  /** Also send the key as `Authorization: Bearer`, for Anthropic relays that
   * read the Bearer header instead of `x-api-key`. */
  bearer_auth: boolean;
  enabled: boolean;
  timeout_secs: number;
  connect_timeout_secs: number;
  anthropic_version: string | null;
  quirks: ProviderQuirks;
  balance: BalanceConfig;
  /** Accounts this provider hosts, when it hosts any. */
  accounts?: Account[];
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
  /**
   * Whether this build contains account maintenance and browser check-in.
   *
   * Asked rather than discovered, for the same reason as `ml_available`: a
   * command that was never registered looks exactly like one that failed, and a
   * panel that probes for it would report "no account maintenance" on a build
   * where the bridge is merely broken.
   */
  account_maintenance_available: boolean;
  /**
   * Whether this build contains the account subsystem at all.
   *
   * Distinct from `account_maintenance_available`, and asked for the same reason:
   * `account-maint` implies `account`, so a build can have one without the other,
   * and the two answer different questions — whether declared accounts can be
   * probed at all, or whether they can be checked in through a browser. Reading
   * one for the other offers a control the build cannot serve.
   */
  account_available: boolean;
  /**
   * Whether this build can check an account in over the panel's own HTTP API.
   *
   * A third flag, not a combination of the two above: it needs the report views
   * of `account-maint` *and* the panel adapter of `newapi`, and a build can hold
   * one without the other. Offering the HTTP check-in from the browser flag
   * alone would call a command that does not exist in an `account-maint`-only
   * build.
   */
  api_checkin_available: boolean;
  /**
   * Whether this build can actually talk to OpenCode's gateway.
   *
   * The two OpenCode provider kinds and the `opencode_cli` client profile are
   * configuration rather than code, so they survive every build — but the
   * request shaping that makes OpenCode's gateway accept a request does not,
   * and the gateway checks a fixed fingerprint and rejects anything else. A
   * build without the feature can therefore be *told* to use them and will have
   * every request refused, so the kinds and the profile are offered only where
   * they work.
   */
  opencode_available: boolean;
  /** What each declared account's check-in is doing. */
  checkin?: CheckinView[];
  /** Observed resource events, newest first. */
  account_events?: AccountEvent[];
  /**
   * Per-account balances.
   *
   * One entry per declared account of every enabled provider. The backend emits
   * them all rather than only the multi-account ones: with one account the
   * provider-level figure may not even exist (most relays publish no balance
   * endpoint), and hiding the account's own reading on the assumption that a
   * provider total covers it is exactly how a probed account used to be visible
   * nowhere.
   */
  account_balances?: AccountBalance[];
}

/**
 * One account's check-in, as the dashboard renders it.
 *
 * `phase` is a closed vocabulary rather than a boolean, because "never tried",
 * "waiting for you", "already done today" and "failed" are four different things
 * that a flag collapses into two. `reward_amount` is present only when an
 * observation established one; an absent reward means nothing was observed, which
 * is not the same as a reward of zero.
 */
/**
 * One account hosted by a provider.
 *
 * A declaration, not an observation: it says the account exists and where its
 * credential lives, never what state it is in. Quota, balance and the last
 * check-in are readings that expire, so they live in the snapshot's `checkin`
 * array rather than here — writing them into the configuration would turn a
 * number that was true once into configuration that still reads as authoritative.
 */
export interface Account {
  account_id: string;
  key_ref: string;
  enabled: boolean;
  maintenance: Maintenance;
}

/**
 * What the user asked to be done about an account's resources.
 *
 * No credential field exists here, by design. Check-in runs in a real browser
 * which holds its own session in its own profile, so there is nothing for a
 * password in a user-editable file to accomplish. `checkin_path` is required for
 * browser check-in and deliberately has no default: relays host their console at
 * different paths, and a wrong default would navigate a real browser somewhere
 * useless while reporting progress.
 */
export interface Maintenance {
  checkin_enabled: boolean;
  checkin_interval_secs: number | null;
  checkin_path: string | null;
  /**
   * CSS selector for the check-in control on that page.
   *
   * Required alongside `checkin_path`. The browser activates the panel's own
   * button rather than posting the request itself, so this is what the operation
   * clicks — and it has no default because the markup of any particular relay's
   * console is not something this file can know.
   */
  checkin_trigger_selector: string | null;
  login_path: string | null;
  browser_executable: string;
}

export interface CheckinView {
  provider_id: string;
  account_id: string;
  /**
   * `idle` | `running` | `waiting_for_user` | `succeeded` |
   * `already_completed` | `not_supported` | `failed` | `cancelled`.
   */
  phase: string;
  /** Whether a browser window is open for this account right now. */
  browser_held: boolean;
  /** Whether the user is being asked to finish a challenge in that window. */
  awaiting_user: boolean;
  failure: string | null;
  reward_amount: number | null;
  reward_unit: string | null;
  /**
   * Where the reward figure came from: `provider_response`, `provider_record`,
   * `event_content` or `balance_delta`.
   *
   * Rendered alongside the figure, because a number recovered from provider log
   * text is weaker evidence than one the provider reported in a structured field,
   * and a panel that shows them identically is claiming more than it knows.
   */
  reward_source: string | null;
  last_completed_at: number | null;
  /**
   * Why the scheduler did or did not act, and when it next will.
   *
   * `decision` is one of `disabled`, `not_applicable`, `needs_first_observation`,
   * `no_policy`, `not_due` or `due`. Each is a different sentence for the user, and
   * collapsing them would leave somebody staring at an account that looks broken
   * because they never turned anything on.
   *
   * `next_due_at` is null whenever the decision names no time, which is not the same
   * as zero: an account that has never run has no future due time, and rendering 0
   * as a date would claim 1970.
   */
  schedule: CheckinScheduleView;
}

/** The scheduler's answer for one account. */
export interface CheckinScheduleView {
  decision: string;
  next_due_at: number | null;
  last_settled_at: number | null;
}
/**
 * One observed thing that happened to an account's resources.
 *
 * `effect` is `credited`, `debited`, `no_change` or `unobserved`, and the last two
 * are different facts rather than synonyms: one says the provider established that
 * nothing moved, the other says nothing established either way.
 *
 * `source` is where the figure came from — `operation_response`, `account_state` or
 * `account_log`. The log is the weakest of the three, because its text is localised
 * and forks reword it, so a figure read from it is a fallback rather than a
 * confirmation.
 */
export interface AccountEvent {
  provider_id: string;
  account_id: string;
  observed_at: number;
  kind: string;
  effect: string;
  /** Present only when an amount is established. */
  amount: number | null;
  unit: string | null;
  source: string;
  description: string;
}

/**
 * What a reconcile did with one account, mirroring
 * `zroutery_core::account::AccountSyncOutcome`.
 *
 * Serde's external tagging, so the two carrying variants arrive as objects and
 * the two bare ones as strings. The shape is load-bearing rather than decorative:
 * it is what keeps `failed` from rendering as an account with no reason, which is
 * the mistake a panel is most likely to make with this document.
 */
export type AccountSyncOutcome =
  | "refreshed"
  /** The provider answered, but declines to report usage or quota. */
  | { partially_reported: { missing: string[] } }
  /** No backend in this build speaks to this provider's account API. */
  | "unsupported"
  | { failed: { reason: string } };

/**
 * The result of probing the declared accounts, mirroring
 * `zroutery_core::account::ReconcileReport`.
 *
 * One entry per declared *enabled* account, in probe order, as
 * `(provider_id, account_id, outcome)`. The pair is carried rather than looked up
 * because the account store is keyed on it.
 *
 * Absence is itself a reading and the two halves must not be merged: an account
 * missing from this report is one that is disabled or not declared, while an
 * account present as `unsupported` is one that was probed and established
 * nothing. A panel that collapses the two renders a deliberately disabled
 * account as an unhealthy one.
 */
export interface ReconcileReport {
  outcomes: [string, string, AccountSyncOutcome][];
}

// ------------------------------------------------------------------ takeover

/** The external clients Zroutery can hold config fields for. */
export type TakeoverAgent = "claude" | "codex" | "gemini" | "opencode";

/** One field an adoption would rewrite: where it is, what it holds, what lands. */
export interface ProposedChange {
  field_path: string;
  current: unknown;
  managed: unknown;
}

/**
 * A preview of an adoption, bound to the config it was computed against.
 *
 * `fingerprint` must travel back with the confirm call: the backend refuses an
 * adopt whose config has moved since this digest was taken, so the thing the
 * user approved is provably the thing that gets written.
 */
export interface TakeoverProposal {
  managed_fields: string[];
  changes: ProposedChange[];
  fingerprint: string;
}

/** One field someone changed behind a held takeover. */
export interface FieldConflict {
  field_path: string;
  original_value: unknown;
  last_applied: unknown;
  current_external: unknown;
}

/**
 * Where one client's takeover stands, mirroring
 * `zroutery_core::agent_takeover::TakeoverStatus` (tagged, snake_case).
 *
 * The nine states are kept apart rather than collapsed into held/not-held:
 * `degraded` is a written config nobody has proven in effect, and `conflict`
 * is a field someone changed behind Zroutery's back — merging either into
 * `active` would hide the exact moment a user must decide something.
 */
export type TakeoverStatus =
  | { state: "unsupported"; reason: string }
  | { state: "not_detected"; searched: string[]; reason: string }
  | { state: "available"; config_path: string; blocker: string }
  | { state: "ready"; config_path: string; proposal: TakeoverProposal }
  | { state: "active"; config_path: string; generation: number }
  | { state: "conflict"; config_path: string; conflicts: FieldConflict[] }
  | { state: "degraded"; config_path: string; generation: number; reasons: string[] }
  | { state: "released"; config_path: string; generation: number }
  | { state: "error"; reason: string };

export interface TakeoverView {
  agent: TakeoverAgent;
  status: TakeoverStatus;
}

/** What a release did, per field. Mirrors `ResolutionReport`. */
export interface ResolutionReport {
  strategy: "keep_external" | "overwrite_with_managed" | "skip";
  kept_external: string[];
  overwritten: string[];
  skipped: string[];
}

export interface TakeoverReleaseView {
  agent: TakeoverAgent;
  report: ResolutionReport;
  conflicts: FieldConflict[];
  status: TakeoverStatus;
}

// ------------------------------------------------------------------ cutover

/**
 * Where the cutover record stands, mirroring
 * `zroutery_core::migration::MigrationState`.
 *
 * `prepared`, `verified` and `switched` are kept apart from `detected` even
 * though the panel mostly treats them alike, because they are not alike: they
 * are the three states that mean work started and did not finish, and the
 * distinction is what tells a fresh install from one that was interrupted
 * mid-cutover.
 */
export type MigrationStateName =
  | "detected"
  | "prepared"
  | "verified"
  | "switched"
  | "completed"
  | "rolled_back"
  | "failed";

/**
 * One step of the cutover, as a reviewable sentence.
 *
 * The typed action itself is deliberately not on the wire. A panel that
 * received it would be asking a user to confirm a Rust enum rather than a
 * change to their system.
 */
export interface MigrationStepView {
  kind: string;
  description: string;
  /** Whether a rollback puts back what this step changed. */
  reversible: boolean;
}

/**
 * What a cutover would do, bound to the config it was built against.
 *
 * `fingerprint` travels back with the apply call for the same reason the
 * takeover proposal's does: the backend refuses a plan whose digest has moved,
 * so the plan the user read is provably the plan that runs.
 */
export interface MigrationPreview {
  agent: TakeoverAgent;
  config_path: string | null;
  steps: MigrationStepView[];
  fingerprint: string;
  /** Why this client cannot be cut over, when it cannot. */
  blocked_reason: string | null;
}

/**
 * Where the migration record stands and whether the way back is open.
 *
 * `interrupted` and `has_snapshot` answer different questions and the page
 * renders them apart: an interrupted migration with no snapshot is a cutover
 * that cannot be undone, which is the one case where offering rollback would
 * be offering a button that cannot help.
 */
export interface MigrationStatus {
  state: MigrationStateName;
  interrupted: boolean;
  has_snapshot: boolean;
  snapshot_files: string[];
  durable: boolean;
  persist_error: string | null;
  owned_processes: string[];
}

/**
 * What a run produced. Mirrors `zroutery_core::migration::MigrationResult`.
 *
 * `rolled_back` is *not* the same question as `state`, and the page keeps them
 * apart: the state machine records that a rollback transition happened, while
 * this records whether the files actually came back. They diverge exactly when a
 * restore fails, which is the case a user most needs to be told about.
 */
export interface MigrationResult {
  state: MigrationStateName;
  steps_completed: number;
  steps_total: number;
  errors: string[];
  warnings: string[];
  duration_ms: number;
  rolled_back: boolean;
  restored_files: string[];
  failed_restores: string[];
}

/** One entry of a provider's catalogue, with prices when it publishes them. */
export interface DiscoveredModel {
  id: string;
  pricing: Pricing | null;
  /**
   * Whether the provider's source marks this model as served on a zero-cost
   * tier. Always present — the Rust side defaults it to `false` rather than
   * omitting it.
   *
   * This is a property of the source's catalogue, not a promise that the model
   * will answer: a paid id, a region-blocked id and a deprecated id can all be
   * free, and a free id can stop serving without the catalogue changing.
   */
  free: boolean;
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

/**
 * What one promotion round decided, mirroring
 * `zroutery_core::ml::status::PromotionRoundStatus`.
 *
 * The states are kept apart rather than collapsed, because each answers a
 * different question:
 *
 * * `decision` set — the gate judged a candidate;
 * * `reason` set with no decision — no round could be run, and the reason says
 *   which precondition was missing;
 * * `installed` set — a model is serving as a result of this round.
 *
 * `installed` is deliberately not implied by an authorising decision. Judging a
 * model and installing it are separate acts, and the endpoint takes
 * `install: false` by default for exactly that reason.
 */
export interface MlPromotionRoundStatus {
  /** Traces the round read. */
  traces_read: number;
  candidate_commit: string | null;
  decision: PromotionDecision | null;
  /** What the candidate would have done with the same traffic. */
  analysis: ShadowAnalysis | null;
  /**
   * How often the learned policy named each candidate.
   *
   * A ranking that named one candidate on every request has not learned a
   * ranking, whatever the gate said, so this is the number from the round worth
   * having at a glance.
   */
  policy_choices: Record<string, number>;
  installed: string | null;
  reload: ReloadOutcome;
  reason: string | null;
  /** A fault that did not prevent the round from being reported. */
  error: string | null;
}

/** The round's verdict, and the status it left behind. */
export interface MlPromotionRound {
  round: MlPromotionRoundStatus;
  status: MlStatus;
}

/**
 * How much durable history exists, without reading it into memory.
 *
 * `records` is `null` when the log could not be counted, which is a different
 * answer from zero records: a zero renders as "nothing collected" and a null
 * renders as "could not tell", and conflating them is how an operator reads a
 * permission fault as an empty install.
 */
export interface MlTraceInfo {
  records: number | null;
  bytes_on_disk: number;
  path: string | null;
  counters: MlStatus["traces"];
  error: string | null;
}

/**
 * What discarding the durable history removed.
 *
 * `cleared: false` with no error is a real answer rather than a failure:
 * deleting an empty log did what was asked.
 */
export interface MlTraceClear {
  cleared: boolean;
  removed_bytes: number;
  error: string | null;
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
  /**
   * Run one promotion round over the recorded traffic.
   *
   * `install` defaults to false and this side passes it explicitly rather than
   * relying on that default: judging a model and installing it are separate
   * acts, and only the second changes what every later request is served by.
   * `baseline` names the comparison the gate must beat; the evidence floors are
   * not caller-controlled in either the command or the HTTP twin.
   *
   * Call only when `snapshot.ml_available` is true.
   */
  mlPromote: (install: boolean, baseline?: string) =>
    invoke<MlPromotionRound>("run_ml_promotion_round", { install, baseline }),
  /**
   * Size and record count of the durable trace log.
   *
   * Read-only and streaming inside the backend: this is a button a panel polls,
   * not a request path, so it must not load a year of traffic into memory.
   *
   * Call only when `snapshot.ml_available` is true.
   */
  mlTraces: () => invoke<MlTraceInfo>("get_ml_traces"),
  /**
   * Discard the operator's own history.
   *
   * Operator-initiated only, and the absence of a retention policy is
   * deliberate: the promotion round trains from the whole log, so a policy that
   * fired on its own would change what the next model learns from without
   * anyone asking.
   *
   * Call only when `snapshot.ml_available` is true.
   */
  clearMlTraces: () => invoke<MlTraceClear>("clear_ml_traces"),
  /**
   * Open a real browser and check this account in.
   *
   * `start` and `resume` are deliberately separate calls rather than one with a
   * flag. A resume continues the browser already open — with the session that was
   * halfway through solving a challenge — and implementing it as a fresh start
   * would make the user authenticate again every time a WAF appears, which is the
   * whole thing this feature exists to avoid.
   *
   * Call only when `snapshot.account_maintenance_available` is true.
   */
  startCheckin: (provider_id: string, account_id: string) =>
    invoke<CheckinView>("start_checkin", { providerId: provider_id, accountId: account_id }),
  /**
   * Check one account in over the panel's own HTTP API — no browser.
   *
   * The stored credential is the login: the panel adapter authenticates with
   * it, posts the check-in endpoint, and observes what was granted. An instance
   * that demands a Turnstile challenge answers `waiting_for_user`, which is the
   * browser path's cue — the challenge exists to be solved by a person in a
   * real page.
   *
   * Call only when `snapshot.api_checkin_available` is true.
   */
  apiCheckin: (provider_id: string, account_id: string) =>
    invoke<CheckinView>("api_checkin", { providerId: provider_id, accountId: account_id }),
  /** Continue a check-in that is waiting on a human-verification challenge. */
  resumeCheckin: (provider_id: string, account_id: string) =>
    invoke<CheckinView>("resume_checkin", { providerId: provider_id, accountId: account_id }),
  /** Stop a check-in and close its browser. */
  cancelCheckin: (provider_id: string, account_id: string) =>
    invoke<CheckinView>("cancel_checkin", { providerId: provider_id, accountId: account_id }),
  /** Read one account's check-in state. Cheap enough to poll. */
  checkinStatus: (provider_id: string, account_id: string) =>
    invoke<CheckinView>("get_checkin_status", {
      providerId: provider_id,
      accountId: account_id,
    }),
  /**
   * Probe every declared, enabled account and return what the reconciler did.
   *
   * One call covers every provider, so a caller that wants this per provider has
   * to filter the report itself — the backend does not probe a subset.
   *
   * Costs one network request per declared account, which is why nothing calls
   * it on a timer or at startup: this is the operation, not a policy about when.
   * The returned document is Core's own `ReconcileReport`, so what is shown and
   * what the reconciler did are the same value by construction.
   *
   * Call only when `snapshot.account_available` is true, for the same reason the
   * other gated wrappers carry: a command that was never registered and a
   * command that failed are the same event to the webview.
   */
  reconcileAccounts: () => invoke<ReconcileReport>("reconcile_accounts"),
  /**
   * Store one account's own credential and point its declaration at it.
   *
   * Call only when `snapshot.account_available` is true, and only after the
   * declaration itself is saved: the command looks the account up in the
   * stored configuration, so an unsaved declaration reads as unknown.
   */
  setAccountKey: (provider_id: string, account_id: string, api_key: string) =>
    invoke<Snapshot>("set_account_key", {
      providerId: provider_id,
      accountId: account_id,
      apiKey: api_key,
    }),
  /** Drop one account's own credential; it resolves to the provider's key again. */
  clearAccountKey: (provider_id: string, account_id: string) =>
    invoke<Snapshot>("clear_account_key", { providerId: provider_id, accountId: account_id }),
  /** Remove one account declaration, and the credential only it referenced.
   *
   * Call only when `snapshot.account_available` is true.
   */
  removeAccount: (provider_id: string, account_id: string) =>
    invoke<Snapshot>("remove_account", { providerId: provider_id, accountId: account_id }),
  /**
   * Every external agent's takeover reading, in a fixed order.
   *
   * Not part of the snapshot: it reads each client's config file from disk,
   * which is work a poll would repeat for nothing. Call on page mount and
   * after adopt/release.
   */
  takeoverStatus: () => invoke<TakeoverView[]>("takeover_status"),
  /**
   * Take one agent over.
   *
   * `expected_fingerprint` is the digest of the proposal the user confirmed;
   * passing it makes the backend refuse when the agent's config moved between
   * the preview and the click, which is the whole guarantee the preview buys.
   */
  takeoverAdopt: (agent: TakeoverAgent, expected_fingerprint: string | null) =>
    invoke<TakeoverView>("takeover_adopt", { agent, expectedFingerprint: expected_fingerprint }),
  /**
   * Give one agent's fields back, deciding conflicts with `strategy`.
   *
   * `keep_external` preserves what the user changed behind the takeover,
   * `overwrite_with_managed` restores Zroutery's values, `skip` leaves the
   * field exactly as it stands and stops managing it.
   */
  takeoverRelease: (agent: TakeoverAgent, strategy: ResolutionReport["strategy"]) =>
    invoke<TakeoverReleaseView>("takeover_release", { agent, strategy }),

  /**
   * Where the cutover record stands, and whether the way back is open.
   *
   * Not part of the snapshot: it opens a manifest on first call and reads the
   * rollback snapshot beside it. Call on page mount and after any run.
   *
   * Not capability-gated. `zroutery_core::migration` is compiled into every
   * build, so unlike `account` or `ml` there is no configuration in which these
   * commands are absent and the page has nothing to detect.
   */
  migrationStatus: () => invoke<MigrationStatus>("migration_status"),
  /**
   * The cutover plan for every client this build can migrate.
   *
   * Read-only: it reads each client's config path and builds a plan without
   * writing anything, so a page may call it as often as it likes.
   */
  migrationPreviews: () => invoke<MigrationPreview[]>("migration_previews"),
  /**
   * Run one client's cutover.
   *
   * `expected_fingerprint` is the digest of the plan the user confirmed;
   * passing it makes the backend refuse when the plan moved between the
   * preview and the click.
   *
   * The backend snapshots the client's config file before writing anything, so
   * a failed run can be put back. That is the guarantee worth reading before
   * offering the button: without it this call would rewrite a config with no
   * record of what was there.
   */
  migrationApply: (agent: TakeoverAgent, expected_fingerprint: string | null) =>
    invoke<MigrationResult>("migration_apply", {
      agent,
      expectedFingerprint: expected_fingerprint,
    }),
  /**
   * Restore the client's config file exactly as the snapshot captured it.
   *
   * Offered only when the record is `failed`, because that is the only state
   * with a permitted transition back — a `completed` cutover has no rollback
   * the state machine will record, even though the bytes are on disk.
   */
  migrationRollback: () => invoke<MigrationResult>("migration_rollback"),

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
 * A `zroutery://` link, handed over from the desktop shell.
 *
 * The name is a pair with `DEEP_LINK_EVENT` in `src-tauri/src/shell.rs`, the
 * same way `GATEWAY_STATE_EVENT` is a pair with the one in `tray.rs`. The
 * payload is the URL as a string, because the shell does not know what the
 * pages are called — that is the window layer's business, and the shell is
 * the one that has to keep working when the window does not exist yet.
 */
export const DEEP_LINK_EVENT = "zroutery://deep-link";

export function onDeepLink(handler: (url: string) => void): Promise<UnlistenFn> {
  return listen(DEEP_LINK_EVENT, (event) => handler(String(event.payload ?? "")));
}

/**
 * The link that started this application, if one did.
 *
 * A link that arrives while the application is already running is delivered as
 * an event, and the listener above catches it. A link that *started* the
 * application is announced before the dashboard has finished loading, so there
 * is no listener yet and the event goes to nobody. The shell keeps it, and
 * this asks for it — which is what makes "open this application with a link"
 * and "the application was already running" land on the same page.
 */
export function deepLinkCurrent(): Promise<string | null> {
  return invoke<string | null>("deep_link_current");
}

/**
 * The page a `zroutery://` link names, or `null` when it names none.
 *
 * The host of the URL is the page: `zroutery://providers` opens the providers
 * page, and `zroutery://providers/settings` would open the same one, because
 * the path is a sub-view of the page and a sub-view is not a page.
 *
 * A link that names nothing is not an error — a scheme that can be pointed at
 * by a person typing it has to be able to arrive with a path that means
 * nothing to the application — so it returns `null` and the caller keeps the
 * page it was on. The name is matched exactly rather than loosely: a link
 * that opened the wrong page would be worse than one that opened none.
 */
export function pageForDeepLink(url: string): string | null {
  // Only this application's own scheme is acted on. The shell only emits its
  // own, but a link is user input and this is the boundary where that matters.
  const text = String(url ?? "");
  if (!text.startsWith("zroutery://")) return null;
  const rest = text.slice("zroutery://".length);
  const host = rest.split(/[/?#]/, 1)[0]?.toLowerCase() ?? "";
  return host.length > 0 ? host : null;
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

/** The virtual model id a tier is exposed as. */
export function virtualId(tier: ModelTier, style: NamingStyle = "internal"): string {
  const map: Record<NamingStyle, Record<ModelTier, string>> = {
    internal: { fast: "fast-class", standard: "standard-class", reasoning: "reasoning-class", frontier: "frontier-class" },
    anthropic: { fast: "haiku-class", standard: "sonnet-class", reasoning: "opus-class", frontier: "fable-class" },
    openai: { fast: "luna-class", standard: "terra-class", reasoning: "sol-class", frontier: "astra-class" },
  };
  return map[style][tier];
}

/**
 * The tier in words, for prose that is describing a model rather than routing
 * it.
 *
 * `virtualId` is the name a client asks for and has to stay exactly as
 * configured; this is the readable label, so a row can say which tier a model
 * belongs to without printing `standard-class` or a bare enum value.
 *
 * The translator is typed by parameter rather than taken as a dependency,
 * because the dictionary's key union lives in i18n and this file is what it
 * describes. The import is type-only and erased, so there is no cycle at
 * runtime.
 */
export function tierName(
  tier: ModelTier,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): string {
  return t(`tier.name.${tier}` as TKey);
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
/**
 * One account's probed state.
 *
 * `status` is `active`, `suspended`, `quota_exhausted`, `rate_limited`,
 * `authentication_expired`, `unknown` or `unprobed`. `unprobed` is not a status the
 * provider reported: it means nobody has asked this account yet, which is not the
 * same as an account with nothing left, and a zero balance would claim the second.
 */
export interface AccountBalance {
  provider_id: string;
  account_id: string;
  status: string;
  remaining: number | null;
  total: number | null;
  used: number | null;
  unit: string | null;
  last_sync: number | null;
  last_failure: number | null;
}
