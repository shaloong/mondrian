//! # mondrian-media
//!
//! 媒体处理核心模块。负责：
//! - 媒体文件探针（格式/编解码/元数据）
//! - 视频帧解码（FFmpeg + GPU 硬解码）
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
pub use decoder::DecoderPool;
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
    clear_global_preview_frame_cache, decode_first_video_frame_rgba,
    decode_video_frame_at_time_rgba, decode_video_frame_at_time_rgba_scaled,
    preview_decode_backend, set_preview_decode_backend, PreviewDecodeBackend, RgbaFrame,
};
pub use proxy::{ProxyConfig, ProxyGenerator};
pub use waveform::{compute_waveform, WaveformCache, WaveformData};
