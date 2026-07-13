//! Headless Viewer GPU presentation Adapter used by execution/performance gates.
//!
//! This Adapter owns a real wgpu device and calls the production
//! [`ViewerGpuExecutionRuntime`] Interface. It deliberately has no UI texture
//! registry; successful completion means commands were submitted and the GPU
//! queue reached the recorded presentation output.

use std::time::Instant;

use super::preview::{
    AppUiGpuPreviewFrame, AppUiGpuPreviewWorkingInput, AppUiPreviewDecodeExecutionSummary,
};
use mondrian_renderer::{
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    profile::gpu_timestamp_query_device_features,
    profile::{
        GpuTimestampQueryRing, GpuTimestampSample, GpuTimestampStageMarker, GpuTimestampToken,
    },
    request_adapter_with_native_video_preference, GpuCompositingDiagnostics,
    GpuNativeDecodedFrameImportSupport, GpuViewerSpatialRuntimeDiagnostics,
    RenderColorStageDiagnostics, ViewerGpuExecutionCpuStageTimings, ViewerGpuExecutionGpuStage,
    ViewerGpuExecutionRequest, ViewerGpuExecutionRuntime, ViewerGpuExecutionStageMarker,
    ViewerSourceRect,
};
use mondrian_ui_widgets::ViewerExternalTexturePresentation;

const HEADLESS_GPU_TIMESTAMP_RING_CAPACITY: usize = 16;

/// Evidence for one real headless Viewer GPU execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadlessViewerGpuExecution {
    /// Exact output width submitted to the shared Viewer GPU Runtime.
    pub output_width: u32,
    /// Exact output height submitted to the shared Viewer GPU Runtime.
    pub output_height: u32,
    /// Whether the runtime already retained this exact output.
    pub cached: bool,
    /// Wall time spent recording and submitting without a per-frame GPU wait.
    pub duration_us: u64,
    /// CPU wall time through command recording and queue submission.
    pub record_submit_us: u64,
    /// CPU wall time waiting for submitted work; always zero on this async path.
    pub completion_wait_us: u64,
    /// Deferred timestamp token, absent if unsupported or the bounded ring discarded it.
    pub gpu_timestamp_token: Option<u64>,
    /// CPU attribution inside the renderer record call.
    pub cpu_stage_timings: Option<ViewerGpuExecutionCpuStageTimings>,
    /// Frame-local working-space compositing evidence.
    pub compositing_diagnostics: Option<GpuCompositingDiagnostics>,
    /// Cumulative spatial-runtime evidence after this frame.
    pub spatial_diagnostics: Option<GpuViewerSpatialRuntimeDiagnostics>,
    /// Structured GPU color-stage evidence for a newly rendered output.
    pub stage_diagnostics: Option<RenderColorStageDiagnostics>,
    /// Explicit native/GPU-input fallback reasons for a newly rendered output.
    pub fallback_reasons: Vec<String>,
    /// Frame-local decode provenance bound to this exact Viewer candidate.
    pub decode_execution: AppUiPreviewDecodeExecutionSummary,
}

/// Stable adapter identity serialized by real-GPU execution gates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HeadlessViewerGpuAdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
    pub driver_info: String,
}

/// Real no-Surface Adapter over the shared Viewer GPU Preview Runtime.
pub(crate) struct HeadlessViewerGpuAdapter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    runtime: ViewerGpuExecutionRuntime,
    timestamp_ring: Option<GpuTimestampQueryRing>,
    adapter_info: HeadlessViewerGpuAdapterInfo,
    current_output_key: Option<String>,
}

struct HeadlessGpuStageMarker<'a> {
    ring: &'a mut GpuTimestampQueryRing,
    token: GpuTimestampToken,
}

impl ViewerGpuExecutionStageMarker for HeadlessGpuStageMarker<'_> {
    fn mark(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stage: ViewerGpuExecutionGpuStage,
    ) -> Result<(), String> {
        let marker = match stage {
            ViewerGpuExecutionGpuStage::WorkingComposite => {
                GpuTimestampStageMarker::AfterWorkingComposite
            }
            ViewerGpuExecutionGpuStage::Spatial => GpuTimestampStageMarker::AfterSpatial,
            ViewerGpuExecutionGpuStage::OutputBoundary => {
                GpuTimestampStageMarker::AfterOutputBoundary
            }
        };
        self.ring
            .mark_stage(encoder, self.token, marker)
            .map_err(|error| error.to_string())
    }
}

impl HeadlessViewerGpuAdapter {
    /// Create a high-performance headless device with the same native-video
    /// feature selection used by the production Window Adapter.
    pub(crate) fn new() -> Result<Self, HeadlessViewerGpuError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(request_adapter_with_native_video_preference(
            &instance,
            &wgpu::RequestAdapterOptions {
                compatible_surface: None,
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                ..wgpu::RequestAdapterOptions::default()
            },
        ))
        .map_err(|error| HeadlessViewerGpuError::Adapter(error.to_string()))?;
        let supported_features = adapter.features();
        let raw_adapter_info = adapter.get_info();
        let descriptor = wgpu::DeviceDescriptor {
            required_features: native_video_texture_device_features(supported_features)
                | ocio_lut_filtering_device_features(supported_features)
                | gpu_timestamp_query_device_features(supported_features),
            ..wgpu::DeviceDescriptor::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(|error| HeadlessViewerGpuError::Device(error.to_string()))?;
        let runtime = ViewerGpuExecutionRuntime::new(&adapter, &device, &queue);
        let timestamp_ring =
            GpuTimestampQueryRing::new(&device, &queue, HEADLESS_GPU_TIMESTAMP_RING_CAPACITY);
        let adapter_info = HeadlessViewerGpuAdapterInfo {
            name: raw_adapter_info.name,
            vendor: raw_adapter_info.vendor,
            device: raw_adapter_info.device,
            device_type: format!("{:?}", raw_adapter_info.device_type),
            backend: format!("{:?}", raw_adapter_info.backend),
            driver: raw_adapter_info.driver,
            driver_info: raw_adapter_info.driver_info,
        };
        Ok(Self {
            device,
            queue,
            runtime,
            timestamp_ring,
            adapter_info,
            current_output_key: None,
        })
    }

    /// Adapter identity bound to this execution device.
    pub(crate) fn adapter_info(&self) -> &HeadlessViewerGpuAdapterInfo {
        &self.adapter_info
    }

    /// Native import support exposed to the preview scheduling Adapter.
    pub(crate) fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.runtime.native_import_support()
    }

    /// Whether a previously completed output can remain visible as stale/repeat.
    pub(crate) fn has_presented_output(&self) -> bool {
        self.current_output_key.is_some()
    }

    /// Finish deferred timestamp maps after the measured playback interval.
    pub(crate) fn finish_gpu_timings(
        &mut self,
    ) -> Result<Vec<GpuTimestampSample>, HeadlessViewerGpuError> {
        let Some(ring) = &mut self.timestamp_ring else {
            return Ok(Vec::new());
        };
        ring.finish_all(&self.device)
            .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))
    }

    /// Samples discarded instead of blocking when every query slot was busy.
    pub(crate) fn discarded_gpu_timings(&self) -> u64 {
        self.timestamp_ring.as_ref().map_or(0, GpuTimestampQueryRing::discarded_samples)
    }

    /// Execute or reuse the exact output represented by `frame`.
    pub(crate) fn execute(
        &mut self,
        frame: &AppUiGpuPreviewFrame,
    ) -> Result<HeadlessViewerGpuExecution, HeadlessViewerGpuError> {
        let started = Instant::now();
        let output_key = frame.external_texture_key();
        if self.current_output_key.as_deref() == Some(output_key.as_str()) {
            return Ok(HeadlessViewerGpuExecution {
                output_width: frame.width,
                output_height: frame.height,
                cached: true,
                duration_us: elapsed_us(started),
                record_submit_us: elapsed_us(started),
                completion_wait_us: 0,
                gpu_timestamp_token: None,
                cpu_stage_timings: None,
                compositing_diagnostics: None,
                spatial_diagnostics: None,
                stage_diagnostics: None,
                fallback_reasons: Vec::new(),
                decode_execution: frame.decode_execution(),
            });
        }
        let presentation = ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .ok_or(HeadlessViewerGpuError::InvalidPresentation {
                width: frame.width,
                height: frame.height,
            })?;
        self.runtime.clear_frame_resources();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("headless_viewer_gpu_preview_encoder"),
        });
        let timestamp_token = self
            .timestamp_ring
            .as_mut()
            .map(|ring| ring.begin_frame(&self.device, &mut encoder))
            .transpose()
            .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?
            .flatten();
        let layers = match &frame.working_input {
            AppUiGpuPreviewWorkingInput::GpuComposite { layers } => layers,
        };
        let source_rect = presentation.normalized_source_rect();
        let request = ViewerGpuExecutionRequest {
            sequence_id: frame.sequence_id,
            timeline_frame: frame.frame,
            width: frame.width,
            height: frame.height,
            working_color_space: frame.working_color_space,
            layers,
            output_boundary: &frame.boundary,
            source_rect: ViewerSourceRect {
                x: source_rect.x,
                y: source_rect.y,
                width: source_rect.width,
                height: source_rect.height,
            },
            output_width: presentation.output_width,
            output_height: presentation.output_height,
            display_calibration: None,
        };
        let record_result =
            if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
                let mut stage_marker = HeadlessGpuStageMarker { ring, token };
                self.runtime.record_with_stage_marker(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    request,
                    Some(&mut stage_marker),
                )
            } else {
                self.runtime.record(&self.device, &self.queue, &mut encoder, request)
            };
        if record_result.is_err() {
            if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
                ring.abandon_frame(token)
                    .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?;
            }
        }
        let record =
            record_result.map_err(|error| HeadlessViewerGpuError::Record(error.to_string()))?;
        let _output_texture = self
            .runtime
            .output_texture_view(&record)
            .map_err(|error| HeadlessViewerGpuError::Record(error.to_string()))?;
        if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
            ring.finish_frame(&mut encoder, token)
                .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?;
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        let record_submit_us = elapsed_us(started);
        if let (Some(ring), Some(token)) = (&mut self.timestamp_ring, timestamp_token) {
            ring.after_submit(token)
                .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?;
        }
        self.current_output_key = Some(output_key);
        Ok(HeadlessViewerGpuExecution {
            output_width: frame.width,
            output_height: frame.height,
            cached: false,
            duration_us: elapsed_us(started),
            record_submit_us,
            completion_wait_us: 0,
            gpu_timestamp_token: timestamp_token.map(|token| token.id()),
            cpu_stage_timings: Some(record.cpu_stage_timings),
            compositing_diagnostics: Some(record.compositing_diagnostics),
            spatial_diagnostics: Some(record.spatial_diagnostics),
            stage_diagnostics: Some(record.stage_diagnostics),
            fallback_reasons: record.fallback_reasons,
            decode_execution: frame.decode_execution(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HeadlessViewerGpuError {
    #[error("no headless GPU adapter is available: {0}")]
    Adapter(String),
    #[error("headless GPU device creation failed: {0}")]
    Device(String),
    #[error("invalid headless Viewer presentation extent {width}x{height}")]
    InvalidPresentation { width: u32, height: u32 },
    #[error("headless Viewer GPU recording failed: {0}")]
    Record(String),
    #[error("headless Viewer GPU timestamp query failed: {0}")]
    Timestamp(String),
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
