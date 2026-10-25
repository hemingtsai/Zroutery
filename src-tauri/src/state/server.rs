//! Binding, rebinding and unbinding the listener, and saving the configuration around it.

use std::sync::Arc;

use zroutery_core::config::{AppConfig, IssueSeverity, ServerConfig};
use zroutery_core::server::ServerHandle;

use super::helpers::lock;
use super::helpers::{needs_rebind, server_addr};
use super::views::*;
use super::Desktop;
use crate::store;

impl Desktop {
    pub async fn is_running(&self) -> bool {
        self.server.lock().await.is_some()
    }

    /// The address the listener actually bound, when one is running. A
    /// configuration of port 0 resolves to a real port here.
    pub async fn bound_addr(&self) -> Option<std::net::SocketAddr> {
        self.server.lock().await.as_ref().map(|s| s.addr)
    }

    pub async fn start(&self) -> Result<(), String> {
        let mut guard = self.server.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let handle = ServerHandle::start(Arc::clone(&self.core))
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!("proxy listening on http://{}", handle.addr);
        *guard = Some(handle);
        Ok(())
    }

    pub async fn stop(&self) {
        self.stop_listener().await;
        // Whatever was spent while it ran must outlive the process.
        self.flush_ledger();
    }

    /// Drop the running listener without touching the ledger.
    ///
    /// The migration uses this so the listener handover and the ledger flush
    /// stay separate steps.
    async fn stop_listener(&self) {
        let handle = self.server.lock().await.take();
        if let Some(h) = handle {
            h.stop().await;
            tracing::info!("proxy stopped");
        }
    }

    pub async fn restart(&self) -> Result<(), String> {
        self.stop().await;
        self.start().await
    }

    /// Remove a provider, its models and the credential only it used.
    ///
    /// The real `key_ref` is captured before the configuration changes, so a
    /// custom reference is cleared instead of a guessed `provider:{id}`. A
    /// reference another provider still holds is left alone, and a delete that
    /// failed comes back as a warning on the next snapshot rather than a
    /// silent success.
    ///
    /// Returns the warning when the credential could not be removed.
    pub async fn remove_provider(&self, provider_id: &str) -> Result<Option<String>, String> {
        let _migration = self.migration.lock().await;
        let previous = self.core.config();
        let provider = previous
            .provider(provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        // Every credential the provider could own: its own key, each account's
        // panel token, and every inference key on those accounts. Which of them
        // is actually orphaned is decided against the post-removal document, so
        // a reference another provider or account still names survives.
        let mut removed_refs: Vec<String> = std::iter::once(provider.key_ref.clone())
            .chain(provider.accounts.iter().flat_map(|account| {
                std::iter::once(account.key_ref.clone())
                    .chain(account.inference_keys.iter().map(|key| key.key_ref.clone()))
            }))
            .filter(|key_ref| !key_ref.trim().is_empty())
            .collect();
        removed_refs.sort();
        removed_refs.dedup();

        let mut next = (*previous).clone();
        next.providers.retain(|p| p.id != provider_id);
        next.models.retain(|m| m.provider_id != provider_id);
        // The Auto Mode pool names models by exposed id or alias, and every model
        // this provider offered is gone now — leaving the candidates behind would
        // dangle them (reported as a warning since the classifier simply skips
        // unknown members). Removing a provider is an explicit intent that
        // covers its models, so the pool members that lived on it go too.
        let gone: std::collections::HashSet<String> = previous
            .models
            .iter()
            .filter(|m| m.provider_id == provider_id)
            .flat_map(|m| {
                let mut ids = m.aliases.clone();
                ids.push(m.exposed_id());
                ids
            })
            .collect();
        next.classifier
            .candidates
            .retain(|c| !gone.contains(&c.model));

        let live = crate::secret_refs::referenced_secrets(&next);
        let mut failed = Vec::new();
        for key_ref in removed_refs {
            if live.contains(&key_ref) {
                tracing::info!(
                    "provider `{provider_id}` removed; `{key_ref}` is still used elsewhere"
                );
                continue;
            }
            if let Err(e) = self.secrets.delete(&key_ref) {
                failed.push(format!("`{key_ref}` ({e})"));
            }
        }
        let orphaned = if failed.is_empty() {
            None
        } else {
            Some(format!(
                "provider `{provider_id}` was removed, but a stored secret could not be removed: {}; remove it in the credential manager",
                failed.join(", ")
            ))
        };

        store::save(&self.config_dir, &next)?;
        *lock(&self.window_rules) = WindowRules {
            keep_in_tray: next.window.keep_in_tray,
        };
        self.core.set_config(next);
        if orphaned.is_some() {
            self.set_warning(orphaned.clone());
        }
        Ok(orphaned)
    }

    /// Persist and hot swap a new configuration.
    ///
    /// Returns `true` when the listener had to be rebound, which only happens
    /// for host, port or CORS changes.
    ///
    /// The configuration change and the listener migration are one step. When
    /// the address changes the target has to be bindable *before* the running
    /// gateway is touched, so a save that fails leaves the old configuration —
    /// on disk as well as in memory — and the old listener in place. If the
    /// new listener cannot be started after the old one stopped, the previous
    /// configuration and listener are restored, so what the caller is told and
    /// what the app is doing cannot disagree.
    pub async fn apply_config(&self, mut next: AppConfig) -> Result<bool, String> {
        // Serialise with other saves and with any listener stop.
        let _migration = self.migration.lock().await;

        // Tidy the alias lists and fold any legacy id the dashboard echoed back.
        next.normalize();
        let previous = self.core.config();
        // The dashboard never receives the token, so an empty one means "keep
        // what you have" rather than "clear it".
        if next.server.auth_token.trim().is_empty() {
            next.server.auth_token = previous.server.auth_token.clone();
        }
        // Warn when the server is about to become exposed to the network.
        if !previous.server.is_exposed() && next.server.is_exposed() {
            tracing::warn!(
                "server host is changing from {} to {} — the proxy will be \
                 reachable from the network",
                previous.server.host,
                next.server.host,
            );
        }
        let issues = next.validate();
        if let Some(err) = issues.iter().find(|i| i.severity == IssueSeverity::Error) {
            return Err(err.message.clone());
        }
        let needs_rebind = needs_rebind(&previous.server, &next.server);

        // Refuse an address the gateway could not claim while nothing has been
        // written yet: a running listener keeps serving the saved settings and
        // a stopped one stays stopped. Port 0 is excluded: the OS picks a free
        // port, so there is nothing to conflict with and probing the resolved
        // port would be a false clash.
        let running = self.is_running().await;
        if needs_rebind && next.server.port != 0 {
            self.prober
                .probe(&server_addr(&next.server))
                .map_err(|e| self.refused_rebind_error(&previous.server, running, &e))?;
        }

        store::save(&self.config_dir, &next)?;
        // Rebuild the upstream HTTP client if proxy bypass setting changed.
        if previous.server.bypass_proxy != next.server.bypass_proxy {
            self.core.rebuild_upstream(next.server.bypass_proxy);
        }
        // The close button reads the rules synchronously, so the window layer
        // is told directly rather than discovering the change later.
        *lock(&self.window_rules) = WindowRules {
            keep_in_tray: next.window.keep_in_tray,
        };
        self.core.set_config(next.clone());

        if let Err(e) = self.migrate_listener(&previous, &next, needs_rebind).await {
            // The change did not take: put the document back the way it was, so
            // a dashboard that re-reads the state sees the truth.
            if let Err(restore) = store::save(&self.config_dir, &previous) {
                tracing::error!("cannot restore the previous configuration: {restore}");
            }
            self.core.set_config((*previous).clone());
            *lock(&self.window_rules) = WindowRules {
                keep_in_tray: previous.window.keep_in_tray,
            };
            let note = format!(
                "the change to {} was refused and the previous configuration and gateway are still in place ({e})",
                server_addr(&next.server),
            );
            self.set_warning(Some(note.clone()));
            return Err(note);
        }

        self.set_warning(None);
        Ok(needs_rebind)
    }

    /// What to say when the target address could not be claimed at all.
    ///
    /// The caller is told what actually happened: nothing was saved and the old
    /// listener is untouched. That is the state the dashboard has to re-read.
    fn refused_rebind_error(&self, previous: &ServerConfig, running: bool, cause: &str) -> String {
        let note = if running {
            format!(
                "cannot use the new address: {cause}; the gateway is still listening on {} and the configuration was not changed",
                server_addr(previous),
            )
        } else {
            format!("cannot use the new address: {cause}; the configuration was not changed",)
        };
        self.set_warning(Some(note.clone()));
        note
    }

    /// Move the running listener onto `next`, or stop it when the change only
    /// needs a new port that nothing is serving.
    async fn migrate_listener(
        &self,
        previous: &AppConfig,
        next: &AppConfig,
        needs_rebind: bool,
    ) -> Result<(), String> {
        if !needs_rebind {
            return Ok(());
        }
        if self.is_running().await {
            // Remember the address that was actually served, not the one in the
            // document: a configuration of port 0 has a real port here.
            let served = self.bound_addr().await;
            self.stop_listener().await;
            if let Err(e) = self.start().await {
                // The old listener is gone and the new one would not come up:
                // restore the previous listener so the user is left with a
                // working gateway rather than none.
                self.core.set_config((*previous).clone());
                let mut restored = self.start().await;
                if restored.is_err() {
                    if let Some(addr) = served {
                        // The document said port 0, so bring the listener back
                        // on the port it was really serving.
                        let mut pinned = (*previous).clone();
                        pinned.server.port = addr.port();
                        self.core.set_config(pinned);
                        restored = self.start().await;
                    }
                }
                return match restored {
                    Ok(()) => Err(format!(
                        "cannot listen on {}: {e}; the previous gateway was restored",
                        server_addr(&next.server),
                    )),
                    Err(restore) => Err(format!(
                        "cannot listen on {}: {e}; the previous gateway could not be restored either: {restore}",
                        server_addr(&next.server),
                    )),
                };
            }
        }
        Ok(())
    }
}
