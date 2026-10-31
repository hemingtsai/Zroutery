//! Panel identity and keychain/config interruption fixtures. No real credential.
#![cfg(all(feature = "account", feature = "newapi"))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::json;
use zroutery_core::config::{
    AccountBinding, AccountConfig, AccountProbeKind, AppConfig, BindingVerification,
    ProviderConfig, ProviderKind, SecretStore,
};

use crate::secrets::{CredentialBackend, KeychainSecrets, StoreError};
use crate::state::Desktop;

#[derive(Default, Clone)]
struct Secrets(Arc<Mutex<HashMap<String, String>>>);
impl CredentialBackend for Secrets {
    fn get(&self, reference: &str) -> Result<String, StoreError> {
        self.0
            .lock()
            .unwrap()
            .get(reference)
            .cloned()
            .ok_or(StoreError::NoEntry)
    }
    fn set(&self, reference: &str, value: &str) -> Result<(), StoreError> {
        self.0
            .lock()
            .unwrap()
            .insert(reference.into(), value.into());
        Ok(())
    }
    fn delete(&self, reference: &str) -> Result<(), StoreError> {
        self.0.lock().unwrap().remove(reference);
        Ok(())
    }
}

async fn fixture() -> (
    Desktop,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
    Secrets,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let app = axum::Router::new().route("/api/user/self", get(|headers: axum::http::HeaderMap| async move {
            let token = headers.get("authorization").and_then(|value| value.to_str().ok());
            let (id, name, group) = match token {
                Some("token-a") => (11, "alice", "team-a"),
                Some("token-b") => (29, "bob", "team-b"),
                _ => return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({"success":false,"code":"AUTH_UNAUTHORIZED"}))).into_response(),
            };
            if headers.get("new-api-user").is_some_and(|value| value.to_str().ok() != Some(id.to_string().as_str())) {
                return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({"success":false,"code":"AUTH_USER_ID_MISMATCH"}))).into_response();
            }
            axum::Json(json!({"success":true,"data":{"id":id,"username":name,"group":group,"status":1}})).into_response()
        }));
        axum::serve(listener, app).await.unwrap();
    });
    let directory = std::env::temp_dir().join(format!(
        "zroutery-binding-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let backend = Secrets::default();
    backend.set("shared-legacy", "legacy-unchanged").unwrap();
    let mut provider = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    provider.base_url = root;
    provider.accounts = ["a", "b"]
        .map(|id| AccountConfig {
            account_id: id.into(),
            key_ref: "shared-legacy".into(),
            enabled: true,
            binding: Some(AccountBinding::fresh()),
            probe_kind: Some(AccountProbeKind::NewApi),
            ..Default::default()
        })
        .to_vec();
    let mut config = AppConfig::default();
    config.providers.push(provider);
    config.server.autostart = false;
    crate::store::save(&directory, &config).unwrap();
    let desktop = Desktop::new(
        directory.clone(),
        config,
        Arc::new(KeychainSecrets::with_backend(Box::new(backend.clone()))),
    );
    (desktop, directory, task, backend)
}

#[tokio::test]
async fn two_distinct_users_bind_without_overwriting_a_shared_legacy_reference() {
    let (desktop, directory, task, backend) = fixture().await;
    let a = desktop
        .stage_account_binding("p", "a", "token-a", Some("11"), None, None)
        .await
        .unwrap();
    assert_eq!(a.identity.as_ref().unwrap().user_id, "11");
    assert_eq!(
        a.identity.as_ref().unwrap().group.as_deref(),
        Some("team-a")
    );
    assert_eq!(
        desktop.core.config().providers[0].accounts[0].key_ref,
        "shared-legacy",
        "staging does not publish"
    );
    desktop
        .publish_account_binding(&a.publication_id, false)
        .await
        .unwrap();
    let b = desktop
        .stage_account_binding("p", "b", "token-b", Some("29"), None, None)
        .await
        .unwrap();
    desktop
        .publish_account_binding(&b.publication_id, false)
        .await
        .unwrap();
    let config = desktop.core.config();
    let a = &config.providers[0].accounts[0];
    let b = &config.providers[0].accounts[1];
    assert_ne!(a.key_ref, b.key_ref);
    assert_eq!(desktop.secrets.get(&a.key_ref).as_deref(), Some("token-a"));
    assert_eq!(desktop.secrets.get(&b.key_ref).as_deref(), Some("token-b"));
    assert_eq!(
        b.binding.as_ref().unwrap().verification,
        BindingVerification::Verified
    );
    assert_eq!(b.maintenance.new_api_user.as_deref(), Some("29"));
    assert!(
        backend.get("shared-legacy").is_err(),
        "orphan is cleaned only after the last reference moves"
    );
    task.abort();
    drop(desktop);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn identity_switch_requires_confirmation_and_stale_preview_is_refused() {
    let (desktop, directory, task, _) = fixture().await;
    let a = desktop
        .stage_account_binding("p", "a", "token-a", Some("11"), None, None)
        .await
        .unwrap();
    desktop
        .publish_account_binding(&a.publication_id, false)
        .await
        .unwrap();
    let old = desktop.binding_version("p", "a").unwrap();
    let switch = desktop
        .stage_account_binding("p", "a", "token-b", Some("29"), Some(&old), None)
        .await
        .unwrap();
    assert!(switch.identity_changed);
    assert!(desktop
        .publish_account_binding(&switch.publication_id, false)
        .await
        .is_err());
    assert_eq!(desktop.binding_version("p", "a").unwrap(), old);
    desktop
        .publish_account_binding(&switch.publication_id, true)
        .await
        .unwrap();
    assert_ne!(desktop.binding_version("p", "a").unwrap(), old);
    assert!(desktop
        .stage_account_binding("p", "a", "token-a", Some("11"), Some(&old), None)
        .await
        .is_err());
    let current = (*desktop.core.config()).clone();
    let mut stale = current.clone();
    stale.providers[0].accounts[0].binding = None;
    assert!(
        desktop.apply_config(stale).await.is_err(),
        "ordinary save cannot undo binding version"
    );
    task.abort();
    drop(desktop);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn save_failure_keeps_old_binding_and_restart_compensates_only_orphan_staging() {
    let (desktop, directory, task, _) = fixture().await;
    let preview = desktop
        .stage_account_binding("p", "a", "token-a", Some("11"), None, None)
        .await
        .unwrap();
    let staged = desktop
        .runtime
        .as_ref()
        .unwrap()
        .binding_publication(&preview.publication_id)
        .unwrap()
        .unwrap()
        .staged_ref;
    let temporary = directory.join("config.json");
    let original = std::fs::read(&temporary).unwrap();
    std::fs::remove_file(&temporary).unwrap();
    std::fs::create_dir(&temporary).unwrap();
    assert!(desktop
        .publish_account_binding(&preview.publication_id, false)
        .await
        .is_err());
    assert_eq!(
        desktop.core.config().providers[0].accounts[0].key_ref,
        "shared-legacy"
    );
    assert_eq!(
        desktop.secrets.get("shared-legacy").as_deref(),
        Some("legacy-unchanged")
    );
    assert!(desktop.secrets.has(&staged));
    std::fs::remove_dir(&temporary).unwrap();
    std::fs::write(&temporary, original).unwrap();
    desktop.recover_bindings().unwrap();
    assert!(!desktop.secrets.has(&staged));
    assert_eq!(
        desktop.core.config().providers[0].accounts[1].key_ref,
        "shared-legacy"
    );
    task.abort();
    drop(desktop);
    std::fs::remove_dir_all(directory).unwrap();
}
