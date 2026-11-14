//! Platform launchers fail closed. Windows creates a LowBox token before the
//! helper can read any package bytes. Job-only/plain-process launch is impossible.
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{launch, RestrictedProcess};

use crate::supervisor::SandboxStatus;
use std::path::Path;

/// Prove the OS sandbox boundary with an isolated negative-fixture probe.
/// Returns `Ok(Verified)` only when the restricted worker passes all checks.
pub fn probe(worker: &Path, temp_root: &Path) -> zroutery_plugin_api::Result<SandboxStatus> {
    #[cfg(target_os = "windows")]
    {
        windows::probe(worker, temp_root)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (worker, temp_root);
        Ok(SandboxStatus::Unavailable)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn unavailable() -> zroutery_plugin_api::PluginError {
    // macOS needs a signed helper/entitlements; Linux needs runtime namespace and
    // seccomp evidence. Neither is inferred from the compiling host target.
    zroutery_plugin_api::PluginError::SandboxUnavailable
}
