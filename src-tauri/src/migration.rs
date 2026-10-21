//! The cutover plan: what moving a client onto the Zroutery gateway would do,
//! built from real detected state rather than written by hand.
//!
//! # Why a builder and not a stored plan
//!
//! [`MigrationPlan`] is data, so a caller could hand-write one. Nothing did,
//! and that is why the engine had no production caller: a plan is only useful
//! when someone can say where its paths and its endpoint came from. This module
//! is that someone. It reads the client's real config path through the same
//! adapters takeover uses, and takes the endpoint and port from the gateway
//! target, so a plan cannot name a file that does not exist or a URL that is
//! not this installation's.
//!
//! # Why the plan stages a document instead of editing in place
//!
//! [`MigrationAction::CopyConfig`] copies a file. The cutover therefore writes
//! the merged document to a staging file first and installs it with a copy,
//! which is what makes the step undoable by bytes rather than by field: the
//! snapshot taken before execution holds the original file exactly as it was.
//! Takeover writes managed *fields* and records their previous values; this
//! path writes the whole *file* and keeps the whole file. The two overlap on
//! purpose and neither replaces the other — see the `UI-NEW-TRACKS` record.

use std::path::{Path, PathBuf};

use serde::Serialize;
use zroutery_core::agent_takeover::{adapter_for, AgentType};
use zroutery_core::migration::{
    EndpointExpect, MigrationAction, MigrationPlan, MigrationState, MigrationStep,
};

/// The body marker a healthy Zroutery answers `/health` with.
///
/// A status check alone is not enough to call a cutover verified: a stale
/// process still holding the port answers 200, and so does an unrelated proxy
/// on the same address. The marker is what distinguishes *this* gateway. It
/// matches `handlers::health`, which returns `{"status":"ok"}`.
pub const HEALTH_BODY_MARKER: &str = "\"status\":\"ok\"";

// ---------------------------------------------------------------------------
// Plan view
// ---------------------------------------------------------------------------

/// One step as a surface renders it: a kind, a sentence, and whether undo can
/// put it back.
///
/// The action itself is not serialised to the window. It is the *plan*, and the
/// plan is what the confirmation is rendered from — a panel that showed the raw
/// enum would be asking a user to review a Rust type rather than a change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PlanStepView {
    pub kind: &'static str,
    pub description: String,
    /// Whether a rollback restores what this step changed.
    pub reversible: bool,
    /// The values this step's sentence is built from.
    ///
    /// The window says the step in the reader's language, and a sentence that
    /// names a client, a port, a file or an endpoint cannot be translated
    /// without them — so the plan sends them, and the window words them. The
    /// English description beside them is the fallback for a kind the window
    /// has no translation for, and it is also what the plan record keeps.
    pub params: StepParams,
}

/// The values one step's sentence is built from.
///
/// One field per kind, and each kind uses exactly one: a step about a process
/// names the process, a step about a port names the port, a step about a file
/// names the file, and a step about an endpoint names the endpoint. They are
/// separate fields rather than one list of strings because a list would leave
/// the window guessing which is which.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepParams {
    /// The client or process the step is about.
    pub subject: Option<String>,
    /// The port, when the step is about one.
    pub port: Option<u16>,
    /// The file the step installs over, when it installs one.
    pub path: Option<String>,
    /// The endpoint the step asks, when it asks one.
    pub url: Option<String>,
}

/// One client's cutover, as the panel reviews it.
#[derive(Debug, Clone, Serialize)]
pub struct CutoverPreview {
    pub agent: AgentType,
    /// The file the cutover replaces, and therefore the one a snapshot is taken
    /// of first. `None` when no adapter knows where the client keeps it.
    pub config_path: Option<String>,
    pub steps: Vec<PlanStepView>,
    /// Digest over the plan and the file it touches.
    ///
    /// `migration_apply` refuses a preview whose fingerprint no longer matches,
    /// which is the same compare-and-swap takeover uses. Confirming a plan
    /// drawn against one file and executing it against another is the failure
    /// the whole preview step exists to prevent.
    pub fingerprint: String,
    /// Why this client cannot be cut over, when it cannot.
    ///
    /// Carried rather than omitted so an unsupported client renders as an
    /// explained refusal instead of a row with no button and no reason.
    pub blocked_reason: Option<String>,
}

impl CutoverPreview {
    /// Whether applying this preview is possible at all.
    pub fn is_applicable(&self) -> bool {
        self.blocked_reason.is_none() && self.config_path.is_some() && !self.steps.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Where the cutover stages its merged document before installing it.
///
/// Under the configuration directory rather than beside the client's own file:
/// the destination directory belongs to another application, and leaving files
/// there would make a rollback that forgot one of them harder to reason about.
pub fn staging_path(config_dir: &Path, agent: AgentType) -> PathBuf {
    config_dir
        .join("migration")
        .join("staging")
        .join(format!("{}.json", agent.client_name()))
}

/// Build the cutover plan for `agent`, or explain why it has none.
///
/// The steps are in execution order and each one earns its place:
///
/// 1. `StopExternal` — advisory. The client was not started by this migration,
///    so it is reported rather than killed; "still running" has to be visible
///    before a config underneath it is rewritten.
/// 2. `StartZroutery` — a report of whether the port is free, and it does
///    not fail when it is not. The port this cutover switches to is usually
///    Zroutery's own, and Zroutery is usually already running there, so a port
///    in use is the ordinary case. What the operator needs to know is not
///    "is the number taken" but "is the holder the right service", and only
///    the endpoint check can answer that.
/// 3. `CopyConfig` — installs the staged document, backing up whatever was
///    there.
/// 4. `VerifyEndpoint` — proves the gateway answered *and* answered as
///    Zroutery, which is the step that makes a completed cutover mean
///    something. It is the last step, and it is the one that decides: the file
///    has been replaced by the time it runs, which is why the snapshot exists.
pub fn build_plan(
    config_dir: &Path,
    agent: AgentType,
    gateway_base_url: &str,
    port: u16,
) -> Result<(MigrationPlan, PathBuf), String> {
    let adapter = adapter_for(agent).ok_or_else(|| {
        format!(
            "this build has no migration adapter for {}",
            agent.client_name()
        )
    })?;
    let config_path = adapter.config_path()?;

    let staged = staging_path(config_dir, agent);
    let verify_url = format!("{}/health", gateway_base_url.trim_end_matches('/'));

    let steps = vec![
        MigrationStep {
            description: format!("check whether {} is still running", agent.client_name()),
            action: MigrationAction::StopExternal {
                process_name: agent.client_name().to_string(),
            },
            reversible: false,
        },
        MigrationStep {
            description: format!("check that port {port} is free"),
            action: MigrationAction::StartZroutery { port },
            reversible: false,
        },
        MigrationStep {
            description: format!("install the staged document over {}", config_path.display()),
            action: MigrationAction::CopyConfig {
                source: staged.to_string_lossy().into_owned(),
                dest: config_path.to_string_lossy().into_owned(),
            },
            reversible: true,
        },
        MigrationStep {
            description: format!("confirm {verify_url} answers as Zroutery"),
            action: MigrationAction::VerifyEndpoint {
                url: verify_url,
                expect: EndpointExpect::BodyContains(HEALTH_BODY_MARKER.to_string()),
            },
            reversible: false,
        },
    ];

    let plan = MigrationPlan {
        plan_id: format!("cutover-{}", agent.client_name()),
        source_description: format!("Zroutery cutover for {}", agent.client_name()),
        created_at: chrono::Utc::now().timestamp(),
        steps,
    };

    Ok((plan, config_path))
}

/// The preview of a plan: the sentences a user reads, and the digest that
/// refuses an apply against a plan that has since changed.
pub fn preview_of(
    agent: AgentType,
    plan: &MigrationPlan,
    config_path: Option<&Path>,
    blocked_reason: Option<String>,
) -> CutoverPreview {
    CutoverPreview {
        agent,
        config_path: config_path.map(|p| p.to_string_lossy().into_owned()),
        steps: plan
            .steps
            .iter()
            .map(|step| PlanStepView {
                kind: action_kind(&step.action),
                description: step.description.clone(),
                reversible: step.reversible,
                params: step_params(&step.action),
            })
            .collect(),
        fingerprint: fingerprint(plan, config_path),
        blocked_reason,
    }
}

/// The values one step's sentence is built from, read off the action.
///
/// Read off the action rather than parsed back out of the English sentence:
/// the sentence is for the plan record, and a window that had to recover the
/// values from prose would be guessing at its own protocol.
fn step_params(action: &MigrationAction) -> StepParams {
    match action {
        MigrationAction::StopExternal { process_name } => StepParams {
            subject: Some(process_name.clone()),
            ..StepParams::default()
        },
        MigrationAction::StartZroutery { port } => StepParams {
            port: Some(*port),
            ..StepParams::default()
        },
        MigrationAction::CopyConfig { dest, .. } => StepParams {
            path: Some(dest.clone()),
            ..StepParams::default()
        },
        MigrationAction::VerifyEndpoint { url, .. } => StepParams {
            url: Some(url.clone()),
            ..StepParams::default()
        },
        _ => StepParams::default(),
    }
}

/// The wire name of a step's kind, for a surface that groups or filters by it.
fn action_kind(action: &MigrationAction) -> &'static str {
    match action {
        MigrationAction::CopyConfig { .. } => "copy_config",
        MigrationAction::ValidateConfig { .. } => "validate_config",
        MigrationAction::StopExternal { .. } => "stop_external",
        MigrationAction::StartOwned { .. } => "start_owned",
        MigrationAction::StopOwned { .. } => "stop_owned",
        MigrationAction::StartZroutery { .. } => "start_zroutery",
        MigrationAction::VerifyEndpoint { .. } => "verify_endpoint",
        // Unreachable through [`build_plan`], which emits no custom steps. Named
        // rather than collapsed into an "other" so a plan that ever grows one
        // renders as itself instead of as a step nobody can interpret.
        MigrationAction::Custom { .. } => "custom",
    }
}

/// A digest over everything an apply depends on.
///
/// The config path is in the digest as well as the steps: the same step list
/// pointed at a different file is a different cutover, and a preview the user
/// confirmed must not authorise writing to a file they were not shown.
pub fn fingerprint(plan: &MigrationPlan, config_path: Option<&Path>) -> String {
    let mut text = String::new();
    text.push_str(&plan.plan_id);
    text.push('\u{1}');
    text.push_str(&plan.source_description);
    if let Some(path) = config_path {
        text.push('\u{1}');
        text.push_str(&path.to_string_lossy());
    }
    for step in &plan.steps {
        text.push('\u{1}');
        text.push_str(&step.description);
        text.push('\u{1}');
        text.push_str(&format!("{:?}", step.action));
    }
    format!("{:016x}", fnv1a(text.as_bytes()))
}

/// FNV-1a. Not a security hash: it only has to be stable and to keep a
/// confirmed preview from authorising a different plan than the one rendered.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// State view
// ---------------------------------------------------------------------------

/// Where a cutover stands, and whether the way back is still open.
///
/// `recoverable` and `can_rollback` answer different questions and the panel
/// renders them separately, for the same reason `account_available` and
/// `account_maintenance_available` are separate: "a migration was interrupted
/// and can be picked up" and "a migration failed and its files can be put back"
/// are not the same offer, and conflating them sends the user to a button that
/// cannot help.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationStatusView {
    pub state: MigrationState,
    /// True when the state means work was started and not finished
    /// (`Prepared`, `Verified`, `Switched`, `Failed`).
    pub interrupted: bool,
    /// Whether a snapshot exists, so a rollback has bytes to restore.
    pub has_snapshot: bool,
    /// Files a rollback would restore.
    pub snapshot_files: Vec<String>,
    /// Whether the store is writing its manifest somewhere durable.
    pub durable: bool,
    /// The last persistence failure, if the state is not reaching disk.
    pub persist_error: Option<String>,
    /// Owned child processes, if the plan started any.
    pub owned_processes: Vec<String>,
}
