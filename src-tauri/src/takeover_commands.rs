//! The takeover commands: per-agent status, adopt and release.
//!
//! Thin over [`crate::state::Desktop`]'s takeover methods — the sequence and
//! its safety argument live there; this file is the Tauri boundary.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use zroutery_core::agent_takeover::{
    AgentType, ConflictResolution, FieldConflict, ResolutionReport, TakeoverStatus,
};

use crate::state::{ClientSetupOptions, ClientSetupPreview, Desktop};

/// One agent's takeover reading: who it is and where its ownership stands.
#[derive(Serialize)]
pub struct AgentTakeoverView {
    pub agent: AgentType,
    pub status: TakeoverStatus,
}

/// What a release decided, per field, plus the agent's fresh reading.
#[derive(Serialize)]
pub struct ReleaseView {
    pub agent: AgentType,
    pub report: ResolutionReport,
    pub conflicts: Vec<FieldConflict>,
    pub status: TakeoverStatus,
}

/// Every agent's takeover status, in a fixed order.
#[tauri::command]
pub async fn takeover_status(
    desktop: State<'_, Arc<Desktop>>,
) -> Result<Vec<AgentTakeoverView>, String> {
    // The walk is file I/O on local config files; moved off the main thread by
    // Tauri's async runtime as with every other command.
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(desktop
            .takeover_views()
            .into_iter()
            .map(|(agent, status)| AgentTakeoverView { agent, status })
            .collect())
    })
    .await
    .map_err(|e| format!("takeover status task failed: {e}"))?
}

/// Preview the native four-class menu/default and any affected client files.
#[tauri::command]
pub async fn client_setup_preview(
    desktop: State<'_, Arc<Desktop>>,
    agent: AgentType,
    options: ClientSetupOptions,
) -> Result<ClientSetupPreview, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || desktop.client_setup_preview(agent, &options))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn client_setup_apply(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    agent: AgentType,
    options: ClientSetupOptions,
    expected_fingerprint: String,
) -> Result<AgentTakeoverView, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let status = desktop.client_setup_apply(agent, Some(&expected_fingerprint), &options)?;
        crate::commands::announce_config(&app);
        Ok(AgentTakeoverView { agent, status })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Take one agent over, refusing if its config moved since the preview the
/// user confirmed.
#[tauri::command]
pub async fn takeover_adopt(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    agent: AgentType,
    expected_fingerprint: Option<String>,
) -> Result<AgentTakeoverView, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let status = desktop.takeover_adopt(agent, expected_fingerprint.as_deref())?;
        crate::commands::announce_config(&app);
        Ok(AgentTakeoverView { agent, status })
    })
    .await
    .map_err(|e| format!("takeover adopt task failed: {e}"))?
}

/// Give one agent's managed fields back, resolving any conflicts with
/// `strategy` (`keep_external`, `overwrite_with_managed` or `skip`).
#[tauri::command]
pub async fn takeover_release(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    agent: AgentType,
    strategy: ConflictResolution,
) -> Result<ReleaseView, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (report, conflicts, status) = desktop.takeover_release(agent, strategy)?;
        crate::commands::announce_config(&app);
        Ok(ReleaseView {
            agent,
            report,
            conflicts,
            status,
        })
    })
    .await
    .map_err(|e| format!("takeover release task failed: {e}"))?
}
