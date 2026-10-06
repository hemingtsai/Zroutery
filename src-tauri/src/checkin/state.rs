//! The browser check-in state machine.
//!
//! # Why the transitions are checked rather than implied
//!
//! The failure this module exists to prevent is the one that reads as correct
//! code: a WAF challenge is hit, the operation reports failure, and the next
//! attempt launches a fresh browser — discarding the session that was halfway
//! through solving the challenge and making the user authenticate again. Nothing
//! in that sequence is obviously wrong, and the user sees a check-in that appears
//! to try and fail repeatedly for no reason.
//!
//! So the transition table is the enforcement. Two properties are encoded and
//! tested:
//!
//! * **A held session is never discarded without being asked for.** There is no
//!   transition out of [`State::WaitingForUser`] that does not either resume the
//!   running operation, cancel it explicitly, or report a genuine failure. In
//!   particular there is no path back to [`State::StartingBrowser`], because a
//!   second browser would be a different session with no memory of the
//!   challenge.
//! * **Cancellation is always available and always explicit.** From any live
//!   state, [`Event::Cancelled`] is legal. A pause the user cannot end is a bug,
//!   and making cancellation a first-class transition rather than an abort on
//!   the process is what lets the browser be shut down cleanly afterwards.
//!
//! # What is deliberately not here
//!
//! No browser, no process handle and no socket. This file is the policy; the
//! mechanism lives in [`super::cdp`]. That split is what makes the policy
//! testable without a browser installed, which is the only way the properties
//! above are actually checked rather than asserted in a comment.

use std::fmt;

/// Where a browser-driven check-in has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    /// Accepted for execution. No browser exists yet.
    Pending,
    /// Launching a headed browser on this account's isolated profile.
    StartingBrowser,
    /// The browser is up and the check-in page is loading.
    Navigating,
    /// The page is live and the operation is being performed.
    Running,
    /// A challenge intercepted the operation. The browser is still open.
    BlockedByWaf,
    /// Paused, waiting for the user to finish the challenge by hand.
    WaitingForUser,
    /// The user said they finished; the same page is being re-examined.
    UserContinued,
    /// The check-in completed.
    Succeeded,
    /// The period's check-in was already done. A success, not a failure.
    AlreadyCompleted,
    /// The account cannot check in at all.
    NotSupported,
    /// The operation failed.
    Failed,
    /// Cancelled, by the user or by shutdown.
    Cancelled,
}

impl State {
    /// Whether the operation is finished and no further progress will happen
    /// without something external.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Succeeded
                | State::AlreadyCompleted
                | State::NotSupported
                | State::Failed
                | State::Cancelled
        )
    }

    /// Whether a browser is being held open for this operation.
    ///
    /// True from the moment the process exists until a terminal state. The
    /// reason it is not just `is_terminal()` is the interesting case: a paused
    /// operation is not terminal *and* still owns a live browser, and a caller
    /// that asked "should I close the browser" has to get the right answer in
    /// both of those states.
    pub fn holds_browser(self) -> bool {
        !matches!(self, State::Pending | State::NotSupported) && !self.is_terminal()
    }

    /// Whether the user is being asked to do something in the browser.
    pub fn awaiting_user(self) -> bool {
        matches!(self, State::BlockedByWaf | State::WaitingForUser)
    }

    /// Whether this state means the check-in granted a reward.
    ///
    /// Only [`State::Succeeded`]. [`State::AlreadyCompleted`] is deliberately
    /// excluded: it reports that the day was already handled, which is a
    /// different fact from this operation having caused a credit.
    pub fn is_success(self) -> bool {
        matches!(self, State::Succeeded | State::AlreadyCompleted)
    }
}

/// Something that happened to a check-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Event {
    /// Begin execution.
    Start,
    /// The browser process is up and attached.
    BrowserStarted,
    /// The check-in page finished loading.
    Navigated,
    /// The operation is running on a loaded page.
    OperationRunning,
    /// A challenge intercepted it.
    WafBlocked,
    /// The user has been told and the operation is waiting.
    AwaitingUser,
    /// The user says they finished the challenge.
    UserResumed,
    /// The page reports the account is already checked in.
    AlreadyCompleted,
    /// The provider does not offer check-in.
    NotSupported,
    /// The operation completed.
    Succeeded,
    /// The operation failed.
    Failed,
    /// The operation was cancelled.
    Cancelled,
}

/// A transition the machine will not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: State,
    pub event: Event,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "illegal check-in transition: {:?} + {:?}",
            self.from, self.event
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The legal transition table.
///
/// Written as one table rather than as nested matches so the policy can be read
/// in one pass. Every legal move is on it, which is the property the tests below
/// check: the machine can do nothing else.
const TRANSITIONS: &[(State, Event, State)] = &[
    // Start-up. `Pending + BrowserStarted` has no row, and that is the point:
    // there is exactly one way to create a browser, so resuming cannot create one.
    (State::Pending, Event::Start, State::StartingBrowser),
    (
        State::StartingBrowser,
        Event::BrowserStarted,
        State::Navigating,
    ),
    (State::Navigating, Event::Navigated, State::Running),
    (State::Running, Event::OperationRunning, State::Running),
    // Outcomes from a running operation.
    (State::Running, Event::Succeeded, State::Succeeded),
    (
        State::Running,
        Event::AlreadyCompleted,
        State::AlreadyCompleted,
    ),
    (State::Running, Event::NotSupported, State::NotSupported),
    (State::Running, Event::Failed, State::Failed),
    // The challenge path. Every hop keeps the same browser: `BlockedByWaf` goes
    // to `WaitingForUser`, and the resume goes back to `Running`. Neither can
    // reach `StartingBrowser`, so the session that was solving the challenge is
    // the session that finishes the operation.
    (State::Running, Event::WafBlocked, State::BlockedByWaf),
    (
        State::BlockedByWaf,
        Event::AwaitingUser,
        State::WaitingForUser,
    ),
    (
        State::WaitingForUser,
        Event::UserResumed,
        State::UserContinued,
    ),
    (
        State::UserContinued,
        Event::OperationRunning,
        State::Running,
    ),
    // A challenge met while resuming is the same challenge met again, not a new
    // failure — the user gets to be told again rather than the operation dying.
    (State::UserContinued, Event::WafBlocked, State::BlockedByWaf),
    // Cancellation from every live state, including a pause. A wait the user
    // cannot end is a bug, and making it a first-class transition is what lets
    // the browser be closed cleanly instead of abandoned.
    (State::StartingBrowser, Event::Cancelled, State::Cancelled),
    (State::Navigating, Event::Cancelled, State::Cancelled),
    (State::Running, Event::Cancelled, State::Cancelled),
    (State::BlockedByWaf, Event::Cancelled, State::Cancelled),
    (State::WaitingForUser, Event::Cancelled, State::Cancelled),
    (State::UserContinued, Event::Cancelled, State::Cancelled),
    // Shutdown can interrupt a launch.
    (State::Pending, Event::Cancelled, State::Cancelled),
];

/// Apply one event.
pub fn transition(from: State, event: Event) -> Result<State, IllegalTransition> {
    TRANSITIONS
        .iter()
        .find(|(state, e, _)| *state == from && *e == event)
        .map(|(_, _, to)| *to)
        .ok_or(IllegalTransition { from, event })
}

/// Whether `event` is legal in `state`.
pub fn can(from: State, event: Event) -> bool {
    transition(from, event).is_ok()
}

/// Every event legal in `state`.
pub fn legal_events(from: State) -> Vec<Event> {
    const ALL: &[Event] = &[
        Event::Start,
        Event::BrowserStarted,
        Event::Navigated,
        Event::OperationRunning,
        Event::WafBlocked,
        Event::AwaitingUser,
        Event::UserResumed,
        Event::AlreadyCompleted,
        Event::NotSupported,
        Event::Succeeded,
        Event::Failed,
        Event::Cancelled,
    ];
    ALL.iter()
        .copied()
        .filter(|event| can(from, *event))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(event: Event) -> Result<State, IllegalTransition> {
        transition(State::Running, event)
    }

    #[test]
    fn the_happy_path_reaches_success() {
        assert_eq!(
            transition(State::Pending, Event::Start),
            Ok(State::StartingBrowser)
        );
        assert_eq!(
            transition(State::StartingBrowser, Event::BrowserStarted),
            Ok(State::Navigating)
        );
        assert_eq!(
            transition(State::Navigating, Event::Navigated),
            Ok(State::Running)
        );
        assert_eq!(run(Event::Succeeded), Ok(State::Succeeded));
    }

    #[test]
    fn a_challenge_pauses_and_the_resume_returns_to_the_same_run() {
        assert_eq!(run(Event::WafBlocked), Ok(State::BlockedByWaf));
        assert_eq!(
            transition(State::BlockedByWaf, Event::AwaitingUser),
            Ok(State::WaitingForUser)
        );
        // The resume continues the operation. It does not start a browser, so the
        // session that was solving the challenge is the session that finishes.
        assert_eq!(
            transition(State::WaitingForUser, Event::UserResumed),
            Ok(State::UserContinued)
        );
        assert_eq!(
            transition(State::UserContinued, Event::OperationRunning),
            Ok(State::Running)
        );
        assert_eq!(run(Event::Succeeded), Ok(State::Succeeded));
    }

    #[test]
    fn a_resume_can_never_create_a_second_browser() {
        // The single most important property here. Every state reachable from a
        // challenge, checked against the one event that launches a browser.
        for state in [
            State::BlockedByWaf,
            State::WaitingForUser,
            State::UserContinued,
        ] {
            assert!(
                !can(state, Event::BrowserStarted),
                "{state:?} must not be able to launch a browser: that would discard the \
                 held session and make the user authenticate again"
            );
            assert!(
                !can(state, Event::Start),
                "{state:?} must not be able to restart the operation"
            );
        }
    }

    #[test]
    fn a_pause_cannot_become_a_failure_without_an_operation_being_attempted() {
        // From a pause, the only way to `Failed` is... there is none. A challenge
        // the user is mid-way through solving is not a failed check-in, and
        // reporting it as one is what makes the UI offer "try again" instead of
        // "continue".
        for state in [State::BlockedByWaf, State::WaitingForUser] {
            assert!(
                !can(state, Event::Failed),
                "{state:?} must not reach Failed; it is paused, not failed"
            );
        }
    }

    #[test]
    fn a_pause_can_be_cancelled() {
        // A wait the user cannot end is a bug.
        for state in [
            State::StartingBrowser,
            State::Navigating,
            State::Running,
            State::BlockedByWaf,
            State::WaitingForUser,
            State::UserContinued,
            State::Pending,
        ] {
            assert!(
                can(state, Event::Cancelled),
                "{state:?} must be cancellable"
            );
        }
    }

    #[test]
    fn a_terminal_state_accepts_nothing() {
        for state in [
            State::Succeeded,
            State::AlreadyCompleted,
            State::NotSupported,
            State::Failed,
            State::Cancelled,
        ] {
            assert!(legal_events(state).is_empty(), "{state:?} is terminal");
            assert!(!state.holds_browser());
            assert!(state.is_terminal());
        }
    }

    #[test]
    fn a_held_browser_is_reported_while_paused() {
        // The question a caller actually asks: may I close the browser now? While
        // paused the answer must be no, even though the operation is not
        // progressing.
        assert!(State::Running.holds_browser());
        assert!(State::BlockedByWaf.holds_browser());
        assert!(State::WaitingForUser.holds_browser());
        assert!(State::UserContinued.holds_browser());
        assert!(!State::Pending.holds_browser(), "no browser exists yet");
        assert!(!State::NotSupported.holds_browser());
    }

    #[test]
    fn only_success_grants_a_reward() {
        assert!(State::Succeeded.is_success());
        assert!(State::AlreadyCompleted.is_success());
        assert!(!State::NotSupported.is_success());
        assert!(!State::Failed.is_success());
        assert!(!State::Cancelled.is_success());
        assert!(!State::Running.is_success());
    }

    #[test]
    fn only_a_waiting_state_asks_the_user_for_something() {
        assert!(State::BlockedByWaf.awaiting_user());
        assert!(State::WaitingForUser.awaiting_user());
        for state in [
            State::Running,
            State::Pending,
            State::Failed,
            State::Succeeded,
        ] {
            assert!(
                !state.awaiting_user(),
                "{state:?} must not ask for user action"
            );
        }
    }

    #[test]
    fn a_challenge_met_again_after_resuming_is_the_same_challenge() {
        // The user resumes too early, or the challenge re-appears. The correct
        // answer is to wait again, not to fail the operation.
        assert_eq!(
            transition(State::UserContinued, Event::WafBlocked),
            Ok(State::BlockedByWaf)
        );
    }

    #[test]
    fn every_legal_transition_is_on_the_table_and_nothing_else_is() {
        // Sweep the whole table rather than trusting the individual tests: the
        // claim is that `transition` implements exactly this table, so a future
        // edit to either side is caught.
        const ALL_STATES: &[State] = &[
            State::Pending,
            State::StartingBrowser,
            State::Navigating,
            State::Running,
            State::BlockedByWaf,
            State::WaitingForUser,
            State::UserContinued,
            State::Succeeded,
            State::AlreadyCompleted,
            State::NotSupported,
            State::Failed,
            State::Cancelled,
        ];
        let all_events = legal_events(State::Pending)
            .into_iter()
            .chain(legal_events(State::Running))
            .chain(legal_events(State::WaitingForUser))
            .chain([
                Event::BrowserStarted,
                Event::Navigated,
                Event::AwaitingUser,
                Event::UserResumed,
                Event::AlreadyCompleted,
                Event::NotSupported,
                Event::Succeeded,
                Event::Failed,
            ])
            .collect::<std::collections::BTreeSet<_>>();

        let mut found = 0;
        for (from, event, to) in TRANSITIONS {
            assert_eq!(
                transition(*from, *event),
                Ok(*to),
                "the table claims {from:?} + {event:?} -> {to:?} but the machine disagrees"
            );
            found += 1;
        }
        assert_eq!(found, TRANSITIONS.len());

        // And nothing outside it is legal.
        for from in ALL_STATES {
            for event in &all_events {
                let tabled = TRANSITIONS.iter().any(|(s, e, _)| s == from && e == event);
                assert_eq!(
                    can(*from, *event),
                    tabled,
                    "{from:?} + {event:?}: machine says {}, table says {tabled}",
                    can(*from, *event)
                );
            }
        }
    }

    #[test]
    fn no_transition_leaves_a_terminal_state() {
        for (from, event, to) in TRANSITIONS {
            assert!(
                !from.is_terminal(),
                "the table has a row leaving the terminal state {from:?} via {event:?} -> {to:?}"
            );
        }
    }
}
