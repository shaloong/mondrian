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

/// Chroma sampling retained by a compact CPU YUV Preview frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CpuYuvChromaSubsampling {
    /// One Cb/Cr pair covers a 2x2 luma region.
    Cs420,
    /// One Cb/Cr pair covers a 2x1 luma region.
    Cs422,
}

/// Normalized texture representation used by a compact CPU YUV frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CpuYuvSampleFormat {
    /// Eight-bit code values uploaded as `R8Unorm` / `Rg8Unorm`.
    Unorm8,
    /// Ten-bit code values retained right-aligned in `R16Unorm` / `Rg16Unorm`.
    ///
    /// The renderer applies the normalized-texture scale required to recover
    /// the original code values. Keeping FFmpeg's native alignment makes plane
    /// publication a bounded copy instead of a full-frame sample rewrite.
    Unorm16Lsb10,
}

impl CpuYuvSampleFormat {
    /// Number of retained bytes for one component sample.
    pub const fn bytes_per_component(self) -> usize {
        match self {
            Self::Unorm8 => 1,
            Self::Unorm16Lsb10 => 2,
        }
    }

    /// Effective coded bit depth.
    pub const fn bit_depth(self) -> u8 {
        match self {
            Self::Unorm8 => 8,
            Self::Unorm16Lsb10 => 10,
        }
    }
}

/// Compact CPU-decoded YUV payload consumed directly by the renderer's GPU
/// video-materialization path.
///
/// FFmpeg's immutable luma, Cb, and Cr plane allocations are retained with
/// explicit row strides so the renderer can upload them without an RGBA
/// expansion, chroma interleave, or full-frame CPU copy. Ten-bit samples retain
/// FFmpeg's least-significant-bit alignment in a little-endian `u16`; the
/// renderer owns normalized-texture recovery.
#[derive(Clone)]
pub struct CpuYuvFrame {
    /// Visible source width in pixels.
    pub width: u32,
    /// Visible source height in pixels.
    pub height: u32,
    /// Chroma-plane width in CbCr pairs.
    pub chroma_width: u32,
    /// Chroma-plane height in rows.
    pub chroma_height: u32,
    /// Chroma subsampling represented by the compact planes.
    pub chroma_subsampling: CpuYuvChromaSubsampling,
    /// Component storage representation.
    pub sample_format: CpuYuvSampleFormat,
    /// Resolved source color interpretation retained by the YUV code values.
    pub source_color: PreviewSourceColorContract,
    /// Resolved matrix, range, siting, and bit-depth facts.
    pub video_sampling: DecodedVideoSampling,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
    /// Frame-local execution provenance retained across every cache layer.
    pub decode_execution: PreviewDecodeExecutionPath,
    frame: Arc<ffmpeg::util::frame::video::Video>,
}

/// Physical chroma-plane layout retained by a compact CPU YUV frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CpuYuvChromaPlaneLayout {
    /// Cb and Cr samples are interleaved in one two-component plane.
    Interleaved,
    /// Cb and Cr samples remain in separate one-component FFmpeg planes.
    Planar,
}

/// One immutable CPU YUV plane with explicit row stride.
#[derive(Debug, Clone, Copy)]
pub struct CpuYuvPlane<'a> {
    bytes: &'a [u8],
    bytes_per_row: u32,
}

impl<'a> CpuYuvPlane<'a> {
    /// Borrow the complete plane allocation, including any row padding.
    pub const fn data(self) -> &'a [u8] {
        self.bytes
    }

    /// Physical byte distance between consecutive rows.
    pub const fn bytes_per_row(self) -> u32 {
        self.bytes_per_row
    }
}

/// Immutable chroma planes retained by a compact CPU YUV frame.
#[derive(Debug, Clone, Copy)]
pub enum CpuYuvChromaPlanes<'a> {
    /// One tightly packed two-component CbCr plane.
    Interleaved(CpuYuvPlane<'a>),
    /// Independent Cb and Cr component planes.
    Planar {
        cb: CpuYuvPlane<'a>,
        cr: CpuYuvPlane<'a>,
    },
}

impl std::fmt::Debug for CpuYuvFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CpuYuvFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("chroma_width", &self.chroma_width)
            .field("chroma_height", &self.chroma_height)
            .field("chroma_subsampling", &self.chroma_subsampling)
            .field("sample_format", &self.sample_format)
            .field("chroma_plane_layout", &self.chroma_plane_layout())
            .field("source_color", &self.source_color)
            .field("video_sampling", &self.video_sampling)
            .field("diagnostics", &self.diagnostics)
            .field("decode_execution", &self.decode_execution)
            .finish()
    }
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

    pub(super) fn with_seek_strategy(mut self, seek_strategy: PreviewDecodeSeekStrategy) -> Self {
        self.diagnostics.seek_strategy = seek_strategy;
        self
    }

    pub(super) fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_extent: Option<DecodedTemporalExtent>,
    ) -> Self {
        self.diagnostics = self.diagnostics.with_temporal_selection(requested_pts, selected_extent);
        self
    }

    pub(super) fn with_session_disposition(
        mut self,
        disposition: super::PreviewDecodeSessionDisposition,
    ) -> Self {
        self.diagnostics.session_disposition = disposition;
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

    pub(super) fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        let requested_pts = self.diagnostics.requested_pts;
        let selected_pts = self.diagnostics.selected_pts;
        let selected_duration_pts = self.diagnostics.selected_duration_pts;
        let selected_temporal_extent_source = self.diagnostics.selected_temporal_extent_source;
        let temporal_approximation = self.diagnostics.temporal_approximation;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self.diagnostics.requested_pts = requested_pts;
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.selected_duration_pts = selected_duration_pts;
        self.diagnostics.selected_temporal_extent_source = selected_temporal_extent_source;
        self.diagnostics.temporal_approximation = temporal_approximation;
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

    pub(super) fn with_session_disposition(
        mut self,
        disposition: super::PreviewDecodeSessionDisposition,
    ) -> Self {
        self.diagnostics.session_disposition = disposition;
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
        selected_extent: Option<DecodedTemporalExtent>,
    ) -> Self {
        self.diagnostics = self.diagnostics.with_temporal_selection(requested_pts, selected_extent);
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

    pub(super) fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        let requested_pts = self.diagnostics.requested_pts;
        let selected_pts = self.diagnostics.selected_pts;
        let selected_duration_pts = self.diagnostics.selected_duration_pts;
        let selected_temporal_extent_source = self.diagnostics.selected_temporal_extent_source;
        let temporal_approximation = self.diagnostics.temporal_approximation;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self.diagnostics.requested_pts = requested_pts;
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.selected_duration_pts = selected_duration_pts;
        self.diagnostics.selected_temporal_extent_source = selected_temporal_extent_source;
        self.diagnostics.temporal_approximation = temporal_approximation;
        self
    }
}

impl CpuYuvFrame {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_planar_ffmpeg(
        width: u32,
        height: u32,
        chroma_width: u32,
        chroma_height: u32,
        chroma_subsampling: CpuYuvChromaSubsampling,
        sample_format: CpuYuvSampleFormat,
        source_color: PreviewSourceColorContract,
        video_sampling: DecodedVideoSampling,
        frame: ffmpeg::util::frame::video::Video,
        path: PreviewDecodePath,
    ) -> Self {
        debug_assert_eq!(frame.width(), width);
        debug_assert_eq!(frame.height(), height);
        debug_assert_eq!(video_sampling.bit_depth, sample_format.bit_depth());
        let mut diagnostics = PreviewDecodeDiagnostics::new(path);
        diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuYuv;
        diagnostics.decoded_video_sampling = video_sampling;
        Self {
            width,
            height,
            chroma_width,
            chroma_height,
            chroma_subsampling,
            sample_format,
            source_color,
            video_sampling,
            diagnostics,
            decode_execution: PreviewDecodeExecutionPath::SoftwareCpu,
            frame: Arc::new(frame),
        }
    }

    /// Borrow luma bytes and their physical row stride.
    pub fn luma_plane(&self) -> CpuYuvPlane<'_> {
        CpuYuvPlane {
            bytes: self.frame.data(0),
            bytes_per_row: self.frame.stride(0).min(u32::MAX as usize) as u32,
        }
    }

    /// Borrow the retained chroma planes and their physical row strides.
    pub fn chroma_planes(&self) -> CpuYuvChromaPlanes<'_> {
        CpuYuvChromaPlanes::Planar {
            cb: CpuYuvPlane {
                bytes: self.frame.data(1),
                bytes_per_row: self.frame.stride(1).min(u32::MAX as usize) as u32,
            },
            cr: CpuYuvPlane {
                bytes: self.frame.data(2),
                bytes_per_row: self.frame.stride(2).min(u32::MAX as usize) as u32,
            },
        }
    }

    /// Physical chroma storage layout.
    pub const fn chroma_plane_layout(&self) -> CpuYuvChromaPlaneLayout {
        CpuYuvChromaPlaneLayout::Planar
    }

    /// Retained CPU bytes across both compact planes.
    pub fn retained_bytes(&self) -> usize {
        let luma = self.luma_plane().data().len();
        match self.chroma_planes() {
            CpuYuvChromaPlanes::Interleaved(chroma) => luma.saturating_add(chroma.data().len()),
            CpuYuvChromaPlanes::Planar { cb, cr } => {
                luma.saturating_add(cb.data().len()).saturating_add(cr.data().len())
            }
        }
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

    pub(super) fn with_seek_strategy(mut self, seek_strategy: PreviewDecodeSeekStrategy) -> Self {
        self.diagnostics.seek_strategy = seek_strategy;
        self
    }

    pub(super) fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_extent: Option<DecodedTemporalExtent>,
    ) -> Self {
        self.diagnostics = self.diagnostics.with_temporal_selection(requested_pts, selected_extent);
        self
    }

    pub(super) fn with_session_disposition(
        mut self,
        disposition: super::PreviewDecodeSessionDisposition,
    ) -> Self {
        self.diagnostics.session_disposition = disposition;
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
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuYuv;
        self
    }

    pub(super) fn with_decoded_surface_format(mut self, format: DecodedVideoSurfaceFormat) -> Self {
        if self.diagnostics.decoded_surface_format == DecodedVideoSurfaceFormat::Unknown {
            self.diagnostics.decoded_surface_format = format;
        }
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

    pub(super) fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        let requested_pts = self.diagnostics.requested_pts;
        let selected_pts = self.diagnostics.selected_pts;
        let selected_duration_pts = self.diagnostics.selected_duration_pts;
        let selected_temporal_extent_source = self.diagnostics.selected_temporal_extent_source;
        let temporal_approximation = self.diagnostics.temporal_approximation;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuYuv;
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self.diagnostics.requested_pts = requested_pts;
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.selected_duration_pts = selected_duration_pts;
        self.diagnostics.selected_temporal_extent_source = selected_temporal_extent_source;
        self.diagnostics.temporal_approximation = temporal_approximation;
        self
    }
}
