//! # mondrian-media
//!
//! 媒体处理核心模块。负责：
//! - 媒体文件探针（格式/编解码/元数据）
//! - 视频帧解码（当前为 FFmpeg CPU RGBA 路径；硬解/零拷贝必须通过显式 residency 诊断证明）
//! - 音频采样解码与混合
//! - 预览帧缓存与解码调度
//! - 代理文件生成（Proxy）

pub mod audio;
mod audio_device;
mod audio_output;
mod audio_playback;
pub mod audio_source;
mod camera_raw;
pub mod decoder;
mod ffmpeg_runtime;
mod ffmpeg_tools;
pub mod info;
mod media_probe_process;
pub mod multilevel_cache;
mod packet_identity;
pub mod preview;
mod process_supervisor;
pub mod proxy;
mod resident_encode;
pub mod waveform;

pub use audio::{AudioBuffer, RealtimeAudioOutputControlError, RealtimeAudioOutputSnapshot};
pub use audio_device::{
    discover_realtime_audio_output_devices, RealtimeAudioCandidateCounts,
    RealtimeAudioChannelSemantics, RealtimeAudioOutputContract, RealtimeAudioOutputDeviceCatalog,
    RealtimeAudioOutputDeviceDescriptor, RealtimeAudioOutputDeviceEvidence,
    RealtimeAudioOutputDeviceId, RealtimeAudioOutputDeviceIdError,
    RealtimeAudioOutputDeviceSelection, RealtimeAudioOutputDiscoveryFailure,
    RealtimeAudioOutputOpenFailure, RealtimeAudioOutputOpenFailureCode, RealtimeAudioSampleFormat,
    RealtimeAudioSupportedBufferSize,
};
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
pub use camera_raw::probe_camera_raw_metadata;
pub use decoder::{
    resolve_decoded_video_range, DecodedFrameResidency, DecodedGpuFrameHandleKind,
    DecodedVideoChromaLocation, DecodedVideoMatrix, DecodedVideoRange, DecodedVideoRangeContract,
    DecodedVideoSampling, DecodedVideoSurfaceChromaSubsampling, DecodedVideoSurfaceColorModel,
    DecodedVideoSurfaceDescriptor, DecodedVideoSurfaceFormat, DecodedVideoSurfaceNumericEncoding,
    DecodedVideoSurfacePlaneLayout, HwAccelBackend, HwAccelCodecConfigMethods,
    HwAccelCodecConfigProbe, HwAccelDeviceContextProbe, HwAccelDeviceSelector, HwAccelPixelFormat,
    HwAccelProbe, HwDeviceContextPool, HwDeviceContextPoolDiagnostics, HwDeviceContextPoolPolicy,
    RendererHwAccelDeviceContext, RendererHwAccelDeviceContextCreateError,
    RendererHwAccelDeviceContextInstallError,
};
pub use ffmpeg_runtime::verify_ffmpeg_runtime;
pub use ffmpeg_tools::{ffmpeg_command, ffprobe_command};
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
pub use media_probe_process::{
    prepare_media_probe_isolated, run_media_probe_worker, IsolatedMediaProbeError,
    IsolatedMediaProbeSnapshot, MEDIA_PROBE_WORKER_ARGUMENT,
};
pub use multilevel_cache::{CacheTier, MultiLevelCache, ResolvedMediaPath};
pub use packet_identity::{
    capture_video_packet_identity, capture_video_packet_identity_cancellable, VideoPacketIdentity,
    VideoPacketIdentityError,
};
pub use preview::{
    clear_thread_local_preview_decode_session, decode_preview_frame_cancellable,
    preview_decode_backend, preview_decode_cpu_budget, run_preview_demux_worker,
    CameraRawDecodeIntent, CpuYuvChromaPlaneLayout, CpuYuvChromaPlanes, CpuYuvChromaSubsampling,
    CpuYuvFrame, CpuYuvPlane, CpuYuvSampleFormat, DecodedRgbaAlphaMode, DecodedRgbaEncoding,
    DecodedRgbaFrameContract, FfmpegD3D11TextureView, FfmpegD3D12TextureView,
    FfmpegNativeDecodedFrameResource, FfmpegNativeDecodedFrameResourceError, FloatRgbaFrame,
    MediaFileChangeStamp, MediaFileFingerprint, MediaFileObjectIdentity, PreviewCompactCpuYuvHint,
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeAlphaPresence,
    PreviewDecodeBackend, PreviewDecodeCancellation, PreviewDecodeCancellationCheckpoint,
    PreviewDecodeCancellationCheckpointEvidence, PreviewDecodeCancellationEvidence,
    PreviewDecodeCancellationSource, PreviewDecodeContractError, PreviewDecodeCpuBudget,
    PreviewDecodeDiagnostics, PreviewDecodeExecutionObserver, PreviewDecodeExecutionPath,
    PreviewDecodeExecutionProgress, PreviewDecodeExecutionStage, PreviewDecodeKey,
    PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodePayloadRequirement,
    PreviewDecodeRepresentation, PreviewDecodeRequest, PreviewDecodeSeekStrategy,
    PreviewDecodeSessionContext, PreviewDecodeSessionContextBootstrap,
    PreviewDecodeSessionDisposition, PreviewDecodeSessionResidencyConfig, PreviewDecodeSource,
    PreviewDecodeStageDurations, PreviewDecodeTemporalSelection, PreviewDecodeThreadingKind,
    PreviewDecodeWorkerResources, PreviewHardwareDecodeBlocker,
    PreviewHardwareDecodeCpuTransferStatus, PreviewHardwareDecodeDecision,
    PreviewHardwareDecodeRequest, PreviewIsolatedDemuxExecutionEvidence,
    PreviewNativeDecodeFallback, PreviewNativeDecodedFrame, PreviewNativeDecodedFrameError,
    PreviewNativeDecodedFrameHandle, PreviewNativeDecodedFrameResource, PreviewNativeSurfaceHint,
    PreviewPlaybackDirection, PreviewRepresentationQuality, PreviewScrubAdaptiveClass,
    PreviewSeekIndexCache, PreviewSeekIndexCacheDiagnostics, PreviewSeekIndexCachePolicy,
    PreviewSeekIndexSource, PreviewSourceColorContract, PreviewSourceSampleIdentity,
    PreviewTemporalExtentSource, RgbaFrame,
};
#[cfg(target_os = "linux")]
pub use preview::{
    FfmpegDrmPrimeFrame, FfmpegDrmPrimeLayer, FfmpegDrmPrimeObject, FfmpegDrmPrimePlane,
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
pub use resident_encode::{
    D3D12ResidentEncodeInputFrame, D3D12ResidentEncodeReadyFrame, D3D12ResidentHevcEncoderSession,
    D3D12ResidentHevcEncoderSessionDiagnostics, ResidentEncodeBitDepth, ResidentEncodeColorimetry,
    ResidentEncodeError, ResidentHevcEncoderConfig,
};
pub use waveform::{
    WaveformAnalysisError, WaveformEnvelope, WaveformEnvelopeBuilder, MAX_WAVEFORM_WIDTH,
};
