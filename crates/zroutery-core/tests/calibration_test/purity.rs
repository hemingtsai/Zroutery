use zroutery_core::ml::calibration::{run_calibration, DISTRIBUTION_ROLE};

use super::support::*;

// ---------------------------------------------------------------------------
// GATE 6 — production inaccessibility
// ---------------------------------------------------------------------------

/// GATE 6: this module reaches no installation, scheduling, or routing surface.
///
/// The tokens are code-shaped rather than prose-shaped, so a documentation
/// sentence about what the module does not do cannot satisfy or trip the wire.
#[test]
fn the_calibration_source_reaches_no_installation_or_background_surface() {
    // `calibration.rs` became `calibration/`, so the fence reads every file of
    // the module, the way the router and policy fences do.
    let source = CALIBRATION_SOURCES
        .iter()
        .map(|(_name, src)| *src)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    for forbidden in [
        // Live-state mutation and the stores that carry it.
        "rwlock",
        "crate::sync::write",
        "crate::sync::read",
        "modelstore",
        "datasetstore",
        "try_commit",
        // The only type that carries an installation call.
        "shadowengine",
        "shadow::shadowstore",
        "swap",
        "try_train_and_swap",
        "from_trained_parts",
        // Server wiring. Path-shaped tokens, so a doc sentence that *describes*
        // what the module does not do can neither satisfy nor trip the wire.
        "appstate",
        "build_app",
        "serverhandle",
        "server::",
        "axum",
        "reqwest",
        "router::",
        "policy::",
        "coordinator::",
        "shadow::",
        "model::shadowengine",
        // Background work, scheduling, and timers.
        "std::thread",
        "thread::spawn",
        "tokio::spawn",
        "spawn(",
        "sleep(",
        "interval(",
        "tokio::time",
        "tokio::task",
        // Durable state and the network.
        "filesystem",
        "std::fs",
        "reqwest",
        "listen",
    ] {
        assert!(
            !source.contains(forbidden),
            "calibration.rs must not reference {forbidden}"
        );
    }
}

/// Every file of the router, spelled out.
///
/// `router.rs` became `router/` and this tripwire has to follow it. A child that
/// reached the module could not be seen if only the barrel were scanned, and a
/// tripwire that cannot see a moved file is indistinguishable from one that does
/// not exist. Adding a file here is the cost of keeping it honest.
const ROUTER_SOURCES: &[(&str, &str)] = &[
    ("router/mod.rs", include_str!("../../src/router/mod.rs")),
    (
        "router/candidate.rs",
        include_str!("../../src/router/candidate.rs"),
    ),
    (
        "router/candidates.rs",
        include_str!("../../src/router/candidates.rs"),
    ),
    (
        "router/classifier.rs",
        include_str!("../../src/router/classifier.rs"),
    ),
    (
        "router/health.rs",
        include_str!("../../src/router/health.rs"),
    ),
    (
        "router/helpers.rs",
        include_str!("../../src/router/helpers.rs"),
    ),
    ("router/plan.rs", include_str!("../../src/router/plan.rs")),
    (
        "router/reporting.rs",
        include_str!("../../src/router/reporting.rs"),
    ),
    (
        "router/selection.rs",
        include_str!("../../src/router/selection.rs"),
    ),
    ("router/tiers.rs", include_str!("../../src/router/tiers.rs")),
    ("router/tests.rs", include_str!("../../src/router/tests.rs")),
];

/// Every file of the routing policy, spelled out.
///
/// `policy.rs` became `policy/` and this tripwire has to follow it. A child
/// that reached `ml::calibration` would be invisible if only the barrel were
/// scanned, and a tripwire that cannot see a moved file is indistinguishable
/// from one that does not exist. Adding a file here is the cost of keeping it
/// honest.
const POLICY_SOURCES: &[(&str, &str)] = &[
    ("policy/mod.rs", include_str!("../../src/policy/mod.rs")),
    (
        "policy/capabilities.rs",
        include_str!("../../src/policy/capabilities.rs"),
    ),
    (
        "policy/clients.rs",
        include_str!("../../src/policy/clients.rs"),
    ),
    (
        "policy/decision.rs",
        include_str!("../../src/policy/decision.rs"),
    ),
    (
        "policy/eligibility.rs",
        include_str!("../../src/policy/eligibility.rs"),
    ),
    (
        "policy/policy.rs",
        include_str!("../../src/policy/policy.rs"),
    ),
    (
        "policy/profile.rs",
        include_str!("../../src/policy/profile.rs"),
    ),
    (
        "policy/scoring.rs",
        include_str!("../../src/policy/scoring.rs"),
    ),
    ("policy/tests.rs", include_str!("../../src/policy/tests.rs")),
];

/// GATE 6, behavioural: nothing outside this module names it, so the running
/// product cannot reach the distribution.
///
/// The distribution is a diagnostic artifact. If the product cannot name the
/// module, it cannot consume the vector — which is the claim, stated as a
/// property of the repository rather than of a promise in a doc comment.
#[test]
fn nothing_in_the_running_product_can_reach_this_module() {
    // The live surfaces that would have to name it. `include_str!` needs a
    // literal, so the list is spelled out rather than looped.
    for (path, source) in [
        (
            "src-tauri/src/main.rs",
            include_str!("../../../../src-tauri/src/main.rs"),
        ),
        (
            "src/ml/coordinator.rs",
            include_str!("../../src/ml/coordinator.rs"),
        ),
    ]
    .into_iter()
    .chain(POLICY_SOURCES.iter().copied())
    .chain(ROUTER_SOURCES.iter().copied())
    {
        let source = source.to_lowercase();
        assert!(
            !source.contains("ml::calibration"),
            "{path} must not reach ml::calibration"
        );
        assert!(
            !source.contains("run_calibration"),
            "{path} must not reach run_calibration"
        );
        assert!(
            !source.contains("decisiondistribution::try_new"),
            "{path} must not construct a distribution"
        );
    }

    // The re-export exists, so the surface is public and consumable by a later
    // node — it is simply not consumed yet.
    assert!(
        include_str!("../../src/ml/mod.rs").contains("pub mod calibration;"),
        "the module must be a public, reusable surface for the node that consumes it"
    );

    // And the report says, in words, that the vector is unconsumed.
    let snapshot = render(&main_fixture(200), 107);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    assert_eq!(outcome.report().distribution_role, DISTRIBUTION_ROLE);
    assert!(DISTRIBUTION_ROLE.contains("unconsumed"));
    assert!(DISTRIBUTION_ROLE.contains("no routing effect"));
    assert!(DISTRIBUTION_ROLE.contains("not a live probability"));
}
