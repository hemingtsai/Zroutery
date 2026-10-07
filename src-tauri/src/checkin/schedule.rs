//! When each account's check-in last settled, kept across restarts.
//!
//! # Why this file exists at all
//!
//! The scheduler asks [`zroutery_core::account::checkin::decide`] whether an
//! account is due, and the decision needs a history to be measured against. The
//! in-memory [`crate::state::CheckinView`] is not that history: it is empty on
//! every launch, and a scheduler reading it would find every account without a
//! history the instant the window opened.
//!
//! That failure is not hypothetical and its shape is the whole reason this file
//! exists. `NeedsFirstObservation` correctly refuses to run an account with no
//! history, so the first scan after a restart would do nothing — until it didn't,
//! because an account whose history was in memory looked the same as one that had
//! genuinely never been tried, and one visible browser window per account
//! appearing seconds after launch is a bug nobody would choose to ship.
//!
//! # What it deliberately does not store
//!
//! Provider and account identifiers and one unix timestamp each. No cookie, token,
//! password, page text, URL with a query string, or balance — the schedule is a
//! timing record, and anything more would be data on disk that no part of the
//! feature needs. Nested maps rather than a joined `provider/account` string so
//! that an identifier containing a separator cannot collide with another pair.
//!
//! # Failing safe
//!
//! An unreadable or corrupt file loads as empty. That is the safe direction and it
//! is worth being explicit about, because the unsafe direction is also plausible:
//! loading as empty means every account reports no history, every account is
//! `NeedsFirstObservation`, and the scheduler runs nothing until a person starts
//! one check-in by hand. Loading as "everything is ancient" would instead launch
//! every account at once. Refusing to load the application over a timing file
//! would be a third option and a worse one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// On-disk shape: provider id, then account id, then a unix timestamp.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ScheduleFile {
    /// Written so a file from a future shape is refused rather than half-read.
    #[serde(default)]
    version: u32,
    #[serde(default)]
    settled: BTreeMap<String, BTreeMap<String, i64>>,
}

/// The durable part of the scheduler's memory.
///
/// Cloneable and internally synchronised: a scan runs on a timer task while a
/// check-in finishes on a background task, and those two genuinely race — the scan
/// can read the map while an attempt is recording its settlement.
#[derive(Debug, Clone)]
pub struct Schedule {
    path: PathBuf,
    settled: std::sync::Arc<std::sync::Mutex<BTreeMap<String, BTreeMap<String, i64>>>>,
}

impl Schedule {
    /// The file a schedule lives in, under the same configuration directory the
    /// browser profiles use so there is one place to look and one place to back up.
    pub fn path_in(config_dir: &Path) -> PathBuf {
        config_dir.join("checkin-schedule.json")
    }

    /// Read the schedule, treating anything unreadable as empty.
    ///
    /// A missing file is the ordinary first-run case and is not logged as a
    /// problem. A *present but unreadable* file is logged, because silently
    /// starting from empty would otherwise look like a successful first run.
    pub fn load(path: PathBuf) -> Self {
        let settled = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<ScheduleFile>(&text) {
                Ok(file) => {
                    if file.version != 1 {
                        tracing::warn!(
                            version = file.version,
                            "check-in schedule has an unknown version; starting empty"
                        );
                        BTreeMap::new()
                    } else {
                        file.settled
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "check-in schedule is unreadable; starting empty rather than \
                         treating every account as due"
                    );
                    BTreeMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "check-in schedule could not be read; starting empty"
                );
                BTreeMap::new()
            }
        };
        Self {
            path,
            settled: std::sync::Arc::new(std::sync::Mutex::new(settled)),
        }
    }

    /// When this account's last attempt settled, if any.
    pub fn last_settled_at(&self, provider_id: &str, account_id: &str) -> Option<i64> {
        let guard = lock(&self.settled);
        guard
            .get(provider_id)
            .and_then(|accounts| accounts.get(account_id))
            .copied()
    }

    /// Record that this account settled at `at`.
    ///
    /// Returns whether the write landed, so a caller that cares can log it. The
    /// in-memory value is updated first and unconditionally: a schedule that fails
    /// to persist must not also lose the fact it just learned, because the next
    /// scan in this session is what would otherwise re-run the account.
    pub fn record_settled(&self, provider_id: &str, account_id: &str, at: i64) -> bool {
        {
            let mut guard = lock(&self.settled);
            guard
                .entry(provider_id.to_string())
                .or_default()
                .insert(account_id.to_string(), at);
        }
        self.persist()
    }

    /// Drop entries for accounts that are no longer declared.
    ///
    /// Called from the scan rather than from load, because at load time the
    /// configuration may not be readable yet and pruning against an empty
    /// declaration would erase every history in the file. Returns how many account
    /// histories were dropped — the figure a caller logs — and a provider whose
    /// accounts all went is removed with them, so the file does not accumulate
    /// empty providers for providers that no longer exist.
    pub fn prune_to(&self, declared: &[(String, String)]) -> usize {
        let mut dropped = 0usize;
        {
            let mut guard = lock(&self.settled);
            let keep: std::collections::HashSet<(&str, &str)> = declared
                .iter()
                .map(|(p, a)| (p.as_str(), a.as_str()))
                .collect();
            guard.retain(|provider, accounts| {
                accounts.retain(|account, _| {
                    let wanted = keep.contains(&(provider.as_str(), account.as_str()));
                    if !wanted {
                        dropped += 1;
                    }
                    wanted
                });
                !accounts.is_empty()
            });
        }
        if dropped > 0 {
            let _ = self.persist();
        }
        dropped
    }

    /// Write the whole file, or leave it untouched.
    ///
    /// Write-to-temporary and rename, because a schedule truncated by a crash
    /// mid-write is a file that loads as corrupt — which happens to be the safe
    /// direction, but only by luck, and the rename removes the dependence on luck.
    fn persist(&self) -> bool {
        let file = ScheduleFile {
            version: 1,
            settled: lock(&self.settled).clone(),
        };
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            tracing::warn!("check-in schedule could not be serialised; not written");
            return false;
        };
        let temp = self.path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&temp, text.as_bytes()) {
            tracing::warn!(
                path = %temp.display(),
                error = %e,
                "check-in schedule could not be written"
            );
            return false;
        }
        if let Err(e) = std::fs::rename(&temp, &self.path) {
            let _ = std::fs::remove_file(&temp);
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "check-in schedule could not be replaced"
            );
            return false;
        }
        true
    }
}

/// Lock a mutex, recovering from a poisoned one.
///
/// A panic while holding this map poisons it, and every subsequent `.lock()` would
/// then panic too — turning one panic into a permanent failure of the scheduler.
/// The value behind the lock is a plain map of timestamps, so there is no
/// invariant a poisoned lock could be protecting.
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zroutery-schedule-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("checkin-schedule.json")
    }

    #[test]
    fn a_settled_account_survives_a_restart() {
        let path = scratch("restart");
        let first = Schedule::load(path.clone());
        assert_eq!(first.last_settled_at("p", "a"), None);
        assert!(first.record_settled("p", "a", 1_700_000_000));

        // A second handle on the same file is what a relaunch looks like. Without
        // this the whole scheduler re-runs every account on every launch.
        let second = Schedule::load(path);
        assert_eq!(second.last_settled_at("p", "a"), Some(1_700_000_000));
    }

    #[test]
    fn accounts_are_told_apart_by_both_ids() {
        let path = scratch("identity");
        let s = Schedule::load(path);
        assert!(s.record_settled("p", "a", 10));
        assert!(s.record_settled("p", "b", 20));
        assert!(s.record_settled("q", "a", 30));
        assert_eq!(s.last_settled_at("p", "a"), Some(10));
        assert_eq!(s.last_settled_at("p", "b"), Some(20));
        assert_eq!(s.last_settled_at("q", "a"), Some(30));
        assert_eq!(s.last_settled_at("q", "b"), None);
    }

    #[test]
    fn an_identifier_containing_a_separator_cannot_collide() {
        // The reason this is a nested map and not a joined "provider/account"
        // string: a provider id of "p/a" with account "b" is the same joined text
        // as provider "p" with account "a/b".
        let path = scratch("collision");
        let s = Schedule::load(path);
        assert!(s.record_settled("p/a", "b", 111));
        assert!(s.record_settled("p", "a/b", 222));
        assert_eq!(s.last_settled_at("p/a", "b"), Some(111));
        assert_eq!(s.last_settled_at("p", "a/b"), Some(222));
    }

    #[test]
    fn a_missing_file_is_an_ordinary_first_run() {
        let dir =
            std::env::temp_dir().join(format!("zroutery-schedule-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Schedule::load(dir.join("nope.json"));
        assert_eq!(s.last_settled_at("p", "a"), None);
    }

    #[test]
    fn a_corrupt_file_starts_empty_rather_than_due() {
        // The unsafe alternative is loading as "everything is ancient", which
        // launches every account at once. Starting empty means the scheduler does
        // nothing until a person starts one attempt — the conservative direction.
        let path = scratch("corrupt");
        std::fs::write(&path, b"{ this is not json").expect("plant a corrupt file");
        let s = Schedule::load(path);
        assert_eq!(s.last_settled_at("p", "a"), None);
    }

    #[test]
    fn an_unknown_version_is_refused_rather_than_half_read() {
        let path = scratch("version");
        std::fs::write(&path, br#"{"version":99,"settled":{"p":{"a":5}}}"#)
            .expect("plant a future version");
        let s = Schedule::load(path);
        assert_eq!(
            s.last_settled_at("p", "a"),
            None,
            "a shape from a newer build must not be interpreted by this one"
        );
    }

    #[test]
    fn pruning_forgets_accounts_that_are_no_longer_declared() {
        let path = scratch("prune");
        let s = Schedule::load(path.clone());
        assert!(s.record_settled("p", "a", 10));
        assert!(s.record_settled("p", "gone", 20));
        assert!(s.record_settled("other", "b", 30));

        let dropped = s.prune_to(&[("p".to_string(), "a".to_string())]);
        assert_eq!(dropped, 2, "two providers end up empty and are dropped");
        assert_eq!(
            s.last_settled_at("p", "a"),
            Some(10),
            "declared history survives"
        );
        assert_eq!(s.last_settled_at("p", "gone"), None);
        assert_eq!(s.last_settled_at("other", "b"), None);

        // And the pruning is durable, not just in memory.
        let reopened = Schedule::load(path);
        assert_eq!(reopened.last_settled_at("other", "b"), None);
        assert_eq!(reopened.last_settled_at("p", "a"), Some(10));
    }

    #[test]
    fn the_file_carries_no_breadcrumb_to_a_credential() {
        // Read as bytes and looked at, rather than trusted: the schedule is a
        // timing record and a session secret written here would outlive every
        // browser profile that could be deleted.
        let path = scratch("contents");
        let s = Schedule::load(path.clone());
        assert!(s.record_settled("prov", "acct", 99));
        let raw = std::fs::read(&path).expect("read back");
        let text = String::from_utf8_lossy(&raw);
        for forbidden in [
            "cookie", "token", "password", "secret", "http://", "https://",
        ] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "the schedule must not contain {forbidden:?}: {text}"
            );
        }
        // The ids and the timestamp are the whole payload.
        assert!(text.contains("prov"));
        assert!(text.contains("acct"));
        assert!(text.contains("99"));
    }

    #[test]
    fn an_unwritable_location_keeps_the_fact_in_memory() {
        // A schedule that cannot be persisted must not also lose what it just
        // learned, or the next scan in this session re-runs the account it just
        // ran. Pointing at a directory that does not exist is how that is provoked
        // without needing a permission the test runner may hold anyway.
        let missing =
            std::env::temp_dir().join(format!("zroutery-schedule-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        let s = Schedule::load(missing.join("deeper").join("checkin-schedule.json"));
        assert!(!s.record_settled("p", "a", 5));
        assert_eq!(s.last_settled_at("p", "a"), Some(5));
    }
}
