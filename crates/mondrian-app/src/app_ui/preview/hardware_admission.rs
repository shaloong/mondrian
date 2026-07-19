//! Coherent renderer/platform hardware-decode admission snapshot and request projection.

use super::*;

/// One coherent renderer/platform admission observation.
///
/// `None` is the pre-discovery state. A reported observation is replaced as a
/// whole so request projection cannot combine fields from different probes.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PreviewHardwareDecodeAdmissionState(Option<PlaybackHardwareDecodeAdmission>);

impl PreviewHardwareDecodeAdmissionState {
    fn reported(admission: PlaybackHardwareDecodeAdmission) -> Self {
        Self(Some(admission))
    }

    fn request(self) -> PreviewHardwareDecodeRequest {
        self.0.map_or(PreviewHardwareDecodeRequest::Auto, |admission| {
            admission.request
        })
    }
}

impl AppUiPreviewService {
    /// Set playback hardware-decode admission selected by the app runtime.
    ///
    /// The default is `Auto` until renderer/platform readiness is reported. The
    /// runtime may raise playback to `PreferHardwareDecode` for FFmpeg
    /// CPU-transfer fallback or to `PreferGpuResident` once native video import
    /// support is actually ready.
    pub(crate) fn set_playback_hardware_decode_admission(
        &self,
        admission: PlaybackHardwareDecodeAdmission,
    ) {
        self.hardware_decode_admission
            .set(PreviewHardwareDecodeAdmissionState::reported(admission));
    }

    #[cfg(test)]
    pub(crate) fn playback_hardware_decode_request_for_test(&self) -> PreviewHardwareDecodeRequest {
        self.hardware_decode_admission.get().request()
    }

    pub(super) fn hardware_decode_admission_diagnostics(
        &self,
    ) -> AppUiPreviewHardwareDecodeAdmissionDiagnostics {
        let state = self.hardware_decode_admission.get();
        let Some(admission) = state.0 else {
            return AppUiPreviewHardwareDecodeAdmissionDiagnostics {
                playback_request: state.request(),
                ..AppUiPreviewHardwareDecodeAdmissionDiagnostics::default()
            };
        };
        AppUiPreviewHardwareDecodeAdmissionDiagnostics {
            playback_request: admission.request,
            renderer_native_import_support_known: true,
            renderer_native_import_ready: admission.renderer_native_import_ready,
            platform_native_import_ready: admission.platform_native_import_ready,
            native_import_admission_ready: admission.native_import_admission_ready,
            admission_blocker: admission.admission_blocker,
            platform_discovery_available: admission.platform_discovery_available,
            platform_zero_copy_supported: admission.platform_zero_copy_supported,
            platform_low_copy_fallback_supported: admission.platform_low_copy_fallback_supported,
            renderer_supported_handle_kinds: admission.renderer_supported_handle_kinds,
            renderer_supported_source_texture_formats: admission
                .renderer_supported_source_texture_formats,
        }
    }

    pub(super) fn hardware_decode_request_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> PreviewHardwareDecodeRequest {
        self.hardware_decode_admission.get().request()
    }

    pub(super) fn hardware_decode_request_for_key(
        &self,
        access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewHardwareDecodeRequest {
        let request = self.hardware_decode_request_for_access_mode(access_mode);
        if request != PreviewHardwareDecodeRequest::PreferGpuResident {
            return request;
        }
        let admission = self.hardware_decode_admission.get().0;
        let supported = match key.native_surface_hint {
            Some(MediaPreviewNativeSurfaceHint::Nv12) => {
                admission.is_some_and(|admission| admission.renderer_supports_nv12)
            }
            Some(MediaPreviewNativeSurfaceHint::P010) => {
                admission.is_some_and(|admission| admission.renderer_supports_p010)
            }
            None => true,
        };
        if supported {
            request
        } else {
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        }
    }

    pub(super) fn hardware_decode_device_selector_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> Option<HwAccelDeviceSelector> {
        self.hardware_decode_admission
            .get()
            .0
            .and_then(|admission| admission.hardware_decode_device_selector)
    }
}
