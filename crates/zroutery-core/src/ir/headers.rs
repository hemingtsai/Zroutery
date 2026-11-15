//! Request-local reply metadata. Never serialized into history or ML input.

use axum::http::{HeaderMap, HeaderValue};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct ReplyMetadata {
    headers: HeaderMap,
    /// Request-local only, never serialized into diagnostics or accounting.
    native_error: Option<(u16, serde_json::Value)>,
    session_id: Option<HeaderValue>,
    claude_session: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ReplyHeaders(Arc<Mutex<ReplyMetadata>>);

impl PartialEq for ReplyHeaders {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.snapshot() == other.snapshot()
    }
}

impl ReplyHeaders {
    fn snapshot(&self) -> HeaderMap {
        let metadata = crate::sync::lock(&self.0);
        let mut headers = metadata.headers.clone();
        if let Some(id) = &metadata.session_id {
            headers.insert("x-zroutery-session-id", id.clone());
            headers.insert("x-session-id", id.clone());
            headers.insert("session_id", id.clone());
            if metadata.claude_session {
                headers.insert("x-claude-code-session-id", id.clone());
            }
        }
        headers
    }

    pub(crate) fn clear_session_id(&self) {
        let mut metadata = crate::sync::lock(&self.0);
        metadata.session_id = None;
        metadata.claude_session = false;
    }

    pub(crate) fn set_session_id(&self, id: HeaderValue, claude: bool) {
        let mut metadata = crate::sync::lock(&self.0);
        metadata.session_id = Some(id);
        metadata.claude_session = claude;
    }

    pub(crate) fn session_id(&self) -> Option<String> {
        crate::sync::lock(&self.0)
            .session_id
            .as_ref()
            .and_then(|id| id.to_str().ok())
            .map(str::to_string)
    }

    pub fn capture(&self, headers: &HeaderMap) {
        let mut kept = HeaderMap::new();
        for (name, value) in headers {
            let name_text = name.as_str();
            let credential = matches!(
                name_text,
                "anthropic-api-key" | "anthropic-auth-token" | "anthropic-set-cookie"
            );
            if !credential
                && !crate::config::passthrough_denied(name_text)
                && (name_text.starts_with("anthropic-")
                    || matches!(name_text, "retry-after" | "x-should-retry" | "request-id"))
            {
                kept.append(name.clone(), value.clone());
            }
        }
        // A later attempt replaces metadata; stale headers from a failed
        // sibling must not survive into the final reply.
        let mut metadata = crate::sync::lock(&self.0);
        metadata.headers = kept;
        metadata.native_error = None;
    }

    pub(crate) fn clear_upstream_response(&self) {
        let mut metadata = crate::sync::lock(&self.0);
        metadata.headers.clear();
        metadata.native_error = None;
    }

    pub(crate) fn capture_native_error(&self, status: u16, body: &serde_json::Value) {
        if body.get("type").and_then(serde_json::Value::as_str) == Some("error")
            && body
                .pointer("/error/type")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && body
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && body.to_string().len() <= 64 * 1024
        {
            crate::sync::lock(&self.0).native_error = Some((status, body.clone()));
        }
    }

    pub(crate) fn native_error(&self, status: u16) -> Option<serde_json::Value> {
        crate::sync::lock(&self.0)
            .native_error
            .as_ref()
            .filter(|(upstream_status, _)| *upstream_status == status)
            .map(|(_, body)| body.clone())
    }

    pub fn apply(&self, headers: &mut HeaderMap) {
        let snapshot = self.snapshot();
        for name in snapshot.keys() {
            headers.remove(name);
            for value in snapshot.get_all(name) {
                headers.append(name.clone(), value.clone());
            }
        }
    }
}
