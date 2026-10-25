//! Integration-style unit tests for the three-state circuit breaker.

use zroutery_core::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

fn breaker(failure_threshold: u32) -> CircuitBreaker {
    CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold,
        success_threshold: 2,
        timeout_secs: 0,
        error_rate_threshold: 0.6,
        min_requests: 10,
    })
}

#[test]
fn closed_opens_after_consecutive_failures() {
    let b = breaker(2);
    assert_eq!(b.state(), CircuitState::Closed);

    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Closed);

    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
    assert!(b.consecutive_failures() == 0);
}

#[test]
fn closed_opens_when_error_rate_exceeds_threshold() {
    let b = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 100, // disable consecutive-failure opening for this test
        success_threshold: 2,
        timeout_secs: 0,
        error_rate_threshold: 0.6,
        min_requests: 10,
    });

    // 5 failures + 4 successes => 9 requests, 5 failed (55%).
    for _ in 0..5 {
        assert!(b.allow_request());
        b.record_failure();
    }
    for _ in 0..4 {
        assert!(b.allow_request());
        b.record_success();
    }
    assert_eq!(b.state(), CircuitState::Closed);

    // The 10th request is a failure, pushing the rate to 6/10 (60%).
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
}

#[test]
fn open_transitions_to_half_open_after_timeout() {
    let b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);

    // timeout_secs is 0, so the very next allow_request is the half-open probe.
    assert!(b.allow_request());
    assert_eq!(b.state(), CircuitState::HalfOpen);
}

#[test]
fn half_open_closes_after_success_threshold() {
    let b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);

    assert!(b.allow_request());
    assert_eq!(b.state(), CircuitState::HalfOpen);
    b.record_success();
    assert_eq!(b.state(), CircuitState::HalfOpen);

    // The first success did not meet the threshold yet, so the permit must be
    // released before the next real request can probe again.
    assert!(
        b.allow_request(),
        "half-open permit should be released after a sub-threshold success"
    );
    b.record_success();
    assert_eq!(b.state(), CircuitState::Closed);
}

#[test]
fn half_open_opens_immediately_on_probe_failure() {
    let b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);

    assert!(b.allow_request());
    assert_eq!(b.state(), CircuitState::HalfOpen);
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
}

#[test]
fn half_open_allows_only_one_concurrent_probe() {
    let b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);

    assert!(b.allow_request());
    assert!(
        !b.allow_request(),
        "the second half-open probe must be rejected"
    );

    b.release_half_open_permit();
    assert!(
        b.allow_request(),
        "releasing the permit lets the next probe through"
    );
}

#[test]
fn release_half_open_permit_does_not_count_health() {
    let b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert!(b.allow_request());
    let total_before = b.total_requests();
    let failures_before = b.failed_requests();

    b.release_half_open_permit();

    assert_eq!(b.total_requests(), total_before);
    assert_eq!(b.failed_requests(), failures_before);
    assert_eq!(b.consecutive_successes(), 0);
    assert_eq!(b.state(), CircuitState::HalfOpen);
}

#[test]
fn set_config_swaps_tunables_and_preserves_state_and_counts() {
    let mut b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
    let failures = b.failed_requests();

    let mut next = b.config().clone();
    next.failure_threshold = 100;
    next.timeout_secs = 30;
    assert!(b.set_config(next.clone()), "a changed config is applied");
    assert_eq!(b.config(), &next);

    // The tripped state and the evidence behind it survive the swap: saving
    // new thresholds must not reset the breaker behind the operator's back.
    assert_eq!(b.state(), CircuitState::Open);
    assert_eq!(b.failed_requests(), failures);
    assert!(!b.set_config(next), "an identical config is a no-op");
}

#[test]
fn reset_returns_to_closed_and_clears_metrics() {
    let b = breaker(1);
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);

    b.reset();
    assert_eq!(b.state(), CircuitState::Closed);
    assert_eq!(b.consecutive_failures(), 0);
    assert_eq!(b.total_requests(), 0);
    assert_eq!(b.failed_requests(), 0);
    assert!(b.allow_request());
}

#[test]
fn closed_opens_at_exact_error_rate_threshold() {
    // With error_rate_threshold=0.5 and min_requests=10,
    // exactly 5/10 failures should open the breaker.
    let b = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 100,
        success_threshold: 2,
        timeout_secs: 0,
        error_rate_threshold: 0.5,
        min_requests: 10,
    });
    for _ in 0..5 {
        assert!(b.allow_request());
        b.record_failure();
    }
    for _ in 0..4 {
        assert!(b.allow_request());
        b.record_success();
    }
    assert_eq!(b.state(), CircuitState::Closed);
    // 10th request: 5/10 = exactly 0.5, which should trigger (>= threshold).
    assert!(b.allow_request());
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
}

#[test]
fn record_failure_on_open_is_noop() {
    let b = breaker(1);
    b.allow_request();
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
    let failures_before = b.consecutive_failures();
    b.record_failure(); // should be no-op on consecutive_failures
    assert_eq!(b.consecutive_failures(), failures_before);
    assert_eq!(b.state(), CircuitState::Open);
}

#[test]
fn record_success_on_open_is_noop() {
    let b = breaker(1);
    b.allow_request();
    b.record_failure();
    assert_eq!(b.state(), CircuitState::Open);
    b.record_success(); // should be no-op
    assert_eq!(b.state(), CircuitState::Open);
}

/// The half-open probe is elected, not raced: many threads arriving at an open
/// breaker once its cooldown has elapsed must produce exactly one probe.
///
/// This is the property the routing layer relies on when it fans a request out
/// across candidates: `allow_request` is called from several request tasks at
/// once, and two probes would be two pieces of evidence from one recovery
/// attempt — the "only one probe" promise would hold on a quiet system and
/// quietly stop holding under exactly the load that opens breakers.
///
/// The run repeats because the failure is an interleaving: one pass can miss
/// it, sixteen passes with a barrier make it overwhelmingly likely to appear if
/// the permit has stopped being atomic.
#[test]
fn a_half_open_probe_is_elected_once_under_concurrent_callers() {
    use std::sync::{Arc, Barrier};

    const CALLERS: usize = 16;
    const ROUNDS: usize = 16;

    for round in 0..ROUNDS {
        let breaker = Arc::new(breaker(1));
        assert!(breaker.allow_request());
        breaker.record_failure();
        assert_eq!(
            breaker.state(),
            CircuitState::Open,
            "round {round}: the breaker must start open"
        );
        let requests_before = breaker.total_requests();

        let barrier = Arc::new(Barrier::new(CALLERS));
        let mut callers = Vec::with_capacity(CALLERS);
        for _ in 0..CALLERS {
            let breaker = Arc::clone(&breaker);
            let barrier = Arc::clone(&barrier);
            callers.push(std::thread::spawn(move || {
                // All threads arrive together, so the window where the state
                // flips and the permit is taken is actually contended.
                barrier.wait();
                breaker.allow_request()
            }));
        }
        let allowed = callers
            .into_iter()
            .map(|caller| caller.join().expect("no probe thread panicked"))
            .filter(|allowed| *allowed)
            .count();

        assert_eq!(
            allowed, 1,
            "round {round}: exactly one caller may hold the half-open probe, got {allowed}"
        );
        assert_eq!(
            breaker.state(),
            CircuitState::HalfOpen,
            "round {round}: a probe is in flight, so the breaker stays half open"
        );
        assert_eq!(
            breaker.total_requests(),
            requests_before + 1,
            "round {round}: only the elected probe may be counted as a request"
        );
    }
}
