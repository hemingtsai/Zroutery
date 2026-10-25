//! The server fixtures: routing, CORS, auth, ledger flushes and budget admission.

use super::*;
use crate::budget::BudgetScope;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use chrono::Local;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use super::cors::{cors_layer, origin_policy, OriginPolicy};
use super::handlers::{constant_time_eq, unknown_route, wrong_method};
use super::state::AppState;
use crate::billing::Cost;
use crate::config::ProviderKind;
use crate::config::{AppConfig, ModelTier, ProviderConfig, ServerConfig};

/// Read a handler's JSON body back out.
async fn body_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

#[test]
fn token_comparison_is_length_safe_and_exact() {
    assert!(constant_time_eq(b"zr-abc", b"zr-abc"));
    assert!(!constant_time_eq(b"zr-abc", b"zr-abd"));
    // A prefix must not pass, which is what a naive loop would allow.
    assert!(!constant_time_eq(b"zr-ab", b"zr-abc"));
    assert!(!constant_time_eq(b"", b"zr-abc"));
    assert!(constant_time_eq(b"", b""));
    // Length differences that alias through a narrow accumulator must not
    // sneak past either.
    let zeros = vec![0u8; 256];
    assert!(!constant_time_eq(b"", &zeros));
    assert!(constant_time_eq(&zeros, &zeros));
}

#[tokio::test]
async fn an_unknown_path_answers_in_the_likely_dialect() {
    // A models path is an OpenAI client's, so use its envelope.
    let response = unknown_route(Method::GET, "/v2/models".parse().unwrap()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "not_found_error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("GET /v2/models"));
    assert!(body["zroutery"]["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e == "/v1/models"));

    // Anything that mentions messages is an Anthropic client's.
    let response = unknown_route(Method::POST, "/v1/messages/typo".parse().unwrap()).await;
    let body = body_json(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "not_found_error");
}

#[tokio::test]
async fn a_doubled_version_prefix_is_named_as_the_cause() {
    let response = unknown_route(Method::POST, "/v1/v1/messages".parse().unwrap()).await;
    let body = body_json(response).await;
    let cause = body["zroutery"]["likely_cause"].as_str().unwrap();
    assert!(cause.contains("base URL already ends in /v1"), "{cause}");
    assert!(cause.contains("/v1/messages"), "{cause}");

    // A single prefix is normal and gets no lecture.
    let response = unknown_route(Method::POST, "/v1/nope".parse().unwrap()).await;
    assert!(body_json(response).await["zroutery"]["likely_cause"].is_null());
}

#[tokio::test]
async fn a_wrong_verb_names_the_verb_it_wants() {
    let response = wrong_method(Method::GET, "/v1/chat/completions".parse().unwrap()).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let message = body_json(response).await["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("POST endpoint"), "{message}");
    assert!(message.contains("not GET"), "{message}");

    // Listings are the other way round.
    let response = wrong_method(Method::POST, "/v1/models".parse().unwrap()).await;
    let message = body_json(response).await["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("GET endpoint"), "{message}");
}

#[test]
fn cors_is_off_unless_asked_for() {
    let mut server = ServerConfig::default();
    assert!(cors_layer(&server).is_none());
    server.allow_cors = true;
    assert!(cors_layer(&server).is_some());
    // An empty list still builds a layer, and `validate` is what complains.
    server.cors_origins = vec!["http://localhost:3000".into()];
    assert!(cors_layer(&server).is_some());
    // A malformed origin is dropped rather than rejected: the rest still works.
    server.cors_origins = vec!["not a header value\n".into()];
    assert!(cors_layer(&server).is_some());
}

#[test]
fn is_valid_origin_rejects_paths_scheme_less_and_bad_ports() {
    assert!(is_valid_origin("http://localhost:3000"));
    assert!(is_valid_origin("https://app.example.com"));
    assert!(is_valid_origin("tauri://localhost"));
    assert!(is_valid_origin("http://[::1]:8787"));
    assert!(is_valid_origin("http://[::1]"));
    assert!(!is_valid_origin("https://app.example.com/path"));
    assert!(!is_valid_origin("https://app.example.com?q=1"));
    assert!(!is_valid_origin("app.example.com"));
    assert!(!is_valid_origin(""));
    assert!(!is_valid_origin("has space://x"));
    assert!(!is_valid_origin("https://app.example.com:99999"));
    assert!(!is_valid_origin("https://app.example.com:abc"));
    assert!(!is_valid_origin("https://app.example.com:"));
    assert!(!is_valid_origin("https://user@example.com"));
    assert!(!is_valid_origin("https://例子.测试"));
}

#[test]
fn origin_policy_never_widens_on_invalid_entries() {
    use OriginPolicy::*;
    // Nothing configured: documented Any.
    assert_eq!(origin_policy(&[]), Any);
    // Valid entries: a list.
    assert_eq!(
        origin_policy(&["http://a".into(), "https://b".into()]),
        List(vec!["http://a".to_string(), "https://b".to_string()])
    );
    // Mixed: invalid entries are dropped, valid ones survive.
    assert_eq!(
        origin_policy(&["https://ok".into(), "garbage".into()]),
        List(vec!["https://ok".into()])
    );
    // All invalid (or whitespace-only): deny — never widen to Any.
    assert_eq!(origin_policy(&["garbage".into()]), DenyAll);
    assert_eq!(origin_policy(&["   ".into()]), Any);
}

#[test]
fn a_missing_key_is_a_precondition_not_a_silent_none() {
    let secrets = Arc::new(crate::config::MemorySecretStore::new().with("provider:has", "sk-1"));
    let state = AppState::new(AppConfig::default(), secrets);

    let mut provider = ProviderConfig::new("has", "Has", ProviderKind::OpenAICompatible);
    provider.key_ref = "provider:has".into();
    assert_eq!(state.api_key(&provider).unwrap().as_deref(), Some("sk-1"));

    provider.key_ref = "provider:missing".into();
    assert!(matches!(
        state.api_key(&provider),
        Err(Error::MissingApiKey(_))
    ));

    // A provider that needs no credential says so explicitly.
    provider.key_ref = String::new();
    assert!(state.api_key(&provider).unwrap().is_none());
}

#[test]
fn the_free_tier_is_anonymous_when_no_key_was_ever_stored() {
    // The preset carries a key_ref, but the free gate authenticates the
    // request shape, not a key. A missing secret must not fail the provider.
    let state = AppState::new(
        AppConfig::default(),
        Arc::new(crate::config::MemorySecretStore::new()),
    );
    let provider = ProviderConfig::new("zen", "OpenCode Zen", ProviderKind::OpenCodeZenFree);
    assert!(!provider.key_ref.is_empty(), "the preset names a secret");
    assert_eq!(state.api_key(&provider).unwrap().as_deref(), Some("public"));

    // The anonymous preset never borrows a paid credential; Console/ordinary
    // providers are separate configurations for those credentials.
    let with_key = Arc::new(crate::config::MemorySecretStore::new().with("provider:zen", "sk-zen"));
    let state = AppState::new(AppConfig::default(), with_key);
    let provider = ProviderConfig::new("zen", "OpenCode Zen", ProviderKind::OpenCodeZenFree);
    assert_eq!(state.api_key(&provider).unwrap().as_deref(), Some("public"));
}

#[test]
fn swapping_the_config_rebuilds_the_registry_with_it() {
    let secrets = Arc::new(crate::config::MemorySecretStore::new());
    let state = AppState::new(AppConfig::default(), secrets);
    assert!(state.registry().list().is_empty());

    let mut next = AppConfig::default();
    next.providers.push(ProviderConfig::new(
        "p",
        "P",
        ProviderKind::OpenAICompatible,
    ));
    next.models.push(crate::config::ModelEntry::for_upstream(
        "p",
        "m",
        Some(crate::config::ModelTier::Standard),
    ));
    state.set_config(next);

    // The cached index has to move with the document, not lag behind it.
    assert_eq!(state.config().models.len(), 1);
    assert!(state.registry().list().iter().any(|m| m.id == "p-m"));
    assert!(state
        .registry()
        .resolve("standard-class")
        .is_ok_and(|r| matches!(r, crate::registry::Resolution::Tier(_))));
}

/// The shadow switch and retention are configuration the running engine
/// carries: the documented limits arrive at construction, and `set_config`
/// applies a changed switch and smaller limits without a restart.
#[cfg(feature = "ml")]
#[test]
fn swapping_the_config_applies_shadow_settings_to_the_running_engine() {
    let secrets = Arc::new(crate::config::MemorySecretStore::new());
    let mut initial = AppConfig::default();
    initial.shadow.enabled = true;
    initial.shadow.max_decisions = 5;
    initial.shadow.max_age_secs = 60;
    let state = AppState::new(initial, secrets);
    assert!(state.shadow().enabled());
    assert_eq!(
        state.shadow().store().limits(),
        (5, 60),
        "the configured retention must reach the store"
    );

    let mut next = state.config().as_ref().clone();
    next.shadow.enabled = false;
    next.shadow.max_decisions = 2;
    next.shadow.max_age_secs = 30;
    state.set_config(next);
    assert!(
        !state.shadow().enabled(),
        "turning shadow off must reach the running engine"
    );
    assert_eq!(
        state.shadow().store().limits(),
        (2, 30),
        "the new retention must reach the running store"
    );

    let mut back_on = state.config().as_ref().clone();
    back_on.shadow.enabled = true;
    state.set_config(back_on);
    assert!(
        state.shadow().enabled(),
        "turning shadow back on must reach the running engine"
    );
}

/// The global day spend the ledger currently holds.
fn global_day_spend(state: &AppState) -> f64 {
    state
        .ledger()
        .totals_for(&crate::budget::BudgetScope::Global, Local::now())
        .into_iter()
        .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .unwrap_or(0.0)
}

fn state_with_charge(amount: f64) -> AppState {
    let state = AppState::new(
        AppConfig::default(),
        Arc::new(crate::config::MemorySecretStore::new()),
    );
    state.charge(
        "provider",
        Some(ModelTier::Standard),
        &Cost {
            currency: "USD".into(),
            amount,
        },
    );
    state
}

#[test]
fn a_failed_flush_keeps_the_pending_spend_and_the_next_flush_retries() {
    let state = state_with_charge(1.0);
    let mut failed_attempts = 0;

    let result = state.flush_ledger(|_| {
        failed_attempts += 1;
        Err("disk is not writable".to_string())
    });
    assert_eq!(result, Err("disk is not writable".to_string()));
    assert_eq!(failed_attempts, 1);
    assert_eq!(
        global_day_spend(&state),
        1.0,
        "a failed write must not drop the spend it could not persist"
    );

    // The disk recovers: the very next flush must still see the pending
    // spend and write it, not report "nothing changed".
    let mut written: Option<f64> = None;
    state
        .flush_ledger(|ledger| {
            written = Some(
                ledger
                    .totals_for(&crate::budget::BudgetScope::Global, Local::now())
                    .into_iter()
                    .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
                    .map(|(_, cost)| cost.amount)
                    .unwrap_or(0.0),
            );
            Ok(())
        })
        .expect("the retry succeeds once the destination is writable again");
    assert_eq!(written, Some(1.0));

    // Nothing is pending after a successful write, so a flush on an idle
    // proxy does not rewrite the file.
    let mut writes_after_success = 0;
    state
        .flush_ledger(|_| {
            writes_after_success += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(writes_after_success, 0);
}

#[test]
fn a_charge_that_lands_during_a_write_survives_the_confirmation() {
    let state = state_with_charge(1.0);

    // The charge arrives while the snapshot is on its way to disk. Clearing
    // the dirty flag unconditionally afterwards would lose it.
    state
        .flush_ledger(|_| {
            state.charge(
                "provider",
                Some(ModelTier::Standard),
                &Cost {
                    currency: "USD".into(),
                    amount: 0.5,
                },
            );
            Ok(())
        })
        .expect("the first write succeeds");

    let mut written: Option<f64> = None;
    state
        .flush_ledger(|ledger| {
            written = Some(
                ledger
                    .totals_for(&crate::budget::BudgetScope::Global, Local::now())
                    .into_iter()
                    .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
                    .map(|(_, cost)| cost.amount)
                    .unwrap_or(0.0),
            );
            Ok(())
        })
        .expect("the mid-write charge is flushed next");
    assert_eq!(written, Some(1.5));
}

#[test]
fn concurrent_flushes_never_run_their_writes_at_the_same_time() {
    let state = Arc::new(state_with_charge(1.0));
    let inside = Arc::new(AtomicBool::new(false));
    let overlap = Arc::new(AtomicBool::new(false));

    let mut threads = Vec::new();
    for _ in 0..8 {
        let state = Arc::clone(&state);
        let inside = Arc::clone(&inside);
        let overlap = Arc::clone(&overlap);
        threads.push(std::thread::spawn(move || {
            state.flush_ledger(|_| {
                if inside.swap(true, Ordering::AcqRel) {
                    overlap.store(true, Ordering::Release);
                }
                std::thread::sleep(Duration::from_millis(20));
                inside.store(false, Ordering::Release);
                Ok(())
            })
        }));
    }
    for thread in threads {
        thread.join().expect("no flush panicked").unwrap();
    }
    assert!(
        !overlap.load(Ordering::Acquire),
        "two flushes wrote at once, so an older snapshot could overwrite a newer one"
    );
}

fn budget_scope_state() -> AppState {
    AppState::new(
        AppConfig::default(),
        Arc::new(crate::config::MemorySecretStore::new()),
    )
}

/// One permit per scope, released by the drop and never by an explicit call.
#[tokio::test]
async fn an_admitted_scope_is_exclusive_until_the_guard_drops() {
    let state = budget_scope_state();
    let scope = BudgetScope::Global;

    let held = state.admit(std::slice::from_ref(&scope)).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            state.admit(std::slice::from_ref(&scope))
        )
        .await
        .is_err(),
        "a second request for a held scope must wait, not run against the same total"
    );
    drop(held);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            state.admit(std::slice::from_ref(&scope))
        )
        .await
        .is_ok(),
        "dropping the guard must release the scope"
    );
}

/// A request that occupies no budgeted scope is never gated, and two
/// different scopes do not block each other.
#[tokio::test]
async fn scopes_without_a_budget_stay_concurrent() {
    let state = budget_scope_state();
    let provider = BudgetScope::Provider {
        id: "deepseek".into(),
    };

    let held = state.admit(std::slice::from_ref(&provider)).await;
    // Nothing budgeted at all: admitted immediately, no waiting.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), state.admit(&[]))
            .await
            .is_ok(),
        "an unbudgeted request must not wait on anyone"
    );
    // A different scope is a different gate.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            state.admit(&[BudgetScope::Tier {
                tier: ModelTier::Standard
            }])
        )
        .await
        .is_ok(),
        "one held scope must not serialise another"
    );
    drop(held);
}

/// Removing a scope's budget while a request holds its permit must not
/// strand the permit: the gate outlives the configuration that made it.
#[tokio::test]
async fn removing_a_budget_does_not_leak_an_in_flight_permit() {
    let state = budget_scope_state();
    let scope = BudgetScope::Global;
    let held = state.admit(std::slice::from_ref(&scope)).await;

    // The budget disappears while the request is still in flight.
    state.set_config(AppConfig::default());
    drop(held);

    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            state.admit(std::slice::from_ref(&scope))
        )
        .await
        .is_ok(),
        "the released permit must still be the one the gate hands out"
    );
}

// ---------------------------------------------------------------- bursts
//
// The three tests below are the performance contract of the accounting path
// under load, stated as behaviour rather than as a benchmark: a burst of a
// thousand charges must all survive a flush, a writer that stalls must not
// stall the request path, and a scope at its limit must keep handing its gate
// to the next request without deadlocking.

/// The global day total in a ledger the test is about to write out.
fn flushed_day_total(ledger: &crate::budget::Ledger) -> f64 {
    ledger
        .totals_for(&BudgetScope::Global, Local::now())
        .into_iter()
        .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .unwrap_or(0.0)
}

/// A 1k burst, flushed concurrently, loses nothing and never writes an older
/// total over a newer one.
///
/// The protocol under test is `ledger_dirty` + `ledger_generation` + the flush
/// mutex: the flusher snapshots, writes, and only clears the flag when the
/// generation still matches. A regression here is silent — a request's spend
/// disappears from `spend.json` and the budget lets it through again after a
/// restart — so the assertion is on the sequence of written totals, not only on
/// the last one.
#[test]
fn a_thousand_charges_survive_concurrent_flushes() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 125;
    const EACH: f64 = 0.001;
    let expected = THREADS as f64 * PER_THREAD as f64 * EACH;

    let state = Arc::new(budget_scope_state());
    let written: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let flusher = {
        let state = Arc::clone(&state);
        let written = Arc::clone(&written);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let seen = Arc::clone(&written);
                // The save closure runs with the flush lock held, so recording
                // here records the order the writes actually happened in.
                let _ = state.flush_ledger(|ledger| {
                    seen.lock().unwrap().push(flushed_day_total(ledger));
                    Ok(())
                });
                std::thread::yield_now();
            }
        })
    };

    let mut chargers = Vec::new();
    for _ in 0..THREADS {
        let state = Arc::clone(&state);
        chargers.push(std::thread::spawn(move || {
            for _ in 0..PER_THREAD {
                state.charge(
                    "provider",
                    Some(ModelTier::Standard),
                    &Cost {
                        currency: "USD".into(),
                        amount: EACH,
                    },
                );
            }
        }));
    }
    for charger in chargers {
        charger.join().expect("no charge panicked");
    }
    stop.store(true, Ordering::Release);
    flusher.join().expect("the flusher panicked");

    // The final flush has to see everything the burst charged. It writes into
    // the same log, because the flusher thread stopped on the flag rather than
    // on the last charge: its final write may have been taken before the last
    // charge landed, and reading *its* last value would be reading a snapshot
    // of a race rather than the result.
    //
    // If the flusher already wrote everything, the ledger is no longer dirty
    // and this flush writes nothing at all — so the assertion is on the
    // largest total any write ever carried, not on the last one.
    let final_total = Arc::new(Mutex::new(None::<f64>));
    {
        let final_total = Arc::clone(&final_total);
        state
            .flush_ledger(|ledger| {
                *final_total.lock().unwrap() = Some(flushed_day_total(ledger));
                Ok(())
            })
            .expect("the last flush succeeds");
    }
    let written_totals = written.lock().unwrap().clone();
    let final_written = *final_total.lock().unwrap();
    let last = written_totals
        .iter()
        .copied()
        .chain(final_written)
        .fold(0.0f64, f64::max);
    assert!(
        (last - expected).abs() < 1e-9,
        "some write must contain all {THREADS}x{PER_THREAD} charges: \
         expected {expected}, wrote {last}"
    );

    // And no flush ever wrote a total smaller than one it had already written:
    // that is what "an older snapshot must not overwrite a newer one" means on
    // disk, observed from the values rather than from the lock.
    for pair in written_totals.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "a flush wrote an older total over a newer one: {pair:?}"
        );
    }
    assert!(
        (flushed_day_total(&state.ledger()) - expected).abs() < 1e-9,
        "in-memory accounting must agree with the flush"
    );
}

/// A writer that blocks inside `save` must not block the request path.
///
/// This is the A9 defect in miniature: the disk belongs to the desktop shell,
/// and if a slow `std::fs::write` were called while the ledger lock was held,
/// every charge behind it would wait for the filesystem. The test holds a
/// writer inside `save` and proves that charges complete anyway, then that the
/// stalled snapshot and the next flush are both honest about what they contain.
#[test]
fn a_stalled_writer_does_not_stall_charges() {
    const CHARGE_THREADS: usize = 4;
    const PER_THREAD: usize = 25;
    const EACH: f64 = 0.001;

    let state = Arc::new(state_with_charge(EACH));
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let stalled_total = Arc::new(Mutex::new(None::<f64>));

    let writer = {
        let state = Arc::clone(&state);
        let stalled_total = Arc::clone(&stalled_total);
        std::thread::spawn(move || {
            state.flush_ledger(|ledger| {
                entered_tx.send(()).expect("the test is still listening");
                // Blocks until the charges below have finished. If a charge
                // needed the flush lock, this would deadlock the whole scope.
                release_rx.recv().expect("the test releases the writer");
                *stalled_total.lock().unwrap() = Some(flushed_day_total(ledger));
                Ok(())
            })
        })
    };
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the writer must reach the save closure");

    let charged = Arc::new(AtomicUsize::new(0));
    let mut chargers = Vec::new();
    for _ in 0..CHARGE_THREADS {
        let state = Arc::clone(&state);
        let charged = Arc::clone(&charged);
        chargers.push(std::thread::spawn(move || {
            for _ in 0..PER_THREAD {
                state.charge(
                    "provider",
                    Some(ModelTier::Standard),
                    &Cost {
                        currency: "USD".into(),
                        amount: EACH,
                    },
                );
                charged.fetch_add(1, Ordering::AcqRel);
            }
        }));
    }

    // Poll rather than join, so a regression fails the test instead of hanging
    // the suite: the writer is only released on the way out.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let target = CHARGE_THREADS * PER_THREAD;
    while charged.load(Ordering::Acquire) < target && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    let reached = charged.load(Ordering::Acquire);
    let _ = release_tx.send(());
    for charger in chargers {
        charger.join().expect("no charge panicked");
    }
    writer
        .join()
        .expect("the writer panicked")
        .expect("the write succeeds");

    assert_eq!(
        reached, target,
        "charges must not wait on a writer that is inside `save`"
    );
    // The snapshot the stalled writer took holds exactly what existed when it
    // started; the burst is not in it, and is not lost either.
    let stalled = stalled_total.lock().unwrap().expect("the writer recorded");
    assert!(
        (stalled - EACH).abs() < 1e-9,
        "the snapshot must be the pre-burst total, got {stalled}"
    );
    let mut after = None;
    state
        .flush_ledger(|ledger| {
            after = Some(flushed_day_total(ledger));
            Ok(())
        })
        .expect("the next flush succeeds");
    let after = after.expect("the burst left the ledger dirty");
    let expected = EACH + target as f64 * EACH;
    assert!(
        (after - expected).abs() < 1e-9,
        "every charge that landed during the write must be flushed next time: \
         expected {expected}, wrote {after}"
    );
}

/// A refused request must hand its gate back, or the retry waits on the permit
/// the refusal still holds.
///
/// The pipeline's shape for an over-budget request is: take the gate, check the
/// budget, refuse, drop the guard. A guard that outlived the refusal would make
/// the client's rectified retry — which takes the same scope — wait forever on
/// a permit nobody is going to release.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_requests_release_their_gate_for_the_retry() {
    let state = Arc::new(budget_scope_state());
    let scope = BudgetScope::Global;
    let mut tasks = Vec::new();
    for _ in 0..64 {
        let state = Arc::clone(&state);
        let scope = scope.clone();
        tasks.push(tokio::spawn(async move {
            let guard = state.admit(std::slice::from_ref(&scope)).await;
            // The verdict is read while the gate is held, exactly as the
            // pipeline does before deciding to refuse.
            let _ = state.budget_verdict(&[], None);
            drop(guard);
        }));
    }
    for (index, task) in tasks.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap_or_else(|_| panic!("request {index} deadlocked on its own scope"))
            .expect("no task panicked");
    }

    // The scope is still usable afterwards, which is the property a stranded
    // permit would have taken away.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            state.admit(std::slice::from_ref(&BudgetScope::Global))
        )
        .await
        .is_ok(),
        "the gate must be free once every refused request has dropped its guard"
    );
}

/// The gate is still exclusive under contention, and contention still drains.
///
/// Exclusivity is the budget guarantee: two requests inside one scope would
/// both read the same historical total and both pass. Draining is the liveness
/// guarantee: a gate nobody releases is a deadlock. The counter below observes
/// the first from inside the critical section, and the timeout observes the
/// second.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_contended_scope_stays_exclusive_and_still_drains() {
    let state = Arc::new(budget_scope_state());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();

    for _ in 0..64 {
        let state = Arc::clone(&state);
        let inside = Arc::clone(&inside);
        let peak = Arc::clone(&peak);
        tasks.push(tokio::spawn(async move {
            let guard = state.admit(&[BudgetScope::Global]).await;
            let now = inside.fetch_add(1, Ordering::AcqRel) + 1;
            peak.fetch_max(now, Ordering::AcqRel);
            // Yield so a second holder, if the gate were broken, would be
            // observed instead of the window closing inside one poll.
            tokio::task::yield_now().await;
            inside.fetch_sub(1, Ordering::AcqRel);
            drop(guard);
        }));
    }
    for (index, task) in tasks.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap_or_else(|_| panic!("request {index} never got its turn"))
            .expect("no task panicked");
    }
    assert_eq!(
        peak.load(Ordering::Acquire),
        1,
        "two requests were inside one budget scope at once, so the overshoot bound is not real"
    );
}
