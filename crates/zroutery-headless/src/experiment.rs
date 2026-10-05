//! Empirical ML Router experiment.
//!
//! # What this is, and what it deliberately is not
//!
//! This drives the **production** ML Router. Every request goes through
//! `AppState` -> the axum pipeline -> `apply_ml_ranking` -> `MlRouter::rank` ->
//! the upstream, over a real socket, exactly as a deployed proxy would. There is no
//! second router here and no simulated ML: the fakes below are *providers*, and
//! they know nothing about ranking, features, models or promotion.
//!
//! That distinction is the whole point. A harness that calls `compute_utility`
//! directly would prove that arithmetic works. This one can only report what the
//! shipped routing path actually did with the traffic.
//!
//! # The arc
//!
//! ```text
//! phase 1  collect     no model is attached; the deterministic plan serves
//!         promote     run a round over the collected history and install it
//! phase 2  exploit     same environment: does ML now route differently?
//! phase 3  degrade     the environment changes under the promoted model
//!         relearn     run a round again over the degraded history
//! phase 4  adapt       does it recover?
//! ```
//!
//! Phases 3 and 4 are the part that matters. A model that cannot be shown to *lose*
//! when its world changes, and recover when it is retrained, has not demonstrated
//! anything a fixed baseline would not also demonstrate.
//!
//! # Determinism
//!
//! Provider behaviour is a function of a per-model call index, never a random
//! number, so a replay of the same body is the same body and a comparison is not
//! confounded by luck. The seed is accepted and recorded for provenance but does not
//! currently drive anything stochastic — which is stated rather than implied,
//! because a "seed" that does nothing is exactly the kind of thing a report should
//! not claim.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ml::{
    deduped_samples_from, run_comparison, MlPolicy, PromotionConfig, ReplayBaseline, RewardPolicy,
    RoundConfig, TraceLog,
};
use zroutery_core::server::{AppState, ServerHandle};

const TOKEN: &str = "experiment-token";

/// The tier the experiment's requests are class-routed through.
///
/// A direct model id resolves straight to one candidate and carries no decision, so
/// there is nothing to learn from. A tier virtual id goes through the classifier and
/// produces the `RouteDecision` the ML path ranks against.
const TIER: ModelTier = ModelTier::Standard;

/// How one provider behaves. This is the *environment*; nothing here knows about
/// routing, models or rewards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// Fast, and fails three calls in four. Its failures are fast too, which is what
    /// makes it attractive to anything that reads only latency.
    FastFlaky,
    /// Slow and always succeeds.
    SlowReliable,
    /// Fast *and* always succeeds.
    ///
    /// "Best on every axis at once" was the note here once, and it was wrong in a
    /// way that mattered: a single behaviour cannot be both best and dearest. Price
    /// now lives on the provider (`CHARLIE_PRICE`), so the best provider is fast,
    /// reliable, and the most expensive one — which is the trade-off the cost axis
    /// exists to expose.
    FastReliable,
}

impl Behaviour {
    fn latency_ms(self) -> u64 {
        match self {
            Self::FastFlaky => 2,
            Self::SlowReliable => 40,
            Self::FastReliable => 5,
        }
    }

    /// Whether this call fails. A function of the call index, not a coin.
    fn fails_at(self, index: u64) -> bool {
        match self {
            Self::FastFlaky => !index.is_multiple_of(4),
            _ => false,
        }
    }
}

/// Per-million-token prices, USD.
///
/// Copied from CC Switch's own `model_pricing` table, for models that actually
/// served its traffic — not invented, and not the 40x-arbitrary ratio an earlier
/// draft used. `Pricing::cost_of` divides by 1_000_000, so these are per-million
/// figures: the earlier values (`0.000_000_5`) were written as if per *token*,
/// which made every arm price to `0.000000` and deleted the cost axis from every
/// comparison in the report without anything failing.
///
/// The cache-read price is not decoration. In the observed traffic the cache read
/// count runs 360x–595x the fresh input count, so nearly all of a real request's
/// cost is a cache read. A fixture that omits it does not measure the same axis a
/// real router has to optimise.
#[derive(Clone, Copy)]
struct Price {
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_read_per_mtok: f64,
}

/// Cheapest, and the one that fails three calls in four.
const ALPHA_PRICE: Price = Price {
    input_per_mtok: 0.15, // deepseek-v4-flash
    output_per_mtok: 0.6,
    cache_read_per_mtok: 0.003,
};

/// Mid-priced and dependable — the choice a static policy is expected to make.
const BRAVO_PRICE: Price = Price {
    input_per_mtok: 1.4, // glm-5.3
    output_per_mtok: 4.4,
    cache_read_per_mtok: 0.26,
};

/// Fast, reliable, and dearest. This is the trade-off that makes the cost axis
/// worth having: the best provider is the expensive one, so "best" and "cheapest"
/// disagree and utility has a real decision to make.
const CHARLIE_PRICE: Price = Price {
    input_per_mtok: 5.0, // claude-opus-4-8
    output_per_mtok: 25.0,
    cache_read_per_mtok: 0.5,
};

/// (total prompt, cache reads, completion) tokens.
///
/// `prompt_tokens` is cache-**inclusive** and the cache count is a subset of it,
/// which is the invariant `ir::Usage` documents and `protocol::openai::decode_usage`
/// enforces by clamping. The fresh portion is deliberately small, because in the
/// observed traffic it is: one busy model averaged 2.3k fresh input against 177k
/// cache reads.
///
/// Identical across all three providers on purpose. Token volume is a property of
/// the request, not of the provider, so holding it fixed is what makes price the
/// only thing that varies the cost.
const TOKENS: (u64, u64, u64) = (15_000, 11_000, 800);

impl Behaviour {
    fn label(self) -> &'static str {
        match self {
            Self::FastFlaky => "fast-flaky",
            Self::SlowReliable => "slow-reliable",
            Self::FastReliable => "fast-reliable",
        }
    }
}

/// Counters one fake upstream keeps, so a phase can be measured as a delta rather
/// than as a cumulative total nobody can interpret.
#[derive(Debug, Default)]
struct Counters {
    calls: AtomicU64,
    failures: AtomicU64,
    latency_ms_sum: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.calls.load(Ordering::Relaxed),
            self.failures.load(Ordering::Relaxed),
            self.latency_ms_sum.load(Ordering::Relaxed),
        )
    }
}

/// One fake provider upstream.
#[derive(Debug)]
struct Upstream {
    name: String,
    /// Swapped between phases; that is how a regime change is expressed.
    behaviour: Mutex<BTreeMap<String, Behaviour>>,
    counters: Counters,
    /// Per-model call index, so failure patterns are reproducible.
    per_model: Mutex<BTreeMap<String, u64>>,
}

impl Upstream {
    fn new(name: &str, models: &[(&str, Behaviour)]) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            behaviour: Mutex::new(models.iter().map(|(m, b)| (m.to_string(), *b)).collect()),
            counters: Counters::default(),
            per_model: Mutex::new(BTreeMap::new()),
        })
    }

    /// Change the environment without restarting anything.
    ///
    /// This is the regime change. A provider does not "become" different — the
    /// world does, and a running router has to notice from traffic alone.
    fn set(&self, models: &[(&str, Behaviour)]) {
        *self.behaviour.lock().expect("behaviour") =
            models.iter().map(|(m, b)| (m.to_string(), *b)).collect();
    }
}

async fn fake_chat(
    axum::extract::State(upstream): axum::extract::State<Arc<Upstream>>,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let model = body["model"].as_str().unwrap_or("").to_string();
    let behaviour = upstream
        .behaviour
        .lock()
        .expect("behaviour")
        .get(&model)
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "upstream {} was asked for undeclared model {model:?}",
                upstream.name
            )
        });
    let index = {
        let mut per_model = upstream.per_model.lock().expect("per_model");
        let entry = per_model.entry(model.clone()).or_insert(0);
        let current = *entry;
        *entry += 1;
        current
    };

    upstream.counters.calls.fetch_add(1, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(behaviour.latency_ms())).await;
    upstream
        .counters
        .latency_ms_sum
        .fetch_add(behaviour.latency_ms(), Ordering::Relaxed);

    if behaviour.fails_at(index) {
        upstream.counters.failures.fetch_add(1, Ordering::Relaxed);
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "error": {"message": "this provider is unhealthy", "type": "server_error"}
            })),
        )
            .into_response();
    }

    // `prompt_tokens_details.cached_tokens` is the read part of a cache-inclusive
    // total, which is what `protocol::openai::decode_usage` reads. Without it the
    // whole prompt bills at the fresh input price and the cache-read axis is
    // invisible.
    let (prompt, cache_read, completion) = TOKENS;
    axum::Json(json!({
        "id": "chatcmpl-experiment",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "answered"},
                     "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "prompt_tokens_details": {"cached_tokens": cache_read}
        }
    }))
    .into_response()
}

/// One provider in the experiment's world.
struct Site {
    upstream: Arc<Upstream>,
    addr: SocketAddr,
    models: Vec<(String, String)>,
}

/// The whole fake environment: three providers, one model each.
struct Environment {
    sites: Vec<Site>,
}

impl Environment {
    async fn start(regime: &[Behaviour; 3]) -> Self {
        let mut sites = Vec::new();
        for (index, name) in ["alpha", "bravo", "charlie"].iter().enumerate() {
            let model = format!("{name}-1");
            let upstream = Upstream::new(name, &[(model.as_str(), regime[index])]);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind fake upstream");
            let addr = listener.local_addr().expect("addr");
            let router = axum::Router::new()
                .route("/v1/chat/completions", axum::routing::post(fake_chat))
                .with_state(Arc::clone(&upstream));
            tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });
            sites.push(Site {
                upstream,
                addr,
                models: vec![(name.to_string(), model)],
            });
        }
        Self { sites }
    }

    /// Apply a new regime to every provider.
    fn set_regime(&self, regime: &[Behaviour; 3]) {
        for (site, behaviour) in self.sites.iter().zip(regime) {
            let models: Vec<(&str, Behaviour)> = site
                .models
                .iter()
                .map(|(_, model)| (model.as_str(), *behaviour))
                .collect();
            site.upstream.set(&models);
        }
    }

    fn config(&self, state_dir: &Path, exploration_probability: f64) -> AppConfig {
        let mut config = AppConfig::default();
        config.server.host = "127.0.0.1".into();
        // Port 0: the OS picks. This is an experiment, not a service, and a fixed
        // port would collide with a running proxy or with itself between runs.
        config.server.port = 0;
        config.server.auth_token = TOKEN.into();

        for (index, site) in self.sites.iter().enumerate() {
            let name = ["alpha", "bravo", "charlie"][index];
            let mut provider = ProviderConfig::new(name, name, ProviderKind::OpenAICompatible);
            provider.base_url = format!("http://{}", site.addr);
            provider.key_ref = format!("provider:{name}");
            provider.timeout_secs = 10;
            config.providers.push(provider);

            let (_, model) = &site.models[0];
            let mut entry = ModelEntry::for_upstream(name, model, Some(TIER));
            // Distinct priorities so the deterministic plan has a defined order to
            // be wrong about, and the ML path has something to disagree with.
            entry.priority = index as i32;
            // Price is a property of the *provider*, not of the phase or of its current
            // behaviour — a provider that degrades does not get cheaper. So it is
            // looked up by name here rather than derived from `Behaviour`, which
            // used to conflate "how this provider behaves right now" with "what it
            // charges".
            //
            // Pricing every model identically would make mean cost a constant per
            // arm and quietly remove the trade-off. An earlier draft reached for
            // the expensive price three times and did exactly that.
            let price = match name {
                "alpha" => ALPHA_PRICE,
                "bravo" => BRAVO_PRICE,
                _ => CHARLIE_PRICE,
            };
            let mut pricing = Pricing::new("USD", price.input_per_mtok, price.output_per_mtok);
            // `Pricing::new` leaves both cache prices unset, which bills every cache
            // read at the fresh input price. Given the token profile above that
            // would overcharge each provider by more than an order of magnitude,
            // and it would do so unevenly — so it would invent a cost difference
            // that the price table does not contain.
            pricing.cache_read_per_mtok = Some(price.cache_read_per_mtok);
            entry.pricing = Some(pricing);
            config.models.push(entry);
        }

        config.ml_routing.enabled = true;
        config.ml_routing.state_dir = state_dir.display().to_string();
        // Exploration is the only mechanism in the system that can route to a
        // candidate the deterministic plan does not already pick, so it is the
        // parameter that decides whether a newly added provider is reachable at
        // all. It is exposed rather than fixed so the trade-off against the
        // promotion gate can be measured instead of argued: report E6 found that
        // exploration collapses the paired evidence a promotion needs, and whether a
        // *small* value gets discovery without that collapse is an empirical
        // question this harness exists to answer.
        config.ml_routing.exploration_probability = exploration_probability;

        // **This is not optional and the coupling is not obvious.**
        //
        // The features a sample is built from are the *retained decision-time
        // input*, which is the `ShadowInput` the shadow engine evaluated. With
        // `shadow.enabled` false there is no retained record, so every request
        // ingests as `NoDecisionTimeInput`, nothing is stored, no trace is written,
        // and no round can ever find a body to train on.
        //
        // The first run of this harness produced `ingested=0 samples=0
        // no_decision_time_input=120 traces_appended=0` and two promotion rounds with
        // "no verdict" — an ML stack that is switched on and cannot learn anything.
        // It is recorded here because it is exactly the failure this experiment
        // exists to catch, and because nothing in `ml_routing.enabled` implies it.
        config.shadow.enabled = true;

        // Circuit breaking is left effectively off, as the multi-provider fixture
        // does and for the same reason: `alpha` fails three calls in four, so at the
        // shipped thresholds its breaker opens early and quarantines it for the rest
        // of the run. That would make every phase after the first a measurement of
        // a quarantined provider rather than of a router's choices.
        //
        // The trade-off is stated rather than hidden: with the breaker open, a
        // provider's sustained failure stops being routed to at all, which is a real
        // behaviour — just not the one this experiment is measuring.
        config.routing.circuit_breaker.failure_threshold = 1000;
        config.routing.circuit_breaker.min_requests = 1000;
        config
    }
}

/// What one phase measured.
struct PhaseReport {
    name: String,
    regime: Vec<String>,
    requests: usize,
    /// Wall-clock time the phase took, and what was left after subtracting the
    /// simulated upstream sleeps.
    ///
    /// The fake upstreams sleep a *fixed, known* duration per call, so their
    /// contribution is exactly known and subtracting it isolates what this process
    /// itself spent. That matters more than it sounds: arms route differently, so
    /// they take different-length fallback chains, and comparing raw wall time
    /// between them would mostly measure how many upstreams each one called.
    /// Subtracting makes the comparison about routing cost instead.
    wall_ms: f64,
    /// `wall_ms` minus the total simulated upstream sleep.
    overhead_ms: f64,
    /// Requests the client saw succeed end to end.
    ok: usize,
    /// Upstream calls and failures, per provider.
    per_site: Vec<SiteReport>,
    /// Whether a model was attached and ranking during this phase.
    rankings: u64,
    attached_commit: Option<String>,
    /// Ingestion counters, so a phase that collected nothing says so.
    ingested: u64,
    samples_collected: u64,
    no_decision_time_input: u64,
    traces_appended: u64,
    traces_nothing: u64,
}

struct SiteReport {
    name: String,
    calls: u64,
    failures: u64,
    mean_latency_ms: f64,
    /// Total simulated sleep this provider performed, so a phase can subtract the
    /// environment's contribution from its own wall clock.
    latency_total_ms: f64,
}

/// The whole experiment's findings, as one report.
pub struct Report {
    phases: Vec<PhaseReport>,
    promotions: Vec<PromotionReport>,
    replay: Vec<ArmReport>,
    traces_read: usize,
    notes: Vec<String>,
}

struct PromotionReport {
    revision: String,
    verdict: String,
    installed: bool,
    serving: bool,
    paired_requests: usize,
    required_baseline: String,
    blockers: Vec<String>,
    policy_choices: BTreeMap<String, usize>,
}

struct ArmReport {
    policy: String,
    measured: usize,
    success_rate: f64,
    fallback_rate: f64,
    mean_latency_ms: f64,
    mean_cost: f64,
    mean_utility: f64,
    distinct_providers: usize,
    ineligible: usize,
}

/// Run the experiment.
///
/// `exploration_probability` is the thing under test, not a fixed setting. At 0 a
/// configured provider the deterministic plan never picks receives no traffic from
/// any source, so it can never be discovered — which is what makes a regime change
/// involving a *new* best provider untestable rather than merely unlikely.
pub async fn run(
    state_dir: &Path,
    requests_per_phase: usize,
    exploration_probability: f64,
) -> Result<Report, String> {
    std::fs::create_dir_all(state_dir).map_err(|e| format!("state dir: {e}"))?;
    let _ = std::fs::remove_file(state_dir.join("traces.jsonl"));

    // Regime 1: charlie is best on speed and reliability, alpha is fast and broken,
    // bravo is dependable and slow.
    let regime_one = [
        Behaviour::FastFlaky,
        Behaviour::SlowReliable,
        Behaviour::FastReliable,
    ];
    let environment = Environment::start(&regime_one).await;
    let config = environment.config(state_dir, exploration_probability);

    let mut secrets = MemorySecretStore::new();
    for name in ["alpha", "bravo", "charlie"] {
        secrets = secrets.with(format!("provider:{name}"), "sk-experiment");
    }

    let state = Arc::new(AppState::new(config, Arc::new(secrets)));
    let server = ServerHandle::start(Arc::clone(&state))
        .await
        .map_err(|e| format!("server: {e}"))?;
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| format!("client: {e}"))?;

    let tier_virtual = TIER.virtual_id();
    let mut phases = Vec::new();
    let mut promotions = Vec::new();
    let mut notes = Vec::new();

    // -- phase 1: collect ----------------------------------------------------
    phases.push(
        drive(
            &client,
            server.addr,
            &environment,
            &state,
            "collect",
            &regime_one,
            requests_per_phase,
        )
        .await,
    );

    // -- promote -------------------------------------------------------------
    promotions.push(promote(&state, "round-1").await);
    let after_first = state.ml_status();
    notes.push(format!(
        "after round 1: active={:?}, routing_enabled={}, blind_candidates={}",
        after_first.active.as_ref().map(|a| a.commit_id.clone()),
        after_first.routing_enabled,
        after_first.blind_candidates.len()
    ));

    // -- phase 2: exploit, same world ---------------------------------------
    phases.push(
        drive(
            &client,
            server.addr,
            &environment,
            &state,
            "exploit",
            &regime_one,
            requests_per_phase,
        )
        .await,
    );

    // -- phase 3: the world changes -----------------------------------------
    // **bravo** is the provider the promoted model has been relying on, and it is
    // the one that degrades. An earlier draft degraded `charlie` instead, which
    // produced a phase that looked like an *improvement* (bravo 40ms -> 5ms) and
    // taught nothing: `charlie` was never being called, so degrading it changed no
    // traffic at all. A regime change has to touch the candidate that is actually
    // in the rotation, or the phase measures nothing.
    //
    // So bravo becomes fast *and* broken, and charlie -- unreachable without
    // exploration -- stays the best provider in the set. That is the sharpest
    // version of the question: the learned model now has nowhere good to go among
    // the candidates it knows about, and the only good provider is one it may never
    // have observed.
    let regime_two = [
        Behaviour::FastFlaky,
        Behaviour::FastFlaky,
        Behaviour::FastReliable,
    ];
    environment.set_regime(&regime_two);
    phases.push(
        drive(
            &client,
            server.addr,
            &environment,
            &state,
            "degrade",
            &regime_two,
            requests_per_phase,
        )
        .await,
    );

    // -- relearn, then adapt -------------------------------------------------
    promotions.push(promote(&state, "round-2").await);
    phases.push(
        drive(
            &client,
            server.addr,
            &environment,
            &state,
            "adapt",
            &regime_two,
            requests_per_phase,
        )
        .await,
    );

    server.stop().await;

    // -- replay: every arm over the whole body -------------------------------
    let log = TraceLog::open(state_dir).map_err(|e| format!("trace log: {e}"))?;
    let traces = log.load().map_err(|e| format!("load traces: {e}"))?;
    let traces_read = traces.len();
    let samples = deduped_samples_from(&traces);

    // The replay is reported, not depended on. An experiment that aborts on one
    // step and prints nothing is a bad experiment: the phase measurements above are
    // still true whether or not a body can be replayed, and a body that cannot be
    // replayed is itself a finding worth printing rather than a reason to lose the
    // report.
    let mut replay: Vec<ArmReport> = Vec::new();
    if samples.is_empty() {
        notes.push(format!(
            "replay skipped: {traces_read} traces produced {} learnable samples",
            samples.len()
        ));
    } else {
        match zroutery_core::ml::run_training(
            &samples,
            &zroutery_core::ml::TrainingConfig::default(),
        ) {
            Ok(training) => {
                let policy = RewardPolicy::default();
                let candidate = MlPolicy::new(&training, policy.clone());
                match run_comparison(&traces, &candidate, &ReplayBaseline::ALL, &policy) {
                    Ok(comparison) => {
                        replay = comparison
                            .arms
                            .iter()
                            .map(|arm| ArmReport {
                                policy: arm.policy.clone(),
                                measured: arm.requests_measured,
                                success_rate: arm.success_rate,
                                fallback_rate: arm.fallback_rate,
                                mean_latency_ms: arm.mean_latency_ms,
                                mean_cost: arm.mean_cost,
                                mean_utility: arm.mean_observed_utility,
                                distinct_providers: arm.distinct_providers,
                                ineligible: arm.ineligible_selections,
                            })
                            .collect();
                        replay.sort_by(|a, b| a.policy.cmp(&b.policy));
                    }
                    Err(error) => notes.push(format!("replay failed: {error}")),
                }
            }
            Err(error) => {
                notes.push(format!("replay training failed: {error}"));
            }
        }
    }

    let _ = tier_virtual;
    Ok(Report {
        phases,
        promotions,
        replay,
        traces_read,
        notes,
    })
}

async fn drive(
    client: &reqwest::Client,
    addr: SocketAddr,
    environment: &Environment,
    state: &Arc<AppState>,
    name: &str,
    regime: &[Behaviour; 3],
    count: usize,
) -> PhaseReport {
    let before: Vec<(u64, u64, u64)> = environment
        .sites
        .iter()
        .map(|site| site.upstream.counters.snapshot())
        .collect();
    let rankings_before = state.ml_routing().counts().rankings;
    let attached = state.ml_routing().attached_commit();

    let mut ok = 0usize;
    let started = std::time::Instant::now();
    for index in 0..count {
        let response = client
            .post(format!("http://{addr}/v1/messages"))
            .header("x-api-key", TOKEN)
            .json(&json!({
                "model": TIER.virtual_id(),
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "route this"}],
            }))
            .send()
            .await;
        if let Ok(response) = response {
            if response.status().as_u16() == 200 {
                ok += 1;
            }
        }
        if index % 20 == 19 {
            tokio::task::yield_now().await;
        }
    }

    let per_site: Vec<SiteReport> = environment
        .sites
        .iter()
        .enumerate()
        .map(|(index, site)| {
            let (calls, failures, latency) = site.upstream.counters.snapshot();
            let (c0, f0, l0) = before[index];
            let calls = calls - c0;
            let failures = failures - f0;
            let latency = latency.saturating_sub(l0);
            SiteReport {
                name: site.upstream.name.clone(),
                calls,
                failures,
                mean_latency_ms: if calls == 0 {
                    0.0
                } else {
                    latency as f64 / calls as f64
                },
                latency_total_ms: latency as f64,
            }
        })
        .collect();

    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let simulated_upstream_ms: f64 = per_site.iter().map(|s| s.latency_total_ms).sum();
    // A negative overhead would mean the sleeps accounted for more than the phase
    // took, which is impossible. Clamping rather than asserting: a negative number
    // here means the measurement is wrong, and printing it as ~0 would hide that
    // while still reporting a plausible total.
    let overhead_ms = (wall_ms - simulated_upstream_ms).max(0.0);

    let status = state.ml_status();
    PhaseReport {
        name: name.to_string(),
        regime: regime.iter().map(|b| b.label().to_string()).collect(),
        requests: count,
        wall_ms,
        overhead_ms,
        ok,
        per_site,
        rankings: state.ml_routing().counts().rankings - rankings_before,
        attached_commit: attached,
        ingested: status.dataset.ingested,
        samples_collected: status.dataset.samples,
        no_decision_time_input: status.dataset.no_decision_time_input,
        traces_appended: status.traces.as_ref().map(|t| t.appended).unwrap_or(0),
        traces_nothing: status.traces.as_ref().map(|t| t.nothing).unwrap_or(0),
    }
}

async fn promote(state: &Arc<AppState>, revision: &str) -> PromotionReport {
    // `baseline.priority` is the strategy Zroutery actually routes by, so "beats the
    // incumbent" is the claim an operator is making. Only the baseline is chosen here;
    // the evidence floors are the shipped ones.
    let config = RoundConfig {
        gate: PromotionConfig {
            required_baseline: "baseline.priority".to_string(),
            ..PromotionConfig::default()
        },
        ..RoundConfig::default()
    };
    let status = state.ml_run_promotion_round(config, true, Some(revision.to_string()));
    let decision = status.decision.clone();
    PromotionReport {
        revision: revision.to_string(),
        verdict: decision
            .as_ref()
            .map(|d| d.verdict.as_str().to_string())
            .unwrap_or_else(|| "no verdict".into()),
        installed: status.installed.is_some(),
        serving: status.is_serving(),
        paired_requests: decision.as_ref().map(|d| d.paired_requests).unwrap_or(0),
        required_baseline: decision
            .as_ref()
            .map(|d| d.baseline.clone())
            .unwrap_or_default(),
        blockers: decision
            .as_ref()
            .map(|d| {
                d.blockers()
                    .iter()
                    .map(|c| format!("{}: {}", c.name, c.reason))
                    .collect()
            })
            .unwrap_or_default(),
        policy_choices: status.policy_choices.clone(),
    }
}

impl Report {
    /// Render the report as plain text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("ZROUTERY ML ROUTER EXPERIMENT\n");
        out.push_str("===========================\n\n");

        out.push_str("PHASES (upstream-observed, so 'the model chose differently' is a claim\nabout where traffic actually went)\n\n");
        for phase in &self.phases {
            out.push_str(&format!(
                "  {} (n={}, ok={}, rankings={}, attached={})\n",
                phase.name,
                phase.requests,
                phase.ok,
                phase.rankings,
                phase
                    .attached_commit
                    .as_deref()
                    .map(|c| &c[..c.len().min(8)])
                    .unwrap_or("none")
            ));
            out.push_str(&format!("    regime: {}\n", phase.regime.join(", ")));
            // `overhead` is the only figure here that is attributable to this
            // process: wall time minus the environment's own known sleeps, per
            // request. It is what a router's cost actually looks like to a caller.
            out.push_str(&format!(
                "    wall={:.0}ms  simulated_upstream={:.0}ms  overhead={:.0}ms ({:.3}ms/request)\n",
                phase.wall_ms,
                phase.wall_ms - phase.overhead_ms,
                phase.overhead_ms,
                phase.overhead_ms / phase.requests.max(1) as f64
            ));
            out.push_str(&format!(
                "    ingested={} samples={} no_decision_time={} traces_appended={} traces_nothing={}\n",
                phase.ingested,
                phase.samples_collected,
                phase.no_decision_time_input,
                phase.traces_appended,
                phase.traces_nothing
            ));
            for site in &phase.per_site {
                out.push_str(&format!(
                    "    {:<9} calls={:<5} failures={:<5} mean_latency={:.1}ms\n",
                    site.name, site.calls, site.failures, site.mean_latency_ms
                ));
            }
            out.push('\n');
        }

        out.push_str("PROMOTION GATE\n\n");
        for promotion in &self.promotions {
            out.push_str(&format!(
                "  {} -> {} (baseline {}, paired {})\n",
                promotion.revision,
                promotion.verdict,
                promotion.required_baseline,
                promotion.paired_requests
            ));
            out.push_str(&format!(
                "    installed={} serving={}\n",
                promotion.installed, promotion.serving
            ));
            if !promotion.policy_choices.is_empty() {
                out.push_str("    policy choices: ");
                let parts: Vec<String> = promotion
                    .policy_choices
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                out.push_str(&parts.join(", "));
                out.push('\n');
            }
            for blocker in &promotion.blockers {
                out.push_str(&format!("    BLOCKED {blocker}\n"));
            }
        }

        out.push_str("\nREPLAY: every arm over the same body, same constraints\n\n");
        // `cost_usd` is labelled and given six decimals because a per-request cost is a
        // fraction of a cent: at the prices this fixture now uses the arms land
        // between 0.001 and 0.046, so a fixed 2-decimal column would print them all as
        // `0.00` and reintroduce exactly the unreadable axis this change exists to fix.
        out.push_str("  policy                  n   success  fallback  lat(ms)   cost_usd  utility  providers  ineligible\n");
        for arm in &self.replay {
            out.push_str(&format!(
                "  {:<22} {:>3}   {:.3}     {:.3}   {:>7.1}  {:>9.6}  {:>7.4}      {:>2}        {:>2}\n",
                arm.policy,
                arm.measured,
                arm.success_rate,
                arm.fallback_rate,
                arm.mean_latency_ms,
                arm.mean_cost,
                arm.mean_utility,
                arm.distinct_providers,
                arm.ineligible
            ));
        }

        out.push_str(&format!("\ntraces read: {}\n", self.traces_read));
        for note in &self.notes {
            out.push_str(&format!("note: {note}\n"));
        }
        out
    }
}
