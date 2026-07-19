//! Coherent UI-independent hardware-decode admission state.
//!
//! Request, device selection, native surface-format support, and diagnostics
//! must be projected from one renderer/platform observation. Window and
//! Headless Adapters cannot maintain parallel booleans or downgrade rules.

use mondrian_media::{HwAccelDeviceSelector, PreviewHardwareDecodeRequest};

use super::native_video_import::PlaybackHardwareDecodeAdmission;
use super::preview_access_mode::MediaPreviewNativeSurfaceHint;

/// One coherent renderer/platform admission observation.
///
/// `None` is the explicit pre-discovery state. A reported observation is
/// replaced as a whole so no consumer can combine fields from different probes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PreviewHardwareDecodeAdmissionState(Option<PlaybackHardwareDecodeAdmission>);

impl PreviewHardwareDecodeAdmissionState {
    /// Replace the complete renderer/platform observation.
    pub(crate) const fn reported(admission: PlaybackHardwareDecodeAdmission) -> Self {
        Self(Some(admission))
    }

    /// Return the complete observation for diagnostics projection.
    pub(crate) const fn observation(self) -> Option<PlaybackHardwareDecodeAdmission> {
        self.0
    }

    /// Base request selected by renderer/platform admission.
    pub(crate) fn request(self) -> PreviewHardwareDecodeRequest {
        self.0.map_or(PreviewHardwareDecodeRequest::Auto, |admission| {
            admission.request
        })
    }

    /// Request for one source surface family without inventing GPU residency.
    pub(crate) fn request_for_surface(
        self,
        surface: Option<MediaPreviewNativeSurfaceHint>,
    ) -> PreviewHardwareDecodeRequest {
        let request = self.request();
        if request != PreviewHardwareDecodeRequest::PreferGpuResident {
            return request;
        }
        let supported = match (self.0, surface) {
            (Some(admission), Some(MediaPreviewNativeSurfaceHint::Nv12)) => {
                admission.renderer_supports_nv12
            }
            (Some(admission), Some(MediaPreviewNativeSurfaceHint::P010)) => {
                admission.renderer_supports_p010
            }
            (_, None) => true,
            (None, Some(_)) => false,
        };
        if supported {
            request
        } else {
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        }
    }

    /// Hardware device selected by the same renderer/platform observation.
    pub(crate) fn device_selector(self) -> Option<HwAccelDeviceSelector> {
        self.0.and_then(|admission| admission.hardware_decode_device_selector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::native_video_import::PreviewHardwareDecodeAdmissionBlocker;

    fn admission() -> PlaybackHardwareDecodeAdmission {
        PlaybackHardwareDecodeAdmission {
            request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_device_selector: Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(3)),
            renderer_native_import_ready: true,
            platform_native_import_ready: true,
            native_import_admission_ready: true,
            admission_blocker: None::<PreviewHardwareDecodeAdmissionBlocker>,
            platform_discovery_available: true,
            platform_zero_copy_supported: false,
            platform_low_copy_fallback_supported: true,
            renderer_supported_handle_kinds: 1,
            renderer_supported_source_texture_formats: 1,
            renderer_supports_nv12: false,
            renderer_supports_p010: true,
        }
    }

    #[test]
    fn one_snapshot_projects_request_surface_support_and_device() {
        let state = PreviewHardwareDecodeAdmissionState::reported(admission());

        assert_eq!(
            state.request(),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            state.request_for_surface(Some(MediaPreviewNativeSurfaceHint::Nv12)),
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert_eq!(
            state.request_for_surface(Some(MediaPreviewNativeSurfaceHint::P010)),
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            state.device_selector(),
            Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(3))
        );
    }

    #[test]
    fn undiscovered_state_is_auto_and_has_no_device() {
        let state = PreviewHardwareDecodeAdmissionState::default();
        assert_eq!(state.request(), PreviewHardwareDecodeRequest::Auto);
        assert_eq!(state.device_selector(), None);
    }
}
