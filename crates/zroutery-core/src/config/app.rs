//! The top-level configuration document, plus the window and notification behaviour it carries.

use serde::{Deserialize, Serialize};

use super::ml::MlRoutingConfig;
use super::provider::default_true;
use super::registry::{ModelEntry, ProviderConfig};
use super::routing::{ClassifierConfig, RoutingConfig, VisionConfig};
use super::server::{ServerConfig, ShadowConfig};
use crate::budget::Budget;

/// How the desktop app behaves as a resident process: what autostart, the
/// launch, and the close button do.
///
/// One set of names on every platform — the tray is a tray, the close button
/// is the close button, and "minimize" is deliberately not used: minimizing is
/// a window state, hiding to the tray is a lifecycle, and conflating them is
/// how users lose track of whether the app is still running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowBehavior {
    /// Register with the OS to launch Zroutery at login.
    #[serde(default)]
    pub launch_on_login: bool,
    /// Start without showing the main window; the tray is the only presence.
    #[serde(default)]
    pub silent_start: bool,
    /// Closing the window keeps the process (and the gateway) alive in the
    /// tray. Off: closing the window quits, stopping the gateway with it.
    #[serde(default = "default_true")]
    pub keep_in_tray: bool,
}

impl Default for WindowBehavior {
    fn default() -> Self {
        WindowBehavior {
            launch_on_login: false,
            silent_start: false,
            keep_in_tray: true,
        }
    }
}

/// Desktop notifications. Like [`WindowBehavior`], the gateway never reads this;
/// it belongs to the window layer.
///
/// Nothing here affects routing. It decides only whether a fact the router has
/// already produced is also raised at the user, and a notification that is not
/// raised changes nothing about the request it was about.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationConfig {
    /// Raise an OS notification when a tier's members all trip and requests
    /// start being escalated away from it, and again when it recovers.
    ///
    /// **Off by default, and the escalation is the reason.** Being answered from
    /// a tier you did not ask for costs more, so this is a billing signal rather
    /// than an emergency, and this app is built to sit in a menu bar all day.
    ///
    /// Fires on the transition, not per request: one notification per outage
    /// episode and one on recovery. A provider flapping cannot produce a stream
    /// of them, because the trigger is breaker state entering and leaving "fully
    /// cooling" rather than a request being escalated.
    ///
    /// Delivery is best effort and dependency-free — each platform's own
    /// mechanism, no plugin — so a failure is a missing notification and nothing
    /// else. See `tray::notify`.
    #[serde(default)]
    pub notify_on_tier_escalation: bool,
}

/// Root persisted document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Routing for Auto Mode classifier side queries. Orthogonal to
    /// `routing`, which stays about the main conversation.
    #[serde(default)]
    pub classifier: ClassifierConfig,
    /// Desktop application lifecycle. The gateway itself never reads this —
    /// it belongs to the window layer.
    #[serde(default)]
    pub window: WindowBehavior,
    /// OS notifications. Also the window layer's, and also not something the
    /// gateway reads.
    #[serde(default)]
    pub notifications: NotificationConfig,
    /// Vision fallback for non-vision models.
    #[serde(default)]
    pub vision: VisionConfig,
    /// Shadow decision evaluation. Only present in `ml` builds; the default
    /// (off) keeps request handling byte-identical to a build without it.
    #[cfg(feature = "ml")]
    #[serde(default)]
    pub shadow: ShadowConfig,
    /// The learned model's role in routing. Only present in `ml` builds.
    ///
    /// `shadow` records what the model would have done; this decides whether it
    /// actually does. They are separate switches on purpose — a deployment can
    /// gather shadow evidence for weeks before letting a promoted model change a
    /// single served request.
    #[cfg(feature = "ml")]
    #[serde(default)]
    pub ml_routing: MlRoutingConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Spending limits. Empty means no limit, which is the default because a proxy
    /// that refuses requests out of the box would be a surprise.
    #[serde(default)]
    pub budgets: Vec<Budget>,
}
