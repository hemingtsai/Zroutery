//! What a client surface is allowed to claim, and the report a caller must supply before it may claim `active`.

use super::ownership::{FieldConflict, TakeoverProposal};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------

// AgentType
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// TakeoverStatus — what a client surface is allowed to claim
// ---------------------------------------------------------------------------

/// Where one client's takeover stands, right now.
///
/// # Why this exists separately from `OwnershipState`
///
/// [`OwnershipState`] is the manifest's four-state bookkeeping: verified,
/// adopted, releasing, released. It answers "what may this store do next". It
/// cannot answer the question a user actually asks, which is *is this client
/// really using Zroutery?* — and getting that wrong is the failure mode that
/// produced the `~/.claude.json` bug: a write that succeeded, a state machine
/// that said `Adopted`, and a client still talking to `api.anthropic.com`.
///
/// So the surface state is derived from the manifest **plus the live config on
/// disk** **plus whatever the caller could verify**, and the two are never
/// collapsed. `Active` requires the config to still match; `Degraded` is what a
/// takeover becomes the moment that stops being provable.
///
/// The transitions a user sees:
///
/// ```text
/// unsupported ─▶ not_detected ─▶ available ─▶ ready
///                                     │          │
///                                     │          ▼
///                                     └──────▶ active ◀──┐
///                                                │  ▲      │
///                              ┌─────────────────┤  │      │
///                              ▼                 ▼  │      │
///                          conflict ──resolve──▶ degraded  │
///                              │                 │  │      │
///                              └──────release────┴──┴──────┘
///                                               │
///                                               ▼
///                                            released
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TakeoverStatus {
    /// This build has no adapter for the client.
    ///
    /// The only current case is Gemini CLI. The reason is carried so a UI can
    /// explain *why* rather than showing an inert control.
    Unsupported {
        /// Human-readable reason, safe to show.
        reason: String,
    },

    /// An adapter exists, but no usable client config was found.
    ///
    /// `searched` lists the paths that were tried, which is the only thing that
    /// lets a user fix this themselves.
    NotDetected {
        /// Paths that were checked, in priority order.
        searched: Vec<String>,
        /// Why none of them was usable.
        reason: String,
    },

    /// A config was found and parses, but Zroutery cannot yet serve it.
    ///
    /// Distinct from [`TakeoverStatus::Ready`] because the blocker is on
    /// Zroutery's side, not the client's: there is no gateway token, no base URL
    /// or no model class to offer. `blocker` says which, because "not ready" with
    /// no reason is a dead end the user cannot act on.
    Available {
        /// The config that would be taken over.
        config_path: String,
        /// What Zroutery is missing.
        blocker: String,
    },

    /// Everything needed to take over is present; the user has not confirmed.
    ///
    /// The difference from [`TakeoverStatus::Available`] is the gateway target:
    /// `available` with no token, no base URL or no model classes is a client
    /// Zroutery cannot actually serve, and reporting it as `ready` would set up
    /// a button that fails when pressed.
    Ready {
        /// The config that would be taken over.
        config_path: String,
        /// What the takeover would change, for the confirmation dialog.
        proposal: Box<TakeoverProposal>,
    },

    /// Taken over, and the live config still matches what Zroutery wrote.
    Active {
        /// The config under management.
        config_path: String,
        /// The manifest generation, so a caller can tell two takeovers apart.
        generation: u64,
    },

    /// Taken over, but the live config no longer matches the record.
    ///
    /// This is never auto-resolved. Someone else changed a field Zroutery owns,
    /// and overwriting it without asking is how a takeover destroys work.
    Conflict {
        /// The config under management.
        config_path: String,
        /// Fields whose live value differs from what Zroutery wrote.
        conflicts: Vec<FieldConflict>,
    },

    /// Taken over and the config matches, but the takeover is not provably working.
    ///
    /// The honest state for a config that was written and never confirmed by a
    /// real request. Presenting this as `active` is exactly the lie this
    /// subsystem is meant to stop telling.
    Degraded {
        /// The config under management.
        config_path: String,
        /// The manifest generation.
        generation: u64,
        /// What could not be confirmed, phrased for a user.
        reasons: Vec<String>,
    },

    /// Not held; a previous takeover was released and the config restored.
    Released {
        /// The config that was restored.
        config_path: String,
        /// The manifest generation, so a re-adopt is distinguishable.
        generation: u64,
    },

    /// An operation failed for a reason the user can act on.
    Error {
        /// Human-readable reason, safe to show.
        reason: String,
    },
}

impl TakeoverStatus {
    /// The short state name, for logs and for a UI that switches on it.
    pub fn name(&self) -> &'static str {
        match self {
            TakeoverStatus::Unsupported { .. } => "unsupported",
            TakeoverStatus::NotDetected { .. } => "not_detected",
            TakeoverStatus::Available { .. } => "available",
            TakeoverStatus::Ready { .. } => "ready",
            TakeoverStatus::Active { .. } => "active",
            TakeoverStatus::Conflict { .. } => "conflict",
            TakeoverStatus::Degraded { .. } => "degraded",
            TakeoverStatus::Released { .. } => "released",
            TakeoverStatus::Error { .. } => "error",
        }
    }

    /// Whether Zroutery currently holds managed fields for this client.
    ///
    /// `true` for `active`, `conflict` and `degraded` — all three still hold
    /// ownership and all three need the release path available. A caller that
    /// used `== Active` here would hide the release button exactly when the user
    /// most needs it.
    pub fn holds_ownership(&self) -> bool {
        matches!(
            self,
            TakeoverStatus::Active { .. }
                | TakeoverStatus::Conflict { .. }
                | TakeoverStatus::Degraded { .. }
        )
    }

    /// Whether the takeover can be confirmed as genuinely in effect.
    ///
    /// Only [`TakeoverStatus::Active`] qualifies. `Degraded` and `Conflict` do
    /// not, which is the whole point of having them.
    pub fn is_verified_effective(&self) -> bool {
        matches!(self, TakeoverStatus::Active { .. })
    }
}

/// What a caller could actually prove about a client that was told to reload.
///
/// The core cannot start a Claude Code process and watch its traffic, so this is
/// the seam: the command layer observes what it can and passes it in, and the
/// status machine refuses to say `Active` on an unproven report.
///
/// `request_confirmed` is three-valued on purpose. `None` means "not
/// observable here" — no real request was seen — and that is *not* the same as
/// `Some(false)`, which means a request was seen and did not arrive. Collapsing
/// them would let an environment that cannot observe clients claim `Active`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    /// The live config still carries the values Zroutery wrote.
    pub config_matches: bool,
    /// The credential Zroutery handed out still authenticates.
    pub credential_ok: bool,
    /// A real request from the client was observed reaching Zroutery.
    pub request_confirmed: Option<bool>,
    /// Anything a user should be told, phrased for them.
    pub notes: Vec<String>,
}

impl VerificationReport {
    /// A report for a takeover nobody has checked yet.
    ///
    /// Deliberately `None` rather than `Some(false)`: not knowing is not the
    /// same as knowing it failed, and it keeps the status at `Degraded` rather
    /// than `Conflict`.
    pub fn unchecked() -> Self {
        Self {
            config_matches: false,
            credential_ok: false,
            request_confirmed: None,
            notes: vec!["this takeover has not been verified yet".into()],
        }
    }
}

// ---------------------------------------------------------------------------
