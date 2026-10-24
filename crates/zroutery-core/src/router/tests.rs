//! The router fixtures: planning, failover, circuit breakers, selection strategy and scoring.

use super::*;

use crate::circuit_breaker::CircuitState;
use crate::config::{
    AppConfig, ClassifierConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
    RoutingStrategy,
};
use crate::error::Error;
use crate::ir::Capability;
use crate::policy::{PolicyFallback, PolicyPreference, PolicyRequirements};
use crate::registry::{Registry, Resolution};
use std::sync::Arc;

fn cfg_with(models: Vec<ModelEntry>) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig::new(
        "p1",
        "P1",
        ProviderKind::OpenAICompatible,
    ));
    cfg.providers
        .push(ProviderConfig::new("p2", "P2", ProviderKind::Anthropic));
    cfg.models = models;
    cfg
}

fn reg(cfg: AppConfig) -> Registry {
    Registry::new(Arc::new(cfg))
}

fn ids(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.exposed_id.as_str()).collect()
}

#[test]
fn direct_request_does_not_failover() {
    let r = reg(cfg_with(vec![
        ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
    ]));
    let router = Router::new();
    let plan = router
        .plan(&r, &Resolution::Direct("p1-a".into()), &[])
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-a"]);
}

#[test]
fn tier_plan_follows_priority_and_respects_max_attempts() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "first", Some(ModelTier::Reasoning)).with_priority(0),
        ModelEntry::for_upstream("p2", "second", Some(ModelTier::Reasoning)).with_priority(10),
        ModelEntry::for_upstream("p1", "third", Some(ModelTier::Reasoning)).with_priority(20),
    ]);
    cfg.routing.max_attempts = 2;
    let r = reg(cfg);
    let router = Router::new();
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Reasoning), &[])
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-first", "p2-second"]);
}

#[test]
fn failover_disabled_yields_single_attempt() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "a", Some(ModelTier::Fast)),
        ModelEntry::for_upstream("p2", "b", Some(ModelTier::Fast)),
    ]);
    cfg.routing.failover = false;
    let r = reg(cfg);
    let plan = Router::new()
        .plan(&r, &Resolution::Tier(ModelTier::Fast), &[])
        .unwrap();
    assert_eq!(plan.len(), 1);
}

#[test]
fn empty_tier_is_an_error() {
    let r = reg(cfg_with(vec![ModelEntry::for_upstream("p1", "a", None)]));
    let err = Router::new()
        .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn circuit_breaker_demotes_then_recovers() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "bad", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p2", "good", Some(ModelTier::Standard)).with_priority(5),
    ]);
    cfg.routing.circuit_breaker.timeout_secs = 0;
    let routing = cfg.routing.clone();
    let r = reg(cfg);
    let router = Router::new();

    assert_eq!(
        ids(&router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap())[0],
        "p1-bad"
    );

    let err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-bad", &err, &routing);
    }
    assert!(router.is_cooling("p1-bad"));
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap();
    assert_eq!(ids(&plan), vec!["p2-good", "p1-bad"]);
    assert!(!plan[0].degraded && plan[1].degraded);

    // The timeout has already elapsed (0s), so the router offers the model
    // as a half-open probe. One probe success is not enough; the configured
    // success threshold (2) must be reached before it is healthy again.
    assert!(router.allow_request("p1-bad"));
    router.report_success("p1-bad", 10, &routing);
    assert!(
        router.health_snapshot()[0].state == CircuitState::HalfOpen,
        "a single probe success should not close the breaker yet"
    );
    router.report_success("p1-bad", 10, &routing);
    assert_eq!(router.health_snapshot()[0].state, CircuitState::Closed);
    assert_eq!(
        ids(&router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap())[0],
        "p1-bad"
    );
}

#[test]
fn a_recovered_breaker_needs_fresh_evidence_to_trip_again() {
    let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "flaky",
        Some(ModelTier::Reasoning),
    )]);
    cfg.routing.circuit_breaker.timeout_secs = 0;
    let routing = cfg.routing.clone();
    let router = Router::new();
    let err = Error::Timeout(1);

    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-flaky", &err, &routing);
    }
    assert!(router.is_cooling("p1-flaky"));

    // Move through half-open to a fully closed breaker.
    assert!(router.allow_request("p1-flaky"));
    router.report_success("p1-flaky", 10, &routing);
    router.report_success("p1-flaky", 10, &routing);
    assert!(!router.is_cooling("p1-flaky"));

    // A single transient blip after recovery is not a pattern yet.
    router.report_failure("p1-flaky", &err, &routing);
    assert!(
        !router.is_cooling("p1-flaky"),
        "one failure after recovery should not re-open the breaker"
    );

    // Reaching the threshold again with new failures does.
    for _ in 1..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-flaky", &err, &routing);
    }
    assert!(router.is_cooling("p1-flaky"));
}

#[test]
fn retain_models_drops_entries_that_no_longer_exist() {
    let cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "kept",
        Some(ModelTier::Reasoning),
    )]);
    let routing = cfg.routing.clone();
    let router = Router::new();
    router.report_failure("p1-kept", &Error::Timeout(1), &routing);
    router.report_failure("p1-gone", &Error::Timeout(1), &routing);

    let known: std::collections::HashSet<String> = ["p1-kept".to_string()].into_iter().collect();
    router.retain_models(|id| known.contains(id));

    let snapshot = router.health_snapshot();
    let ids: Vec<&str> = snapshot.iter().map(|h| h.model_id.as_str()).collect();
    assert_eq!(ids, vec!["p1-kept"]);
}

#[test]
fn all_open_with_timeout_elapsed_still_produces_a_probe_plan() {
    let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "only",
        Some(ModelTier::Reasoning),
    )]);
    cfg.routing.circuit_breaker.timeout_secs = 0;
    let routing = cfg.routing.clone();
    let r = reg(cfg);
    let router = Router::new();
    for _ in 0..10 {
        router.report_failure("p1-only", &Error::Timeout(1), &routing);
    }
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Reasoning), &[])
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-only"]);
    assert!(plan[0].degraded);
}

#[test]
fn client_errors_do_not_open_the_breaker() {
    let cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "a",
        Some(ModelTier::Reasoning),
    )]);
    let routing = cfg.routing.clone();
    let router = Router::new();
    for _ in 0..10 {
        router.report_failure("p1-a", &Error::invalid("bad json"), &routing);
    }
    assert!(!router.is_cooling("p1-a"));
    assert!(router.health_snapshot().is_empty());
}

#[test]
fn success_resets_streak_and_tracks_latency() {
    let cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "a",
        Some(ModelTier::Reasoning),
    )]);
    let routing = cfg.routing.clone();
    let router = Router::new();
    router.report_failure("p1-a", &Error::Timeout(1), &routing);
    router.report_success("p1-a", 200, &routing);
    let snap = router.health_snapshot();
    assert_eq!(snap[0].consecutive_failures, 0);
    assert_eq!(snap[0].total_success, 1);
    assert_eq!(snap[0].total_failure, 1);
    assert_eq!(snap[0].avg_latency_ms, 200.0);
    router.report_success("p1-a", 400, &routing);
    assert!((router.health_snapshot()[0].avg_latency_ms - 260.0).abs() < 0.001);
}

#[test]
fn success_creates_health_with_routing_breaker_config() {
    let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "a",
        Some(ModelTier::Reasoning),
    )]);
    cfg.routing.circuit_breaker.failure_threshold = 1;
    let routing = cfg.routing.clone();
    let router = Router::new();

    // First observation is a success, so the health entry must be created
    // with the routing config rather than the default breaker config.
    router.report_success("p1-a", 10, &routing);
    router.report_failure("p1-a", &Error::Timeout(1), &routing);
    assert!(
        router.is_cooling("p1-a"),
        "routing failure_threshold=1 should open after one failure"
    );
}

#[test]
fn round_robin_rotates() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "a", Some(ModelTier::Fast)),
        ModelEntry::for_upstream("p2", "b", Some(ModelTier::Fast)),
    ]);
    cfg.routing.strategy = RoutingStrategy::RoundRobin;
    let r = reg(cfg);
    let router = Router::new();
    let first = ids(&router
        .plan(&r, &Resolution::Tier(ModelTier::Fast), &[])
        .unwrap())[0]
        .to_string();
    let second = ids(&router
        .plan(&r, &Resolution::Tier(ModelTier::Fast), &[])
        .unwrap())[0]
        .to_string();
    assert_ne!(first, second);
}

#[test]
fn lowest_latency_prefers_the_fast_model() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "slow", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "fast", Some(ModelTier::Standard)),
    ]);
    cfg.routing.strategy = RoutingStrategy::LowestLatency;
    let routing = cfg.routing.clone();
    let r = reg(cfg);
    let router = Router::new();
    router.report_success("p1-slow", 3000, &routing);
    router.report_success("p2-fast", 300, &routing);
    assert_eq!(
        ids(&router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap())[0],
        "p2-fast"
    );
}

#[test]
fn weighted_random_covers_all_and_favours_weight() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "heavy", Some(ModelTier::Reasoning)),
        ModelEntry::for_upstream("p2", "light", Some(ModelTier::Reasoning)),
    ]);
    cfg.models[0].weight = 9;
    cfg.models[1].weight = 1;
    cfg.routing.strategy = RoutingStrategy::WeightedRandom;
    let r = reg(cfg);
    let router = Router::new();
    let mut heavy_first = 0;
    for _ in 0..400 {
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Reasoning), &[])
            .unwrap();
        assert_eq!(
            plan.len(),
            2,
            "every member must remain in the failover chain"
        );
        if plan[0].exposed_id == "p1-heavy" {
            heavy_first += 1;
        }
    }
    assert!(heavy_first > 280, "heavy won only {heavy_first}/400");
}

// ------------------------------------------------------- classifier pool

fn classifier_cfg_with(models: Vec<ModelEntry>, candidates: &[(&str, i32)]) -> AppConfig {
    let mut cfg = cfg_with(models);
    cfg.classifier = ClassifierConfig {
        enabled: true,
        candidates: candidates
            .iter()
            .map(|(model, priority)| crate::config::ClassifierCandidate {
                model: (*model).to_string(),
                priority: *priority,
                enabled: true,
            })
            .collect(),
        ..ClassifierConfig::default()
    };
    cfg
}

#[test]
fn classifier_pool_follows_candidate_priority_not_tier_priority() {
    // glm is the *last* member of its tier but the *first* classifier
    // candidate; the two orderings are independent.
    let r = reg(classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "glm", Some(ModelTier::Fast)).with_priority(50),
            ModelEntry::for_upstream("p2", "deepseek", Some(ModelTier::Standard)).with_priority(0),
        ],
        &[("p2-deepseek", 20), ("p1-glm", 10)],
    ));
    let plan = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-glm", "p2-deepseek"]);
}

#[test]
fn classifier_candidates_may_point_at_unclassified_models() {
    // A model with no tier is still a perfectly good classifier.
    let r = reg(classifier_cfg_with(
        vec![ModelEntry::for_upstream("p1", "glm", None)],
        &[("p1-glm", 10)],
    ));
    let plan = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-glm"]);
}

#[test]
fn classifier_pool_respects_its_own_attempt_budget() {
    let mut cfg = classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "a", None),
            ModelEntry::for_upstream("p2", "b", None),
            ModelEntry::for_upstream("p1", "c", None),
        ],
        &[("p1-a", 10), ("p2-b", 20), ("p1-c", 30)],
    );
    cfg.classifier.max_attempts = 2;
    let r = reg(cfg);
    let plan = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-a", "p2-b"]);
}

#[test]
fn classifier_failover_off_yields_one_attempt() {
    let mut cfg = classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "a", None),
            ModelEntry::for_upstream("p2", "b", None),
        ],
        &[("p1-a", 10), ("p2-b", 20)],
    );
    cfg.classifier.failover = false;
    let r = reg(cfg);
    let plan = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap();
    assert_eq!(plan.len(), 1);
}

#[test]
fn unknown_or_disabled_candidates_are_skipped_not_fatal() {
    // Two live candidates and one dangling reference: the pool still works.
    let mut cfg = classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "a", None),
            ModelEntry::for_upstream("p2", "b", None),
        ],
        &[("p1-a", 10), ("p3-missing", 5), ("p2-b", 20)],
    );
    // A disabled candidate entry is skipped too.
    cfg.classifier.candidates[2].enabled = false;
    let r = reg(cfg);
    let plan = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-a"]);
}

#[test]
fn an_empty_classifier_pool_is_an_error() {
    // No candidates at all, or only ones that resolve to nothing: the
    // request must fail rather than silently fall through to the main pool.
    let cfg = classifier_cfg_with(vec![ModelEntry::for_upstream("p1", "a", None)], &[]);
    let r = reg(cfg);
    let err = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));

    let cfg = classifier_cfg_with(vec![], &[("p1-gone", 10)]);
    let r = reg(cfg);
    assert!(Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .is_err());
}

#[test]
fn classifier_health_is_shared_with_the_main_pool() {
    // A model that fails as a classifier must be demoted for main requests
    // too: the circuit breaker is keyed by model, not by pool.
    let cfg = classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "glm", Some(ModelTier::Standard)).with_priority(0),
            ModelEntry::for_upstream("p2", "main", Some(ModelTier::Standard)).with_priority(5),
        ],
        &[("p1-glm", 10)],
    );
    let routing = cfg.routing.clone();
    let r = reg(cfg);
    let router = Router::new();

    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-glm", &Error::Timeout(1), &routing);
    }
    // The classifier pool has nothing left: glm is cooling and it was the
    // only candidate.
    assert!(router.plan_classifier(&r, &r.config().classifier).is_err());

    // ...and the main tier plan demotes glm for main requests as well.
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap();
    assert_eq!(ids(&plan)[0], "p2-main");
}

#[test]
fn classifier_balanced_strategy_falls_back_to_priority() {
    // Elections are per tier; the classifier pool has none, so Balanced
    // must not silently produce an unsorted plan.
    let mut cfg = classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "a", None).with_priority(30),
            ModelEntry::for_upstream("p2", "b", None).with_priority(10),
        ],
        &[("p1-a", 30), ("p2-b", 10)],
    );
    cfg.classifier.strategy = RoutingStrategy::Balanced;
    let r = reg(cfg);
    let plan = Router::new()
        .plan_classifier(&r, &r.config().classifier)
        .unwrap();
    assert_eq!(ids(&plan), vec!["p2-b", "p1-a"]);
}

#[test]
fn classifier_round_robin_rotates_independently_of_tiers() {
    let mut cfg = classifier_cfg_with(
        vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
        ],
        &[("p1-a", 10), ("p2-b", 10)],
    );
    cfg.classifier.strategy = RoutingStrategy::RoundRobin;
    let r = reg(cfg);
    let router = Router::new();
    let first = ids(&router.plan_classifier(&r, &r.config().classifier).unwrap())[0].to_string();
    let second = ids(&router.plan_classifier(&r, &r.config().classifier).unwrap())[0].to_string();
    assert_ne!(first, second);
}

// ---------------------------------------- capability filter (strict/soft)

#[test]
fn request_capability_filter_is_fail_closed_when_none_match() {
    // Both models lack vision; a request-derived vision requirement must
    // not be softened back into the unfiltered pool.
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "text-only", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p2", "also-text", Some(ModelTier::Standard)).with_priority(1),
    ]);
    cfg.routing.rectifier.enabled = false;
    let r = reg(cfg);
    let router = Router::new();
    let err = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn a_shared_rate_limit_gates_every_model_of_the_provider() {
    let r = reg(cfg_with(vec![
        ModelEntry::for_upstream("p1", "limited", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "sibling", Some(ModelTier::Standard)).with_priority(1),
        ModelEntry::for_upstream("p2", "elsewhere", Some(ModelTier::Standard)).with_priority(2),
    ]));
    let router = Router::new();
    router.set_group_limited_providers(["p1".to_string()]);
    let routing = r.config().routing.clone();

    let rate_limit = crate::failure::ClassifiedFailure::from_core_error(Error::Upstream {
        provider: "p1".into(),
        status: 429,
        body: "slow down".into(),
        retry_after: None,
    });
    router.record_classified_attempt("p1-limited", "p1", &rate_limit, &routing);

    assert!(router.is_provider_gated("p1"));
    assert!(
        !router.is_provider_gated("p2"),
        "another provider's quota is unaffected"
    );
    // Neither the model that saw the 429 nor its sibling may be planned; the
    // per-model circuit is still closed, because a quota is not a sick model.
    assert!(!router.is_cooling("p1-limited"));
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap();
    assert_eq!(ids(&plan), vec!["p2-elsewhere"]);
    assert!(!router.allow_request_for("p1-sibling", "p1"));
    assert!(router.allow_request_for("p2-elsewhere", "p2"));
}

#[test]
fn a_rate_limit_does_not_gate_a_provider_that_does_not_share_its_quota() {
    let r = reg(cfg_with(vec![
        ModelEntry::for_upstream("p1", "limited", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p1", "sibling", Some(ModelTier::Standard)),
    ]));
    let router = Router::new();
    let routing = r.config().routing.clone();
    let rate_limit = crate::failure::ClassifiedFailure::from_core_error(Error::Upstream {
        provider: "p1".into(),
        status: 429,
        body: "slow down".into(),
        retry_after: None,
    });
    router.record_classified_attempt("p1-limited", "p1", &rate_limit, &routing);
    assert!(!router.is_provider_gated("p1"));
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-limited", "p1-sibling"]);
}

#[test]
fn unknown_thinking_is_remediated_to_a_degraded_candidate() {
    // A client that selects a reasoning variant must not lose the whole tier
    // because no member declares thinking support. The documented fallback
    // serves an eligible member and marks it degraded.
    let r = reg(cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "reasoner",
        Some(ModelTier::Reasoning),
    )]));
    let router = Router::new();
    let plan = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Reasoning),
            &[Capability::Thinking],
        )
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-reasoner"]);
    assert!(plan[0].degraded);
}

#[test]
fn thinking_fallback_off_keeps_unknown_thinking_fail_closed() {
    let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "reasoner",
        Some(ModelTier::Reasoning),
    )]);
    cfg.routing.rectifier.thinking_fallback = false;
    let r = reg(cfg);
    let router = Router::new();
    let err = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Reasoning),
            &[Capability::Thinking],
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn declared_thinking_support_is_not_degraded() {
    let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "reasoner",
        Some(ModelTier::Reasoning),
    )]);
    cfg.models[0].capabilities.thinking = true;
    let r = reg(cfg);
    let router = Router::new();
    let plan = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Reasoning),
            &[Capability::Thinking],
        )
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-reasoner"]);
    assert!(!plan[0].degraded);
}

#[test]
fn strict_mode_rejects_when_no_candidate_satisfies_capability() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "text-only", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "also-text", Some(ModelTier::Standard)),
    ]);
    cfg.routing.strict_capability_filter = true;
    cfg.routing.rectifier.enabled = false;
    let r = reg(cfg);
    let router = Router::new();
    let err = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn capability_filter_prefers_matching_candidates() {
    // p1 has vision, p2 does not. A request requiring vision should
    // prefer p1, regardless of strict mode.
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "vision", Some(ModelTier::Standard)).with_priority(10),
        ModelEntry::for_upstream("p2", "no-vision", Some(ModelTier::Standard)).with_priority(0),
    ]);
    cfg.models[0].capabilities.vision = true;
    cfg.routing.strict_capability_filter = true;
    cfg.routing.rectifier.enabled = false;
    let r = reg(cfg);
    let router = Router::new();
    let plan = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap();
    assert_eq!(ids(&plan), vec!["p1-vision"]);
}

#[test]
fn strict_mode_accepts_when_at_least_one_candidate_matches() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "vision", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p2", "no-vision", Some(ModelTier::Standard)).with_priority(5),
    ]);
    cfg.models[0].capabilities.vision = true;
    cfg.routing.strict_capability_filter = true;
    cfg.routing.rectifier.enabled = false;
    let r = reg(cfg);
    let router = Router::new();
    let plan = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap();
    // Only the vision-capable model should be returned.
    assert_eq!(ids(&plan), vec!["p1-vision"]);
}

#[test]
fn strict_mode_ignores_when_no_capabilities_required() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
    ]);
    cfg.routing.strict_capability_filter = true;
    let r = reg(cfg);
    let router = Router::new();
    let plan = router
        .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap();
    assert_eq!(plan.len(), 2);
}

#[test]
fn request_capability_filter_cannot_be_disabled_by_legacy_config() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
    ]);
    // The legacy switch remains readable for compatibility, but request
    // capabilities are a hard Core input and are still enforced.
    cfg.routing.capability_filter = false;
    cfg.routing.rectifier.enabled = false;
    let r = reg(cfg);
    let router = Router::new();
    let err = router
        .plan(
            &r,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

// ------------------------------------------------ observation-aware scoring

#[test]
fn observation_fast_latency_scores_higher() {
    let router = Router::new();
    // model-a: fast (100ms), model-b: slow (3000ms)
    router.record_outcome("p1-a", "p1", 100.0, Some(50.0));
    router.record_outcome("p2-b", "p2", 3000.0, Some(800.0));

    let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
    let members = vec![&m1, &m2];
    let pref = PolicyPreference {
        latency_weight: 1.0,
        health_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };

    let (sorted, _) = router.score_and_sort(members, &pref, None);
    assert_eq!(
        sorted[0].exposed_id(),
        "p1-a",
        "fast model should rank first"
    );
}

#[test]
fn observation_healthy_beats_degraded() {
    let router = Router::new();
    // model-a: healthy (1 success)
    router.record_outcome("p1-a", "p1", 200.0, None);
    // model-b: 2 consecutive failures = Degraded
    router.record_failure("p2-b", "p2");
    router.record_failure("p2-b", "p2");

    let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
    let members = vec![&m1, &m2];
    let pref = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };

    let (sorted, breakdowns) = router.score_and_sort(members, &pref, None);
    assert_eq!(
        sorted[0].exposed_id(),
        "p1-a",
        "healthy should rank above degraded"
    );

    // Verify the health scores are actually different.
    let bd_a = breakdowns.iter().find(|(id, _, _)| id == "p1-a").unwrap();
    let bd_b = breakdowns.iter().find(|(id, _, _)| id == "p2-b").unwrap();
    assert!(
        bd_a.1.health > bd_b.1.health,
        "healthy ({}) > degraded ({})",
        bd_a.1.health,
        bd_b.1.health
    );
}

#[test]
fn observation_unknown_falls_back_to_legacy() {
    let cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
    ]);
    let routing = cfg.routing.clone();
    let router = Router::new();

    // Legacy health: model-a has some history, model-b has none.
    router.report_success("p1-a", 200, &routing);
    // No observations recorded — scoring should use legacy health_score.

    let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
    let members = vec![&m1, &m2];
    let pref = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };

    let (sorted, breakdowns) = router.score_and_sort(members, &pref, None);
    // Both have unknown observations: p1-a has legacy success (ratio=1.0),
    // p2-b has no legacy data (optimistic=1.0). Scores should be equal.
    assert_eq!(sorted.len(), 2);
    let bd_a = breakdowns.iter().find(|(id, _, _)| id == "p1-a").unwrap();
    let bd_b = breakdowns.iter().find(|(id, _, _)| id == "p2-b").unwrap();
    assert!(
        (bd_a.1.health - bd_b.1.health).abs() < 0.001,
        "both should use legacy health; got {} vs {}",
        bd_a.1.health,
        bd_b.1.health,
    );
}

#[test]
fn observation_overrides_legacy_circuit_open() {
    let cfg = cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "a",
        Some(ModelTier::Standard),
    )]);
    let routing = cfg.routing.clone();
    let router = Router::new();

    // Trip the circuit breaker via legacy health.
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-a", &Error::Timeout(1), &routing);
    }
    assert_eq!(router.health_score("p1-a"), 0.0, "legacy: circuit open");

    // Record a fresh observation success.
    router.record_outcome("p1-a", "p1", 100.0, Some(50.0));

    let m = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
    let members = vec![&m];
    let pref = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };

    let (_, breakdowns) = router.score_and_sort(members, &pref, None);
    // Observation health is Healthy with success_rate=1.0, so score=1.0.
    // Legacy would give 0.0 (circuit open). Observation should win.
    assert!(
        breakdowns[0].1.health > 0.5,
        "observation health should override legacy; got {}",
        breakdowns[0].1.health,
    );
}

#[test]
fn observation_recording_flows_through_to_plan_with_policy() {
    let mut cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "fast", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p2", "slow", Some(ModelTier::Standard)).with_priority(0),
    ]);
    cfg.routing.strategy = crate::config::RoutingStrategy::Priority;
    let r = reg(cfg);
    let router = Router::new();

    // Record observations: p1-fast is fast, p2-slow is slow.
    router.record_outcome("p1-fast", "p1", 100.0, Some(50.0));
    router.record_outcome("p2-slow", "p2", 3000.0, Some(800.0));

    let pref = PolicyPreference {
        latency_weight: 1.0,
        health_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let requirements = PolicyRequirements::default();

    let (candidates, _decision) = router
        .plan_with_policy(
            &r,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &pref,
            &PolicyFallback::default(),
            None,
        )
        .unwrap();

    assert_eq!(
        candidates[0].model_id(),
        "p1-fast",
        "plan_with_policy should prefer the faster model via observations"
    );
}

// --------------------------------------------------- 4B feedback loop tests

#[test]
fn observation_adapts_across_requests() {
    // Setup: two models, same tier
    let cfg = cfg_with(vec![
        ModelEntry::for_upstream("p1", "model-a", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p2", "model-b", Some(ModelTier::Standard)),
    ]);
    let _r = reg(cfg);
    let router = Router::new();

    // Request 1: model-a is fast (100ms), model-b is slow (2000ms)
    router.record_outcome("p1-model-a", "p1", 100.0, Some(50.0));
    router.record_outcome("p2-model-b", "p2", 2000.0, Some(800.0));

    let pref = PolicyPreference {
        latency_weight: 1.0,
        health_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let ma = ModelEntry::for_upstream("p1", "model-a", Some(ModelTier::Standard));
    let mb = ModelEntry::for_upstream("p2", "model-b", Some(ModelTier::Standard));
    let (sorted1, _) = router.score_and_sort(vec![&ma, &mb], &pref, None);
    assert_eq!(
        sorted1[0].exposed_id(),
        "p1-model-a",
        "first request: fast model wins"
    );

    // Now model-a fails twice, model-b succeeds
    router.record_failure("p1-model-a", "p1");
    router.record_failure("p1-model-a", "p1");
    router.record_outcome("p2-model-b", "p2", 200.0, Some(80.0));

    // Request 2: model-b should now rank higher (healthier)
    let pref_health = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let ma = ModelEntry::for_upstream("p1", "model-a", Some(ModelTier::Standard));
    let mb = ModelEntry::for_upstream("p2", "model-b", Some(ModelTier::Standard));
    let (sorted2, _) = router.score_and_sort(vec![&ma, &mb], &pref_health, None);
    assert_eq!(
        sorted2[0].exposed_id(),
        "p2-model-b",
        "after failures: healthy model wins"
    );
}

#[test]
fn scoring_isolates_same_model_different_providers() {
    let router = Router::new();
    // Same model "shared-model" from two providers
    router.record_outcome("p1-shared-model", "p1", 100.0, Some(50.0));
    router.record_outcome("p2-shared-model", "p2", 3000.0, Some(800.0));

    let m1 = ModelEntry::for_upstream("p1", "shared-model", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p2", "shared-model", Some(ModelTier::Standard));
    let members = vec![&m1, &m2];
    let pref = PolicyPreference {
        latency_weight: 1.0,
        health_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let (sorted, _) = router.score_and_sort(members, &pref, None);
    assert_eq!(
        sorted[0].provider_id, "p1",
        "faster provider wins for same model"
    );
}

#[test]
fn streaming_scoring_prefers_ttft() {
    let router = Router::new();
    // Model A: fast TTFT (100ms), slow total (5000ms)
    router.record_outcome("p1-a", "p1", 5000.0, Some(100.0));
    // Model B: slow TTFT (800ms), fast total (1000ms)
    router.record_outcome("p2-b", "p2", 1000.0, Some(800.0));

    // For streaming (TTFT matters), A should win
    let task_streaming = crate::policy::TaskProfile {
        streaming: true,
        ..Default::default()
    };
    let pref = PolicyPreference {
        latency_weight: 1.0,
        health_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
    let (sorted, _) = router.score_and_sort(vec![&m1, &m2], &pref, Some(&task_streaming));
    assert_eq!(
        sorted[0].exposed_id(),
        "p1-a",
        "streaming prefers fast TTFT"
    );

    // For buffered (total matters), B should win
    let task_buffered = crate::policy::TaskProfile {
        streaming: false,
        ..Default::default()
    };
    let (sorted, _) = router.score_and_sort(vec![&m1, &m2], &pref, Some(&task_buffered));
    assert_eq!(
        sorted[0].exposed_id(),
        "p2-b",
        "buffered prefers fast total"
    );
}

#[test]
fn legacy_scoring_unchanged_without_observations() {
    // When no observations exist, scoring should behave identically to pre-4B
    let router = Router::new();
    let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
    let members = vec![&m1, &m2];
    let pref = PolicyPreference::default();
    let (sorted, breakdowns) = router.score_and_sort(members, &pref, None);
    // Both have same config -> scores should be equal
    assert!(
        (breakdowns[0].2 - breakdowns[1].2).abs() < 0.01,
        "identical models without observations should have equal scores"
    );
    // Order preserved (stable sort)
    assert_eq!(sorted[0].exposed_id(), "p1-a");
}
