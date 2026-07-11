//! Stateful GPU resources for one Viewer preview execution context.
//!
//! Windowing is an Adapter concern. The resources below instead belong to the
//! Viewer GPU execution lifetime and must be shared by every production or
//! headless Adapter that executes the same preview path.

use std::sync::Arc;

use super::native_video_import::{
    native_source_texture_format_from_decoded, native_video_sampling_from_decoded,
    AppUiNativeVideoImportRuntime,
};
use super::preview::{
    AppUiGpuPreviewCompositeLayer, AppUiGpuPreviewFrame, AppUiGpuPreviewMediaSource,
    AppUiGpuPreviewNativeSource,
};
use mondrian_core::types::{BlendMode, Color};
use mondrian_media::{DecodedFrameResidency, DecodedGpuFrameHandleKind};
use mondrian_renderer::{
    CpuColorFrame, GpuColorFrameHandle, GpuColorFrameTextureFormat, GpuCompositeLayer,
    GpuCompositeLayerSource, GpuCompositeRequest, GpuCompositingDiagnostics,
    GpuDisplayCalibrationRuntime, GpuFrameCompositor, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling,
    GpuViewerSpatialRuntime, GpuViewerSpatialRuntimeDiagnostics, RenderColorStageDiagnostics,
    RenderColorTransformGpuOptions, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, RenderGpuOutputBoundaryRuntimeRecordError,
    ViewerSourceRect,
};
use mondrian_ui_renderer::ExternalTextureKey;
use mondrian_ui_widgets::ViewerExternalTexturePresentation;

/// Long-lived GPU state for a single Viewer preview execution context.
///
/// Frame resources are cleared between candidates; pipelines and backend
/// capabilities remain resident for the lifetime of this object. Fields are
/// temporarily visible to the sibling Window Adapter while execution is moved
/// behind this module's stable interface.
pub(crate) struct ViewerGpuPreviewRuntime {
    native_video_import: AppUiNativeVideoImportRuntime,
    color_output: RenderGpuOutputBoundaryRuntime,
    spatial: GpuViewerSpatialRuntime,
    display_calibration: GpuDisplayCalibrationRuntime,
    working_compositor: GpuFrameCompositor,
    registered_texture_key: Option<ExternalTextureKey>,
    presentation: Option<ViewerExternalTexturePresentation>,
}

impl ViewerGpuPreviewRuntime {
    /// Create one execution context for a renderer device.
    pub(crate) fn new(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            native_video_import: AppUiNativeVideoImportRuntime::new(adapter, device, queue),
            color_output: RenderGpuOutputBoundaryRuntime::default(),
            spatial: GpuViewerSpatialRuntime::default(),
            display_calibration: GpuDisplayCalibrationRuntime::default(),
            working_compositor: GpuFrameCompositor::new(device),
            registered_texture_key: None,
            presentation: None,
        }
    }

    /// Native decoder import capability exposed to preview scheduling.
    pub(crate) fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.native_video_import.support()
    }

    /// Aggregate output-stage diagnostics without exposing the resource table.
    pub(crate) fn color_output_diagnostics(
        &self,
    ) -> mondrian_renderer::RenderGpuOutputBoundaryRuntimeDiagnostics {
        self.color_output.diagnostics()
    }

    /// Release resources scoped to the current candidate, retaining pipelines.
    pub(crate) fn clear_frame_resources(&mut self) {
        self.color_output.clear_frame_resources();
        self.spatial.clear_frame_resources();
        self.display_calibration.clear_frame_resources();
    }

    /// Record one current Viewer frame through the shared GPU execution path.
    ///
    /// The returned handle remains owned by this runtime until the next frame
    /// clear/reset. Presentation registration and publication are Adapter work.
    pub(super) fn record_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &AppUiGpuPreviewFrame,
        layers: &[AppUiGpuPreviewCompositeLayer],
        presentation: ViewerExternalTexturePresentation,
        calibration: Option<Arc<mondrian_core::display_calibration::DisplayCalibrationLut3d>>,
    ) -> Result<ViewerGpuPreviewRecord, ViewerGpuPreviewRecordError> {
        let prepared = prepare_composite(
            frame,
            layers,
            &mut self.color_output,
            &mut self.native_video_import,
            device,
            queue,
            encoder,
        )?;
        let residency = prepared.residency;
        let fallback_reasons = prepared.fallback_reasons;
        let mut stage_diagnostics = prepared.input_stage_diagnostics;
        let gpu_layers = composite_layers(&prepared.layers, &prepared.gpu_input_handles);
        let composite = self
            .color_output
            .record_wgpu_working_composite(
                &self.working_compositor,
                device,
                queue,
                encoder,
                GpuCompositeRequest {
                    width: frame.width,
                    height: frame.height,
                    working_color_space: frame.working_color_space,
                    layers: &gpu_layers,
                },
            )
            .map_err(ViewerGpuPreviewRecordError::WorkingComposite)?;
        let working_view = self
            .color_output
            .frame_table()
            .get(&composite.output)
            .map_err(|error| {
                ViewerGpuPreviewRecordError::WorkingOutputMissing(format!("{error:?}"))
            })?
            .resource()
            .texture_view
            .clone();
        let source_rect = presentation.normalized_source_rect();
        let spatial_output = self
            .spatial
            .record(
                device,
                encoder,
                self.color_output.frame_ids_mut(),
                composite.output,
                &working_view,
                ViewerSourceRect {
                    x: source_rect.x,
                    y: source_rect.y,
                    width: source_rect.width,
                    height: source_rect.height,
                },
                presentation.output_width,
                presentation.output_height,
            )
            .map_err(|error| ViewerGpuPreviewRecordError::Spatial(error.to_string()))?;
        let spatial_diagnostics = self.spatial.diagnostics();
        let spatial_resource = self
            .spatial
            .take_output(&spatial_output)
            .ok_or(ViewerGpuPreviewRecordError::SpatialOutputMissing)?;
        self.color_output
            .frame_table_mut()
            .insert(spatial_resource)
            .map_err(|error| ViewerGpuPreviewRecordError::SpatialTransfer(format!("{error:?}")))?;
        let mut output_record = self
            .color_output
            .record_wgpu_output_boundary_gpu_frame_owned_backend(
                &frame.boundary,
                &spatial_output,
                if calibration.is_some() {
                    GpuColorFrameTextureFormat::Rgba16Float
                } else {
                    GpuColorFrameTextureFormat::Rgba8Unorm
                },
                RenderColorTransformGpuOptions::default(),
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device,
                    queue,
                    encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                },
            )
            .map_err(ViewerGpuPreviewRecordError::OutputBoundary)?;
        stage_diagnostics.accumulate(output_record.stage_diagnostics);
        output_record.stage_diagnostics = stage_diagnostics;
        let output = output_record.materialized.output;
        let (output, output_owner) = if let Some(calibration) = calibration {
            let output_view = self
                .color_output
                .frame_table()
                .get(&output)
                .map_err(|error| {
                    ViewerGpuPreviewRecordError::DisplayOutputMissing(format!("{error:?}"))
                })?
                .resource()
                .texture_view
                .clone();
            let calibrated = self
                .display_calibration
                .record(
                    device,
                    queue,
                    encoder,
                    output,
                    &output_view,
                    calibration,
                    GpuColorFrameTextureFormat::Rgba16Float,
                )
                .map_err(|error| ViewerGpuPreviewRecordError::Calibration(error.to_string()))?;
            (calibrated, ViewerGpuPreviewOutputOwner::DisplayCalibration)
        } else {
            (output, ViewerGpuPreviewOutputOwner::ColorOutput)
        };
        Ok(ViewerGpuPreviewRecord {
            output,
            output_owner,
            stage_diagnostics: output_record.stage_diagnostics,
            compositing_diagnostics: composite.diagnostics,
            spatial_diagnostics,
            residency,
            fallback_reasons,
        })
    }

    /// Resolve the recorded presentation texture without exposing resource tables.
    pub(super) fn output_texture_view(
        &self,
        record: &ViewerGpuPreviewRecord,
    ) -> Result<wgpu::TextureView, ViewerGpuPreviewRecordError> {
        match record.output_owner {
            ViewerGpuPreviewOutputOwner::ColorOutput => self
                .color_output
                .frame_table()
                .get(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .map_err(|error| {
                    ViewerGpuPreviewRecordError::DisplayOutputMissing(format!("{error:?}"))
                }),
            ViewerGpuPreviewOutputOwner::DisplayCalibration => self
                .display_calibration
                .output(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .ok_or_else(|| {
                    ViewerGpuPreviewRecordError::DisplayOutputMissing(
                        "calibrated output disappeared before presentation".to_owned(),
                    )
                }),
        }
    }

    /// Remove and return the texture registration owned by this context.
    ///
    /// The Adapter must unregister the returned key from its renderer before
    /// discarding or replacing the associated frame resources.
    pub(crate) fn take_registered_texture_key(&mut self) -> Option<ExternalTextureKey> {
        self.registered_texture_key.take()
    }

    /// Record the renderer registration owned by this execution context.
    pub(crate) fn set_registered_texture_key(&mut self, key: ExternalTextureKey) {
        self.registered_texture_key = Some(key);
    }

    /// Current spatial presentation identity, if one is active.
    pub(crate) fn presentation(&self) -> Option<ViewerExternalTexturePresentation> {
        self.presentation
    }

    /// Replace the active spatial presentation identity.
    pub(crate) fn set_presentation(&mut self, presentation: ViewerExternalTexturePresentation) {
        self.presentation = Some(presentation);
    }

    /// Clear and report whether a spatial presentation was active.
    pub(crate) fn take_presentation(&mut self) -> bool {
        self.presentation.take().is_some()
    }

    /// Reset all retained execution resources after a device/surface transition.
    pub(crate) fn reset(&mut self) {
        debug_assert!(
            self.registered_texture_key.is_none(),
            "renderer registration must be released before resetting Viewer GPU resources"
        );
        self.color_output.clear_frame_resources();
        self.spatial.clear();
        self.display_calibration.clear();
        self.registered_texture_key = None;
        self.presentation = None;
    }
}

/// Successful GPU recording evidence consumed by presentation Adapters.
pub(super) struct ViewerGpuPreviewRecord {
    pub(super) output: GpuColorFrameHandle,
    output_owner: ViewerGpuPreviewOutputOwner,
    pub(super) stage_diagnostics: RenderColorStageDiagnostics,
    pub(super) compositing_diagnostics: GpuCompositingDiagnostics,
    pub(super) spatial_diagnostics: GpuViewerSpatialRuntimeDiagnostics,
    pub(super) residency: ViewerGpuPreviewResidencyFacts,
    pub(super) fallback_reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerGpuPreviewOutputOwner {
    ColorOutput,
    DisplayCalibration,
}

/// Stage-specific failures from the shared Viewer GPU execution Interface.
#[derive(Debug, thiserror::Error)]
pub(super) enum ViewerGpuPreviewRecordError {
    #[error("Viewer GPU input preparation failed: {0}")]
    InputPreparation(String),
    #[error("Viewer GPU working composite failed: {0:?}")]
    WorkingComposite(mondrian_renderer::GpuCompositeError),
    #[error("Viewer GPU working output is missing: {0}")]
    WorkingOutputMissing(String),
    #[error("Viewer GPU spatial processing failed: {0}")]
    Spatial(String),
    #[error("Viewer GPU spatial output disappeared before the display boundary")]
    SpatialOutputMissing,
    #[error("Viewer GPU spatial resource transfer failed: {0}")]
    SpatialTransfer(String),
    #[error("Viewer GPU display output boundary failed: {0:?}")]
    OutputBoundary(RenderGpuOutputBoundaryRuntimeRecordError),
    #[error("Viewer GPU display output is missing: {0}")]
    DisplayOutputMissing(String),
    #[error("Viewer GPU display calibration failed: {0}")]
    Calibration(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ViewerGpuPreviewResidencyFacts {
    pub(super) media_layers: u32,
    pub(super) procedural_layers: u32,
    pub(super) native_decoder_gpu_layers: u32,
    pub(super) gpu_input_layers: u32,
    pub(super) cpu_upload_layers: u32,
    pub(super) gpu_input_failures: u32,
    pub(super) native_video_import: Option<ViewerGpuPreviewNativeVideoFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ViewerGpuPreviewNativeVideoFacts {
    pub(super) decoder_residency: DecodedFrameResidency,
    pub(super) decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    pub(super) source_texture_format: Option<GpuNativeDecodedFrameTextureFormat>,
    pub(super) source_video_sampling: Option<GpuNativeDecodedFrameVideoSampling>,
}

impl Default for ViewerGpuPreviewNativeVideoFacts {
    fn default() -> Self {
        Self {
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            source_texture_format: None,
            source_video_sampling: None,
        }
    }
}

struct PreparedComposite<'a> {
    gpu_input_handles: Vec<GpuColorFrameHandle>,
    layers: Vec<PreparedCompositeLayer<'a>>,
    residency: ViewerGpuPreviewResidencyFacts,
    input_stage_diagnostics: RenderColorStageDiagnostics,
    fallback_reasons: Vec<String>,
}

struct PreparedCompositeLayer<'a> {
    source: PreparedCompositeLayerSource<'a>,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_plan: Option<&'a mondrian_effects::CompiledEffectGpuPlan>,
    frame_seed: i64,
}

enum PreparedCompositeLayerSource<'a> {
    CpuFrame(&'a CpuColorFrame),
    GpuFrame(usize),
    SolidColor(Color),
    Adjustment,
}

fn prepare_composite<'a>(
    preview_frame: &AppUiGpuPreviewFrame,
    layers: &'a [AppUiGpuPreviewCompositeLayer],
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    native_runtime: &mut AppUiNativeVideoImportRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<PreparedComposite<'a>, ViewerGpuPreviewRecordError> {
    let mut prepared = PreparedComposite {
        gpu_input_handles: Vec::new(),
        layers: Vec::with_capacity(layers.len()),
        residency: ViewerGpuPreviewResidencyFacts::default(),
        input_stage_diagnostics: RenderColorStageDiagnostics::default(),
        fallback_reasons: Vec::new(),
    };

    for layer in layers {
        match layer {
            AppUiGpuPreviewCompositeLayer::Media {
                frame,
                gpu_source,
                native_source,
                opacity,
                transform,
                effect_plan,
                frame_seed,
            } => {
                prepared.residency.media_layers = prepared.residency.media_layers.saturating_add(1);
                prepared.residency.record_source(gpu_source.as_ref(), native_source.as_ref());
                let native_handle = match native_source.as_ref() {
                    Some(source) => {
                        match record_native_video_layer(source, native_runtime, runtime) {
                            Ok(handle) => Some(handle),
                            Err(error) => {
                                prepared.residency.gpu_input_failures =
                                    prepared.residency.gpu_input_failures.saturating_add(1);
                                prepared
                                    .fallback_reasons
                                    .push(format!("viewer native video import failed: {error}"));
                                tracing::warn!(
                                    sequence_id = %preview_frame.sequence_id,
                                    frame = preview_frame.frame,
                                    width = preview_frame.width,
                                    height = preview_frame.height,
                                    "viewer native video import failed: {error}"
                                );
                                None
                            }
                        }
                    }
                    None => None,
                };
                let source = if let Some(handle) = native_handle {
                    let index = prepared.gpu_input_handles.len();
                    prepared.gpu_input_handles.push(handle);
                    PreparedCompositeLayerSource::GpuFrame(index)
                } else {
                    match gpu_source.as_ref() {
                        Some(source) => {
                            match record_gpu_input_layer(source, runtime, device, queue, encoder) {
                                Ok(record) => {
                                    prepared
                                        .input_stage_diagnostics
                                        .accumulate(record.stage_diagnostics);
                                    let index = prepared.gpu_input_handles.len();
                                    prepared.gpu_input_handles.push(record.materialized.output);
                                    prepared.residency.gpu_input_layers =
                                        prepared.residency.gpu_input_layers.saturating_add(1);
                                    PreparedCompositeLayerSource::GpuFrame(index)
                                }
                                Err(error) => {
                                    prepared.residency.gpu_input_failures =
                                        prepared.residency.gpu_input_failures.saturating_add(1);
                                    prepared.fallback_reasons.push(format!(
                                        "viewer GPU input transform failed: {error:?}"
                                    ));
                                    if let Some(frame) = frame.as_ref() {
                                        prepared.residency.cpu_upload_layers =
                                            prepared.residency.cpu_upload_layers.saturating_add(1);
                                        tracing::warn!(
                                            sequence_id = %preview_frame.sequence_id,
                                            frame = preview_frame.frame,
                                            width = preview_frame.width,
                                            height = preview_frame.height,
                                            "viewer GPU input transform failed; using CPU working layer upload: {error:?}"
                                        );
                                        PreparedCompositeLayerSource::CpuFrame(frame)
                                    } else {
                                        return Err(ViewerGpuPreviewRecordError::InputPreparation(
                                        format!(
                                            "GPU input transform failed without a CPU working fallback: {error:?}"
                                        ),
                                    ));
                                    }
                                }
                            }
                        }
                        None => {
                            let Some(frame) = frame.as_ref() else {
                                let reason = native_source.as_ref().map_or_else(
                                    || "media layer has no GPU source or CPU working fallback".to_owned(),
                                    |source| format!(
                                        "native GPU import failed for {} {:?} without a CPU working fallback",
                                        source.native_frame.handle_kind().as_str(),
                                        source.native_frame.surface_format
                                    ),
                                );
                                return Err(ViewerGpuPreviewRecordError::InputPreparation(reason));
                            };
                            prepared.residency.cpu_upload_layers =
                                prepared.residency.cpu_upload_layers.saturating_add(1);
                            PreparedCompositeLayerSource::CpuFrame(frame)
                        }
                    }
                };
                prepared.layers.push(PreparedCompositeLayer {
                    source,
                    opacity: *opacity,
                    blend_mode: BlendMode::Normal,
                    transform: *transform,
                    effect_plan: Some(effect_plan),
                    frame_seed: *frame_seed,
                });
            }
            AppUiGpuPreviewCompositeLayer::SolidColor { layer, effect_plan } => {
                prepared.residency.procedural_layers =
                    prepared.residency.procedural_layers.saturating_add(1);
                prepared.layers.push(PreparedCompositeLayer {
                    source: PreparedCompositeLayerSource::SolidColor(layer.color),
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: layer.transform,
                    effect_plan: Some(effect_plan),
                    frame_seed: layer.frame_seed,
                });
            }
            AppUiGpuPreviewCompositeLayer::Adjustment {
                effect_plan,
                opacity,
                blend_mode,
                frame_seed,
            } => prepared.layers.push(PreparedCompositeLayer {
                source: PreparedCompositeLayerSource::Adjustment,
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Some(effect_plan),
                frame_seed: *frame_seed,
            }),
        }
    }

    Ok(prepared)
}

fn record_native_video_layer(
    source: &AppUiGpuPreviewNativeSource,
    native_runtime: &mut AppUiNativeVideoImportRuntime,
    color_runtime: &mut RenderGpuOutputBoundaryRuntime,
) -> Result<GpuColorFrameHandle, String> {
    let resource = native_runtime.import(
        color_runtime.frame_ids_mut(),
        source.source_color_space,
        &source.input_transform,
        &source.native_frame,
    )?;
    let handle = resource.handle().clone();
    if color_runtime
        .frame_table_mut()
        .insert(resource)
        .map_err(|error| format!("native working resource insertion failed: {error:?}"))?
        .is_some()
    {
        return Err("native working frame unexpectedly replaced a live resource".to_owned());
    }
    Ok(handle)
}

fn record_gpu_input_layer(
    source: &AppUiGpuPreviewMediaSource,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<
    mondrian_renderer::RenderGpuInputStageRecord,
    mondrian_renderer::RenderGpuInputStageRuntimeRecordError,
> {
    runtime.record_wgpu_input_stage_owned_backend(
        &source.input_transform,
        &source.source,
        RenderColorTransformGpuOptions::default(),
        RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
            device,
            queue,
            encoder,
            load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
        },
    )
}

fn composite_layers<'a>(
    layers: &'a [PreparedCompositeLayer<'a>],
    gpu_input_handles: &'a [GpuColorFrameHandle],
) -> Vec<GpuCompositeLayer<'a>> {
    layers
        .iter()
        .map(|layer| GpuCompositeLayer {
            source: match layer.source {
                PreparedCompositeLayerSource::CpuFrame(frame) => {
                    GpuCompositeLayerSource::CpuFrame(frame)
                }
                PreparedCompositeLayerSource::GpuFrame(index) => {
                    GpuCompositeLayerSource::GpuFrame(&gpu_input_handles[index])
                }
                PreparedCompositeLayerSource::SolidColor(color) => {
                    GpuCompositeLayerSource::SolidColor(color)
                }
                PreparedCompositeLayerSource::Adjustment => GpuCompositeLayerSource::Adjustment,
            },
            opacity: layer.opacity,
            blend_mode: layer.blend_mode,
            transform: layer.transform,
            effect_plan: layer.effect_plan,
            frame_seed: layer.frame_seed,
        })
        .collect()
}

impl ViewerGpuPreviewResidencyFacts {
    fn record_source(
        &mut self,
        media_source: Option<&AppUiGpuPreviewMediaSource>,
        native_source: Option<&AppUiGpuPreviewNativeSource>,
    ) {
        let facts = native_source
            .map(ViewerGpuPreviewNativeVideoFacts::from_native_source)
            .or_else(|| media_source.map(ViewerGpuPreviewNativeVideoFacts::from_media_source))
            .unwrap_or_default();
        if facts.decoder_residency == DecodedFrameResidency::GpuTexture {
            self.native_decoder_gpu_layers = self.native_decoder_gpu_layers.saturating_add(1);
        }
        let should_replace = self
            .native_video_import
            .map(|current| {
                current.decoder_residency != DecodedFrameResidency::GpuTexture
                    && facts.decoder_residency == DecodedFrameResidency::GpuTexture
            })
            .unwrap_or(true);
        if should_replace {
            self.native_video_import = Some(facts);
        }
    }
}

impl ViewerGpuPreviewNativeVideoFacts {
    fn from_media_source(source: &AppUiGpuPreviewMediaSource) -> Self {
        let source_texture_format = (source.decoder_residency == DecodedFrameResidency::GpuTexture)
            .then(|| native_source_texture_format_from_decoded(source.decoded_surface_format))
            .flatten();
        let source_video_sampling = source_texture_format.and_then(|format| {
            source.source.descriptor().color_space.encoded().and_then(|encoded| {
                native_video_sampling_from_decoded(encoded, format, source.decoded_video_sampling)
            })
        });
        Self {
            decoder_residency: source.decoder_residency,
            decoder_handle_kind: source.decoder_handle_kind,
            source_texture_format,
            source_video_sampling,
        }
    }

    fn from_native_source(source: &AppUiGpuPreviewNativeSource) -> Self {
        let source_texture_format =
            native_source_texture_format_from_decoded(source.native_frame.surface_format);
        let source_video_sampling = source_texture_format.and_then(|format| {
            native_video_sampling_from_decoded(
                source.source_color_space,
                format,
                source.native_frame.diagnostics.decoded_video_sampling,
            )
        });
        Self {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(source.native_frame.handle_kind()),
            source_texture_format,
            source_video_sampling,
        }
    }
}
