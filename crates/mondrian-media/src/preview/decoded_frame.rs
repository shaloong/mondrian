//! Typed CPU Preview frame payloads and frame-local decode evidence.
//!
//! These payloads preserve source color, alpha, residency, temporal-selection,
//! seek-index, hardware-path, and cache provenance. Cache/session code may
//! decorate evidence through crate-private builders, while public consumers
//! receive immutable typed frame contracts.

use super::*;

#[derive(Debug, Clone)]
pub struct RgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Shared CPU-resident RGBA8 pixels.
    pub data: Arc<Vec<u8>>,
    /// Color and alpha semantics of the decoded pixels.
    pub color_contract: DecodedRgbaFrameContract,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
    /// Frame-local execution provenance retained across every cache layer.
    pub decode_execution: PreviewDecodeExecutionPath,
}

/// CPU-resident RGBA f32 preview frame that preserves scene-linear samples.
#[derive(Debug, Clone)]
pub struct FloatRgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Shared CPU-resident interleaved RGBA f32 pixels.
    data: Arc<Vec<f32>>,
    /// Color and alpha semantics of the decoded pixels.
    pub color_contract: DecodedRgbaFrameContract,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
    /// Frame-local execution provenance retained across every cache layer.
    pub decode_execution: PreviewDecodeExecutionPath,
}

/// Encoding represented by a decoded CPU RGBA payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecodedRgbaEncoding {
    /// RGB channels retain the source transfer function and primaries.
    SourceEncodedRgb,
    /// RGB channels retain scene-linear source values and primaries.
    SourceLinearRgb,
}

/// Alpha representation of a decoded CPU RGBA payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecodedRgbaAlphaMode {
    /// Alpha is independent of the RGB channels.
    Straight,
}

/// Applied conversion contract for a decoded CPU RGBA payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DecodedRgbaFrameContract {
    /// Effective source color interpretation retained by RGB channels.
    pub source: PreviewSourceColorContract,
    /// Encoding of the RGB channels after decode.
    pub encoding: DecodedRgbaEncoding,
    /// Alpha representation after decode.
    pub alpha_mode: DecodedRgbaAlphaMode,
    /// Matrix actually applied while converting decoder pixels to RGB.
    pub applied_matrix: DecodedVideoMatrix,
    /// Quantization range actually applied while converting decoder pixels to RGB.
    pub applied_range: DecodedVideoRange,
}

impl DecodedRgbaFrameContract {
    pub(super) fn source_encoded(
        source: PreviewSourceColorContract,
        applied_matrix: DecodedVideoMatrix,
        applied_range: DecodedVideoRange,
    ) -> Self {
        Self {
            source,
            encoding: DecodedRgbaEncoding::SourceEncodedRgb,
            alpha_mode: DecodedRgbaAlphaMode::Straight,
            applied_matrix,
            applied_range,
        }
    }

    pub(super) fn source_linear(source: PreviewSourceColorContract) -> Self {
        Self {
            source,
            encoding: DecodedRgbaEncoding::SourceLinearRgb,
            alpha_mode: DecodedRgbaAlphaMode::Straight,
            applied_matrix: DecodedVideoMatrix::Rgb,
            applied_range: DecodedVideoRange::Full,
        }
    }
}

impl RgbaFrame {
    pub(crate) fn new(
        width: u32,
        height: u32,
        data: Vec<u8>,
        color_contract: DecodedRgbaFrameContract,
        path: PreviewDecodePath,
    ) -> Self {
        Self {
            width,
            height,
            data: Arc::new(data),
            color_contract,
            diagnostics: PreviewDecodeDiagnostics::new(path),
            decode_execution: PreviewDecodeExecutionPath::SoftwareCpu,
        }
    }

    /// Borrow decoded RGBA8 pixels.
    pub fn rgba(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Consume this frame and return shared decoded RGBA8 pixels.
    pub fn into_shared_data(self) -> Arc<Vec<u8>> {
        self.data
    }

    /// Consume this frame and return owned decoded RGBA8 pixels.
    pub fn into_data(self) -> Vec<u8> {
        Arc::try_unwrap(self.data).unwrap_or_else(|data| data.as_ref().clone())
    }

    pub(super) fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.diagnostics = self.diagnostics.with_elapsed(elapsed);
        self
    }

    pub(super) fn with_decode_work(
        mut self,
        seek_performed: bool,
        decoded_frame_count: usize,
    ) -> Self {
        self.diagnostics.seek_performed = seek_performed;
        self.diagnostics.decoded_frame_count = decoded_frame_count.min(u32::MAX as usize) as u32;
        self
    }

    pub(super) fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_pts: Option<i64>,
        hit_tolerance_pts: i64,
        policy: PreviewDecodeAccessPolicy,
    ) -> Self {
        self.diagnostics.requested_pts = Some(requested_pts);
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.temporal_approximation = temporal_selection_is_approximate(
            requested_pts,
            selected_pts,
            hit_tolerance_pts,
            policy,
        );
        self
    }

    pub(super) fn with_seek_strategy(mut self, seek_strategy: PreviewDecodeSeekStrategy) -> Self {
        self.diagnostics.seek_strategy = seek_strategy;
        self
    }

    pub(super) fn with_session_reused(mut self, session_reused: bool) -> Self {
        self.diagnostics.session_reused = session_reused;
        self
    }

    pub(super) fn with_forward_reused(mut self, forward_reused: bool) -> Self {
        self.diagnostics.forward_reused = forward_reused;
        self
    }

    pub(super) fn with_seek_index_diagnostics(
        mut self,
        diagnostics: PreviewSeekIndexDiagnostics,
        resolution: PreviewSeekResolution,
    ) -> Self {
        self.diagnostics.seek_index_available = diagnostics.available;
        self.diagnostics.seek_index_keyframes = diagnostics.keyframes;
        self.diagnostics.seek_index_observed_packets = diagnostics.observed_packets;
        self.diagnostics.seek_index_source = diagnostics.source;
        self.diagnostics.seek_index_used = resolution.used_index;
        self.diagnostics.seek_index_anchor_pts = resolution.anchor_pts;
        self
    }

    pub(super) fn with_threading(mut self, kind: PreviewDecodeThreadingKind, count: usize) -> Self {
        self.diagnostics.threading_kind = kind;
        self.diagnostics.threading_count = count.min(u32::MAX as usize) as u32;
        self
    }

    pub(super) fn with_hardware_decode_plan(mut self, plan: &PreviewHardwareDecodePlan) -> Self {
        self.diagnostics = self.diagnostics.with_hardware_decode_plan(plan);
        self
    }

    pub(super) fn with_decoded_surface_format(mut self, format: DecodedVideoSurfaceFormat) -> Self {
        if self.diagnostics.decoded_surface_format == DecodedVideoSurfaceFormat::Unknown {
            self.diagnostics.decoded_surface_format = format;
        }
        self
    }

    pub(super) fn with_decoded_video_sampling(mut self, sampling: DecodedVideoSampling) -> Self {
        self.diagnostics.decoded_video_sampling = sampling;
        self
    }

    pub(super) fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.diagnostics = self.diagnostics.with_access_mode(access_mode);
        self
    }

    pub(super) fn with_access_policy(mut self, policy: PreviewDecodeAccessPolicy) -> Self {
        self.diagnostics = self.diagnostics.with_access_policy(policy);
        self
    }

    pub(super) fn with_stage_durations(mut self, durations: PreviewDecodeStageDurations) -> Self {
        self.diagnostics.stage_durations.accumulate(durations);
        self
    }

    pub(super) fn with_decode_execution(mut self) -> Self {
        self.decode_execution = self.diagnostics.execution_path();
        self
    }

    pub(super) fn into_cache_hit(
        mut self,
        elapsed: Duration,
        access_mode: PreviewDecodeAccessMode,
    ) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::cache_hit_for_mode(elapsed, access_mode);
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }

    pub(super) fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }
}

impl FloatRgbaFrame {
    pub(super) fn new(
        width: u32,
        height: u32,
        data: Vec<f32>,
        color_contract: DecodedRgbaFrameContract,
        path: PreviewDecodePath,
    ) -> Self {
        debug_assert_eq!(data.len(), width as usize * height as usize * 4);
        let mut diagnostics = PreviewDecodeDiagnostics::new(path);
        diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        Self {
            width,
            height,
            data: Arc::new(data),
            color_contract,
            diagnostics,
            decode_execution: PreviewDecodeExecutionPath::SoftwareCpu,
        }
    }

    /// Borrow decoded RGBA f32 pixels.
    pub fn rgba(&self) -> &[f32] {
        self.data.as_slice()
    }

    /// Consume this frame and return shared decoded RGBA f32 pixels.
    pub fn into_shared_data(self) -> Arc<Vec<f32>> {
        self.data
    }

    /// Consume this frame and return owned decoded RGBA f32 pixels.
    pub fn into_data(self) -> Vec<f32> {
        Arc::try_unwrap(self.data).unwrap_or_else(|data| data.as_ref().clone())
    }

    pub(super) fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.diagnostics = self.diagnostics.with_elapsed(elapsed);
        self
    }

    pub(super) fn with_seek_strategy(mut self, seek_strategy: PreviewDecodeSeekStrategy) -> Self {
        self.diagnostics.seek_strategy = seek_strategy;
        self
    }

    pub(super) fn with_session_reused(mut self, session_reused: bool) -> Self {
        self.diagnostics.session_reused = session_reused;
        self
    }

    pub(super) fn with_decode_work(
        mut self,
        seek_performed: bool,
        decoded_frame_count: usize,
    ) -> Self {
        self.diagnostics.seek_performed = seek_performed;
        self.diagnostics.decoded_frame_count = decoded_frame_count.min(u32::MAX as usize) as u32;
        self
    }

    pub(super) fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_pts: Option<i64>,
        hit_tolerance_pts: i64,
        policy: PreviewDecodeAccessPolicy,
    ) -> Self {
        self.diagnostics.requested_pts = Some(requested_pts);
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.temporal_approximation = temporal_selection_is_approximate(
            requested_pts,
            selected_pts,
            hit_tolerance_pts,
            policy,
        );
        self
    }

    pub(super) fn with_forward_reused(mut self, forward_reused: bool) -> Self {
        self.diagnostics.forward_reused = forward_reused;
        self
    }

    pub(super) fn with_seek_index_diagnostics(
        mut self,
        diagnostics: PreviewSeekIndexDiagnostics,
        resolution: PreviewSeekResolution,
    ) -> Self {
        self.diagnostics.seek_index_available = diagnostics.available;
        self.diagnostics.seek_index_keyframes = diagnostics.keyframes;
        self.diagnostics.seek_index_observed_packets = diagnostics.observed_packets;
        self.diagnostics.seek_index_source = diagnostics.source;
        self.diagnostics.seek_index_used = resolution.used_index;
        self.diagnostics.seek_index_anchor_pts = resolution.anchor_pts;
        self
    }

    pub(super) fn with_threading(mut self, kind: PreviewDecodeThreadingKind, count: usize) -> Self {
        self.diagnostics.threading_kind = kind;
        self.diagnostics.threading_count = count.min(u32::MAX as usize) as u32;
        self
    }

    pub(super) fn with_hardware_decode_plan(mut self, plan: &PreviewHardwareDecodePlan) -> Self {
        self.diagnostics = self.diagnostics.with_hardware_decode_plan(plan);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self
    }

    pub(super) fn with_decoded_surface_format(mut self, format: DecodedVideoSurfaceFormat) -> Self {
        if self.diagnostics.decoded_surface_format == DecodedVideoSurfaceFormat::Unknown {
            self.diagnostics.decoded_surface_format = format;
        }
        self
    }

    pub(super) fn with_decoded_video_sampling(mut self, sampling: DecodedVideoSampling) -> Self {
        self.diagnostics.decoded_video_sampling = sampling;
        self
    }

    pub(super) fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.diagnostics = self.diagnostics.with_access_mode(access_mode);
        self
    }

    pub(super) fn with_access_policy(mut self, policy: PreviewDecodeAccessPolicy) -> Self {
        self.diagnostics = self.diagnostics.with_access_policy(policy);
        self
    }

    pub(super) fn with_stage_durations(mut self, durations: PreviewDecodeStageDurations) -> Self {
        self.diagnostics.stage_durations.accumulate(durations);
        self
    }

    pub(super) fn with_decode_execution(mut self) -> Self {
        self.decode_execution = self.diagnostics.execution_path();
        self
    }

    pub(super) fn into_cache_hit(
        mut self,
        elapsed: Duration,
        access_mode: PreviewDecodeAccessMode,
    ) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::cache_hit_for_mode(elapsed, access_mode);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }

    pub(super) fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }
}
