//! Trace schema v2: execution identity, task evidence, and backwards compatibility.
//!
//! The properties under test are the ones that make a trace usable as *evidence*
//! rather than as a log line. Each corresponds to a way the previous schema could
//! mislead a later reader:
//!
//! * An unobserved identity must stay unknown rather than being back-filled from the
//!   requested model, so a relay fronting several backends cannot be reported as one.
//! * A version-1 line must still load, so existing history survives the upgrade.
//! * A newer line must be refused rather than silently misread.
//! * Task evidence must stay attributable to the candidate that served it.

use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::execution_identity::{
    ExecutionIdentity, IdentityEvidence, UnavailableReason, EXECUTION_IDENTITY_SCHEMA_VERSION,
};
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
};
use zroutery_core::ml::shadow::ShadowInput;
use zroutery_core::ml::task_evidence::{
    EvidenceCoverage, EvidenceOrigin, TaskOutcomeEvidence, TaskStatus, VerifierKind,
};
use zroutery_core::ml::traces::{
    contained_append_with_evidence, DatasetFingerprint, RequestTrace, TraceError, TraceIngestion,
    TraceLog, TRACE_SCHEMA_MAX_READABLE, TRACE_SCHEMA_VERSION,
};
use zroutery_core::outcome::FinalStatus;
use zroutery_core::policy::TaskType;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sample(id: &str, provider: &str, model: &str) -> OutcomeTrainingSample {
    let mut values = [UNKNOWN; FEATURE_DIMENSION];
    values[0] = 0.5;
    OutcomeTrainingSample {
        sample_id: id.to_string(),
        schema_version: 1,
        timestamp: 1_700_000_000,
        streaming: false,
        dialect: "anthropic".to_string(),
        features: RoutingFeatures {
            schema_version: FEATURE_SCHEMA_VERSION,
            values,
        },
        targets: Targets {
            success: true,
            latency_ms: Some(120.0),
            ttft_ms: None,
            cost: Some(0.01),
            failure_class: None,
            fallback_count: 0,
        },
        provider_id: provider.to_string(),
        model_id: model.to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{id}"),
        request_id: format!("req-{id}"),
        decision_id: Some("dec-1".to_string()),
        response_id: None,
        final_status: FinalStatus::Success,
        success: true,
        identity: Default::default(),
        scope: SampleScope::Attempt {
            index: 0,
            attempt_id: format!("att-{id}"),
        },
        attempt_id: Some(format!("att-{id}")),
        rectified: false,
        attempts: Vec::new(),
        usage: None,
        estimated_cost: None,
        actual_cost: None,
        terminal_error: None,
        feedback: None,
    }
}

fn input() -> ShadowInput {
    ShadowInput {
        decision_id: "dec-1".to_string(),
        production_selected: "p-model".to_string(),
        ..ShadowInput::default()
    }
}

fn evidence(
    id: &str,
    attempt: Option<&str>,
    served: Option<&str>,
    status: TaskStatus,
) -> TaskOutcomeEvidence {
    TaskOutcomeEvidence::new(
        id,
        "req-1",
        "out-1",
        attempt.map(str::to_string),
        served.map(str::to_string),
        Some("p".to_string()),
        TaskType::Code,
        VerifierKind::Compilation,
        status,
        1_700_000_000,
        EvidenceOrigin::RuntimeVerifier,
    )
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[test]
fn a_new_trace_records_the_current_schema_version() {
    let trace = RequestTrace::new("req-1", 1, input(), vec![sample("s", "p", "m")]);
    assert_eq!(trace.schema_version, TRACE_SCHEMA_VERSION);
}

#[test]
fn the_execution_identity_carries_its_own_schema_version() {
    let identity = ExecutionIdentity::unrouted("m");
    assert_eq!(identity.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION);
}

#[test]
fn a_trace_round_trips_through_disk_with_identity_and_evidence_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = TraceLog::open(dir.path()).expect("open");

    let identity = ExecutionIdentity::unrouted("zr-model")
        .with_declared_model(Some("gpt-4o-2024-11-20"))
        .with_routed(Some("openrouter-gpt-4o"), Some("openrouter"));
    let trace = RequestTrace::with_evidence(
        "req-1",
        1_700_000_000,
        input(),
        vec![sample("s", "openrouter", "gpt-4o")],
        identity,
        vec![evidence(
            "ev-1",
            Some("att-s"),
            Some("gpt-4o"),
            TaskStatus::Succeeded,
        )],
    );
    log.append(&trace).expect("append");

    let loaded = log.load().expect("load");
    assert_eq!(loaded.len(), 1);
    let back = &loaded[0];
    assert_eq!(
        back.execution_identity.provider_declared_model.value(),
        Some("gpt-4o-2024-11-20")
    );
    assert_eq!(back.execution_identity.requested_model, "zr-model");
    assert_eq!(
        back.execution_identity.routed_candidate.as_deref(),
        Some("openrouter-gpt-4o")
    );
    assert_eq!(back.task_evidence.len(), 1);
    assert_eq!(back.task_evidence[0].status, TaskStatus::Succeeded);
}

// ---------------------------------------------------------------------------
// The anti-fabrication property
// ---------------------------------------------------------------------------

#[test]
fn a_provider_that_declares_nothing_is_recorded_as_unknown_not_as_the_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = TraceLog::open(dir.path()).expect("open");

    let identity = ExecutionIdentity::unrouted("claude-opus-5")
        .with_declared_model(None)
        .with_routed(Some("anthropic-claude-opus-5"), Some("anthropic"));
    let trace = RequestTrace::with_evidence(
        "req-1",
        1,
        input(),
        vec![sample("s", "anthropic", "claude-opus-5")],
        identity,
        Vec::new(),
    );
    log.append(&trace).expect("append");

    let loaded = log.load().expect("load");
    let declared = &loaded[0].execution_identity.provider_declared_model;
    // The crucial assertion: the requested name did NOT leak into the declared field.
    assert_eq!(declared.value(), None);
    assert_eq!(
        declared,
        &IdentityEvidence::Unavailable {
            reason: UnavailableReason::NotDeclaredByProvider
        }
    );
    assert_eq!(loaded[0].execution_identity.coverage(), 0.0);
}

#[test]
fn a_declared_backend_disagreeing_with_the_request_is_reported_not_corrected() {
    // A relay routing to a different backend is the finding this whole field exists
    // for. Normalising it away would hide the only evidence that it happened.
    let identity = ExecutionIdentity::unrouted("gpt-4o").with_declared_model(Some("gpt-4o-mini"));
    assert_eq!(identity.declared_matches_request(), Some(false));
    assert_eq!(
        identity.provider_declared_model.value(),
        Some("gpt-4o-mini")
    );
}

#[test]
fn a_default_trace_identity_claims_nothing_was_observed() {
    let trace = RequestTrace::new("req-1", 1, input(), vec![sample("s", "p", "m")]);
    assert_eq!(
        trace.execution_identity.provider_declared_model,
        IdentityEvidence::Unavailable {
            reason: UnavailableReason::UpstreamNotContacted
        }
    );
}

// ---------------------------------------------------------------------------
// Backwards and forwards compatibility
// ---------------------------------------------------------------------------

#[test]
fn a_version_one_line_still_loads_after_the_upgrade() {
    // Existing local history must survive. The new fields default, so an old line
    // reads with an honest absence rather than being refused or fabricated.
    let legacy = serde_json::json!({
        "schema_version": 1,
        "request_id": "req-legacy",
        "decision_id": "dec-1",
        "recorded_at": 1_700_000_000,
        "input": input(),
        "samples": [sample("s", "p", "m")],
    });
    let trace: RequestTrace = serde_json::from_value(legacy).expect("a v1 line must still load");
    assert_eq!(trace.request_id, "req-legacy");
    assert_eq!(trace.schema_version, 1);
    // Unknown, not invented.
    assert!(trace
        .execution_identity
        .provider_declared_model
        .is_unavailable());
    assert!(trace.task_evidence.is_empty());
}

#[test]
fn a_newer_schema_is_refused_rather_than_silently_misread() {
    let mut future = serde_json::json!({
        "schema_version": TRACE_SCHEMA_MAX_READABLE + 1,
        "request_id": "req-future",
        "decision_id": "dec-1",
        "recorded_at": 1,
        "input": input(),
        "samples": [],
        "field_this_build_has_never_heard_of": 42,
    });
    // Keep the unknown field so the line is not merely malformed.
    future["execution_identity"] = serde_json::json!({
        "schema_version": EXECUTION_IDENTITY_SCHEMA_VERSION,
        "requested_model": "m",
        "provider_declared_model": {"state": "Unavailable", "value": {"reason": "NotDeclaredByProvider"}},
        "observed_execution_fingerprint": {"state": "Unavailable", "value": {"reason": "NotDeclaredByProvider"}},
    });
    let line = serde_json::to_string(&future).expect("serialize");

    let dir = tempfile::tempdir().expect("tempdir");
    let log = TraceLog::open(dir.path()).expect("open");
    std::fs::write(log.path(), format!("{line}\n")).expect("write");

    match log.load() {
        Err(TraceError::UnsupportedSchema {
            found, supported, ..
        }) => {
            assert_eq!(found, TRACE_SCHEMA_MAX_READABLE + 1);
            assert_eq!(supported, TRACE_SCHEMA_MAX_READABLE);
        }
        other => panic!("expected an unsupported-schema refusal, got {other:?}"),
    }
}

#[test]
fn a_line_with_no_schema_version_is_reported_as_corrupt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = TraceLog::open(dir.path()).expect("open");
    std::fs::write(log.path(), "{\"request_id\":\"x\"}\n").expect("write");
    match log.load() {
        Err(TraceError::Corrupt { line, .. }) => assert_eq!(line, 1),
        other => panic!("expected a corrupt report, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Task evidence attribution
// ---------------------------------------------------------------------------

#[test]
fn evidence_is_attributed_to_the_attempt_that_actually_served() {
    // The fallback case: A failed, B served and passed the tests. Filing B's pass
    // against A teaches the model that A succeeds.
    let trace = RequestTrace::with_evidence(
        "req-1",
        1,
        input(),
        vec![sample("s", "p", "m")],
        ExecutionIdentity::unrouted("m"),
        vec![evidence(
            "ev-b",
            Some("att-b"),
            Some("model-b"),
            TaskStatus::Succeeded,
        )],
    );
    let attributable = trace.attributable_task_evidence();
    assert_eq!(attributable.len(), 1);
    assert_eq!(attributable[0].served_model.as_deref(), Some("model-b"));
    assert_eq!(attributable[0].attempt_id.as_deref(), Some("att-b"));
}

#[test]
fn request_level_evidence_is_not_credited_to_a_candidate() {
    let trace = RequestTrace::with_evidence(
        "req-1",
        1,
        input(),
        vec![sample("s", "p", "m")],
        ExecutionIdentity::unrouted("m"),
        // No attempt id: a verifier that judged the request as a whole.
        vec![evidence("ev-loose", None, None, TaskStatus::Succeeded)],
    );
    assert!(trace.attributable_task_evidence().is_empty());
    assert_eq!(trace.task_evidence.len(), 1);
}

#[test]
fn an_absent_task_result_is_not_a_failed_task() {
    let trace = RequestTrace::new("req-1", 1, input(), vec![sample("s", "p", "m")]);
    // No verifier ran. Reporting this as a failure would make every unverifiable
    // request look like evidence against the model that served it.
    let coverage = EvidenceCoverage::of(&trace.task_evidence);
    assert_eq!(coverage.total, 0);
    assert_eq!(coverage.observed_rate(), 0.0);
    assert_eq!(coverage.success_rate(), (0.0, 0));
}

// ---------------------------------------------------------------------------
// Containment
// ---------------------------------------------------------------------------

#[test]
fn a_trace_with_identity_evidence_and_no_samples_is_still_nothing() {
    // A record without feature vectors looks like counterfactual evidence and cannot
    // support a counterfactual claim, so it is not written.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = TraceLog::open(dir.path()).expect("open");
    let verdict = contained_append_with_evidence(
        &log,
        "dec-1",
        &input(),
        Vec::new(),
        1,
        ExecutionIdentity::unrouted("m").with_declared_model(Some("real")),
        vec![evidence(
            "ev",
            Some("att"),
            Some("m"),
            TaskStatus::Succeeded,
        )],
    );
    assert_eq!(verdict, TraceIngestion::Nothing);
    assert_eq!(log.counters().nothing, 1);
    assert_eq!(log.count().expect("count"), 0);
}

#[test]
fn a_write_failure_is_contained_and_leaves_the_request_unaffected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = TraceLog::open(dir.path()).expect("open");

    // Break the path *before* any append opens a handle on it. Replacing the file
    // with a directory makes the append open fail deterministically on every
    // platform. (Replacing it after an append would not: the cached handle stays
    // valid against the unlinked file, so the write would succeed into an inode
    // nobody can read back — a silent, undetectable evidence gap rather than a
    // reported failure.)
    std::fs::remove_file(log.path()).expect("remove");
    std::fs::create_dir(log.path()).expect("create dir in place");

    let refused = contained_append_with_evidence(
        &log,
        "dec-2",
        &input(),
        vec![sample("s2", "p", "m")],
        2,
        ExecutionIdentity::unrouted("m"),
        Vec::new(),
    );
    // Refused and counted, never propagated: the request's result is untouched.
    assert!(
        matches!(refused, TraceIngestion::Refused { .. }),
        "expected a refusal, got {refused:?}"
    );
    let counters = log.counters();
    assert_eq!(counters.appended, 0);
    assert!(counters.refused >= 1);
    assert!(counters.io_errors >= 1);
}

#[test]
fn the_fingerprint_is_unaffected_by_identity_and_evidence() {
    // The fingerprint identifies the *training body*. Identity and task evidence are
    // orthogonal, so adding them must not silently redefine what "the same data"
    // means for an already-trained model.
    let a = RequestTrace::new("r", 1, input(), vec![sample("s", "p", "m")]);
    let b = RequestTrace::with_evidence(
        "r",
        1,
        input(),
        vec![sample("s", "p", "m")],
        ExecutionIdentity::unrouted("m").with_declared_model(Some("different")),
        vec![evidence("ev", Some("att"), Some("m"), TaskStatus::Failed)],
    );
    assert_eq!(
        DatasetFingerprint::of(&a.samples),
        DatasetFingerprint::of(&b.samples)
    );
}
