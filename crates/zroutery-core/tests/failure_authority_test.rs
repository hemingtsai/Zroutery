use zroutery_core::{ClassifiedFailure, Error, FailureClass, FailureImpact};

fn assert_error_class(error: Error, expected: FailureClass) {
    let status = error.status().as_u16();
    let classified = ClassifiedFailure::from_core_error(&error);
    assert_eq!(classified.class, expected, "wrong class for {error:?}");
    assert_eq!(classified.status, Some(status));
    assert_eq!(classified.impact, expected.impact());
    assert!(classified.records_stats());
    assert!(!classified.is_success());
}

#[test]
fn public_constructor_compatibility_preserves_message_and_structural_apis() {
    // This is the original public call shape and must remain source-compatible.
    let legacy = ClassifiedFailure::from_error("connection refused".into());
    assert_eq!(legacy.class, FailureClass::Transport);
    assert_eq!(legacy.status, None);
    assert!(legacy.impact.retryable);

    // Structural errors use the explicitly named canonical constructor.
    let error = Error::Timeout(5);
    let structural = ClassifiedFailure::from_core_error(&error);
    assert_eq!(structural.class, FailureClass::Timeout);
    assert_eq!(structural.status, Some(error.status().as_u16()));
    assert!(structural.impact.fallbackable);
}

#[test]
fn generic_402_412_statuses_need_explicit_local_markers() {
    // Status-only upstream responses are provider rejections, not local
    // configuration or budget decisions.
    assert_eq!(
        FailureClass::from_status(402),
        FailureClass::ProviderRejected
    );
    assert_eq!(
        FailureClass::from_status(412),
        FailureClass::ProviderRejected
    );

    let payment = ClassifiedFailure::from_status(402, "payment required".to_string());
    assert_eq!(payment.class, FailureClass::ProviderRejected);
    assert!(payment.affects_observation());
    assert!(payment.affects_circuit());
    assert!(payment.provider_fault());
    assert!(!payment.retryable());
    assert!(!payment.fallbackable());

    let precondition = ClassifiedFailure::from_status(412, "precondition failed".to_string());
    assert_eq!(precondition.class, FailureClass::ProviderRejected);
    assert!(precondition.affects_observation());
    assert!(precondition.affects_circuit());
    assert!(precondition.provider_fault());
    assert!(!precondition.retryable());
    assert!(!precondition.fallbackable());

    // Explicit body markers are the only status-path route to local classes.
    assert_eq!(
        FailureClass::from_status_with_body(402, "budget exceeded"),
        FailureClass::OverBudget
    );
    let budget_marker = ClassifiedFailure::from_status(402, "budget exceeded".to_string());
    assert_eq!(budget_marker.class, FailureClass::OverBudget);
    assert!(!budget_marker.affects_observation());
    assert!(!budget_marker.affects_circuit());
    assert!(!budget_marker.provider_fault());
    assert!(!budget_marker.fallbackable());

    assert_eq!(
        FailureClass::from_status_with_body(412, "missing api key"),
        FailureClass::MissingApiKey
    );
    let key_marker = ClassifiedFailure::from_status(412, "missing api key".to_string());
    assert_eq!(key_marker.class, FailureClass::MissingApiKey);
    assert!(!key_marker.affects_observation());
    assert!(!key_marker.affects_circuit());
    assert!(!key_marker.provider_fault());
    assert!(key_marker.fallbackable());

    // Structural Error variants remain authoritative without status guessing.
    let structural_budget = ClassifiedFailure::from_core_error(Error::OverBudget("limit".into()));
    assert_eq!(structural_budget.class, FailureClass::OverBudget);
    assert!(!structural_budget.affects_observation());
    assert!(!structural_budget.fallbackable());

    let structural_key = ClassifiedFailure::from_core_error(Error::MissingApiKey("p".into()));
    assert_eq!(structural_key.class, FailureClass::MissingApiKey);
    assert!(!structural_key.affects_observation());
    assert!(structural_key.fallbackable());

    let generic_upstream = Error::Upstream {
        provider: "p".into(),
        status: 402,
        body: "payment required".into(),
    };
    assert!(!generic_upstream.is_retryable());
    assert!(generic_upstream.counts_against_health());
}

#[tokio::test]
async fn every_error_variant_has_one_canonical_classification() {
    // An invalid URL gives us a real reqwest::Error without making a network
    // request, so the exhaustive table still covers Error::Transport.
    let transport_source = reqwest::Client::new()
        .get("http://[::1")
        .send()
        .await
        .expect_err("invalid test URL must produce a transport error");

    let cases = [
        (
            Error::InvalidRequest("bad request".into()),
            FailureClass::InvalidRequest,
        ),
        (
            Error::TooLarge { limit_mib: 32 },
            FailureClass::InvalidRequest,
        ),
        (
            Error::UnknownModel("missing-model".into()),
            FailureClass::NoCandidate,
        ),
        (
            Error::UnknownRoute("GET /missing".into()),
            FailureClass::InvalidRequest,
        ),
        (
            Error::OverBudget("daily limit reached".into()),
            FailureClass::OverBudget,
        ),
        (
            Error::NoCandidate("standard".into()),
            FailureClass::NoCandidate,
        ),
        (Error::Unauthorized, FailureClass::Authentication),
        (
            Error::MissingApiKey("provider-a".into()),
            FailureClass::MissingApiKey,
        ),
        (
            Error::Upstream {
                provider: "provider-a".into(),
                status: 503,
                body: "temporarily unavailable".into(),
            },
            FailureClass::ProviderUnavailable,
        ),
        (
            Error::Transport {
                provider: "provider-a".into(),
                source: transport_source,
            },
            FailureClass::Transport,
        ),
        (
            Error::BadUpstreamPayload("response was not JSON".into()),
            FailureClass::Protocol,
        ),
        (Error::Timeout(30), FailureClass::Timeout),
        (
            Error::Internal("cannot read local configuration".into()),
            FailureClass::Configuration,
        ),
    ];

    for (error, expected) in cases {
        assert_error_class(error, expected);
    }
}

#[test]
fn one_impact_table_covers_routing_health_stats_and_fault_effects() {
    let expected = [
        (
            FailureClass::Transport,
            FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
        ),
        (
            FailureClass::Timeout,
            FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
        ),
        (
            FailureClass::RateLimit,
            FailureImpact {
                affects_observation: true,
                affects_circuit: false,
                retryable: true,
                fallbackable: true,
                provider_fault: false,
            },
        ),
        (
            FailureClass::Authentication,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::ProviderUnavailable,
            FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
        ),
        (
            FailureClass::ProviderRejected,
            FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: false,
                fallbackable: false,
                provider_fault: true,
            },
        ),
        (
            FailureClass::Protocol,
            FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: false,
                fallbackable: true,
                provider_fault: true,
            },
        ),
        (
            FailureClass::Capability,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: true,
                provider_fault: false,
            },
        ),
        (
            FailureClass::InvalidRequest,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::ClientCancelled,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::MissingApiKey,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: true,
                provider_fault: false,
            },
        ),
        (
            FailureClass::OverBudget,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::NoCandidate,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::Configuration,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::Interrupted,
            FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
        ),
        (
            FailureClass::Unknown,
            FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
        ),
    ];

    assert_eq!(FailureClass::ALL.len(), expected.len());
    for (class, impact) in expected {
        assert_eq!(class.impact(), impact, "wrong impact for {class:?}");
        assert!(class.records_stats());
        assert!(impact.records_stats());
        assert!(impact.affects_stats());
    }
}

#[test]
fn local_and_configuration_failures_cannot_poison_provider_health() {
    let local = [
        Error::MissingApiKey("provider-a".into()),
        Error::OverBudget("budget exhausted".into()),
        Error::NoCandidate("fast".into()),
        Error::UnknownModel("missing".into()),
        Error::InvalidRequest("invalid JSON".into()),
        Error::TooLarge { limit_mib: 1 },
        Error::UnknownRoute("GET /nope".into()),
        Error::Internal("invalid local configuration".into()),
    ];

    for error in local {
        let classified = error.classified();
        assert!(!classified.affects_observation(), "{classified:?}");
        assert!(!classified.affects_circuit(), "{classified:?}");
        assert!(!classified.provider_fault(), "{classified:?}");
        assert!(classified.records_stats(), "{classified:?}");
        assert!(!error.counts_against_health());
    }

    let missing_key = ClassifiedFailure::from_core_error(Error::MissingApiKey("p".into()));
    assert!(missing_key.fallbackable());
    assert!(!missing_key.retryable());

    let budget = ClassifiedFailure::from_core_error(Error::OverBudget("limit".into()));
    assert!(!budget.fallbackable());
    assert!(!budget.retryable());

    let no_candidate = ClassifiedFailure::from_core_error(Error::NoCandidate("model".into()));
    assert!(!no_candidate.fallbackable());
    assert!(!no_candidate.retryable());
}

#[test]
fn capability_authentication_transport_timeout_and_rate_limit_are_explicit() {
    let capability = ClassifiedFailure::from_core_error(&Error::Upstream {
        provider: "p".into(),
        status: 422,
        body: "model does not support vision".into(),
    });
    assert_eq!(capability.class, FailureClass::Capability);
    assert!(capability.fallbackable());
    assert!(!capability.retryable());
    assert!(!capability.affects_observation());
    assert!(!capability.affects_circuit());
    assert!(!capability.provider_fault());

    let authentication = ClassifiedFailure::from_core_error(&Error::Unauthorized);
    assert_eq!(authentication.class, FailureClass::Authentication);
    assert!(!authentication.fallbackable());
    assert!(!authentication.affects_observation());
    assert!(!authentication.affects_circuit());
    assert!(!authentication.provider_fault());

    let timeout = ClassifiedFailure::from_core_error(Error::Timeout(7));
    assert_eq!(timeout.class, FailureClass::Timeout);
    assert!(timeout.retryable());
    assert!(timeout.fallbackable());
    assert!(timeout.affects_observation());
    assert!(timeout.affects_circuit());
    assert!(timeout.provider_fault());

    let rate_limit = ClassifiedFailure::from_core_error(&Error::Upstream {
        provider: "p".into(),
        status: 429,
        body: "too many requests".into(),
    });
    assert_eq!(rate_limit.class, FailureClass::RateLimit);
    assert!(rate_limit.retryable());
    assert!(rate_limit.fallbackable());
    assert!(rate_limit.affects_observation());
    assert!(!rate_limit.affects_circuit());
    assert!(!rate_limit.provider_fault());

    let transport = ClassifiedFailure::from_message("connection refused");
    assert_eq!(transport.class, FailureClass::Transport);
    assert!(transport.retryable());
    assert!(transport.fallbackable());
    assert!(transport.affects_observation());
    assert!(transport.affects_circuit());
    assert!(transport.provider_fault());
}

#[test]
fn legacy_error_helpers_delegate_to_the_canonical_result() {
    let cases = [
        Error::Timeout(1),
        Error::OverBudget("limit".into()),
        Error::MissingApiKey("provider-a".into()),
        Error::Unauthorized,
        Error::Upstream {
            provider: "provider-a".into(),
            status: 429,
            body: "rate limited".into(),
        },
    ];

    for error in cases {
        let classified = error.classified();
        assert_eq!(error.is_retryable(), classified.impact.fallbackable);
        assert_eq!(
            error.counts_against_health(),
            classified.impact.affects_observation
        );
    }

    let mislabeled_validation = Error::Upstream {
        provider: "provider-a".into(),
        status: 500,
        body: r#"{"type":"invalid_request_error"}"#.into(),
    };
    let classified = mislabeled_validation.classified();
    assert_eq!(classified.class, FailureClass::InvalidRequest);
    assert!(!mislabeled_validation.is_retryable());
    assert!(!mislabeled_validation.counts_against_health());
}

#[test]
fn cancellation_and_interruption_are_terminal_non_successes() {
    let cancelled = ClassifiedFailure::cancelled("client cancelled request");
    assert_eq!(cancelled.class, FailureClass::ClientCancelled);
    assert!(!cancelled.is_success());
    assert!(!cancelled.retryable());
    assert!(!cancelled.fallbackable());
    assert!(!cancelled.affects_observation());
    assert!(!cancelled.affects_circuit());
    assert!(!cancelled.provider_fault());
    assert!(cancelled.records_stats());

    let interrupted = ClassifiedFailure::interrupted("stream interrupted after partial output");
    assert_eq!(interrupted.class, FailureClass::Interrupted);
    assert!(!interrupted.is_success());
    assert!(!interrupted.retryable());
    assert!(!interrupted.fallbackable());
    assert!(!interrupted.affects_observation());
    assert!(!interrupted.affects_circuit());
    assert!(!interrupted.provider_fault());
    assert!(interrupted.records_stats());

    assert_eq!(
        FailureClass::from_error_message("request cancelled by client"),
        FailureClass::ClientCancelled
    );
    assert_eq!(
        FailureClass::from_error_message("stream interrupted after partial output"),
        FailureClass::Interrupted
    );
    assert_eq!(
        FailureClass::from_error_message("client disconnected before the terminal event"),
        FailureClass::Interrupted
    );

    let cancelled_error =
        ClassifiedFailure::from_core_error(Error::Internal("request cancelled by client".into()));
    assert_eq!(cancelled_error.class, FailureClass::ClientCancelled);
    let interrupted_error = ClassifiedFailure::from_core_error(Error::BadUpstreamPayload(
        "stream interrupted after partial output".into(),
    ));
    assert_eq!(interrupted_error.class, FailureClass::Interrupted);
}
