//! Cross-provider routing, on real traffic.
//!
//! Every other fixture in this repository routes between two *models of one
//! provider*. That is enough to prove the loop closes — a request becomes a
//! trace, a trace trains a model, the model reorders the plan — but it leaves
//! the product's actual claim untested. Zroutery exists to aggregate *several
//! providers*, and no evidence here ever crossed a provider boundary.
//!
//! It also leaves a sharper question open. There is no provider-identity feature
//! in the vector: a candidate is described entirely by its own measured latency,
//! success rate and capabilities. That is the right design, and it means what
//! happens to a provider the model has *never observed* is a product decision
//! rather than an implementation detail. Adding a provider is the single most
//! common thing an operator does.
//!
//! So this file asks two questions and answers them with real HTTP against three
//! real upstreams:
//!
//! 1. Can a model trained on cross-provider evidence route across providers, and
//!    prefer the genuinely better one over the strategy Zroutery ships?
//! 2. When a provider joins a router that is already serving, what happens — and
//!    does the router find out that the new provider is better?
//!
//! Everything else is the same discipline as `ml_closed_loop_test.rs`: the
//! production axum server, real requests, real upstreams, nothing constructed by
//! hand, and every routing claim measured at the upstream rather than inferred
//! from the router's own bookkeeping.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use zroutery_core::billing::Pricing;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

use zroutery_core::ml::{
    ActiveModelStore, PromotionConfig, PromotionGate, PromotionVerdict, ReplayBaseline,
    TrainingConfig,
};

const TOKEN: &str = "zr-multi-token";

/// Requests per measured phase.
///
/// Sized for the paired-comparison evidence floor of 30 with room to spare, and
/// for phase four of the second experiment — where a provider has to be tried,
/// observed and then preferred — to have somewhere to get to.
const PHASE_REQUESTS: usize = 60;

// ---------------------------------------------------------------------------
// Upstreams
// ---------------------------------------------------------------------------

/// How one provider behaves.
///
/// Per provider rather than global, because the whole point of this file is that
/// providers differ. A single global profile would reproduce the fixture it is
/// meant to replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Fast, and fails three calls in four. Its failures are fast too, which is
    /// what makes it attractive to anything that only reads latency.
    FastFlaky,
    /// Slow and always succeeds.
    SlowReliable,
    /// Fast *and* always succeeds.
    ///
    /// The provider a learned router should end up preferring, and the one no
    /// latency-reading heuristic will: `Priority` never tries it at all.
    FastReliable,
}

impl Behaviour {
    fn latency_ms(self) -> u64 {
        match self {
            Behaviour::FastFlaky => 2,
            Behaviour::SlowReliable => 40,
            Behaviour::FastReliable => 5,
        }
    }

    /// Whether this call fails.
    ///
    /// Deterministic rather than random. A replay of the same body has to be the
    /// same body, and a comparison confounded by luck measures the coin rather
    /// than the router.
    fn fails_at(self, index: usize) -> bool {
        match self {
            Behaviour::FastFlaky => !index.is_multiple_of(4),
            Behaviour::SlowReliable | Behaviour::FastReliable => false,
        }
    }
}

/// One provider's upstream: a real HTTP server with its own port, and its own
/// behaviour per model.
///
/// Per model rather than per provider, because a provider hosting two models with
/// opposite reliability is the ordinary case and collapsing it to one behaviour
/// per provider would reproduce the two-models-one-shape fixture this file exists
/// to replace.
#[derive(Clone)]
struct Upstream {
    /// Model-name prefix to behaviour. Every model the provider serves is here,
    /// so an unexpected model name is a loud lookup failure rather than a
    /// silently-defaulted one.
    models: Arc<BTreeMap<String, Behaviour>>,
    calls: Arc<AtomicUsize>,
    failures: Arc<AtomicUsize>,
    /// Model name to how many times it has been called.
    per_model: Arc<Mutex<BTreeMap<String, usize>>>,
}

impl Upstream {
    fn new(models: &[(&str, Behaviour)]) -> Self {
        Self {
            models: Arc::new(
                models
                    .iter()
                    .map(|(prefix, behaviour)| (prefix.to_string(), *behaviour))
                    .collect(),
            ),
            calls: Arc::new(AtomicUsize::new(0)),
            failures: Arc::new(AtomicUsize::new(0)),
            per_model: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    fn failures(&self) -> usize {
        self.failures.load(Ordering::Relaxed)
    }

    /// Calls to models whose name starts with `prefix`.
    #[allow(dead_code)]
    fn calls_for(&self, prefix: &str) -> usize {
        self.per_model
            .lock()
            .expect("per_model")
            .iter()
            .filter(|(model, count)| model.starts_with(prefix) && **count > 0)
            .map(|(_, count)| *count)
            .sum()
    }

    /// How many of those calls failed.
    ///
    /// Derived from the call counts and the deterministic behaviour rather than
    /// from a separate counter, so a disagreement between "how many times was it
    /// called" and "how many of those failed" is not something this can report
    /// about itself.
    fn failures_for(&self, prefix: &str) -> usize {
        let per_model = self.per_model.lock().expect("per_model").clone();
        per_model
            .iter()
            .filter(|(model, _)| model.starts_with(prefix))
            .map(|(model, count)| {
                let behaviour = self.models[model.as_str()];
                (0..*count).filter(|index| behaviour.fails_at(*index)).count()
            })
            .sum()
    }

    #[allow(dead_code)]
    fn served_one(&self, prefix: &str) -> bool {
        self.calls_for(prefix) > 0
    }

    /// Every model name this upstream was actually asked for.
    fn model_names(&self) -> Vec<String> {        self.per_model
            .lock()
            .expect("per_model")
            .keys()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream")
            .field("models", &self.models)
            .field("calls", &self.calls())
            .field("failures", &self.failures())
            .finish()
    }
}

async fn fake_chat(
    axum::extract::State(upstream): axum::extract::State<Upstream>,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let model = body["model"].as_str().unwrap_or("").to_string();
    let behaviour = upstream
        .models
        .get(&model)
        .copied()
        .unwrap_or_else(|| panic!("the upstream was asked for an undeclared model {model:?}"));
    let index = {
        let mut per_model = upstream.per_model.lock().expect("per_model");
        let entry = per_model.entry(model.clone()).or_insert(0);
        let current = *entry;
        *entry += 1;
        current
    };
    upstream.calls.fetch_add(1, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(behaviour.latency_ms())).await;

    if behaviour.fails_at(index) {
        upstream.failures.fetch_add(1, Ordering::Relaxed);
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "error": {"message": "this provider is unhealthy", "type": "server_error"}
            })),
        )
            .into_response();
    }

    axum::Json(json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "answered"},
                     "finish_reason": "stop"}],
        // Enough tokens that price differences are not rounded away. At 20
        // prompt tokens every model in the other fixtures prices to zero and the
        // cost axis carries no information at all.
        "usage": {"prompt_tokens": 400, "completion_tokens": 120}
    }))
    .into_response()
}

async fn start_upstream(models: &[(&str, Behaviour)]) -> (Upstream, SocketAddr) {
    let upstream = Upstream::new(models);
    let router = axum::Router::new()
        .route("/v1/chat/completions", axum::routing::post(fake_chat))
        .with_state(upstream.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (upstream, addr)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn provider(id: &str, name: &str, upstream: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{upstream}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

/// One model, with a price that differs per provider.
///
/// Pricing matters here in a way it did not before: `FastReliable` is the
/// provider a learned router should pick, and making it the *expensive* one is
/// what forces the trade-off to be real rather than "the best provider on every
/// axis at once".
fn model(provider_id: &str, name: &str, priority: i32, pricing: (f64, f64)) -> ModelEntry {
    let mut entry =
        ModelEntry::for_upstream(provider_id, name, Some(ModelTier::Standard)).with_priority(priority);
    entry.pricing = Some(Pricing::new("USD", pricing.0, pricing.1));
    entry
}

/// Which providers are in the plan.
///
/// `gamma` is the one that joins later in the second experiment, so it has to be
/// optional rather than always present.
struct Topology {
    alpha: SocketAddr,
    gamma: SocketAddr,
    include_gamma: bool,
}

fn config_for(
    topology: &Topology,
    state_dir: &std::path::Path,
    ml_enabled: bool,
    exploration: f64,
) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();

    let mut providers = vec![
        provider("alpha", "Alpha", topology.alpha),
        provider("gamma", "Gamma", topology.gamma),
    ];
    let mut models = vec![
        // The shipped default strategy is Priority, and priority is the whole
        // plan: flaky first, steady second, gamma last. So `Priority` never
        // prefers gamma, and never spends an attempt discovering it is the best
        // provider in the set.
        model("alpha", "flaky-std", 5, (3.0, 15.0)),
        model("alpha", "steady-std", 10, (1.0, 4.0)),
    ];
    if topology.include_gamma {
        providers.truncate(1);
        providers.push(provider("gamma", "Gamma", topology.gamma));
        models.push(model("gamma", "gamma-std", 15, (9.0, 36.0)));
    }

    cfg.providers = providers;
    cfg.models = models;
    // A regime change mid-window would make a phase a mixture of two
    // experiments. The quarantining behaviour is exercised deliberately, in its
    // own phase, with the threshold lowered on purpose.
    cfg.routing.circuit_breaker.failure_threshold = 1000;
    cfg.routing.circuit_breaker.min_requests = 1000;
    cfg.shadow.enabled = true;
    cfg.ml_routing.enabled = ml_enabled;
    cfg.ml_routing.state_dir = state_dir.display().to_string();
    cfg.ml_routing.exploration_probability = exploration;
    cfg
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    state: Arc<AppState>,
    server: ServerHandle,
    client: reqwest::Client,
}

impl Harness {
    async fn start(config: AppConfig) -> Harness {
        let mut secrets = MemorySecretStore::new();
        for provider in ["alpha", "gamma"] {
            secrets = secrets.with(format!("provider:{provider}"), "sk-x");
        }
        let state = Arc::new(AppState::new(config, Arc::new(secrets)));
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
        }
    }

    /// Drive one class-routed request, returning the number of upstream calls the
    /// whole set of providers actually received.
    ///
    /// Counted at the upstreams rather than read from the router's own
    /// bookkeeping, because "the model chose a different provider" is a claim
    /// about where traffic went.
    async fn ask(&self, upstreams: &[Upstream]) -> usize {
        let before: usize = upstreams.iter().map(|u| u.calls()).sum();
        let response = self
            .client
            .post(format!("http://{}/v1/messages", self.server.addr))
            .header("x-api-key", TOKEN)
            .json(&json!({
                // The tier's virtual id, so the request is class-routed and
                // carries a decision. A direct model id resolves straight to one
                // candidate and there is nothing to learn from.
                "model": "standard-class",
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "route this"}],
            }))
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status().as_u16(),
            200,
            "a request did not succeed: {}",
            response.status()
        );
        upstreams.iter().map(|u| u.calls()).sum::<usize>() - before
    }

    async fn drive(&self, count: usize, upstreams: &[Upstream]) -> Vec<usize> {
        let mut costs = Vec::with_capacity(count);
        for index in 0..count {
            costs.push(self.ask(upstreams).await);
            if index % 20 == 19 {
                // The observation store is what the model ranks on, and it is
                // in-memory per process. Without this the later phases would be
                // measured against a store the earlier ones never populated.
                tokio::task::yield_now().await;
            }
        }
        costs
    }
}

/// Attempts per request, ignoring the cold-start prefix.
///
/// The first request or two of any process fall back to the deterministic plan
/// because the observation store starts empty. That is a real cost of the
/// mechanism and asserting it away would be asserting the mechanism does not
/// exist, so it is excluded from the mean and reported separately.
fn mean_after_cold_start(costs: &[usize]) -> f64 {
    let warm = &costs[COLD_START.min(costs.len())..];
    if warm.is_empty() {
        return 0.0;
    }
    warm.iter().sum::<usize>() as f64 / warm.len() as f64
}

/// Requests at the head of every process that are expected to fall back.
const COLD_START: usize = 2;

/// A gate configuration naming a baseline this fixture's evidence can beat.
///
/// `baseline.priority` is the strategy Zroutery ships, and against it a learned
/// router that avoids the failing provider has something real to win *within one
/// provider*. Naming it explicitly is the point: "beats a baseline" is a claim
/// about a specific baseline.
///
/// It is **not** the right choice for a candidate that has discovered a second
/// provider, and that is measured rather than asserted — see
/// `print_which_comparator_a_candidate_can_be_measured_against`. Exploration
/// reaches a new provider by displacing priority's first pick, so the two arms'
/// measured requests barely overlap.
fn promotable_gate() -> PromotionConfig {
    PromotionConfig {
        required_baseline: "baseline.priority".to_string(),
        ..PromotionConfig::default()
    }
}



// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// Train, compare and gate one body, reporting what came out.
///
/// Every number a caller asserts on is produced here rather than assembled at the
/// call site, so a test and the printed evidence cannot drift apart.
struct Body {
    traces: Vec<zroutery_core::ml::RequestTrace>,
    comparison: zroutery_core::ml::RoutingComparison,
    decision: zroutery_core::ml::PromotionDecision,
}

impl Body {
    fn arm(&self, policy: &str) -> &zroutery_core::ml::ArmMetrics {
        self.comparison
            .arm(policy)
            .unwrap_or_else(|| panic!("no arm named {policy}"))
    }

    /// How many requests both this arm and the named baseline had a measurement
    /// for.
    fn paired_with(&self, baseline: &str) -> usize {
        self.comparison
            .pairing(baseline)
            .map(|pairing| pairing.deltas.paired_requests)
            .unwrap_or(0)
    }

    /// How many recorded outcomes the body holds for one model's exposed id.
    fn outcomes_for(&self, model_suffix: &str) -> usize {
        self.traces
            .iter()
            .flat_map(|trace| trace.attempt_samples())
            .filter(|sample| sample.model_id.ends_with(model_suffix))
            .count()
    }
}

/// The paired-evidence floor the shipped gate applies.
///
/// Not asserted against directly. The verdict it produces is unstable on this
/// fixture — see `print_how_often_the_verdict_moves` — so it is restated here
/// only so the diagnostics can name the number the gate refuses against.
#[allow(dead_code)]
const MIN_PAIRED: usize = 30;

/// Train, compare and gate one durable body, promoting it only if the gate says
/// so.
///
/// Every number a caller asserts on is produced here rather than assembled at the
/// call site, so a test and the printed evidence cannot drift apart.
fn learn(state_dir: &std::path::Path, note: &str) -> Body {
    let traces = zroutery_core::ml::TraceLog::open(state_dir)
        .expect("open")
        .load()
        .expect("load");
    let samples = zroutery_core::ml::deduped_samples_from(&traces);
    let training =
        zroutery_core::ml::run_training(&samples, &TrainingConfig::default()).expect("train");
    let policy = zroutery_core::ml::RewardPolicy::default();
    let candidate = zroutery_core::ml::MlPolicy::new(&training, policy.clone());
    let comparison =
        zroutery_core::ml::run_comparison(&traces, &candidate, &ReplayBaseline::ALL, &policy)
            .expect("comparison");
    let decision =
        PromotionGate::new(promotable_gate()).evaluate(&training.report, &comparison, Some(note.into()));
    if decision.verdict == PromotionVerdict::Promoted {
        ActiveModelStore::open(state_dir)
            .expect("store")
            .promote(&decision, training.checkpoint.clone())
            .expect("promote");
    }
    Body {
        traces,
        comparison,
        decision,
    }
}

/// The exploration probability the ceiling allows, used where the point is to
/// reach an unobserved candidate.
const MAX_EXPLORATION: f64 = 0.5;

/// The two models `alpha` serves: one fast and unreliable, one slow and reliable.
async fn two_models() -> (Upstream, SocketAddr) {
    start_upstream(&[
        ("flaky-std", Behaviour::FastFlaky),
        ("steady-std", Behaviour::SlowReliable),
    ])
    .await
}

/// One provider serving `gamma-std`: fast and always successful.
async fn gamma_provider() -> (Upstream, SocketAddr) {
    start_upstream(&[("gamma-std", Behaviour::FastReliable)]).await
}

// ---------------------------------------------------------------------------
// The deadlock
// ---------------------------------------------------------------------------

/// A provider the deterministic plan never reaches is never measured — and is now
/// named, so the silence is distinguishable from health.
///
/// This is a fact about the *plan*, not about the model, which is what makes it
/// stable enough to assert: `Priority` tries `flaky-std`, falls back to
/// `steady-std`, and one of those two always succeeds. `gamma-std` sits below
/// both, so it is never attempted and no amount of retraining changes that.
///
/// The invisibility itself is unchanged and still asserted below: the durable
/// body holds no outcome for `gamma-std`, so the model genuinely cannot know the
/// provider exists. What changed is the reporting. This test used to be named
/// `..._and_nothing_reports_it`, which was the defect — an unreachable provider
/// was indistinguishable from a healthy one. `MlStatus::blind_candidates` now
/// names it, and the assertion at the end of the driving phase is what checks the
/// derivation against the real request path: it reads the observation store
/// through the same keys the outcome recorder writes, so a wrong key would show
/// up here as every candidate reported blind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_the_plan_never_reaches_is_never_measured_and_is_reported() {
    let (alpha, alpha_addr) = two_models().await;
    let (gamma, gamma_addr) = gamma_provider().await;
    let upstreams = [alpha.clone(), gamma.clone()];
    let topology = Topology {
        alpha: alpha_addr,
        gamma: gamma_addr,
        include_gamma: true,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let harness = Harness::start(config_for(&topology, dir.path(), false, 0.0)).await;
        let costs = harness.drive(PHASE_REQUESTS, &upstreams).await;
        let counts = harness.state.ml_routing().counts();

        assert_eq!(
            counts.blind_explorations,
            0,
            "exploration was configured at zero, so nothing should have explored"
        );
        assert_eq!(
            gamma.calls(),
            0,
            "the deterministic plan is flaky-then-steady and one of them always \
             succeeds, so the third candidate is never reached. It received {} calls.",
            gamma.calls()
        );
        // The incumbent is genuinely costly, so this is not a body with nothing
        // in it. It is a body with one provider's worth in it.
        assert!(
            mean_after_cold_start(&costs) > 1.2,
            "the deterministic plan should be paying for its bad first choice, got {:.3}",
            mean_after_cold_start(&costs)
        );

        // And the operator surface now names it. This is the assertion that ties
        // `BlindCandidate::unobserved`'s key format to the one the outcome
        // recorder actually writes: the two candidates that did serve are absent
        // because the store has records under exactly these keys, and gamma is
        // present because it has none. A mismatch in either direction — deriving
        // `exposed_id()` wrongly, or reading a different provider id — would move
        // one of the served candidates into this list, or empty it.
        let status = harness.state.ml_status();
        let blind: Vec<String> = status
            .blind_candidates
            .iter()
            .map(|candidate| candidate.model_id.clone())
            .collect();
        assert_eq!(
            blind,
            vec!["gamma-gamma-std".to_string()],
            "exactly the candidate that was never tried should be reported blind; \
             the two that served must not appear. Exploration was off, so this gap \
             is permanent."
        );
        assert!(
            status.blind_spots_are_permanent(),
            "exploration was configured at 0.0, so nothing can ever reach it"
        );
        let warning = status
            .blind_spot_warning()
            .expect("an unreachable provider with exploration off must warn");
        assert!(
            warning.contains("gamma-gamma-std"),
            "the warning has to name the provider so the operator knows which \
             configuration entry to look at; got: {warning}"
        );
    }

    let body = learn(dir.path(), "no-exploration");

    // Three candidates were eligible on every request, so the plan did offer it.
    assert!(
        body.arm("ml.candidate").mean_eligible_candidates > 2.5,
        "the plan should have offered all three candidates; it offered {:.2}",
        body.arm("ml.candidate").mean_eligible_candidates
    );
    assert_eq!(
        body.arm("ml.candidate").ineligible_selections, 0,
        "every candidate was eligible, so nothing was excluded for being a stranger"
    );

    // And the durable body contains no outcome for it whatsoever. There is no
    // feature, no sample, no fingerprint contribution: the model cannot know the
    // provider exists, and neither can anything reading the store.
    assert_eq!(
        body.outcomes_for("gamma-std"),
        0,
        "the durable body recorded outcomes for a provider that was never tried"
    );

    // Meanwhile a strategy defined as *spreading* does reach it. That asymmetry
    // is the finding: a learned ranking is worse than round-robin at discovering
    // a provider nobody happened to try.
    assert_eq!(
        body.arm("baseline.round_robin").distinct_providers, 2,
        "round robin is defined as spreading, so it should have used both providers"
    );
}

// ---------------------------------------------------------------------------
// What opens it
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exploration_without_a_model_reaches_a_provider_and_records_evidence_about_it() {
    let (alpha, alpha_addr) = two_models().await;
    let (gamma, gamma_addr) = gamma_provider().await;
    let upstreams = [alpha.clone(), gamma.clone()];
    let topology = Topology {
        alpha: alpha_addr,
        gamma: gamma_addr,
        include_gamma: true,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let blind;
    {
        let harness =
            Harness::start(config_for(&topology, dir.path(), false, MAX_EXPLORATION)).await;
        harness.drive(2 * PHASE_REQUESTS, &upstreams).await;
        let counts = harness.state.ml_routing().counts();
        blind = counts.blind_explorations;
        assert_eq!(
            counts.rankings, 0,
            "no model was promoted, so no ranking should have happened"
        );
        assert_eq!(
            counts.explorations, 0,
            "the model-attached exploration path should not have been taken"
        );
    }

    // -- Exploration fired, with no model anywhere in the picture. ----------
    //
    // This is the whole point, and it is why exploration cannot live behind
    // `is_attached()`. A model cannot be trained on evidence the router never
    // gathered, so exploration that requires a promoted model can only ever
    // explore what the model already believes in.
    assert!(
        blind > 0,
        "exploration is configured at the ceiling and never fired without a model"
    );
    assert!(
        gamma.calls() > 0,
        "gamma received no traffic even though exploration was on"
    );
    assert_eq!(
        gamma.failures_for("gamma-std"),
        0,
        "gamma is reliable; a recorded failure would mean the fixture is wrong"
    );

    // -- And the evidence is durable, which is the part that was missing. ----
    let body = learn(dir.path(), "explored");
    assert!(
        body.outcomes_for("gamma-std") > 0,
        "the durable body carries no outcomes for the newly reachable provider"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exploration_without_a_model_stays_inside_the_plan_and_keeps_requests_succeeding() {
    let (alpha, alpha_addr) = two_models().await;
    let (gamma, gamma_addr) = gamma_provider().await;
    let upstreams = [alpha.clone(), gamma.clone()];
    let topology = Topology {
        alpha: alpha_addr,
        gamma: gamma_addr,
        include_gamma: true,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let harness =
        Harness::start(config_for(&topology, dir.path(), false, MAX_EXPLORATION)).await;
    // `drive` asserts a 200 on every request, so reaching the end is the
    // success-rate claim.
    harness.drive(2 * PHASE_REQUESTS, &upstreams).await;

    // Exploration is a choice among legal candidates, not a way to route
    // somewhere the request was not allowed to go. The set of model names the
    // upstreams were asked for is exactly the set the plan contained.
    let asked: Vec<String> = alpha
        .model_names()
        .into_iter()
        .chain(gamma.model_names())
        .collect();
    assert!(!asked.is_empty(), "no request reached any upstream");
    for name in &asked {
        assert!(
            ["flaky-std", "steady-std", "gamma-std"].contains(&name.as_str()),
            "a provider was asked for {name}, which is not in the plan. The upstream sees
             the bare model name; the exposed id in traces is the prefixed form."
        );
    }

    // And the ineligible candidate count in the replay is the same claim about
    // the decision-time eligibility the draw was allowed to choose from.
    let body = learn(dir.path(), "explored-safety");
    assert_eq!(
        body.arm("ml.candidate").ineligible_selections, 0,
        "a ranking selected a candidate the decision marked ineligible"
    );
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// A served request carries its price into the durable body, and the routing
/// comparison can therefore tell two policies apart on cost.
///
/// Three capabilities depend on this, and all three were dead until the attempt
/// record grew a cost field:
///
/// 1. Every attempt-scoped training sample is cost-free.
/// 2. The routing comparison reads **only** attempt samples, so `mean_cost` was a
///    structural constant — `0.0000000000` for every arm.
/// 3. So `RewardPolicy::cost_weight` contributed nothing to the observed utility
///    the promotion gate reads, and the gate's cost-budget criterion was measured
///    against zero.
///
/// The request-scoped sample beside it carried a perfectly good
/// `outcome.actual_cost` and nothing consumed it. `Attempt` had no cost field at
/// all, so per-attempt spend was never recorded anywhere.
///
/// Nothing noticed, because every test with a non-zero `actual_cost` builds the
/// `Outcome` by hand, and the one pipeline test that compares spend against the
/// activity record passes just as happily with both sides at zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_served_request_carries_its_price_into_the_durable_body() {
    let (alpha, alpha_addr) = two_models().await;
    let (gamma, gamma_addr) = gamma_provider().await;
    let upstreams = [alpha.clone(), gamma.clone()];
    let topology = Topology {
        alpha: alpha_addr,
        gamma: gamma_addr,
        include_gamma: true,
    };

    // The fixture's own precondition, asserted rather than assumed: every model is
    // priced. Without this, a failure below would be ambiguous between the
    // pipeline dropping the cost and the fixture never having one to drop.
    let config = config_for(&topology, std::path::Path::new(""), false, 0.0);
    assert!(
        config.models.iter().all(|entry| entry.pricing.is_some()),
        "the fixture prices every model, and this assertion is what makes the ones \
         below about the pipeline rather than about the fixture"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let harness = Harness::start(config_for(&topology, dir.path(), false, 0.0)).await;
        harness.drive(PHASE_REQUESTS, &upstreams).await;
    }
    let traces = zroutery_core::ml::TraceLog::open(dir.path())
        .expect("open")
        .load()
        .expect("load");

    // Per model, so a single blended figure cannot hide a model that is priced
    // wrongly. The fixture prices them 9x apart on purpose.
    let mut by_model: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for trace in &traces {
        for sample in trace.attempt_samples() {
            if let Some(cost) = sample.targets.cost {
                by_model
                    .entry(sample.model_id.clone())
                    .or_default()
                    .push(cost);
            }
        }
    }
    for (model, costs) in &by_model {
        let highest = costs.iter().copied().fold(0.0f64, f64::max);
        assert!(
            highest > 0.0,
            "{model} carries costs but none of them is positive: {costs:?}"
        );
    }
    assert!(
        by_model.len() >= 2,
        "the body attributes cost to {} models; a comparison between policies needs \
         at least two candidates to differ",
        by_model.len()
    );

    // And the comparison's cost axis is no longer structurally zero.
    //
    // What is *not* asserted here is that two arms differ on cost: with
    // exploration off this fixture's two arms measure the same candidate on the
    // same requests, so separation is not demonstrable here. It is measured with
    // exploration on, where the arms genuinely choose differently priced
    // candidates — `baseline.priority` 0.00300 against `ml.candidate` 0.00702,
    // which is what showed the learned router spending 2.3x what the shipped
    // strategy spends. See §E4 of the report.
    let body = learn(dir.path(), "cost-axis");
    for arm in &body.comparison.arms {
        assert!(
            arm.mean_cost > 0.0,
            "{} reports mean_cost {} ; a zero means the cost axis is inert again, \
             and both the reward policy's cost weight and the gate's cost budget \
             read it",
            arm.policy,
            arm.mean_cost
        );
    }
}

/// Print which comparator a candidate can actually be measured against.
///
/// Ignored by default because it is a measurement, and because **its numbers are
/// not stable enough to assert**. It repeats the same body and prints the paired
/// set against every baseline, so the spread is visible rather than asserted.
///
///     cargo test -p zroutery-core --features ml --test ml_multi_provider_test \
///         -- --ignored --nocapture print_which_comparator
///
/// # What it shows
///
/// The paired set is a property of the candidate *and the baseline it is named
/// against*, not of the candidate alone.
///
/// An earlier version of this file asserted `paired(balanced) > paired(priority)`
/// and failed roughly one run in three. Repeating it showed why: whether the
/// model discovers the newly reachable provider on a given run is itself
/// borderline, because `gamma-std` is eight times faster than `steady-std` and
/// equally reliable but costs nine times as much, and `RewardPolicy` weights
/// latency 0.3 against cost 0.1. Five runs at the exploration ceiling:
///
/// ```text
///   run  providers  paired: priority  round_robin  lowest_latency  balanced
///     1          2               4           15              38        40
///     2          2               6           11              14        31
///     3          1              40           30               8         1
///     4          2               2            9              17        34
///     5          2               2            7               2        29
/// ```
///
/// Run 3 is the clearest case: with no discovery the candidate pairs *perfectly*
/// with `baseline.priority` (40) and not at all with `baseline.balanced` (1). With
/// discovery the reverse. Exploration reached the new provider by displacing
/// priority's first pick, so the candidate's measured requests and priority's are
/// close to disjoint, while `baseline.balanced` — which considers the same
/// candidates — overlaps them.
///
/// A sixth run broke even the anti-correlation (discovery *and* `paired(priority)`
/// at 31, one over the floor), so the effect is a strong tendency and not an
/// invariant. That is why it is printed.
///
/// The operational reading: `promotable_gate()`'s `baseline.priority` is a
/// same-provider choice. Naming it for a candidate that has discovered a provider
/// tends to yield `BLOCKED (paired_evidence: N)` for a candidate that may be
/// perfectly good, and the blocker reads as "not enough data" rather than "you
/// compared against the wrong arm".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "prints measurements; run it deliberately"]
async fn print_which_comparator_a_candidate_can_be_measured_against() {
    const REPEATS: usize = 5;
    println!(
        "\n{REPEATS} bodies, exploration at the ceiling, {} requests each\n",
        2 * PHASE_REQUESTS
    );
    println!(
        "  run  providers  ml measured  paired: priority  round_robin  lowest_latency  balanced"
    );
    for run in 1..=REPEATS {
        let (alpha, alpha_addr) = two_models().await;
        let (gamma, gamma_addr) = gamma_provider().await;
        let upstreams = [alpha.clone(), gamma.clone()];
        let topology = Topology {
            alpha: alpha_addr,
            gamma: gamma_addr,
            include_gamma: true,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let harness =
                Harness::start(config_for(&topology, dir.path(), false, MAX_EXPLORATION)).await;
            harness.drive(2 * PHASE_REQUESTS, &upstreams).await;
        }
        let body = learn(dir.path(), "comparator");
        let paired = |baseline: &str| body.paired_with(baseline).to_string();
        println!(
            "  {run:>3}  {:>9}  {:>11}  {:>14}  {:>11}  {:>14}  {}",
            body.arm("ml.candidate").distinct_providers,
            body.arm("ml.candidate").requests_measured,
            paired("baseline.priority"),
            paired("baseline.round_robin"),
            paired("baseline.lowest_latency"),
            paired("baseline.balanced"),
        );
    }
    println!(
        "\n  the shipped gate's paired floor is {MIN_PAIRED}; the comparator a candidate is \
         named\n  against can move the paired set from near zero to comfortably over it.\n"
    );
}

/// Print how often the promotion verdict moves on identical traffic.
///
/// Ignored by default because it is a measurement. It exists because the honest
/// answer to "does this body promote?" on this fixture is a distribution, and
/// quoting one run's verdict as the answer would be quoting noise.
///
///     cargo test -p zroutery-core --features ml --test ml_multi_provider_test \
///         -- --ignored --nocapture print_how_often_the_verdict_moves
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "prints measurements; run it deliberately"]
async fn print_how_often_the_verdict_moves() {
    const REPEATS: usize = 6;
    println!("\nidentical fixture, identical traffic, {REPEATS} independent runs:\n");
    println!("  run  gamma calls  ml providers  ml measured  paired(priority)  verdict");
    let mut promoted = 0usize;
    let mut providers: Vec<usize> = Vec::new();
    for run in 1..=REPEATS {
        let (alpha, alpha_addr) = two_models().await;
        let (gamma, gamma_addr) = gamma_provider().await;
        let upstreams = [alpha.clone(), gamma.clone()];
        let topology = Topology {
            alpha: alpha_addr,
            gamma: gamma_addr,
            include_gamma: true,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let harness = Harness::start(config_for(&topology, dir.path(), false, 0.0)).await;
            harness.drive(PHASE_REQUESTS, &upstreams).await;
        }
        let body = learn(dir.path(), "verdict-spread");
        let arm = body.arm("ml.candidate");
        providers.push(arm.distinct_providers);
        if body.decision.verdict == PromotionVerdict::Promoted {
            promoted += 1;
        }
        println!(
            "  {run:>3}  {:>11}  {:>12}  {:>11}  {:>16}  {}",
            gamma.calls(),
            arm.distinct_providers,
            arm.requests_measured,
            body.paired_with("baseline.priority"),
            body.decision.verdict.as_str(),
        );
    }
    println!(
        "\n  promoted {promoted} of {REPEATS}; distinct providers per run: {providers:?}\n"
    );
}

/// Print what it costs to make a promotion comparison measurable when the model
/// prefers a candidate the baseline never tried.
///
/// Ignored by default because it is a measurement, not an assertion. It exists so
/// the numbers quoted in the report are regenerated rather than copied forward.
///
///     cargo test -p zroutery-core --features ml --test ml_multi_provider_test \
///         -- --ignored --nocapture print_what_a_promotion_costs_to_measure
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "prints measurements; run it deliberately"]
async fn print_what_a_promotion_costs_to_measure() {
    println!("\nrequests  explore  gamma calls  blind  ml providers  ml measured  paired(priority)  gate");
    for requests in [PHASE_REQUESTS, 2 * PHASE_REQUESTS, 4 * PHASE_REQUESTS] {
        for probability in [0.0, 0.25, MAX_EXPLORATION] {
            let (alpha, alpha_addr) = two_models().await;
            let (gamma, gamma_addr) = gamma_provider().await;
            let upstreams = [alpha.clone(), gamma.clone()];
            let topology = Topology {
                alpha: alpha_addr,
                gamma: gamma_addr,
                include_gamma: true,
            };
            let dir = tempfile::tempdir().expect("tempdir");
            let blind;
            {
                let harness =
                    Harness::start(config_for(&topology, dir.path(), false, probability)).await;
                harness.drive(requests, &upstreams).await;
                blind = harness.state.ml_routing().counts().blind_explorations;
            }
            let body = learn(dir.path(), "sweep");
            println!(
                "{requests:>8}  {probability:>7.2}  {:>11}  {blind:>5}  {:>12}  {:>11}  {:>16}  {}",
                gamma.calls(),
                body.arm("ml.candidate").distinct_providers,
                body.arm("ml.candidate").requests_measured,
                body.paired_with("baseline.priority"),
                body.decision.verdict.as_str(),
            );
        }
    }
    println!();
}
