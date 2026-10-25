//! Gate fixtures for client attribution and per-client aggregation.
//!
//! # What these exist to prevent
//!
//! Four claims, each of which a dashboard will happily print:
//!
//! * **"Claude Code sent this."** A `User-Agent` is caller-controlled text. The
//!   ladder must therefore record *how* it concluded that, and a header rule can
//!   never reach `strong` — only an authenticated credential can. Every fixture
//!   here that asserts a client name also asserts the source and confidence
//!   that earned it.
//!
//! * **"These are two different clients."** `curl/8.4.0` appears verbatim inside
//!   the agent string of tools that embed libcurl, and `python-requests` appears
//!   inside half the Python ecosystem. Substring matching would manufacture a
//!   client per embedder.
//!
//! * **"The per-client table adds up."** If the buckets do not sum to the
//!   global totals, the headline number and the breakdown are describing
//!   different traffic. [`per_client_sums_to_the_global_totals`] and its siblings
//!   exist to make that impossible to regress silently.
//!
//! * **"We recognise 90% of our traffic."** Meaningless without saying how. A
//!   `python-requests` match identifies a library, not an application, so it
//!   counts as *weak* and never inflates the recognised share.
//!
//! **High-cardinality traffic is not a storage problem.** `User-Agent` is
//! caller-controlled, so a caller that varies one character per request would
//! grow an unbounded table. The registry folds the overflow into one bucket that
//! still *counts*, so the totals keep adding up.

use axum::http::{header, HeaderMap, HeaderName, HeaderValue};
use zroutery_core::client::{
    identify, AttributionSource, ClientFamily, ClientFingerprint, ClientId, ClientRegistry,
    Confidence, CredentialBinding, MAX_CUSTOM_CLIENT_ID_LEN, MAX_TRACKED_CLIENTS,
};
use zroutery_core::ir::{Dialect, Usage};
use zroutery_core::stats::{RecordBuilder, Stats};

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes()).expect("header name");
        let value = HeaderValue::from_str(value).expect("header value");
        map.insert(name, value);
    }
    map
}

fn ua(value: &str) -> HeaderMap {
    headers(&[(header::USER_AGENT.as_str(), value)])
}

#[test]
fn takeover_confirmation_uses_recent_recognised_arrivals_not_activity_totals() {
    let stats = Stats::new(1);
    let adopted = chrono::Utc::now();
    let opencode = identify(
        &headers(&[
            ("x-opencode-client", "cli"),
            ("x-opencode-session", "session-fixture"),
        ]),
        None,
    );
    stats.observe_client(&opencode, adopted - chrono::Duration::seconds(1));
    assert!(!stats.client_seen_since("opencode", adopted));
    stats.observe_client(
        &ClientFingerprint::unknown(),
        adopted + chrono::Duration::seconds(1),
    );
    stats.observe_client(
        &ClientFingerprint::custom("codex", ClientFamily::Cli),
        adopted + chrono::Duration::seconds(1),
    );
    assert!(!stats.client_seen_since("codex", adopted));
    stats.observe_client(&opencode, adopted + chrono::Duration::seconds(1));
    assert!(stats.client_seen_since("opencode", adopted));
    assert!(!stats.client_seen_since("claude_code", adopted));
    assert_eq!(
        stats.summary().requests,
        0,
        "an arrival is not a completion"
    );
    stats.clear();
    assert!(
        stats.client_seen_since("opencode", adopted),
        "clearing activity does not undo connectivity"
    );
    assert!(
        !stats.client_seen_since("opencode", adopted + chrono::Duration::seconds(2)),
        "a new adopt needs new evidence"
    );
    stats.observe_client(&opencode, adopted - chrono::Duration::seconds(2));
    assert!(
        stats.client_seen_since("opencode", adopted),
        "an older concurrent arrival cannot erase newer evidence"
    );
    stats.forget_client_observation("opencode");
    assert!(!stats.client_seen_since("opencode", adopted));
}

/// The id, source and confidence a rule produced, as one comparable triple.
fn triple(fingerprint: &ClientFingerprint) -> (&str, AttributionSource, Confidence) {
    (
        fingerprint.client_id.as_str(),
        fingerprint.source,
        fingerprint.confidence,
    )
}

// ---------------------------------------------------------------------------
// The ladder: one order, shared by every ingress
// ---------------------------------------------------------------------------

#[test]
fn an_authenticated_named_credential_outranks_every_header() {
    let binding = CredentialBinding {
        credential_id: "cred-1".into(),
        client_id: ClientId::trusted("my_team_ide"),
    };
    // A request that also claims, by header, to be something else entirely.
    let inbound = headers(&[
        ("x-zroutery-client", "claude_code"),
        ("originator", "codex_cli_rs"),
        (header::USER_AGENT.as_str(), "codex_cli_rs/0.9.0"),
    ]);

    let fingerprint = identify(&inbound, Some(&binding));

    assert_eq!(fingerprint.client_id.as_str(), "my_team_ide");
    assert_eq!(fingerprint.source, AttributionSource::CredentialBinding);
    assert_eq!(
        fingerprint.confidence,
        Confidence::Strong,
        "a credential that authenticated is the only source that may be strong"
    );
    assert!(
        fingerprint.confidence.is_recognised(),
        "and it counts towards the recognised share"
    );
}

#[test]
fn an_explicit_client_header_beats_a_user_agent_claim() {
    let inbound = headers(&[
        ("x-zroutery-client", "codex"),
        (header::USER_AGENT.as_str(), "claude-cli/2.0.0"),
    ]);
    let fingerprint = identify(&inbound, None);

    assert_eq!(fingerprint.client_id.as_str(), "codex");
    assert_eq!(fingerprint.source, AttributionSource::ExplicitHeader);
    assert_eq!(fingerprint.confidence, Confidence::Probable);
}

#[test]
fn an_unrecognised_explicit_value_falls_through_rather_than_being_trusted() {
    // An operator typo must not become a client id and must not suppress the
    // rules that could still identify the caller.
    let inbound = headers(&[
        ("x-zroutery-client", "claud-code"),
        (header::USER_AGENT.as_str(), "codex_cli_rs/0.9.0"),
    ]);
    let fingerprint = identify(&inbound, None);
    assert_eq!(
        fingerprint.client_id.as_str(),
        "codex",
        "a hint we do not recognise is not an answer"
    );
}

/// A client naming itself something Zroutery has never heard of is still a
/// client — but only on its own word, which is a header.
#[test]
fn a_custom_declared_identity_is_kept_but_only_weakly() {
    let inbound = headers(&[("x-zroutery-client", "AcmeCorp IDE")]);
    let fingerprint = identify(&inbound, None);

    assert_eq!(fingerprint.client_id.as_str(), "acmecorp_ide");
    assert_eq!(fingerprint.source, AttributionSource::ExplicitHeader);
    assert_eq!(
        fingerprint.confidence,
        Confidence::Weak,
        "a header the caller sets is not evidence of a product"
    );
    assert!(
        !fingerprint.confidence.is_recognised(),
        "and it must not inflate the recognised share"
    );
}

/// An unregistered declaration does not beat a rule that actually matches.
#[test]
fn a_custom_declaration_loses_to_a_real_fingerprint() {
    let inbound = headers(&[
        ("x-zroutery-client", "AcmeCorp IDE"),
        (header::USER_AGENT.as_str(), "claude-cli/2.0.14"),
    ]);
    assert_eq!(identify(&inbound, None).client_id.as_str(), "claude_code");
}

#[test]
fn the_codex_originator_header_identifies_codex() {
    let fingerprint = identify(&headers(&[("originator", "codex_cli_rs")]), None);
    assert_eq!(
        triple(&fingerprint),
        (
            "codex",
            AttributionSource::ClientHeader,
            Confidence::Probable
        )
    );
}

#[test]
fn the_claude_code_header_pair_needs_both_halves() {
    let both = headers(&[("x-app", "cli"), ("anthropic-beta", "claude-code-20250219")]);
    assert_eq!(
        identify(&both, None).source,
        AttributionSource::ClientHeader,
        "the pairing identifies Claude Code"
    );

    // `x-app: cli` alone is generic: plenty of CLIs send it.
    let generic = identify(&headers(&[("x-app", "cli")]), None);
    assert_eq!(
        generic.source,
        AttributionSource::Fallback,
        "one generic header is not an identity"
    );

    // `anthropic-beta` alone is not unique to one client either.
    let beta_only = identify(
        &headers(&[("anthropic-beta", "claude-code-20250219")]),
        None,
    );
    assert_eq!(beta_only.source, AttributionSource::Fallback);
}

// ---------------------------------------------------------------------------
// User-Agent rules: prefix, never substring
// ---------------------------------------------------------------------------

/// OpenCode is identified by its own headers, not only by its User-Agent.
///
/// This is the case a `User-Agent` rule alone gets wrong. Verified: OpenCode
/// sets `opencode/<channel>/<version>/<name>` only on its built-in provider
/// paths. A **custom** `openai-compatible` provider — which is exactly what an
/// OpenCode takeover creates — inherits the AI SDK's agent string instead, so
/// the UA says `ai-sdk/openai-compatible/...` and nothing else. The
/// `x-opencode-*` headers are set on the shared request path every provider
/// goes through, so they survive where the User-Agent does not.
#[test]
fn opencode_is_still_identified_when_it_uses_a_custom_provider() {
    let fingerprint = identify(
        &headers(&[
            ("x-opencode-client", "opencode"),
            ("x-opencode-session", "ses_abc"),
            ("x-opencode-session-id", "ses_abc"),
            (
                header::USER_AGENT.as_str(),
                "ai-sdk/openai-compatible/1.2.3",
            ),
        ]),
        None,
    );

    assert_eq!(
        fingerprint.client_id.as_str(),
        "opencode",
        "a custom provider strips the opencode User-Agent; the headers do not"
    );
    assert_eq!(fingerprint.source, AttributionSource::ClientHeader);
    assert_eq!(fingerprint.confidence, Confidence::Probable);
}

/// One header is not enough. OpenCode sends several `x-opencode-*` headers, and
/// the rule requires the pair rather than trusting any single one.
#[test]
fn a_lone_opencode_header_is_not_enough() {
    let fingerprint = identify(&headers(&[("x-opencode-client", "opencode")]), None);
    assert_ne!(
        fingerprint.client_id.as_str(),
        "opencode",
        "one header alone is a weaker signal than the pair"
    );
}

/// The verified agent strings include an OS/arch/terminal tail.
///
/// Codex builds `codex_cli_rs/<version> (<OS> <ver>; <arch>) <terminal>`, so a
/// rule that compared the whole string would fail on every machine.
#[test]
fn the_verified_agent_string_tails_do_not_defeat_prefix_matching() {
    for agent in [
        "codex_cli_rs/0.152.2 (Ubuntu 24.04; x86_64) vscode",
        "codex_cli_rs/0.152.2 (macOS 15.1; arm64) iTerm.app",
        "claude-cli/2.1.281 (external, cli)",
        "claude-cli/2.1.281 (external, cli, agent-sdk/1.0.0, client-app/2)",
        "opencode/stable/2.0.30/opencode",
    ] {
        let fingerprint = identify(&ua(agent), None);
        assert!(
            fingerprint.confidence.is_recognised(),
            "{agent} must resolve to a named client, got {fingerprint:?}"
        );
    }
}

/// Claude Code sends a *second* agent string for metadata and token requests.
///
/// Matching only `claude-cli/` would leave those unattributed.
#[test]
fn the_second_claude_code_agent_string_is_also_matched() {
    assert_eq!(
        identify(&ua("claude-code/2.1.281"), None)
            .client_id
            .as_str(),
        "claude_code",
        "metadata and token traffic uses a different prefix"
    );
}

/// A real Claude Code string is not a substring accident.
#[test]
fn a_claude_code_string_embedded_in_another_product_is_not_claude_code() {
    let fingerprint = identify(&ua("acme-wrapper/1.0 (proxying claude-cli/2.1.281)"), None);
    assert_ne!(
        fingerprint.client_id.as_str(),
        "claude_code",
        "prefix matching, not substring matching: {fingerprint:?}"
    );
}

#[test]
fn the_verified_agent_fingerprints_are_recognised() {
    for (agent, expected_id, family) in [
        ("claude-cli/2.0.14", "claude_code", ClientFamily::Cli),
        ("codex_cli_rs/0.9.0", "codex", ClientFamily::Cli),
        ("opencode/0.15.3", "opencode", ClientFamily::Cli),
        ("aider/0.86.0", "aider", ClientFamily::Cli),
    ] {
        let fingerprint = identify(&ua(agent), None);
        assert_eq!(
            fingerprint.client_id.as_str(),
            expected_id,
            "user agent {agent}"
        );
        assert_eq!(fingerprint.family, family, "user agent {agent}");
        assert_eq!(
            fingerprint.source,
            AttributionSource::UserAgent,
            "user agent {agent}"
        );
        assert_eq!(
            fingerprint.confidence,
            Confidence::Probable,
            "user agent {agent}"
        );
    }
}

#[test]
fn versions_do_not_create_new_clients() {
    // The same product across releases must land in one bucket, or a
    // per-client table becomes a per-version table.
    for version in ["1.0.0", "2.0.14", "2.1.0-beta.3", "10.4.2"] {
        let fingerprint = identify(&ua(&format!("claude-cli/{version}")), None);
        assert_eq!(
            fingerprint.client_id.as_str(),
            "claude_code",
            "version {version} must not fork the bucket"
        );
    }
}

#[test]
fn an_alias_does_not_create_a_second_bucket_for_one_product() {
    // Codex sends more than one agent string across versions.
    for agent in ["codex_cli_rs/0.9.0", "codex-cli/0.9.0", "codex/0.9.0"] {
        assert_eq!(
            identify(&ua(agent), None).client_id.as_str(),
            "codex",
            "{agent} is Codex"
        );
    }
}

/// The one that a substring implementation gets wrong.
///
/// `curl/8.4.0` appears *inside* the agent string of anything embedding
/// libcurl. Prefix matching keeps those as themselves.
#[test]
fn another_products_name_inside_a_user_agent_is_not_that_product() {
    let cases = [
        ("myapp/1.0 (embedded curl/8.4.0)", "not curl"),
        ("python-requests-toolbelt/1.0.0", "not python_requests"),
        ("acme-agent (python-requests/2.31.0)", "not python_requests"),
        ("some-proxy/v1 (openai-python/1.2)", "not openai_sdk"),
    ];
    for (agent, why) in cases {
        let fingerprint = identify(&ua(agent), None);
        assert_ne!(
            fingerprint.client_id.as_str(),
            "curl",
            "{agent} must not be attributed to curl: {why}"
        );
        assert_ne!(
            fingerprint.client_id.as_str(),
            "python_requests",
            "{agent} must not be attributed to python_requests: {why}"
        );
        assert_ne!(
            fingerprint.client_id.as_str(),
            "openai_sdk",
            "{agent} must not be attributed to openai_sdk: {why}"
        );
    }
}

#[test]
fn a_product_rule_wins_over_the_runtime_it_embeds() {
    // A real client that happens to send a generic-looking agent string must
    // still resolve to the product, because product rules are scanned first.
    let fingerprint = identify(&ua("claude-cli/2.0.14"), None);
    assert_eq!(fingerprint.client_id.as_str(), "claude_code");
}

// ---------------------------------------------------------------------------
// Confidence: a library is not an application
// ---------------------------------------------------------------------------

#[test]
fn a_runtime_or_sdk_match_is_weak_even_though_the_rule_is_documented() {
    for (agent, expected) in [
        ("python-requests/2.31.0", "python_requests"),
        ("curl/8.4.0", "curl"),
        ("Go-http-client/2.0", "go_http"),
        ("openai-python/1.55.0", "openai_sdk"),
    ] {
        let fingerprint = identify(&ua(agent), None);
        assert_eq!(fingerprint.client_id.as_str(), expected, "{agent}");
        assert_eq!(
            fingerprint.confidence,
            Confidence::Weak,
            "{agent} identifies a library, not an application"
        );
        assert!(
            !fingerprint.confidence.is_recognised(),
            "{agent} must not inflate the recognised share"
        );
    }
}

#[test]
fn an_unregistered_agent_is_kept_but_only_weakly() {
    let fingerprint = identify(&ua("AcmeCorpInternal/3.1 (+support@acme.test)"), None);
    assert_eq!(fingerprint.confidence, Confidence::Weak);
    assert_eq!(fingerprint.family, ClientFamily::Unknown);
    // Case, punctuation and separator runs are folded, so the same product
    // keeps one bucket however it spells itself.
    assert_eq!(
        fingerprint.client_id.as_str(),
        "acmecorpinternal_3.1_support_acme.test",
        "got {}",
        fingerprint.client_id
    );
    assert_eq!(
        identify(&ua("AcmeCorpInternal/3.1_(+support@acme.test)"), None).client_id,
        fingerprint.client_id,
        "separator runs must not fork the bucket"
    );
}

#[test]
fn two_spellings_of_one_unknown_agent_share_a_bucket() {
    let a = identify(&ua("MyTool/1.0"), None);
    let b = identify(&ua("mytool/1.0"), None);
    assert_eq!(
        a.client_id, b.client_id,
        "caller-controlled text must not be a free storage dimension"
    );
}

#[test]
fn nothing_at_all_is_unknown() {
    let fingerprint = identify(&HeaderMap::new(), None);
    assert_eq!(
        triple(&fingerprint),
        (
            ClientId::UNKNOWN,
            AttributionSource::Fallback,
            Confidence::Unknown
        )
    );
    assert_eq!(fingerprint.family, ClientFamily::Unknown);
}

#[test]
fn an_empty_or_blank_user_agent_is_not_a_client() {
    for value in ["", "   "] {
        let fingerprint = identify(&ua(value), None);
        assert_eq!(
            fingerprint.confidence,
            Confidence::Unknown,
            "a blank agent string is the absence of evidence, not evidence"
        );
    }
}

#[test]
fn a_header_value_that_is_not_text_is_treated_as_absent() {
    let mut map = HeaderMap::new();
    map.insert(
        header::USER_AGENT,
        HeaderValue::from_bytes(&[0xff, 0xfe])
            .expect("invalid utf-8 is still a valid header value"),
    );
    assert_eq!(identify(&map, None).confidence, Confidence::Unknown);
}

// ---------------------------------------------------------------------------
// Bounded cardinality
// ---------------------------------------------------------------------------

#[test]
fn a_caller_controlled_id_is_length_capped_and_folded() {
    let long = "a".repeat(MAX_CUSTOM_CLIENT_ID_LEN * 4);
    let id = ClientId::sanitised(&long);
    assert!(
        id.as_str().len() <= MAX_CUSTOM_CLIENT_ID_LEN,
        "got {}",
        id.as_str().len()
    );
}

#[test]
fn an_unusable_client_id_folds_to_unknown_rather_than_passing_through() {
    for raw in ["", "   ", "///", "\u{1f600}\u{1f600}"] {
        let id = ClientId::sanitised(raw);
        assert!(id.is_unknown(), "{raw:?} must fold to unknown, got {id}");
    }
}

#[test]
fn the_registry_stops_growing_and_keeps_counting() {
    let mut registry = ClientRegistry::new();

    // One more than the cap, each with a distinct id.
    for i in 0..(MAX_TRACKED_CLIENTS + 10) {
        let fingerprint = ClientFingerprint::custom(&format!("agent-{i}"), ClientFamily::Unknown);
        registry.register(fingerprint);
    }

    assert!(
        registry.len() <= MAX_TRACKED_CLIENTS,
        "a caller must not be able to grow the table without bound: {}",
        registry.len()
    );
    assert!(
        registry.overflowed(),
        "and the fold has to be visible rather than silent"
    );

    // The overflow bucket is one shared entry, and a caller that keeps varying
    // the id keeps landing in it rather than creating more.
    let overflowed = ClientId::overflow();
    assert!(
        registry.get(&overflowed).is_none(),
        "the overflow id is what a record stores, not a registered fingerprint"
    );
    assert_eq!(overflowed, ClientId::overflow(), "and it must be stable");
}

#[test]
fn a_registered_id_is_returned_unchanged_so_records_and_totals_agree() {
    let mut registry = ClientRegistry::new();
    let fingerprint = ClientFingerprint::custom("my-agent", ClientFamily::Unknown);
    let stored = registry.register(fingerprint);

    assert_eq!(stored.as_str(), "my-agent");
    assert_eq!(
        registry.register(ClientFingerprint::custom("my-agent", ClientFamily::Unknown)),
        stored,
        "re-registering an existing id must not fork it"
    );
    assert_eq!(registry.len(), 1);
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

fn record(
    client: &ClientFingerprint,
    model: &str,
    usage: Usage,
    ok: bool,
    latency_ms: u64,
    ttft: Option<u64>,
    cost: Option<(&str, f64)>,
) -> zroutery_core::stats::RequestRecord {
    let mut builder = RecordBuilder::new(Dialect::Anthropic, model, ttft.is_some());
    builder
        .client(client.clone())
        .resolved(model, "Some Provider")
        .usage(usage);
    if ok {
        builder.ok();
    } else {
        builder.fail(502, "upstream refused".into());
    }
    let mut record = builder.finish(latency_ms);
    record.ttft_ms = ttft;
    record.cost = cost.map(|(currency, amount)| zroutery_core::billing::Cost {
        currency: currency.to_string(),
        amount,
    });
    record
}

/// A probable CLI client, the common case in these fixtures.
fn sample(client_id: &str) -> ClientFingerprint {
    ClientFingerprint {
        client_id: ClientId::trusted(client_id),
        client_name: client_id.to_string(),
        family: ClientFamily::Cli,
        source: AttributionSource::UserAgent,
        confidence: Confidence::Probable,
    }
}

#[test]
fn per_client_sums_to_the_global_totals() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");
    let codex = sample("codex");
    let curl = ClientFingerprint {
        client_id: ClientId::trusted("curl"),
        client_name: "curl".into(),
        family: ClientFamily::Http,
        source: AttributionSource::UserAgent,
        confidence: Confidence::Weak,
    };

    stats.record(record(
        &claude,
        "sonnet-class",
        Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: 40,
            cache_write_tokens: 10,
            reasoning_tokens: 3,
        },
        true,
        120,
        Some(20),
        Some(("USD", 0.01)),
    ));
    stats.record(record(
        &codex,
        "sonnet-class",
        Usage {
            input_tokens: 200,
            output_tokens: 20,
            ..Usage::default()
        },
        false,
        240,
        None,
        Some(("USD", 0.02)),
    ));
    stats.record(record(
        &curl,
        "haiku-class",
        Usage {
            input_tokens: 50,
            output_tokens: 5,
            ..Usage::default()
        },
        true,
        60,
        None,
        None,
    ));

    let summary = stats.summary();

    let per_client_requests: u64 = summary.per_client.iter().map(|c| c.requests).sum();
    let per_client_failures: u64 = summary.per_client.iter().map(|c| c.failures).sum();
    let per_client_input: u64 = summary.per_client.iter().map(|c| c.input_tokens).sum();
    let per_client_output: u64 = summary.per_client.iter().map(|c| c.output_tokens).sum();

    assert_eq!(per_client_requests, summary.requests);
    assert_eq!(per_client_failures, summary.failures);
    assert_eq!(per_client_input, summary.input_tokens);
    assert_eq!(per_client_output, summary.output_tokens);

    // And it has to agree with the model split too, or the dashboard is
    // describing two different sets of traffic.
    let per_model_requests: u64 = summary.per_model.iter().map(|m| m.requests).sum();
    assert_eq!(per_model_requests, summary.requests);
}

#[test]
fn per_client_costs_sum_per_currency_and_never_across() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");
    let codex = sample("codex");

    stats.record(record(
        &claude,
        "m",
        Usage {
            input_tokens: 10,
            output_tokens: 1,
            ..Usage::default()
        },
        true,
        10,
        None,
        Some(("USD", 1.5)),
    ));
    stats.record(record(
        &claude,
        "m",
        Usage {
            input_tokens: 10,
            output_tokens: 1,
            ..Usage::default()
        },
        true,
        10,
        None,
        Some(("CNY", 20.0)),
    ));
    stats.record(record(
        &codex,
        "m",
        Usage {
            input_tokens: 10,
            output_tokens: 1,
            ..Usage::default()
        },
        true,
        10,
        None,
        Some(("USD", 0.5)),
    ));

    let summary = stats.summary();
    let claude_row = summary
        .per_client
        .iter()
        .find(|c| c.client_id == "claude_code")
        .expect("claude row");
    let codex_row = summary
        .per_client
        .iter()
        .find(|c| c.client_id == "codex")
        .expect("codex row");

    assert!((claude_row.cost.get("USD") - 1.5).abs() < 1e-9);
    assert!((claude_row.cost.get("CNY") - 20.0).abs() < 1e-9);
    assert!((codex_row.cost.get("USD") - 0.5).abs() < 1e-9);
    assert_eq!(
        claude_row.cost.0.len(),
        2,
        "a USD amount and a CNY amount have no meaningful sum"
    );

    let summed_usd: f64 = summary.per_client.iter().map(|c| c.cost.get("USD")).sum();
    assert!(
        (summed_usd - 2.0).abs() < 1e-9,
        "USD adds up across clients"
    );
}

#[test]
fn cache_and_reasoning_columns_are_subsets_not_additions() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");

    stats.record(record(
        &claude,
        "m",
        Usage {
            // Cache-inclusive, per the Usage contract.
            input_tokens: 1200,
            output_tokens: 10,
            cache_read_tokens: 900,
            cache_write_tokens: 200,
            reasoning_tokens: 4,
        },
        true,
        10,
        None,
        None,
    ));

    let summary = stats.summary();
    let row = &summary.per_client[0];

    assert_eq!(row.input_tokens, 1200, "the total, not 1200+900+200");
    assert_eq!(row.cache_read_tokens, 900);
    assert_eq!(row.cache_write_tokens, 200);
    assert_eq!(row.output_tokens, 10, "reasoning is already inside output");
    assert_eq!(row.reasoning_tokens, 4);
    assert_eq!(row.fresh_input_tokens(), 100, "1200 - 900 - 200");
    assert_eq!(row.total_tokens(), 1210);
    assert_eq!(
        summary.input_tokens, 1200,
        "the global total must not double count the cache either"
    );
}

#[test]
fn the_attribution_split_separates_identified_from_weak_from_unknown() {
    let stats = Stats::new(500);
    let identified = sample("claude_code");
    let weak = ClientFingerprint {
        client_id: ClientId::trusted("python_requests"),
        client_name: "python_requests".into(),
        family: ClientFamily::Http,
        source: AttributionSource::UserAgent,
        confidence: Confidence::Weak,
    };

    for fingerprint in [
        &identified,
        &identified,
        &identified,
        &weak,
        &ClientFingerprint::unknown(),
        &ClientFingerprint::unknown(),
    ] {
        stats.record(record(
            fingerprint,
            "m",
            Usage::default(),
            true,
            1,
            None,
            None,
        ));
    }

    let attribution = stats.summary().attribution;
    assert_eq!(attribution.recognised, 3);
    assert_eq!(attribution.weak, 1);
    assert_eq!(attribution.unknown, 2);
    assert_eq!(attribution.total(), 6);

    assert!((attribution.recognised_share() - 0.5).abs() < 1e-9);
    assert!((attribution.weak_share() - (1.0 / 6.0)).abs() < 1e-9);
    assert!((attribution.unknown_share() - (2.0 / 6.0)).abs() < 1e-9);
}

#[test]
fn an_empty_window_reports_zero_rather_than_nan() {
    let attribution = Stats::new(10).summary().attribution;
    assert_eq!(attribution.recognised_share(), 0.0);
    assert_eq!(attribution.weak_share(), 0.0);
    assert_eq!(attribution.unknown_share(), 0.0);
}

#[test]
fn ttft_is_averaged_over_streamed_requests_only() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");

    stats.record(record(
        &claude,
        "m",
        Usage::default(),
        true,
        10,
        Some(100),
        None,
    ));
    stats.record(record(
        &claude,
        "m",
        Usage::default(),
        true,
        10,
        Some(300),
        None,
    ));
    // A non-streamed request has no first token. Averaging its absence in as a
    // zero would halve the figure and make the client look faster than it is.
    stats.record(record(
        &claude,
        "m",
        Usage::default(),
        true,
        900,
        None,
        None,
    ));

    let summary = stats.summary();
    let row = &summary.per_client[0];
    assert_eq!(row.streamed, 2);
    assert!(
        (row.avg_ttft_ms.expect("a ttft average") - 200.0).abs() < 1e-9,
        "got {:?}",
        row.avg_ttft_ms
    );
    assert!(
        (row.avg_latency_ms - (10.0 + 10.0 + 900.0) / 3.0).abs() < 1e-6,
        "latency, unlike ttft, averages over every request"
    );
}

#[test]
fn success_rate_is_computed_from_the_stored_counters() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");
    for ok in [true, true, false, true] {
        stats.record(record(&claude, "m", Usage::default(), ok, 1, None, None));
    }
    let row = &stats.summary().per_client[0];
    assert_eq!(row.requests, 4);
    assert_eq!(row.failures, 1);
    assert!((row.success_rate() - 0.75).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Cross-protocol and lifecycle
// ---------------------------------------------------------------------------

/// Every ingress must reach the same conclusion for the same caller.
///
/// A second ladder inside one protocol would be a second set of rules quietly
/// disagreeing with the first, and the symptom would be "Gemini traffic always
/// shows up as unknown" rather than anything that looks like a bug.
#[test]
fn every_ingress_attributes_the_same_caller_the_same_way() {
    let inbound = ua("claude-cli/2.0.14");
    let baseline = identify(&inbound, None);

    // The ingress is orthogonal to attribution: it is which API shape arrived,
    // not who sent it.
    for dialect in [
        Dialect::Anthropic,
        Dialect::OpenAI,
        Dialect::OpenAIResponses,
        Dialect::Gemini,
    ] {
        let mut builder = RecordBuilder::new(dialect, "m", false);
        builder.client(identify(&inbound, None));
        let record = builder.finish(1);
        assert_eq!(
            record.client, baseline,
            "{dialect:?} reached a different conclusion about the same caller"
        );
    }
}

/// A retry must not re-attribute the request.
///
/// The fingerprint is decided once at ingress and carried; deciding it per
/// attempt would let a failover change the answer for the same request.
#[test]
fn one_client_failing_over_stays_one_client() {
    let stats = Stats::new(500);
    let codex = sample("codex");

    let mut builder = RecordBuilder::new(Dialect::Anthropic, "sonnet-class", false);
    builder
        .client(codex.clone())
        .resolved("first-model", "Provider A")
        .attempt()
        .resolved("second-model", "Provider B")
        .attempt()
        .usage(Usage {
            input_tokens: 10,
            output_tokens: 2,
            ..Usage::default()
        })
        .ok();
    let record = builder.finish(50);

    assert_eq!(record.attempts, 2);
    assert_eq!(record.client, codex);
    assert_eq!(
        record.resolved_model.as_deref(),
        Some("second-model"),
        "the served model is the last attempt, but the client is unchanged"
    );

    stats.record(record);
    let summary = stats.summary();
    assert_eq!(
        summary.per_client.len(),
        1,
        "one request, one client bucket"
    );
    assert_eq!(summary.per_client[0].requests, 1);
}

/// A rejected request is still attributed.
///
/// Budget denials and unresolvable models land in `reject`, which builds its
/// own lifecycle. If that path skipped attribution, exactly the traffic a user
/// would most want to attribute — the ones being throttled — would be invisible.
#[test]
fn a_request_with_no_usage_is_still_counted_and_attributed() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");

    stats.record(record(&claude, "m", Usage::default(), false, 0, None, None));

    let summary = stats.summary();
    assert_eq!(summary.requests, 1, "a denial is still a request");
    assert_eq!(summary.failures, 1);
    assert_eq!(
        summary.per_client[0].client_id, "claude_code",
        "and it is still attributed"
    );
    assert_eq!(
        summary.per_client.iter().map(|c| c.requests).sum::<u64>(),
        summary.requests
    );
}

// ---------------------------------------------------------------------------
// Compatibility and lifecycle
// ---------------------------------------------------------------------------

/// A record written before client attribution existed must load.
///
/// The log is the one thing that has to survive a version bump; a new optional
/// field that hard-fails to deserialize would take the Activity tab down with
/// it.
#[test]
fn a_legacy_record_without_a_client_loads_as_unknown() {
    let legacy = serde_json::json!({
        "id": "req_x",
        "at": "2026-01-01T00:00:00Z",
        "ingress": "anthropic",
        "requested_model": "m",
        "resolved_model": null,
        "provider_name": null,
        "stream": false,
        "status": 200,
        "ok": true,
        "error": null,
        "latency_ms": 10,
        "ttft_ms": null,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "reasoning_tokens": 0
        },
        "cost": null,
        "attempts": 1
    });

    let record: zroutery_core::stats::RequestRecord =
        serde_json::from_value(legacy).expect("a legacy record must still load");
    assert_eq!(
        record.client,
        ClientFingerprint::unknown(),
        "and it must land in the explicit unknown bucket, not a guessed client"
    );
    assert_eq!(record.client.confidence, Confidence::Unknown);

    // And it aggregates like any other record.
    let stats = Stats::new(10);
    stats.record(record);
    let summary = stats.summary();
    assert_eq!(summary.per_client.len(), 1);
    assert_eq!(summary.per_client[0].client_id, ClientId::UNKNOWN);
    assert_eq!(summary.attribution.unknown, 1);
}

#[test]
fn clearing_resets_the_client_split_and_the_attribution_counts() {
    let stats = Stats::new(500);
    let claude = sample("claude_code");
    stats.record(record(
        &claude,
        "m",
        Usage::default(),
        true,
        1,
        None,
        Some(("USD", 1.0)),
    ));

    stats.clear();

    let summary = stats.summary();
    assert_eq!(summary.requests, 0);
    assert!(
        summary.per_client.is_empty(),
        "the split must be cleared too"
    );
    assert_eq!(summary.attribution.recognised, 0);
    assert!(summary.cost.is_empty());
}

#[test]
fn the_log_stays_bounded_while_the_client_split_does_not() {
    // The request log is a ring buffer; the aggregates are cumulative by design,
    // and their cardinality is bounded by the client id cap rather than by the
    // log limit. A record evicted from the log keeps its contribution.
    let stats = Stats::new(4);
    let claude = sample("claude_code");
    for _ in 0..10 {
        stats.record(record(&claude, "m", Usage::default(), true, 1, None, None));
    }

    assert_eq!(stats.recent(100).len(), 4, "the log is bounded");
    let summary = stats.summary();
    assert_eq!(
        summary.requests, 10,
        "but the counters describe the window, not the buffer"
    );
    assert_eq!(summary.per_client[0].requests, 10);
}

// ---------------------------------------------------------------------------
// The rule table is inspectable
// ---------------------------------------------------------------------------

#[test]
fn every_registered_rule_reports_what_it_rests_on() {
    let rules = zroutery_core::client::registered_rules();
    assert!(!rules.is_empty());

    for (id, name, _family, provenance) in rules {
        assert!(!id.is_empty(), "every rule needs an id");
        assert!(!name.is_empty(), "every rule needs a display name");
        assert!(
            matches!(
                provenance,
                zroutery_core::client::RuleProvenance::Observed
                    | zroutery_core::client::RuleProvenance::Documented
                    | zroutery_core::client::RuleProvenance::Inferred
            ),
            "{id} has to classify its own provenance"
        );
    }
}

/// The three takeover clients resolve to stable ids the stats table can key on.
///
/// The ids are shared deliberately. A takeover manifest naming `claude_code`
/// and a stats row naming `claude-code` would be one client filed under two
/// names in two places, and nothing would reconcile them.
#[test]
fn the_takeover_clients_and_the_attribution_ids_are_the_same_ids() {
    use zroutery_core::agent_takeover::AgentType;

    // The adapter's client-facing name and the attribution id share a stem, so a
    // UI can cross-reference a takeover panel with a stats row without a
    // lookup table that could fall out of step.
    for (agent, agent_name, agent_id) in [
        (AgentType::Claude, "claude", "claude_code"),
        (AgentType::Codex, "codex", "codex"),
        (AgentType::OpenCode, "opencode", "opencode"),
    ] {
        assert_eq!(
            agent.client_name(),
            agent_name,
            "{agent:?} client name drifted"
        );
        assert!(
            agent_id.starts_with(agent_name)
                || agent_name.starts_with(agent_id.trim_end_matches("_code")),
            "{agent:?}: `{agent_id}` and `{agent_name}` must be recognisably the same client"
        );
    }

    for (agent_string, expected) in [
        ("claude-cli/2.0.14", "claude_code"),
        ("codex_cli_rs/0.9.0", "codex"),
        ("opencode/0.15.3", "opencode"),
    ] {
        assert_eq!(
            identify(&ua(agent_string), None).client_id.as_str(),
            expected,
            "{agent_string}"
        );
    }
}
