//! Focused CORE-P1-ELIGIBILITY-TRACE contract tests.

use std::sync::Arc;

use zroutery_core::config::{
    AppConfig, ModelCapabilities, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::error::Error;
use zroutery_core::failure::{ClassifiedFailure, FailureClass};
use zroutery_core::ir::Capability;
use zroutery_core::policy::{
    canonical_capabilities, hash_to_u64, PolicyFallback, PolicyPreference, PolicyRequirements,
    RejectionReason,
};
use zroutery_core::registry::{Registry, Resolution};
use zroutery_core::router::{FailureDisposition, Router};

fn config(models: Vec<ModelEntry>) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig::new(
        "provider-a",
        "Provider A",
        ProviderKind::OpenAICompatible,
    ));
    // These unit-level eligibility cases isolate the hard request gate. The
    // production vision tests exercise the separately documented remediation.
    cfg.routing.rectifier.enabled = false;
    cfg.models = models;
    cfg
}

fn registry(cfg: AppConfig) -> Registry {
    Registry::new(Arc::new(cfg))
}

fn model(name: &str, tier: ModelTier) -> ModelEntry {
    ModelEntry::for_upstream("provider-a", name, Some(tier))
}

fn with_vision(mut entry: ModelEntry) -> ModelEntry {
    entry.capabilities.vision = true;
    entry
}

fn no_candidate(result: Result<(), Error>) {
    assert!(matches!(result, Err(Error::NoCandidate(_))));
}

#[test]
fn direct_and_tier_resolution_have_request_capability_parity() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let vision = with_vision(model("vision", ModelTier::Standard));
    let mut cfg = config(vec![text.clone(), vision.clone()]);
    // Neither legacy switch may weaken a request-derived requirement.
    cfg.routing.capability_filter = false;
    cfg.routing.strict_capability_filter = false;
    let reg = registry(cfg);
    let router = Router::new();

    no_candidate(
        router
            .plan(
                &reg,
                &Resolution::Direct("provider-a-text".into()),
                &[Capability::Vision],
            )
            .map(|_| ()),
    );
    let direct = router
        .plan(
            &reg,
            &Resolution::Direct("provider-a-vision".into()),
            &[Capability::Vision],
        )
        .unwrap();
    assert_eq!(direct[0].model_id(), "provider-a-vision");

    let tier = router
        .plan(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap();
    assert_eq!(tier.len(), 1);
    assert_eq!(tier[0].model_id(), "provider-a-vision");
}

#[test]
fn policy_path_keeps_request_capabilities_hard_even_when_policy_is_empty() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let reg = registry(config(vec![text]));
    let router = Router::new();

    let err = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn policy_only_unknown_is_explicitly_degraded_but_request_unknown_is_rejected() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let mut remediated_cfg = config(vec![text.clone()]);
    remediated_cfg.vision.enabled = true;
    let remediated = registry(remediated_cfg);
    let router = Router::new();

    let policy_requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: false,
        ..Default::default()
    };
    let (soft, soft_decision) = router
        .plan_with_policy(
            &remediated,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &policy_requirements,
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap();
    assert_eq!(soft.len(), 1);
    assert!(
        soft[0].degraded,
        "documented remediation must be observable"
    );
    assert!(!soft_decision.candidates[0].eligible);
    assert!(soft_decision.candidates[0]
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("unknown_capability:vision")));

    // Without a documented remediation, the same unknown requirement is
    // rejected rather than passed through a policy fallback.
    let strict = registry(config(vec![text]));
    let err = router
        .plan_with_policy(
            &strict,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
            &policy_requirements,
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn fallback_rechecks_request_capabilities_and_keeps_rejection_trace() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let vision = with_vision(model("vision", ModelTier::Reasoning));
    let reg = registry(config(vec![text, vision]));
    let router = Router::new();

    let (plan, decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::Escalate {
                enabled: true,
                max_steps: 1,
            },
            None,
        )
        .unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].model_id(), "provider-a-vision");
    let rejected = decision
        .candidates
        .iter()
        .find(|candidate| candidate.model_id == "provider-a-text")
        .unwrap();
    assert!(!rejected.eligible);
    assert!(rejected
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("unknown_capability:vision")));
    assert!(!decision
        .candidates
        .iter()
        .any(|candidate| candidate.model_id == "provider-a-text" && candidate.eligible));
}

#[test]
fn ignore_requirements_cannot_reinsert_an_incapable_request_candidate() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let reg = registry(config(vec![text]));
    let router = Router::new();

    let err = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::IgnoreRequirements,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoCandidate(_)));
}

#[test]
fn ignore_requirements_bypasses_non_capability_constraints_but_keeps_capability_gates() {
    // A Standard-tier model with tools only, so vision is `Unknown` rather than
    // supported, plus a vision-capable peer for the positive control.
    let text = model("text", ModelTier::Standard);
    let vision = with_vision(model("vision", ModelTier::Standard));
    let standard = Resolution::Tier(ModelTier::Standard);
    let router = Router::new();

    // Non-capability policy constraints: a provider allow-list that excludes the
    // whole pool, and a tier bound above the whole pool.
    let non_capability = PolicyRequirements {
        allowed_providers: vec!["provider-b".into()],
        min_tier: Some(ModelTier::Frontier),
        ..Default::default()
    };

    // The constraints are real: a strict fallback rejects the entire pool.
    no_candidate(
        router
            .plan_with_policy(
                &registry(config(vec![text.clone()])),
                &standard,
                &[],
                &non_capability,
                &PolicyPreference::default(),
                &PolicyFallback::Reject,
                None,
            )
            .map(|_| ()),
    );

    // `IgnoreRequirements` bypasses exactly those non-capability constraints.
    let text_only = registry(config(vec![text]));
    let (plan, decision) = router
        .plan_with_policy(
            &text_only,
            &standard,
            &[],
            &non_capability,
            &PolicyPreference::default(),
            &PolicyFallback::IgnoreRequirements,
            None,
        )
        .expect("non-capability policy constraints are bypassed");
    assert_eq!(plan[0].model_id(), "provider-a-text");
    let candidate = &decision.candidates[0];
    assert!(
        candidate.eligible && candidate.rejection.is_none(),
        "the bypassed candidate must be planned without residual rejection evidence"
    );

    // The bypass does not rescue a request-derived capability. The same
    // non-capability constraints are still relaxed here, so `NoCandidate` can
    // only come from the remaining hard capability gate.
    no_candidate(
        router
            .plan_with_policy(
                &text_only,
                &standard,
                &[Capability::Vision],
                &non_capability,
                &PolicyPreference::default(),
                &PolicyFallback::IgnoreRequirements,
                None,
            )
            .map(|_| ()),
    );

    // Policy `required_capabilities` is equally preserved: an unsatisfied
    // policy capability still rejects, a satisfied one still passes.
    let policy_capability = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        ..Default::default()
    };
    no_candidate(
        router
            .plan_with_policy(
                &text_only,
                &standard,
                &[],
                &policy_capability,
                &PolicyPreference::default(),
                &PolicyFallback::IgnoreRequirements,
                None,
            )
            .map(|_| ()),
    );
    let (plan, decision) = router
        .plan_with_policy(
            &registry(config(vec![vision])),
            &standard,
            &[],
            &policy_capability,
            &PolicyPreference::default(),
            &PolicyFallback::IgnoreRequirements,
            None,
        )
        .expect("a satisfied policy capability requirement is preserved, not bypassed");
    assert_eq!(plan[0].model_id(), "provider-a-vision");
    assert!(decision.candidates[0].eligible);
}

#[test]
fn planned_identity_is_explicit_and_not_a_served_identity() {
    let vision = with_vision(model("vision", ModelTier::Standard));
    let reg = registry(config(vec![vision]));
    let router = Router::new();

    let (plan, decision) = router
        .plan_with_trace(
            &reg,
            &Resolution::Direct("provider-a-vision".into()),
            &[Capability::Vision],
        )
        .unwrap();
    let planned = decision.planned_identity().expect("planned identity");
    assert_eq!(planned.model_id, plan[0].model_id());
    assert_eq!(planned.provider_id, "provider-a");
    assert_eq!(planned.tier.as_deref(), Some("standard"));
    assert_eq!(decision.planned_selected(), Some("provider-a-vision"));
    assert_eq!(decision.selected.as_deref(), Some("provider-a-vision"));
}

#[test]
fn policy_revision_preserves_empty_hash_and_tracks_canonical_nonempty_capabilities() {
    let mut capable = model("capable", ModelTier::Standard);
    capable.capabilities.vision = true;
    let reg = registry(config(vec![capable]));
    let router = Router::new();
    let requirements = PolicyRequirements::default();

    let empty = router
        .plan_with_trace(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
        )
        .unwrap()
        .1;
    assert_eq!(
        empty.policy_revision.requirements_hash,
        hash_to_u64(&requirements),
        "empty request capabilities must retain the legacy policy revision hash"
    );

    let vision = router
        .plan_with_trace(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap()
        .1;
    let duplicate_vision = router
        .plan_with_trace(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision, Capability::Vision],
        )
        .unwrap()
        .1;
    let tools = router
        .plan_with_trace(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Tools],
        )
        .unwrap()
        .1;

    assert_eq!(
        vision.policy_revision.requirements_hash,
        duplicate_vision.policy_revision.requirements_hash,
        "capability hashing must be canonical and deduplicated"
    );
    assert_ne!(
        vision.policy_revision.requirements_hash,
        tools.policy_revision.requirements_hash,
        "different request capability vectors must have different revisions"
    );
    assert_ne!(
        vision.policy_revision.requirements_hash,
        hash_to_u64(&requirements),
        "nonempty request capabilities must be included in the revision"
    );
}

#[test]
fn failure_disposition_is_a_priority_summary_not_a_second_classifier() {
    let router = Router::new();
    let both = ClassifiedFailure::from_core_error(Error::Upstream {
        provider: "provider-a".into(),
        status: 429,
        body: "rate limited".into(),
    });
    assert_eq!(router.failure_disposition(&both), FailureDisposition::Retry);
    assert!(router.should_retry_failure(&both));
    assert!(router.should_fallback_failure(&both));

    let unknown = ClassifiedFailure::from_core_error(Error::Upstream {
        provider: "provider-a".into(),
        status: 500,
        body: "unclassified".into(),
    });
    assert_eq!(unknown.class, FailureClass::Unknown);
    assert_eq!(
        router.failure_disposition(&unknown),
        FailureDisposition::Retry
    );
    assert!(router.should_retry_failure(&unknown));
    assert!(router.should_fallback_failure(&unknown));

    let timeout = ClassifiedFailure::from_core_error(Error::Timeout(5));
    assert_eq!(
        router.failure_disposition(&timeout),
        FailureDisposition::Retry
    );
    assert!(router.should_retry_failure(&timeout));
    assert!(router.should_fallback_failure(&timeout));
}

#[test]
fn eligibility_trace_reasons_are_deterministic_and_bounded() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let vision = with_vision(model("vision", ModelTier::Standard));
    let reg = registry(config(vec![text, vision]));
    let router = Router::new();

    let make = || {
        router
            .plan_with_trace(
                &reg,
                &Resolution::Tier(ModelTier::Standard),
                &[Capability::Vision, Capability::Vision],
            )
            .unwrap()
            .1
    };
    let first = make();
    let second = make();
    let evidence = |decision: &zroutery_core::policy::RouteDecision| {
        decision
            .candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.model_id.clone(),
                    candidate.eligible,
                    candidate.rejection.clone(),
                    candidate.trace_reason(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(evidence(&first), evidence(&second));
    assert!(first
        .candidates
        .iter()
        .all(|candidate| candidate.trace_reason().len() <= 128));
    assert_eq!(
        canonical_capabilities(&[Capability::Tools, Capability::Vision, Capability::Tools]),
        vec![Capability::Vision, Capability::Tools]
    );
}

#[test]
fn classified_failure_adapter_preserves_fallback_and_health_authority() {
    let reg = registry(config(vec![model("m", ModelTier::Standard)]));
    let router = Router::new();
    let routing = reg.config().routing.clone();

    let missing_key = ClassifiedFailure::from_core_error(Error::MissingApiKey("provider-a".into()));
    assert_eq!(
        router.failure_disposition(&missing_key),
        FailureDisposition::Fallback
    );
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_classified_failure("provider-a-m", &missing_key, &routing);
    }
    assert!(router.health_snapshot().is_empty());

    let rate_limit = ClassifiedFailure::from_core_error(Error::Upstream {
        provider: "provider-a".into(),
        status: 429,
        body: "secret upstream detail".into(),
    });
    assert_eq!(
        router.failure_disposition(&rate_limit),
        FailureDisposition::Retry
    );
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_classified_failure("provider-a-m", &rate_limit, &routing);
    }
    assert!(!router.is_cooling("provider-a-m"));
    let health = router.health_snapshot();
    assert_eq!(
        health[0].total_failure,
        routing.circuit_breaker.failure_threshold as u64
    );
    assert!(health[0]
        .last_error
        .as_deref()
        .is_some_and(|message| !message.contains("secret upstream detail")));

    let invalid = ClassifiedFailure::from_core_error(Error::invalid("bad request"));
    assert_eq!(invalid.class, FailureClass::InvalidRequest);
    assert_eq!(
        router.failure_disposition(&invalid),
        FailureDisposition::Stop
    );
}

#[test]
fn unknown_model_and_empty_tier_keep_explicit_no_candidate_errors() {
    let reg = registry(config(vec![]));
    let router = Router::new();
    let unknown = router
        .plan(
            &reg,
            &Resolution::Direct("provider-a-does-not-exist".into()),
            &[],
        )
        .unwrap_err();
    assert!(matches!(unknown, Error::UnknownModel(_)));
    let empty = router
        .plan(&reg, &Resolution::Tier(ModelTier::Standard), &[])
        .unwrap_err();
    assert!(matches!(empty, Error::NoCandidate(_)));
}

#[test]
fn missing_key_is_fallbackable_but_not_a_health_failure() {
    let reg = registry(config(vec![model("m", ModelTier::Standard)]));
    let router = Router::new();
    let routing = reg.config().routing.clone();
    let failure = ClassifiedFailure::from_core_error(Error::MissingApiKey("provider-a".into()));

    assert!(router.should_fallback_failure(&failure));
    assert!(!router.should_retry_failure(&failure));
    router.report_failure(
        "provider-a-m",
        &Error::MissingApiKey("provider-a".into()),
        &routing,
    );
    assert!(router.health_snapshot().is_empty());
}

#[test]
fn documented_vision_remediation_is_degraded_and_not_a_capability_pass() {
    let mut text = model("text", ModelTier::Standard);
    text.capabilities = ModelCapabilities::default();
    let mut cfg = config(vec![text]);
    cfg.vision.enabled = true;
    let reg = registry(cfg);
    let router = Router::new();

    let (plan, decision) = router
        .plan_with_trace(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[Capability::Vision],
        )
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert!(plan[0].degraded);
    assert!(!decision.candidates[0].eligible);
    assert!(decision.candidates[0]
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("unknown_capability:vision")));
}

#[test]
fn canonical_rejection_reason_distinguishes_unknown_capability() {
    let requirements = PolicyRequirements::default();
    let check = requirements.check_with_request_capabilities(
        "m",
        "p",
        Some(ModelTier::Standard),
        &ModelCapabilities::default(),
        false,
        &[Capability::Vision],
    );
    assert!(!check.eligible);
    assert!(check.reasons.iter().any(|reason| matches!(
        reason,
        RejectionReason::UnknownCapability(Capability::Vision)
    )));
}
