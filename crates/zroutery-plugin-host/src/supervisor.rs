//! Admission quotas and IPC result matching independent of any JS engine.
use std::sync::{Arc, Mutex};
use zroutery_plugin_api::ipc::{Envelope, WorkerCall, WorkerReply};
use zroutery_plugin_api::{PluginError, Result};

pub const MAX_WORKERS: usize = 8;
pub const MAX_PLUGIN_CALLS: usize = 4;
pub const MAX_QUEUE: usize = 32;
pub const WORKER_MEMORY: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Supervisor {
    live: Arc<Mutex<usize>>,
}
pub struct WorkerPermit {
    live: Arc<Mutex<usize>>,
}
impl Supervisor {
    pub fn admit(&self) -> Result<WorkerPermit> {
        let mut live = self.live.lock().map_err(|_| PluginError::PluginBusy)?;
        if *live >= MAX_WORKERS {
            return Err(PluginError::PluginBusy);
        }
        *live += 1;
        Ok(WorkerPermit {
            live: Arc::clone(&self.live),
        })
    }
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        if let Ok(mut live) = self.live.lock() {
            *live = live.saturating_sub(1);
        }
    }
}

/// The caller retains this table entry. Replayed/stale/mismatched replies cannot
/// authorize a host action even if the worker supplies matching identity text.
pub struct PendingCall {
    pub call: Envelope<WorkerCall>,
    completed: bool,
}
impl PendingCall {
    pub fn new(call: Envelope<WorkerCall>) -> Self {
        Self {
            call,
            completed: false,
        }
    }
    pub fn complete(
        &mut self,
        reply: Envelope<WorkerReply>,
        live_epoch: &str,
        live_generation: &str,
    ) -> Result<WorkerReply> {
        if self.completed
            || reply.ipc_version != 1
            || reply.call_id != self.call.call_id
            || reply.worker_generation != self.call.worker_generation
            || reply.worker_generation != live_generation
            || reply.grant_epoch != self.call.grant_epoch
            || reply.grant_epoch != live_epoch
            || !self.call.value.accepts(&reply.value)
        {
            return Err(PluginError::StaleRevision);
        }
        self.completed = true;
        Ok(reply.value)
    }
    pub fn cancel(&mut self) {
        self.completed = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    Unavailable,
    Verified,
}

static SANDBOX_STATUS: std::sync::RwLock<SandboxStatus> =
    std::sync::RwLock::new(SandboxStatus::Unavailable);

/// Cached runtime sandbox attestation. Defaults to unavailable until a
/// successful platform probe proves the OS boundary and negative checks.
pub fn sandbox_status() -> SandboxStatus {
    *SANDBOX_STATUS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Replace the cached attestation. The plugin reload command re-runs the
/// platform probe and publishes the fresh answer here, so a worker that was
/// missing at startup can be picked up without restarting the app.
pub fn set_sandbox_status(status: SandboxStatus) {
    *SANDBOX_STATUS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
}

/// Run the platform sandbox probe and cache the result. A failed or
/// unavailable probe keeps executable facets denied.
pub fn verify_sandbox(worker: &std::path::Path, temp_root: &std::path::Path) -> SandboxStatus {
    let status = crate::sandbox::probe(worker, temp_root).unwrap_or(SandboxStatus::Unavailable);
    set_sandbox_status(status);
    sandbox_status()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_plugin_api::ipc::{Locale, UiInput};
    use zroutery_plugin_api::ui::Projection;
    #[test]
    fn global_workers_are_bounded_and_a_cancelled_call_never_accepts_late_reply() {
        let supervisor = Supervisor::default();
        let permits: Vec<_> = (0..MAX_WORKERS)
            .map(|_| supervisor.admit().unwrap())
            .collect();
        assert!(supervisor.admit().is_err());
        drop(permits);
        assert!(supervisor.admit().is_ok());
        let call = Envelope {
            ipc_version: 1,
            call_id: "one".into(),
            worker_generation: "g1".into(),
            grant_epoch: "1".into(),
            value: WorkerCall::UiRender {
                hook: "render".into(),
                input: UiInput {
                    contribution_id: "ui".into(),
                    locale: Locale::En,
                    view_revision: "1".into(),
                    projection: Projection::default(),
                },
            },
        };
        let mut pending = PendingCall::new(call);
        pending.cancel();
        let reply = Envelope {
            ipc_version: 1,
            call_id: "one".into(),
            worker_generation: "g1".into(),
            grant_epoch: "1".into(),
            value: WorkerReply::Error {
                code: PluginError::InvalidResult,
            },
        };
        assert!(pending.complete(reply, "1", "g1").is_err());
    }
}
