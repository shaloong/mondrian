//! Stateful GPU resources for one Viewer preview execution context.
//!
//! Windowing is an Adapter concern. The resources below instead belong to the
//! Viewer GPU execution lifetime and must be shared by every production or
//! headless Adapter that executes the same preview path.

use std::sync::Arc;

use crate::{
    native_source_texture_format_from_decoded, native_video_sampling_from_decoded, CpuColorFrame,
    GpuColorFrameHandle, GpuColorFrameTextureFormat, GpuCompositeLayer, GpuCompositeLayerSource,
    GpuCompositeRequest, GpuCompositingDiagnostics, GpuDisplayCalibrationRuntime,
    GpuFrameCompositor, GpuNativeDecodedFrameImportSupport, GpuNativeDecodedFrameTextureFormat,
    GpuNativeDecodedFrameVideoSampling, GpuViewerSpatialRuntime,
    GpuViewerSpatialRuntimeDiagnostics, RenderColorStageDiagnostics,
    RenderColorTransformGpuOptions, RenderGpuInputStageRecord,
    RenderGpuInputStageRuntimeRecordError, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, RenderGpuOutputBoundaryRuntimeRecordError,
    RenderOutputColorBoundary, ViewerGpuExecutionLayer, ViewerGpuMediaSource,
    ViewerGpuNativeSource, ViewerNativeVideoImportRuntime, ViewerSourceRect,
};
use mondrian_core::display_calibration::DisplayCalibrationLut3d;
use mondrian_core::types::{BlendMode, Color, SequenceId};
use mondrian_core::WorkingColorSpace;
use mondrian_media::{DecodedFrameResidency, DecodedGpuFrameHandleKind};

/// Immutable renderer input for one Viewer GPU execution.
pub struct ViewerGpuExecutionRequest<'a> {
    /// Sequence identity used only to correlate renderer diagnostics.
    pub sequence_id: SequenceId,
    /// Timeline frame used only to correlate renderer diagnostics.
    pub timeline_frame: i64,
    /// Working-frame width before Viewer crop and resize.
    pub width: u32,
    /// Working-frame height before Viewer crop and resize.
    pub height: u32,
    /// Timeline working color space represented by all input layers.
    pub working_color_space: WorkingColorSpace,
    /// Bottom-to-top layer stack entering working-linear compositing.
    pub layers: &'a [ViewerGpuExecutionLayer],
    /// Exact display/output transform to execute after spatial processing.
    pub output_boundary: &'a RenderOutputColorBoundary,
    /// Normalized crop in the working composite.
    pub source_rect: ViewerSourceRect,
    /// Output width after Viewer spatial processing.
    pub output_width: u32,
    /// Output height after Viewer spatial processing.
    pub output_height: u32,
    /// Optional proven display calibration applied after the output boundary.
    pub display_calibration: Option<Arc<DisplayCalibrationLut3d>>,
}

/// Long-lived GPU state for a single Viewer preview execution context.
///
/// Frame resources are cleared between candidates; pipelines and backend
/// capabilities remain resident for the lifetime of this object. Fields are
/// temporarily visible to the sibling Window Adapter while execution is moved
/// behind this module's stable interface.
pub struct ViewerGpuExecutionRuntime {
    native_video_import: ViewerNativeVideoImportRuntime,
    color_output: RenderGpuOutputBoundaryRuntime,
    spatial: GpuViewerSpatialRuntime,
    display_calibration: GpuDisplayCalibrationRuntime,
    working_compositor: GpuFrameCompositor,
}

impl ViewerGpuExecutionRuntime {
    /// Create one execution context for a renderer device.
    pub fn new(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            native_video_import: ViewerNativeVideoImportRuntime::new(adapter, device, queue),
            color_output: RenderGpuOutputBoundaryRuntime::default(),
            spatial: GpuViewerSpatialRuntime::default(),
            display_calibration: GpuDisplayCalibrationRuntime::default(),
            working_compositor: GpuFrameCompositor::new(device),
        }
    }

    /// Native decoder import capability exposed to preview scheduling.
    pub fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.native_video_import.support()
    }

    /// Aggregate output-stage diagnostics without exposing the resource table.
    pub fn color_output_diagnostics(&self) -> crate::RenderGpuOutputBoundaryRuntimeDiagnostics {
        self.color_output.diagnostics()
    }

    /// Release resources scoped to the current candidate, retaining pipelines.
    pub fn clear_frame_resources(&mut self) {
        self.color_output.clear_frame_resources();
        self.spatial.clear_frame_resources();
        self.display_calibration.clear_frame_resources();
    }

    /// Record one current Viewer frame through the shared GPU execution path.
    ///
    /// The returned handle remains owned by this runtime until the next frame
    /// clear/reset. Presentation registration and publication are Adapter work.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: ViewerGpuExecutionRequest<'_>,
    ) -> Result<ViewerGpuExecutionRecord, ViewerGpuExecutionError> {
        let prepared = prepare_composite(
            &request,
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
                    width: request.width,
                    height: request.height,
                    working_color_space: request.working_color_space,
                    layers: &gpu_layers,
                },
            )
            .map_err(ViewerGpuExecutionError::WorkingComposite)?;
        let working_view = self
            .color_output
            .frame_table()
            .get(&composite.output)
            .map_err(|error| ViewerGpuExecutionError::WorkingOutputMissing(format!("{error:?}")))?
            .resource()
            .texture_view
            .clone();
        let spatial_output = self
            .spatial
            .record(
                device,
                encoder,
                self.color_output.frame_ids_mut(),
                composite.output,
                &working_view,
                request.source_rect,
                request.output_width,
                request.output_height,
            )
            .map_err(|error| ViewerGpuExecutionError::Spatial(error.to_string()))?;
        let spatial_diagnostics = self.spatial.diagnostics();
        let spatial_resource = self
            .spatial
            .take_output(&spatial_output)
            .ok_or(ViewerGpuExecutionError::SpatialOutputMissing)?;
        self.color_output
            .frame_table_mut()
            .insert(spatial_resource)
            .map_err(|error| ViewerGpuExecutionError::SpatialTransfer(format!("{error:?}")))?;
        let mut output_record = self
            .color_output
            .record_wgpu_output_boundary_gpu_frame_owned_backend(
                request.output_boundary,
                &spatial_output,
                if request.display_calibration.is_some() {
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
            .map_err(ViewerGpuExecutionError::OutputBoundary)?;
        stage_diagnostics.accumulate(output_record.stage_diagnostics);
        output_record.stage_diagnostics = stage_diagnostics;
        let output = output_record.materialized.output;
        let (output, output_owner) = if let Some(calibration) = request.display_calibration {
            let output_view = self
                .color_output
                .frame_table()
                .get(&output)
                .map_err(|error| {
                    ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
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
                .map_err(|error| ViewerGpuExecutionError::Calibration(error.to_string()))?;
            (
                calibrated,
                ViewerGpuExecutionOutputOwner::DisplayCalibration,
            )
        } else {
            (output, ViewerGpuExecutionOutputOwner::ColorOutput)
        };
        Ok(ViewerGpuExecutionRecord {
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
    pub fn output_texture_view(
        &self,
        record: &ViewerGpuExecutionRecord,
    ) -> Result<wgpu::TextureView, ViewerGpuExecutionError> {
        match record.output_owner {
            ViewerGpuExecutionOutputOwner::ColorOutput => self
                .color_output
                .frame_table()
                .get(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .map_err(|error| {
                    ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
                }),
            ViewerGpuExecutionOutputOwner::DisplayCalibration => self
                .display_calibration
                .output(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .ok_or_else(|| {
                    ViewerGpuExecutionError::DisplayOutputMissing(
                        "calibrated output disappeared before presentation".to_owned(),
                    )
                }),
        }
    }

    /// Reset all retained execution resources after a device/surface transition.
    pub fn reset(&mut self) {
        self.color_output.clear_frame_resources();
        self.spatial.clear();
        self.display_calibration.clear();
    }
}

/// Successful GPU recording evidence consumed by presentation Adapters.
pub struct ViewerGpuExecutionRecord {
    /// Renderer-owned output handle retained until frame resources clear.
    pub output: GpuColorFrameHandle,
    output_owner: ViewerGpuExecutionOutputOwner,
    /// Accumulated input and output color-stage diagnostics.
    pub stage_diagnostics: RenderColorStageDiagnostics,
    /// Working compositor execution diagnostics.
    pub compositing_diagnostics: GpuCompositingDiagnostics,
    /// Viewer crop/resize execution diagnostics.
    pub spatial_diagnostics: GpuViewerSpatialRuntimeDiagnostics,
    /// Exact media residency used for this execution.
    pub residency: ViewerGpuExecutionResidency,
    /// Explicit reasons for native/GPU-input correctness fallbacks.
    pub fallback_reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerGpuExecutionOutputOwner {
    ColorOutput,
    DisplayCalibration,
}

/// Stage-specific failures from the shared Viewer GPU execution Interface.
#[derive(Debug, thiserror::Error)]
pub enum ViewerGpuExecutionError {
    #[error("Viewer GPU input preparation failed: {0}")]
    InputPreparation(String),
    #[error("Viewer GPU working composite failed: {0:?}")]
    WorkingComposite(crate::GpuCompositeError),
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
pub struct ViewerGpuExecutionResidency {
    /// Media layer count.
    pub media_layers: u32,
    /// Procedural layer count.
    pub procedural_layers: u32,
    /// Media layers backed by native decoder GPU surfaces.
    pub native_decoder_gpu_layers: u32,
    /// Media layers transformed through the GPU input path.
    pub gpu_input_layers: u32,
    /// Media layers uploaded from CPU working frames.
    pub cpu_upload_layers: u32,
    /// Native/GPU-input attempts that fell back.
    pub gpu_input_failures: u32,
    /// Sampling and residency facts for native-video admission evidence.
    pub native_video_import: Option<ViewerGpuNativeVideoFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerGpuNativeVideoFacts {
    /// Actual decoder output residency.
    pub decoder_residency: DecodedFrameResidency,
    /// Native handle family if retained by media.
    pub decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Renderer import texture format derived from media facts.
    pub source_texture_format: Option<GpuNativeDecodedFrameTextureFormat>,
    /// Fail-closed renderer sampling contract derived from media facts.
    pub source_video_sampling: Option<GpuNativeDecodedFrameVideoSampling>,
}

impl Default for ViewerGpuNativeVideoFacts {
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
    residency: ViewerGpuExecutionResidency,
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
    request: &ViewerGpuExecutionRequest<'a>,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<PreparedComposite<'a>, ViewerGpuExecutionError> {
    let mut prepared = PreparedComposite {
        gpu_input_handles: Vec::new(),
        layers: Vec::with_capacity(request.layers.len()),
        residency: ViewerGpuExecutionResidency::default(),
        input_stage_diagnostics: RenderColorStageDiagnostics::default(),
        fallback_reasons: Vec::new(),
    };

    for layer in request.layers {
        match layer {
            ViewerGpuExecutionLayer::Media {
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
                let mut native_import_error = None;
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
                                    sequence_id = %request.sequence_id,
                                    frame = request.timeline_frame,
                                    width = request.width,
                                    height = request.height,
                                    "viewer native video import failed: {error}"
                                );
                                native_import_error = Some(error);
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
                                            sequence_id = %request.sequence_id,
                                            frame = request.timeline_frame,
                                            width = request.width,
                                            height = request.height,
                                            "viewer GPU input transform failed; using CPU working layer upload: {error:?}"
                                        );
                                        PreparedCompositeLayerSource::CpuFrame(frame)
                                    } else {
                                        return Err(ViewerGpuExecutionError::InputPreparation(
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
                                    || {
                                        "media layer has no GPU source or CPU working fallback"
                                            .to_owned()
                                    },
                                    |source| {
                                        native_import_failure_without_cpu_fallback(
                                            source.native_frame.handle_kind(),
                                            source.native_frame.surface_format,
                                            native_import_error.as_deref(),
                                        )
                                    },
                                );
                                return Err(ViewerGpuExecutionError::InputPreparation(reason));
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
            ViewerGpuExecutionLayer::SolidColor { layer, effect_plan } => {
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
            ViewerGpuExecutionLayer::Adjustment {
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

fn native_import_failure_without_cpu_fallback(
    handle_kind: mondrian_media::DecodedGpuFrameHandleKind,
    surface_format: mondrian_media::DecodedVideoSurfaceFormat,
    error: Option<&str>,
) -> String {
    format!(
        "native GPU import failed for {} {surface_format:?} without a CPU working fallback: {}",
        handle_kind.as_str(),
        error.unwrap_or("native import returned no error detail")
    )
}

fn record_native_video_layer(
    source: &ViewerGpuNativeSource,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
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
    source: &ViewerGpuMediaSource,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<RenderGpuInputStageRecord, RenderGpuInputStageRuntimeRecordError> {
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

impl ViewerGpuExecutionResidency {
    fn record_source(
        &mut self,
        media_source: Option<&ViewerGpuMediaSource>,
        native_source: Option<&ViewerGpuNativeSource>,
    ) {
        let facts = native_source
            .map(ViewerGpuNativeVideoFacts::from_native_source)
            .or_else(|| media_source.map(ViewerGpuNativeVideoFacts::from_media_source))
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

impl ViewerGpuNativeVideoFacts {
    fn from_media_source(source: &ViewerGpuMediaSource) -> Self {
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

    fn from_native_source(source: &ViewerGpuNativeSource) -> Self {
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

#[cfg(test)]
mod tests {
    use super::native_import_failure_without_cpu_fallback;
    use mondrian_media::{DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat};

    #[test]
    fn terminal_native_import_failure_preserves_backend_error_detail() {
        let message = native_import_failure_without_cpu_fallback(
            DecodedGpuFrameHandleKind::D3D11Texture2D,
            DecodedVideoSurfaceFormat::P010,
            Some("adapter LUID mismatch"),
        );

        assert!(message.contains("D3D11Texture2D P010"));
        assert!(message.contains("adapter LUID mismatch"));
    }
}
