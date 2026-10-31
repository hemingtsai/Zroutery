/**
 * A mock Tauri bridge, so the dashboard can be driven in a plain browser.
 *
 * ## Why this exists
 *
 * The UI is a Tauri webview. It gets `invoke` from the Tauri runtime at
 * startup, so opening `index.html` in a browser rejects every call, the
 * ErrorBoundary catches the first one, and the whole app is replaced by a
 * crash screen. Nothing renders, nothing can be inspected, and a change to a
 * page cannot be checked without building and launching the desktop binary.
 *
 * This installs the same `window.__TAURI_INTERNALS__` shape that
 * `@tauri-apps/api` v2 talks to and answers the commands from fixtures, so
 * `pnpm dev` plus `mock.html` renders the real application against invented
 * data. It is how the routing tabs, the ML panels and the account probe were
 * verified without a build.
 *
 * ## What it is not
 *
 * It is not a test harness and it asserts nothing. A fixture that drifts from
 * the real response shape produces a *wrong* rendering, not a failure — which
 * is exactly the trap worth naming, because a page can look correct against
 * invented data and still be broken against the real thing. The types in
 * `api.ts` are the contract these fixtures are written against; when a
 * response shape changes, update both.
 *
 * ## Running it
 *
 *     pnpm --dir ui dev
 *     # then open http://localhost:5173/mock.html
 *
 * `mock.html` is identical to `index.html` except that it loads this module
 * first. Neither file is referenced by the production build: `vite build`
 * only follows `index.html`, so nothing here reaches `dist/`.
 *
 * ## Keeping the fixtures honest
 *
 * `get_ml_status`, `get_ml_shadow`, `get_ml_traces` and the rest are written
 * against the interfaces in `api.ts` rather than invented. When a panel throws
 * on `undefined`, the fixture is nearly always the thing that is wrong: the
 * real backend always sends the field, the type says so, and a mock that
 * omitted it renders nothing and says nothing about the panel.
 *
 * The `reconcile_accounts` fixture deliberately returns all four
 * `AccountSyncOutcome` variants, because the four render differently and a
 * fixture carrying only the happy path leaves three of them unverified.
 */

type Json = unknown;

/**
 * The fixture is typechecked against the real contract.
 *
 * Three fields drifted this way before this line existed — wallet_holds,
 * account inference_keys and config balances — and each one crashed a page
 * with "reading 'length'/'filter'/'enabled'" only in the mock, because the
 * real backend always sends them. A typed fixture turns the next omission
 * into a compile error instead of a white screen.
 */
import type {
  Account,
  AppConfig,
  BalanceConfig,
  BalancePreset,
  ClientTotals,
  CostTotals,
  KindTotals,
  Maintenance,
  ModelCapabilities,
  ModelEntry,
  ModelTier,
  ModelTotals,
  Pricing,
  Provider,
  ProviderQuirks,
  RequestRecord,
  StatsSummary,
  TraySnapshot,
  UsageDay,
  UsageHistoryPage,
  UsageHistoryQuery,
} from "./src/api";

/**
 * Stress fixture mode — MEASUREMENT ONLY, never a product path.
 *
 * Triggered exclusively by a URL query parameter on mock.html, e.g.
 * `http://localhost:5173/mock.html?stress=600`. Without the parameter this
 * module is byte-for-byte the ordinary small fixture (8 models / 120 records /
 * 2 accounts) that the everyday mock workflow expects; nothing else reads
 * `location.search`, and `vite build` never bundles this file at all.
 *
 * The scale mirrors the master plan M0 frontend baseline target
 * (docs/development/upgrade-master-plan.md): 600 models spread over 13
 * providers and the four tiers, 500 recent records with client attribution,
 * 30 accounts in total. The perf baseline (docs/development/perf-baseline.md)
 * and any later before/after optimization runs measure against this fixture.
 */
const STRESS_MODELS = (() => {
  const raw = new URLSearchParams(window.location.search).get("stress");
  if (raw === null || raw === "") return 0;
  const n = Number.parseInt(raw, 10);
  return Number.isFinite(n) && n > 0 ? n : 600;
})();

const NOW = new Date("2026-10-17T09:41:00Z").getTime();

/**
 * Two clocks, because the backend has two.
 *
 * `ago()` feeds the fields serde renders out of `DateTime<Utc>`; those arrive as
 * ISO strings and are milliseconds. The fields the ML stack and the account
 * maintenance code set from `now_seconds()` / `Utc::now().timestamp()` are bare
 * i64 unix *seconds*, and the UI multiplies those by 1000 before formatting.
 *
 * Getting this wrong is not a cosmetic slip in the fixture. `when()` multiplied
 * milliseconds by 1000 a second time and the promotion history rendered as
 * dates in the year 58,700 — a correct UI made to look broken by the data.
 */
const NOW_SECS = Math.floor(NOW / 1000);
const ago = (ms: number) => new Date(NOW - ms).toISOString();
const TRAY_TIERS: ModelTier[] = ["fast", "standard", "reasoning", "frontier"];

const quirks = (): ProviderQuirks => ({
  use_max_completion_tokens: false,
  drop_temperature: false,
  drop_top_p: false,
  drop_stop: false,
  stream_usage: true,
  system_as_developer: false,
  send_reasoning_effort: false,
  portable_history: false,
});

const balanceConfig = (preset: BalancePreset): BalanceConfig => ({
  preset,
  custom: preset === "custom"
    ? {
        path: "/v1/usage",
        remaining_pointer: "data.wallet.remaining",
        total_pointer: "data.wallet.total",
        used_pointer: "data.wallet.used",
        currency_pointer: null,
        currency: "USD",
      }
    : null,
});

const maintenance = (enabled: boolean): Maintenance => ({
  checkin_enabled: enabled,
  checkin_interval_secs: enabled ? 86400 : null,
  checkin_path: enabled ? "/console/checkin" : null,
  checkin_trigger_selector: enabled ? "button.checkin" : null,
  login_path: enabled ? "/console/login" : null,
  panel_url: null,
  new_api_user: null,
  browser_executable: "chrome",
});

/** Account.inference_keys is required; the provider drawer filters on it. */
const account = (
  account_id: string,
  enabled: boolean,
  maintenanceEnabled: boolean,
): Account => ({
  account_id,
  key_ref: `zroutery/account/relay-main/${account_id.slice(-1)}`,
  enabled,
  inference_keys: [],
  maintenance: maintenance(maintenanceEnabled),
});

const capabilities = (over: Partial<ModelCapabilities> = {}): ModelCapabilities => ({
  vision: false,
  tools: true,
  thinking: false,
  structured_output: false,
  audio: false,
  video: false,
  files: false,
  ...over,
});

const providers: Provider[] = [
  {
    id: "relay-main",
    name: "Aurora Relay",
    kind: "anthropic",
    base_url: "https://relay.example.com",
    key_ref: "zroutery/anthropic/relay-main",
    extra_headers: {},
    client_profile: "claude_code",
    bearer_auth: true,
    enabled: true,
    timeout_secs: 600,
    connect_timeout_secs: 15,
    anthropic_version: "2023-06-01",
    api_surface: "chat",
    decision: { path: "/v1/systemone", threshold: 0.85 },
    quirks: quirks(),
    balance: balanceConfig("none"),
    accounts: [account("acct-0", true, true), account("acct-1", true, false)],
  },
  {
    id: "dsk",
    name: "DeepSeek",
    kind: "openai_compatible",
    base_url: "https://api.deepseek.com",
    key_ref: "zroutery/openai/dsk",
    extra_headers: {},
    client_profile: "native",
    bearer_auth: false,
    enabled: true,
    timeout_secs: 300,
    connect_timeout_secs: 15,
    anthropic_version: null,
    api_surface: "chat",
    decision: { path: "/v1/systemone", threshold: 0.85 },
    quirks: quirks(),
    balance: balanceConfig("deep_seek"),
  },
  {
    // Disabled on purpose: the provider list, the models page and the routing
    // flows all read `enabled` differently, and a fixture where everything is
    // on never exercises the other branch.
    id: "codex",
    name: "Codex Relay",
    kind: "openai_compatible",
    base_url: "https://codex.example.net/v1",
    key_ref: "zroutery/openai/codex",
    extra_headers: { "X-Tenant": "lab" },
    client_profile: "codex",
    bearer_auth: false,
    enabled: false,
    timeout_secs: 600,
    connect_timeout_secs: 20,
    anthropic_version: null,
    api_surface: "chat",
    decision: { path: "/v1/systemone", threshold: 0.85 },
    quirks: { ...quirks(), use_max_completion_tokens: true },
    balance: balanceConfig("none"),
  },
];

/*
 * Stress mode: pad the provider list to 13 (3 handcrafted + 10 generated) and
 * spread 30 accounts over them — the account surface the M0 baseline calls
 * for. Generated providers are deliberately ordinary: one disabled, the rest
 * enabled, kinds alternating anthropic / openai_compatible, so every render
 * branch the pages already have stays exercised at scale.
 */
if (STRESS_MODELS > 0) {
  const STRESS_PROVIDERS = 10;
  const accountQuotas = [3, 3, 3, 3, 3, 3, 3, 3, 2, 2]; // 28 + relay-main's 2 = 30
  let acctSeq = 0;
  for (let p = 0; p < STRESS_PROVIDERS; p++) {
    const id = `stress-p${String(p + 1).padStart(2, "0")}`;
    const enabled = p !== 6; // one disabled provider, like the handcrafted set
    const generatedAccounts = Array.from({ length: accountQuotas[p] }, (_, a): Account => {
      acctSeq += 1;
      const acctId = `acct-s${String(acctSeq).padStart(2, "0")}`;
      return account(acctId, a !== accountQuotas[p] - 1, a === 0);
    });
    providers.push({
      id,
      name: `Stress Relay ${p + 1}`,
      kind: p % 2 === 0 ? "anthropic" : "openai_compatible",
      base_url: `https://${id}.example.com`,
      key_ref: `zroutery/stress/${id}`,
      extra_headers: {},
      client_profile: "native",
      bearer_auth: p % 2 === 0,
      enabled,
      timeout_secs: 600,
      connect_timeout_secs: 15,
      anthropic_version: p % 2 === 0 ? "2023-06-01" : null,
      api_surface: "chat",
      decision: { path: "/v1/systemone", threshold: 0.85 },
      quirks: quirks(),
      balance: balanceConfig("none"),
      accounts: generatedAccounts,
    });
  }
}

type ModelSeed = [
  provider_id: string,
  upstream_model: string,
  tier: ModelEntry["tier"],
  priority: number,
  weight: number,
];

const models: ModelEntry[] = ([
  ["relay-main", "claude-sonnet-4-5", "standard", 10, 1.0],
  ["relay-main", "claude-opus-4-1", "frontier", 20, 0.6],
  ["relay-main", "claude-haiku-4-5", "fast", 10, 1.4],
  ["relay-main", "glm-4.6", "reasoning", 15, 0.9],
  ["dsk", "deepseek-chat", "standard", 10, 1.2],
  ["dsk", "deepseek-reasoner", "reasoning", 20, 0.8],
  ["codex", "gpt-5-codex", "standard", 10, 1.0],
] as ModelSeed[]).map(([provider_id, upstream_model, tier, priority, weight], i): ModelEntry => ({
  provider_id,
  upstream_model,
  tier,
  priority,
  weight,
  enabled: true,
  capabilities: capabilities({ vision: i === 0, thinking: i === 3 || i === 5 }),
  display_name: null,
  aliases: i === 0 ? ["sonnet"] : [],
  max_output_tokens: 8192,
  context_window: 200_000,
  effort_levels: [],
  pricing: {
    currency: "USD",
    input_per_mtok: i < 4 ? 3 : 0.27,
    output_per_mtok: i < 4 ? 15 : 1.1,
    cache_read_per_mtok: i < 4 ? 0.3 : null,
    cache_write_per_mtok: i < 4 ? 3.75 : null,
  } as Pricing,
}));

// Unclassified and disabled, so the Models page has its warning banner and the
// nav badge has something to count.
models.push({
  provider_id: "dsk",
  upstream_model: "deepseek-coder",
  tier: null,
  priority: 99,
  weight: 1,
  enabled: false,
  capabilities: capabilities(),
  display_name: "DeepSeek Coder",
  aliases: [],
  max_output_tokens: 4096,
  context_window: 128_000,
  effort_levels: [],
  pricing: null,
});

/*
 * Stress mode: pad the catalogue to `?stress=N` models (the 8 handcrafted
 * ones stay first), spread across all 13 providers and all four tiers, with
 * the same mix of enabled/disabled and priced/unpriced the small fixture has.
 * `exposed` below picks the padded list up, which also scales modelHealth()
 * (one entry per exposed id) — exactly the payload the Models page and the
 * 2-second activity poll have to chew on.
 */
if (STRESS_MODELS > models.length) {
  const allProviderIds = providers.map((p) => p.id);
  const tiers: ModelEntry["tier"][] = ["standard", "frontier", "fast", "reasoning"];
  for (let i = models.length; i < STRESS_MODELS; i++) {
    const providerId = allProviderIds[i % allProviderIds.length];
    const tier = tiers[i % tiers.length];
    const priced = tier !== "frontier";
    models.push({
      provider_id: providerId,
      upstream_model: `stress-model-${String(i + 1).padStart(4, "0")}`,
      tier,
      priority: 10 + (i % 3) * 5,
      weight: 0.6 + (i % 5) * 0.2,
      enabled: i % 40 !== 7,
      capabilities: capabilities({ thinking: tier === "reasoning", vision: i % 25 === 0 }),
      display_name: null,
      aliases: [],
      max_output_tokens: 8192,
      context_window: 128_000 + (i % 4) * 32_000,
      effort_levels: [],
      pricing: priced
        ? {
            currency: "USD",
            input_per_mtok: 0.27 + (i % 7) * 0.4,
            output_per_mtok: 1.1 + (i % 7) * 1.6,
            cache_read_per_mtok: 0.07,
            cache_write_per_mtok: null,
          }
        : null,
    });
  }
}

const exposed = models.map((m) => `${m.provider_id}:${m.upstream_model}`);

const appConfig = (): AppConfig => ({
  server: {
    host: "127.0.0.1",
    port: 8787,
    require_auth: true,
    auth_token: "",
    autostart: false,
    allow_cors: false,
    cors_origins: [],
    max_body_mib: 16,
    log_limit: 500,
    bypass_proxy: false,
  },
  routing: {
    strategy: "balanced",
    failover: true,
    max_attempts: 3,
    break_after_failures: 2,
    cooldown_secs: 60,
    unknown_model_fallback: "standard",
    client_aliases: { opus: "frontier", sonnet: "standard", haiku: "fast" },
    match_claude_names: true,
    scoring: {
      price_weight: 0.5,
      latency_weight: 0.5,
      reference_input_tokens: 2000,
      reference_output_tokens: 500,
    },
    rectifier: {
      enabled: true,
      thinking_signature: true,
      media_fallback: true,
      thinking_budget: true,
      thinking_fallback: true,
      codex_metadata: true,
      codex_auto: true,
    },
    elect_on_start: true,
    naming_style: "internal",
  },
  classifier: {
    enabled: true,
    strategy: "priority",
    failover: true,
    max_attempts: 2,
    candidates: [
      { model: "dsk:deepseek-chat", priority: 10, enabled: true },
      { model: "relay-main:glm-4.6", priority: 20, enabled: true },
    ],
    detection: {
      enabled: true,
      minimum_confidence: 0.7,
      builtins: {
        anthropic_beta: true,
        xml_classifier_signature: true,
        model_1m_signature: false,
        codex_guardian_signature: false,
      },
      signatures: [],
    },
  },
  window: { launch_on_login: false, silent_start: false, keep_in_tray: true },
  vision: { enabled: true, model: "relay-main:claude-sonnet-4-5", placeholder: "[image]" },
  // AppConfig.balances is required (Settings reads config.balances.enabled
  // unconditionally); omitting it crashed the settings page — the same
  // fixture-drift trap as wallet_holds and inference_keys.
  balances: { enabled: true, interval_secs: 900 },
  shadow: { enabled: true, max_decisions: 10000, max_age_secs: 604800 },
  ml_routing: { enabled: true, state_dir: "ml", exploration_probability: 0, exploration_seed: 42 },
  providers,
  models,
  budgets: [
    {
      id: "daily-global",
      scope: { kind: "global" },
      period: "day",
      limit: { currency: "USD", amount: 20 },
      on_exceeded: { action: "degrade", to: "fast" },
      enabled: true,
    },
  ],
});

const modelHealth = () =>
  exposed.map((model_id, i) => ({
    model_id,
    consecutive_failures: i === 2 ? 2 : 0,
    total_success: 40 - i * 3,
    total_failure: i === 2 ? 5 : i,
    avg_latency_ms: 900 + i * 130,
    cooldown_remaining_secs: i === 2 ? 42 : 0,
    last_error: i === 2 ? "upstream returned 529 overloaded" : null,
  }));

/**
 * Recent-record count: 500 under `?stress` (the M0 baseline's activity scale,
 * each attributed to a client because the ledger keys on it), 120 otherwise.
 */
const RECENT_COUNT = STRESS_MODELS > 0 ? 500 : 120;

/** Client attribution rotation for stress records, unknown-UA straggler last. */
const STRESS_CLIENTS = [
  { client_id: "claude_code", client_name: "Claude Code" },
  { client_id: "opencode", client_name: "OpenCode" },
  { client_id: "codex_cli", client_name: "Codex CLI" },
  { client_id: "gemini_cli", client_name: "Gemini CLI" },
  { client_id: "mozilla_5.0_windows_nt_10.0", client_name: "" },
];

/** Accounting totals replace legacy last-attempt fields; they never add to them. */
const observedUsage = (record: RequestRecord) =>
  record.accounting?.observed_usage ?? record.usage;

const unpriced = (record: RequestRecord) =>
  record.accounting
    ? record.accounting.unknown_cost_attempts > 0
    : record.cost === null;

const addKnownCost = (totals: CostTotals, record: RequestRecord) => {
  if (record.accounting) {
    for (const [currency, amount] of Object.entries(record.accounting.known_cost)) {
      totals[currency] = (totals[currency] ?? 0) + amount;
    }
  } else if (record.cost) {
    totals[record.cost.currency] = (totals[record.cost.currency] ?? 0) + record.cost.amount;
  }
};

const recent = (): RequestRecord[] =>
  // 120 records, not six: the pager needs more than one page to exist, and
  // a failure every seventh row keeps 仅失败 from being an empty view. One
  // very long request stays, because a real relay request once took 267522
  // ms — the figure that forced the duration formatter to grow a tier.
  Array.from({ length: RECENT_COUNT }, (_, i): RequestRecord => {
    const modelIndex = i % (STRESS_MODELS > 0 ? exposed.length : 4);
    const model = models[modelIndex];
    const provider = providers.find((candidate) => candidate.id === model.provider_id);
    const failed = i % 7 === 5;
    const legacyUnpriced = !failed && i % 29 === 11;
    const legacyUsage = {
      input_tokens: 1800 + (i % 6) * 220,
      output_tokens: 420 + (i % 6) * 60,
      cache_read_tokens: i % 2 === 0 ? 900 : 0,
      cache_write_tokens: 0,
      reasoning_tokens: i % 17 === 3 ? 310 : 0,
    };
    return {
      id: `req_${1000 + i}`,
      at: ago(i * 47_000),
      ingress: "http",
      kind: i % 17 === 3 ? "auto_mode" : "main",
      requested_model: i % 17 === 3 ? "glm-4.6" : "sonnet",
      resolved_model: exposed[modelIndex],
      provider_id: model.provider_id,
      model_name: model.upstream_model,
      provider_name: provider?.name ?? null,
      // The durable ledger keys every record by client (stats.rs), and the
      // ledger table's row-click filter matches on it — without a client on
      // each fixture record the linkage renders an empty stream.
      client: STRESS_MODELS > 0
        ? STRESS_CLIENTS[i % STRESS_CLIENTS.length]
        : {
            client_id: i === 7 ? "mozilla_5.0_windows_nt_10.0" : i % 2 === 0 ? "claude_code" : "opencode",
            client_name: i === 7 ? "" : i % 2 === 0 ? "Claude Code" : "OpenCode",
          },
      stream: i % 2 === 0,
      status: failed ? 529 : 200,
      ok: !failed,
      error: failed ? "upstream overloaded" : null,
      latency_ms: i === 2 ? 267_522 : 780 + (i % 6) * 210,
      ttft_ms: 210 + (i % 6) * 40,
      usage: legacyUsage,
      // Failed multi-attempt requests deliberately carry misleading legacy
      // last-attempt values. Aggregates replace them with accounting totals;
      // adding both would make this fixture's usage/cost obviously too large.
      cost: failed
        ? { currency: "USD", amount: 999 }
        : legacyUnpriced
          ? null
          : { currency: "USD", amount: 0.0121 + (i % 6) * 0.002 },
      attempts: failed ? 3 : 1,
      accounting: failed
        ? {
            schema_version: 1,
            persistence: "durable",
            attempt_ids: [`req_${1000 + i}_attempt_1`, `req_${1000 + i}_attempt_2`],
            known_cost: { USD: 0.0095 + (i % 6) * 0.001 },
            unknown_cost_attempts: 1,
            observed_usage: {
              input_tokens: legacyUsage.input_tokens + 600,
              output_tokens: legacyUsage.output_tokens + 80,
              cache_read_tokens: legacyUsage.cache_read_tokens + 200,
              cache_write_tokens: 50,
              reasoning_tokens: legacyUsage.reasoning_tokens + 20,
            },
            unknown_usage_attempts: 0,
          }
        : undefined,
    };
  });

const clientMetadata = (client_id: string) => {
  const recognised = ["claude_code", "opencode", "codex_cli", "gemini_cli"].includes(client_id);
  return {
    family: recognised ? client_id : "unknown",
    source: "header",
    confidence: recognised ? "certain" : "guessed",
  };
};

/**
 * Build every dimension from the same records and the same accounting rule.
 * The 999 USD legacy values on failed records must therefore appear nowhere in
 * these totals, while their small known subtotals survive beside unpriced rows.
 */
const statsSummary = (): StatsSummary => {
  const records = recent();
  const perModel = new Map<string, ModelTotals>();
  const perKind = new Map<string, KindTotals>();
  const perClient = new Map<string, ClientTotals>();
  const cost: CostTotals = {};
  let failures = 0;
  let inputTokens = 0;
  let outputTokens = 0;
  let cacheReadTokens = 0;
  let cacheWriteTokens = 0;
  let unpricedRequests = 0;

  for (const record of records) {
    const usage = observedUsage(record);
    failures += Number(!record.ok);
    inputTokens += usage.input_tokens;
    outputTokens += usage.output_tokens;
    cacheReadTokens += usage.cache_read_tokens;
    cacheWriteTokens += usage.cache_write_tokens;
    unpricedRequests += Number(unpriced(record));
    addKnownCost(cost, record);

    const modelId = record.resolved_model ?? "unresolved";
    let model = perModel.get(modelId);
    if (!model) {
      model = {
        model_id: modelId,
        requests: 0,
        failures: 0,
        input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        cached_tokens: 0,
        cost: {},
        avg_latency_ms: 0,
      };
      perModel.set(modelId, model);
    }
    model.requests += 1;
    model.failures += Number(!record.ok);
    model.input_tokens += usage.input_tokens;
    model.output_tokens += usage.output_tokens;
    model.reasoning_tokens += usage.reasoning_tokens;
    model.cached_tokens += usage.cache_read_tokens;
    model.avg_latency_ms += (record.latency_ms - model.avg_latency_ms) / model.requests;
    addKnownCost(model.cost, record);

    let kind = perKind.get(record.kind);
    if (!kind) {
      kind = {
        kind: record.kind,
        requests: 0,
        failures: 0,
        input_tokens: 0,
        output_tokens: 0,
        avg_latency_ms: 0,
      };
      perKind.set(record.kind, kind);
    }
    kind.requests += 1;
    kind.failures += Number(!record.ok);
    kind.input_tokens += usage.input_tokens;
    kind.output_tokens += usage.output_tokens;
    kind.avg_latency_ms += (record.latency_ms - kind.avg_latency_ms) / kind.requests;

    const clientId = record.client?.client_id ?? "unknown";
    let client = perClient.get(clientId);
    if (!client) {
      client = {
        client_id: clientId,
        client_name: record.client?.client_name ?? "",
        ...clientMetadata(clientId),
        requests: 0,
        failures: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost: {},
        avg_latency_ms: 0,
        avg_ttft_ms: null,
        streamed: 0,
      };
      perClient.set(clientId, client);
    }
    client.requests += 1;
    client.failures += Number(!record.ok);
    client.input_tokens += usage.input_tokens;
    client.output_tokens += usage.output_tokens;
    client.cache_read_tokens += usage.cache_read_tokens;
    client.cache_write_tokens += usage.cache_write_tokens;
    client.reasoning_tokens += usage.reasoning_tokens;
    client.avg_latency_ms += (record.latency_ms - client.avg_latency_ms) / client.requests;
    addKnownCost(client.cost, record);
    if (record.ttft_ms !== null) {
      client.streamed += 1;
      client.avg_ttft_ms =
        client.avg_ttft_ms === null
          ? record.ttft_ms
          : client.avg_ttft_ms +
            (record.ttft_ms - client.avg_ttft_ms) / client.streamed;
    }
  }

  const byKey = <T>(key: (value: T) => string) => (left: T, right: T) =>
    key(left).localeCompare(key(right));
  return {
    since: records.at(-1)?.at ?? ago(0),
    requests: records.length,
    failures,
    input_tokens: inputTokens,
    output_tokens: outputTokens,
    cache_read_tokens: cacheReadTokens,
    cache_write_tokens: cacheWriteTokens,
    unpriced_requests: unpricedRequests,
    cost,
    per_model: [...perModel.values()].sort(byKey((value) => value.model_id)),
    per_kind: [...perKind.values()].sort(byKey((value) => value.kind)),
    per_client: [...perClient.values()].sort(byKey((value) => value.client_id)),
  };
};

/**
 * The learned model's state.
 *
 * One promote in the history is what makes the rollback button available, and
 * two candidates that the deterministic plan never reaches is what makes the
 * blind-spot warning appear. A fixture without either leaves those branches
 * unrendered.
 */
function mlStatusFixture() {
  return {
    routing_enabled: true,
    durable_state: true,
    traces_open: true,
    model_store_open: true,
    active: {
      model_id: "ranker-2026-10-15",
      commit_id: "a91f3c2",
      verdict: "promoted",
      dataset_fingerprint: "ds-7f21",
      fitted_partition_fingerprint: "fp-4c0e",
      gate_config_identity: "gate-v3",
      required_baseline: "ranker-2026-10-02",
      paired_requests: 1_842,
      holdout_loss: 0.214,
      promoted_at: NOW_SECS - 2 * 86_400,
    },
    active_decision: {
      verdict: "promoted",
      candidate_commit: "a91f3c2",
      model_id: "ranker-2026-10-15",
      learning_event_count: 18_402,
      dataset_fingerprint: "ds-7f21",
      fitted_partition_fingerprint: "fp-4c0e",
      holdout_loss: 0.214,
      gate_config_identity: "gate-v3",
      criteria: [
        /*
         * The nine names, their order and their shapes are copied from
         * `PromotionGate::evaluate`, including which two carry no figure at all
         * — `baseline_present` and `dataset_identity` pass `None` for both
         * fields, because each is a fact rather than a measurement.
         *
         * An invented name here is not cosmetic: it renders as an untranslated
         * identifier, which is indistinguishable from a criterion a newer
         * backend added that this build has no translation for.
         */
        { name: "baseline_present", held: true, measured: null, threshold: null, reason: "paired against 'ranker-2026-10-02'" },
        { name: "paired_evidence", held: true, measured: 1842, threshold: 1000, reason: "" },
        { name: "holdout_quality", held: true, measured: 0.214, threshold: 0.05, reason: "" },
        { name: "routing_utility", held: true, measured: 0.044, threshold: 0.01, reason: "" },
        { name: "selection_safety", held: true, measured: 0, threshold: 0, reason: "" },
        { name: "success_regression", held: true, measured: 3, threshold: 20, reason: "" },
        { name: "cost_regression", held: true, measured: -0.0012, threshold: 0.05, reason: "" },
        { name: "latency_regression", held: true, measured: 12.4, threshold: 250, reason: "" },
        { name: "dataset_identity", held: true, measured: null, threshold: null, reason: "both name source body ds-7f21 (fitted partition fp-4c0e)" },
      ],
      baseline: "ranker-2026-10-02",
      paired_requests: 1_842,
      decided_at: NOW_SECS - 2 * 86_400,
      revision: "rev-31",
    },
    history: [
      {
        action: "promote",
        commit_id: "a91f3c2",
        gate_identity: "gate-v3",
        verdict: "promoted",
        at: NOW_SECS - 2 * 86_400,
        note: "beat baseline on the held-out split",
      },
      {
        action: "promote",
        commit_id: "5b0e7d1",
        gate_identity: "gate-v3",
        verdict: "promoted",
        at: NOW_SECS - 16 * 86_400,
        note: "first learned model in the routing path",
      },
      {
        action: "rollback",
        commit_id: "5b0e7d1",
        gate_identity: "gate-v3",
        verdict: "rejected",
        at: NOW_SECS - 12 * 86_400,
        note: "held-out accuracy regressed after promotion",
      },
    ],
    routing: { rankings: 4_812, fallbacks: 96, explorations: 41, blind_explorations: 7, attached: true },
    exploration_probability: 0,
    exploration_seed: 0,
    blind_candidates: [
      { model_id: exposed[6], provider_id: "codex" },
      { model_id: exposed[3], provider_id: "relay-main" },
    ],
    dataset: {
      ingested: 18_402,
      samples: 17_980,
      no_decision_time_input: 0,
      rejected: 422,
      evicted_by_count: 1_204,
      evicted_by_age: 640,
      faults: 0,
    },
    traces: { appended: 18_402, nothing: 312, refused: 9, io_errors: 0 },
    shadow: { enabled: true, decisions_recorded: 2_140, faults: 3 },
    read_at: NOW_SECS,
  };
}

let running = true;

/**
 * The config the bridge hands out, as state rather than a fresh `appConfig()`
 * per call.
 *
 * The shell refreshes the whole snapshot on focus and visibility changes, and
 * a bridge that rebuilt the fixture each time would answer a refresh with the
 * pre-edit configuration — every save silently undone by the next alt-tab.
 * The real backend persists to disk; sessionStorage is the honest stand-in:
 * it survives the reload that a fixture constant would not, and dies with the
 * tab, so one mock session never leaks into the next.
 */
/**
 * Stress sessions get their own config slot. The stored key would otherwise
 * leak a small fixture's saved config across a `?stress` reload (the shell
 * persists the whole config, providers and models included), silently
 * replacing the stress catalogue with the 8-model one.
 */
const CONFIG_STORE_KEY =
  STRESS_MODELS > 0 ? `zroutery-mock-config:stress-${STRESS_MODELS}` : "zroutery-mock-config";

let currentConfig: AppConfig = (() => {
  const stored = sessionStorage.getItem(CONFIG_STORE_KEY);
  if (stored) {
    try {
      const config = JSON.parse(stored) as AppConfig;
      // Older mock sessions may predate the bridge-only marker stripping.
      // It is not configuration and must never participate in the digest.
      delete config.__config_revision;
      return config;
    } catch {
      sessionStorage.removeItem(CONFIG_STORE_KEY);
    }
  }
  return appConfig();
})();

/** Stable JSON digest, opaque to callers just like the backend's config digest. */
const configRevision = (config: AppConfig): string => {
  const json = JSON.stringify(config);
  let hash = 0x811c9dc5;
  for (let index = 0; index < json.length; index += 1) {
    hash ^= json.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return `mock-${(hash >>> 0).toString(16).padStart(8, "0")}`;
};

const snapshot = (): Json => ({
  // Tauri deserializes a fresh object in each webview. Clone here too so the
  // API wrapper can annotate this returned document without mutating the mock's
  // persisted source of truth or changing its digest.
  config: structuredClone(currentConfig),
  config_revision: configRevision(currentConfig),
  exposed_ids: exposed,
  issues: [
    {
      severity: "warning",
      code: "model.no_tier",
      message: "deepseek-coder has no tier, so clients cannot ask for it by name.",
      subject: "dsk:deepseek-coder",
    },
  ],
  blocking: false,
  server: {
    running,
    address: running ? "127.0.0.1:8787" : null,
    base_url: running ? "http://127.0.0.1:8787" : null,
    host: "127.0.0.1",
    port: 8787,
    require_auth: true,
    token_hint: "zr-…7c41",
    // The mock always listens on loopback, so it is never exposed. The old
    // hardcoded `true` pinned a logically self-contradictory banner over
    // every mock page ("listening on 127.0.0.1 exposes the gateway to your
    // network") — a real backend only sets this for non-loopback binds.
    exposed: false,
  },
  keys: { "relay-main": true, dsk: true, codex: false },
  // Snapshot.wallet_holds is a required field in api.ts; omitting it crashed
  // the providers page with "reading 'length'" — the fixture-drift trap this
  // file's header warns about.
  wallet_holds: [],
  health: modelHealth(),
  summary: statsSummary(),
  recent: recent(),
  warning: null,
  config_path: "C:\\Users\\operator\\.zroutery\\config.toml",
  version: "1.0.0-rc.1",
  balances: {
    dsk: {
      checked_at: ago(120_000),
      balance: { currency: "USD", remaining: 42.18, total: 60, used: 17.82 },
      error: null,
    },
    "relay-main": {
      checked_at: ago(120_000),
      balance: null,
      error: "this provider publishes no balance",
    },
  },
  election: {
    decided_at: ago(600_000),
    scoring: appConfig().routing.scoring,
    tiers: {
      standard: {
        tier: "standard",
        ranked: [
          { model_id: exposed[0], score: 0.82, latency_ms: 940, price: { currency: "USD", amount: 3.2 }, note: null },
          { model_id: exposed[4], score: 0.74, latency_ms: 1120, price: { currency: "USD", amount: 1.1 }, note: null },
        ],
        priced: true,
        note: null,
      },
      fast: {
        tier: "fast",
        ranked: [{ model_id: exposed[2], score: 0.91, latency_ms: 700, price: null, note: null }],
        priced: false,
        note: "this tier has no published prices, so price was left out",
      },
    },
  },
  budgets: [
    { budget: appConfig().budgets[0], spent: { currency: "USD", amount: 6.41 }, used: 0.32 },
  ],
  ml_available: true,
  account_maintenance_available: true,
  account_available: true,
  api_checkin_available: true,
  token_capture_available: true,
  checkin: [
    {
      provider_id: "relay-main",
      account_id: "acct-0",
      phase: "succeeded",
      browser_held: false,
      awaiting_user: false,
      failure: null,
      reward_amount: 0.5,
      reward_unit: "USD",
      reward_source: "provider_response",
      last_completed_at: NOW_SECS - 3 * 3600,
      schedule: {
        decision: "not_due",
        next_due_at: NOW_SECS + 21 * 3600,
        last_settled_at: NOW_SECS - 3 * 3600,
      },
    },
    {
      // Disabled rather than successful, so the scheduler's own wording for
      // "nothing is switched on" is exercised next to the ordinary case.
      provider_id: "relay-main",
      account_id: "acct-1",
      phase: "idle",
      browser_held: false,
      awaiting_user: false,
      failure: null,
      reward_amount: null,
      reward_unit: null,
      reward_source: null,
      last_completed_at: null,
      schedule: { decision: "disabled", next_due_at: null, last_settled_at: null },
    },
  ],
  account_events: [
    {
      provider_id: "relay-main",
      account_id: "acct-0",
      observed_at: NOW_SECS - 3 * 3600,
      kind: "checkin",
      effect: "credited",
      amount: 0.5,
      unit: "USD",
      source: "provider_response",
      description: "check-in credited",
    },
  ],
  account_balances: [
    {
      provider_id: "relay-main",
      account_id: "acct-0",
      status: "active",
      remaining: 12.5,
      total: null,
      used: null,
      unit: "USD",
      last_sync: NOW_SECS,
      last_failure: null,
    },
    {
      provider_id: "relay-main",
      account_id: "acct-1",
      status: "active",
      remaining: 0,
      total: null,
      used: null,
      unit: "USD",
      last_sync: NOW_SECS,
      last_failure: null,
    },
  ],
});

/**
 * Safe projection for the tray panel's restricted command allowlist.
 *
 * This reads the full mock state internally but returns no provider URL,
 * headers, credentials, configuration, raw error or trace. "Recent" means an
 * observed request only: a tier with no matching record remains null rather
 * than pretending a configured priority was executed.
 */
const traySnapshot = (): TraySnapshot => {
  const records = recent().slice(0, 8);
  return {
    server: {
      running,
      base_url: running ? "http://127.0.0.1:8787" : null,
      host: currentConfig.server.host,
      port: currentConfig.server.port,
    },
    strategy: currentConfig.routing.strategy,
    tiers: TRAY_TIERS.map((tier) => {
      const tierModels = currentConfig.models
        .map((model) => ({
          model,
          id: `${model.provider_id}:${model.upstream_model}`,
        }))
        .filter(({ model }) => model.tier === tier && currentConfig.providers.some((provider) => provider.id === model.provider_id && provider.kind !== "decision"))
        .sort(
          (left, right) =>
            left.model.priority - right.model.priority || left.id.localeCompare(right.id),
        );
      const ids = new Set(tierModels.map(({ id }) => id));
      return {
        tier,
        models: tierModels.map(({ model, id }) => ({
          id,
          name: model.display_name?.trim() || model.upstream_model,
          provider_id: model.provider_id,
          enabled: model.enabled && currentConfig.providers.some((provider) => provider.id === model.provider_id && provider.enabled),
          priority: model.priority,
        })),
        recent_model_id:
          records.find((record) => record.resolved_model !== null && ids.has(record.resolved_model))
            ?.resolved_model ?? null,
      };
    }),
    providers: currentConfig.providers.map(({ id, name, enabled }) => ({ id, name, enabled })),
    recent: records.slice(0, 8).map((record) => ({
      id: record.id,
      at: record.at,
      client_name: record.client?.client_name ?? "",
      model_name: record.model_name ?? record.resolved_model,
      ok: record.ok,
      latency_ms: record.latency_ms,
    })),
  };
};

function checkin(phase: string, held: boolean) {
  return {
    provider_id: "relay-main",
    account_id: "acct-0",
    phase,
    browser_held: held,
    awaiting_user: false,
    failure: null,
    reward_amount: phase === "succeeded" ? 0.5 : null,
    reward_unit: phase === "succeeded" ? "USD" : null,
    reward_source: phase === "succeeded" ? "provider_response" : null,
    last_completed_at: NOW_SECS,
    schedule: { decision: "not_due", next_due_at: NOW_SECS + 21 * 3600, last_settled_at: NOW_SECS },
  };
}

const handlers: Record<string, (args: any) => Json | Promise<Json>> = {
  // Tauri events go through the event plugin rather than a command name, and
  // `listen` is called on mount, so a missing pair here shows up as an
  // unhandled rejection on every page.
  "plugin:event|listen": () => nextCallback++,
  "plugin:event|unlisten": () => null,

  get_snapshot: () => snapshot(),
  get_activity: () => ({ health: modelHealth(), summary: statsSummary(), recent: recent() }),
  get_tray_snapshot: () => traySnapshot(),
  tray_set_provider_enabled: (args: { providerId?: string; enabled?: boolean }) => {
    const providerId = args.providerId ?? "";
    const provider = currentConfig.providers.find((candidate) => candidate.id === providerId);
    if (!provider) throw new Error(`unknown provider \`${providerId}\``);
    if (typeof args.enabled !== "boolean") throw new Error("provider enabled state is required");
    if (provider.enabled !== args.enabled) {
      const next = structuredClone(currentConfig);
      next.providers.find((candidate) => candidate.id === providerId)!.enabled = args.enabled;
      currentConfig = next;
      sessionStorage.setItem(CONFIG_STORE_KEY, JSON.stringify(currentConfig));
    }
    return traySnapshot();
  },
  tray_set_gateway_running: (args: { running?: boolean }) => {
    if (typeof args.running !== "boolean") throw new Error("gateway running state is required");
    running = args.running;
    return traySnapshot();
  },
  // Representative Windows native accent. The real command may return null;
  // pages must retain their CSS fallback rather than deriving availability
  // from these fixture values.
  get_system_accent: () => ({
    accent: "#0078d4",
    accent_soft: "rgba(0, 120, 212, 0.16)",
    accent_fg: "#ffffff",
  }),
  // One row per local day, oldest last, for the traffic charts. A young
  // install is mostly zero days — the heatmap's honest shape.
  get_usage_days: (args: { days?: number }) => {
    const requested = Number.isFinite(args.days) ? Math.trunc(args.days as number) : 1;
    const days = Math.min(400, Math.max(1, requested));
    const rows: UsageDay[] = [];
    const start = new Date(NOW);
    for (let i = days - 1; i >= 0; i--) {
      const d = new Date(start.getTime() - i * 86_400_000);
      const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
        d.getDate(),
      ).padStart(2, "0")}`;
      rows.push({
        day: key,
        requests: 0,
        failures: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        unpriced_requests: 0,
        cost: {},
      });
    }
    const byDay = new Map(rows.map((row) => [row.day, row]));
    for (const record of recent()) {
      const at = new Date(record.at);
      const key = `${at.getFullYear()}-${String(at.getMonth() + 1).padStart(2, "0")}-${String(
        at.getDate(),
      ).padStart(2, "0")}`;
      const row = byDay.get(key);
      if (!row) continue;
      const usage = observedUsage(record);
      row.requests += 1;
      row.failures += Number(!record.ok);
      row.input_tokens += usage.input_tokens;
      row.output_tokens += usage.output_tokens;
      row.cache_read_tokens += usage.cache_read_tokens;
      row.cache_write_tokens += usage.cache_write_tokens;
      row.unpriced_requests += Number(unpriced(record));
      addKnownCost(row.cost, record);
    }
    return rows;
  },
  query_usage_history: async (args: { query?: UsageHistoryQuery }): Promise<UsageHistoryPage> => {
    const query = args.query ?? {};
    const revision = 1;
    const parseBound = (name: "from" | "until", value: string | null | undefined) => {
      if (value == null) return null;
      const parsed = Date.parse(value);
      if (!Number.isFinite(parsed)) throw new Error(`invalid usage history ${name} timestamp`);
      return parsed;
    };
    const from = parseBound("from", query.from);
    const until = parseBound("until", query.until);
    if (from !== null && until !== null && from >= until) {
      throw new Error("history start must precede its exclusive end");
    }
    for (const value of [query.client_id, query.provider_id, query.model_id]) {
      if (value != null && new TextEncoder().encode(value).byteLength > 512) {
        throw new Error("history filter exceeds 512 bytes");
      }
    }
    if (query.expected_revision != null && query.expected_revision < 0) {
      throw new Error("history revision cannot be negative");
    }
    if (query.expected_revision != null && query.expected_revision !== revision) {
      throw new Error("usage history changed; refresh from the first page");
    }
    const records = recent()
      .filter((record) => {
        const at = Date.parse(record.at);
        return (
          (query.client_id == null || record.client?.client_id === query.client_id) &&
          (query.provider_id == null || record.provider_id === query.provider_id) &&
          (query.model_id == null || record.resolved_model === query.model_id) &&
          (!query.failures_only || !record.ok) &&
          (from === null || at >= from) &&
          (until === null || at < until)
        );
      })
      .sort((left, right) => {
        const byTime = right.at.localeCompare(left.at);
        return byTime !== 0 ? byTime : right.id.localeCompare(left.id);
      });
    const pageSize = Math.min(200, Math.max(1, Math.trunc(query.page_size ?? 50)));
    const lastPage = Math.max(1, Math.ceil(records.length / pageSize));
    const requestedPage = Math.max(1, Math.trunc(query.page ?? 1));
    const page = Math.min(requestedPage, lastPage);
    const offset = (page - 1) * pageSize;
    return {
      records: records.slice(offset, offset + pageSize),
      page,
      page_size: pageSize,
      total: records.length,
      revision,
    };
  },
  get_logs: () => [
    `${ago(4_000)}  INFO zroutery: gateway listening on 127.0.0.1:8787`,
    `${ago(9_000)}  WARN zroutery: upstream 529 from DeepSeek, retrying`,
    `${ago(12_000)}  INFO zroutery: election held, standard -> relay-main:claude-sonnet-4-5`,
  ],

  get_ml_status: () => mlStatusFixture(),
  get_ml_shadow: () => ({
    traces_read: 2_140,
    commit_id: "a91f3c2",
    reason: null,
    analysis: {
      dataset_fingerprint: "ds-7f21",
      records: 2_140,
      decision_records: 1_986,
      prediction_only_records: 154,
      agreements: 1_604,
      disagreements: 382,
      disagreements_measured: 318,
      agreement_rate: 0.834,
      measured_alternative_rate: 0.913,
      alternative_selections: { "dsk:deepseek-chat": 604, "relay-main:glm-4.6": 412 },
      mean_estimated_utility_delta: 0.031,
      mean_observed_utility_delta: 0.044,
      mean_regret: 0.018,
      harmful_alternatives: 27,
      helpful_alternatives: 355,
      gaps: {
        alternative_unmeasured: 64,
        agreement: 1_604,
        no_alternative: 154,
        no_candidates: 318,
      },
      produced_at: NOW_SECS,
    },
  }),
  get_ml_traces: () => ({
    records: 18_402,
    bytes_on_disk: 41_928_012,
    path: "C:\\Users\\operator\\.zroutery\\ml\\traces.log",
    counters: { appended: 18_402, nothing: 312, refused: 9, io_errors: 0 },
    error: null,
  }),
  clear_ml_traces: () => ({ cleared: true, removed_bytes: 41_928_012, error: null }),
  rollback_ml_model: () => ({ outcome: "reloaded", status: mlStatusFixture() }),
  run_ml_promotion_round: () => ({
    round: {
      traces_read: 2_140,
      candidate_commit: "c77be04",
      // Rejected, so the gate panel shows a criterion that did not hold rather
      // than only satisfied ones.
      decision: {
        verdict: "rejected",
        candidate_commit: "c77be04",
        model_id: "ranker-2026-10-16",
        learning_event_count: 2_140,
        dataset_fingerprint: "ds-8820",
        fitted_partition_fingerprint: "fp-1a77",
        holdout_loss: 0.271,
        gate_config_identity: "gate-v3",
        criteria: [
          { name: "baseline_present", held: true, measured: null, threshold: null, reason: "paired against 'ranker-2026-10-15'" },
          { name: "paired_evidence", held: true, measured: 1986, threshold: 1000, reason: "" },
          {
            // The one check the round was rejected on, kept failing so the
            // panel exercises the reason-and-warn-tone branch rather than nine
            // green rows.
            name: "holdout_quality",
            held: false,
            measured: 0.271,
            threshold: 0.05,
            reason: "holdout log loss 0.2710 did not beat an uninformed model by the required 0.0500",
          },
          { name: "routing_utility", held: true, measured: 0.019, threshold: 0.01, reason: "" },
          { name: "selection_safety", held: true, measured: 0, threshold: 0, reason: "" },
          { name: "success_regression", held: true, measured: 6, threshold: 20, reason: "" },
          { name: "cost_regression", held: true, measured: 0.0004, threshold: 0.05, reason: "" },
          { name: "latency_regression", held: true, measured: 22.1, threshold: 250, reason: "" },
          { name: "dataset_identity", held: true, measured: null, threshold: null, reason: "both name source body ds-8820 (fitted partition fp-1a77)" },
        ],
        baseline: "ranker-2026-10-15",
        paired_requests: 1_986,
        decided_at: NOW_SECS,
        revision: "rev-32",
      },
      analysis: null,
      policy_choices: { "dsk:deepseek-chat": 1_220, "relay-main:glm-4.6": 766 },
      installed: null,
      reload: "unchanged",
      reason: null,
      error: null,
    },
    status: mlStatusFixture(),
  }),

  /**
   * All four `AccountSyncOutcome` variants.
   *
   * They render as four different sentences, and a fixture carrying only the
   * happy path leaves three of them unverified — which is how a panel can look
   * finished while the failure wording has never been on screen.
   */
  reconcile_accounts: () => ({
    outcomes: [
      ["relay-main", "acct-0", "refreshed"],
      ["relay-main", "acct-1", { partially_reported: { missing: ["quota"] } }],
      ["dsk", "acct-deep", "unsupported"],
      ["codex", "acct-broken", { failed: { reason: "credential rejected: 401 from /user/balance" } }],
    ],
  }),
  ccswitch_preview: () => ({
    source: "C:\\Users\\operator\\.cc-switch",
    providers: [],
    warning: null,
  }),
  fetch_provider_models: () => [
    {
      id: "deepseek-chat",
      pricing: {
        currency: "USD",
        input_per_mtok: 0.27,
        output_per_mtok: 1.1,
        cache_read_per_mtok: 0.07,
        cache_write_per_mtok: null,
      },
      context: 128_000,
    },
  ],
  /*
   * The takeover page, one state per agent.
   *
   * Four agents, four different truths: `active` for the one already held,
   * `ready` with a real diff for the one that can be adopted now,
   * `not_detected` for the one whose config does not exist, `unsupported` for
   * the one this build does not write. One state repeated four times would
   * render one AgentRow four times and leave three branches unverified — the
   * same trap the reconcile_accounts fixture exists to avoid.
   */
  takeover_status: () => [
    {
      agent: "claude",
      status: {
        state: "active",
        config_path: "C:\\Users\\operator\\.claude\\settings.json",
        generation: 3,
      },
    },
    {
      agent: "codex",
      status: {
        state: "ready",
        config_path: "C:\\Users\\operator\\.codex\\config.toml",
        proposal: {
          managed_fields: ["env.ANTHROPIC_BASE_URL", "env.ANTHROPIC_AUTH_TOKEN"],
          changes: [
            {
              field_path: "env.ANTHROPIC_BASE_URL",
              current: "https://relay.example.com",
              managed: "http://127.0.0.1:8787",
            },
            {
              field_path: "env.ANTHROPIC_AUTH_TOKEN",
              current: "sk-relay-token",
              managed: "zr-mock-gateway-token",
            },
          ],
          fingerprint: "mock-takeover-1",
        },
      },
    },
    {
      agent: "opencode",
      status: {
        state: "not_detected",
        searched: [
          "C:\\Users\\operator\\.config\\opencode\\opencode.json",
          "C:\\Users\\operator\\opencode.json",
        ],
        reason: "no OpenCode configuration found",
      },
    },
    {
      agent: "gemini",
      status: {
        state: "not_detected",
        searched: [
          "C:\\Users\\operator\\.gemini\\settings.json",
          "C:\\Users\\operator\\AppData\\Roaming\\Gemini\\settings.json",
        ],
        reason: "Gemini CLI is not installed on this machine",
      },
    },
  ],
  takeover_adopt: () => ({
    agent: "codex",
    status: {
      state: "active",
      config_path: "C:\\Users\\operator\\.codex\\config.toml",
      generation: 4,
    },
  }),
  takeover_release: () => ({
    agent: "claude",
    report: {
      strategy: "keep_external",
      kept_external: ["env.ANTHROPIC_BASE_URL"],
      overwritten: ["env.ANTHROPIC_AUTH_TOKEN"],
      skipped: [],
    },
    conflicts: [],
    status: {
      state: "released",
      config_path: "C:\\Users\\operator\\.claude\\settings.json",
      generation: 4,
    },
  }),

  /*
   * The cutover page: a settled record, and previews where one agent is
   * blocked so the page's blocked rendering is on screen too.
   */
  migration_status: () => ({
    state: "completed",
    interrupted: false,
    has_snapshot: true,
    snapshot_files: [
      "C:\\Users\\operator\\.zroutery\\migration\\claude.settings.json.bak",
      "C:\\Users\\operator\\.zroutery\\migration\\codex.config.toml.bak",
    ],
    durable: true,
    persist_error: null,
    owned_processes: [],
  }),
  migration_previews: () => [
    {
      agent: "claude",
      config_path: "C:\\Users\\operator\\.claude\\settings.json",
      steps: [
        {
          kind: "stop_external",
          description: "check whether Claude Code is still running",
          reversible: false,
          params: {
            subject: "Claude Code",
            port: null,
            path: null,
            url: null,
          },
        },
        {
          kind: "copy_config",
          description: "install the staged document over settings.json",
          reversible: true,
          params: {
            subject: null,
            port: null,
            path: "C:\\Users\\operator\\.claude\\settings.json",
            url: null,
          },
        },
        {
          kind: "verify_endpoint",
          description: "confirm http://127.0.0.1:8787 answers as Zroutery",
          reversible: false,
          params: {
            subject: null,
            port: null,
            path: null,
            url: "http://127.0.0.1:8787",
          },
        },
      ],
      fingerprint: "mock-migration-claude-1",
      blocked_reason: null,
    },
    {
      agent: "gemini",
      config_path: null,
      steps: [],
      fingerprint: "mock-migration-gemini-1",
      blocked_reason: "no Gemini CLI configuration to cut over",
    },
  ],
  migration_apply: () => ({
    state: "completed",
    steps_completed: 3,
    steps_total: 3,
    errors: [],
    warnings: [],
    duration_ms: 412,
    rolled_back: false,
    restored_files: [],
    failed_restores: [],
  }),
  migration_rollback: () => ({
    state: "rolled_back",
    steps_completed: 3,
    steps_total: 3,
    errors: [],
    warnings: [],
    duration_ms: 261,
    rolled_back: true,
    restored_files: [
      "C:\\Users\\operator\\.claude\\settings.json",
      "C:\\Users\\operator\\.codex\\config.toml",
    ],
    failed_restores: [],
  }),

  // The durable-history commands. export returns a portable JSON archive
  // string (the real backend hands back exactly that, for the textarea);
  // import answers with the whole snapshot like every other mutating
  // command; clearing a client is the same story.
  export_usage_history: () =>
    JSON.stringify(
      { version: 1, exported_at: ago(0), records: recent() },
      null,
      2,
    ),
  // Stands in for a completed native save-dialog choice and durable write. The
  // command accepts no target pathname from the webview, and the file name is
  // what the real dialog pre-fills: zroutery-usage-<local %Y%m%d-%H%M%S>.json.
  export_usage_history_file: () => {
    const stamp = new Date(NOW);
    const two = (n: number) => String(n).padStart(2, "0");
    const name =
      `${stamp.getFullYear()}${two(stamp.getMonth() + 1)}${two(stamp.getDate())}` +
      `-${two(stamp.getHours())}${two(stamp.getMinutes())}${two(stamp.getSeconds())}`;
    return `C:\\Users\\operator\\Downloads\\zroutery-usage-${name}.json`;
  },
  import_usage_history: () => snapshot(),
  clear_client_usage: () => snapshot(),
  reveal_token: () => "zr-0123456789abcdef7c41",
  copy_text: () => null,
  copy_token: () => null,
  tray_panel_ready: () => null,
  hide_tray_panel: () => null,
  show_dashboard: () => null,
  hide_window: () => null,
  quit_app: () => null,
  start_checkin: () => checkin("running", true),
  resume_checkin: () => checkin("running", true),
  cancel_checkin: () => checkin("idle", false),
  get_checkin_status: () => checkin("succeeded", false),
  start_proxy: () => {
    running = true;
    return snapshot();
  },
  stop_proxy: () => {
    running = false;
    return snapshot();
  },
  // Every mutating command answers with the whole snapshot, because that is
  // what the real commands return and the pages redraw from it. save_config
  // commits the config it was handed — a pristine snapshot here would read as
  // "the save landed and reverted your edit", which is a lie about a command
  // the real backend honors.
  clear_stats: () => snapshot(),
  save_config: (args: { config?: AppConfig; expectedRevision?: string }) => {
    const currentRevision = configRevision(currentConfig);
    if (
      args.expectedRevision !== undefined &&
      args.expectedRevision !== currentRevision
    ) {
      throw new Error("configuration changed; refresh before saving");
    }
    if (args.config) {
      currentConfig = structuredClone(args.config);
      // Defensive parity with Rust serde: this bridge-only field is not part of
      // the backend config schema even when a caller bypasses api.saveConfig.
      delete currentConfig.__config_revision;
    }
    sessionStorage.setItem(CONFIG_STORE_KEY, JSON.stringify(currentConfig));
    return snapshot();
  },
  set_provider_enabled: (args: { providerId?: string; enabled?: boolean }) => {
    const providerId = args.providerId ?? "";
    const provider = currentConfig.providers.find((candidate) => candidate.id === providerId);
    if (!provider) throw new Error(`unknown provider \`${providerId}\``);
    if (typeof args.enabled !== "boolean") throw new Error("provider enabled state is required");
    if (provider.enabled === args.enabled) return snapshot();
    // Preserve every existing config field, including fields a newer fixture
    // may add, and mutate only the provider requested by the tray panel.
    const next = structuredClone(currentConfig);
    next.providers.find((candidate) => candidate.id === providerId)!.enabled = args.enabled;
    currentConfig = next;
    sessionStorage.setItem(CONFIG_STORE_KEY, JSON.stringify(currentConfig));
    return snapshot();
  },
  set_provider_key: () => snapshot(),
  clear_provider_key: () => snapshot(),
  remove_provider: () => snapshot(),
  refresh_balance: () => snapshot(),
  refresh_balances: () => snapshot(),
  run_election: () => snapshot(),
  reset_model_health: () => snapshot(),
  ccswitch_import: () => snapshot(),
};

const callbacks = new Map<number, (payload: any) => void>();
let nextCallback = 1;

(window as any).__TAURI_INTERNALS__ = {
  invoke(cmd: string, args?: any): Promise<Json> {
    const handler = handlers[cmd];
    if (!handler) {
      // Loud rather than silent: a command the bridge does not model is a gap
      // in the fixtures, and returning null would let it read as "this build
      // does not have that feature", which is a different claim entirely.
      console.warn("[mock-bridge] no fixture for", cmd, args);
      return Promise.reject(new Error(`mock-bridge: no fixture for ${cmd}`));
    }
    return Promise.resolve(handler(args ?? {}));
  },
  transformCallback(cb: (payload: any) => void): number {
    const id = nextCallback++;
    callbacks.set(id, cb);
    return id;
  },
  unregisterCallback(id: number): void {
    callbacks.delete(id);
  },
  convertFileSrc(path: string): string {
    return path;
  },
};

(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener() {
    // StrictMode mounts effects twice, so this runs while a component is being
    // torn down. The real plugin keeps a set per event name; nothing here
    // reads it, and throwing here would fail the unmount rather than the mount.
  },
};

console.info("[mock-bridge] installed; commands:", Object.keys(handlers).length);
