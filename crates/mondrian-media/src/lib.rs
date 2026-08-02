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
mod process_supervisor;
pub mod proxy;
pub mod waveform;

pub use audio::{AudioBuffer, RealtimeAudioOutputControlError, RealtimeAudioOutputSnapshot};
pub use audio_output::RealtimeAudioOutputLossReason;
#[cfg(feature = "validation")]
pub use audio_playback::AudioPlaybackValidationError;
pub use audio_playback::{
    validate_audio_playback_anchor, AudioOutputLifecycleDiagnostics, AudioOutputLossSnapshot,
    AudioPcmContinuity, AudioPcmContinuityModel, AudioPcmRenderGeneration, AudioPcmRenderRequest,
    AudioPcmRenderer, AudioPlayback, AudioPlaybackConfig, AudioPlaybackConfigError,
    AudioPlaybackCreateError, AudioPlaybackError, AudioPlaybackEvent, AudioPlaybackMode,
    AudioPlaybackPoll, AudioPlaybackShutdownError, AudioPlaybackSnapshot, AudioPlaybackState,
    AudioRenderRecoveryDisposition,
};
pub use audio_source::{
    AudioSourceCache, AudioSourceCacheConfig, AudioSourceCacheDiagnostics, AudioSourceReader,
    AudioSourceSelection,
};
pub use decoder::{
    resolve_decoded_video_range, DecodedFrameResidency, DecodedGpuFrameHandleKind,
    DecodedVideoChromaLocation, DecodedVideoMatrix, DecodedVideoRange, DecodedVideoRangeContract,
    DecodedVideoSampling, DecodedVideoSurfaceFormat, HwAccelBackend, HwAccelCodecConfigMethods,
    HwAccelCodecConfigProbe, HwAccelDeviceContextProbe, HwAccelDeviceSelector, HwAccelPixelFormat,
    HwAccelProbe, HwDeviceContextPool, HwDeviceContextPoolDiagnostics, HwDeviceContextPoolPolicy,
};
pub use ffmpeg_runtime::verify_ffmpeg_runtime;
pub use info::{
    interpret_video_color_metadata, is_picture_file_extension, parse_video_color_metadata_hint,
    probe_media_info, AudioStreamInfo, DetectedColorInterpretation, MediaInfo, MediaProbeSnapshot,
    ProvenVideoSampling, VideoCodecProfile, VideoColorDetectionMethod, VideoColorDiagnostic,
    VideoColorDiagnosticIssueAggregate, VideoColorDiagnosticIssueSummary,
    VideoColorInterpretationConfidence, VideoColorInterpretationEvidence,
    VideoColorInterpretationWarning, VideoColorMetadata, VideoColorMetadataDeclaration,
    VideoColorMetadataHint, VideoColorMetadataHintAuthority, VideoColorMetadataHintScope,
    VideoColorSpaceSource, VideoColorTag, VideoHdrMetadataSummary, VideoHdrSideDataKind,
    VideoStreamInfo,
};
pub use multilevel_cache::{CacheTier, MultiLevelCache, ResolvedMediaPath};
pub use preview::{
    clear_thread_local_preview_decode_session, decode_preview_frame_cancellable,
    preview_decode_backend, preview_decode_cpu_budget, run_preview_demux_worker,
    set_preview_decode_backend, DecodedRgbaAlphaMode, DecodedRgbaEncoding,
    DecodedRgbaFrameContract, FfmpegD3D11TextureView, FfmpegD3D12TextureView,
    FfmpegNativeDecodedFrameResource, FfmpegNativeDecodedFrameResourceError, FloatRgbaFrame,
    MediaFileChangeStamp, MediaFileFingerprint, MediaFileObjectIdentity, PreviewDecodeAccessMode,
    PreviewDecodeAdaptiveHints, PreviewDecodeAlphaPresence, PreviewDecodeBackend,
    PreviewDecodeCancellation, PreviewDecodeCancellationCheckpoint,
    PreviewDecodeCancellationCheckpointEvidence, PreviewDecodeCancellationEvidence,
    PreviewDecodeCancellationSource, PreviewDecodeContractError, PreviewDecodeCpuBudget,
    PreviewDecodeDiagnostics, PreviewDecodeExecutionObserver, PreviewDecodeExecutionPath,
    PreviewDecodeExecutionProgress, PreviewDecodeExecutionStage, PreviewDecodeGeometry,
    PreviewDecodeKey, PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodePayloadRequirement,
    PreviewDecodeRequest, PreviewDecodeSeekStrategy, PreviewDecodeSessionContext,
    PreviewDecodeSessionContextBootstrap, PreviewDecodeSessionDisposition, PreviewDecodeSource,
    PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewDecodeWorkerResources,
    PreviewHardwareDecodeBlocker, PreviewHardwareDecodeCpuTransferStatus,
    PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest,
    PreviewIsolatedDemuxExecutionEvidence, PreviewNativeDecodeFallback, PreviewNativeDecodedFrame,
    PreviewNativeDecodedFrameError, PreviewNativeDecodedFrameHandle,
    PreviewNativeDecodedFrameResource, PreviewNativeSurfaceHint, PreviewScrubAdaptiveClass,
    PreviewSeekIndexCache, PreviewSeekIndexCacheDiagnostics, PreviewSeekIndexCachePolicy,
    PreviewSeekIndexSource, PreviewSourceColorContract, PreviewTemporalExtentSource, RgbaFrame,
};
pub use process_supervisor::{
    run_supervised_command, run_supervised_command_while, SupervisedChild, SupervisedProcessError,
    SupervisedProcessOutput, SupervisedProcessPolicy, SupervisedProcessStage,
    SupervisedProcessStream, SupervisedStreamCapture,
};
pub use proxy::{
    ProxyArtifactManifest, ProxyArtifactSettings, ProxyCodec, ProxyColorContract,
    ProxyColorContractError, ProxyConfig, ProxyEncodingProfile, ProxyGenerationError,
    ProxyGenerationOutcome, ProxyGenerator, ProxyPublicationEvidence, ProxyPublicationFailure,
    ProxyPublicationFailureKind, ProxyPublicationPhase, ProxyResolution, ProxySourceFingerprint,
    ProxyStatus,
};
pub use waveform::{
    WaveformAnalysisError, WaveformEnvelope, WaveformEnvelopeBuilder, MAX_WAVEFORM_WIDTH,
};
