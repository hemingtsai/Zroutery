//! Staged publication of inference keys, against an isolated tempdir and a
//! fake credential backend.
//!
//! Nothing here touches the real keychain, the user's configuration or a
//! running gateway: every `Desktop` is built fresh on a temporary directory,
//! its credential store answers from a `FakeBackend` the test can inspect, and
//! the helpers under test are the same functions the Tauri commands call.
//! Save failures are produced by making the configuration file path a
//! directory, so `store::save`'s atomic rename cannot land — the deterministic,
//! platform-honest version of a full disk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use zroutery_core::billing::{EffectiveRate, Pricing, RateSource, RATE_SCHEMA_VERSION};
use zroutery_core::config::{
    AccountConfig, AppConfig, InferenceKeyConfig, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::Dialect;

use super::publication::{
    apply_inference_key_change, expire_key_rates, fresh_reference, remove_inference_key,
};
use crate::secrets::{CredentialBackend, KeychainSecrets, StoreError};
use crate::state::Desktop;
use zroutery_core::runtime_store::{InferencePublication, INFERENCE_PUBLICATION_PUBLISHED};

const OLD_REF: &str = "provider:relay:account:a:inference:k1";
const SHARED_REF: &str = "vault:shared-pool";
const UNIQUE_REF: &str = "provider:relay:account:a1:inference:k1";

/// An in-memory credential store. The map is shared with the test, which is
/// the only reason an entry written inside the publication can be asserted on.
#[derive(Default)]
struct FakeBackend {
    entries: Arc<Mutex<BTreeMap<String, String>>>,
}

impl FakeBackend {
    /// A store already holding one secret. The returned `Arc` is the same map
    /// the backend owns, so the test reads the store of record, not a copy.
    fn seeded(key_ref: &str, secret: &str) -> Self {
        let backend = FakeBackend::default();
        backend
            .entries
            .lock()
            .unwrap()
            .insert(key_ref.to_string(), secret.to_string());
        backend
    }
}

impl CredentialBackend for FakeBackend {
    fn get(&self, key_ref: &str) -> Result<String, StoreError> {
        self.entries
            .lock()
            .unwrap()
            .get(key_ref)
            .cloned()
            .ok_or(StoreError::NoEntry)
    }

    fn set(&self, key_ref: &str, secret: &str) -> Result<(), StoreError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key_ref.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, key_ref: &str) -> Result<(), StoreError> {
        self.entries.lock().unwrap().remove(key_ref);
        Ok(())
    }
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zroutery-inference-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A credential store that accepts reads and deletes but refuses every write,
/// which is the shape of a locked or full keychain during publication.
#[derive(Default)]
struct SetRefusingBackend;

impl CredentialBackend for SetRefusingBackend {
    fn get(&self, _key_ref: &str) -> Result<String, StoreError> {
        Err(StoreError::NoEntry)
    }

    fn set(&self, _key_ref: &str, _secret: &str) -> Result<(), StoreError> {
        Err(StoreError::Backend("the credential store is locked".into()))
    }

    fn delete(&self, _key_ref: &str) -> Result<(), StoreError> {
        Ok(())
    }
}

/// Write a pending publication row into a not-yet-opened runtime database, the
/// way a process death between the staging write and the save would leave it.
fn journal_before_startup(dir: &std::path::Path, publication: &InferencePublication) {
    let store =
        zroutery_core::runtime_store::RuntimeStore::open(&dir.join("runtime.sqlite3")).unwrap();
    store.journal_inference_publication(publication).unwrap();
}

fn pending_publications(desktop: &Desktop) -> usize {
    desktop
        .runtime
        .as_ref()
        .unwrap()
        .pending_inference_publications()
        .unwrap()
        .len()
}

fn key(id: &str, key_ref: &str) -> InferenceKeyConfig {
    InferenceKeyConfig {
        id: id.to_string(),
        key_ref: key_ref.to_string(),
        enabled: true,
        priority: 0,
        note: String::new(),
    }
}

fn account(id: &str, keys: Vec<InferenceKeyConfig>) -> AccountConfig {
    AccountConfig {
        account_id: id.to_string(),
        key_ref: format!("provider:relay:account:{id}"),
        enabled: true,
        inference_keys: keys,
        ..AccountConfig::default()
    }
}

fn config_with(accounts: Vec<AccountConfig>) -> AppConfig {
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    provider.accounts = accounts;
    let mut config = AppConfig::default();
    // `validate_and_save` refuses a document the gateway would reject every
    // request for, so the fixture carries the token startup would have filled
    // in (`store::with_defaults`); a fixed value keeps failures readable.
    config.server.auth_token = "zr-test-token".to_string();
    config.providers.push(provider);
    config
}

/// What a fixture hands back: the desktop, the credential store of record and
/// the temporary directory to clean up afterwards.
type Fixture = (Arc<Desktop>, Arc<Mutex<BTreeMap<String, String>>>, PathBuf);

/// A desktop whose configuration saves work and whose secrets live in the fake
/// backend. The directory is returned so the test can clean up after itself.
fn desktop_with(config: AppConfig) -> Fixture {
    let dir = tempdir();
    let backend = FakeBackend::default();
    let entries = Arc::clone(&backend.entries);
    let secrets = Arc::new(KeychainSecrets::with_backend(Box::new(backend)));
    let desktop = Arc::new(Desktop::new(dir.clone(), config, secrets));
    (desktop, entries, dir)
}

/// A desktop whose configuration saves always fail: `config.json` is a
/// directory, so the atomic rename at the end of `store::save` cannot land.
/// Everything before that point — keychain writes, validation, staging — is
/// fully functional, which is exactly the failure shape the compensation has
/// to survive.
fn desktop_with_blocked_save(config: AppConfig, key_ref: &str, secret: &str) -> Fixture {
    let dir = tempdir();
    std::fs::create_dir_all(dir.join(crate::store::FILE_NAME)).unwrap();
    let backend = FakeBackend::seeded(key_ref, secret);
    let entries = Arc::clone(&backend.entries);
    let secrets = Arc::new(KeychainSecrets::with_backend(Box::new(backend)));
    let desktop = Arc::new(Desktop::new(dir.clone(), config, secrets));
    (desktop, entries, dir)
}

/// A desktop whose credential store refuses every write, leaving staging
/// impossible but the journal fully functional.
fn desktop_with_refusing_set(config: AppConfig) -> (Arc<Desktop>, PathBuf) {
    let dir = tempdir();
    let secrets = Arc::new(KeychainSecrets::with_backend(Box::new(SetRefusingBackend)));
    let desktop = Arc::new(Desktop::new(dir.clone(), config, secrets));
    (desktop, dir)
}

fn rate(id: &str, key_id: Option<&str>) -> EffectiveRate {
    EffectiveRate {
        schema_version: RATE_SCHEMA_VERSION,
        id: id.to_string(),
        upstream_model: "m".to_string(),
        dialect: Dialect::OpenAI,
        binding_version: "binding-1".to_string(),
        inference_key_id: key_id.map(str::to_string),
        group: Some("svip".to_string()),
        source: RateSource::VerifiedEffective,
        observed_at: 1,
        expires_at: None,
        evidence: "test evidence".to_string(),
        pricing: Pricing::new("USD", 1.0, 2.0),
    }
}

/// The two committed unit tests for the reference shape and id validation,
/// kept in the owned test module.
#[test]
fn an_inference_reference_never_collides_with_a_panel_reference() {
    assert_eq!(
        super::inference_key_ref("p", "a", "k1"),
        "provider:p:account:a:inference:k1"
    );
    assert_ne!(
        super::inference_key_ref("p", "a", "k1"),
        "provider:p:account:a"
    );
    assert_ne!(super::inference_key_ref("p", "a", "k1"), "provider:p");
}

#[test]
fn key_ids_are_trimmed_and_reject_separators() {
    assert_eq!(super::validated_key_id("  alpha  ").unwrap(), "alpha");
    assert!(super::validated_key_id("").is_err());
    assert!(super::validated_key_id("has space").is_err());
    assert!(super::validated_key_id("has:colon").is_err());
    assert!(super::validated_key_id(&"x".repeat(super::MAX_KEY_ID + 1)).is_err());
}

/// A fresh reference keeps the reproducible base shape, and repeated
/// generation against a live set never reuses one of its names.
#[test]
fn fresh_references_never_name_anything_live() {
    let live: BTreeSet<String> = [OLD_REF.to_string(), "provider:relay:account:a".to_string()]
        .into_iter()
        .collect();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let candidate = fresh_reference("relay", "a", "k1", &live);
        assert!(
            candidate.starts_with("provider:relay:account:a:inference:k1:"),
            "{candidate}"
        );
        assert!(!live.contains(&candidate), "{candidate}");
        assert!(seen.insert(candidate), "two fresh references collided");
    }
}

/// Expiring a key's rates leaves account- and sibling-key-scoped rows alone.
#[test]
fn expiring_key_rates_spares_every_other_scope() {
    let mut account = account("a", vec![]);
    account.effective_rates = vec![
        rate("key-rate", Some("k1")),
        rate("account-rate", None),
        rate("sibling-rate", Some("k2")),
    ];
    expire_key_rates(&mut account, "k1");
    let ids: Vec<&str> = account
        .effective_rates
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["account-rate", "sibling-rate"]);
}

/// The core failure case: the secret was staged, the save failed, and the
/// result must be the old world with the staging entry taken back — never the
/// new secret sitting behind the old, possibly shared, reference.
#[tokio::test]
async fn a_failed_save_keeps_the_old_secret_and_recovers_the_staged_one() {
    let config = config_with(vec![account("a", vec![key("k1", OLD_REF)])]);
    let (desktop, entries, dir) = desktop_with_blocked_save(config, OLD_REF, "sk-old");

    let error = apply_inference_key_change(
        &desktop,
        "relay",
        "a",
        "k1",
        Some("sk-new".to_string()),
        None,
        None,
        None,
    )
    .await
    .expect_err("the save must fail and the command must report it");
    assert!(error.contains("cannot replace"), "{error}");

    let stored = entries.lock().unwrap().clone();
    assert_eq!(
        stored.get(OLD_REF).map(String::as_str),
        Some("sk-old"),
        "the old secret must be untouched"
    );
    assert_eq!(
        stored.len(),
        1,
        "the staged entry must have been recovered, not left behind: {stored:?}"
    );
    assert_eq!(
        desktop.core.config().providers[0].accounts[0].inference_keys[0].key_ref,
        OLD_REF,
        "the running configuration must still name the old reference"
    );
    assert_eq!(
        desktop.core.config().providers[0].accounts[0].inference_keys[0].id,
        "k1",
        "the key declaration itself must be unchanged"
    );
    assert_eq!(
        pending_publications(&desktop),
        0,
        "a compensated publication must not stay pending for the next start"
    );
    std::fs::remove_dir_all(dir).ok();
}

/// A successful publication: the declaration moves to a fresh reference, the
/// new secret answers there, the old secret is retired only after the document
/// stopped naming it, and rate evidence scoped to the replaced key dies with
/// it while wider-scoped evidence survives.
#[tokio::test]
async fn a_replaced_secret_lands_on_a_fresh_reference_and_retires_the_old_one() {
    let mut replaced = account("a", vec![key("k1", OLD_REF)]);
    replaced.effective_rates = vec![
        rate("key-rate", Some("k1")),
        rate("account-rate", None),
        rate("sibling-rate", Some("k2")),
    ];
    let (desktop, entries, dir) = desktop_with(config_with(vec![replaced]));
    entries
        .lock()
        .unwrap()
        .insert(OLD_REF.to_string(), "sk-old".to_string());

    apply_inference_key_change(
        &desktop,
        "relay",
        "a",
        "k1",
        Some("sk-new".to_string()),
        None,
        None,
        None,
    )
    .await
    .expect("the publication must succeed");

    let live = desktop.core.config();
    let published = &live.providers[0].accounts[0].inference_keys[0];
    assert_eq!(published.id, "k1");
    assert_ne!(published.key_ref, OLD_REF);
    assert!(
        published
            .key_ref
            .starts_with("provider:relay:account:a:inference:k1:"),
        "the published reference must be a fresh staging reference: {}",
        published.key_ref
    );

    let stored = entries.lock().unwrap().clone();
    assert_eq!(
        stored.get(&published.key_ref).map(String::as_str),
        Some("sk-new"),
        "the new secret must answer at the new reference"
    );
    assert_eq!(
        stored.len(),
        1,
        "the old secret is retired once nothing names it: {stored:?}"
    );

    let rates: Vec<&str> = live.providers[0].accounts[0]
        .effective_rates
        .iter()
        .map(|rate| rate.id.as_str())
        .collect();
    assert_eq!(
        rates,
        ["account-rate", "sibling-rate"],
        "key-scoped rate evidence must not price the replaced secret"
    );

    let document = std::fs::read_to_string(dir.join(crate::store::FILE_NAME)).unwrap();
    assert!(document.contains(&published.key_ref));
    // The staged reference carries the old one as its prefix, so the old
    // reference must be matched as a whole JSON string value.
    assert!(
        !document.contains(&format!("\"{OLD_REF}\"")),
        "the saved document moved on"
    );
    std::fs::remove_dir_all(dir).ok();
}

/// The shared-reference rule: replacing one holder's secret rekeys only that
/// holder. The sibling's declaration and the secret behind the shared
/// reference stay exactly as they were.
#[tokio::test]
async fn a_shared_reference_survives_a_sibling_secret_replacement() {
    let config = config_with(vec![
        account("a1", vec![key("k1", SHARED_REF)]),
        account("a2", vec![key("k2", SHARED_REF)]),
    ]);
    let (desktop, entries, dir) = desktop_with(config);
    entries
        .lock()
        .unwrap()
        .insert(SHARED_REF.to_string(), "sk-shared".to_string());

    apply_inference_key_change(
        &desktop,
        "relay",
        "a1",
        "k1",
        Some("sk-new".to_string()),
        None,
        None,
        None,
    )
    .await
    .expect("the publication must succeed");

    let live = desktop.core.config();
    let a1 = live.providers[0]
        .accounts
        .iter()
        .find(|account| account.account_id == "a1")
        .unwrap();
    let a2 = live.providers[0]
        .accounts
        .iter()
        .find(|account| account.account_id == "a2")
        .unwrap();
    assert_ne!(a1.inference_keys[0].key_ref, SHARED_REF);
    assert_eq!(
        a2.inference_keys[0].key_ref, SHARED_REF,
        "the sibling's declaration must not move"
    );
    let stored = entries.lock().unwrap();
    assert_eq!(
        stored.get(SHARED_REF).map(String::as_str),
        Some("sk-shared"),
        "the shared secret must be neither rewritten nor deleted"
    );
    assert_eq!(stored.len(), 2, "{:?}", *stored);
    std::fs::remove_dir_all(dir).ok();
}

/// Two commands racing on one key must not lose either change. Both read the
/// document under the migration lock, so the second sees the first's landed
/// change instead of saving over it with a stale snapshot.
#[tokio::test]
async fn concurrent_metadata_and_secret_edits_do_not_lose_each_other() {
    let config = config_with(vec![account("a", vec![key("k1", OLD_REF)])]);
    let (desktop, entries, dir) = desktop_with(config);
    entries
        .lock()
        .unwrap()
        .insert(OLD_REF.to_string(), "sk-old".to_string());

    let (metadata, replacement) = tokio::join!(
        apply_inference_key_change(
            &desktop,
            "relay",
            "a",
            "k1",
            None,
            Some(3),
            None,
            Some("ops note".to_string()),
        ),
        apply_inference_key_change(
            &desktop,
            "relay",
            "a",
            "k1",
            Some("sk-new".to_string()),
            None,
            None,
            None,
        ),
    );
    metadata.expect("the metadata change must land");
    replacement.expect("the secret replacement must land");

    let live = desktop.core.config();
    let key = &live.providers[0].accounts[0].inference_keys[0];
    assert_eq!(key.note, "ops note", "the metadata edit must survive");
    assert_eq!(key.priority, 3);
    assert!(
        key.key_ref
            .starts_with("provider:relay:account:a:inference:k1:"),
        "the secret edit must have rekeyed: {}",
        key.key_ref
    );
    let stored = entries.lock().unwrap();
    assert_eq!(
        stored.get(&key.key_ref).map(String::as_str),
        Some("sk-new"),
        "the new secret must answer at the published reference"
    );
    assert!(!stored.contains_key(OLD_REF));
    std::fs::remove_dir_all(dir).ok();
}

/// Removal publishes first and cleans second: a uniquely referenced secret is
/// deleted, a reference a sibling still names survives, and an unknown id is
/// an error rather than a silent nothing.
#[tokio::test]
async fn removal_cleans_only_secrets_nothing_else_names() {
    let config = config_with(vec![
        account("a1", vec![key("k1", UNIQUE_REF), key("k2", SHARED_REF)]),
        account("a2", vec![key("k3", SHARED_REF)]),
    ]);
    let (desktop, entries, dir) = desktop_with(config);
    {
        let mut stored = entries.lock().unwrap();
        stored.insert(UNIQUE_REF.to_string(), "sk-unique".to_string());
        stored.insert(SHARED_REF.to_string(), "sk-shared".to_string());
    }

    remove_inference_key(&desktop, "relay", "a1", "k1")
        .await
        .expect("removing a uniquely referenced key must succeed");
    {
        let stored = entries.lock().unwrap();
        assert!(
            !stored.contains_key(UNIQUE_REF),
            "the orphaned secret is deleted after the document landed"
        );
        assert_eq!(
            stored.get(SHARED_REF).map(String::as_str),
            Some("sk-shared")
        );
    }

    remove_inference_key(&desktop, "relay", "a1", "k2")
        .await
        .expect("removing the second key must succeed");
    {
        let stored = entries.lock().unwrap();
        assert_eq!(
            stored.get(SHARED_REF).map(String::as_str),
            Some("sk-shared"),
            "the sibling still names the shared reference; it must survive"
        );
    }
    let live = desktop.core.config();
    let a2 = live.providers[0]
        .accounts
        .iter()
        .find(|account| account.account_id == "a2")
        .unwrap();
    assert_eq!(a2.inference_keys[0].key_ref, SHARED_REF);

    let error = remove_inference_key(&desktop, "relay", "a1", "k1")
        .await
        .expect_err("an already-removed key must be an error, not silence");
    assert!(error.contains("unknown inference key"), "{error}");
    std::fs::remove_dir_all(dir).ok();
}

/// The crash-recovery contract for an orphan: a pending row whose staging
/// reference nothing names is compensated — the secret is deleted and the row
/// cleared — while every reference the document still names, including one
/// shared by a sibling account, is untouched.
#[tokio::test]
async fn recovery_deletes_only_the_unreferenced_staging_entry() {
    let config = config_with(vec![
        account("a1", vec![key("k1", OLD_REF), key("k2", SHARED_REF)]),
        account("a2", vec![key("k3", SHARED_REF)]),
    ]);
    let (desktop, entries, dir) = desktop_with(config);
    let orphan = "provider:relay:account:a1:inference:k1:orphan";
    {
        let mut stored = entries.lock().unwrap();
        stored.insert(OLD_REF.to_string(), "sk-old".to_string());
        stored.insert(SHARED_REF.to_string(), "sk-shared".to_string());
        stored.insert(orphan.to_string(), "sk-orphan".to_string());
    }
    let runtime = desktop.runtime.as_ref().unwrap();
    runtime
        .journal_inference_publication(&InferencePublication::pending("relay", "a1", "k1", orphan))
        .unwrap();
    assert_eq!(pending_publications(&desktop), 1);

    desktop
        .recover_inference_publications()
        .expect("recovery must succeed");

    {
        let stored = entries.lock().unwrap();
        assert!(
            !stored.contains_key(orphan),
            "the unreferenced staging secret must be deleted: {stored:?}"
        );
        assert_eq!(
            stored.get(OLD_REF).map(String::as_str),
            Some("sk-old"),
            "a reference the document names is never deleted"
        );
        assert_eq!(
            stored.get(SHARED_REF).map(String::as_str),
            Some("sk-shared"),
            "a sibling's shared reference must not move"
        );
        assert_eq!(stored.len(), 2, "{stored:?}");
    }
    assert!(runtime.pending_inference_publications().unwrap().is_empty());
    assert!(
        runtime.inference_publication(orphan).unwrap().is_none(),
        "the compensated journal row is cleared"
    );

    // A repeated sweep on a clean journal is a no-op.
    desktop.recover_inference_publications().unwrap();
    assert!(!entries.lock().unwrap().contains_key(orphan));
    assert_eq!(pending_publications(&desktop), 0);
    std::fs::remove_dir_all(dir).ok();
}

/// The other half of the sweep: a pending row whose reference the saved
/// document already names means the publication landed. The row is completed,
/// the live secret stays, and later sweeps change nothing.
#[tokio::test]
async fn recovery_completes_a_reference_the_document_already_names() {
    let staged = "provider:relay:account:a1:inference:k1:published";
    let config = config_with(vec![
        account("a1", vec![key("k1", staged)]),
        account("a2", vec![key("k2", SHARED_REF)]),
    ]);
    let (desktop, entries, dir) = desktop_with(config);
    {
        let mut stored = entries.lock().unwrap();
        stored.insert(staged.to_string(), "sk-live".to_string());
        stored.insert(SHARED_REF.to_string(), "sk-shared".to_string());
    }
    let runtime = desktop.runtime.as_ref().unwrap();
    runtime
        .journal_inference_publication(&InferencePublication::pending("relay", "a1", "k1", staged))
        .unwrap();

    desktop.recover_inference_publications().unwrap();

    {
        let stored = entries.lock().unwrap();
        assert_eq!(
            stored.get(staged).map(String::as_str),
            Some("sk-live"),
            "the document names the reference, so the secret stays"
        );
        assert_eq!(
            stored.get(SHARED_REF).map(String::as_str),
            Some("sk-shared")
        );
        assert_eq!(stored.len(), 2, "{stored:?}");
    }
    assert_eq!(
        runtime
            .inference_publication(staged)
            .unwrap()
            .unwrap()
            .phase,
        INFERENCE_PUBLICATION_PUBLISHED
    );
    assert!(runtime.pending_inference_publications().unwrap().is_empty());

    desktop.recover_inference_publications().unwrap();
    assert_eq!(
        entries.lock().unwrap().get(staged).map(String::as_str),
        Some("sk-live"),
        "a completed publication is never swept again"
    );
    std::fs::remove_dir_all(dir).ok();
}

/// The normal path completes its journal as part of the publication, so a
/// restart has nothing left to judge and keeps the published secret.
#[tokio::test]
async fn a_successful_publication_completes_its_journal_for_the_next_start() {
    let config = config_with(vec![account("a", vec![key("k1", OLD_REF)])]);
    let (desktop, entries, dir) = desktop_with(config);
    entries
        .lock()
        .unwrap()
        .insert(OLD_REF.to_string(), "sk-old".to_string());

    apply_inference_key_change(
        &desktop,
        "relay",
        "a",
        "k1",
        Some("sk-new".to_string()),
        None,
        None,
        None,
    )
    .await
    .expect("the publication must succeed");

    let staged = desktop.core.config().providers[0].accounts[0].inference_keys[0]
        .key_ref
        .clone();
    let runtime = desktop.runtime.as_ref().unwrap();
    assert!(
        runtime.pending_inference_publications().unwrap().is_empty(),
        "the journal is completed before the command returns"
    );
    assert_eq!(
        runtime
            .inference_publication(&staged)
            .unwrap()
            .unwrap()
            .phase,
        INFERENCE_PUBLICATION_PUBLISHED
    );

    desktop.recover_inference_publications().unwrap();
    assert_eq!(
        entries.lock().unwrap().get(&staged).map(String::as_str),
        Some("sk-new"),
        "a restart must not delete the reference the document names"
    );
    std::fs::remove_dir_all(dir).ok();
}

/// A staging write that never lands leaves no pending row: the journal written
/// before the credential store was touched is discarded with the compensation.
#[tokio::test]
async fn a_refused_staging_write_leaves_no_pending_journal() {
    let config = config_with(vec![account("a", vec![key("k1", OLD_REF)])]);
    let (desktop, dir) = desktop_with_refusing_set(config);

    let error = apply_inference_key_change(
        &desktop,
        "relay",
        "a",
        "k1",
        Some("sk-new".to_string()),
        None,
        None,
        None,
    )
    .await
    .expect_err("a refused staging write must be reported");
    assert!(error.contains("locked"), "{error}");
    assert_eq!(pending_publications(&desktop), 0);
    assert_eq!(
        desktop.core.config().providers[0].accounts[0].inference_keys[0].key_ref,
        OLD_REF,
        "the failed publication must leave the document unchanged"
    );
    std::fs::remove_dir_all(dir).ok();
}

/// The startup call site itself: a journal row left in the runtime database by
/// a previous process is swept while `Desktop::new` builds the application.
#[tokio::test]
async fn a_restart_sweeps_a_publication_journal_left_by_a_crash() {
    let config = config_with(vec![account("a", vec![key("k1", OLD_REF)])]);
    let dir = tempdir();
    let orphan = "provider:relay:account:a:inference:k1:crashed";
    journal_before_startup(
        &dir,
        &InferencePublication::pending("relay", "a", "k1", orphan),
    );
    let backend = FakeBackend::seeded(orphan, "sk-orphan");
    let entries = Arc::clone(&backend.entries);
    let secrets = Arc::new(KeychainSecrets::with_backend(Box::new(backend)));

    let desktop = Desktop::new(dir.clone(), config, secrets);

    assert!(
        !entries.lock().unwrap().contains_key(orphan),
        "startup must compensate the orphan the crash left behind"
    );
    let runtime = desktop.runtime.as_ref().unwrap();
    assert!(runtime.pending_inference_publications().unwrap().is_empty());
    assert!(runtime.inference_publication(orphan).unwrap().is_none());
    std::fs::remove_dir_all(dir).ok();
}
