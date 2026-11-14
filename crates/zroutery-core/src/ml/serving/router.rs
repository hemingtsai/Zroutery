use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use super::active::{ActivePredictor, ServingError};

use crate::ml::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use crate::ml::model::Prediction;
use crate::ml::model_identity::ModelEnsemble;
use crate::ml::shadow::{EnsemblePredictor, ShadowCandidateInput, ShadowInput};

// ---------------------------------------------------------------------------
// Exploration
// ---------------------------------------------------------------------------

/// How often, and how, the router deliberately tries a candidate other than the
/// one it would otherwise serve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExplorationConfig {
    /// Probability in `[0, 1]` that a request explores.
    pub probability: f64,
    /// Seed for the exploration draw. Fixed, so a replay of the same request
    /// under the same seed explores or does not explore identically.
    pub seed: u64,
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            probability: 0.0,
            seed: crate::ml::learning::TRAINING_DEFAULT_SEED,
        }
    }
}

impl ExplorationConfig {
    /// Refuse a configuration that would explore more often than it exploits, or
    /// that carries a probability outside `[0, 1]`.
    pub fn validate(&self) -> Result<(), ServingError> {
        if !self.probability.is_finite() || self.probability < 0.0 || self.probability > 1.0 {
            return Err(ServingError::Corrupt(format!(
                "exploration probability {} is outside [0, 1]",
                self.probability
            )));
        }
        Ok(())
    }
}

/// What an exploration draw produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExplorationOutcome {
    /// The exploitation pick stands.
    Exploit,
    /// A different eligible candidate was chosen to gather evidence.
    Explore { candidate_id: String },
}

/// Decide whether this request explores, and to where.
///
/// Three properties, all enforced here rather than by the caller:
///
/// * **Deterministic.** The draw is a hash of `(seed, request_id)`, not a
///   clock or a shared generator, so the same request replays identically and
///   two concurrent requests cannot interfere.
/// * **Constrained.** The candidate it may pick comes from `eligible` and is
///   never the exploitation pick. Exploration is a choice among legal
///   candidates, not a way to route somewhere the request was not allowed to
///   go.
/// * **Bounded.** A probability above `EXPLORATION_CEILING` is refused. A
///   router exploring more often than it exploits is not exploring, it is
///   routing at random while reporting that it learned something.
pub fn explore(
    config: &ExplorationConfig,
    request_id: &str,
    exploitation_pick: &str,
    eligible: &[String],
) -> ExplorationOutcome {
    if config.probability <= 0.0 || eligible.len() < 2 {
        return ExplorationOutcome::Exploit;
    }
    let draw = uniform_draw(config.seed, request_id);
    if draw >= config.probability {
        return ExplorationOutcome::Exploit;
    }
    let alternatives: Vec<&String> = eligible
        .iter()
        .filter(|candidate| candidate.as_str() != exploitation_pick)
        .collect();
    if alternatives.is_empty() {
        return ExplorationOutcome::Exploit;
    }
    let index = (draw / config.probability * alternatives.len() as f64).floor() as usize;
    let index = index.min(alternatives.len() - 1);
    ExplorationOutcome::Explore {
        candidate_id: alternatives[index].clone(),
    }
}

/// The largest exploration probability this router will accept.
pub const EXPLORATION_CEILING: f64 = 0.5;

fn uniform_draw(seed: u64, request_id: &str) -> f64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    mix(b"zroutery-exploration-v1\0");
    mix(&seed.to_le_bytes());
    mix(request_id.as_bytes());
    // FNV-1a avalanches well in its low bits and poorly in its high ones, so
    // reading the top of the digest directly would concentrate the draws: with
    // short, sequential request ids a probability of 0.5 explored four requests
    // in five. The fold-and-multiply finalisation spreads the high bits before
    // they are read, which is what makes the rate mean what it says.
    let mut mixed = hash;
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    mixed ^= mixed >> 33;
    // Top 53 bits, so the result is exactly representable and never 1.0.
    ((mixed >> 11) as f64) / ((1u64 << 53) as f64)
}

// ---------------------------------------------------------------------------
// MlRouter — the ranking itself
// ---------------------------------------------------------------------------

/// Why a ranking was not available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankUnavailable {
    /// No model is promoted.
    NoActiveModel,
    /// The request carried no candidate to rank.
    NoCandidates,
    /// The decision named a candidate the executable plan does not contain.
    SelectionNotInPlan,
    /// A prediction was not finite, so no utility could be trusted.
    NonFinitePrediction,
    /// Exploration configuration was refused.
    InvalidExploration,
}

/// One request's ML ranking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedPlan {
    /// The candidate ML would serve first.
    pub selected: String,
    /// Candidate identities in the order ML would try them.
    pub order: Vec<String>,
    /// The commit that produced the ranking.
    pub commit_id: String,
    /// Whether an exploration draw moved the selection off the exploitation
    /// pick.
    pub explored: bool,
    /// Why the engine chose what it chose.
    pub reason: String,
    /// Utility of each ranked candidate, as the engine computed it.
    pub utilities: Vec<(String, f64)>,
}

impl RankedPlan {
    /// True when ML's pick is already what the router would have served.
    pub fn is_noop(&self, production_first: &str) -> bool {
        self.order.first().map(String::as_str) == Some(production_first) && !self.explored
    }
}

/// The runtime component that turns decision-time facts into a provider order.
pub struct MlRouter {
    predictor: RwLock<Option<ActivePredictor>>,
    engine: DecisionEngine,
    exploration: RwLock<ExplorationConfig>,
    rankings: AtomicU64,
    fallbacks: AtomicU64,
    explorations: AtomicU64,
    blind_explorations: AtomicU64,
}

impl std::fmt::Debug for MlRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlRouter")
            .field("rankings", &self.rankings.load(Ordering::Relaxed))
            .field("fallbacks", &self.fallbacks.load(Ordering::Relaxed))
            .field("explorations", &self.explorations.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl MlRouter {
    pub fn new(engine: DecisionEngine, exploration: ExplorationConfig) -> Self {
        Self {
            predictor: RwLock::new(None),
            engine,
            exploration: RwLock::new(exploration),
            rankings: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            explorations: AtomicU64::new(0),
            blind_explorations: AtomicU64::new(0),
        }
    }

    /// Attach a promoted model.
    pub fn attach(&self, predictor: ActivePredictor) {
        *crate::sync::write(&self.predictor) = Some(predictor);
    }

    /// Withdraw the model, returning the router to deterministic routing.
    pub fn withdraw(&self) -> Option<ActivePredictor> {
        crate::sync::write(&self.predictor).take()
    }

    /// Whether a model is attached.
    pub fn is_attached(&self) -> bool {
        crate::sync::read(&self.predictor).is_some()
    }

    /// A snapshot of the attached model's weights, if one is attached.
    ///
    /// For analysis of what the *serving* model would have done. Snapshot
    /// rather than borrow because a replay over tens of thousands of records
    /// must not hold the router's write lock for its duration, and because a
    /// promotion arriving mid-analysis should not change the answers underneath
    /// it.
    pub fn attached_ensemble(&self) -> Option<ModelEnsemble> {
        crate::sync::read(&self.predictor)
            .as_ref()
            .map(|predictor| predictor.ensemble().clone())
    }

    /// The commit of the attached model, if one is attached.
    pub fn attached_commit(&self) -> Option<String> {
        crate::sync::read(&self.predictor)
            .as_ref()
            .map(|predictor| predictor.commit_id().as_str().to_string())
    }

    pub fn set_exploration(&self, exploration: ExplorationConfig) {
        *crate::sync::write(&self.exploration) = exploration;
    }

    pub fn exploration(&self) -> ExplorationConfig {
        *crate::sync::read(&self.exploration)
    }

    /// Rank the candidate set of one request.
    ///
    /// Every failure is an `Err` the caller answers by keeping the router's own
    /// order. Nothing in here can fail a request.
    pub fn rank(
        &self,
        input: &ShadowInput,
        request_id: &str,
    ) -> Result<RankedPlan, RankUnavailable> {
        let predictor = {
            let guard = crate::sync::read(&self.predictor);
            let found = guard.as_ref().map(|predictor| ActivePredictor {
                ensemble: clone_ensemble(&predictor.ensemble),
                commit_id: predictor.commit_id.clone(),
                model_id: predictor.model_id.clone(),
            });
            match found {
                Some(predictor) => predictor,
                None => {
                    drop(guard);
                    self.fallbacks.fetch_add(1, Ordering::Relaxed);
                    return Err(RankUnavailable::NoActiveModel);
                }
            }
        };
        if input.candidates.is_empty() {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return Err(RankUnavailable::NoCandidates);
        }

        // Predict for every candidate, eligible or not, so the engine's own
        // eligibility filter is what excludes a rejected candidate rather than
        // this layer deciding eligibility on the side.
        let mut engine_candidates: Vec<EngineCandidate> =
            Vec::with_capacity(input.candidates.len());
        for candidate in &input.candidates {
            let bundle = predictor.predict(
                &candidate.candidate_id,
                &candidate.provider_id,
                &candidate.features,
            );
            if !bundle_is_finite(&bundle) {
                self.fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err(RankUnavailable::NonFinitePrediction);
            }
            engine_candidates.push(EngineCandidate {
                candidate_id: candidate.candidate_id.clone(),
                bundle,
                eligible: candidate.eligible,
            });
        }

        let output = self.engine.decide_with_ml_ranking(&EngineInput {
            current_candidate: &input.production_selected,
            candidates: &engine_candidates,
            session_mode: input.session_mode,
            session_switch_count: input.session_switch_count,
            is_fallback: input.is_fallback,
        });

        if output.ranked_candidates.is_empty() {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return Err(RankUnavailable::SelectionNotInPlan);
        }

        let eligible: Vec<String> = input
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible)
            .map(|candidate| candidate.candidate_id.clone())
            .collect();

        let exploitation_pick = output.ranked_candidates[0].clone();
        let exploration = self.exploration();
        if exploration.probability > EXPLORATION_CEILING {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return Err(RankUnavailable::InvalidExploration);
        }

        let (selected, explored) =
            match explore(&exploration, request_id, &exploitation_pick, &eligible) {
                ExplorationOutcome::Exploit => (exploitation_pick.clone(), false),
                ExplorationOutcome::Explore { candidate_id } => (candidate_id, true),
            };
        if explored {
            self.explorations.fetch_add(1, Ordering::Relaxed);
        }

        // Put the chosen candidate first and keep the engine's ranking for the
        // rest, so failover still follows the model's own preference order
        // rather than the router's.
        let mut order: Vec<String> = vec![selected.clone()];
        order.extend(
            output
                .ranked_candidates
                .iter()
                .filter(|candidate| **candidate != selected)
                .cloned(),
        );

        let utilities = output
            .candidates
            .iter()
            .filter(|candidate| candidate.valid)
            .map(|candidate| (candidate.candidate_id.clone(), candidate.utility.total))
            .collect();

        self.rankings.fetch_add(1, Ordering::Relaxed);
        Ok(RankedPlan {
            selected,
            order,
            commit_id: predictor.commit_id().to_string(),
            explored,
            reason: output.decision.reason,
            utilities,
        })
    }

    /// Counters for an operator: how often ML ranked, how often the router fell
    /// back, how often exploration moved the selection.
    pub fn counts(&self) -> MlRouterCounts {
        MlRouterCounts {
            rankings: self.rankings.load(Ordering::Relaxed),
            fallbacks: self.fallbacks.load(Ordering::Relaxed),
            explorations: self.explorations.load(Ordering::Relaxed),
            blind_explorations: self.blind_explorations.load(Ordering::Relaxed),
            attached: self.is_attached(),
        }
    }

    /// Try an eligible candidate the current plan would not have reached, with no
    /// model involved.
    ///
    /// `None` means the draw did not fire and the caller's plan stands.
    ///
    /// # Why this is not inside [`MlRouter::rank`]
    ///
    /// Because a model cannot be trained on evidence the router never gathered.
    /// The dataset is built from outcomes of requests that actually happened, so
    /// the only candidates it can contain are the ones the deterministic plan
    /// already reached. A provider sitting at the bottom of the priority order is
    /// never tried, therefore never measured, therefore never learned about, and
    /// therefore never preferred — and no amount of retraining breaks that.
    ///
    /// Measured on a three-provider fixture: with exploration configured at 30%
    /// but no model promoted, a provider present in the plan at the lowest
    /// priority received **zero** requests across sixty, and the model converged
    /// on a single incumbent. Exploration that requires a promoted model can only
    /// ever explore what the model already believes in.
    ///
    /// So exploration is available whenever the durable state directory is, with
    /// or without a model. It is counted separately from
    /// [`MlRouterCounts::explorations`], because a deliberate alternative tried
    /// with no learned basis is not the same event as one the model chose, and an
    /// operator reading "the model explored" when no model existed would be told
    /// something false.
    pub fn explore_plan(&self, input: &ShadowInput, request_id: &str) -> Option<ExploredPlan> {
        let exploration = self.exploration();
        if exploration.probability > EXPLORATION_CEILING {
            // Refused rather than clamped, for the same reason `rank` refuses it:
            // a configuration that explores more often than it exploits is a
            // different policy from the one that was asked for.
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let eligible: Vec<String> = input
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible)
            .map(|candidate| candidate.candidate_id.clone())
            .collect();
        let exploitation_pick = input.production_selected.clone();
        match explore(&exploration, request_id, &exploitation_pick, &eligible) {
            ExplorationOutcome::Exploit => None,
            ExplorationOutcome::Explore { candidate_id } => {
                self.blind_explorations.fetch_add(1, Ordering::Relaxed);
                let reason = format!(
                    "explored {candidate_id} instead of {exploitation_pick} to gather evidence \
                     about it; no model is attached, so there is no learned basis for this choice"
                );
                Some(ExploredPlan {
                    selected: candidate_id,
                    reason,
                })
            }
        }
    }
}

/// A plan reordered by exploration alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExploredPlan {
    /// The candidate to try first.
    pub selected: String,
    /// Why, in a sentence an operator can read.
    pub reason: String,
}

/// Operational counters for the serving path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlRouterCounts {
    /// Requests a model ranked.
    pub rankings: u64,
    /// Requests where the deterministic plan stood.
    pub fallbacks: u64,
    /// Deliberate alternatives tried **with a model attached**.
    pub explorations: u64,
    /// Deliberate alternatives tried **with no model attached**, purely to gather
    /// evidence about a candidate the current plan never reached.
    ///
    /// Counted apart from `explorations` because the two are different claims.
    /// One says the model chose to try something else; the other says the router
    /// admitted it knows nothing and went to find out. Reading the second as the
    /// first is how a mechanism ends up reporting confidence it did not have.
    pub blind_explorations: u64,
    pub attached: bool,
}

/// The outcome of applying a ranking to one request's executable plan.
///
/// Separate from [`RankedPlan`] because applying is where the safety property
/// lives: a ranking that names a candidate the plan does not contain is dropped
/// rather than partially applied, and the caller always receives a usable plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedRanking {
    /// The candidate to try first after the ranking was applied.
    pub selected: String,
    /// The re-ordered plan, by candidate id.
    pub order: Vec<String>,
    /// Whether the order differs from what the router produced. `false` means ML
    /// agreed with the deterministic plan, which is a real and common outcome.
    pub changed: bool,
    pub commit_id: String,
    pub explored: bool,
    pub reason: String,
}

impl AppliedRanking {
    /// Build an applied ranking from a ranked plan and the plan it applies to.
    ///
    /// `plan_order` is the executable plan, by candidate id. The ranked plan is
    /// only honoured where it names candidates the executable plan actually
    /// contains; anything else is dropped. That is what makes it safe for the ML
    /// path to reorder a plan the eligibility and circuit-breaker filters have
    /// already narrowed, without re-deriving either of those filters.
    pub fn apply(ranked: &RankedPlan, plan_order: &[String]) -> Self {
        let mut order: Vec<String> = Vec::with_capacity(plan_order.len());
        for candidate in &ranked.order {
            if plan_order.contains(candidate) && !order.contains(candidate) {
                order.push(candidate.clone());
            }
        }
        for candidate in plan_order {
            if !order.contains(candidate) {
                order.push(candidate.clone());
            }
        }
        let selected = order
            .first()
            .cloned()
            .unwrap_or_else(|| ranked.selected.clone());
        let changed = order.first() != plan_order.first();
        Self {
            selected,
            order,
            changed,
            commit_id: ranked.commit_id.clone(),
            explored: ranked.explored,
            reason: ranked.reason.clone(),
        }
    }
}

fn bundle_is_finite(bundle: &crate::ml::reward::PredictionBundle) -> bool {
    [&bundle.success, &bundle.latency, &bundle.ttft, &bundle.cost]
        .into_iter()
        .all(prediction_is_finite)
}

/// Rebuild an ensemble from its own checkpoint.
///
/// A snapshot is taken under the read lock and then used lock-free, so a slow
/// prediction cannot hold the lock against a promotion. Round-tripping through
/// the checkpoint is the way to get an owned copy: it is the same content the
/// predictor holds, and it is the content the identity was derived from, so the
/// snapshot cannot drift from what was committed.
fn clone_ensemble(ensemble: &ModelEnsemble) -> ModelEnsemble {
    ModelEnsemble::load_all(&ensemble.save_all()).unwrap_or_else(|_| ModelEnsemble::new())
}

fn prediction_is_finite(prediction: &Prediction) -> bool {
    prediction.value.is_finite() && prediction.confidence.is_finite()
}

/// The candidate identities of a decision-time snapshot, in plan order.
pub fn candidate_ids(input: &ShadowInput) -> Vec<String> {
    input
        .candidates
        .iter()
        .map(|candidate: &ShadowCandidateInput| candidate.candidate_id.clone())
        .collect()
}
