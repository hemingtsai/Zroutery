//! Spending limits, and the ledger that makes them mean something across restarts.
//!
//! A budget answers "stop when I have spent this much". Three things about that are
//! worth stating up front, because they shape the whole design:
//!
//! * The cost of a request is only known once it has finished, so a budget is a
//!   line-crossing detector, not a pre-authorisation. The request that crosses the
//!   line completes; the next one is stopped. Overshoot is bounded by one request
//!   per class, and pretending otherwise would need a token estimate the providers
//!   do not agree on.
//! * A guardrail that forgets is not a guardrail, so the ledger is persisted. The
//!   request log is deliberately in memory only, which makes it the wrong place to
//!   count money.
//! * Currencies are never converted. A budget in USD counts USD spend and nothing
//!   else; a provider that bills in something else is a configuration mistake that
//!   `validate` reports rather than a rounding decision taken here.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Local};
use serde::{Deserialize, Serialize};

use crate::billing::Cost;
use crate::config::ModelClass;

/// What a budget covers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetScope {
    /// Everything the proxy spends.
    Global,
    /// One provider, across all of its models.
    Provider { id: String },
    /// Requests routed to one class, wherever they landed.
    Class { class: ModelClass },
}

impl BudgetScope {
    /// Stable key for the ledger. Prefixed so a provider called `global` cannot
    /// collide with the global scope.
    fn key(&self) -> String {
        match self {
            BudgetScope::Global => "global".into(),
            BudgetScope::Provider { id } => format!("provider:{id}"),
            BudgetScope::Class { class } => format!("class:{}", class.as_str()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            BudgetScope::Global => "everything".into(),
            BudgetScope::Provider { id } => format!("provider {id}"),
            BudgetScope::Class { class } => format!("{}-class", class.as_str()),
        }
    }
}

/// How often a budget starts over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    /// Since local midnight. Local rather than UTC because "today" is what the
    /// person setting the limit means, even though providers reset on their own
    /// schedules.
    Day,
    /// Since the first of the local month.
    Month,
}

impl BudgetPeriod {
    pub const ALL: [BudgetPeriod; 2] = [BudgetPeriod::Day, BudgetPeriod::Month];

    /// The bucket `at` falls into, e.g. `2026-08-22` or `2026-08`.
    pub fn key(&self, at: DateTime<Local>) -> String {
        match self {
            BudgetPeriod::Day => at.format("%Y-%m-%d").to_string(),
            BudgetPeriod::Month => at.format("%Y-%m").to_string(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            BudgetPeriod::Day => "today",
            BudgetPeriod::Month => "this month",
        }
    }
}

/// What to do once the limit is reached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OnExceeded {
    /// Refuse the request and say why.
    #[default]
    Reject,
    /// Serve it from a cheaper class instead. The cheaper class's own budget still
    /// applies, so this cannot be used to route around a limit.
    Degrade { to: ModelClass },
}

/// One limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// Stable id used by the dashboard to edit a specific budget even when
    /// another budget is added or removed in a queued save.
    #[serde(default)]
    pub id: String,
    pub scope: BudgetScope,
    pub period: BudgetPeriod,
    /// The amount, in the currency it is counted in.
    pub limit: Cost,
    #[serde(default)]
    pub on_exceeded: OnExceeded,
    #[serde(default = "crate::budget::yes")]
    pub enabled: bool,
}

pub(crate) fn yes() -> bool {
    true
}

/// Six decimal places is far below the smallest billed unit of any provider and
/// still exact enough that repeated addition cannot drift visibly.
fn round_cents(amount: f64) -> f64 {
    (amount * 1_000_000.0).round() / 1_000_000.0
}

impl Budget {
    pub fn new(scope: BudgetScope, period: BudgetPeriod, currency: &str, amount: f64) -> Self {
        Budget {
            id: String::new(),
            scope,
            period,
            limit: Cost {
                currency: currency.to_string(),
                amount,
            },
            on_exceeded: OnExceeded::Reject,
            enabled: true,
        }
    }

    pub fn rejecting(mut self) -> Self {
        self.on_exceeded = OnExceeded::Reject;
        self
    }

    pub fn degrading_to(mut self, class: ModelClass) -> Self {
        self.on_exceeded = OnExceeded::Degrade { to: class };
        self
    }

    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.limit.currency.trim().is_empty() {
            out.push("has no currency".into());
        }
        if self.limit.amount <= 0.0 || !self.limit.amount.is_finite() {
            out.push("has an impossible limit".into());
        }
        if let (BudgetScope::Class { class }, OnExceeded::Degrade { to }) =
            (&self.scope, &self.on_exceeded)
        {
            if class == to {
                out.push("degrades to the class it limits, which would loop".into());
            }
        }
        out
    }

    fn ledger_key(&self, at: DateTime<Local>) -> LedgerKey {
        LedgerKey {
            scope: self.scope.key(),
            period: self.period.key(at),
            currency: self.limit.currency.clone(),
        }
    }
}

/// Where one number of spend lives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct LedgerKey {
    scope: String,
    period: String,
    currency: String,
}

/// Spend so far, by scope, period and currency.
///
/// Every request writes six entries: a day and a month bucket for the global scope,
/// its provider and its class. That is more rows than a single accumulator would
/// need, and it means a daily and a monthly limit on the same scope both work
/// without either having to reconstruct the other.
///
/// Amounts are `f64` and are rounded to six decimal places as they accumulate:
/// costs arrive as small floats and unbounded addition would drift away from the
/// decimal amounts the pricing tables mean, which matters for a number that is
/// compared against a limit someone typed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    entries: BTreeMap<String, f64>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    fn flat(key: &LedgerKey) -> String {
        format!("{}|{}|{}", key.scope, key.period, key.currency)
    }

    /// Record what a finished request cost.
    pub fn charge(
        &mut self,
        at: DateTime<Local>,
        provider_id: &str,
        class: Option<ModelClass>,
        cost: &Cost,
    ) {
        let mut scopes = vec![
            BudgetScope::Global,
            BudgetScope::Provider {
                id: provider_id.to_string(),
            },
        ];
        if let Some(class) = class {
            scopes.push(BudgetScope::Class { class });
        }
        for scope in scopes {
            for period in BudgetPeriod::ALL {
                let key = LedgerKey {
                    scope: scope.key(),
                    period: period.key(at),
                    currency: cost.currency.clone(),
                };
                let entry = self.entries.entry(Self::flat(&key)).or_insert(0.0);
                *entry = round_cents(*entry + cost.amount);
            }
        }
    }

    /// Spend counted against one budget in its current period.
    pub fn spent(&self, budget: &Budget, at: DateTime<Local>) -> f64 {
        self.entries
            .get(&Self::flat(&budget.ledger_key(at)))
            .copied()
            .unwrap_or(0.0)
    }

    /// Drop buckets old enough that nothing can read them again, so the file stays
    /// a fixed size instead of growing for the life of the install.
    pub fn prune(&mut self, at: DateTime<Local>) {
        let day_cutoff = (at - chrono::Duration::days(40))
            .format("%Y-%m-%d")
            .to_string();
        let month_cutoff = {
            let months = at.year() * 12 + at.month0() as i32 - 13;
            format!(
                "{:04}-{:02}",
                months.div_euclid(12),
                months.rem_euclid(12) + 1
            )
        };
        self.entries.retain(|key, _| {
            let Some(period) = key.split('|').nth(1) else {
                return false;
            };
            // A day bucket has two dashes, a month bucket one.
            if period.matches('-').count() == 2 {
                period >= day_cutoff.as_str()
            } else {
                period >= month_cutoff.as_str()
            }
        });
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Everything counted for one scope in the current periods, for display.
    pub fn totals_for(
        &self,
        scope: &BudgetScope,
        at: DateTime<Local>,
    ) -> Vec<(BudgetPeriod, Cost)> {
        let prefix = scope.key();
        let mut out = Vec::new();
        for period in BudgetPeriod::ALL {
            let wanted = format!("{prefix}|{}|", period.key(at));
            for (key, amount) in &self.entries {
                if let Some(currency) = key.strip_prefix(&wanted) {
                    out.push((
                        period,
                        Cost {
                            currency: currency.to_string(),
                            amount: *amount,
                        },
                    ));
                }
            }
        }
        out
    }
}

/// What the budgets say about a request that is about to be routed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Verdict {
    /// Nothing is in the way.
    Allow,
    /// Serve it from a cheaper class.
    Degrade {
        to: ModelClass,
        /// The budget that caused it, for the log and the response header.
        because: String,
    },
    /// Refuse, and say which limit was hit.
    Reject { because: String },
}

/// Check a request against every budget that covers it.
///
/// A rejection outranks a degrade: if one budget says stop and another says use the
/// cheap tier, stopping is the safer reading of the user's intent.
pub fn check(
    budgets: &[Budget],
    ledger: &Ledger,
    at: DateTime<Local>,
    provider_ids: &[String],
    class: Option<ModelClass>,
) -> Verdict {
    let mut degrade: Option<Verdict> = None;

    for budget in budgets.iter().filter(|b| b.enabled) {
        let covers = match &budget.scope {
            BudgetScope::Global => true,
            BudgetScope::Provider { id } => provider_ids.iter().any(|p| p == id),
            BudgetScope::Class { class: scoped } => class == Some(*scoped),
        };
        if !covers || ledger.spent(budget, at) < budget.limit.amount {
            continue;
        }

        let because = format!(
            "the {} limit for {} ({:.2} {}) is used up",
            budget.period.label(),
            budget.scope.label(),
            budget.limit.amount,
            budget.limit.currency
        );
        match &budget.on_exceeded {
            OnExceeded::Reject => return Verdict::Reject { because },
            OnExceeded::Degrade { to } => {
                degrade.get_or_insert(Verdict::Degrade { to: *to, because });
            }
        }
    }

    degrade.unwrap_or(Verdict::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(text: &str) -> DateTime<Local> {
        let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).unwrap()
    }

    fn usd(amount: f64) -> Cost {
        Cost {
            currency: "USD".into(),
            amount,
        }
    }

    fn daily_global(limit: f64) -> Budget {
        Budget::new(BudgetScope::Global, BudgetPeriod::Day, "USD", limit)
    }

    #[test]
    fn spend_accumulates_into_the_day_and_the_month() {
        let mut ledger = Ledger::new();
        ledger.charge(
            at("2026-08-22 10:00:00"),
            "deepseek",
            Some(ModelClass::Sonnet),
            &usd(1.5),
        );
        ledger.charge(
            at("2026-08-23 10:00:00"),
            "deepseek",
            Some(ModelClass::Sonnet),
            &usd(2.0),
        );

        let daily = daily_global(100.0);
        let monthly = Budget::new(BudgetScope::Global, BudgetPeriod::Month, "USD", 100.0);
        // Each day counts on its own, the month counts both.
        assert_eq!(ledger.spent(&daily, at("2026-08-22 23:59:00")), 1.5);
        assert_eq!(ledger.spent(&daily, at("2026-08-23 00:01:00")), 2.0);
        assert_eq!(ledger.spent(&monthly, at("2026-08-23 00:01:00")), 3.5);
        // And a new month starts from nothing.
        assert_eq!(ledger.spent(&monthly, at("2026-09-01 00:00:00")), 0.0);
    }

    #[test]
    fn a_scope_only_counts_what_belongs_to_it() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "deepseek", Some(ModelClass::Sonnet), &usd(1.0));
        ledger.charge(now, "openai", Some(ModelClass::Opus), &usd(4.0));

        let of = |scope: BudgetScope| {
            ledger.spent(&Budget::new(scope, BudgetPeriod::Day, "USD", 999.0), now)
        };
        assert_eq!(of(BudgetScope::Global), 5.0);
        assert_eq!(
            of(BudgetScope::Provider {
                id: "deepseek".into()
            }),
            1.0
        );
        assert_eq!(
            of(BudgetScope::Class {
                class: ModelClass::Opus
            }),
            4.0
        );
        assert_eq!(
            of(BudgetScope::Class {
                class: ModelClass::Haiku
            }),
            0.0
        );
    }

    #[test]
    fn a_provider_named_global_does_not_collide_with_the_global_scope() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "global", None, &usd(2.0));
        let global = ledger.spent(&daily_global(999.0), now);
        let provider = ledger.spent(
            &Budget::new(
                BudgetScope::Provider {
                    id: "global".into(),
                },
                BudgetPeriod::Day,
                "USD",
                999.0,
            ),
            now,
        );
        assert_eq!((global, provider), (2.0, 2.0));
        // Same number here, but from two different rows, which the flat keys prove.
        assert_eq!(ledger.entries.len(), 4);
    }

    #[test]
    fn currencies_are_counted_separately_and_never_added() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "deepseek", None, &usd(1.0));
        ledger.charge(
            now,
            "deepseek",
            None,
            &Cost {
                currency: "CNY".into(),
                amount: 50.0,
            },
        );
        assert_eq!(ledger.spent(&daily_global(999.0), now), 1.0);
        let cny = Budget::new(BudgetScope::Global, BudgetPeriod::Day, "CNY", 999.0);
        assert_eq!(ledger.spent(&cny, now), 50.0);
    }

    #[test]
    fn a_budget_allows_until_it_is_used_up() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        let budgets = vec![daily_global(10.0)];

        assert_eq!(check(&budgets, &ledger, now, &[], None), Verdict::Allow);
        ledger.charge(now, "p", None, &usd(9.99));
        assert_eq!(
            check(&budgets, &ledger, now, &[], None),
            Verdict::Allow,
            "under the limit is still under"
        );

        // The request that crosses the line completes; the next one is stopped.
        ledger.charge(now, "p", None, &usd(0.02));
        match check(&budgets, &ledger, now, &[], None) {
            Verdict::Reject { because } => {
                assert!(because.contains("today") && because.contains("10.00 USD"))
            }
            other => panic!("expected a rejection, got {other:?}"),
        }

        // Tomorrow it is clear again.
        assert_eq!(
            check(&budgets, &ledger, at("2026-08-23 00:00:01"), &[], None),
            Verdict::Allow
        );
    }

    #[test]
    fn a_disabled_budget_is_ignored() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "p", None, &usd(99.0));
        let mut budget = daily_global(1.0);
        budget.enabled = false;
        assert_eq!(check(&[budget], &ledger, now, &[], None), Verdict::Allow);
    }

    #[test]
    fn a_budget_only_stops_what_it_covers() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "openai", Some(ModelClass::Opus), &usd(5.0));
        let budgets = vec![Budget::new(
            BudgetScope::Provider {
                id: "openai".into(),
            },
            BudgetPeriod::Day,
            "USD",
            1.0,
        )];

        // A request that could land on openai is stopped.
        assert!(matches!(
            check(
                &budgets,
                &ledger,
                now,
                &["openai".into()],
                Some(ModelClass::Opus)
            ),
            Verdict::Reject { .. }
        ));
        // One that cannot is not.
        assert_eq!(
            check(
                &budgets,
                &ledger,
                now,
                &["deepseek".into()],
                Some(ModelClass::Opus)
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_class_budget_can_degrade_instead_of_refusing() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "openai", Some(ModelClass::Opus), &usd(20.0));
        let budgets = vec![Budget::new(
            BudgetScope::Class {
                class: ModelClass::Opus,
            },
            BudgetPeriod::Day,
            "USD",
            10.0,
        )
        .degrading_to(ModelClass::Sonnet)];

        match check(
            &budgets,
            &ledger,
            now,
            &["openai".into()],
            Some(ModelClass::Opus),
        ) {
            Verdict::Degrade { to, because } => {
                assert_eq!(to, ModelClass::Sonnet);
                assert!(because.contains("opus-class"), "{because}");
            }
            other => panic!("expected a degrade, got {other:?}"),
        }
        // Sonnet itself is untouched, so the degraded request goes through.
        assert_eq!(
            check(
                &budgets,
                &ledger,
                now,
                &["openai".into()],
                Some(ModelClass::Sonnet)
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_rejection_outranks_a_degrade() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "openai", Some(ModelClass::Opus), &usd(50.0));
        let budgets = vec![
            Budget::new(
                BudgetScope::Class {
                    class: ModelClass::Opus,
                },
                BudgetPeriod::Day,
                "USD",
                10.0,
            )
            .degrading_to(ModelClass::Haiku),
            daily_global(20.0),
        ];
        // One budget would settle for the cheap tier, the other says stop entirely.
        assert!(matches!(
            check(
                &budgets,
                &ledger,
                now,
                &["openai".into()],
                Some(ModelClass::Opus)
            ),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn impossible_budgets_are_reported() {
        assert!(daily_global(10.0).problems().is_empty());

        let mut zero = daily_global(0.0);
        assert!(zero.problems()[0].contains("impossible limit"));
        zero.limit.currency = "  ".into();
        assert_eq!(zero.problems().len(), 2);

        // Degrading a class to itself would spin.
        let looping = Budget::new(
            BudgetScope::Class {
                class: ModelClass::Sonnet,
            },
            BudgetPeriod::Day,
            "USD",
            5.0,
        )
        .degrading_to(ModelClass::Sonnet);
        assert!(looping.problems()[0].contains("would loop"));
    }

    #[test]
    fn old_buckets_are_pruned_and_current_ones_survive() {
        let mut ledger = Ledger::new();
        ledger.charge(at("2024-01-05 12:00:00"), "p", None, &usd(1.0));
        ledger.charge(at("2026-08-01 12:00:00"), "p", None, &usd(2.0));
        ledger.charge(at("2026-08-22 12:00:00"), "p", None, &usd(3.0));
        let before = ledger.entries.len();

        ledger.prune(at("2026-08-22 23:00:00"));
        assert!(ledger.entries.len() < before);

        // This month and today are still readable; the 2024 rows are gone.
        let now = at("2026-08-22 23:00:00");
        assert_eq!(ledger.spent(&daily_global(999.0), now), 3.0);
        assert_eq!(
            ledger.spent(
                &Budget::new(BudgetScope::Global, BudgetPeriod::Month, "USD", 999.0),
                now
            ),
            5.0
        );
        assert_eq!(
            ledger.spent(&daily_global(999.0), at("2024-01-05 12:00:00")),
            0.0
        );
    }

    #[test]
    fn the_ledger_round_trips_through_json() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(now, "deepseek", Some(ModelClass::Haiku), &usd(0.25));
        let json = serde_json::to_string(&ledger).unwrap();
        let back: Ledger = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ledger);
        assert_eq!(back.spent(&daily_global(1.0), now), 0.25);
    }

    #[test]
    fn repeated_charges_do_not_drift() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        // 0.1 is not representable in binary; a naive accumulator shows it.
        for _ in 0..10 {
            ledger.charge(now, "p", None, &usd(0.1));
        }
        assert_eq!(ledger.spent(&daily_global(999.0), now), 1.0);
    }

    #[test]
    fn totals_are_reported_per_period_for_display() {
        let mut ledger = Ledger::new();
        let now = at("2026-08-22 12:00:00");
        ledger.charge(at("2026-08-01 09:00:00"), "deepseek", None, &usd(2.0));
        ledger.charge(now, "deepseek", None, &usd(1.0));

        let totals = ledger.totals_for(
            &BudgetScope::Provider {
                id: "deepseek".into(),
            },
            now,
        );
        let day = totals
            .iter()
            .find(|(p, _)| *p == BudgetPeriod::Day)
            .unwrap();
        let month = totals
            .iter()
            .find(|(p, _)| *p == BudgetPeriod::Month)
            .unwrap();
        assert_eq!(day.1.amount, 1.0);
        assert_eq!(month.1.amount, 3.0);
    }

    #[test]
    fn scopes_and_actions_round_trip_through_json() {
        let budget = Budget::new(
            BudgetScope::Provider {
                id: "deepseek".into(),
            },
            BudgetPeriod::Month,
            "CNY",
            200.0,
        )
        .degrading_to(ModelClass::Haiku);
        let json = serde_json::to_string(&budget).unwrap();
        assert!(json.contains("\"kind\":\"provider\""));
        assert!(json.contains("\"action\":\"degrade\""));
        assert_eq!(serde_json::from_str::<Budget>(&json).unwrap(), budget);
    }
}
