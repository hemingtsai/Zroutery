//! The headers each client profile puts on the wire.
//!
//! `build_headers_for_profile` is the one place a request's identity is turned
//! into bytes an upstream gateway can read. Two rules are being defended here.
//!
//! First, the identity must be a *fingerprint*, complete enough that a gateway
//! checking for a specific client accepts it, and minimal enough that nothing
//! in it is inconsistent — version strings that disagree with each other are
//! exactly what a strict gateway looks for.
//!
//! Second, the fingerprint is orthogonal to authentication. Every profile has
//! to carry the same credentials the provider would otherwise send, because a
//! profile is a statement about who is talking, not about whether the request
//! is allowed. A profile that dropped `x-api-key` would fail as an auth error
//! while the real cause was a header merge, which is an expensive thing to
//! debug from a 401.

// The deprecated `impersonate_claude_code` switch is deliberately exercised
// below: the two-argument `build_headers` is what existing callers use, and it
// has to keep working for a config that has not been normalised yet.
#![allow(deprecated)]

use reqwest::header::HeaderMap;
use zroutery_core::upstream::{build_headers, build_headers_for_profile};
use zroutery_core::{ProviderClientProfile as Profile, ProviderConfig, ProviderKind};

/// The four profiles, so "under every profile" means every profile rather than
/// whichever two somebody remembered to write down.
const ALL: [Profile; 4] = [
    Profile::Auto,
    Profile::Native,
    Profile::ClaudeCode,
    Profile::Codex,
];

const KEY: &str = "sk-test-key";

/// A provider on one profile. `ProviderConfig::new` chooses a profile per kind,
/// so it is always overwritten — otherwise these tests would be measuring the
/// constructor's default rather than the profile under test.
fn provider(kind: ProviderKind, profile: Profile) -> ProviderConfig {
    let mut p = ProviderConfig::new("relay", "Relay", kind);
    p.client_profile = profile;
    p
}

fn anthropic(profile: Profile) -> ProviderConfig {
    provider(ProviderKind::Anthropic, profile)
}

fn openai_compatible(profile: Profile) -> ProviderConfig {
    provider(ProviderKind::OpenAICompatible, profile)
}

/// The header's value as text, or `None` if the header was not set at all.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Every header as sorted `(lowercase name, value)` text pairs.
///
/// Compared instead of the `HeaderMap` itself so that a difference anywhere in
/// the request shows up, not only in the two or three headers a hand-written
/// assertion happened to name.
fn flattened(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or("<non-text>").to_string(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// The names a client fingerprint can present itself under. `Native` is the
/// absence of all of them.
const FINGERPRINT_HEADERS: [&str; 4] = ["user-agent", "x-app", "anthropic-beta", "originator"];

#[test]
fn the_claude_code_profile_presents_a_claude_cli_fingerprint() {
    let headers = build_headers_for_profile(
        &anthropic(Profile::ClaudeCode),
        Some(KEY),
        Profile::ClaudeCode,
    )
    .expect("an Anthropic provider builds headers");

    // The version in the User-Agent moves with the CLI, so the test asserts the
    // shape rather than pinning a version that will be wrong by next quarter.
    assert!(
        header(&headers, "user-agent").is_some_and(|ua| ua.starts_with("claude-cli/")),
        "a Claude Code profile should present the claude-cli User-Agent; got {:?}",
        header(&headers, "user-agent")
    );
    assert_eq!(
        header(&headers, "x-app"),
        Some("cli"),
        "x-app is how the CLI marks itself as a CLI client"
    );
    assert_eq!(
        header(&headers, "anthropic-dangerous-direct-browser-access"),
        Some("true"),
        "the real CLI sends this; a gateway that checks for it rejects requests without it"
    );
    assert!(
        header(&headers, "anthropic-beta")
            .is_some_and(|beta| beta.contains("claude-code-20250219")),
        "the claude-code beta marker is the versioned half of the fingerprint; got {:?}",
        header(&headers, "anthropic-beta")
    );
}

#[test]
fn the_codex_profile_presents_a_codex_fingerprint() {
    let headers = build_headers_for_profile(
        &openai_compatible(Profile::Codex),
        Some(KEY),
        Profile::Codex,
    )
    .expect("an OpenAI-compatible provider builds headers");

    assert!(
        header(&headers, "user-agent").is_some_and(|ua| ua.starts_with("codex_cli_rs/")),
        "a Codex profile should present the codex_cli_rs User-Agent; got {:?}",
        header(&headers, "user-agent")
    );
    assert_eq!(
        header(&headers, "originator"),
        Some("codex_cli_rs"),
        "originator is how the Codex client identifies itself to a relay"
    );
}

#[test]
fn the_profiles_do_not_leak_into_each_other() {
    // A Codex provider must not also claim to be a CLI-client Claude Code
    // request. Both fingerprints present at once is the inconsistent-header
    // case a strict gateway exists to reject, so it is worth asserting the
    // profiles are exclusive rather than accumulating.
    let codex = build_headers_for_profile(
        &openai_compatible(Profile::Codex),
        Some(KEY),
        Profile::Codex,
    )
    .unwrap();
    assert_eq!(
        header(&codex, "x-app"),
        None,
        "the Codex profile has no x-app; that marker belongs to Claude Code"
    );
    assert_eq!(
        header(&codex, "anthropic-beta"),
        None,
        "the Codex profile sends no anthropic-beta"
    );

    let claude = build_headers_for_profile(
        &anthropic(Profile::ClaudeCode),
        Some(KEY),
        Profile::ClaudeCode,
    )
    .unwrap();
    assert_eq!(
        header(&claude, "originator"),
        None,
        "the Claude Code profile sends no codex originator"
    );
}

#[test]
fn the_native_profile_presents_no_fingerprint_at_all() {
    // The fallback has to be visibly absent, not merely different. Presenting
    // nothing is an honest answer for an unknown client; presenting the wrong
    // thing is a lie a gateway can catch.
    let headers =
        build_headers_for_profile(&anthropic(Profile::Native), Some(KEY), Profile::Native)
            .expect("an Anthropic provider builds headers");

    for name in FINGERPRINT_HEADERS {
        assert_eq!(
            header(&headers, name),
            None,
            "`{name}` is a fingerprint and Native is the absence of one; got {:?}",
            header(&headers, name)
        );
    }
    assert_eq!(
        header(&headers, "anthropic-dangerous-direct-browser-access"),
        None,
        "that header is part of the Claude Code fingerprint too"
    );

    // What is left is the protocol, not the identity: a native request still
    // has to be a well-formed HTTP request.
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/json"),
        "Native withholds an identity, not the request"
    );
    assert_eq!(
        header(&headers, "anthropic-version"),
        Some("2023-06-01"),
        "an Anthropic provider still gets its protocol version under Native"
    );
}

#[test]
fn the_claude_code_profile_merges_with_an_anthropic_beta_from_extra_headers() {
    // `extra_headers` is the documented escape hatch for gateways that need a
    // beta Zroutery does not know about. If the Claude Code branch *replaced*
    // the header instead of merging, that escape hatch would stop working for
    // exactly the users who need it, and the loss would be invisible: the
    // request still succeeds against every gateway that does not check.
    let mut p = anthropic(Profile::ClaudeCode);
    p.extra_headers
        .insert("anthropic-beta".into(), "prompt-caching-2024-07-31".into());

    let headers =
        build_headers_for_profile(&p, Some(KEY), Profile::ClaudeCode).expect("headers build");

    let beta = header(&headers, "anthropic-beta").expect("anthropic-beta is set");
    assert!(
        beta.contains("claude-code-20250219"),
        "the operator's own beta must not cost them the Claude Code marker; got: {beta}"
    );
    assert!(
        beta.contains("prompt-caching-2024-07-31"),
        "the operator's own beta must survive the profile; got: {beta}"
    );
    assert_eq!(
        beta.matches("claude-code-20250219").count(),
        1,
        "a marker sent twice reads as two features, not one; got: {beta}"
    );
}

#[test]
fn the_claude_code_marker_is_not_duplicated_when_it_is_already_present() {
    // An operator who has been running Claude Code through a gateway may well
    // have pinned the marker themselves. Merging blindly would produce
    // `claude-code-20250219,claude-code-20250219` — a duplicate that a strict
    // gateway can reject, and one the operator cannot see, because their own
    // config looks untouched.
    let already = "claude-code-20250219,fine-grained-tool-streaming-2025-05-14";
    let mut p = anthropic(Profile::ClaudeCode);
    p.extra_headers
        .insert("anthropic-beta".into(), already.into());

    let headers =
        build_headers_for_profile(&p, Some(KEY), Profile::ClaudeCode).expect("headers build");

    assert_eq!(
        header(&headers, "anthropic-beta"),
        Some(already),
        "a beta list that already carries the marker must be passed through unchanged"
    );
}

#[test]
fn anthropic_auth_is_an_api_key_under_every_profile() {
    // Auth and identity are separate concerns. A profile says who is talking;
    // it must not be able to remove the credential that says the request is
    // allowed.
    for profile in ALL {
        let headers = build_headers_for_profile(&anthropic(profile), Some(KEY), profile)
            .expect("headers build");
        assert_eq!(
            header(&headers, "x-api-key"),
            Some(KEY),
            "{profile:?} must still send the Anthropic credential"
        );
        assert_eq!(
            header(&headers, "authorization"),
            None,
            "{profile:?} must not add a Bearer header nobody asked for — some relays reject a \
             request carrying both"
        );
    }
}

#[test]
fn bearer_auth_adds_authorization_for_an_anthropic_relay_under_every_profile() {
    // Some Anthropic-protocol relays read Authorization and reject a request
    // carrying only `x-api-key`. Both headers are sent when the operator asks
    // for it, and that choice has to survive whichever profile is in play.
    for profile in ALL {
        let mut p = anthropic(profile);
        p.bearer_auth = true;

        let headers = build_headers_for_profile(&p, Some(KEY), profile).expect("headers build");
        let bearer = format!("Bearer {KEY}");

        assert_eq!(
            header(&headers, "x-api-key"),
            Some(KEY),
            "{profile:?}: bearer_auth adds a header, it does not replace one"
        );
        assert_eq!(
            header(&headers, "authorization"),
            Some(bearer.as_str()),
            "{profile:?}: the relay reads the Bearer header"
        );
    }
}

#[test]
fn openai_compatible_auth_is_a_bearer_token_under_every_profile() {
    for profile in ALL {
        let headers = build_headers_for_profile(&openai_compatible(profile), Some(KEY), profile)
            .expect("headers build");
        let bearer = format!("Bearer {KEY}");
        assert_eq!(
            header(&headers, "authorization"),
            Some(bearer.as_str()),
            "{profile:?} must still send the OpenAI-compatible credential"
        );
        assert_eq!(
            header(&headers, "x-api-key"),
            None,
            "{profile:?}: an Anthropic header on an OpenAI-compatible request is a mistake the \
             gateway should not have to ignore"
        );
    }
}

#[test]
fn an_invalid_extra_header_name_is_still_rejected() {
    // The profile branches are added after the `extra_headers` loop, so a bad
    // header name has to fail on its own rather than being masked by whatever
    // the profile does next. A silently dropped header is a debugging session.
    let mut p = anthropic(Profile::ClaudeCode);
    p.extra_headers.insert("bad header name".into(), "v".into());

    let err = build_headers_for_profile(&p, Some(KEY), Profile::ClaudeCode)
        .expect_err("an invalid header name must not be accepted");
    let message = err.to_string();
    assert!(
        message.contains("invalid header name"),
        "the error should say what is wrong and which header; got: {message}"
    );
}

#[test]
fn the_legacy_builder_resolves_the_providers_fixed_profile() {
    // `build_headers(provider, key)` is what existing callers use — the e2e
    // suite calls it for every provider it probes. It has to keep resolving the
    // provider's own profile, with no identity, which for a fixed profile is
    // the profile itself. Getting this wrong would silently strip the
    // fingerprint off probes and auxiliary calls that have no request to read
    // an identity from.
    for profile in [Profile::Native, Profile::ClaudeCode, Profile::Codex] {
        let p = anthropic(profile);
        let legacy = build_headers(&p, Some(KEY)).expect("the two-argument builder still builds");
        let resolved =
            build_headers_for_profile(&p, Some(KEY), p.client_profile_for(None)).expect("builds");

        assert_eq!(
            flattened(&legacy),
            flattened(&resolved),
            "{profile:?}: build_headers is defined as the explicitly resolved profile with no \
             identity, and for a fixed profile the two must be byte-identical"
        );

        match profile {
            Profile::ClaudeCode => assert_eq!(
                header(&legacy, "x-app"),
                Some("cli"),
                "a Claude Code provider probed through the legacy builder still fingerprints"
            ),
            Profile::Native => {
                for name in FINGERPRINT_HEADERS {
                    assert_eq!(
                        header(&legacy, name),
                        None,
                        "{name} is a fingerprint and Native is the absence of one"
                    );
                }
            }
            Profile::Codex => assert_eq!(
                header(&legacy, "originator"),
                Some("codex_cli_rs"),
                "a Codex provider probed through the legacy builder still fingerprints"
            ),
            Profile::Auto => unreachable!("Auto is not a fixed profile; see the test below"),
            Profile::OpenCodeCli => {
                unreachable!("OpenCodeCli is a gateway target, not in this fixed-profile set")
            }
        }
    }
}

#[test]
fn the_legacy_builder_still_honours_the_deprecated_switch() {
    // A configuration still in memory from before `normalize()` ran carries the
    // old boolean and nothing else. The builder is called on such providers, so
    // it has to produce the Claude Code fingerprint for them — this is the path
    // that keeps an un-normalized config from quietly losing its identity.
    let mut p = provider(ProviderKind::Anthropic, Profile::Auto);
    #[allow(deprecated)]
    {
        p.impersonate_claude_code = true;
    }

    let headers = build_headers(&p, Some(KEY)).expect("headers build");

    assert_eq!(
        header(&headers, "x-app"),
        Some("cli"),
        "the legacy switch still has to produce a fingerprint before normalization runs"
    );
    assert_eq!(header(&headers, "x-api-key"), Some(KEY));
}

#[test]
fn the_legacy_builder_sends_no_identity_so_auto_resolves_to_native() {
    // The converse, and the reason the two-argument builder is not enough on
    // its own: it has no request to read an identity from, so `Auto` resolves
    // to `Native`. Request paths therefore use the profile-aware builder and
    // resolve the identity first — which is why the resolved profile has to
    // agree with what `client_profile_for` would have chosen.
    let p = anthropic(Profile::Auto);

    let headers = build_headers(&p, Some(KEY)).expect("headers build");

    assert_eq!(
        p.client_profile_for(None),
        Profile::Native,
        "no identity means the native profile"
    );
    for name in FINGERPRINT_HEADERS {
        assert_eq!(
            header(&headers, name),
            None,
            "`{name}` must not appear for an Auto provider reached with no identity"
        );
    }
}
