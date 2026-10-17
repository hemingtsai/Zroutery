//! What `AppConfig::validate` checks, and the issues it reports.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::app::AppConfig;
use super::model::ModelCapabilities;
use super::model::ModelTier;
use super::provider::ProviderClientProfile;
use super::registry::qualified_id;
use super::registry::{ModelEntry, ProviderConfig};

/// A problem found by [`AppConfig::validate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigIssue {
    pub severity: IssueSeverity,
    pub code: String,
    pub message: String,
    /// Model id or provider id the issue refers to.
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Error,
    Warning,
}

impl AppConfig {
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn model(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.answers_to(id))
    }

    /// Look a model up by identity rather than by exposed id.
    pub fn model_by_key(&self, provider_id: &str, upstream_model: &str) -> Option<&ModelEntry> {
        self.models
            .iter()
            .find(|m| m.provider_id == provider_id && m.upstream_model == upstream_model)
    }

    /// The exposed ids of every configured model, in order.
    ///
    /// The GUI renders these instead of deriving ids itself, so the naming rule
    /// has exactly one implementation.
    pub fn exposed_ids(&self) -> Vec<String> {
        self.models.iter().map(|m| m.exposed_id()).collect()
    }

    /// Fold configurations written before ids were derived into the current
    /// shape, and tidy the alias lists.
    ///
    /// Must be called after every deserialization (including from the config
    /// file) so that legacy fields like `break_after_failures` and `cooldown_secs`
    /// are migrated into their current equivalents.  [`AppConfig::validate`]
    /// runs after this and only sees the post-migration state.
    ///
    /// Returns a note for every id that moved, so the UI can explain itself.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        self.routing.apply_legacy();
        for provider in &mut self.providers {
            #[allow(deprecated)]
            if provider.impersonate_claude_code {
                provider.client_profile = ProviderClientProfile::ClaudeCode;
                provider.impersonate_claude_code = false;
                notes.push(format!(
                    "Provider `{}` migrated from the Claude Code impersonation switch to the client profile",
                    provider.name
                ));
            }
        }
        for model in &mut self.models {
            let exposed = qualified_id(&model.provider_id, &model.upstream_model);
            if let Some(legacy) = model.legacy_id.take() {
                let legacy = legacy.trim().to_string();
                if !legacy.is_empty() && legacy != exposed && !model.aliases.contains(&legacy) {
                    model.aliases.push(legacy.clone());
                    notes.push(format!(
                        "`{legacy}` is now exposed as `{exposed}`; the old id still works as an alias"
                    ));
                }
            }
            model
                .aliases
                .retain(|a| !a.trim().is_empty() && a != &exposed);
            model.aliases.sort();
            model.aliases.dedup();

            // Migrate old supports_* fields into capabilities when capabilities
            // is still at its default (all false) and the old fields are set.
            #[allow(deprecated)]
            if model.capabilities == ModelCapabilities::default()
                && (model.supports_tools || model.supports_vision || model.supports_thinking)
            {
                model.capabilities.tools = model.supports_tools;
                model.capabilities.vision = model.supports_vision;
                model.capabilities.thinking = model.supports_thinking;
            }
            // Clear deprecated fields so they don't persist in new configs.
            #[allow(deprecated)]
            {
                model.supports_tools = false;
                model.supports_vision = false;
                model.supports_thinking = false;
            }
        }
        for budget in &mut self.budgets {
            if budget.id.trim().is_empty() {
                budget.id = format!("budget_{}", uuid::Uuid::new_v4().simple());
            }
        }
        notes
    }

    /// Models that the user still has to classify. The GUI nags about these.
    pub fn unclassified_models(&self) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| m.tier.is_none()).collect()
    }

    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();
        let mut seen_ids: BTreeMap<String, usize> = BTreeMap::new();
        let mut seen_providers: BTreeMap<String, usize> = BTreeMap::new();

        for p in &self.providers {
            *seen_providers.entry(p.id.clone()).or_insert(0) += 1;
        }
        for (id, count) in seen_providers {
            if count > 1 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.duplicate_id".into(),
                    message: format!(
                        "Provider id `{id}` is used {count} times; provider ids have to be unique"
                    ),
                    subject: Some(id),
                });
            }
        }

        if self.server.allow_cors {
            for o in &self.server.cors_origins {
                let trimmed = o.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !crate::server::is_valid_origin(trimmed) {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "server.cors_origin_invalid".into(),
                        message: format!(
                            "`{o}` is not a valid CORS origin (expected scheme://host[:port]); \
                             invalid entries are ignored, and if none are valid no origin is \
                             allowed at all"
                        ),
                        subject: None,
                    });
                }
            }
        }

        for p in &self.providers {
            if p.kind.is_opencode()
                && p.configured_client_profile() != ProviderClientProfile::OpenCodeCli
            {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.opencode_profile_locked".into(),
                    message: format!(
                        "Provider `{}` targets OpenCode and must use the `opencode_cli` client \
                         profile; its fingerprint is locked",
                        p.name
                    ),
                    subject: Some(p.id.clone()),
                });
            }
            if p.base_url.trim().is_empty() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.base_url_empty".into(),
                    message: format!("Provider `{}` has no base url", p.name),
                    subject: Some(p.id.clone()),
                });
            }
            if p.timeout_secs == 0 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.zero_timeout".into(),
                    message: format!(
                        "Provider `{}` has a zero timeout, so every request would time out \
                         before it is sent",
                        p.name
                    ),
                    subject: Some(p.id.clone()),
                });
            }
            let managed_headers = ["content-type", "authorization", "x-api-key", "accept"];
            for key in p.extra_headers.keys() {
                if managed_headers.contains(&key.to_lowercase().as_str()) {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Warning,
                        code: "provider.extra_header_override".into(),
                        message: format!(
                            "Provider `{}` has `extra_headers` setting `{}`, which Zroutery \
                             manages itself; the override may cause unexpected behavior",
                            p.name, key
                        ),
                        subject: Some(p.id.clone()),
                    });
                }
            }
        }

        for m in &self.models {
            let exposed = m.exposed_id();
            *seen_ids.entry(exposed.clone()).or_insert(0) += 1;
            for alias in &m.aliases {
                *seen_ids.entry(alias.clone()).or_insert(0) += 1;
            }
            if self.provider(&m.provider_id).is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.orphan".into(),
                    message: format!(
                        "Model `{}` points at missing provider `{}`",
                        m.upstream_model, m.provider_id
                    ),
                    subject: Some(exposed.clone()),
                });
            }
            if m.upstream_model.trim().is_empty() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.no_upstream".into(),
                    message: format!("A model on provider `{}` has no name", m.provider_id),
                    subject: Some(exposed.clone()),
                });
            }
            for reserved in std::iter::once(&exposed).chain(m.aliases.iter()) {
                if ModelTier::from_virtual_id(reserved).is_some() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "model.reserved_id".into(),
                        message: format!("`{reserved}` is a reserved virtual model id"),
                        subject: Some(exposed.clone()),
                    });
                }
            }
            if m.tier.is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "model.unclassified".into(),
                    message: format!(
                        "Model `{exposed}` has no tier yet, so it is excluded from *-class routing"
                    ),
                    subject: Some(exposed.clone()),
                });
            }
            if let Some(pricing) = &m.pricing {
                for problem in pricing.problems() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "model.bad_pricing".into(),
                        message: format!("The price for `{exposed}` {problem}"),
                        subject: Some(exposed.clone()),
                    });
                }
            }
        }

        for (id, count) in seen_ids {
            if count > 1 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.duplicate_id".into(),
                    message: format!(
                        "`{id}` resolves to {count} models; the same provider and model must not \
                         be listed twice, and aliases have to be unique"
                    ),
                    subject: Some(id),
                });
            }
        }

        for tier in ModelTier::ALL {
            let has = self.models.iter().any(|m| {
                m.enabled
                    && m.tier == Some(tier)
                    && self
                        .provider(&m.provider_id)
                        .map(|p| p.enabled)
                        .unwrap_or(false)
            });
            if !has {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "tier.empty".into(),
                    message: format!(
                        "No enabled model is assigned to `{}`, requests to it will fail",
                        tier.virtual_id()
                    ),
                    subject: Some(tier.virtual_id().to_string()),
                });
            }
        }

        // Classifier candidates reference models by exposed id or alias; a
        // dangling reference means a classifier request would fail at routing
        // time, which is better caught at save time.
        if self.classifier.enabled {
            if !(0.0..=1.0).contains(&self.classifier.detection.minimum_confidence) {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "classifier.confidence_out_of_range".into(),
                    message: format!(
                        "Classifier `minimum_confidence` is {}, which is outside the expected \
                         range of 0.0 to 1.0; the detector may never fire (or always fire)",
                        self.classifier.detection.minimum_confidence,
                    ),
                    subject: None,
                });
            }
            for candidate in &self.classifier.candidates {
                if candidate.model.trim().is_empty() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "classifier.candidate_empty".into(),
                        message: "A classifier candidate has no model id".into(),
                        subject: None,
                    });
                    continue;
                }
                let entry = self.model(&candidate.model);
                if let Some(entry) = entry {
                    let exposed = entry.exposed_id();
                    if !entry.enabled {
                        issues.push(ConfigIssue {
                            severity: IssueSeverity::Warning,
                            code: "classifier.candidate_disabled".into(),
                            message: format!(
                                "Classifier candidate `{}` is a disabled model",
                                candidate.model
                            ),
                            subject: Some(exposed),
                        });
                    } else if self
                        .provider(&entry.provider_id)
                        .map(|p| !p.enabled)
                        .unwrap_or(true)
                    {
                        issues.push(ConfigIssue {
                            severity: IssueSeverity::Warning,
                            code: "classifier.candidate_provider_off".into(),
                            message: format!(
                                "Classifier candidate `{}` sits on a disabled provider",
                                candidate.model
                            ),
                            subject: Some(exposed),
                        });
                    }
                } else {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "classifier.candidate_unknown".into(),
                        message: format!(
                            "Classifier candidate `{}` is not a configured model",
                            candidate.model
                        ),
                        subject: Some(candidate.model.clone()),
                    });
                }
            }
            if self.classifier.candidates.iter().all(|c| !c.enabled) {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "classifier.no_candidates".into(),
                    message: "Classifier routing is on but no candidate is enabled, so \
                              classifier requests will fail"
                        .into(),
                    subject: None,
                });
            }
        }

        if self.server.is_exposed() {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.exposed".into(),
                message: format!(
                    "Server binds {} which is reachable from the network; keep authentication on",
                    self.server.host
                ),
                subject: None,
            });
        }
        if !self.server.require_auth {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.no_auth".into(),
                message: "Authentication is disabled: any local process can spend your API credit"
                    .into(),
                subject: None,
            });
        } else if self.server.auth_token.trim().is_empty() {
            // Auth that rejects everyone looks like a broken proxy from the
            // outside; say so instead of letting the user debug their client.
            issues.push(ConfigIssue {
                severity: IssueSeverity::Error,
                code: "server.empty_token".into(),
                message: "Authentication is required but the token is empty, so every request \
                          would be rejected"
                    .into(),
                subject: None,
            });
        }
        // Surface the dangerous combination explicitly: either warning alone is
        // recoverable, but no auth plus reachability (the network, or any website
        // when CORS is wide open) hands the proxy spending power to anything that
        // can reach the port.
        if !self.server.require_auth
            && (self.server.is_exposed() || self.server.cors_is_wide_open())
        {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.no_auth_reachable".into(),
                message: "Authentication is off while the proxy is reachable from the network or from any website via CORS; anything that can reach the port can spend your API credit".into(),
                subject: None,
            });
        }

        if self.server.cors_is_wide_open() {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.cors_any_origin".into(),
                message: "CORS is on with no origin list, so any website you visit can call the \
                          proxy from your browser; list the origins you need"
                    .into(),
                subject: None,
            });
        }

        for budget in &self.budgets {
            let subject = format!("{} / {}", budget.scope.label(), budget.period.label());
            for problem in budget.problems() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "budget.impossible".into(),
                    message: format!("The budget for {subject} {problem}"),
                    subject: Some(subject.clone()),
                });
            }
            // A limit in a currency nothing bills in can never be reached, which
            // looks like protection without being any.
            let billed: Vec<&str> = self
                .models
                .iter()
                .filter_map(|m| m.pricing.as_ref().map(|p| p.currency.as_str()))
                .collect();
            if !billed.is_empty() && !billed.contains(&budget.limit.currency.as_str()) {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "budget.unused_currency".into(),
                    message: format!(
                        "The budget for {subject} counts {}, which none of your models bill in, \
                         so it can never be reached",
                        budget.limit.currency
                    ),
                    subject: Some(subject.clone()),
                });
            }
            if let crate::budget::BudgetScope::Provider { id } = &budget.scope {
                if self.provider(id).is_none() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Warning,
                        code: "budget.orphan".into(),
                        message: format!("The budget for {subject} names a missing provider"),
                        subject: Some(subject.clone()),
                    });
                }
            }
            if let crate::budget::OnExceeded::Degrade { to } = &budget.on_exceeded {
                let reachable = self.models.iter().any(|m| m.enabled && m.tier == Some(*to));
                if !reachable {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Warning,
                        code: "budget.degrade_nowhere".into(),
                        message: format!(
                            "The budget for {subject} degrades to {}, which has no enabled model",
                            to.virtual_id()
                        ),
                        subject: Some(subject),
                    });
                }
            }
        }

        issues
    }
}
