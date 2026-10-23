//! Reading an upstream refusal.

use std::time::Duration;

use serde_json::Value;

use crate::config::ProviderConfig;
use crate::error::Error;

/// The most same-candidate retries one request may spend, across every class.
///
/// The reference filter (alexcarterio/AgentRouter-Codex-Filter) allows up to 20
/// retries and a 30-minute budget because it stages the whole SSE response, so a
/// long wait is invisible to the client. Zroutery forwards a stream live and
/// blocks the request while it waits, so the same policy would hold a client
/// for half an hour. Six retries is enough to ride out a short batch grant or a
/// gateway flap without turning a stall into a hang.
const MAX_TOTAL_RETRIES: u32 = 6;
/// Waits for a rate limit: a token/request window is minutes, not milliseconds.
const RATE_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// Waits for a dropped handshake or a gateway blip.
const TRANSPORT_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
/// Waits for a relay with no available channel.
const AVAILABILITY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
];
/// Waits for a history item that belongs to another upstream resource.
const HISTORY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
/// No single wait exceeds this, even when the upstream asks for longer.
const MAX_WAIT: Duration = Duration::from_secs(120);

/// How long to wait before replaying the same candidate, or `None` when this
/// failure is not one a replay can fix.
///
/// The classes and limits come from the AgentRouter/CC Switch filter, which
/// field-tested them against the failures a multi-account relay produces:
///
/// * a **rate limit** (429, or a 400 carrying `rate_limit_exceeded`) is a
///   window that reopens, so it is retried with a long backoff;
/// * a **transport** failure or a 408/502/504 gateway blip happened before the
///   upstream processed anything;
/// * **no available channel** (503) is the relay failing over between accounts;
/// * a history item **created under a different resource** (Azure) can be
///   accepted once the relay's next account has the item.
///
/// Anything else — authentication, quota/budget exhaustion, a malformed
/// request, a deterministic content rejection — is returned immediately.
pub(crate) fn retry_advice(
    err: &Error,
    retries_done: u32,
    can_fail_over: bool,
    transport_limit: u32,
) -> Option<Duration> {
    if retries_done >= MAX_TOTAL_RETRIES {
        return None;
    }
    // A candidate with a sibling behind it is failed over, not waited on: the
    // next upstream answers sooner than any backoff, and a rate limit on one
    // account says nothing about the next. Waiting is the last resort, when
    // this candidate is the only one left.
    if can_fail_over {
        return None;
    }
    let (delays, class_limit, floor, transport): (&[Duration], u32, Duration, bool) = match err {
        Error::Upstream {
            status: 429,
            retry_after,
            ..
        } => (&RATE_DELAYS, 4, retry_after.unwrap_or_default(), false),
        Error::Upstream {
            status,
            body,
            retry_after,
            ..
        } if *status == 400 && is_rate_limit_body(body) => {
            (&RATE_DELAYS, 4, retry_after.unwrap_or_default(), false)
        }
        Error::Upstream {
            status: 503,
            body,
            retry_after,
            ..
        } if is_no_available_channel(body) => (
            &AVAILABILITY_DELAYS,
            3,
            retry_after.unwrap_or_default(),
            false,
        ),
        Error::Upstream {
            status: 400,
            body,
            retry_after,
            ..
        } if is_history_resource_error(body) => {
            (&HISTORY_DELAYS, 2, retry_after.unwrap_or_default(), false)
        }
        Error::Upstream {
            status: 408 | 502 | 503 | 504 | 529,
            retry_after,
            ..
        } => (
            &TRANSPORT_DELAYS,
            transport_limit,
            retry_after.unwrap_or_default(),
            true,
        ),
        Error::Transport { .. } | Error::Timeout(_) => {
            (&TRANSPORT_DELAYS, transport_limit, Duration::ZERO, true)
        }
        _ => return None,
    };
    // A caller that cannot protect a stream (the buffered path) hands a
    // transport failure to the router, which fails over at once.
    if transport && transport_limit == 0 {
        return None;
    }
    if retries_done >= class_limit {
        return None;
    }
    let base = delays[(retries_done as usize).min(delays.len() - 1)];
    Some(base.max(floor).min(MAX_WAIT))
}

/// A 400 that is really a rate limit (some relays use 400 for token windows).
fn is_rate_limit_body(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("rate_limit_exceeded")
        || lower.contains("exceeded token rate limit")
        || (lower.contains("rate limit exceeded")
            && (lower.contains("requests per minute") || lower.contains("token")))
}

/// A relay that could not find a channel for the request.
fn is_no_available_channel(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("no_available_channel")
        || lower.contains("no available channel")
        || body.contains("无可用渠道")
        || body.contains("没有可用渠道")
}

/// Azure's "this item belongs to another resource" refusal.
fn is_history_resource_error(body: &str) -> bool {
    body.contains("The requested item was created under a different")
        && body.contains("Use the same resource that created the item")
}

/// DEBUG: dump a request body whose upstream call failed, so relay
/// rejections can be replayed and bisected offline. Opt-in via
/// ZROUTERY_CAPTURE_FAILED_BODIES=1 — the dumps contain full conversation
/// content, so they are never written by default.
pub(super) fn capture_failed_body(
    provider: &ProviderConfig,
    body: &Value,
    status: u16,
    text: &str,
) {
    if std::env::var("ZROUTERY_CAPTURE_FAILED_BODIES").as_deref() != Ok("1") {
        return;
    }
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    if n >= 10 {
        return;
    }
    let dir = std::env::temp_dir().join("zroutery_failed_bodies");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let path = dir.join(format!(
        "{ts}_s{status}_{}.json",
        provider.name.replace(' ', "_")
    ));
    if let Ok(pretty) = serde_json::to_string_pretty(body) {
        let _ = std::fs::write(&path, pretty);
        let meta = format!("status: {status}\nresponse: {text}\n");
        let _ = std::fs::write(dir.join(format!("{ts}_s{status}_meta.txt")), meta);
        tracing::warn!("captured failed upstream body to {}", path.display());
    }
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(status: u16, body: &str, retry_after: Option<Duration>) -> Error {
        Error::Upstream {
            provider: "relay".into(),
            status,
            body: body.into(),
            retry_after,
        }
    }

    #[test]
    fn a_rate_limit_waits_for_the_window_and_honours_the_server_floor() {
        let limited = upstream(429, "rate limited", None);
        assert_eq!(
            retry_advice(&limited, 0, false, 0),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            retry_advice(&limited, 1, false, 0),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            retry_advice(&limited, 2, false, 0),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            retry_advice(&limited, 3, false, 0),
            Some(Duration::from_secs(60))
        );
        // Four retries is the class limit.
        assert_eq!(retry_advice(&limited, 4, false, 0), None);

        // The header is a floor, never a way to retry early.
        let asked = upstream(429, "", Some(Duration::from_secs(42)));
        assert_eq!(
            retry_advice(&asked, 0, false, 0),
            Some(Duration::from_secs(42))
        );
        // And a preposterous wait is capped.
        let huge = upstream(429, "", Some(Duration::from_secs(3600)));
        assert_eq!(retry_advice(&huge, 0, false, 0), Some(MAX_WAIT));
    }

    #[test]
    fn a_relay_that_reports_rate_limits_as_400_is_recognised() {
        let limited = upstream(400, r#"{"code":"rate_limit_exceeded"}"#, None);
        assert_eq!(
            retry_advice(&limited, 0, false, 0),
            Some(Duration::from_secs(5))
        );
        let tokens = upstream(400, "exceeded token rate limit", None);
        assert_eq!(
            retry_advice(&tokens, 0, false, 0),
            Some(Duration::from_secs(5))
        );
        // An ordinary 400 is not retried.
        let bad = upstream(400, "invalid model", None);
        assert_eq!(retry_advice(&bad, 0, false, 0), None);
    }

    #[test]
    fn gateway_blips_and_no_available_channel_are_retried_but_not_forever() {
        // The streaming handshake retries a 502; the buffered path hands it to
        // the failover instead.
        assert_eq!(
            retry_advice(&upstream(502, "", None), 0, false, 2),
            Some(Duration::from_secs(1))
        );
        assert_eq!(retry_advice(&upstream(502, "", None), 0, false, 0), None);
        assert_eq!(
            retry_advice(
                &upstream(503, r#"{"message":"无可用渠道"}"#, None),
                0,
                false,
                0
            ),
            Some(Duration::from_secs(5))
        );
        assert_eq!(retry_advice(&upstream(502, "", None), 3, false, 2), None);
    }

    #[test]
    fn a_candidate_with_a_sibling_behind_it_is_failed_over_not_waited_on() {
        for error in [
            upstream(429, "rate limited", Some(Duration::from_secs(30))),
            upstream(502, "", None),
            upstream(503, "no_available_channel", None),
            Error::Timeout(2),
        ] {
            assert_eq!(
                retry_advice(&error, 0, true, 2),
                None,
                "a fallback is faster than any backoff: {error}"
            );
        }
    }

    #[test]
    fn a_history_item_from_another_resource_is_retried_briefly() {
        let azure = upstream(
            400,
            "The requested item was created under a different Azure OpenAI resource. \
             Use the same resource that created the item to access it.",
            None,
        );
        assert_eq!(
            retry_advice(&azure, 0, false, 0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(retry_advice(&azure, 2, false, 0), None);
    }

    #[test]
    fn failures_a_replay_cannot_fix_are_returned_immediately() {
        for error in [
            upstream(401, "bad key", None),
            upstream(402, "no balance", None),
            upstream(500, "deterministic", None),
            Error::InvalidRequest("bad".into()),
            Error::Unauthorized,
        ] {
            assert_eq!(retry_advice(&error, 0, false, 0), None, "{error}");
        }
    }

    #[test]
    fn the_total_retry_budget_is_shared_across_classes() {
        // Six retries have been spent, whatever mix of classes produced them.
        assert_eq!(
            retry_advice(&upstream(429, "", None), MAX_TOTAL_RETRIES, false, 0),
            None
        );
    }
}
