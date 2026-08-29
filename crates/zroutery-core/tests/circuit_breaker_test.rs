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
    assert!(!b.allow_request(), "the second half-open probe must be rejected");

    b.release_half_open_permit();
    assert!(b.allow_request(), "releasing the permit lets the next probe through");
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
