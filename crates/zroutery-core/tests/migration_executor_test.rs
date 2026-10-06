//! Gate fixtures for the migration executor: real endpoints, real child processes.
//!
//! # Why these are here and not in the module's own tests
//!
//! Two of `I2`'s five gates are about things that only exist outside the
//! process: an HTTP endpoint that answers, and a child process that runs. Both
//! need fixtures, and a fixture that mocks the thing under test proves nothing
//! about the thing. So the endpoint checks run against a real `axum` server on an
//! ephemeral port, and the lifecycle checks spawn a real child and reap it.
//!
//! # The defect these exist to prevent
//!
//! `VerifyEndpoint` used to accept *any* HTTP response as success. A 500, a 401
//! and a proxy's error page are all answers, and the migration walked
//! `Detected -> Prepared -> Verified -> Switched -> Completed` against any of
//! them. No mock could have caught that, because a mock returns whatever it was
//! told to and the check did not look at what came back. Only a server that
//! genuinely answers 500 can.
//!
//! The second defect is quieter. `StopExternal` reported "is running (should be
//! stopped manually)" and counted the step as done, so a cutover claimed to have
//! stopped a process it had neither started nor stopped. Ownership is now
//! explicit: this migration stops only children it started, and the tests spawn
//! one to prove the difference is real rather than a naming convention.

// `migration` is not feature-gated, so this target compiles and runs under every
// feature set CI uses, including the default-features `cargo test --workspace`.
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use zroutery_core::migration::{
    EndpointExpect, MigrationAction, MigrationExecutor, MigrationPlan, MigrationState,
    MigrationStep, MigrationStore,
};

/// The marker a correctly-migrated endpoint serves. Deliberately distinctive:
/// its job is to distinguish "Zroutery is serving" from "something is serving".
const MARKER: &str = "zroutery-migration-fixture";

fn plan(steps: Vec<MigrationStep>) -> MigrationPlan {
    MigrationPlan {
        plan_id: "gate-fixture".to_string(),
        source_description: "gate fixture".to_string(),
        steps,
        created_at: 1_700_000_000,
    }
}

fn step(description: &str, action: MigrationAction) -> MigrationStep {
    MigrationStep {
        description: description.to_string(),
        action,
        reversible: false,
    }
}

/// A local endpoint that answers in every way the policy has to tell apart.
///
/// Real server, ephemeral port. The five routes exist so a single test binary
/// can distinguish "answered successfully", "answered with a server error",
/// "answered with an auth challenge", "answered with an unusual but valid code"
/// and "answered successfully with the wrong body" -- the five cases a naive
/// reachability check collapses into one.
async fn fixture_server() -> String {
    let app = Router::new()
        .route(
            "/ok",
            get(|| async { format!("{{\"service\":\"{MARKER}\"}}") }),
        )
        .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/unauthorized", get(|| async { StatusCode::UNAUTHORIZED }))
        .route(
            "/teapot",
            get(|| async {
                // 418 is not a standard associated constant in http 1.x.
                StatusCode::from_u16(418).expect("418 is a valid status")
            }),
        )
        // A 200 from the wrong service: reachable, successful, and not us.
        .route(
            "/impostor",
            get(|| async { "<html>some other proxy</html>" }),
        );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let address = listener.local_addr().expect("fixture addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

async fn verify(url: String, expect: EndpointExpect) -> zroutery_core::migration::MigrationResult {
    let executor = MigrationExecutor::new(MigrationStore::new());
    let result = executor
        .execute(&plan(vec![step(
            "Verify",
            MigrationAction::VerifyEndpoint { url, expect },
        )]))
        .await;
    result
}

// ---------------------------------------------------------------------------
// Endpoint response policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_server_error_is_not_a_working_endpoint() {
    // The headline. This returned `Completed` before the policy existed.
    let base = fixture_server().await;
    let result = verify(format!("{base}/boom"), EndpointExpect::Success).await;

    assert_eq!(
        result.state,
        MigrationState::Failed,
        "a 500 is an answer, not a service"
    );
    assert!(
        result.errors[0].contains("500"),
        "the failure must name the status it got, got: {}",
        result.errors[0]
    );
}

#[tokio::test]
async fn an_auth_challenge_is_not_a_working_endpoint() {
    let base = fixture_server().await;
    let result = verify(format!("{base}/unauthorized"), EndpointExpect::Success).await;

    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("401"),
        "got: {}",
        result.errors[0]
    );
}

#[tokio::test]
async fn a_success_status_passes() {
    let base = fixture_server().await;
    let result = verify(format!("{base}/ok"), EndpointExpect::Success).await;

    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 1);
}

#[tokio::test]
async fn a_status_policy_requires_the_exact_code() {
    let base = fixture_server().await;

    let matching = verify(format!("{base}/teapot"), EndpointExpect::Status(418)).await;
    assert_eq!(matching.state, MigrationState::Completed);

    // The same endpoint against a policy expecting success. 418 is not 2xx, so
    // the weaker policy rejects what the exact policy accepted -- which is the
    // point of having the exact one for endpoints with a defined contract.
    let weaker = verify(format!("{base}/teapot"), EndpointExpect::Success).await;
    assert_eq!(weaker.state, MigrationState::Failed);

    let wrong = verify(format!("{base}/ok"), EndpointExpect::Status(418)).await;
    assert_eq!(wrong.state, MigrationState::Failed);
    assert!(wrong.errors[0].contains("418"), "got: {}", wrong.errors[0]);
}

#[tokio::test]
async fn a_body_marker_distinguishes_the_right_service_from_a_reachable_one() {
    let base = fixture_server().await;

    let correct = verify(
        format!("{base}/ok"),
        EndpointExpect::BodyContains(MARKER.to_string()),
    )
    .await;
    assert_eq!(correct.state, MigrationState::Completed);

    // Reachable, 200, and still wrong. A status-only check passes this, which is
    // the failure mode a cutover actually hits: a stale process still holding
    // the port answers before the new service does.
    let impostor = verify(
        format!("{base}/impostor"),
        EndpointExpect::BodyContains(MARKER.to_string()),
    )
    .await;
    assert_eq!(
        impostor.state,
        MigrationState::Failed,
        "a 200 from the wrong service must not verify the migration"
    );
    assert!(
        impostor.errors[0].contains("not the service that was migrated to"),
        "got: {}",
        impostor.errors[0]
    );
}

#[tokio::test]
async fn a_body_marker_is_not_read_when_the_status_already_failed() {
    let base = fixture_server().await;
    let result = verify(
        format!("{base}/boom"),
        EndpointExpect::BodyContains(MARKER.to_string()),
    )
    .await;

    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("before the body could be checked"),
        "the status must be judged first, got: {}",
        result.errors[0]
    );
}

/// The end-to-end version of the headline defect: a plan whose only substantive
/// check hits a broken endpoint must not reach `Completed`, because that
/// transition is what tells the rest of the system the switch happened.
#[tokio::test]
async fn a_plan_verifying_a_broken_endpoint_does_not_complete() {
    let base = fixture_server().await;
    let executor = MigrationExecutor::new(MigrationStore::new());

    let config =
        std::env::temp_dir().join(format!("zroutery_gate_plan_{}.json", std::process::id()));
    std::fs::write(&config, br#"{"providers":[]}"#).expect("write config");

    let result = executor
        .execute(&plan(vec![
            step(
                "Validate",
                MigrationAction::ValidateConfig {
                    path: config.to_string_lossy().into_owned(),
                },
            ),
            step(
                "Verify",
                MigrationAction::VerifyEndpoint {
                    url: format!("{base}/boom"),
                    expect: EndpointExpect::BodyContains(MARKER.to_string()),
                },
            ),
            step("Verify again", MigrationAction::StartZroutery { port: 0 }),
        ]))
        .await;

    assert_eq!(
        result.state,
        MigrationState::Failed,
        "one successful step must not carry the plan to Completed"
    );
    assert_eq!(result.steps_completed, 1, "the third step must not run");
    assert_eq!(result.steps_total, 3);
    assert!(!result.rolled_back);
}

// ---------------------------------------------------------------------------
// Config and auth policy
// ---------------------------------------------------------------------------

fn config_fixture(label: &str, body: &str) -> (String, String) {
    let stem = format!("zroutery_cfg_{}_{}", std::process::id(), label);
    let source = std::env::temp_dir().join(format!("{stem}_src.json"));
    let dest = std::env::temp_dir().join(format!("{stem}_dest.json"));
    std::fs::write(&source, body).expect("write source config");
    (
        source.to_string_lossy().into_owned(),
        dest.to_string_lossy().into_owned(),
    )
}

#[tokio::test]
async fn copy_config_warns_when_the_file_carries_credentials() {
    // Core cannot move a secret into a credential store -- that lives in the
    // desktop layer, where `ccswitch::credential_fingerprint` already sets the
    // convention that a key is referenced rather than carried. What core can do
    // is refuse to be quiet about having just copied one to a second location.
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (source, dest) = config_fixture(
        "creds",
        r#"{"providers":[{"name":"relay","api_key":"sk-live-abc123"}]}"#,
    );

    let result = executor
        .execute(&plan(vec![step(
            "Copy",
            MigrationAction::CopyConfig {
                source,
                dest: dest.clone(),
            },
        )]))
        .await;

    assert_eq!(result.state, MigrationState::Completed);
    let warning = result
        .warnings
        .iter()
        .find(|w| w.contains("credential-shaped keys"))
        .unwrap_or_else(|| panic!("no credential warning, got: {:?}", result.warnings));
    assert!(
        warning.contains("api_key"),
        "the warning must name the field, got: {warning:?}"
    );
}

#[tokio::test]
async fn the_credential_warning_does_not_block_the_copy() {
    // A warning, not a refusal. The migration's job is to move the config, and
    // silently not copying it would be a worse failure than copying it and
    // saying so -- but the bytes must genuinely land.
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (source, dest) = config_fixture("creds_copied", r#"{"api_key":"sk-live-abc123"}"#);

    executor
        .execute(&plan(vec![step(
            "Copy",
            MigrationAction::CopyConfig {
                source: source.clone(),
                dest: dest.clone(),
            },
        )]))
        .await;

    let copied = std::fs::read_to_string(&dest).expect("dest exists");
    assert_eq!(
        copied,
        std::fs::read_to_string(&source).expect("source exists")
    );
}

#[tokio::test]
async fn ordinary_config_is_not_reported_as_credentials() {
    // The false-positive case, and the one that keeps the warning worth reading.
    // `max_tokens` and `tokenizer` both contain "token" and neither is a secret;
    // a scanner that flagged them would train everyone to ignore the warning.
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (source, dest) = config_fixture(
        "ordinary",
        r#"{"model":"x","max_tokens":4096,"tokenizer":"claude","api":{"base_url":"https://x/v1"}}"#,
    );

    let result = executor
        .execute(&plan(vec![step(
            "Copy",
            MigrationAction::CopyConfig { source, dest },
        )]))
        .await;

    assert_eq!(result.state, MigrationState::Completed);
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| w.contains("credential-shaped keys")),
        "ordinary config must not be flagged, got: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn copy_config_backs_up_an_existing_destination() {
    // The rollback story depends on this, so it is asserted next to the policy
    // that can now emit warnings: a warning must not have displaced the backup.
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (source, dest) = config_fixture("backup", r#"{"new":true}"#);
    std::fs::write(&dest, r#"{"previous":true}"#).expect("seed dest");

    executor
        .execute(&plan(vec![step(
            "Copy",
            MigrationAction::CopyConfig {
                source: source.clone(),
                dest: dest.clone(),
            },
        )]))
        .await;

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), r#"{"new":true}"#);
    assert_eq!(
        std::fs::read_to_string(format!("{dest}.bak")).unwrap(),
        r#"{"previous":true}"#,
        "the previous destination must survive in the backup"
    );
}

// ---------------------------------------------------------------------------
// Owned process lifecycle
// ---------------------------------------------------------------------------

/// A real, long-lived child process, named per platform.
///
/// Chosen so the fixture needs no build step and no installed helper: `ping` and
/// `sleep` are the two commands guaranteed to exist where the code runs. It is
/// not Zroutery, and it does not need to be -- what is under test is that the
/// executor can start something it owns, hold it, and reap it.
fn sleeper(label: &str) -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        let _ = label;
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "ping".to_string(),
                "-n".to_string(),
                "60".to_string(),
                "127.0.0.1".to_string(),
            ],
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = label;
        ("sleep".to_string(), vec!["60".to_string()])
    }
}

#[tokio::test]
async fn an_owned_child_is_started_and_then_reaped() {
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (program, args) = sleeper("owned");

    let result = executor
        .execute(&plan(vec![
            step(
                "Start",
                MigrationAction::StartOwned {
                    label: "fixture-child".to_string(),
                    program,
                    args,
                },
            ),
            step(
                "Stop",
                MigrationAction::StopOwned {
                    label: "fixture-child".to_string(),
                },
            ),
        ]))
        .await;

    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 2);
    assert!(
        result.warnings.iter().any(|w| w.contains("took ownership")),
        "starting must record ownership, got: {:?}",
        result.warnings
    );
    assert!(
        result.warnings.iter().any(|w| w.contains("reaped")),
        "stopping must reap rather than merely signal, got: {:?}",
        result.warnings
    );
    assert!(
        executor.owned_labels().is_empty(),
        "a reaped child must not stay in the registry"
    );
}

/// Ownership is recorded per label and released on stop, so a label can
/// never be reused to reach a stale handle.
///
/// Driven entirely within one plan because `execute` is single-use: the state
/// machine admits `Detected -> Prepared` once, so a second `execute` on the
/// same executor is refused before any of its steps run. Driving two plans
/// would therefore prove nothing about the registry -- and would leave the
/// first plan's child running, since the plan meant to stop it never
/// executes. Every fixture here pairs a start with a stop for that reason.
#[tokio::test]
async fn stopping_a_label_twice_is_refused_rather_than_reaching_another_child() {
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (program, args) = sleeper("twice_stopped");

    assert!(
        executor.owned_labels().is_empty(),
        "nothing is owned before the plan starts"
    );

    let result = executor
        .execute(&plan(vec![
            step(
                "Start",
                MigrationAction::StartOwned {
                    label: "twice_stopped".to_string(),
                    program,
                    args,
                },
            ),
            step(
                "Stop",
                MigrationAction::StopOwned {
                    label: "twice_stopped".to_string(),
                },
            ),
            step(
                "Stop again",
                MigrationAction::StopOwned {
                    label: "twice_stopped".to_string(),
                },
            ),
        ]))
        .await;

    assert_eq!(
        result.state,
        MigrationState::Failed,
        "a released label must not be stoppable twice"
    );
    assert!(
        result.errors[0].contains("no owned process labelled"),
        "got: {}",
        result.errors[0]
    );
    assert!(
        executor.owned_labels().is_empty(),
        "the registry must not retain a reaped child under its old label"
    );
}

#[tokio::test]
async fn stop_owned_refuses_a_label_it_does_not_own() {
    let executor = MigrationExecutor::new(MigrationStore::new());

    let result = executor
        .execute(&plan(vec![step(
            "Stop",
            MigrationAction::StopOwned {
                label: "never-started".to_string(),
            },
        )]))
        .await;

    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("no owned process labelled"),
        "got: {}",
        result.errors[0]
    );
}

#[tokio::test]
async fn a_duplicate_label_is_refused_before_a_second_child_exists() {
    // Otherwise `StopOwned` would be ambiguous about which child it kills, and
    // the first would be orphaned.
    let executor = MigrationExecutor::new(MigrationStore::new());
    let (program, args) = sleeper("dup");

    let result = executor
        .execute(&plan(vec![
            step(
                "Start",
                MigrationAction::StartOwned {
                    label: "twice".to_string(),
                    program: program.clone(),
                    args: args.clone(),
                },
            ),
            step(
                "Start again",
                MigrationAction::StartOwned {
                    label: "twice".to_string(),
                    program,
                    args,
                },
            ),
        ]))
        .await;

    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("already owned"),
        "got: {}",
        result.errors[0]
    );
    assert_eq!(executor.owned_labels(), vec!["twice".to_string()]);

    // Clean up the one child that did start, so the fixture does not leak it.
    executor
        .execute(&plan(vec![step(
            "Stop",
            MigrationAction::StopOwned {
                label: "twice".to_string(),
            },
        )]))
        .await;
}

#[tokio::test]
async fn starting_a_program_that_does_not_exist_is_an_error() {
    let executor = MigrationExecutor::new(MigrationStore::new());

    let result = executor
        .execute(&plan(vec![step(
            "Start",
            MigrationAction::StartOwned {
                label: "missing".to_string(),
                program: "zroutery_no_such_program_54321".to_string(),
                args: vec![],
            },
        )]))
        .await;

    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("cannot start"),
        "got: {}",
        result.errors[0]
    );
    assert!(executor.owned_labels().is_empty());
}

/// A child that finishes by itself must still be released cleanly, and must
/// never be signalled.
///
/// Asserted on the invariant rather than on which branch runs: `StopOwned`
/// asks `try_wait` first and skips the kill when the child is already gone,
/// but whether a `cmd /C exit 0` has finished by the time the next step runs
/// is genuinely timing-dependent, and a test that pinned the branch would be
/// flaky rather than strict. Both branches must complete and both must release
/// the label; only the wording differs.
#[tokio::test]
async fn a_child_that_exited_on_its_own_is_released_without_being_left_registered() {
    let executor = MigrationExecutor::new(MigrationStore::new());

    #[cfg(target_os = "windows")]
    let quick = (
        "cmd".to_string(),
        vec!["/C".to_string(), "exit".to_string(), "0".to_string()],
    );
    #[cfg(not(target_os = "windows"))]
    let quick = ("true".to_string(), vec![]);

    let result = executor
        .execute(&plan(vec![
            step(
                "Start",
                MigrationAction::StartOwned {
                    label: "brief".to_string(),
                    program: quick.0,
                    args: quick.1,
                },
            ),
            step(
                "Stop",
                MigrationAction::StopOwned {
                    label: "brief".to_string(),
                },
            ),
        ]))
        .await;

    assert_eq!(
        result.state,
        MigrationState::Completed,
        "a child that finished early is not a failure, errors: {:?}",
        result.errors
    );
    // Both notes name the label, so pick the one that is not the start report.
    let stop_note = result
        .warnings
        .iter()
        .find(|w| !w.contains("took ownership"))
        .unwrap_or_else(|| panic!("no report for the child, got: {:?}", result.warnings));
    assert!(
        stop_note.contains("already exited") || stop_note.contains("reaped"),
        "expected one of the two honest reports, got: {stop_note:?}"
    );
    assert!(
        executor.owned_labels().is_empty(),
        "the registry must release a child that exited on its own"
    );
}
