//! The offline release gate: the evidence that decides whether a trained model
//! may be **considered** at all.
//!
//! This is not an activation, a shadow window, or a statistical test. It is the
//! question that has to be answered *before* any of those are worth doing: is
//! this artifact the one that produced the recorded decisions, does replaying
//! those decisions from their retained inputs reproduce them exactly, does the
//! evidence that would justify a look exist at all, and is it intact? A model
//! that cannot answer "yes" to all four is not a model to be argued about.
//!
//! # The one rule everything else serves
//!
//! **Replay equivalence is bit-exact, and a near miss is a refusal.**
//!
//! The replay re-derives the decision through
//! [`ShadowEngine::evaluate_with`](crate::ml::shadow::ShadowEngine::evaluate_with)
//! — the *same* accepted path the original decision took — and compares every
//! component against the recorded decision. Floats compare through
//! [`f64::to_bits`], never through `==`, never through an epsilon, and never
//! after rounding. There is no tolerance anywhere in this module, and there is
//! no "close enough" path to fall into: the first non-identical component ends
//! the comparison and is carried out as
//! [`OfflineGateError::ReplayDivergence`].
//!
//! Reusing the original's code path is what makes the claim worth anything. If
//! the gate carried its own re-implementation of the decision, a divergence
//! could only ever mean "the two implementations differ", which is a fact about
//! the gate. Running the accepted path means a divergence can only mean the
//! retained input or the commit is not the one the original used — a fact about
//! the evidence.
//!
//! # The replay is driven by the retained input, and that is shown, not asserted
//!
//! This module reads candidate features from exactly one place:
//! [`ShadowDecision::input`], the immutable decision-time snapshot. It does not
//! import `extract_features`, an `ObservationStore` or a `StatsStore`, so
//! re-deriving a feature vector is not a choice this module declines to make —
//! it is not reachable from here.
//!
//! Structure alone is still a claim about the code, so the gate also *measures*
//! it: [`RetentionAblation`] perturbs retained feature positions one at a time
//! and replays. A position whose perturbation changes a prediction, a utility,
//! the selection, the action, the reason or the ranking is **load-bearing**;
//! the retained bytes reach the decision arithmetic. A position whose
//! perturbation changes nothing is reported as inert, not hidden. The gate
//! requires at least one load-bearing position before it will say the retention
//! is what made the equivalence possible, because "the replay reads the retained
//! input" is a claim about a function, and a function that ignored its input
//! would satisfy every other gate here.
//!
//! Drift in the retained input is caught separately and earlier: the replay
//! recomputes `decision_input_checksum`, and a retained input whose checksum
//! differs from the recorded one did not produce the recorded decision
//! ([`OfflineGateError::RetainedInputDrifted`]).
//!
//! # Floats: what this workspace's JSON does, and what this gate does about it
//!
//! **This section is a historical record and is kept for the reasoning. Read the
//! next paragraph for what is true now.**
//!
//! `serde_json` WAS used here without its `float_roundtrip` feature, so its
//! float parsing was a multiply/divide-by-a-power-of-ten fast path rather than a
//! correctly-rounded parse. A canonical sample carries 32 `f32` features plus
//! several `f64` targets (`Targets::{latency_ms, ttft_ms, cost}`, the per
//! attempt `latency_ms`/`ttft_ms`, and the cost facts), and the `f64` targets
//! could come back **one ULP different** after a round trip. The parent measured
//! this and recorded it as E-089 against the journal node, then again as E-097
//! against the artifact path.
//!
//! **The workspace now enables `float_roundtrip` workspace-wide.** The transport
//! is exact, the hazard class is empty, and every claim below that depended on it
//! being lossy has flipped. The three answers are all still correct and all still
//! worth keeping, because each is defence in depth rather than a fix for a live
//! bug:
//!
//! 1. **The model artifact.** A checkpoint could not survive plain JSON, because
//!    the accepted verification hashes `f64::to_bits`. 7E-2F solved the transport
//!    with a bit-exact wire form. This gate does **not** reach into that module:
//!    an accepted boundary test requires that nothing else in `ml` may name it,
//!    which is the right constraint, because the installer must stay unreachable
//!    from the ML stack. What the gate does instead is verify the commit with the
//!    accepted check and *measure*, on every run, whether plain JSON would have
//!    carried it ([`CommitTransport`]). That measurement now reports exact, and it
//!    is kept so that a future regression in the transport is caught by a
//!    constituent of the verdict rather than discovered as an unexplainable
//!    verification failure.
//!
//! 2. **The compared decision.** No journal-carried `f64` is ever a compared
//!    component of replay equivalence. The compared floats are model *outputs*,
//!    recomputed from the retained `f32` feature vector through the bit-exact
//!    commit. This exclusion was the reason the gate stayed decidable at all
//!    under E-089, and it is stated here rather than left as an accident of which
//!    fields happen to be compared. It is now belt-and-braces.
//!
//! 3. **The measurements that do consume those targets.** The holdout and
//!    calibration path reads those `f64`s as regression and loss targets.
//!    [`JournalFloatFidelity`] re-derives every `f64` field's bits through the
//!    same `to_vec`/`from_slice` path the journal uses, on every run, and reports
//!    how many fields moved and by how many ULPs. It now reports zero, and that
//!    is a **positive** result: it is the regression test for the transport being
//!    exact, and it fails loudly if `float_roundtrip` is ever removed. Its honest
//!    limit is written on the constant [`JOURNAL_FLOAT_NOTE`]: it measures the
//!    *transport*, so a value that arrived already drifted is invisible to it and
//!    can only be prevented, not detected, by sourcing it bit-exactly.
//!
//! A consequence worth stating plainly: if a caller persists a
//! [`ShadowDecision`] as plain JSON and hands the gate the re-parsed record,
//! the gate **must refuse** on the first differing component. That is the
//! correct outcome. `JournalFloatFidelity` is how the caller finds out why,
//! rather than discovering an unexplained mismatch.
//!
//! # What this gate consumes rather than reimplements
//!
//! * Calibration measurement and its `passes` rule are 7E-2D's. The gate runs
//!   [`run_calibration`] over a holdout proven disjoint from the fit set and
//!   carries [`CalibrationVerdict::from_measurements`] verbatim. A degenerate
//!   holdout refuses through 7E-2D's own
//!   [`DegeneracyReason`]; the gate does not classify degeneracy itself.
//! * Drift, PSI and the priced marginal route are 7E-2D's and are read, never
//!   reinterpreted.
//! * The failure vocabulary and the impact table are `FailureClass::impact` and
//!   `FailureFacts::classified`. The gate reads them. It never re-derives a
//!   failure class from a message or a status code.
//! * The commit, its checkpoint and its checkpoint verification are
//!   `ModelCommit`'s. The gate never re-implements content hashing.
//!
//! # What this gate deliberately does not answer
//!
//! [`RELEASE_SCOPE`] says it in one line, and it is repeated in the release
//! verdict's own type because a verdict is exactly the place a reader is most
//! likely to over-read it:
//!
//! * **Statistical support.** No confidence interval, no power analysis, no
//!   significance test, no minimum detectable effect, no multiple-comparison
//!   control, no bootstrap. [`EvidenceFloors`] is a *presence* floor: it
//!   refuses when there is too little evidence to make the claim at all. It is
//!   not a test of whether the claim is supported. Attempt-level attribution
//!   and the statistical release methodology belong to node 7D, sequenced after
//!   this one (E-092).
//! * **Reachability.** Nothing here says a model can be served, will be
//!   reached, or is safe to activate. A `Considerable` verdict means the
//!   evidence survived being checked. 7F owns real-traffic shadowing.
//! * **Any of the forbidden operations.** This module starts no thread, runs no
//!   timer and installs nothing. The [`ShadowEngine`] it builds is local to the
//!   call, is never registered with a runtime, and is never `swap`ped or
//!   `try_train`ed.

mod error;
mod replay;
mod report;
mod run;

pub use error::*;
pub use report::*;
pub use run::*;

/// What a release verdict from this gate is, and what it is not.
///
/// Carried on the report and repeated in the verdict's own documentation,
/// because a verdict is the one artifact a reader is most likely to over-read.
///
/// **Scope of this string.** It describes the INTEGRITY constituents, and it was
/// written when those were the only ones. It is still true of them and its
/// value is unchanged. It is no longer a complete description of the combined
/// verdict, because node 7D added a statistical constituent that is exactly a
/// claim about beating a baseline. [`crate::ml::statistics::STATISTICAL_SCOPE`]
/// covers that part and the report carries both, so a reader can tell evidence
/// of integrity from evidence of effect. Reading this string alone would now
/// understate what the verdict says.
pub const RELEASE_SCOPE: &str = "whether the recorded evidence for this model is intact, \
replayable and sufficient to be worth looking at; it is NOT a statistical claim, NOT a claim \
that the model is better than the baseline, and NOT a claim that the model can be served";

/// What 7D's statistical constituent of that verdict is, and what it is not.
///
/// Carried beside [`RELEASE_SCOPE`] rather than folded into it. 7E-3's string
/// describes the replay and integrity constituents, which are unchanged; this
/// one describes the statistical constituent added alongside them. A reader
/// holding both can see which part of the verdict is evidence of integrity and
/// which part is evidence of effect, and neither string over-reads the other.
pub use crate::ml::statistics::STATISTICAL_SCOPE;

/// The honest limit of [`JournalFloatFidelity`].
pub const JOURNAL_FLOAT_NOTE: &str = "measured by re-serialising and re-parsing the canonical \
samples this run actually consumed, on every run; this measures the TRANSPORT, so a value that \
arrived already drifted is invisible to it and must be prevented at the source rather than \
detected here";
