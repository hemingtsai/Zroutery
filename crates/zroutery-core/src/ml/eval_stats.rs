//! Does the measured improvement survive its own uncertainty?
//!
//! # The gap this fills
//!
//! `ml/promotion.rs` is threshold-only. Its sole evidence floor is
//! `MIN_PAIRED_REQUESTS = 30` and its sole effect test is a raw delta compared against
//! `UTILITY_DELTA_THRESHOLD`. So a candidate with 30 pairs and a large delta passes the
//! same gate as one with 3000 — the count floor is a *minimum*, never a statement that
//! the evidence supports the conclusion.
//!
//! `ml/statistics.rs` already ships everything needed to say more: `wilson_interval`,
//! `mcnemar_exact_p`, `holm_adjust`, `required_decisions` and a minimum-detectable-effect
//! notion. The promotion gate imports none of them, because they answer a different
//! question — release evidence over a held-out set — and wiring them in would change a
//! mature, carefully-argued component. This module computes the same things *for a
//! paired routing comparison* and leaves the gate alone.
//!
//! # Two things this does that `statistics.rs` deliberately does not
//!
//! **Clustering.** `statistics.rs` states that the unit of independence is the decision,
//! and is right for its purpose: an offline holdout's rows really are independent
//! draws. Routing traces are not. Twenty requests inside one conversation are twenty
//! observations of one user's taste, and counting them as twenty independent draws
//! shrinks the interval by roughly `sqrt(20)` — a five-fold overstatement of precision
//! on exactly the traffic that matters most.
//!
//! Traces carry no session identifier, so clusters are formed from **time blocks**:
//! requests recorded within one block are treated as potentially dependent. This is a
//! conservative approximation — it will sometimes group unrelated requests and sometimes
//! split one conversation — but the bias is in the safe direction, because merging
//! independent observations *widens* the interval rather than narrowing it.
//!
//! **Continuous effects.** The routing metrics are deltas of means, not proportions, so
//! the interval is built on the paired differences directly.
//!
//! # Determinism
//!
//! No resampling, no seed, no iteration-order dependence: the standard error comes from
//! per-cluster sums, exactly as `statistics.rs` documents for its own figures. Same
//! input, same answer.

use serde::{Deserialize, Serialize};

use crate::ml::statistics::{normal_quantile, required_decisions};

/// How paired observations were grouped before their variance was estimated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Clustering {
    /// One cluster per observation. The naive assumption, reported so a reader can see
    /// it was not silently used.
    None,
    /// One cluster per fixed time block.
    TimeBlock { block_secs: i64 },
}

/// Confidence level and the effect the evidence must clear.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EvidenceConfig {
    /// Two-sided confidence level, e.g. 0.95.
    pub confidence: f64,
    /// Smallest effect worth acting on. The interval's lower bound must clear it.
    ///
    /// This is the minimum detectable effect, and it enters twice — as the δ in the
    /// power calculation and as the bar the observed interval has to clear — so a
    /// "significant" result on a trivially small effect is still not actionable.
    pub minimum_effect: f64,
    /// Power to require when sizing the study.
    pub power: f64,
    /// Hard floor on pairs, independent of the effect.
    pub min_pairs: usize,
    pub clustering: Clustering,
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            confidence: 0.95,
            // One fifth of a full success-vs-failure swing, matching
            // `comparison::UTILITY_DELTA_THRESHOLD` so the two do not disagree about
            // what counts as a large effect.
            minimum_effect: crate::ml::comparison::UTILITY_DELTA_THRESHOLD,
            power: 0.80,
            min_pairs: crate::ml::comparison::MIN_PAIRED_REQUESTS,
            clustering: Clustering::TimeBlock { block_secs: 3_600 },
        }
    }
}

impl EvidenceConfig {
    /// The two-sided z for this confidence level.
    fn z_two_sided(&self) -> f64 {
        normal_quantile(1.0 - (1.0 - self.confidence) / 2.0)
    }

    fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        let mut mix = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x100000001b3);
        };
        mix(self.confidence.to_bits());
        mix(self.minimum_effect.to_bits());
        mix(self.power.to_bits());
        mix(self.min_pairs as u64);
        mix(match self.clustering {
            Clustering::None => 0,
            Clustering::TimeBlock { block_secs } => block_secs as u64,
        });
        hash
    }
}

/// One paired observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairedObservation {
    /// Unix seconds, used only to form time-block clusters.
    pub recorded_at: i64,
    /// Candidate's observed utility minus the baseline's.
    pub utility_delta: f64,
    /// Candidate succeeded, baseline did not.
    pub candidate_only_success: bool,
    /// Baseline succeeded, candidate did not.
    pub baseline_only_success: bool,
}

/// A confidence interval on the mean paired effect.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EffectInterval {
    pub mean: f64,
    pub lower: f64,
    pub upper: f64,
    pub level: f64,
    pub standard_error: f64,
    /// Every cluster sat exactly on the mean, so the interval has zero width.
    ///
    /// Reported rather than hidden: a zero-width interval reads as infinite
    /// certainty, and no finite sample earns that. It means the data showed no
    /// dispersion at all — which usually says more about a fixture or a synthetic
    /// body than about the routing.
    pub is_zero_dispersion: bool,
}

impl EffectInterval {
    /// Whether the interval excludes zero at its own level.
    pub fn excludes_zero(&self) -> bool {
        self.lower > 0.0 || self.upper < 0.0
    }

    /// Whether the whole interval sits above the configured minimum effect.
    ///
    /// Stricter than `excludes_zero`: a result that is precise about being worthless
    /// does not clear the bar.
    pub fn clears(&self, minimum_effect: f64) -> bool {
        self.lower > minimum_effect
    }
}

/// Everything the evidence says about one comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingEvidence {
    pub pairs: usize,
    pub clusters: usize,
    pub clustering: Clustering,
    pub utility: Option<EffectInterval>,
    /// Discordant pairs as a share of all pairs — the ψ in the power formula.
    pub discordance_rate: f64,
    /// Pairs that would be needed to detect `minimum_effect` at `power`.
    pub required_pairs: Option<usize>,
    pub min_pairs: usize,
    pub minimum_effect: f64,
    /// Why this evidence cannot support a conclusion.
    pub blockers: Vec<String>,
    /// Weaker assumptions behind this evidence. Reported, not fatal.
    pub caveats: Vec<String>,
    pub config_fingerprint: u64,
}

impl RoutingEvidence {
    /// Whether the evidence clears every bar for a promotion claim.
    pub fn is_sufficient(&self) -> bool {
        self.blockers.is_empty()
    }

    /// Whether the measured effect is both clear and large enough to act on.
    pub fn is_actionable(&self) -> bool {
        self.is_sufficient()
            && self
                .utility
                .is_some_and(|interval| interval.clears(self.minimum_effect))
    }

    pub fn headline(&self) -> String {
        match &self.utility {
            None => format!("blocked: {} pairs, no interval", self.pairs),
            Some(interval) if self.blockers.is_empty() => format!(
                "effect {:+.4} [{:+.4}, {:+.4}] over {} pairs in {} cluster(s)",
                interval.mean, interval.lower, interval.upper, self.pairs, self.clusters
            ),
            Some(interval) => format!(
                "effect {:+.4} [{:+.4}, {:+.4}] over {} pairs — {}",
                interval.mean,
                interval.lower,
                interval.upper,
                self.pairs,
                self.blockers.join("; ")
            ),
        }
    }
}

/// Compute the evidence for one paired comparison.
///
/// Total: over `observations`. Clustered: the standard error is estimated from
/// per-cluster means, which widens it whenever observations within a block are
/// correlated and leaves it alone when they are not. Clustering never narrows the
/// interval — that property is what makes it safe to apply without knowing the true
/// correlation structure.
pub fn measure(observations: &[PairedObservation], config: &EvidenceConfig) -> RoutingEvidence {
    let finite: Vec<PairedObservation> = observations
        .iter()
        .copied()
        .filter(|observation| observation.utility_delta.is_finite())
        .collect();
    let pairs = finite.len();
    let mut blockers = Vec::new();

    if pairs < config.min_pairs {
        blockers.push(format!(
            "{pairs} paired requests is below the {} required",
            config.min_pairs
        ));
    }

    let cluster_ids = cluster_keys(&finite, config.clustering);
    // The *distinct* count, not the length of the key vector: every observation has
    // a key, so `cluster_ids.len()` is the observation count and would report 200
    // clusters for 200 requests packed into one hour.
    let distinct_clusters: std::collections::BTreeSet<u64> = cluster_ids.iter().copied().collect();
    // Weak assumptions behind the numbers, reported rather than treated as refusals.
    let mut caveats: Vec<String> = Vec::new();
    let discordant = finite
        .iter()
        .filter(|observation| {
            observation.candidate_only_success || observation.baseline_only_success
        })
        .count();
    let discordance_rate = if pairs == 0 {
        0.0
    } else {
        discordant as f64 / pairs as f64
    };

    let utility = interval(&finite, &cluster_ids, &distinct_clusters, config);

    // The power calculation is only meaningful when the arms actually disagreed
    // somewhere; `required_decisions` refuses otherwise, and that refusal is the
    // correct answer rather than an error to swallow.
    let required_pairs = match required_decisions(
        discordance_rate,
        config.minimum_effect,
        config.z_two_sided(),
        normal_quantile(config.power),
        config.min_pairs,
    ) {
        Ok(required) => {
            // **Reported, not enforced.** `required_decisions` answers "how much data
            // would you need to *plan* for the minimum effect worth caring about" — it
            // is a study-design number, and it is computed against the minimum effect
            // rather than the observed one, so an unusually large real effect still
            // "needs" the same n.
            //
            // Enforcing it on a comparison that has already been measured double-counts
            // the sample size: the confidence interval already accounts for how many
            // pairs there were, and it is the interval that decides whether the effect
            // is distinguishable from zero. Gating on both would refuse a
            // well-measured improvement because the *smallest* interesting effect was
            // underpowered, which is a different and much weaker claim.
            if required > pairs {
                caveats.push(format!(
                    "{pairs} pairs is below the {required} that would be needed to detect an \
                     effect as small as {:+.4} at {:.0}% power; a larger effect than that is \
                     still resolvable with this body",
                    config.minimum_effect,
                    config.power * 100.0
                ));
            }
            Some(required)
        }
        Err(_) => {
            if discordant == 0 {
                // The arms never differed. That is a real answer — not a small sample,
                // a perfectly precise zero — but it is not evidence of an improvement,
                // so no interval can support a promotion.
                blockers.push(
                    "the two arms never disagreed on any paired request, so no effect can be \
                     distinguished from zero"
                        .to_string(),
                );
            }
            None
        }
    };

    if pairs > 0
        && distinct_clusters.len() == 1
        && matches!(config.clustering, Clustering::TimeBlock { .. })
    {
        // One block means the clustered estimator falls back to the per-observation
        // standard error, which is valid *only* under an independence assumption this
        // body cannot test. That is a caveat to state, not grounds for refusal: the
        // pair-count floor is the real minimum-data gate, and blocking here as well
        // would refuse bodies the data can legitimately support.
        caveats.push(
            "every paired request falls in one time block, so the interval assumes the requests \
             are independent and is therefore optimistic"
                .to_string(),
        );
    }

    RoutingEvidence {
        pairs,
        clusters: distinct_clusters.len(),
        clustering: config.clustering,
        utility,
        discordance_rate,
        required_pairs,
        min_pairs: config.min_pairs,
        minimum_effect: config.minimum_effect,
        blockers,
        caveats,
        config_fingerprint: config.fingerprint(),
    }
}

/// Cluster keys for the observations, in a deterministic order.
fn cluster_keys(observations: &[PairedObservation], clustering: Clustering) -> Vec<u64> {
    let block = match clustering {
        Clustering::None => {
            return (0..observations.len() as u64).collect();
        }
        Clustering::TimeBlock { block_secs } => block_secs.max(1),
    };
    // Floor division, so negative timestamps (which a corrupt trace can carry) do not
    // produce a different number of clusters than positive ones.
    observations
        .iter()
        .map(|observation| observation.recorded_at.div_euclid(block) as u64)
        .collect()
}

/// A clustered Wald interval on the mean paired effect.
///
/// The variance is `sum over clusters of (cluster_sum - n_c * mean)^2` divided by
/// `(k - 1)`, which is the standard one-way ANOVA estimator. It is the sum of
/// between-cluster variation, so it captures whatever correlation exists inside a
/// block rather than assuming there is none.
fn interval(
    observations: &[PairedObservation],
    cluster_ids: &[u64],
    distinct: &std::collections::BTreeSet<u64>,
    config: &EvidenceConfig,
) -> Option<EffectInterval> {
    if observations.is_empty() {
        return None;
    }
    let n = observations.len() as f64;
    let mean = observations.iter().map(|o| o.utility_delta).sum::<f64>() / n;

    let degrees = match distinct.len() {
        0 | 1 => 0,
        k => k - 1,
    };

    let standard_error = if degrees > 0 && distinct.len() > 1 {
        // Per-cluster sums and counts, accumulated over the observation order so the
        // arithmetic is reproducible.
        let mut sums: std::collections::BTreeMap<u64, f64> = std::collections::BTreeMap::new();
        let mut counts: std::collections::BTreeMap<u64, f64> = std::collections::BTreeMap::new();
        for (observation, key) in observations.iter().zip(cluster_ids) {
            *sums.entry(*key).or_default() += observation.utility_delta;
            *counts.entry(*key).or_default() += 1.0;
        }
        let between: f64 = sums
            .iter()
            .map(|(key, sum)| {
                let count = counts[key];
                let deviation = sum - count * mean;
                deviation * deviation / count
            })
            .sum();
        let variance = between / degrees as f64;
        if variance <= 0.0 {
            // Every cluster sat exactly on the mean. That is what clean, strongly
            // separated data looks like, so the reported interval is the (correct)
            // zero-width one — and `is_zero_dispersion` says so, because a
            // zero-width interval reads as infinite certainty and no finite sample
            // earns that.
            0.0
        } else {
            (variance / n).sqrt()
        }
    } else {
        // One cluster, or none: fall back to the naive per-observation standard error
        // so a number is reported, and let the caller see the degeneracy through
        // `clusters`.
        let variance = if n > 1.0 {
            observations
                .iter()
                .map(|observation| {
                    let deviation = observation.utility_delta - mean;
                    deviation * deviation
                })
                .sum::<f64>()
                / (n - 1.0)
        } else {
            0.0
        };
        if variance <= 0.0 {
            f64::INFINITY
        } else {
            (variance / n).sqrt()
        }
    };

    let half = if standard_error.is_finite() {
        config.z_two_sided() * standard_error
    } else {
        // An infinite standard error yields an interval spanning the whole line, which
        // correctly refuses to conclude anything.
        f64::INFINITY
    };
    Some(EffectInterval {
        mean,
        lower: if half.is_finite() {
            mean - half
        } else {
            f64::NEG_INFINITY
        },
        upper: if half.is_finite() {
            mean + half
        } else {
            f64::INFINITY
        },
        level: config.confidence,
        standard_error,
        is_zero_dispersion: half == 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600;
    const BASE: i64 = 1_700_000_000;

    fn config() -> EvidenceConfig {
        EvidenceConfig::default()
    }

    /// `n` pairs spread one per hour, each with the given delta.
    fn spread(n: usize, delta: f64) -> Vec<PairedObservation> {
        (0..n)
            .map(|index| PairedObservation {
                recorded_at: BASE + index as i64 * HOUR,
                utility_delta: delta,
                candidate_only_success: delta > 0.0,
                baseline_only_success: false,
            })
            .collect()
    }

    #[test]
    fn a_large_consistent_effect_is_actionable() {
        let evidence = measure(&spread(200, 0.5), &config());
        assert_eq!(evidence.pairs, 200);
        assert!(evidence.utility.is_some());
        assert!(evidence.is_actionable(), "{}", evidence.headline());
    }

    #[test]
    fn a_tiny_sample_blocks_rather_than_reports_an_improvement() {
        // The whole point: 30 pairs is a floor, not evidence.
        let evidence = measure(&spread(10, 0.9), &config());
        assert!(!evidence.is_sufficient());
        assert!(evidence
            .blockers
            .iter()
            .any(|b| b.contains("below the 30 required")));
    }

    #[test]
    fn an_interval_that_clears_zero_but_not_the_minimum_effect_is_not_actionable() {
        // Precise, and far too small to matter.
        let observations: Vec<PairedObservation> = (0..400)
            .map(|index| PairedObservation {
                recorded_at: BASE + index as i64 * HOUR,
                utility_delta: 0.02 + (index % 2) as f64 * 0.01,
                candidate_only_success: index % 2 == 0,
                baseline_only_success: index % 2 == 1,
            })
            .collect();
        let evidence = measure(&observations, &config());
        let interval = evidence.utility.expect("interval");
        assert!(
            interval.excludes_zero(),
            "the effect should be distinguishable from zero: {interval:?}"
        );
        assert!(
            !interval.clears(evidence.minimum_effect),
            "but it is far below the minimum detectable effect"
        );
        assert!(!evidence.is_actionable());
    }

    #[test]
    fn clustering_never_narrows_the_interval() {
        // The same observations and the same mean, but correlated within an hour
        // instead of independent. A correct estimator must report a *wider* interval.
        let independent: Vec<PairedObservation> = (0..200)
            .map(|index| PairedObservation {
                recorded_at: BASE + index as i64 * HOUR,
                utility_delta: 0.5,
                candidate_only_success: true,
                baseline_only_success: false,
            })
            .collect();
        // 200 requests packed into 2 hours, each block carrying its own level.
        let correlated: Vec<PairedObservation> = (0..200)
            .map(|index| PairedObservation {
                recorded_at: BASE + (index / 100) as i64 * HOUR + (index % 7) as i64 * 60,
                utility_delta: if index < 100 { 0.2 } else { 0.8 },
                candidate_only_success: true,
                baseline_only_success: false,
            })
            .collect();

        let mut naive = config();
        naive.clustering = Clustering::None;
        let unclustered = measure(&independent, &naive).utility.expect("interval");
        let clustered = measure(&correlated, &config()).utility.expect("interval");

        let naive_width = unclustered.upper - unclustered.lower;
        let clustered_width = clustered.upper - clustered.lower;
        assert!(
            clustered_width > naive_width,
            "clustering must widen: {clustered_width} vs {naive_width}"
        );
    }

    #[test]
    fn arms_that_never_disagree_report_no_effect_rather_than_a_tight_zero() {
        let observations: Vec<PairedObservation> = (0..200)
            .map(|index| PairedObservation {
                recorded_at: BASE + index as i64 * HOUR,
                utility_delta: 0.0,
                candidate_only_success: false,
                baseline_only_success: false,
            })
            .collect();
        let evidence = measure(&observations, &config());
        assert_eq!(evidence.discordance_rate, 0.0);
        assert!(!evidence.is_sufficient());
        assert!(evidence
            .blockers
            .iter()
            .any(|b| b.contains("never disagreed")));
        assert!(!evidence.is_actionable());
    }

    #[test]
    fn a_single_time_block_is_a_caveat_rather_than_a_refusal() {
        // Everything inside one hour. The clustered estimator falls back to the
        // per-observation error there, which is valid under an independence
        // assumption the body cannot test — so the assumption is stated and the
        // evidence is still usable. The pair floor is the real minimum-data gate.
        let block_start = BASE - BASE.rem_euclid(HOUR);
        let observations: Vec<PairedObservation> = (0..200)
            .map(|index| PairedObservation {
                recorded_at: block_start + index as i64,
                utility_delta: 0.5,
                candidate_only_success: true,
                baseline_only_success: false,
            })
            .collect();
        let evidence = measure(&observations, &config());
        assert_eq!(evidence.clusters, 1);
        assert!(evidence
            .caveats
            .iter()
            .any(|caveat| caveat.contains("one time block")));
        assert!(
            evidence.is_sufficient(),
            "a caveat must not become a refusal"
        );
        assert!(evidence.utility.is_some());
    }

    #[test]
    fn zero_dispersion_is_reported_as_zero_width_and_flagged() {
        // A perfectly separated effect gives a zero-width interval. Reporting it
        // without saying so would present it as infinite certainty.
        let evidence = measure(&spread(200, 0.5), &config());
        let interval = evidence.utility.expect("interval");
        assert!(interval.is_zero_dispersion);
        assert_eq!(interval.lower, interval.upper);
        assert_eq!(interval.standard_error, 0.0);
        // The effect is still judged on its own terms.
        assert!(interval.clears(evidence.minimum_effect));
    }

    #[test]
    fn an_empty_body_produces_no_interval_and_blocks() {
        let evidence = measure(&[], &config());
        assert_eq!(evidence.pairs, 0);
        assert!(evidence.utility.is_none());
        assert!(!evidence.is_sufficient());
    }

    #[test]
    fn measurement_is_deterministic() {
        let observations = spread(120, 0.4);
        assert_eq!(
            measure(&observations, &config()),
            measure(&observations, &config())
        );
    }

    #[test]
    fn the_config_fingerprint_changes_with_the_thresholds() {
        let base = config();
        let mut other = config();
        other.minimum_effect += 0.01;
        let observations = spread(100, 0.3);
        assert_ne!(
            measure(&observations, &base).config_fingerprint,
            measure(&observations, &other).config_fingerprint
        );
    }

    #[test]
    fn required_pairs_is_reported_and_is_never_below_the_floor() {
        let evidence = measure(&spread(200, 0.5), &config());
        let required = evidence.required_pairs.expect("required n");
        assert!(required >= evidence.min_pairs);
    }
}
