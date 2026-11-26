//! Existing media allocation and protection leases retained by asynchronous Preview work.

use std::sync::Arc;

/// Existing Frame Store leases carried by lowered GPU or asynchronous CPU work.
#[derive(Debug, Clone)]
pub(crate) struct PreviewMediaResidencyGuard {
    _resource: Option<mondrian_playback::MediaFrameResourceLease>,
    _protection: Option<mondrian_playback::MediaFrameProtectionLease>,
    _temporal_inputs: Option<Arc<mondrian_playback::ViewerFrameWorkLease>>,
}

impl PreviewMediaResidencyGuard {
    /// Carry existing leases without changing their admission class or charge.
    pub(crate) fn from_leases(
        resource: Option<mondrian_playback::MediaFrameResourceLease>,
        protection: Option<mondrian_playback::MediaFrameProtectionLease>,
        temporal_inputs: Option<Arc<mondrian_playback::ViewerFrameWorkLease>>,
    ) -> Self {
        Self {
            _resource: resource,
            _protection: protection,
            _temporal_inputs: temporal_inputs,
        }
    }
}
