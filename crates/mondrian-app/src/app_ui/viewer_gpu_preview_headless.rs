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
    native_video_texture_device_features, profile::gpu_timestamp_query_device_features,
    profile::GpuTimestampFrameTimer, request_adapter_with_native_video_preference,
    GpuCompositingDiagnostics, GpuNativeDecodedFrameImportSupport,
    GpuViewerSpatialRuntimeDiagnostics, RenderColorStageDiagnostics, ViewerGpuExecutionRequest,
    ViewerGpuExecutionRuntime, ViewerSourceRect,
};
use mondrian_ui_widgets::ViewerExternalTexturePresentation;

/// Evidence for one real headless Viewer GPU execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadlessViewerGpuExecution {
    /// Exact output width submitted to the shared Viewer GPU Runtime.
    pub output_width: u32,
    /// Exact output height submitted to the shared Viewer GPU Runtime.
    pub output_height: u32,
    /// Whether the runtime already retained this exact output.
    pub cached: bool,
    /// Wall time spent recording, submitting, and waiting for the GPU.
    pub duration_us: u64,
    /// CPU wall time through command recording and queue submission.
    pub record_submit_us: u64,
    /// CPU wall time waiting for the submitted work to complete.
    pub completion_wait_us: u64,
    /// Hardware timestamp duration for the Viewer GPU commands.
    pub gpu_duration_us: Option<u64>,
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
    timestamp_timer: Option<GpuTimestampFrameTimer>,
    adapter_info: HeadlessViewerGpuAdapterInfo,
    current_output_key: Option<String>,
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
                | gpu_timestamp_query_device_features(supported_features),
            ..wgpu::DeviceDescriptor::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(|error| HeadlessViewerGpuError::Device(error.to_string()))?;
        let runtime = ViewerGpuExecutionRuntime::new(&adapter, &device, &queue);
        let timestamp_timer = GpuTimestampFrameTimer::new(&device, &queue);
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
            timestamp_timer,
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
                gpu_duration_us: None,
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
        if let Some(timer) = &self.timestamp_timer {
            timer.begin(&mut encoder);
        }
        let layers = match &frame.working_input {
            AppUiGpuPreviewWorkingInput::GpuComposite { layers } => layers,
        };
        let source_rect = presentation.normalized_source_rect();
        let record = self
            .runtime
            .record(
                &self.device,
                &self.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
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
                },
            )
            .map_err(|error| HeadlessViewerGpuError::Record(error.to_string()))?;
        let _output_texture = self
            .runtime
            .output_texture_view(&record)
            .map_err(|error| HeadlessViewerGpuError::Record(error.to_string()))?;
        if let Some(timer) = &self.timestamp_timer {
            timer.finish(&mut encoder);
        }
        let submission = self.queue.submit(std::iter::once(encoder.finish()));
        let record_submit_us = elapsed_us(started);
        let wait_started = Instant::now();
        let gpu_duration_us = if let Some(timer) = &self.timestamp_timer {
            Some(
                timer
                    .read_elapsed_us_after_submission(&self.device, submission)
                    .map_err(|error| HeadlessViewerGpuError::Timestamp(error.to_string()))?,
            )
        } else {
            self.device
                .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
                .map_err(|error| HeadlessViewerGpuError::Poll(error.to_string()))?;
            None
        };
        let completion_wait_us = elapsed_us(wait_started);
        self.current_output_key = Some(output_key);
        Ok(HeadlessViewerGpuExecution {
            output_width: frame.width,
            output_height: frame.height,
            cached: false,
            duration_us: elapsed_us(started),
            record_submit_us,
            completion_wait_us,
            gpu_duration_us,
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
    #[error("headless Viewer GPU completion wait failed: {0}")]
    Poll(String),
    #[error("headless Viewer GPU timestamp query failed: {0}")]
    Timestamp(String),
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
