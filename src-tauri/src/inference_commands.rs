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

use std::collections::BTreeSet;
use std::sync::Arc;

use tauri::{AppHandle, State};
use zroutery_core::config::{AppConfig, InferenceKeyConfig};

use crate::commands::{refreshed, Cmd};
use crate::state::Desktop;

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
/// `provider:{p}:account:{a}` or a provider key's `provider:{p}`.
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

    let mut config = (*desktop.core.config()).clone();
    let key_ref = {
        let provider = config
            .providers
            .iter_mut()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let account = provider
            .accounts
            .iter_mut()
            .find(|a| a.account_id == account_id)
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        let key_ref = inference_key_ref(&provider_id, &account_id, &key_id);
        match account.inference_keys.iter_mut().find(|k| k.id == key_id) {
            Some(existing) => {
                if let Some(priority) = priority {
                    existing.priority = priority;
                }
                if let Some(enabled) = enabled {
                    existing.enabled = enabled;
                }
                if let Some(note) = &note {
                    existing.note = note.chars().take(200).collect();
                }
                // A hand-edited reference is kept so the repair does not move a
                // secret the operator put somewhere on purpose.
                if existing.key_ref.trim().is_empty() {
                    existing.key_ref = key_ref.clone();
                }
                existing.key_ref.clone()
            }
            None => {
                if secret.is_none() {
                    return Err("a new inference key needs its secret".into());
                }
                if account.inference_keys.len() >= MAX_KEYS {
                    return Err(format!(
                        "an account cannot hold more than {MAX_KEYS} inference keys"
                    ));
                }
                account.inference_keys.push(InferenceKeyConfig {
                    id: key_id.clone(),
                    key_ref: key_ref.clone(),
                    enabled: enabled.unwrap_or(true),
                    priority: priority.unwrap_or(0),
                    note: note.unwrap_or_default().chars().take(200).collect(),
                });
                key_ref
            }
        }
    };

    if let Some(secret) = secret {
        let secrets = Arc::clone(&desktop.secrets);
        let target = key_ref.clone();
        tauri::async_runtime::spawn_blocking(move || secrets.set(&target, &secret))
            .await
            .map_err(|e| e.to_string())??;
    }

    desktop.apply_config(config).await?;
    Ok(refreshed(&app, &desktop).await)
}

/// Remove one inference key and the secret only it referenced.
#[tauri::command]
pub async fn remove_account_inference_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
    key_id: String,
) -> Cmd<crate::state::Snapshot> {
    let mut config = (*desktop.core.config()).clone();
    let key_ref = {
        let provider = config
            .providers
            .iter_mut()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let account = provider
            .accounts
            .iter_mut()
            .find(|a| a.account_id == account_id)
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        let position = account
            .inference_keys
            .iter()
            .position(|k| k.id == key_id)
            .ok_or_else(|| format!("unknown inference key `{key_id}` on account `{account_id}`"))?;
        account.inference_keys.remove(position).key_ref
    };
    desktop.apply_config(config.clone()).await?;
    let warnings = forget_unreferenced(&desktop, &[key_ref], &config).await;
    store(&warnings, &desktop);
    Ok(refreshed(&app, &desktop).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inference_reference_never_collides_with_a_panel_reference() {
        assert_eq!(
            inference_key_ref("p", "a", "k1"),
            "provider:p:account:a:inference:k1"
        );
        assert_ne!(inference_key_ref("p", "a", "k1"), "provider:p:account:a");
        assert_ne!(inference_key_ref("p", "a", "k1"), "provider:p");
    }

    #[test]
    fn key_ids_are_trimmed_and_reject_separators() {
        assert_eq!(validated_key_id("  alpha  ").unwrap(), "alpha");
        assert!(validated_key_id("").is_err());
        assert!(validated_key_id("has space").is_err());
        assert!(validated_key_id("has:colon").is_err());
        assert!(validated_key_id(&"x".repeat(MAX_KEY_ID + 1)).is_err());
    }
}
