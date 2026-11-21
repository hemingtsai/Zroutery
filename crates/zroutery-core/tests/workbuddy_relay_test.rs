//! The WorkBuddy relay path, asserted against a real socket.
//!
//! A community plugin declares a provider whose base URL carries the relay's
//! own version segment (`/v2`). Three properties must survive the round trip:
//!
//! * the declared `/v2` base must not grow a second version segment — the
//!   request reaches `/v2/chat/completions`, not `/v2/v1/...`;
//! * the manifest's `extra_headers` (the official CLI identity) reach the
//!   upstream, and the credential is presented as `Bearer`;
//! * a completion still decodes — the conversation works end to end.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use zroutery_core::config::ProviderConfig;
use zroutery_core::upstream::Upstream;

const COMPLETION_BODY: &str = concat!(
    "{\"id\":\"chatcmpl-wb\",\"object\":\"chat.completion\",\"created\":0,",
    "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"message\":",
    "{\"role\":\"assistant\",\"content\":\"pong\"},\"finish_reason\":\"stop\"}],",
    "\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}"
);

fn response(status_line: &str, headers: &str, body: &str) -> String {
    format!(
        "{status_line}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Serve exactly one request and hand its head and body back.
async fn one_shot(reply: String) -> (u16, tokio::sync::oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 16384];
        let mut request = Vec::new();
        let head_end = loop {
            let Ok(read) = socket.read(&mut buf).await else {
                return;
            };
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buf[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let length = request
                    .split(|b| *b == b'\n')
                    .find_map(|line| {
                        let line = String::from_utf8_lossy(line);
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")?
                            .trim()
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break end + 4 + length;
                }
            }
        };
        while request.len() < head_end {
            let Ok(read) = socket.read(&mut buf).await else {
                return;
            };
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
        }
        let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
        let _ = socket.write_all(reply.as_bytes()).await;
        let _ = socket.flush().await;
    });
    (port, rx)
}

/// The provider shape the workbuddy plugin synthesizes: an OpenAI-compatible
/// relay with the manifest's `extra_headers` and the relay's own `/v2` base.
fn workbuddy_provider(port: u16) -> ProviderConfig {
    let mut provider = ProviderConfig::new(
        "wb",
        "WorkBuddy",
        zroutery_core::config::ProviderKind::OpenAICompatible,
    );
    provider.base_url = format!("http://127.0.0.1:{port}/v2");
    provider.timeout_secs = 5;
    provider.connect_timeout_secs = 5;
    provider.bearer_auth = true;
    provider.extra_headers = [
        (
            "user-agent",
            "WorkBuddy/5.11.3 WorkBuddy/5.11.3 CLI/2.176.0",
        ),
        ("accept", "application/json"),
        ("x-ide-version", "5.11.3"),
        ("x-client-version", "5.11.3"),
        ("x-client-platform", "windows"),
        ("x-client-product", "WorkBuddy"),
        ("x-requested-with", "XMLHttpRequest"),
        ("x-no-user-id", "true"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    provider
}

#[tokio::test]
async fn a_workbuddy_relay_reaches_v2_chat_with_the_declared_identity() {
    let (port, captured) = one_shot(response(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        COMPLETION_BODY,
    ))
    .await;
    let provider = workbuddy_provider(port);
    let upstream = Upstream::new(true, 5);
    let body = serde_json::json!({
        "model": "test-model",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        upstream.send(&provider, Some("wb-test-token"), &body),
    )
    .await
    .expect("the relay conversation must not hang")
    .expect("the round trip completes against the loopback relay");
    assert_eq!(answer.model, "test-model");

    let request = captured.await.expect("the relay saw the request");
    assert!(
        request.starts_with("POST /v2/chat/completions HTTP/1.1\r\n"),
        "the declared /v2 base must reach the relay's chat path unchanged: {request:?}"
    );
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.contains("\r\nauthorization: bearer wb-test-token\r\n"),
        "the access token must travel as a Bearer credential: {request:?}"
    );
    for (name, value) in [
        (
            "user-agent",
            "workbuddy/5.11.3 workbuddy/5.11.3 cli/2.176.0",
        ),
        ("accept", "application/json"),
        ("x-ide-version", "5.11.3"),
        ("x-client-version", "5.11.3"),
        ("x-client-platform", "windows"),
        ("x-client-product", "workbuddy"),
        ("x-requested-with", "xmlhttprequest"),
        ("x-no-user-id", "true"),
    ] {
        assert!(
            lower.contains(&format!("\r\n{name}: {value}\r\n")),
            "the manifest identity header {name} must reach the relay: {request:?}"
        );
    }
    assert!(
        request.contains("\"model\":\"test-model\""),
        "the conversation body must carry the model: {request:?}"
    );
}

#[test]
fn a_declared_v2_base_is_never_doubled() {
    let mut provider = ProviderConfig::new(
        "wb",
        "WorkBuddy",
        zroutery_core::config::ProviderKind::OpenAICompatible,
    );
    provider.base_url = "https://www.workbuddy.cn/v2".into();
    assert_eq!(
        provider.chat_url(),
        "https://www.workbuddy.cn/v2/chat/completions"
    );
    // The tolerance is version-segment wide and keeps the bare-host / /v1
    // behavior of every other relay.
    let mut bare = ProviderConfig::new(
        "p",
        "P",
        zroutery_core::config::ProviderKind::OpenAICompatible,
    );
    bare.base_url = "https://api.example.com".into();
    assert_eq!(
        bare.chat_url(),
        "https://api.example.com/v1/chat/completions"
    );
    let mut v1 = ProviderConfig::new(
        "p",
        "P",
        zroutery_core::config::ProviderKind::OpenAICompatible,
    );
    v1.base_url = "https://api.example.com/v1/".into();
    assert_eq!(v1.chat_url(), "https://api.example.com/v1/chat/completions");
}
