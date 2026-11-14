//! Node 7E-2D: offline calibration of a K-way candidate distribution.
//!
//! # What this module is
//!
//! It produces a [`DecisionDistribution`] — the validated K-way type accepted
//! from 7E-2A — as an **offline diagnostic and planning artifact**, measures
//! whether that distribution is calibrated, and refuses to report a calibration
//! claim it cannot support. It is a pure library entry point. It installs
//! nothing, wires nothing, schedules nothing, and exposes no endpoint. The
//! distribution it emits is *unconsumed*: no live predictor, no router, and no
//! policy in the running product can name it, because nothing outside this
//! module names the live surfaces at all.
//!
//! # The event, and why the model is joint
//!
//! The quantity being predicted is **"candidate _i_ serves this request"** — a
//! mutually exclusive K-way outcome, not "candidate _i_ succeeds". Those are
//! different events, and conflating them is the whole difficulty.
//!
//! The per-dimension models produce `Pr(candidate i would succeed)`. Nothing
//! forces K such numbers to sum to one, and nothing forces their ranking to
//! agree with the empirical one. Dividing them by their sum asserts they are
//! already consistent marginals of the serve event; that assertion is usually
//! false, and the assertion is made silently.
//!
//! So the calibrator here is a **joint** model on the simplex:
//!
//! ```text
//! u_i = (ln p_i + b_i) / T
//! q_i = softmax_i(u)
//! ```
//!
//! One global temperature `T` and a per-candidate intercept `b_i` constrained
//! to sum to zero for identifiability, L2-pulled toward zero and clamped so a
//! candidate the fit partition never sees serving stays near zero. The
//! parameterization *is* a simplex point: sum-to-one is inherent, and there is
//! no normalization step left over to damage anything.
//!
//! The competing route is built anyway, and measured. `MarginalCalibrator` fits
//! one shared 1-D Platt map on the log-odds — the most defensible form of
//! "calibrate each candidate independently", since a per-candidate map would
//! only be flattered by per-candidate sparsity — and
//! [`NormalizationDamage::measure`] reports the literal difference
//! `ece(after) - ece(before)` between normalizing that vector and not. The cost
//! of the naive route is a number in the report rather than an assertion in a
//! doc comment.
//!
//! # The final vector is the only thing that is claimed
//!
//! [`measure_emitted`] accepts [`EmittedDecision`], and `EmittedDecision`'s
//! `distribution` field is **private**: the only way to obtain one is
//! [`DecisionDistribution::try_new`]. There is no constructor that takes a bare
//! `f64` vector, and `EmittedDecision` deliberately derives neither
//! `Deserialize` nor `Default`. So the headline measurement *cannot* be
//! computed from pre-normalization numbers — the API does not exist.
//!
//! The two marginal measurements live in a different type,
//! [`MarginalCalibration`], which `measure_emitted` does not accept, and they
//! are reported as strictly subordinate diagnostics.
//!
//! [`CalibrationVerdict::from_measurements`] recomputes the verdict from the
//! stored final numbers and the stored ceilings;
//! [`CalibrationReport::recomputed_verdict`] recomputes it again from the
//! report, and [`CalibrationReport::is_calibrated`] asks *that*. There is no
//! stored boolean anywhere in the claim path.
//!
//! # Determinism, and the ordering trap
//!
//! Two ordering facts decide whether this module is reproducible at all, and
//! both were nearly got wrong.
//!
//! **The identifiers are not content.** `deterministic_sample_id` is
//! `format!("samp-{}-{suffix}", outcome.outcome_id)`
//! (`ml/dataset.rs`), and `outcome_id` is `req_{uuid}`
//! (`stats.rs`), while `decision_id` is `dec_{uuid}` (`router.rs`). Both are
//! fresh per request, and `Decision.timestamp` is second-resolution, so a
//! timestamp comparison ties constantly and the UUID becomes the tiebreaker.
//! Ordering by either one makes the report a function of randomness. This
//! module therefore orders cohorts by [`CohortOrderKey`], which is built only
//! from what the cohort *contains*. `sample_id` is used to **reject** a
//! repeated row and never as a sort key.
//!
//! **Why a content key is enough.** `CohortOrderKey` carries every input this
//! module's computation consumes: the ordered candidate identities, each
//! candidate's rank, each candidate's raw success probability as exact `f64`
//! bits, and the observed served identity. Two cohorts that tie on the key are
//! therefore *indistinguishable to the computation* — same axis, same
//! probabilities, same label — so exchanging them cannot change the fit gradient
//! sum, the multiclass log-loss sum, any reliability bin, any drift bin, the
//! base rate, or the arity. Ties are not a tiebreaker waiting for a UUID; they
//! are an equivalence class over which the report is invariant. The
//! request-level facts in the key (`timestamp`, `dialect`, `streaming`, the
//! terminal status and failure class) only make ties rarer.
//!
//! Two runs of the same snapshot and configuration produce byte-identical
//! calibrator and report. The `UUID-invariance` test proves the stronger
//! property: two snapshots that are the *same logical data* carrying
//! *deliberately different* UUIDs produce the same partition, the same
//! calibrator, the same emitted distributions, the same measurements, and the
//! same verdict. An in-process byte comparison cannot catch a UUID leak — the
//! UUIDs are fixed within a process — so that test is the one that bites.
//!
//! # Cohort scope
//!
//! One cohort is one **decision**: the set of candidates that decision
//! considered. A request whose attempt samples disagree about carrying a
//! `decision_id` is **refused** ([`CalibrationError::MixedDecisionScope`]),
//! not coalesced. Coalescing would merge two real planning events into one
//! invented candidate set, and would count a candidate twice if the same
//! identity appears in both. Refusing keeps the K axis equal to one decision's
//! candidate set and treats "the dataset lost a fact" as the fail-closed
//! condition it is. A `decision_id` spanning two requests is refused for the
//! same reason ([`CalibrationError::DecisionScopeSpansRequests`]).
//!
//! # Unranked candidates and exact zero mass
//!
//! A candidate can enter the axis with no prediction at all
//! ([`CandidateInput::Unranked`]) — ineligible, or without evidence. It
//! receives **exactly `0.0`**: the softmax runs over the ranked candidates only
//! and the zero is placed afterwards, so the ranked candidates keep the vector
//! they would have had and the type's total mass is untouched. `0.0` is a
//! legal probability for the 7E-2A type, so this is representable without
//! distorting the rest.
//!
//! # A known limitation, stated rather than hidden
//!
//! The K axis is the set of candidates **actually attempted**, reconstructed
//! retrospectively from recorded attempts. It is not a counterfactual set of
//! candidates that *could* have served, and a candidate that was never tried
//! carries no observation at all. Every number here is therefore a statement
//! about the recorded attempt cohort, and the report says so in
//! [`DISTRIBUTION_ROLE`]. Widening the axis to counterfactual candidates would
//! require a counterfactual store this repository does not have.

mod cohort;
mod config;
mod drift;
mod errors;
mod kway;
mod marginal;
mod measure;
mod report;
mod run;
mod types;

pub use cohort::*;
pub use config::*;
pub use drift::*;
pub use errors::*;
pub use kway::*;
pub use marginal::*;
pub use measure::*;
pub use report::*;
pub use run::*;
pub use types::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Reliability and drift bins, when the configuration does not say.
pub const DEFAULT_RELIABILITY_BINS: usize = 10;
/// Default drift bin count.
pub const DEFAULT_DRIFT_BINS: usize = 10;

/// Floor substituted for a zero bin proportion inside the population stability
/// index. A zero would make the term infinite, and an infinite drift number is
/// a refusal expressed as a measurement.
pub const PSI_PROPORTION_FLOOR: f64 = 1e-6;

/// Default clamp applied to a finite, in-range success probability before it is
/// logged. `ln(0)` is not a number and `ln(1)` is not finite.
pub const DEFAULT_PROBABILITY_FLOOR: f64 = 1e-6;

/// Clamp used only inside a log, mirroring the accepted evaluator's own guard.
pub const LOG_LOSS_CLAMP: f64 = 1e-15;

/// FNV-1a 64-bit offset basis, for the content fingerprint.
const FINGERPRINT_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FINGERPRINT_PRIME: u64 = 0x0000_0100_0000_01b3;

/// What the emitted distribution **is**, restated in every report.
///
/// There is no consumer. The distribution is a diagnostic and planning
/// artifact; it is not a live probability, and nothing routes on it.
pub const DISTRIBUTION_ROLE: &str = "unconsumed offline diagnostic and planning artifact; \
no installed consumer, no routing effect, and not a live probability";
