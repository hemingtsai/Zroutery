//! Inference credentials attached to one provider account.
//!
//! An account's `key_ref` is its *panel* credential: the token the balance probe
//! and check-in use. Chat traffic never uses it. `inference_keys` is the
//! explicit pool of credentials the routing path may send upstream, and each
//! entry keeps only a keychain reference; the secret itself never reaches the
//! configuration file or the webview.
//!
//! Removing a declaration deletes its secret only when the post-change document
//! no longer names that reference. Two accounts may deliberately share one
//! secret, and one account's delete must not rekey another.
//!
//! A change that carries a secret publishes through
//! [`publication`]: the secret is staged on a reference nothing has used
//! before, the document naming it is saved under the migration lock, and only
//! a landed document moves the running state onto the new reference. The
//! reference that was replaced is never overwritten and never deleted while a
//! sibling names it, so a failed save leaves the previous credential exactly
//! as the configuration served it.

use std::collections::BTreeSet;
use std::sync::Arc;

use tauri::{AppHandle, State};
use zroutery_core::config::AppConfig;

use crate::commands::{refreshed, Cmd};
use crate::state::Desktop;

mod publication;
#[cfg(test)]
mod tests;

/// Longest accepted key id. It is part of a keychain reference and a table
/// column rather than a free-form note, and core validation checks the same
/// bound.
const MAX_KEY_ID: usize = 64;
const MAX_SECRET: usize = 4096;
/// A financial account with more live credentials than this is a configuration
/// mistake, not a pool: every send tries them in order.
const MAX_KEYS: usize = 32;

/// Where one inference key's secret lives.
///
/// Derived from the triple rather than stored by the caller, so the reference is
/// reproducible and can never collide with a panel credential's
/// `provider:{p}:account:{a}` or a provider key's `provider:{p}`. Publications
/// append a fresh suffix to this base, so a staged reference keeps the same
/// shape plus one more segment.
pub(crate) fn inference_key_ref(provider_id: &str, account_id: &str, key_id: &str) -> String {
    format!("provider:{provider_id}:account:{account_id}:inference:{key_id}")
}

fn validated_key_id(key_id: &str) -> Result<String, String> {
    let key_id = key_id.trim();
    if key_id.is_empty() {
        return Err("the inference key id is empty".into());
    }
    if key_id.chars().count() > MAX_KEY_ID {
        return Err(format!(
            "the inference key id exceeds {MAX_KEY_ID} characters"
        ));
    }
    if key_id.contains(':') || key_id.chars().any(char::is_whitespace) {
        return Err("the inference key id cannot contain `:` or whitespace".into());
    }
    Ok(key_id.to_string())
}

/// Delete the candidate references nothing in `remaining` names any more.
///
/// Warnings rather than failures: the configuration change has already landed,
/// and a credential left behind is something the operator can remove by hand.
/// Reverting the change to delete a secret would be the worse trade.
pub(crate) async fn forget_unreferenced(
    desktop: &Desktop,
    candidates: &[String],
    remaining: &AppConfig,
) -> Vec<String> {
    let live = crate::secret_refs::referenced_secrets(remaining);
    let orphans: Vec<String> = candidates
        .iter()
        .filter(|key_ref| !key_ref.is_empty() && !live.contains(*key_ref))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if orphans.is_empty() {
        return Vec::new();
    }
    let secrets = Arc::clone(&desktop.secrets);
    match tauri::async_runtime::spawn_blocking(move || {
        let mut warnings = Vec::new();
        for key_ref in orphans {
            if let Err(e) = secrets.delete(&key_ref) {
                warnings.push(format!("`{key_ref}` could not be removed ({e})"));
            }
        }
        warnings
    })
    .await
    {
        Ok(warnings) => warnings,
        Err(e) => vec![format!("the credential store task failed ({e})")],
    }
}

fn store(warnings: &[String], desktop: &Desktop) {
    if warnings.is_empty() {
        return;
    }
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    desktop.set_warning(Some(format!(
        "an inference key was changed, but a stored secret could not be removed: {}",
        warnings.join("; ")
    )));
}

/// Create or update one inference key on an account.
///
/// `api_key` is `None` or blank when only the metadata changes (enable, priority),
/// which is what keeps a stored secret from having to be re-entered to reorder
/// the pool. A new id requires a secret: an enabled entry without one would be a
/// key the routing path skips while the dashboard shows it as present.
// The flat parameter list is the IPC contract: Tauri binds each argument by
// name from the webview's object, so this is a wire shape rather than a
// positional call site, and the lint's premise (a caller can transpose two
// arguments) does not apply to it. Grouping them into one struct would rename
// the wire object the dashboard already sends.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn set_account_inference_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
    key_id: String,
    api_key: Option<String>,
    priority: Option<i32>,
    enabled: Option<bool>,
    note: Option<String>,
) -> Cmd<crate::state::Snapshot> {
    let key_id = validated_key_id(&key_id)?;
    let secret = api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if secret
        .as_ref()
        .is_some_and(|value| value.len() > MAX_SECRET)
    {
        return Err(format!("the inference key exceeds {MAX_SECRET} bytes"));
    }
    publication::apply_inference_key_change(
        &desktop,
        &provider_id,
        &account_id,
        &key_id,
        secret,
        priority,
        enabled,
        note,
    )
    .await?;
    Ok(refreshed(&app, &desktop).await)
}

/// Remove one inference key and the secret only it referenced.
///
/// Lookup is by the id exactly as the caller sent it: an unknown id is an
/// error, not a trimmed near-miss.
#[tauri::command]
pub async fn remove_account_inference_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
    key_id: String,
) -> Cmd<crate::state::Snapshot> {
    publication::remove_inference_key(&desktop, &provider_id, &account_id, &key_id).await?;
    Ok(refreshed(&app, &desktop).await)
}
