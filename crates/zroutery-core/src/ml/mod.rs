//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates, and the training dataset
//! that collects samples for model training.

pub mod bandit;
pub mod calibration;
pub mod coordinator;
pub mod dataset;
pub mod decision_contract;
pub mod decision_engine;
pub mod evaluation;
pub mod features;
pub mod model;
pub mod model_identity;
pub mod reward;
pub mod shadow;
pub mod warmup;
pub use bandit::{
    accepted_outcome_proxy_score, compare_outcome_proxy, percentile, policy_from_weights,
    run_bandit, weights_vector, ArmSafetyMetrics, ArmSelectionStatistics, BanditConfig, BanditError,
    BanditOutcome, BanditReport, OutcomeProxy, RewardArm, RewardBasis, RewardFitConfig,
    RewardFitReport, RewardFitVerdict, SafetyConfig, SafetyEvaluation, SafetyEvidence, SafetyTolerances,
    SafetyVerdict, SafetyViolation, SelectionConfig, SelectionTrace, ACCEPTED_PRIOR_ARM_NAME,
    B_COST, B_FALLBACK, B_LATENCY, B_SUCCESS, B_SWITCH, B_UNCERTAINTY, BASIS_WIDTH, DEFAULT_SEED,
    DEFAULT_TAIL_PERCENTILE, FITTED_ARM_NAME, OUTCOME_PROXY_FIT_TARGET, OUTCOME_PROXY_ORDER,
    REWARD_FIT_TARGET_DESCRIPTION, UNIDENTIFIABLE_REASON, UNIDENTIFIABLE_WEIGHT, WEIGHT_NAMES,
};
pub use calibration::{
    collect_marginal_observations, measure_drift, measure_emitted, measure_marginal, project_cohorts,
    run_calibration, AcceptanceTolerances, CalibrationConfig, CalibrationError, CalibrationMeasure,
    CalibrationOutcome, CalibrationReport, CalibrationVerdict, CandidateInput, CandidateIntercept,
    CohortContext, DecisionCohort, DegeneracyReason, DistributionRecord, DriftConfig,
    DriftMeasurement, DriftTolerances, DriftVerdict, EmittedDecision, FitConfig, HoldoutConfig,
    KWayCalibrator, MarginalCalibration, MarginalCalibrator, MarginalFitConfig, MarginalObservation,
    MarginalView, NormalizationDamage, PartitionKind, ReliabilityBin, ReliabilityConfig,
    ReliabilityCurve, UnrankedReason, DEFAULT_DRIFT_BINS, DEFAULT_PROBABILITY_FLOOR,
    DEFAULT_RELIABILITY_BINS, DISTRIBUTION_ROLE,
};
pub use coordinator::{Coordinator, CoordinatorConfig, RoutingAction, RoutingDecision};
pub use dataset::{
    canonical_samples_from_outcome, outcome_to_dataset_sample, sample_from_outcome, samples_from_outcome,
    try_outcome_sample, try_samples_from_outcome, try_samples_from_outcome_with_feedback,
    validate_outcome_sample, CanonicalTrainingSample, DatasetStore, OutcomeDatasetSample,
    OutcomeTrainingSample, SampleBuilder, SampleScope, Targets, TrainingSample as DatasetTrainingSample,
    validate_sample,
};
pub use decision_contract::{
    CandidateEligibility, CandidatePredictions, CandidateRoles, CandidateScore,
    DecisionCandidate, DecisionContractError, DecisionDimension, DecisionDistribution,
    DecisionIdentities, DecisionModel, DecisionModelStates, DecisionPhase, DecisionState,
    ModelInput, OpenStep, SettledStep, DISTRIBUTION_NORMALIZATION_TOLERANCE,
};
pub use decision_engine::{DecisionEngine, EngineCandidate, EngineInput, EngineOutput};
pub use evaluation::{
    ComparisonReport, EvaluationError, Evaluator, FrozenHoldout, PredictionMetrics, Recommendation,
    RoutingDeltas, RoutingMetrics, temporal_split,
};
pub use features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
    UNKNOWN,
};
pub use model::{
    CostModel, LatencyModel, ModelState, Prediction, RoutingModel, SuccessModel, TtftModel,
};
pub use model_identity::{
    CommitId, CommitInfo, LearningEvent, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId,
    ModelRef, ModelStore, ReplayEngine, ReplayError,
};
pub use reward::{
    Action, ActionGuard, AttemptReward, PredictionBundle, RequestReward, RewardComputer,
    RewardPolicy, UtilityBreakdown, compute_utility,
};
pub use shadow::{
    EnsemblePredictor, ModelEnsemblePredictor, ProductionDecisionRef, ShadowCandidate,
    ShadowCandidateInput, ShadowDecision, ShadowEngine, ShadowInput, ShadowObservation, ShadowScope,
    ShadowStore, ShadowVerdict,
};
pub use warmup::{
    run_warmup, LabelCoverage, WarmupConfig, WarmupError, WarmupOutcome, WarmupReport,
    WarmupVerdict, BASELINE_DESCRIPTION,
};
