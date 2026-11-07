//! Explicit upstream SSE failures retain their business meaning across dialects.

use serde_json::{json, Value};
use zroutery_core::ir::Dialect;
use zroutery_core::protocol::{self, SseFrame};
use zroutery_core::{Error, FailureClass};

const DIALECTS: [Dialect; 3] = [
    Dialect::OpenAI,
    Dialect::Anthropic,
    Dialect::OpenAIResponses,
];

#[test]
fn error_codes_and_types_survive_a_generic_relay_message() {
    let cases = [
        (
            json!({"type":"new_api_error","code":"insufficient_user_quota"}),
            FailureClass::BalanceExhausted,
            402,
        ),
        (
            json!({"type":"new_api_error","code":"rate_limit_exceeded"}),
            FailureClass::RateLimit,
            429,
        ),
        (
            json!({"type":"rate_limit_error","code":null}),
            FailureClass::RateLimit,
            429,
        ),
        (
            json!({"type":"rate_limit_error","code":"relay_specific_code"}),
            FailureClass::RateLimit,
            429,
        ),
        (
            json!({"type":"authentication_error","code":"invalid_api_key"}),
            FailureClass::Authentication,
            401,
        ),
        (
            json!({"type":"authentication_error","code":"invalid_encrypted_content"}),
            FailureClass::Authentication,
            401,
        ),
        (
            json!({"type":"overloaded_error","code":null}),
            FailureClass::ProviderUnavailable,
            529,
        ),
        (
            json!({"type":"invalid_request_error","code":"tool_call_mismatch"}),
            FailureClass::InvalidRequest,
            400,
        ),
        (
            json!({"type":"new_api_error","code":null}),
            FailureClass::ProviderUnavailable,
            502,
        ),
    ];
    for dialect in DIALECTS {
        for (mut detail, class, status) in cases.clone() {
            // The message alone cannot distinguish any of these refusals.
            detail["message"] = json!("openai_error");
            let payload = json!({"error":detail});
            let error = protocol::stream_parser(dialect, "model")
                .push(&SseFrame {
                    event: Some("error".into()),
                    data: payload.to_string(),
                })
                .unwrap_err();
            assert!(
                matches!(error, Error::Upstream { .. }),
                "{dialect:?}: {error:?}"
            );
            assert_eq!(error.classified().class, class, "{dialect:?}: {error:?}");
            assert_eq!(error.status().as_u16(), status, "{dialect:?}: {error:?}");
            let Error::Upstream { body, .. } = &error else {
                unreachable!()
            };
            let retained: Value = serde_json::from_str(body).unwrap();
            assert_eq!(retained["error"], detail);

            // The staged HTTP-200 path and live parser share one classifier.
            let staged = Error::Upstream {
                provider: "relay".into(),
                status: 200,
                body: payload.to_string(),
                retry_after: None,
            };
            assert_eq!(staged.classified().class, class);
            assert_eq!(staged.classified().status, Some(status));
        }
    }
}

#[test]
fn wrapped_errors_keep_only_error_fields_and_never_echo_them_to_clients() {
    for dialect in DIALECTS {
        let error = protocol::stream_parser(dialect, "model")
            .push(&SseFrame {
                event: Some("error".into()),
                data: json!({
                    "error":{"error":{
                        "type":"new_api_error", "code":"insufficient_user_quota",
                        "message":"private-message", "credential":"private-credential"
                    }},
                    "request":{"messages":[{"content":"private-request"}]}
                })
                .to_string(),
            })
            .unwrap_err();
        assert_eq!(error.classified().class, FailureClass::BalanceExhausted);
        let Error::Upstream { body, .. } = &error else {
            panic!("expected upstream refusal: {error:?}");
        };
        assert!(body.contains("private-message"));
        assert!(!body.contains("private-credential"));
        assert!(!body.contains("private-request"));
        for client_dialect in DIALECTS {
            assert!(!error
                .to_wire(client_dialect)
                .to_string()
                .contains("private-"));
        }
    }
}

#[test]
fn error_envelopes_without_event_names_and_flat_error_events_are_recognized() {
    for dialect in DIALECTS {
        for frame in [
            SseFrame {
                event: None,
                data: json!({"error":{"type":"rate_limit_error","code":null}}).to_string(),
            },
            SseFrame {
                event: Some("error".into()),
                data: json!({"code":"rate_limit_exceeded","message":"openai_error"}).to_string(),
            },
        ] {
            let error = protocol::stream_parser(dialect, "model")
                .push(&frame)
                .unwrap_err();
            assert_eq!(
                error.classified().class,
                FailureClass::RateLimit,
                "{dialect:?}: {error:?}"
            );
        }
    }

    let error = protocol::stream_parser(Dialect::OpenAIResponses, "model")
        .push(&SseFrame {
            event: Some("response.failed".into()),
            data: json!({"type":"response.failed","response":{
                "error":{"type":"rate_limit_error","code":null,"message":"openai_error"}
            }})
            .to_string(),
        })
        .unwrap_err();
    assert_eq!(error.classified().class, FailureClass::RateLimit);
}

#[test]
fn null_error_and_error_text_in_an_answer_are_not_refusals() {
    let text = r#"{"error":{"code":"insufficient_user_quota"}}"#;
    let mut parser = protocol::stream_parser(Dialect::OpenAI, "model");
    let events = parser
        .push(&SseFrame {
            event: None,
            data: json!({"error":null,"choices":[{
                "index":0,"delta":{"content":text},"finish_reason":null
            }]})
            .to_string(),
        })
        .unwrap();
    assert!(events.iter().any(|event| matches!(event,
        zroutery_core::ir::StreamEvent::TextDelta { text: output, .. } if output == text)));
}

#[test]
fn genuinely_invalid_json_stays_a_protocol_failure() {
    for dialect in DIALECTS {
        let error = protocol::stream_parser(dialect, "model")
            .push(&SseFrame {
                event: Some("error".into()),
                data: "{invalid json".into(),
            })
            .unwrap_err();
        assert!(matches!(error, Error::BadUpstreamPayload(_)));
        assert_eq!(error.classified().class, FailureClass::Protocol);
    }
}

#[tokio::test]
async fn native_review_does_not_forward_an_unnamed_error_as_an_extension() {
    use axum::{body::Body, response::Response, routing::post, Router};
    use zroutery_core::config::{ProviderConfig, ProviderKind};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = Router::new().route(
        "/v1/messages",
        post(|| async {
            Response::builder()
                .header("content-type", "text/event-stream")
                .header("retry-after", "120")
                .body(Body::from(format!(
                    "data: {}\n\n",
                    json!({"error":{"type":"rate_limit_error","code":null,"message":"openai_error"}})
                )))
                .unwrap()
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
    provider.base_url = format!("http://{address}");
    let result = zroutery_core::upstream::Upstream::new(true, 5)
        .stream(
            &provider,
            None,
            &json!({"model":"model","stream":true,"safeguards":{}}),
            "model",
        )
        .await;
    task.abort();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("the error must be rejected before publishing native frames"),
    };
    assert_eq!(error.classified().class, FailureClass::RateLimit);
    assert_eq!(error.safe_message(), "upstream Relay returned 429");
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(120))
    );
}
