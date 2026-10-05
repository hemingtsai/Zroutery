//! Tauri commands: the entire surface the dashboard can call.

use std::collections::BTreeMap;
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use zroutery_core::config::{AppConfig, ProviderConfig, SecretStore};
use zroutery_core::upstream::{DiscoveredModel, Upstream};
#[cfg(feature = "ml")]
use zroutery_core::{MlStatus, ReloadOutcome, ShadowAnalysisStatus};

use crate::ccswitch;
use crate::logs::LogBuffer;
use crate::state::{Activity, Desktop, Snapshot};
#[cfg(feature = "ml")]
use crate::state::{MlTraceClear, MlTraceInfo};
use crate::store;
use crate::tray;

type Cmd<T> = Result<T, String>;

async fn refreshed(app: &AppHandle, desktop: &Desktop) -> Snapshot {
    tray::refresh(app, desktop).await;
    desktop.snapshot().await
}

#[tauri::command]
pub async fn get_snapshot(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    Ok(refreshed(&app, &desktop).await)
}

/// Counters and log only: what the Activity tab polls, without cloning the
/// configuration or asking the keychain about every provider.
#[tauri::command]
pub fn get_activity(desktop: State<'_, Arc<Desktop>>) -> Cmd<Activity> {
    Ok(desktop.activity())
}

/// Recent tracing output for the Logs tab. The buffer is capped in memory, so
/// this is a rolling window rather than the full process log.
#[tauri::command]
pub fn get_logs(logs: State<'_, LogBuffer>) -> Cmd<Vec<String>> {
    Ok(logs.lines())
}

/// What the learned model is doing, for the Routing tab.
///
/// Read-only and cheap: it reads live counters and the stored pointer, and
/// installs nothing. The same document is served over HTTP at `/ml/status`, so
/// an operator without the dashboard can answer the same question.
///
/// Registered only in an ML build, which is why the dashboard must consult
/// [`Snapshot::ml_available`] rather than discovering the absence by calling
/// this and reading an error: to the webview a command that does not exist and
/// a command that failed look identical, and "this build has no learning stack"
/// is a fact worth stating instead of a failure worth guessing at.
#[cfg(feature = "ml")]
#[tauri::command]
pub fn get_ml_status(desktop: State<'_, Arc<Desktop>>) -> Cmd<MlStatus> {
    Ok(desktop.ml_status())
}

/// Replay the serving model over recorded history.
///
/// Bounded by `limit` and capped again inside, so this is safe to call from a
/// button press rather than only from a batch job.
#[cfg(feature = "ml")]
#[tauri::command]
pub fn get_ml_shadow(
    desktop: State<'_, Arc<Desktop>>,
    limit: Option<usize>,
) -> Cmd<ShadowAnalysisStatus> {
    Ok(desktop.ml_shadow_analysis(limit.unwrap_or(5_000)))
}

/// Roll back to the previously promoted model.
///
/// The freshly read status travels with the outcome so the dashboard cannot
/// render a state the process is not in, which is the entire failure mode a
/// cosmetic rollback would have.
#[cfg(feature = "ml")]
#[tauri::command]
pub fn rollback_ml_model(desktop: State<'_, Arc<Desktop>>) -> Cmd<MlRollback> {
    let outcome = desktop.rollback_active_model();
    Ok(MlRollback {
        outcome,
        status: desktop.ml_status(),
    })
}

/// What a rollback did, and the status that resulted.
#[cfg(feature = "ml")]
#[derive(serde::Serialize)]
pub struct MlRollback {
    pub outcome: ReloadOutcome,
    pub status: MlStatus,
}

/// What one promotion round decided, and the status it left behind.
///
/// The two travel together for the same reason a rollback carries its own: a
/// round that installed a model has moved the pointer and reloaded the router,
/// so the dashboard has to read the status after that happened rather than
/// render the one it had before.
///
/// `install` defaults to false and the default is load-bearing. Judging a model
/// and installing it are separate acts, and only the second changes what every
/// later request is served by — so a dashboard that asks once to "see what the
/// gate would decide" cannot promote by accident.
#[cfg(feature = "ml")]
#[tauri::command]
pub fn run_ml_promotion_round(
    desktop: State<'_, Arc<Desktop>>,
    install: Option<bool>,
    baseline: Option<String>,
) -> Cmd<MlPromotionRound> {
    let round = desktop.ml_run_promotion_round(install.unwrap_or(false), baseline);
    Ok(MlPromotionRound {
        round,
        status: desktop.ml_status(),
    })
}

/// The gate's verdict for one round, plus the routing state it produced.
#[cfg(feature = "ml")]
#[derive(serde::Serialize)]
pub struct MlPromotionRound {
    pub round: zroutery_core::ml::status::PromotionRoundStatus,
    pub status: MlStatus,
}

/// How much durable history exists, without reading it into memory.
///
/// `count` streams the file, so the number is a record count rather than a
/// claim that the whole log was loaded — which matters because the promotion
/// round trains from all of it.
#[cfg(feature = "ml")]
#[tauri::command]
pub fn get_ml_traces(desktop: State<'_, Arc<Desktop>>) -> Cmd<MlTraceInfo> {
    Ok(desktop.ml_traces())
}

/// Discard the operator's own history.
///
/// Operator-initiated only, and that is a deliberate absence rather than a
/// missing feature: the promotion round trains from the whole log, so a
/// retention policy that fired on its own would change what the next model
/// learns from without anyone having asked. The caller is told what was removed
/// *and* what it affects, because the second part is the one that is not
/// obvious — see `MlTraceClear`'s use in the dashboard.
#[cfg(feature = "ml")]
#[tauri::command]
pub fn clear_ml_traces(desktop: State<'_, Arc<Desktop>>) -> Cmd<MlTraceClear> {
    Ok(desktop.ml_clear_traces())
}

/// The token in plain text, for the dashboard's explicit "Reveal" action. Every
/// other path only ever sees the hint.
#[tauri::command]
pub fn reveal_token(desktop: State<'_, Arc<Desktop>>) -> Cmd<String> {
    Ok(desktop.auth_token())
}

/// Put the token on the clipboard without it entering the webview at all.
#[tauri::command]
pub fn copy_token(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<()> {
    app.clipboard()
        .write_text(desktop.auth_token())
        .map_err(|e| e.to_string())
}

/// Replace the whole configuration document.
///
/// The dashboard always sends the full config, which keeps conflict handling
/// trivial for a single user desktop app.
#[tauri::command]
pub async fn save_config(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    config: AppConfig,
) -> Cmd<Snapshot> {
    let launch_on_login = config.window.launch_on_login;
    desktop.apply_config(config).await?;
    desktop.set_warning(None);
    // Login launch is an OS registration, not a document field: converge the
    // registry towards the setting now that the document carrying it is saved.
    #[cfg(desktop)]
    sync_autostart(&app, launch_on_login);
    Ok(refreshed(&app, &desktop).await)
}

/// Bring the OS login registration in line with the setting.
///
/// Mirrors the startup-time sync in lib.rs; kept here as well because a save
/// is the only other moment the setting can change. Failures are warnings —
/// a machine whose policy blocks autostart still gets a working gateway.
#[cfg(desktop)]
pub(crate) fn sync_autostart_public(app: &AppHandle, enable: bool) {
    sync_autostart(app, enable);
}

#[cfg(desktop)]
fn sync_autostart(app: &AppHandle, enable: bool) {
    use tauri_plugin_autostart::ManagerExt;

    let current = app.autolaunch().is_enabled().unwrap_or(false);
    if current == enable {
        return;
    }
    let result = if enable {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    if let Err(e) = result {
        tracing::warn!(
            "cannot {} login launch: {e}",
            if enable { "register" } else { "unregister" }
        );
    }
}

#[tauri::command]
pub async fn set_provider_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    api_key: String,
) -> Cmd<Snapshot> {
    let config = desktop.core.config();
    let provider = config
        .provider(&provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let key = api_key.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    if key.len() > 4096 {
        return Err("the API key exceeds 4096 bytes".into());
    }
    // Keychain I/O blocks; keep it off the async worker thread.
    let secrets = Arc::clone(&desktop.secrets);
    let key_ref = provider.key_ref.clone();
    let key = key.to_string();
    tauri::async_runtime::spawn_blocking(move || secrets.set(&key_ref, &key))
        .await
        .map_err(|e| e.to_string())??;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_provider_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
) -> Cmd<Snapshot> {
    // We delete the key regardless of whether the provider is present in the
    // current configuration.  Existing code early-returned an error which
    // surfaced when a provider had been removed from the UI but its key
    // remained; the next delete request then failed.  Returning a success
    // keeps the UI consistent and the key is effectively removed.
    let config = desktop.core.config();
    let key_ref = match config.provider(&provider_id) {
        Some(p) => p.key_ref.clone(),
        None => format!("provider:{provider_id}"),
    };
    let secrets = Arc::clone(&desktop.secrets);
    // Delete from keychain; a missing entry is not an error.
    tauri::async_runtime::spawn_blocking(move || secrets.delete(&key_ref))
        .await
        .map_err(|e| e.to_string())??;
    Ok(refreshed(&app, &desktop).await)
}

/// Remove a provider, its models and the credential only it referenced.
///
/// The backend reads the provider's real `key_ref` before changing the
/// configuration, so a custom reference is cleared instead of the
/// name-derived guess, and a reference another provider still holds is left
/// alone. A credential that cannot be removed is reported on the returned
/// snapshot rather than swallowed.
#[tauri::command]
pub async fn remove_provider(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
) -> Cmd<Snapshot> {
    desktop.remove_provider(&provider_id).await?;
    Ok(refreshed(&app, &desktop).await)
}

/// Ask a provider what credit is left. The stored answer, including a failure,
/// comes back in the snapshot.
#[tauri::command]
pub async fn refresh_balance(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
) -> Cmd<Snapshot> {
    // The error is already recorded against the provider, so the command itself
    // succeeds and the dashboard renders the reason next to the provider.
    let _ = desktop.refresh_balance(&provider_id).await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn refresh_balances(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    let _ = desktop.refresh_all_balances().await;
    Ok(refreshed(&app, &desktop).await)
}

/// Ask a provider for its model list. Works on unsaved providers too, so the
/// user can test before committing.
#[tauri::command]
pub async fn fetch_provider_models(
    desktop: State<'_, Arc<Desktop>>,
    provider: ProviderConfig,
) -> Cmd<Vec<DiscoveredModel>> {
    // Keychain I/O blocks; keep it off the async worker thread.
    let secrets = Arc::clone(&desktop.secrets);
    let key_ref = provider.key_ref.clone();
    let key = tauri::async_runtime::spawn_blocking(move || secrets.get(&key_ref))
        .await
        .map_err(|e| e.to_string())?;
    let bypass_proxy = desktop.core.config().server.bypass_proxy;
    let connect_timeout_secs = desktop
        .core
        .config()
        .providers
        .first()
        .map(|p| p.connect_timeout_secs)
        .unwrap_or(15);
    Upstream::new(bypass_proxy, connect_timeout_secs)
        .list_models(&provider, key.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Hold an election now and pin the outcome. Costs one tiny request per model.
#[tauri::command]
pub async fn run_election(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.hold_election().await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn start_proxy(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.start().await?;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.stop().await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn regenerate_token(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    let mut config = (*desktop.core.config()).clone();
    config.server.auth_token = store::generate_token();
    desktop.apply_config(config).await?;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_stats(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.core.stats().clear();
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn reset_model_health(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    model_id: String,
) -> Cmd<Snapshot> {
    desktop.core.router().reset(&model_id);
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub fn copy_text(app: AppHandle, text: String) -> Cmd<()> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_window(app: AppHandle) -> Cmd<()> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Same path as the tray's Quit item: stop the proxy, then end the process.
#[tauri::command]
pub fn quit_app(app: AppHandle) -> Cmd<()> {
    tray::quit(&app);
    Ok(())
}

// ------------------------------------------------------- CC Switch import

/// What CC Switch has, reduced to what an import decision needs.
///
/// The API key travels with the draft but the dashboard never renders it;
/// it exists so the import command can be a single round trip without
/// re-reading the database.
#[derive(serde::Serialize)]
pub struct CcSwitchPreview {
    /// The source path the providers came from, for the panel's subtitle.
    pub source: String,
    /// Every provider found, each with the decision an import would make.
    pub providers: Vec<ccswitch::CcProviderDraft>,
}

/// Digests of the credentials a configuration references, for the accounts it
/// could read one from.
///
/// The import uses these to recognise an account whose provider was created
/// before the source marker existed. A reference whose credential is missing
/// (or whose lookup failed) is simply absent: that account can then only be
/// recognised by its marker, never mistaken for another one.
fn key_fingerprints(
    config: &AppConfig,
    secrets: &crate::secrets::KeychainSecrets,
) -> BTreeMap<String, String> {
    config
        .providers
        .iter()
        .filter(|p| !p.key_ref.is_empty())
        .filter_map(|p| {
            secrets
                .get(&p.key_ref)
                .map(|key| (p.key_ref.clone(), ccswitch::credential_fingerprint(&key)))
        })
        .collect()
}

/// Every found provider with the id an import would use and whether it is
/// already configured, decided by [`ccswitch::plan_import`].
///
/// Both the preview and the import command go through here. The rule used to
/// live only in the preview, which made the disabled rows and the import
/// command disagree.
fn draft_preview(
    config: &AppConfig,
    found: Vec<ccswitch::CcProvider>,
    fingerprints: &BTreeMap<String, String>,
) -> Vec<ccswitch::CcProviderDraft> {
    let accounts: Vec<ccswitch::SourceAccount> = found.iter().map(ccswitch::account_of).collect();
    let plan = ccswitch::plan_import(config, &accounts, fingerprints);
    found
        .into_iter()
        .zip(plan)
        .map(|(provider, planned)| {
            let (target_id, already_imported) = match planned.decision {
                ccswitch::ImportDecision::Planned(id) => (id, false),
                ccswitch::ImportDecision::AlreadyImported(id, _) => (id, true),
            };
            ccswitch::CcProviderDraft {
                provider,
                target_id,
                already_imported,
            }
        })
        .collect()
}

#[tauri::command]
pub async fn ccswitch_preview(desktop: State<'_, Arc<Desktop>>) -> Cmd<CcSwitchPreview> {
    // SQLite and file reads block; keep them off the async worker thread.
    let found = tauri::async_runtime::spawn_blocking(ccswitch::read_providers)
        .await
        .map_err(|e| e.to_string())??;

    let config = desktop.core.config();
    Ok(CcSwitchPreview {
        source: ccswitch::cc_switch_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
        providers: draft_preview(&config, found, &key_fingerprints(&config, &desktop.secrets)),
    })
}

/// Import the selected CC Switch providers.
///
/// Providers are matched by CC Switch's own provider id (stored in the
/// preview draft), so the selection survives re-ordering in CC Switch.
/// API keys go straight into the credential store; the config file keeps
/// only key refs, exactly like a hand-entered provider.
///
/// The same [`ccswitch::plan_import`] decision the preview showed is applied
/// again here: an entry that is already configured is skipped, and one relay's
/// second account — a different source entry on the same endpoint — is
/// imported under its own id instead of being mistaken for the first.
///
/// Each draft is validated before conversion; invalid entries are skipped
/// with a warning logged. The import batch is also validated after
/// conversion to catch duplicates and other cross-entry issues.
#[tauri::command]
pub async fn ccswitch_import(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    ids: Vec<String>,
) -> Cmd<Snapshot> {
    let source = ccswitch::cc_switch_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "unknown".into());

    let selected = {
        let providers = tauri::async_runtime::spawn_blocking(ccswitch::read_providers)
            .await
            .map_err(|e| e.to_string())??;
        providers
            .into_iter()
            .filter(|p| ids.contains(&p.source_id))
            .collect::<Vec<_>>()
    };

    let config = desktop.core.config();
    let accounts: Vec<ccswitch::SourceAccount> =
        selected.iter().map(ccswitch::account_of).collect();
    let plan = ccswitch::plan_import(
        &config,
        &accounts,
        &key_fingerprints(&config, &desktop.secrets),
    );
    let decisions: BTreeMap<&str, &ccswitch::ImportDecision> = selected
        .iter()
        .zip(plan.iter())
        .map(|(provider, planned)| (provider.source_id.as_str(), &planned.decision))
        .collect();

    let mut warnings: Vec<String> = Vec::new();
    let mut imported_keys: Vec<(String, String)> = Vec::new();
    let mut batch: Vec<(
        zroutery_core::config::ProviderConfig,
        Vec<zroutery_core::config::ModelEntry>,
    )> = Vec::new();

    // The current CC Switch provider becomes the class primary; the rest keep
    // CC Switch's order behind it.
    let mut priority = 0;
    for draft in &selected {
        // Validate before conversion; skip bad entries.
        if let Err(e) = ccswitch::validate_draft(draft) {
            tracing::warn!("skipping CC Switch provider `{}`: {e}", draft.name);
            warnings.push(format!("skipped `{}`: {e}", draft.name));
            continue;
        }

        let provider_id = match decisions.get(draft.source_id.as_str()) {
            // Already configured: importing it again would duplicate an account.
            Some(ccswitch::ImportDecision::AlreadyImported(existing, _)) => {
                tracing::info!(
                    "CC Switch provider `{}` is already configured as `{existing}`; skipping",
                    draft.name
                );
                continue;
            }
            Some(ccswitch::ImportDecision::Planned(id)) => id.clone(),
            None => ccswitch::source_target_id(&draft.name, &draft.source_id),
        };

        let (provider, models) = ccswitch::to_zroutery(
            draft,
            provider_id,
            if draft.is_current {
                0
            } else {
                priority.max(10)
            },
            draft.timeout_ms,
        );
        if let Some(key) = draft.api_key.clone() {
            imported_keys.push((provider.key_ref.clone(), key));
        }
        batch.push((provider.clone(), models.clone()));
        priority += 10;
    }

    // Validate the batch for cross-entry issues (duplicates, etc.).
    let (batch_warnings, batch_errors) = ccswitch::validate_import_batch(&batch);
    warnings.extend(batch_warnings);

    if !batch_errors.is_empty() {
        // Errors in the batch are fatal — do not commit.
        let msg = format!("CC Switch import aborted: {}", batch_errors.join("; "));
        tracing::error!("{msg}");
        return Err(msg);
    }

    if batch.is_empty() {
        // Nothing new to write: every selected entry was already configured.
        return Ok(refreshed(&app, &desktop).await);
    }

    let mut next = (*config).clone();
    for (provider, models) in &batch {
        next.providers.push(provider.clone());
        next.models.extend(models.iter().cloned());
    }

    // Store the keys first: an import whose providers reference keys that do
    // not exist yet would look broken in the dashboard.
    let secrets = Arc::clone(&desktop.secrets);
    tauri::async_runtime::spawn_blocking(move || {
        for (key_ref, key) in &imported_keys {
            secrets.set(key_ref, key)?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;

    desktop.apply_config(next).await?;

    // Build and log the import report.
    let report = ccswitch::build_report(&source, &batch, warnings, vec![]);
    tracing::info!(
        "CC Switch import: {} providers, {} models from {} ({} warnings)",
        report.providers_imported,
        report.models_imported,
        report.source,
        report.warnings.len(),
    );
    for w in &report.warnings {
        tracing::warn!("CC Switch import warning: {w}");
    }

    let mut snapshot = refreshed(&app, &desktop).await;
    if !report.warnings.is_empty() {
        snapshot.warning = Some(format!(
            "CC Switch import: {} providers imported with {} warning(s). See logs for details.",
            report.providers_imported,
            report.warnings.len(),
        ));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::config::{ModelTier, ProviderKind};

    /// One CC Switch entry, in the order CC Switch hands it over when the same
    /// relay holds two subscriptions with different keys.
    fn entry(source_id: &str, name: &str, base_url: &str, key: &str) -> ccswitch::CcProvider {
        ccswitch::CcProvider {
            source_id: source_id.into(),
            name: name.into(),
            base_url: base_url.into(),
            api_key: Some(key.into()),
            models: vec![ccswitch::CcModel {
                upstream_model: "m1".into(),
                tier: Some(ModelTier::Standard),
            }],
            is_current: false,
            timeout_ms: None,
        }
    }

    fn target_of(draft: &ccswitch::CcProviderDraft) -> String {
        draft.target_id.clone()
    }

    /// The preview decides what the import writes, so the id the row shows and
    /// the id the provider gets cannot drift apart.
    #[test]
    fn preview_and_import_share_one_decision_rule() {
        let found = vec![
            entry("src-a", "Relay", "https://relay.example/v1", "sk-a"),
            entry("src-b", "Relay", "https://relay.example/v1", "sk-b"),
        ];

        // Nothing is configured yet: both accounts are importable, under
        // different ids, even though the endpoint and the name match.
        let config = AppConfig::default();
        let preview = draft_preview(&config, found.clone(), &std::collections::BTreeMap::new());
        assert_eq!(preview.len(), 2);
        assert!(preview.iter().all(|d| !d.already_imported));
        assert_ne!(target_of(&preview[0]), target_of(&preview[1]));

        // Importing the first must not make the second look imported: the
        // account marker, not the URL, decides.
        let first = preview[0].clone();
        let mut config = AppConfig::default();
        config
            .providers
            .push(ccswitch::to_zroutery(&first.provider, target_of(&first), 0, None).0);
        let preview = draft_preview(&config, found, &std::collections::BTreeMap::new());
        assert!(
            preview[0].already_imported,
            "the imported account must be recognised"
        );
        assert!(
            !preview[1].already_imported,
            "the second account on the same endpoint must stay importable"
        );

        // The import command would apply the same decision: the id it would
        // write for the second entry is the one the preview showed.
        let accounts: Vec<ccswitch::SourceAccount> =
            vec![ccswitch::account_of(&preview[1].provider)];
        let plan = ccswitch::plan_import(&config, &accounts, &std::collections::BTreeMap::new());
        assert_eq!(
            plan[0].decision,
            ccswitch::ImportDecision::Planned(target_of(&preview[1])),
            "preview and import disagree about the second account"
        );
    }

    /// A key already stored under an endpoint is the same account, even when
    /// the configured provider predates the source marker.
    #[test]
    fn a_stored_credential_recognises_an_account_without_a_marker() {
        let found = vec![entry("src-a", "Relay", "https://relay.example/v1", "sk-a")];
        let mut legacy =
            zroutery_core::config::ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
        legacy.base_url = "https://relay.example/v1".into();
        let mut config = AppConfig::default();
        config.providers.push(legacy);

        let fingerprints = std::collections::BTreeMap::from([(
            "provider:relay".to_string(),
            ccswitch::credential_fingerprint("sk-a"),
        )]);
        let preview = draft_preview(&config, found, &fingerprints);
        assert!(
            preview[0].already_imported,
            "the same account was not recognised through its stored credential"
        );
        assert_eq!(target_of(&preview[0]), "relay");
    }
}
