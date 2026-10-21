//! The state machine and the plan it executes: what a migration is, what step it is on, and what it may do next.

use serde::{Deserialize, Serialize};

// MigrationState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationState {
    Detected,
    Prepared,
    Verified,
    Switched,
    Completed,
    RolledBack,
    Failed,
}

impl MigrationState {
    /// Returns `true` if transitioning from `self` to `target` is allowed.
    pub fn can_transition_to(self, target: MigrationState) -> bool {
        use MigrationState::*;
        matches!(
            (self, target),
            (Detected, Prepared)
                | (Prepared, Verified)
                | (Verified, Switched)
                | (Switched, Completed)
                // Any state may fail
                | (_, Failed)
                // Only Failed may roll back
                | (Failed, RolledBack)
        )
    }
}

// ---------------------------------------------------------------------------
// MigrationPlan / MigrationStep / MigrationAction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub plan_id: String,
    pub source_description: String,
    pub steps: Vec<MigrationStep>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    pub description: String,
    pub action: MigrationAction,
    pub reversible: bool,
}

/// What counts as a healthy answer from an endpoint the migration just switched to.
///
/// This type exists because "the socket answered" is not evidence that the
/// endpoint works. A 500 is an answer, a 401 is an answer, and a proxy's
/// connection-refused page is an answer. Accepting any of them is what let a
/// migration walk all the way to `Completed` against a service that was broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointExpect {
    /// Any 2xx. The floor, not the goal: it proves the right port is serving.
    Success,
    /// One specific status code, for an endpoint with a defined non-2xx contract.
    Status(u16),
    /// A 2xx whose body contains this marker.
    ///
    /// Stronger than [`EndpointExpect::Success`] on purpose, and the one worth
    /// reaching for during a cutover: a 200 from the wrong service, or from a
    /// stale process still holding the port, passes a status check and fails
    /// this. The marker is what distinguishes "Zroutery is serving" from
    /// "something is serving".
    BodyContains(String),
}

impl Default for EndpointExpect {
    /// [`EndpointExpect::Success`], so a plan serialized before this field
    /// existed deserializes to the documented floor rather than failing to
    /// parse. It is deliberately the *weakest* option: an old plan that named no
    /// expectation gets the check it would have got before, and no more.
    fn default() -> Self {
        Self::Success
    }
}

/// One step of a migration, as data.
///
/// Every variant is executable by [`MigrationExecutor::execute_step`]. There is
/// no untyped escape hatch: [`MigrationAction::Custom`] exists on the wire for
/// compatibility and **fails** when executed, because a step that reports success
/// without doing anything is worse than a migration that refuses to start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MigrationAction {
    /// Copy a config file, backing up any existing destination first.
    ///
    /// Warns when the copied bytes contain credential-shaped keys. Core has no
    /// credential store, so it cannot route a secret anywhere -- it can only
    /// refuse to be quiet about one. Keeping the keys out of config is the
    /// desktop layer's job, where `ccswitch::credential_fingerprint` already
    /// establishes the convention that a credential is referenced, never carried.
    CopyConfig { source: String, dest: String },
    /// Parse a config file and fail if it is malformed.
    ///
    /// Carries the path because a validator with no input cannot validate: the
    /// earlier unit variant returned a success message unconditionally.
    ValidateConfig { path: String },
    /// Report whether a named external process is running.
    ///
    /// **Does not stop it.** This migration did not start that process, so it
    /// does not own its lifetime and will not terminate a program the user did
    /// not ask us to launch. Use [`MigrationAction::StopOwned`] for processes
    /// this migration started.
    StopExternal { process_name: String },
    /// Start a child process and take ownership of it.
    ///
    /// Ownership is what makes stopping safe: only a process recorded here can
    /// be stopped by label, and it is stopped with [`MigrationAction::StopOwned`].
    StartOwned {
        label: String,
        program: String,
        args: Vec<String>,
    },
    /// Stop a child this migration started, and wait for it to actually exit.
    StopOwned { label: String },
    /// Report whether a port is free, so a switch knows what it is about to
    /// collide with.
    ///
    /// **It does not fail when the port is taken**, and that is deliberate
    /// rather than a gap. The port a cutover switches to is usually Zroutery's
    /// own, and Zroutery is usually already running there — so a port in use
    /// is the ordinary case, not a collision. What decides whether the holder
    /// is the right service is the endpoint check and its body marker, which
    /// can tell "Zroutery is serving" from "something else is serving"; a port
    /// number cannot tell them apart at all.
    ///
    /// So this is a report, and the step that needs a guarantee asks
    /// [`MigrationAction::VerifyEndpoint`] for one.
    StartZroutery { port: u16 },
    /// Require a real answer from `url`, judged by `expect`.
    VerifyEndpoint {
        url: String,
        /// Defaults to [`EndpointExpect::Success`] so a plan written before this
        /// field existed still parses.
        #[serde(default)]
        expect: EndpointExpect,
    },
    /// Retained for wire compatibility. Executing it is an error.
    Custom { description: String },
}

/// Writes one snapshot file back to its original location.
///
/// A trait so that a restore *failure* can be exercised deterministically. The
/// obvious way to make `std::fs::write` fail is to make the file read-only, and
/// that does not port: CI runs Linux, macOS and Windows, and read-only semantics
/// differ between them. A gate whose outcome depends on the OS is worse than no
/// gate, because it is green until it is not.
pub trait SnapshotWriter: Send + Sync {
    /// Write `content` to `path`, or explain why it could not.
    fn write(&self, path: &str, content: &[u8]) -> Result<(), String>;
}

/// The real filesystem. What production uses.
pub struct StdFsWriter;

impl SnapshotWriter for StdFsWriter {
    fn write(&self, path: &str, content: &[u8]) -> Result<(), String> {
        std::fs::write(path, content).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
