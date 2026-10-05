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
use zroutery_core::server::AppState;

/// The modules that run in a shipped build. None of them may promote a model.
const RUNNING_MODULES: [(&str, &str); 6] = [
    ("server/pipeline.rs", include_str!("../src/server/pipeline.rs")),
    ("server/mod.rs", include_str!("../src/server/mod.rs")),
    ("ml/serving.rs", include_str!("../src/ml/serving.rs")),
    ("ml/promotion.rs", include_str!("../src/ml/promotion.rs")),
    ("ml/comparison.rs", include_str!("../src/ml/comparison.rs")),
    ("ml/learning.rs", include_str!("../src/ml/learning.rs")),
];

/// **No running module promotes a model.**
///
/// This is the half that will fail when the gap closes, which is the point. The
/// message says what to do then rather than leaving a silent contradiction.
#[test]
fn no_running_module_promotes_a_model() {
    for (path, source) in RUNNING_MODULES {
        // Only the part outside `#[cfg(test)]` counts, and a *definition* is not a
        // call: `pub fn run_comparison(` sits above the test module in the very file
        // that defines it, so a naive substring search reports the definition as a
        // caller. Definition lines are therefore skipped explicitly.
        let outside_tests = match source.find("#[cfg(test)]") {
            Some(at) => &source[..at],
            None => source,
        };
        for token in [".promote(", "PromotionGate::new", "run_comparison(", "run_training("] {
            let name = token.trim_start_matches('.').trim_end_matches('(');
            let offender = outside_tests.lines().find(|line| {
                line.contains(token) && !line.contains(&format!("fn {name}("))
            });
            assert!(
                offender.is_none(),
                "{path} calls {token} outside its test module, so a running module can \
                 now produce a promotion. That closes the gap this test records — \
                 update it, and update the capability matrix's product-wiring row with \
                 what is now reachable.\n  found: {}",
                offender.unwrap_or_default().trim()
            );
        }
    }
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