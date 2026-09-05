//! In-memory response store for the OpenAI Responses API lifecycle.
//!
//! Completed responses are kept in a bounded ring buffer keyed by id,
//! supporting GET, DELETE, and multi-turn `previous_response_id` lookups.
//! In-flight responses carry an abort handle for cancellation.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use crate::ir::Usage;
use crate::policy::RouteDecision;

/// Lifecycle status of a stored response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// A stored response, matching the OpenAI Responses API wire format.
///
/// NOTE: This is an in-memory replay snapshot, not a durable replay artifact.
/// The [`ResponseStore`] has bounded capacity and evicts oldest entries.
/// For durable replay, [`RouteDecision`](crate::policy::RouteDecision) should
/// be persisted separately.  The `routing_decision` field captures the decision
/// inputs and outputs for diagnostic replay, but policy revisions may change
/// between restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    pub id: String,
    pub status: ResponseStatus,
    pub model: String,
    pub created_at: i64,
    /// The original input items (messages, function calls, etc.) as the client
    /// sent them, stored verbatim for `GET /input_items`.
    pub input: Vec<Value>,
    /// Output items in Responses API format.
    pub output: Vec<Value>,
    pub usage: Option<Usage>,
    /// Error details when status is Failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// Set when a previous_response_id was used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// Routing decision trace (for diagnostics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_decision: Option<RouteDecision>,
}

impl StoredResponse {
    /// Create a completed response from a pipeline result.
    pub fn completed(
        id: String,
        model: String,
        input: Vec<Value>,
        output: Vec<Value>,
        usage: Usage,
        previous_response_id: Option<String>,
        routing_decision: Option<RouteDecision>,
    ) -> Self {
        StoredResponse {
            id,
            status: ResponseStatus::Completed,
            model,
            created_at: chrono::Utc::now().timestamp(),
            input,
            output,
            usage: Some(usage),
            error: None,
            previous_response_id,
            routing_decision,
        }
    }

    /// Create a failed response.
    pub fn failed(id: String, model: String, error: Value) -> Self {
        StoredResponse {
            id,
            status: ResponseStatus::Failed,
            model,
            created_at: chrono::Utc::now().timestamp(),
            input: Vec::new(),
            output: Vec::new(),
            usage: None,
            error: Some(error),
            previous_response_id: None,
            routing_decision: None,
        }
    }
}

/// Handle to an in-flight streaming response, used for cancellation.
pub struct InFlightResponse {
    /// Sending `true` signals cancellation.
    pub cancel_tx: watch::Sender<bool>,
    /// The response id.
    pub id: String,
}

/// Thread-safe response store with bounded capacity.
#[derive(Clone)]
pub struct ResponseStore {
    inner: Arc<ResponseStoreInner>,
}

struct ResponseStoreInner {
    /// Completed/failed/cancelled responses.
    responses: std::sync::Mutex<HashMap<String, StoredResponse>>,
    /// In-flight streaming responses.
    in_flight: std::sync::Mutex<HashMap<String, InFlightResponse>>,
    /// Maximum number of stored responses (oldest evicted on overflow).
    capacity: usize,
}

impl ResponseStore {
    pub fn new(capacity: usize) -> Self {
        ResponseStore {
            inner: Arc::new(ResponseStoreInner {
                responses: std::sync::Mutex::new(HashMap::new()),
                in_flight: std::sync::Mutex::new(HashMap::new()),
                capacity,
            }),
        }
    }

    /// Store a completed or failed response.
    pub fn put(&self, response: StoredResponse) {
        let mut map = crate::sync::lock(&self.inner.responses);
        if map.len() >= self.inner.capacity {
            // Evict the oldest entry (by created_at).
            if let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, v)| v.created_at)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest_key);
            }
        }
        map.insert(response.id.clone(), response);
    }

    /// Retrieve a stored response by id.
    pub fn get(&self, id: &str) -> Option<StoredResponse> {
        crate::sync::lock(&self.inner.responses).get(id).cloned()
    }

    /// Delete a stored response. Returns true if it existed.
    pub fn delete(&self, id: &str) -> bool {
        crate::sync::lock(&self.inner.responses).remove(id).is_some()
    }

    /// Register an in-flight response. Returns a cancel receiver.
    pub fn register_in_flight(&self, id: String) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        crate::sync::lock(&self.inner.in_flight).insert(
            id.clone(),
            InFlightResponse { cancel_tx: tx, id },
        );
        rx
    }

    /// Mark an in-flight response as completed and move it to the store.
    pub fn complete_in_flight(&self, id: &str) {
        crate::sync::lock(&self.inner.in_flight).remove(id);
    }

    /// Cancel an in-flight response. Returns true if it was found.
    pub fn cancel(&self, id: &str) -> bool {
        let mut map = crate::sync::lock(&self.inner.in_flight);
        if let Some(inflight) = map.remove(id) {
            let _ = inflight.cancel_tx.send(true);
            true
        } else {
            false
        }
    }

    /// Store a cancelled response placeholder. Used when cancellation is requested
    /// but the pipeline hasn't finished yet.
    pub fn mark_cancelled(&self, id: &str, model: String) {
        let resp = StoredResponse {
            id: id.to_string(),
            status: ResponseStatus::Cancelled,
            model,
            created_at: chrono::Utc::now().timestamp(),
            input: Vec::new(),
            output: Vec::new(),
            usage: None,
            error: None,
            previous_response_id: None,
            routing_decision: None,
        };
        self.put(resp);
    }

    /// Check if cancellation has been requested for an in-flight response.
    pub fn is_cancelled(&self, id: &str) -> bool {
        crate::sync::lock(&self.inner.in_flight)
            .get(id)
            .map(|h| *h.cancel_tx.borrow())
            .unwrap_or(false)
    }

    /// Number of stored responses.
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.inner.responses).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ResponseStore {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_response(id: &str) -> StoredResponse {
        StoredResponse::completed(
            id.to_string(),
            "gpt-4".to_string(),
            vec![json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]})],
            vec![json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]})],
            Usage { input_tokens: 5, output_tokens: 3, ..Usage::default() },
            None,
            None,
        )
    }

    #[test]
    fn put_and_get() {
        let store = ResponseStore::new(10);
        store.put(sample_response("r1"));
        assert!(store.get("r1").is_some());
        assert_eq!(store.get("r1").unwrap().id, "r1");
        assert!(store.get("r2").is_none());
    }

    #[test]
    fn delete() {
        let store = ResponseStore::new(10);
        store.put(sample_response("r1"));
        assert!(store.delete("r1"));
        assert!(store.get("r1").is_none());
        assert!(!store.delete("r1"));
    }

    #[test]
    fn eviction_at_capacity() {
        let store = ResponseStore::new(2);
        let mut r1 = sample_response("r1");
        r1.created_at = 1;
        let mut r2 = sample_response("r2");
        r2.created_at = 2;
        let mut r3 = sample_response("r3");
        r3.created_at = 3;
        store.put(r1);
        store.put(r2);
        store.put(r3);
        assert_eq!(store.len(), 2);
        assert!(store.get("r1").is_none(), "oldest should be evicted");
        assert!(store.get("r2").is_some());
        assert!(store.get("r3").is_some());
    }

    #[tokio::test]
    async fn cancel_in_flight() {
        let store = ResponseStore::new(10);
        let mut rx = store.register_in_flight("r1".to_string());
        assert!(!store.is_cancelled("r1"));
        assert!(store.cancel("r1"));
        assert!(rx.changed().await.is_ok());
        assert!(*rx.borrow());
    }

    #[test]
    fn complete_removes_from_in_flight() {
        let store = ResponseStore::new(10);
        let _rx = store.register_in_flight("r1".to_string());
        store.complete_in_flight("r1");
        assert!(!store.cancel("r1"), "should no longer be in-flight");
    }

    #[test]
    fn default_capacity() {
        let store = ResponseStore::default();
        assert_eq!(store.inner.capacity, 1000);
    }
}
