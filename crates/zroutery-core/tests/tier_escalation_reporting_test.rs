#![cfg(feature = "ml")]

//! When a tier has no usable candidate and the fallback policy escalates, the
//! answer comes from a tier the caller did not ask for.
//!
//! That is a decision the router has always made and nobody could see:
//! `x-zroutery-degraded` covers the opposite case — an effective pool that is
//! itself unhealthy — so an escalated request looked entirely normal on the wire
//! and in the dashboard. It is also a billing event, since the tier it landed in
//! is generally not the cheap one.
//!
//! Three separate claims are pinned here:
//!
//! * `RequestRecord::escalated_from` names the tier a request was moved away
//!   from, and is absent when nothing moved it;
//! * `x-zroutery-tier` names the tier an answer actually came from;
//! * `tiers_fully_cooling` derives the current condition from breaker state.
//!
//! The third carries the weight. It is deliberately *not* a flag the router
//! sets: a second record of "am I escalated" would be a second opinion about a
//! decision the breaker already knows, and the two could disagree.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::circuit_breaker::{CircuitBreakerConfig, CircuitState};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::Dialect;
use zroutery_core::policy::{DecisionReason, PolicyRevision, RouteDecision, TaskProfileSummary};
use zroutery_core::router::{tiers_fully_cooling, ModelHealth};
use zroutery_core::stats::RecordBuilder;
use zroutery_core::{AppState, ServerHandle};

const TOKEN: &str = "zr-test-token";

// ------------------------------------------------------------------ record projection

/// A decision carrying only what the projection reads.
///
/// Built by hand so these tests are about the projection alone. The real thing
/// is produced end to end below, which is also what keeps this from becoming a
/// test of a literal it wrote itself.
fn decision_with(reason: DecisionReason) -> RouteDecision {
    RouteDecision {
        decision_id: "dec-test".into(),
        timestamp: 0,
        task: TaskProfileSummary::default(),
        policy_id: "p".into(),
        client_id: None,
        candidates: Vec::new(),
        selected: None,
        fallback_chain: vec!["standard-class".into()],
        reason,
        policy_revision: PolicyRevision::default(),
        #[cfg(feature = "ml")]
        ml_ranking: None,
    }
}

#[test]
fn an_escalated_record_names_the_tier_it_was_moved_away_from() {
    let mut builder = RecordBuilder::new(Dialect::Anthropic, "standard-class", false);
    builder.routing_decision(decision_with(DecisionReason::Escalated {
        from_tier: "standard-class".into(),
    }));
    let record = builder.finish(10);
    assert_eq!(
        record.escalated_from.as_deref(),
        Some("standard-class"),
        "the record must name the tier the caller asked for and did not get"
    );
}

/// The negative case, and the one that catches a projection firing
/// unconditionally: a plain decision leaves the field absent.
#[test]
fn a_request_served_from_the_tier_it_asked_for_carries_no_escalation() {
    let mut builder = RecordBuilder::new(Dialect::Anthropic, "standard-class", false);
    builder.routing_decision(decision_with(DecisionReason::PolicySelected));
    let record = builder.finish(10);
    assert_eq!(
        record.escalated_from, None,
        "a decision that moved nothing must not claim an escalation"
    );
}

/// `Degraded` is the opposite direction and shares the word. Reading the wrong
/// variant would report every cost-saving downgrade as an escalation.
#[test]
fn a_degradation_is_not_an_escalation() {
    let mut builder = RecordBuilder::new(Dialect::Anthropic, "reasoning-class", false);
    builder.routing_decision(decision_with(DecisionReason::Degraded {
        from_tier: "reasoning-class".into(),
    }));
    let record = builder.finish(10);
    assert_eq!(
        record.escalated_from, None,
        "moving down a tier must not be reported as moving up one"
    );
}

/// A direct model id has no fallback policy and so no escalation to report.
#[test]
fn a_direct_request_records_no_escalation() {
    let record = RecordBuilder::new(Dialect::OpenAI, "p1-deepseek-v4-pro", false).finish(5);
    assert_eq!(record.escalated_from, None);
    assert!(record.routing_decision.is_none());
}

/// The escalation survives the trip to JSON, because the dashboard reads a
/// serialized record and a field that only exists in memory is not reported.
#[test]
fn the_escalation_survives_serialization() {
    let mut builder = RecordBuilder::new(Dialect::Anthropic, "standard-class", false);
    builder.routing_decision(decision_with(DecisionReason::Escalated {
        from_tier: "standard-class".into(),
    }));
    let record = builder.finish(10);
    let json = serde_json::to_value(&record).expect("record serializes");
    assert_eq!(
        json["escalated_from"], "standard-class",
        "the dashboard reads a serialized record; the field must be on the wire"
    );
}

// ------------------------------------------------------------------ tier-wide outage

fn members(tier: ModelTier, names: &[&str]) -> Vec<ModelEntry> {
    names
        .iter()
        .map(|name| ModelEntry::for_upstream("p1", *name, Some(tier)))
        .collect()
}

fn cooling(model_id: &str) -> ModelHealth {
    ModelHealth {
        model_id: model_id.to_string(),
        state: CircuitState::Open,
        consecutive_failures: 3,
        total_success: 0,
        total_failure: 3,
        avg_latency_ms: 0.0,
        cooldown_remaining_secs: 30,
        rate_limit_remaining_secs: 0,
        last_error: Some("upstream exploded".into()),
    }
}

fn healthy(model_id: &str) -> ModelHealth {
    ModelHealth {
        state: CircuitState::Closed,
        cooldown_remaining_secs: 0,
        ..cooling(model_id)
    }
}

#[test]
fn a_tier_is_fully_cooling_when_every_enabled_member_is() {
    let models = members(ModelTier::Standard, &["a", "b"]);
    let health = vec![cooling("p1-a"), cooling("p1-b")];
    assert_eq!(
        tiers_fully_cooling(&models, &health),
        vec![ModelTier::Standard]
    );
}

/// One healthy member keeps the tier routable, which is exactly why the router
/// did not escalate. An indicator reporting here would claim an escalation that
/// did not happen.
#[test]
fn one_live_member_keeps_a_tier_out_of_the_report() {
    let models = members(ModelTier::Standard, &["a", "b"]);
    let health = vec![cooling("p1-a"), healthy("p1-b")];
    assert!(
        tiers_fully_cooling(&models, &health).is_empty(),
        "a tier with one usable member is still routable"
    );
}

/// The vacuous-empty-set guard. "Every member is cooling" is trivially true of a
/// tier the user configured nothing for, and an unconfigured tier is not an
/// outage.
#[test]
fn an_unconfigured_tier_is_not_an_outage() {
    let models = members(ModelTier::Standard, &["a"]);
    assert!(
        tiers_fully_cooling(&models, &[]).is_empty(),
        "a tier with no members must not be reported as fully cooling"
    );
}

/// A disabled member is not a candidate, so its breaker state says nothing
/// about whether the tier can be routed at all.
#[test]
fn a_disabled_member_does_not_count() {
    let mut models = members(ModelTier::Standard, &["a", "b"]);
    models[1].enabled = false;
    let health = vec![cooling("p1-a"), healthy("p1-b")];
    assert_eq!(
        tiers_fully_cooling(&models, &health),
        vec![ModelTier::Standard],
        "the disabled member is not a candidate, so the tier really is down"
    );
}

/// An unobserved candidate is closed as far as the router is concerned, so a
/// member with no health row at all is not cooling.
#[test]
fn an_unobserved_member_is_not_cooling() {
    let models = members(ModelTier::Standard, &["a", "b"]);
    let health = vec![cooling("p1-a")];
    assert!(
        tiers_fully_cooling(&models, &health).is_empty(),
        "a member the router treats as closed must not be reported as cooling"
    );
}

/// One tier down must not drag its neighbours in with it.
#[test]
fn a_down_tier_does_not_report_its_neighbours() {
    let models = members(ModelTier::Standard, &["a"]);
    let health = vec![cooling("p1-a")];
    assert_eq!(tiers_fully_cooling(&models, &health).len(), 1);
}

#[test]
fn the_report_is_ordered_by_tier() {
    let mut models = members(ModelTier::Reasoning, &["r"]);
    models.extend(members(ModelTier::Fast, &["f"]));
    let health = vec![cooling("p1-r"), cooling("p1-f")];
    assert_eq!(
        tiers_fully_cooling(&models, &health),
        vec![ModelTier::Fast, ModelTier::Reasoning],
        "the order must not depend on how the models were configured"
    );
}

// ------------------------------------------------------------------ end to end

/// Over a real socket: once the requested tier's only member is tripping, the
/// request is answered from the tier above, and both the header and the record
/// say so.
#[tokio::test]
async fn an_escalated_request_names_the_tier_it_landed_in() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr), mock).await;

    // Three failures trip the breaker on the standard tier's only member.
    for _ in 0..3 {
        let resp = h.post("standard-class").await;
        assert_ne!(
            resp.status(),
            200,
            "the mock fails `std`, so this request must not succeed"
        );
    }

    let resp = h.post("standard-class").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "p1-reas",
        "the answer must come from the tier above"
    );
    assert_eq!(
        resp.headers()["x-zroutery-tier"],
        "reasoning",
        "the tier header must name the tier the answer came from, so a caller \
         billing against tiers can see the request left the one it asked for"
    );
    assert!(
        resp.headers().get("x-zroutery-degraded").is_none(),
        "an escalation is not a degraded pool — the effective pool is healthy, \
         which is why this was invisible before"
    );

    h.shutdown().await;
}

/// The other half: a healthy request reports the tier that was asked for, so
/// the header means the same thing in both cases and a client can compare it
/// against what it sent.
#[tokio::test]
async fn an_unescalated_request_names_the_tier_it_asked_for() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr), mock).await;

    let resp = h.post("reasoning-class").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "p1-reas");
    assert_eq!(resp.headers()["x-zroutery-tier"], "reasoning");
    assert!(resp.headers().get("x-zroutery-degraded").is_none());

    h.shutdown().await;
}

/// The escalation is recorded against the request that caused it, so a week
/// later the dashboard can still answer "did this happen, how often, for how
/// long" — which a toast never could.
#[tokio::test]
async fn the_record_keeps_the_escalation_after_the_fact() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr), mock).await;

    for _ in 0..3 {
        let _ = h.post("standard-class").await;
    }
    let resp = h.post("standard-class").await;
    assert_eq!(resp.status(), 200);

    let recent = h.state.stats().recent(50);
    let escalated: Vec<_> = recent
        .iter()
        .filter(|r| r.escalated_from.is_some())
        .collect();
    assert_eq!(
        escalated.len(),
        1,
        "exactly the one request that escalated should carry the fact"
    );
    assert_eq!(
        escalated[0].escalated_from.as_deref(),
        Some("standard-class")
    );
    assert_eq!(
        escalated[0].resolved_model.as_deref(),
        Some("p1-reas"),
        "and it must agree with what actually answered it"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ notification de-duplication

use zroutery_core::{tier_escalation_change, TierAnnouncement};

/// A watcher that keeps the previous set and announces what changed. This is the
/// whole of the de-duplication, and the tests below drive it the way the tray
/// watcher does rather than calling the comparison directly, because "once" is a
/// claim about the loop and not about one call.
#[derive(Default)]
struct Watcher {
    previous: Vec<ModelTier>,
}

impl Watcher {
    fn new(initial: &[ModelTier]) -> Self {
        Watcher {
            previous: initial.to_vec(),
        }
    }

    /// One observation. Returns what the user would have been told.
    fn observe(&mut self, current: &[ModelTier]) -> Option<TierAnnouncement> {
        let announcement = tier_escalation_change(&self.previous, current).announcement();
        self.previous = current.to_vec();
        announcement
    }
}

const FAST: &[ModelTier] = &[ModelTier::Fast];
const FAST_AND_STD: &[ModelTier] = &[ModelTier::Fast, ModelTier::Standard];

/// The claim the whole feature rests on: a steady outage is announced once, not
/// once per observation — and observations are what stand in for requests, since
/// a request that gets escalated is not what the watcher watches.
#[test]
fn a_steady_outage_announces_once_and_not_again() {
    let mut watcher = Watcher::default();
    assert_eq!(
        watcher.observe(&[]),
        None,
        "the app starting mid-outage is not news"
    );

    assert_eq!(
        watcher.observe(FAST),
        Some(TierAnnouncement::Escalated(vec![ModelTier::Fast])),
        "the trip itself is announced"
    );
    // Fifty more observations of an unchanged set: one provider flapping for a
    // minute, or an outage during which every single request was escalated.
    for _ in 0..50 {
        assert_eq!(
            watcher.observe(FAST),
            None,
            "an unchanged set must stay silent, however long it lasts"
        );
    }
}

/// And the other end: the recovery is announced once, and silence returns.
#[test]
fn a_recovery_announces_once_and_not_again() {
    let mut watcher = Watcher::new(FAST);
    assert_eq!(
        watcher.observe(&[]),
        Some(TierAnnouncement::Recovered(vec![ModelTier::Fast]))
    );
    for _ in 0..50 {
        assert_eq!(watcher.observe(&[]), None);
    }
}

/// A provider that flaps produces one announcement per episode, in order, and
/// never two for the same episode.
#[test]
fn flapping_announces_once_per_episode() {
    let mut watcher = Watcher::default();
    assert_eq!(
        watcher.observe(FAST),
        Some(TierAnnouncement::Escalated(vec![ModelTier::Fast]))
    );
    assert_eq!(
        watcher.observe(&[]),
        Some(TierAnnouncement::Recovered(vec![ModelTier::Fast]))
    );
    assert_eq!(
        watcher.observe(FAST),
        Some(TierAnnouncement::Escalated(vec![ModelTier::Fast]))
    );
    assert_eq!(
        watcher.observe(&[]),
        Some(TierAnnouncement::Recovered(vec![ModelTier::Fast]))
    );
}

/// A trip and a recovery inside one observation window is one event, and an
/// escalation outranks it. Reporting both would put two notifications in front
/// of the user for a single outage.
#[test]
fn an_outage_and_its_recovery_in_one_window_announce_once() {
    let mut watcher = Watcher::new(FAST);
    assert_eq!(
        watcher.observe(&[]),
        Some(TierAnnouncement::Recovered(vec![ModelTier::Fast])),
        "with nothing opened, the recovery is the only end that is news"
    );

    // And the other direction: something opened while something else closed.
    let mut watcher = Watcher::new(&[ModelTier::Standard, ModelTier::Reasoning]);
    assert_eq!(
        watcher.observe(&[ModelTier::Fast]),
        Some(TierAnnouncement::Escalated(vec![ModelTier::Fast])),
        "the escalation is what the user needs to know, so the recovery beside \
         it does not get a second notification"
    );
}

/// Members are compared, not positions: a set that comes back in a different
/// order is the same set, and reordering must not manufacture an announcement.
#[test]
fn reordering_is_not_a_change() {
    let mut watcher = Watcher::new(&[ModelTier::Standard, ModelTier::Fast]);
    assert_eq!(
        watcher.observe(&[ModelTier::Fast, ModelTier::Standard]),
        None,
        "the same tiers in a different order are the same outage"
    );
}

/// Two tiers going down together is one announcement naming both, not two
/// notifications — and it names them in tier order whatever order they were
/// observed in.
#[test]
fn two_tiers_tripping_together_announce_once_naming_both() {
    let mut watcher = Watcher::default();
    assert_eq!(
        watcher.observe(&[ModelTier::Standard, ModelTier::Fast]),
        Some(TierAnnouncement::Escalated(vec![
            ModelTier::Fast,
            ModelTier::Standard
        ]))
    );
}

/// The comparison reports the change even where the announcement collapses it,
/// so a caller that wants to draw both ends in a dashboard is not limited to the
/// single notification.
#[test]
fn the_change_reports_both_ends_where_the_announcement_keeps_one() {
    let change = tier_escalation_change(
        &[ModelTier::Standard, ModelTier::Reasoning],
        &[ModelTier::Fast],
    );
    assert_eq!(change.opened, vec![ModelTier::Fast]);
    assert_eq!(
        change.closed,
        vec![ModelTier::Standard, ModelTier::Reasoning]
    );
    assert_eq!(
        change.announcement(),
        Some(TierAnnouncement::Escalated(vec![ModelTier::Fast])),
        "the announcement is deliberately narrower than the change"
    );
    assert!(!change.is_empty());
}

#[test]
fn no_change_is_empty_and_says_nothing() {
    let change = tier_escalation_change(FAST_AND_STD, FAST_AND_STD);
    assert!(change.is_empty());
    assert_eq!(change.announcement(), None);
}

// ------------------------------------------------------------------ the switch

/// Asserted against a default-constructed **and** a deserialised document,
/// because "off unless asked" is a claim about what those configurations do and
/// not about what `Default`'s body says.
#[test]
fn tier_escalation_notifications_are_off_unless_asked_for() {
    assert!(!AppConfig::default().notifications.notify_on_tier_escalation);

    // A document written before this setting existed deserialises without it.
    let older = serde_json::json!({ "server": { "port": 8787 } });
    let parsed: AppConfig = serde_json::from_value(older).expect("an older document still parses");
    assert!(!parsed.notifications.notify_on_tier_escalation);

    // And the setting round-trips rather than being silently dropped on save.
    let mut asked = AppConfig::default();
    asked.notifications.notify_on_tier_escalation = true;
    let back: AppConfig =
        serde_json::from_value(serde_json::to_value(&asked).expect("serializes")).expect("parses");
    assert!(back.notifications.notify_on_tier_escalation);
}

/// The switch must not be able to reach routing. It lives on the window layer's
/// block, not on `routing`, so a document that turns notifications on cannot
/// have altered how a request is routed.
#[test]
fn the_notification_switch_lives_apart_from_routing() {
    let asked = serde_json::json!({ "notifications": { "notify_on_tier_escalation": true } });
    let cfg: AppConfig = serde_json::from_value(asked).expect("parses");
    assert!(cfg.notifications.notify_on_tier_escalation);
    assert!(
        cfg.routing == AppConfig::default().routing,
        "asking for a notification must leave routing configuration untouched"
    );
}

// ------------------------------------------------------------------ harness

#[derive(Clone)]
struct Mock;

/// Fails `std`, answers `reas`, so a trip on one tier is observable as an
/// escalation rather than as a dead request.
async fn mock_upstream(State(_mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    if model.starts_with("std") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded"}})),
        )
            .into_response();
    }
    Json(json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}
    }))
    .into_response()
}

async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock;
    let app = axum::Router::new()
        .route("/chat/completions", post(mock_upstream))
        .route("/v1/chat/completions", post(mock_upstream))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

fn config_for(mock: SocketAddr) -> AppConfig {
    let mut provider = ProviderConfig::new("p1", "P1", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = "provider:p1".into();
    provider.timeout_secs = 5;

    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![provider];
    cfg.models = vec![
        ModelEntry::for_upstream("p1", "std", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p1", "reas", Some(ModelTier::Reasoning)),
    ];
    // A short threshold so three requests trip it, and a long timeout so it
    // stays open for the rest of the test.
    cfg.routing.circuit_breaker = CircuitBreakerConfig {
        failure_threshold: 3,
        timeout_secs: 600,
        ..CircuitBreakerConfig::default()
    };
    cfg
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    state: Arc<AppState>,
    client: reqwest::Client,
}

impl Harness {
    async fn start(cfg: AppConfig, _mock: Mock) -> Harness {
        let state = Arc::new(AppState::new(
            cfg,
            Arc::new(MemorySecretStore::new().with("provider:p1", "sk-p1")),
        ));
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        let base = format!("http://{}", server.addr);
        Harness {
            base,
            server: Some(server),
            state,
            client: reqwest::Client::new(),
        }
    }

    async fn post(&self, model: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/v1/chat/completions", self.base))
            .header("x-api-key", TOKEN)
            .json(&json!({
                "model": model,
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap()
    }

    async fn shutdown(mut self) {
        if let Some(s) = self.server.take() {
            s.stop().await;
        }
    }
}
