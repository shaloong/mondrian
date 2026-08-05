//! Preview Runtime Adapter for application-owned hardware-decode admission.

use super::*;
use mondrian_media::PreviewDecodeGeometry;

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Set playback hardware-decode admission selected by the app runtime.
    ///
    /// The default is `Auto` until renderer-device readiness is reported. The
    /// runtime may raise playback to `PreferHardwareDecode` for FFmpeg
    /// CPU-transfer fallback or to `PreferGpuResident` once native video import
    /// support is actually ready.
    pub(crate) fn set_playback_hardware_decode_admission(
        &self,
        admission: PlaybackHardwareDecodeAdmission,
    ) {
        if self.hardware_decode_admission.get().observation() != Some(admission) {
            self.future_media_window.borrow_mut().clear();
        }
        self.hardware_decode_admission
            .set(PreviewHardwareDecodeAdmissionState::reported(admission));
    }

    #[cfg(test)]
    pub(crate) fn playback_hardware_decode_request_for_test(&self) -> PreviewHardwareDecodeRequest {
        self.hardware_decode_admission.get().request()
    }

    pub(super) fn hardware_decode_admission_diagnostics(
        &self,
    ) -> PreviewHardwareDecodeAdmissionDiagnostics {
        let state = self.hardware_decode_admission.get();
        let Some(admission) = state.observation() else {
            return PreviewHardwareDecodeAdmissionDiagnostics {
                playback_request: state.request(),
                ..PreviewHardwareDecodeAdmissionDiagnostics::default()
            };
        };
        PreviewHardwareDecodeAdmissionDiagnostics {
            playback_request: admission.request,
            renderer_native_import_support_known: true,
            renderer_native_import_ready: admission.renderer_native_import_ready,
            renderer_import_mode: admission.renderer_import_mode,
            native_import_admission_ready: admission.native_import_admission_ready,
            admission_blocker: admission.admission_blocker,
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
        let request = self
            .hardware_decode_admission
            .get()
            .request_for_surface(key.native_surface_hint());
        if matches!(key.decode.geometry(), PreviewDecodeGeometry::FitWithin(_))
            && request == PreviewHardwareDecodeRequest::PreferGpuResident
        {
            // The immutable key's CPU-addressable/scaled geometry is the
            // payload authority. A later hardware-admission observation may
            // still prefer hardware decode, but cannot mutate that key into a
            // native-surface request.
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        } else {
            request
        }
    }

    pub(super) fn hardware_decode_device_selector_for_access_mode(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> Option<HwAccelDeviceSelector> {
        self.hardware_decode_admission.get().device_selector()
    }
}
