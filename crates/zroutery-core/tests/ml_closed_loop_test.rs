//! The closed loop, end to end, on real traffic.
//!
//! Everything below drives the production axum server over real HTTP against a
//! real local upstream. Nothing constructs a training sample by hand, nothing
//! injects a decision, and no model artifact is written into the source tree.
//!
//! The four phases are the four claims that have to be true before "Zroutery
//! learns to route" means anything:
//!
//! 1. **Collect.** Real requests produce terminal outcomes, canonical samples,
//!    and durable traces on disk.
//! 2. **Learn.** Those traces train an identified candidate model on a
//!    group-disjoint temporal split, and it is compared against fixed baselines
//!    over the same traces.
//! 3. **Gate.** A promotion decision is taken from that comparison and is
//!    explainable in both directions.
//! 4. **Serve.** A promoted model re-orders a real request's provider plan, and
//!    the provider that actually answers changes.
//!
//! The upstream is a local fake. That is a real limitation and it is stated
//! rather than hidden: what is real here is the router, the pipeline, the
//! features, the outcomes and the loop, and what is simulated is the provider's
//! own behaviour. It is sufficient to prove the loop closes and that learning
//! changes a served decision, and it is not sufficient to claim the learned
//! weights are good for any particular upstream.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};

use zroutery_core::billing::Pricing;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ml::RoutingModel;
use zroutery_core::ml::{
    ActiveModelStore, ExplorationConfig, PromotionConfig, PromotionGate, PromotionVerdict,
    ReplayBaseline,
};
use zroutery_core::server::{AppState, ServerHandle};

/// The upstream model name whose failures are injected.
const FLAKY_UPSTREAM: &str = "flaky";

/// How the two providers behave.
///
/// Two shapes, and the difference between them is why there are two fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    /// `flaky` fails every call and `steady` always succeeds.
    ///
    /// The shipped default strategy spends two attempts on every request and the
    /// model fixes that. But every latency-reading baseline fixes it too: a
    /// provider that only ever fails records no latency at all, so they simply
    /// prefer the one that works. Nothing is left for a learned router to add,
    /// which is exactly what the first fixture demonstrated.
    DeadFlaky,
    /// `flaky` succeeds on one call in four and `steady` always succeeds, and
    /// both record a latency — `flaky`'s fast, because its failures are fast too.
    ///
    /// Now no cheap deterministic signal is sufficient. Every baseline that reads
    /// latency prefers `flaky`, because `flaky` genuinely does answer faster on
    /// the calls where it answers. Only something that has learned the relation
    /// between *observed success rate* and *outcome* can tell that the fast
    /// provider is the unreliable one.
    ///
    /// This is the shape real providers have, and it is the only one in which a
    /// learned router offers something a heuristic does not.
    HalfFlaky,
}

impl Profile {
    /// Whether the `index`-th call to a flaky model fails.
    ///
    /// Deterministic rather than random, so a replay of the same body is the
    /// same body and a comparison is not confounded by luck.
    fn flaky_fails(&self, index: usize) -> bool {
        match self {
            Profile::DeadFlaky => true,
            // One success in four. A coin flip was tried first and is the wrong
            // fixture: at exactly 50% the outcome is unpredictable from any
            // slowly-moving observation, so the feature that identifies the bad
            // provider carries no information about the next call. That measures
            // the limit of the observation, not the value of the model.
            Profile::HalfFlaky => !index.is_multiple_of(4),
        }
    }

    /// Milliseconds the upstream waits, per provider.
    ///
    /// Paid on every attempt, including the ones that fail. That is what makes
    /// `flaky` look fast: it is not slow and then broken, it is fast and
    /// intermittently broken.
    fn latency_ms(&self, model: &str) -> u64 {
        let flaky = model.starts_with(FLAKY_UPSTREAM);
        match self {
            Profile::DeadFlaky if flaky => 1,
            Profile::DeadFlaky => 40,
            Profile::HalfFlaky if flaky => 2,
            Profile::HalfFlaky => 40,
        }
    }
}

#[derive(Clone)]
struct FakeUpstream {
    calls: Arc<Mutex<Vec<String>>>,
    flaky_calls: Arc<AtomicUsize>,
    flaky_failures: Arc<AtomicUsize>,
    profile: Profile,
    /// Per-model call counter, so `HalfFlaky` alternates deterministically.
    counters: Arc<Mutex<std::collections::BTreeMap<String, usize>>>,
}

impl FakeUpstream {
    fn new(profile: Profile) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            flaky_calls: Arc::new(AtomicUsize::new(0)),
            flaky_failures: Arc::new(AtomicUsize::new(0)),
            profile,
            counters: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    /// The most recent call, method, path and model, for a failure message.
    fn last(&self) -> String {
        self.calls().last().cloned().unwrap_or_default()
    }

    fn flaky_calls(&self) -> usize {
        self.flaky_calls.load(Ordering::Relaxed)
    }
}

async fn fake_chat(
    State(upstream): State<FakeUpstream>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> Response {
    let model = body["model"].as_str().unwrap_or("").to_string();
    upstream
        .calls
        .lock()
        .expect("calls")
        .push(format!("{method} {uri} {model}"));

    let index = {
        let mut counters = upstream.counters.lock().expect("counters");
        let entry = counters.entry(model.clone()).or_insert(0);
        let current = *entry;
        *entry += 1;
        current
    };

    let latency = upstream.profile.latency_ms(&model);
    tokio::time::sleep(std::time::Duration::from_millis(latency)).await;

    if model.starts_with(FLAKY_UPSTREAM) {
        upstream.flaky_calls.fetch_add(1, Ordering::Relaxed);
        if upstream.profile.flaky_fails(index) {
            upstream.flaky_failures.fetch_add(1, Ordering::Relaxed);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {"message": "this upstream is unhealthy", "type": "server_error"}
                })),
            )
                .into_response();
        }
    }

    Json(json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "answered"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 20, "completion_tokens": 5}
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const TOKEN: &str = "zr-loop-token";

/// Requests driven in the collection phase.
///
/// Large enough that the training split has a holdout and the paired comparison
/// clears its evidence floor of 30 paired requests; small enough to stay a unit
/// of test time rather than a benchmark.
const COLLECTION_REQUESTS: usize = 120;

fn provider(id: &str, name: &str, upstream: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{upstream}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

fn model(provider_id: &str, upstream: &str, priority: i32, tier: ModelTier) -> ModelEntry {
    let mut entry =
        ModelEntry::for_upstream(provider_id, upstream, Some(tier)).with_priority(priority);
    entry.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    entry
}

/// One tier, two candidates, and priority is the whole deterministic strategy.
///
/// `flaky` sits at priority 5 and `steady` at 10, so `Priority` — the shipped
/// default strategy — always tries `flaky` first, fails, and falls back to
/// `steady`. Every request therefore costs two upstream calls and every request
/// produces evidence about both candidates. Nothing here is arranged to flatter
/// the learned model: the deterministic plan is already right about the outcome,
/// and the only thing available to improve is the *number of attempts*.
fn config_for(upstream: SocketAddr, state_dir: &std::path::Path, ml_enabled: bool) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![
        provider("alpha", "Alpha", upstream),
        provider("beta", "Beta", upstream),
    ];
    cfg.models = vec![
        model("alpha", "flaky-std", 5, ModelTier::Standard),
        model("alpha", "steady-std", 10, ModelTier::Standard),
    ];
    // Nothing may be quarantined mid-window: a mid-window regime change would
    // make the collection phase a mixture of two experiments.
    cfg.routing.circuit_breaker.failure_threshold = 1000;
    cfg.routing.circuit_breaker.min_requests = 1000;
    // Shadow evidence and durable history both come from the same terminal
    // transition, so the trace log is only written where the decision-time
    // snapshot is retained.
    cfg.shadow.enabled = true;
    cfg.ml_routing.enabled = ml_enabled;
    cfg.ml_routing.state_dir = state_dir.display().to_string();
    cfg.ml_routing.exploration_probability = 0.0;
    cfg
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    state: Arc<AppState>,
    server: ServerHandle,
    client: reqwest::Client,
    upstream: FakeUpstream,
}

impl Harness {
    async fn start(config: AppConfig, upstream: FakeUpstream) -> Harness {
        let secrets = Arc::new(
            MemorySecretStore::new()
                .with("provider:alpha", "sk-alpha")
                .with("provider:beta", "sk-beta"),
        );
        let state = Arc::new(AppState::new(config, secrets));
        let server = ServerHandle::start(Arc::clone(&state))
            .await
            .expect("server");
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("client");
        Harness {
            state,
            server,
            client,
            upstream,
        }
    }

    /// Drive one class-routed request.
    ///
    /// Returns the status and the number of upstream calls the request actually
    /// cost, counted from the fake upstream's own log around the call. "Did the
    /// router try twice" is therefore measured at the provider, not inferred
    /// from the router's own bookkeeping — which is the only way the claim
    /// "the learned plan stopped spending an attempt on the failing provider"
    /// can be worth anything.
    async fn ask(&self) -> (u16, usize) {
        let before = self.upstream.calls().len();
        let response = self
            .client
            .post(format!("http://{}/v1/messages", self.server.addr))
            .header("x-api-key", TOKEN)
            .json(&json!({
                // The tier's virtual id, so the request is class-routed and
                // carries a routing decision. A direct model id would resolve
                // straight to one candidate and produce no decision to learn
                // from.
                "model": "standard-class",
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "route this"}],
            }))
            .send()
            .await
            .expect("request");
        let after = self.upstream.calls().len();
        let status = response.status().as_u16();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            panic!(
                "request failed with {status}: {body}\nlast upstream call: {}",
                self.upstream.last()
            );
        }
        (status, after - before)
    }

    /// Drive `count` requests and return the upstream cost of each.
    async fn drive(&self, count: usize) -> Vec<usize> {
        let mut costs = Vec::with_capacity(count);
        for index in 0..count {
            let (status, cost) = self.ask().await;
            assert_eq!(status, 200, "request {index} did not succeed");
            costs.push(cost);
        }
        costs
    }
}

/// Start the fake upstream and return its handle and address.
async fn start_upstream(profile: Profile) -> (FakeUpstream, SocketAddr) {
    let upstream = FakeUpstream::new(profile);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let fake = upstream.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .route("/chat/completions", post(fake_chat))
                .route("/v1/chat/completions", post(fake_chat))
                .with_state(fake),
        )
        .await
        .expect("serve");
    });
    (upstream, addr)
}

// ---------------------------------------------------------------------------
// Gate ML-G: the online loop, across a real process boundary
// ---------------------------------------------------------------------------

/// Collect → shut down → retrain → gate → promote → serve → collect again →
/// retrain again, with each "process" a separate `AppState` over the same state
/// directory.
///
/// This is the only test that exercises the whole lifecycle, and the one thing
/// it is built to prove is that **a new process's outcomes reach the next round
/// of learning**. Everything else in this file proves one link; this proves the
/// chain, including the boundary where the previous work was silent — the
/// durable trace log and the active-model pointer are the only things that
/// cross it, so if either were in-memory-only the second round would retrain on
/// the first round's data and be indistinguishable from doing nothing.
///
/// `Profile::HalfFlaky` is required. Under `DeadFlaky` the gate correctly
/// refuses the model against the strongest baseline, and a refused model serves
/// nothing, so round two would collect the same deterministic traffic and there
/// would be no new outcome to learn from. The loop has to be demonstrated on a
/// body where the model is genuinely promotable, and on one where it is not the
/// gate — not the harness — stops the loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_outcomes_from_a_new_process_reach_the_next_round_of_learning() {
    let (upstream, addr) = start_upstream(Profile::HalfFlaky).await;
    let state_dir = tempfile::tempdir().expect("tempdir");
    let round_one_requests = 80;
    let round_two_requests = 40;

    // ================= round 1: collect, deterministically ==================
    {
        let harness =
            Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
        // Priority puts `flaky` first and it fails three times in four, so most
        // requests cost a second attempt.
        let costs = harness.drive(round_one_requests).await;
        let fallbacks = costs.iter().filter(|cost| **cost == 2).count();
        assert!(
            fallbacks > round_one_requests / 2,
            "the deterministic plan should fall back on most requests, got \
             {fallbacks} of {round_one_requests}"
        );
        assert_eq!(
            harness
                .state
                .traces()
                .expect("the fixture configures a state directory")
                .counters()
                .appended,
            round_one_requests as u64,
            "every round-one request must be durable"
        );
    }
    let round_one_flaky_calls = upstream.flaky_calls();
    assert_eq!(
        round_one_flaky_calls, round_one_requests,
        "every round-one request should have gone to the flaky model first"
    );

    // The first process is gone. Nothing survives in memory: the next
    // `AppState` is built from the state directory and nothing else, which is
    // the boundary under test.

    // ================= round 1: learn and gate =============================
    let round_one = loop_over(state_dir);
    let round_one_traces = round_one.traces.len();
    let round_one_body = round_one.training.report.source_fingerprint.clone();
    let round_one_commit = round_one.training.report.final_commit.clone();

    // Half-flaky is the shape where a learned router should win, and this is
    // the assertion that justifies the rest of the test: every baseline that
    // reads latency prefers the *fast* provider, and it fails three times in
    // four. Only something that learned the relation between observed success
    // rate and outcome can tell that the fast provider is the unreliable one.
    let lowest = round_one
        .comparison
        .arm("baseline.lowest_latency")
        .expect("the lowest-latency arm");
    let candidate = round_one
        .comparison
        .arm("ml.candidate")
        .expect("the candidate arm");
    assert!(
        lowest.success_rate < 0.5,
        "the latency baseline should be fooled by the fast unreliable \
         provider, got {}",
        lowest.success_rate
    );
    assert!(
        candidate.success_rate > 0.9,
        "the learned candidate should route around it, got {}",
        candidate.success_rate
    );
    let pairing = round_one
        .comparison
        .pairing("baseline.lowest_latency")
        .expect("a pairing against the strongest baseline");
    assert!(
        pairing.deltas.mean_observed_utility_delta > 1.0,
        "expected a decisive paired improvement, got {}",
        pairing.deltas.mean_observed_utility_delta
    );
    assert_eq!(pairing.deltas.candidate_regressions, 0);

    // The gate's shipped default names `baseline.lowest_latency`, so this run
    // is promoted by the default configuration, not by a relaxed one.
    let gate = PromotionGate::new(PromotionConfig::default());
    let round_one_decision = gate.evaluate(
        &round_one.training.report,
        &round_one.comparison,
        Some("round-1".into()),
    );
    assert_eq!(
        round_one_decision.verdict,
        PromotionVerdict::Promoted,
        "round one should be promotable on its own evidence: {:?}",
        round_one_decision
            .blockers()
            .into_iter()
            .map(|c| format!("{} ({})", c.name, c.reason))
            .collect::<Vec<_>>()
    );

    // ================= round 1: promote, durably ==========================
    {
        let store = ActiveModelStore::open(round_one.state_dir.path()).expect("store");
        store
            .promote(&round_one_decision, round_one.training.checkpoint.clone())
            .expect("promote");
    }

    // ================= round 2: a NEW process, model attached =============
    let costs = {
        let mut config = config_for(addr, round_one.state_dir.path(), true);
        config.ml_routing.exploration_probability = 0.0;
        let harness = Harness::start(config, upstream.clone()).await;
        assert!(
            harness.state.ml_routing().is_attached(),
            "a fresh AppState must pick up the promoted model up from the state \
             directory; if this fails, promotion did not survive the restart"
        );
        assert_eq!(
            harness.state.ml_routing().counts().fallbacks,
            0,
            "attaching a verified model should not fall back"
        );

        let costs = harness.drive(round_two_requests).await;
        assert_eq!(
            harness
                .state
                .traces()
                .expect("the fixture configures a state directory")
                .counters()
                .appended,
            round_two_requests as u64,
            "round two's outcomes must be durable too"
        );

        // The learned plan avoids the unreliable provider, so round two costs
        // about one attempt per request where round one cost two on most of
        // them.
        //
        // A fresh process starts with an empty observation store, so the first
        // request or two have no measured history to rank on and the
        // DecisionEngine's switch threshold holds the model on the production
        // pick. That is the cold-start cost of the mechanism. It is asserted as
        // a bounded prefix rather than waved away, and bounded strictly: once
        // the process has observations, nothing may fall back at all.
        let mean: f64 = costs.iter().sum::<usize>() as f64 / costs.len() as f64;
        assert!(
            mean < 1.15,
            "the learned plan averaged {mean} attempts per request over {costs:?}"
        );
        let late = costs
            .iter()
            .enumerate()
            .skip(3)
            .filter(|(_, cost)| **cost != 1)
            .count();
        assert_eq!(
            late, 0,
            "once the process has observations every request should answer \
             first time; got {costs:?}"
        );
        costs
    };

    // ================= round 2: retrain on the combined body ==============
    //
    // This is the assertion the whole test exists for: the body round two trains
    // on contains round two's requests, which only exist because the previous
    // process served them.
    let round_two_dir = round_one.state_dir.path().to_path_buf();
    let round_two = loop_over_keep(round_two_dir);
    assert_eq!(
        round_two.traces.len(),
        round_one_traces + round_two_requests,
        "the durable log must hold both rounds"
    );
    assert_ne!(
        round_two.training.report.source_fingerprint, round_one_body,
        "round two must be fitted on a body round one never saw"
    );
    assert!(
        round_two.training.report.sample_count > round_one.training.report.sample_count,
        "the body grew, so the sample count must have grown with it"
    );
    assert!(
        round_two.training.report.request_count > round_one.training.report.request_count,
        "round two's own requests must be in the body"
    );

    // The new traces are genuinely new, not a replay of the first round.
    let round_one_ids: std::collections::BTreeSet<String> = round_one
        .traces
        .iter()
        .map(|trace| trace.request_id.clone())
        .collect();
    let fresh = round_two
        .traces
        .iter()
        .filter(|trace| !round_one_ids.contains(&trace.request_id))
        .count();
    assert_eq!(
        fresh, round_two_requests,
        "every round-two trace must be new"
    );

    // A trace records the *production* decision as it was made, before any
    // model consulted it. That is what makes it a shadow record: the shadow's
    // choice is compared against exactly this. So `production_selected` is the
    // deterministic plan's pick in both rounds, and asserting it changed would
    // be asserting the shadow record stopped recording production.
    assert!(
        round_two
            .traces
            .iter()
            .skip(round_one_traces)
            .all(|trace| trace.input.production_selected == "alpha-flaky-std"),
        "every trace should record the deterministic plan's pick"
    );

    // What changed is what the provider actually received, and that is measured
    // at the provider rather than inferred from the router's own bookkeeping.
    // In round one every request went to the flaky model first. In round two the
    // promoted model routes around it, so it is barely called at all.
    let flaky_in_round_two = upstream.flaky_calls() - round_one_flaky_calls;
    assert!(
        flaky_in_round_two <= 3,
        "round two called the unreliable provider {flaky_in_round_two} times \
         for {round_two_requests} requests; the promoted model did not change \
         which provider served"
    );
    assert!(
        costs.iter().skip(3).all(|cost| *cost == 1),
        "and every request after cold start was served without a fallback"
    );

    // ================= round 2: gate again ================================
    let round_two_decision = gate.evaluate(
        &round_two.training.report,
        &round_two.comparison,
        Some("round-2".into()),
    );
    println!(
        "round 1: {} requests, commit {}, verdict {}",
        round_one.training.report.request_count,
        round_one_commit,
        round_one_decision.verdict.as_str()
    );
    println!(
        "round 2: {} requests, commit {}, verdict {}",
        round_two.training.report.request_count,
        round_two.training.report.final_commit,
        round_two_decision.verdict.as_str()
    );

    assert_eq!(
        round_two_decision.verdict,
        PromotionVerdict::Promoted,
        "round two should still be promotable: {:?}",
        round_two_decision
            .blockers()
            .into_iter()
            .map(|c| format!("{} ({})", c.name, c.reason))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        round_two_decision.dataset_fingerprint.as_str(),
        round_two.training.report.source_fingerprint.as_str(),
        "the decision must name the body round two was judged on"
    );
    assert_ne!(
        round_two.training.report.final_commit, round_one_commit,
        "a larger body must produce a different model"
    );

    // ================= round 2: promote over round 1, then roll back ======
    let store = ActiveModelStore::open(round_two.state_dir.path()).expect("store");
    store
        .promote(&round_two_decision, round_two.training.checkpoint.clone())
        .expect("promote");
    assert_eq!(
        store.active_identity().expect("read").as_deref(),
        Some(round_two.training.report.final_commit.as_str())
    );
    assert!(
        store.rollback().expect("rollback"),
        "round one's model is the rollback target"
    );
    assert_eq!(
        store.active_identity().expect("read").as_deref(),
        Some(round_one_commit.as_str()),
        "rollback should return to round one's model"
    );
    let audit = store.audit().expect("audit");
    assert_eq!(audit.len(), 3, "two promotions and one rollback");
    assert!(audit
        .iter()
        .filter(|entry| entry.action == zroutery_core::ml::ActiveModelAction::Promote)
        .all(|entry| entry.verdict == PromotionVerdict::Promoted));

    drop(round_one);
    drop(round_two);
}

// ---------------------------------------------------------------------------
// Durable state is opt-in
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_configured_state_directory_there_is_no_durable_ml_state() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    // The default configuration: shadow on, ML routing off, no state directory.
    let mut config = config_for(addr, std::path::Path::new("unused"), false);
    config.ml_routing.state_dir = String::new();
    assert!(
        !config.ml_routing.has_state_dir(),
        "the default must not name a directory"
    );

    let harness = Harness::start(config, upstream.clone()).await;
    assert!(
        harness.state.traces().is_none(),
        "no state directory means no trace log"
    );
    assert!(
        harness.state.active_models().is_none(),
        "no state directory means no model store, so no model can serve"
    );
    assert!(!harness.state.ml_routing().is_attached());

    // The request path is unaffected: outcomes, samples and the shadow all
    // still work, because none of them need durability.
    let costs = harness.drive(20).await;
    assert!(costs.iter().all(|cost| *cost == 2));
    assert_eq!(
        harness.state.dataset().counters().ingested,
        20,
        "the in-memory dataset still collects without a state directory"
    );
    assert_eq!(harness.state.outcomes().len(), 20);
}

// ---------------------------------------------------------------------------
// Phase 1: real requests produce durable history
// ---------------------------------------------------------------------------

fn histogram(costs: &[usize]) -> String {
    let mut counts: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for cost in costs {
        *counts.entry(*cost).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(cost, count)| format!("{cost} attempt(s) x{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_requests_produce_outcomes_samples_and_durable_traces() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    let costs = harness.drive(COLLECTION_REQUESTS).await;

    // Every request took the deterministic path: try `flaky`, fail, fall back.
    // Two upstream calls each, and the flaky one is genuinely being called.
    assert!(
        costs.iter().all(|cost| *cost == 2),
        "every request should cost two upstream attempts, got {costs:?}"
    );
    assert_eq!(upstream.flaky_calls(), COLLECTION_REQUESTS);

    // The in-memory dataset received the request.
    assert_eq!(
        harness.state.dataset().counters().ingested,
        COLLECTION_REQUESTS as u64,
        "the dataset did not receive every request"
    );

    // And the durable log has them too. This is the part that did not exist
    // before: without it there is no history to learn from across restarts.
    let traces = zroutery_core::ml::TraceLog::open(state_dir.path())
        .expect("reopen")
        .load()
        .expect("load");
    assert_eq!(
        traces.len(),
        COLLECTION_REQUESTS,
        "the durable trace log lost records"
    );
    let counters = harness
        .state
        .traces()
        .expect("the fixture configures a state directory")
        .counters();
    assert_eq!(counters.appended, COLLECTION_REQUESTS as u64);
    assert_eq!(counters.refused, 0);
    assert_eq!(counters.io_errors, 0);

    // Each trace carries the counterfactual surface a comparison needs: which
    // candidates were on the table, and which of them were eligible.
    let first = &traces[0];
    assert!(first.input.candidates.len() >= 2);
    assert!(first
        .input
        .candidates
        .iter()
        .any(|candidate| candidate.eligible));
    // The exposed id is provider-qualified, which is how the router, the
    // decision trace and the outcome all key the same candidate.
    assert_eq!(first.input.production_selected, "alpha-flaky-std");
    assert!(first
        .input
        .candidates
        .iter()
        .any(|candidate| candidate.candidate_id == "alpha-steady-std" && candidate.eligible));
}

// ---------------------------------------------------------------------------
// Phase 2: the traces train a model and compare it against baselines
// ---------------------------------------------------------------------------

/// Load the collected traces and train, compare and analyse them.
///
/// Returns the pieces the later phases need. Split out so each phase's test can
/// assert on one claim without repeating the whole loop.
struct LoopArtefacts {
    state_dir: tempfile::TempDir,
    traces: Vec<zroutery_core::ml::RequestTrace>,
    training: zroutery_core::ml::TrainingOutcome,
    comparison: zroutery_core::ml::RoutingComparison,
    analysis: zroutery_core::ml::ShadowAnalysis,
}

/// The offline half of the loop over an existing state directory, leaving the
/// directory in place.
///
/// [`loop_over`] takes ownership because most tests are done with it afterwards.
/// This one is not: a second round has to read the same directory the first
/// round wrote, and dropping the `TempDir` would delete it.
fn loop_over_keep(state_dir: std::path::PathBuf) -> LoopArtefacts {
    loop_over(copy_state_dir(&state_dir))
}

/// A private `TempDir` seeded from a directory that already exists.
///
/// `TempDir` deletes on drop, which is the right default and the wrong behaviour
/// for a second round over the first round's output. Rather than reach for
/// `TempDir::into_path` and leak the directory, the existing one is copied: the
/// test keeps its own isolation and nothing is left behind.
fn copy_state_dir(source: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for entry in std::fs::read_dir(source).expect("read state dir") {
        let entry = entry.expect("entry");
        let from = entry.path();
        let to = dir.path().join(entry.file_name());
        if from.is_dir() {
            let _ = std::fs::create_dir_all(&to);
        } else {
            std::fs::copy(&from, &to).expect("copy state file");
        }
    }
    dir
}

/// The offline half of the loop over an existing state directory, leaving the
/// directory in place.
///
/// Like `learn` in `ml_multi_provider_test.rs`, this used to assemble the round by
/// hand. It now calls `ml::run_promotion_round` and reads the artefacts off the
/// result, so the two fixtures that between them proved the loop no longer keep
/// private copies of the spine — and so a future entry point would run this same
/// code rather than a third version of it.
fn loop_over(state_dir: tempfile::TempDir) -> LoopArtefacts {
    let round = zroutery_core::ml::run_promotion_round(
        state_dir.path(),
        &zroutery_core::ml::RoundConfig::default(),
        None,
    )
    .expect("a round over a body this test just served");

    let traces = zroutery_core::ml::TraceLog::open(state_dir.path())
        .expect("reopen")
        .load()
        .expect("load");
    assert!(!traces.is_empty(), "there is no history to learn from");

    LoopArtefacts {
        state_dir,
        traces,
        training: round.training,
        comparison: round.comparison,
        analysis: round.analysis,
    }
}

/// A gate configuration this fixture's evidence genuinely satisfies.
///
/// The shipped default names `baseline.lowest_latency`, and against that
/// baseline the learned candidate loses one request in 120, so the default gate
/// returns REJECTED — correctly. `baseline.priority` is the strategy Zroutery
/// ships by default, against which the same candidate is much better.
///
/// Naming the baseline explicitly is the point: "beats a baseline" is a claim
/// about a specific baseline, and a promotion that quietly picked whichever one
/// promoted would prove nothing about the gate.
fn promotable_gate() -> PromotionConfig {
    PromotionConfig {
        required_baseline: "baseline.priority".to_string(),
        ..PromotionConfig::default()
    }
}

/// A training report that names a given commit while keeping everything else
/// the fixture produced.
///
/// Used where a test needs a decision about a *different* model than the one
/// `run_training` produced: the evidence stays the same, only the identity
/// under judgement changes.
fn report_for(commit: &str, artefacts: &LoopArtefacts) -> zroutery_core::ml::TrainingReport {
    let mut report = artefacts.training.report.clone();
    report.final_commit = commit.to_string();
    report
}

/// Promote the trained model, refusing to proceed if the gate did not say so.
fn promote_trained(
    store: &ActiveModelStore,
    artefacts: &LoopArtefacts,
    config: PromotionConfig,
) -> zroutery_core::ml::PromotionDecision {
    let decision = PromotionGate::new(config).evaluate(
        &artefacts.training.report,
        &artefacts.comparison,
        Some("loop-test".into()),
    );
    assert_eq!(
        decision.verdict,
        PromotionVerdict::Promoted,
        "this fixture is supposed to promote; the gate said {}: {:?}",
        decision.verdict.as_str(),
        decision
            .blockers()
            .into_iter()
            .map(|c| format!("{} ({})", c.name, c.reason))
            .collect::<Vec<_>>()
    );
    store
        .promote(&decision, artefacts.training.checkpoint.clone())
        .expect("a promoted decision installs its model");
    decision
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collected_traces_train_an_identified_model_that_beats_the_deterministic_plan() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    harness.drive(COLLECTION_REQUESTS).await;
    drop(harness);

    let artefacts = loop_over(state_dir);

    // -- The model is identified, and identified reproducibly. ---------------
    let report = &artefacts.training.report;
    assert!(report.sample_count > 0);
    assert_eq!(
        report.train_size + report.validation_size + report.holdout_size,
        report.sample_count,
        "the split dropped samples"
    );
    assert!(
        report.holdout_size > 0,
        "there was no frozen holdout to judge on"
    );
    assert!(report.holdout_loss.is_finite());
    assert!(
        report.holdout_loss < std::f64::consts::LN_2,
        "an untrained model scores ln(2); this one scored {}",
        report.holdout_loss
    );
    assert!(!report.final_commit.is_empty());
    assert!(!report.dataset_fingerprint.as_str().is_empty());

    // -- The comparison is over the same body, and is routing-level. ----------
    assert_eq!(artefacts.comparison.traces, artefacts.traces.len());
    // The comparison runs over the whole body the model was trained *from*; the
    // fitted partition is a strictly smaller set and is named separately.
    assert_eq!(
        artefacts.comparison.dataset_fingerprint.as_str(),
        report.source_fingerprint.as_str(),
        "the comparison and the model were computed over different data"
    );
    assert_ne!(
        report.dataset_fingerprint.as_str(),
        report.source_fingerprint.as_str(),
        "the fitted partition should not be the whole body"
    );
    assert_eq!(
        artefacts.comparison.arms.len(),
        ReplayBaseline::ALL.len() + 1
    );

    // Every arm is measured, and each is measured over a body with real
    // coverage rather than over a handful of lucky requests.
    for arm in &artefacts.comparison.arms {
        assert!(
            arm.requests_measured > 0,
            "arm {} measured nothing",
            arm.policy
        );
        assert!(
            arm.coverage > 0.5,
            "arm {} had only {:.0}% coverage",
            arm.policy,
            arm.coverage * 100.0
        );
    }

    // The deterministic plan is what the learned candidate has to beat, and in
    // this fixture it is beatable for one specific, checkable reason: it always
    // spends an attempt on a provider that fails.
    let deterministic = artefacts
        .comparison
        .arm("ml.candidate")
        .expect("the candidate arm");
    assert!(
        deterministic.success_rate > 0.0,
        "the candidate measured no successful routing at all"
    );

    let priority_pairing = artefacts
        .comparison
        .pairing("baseline.priority")
        .expect("a pairing against the shipped default strategy");
    assert_eq!(
        priority_pairing.deltas.paired_requests, COLLECTION_REQUESTS,
        "the pairing should cover every collected request"
    );
    assert_eq!(priority_pairing.deltas.candidate_regressions, 0);
    assert!(
        priority_pairing.deltas.candidate_improvements > 0,
        "the learned candidate never picked a candidate the baseline had not already beaten"
    );

    // The concrete, checkable improvement in this fixture: the shipped default
    // strategy always tries `flaky` first, which always fails, so every request
    // costs a fallback. The learned candidate does not.
    let priority_arm = artefacts
        .comparison
        .arm("baseline.priority")
        .expect("the priority arm");
    let candidate_arm = artefacts
        .comparison
        .arm("ml.candidate")
        .expect("the candidate arm");
    assert!(
        (priority_arm.fallback_rate - candidate_arm.fallback_rate) > 0.9,
        "expected the learned candidate to avoid nearly all fallbacks; \
         baseline {} vs candidate {}",
        priority_arm.fallback_rate,
        candidate_arm.fallback_rate
    );
    assert!(
        candidate_arm.success_rate > priority_arm.success_rate,
        "expected the learned candidate to raise the success rate; \
         baseline {} vs candidate {}",
        priority_arm.success_rate,
        candidate_arm.success_rate
    );

    // -- The shadow analysis is decision-shaped and measurable. --------------
    assert!(artefacts.analysis.decision_records > 0);
    assert!(!artefacts.analysis.is_evaluable() || artefacts.analysis.disagreements > 0);
    assert_eq!(
        artefacts.analysis.dataset_fingerprint.as_str(),
        report.source_fingerprint.as_str()
    );
}

// ---------------------------------------------------------------------------
// Phase 3: the gate refuses, explains, and can promote
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promotion_decision_is_explainable_and_a_promoted_model_becomes_the_active_one() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    harness.drive(COLLECTION_REQUESTS).await;
    drop(harness);

    let artefacts = loop_over(state_dir);
    let gate = PromotionGate::new(PromotionConfig::default());
    let decision = gate.evaluate(&artefacts.training.report, &artefacts.comparison, None);

    // Whatever the verdict, it names the criterion that produced it and records
    // every identity a reader needs to re-derive it.
    assert!(!decision.criteria.is_empty());
    assert_eq!(
        decision.candidate_commit,
        artefacts.training.report.final_commit
    );
    assert_eq!(
        decision.dataset_fingerprint.as_str(),
        artefacts.training.report.source_fingerprint.as_str()
    );
    assert_eq!(
        decision.fitted_partition_fingerprint.as_str(),
        artefacts.training.report.dataset_fingerprint.as_str()
    );
    assert!(!decision.gate_config_identity.is_empty());
    if decision.verdict != PromotionVerdict::Promoted {
        for blocker in decision.blockers() {
            assert!(
                !blocker.reason.is_empty(),
                "blocker {} carries no reason",
                blocker.name
            );
        }
    }

    // -- Promotion is refused when the evidence cannot support it. -----------
    let impossible = PromotionConfig {
        min_utility_delta: 100.0,
        ..PromotionConfig::default()
    };
    let refused = PromotionGate::new(impossible).evaluate(
        &artefacts.training.report,
        &artefacts.comparison,
        None,
    );
    assert_eq!(refused.verdict, PromotionVerdict::Rejected);
    assert!(refused
        .blockers()
        .iter()
        .any(|criterion| criterion.name == "routing_utility"));

    // -- A promoted model becomes the active model, durably. -----------------
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    assert!(
        store.active().expect("read").is_none(),
        "a fresh install has none"
    );

    // The rejected decision cannot install anything, and says why.
    let refusal = store
        .promote(&decision, artefacts.training.checkpoint.clone())
        .expect_err("a rejected decision must not install a model");
    assert!(refusal.to_string().contains("REJECTED"), "{refusal}");
    assert!(store.active().expect("read").is_none());

    // The same evidence against the baseline the fixture can actually beat
    // promotes, and the store re-derives the commit rather than trusting it.
    let promotable = promote_trained(&store, &artefacts, promotable_gate());
    let commit = promotable.candidate_commit.clone();
    assert_eq!(
        store.active_identity().expect("read").as_deref(),
        Some(commit.as_str())
    );

    let reloaded = store.active().expect("read").expect("an active model");
    assert_eq!(reloaded.commit_id().as_str(), commit);
    assert_eq!(store.audit().expect("audit").len(), 1);
}

// ---------------------------------------------------------------------------
// Phase 4: a promoted model re-orders a real request, and rollback undoes it
// ---------------------------------------------------------------------------

/// A second, different promotable model, so a rollback has somewhere to go.
///
/// Built from the same evidence as `artefacts` but with different weights, which
/// is what makes it a *different* model rather than a copy: it gets its own
/// commit id, and promoting it moves the pointer away from the trained one.
///
/// Used by the offline gate tests, which hand the gate two models, and by the
/// operator-surface tests, which need a live rollback to have somewhere to land.
fn second_model(artefacts: &LoopArtefacts, store: &ActiveModelStore, label: &str) -> String {
    let mut other = zroutery_core::ml::model_identity::ModelEnsemble::new();
    other.success.update(
        &zroutery_core::ml::features::RoutingFeatures {
            schema_version: zroutery_core::ml::features::FEATURE_SCHEMA_VERSION,
            values: {
                let mut values = [zroutery_core::ml::features::UNKNOWN;
                    zroutery_core::ml::features::FEATURE_DIMENSION];
                values[0] = -0.75;
                values
            },
        },
        1.0,
    );
    let other_checkpoint = other.save_all();
    let other_record = zroutery_core::ml::model_identity::ModelCommit::new(
        zroutery_core::ml::model_identity::ModelId::new(artefacts.training.model_id.clone()),
        other_checkpoint.clone(),
        None,
        artefacts.training.report.learning_event_count,
    );

    // The second model needs a gate decision naming *it*. The gate does not
    // choose between models; it only says whether the one it is handed may
    // serve.
    let other_decision = PromotionGate::new(promotable_gate()).evaluate(
        &report_for(&other_record.commit_id.to_string(), artefacts),
        &artefacts.comparison,
        Some(label.into()),
    );
    assert_eq!(
        other_decision.verdict,
        PromotionVerdict::Promoted,
        "the second model should also be promotable; the gate said {}",
        other_decision.verdict.as_str()
    );
    store
        .promote(&other_decision, other_checkpoint)
        .expect("a second promoted model installs");
    other_record.commit_id.to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promoted_model_changes_which_provider_serves_and_rollback_restores_the_plan() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");

    // -- Collect, deterministically. -----------------------------------------
    {
        let harness =
            Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
        let costs = harness.drive(40).await;
        assert!(
            costs.iter().all(|cost| *cost == 2),
            "the deterministic plan should cost two attempts per request"
        );
    }
    let flaky_before = upstream.flaky_calls();

    // -- Learn from what was collected. --------------------------------------
    let artefacts = loop_over(state_dir);

    // -- Promote, on a gate decision that actually says so. -------------------
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    let first_commit = promote_trained(&store, &artefacts, promotable_gate()).candidate_commit;
    let state_dir = artefacts.state_dir.path().to_path_buf();

    // -- Serve with the model attached. --------------------------------------
    {
        let mut config = config_for(addr, &state_dir, true);
        // Exploration off: this phase is about the learned ranking, and an
        // exploration draw would make the outcome a coin flip that says nothing
        // about whether the model learned anything.
        config.ml_routing.exploration_probability = 0.0;
        let harness = Harness::start(config, upstream.clone()).await;

        assert!(
            harness.state.ml_routing().is_attached(),
            "a promoted model should be attached when ml_routing.enabled is on"
        );

        let costs = harness.drive(40).await;
        let learned_costs: Vec<usize> = costs.clone();

        // The model was consulted on every request.
        let counts = harness.state.ml_routing().counts();
        assert!(counts.rankings > 0, "the model was never asked to rank");
        assert_eq!(
            counts.fallbacks, 0,
            "ranking should not have fallen back on this fixture"
        );

        // And the point of the whole exercise: the learned plan costs one attempt per
        // request where the deterministic plan cost two, because it stopped
        // trying the provider that always fails.
        //
        // The first request is allowed to cost two. A fresh process has an empty
        // observation store, so the model has no measured history to rank on and
        // the DecisionEngine's own switch threshold holds it on the production
        // pick. That is the cold-start cost of the mechanism, and asserting it
        // away would be asserting the mechanism does not exist.
        let mean = learned_costs.iter().sum::<usize>() as f64 / learned_costs.len() as f64;
        assert!(
            mean < 1.1,
            "the learned plan still averaged {mean} attempts per request; \
             it did not change routing"
        );
        assert_eq!(
            learned_costs[0], 2,
            "the first request after a cold start is expected to fall back once"
        );
        assert!(
            learned_costs[1..].iter().all(|cost| *cost == 1),
            "every request after the first should have succeeded on the first \
             attempt, got {:?}",
            &learned_costs[1..]
        );
    }

    // The flaky provider is genuinely being avoided now: it was called on every one
    // of the 40 collection requests and on none of the 40 served requests.
    let flaky_after = upstream.flaky_calls();
    assert_eq!(
        flaky_after - flaky_before,
        1,
        "the learned plan should have tried the failing provider once, at cold start"
    );

    // -- Rollback restores the previous model, and the router follows. --------
    //
    // A second, different model is promoted so rollback has somewhere to go.
    // Rolling back after a single promotion is correctly a refusal — there is no
    // prior model — and asserting otherwise would be asserting a rollback into a
    // state that never existed.
    let store = ActiveModelStore::open(&state_dir).expect("store");
    let other_commit = second_model(&artefacts, &store, "second-model");

    assert!(
        store.rollback().expect("rollback"),
        "there was a prior model"
    );
    assert_eq!(
        store.active_identity().expect("read").as_deref(),
        Some(first_commit.as_str()),
        "rollback did not restore the model that was replaced"
    );
    assert_ne!(
        other_commit, first_commit,
        "the second model must be a different model, or this proves nothing"
    );
    let audit = store.audit().expect("audit");
    assert_eq!(
        audit
            .iter()
            .filter(|entry| entry.action == zroutery_core::ml::ActiveModelAction::Rollback)
            .count(),
        1,
        "the rollback was not audited"
    );

    // -- With the restored model, the router is ML-routed again. -------------
    let harness = Harness::start(config_for(addr, &state_dir, true), upstream.clone()).await;
    assert!(
        harness.state.ml_routing().is_attached(),
        "the restored model should be attached"
    );
    assert_eq!(
        harness.state.ml_routing().counts().fallbacks,
        0,
        "attaching a verified model should not fall back"
    );
}

// ---------------------------------------------------------------------------
// Exploration reaches real requests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exploration_moves_real_requests_and_never_serves_an_ineligible_candidate() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    harness.drive(40).await;
    drop(harness);

    let artefacts = loop_over(state_dir);
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    promote_trained(&store, &artefacts, promotable_gate());
    let path = artefacts.state_dir.path().to_path_buf();

    // Exploration on, at a rate well under the ceiling.
    let mut config = config_for(addr, &path, true);
    config.ml_routing.exploration_probability = 0.3;
    let harness = Harness::start(config, upstream.clone()).await;
    let costs = harness.drive(60).await;

    // Exploration happened, and it happened often enough to be a behaviour
    // rather than a coin that never landed.
    let counts = harness.state.ml_routing().counts();
    assert!(counts.rankings > 0);
    assert!(
        counts.explorations > 0,
        "exploration was configured at 30% and never fired"
    );
    assert!(
        counts.explorations < counts.rankings,
        "exploration fired on every single request, which is not exploration"
    );

    // The requests still succeeded, which is the safety property: an explored
    // candidate is drawn from the eligible set, so it is a provider this
    // request was allowed to use.
    assert!(
        costs.iter().all(|cost| *cost >= 1),
        "exploration produced a request that never reached a provider: {costs:?}"
    );

    // And the model's configuration is readable, so an operator can see what
    // exploration rate is in force rather than inferring it.
    assert_eq!(harness.state.ml_routing().exploration().probability, 0.3);
    assert!(ExplorationConfig {
        probability: harness.state.ml_routing().exploration().probability,
        seed: harness.state.ml_routing().exploration().seed,
    }
    .validate()
    .is_ok());
}

// ---------------------------------------------------------------------------
// The operator surface
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_status_document_describes_the_process_that_is_actually_serving() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    // -- A fresh installation says so, in every field that could be misread. ---
    //
    // No state directory is the default, and "enabled but nothing is attached" is
    // the state a misconfiguration produces. Neither may read as "working".
    let harness = Harness::start(
        config_for(addr, std::path::Path::new(""), false),
        upstream.clone(),
    )
    .await;
    let fresh = harness.state.ml_status();
    assert!(!fresh.durable_state, "no directory was configured");
    assert!(!fresh.traces_open);
    assert!(!fresh.model_store_open);
    assert!(fresh.active.is_none());
    assert!(
        !fresh.is_routing_with_a_model(),
        "a fresh install cannot be routing with a model"
    );
    assert!(
        fresh.headline().contains("deterministic"),
        "headline was {:?}",
        fresh.headline()
    );
    drop(harness);

    // A directory but no model: the switch could be on with nothing attached, and
    // that is the state an operator is most likely to misread as working.
    let empty_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, empty_dir.path(), true), upstream.clone()).await;
    let nothing = harness.state.ml_status();
    assert!(nothing.durable_state && nothing.traces_open && nothing.model_store_open);
    assert!(nothing.active.is_none());
    assert!(
        nothing.headline().contains("no model is attached"),
        "the enabled-with-no-model state must name itself; headline was {:?}",
        nothing.headline()
    );
    drop(harness);

    // -- Collect, learn, promote, serve. --------------------------------------
    let state_dir = tempfile::tempdir().expect("tempdir");
    {
        let harness =
            Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
        harness.drive(40).await;
    }
    let artefacts = loop_over(state_dir);
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    let commit = promote_trained(&store, &artefacts, promotable_gate()).candidate_commit;

    let harness = Harness::start(
        config_for(addr, artefacts.state_dir.path(), true),
        upstream.clone(),
    )
    .await;
    harness.drive(20).await;

    // -- Now the document has to agree with the router, field by field. --------
    let status = harness.state.ml_status();
    assert!(
        status.is_routing_with_a_model(),
        "a promoted, enabled model should read as serving; headline was {:?}",
        status.headline()
    );
    let active = status.active.as_ref().expect("an active model");
    assert_eq!(
        active.commit_id, commit,
        "the status names the wrong commit"
    );
    assert_eq!(
        harness.state.ml_routing().attached_commit().as_deref(),
        Some(commit.as_str()),
        "the status document and the router disagree about what is serving"
    );
    assert_eq!(active.verdict, PromotionVerdict::Promoted);
    assert!(active.paired_requests > 0);
    assert!(active.holdout_loss.is_finite());

    // The gate's own reasoning is readable, criteria and all, so "why is this
    // serving" has an answer rather than a verdict.
    let decision = status
        .active_decision
        .as_ref()
        .expect("the decision travels");
    assert_eq!(decision.verdict, PromotionVerdict::Promoted);
    assert!(
        !decision.criteria.is_empty(),
        "a promotion with no criteria listed is not explainable"
    );
    for criterion in &decision.criteria {
        assert!(
            criterion.held,
            "criterion {} did not hold on a PROMOTED decision: {}",
            criterion.name, criterion.reason
        );
    }

    // Collection counters are real, and they are counts rather than claims.
    assert!(status.dataset.samples > 0, "no samples were ingested");
    assert!(
        status.traces.as_ref().expect("the log is open").appended > 0,
        "no traces were written"
    );
    assert!(status.routing.rankings > 0, "the model never ranked");
    assert!(status.routing.attached);
    assert!(status.routing.fallbacks == 0);

    // And the history records the promotion that got here.
    assert!(
        status.history.iter().any(|entry| entry.commit_id == commit
            && entry.action == zroutery_core::ml::ActiveModelAction::Promote),
        "the promotion that produced the serving model is not in the history"
    );

    // It survives the trip to JSON, because that is how an operator reads it.
    let encoded = serde_json::to_string(&status).expect("encode");
    let decoded: zroutery_core::MlStatus = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, status);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operator_can_replay_the_serving_model_over_their_own_traffic() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    // -- Refusals first, because they are the states an operator hits first. ---
    //
    // No history is not the same answer as no model, and an analysis that
    // returned an empty result for both would leave the operator unable to tell
    // "serve some traffic" from "promote a model".
    {
        let harness = Harness::start(
            config_for(addr, std::path::Path::new(""), false),
            upstream.clone(),
        )
        .await;
        let analysis = harness.state.ml_shadow_analysis(1_000);
        assert!(!analysis.is_analysed());
        assert_eq!(analysis.traces_read, 0);
        assert!(
            analysis
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("durable state"),
            "reason was {:?}",
            analysis.reason
        );
    }
    let empty_dir = tempfile::tempdir().expect("tempdir");
    {
        let harness =
            Harness::start(config_for(addr, empty_dir.path(), false), upstream.clone()).await;
        harness.drive(10).await;
        let analysis = harness.state.ml_shadow_analysis(1_000);
        assert!(!analysis.is_analysed());
        assert!(
            analysis.traces_read >= 10,
            "history was collected, so the replay should have read it before \
             concluding there was nothing to replay"
        );
        assert!(
            analysis
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("no model is attached"),
            "reason was {:?}",
            analysis.reason
        );
    }

    // -- Now with a model serving. --------------------------------------------
    let state_dir = tempfile::tempdir().expect("tempdir");
    {
        let harness =
            Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
        harness.drive(40).await;
    }
    let artefacts = loop_over(state_dir);
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    let commit = promote_trained(&store, &artefacts, promotable_gate()).candidate_commit;

    let harness = Harness::start(
        config_for(addr, artefacts.state_dir.path(), true),
        upstream.clone(),
    )
    .await;
    harness.drive(20).await;

    let status = harness.state.ml_shadow_analysis(1_000);
    assert!(status.is_analysed(), "reason was {:?}", status.reason);
    assert_eq!(
        status.commit_id.as_deref(),
        Some(commit.as_str()),
        "the replay judged a different model than the one serving"
    );
    assert!(
        status.traces_read > 0,
        "it read nothing and still reported numbers"
    );

    let analysis = status.analysis.as_ref().expect("the analysis");
    assert_eq!(analysis.records, status.traces_read);
    assert!(
        (0.0..=1.0).contains(&analysis.agreement_rate),
        "agreement rate {} is not a rate",
        analysis.agreement_rate
    );
    assert_eq!(
        analysis.agreements + analysis.disagreements,
        analysis.decision_records,
        "agreements and disagreements do not account for every decision record"
    );
    // Prediction-only records are counted apart from decisions, because a
    // counterfactual that never decided is a different kind of evidence.
    assert_eq!(
        analysis.records,
        analysis.decision_records + analysis.prediction_only_records,
        "records are not accounted for"
    );

    // A limit is a real bound, and a smaller window is a smaller analysis rather
    // than the same one repeated.
    let bounded = harness.state.ml_shadow_analysis(5);
    assert!(bounded.traces_read <= 5, "the bound was not applied");
    if let Some(bounded) = bounded.analysis {
        assert!(bounded.records <= 5);
    }

    // A bound of zero reads nothing rather than everything.
    let none = harness.state.ml_shadow_analysis(0);
    assert_eq!(none.traces_read, 0);
    assert!(!none.is_analysed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rollback_takes_effect_in_the_live_process_and_not_only_on_disk() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    {
        let harness =
            Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
        harness.drive(40).await;
    }
    let artefacts = loop_over(state_dir);
    let state_dir = artefacts.state_dir.path().to_path_buf();
    let store = ActiveModelStore::open(&state_dir).expect("store");
    let first_commit = promote_trained(&store, &artefacts, promotable_gate()).candidate_commit;

    let harness = Harness::start(config_for(addr, &state_dir, true), upstream.clone()).await;
    assert_eq!(
        harness.state.ml_routing().attached_commit().as_deref(),
        Some(first_commit.as_str())
    );

    // -- Promote a second model while the process is running. -----------------
    //
    // This is the step a pointer-only implementation gets wrong. Nothing tells
    // the router that the durable pointer moved, so a rollback afterwards would
    // restore the pointer and leave the withdrawn model ranking traffic.
    let second_commit = second_model(&artefacts, &store, "second-model");
    assert_ne!(second_commit, first_commit);

    let reload = harness.state.reload_active_model();
    assert!(
        reload.is_clean(),
        "reloading a good pointer reported {:?}",
        reload.error
    );
    assert_eq!(
        harness.state.ml_routing().attached_commit().as_deref(),
        Some(second_commit.as_str()),
        "a promotion on disk did not reach the running router"
    );

    // -- Roll back, and require the router to follow. --------------------------
    let outcome = harness.state.rollback_active_model();
    assert!(
        outcome.is_clean(),
        "a rollback with a prior model reported {:?}",
        outcome.error
    );
    assert!(outcome.attached, "the restored model should be serving");
    assert_eq!(
        harness.state.ml_routing().attached_commit().as_deref(),
        Some(first_commit.as_str()),
        "the router is still ranking with the model that was rolled back"
    );
    assert_eq!(
        harness.state.ml_status().active.expect("active").commit_id,
        first_commit,
        "the status document disagrees with the router about what is serving"
    );

    // And it keeps serving: the restored model is usable, not just named.
    // Measured as a delta, so this says "every one of these twenty requests was
    // ranked by the restored model" rather than "some number above twenty".
    let rankings_before = harness.state.ml_routing().counts().rankings;
    harness.drive(20).await;
    let counts = harness.state.ml_routing().counts();
    assert_eq!(
        counts.fallbacks, 0,
        "the restored model should rank without falling back"
    );
    assert_eq!(
        counts.rankings,
        rankings_before + 20,
        "the restored model was not consulted on every request"
    );

    // -- A second rollback has nowhere to go, and says so. ---------------------
    let refused = harness.state.rollback_active_model();
    assert!(
        !refused.is_clean(),
        "rolling back with no prior model reported success"
    );
    assert!(
        refused
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("earlier"),
        "the refusal did not say why; error was {:?}",
        refused.error
    );
    assert_eq!(
        harness.state.ml_routing().attached_commit().as_deref(),
        Some(first_commit.as_str()),
        "a refused rollback changed what is serving"
    );

    // The refusal is on the record, next to the rollback that worked.
    let audit = store.audit().expect("audit");
    assert_eq!(
        audit
            .iter()
            .filter(|entry| entry.action == zroutery_core::ml::ActiveModelAction::Rollback)
            .count(),
        1,
        "the audit does not match the one rollback that happened"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operator_surface_is_served_over_http_and_behind_the_auth_layer() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    {
        let harness =
            Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
        harness.drive(40).await;
    }
    let artefacts = loop_over(state_dir);
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    let commit = promote_trained(&store, &artefacts, promotable_gate()).candidate_commit;

    let mut config = config_for(addr, artefacts.state_dir.path(), true);
    config.server.require_auth = true;
    let harness = Harness::start(config, upstream.clone()).await;
    let base = format!("http://{}", harness.server.addr);

    // Which model is serving, and on whose authority, is not public
    // information: it says what an installation's routing is doing right now.
    for path in ["/v1/ml/status", "/v1/ml/shadow"] {
        let response = harness
            .client
            .get(format!("{base}{path}"))
            .send()
            .await
            .expect("send");
        assert_eq!(
            response.status(),
            401,
            "{path} answered an unauthenticated caller"
        );
    }
    let response = harness
        .client
        .post(format!("{base}/v1/ml/rollback"))
        .send()
        .await
        .expect("send");
    assert_eq!(
        response.status(),
        401,
        "the rollback route answered an unauthenticated caller"
    );

    // With the token, all three answer, and they answer about *this* model.
    let status: serde_json::Value = harness
        .client
        .get(format!("{base}/v1/ml/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("status")
        .json()
        .await
        .expect("json");
    assert_eq!(status["available"], serde_json::json!(true));
    assert_eq!(status["routing_with_a_model"], serde_json::json!(true));
    assert_eq!(
        status["status"]["active"]["commit_id"],
        serde_json::json!(commit)
    );

    let shadow: serde_json::Value = harness
        .client
        .get(format!("{base}/v1/ml/shadow"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("shadow")
        .json()
        .await
        .expect("json");
    assert!(shadow["traces_read"].as_u64().unwrap_or(0) > 0);
    assert_eq!(shadow["commit_id"], serde_json::json!(commit));

    // A bound can be asked for, and is honoured.
    let bounded: serde_json::Value = harness
        .client
        .get(format!("{base}/v1/ml/shadow?limit=3"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("shadow")
        .json()
        .await
        .expect("json");
    assert!(bounded["traces_read"].as_u64().unwrap_or(u64::MAX) <= 3);

    // -- The rollback route does what the command does, in this process. ------
    let second_commit = second_model(&artefacts, &store, "second-model");
    let _ = harness.state.reload_active_model();
    let rollback: serde_json::Value = harness
        .client
        .post(format!("{base}/v1/ml/rollback"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("rollback")
        .json()
        .await
        .expect("json");
    assert_eq!(rollback["outcome"]["error"], serde_json::Value::Null);
    assert_eq!(
        rollback["status"]["active"]["commit_id"],
        serde_json::json!(commit),
        "the HTTP rollback did not restore the trained model"
    );
    assert_ne!(second_commit, commit);
    assert_eq!(
        harness.state.ml_routing().attached_commit().as_deref(),
        Some(commit.as_str())
    );
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// Print the whole loop's numbers, from a real run, in one place.
///
/// Ignored by default because it is a report, not an assertion: it exists so the
/// figures in `docs/development/ml-closed-loop-report.md` can be regenerated
/// rather than copied forward and trusted.
///
///     cargo test -p zroutery-core --features ml --test ml_closed_loop_test \
///         -- --ignored --nocapture print_the_closed_loop_evidence
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "prints the evidence report; run it deliberately"]
async fn print_the_closed_loop_evidence() {
    let (upstream, addr) = start_upstream(Profile::DeadFlaky).await;
    let state_dir = tempfile::tempdir().expect("tempdir");

    // Phase 1 -- collect.
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    let costs = harness.drive(COLLECTION_REQUESTS).await;
    let mean: f64 = costs.iter().sum::<usize>() as f64 / costs.len() as f64;
    println!("== COLLECT ==");
    println!("requests                 {COLLECTION_REQUESTS}");
    println!(
        "traces persisted         {}",
        harness
            .state
            .traces()
            .expect("the fixture configures a state directory")
            .counters()
            .appended
    );
    println!(
        "samples ingested         {}",
        harness.state.dataset().counters().samples
    );
    println!("upstream calls           {}", upstream.calls().len());
    println!("attempts per request     {mean:.3}");
    println!(
        "fallback rate            {:.3}",
        harness
            .state
            .dataset()
            .legacy_training_slice()
            .iter()
            .filter(|s| !s.targets.success)
            .count() as f64
            / COLLECTION_REQUESTS as f64
    );
    drop(harness);

    let artefacts = loop_over(state_dir);

    // Phase 2 -- learn, compare, analyse.
    let report = &artefacts.training.report;
    println!();
    println!("== TRAIN ==");
    println!("samples                  {}", report.sample_count);
    println!("requests                 {}", report.request_count);
    println!(
        "train / val / holdout    {} / {} / {}",
        report.train_size, report.validation_size, report.holdout_size
    );
    println!(
        "groups                   {} / {} / {}",
        report.train_groups, report.validation_groups, report.holdout_groups
    );
    println!("passes                   {}", report.passes.len());
    println!(
        "holdout log loss         {:.4}  (uninformed = {:.4})",
        report.holdout_loss,
        std::f64::consts::LN_2
    );
    println!(
        "holdout brier            {:.4}",
        report.holdout.brier_score.unwrap_or(f64::NAN)
    );
    println!(
        "features observed        {}/{}",
        report.coverage.observed, report.coverage.dimension
    );
    println!("commit                   {}", report.final_commit);
    println!("fitted partition         {}", report.dataset_fingerprint);
    println!("source body              {}", report.source_fingerprint);
    println!("config identity          {}", report.config_identity);

    println!();
    println!("== COMPARE (routing metrics) ==");
    println!(
        "{:<26} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9} {:>7}",
        "arm", "n", "cover", "success", "fallbk", "lat_ms", "cost", "provs"
    );
    for arm in &artefacts.comparison.arms {
        println!(
            "{:<26} {:>7} {:>7.3} {:>9.3} {:>9.3} {:>9.1} {:>9.5} {:>7}",
            arm.policy,
            arm.requests_measured,
            arm.coverage,
            arm.success_rate,
            arm.fallback_rate,
            arm.mean_latency_ms,
            arm.mean_cost,
            arm.distinct_providers
        );
    }

    println!();
    println!("== COMPARE (paired, candidate minus baseline) ==");
    for pairing in &artefacts.comparison.paired {
        println!(
            "{:<26} paired={:<5} verdict={:<20} utility={:+.4} latency={:+.1}ms cost={:+.5} \
succ={:+.3} regr={} impr={} both_fail={}",
            pairing.baseline,
            pairing.deltas.paired_requests,
            match pairing.verdict {
                zroutery_core::ml::RoutingVerdict::Improved => "Improved",
                zroutery_core::ml::RoutingVerdict::Regressed => "Regressed",
                zroutery_core::ml::RoutingVerdict::NoDifference => "NoDifference",
                zroutery_core::ml::RoutingVerdict::InsufficientEvidence => "InsufficientEvidence",
            },
            pairing.deltas.mean_observed_utility_delta,
            pairing.deltas.mean_latency_delta_ms,
            pairing.deltas.mean_cost_delta,
            pairing.deltas.success_rate_delta,
            pairing.deltas.candidate_regressions,
            pairing.deltas.candidate_improvements,
            pairing.deltas.both_failed
        );
    }

    println!();
    println!("== SHADOW ==");
    println!("records                  {}", artefacts.analysis.records);
    println!(
        "decision-shaped          {}",
        artefacts.analysis.decision_records
    );
    println!(
        "agreement rate           {:.3}",
        artefacts.analysis.agreement_rate
    );
    println!(
        "disagreements            {}",
        artefacts.analysis.disagreements
    );
    println!(
        "measured disagreements   {}",
        artefacts.analysis.disagreements_measured
    );
    println!(
        "measured alt rate        {:.3}",
        artefacts.analysis.measured_alternative_rate
    );
    println!(
        "observed utility delta   {:?}",
        artefacts.analysis.mean_observed_utility_delta
    );
    println!(
        "mean regret              {:?}",
        artefacts.analysis.mean_regret
    );
    println!(
        "helpful / harmful        {} / {}",
        artefacts.analysis.helpful_alternatives, artefacts.analysis.harmful_alternatives
    );
    println!(
        "evaluable                {}",
        artefacts.analysis.is_evaluable()
    );
    for (reason, count) in &artefacts.analysis.gaps {
        println!("  gap {reason:<24} {count}");
    }

    // Phase 3 -- gate.
    let gate = PromotionGate::new(PromotionConfig::default());
    let decision = gate.evaluate(report, &artefacts.comparison, Some("evidence-run".into()));
    println!();
    println!("== GATE ==");
    println!("verdict                  {}", decision.verdict.as_str());
    println!("gate identity            {}", decision.gate_config_identity);
    println!("candidate commit         {}", decision.candidate_commit);
    println!("judged over body         {}", decision.dataset_fingerprint);
    println!(
        "fitted partition         {}",
        decision.fitted_partition_fingerprint
    );
    println!("paired requests          {}", decision.paired_requests);
    for criterion in &decision.criteria {
        println!(
            "  [{}] {:<22} {}",
            if criterion.held { "ok" } else { "--" },
            criterion.name,
            criterion.reason
        );
    }

    // Phase 4 -- serve.
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    promote_trained(&store, &artefacts, promotable_gate());
    let path = artefacts.state_dir.path().to_path_buf();

    let mut served_config = config_for(addr, &path, true);
    served_config.ml_routing.exploration_probability = 0.0;
    let served = Harness::start(served_config, upstream.clone()).await;
    let served_costs = served.drive(COLLECTION_REQUESTS).await;
    let served_mean: f64 = served_costs.iter().sum::<usize>() as f64 / served_costs.len() as f64;
    let counts = served.state.ml_routing().counts();

    println!();
    println!("== SERVE (with the promoted model) ==");
    println!("model attached           {}", counts.attached);
    println!("rankings                 {}", counts.rankings);
    println!("fallbacks                {}", counts.fallbacks);
    println!("attempts per request     {served_mean:.3}");
    println!("total upstream calls     {}", upstream.calls().len());
    println!("attempt-cost histogram   {}", histogram(&served_costs));
    println!("baseline cost histogram  {}", histogram(&costs));

    drop(served);
    drop(artefacts);
}
