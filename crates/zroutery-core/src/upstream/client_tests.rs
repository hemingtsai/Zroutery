//! The HTTP client's own contract, asserted against a real socket.
//!
//! Everything else in this module tests what the client is *given* — headers,
//! bodies, dialects. This file tests what it *does* on the wire, because the
//! three properties that matter here are invisible in a header map and are the
//! difference between a private API key and a leaked one:
//!
//! * no proxy unless the operator asked for one (`ZROUTERY_NO_PROXY`, the
//!   `bypass_proxy` setting), so a local relay is reached directly;
//! * no redirect following, because reqwest strips `Authorization` across
//!   hosts but not `x-api-key`, and that is where the Anthropic key lives;
//! * HTTP/1.1 only, which is the pool shape the desktop build was measured
//!   with and the one every configured relay is known to speak.
//!
//! The tests spawn a one-shot `TcpListener` rather than mock the transport, so
//! the bytes the client actually wrote are what is asserted. They depend on
//! nothing but the crate's default features: no `ml`, no `account`, no
//! `newapi`, which is what keeps `cargo test -p zroutery-core` honest
//! (E-115).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::config::{ProviderConfig, ProviderKind};

/// One OpenAI-compatible completion, which is what the client expects to
/// decode. The tests care about the request, but a request only counts if the
/// round trip finishes, so the answer has to be real.
const COMPLETION_BODY: &str = concat!(
    "{\"id\":\"chatcmpl-test\",\"object\":\"chat.completion\",\"created\":0,",
    "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"message\":",
    "{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":\"stop\"}],",
    "\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}"
);

/// A response with the framing a real server would send. `content-length` is
/// computed rather than written by hand: a wrong length would make the client
/// wait for bytes that never arrive, and the failure would look like a hang
/// rather than a bad fixture.
fn response(status_line: &str, headers: &str, body: &str) -> String {
    format!(
        "{status_line}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A provider pointed at a loopback port.
fn local_provider(port: u16) -> ProviderConfig {
    let mut provider = ProviderConfig::new("local", "Local", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://127.0.0.1:{port}");
    provider.timeout_secs = 5;
    provider.connect_timeout_secs = 5;
    provider
}

/// Serve exactly one request, hand its head and body to the test, and answer.
///
/// The body is read as well as the head: closing the socket after the head
/// makes the client's write fail, which would turn a header assertion into a
/// transport failure.
async fn one_shot(response: String) -> (u16, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        let mut buf = [0u8; 2048];
        let head_end = loop {
            let Ok(read) = socket.read(&mut buf).await else {
                return;
            };
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buf[..read]);
            if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while request.len() < head_end + length {
            let Ok(read) = socket.read(&mut buf).await else {
                return;
            };
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
        }
        let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });

    (port, rx)
}

/// Guard for the process-wide environment the client reads at build time.
///
/// `std::env` is global, so a test that sets a proxy variable has to hold this
/// for its whole life or a sibling test in the same binary could build a client
/// against the wrong environment.
///
/// It is a *tokio* mutex on purpose: the guard is held across an await (the
/// request itself), and a `std::sync::MutexGuard` held across an await is a
/// lint error rather than a style question — the guard would be held while the
/// runtime moves the task between threads.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard {
    keys: Vec<&'static str>,
    previous: Vec<Option<std::ffi::OsString>>,
}

impl EnvGuard {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        let keys: Vec<&'static str> = pairs.iter().map(|(key, _)| *key).collect();
        let previous = keys.iter().map(std::env::var_os).collect::<Vec<_>>();
        for (key, value) in pairs {
            std::env::set_var(key, value);
        }
        Self { keys, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, previous) in self.keys.iter().zip(self.previous.iter()) {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
async fn a_direct_client_speaks_http1_and_presents_every_configured_header() {
    let (port, captured) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        COMPLETION_BODY,
    ))
    .await;
    let mut provider = local_provider(port);
    provider
        .extra_headers
        .insert("x-zroutery-probe".into(), "through".into());

    // `bypass_proxy = true` is the explicit "do not use a proxy" switch.
    let upstream = super::Upstream::new(true, 5);
    let body = serde_json::json!({
        "model": "test-model",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        upstream.send(&provider, Some("sk-test"), &body),
    )
    .await
    .expect("the direct client must not hang")
    .expect("the round trip completes against a loopback server");
    assert_eq!(answer.model, "test-model");

    let request = captured.await.expect("the server saw the request");
    // HTTP/1.1 on the wire is what `http1_only()` buys: an HTTP/2 client would
    // open with the connection preface instead of a request line.
    assert!(
        request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "the client must speak HTTP/1.1 and reach the chat endpoint: {request:?}"
    );
    // Header passthrough on the direct path, including the one the proxy would
    // otherwise have had a chance to rewrite.
    assert!(
        request
            .to_ascii_lowercase()
            .contains("\r\nauthorization: bearer sk-test\r\n"),
        "the api key must reach the upstream intact: {request:?}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("\r\nx-zroutery-probe: through\r\n"),
        "configured extra headers must be forwarded: {request:?}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("\r\nuser-agent: {}\r\n", super::USER_AGENT).to_lowercase()),
        "the client's own user agent must be the one configured: {request:?}"
    );
    assert!(
        !request
            .to_ascii_lowercase()
            .contains("proxy-authorization:"),
        "a direct request must not carry proxy credentials: {request:?}"
    );
}

#[tokio::test]
async fn a_redirect_is_answered_as_a_refusal_not_followed() {
    const REDIRECT: &str = concat!(
        "HTTP/1.1 302 Found\r\n",
        // A destination that cannot be reached. Following it would surface as a
        // transport error; not following it surfaces as the 302 itself, which
        // is the distinction this test is about.
        "location: http://127.0.0.1:1/elsewhere\r\n",
        "content-length: 0\r\n",
        "connection: close\r\n",
        "\r\n"
    );
    let (port, captured) = one_shot(REDIRECT.to_string()).await;
    let provider = local_provider(port);
    let upstream = super::Upstream::new(true, 5);

    let error = upstream
        .send(
            &provider,
            Some("sk-secret"),
            &serde_json::json!({ "model": "m" }),
        )
        .await
        .expect_err("a 3xx is a refusal, never a silent second request");

    match error {
        crate::error::Error::Upstream { status, body, .. } => {
            assert_eq!(
                status, 302,
                "the caller must be told the upstream answered 302 so it can decide"
            );
            assert!(body.is_empty() || !body.contains("sk-secret"));
        }
        other => panic!("expected the 302 itself, got {other:?}"),
    }

    let request = captured
        .await
        .expect("the redirecting server saw one request");
    assert!(
        request.contains("/v1/chat/completions"),
        "the first request is the only request: {request:?}"
    );
}

#[tokio::test]
async fn codex_profile_overrides_the_default_ua_on_the_actual_wire() {
    let (port, captured) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        COMPLETION_BODY,
    ))
    .await;
    let mut provider = local_provider(port);
    provider.client_profile = crate::config::ProviderClientProfile::Codex;
    let mut inbound = axum::http::HeaderMap::new();
    inbound.insert("user-agent", "opencode/test".parse().unwrap());
    let identity = crate::config::resolve_client_identity(provider.client_profile, &inbound);
    super::Upstream::new(true, 5).send_with_identity(
        &provider, Some("sk-fixture"),
        &serde_json::json!({"model": "test-model", "messages": [{"role": "user", "content": "hi"}]}),
        &identity,
    ).await.unwrap();
    let request = captured.await.unwrap().to_ascii_lowercase();
    assert!(request.contains("\r\nuser-agent: codex_cli_rs/0.1.0\r\n"));
    assert!(request.contains("\r\noriginator: codex_cli_rs\r\n"));
    assert!(!request.contains("\r\nuser-agent: zroutery/"));
    assert!(!request.contains("\r\nuser-agent: opencode/"));
}

fn with_proxy(inherited_bypass: bool, port: u16) -> super::Upstream {
    super::Upstream {
        clients: std::sync::RwLock::new(super::http::HttpClients::with_test_proxy(
            inherited_bypass,
            &format!("http://127.0.0.1:{port}"),
        )),
        connect_timeout_secs: 5,
    }
}

#[tokio::test]
async fn only_the_opted_in_decision_provider_uses_the_proxy() {
    let (proxy, proxied) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        r#"{"answers":{"decision":{"type":"noul","noul":0.03}}}"#,
    ))
    .await;
    let upstream = with_proxy(true, proxy);
    let mut jev = ProviderConfig::new("jev", "Jev fixture", ProviderKind::Decision);
    jev.base_url = "http://proxy-only.invalid".into();
    jev.bypass_proxy = Some(false);
    jev.extra_headers
        .insert("x-opencode-client".into(), "zroutery".into());
    let body = super::build_decision_body(
        "non-executable fixture",
        "Is it dangerous?",
        "jev-1.13-free",
    );
    let answer = upstream.send_decision(&jev, None, &body).await.unwrap();
    assert_eq!(answer.noul, 0.03);
    let request = proxied.await.unwrap();
    assert!(
        request.starts_with("POST http://proxy-only.invalid/v1/systemone HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(request
        .to_ascii_lowercase()
        .contains("\r\nx-opencode-client: zroutery\r\n"));
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
    assert!(!request.contains("opencode/latest"));

    let (port, direct) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        COMPLETION_BODY,
    ))
    .await;
    let sibling = local_provider(port);
    assert_eq!(sibling.bypass_proxy, None);
    upstream
        .send(
            &sibling,
            Some("sk-sibling-fixture"),
            &serde_json::json!({"model":"m"}),
        )
        .await
        .unwrap();
    let request = direct.await.unwrap();
    assert!(
        request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(request
        .to_ascii_lowercase()
        .contains("\r\nauthorization: bearer sk-sibling-fixture\r\n"));
}

#[tokio::test]
async fn an_explicit_direct_provider_bypasses_a_proxied_gateway_default() {
    let upstream = with_proxy(false, 1);
    let (port, direct) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        COMPLETION_BODY,
    ))
    .await;
    let mut provider = local_provider(port);
    provider.bypass_proxy = Some(true);
    upstream
        .send(&provider, None, &serde_json::json!({"model":"m"}))
        .await
        .unwrap();
    assert!(direct
        .await
        .unwrap()
        .starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
}

#[tokio::test]
async fn a_proxied_decision_refusal_is_not_retried_directly() {
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (proxy, captured) = one_shot(response(
        "HTTP/1.1 451 Unavailable For Legal Reasons",
        "content-type: application/json\r\n",
        r#"{"error":{"type":"server_error","message":"Endpoint is unavailable."}}"#,
    ))
    .await;
    let upstream = with_proxy(true, proxy);
    let mut provider = ProviderConfig::new("jev", "Jev fixture", ProviderKind::Decision);
    provider.base_url = format!("http://{}", direct.local_addr().unwrap());
    provider.bypass_proxy = Some(false);
    let error = upstream
        .send_decision(
            &provider,
            None,
            &super::build_decision_body("fixture", "Dangerous?", "jev-1.13-free"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status().as_u16(), 451);
    assert_eq!(super::retry_advice(&error, 0, false, 2), None);
    assert!(captured
        .await
        .unwrap()
        .starts_with("POST http://127.0.0.1:"));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), direct.accept())
            .await
            .is_err(),
        "a proxy refusal must never trigger a direct retry"
    );
}

/// The environment variable is a second way to ask for the same direct path,
/// because a packaged desktop build cannot pass `bypass_proxy` before the first
/// client exists.
///
/// The test sets a proxy that is not listening. On a platform whose system
/// proxy honours `HTTP_PROXY`, a client that did not honour the bypass would
/// fail to connect; here the request has to succeed *and* arrive with its
/// headers, which is the property the operator set the variable for.
#[tokio::test]
async fn the_no_proxy_environment_switch_keeps_the_request_direct() {
    let _lock = ENV.lock().await;
    let _env = EnvGuard::set(&[
        ("ZROUTERY_NO_PROXY", "1"),
        ("HTTP_PROXY", "http://127.0.0.1:1"),
        ("http_proxy", "http://127.0.0.1:1"),
    ]);

    let (port, captured) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        COMPLETION_BODY,
    ))
    .await;
    let mut provider = local_provider(port);
    provider.bypass_proxy = Some(false);
    // `bypass_proxy = false`: the variable is the only thing asking for direct.
    let upstream = super::Upstream::new(false, 5);

    tokio::time::timeout(
        Duration::from_secs(10),
        upstream.send(
            &provider,
            Some("sk-env"),
            &serde_json::json!({ "model": "m" }),
        ),
    )
    .await
    .expect("a direct client must not wait on a proxy that is not there")
    .expect("the direct path reaches the loopback server");

    let request = captured.await.expect("the server saw the request");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("\r\nauthorization: bearer sk-env\r\n"),
        "ZROUTERY_NO_PROXY must not cost the request its headers: {request:?}"
    );
}
