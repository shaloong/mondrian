use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::{AssetId, ColorEngine, ColorSpace, OutputTransformIntent, Rational};
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{
    DecodedVideoRange, DecodedVideoRangeContract, DetectedColorInterpretation, MediaInfo,
    VideoColorDetectionMethod, VideoColorInterpretationConfidence, VideoColorSpaceSource,
    VideoStreamInfo,
};
use mondrian_timeline::sequence::{MissingColorMetadataPolicy, SequenceSettings};

use super::analysis::{color_manage_rgba, thumbnail_key, ThumbnailColorContract};
use super::*;

fn color_context() -> ColorContext {
    SequenceSettings::default().root_preview_color_context(
        &mondrian_core::ProjectColorManagement::default(),
        ColorSpace::Srgb,
    )
}

fn color_contract() -> ThumbnailColorContract {
    ThumbnailColorContract {
        source_color_space: ColorSpace::Rec709,
        source_range: DecodedVideoRangeContract::Automatic {
            probed_range: DecodedVideoRange::Limited,
        },
        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        output_color_space: ColorSpace::Srgb,
        tone_map: true,
        engine: ColorEngine::mondrian_standard(),
        output_transform: OutputTransformIntent::mondrian_standard(),
        ocio_generation: mondrian_core::ocio_config_generation(),
    }
}

fn fingerprint(seed: u64) -> PreviewFileFingerprint {
    PreviewFileFingerprint {
        len: Some(seed),
        modified_secs: Some(seed),
        modified_nanos: Some(seed as u32),
    }
}

fn key(asset_id: AssetId, seed: u64) -> ThumbnailRequestKey {
    ThumbnailRequestKey {
        asset_id,
        path: PathBuf::from(format!("E:/media/thumb-{seed}.mov")),
        fingerprint: fingerprint(seed),
        color: color_contract(),
    }
}

fn frame(seed: u64) -> ThumbnailRasterFrame {
    ThumbnailRasterFrame::new(
        format!("thumb-{seed}"),
        1,
        1,
        ThumbnailRasterColorSpace::Srgb,
        vec![seed as u8, 0, 0, 255],
    )
    .expect("valid test frame")
}

fn isolated_service() -> AssetThumbnailService {
    let (job_tx, _job_rx) = mpsc::sync_channel(1);
    let (_result_tx, result_rx) = mpsc::sync_channel(1);
    AssetThumbnailService {
        state: Mutex::new(ThumbnailState::default()),
        jobs: job_tx,
        results: Mutex::new(result_rx),
    }
}

fn service_with_result_transport() -> (AssetThumbnailService, mpsc::SyncSender<ThumbnailResult>) {
    let (job_tx, _job_rx) = mpsc::sync_channel(1);
    let (result_tx, result_rx) = mpsc::sync_channel(4);
    (
        AssetThumbnailService {
            state: Mutex::new(ThumbnailState::default()),
            jobs: job_tx,
            results: Mutex::new(result_rx),
        },
        result_tx,
    )
}

fn mark_pending(service: &AssetThumbnailService, key: &ThumbnailRequestKey, generation: u64) {
    let cancellation = ExecutionCancellationToken::new();
    let mut state = service.state.lock();
    state.pending.insert(key.clone(), PendingThumbnail { generation, cancellation });
    state.active.insert(key.asset_id, key.clone());
}

fn video_asset(path: PathBuf) -> AssetRecord {
    let mut media_info = MediaInfo::synthetic_solid_color();
    media_info.has_video = true;
    media_info.video_streams.push(VideoStreamInfo {
        index: 0,
        codec: VideoCodec::H264,
        duration: Some(Duration::from_secs(1)),
        codec_profile: mondrian_media::VideoCodecProfile::Unknown,
        width: 1920,
        height: 1080,
        frame_rate: Rational::FPS_24,
        frame_rate_proven: true,
        pixel_format: PixelFormat::Yuv420p,
        pixel_format_proven: true,
        color_range: DecodedVideoRange::Limited,
        detected_color_space: Some(ColorSpace::Rec709),
        color_interpretation: DetectedColorInterpretation {
            color_space: Some(ColorSpace::Rec709),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            evidence: Vec::new(),
            warnings: Vec::new(),
            user_overridable: true,
        },
        color_space_source: VideoColorSpaceSource::Metadata,
        color_detection_method: VideoColorDetectionMethod::CicpTags,
        color_metadata: None,
        color_metadata_hints: Vec::new(),
        hdr_metadata: Vec::new(),
        bit_depth: 8,
        has_alpha: false,
        avg_bitrate: 10_000_000,
        total_frames: Some(24),
    });
    AssetRecord {
        id: AssetId::new(),
        name: "missing".to_owned(),
        kind: AssetKind::Video,
        path,
        source: None,
        folder_id: None,
        interpretation: AssetMediaInterpretation::default(),
        media_info,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn missing_video_asset() -> AssetRecord {
    video_asset(PathBuf::from(format!(
        "definitely-missing-thumbnail-source-{}.mov",
        AssetId::new()
    )))
}

#[test]
fn raster_contract_rejects_invalid_extent_and_payload() {
    assert!(ThumbnailRasterFrame::new(
        "zero",
        0,
        1,
        ThumbnailRasterColorSpace::Srgb,
        Vec::<u8>::new(),
    )
    .is_none());
    assert!(
        ThumbnailRasterFrame::new("short", 2, 1, ThumbnailRasterColorSpace::Srgb, vec![0; 7],)
            .is_none()
    );
}

#[test]
fn boundary_resolves_shared_standard_output_intent() {
    let boundary = color_contract().output_boundary().expect("thumbnail boundary");
    let display_view = boundary.display_view.expect("standard display/view");
    assert_eq!(display_view.display, "sRGB - Display");
    assert_eq!(display_view.view, "Mondrian Standard SDR v2");
}

#[test]
fn non_video_assets_never_enter_thumbnail_admission() {
    let service = AssetThumbnailService::new();
    service.set_color_context(Some(color_context()));
    let mut asset = missing_video_asset();
    asset.kind = AssetKind::SolidColor;

    assert!(matches!(
        service.thumbnail_for_asset(&asset),
        ThumbnailLookupState::Unavailable
    ));
    assert_eq!(service.diagnostics().pending_requests, 0);
}

#[test]
fn color_contract_rejections_and_range_authority_remain_distinct() {
    let mut asset = missing_video_asset();
    let context = color_context();

    asset.interpretation.payload = mondrian_core::timeline_data::AssetColorPayload::NonColorData;
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &context)
            .expect_err("non-color video is unsupported")
            .reason,
        ThumbnailFailureReason::NonColorDataUnsupported
    );

    asset.interpretation = AssetMediaInterpretation::default();
    asset.media_info.video_streams[0].detected_color_space = None;
    let mut rejecting_context = context.clone();
    rejecting_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &rejecting_context)
            .expect_err("missing metadata must be rejected")
            .reason,
        ThumbnailFailureReason::InputColorRejected
    );

    asset.media_info.video_streams[0].detected_color_space = Some(ColorSpace::Rec709);
    asset.media_info.video_streams[0].color_range = DecodedVideoRange::Unknown;
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &context)
            .expect("unknown probe range remains frame-resolvable")
            .source_range,
        DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Unknown }
    );

    asset.interpretation.range = mondrian_core::timeline_data::MediaRangeInterpretation::Override {
        range: mondrian_core::timeline_data::MediaSignalRange::Full,
    };
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &context)
            .expect("authoritative range override")
            .source_range,
        DecodedVideoRangeContract::OverrideFull
    );

    let mut internal_context = context.clone();
    internal_context.output_color_space = mondrian_core::OcioColorSpaceIdentity::Working(
        mondrian_core::WorkingColorSpace::LinearRec709,
    );
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &internal_context)
            .expect_err("internal identity cannot cross the raster boundary")
            .reason,
        ThumbnailFailureReason::InternalOutputIdentity
    );

    asset.media_info.video_streams.clear();
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &context)
            .expect_err("missing primary stream contract")
            .reason,
        ThumbnailFailureReason::MissingVideoStreamContract
    );
}

#[test]
fn unsupported_encoded_output_is_rejected_before_decode() {
    let asset = missing_video_asset();
    let context = SequenceSettings::default().root_preview_color_context(
        &mondrian_core::ProjectColorManagement::default(),
        ColorSpace::DisplayP3,
    );
    let failure = ThumbnailColorContract::resolve(&asset, &context)
        .expect_err("Widget raster currently supports only sRGB");
    assert_eq!(
        failure.reason,
        ThumbnailFailureReason::UnsupportedRasterOutput
    );
}

#[test]
fn explicit_srgb_boundary_changes_pixels_for_distinct_source_transfers() {
    let rec709 = color_contract();
    let mut pq = rec709.clone();
    pq.source_color_space = ColorSpace::Rec2100Pq;
    let source = vec![128, 96, 64, 255];

    let rec709_output =
        color_manage_rgba(1, 1, source.clone(), &rec709).expect("Rec.709 transform");
    let pq_output = color_manage_rgba(1, 1, source, &pq).expect("PQ transform");
    assert_eq!(rec709_output.len(), 4);
    assert_eq!(pq_output.len(), 4);
    assert_ne!(rec709_output, pq_output);
}

#[test]
fn raster_resource_identity_includes_extent_source_revision_and_color_contract() {
    let asset_id = AssetId::new();
    let request = key(asset_id, 42);
    let job = ThumbnailJob {
        key: request,
        generation: 1,
        cancellation: ExecutionCancellationToken::new(),
    };
    let base = thumbnail_key(&job, 320, 180);
    assert!(base.starts_with(&format!("asset-thumb:{asset_id}:320x180:len42:mtime42-42")));
    let mut changed_color = job.key.clone();
    changed_color.color.source_color_space = ColorSpace::Rec2100Pq;
    let changed_job = ThumbnailJob {
        key: changed_color,
        generation: 1,
        cancellation: ExecutionCancellationToken::new(),
    };
    assert_ne!(base, thumbnail_key(&changed_job, 320, 180));
}

#[test]
fn missing_source_failure_is_deduplicated_and_terminal() {
    let service = AssetThumbnailService::new();
    service.set_color_context(Some(color_context()));
    let asset = missing_video_asset();
    assert!(matches!(
        service.thumbnail_for_asset(&asset),
        ThumbnailLookupState::Failed(ThumbnailFailure {
            reason: ThumbnailFailureReason::MissingSourceFile,
            ..
        })
    ));
    let _ = service.thumbnail_for_asset(&asset);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.failures, 1);
    assert_eq!(diagnostics.retained_failures, 1);
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.terminal_records.len(), 1);
}

#[test]
fn disconnected_worker_fails_without_leaving_pending_state() {
    let service = isolated_service();
    service.set_color_context(Some(color_context()));
    let path = std::env::temp_dir().join(format!(
        "mondrian-disconnected-thumbnail-worker-{}.mov",
        AssetId::new()
    ));
    std::fs::write(&path, b"fixture").expect("write thumbnail fixture");
    let asset = video_asset(path.clone());

    assert!(matches!(
        service.thumbnail_for_asset(&asset),
        ThumbnailLookupState::Failed(ThumbnailFailure {
            reason: ThumbnailFailureReason::WorkerUnavailable,
            ..
        })
    ));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.retained_failures, 1);
    assert_eq!(
        diagnostics.failures_by_reason.get(&ThumbnailFailureReason::WorkerUnavailable),
        Some(&1)
    );
    std::fs::remove_file(path).expect("remove thumbnail fixture");
}

#[test]
fn stale_completion_cannot_publish_or_consume_current_request() {
    let service = isolated_service();
    let request = key(AssetId::new(), 1);
    {
        let mut state = service.state.lock();
        state.generation = 2;
    }
    let changed = service.publish(ThumbnailResult {
        key: request,
        generation: 1,
        result: Ok(frame(1)),
        elapsed: Duration::from_millis(1),
    });
    assert!(!changed);
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.superseded, 1);
    assert_eq!(diagnostics.cached_entries, 0);
}

#[test]
fn stale_decode_cancellation_retains_cancellation_evidence() {
    let service = isolated_service();
    let request = key(AssetId::new(), 2);
    let changed = service.publish(ThumbnailResult {
        key: request,
        generation: 1,
        result: Err(ThumbnailFailure::new(
            ThumbnailFailureReason::DecodeCanceled,
            "test cancellation",
        )),
        elapsed: Duration::from_millis(1),
    });
    assert!(!changed);
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.cancellations, 1);
    assert_eq!(diagnostics.superseded, 0);
    assert_eq!(diagnostics.failures, 0);
    assert_eq!(
        diagnostics.terminal_records[0].evidence.disposition,
        ExecutionTerminalDisposition::Canceled
    );
}

#[test]
fn cache_and_terminal_evidence_are_hard_bounded() {
    let service = isolated_service();
    for seed in 1..=THUMBNAIL_CACHE_ENTRY_CAPACITY as u64 + 1 {
        let request = key(AssetId::new(), seed);
        mark_pending(&service, &request, 1);
        assert!(service.publish(ThumbnailResult {
            key: request,
            generation: 1,
            result: Ok(frame(seed)),
            elapsed: Duration::ZERO,
        }));
    }
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.cached_entries, THUMBNAIL_CACHE_ENTRY_CAPACITY);
    assert_eq!(diagnostics.cached_bytes, THUMBNAIL_CACHE_ENTRY_CAPACITY * 4);
    assert_eq!(diagnostics.evictions, 1);
    assert_eq!(
        diagnostics.terminal_records.len(),
        THUMBNAIL_TERMINAL_CAPACITY
    );
}

#[test]
fn replacing_one_resident_asset_does_not_evict_an_unrelated_asset() {
    let service = isolated_service();
    let replaced_asset = AssetId::new();
    for seed in 1..=THUMBNAIL_CACHE_ENTRY_CAPACITY as u64 {
        let asset_id = if seed == 1 {
            replaced_asset
        } else {
            AssetId::new()
        };
        let request = key(asset_id, seed);
        mark_pending(&service, &request, 1);
        assert!(service.publish(ThumbnailResult {
            key: request,
            generation: 1,
            result: Ok(frame(seed)),
            elapsed: Duration::ZERO,
        }));
    }
    let replacement = key(replaced_asset, 999);
    mark_pending(&service, &replacement, 1);
    assert!(service.publish(ThumbnailResult {
        key: replacement,
        generation: 1,
        result: Ok(frame(999)),
        elapsed: Duration::ZERO,
    }));

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.cached_entries, THUMBNAIL_CACHE_ENTRY_CAPACITY);
    assert_eq!(diagnostics.evictions, 0);
}

#[test]
fn color_generation_rotation_retires_deferred_work() {
    let service = isolated_service();
    let request = key(AssetId::new(), 7);
    let cancellation = ExecutionCancellationToken::new();
    {
        let mut state = service.state.lock();
        state.pending.insert(
            request.clone(),
            PendingThumbnail { generation: 1, cancellation: cancellation.clone() },
        );
        state
            .deferred
            .push_back(ThumbnailJob { key: request, generation: 1, cancellation });
    }
    service.set_color_context(Some(color_context()));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.deferred_requests, 0);
    assert_eq!(diagnostics.cancellations, 1);
    assert_eq!(
        diagnostics.terminal_records[0].evidence.disposition,
        ExecutionTerminalDisposition::Canceled
    );
}

#[test]
fn completion_poll_obeys_count_and_time_budgets() {
    let (service, results) = service_with_result_transport();
    let first = key(AssetId::new(), 10);
    let second = key(AssetId::new(), 11);
    mark_pending(&service, &first, 1);
    mark_pending(&service, &second, 1);
    results
        .send(ThumbnailResult {
            key: first,
            generation: 1,
            result: Ok(frame(10)),
            elapsed: Duration::ZERO,
        })
        .expect("first result transport");
    results
        .send(ThumbnailResult {
            key: second,
            generation: 1,
            result: Ok(frame(11)),
            elapsed: Duration::ZERO,
        })
        .expect("second result transport");

    assert!(service.poll_finished_with_budget(8, Duration::ZERO));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.completions, 1);
    assert_eq!(diagnostics.pending_requests, 1);

    assert!(service.poll_finished_with_budget(1, Duration::from_secs(1)));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.completions, 2);
    assert_eq!(diagnostics.pending_requests, 0);
}

#[test]
fn completion_poll_keeps_pumping_after_a_canceled_result_exhausts_time_budget() {
    let (service, results) = service_with_result_transport();
    let canceled = key(AssetId::new(), 12);
    let ready = key(AssetId::new(), 13);
    mark_pending(&service, &canceled, 1);
    mark_pending(&service, &ready, 1);
    results
        .send(ThumbnailResult {
            key: canceled,
            generation: 1,
            result: Err(ThumbnailFailure::new(
                ThumbnailFailureReason::DecodeCanceled,
                "test cancellation",
            )),
            elapsed: Duration::ZERO,
        })
        .expect("canceled result transport");
    results
        .send(ThumbnailResult {
            key: ready,
            generation: 1,
            result: Ok(frame(13)),
            elapsed: Duration::ZERO,
        })
        .expect("ready result transport");

    assert!(service.poll_finished_with_budget(8, Duration::ZERO));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.cancellations, 1);
    assert_eq!(diagnostics.pending_requests, 1);
}

#[test]
fn failed_completion_requests_refresh_and_retains_structured_failure() {
    let (service, results) = service_with_result_transport();
    let request = key(AssetId::new(), 14);
    let asset_id = request.asset_id;
    mark_pending(&service, &request, 1);
    results
        .send(ThumbnailResult {
            key: request,
            generation: 1,
            result: Err(ThumbnailFailure::new(
                ThumbnailFailureReason::OutputTransformFailed,
                "test transform failure",
            )),
            elapsed: Duration::ZERO,
        })
        .expect("failed result transport");

    assert!(service.poll_finished_with_budget(1, Duration::from_secs(1)));
    let state = service.state.lock();
    assert!(!state.pending.keys().any(|key| key.asset_id == asset_id));
    assert_eq!(
        state.failures.get(&asset_id).map(|entry| entry.failure.reason),
        Some(ThumbnailFailureReason::OutputTransformFailed)
    );
}

#[test]
fn failure_codes_are_stable_and_distinct() {
    assert_eq!(
        ThumbnailFailureReason::AdmissionRejected.code(),
        "admission_rejected"
    );
    assert_ne!(
        ThumbnailFailureReason::DecodeCanceled.code(),
        ThumbnailFailureReason::DecodeFailed.code()
    );
}
