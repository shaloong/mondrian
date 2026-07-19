//! Preview-service Adapter for application-owned hardware-decode admission.

use super::*;

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
        let Some(admission) = state.observation() else {
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

    #[cfg(test)]
    pub(super) fn hardware_decode_request_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> PreviewHardwareDecodeRequest {
        self.hardware_decode_admission.get().request()
    }

    pub(super) fn hardware_decode_request_for_key(
        &self,
        _access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewHardwareDecodeRequest {
        self.hardware_decode_admission
            .get()
            .request_for_surface(key.native_surface_hint)
    }

    pub(super) fn hardware_decode_device_selector_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> Option<HwAccelDeviceSelector> {
        self.hardware_decode_admission.get().device_selector()
    }
}
