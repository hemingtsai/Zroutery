//! The migration executor's **completed** path, end to end.
//!
//! # Why this file exists
//!
//! Every other migration gate drives one step at a time: one endpoint check,
//! one rollback, one child process. What none of them drove is the thing a
//! cutover actually is — a *sequence*, against a *real file*, verified against
//! a *real endpoint* — reaching `Completed` and then being undone.
//!
//! That is the shape the desktop layer runs, and it is the shape the product
//! claims works. It was the last thing the `UI-NEW-TRACKS` record could not
//! say: the `Completed` state was asserted only by fixtures that never put the
//! steps together, so the state machine's happy path was unproven even though
//! every one of its steps was proven on its own.
//!
//! The sequence here is the one the desktop builds, in the order it builds it:
//! check the port, install the staged document, then verify that the gateway
//! answers as Zroutery. The endpoint is a real `axum` server on an ephemeral
//! port, and it answers exactly what the real gateway's `/health` answers —
//! `{"status":"ok"}` — because a cutover that completes against a *different*
//! answer has proved nothing.
//!
//! The file the plan installs over is a real file in a real temporary
//! directory, and the snapshot that makes the undo possible is the real
//! snapshot the executor takes.

use axum::routing::get;
use axum::Router;
use std::path::Path;
use tokio::net::TcpListener;
use zroutery_core::migration::{
    EndpointExpect, MigrationAction, MigrationExecutor, MigrationPlan, MigrationState,
    MigrationStep, MigrationStore,
};

/// The body the real gateway's `/health` answers.
///
/// `handlers::health` returns `{"status":"ok"}`, and the desktop's plan asks
/// for a `BodyContains` of `"status":"ok"`. This constant is the same mark,
/// so the fixture and the product are asking for the same thing.
const HEALTH_BODY: &str = "{\"status\":\"ok\"}";

/// The marker the body check asks for, exactly as the desktop's plan writes
/// it.
const HEALTH_MARKER: &str = "\"status\":\"ok\"";

/// A local gateway that answers `/health` the way the real one does.
async fn gateway() -> String {
    let app = Router::new().route("/health", get(|| async { HEALTH_BODY.to_string() }));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fixture gateway");
    let address = listener
        .local_addr()
        .expect("the fixture gateway's address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// A gateway that answers, and is not this one.
///
/// The case a cutover actually hits: a stale process still holding the port,
/// or a reverse proxy in front of something else. It answers 200, so a status
/// check passes it — which is exactly why the body marker exists.
async fn impostor() -> String {
    let app = Router::new().route("/health", get(|| async { "<html>some other proxy</html>" }));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fixture impostor");
    let address = listener.local_addr().expect("the impostor's address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// The plan the desktop builds, in the order it builds it.
///
/// The step kinds, the order and the body marker are the product's; the
/// desktop's own test asserts that its builder produces exactly this shape, so
/// the two cannot drift apart silently.
fn cutover_plan(staged: &Path, client_config: &Path, gateway: &str, port: u16) -> MigrationPlan {
    MigrationPlan {
        plan_id: "cutover-fixture".to_string(),
        source_description: "a client cut over onto the gateway".to_string(),
        created_at: 1_700_000_000,
        steps: vec![
            MigrationStep {
                description: format!("check that port {port} is free"),
                action: MigrationAction::StartZroutery { port },
                reversible: false,
            },
            MigrationStep {
                description: "install the staged document".to_string(),
                action: MigrationAction::CopyConfig {
                    source: staged.to_string_lossy().into_owned(),
                    dest: client_config.to_string_lossy().into_owned(),
                },
                reversible: true,
            },
            MigrationStep {
                description: "confirm the gateway answers as Zroutery".to_string(),
                action: MigrationAction::VerifyEndpoint {
                    url: format!("{gateway}/health"),
                    expect: EndpointExpect::BodyContains(HEALTH_MARKER.to_string()),
                },
                reversible: false,
            },
        ],
    }
}

/// A cutover that runs to completion, over a real file, against a real
/// endpoint — and then is undone.
///
/// The three claims, in the order they matter:
///
/// 1. The sequence reaches `Completed`. Every step of it was proven on its own
///    before; that they compose was not.
/// 2. The client's configuration really holds the new document afterwards, and
///    the snapshot really holds the original bytes. A `Completed` state that
///    did not change the file would be a lie the state machine cannot see.
/// 3. The rollback puts the original bytes back. The point of a snapshot is
///    the undo, and an undo that is never exercised is a hope.
#[tokio::test]
async fn a_cutover_reaches_completed_over_a_real_file_and_is_undone() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let client_config = dir.path().join("settings.json");
    let staged = dir.path().join("staged.json");

    // The client as it was: a configuration of its own.
    let original = b"{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.anthropic.com\"}}";
    std::fs::write(&client_config, original).expect("write the client's configuration");

    // The staged document the cutover installs.
    let staged_body = b"{\"env\":{\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:8787\"}}";
    std::fs::write(&staged, staged_body).expect("write the staged document");

    let gateway = gateway().await;
    let port = gateway
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .expect("the fixture gateway's port");
    let plan = cutover_plan(&staged, &client_config, &gateway, port);

    // A durable store, because that is what the desktop opens and because a
    // memory-only store keeps no snapshot at all — so the undo this test is
    // about would not exist.
    let manifest = dir.path().join("manifest.json");
    let store = MigrationStore::open(&manifest).expect("a durable store");
    let executor = MigrationExecutor::new(store);

    // The snapshot comes first, and it comes from the caller rather than from
    // `execute`. That is the desktop's order: capture the file as it is, then
    // run the plan, so that a plan which fails half way still leaves a
    // restore point behind it.
    let captured = executor
        .create_snapshot(&[client_config.to_string_lossy().as_ref()])
        .expect("the snapshot of the client's configuration");
    assert_eq!(
        captured.files.len(),
        1,
        "the snapshot did not capture the client's configuration"
    );

    let result = executor.execute(&plan).await;

    assert_eq!(
        result.state,
        MigrationState::Completed,
        "the cutover did not complete: {:?}",
        result.errors
    );
    assert!(result.errors.is_empty(), "the cutover reported errors");
    assert_eq!(result.steps_completed, plan.steps.len());
    assert_eq!(result.steps_total, plan.steps.len());

    // The file really changed, and the snapshot really holds what was there.
    let installed = std::fs::read(&client_config).expect("read the installed configuration");
    assert_eq!(
        installed, staged_body,
        "the cutover completed without installing the staged document"
    );
    let snapshot = executor.snapshot_from_disk();
    assert!(
        snapshot.is_some(),
        "a cutover that overwrote a file kept no snapshot, so it could not be undone"
    );
    let snapshot = snapshot.expect("the snapshot");
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].path, client_config.to_string_lossy());
    assert_eq!(
        snapshot.files[0].content, original,
        "the snapshot did not capture the file as it was"
    );

    // And the undo puts it back.
    let rollback = executor.rollback(executor.snapshot_from_disk().as_ref());
    assert_eq!(rollback.state, MigrationState::RolledBack);
    assert!(
        rollback.rolled_back,
        "the rollback reported that it restored nothing: {:?}",
        rollback.errors
    );
    assert_eq!(rollback.restored_files.len(), 1);
    let restored = std::fs::read(&client_config).expect("read the restored configuration");
    assert_eq!(
        restored, original,
        "the rollback did not put the original bytes back"
    );
}

/// A gateway that answers, and is not this one, does not complete the
/// cutover — and the file is not left half-installed.
///
/// This is the case the body marker exists for, and here it is the *whole*
/// sequence rather than one step: the copy succeeded, the endpoint answered
/// 200, and the cutover still did not complete. A status-only check would have
/// completed it and left the client pointing at a proxy that is not this one.
#[tokio::test]
async fn a_cutover_against_the_wrong_service_does_not_complete() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let client_config = dir.path().join("settings.json");
    let staged = dir.path().join("staged.json");

    let original = b"{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.anthropic.com\"}}";
    std::fs::write(&client_config, original).expect("write the client's configuration");
    std::fs::write(&staged, b"{\"env\":{}}").expect("write the staged document");

    let gateway = impostor().await;
    let port = gateway
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .expect("the impostor's port");
    let plan = cutover_plan(&staged, &client_config, &gateway, port);

    let executor = MigrationExecutor::new(MigrationStore::new());
    let result = executor.execute(&plan).await;

    assert_eq!(
        result.state,
        MigrationState::Failed,
        "a cutover against the wrong service completed"
    );
    assert!(!result.errors.is_empty(), "the failure carried no reason");
    assert!(
        result.errors[0].contains("body"),
        "the failure did not name the body as the disagreement: {:?}",
        result.errors
    );
}

/// The port check is a *report*, and a port that is taken does not stop the
/// cutover.
///
/// This is the shape the product has, and the shape it should have: the port a
/// cutover switches to is usually Zroutery's own, and Zroutery is usually
/// already running there, so a port in use is the ordinary case rather than a
/// collision. What decides whether the holder is the right service is the
/// endpoint check and its body marker — a port number cannot tell "Zroutery is
/// serving" from "something else is serving".
///
/// So the cutover goes on to install the document, and the endpoint check is
/// what has the last word. The gate here is that the report is *in the
/// result*, because an operator who sees "port 8787 is already in use" learns
/// something the plan would otherwise have hidden from them.
#[tokio::test]
async fn a_taken_port_is_reported_rather_than_fatal() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let client_config = dir.path().join("settings.json");
    let staged = dir.path().join("staged.json");

    let original = b"{\"env\":{}}";
    std::fs::write(&client_config, original).expect("write the client's configuration");
    std::fs::write(&staged, b"{\"env\":{\"CHANGED\":true}}").expect("write the staged document");

    // Hold a port, so the plan's port report has something to find.
    let taken = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a port to take");
    let port = taken.local_addr().expect("the taken port's address").port();

    let gateway = gateway().await;
    let plan = cutover_plan(&staged, &client_config, &gateway, port);

    let executor = MigrationExecutor::new(MigrationStore::new());
    let result = executor.execute(&plan).await;

    // The port was reported, and the cutover carried on to the check that
    // actually decides.
    assert!(
        result.warnings.iter().any(|w| w.contains("already in use")),
        "the taken port was not reported: {:?}",
        result.warnings
    );
    assert_eq!(
        result.state,
        MigrationState::Completed,
        "the cutover stopped at the port report: {:?}",
        result.errors
    );
    let installed = std::fs::read(&client_config).expect("read the configuration");
    assert_eq!(
        installed, b"{\"env\":{\"CHANGED\":true}}",
        "the cutover completed without installing the staged document"
    );
}

/// The snapshot is what the undo is made of, so a cutover over a file that
/// cannot be read captures nothing — and a rollback of that nothing restores
/// nothing and says so.
///
/// The empty snapshot is still written, which is worth knowing: `recover` in a
/// later process finds a restore point and finds it empty. That is not a
/// hazard, because the engine reports a rollback that restored nothing as
/// `rolled_back: false` rather than as a success, and the desktop refuses to
/// run a cutover whose snapshot came back empty.
#[tokio::test]
async fn a_cutover_over_a_file_that_is_not_there_snapshots_nothing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let client_config = dir.path().join("absent.json");
    let staged = dir.path().join("staged.json");
    std::fs::write(&staged, b"{\"env\":{}}").expect("write the staged document");

    let executor = MigrationExecutor::new(
        MigrationStore::open(dir.path().join("manifest.json")).expect("a durable store"),
    );
    let snapshot = executor
        .create_snapshot(&[client_config.to_string_lossy().as_ref()])
        .expect("the snapshot of an absent file is not an error");

    assert!(
        snapshot.files.is_empty(),
        "a snapshot was taken of a file that was not there"
    );

    // And the undo of it restores nothing, without claiming otherwise.
    let rollback = executor.rollback(Some(&snapshot));
    assert_eq!(rollback.state, MigrationState::RolledBack);
    assert!(
        !rollback.rolled_back,
        "a rollback of an empty snapshot claimed to have restored something: {:?}",
        rollback.errors
    );
    assert!(
        rollback.restored_files.is_empty(),
        "a rollback of an empty snapshot named files it restored"
    );
}
