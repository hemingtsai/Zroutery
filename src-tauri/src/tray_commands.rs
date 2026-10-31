//! Minimum data and mutations needed by the tray panel; no provider credentials,
//! custom headers, account metadata, raw diagnostics or complete config crosses.

use crate::state::Desktop;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use zroutery_core::config::{ModelTier, RoutingStrategy};

#[derive(Serialize)]
pub struct TraySnapshot {
    server: TrayServer,
    strategy: RoutingStrategy,
    tiers: Vec<TrayTier>,
    providers: Vec<TrayProvider>,
    recent: Vec<TrayRequest>,
}

#[derive(Serialize)]
struct TrayServer {
    running: bool,
    base_url: Option<String>,
    host: String,
    port: u16,
}

#[derive(Serialize)]
struct TrayProvider {
    id: String,
    name: String,
    enabled: bool,
}

#[derive(Serialize)]
struct TrayModel {
    id: String,
    name: String,
    provider_id: String,
    enabled: bool,
    priority: i32,
}

#[derive(Serialize)]
struct TrayTier {
    tier: ModelTier,
    models: Vec<TrayModel>,
    recent_model_id: Option<String>,
}

#[derive(Serialize)]
struct TrayRequest {
    id: String,
    at: chrono::DateTime<chrono::Utc>,
    client_name: String,
    model_name: Option<String>,
    ok: bool,
    latency_ms: u64,
}

async fn snapshot(desktop: &Desktop) -> TraySnapshot {
    let config = desktop.core.config();
    let address = desktop.bound_addr().await;
    let recent = desktop.core.stats().recent_local(8);
    let tiers = ModelTier::ALL
        .into_iter()
        .map(|tier| {
            let mut models: Vec<_> = config
                .models
                .iter()
                .filter(|model| {
                    model.tier == Some(tier)
                        && config
                            .provider(&model.provider_id)
                            .is_some_and(|provider| !provider.kind.is_decision())
                })
                .map(|model| TrayModel {
                    id: model.exposed_id(),
                    name: model
                        .display_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&model.upstream_model)
                        .to_owned(),
                    provider_id: model.provider_id.clone(),
                    enabled: model.enabled
                        && config
                            .provider(&model.provider_id)
                            .is_some_and(|provider| provider.enabled),
                    priority: model.priority,
                })
                .collect();
            models.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
            let recent_model_id = recent
                .iter()
                .filter_map(|request| request.resolved_model.as_ref())
                .find(|id| models.iter().any(|model| &model.id == *id))
                .cloned();
            TrayTier {
                tier,
                models,
                recent_model_id,
            }
        })
        .collect();
    TraySnapshot {
        server: TrayServer {
            running: address.is_some(),
            base_url: address.map(|address| format!("http://{address}")),
            host: config.server.host.clone(),
            port: config.server.port,
        },
        strategy: config.routing.strategy,
        tiers,
        providers: config
            .providers
            .iter()
            .map(|provider| TrayProvider {
                id: provider.id.clone(),
                name: provider.name.clone(),
                enabled: provider.enabled,
            })
            .collect(),
        recent: recent
            .into_iter()
            .map(|record| TrayRequest {
                id: record.id,
                at: record.at,
                client_name: record.client.client_name,
                model_name: record.model_name.or(record.resolved_model),
                ok: record.ok,
                latency_ms: record.latency_ms,
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn get_tray_snapshot(desktop: State<'_, Arc<Desktop>>) -> Result<TraySnapshot, String> {
    Ok(snapshot(&desktop).await)
}

#[tauri::command]
pub async fn tray_set_provider_enabled(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    enabled: bool,
) -> Result<TraySnapshot, String> {
    desktop.set_provider_enabled(&provider_id, enabled).await?;
    crate::commands::announce_config(&app);
    Ok(snapshot(&desktop).await)
}

#[tauri::command]
pub async fn tray_set_gateway_running(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    running: bool,
) -> Result<TraySnapshot, String> {
    if running {
        desktop.start().await?;
    } else {
        desktop.stop().await;
    }
    crate::tray::refresh(&app, &desktop).await;
    if let Err(error) = app.emit(crate::tray::GATEWAY_STATE_EVENT, ()) {
        tracing::warn!(%error, "cannot announce gateway change");
    }
    Ok(snapshot(&desktop).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::config::{AppConfig, ProviderConfig, ProviderKind};

    #[tokio::test]
    async fn tray_dto_excludes_private_configuration_and_diagnostics() {
        let dir = std::env::temp_dir().join(format!("zr-tray-dto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let mut config = AppConfig::default();
        config.server.auth_token = "private-gateway-token".into();
        let mut provider = ProviderConfig::new("p", "Readable", ProviderKind::OpenAICompatible);
        provider.base_url = "https://private-panel.invalid".into();
        provider
            .extra_headers
            .insert("Authorization".into(), "private-upstream-key".into());
        config.providers.push(provider);
        let desktop = Desktop::new(
            dir.clone(),
            config,
            Arc::new(crate::secrets::KeychainSecrets::new("test-tray-dto")),
        );
        let encoded = serde_json::to_string(&snapshot(&desktop).await).unwrap();
        assert!(encoded.contains("Readable"));
        for forbidden in [
            "private-gateway-token",
            "private-panel",
            "private-upstream-key",
            "key_ref",
            "extra_headers",
            "routing_decision",
        ] {
            assert!(!encoded.contains(forbidden), "{forbidden}");
        }
        drop(desktop);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
