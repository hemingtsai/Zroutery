//! Request-local reply metadata. Never serialized into history or ML input.

use axum::http::{HeaderMap, HeaderValue};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct ReplyMetadata {
    headers: HeaderMap,
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
        crate::sync::lock(&self.0).headers = kept;
    }

    pub fn apply(&self, headers: &mut HeaderMap) {
        for (name, value) in &self.snapshot() {
            headers.insert(name.clone(), value.clone());
        }
    }
}
