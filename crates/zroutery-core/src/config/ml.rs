//! Whether a promoted model may re-order the plan, where its durable state lives, and the utility weights it is scored under.

use serde::{Deserialize, Serialize};

use super::app::AppConfig;

/// How the learned model participates in routing.
///
/// This is the switch that replaced the previous arrangement, in which the model
/// could not affect a served request at all. It defaults to off, so an
/// installation that has never promoted a model routes exactly as it did before
/// any of this existed — turning it on is a decision made after a promotion, not
/// a default.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlRoutingConfig {
    /// Whether a promoted model may re-order the provider plan.
    #[serde(default)]
    pub enabled: bool,
    /// Where the durable trace log and the active-model pointer live, relative
    /// to the application state directory.
    ///
    /// **Empty by default, and empty means no durable ML state.** This was an
    /// OS-derived default and it was wrong: every `AppState` in every process
    /// then opened the same directory, so unrelated instances appended to one
    /// another's history, and a test run wrote to the developer's real data
    /// directory. A hidden global directory is not a default, it is a shared
    /// mutable resource nobody asked for.
    ///
    /// A deployment that wants history across restarts sets this explicitly, and
    /// a deployment that wants a promoted model to survive a restart must. The
    /// in-memory dataset and the shadow still work either way; only durability
    /// and promotion are off.
    #[serde(default)]
    pub state_dir: String,
    /// Probability that a request deliberately tries an eligible candidate other
    /// than the one the model would pick. Zero means no exploration.
    #[serde(default)]
    pub exploration_probability: f64,
    /// Seed for exploration. Fixed by default so a replay of the same request
    /// explores, or does not, identically.
    #[serde(default = "MlRoutingConfig::default_exploration_seed")]
    pub exploration_seed: u64,
}

#[cfg(feature = "ml")]
impl Default for MlRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            state_dir: String::new(),
            exploration_probability: 0.0,
            exploration_seed: Self::default_exploration_seed(),
        }
    }
}

#[cfg(feature = "ml")]
impl MlRoutingConfig {
    fn default_exploration_seed() -> u64 {
        crate::ml::learning::TRAINING_DEFAULT_SEED
    }

    /// Whether durable ML state was asked for.
    ///
    /// False means the trace log and the active-model store are not opened at
    /// all, rather than opened somewhere implicit.
    pub fn has_state_dir(&self) -> bool {
        !self.state_dir.trim().is_empty()
    }
}

#[cfg(all(test, feature = "ml"))]
mod ml_routing_defaults {
    use super::MlRoutingConfig;

    /// Compiling the learning stack into a binary must not by itself change how
    /// that binary routes.
    ///
    /// ADR-0006 made the desktop app depend on `zroutery-core/ml`, and the
    /// temptation that creates is to reason that the stack is now live because it
    /// is linked in. It is not: what makes a model able to rank is
    /// `enabled`, and what makes history durable is a state directory. Both are
    /// off by default, so an installation that has never promoted a model routes
    /// exactly as it did before the feature existed.
    ///
    /// Asserted as behaviour rather than by reading `Default`'s body, because the
    /// property that matters is what a default-constructed config *does*, not how
    /// it is written.
    #[test]
    fn a_default_configuration_cannot_rank_with_a_model_and_keeps_nothing() {
        let config = MlRoutingConfig::default();
        assert!(
            !config.enabled,
            "a default configuration lets a model re-order the provider plan"
        );
        assert!(
            !config.has_state_dir(),
            "a default configuration opens durable state somewhere implicit"
        );
        assert_eq!(
            config.exploration_probability, 0.0,
            "a default configuration explores"
        );
    }

    /// A configuration read from a document that predates the feature, or that
    /// simply never mentions it, must land in the same place.
    ///
    /// This is the upgrade path every existing install takes, and it is the one
    /// where "opt-in" is easy to get wrong: a missing `ml_routing` block has to
    /// mean off, not mean "use whatever is in memory".
    #[test]
    fn a_document_without_an_ml_routing_block_lands_in_the_same_place() {
        let config: MlRoutingConfig =
            serde_json::from_str("{}").expect("an empty block deserialises");
        assert!(!config.enabled);
        assert!(!config.has_state_dir());
    }
}

#[cfg(feature = "ml")]
impl AppConfig {
    /// The utility weights the learned model is scored and ranked under.
    ///
    /// Shared with training, comparison and promotion on purpose: a predictor is
    /// only meaningful relative to the utility it is meant to maximise, and four
    /// components holding four different policies would make a promotion decision
    /// unfalsifiable.
    pub fn ml_routing_reward_policy(&self) -> crate::ml::RewardPolicy {
        crate::ml::RewardPolicy::default()
    }
}
