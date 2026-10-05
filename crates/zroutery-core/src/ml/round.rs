//! One promotion round: history in, a gate decision out.
//!
//! # Why this exists
//!
//! Every mechanism on the path from a served request to a re-ordered plan was built
//! and separately verified. What was missing is the *sequence* — and because it was
//! missing, every caller re-assembled it by hand. Two survived, both in test files:
//! `loop_over` in `ml_closed_loop_test.rs` and `learn` in `ml_multi_provider_test.rs`,
//! sharing the spine (load traces → dedupe → train → compare) and diverging at the
//! end, one into shadow analysis and the other into the gate and the store.
//!
//! That is the shape of the gap recorded as §E9 of the closed-loop report. Not one
//! of those two is the copy that runs, so the loop was closed end to end in the test
//! suite and open at both ends in the product. Two hand-rolled copies is also two
//! places for the spine to drift, and neither of them was obliged to keep the
//! shadow analysis that the other one does.
//!
//! This module is the spine, once. It is **mechanism, not policy**: it trains,
//! compares, analyses and judges, and it installs a decision the gate authorised. It
//! does not decide *when* a round happens or *on whose authority*, because those are
//! product decisions and are not taken here — see the report's §E9.
//!
//! # What it deliberately does not do
//!
//! It does not schedule itself, expose an endpoint, retry, or hold state between
//! calls. It is a function you call when you have decided to, which is what makes it
//! safe to call from a test harness *and* from an entry point: the harness and the
//! product would then run the same code rather than two similar copies.
//!
//! # Why installing is separate from judging
//!
//! [`PromotionRound::install`] is a separate call rather than something `run` does.
//! A round that trains and judges is useful on its own — it is how you find out *why*
//! a model was refused, and `ml_shadow_analysis` is the operator-facing form of that.
//! Folding the install in would make the expensive, informative half unreachable
//! without also mutating the store.

use serde::{Deserialize, Serialize};

use super::comparison::{run_comparison, MlPolicy, ReplayBaseline, RoutingComparison};
use super::learning::{run_training, TrainingConfig, TrainingOutcome};
use super::promotion::{PromotionConfig, PromotionDecision, PromotionGate, PromotionVerdict};
use super::reward::RewardPolicy;
use super::serving::ActiveModelStore;
use super::shadow_analysis::{analyse, ShadowAnalysis, ShadowEvidence};
use super::traces::{deduped_samples_from, RequestTrace, TraceLog};

/// Why a round could not be run at all.
///
/// Distinct from a gate that *refused*: a refusal is a successful round with an
/// answer, and this is the absence of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundError {
    /// The durable log could not be read.
    TraceUnavailable(String),
    /// The log holds nothing that can be learned from.
    ///
    /// Its own case rather than a generic empty body, because "no traffic yet" and
    /// "traffic arrived but none of it carried a decision" want different responses
    /// from whoever schedules the round, and neither is an error in the round.
    NothingToLearn,
    /// The split or the fit failed.
    Training(String),
    /// The replay over the body failed.
    Comparison(String),
}

impl std::fmt::Display for RoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TraceUnavailable(why) => write!(f, "the durable log could not be read: {why}"),
            Self::NothingToLearn => write!(
                f,
                "the durable log holds no learnable body, so there is nothing to judge"
            ),
            Self::Training(why) => write!(f, "training failed: {why}"),
            Self::Comparison(why) => write!(f, "comparison failed: {why}"),
        }
    }
}

impl std::error::Error for RoundError {}

/// Everything one round produced.
///
/// The artefacts are kept whole rather than reduced to the decision, because the
/// decision is only interpretable next to the evidence behind it: a refusal whose
/// `comparison` is not there is a verdict with no argument attached.
pub struct PromotionRound {
    /// How many traces were read, which is the bound applied rather than a claim
    /// that this is all the history there is.
    pub traces_read: usize,
    /// The fit and its checkpoint.
    pub training: TrainingOutcome,
    /// The learned policy replayed against every baseline.
    pub comparison: RoutingComparison,
    /// What the model would have done with this same traffic.
    pub analysis: ShadowAnalysis,
    /// The gate's verdict, in full.
    pub decision: PromotionDecision,
}

/// Summarised, not derived.
///
/// `TrainingOutcome` is neither `Debug` nor `Clone`-with-`Debug`, and a derived
/// impl would fail on a type this struct legitimately holds. What a log line or a
/// test failure needs is the verdict and the evidence behind it, not the whole
/// ensemble, so that is what this prints — the same reason `ActivePredictor` and
/// `MlRouter` write their own.
impl std::fmt::Debug for PromotionRound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromotionRound")
            .field("traces_read", &self.traces_read)
            .field("model_id", &self.training.model_id)
            .field("commit_id", &self.training.commit_id.as_str())
            .field("verdict", &self.decision.verdict.as_str())
            .field("paired_requests", &self.decision.paired_requests)
            .field("required_baseline", &self.decision.baseline)
            .field("agreements", &self.analysis.agreements)
            .field("disagreements", &self.analysis.disagreements)
            .finish_non_exhaustive()
    }
}

impl PromotionRound {
    /// Whether the gate authorised this model.
    pub fn is_promoted(&self) -> bool {
        self.decision.verdict == PromotionVerdict::Promoted
    }

    /// Install the decision, if the gate authorised it.
    ///
    /// Separate from running the round so that training-and-judging stays usable on
    /// its own. Returns `None` for a refusal rather than erroring, because a refusal
    /// is the gate working: nothing was installed and nothing needed to be.
    ///
    /// The store re-derives the commit id from the checkpoint and refuses a decision
    /// whose identity does not match, so this cannot install a model under a
    /// borrowed name.
    pub fn install(&self, store: &ActiveModelStore) -> Result<Option<String>, String> {
        if !self.is_promoted() {
            return Ok(None);
        }
        store
            .promote(&self.decision, self.training.checkpoint.clone())
            .map(|commit| Some(commit.to_string()))
            .map_err(|error| error.to_string())
    }
}

/// Configuration for one round. Defaults are the shipped ones.
///
/// Taken as a struct rather than four positional arguments so that a caller adding a
/// fifth knob does not have to reorder the four, and so the gate configuration is
/// visibly separate from the training configuration — they are different decisions
/// and were being passed together in both hand-rolled copies.
#[derive(Debug, Clone, Default)]
pub struct RoundConfig {
    pub training: TrainingConfig,
    pub gate: PromotionConfig,
    pub reward_policy: RewardPolicy,
}

/// Run one round over the durable log in `state_dir`.
///
/// The full spine: read history, fit a model, replay it against every baseline,
/// analyse the counterfactual, and put it to the gate.
///
/// `revision` is recorded on the decision and is free-form; it is what lets an
/// operator tie a promotion or refusal back to the round that produced it.
///
/// This installs nothing. Call [`PromotionRound::install`] for that, so a round can
/// be run purely to find out what the gate would say.
pub fn run_promotion_round(
    state_dir: &std::path::Path,
    config: &RoundConfig,
    revision: Option<String>,
) -> Result<PromotionRound, RoundError> {
    let traces = read_traces(state_dir)?;
    let traces_read = traces.len();
    if traces.is_empty() {
        return Err(RoundError::NothingToLearn);
    }

    let samples = deduped_samples_from(&traces);
    if samples.is_empty() {
        return Err(RoundError::NothingToLearn);
    }

    let training = run_training(&samples, &config.training)
        .map_err(|e| RoundError::Training(e.to_string()))?;
    let policy = config.reward_policy.clone();
    let candidate = MlPolicy::new(&training, policy.clone());
    let comparison = run_comparison(&traces, &candidate, &ReplayBaseline::ALL, &policy)
        .map_err(|e| RoundError::Comparison(e.to_string()))?;
    let evidence = ShadowEvidence::from_policy(&traces, &candidate, Default::default());
    let analysis = analyse(&traces, &evidence, &policy);
    let decision =
        PromotionGate::new(config.gate.clone()).evaluate(&training.report, &comparison, revision);

    Ok(PromotionRound {
        traces_read,
        training,
        comparison,
        analysis,
        decision,
    })
}

fn read_traces(state_dir: &std::path::Path) -> Result<Vec<RequestTrace>, RoundError> {
    let log = TraceLog::open(state_dir).map_err(|e| RoundError::TraceUnavailable(e.to_string()))?;
    log.load()
        .map_err(|e| RoundError::TraceUnavailable(e.to_string()))
}
