//! The cutover surface: where a migration stands, how to start one, and how to
//! put the files back when it went wrong.
//!
//! # Why this exists next to takeover
//!
//! Both write an external client's configuration to point at the Zroutery
//! gateway, and they overlap on purpose:
//!
//! | | takeover | cutover |
//! |---|---|---|
//! | writes | managed *fields* | the whole *file* |
//! | records | previous field values | a byte snapshot of the original |
//! | undo | restores fields it wrote | restores the file as it was |
//! | survives a crash | no | yes -- [`MigrationStore`] is durable |
//! | refuses when | a field diverged | the plan changed under the dialog |
//!
//! Neither subsumes the other. Takeover is the better tool for putting a client
//! on the gateway and taking it off again, and this page does not offer that as
//! a second button. What this page owns is the part takeover has no answer for:
//! a cutover that failed partway through, or was interrupted by the process
//! dying mid-run, and the question of whether the user's file came back.
//!
//! # Why the record is durable
//!
//! `MigrationStore::new` forgets. The three states that mean work was started
//! and not finished -- `Prepared`, `Verified`, `Switched` -- are exactly the
//! ones that need recovering, and an in-memory store reports `Detected` for all
//! of them after a restart: `recover` then refuses the states it exists for,
//! and the snapshot a rollback needs is gone. So the store here is opened on a
//! path, never in memory, and a failure to open is reported rather than
//! downgraded to a working in-memory store that lies.

use std::path::PathBuf;
use std::sync::Arc;

use zroutery_core::agent_takeover::{adapter_for, takeover_fields, AgentType};
use zroutery_core::migration::{
    MigrationExecutor, MigrationResult, MigrationSnapshot, MigrationState, MigrationStore,
};

use super::helpers::lock;
use super::Desktop;
use crate::migration::{
    build_plan, fingerprint, preview_of, staging_path, CutoverPreview, MigrationStatusView,
};

impl Desktop {
    /// Where the migration record and its rollback snapshot live.
    fn migration_manifest_path(&self) -> PathBuf {
        self.config_dir.join("migration").join("manifest.json")
    }

    /// The durable executor, opened on first use.
    ///
    /// Held here rather than rebuilt per command so one process keeps one view
    /// of the record; the manifest is what survives a restart, and re-opening it
    /// per call would make a half-run migration look absent to a second command
    /// in the same session.
    pub(crate) fn migration_executor(&self) -> Result<Arc<MigrationExecutor>, String> {
        let mut slot = lock(&self.cutover);
        if let Some(existing) = slot.as_ref() {
            return Ok(Arc::clone(existing));
        }

        let path = self.migration_manifest_path();
        let dir = self.config_dir.join("migration");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let store = MigrationStore::open(&path).map_err(|e| format!("{e} ({})", path.display()))?;
        let executor = Arc::new(MigrationExecutor::new(store));
        *slot = Some(Arc::clone(&executor));
        Ok(executor)
    }

    /// Where a cutover stands, and whether the way back is open.
    pub(crate) fn migration_status(&self) -> MigrationStatusView {
        let executor = match self.migration_executor() {
            Ok(executor) => executor,
            Err(reason) => {
                // A record that cannot be opened is reported as a status, not
                // swallowed: the panel has to be able to say "this build cannot
                // read its own migration record" rather than rendering a clean
                // "not migrated" that is not what is happening.
                return MigrationStatusView {
                    state: MigrationState::Detected,
                    interrupted: false,
                    has_snapshot: false,
                    snapshot_files: Vec::new(),
                    durable: false,
                    persist_error: Some(reason),
                    owned_processes: Vec::new(),
                };
            }
        };
        let snapshot = executor.snapshot_from_disk();
        MigrationStatusView {
            state: executor.state(),
            // A `Failed` state is interrupted in exactly the sense that matters
            // here: the record says work started, and the way back is open.
            interrupted: matches!(
                executor.state(),
                MigrationState::Prepared
                    | MigrationState::Verified
                    | MigrationState::Switched
                    | MigrationState::Failed
            ),
            snapshot_files: snapshot
                .as_ref()
                .map(|s| s.files.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default(),
            has_snapshot: snapshot.is_some(),
            durable: executor.is_durable(),
            persist_error: executor.persist_error(),
            owned_processes: executor.owned_labels(),
        }
    }

    /// The cutover plan for every client this build can migrate.
    ///
    /// Every agent is listed, including the ones that cannot be cut over, so an
    /// unsupported client renders as an explained refusal rather than silently
    /// vanishing -- the same reason `Takeover` keeps Gemini in its row.
    pub(crate) fn migration_previews(&self) -> Vec<CutoverPreview> {
        let (base_url, port) = self.migration_gateway();
        AgentType::ALL
            .iter()
            .map(|&agent| match &base_url {
                Some(url) => match build_plan(&self.config_dir, agent, url, port) {
                    Ok((plan, config_path)) => preview_of(agent, &plan, Some(&config_path), None),
                    Err(reason) => preview_of(agent, &empty_plan(agent), None, Some(reason)),
                },
                // No gateway target: nothing to cut over *to*. Reported on every
                // row rather than hidden, because the fix is in Settings and a
                // row that vanished would send the user looking in the wrong
                // place.
                None => preview_of(
                    agent,
                    &empty_plan(agent),
                    None,
                    Some(
                        "Zroutery is not ready to serve a client yet: set the gateway token in \
                         Settings and assign at least one enabled model to a tier"
                            .to_string(),
                    ),
                ),
            })
            .collect()
    }

    /// The gateway address a cutover would point a client at, and its port.
    fn migration_gateway(&self) -> (Option<String>, u16) {
        let port = self.core.config().server.port;
        (self.gateway_target().map(|t| t.base_url), port)
    }

    /// Run one client's cutover, refusing a plan the user did not confirm.
    ///
    /// The order is the whole point and is not interchangeable:
    ///
    /// 1. refuse if there is no gateway to point at;
    /// 2. refuse if the record is not in a state that admits a run -- the state
    ///    machine admits `Detected -> Prepared` once, so a second run would be
    ///    refused by the executor after the snapshot was already taken;
    /// 3. compare the fingerprint, so a plan that changed under the dialog is
    ///    not executed;
    /// 4. stage the merged document;
    /// 5. **snapshot the file, before anything is written**;
    /// 6. execute.
    ///
    /// Step 5 is the one a reordering would cost: a snapshot taken afterwards
    /// captures the migrated file, and the rollback restores the thing that went
    /// wrong.
    pub(crate) async fn migration_apply(
        &self,
        agent: AgentType,
        expected_fingerprint: Option<&str>,
    ) -> Result<MigrationResult, String> {
        let (base_url, port) = self.migration_gateway();
        let base_url = base_url.ok_or_else(|| {
            "Zroutery is not ready to serve a client yet: set the gateway token in Settings and \
             assign at least one enabled model to a tier"
                .to_string()
        })?;

        let executor = self.migration_executor()?;
        let state = executor.state();
        if state != MigrationState::Detected {
            return Err(format!(
                "a migration has already reached {state:?}; this record admits one run, and \
                 running again would rewrite a system that is already switched. Review the \
                 current state above instead."
            ));
        }

        let (plan, config_path) = build_plan(&self.config_dir, agent, &base_url, port)?;
        let actual = fingerprint(&plan, Some(&config_path));
        if let Some(expected) = expected_fingerprint {
            if expected != actual {
                return Err(
                    "the cutover changed since it was previewed. Review it again before \
                     confirming."
                        .to_string(),
                );
            }
        }

        // Staged before the snapshot so a failure to build the document cannot
        // leave a snapshot of a file that is about to be half-changed.
        self.stage_cutover(agent, &config_path)?;

        let paths = [config_path.to_string_lossy().into_owned()];
        let snapshot = executor.create_snapshot(&[paths[0].as_str()])?;
        if snapshot.files.is_empty() {
            // The engine skips an unreadable path with a warning and returns a
            // snapshot with nothing in it. Continuing would install a document
            // with no way back, and `rolled_back` would later report the truth
            // (`false`) only after the damage.
            return Err(format!(
                "no rollback snapshot could be taken of {} — the file could not be read, so a \
                 cutover could not be undone. Fix the file's permissions and try again.",
                config_path.display()
            ));
        }

        let result = executor.execute(&plan).await;
        executor_store_result(&executor, &result);
        Ok(result)
    }

    /// Put the files back from the persisted snapshot.
    ///
    /// Only offered from a state that admits `Failed -> RolledBack`, which is
    /// the state a cutover that did not finish leaves behind. Refused from
    /// `Completed` on purpose: the state machine has no transition out of it,
    /// and `rollback_with` ignores that refusal -- it would restore the files and
    /// leave the record saying `Completed`. A panel offering that button would
    /// be offering a button whose effect the record would deny.
    pub(crate) fn migration_rollback(&self) -> Result<MigrationResult, String> {
        let executor = self.migration_executor()?;
        let state = executor.state();
        if state != MigrationState::Failed {
            return Err(format!(
                "a rollback is only offered from Failed, because that is the only state with a \
                 permitted transition back. This record is {state:?}."
            ));
        }
        let snapshot: Option<MigrationSnapshot> = executor.snapshot_from_disk();
        if snapshot.is_none() {
            return Err(
                "there is no rollback snapshot on disk, so there is nothing to restore. The \
                 record says the cutover failed and the file it would have restored was never \
                 captured."
                    .to_string(),
            );
        }
        let result = executor.rollback(snapshot.as_ref());
        executor_store_result(&executor, &result);
        Ok(result)
    }

    /// Write the merged document to the staging path the plan installs from.
    ///
    /// The patch runs against a snapshot whose `config_path` has been pointed at
    /// the staging file, so the adapter writes the staged copy and never the
    /// live config. The byte-for-byte copy first is not incidental: the adapter
    /// refuses a write whose snapshot hash does not match the file, and a
    /// verbatim copy is exactly the case where the staging hash equals the
    /// hash `read_config` already returned.
    fn stage_cutover(&self, agent: AgentType, config_path: &std::path::Path) -> Result<(), String> {
        let adapter = adapter_for(agent).ok_or_else(|| {
            format!(
                "this build has no migration adapter for {}",
                agent.client_name()
            )
        })?;
        let target = self
            .gateway_target()
            .ok_or_else(|| "Zroutery cannot serve a client yet".to_string())?;
        let live = adapter.read_config()?;
        let fields = takeover_fields(agent, &target, Some(&live.raw))?;

        let staged = staging_path(&self.config_dir, agent);
        let dir = staged.parent().expect("staging path has a parent");
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

        if config_path.exists() {
            std::fs::copy(config_path, &staged)
                .map_err(|e| format!("cannot stage {}: {e}", config_path.display()))?;
        } else if staged.exists() {
            // A client with no config yet must be staged onto an empty
            // document, or the hash check would compare a previous run's staged
            // copy against a fresh client and refuse for the wrong reason.
            std::fs::remove_file(&staged)
                .map_err(|e| format!("cannot clear {}: {e}", staged.display()))?;
        }

        let staged_snapshot = zroutery_core::agent_takeover::AgentConfigSnapshot {
            agent_type: agent,
            config_path: staged,
            raw: live.raw,
            // Empty only when the client has no config yet, which is the one
            // case the adapter's reread path treats as "absent" rather than
            // "changed".
            config_hash: live.config_hash,
        };
        adapter
            .apply_patch(&staged_snapshot, &fields)
            .map(|_| ())
            .map_err(|e| format!("cannot stage the cutover document: {e}"))
    }
}

/// A plan with no steps, for a client that has none.
///
/// Exists so a row that cannot be migrated still carries the plan's identity
/// and fingerprint, and so the fingerprint of "no plan" is stable rather than
/// absent.
fn empty_plan(agent: AgentType) -> zroutery_core::migration::MigrationPlan {
    zroutery_core::migration::MigrationPlan {
        plan_id: format!("cutover-{}", agent.client_name()),
        source_description: format!("Zroutery cutover for {}", agent.client_name()),
        steps: Vec::new(),
        created_at: 0,
    }
}

/// Record the outcome in the store's history.
///
/// `execute` and `rollback` return a result but do not file it, so the durable
/// record keeps the state without keeping what produced it. A `recover` in a
/// later process reads the state and has nothing to tell a user about how the
/// run went.
fn executor_store_result(executor: &MigrationExecutor, result: &MigrationResult) {
    executor.record(result.clone());
}
