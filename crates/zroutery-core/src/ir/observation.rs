//! Per-send metadata shared by transport and settlement, never response bodies.
use super::{UpstreamModelDeclaration, Usage};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct AttemptObservation(Arc<Mutex<ObservedResponse>>);

#[derive(Debug, Clone, Default)]
pub struct ObservedResponse {
    pub declaration: UpstreamModelDeclaration,
    pub usage: Option<Usage>,
    /// How the retained usage is evidenced. `usage` staying `None` is
    /// unaffected by this field: an absent or refused reading is never
    /// dressed up as a measured total.
    pub usage_evidence: UsageEvidence,
    pub usage_iterations: Option<Vec<super::UsageIteration>>,
    pub(crate) iterated: bool,
}

impl ObservedResponse {
    /// The usage this send's evidence may support for accounting, or `None`
    /// when the attempt's usage must stay unknown.
    ///
    /// * `Measured` and `Partial` carry the retained reading: a `Partial`
    ///   reading is real evidence — a failed attempt's known charge — and
    ///   its not-terminal-confirmed nature travels beside it in
    ///   `usage_evidence` for the caller to persist.
    /// * `Invalid` yields `None`: the wire refused, and no decoder value may
    ///   stand in for it.
    /// * `Conflicting` yields `None`: the retained reading is not a
    ///   confirmed complete account; the disagreement is the caller's to
    ///   resolve, never a billing fact.
    /// * `Absent` yields `None`: nothing was measured — unknown, not a
    ///   measured zero.
    pub fn accounting_usage(&self) -> Option<Usage> {
        match self.usage_evidence {
            UsageEvidence::Measured | UsageEvidence::Partial => self.usage,
            UsageEvidence::Absent | UsageEvidence::Invalid | UsageEvidence::Conflicting => None,
        }
    }
}

/// How one send's wire usage is evidenced, kept beside `usage` so a retained
/// reading is never mistaken for the send's confirmed complete account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageEvidence {
    /// The wire stated no usage, or only an all-zero total — which this IR
    /// cannot separate from "nothing reported". The attempt's usage is
    /// unknown, not a measured zero.
    #[default]
    Absent,
    /// A complete reading the wire stated before a terminal frame confirmed
    /// it: raw frames, opening events, or a response whose decoding never
    /// ran. Real evidence — e.g. a failed attempt's known charge — but not a
    /// confirmed final account.
    Partial,
    /// A terminal frame or a decoded response confirmed the account.
    Measured,
    /// The wire stated a usage this capture could not trust: a required
    /// total missing or malformed for the protocol's documented shape, or a
    /// counter that violates its own subset. Nothing is recorded, the
    /// attempt's usage stays unknown, and a lenient decoder's backfilled
    /// defaults must not become accounting. A later well-formed reading
    /// recovers from it.
    Invalid,
    /// Two well-formed readings disagreed: a later total would shrink a
    /// retained one. The retained reading stands as evidence but is not
    /// confirmed as the complete account; the flag is sticky, and the
    /// conflict is the caller's to resolve, not a fact to bill silently.
    Conflicting,
}

impl AttemptObservation {
    pub fn snapshot(&self) -> ObservedResponse {
        crate::sync::lock(&self.0).clone()
    }

    /// Record one raw, unconfirmed observation of the response's declared
    /// identity and usage: a frame as it arrived, an opening event, or a
    /// body captured before its own decoding ran.
    ///
    /// The declaration is folded in unconditionally: identity evidence does
    /// not depend on usage quality, so a response whose usage is missing,
    /// malformed or refused still records which model answered. Usage
    /// follows the rules documented on `record_usage`.
    pub fn observe(&self, declaration: &UpstreamModelDeclaration, usage: Option<Usage>) {
        let mut state = crate::sync::lock(&self.0);
        state.declaration.merge(declaration);
        if let Some(usage) = usage {
            record_usage(&mut state, usage, false);
        }
    }

    /// Record the parsed terminal event's cumulative usage. A terminal frame
    /// is the stream's last word, so its reading is confirmed evidence — but
    /// only on top of wire the raw capture did not refuse: a parser builds
    /// its reading from normalized counters, so while the evidence is
    /// `Invalid` the terminal restatement is refused too. The raw capture of
    /// the terminal frame itself runs first and recovers, so a healthy
    /// stream still confirms.
    pub fn observe_terminal_usage(&self, declaration: &UpstreamModelDeclaration, usage: Usage) {
        let mut state = crate::sync::lock(&self.0);
        state.declaration.merge(declaration);
        if matches!(state.usage_evidence, UsageEvidence::Invalid) {
            return;
        }
        if state.iterated {
            if state.usage_iterations.is_some()
                && !matches!(state.usage_evidence, UsageEvidence::Conflicting)
            {
                state.usage_evidence = UsageEvidence::Measured;
            }
            return;
        }
        record_usage(&mut state, usage, true);
    }

    /// Adopt a parsed stream event's non-terminal usage.
    ///
    /// The opening frame's usage is a partial-in-time reading: real evidence,
    /// superseded by the terminal restatement. The parser builds it from
    /// lenient counters, so like every decoder product it is refused while
    /// the raw capture holds a refused (`Invalid`) reading — a well-formed
    /// raw frame recovers through the raw capture, never through the parser.
    /// It is never confirmed here: the reading stays partial until the
    /// terminal restatement confirms it.
    pub fn observe_parsed_usage(&self, declaration: &UpstreamModelDeclaration, usage: Usage) {
        let mut state = crate::sync::lock(&self.0);
        state.declaration.merge(declaration);
        if state.iterated || matches!(state.usage_evidence, UsageEvidence::Invalid) {
            return;
        }
        record_usage(&mut state, usage, false);
    }

    /// Adopt a decoded response's usage for this send.
    ///
    /// This is the one adoption path a *lenient* decoder feeds: it is refused
    /// outright when the raw capture already examined the same wire usage and
    /// found it untrustworthy (`Invalid`), so filled zeros and clamped
    /// subsets can never become accounting facts. A decoding success is
    /// itself terminal evidence, so a surviving reading is confirmed
    /// `Measured`; a same-source re-extraction never raises `Conflicting` —
    /// a smaller decoded value keeps the retained, at-least-as-informative
    /// raw reading instead of flagging the wire. A seen conflict stays: the
    /// decoded re-extraction refines the retained values only when it
    /// dominates, and the flag remains the send's headline fact.
    pub fn observe_decoded(&self, declaration: &UpstreamModelDeclaration, usage: Usage) {
        let mut state = crate::sync::lock(&self.0);
        state.declaration.merge(declaration);
        if matches!(state.usage_evidence, UsageEvidence::Invalid) {
            return;
        }
        if state.iterated {
            if state.usage_iterations.is_some()
                && !matches!(state.usage_evidence, UsageEvidence::Conflicting)
            {
                state.usage_evidence = UsageEvidence::Measured;
            }
            return;
        }
        if usage == Usage::default() {
            return;
        }
        if let Some(previous) = state.usage {
            if dominates(&usage, &previous) {
                state.usage = Some(usage);
            }
        } else {
            state.usage = Some(usage);
        }
        if !matches!(state.usage_evidence, UsageEvidence::Conflicting) {
            state.usage_evidence = UsageEvidence::Measured;
        }
    }

    /// Record that the wire stated a usage this send's capture could not
    /// trust. Nothing is recorded and `usage` stays `None`; a later
    /// well-formed reading recovers, and a decoded backfill is refused until
    /// then. A confirmed reading or a seen conflict is not reopened by it.
    pub fn observe_invalid_usage(&self) {
        let mut state = crate::sync::lock(&self.0);
        if matches!(
            state.usage_evidence,
            UsageEvidence::Measured | UsageEvidence::Conflicting
        ) {
            return;
        }
        state.usage = None;
        state.usage_evidence = UsageEvidence::Invalid;
    }

    pub(crate) fn observe_iterations(&self, iterations: Result<Vec<super::UsageIteration>, ()>) {
        let mut state = crate::sync::lock(&self.0);
        let was_iterated = state.iterated;
        state.iterated = true;
        match iterations.ok().and_then(|iterations| {
            super::iteration_total(&iterations).map(|usage| (iterations, usage))
        }) {
            Some((iterations, usage)) => {
                if was_iterated
                    && (state
                        .usage
                        .is_some_and(|previous| !dominates(&usage, &previous))
                        || state.usage_iterations.as_ref().is_some_and(|previous| {
                            iterations.len() < previous.len()
                                || iterations.iter().zip(previous).any(|(next, previous)| {
                                    next.kind != previous.kind
                                        || next.model != previous.model
                                        || !dominates(&next.usage, &previous.usage)
                                })
                        }))
                {
                    state.usage_evidence = UsageEvidence::Conflicting;
                    return;
                }
                state.usage_iterations = Some(iterations);
                state.usage = Some(usage);
                if !matches!(state.usage_evidence, UsageEvidence::Conflicting) {
                    state.usage_evidence = UsageEvidence::Partial;
                }
            }
            None => {
                if matches!(state.usage_evidence, UsageEvidence::Conflicting) {
                    return;
                }
                state.usage_iterations = None;
                state.usage = None;
                state.usage_evidence = UsageEvidence::Invalid;
            }
        }
    }
}

/// Fold one usage reading into the observed state.
///
/// One send is observed several times before it settles: raw frames are
/// captured as they arrive, the parsed events restate what the parser built
/// from them, and a buffered response is observed once more after decoding.
/// A later reading may therefore replace the retained one only on evidence,
/// and the two readings must never be mixed field by field — a per-field
/// maximum would dress a counter reset up as growth.
///
/// * An all-zero usage is not recorded. The parsers emit default usage in
///   frames that carry no counters yet, so this limited IR cannot separate a
///   genuinely measured zero from "nothing was reported yet"; the value stays
///   unset and the attempt reports unknown usage rather than a fabricated
///   measured zero. (Outer dependency: expressing a measured-zero total needs
///   an IR shape this slice does not grow.)
/// * The first complete reading is kept as the send's evidence — including on
///   a failed attempt, where it is the only record of a known charge.
/// * A later reading replaces the retained one when it dominates it on both
///   billed totals. Restating frames are cumulative: Anthropic's
///   `message_delta` and Gemini's per-chunk `usageMetadata` restate running
///   totals, so growth is the documented direction of a restatement. The
///   subset counters ride along with the reading that stated them, because
///   optional detail fields may legitimately be absent from a later frame.
/// * A later reading that would shrink a retained total conflicts with it.
///   The retained reading stands and the challenger is refused — a reset, a
///   truncated relay or a less complete re-extraction must not silently
///   replace known evidence with a smaller, unprovable claim — and the
///   `Conflicting` evidence flag makes the disagreement visible instead of
///   billing the retained reading as the confirmed account.
fn record_usage(state: &mut ObservedResponse, usage: Usage, confirmed: bool) {
    if usage == Usage::default() {
        return;
    }
    if matches!(state.usage_evidence, UsageEvidence::Conflicting) {
        // The conflict stays the send's headline fact; dominating
        // restatements may still refine the retained reading.
        if let Some(previous) = state.usage {
            if dominates(&usage, &previous) {
                state.usage = Some(usage);
            }
        } else {
            state.usage = Some(usage);
        }
        return;
    }
    match state.usage {
        None => {
            state.usage = Some(usage);
            state.usage_evidence = if confirmed {
                UsageEvidence::Measured
            } else {
                UsageEvidence::Partial
            };
        }
        Some(previous) if dominates(&usage, &previous) => {
            state.usage = Some(usage);
            if confirmed {
                state.usage_evidence = UsageEvidence::Measured;
            }
        }
        Some(_) => {
            state.usage_evidence = UsageEvidence::Conflicting;
        }
    }
}

/// Whether `next` restates `held` as a cumulative total may.
fn dominates(next: &Usage, held: &Usage) -> bool {
    next.input_tokens >= held.input_tokens && next.output_tokens >= held.output_tokens
}
