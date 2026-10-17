//! Reading an upstream refusal.

use serde_json::Value;

use crate::config::ProviderConfig;
use crate::error::Error;
/// Whether a handshake error is worth retrying: failures that happen before
/// the provider processed anything (connection problems, TLS issues) or
/// gateway-level blips (408/502/503/504).
///
/// Deliberately excluded: 405 (both known modes — TLS-fingerprint and
/// content rules on the relay's WAF — are deterministic, so replaying just
/// re-uploads the body for nothing), 429 (retrying deepens relay rate-limit
/// blocks) and 500 (this relay reports deterministic content rejections as
/// 500, and replaying those just burns quota).
pub(super) fn is_transient_handshake_failure(err: &Error) -> bool {
    match err {
        Error::Transport { .. } => true,
        Error::Upstream { status, .. } => matches!(status, 408 | 502 | 503 | 504),
        _ => false,
    }
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
