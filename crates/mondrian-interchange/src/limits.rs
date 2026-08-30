//! Resource bounds enforced before untrusted interchange input is admitted.

use serde::{Deserialize, Serialize};

/// Closed resource limits for all native parsers and the AAF helper Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeLimits {
    /// Maximum input or generated artifact size.
    pub max_bytes: usize,
    /// Maximum number of video plus audio tracks.
    pub max_tracks: usize,
    /// Maximum number of clips, gaps, and transitions.
    pub max_items: usize,
    /// Maximum JSON/XML structural nesting depth.
    pub max_nesting_depth: usize,
    /// Maximum bytes in one metadata string.
    pub max_string_bytes: usize,
    /// Maximum captured bytes from each AAF helper output stream.
    pub max_helper_output_bytes: usize,
    /// Maximum AAF helper wall-clock duration.
    pub helper_timeout_ms: u64,
}

impl Default for InterchangeLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_tracks: 128,
            max_items: 100_000,
            max_nesting_depth: 32,
            max_string_bytes: 16 * 1024,
            max_helper_output_bytes: 1024 * 1024,
            helper_timeout_ms: 30_000,
        }
    }
}

impl InterchangeLimits {
    pub(crate) fn validate(self) -> Result<(), crate::InterchangeError> {
        if self.max_bytes == 0
            || self.max_tracks == 0
            || self.max_items == 0
            || self.max_nesting_depth == 0
            || self.max_string_bytes == 0
            || self.max_helper_output_bytes == 0
            || self.helper_timeout_ms == 0
        {
            return Err(crate::InterchangeError::InvalidLimits);
        }
        Ok(())
    }
}
