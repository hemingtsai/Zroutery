//! How an inbound client identity becomes the identity presented upstream.
//!
//! A provider names one of four [`ProviderClientProfile`]s. `Auto` is the only
//! one that reads the request: it looks at the identity the pipeline carried
//! in (`x-zroutery-client`, else the User-Agent) and picks the matching fixed
//! profile. The other three are *fixed*, and that is the property these tests
//! are mostly about — a fixed profile ignores the identity altogether, which is
//! what lets it apply to requests that carry no identity of their own:
//! auxiliary model calls, balance probes, classifier and rectifier hops. If a
//! fixed profile consulted the identity, those would fall through to `Native`
//! and a gateway that only admits Claude Code would refuse them.
//!
//! `Auto` falls back to `Native` rather than guessing. An unrecognised client is
//! a client we have no fingerprint for, and presenting a wrong fingerprint is a
//! worse failure than presenting none.

#![allow(deprecated)]

use zroutery_core::{ProviderClientProfile as Profile, ProviderConfig, ProviderKind};

/// A provider on one fixed profile. `ProviderConfig::new` picks a default
/// profile per kind, so it is always overwritten here — otherwise these tests
/// would be measuring the constructor's choice rather than the resolution.
fn provider(kind: ProviderKind, profile: Profile) -> ProviderConfig {
    let mut p = ProviderConfig::new("relay", "Relay", kind);
    p.client_profile = profile;
    p
}

/// The Claude Code client's own User-Agent, as it arrives.
const CLAUDE_UA: &str = "claude-cli/2.1.217 (external, sdk-cli)";
/// The Codex client's own User-Agent, as it arrives.
const CODEX_UA: &str = "codex_cli_rs/0.1.0";

#[test]
fn auto_resolves_a_claude_client_to_the_claude_code_profile() {
    assert_eq!(
        Profile::Auto.resolve(Some(CLAUDE_UA)),
        Profile::ClaudeCode,
        "a Claude Code client talking to an Auto provider should be relayed as Claude Code"
    );
}

#[test]
fn auto_resolves_a_codex_client_to_the_codex_profile() {
    assert_eq!(
        Profile::Auto.resolve(Some(CODEX_UA)),
        Profile::Codex,
        "a Codex client talking to an Auto provider should be relayed as Codex"
    );
}

#[test]
fn auto_resolves_an_unknown_client_to_the_native_profile() {
    // curl, an SDK written by someone else, a browser: none of these have a
    // fingerprint we can present honestly.
    for identity in [
        "curl/8.6.0",
        "python-requests/2.32.3",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
        "",
    ] {
        assert_eq!(
            Profile::Auto.resolve(Some(identity)),
            Profile::Native,
            "an unrecognised client {identity:?} should fall back to the native identity, \
             not to a fingerprint we would be inventing"
        );
    }
}

#[test]
fn auto_resolves_a_request_with_no_identity_to_the_native_profile() {
    assert_eq!(
        Profile::Auto.resolve(None),
        Profile::Native,
        "no identity at all is the same case as an unrecognised one"
    );
}

#[test]
fn a_fixed_profile_ignores_the_identity() {
    // The load-bearing property. A provider pinned to `Native` relays as
    // Zroutery even when a Claude Code client is on the other end, and one
    // pinned to `Codex` relays as Codex even when a Claude Code client is.
    let native = provider(ProviderKind::OpenAICompatible, Profile::Native);
    assert_eq!(
        native.client_profile_for(Some(CLAUDE_UA)),
        Profile::Native,
        "a fixed profile must not be re-decided by the request"
    );

    let codex = provider(ProviderKind::OpenAICompatible, Profile::Codex);
    assert_eq!(
        codex.client_profile_for(Some(CLAUDE_UA)),
        Profile::Codex,
        "a fixed profile must not be re-decided by the request"
    );

    // `Native` is not the only fixed profile that holds; the other end of the
    // identity space resolves the same way.
    let claude = provider(ProviderKind::OpenAICompatible, Profile::ClaudeCode);
    assert_eq!(
        claude.client_profile_for(Some(CODEX_UA)),
        Profile::ClaudeCode
    );
}

#[test]
fn a_fixed_profile_is_unaffected_by_a_missing_identity() {
    // Auxiliary and probe requests carry no inbound identity. If a fixed
    // profile needed one it would be unusable there, which is why resolution
    // short-circuits before the identity is even looked at.
    for profile in [Profile::Native, Profile::ClaudeCode, Profile::Codex] {
        let p = provider(ProviderKind::OpenAICompatible, profile);
        assert_eq!(
            p.client_profile_for(None),
            profile,
            "{profile:?} is fixed and must survive a request with no identity"
        );
    }
}

#[test]
fn the_deprecated_boolean_outranks_an_auto_profile() {
    // A configuration written before `client_profile` existed deserialises with
    // `client_profile: Auto` — the struct default — and only the legacy boolean
    // says the operator wanted Claude Code. If the boolean lost, every old
    // install would silently stop presenting its fingerprint on the next
    // restart, with nothing in the config to show why.
    let mut p = provider(ProviderKind::Anthropic, Profile::Auto);
    #[allow(deprecated)]
    {
        p.impersonate_claude_code = true;
    }

    for identity in [None, Some(CLAUDE_UA), Some(CODEX_UA), Some("curl/8.6.0")] {
        assert_eq!(
            p.client_profile_for(identity),
            Profile::ClaudeCode,
            "the legacy switch is the only thing that says Claude Code here; \
             it must win for every inbound identity, including none"
        );
    }
}

#[test]
fn the_identity_match_is_case_insensitive() {
    // The identity is an opaque header a client chose to send; casing is not
    // something a caller can be relied on to hold constant.
    for identity in ["CLAUDE-CLI/1.0", "Claude-CLI/1.0", "cLaUdE"] {
        assert_eq!(
            Profile::Auto.resolve(Some(identity)),
            Profile::ClaudeCode,
            "{identity:?} names Claude Code whatever its casing"
        );
    }
    for identity in ["CODEX_CLI_RS/0.1.0", "Codex_CLI_RS", "coDeX"] {
        assert_eq!(
            Profile::Auto.resolve(Some(identity)),
            Profile::Codex,
            "{identity:?} names Codex whatever its casing"
        );
    }
}

// The identity is classified by its leading client token, so an identity that
// names one client up front and another only in a trailing compatibility string
// resolves to the one up front. This is the case a `contains` rule got backwards.
#[test]
fn an_identity_is_classified_by_its_leading_client_token() {
    // A relay or compatibility shim that advertises itself as a Codex client
    // while naming Claude Code in a compatibility string is asking to be relayed
    // as Codex. A substring rule ordered `claude` before `codex` returned Claude
    // Code instead; the leading token is what the wire actually carries.
    let codex_first = "codex_cli_rs/0.1.0 (claude-code-compat)";
    assert_eq!(
        Profile::Auto.resolve(Some(codex_first)),
        Profile::Codex,
        "{codex_first:?} names Codex first, so it is a Codex client"
    );

    // The converse holds for the same reason, so the rule is "leading token",
    // not "prefer Codex".
    let claude_first = "claude-cli/2.1.217 (codex-compat)";
    assert_eq!(
        Profile::Auto.resolve(Some(claude_first)),
        Profile::ClaudeCode,
        "{claude_first:?} names Claude Code first, so it is a Claude Code client"
    );

    // A client named only inside a trailing string, with nothing leading, is not
    // that client. A browser that merely mentions one in its comment is native.
    assert_eq!(
        Profile::Auto.resolve(Some("Mozilla/5.0 (claude-cli compat)")),
        Profile::Native,
        "a client token that is not leading is not the client's identity"
    );
}

#[test]
fn anthropic_providers_default_to_the_claude_code_profile() {
    // The constructor's default, which is what every provider added in the GUI
    // starts on. Anthropic-first because a relay imported from CC Switch is the
    // common case and its gateway admits Claude Code only; everything else
    // starts on Auto so an ordinary OpenAI-compatible relay is relayed as the
    // client that is actually calling.
    let anthropic = ProviderConfig::new("anth", "Anthropic", ProviderKind::Anthropic);
    assert_eq!(
        anthropic.client_profile,
        Profile::ClaudeCode,
        "a new Anthropic provider should present the Claude Code fingerprint"
    );

    let openai = ProviderConfig::new("oai", "OpenAI", ProviderKind::OpenAICompatible);
    assert_eq!(
        openai.client_profile,
        Profile::Auto,
        "a new OpenAI-compatible provider should follow the calling client"
    );
}

#[test]
fn every_profile_round_trips_through_its_own_name() {
    // `as_str` and the serde rename have to agree, because `as_str` is what the
    // UI writes and serde is what reads the file back. If they drift, a
    // profile saved as `claudeCode` and read back as an unknown value would
    // fail to parse — or worse, parse as a different profile.
    for (profile, name) in [
        (Profile::Auto, "auto"),
        (Profile::Native, "native"),
        (Profile::ClaudeCode, "claude_code"),
        (Profile::Codex, "codex"),
    ] {
        assert_eq!(
            profile.as_str(),
            name,
            "{profile:?} is written to the config file as `as_str`, and the spelling is \
             part of the file format"
        );
        assert_eq!(
            serde_json::from_value::<Profile>(serde_json::json!(name)).unwrap(),
            profile,
            "{name:?} should deserialise back to {profile:?}; these are the strings that \
             appear in every user's config file"
        );
        assert_eq!(
            serde_json::to_string(&profile).unwrap(),
            format!("\"{name}\""),
            "the serialised form and `as_str` must be the same spelling"
        );
    }
}

#[test]
fn an_unknown_profile_name_is_rejected_rather_than_defaulted() {
    // The other half of the round trip: a typo in a config file has to be
    // visible. Silently becoming `Auto` would turn a provider the operator
    // pinned into one that follows whatever client calls it.
    assert!(
        serde_json::from_str::<Profile>("\"claudecode\"").is_err(),
        "an unrecognised profile name should fail to parse"
    );
}
