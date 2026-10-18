//! The cutover commands: where the migration stands, what a cutover would do,
//! and the two operations that move it.
//!
//! Thin over [`crate::state::Desktop`]'s migration methods — the ordering and
//! its safety argument live there; this file is the Tauri boundary.
//!
//! # Why none of these are feature-gated
//!
//! `zroutery_core::migration` is compiled unconditionally, so the record, the
//! snapshot and the rollback exist in every build. Unlike `account` or `ml`,
//! there is no subsystem here that a default build might be missing, which is
//! why these carry no `available` flag and why this page never renders the
//! "this build has no such feature" empty state that `Accounts` does.

use std::sync::Arc;

use tauri::State;
use zroutery_core::agent_takeover::AgentType;
use zroutery_core::migration::MigrationResult;

use crate::migration::{CutoverPreview, MigrationStatusView};
use crate::state::Desktop;

/// Where the cutover record stands, and whether the way back is open.
#[tauri::command]
pub async fn migration_status(
    desktop: State<'_, Arc<Desktop>>,
) -> Result<MigrationStatusView, String> {
    let desktop = desktop.inner().clone();
    // Opening the manifest is file I/O, and a missing directory is created on
    // the way. Off the main thread, as with every other command.
    tauri::async_runtime::spawn_blocking(move || Ok(desktop.migration_status()))
        .await
        .map_err(|e| format!("migration status task failed: {e}"))?
}

/// The cutover plan for every client this build can migrate.
#[tauri::command]
pub async fn migration_previews(
    desktop: State<'_, Arc<Desktop>>,
) -> Result<Vec<CutoverPreview>, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || Ok(desktop.migration_previews()))
        .await
        .map_err(|e| format!("migration preview task failed: {e}"))?
}

/// Run one client's cutover, refusing a plan whose fingerprint has moved since
/// the confirmation dialog rendered it.
#[tauri::command]
pub async fn migration_apply(
    desktop: State<'_, Arc<Desktop>>,
    agent: AgentType,
    expected_fingerprint: Option<String>,
) -> Result<MigrationResult, String> {
    let desktop = desktop.inner().clone();
    // Not `spawn_blocking`: `execute` is async and awaits the endpoint probe,
    // so the command is awaited directly on Tauri's runtime rather than
    // occupying a blocking thread for the length of an HTTP round trip.
    desktop
        .migration_apply(agent, expected_fingerprint.as_deref())
        .await
}

/// Put the client's config file back exactly as the snapshot captured it.
#[tauri::command]
pub async fn migration_rollback(
    desktop: State<'_, Arc<Desktop>>,
) -> Result<MigrationResult, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || desktop.migration_rollback())
        .await
        .map_err(|e| format!("migration rollback task failed: {e}"))?
}
