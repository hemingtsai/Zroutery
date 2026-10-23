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
