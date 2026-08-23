//! Coherent UI-independent hardware-decode admission state.
//!
//! Request, device selection, native surface-format support, and diagnostics
//! must be projected from one device-bound renderer observation. Window and
//! Headless Adapters cannot maintain parallel booleans or downgrade rules.

use mondrian_media::{
    HwAccelDeviceSelector, PreviewHardwareDecodeRequest, PreviewNativeSurfaceHint,
};

use super::native_video_import::PlaybackHardwareDecodeAdmission;

/// One coherent device-bound renderer admission observation.
///
/// `None` is the explicit pre-discovery state. A reported observation is
/// replaced as a whole so no consumer can combine fields from different probes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PreviewHardwareDecodeAdmissionState(Option<PlaybackHardwareDecodeAdmission>);

impl PreviewHardwareDecodeAdmissionState {
    /// Replace the complete renderer-device observation.
    pub(crate) const fn reported(admission: PlaybackHardwareDecodeAdmission) -> Self {
        Self(Some(admission))
    }

    /// Return the complete observation for diagnostics projection.
    pub(crate) const fn observation(self) -> Option<PlaybackHardwareDecodeAdmission> {
        self.0
    }

    /// Base request selected by renderer-device admission.
    pub(crate) fn request(self) -> PreviewHardwareDecodeRequest {
        self.0.map_or(PreviewHardwareDecodeRequest::Auto, |admission| {
            admission.request
        })
    }

    /// Request for one source surface family without inventing GPU residency.
    pub(crate) fn request_for_surface(
        self,
        surface: Option<PreviewNativeSurfaceHint>,
    ) -> PreviewHardwareDecodeRequest {
        let request = self.request();
        if request != PreviewHardwareDecodeRequest::PreferGpuResident {
            return request;
        }
        let supported = match (self.0, surface) {
            (Some(admission), Some(PreviewNativeSurfaceHint::Nv12)) => {
                admission.renderer_supports_nv12
            }
            (Some(admission), Some(PreviewNativeSurfaceHint::P010)) => {
                admission.renderer_supports_p010
            }
            (_, None) => false,
            (None, Some(_)) => false,
        };
        if supported {
            request
        } else {
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        }
    }

    /// Hardware device selected by the same renderer-device observation.
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
            renderer_import_mode: Some(
                mondrian_renderer::GpuNativeDecodedFrameImportMode::ZeroCopy,
            ),
            native_import_admission_ready: true,
            admission_blocker: None::<PreviewHardwareDecodeAdmissionBlocker>,
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
            state.request_for_surface(Some(PreviewNativeSurfaceHint::Nv12)),
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert_eq!(
            state.request_for_surface(Some(PreviewNativeSurfaceHint::P010)),
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
        assert_eq!(
            state.request_for_surface(Some(PreviewNativeSurfaceHint::P010)),
            PreviewHardwareDecodeRequest::Auto
        );
        assert_eq!(
            state.request_for_surface(None),
            PreviewHardwareDecodeRequest::Auto
        );
        assert_eq!(state.device_selector(), None);
    }

    #[test]
    fn unknown_surface_never_authorizes_gpu_residency() {
        let state = PreviewHardwareDecodeAdmissionState::reported(admission());

        assert_eq!(
            state.request_for_surface(None),
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
    }
}
