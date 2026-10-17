//! The terminal status a request settles on.

use serde::{Deserialize, Serialize};
/// The terminal status of a request after all attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalStatus {
    /// Request completed successfully.
    Success,
    /// Request failed after all attempts exhausted.
    Failed,
    /// Client cancelled the request.
    Cancelled,
    /// Stream was interrupted after partial output.
    Interrupted,
}

impl FinalStatus {
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    pub const fn is_failure(self) -> bool {
        !self.is_success()
    }
}
