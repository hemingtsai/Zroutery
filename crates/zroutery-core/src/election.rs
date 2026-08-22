//! Choosing a class's primary model from latency and price together.
//!
//! Priority ordering asks the user to rank models by hand. That is fine when the
//! ranking is obvious and tedious when it is not: which of three sonnet-class
//! models is the best default depends on what they charge and how fast they
//! answer today, and neither is knowable from the configuration.
//!
//! So an *election* measures both and pins an order. It runs when asked, not per
//! request: a probe costs a request, and a routing decision that changes under
//! load is impossible to reason about. The scoring is a pure function of the
//! measurements so it can be tested without touching a network.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::billing::{Cost, Pricing};
use crate::config::ModelClass;
use crate::ir::Usage;

fn default_price_weight() -> f64 {
    0.5
}
fn default_latency_weight() -> f64 {
    0.5
}
fn default_reference_input() -> u32 {
    1_000
}
fn default_reference_output() -> u32 {
    500
}

/// How the two axes are weighted, and the request they are priced against.
///
/// Prices are per million tokens, so comparing them needs a request shape to
/// price: a model that is cheap on input and dear on output ranks differently for
/// a summariser than for a chat. The default leans on input, which is what most
/// prompts look like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoringConfig {
    #[serde(default = "default_price_weight")]
    pub price_weight: f64,
    #[serde(default = "default_latency_weight")]
    pub latency_weight: f64,
    #[serde(default = "default_reference_input")]
    pub reference_input_tokens: u32,
    #[serde(default = "default_reference_output")]
    pub reference_output_tokens: u32,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        ScoringConfig {
            price_weight: default_price_weight(),
            latency_weight: default_latency_weight(),
            reference_input_tokens: default_reference_input(),
            reference_output_tokens: default_reference_output(),
        }
    }
}

impl ScoringConfig {
    /// The usage the reference request would report.
    pub fn reference_usage(&self) -> Usage {
        Usage {
            input_tokens: self.reference_input_tokens,
            output_tokens: self.reference_output_tokens,
            ..Usage::default()
        }
    }

    /// What one reference request costs at this price.
    pub fn reference_cost(&self, pricing: &Pricing) -> Cost {
        pricing.cost_of(&self.reference_usage())
    }

    /// Weights normalised to sum to 1, so the score stays comparable when a user
    /// types 3 and 1 rather than 0.75 and 0.25. Both at zero means neither axis
    /// was chosen, which is treated as an even split rather than a divide by zero.
    pub fn normalised_weights(&self) -> (f64, f64) {
        let price = self.price_weight.max(0.0);
        let latency = self.latency_weight.max(0.0);
        let total = price + latency;
        if total <= 0.0 || !total.is_finite() {
            return (0.5, 0.5);
        }
        (price / total, latency / total)
    }

    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (name, value) in [
            ("price weight", self.price_weight),
            ("latency weight", self.latency_weight),
        ] {
            if value < 0.0 || !value.is_finite() {
                out.push(format!("has an impossible {name}"));
            }
        }
        if self.reference_input_tokens == 0 && self.reference_output_tokens == 0 {
            out.push("prices every model against an empty request".to_string());
        }
        out
    }
}

/// One probe result: how a model answered, and what it charges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub model_id: String,
    /// Round trip of the probe. `None` when the probe failed.
    pub latency_ms: Option<u64>,
    /// Why the probe failed, when it did.
    pub error: Option<String>,
    /// Cost of the reference request, when the model has a price.
    pub price: Option<Cost>,
    /// Manual priority, used only to break exact ties.
    pub priority: i32,
}

impl Measurement {
    pub fn new(model_id: impl Into<String>) -> Self {
        Measurement {
            model_id: model_id.into(),
            latency_ms: None,
            error: None,
            price: None,
            priority: 0,
        }
    }

    pub fn answered(mut self, latency_ms: u64) -> Self {
        self.latency_ms = Some(latency_ms);
        self
    }

    pub fn failed(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub fn priced(mut self, currency: &str, amount: f64) -> Self {
        self.price = Some(Cost {
            currency: currency.to_string(),
            amount,
        });
        self
    }

    fn is_available(&self) -> bool {
        self.latency_ms.is_some()
    }
}

/// A model's place in its class, with the numbers that put it there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranked {
    pub model_id: String,
    /// Lower is better. `None` when the model did not answer the probe.
    pub score: Option<f64>,
    pub latency_ms: Option<u64>,
    pub price: Option<Cost>,
    /// Why this one is where it is, in words.
    pub note: Option<String>,
}

/// The outcome for one class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassElection {
    pub class: ModelClass,
    /// Best first. Models that failed their probe are last and unscored.
    pub ranked: Vec<Ranked>,
    /// Whether price took part in the scoring.
    pub priced: bool,
    /// Set when something kept price out of it, or when nothing answered.
    pub note: Option<String>,
}

impl ClassElection {
    /// The order the router should try, best first.
    pub fn order(&self) -> Vec<&str> {
        self.ranked.iter().map(|r| r.model_id.as_str()).collect()
    }

    pub fn winner(&self) -> Option<&str> {
        self.ranked.first().map(|r| r.model_id.as_str())
    }
}

/// Every class's outcome, from one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Election {
    pub decided_at: DateTime<Utc>,
    pub scoring: ScoringConfig,
    pub classes: BTreeMap<ModelClass, ClassElection>,
}

impl Election {
    pub fn new(scoring: ScoringConfig) -> Self {
        Election {
            decided_at: Utc::now(),
            scoring,
            classes: BTreeMap::new(),
        }
    }

    pub fn order_for(&self, class: ModelClass) -> Option<Vec<&str>> {
        self.classes.get(&class).map(|c| c.order())
    }
}

/// Rank one class's measurements. Pure: same input, same order out.
///
/// Price only takes part when every model that answered has a price and they all
/// bill in one currency, because there is no honest way to compare 2 CNY with
/// 3 USD or to guess what an unpriced model costs. When price is out, the ranking
/// is latency alone and the reason is recorded rather than hidden.
pub fn rank(
    class: ModelClass,
    measurements: &[Measurement],
    scoring: &ScoringConfig,
) -> ClassElection {
    let (mut available, unavailable): (Vec<&Measurement>, Vec<&Measurement>) =
        measurements.iter().partition(|m| m.is_available());

    if available.is_empty() {
        return ClassElection {
            class,
            ranked: unavailable.iter().map(|m| unavailable_entry(m)).collect(),
            priced: false,
            note: Some("nothing in this class answered the probe".into()),
        };
    }

    // One currency, and a price for everyone, or no price scoring at all.
    let currencies: Vec<&str> = available
        .iter()
        .filter_map(|m| m.price.as_ref().map(|p| p.currency.as_str()))
        .collect();
    let priced_all = currencies.len() == available.len();
    let one_currency = currencies.windows(2).all(|w| w[0] == w[1]);
    let priced = priced_all && one_currency;
    let note = if priced {
        None
    } else if !priced_all {
        Some("scored on latency alone: not every model has a price".to_string())
    } else {
        Some(format!(
            "scored on latency alone: prices are in {} different currencies",
            {
                let mut seen: Vec<&str> = currencies.clone();
                seen.sort_unstable();
                seen.dedup();
                seen.len()
            }
        ))
    };

    // Compare against the class average, which keeps the magnitude of a
    // difference: twice the price reads as twice as bad, and a free model does
    // not divide anything by zero.
    let mean_latency = mean(available.iter().map(|m| m.latency_ms.unwrap_or(0) as f64));
    let mean_price = if priced {
        mean(
            available
                .iter()
                .map(|m| m.price.as_ref().map_or(0.0, |p| p.amount)),
        )
    } else {
        0.0
    };
    let (price_weight, latency_weight) = scoring.normalised_weights();

    let score_of = |m: &Measurement| -> f64 {
        let latency_ratio = ratio(m.latency_ms.unwrap_or(0) as f64, mean_latency);
        if !priced {
            return latency_ratio;
        }
        let price_ratio = ratio(m.price.as_ref().map_or(0.0, |p| p.amount), mean_price);
        price_weight * price_ratio + latency_weight * latency_ratio
    };

    available.sort_by(|a, b| {
        score_of(a)
            .partial_cmp(&score_of(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            // Exact ties fall back to what the user asked for, then to the id, so
            // the outcome never depends on iteration order.
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.model_id.cmp(&b.model_id))
    });

    let mut ranked: Vec<Ranked> = available
        .iter()
        .enumerate()
        .map(|(place, m)| Ranked {
            model_id: m.model_id.clone(),
            score: Some(score_of(m)),
            latency_ms: m.latency_ms,
            price: m.price.clone(),
            note: Some(describe(place, m, priced)),
        })
        .collect();
    ranked.extend(unavailable.iter().map(|m| unavailable_entry(m)));

    ClassElection {
        class,
        ranked,
        priced,
        note,
    }
}

fn unavailable_entry(m: &Measurement) -> Ranked {
    Ranked {
        model_id: m.model_id.clone(),
        score: None,
        latency_ms: None,
        price: m.price.clone(),
        note: Some(match &m.error {
            Some(e) => format!("did not answer: {e}"),
            None => "did not answer".to_string(),
        }),
    }
}

fn describe(place: usize, m: &Measurement, priced: bool) -> String {
    let latency = m
        .latency_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "unmeasured".into());
    let price = match (&m.price, priced) {
        (Some(cost), true) => format!(
            ", {:.4} {} per reference request",
            cost.amount, cost.currency
        ),
        (Some(cost), false) => format!(", {:.4} {} (not scored)", cost.amount, cost.currency),
        (None, _) => String::new(),
    };
    let place = if place == 0 {
        "primary".to_string()
    } else {
        format!("fallback {place}")
    };
    format!("{place}: {latency}{price}")
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let mut count = 0usize;
    let mut total = 0.0;
    for v in values {
        total += v;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    total / count as f64
}

/// `value` relative to `mean`, with 1.0 when there is nothing to compare.
fn ratio(value: f64, mean: f64) -> f64 {
    if mean <= 0.0 || !mean.is_finite() {
        1.0
    } else {
        value / mean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoring(price_weight: f64, latency_weight: f64) -> ScoringConfig {
        ScoringConfig {
            price_weight,
            latency_weight,
            ..ScoringConfig::default()
        }
    }

    #[test]
    fn the_reference_request_prices_input_and_output() {
        let scoring = ScoringConfig::default();
        let cost = scoring.reference_cost(&Pricing::new("USD", 3.0, 15.0));
        // 1000 in at 3/Mtok plus 500 out at 15/Mtok
        assert!((cost.amount - (0.003 + 0.0075)).abs() < 1e-9);
        assert_eq!(cost.currency, "USD");
    }

    #[test]
    fn weights_are_normalised_and_survive_nonsense() {
        assert_eq!(scoring(3.0, 1.0).normalised_weights(), (0.75, 0.25));
        assert_eq!(scoring(0.0, 0.0).normalised_weights(), (0.5, 0.5));
        assert_eq!(scoring(-1.0, 1.0).normalised_weights(), (0.0, 1.0));
        let (p, l) = scoring(f64::NAN, 1.0).normalised_weights();
        assert!(p.is_finite() && l.is_finite());
    }

    #[test]
    fn cheap_and_fast_wins_on_both_axes() {
        let measurements = vec![
            Measurement::new("slow-dear")
                .answered(2000)
                .priced("USD", 0.10),
            Measurement::new("fast-cheap")
                .answered(500)
                .priced("USD", 0.01),
        ];
        let election = rank(ModelClass::Sonnet, &measurements, &ScoringConfig::default());
        assert!(election.priced);
        assert_eq!(election.winner(), Some("fast-cheap"));
        assert_eq!(election.order(), vec!["fast-cheap", "slow-dear"]);
        assert!(election.note.is_none());
    }

    #[test]
    fn the_weights_decide_when_the_axes_disagree() {
        // Twice as fast, ten times the price.
        let measurements = vec![
            Measurement::new("fast").answered(500).priced("USD", 0.10),
            Measurement::new("cheap").answered(1000).priced("USD", 0.01),
        ];

        let by_price = rank(ModelClass::Sonnet, &measurements, &scoring(1.0, 0.0));
        assert_eq!(by_price.winner(), Some("cheap"));

        let by_latency = rank(ModelClass::Sonnet, &measurements, &scoring(0.0, 1.0));
        assert_eq!(by_latency.winner(), Some("fast"));

        // Evenly weighted, the tenfold price gap outweighs the twofold latency gap.
        let even = rank(ModelClass::Sonnet, &measurements, &scoring(0.5, 0.5));
        assert_eq!(even.winner(), Some("cheap"));
    }

    #[test]
    fn magnitude_matters_not_just_order() {
        // A barely cheaper model should not beat a much faster one.
        let measurements = vec![
            Measurement::new("much-faster")
                .answered(200)
                .priced("USD", 0.0101),
            Measurement::new("barely-cheaper")
                .answered(2000)
                .priced("USD", 0.0100),
        ];
        let election = rank(ModelClass::Opus, &measurements, &ScoringConfig::default());
        assert_eq!(election.winner(), Some("much-faster"));
    }

    #[test]
    fn a_free_model_does_not_divide_by_zero() {
        let measurements = vec![
            Measurement::new("free").answered(900).priced("USD", 0.0),
            Measurement::new("paid").answered(800).priced("USD", 0.05),
        ];
        let election = rank(ModelClass::Haiku, &measurements, &ScoringConfig::default());
        assert!(election.ranked.iter().all(|r| r.score.unwrap().is_finite()));
        assert_eq!(election.winner(), Some("free"));

        // All free: price stops being a differentiator and latency decides.
        let all_free = vec![
            Measurement::new("slow").answered(900).priced("USD", 0.0),
            Measurement::new("quick").answered(100).priced("USD", 0.0),
        ];
        let election = rank(ModelClass::Haiku, &all_free, &ScoringConfig::default());
        assert_eq!(election.winner(), Some("quick"));
    }

    #[test]
    fn an_unpriced_model_takes_price_out_of_the_scoring() {
        let measurements = vec![
            Measurement::new("priced")
                .answered(2000)
                .priced("USD", 0.01),
            Measurement::new("unpriced").answered(300),
        ];
        let election = rank(ModelClass::Sonnet, &measurements, &scoring(1.0, 0.0));
        assert!(!election.priced);
        // Price was asked for and could not be used, so latency decided instead.
        assert_eq!(election.winner(), Some("unpriced"));
        assert!(election
            .note
            .unwrap()
            .contains("not every model has a price"));
    }

    #[test]
    fn mixed_currencies_take_price_out_of_the_scoring() {
        let measurements = vec![
            Measurement::new("in-usd")
                .answered(2000)
                .priced("USD", 0.01),
            Measurement::new("in-cny").answered(300).priced("CNY", 0.05),
        ];
        let election = rank(ModelClass::Sonnet, &measurements, &ScoringConfig::default());
        assert!(!election.priced);
        assert_eq!(election.winner(), Some("in-cny"), "latency alone decides");
        let note = election.note.unwrap();
        assert!(note.contains("2 different currencies"), "{note}");
        // The amounts are still reported, just marked as unscored.
        assert!(election.ranked[0]
            .note
            .as_ref()
            .unwrap()
            .contains("not scored"));
    }

    #[test]
    fn a_model_that_did_not_answer_goes_last_and_unscored() {
        let measurements = vec![
            Measurement::new("broken").failed("502 from upstream"),
            Measurement::new("works").answered(700).priced("USD", 0.02),
        ];
        let election = rank(ModelClass::Opus, &measurements, &ScoringConfig::default());
        assert_eq!(election.order(), vec!["works", "broken"]);
        let last = election.ranked.last().unwrap();
        assert!(last.score.is_none());
        assert!(last.note.as_ref().unwrap().contains("502 from upstream"));
    }

    #[test]
    fn a_class_where_nothing_answers_says_so() {
        let measurements = vec![
            Measurement::new("a").failed("timeout"),
            Measurement::new("b").failed("401"),
        ];
        let election = rank(ModelClass::Haiku, &measurements, &ScoringConfig::default());
        assert!(election.winner().is_some(), "the order is still reported");
        assert!(election.ranked.iter().all(|r| r.score.is_none()));
        assert!(election
            .note
            .unwrap()
            .contains("nothing in this class answered"));
    }

    #[test]
    fn exact_ties_fall_back_to_priority_then_id() {
        let mut first = Measurement::new("b-model")
            .answered(500)
            .priced("USD", 0.01);
        first.priority = 10;
        let mut second = Measurement::new("a-model")
            .answered(500)
            .priced("USD", 0.01);
        second.priority = 0;
        let election = rank(
            ModelClass::Sonnet,
            &[first.clone(), second.clone()],
            &ScoringConfig::default(),
        );
        // Same score, so the lower priority number wins.
        assert_eq!(election.order(), vec!["a-model", "b-model"]);

        // Same score and same priority: the id decides, so runs agree.
        let mut third = second.clone();
        third.priority = 10;
        let election = rank(
            ModelClass::Sonnet,
            &[first, third],
            &ScoringConfig::default(),
        );
        assert_eq!(election.order(), vec!["a-model", "b-model"]);
    }

    #[test]
    fn one_candidate_is_simply_the_primary() {
        let election = rank(
            ModelClass::Opus,
            &[Measurement::new("only").answered(1234).priced("USD", 0.3)],
            &ScoringConfig::default(),
        );
        assert_eq!(election.winner(), Some("only"));
        assert!(election.ranked[0]
            .note
            .as_ref()
            .unwrap()
            .contains("primary"));
        assert!(election.ranked[0]
            .note
            .as_ref()
            .unwrap()
            .contains("1234 ms"));
    }

    #[test]
    fn an_empty_class_is_not_an_election() {
        let election = rank(ModelClass::Opus, &[], &ScoringConfig::default());
        assert!(election.ranked.is_empty());
        assert!(election.winner().is_none());
        assert!(election.note.is_some());
    }

    #[test]
    fn nonsense_scoring_settings_are_reported() {
        assert!(ScoringConfig::default().problems().is_empty());
        assert!(scoring(-1.0, 1.0).problems()[0].contains("price weight"));
        let empty = ScoringConfig {
            reference_input_tokens: 0,
            reference_output_tokens: 0,
            ..ScoringConfig::default()
        };
        assert!(empty.problems()[0].contains("empty request"));
    }

    #[test]
    fn an_election_round_trips_through_json() {
        let election = Election {
            decided_at: Utc::now(),
            scoring: ScoringConfig::default(),
            classes: BTreeMap::from([(
                ModelClass::Sonnet,
                rank(
                    ModelClass::Sonnet,
                    &[Measurement::new("m").answered(1).priced("USD", 0.1)],
                    &ScoringConfig::default(),
                ),
            )]),
        };
        let json = serde_json::to_string(&election).unwrap();
        let back: Election = serde_json::from_str(&json).unwrap();
        assert_eq!(back, election);
        assert_eq!(back.order_for(ModelClass::Sonnet).unwrap(), vec!["m"]);
        assert!(back.order_for(ModelClass::Opus).is_none());
    }
}
