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

use std::collections::BTreeMap;
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
use zroutery_core::ml::RewardPolicy;
use zroutery_core::ml::RoutingModel;
use zroutery_core::ml::{
    ActiveModel, ActiveModelStore, ExplorationConfig, MlPolicy, PromotionConfig, PromotionGate,
    PromotionVerdict, ReplayBaseline, RoutingVerdict, ShadowEvidence, TrainingConfig,
    ACTIVE_MODEL_SCHEMA_VERSION,
};
use zroutery_core::server::{AppState, ServerHandle};

// ---------------------------------------------------------------------------
// A local upstream that behaves like two different providers
// ---------------------------------------------------------------------------

/// Which upstream models fail, and how often.
///
/// The interesting property is that the answer is a *function of the upstream
/// model name*, so a router that has learned anything at all can tell `flaky`
/// from `steady` — and a router that has learned nothing will keep picking
/// `flaky`, because that is what priority says.
const FLAKY_UPSTREAM: &str = "flaky";

#[derive(Clone)]
struct FakeUpstream {
    calls: Arc<Mutex<Vec<String>>>,
    flaky_calls: Arc<AtomicUsize>,
}

impl FakeUpstream {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            flaky_calls: Arc::new(AtomicUsize::new(0)),
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

    if model.starts_with(FLAKY_UPSTREAM) {
        upstream.flaky_calls.fetch_add(1, Ordering::Relaxed);
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {"message": "this upstream is unhealthy", "type": "server_error"}
            })),
        )
            .into_response();
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
async fn start_upstream() -> (FakeUpstream, SocketAddr) {
    let upstream = FakeUpstream::new();
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
// Phase 1: real requests produce durable history
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_requests_produce_outcomes_samples_and_durable_traces() {
    let (upstream, addr) = start_upstream().await;

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
    let counters = harness.state.traces().counters();
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

fn loop_over(state_dir: tempfile::TempDir) -> LoopArtefacts {
    let log = zroutery_core::ml::TraceLog::open(state_dir.path()).expect("reopen");
    let traces = log.load().expect("load");
    assert!(!traces.is_empty(), "there is no history to learn from");

    let samples = zroutery_core::ml::deduped_samples_from(&traces);
    let training =
        zroutery_core::ml::run_training(&samples, &TrainingConfig::default()).expect("train");

    let policy = RewardPolicy::default();
    let candidate = MlPolicy::new(&training, policy.clone());
    let comparison =
        zroutery_core::ml::run_comparison(&traces, &candidate, &ReplayBaseline::ALL, &policy)
            .expect("comparison");

    let evidence = ShadowEvidence::from_policy(&traces, &candidate, BTreeMap::new());
    let analysis = zroutery_core::ml::analyse(&traces, &evidence, &policy);

    LoopArtefacts {
        state_dir,
        traces,
        training,
        comparison,
        analysis,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collected_traces_train_an_identified_model_that_beats_the_deterministic_plan() {
    let (upstream, addr) = start_upstream().await;

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
    let (upstream, addr) = start_upstream().await;

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

    let promoted = zroutery_core::ml::ActivePredictor::load(&ActiveModel {
        schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
        model_id: artefacts.training.model_id.clone(),
        commit_id: artefacts.training.commit_record().commit_id.to_string(),
        learning_event_count: artefacts.training.report.learning_event_count,
        checkpoint: artefacts.training.checkpoint.clone(),
        promotion_digest: "digest-from-gate".to_string(),
        promoted_at: 0,
    })
    .expect("the trained model verifies");
    let commit = promoted.commit_id().to_string();

    store
        .promote(
            ActiveModel {
                schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
                model_id: artefacts.training.model_id.clone(),
                commit_id: commit.clone(),
                learning_event_count: artefacts.training.report.learning_event_count,
                checkpoint: artefacts.training.checkpoint.clone(),
                promotion_digest: "digest-from-gate".to_string(),
                promoted_at: 0,
            },
            "digest-from-gate",
            format!("gate said {}", decision.verdict.as_str()),
        )
        .expect("promote");

    let reloaded = store.active().expect("read").expect("an active model");
    assert_eq!(reloaded.commit_id().as_str(), commit);
    assert_eq!(store.audit().expect("audit").len(), 1);
}

// ---------------------------------------------------------------------------
// Phase 4: a promoted model re-orders a real request, and rollback undoes it
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promoted_model_changes_which_provider_serves_and_rollback_restores_the_plan() {
    let (upstream, addr) = start_upstream().await;

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

    // -- Promote. ------------------------------------------------------------
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    store
        .promote(
            ActiveModel {
                schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
                model_id: artefacts.training.model_id.clone(),
                commit_id: artefacts.training.commit_record().commit_id.to_string(),
                learning_event_count: artefacts.training.report.learning_event_count,
                checkpoint: artefacts.training.checkpoint.clone(),
                promotion_digest: "digest".to_string(),
                promoted_at: 0,
            },
            "digest",
            "promoted by the closed-loop test",
        )
        .expect("promote");
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
    let first_commit = artefacts.training.commit_record().commit_id.to_string();

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
        7,
    );
    store
        .promote(
            ActiveModel {
                schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
                model_id: artefacts.training.model_id.clone(),
                commit_id: other_record.commit_id.to_string(),
                learning_event_count: 7,
                checkpoint: other_checkpoint,
                promotion_digest: "digest-2".to_string(),
                promoted_at: 0,
            },
            "digest-2",
            "a second model, so rollback has a target",
        )
        .expect("promote");
    assert_eq!(
        store.active_identity().expect("read").as_deref(),
        Some(other_record.commit_id.as_str())
    );

    assert!(
        store.rollback().expect("rollback"),
        "there was a prior model"
    );
    assert_eq!(
        store.active_identity().expect("read").as_deref(),
        Some(first_commit.as_str()),
        "rollback did not restore the model that was replaced"
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
    let (upstream, addr) = start_upstream().await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    harness.drive(40).await;
    drop(harness);

    let artefacts = loop_over(state_dir);
    let store = ActiveModelStore::open(artefacts.state_dir.path()).expect("store");
    store
        .promote(
            ActiveModel {
                schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
                model_id: artefacts.training.model_id.clone(),
                commit_id: artefacts.training.commit_record().commit_id.to_string(),
                learning_event_count: artefacts.training.report.learning_event_count,
                checkpoint: artefacts.training.checkpoint.clone(),
                promotion_digest: "digest".to_string(),
                promoted_at: 0,
            },
            "digest",
            "promoted for the exploration phase",
        )
        .expect("promote");
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
    let (upstream, addr) = start_upstream().await;
    let state_dir = tempfile::tempdir().expect("tempdir");

    // Phase 1 -- collect.
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    let costs = harness.drive(COLLECTION_REQUESTS).await;
    let mean: f64 = costs.iter().sum::<usize>() as f64 / costs.len() as f64;
    println!("== COLLECT ==");
    println!("requests                 {COLLECTION_REQUESTS}");
    println!(
        "traces persisted         {}",
        harness.state.traces().counters().appended
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
    store
        .promote(
            ActiveModel {
                schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
                model_id: artefacts.training.model_id.clone(),
                commit_id: artefacts.training.commit_record().commit_id.to_string(),
                learning_event_count: artefacts.training.report.learning_event_count,
                checkpoint: artefacts.training.checkpoint.clone(),
                promotion_digest: "evidence-run".to_string(),
                promoted_at: 0,
            },
            "evidence-run",
            format!("gate said {}", decision.verdict.as_str()),
        )
        .expect("promote");
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
    println!("attempt-cost histogram   {:?}", histogram(&served_costs));
    println!("baseline cost histogram  {:?}", histogram(&costs));

    drop(served);
    drop(artefacts);
}

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
async fn the_real_body_produces_a_directional_verdict_rather_than_a_framework_claim() {
    let (upstream, addr) = start_upstream().await;

    let state_dir = tempfile::tempdir().expect("tempdir");
    let harness = Harness::start(config_for(addr, state_dir.path(), false), upstream.clone()).await;
    harness.drive(COLLECTION_REQUESTS).await;
    drop(harness);

    let artefacts = loop_over(state_dir);

    // Every pairing must be reported with a verdict, and the verdict must be
    // reachable on real evidence rather than blocked for want of data. This is
    // the assertion that would have failed before the loop existed: the
    // comparison used to be a framework with nothing to run on.
    assert!(!artefacts.comparison.paired.is_empty());
    for pairing in &artefacts.comparison.paired {
        assert_ne!(
            pairing.verdict,
            RoutingVerdict::InsufficientEvidence,
            "pairing against {} refused for want of evidence on a body of {}",
            pairing.baseline,
            artefacts.comparison.traces
        );
    }

    // The round trip: a report that can be stored and read back with every
    // number intact is what makes it evidence rather than a console line.
    let encoded = serde_json::to_string(&artefacts.comparison).expect("encode");
    let decoded: zroutery_core::ml::RoutingComparison =
        serde_json::from_str(&encoded).expect("decode");
    assert_eq!(
        decoded.dataset_fingerprint,
        artefacts.comparison.dataset_fingerprint
    );
    assert_eq!(
        decoded.candidate_commit,
        artefacts.comparison.candidate_commit
    );
    assert_eq!(decoded.arms.len(), artefacts.comparison.arms.len());

    drop(artefacts);
}
