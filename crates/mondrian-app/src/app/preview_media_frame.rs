//! Decoded Viewer media payload and lazy CPU working-frame adaptation.
//!
//! The payload retains either working CPU pixels, a source-domain GPU-capable
//! frame, or an opaque native decoder surface. Conversion is centralized here
//! so schedulers and presentation code cannot invent a second color path.

use std::sync::{Arc, OnceLock};

use mondrian_core::types::{ColorSpace, Resolution};
use mondrian_core::{compose_picture_affine, ResolvedPictureGeometry};
#[cfg(any(test, feature = "validation"))]
use mondrian_media::PreviewDecodeTemporalSelection;
use mondrian_media::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoSampling,
    DecodedVideoSurfaceFormat, PreviewDecodeDiagnostics, PreviewNativeDecodedFrame,
};
use mondrian_playback::FramePresentationQuality;
use mondrian_renderer::{
    execute_cpu_source_input_stage_with_session, project_affine_to_sampled_extents, CpuColorFrame,
    CpuSourceColorFrame, RenderColorStageDiagnostics, RenderColorTransformDiagnostics,
    RenderColorTransformError, RenderCpuColorExecutionSession, RenderInputTransform,
    ViewerGpuMediaSource, ViewerGpuNativeSource,
};

use super::preview_execution::{PreviewDecodeExecutionSummary, PreviewSemanticIdentity};

#[derive(Debug, Clone)]
pub(crate) struct MediaPreviewFrame {
    payload: MediaPreviewPayload,
    sampled_resolution: Resolution,
    logical_resolution: Resolution,
    source_to_display_affine: [f32; 6],
    identity: PreviewSemanticIdentity,
    cross_call_reusable: bool,
    presentation_quality: FramePresentationQuality,
    decode_execution: PreviewDecodeExecutionSummary,
    residency_resource: Option<mondrian_playback::MediaFrameResourceLease>,
    residency_protection: Option<mondrian_playback::MediaFrameProtectionLease>,
}

#[derive(Debug, Clone)]
enum MediaPreviewPayload {
    Working(CpuColorFrame),
    Source(MediaPreviewGpuSourceFrame),
    Native(MediaPreviewNativeSourceFrame),
}

impl MediaPreviewFrame {
    pub(crate) fn from_working(
        frame: CpuColorFrame,
        logical_resolution: Resolution,
        identity: PreviewSemanticIdentity,
        presentation_quality: FramePresentationQuality,
        decode_execution: PreviewDecodeExecutionSummary,
    ) -> Self {
        let descriptor = frame.descriptor();
        Self {
            payload: MediaPreviewPayload::Working(frame),
            sampled_resolution: Resolution { width: descriptor.width, height: descriptor.height },
            logical_resolution,
            source_to_display_affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            identity,
            cross_call_reusable: true,
            presentation_quality,
            decode_execution,
            residency_resource: None,
            residency_protection: None,
        }
    }

    pub(crate) fn from_source(
        source: MediaPreviewGpuSourceFrame,
        logical_resolution: Resolution,
        identity: PreviewSemanticIdentity,
        presentation_quality: FramePresentationQuality,
        decode_execution: PreviewDecodeExecutionSummary,
    ) -> Self {
        let descriptor = source.source.descriptor();
        Self {
            payload: MediaPreviewPayload::Source(source),
            sampled_resolution: Resolution { width: descriptor.width, height: descriptor.height },
            logical_resolution,
            source_to_display_affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            identity,
            cross_call_reusable: true,
            presentation_quality,
            decode_execution,
            residency_resource: None,
            residency_protection: None,
        }
    }

    pub(crate) fn from_native(
        source: MediaPreviewNativeSourceFrame,
        sampled_resolution: Resolution,
        logical_resolution: Resolution,
        identity: PreviewSemanticIdentity,
        presentation_quality: FramePresentationQuality,
        decode_execution: PreviewDecodeExecutionSummary,
    ) -> Self {
        Self {
            payload: MediaPreviewPayload::Native(source),
            sampled_resolution,
            logical_resolution,
            source_to_display_affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            identity,
            cross_call_reusable: true,
            presentation_quality,
            decode_execution,
            residency_resource: None,
            residency_protection: None,
        }
    }
    pub(crate) fn reserved_cpu_bytes(&self) -> usize {
        match &self.payload {
            MediaPreviewPayload::Working(frame) => {
                std::mem::size_of_val(frame.rgba_f32().data.as_slice())
            }
            MediaPreviewPayload::Source(source) => {
                let working_reservation = (self.sampled_resolution.width as usize)
                    .saturating_mul(self.sampled_resolution.height as usize)
                    .saturating_mul(4)
                    .saturating_mul(std::mem::size_of::<f32>());
                source.source.retained_bytes().saturating_add(working_reservation)
            }
            MediaPreviewPayload::Native(_) => 0,
        }
    }

    /// Bind resolved source picture geometry before Clip-local authoring transforms.
    pub(crate) fn with_picture_geometry(mut self, geometry: ResolvedPictureGeometry) -> Self {
        self.source_to_display_affine = geometry.source_to_display_affine();
        self
    }

    pub(crate) fn decoder_resource_units(&self) -> usize {
        usize::from(matches!(self.payload, MediaPreviewPayload::Native(_)))
    }

    /// Attach the Store-owned physical allocation shared by every frame clone.
    pub(crate) fn with_residency_resource(
        mut self,
        resource: mondrian_playback::MediaFrameResourceLease,
    ) -> Self {
        self.residency_resource = Some(resource);
        self
    }

    /// Strip caller-held residency before this payload enters Store ownership.
    ///
    /// The Store retains its allocation lease separately and reattaches a
    /// clone on fetch. Keeping a lease inside the cached payload would make the
    /// Store appear permanently non-exclusive and defeat bounded eviction.
    pub(crate) fn into_unbound_store_payload(mut self) -> Self {
        self.residency_resource = None;
        self.residency_protection = None;
        self
    }

    /// Attach Store-owned protection while this frame participates in a
    /// current Viewer candidate or GPU continuation.
    pub(crate) fn with_residency_protection(
        mut self,
        protection: mondrian_playback::MediaFrameProtectionLease,
    ) -> Self {
        self.residency_protection = Some(protection);
        self
    }

    /// Clone the protection carried by this current-frame payload.
    pub(crate) fn residency_protection(
        &self,
    ) -> Option<mondrian_playback::MediaFrameProtectionLease> {
        self.residency_protection.clone()
    }

    pub(crate) fn width(&self) -> u32 {
        self.sampled_resolution.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.sampled_resolution.height
    }

    pub(crate) fn logical_resolution(&self) -> Resolution {
        self.logical_resolution
    }

    /// Complete semantic identity of the decoded or generated source frame.
    pub(crate) fn identity(&self) -> PreviewSemanticIdentity {
        self.identity
    }

    /// Whether this frame may participate in semantic cross-call cache reuse.
    pub(crate) const fn permits_cross_call_reuse(&self) -> bool {
        self.cross_call_reusable
    }

    /// Restrict this frame to one concrete execution when an upstream nested
    /// plan contains stateful or explicitly uncacheable work.
    pub(crate) fn with_cross_call_reuse(mut self, reusable: bool) -> Self {
        self.cross_call_reusable = reusable;
        self
    }

    pub(crate) fn working_payload(&self) -> Option<CpuColorFrame> {
        match &self.payload {
            MediaPreviewPayload::Working(frame) => Some(frame.clone()),
            MediaPreviewPayload::Source(_) | MediaPreviewPayload::Native(_) => None,
        }
    }

    pub(crate) fn working_color_space(&self) -> Option<mondrian_core::WorkingColorSpace> {
        match &self.payload {
            MediaPreviewPayload::Working(frame) => frame.descriptor().color_space.working(),
            MediaPreviewPayload::Source(source) => Some(source.input_transform.working_color_space),
            MediaPreviewPayload::Native(source) => Some(source.input_transform.working_color_space),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_logical_resolution(&mut self, resolution: Resolution) {
        self.logical_resolution = resolution;
    }

    #[cfg(test)]
    pub(crate) fn set_presentation_quality(&mut self, quality: FramePresentationQuality) {
        self.presentation_quality = quality;
    }

    pub(crate) fn presentation_quality(&self) -> FramePresentationQuality {
        self.presentation_quality
    }

    pub(crate) fn decode_execution(&self) -> PreviewDecodeExecutionSummary {
        self.decode_execution
    }

    /// Physical PTS interval selected by the decoder for this media payload.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn temporal_selection(&self) -> Option<PreviewDecodeTemporalSelection> {
        match &self.payload {
            MediaPreviewPayload::Working(_) => None,
            MediaPreviewPayload::Source(source) => source.temporal_selection,
            MediaPreviewPayload::Native(source) => source.temporal_selection,
        }
    }

    pub(crate) fn gpu_source(&self) -> Option<ViewerGpuMediaSource> {
        let MediaPreviewPayload::Source(source) = &self.payload else {
            return None;
        };
        Some(ViewerGpuMediaSource {
            source: Arc::clone(&source.source),
            input_transform: source.input_transform.clone(),
            decoder_residency: source.decoder_residency,
            decoder_handle_kind: source.decoder_handle_kind,
            decoded_surface_format: source.decoded_surface_format,
            decoded_video_sampling: source.decoded_video_sampling,
        })
    }

    pub(crate) fn native_source(&self) -> Option<ViewerGpuNativeSource> {
        let MediaPreviewPayload::Native(source) = &self.payload else {
            return None;
        };
        Some(ViewerGpuNativeSource {
            source_color_space: source.source_color_space,
            input_transform: source.input_transform.clone(),
            materialization_width: self.sampled_resolution.width,
            materialization_height: self.sampled_resolution.height,
            native_frame: source.native_frame.clone(),
        })
    }

    pub(crate) fn working_frame_with_session(
        &self,
        color_session: &mut RenderCpuColorExecutionSession,
    ) -> Result<MediaPreviewWorkingFrame, MediaPreviewWorkingFrameError> {
        match &self.payload {
            MediaPreviewPayload::Working(frame) => Ok(MediaPreviewWorkingFrame {
                frame: frame.clone(),
                color_diagnostics: None,
                stage_diagnostics: RenderColorStageDiagnostics::default(),
            }),
            MediaPreviewPayload::Native(native) => {
                Err(MediaPreviewWorkingFrameError::NativeSurfaceRequiresGpu {
                    handle_kind: native.native_frame.handle_kind(),
                    surface_format: native.native_frame.surface_format,
                })
            }
            MediaPreviewPayload::Source(source) => {
                let cached_before = source.working_cache.get().is_some();
                let entry = source
                    .working_cache
                    .get_or_init(|| {
                        execute_cpu_source_input_stage_with_session(
                            source.source.as_ref(),
                            &source.input_transform,
                            color_session,
                        )
                        .map(|output| MediaPreviewWorkingFrameCacheEntry {
                            frame: output.result.frame,
                            color_diagnostics: output.result.diagnostics,
                            stage_diagnostics: output.stage_diagnostics,
                        })
                        .map_err(Arc::new)
                    })
                    .as_ref()
                    .map_err(
                        |source| MediaPreviewWorkingFrameError::InputColorTransform {
                            source: Arc::clone(source),
                        },
                    )?;
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
    }

    #[cfg(test)]
    pub(crate) fn working_frame(
        &self,
    ) -> Result<MediaPreviewWorkingFrame, MediaPreviewWorkingFrameError> {
        let mut session = RenderCpuColorExecutionSession::new(0);
        self.working_frame_with_session(&mut session)
    }
}

pub(crate) fn project_preview_media_transform(
    transform: [f32; 6],
    frame: &MediaPreviewFrame,
    output_authoring: Resolution,
    output_sampled: Resolution,
) -> Option<[f32; 6]> {
    let interpreted = compose_picture_affine(transform, frame.source_to_display_affine)?;
    project_affine_to_sampled_extents(
        interpreted,
        frame.logical_resolution(),
        Resolution { width: frame.width(), height: frame.height() },
        output_authoring,
        output_sampled,
    )
}

pub(crate) struct MediaPreviewWorkingFrame {
    pub(crate) frame: CpuColorFrame,
    pub(crate) color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) stage_diagnostics: RenderColorStageDiagnostics,
}

/// Failure to adapt a decoded Preview payload into a CPU working frame.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum MediaPreviewWorkingFrameError {
    #[error(
        "native decoded surface ({handle_kind:?} {surface_format:?}) requires renderer native import; no CPU working fallback exists"
    )]
    NativeSurfaceRequiresGpu {
        handle_kind: DecodedGpuFrameHandleKind,
        surface_format: DecodedVideoSurfaceFormat,
    },
    #[error("lazy Preview input color transform failed: {source}")]
    InputColorTransform {
        #[source]
        source: Arc<RenderColorTransformError>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct MediaPreviewGpuSourceFrame {
    pub(crate) source: Arc<CpuSourceColorFrame>,
    pub(crate) input_transform: RenderInputTransform,
    pub(crate) decoder_residency: DecodedFrameResidency,
    pub(crate) decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    pub(crate) decoded_surface_format: DecodedVideoSurfaceFormat,
    pub(crate) decoded_video_sampling: DecodedVideoSampling,
    #[cfg(any(test, feature = "validation"))]
    temporal_selection: Option<PreviewDecodeTemporalSelection>,
    working_cache:
        Arc<OnceLock<Result<MediaPreviewWorkingFrameCacheEntry, Arc<RenderColorTransformError>>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct MediaPreviewNativeSourceFrame {
    pub(crate) source_color_space: ColorSpace,
    pub(crate) input_transform: RenderInputTransform,
    pub(crate) native_frame: Arc<PreviewNativeDecodedFrame>,
    #[cfg(any(test, feature = "validation"))]
    temporal_selection: Option<PreviewDecodeTemporalSelection>,
}

impl MediaPreviewNativeSourceFrame {
    pub(crate) fn from_native_frame(
        native_frame: PreviewNativeDecodedFrame,
        source_color_space: ColorSpace,
        input_transform: RenderInputTransform,
    ) -> Self {
        #[cfg(any(test, feature = "validation"))]
        let temporal_selection = native_frame.diagnostics.temporal_selection();
        Self {
            source_color_space,
            input_transform,
            native_frame: Arc::new(native_frame),
            #[cfg(any(test, feature = "validation"))]
            temporal_selection,
        }
    }
}

impl MediaPreviewGpuSourceFrame {
    #[cfg(test)]
    pub(crate) fn new(
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
            #[cfg(any(test, feature = "validation"))]
            temporal_selection: None,
            working_cache: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn from_decode_diagnostics(
        source: impl Into<CpuSourceColorFrame>,
        input_transform: RenderInputTransform,
        diagnostics: PreviewDecodeDiagnostics,
    ) -> Self {
        #[cfg(any(test, feature = "validation"))]
        let temporal_selection = diagnostics.temporal_selection();
        Self {
            source: Arc::new(source.into()),
            input_transform,
            decoder_residency: diagnostics.decoded_frame_residency,
            decoder_handle_kind: diagnostics.gpu_frame_handle_kind,
            decoded_surface_format: diagnostics.decoded_surface_format,
            decoded_video_sampling: diagnostics.decoded_video_sampling,
            #[cfg(any(test, feature = "validation"))]
            temporal_selection,
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
