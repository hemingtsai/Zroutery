//! The panel-backed balance path: what a provider's accounts add up to.
//!
//! The claim this whole path rests on is narrow and worth stating: a provider
//! that declares accounts has a balance, and the balance is the sum of what
//! those accounts report from the panel they are users of. Everything here is
//! about the sum being *true* — that it adds up, that it refuses to add up
//! what cannot be added, and that a provider with no accounts does not
//! pretend to have a balance.

use super::balances::{has_balance_source, sum_account_quotas};
use zroutery_core::account::types::AccountQuota;
use zroutery_core::config::{AccountConfig, ProviderConfig, ProviderKind};

fn quota(remaining: f64, used: f64, unit: &str) -> AccountQuota {
    AccountQuota {
        total: remaining + used,
        used,
        remaining,
        unit: unit.to_string(),
        resets_at: None,
    }
}

fn provider_with(accounts: Vec<AccountConfig>) -> ProviderConfig {
    let mut p = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    p.accounts = accounts;
    p
}

fn account(enabled: bool) -> AccountConfig {
    AccountConfig {
        account_id: "acct-0".into(),
        enabled,
        ..AccountConfig::default()
    }
}

/// Two accounts of one panel are two wallets, and the provider's balance is
/// both of them.
#[test]
fn one_providers_balance_is_the_sum_of_its_accounts() {
    let balance = sum_account_quotas(&[quota(12.5, 27.5, "USD"), quota(3.0, 1.0, "USD")])
        .expect("two accounts in one currency add up");

    assert_eq!(balance.currency, "USD");
    assert_eq!(balance.remaining, Some(15.5));
    assert_eq!(balance.used, Some(28.5));
    assert_eq!(balance.total, Some(44.0));
}

/// Adding dollars to yuan produces a number, and a number is not a balance.
///
/// This is the direction that matters: a total that cannot be spent, rendered
/// as though it could, is worse than no total. Two accounts reporting
/// different currencies is a fact about the declaration, and it is reported.
#[test]
fn accounts_in_different_currencies_are_not_added_together() {
    let error = sum_account_quotas(&[quota(10.0, 0.0, "USD"), quota(10.0, 0.0, "CNY")])
        .expect_err("two currencies cannot be one figure");

    assert!(
        error.contains("different currencies"),
        "the refusal names the cause: {error}"
    );
    assert!(
        error.contains("USD") && error.contains("CNY"),
        "the refusal names both currencies, so the user can see which two disagree: {error}"
    );
}

/// No account reported anything, so there is no figure.
///
/// The empty case is not the same as a zero balance, and returning zero here
/// would be the defect this path exists to avoid: a panel that answered
/// "nothing" would render as a wallet that is empty.
#[test]
fn no_account_reporting_is_not_a_balance_of_zero() {
    let error = sum_account_quotas(&[]).expect_err("nothing reported is not a figure");
    assert!(
        error.contains("no account reported"),
        "the refusal says what is missing rather than reporting a zero: {error}"
    );
}

/// A single account is the whole figure, and it is not doubled.
#[test]
fn a_single_account_is_the_whole_figure() {
    let balance = sum_account_quotas(&[quota(7.25, 0.0, "USD")]).expect("one account adds up");
    assert_eq!(balance.remaining, Some(7.25));
    assert_eq!(balance.total, Some(7.25));
}

/// A provider that declares an account has a balance to ask for, even though
/// the relay publishes none of its own.
///
/// This is the case the whole path exists for. A NewAPI relay serves `/v1`
/// and publishes no balance there, while the panel that owns the wallet does
/// publish one — to the user's access token, which is the account's
/// credential and not the provider's relay key. Before this, such a provider
/// was treated as having no balance at all: the chip never rendered and the
/// button was disabled.
#[test]
fn a_provider_with_an_account_has_a_balance_to_ask_for() {
    let mut provider = provider_with(vec![account(true)]);
    provider.balance.preset = zroutery_core::billing::BalancePreset::None;

    assert!(
        has_balance_source(&provider),
        "an account's panel holds a balance even when the relay publishes none"
    );
}

/// A disabled account is not an account to ask.
///
/// A declaration that is switched off is not probed, so its panel is not asked
/// and its balance is not in the sum. Counting it would make the provider look
/// like it had a balance source when the only account is off.
#[test]
fn a_provider_whose_only_account_is_disabled_has_nothing_to_ask_for() {
    let mut provider = provider_with(vec![account(false)]);
    provider.balance.preset = zroutery_core::billing::BalancePreset::None;

    assert!(
        !has_balance_source(&provider),
        "a disabled account is not probed, so it is not a source"
    );
}

/// A provider that publishes a balance of its own has one, whatever else is
/// declared.
#[test]
fn a_provider_that_publishes_its_own_balance_has_one() {
    let mut provider = provider_with(vec![]);
    provider.balance.preset = zroutery_core::billing::BalancePreset::DeepSeek;

    assert!(
        has_balance_source(&provider),
        "the relay's own endpoint is a source on its own"
    );
}
