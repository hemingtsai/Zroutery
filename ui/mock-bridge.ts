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

const quirks = () => ({
  use_max_completion_tokens: false,
  drop_temperature: false,
  drop_top_p: false,
  drop_stop: false,
  stream_usage: true,
  system_as_developer: false,
  send_reasoning_effort: false,
});

const balanceConfig = (preset: string) => ({
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

const maintenance = (enabled: boolean) => ({
  checkin_enabled: enabled,
  checkin_interval_secs: enabled ? 86400 : null,
  checkin_path: enabled ? "/console/checkin" : null,
  checkin_trigger_selector: enabled ? "button.checkin" : null,
  login_path: enabled ? "/console/login" : null,
  browser_executable: "chrome",
});

const capabilities = (over: Partial<Record<string, boolean>> = {}) => ({
  vision: false,
  tools: true,
  thinking: false,
  structured_output: false,
  audio: false,
  video: false,
  files: false,
  ...over,
});

const providers = [
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
    quirks: quirks(),
    balance: balanceConfig("none"),
    accounts: [
      {
        account_id: "acct-0",
        key_ref: "zroutery/account/relay-main/0",
        enabled: true,
        maintenance: maintenance(true),
      },
      {
        account_id: "acct-1",
        key_ref: "zroutery/account/relay-main/1",
        enabled: true,
        maintenance: maintenance(false),
      },
    ],
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
    quirks: { ...quirks(), use_max_completion_tokens: true },
    balance: balanceConfig("none"),
  },
];

interface ModelFixture {
  provider_id: string;
  upstream_model: string;
  tier: string | null;
  priority: number;
  weight: number;
  enabled: boolean;
  capabilities: ReturnType<typeof capabilities>;
  display_name: string | null;
  aliases: string[];
  max_output_tokens: number | null;
  pricing: {
    currency: string;
    input_per_mtok: number;
    output_per_mtok: number;
    cache_read_per_mtok: number | null;
    cache_write_per_mtok: number | null;
  } | null;
}

type ModelSeed = [
  provider_id: string,
  upstream_model: string,
  tier: string | null,
  priority: number,
  weight: number,
];

const models: ModelFixture[] = ([
  ["relay-main", "claude-sonnet-4-5", "standard", 10, 1.0],
  ["relay-main", "claude-opus-4-1", "frontier", 20, 0.6],
  ["relay-main", "claude-haiku-4-5", "fast", 10, 1.4],
  ["relay-main", "glm-4.6", "reasoning", 15, 0.9],
  ["dsk", "deepseek-chat", "standard", 10, 1.2],
  ["dsk", "deepseek-reasoner", "reasoning", 20, 0.8],
  ["codex", "gpt-5-codex", "standard", 10, 1.0],
] as ModelSeed[]).map(([provider_id, upstream_model, tier, priority, weight], i) => ({
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
  pricing: {
    currency: "USD",
    input_per_mtok: i < 4 ? 3 : 0.27,
    output_per_mtok: i < 4 ? 15 : 1.1,
    cache_read_per_mtok: i < 4 ? 0.3 : null,
    cache_write_per_mtok: i < 4 ? 3.75 : null,
  },
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
  pricing: null,
});

const exposed = models.map((m) => `${m.provider_id}:${m.upstream_model}`);

const appConfig = () => ({
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
      builtins: { anthropic_beta: true, xml_classifier_signature: true, model_1m_signature: false },
      signatures: [],
    },
  },
  window: { launch_on_login: false, silent_start: false, keep_in_tray: true },
  vision: { enabled: true, model: "relay-main:claude-sonnet-4-5", placeholder: "[image]" },
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

const recent = () =>
  [0, 1, 2, 3, 4, 5].map((i) => ({
    id: `req_${1000 + i}`,
    at: ago(i * 47_000),
    ingress: "http",
    kind: i === 3 ? "auto_mode" : "main",
    requested_model: i === 3 ? "glm-4.6" : "sonnet",
    resolved_model: exposed[i % 4],
    provider_name: providers[i % 2].name,
    stream: i % 2 === 0,
    // One failure, so the Activity page renders the error styling and the
    // cooldown badge rather than an all-green table.
    status: i === 4 ? 529 : 200,
    ok: i !== 4,
    error: i === 4 ? "upstream overloaded" : null,
    latency_ms: 780 + i * 210,
    ttft_ms: 210 + i * 40,
    usage: {
      input_tokens: 1800 + i * 220,
      output_tokens: 420 + i * 60,
      cache_read_tokens: i % 2 === 0 ? 900 : 0,
      cache_write_tokens: 0,
      reasoning_tokens: i === 3 ? 310 : 0,
    },
    cost: i === 4 ? null : { currency: "USD", amount: 0.0121 + i * 0.002 },
    attempts: i === 4 ? 3 : 1,
  }));

const statsSummary = () => ({
  since: ago(86_400_000),
  requests: 1284,
  failures: 37,
  input_tokens: 4_182_000,
  output_tokens: 921_400,
  cost: { USD: 148.72 },
  per_model: exposed.slice(0, 5).map((model_id, i) => ({
    model_id,
    requests: 420 - i * 40,
    failures: i * 3,
    input_tokens: 1_200_000 - i * 90_000,
    output_tokens: 300_000 - i * 20_000,
    reasoning_tokens: i === 3 ? 40_000 : 0,
    cached_tokens: 220_000,
    cost: { USD: 52.4 - i * 4.1 },
    avg_latency_ms: 940 + i * 60,
  })),
  per_kind: [
    {
      kind: "main",
      requests: 1120,
      failures: 31,
      input_tokens: 3_800_000,
      output_tokens: 860_000,
      avg_latency_ms: 980,
    },
    {
      kind: "auto_mode",
      requests: 164,
      failures: 6,
      input_tokens: 382_000,
      output_tokens: 61_400,
      avg_latency_ms: 610,
    },
  ],
});

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

const snapshot = (): Json => ({
  config: appConfig(),
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
    exposed: true,
  },
  keys: { "relay-main": true, dsk: true, codex: false },
  health: modelHealth(),
  summary: statsSummary(),
  recent: recent(),
  warning: null,
  config_path: "C:\\Users\\operator\\.zroutery\\config.toml",
  version: "0.8.0",
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
  reveal_token: () => "zr-0123456789abcdef7c41",
  copy_text: () => null,
  copy_token: () => null,
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
  // what the real commands return and the pages redraw from it.
  clear_stats: () => snapshot(),
  save_config: () => snapshot(),
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