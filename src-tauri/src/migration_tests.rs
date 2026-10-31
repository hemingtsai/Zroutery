//! The cutover plan the desktop builds, and the shape the core gate assumes.
//!
//! # Why this is a gate and not a convenience
//!
//! `crates/zroutery-core/tests/migration_completion_test.rs` drives a cutover
//! to `Completed` over a real file, against a real endpoint, and undoes it. It
//! builds the plan by hand, because a core test cannot reach the desktop's
//! builder.
//!
//! That leaves a seam: the core gate proves the *engine* completes a cutover
//! of that shape, and this proves the *product* builds a plan of that shape.
//! If either moved without the other, the proof would be of a plan nobody
//! runs — which is the defect the whole cutover surface was built to avoid,
//! and the one the records keep warning about: an engine that is implemented
//! and proven but integrated with nothing.
//!
//! So the steps, their order, the endpoint the plan asks, the marker it
//! requires and the one step a rollback can undo are all pinned here.

use zroutery_core::agent_takeover::AgentType;
use zroutery_core::migration::MigrationAction;

use crate::migration::{build_plan, HEALTH_BODY_MARKER};

/// The plan is the one the product runs: four steps, in this order, with the
/// endpoint check last.
#[test]
fn the_plan_is_the_one_the_core_gate_drives() {
    let dir = std::env::temp_dir().join(format!("zr-plan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");

    let (plan, config_path) = build_plan(&dir, AgentType::Claude, "http://127.0.0.1:8787", 8787)
        .expect("a plan for a client this build can migrate");

    let kinds: Vec<&str> = plan.steps.iter().map(|s| kind_of(&s.action)).collect();
    assert_eq!(
        kinds,
        vec![
            "stop_external",
            "start_zroutery",
            "copy_config",
            "verify_endpoint"
        ],
        "the plan's steps, in order, are what the core gate drives"
    );

    // The endpoint check is the one that decides, and it asks for the mark the
    // real gateway's `/health` answers.
    let verify = plan
        .steps
        .iter()
        .find_map(|s| match &s.action {
            MigrationAction::VerifyEndpoint { url, expect } => Some((url, expect)),
            _ => None,
        })
        .expect("the plan asks the endpoint");
    assert!(
        verify.0.ends_with("/health"),
        "the plan asks {} rather than the gateway's health endpoint",
        verify.0
    );
    match verify.1 {
        zroutery_core::migration::EndpointExpect::BodyContains(marker) => {
            // The literal, not the constant the plan is built from. Comparing
            // the plan against the constant it was built from would be circular:
            // it would pass with any mark at all, and the mark is the whole
            // point of the check. This is what the gateway's `/health` answers,
            // written out.
            assert_eq!(
                marker, "\"status\":\"ok\"",
                "the plan requires a mark the gateway does not answer"
            );
            assert_eq!(
                HEALTH_BODY_MARKER, "\"status\":\"ok\"",
                "the marker the plan is built from is not the one the gateway answers"
            );
        }
        other => panic!("the plan requires {other:?} rather than a body marker"),
    }

    // The copy is the step a rollback can undo, and it is the only one.
    let reversible: Vec<&str> = plan
        .steps
        .iter()
        .filter(|s| s.reversible)
        .map(|s| kind_of(&s.action))
        .collect();
    assert_eq!(
        reversible,
        vec!["copy_config"],
        "the step a rollback can undo is the one that replaced the file"
    );

    // The copy installs the staged document over the client's own file, and the
    // staging file lives under the configuration directory rather than in the
    // client's own directory.
    let copy = plan
        .steps
        .iter()
        .find_map(|s| match &s.action {
            MigrationAction::CopyConfig { source, dest } => Some((source, dest)),
            _ => None,
        })
        .expect("the plan installs a document");
    assert_eq!(
        copy.1,
        &config_path.to_string_lossy(),
        "the plan installs over a file other than the client's own"
    );
    assert!(
        copy.0.starts_with(&dir.to_string_lossy().into_owned()),
        "the staged document was left outside the configuration directory: {}",
        copy.0
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A client this build cannot migrate has no plan, and says why.
#[test]
fn composite_gemini_setup_has_no_single_file_cutover_plan() {
    let dir = std::env::temp_dir().join(format!("zr-plan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");

    let error = build_plan(&dir, AgentType::Gemini, "http://127.0.0.1:8787", 8787)
        .expect_err("two-file setup cannot use single-file cutover");
    assert!(
        error.contains("Gemini") && error.contains("two configuration files"),
        "the refusal names the client it is about: {error}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The step kind, by the same naming the panel shows.
fn kind_of(action: &MigrationAction) -> &'static str {
    match action {
        MigrationAction::CopyConfig { .. } => "copy_config",
        MigrationAction::ValidateConfig { .. } => "validate_config",
        MigrationAction::StopExternal { .. } => "stop_external",
        MigrationAction::StartOwned { .. } => "start_owned",
        MigrationAction::StopOwned { .. } => "stop_owned",
        MigrationAction::StartZroutery { .. } => "start_zroutery",
        MigrationAction::VerifyEndpoint { .. } => "verify_endpoint",
        MigrationAction::Custom { .. } => "custom",
    }
}
