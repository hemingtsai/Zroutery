//! The takeover surface: what each external agent's takeover looks like right
//! now, and the two operations that move it.
//!
//! The state machine, the ownership manifest and the per-client adapters all
//! live in [`zroutery_core::agent_takeover`]. This file is the seam between
//! that machinery and the window layer: it decides what Zroutery offers a
//! client (the gateway target), opens the durable store per agent, and walks
//! the adopt sequence in the one order that is safe.

use std::collections::HashMap;
use std::sync::Arc;

use zroutery_core::agent_takeover::{
    adapter_for, extract_field_values, takeover_fields, AgentType, ConflictResolution,
    FieldConflict, GatewayTarget, ReleaseOutcome, ResolutionReport, TakeoverStatus, TakeoverStore,
    TierClass, VerificationReport,
};
use zroutery_core::config::ModelTier;

use super::helpers::lock;
use super::Desktop;

impl Desktop {
    /// The durable ownership store for one agent, opened on first use.
    ///
    /// One manifest per agent under `<config_dir>/takeover/`: the manifest is
    /// the only record of what an adopt overwrote, so it outlives the process.
    /// A corrupt manifest is reported rather than silently treated as "no
    /// manifest" — starting from nothing would orphan a client config that
    /// still points at this proxy.
    pub(crate) fn takeover_store(
        &self,
        agent: AgentType,
    ) -> Result<Arc<TakeoverStore>, TakeoverStatus> {
        if let Some(existing) = lock(&self.takeover_stores).get(&agent) {
            return Ok(Arc::clone(existing));
        }
        let dir = self.config_dir.join("takeover");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))
            .map_err(|reason| TakeoverStatus::Error { reason })?;
        let path = dir.join(format!("{}.json", agent.client_name()));
        let store = TakeoverStore::open(&path).map_err(|reason| TakeoverStatus::Error {
            reason: format!("{reason} ({})", path.display()),
        })?;
        let store = Arc::new(store);
        lock(&self.takeover_stores).insert(agent, Arc::clone(&store));
        Ok(store)
    }

    /// What Zroutery would offer a taken-over client, or `None` while it is
    /// not in a position to serve one.
    ///
    /// The token is the local gateway credential — the same one
    /// `reveal_token` shows — and the classes are the tiers that currently
    /// have at least one enabled model, named the way the proxy exposes them
    /// under the active naming style. No default class is chosen on the
    /// user's behalf; the gateway contract leaves model selection to the
    /// client or to Zroutery's per-client policy.
    pub(crate) fn gateway_target(&self) -> Option<GatewayTarget> {
        let config = self.core.config();
        let token = config.server.auth_token.trim();
        if token.is_empty() {
            return None;
        }

        let style = config.routing.naming_style;
        let classes: Vec<TierClass> = ModelTier::ALL
            .iter()
            .filter(|tier| {
                config
                    .models
                    .iter()
                    .any(|m| m.enabled && m.tier == Some(**tier))
            })
            .map(|tier| TierClass {
                model: tier.virtual_id_styled(style).to_string(),
                display_name: capitalize(tier.as_str()),
            })
            .collect();
        if classes.is_empty() {
            return None;
        }

        Some(GatewayTarget {
            base_url: format!("http://{}", super::server_addr(&config.server)),
            token: token.to_string(),
            classes,
            default_class: None,
        })
    }

    /// What this call can vouch for about a held takeover.
    ///
    /// The desktop cannot watch a client's traffic, so `request_confirmed`
    /// stays `None` — and the status machine answers `degraded`, never
    /// `active`, for an unproven takeover. What *can* be checked here is the
    /// credential: if the gateway token was regenerated after the adopt, the
    /// client is still sending the old one and every request it makes will
    /// fail. That is worth a note of its own.
    fn verification_for(
        &self,
        agent: AgentType,
        target: Option<&GatewayTarget>,
    ) -> Option<VerificationReport> {
        let store = self.takeover_store(agent).ok()?;
        if store.state() != zroutery_core::agent_takeover::OwnershipState::Adopted {
            return None;
        }
        let mut report = VerificationReport::unchecked();

        let (Some(adapter), Some(target)) = (adapter_for(agent), target) else {
            return Some(report);
        };
        let Ok(snapshot) = adapter.read_config() else {
            report
                .notes
                .push("the client config could not be read right now".into());
            return Some(report);
        };
        report.config_matches = true;

        if let Ok(fields) = takeover_fields(agent, target, Some(&snapshot.raw)) {
            let token_paths: Vec<String> = fields
                .iter()
                .filter(|f| f.value == serde_json::json!(target.token))
                .map(|f| f.path.clone())
                .collect();
            let live = extract_field_values(&snapshot.raw, &token_paths);
            report.credential_ok =
                !token_paths.is_empty() && token_paths.iter().all(|p| live.contains_key(p));
            if !report.credential_ok {
                report.notes.push(
                    "the gateway token has changed since this client was taken over — \
                     release and take over again to hand the client the new one"
                        .into(),
                );
            }
        }
        Some(report)
    }

    /// Every agent's takeover reading, in `AgentType::ALL` order.
    pub(crate) fn takeover_views(&self) -> Vec<(AgentType, TakeoverStatus)> {
        let target = self.gateway_target();
        AgentType::ALL
            .iter()
            .map(|&agent| {
                let status = match self.takeover_store(agent) {
                    Err(status) => status,
                    Ok(store) => {
                        let adapter = adapter_for(agent);
                        let verified = self.verification_for(agent, target.as_ref());
                        store.inspect(adapter.as_deref(), target.as_ref(), verified.as_ref())
                    }
                };
                (agent, status)
            })
            .collect()
    }

    /// Take one agent over: record what would be replaced, write the gateway
    /// into its config, record what was written.
    ///
    /// `expected_fingerprint` is the proposal digest the confirmation dialog
    /// was rendered from. The adopt refuses when the agent's config has moved
    /// since — confirming a preview of one file against a different one is
    /// the failure the whole preview flow exists to prevent.
    pub(crate) fn takeover_adopt(
        &self,
        agent: AgentType,
        expected_fingerprint: Option<&str>,
    ) -> Result<TakeoverStatus, String> {
        let adapter = adapter_for(agent)
            .ok_or_else(|| format!("this build has no takeover adapter for {agent:?}"))?;
        let target = self.gateway_target().ok_or_else(|| {
            "Zroutery is not ready to serve a client yet: set the gateway token in Settings and \
             assign at least one enabled model to a tier"
                .to_string()
        })?;

        let store = self.takeover_store(agent).map_err(|s| match s {
            TakeoverStatus::Error { reason } => reason,
            other => format!("unexpected takeover state: {other:?}"),
        })?;
        // The document decides which schema generation is managed, so it is
        // read before the field set is derived.
        let snapshot = adapter.read_config()?;
        let fields = takeover_fields(agent, &target, Some(&snapshot.raw))?;

        let paths: Vec<String> = fields.iter().map(|f| f.path.clone()).collect();
        let managed_values: HashMap<String, serde_json::Value> = fields
            .iter()
            .map(|f| (f.path.clone(), f.value.clone()))
            .collect();
        let current = extract_field_values(&snapshot.raw, &paths);
        let proposal = store.plan_adopt(&paths, &current, &managed_values)?;
        if let Some(expected) = expected_fingerprint {
            if expected != proposal.fingerprint {
                return Err(
                    "the agent's config changed since the takeover was previewed. Refresh and \
                     review the changes again before confirming."
                        .to_string(),
                );
            }
        }

        // The store records the originals before anything is written: an adopt
        // that cannot say what it replaced must not write.
        store.adopt(proposal.managed_fields.clone(), &current)?;
        let write = adapter.apply_patch(&snapshot, &fields);
        if let Err(err) = write {
            // The config was not changed (apply_patch is atomic), so handing
            // ownership straight back keeps the store and the disk consistent.
            let _ = store.release();
            return Err(err);
        }
        store.record_applied(&managed_values)?;

        Ok(store.inspect(
            Some(adapter.as_ref()),
            Some(&target),
            self.verification_for(agent, Some(&target)).as_ref(),
        ))
    }

    /// Give one agent's fields back, deciding every detected conflict with
    /// `strategy`. Returns what was decided, per field, plus the fresh status.
    pub(crate) fn takeover_release(
        &self,
        agent: AgentType,
        strategy: ConflictResolution,
    ) -> Result<(ResolutionReport, Vec<FieldConflict>, TakeoverStatus), String> {
        let adapter = adapter_for(agent)
            .ok_or_else(|| format!("this build has no takeover adapter for {agent:?}"))?;
        let store = self.takeover_store(agent).map_err(|s| match s {
            TakeoverStatus::Error { reason } => reason,
            other => format!("unexpected takeover state: {other:?}"),
        })?;
        let outcome: ReleaseOutcome = store.release_resolved(adapter.as_ref(), strategy)?;
        let status = self
            .takeover_views()
            .into_iter()
            .find(|(a, _)| *a == agent)
            .map(|(_, status)| status)
            .unwrap_or(TakeoverStatus::Error {
                reason: "the agent vanished from the takeover sweep".into(),
            });
        Ok((outcome.report, outcome.conflicts, status))
    }
}

/// `fast` → `Fast`, for a model picker that shows words, not enum names.
fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
