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

use serde::{Deserialize, Serialize};
use zroutery_core::agent_takeover::{
    adapter_for, extract_field_values, takeover_fields, AgentType, ConflictResolution,
    FieldConflict, GatewayTarget, ResolutionReport, TakeoverStatus, TakeoverStore, TierClass,
    VerificationReport,
};
use zroutery_core::config::ModelTier;
use zroutery_core::file_transaction::{self, FileChange};

use super::helpers::lock;
use super::Desktop;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientSetupOptions {
    pub default_class: Option<String>,
    /// Explicit development handoff; never inferred from a failed main port.
    #[serde(default)]
    pub standby: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientSetupPreview {
    pub agent: AgentType,
    pub classes: Vec<TierClass>,
    pub unavailable_classes: Vec<String>,
    pub catalogue_fingerprint: String,
    pub proposal: zroutery_core::agent_takeover::TakeoverProposal,
    pub config_path: String,
    pub gateway_url: String,
    pub notes: Vec<String>,
}

impl Desktop {
    fn client_journal_dir(&self) -> std::path::PathBuf {
        self.config_dir.join("takeover").join("transactions")
    }

    pub(super) fn recover_client_files(&self) -> Result<(), String> {
        let _files = lock(&self.client_files);
        self.recover_client_files_locked()
    }

    fn recover_client_files_locked(&self) -> Result<(), String> {
        if let Err(error) = file_transaction::recover(&self.client_journal_dir()) {
            self.set_warning(Some(format!(
                "client configuration recovery needs attention: {error}"
            )));
            return Err(error);
        }
        for store in lock(&self.takeover_stores).values() {
            store.reload()?;
        }
        Ok(())
    }

    /// Bind the preview to its write intent and all owned files, not just the
    /// old managed values. A pool or auxiliary catalogue change also expires it.
    fn bind_setup_proposal(
        &self,
        agent: AgentType,
        proposal: &mut zroutery_core::agent_takeover::TakeoverProposal,
        snapshot: &zroutery_core::agent_takeover::AgentConfigSnapshot,
        target: &GatewayTarget,
    ) -> Result<(), String> {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        proposal.fingerprint.hash(&mut hash);
        snapshot.config_hash.hash(&mut hash);
        serde_json::to_vec(&proposal.changes)
            .map_err(|error| error.to_string())?
            .hash(&mut hash);
        zroutery_core::agent_takeover::catalogue_fingerprint(&target.classes).hash(&mut hash);
        let ownership = self
            .config_dir
            .join("takeover")
            .join(format!("{}.json", agent.client_name()));
        super::client_catalogue::read_optional(&ownership)?.hash(&mut hash);
        let selection = self
            .config_dir
            .join("takeover")
            .join(format!("{}-selection.json", agent.client_name()));
        super::client_catalogue::read_optional(&selection)?.hash(&mut hash);
        if agent == AgentType::Codex {
            for change in self.prepare_codex_catalogue(
                &zroutery_core::agent_takeover::codex_catalogue(&target.classes),
            )? {
                change.path.hash(&mut hash);
                change.before.hash(&mut hash);
                change.after.hash(&mut hash);
            }
        }
        proposal.fingerprint = format!("{:016x}", hash.finish());
        Ok(())
    }

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

        // A class offers the ordered union of its usable members' effort levels.
        // Members whose provider is disabled, or that are decision endpoints,
        // cannot serve chat traffic and must not contribute a level. The request
        // pipeline filters the candidate plan by the requested effort, so a level
        // is safe to publish when at least one usable member accepts it.
        let classes: Vec<TierClass> = ModelTier::ALL
            .iter()
            .filter_map(|tier| {
                let members: Vec<&zroutery_core::config::ModelEntry> = config
                    .models
                    .iter()
                    .filter(|model| {
                        model.enabled
                            && model.tier == Some(*tier)
                            && config.provider(&model.provider_id).is_some_and(|provider| {
                                provider.enabled && !provider.kind.is_decision()
                            })
                    })
                    .collect();
                if members.is_empty() {
                    return None;
                }
                // Keep canonical picker order and publish no levels when none
                // of the usable members declares an accepted native effort.
                let effort_levels: Vec<String> = zroutery_core::config::EFFORT_LEVELS
                    .iter()
                    .filter(|level| {
                        members.iter().any(|model| {
                            model
                                .normalized_effort_levels()
                                .iter()
                                .any(|value| value == **level)
                        })
                    })
                    .map(|level| (*level).to_string())
                    .collect();
                Some(TierClass {
                    model: tier.virtual_id().to_string(),
                    display_name: format!(
                        "Zroutery · {}",
                        match tier {
                            ModelTier::Fast => "快速",
                            ModelTier::Standard => "标准",
                            ModelTier::Reasoning => "推理",
                            ModelTier::Frontier => "前沿",
                        }
                    ),
                    effort_levels,
                    capabilities: {
                        let mut common = members[0].capabilities.clone();
                        for member in &members[1..] {
                            common.tools &= member.capabilities.tools;
                            common.vision &= member.capabilities.vision;
                            common.thinking &= member.capabilities.thinking;
                            common.structured_output &= member.capabilities.structured_output;
                            common.audio &= member.capabilities.audio;
                            common.video &= member.capabilities.video;
                            common.files &= member.capabilities.files;
                        }
                        common
                    },
                    context_window: members
                        .iter()
                        .all(|model| model.context_window.is_some())
                        .then(|| {
                            members
                                .iter()
                                .filter_map(|model| model.context_window)
                                .min()
                        })
                        .flatten(),
                    max_output_tokens: members
                        .iter()
                        .all(|model| model.max_output_tokens.is_some())
                        .then(|| {
                            members
                                .iter()
                                .filter_map(|model| model.max_output_tokens)
                                .min()
                        })
                        .flatten(),
                    capability_source:
                        "configured enabled members; effort union filtered per request, shared capabilities and limits remain conservative, not client defaults"
                            .into(),
                })
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
            codex_catalogue_path: Some(
                self.config_dir
                    .join("takeover")
                    .join("codex-models.json")
                    .display()
                    .to_string(),
            ),
        })
    }

    fn setup_options(&self, agent: AgentType) -> ClientSetupOptions {
        let path = self
            .config_dir
            .join("takeover")
            .join(format!("{}-selection.json", agent.client_name()));
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    fn target_for(
        &self,
        agent: AgentType,
        options: Option<&ClientSetupOptions>,
    ) -> Option<GatewayTarget> {
        let mut target = self.gateway_target()?;
        let saved = self.setup_options(agent);
        let options = options.unwrap_or(&saved);
        target.default_class = options.default_class.clone();
        if options.standby {
            target.base_url = "http://127.0.0.1:8788".into();
        }
        Some(target)
    }

    pub fn client_setup_preview(
        &self,
        agent: AgentType,
        options: &ClientSetupOptions,
    ) -> Result<ClientSetupPreview, String> {
        let _files = lock(&self.client_files);
        self.recover_client_files_locked()?;
        let adapter = adapter_for(agent).ok_or("this client is not supported for setup")?;
        let target = self
            .target_for(agent, Some(options))
            .ok_or("configure the gateway token and at least one tier first")?;
        let snapshot = adapter.read_config()?;
        let fields = takeover_fields(agent, &target, Some(&snapshot.raw))?;
        let paths: Vec<_> = fields.iter().map(|field| field.path.clone()).collect();
        let managed = fields
            .into_iter()
            .map(|field| (field.path, field.value))
            .collect();
        let current = extract_field_values(&snapshot.raw, &paths);
        let store = self
            .takeover_store(agent)
            .map_err(|status| format!("cannot open ownership: {}", status.name()))?;
        let mut proposal = store.plan_adopt(&paths, &current, &managed)?;
        self.bind_setup_proposal(agent, &mut proposal, &snapshot, &target)?;
        let mut notes = Vec::new();
        if agent == AgentType::Gemini {
            notes.extend(super::client_environment::gemini_notes(
                &snapshot.config_path,
            ));
        }
        if agent == AgentType::Claude
            && target.default_class.is_none()
            && snapshot.raw.pointer("/env/ANTHROPIC_MODEL").is_some()
        {
            notes.push("ANTHROPIC_MODEL in the client settings overrides settings.model; choose an explicit default to manage it, or keep the existing session intentionally".into());
        }
        if agent == AgentType::Codex {
            notes.push("Codex applies model_catalog_json at process startup; reopen the client after synchronizing".into());
        }
        if target
            .classes
            .iter()
            .any(|class| class.context_window.is_none() || class.max_output_tokens.is_none())
        {
            notes.push("some shared context/output limits are unknown; client fallback limits are not detected capabilities".into());
        }
        Ok(ClientSetupPreview {
            agent,
            unavailable_classes: ModelTier::ALL
                .iter()
                .filter(|tier| {
                    !target
                        .classes
                        .iter()
                        .any(|class| class.model == tier.virtual_id())
                })
                .map(|tier| tier.virtual_id().to_string())
                .collect(),
            catalogue_fingerprint: zroutery_core::agent_takeover::catalogue_fingerprint(
                &target.classes,
            ),
            classes: target.classes,
            proposal,
            config_path: snapshot.config_path.display().to_string(),
            gateway_url: target.base_url,
            notes,
        })
    }

    /// What this call can vouch for about a held takeover.
    ///
    /// Authenticated chat arrivals after this adopt confirm the client's route
    /// to the gateway; upstream success is a separate concern. Unknown clients
    /// and probes cannot confirm it. Also check the credential and managed
    /// fields: if the gateway token was regenerated after the adopt, the
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
        let mut report = verification_from_config(
            agent,
            target,
            &snapshot.raw,
            self.core.stats(),
            store.manifest().and_then(|manifest| manifest.adopted_at),
        );
        if agent == AgentType::Gemini {
            let notes = super::client_environment::gemini_notes(&snapshot.config_path);
            if report.request_confirmed != Some(true) && notes.len() > 1 {
                report.config_matches = false;
            }
            report.notes.extend(notes);
        }
        if agent == AgentType::Codex {
            let expected = serde_json::to_vec_pretty(
                &zroutery_core::agent_takeover::codex_catalogue(&target.classes),
            );
            let path = self.config_dir.join("takeover").join("codex-models.json");
            let matches = expected.ok().is_some_and(|expected| {
                std::fs::read(&path).is_ok_and(|actual| actual == expected)
            });
            if !matches {
                report.config_matches = false;
                report.notes.push("the Codex model catalogue is missing or changed; synchronize it and reopen the client".into());
            }
        }
        Some(report)
    }

    /// Every agent's takeover reading, in `AgentType::ALL` order.
    pub(crate) fn takeover_views(&self) -> Vec<(AgentType, TakeoverStatus)> {
        let _files = lock(&self.client_files);
        if let Err(reason) = self.recover_client_files_locked() {
            return AgentType::ALL
                .iter()
                .map(|agent| {
                    (
                        *agent,
                        TakeoverStatus::Error {
                            reason: reason.clone(),
                        },
                    )
                })
                .collect();
        }
        AgentType::ALL
            .iter()
            .map(|&agent| {
                let target = self.target_for(agent, None);
                let status = match self.takeover_store(agent) {
                    Err(status) => status,
                    Ok(store) => {
                        let adapter = adapter_for(agent);
                        let verified = self.verification_for(agent, target.as_ref());
                        let mut status =
                            store.inspect(adapter.as_deref(), target.as_ref(), verified.as_ref());
                        if matches!(status, TakeoverStatus::Ready { .. }) {
                            if let (Some(adapter), Some(target)) =
                                (adapter.as_deref(), target.as_ref())
                            {
                                let refreshed = (|| {
                                    let snapshot = adapter.read_config()?;
                                    let fields =
                                        takeover_fields(agent, target, Some(&snapshot.raw))?;
                                    let paths: Vec<_> =
                                        fields.iter().map(|field| field.path.clone()).collect();
                                    let current = extract_field_values(&snapshot.raw, &paths);
                                    let values = fields
                                        .into_iter()
                                        .map(|field| (field.path, field.value))
                                        .collect();
                                    let mut proposal =
                                        store.plan_adopt(&paths, &current, &values)?;
                                    self.bind_setup_proposal(
                                        agent,
                                        &mut proposal,
                                        &snapshot,
                                        target,
                                    )?;
                                    Ok::<_, String>(TakeoverStatus::Ready {
                                        config_path: snapshot.config_path.display().to_string(),
                                        proposal: Box::new(proposal),
                                    })
                                })();
                                status = refreshed
                                    .unwrap_or_else(|reason| TakeoverStatus::Error { reason });
                            }
                        }
                        status
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
        self.client_setup_apply(agent, expected_fingerprint, &self.setup_options(agent))
    }

    pub fn client_setup_apply(
        &self,
        agent: AgentType,
        expected_fingerprint: Option<&str>,
        options: &ClientSetupOptions,
    ) -> Result<TakeoverStatus, String> {
        let _files = lock(&self.client_files);
        self.recover_client_files_locked()?;
        let adapter = adapter_for(agent)
            .ok_or_else(|| format!("this build has no takeover adapter for {agent:?}"))?;
        let target = self.target_for(agent, Some(options)).ok_or_else(|| {
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
        let mut proposal = store.plan_adopt(&paths, &current, &managed_values)?;
        self.bind_setup_proposal(agent, &mut proposal, &snapshot, &target)?;
        if let Some(expected) = expected_fingerprint {
            if expected != proposal.fingerprint {
                return Err(
                    "the agent's config changed since the takeover was previewed. Refresh and \
                     review the changes again before confirming."
                        .to_string(),
                );
            }
        }

        if agent == AgentType::Gemini {
            if let Some(manifest) = store.manifest().filter(|manifest| {
                manifest.state == zroutery_core::agent_takeover::OwnershipState::Adopted
            }) {
                if manifest
                    .managed_fields
                    .iter()
                    .any(|path| !path.starts_with("settings.") && !path.starts_with("env."))
                {
                    return Err("release the legacy settings-only Gemini takeover before adopting the two-file setup".into());
                }
            }
        }

        let ownership = store.prepare_adoption(&paths, &current, &managed_values)?;
        let (_, mut changes) = adapter.prepare_patch(&snapshot, &fields)?;
        // Auxiliary files needed by the client must exist before the new
        // config points at them; recovery still covers the complete set.
        if agent == AgentType::Codex {
            let mut catalogue = self.prepare_codex_catalogue(
                &zroutery_core::agent_takeover::codex_catalogue(&target.classes),
            )?;
            catalogue.append(&mut changes);
            changes = catalogue;
        }
        if let Some(manifest) = ownership.file {
            changes.push(manifest);
        }
        let selection = self
            .config_dir
            .join("takeover")
            .join(format!("{}-selection.json", agent.client_name()));
        changes.push(FileChange {
            before: super::client_catalogue::read_optional(&selection)?,
            path: selection,
            after: Some(serde_json::to_vec_pretty(options).map_err(|error| error.to_string())?),
        });
        // Every live file, original-value manifest and selected default is
        // staged before the first publication. A crash rolls the set forward.
        file_transaction::commit(&self.client_journal_dir(), &changes)?;
        store.reload()?;
        self.core
            .stats()
            .forget_client_observation(attribution_id(agent));

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
        let _files = lock(&self.client_files);
        self.recover_client_files_locked()?;
        let adapter = adapter_for(agent)
            .ok_or_else(|| format!("this build has no takeover adapter for {agent:?}"))?;
        let store = self.takeover_store(agent).map_err(|s| match s {
            TakeoverStatus::Error { reason } => reason,
            other => format!("unexpected takeover state: {other:?}"),
        })?;
        let snapshot = adapter.read_config()?;
        let manifest = store.manifest().ok_or("client ownership is missing")?;
        let raw = if agent == AgentType::Gemini {
            legacy_gemini_view(&snapshot.raw)
        } else {
            snapshot.raw.clone()
        };
        let current = extract_field_values(&raw, &manifest.managed_fields);
        let prepared = store.prepare_release_transaction(&current, strategy)?;
        let mut changes = adapter.prepare_release(&snapshot, &prepared.effective)?;
        if agent == AgentType::Codex {
            changes.extend(self.prepare_codex_restore(matches!(
                strategy,
                ConflictResolution::KeepExternal | ConflictResolution::Skip
            ))?);
        }
        if let Some(manifest) = prepared.ownership.file {
            changes.push(manifest);
        }
        file_transaction::commit(&self.client_journal_dir(), &changes)?;
        store.reload()?;
        let status = store.inspect(
            Some(adapter.as_ref()),
            self.target_for(agent, None).as_ref(),
            None,
        );
        Ok((prepared.report, prepared.conflicts, status))
    }
}

/// Verify a snapshot without opening or rewriting the user's client config.
fn verification_from_config(
    agent: AgentType,
    target: &GatewayTarget,
    raw: &serde_json::Value,
    stats: &zroutery_core::stats::Stats,
    adopted_at: Option<i64>,
) -> VerificationReport {
    let mut report = VerificationReport::unchecked();
    let Ok(fields) = takeover_fields(agent, target, Some(raw)) else {
        return report;
    };
    report.notes.clear();
    // Every managed field the current gateway would write must still be on
    // disk with that value. The token alone is not enough: a client adopted
    // before a new field existed keeps a config the current target no
    // longer describes (the effort variants are the case that exposed
    // this), and the status must say the config needs refreshing rather
    // than call it healthy.
    let paths: Vec<String> = fields.iter().map(|field| field.path.clone()).collect();
    let live = extract_field_values(raw, &paths);
    report.config_matches = fields
        .iter()
        .all(|field| field.matches_value(live.get(&field.path)));
    if !report.config_matches {
        report.notes.push(
            "the client config does not match what this build manages — release and take \
                 the client over again to write the current field set"
                .into(),
        );
    }

    let token_paths: Vec<String> = fields
        .iter()
        .filter(|f| f.value == serde_json::json!(target.token))
        .map(|f| f.path.clone())
        .collect();
    let live_token = extract_field_values(raw, &token_paths);
    report.credential_ok = !token_paths.is_empty()
        && token_paths
            .iter()
            .all(|p| live_token.get(p) == Some(&serde_json::json!(target.token)));
    if !report.credential_ok {
        report.notes.push(
            "the gateway token has changed since this client was taken over — \
                 release and take over again to hand the client the new one"
                .into(),
        );
    }
    let client_id = attribution_id(agent);
    if adopted_at
        .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
        .is_some_and(|at| stats.client_seen_since(client_id, at))
    {
        report.request_confirmed = Some(true);
    }
    if report.request_confirmed.is_none() {
        report
            .notes
            .push("this takeover has not been verified by a request from this client yet".into());
    }
    report
}

fn attribution_id(agent: AgentType) -> &'static str {
    match agent {
        AgentType::Claude => "claude_code",
        AgentType::Codex => "codex",
        AgentType::OpenCode => "opencode",
        AgentType::Gemini => "gemini",
    }
}

fn legacy_gemini_view(raw: &serde_json::Value) -> serde_json::Value {
    let mut view = raw.clone();
    if let (Some(settings), Some(root)) = (
        raw.get("settings").and_then(serde_json::Value::as_object),
        view.as_object_mut(),
    ) {
        for (key, value) in settings {
            root.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    view
}

/// `fast` → `Fast`, for a model picker that shows words, not enum names.
#[cfg(test)]
mod verification_tests {
    use super::*;
    use zroutery_core::client::identify;
    use zroutery_core::stats::Stats;

    #[test]
    fn setup_fingerprint_covers_desired_values_catalogue_and_ownership() {
        let (desktop, directory) = super::super::tests::desktop_with(Default::default());
        let (mut target, _, _, _) = fixture();
        let snapshot = zroutery_core::agent_takeover::AgentConfigSnapshot {
            agent_type: AgentType::Codex,
            config_path: directory.join("isolated-config.toml"),
            config_hash: "fixed".into(),
            raw: serde_json::json!({}),
        };
        let fingerprint = |target: &GatewayTarget| {
            let fields = takeover_fields(AgentType::Codex, target, Some(&snapshot.raw)).unwrap();
            let paths: Vec<_> = fields.iter().map(|field| field.path.clone()).collect();
            let managed = fields
                .into_iter()
                .map(|field| (field.path, field.value))
                .collect();
            let mut proposal = TakeoverStore::new()
                .plan_adopt(&paths, &HashMap::new(), &managed)
                .unwrap();
            desktop
                .bind_setup_proposal(AgentType::Codex, &mut proposal, &snapshot, target)
                .unwrap();
            proposal.fingerprint
        };
        target.codex_catalogue_path = Some(
            directory
                .join("takeover")
                .join("codex-models.json")
                .display()
                .to_string(),
        );
        let original = fingerprint(&target);
        assert_eq!(fingerprint(&target), original);
        target.default_class = Some("standard-class".into());
        assert_ne!(fingerprint(&target), original);
        target.default_class = None;
        std::fs::create_dir_all(directory.join("takeover")).unwrap();
        std::fs::write(
            directory.join("takeover").join("codex-models.json"),
            b"{\"models\":[]}",
        )
        .unwrap();
        let catalogue_changed = fingerprint(&target);
        assert_ne!(catalogue_changed, original);
        std::fs::write(directory.join("takeover").join("codex.json"), b"{}").unwrap();
        assert_ne!(fingerprint(&target), catalogue_changed);
        drop(desktop);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn fixture() -> (GatewayTarget, serde_json::Value, Stats, i64) {
        let target = GatewayTarget {
            base_url: "http://127.0.0.1:8787".into(),
            token: "test-gateway-token".into(),
            classes: vec![TierClass {
                model: "standard-class".into(),
                display_name: "Standard".into(),
                effort_levels: Vec::new(),
                capabilities: Default::default(),
                context_window: None,
                max_output_tokens: None,
                capability_source: "test".into(),
            }],
            default_class: None,
            codex_catalogue_path: None,
        };
        let mut raw = serde_json::json!({});
        for field in takeover_fields(AgentType::OpenCode, &target, Some(&raw)).unwrap() {
            let path: Vec<&str> = field.path.split('.').collect();
            let mut cursor = &mut raw;
            for segment in &path[..path.len() - 1] {
                cursor = cursor
                    .as_object_mut()
                    .unwrap()
                    .entry((*segment).to_string())
                    .or_insert_with(|| serde_json::json!({}));
            }
            cursor
                .as_object_mut()
                .unwrap()
                .insert(path[path.len() - 1].to_string(), field.value);
        }
        (
            target,
            raw,
            Stats::new(1),
            chrono::Utc::now().timestamp() - 10,
        )
    }

    fn observe(stats: &Stats, at: i64) {
        let mut headers = tauri::http::HeaderMap::new();
        headers.insert("x-opencode-client", "cli".parse().unwrap());
        headers.insert("x-opencode-session", "test-session".parse().unwrap());
        stats.observe_client(
            &identify(&headers, None),
            chrono::DateTime::from_timestamp(at, 0).unwrap(),
        );
    }

    #[test]
    fn an_opencode_request_after_adopt_confirms_the_matching_config() {
        let (target, raw, stats, adopted) = fixture();
        let check =
            || verification_from_config(AgentType::OpenCode, &target, &raw, &stats, Some(adopted));
        assert_eq!(check().request_confirmed, None);
        observe(&stats, adopted - 1);
        assert_eq!(
            check().request_confirmed,
            None,
            "old traffic cannot confirm a new adopt"
        );
        observe(&stats, adopted + 1);
        let report = check();
        assert!(report.config_matches && report.credential_ok);
        assert_eq!(report.request_confirmed, Some(true));
        assert!(report.notes.is_empty());
        stats.clear();
        assert_eq!(check().request_confirmed, Some(true));
    }

    #[test]
    fn traffic_does_not_hide_a_changed_token_or_managed_field() {
        let (target, mut raw, stats, adopted) = fixture();
        observe(&stats, adopted + 1);
        raw["providers"]["zroutery"]["settings"]["apiKey"] = serde_json::json!("old-token");
        let report =
            verification_from_config(AgentType::OpenCode, &target, &raw, &stats, Some(adopted));
        assert!(!report.credential_ok);
        assert!(!report.config_matches);
        assert_eq!(report.request_confirmed, Some(true));
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("token has changed")));
        raw["providers"]["zroutery"]["settings"]["apiKey"] = serde_json::json!(target.token);
        raw["providers"]["zroutery"]["settings"]["baseURL"] =
            serde_json::json!("http://elsewhere.invalid/v1");
        let report =
            verification_from_config(AgentType::OpenCode, &target, &raw, &stats, Some(adopted));
        assert!(report.credential_ok);
        assert!(!report.config_matches);
        assert!(!report.notes.is_empty());
    }
}
