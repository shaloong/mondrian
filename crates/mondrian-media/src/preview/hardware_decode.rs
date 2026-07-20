//! FFmpeg hardware-decode admission and codec-context configuration state.
//!
//! This module lowers one typed hardware request and device selector into
//! backend probes, an execution decision, and the exact pixel-format callback
//! state. Observed native/CPU-transfer facts update the same plan; callers do
//! not infer hardware success from requested capabilities.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreviewHardwareDecodePlan {
    pub(super) request: PreviewHardwareDecodeRequest,
    pub(super) decision: PreviewHardwareDecodeDecision,
    pub(super) probe: HwAccelProbe,
    pub(super) ffmpeg_codec_config: HwAccelCodecConfigProbe,
    pub(super) ffmpeg_device_context: HwAccelDeviceContextProbe,
    pub(super) device_selector: Option<HwAccelDeviceSelector>,
    pub(super) hardware_cpu_transfer_configured: bool,
    pub(super) hardware_cpu_transfer_observed: bool,
    pub(super) hardware_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus,
    pub(super) native_decode_fallback: Option<PreviewNativeDecodeFallback>,
}

impl PreviewHardwareDecodePlan {
    pub(super) fn resolve(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        codec_id: ffmpeg::codec::Id,
        device_selector: Option<HwAccelDeviceSelector>,
    ) -> Self {
        let probe = HwAccelBackend::probe();
        let (probe, ffmpeg_codec_config, ffmpeg_device_context) = Self::resolve_backend_probes(
            request,
            access_mode,
            backend,
            codec_id,
            device_selector,
            probe,
        );
        let decision = Self::decision_for(
            request,
            access_mode,
            backend,
            &probe,
            &ffmpeg_codec_config,
            &ffmpeg_device_context,
        );
        Self {
            request,
            decision,
            probe,
            ffmpeg_codec_config,
            ffmpeg_device_context,
            device_selector,
            hardware_cpu_transfer_configured: false,
            hardware_cpu_transfer_observed: false,
            hardware_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            native_decode_fallback: None,
        }
    }

    pub(super) fn resolve_backend_probes(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        codec_id: ffmpeg::codec::Id,
        device_selector: Option<HwAccelDeviceSelector>,
        mut probe: HwAccelProbe,
    ) -> (
        HwAccelProbe,
        HwAccelCodecConfigProbe,
        HwAccelDeviceContextProbe,
    ) {
        let mut fallback = None;
        for candidate in probe.candidate_backends.iter().copied() {
            let codec_config = candidate.probe_ffmpeg_codec_config(codec_id);
            let device_context = Self::device_context_probe_for_plan(
                request,
                access_mode,
                backend,
                candidate,
                &codec_config,
                device_selector,
            );
            fallback
                .get_or_insert_with(|| (candidate, codec_config.clone(), device_context.clone()));
            let codec_ready = codec_config.backend_maps_to_ffmpeg_device
                && codec_config.ffmpeg_device_type_available
                && codec_config.ffmpeg_decoder_available
                && codec_config.ffmpeg_codec_config_available;
            if !codec_ready {
                continue;
            }
            if request.prefers_gpu_residency()
                && !ffmpeg_native_resource_adapter_available(&codec_config)
            {
                continue;
            }
            if request.prefers_gpu_residency()
                && device_selector.is_some_and(|selector| !selector.selects_backend(candidate))
            {
                continue;
            }
            if Self::plan_requires_device_context(request, access_mode, backend)
                && !device_context.device_context_created
            {
                continue;
            }
            probe.candidate_backend = Some(candidate);
            probe.candidate_handle_kind = candidate.native_handle_kind();
            probe.candidate_surface_formats = candidate.preferred_surface_formats();
            probe.decoder_adapter_available =
                ffmpeg_native_resource_adapter_available(&codec_config);
            return (probe, codec_config, device_context);
        }

        let (candidate, codec_config, device_context) = fallback.unwrap_or_else(|| {
            let backend = HwAccelBackend::None;
            (
                backend,
                backend.probe_ffmpeg_codec_config(codec_id),
                HwAccelDeviceContextProbe::unavailable(
                    backend,
                    "no platform hardware decode backend candidates are available",
                ),
            )
        });
        probe.candidate_backend = if candidate == HwAccelBackend::None {
            None
        } else {
            Some(candidate)
        };
        probe.candidate_handle_kind = candidate.native_handle_kind();
        probe.candidate_surface_formats = candidate.preferred_surface_formats();
        (probe, codec_config, device_context)
    }

    pub(super) fn plan_requires_device_context(
        request: PreviewHardwareDecodeRequest,
        _access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
    ) -> bool {
        matches!(
            request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
                | PreviewHardwareDecodeRequest::PreferGpuResident
                | PreviewHardwareDecodeRequest::RequireGpuResident
        ) && backend != PreviewDecodeBackend::ExternalFfmpegCpuRgba
    }

    pub(super) fn should_configure_hardware_decoder(
        &self,
        _access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        self.request != PreviewHardwareDecodeRequest::Auto
            && self.ffmpeg_codec_config.ffmpeg_codec_config_available
            && self.ffmpeg_device_context.device_context_created
    }

    pub(super) fn allows_cpu_transfer_fallback(&self) -> bool {
        matches!(
            self.request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
                | PreviewHardwareDecodeRequest::PreferGpuResident
        )
    }

    pub(super) fn mark_hardware_cpu_transfer_configured(&mut self, backend: HwAccelBackend) {
        self.hardware_cpu_transfer_configured = true;
        self.hardware_cpu_transfer_status =
            PreviewHardwareDecodeCpuTransferStatus::ConfiguredAwaitingFrame;
        self.probe.reason = format!(
            "{} FFmpeg hardware decode is configured; waiting for hardware frames before reporting active CPU-transfer decode",
            backend.as_str()
        );
    }

    pub(super) fn mark_hardware_cpu_transfer_setup_failed(&mut self) {
        self.hardware_cpu_transfer_status = PreviewHardwareDecodeCpuTransferStatus::SetupFailed;
    }

    pub(super) fn mark_hardware_cpu_transfer_decoder_open_failed(&mut self) {
        self.hardware_cpu_transfer_status =
            PreviewHardwareDecodeCpuTransferStatus::DecoderOpenFailed;
    }

    pub(super) fn mark_hardware_cpu_transfer_observed(&mut self) {
        if self.hardware_cpu_transfer_configured {
            self.hardware_cpu_transfer_observed = true;
            self.hardware_cpu_transfer_status = PreviewHardwareDecodeCpuTransferStatus::Observed;
            self.decision = PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer;
            let backend = self.probe.candidate_backend.unwrap_or(HwAccelBackend::None);
            self.probe.selected_backend = backend;
            self.probe.hardware_decode_active = true;
            self.probe.zero_copy_active = false;
            self.probe.frame_residency = DecodedFrameResidency::CpuRgba;
            self.probe.gpu_frame_handle_kind = None;
            self.probe.reason = format!(
                "{} FFmpeg hardware decode is active; this request permits transfer to the CPU RGBA boundary",
                backend.as_str()
            );
        }
    }

    pub(super) fn mark_native_decode_fallback(&mut self, reason: PreviewNativeDecodeFallback) {
        self.native_decode_fallback = Some(reason);
    }

    pub(super) fn mark_gpu_resident_native_observed(&mut self, kind: DecodedGpuFrameHandleKind) {
        self.hardware_cpu_transfer_configured = false;
        self.hardware_cpu_transfer_observed = false;
        self.hardware_cpu_transfer_status = PreviewHardwareDecodeCpuTransferStatus::NotAttempted;
        self.native_decode_fallback = None;
        self.decision = PreviewHardwareDecodeDecision::GpuResidentNative;
        let backend = self.probe.candidate_backend.unwrap_or(HwAccelBackend::None);
        self.probe.selected_backend = backend;
        self.probe.decoder_adapter_available = true;
        self.probe.hardware_decode_active = true;
        self.probe.zero_copy_active = true;
        self.probe.frame_residency = DecodedFrameResidency::GpuTexture;
        self.probe.gpu_frame_handle_kind = Some(kind);
        self.probe.reason = format!(
            "{} FFmpeg hardware decode produced a retained native decoder surface",
            backend.as_str()
        );
    }

    pub(super) fn device_context_probe_for_plan(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        selected_backend: HwAccelBackend,
        ffmpeg_codec_config: &HwAccelCodecConfigProbe,
        device_selector: Option<HwAccelDeviceSelector>,
    ) -> HwAccelDeviceContextProbe {
        if !Self::plan_requires_device_context(request, access_mode, backend)
            || !ffmpeg_codec_config.ffmpeg_codec_config_available
        {
            return HwAccelDeviceContextProbe::unavailable(
                selected_backend,
                "hardware device context creation was not required for this preview plan",
            );
        }
        selected_backend.cached_ffmpeg_device_context_probe_for(device_selector)
    }

    pub(super) fn decision_for(
        request: PreviewHardwareDecodeRequest,
        _access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        probe: &HwAccelProbe,
        ffmpeg_codec_config: &HwAccelCodecConfigProbe,
        ffmpeg_device_context: &HwAccelDeviceContextProbe,
    ) -> PreviewHardwareDecodeDecision {
        if request == PreviewHardwareDecodeRequest::Auto {
            return PreviewHardwareDecodeDecision::CpuRgbaNotRequested;
        }
        if backend == PreviewDecodeBackend::ExternalFfmpegCpuRgba {
            return PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary;
        }
        if probe.candidate_backend.is_none()
            || !ffmpeg_codec_config.backend_maps_to_ffmpeg_device
            || !ffmpeg_codec_config.ffmpeg_device_type_available
        {
            return PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable;
        }
        if !ffmpeg_codec_config.ffmpeg_decoder_available
            || !ffmpeg_codec_config.ffmpeg_codec_config_available
        {
            return PreviewHardwareDecodeDecision::CpuRgbaCodecUnsupported;
        }
        if !ffmpeg_device_context.device_context_created {
            return PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable;
        }
        if !probe.decoder_adapter_available {
            return PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable;
        }
        if !probe.hardware_decode_active
            || !probe.zero_copy_active
            || probe.frame_residency != DecodedFrameResidency::GpuTexture
        {
            return PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable;
        }
        if probe.gpu_frame_handle_kind.is_none() {
            return PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable;
        }
        PreviewHardwareDecodeDecision::GpuResidentNative
    }
}

pub(super) struct PreviewHardwareDecodeContextState {
    pub(super) preferred_hw_pixel_format: ffmpeg::ffi::AVPixelFormat,
}

pub(super) unsafe extern "C" fn preview_hardware_decode_get_format(
    context: *mut ffmpeg::ffi::AVCodecContext,
    pixel_formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    if context.is_null() || pixel_formats.is_null() {
        return ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }
    let state = unsafe { (*context).opaque as *const PreviewHardwareDecodeContextState };
    if state.is_null() {
        return ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }
    let preferred = unsafe { (*state).preferred_hw_pixel_format };
    let mut index = 0usize;
    let mut first = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    loop {
        let candidate = unsafe { *pixel_formats.add(index) };
        if candidate == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            return first;
        }
        if first == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            first = candidate;
        }
        if candidate == preferred {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

pub(super) fn preview_hardware_frame_format(format: ffmpeg::util::format::pixel::Pixel) -> bool {
    let format: ffmpeg::ffi::AVPixelFormat = format.into();
    matches!(
        format,
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VDPAU
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA
    )
}

pub(super) fn ffmpeg_native_resource_adapter_available(config: &HwAccelCodecConfigProbe) -> bool {
    matches!(
        config.hw_pixel_format,
        Some(HwAccelPixelFormat::D3D12 | HwAccelPixelFormat::D3D11)
    )
}
