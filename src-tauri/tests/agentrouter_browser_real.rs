//! Local, headed-browser acceptance for the AgentRouter executor. No real
//! account, GitHub consent, upstream cookie or public-site mutation is involved.
#![cfg(feature = "account-maint")]

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use zroutery_lib::checkin::agentrouter::{self, Attempt, Probe, Settlement};
use zroutery_lib::checkin::cdp::{self, BrowserSession, PageBinding};
use zroutery_lib::checkin::state::{Event, State};
use zroutery_lib::checkin::{CheckinRuntime, FinishOutcome, SessionKey};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let root = std::env::var_os("TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = root
        .join("opencode")
        .join(format!("zr-agentrouter-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    Scratch(path)
}

async fn fixture_browser(dir: &Scratch) -> (BrowserSession, PageBinding) {
    let executable = cdp::find_browser("")
        .filter(|path| path.is_file())
        .expect("this explicitly invoked acceptance test needs Chromium");
    let mut browser = cdp::launch(&executable, &dir.0, "about:blank")
        .await
        .unwrap();
    let page = browser
        .open_page("data:text/html,%3Cmain%3Efixture%3C/main%3E")
        .await
        .unwrap();
    for _ in 0..40 {
        if browser
            .evaluate(&page, "!!document.querySelector('main')")
            .await
            .ok()
            == Some(json!(true))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    (browser, page)
}

/// Execute the unmodified production expression in Chromium with lexical test
/// doubles for the panel's origin/storage/HTTP. All network is fake; this tests
/// browser JavaScript and the CDP promise/serialization boundary, not a live grant.
fn fixture_expression(script: &str, checked_in: bool, timestamp: i64) -> String {
    format!(
        r#"(async () => {{
        const location = {{ origin: 'https://agentrouter.org' }};
        const user = {{ id: 7, checked_in: {checked_in}, last_login_time: {timestamp}, password: 'stay-in-page' }};
        const storage = new Map([['user', JSON.stringify(user)]]);
        const localStorage = {{ getItem: k => storage.get(k) || null,
            setItem: (k, v) => storage.set(k, v), removeItem: k => storage.delete(k) }};
        const fetch = async () => ({{ ok: true, status: 200,
            json: async () => ({{ success: true, data: {{ id: 7, last_login_time: {timestamp},
                access_token: 'stay-in-page', quota: 50000000 }} }}) }});
        return await {script};
    }})()"#
    )
}

#[tokio::test]
#[ignore = "local CDP fixture; opens a visible isolated Chromium window"]
async fn the_actual_page_probe_rejects_old_login_and_never_returns_secrets() {
    let dir = scratch();
    let (mut browser, page) = fixture_browser(&dir).await;
    let mut attempt = Attempt::new(100, "test:1".into(), "7".into());
    attempt.prepared = true;
    attempt.oauth_started = true;
    let script = agentrouter::observe_expression(&attempt);
    let checked = browser
        .evaluate(&page, &fixture_expression(&script, true, 101))
        .await
        .unwrap();
    assert_eq!(
        agentrouter::parse(&checked, &attempt),
        Probe::SignedIn(Settlement {
            checked_in: true,
            quota_after: Some(50_000_000),
            log_quota: None,
            log_at: None,
        })
    );
    let raw = checked.as_str().unwrap();
    // Only an amount may leave the page (observe.js); the fake secret markers
    // and every other response field must stay in it.
    for forbidden in ["password", "access_token", "stay-in-page"] {
        assert!(!raw.contains(forbidden));
    }
    let old = browser
        .evaluate(&page, &fixture_expression(&script, true, 99))
        .await
        .unwrap();
    assert_eq!(agentrouter::parse(&old, &attempt), Probe::Pending);
    let repeated = browser
        .evaluate(&page, &fixture_expression(&script, false, 101))
        .await
        .unwrap();
    assert_eq!(
        agentrouter::parse(&repeated, &attempt),
        Probe::SignedIn(Settlement {
            checked_in: false,
            quota_after: Some(50_000_000),
            log_quota: None,
            log_at: None,
        })
    );
    browser.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "local CDP fixture; opens a visible isolated Chromium window"]
async fn a_paused_browser_keeps_the_owned_popup_and_honours_cancel() {
    let dir = scratch();
    let executable = cdp::find_browser("")
        .filter(|path| path.is_file())
        .expect("Chromium required");
    let mut browser = cdp::launch(&executable, &dir.0, "about:blank")
        .await
        .unwrap();
    let page = browser.open_page("about:blank").await.unwrap();
    let marker = browser
        .evaluate_user_action(
            &page,
            r#"(() => {
        window.marker = 'original';
        window.child = window.open('about:blank', '_blank');
        window.child.marker = 'popup';
        return true;
    })()"#,
        )
        .await
        .unwrap();
    assert_eq!(marker, json!(true));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let targets = browser.target_infos().await.unwrap();
    let popup = targets
        .iter()
        .find(|info| info.get("openerId").and_then(|v| v.as_str()) == Some(&page.target_id))
        .and_then(|info| info.get("targetId"))
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let popup_page = browser.bind_page(popup).await.unwrap();
    let runtime = CheckinRuntime::new(dir.0.clone());
    let key = SessionKey::new("fixture", "main");
    runtime.begin(key.clone()).await.unwrap();
    for event in [Event::Start, Event::BrowserStarted, Event::Navigated] {
        runtime.advance(&key, event).await.unwrap();
    }
    runtime.attach(&key, browser).await.unwrap();
    let browser = runtime.take_browser(&key).await.unwrap();
    runtime.advance(&key, Event::WafBlocked).await.unwrap();
    runtime.advance(&key, Event::AwaitingUser).await.unwrap();
    assert!(runtime.park_browser(&key, browser).await.is_ok());
    runtime.advance(&key, Event::UserResumed).await.unwrap();
    let mut browser = runtime.take_browser(&key).await.unwrap();
    assert_eq!(
        runtime
            .advance(&key, Event::OperationRunning)
            .await
            .unwrap(),
        State::Running
    );
    assert_eq!(
        browser
            .evaluate(&popup_page, "window.marker")
            .await
            .unwrap(),
        json!("popup")
    );
    assert!(runtime.is_being_driven(&key).await);
    assert_eq!(runtime.finish(&key).await, FinishOutcome::DeferredToDriver);
    let returned = runtime
        .park_browser(&key, browser)
        .await
        .expect_err("cancel refuses to park");
    returned.shutdown().await.unwrap();
    runtime.end_driving(&key).await;
    runtime.finish(&key).await;
}

#[tokio::test]
#[ignore = "local CDP fixture; opens a visible isolated Chromium window"]
async fn closing_checkin_pages_does_not_close_an_unrelated_tab() {
    let dir = scratch();
    let executable = cdp::find_browser("")
        .filter(|path| path.is_file())
        .expect("Chromium required");
    let mut browser = cdp::launch(&executable, &dir.0, "about:blank")
        .await
        .unwrap();
    let login = browser.open_page("about:blank").await.unwrap();
    let target = browser
        .call("Target.createTarget", json!({"url":"about:blank"}))
        .await
        .unwrap();
    let unrelated = target["targetId"].as_str().unwrap().to_string();
    let page = browser.bind_page(unrelated.clone()).await.unwrap();
    browser
        .evaluate(&page, "window.unrelatedMarker = 42")
        .await
        .unwrap();
    assert!(browser.close_owned_pages(&[login.target_id]).await.unwrap());
    assert_eq!(
        browser
            .evaluate(&page, "window.unrelatedMarker")
            .await
            .unwrap(),
        json!(42)
    );
    let targets = browser.target_infos().await.unwrap();
    assert_eq!(targets.iter().filter(|t| t["type"] == "page").count(), 1);
    assert!(browser.is_running().await);
    // This explicit test owns both tabs and may close its own browser.
    browser.shutdown().await.unwrap();
}
