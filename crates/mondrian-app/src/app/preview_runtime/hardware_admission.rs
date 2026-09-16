//! Preview Runtime Adapter for application-owned hardware-decode admission.

use super::*;

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Atomically install a renderer-qualified decoder root and publish its admission.
    ///
    /// Same-device D3D12VA support is unusable until the exact FFmpeg device
    /// root is present in the worker-family pool. Installation therefore
    /// precedes the immutable admission observation; a caller can never expose
    /// `PreferGpuResident` while workers still create an unrelated device.
    pub(crate) fn set_renderer_hardware_decode_admission(
        &self,
        admission: PlaybackHardwareDecodeAdmission,
        decoder_device_root: Option<mondrian_media::RendererHwAccelDeviceContext>,
    ) -> Result<(), RendererHardwareDecodeAdmissionError> {
        let previous_selector = self
            .hardware_decode_admission
            .get()
            .observation()
            .and_then(|previous| previous.hardware_decode_device_selector);
        let has_decoder_device_root = decoder_device_root.is_some();
        let installed_new_device_generation = match (
            admission.hardware_decode_device_selector,
            decoder_device_root,
        ) {
            (Some(selector), Some(root)) => self
                .decode_worker_resources
                .hardware_device_context_pool()
                .install_renderer_device_context(selector, root)
                .map_err(RendererHardwareDecodeAdmissionError::Install)?,
            (Some(mondrian_media::HwAccelDeviceSelector::D3D12VaAdapterIndex(_)), None)
                if admission.native_import_admission_ready
                    && admission.renderer_import_mode
                        == Some(mondrian_renderer::GpuNativeDecodedFrameImportMode::ZeroCopy) =>
            {
                return Err(RendererHardwareDecodeAdmissionError::MissingDeviceRoot);
            }
            _ => false,
        };
        let retired_previous_device_generation = previous_selector
            .filter(|previous| {
                !has_decoder_device_root
                    || Some(*previous) != admission.hardware_decode_device_selector
            })
            .is_some_and(|previous| {
                self.decode_worker_resources
                    .hardware_device_context_pool()
                    .retire_renderer_device_context(previous)
            });
        if installed_new_device_generation || retired_previous_device_generation {
            // A renderer device replacement is not a locality-preserving
            // Preview generation change. Old native frames cannot enter the
            // new device's Frame Store even as cache-only completions.
            self.retire_decoder_device_generation();
        }
        self.set_playback_hardware_decode_admission(admission);
        Ok(())
    }

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
        if self.viewer_cpu_fallback_active.get() {
            return PreviewHardwareDecodeRequest::Auto;
        }
        let request = self
            .hardware_decode_admission
            .get()
            .request_for_surface(key.native_surface_hint());
        if key.decode.representation().is_cpu_addressable()
            && request == PreviewHardwareDecodeRequest::PreferGpuResident
        {
            // The immutable key's CPU-addressable representation is the
            // payload authority. A later hardware-admission observation may
            // still prefer hardware decode, but cannot mutate that key into a
            // native-surface request.
            if key.native_surface_hint().is_some() {
                PreviewHardwareDecodeRequest::PreferHardwareDecode
            } else {
                // The current native contract only admits proven NV12/P010.
                // Do not repeatedly open a hardware Session for a profile or
                // chroma layout that cannot produce either supported surface.
                PreviewHardwareDecodeRequest::Auto
            }
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

/// Failure to publish a renderer-bound hardware-decode generation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RendererHardwareDecodeAdmissionError {
    /// The renderer advertised same-device D3D12VA without its FFmpeg root.
    #[error("same-device D3D12VA admission is missing the renderer-qualified FFmpeg device root")]
    MissingDeviceRoot,
    /// The worker-family device pool rejected the generation.
    #[error(transparent)]
    Install(#[from] mondrian_media::RendererHwAccelDeviceContextInstallError),
}
