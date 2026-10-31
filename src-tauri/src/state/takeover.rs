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
        // A class offers the intersection of its usable members' effort levels.
        // Members whose provider is disabled, or that are decision endpoints,
        // cannot serve chat traffic and must not contribute a level.
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
                // A level the class publishes must be one **every** member
                // accepts, not merely one member. A union offered `max` to a
                // picker whose class also holds a model that tops out at
                // `high`; the router served the class from that member, the
                // client sent `reasoning_effort: max`, and the upstream
                // refused the request. An intersection shrinks the picker
                // instead of failing the request.
                let effort_levels: Vec<String> = zroutery_core::config::EFFORT_LEVELS
                    .iter()
                    .filter(|level| {
                        members.iter().all(|model| {
                            model
                                .normalized_effort_levels()
                                .iter()
                                .any(|value| value == **level)
                        })
                    })
                    .map(|level| (*level).to_string())
                    .collect();
                Some(TierClass {
                    model: tier.virtual_id_styled(style).to_string(),
                    display_name: capitalize(tier.as_str()),
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
                        .then(|| members.iter().filter_map(|model| model.context_window).min())
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
                    capability_source: "configured enabled members; shared capabilities and limits use conservative intersection and minimum evidence".into(),
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
            // The later desktop setup transaction owns and publishes this
            // auxiliary file. Do not point Codex at a file this slice does not
            // create and recover.
            codex_catalogue_path: None,
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
        Some(verification_from_config(
            agent,
            target,
            &snapshot.raw,
            self.core.stats(),
            store.manifest().and_then(|manifest| manifest.adopted_at),
        ))
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

/// `fast` → `Fast`, for a model picker that shows words, not enum names.
fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod verification_tests {
    use super::*;
    use zroutery_core::client::identify;
    use zroutery_core::stats::Stats;

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
                capability_source: "test fixture with no capability evidence".into(),
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
