//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates, and the training dataset
//! that collects samples for model training.

pub mod activation;
pub mod attribution;
pub mod bandit;
pub mod calibration;
pub mod comparison;
pub mod coordinator;
pub mod dataset;
pub mod decision_contract;
pub mod decision_engine;
pub mod evaluation;
pub mod features;
pub mod journal;
pub mod learning;
pub mod model;
pub mod model_identity;
pub mod offline_gate;
pub mod promotion;
pub mod reward;
pub mod serving;
pub mod shadow;
pub mod shadow_analysis;
pub mod statistics;
pub mod status;
pub mod traces;
pub mod warmup;
pub use activation::{
    activation_applied_event_id, activation_plan_event_id, pointer_checksum, snapshot_checksum,
    snapshot_id_for, ActivationAudit, ActivationEntry, ActivationError, ActivationKind,
    ActivationOutcome, ActivationPointer, ActivationRequest, ActivationStage, ActivationStore,
    ActivationTrace, ActiveSnapshot, CheckpointFile, CommitFile, JournalContext, PendingActivation,
    PointerDisagreement, PointerEntryFile, PointerFile, PointerState, Snapshot, SnapshotFile,
    SnapshotId, StateFile, ACTIVATION_LOCK_NAME, ACTIVATION_POINTER_NAME,
    ACTIVATION_POINTER_SCHEMA_VERSION, ACTIVATION_POINTER_TMP_NAME, ACTIVATION_ROLE,
    ACTIVATION_SNAPSHOT_SCHEMA_VERSION, ACTIVATION_SOURCE_PREFIX, DONE_EVENT_PREFIX,
    JOURNAL_DIR_NAME, PLAN_EVENT_PREFIX, SNAPSHOTS_DIR_NAME, SNAPSHOT_FILE_SUFFIX,
    SNAPSHOT_ID_HEX_DIGITS, SNAPSHOT_ID_PREFIX, SNAPSHOT_INCOMING_SUFFIX,
};
pub use attribution::{
    attribute, AttributionError, CandidateCredit, CreditLedger, DecisionOutcome, Independence,
};
pub use bandit::{
    accepted_outcome_proxy_score, compare_outcome_proxy, percentile, policy_from_weights,
    run_bandit, weights_vector, ArmSafetyMetrics, ArmSelectionStatistics, BanditConfig,
    BanditError, BanditOutcome, BanditReport, OutcomeProxy, RewardArm, RewardBasis,
    RewardFitConfig, RewardFitReport, RewardFitVerdict, SafetyConfig, SafetyEvaluation,
    SafetyEvidence, SafetyTolerances, SafetyVerdict, SafetyViolation, SelectionConfig,
    SelectionTrace, ACCEPTED_PRIOR_ARM_NAME, BASIS_WIDTH, B_COST, B_FALLBACK, B_LATENCY, B_SUCCESS,
    B_SWITCH, B_UNCERTAINTY, DEFAULT_SEED, DEFAULT_TAIL_PERCENTILE, FITTED_ARM_NAME,
    OUTCOME_PROXY_FIT_TARGET, OUTCOME_PROXY_ORDER, REWARD_FIT_TARGET_DESCRIPTION,
    UNIDENTIFIABLE_REASON, UNIDENTIFIABLE_WEIGHT, WEIGHT_NAMES,
};
pub use calibration::{
    collect_marginal_observations, measure_drift, measure_emitted, measure_marginal,
    project_cohorts, run_calibration, AcceptanceTolerances, CalibrationConfig, CalibrationError,
    CalibrationMeasure, CalibrationOutcome, CalibrationReport, CalibrationVerdict, CandidateInput,
    CandidateIntercept, CohortContext, DecisionCohort, DegeneracyReason, DistributionRecord,
    DriftConfig, DriftMeasurement, DriftTolerances, DriftVerdict, EmittedDecision, FitConfig,
    HoldoutConfig, KWayCalibrator, MarginalCalibration, MarginalCalibrator, MarginalFitConfig,
    MarginalObservation, MarginalView, NormalizationDamage, PartitionKind, ReliabilityBin,
    ReliabilityConfig, ReliabilityCurve, UnrankedReason, DEFAULT_DRIFT_BINS,
    DEFAULT_PROBABILITY_FLOOR, DEFAULT_RELIABILITY_BINS, DISTRIBUTION_ROLE,
};
pub use comparison::{
    aggregate, observed_utility, pair_against_baseline, run_comparison, ArmMetrics, ArmRecord,
    BaselinePairing, ComparisonError, MeasuredOutcome, MlPolicy, PairedDeltas, PolicyChoice,
    ReplayBaseline, ReplayPolicy, ReplayState, RoutingComparison, RoutingVerdict,
    MIN_PAIRED_REQUESTS, UTILITY_DELTA_THRESHOLD,
};
pub use coordinator::{Coordinator, CoordinatorConfig, RoutingAction, RoutingDecision};
pub use dataset::{
    canonical_samples_from_outcome, outcome_to_dataset_sample, sample_from_outcome,
    samples_from_outcome, try_outcome_sample, try_samples_from_outcome,
    try_samples_from_outcome_with_feedback, validate_outcome_sample, validate_sample,
    CanonicalTrainingSample, DatasetStore, OutcomeDatasetSample, OutcomeTrainingSample,
    SampleBuilder, SampleScope, Targets, TrainingSample as DatasetTrainingSample,
};
pub use decision_contract::{
    CandidateEligibility, CandidatePredictions, CandidateRoles, CandidateScore, DecisionCandidate,
    DecisionContractError, DecisionDimension, DecisionDistribution, DecisionIdentities,
    DecisionModel, DecisionModelStates, DecisionPhase, DecisionState, ModelInput, OpenStep,
    SettledStep, DISTRIBUTION_NORMALIZATION_TOLERANCE,
};
pub use decision_engine::{DecisionEngine, EngineCandidate, EngineInput, EngineOutput};
pub use evaluation::{
    f32_identical, f64_identical, find_nonfinite_f64, temporal_split, ulp_distance,
    ComparisonReport, Divergence, EvaluationError, Evaluator, Exactness, FrozenHoldout,
    NonFiniteComponent, PredictionMetrics, Recommendation, RoutingDeltas, RoutingMetrics,
};
pub use features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
    UNKNOWN,
};
pub use journal::{
    frame_checksum, CanonicalEvent, JournalError, JournalEvidence, JournalMode, JournalRecord,
    JournalRecordBody, JournalReport, LearningJournal, RecordOutcome, SequenceFault,
    ANCHOR_SCHEMA_VERSION, FIRST_SEQUENCE, JOURNAL_ANCHOR_NAME, JOURNAL_ANCHOR_TMP_NAME,
    JOURNAL_LOCK_NAME, JOURNAL_LOG_NAME, JOURNAL_ROLE, JOURNAL_SCHEMA_VERSION, LEGACY_DEGRADATION,
    MAX_FRAME_BYTES,
};
pub use learning::{
    evaluate_success, predict_bundle, prediction_is_finite, run_training, split_samples,
    success_log_loss, FeatureCoverage, LearningError, PassReport, Split, TrainingConfig,
    TrainingOutcome, TrainingReport, TRAINING_DEFAULT_SEED,
};
pub use model::{
    CostModel, LatencyModel, ModelState, Prediction, RoutingModel, SuccessModel, TtftModel,
};
pub use model_identity::{
    CommitId, CommitInfo, LearningEvent, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId,
    ModelRef, ModelStore, ReplayEngine, ReplayError,
};
pub use offline_gate::{
    run_offline_gate, CommitTransport, EvidenceFloors, FailureAuthority, FloatDrift, GateConfig,
    GateInput, GateOutcome, HoldoutSummary, JournalFloatFidelity, OfflineGateError,
    RecordedDecision, ReleaseMeasurements, ReleaseReport, ReleaseVerdict, ReplayEvidence,
    RetentionAblation, RetentionProof, ServedIdentity, TerminalAgreement, JOURNAL_FLOAT_NOTE,
    RELEASE_SCOPE,
};
pub use promotion::{
    comparison_was_improved, PromotionConfig, PromotionCriterion, PromotionDecision, PromotionGate,
    PromotionVerdict,
};
pub use reward::{
    compute_utility, Action, ActionGuard, AttemptReward, PredictionBundle, RequestReward,
    RewardComputer, RewardPolicy, UtilityBreakdown,
};
pub use serving::{
    candidate_ids, explore, ActiveModel, ActiveModelAction, ActiveModelAuditEntry,
    ActiveModelStore, ActivePredictor, AppliedRanking, ExplorationConfig, ExplorationOutcome,
    MlRouter, MlRouterCounts, RankUnavailable, RankedPlan, ACTIVE_MODEL_AUDIT_FILE,
    ACTIVE_MODEL_FILE, ACTIVE_MODEL_SCHEMA_VERSION, EXPLORATION_CEILING,
};
pub use shadow::{
    EnsemblePredictor, ModelEnsemblePredictor, ProductionDecisionRef, ShadowCandidate,
    ShadowCandidateInput, ShadowDecision, ShadowEngine, ShadowInput, ShadowObservation,
    ShadowScope, ShadowStore, ShadowVerdict,
};
pub use shadow_analysis::{
    analyse, ShadowAnalysis, ShadowEvidence, ShadowGap, ShadowKind, ShadowVerdictRecord,
};
pub use status::{
    MlStatus, PromotedModelStatus, PromotionHistoryEntry, ReloadOutcome, ShadowAnalysisStatus,
    ShadowStatus,
};
pub use statistics::{
    holm_adjust, ln_gamma, mcnemar_exact_log_p, mcnemar_exact_p, measure_release_evidence,
    normal_quantile, required_decisions, wilson_interval, BaselinePolicy, Criterion,
    EvidenceSupport, Family, FamilyMember, FamilyMemberKind, Interval, PairedComparison,
    StatisticalConfig, StatisticalInput, StatisticalRefusal, StatisticalRelease, StatisticsError,
    STATISTICAL_SCOPE, UNMEASURABLE_LABEL,
};
pub use traces::{
    contained_append, deduped_samples_from, samples_from, DatasetFingerprint, RequestTrace,
    TraceCounters, TraceError, TraceIngestion, TraceLog, TRACES_FILE_NAME, TRACE_SCHEMA_VERSION,
};
pub use warmup::{
    run_warmup, LabelCoverage, WarmupConfig, WarmupError, WarmupOutcome, WarmupReport,
    WarmupVerdict, BASELINE_DESCRIPTION,
};
