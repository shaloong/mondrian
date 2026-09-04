use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::app::single_worker_activity::SingleWorkerActivity;
use mondrian_assets::{AssetKind, AssetLibrary, AssetMediaProbeCandidate, AssetRecord};
use mondrian_core::{AssetId, ColorEngine, ColorSpace, OutputTransformIntent, Rational};
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{
    DecodedVideoRange, DecodedVideoRangeContract, DetectedColorInterpretation, MediaInfo,
    VideoColorDetectionMethod, VideoColorInterpretationConfidence,
    VideoColorInterpretationEvidence, VideoColorMetadataHintScope, VideoColorSpaceSource,
    VideoStreamInfo,
};
use mondrian_timeline::sequence::{MissingColorMetadataPolicy, SequenceSettings};

use super::analysis::{color_manage_rgba, thumbnail_key, ThumbnailColorContract};
use super::*;

#[test]
fn thumbnail_shutdown_retains_real_worker_and_revokes_shared_admission() {
    let service = AssetThumbnailService::new();
    let observer = Arc::clone(&service);
    service.set_color_context(Some(color_context()));
    let receipt: ThumbnailShutdownEvidence = service
        .shutdown_until(Instant::now() + Duration::from_secs(5))
        .expect("sole closer");
    assert!(receipt.all_resources_released(), "{receipt:?}");
    observer.set_color_context(Some(color_context()));
    observer.set_resource_policy(true, true, THUMBNAIL_CACHE_BYTE_BUDGET);
    assert!(matches!(
        observer.admit(key(AssetId::new(), 990), 1),
        ThumbnailLookupState::Unavailable
    ));
    assert!(!observer.poll_finished());
    assert!(observer.state.lock().color_context.is_none());
    assert_eq!(Ok(receipt), observer.shutdown_until(Instant::now()));
}

#[test]
fn thumbnail_prepared_shutdown_is_not_successful_production_start() {
    let service = AssetThumbnailService::prepare();
    let receipt = service.shutdown_until(Instant::now()).expect("sole closer");
    assert!(receipt.all_created_resources_released());
    assert!(!receipt.all_resources_released());
    service.start_in_place();
    assert_eq!(Ok(receipt), service.shutdown_until(Instant::now()));
    assert!(!service.lifecycle.lock().started);
}

#[test]
fn thumbnail_shutdown_unblocks_full_result_transport_and_cancels_deferred_work() {
    let service = AssetThumbnailService::new();
    service.set_color_context(Some(color_context()));
    let generation = service.diagnostics().generation;
    // Pre-canceled actual jobs exercise the production worker and bounded sender,
    // without requiring a media fixture or entering native decode.
    for seed in 0..(THUMBNAIL_JOB_QUEUE_CAPACITY + 2) {
        let request = key(AssetId::new(), seed as u64 + 1200);
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();
        service.state.lock().pending.insert(
            request.clone(),
            PendingThumbnail { generation, cancellation: cancellation.clone() },
        );
        let mut job = ThumbnailJob { key: request, generation, cancellation };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match service.send_job(job) {
                ThumbnailDispatchOutcome::Sent => break,
                ThumbnailDispatchOutcome::Full(returned) => {
                    job = returned;
                }
                ThumbnailDispatchOutcome::Closed(_) => panic!("actual worker disconnected"),
            }
            assert!(
                Instant::now() < deadline,
                "worker did not drain bounded jobs"
            );
            std::thread::yield_now();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while service.diagnostics().awaiting_publication < THUMBNAIL_JOB_QUEUE_CAPACITY + 2 {
        assert!(
            Instant::now() < deadline,
            "actual result queue did not saturate"
        );
        std::thread::yield_now();
    }
    service.set_resource_policy(false, false, THUMBNAIL_CACHE_BYTE_BUDGET);
    assert!(matches!(
        service.admit(key(AssetId::new(), 1400), generation),
        ThumbnailLookupState::Loading
    ));
    assert_eq!(service.diagnostics().deferred_requests, 1);
    let receipt = service
        .shutdown_until(Instant::now() + Duration::from_secs(5))
        .expect("sole closer");
    assert!(receipt.all_resources_released(), "{receipt:?}");
    assert_eq!(service.diagnostics().running_requests, 0);
    assert!(
        service.worker_activity.snapshot().awaiting_publication.is_empty(),
        "joined thumbnail shutdown retained queued publication identities"
    );
}

#[test]
fn thumbnail_timeout_and_opaque_panic_receipts_cannot_be_repaired() {
    use crate::app::owned_worker_lifecycle::OwnedWorkerShutdown;
    let service = AssetThumbnailService::prepare();
    let (release, blocked) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    {
        let mut lifecycle = service.lifecycle.lock();
        lifecycle.attempted = true;
        lifecycle.started = true;
        lifecycle.worker = Some(std::thread::spawn(move || {
            let _ = blocked.recv_timeout(Duration::from_secs(5));
            let _ = finished.send(());
        }));
    }
    let receipt = service.shutdown_until(Instant::now()).expect("sole closer");
    let _ = release.send(());
    finish.recv_timeout(Duration::from_secs(5)).expect("late worker returned");
    assert_eq!(receipt.worker, OwnedWorkerShutdown::TimedOutDetached);
    assert!(!receipt.all_resources_released());
    assert_eq!(
        Ok(receipt),
        service.shutdown_until(Instant::now() + Duration::from_secs(5))
    );

    struct HostilePayload;
    impl Drop for HostilePayload {
        fn drop(&mut self) {
            panic!("must not run opaque destructor");
        }
    }
    let service = AssetThumbnailService::prepare();
    {
        let mut lifecycle = service.lifecycle.lock();
        lifecycle.attempted = true;
        lifecycle.started = true;
        lifecycle.worker = Some(std::thread::spawn(|| std::panic::panic_any(HostilePayload)));
    }
    let receipt = service
        .shutdown_until(Instant::now() + Duration::from_secs(5))
        .expect("sole closer");
    assert_eq!(
        receipt.worker,
        OwnedWorkerShutdown::PanickedPayloadAbandoned
    );
    assert!(!receipt.all_resources_released());
}

#[test]
fn thumbnail_shared_poll_and_admission_cannot_reopen_closed_owner() {
    let service = AssetThumbnailService::new();
    service.set_color_context(Some(color_context()));
    service.set_resource_policy(false, false, THUMBNAIL_CACHE_BYTE_BUDGET);
    let generation = service.diagnostics().generation;
    let gate = Arc::new(std::sync::Barrier::new(3));
    let closed = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for index in 0..2 {
        let service = Arc::clone(&service);
        let gate = Arc::clone(&gate);
        let closed = Arc::clone(&closed);
        threads.push(std::thread::spawn(move || {
            gate.wait();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !closed.load(Ordering::Acquire) && Instant::now() < deadline {
                if index == 0 {
                    let _ = service.admit(key(AssetId::new(), 1600), generation);
                } else {
                    service.poll_finished();
                }
                std::thread::yield_now();
            }
            service.set_color_context(Some(color_context()));
            service.set_resource_policy(true, true, THUMBNAIL_CACHE_BYTE_BUDGET);
            matches!(
                service.admit(key(AssetId::new(), 1601), generation),
                ThumbnailLookupState::Unavailable
            ) && !service.poll_finished()
        }));
    }
    gate.wait();
    service.begin_shutdown();
    closed.store(true, Ordering::Release);
    let observed = threads.into_iter().map(|thread| thread.join()).collect::<Vec<_>>();
    let receipt = service
        .shutdown_until(Instant::now() + Duration::from_secs(5))
        .expect("sole closer");
    assert!(receipt.all_resources_released(), "{receipt:?}");
    for result in observed {
        assert!(result.expect("shared caller returned"));
    }
    assert_eq!(receipt.awaiting_publication_remaining, 0);
    assert_eq!(receipt.active_requests_remaining, 0);
    assert_eq!(Ok(receipt), service.shutdown_until(Instant::now()));
}

#[test]
fn thumbnail_concurrent_closer_is_rejected_without_waiting_for_other_deadline() {
    let service = AssetThumbnailService::prepare();
    let (release, blocked) = mpsc::channel();
    {
        let mut lifecycle = service.lifecycle.lock();
        lifecycle.attempted = true;
        lifecycle.started = true;
        lifecycle.worker = Some(std::thread::spawn(move || {
            let _ = blocked.recv_timeout(Duration::from_secs(10));
        }));
    }
    let owner = Arc::clone(&service);
    let closer =
        std::thread::spawn(move || owner.shutdown_until(Instant::now() + Duration::from_secs(10)));
    let observation_deadline = Instant::now() + Duration::from_secs(5);
    let observed = loop {
        if service.dispatch_gate.shutdown.load(Ordering::Acquire)
            && service.lifecycle.try_lock().is_none()
        {
            break true;
        }
        if Instant::now() >= observation_deadline {
            break false;
        }
        std::thread::yield_now();
    };
    let started = Instant::now();
    let competing = service.shutdown_until(started);
    let elapsed = started.elapsed();
    let _ = release.send(());
    let receipt = closer.join().expect("first closer returned").expect("sole lifecycle owner");
    assert!(observed, "first closer did not acquire native ownership");
    assert_eq!(competing, Err(ThumbnailShutdownUnavailable::LifecycleBusy));
    assert!(
        elapsed < Duration::from_millis(500),
        "competing closer waited: {elapsed:?}"
    );
    assert!(receipt.all_resources_released(), "{receipt:?}");
    assert_eq!(Ok(receipt), service.shutdown_until(Instant::now()));
}

fn color_context() -> ProgramColorContext {
    let mut settings = SequenceSettings::default();
    settings.color.program_output.color_space = ColorSpace::Srgb;
    settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid thumbnail context")
}

fn color_contract() -> ThumbnailColorContract {
    ThumbnailColorContract {
        video_stream_index: 0,
        source_color_space: ColorSpace::Rec709,
        source_range: DecodedVideoRangeContract::Automatic {
            probed_range: DecodedVideoRange::Limited,
        },
        camera_raw: None,
        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        missing_metadata_policy: MissingColorMetadataPolicy::AssumeRec709,
        output_color_space: ColorSpace::Srgb,
        tone_map: true,
        engine: ColorEngine::mondrian_standard(),
        output_transform: OutputTransformIntent::mondrian_standard(),
    }
}

fn fingerprint(seed: u64) -> MediaFileFingerprint {
    MediaFileFingerprint {
        len: Some(seed),
        modified_secs: Some(seed),
        modified_nanos: Some(seed as u32),
        object_identity: Some(mondrian_core::MediaFileObjectIdentity::Unix {
            device: 1,
            inode: seed,
        }),
        change_stamp: Some(mondrian_core::MediaFileChangeStamp::Unix {
            seconds: seed as i64,
            nanoseconds: i64::from(seed as u32),
        }),
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
        jobs: Mutex::new(Some(job_tx)),
        results: Mutex::new(Some(result_rx)),
        lifecycle: Mutex::new(lifecycle::ThumbnailLifecycle::default()),
        dispatch_gate: ThumbnailDispatchGate::new(),
        worker_activity: Arc::new(SingleWorkerActivity::default()),
    }
}

fn service_with_result_transport() -> (AssetThumbnailService, mpsc::SyncSender<ThumbnailResult>) {
    let (job_tx, _job_rx) = mpsc::sync_channel(1);
    let (result_tx, result_rx) = mpsc::sync_channel(4);
    (
        AssetThumbnailService {
            state: Mutex::new(ThumbnailState::default()),
            jobs: Mutex::new(Some(job_tx)),
            results: Mutex::new(Some(result_rx)),
            lifecycle: Mutex::new(lifecycle::ThumbnailLifecycle::default()),
            dispatch_gate: ThumbnailDispatchGate::new(),
            worker_activity: Arc::new(SingleWorkerActivity::default()),
        },
        result_tx,
    )
}

fn service_with_job_transport() -> (AssetThumbnailService, mpsc::Receiver<ThumbnailJob>) {
    let (job_tx, job_rx) = mpsc::sync_channel(2);
    let (_result_tx, result_rx) = mpsc::sync_channel(1);
    (
        AssetThumbnailService {
            state: Mutex::new(ThumbnailState::default()),
            jobs: Mutex::new(Some(job_tx)),
            results: Mutex::new(Some(result_rx)),
            lifecycle: Mutex::new(lifecycle::ThumbnailLifecycle::default()),
            dispatch_gate: ThumbnailDispatchGate::new(),
            worker_activity: Arc::new(SingleWorkerActivity::default()),
        },
        job_rx,
    )
}

fn mark_pending(service: &AssetThumbnailService, key: &ThumbnailRequestKey, generation: u64) {
    let cancellation = ExecutionCancellationToken::new();
    let mut state = service.state.lock();
    state.pending.insert(key.clone(), PendingThumbnail { generation, cancellation });
    state.active.insert(key.asset_id, key.clone());
}

fn video_media_info(file_size: u64) -> MediaInfo {
    let video = VideoStreamInfo {
        index: 0,
        codec: VideoCodec::H264,
        duration: Some(Duration::from_secs(1)),
        codec_profile: mondrian_media::VideoCodecProfile::Unknown,
        width: 1920,
        height: 1080,
        picture: Default::default(),
        frame_rate: Rational::FPS_24,
        frame_rate_proven: true,
        pixel_format: PixelFormat::Yuv420p,
        pixel_format_proven: true,
        color_range: DecodedVideoRange::Limited,
        color_interpretation: DetectedColorInterpretation {
            candidate_color_space: Some(ColorSpace::Rec709),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: vec![VideoColorInterpretationEvidence::MetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "source_color_space".to_owned(),
                value: "Rec709".to_owned(),
                detected_color_space: ColorSpace::Rec709,
                authority: mondrian_media::VideoColorMetadataHintAuthority::SourceDeclaration(
                    mondrian_media::VideoColorMetadataDeclaration::SourceColorSpace,
                ),
            }],
            warnings: Vec::new(),
            user_overridable: true,
        },
        color_metadata: None,
        color_metadata_hints: Vec::new(),
        hdr_metadata: Vec::new(),
        camera_raw: None,
        bit_depth: 8,
        has_alpha: false,
        avg_bitrate: 10_000_000,
        total_frames: Some(24),
    };
    MediaInfo {
        duration: Duration::from_secs(1),
        file_size,
        container: "test".to_owned(),
        video_streams: vec![video],
        audio_streams: Vec::new(),
        has_video: true,
        has_audio: false,
    }
}

fn video_asset_with_probe(path: PathBuf, configure: impl FnOnce(&mut MediaInfo)) -> AssetRecord {
    let path = path.canonicalize().expect("canonical thumbnail fixture");
    let fingerprint = MediaFileFingerprint::capture(&path);
    let mut media_info = video_media_info(fingerprint.len.expect("fixture file size"));
    configure(&mut media_info);
    let library_root = path
        .parent()
        .expect("thumbnail fixture parent")
        .join(format!("thumbnail-asset-library-{}", AssetId::new()));
    let library = AssetLibrary::open(library_root.clone()).expect("fixture Asset Library");
    let candidate =
        AssetMediaProbeCandidate::new(path, fingerprint, media_info).expect("media candidate");
    let asset_id = library.commit_media_probe(candidate, None).expect("register Asset");
    let asset = library.get_asset(asset_id).expect("read fixture Asset").expect("fixture Asset");
    drop(library);
    let _ = std::fs::remove_dir_all(library_root);
    asset
}

fn video_asset(path: PathBuf) -> AssetRecord {
    video_asset_with_probe(path, |_| {})
}

fn missing_video_asset_with_probe(configure: impl FnOnce(&mut MediaInfo)) -> AssetRecord {
    let root = std::env::temp_dir().join(format!(
        "mondrian-missing-thumbnail-source-{}",
        AssetId::new()
    ));
    std::fs::create_dir_all(&root).expect("create thumbnail fixture root");
    let path = root.join("missing.mov");
    std::fs::write(&path, b"fixture").expect("write thumbnail fixture");
    let asset = video_asset_with_probe(path, configure);
    std::fs::remove_dir_all(root).expect("retire thumbnail fixture");
    asset
}

fn missing_video_asset() -> AssetRecord {
    missing_video_asset_with_probe(|_| {})
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
    let display_view = boundary.ocio_display_view().expect("standard display/view");
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
fn resource_policy_pauses_deferred_dispatch_trims_cache_and_resumes() {
    let (service, jobs) = service_with_job_transport();
    let asset_id = AssetId::new();
    let request_key = key(asset_id, 700);
    {
        let mut state = service.state.lock();
        state.cache.insert(
            asset_id,
            ThumbnailCacheEntry { key: request_key.clone(), frame: frame(7) },
        );
        state.cache_lru.push_front(asset_id);
        state.cached_bytes = 4;
        let cancellation = ExecutionCancellationToken::new();
        state.pending.insert(
            request_key.clone(),
            PendingThumbnail { generation: 1, cancellation: cancellation.clone() },
        );
        state
            .deferred
            .push_back(ThumbnailJob { key: request_key, generation: 1, cancellation });
    }

    service.set_resource_policy(false, false, 1);
    assert!(matches!(jobs.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let paused = service.diagnostics();
    assert!(!paused.automatic_admission_enabled);
    assert!(!paused.dispatch_enabled);
    assert_eq!(paused.cached_entries, 0);

    service.set_resource_policy(true, true, THUMBNAIL_CACHE_BYTE_BUDGET);
    assert!(jobs.recv_timeout(Duration::from_millis(100)).is_ok());
    let resumed = service.diagnostics();
    assert!(resumed.automatic_admission_enabled);
    assert!(resumed.dispatch_enabled);
}

#[test]
fn diagnostics_distinguish_queued_running_and_awaiting_publication() {
    let service = isolated_service();
    let request = key(AssetId::new(), 401);
    mark_pending(&service, &request, 1);
    let identity = ThumbnailWorkerIdentity { key: request.clone(), generation: 1 };
    let mut lease = service.worker_activity.begin(identity);

    let waiting = service.diagnostics();
    assert_eq!(
        waiting.worker_phase,
        ThumbnailWorkerPhase::WaitingForDispatch
    );
    assert_eq!(waiting.queued_requests, 1);
    assert_eq!(waiting.running_requests, 0);

    lease.mark_running();
    let running = service.diagnostics();
    assert_eq!(running.worker_phase, ThumbnailWorkerPhase::Running);
    assert_eq!(running.queued_requests, 0);
    assert_eq!(running.running_requests, 1);

    lease.finish_for_publication();
    lease.commit_publication();
    let awaiting = service.diagnostics();
    assert_eq!(awaiting.worker_phase, ThumbnailWorkerPhase::Idle);
    assert_eq!(awaiting.queued_requests, 0);
    assert_eq!(awaiting.running_requests, 0);
    assert_eq!(awaiting.awaiting_publication, 1);

    assert!(service.publish(ThumbnailResult {
        key: request,
        generation: 1,
        result: Ok(frame(401)),
        elapsed: Duration::ZERO,
    }));
    assert_eq!(service.diagnostics().awaiting_publication, 0);
}

#[test]
fn generation_rotation_keeps_obsolete_physical_work_visible_without_owning_new_demand() {
    let service = isolated_service();
    let obsolete = key(AssetId::new(), 410);
    mark_pending(&service, &obsolete, 1);
    let mut obsolete_lease = service
        .worker_activity
        .begin(ThumbnailWorkerIdentity { key: obsolete, generation: 1 });
    obsolete_lease.mark_running();

    service.set_color_context(Some(color_context()));
    let current = key(AssetId::new(), 411);
    mark_pending(&service, &current, 2);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.generation, 2);
    assert_eq!(diagnostics.running_requests, 1);
    assert_eq!(diagnostics.queued_requests, 1);
    assert_eq!(diagnostics.awaiting_publication, 0);
}

#[test]
fn dispatch_gate_holds_already_transported_work_until_resume() {
    let service = AssetThumbnailService::new();
    let request_key = key(AssetId::new(), 701);
    mark_pending(&service, &request_key, 1);
    service.set_resource_policy(false, false, THUMBNAIL_CACHE_BYTE_BUDGET);
    assert!(
        matches!(
            service.send_job(ThumbnailJob {
                key: request_key,
                generation: 1,
                cancellation: ExecutionCancellationToken::new(),
            }),
            ThumbnailDispatchOutcome::Sent
        ),
        "transport one paused thumbnail"
    );

    std::thread::sleep(Duration::from_millis(20));
    assert!(matches!(
        service.results.lock().as_ref().expect("open result transport").try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    service.set_resource_policy(true, true, THUMBNAIL_CACHE_BYTE_BUDGET);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        service.poll_finished();
        if service.diagnostics().pending_requests == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "thumbnail gate did not resume transported work"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
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

    let asset = missing_video_asset_with_probe(|probe| {
        let interpretation = &mut probe.video_streams[0].color_interpretation;
        interpretation.candidate_color_space = None;
        interpretation.confidence = VideoColorInterpretationConfidence::None;
        interpretation.source = VideoColorSpaceSource::MissingMetadata;
        interpretation.method = VideoColorDetectionMethod::MissingMetadata;
        interpretation.evidence.clear();
    });
    let mut rejecting_settings = SequenceSettings::default();
    rejecting_settings.color.program_output.color_space = ColorSpace::Srgb;
    rejecting_settings.color.input.missing_metadata_policy =
        MissingColorMetadataPolicy::RejectMedia;
    let rejecting_context = rejecting_settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid rejecting thumbnail context");
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &rejecting_context)
            .expect_err("missing metadata must be rejected")
            .reason,
        ThumbnailFailureReason::InputColorRejected
    );

    let mut asset = missing_video_asset_with_probe(|probe| {
        probe.video_streams[0].color_range = DecodedVideoRange::Unknown;
    });
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

    let internal_context = SequenceSettings::default()
        .nested_render_color_context(
            &context,
            mondrian_core::timeline_data::NestedColorProcessing::ForceParentWorkingSpace,
        )
        .expect("valid nested working context");
    assert_eq!(
        ThumbnailColorContract::resolve(&asset, &internal_context)
            .expect_err("internal identity cannot cross the raster boundary")
            .reason,
        ThumbnailFailureReason::InternalOutputIdentity
    );

    let asset = missing_video_asset_with_probe(|probe| {
        probe.video_streams.clear();
    });
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
    let mut settings = SequenceSettings::default();
    settings.color.program_output.color_space = ColorSpace::DisplayP3;
    let context = settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid Display P3 context");
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
    let base = thumbnail_key(&job, 320, 180).expect("base raster identity");
    assert!(base.starts_with("asset-thumb:"));
    assert_eq!(base.len(), "asset-thumb:".len() + 64);
    let mut changed_color = job.key.clone();
    changed_color.color.source_color_space = ColorSpace::Rec2100Pq;
    let changed_job = ThumbnailJob {
        key: changed_color,
        generation: 1,
        cancellation: ExecutionCancellationToken::new(),
    };
    assert_ne!(
        base,
        thumbnail_key(&changed_job, 320, 180).expect("changed color identity")
    );

    let mut changed_revision = job.key.clone();
    changed_revision.fingerprint.object_identity =
        Some(mondrian_core::MediaFileObjectIdentity::Unix { device: 1, inode: 9_999 });
    let changed_revision_job = ThumbnailJob {
        key: changed_revision,
        generation: 1,
        cancellation: ExecutionCancellationToken::new(),
    };
    assert_ne!(
        base,
        thumbnail_key(&changed_revision_job, 320, 180).expect("changed source-revision identity")
    );
    assert_ne!(
        base,
        thumbnail_key(&job, 160, 90).expect("changed extent identity")
    );
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
