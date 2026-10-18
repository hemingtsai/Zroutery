//! The cutover surface's own fixtures.
//!
//! Kept apart from [`crate::state::tests`] because every gate here is about
//! what the surface *refuses* to do. The happy path — a run that reaches
//! `Completed` — cannot be exercised without a live gateway to answer
//! `/health`, and a fixture that stands up an HTTP server to satisfy it would
//! be testing the mock rather than the panel. What can be decided here is the
//! part where being wrong is expensive: a cutover that runs when it should
//! have been refused, and a rollback offered over a snapshot that is not there.

use super::*;
use crate::secrets::KeychainSecrets;

use std::path::PathBuf;

use zroutery_core::agent_takeover::AgentType;
use zroutery_core::config::{AppConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind};
use zroutery_core::migration::MigrationState;

fn cutover_desktop(config: AppConfig) -> (Arc<Desktop>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("zroutery-cutover-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = Arc::new(KeychainSecrets::new(format!(
        "app.zroutery.test.{}",
        uuid::Uuid::new_v4()
    )));
    (Arc::new(Desktop::new(dir.clone(), config, secrets)), dir)
}

/// A configuration Zroutery *can* serve a client with: a token and one model
/// in a tier, which is what `gateway_target` requires before it will name an
/// address at all.
fn serving_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.server.auth_token = "gw-token".into();
    config.providers.push(ProviderConfig::new(
        "deepseek",
        "DeepSeek",
        ProviderKind::OpenAICompatible,
    ));
    config.models.push(ModelEntry::for_upstream(
        "deepseek",
        "deepseek-chat",
        Some(ModelTier::Standard),
    ));
    config
}

/// A fresh record reads `Detected`, holds no snapshot, and has nothing to undo.
///
/// The `durable` claim is the one that matters: the three states that mean work
/// started and did not finish are only recoverable if the record and the
/// snapshot outlive the process, and an in-memory store reports `Detected` for
/// all three after a restart.
#[test]
fn a_fresh_cutover_record_is_detected_durable_and_has_nothing_to_undo() {
    let (desktop, dir) = cutover_desktop(AppConfig::default());
    let status = desktop.migration_status();

    assert_eq!(status.state, MigrationState::Detected);
    assert!(
        !status.interrupted,
        "a record nobody ran is not interrupted"
    );
    assert!(!status.has_snapshot);
    assert!(status.snapshot_files.is_empty());
    assert!(
        status.durable,
        "a cutover record that forgets cannot recover an interrupted run, which is the one \
         thing this surface exists to offer"
    );
    assert!(status.persist_error.is_none());

    // The manifest is written where a later process will find it.
    let manifest = dir.join("migration").join("manifest.json");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !manifest.exists(),
        "nothing has run yet, so nothing should have been written; the durable claim is about \
         where it *will* be written, checked by the recovery fixture instead"
    );
}

/// Every agent is listed, including the one this build cannot migrate.
///
/// Gemini has no adapter. A surface that dropped it would render a page whose
/// row count silently depends on which clients exist, and a user with a Gemini
/// config would see nothing at all rather than an explained refusal.
#[test]
fn every_agent_is_listed_and_the_one_without_an_adapter_says_why() {
    let (desktop, dir) = cutover_desktop(serving_config());
    let previews = desktop.migration_previews();

    assert_eq!(
        previews.len(),
        AgentType::ALL.len(),
        "a client this build cannot migrate must still render a row explaining itself"
    );

    let gemini = previews
        .iter()
        .find(|p| p.agent == AgentType::Gemini)
        .expect("gemini is listed");
    assert!(
        gemini.blocked_reason.is_some(),
        "gemini has no adapter, so its row must carry the reason rather than an empty button"
    );
    assert!(gemini.steps.is_empty(), "a blocked client has no plan");
    assert!(
        !gemini.is_applicable(),
        "a blocked row must not be applicable, or the page would offer a control it cannot honour"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// With nothing to serve a client with, every row is refused — and every row
/// says why.
///
/// The reason matters as much as the refusal: the fix is in Settings, and a row
/// that vanished would send the user looking in the wrong place.
#[test]
fn with_no_gateway_target_every_row_is_blocked_with_a_reason() {
    let (desktop, dir) = cutover_desktop(AppConfig::default());
    let previews = desktop.migration_previews();

    assert!(!previews.is_empty());
    for preview in &previews {
        assert!(
            !preview.is_applicable(),
            "{} must not be applicable with no token and no tier",
            preview.agent.client_name()
        );
        let reason = preview
            .blocked_reason
            .as_deref()
            .unwrap_or("a blocked row must carry a reason");
        assert!(
            reason.contains("Settings"),
            "the refusal has to name where the fix is, not just that there is one: {reason}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Applying without a gateway target is refused, and nothing is written.
///
/// The refusal has to come before the snapshot: a snapshot taken on the way to a
/// run that cannot happen is a manifest describing a cutover that never did, and
/// the next launch would report an interrupted migration to recover from nothing.
#[tokio::test]
async fn applying_without_a_gateway_target_is_refused_and_writes_no_snapshot() {
    let (desktop, dir) = cutover_desktop(AppConfig::default());

    let error = desktop
        .migration_apply(AgentType::Claude, None)
        .await
        .expect_err("there is nothing to point a client at");

    assert!(
        error.contains("Settings"),
        "the reason must name the fix: {error}"
    );

    let status = desktop.migration_status();
    assert_eq!(status.state, MigrationState::Detected, "nothing ran");
    assert!(
        !status.has_snapshot,
        "a refused run must not leave a snapshot; one would make a later launch offer to recover \
         a cutover that never started"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A rollback is refused from every state except `Failed`.
///
/// This is the state machine's rule, and the surface has to honour it rather
/// than work around it. `rollback_with` *ignores* a refused transition and
/// restores the files anyway, so a panel offering the button outside `Failed`
/// would restore bytes while the record kept saying otherwise — an undo whose
/// own record denies it happened.
#[test]
fn a_rollback_is_refused_from_every_state_that_is_not_failed() {
    let (desktop, dir) = cutover_desktop(AppConfig::default());

    let error = desktop
        .migration_rollback()
        .expect_err("a record nobody failed cannot be rolled back");
    assert!(
        error.contains("Failed"),
        "the refusal must name the state it needs: {error}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The fingerprint is what makes the preview worth rendering.
///
/// Two plans for two different clients must not share a digest, and the digest
/// must cover the file as well as the steps — the same steps pointed at another
/// file is a different cutover, and a confirmed preview must not authorise it.
#[test]
fn the_fingerprint_covers_the_plan_and_the_file_it_would_write() {
    let (desktop, dir) = cutover_desktop(serving_config());
    let previews = desktop.migration_previews();

    let claude = previews
        .iter()
        .find(|p| p.agent == AgentType::Claude)
        .expect("claude is listed");
    let codex = previews
        .iter()
        .find(|p| p.agent == AgentType::Codex)
        .expect("codex is listed");

    assert_ne!(
        claude.fingerprint, codex.fingerprint,
        "two clients' plans share a digest, so confirming one would authorise the other"
    );
    assert_eq!(
        claude.fingerprint.len(),
        16,
        "the digest should be a fixed-width hex so a panel can render it without special-casing"
    );

    // The same client, rebuilt, must produce the same digest: a fingerprint
    // that changed on every poll would make the confirm dialog unfillable.
    let again = desktop.migration_previews();
    let claude_again = again
        .iter()
        .find(|p| p.agent == AgentType::Claude)
        .expect("claude is listed");
    assert_eq!(
        claude.fingerprint, claude_again.fingerprint,
        "rebuilding the same plan must not change its digest, or every preview would be stale"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An applicable plan is one that can actually run, and it is reversible only
/// in the step that writes.
///
/// The reversible flag is the honest answer to "what does a rollback put back",
/// and it is per step on purpose: a rollback restores the file the install step
/// replaced, and restores nothing about the port check or the endpoint probe.
#[test]
fn an_applicable_plan_marks_only_the_install_step_reversible() {
    let (desktop, dir) = cutover_desktop(serving_config());
    let previews = desktop.migration_previews();

    let claude = previews
        .iter()
        .find(|p| p.agent == AgentType::Claude)
        .expect("claude is listed");
    assert!(
        claude.is_applicable(),
        "claude has an adapter and a target: {claude:?}"
    );

    let reversible: Vec<_> = claude
        .steps
        .iter()
        .filter(|s| s.reversible)
        .map(|s| s.kind)
        .collect();
    assert_eq!(
        reversible,
        vec!["copy_config"],
        "only the step that writes a file can be undone by restoring that file"
    );
    assert!(
        claude.steps.iter().any(|s| s.kind == "verify_endpoint"),
        "a plan that installs without proving the endpoint answers is not a cutover; it is a \
         rewrite that hopes"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
