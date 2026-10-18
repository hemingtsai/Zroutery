//! What actually ran, as distinct from what Zroutery asked for.
//!
//! # The distinction this type exists to keep
//!
//! Routing identity and execution identity are different questions and the proxy
//! currently answers only the first one. `OutcomeIdentity` (`crate::outcome`)
//! records `planned` / `last_attempted` / `served` — *which configured model Zroutery
//! selected*. Nothing anywhere recorded *which backend the provider actually ran*.
//!
//! That gap is load-bearing. A relay that fronts a pool of backends behind a single
//! name will happily answer `gpt-4o` for every one of them. `served_model` then says
//! `gpt-4o` whether the request landed on the model that was named or on the cheapest
//! node in the pool, and any per-model quality statistic fitted on top of it is
//! measuring the relay's routing policy, not the model.
//!
//! # What is recorded, and what is refused
//!
//! Four layered fields, each independently `Observed` or `Unavailable`:
//!
//! * [`requested_model`](ExecutionIdentity::requested_model) — always present; it is
//!   the client's logical request and is known by construction.
//! * `provider_declared_model` — only what the upstream body actually declared.
//! * `observed_execution_fingerprint` — only a real fingerprint or a behavioural
//!   grouping with a stated derivation.
//! * [`provenance`](ExecutionIdentity::provenance) — per-field source and confidence.
//!
//! The load-bearing rule is the one enforced by [`IdentityEvidence::Unavailable`]:
//! an unobservable field is recorded as **unavailable with a reason**. It is never
//! filled in from the requested model, never defaulted to the configured model name,
//! and never inferred from latency. A fabricated "real model id" is worse than an
//! honest unknown, because an unknown is visible in a data-quality report and a
//! fabrication is not.
//!
//! # Why the proxy overwrites the declared model
//!
//! `server/pipeline.rs` replaces the upstream `model` field with the client-facing
//! `exposed_id` before encoding, so that a client asking for a Zroutery model id does
//! not learn which relay served it. That behaviour is correct for the wire and is
//! kept exactly as it is. The fix is to *copy* the declared value into this type
//! before the overwrite, never to change what the client sees.

use serde::{Deserialize, Serialize};

/// Schema version of [`ExecutionIdentity`].
pub const EXECUTION_IDENTITY_SCHEMA_VERSION: u32 = 1;

/// Where an observed identity value was obtained.
///
/// This is recorded so a later reader can tell a value the provider stated from one
/// Zroutery derived. A derived fingerprint is evidence about *a group of backends*,
/// not about a named model, and conflating the two is the failure this enum exists
/// to make visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    /// The value appeared verbatim in the upstream response body.
    ResponseBody,
    /// The value came from an upstream response header.
    ResponseHeader,
    /// Zroutery derived the value by grouping observations behaviourally.
    DerivedBehavioural,
    /// The value is Zroutery's own routing identity, not an observation.
    RoutingIdentity,
}

impl IdentitySource {
    /// Whether a source reports the provider's own claim rather than a derivation.
    ///
    /// A `DerivedBehavioural` fingerprint says "these requests behaved alike", which
    /// is weaker than "these requests ran on model X" and must not be promoted into
    /// the same column when a quality statistic is cut.
    pub fn is_provider_declared(self) -> bool {
        matches!(self, Self::ResponseBody | Self::ResponseHeader)
    }
}

/// Why an identity field could not be observed.
///
/// Every variant is a *fact about the provider*, not a placeholder. "The provider did
/// not send it" and "we failed to parse it" are different findings and an operator
/// diagnosing a data gap needs to tell them apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum UnavailableReason {
    /// The provider returned a response with no model identifier in it.
    NotDeclaredByProvider,
    /// The provider sent one, but the protocol decoder has no field for it.
    FieldNotParsed,
    /// The value exists but is empty, whitespace, or a placeholder.
    EmptyValue,
    /// The upstream metadata could not be read at all (parse error, truncated stream).
    ResponseUnreadable,
    /// The provider was never reached, so there is nothing to observe.
    UpstreamNotContacted,
}

impl UnavailableReason {
    /// Whether this reason reflects a provider limitation rather than a local fault.
    ///
    /// Used by the data-quality audit to separate "we could not have recorded this"
    /// from "we failed to record it".
    pub fn is_provider_limitation(self) -> bool {
        matches!(
            self,
            Self::NotDeclaredByProvider | Self::FieldNotParsed | Self::ResponseUnreadable
        )
    }
}

/// One identity field: either a value with its provenance, or a stated absence.
///
/// Absence is a value here. Modelling it as `Option<String>` would make "we looked and
/// the provider did not say" indistinguishable from "nobody looked", and the first is
/// a finding while the second is a bug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum IdentityEvidence {
    /// The value was observed, from `source`, with `confidence` in `[0, 1]`.
    Observed {
        value: String,
        source: IdentitySource,
        /// Self-reported reliability of this field. A derived behavioural grouping
        /// is low-confidence by construction; a provider-declared name is higher but
        /// still only as good as the provider's honesty.
        confidence: f64,
    },
    /// The field could not be observed, for `reason`.
    Unavailable { reason: UnavailableReason },
}

impl IdentityEvidence {
    /// A provider-declared value with full confidence in *the observation*.
    ///
    /// Confidence here is about being able to read the field, not about whether the
    /// named backend is the one that ran — no provider can attest to that.
    pub fn declared(value: impl Into<String>) -> Self {
        Self::Observed {
            value: value.into(),
            source: IdentitySource::ResponseBody,
            confidence: 1.0,
        }
    }

    /// A derived behavioural grouping, deliberately low-confidence.
    pub fn derived(value: impl Into<String>) -> Self {
        Self::Observed {
            value: value.into(),
            source: IdentitySource::DerivedBehavioural,
            // A behavioural grouping is evidence of similarity, not of identity. It
            // must never be able to stand in for a provider-declared name.
            confidence: 0.25,
        }
    }

    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self::Unavailable { reason }
    }

    pub fn is_observed(&self) -> bool {
        matches!(self, Self::Observed { .. })
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// The observed string, if any. Absence is `None` — never a default.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Observed { value, .. } => Some(value.as_str()),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn source(&self) -> Option<IdentitySource> {
        match self {
            Self::Observed { source, .. } => Some(*source),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Observed { confidence, .. } => Some(*confidence),
            Self::Unavailable { .. } => None,
        }
    }

    /// Whether this field is a provider-declared claim rather than a derivation.
    pub fn is_provider_declared(&self) -> bool {
        self.source()
            .is_some_and(IdentitySource::is_provider_declared)
    }
}

/// The layered identity of one executed request.
///
/// Constructed from the routing decision plus whatever the upstream actually revealed.
/// The default is the honest baseline for a request that never reached an upstream:
/// everything unknown, each with a reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub schema_version: u32,
    /// The logical model the client asked for. Always known: Zroutery resolved it
    /// before the request was sent.
    pub requested_model: String,
    /// The model name the provider put in its response body, if any.
    pub provider_declared_model: IdentityEvidence,
    /// An observed execution fingerprint or behavioural grouping, if any.
    pub observed_execution_fingerprint: IdentityEvidence,
    /// The Zroutery candidate identity that was routed to. This is routing
    /// provenance, not execution provenance, and it is kept in its own field so it
    /// cannot be mistaken for the two above.
    pub routed_candidate: Option<String>,
    pub routed_provider: Option<String>,
}

impl ExecutionIdentity {
    /// A request that was planned but never reached an upstream.
    pub fn unrouted(requested_model: impl Into<String>) -> Self {
        Self {
            schema_version: EXECUTION_IDENTITY_SCHEMA_VERSION,
            requested_model: requested_model.into(),
            provider_declared_model: IdentityEvidence::unavailable(
                UnavailableReason::UpstreamNotContacted,
            ),
            observed_execution_fingerprint: IdentityEvidence::unavailable(
                UnavailableReason::UpstreamNotContacted,
            ),
            routed_candidate: None,
            routed_provider: None,
        }
    }

    /// Record what the upstream declared, and note when it declared nothing.
    ///
    /// `declared` is the value read off the upstream response **before** the proxy
    /// normalises it to the client-facing id. Passing `None` is the honest result for
    /// a provider that sends no model field, and it is recorded as
    /// [`UnavailableReason::NotDeclaredByProvider`] rather than being back-filled from
    /// the requested model.
    pub fn with_declared_model(mut self, declared: Option<&str>) -> Self {
        self.provider_declared_model = match declared.map(str::trim).filter(|s| !s.is_empty()) {
            Some(value) => IdentityEvidence::declared(value),
            None => IdentityEvidence::unavailable(UnavailableReason::NotDeclaredByProvider),
        };
        self
    }

    pub fn with_fingerprint(mut self, fingerprint: IdentityEvidence) -> Self {
        self.observed_execution_fingerprint = fingerprint;
        self
    }

    pub fn with_routed(mut self, candidate: Option<&str>, provider: Option<&str>) -> Self {
        self.routed_candidate = candidate.map(str::to_string);
        self.routed_provider = provider.map(str::to_string);
        self
    }

    /// Whether the provider confirmed the same model that was requested.
    ///
    /// `false` is the interesting answer: it is the case where a relay routed to a
    /// different backend than the client named. It is reported, never corrected.
    pub fn declared_matches_request(&self) -> Option<bool> {
        let declared = self.provider_declared_model.value()?;
        Some(declared == self.requested_model)
    }

    /// Count of fields that carry an observation, out of the two observable ones.
    ///
    /// Exposed as a ratio so a data-quality report can state execution-identity
    /// coverage without every caller recomputing it.
    pub fn coverage(&self) -> f64 {
        let observed = usize::from(self.provider_declared_model.is_observed())
            + usize::from(self.observed_execution_fingerprint.is_observed());
        observed as f64 / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrouted_request_records_why_it_knows_nothing() {
        let identity = ExecutionIdentity::unrouted("gpt-4o");
        assert_eq!(identity.requested_model, "gpt-4o");
        assert!(identity.provider_declared_model.is_unavailable());
        assert_eq!(
            identity.provider_declared_model,
            IdentityEvidence::Unavailable {
                reason: UnavailableReason::UpstreamNotContacted
            }
        );
        assert_eq!(identity.coverage(), 0.0);
    }

    #[test]
    fn a_declared_model_is_kept_verbatim_and_not_normalised() {
        // The proxy overwrites the wire value with `exposed_id`; this is the copy
        // taken before that happens.
        let identity =
            ExecutionIdentity::unrouted("zr-gpt-4o").with_declared_model(Some("gpt-4o-2024-11-20"));
        assert_eq!(
            identity.provider_declared_model.value(),
            Some("gpt-4o-2024-11-20")
        );
        assert!(identity.provider_declared_model.is_provider_declared());
        assert_eq!(identity.declared_matches_request(), Some(false));
    }

    #[test]
    fn a_provider_that_declares_nothing_stays_unknown() {
        // The critical anti-fabrication test: the requested model must NOT leak into
        // the declared field as a fallback.
        let identity = ExecutionIdentity::unrouted("gpt-4o").with_declared_model(None);
        assert!(identity.provider_declared_model.is_unavailable());
        assert_eq!(identity.provider_declared_model.value(), None);
        assert_eq!(
            identity.provider_declared_model,
            IdentityEvidence::Unavailable {
                reason: UnavailableReason::NotDeclaredByProvider
            }
        );
    }

    #[test]
    fn an_empty_declared_string_is_treated_as_not_declared() {
        let identity = ExecutionIdentity::unrouted("m").with_declared_model(Some("   "));
        assert_eq!(
            identity.provider_declared_model,
            IdentityEvidence::Unavailable {
                reason: UnavailableReason::NotDeclaredByProvider
            }
        );
    }

    #[test]
    fn a_derived_fingerprint_is_low_confidence_and_not_provider_declared() {
        let identity = ExecutionIdentity::unrouted("m")
            .with_declared_model(Some("m"))
            .with_fingerprint(IdentityEvidence::derived("cohort-a"));
        assert_eq!(
            identity.observed_execution_fingerprint.source(),
            Some(IdentitySource::DerivedBehavioural)
        );
        assert!(!identity
            .observed_execution_fingerprint
            .is_provider_declared());
        assert!(
            identity
                .observed_execution_fingerprint
                .confidence()
                .unwrap()
                < 0.5
        );
        assert_eq!(identity.coverage(), 1.0);
    }

    #[test]
    fn routing_identity_is_not_execution_identity() {
        let identity = ExecutionIdentity::unrouted("m")
            .with_routed(Some("openrouter-gpt-4o"), Some("openrouter"));
        assert_eq!(
            identity.routed_candidate.as_deref(),
            Some("openrouter-gpt-4o")
        );
        // Routing provenance never becomes execution evidence.
        assert!(identity.provider_declared_model.is_unavailable());
    }

    #[test]
    fn coverage_counts_only_observed_fields() {
        let partial = ExecutionIdentity::unrouted("m").with_declared_model(Some("m"));
        assert_eq!(partial.coverage(), 0.5);
    }

    #[test]
    fn unavailability_distinguishes_a_provider_gap_from_a_local_fault() {
        assert!(UnavailableReason::NotDeclaredByProvider.is_provider_limitation());
        assert!(UnavailableReason::FieldNotParsed.is_provider_limitation());
        assert!(!UnavailableReason::UpstreamNotContacted.is_provider_limitation());
        assert!(!UnavailableReason::EmptyValue.is_provider_limitation());
    }

    #[test]
    fn identity_round_trips_through_json() {
        let identity = ExecutionIdentity::unrouted("m")
            .with_declared_model(Some("real-model"))
            .with_routed(Some("c"), Some("p"));
        let json = serde_json::to_string(&identity).expect("serialize");
        let back: ExecutionIdentity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(identity, back);
    }
}
