//! Tauri commands: the entire surface the dashboard can call.

use std::collections::BTreeMap;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};
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

pub(crate) type Cmd<T> = Result<T, String>;

pub(crate) const CONFIG_STATE_EVENT: &str = "zroutery://config-state-changed";

pub(crate) fn announce_config(app: &AppHandle) {
    if let Err(error) = app.emit(CONFIG_STATE_EVENT, ()) {
        tracing::warn!(%error, "cannot announce configuration change");
    }
}

pub(crate) async fn refreshed(app: &AppHandle, desktop: &Desktop) -> Snapshot {
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

/// AppKit must be read on the UI thread; slow Linux desktop settings are read
/// off it. No OS preference or application configuration is changed.
#[tauri::command]
pub async fn get_system_accent(app: AppHandle) -> Cmd<Option<crate::accent::SystemAccent>> {
    #[cfg(target_os = "macos")]
    {
        let (send, receive) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let _ = send.send(crate::accent::current());
        })
        .map_err(|error| error.to_string())?;
        receive.await.map_err(|error| error.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        tauri::async_runtime::spawn_blocking(crate::accent::current)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Daily totals for the Activity page's charts.
///
/// `days` is the size of the window ending today, and the reply always holds
/// exactly that many rows: the chart draws a quiet day as a zero rather than
/// closing the gap.
#[tauri::command]
pub async fn get_usage_days(
    desktop: State<'_, Arc<Desktop>>,
    days: u32,
) -> Cmd<Vec<zroutery_core::stats::UsageDay>> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || desktop.core.stats().usage_days(days))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn query_usage_history(
    desktop: State<'_, Arc<Desktop>>,
    query: zroutery_core::stats::UsageHistoryQuery,
) -> Cmd<zroutery_core::stats::UsageHistoryPage> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || desktop.core.stats().query_history(&query))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_usage_attempts(
    desktop: State<'_, Arc<Desktop>>,
    request_id: String,
) -> Cmd<Vec<zroutery_core::accounting::AttemptFact>> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable")?
            .usage_attempts(&request_id)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSourceView {
    pub source_name: String,
    pub revision: i64,
    pub summary: zroutery_core::stats::StatsSummary,
    pub recent: Vec<zroutery_core::stats::RequestRecord>,
    pub days: Vec<zroutery_core::stats::UsageDay>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSourcesView {
    pub local: UsageSourceView,
    pub standby: Option<UsageSourceView>,
}

#[tauri::command]
pub async fn get_usage_sources(
    desktop: State<'_, Arc<Desktop>>,
    days: u32,
) -> Cmd<UsageSourcesView> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let stats = desktop.core.stats();
        /*
         * Imported history is part of this device's history: it was made here,
         * by another tool, and the Activity page is where a person looks for
         * it. The headline and the day series therefore carry both stores.
         *
         * What is added is only what the imported store measures. Its per-day
         * and per-request rows are already deduplicated by source, instance
         * and entity id, so a re-import cannot make the figures grow; the
         * client and model breakdowns stay the live ledger's own and are
         * labelled as such, because the imported store carries no request kind
         * or attribution evidence to build an honest split from.
         */
        let mut summary = stats.summary();
        let mut days_rows = stats.usage_days(days)?;
        if let Some(store) = desktop.runtime.as_ref() {
            let totals = store.imported_totals(None, None)?;
            let first_day = store
                .imported_first_day()?
                .as_deref()
                .and_then(|day| chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok());
            summary.add_imported(&totals, first_day);
            let today = chrono::Local::now().date_naive();
            let first = today - chrono::Duration::days(i64::from(days.clamp(1, 400)) - 1);
            days_rows = zroutery_core::stats::merge_imported_days(
                days_rows,
                store.imported_days(first, today)?,
            );
        }
        let local = UsageSourceView {
            source_name: "This device".into(),
            revision: 0,
            summary,
            recent: stats.recent(200),
            days: days_rows,
        };
        let reader = desktop.standby_usage_reader();
        let standby = reader
            .map(|reader| {
                let snapshot = reader.snapshot(days)?;
                Ok::<UsageSourceView, String>(UsageSourceView {
                    source_name: snapshot.source_name,
                    revision: snapshot.revision,
                    summary: snapshot.summary,
                    recent: snapshot.recent,
                    days: snapshot.days,
                })
            })
            .transpose()?;
        Ok(UsageSourcesView { local, standby })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn choose_standby_usage_database(
    app: AppHandle,
    window: tauri::WebviewWindow,
    desktop: State<'_, Arc<Desktop>>,
) -> Cmd<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_parent(&window)
        .add_filter("SQLite database", &["sqlite3", "sqlite", "db"])
        .pick_file(move |path| {
            let _ = send.send(path);
        });
    let Some(path) = receive.await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    let display = path.display().to_string();
    let reader = tauri::async_runtime::spawn_blocking(move || {
        zroutery_core::runtime_store::UsageReader::open(&path).map(Arc::new)
    })
    .await
    .map_err(|error| error.to_string())??;
    desktop.set_standby_usage_reader(reader)?;
    Ok(Some(display))
}

/// Imported history, as the Activity page reports it: canonical totals over the
/// requested window plus the same per-day rows the local chart uses.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportedUsageView {
    pub totals: zroutery_core::history_import::ImportTotals,
    pub days: Vec<zroutery_core::stats::UsageDay>,
    /// The earliest counted imported day in the whole store, `YYYY-MM-DD`.
    ///
    /// Separate from `days[0]`, which is only the first day of the requested
    /// window: an import older than the window still moves the period the page
    /// reports over, and the lede must not name the window's edge as its start.
    pub first_day: Option<String>,
}

/// The default label for a source copy: the file's stem.
///
/// The label is the importer's identity for a copy of a source database, so it
/// has to stay the same across re-imports. A stem is stable for a relocated or
/// re-copied file, and the confirmation screen shows it, which is why a person
/// can correct it rather than having a path-derived guess applied silently.
fn default_import_instance(path: &std::path::Path) -> String {
    let name = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    };
    name.and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| "history".into())
}

/// Choose a history source: a database file, or a folder of client transcripts.
///
/// Only the path is returned. Opening and reading happen in the preview step so
/// a source the importer cannot understand produces a readable error next to the
/// confirmation screen instead of failing the file dialog.
#[tauri::command]
pub async fn pick_usage_history_source(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    window: tauri::WebviewWindow,
    kind: Option<String>,
) -> Cmd<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let folder = kind.as_deref() == Some("folder");
    // Open where this kind of source was last used, so a copy in an unusual
    // place is one confirmation away instead of a filesystem hunt.
    let start = crate::history_sources::load(&desktop.config_dir)
        .into_iter()
        .map(|entry| std::path::PathBuf::from(entry.path))
        .find(|path| {
            if folder {
                path.is_dir()
            } else {
                path.is_file()
            }
        });
    let (send, receive) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_parent(&window);
    if let Some(path) = start {
        let dir = if folder {
            path
        } else {
            path.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or(path)
        };
        if dir.is_dir() {
            dialog = dialog.set_directory(dir);
        }
    }
    if folder {
        dialog.pick_folder(move |path| {
            let _ = send.send(path);
        });
    } else {
        dialog
            .add_filter("Source database", &["db", "sqlite", "sqlite3"])
            .pick_file(move |path| {
                let _ = send.send(path);
            });
    }
    let Some(path) = receive.await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    Ok(Some(path.display().to_string()))
}

/// History sources this machine already has, and paths used before.
///
/// Nothing is opened here: the command reports paths that exist, so the
/// dashboard can offer one without a file dialog. Reading stays the preview's
/// job, where a source that turns out to be unreadable can say why beside the
/// counts.
#[tauri::command]
pub async fn discover_usage_history_sources(
    desktop: State<'_, Arc<Desktop>>,
) -> Cmd<Vec<crate::history_sources::HistorySourceOffer>> {
    let config_dir = desktop.config_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let remembered = crate::history_sources::load(&config_dir);
        crate::history_sources::offers(&crate::history_sources::Discovery::from_env(), &remembered)
    })
    .await
    .map_err(|error| error.to_string())
}

/// Prepare a durable, unpublished snapshot from a read-only history source.
#[tauri::command]
pub async fn preview_usage_history_import(
    desktop: State<'_, Arc<Desktop>>,
    path: String,
    instance: Option<String>,
    run_id: String,
    range: Option<zroutery_core::history_import::ImportRange>,
) -> Cmd<zroutery_core::history_import::ImportPreview> {
    let desktop = Arc::clone(desktop.inner());
    let config_dir = desktop.config_dir.clone();
    let chosen = path.clone();
    let preview = tauri::async_runtime::spawn_blocking(move || {
        let operation = desktop.history_imports.begin(&run_id)?;
        if operation.cancel.is_cancelled() {
            return Ok(zroutery_core::history_import::ImportPreview {
                cancelled: true,
                ..Default::default()
            });
        }
        let store = desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable")?;
        let path = std::path::PathBuf::from(path);
        let instance = instance.unwrap_or_else(|| default_import_instance(&path));
        let source = zroutery_core::history_import::Source::open_with_cancel(
            &path,
            instance,
            &operation.cancel,
        )?;
        source.stage(
            store,
            &range.unwrap_or_default(),
            &run_id,
            &operation.cancel,
        )
    })
    .await
    .map_err(|error| error.to_string())??;
    // Reading it worked, so the path is worth offering next time. Failing to
    // file that note must not turn a readable source into a refused import.
    if !preview.cancelled {
        if let Err(error) = crate::history_sources::remember(&config_dir, &chosen, &preview.source)
        {
            tracing::warn!(%error, "cannot remember the usage history source");
        }
    }
    Ok(preview)
}

/// Store the facts the preview showed.
///
/// Confirmation uses only the saved snapshot and its fingerprint. A retry after
/// a lost IPC response returns the saved outcome instead of publishing twice.
#[tauri::command]
pub async fn commit_usage_history_import(
    desktop: State<'_, Arc<Desktop>>,
    run_id: String,
    fingerprint: String,
) -> Cmd<zroutery_core::history_import::ImportOutcome> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let operation = desktop.history_imports.begin(&run_id)?;
        let store = desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable, so usage history cannot be imported")?;
        match store.publish_import(&run_id, &fingerprint, &operation.cancel) {
            Err(_) if operation.cancel.is_cancelled() => {
                store.discard_import_run(&run_id)?;
                Ok(zroutery_core::history_import::ImportOutcome {
                    cancelled: true,
                    revision: store.imported_revision()?,
                    ..Default::default()
                })
            }
            result => result,
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn cancel_usage_history_import(
    desktop: State<'_, Arc<Desktop>>,
    run_id: String,
) -> Cmd<bool> {
    // Signal first: the writer may currently own the runtime database lock.
    if !desktop.history_imports.cancel(&run_id)? {
        return Ok(false);
    }
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable")?
            .discard_import_run(&run_id)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn pending_usage_history_imports(
    desktop: State<'_, Arc<Desktop>>,
) -> Cmd<Vec<zroutery_core::history_import::ImportPreview>> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable")?
            .pending_imports()
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Imported history for the Activity page: totals for the window and its days.
#[tauri::command]
pub async fn get_imported_usage(
    desktop: State<'_, Arc<Desktop>>,
    days: u32,
) -> Cmd<ImportedUsageView> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let store = desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable")?;
        let days = days.clamp(1, 400);
        let today = chrono::Local::now().date_naive();
        let first = today - chrono::Duration::days(i64::from(days) - 1);
        let from =
            zroutery_core::history_import::day_start_utc(&first.format("%Y-%m-%d").to_string());
        Ok::<ImportedUsageView, String>(ImportedUsageView {
            totals: store.imported_totals(from, None)?,
            days: store.imported_days(first, today)?,
            first_day: store.imported_first_day()?,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Remove every imported fact. Local usage, spend, wallet and health are
/// untouched: this clears history that came from another tool.
#[tauri::command]
pub async fn clear_imported_usage(desktop: State<'_, Arc<Desktop>>) -> Cmd<usize> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let store = desktop
            .runtime
            .as_ref()
            .ok_or("runtime storage is unavailable")?;
        store.clear_imported(None)
    })
    .await
    .map_err(|error| error.to_string())?
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

/// Probe every declared account and return what the reconciler did.
///
/// Registered only in an account build, which is why the dashboard must consult
/// [`Snapshot::account_available`] rather than discovering the absence by calling
/// this and reading an error: to the webview a command that does not exist and a
/// command that failed look identical, and "this build has no account subsystem"
/// is a fact worth stating rather than a failure to guess at.
///
/// The returned report is Core's own `ReconcileReport`, not a summary shaped for
/// the dashboard. One entry per declared enabled account, including the ones no
/// backend could reach, because an account absent from the report is one that is
/// disabled while an account present as `Unknown` is one that was probed and
/// established nothing. A panel that collapses the two renders a deliberately
/// disabled account as an unhealthy one.
#[cfg(feature = "account")]
#[tauri::command]
pub async fn reconcile_accounts(
    desktop: State<'_, Arc<Desktop>>,
) -> Cmd<zroutery_core::ReconcileReport> {
    Ok(desktop.reconcile_accounts().await)
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
    expected_revision: Option<String>,
) -> Cmd<Snapshot> {
    let launch_on_login = config.window.launch_on_login;
    desktop
        .apply_config_versioned(config, expected_revision.as_deref())
        .await?;
    announce_config(&app);
    desktop.set_warning(None);
    // Login launch is an OS registration, not a document field: converge the
    // registry towards the setting now that the document carrying it is saved.
    #[cfg(desktop)]
    sync_autostart(&app, launch_on_login);
    Ok(refreshed(&app, &desktop).await)
}

/// A narrow read-modify-write under the configuration mutex, so a tray toggle
/// cannot overwrite edits from the dashboard's older full snapshot.
#[tauri::command]
pub async fn set_provider_enabled(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    enabled: bool,
) -> Cmd<Snapshot> {
    desktop.set_provider_enabled(&provider_id, enabled).await?;
    announce_config(&app);
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

/// Where an account's own credential lives, when it does not share the
/// provider's. Derived rather than chosen, so two accounts on one provider can
/// never collide and the reference is reproducible from the pair alone.
#[cfg(all(test, feature = "account"))]
fn account_key_ref(provider_id: &str, account_id: &str) -> String {
    format!("provider:{provider_id}:account:{account_id}")
}

/// Resolve where one account's own credential would live.
///
/// `(key_ref, repoint)`. A declaration whose reference is empty or equal to the
/// provider's is sharing the provider key: storing an account secret has to
/// point it at the derived reference. A reference the operator set by hand is
/// kept and its stored value replaced.
#[cfg(all(test, feature = "account"))]
pub(crate) fn account_secret_target(
    config: &AppConfig,
    provider_id: &str,
    account_id: &str,
) -> Result<(String, bool), String> {
    let provider = config
        .provider(provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let declaration = provider
        .accounts
        .iter()
        .find(|a| a.account_id == account_id)
        .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
    if declaration.key_ref.is_empty() || declaration.key_ref == provider.key_ref {
        Ok((account_key_ref(provider_id, account_id), true))
    } else {
        Ok((declaration.key_ref.clone(), false))
    }
}

/// Write a secret into the system credential store.
///
/// Keychain I/O blocks, so it is kept off the async worker thread.
#[cfg(feature = "account")]
pub(crate) async fn write_account_secret(
    desktop: &Desktop,
    key_ref: &str,
    secret: &str,
) -> Result<(), String> {
    let secrets = Arc::clone(&desktop.secrets);
    let key_ref = key_ref.to_string();
    let secret = secret.to_string();
    tauri::async_runtime::spawn_blocking(move || secrets.set(&key_ref, &secret))
        .await
        .map_err(|e| e.to_string())??;
    Ok(())
}

/// Store one account's own credential and point its declaration at it.
///
/// An account whose `key_ref` is empty (or equal to the provider's own
/// reference) resolves to the provider's key, which is the common
/// single-account case; this command exists for the case where the account
/// must not share it. The declaration is patched here rather than edited from
/// the dashboard because the reference and the stored secret must agree, and
/// only this command sees both.
#[cfg(feature = "account")]
#[tauri::command]
pub async fn set_account_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
    api_key: String,
) -> Cmd<Snapshot> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    if key.len() > 4096 {
        return Err("the API key exceeds 4096 bytes".into());
    }
    let preview = desktop
        .stage_account_binding(&provider_id, &account_id, key, None, None, None)
        .await?;
    if preview.identity_changed {
        desktop
            .discard_account_binding(&preview.publication_id)
            .await?;
        return Err("the token belongs to a different remote identity; use the binding preview and explicitly confirm the identity change".into());
    }
    desktop
        .publish_account_binding(&preview.publication_id, false)
        .await?;
    announce_config(&app);
    Ok(refreshed(&app, &desktop).await)
}

#[cfg(feature = "account")]
#[tauri::command]
pub async fn stage_account_key(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
    api_key: String,
    user_id: Option<String>,
) -> Cmd<crate::account_binding::BindingPreview> {
    desktop
        .stage_account_binding(
            &provider_id,
            &account_id,
            &api_key,
            user_id.as_deref(),
            None,
            None,
        )
        .await
}

#[cfg(feature = "account")]
#[tauri::command]
pub async fn publish_account_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    publication_id: String,
    confirm_identity_change: bool,
) -> Cmd<Snapshot> {
    desktop
        .publish_account_binding(&publication_id, confirm_identity_change)
        .await?;
    announce_config(&app);
    Ok(refreshed(&app, &desktop).await)
}

#[cfg(feature = "account")]
#[tauri::command]
pub async fn discard_account_key(
    desktop: State<'_, Arc<Desktop>>,
    publication_id: String,
) -> Cmd<()> {
    desktop.discard_account_binding(&publication_id).await
}

/// Drop one account's own credential and let it resolve to the provider's key
/// again.
///
/// The stored secret is deleted only when nothing else references it; a
/// reference shared with another declaration survives, because clearing one
/// account must not rekey another. A missing declaration is an error, unlike
/// [`clear_provider_key`]: this command also edits the configuration, and
/// editing a document that does not contain the account would silently do
/// nothing while reporting success.
#[cfg(feature = "account")]
#[tauri::command]
pub async fn clear_account_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
) -> Cmd<Snapshot> {
    let _migration = desktop.migration.lock().await;
    let mut config = (*desktop.core.config()).clone();
    let key_ref = {
        let config = desktop.core.config();
        let provider = config
            .provider(&provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let declaration = provider
            .accounts
            .iter()
            .find(|a| a.account_id == account_id)
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        if declaration.key_ref.is_empty() || declaration.key_ref == provider.key_ref {
            // Nothing of its own to clear: the account follows the provider's
            // key already, and clearing is a no-op rather than an error.
            return Ok(refreshed(&app, &desktop).await);
        }
        declaration.key_ref.clone()
    };
    if let Some(declaration) = config
        .providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .and_then(|p| p.accounts.iter_mut().find(|a| a.account_id == account_id))
    {
        // Empty is a missing panel token, not consent to borrow a provider key.
        declaration.key_ref = format!(
            "provider:{provider_id}:account:{account_id}:missing:{}",
            uuid::Uuid::new_v4().simple()
        );
        let mut binding = declaration
            .binding
            .clone()
            .unwrap_or_else(zroutery_core::config::AccountBinding::fresh);
        binding.credential_revision += 1;
        binding.verification = zroutery_core::config::BindingVerification::Pending;
        binding.reason = Some("panel token removed".into());
        declaration.binding = Some(binding);
    }
    store::save(&desktop.config_dir, &config)?;
    desktop.core.set_config(config.clone());
    if !crate::secret_refs::referenced_secrets(&config).contains(&key_ref) {
        let secrets = Arc::clone(&desktop.secrets);
        tauri::async_runtime::spawn_blocking(move || secrets.delete(&key_ref))
            .await
            .map_err(|error| error.to_string())??;
    }
    Ok(refreshed(&app, &desktop).await)
}

/// Remove one account declaration, and the credential only it referenced.
///
/// Mirrors [`remove_provider`]'s discipline: the declaration's real `key_ref`
/// is read before the configuration changes, a reference another declaration
/// still holds is left alone, and an account that followed the provider's key
/// deletes nothing. The probe and check-in stores key their runtime state by
/// `(provider_id, account_id)`, so a re-added account with the same pair starts
/// from observations of the previous one — recorded here rather than hidden.
#[cfg(feature = "account")]
#[tauri::command]
pub async fn remove_account(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
) -> Cmd<Snapshot> {
    // Both credentials the account may hold: its panel token and every
    // inference key. The deletion below is decided against the post-removal
    // document, so a reference a sibling account still names survives.
    let candidates = {
        let config = desktop.core.config();
        let provider = config
            .provider(&provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let declaration = provider
            .accounts
            .iter()
            .find(|a| a.account_id == account_id)
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        let mut candidates: Vec<String> = declaration
            .inference_keys
            .iter()
            .map(|key| key.key_ref.clone())
            .collect();
        candidates.push(declaration.key_ref.clone());
        candidates
    };
    let mut config = (*desktop.core.config()).clone();
    let provider = config
        .providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let index = provider
        .accounts
        .iter()
        .position(|a| a.account_id == account_id)
        .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
    provider.accounts.remove(index);
    desktop.apply_config(config.clone()).await?;
    announce_config(&app);
    let warnings =
        crate::inference_commands::forget_unreferenced(&desktop, &candidates, &config).await;
    if !warnings.is_empty() {
        desktop.set_warning(Some(format!(
            "account `{account_id}` was removed, but a stored secret could not be removed: {}",
            warnings.join("; ")
        )));
    }
    Ok(refreshed(&app, &desktop).await)
}

/// Announcements the relay's panel publishes on `/api/status`.
///
/// Public and credential-free. This is an explicit action rather than part of
/// the snapshot: it is a network call, and an announcement that is days old
/// does not need to be re-fetched every poll.
#[cfg(feature = "newapi")]
#[tauri::command]
pub async fn fetch_newapi_announcements(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: Option<String>,
) -> Cmd<Vec<zroutery_core::account::adapters::newapi::NewApiAnnouncement>> {
    let provider = desktop
        .core
        .config()
        .provider(&provider_id)
        .cloned()
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let adapter = newapi_adapter(&provider, account_id.as_deref())?;
    let status = adapter.probe_status().await.map_err(|e| e.to_string())?;
    Ok(if status.announcements_enabled == Some(false) {
        Vec::new()
    } else {
        status.announcements
    })
}

/// Read the panel's model prices, converted to USD per million tokens.
#[cfg(feature = "newapi")]
#[tauri::command]
pub async fn preview_newapi_pricing(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: Option<String>,
) -> Cmd<Vec<zroutery_core::account::adapters::newapi::NewApiPrice>> {
    let provider = desktop
        .core
        .config()
        .provider(&provider_id)
        .cloned()
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let adapter = newapi_adapter(&provider, account_id.as_deref())?;
    adapter.probe_pricing().await.map_err(|e| e.to_string())
}

/// Explicitly adopt public panel prices as estimated account/key fallbacks.
/// Public group ratios are not evidence of the account's actual billed group.
#[cfg(feature = "newapi")]
#[tauri::command]
pub async fn import_newapi_pricing(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: Option<String>,
    group: Option<String>,
    inference_key_id: Option<String>,
) -> Cmd<crate::state::Snapshot> {
    let provider = desktop
        .core
        .config()
        .provider(&provider_id)
        .cloned()
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let adapter = newapi_adapter(&provider, account_id.as_deref())?;
    let prices = adapter.probe_pricing().await.map_err(|e| e.to_string())?;
    {
        let _migration = desktop.migration.lock().await;
        let mut config = (*desktop.core.config()).clone();
        if config.provider(&provider_id) != Some(&provider) {
            return Err(
                "the provider changed while reading its panel; read the panel again".into(),
            );
        }
        let account = account_id
            .as_deref()
            .ok_or("select the account whose fallback price will change")?;
        let group = group
            .as_deref()
            .filter(|group| !group.trim().is_empty())
            .ok_or("select a billing group explicitly")?;
        apply_newapi_prices(
            &mut config,
            &provider_id,
            account,
            inference_key_id.as_deref(),
            group,
            &prices,
        )?;
        store::save(&desktop.config_dir, &config)?;
        desktop.core.set_config(config);
    }
    announce_config(&app);
    Ok(refreshed(&app, &desktop).await)
}

#[cfg(feature = "newapi")]
fn apply_newapi_prices(
    config: &mut AppConfig,
    provider_id: &str,
    account_id: &str,
    inference_key_id: Option<&str>,
    group: &str,
    prices: &[zroutery_core::account::adapters::newapi::NewApiPrice],
) -> Cmd<usize> {
    use zroutery_core::billing::{EffectiveRate, RateSource, RATE_SCHEMA_VERSION};
    let provider = config
        .provider(provider_id)
        .ok_or("unknown price provider")?;
    let dialect = provider.dialect();
    let account = provider
        .accounts
        .iter()
        .find(|account| account.account_id == account_id)
        .ok_or("unknown price account")?;
    let binding = account
        .binding
        .as_ref()
        .ok_or("account has no versioned binding; save the account before assigning its price")?;
    if inference_key_id.is_some_and(|id| !account.inference_keys.iter().any(|key| key.id == id)) {
        return Err("unknown inference key for price scope".into());
    }
    let version = binding.version();
    let now = chrono::Utc::now().timestamp();
    let mut rates = Vec::new();
    for model in config
        .models
        .iter()
        .filter(|m| m.provider_id == provider_id)
    {
        let Some(price) = prices
            .iter()
            .find(|price| price.group == group && price.model == model.upstream_model)
        else {
            continue;
        };
        let (input, output) = match (
            price.per_request,
            price.input_per_mtok,
            price.output_per_mtok,
        ) {
            (Some(_), _, _) => (0.0, 0.0),
            (None, Some(input), Some(output)) => (input, output),
            _ => continue,
        };
        let pricing = zroutery_core::billing::Pricing {
            per_request: price.per_request,
            currency: "USD".into(),
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: price.cache_read_per_mtok,
            cache_write_per_mtok: price.cache_write_per_mtok,
        };
        if !pricing.problems().is_empty() {
            continue;
        }
        rates.push(EffectiveRate {
            schema_version: RATE_SCHEMA_VERSION,
            id: uuid::Uuid::new_v4().simple().to_string(),
            upstream_model: model.upstream_model.clone(), dialect,
            binding_version: version.clone(), inference_key_id: inference_key_id.map(str::to_string),
            group: Some(group.into()), source: RateSource::ManualFallback,
            observed_at: now, expires_at: Some(now + 86_400),
            evidence: "operator selected public NewAPI /api/pricing group as a fallback; actual key group and invoice unverified".into(),
            pricing,
        });
    }
    if rates.is_empty() {
        return Err(format!(
            "the panel published no usable token or per-request prices for this account in group `{group}`"
        ));
    }
    let matched = rates.len();
    let account = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
        .and_then(|provider| {
            provider
                .accounts
                .iter_mut()
                .find(|account| account.account_id == account_id)
        })
        .ok_or("price account disappeared")?;
    account.effective_rates.retain(|previous| {
        previous.source != RateSource::ManualFallback
            || previous.inference_key_id.as_deref() != inference_key_id
            || !rates.iter().any(|rate| {
                rate.upstream_model == previous.upstream_model && rate.dialect == previous.dialect
            })
    });
    account.effective_rates.extend(rates);
    Ok(matched)
}

#[cfg(all(test, feature = "newapi"))]
mod price_scope_tests {
    use super::*;
    use zroutery_core::billing::{Pricing, RateSource};
    use zroutery_core::config::{
        AccountBinding, AccountConfig, InferenceKeyConfig, ModelEntry, ProviderKind,
    };

    #[test]
    fn public_prices_are_scoped_estimates_and_accept_per_call_tariffs() {
        let mut config = AppConfig::default();
        let mut provider = ProviderConfig::new("p", "p", ProviderKind::OpenAICompatible);
        for id in ["a", "b"] {
            provider.accounts.push(AccountConfig {
                account_id: id.into(),
                enabled: true,
                binding: Some(AccountBinding::fresh()),
                inference_keys: vec![InferenceKeyConfig {
                    id: "key".into(),
                    key_ref: format!("inference-{id}"),
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        config.providers.push(provider);
        let mut model = ModelEntry::for_upstream("p", "m", None);
        model.pricing = Some(Pricing::new("CNY", 3.0, 9.0));
        config.models.push(model);
        let models_before = config.models.clone();
        let sibling_before = config.providers[0].accounts[1].clone();
        let prices = vec![zroutery_core::account::adapters::newapi::NewApiPrice {
            model: "m".into(),
            group: "vip".into(),
            input_per_mtok: None,
            output_per_mtok: None,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            per_request: Some(0.03),
        }];
        assert_eq!(
            apply_newapi_prices(&mut config, "p", "a", Some("key"), "vip", &prices).unwrap(),
            1
        );
        assert_eq!(config.models, models_before);
        assert_eq!(config.providers[0].accounts[1], sibling_before);
        let rate = &config.providers[0].accounts[0].effective_rates[0];
        assert_eq!(rate.source, RateSource::ManualFallback);
        assert_eq!(rate.inference_key_id.as_deref(), Some("key"));
        assert_eq!(rate.pricing.per_request, Some(0.03));
        assert_eq!(rate.expires_at, Some(rate.observed_at + 86_400));
        assert!(
            apply_newapi_prices(&mut config, "p", "a", Some("missing"), "vip", &prices).is_err()
        );
    }
}

#[cfg(feature = "newapi")]
fn newapi_adapter(
    provider: &zroutery_core::config::ProviderConfig,
    account_id: Option<&str>,
) -> Result<zroutery_core::account::adapters::newapi::NewApiAdapter, String> {
    use zroutery_core::account::adapters::newapi::NewApiConfig;
    let account = match account_id {
        Some(id) => Some(
            provider
                .accounts
                .iter()
                .find(|account| account.account_id == id)
                .ok_or_else(|| format!("unknown account `{id}` on provider `{}`", provider.id))?,
        ),
        None => provider.accounts.iter().find(|account| account.enabled),
    };
    let config = NewApiConfig {
        base_url: zroutery_core::config::account_panel_root(
            &provider.base_url,
            account.and_then(|account| account.maintenance.panel_override()),
        ),
        connect_timeout_secs: provider.connect_timeout_secs,
        request_timeout_secs: provider.timeout_secs.min(30),
        ..NewApiConfig::default()
    };
    zroutery_core::account::adapters::newapi::NewApiAdapter::new(config).map_err(|e| e.to_string())
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
    announce_config(&app);
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
    // The same press refreshes both levels, and deliberately so. Attaching account
    // probing here rather than to a timer of its own is what stops it from
    // multiplying one provider's API traffic by the number of its accounts every
    // minute, unasked. One button, one reading per account, one snapshot back.
    // `account`, not `newapi`: this calls `Desktop::reconcile_accounts`, which is
    // gated on `account`, and `newapi` does not enable that in this crate even
    // though it does in zroutery-core. A build with newapi and without account was
    // legal on the command line and did not compile.
    #[cfg(feature = "account")]
    let _ = desktop.reconcile_accounts().await;
    Ok(refreshed(&app, &desktop).await)
}

/// Ask a provider for its model list. Works on unsaved providers too, so the
/// user can test before committing.
#[tauri::command]
pub async fn fetch_provider_models(
    desktop: State<'_, Arc<Desktop>>,
    provider: ProviderConfig,
) -> Cmd<Vec<DiscoveredModel>> {
    // The Console tier publishes no `/models` route; its catalog comes from the
    // Console config endpoint, which needs the stored OAuth token.
    #[cfg(feature = "opencode")]
    if provider.kind == zroutery_core::config::ProviderKind::OpenCodeConsole {
        return zroutery_core::opencode::console::discover(desktop.secrets.as_ref(), &provider)
            .await
            .map_err(|e| e.to_string());
    }
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
    let _ = app.emit(tray::GATEWAY_STATE_EVENT, ());
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.stop().await;
    let _ = app.emit(tray::GATEWAY_STATE_EVENT, ());
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn regenerate_token(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    let mut config = (*desktop.core.config()).clone();
    config.server.auth_token = store::generate_token();
    desktop.apply_config(config).await?;
    announce_config(&app);
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_stats(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    let worker = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || worker.core.stats().clear_history(None))
        .await
        .map_err(|error| error.to_string())??;
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
pub async fn release_wallet_hold(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: Option<String>,
    key_id: Option<String>,
) -> Cmd<Snapshot> {
    if desktop.core.config().provider(&provider_id).is_none() {
        return Err(format!("unknown provider `{provider_id}`"));
    }
    desktop
        .core
        .router()
        .release_wallet(&provider_id, account_id.as_deref(), key_id.as_deref())?;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn export_usage_history(desktop: State<'_, Arc<Desktop>>) -> Cmd<String> {
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        serde_json::to_string_pretty(&crate::usage_export::archive(desktop.core.stats())?)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn export_usage_history_file(
    app: AppHandle,
    window: tauri::WebviewWindow,
    desktop: State<'_, Arc<Desktop>>,
) -> Cmd<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_parent(&window)
        .set_file_name(format!(
            "zroutery-usage-{}.json",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
        .add_filter("JSON", &["json"])
        .save_file(move |path| {
            let _ = send.send(path);
        });
    let Some(path) = receive.await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    let desktop = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        crate::usage_export::write_archive(desktop.core.stats(), &path)?;
        Ok(Some(path.display().to_string()))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn import_usage_history(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    archive: String,
) -> Cmd<Snapshot> {
    if archive.len() > 32 * 1024 * 1024 {
        return Err("usage archive exceeds 32 MiB".into());
    }
    #[derive(serde::Deserialize)]
    struct Archive {
        schema_version: u32,
        records: Vec<zroutery_core::stats::RequestRecord>,
    }
    let worker = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let archive: Archive = serde_json::from_str(&archive).map_err(|e| e.to_string())?;
        if archive.schema_version != 1 {
            return Err("unsupported usage archive version".into());
        }
        worker.core.stats().import_history(archive.records)
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_client_usage(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    client_id: String,
) -> Cmd<Snapshot> {
    let worker = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn_blocking(move || {
        worker.core.stats().clear_history(Some(&client_id))
    })
    .await
    .map_err(|error| error.to_string())??;
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

#[tauri::command]
pub fn tray_panel_ready(app: AppHandle, window: tauri::WebviewWindow) -> Cmd<()> {
    if window.label() != crate::tray_panel::LABEL {
        return Err("only the tray panel can acknowledge readiness".into());
    }
    crate::tray_panel::ready(&app)
}

#[tauri::command]
pub fn hide_tray_panel(app: AppHandle) -> Cmd<()> {
    crate::tray_panel::hide(&app)
}

#[tauri::command]
pub fn show_dashboard(app: AppHandle) -> Cmd<()> {
    tray::show_dashboard(&app);
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
    announce_config(&app);

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

    #[cfg(feature = "account")]
    #[test]
    fn an_account_sharing_the_provider_key_is_repointed_to_its_own() {
        let mut cfg = AppConfig::default();
        let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
        provider.key_ref = "provider:relay".into();
        provider.accounts.push(Default::default());
        cfg.providers.push(provider);
        cfg.providers[0].accounts[0].account_id = "alice".into();

        let (key_ref, repoint) =
            account_secret_target(&cfg, "relay", "alice").expect("the account is declared");
        assert_eq!(key_ref, "provider:relay:account:alice");
        assert!(
            repoint,
            "the declaration has to move off the provider's key"
        );
    }

    #[cfg(feature = "account")]
    #[test]
    fn an_account_with_its_own_reference_keeps_it() {
        let mut cfg = AppConfig::default();
        let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
        provider.accounts.push(Default::default());
        cfg.providers.push(provider);
        cfg.providers[0].accounts[0].account_id = "alice".into();
        cfg.providers[0].accounts[0].key_ref = "custom:alice".into();

        let (key_ref, repoint) =
            account_secret_target(&cfg, "relay", "alice").expect("the account is declared");
        assert_eq!(key_ref, "custom:alice");
        assert!(!repoint, "a hand-set reference is not moved");
    }

    #[cfg(feature = "account")]
    #[test]
    fn an_undeclared_account_has_no_secret_target() {
        let cfg = AppConfig::default();
        assert!(account_secret_target(&cfg, "relay", "alice").is_err());
    }

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
