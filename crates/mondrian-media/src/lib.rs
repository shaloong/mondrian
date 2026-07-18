//! # mondrian-media
//!
//! 媒体处理核心模块。负责：
//! - 媒体文件探针（格式/编解码/元数据）
//! - 视频帧解码（当前为 FFmpeg CPU RGBA 路径；硬解/零拷贝必须通过显式 residency 诊断证明）
//! - 音频采样解码与混合
//! - 预览帧缓存与解码调度
//! - 代理文件生成（Proxy）

pub mod audio;
mod audio_output;
mod audio_playback;
pub mod audio_source;
pub mod decoder;
mod ffmpeg_runtime;
pub mod info;
pub mod multilevel_cache;
pub mod preview;
pub mod proxy;
pub mod waveform;

pub use audio::{AudioBuffer, AudioRenderCursor, RealtimeAudioOutput, RealtimeAudioOutputSnapshot};
pub use audio_output::{RealtimeAudioOutputEvent, RealtimeAudioOutputManager};
pub use audio_playback::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderRequest, AudioPcmRenderer,
    AudioPlayback, AudioPlaybackConfig, AudioPlaybackConfigError, AudioPlaybackEvent,
    AudioPlaybackMode, AudioPlaybackPoll, AudioPlaybackSnapshot, AudioPlaybackState,
};
pub use audio_source::{AudioSourceCache, AudioSourceCacheDiagnostics, AudioSourceReader};
pub use decoder::{
    resolve_decoded_video_range, DecodedFrameResidency, DecodedGpuFrameHandleKind,
    DecodedVideoChromaLocation, DecodedVideoMatrix, DecodedVideoRange, DecodedVideoRangeContract,
    DecodedVideoSampling, DecodedVideoSurfaceFormat, HwAccelBackend, HwAccelCodecConfigMethods,
    HwAccelCodecConfigProbe, HwAccelDeviceContextProbe, HwAccelDeviceSelector, HwAccelPixelFormat,
    HwAccelProbe,
};
pub use ffmpeg_runtime::verify_ffmpeg_runtime;
pub use info::{
    interpret_video_color_metadata, parse_video_color_metadata_hint, AudioStreamInfo,
    DetectedColorInterpretation, MediaInfo, VideoCodecProfile, VideoColorDetectionMethod,
    VideoColorDiagnostic, VideoColorDiagnosticIssueAggregate, VideoColorDiagnosticIssueSummary,
    VideoColorInterpretationConfidence, VideoColorInterpretationEvidence,
    VideoColorInterpretationWarning, VideoColorMetadata, VideoColorMetadataHint,
    VideoColorMetadataHintScope, VideoColorSpaceSource, VideoColorTag, VideoHdrMetadataSummary,
    VideoHdrSideDataKind, VideoStreamInfo,
};
pub use multilevel_cache::{CacheTier, MultiLevelCache, ResolvedMediaPath};
pub use preview::{
    clear_global_preview_frame_cache, clear_thread_local_preview_decode_session,
    decode_preview_frame_cancellable, preview_decode_backend, preview_decode_cpu_budget,
    set_preview_decode_backend, DecodedRgbaAlphaMode, DecodedRgbaEncoding,
    DecodedRgbaFrameContract, FfmpegD3D11TextureView, FfmpegD3D12TextureView,
    FfmpegNativeDecodedFrameResource, FfmpegNativeDecodedFrameResourceError, FloatRgbaFrame,
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeBackend,
    PreviewDecodeCpuBudget, PreviewDecodeDiagnostics, PreviewDecodeExecutionPath,
    PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRequest, PreviewDecodeSeekStrategy,
    PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewFileFingerprint,
    PreviewHardwareDecodeBlocker, PreviewHardwareDecodeCpuTransferStatus,
    PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest, PreviewNativeDecodeFallback,
    PreviewNativeDecodedFrame, PreviewNativeDecodedFrameError, PreviewNativeDecodedFrameHandle,
    PreviewNativeDecodedFrameResource, PreviewScrubAdaptiveClass, PreviewSeekIndexSource,
    PreviewSourceColorContract, RgbaFrame,
};
pub use proxy::{
    ProxyArtifactManifest, ProxyArtifactSettings, ProxyCodec, ProxyColorContract,
    ProxyColorContractError, ProxyConfig, ProxyEncodingProfile, ProxyGenerator, ProxyResolution,
    ProxySourceFingerprint, ProxyStatus,
};
pub use waveform::{compute_waveform, WaveformCache, WaveformData};
