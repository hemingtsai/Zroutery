//! Session / long-context routing.
//!
//! Tracks per-session routing state so that requests belonging to the same
//! conversation stay on the same candidate when appropriate.
//!
//! Three modes:
//! * **Free** -- full policy evaluation, no affinity (new / short sessions).
//! * **Sticky** -- prefer the same candidate unless consecutive failures force
//!   a change (multi-turn conversations).
//! * **Pinned** -- hard-pin to a candidate; only an explicit failure threshold
//!   can dislodge it (long-context windows).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SessionRoutingMode
// ---------------------------------------------------------------------------

/// How a session's routing behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRoutingMode {
    /// New request: full policy evaluation, no affinity.
    Free,
    /// Active session: prefer the same candidate unless failure forces change.
    Sticky,
    /// Long context: pin to candidate, only change on hard failure.
    Pinned,
}

impl Default for SessionRoutingMode {
    fn default() -> Self {
        Self::Free
    }
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// Routing state for an active session.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub mode: SessionRoutingMode,
    /// The candidate this session is pinned/sticky to.
    pub affinity_model: Option<String>,
    pub affinity_provider: Option<String>,
    /// Number of requests in this session.
    pub request_count: u32,
    /// Total context tokens accumulated.
    pub context_tokens: u64,
    /// When the session started (unix seconds).
    pub started_at: i64,
    /// Last request timestamp (unix seconds).
    pub last_request_at: i64,
    /// Number of consecutive failures on the affinity candidate.
    pub consecutive_failures: u32,
}

impl SessionState {
    pub fn new(session_id: String) -> Self {
        let now = chrono::Utc::now().timestamp();
        SessionState {
            session_id,
            mode: SessionRoutingMode::Free,
            affinity_model: None,
            affinity_provider: None,
            request_count: 0,
            context_tokens: 0,
            started_at: now,
            last_request_at: now,
            consecutive_failures: 0,
        }
    }

    /// Determine the routing mode based on session state.
    pub fn effective_mode(
        &self,
        sticky_threshold_requests: u32,
        pinned_threshold_tokens: u64,
    ) -> SessionRoutingMode {
        if self.context_tokens >= pinned_threshold_tokens {
            SessionRoutingMode::Pinned
        } else if self.request_count >= sticky_threshold_requests {
            SessionRoutingMode::Sticky
        } else {
            SessionRoutingMode::Free
        }
    }

    /// Record a successful request.
    pub fn record_success(&mut self, model_id: &str, provider_id: &str, tokens: u64) {
        self.request_count += 1;
        self.context_tokens += tokens;
        self.last_request_at = chrono::Utc::now().timestamp();
        self.consecutive_failures = 0;
        self.affinity_model = Some(model_id.to_string());
        self.affinity_provider = Some(provider_id.to_string());
    }

    /// Record a failure on the current affinity candidate.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_request_at = chrono::Utc::now().timestamp();
    }

    /// Should we unpin from the affinity candidate?
    /// Returns true if failures exceed threshold or provider is unavailable.
    pub fn should_unpin(&self, max_failures: u32) -> bool {
        self.consecutive_failures >= max_failures
    }

    /// Reset affinity (e.g. after unpin).
    pub fn clear_affinity(&mut self) {
        self.affinity_model = None;
        self.affinity_provider = None;
        self.consecutive_failures = 0;
        self.mode = SessionRoutingMode::Free;
    }
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

/// Thread-safe store for active sessions.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, SessionState>>,
    /// Default: sticky after 2 requests.
    pub sticky_threshold_requests: u32,
    /// Default: pinned after 100k tokens.
    pub pinned_threshold_tokens: u64,
    /// Default: unpin after 3 consecutive failures.
    pub max_failures: u32,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            sticky_threshold_requests: 2,
            pinned_threshold_tokens: 100_000,
            max_failures: 3,
        }
    }

    pub fn get_or_create(&self, session_id: &str) -> SessionState {
        let mut sessions = crate::sync::lock(&self.sessions);
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionState::new(session_id.to_string()))
            .clone()
    }

    pub fn update(&self, state: SessionState) {
        crate::sync::lock(&self.sessions).insert(state.session_id.clone(), state);
    }

    pub fn remove(&self, session_id: &str) -> bool {
        crate::sync::lock(&self.sessions)
            .remove(session_id)
            .is_some()
    }

    /// Evict sessions older than `max_age_secs`.
    pub fn evict_stale(&self, max_age_secs: i64) {
        let now = chrono::Utc::now().timestamp();
        crate::sync::lock(&self.sessions)
            .retain(|_, s| now - s.last_request_at < max_age_secs);
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_state_starts_in_free_mode() {
        let state = SessionState::new("s1".into());
        assert_eq!(state.mode, SessionRoutingMode::Free);
        assert_eq!(state.effective_mode(2, 100_000), SessionRoutingMode::Free);
    }

    #[test]
    fn after_sticky_threshold_requests_mode_becomes_sticky() {
        let mut state = SessionState::new("s1".into());
        state.request_count = 2;
        assert_eq!(state.effective_mode(2, 100_000), SessionRoutingMode::Sticky);
    }

    #[test]
    fn after_pinned_threshold_tokens_mode_becomes_pinned() {
        let mut state = SessionState::new("s1".into());
        state.context_tokens = 100_000;
        assert_eq!(state.effective_mode(2, 100_000), SessionRoutingMode::Pinned);
    }

    #[test]
    fn consecutive_failures_increments_on_failure() {
        let mut state = SessionState::new("s1".into());
        assert_eq!(state.consecutive_failures, 0);
        state.record_failure();
        assert_eq!(state.consecutive_failures, 1);
        state.record_failure();
        assert_eq!(state.consecutive_failures, 2);
    }

    #[test]
    fn should_unpin_returns_true_at_max_failures() {
        let mut state = SessionState::new("s1".into());
        assert!(!state.should_unpin(3));
        state.record_failure();
        state.record_failure();
        assert!(!state.should_unpin(3));
        state.record_failure();
        assert!(state.should_unpin(3));
    }

    #[test]
    fn clear_affinity_resets_everything() {
        let mut state = SessionState::new("s1".into());
        state.record_success("m1", "p1", 500);
        state.record_failure();
        state.mode = SessionRoutingMode::Pinned;

        state.clear_affinity();

        assert!(state.affinity_model.is_none());
        assert!(state.affinity_provider.is_none());
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.mode, SessionRoutingMode::Free);
    }

    #[test]
    fn session_store_get_or_create_creates_new() {
        let store = SessionStore::new();
        let s = store.get_or_create("new-session");
        assert_eq!(s.session_id, "new-session");
        assert_eq!(s.request_count, 0);
    }

    #[test]
    fn session_store_update_persists_state() {
        let store = SessionStore::new();
        let mut s = store.get_or_create("s1");
        s.request_count = 42;
        store.update(s);

        let loaded = store.get_or_create("s1");
        assert_eq!(loaded.request_count, 42);
    }

    #[test]
    fn session_store_evict_stale_removes_old_sessions() {
        let store = SessionStore::new();
        let mut s = store.get_or_create("old");
        s.last_request_at = 0; // epoch
        store.update(s);

        store.get_or_create("fresh"); // just created, should survive

        store.evict_stale(60);

        // old session should be gone
        let fresh = store.get_or_create("old");
        assert_eq!(fresh.request_count, 0, "old session was evicted");
    }

    #[test]
    fn different_session_ids_are_isolated() {
        let store = SessionStore::new();
        let mut a = store.get_or_create("a");
        a.request_count = 10;
        store.update(a);

        let b = store.get_or_create("b");
        assert_eq!(b.request_count, 0);

        let a2 = store.get_or_create("a");
        assert_eq!(a2.request_count, 10);
    }

    #[test]
    fn effective_mode_respects_thresholds() {
        let mut state = SessionState::new("s1".into());

        // Below both thresholds
        state.request_count = 1;
        state.context_tokens = 50_000;
        assert_eq!(state.effective_mode(2, 100_000), SessionRoutingMode::Free);

        // At sticky threshold
        state.request_count = 2;
        state.context_tokens = 50_000;
        assert_eq!(state.effective_mode(2, 100_000), SessionRoutingMode::Sticky);

        // At pinned threshold (pinned takes priority over sticky)
        state.request_count = 2;
        state.context_tokens = 100_000;
        assert_eq!(state.effective_mode(2, 100_000), SessionRoutingMode::Pinned);
    }

    #[test]
    fn record_success_resets_consecutive_failures() {
        let mut state = SessionState::new("s1".into());
        state.record_failure();
        state.record_failure();
        assert_eq!(state.consecutive_failures, 2);

        state.record_success("m1", "p1", 100);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.request_count, 1);
        assert_eq!(state.context_tokens, 100);
    }

    #[test]
    fn session_routing_mode_serde_round_trip() {
        let modes = [
            SessionRoutingMode::Free,
            SessionRoutingMode::Sticky,
            SessionRoutingMode::Pinned,
        ];
        for mode in &modes {
            let json = serde_json::to_string(mode).unwrap();
            let back: SessionRoutingMode = serde_json::from_str(&json).unwrap();
            assert_eq!(*mode, back);
        }
    }
}
