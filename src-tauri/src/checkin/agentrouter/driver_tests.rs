//! The real asynchronous driver against a local CDP server. These tests do not
//! start Chromium, make upstream calls or store any browser credentials.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::secrets::{CredentialBackend, KeychainSecrets, StoreError};

#[derive(Clone, Copy)]
enum Scenario {
    Completed,
    Pause,
    PopupClosed,
    TransientError,
    Unconfirmed,
    CancelAfterResponse,
    ConcurrentPageClose,
}

struct Fixture {
    scenario: Scenario,
    prepared: usize,
    oauth: usize,
    observations: usize,
    callback_ready: bool,
    popup_created: bool,
    closed: BTreeSet<String>,
    browser_closed: usize,
    started_at: i64,
    /// Stops the fake browser's listener when the terminal path sends
    /// `Browser.close`, so the port probe observes the close.
    stop_listening: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

impl Fixture {
    fn targets(&self) -> Value {
        let mut targets = vec![
            json!({"targetId":"login","type":"page","url":format!("{ORIGIN}/login")}),
            json!({"targetId":"unrelated","type":"page","url":"https://other.example/"}),
        ];
        if self.popup_created && !matches!(self.scenario, Scenario::PopupClosed) {
            let url = if self.callback_ready {
                format!("{ORIGIN}/console/token")
            } else {
                "https://github.com/login/oauth/authorize".into()
            };
            targets.push(json!({"targetId":"popup","type":"page","url":url,"openerId":"login"}));
        }
        targets.retain(|target| !self.closed.contains(target["targetId"].as_str().unwrap()));
        json!({"targetInfos":targets})
    }

    fn answer(&mut self, command: &Value) -> (Value, bool) {
        let method = command["method"].as_str().unwrap();
        let result = match method {
            "Target.getTargets" => self.targets(),
            "Target.attachToTarget" => json!({"sessionId":"popup-session"}),
            "Target.closeTarget" => {
                let id = command["params"]["targetId"].as_str().unwrap();
                assert_ne!(id, "unrelated");
                self.closed.insert(id.into());
                if matches!(self.scenario, Scenario::ConcurrentPageClose) && id == "login" {
                    return (
                        json!({"id":command["id"],"error":{"message":"target closed concurrently"}}),
                        false,
                    );
                }
                json!({"success":true})
            }
            "Runtime.evaluate" => {
                let expression = command["params"]["expression"].as_str().unwrap();
                let value = if expression.contains("application:") {
                    json!(json!({"url":format!("{ORIGIN}/login"),"title":"Panel",
                        "body":"Sign in","hosts":[],"application":true})
                    .to_string())
                } else if expression.contains("const logout") {
                    self.prepared += 1;
                    json!(r#"{"state":"prepared"}"#)
                } else if expression.contains("const authorize") {
                    assert_eq!(command["params"]["userGesture"], true);
                    self.oauth += 1;
                    self.popup_created = true;
                    json!(r#"{"state":"started"}"#)
                } else if expression.contains("document.readyState") {
                    json!(true)
                } else if expression.contains("last_login_time") {
                    assert_eq!(command["sessionId"], "popup-session");
                    self.observations += 1;
                    if matches!(self.scenario, Scenario::TransientError) {
                        return (
                            json!({"id":command["id"],"error":{"message":"must-not-echo-cookie"}}),
                            false,
                        );
                    }
                    if matches!(self.scenario, Scenario::Unconfirmed) {
                        json!(r#"{"state":"unconfirmed"}"#)
                    } else {
                        let response = json!({"state":"signed_in","user_id":"7",
                            "last_login_time":self.started_at + 1,"checked_in":true})
                        .to_string();
                        let cancel = matches!(self.scenario, Scenario::CancelAfterResponse);
                        return (
                            json!({"id":command["id"],"result":{"result":{"type":"string","value":response}}}),
                            cancel,
                        );
                    }
                } else {
                    panic!("unexpected probe");
                };
                json!({"result":{"value":value}})
            }
            "Page.enable" | "Page.navigate" | "Runtime.enable" => json!({}),
            // The terminal path now shuts the whole flow-owned browser down.
            "Browser.close" => {
                self.browser_closed += 1;
                if let Some(stop) = &self.stop_listening {
                    let _ = stop.send(());
                }
                json!({})
            }
            unexpected => panic!("unexpected CDP command {unexpected}"),
        };
        (json!({"id":command["id"],"result":result}), false)
    }
}

struct NoSecrets;
impl CredentialBackend for NoSecrets {
    fn get(&self, _: &str) -> Result<String, StoreError> {
        Err(StoreError::NoEntry)
    }
    fn set(&self, _: &str, _: &str) -> Result<(), StoreError> {
        panic!("test must not persist a secret")
    }
    fn delete(&self, _: &str) -> Result<(), StoreError> {
        Ok(())
    }
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    desktop: Arc<Desktop>,
    runtime: CheckinRuntime,
    key: SessionKey,
    fixture: Arc<Mutex<Fixture>>,
    server: tokio::task::JoinHandle<()>,
    _scratch: Scratch,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Harness {
    async fn new(scenario: Scenario) -> (Self, BrowserSession, PageBinding) {
        let root = std::env::temp_dir()
            .join("opencode")
            .join(format!("zr-login-driver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = serde_json::from_value(json!({"providers":[{
            "id":"p","name":"Panel","kind":"openai_compatible","base_url":ORIGIN,
            "accounts":[{"account_id":"a","probe_kind":"none","maintenance":{
                "checkin_enabled":true,"checkin_interval_secs":86400,"new_api_user":"7"
            }}]
        }]}))
        .unwrap();
        let secrets = Arc::new(KeychainSecrets::with_backend(Box::new(NoSecrets)));
        let desktop = Arc::new(Desktop::new(root.clone(), config, secrets));
        let runtime = desktop.checkin.clone();
        let key = SessionKey::new("p", "a");
        runtime.begin(key.clone()).await.unwrap();
        for event in [Event::Start, Event::BrowserStarted, Event::Navigated] {
            runtime.advance(&key, event).await.unwrap();
        }
        let started_at = chrono::Utc::now().timestamp();
        let mut attempt = Attempt::new(
            started_at,
            crate::checkin_commands::checkin_binding_version(&desktop, "p", "a").unwrap(),
            "7".into(),
        );
        attempt.login_target = Some("login".into());
        let popup_created = matches!(scenario, Scenario::PopupClosed);
        if popup_created {
            attempt.prepared = true;
            attempt.oauth_started = true;
            attempt.popup_target = Some("popup".into());
        }
        let fixture = Arc::new(Mutex::new(Fixture {
            scenario,
            prepared: 0,
            oauth: 0,
            observations: 0,
            callback_ready: !matches!(scenario, Scenario::Pause),
            popup_created,
            closed: BTreeSet::new(),
            browser_closed: 0,
            started_at,
            stop_listening: None,
        }));
        let (stop_listening, mut stop_requests) = tokio::sync::mpsc::unbounded_channel();
        fixture.lock().unwrap().stop_listening = Some(stop_listening);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let fixture_for_server = fixture.clone();
        let runtime_for_server = runtime.clone();
        let key_for_server = key.clone();
        let server = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    // The driver's terminal path asked the browser to close; drop
                    // the listener so the port probe observes it.
                    _ = stop_requests.recv() => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let fixture = fixture_for_server.clone();
                        let runtime = runtime_for_server.clone();
                        let key = key_for_server.clone();
                        connections.spawn(async move {
                            let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else { return };
                            while let Some(Ok(Message::Text(text))) = socket.next().await {
                                let command: Value = serde_json::from_str(&text).unwrap();
                                let (answer, cancel) = fixture.lock().unwrap().answer(&command);
                                if cancel {
                                    runtime.advance(&key, Event::Cancelled).await.unwrap();
                                    assert_eq!(runtime.finish(&key).await, super::super::FinishOutcome::DeferredToDriver);
                                }
                                if socket.send(Message::Text(answer.to_string().into())).await.is_err() { break; }
                            }
                        });
                    }
                    joined = connections.join_next(), if !connections.is_empty() => { joined.unwrap().unwrap(); }
                }
            }
        });
        let binding = PageBinding {
            target_id: "login".into(),
            session_id: "login-session".into(),
        };
        let browser = BrowserSession::test_connection(&endpoint, binding.clone(), attempt)
            .await
            .unwrap();
        runtime.attach(&key, browser).await.unwrap();
        let browser = runtime.take_browser(&key).await.unwrap();
        (
            Self {
                desktop,
                runtime,
                key,
                fixture,
                server,
                _scratch: Scratch(root),
            },
            browser,
            binding,
        )
    }

    async fn run(&self, browser: BrowserSession, binding: PageBinding) {
        drive_with_limits(
            self.runtime.clone(),
            self.desktop.clone(),
            self.key.clone(),
            browser,
            binding,
            DriveLimits {
                timeout: Duration::from_secs(2),
                human: Duration::from_secs(2),
                poll: Duration::from_millis(1),
            },
        )
        .await;
    }

    async fn view(&self) -> CheckinView {
        self.desktop.checkin_view("p", "a").await.unwrap()
    }
}

#[tokio::test]
async fn the_real_driver_runs_prepare_oauth_callback_observe_and_settles_once() {
    let (harness, browser, binding) = Harness::new(Scenario::Completed).await;
    harness.run(browser, binding).await;
    let view = harness.view().await;
    assert_eq!(view.phase, "succeeded");
    assert!(view.reward_amount.is_none());
    let fixture = harness.fixture.lock().unwrap();
    assert_eq!(
        (fixture.prepared, fixture.oauth, fixture.observations),
        (1, 1, 1)
    );
    assert_eq!(
        fixture.closed,
        BTreeSet::from(["login".into(), "popup".into()])
    );
    assert_eq!(
        fixture.browser_closed, 1,
        "a terminal attempt closes the browser it launched"
    );
    assert!(harness
        .desktop
        .checkin_schedule
        .last_settled_at("p", "a")
        .is_some());
}

#[tokio::test]
async fn the_real_driver_resumes_the_existing_callback_without_another_logout_or_oauth() {
    let (harness, browser, binding) = Harness::new(Scenario::Pause).await;
    harness.run(browser, binding).await;
    assert_eq!(harness.view().await.phase, "waiting_for_user");
    assert!(harness
        .desktop
        .checkin_schedule
        .last_settled_at("p", "a")
        .is_none());
    assert_eq!(
        harness.fixture.lock().unwrap().browser_closed,
        0,
        "a pause keeps the flow-owned browser open for its resume"
    );
    harness.fixture.lock().unwrap().callback_ready = true;
    harness
        .runtime
        .advance(&harness.key, Event::UserResumed)
        .await
        .unwrap();
    let mut browser = harness.runtime.take_browser(&harness.key).await.unwrap();
    let binding = browser.resume_page(ORIGIN).await.unwrap().unwrap();
    harness
        .runtime
        .advance(&harness.key, Event::OperationRunning)
        .await
        .unwrap();
    harness.run(browser, binding).await;
    assert_eq!(harness.view().await.phase, "succeeded");
    let fixture = harness.fixture.lock().unwrap();
    assert_eq!(
        (fixture.prepared, fixture.oauth, fixture.observations),
        (1, 1, 1)
    );
    assert_eq!(
        fixture.browser_closed, 1,
        "the resumed attempt closes the browser it launched"
    );
}

#[tokio::test]
async fn a_closed_owned_callback_ends_unconfirmed_instead_of_waiting_or_replaying() {
    let (harness, browser, binding) = Harness::new(Scenario::PopupClosed).await;
    harness.run(browser, binding).await;
    let view = harness.view().await;
    assert_eq!(view.phase, "failed");
    assert!(view.failure.unwrap().contains("callback page closed"));
    let fixture = harness.fixture.lock().unwrap();
    assert_eq!(
        (fixture.prepared, fixture.oauth, fixture.observations),
        (0, 0, 0)
    );
    assert_eq!(fixture.browser_closed, 1);
}

#[tokio::test]
async fn a_cancel_after_the_response_wins_before_success_is_published() {
    let (harness, browser, binding) = Harness::new(Scenario::CancelAfterResponse).await;
    harness.run(browser, binding).await;
    let view = harness.view().await;
    assert_eq!(view.phase, "cancelled");
    assert!(harness.desktop.account_events.recent(10).is_empty());
    assert_eq!(harness.fixture.lock().unwrap().browser_closed, 1);
}

#[tokio::test]
async fn five_protocol_errors_end_without_leaking_the_error_or_restarting_oauth() {
    let (harness, browser, binding) = Harness::new(Scenario::TransientError).await;
    harness.run(browser, binding).await;
    let view = harness.view().await;
    assert_eq!(view.phase, "failed");
    assert!(!view.failure.unwrap().contains("must-not-echo"));
    let fixture = harness.fixture.lock().unwrap();
    assert_eq!((fixture.oauth, fixture.observations), (1, 5));
    assert_eq!(fixture.browser_closed, 1);
}

#[tokio::test]
async fn missing_provider_metadata_is_unconfirmed_not_a_human_login_pause() {
    let (harness, browser, binding) = Harness::new(Scenario::Unconfirmed).await;
    harness.run(browser, binding).await;
    let view = harness.view().await;
    assert_eq!(view.phase, "failed");
    assert!(view.failure.unwrap().contains("fresh, matching login time"));
    assert!(!view.awaiting_user);
    assert_eq!(harness.fixture.lock().unwrap().browser_closed, 1);
}

#[tokio::test]
async fn a_page_close_race_still_shuts_the_flow_owned_browser() {
    // A concurrent close of one owned page is a race, not a reason to leave the
    // window behind: the browser was launched by this flow on an isolated
    // profile, so the terminal attempt closes it regardless.
    let (harness, browser, binding) = Harness::new(Scenario::ConcurrentPageClose).await;
    harness.run(browser, binding).await;
    assert_eq!(harness.view().await.phase, "succeeded");
    let fixture = harness.fixture.lock().unwrap();
    assert!(fixture.closed.contains("popup"));
    assert_eq!(fixture.browser_closed, 1);
}

#[tokio::test]
async fn an_unattended_login_pause_is_closed_and_settled() {
    // The reported hang: a sign-in pause that nobody returns to left the browser
    // open forever. The scheduler's reaper closes that window and records the
    // attempt as ended so the next tick does not reopen it.
    let (harness, browser, binding) = Harness::new(Scenario::Pause).await;
    harness.run(browser, binding).await;
    assert_eq!(harness.view().await.phase, "waiting_for_user");

    let expired =
        crate::checkin::scheduler::expire_login_pauses(&harness.desktop, 500, Duration::ZERO).await;
    assert_eq!(expired, 1);
    assert!(harness.runtime.purpose(&harness.key).await.is_none());
    let view = harness.view().await;
    assert_eq!(view.phase, "failed");
    assert!(view.failure.unwrap().contains("human-time limit"));
    assert_eq!(
        harness.desktop.checkin_schedule.last_settled_at("p", "a"),
        Some(500)
    );
    assert_eq!(
        harness.fixture.lock().unwrap().browser_closed,
        1,
        "the unattended pause closes its own browser"
    );
}
