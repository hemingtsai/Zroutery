//! The pipeline fixtures: continuation expansion, admission scopes, and the terminal transition.

use super::budget::main_admission_scopes;
use super::lifecycle::TerminalKind;
use super::*;
use crate::budget::{Budget, BudgetScope};
use crate::config::{AppConfig, ModelTier};
use crate::error::Error;
use crate::registry::Registry;
use crate::router::Candidate;
use crate::stats::RecordBuilder;

/// The Activity message for a failed request, through the same two steps the
/// terminal transition uses: the error becomes a terminal state, and that
/// state's message becomes the record's error.
fn activity_error(error: &Error) -> String {
    let terminal = TerminalKind::failed(error);
    let mut builder = RecordBuilder::new(Dialect::OpenAI, "sonnet-class", false);
    builder.fail(terminal.record_status(), terminal.record_message());
    builder.finish(0).error.unwrap_or_default()
}

#[test]
fn no_upstream_body_shape_reaches_the_activity_record() {
    // Bodies that the old `": <"` rule let through, plus the one it caught.
    let bodies = [
        "{\"error\":{\"message\":\"invalid key sk-SYNTHETIC-TEST-ONLY\"}}",
        "invalid key sk-SYNTHETIC-TEST-ONLY",
        "\n\t  <html><body>openresty</body></html>",
        "<html><body>openresty</body></html>",
    ];
    for body in bodies {
        let error = Error::Upstream {
            provider: "test-provider".into(),
            status: 401,
            body: body.into(),
            retry_after: None,
        };
        let shown = activity_error(&error);
        assert!(
            !shown.contains("sk-SYNTHETIC-TEST-ONLY") && !shown.contains("openresty"),
            "the provider body reached Activity: {shown}"
        );
        assert_eq!(shown, "upstream test-provider returned 401");
    }

    // A malformed payload is a body too, and its text can echo credentials.
    let error = Error::BadUpstreamPayload("{\"access_token\":\"sk-SYNTHETIC-TEST-ONLY\"}".into());
    let shown = activity_error(&error);
    assert!(
        !shown.contains("sk-SYNTHETIC-TEST-ONLY"),
        "the malformed payload reached Activity: {shown}"
    );
    assert_eq!(shown, "upstream returned malformed data");

    // A refused request is still explained, not blanked.
    let error = Error::invalid("model sonnet-class is not configured");
    assert_eq!(
        activity_error(&error),
        "invalid request: model sonnet-class is not configured"
    );
}

/// Two providers and one model per tier, so a resolution's provider and tier
/// scopes are both non-trivial.
fn scope_registry(budgets: Vec<Budget>) -> Registry {
    use crate::config::{ModelEntry, ProviderConfig, ProviderKind};

    let config = AppConfig {
        providers: vec![
            ProviderConfig::new("deepseek", "DeepSeek", ProviderKind::OpenAICompatible),
            ProviderConfig::new("openai", "OpenAI", ProviderKind::OpenAICompatible),
        ],
        models: vec![
            ModelEntry::for_upstream("deepseek", "flash", Some(ModelTier::Fast)),
            ModelEntry::for_upstream("deepseek", "pro", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("openai", "sol", Some(ModelTier::Reasoning)),
        ],
        budgets,
        ..Default::default()
    };
    Registry::new(Arc::new(config))
}

fn daily(scope: BudgetScope) -> Budget {
    Budget::new(scope, crate::budget::BudgetPeriod::Day, "USD", 1.0)
}

/// The permits cover exactly the scopes that have a configured limit: no
/// budget, no gate.
#[test]
fn admission_scopes_are_exactly_the_budgeted_ones() {
    let registry = scope_registry(Vec::new());
    assert!(
        main_admission_scopes(
            registry.config(),
            &registry,
            &Resolution::Tier(ModelTier::Standard)
        )
        .is_empty(),
        "no budgets at all means no admission gates"
    );

    // A budget for a provider this request cannot reach is not a permit it
    // has to hold, or a proxy would serialise traffic it does not have to.
    let registry = scope_registry(vec![daily(BudgetScope::Provider {
        id: "openai".into(),
    })]);
    assert!(main_admission_scopes(
        registry.config(),
        &registry,
        &Resolution::Tier(ModelTier::Standard)
    )
    .is_empty());

    // A disabled limit is not a limit.
    let mut disabled = daily(BudgetScope::Global);
    disabled.enabled = false;
    let registry = scope_registry(vec![disabled]);
    assert!(main_admission_scopes(
        registry.config(),
        &registry,
        &Resolution::Tier(ModelTier::Standard)
    )
    .is_empty());
}

/// Global, the candidate provider and the tier, in canonical order.
#[test]
fn admission_scopes_bundle_global_provider_and_tier() {
    let registry = scope_registry(vec![
        daily(BudgetScope::Global),
        daily(BudgetScope::Provider {
            id: "deepseek".into(),
        }),
        daily(BudgetScope::Tier {
            tier: ModelTier::Standard,
        }),
    ]);
    let scopes = main_admission_scopes(
        registry.config(),
        &registry,
        &Resolution::Tier(ModelTier::Standard),
    );
    assert_eq!(
        scopes,
        vec![
            BudgetScope::Global,
            BudgetScope::Provider {
                id: "deepseek".into()
            },
            BudgetScope::Tier {
                tier: ModelTier::Standard
            },
        ]
    );

    // A direct id occupies the provider it names and the tier it is billed
    // under, exactly as the check does — and neither is taken when the
    // configured limits do not cover them.
    let scopes = main_admission_scopes(
        registry.config(),
        &registry,
        &Resolution::Direct("openai-sol".into()),
    );
    assert_eq!(
        scopes,
        vec![BudgetScope::Global],
        "the deepseek provider budget and the standard tier budget do not \
         cover an openai reasoning model"
    );
}

/// A degrade target is part of the set before the first check, so the
/// degrade cannot leave the permit set.
#[test]
fn admission_scopes_include_the_degrade_closure() {
    let registry = scope_registry(vec![
        daily(BudgetScope::Provider {
            id: "deepseek".into(),
        }),
        daily(BudgetScope::Tier {
            tier: ModelTier::Fast,
        }),
        daily(BudgetScope::Tier {
            tier: ModelTier::Reasoning,
        })
        .degrading_to(ModelTier::Fast),
    ]);
    let scopes = main_admission_scopes(
        registry.config(),
        &registry,
        &Resolution::Tier(ModelTier::Reasoning),
    );
    assert_eq!(
        scopes,
        vec![
            BudgetScope::Provider {
                id: "deepseek".into()
            },
            BudgetScope::Tier {
                tier: ModelTier::Fast
            },
            BudgetScope::Tier {
                tier: ModelTier::Reasoning
            },
        ],
        "the tier the request can degrade to — and the providers it could \
         then reach — are held from the start"
    );
}

/// A classifier side request is gated by global and provider limits over
/// every candidate the pool could fail over to, and by no class limit.
#[test]
fn classifier_admission_scopes_cover_the_pool_but_not_the_class() {
    let registry = scope_registry(vec![
        daily(BudgetScope::Global),
        daily(BudgetScope::Provider {
            id: "deepseek".into(),
        }),
        daily(BudgetScope::Provider {
            id: "openai".into(),
        }),
        daily(BudgetScope::Tier {
            tier: ModelTier::Fast,
        }),
    ]);

    let candidate = |id: &str| {
        let entry = registry.entry(id).unwrap().clone();
        let provider = registry.provider_of(&entry).unwrap().clone();
        Candidate {
            exposed_id: id.to_string(),
            entry,
            provider,
            degraded: false,
            credential: None,
            rate_quote: None,
        }
    };
    let plan = vec![candidate("deepseek-flash"), candidate("openai-sol")];
    assert_eq!(
        classifier_admission_scopes(registry.config(), &plan),
        vec![
            BudgetScope::Global,
            BudgetScope::Provider {
                id: "deepseek".into()
            },
            BudgetScope::Provider {
                id: "openai".into()
            },
        ],
        "every candidate provider in the pool is held, and no class scope is"
    );

    // A pool that no budget covers takes no permits at all.
    assert!(classifier_admission_scopes(scope_registry(Vec::new()).config(), &plan).is_empty());
}

/// A candidate for the lifecycle-only tests, independent of any registry.
fn test_candidate(provider: &str, model: &str) -> Candidate {
    let entry = crate::config::ModelEntry::for_upstream(provider, model, Some(ModelTier::Standard));
    let provider = crate::config::ProviderConfig::new(
        provider,
        provider,
        crate::config::ProviderKind::OpenAICompatible,
    );
    Candidate {
        exposed_id: entry.exposed_id(),
        entry,
        provider,
        degraded: false,
        credential: None,
        rate_quote: None,
    }
}

/// One lifecycle for a main request, with the candidate's first attempt open.
fn lifecycle_with_attempt(state: &Arc<AppState>, candidate: &Candidate) -> RequestLifecycle {
    let mut lifecycle = RequestLifecycle::new(
        Arc::clone(state),
        Dialect::OpenAI,
        false,
        crate::query::RequestKind::Main,
        "requested-model",
        &axum::http::HeaderMap::new(),
        #[cfg(feature = "ml")]
        None,
    );
    lifecycle.begin_attempt(candidate, false).unwrap();
    lifecycle
}

/// A charge that measures exactly `amount` USD: `pricing` prices this usage at
/// that figure, and it is the settlement the terminal transition receives.
fn settlement(
    candidate: &Candidate,
    pricing: &crate::billing::Pricing,
    usage: crate::ir::Usage,
) -> Settlement {
    Settlement {
        usage,
        pricing: Some(pricing.clone()),
        provider_id: candidate.provider.id.clone(),
        tier: Some(ModelTier::Standard),
    }
}

/// The overshoot bound, exercised the way the pipeline exercises it.
///
/// Every request takes its budget gate, checks the ledger while holding it, and
/// only then reaches its one terminal transition — which is the single charge
/// site. Eight requests race for a limit that covers exactly two of them, so the
/// assertions are: exactly two are admitted, the ledger holds exactly two
/// charges, and a second `finalize` on the same lifecycle (a duplicate terminal
/// path, such as a stream drop after completion) cannot charge again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_are_admitted_to_the_limit_and_charged_once() {
    let mut config = AppConfig::default();
    config.models = vec![
        crate::config::ModelEntry::for_upstream("relay", "stream-model", Some(ModelTier::Standard)),
        crate::config::ModelEntry::for_upstream("relay", "other-model", Some(ModelTier::Standard)),
    ];
    config.budgets = vec![crate::budget::Budget::new(
        BudgetScope::Global,
        crate::budget::BudgetPeriod::Day,
        "USD",
        // Two charges of 0.6 fit; the third does not.
        1.0,
    )];
    let state = Arc::new(AppState::new(
        config,
        Arc::new(crate::config::MemorySecretStore::new()),
    ));
    let registry = state.registry();

    let pricing = crate::billing::Pricing::new("USD", 3.0, 0.0);
    let usage = crate::ir::Usage {
        input_tokens: 200_000,
        ..Default::default()
    };
    let each = pricing.cost_of(&usage).amount;
    assert!(
        (each - 0.6).abs() < 1e-9,
        "the fixture charges 0.6 per request"
    );

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let state = Arc::clone(&state);
        let registry = Arc::clone(&registry);
        let pricing = pricing.clone();
        tasks.push(tokio::spawn(async move {
            match apply_budgets(&state, &registry, Resolution::Tier(ModelTier::Standard)).await {
                Ok((resolution, guard)) => {
                    assert_eq!(resolution, Resolution::Tier(ModelTier::Standard));
                    // The gate is still held here, exactly as it is between the
                    // check and the terminal transition in the real pipeline.
                    let mut candidate = test_candidate("relay", "stream-model");
                    candidate.entry.pricing = Some(pricing.clone());
                    let mut lifecycle = lifecycle_with_attempt(&state, &candidate);
                    lifecycle
                        .finalize(
                            TerminalKind::Served,
                            settlement(&candidate, &pricing, usage),
                        )
                        .unwrap();
                    // A duplicate terminal attempt must be a no-op, including
                    // the charge.
                    lifecycle
                        .finalize(
                            TerminalKind::Served,
                            settlement(&candidate, &pricing, usage),
                        )
                        .unwrap();
                    drop(guard);
                    true
                }
                Err(Error::OverBudget(_)) => false,
                Err(other) => panic!("an unexpected refusal: {other}"),
            }
        }));
    }

    let mut admitted = 0;
    for (index, task) in tasks.into_iter().enumerate() {
        let served = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap_or_else(|_| panic!("request {index} never got its turn"))
            .expect("no task panicked");
        if served {
            admitted += 1;
        }
    }

    assert_eq!(
        admitted, 2,
        "the gate must serialise admission, so a limit covering two requests admits two"
    );
    let spent: f64 = state
        .ledger()
        .totals_for(&BudgetScope::Global, chrono::Local::now())
        .into_iter()
        .filter(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .sum();
    assert!(
        (spent - 2.0 * each).abs() < 1e-9,
        "the ledger must hold one charge per admitted request: {spent} vs {}",
        2.0 * each
    );
    assert_eq!(
        state.outcomes().len(),
        2,
        "a duplicate terminal transition must not record a second outcome"
    );
    for outcome in state.outcomes().recent(2) {
        assert!(
            outcome
                .actual_cost
                .is_some_and(|cost| (cost - each).abs() < 1e-9),
            "every admitted request carries exactly one charge: {:?}",
            outcome.actual_cost
        );
    }
}

/// An exhausted tier follows its configured degrade, and the spend lands on the
/// tier that actually served.
///
/// The permit set covers the degrade closure before the first check, so the
/// cheaper tier is already held when the request arrives there. The single
/// terminal transition then charges once, against Fast — never against the
/// Standard scope that refused.
#[tokio::test]
async fn an_exhausted_tier_degrades_and_charges_the_tier_that_served() {
    let mut config = AppConfig::default();
    config.models = vec![
        crate::config::ModelEntry::for_upstream("relay", "pro-model", Some(ModelTier::Standard)),
        crate::config::ModelEntry::for_upstream("relay", "flash-model", Some(ModelTier::Fast)),
    ];
    config.budgets = vec![
        crate::budget::Budget::new(
            BudgetScope::Tier {
                tier: ModelTier::Standard,
            },
            crate::budget::BudgetPeriod::Day,
            "USD",
            0.0,
        )
        .degrading_to(ModelTier::Fast),
        crate::budget::Budget::new(
            BudgetScope::Tier {
                tier: ModelTier::Fast,
            },
            crate::budget::BudgetPeriod::Day,
            "USD",
            100.0,
        ),
    ];
    let state = Arc::new(AppState::new(
        config,
        Arc::new(crate::config::MemorySecretStore::new()),
    ));
    let registry = state.registry();

    let (resolution, guard) =
        apply_budgets(&state, &registry, Resolution::Tier(ModelTier::Standard))
            .await
            .expect("the exhausted tier degrades instead of refusing");
    assert_eq!(
        resolution,
        Resolution::Tier(ModelTier::Fast),
        "the request is served by the cheaper tier"
    );

    let mut candidate = test_candidate("relay", "flash-model");
    let pricing = crate::billing::Pricing::new("USD", 3.0, 0.0);
    let usage = crate::ir::Usage {
        input_tokens: 200_000,
        ..Default::default()
    };
    candidate.entry.pricing = Some(pricing.clone());
    candidate.entry.tier = Some(ModelTier::Fast);
    let mut lifecycle = lifecycle_with_attempt(&state, &candidate);
    let mut fast_settlement = settlement(&candidate, &pricing, usage);
    fast_settlement.tier = Some(ModelTier::Fast);
    lifecycle
        .finalize(TerminalKind::Served, fast_settlement)
        .unwrap();
    drop(guard);

    let fast: f64 = state
        .ledger()
        .totals_for(
            &BudgetScope::Tier {
                tier: ModelTier::Fast,
            },
            chrono::Local::now(),
        )
        .into_iter()
        .filter(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .sum();
    let standard: f64 = state
        .ledger()
        .totals_for(
            &BudgetScope::Tier {
                tier: ModelTier::Standard,
            },
            chrono::Local::now(),
        )
        .into_iter()
        .filter(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .sum();
    assert!(
        (fast - pricing.cost_of(&usage).amount).abs() < 1e-12,
        "the serving tier is charged once: {fast}"
    );
    assert_eq!(standard, 0.0, "the refusing tier is never charged");
    assert_eq!(state.outcomes().len(), 1);
}

/// A stream the client walks away from mid-answer is not a success, is still
/// billed for what it used, and leaves provider health untouched.
///
/// This is the `SseState::drop` path itself, not a server round trip: the body
/// produces one frame, the consumer is dropped, and the drop guard owns the one
/// terminal transition. It is the direct assertion that an interruption is a
/// terminal state and not a silent zero.
#[tokio::test]
async fn a_disconnected_stream_is_billed_once_and_leaves_health_alone() {
    use futures_util::StreamExt as _;

    use super::sse::{sse_body, StreamContext};
    use crate::billing::Pricing;
    use crate::ir::{StreamEvent, Usage};

    let state = Arc::new(AppState::new(
        AppConfig::default(),
        Arc::new(crate::config::MemorySecretStore::new()),
    ));
    let pricing = Pricing::new("USD", 3.0, 15.0);
    let reported = Usage {
        input_tokens: 11,
        output_tokens: 7,
        ..Default::default()
    };
    let expected = pricing.cost_of(&reported).amount;

    let mut candidate = test_candidate("relay", "stream-model");
    candidate.entry.pricing = Some(pricing.clone());
    let lifecycle = lifecycle_with_attempt(&state, &candidate);
    let events: crate::upstream::EventStream = Box::pin(futures_util::stream::iter(vec![
        Ok(StreamEvent::Start {
            id: "chunk-1".into(),
            model: "upstream-stream-model".into(),
            usage: reported,
        }),
        Ok(StreamEvent::TextDelta {
            index: 0,
            text: "hel".into(),
        }),
        Ok(StreamEvent::TextDelta {
            index: 0,
            text: "lo".into(),
        }),
    ]));
    let context = StreamContext {
        lifecycle,
        usage: Usage::default(),
        pricing: Some(pricing.clone()),
        provider_id: "relay".into(),
        tier: Some(ModelTier::Standard),
        kind: crate::query::RequestKind::Main,
        classifier_severity: false,
        response_id: None,
        cancel_rx: None,
        storage: Default::default(),
        input_items: Vec::new(),
        previous_response_id: None,
        admission: state.admit(&[]).await,
    };
    let mut body = Box::pin(sse_body(
        Arc::clone(&state),
        events,
        protocol::stream_encoder(Dialect::OpenAI, "stream-model", false),
        context,
    ));

    // The client reads the opening frame — so the answer really has begun —
    // and then goes away.
    let first = body
        .next()
        .await
        .expect("the opening frame")
        .expect("no transport error");
    assert!(!first.is_empty(), "the upstream answer reached the client");
    drop(body);

    let outcome = state
        .outcomes()
        .recent(1)
        .pop()
        .expect("the drop guard wrote a terminal outcome");
    assert_eq!(
        outcome.final_status,
        crate::outcome::FinalStatus::Interrupted,
        "an abandoned answer is an interruption"
    );
    assert_eq!(
        outcome.failure_class(),
        Some(crate::failure::FailureClass::Interrupted)
    );
    assert!(!outcome.is_terminal_success());
    assert!(
        outcome.served_identity().is_none(),
        "nothing delivered a finished answer"
    );
    assert_eq!(outcome.attempts.len(), 1);
    assert!(
        !outcome.attempts[0].success,
        "the stream's attempt is not a served attempt"
    );

    // Billing keeps what the provider reported, once, and attributes it both to
    // the request and to the attempt that incurred it.
    assert!(
        outcome
            .actual_cost
            .is_some_and(|cost| (cost - expected).abs() < 1e-12),
        "the interrupted request is charged for the usage it reported: {:?}",
        outcome.actual_cost
    );
    assert!(
        outcome.attempts[0]
            .cost
            .is_some_and(|cost| (cost - expected).abs() < 1e-12),
        "the attempt carries the spend a cost-aware router reads"
    );
    let spent: f64 = state
        .ledger()
        .totals_for(&BudgetScope::Global, chrono::Local::now())
        .into_iter()
        .filter(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .sum();
    assert!(
        (spent - expected).abs() < 1e-12,
        "exactly one charge reached the ledger: {spent} vs {expected}"
    );

    // A client leaving says nothing about the provider.
    assert!(
        state.router().health_snapshot().is_empty(),
        "an interruption must not create a health row: {:?}",
        state.router().health_snapshot()
    );

    let record = state
        .stats()
        .recent(1)
        .pop()
        .expect("the terminal transition wrote one activity record");
    assert!(!record.ok);
    assert_eq!(record.status, 499, "the client closed the request");
}

/// A Responses client may close as soon as it receives response.completed,
/// without waiting for TCP EOF. That is a completed answer, not a 499.
#[tokio::test]
async fn a_client_closing_after_the_terminal_frame_is_not_a_disconnect() {
    use super::sse::{sse_body, StreamContext};
    use crate::ir::{StopReason, StreamEvent, Usage};
    use futures_util::StreamExt as _;

    let state = Arc::new(AppState::new(
        AppConfig::default(),
        Arc::new(crate::config::MemorySecretStore::new()),
    ));
    let candidate = test_candidate("relay", "stream-model");
    let lifecycle = lifecycle_with_attempt(&state, &candidate);
    let terminal_usage = Usage {
        input_tokens: 11,
        output_tokens: 7,
        ..Default::default()
    };
    let events = futures_util::stream::iter(vec![
        Ok(StreamEvent::Start {
            id: "stream-test".into(),
            model: "stream-model".into(),
            usage: Usage::default(),
        }),
        Ok(StreamEvent::TextDelta {
            index: 0,
            text: "answer".into(),
        }),
        Ok(StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: terminal_usage,
        }),
    ])
    .chain(futures_util::stream::pending());
    let body = sse_body(
        Arc::clone(&state),
        Box::pin(events),
        crate::protocol::stream_encoder(Dialect::OpenAIResponses, "stream-model", false),
        StreamContext {
            lifecycle,
            usage: Usage::default(),
            pricing: None,
            provider_id: "relay".into(),
            tier: Some(ModelTier::Standard),
            kind: crate::query::RequestKind::Main,
            classifier_severity: false,
            response_id: None,
            cancel_rx: None,
            storage: crate::protocol::responses::StoragePolicy::Discard,
            input_items: Vec::new(),
            previous_response_id: None,
            admission: state.admit(&[]).await,
        },
    );
    let mut body = Box::pin(body);
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
            .await
            .expect("a terminal frame arrives without upstream EOF")
            .expect("terminal frame")
            .expect("stream bytes");
        if String::from_utf8_lossy(&frame).contains("event: response.completed") {
            break;
        }
    }
    drop(body);
    let record = &state.stats().recent(1)[0];
    assert!(
        record.ok,
        "reading the complete answer must not be recorded as 499: {:?}",
        record.error
    );
    assert_eq!(record.status, 200);
    assert_eq!(record.usage, terminal_usage);
    assert_eq!(state.outcomes().len(), 1);
}

#[test]
fn explicit_credentials_order_is_stable_and_does_not_change_models() {
    use super::credentials::expand_inference_keys;
    use crate::config::{AccountConfig, InferenceKeyConfig};
    let mut candidate = test_candidate("p", "model");
    candidate.provider.accounts = vec![AccountConfig {
        account_id: "a".into(),
        key_ref: "personal".into(),
        enabled: true,
        inference_keys: vec![
            InferenceKeyConfig {
                id: "z".into(),
                key_ref: "inference-z".into(),
                enabled: true,
                priority: 0,
                note: String::new(),
            },
            InferenceKeyConfig {
                id: "a".into(),
                key_ref: "inference-a".into(),
                enabled: true,
                priority: 0,
                note: String::new(),
            },
        ],
        ..Default::default()
    }];
    let mut config = AppConfig::default();
    config.providers.push(candidate.provider.clone());
    let state = AppState::new(config, Arc::new(crate::config::MemorySecretStore::new()));
    let expanded = expand_inference_keys(&state, vec![candidate.clone()], 2).unwrap();
    assert_eq!(
        expanded
            .iter()
            .map(|candidate| candidate.credential.as_ref().unwrap().key_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert!(expanded
        .iter()
        .all(|expanded| expanded.model_id() == candidate.model_id()));
    assert_eq!(
        expand_inference_keys(&state, vec![candidate], 1)
            .unwrap()
            .len(),
        1
    );
}
