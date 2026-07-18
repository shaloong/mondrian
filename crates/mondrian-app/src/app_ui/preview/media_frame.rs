//! Decoded Viewer media payload and lazy CPU working-frame adaptation.
//!
//! The payload retains either working CPU pixels, a source-domain GPU-capable
//! frame, or an opaque native decoder surface. Conversion is centralized here
//! so schedulers and presentation code cannot invent a second color path.

use super::*;

#[derive(Debug, Clone)]
pub(crate) struct MediaPreviewFrame {
    pub(super) frame: Option<CpuColorFrame>,
    pub(super) gpu_source: Option<MediaPreviewGpuSourceFrame>,
    pub(super) native_source: Option<MediaPreviewNativeSourceFrame>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) logical_width: u32,
    pub(super) logical_height: u32,
    pub(super) signature: u64,
    pub(super) presentation_quality: mondrian_playback::FramePresentationQuality,
    pub(super) decode_execution: AppUiPreviewDecodeExecutionSummary,
}

impl MediaPreviewFrame {
    pub(crate) fn reserved_cpu_bytes(&self) -> usize {
        let linear_bytes = self
            .frame
            .as_ref()
            .map(|frame| std::mem::size_of_val(frame.rgba_f32().data.as_slice()))
            .unwrap_or(0);
        let source_and_lazy_working_bytes = self
            .gpu_source
            .as_ref()
            .map(|source| {
                let working_reservation = (self.width as usize)
                    .saturating_mul(self.height as usize)
                    .saturating_mul(4)
                    .saturating_mul(std::mem::size_of::<f32>());
                source.source.retained_bytes().saturating_add(working_reservation)
            })
            .unwrap_or(0);
        linear_bytes.saturating_add(source_and_lazy_working_bytes)
    }

    pub(crate) fn decoder_resource_units(&self) -> usize {
        usize::from(self.native_source.is_some())
    }

    pub(super) fn width(&self) -> u32 {
        self.width
    }

    pub(super) fn height(&self) -> u32 {
        self.height
    }

    fn logical_resolution(&self) -> Resolution {
        Resolution {
            width: self.logical_width,
            height: self.logical_height,
        }
    }

    pub(super) fn presentation_quality(&self) -> mondrian_playback::FramePresentationQuality {
        self.presentation_quality
    }

    pub(super) fn decode_execution(&self) -> AppUiPreviewDecodeExecutionSummary {
        self.decode_execution
    }

    pub(super) fn gpu_source(&self) -> Option<mondrian_renderer::ViewerGpuMediaSource> {
        self.gpu_source.as_ref().map(|source| mondrian_renderer::ViewerGpuMediaSource {
            source: Arc::clone(&source.source),
            input_transform: source.input_transform.clone(),
            decoder_residency: source.decoder_residency,
            decoder_handle_kind: source.decoder_handle_kind,
            decoded_surface_format: source.decoded_surface_format,
            decoded_video_sampling: source.decoded_video_sampling,
        })
    }

    pub(super) fn native_source(&self) -> Option<mondrian_renderer::ViewerGpuNativeSource> {
        self.native_source
            .as_ref()
            .map(|source| mondrian_renderer::ViewerGpuNativeSource {
                source_color_space: source.source_color_space,
                input_transform: source.input_transform.clone(),
                native_frame: source.native_frame.clone(),
            })
    }

    pub(super) fn working_frame(&self) -> Result<MediaPreviewWorkingFrame, String> {
        if let Some(frame) = self.frame.as_ref() {
            return Ok(MediaPreviewWorkingFrame {
                frame: frame.clone(),
                color_diagnostics: None,
                stage_diagnostics: RenderColorStageDiagnostics::default(),
            });
        }
        let Some(source) = self.gpu_source.as_ref() else {
            if let Some(native) = self.native_source.as_ref() {
                return Err(format!(
                    "media preview frame is native GPU decoded ({} {:?}) and requires renderer native import; no CPU working fallback exists",
                    native.native_frame.handle_kind().as_str(),
                    native.native_frame.surface_format
                ));
            }
            return Err("media preview frame has no CPU working frame or GPU source".to_owned());
        };
        let cached_before = source.working_cache.get().is_some();
        let entry = source
            .working_cache
            .get_or_init(|| {
                execute_cpu_source_input_stage(source.source.as_ref(), &source.input_transform)
                    .map(|output| MediaPreviewWorkingFrameCacheEntry {
                        frame: output.result.frame,
                        color_diagnostics: output.result.diagnostics,
                        stage_diagnostics: output.stage_diagnostics,
                    })
                    .map_err(|err| {
                        format!("viewer preview lazy input color transform failed: {err}")
                    })
            })
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(MediaPreviewWorkingFrame {
            frame: entry.frame.clone(),
            color_diagnostics: (!cached_before).then_some(entry.color_diagnostics),
            stage_diagnostics: if cached_before {
                RenderColorStageDiagnostics::default()
            } else {
                entry.stage_diagnostics
            },
        })
    }
}

pub(super) fn project_preview_media_transform(
    transform: [f32; 6],
    frame: &MediaPreviewFrame,
    output_authoring: Resolution,
    output_sampled: Resolution,
) -> Option<[f32; 6]> {
    project_affine_to_sampled_extents(
        transform,
        frame.logical_resolution(),
        Resolution { width: frame.width(), height: frame.height() },
        output_authoring,
        output_sampled,
    )
}

pub(super) struct MediaPreviewWorkingFrame {
    pub(super) frame: CpuColorFrame,
    pub(super) color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(super) stage_diagnostics: RenderColorStageDiagnostics,
}

#[derive(Debug, Clone)]
pub(super) struct MediaPreviewGpuSourceFrame {
    pub(super) source: Arc<CpuSourceColorFrame>,
    pub(super) input_transform: RenderInputTransform,
    pub(super) decoder_residency: DecodedFrameResidency,
    pub(super) decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    pub(super) decoded_surface_format: DecodedVideoSurfaceFormat,
    pub(super) decoded_video_sampling: DecodedVideoSampling,
    working_cache: Arc<OnceLock<Result<MediaPreviewWorkingFrameCacheEntry, String>>>,
}

#[derive(Debug, Clone)]
pub(super) struct MediaPreviewNativeSourceFrame {
    pub(super) source_color_space: ColorSpace,
    pub(super) input_transform: RenderInputTransform,
    pub(super) native_frame: Arc<PreviewNativeDecodedFrame>,
}

impl MediaPreviewNativeSourceFrame {
    pub(super) fn from_native_frame(
        native_frame: PreviewNativeDecodedFrame,
        source_color_space: ColorSpace,
        input_transform: RenderInputTransform,
    ) -> Self {
        Self {
            source_color_space,
            input_transform,
            native_frame: Arc::new(native_frame),
        }
    }
}

impl MediaPreviewGpuSourceFrame {
    #[cfg(test)]
    pub(super) fn new(
        source: impl Into<CpuSourceColorFrame>,
        input_transform: RenderInputTransform,
    ) -> Self {
        Self {
            source: Arc::new(source.into()),
            input_transform,
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            decoded_video_sampling: DecodedVideoSampling::default(),
            working_cache: Arc::new(OnceLock::new()),
        }
    }

    pub(super) fn from_decode_diagnostics(
        source: impl Into<CpuSourceColorFrame>,
        input_transform: RenderInputTransform,
        diagnostics: PreviewDecodeDiagnostics,
    ) -> Self {
        Self {
            source: Arc::new(source.into()),
            input_transform,
            decoder_residency: diagnostics.decoded_frame_residency,
            decoder_handle_kind: diagnostics.gpu_frame_handle_kind,
            decoded_surface_format: diagnostics.decoded_surface_format,
            decoded_video_sampling: diagnostics.decoded_video_sampling,
            working_cache: Arc::new(OnceLock::new()),
        }
    }
}

#[derive(Debug, Clone)]
struct MediaPreviewWorkingFrameCacheEntry {
    frame: CpuColorFrame,
    color_diagnostics: RenderColorTransformDiagnostics,
    stage_diagnostics: RenderColorStageDiagnostics,
}
