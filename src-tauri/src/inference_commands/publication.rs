//! Publishing an inference key secret without ever overwriting a live one.
//!
//! The publication contract (upgrade-contracts.md, "Credential publication and
//! recovery") applied to the inference pool: serialize the mutation against
//! configuration saves, write the new secret to a fresh staging reference,
//! persist the document that names it, and only then let the running state —
//! and the routing path — resolve the credential through the new reference.
//! When the publication fails, the only thing compensated is the staging entry
//! this call created; the previous document, its reference and every sibling
//! that shares it stand untouched.
//!
//! The reason the declaration moves instead of rewriting the secret in place:
//! a reference can be shared with a sibling account or kept by hand, and the
//! secret behind it is what the configuration serves until the save lands.
//! Writing the replacement over it would rekey every other holder on a save
//! that may still fail — exactly the failure the staging rule exists to make
//! impossible.
//!
//! The staging write and the configuration save are two stores with no common
//! transaction, so a process death between them could leave an unreferenced
//! staging entry in the credential store. The durable journal closes that gap:
//! a pending row naming the staging reference is written before the secret
//! exists, completed once the saved document names it, and cleared when the
//! staging entry is compensated. [`Desktop::recover_inference_publications`]
//! settles anything a previous process left behind at startup, judging each
//! row against the saved document so a published reference is never deleted.

use std::collections::BTreeSet;
use std::sync::Arc;

use zroutery_core::config::{AccountConfig, AppConfig, InferenceKeyConfig, IssueSeverity};
use zroutery_core::runtime_store::InferencePublication;

use crate::state::Desktop;

use super::MAX_KEYS;

/// The staging facts one key change produced, decided against one snapshot of
/// the configuration.
///
/// The caller keeps the mutated configuration clone; these fields say what the
/// credential store was asked to do around it.
pub(super) struct StagedChange {
    /// The reference the new secret belongs to. Empty for a metadata-only
    /// change, which never touches the credential store.
    pub(super) staged_ref: String,
    /// The reference the key pointed at before the change, when there was one.
    /// Retired after the document lands, and only when nothing else names it.
    pub(super) previous_ref: String,
}

/// A fresh reference for one secret publication: the reproducible base plus a
/// random suffix, guaranteed not to name anything the document already points
/// at.
///
/// Freshness is what makes "never overwrite a live secret" hold even for
/// hand-maintained references this code has never seen: the panel credentials,
/// sibling keys and shared entries in `live` cannot collide with a suffix that
/// did not exist when the snapshot was taken.
pub(super) fn fresh_reference(
    provider_id: &str,
    account_id: &str,
    key_id: &str,
    live: &BTreeSet<String>,
) -> String {
    loop {
        let candidate = format!(
            "{}:{}",
            super::inference_key_ref(provider_id, account_id, key_id),
            uuid::Uuid::new_v4().simple()
        );
        if !live.contains(&candidate) {
            return candidate;
        }
    }
}

/// Drop the effective rates scoped to one inference key.
///
/// [`crate::billing::resolve_rate`] matches rates by binding version and key
/// id — not by reference — so a replaced secret (same id, new reference) would
/// otherwise inherit the old credential's verified or manual price. Rates
/// scoped to the account or the model are not evidence about this secret and
/// survive; key-scoped rows die with the secret they were observed against.
pub(super) fn expire_key_rates(account: &mut AccountConfig, key_id: &str) {
    account
        .effective_rates
        .retain(|rate| rate.inference_key_id.as_deref() != Some(key_id));
}

/// Apply one key change to `config` (a clone the caller owns) and return the
/// staging facts. Errors leave `config` untouched.
///
/// A change carrying a secret always moves the declaration to a fresh
/// reference; a change without one is metadata only and keeps the reference,
/// so reordering or annotating never rekeys a stored credential.
#[allow(clippy::too_many_arguments)]
pub(super) fn stage_key_change(
    config: &mut AppConfig,
    provider_id: &str,
    account_id: &str,
    key_id: &str,
    secret: Option<&str>,
    priority: Option<i32>,
    enabled: Option<bool>,
    note: Option<String>,
) -> Result<StagedChange, String> {
    // Judge freshness against everything the document names right now — panel
    // credentials, sibling keys, deliberately shared entries. Holding the
    // migration lock is what makes this set stable while the change is built.
    let live = crate::secret_refs::referenced_secrets(config);
    let fresh = secret.map(|_| fresh_reference(provider_id, account_id, key_id, &live));

    let provider = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let account = provider
        .accounts
        .iter_mut()
        .find(|account| account.account_id == account_id)
        .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;

    let mut staged = StagedChange {
        staged_ref: String::new(),
        previous_ref: String::new(),
    };
    let mut rekeyed = false;
    match account
        .inference_keys
        .iter()
        .position(|key| key.id == key_id)
    {
        Some(index) => {
            let existing = &mut account.inference_keys[index];
            if let Some(priority) = priority {
                existing.priority = priority;
            }
            if let Some(enabled) = enabled {
                existing.enabled = enabled;
            }
            if let Some(note) = &note {
                existing.note = note.chars().take(200).collect();
            }
            if let Some(fresh) = fresh {
                staged.previous_ref = existing.key_ref.clone();
                existing.key_ref = fresh.clone();
                staged.staged_ref = fresh;
                rekeyed = true;
            }
        }
        None => {
            let fresh = fresh.ok_or("a new inference key needs its secret")?;
            if account.inference_keys.len() >= MAX_KEYS {
                return Err(format!(
                    "an account cannot hold more than {MAX_KEYS} inference keys"
                ));
            }
            account.inference_keys.push(InferenceKeyConfig {
                id: key_id.to_string(),
                key_ref: fresh.clone(),
                enabled: enabled.unwrap_or(true),
                priority: priority.unwrap_or(0),
                note: note.unwrap_or_default().chars().take(200).collect(),
            });
            staged.staged_ref = fresh;
            rekeyed = true;
        }
    }
    if rekeyed {
        // A replaced secret is a new credential, and a recreated id is a new
        // key wearing an old name: neither inherits rate evidence that was
        // observed against what it replaced.
        expire_key_rates(account, key_id);
    }
    Ok(staged)
}

/// Remove one key and everything scoped to it. Returns the reference the
/// secret lived at, for the orphan check after the document lands.
///
/// Errors leave `config` untouched, and nothing here touches the credential
/// store: the document must publish first, exactly like every other removal.
pub(super) fn stage_key_removal(
    config: &mut AppConfig,
    provider_id: &str,
    account_id: &str,
    key_id: &str,
) -> Result<String, String> {
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let account = provider
        .accounts
        .iter_mut()
        .find(|account| account.account_id == account_id)
        .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
    let position = account
        .inference_keys
        .iter()
        .position(|key| key.id == key_id)
        .ok_or_else(|| format!("unknown inference key `{key_id}` on account `{account_id}`"))?;
    let key_ref = account.inference_keys.remove(position).key_ref;
    expire_key_rates(account, key_id);
    Ok(key_ref)
}

/// The save half of the publication, with the same document discipline as the
/// general save path: normalize, refuse an invalid document, persist, and only
/// then let the caller move the running state.
///
/// The server block is carried over from the live configuration untouched, so
/// no listener step can be needed and the migration lock the caller holds is
/// never re-entered by [`crate::state::Desktop::apply_config`].
pub(super) fn validate_and_save(desktop: &Desktop, next: &mut AppConfig) -> Result<(), String> {
    next.normalize();
    let issues = next.validate();
    if let Some(issue) = issues
        .iter()
        .find(|issue| issue.severity == IssueSeverity::Error)
    {
        return Err(issue.message.clone());
    }
    crate::store::save(&desktop.config_dir, next).map(|_| ())
}

/// Delete a staging entry this module created, but only while nothing names it.
///
/// Called with the migration lock held, so the live document is still the one
/// the change failed against and the reference check cannot race a save. The
/// guard is deliberate redundancy: this function must be incapable of deleting
/// a secret the running configuration still serves.
pub(super) async fn compensate_staging(desktop: &Desktop, staged_ref: &str) {
    if staged_ref.is_empty() {
        return;
    }
    if crate::secret_refs::referenced_secrets(&desktop.core.config()).contains(staged_ref) {
        return;
    }
    let secrets = Arc::clone(&desktop.secrets);
    let target = staged_ref.to_string();
    let removed = tauri::async_runtime::spawn_blocking(move || secrets.delete(&target))
        .await
        .map_err(|error| error.to_string())
        .flatten();
    match removed {
        Ok(()) => {
            // Only a confirmed deletion may forget the journal row. If clearing
            // it fails the row stays pending, and the next startup sweep judges
            // it again from the saved document rather than trusting this call.
            if let Some(runtime) = &desktop.runtime {
                if let Err(error) = runtime.discard_inference_publication(staged_ref) {
                    let note = format!(
                        "a staged inference credential was recovered but its journal \
                         entry could not be cleared: {error}"
                    );
                    tracing::warn!("{note}");
                    desktop.set_warning(Some(note));
                }
            }
        }
        Err(error) => {
            let note = format!(
                "a staged inference credential could not be recovered after a failed save: {error}"
            );
            tracing::warn!("{note}");
            desktop.set_warning(Some(note));
        }
    }
}

/// Create or update one inference key, staged and compensated.
///
/// Reads the document inside the migration lock — a read taken before the lock
/// is a snapshot two racing commands can both save, the second reverting the
/// first's fields to what it saw — and keeps the lock across the whole
/// read–modify–save. The lock replaces the configuration-revision CAS here:
/// nothing between the read and the save can interleave, and the bindings the
/// write carries are the ones the live document already had.
// Mirrors the flat IPC parameter list of the command it serves; the same
// wire-shape reasoning applies.
#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_inference_key_change(
    desktop: &Desktop,
    provider_id: &str,
    account_id: &str,
    key_id: &str,
    secret: Option<String>,
    priority: Option<i32>,
    enabled: Option<bool>,
    note: Option<String>,
) -> Result<(), String> {
    let _migration = desktop.migration.lock().await;
    let mut config = (*desktop.core.config()).clone();

    let staged = stage_key_change(
        &mut config,
        provider_id,
        account_id,
        key_id,
        secret.as_deref(),
        priority,
        enabled,
        note,
    )?;

    if let Some(secret) = secret {
        // Durable journal first: the row names the reference the secret is
        // about to live at, so a death between the two stores leaves a record
        // a startup sweep can settle. A journal that cannot be written aborts
        // before the credential store is touched.
        let runtime = desktop.runtime.as_ref().ok_or(
            "durable publication storage is unavailable; the inference key was not changed",
        )?;
        runtime.journal_inference_publication(&InferencePublication::pending(
            provider_id,
            account_id,
            key_id,
            &staged.staged_ref,
        ))?;
        // Staging first: the new secret lands on a reference nothing has used
        // before, so whatever happens below, the secret the configuration
        // serves right now is never rewritten underneath it.
        let secrets = Arc::clone(&desktop.secrets);
        let target = staged.staged_ref.clone();
        let stored = tauri::async_runtime::spawn_blocking(move || secrets.set(&target, &secret))
            .await
            .map_err(|error| error.to_string())
            .flatten();
        if let Err(error) = stored {
            compensate_staging(desktop, &staged.staged_ref).await;
            return Err(error);
        }
    }

    if let Err(error) = validate_and_save(desktop, &mut config) {
        // The document never landed. The staged reference is an orphan this
        // call created; the previous document and its credential still stand.
        compensate_staging(desktop, &staged.staged_ref).await;
        return Err(error);
    }
    // Only now does the running state point at the new reference.
    desktop.core.set_config(config.clone());
    if let Some(runtime) = &desktop.runtime {
        if let Err(error) = runtime.complete_inference_publication(&staged.staged_ref) {
            // The change is live; the pending row is harmless and the next
            // startup sweep will complete it from the saved document. Report
            // the local persistence failure rather than reverting the change.
            let note = format!(
                "an inference key was published but its publication journal could not be \
                 completed: {error}"
            );
            tracing::warn!("{note}");
            desktop.set_warning(Some(note));
        }
    }

    // The old secret is retired after the document that stopped naming it has
    // landed, and only when no other declaration names it either. A failure
    // here is a warning, not a rollback: the change is live, and reverting it
    // to delete a secret would be the worse trade.
    if !staged.previous_ref.is_empty() {
        let warnings = super::forget_unreferenced(desktop, &[staged.previous_ref], &config).await;
        super::store(&warnings, desktop);
    }
    Ok(())
}

/// Remove one inference key: document first, credential store second.
///
/// Same lock discipline as [`apply_inference_key_change`]. A refused save
/// leaves the previous document and the secret it names usable, which is why
/// the removal is staged before anything here can touch the store.
pub(super) async fn remove_inference_key(
    desktop: &Desktop,
    provider_id: &str,
    account_id: &str,
    key_id: &str,
) -> Result<(), String> {
    let _migration = desktop.migration.lock().await;
    let mut config = (*desktop.core.config()).clone();
    let key_ref = stage_key_removal(&mut config, provider_id, account_id, key_id)?;
    validate_and_save(desktop, &mut config)?;
    desktop.core.set_config(config.clone());
    let warnings = super::forget_unreferenced(desktop, &[key_ref], &config).await;
    super::store(&warnings, desktop);
    Ok(())
}

/// Settle inference publications a previous process died in the middle of.
///
/// The saved document is the only authority. A staging reference it still names
/// means the publication landed before the death: the journal row is completed
/// and the secret behind it is left alone. A reference nothing names is an
/// orphan this process created: the staging entry is deleted and the row is
/// cleared. A reference the document names is never deleted, even when a stale
/// row says it was pending.
///
/// Idempotent by construction — pending rows are the only input, one is either
/// completed or removed on each pass, and a missing row or credential is
/// already a success. Mirrors [`Desktop::recover_bindings`] in shape and in the
/// warning the caller raises for an error.
impl Desktop {
    pub(crate) fn recover_inference_publications(&self) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        let live = crate::secret_refs::referenced_secrets(&self.core.config());
        for publication in runtime.pending_inference_publications()? {
            if live.contains(&publication.staged_ref) {
                // The save landed; the staging reference is now the live one.
                runtime.complete_inference_publication(&publication.id)?;
            } else {
                // The save never landed. Delete exactly the staging entry, then
                // forget the row; an interrupted delete stays pending so the
                // next start retries it instead of stranding the secret.
                self.secrets.delete(&publication.staged_ref)?;
                runtime.discard_inference_publication(&publication.id)?;
            }
        }
        Ok(())
    }
}
