//! # mondrian-media
//!
//! 媒体处理核心模块。负责：
//! - 媒体文件探针（格式/编解码/元数据）
//! - 视频帧解码（当前为 FFmpeg CPU RGBA/YUV 路径；硬解/零拷贝必须通过显式 residency 诊断证明）
//! - 音频采样解码与混合
//! - 帧缓存（LRU）
//! - 代理文件生成（Proxy）

pub mod audio;
pub mod cache;
pub mod decoder;
pub mod info;
pub mod multilevel_cache;
pub mod preview;
pub mod proxy;
pub mod waveform;

pub use audio::{
    AudioBuffer, AudioMixer, AudioSourceCache, AudioTrackConfig, AudioTrackData, ClockRole,
    RealtimeAudioOutput,
};
pub use cache::FrameCache;
pub use decoder::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecoderMetricsSnapshot, DecoderPool,
    HwAccelBackend, HwAccelProbe,
};
pub use info::{
    AudioStreamInfo, DetectedColorInterpretation, MediaInfo, VideoColorDetectionMethod,
    VideoColorDiagnostic, VideoColorDiagnosticIssueAggregate, VideoColorDiagnosticIssueSummary,
    VideoColorInterpretationConfidence, VideoColorInterpretationEvidence,
    VideoColorInterpretationWarning, VideoColorMetadata, VideoColorMetadataHint,
    VideoColorMetadataHintScope, VideoColorSpaceSource, VideoColorTag, VideoHdrMetadataSummary,
    VideoHdrSideDataKind, VideoStreamInfo,
};
pub use multilevel_cache::{CacheTier, MultiLevelCache, ResolvedMediaPath};
pub use preview::{
    clear_global_preview_frame_cache, clear_thread_local_preview_decode_session,
    decode_first_still_frame_rgba, decode_playback_cursor_frame_rgba_scaled_cancellable,
    decode_playback_cursor_frame_rgba_scaled_cancellable_with_fingerprint,
    decode_preview_rgba_scaled_cancellable, decode_scrub_cursor_frame_rgba_scaled_cancellable,
    decode_scrub_cursor_frame_rgba_scaled_cancellable_with_fingerprint, decode_still_frame_rgba,
    decode_still_frame_rgba_scaled, decode_still_frame_rgba_scaled_cancellable,
    decode_still_frame_rgba_scaled_cancellable_with_fingerprint, preview_decode_backend,
    preview_decode_cpu_budget, set_preview_decode_backend, PreviewDecodeAccessMode,
    PreviewDecodeBackend, PreviewDecodeCpuBudget, PreviewDecodeDiagnostics, PreviewDecodeOutcome,
    PreviewDecodePath, PreviewDecodeRgbaRequest, PreviewDecodeStageDurations,
    PreviewDecodeThreadingKind, PreviewFileFingerprint, RgbaFrame,
};
pub use proxy::{ProxyCodec, ProxyConfig, ProxyGenerator, ProxyResolution, ProxyStatus};
pub use waveform::{compute_waveform, WaveformCache, WaveformData};
