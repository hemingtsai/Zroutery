//! The browser check-in path, verified against a real headed browser.
//!
//! # These are ignored by default, and that is the correct default
//!
//! Everything in `src-tauri/src/checkin/` that can be decided without a browser
//! is decided without one: the state machine, the profile isolation and the
//! challenge classification are all covered by ordinary unit tests that CI runs.
//! What those cannot cover is the part that touches the outside world — that
//! Chrome accepts the argument list, writes a `DevToolsActivePort` file, serves a
//! WebSocket on it, and answers a page-scoped `Runtime.evaluate`.
//!
//! That is what this file is for, and it is `#[ignore]`d so that CI does not
//! fail for want of a browser. Run it deliberately:
//!
//! ```sh
//! cargo test -p zroutery --features account-maint --test checkin_browser_real \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! **A visible window will open.** That is not incidental — the browser is
//! launched without `--headless`, and asserting on a headless launch would prove
//! nothing about the headed mode this operation depends on.
//!
//! # Why the pages are `data:` URLs and not a local server
//!
//! The first version of this file served the test pages from `127.0.0.1` and
//! every navigation test failed: a launched browser inherits the machine's proxy
//! configuration, so the request went to a proxy that has no route back to
//! loopback, and the page never loaded. That is a fact about browsers worth
//! knowing rather than working around — a panel reachable only through a proxy is
//! a real case — but it is not what these tests are about. A `data:` URL removes
//! networking entirely and leaves only the protocol round trip under test.

#![cfg(feature = "account-maint")]

use std::time::Duration;

use zroutery_lib::checkin::cdp::{self, BrowserSession};
use zroutery_lib::checkin::profile;
use zroutery_lib::checkin::state::{self, Event, State};
use zroutery_lib::checkin::waf;
use zroutery_lib::checkin::{CheckinRuntime, SessionKey};

/// Skip rather than fail when no browser is installed.
///
/// CI has no Chromium, and a missing browser is a different fact from a broken
/// browser. Reporting the first as the second would train everyone to ignore it.
fn browser() -> Option<std::path::PathBuf> {
    match cdp::find_browser("") {
        Some(path) if path.is_file() => Some(path),
        _ => {
            eprintln!("skipping: no Chromium browser found on this machine");
            None
        }
    }
}

/// A scratch profile directory that removes itself.
///
/// A guard rather than a cleanup call at the end of each test: a test that fails or
/// panics before its last line leaves a *real browser profile* behind, which is the
/// expensive kind of leftover — tens of megabytes each, and a directory that looks
/// like a user's data.
struct Scratch(std::path::PathBuf);

/// Derefs to the path so a scratch directory can be passed anywhere a path is
/// wanted without every call site unwrapping it.
impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("zroutery-real-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    Scratch(dir)
}

async fn launch(executable: &std::path::Path, dir: &std::path::Path) -> BrowserSession {
    cdp::launch(executable, dir, "about:blank")
        .await
        .unwrap_or_else(|e| panic!("a headed browser must launch: {e}"))
}

/// A page the browser can render with no network involved.
///
/// Percent-encoded rather than pasted raw. A raw `data:` URL is silently
/// truncated at the first character the URL parser claims — a space, a quote, a
/// `#` — and the symptom is an element that quietly fails to exist, which is a
/// miserable thing to debug. Encoding makes the fixture unambiguous.
///
/// Kept free of `#` and `"` handling by encoding everything outside
/// `A-Za-z0-9` and the few punctuation characters that carry no meaning here.
fn page_encoded(html: &str) -> String {
    let mut out = String::from("data:text/html;charset=utf-8,");
    for byte in html.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b':' | b',' | b'=' | b'.' | b'(' | b')')
        {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// A page the browser can render with no network involved.
fn page(title: &str, body: &str) -> String {
    page_encoded(&format!(
        "<title>{title}</title><main id=root>{body}</main>"
    ))
}

/// Poll until the page is showing `url`, or give up.
async fn wait_for(
    session: &mut BrowserSession,
    page: &cdp::PageBinding,
    url_prefix: &str,
) -> waf::PageEvidence {
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let probe = session.page_evidence(page).await.expect("page readable");
        if probe.url.starts_with(url_prefix) {
            return probe;
        }
    }
    panic!("the page must load");
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_headed_browser_reports_a_debugging_endpoint() {
    let Some(executable) = browser() else { return };
    let dir = scratch("endpoint");
    let session = launch(&executable, &dir.0).await;

    // The endpoint file is the only thing the launch depends on, and it exists
    // precisely because the browser chose its own port.
    let port_file = dir.join(profile::DEVTOOLS_PORT_FILE);
    assert!(port_file.is_file(), "the browser wrote its debugging port");
    let text = std::fs::read_to_string(&port_file).expect("readable");
    assert!(text.lines().next().unwrap().parse::<u16>().is_ok());

    // No `--headless` anywhere: a headless launch would prove nothing about the
    // mode this operation needs.
    let args = cdp::build_args(&dir, "about:blank");
    assert!(
        !args.iter().any(|a| a.contains("headless")),
        "the operation requires a visible browser: {args:?}"
    );

    assert!(
        session.is_running().await,
        "the browser is serving its port"
    );
    let _ = session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_navigated_page_can_be_read_back() {
    let Some(executable) = browser() else { return };
    let dir = scratch("navigate");
    let mut session = launch(&executable, &dir.0).await;

    let url = page("Zroutery check-in", "Sign in");
    let binding = session.open_page(&url).await.expect("opens a page");
    assert!(
        !binding.session_id.is_empty(),
        "attaching yields a session id"
    );
    assert!(!binding.target_id.is_empty());

    let evidence = wait_for(&mut session, &binding, "data:").await;
    assert_eq!(evidence.title, "Zroutery check-in");
    assert!(
        evidence.body_text.contains("Sign in"),
        "body: {:?}",
        evidence.body_text
    );
    assert!(
        evidence.application_dom_loaded,
        "the application's own document is present, so the page is not a challenge"
    );
    // And the classifier agrees, on evidence read out of a real browser rather
    // than a fixture.
    assert_eq!(waf::classify(&evidence), waf::Verdict::Clear);

    let _ = session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens two visible windows"]
async fn two_accounts_get_separate_browsers_and_separate_sessions() {
    let Some(executable) = browser() else { return };
    let a = scratch("iso-a");
    let b = scratch("iso-b");

    let mut first = launch(&executable, &a).await;
    let mut second = launch(&executable, &b).await;

    // Distinct profiles on disk, which is the isolation that matters.
    assert_ne!(first.profile_dir(), second.profile_dir());

    // The concrete form of "never share a session between accounts": a cookie
    // written into one account's profile must not be readable from another's.
    // Reading it back from a *third* browser would prove nothing, since two empty
    // jars are trivially equal — so the cookie is written first and looked for
    // second.
    first
        .call(
            "Storage.setCookies",
            serde_json::json!({
                "cookies": [{
                    "name": "zroutery_probe", "value": "first-only",
                    "domain": "127.0.0.1", "path": "/"
                }]
            }),
        )
        .await
        .expect("writes a cookie into the first profile");

    let jar = second
        .call("Storage.getCookies", serde_json::json!({}))
        .await
        .expect("reads the second profile's cookies");
    let names: Vec<String> = cookie_names(&jar);
    assert!(
        !names.iter().any(|n| n == "zroutery_probe"),
        "a cookie written into one account's profile appeared in another's: {names:?}"
    );

    // And it is present in its own, so the check above is not passing because
    // nothing was written at all.
    let own = first
        .call("Storage.getCookies", serde_json::json!({}))
        .await
        .expect("reads the first profile's cookies");
    assert_eq!(
        cookie_names(&own),
        vec!["zroutery_probe".to_string()],
        "the probe cookie must exist in the profile it was written to"
    );

    let _ = first.shutdown().await;
    let _ = second.shutdown().await;
}

fn cookie_names(jar: &serde_json::Value) -> Vec<String> {
    jar.get("cookies")
        .and_then(|c| c.as_array())
        .map(|cookies| {
            cookies
                .iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_browser_held_through_a_pause_is_the_same_browser() {
    let Some(executable) = browser() else { return };
    let dir = scratch("resume");
    let mut session = launch(&executable, &dir.0).await;

    // A page that rewrites itself, so a second look can tell whether it is the
    // same document or a freshly loaded one.
    let url = page_encoded(
        "<title>held</title><main id=root>before</main>\
         <script>setTimeout(function(){document.getElementById('root')\
         .textContent='after'},400)</script>",
    );
    let binding = session.open_page(&url).await.expect("opens a page");
    wait_for(&mut session, &binding, "data:").await;

    // The state machine says a pause keeps the browser, and it does: the same
    // page is still there, with whatever it has since done to itself. A resume
    // that launched a second browser would not be looking at this document.
    assert!(state::transition(State::Running, Event::WafBlocked).is_ok());
    assert!(state::transition(State::WaitingForUser, Event::UserResumed).is_ok());
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let after = session
        .page_evidence(&binding)
        .await
        .expect("still readable");
    assert!(
        after.url.starts_with("data:"),
        "the same page is still loaded after the pause: {}",
        after.url
    );
    assert_eq!(after.title, "held");
    assert!(
        after.body_text.contains("after"),
        "the held document ran its own script: {:?}",
        after.body_text
    );
    assert!(
        session.is_running().await,
        "a pause must leave the browser alive, not merely open"
    );

    let _ = session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_clicked_control_really_runs_the_pages_own_handler() {
    let Some(executable) = browser() else { return };
    let dir = scratch("click");
    let mut session = launch(&executable, &dir.0).await;

    // The button reports whether the event it received was *trusted*, and nothing
    // but the page's own handler writes that. This is the whole claim under test:
    // `Input.dispatchMouseEvent` goes through Chrome's real input pipeline, so the
    // page sees `isTrusted === true`, whereas `element.click()` would deliver an
    // untrusted event a challenge vendor can tell apart from a person pressing.
    let url = page_encoded(
        "<title>click</title><main id=root>ready</main>\
         <button id=go onclick=\"this.textContent=event.isTrusted\">Check in</button>",
    );
    let page = session.open_page(&url).await.expect("opens a page");
    wait_for(&mut session, &page, "data:").await;

    assert_eq!(
        session.click(&page, "#go").await.expect("clicks"),
        zroutery_lib::checkin::cdp::ClickOutcome::Clicked {
            text: "Check in".into()
        },
        "the control should be found and activated"
    );

    // Let the handler run.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let evidence = session.page_evidence(&page).await.expect("readable");
    assert!(
        evidence.body_text.contains("true"),
        "the page's own handler must have run and seen a trusted event: {:?}",
        evidence.body_text
    );
    assert!(
        !evidence.body_text.contains("false"),
        "an untrusted event is exactly what a challenge vendor can distinguish: {:?}",
        evidence.body_text
    );
    // And the page is no longer a challenge — a click cannot have worked at all
    // without getting here first.
    assert_eq!(waf::classify(&evidence), waf::Verdict::Clear);

    let _ = session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_disabled_control_is_reported_rather_than_clicked() {
    let Some(executable) = browser() else { return };
    let dir = scratch("disabled");
    let mut session = launch(&executable, &dir.0).await;

    // The shape of "already checked in today": the control is there and carries
    // its label, but it cannot be activated. Reporting this as a dispatched click
    // would leave an operator waiting for a reward that never arrives.
    let url = page_encoded(
        "<title>done</title><main id=root>ready</main><button id=go disabled>Checked in</button>",
    );
    let page = session.open_page(&url).await.expect("opens a page");
    wait_for(&mut session, &page, "data:").await;

    let outcome = session.click(&page, "#go").await.expect("probes");
    assert!(
        matches!(
            outcome,
            zroutery_lib::checkin::cdp::ClickOutcome::Disabled { .. }
        ),
        "a disabled control must not report as clicked: {outcome:?}"
    );
    assert!(!outcome.dispatched());

    let _ = session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_selector_that_matches_nothing_is_reported_not_silently_ok() {
    let Some(executable) = browser() else { return };
    let dir = scratch("missing");
    let mut session = launch(&executable, &dir.0).await;

    let url = page("no control", "nothing here");
    let page = session.open_page(&url).await.expect("opens a page");
    wait_for(&mut session, &page, "data:").await;

    let outcome = session
        .click(&page, "#does-not-exist")
        .await
        .expect("probes");
    assert_eq!(
        outcome,
        zroutery_lib::checkin::cdp::ClickOutcome::NotFound,
        "an unmatched selector is a configuration problem and must say so"
    );
    assert!(!outcome.dispatched());

    let _ = session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn shutdown_closes_the_browser_and_leaves_no_process() {
    let Some(executable) = browser() else { return };
    let dir = scratch("shutdown");
    let session = launch(&executable, &dir.0).await;
    assert!(session.is_running().await);

    session.shutdown().await.expect("shuts down cleanly");

    // A second launch on the same profile proves no browser survived: one that did
    // would have left the profile locked, and the relaunch would fail or attach
    // to the old one.
    let second = launch(&executable, &dir.0).await;
    assert!(second.is_running().await);
    let _ = second.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_second_driver_is_refused_the_same_browser() {
    // The only rule here that cannot be proven without a browser, because it turns
    // on what `take_browser` hands back when a browser really exists. Two drive
    // loops on one browser would each click the check-in control, and a panel that
    // credits a reward for one click would be reporting two.
    let Some(executable) = browser() else { return };
    let dir = scratch("two-drivers");
    let key = SessionKey::new("relay", "main");
    let runtime = CheckinRuntime::new(dir.0.parent().expect("scratch parent").to_path_buf());

    runtime.begin(key.clone()).await.expect("begins");
    let session = launch(&executable, runtime.profile_dir(&key).as_path()).await;
    runtime.attach(&key, session).await.expect("attaches");

    assert!(!runtime.is_being_driven(&key).await);
    let taken = runtime
        .take_browser(&key)
        .await
        .expect("first driver takes it");
    assert!(
        runtime.is_being_driven(&key).await,
        "the driver now owns the browser, and the account must say so"
    );
    assert!(
        runtime.take_browser(&key).await.is_none(),
        "a second driver must be refused the browser rather than handed the same one"
    );
    // And the session must not claim to hold a browser it has already given away.
    assert!(
        !runtime.is_browser_held(&key).await,
        "the browser is with the driver, not with the session"
    );

    assert!(!runtime.end_driving(&key).await, "no cancel was requested");
    // The refused second driver must not have disturbed the first one's browser.
    let outcome = taken.shutdown().await;
    assert!(
        outcome.is_ok(),
        "the first driver's browser closes cleanly: {outcome:?}"
    );
    runtime.finish(&key).await;
    let _ = std::fs::remove_dir_all(dir.0.parent().expect("scratch parent"));
}

#[tokio::test]
#[ignore = "needs a real Chromium browser and opens a visible window"]
async fn a_finished_attempt_releases_the_account_so_the_next_one_can_start() {
    // The other half of the report: after a check-in ends, the account must be
    // startable again and no browser may be left behind. Both were false at once —
    // the session stayed registered and the browser stayed open — so the next
    // attempt was either refused or, once the key was freed, launched a second
    // browser onto a profile the first still held.
    let Some(executable) = browser() else { return };
    let dir = scratch("release");
    let key = SessionKey::new("relay", "main");
    let runtime = CheckinRuntime::new(dir.0.parent().expect("scratch parent").to_path_buf());

    runtime.begin(key.clone()).await.expect("begins");
    let session = launch(&executable, runtime.profile_dir(&key).as_path()).await;
    runtime.attach(&key, session).await.expect("attaches");

    // Finishing with nothing driving closes the browser here and now.
    assert_eq!(
        runtime.finish(&key).await,
        zroutery_lib::checkin::FinishOutcome::ClosedNow
    );
    assert!(
        runtime.phase(&key).await.is_none(),
        "the key is free, so the next attempt is not a duplicate"
    );
    assert!(!runtime.is_browser_held(&key).await);

    // And the account really is startable again, which is the claim the corrupted
    // profile report turned on: a free key must mean no browser is still out there.
    runtime.begin(key.clone()).await.expect("starts again");
    let reopened = launch(&executable, runtime.profile_dir(&key).as_path()).await;
    assert!(
        runtime.attach(&key, reopened).await.is_ok(),
        "the same profile reopens once the previous browser is gone"
    );
    runtime.finish(&key).await;
    let _ = std::fs::remove_dir_all(dir.0.parent().expect("scratch parent"));
}
