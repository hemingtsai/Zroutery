//! Thin host-only IPC for instance creation and native secret forms.
use crate::state::{Desktop, Snapshot};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[derive(Serialize)]
pub struct CredentialStatus {
    pub present: bool,
    pub state: &'static str,
}

#[tauri::command]
pub async fn instantiate_plugin_provider(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    contribution_id: String,
    name: String,
) -> Result<Snapshot, String> {
    #[cfg(feature = "plugin")]
    {
        desktop
            .instantiate_plugin_instance(&installation_id, &contribution_id, &name)
            .await?;
        return Ok(desktop.snapshot().await);
    }
    #[cfg(not(feature = "plugin"))]
    {
        let _ = (desktop, installation_id, contribution_id, name);
        Err("unsupported_host_service".into())
    }
}

#[cfg(feature = "plugin")]
impl Desktop {
    pub async fn instantiate_plugin_instance(
        &self,
        installation_id: &str,
        contribution_id: &str,
        name: &str,
    ) -> Result<(), String> {
        let _migration = self.migration.lock().await;
        let manager = self.plugins.manager.as_ref().map_err(|e| e.to_string())?;
        let package = manager
            .package(installation_id)
            .map_err(|e| e.to_string())?;
        if !package
            .manifest
            .contributions
            .providers
            .iter()
            .any(|p| p.id == contribution_id)
        {
            return Err("permission_denied".into());
        }
        let owner = zroutery_plugin_host::grants::Owner {
            installation_id: installation_id.into(),
            provider_instance: "new".into(),
            account_generation: None,
        };
        let binding = manager
            .authority
            .bind(
                &zroutery_plugin_host::grants::InstallationIdentity {
                    installation_id: installation_id.into(),
                    plugin_id: package.manifest.id.clone(),
                    package_digest: package.digest.clone(),
                },
                contribution_id,
                owner,
            )
            .map_err(|e| e.to_string())?;
        manager
            .authority
            .authorize(
                &binding,
                zroutery_plugin_api::Principal::HostBinding,
                zroutery_plugin_api::Capability::ProviderRegister,
                zroutery_plugin_api::capability::Scope::contribution(contribution_id),
            )
            .map_err(|e| e.to_string())?;
        if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err("invalid_manifest".into());
        }
        let mut config = (*self.core.config()).clone();
        config.schema_version = zroutery_core::config::CONFIG_SCHEMA_VERSION;
        config
            .plugin_instances
            .push(zroutery_core::config::PluginInstanceConfig {
                id: format!("plugin-{}", uuid::Uuid::new_v4().simple()),
                name: name.into(),
                installation_id: installation_id.into(),
                contribution_id: contribution_id.into(),
                package_digest: package.digest.clone(),
                enabled: true,
                credentials: vec![],
                accounts: vec![],
            });
        // Preserve a pre-migration snapshot once, before the document gains the
        // new schema. Nothing here invents a native fallback for old binaries.
        let backup = self.config_dir.join("config.pre-plugin-v2.json");
        if !backup.exists() {
            if let Ok(bytes) = std::fs::read(self.config_dir.join(crate::store::FILE_NAME)) {
                zroutery_plugin_host::lifecycle::atomic_write(
                    &self.config_dir,
                    "config.pre-plugin-v2.json",
                    &bytes,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        crate::store::save(&self.config_dir, &config)?;
        self.core.set_config(config);
        Ok(())
    }
}

#[tauri::command]
pub async fn store_plugin_credential(
    desktop: State<'_, Arc<Desktop>>,
    instance_id: String,
    slot_id: String,
    secret: String,
    expected_revision: String,
    account_id: Option<String>,
) -> Result<Snapshot, String> {
    #[cfg(feature = "plugin")]
    {
        desktop
            .store_plugin_slot(
                &instance_id,
                &slot_id,
                &secret,
                &expected_revision,
                account_id.as_deref(),
            )
            .await?;
        Ok(desktop.snapshot().await)
    }
    #[cfg(not(feature = "plugin"))]
    {
        let _ = (
            desktop,
            instance_id,
            slot_id,
            secret,
            expected_revision,
            account_id,
        );
        Err("unsupported_host_service".into())
    }
}

#[tauri::command]
pub async fn clear_plugin_credential(
    desktop: State<'_, Arc<Desktop>>,
    instance_id: String,
    slot_id: String,
    expected_revision: String,
    account_id: Option<String>,
) -> Result<Snapshot, String> {
    #[cfg(feature = "plugin")]
    {
        desktop
            .clear_plugin_slot(
                &instance_id,
                &slot_id,
                &expected_revision,
                account_id.as_deref(),
            )
            .await?;
        Ok(desktop.snapshot().await)
    }
    #[cfg(not(feature = "plugin"))]
    {
        let _ = (desktop, instance_id, slot_id, expected_revision, account_id);
        Err("unsupported_host_service".into())
    }
}

#[tauri::command]
pub async fn get_plugin_credential_status(
    desktop: State<'_, Arc<Desktop>>,
    instance_id: String,
    slot_id: String,
    account_id: Option<String>,
) -> Result<CredentialStatus, String> {
    #[cfg(feature = "plugin")]
    {
        desktop.plugin_slot_status(&instance_id, &slot_id, account_id.as_deref())
    }
    #[cfg(not(feature = "plugin"))]
    {
        let _ = (desktop, instance_id, slot_id, account_id);
        Err("unsupported_host_service".into())
    }
}
