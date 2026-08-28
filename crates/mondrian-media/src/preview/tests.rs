use super::decode_session::{external_exact_frame_is_publishable, finalize_preview_decode_outcome};
use super::{
    apply_preview_codec_threading_policy, clear_thread_local_preview_decode_session,
    convert_decoded_to_rgba, decode_preview_frame_cancellable,
    decoded_native_surface_format_from_software_format, decoded_surface_format_from_pixel,
    decoded_temporal_candidate_within_selection_distance, decoded_video_sampling_from_frame,
    default_decoder_threads_for_access_mode, duration_us,
    exact_seek_non_reference_discard_until_pts, forward_decode_work_units,
    materialize_decoded_frame, preview_create_rgba_scaler, preview_decode_interrupt_callback,
    preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode, preview_hardware_extra_frames,
    resolve_cpu_rgba_contract, run_external_decode_command_cancellable,
    select_decoded_temporal_candidate, temporal_selection_is_approximate, CpuYuvChromaPlaneLayout,
    CpuYuvChromaPlanes, DecodedRgbaFrameContract, DecodedTemporalCandidate, DecodedTemporalExtent,
    FfmpegNativeDecodedFrameResource, FfmpegNativeDecodedFrameResourceError, MediaFileChangeStamp,
    MediaFileFingerprint, MediaFileObjectIdentity, PreviewDecodeAccessMode,
    PreviewDecodeAccessPolicy, PreviewDecodeAdaptiveHints, PreviewDecodeBackend,
    PreviewDecodeCancellation, PreviewDecodeCancellationCheckpoint,
    PreviewDecodeCancellationSource, PreviewDecodeDiagnostics, PreviewDecodeExecutionObserver,
    PreviewDecodeExecutionPath, PreviewDecodeExecutionStage, PreviewDecodeInterruptState,
    PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRepresentation, PreviewDecodeRequest,
    PreviewDecodeSeekStrategy, PreviewDecodeSessionContext, PreviewDecodeSessionDisposition,
    PreviewDecodeStageDurations, PreviewDecodeThreadingConfig, PreviewDecodeThreadingKind,
    PreviewDecodedFramePayload, PreviewHardwareDecodeBlocker,
    PreviewHardwareDecodeCpuTransferStatus, PreviewHardwareDecodeDecision,
    PreviewHardwareDecodePlan, PreviewHardwareDecodeRequest, PreviewIsolatedDemuxExecutionEvidence,
    PreviewNativeDecodeFallback, PreviewNativeDecodedFrame, PreviewNativeDecodedFrameError,
    PreviewNativeDecodedFrameHandle, PreviewNativeDecodedFrameResource, PreviewPlaybackRing,
    PreviewScrubAdaptiveClass, PreviewSeekIndex, PreviewSeekIndexCache,
    PreviewSeekIndexCachePolicy, PreviewSeekIndexDiagnostics, PreviewSeekIndexSource,
    PreviewSeekResolution, PreviewSourceColorContract, RgbaFrame,
    PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES, PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES,
    PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES, PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS,
    PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES, PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
    PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS, PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES,
    PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES,
};
#[cfg(mondrian_ffmpeg_7_1)]
use super::{FfmpegAvD3D12VaFrame, FfmpegAvD3D12VaSyncContext};
use crate::decoder::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoChromaLocation,
    DecodedVideoMatrix, DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
    HwAccelBackend,
};
use crate::{DecodedRgbaEncoding, DecodedVideoRangeContract};
use ffmpeg_next as ffmpeg;
use mondrian_core::types::ColorSpace;
use mondrian_core::{
    MondrianError, Rational, SourceSampleTarget, TimelineTime, VideoCodec, VideoCodecProfile,
};
use serde::Serialize;
use std::any::Any;
use std::ffi::c_void;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn synthetic_file_fingerprint(
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
) -> MediaFileFingerprint {
    MediaFileFingerprint {
        len: Some(len),
        modified_secs: Some(modified_secs),
        modified_nanos: Some(modified_nanos),
        object_identity: Some(MediaFileObjectIdentity::Unix {
            device: 1,
            inode: len ^ modified_secs,
        }),
        change_stamp: Some(MediaFileChangeStamp::Unix {
            seconds: modified_secs as i64,
            nanoseconds: i64::from(modified_nanos),
        }),
    }
}

#[test]
fn gpu_resident_decode_reserves_external_hardware_frame_leases() {
    assert_eq!(
        preview_hardware_extra_frames(PreviewHardwareDecodeRequest::PreferGpuResident),
        PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES
    );
    assert_eq!(
        preview_hardware_extra_frames(PreviewHardwareDecodeRequest::RequireGpuResident),
        PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES
    );
    assert_eq!(
        preview_hardware_extra_frames(PreviewHardwareDecodeRequest::PreferHardwareDecode),
        0
    );
    assert_eq!(
        preview_hardware_extra_frames(PreviewHardwareDecodeRequest::Auto),
        0
    );
}

#[test]
fn decoded_temporal_extent_is_end_exclusive_and_unknown_is_point_only() {
    let extent = DecodedTemporalExtent::from_duration(1_000, 40);
    assert!(extent.covers(1_000));
    assert!(extent.covers(1_039));
    assert!(!extent.covers(1_040));

    let unknown = DecodedTemporalExtent::point(2_000);
    assert!(unknown.covers(2_000));
    assert!(!unknown.covers(2_001));
    assert!(!temporal_selection_is_approximate(2_000, Some(unknown)));
    assert!(temporal_selection_is_approximate(2_001, Some(unknown)));
    assert!(!temporal_selection_is_approximate(2_001, None));

    let mut unproven_diagnostics =
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba);
    unproven_diagnostics.selected_pts = Some(3_000);
    unproven_diagnostics.selected_duration_pts = Some(40);
    let unproven = DecodedTemporalExtent::from_diagnostics(unproven_diagnostics)
        .expect("selected PTS must retain point evidence");
    assert_eq!(unproven.source, super::PreviewTemporalExtentSource::Unknown);
    assert!(!unproven.covers(3_001));
}

#[test]
fn decoder_temporal_selection_projects_only_complete_bounded_evidence() {
    let mut diagnostics = PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba);
    diagnostics.requested_pts = Some(1_020);
    diagnostics.selected_pts = Some(1_000);
    diagnostics.selected_duration_pts = Some(40);
    diagnostics.selected_temporal_extent_source = super::PreviewTemporalExtentSource::FrameDuration;
    let selection = diagnostics.temporal_selection().expect("complete temporal selection");
    assert_eq!(selection.requested_pts, 1_020);
    assert_eq!(selection.selected_pts, 1_000);
    assert_eq!(selection.selected_duration_pts, 40);
    assert_eq!(
        selection.extent_source,
        super::PreviewTemporalExtentSource::FrameDuration
    );
    assert!(!selection.temporal_approximation);

    diagnostics.selected_temporal_extent_source = super::PreviewTemporalExtentSource::Unknown;
    assert!(diagnostics.temporal_selection().is_none());
    diagnostics.selected_temporal_extent_source = super::PreviewTemporalExtentSource::FrameDuration;
    diagnostics.selected_duration_pts = Some(0);
    assert!(diagnostics.temporal_selection().is_none());
    diagnostics.selected_pts = Some(i64::MAX);
    diagnostics.selected_duration_pts = Some(1);
    assert!(diagnostics.temporal_selection().is_none());
    diagnostics.selected_pts = Some(1_000);
    diagnostics.requested_pts = None;
    assert!(diagnostics.temporal_selection().is_none());
}

#[test]
fn real_concat_duration_selects_covering_before_even_when_successor_is_nearer() {
    let before = DecodedTemporalExtent::from_duration(735_560_143, 55_882);
    let after = DecodedTemporalExtent::from_duration(735_616_025, 20_020);

    for requested_pts in [735_574_840, 735_594_860] {
        for access_mode in [
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ] {
            let selected = select_decoded_temporal_candidate(
                requested_pts,
                access_mode,
                Some(before),
                Some(after),
            )
            .expect("covering temporal candidate");
            assert_eq!(
                selected,
                (
                    DecodedTemporalCandidate::Before,
                    before.with_successor(after.start_pts),
                )
            );
            assert!(!temporal_selection_is_approximate(
                requested_pts,
                Some(selected.1)
            ));
        }
    }
    assert!(
        after.distance_to(735_594_860) < before.distance_to(735_594_860),
        "fixture must prove nearest-PTS selection would choose the wrong future frame"
    );
}

#[test]
fn successor_boundary_proves_unknown_before_extent_without_claiming_a_gap() {
    let before = DecodedTemporalExtent::point(10_000);
    let after = DecodedTemporalExtent::point(10_040);
    let selected = select_decoded_temporal_candidate(
        10_039,
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(before),
        Some(after),
    )
    .expect("successor-proven temporal candidate");

    assert_eq!(selected.0, DecodedTemporalCandidate::Before);
    assert_eq!(selected.1.duration_pts, Some(40));
    assert!(selected.1.covers(10_039));
    assert!(!selected.1.covers(10_040));
}

#[test]
fn successor_boundary_overrides_an_overlapping_declared_duration() {
    let before = DecodedTemporalExtent::from_duration(100, 100);
    let after = DecodedTemporalExtent::from_duration(140, 20);

    let before_selection = select_decoded_temporal_candidate(
        139,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        Some(before),
        Some(after),
    )
    .expect("pre-successor request must select the predecessor");
    assert_eq!(before_selection.0, DecodedTemporalCandidate::Before);
    assert_eq!(before_selection.1.end_pts(), Some(140));

    let after_selection = select_decoded_temporal_candidate(
        150,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        Some(before),
        Some(after),
    )
    .expect("post-successor request must select the successor");
    assert_eq!(after_selection.0, DecodedTemporalCandidate::After);
    assert!(!temporal_selection_is_approximate(
        150,
        Some(after_selection.1)
    ));
}

#[test]
fn successor_boundary_extends_a_short_declared_duration_across_a_vfr_hold() {
    let before = DecodedTemporalExtent::from_duration(80, 10);
    let after = DecodedTemporalExtent::from_duration(100, 10);

    let selected = select_decoded_temporal_candidate(
        90,
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(before),
        Some(after),
    )
    .expect("the predecessor remains presented until the next decoded frame");

    assert_eq!(selected.0, DecodedTemporalCandidate::Before);
    assert_eq!(selected.1.end_pts(), Some(100));
    assert_eq!(
        selected.1.source,
        super::PreviewTemporalExtentSource::SuccessorBoundary
    );
}

#[test]
fn long_successor_proven_vfr_extent_is_not_rejected_by_nominal_distance() {
    let before = DecodedTemporalExtent::point(100);
    let after = DecodedTemporalExtent::point(1_000);
    let selected = select_decoded_temporal_candidate(
        900,
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(before),
        Some(after),
    )
    .expect("successor must prove the long VFR hold");

    assert_eq!(selected.0, DecodedTemporalCandidate::Before);
    assert!(selected.1.covers(900));
    assert!(
        decoded_temporal_candidate_within_selection_distance(selected.1, 900, 80),
        "a proven covering interval must not be constrained by a nominal-frame distance"
    );
}

#[test]
fn positive_frame_duration_is_mode_independent_without_a_successor() {
    let frame_extent = DecodedTemporalExtent::from_duration(100, 100);

    for access_mode in [
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeAccessMode::ScrubCursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    ] {
        let selected =
            select_decoded_temporal_candidate(150, access_mode, Some(frame_extent), None)
                .expect("positive frame duration must cover independently of access policy");

        assert_eq!(selected.0, DecodedTemporalCandidate::Before);
        assert_eq!(selected.1, frame_extent);
        assert!(!temporal_selection_is_approximate(150, Some(selected.1)));
    }
}

#[test]
fn successor_boundary_closes_a_short_declared_vfr_gap_for_every_access_mode() {
    let before = DecodedTemporalExtent::from_duration(100, 5);
    let after = DecodedTemporalExtent::from_duration(120, 5);
    let requested_pts = 118;

    for access_mode in [
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeAccessMode::ScrubCursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    ] {
        let selected = select_decoded_temporal_candidate(
            requested_pts,
            access_mode,
            Some(before),
            Some(after),
        )
        .expect("the predecessor remains presented until the decoded successor");
        assert_eq!(selected.0, DecodedTemporalCandidate::Before);
        assert_eq!(selected.1.end_pts(), Some(120));
        assert!(!temporal_selection_is_approximate(
            requested_pts,
            Some(selected.1)
        ));
    }
}

#[test]
fn exact_access_rejects_an_expired_declared_extent_without_successor_evidence() {
    let before = DecodedTemporalExtent::from_duration(100, 5);
    let requested_pts = 118;

    let scrub = select_decoded_temporal_candidate(
        requested_pts,
        PreviewDecodeAccessMode::ScrubCursor,
        Some(before),
        None,
    )
    .expect("nearest scrub candidate");
    assert_eq!(scrub.0, DecodedTemporalCandidate::Before);
    assert!(temporal_selection_is_approximate(
        requested_pts,
        Some(scrub.1)
    ));

    for access_mode in [
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    ] {
        assert!(
            select_decoded_temporal_candidate(requested_pts, access_mode, Some(before), None)
                .is_none(),
            "exact access still requires a successor or another covering extent"
        );
    }
}

fn test_source_color() -> PreviewSourceColorContract {
    PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
}

fn covering_decode_request<'a>(
    path: &'a Path,
    source_time: TimelineTime,
    access_mode: PreviewDecodeAccessMode,
    source_color: PreviewSourceColorContract,
) -> PreviewDecodeRequest<'a> {
    PreviewDecodeRequest::new(
        path,
        SourceSampleTarget::covering(source_time),
        access_mode,
        source_color,
    )
}

fn test_linear_source_color() -> PreviewSourceColorContract {
    PreviewSourceColorContract::automatic(ColorSpace::Aces2065_1, DecodedVideoRange::Full)
}

#[test]
fn data_texture_source_contract_round_trips_and_is_a_distinct_cache_identity() {
    let data = PreviewSourceColorContract::data_texture(DecodedVideoRangeContract::OverrideFull)
        .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709);
    let color = PreviewSourceColorContract::new(
        ColorSpace::LinearRec2020,
        DecodedVideoRangeContract::OverrideFull,
    );
    let encoded = serde_json::to_string(&data).expect("serialize DataTexture contract");
    let decoded: PreviewSourceColorContract =
        serde_json::from_str(&encoded).expect("deserialize DataTexture contract");

    assert_eq!(decoded, data);
    assert!(decoded.is_data_texture());
    assert_eq!(decoded.color_space(), None);
    assert!(!decoded.is_scene_linear());
    assert_eq!(
        decoded.yuv_matrix_fallback, None,
        "numeric RGB never accepts a YUV color-matrix fallback"
    );
    let identities = std::collections::HashSet::from([data, color]);
    assert_eq!(identities.len(), 2);
}

fn test_rgba_contract() -> DecodedRgbaFrameContract {
    DecodedRgbaFrameContract::source_encoded(
        test_source_color(),
        DecodedVideoMatrix::Bt709,
        DecodedVideoRange::Limited,
    )
}

#[derive(Debug)]
struct TestNativeDecodedFrameResource {
    kind: DecodedGpuFrameHandleKind,
    id: NonZeroU64,
    drops: Option<Arc<AtomicUsize>>,
}

impl PreviewNativeDecodedFrameResource for TestNativeDecodedFrameResource {
    fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
        self.kind
    }

    fn handle_id(&self) -> NonZeroU64 {
        self.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for TestNativeDecodedFrameResource {
    fn drop(&mut self) {
        if let Some(drops) = &self.drops {
            drops.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn test_native_handle(kind: DecodedGpuFrameHandleKind, id: u64) -> PreviewNativeDecodedFrameHandle {
    PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
        kind,
        id: NonZeroU64::new(id).expect("test native handle id must be non-zero"),
        drops: None,
    })
}

fn synthetic_d3d11_frame(
    software_format: ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::util::frame::video::Video {
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D11);
    frame.set_width(1920);
    frame.set_height(1080);
    frame.set_color_range(ffmpeg::util::color::Range::MPEG);
    // SAFETY: Every AVBufferRef assigned to the synthetic frame is owned by
    // the frame and released by its normal drop. The hardware-context bytes
    // are initialized before being read, and native pointers are never
    // dereferenced by these media contract tests.
    unsafe {
        let raw = frame.as_mut_ptr();
        let surface = ffmpeg::ffi::av_buffer_alloc(1);
        assert!(
            !surface.is_null(),
            "test surface buffer allocation must succeed"
        );
        (*raw).buf[0] = surface;
        (*raw).data[0] = std::ptr::NonNull::<u8>::dangling().as_ptr();
        (*raw).data[1] = 2usize as *mut u8;

        let context =
            ffmpeg::ffi::av_buffer_alloc(std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>());
        assert!(
            !context.is_null(),
            "test hardware context allocation must succeed"
        );
        std::ptr::write_bytes(
            (*context).data,
            0,
            std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>(),
        );
        (*((*context).data.cast::<ffmpeg::ffi::AVHWFramesContext>())).sw_format = software_format;
        (*raw).hw_frames_ctx = context;
        (*raw).chroma_location = ffmpeg::util::chroma::Location::Left.into();
    }
    frame
}

#[cfg(mondrian_ffmpeg_7_1)]
fn synthetic_d3d12_frame(
    software_format: ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::util::frame::video::Video {
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D12);
    frame.set_width(3840);
    frame.set_height(2160);
    frame.set_color_range(ffmpeg::util::color::Range::MPEG);
    // SAFETY: Both AVBufferRefs are owned by the frame. data[0] points into
    // buf[0], so cloning the AVFrame also retains the synthetic descriptor.
    // The fake COM pointers are only checked for non-null and never called.
    unsafe {
        let raw = frame.as_mut_ptr();
        let descriptor = ffmpeg::ffi::av_buffer_alloc(std::mem::size_of::<FfmpegAvD3D12VaFrame>());
        assert!(
            !descriptor.is_null(),
            "test D3D12 descriptor allocation must succeed"
        );
        let native = (*descriptor).data.cast::<FfmpegAvD3D12VaFrame>();
        native.write(FfmpegAvD3D12VaFrame {
            texture: std::ptr::NonNull::<u8>::dangling().as_ptr().cast::<c_void>(),
            sync_ctx: FfmpegAvD3D12VaSyncContext {
                fence: std::ptr::NonNull::<u16>::dangling().as_ptr().cast::<c_void>(),
                event: std::ptr::null_mut(),
                fence_value: 9,
            },
        });
        (*raw).buf[0] = descriptor;
        (*raw).data[0] = native.cast::<u8>();

        let context =
            ffmpeg::ffi::av_buffer_alloc(std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>());
        assert!(
            !context.is_null(),
            "test hardware context allocation must succeed"
        );
        std::ptr::write_bytes(
            (*context).data,
            0,
            std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>(),
        );
        (*((*context).data.cast::<ffmpeg::ffi::AVHWFramesContext>())).sw_format = software_format;
        (*raw).hw_frames_ctx = context;
        (*raw).chroma_location = ffmpeg::util::chroma::Location::Left.into();
    }
    frame
}

#[test]
fn preview_decode_backend_defaults_to_auto_without_a_process_global_setter() {
    assert_eq!(super::preview_decode_backend(), PreviewDecodeBackend::Auto);
}

#[test]
fn external_ffmpeg_cpu_rgba_policy_is_still_frame_only() {
    assert!(!preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
        PreviewDecodeAccessMode::PlaybackCursor
    ));
    assert!(!preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
        PreviewDecodeAccessMode::ScrubCursor
    ));
    assert!(preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
        PreviewDecodeAccessMode::RandomAccessStillFrame
    ));
}

#[test]
fn preview_decode_request_defaults_to_auto_hardware_decode() {
    let request = PreviewDecodeRequest::new(
        Path::new("clip.mov"),
        SourceSampleTarget::covering(TimelineTime::ZERO),
        PreviewDecodeAccessMode::PlaybackCursor,
        test_source_color(),
    );

    assert_eq!(
        request.hardware_decode_request,
        PreviewHardwareDecodeRequest::Auto
    );
    assert_eq!(
        request
            .with_hardware_decode_request(PreviewHardwareDecodeRequest::PreferHardwareDecode)
            .hardware_decode_request,
        PreviewHardwareDecodeRequest::PreferHardwareDecode
    );
    assert_eq!(
        request
            .with_hardware_decode_request(PreviewHardwareDecodeRequest::PreferGpuResident)
            .hardware_decode_request,
        PreviewHardwareDecodeRequest::PreferGpuResident
    );
}

#[test]
fn exact_source_sample_lowers_to_stream_pts_with_start_offset() {
    assert_eq!(
        super::source_sample_to_stream_pts(
            SourceSampleTarget::covering(TimelineTime::new(1, 3).expect("exact source time")),
            ffmpeg::Rational(1, 90_000),
            9_000,
        )
        .expect("valid stream target"),
        39_000
    );
}

#[test]
fn source_sample_boundary_controls_half_open_grid_lowering() {
    assert_eq!(
        super::source_sample_to_stream_pts(
            SourceSampleTarget::covering(TimelineTime::new(1, 2).expect("exact source time")),
            ffmpeg::Rational(1, 1),
            0,
        )
        .expect("valid stream target"),
        0
    );
    assert_eq!(
        super::source_sample_to_stream_pts(
            SourceSampleTarget::strict_predecessor(
                TimelineTime::new(1, 1).expect("exact source boundary"),
            ),
            ffmpeg::Rational(1, 1),
            0,
        )
        .expect("valid strict-predecessor target"),
        0
    );
}

#[test]
fn exact_source_time_preserves_long_duration_without_float_drift() {
    assert_eq!(
        super::source_sample_to_stream_pts(
            SourceSampleTarget::covering(
                TimelineTime::new(360_000, 1).expect("exact source time"),
            ),
            ffmpeg::Rational(1, 90_000),
            0,
        )
        .expect("valid stream target"),
        32_400_000_000
    );
}

#[test]
fn exact_source_time_rejects_negative_targets_and_invalid_time_bases() {
    assert!(super::source_sample_to_stream_pts(
        SourceSampleTarget::covering(TimelineTime::new(-1, 1).expect("exact source time")),
        ffmpeg::Rational(1, 90_000),
        0,
    )
    .is_err());
    assert!(super::source_sample_to_stream_pts(
        SourceSampleTarget::covering(TimelineTime::ZERO),
        ffmpeg::Rational(0, 1),
        0,
    )
    .is_err());
}

#[test]
fn external_ffmpeg_argument_is_lowered_only_at_the_cli_adapter() {
    assert_eq!(
        super::ffmpeg_source_time_arg(
            SourceSampleTarget::covering(TimelineTime::new(5, 4).expect("exact source time")),
            ffmpeg::Rational(1, 100),
        )
        .expect("valid CLI target"),
        "1.250000"
    );
}

#[test]
fn hardware_decode_plan_does_not_report_native_before_frame_is_observed() {
    let plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );

    assert_eq!(
        plan.request,
        PreviewHardwareDecodeRequest::PreferGpuResident
    );
    assert_ne!(
        plan.decision,
        PreviewHardwareDecodeDecision::GpuResidentNative
    );
    assert_eq!(plan.probe.frame_residency, DecodedFrameResidency::CpuRgba);
    assert!(!plan.probe.hardware_decode_active);
    assert!(!plan.probe.zero_copy_active);
    let expected_adapter_available = match plan.ffmpeg_codec_config.hw_pixel_format {
        Some(
            crate::decoder::HwAccelPixelFormat::D3D12 | crate::decoder::HwAccelPixelFormat::D3D11,
        ) => {
            cfg!(target_os = "windows")
        }
        Some(crate::decoder::HwAccelPixelFormat::VideoToolbox) => cfg!(target_os = "macos"),
        Some(crate::decoder::HwAccelPixelFormat::Vaapi) => cfg!(target_os = "linux"),
        _ => false,
    };
    assert_eq!(
        plan.probe.decoder_adapter_available,
        expected_adapter_available
    );
}

#[test]
fn hardware_decode_plan_rejects_h264_high422_before_device_setup() {
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );

    plan.apply_stream_profile(ffmpeg::codec::Profile::H264(
        ffmpeg::codec::profile::H264::High422,
    ));

    assert_eq!(
        plan.decision,
        PreviewHardwareDecodeDecision::CpuRgbaCodecUnsupported
    );
    assert!(!plan.ffmpeg_codec_config.ffmpeg_codec_config_available);
    assert!(!plan.should_configure_hardware_decoder(PreviewDecodeAccessMode::PlaybackCursor));
    assert!(plan.probe.reason.contains("High422"));
}

#[test]
fn hardware_decode_plan_can_prefer_cpu_transfer_without_requiring_native_residency() {
    let plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferHardwareDecode,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );

    assert_eq!(
        plan.request,
        PreviewHardwareDecodeRequest::PreferHardwareDecode
    );
    if plan.ffmpeg_codec_config.ffmpeg_codec_config_available {
        assert!(!plan.ffmpeg_device_context.device_create_attempted);
        assert!(!plan.ffmpeg_device_context.device_context_created);
        assert!(plan.ffmpeg_device_context.reason.contains("owning decode Session"));
    }
}

#[test]
fn hardware_decode_plan_configures_required_gpu_without_cpu_transfer_fallback() {
    let plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::RequireGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );

    assert_eq!(
        plan.request,
        PreviewHardwareDecodeRequest::RequireGpuResident
    );
    assert!(!plan.allows_cpu_transfer_fallback());
    if plan.ffmpeg_codec_config.ffmpeg_codec_config_available {
        assert!(!plan.ffmpeg_device_context.device_create_attempted);
        assert!(!plan.ffmpeg_device_context.device_context_created);
        assert!(plan.ffmpeg_device_context.reason.contains("owning decode Session"));
    }
}

#[test]
fn hardware_decode_plan_supports_interactive_access_modes() {
    for access_mode in [
        PreviewDecodeAccessMode::ScrubCursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    ] {
        assert!(PreviewHardwareDecodePlan::plan_requires_device_context(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            access_mode,
            PreviewDecodeBackend::Auto,
        ));
    }
}

#[test]
fn hardware_decode_plan_advances_to_the_next_static_candidate_after_session_failure() {
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferHardwareDecode,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );
    let first = plan.probe.candidate_backend;
    plan.mark_device_context_setup_failed(crate::decoder::HwAccelDeviceContextProbe::deferred(
        first.unwrap_or(HwAccelBackend::None),
        true,
        "test first-backend failure",
    ));

    if plan.advance_hardware_candidate(
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
    ) {
        assert_ne!(plan.probe.candidate_backend, first);
        assert_eq!(
            plan.hardware_cpu_transfer_status,
            PreviewHardwareDecodeCpuTransferStatus::NotAttempted
        );
    }
}

#[test]
fn hardware_decode_plan_marks_external_cpu_rgba_backend_boundary() {
    let plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::ExternalFfmpegCpuRgba,
        ffmpeg::codec::Id::H264,
        None,
    );

    assert_eq!(
        plan.decision,
        PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary
    );
}

#[test]
fn hardware_decode_cpu_transfer_is_active_but_not_gpu_resident() {
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );

    plan.mark_hardware_cpu_transfer_configured(HwAccelBackend::D3D11VA);
    assert!(plan.hardware_cpu_transfer_configured);
    assert!(!plan.hardware_cpu_transfer_observed);
    assert_eq!(
        plan.hardware_cpu_transfer_status,
        PreviewHardwareDecodeCpuTransferStatus::ConfiguredAwaitingFrame
    );
    assert!(!plan.probe.hardware_decode_active);
    assert!(!plan.probe.zero_copy_active);
    assert_eq!(plan.probe.frame_residency, DecodedFrameResidency::CpuRgba);
    assert_eq!(
        PreviewHardwareDecodeBlocker::from_probe(&plan.probe),
        PreviewHardwareDecodeBlocker::TextureResidencyNotConnected
    );

    plan.mark_hardware_cpu_transfer_observed();
    assert!(plan.hardware_cpu_transfer_observed);
    assert_eq!(
        plan.hardware_cpu_transfer_status,
        PreviewHardwareDecodeCpuTransferStatus::Observed
    );
    assert_eq!(
        plan.decision,
        PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer
    );
    assert_eq!(
        plan.probe.selected_backend,
        plan.probe.candidate_backend.unwrap_or(HwAccelBackend::None)
    );
    assert!(plan.probe.hardware_decode_active);
    assert!(!plan.probe.zero_copy_active);
}

#[test]
fn hardware_decode_cpu_transfer_setup_states_are_structured() {
    let mut setup_failed = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );
    setup_failed.mark_hardware_cpu_transfer_setup_failed();
    assert_eq!(
        setup_failed.hardware_cpu_transfer_status,
        PreviewHardwareDecodeCpuTransferStatus::SetupFailed
    );
    assert!(!setup_failed.hardware_cpu_transfer_configured);
    assert!(!setup_failed.hardware_cpu_transfer_observed);

    let mut open_failed = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::PreferGpuResident,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Auto,
        ffmpeg::codec::Id::H264,
        None,
    );
    open_failed.mark_hardware_cpu_transfer_configured(HwAccelBackend::D3D11VA);
    open_failed.mark_hardware_cpu_transfer_decoder_open_failed();
    assert_eq!(
        open_failed.hardware_cpu_transfer_status,
        PreviewHardwareDecodeCpuTransferStatus::DecoderOpenFailed
    );
    assert!(open_failed.hardware_cpu_transfer_configured);
    assert!(!open_failed.hardware_cpu_transfer_observed);
}

#[test]
fn preview_decode_threading_kind_names_and_env_values_are_stable() {
    assert_eq!(PreviewDecodeThreadingKind::None.as_str(), "None");
    assert_eq!(PreviewDecodeThreadingKind::Frame.as_str(), "Frame");
    assert_eq!(PreviewDecodeThreadingKind::Slice.as_str(), "Slice");
    assert_eq!(
        PreviewDecodeThreadingKind::default(),
        PreviewDecodeThreadingKind::Frame
    );
    assert_eq!(
        PreviewDecodeThreadingKind::from_env("off"),
        Some(PreviewDecodeThreadingKind::None)
    );
    assert_eq!(
        PreviewDecodeThreadingKind::from_env("frame"),
        Some(PreviewDecodeThreadingKind::Frame)
    );
    assert_eq!(
        PreviewDecodeThreadingKind::from_env("slice"),
        Some(PreviewDecodeThreadingKind::Slice)
    );
    assert_eq!(PreviewDecodeThreadingKind::from_env("surprise"), None);
}

#[test]
fn exr_decode_policy_disables_frame_thread_shutdown_deadlock() {
    let requested =
        PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::Frame, count: 8 };
    assert_eq!(
        apply_preview_codec_threading_policy(ffmpeg::codec::Id::EXR, requested),
        PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::None, count: 1 }
    );
    assert_eq!(
        apply_preview_codec_threading_policy(ffmpeg::codec::Id::H264, requested),
        requested
    );
}

#[test]
fn software_decode_threading_defaults_to_frame_pipelining_for_every_access_mode() {
    let playback = super::PreviewDecodeAccessMode::PlaybackCursor;
    let scrub = super::PreviewDecodeAccessMode::ScrubCursor;
    assert_eq!(
        super::default_threading_kind_for_software_decode(playback, 3_840 * 2_160),
        super::PreviewDecodeThreadingKind::Frame
    );
    assert_eq!(
        super::default_threading_kind_for_software_decode(playback, 1_920 * 1_080),
        super::PreviewDecodeThreadingKind::Frame
    );
    assert_eq!(
        super::default_threading_kind_for_software_decode(scrub, 3_840 * 2_160),
        super::PreviewDecodeThreadingKind::Frame
    );
}

#[test]
fn preview_decode_cpu_budget_coordinates_workers_and_decoder_threads() {
    let small = super::PreviewDecodeCpuBudget::for_available_parallelism(4);
    assert_eq!(small.preview_worker_count, 1);
    assert_eq!(small.decoder_threads_per_worker, 3);
    assert_eq!(small.reserved_interactive_threads, 1);

    let common = super::PreviewDecodeCpuBudget::for_available_parallelism(8);
    assert_eq!(common.preview_worker_count, 2);
    assert_eq!(common.decoder_threads_per_worker, 3);
    assert_eq!(common.max_decoder_threads_per_worker, 6);
    assert_eq!(common.reserved_interactive_threads, 2);

    let large = super::PreviewDecodeCpuBudget::for_available_parallelism(24);
    assert_eq!(large.preview_worker_count, 3);
    assert_eq!(large.decoder_threads_per_worker, 7);
    assert_eq!(large.max_decoder_threads_per_worker, 12);
    assert_eq!(large.reserved_interactive_threads, 2);

    let workstation = super::PreviewDecodeCpuBudget::for_available_parallelism(32);
    assert_eq!(workstation.preview_worker_count, 3);
    assert_eq!(workstation.decoder_threads_per_worker, 10);
    assert_eq!(workstation.max_decoder_threads_per_worker, 12);

    assert_eq!(
        default_decoder_threads_for_access_mode(
            workstation,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        12
    );
    assert_eq!(
        default_decoder_threads_for_access_mode(
            workstation,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        10
    );
}

#[test]
fn worker_thread_limit_caps_playback_failover_without_changing_threading_kind() {
    let requested =
        PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::Slice, count: 10 };
    assert_eq!(
        super::cap_preview_decode_threading_config(requested, Some(3)),
        PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::Slice, count: 3 }
    );
    assert_eq!(
        super::cap_preview_decode_threading_config(requested, None),
        requested
    );
    assert_eq!(
        super::cap_preview_decode_threading_config(requested, Some(0)).count,
        1
    );
}

#[test]
fn preview_decode_access_mode_names_and_defaults_are_stable() {
    assert_eq!(
        PreviewDecodeAccessMode::PlaybackCursor.as_str(),
        "PlaybackCursor"
    );
    assert_eq!(PreviewDecodeAccessMode::ScrubCursor.as_str(), "ScrubCursor");
    assert_eq!(
        PreviewDecodeAccessMode::RandomAccessStillFrame.as_str(),
        "RandomAccessStillFrame"
    );
    assert_eq!(
        PreviewDecodeSeekStrategy::KeyframeBefore.as_str(),
        "KeyframeBefore"
    );
    assert_eq!(
        PreviewDecodeSeekStrategy::BoundedAnyFrame.as_str(),
        "BoundedAnyFrame"
    );
}

#[test]
fn preview_decode_rgba_request_preserves_explicit_contract_fields() {
    let path = PathBuf::from("E:/media/source.mov");
    let fingerprint = synthetic_file_fingerprint(10, 20, 30);
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::new(5, 4).expect("exact source time"),
        PreviewDecodeAccessMode::ScrubCursor,
        test_source_color(),
    )
    .with_max_size(Some(640), Some(360))
    .with_fingerprint(fingerprint)
    .with_adaptive_hints(PreviewDecodeAdaptiveHints {
        scrub_class: PreviewScrubAdaptiveClass::HotRegion,
        ..PreviewDecodeAdaptiveHints::default()
    });

    assert_eq!(request.path, path.as_path());
    assert_eq!(
        request.source_sample.time(),
        TimelineTime::new(5, 4).expect("exact source time")
    );
    assert_eq!(request.max_width, Some(640));
    assert_eq!(request.max_height, Some(360));
    assert_eq!(request.access_mode, PreviewDecodeAccessMode::ScrubCursor);
    assert_eq!(request.fingerprint, Some(fingerprint));
    assert_eq!(
        request.adaptive_hints.scrub_class,
        PreviewScrubAdaptiveClass::HotRegion
    );
}

#[test]
fn preview_decode_access_mode_policies_are_distinct() {
    let playback =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
    let scrub = PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
    let still =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame);

    assert_eq!(
        playback.forward_reuse_frame_window,
        PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES
    );
    assert_eq!(
        playback.forward_decode_budget_frames,
        PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES
    );
    assert!(playback.use_playback_ring);
    assert!(!playback.keyframe_only);
    assert_eq!(
        playback.seek_strategy,
        PreviewDecodeSeekStrategy::KeyframeBefore
    );
    assert_eq!(playback.any_seek_window_ms, 0);

    assert_eq!(
        scrub.forward_reuse_frame_window,
        PREVIEW_SCRUB_FORWARD_REUSE_FRAMES
    );
    assert_eq!(
        scrub.forward_decode_budget_frames,
        PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES
    );
    assert!(scrub.forward_decode_budget_frames < playback.forward_decode_budget_frames);
    assert!(!scrub.use_playback_ring);
    assert!(scrub.keyframe_only);
    assert_eq!(
        scrub.seek_strategy,
        PreviewDecodeSeekStrategy::BoundedAnyFrame
    );
    assert_eq!(scrub.any_seek_window_ms, PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS);

    assert_eq!(still.forward_reuse_frame_window, 0);
    assert_eq!(
        still.forward_decode_budget_frames,
        PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES
    );
    assert!(!still.use_playback_ring);
    assert!(!still.keyframe_only);
    assert_eq!(
        still.seek_strategy,
        PreviewDecodeSeekStrategy::KeyframeBefore
    );
    assert_eq!(still.any_seek_window_ms, 0);
}

#[test]
fn exact_still_seek_discards_only_the_distant_non_reference_prefix() {
    let still =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame);
    let playback =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
    let scrub = PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);

    assert_eq!(
        exact_seek_non_reference_discard_until_pts(still, 10_000, 40),
        Some(7_440)
    );
    assert_eq!(
        exact_seek_non_reference_discard_until_pts(playback, 10_000, 40),
        None
    );
    assert_eq!(
        exact_seek_non_reference_discard_until_pts(scrub, 10_000, 40),
        None
    );
}

#[test]
fn forward_decode_budget_counts_packets_whose_output_was_discarded() {
    assert_eq!(forward_decode_work_units(48, 400), 400);
    assert_eq!(forward_decode_work_units(400, 48), 400);
}

#[test]
fn preview_decode_access_policy_budget_exhaustion_is_inclusive() {
    let scrub = PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);

    assert!(!scrub
        .forward_decode_budget_exhausted(scrub.forward_decode_budget_frames.saturating_sub(1)));
    assert!(scrub.forward_decode_budget_exhausted(scrub.forward_decode_budget_frames));
    assert!(scrub.forward_decode_budget_exhausted(scrub.forward_decode_budget_frames + 1));
}

#[test]
fn preview_decode_access_policy_adapts_scrub_budget_from_seek_index() {
    let frame_duration_pts = 10;
    let scrub = PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
    let playback =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
    let probe_index = PreviewSeekIndex::from_probe_keyframes(vec![0, 300, 600]);

    let close_scrub = scrub.adapt_for_request(
        &probe_index,
        40,
        frame_duration_pts,
        PreviewDecodeAdaptiveHints::default(),
    );
    assert_eq!(
        close_scrub.forward_decode_budget_frames,
        PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES
    );

    let near_next_keyframe_scrub = scrub.adapt_for_request(
        &probe_index,
        290,
        frame_duration_pts,
        PreviewDecodeAdaptiveHints::default(),
    );
    assert_eq!(
        near_next_keyframe_scrub.forward_decode_budget_frames,
        PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES
    );

    let unindexed_scrub = scrub.adapt_for_request(
        &PreviewSeekIndex::default(),
        290,
        frame_duration_pts,
        PreviewDecodeAdaptiveHints::default(),
    );
    assert_eq!(
        unindexed_scrub.forward_decode_budget_frames,
        PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES
    );

    let playback_after_adapt = playback.adapt_for_request(
        &probe_index,
        290,
        frame_duration_pts,
        PreviewDecodeAdaptiveHints::default(),
    );
    assert_eq!(
        playback_after_adapt.forward_decode_budget_frames,
        playback.forward_decode_budget_frames
    );
}

#[test]
fn scrub_policy_applies_adaptive_latency_hints() {
    let frame_duration_pts = 1;
    let scrub = PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
    let probe_index = PreviewSeekIndex::from_probe_keyframes(vec![0, 240]);

    let slow = scrub.adapt_for_request(
        &probe_index,
        120,
        frame_duration_pts,
        PreviewDecodeAdaptiveHints {
            scrub_class: PreviewScrubAdaptiveClass::SlowLatency,
            ..PreviewDecodeAdaptiveHints::default()
        },
    );
    assert_eq!(
        slow.scrub_adaptive_class,
        PreviewScrubAdaptiveClass::SlowLatency
    );
    assert_eq!(
        slow.seek_strategy,
        PreviewDecodeSeekStrategy::BoundedAnyFrame
    );
    assert!(slow.any_seek_window_ms > 0);
    assert!(slow.forward_decode_budget_frames > 0);

    let hot = scrub.adapt_for_request(
        &probe_index,
        120,
        frame_duration_pts,
        PreviewDecodeAdaptiveHints {
            scrub_class: PreviewScrubAdaptiveClass::HotRegion,
            ..PreviewDecodeAdaptiveHints::default()
        },
    );
    assert_eq!(
        hot.scrub_adaptive_class,
        PreviewScrubAdaptiveClass::HotRegion
    );
    assert!(hot.forward_decode_budget_frames <= PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES);
    assert!(hot.any_seek_window_ms <= PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS);
}

#[test]
fn preview_decode_access_policy_forward_reuse_is_mode_specific() {
    let playback =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
    let scrub = PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
    let still =
        PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame);
    let frame_duration = 100;
    let last_pts = 1_000;

    assert!(playback.can_continue_forward(
        last_pts,
        last_pts + frame_duration * PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES,
        frame_duration,
        false
    ));
    assert!(!playback.can_continue_forward(
        last_pts,
        last_pts + frame_duration * (PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES + 1),
        frame_duration,
        false
    ));
    assert!(scrub.can_continue_forward(
        last_pts,
        last_pts + frame_duration * PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
        frame_duration,
        false
    ));
    assert!(!scrub.can_continue_forward(
        last_pts,
        last_pts + frame_duration * (PREVIEW_SCRUB_FORWARD_REUSE_FRAMES + 1),
        frame_duration,
        false
    ));
    assert!(
        !playback.can_continue_forward(last_pts, last_pts, frame_duration, false),
        "an exact target equal to the decoder cursor requires retained-frame reuse or a fresh seek"
    );
    assert!(!still.can_continue_forward(last_pts, last_pts, frame_duration, false));
    assert!(!playback.can_continue_forward(
        last_pts,
        last_pts - frame_duration,
        frame_duration,
        false
    ));
    assert!(!playback.can_continue_forward(
        last_pts,
        last_pts + frame_duration,
        frame_duration,
        true
    ));
}

#[test]
fn preview_seek_index_records_distinct_keyframe_packets() {
    let mut index = PreviewSeekIndex::default();

    index.observe_packet(&test_packet(Some(200), None, true));
    index.observe_packet(&test_packet(Some(100), None, true));
    index.observe_packet(&test_packet(Some(200), None, true));
    index.observe_packet(&test_packet(Some(150), None, false));
    index.observe_packet(&test_packet(None, Some(50), true));

    assert_eq!(index.keyframe_at_or_before(49), None);
    assert_eq!(index.keyframe_at_or_before(50), Some(50));
    assert_eq!(index.keyframe_at_or_before(199), Some(100));
    assert_eq!(index.keyframe_at_or_before(200), Some(200));
    assert_eq!(index.keyframe_at_or_before(1_000), Some(200));
    assert_eq!(index.adjacent_keyframe_radius(50), Some(50));
    assert_eq!(index.adjacent_keyframe_radius(100), Some(100));
    assert_eq!(index.adjacent_keyframe_radius(150), Some(50));
    assert_eq!(index.adjacent_keyframe_radius(200), Some(100));
    assert_eq!(
        index.diagnostics(),
        PreviewSeekIndexDiagnostics {
            available: true,
            keyframes: 3,
            observed_packets: 5,
            source: PreviewSeekIndexSource::SessionObserved,
        }
    );
}

#[test]
fn preview_seek_index_uses_decode_timestamp_for_reordered_keyframe_anchor() {
    let mut index = PreviewSeekIndex::from_probe_keyframes(vec![-41]);

    index.observe_packet(&test_packet(Some(10), Some(-41), true));

    assert_eq!(index.keyframe_at_or_before(10), Some(-41));
    assert_eq!(index.keyframe_after(-41), None);
    assert_eq!(index.diagnostics().keyframes, 1);
}

#[test]
fn preview_seek_index_can_be_seeded_from_probe_keyframes() {
    let index = PreviewSeekIndex::from_probe_keyframes(vec![200, 100, 200]);

    assert_eq!(index.keyframe_at_or_before(99), None);
    assert_eq!(index.keyframe_at_or_before(100), Some(100));
    assert_eq!(index.keyframe_at_or_before(150), Some(100));
    assert_eq!(index.keyframe_at_or_before(200), Some(200));
    assert_eq!(
        index.diagnostics(),
        PreviewSeekIndexDiagnostics {
            available: true,
            keyframes: 2,
            observed_packets: 0,
            source: PreviewSeekIndexSource::ProbeBacked,
        }
    );
}

#[test]
fn preview_seek_index_cache_is_keyed_by_path_fingerprint_and_stream() {
    let path = Path::new("cache-keyed-video.mov");
    let fingerprint = synthetic_file_fingerprint(10, 20, 30);
    let cache = PreviewSeekIndexCache::default();

    cache.put(path, fingerprint, 1, &[300, 100, 300]);

    assert!(cache.get(path, fingerprint, 0).is_none());
    assert!(cache.get(Path::new("other-video.mov"), fingerprint, 1).is_none());

    let index = cache
        .get(path, fingerprint, 1)
        .expect("probe-backed seek index should round-trip through cache");
    assert_eq!(index.keyframe_at_or_before(250), Some(100));
    assert_eq!(index.keyframe_at_or_before(400), Some(300));
    assert_eq!(
        index.diagnostics(),
        PreviewSeekIndexDiagnostics {
            available: true,
            keyframes: 2,
            observed_packets: 0,
            source: PreviewSeekIndexSource::ProbeBacked,
        }
    );
}

#[test]
fn preview_seek_index_cache_trims_online_by_lru_entry_and_anchor_budgets() {
    let first_path = Path::new("first.mov");
    let second_path = Path::new("second.mov");
    let first_fingerprint = synthetic_file_fingerprint(11, 21, 31);
    let second_fingerprint = synthetic_file_fingerprint(12, 22, 32);
    let cache =
        PreviewSeekIndexCache::new(PreviewSeekIndexCachePolicy::new(4, usize::MAX, usize::MAX));

    cache.put(first_path, first_fingerprint, 0, &[10, 20]);
    cache.put(second_path, second_fingerprint, 0, &[30, 40]);
    assert!(cache.get(first_path, first_fingerprint, 0).is_some());

    let initial_revision = cache.diagnostics().policy_revision;
    let trimmed_policy = PreviewSeekIndexCachePolicy::new(1, usize::MAX, usize::MAX);
    cache.reconfigure(trimmed_policy);
    assert_eq!(cache.diagnostics().entries, 1);
    assert!(cache.get(first_path, first_fingerprint, 0).is_some());
    assert!(cache.get(second_path, second_fingerprint, 0).is_none());
    assert_eq!(cache.diagnostics().policy_revision, initial_revision + 1);

    cache.reconfigure(trimmed_policy);
    assert_eq!(cache.diagnostics().policy_revision, initial_revision + 1);

    let anchor_bounded =
        PreviewSeekIndexCache::new(PreviewSeekIndexCachePolicy::new(4, 3, usize::MAX));
    anchor_bounded.put(first_path, first_fingerprint, 0, &[10, 20]);
    anchor_bounded.put(second_path, second_fingerprint, 0, &[30, 40]);
    let diagnostics = anchor_bounded.diagnostics();
    assert_eq!(diagnostics.entries, 1);
    assert_eq!(diagnostics.retained_anchors, 2);
    assert_eq!(diagnostics.evictions, 1);
}

#[test]
fn preview_seek_index_cache_rejects_oversize_entries_and_shares_one_owner() {
    let path = Path::new("shared.mov");
    let fingerprint = synthetic_file_fingerprint(13, 23, 33);
    let cache = PreviewSeekIndexCache::default();
    let shared = cache.clone();

    cache.put(path, fingerprint, 0, &[10]);
    assert!(shared.get(path, fingerprint, 0).is_some());
    assert_eq!(cache.diagnostics().hits, 1);

    let byte_bounded = PreviewSeekIndexCache::new(PreviewSeekIndexCachePolicy::new(4, 4, 1));
    byte_bounded.put(path, fingerprint, 0, &[10]);
    assert!(byte_bounded.get(path, fingerprint, 0).is_none());
    let diagnostics = byte_bounded.diagnostics();
    assert_eq!(diagnostics.entries, 0);
    assert_eq!(diagnostics.rejected_entries, 1);
}

#[test]
fn rgba_frame_diagnostics_record_seek_index_evidence() {
    let frame = RgbaFrame::new(
        1,
        1,
        vec![0; 4],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    )
    .with_access_mode(PreviewDecodeAccessMode::ScrubCursor)
    .with_seek_index_diagnostics(
        PreviewSeekIndexDiagnostics {
            available: true,
            keyframes: 4,
            observed_packets: 12,
            source: PreviewSeekIndexSource::ProbeBacked,
        },
        PreviewSeekResolution { used_index: true, anchor_pts: Some(240) },
    );

    assert!(frame.diagnostics.seek_index_available);
    assert_eq!(
        frame.diagnostics.access_mode,
        PreviewDecodeAccessMode::ScrubCursor
    );
    assert_eq!(
        frame.diagnostics.seek_strategy,
        PreviewDecodeSeekStrategy::BoundedAnyFrame
    );
    assert_eq!(
        frame.diagnostics.forward_reuse_frame_window,
        PREVIEW_SCRUB_FORWARD_REUSE_FRAMES
    );
    assert_eq!(
        frame.diagnostics.forward_decode_budget_frames,
        PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES as u32
    );
    assert_eq!(
        frame.diagnostics.any_seek_window_ms,
        PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS
    );
    assert_eq!(frame.diagnostics.seek_index_keyframes, 4);
    assert_eq!(frame.diagnostics.seek_index_observed_packets, 12);
    assert_eq!(
        frame.diagnostics.seek_index_source,
        PreviewSeekIndexSource::ProbeBacked
    );
    assert!(frame.diagnostics.seek_index_used);
    assert_eq!(frame.diagnostics.seek_index_anchor_pts, Some(240));
}

#[test]
fn rgba_frame_diagnostics_record_hw_accel_probe_fail_closed() {
    let probe = HwAccelBackend::probe();
    let mut frame = RgbaFrame::new(
        1,
        1,
        vec![0; 4],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );
    frame.diagnostics = frame.diagnostics.with_hw_accel_probe(&probe);

    assert_eq!(frame.diagnostics.hw_accel_backend, HwAccelBackend::None);
    assert!(!frame.diagnostics.hardware_decode_active);
    assert!(!frame.diagnostics.zero_copy_active);
    assert_eq!(
        frame.diagnostics.decoded_frame_residency,
        DecodedFrameResidency::CpuRgba
    );
    assert_eq!(frame.diagnostics.gpu_frame_handle_kind, None);
    assert_eq!(
        frame.diagnostics.hardware_decode_blocker,
        PreviewHardwareDecodeBlocker::TextureResidencyNotConnected
    );
}

#[test]
fn decoded_surface_format_maps_native_yuv_candidates() {
    assert_eq!(
        decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::NV12),
        DecodedVideoSurfaceFormat::Nv12
    );
    assert_eq!(
        decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::P010LE),
        DecodedVideoSurfaceFormat::P010
    );
    assert_eq!(
        decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::YUV420P10LE),
        DecodedVideoSurfaceFormat::Yuv420p10le
    );
    assert_eq!(
        decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::RGBA),
        DecodedVideoSurfaceFormat::Rgba8
    );
}

#[test]
fn decoded_video_sampling_reads_frame_range_chroma_and_bit_depth() {
    let mut frame =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::P010LE, 16, 16);
    frame.set_color_range(ffmpeg::util::color::Range::MPEG);
    frame.set_color_space(ffmpeg::util::color::Space::SMPTE170M);
    unsafe {
        (*frame.as_mut_ptr()).chroma_location = ffmpeg::util::chroma::Location::Left.into();
    }

    assert_eq!(
        decoded_video_sampling_from_frame(&frame),
        DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Smpte170M,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::Left,
            bit_depth: 10,
        }
    );
}

#[test]
fn decoded_video_sampling_distinguishes_missing_and_unsupported_matrices() {
    let mut frame =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::P010LE, 16, 16);
    assert_eq!(
        decoded_video_sampling_from_frame(&frame).matrix,
        DecodedVideoMatrix::Unknown
    );

    frame.set_color_space(ffmpeg::util::color::Space::BT2020CL);
    assert_eq!(
        decoded_video_sampling_from_frame(&frame).matrix,
        DecodedVideoMatrix::Unsupported
    );
}

#[test]
fn cpu_rgba_contract_uses_explicit_bt2020_matrix_and_limited_range() {
    let mut frame = ffmpeg::util::frame::video::Video::new(
        ffmpeg::util::format::pixel::Pixel::YUV420P10LE,
        16,
        16,
    );
    frame.set_color_space(ffmpeg::util::color::Space::BT2020NCL);
    frame.set_color_range(ffmpeg::util::color::Range::MPEG);

    let contract = resolve_cpu_rgba_contract(
        &frame,
        PreviewSourceColorContract::automatic(ColorSpace::Rec2100Pq, DecodedVideoRange::Limited),
        Path::new("bt2020-pq.mov"),
    )
    .expect("BT.2020 NCL must resolve exactly");

    assert_eq!(
        contract.applied_matrix,
        DecodedVideoMatrix::Bt2020NonConstant
    );
    assert_eq!(contract.applied_range, DecodedVideoRange::Limited);
    assert_eq!(contract.source.color_space(), Some(ColorSpace::Rec2100Pq));
}

#[test]
fn data_texture_cpu_contract_accepts_rgb_and_rejects_yuv_before_swscale() {
    let data = PreviewSourceColorContract::data_texture(DecodedVideoRangeContract::OverrideFull);
    let rgb =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGB24, 16, 16);
    let contract = resolve_cpu_rgba_contract(&rgb, data, Path::new("technical-rgb.exr"))
        .expect("RGB technical channels require no color matrix");
    assert_eq!(contract.encoding, DecodedRgbaEncoding::DataTexture);
    assert_eq!(contract.applied_matrix, DecodedVideoMatrix::Rgb);

    let mut yuv =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::YUV420P, 16, 16);
    yuv.set_color_space(ffmpeg::util::color::Space::BT709);
    yuv.set_color_range(ffmpeg::util::color::Range::MPEG);
    let error = resolve_cpu_rgba_contract(&yuv, data, Path::new("technical-yuv.mov"))
        .expect_err("a YCbCr matrix would change technical channels");
    assert!(error.to_string().contains("data-texture materialization requires RGB/GBR"));
}

#[test]
fn cpu_rgba_contract_rejects_yuv_without_explicit_matrix_evidence() {
    let frame =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::YUV420P, 16, 16);

    let error = resolve_cpu_rgba_contract(
        &frame,
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
        Path::new("untagged-rec709.mov"),
    )
    .expect_err("RGB color identity must not synthesize a missing YUV matrix");
    assert!(error.to_string().contains("YUV matrix is unspecified"));

    let policy_contract = resolve_cpu_rgba_contract(
        &frame,
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
            .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
        Path::new("policy-untagged-rec709.mov"),
    )
    .expect("an explicit source-policy fallback must authorize conversion");
    assert_eq!(policy_contract.applied_matrix, DecodedVideoMatrix::Bt709);

    let mut tagged_yuv = frame;
    tagged_yuv.set_color_space(ffmpeg::util::color::Space::BT709);
    let srgb_contract = resolve_cpu_rgba_contract(
        &tagged_yuv,
        PreviewSourceColorContract::automatic(ColorSpace::Srgb, DecodedVideoRange::Limited),
        Path::new("srgb-transfer-yuv.mov"),
    )
    .expect("decoded YUV matrix remains authoritative for an RGB-defined source space");
    assert_eq!(srgb_contract.applied_matrix, DecodedVideoMatrix::Bt709);

    let mut rgb_matrix_yuv = tagged_yuv;
    rgb_matrix_yuv.set_color_space(ffmpeg::util::color::Space::RGB);
    let error = resolve_cpu_rgba_contract(
        &rgb_matrix_yuv,
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
        Path::new("yuv-with-rgb-matrix.mov"),
    )
    .expect_err("YUV sampling cannot execute with RGB/GBR matrix metadata");
    assert!(error.to_string().contains("cannot use RGB/GBR matrix"));
}

#[test]
fn cpu_rgba_contract_honors_resolved_source_range_over_frame_tag() {
    let mut frame =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::YUV420P, 16, 16);
    frame.set_color_space(ffmpeg::util::color::Space::BT709);
    frame.set_color_range(ffmpeg::util::color::Range::MPEG);

    let contract = resolve_cpu_rgba_contract(
        &frame,
        PreviewSourceColorContract::from_interpretation(
            ColorSpace::Rec709,
            mondrian_core::timeline_data::MediaRangeInterpretation::Override {
                range: mondrian_core::timeline_data::MediaSignalRange::Full,
            },
            DecodedVideoRange::Limited,
        ),
        Path::new("incorrect-limited-tag.mov"),
    )
    .expect("resolved app contract must override an incorrect frame range tag");

    assert_eq!(contract.applied_range, DecodedVideoRange::Full);
}

#[test]
fn cpu_rgba_contract_auto_prefers_frame_range_over_probe_fallback() {
    let mut frame =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::YUV420P, 16, 16);
    frame.set_color_space(ffmpeg::util::color::Space::BT709);
    frame.set_color_range(ffmpeg::util::color::Range::JPEG);

    let contract = resolve_cpu_rgba_contract(
        &frame,
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
        Path::new("frame-full-probe-limited.mov"),
    )
    .expect("automatic range must prefer the decoded frame fact");

    assert_eq!(contract.applied_range, DecodedVideoRange::Full);
}

#[test]
fn cpu_rgba_contract_keeps_explicit_matrix_and_rejects_constant_luminance() {
    let mut conflict =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::YUV420P, 16, 16);
    conflict.set_color_space(ffmpeg::util::color::Space::BT709);
    conflict.set_color_range(ffmpeg::util::color::Range::MPEG);
    let contract = resolve_cpu_rgba_contract(
        &conflict,
        PreviewSourceColorContract::automatic(ColorSpace::Rec2020, DecodedVideoRange::Limited),
        Path::new("conflict.mov"),
    )
    .expect("explicit decoded matrix remains authoritative");
    assert_eq!(contract.applied_matrix, DecodedVideoMatrix::Bt709);

    conflict.set_color_space(ffmpeg::util::color::Space::BT2020CL);
    let error = resolve_cpu_rgba_contract(
        &conflict,
        PreviewSourceColorContract::automatic(ColorSpace::Rec2020, DecodedVideoRange::Limited),
        Path::new("bt2020-cl.mov"),
    )
    .expect_err("constant-luminance BT.2020 needs a dedicated conversion");
    assert!(error.to_string().contains("unsupported FFmpeg YUV matrix"));
}

#[test]
fn swscale_expands_bt709_limited_range_to_full_rgba() {
    fn decoded_yuv420(y: u8) -> ffmpeg::util::frame::video::Video {
        let mut frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            4,
            4,
        );
        frame.set_color_space(ffmpeg::util::color::Space::BT709);
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);
        frame.data_mut(0).fill(y);
        frame.data_mut(1).fill(128);
        frame.data_mut(2).fill(128);
        frame
    }

    let path = Path::new("limited-rec709.mov");
    let mut scaler = preview_create_rgba_scaler(
        ffmpeg::util::format::pixel::Pixel::YUV420P,
        ffmpeg::util::format::pixel::Pixel::RGBA,
        4,
        4,
        4,
        4,
        path,
    )
    .expect("create scaler");
    let black = convert_decoded_to_rgba(
        &decoded_yuv420(16),
        &mut scaler,
        path,
        test_source_color(),
        &mut None,
    )
    .expect("convert limited black");
    let white = convert_decoded_to_rgba(
        &decoded_yuv420(235),
        &mut scaler,
        path,
        test_source_color(),
        &mut None,
    )
    .expect("convert limited white");

    assert!(black.rgba()[..3].iter().all(|channel| *channel <= 2));
    assert!(white.rgba()[..3].iter().all(|channel| *channel >= 253));
    assert_eq!(black.rgba()[3], 255);
    assert_eq!(white.rgba()[3], 255);
}

#[test]
fn percentile_upper_bound_uses_sorted_nearest_rank() {
    assert_eq!(percentile_upper_bound_us([30, 10, 20].into_iter(), 95), 30);
    assert_eq!(percentile_upper_bound_us([30, 10, 20].into_iter(), 50), 20);
    assert_eq!(percentile_upper_bound_us(std::iter::empty(), 95), 0);
}

#[test]
fn preview_decode_stage_durations_accumulate_saturating() {
    let mut durations = PreviewDecodeStageDurations {
        session_open_us: u64::MAX,
        output_lease_wait_us: 1,
        cache_lookup_us: 2,
        seek_us: 3,
        packet_decode_us: 4,
        hardware_transfer_us: 5,
        swscale_us: 6,
        rgba_copy_us: 7,
        external_process_us: 8,
    };

    durations.accumulate(PreviewDecodeStageDurations {
        session_open_us: 1,
        output_lease_wait_us: 10,
        cache_lookup_us: 20,
        seek_us: 30,
        packet_decode_us: 40,
        hardware_transfer_us: 50,
        swscale_us: 60,
        rgba_copy_us: 70,
        external_process_us: 80,
    });

    assert_eq!(durations.session_open_us, u64::MAX);
    assert_eq!(durations.output_lease_wait_us, 11);
    assert_eq!(durations.cache_lookup_us, 22);
    assert_eq!(durations.seek_us, 33);
    assert_eq!(durations.packet_decode_us, 44);
    assert_eq!(durations.hardware_transfer_us, 55);
    assert_eq!(durations.swscale_us, 66);
    assert_eq!(durations.rgba_copy_us, 77);
    assert_eq!(durations.external_process_us, 88);
}

#[test]
fn rgba_frame_diagnostics_record_cpu_residency_and_cache_hits() {
    let frame = RgbaFrame::new(
        2,
        1,
        vec![0; 8],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    )
    .with_elapsed(std::time::Duration::from_micros(42))
    .with_temporal_selection(24_000, Some(DecodedTemporalExtent::point(18_000)));

    assert_eq!(
        frame.diagnostics.path,
        PreviewDecodePath::InProcessFfmpegCpuRgba
    );
    assert_eq!(frame.diagnostics.elapsed_us, 42);
    assert!(!frame.diagnostics.cache_hit);
    assert!(!frame.diagnostics.external_process);
    assert!(frame.diagnostics.cpu_resident);
    assert_eq!(
        frame.diagnostics.session_disposition,
        PreviewDecodeSessionDisposition::Unspecified
    );
    assert!(!frame.diagnostics.forward_reused);
    assert!(!frame.diagnostics.seek_index_available);
    assert_eq!(frame.diagnostics.seek_index_keyframes, 0);
    assert_eq!(frame.diagnostics.seek_index_observed_packets, 0);
    assert!(!frame.diagnostics.seek_index_used);
    assert_eq!(frame.diagnostics.seek_index_anchor_pts, None);
    assert_eq!(
        frame.diagnostics.threading_kind,
        PreviewDecodeThreadingKind::None
    );
    assert_eq!(frame.diagnostics.threading_count, 0);
    assert_eq!(
        frame.diagnostics.access_mode,
        PreviewDecodeAccessMode::RandomAccessStillFrame
    );
    assert_eq!(
        frame.diagnostics.seek_strategy,
        PreviewDecodeSeekStrategy::KeyframeBefore
    );
    assert_eq!(frame.diagnostics.forward_reuse_frame_window, 0);
    assert_eq!(frame.diagnostics.forward_decode_budget_frames, 0);
    assert_eq!(frame.diagnostics.any_seek_window_ms, 0);
    let reused = frame
        .clone()
        .with_session_disposition(PreviewDecodeSessionDisposition::Reused)
        .with_forward_reused(true);
    assert_eq!(
        reused.diagnostics.session_disposition,
        PreviewDecodeSessionDisposition::Reused
    );
    assert!(reused.diagnostics.forward_reused);

    let ring_hit = frame.into_playback_ring_hit(std::time::Duration::from_micros(2));
    assert_eq!(
        ring_hit.diagnostics.path,
        PreviewDecodePath::PlaybackSessionRingHit
    );
    assert_eq!(ring_hit.diagnostics.elapsed_us, 2);
    assert!(ring_hit.diagnostics.cache_hit);
    assert!(ring_hit.diagnostics.cpu_resident);
    assert_eq!(
        ring_hit.diagnostics.session_disposition,
        PreviewDecodeSessionDisposition::BypassedCache
    );
    assert_eq!(
        ring_hit.diagnostics.access_mode,
        PreviewDecodeAccessMode::PlaybackCursor
    );
    assert_eq!(
        ring_hit.diagnostics.forward_reuse_frame_window,
        PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES
    );
    assert_eq!(
        ring_hit.diagnostics.forward_decode_budget_frames,
        PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES as u32
    );
    assert_eq!(ring_hit.diagnostics.any_seek_window_ms, 0);
    assert_eq!(ring_hit.diagnostics.requested_pts, Some(24_000));
    assert_eq!(ring_hit.diagnostics.selected_pts, Some(18_000));
    assert!(ring_hit.diagnostics.temporal_approximation);

    let rebound = PreviewDecodedFramePayload::CpuRgba(ring_hit)
        .with_temporal_selection(18_000, Some(DecodedTemporalExtent::point(18_000)));
    let PreviewDecodedFramePayload::CpuRgba(rebound) = rebound else {
        panic!("RGBA cache payload changed kind");
    };
    assert_eq!(rebound.diagnostics.requested_pts, Some(18_000));
    assert_eq!(rebound.diagnostics.selected_pts, Some(18_000));
    assert!(!rebound.diagnostics.temporal_approximation);
}

#[test]
fn hardware_execution_provenance_survives_playback_ring_reuse() {
    let mut frame = RgbaFrame::new(
        2,
        1,
        vec![0; 8],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );
    frame.diagnostics.hardware_decode_active = true;
    frame.diagnostics.hardware_decode_cpu_transfer_observed = true;
    frame.diagnostics.hw_accel_backend = HwAccelBackend::D3D11VA;
    frame.diagnostics.decoded_surface_format = DecodedVideoSurfaceFormat::P010;
    frame.diagnostics.decoded_video_sampling.bit_depth = 10;
    let frame = frame.with_decode_execution();
    let expected = PreviewDecodeExecutionPath::HardwareCpuTransfer {
        backend: HwAccelBackend::D3D11VA,
        surface: DecodedVideoSurfaceFormat::P010,
        sampling: frame.diagnostics.decoded_video_sampling,
    };
    assert_eq!(frame.decode_execution, expected);

    let ring_hit = frame.into_playback_ring_hit(std::time::Duration::from_micros(2));
    assert_eq!(ring_hit.decode_execution, expected);
    assert!(!ring_hit.diagnostics.hardware_decode_cpu_transfer_observed);
}

#[test]
fn playback_session_ring_uses_exact_temporal_extents_and_lru_capacity() {
    let mut ring = PreviewPlaybackRing::new(2, 8);
    let frame_a = RgbaFrame::new(
        1,
        1,
        vec![1, 2, 3, 4],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );
    let frame_b = RgbaFrame::new(
        1,
        1,
        vec![5, 6, 7, 8],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );
    let frame_c = RgbaFrame::new(
        1,
        1,
        vec![9, 10, 11, 12],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );

    assert!(ring.put(
        DecodedTemporalExtent::from_duration(100, 4),
        frame_a.clone()
    ));
    assert!(ring.put(
        DecodedTemporalExtent::from_duration(110, 10),
        frame_b.clone()
    ));

    let (hit_extent, PreviewDecodedFramePayload::CpuRgba(hit)) =
        ring.get(103).expect("inside the cached presentation interval")
    else {
        panic!("RGBA ring entry changed payload kind");
    };
    assert_eq!(hit_extent, DecodedTemporalExtent::from_duration(100, 4));
    assert_eq!(hit.rgba(), frame_a.rgba());
    assert!(ring.get(104).is_none());

    assert!(ring.put(
        DecodedTemporalExtent::from_duration(120, 10),
        frame_c.clone()
    ));

    assert!(ring.get(110).is_none());
    let (_, PreviewDecodedFramePayload::CpuRgba(recent)) = ring.get(100).expect("recently used")
    else {
        panic!("recent RGBA ring entry changed payload kind");
    };
    assert_eq!(recent.rgba(), frame_a.rgba());
    let (_, PreviewDecodedFramePayload::CpuRgba(newest)) = ring.get(120).expect("newest") else {
        panic!("newest RGBA ring entry changed payload kind");
    };
    assert_eq!(newest.rgba(), frame_c.rgba());
}

#[test]
fn playback_session_ring_rejects_oversize_frames_and_evicts_by_bytes() {
    let mut ring = PreviewPlaybackRing::new(8, 12);
    let frame = |value, bytes| {
        RgbaFrame::new(
            (bytes / 4) as u32,
            1,
            vec![value; bytes],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        )
    };

    assert!(ring.put(DecodedTemporalExtent::from_duration(10, 1), frame(1, 8)));
    assert!(ring.put(DecodedTemporalExtent::from_duration(20, 1), frame(2, 8)));
    assert_eq!(ring.reserved_bytes(), 8);
    assert!(ring.get(10).is_none());
    assert!(ring.get(20).is_some());

    assert!(!ring.put(DecodedTemporalExtent::from_duration(30, 1), frame(3, 16)));
    assert_eq!(ring.reserved_bytes(), 8);
    assert!(ring.get(30).is_none());
}

#[test]
fn rgba_frame_clone_shares_pixel_payload() {
    let frame = RgbaFrame::new(
        2,
        1,
        vec![0, 64, 128, 255, 255, 128, 64, 32],
        test_rgba_contract(),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );
    let cloned = frame.clone();

    assert!(std::sync::Arc::ptr_eq(&frame.data, &cloned.data));
    assert_eq!(cloned.rgba(), frame.rgba());
    assert_eq!(frame.into_data(), vec![0, 64, 128, 255, 255, 128, 64, 32]);
}

#[test]
fn native_decoded_frame_payload_forces_gpu_residency_diagnostics() {
    let handle = test_native_handle(DecodedGpuFrameHandleKind::D3D11Texture2D, 7);
    let frame = PreviewNativeDecodedFrame::new(
        1920,
        1080,
        handle.clone(),
        DecodedVideoSurfaceFormat::P010,
        p010_native_sampling(),
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
    )
    .expect("valid native frame");

    assert_eq!(frame.width, 1920);
    assert_eq!(frame.height, 1080);
    assert_eq!(frame.handle, handle);
    assert_eq!(
        frame.handle_kind(),
        DecodedGpuFrameHandleKind::D3D11Texture2D
    );
    assert_eq!(frame.surface_format, DecodedVideoSurfaceFormat::P010);
    assert!(!frame.diagnostics.cpu_resident);
    assert_eq!(
        frame.diagnostics.decoded_frame_residency,
        DecodedFrameResidency::GpuTexture
    );
    assert_eq!(
        frame.diagnostics.gpu_frame_handle_kind,
        Some(DecodedGpuFrameHandleKind::D3D11Texture2D)
    );
    assert_eq!(
        frame.diagnostics.decoded_surface_format,
        DecodedVideoSurfaceFormat::P010
    );
    assert_eq!(
        frame.diagnostics.decoded_video_sampling,
        p010_native_sampling()
    );
}

#[test]
fn native_decoded_frame_handle_retains_resource_until_last_clone_drops() {
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
        kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
        id: NonZeroU64::new(17).expect("non-zero test id"),
        drops: Some(Arc::clone(&drops)),
    });
    let cloned = handle.clone();
    let separate_same_id = PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
        kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
        id: NonZeroU64::new(17).expect("non-zero test id"),
        drops: None,
    });

    assert_eq!(handle.kind(), DecodedGpuFrameHandleKind::D3D11Texture2D);
    assert_eq!(handle.id().get(), 17);
    assert_eq!(handle, cloned);
    assert_ne!(handle, separate_same_id);
    assert!(handle.resource::<TestNativeDecodedFrameResource>().is_some());

    drop(handle);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(cloned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn ffmpeg_native_resource_retains_d3d11_surface_buffer_and_abi_view() {
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D11);
    frame.set_width(1920);
    frame.set_height(1080);
    let texture = std::ptr::NonNull::<u8>::dangling().as_ptr();
    // SAFETY: The test frame owns the AVBufferRef assigned to buf[0]. The
    // synthetic data pointers are never dereferenced; they only exercise
    // FFmpeg's documented D3D11 texture-plus-slice metadata ABI.
    let source_buffer = unsafe {
        let raw = frame.as_mut_ptr();
        let buffer = ffmpeg::ffi::av_buffer_alloc(1);
        assert!(
            !buffer.is_null(),
            "test AVBufferRef allocation must succeed"
        );
        (*raw).buf[0] = buffer;
        (*raw).data[0] = texture;
        (*raw).data[1] = 3usize as *mut u8;
        buffer
    };
    // SAFETY: source_buffer remains owned by frame.
    assert_eq!(
        unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
        1
    );

    let resource = FfmpegNativeDecodedFrameResource::retain(&frame)
        .expect("D3D11 frame with a ref-counted surface must be retained");
    // SAFETY: source_buffer remains owned by frame and the retained resource.
    assert_eq!(
        unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
        2
    );
    let handle = PreviewNativeDecodedFrameHandle::new(resource);
    drop(frame);

    let retained = handle
        .resource::<FfmpegNativeDecodedFrameResource>()
        .expect("native handle must preserve its concrete FFmpeg resource");
    // SAFETY: retained owns the cloned AVFrame and its buf[0] reference.
    let retained_buffer = unsafe { (*retained.retained_frame_ptr()).buf[0] };
    assert!(!retained_buffer.is_null());
    // SAFETY: retained_buffer remains owned by retained.
    assert_eq!(
        unsafe { ffmpeg::ffi::av_buffer_get_ref_count(retained_buffer) },
        1
    );
    assert_eq!(
        retained.pixel_format(),
        ffmpeg::util::format::pixel::Pixel::D3D11
    );
    let view = retained
        .d3d11_texture()
        .expect("preferred D3D11 frame must expose its texture ABI view");
    assert_eq!(view.texture_ptr(), texture.cast());
    assert_eq!(view.array_slice(), 3);
}

#[cfg(mondrian_ffmpeg_7_1)]
#[test]
fn ffmpeg_native_resource_retains_d3d12_resource_and_fence_abi() {
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D12);
    frame.set_width(3840);
    frame.set_height(2160);
    let texture = std::ptr::NonNull::<u8>::dangling().as_ptr().cast::<c_void>();
    let fence = std::ptr::NonNull::<u16>::dangling().as_ptr().cast::<c_void>();
    let native = Box::new(FfmpegAvD3D12VaFrame {
        texture,
        sync_ctx: FfmpegAvD3D12VaSyncContext {
            fence,
            event: std::ptr::null_mut(),
            fence_value: 42,
        },
    });
    // SAFETY: the synthetic native descriptor remains alive until after
    // every parsed view is consumed. The AVBufferRef only exercises the
    // retained AVFrame ownership path and no COM pointer is dereferenced.
    let source_buffer = unsafe {
        let raw = frame.as_mut_ptr();
        let buffer = ffmpeg::ffi::av_buffer_alloc(1);
        assert!(
            !buffer.is_null(),
            "test AVBufferRef allocation must succeed"
        );
        (*raw).buf[0] = buffer;
        (*raw).data[0] = (&*native as *const FfmpegAvD3D12VaFrame).cast_mut().cast();
        buffer
    };
    // SAFETY: source_buffer remains owned by frame.
    assert_eq!(
        unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
        1
    );

    let resource = FfmpegNativeDecodedFrameResource::retain(&frame)
        .expect("D3D12 frame with a ref-counted resource must be retained");
    // SAFETY: source_buffer remains owned by frame and resource.
    assert_eq!(
        unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
        2
    );
    let handle = PreviewNativeDecodedFrameHandle::new(resource);
    drop(frame);

    let retained = handle
        .resource::<FfmpegNativeDecodedFrameResource>()
        .expect("native handle must preserve the concrete FFmpeg resource");
    let view = retained.d3d12_texture().expect("D3D12 ABI view");
    assert_eq!(view.texture_ptr(), texture);
    assert_eq!(view.fence_ptr(), fence);
    assert_eq!(view.fence_value(), 42);
    drop(native);
}

#[test]
fn ffmpeg_native_resource_rejects_software_frames() {
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    frame.set_format(ffmpeg::util::format::pixel::Pixel::RGBA);

    let error = FfmpegNativeDecodedFrameResource::retain(&frame)
        .expect_err("software RGBA must not masquerade as a native decoder surface");
    assert_eq!(
        error,
        FfmpegNativeDecodedFrameResourceError::UnsupportedPixelFormat {
            pixel_format: ffmpeg::util::format::pixel::Pixel::RGBA,
        }
    );
}

#[test]
fn ffmpeg_legacy_d3d11va_frame_does_not_use_preferred_texture_abi() {
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D11VA_VLD);
    // SAFETY: The frame allocation is alive for this parse call.
    let raw = unsafe {
        std::ptr::NonNull::new(frame.as_mut_ptr()).expect("AVFrame allocation must succeed")
    };
    let error =
        super::parse_ffmpeg_d3d11_texture(raw, ffmpeg::util::format::pixel::Pixel::D3D11VA_VLD)
            .expect_err("legacy D3D11VA layout must fail the preferred D3D11 ABI contract");
    assert_eq!(
        error,
        FfmpegNativeDecodedFrameResourceError::NotPreferredD3D11Frame {
            pixel_format: ffmpeg::util::format::pixel::Pixel::D3D11VA_VLD,
        }
    );
}

#[test]
fn native_surface_format_requires_explicit_nv12_or_p010_layout() {
    assert_eq!(
        decoded_native_surface_format_from_software_format(
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12,
        )
        .expect("NV12 hardware layout must be supported"),
        DecodedVideoSurfaceFormat::Nv12
    );
    assert_eq!(
        decoded_native_surface_format_from_software_format(
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE,
        )
        .expect("P010 hardware layout must be supported"),
        DecodedVideoSurfaceFormat::P010
    );
    assert!(matches!(
        decoded_native_surface_format_from_software_format(
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
        ),
        Err(
            super::PreviewNativeFrameMaterializationError::UnsupportedHardwareSurfaceFormat {
                software_format: ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
            }
        )
    ));
}

#[test]
fn gpu_preferred_software_frame_falls_back_with_structured_reason() {
    let decoded =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA, 2, 2);
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::H264,
        None,
    );
    plan.request = PreviewHardwareDecodeRequest::PreferGpuResident;
    let mut scaler = None;
    let mut scaler_source_format = None;
    let mut scaler_color_contract = None;
    let payload = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeCpu,
        &mut plan,
        &mut scaler,
        &mut scaler_source_format,
        &mut scaler_color_contract,
        2,
        2,
        Path::new("synthetic-rgba"),
        test_source_color(),
    )
    .expect("GPU preference may fall back to CPU RGBA with diagnostics");

    assert!(matches!(payload, PreviewDecodedFramePayload::CpuRgba(_)));
    assert_eq!(
        plan.native_decode_fallback,
        Some(PreviewNativeDecodeFallback::SoftwareFrame)
    );
}

#[test]
fn high_bit_depth_cpu_materialization_preserves_encoded_float_precision() {
    let mut decoded =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA64LE, 2, 1);
    decoded.set_color_space(ffmpeg::util::color::Space::RGB);
    decoded.set_color_range(ffmpeg::util::color::Range::JPEG);
    let samples = [1_u16, 2, 3, u16::MAX, 257, 513, 1025, u16::MAX];
    for (target, sample) in decoded.data_mut(0).chunks_exact_mut(2).zip(samples) {
        target.copy_from_slice(&sample.to_le_bytes());
    }

    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::H264,
        None,
    );
    let payload = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeCpu,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        2,
        1,
        Path::new("synthetic-rgba64"),
        test_source_color(),
    )
    .expect("high-bit software frame materializes as encoded float");

    let PreviewDecodedFramePayload::CpuFloat(frame) = payload else {
        panic!("high-bit software frames must not quantize through RGBA8");
    };
    assert_eq!(
        frame.color_contract.encoding,
        DecodedRgbaEncoding::SourceEncodedRgb
    );
    assert_eq!(frame.diagnostics.decoded_video_sampling.bit_depth, 16);
    assert!((frame.rgba()[0] - 1.0 / 65_535.0).abs() < 1.0e-7);
    assert!((frame.rgba()[4] - 257.0 / 65_535.0).abs() < 1.0e-7);
    assert_eq!(frame.rgba()[3], 1.0);
    assert_eq!(frame.rgba()[7], 1.0);
}

#[test]
fn compact_cpu_yuv_representation_is_exact_and_fail_closed() {
    let mut decoded = ffmpeg::util::frame::video::Video::new(
        ffmpeg::util::format::pixel::Pixel::YUV422P10LE,
        4,
        2,
    );
    decoded.set_color_space(ffmpeg::util::color::Space::BT709);
    decoded.set_color_range(ffmpeg::util::color::Range::MPEG);
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::H264,
        None,
    );
    let payload = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::CompactCpuYuv,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        4,
        2,
        Path::new("synthetic-yuv422p10le"),
        test_source_color(),
    )
    .expect("exact YUV422P10LE representation must remain compact");
    let PreviewDecodedFramePayload::CpuYuv(frame) = payload else {
        panic!("compact representation must not expand into an RGBA payload");
    };
    assert_eq!(frame.chroma_plane_layout(), CpuYuvChromaPlaneLayout::Planar);
    let luma = frame.luma_plane();
    assert!(luma.bytes_per_row() >= 4 * 2);
    assert!(luma.data().len() >= luma.bytes_per_row() as usize * 2);
    let CpuYuvChromaPlanes::Planar { cb, cr } = frame.chroma_planes() else {
        panic!("FFmpeg YUV422P10LE must retain separate Cb and Cr planes");
    };
    assert!(cb.bytes_per_row() >= 2 * 2);
    assert_eq!(cb.bytes_per_row(), cr.bytes_per_row());
    assert!(frame.retained_bytes() >= 4 * 2 * 4);
    assert_eq!(frame.chroma_width, 2);
    assert_eq!(frame.chroma_height, 2);
    assert_eq!(
        frame.diagnostics.path,
        PreviewDecodePath::InProcessFfmpegCpuYuv
    );

    let scaled = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::CompactCpuYuv,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        2,
        1,
        Path::new("synthetic-scaled-yuv422p10le"),
        test_source_color(),
    )
    .expect("adaptive compact representation must materialize its requested extent");
    let PreviewDecodedFramePayload::CpuYuv(scaled) = scaled else {
        panic!("scaled compact representation must remain a YUV payload");
    };
    assert_eq!((scaled.width, scaled.height), (2, 1));
    assert_eq!((scaled.chroma_width, scaled.chroma_height), (1, 1));
    assert_eq!(
        scaled.chroma_plane_layout(),
        CpuYuvChromaPlaneLayout::Planar
    );
    assert!(scaled.retained_bytes() >= 2 * 4);
    assert_eq!(scaled.diagnostics.stage_durations.rgba_copy_us, 0);

    let mismatched =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA, 4, 2);
    let error = materialize_decoded_frame(
        &mismatched,
        PreviewDecodeRepresentation::CompactCpuYuv,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        4,
        2,
        Path::new("synthetic-yuv-mismatch"),
        test_source_color(),
    )
    .expect_err("compact representation must reject a decoder layout mismatch");
    assert!(error.to_string().contains("expected YUV422P10LE"));
}

#[test]
fn interlaced_decoded_frame_fails_before_native_or_cpu_materialization() {
    let mut decoded =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA, 2, 2);
    // SAFETY: the test owns this AVFrame exclusively and only sets FFmpeg's public scan flag.
    unsafe {
        (*decoded.as_mut_ptr()).flags |= ffmpeg::ffi::AV_FRAME_FLAG_INTERLACED;
        (*decoded.as_mut_ptr()).interlaced_frame = 1;
    }
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::H264,
        None,
    );
    let error = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeCpu,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        2,
        2,
        Path::new("synthetic-interlaced"),
        test_source_color(),
    )
    .expect_err("interlaced pixels must not bypass progressive-only admission");

    assert!(error.to_string().contains("interlaced"));
}

#[test]
fn scene_linear_ffmpeg_float_frame_preserves_extended_range_rgba() {
    let pixel_format = if cfg!(target_endian = "little") {
        ffmpeg::util::format::pixel::Pixel::GBRAPF32LE
    } else {
        ffmpeg::util::format::pixel::Pixel::GBRAPF32BE
    };
    let mut decoded = ffmpeg::util::frame::video::Video::new(pixel_format, 2, 1);
    let pixels = [[-0.25_f32, 0.18, 4.0, 0.5], [0.3, 0.4, 0.5, 1.0]];
    for (plane, channel) in [(0, 1), (1, 2), (2, 0), (3, 3)] {
        let stride = decoded.stride(plane);
        let data = decoded.data_mut(plane);
        for (x, pixel) in pixels.iter().enumerate() {
            let start = x * std::mem::size_of::<f32>();
            data[start..start + std::mem::size_of::<f32>()]
                .copy_from_slice(&pixel[channel].to_ne_bytes());
        }
        assert!(stride >= 2 * std::mem::size_of::<f32>());
    }
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::EXR,
        None,
    );
    let payload = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeCpu,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        2,
        1,
        Path::new("synthetic-linear.exr"),
        test_linear_source_color(),
    )
    .expect("scene-linear float frame should materialize without RGBA8 quantization");

    let PreviewDecodedFramePayload::CpuFloat(frame) = payload else {
        panic!("scene-linear FFmpeg float output must remain CPU float");
    };
    assert_eq!(frame.rgba(), pixels.as_flattened());
    assert_eq!(
        frame.diagnostics.decoded_frame_residency,
        DecodedFrameResidency::CpuFloat
    );
}

#[test]
fn float_preview_resize_preserves_extended_range() {
    let source = vec![
        -1.0, 0.0, 1.0, 1.0, 1.0, 2.0, 3.0, 1.0, 3.0, 4.0, 5.0, 1.0, 5.0, 6.0, 7.0, 1.0,
    ];
    let resized = super::resize_float_rgba(&source, 2, 2, 1, 1);
    assert_eq!(resized, vec![2.0, 3.0, 4.0, 1.0]);
}

#[test]
fn gpu_required_software_frame_fails_closed() {
    let decoded =
        ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA, 2, 2);
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::H264,
        None,
    );
    plan.request = PreviewHardwareDecodeRequest::RequireGpuResident;
    let error = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeSurface,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        2,
        2,
        Path::new("synthetic-rgba"),
        test_source_color(),
    )
    .expect_err("required GPU residency must not return a software frame");

    assert!(error.to_string().contains("required GPU-resident decode returned software"));
}

#[test]
fn explicit_d3d11_nv12_frame_materializes_native_without_cpu_payload() {
    let decoded = synthetic_d3d11_frame(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12);
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::H264,
        None,
    );
    plan.request = PreviewHardwareDecodeRequest::PreferGpuResident;
    let payload = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeSurface,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        960,
        540,
        Path::new("synthetic-d3d11"),
        test_source_color(),
    )
    .expect("explicit D3D11 NV12 frame must materialize as a native payload");

    let frame = match &payload {
        PreviewDecodedFramePayload::NativeGpu(frame) => frame,
        PreviewDecodedFramePayload::CpuRgba(_)
        | PreviewDecodedFramePayload::CpuFloat(_)
        | PreviewDecodedFramePayload::CpuYuv(_) => {
            panic!("explicit D3D11 NV12 frame must not transfer to CPU")
        }
    };
    assert_eq!(frame.width, 1920);
    assert_eq!(frame.height, 1080);
    assert_eq!(frame.surface_format, DecodedVideoSurfaceFormat::Nv12);
    assert_eq!(
        frame.diagnostics.path,
        PreviewDecodePath::InProcessFfmpegNative
    );
    assert_eq!(
        plan.decision,
        PreviewHardwareDecodeDecision::GpuResidentNative
    );
    assert_eq!(plan.native_decode_fallback, None);
}

#[cfg(mondrian_ffmpeg_7_1)]
#[test]
fn explicit_d3d12_p010_frame_materializes_native_with_decode_fence() {
    let decoded = synthetic_d3d12_frame(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE);
    let mut plan = PreviewHardwareDecodePlan::resolve(
        PreviewHardwareDecodeRequest::Auto,
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeBackend::Software,
        ffmpeg::codec::Id::HEVC,
        None,
    );
    plan.request = PreviewHardwareDecodeRequest::RequireGpuResident;
    let payload = materialize_decoded_frame(
        &decoded,
        PreviewDecodeRepresentation::NativeSurface,
        &mut plan,
        &mut None,
        &mut None,
        &mut None,
        1920,
        1080,
        Path::new("synthetic-d3d12"),
        PreviewSourceColorContract::automatic(ColorSpace::Rec2100Pq, DecodedVideoRange::Limited),
    )
    .expect("explicit D3D12 P010 frame must remain native");

    let PreviewDecodedFramePayload::NativeGpu(frame) = payload else {
        panic!("explicit D3D12 P010 frame must not transfer to CPU");
    };
    assert_eq!(frame.width, 3840);
    assert_eq!(frame.height, 2160);
    assert_eq!(frame.surface_format, DecodedVideoSurfaceFormat::P010);
    assert_eq!(
        frame.handle_kind(),
        DecodedGpuFrameHandleKind::D3D12Resource
    );
    let resource = frame
        .handle
        .resource::<FfmpegNativeDecodedFrameResource>()
        .expect("native frame must retain FFmpeg's D3D12 resource");
    let view = resource.d3d12_texture().expect("D3D12 ABI view");
    assert_eq!(view.fence_value(), 9);
    assert_eq!(
        plan.decision,
        PreviewHardwareDecodeDecision::GpuResidentNative
    );
    assert_eq!(plan.native_decode_fallback, None);
}

#[test]
fn native_decoded_frame_payload_requires_real_handle_and_native_surface() {
    let handle = test_native_handle(DecodedGpuFrameHandleKind::D3D11Texture2D, 7);
    let empty = PreviewNativeDecodedFrame::new(
        0,
        1080,
        handle.clone(),
        DecodedVideoSurfaceFormat::P010,
        p010_native_sampling(),
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
    )
    .expect_err("empty native payload extent must fail closed");
    assert_eq!(
        empty,
        PreviewNativeDecodedFrameError::EmptyExtent { width: 0, height: 1080 }
    );

    let unsupported = PreviewNativeDecodedFrame::new(
        1920,
        1080,
        handle,
        DecodedVideoSurfaceFormat::Yuv420p,
        DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Bt709,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::Left,
            bit_depth: 8,
        },
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
    )
    .expect_err("planar CPU surface must not masquerade as a native GPU payload");
    assert_eq!(
        unsupported,
        PreviewNativeDecodedFrameError::UnsupportedSurfaceFormat {
            surface_format: DecodedVideoSurfaceFormat::Yuv420p
        }
    );
}

#[test]
fn native_decoded_frame_payload_requires_complete_video_sampling() {
    let handle = test_native_handle(DecodedGpuFrameHandleKind::D3D11Texture2D, 7);

    let missing_range = PreviewNativeDecodedFrame::new(
        1920,
        1080,
        handle.clone(),
        DecodedVideoSurfaceFormat::P010,
        DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Bt2020NonConstant,
            range: DecodedVideoRange::Unknown,
            chroma_location: DecodedVideoChromaLocation::Left,
            bit_depth: 10,
        },
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
    )
    .expect_err("native payload must not guess range");
    assert_eq!(
        missing_range,
        PreviewNativeDecodedFrameError::MissingVideoRange {
            surface_format: DecodedVideoSurfaceFormat::P010
        }
    );

    let missing_chroma = PreviewNativeDecodedFrame::new(
        1920,
        1080,
        handle.clone(),
        DecodedVideoSurfaceFormat::P010,
        DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Bt2020NonConstant,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::Unknown,
            bit_depth: 10,
        },
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
    )
    .expect_err("native YCbCr payload must not guess chroma siting");
    assert_eq!(
        missing_chroma,
        PreviewNativeDecodedFrameError::MissingVideoChromaLocation {
            surface_format: DecodedVideoSurfaceFormat::P010
        }
    );

    let bit_depth_mismatch = PreviewNativeDecodedFrame::new(
        1920,
        1080,
        handle,
        DecodedVideoSurfaceFormat::P010,
        DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Bt2020NonConstant,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::Left,
            bit_depth: 8,
        },
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
    )
    .expect_err("P010 native payload must require 10-bit sampling");
    assert_eq!(
        bit_depth_mismatch,
        PreviewNativeDecodedFrameError::BitDepthMismatch {
            surface_format: DecodedVideoSurfaceFormat::P010,
            expected: 10,
            actual: 8,
        }
    );
}

fn p010_native_sampling() -> DecodedVideoSampling {
    DecodedVideoSampling {
        matrix: DecodedVideoMatrix::Bt2020NonConstant,
        range: DecodedVideoRange::Limited,
        chroma_location: DecodedVideoChromaLocation::TopLeft,
        bit_depth: 10,
    }
}

#[test]
fn cancellable_preview_decode_returns_canceled_before_opening_missing_file() {
    let path = PathBuf::from("E:/definitely-missing/canceled-preview.mov");
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::ZERO,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    )
    .with_max_size(Some(320), Some(180));
    let outcome = decode_preview_frame_cancellable(request, || true)
        .expect("canceled decode should not fail missing media");

    assert_eq!(
        match outcome {
            PreviewDecodeOutcome::Canceled(cancellation) => cancellation,
            other => panic!("expected cancellation, got {other:?}"),
        },
        PreviewDecodeCancellation::cooperative(
            PreviewDecodeCancellationCheckpoint::BeforeInputOpen,
        )
    );
}

#[test]
fn format_interrupt_callback_uses_only_the_active_request_probe() {
    let observer = PreviewDecodeExecutionObserver::new();
    let state = Arc::new(PreviewDecodeInterruptState::with_execution_observer(
        observer.clone(),
    ));
    let opaque = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    assert_eq!(unsafe { preview_decode_interrupt_callback(opaque) }, 0);

    let guard = state.install(Arc::new(|| true));
    assert_eq!(unsafe { preview_decode_interrupt_callback(opaque) }, 1);
    drop(guard);

    assert_eq!(unsafe { preview_decode_interrupt_callback(opaque) }, 0);
    let progress = observer.snapshot();
    assert_eq!(progress.interrupt_poll_sequence, 3);
    assert_eq!(progress.interrupt_cancel_sequence, 1);
    assert_eq!(progress.interrupt_last_cancel_request_sequence, 0);
}

#[test]
fn external_decode_process_is_reaped_when_cancellation_is_observed() {
    let mut command = Command::new("rustc");
    command.arg("--version").stdout(Stdio::piped()).stderr(Stdio::piped());

    let outcome = run_external_decode_command_cancellable(&mut command, 4 * 1024, &|| true)
        .expect("cancellation should reap the external process");

    assert!(outcome.is_none());
}

#[test]
fn playback_session_drains_reordered_frames_between_sequential_requests() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-bframes.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    clear_thread_local_preview_decode_session();

    let mut decoded_pixels = Vec::new();
    for index in 5..20 {
        let request = covering_decode_request(
            path.as_path(),
            TimelineTime::new(i64::from(index), 25).expect("exact source time"),
            PreviewDecodeAccessMode::PlaybackCursor,
            test_source_color(),
        )
        .with_max_size(Some(64), Some(64));
        let outcome = decode_preview_frame_cancellable(request, || false)
            .expect("sequential B-frame playback request must decode");
        let PreviewDecodeOutcome::Frame(frame) = outcome else {
            panic!("CPU playback fixture must return an RGBA frame");
        };
        if index > 5 {
            let expected = if frame.diagnostics.path == PreviewDecodePath::PlaybackSessionRingHit {
                PreviewDecodeSessionDisposition::BypassedCache
            } else {
                PreviewDecodeSessionDisposition::Reused
            };
            assert_eq!(frame.diagnostics.session_disposition, expected);
        }
        decoded_pixels.push(frame.rgba().to_vec());
    }

    assert!(
        decoded_pixels.windows(2).any(|frames| frames[0] != frames[1]),
        "the moving fixture must produce distinct decoded frames"
    );
    clear_thread_local_preview_decode_session();
}

#[test]
fn playback_session_reuses_decoder_across_adaptive_output_geometry() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-adaptive-output.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let mut context = PreviewDecodeSessionContext::new();
    let fingerprint = MediaFileFingerprint::capture(&path);

    for (request_index, (frame_index, extent)) in
        [(5, 64), (6, 32), (7, 64)].into_iter().enumerate()
    {
        let request = covering_decode_request(
            path.as_path(),
            TimelineTime::new(frame_index, 25).expect("exact source time"),
            PreviewDecodeAccessMode::PlaybackCursor,
            test_source_color(),
        )
        .with_max_size(Some(extent), Some(extent))
        .with_fingerprint(fingerprint);
        let outcome = context
            .decode_cancellable(request, || false)
            .unwrap_or_else(|error| panic!("adaptive frame {frame_index} must decode: {error}"));
        let PreviewDecodeOutcome::Frame(frame) = outcome else {
            panic!("software playback fixture must return an RGBA frame");
        };
        assert_eq!((frame.width, frame.height), (extent, extent));
        assert_eq!(
            frame.diagnostics.session_disposition,
            if request_index == 0 {
                PreviewDecodeSessionDisposition::Opened
            } else {
                PreviewDecodeSessionDisposition::Reused
            },
            "output-only scale changes must not reopen the source decoder"
        );
    }
}

#[test]
fn reverse_playback_replays_the_bounded_decoded_gop_tail_without_reseeking() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-reverse-window.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let mut context = PreviewDecodeSessionContext::new();
    let fingerprint = MediaFileFingerprint::capture(&path);

    for (request_index, frame_index) in [12, 11, 10].into_iter().enumerate() {
        let request = covering_decode_request(
            path.as_path(),
            TimelineTime::new(frame_index, 25).expect("exact source time"),
            PreviewDecodeAccessMode::PlaybackCursor,
            test_source_color(),
        )
        .with_max_size(Some(64), Some(64))
        .with_fingerprint(fingerprint)
        .with_adaptive_hints(PreviewDecodeAdaptiveHints {
            playback_direction: crate::preview::PreviewPlaybackDirection::Reverse,
            ..PreviewDecodeAdaptiveHints::default()
        });
        let outcome = context
            .decode_cancellable(request, || false)
            .unwrap_or_else(|error| panic!("reverse frame {frame_index} must decode: {error}"));
        let PreviewDecodeOutcome::Frame(frame) = outcome else {
            panic!("software reverse fixture must return an RGBA frame");
        };
        assert_eq!(frame.diagnostics.requested_pts, Some(frame_index * 512));
        assert_eq!(frame.diagnostics.selected_pts, Some(frame_index * 512));
        if request_index > 0 {
            assert!(!frame.diagnostics.seek_performed);
            assert_eq!(
                frame.diagnostics.decoded_frame_count, 0,
                "adjacent reverse frames must replay the retained GOP tail"
            );
            assert_eq!(
                frame.diagnostics.session_disposition,
                PreviewDecodeSessionDisposition::Reused
            );
        }
    }
}

#[test]
fn exact_random_access_decodes_stream_start_with_negative_dts_preroll() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-bframes-start.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let mut context = PreviewDecodeSessionContext::new();
    let fingerprint = MediaFileFingerprint::capture(&path);
    for frame_index in [0, 1] {
        let request = covering_decode_request(
            path.as_path(),
            TimelineTime::new(frame_index, 25).expect("exact source time"),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            test_source_color(),
        )
        .with_max_size(Some(16), Some(16))
        .with_video_stream_index(0)
        .with_fingerprint(fingerprint);

        let outcome = context
            .decode_cancellable(request, || false)
            .unwrap_or_else(|error| panic!("exact frame {frame_index} must decode: {error}"));
        let PreviewDecodeOutcome::Frame(frame) = outcome else {
            panic!("CPU exact fixture must return an RGBA frame");
        };
        assert_eq!(frame.diagnostics.requested_pts, Some(frame_index * 512));
        assert_eq!(frame.diagnostics.selected_pts, Some(frame_index * 512));
        assert!(!frame.diagnostics.temporal_approximation);
        assert_eq!(
            frame.diagnostics.session_disposition,
            if frame_index > 0 {
                PreviewDecodeSessionDisposition::Reused
            } else {
                PreviewDecodeSessionDisposition::Opened
            }
        );
    }
}

#[test]
#[ignore = "requires packaged mondrian executable via MONDRIAN_PREVIEW_DEMUX_WORKER_PATH"]
fn isolated_demux_worker_reuses_each_access_mode_session_across_requests() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let worker = std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH")
        .map(PathBuf::from)
        .expect("set MONDRIAN_PREVIEW_DEMUX_WORKER_PATH");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("isolated-h264-bframes.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let (bootstrap, observer) =
        PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(worker);
    let mut context = bootstrap.build();
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::new(8, 25).expect("exact source time"),
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    )
    .with_max_size(Some(64), Some(64));

    let outcome = context.decode_cancellable(request, || false).expect("isolated exact decode");
    let PreviewDecodeOutcome::Frame(frame) = outcome else {
        panic!("software exact decode must return an RGBA frame");
    };
    assert_eq!((frame.width, frame.height), (64, 64));
    assert!(frame.diagnostics.seek_performed);

    let second_request = covering_decode_request(
        path.as_path(),
        TimelineTime::new(2, 25).expect("second exact source time"),
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    )
    .with_max_size(Some(64), Some(64));
    let second_outcome = context
        .decode_cancellable(second_request, || false)
        .expect("second isolated exact decode");
    let PreviewDecodeOutcome::Frame(second_frame) = second_outcome else {
        panic!("second software exact decode must return an RGBA frame");
    };
    assert_eq!((second_frame.width, second_frame.height), (64, 64));
    assert!(second_frame.diagnostics.seek_performed);
    assert_eq!(
        second_frame.diagnostics.session_disposition,
        PreviewDecodeSessionDisposition::Reused,
        "isolated demux and codec state must be reused across exact requests"
    );

    for (access_mode, first_frame, second_frame) in [
        (PreviewDecodeAccessMode::ScrubCursor, 12, 13),
        (PreviewDecodeAccessMode::PlaybackCursor, 5, 6),
    ] {
        for (request_index, frame_index) in [first_frame, second_frame].into_iter().enumerate() {
            let request = covering_decode_request(
                path.as_path(),
                TimelineTime::new(frame_index, 25).expect("source time"),
                access_mode,
                test_source_color(),
            )
            .with_max_size(Some(64), Some(64));
            let outcome = context
                .decode_cancellable(request, || false)
                .expect("isolated access-mode decode");
            let PreviewDecodeOutcome::Frame(frame) = outcome else {
                panic!("software access-mode decode must return an RGBA frame");
            };
            if request_index == 1 {
                let expected =
                    if frame.diagnostics.path == PreviewDecodePath::PlaybackSessionRingHit {
                        PreviewDecodeSessionDisposition::BypassedCache
                    } else {
                        PreviewDecodeSessionDisposition::Reused
                    };
                assert_eq!(
                    frame.diagnostics.session_disposition, expected,
                    "{access_mode:?} must reuse its isolated demux/codec session"
                );
            }
        }
    }
    context.clear();
    let evidence = observer.snapshot().isolated_demux;
    assert_eq!(evidence.session_launches, 3);
    assert_eq!(evidence.ready_sessions, 3);
    assert_eq!(evidence.cross_request_reused_sessions, 3);
    assert!(evidence.completed_seeks >= 4);
    assert!(evidence.completed_reads > evidence.session_launches);
    assert!(evidence.packet_responses > 0);
    assert_eq!(evidence.clean_closes, 3);
    assert_eq!(evidence.reaped_sessions(), evidence.session_launches);
    assert_eq!(evidence.active_sessions, 0);
    assert_eq!(evidence.failure_terminations, 0);
    assert_eq!(evidence.forced_close_terminations, 0);
}

#[test]
#[ignore = "requires packaged mondrian executable via MONDRIAN_PREVIEW_DEMUX_WORKER_PATH"]
fn isolated_demux_worker_cancellation_terminates_the_packet_source() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let worker = std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH")
        .map(PathBuf::from)
        .expect("set MONDRIAN_PREVIEW_DEMUX_WORKER_PATH");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("isolated-cancel-h264-bframes.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let (bootstrap, observer) =
        PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(worker);
    let mut context = bootstrap.build();
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::new(8, 25).expect("exact source time"),
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    );
    let started = Instant::now();
    let cancellation_observer = observer.clone();

    let outcome = context
        .decode_cancellable(request, move || {
            cancellation_observer.snapshot().stage == PreviewDecodeExecutionStage::InputOpen
        })
        .expect("isolated cancellation outcome");
    let PreviewDecodeOutcome::Canceled(cancellation) = outcome else {
        panic!("deadline must cancel isolated exact decode");
    };
    assert_eq!(
        cancellation.source,
        PreviewDecodeCancellationSource::IsolatedDemuxTermination
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let evidence = observer.snapshot().isolated_demux;
    assert_eq!(evidence.session_launches, 1);
    assert_eq!(evidence.cancellation_terminations, 1);
    assert_eq!(evidence.reaped_sessions(), 1);
    assert_eq!(evidence.active_sessions, 0);
}

#[test]
#[ignore = "requires packaged mondrian executable via MONDRIAN_PREVIEW_DEMUX_WORKER_PATH"]
fn isolated_demux_worker_attributes_stream_info_seek_and_packet_read_cancellation() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let worker = std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH")
        .map(PathBuf::from)
        .expect("set MONDRIAN_PREVIEW_DEMUX_WORKER_PATH");
    let root = tempfile::tempdir().expect("tempdir");

    for (name, stage, checkpoint) in [
        (
            "stream-info",
            PreviewDecodeExecutionStage::StreamInfo,
            PreviewDecodeCancellationCheckpoint::StreamInfo,
        ),
        (
            "seek",
            PreviewDecodeExecutionStage::Seek,
            PreviewDecodeCancellationCheckpoint::Seek,
        ),
        (
            "packet-read",
            PreviewDecodeExecutionStage::PacketRead,
            PreviewDecodeCancellationCheckpoint::PacketRead,
        ),
    ] {
        let path = root.path().join(format!("isolated-{name}.mp4"));
        std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
        let (bootstrap, observer) =
            PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(worker.clone());
        let mut context = bootstrap.build();
        let request = covering_decode_request(
            path.as_path(),
            TimelineTime::new(8, 25).expect("exact source time"),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            test_source_color(),
        );
        let cancellation_observer = observer.clone();
        let outcome = context
            .decode_cancellable(request, move || {
                cancellation_observer.snapshot().stage == stage
            })
            .expect("isolated stage cancellation outcome");
        let PreviewDecodeOutcome::Canceled(cancellation) = outcome else {
            panic!("{name} checkpoint must cancel isolated decode");
        };
        assert_eq!(cancellation.checkpoint, checkpoint);
        assert_eq!(
            cancellation.source,
            PreviewDecodeCancellationSource::IsolatedDemuxTermination
        );
        let evidence = observer.snapshot().isolated_demux;
        assert_eq!(evidence.session_launches, 1);
        assert_eq!(evidence.cancellation_terminations, 1);
        assert_eq!(evidence.reaped_sessions(), 1);
        assert_eq!(evidence.active_sessions, 0);
    }
}

#[test]
#[ignore = "requires packaged mondrian executable via MONDRIAN_PREVIEW_DEMUX_WORKER_PATH"]
fn isolated_demux_worker_rejects_stale_source_revision_before_publication() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let worker = std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH")
        .map(PathBuf::from)
        .expect("set MONDRIAN_PREVIEW_DEMUX_WORKER_PATH");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("isolated-stale-revision.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let mut stale_revision = MediaFileFingerprint::capture(&path);
    stale_revision.len = stale_revision.len.map(|length| length.saturating_add(1));
    let (bootstrap, observer) =
        PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(worker);
    let mut context = bootstrap.build();
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::new(8, 25).expect("exact source time"),
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    )
    .with_fingerprint(stale_revision);

    let error = context
        .decode_cancellable(request, || false)
        .expect_err("stale source revision must fail before packet publication");
    assert!(matches!(
        error,
        MondrianError::MediaSourceRevisionChanged { .. }
    ));
    let evidence = observer.snapshot().isolated_demux;
    assert_eq!(evidence.session_launches, 0);
    assert_eq!(evidence.failure_terminations, 0);
    assert_eq!(evidence.reaped_sessions(), 0);
    assert_eq!(evidence.active_sessions, 0);
}

#[test]
fn preview_decode_rejects_replaced_source_against_caller_revision() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("replaced-before-preview-open.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let admitted = MediaFileFingerprint::capture(&path);
    let replacement = root.path().join("same-length-replacement.mp4");
    std::fs::write(&replacement, FIXTURE).expect("write same-length replacement");
    std::fs::remove_file(&path).expect("unlink admitted source");
    std::fs::rename(&replacement, &path).expect("install same-length replacement");
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::ZERO,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    )
    .with_fingerprint(admitted);

    let error = decode_preview_frame_cancellable(request, || false)
        .expect_err("replaced source must fail before Preview input publication");
    let MondrianError::MediaSourceRevisionChanged { path: rejected_path, expected, actual } = error
    else {
        panic!("expected typed source-revision failure");
    };
    assert_eq!(rejected_path, path.display().to_string());
    assert_eq!(*expected, admitted);
    assert_ne!(*actual, admitted);
    assert!(actual.authorizes_reuse());
    assert_eq!(actual.len, admitted.len);
}

#[test]
fn preview_decode_revalidates_revision_after_frame_materialization() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("replaced-before-result-publication.mov");
    std::fs::write(&path, b"old-frame").expect("write admitted source");
    let admitted = MediaFileFingerprint::capture(&path);
    let replacement = root.path().join("same-length-result-replacement.mov");
    std::fs::write(&replacement, b"new-frame").expect("write same-length replacement");
    std::fs::remove_file(&path).expect("unlink admitted source");
    std::fs::rename(&replacement, &path).expect("install same-length replacement");
    let frame = RgbaFrame::new(
        1,
        1,
        vec![0, 0, 0, 255],
        DecodedRgbaFrameContract::source_encoded(
            test_source_color(),
            DecodedVideoMatrix::Bt709,
            DecodedVideoRange::Limited,
        ),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    );
    let opened_frame =
        frame.clone().with_session_disposition(PreviewDecodeSessionDisposition::Opened);

    let error =
        finalize_preview_decode_outcome(&path, admitted, PreviewDecodeOutcome::Frame(opened_frame))
            .expect_err("post-materialization replacement must fail before result publication");
    assert!(matches!(
        error,
        MondrianError::MediaSourceRevisionChanged { .. }
    ));
}

#[test]
fn finalizer_skips_filesystem_revalidation_for_reused_session_frames() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("reused-session-frame.mov");
    std::fs::write(&path, b"stable-source").expect("write stable source");
    let fingerprint = MediaFileFingerprint::capture(&path);
    let frame = RgbaFrame::new(
        1,
        1,
        vec![0, 0, 0, 255],
        DecodedRgbaFrameContract::source_encoded(
            test_source_color(),
            DecodedVideoMatrix::Bt709,
            DecodedVideoRange::Limited,
        ),
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    )
    .with_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame)
    .with_temporal_selection(118, Some(DecodedTemporalExtent::from_duration(100, 20)))
    .with_session_disposition(PreviewDecodeSessionDisposition::Reused);

    let outcome =
        finalize_preview_decode_outcome(&path, fingerprint, PreviewDecodeOutcome::Frame(frame))
            .expect("reused-session frames skip the post-decode filesystem revalidation");
    assert!(matches!(outcome, PreviewDecodeOutcome::Frame(_)));
}

#[test]
fn finalizer_rejects_exact_frames_without_proven_temporal_selection() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("unproven-external-still.mov");
    std::fs::write(&path, b"stable-source").expect("write stable source");
    let fingerprint = MediaFileFingerprint::capture(&path);
    let frame = RgbaFrame::new(
        1,
        1,
        vec![0, 0, 0, 255],
        test_rgba_contract(),
        PreviewDecodePath::ExternalFfmpegCpuRgba,
    )
    .with_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame);

    assert!(
        !external_exact_frame_is_publishable(&path, &frame)
            .expect("unproven external output should be discarded, not fail fallback"),
        "external rawvideo without PTS/extent must continue to the in-process exact decoder"
    );

    let error =
        finalize_preview_decode_outcome(&path, fingerprint, PreviewDecodeOutcome::Frame(frame))
            .expect_err("an exact backend without PTS/extent evidence must fail closed");
    assert!(matches!(
        error,
        MondrianError::DecodeTemporalMismatch { requested_pts: None, selected_pts: None, .. }
    ));
}

#[test]
fn finalizer_accepts_exact_interval_coverage_and_rejects_a_true_gap() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("temporal-contract.mov");
    std::fs::write(&path, b"stable-source").expect("write stable source");
    let fingerprint = MediaFileFingerprint::capture(&path);
    let frame = || {
        RgbaFrame::new(
            1,
            1,
            vec![0, 0, 0, 255],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        )
        .with_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame)
    };

    let covered =
        frame().with_temporal_selection(118, Some(DecodedTemporalExtent::from_duration(100, 20)));
    finalize_preview_decode_outcome(&path, fingerprint, PreviewDecodeOutcome::Frame(covered))
        .expect("an interior point in [start, end) is exact");

    let gap =
        frame().with_temporal_selection(118, Some(DecodedTemporalExtent::from_duration(100, 5)));
    let error =
        finalize_preview_decode_outcome(&path, fingerprint, PreviewDecodeOutcome::Frame(gap))
            .expect_err("an exact request in a true presentation gap must fail closed");
    assert!(matches!(
        error,
        MondrianError::DecodeTemporalMismatch {
            requested_pts: Some(118),
            selected_pts: Some(100),
            selected_duration_pts: Some(5),
            ..
        }
    ));
}

#[test]
fn canceled_codec_work_forces_seek_before_session_reuse() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-cancel-recovery.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let fingerprint = MediaFileFingerprint::capture(&path);
    let mut context = PreviewDecodeSessionContext::new();
    let request = |frame| {
        covering_decode_request(
            path.as_path(),
            TimelineTime::new(frame, 25).expect("exact source time"),
            PreviewDecodeAccessMode::PlaybackCursor,
            test_source_color(),
        )
        .with_fingerprint(fingerprint)
        .with_max_size(Some(64), Some(64))
        // Native-preferred requests bypass both CPU cache layers, keeping the
        // cancellation probe on the session/codec path even on CPU fallback.
        .with_hardware_decode_request(PreviewHardwareDecodeRequest::PreferGpuResident)
    };

    let initial = context
        .decode_cancellable(request(5), || false)
        .expect("initial playback frame");
    assert!(!matches!(initial, PreviewDecodeOutcome::Canceled(_)));
    drop(initial);

    let mut codec_cancellation = None;
    for cancel_after in 1..=64 {
        let checks = Arc::new(AtomicUsize::new(0));
        let probe_checks = Arc::clone(&checks);
        let outcome = context
            .decode_cancellable(request(8), move || {
                probe_checks.fetch_add(1, Ordering::AcqRel) >= cancel_after
            })
            .expect("cancellable playback frame");
        if let PreviewDecodeOutcome::Canceled(cancellation) = outcome
            && cancellation.checkpoint == PreviewDecodeCancellationCheckpoint::Codec
        {
            codec_cancellation = Some(cancellation);
            break;
        }
    }
    assert!(
        codec_cancellation.is_some(),
        "test probe must reach a codec-phase cancellation"
    );
    let codec_cancellation = codec_cancellation.expect("codec cancellation evidence");
    assert_eq!(
        codec_cancellation.session_disposition,
        PreviewDecodeSessionDisposition::Reused
    );
    assert_eq!(codec_cancellation.session_open_us, 0);

    let recovered = context
        .decode_cancellable(request(9), || false)
        .expect("playback frame after cancellation");
    let diagnostics = match recovered {
        PreviewDecodeOutcome::Frame(frame) => frame.diagnostics,
        PreviewDecodeOutcome::FloatFrame(frame) => frame.diagnostics,
        PreviewDecodeOutcome::CpuYuvFrame(frame) => frame.diagnostics,
        PreviewDecodeOutcome::NativeGpuFrame(frame) => frame.diagnostics,
        PreviewDecodeOutcome::Canceled(cancellation) => {
            panic!("recovery decode unexpectedly canceled: {cancellation:?}")
        }
    };
    assert_eq!(
        diagnostics.session_disposition,
        PreviewDecodeSessionDisposition::Reused
    );
    assert!(
        diagnostics.seek_performed,
        "a canceled codec position must never continue as forward-reusable state"
    );
}

#[test]
fn preview_file_fingerprint_changes_when_file_is_replaced() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("proxy.mp4");
    std::fs::write(&path, b"old").expect("old");
    let first = MediaFileFingerprint::capture(&path);
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, b"new proxy bytes").expect("new");
    let second = MediaFileFingerprint::capture(&path);

    assert_ne!(first, second);
}

#[test]
fn incomplete_preview_fingerprint_never_authorizes_residency_reuse() {
    let fingerprint = MediaFileFingerprint::default();
    assert!(!fingerprint.authorizes_reuse());
}

fn test_packet(pts: Option<i64>, dts: Option<i64>, key: bool) -> ffmpeg::Packet {
    let mut packet = ffmpeg::Packet::empty();
    packet.set_pts(pts);
    packet.set_dts(dts);
    if key {
        packet.set_flags(ffmpeg::codec::packet::Flags::KEY);
    }
    packet
}

#[test]
#[ignore = "manual decode performance diagnostic; set MONDRIAN_PREVIEW_DECODE_FIXTURE"]
fn preview_decode_fixture_perf_smoke() {
    let Some(path) = std::env::var_os("MONDRIAN_PREVIEW_DECODE_FIXTURE").map(PathBuf::from) else {
        eprintln!(
            "MONDRIAN_PREVIEW_DECODE_PERF_JSON={{\"skipped\":\"MONDRIAN_PREVIEW_DECODE_FIXTURE not set\"}}"
        );
        return;
    };
    let timestamp_secs = std::env::var("MONDRIAN_PREVIEW_DECODE_TIMESTAMP")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(1.0);
    let max_width = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_WIDTH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let max_height = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());

    let started = Instant::now();
    let request = covering_decode_request(
        path.as_path(),
        TimelineTime::from_f64_quantized(timestamp_secs, 1_000_000)
            .expect("quantized diagnostic source time"),
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        test_source_color(),
    )
    .with_max_size(max_width, max_height);
    let frame = match decode_preview_frame_cancellable(request, || false)
        .expect("decode preview fixture")
    {
        PreviewDecodeOutcome::Frame(frame) => frame,
        PreviewDecodeOutcome::Canceled(_) => {
            panic!("still-frame perf decode canceled")
        }
        PreviewDecodeOutcome::NativeGpuFrame(_) => {
            panic!("still-frame perf decode requires CPU RGBA output")
        }
        PreviewDecodeOutcome::FloatFrame(_) => {
            panic!("Rec.709 still-frame perf fixture unexpectedly decoded as float")
        }
        PreviewDecodeOutcome::CpuYuvFrame(_) => {
            panic!("still-frame perf decode requires CPU RGBA output")
        }
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let report = PreviewDecodePerfReport {
        path: path.display().to_string(),
        timestamp_secs,
        max_width,
        max_height,
        decoded_width: frame.width,
        decoded_height: frame.height,
        rgba_bytes: frame.data.len(),
        elapsed_ms,
        diagnostics_elapsed_us: frame.diagnostics.elapsed_us,
        path_kind: frame.diagnostics.path.as_str(),
        cache_hit: frame.diagnostics.cache_hit,
        cpu_resident: frame.diagnostics.cpu_resident,
        seek_performed: frame.diagnostics.seek_performed,
        decoded_frame_count: frame.diagnostics.decoded_frame_count,
        threading_kind: frame.diagnostics.threading_kind.as_str(),
        threading_count: frame.diagnostics.threading_count,
        stage_durations: frame.diagnostics.stage_durations,
    };
    let json = serde_json::to_string(&report).expect("serialize decode perf report");
    eprintln!("MONDRIAN_PREVIEW_DECODE_PERF_JSON={json}");
    clear_thread_local_preview_decode_session();
}

#[test]
#[ignore = "manual sequential decode performance diagnostic; set MONDRIAN_PREVIEW_DECODE_FIXTURE"]
fn preview_decode_fixture_sequence_perf_smoke() {
    let Some(path) = std::env::var_os("MONDRIAN_PREVIEW_DECODE_FIXTURE").map(PathBuf::from) else {
        eprintln!(
            "MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_JSON={{\"skipped\":\"MONDRIAN_PREVIEW_DECODE_FIXTURE not set\"}}"
        );
        return;
    };
    let start_secs = std::env::var("MONDRIAN_PREVIEW_DECODE_TIMESTAMP")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let frame_rate_spec =
        std::env::var("MONDRIAN_PREVIEW_DECODE_FRAME_RATE").unwrap_or_else(|_| "25/1".to_owned());
    let exact_frame_rate = frame_rate_spec.split_once('/').and_then(|(numerator, denominator)| {
        let numerator = numerator.parse::<i64>().ok()?;
        let denominator = denominator.parse::<i64>().ok()?;
        (numerator > 0 && denominator > 0).then_some((numerator, denominator))
    });
    let frame_rate = exact_frame_rate
        .map(|(numerator, denominator)| numerator as f64 / denominator as f64)
        .or_else(|| frame_rate_spec.parse::<f64>().ok())
        .unwrap_or(25.0)
        .max(1.0);
    let frame_count = std::env::var("MONDRIAN_PREVIEW_DECODE_SEQUENCE_FRAMES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(25)
        .clamp(1, 240);
    let max_width = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_WIDTH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let max_height = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let p95_budget_us = std::env::var("MONDRIAN_PREVIEW_DECODE_P95_BUDGET_US")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let compact_yuv = std::env::var("MONDRIAN_PREVIEW_DECODE_COMPACT_YUV")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let demux_worker = std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH").map(PathBuf::from);

    clear_thread_local_preview_decode_session();
    let (mut context, demux_mode, demux_observer) = if let Some(worker) = demux_worker {
        let (bootstrap, observer) =
            PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(worker);
        (bootstrap.build(), "isolated_process", Some(observer))
    } else {
        (PreviewDecodeSessionContext::new(), "in_process", None)
    };
    let representation = if compact_yuv {
        PreviewDecodeRepresentation::CompactCpuYuv
    } else {
        PreviewDecodeRepresentation::NativeCpu
    };
    let representation_name = if compact_yuv {
        "compact_cpu_yuv"
    } else {
        "native_cpu"
    };
    let access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    let fingerprint = MediaFileFingerprint::capture(&path);
    let mut frames = Vec::with_capacity(frame_count);
    let mut total_us = 0u64;
    let mut max_us = 0u64;
    let mut uncached_total_us = 0u64;
    let mut uncached_frame_count = 0usize;
    let mut uncached_max_us = 0u64;
    let mut total_stage_durations = PreviewDecodeStageDurations::default();
    let mut max_frame_stage_durations = PreviewDecodeStageDurations::default();
    let start_time = TimelineTime::from_f64_quantized(start_secs, 1_000_000)
        .expect("quantized diagnostic start time");
    let started = Instant::now();
    for index in 0..frame_count {
        let source_time = if let Some((numerator, denominator)) = exact_frame_rate {
            let frame_numerator = i64::try_from(index)
                .expect("bounded diagnostic frame index")
                .checked_mul(denominator)
                .expect("bounded diagnostic frame offset");
            start_time
                .checked_add(
                    TimelineTime::new(frame_numerator, numerator)
                        .expect("exact diagnostic frame offset"),
                )
                .expect("bounded diagnostic source time")
        } else {
            TimelineTime::from_f64_quantized(start_secs + index as f64 / frame_rate, 1_000_000)
                .expect("quantized diagnostic source time")
        };
        let timestamp_secs = source_time.to_f64();
        let frame_started = Instant::now();
        let mut request = covering_decode_request(
            path.as_path(),
            source_time,
            PreviewDecodeAccessMode::PlaybackCursor,
            test_source_color().with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
        )
        .with_max_size(max_width, max_height)
        .with_fingerprint(fingerprint);
        request.representation = representation;
        let (decoded_width, decoded_height, diagnostics) = match context
            .decode_cancellable(request, || false)
            .expect("decode preview fixture frame")
        {
            PreviewDecodeOutcome::Frame(frame) => (frame.width, frame.height, frame.diagnostics),
            PreviewDecodeOutcome::FloatFrame(frame) => {
                (frame.width, frame.height, frame.diagnostics)
            }
            PreviewDecodeOutcome::Canceled(_) => {
                panic!("playback sequence perf decode canceled")
            }
            PreviewDecodeOutcome::NativeGpuFrame(_) => {
                panic!("playback sequence perf decode requires CPU RGBA output")
            }
            PreviewDecodeOutcome::CpuYuvFrame(frame) if compact_yuv => {
                (frame.width, frame.height, frame.diagnostics)
            }
            PreviewDecodeOutcome::CpuYuvFrame(_) => {
                panic!("native CPU playback perf decode returned compact GPU input")
            }
        };
        let elapsed_us = duration_us(frame_started.elapsed());
        total_us = total_us.saturating_add(elapsed_us);
        max_us = max_us.max(elapsed_us);
        if !diagnostics.cache_hit {
            uncached_total_us = uncached_total_us.saturating_add(elapsed_us);
            uncached_frame_count = uncached_frame_count.saturating_add(1);
            uncached_max_us = uncached_max_us.max(elapsed_us);
        }
        total_stage_durations.accumulate(diagnostics.stage_durations);
        max_frame_stage_durations =
            max_stage_durations(max_frame_stage_durations, diagnostics.stage_durations);
        frames.push(PreviewDecodeSequenceFrameReport {
            index,
            timestamp_secs,
            elapsed_us,
            decoded_width,
            decoded_height,
            cache_hit: diagnostics.cache_hit,
            seek_performed: diagnostics.seek_performed,
            decoded_frame_count: diagnostics.decoded_frame_count,
            threading_kind: diagnostics.threading_kind.as_str(),
            threading_count: diagnostics.threading_count,
            stage_durations: diagnostics.stage_durations,
        });
    }
    let wall_us = duration_us(started.elapsed());
    let p95_us = percentile_upper_bound_us(frames.iter().map(|frame| frame.elapsed_us), 95);
    let uncached_p95_us = percentile_upper_bound_us(
        frames.iter().filter(|frame| !frame.cache_hit).map(|frame| frame.elapsed_us),
        95,
    );
    context.clear();
    let isolated_demux = demux_observer.map(|observer| observer.snapshot().isolated_demux);
    let report = PreviewDecodeSequencePerfReport {
        path: path.display().to_string(),
        demux_mode,
        representation: representation_name,
        access_mode: access_mode.as_str(),
        start_secs,
        frame_rate,
        frame_rate_numerator: exact_frame_rate.map(|(numerator, _)| numerator),
        frame_rate_denominator: exact_frame_rate.map(|(_, denominator)| denominator),
        frame_count,
        max_width,
        max_height,
        total_us,
        wall_us,
        avg_us: total_us / frame_count as u64,
        p95_us,
        max_us,
        uncached_frame_count,
        uncached_avg_us: average_us(uncached_total_us, uncached_frame_count),
        uncached_p95_us,
        uncached_max_us,
        p95_budget_us,
        total_stage_durations,
        max_frame_stage_durations,
        isolated_demux,
        frames,
    };
    let json = serde_json::to_string(&report).expect("serialize sequence decode perf report");
    eprintln!(
        "MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_SUMMARY path=\"{}\" demux_mode={} representation={} access_mode={} frames={} avg_us={} p95_us={} max_us={} uncached_frames={} uncached_avg_us={} uncached_p95_us={} uncached_max_us={} p95_budget_us={:?} packet_decode_us={} hardware_transfer_us={} swscale_us={} rgba_copy_us={}",
        report.path,
        report.demux_mode,
        report.representation,
        report.access_mode,
        report.frame_count,
        report.avg_us,
        report.p95_us,
        report.max_us,
        report.uncached_frame_count,
        report.uncached_avg_us,
        report.uncached_p95_us,
        report.uncached_max_us,
        report.p95_budget_us,
        report.total_stage_durations.packet_decode_us,
        report.total_stage_durations.hardware_transfer_us,
        report.total_stage_durations.swscale_us,
        report.total_stage_durations.rgba_copy_us,
    );
    eprintln!("MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_JSON={json}");
    if let Some(p95_budget_us) = report.p95_budget_us {
        assert!(
            report.p95_us <= p95_budget_us,
            "preview decode p95 {}us exceeded budget {}us",
            report.p95_us,
            p95_budget_us
        );
    }
    clear_thread_local_preview_decode_session();
}

fn average_us(total_us: u64, frame_count: usize) -> u64 {
    if frame_count == 0 {
        return 0;
    }
    total_us / frame_count as u64
}

fn max_stage_durations(
    lhs: PreviewDecodeStageDurations,
    rhs: PreviewDecodeStageDurations,
) -> PreviewDecodeStageDurations {
    PreviewDecodeStageDurations {
        session_open_us: lhs.session_open_us.max(rhs.session_open_us),
        output_lease_wait_us: lhs.output_lease_wait_us.max(rhs.output_lease_wait_us),
        cache_lookup_us: lhs.cache_lookup_us.max(rhs.cache_lookup_us),
        seek_us: lhs.seek_us.max(rhs.seek_us),
        packet_decode_us: lhs.packet_decode_us.max(rhs.packet_decode_us),
        hardware_transfer_us: lhs.hardware_transfer_us.max(rhs.hardware_transfer_us),
        swscale_us: lhs.swscale_us.max(rhs.swscale_us),
        rgba_copy_us: lhs.rgba_copy_us.max(rhs.rgba_copy_us),
        external_process_us: lhs.external_process_us.max(rhs.external_process_us),
    }
}

fn percentile_upper_bound_us(samples: impl Iterator<Item = u64>, percentile: usize) -> u64 {
    let mut samples = samples.collect::<Vec<_>>();
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let percentile = percentile.min(100);
    let rank = samples.len().saturating_mul(percentile).saturating_add(99) / 100;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[derive(Debug, Serialize)]
struct PreviewDecodePerfReport {
    path: String,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    decoded_width: u32,
    decoded_height: u32,
    rgba_bytes: usize,
    elapsed_ms: u64,
    diagnostics_elapsed_us: u64,
    path_kind: &'static str,
    cache_hit: bool,
    cpu_resident: bool,
    seek_performed: bool,
    decoded_frame_count: u32,
    threading_kind: &'static str,
    threading_count: u32,
    stage_durations: PreviewDecodeStageDurations,
}

#[derive(Debug, Serialize)]
struct PreviewDecodeSequencePerfReport {
    path: String,
    demux_mode: &'static str,
    representation: &'static str,
    access_mode: &'static str,
    start_secs: f64,
    frame_rate: f64,
    frame_rate_numerator: Option<i64>,
    frame_rate_denominator: Option<i64>,
    frame_count: usize,
    max_width: Option<u32>,
    max_height: Option<u32>,
    total_us: u64,
    wall_us: u64,
    avg_us: u64,
    p95_us: u64,
    max_us: u64,
    uncached_frame_count: usize,
    uncached_avg_us: u64,
    uncached_p95_us: u64,
    uncached_max_us: u64,
    p95_budget_us: Option<u64>,
    total_stage_durations: PreviewDecodeStageDurations,
    max_frame_stage_durations: PreviewDecodeStageDurations,
    isolated_demux: Option<PreviewIsolatedDemuxExecutionEvidence>,
    frames: Vec<PreviewDecodeSequenceFrameReport>,
}

#[derive(Debug, Serialize)]
struct PreviewDecodeSequenceFrameReport {
    index: usize,
    timestamp_secs: f64,
    elapsed_us: u64,
    decoded_width: u32,
    decoded_height: u32,
    cache_hit: bool,
    seek_performed: bool,
    decoded_frame_count: u32,
    threading_kind: &'static str,
    threading_count: u32,
    stage_durations: PreviewDecodeStageDurations,
}

fn reduced_test_stream() -> crate::info::VideoStreamInfo {
    use crate::info::{PixelFormat, VideoStreamInfo};
    VideoStreamInfo {
        index: 0,
        codec: VideoCodec::H264,
        duration: Some(Duration::from_secs(1)),
        codec_profile: VideoCodecProfile::H264Main,
        width: 64,
        height: 64,
        picture: mondrian_core::PictureStreamMetadata::default(),
        frame_rate: Rational::new(25, 1),
        frame_rate_proven: true,
        pixel_format: PixelFormat::Yuv420p,
        pixel_format_proven: true,
        color_range: DecodedVideoRange::Limited,
        color_interpretation: mondrian_core::DetectedColorInterpretation::decoder_unavailable(),
        color_metadata: None,
        color_metadata_hints: Vec::new(),
        hdr_metadata: Vec::new(),
        bit_depth: 8,
        has_alpha: false,
        avg_bitrate: 1,
        total_frames: Some(25),
    }
}

#[test]
fn reduced_representation_materializes_the_reduced_raster_not_the_source_raster() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-reduced.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    clear_thread_local_preview_decode_session();
    let fingerprint = MediaFileFingerprint::capture(&path);
    let source = crate::preview::PreviewDecodeSource::from_probed_stream(
        path.as_path(),
        fingerprint,
        &reduced_test_stream(),
    )
    .expect("valid reduced source");
    let color = test_source_color();
    let half_key = crate::preview::PreviewDecodeKey::new(
        source,
        SourceSampleTarget::covering(TimelineTime::new(10, 25).expect("frame 10")),
        crate::preview::PreviewDecodeRepresentation::Reduced {
            divisor: NonZeroU32::new(2).expect("divisor"),
        },
        color,
    )
    .expect("valid reduced key");
    let request = crate::preview::PreviewDecodeRequest::from_key(
        &half_key,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    );
    let outcome =
        decode_preview_frame_cancellable(request, || false).expect("reduced decode must succeed");
    let PreviewDecodeOutcome::Frame(frame) = outcome else {
        panic!("reduced decode must return an RGBA frame");
    };
    assert_eq!(
        (frame.width, frame.height),
        (32, 32),
        "a Reduced(2) representation must materialize the reduced raster, never the 64x64 source raster"
    );
    clear_thread_local_preview_decode_session();
}

#[test]
fn full_and_reduced_representations_decode_independently_from_the_same_source() {
    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("h264-full-reduced.mp4");
    std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
    let mut context = PreviewDecodeSessionContext::new();
    let fingerprint = MediaFileFingerprint::capture(&path);

    let key_for = |representation: crate::preview::PreviewDecodeRepresentation, frame: usize| {
        let source = crate::preview::PreviewDecodeSource::from_probed_stream(
            path.as_path(),
            fingerprint,
            &reduced_test_stream(),
        )
        .expect("valid source");
        crate::preview::PreviewDecodeKey::new(
            source,
            SourceSampleTarget::covering(
                TimelineTime::new(i64::try_from(frame).expect("frame in range"), 25)
                    .expect("frame"),
            ),
            representation,
            test_source_color(),
        )
        .expect("valid key")
    };

    for (index, (representation, expected)) in [
        (
            crate::preview::PreviewDecodeRepresentation::NativeCpu,
            (64, 64),
        ),
        (
            crate::preview::PreviewDecodeRepresentation::Reduced {
                divisor: NonZeroU32::new(2).expect("divisor"),
            },
            (32, 32),
        ),
        (
            crate::preview::PreviewDecodeRepresentation::Reduced {
                divisor: NonZeroU32::new(4).expect("divisor"),
            },
            (16, 16),
        ),
        (
            crate::preview::PreviewDecodeRepresentation::NativeCpu,
            (64, 64),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let key = key_for(representation, index + 10);
        let request = crate::preview::PreviewDecodeRequest::from_key(
            &key,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        );
        let outcome = context
            .decode_cancellable(request, || false)
            .unwrap_or_else(|error| panic!("representation {index} must decode: {error}"));
        let PreviewDecodeOutcome::Frame(frame) = outcome else {
            panic!("representation {index} must return an RGBA frame");
        };
        assert_eq!(
            frame.diagnostics.session_disposition,
            if index == 0 {
                PreviewDecodeSessionDisposition::Opened
            } else {
                PreviewDecodeSessionDisposition::Reused
            },
            "materialization representation changes must not reopen the compressed-stream decoder"
        );
        assert_eq!(
            (frame.width, frame.height),
            expected,
            "representation {index} materialized the wrong raster"
        );
    }
}
