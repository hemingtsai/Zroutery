/**
 * Typed bridge to the Rust side. The shapes mirror the serde output of
 * `zroutery-core` and `src-tauri`, so field names stay snake_case.
 */
import { invoke } from "@tauri-apps/api/core";

export type ModelClass = "opus" | "sonnet" | "haiku";
export type ProviderKind = "anthropic" | "openai_compatible";
export type Dialect = "anthropic" | "open_ai" | "openai";
export type RoutingStrategy =
  | "priority"
  | "weighted_random"
  | "round_robin"
  | "lowest_latency";

export const CLASSES: ModelClass[] = ["opus", "sonnet", "haiku"];

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
  enabled: boolean;
  timeout_secs: number;
  connect_timeout_secs: number;
  anthropic_version: string | null;
  quirks: ProviderQuirks;
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
}

export interface RoutingConfig {
  strategy: RoutingStrategy;
  failover: boolean;
  max_attempts: number;
  break_after_failures: number;
  cooldown_secs: number;
  unknown_model_fallback: ModelClass | null;
  client_aliases: Record<string, ModelClass>;
  match_claude_names: boolean;
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
}

export interface AppConfig {
  server: ServerConfig;
  routing: RoutingConfig;
  providers: Provider[];
  models: ModelEntry[];
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
  avg_latency_ms: number;
}

export interface StatsSummary {
  since: string;
  requests: number;
  failures: number;
  input_tokens: number;
  output_tokens: number;
  per_model: ModelTotals[];
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
  saveConfig: (config: AppConfig) => invoke<Snapshot>("save_config", { config }),
  setKey: (provider_id: string, api_key: string) =>
    invoke<Snapshot>("set_provider_key", { providerId: provider_id, apiKey: api_key }),
  clearKey: (provider_id: string) =>
    invoke<Snapshot>("clear_provider_key", { providerId: provider_id }),
  fetchModels: (provider: Provider) =>
    invoke<string[]>("fetch_provider_models", { provider }),
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
