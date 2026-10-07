//! The one-way migration from the legacy impersonation switch to the profile.
//!
//! `impersonate_claude_code: true` meant "present the Claude Code fingerprint".
//! `client_profile: "claude_code"` means the same thing and also means it when
//! the request carries no identity, which is why it is the better encoding —
//! but every config file already on disk spells it the old way. `normalize()`
//! is where that has to happen, because `normalize()` is documented as
//! "call after every deserialization" and is the one place a config is already
//! allowed to move fields around.
//!
//! Two things have to hold at once and they pull against each other. The
//! migration must fire for the old documents, or they silently lose their
//! fingerprint. And it must fire *only* for them, or it overwrites a profile
//! somebody chose deliberately. These tests pin both, plus the note, because a
//! migration that changes configuration without saying so is indistinguishable
//! from one that changes it wrongly.

#![allow(deprecated)]

use zroutery_core::{AppConfig, ProviderClientProfile as Profile, ProviderConfig, ProviderKind};

/// One provider on one profile, with the legacy switch left as the caller wants
/// it. The switch is the only awkward part of `ProviderConfig`: it is
/// deprecated and skipped on serialisation, which is exactly why it has to be
/// migrated rather than merely honoured.
fn config_with(provider: ProviderConfig) -> AppConfig {
    AppConfig {
        providers: vec![provider],
        ..Default::default()
    }
}

fn provider(profile: Profile, legacy_switch: bool) -> ProviderConfig {
    let mut p = ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
    p.client_profile = profile;
    #[allow(deprecated)]
    {
        p.impersonate_claude_code = legacy_switch;
    }
    p
}

#[test]
fn a_legacy_impersonation_switch_migrates_to_the_claude_code_profile() {
    let mut cfg = config_with(provider(Profile::Auto, true));

    cfg.normalize();

    let migrated = &cfg.providers[0];
    assert_eq!(
        migrated.client_profile,
        Profile::ClaudeCode,
        "the legacy switch and the Claude Code profile mean the same thing, so normalizing \
         must land on the profile"
    );
    assert!(
        !migrated.impersonate_claude_code,
        "the switch has to be cleared, or a later edit that sets `client_profile` back to \
         Auto would be overridden by a boolean left behind from the old file"
    );
}

#[test]
fn the_migration_note_names_the_provider() {
    // The note exists so the UI can explain itself. A note that does not say
    // *which* provider changed leaves the operator looking at a config that
    // differs from the one they wrote, with nothing to connect the two.
    let mut cfg = config_with(provider(Profile::Auto, true));
    cfg.providers[0].name = "CC Switch Relay".into();

    let notes = cfg.normalize();

    assert_eq!(
        notes.len(),
        1,
        "one provider migrated and one provider changed, so there is one thing to explain; \
         got {notes:?}"
    );
    let note = &notes[0];
    for expected in ["CC Switch Relay", "client profile"] {
        assert!(
            note.contains(expected),
            "the note should name {expected:?} so the reader knows what moved and where it \
             went; got: {note}"
        );
    }
}

#[test]
fn a_provider_already_on_the_claude_code_profile_is_left_alone() {
    // The no-op case, and the one that decides whether the migration is
    // survivable. A document that already says `claude_code` has nothing to
    // migrate, and reporting a change that did not happen is how a migration
    // note stops being read.
    let mut cfg = config_with(provider(Profile::ClaudeCode, false));

    let notes = cfg.normalize();

    assert_eq!(
        cfg.providers[0].client_profile,
        Profile::ClaudeCode,
        "there was nothing to migrate, so the profile must not move"
    );
    assert!(
        notes.is_empty(),
        "nothing changed, so there is nothing to report; a note here would train the reader \
         to ignore notes. Got: {notes:?}"
    );
}

#[test]
fn the_migration_does_not_clobber_an_explicit_codex_profile() {
    // The dangerous neighbour. `Codex` is a profile that also means "not
    // Claude Code", so a migration written as an unconditional assignment
    // rather than a guarded one would rewrite it to `ClaudeCode` and the
    // operator's provider would start presenting a fingerprint they never asked
    // for.
    let mut cfg = config_with(provider(Profile::Codex, false));

    cfg.normalize();

    assert_eq!(
        cfg.providers[0].client_profile,
        Profile::Codex,
        "a provider that already names Codex keeps naming Codex"
    );
}

#[test]
fn a_legacy_document_migrates_from_deserialisation_through_normalize() {
    // The path that actually matters: the field arrives as text on disk, and
    // `normalize()` is what the loader calls afterwards. Exercising
    // `ProviderConfig` directly would skip the half most likely to be wrong.
    let mut cfg: AppConfig = serde_json::from_str(
        r#"{
            "providers": [
                { "id": "relay", "name": "Legacy Relay", "kind": "anthropic",
                  "base_url": "https://relay.example/v1",
                  "impersonate_claude_code": true }
            ]
        }"#,
    )
    .expect("a document written before `client_profile` existed still parses");

    // Before normalization the boolean is the only statement of intent, and the
    // profile is still the struct default.
    assert_eq!(
        cfg.providers[0].client_profile,
        Profile::Auto,
        "an old document says nothing about the profile, so the default is Auto and the \
         boolean is the only thing carrying the operator's choice"
    );

    cfg.normalize();

    assert_eq!(cfg.providers[0].client_profile, Profile::ClaudeCode);
    assert_eq!(
        cfg.providers[0].client_profile_for(None),
        Profile::ClaudeCode
    );
}

#[test]
fn the_migrated_profile_is_what_gets_written_back_to_disk() {
    // The end state on disk is the one an operator will read back months later,
    // and it is the state a second `normalize()` has to be idempotent against.
    // The legacy field is `skip_serializing`, so it must not reappear; the
    // profile has to be there in the exact spelling the parser accepts.
    let mut cfg = config_with(provider(Profile::Auto, true));
    cfg.normalize();

    let saved = serde_json::to_string(&cfg).expect("a normalized config serialises");
    let provider_json = &serde_json::to_value(&cfg).unwrap()["providers"][0];

    assert_eq!(
        provider_json["client_profile"], "claude_code",
        "what is written back is what will be read next time; got: {saved}"
    );
    assert!(
        provider_json.get("impersonate_claude_code").is_none(),
        "the deprecated switch must not be written back at all; got: {saved}"
    );

    // And the round trip is stable: a second pass changes nothing and says
    // nothing, so saving repeatedly does not accumulate notes.
    let mut reloaded: AppConfig = serde_json::from_str(&saved).expect("the saved config parses");
    let notes = reloaded.normalize();
    assert_eq!(
        reloaded.providers[0].client_profile,
        Profile::ClaudeCode,
        "a reloaded config is already migrated and must not drift"
    );
    assert!(
        notes.is_empty(),
        "normalization has to be idempotent, or every save reports a migration that already \
         happened; got: {notes:?}"
    );
}

#[test]
fn only_the_migrated_provider_is_reported() {
    // One note per provider that actually moved. A document with a mix of old
    // and new spellings is the normal state after someone edits by hand, and
    // naming all of them would bury the one that needs attention.
    let mut cfg = config_with(provider(Profile::Auto, true));
    cfg.providers.push(provider(Profile::Codex, false));
    cfg.providers[1].name = "Already Migrated".into();

    let notes = cfg.normalize();

    assert_eq!(
        notes.len(),
        1,
        "only the provider carrying the legacy switch moved; got: {notes:?}"
    );
    assert!(
        notes[0].contains("Relay"),
        "the note should be about the provider that migrated; got: {}",
        notes[0]
    );
    assert_eq!(cfg.providers[1].client_profile, Profile::Codex);
}
