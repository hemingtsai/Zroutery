//! Is a promoted model reachable from the product at all?
//!
//! # The question
//!
//! Every mechanism on the path from a served request to a re-ordered plan exists and
//! is exercised: outcomes are recorded, the trace log is durable, `run_training`
//! fits a model with a frozen holdout, `run_comparison` replays it against four
//! baselines, `PromotionGate::evaluate` judges it on nine named criteria,
//! `ActiveModelStore::promote` installs it, and `AppState::new` attaches whatever
//! the durable pointer names on the next start. The closed loop is real.
//!
//! So: **is any of it reachable by running the product?**
//!
//! # The answer, as of this commit: no
//!
//! Every call to `ActiveModelStore::promote`, every `PromotionGate::new`, every
//! `run_comparison` and every `run_training` in the repository sits inside a
//! `#[cfg(test)]` module or a `tests/` binary. `src-tauri` names none of them. The
//! HTTP surface has `/v1/ml/status`, `/v1/ml/shadow` and `/v1/ml/rollback`, and
//! nothing that promotes.
//!
//! Which means, in a shipped build:
//!
//! * the durable pointer is never written, so `MlStatus::active` is always `None`;
//! * `MlRouter::is_attached()` is always `false`;
//! * `apply_ml_ranking` always takes its no-model branch, so the model-ranking path
//!   is unreachable and only blind exploration can move a plan;
//! * `ml_shadow_analysis` has no attached model to replay, and `rollback` has no
//!   previous pointer to return to;
//! * `ml_routing.enabled` is inert.
//!
//! The sink is wired and the source is missing. `AppState::new` attaches a verified
//! predictor if a pointer exists — that half is correct and tested — but nothing in
//! the product ever writes one.
//!
//! # What this test is for
//!
//! It is a characterisation, not a wish. Both halves assert what the product
//! actually does today, so the gap is stated as a fact rather than remembered as an
//! impression — and so that **wiring promotion fails this test**, forcing whoever
//! does it to update the claim instead of leaving a test that quietly stopped
//! describing the product.
//!
//! The source-level half follows the idiom already used in `journal_test.rs`
//! (`nothing_in_the_running_product_can_reach_this_journal`): a repository property
//! asserted as a property, by reading the running modules' own source.

use std::sync::Arc;

use zroutery_core::config::{AppConfig, MemorySecretStore};
use zroutery_core::ml::{run_promotion_round, ActiveModelStore, RoundConfig, RoundError};
use zroutery_core::server::AppState;

/// **A running module can start a promotion round.**
///
/// This is the tripwire from §E9 turned around. It used to assert that *nothing* in
/// a running build named the promotion sink, and it fired the moment
/// `AppState::ml_run_promotion_round` appeared — which was the point. The loop was
/// closed in code and open in the product for exactly as long as that held.
///
/// It now asserts the narrower property still worth holding, and easy to lose by
/// accident: the round is reachable **from the request path**, so it runs behind the
/// same auth layer as `status`, `shadow` and `rollback`. Reachability *without*
/// those two properties would be a promotion anyone who can reach the port can
/// trigger, which is why this asserts the call lives in `server/mod.rs` and not
/// merely somewhere in the crate.
#[test]
fn a_promotion_round_is_reachable_from_the_request_path() {
    let source = include_str!("../src/server/mod.rs");
    let outside_tests = match source.find("#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    };
    let caller = outside_tests.lines().find(|line| {
        let code = line.trim();
        if code.starts_with("//") || code.starts_with('*') {
            return false;
        }
        code.contains("run_promotion_round(") && !code.contains("fn ")
    });
    assert!(
        caller.is_some(),
        "nothing in server/mod.rs calls run_promotion_round any more, so the loop is \
         open in the product again. That is the regression §E9 recorded and this file \
         exists to catch: either restore the call, or record deliberately that \
         promotion is no longer reachable."
    );
}

// ---------------------------------------------------------------------------
// The round mechanism itself
// ---------------------------------------------------------------------------

/// **A round refuses to invent a verdict from nothing.**
///
/// The round is now production-resident, so its own failure modes need pinning
/// rather than inheriting whatever a harness happened to produce. An empty log must
/// be its own answer, distinct from a gate refusal: "no traffic yet" and "traffic
/// arrived and the model was not good enough" call for different responses from
/// whoever schedules rounds, and conflating them is how a scheduler retries forever.
#[test]
fn a_round_over_an_empty_log_reports_nothing_to_learn() {
    let dir = tempfile::tempdir().expect("tempdir");

    match run_promotion_round(dir.path(), &RoundConfig::default(), None) {
        Err(RoundError::NothingToLearn) => {}
        Err(other) => panic!("an empty state directory should report NothingToLearn, got {other:?}"),
        Ok(round) => panic!(
            "a round over an empty log produced a verdict ({}), which means it decided \
             a promotion out of no evidence",
            round.decision.verdict.as_str()
        ),
    }
}

/// **A round that cannot run leaves the store exactly as it found it.**
///
/// The separation between judging and installing is why a round is useful on its
/// own: it is how you find out *why* a model was refused without mutating the store.
///
/// **This is the weaker half of that claim and it is worth being precise about
/// which half it is.** An empty log means the round returns before it has a verdict,
/// so this pins the error path — no pointer, no audit entry, no partial write — and
/// not the property one actually cares about, which is that a *successful* round does
/// not install either.
///
/// That property is caught by
/// `ml_closed_loop_test::new_outcomes_from_a_new_process_reach_the_next_round_of_learning`,
/// which drives two rounds over real traffic and asserts on what is installed
/// between them. Verified by mutation: giving `run_promotion_round` an install side
/// effect fails that test and leaves this one green. Pinning it properly here would
/// need a body with traffic in it, which is that test's fixture and not worth
/// duplicating.
#[test]
fn running_a_round_that_cannot_run_touches_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = run_promotion_round(dir.path(), &RoundConfig::default(), None);

    let store = ActiveModelStore::open(dir.path()).expect("store");
    assert!(
        store.active().expect("read pointer").is_none(),
        "a round installed a model on its own; run and install are supposed to be \
         separate calls"
    );
    assert!(
        store.audit().expect("audit").is_empty(),
        "a round wrote a promotion audit entry on its own"
    );
}

/// **So a fresh product never has one, however it is configured.**
///
/// `ml_routing.enabled = true` with a state directory is the most permissive thing
/// an operator can set, and it still yields no attached model, because nothing has
/// written the pointer it would attach from.
#[test]
fn a_configured_product_has_no_attached_model_because_none_can_be_produced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = AppConfig::default();
    config.server.host = "127.0.0.1".into();
    config.server.port = 0;
    config.server.auth_token = "tripwire".into();
    config.ml_routing.enabled = true;
    config.ml_routing.state_dir = dir.path().display().to_string();

    let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));

    assert!(
        !state.ml_routing().is_attached(),
        "a model is attached, so something is now producing promotions"
    );
    let status = state.ml_status();
    assert!(
        status.active.is_none(),
        "MlStatus::active should be None on a product that cannot promote; got {:?}",
        status.active.map(|active| active.commit_id)
    );
    assert!(
        status.durable_state,
        "the state directory is configured, so what is missing is not configuration"
    );
    assert_eq!(
        status.headline(),
        "ml_routing.enabled is set but no model is attached; routing is deterministic",
        "and the headline says so, which is the one thing an operator gets right"
    );
}
