//! What the upstream's structured response said it ran, kept apart from what
//! the client asked for and from what Zroutery configured.
//!
//! # Three identities, three fields
//!
//! One request touches three different model names, and collapsing any two of
//! them is how an activity log starts lying:
//!
//! 1. **The client's entry** — the stable id the client named at the ingress
//!    (`standard-class`, `agentrouter-gpt-6.1-sol`). Recorded as
//!    [`crate::stats::RequestRecord::requested_model`]; it stays the stable
//!    virtual/exposed id the client can recognise.
//! 2. **The configured plan** — the upstream model the routing plan actually
//!    sent. Recorded per attempt in
//!    [`crate::accounting::ExecutionSnapshot::upstream_model`].
//! 3. **The upstream's declaration** — the model name (and version, when a
//!    structured field carries one) that the upstream's *structured response*
//!    put in its own fields. This module owns that fact.
//!
//! The third one is routinely destroyed on the wire: the proxy rewrites the
//! response's `model` field to the client-facing exposed id, which is correct
//! for the client and lossy for the record. The decoders therefore copy the
//! structured values into the internal passthrough keys below *before* that
//! rewrite, and the request lifecycle persists them per attempt — on every
//! build, not only when the `ml` feature is compiled. The ML execution
//! identity (`crate::ml::execution_identity`) is fed from the same capture so
//! there is exactly one source of truth.
//!
//! # The declaration is a claim, not a verification
//!
//! A provider writing `gpt-4o` in its response body is *declaring* a name. It
//! is not evidence of which backend weight set actually ran, of the billing
//! tier, of the price, or of the reasoning effort. The value is stored
//! verbatim with provenance limited to "a structured field said so", and
//! nothing downstream may promote it into a verified fact.
//!
//! # What is refused
//!
//! * A name is never inferred from answer text. Only the response's structured
//!   `model` / `modelVersion` fields count; prose is not a declaration.
//! * Absence is preserved. "The provider declared nothing" is a state
//!   ([`DeclaredIdentity::Missing`]) and is never back-filled from the
//!   requested or configured model.
//! * Placeholders stay placeholders. `auto` — and any `<namespace>/auto`
//!   spelling of it — names a routing policy, not a model
//!   ([`DeclaredIdentity::AutoOnly`]).
//! * Conflict is preserved. When one attempt's structured fields disagree,
//!   every distinct value is kept ([`DeclaredIdentity::Conflicting`]) — the
//!   last writer does not win, and neither does the first.
//! * Only names are stored. No request or response body, no header dump, and
//!   no secret can reach this type; values are trimmed and length-bounded.
//!
//! # The internal passthrough key
//!
//! A decoded [`crate::ir::ChatResponse`] has no dedicated field for these
//! values, and its `model` field is the one the pipeline rewrites. The
//! decoders therefore park the whole classified declaration on the response's
//! passthrough map under one `zroutery.internal.*` key, following the same
//! convention as the existing internal decision-verdict key, and the request
//! lifecycle reads it back with [`declaration_from_passthrough`] and merges it
//! per attempt. The key never reaches a client: every egress encoder rebuilds
//! its wire JSON from named fields and copies only the specific passthrough
//! members it defines.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Schema version of [`UpstreamModelDeclaration`].
pub const DECLARED_MODEL_SCHEMA_VERSION: u32 = 1;

/// A declared name is an identifier, not prose; longer values are not kept.
const DECLARED_VALUE_MAX_LEN: usize = 128;

/// A conflicting attempt keeps every distinct value, but only so many: a
/// stream that flip-flops twenty times is still one bounded fact.
const CONFLICT_VALUES_MAX: usize = 8;

/// Internal passthrough key carrying the complete [`UpstreamModelDeclaration`]
/// one response's structured fields declared, before the pipeline rewrites the
/// wire `model` to the client-facing exposed id. Written by the buffered
/// response decoders and the stream collector, read once by the request
/// lifecycle; never encoded to a client.
pub const DECLARATION_PASSTHROUGH_KEY: &str = "zroutery.internal.upstream_declaration";

/// Store a complete declaration on an internal passthrough map.
///
/// The whole classified structure travels — both axes, including a conflict —
/// so a reader never has to collapse it back from a single scalar. Callers
/// build the declaration with [`DeclaredIdentity::observe`] or
/// [`declaration_from_wire`], so it is bounded by construction. A decode is
/// the only writer and one decode sees one body, so an existing key is
/// overwritten.
pub fn capture_declaration_in_passthrough(
    passthrough: &mut Map<String, Value>,
    declaration: &UpstreamModelDeclaration,
) {
    if let Ok(value) = serde_json::to_value(declaration) {
        passthrough.insert(DECLARATION_PASSTHROUGH_KEY.to_string(), value);
    }
}

/// Read the complete declaration back off a decoded response.
///
/// `None` when the response carries no key (nothing was captured, or the
/// response predates the capture) and when the stored value cannot be read
/// as **this build's schema** — a declaration written by a newer format is
/// not interpreted field-by-field with defaults standing in for what this
/// build cannot see, because reading a future schema as "nothing declared"
/// would be exactly the quiet fabrication the capture exists to prevent.
pub fn declaration_from_passthrough(
    resp: &crate::ir::ChatResponse,
) -> Option<UpstreamModelDeclaration> {
    let value = resp.passthrough.get(DECLARATION_PASSTHROUGH_KEY)?;
    let declaration: UpstreamModelDeclaration = serde_json::from_value(value.clone()).ok()?;
    (declaration.schema_version == DECLARED_MODEL_SCHEMA_VERSION).then_some(declaration)
}

/// Build the declaration one wire object's structured fields carry.
///
/// Reads the response body's `model` member and the version axis in either
/// wire spelling (`modelVersion`, `model_version`), so a body that carries
/// both forms keeps a disagreement between them visible as a conflict instead
/// of silently preferring one. Nothing else is read: prose is not a
/// declaration.
pub fn declaration_from_wire(body: &Value) -> UpstreamModelDeclaration {
    let mut declaration = UpstreamModelDeclaration::default();
    declaration.observe_model(body.get("model").and_then(Value::as_str));
    declaration.observe_model_version(body.get("modelVersion").and_then(Value::as_str));
    declaration.observe_model_version(body.get("model_version").and_then(Value::as_str));
    declaration
}

/// One identity field as one attempt's structured response declared it.
///
/// Absence is a value here, not a default: a provider that names no model is a
/// finding worth recording, and it is distinct from "this build could not look"
/// (which is expressed one level up, as no declaration at all).
///
/// The field accumulates over one attempt. A stream can declare the model in
/// its opening frame and again in a terminal frame; a disagreement between the
/// two is a conflict, and neither frame is allowed to silently overwrite the
/// other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum DeclaredIdentity {
    /// A specific name the upstream put in a structured field, verbatim.
    Declared(String),
    /// The upstream declared only a non-specific routing placeholder
    /// (`auto`, and any `<namespace>/auto` spelling of it). The value is kept
    /// verbatim; it is not a model name and never stands in for one.
    AutoOnly(String),
    /// One attempt's structured fields disagreed. Every distinct value is
    /// kept in first-seen order, bounded by [`CONFLICT_VALUES_MAX`]; the last
    /// writer does not win.
    Conflicting(Vec<String>),
    /// No structured field carried a usable value: absent, empty, longer
    /// than [`DECLARED_VALUE_MAX_LEN`], or carrying control characters.
    Missing,
}

impl Default for DeclaredIdentity {
    /// The honest baseline of an identity field nothing has observed yet.
    fn default() -> Self {
        Self::Missing
    }
}

impl DeclaredIdentity {
    /// Classify one point observation read off a structured field.
    ///
    /// The value is trimmed but otherwise verbatim — a declared name is never
    /// normalised, because normalisation is how two providers' spellings get
    /// collapsed into one fact that neither stated. A value that carries
    /// control characters or exceeds [`DECLARED_VALUE_MAX_LEN`] is not an
    /// identifier at all: keeping either would let a response field smuggle
    /// arbitrary text into the durable record, so both are recorded as an
    /// absence.
    pub fn observe(raw: Option<&str>) -> Self {
        let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::Missing;
        };
        if value.len() > DECLARED_VALUE_MAX_LEN
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Self::Missing;
        }
        if Self::is_placeholder(value) {
            return Self::AutoOnly(value.to_string());
        }
        Self::Declared(value.to_string())
    }

    /// Whether a declared value names a routing policy instead of a model.
    ///
    /// The rule is deliberately mechanical: `auto` itself, and any
    /// `<namespace>/auto` a client or relay writes (`vendor/auto`,
    /// `openrouter/auto`, ...), name *automatic selection* — the prefix names
    /// who does the choosing, never the model that runs. A match is exact on
    /// the whole value after lowercasing; anything else is kept as a specific
    /// declaration, because an unknown string is a model name somebody chose,
    /// not a placeholder Zroutery invented.
    fn is_placeholder(value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        lower == "auto" || lower.ends_with("/auto")
    }

    /// Fold a later observation into this one.
    ///
    /// The rules, in order:
    ///
    /// * A missing observation refines nothing and contradicts nothing: an
    ///   attempt that declared a name and then stopped declaring one did not
    ///   retract it.
    /// * The same value twice is one observation.
    /// * Any two *different* values conflict, whatever their kinds — `auto`
    ///   followed by `gpt-4o` is exactly as uncertain as `gpt-4o` followed by
    ///   `auto`. Neither frame of a stream is allowed to win.
    pub fn merge(&mut self, next: Self) {
        match next {
            Self::Missing => {}
            Self::Conflicting(values) => {
                for (is_auto, value) in values
                    .into_iter()
                    .map(|value| (Self::is_placeholder(&value), value))
                {
                    self.merge_single(is_auto, value);
                }
            }
            Self::Declared(value) => self.merge_single(false, value),
            Self::AutoOnly(value) => self.merge_single(true, value),
        }
    }

    /// Fold one classified observation in, when the incoming value is known
    /// not to be missing.
    fn merge_single(&mut self, incoming_is_auto: bool, incoming: String) {
        let replacement = match self {
            Self::Missing => Some(if incoming_is_auto {
                Self::AutoOnly(incoming)
            } else {
                Self::Declared(incoming)
            }),
            // The same value twice is one observation whatever kind carried
            // it; a placeholder cannot equal a specific name in practice
            // because placeholders are classified before they are stored, and
            // treating equality as identity is the conservative reading.
            Self::Declared(current) | Self::AutoOnly(current) => (incoming != *current)
                .then(|| Self::Conflicting(vec![std::mem::take(current), incoming])),
            Self::Conflicting(values) => {
                Self::merge_value_into(values, incoming);
                None
            }
        };
        if let Some(replacement) = replacement {
            *self = replacement;
        }
    }

    /// Append one value to a conflict list, deduplicated and bounded.
    fn merge_value_into(values: &mut Vec<String>, value: String) {
        if !values.contains(&value) && values.len() < CONFLICT_VALUES_MAX {
            values.push(value);
        }
    }

    /// The specific model name, when the field declared exactly one.
    ///
    /// `None` for every uncertain or absent state: a placeholder, a conflict
    /// and a missing field are all "no specific name", and a caller that needs
    /// the distinction reads the state itself.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Declared(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// The first name this field ever held, whatever its kind.
    ///
    /// Display-facing: it answers "what was named first" without erasing the
    /// uncertainty the state carries.
    pub fn primary(&self) -> Option<&str> {
        match self {
            Self::Declared(value) | Self::AutoOnly(value) => Some(value.as_str()),
            Self::Conflicting(values) => values.first().map(String::as_str),
            Self::Missing => None,
        }
    }

    /// Whether the field declared one specific, non-placeholder model name.
    pub fn is_specific(&self) -> bool {
        matches!(self, Self::Declared(_))
    }

    /// Whether the field's evidence is explicitly uncertain.
    ///
    /// Uncertain is not unknown: an [`Self::AutoOnly`] or
    /// [`Self::Conflicting`] field carries real observations that do not add
    /// up to one name, and a reader may want to surface exactly that.
    pub fn is_uncertain(&self) -> bool {
        matches!(self, Self::AutoOnly(_) | Self::Conflicting(_))
    }
}

/// What one attempt's structured response declared about the model that ran:
/// the model name and — separately — the version, when the protocol carries
/// one.
///
/// The two axes never merge and never overwrite each other. `model` is the
/// routing-level name; `model_version` is the provider's own version spelling
/// (Gemini's `modelVersion`, a relay's `model_version`). A version is not a
/// more-precise model and a model is not a version: conflating them is how a
/// `model: gemini-2.5-flash` / `modelVersion: gemini-2.5-flash-002` pair
/// becomes a phantom third model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamModelDeclaration {
    #[serde(default)]
    pub schema_version: u32,
    /// The model name axis, as the structured response declared it.
    #[serde(default)]
    pub model: DeclaredIdentity,
    /// The version axis. Populated only when a structured version field was
    /// actually present; `Missing` on every protocol that has no such field.
    #[serde(default)]
    pub model_version: DeclaredIdentity,
}

impl Default for UpstreamModelDeclaration {
    fn default() -> Self {
        Self {
            schema_version: DECLARED_MODEL_SCHEMA_VERSION,
            model: DeclaredIdentity::Missing,
            model_version: DeclaredIdentity::Missing,
        }
    }
}

impl UpstreamModelDeclaration {
    /// A declaration built from one wire object's structured fields.
    ///
    /// This is the point-observation form the staged Codex path and any other
    /// caller that holds a raw wire object uses; the streaming lifecycle form
    /// is [`Self::observe_model`]/[`Self::observe_model_version`], which
    /// accumulate.
    pub fn from_wire_fields(model: Option<&str>, version: Option<&str>) -> Self {
        let mut declaration = Self::default();
        declaration.observe_model(model);
        declaration.observe_model_version(version);
        declaration
    }

    /// Fold one model-field observation into the attempt's declaration.
    pub fn observe_model(&mut self, raw: Option<&str>) {
        self.model.merge(DeclaredIdentity::observe(raw));
    }

    /// Fold one version-field observation into the attempt's declaration.
    pub fn observe_model_version(&mut self, raw: Option<&str>) {
        self.model_version.merge(DeclaredIdentity::observe(raw));
    }

    /// Fold a whole later declaration in, axis by axis.
    pub fn merge(&mut self, next: &Self) {
        self.model.merge(next.model.clone());
        self.model_version.merge(next.model_version.clone());
    }

    /// The specific model name, if this attempt declared exactly one.
    pub fn observed_model(&self) -> Option<&str> {
        self.model.value()
    }

    /// The first model name this attempt held, whatever its state.
    pub fn primary_model(&self) -> Option<&str> {
        self.model.primary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_name_is_kept_verbatim() {
        let observed = DeclaredIdentity::observe(Some("  gpt-4o-2024-11-20 "));
        assert_eq!(
            observed,
            DeclaredIdentity::Declared("gpt-4o-2024-11-20".into())
        );
        assert!(observed.is_specific());
        assert!(!observed.is_uncertain());
        assert_eq!(observed.value(), Some("gpt-4o-2024-11-20"));
    }

    #[test]
    fn absent_empty_and_oversized_observations_are_missing() {
        assert_eq!(DeclaredIdentity::observe(None), DeclaredIdentity::Missing);
        assert_eq!(
            DeclaredIdentity::observe(Some("")),
            DeclaredIdentity::Missing
        );
        assert_eq!(
            DeclaredIdentity::observe(Some("   ")),
            DeclaredIdentity::Missing
        );
        let oversized = "x".repeat(DECLARED_VALUE_MAX_LEN + 1);
        assert_eq!(
            DeclaredIdentity::observe(Some(&oversized)),
            DeclaredIdentity::Missing
        );
    }

    #[test]
    fn auto_placeholders_stay_placeholders() {
        for raw in [
            "auto",
            "AUTO",
            "vendor/auto",
            "Vendor/Auto",
            "openrouter/auto",
        ] {
            let observed = DeclaredIdentity::observe(Some(raw));
            assert!(
                matches!(observed, DeclaredIdentity::AutoOnly(_)),
                "{raw} must stay a placeholder"
            );
            assert!(observed.is_uncertain());
            assert_eq!(observed.value(), None, "a placeholder is not a model name");
        }
        // A specific model is not demoted by a prefix match.
        assert!(matches!(
            DeclaredIdentity::observe(Some("automated-model")),
            DeclaredIdentity::Declared(_)
        ));
        assert!(matches!(
            DeclaredIdentity::observe(Some("auto-complete")),
            DeclaredIdentity::Declared(_)
        ));
    }

    #[test]
    fn control_characters_and_oversize_are_refused_at_classification() {
        assert_eq!(
            DeclaredIdentity::observe(Some("gpt-4o\nfree-form text")),
            DeclaredIdentity::Missing,
            "a newline cannot smuggle prose into the durable record"
        );
        assert_eq!(
            DeclaredIdentity::observe(Some("gpt\t4o")),
            DeclaredIdentity::Missing
        );
        let oversized = "x".repeat(DECLARED_VALUE_MAX_LEN + 1);
        assert_eq!(
            DeclaredIdentity::observe(Some(&oversized)),
            DeclaredIdentity::Missing
        );
        // Exactly at the bound is still an identifier.
        let at_bound = "x".repeat(DECLARED_VALUE_MAX_LEN);
        assert!(matches!(
            DeclaredIdentity::observe(Some(&at_bound)),
            DeclaredIdentity::Declared(_)
        ));
    }

    #[test]
    fn absence_never_retracts_an_observation() {
        let mut identity = DeclaredIdentity::observe(Some("gpt-4o"));
        identity.merge(DeclaredIdentity::Missing);
        assert_eq!(identity, DeclaredIdentity::Declared("gpt-4o".into()));
    }

    #[test]
    fn the_first_observation_refines_a_missing_field() {
        let mut identity = DeclaredIdentity::Missing;
        identity.merge(DeclaredIdentity::Declared("glm-5.3".into()));
        assert_eq!(identity, DeclaredIdentity::Declared("glm-5.3".into()));
    }

    #[test]
    fn the_same_value_twice_is_one_observation() {
        let mut identity = DeclaredIdentity::Declared("gpt-4o".into());
        identity.merge(DeclaredIdentity::Declared("gpt-4o".into()));
        assert_eq!(identity, DeclaredIdentity::Declared("gpt-4o".into()));
    }

    #[test]
    fn a_disagreement_is_a_conflict_in_first_seen_order() {
        let mut identity = DeclaredIdentity::Declared("claude-x".into());
        identity.merge(DeclaredIdentity::Declared("claude-y".into()));
        assert_eq!(
            identity,
            DeclaredIdentity::Conflicting(vec!["claude-x".into(), "claude-y".into()])
        );
        assert!(identity.is_uncertain());
        assert_eq!(identity.value(), None);
        assert_eq!(identity.primary(), Some("claude-x"));
    }

    #[test]
    fn a_placeholder_disagreeing_with_a_name_conflicts() {
        // Neither order wins: `auto` then `gpt-4o` and `gpt-4o` then `auto`
        // end in the same uncertainty.
        let mut auto_first = DeclaredIdentity::AutoOnly("auto".into());
        auto_first.merge(DeclaredIdentity::Declared("gpt-4o".into()));
        assert_eq!(
            auto_first,
            DeclaredIdentity::Conflicting(vec!["auto".into(), "gpt-4o".into()])
        );
        let mut name_first = DeclaredIdentity::Declared("gpt-4o".into());
        name_first.merge(DeclaredIdentity::AutoOnly("auto".into()));
        assert_eq!(
            name_first,
            DeclaredIdentity::Conflicting(vec!["gpt-4o".into(), "auto".into()])
        );
    }

    #[test]
    fn a_conflict_accumulates_and_stays_bounded() {
        let mut identity = DeclaredIdentity::Declared("a".into());
        identity.merge(DeclaredIdentity::Declared("b".into()));
        identity.merge(DeclaredIdentity::Declared("c".into()));
        identity.merge(DeclaredIdentity::Declared("a".into()));
        assert_eq!(
            identity,
            DeclaredIdentity::Conflicting(vec!["a".into(), "b".into(), "c".into()])
        );
        for n in 0..(CONFLICT_VALUES_MAX + 4) {
            identity.merge(DeclaredIdentity::Declared(format!("m{n}")));
        }
        let DeclaredIdentity::Conflicting(values) = &identity else {
            panic!("expected a bounded conflict");
        };
        assert_eq!(values.len(), CONFLICT_VALUES_MAX);
    }

    #[test]
    fn a_conflicting_observation_merges_into_a_conflict() {
        let mut identity = DeclaredIdentity::Declared("a".into());
        let mut next = DeclaredIdentity::Declared("b".into());
        next.merge(DeclaredIdentity::Declared("c".into()));
        identity.merge(next);
        assert_eq!(
            identity,
            DeclaredIdentity::Conflicting(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn the_two_axes_stay_separate() {
        let mut declaration = UpstreamModelDeclaration::default();
        declaration.observe_model(Some("gemini-2.5-flash"));
        declaration.observe_model_version(Some("gemini-2.5-flash-002"));
        assert_eq!(
            declaration.model,
            DeclaredIdentity::Declared("gemini-2.5-flash".into())
        );
        assert_eq!(
            declaration.model_version,
            DeclaredIdentity::Declared("gemini-2.5-flash-002".into())
        );
        // A later frame that names only the model does not touch the version.
        declaration.observe_model(Some("gemini-2.5-flash"));
        assert_eq!(
            declaration.model_version,
            DeclaredIdentity::Declared("gemini-2.5-flash-002".into())
        );
    }

    #[test]
    fn a_declaration_round_trips_through_json() {
        let mut declaration = UpstreamModelDeclaration::default();
        declaration.observe_model(Some("auto"));
        declaration.observe_model_version(Some("2024-11-20"));
        let json = serde_json::to_string(&declaration).expect("serialize");
        let back: UpstreamModelDeclaration = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(declaration, back);
        assert!(json.contains("\"state\":\"auto_only\""));
    }

    #[test]
    fn legacy_json_without_the_field_loads_as_missing() {
        let back: UpstreamModelDeclaration =
            serde_json::from_str("{\"schema_version\":1,\"model\":{\"state\":\"missing\"}}")
                .expect("deserialize");
        assert_eq!(back.model_version, DeclaredIdentity::Missing);
    }

    #[test]
    fn passthrough_capture_round_trips_the_whole_declaration() {
        let mut passthrough = Map::new();
        let mut declaration = UpstreamModelDeclaration::default();
        declaration.observe_model(Some(" relay-model "));
        declaration.observe_model_version(Some("2024-11-20"));
        capture_declaration_in_passthrough(&mut passthrough, &declaration);
        assert!(passthrough.contains_key(DECLARATION_PASSTHROUGH_KEY));
        // Read back through the ChatResponse-shaped reader.
        let response = crate::ir::ChatResponse {
            id: "r".into(),
            model: String::new(),
            content: Vec::new(),
            stop_reason: crate::ir::StopReason::Unknown,
            stop_sequence: None,
            usage: crate::ir::Usage::default(),
            passthrough,
        };
        let read = declaration_from_passthrough(&response).expect("the declaration rides");
        assert_eq!(read, declaration);
        // A future-format declaration is not interpreted, not guessed.
        let mut passthrough = Map::new();
        passthrough.insert(
            DECLARATION_PASSTHROUGH_KEY.into(),
            serde_json::json!({"schema_version": 99, "future": true}),
        );
        let response = crate::ir::ChatResponse {
            passthrough,
            ..response
        };
        assert!(declaration_from_passthrough(&response).is_none());
    }

    #[test]
    fn a_wire_body_with_both_version_spellings_keeps_the_conflict() {
        let declaration = declaration_from_wire(&serde_json::json!({
            "model": "gemini-2.5-flash",
            "modelVersion": "gemini-2.5-flash-002",
            "model_version": "gemini-2.5-flash-001",
        }));
        assert_eq!(declaration.model.value(), Some("gemini-2.5-flash"));
        assert_eq!(
            declaration.model_version,
            DeclaredIdentity::Conflicting(vec![
                "gemini-2.5-flash-002".into(),
                "gemini-2.5-flash-001".into()
            ])
        );
    }
}
