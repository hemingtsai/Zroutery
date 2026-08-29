//! Classic three-state circuit breaker (Closed → Open → HalfOpen).
//!
//! This replaces the old two-state Healthy/Cooling model. A breaker is kept per
//! exposed model id and is the single source of truth for routing decisions:
//! `Closed` allows traffic, `Open` rejects it until the timeout expires, and
//! `HalfOpen` admits exactly one probe to gather fresh evidence before deciding
//! whether to close or reopen.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Public state of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Tunables for one breaker.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures that flip `Closed → Open`.
    pub failure_threshold: u32,
    /// Consecutive successes in `HalfOpen` that flip `HalfOpen → Closed`.
    pub success_threshold: u32,
    /// Seconds after opening before `Open → HalfOpen` is allowed.
    pub timeout_secs: u64,
    /// Minimum error rate that opens the breaker once enough requests exist.
    pub error_rate_threshold: f64,
    /// Minimum number of requests before the error rate check applies.
    pub min_requests: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 4,
            success_threshold: 2,
            timeout_secs: 60,
            error_rate_threshold: 0.6,
            min_requests: 10,
        }
    }
}

impl CircuitBreakerConfig {
    /// Backwards-compatible constructor with the old `break_after_failures` and
    /// `cooldown_secs` names.
    pub fn new(failure_threshold: u32, timeout_secs: u64) -> Self {
        Self {
            failure_threshold,
            timeout_secs,
            ..Self::default()
        }
    }
}

/// Per-model health state machine.
#[derive(Debug)]
pub struct CircuitBreaker {
    state: Mutex<CircuitState>,
    config: CircuitBreakerConfig,
    consecutive_failures: AtomicU32,
    consecutive_successes: AtomicU32,
    total_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_opened_at: Mutex<Option<Instant>>,
    /// 1 means a half-open probe is available; 0 means one is in flight.
    half_open_permits: AtomicU32,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: Mutex::new(CircuitState::Closed),
            config,
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            total_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_opened_at: Mutex::new(None),
            half_open_permits: AtomicU32::new(1),
        }
    }

    pub fn state(&self) -> CircuitState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    pub fn consecutive_successes(&self) -> u32 {
        self.consecutive_successes.load(Ordering::Relaxed)
    }

    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    pub fn failed_requests(&self) -> u64 {
        self.failed_requests.load(Ordering::Relaxed)
    }

    /// Whether an open breaker's timeout has elapsed and it is ready to admit a
    /// half-open probe. Does not consume the probe permit.
    pub fn can_probe(&self) -> bool {
        if self.state() != CircuitState::Open {
            return false;
        }
        let opened = *self
            .last_opened_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        opened
            .map(|at| at.elapsed() >= Duration::from_secs(self.config.timeout_secs))
            .unwrap_or(false)
    }

    /// Seconds until an open breaker is allowed to transition to `HalfOpen`.
    pub fn open_remaining_secs(&self) -> u64 {
        if self.state() != CircuitState::Open {
            return 0;
        }
        let opened = *self
            .last_opened_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(opened_at) = opened else {
            return 0;
        };
        self.config
            .timeout_secs
            .saturating_sub(opened_at.elapsed().as_secs())
    }

    /// Whether a request may be attempted right now.
    ///
    /// `Closed` always allows. `Open` checks whether the cooldown timeout has
    /// elapsed; when it has, the breaker moves to `HalfOpen` and consumes the
    /// single probe permit. `HalfOpen` allows only when its one permit is free.
    pub fn allow_request(&self) -> bool {
        match self.state() {
            CircuitState::Closed => {
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                true
            }
            CircuitState::Open => {
                let opened = *self
                    .last_opened_at
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(opened_at) = opened else {
                    return false;
                };
                if opened_at.elapsed() < Duration::from_secs(self.config.timeout_secs) {
                    return false;
                }
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if *state != CircuitState::Open {
                        return *state == CircuitState::HalfOpen && self.take_half_open_permit();
                    }
                    *state = CircuitState::HalfOpen;
                }
                self.consecutive_successes.store(0, Ordering::Relaxed);
                self.half_open_permits.store(1, Ordering::Relaxed);
                self.take_half_open_permit()
            }
            CircuitState::HalfOpen => self.take_half_open_permit(),
        }
    }

    fn take_half_open_permit(&self) -> bool {
        let took = self
            .half_open_permits
            .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok();
        if took {
            self.total_requests.fetch_add(1, Ordering::Relaxed);
        }
        took
    }

    /// Record a successful request.
    ///
    /// In `Closed` this simply resets the failure streak. In `HalfOpen` it
    /// counts as fresh evidence and closes the breaker once the configured
    /// success threshold is reached.
    pub fn record_success(&self) {
        match self.state() {
            CircuitState::Closed => {
                self.consecutive_failures.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                let successes = self.consecutive_successes.fetch_add(1, Ordering::Relaxed) + 1;
                if successes >= self.config.success_threshold.max(1) {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if *state == CircuitState::HalfOpen {
                        *state = CircuitState::Closed;
                    }
                    self.consecutive_successes.store(0, Ordering::Relaxed);
                    self.consecutive_failures.store(0, Ordering::Relaxed);
                    self.half_open_permits.store(1, Ordering::Relaxed);
                }
            }
            CircuitState::Open => {}
        }
    }

    /// Record a failed request.
    ///
    /// In `Closed` this can open the breaker either by consecutive failures or
    /// by a sustained error rate (slow leaks). In `HalfOpen` any failure is
    /// enough to reopen immediately.
    pub fn record_failure(&self) {
        self.failed_requests.fetch_add(1, Ordering::Relaxed);

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            CircuitState::Closed => {
                let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                let total = self.total_requests.load(Ordering::Relaxed);
                let failed = self.failed_requests.load(Ordering::Relaxed);
                let error_rate_hit = total >= u64::from(self.config.min_requests)
                    && failed as f64 / total as f64 >= self.config.error_rate_threshold;
                if failures >= self.config.failure_threshold.max(1) || error_rate_hit {
                    self.open_locked(&mut state);
                }
            }
            CircuitState::HalfOpen => {
                self.open_locked(&mut state);
            }
            CircuitState::Open => {}
        }
    }

    fn open_locked(&self, state: &mut CircuitState) {
        *state = CircuitState::Open;
        *self
            .last_opened_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);
        self.half_open_permits.store(0, Ordering::Relaxed);
    }

    /// Return the half-open probe permit without recording any health outcome.
    ///
    /// This is used by rectifiers when a repaired request is retried on the
    /// same provider. The retry is not a fresh probe and must not consume the
    /// breaker's evidence budget.
    pub fn release_half_open_permit(&self) {
        if self.state() == CircuitState::HalfOpen {
            self.half_open_permits.store(1, Ordering::Relaxed);
        }
    }

    /// Full reset (GUI "retry now").
    pub fn reset(&self) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *state = CircuitState::Closed;
        }
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);
        self.total_requests.store(0, Ordering::Relaxed);
        self.failed_requests.store(0, Ordering::Relaxed);
        *self
            .last_opened_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.half_open_permits.store(1, Ordering::Relaxed);
    }
}
