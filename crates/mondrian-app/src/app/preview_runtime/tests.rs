use super::request_scheduler::MediaPreviewRequestAdmission;
use super::*;
use crate::app::preview_access_mode::MediaPreviewRequestIntent;
use crate::app::preview_execution::{
    PreviewGpuFrameStaging, PreviewOutputKey, PreviewSemanticIdentity,
    PreviewSemanticIdentityBuilder, PREVIEW_GPU_CPU_STAGING_CAPACITY,
};
use crate::app::preview_frame_store::MediaWorkReservationAdmission;
use crate::app::preview_raster_frame::{
    preview_raster_resource_key, PreviewRasterColorSpace, PreviewRasterFrame,
};
use crate::app::preview_timeline_execution::PreviewTimelineResolution;
use crate::app::preview_unavailability::PreviewUnavailabilityDisposition;
use crate::app::preview_viewer_plan::{
    gpu_composite_layers_for_resolved_with_session, viewer_preview_plan_allows_cross_call_reuse,
    ResolvedPreviewTransitionInput,
};
use crate::app::AppState;
use crate::app_ui::panels::{ViewerPreviewSource, ViewerPreviewState};
use crate::app_ui::playback_feedback::ViewerPlaybackFeedback;
use crate::app_ui::preview::WindowPreviewAdapter;
use mondrian_ui_widgets::{
    ViewerExternalTextureFrame, ViewerExternalTexturePresentation, ViewerFrameContent,
    ViewerFrameImage,
};

fn viewer_gpu_source_layer(layer: &ViewerGpuExecutionLayer) -> Option<&ViewerGpuSourceLayer> {
    match layer {
        ViewerGpuExecutionLayer::Source(source) => Some(source.as_ref()),
        ViewerGpuExecutionLayer::Adjustment { .. } | ViewerGpuExecutionLayer::CrossDissolve(_) => {
            None
        }
    }
}

fn viewer_gpu_transition_source(input: &ViewerGpuTransitionInput) -> Option<&ViewerGpuSourceLayer> {
    match input {
        ViewerGpuTransitionInput::Transparent => None,
        ViewerGpuTransitionInput::Source(source) => Some(source.as_ref()),
    }
}

fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
    let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
    mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
}

fn register_test_window_preview_output(
    service: &WindowPreviewAdapter,
    frame: &PreviewGpuFrame,
    texture_key: impl Into<String>,
    presentation: ViewerExternalTexturePresentation,
) -> bool {
    let Some(output) = ViewerExternalTextureFrame::new_spatial(texture_key, presentation) else {
        service.reject_gpu_output_registration();
        return false;
    };
    service.register_gpu_output(frame.output_key.clone(), output);
    service.try_release_settled_transport_media_residency();
    true
}

#[test]
fn visual_result_drain_limit_requires_follow_up_without_a_new_notification() {
    assert!(!visual_execution_drain_needs_follow_up(
        MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL - 1
    ));
    assert!(visual_execution_drain_needs_follow_up(
        MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL
    ));
}

fn evaluation_resolve_count<O: Clone>(runtime: &PreviewProductionRuntime<O>) -> u64 {
    runtime.metrics.timeline_resolve_count.get()
}

#[test]
fn gpu_then_presentation_share_one_evaluation_for_the_same_frame() {
    let state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    execute_gpu_preview_for_test_app(&runtime, &state);
    let _ = execute_preview_presentation_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        1,
        "GPU production followed by presentation arbitration must resolve one frame evaluation exactly once"
    );
}

#[test]
fn presentation_then_gpu_share_one_evaluation_for_the_same_frame() {
    let state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    let _ = execute_preview_presentation_for_test_app(&runtime, &state);
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        1,
        "presentation arbitration followed by GPU production must resolve one frame evaluation exactly once"
    );
}

#[test]
fn repeated_acquires_for_the_same_evaluation_do_not_re_resolve() {
    let state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    for _ in 0..4 {
        execute_gpu_preview_for_test_app(&runtime, &state);
    }
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        1,
        "repeated acquires for the same picture must hit the evaluation working set"
    );
}

#[test]
fn asset_arrival_re_evaluates_the_next_headless_style_attempt() {
    // Headless drivers attempt the same frame repeatedly. Without a
    // dependency change those attempts must not re-resolve; once an asset
    // arrives (invalidation), the next attempt re-evaluates exactly once
    // and the following attempts deduplicate again.
    let state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    for _ in 0..3 {
        execute_gpu_preview_for_test_app(&runtime, &state);
    }
    assert_eq!(evaluation_resolve_count(&runtime) - before, 1);

    runtime.invalidate_evaluations_for_asset(AssetId::new());
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        2,
        "the first attempt after asset arrival must re-evaluate"
    );

    for _ in 0..3 {
        execute_gpu_preview_for_test_app(&runtime, &state);
    }
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        2,
        "subsequent attempts must deduplicate against the fresh evaluation"
    );
}

#[test]
fn generation_rollover_reuses_semantically_valid_evaluations() {
    // Playing is scheduling state, never frame content: a play/pause
    // transition rotates the preview generation but the evaluation key
    // stays identical, so the working set serves the same evaluation and
    // produced-candidate authority stays generation-scoped.
    let mut state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    // The host profile is an independent picture input: conservative Linux
    // runners intentionally lower realtime Preview to Half. Pin a Standard
    // profile so play/pause changes only scheduling state in this test.
    pin_standard_execution_resources(&mut state);
    // `play` from Stopped intentionally restarts at frame zero. Establish a
    // paused transport at the fixture's exact frame so this test varies only
    // scheduling generation, never semantic frame content.
    state.play().expect("start transport fixture");
    state.pause().expect("pause transport fixture");
    state.seek(4).expect("restore exact fixture frame");
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(evaluation_resolve_count(&runtime) - before, 1);

    state.play().expect("play from the paused fixture frame");
    assert_eq!(state.current_frame(), 4);
    assert_eq!(
        state.playback_preview_resolution_scale(),
        mondrian_playback::PreviewResolutionScale::Full
    );
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        1,
        "generation rollover on play must reuse the semantically valid evaluation"
    );

    state.pause().expect("pause on the same fixture frame");
    assert_eq!(state.current_frame(), 4);
    assert_eq!(
        state.playback_preview_resolution_scale(),
        mondrian_playback::PreviewResolutionScale::Full
    );
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        1,
        "generation rollover on pause must reuse the semantically valid evaluation"
    );
}

#[test]
fn viewer_gpu_failure_executes_bounded_cpu_fallback_off_thread() {
    let mut state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    runtime.request_viewer_cpu_fallback("test GPU record failure");

    assert!(matches!(
        execute_gpu_preview_for_test_app(&runtime, &state),
        PreviewGpuFrameState::Loading
    ));

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let poll = runtime.pump_cpu_fallback_results();
        if poll.visible_change {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "CPU fallback worker did not complete"
        );
        std::thread::yield_now();
    }

    assert!(matches!(
        execute_preview_presentation_for_test_app(&runtime, &state),
        PreviewPresentationState::Ready(_)
    ));
    runtime.clear_viewer_cpu_fallback();
    assert!(!runtime.viewer_cpu_fallback_active.get());

    let _ = state.pause();
}

#[test]
fn cpu_fallback_rejects_non_default_display_policy_instead_of_showing_srgb() {
    let mut state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    state.set_viewer_display_management(
        mondrian_core::DisplayManagementPolicy::default()
            .with_monitor_output(mondrian_core::MonitorOutputIntent::ColorSpace(
                ColorSpace::DisplayP3,
            ))
            .expect("Display P3 monitor target"),
    );
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    runtime.request_viewer_cpu_fallback("test GPU record failure");

    let PreviewGpuFrameState::Unavailable(unavailable) =
        execute_gpu_preview_for_test_app(&runtime, &state)
    else {
        panic!("non-default display policy must not use the fixed sRGB CPU atlas");
    };
    assert_eq!(unavailable.stage(), PreviewOutputStage::DisplayContract);
    assert!(unavailable.detail().contains("cpu_viewer_display_policy_carrier"));
}

#[test]
fn resource_scale_change_re_resolves_across_generation_rollover() {
    let mut state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    state.execution_resources =
        crate::app::execution_resource_coordination::ExecutionResourceCoordinator::new(
            crate::app::execution_resource_coordination::MachineResourceProfile::from_capacity(
                None, 1,
            ),
        );
    state.play().expect("start conservative transport fixture");
    state.pause().expect("pause conservative transport fixture");
    state.seek(4).expect("restore exact conservative fixture frame");
    assert_eq!(
        state.playback_preview_resolution_scale(),
        mondrian_playback::PreviewResolutionScale::Full
    );

    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(evaluation_resolve_count(&runtime) - before, 1);

    state.play().expect("play on a conservative machine profile");
    assert_eq!(state.current_frame(), 4);
    assert_eq!(
        state.playback_preview_resolution_scale(),
        mondrian_playback::PreviewResolutionScale::Half,
        "conservative realtime policy must lower the semantic Preview scale"
    );
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        2,
        "a runtime-scale picture change must not reuse the Full evaluation"
    );
}

#[test]
fn sequence_revision_change_forces_re_resolution() {
    let mut state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let before = evaluation_resolve_count(&runtime);
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(evaluation_resolve_count(&runtime) - before, 1);

    let mut sequence = state.active_sequence().expect("active Sequence").clone();
    sequence.revision = sequence.revision.checked_next().expect("test revision can advance");
    state.test_set_sequence(Some(sequence));
    execute_gpu_preview_for_test_app(&runtime, &state);
    assert_eq!(
        evaluation_resolve_count(&runtime) - before,
        2,
        "a sequence revision change is part of the evaluation key and must re-resolve"
    );
}

#[test]
fn evaluation_working_set_dedupes_wait_entries_and_invalidates_by_asset() {
    let mut set = EvaluationWorkingSet::new();
    let sequence = Sequence::new("working-set");
    let key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: 0,
        frame: 4,
        width: 640,
        height: 360,
        runtime_scale: mondrian_playback::PreviewResolutionScale::Full,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    let asset = AssetId::new();

    assert!(set.waiting_for(key).is_none());
    set.insert_waiting(key, Arc::from([EvaluationDependency::MediaProducer(asset)]));
    assert!(
        set.waiting_for(key).is_some(),
        "a pending evaluation must be retained as a typed wait entry"
    );

    // An unrelated asset arrival must not invalidate this wait entry.
    set.invalidate_for_asset(AssetId::new());
    assert!(set.waiting_for(key).is_some());

    // The exact asset arrival removes the wait entry.
    set.invalidate_for_asset(asset);
    assert!(set.waiting_for(key).is_none());

    // A ready evaluation is retained and cleared by the same invalidation.
    let ready = Arc::new(ResolvedFrameEvaluation {
        key,
        output_key: PreviewOutputKey::new(
            key.sequence_id,
            key.width,
            key.height,
            test_preview_semantic_identity(1),
        ),
        elements: Arc::from([]),
        color_context: test_color_context(ColorSpace::Srgb),
        resolved_quality: ResolvedFrameQuality::Full,
        reuse_policy: EvaluationReusePolicy::Reusable,
        dependencies: Arc::from([]),
        render_cache_identity: None,
    });
    set.insert(key, Arc::clone(&ready), 1);
    assert!(set.get(key, 2).is_some());
    set.invalidate_for_asset(asset);
    assert!(set.get(key, 3).is_none());
}

#[test]
fn retained_media_producer_wait_reasserts_pending_each_presentation_turn() {
    let (state, _asset_id, root) = state_with_invalid_video_asset();
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();

    assert!(matches!(
        execute_gpu_preview_for_test_app(&runtime, &state),
        PreviewGpuFrameState::Loading
    ));
    assert!(runtime.execution.borrow().is_pending());

    assert!(matches!(
        execute_gpu_preview_for_test_app(&runtime, &state),
        PreviewGpuFrameState::Loading
    ));
    assert!(
        runtime.execution.borrow().is_pending(),
        "a retained producer wait must restore the per-turn pending level"
    );
    assert_eq!(runtime.diagnostics().unavailability.failed, 0);

    runtime.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn evaluation_working_set_retires_native_decoder_resource_owners() {
    let mut set = EvaluationWorkingSet::new();
    let sequence = Sequence::new("native-residency");
    let key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: 0,
        frame: 0,
        width: 320,
        height: 180,
        runtime_scale: mondrian_playback::PreviewResolutionScale::Full,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    let native = MediaPreviewFrame::from_native(
        test_native_source_frame(320, 180),
        Resolution { width: 320, height: 180 },
        Resolution { width: 320, height: 180 },
        test_preview_semantic_identity(2),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::default(),
    );
    let evaluation = Arc::new(ResolvedFrameEvaluation {
        key,
        output_key: PreviewOutputKey::new(
            key.sequence_id,
            key.width,
            key.height,
            test_preview_semantic_identity(3),
        ),
        elements: Arc::from([ResolvedPreviewElement::Media {
            frame: native,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: mondrian_effects::identity_compiled_effect_graph()
                .expect("identity graph"),
            prepared_heterogeneous_route: None,
            frame_seed: 0,
        }]),
        color_context: test_color_context(ColorSpace::Srgb),
        resolved_quality: ResolvedFrameQuality::Full,
        reuse_policy: EvaluationReusePolicy::Reusable,
        dependencies: Arc::from([]),
        render_cache_identity: None,
    });
    set.insert(key, evaluation, 1);
    assert!(set.get(key, 2).is_some());

    set.clear_decoder_resource_entries();

    assert!(
        set.get(key, 3).is_none(),
        "decoder-family retirement must not leave native surfaces pinned by evaluation reuse"
    );
}

fn test_preview_semantic_identity(revision: u64) -> PreviewSemanticIdentity {
    let mut builder =
        PreviewSemanticIdentityBuilder::new(b"mondrian.preview.test-frame-identity.v1");
    std::hash::Hasher::write_u64(&mut builder, revision);
    builder.finish_identity()
}

fn test_media_file_fingerprint(len: u64, revision: u64) -> MediaFileFingerprint {
    MediaFileFingerprint {
        len: Some(len),
        modified_secs: Some(revision),
        modified_nanos: Some(20),
        object_identity: Some(mondrian_core::MediaFileObjectIdentity::Unix {
            device: 1,
            inode: revision,
        }),
        change_stamp: Some(mondrian_core::MediaFileChangeStamp::Unix {
            seconds: revision as i64,
            nanoseconds: 20,
        }),
    }
}

fn cancellation_evidence(
    work_class: mondrian_playback::FrameWorkClass,
    cause: mondrian_playback::FrameCancellationCause,
    execution_us: u64,
    execution_to_logical_cancellation_us: Option<u64>,
    request_to_logical_cancellation_us: Option<u64>,
) -> mondrian_playback::FrameCancellationEvidenceReport {
    let mut collector = mondrian_playback::FrameCancellationEvidenceCollector::default();
    collector.observe(mondrian_playback::FrameCancellationObservation {
        work_class,
        cause,
        execution_duration: Duration::from_micros(execution_us),
        execution_to_logical_cancellation: execution_to_logical_cancellation_us
            .map(Duration::from_micros),
        request_to_logical_cancellation: request_to_logical_cancellation_us
            .map(Duration::from_micros),
    });
    collector.report()
}

#[test]
fn runtime_diagnostics_exposes_each_bounded_worker_progress_observer() {
    let runtime = PreviewProductionRuntime::<()>::with_direct_worker_count_for_test(
        preview_decode_cpu_budget(),
        2,
    );
    let watch = runtime.decode_execution_watch();
    let diagnostics = runtime.diagnostics();

    assert_eq!(diagnostics.decode_worker_count, 2);
    assert!(diagnostics.decode_worker_execution.any.is_none());
    assert_eq!(
        diagnostics
            .decode_worker_execution
            .playback
            .expect("playback worker progress")
            .stage,
        mondrian_media::PreviewDecodeExecutionStage::Idle
    );
    assert_eq!(
        diagnostics
            .decode_worker_execution
            .non_playback
            .expect("non-playback worker progress")
            .stage,
        mondrian_media::PreviewDecodeExecutionStage::Idle
    );
    assert_eq!(watch.snapshot(), diagnostics.decode_worker_execution);

    drop(runtime);
    let retired_runtime_snapshot = watch.snapshot();
    assert_eq!(
        retired_runtime_snapshot
            .playback
            .expect("retired playback observer remains readable")
            .stage,
        mondrian_media::PreviewDecodeExecutionStage::Idle
    );
    assert_eq!(
        retired_runtime_snapshot
            .non_playback
            .expect("retired non-playback observer remains readable")
            .stage,
        mondrian_media::PreviewDecodeExecutionStage::Idle
    );
}

#[test]
fn runtime_applies_one_resource_policy_to_the_shared_decode_worker_family_owner() {
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let shared_owner = runtime.decode_worker_resources.clone();
    let coordinator =
        crate::app::execution_resource_coordination::ExecutionResourceCoordinator::new(
            crate::app::execution_resource_coordination::MachineResourceProfile::from_capacity(
                Some(16 * 1024 * 1024 * 1024),
                8,
            ),
        );
    let decision = coordinator.decision();

    runtime.apply_resource_decision(&decision.preview);
    runtime.apply_resource_decision(&decision.preview);

    assert_eq!(
        shared_owner.seek_index_cache().diagnostics().policy,
        decision.preview.seek_index_cache
    );
    assert_eq!(
        shared_owner.hardware_device_context_pool().diagnostics().policy,
        decision.preview.hardware_device_contexts
    );
    assert_eq!(
        shared_owner.session_residency_config().max_interactive_sessions_per_worker(),
        decision.preview.frame_store.current_media_working_set_resource_unit_limit
    );
    assert_eq!(
        runtime.diagnostics().resource_decision_applications,
        1,
        "an equal immutable policy must not reconfigure Preview twice"
    );
}

use mondrian_assets::AssetLibrary;
use mondrian_core::types::{AssetId, Rational};
use mondrian_core::{ensure_mondrian_default_ocio_loaded, Color};
use mondrian_effects::EffectNodeExt;
use mondrian_effects::{compile_reference_effect_graph, EffectCachePolicy, EffectRenderPlan};
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{
    DetectedColorInterpretation, HwAccelPixelFormat, MediaInfo, VideoColorDetectionMethod,
    VideoColorInterpretationConfidence, VideoColorSpaceSource, VideoStreamInfo,
};
use mondrian_renderer::{
    color::RenderColorStageGpuBlockerBreakdown, ColorFrameDomain, ViewerGpuSourceLayer,
    ViewerGpuTransitionInput,
};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::{MissingColorMetadataPolicy, Sequence};
use mondrian_timeline::track::Track;

fn custom_u8_effect_graph(
    label: &str,
    cache_policy: EffectCachePolicy,
    processor: mondrian_effects::CustomEffectRenderProcessor,
) -> Arc<mondrian_effects::CompiledEffectGraph> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DEFINITION: AtomicU64 = AtomicU64::new(1);
    let suffix = NEXT_DEFINITION.fetch_add(1, Ordering::Relaxed);
    let effect_type =
        mondrian_effects::EffectType::Plugin(format!("test.preview.custom.{label}.{suffix}"));
    let determinism = match cache_policy {
        EffectCachePolicy::Deterministic => mondrian_effects::EffectDeterminism::Deterministic,
        EffectCachePolicy::FrameDependent => mondrian_effects::EffectDeterminism::FrameSeeded,
        EffectCachePolicy::Uncacheable => mondrian_effects::EffectDeterminism::Nondeterministic,
    };
    mondrian_effects::register_effect_definition(
        mondrian_effects::EffectDefinition::new(
            effect_type.key(),
            "Custom preview test",
            Default::default(),
            mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(mondrian_effects::EffectExecutionContract {
            execution_modes: mondrian_effects::EffectExecutionModes::CPU_U8,
            determinism,
            state_model: mondrian_effects::EffectStateModel::Stateless,
            temporal_input: mondrian_effects::EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: mondrian_effects::EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: mondrian_effects::EffectResourceLifetime::Frame,
            topology: mondrian_effects::EffectGraphTopology::LinearChain,
        })
        .with_custom_render_backend(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            cache_policy,
            processor,
        ),
    )
    .expect("register custom preview test definition");
    mondrian_effects::PreparedEffectProgram::prepare(
        &[mondrian_effects::EffectNode::new(effect_type)],
        &[],
        mondrian_core::WorkingColorSpace::LinearRec709,
    )
    .expect("prepare custom preview test program")
    .evaluate(mondrian_core::TimelineTime::ZERO)
    .expect("evaluate custom preview test graph")
}

fn ensure_test_ocio_loaded() {
    ensure_mondrian_default_ocio_loaded().expect("preview tests require Mondrian default OCIO");
}

fn state_with_solid_color_clip(color: Color) -> AppState {
    ensure_test_ocio_loaded();
    let mut state = AppState::new();
    let mut sequence = Sequence::new("preview");
    let tb = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(AssetId::new(), color, tt(0, tb), tt(24, tb))
                .expect("valid clip"),
        )
        .expect("solid clip should be insertable");
    state.test_set_sequence(Some(sequence));
    state.seek(4).expect("seek");
    state
}

fn execute_gpu_preview_for_test_app<O: Clone>(
    runtime: &PreviewProductionRuntime<O>,
    state: &AppState,
) -> PreviewGpuFrameState {
    runtime.gpu_preview_frame(state.preview_frame_execution_request(Instant::now()))
}

fn execute_preview_presentation_for_test_app<O: Clone>(
    runtime: &PreviewProductionRuntime<O>,
    state: &AppState,
) -> PreviewPresentationState<O> {
    runtime.presentation(state.preview_frame_execution_request(Instant::now()))
}

#[test]
fn resolution_scale_change_invalidates_still_frame_generation() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let sequence_id = state.active_sequence_id().expect("active sequence");

    let set_scale = |state: &mut AppState, scale: f32| {
        state
            .commit_sequence_edit(sequence_id, "修改预览分辨率", |sequence| {
                let mut settings = sequence.settings.clone();
                settings.preview.resolution_scale = scale;
                sequence.apply_settings(settings)
            })
            .expect("commit resolution scale edit");
    };

    set_scale(&mut state, 1.0);
    let full = execute_gpu_preview_for_test_app(&service, &state);
    let full_frame = match full {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected full-resolution still frame"),
    };
    assert_eq!((full_frame.width, full_frame.height), (1920, 1080));

    set_scale(&mut state, 0.5);
    let half = execute_gpu_preview_for_test_app(&service, &state);
    let half_frame = match half {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected re-evaluated still frame after resolution change"),
    };
    assert_eq!((half_frame.width, half_frame.height), (960, 540));
}

fn playback_presentation_ticket_for_state<O: Clone>(
    runtime: &PreviewProductionRuntime<O>,
    state: &AppState,
) -> Option<mondrian_playback::FramePresentationTicket> {
    let snapshot = state.preview_execution_snapshot(Instant::now());
    runtime.playback_presentation_ticket(&snapshot)
}

fn synchronize_visual_program_for_state<O: Clone>(
    runtime: &PreviewProductionRuntime<O>,
    state: &AppState,
) {
    let snapshot = state.preview_execution_snapshot(Instant::now());
    runtime.synchronize_visual_program_authoring_session(&snapshot);
}

fn playback_video_preroll_for_state<O: Clone>(
    runtime: &PreviewProductionRuntime<O>,
    state: &AppState,
) -> Option<PreviewVideoPreroll> {
    let snapshot = state.preview_execution_snapshot(Instant::now());
    runtime.playback_video_preroll_readiness(&snapshot, state)
}

fn schedule_media_prefetches_for_state<O: Clone>(
    runtime: &PreviewProductionRuntime<O>,
    state: &AppState,
    sequence: &Sequence,
    frame: i64,
) {
    let snapshot = state.preview_execution_snapshot(Instant::now());
    // Production planning lowers geometry from the snapshot's runtime scale;
    // tests must feed the same scale-aware route so planned keys can match
    // key constructions built from the same snapshot.
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    runtime.schedule_media_prefetches(&snapshot, state, sequence, frame, width, height);
}

#[allow(clippy::too_many_arguments)]
fn viewer_preview_generation_key_for_state(
    state: &AppState,
    sequence: &Sequence,
    frame: i64,
    width: u32,
    height: u32,
    display_color_space: ColorSpace,
    display_contract_identity: Option<DisplayOutputIdentity>,
) -> ViewerPreviewGenerationKey {
    let snapshot = state.preview_execution_snapshot(Instant::now());
    ViewerPreviewGenerationKey::from_snapshot(
        &snapshot,
        sequence,
        frame,
        width,
        height,
        display_color_space,
        display_contract_identity,
    )
}

#[test]
fn visual_program_cache_does_not_cross_equal_author_state_between_open_sessions() {
    fn resolve_author_lifetime(
        runtime: &PreviewProductionRuntime<()>,
        state: &AppState,
    ) -> crate::app::preview_timeline_execution::ResolvedPreviewTimeline {
        let mut last_failure = "no resolution attempt completed".to_owned();
        for _ in 0..32 {
            let sequence = state.active_sequence().expect("active Sequence");
            let snapshot = state.preview_execution_snapshot(Instant::now());
            match runtime.resolve_timeline_for_test(
                &snapshot,
                state,
                sequence,
                state.current_frame(),
                64,
                36,
                sequence
                    .settings
                    .root_program_color_context(state.project_color_environment())
                    .expect("valid test context"),
            ) {
                PreviewTimelineResolution::Ready(resolved) => return resolved,
                PreviewTimelineResolution::Empty => {
                    last_failure = "unexpected empty plan".to_owned();
                }
                PreviewTimelineResolution::Pending { .. } => {
                    last_failure = "unexpected pending dependency".to_owned();
                }
                PreviewTimelineResolution::Unavailable { reason } => {
                    last_failure = reason.to_string();
                }
            }
        }
        panic!("visual program preparation did not stabilize: {last_failure}");
    }

    let first_state = state_with_solid_color_clip(Color::BLACK);
    let mut second_sequence = first_state.active_sequence().expect("first Sequence").clone();
    let content = &mut second_sequence.video_tracks[0].clips[0].content;
    let asset_id = match content {
        mondrian_core::timeline_data::ClipContent::SolidColor { asset_id, .. } => *asset_id,
        _ => panic!("test fixture must remain a Solid Color Clip"),
    };
    *content =
        mondrian_core::timeline_data::ClipContent::SolidColor { asset_id, color: Color::WHITE };

    let mut second_state = AppState::new();
    second_state.test_set_sequence(Some(second_sequence));
    second_state.seek(first_state.current_frame()).expect("seek");

    assert_ne!(
        first_state.authoring_session_id(),
        second_state.authoring_session_id(),
        "separate opens require distinct process-local Authoring Sessions"
    );
    assert_eq!(
        first_state.active_sequence().expect("first Sequence").id,
        second_state.active_sequence().expect("second Sequence").id
    );
    assert_eq!(
        first_state.active_sequence().expect("first Sequence").revision,
        second_state.active_sequence().expect("second Sequence").revision
    );

    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let first_resolution = resolve_author_lifetime(&runtime, &first_state);
    let ResolvedPreviewElement::SolidColor(first_solid) = &first_resolution.plan.elements[0] else {
        panic!("first author plan must remain a Solid Color");
    };
    assert_eq!(first_solid.color, Color::BLACK);
    let first_diagnostics = runtime.visual_programs.borrow().diagnostics();

    let second_resolution = resolve_author_lifetime(&runtime, &second_state);
    let ResolvedPreviewElement::SolidColor(second_solid) = &second_resolution.plan.elements[0]
    else {
        panic!("second author plan must remain a Solid Color");
    };
    assert_eq!(
        second_solid.color,
        Color::WHITE,
        "equal durable IDs and revisions must not reuse the prior Open lifetime"
    );

    let second_diagnostics = runtime.visual_programs.borrow().diagnostics();
    assert_eq!(
        second_diagnostics.scope_rotations,
        first_diagnostics.scope_rotations.saturating_add(1)
    );
    assert_eq!(
        second_diagnostics.misses,
        first_diagnostics.misses.saturating_add(1),
        "the second Authoring Session must prepare a new program"
    );
    assert_eq!(
        runtime.visual_program_authoring_session.get(),
        second_state.authoring_session_id()
    );
    runtime.shutdown();
}

#[test]
fn playback_generation_survives_frame_advance_but_not_discontinuity() {
    let mut state = state_with_solid_color_clip(Color::from_rgba8(12, 34, 56, 255));
    let sequence = state.active_sequence().expect("sequence").clone();
    state.play().expect("play");

    let current = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        4,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    let advanced = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        5,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    assert_eq!(
        current, advanced,
        "ordinary playback must retain forward prefetch work"
    );

    let presentation_rotated = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        5,
        960,
        540,
        ColorSpace::Srgb,
        Some(managed_icc_display_snapshot(ColorSpace::Srgb).contract_identity()),
    );
    assert_ne!(current, presentation_rotated);
    assert_eq!(
        presentation_rotated.transition_from(&current),
        ViewerPreviewGenerationTransition::ViewerOnly,
        "presentation-only rotation must retain the running decoder session"
    );

    let spatially_rotated = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        5,
        640,
        360,
        ColorSpace::Srgb,
        Some(managed_icc_display_snapshot(ColorSpace::Srgb).contract_identity()),
    );
    assert_ne!(current, spatially_rotated);
    assert_eq!(
        spatially_rotated.transition_from(&current),
        ViewerPreviewGenerationTransition::Representation,
        "representation rotation must retain only the running decoder session"
    );

    state.seek(6).expect("seek");
    let after_seek = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        6,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    assert_ne!(
        current, after_seek,
        "seek must invalidate the prior playback epoch"
    );
    assert_eq!(
        after_seek.transition_from(&current),
        ViewerPreviewGenerationTransition::Semantic
    );

    state.pause().expect("pause");
    let idle_a = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        6,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    let idle_b = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        7,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    assert_ne!(
        idle_a, idle_b,
        "idle current-frame work remains latest-wins"
    );

    let mut revised_sequence = sequence.clone();
    revised_sequence.revision =
        revised_sequence.revision.checked_next().expect("test revision can advance");
    let revised = viewer_preview_generation_key_for_state(
        &state,
        &revised_sequence,
        6,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    assert_ne!(
        idle_a, revised,
        "Sequence authoring must rotate Preview work"
    );
    state.test_advance_project_generation();
    let project_revised = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        6,
        960,
        540,
        ColorSpace::Srgb,
        None,
    );
    assert_ne!(
        idle_a, project_revised,
        "Project authoring must rotate Preview work"
    );

    let display_revised = viewer_preview_generation_key_for_state(
        &state,
        &sequence,
        6,
        960,
        540,
        ColorSpace::Srgb,
        Some(managed_icc_display_snapshot(ColorSpace::Srgb).contract_identity()),
    );
    assert_ne!(
        project_revised, display_revised,
        "Display contract changes must rotate Preview work"
    );
}

#[test]
fn app_frame_store_exposes_independent_byte_and_decoder_resource_budgets() {
    let diagnostics = PreviewFrameStoreAdapter::default().diagnostics();
    assert!(diagnostics.media_byte_budget > 0);
    assert!(diagnostics.media_resource_unit_budget > 0);
    assert!(
        diagnostics.media_resource_unit_budget < MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
        "the default store budget must not be inflated to match a fixed prefetch horizon"
    );
}

#[test]
fn authoring_session_rotation_clears_decoded_media_residency() {
    let first_state = state_with_solid_color_clip(Color::BLACK);
    let second_state = state_with_solid_color_clip(Color::WHITE);
    assert_ne!(
        first_state.authoring_session_id(),
        second_state.authoring_session_id()
    );

    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    synchronize_visual_program_for_state(&runtime, &first_state);
    assert!(admit_test_media_frame(
        &mut runtime.frame_store.borrow_mut(),
        test_media_key(1),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert_eq!(runtime.frame_store.borrow().diagnostics().media_entries, 1);

    synchronize_visual_program_for_state(&runtime, &second_state);
    let diagnostics = runtime.frame_store.borrow().diagnostics();
    assert_eq!(diagnostics.media_entries, 0);
    assert_eq!(diagnostics.media_reserved_bytes, 0);
    runtime.shutdown();
}

fn unique_preview_test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ))
}

fn rec709_video_media_info(file_size: u64) -> MediaInfo {
    MediaInfo {
        duration: Duration::from_secs(2),
        file_size,
        container: "mp4".to_owned(),
        video_streams: vec![VideoStreamInfo {
            index: 0,
            codec: VideoCodec::H265,
            duration: Some(Duration::from_secs(2)),
            codec_profile: mondrian_media::VideoCodecProfile::HevcMain10,
            width: 3840,
            height: 2160,
            picture: Default::default(),
            frame_rate: Rational::new(25, 1),
            frame_rate_proven: true,
            pixel_format: PixelFormat::Yuv420p10le,
            pixel_format_proven: true,
            color_range: DecodedVideoRange::Limited,
            color_interpretation: DetectedColorInterpretation {
                candidate_color_space: Some(ColorSpace::Rec709),
                confidence: VideoColorInterpretationConfidence::High,
                source: VideoColorSpaceSource::Metadata,
                method: VideoColorDetectionMethod::MetadataHint,
                evidence: vec![
                    mondrian_media::VideoColorInterpretationEvidence::MetadataHint {
                        scope: mondrian_media::VideoColorMetadataHintScope::Stream,
                        key: "source_color_space".to_owned(),
                        value: "Rec709".to_owned(),
                        detected_color_space: ColorSpace::Rec709,
                        authority:
                            mondrian_media::VideoColorMetadataHintAuthority::SourceDeclaration(
                                mondrian_media::VideoColorMetadataDeclaration::SourceColorSpace,
                            ),
                    },
                ],
                warnings: Vec::new(),
                user_overridable: true,
            },
            color_metadata: None,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            camera_raw: None,
            bit_depth: 10,
            has_alpha: false,
            avg_bitrate: 20_000_000,
            total_frames: Some(50),
        }],
        audio_streams: Vec::new(),
        has_video: true,
        has_audio: false,
    }
}

fn commit_preview_test_media(
    library: &AssetLibrary,
    media_path: PathBuf,
    media_info: MediaInfo,
) -> AssetId {
    let media_path =
        std::fs::canonicalize(media_path).expect("canonical Preview test media fixture");
    let fingerprint = mondrian_media::MediaFileFingerprint::capture(&media_path);
    let candidate =
        mondrian_assets::AssetMediaProbeCandidate::new(media_path, fingerprint, media_info)
            .expect("valid Preview test media candidate");
    library.commit_media_probe(candidate, None).expect("insert video asset")
}

fn pin_standard_execution_resources(state: &mut AppState) {
    state.execution_resources =
        crate::app::execution_resource_coordination::ExecutionResourceCoordinator::new(
            crate::app::execution_resource_coordination::MachineResourceProfile::from_capacity(
                Some(16 * 1024 * 1024 * 1024),
                8,
            ),
        );
}

fn state_with_invalid_video_asset() -> (AppState, AssetId, PathBuf) {
    ensure_test_ocio_loaded();
    let root = unique_preview_test_root("mondrian-preview-invalid-video");
    let media_path = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&media_path, b"not a real video").expect("invalid media");
    let file_size = std::fs::metadata(&media_path).expect("media metadata").len();
    let library = AssetLibrary::open(root.join("library")).expect("asset library");
    let asset_id =
        commit_preview_test_media(&library, media_path, rec709_video_media_info(file_size));

    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state.test_set_asset_library(Some(library));
    let mut sequence = Sequence::new("media");
    let tb = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(Clip::new(asset_id, tt(0, tb), tt(50, tb)).expect("valid clip"))
        .expect("media clip should be insertable");
    state.test_set_sequence(Some(sequence));
    state.seek(0).expect("seek");
    (state, asset_id, root)
}

fn state_with_two_invalid_video_assets() -> (AppState, PathBuf) {
    ensure_test_ocio_loaded();
    let root = unique_preview_test_root("mondrian-preview-two-invalid-videos");
    std::fs::create_dir_all(&root).expect("test root");
    let library = AssetLibrary::open(root.join("library")).expect("asset library");
    let mut asset_ids = Vec::new();
    for name in ["bottom.mp4", "top.mp4"] {
        let media_path = root.join(name);
        std::fs::write(&media_path, b"not a real video").expect("invalid media");
        let file_size = std::fs::metadata(&media_path).expect("media metadata").len();
        let asset_id =
            commit_preview_test_media(&library, media_path, rec709_video_media_info(file_size));
        asset_ids.push(asset_id);
    }

    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state.test_set_asset_library(Some(library));
    let mut sequence = Sequence::new("multi-track media");
    let tb = sequence.time_base();
    for (track_index, asset_id) in asset_ids.into_iter().enumerate() {
        sequence.video_tracks[track_index]
            .add_clip(Clip::new(asset_id, tt(0, tb), tt(50, tb)).expect("valid clip"))
            .expect("media clip should be insertable");
    }
    state.test_set_sequence(Some(sequence));
    state.seek(0).expect("seek");
    (state, root)
}

#[test]
fn invalid_media_fixtures_pin_full_realtime_preview_quality() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play deterministic media fixture");
    assert_eq!(
        state.playback_preview_resolution_scale(),
        mondrian_playback::PreviewResolutionScale::Full,
        "fixed residency assertions must not inherit the host machine profile"
    );
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

fn state_with_icc_display_policy(color: Color) -> AppState {
    let mut state = state_with_solid_color_clip(color);
    *state.test_viewer_display_management_mut() = mondrian_core::DisplayManagementPolicy::default()
        .with_calibration(mondrian_core::DisplayCalibrationPolicy::OsDefault)
        .expect("OS default ICC policy")
        .with_viewer_mode(mondrian_core::ViewerDisplayMode::Sdr);
    state
}

fn managed_icc_display_snapshot(color_space: ColorSpace) -> DisplayOutputSnapshot {
    let mut snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
    snapshot.monitor_profile_status = MonitorProfileStatus::ManagedColorSpace {
        color_space,
        source: mondrian_core::display_contract::MonitorProfileSource::OsIccProfile,
    };
    snapshot.resolved_output_color_space = format!("{color_space:?}");
    snapshot
}

fn calibrated_icc_display_snapshot(color_space: ColorSpace) -> DisplayOutputSnapshot {
    let mut snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
    snapshot.monitor_profile_status = MonitorProfileStatus::ManagedIccCalibration {
        source_color_space: color_space,
        profile_fingerprint: mondrian_core::display_calibration::IccProfileFingerprint::from_bytes(
            b"test-monitor-profile",
        ),
    };
    snapshot.resolved_output_color_space = format!("{color_space:?}");
    snapshot
}

fn test_color_context(output_color_space: ColorSpace) -> ProgramColorContext {
    ensure_test_ocio_loaded();
    let mut sequence = Sequence::new("color-context");
    sequence.settings.color.program_output.color_space = output_color_space;
    sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid test context")
}

fn test_color_context_with_engine(
    output_color_space: ColorSpace,
    engine: ColorEngine,
) -> ProgramColorContext {
    ensure_test_ocio_loaded();
    let mut sequence = Sequence::new("engine-color-context");
    sequence.settings.color.program_output.color_space = output_color_space;
    sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::new(engine))
        .expect("valid engine color context")
}

fn test_colorimetric_context(output_color_space: ColorSpace) -> ProgramColorContext {
    ensure_test_ocio_loaded();
    let mut sequence = Sequence::new("colorimetric-context");
    sequence.settings.color.program_output.color_space = output_color_space;
    sequence.settings.color.program_output.tone_map_policy =
        mondrian_core::DisplayToneMapPolicy::Never;
    sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid colorimetric context")
}

fn test_color_context_in_working(
    output_color_space: ColorSpace,
    working_color_space: WorkingColorSpace,
) -> ProgramColorContext {
    ensure_test_ocio_loaded();
    let mut sequence = Sequence::new("working-color-context");
    sequence.settings.color.program_output.color_space = output_color_space;
    sequence.settings.color.working_color_space = working_color_space;
    sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid working color context")
}

#[test]
fn preview_raster_presentation_contract_encodes_sdr_video_for_srgb_atlas() {
    let requested = test_color_context(ColorSpace::Rec709);

    let contract = preview_raster_presentation_contract(&requested)
        .expect("Rec.709 viewer output has an sRGB raster presentation contract");

    assert_eq!(requested.output_color_space(), ColorSpace::Rec709.into());
    assert_eq!(contract.color_space, PreviewRasterColorSpace::Srgb);
}

#[test]
fn preview_raster_presentation_contract_adapts_wide_gamut_sdr_and_rejects_hdr() {
    let p3 = test_color_context(ColorSpace::DisplayP3);
    assert!(preview_raster_presentation_contract(&p3).is_ok());

    for output in [ColorSpace::Rec2100Pq, ColorSpace::Rec2100Hlg] {
        let requested = test_color_context(output);
        let error = preview_raster_presentation_contract(&requested)
            .expect_err("HDR to sRGB raster requires an explicit rendering policy");
        assert!(error.to_string().contains("dynamic-range class"));
    }
}

#[test]
fn cpu_raster_preview_retains_program_output_before_srgb_adaptation() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let resolved = [ResolvedPreviewElement::SolidColor(
        TimelineSolidColorLayer {
            color: Color::from_rgba8(48, 96, 192, 255),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        },
    )];
    let color_context = test_color_context(ColorSpace::Rec709);
    let mut scratch = TimelineCompositeScratch::default();

    let output = composite_resolved_preview(2, 2, &resolved, &color_context, &mut scratch)
        .expect("CPU raster Program Output and monitor adaptation");

    assert_eq!(
        output.color_diagnostics.output.color_space,
        ColorSpace::Rec709.into()
    );
    let monitor = output.monitor_color_diagnostics.expect("sRGB adaptation diagnostics");
    assert_eq!(monitor.input.color_space, ColorSpace::Rec709.into());
    assert_eq!(monitor.output.color_space, ColorSpace::Srgb.into());
    assert_eq!(
        monitor.direction,
        RenderColorTransformDirection::Intermediate
    );
    assert!(!monitor.used_rgba8_boundary);
    assert_eq!(output.color_stage_diagnostics.cpu_output_stages, 2);
    assert_eq!(output.rgba.len(), 2 * 2 * 4);
}

#[test]
fn preview_display_color_space_resolves_managed_monitor_profile() {
    let sequence = Sequence::new("p3-preview");
    let viewer = mondrian_core::DisplayManagementPolicy::default()
        .with_monitor_output(mondrian_core::MonitorOutputIntent::ColorSpace(
            ColorSpace::DisplayP3,
        ))
        .expect("Display P3 monitor target")
        .with_viewer_mode(mondrian_core::ViewerDisplayMode::Sdr);

    assert_eq!(
        preview_display_color_space(
            &sequence,
            &mondrian_core::ColorEngine::mondrian_standard(),
            &viewer,
            None,
        )
        .expect("display color space"),
        ColorSpace::DisplayP3
    );
}

#[test]
fn preview_display_color_space_resolves_explicit_hdr_viewer_mode() {
    let sequence = Sequence::new("hdr-preview");
    let viewer = mondrian_core::DisplayManagementPolicy::default()
        .with_monitor_output(mondrian_core::MonitorOutputIntent::ColorSpace(
            ColorSpace::Rec709,
        ))
        .expect("Rec.709 monitor target")
        .with_viewer_mode(mondrian_core::ViewerDisplayMode::HdrPq);

    assert_eq!(
        preview_display_color_space(
            &sequence,
            &mondrian_core::ColorEngine::mondrian_standard(),
            &viewer,
            None,
        )
        .expect("display color space"),
        ColorSpace::Rec2100Pq
    );
}

#[test]
fn preview_display_color_space_rejects_icc_before_display_contract_resolution() {
    let sequence = Sequence::new("icc-preview");
    let viewer = mondrian_core::DisplayManagementPolicy::default()
        .with_calibration(mondrian_core::DisplayCalibrationPolicy::OsDefault)
        .expect("OS default ICC policy")
        .with_viewer_mode(mondrian_core::ViewerDisplayMode::Sdr);

    let err = preview_display_color_space(
        &sequence,
        &mondrian_core::ColorEngine::mondrian_standard(),
        &viewer,
        None,
    )
    .expect_err("ICC profile requires display contract resolution and must fail closed");

    assert!(matches!(
        err,
        crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
            ref feature,
            ..
        } if feature == "icc_preview_color_space_resolution"
    ));
}

#[test]
fn preview_display_color_space_rejects_uncalibrated_managed_icc_status() {
    let sequence = Sequence::new("icc-preview");
    let viewer = mondrian_core::DisplayManagementPolicy::default()
        .with_calibration(mondrian_core::DisplayCalibrationPolicy::OsDefault)
        .expect("OS default ICC policy")
        .with_viewer_mode(mondrian_core::ViewerDisplayMode::Sdr);
    let snapshot = managed_icc_display_snapshot(ColorSpace::DisplayP3);

    let err = preview_display_color_space(
        &sequence,
        &mondrian_core::ColorEngine::mondrian_standard(),
        &viewer,
        Some(&snapshot),
    )
    .expect_err("ICC status without a renderer calibration processor must fail closed");
    assert!(matches!(
        err,
        crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker::UnsupportedFeature {
            ref feature,
            ..
        } if feature == "icc_monitor_calibration_processor"
    ));
}

#[test]
fn preview_display_color_space_accepts_calibrated_icc_status() {
    let sequence = Sequence::new("icc-preview");
    let viewer = mondrian_core::DisplayManagementPolicy::default()
        .with_monitor_output(mondrian_core::MonitorOutputIntent::ColorSpace(
            ColorSpace::Srgb,
        ))
        .expect("sRGB monitor target")
        .with_calibration(mondrian_core::DisplayCalibrationPolicy::OsDefault)
        .expect("OS default ICC policy")
        .with_viewer_mode(mondrian_core::ViewerDisplayMode::Sdr);
    let snapshot = calibrated_icc_display_snapshot(ColorSpace::Srgb);

    assert_eq!(
        preview_display_color_space(
            &sequence,
            &mondrian_core::ColorEngine::mondrian_standard(),
            &viewer,
            Some(&snapshot),
        )
        .expect("calibrated ICC display source"),
        ColorSpace::Srgb
    );
}

#[test]
fn gpu_preview_frame_for_icc_policy_rejects_uncalibrated_monitor_profile() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_icc_display_policy(Color::from_rgba8(24, 80, 160, 255));
    let snapshot = managed_icc_display_snapshot(ColorSpace::DisplayP3);
    service.set_display_output_snapshot(Some(&snapshot));

    let PreviewGpuFrameState::Unavailable(reason) =
        execute_gpu_preview_for_test_app(&service, &state)
    else {
        panic!("uncalibrated monitor profile must block GPU Preview");
    };
    assert_eq!(
        reason.disposition(),
        PreviewUnavailabilityDisposition::Blocked
    );
    assert_eq!(reason.stage(), PreviewOutputStage::DisplayContract);
}

fn ready_frame(state: ViewerPreviewState) -> ViewerFrameImage {
    match state {
        ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame)) => frame,
        ViewerPreviewState::Ready(other) => {
            panic!("expected raster ready frame, got {other:?}")
        }
        other => panic!("expected ready frame, got {other:?}"),
    }
}

#[test]
fn solid_color_sequence_returns_preview_frame_at_preview_scale() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

    let frame = service.viewer_preview_for_state(&state);
    let frame = ready_frame(frame);

    assert_eq!(frame.width, 960);
    assert_eq!(frame.height, 540);
    assert_eq!(frame.rgba.len(), 960 * 540 * 4);
    assert!(frame.key.starts_with("preview.raster:"));
    assert!(frame.key.contains(":960x540:"));
}

#[test]
fn paused_gpu_candidate_carries_untimed_presentation_authority() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        PreviewGpuFrameState::Current(_) => panic!("expected new GPU preview candidate"),
        PreviewGpuFrameState::Prepared => panic!("expected current GPU preview candidate"),
        PreviewGpuFrameState::Transparent(_) => {
            panic!("expected rendered GPU preview candidate")
        }
        PreviewGpuFrameState::Loading => panic!("expected ready GPU preview candidate"),
        PreviewGpuFrameState::Unavailable(_) => {
            panic!("expected available GPU preview candidate")
        }
    };

    assert_eq!(frame.width, 960);
    assert_eq!(frame.height, 540);
    assert_eq!(frame.working_color_space, WorkingColorSpace::LinearRec2020);
    match &frame.working_input {
        PreviewGpuWorkingInput::GpuComposite { layers } => {
            assert_eq!(layers.len(), 1);
            assert!(matches!(
                viewer_gpu_source_layer(&layers[0]),
                Some(ViewerGpuSourceLayer::SolidColor { .. })
            ));
        }
    }
    assert!(frame.external_texture_key().starts_with("viewer.gpu:"));
    assert_eq!(frame.candidate_id(), 1);
    let ticket = frame.presentation_ticket().expect("paused current-frame presentation ticket");
    assert_eq!(ticket.deadline(), None);
    assert_eq!(ticket.identity().target_frame, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.gpu_preview_candidate_requests, 1);
    assert_eq!(diagnostics.gpu_preview_candidate_ready, 1);
    assert_eq!(diagnostics.gpu_preview_candidate_current, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_loading, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_unavailable, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_pixels, 960_u64 * 540);
}

#[test]
fn gpu_candidate_separates_program_output_from_monitor_identity() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let baseline_frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected baseline GPU preview candidate"),
    };
    *state.test_viewer_display_management_mut() = mondrian_core::DisplayManagementPolicy::default()
        .with_monitor_output(mondrian_core::MonitorOutputIntent::ColorSpace(
            ColorSpace::Srgb,
        ))
        .expect("sRGB monitor target")
        .with_calibration(mondrian_core::DisplayCalibrationPolicy::OsDefault)
        .expect("OS default ICC policy");
    let snapshot = calibrated_icc_display_snapshot(ColorSpace::Srgb);
    service.set_display_output_snapshot(Some(&snapshot));

    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };

    assert_eq!(
        frame.program_output_boundary.output_color_space(),
        ColorSpace::Rec709
    );
    assert_eq!(
        frame.monitor_adaptation.program_output_color_space(),
        ColorSpace::Rec709
    );
    assert_eq!(
        frame.monitor_adaptation.monitor_color_space(),
        ColorSpace::Srgb
    );
    assert!(frame.monitor_adaptation.requires_pass());
    assert_ne!(frame.output_key, baseline_frame.output_key);
}

#[test]
fn playing_gpu_candidate_carries_exact_presentation_ticket() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let identity = state.pending_playback_frame_demand_identity().expect("frame demand identity");

    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };

    let ticket = frame.presentation_ticket().expect("presentation ticket");
    assert_eq!(ticket.identity(), identity);
    let completion = state
        .complete_frame_presentation(ticket, Instant::now())
        .expect("exact current presentation");
    assert!(
        !completion.transport_changed(),
        "presenting the current frame must hold priming until lookahead is observed"
    );
    assert!(
        state.observe_video_preroll(0, 0),
        "procedural playback has no future media payload to preroll"
    );
}

#[test]
fn exact_staged_gpu_candidate_acquires_only_the_current_demand_ticket() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let current = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected priming current candidate"),
    };
    let current_ticket = current.presentation_ticket().expect("priming current ticket");
    state
        .complete_frame_presentation(current_ticket, Instant::now())
        .expect("present priming current");
    assert!(state.observe_video_preroll(0, 0));
    let request = state
        .preview_lookahead_execution_request(Instant::now(), 2)
        .expect("lookahead request");
    let staged_intent = request.snapshot().transport().playback_intent();
    let staged = match service.gpu_preview_frame(request) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected CPU-complete speculative candidate"),
    };
    assert!(staged.is_successor_preparation());
    assert!(staged.presentation_ticket().is_none());

    let mut tick_at = Instant::now();
    for _ in 0..4 {
        if state.current_frame() == staged_intent.frame {
            break;
        }
        tick_at += Duration::from_millis(45);
        state.advance_playback_clock_at(tick_at);
    }
    let identity = state.pending_playback_frame_demand_identity().expect("current demand");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    assert_eq!(snapshot.transport().playback_intent(), staged_intent);
    let current = service
        .bind_staged_gpu_frame_for_current(staged, &snapshot)
        .expect("exact generation and intent bind");

    assert!(!current.is_successor_preparation());
    assert_eq!(
        current.presentation_ticket().expect("fresh ticket").identity(),
        identity
    );
    assert_eq!(
        service.scheduler.diagnostics().active_playback_demand,
        Some(identity),
        "staged-current binding must synchronize demand even though it bypasses Timeline evaluation"
    );
}

#[test]
fn cpu_staging_is_bounded_and_retires_intents_outside_the_horizon() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let mut staging = PreviewGpuFrameStaging::default();
    let mut intents = Vec::new();

    for frame in 0..=PREVIEW_GPU_CPU_STAGING_CAPACITY {
        state.set_playback_frame_running(frame as i64);
        let request = state
            .preview_lookahead_execution_request(Instant::now(), 2)
            .expect("lookahead request");
        let intent = request.snapshot().transport().playback_intent();
        let candidate = match service.gpu_preview_frame(request) {
            PreviewGpuFrameState::Ready(candidate) => candidate,
            _ => panic!("expected distinct speculative candidate"),
        };
        intents.push(intent);
        staging.stage(candidate);
    }

    assert_eq!(staging.len(), PREVIEW_GPU_CPU_STAGING_CAPACITY);
    assert!(!staging.contains(intents[0]));
    assert!(staging.contains(*intents.last().expect("last intent")));
    staging.retain_only(&intents[2..4]);
    assert_eq!(staging.len(), 2);
}

#[test]
fn running_transport_without_a_pending_demand_cannot_publish_a_retry() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let identity = state.pending_playback_frame_demand_identity().expect("frame demand identity");

    state.observe_frame_delivery_candidate(
        mondrian_playback::FrameDeliveryCandidate::for_demand(
            identity,
            mondrian_playback::FrameDeliveryKind::Late,
        ),
        Instant::now(),
    );

    assert!(state.pending_playback_frame_demand_identity().is_none());
    assert!(matches!(
        execute_gpu_preview_for_test_app(&service, &state),
        PreviewGpuFrameState::Loading
    ));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.gpu_preview_candidate_ready, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_current, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_loading, 1);
}

#[test]
fn running_empty_timeline_without_a_pending_demand_is_not_republished_transparent() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
    let time_base = sequence.time_base();
    sequence.video_tracks[0].clips[0].position = tt(20, time_base);
    state.seek(0).expect("seek");
    state.play().expect("play");
    let identity = state.pending_playback_frame_demand_identity().expect("frame demand identity");
    state.observe_frame_delivery_candidate(
        mondrian_playback::FrameDeliveryCandidate::for_demand(
            identity,
            mondrian_playback::FrameDeliveryKind::Late,
        ),
        Instant::now(),
    );

    assert!(matches!(
        execute_preview_presentation_for_test_app(&service, &state),
        PreviewPresentationState::Loading
    ));
}

#[test]
fn gpu_composite_layers_accept_transformed_media_frame() {
    let media = test_media_frame_with_size(180, 320, 180, 42);
    let transform = [3.0, 0.0, 12.0, 0.0, 3.0, 18.0];
    let effect_graph = mondrian_effects::compile_reference_effect_graph(
        &mondrian_effects::EffectRenderPlan::default(),
    )
    .expect("compile identity graph");
    let elements = vec![ResolvedPreviewElement::Media {
        frame: media,
        opacity: 0.85,
        blend_mode: BlendMode::Normal,
        transform,
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 7,
    }];

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("affine transformed media should stay on GPU composite path");

    assert_eq!(layers.len(), 1);
    match viewer_gpu_source_layer(&layers[0]) {
        Some(ViewerGpuSourceLayer::Media { opacity, transform: actual_transform, .. }) => {
            assert_eq!(*opacity, 0.85);
            assert_eq!(*actual_transform, transform);
        }
        _ => panic!("expected media layer"),
    }
}

#[test]
fn gpu_viewer_lowering_reuses_the_preview_owned_effect_session() {
    let media = test_media_frame_with_size(180, 320, 180, 42);
    let effect_graph =
        mondrian_effects::compile_reference_effect_graph(&mondrian_effects::EffectRenderPlan {
            ops: vec![mondrian_effects::EffectRenderOp::Grain { amount: 0.2 }],
        })
        .expect("compile GPU effect graph");
    let elements = vec![ResolvedPreviewElement::Media {
        frame: media,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 7,
    }];
    let mut scratch = TimelineCompositeScratch::default();

    let first = gpu_composite_layers_for_resolved_with_session(
        &elements,
        WorkingColorSpace::LinearRec709,
        &mut scratch,
    )
    .expect("first lowering");
    let first_plan = match viewer_gpu_source_layer(&first[0]) {
        Some(ViewerGpuSourceLayer::Media { effect_plan, .. }) => Arc::clone(effect_plan),
        _ => panic!("expected media layer"),
    };
    let second = gpu_composite_layers_for_resolved_with_session(
        &elements,
        WorkingColorSpace::LinearRec709,
        &mut scratch,
    )
    .expect("cached lowering");
    let second_plan = match viewer_gpu_source_layer(&second[0]) {
        Some(ViewerGpuSourceLayer::Media { effect_plan, .. }) => effect_plan,
        _ => panic!("expected media layer"),
    };

    assert!(Arc::ptr_eq(&first_plan, second_plan));
    assert_eq!(scratch.effect_execution_diagnostics().gpu_plan_entries, 1);
}

#[test]
fn gpu_composite_layers_lower_cross_dissolve_as_typed_two_input_node() {
    let effect_graph = mondrian_effects::compile_reference_effect_graph(
        &mondrian_effects::EffectRenderPlan::default(),
    )
    .expect("compile identity graph");
    let elements = vec![ResolvedPreviewElement::CrossDissolve {
        left: Box::new(ResolvedPreviewTransitionInput::Media {
            frame: test_media_frame_with_size(180, 320, 180, 42),
            opacity: 0.8,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            prepared_heterogeneous_route: None,
            frame_seed: 7,
        }),
        right: Box::new(ResolvedPreviewTransitionInput::SolidColor(
            TimelineSolidColorLayer {
                color: Color { r: 0.1, g: 0.2, b: 0.3, a: 0.6 },
                opacity: 0.75,
                blend_mode: BlendMode::Normal,
                transform: [0.75, 0.0, 0.125, 0.0, 0.75, 0.125],
                effect_graph,
                frame_seed: 8,
            },
        )),
        progress: 0.25,
    }];

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("Cross Dissolve inputs should share the ordinary GPU source contract");

    assert_eq!(layers.len(), 1);
    match &layers[0] {
        ViewerGpuExecutionLayer::CrossDissolve(transition) => {
            assert_eq!(transition.progress, 0.25);
            assert!(matches!(
                viewer_gpu_transition_source(&transition.left),
                Some(ViewerGpuSourceLayer::Media { opacity: 0.8, .. })
            ));
            assert!(matches!(
                viewer_gpu_transition_source(&transition.right),
                Some(ViewerGpuSourceLayer::SolidColor { .. })
            ));
        }
        _ => panic!("expected typed Cross Dissolve execution node"),
    }
}

#[test]
fn preview_transform_projection_preserves_fit_across_quality_and_proxy_extents() {
    let cases = [
        (3840, 2160, Resolution { width: 1920, height: 1080 }),
        (1920, 1080, Resolution { width: 960, height: 540 }),
        (1280, 720, Resolution { width: 480, height: 270 }),
        (960, 540, Resolution { width: 480, height: 270 }),
    ];

    for (source_width, source_height, output_sampled) in cases {
        let mut media = test_media_frame_with_size(180, source_width, source_height, 42);
        media.set_logical_resolution(Resolution { width: 3840, height: 2160 });

        let projected = project_preview_media_transform(
            [0.5, 0.0, 0.0, 0.0, 0.5, 0.0],
            &media,
            Resolution { width: 1920, height: 1080 },
            output_sampled,
        )
        .expect("valid preview projection");

        assert!((projected[0] * source_width as f32 - output_sampled.width as f32).abs() < 0.01);
        assert!((projected[4] * source_height as f32 - output_sampled.height as f32).abs() < 0.01);
    }
}

#[test]
fn gpu_composite_layers_lower_supported_working_effects() {
    let media = test_media_frame_with_size(180, 320, 180, 43);
    let mut graph = mondrian_effects::EffectGraphBuilderState::new();
    graph.append_unary(mondrian_effects::EffectRenderOp::ColorAdjust {
        exposure: 0.2,
        contrast: 1.0,
        saturation: 0.9,
        working_color_space: WorkingColorSpace::LinearRec709,
    });
    graph.append_unary(mondrian_effects::EffectRenderOp::Vignette { intensity: 0.6, feather: 0.7 });
    let effect_graph = mondrian_effects::compile_reference_render_graph(graph.finish())
        .expect("compile supported effect graph");
    let elements = vec![ResolvedPreviewElement::Media {
        frame: media,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 19,
    }];

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("supported effects should stay on GPU composite path");

    match viewer_gpu_source_layer(&layers[0]) {
        Some(ViewerGpuSourceLayer::Media { effect_plan, frame_seed, .. }) => {
            assert_eq!(effect_plan.operations().len(), 2);
            assert_eq!(*frame_seed, 19);
        }
        _ => panic!("expected media layer"),
    }
}

#[test]
fn gpu_composite_layers_lower_solid_and_adjustment_effects() {
    let mut graph = mondrian_effects::EffectGraphBuilderState::new();
    graph.append_unary(mondrian_effects::EffectRenderOp::ColorAdjust {
        exposure: 0.2,
        contrast: 1.1,
        saturation: 0.9,
        working_color_space: WorkingColorSpace::LinearRec709,
    });
    let effect_graph = mondrian_effects::compile_reference_render_graph(graph.finish())
        .expect("compile supported effect graph");
    let elements = vec![
        ResolvedPreviewElement::SolidColor(TimelineSolidColorLayer {
            color: Color { r: 0.2, g: 0.4, b: 0.6, a: 1.0 },
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 11,
        }),
        ResolvedPreviewElement::Adjustment(TimelineAdjustmentLayer {
            effect_graph,
            opacity: 0.6,
            blend_mode: None,
            frame_seed: 13,
        }),
    ];

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("solid and adjustment point effects should remain GPU-native");

    assert_eq!(layers.len(), 2);
    match viewer_gpu_source_layer(&layers[0]) {
        Some(ViewerGpuSourceLayer::SolidColor { effect_plan, .. }) => {
            assert_eq!(effect_plan.operations().len(), 1);
        }
        _ => panic!("expected solid layer"),
    }
    match &layers[1] {
        ViewerGpuExecutionLayer::Adjustment { effect_plan, opacity, blend_mode, frame_seed } => {
            assert_eq!(effect_plan.operations().len(), 1);
            assert_eq!(*opacity, 0.6);
            assert_eq!(*blend_mode, BlendMode::Normal);
            assert_eq!(*frame_seed, 13);
        }
        _ => panic!("expected adjustment layer"),
    }
}

#[test]
fn gpu_composite_layers_skip_leading_adjustment_before_layer_limit() {
    let mut graph = mondrian_effects::EffectGraphBuilderState::new();
    graph.append_unary(mondrian_effects::EffectRenderOp::Grain { amount: 0.1 });
    let effect_graph = mondrian_effects::compile_reference_render_graph(graph.finish())
        .expect("compile supported effect graph");
    let mut elements = (0..5)
        .map(|seed| {
            ResolvedPreviewElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph: Arc::clone(&effect_graph),
                opacity: 1.0,
                blend_mode: None,
                frame_seed: seed,
            })
        })
        .collect::<Vec<_>>();
    elements.push(ResolvedPreviewElement::SolidColor(
        TimelineSolidColorLayer {
            color: Color { r: 0.1, g: 0.2, b: 0.3, a: 1.0 },
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: mondrian_effects::identity_compiled_effect_graph()
                .expect("identity compiled graph"),
            frame_seed: 0,
        },
    ));

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("non-rendering leading adjustments should not consume GPU layer capacity");

    assert_eq!(layers.len(), 1);
    assert!(matches!(
        viewer_gpu_source_layer(&layers[0]),
        Some(ViewerGpuSourceLayer::SolidColor { .. })
    ));
}

#[test]
fn gpu_composite_layers_accept_source_only_media_frame() {
    let source =
        CpuEncodedColorFrame::source_rgba8(320, 180, ColorSpace::Rec709, vec![0; 320 * 180 * 4]);
    let input_transform = RenderInputTransform::to_working(
        WorkingColorSpace::LinearRec709,
        false,
        ColorEngine::mondrian_standard(),
    );
    let media = MediaPreviewFrame::from_source(
        MediaPreviewGpuSourceFrame::new(source, input_transform),
        Resolution { width: 320, height: 180 },
        test_preview_semantic_identity(44),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::from_path(PreviewDecodeExecutionPath::SoftwareCpu),
    );
    let effect_graph = mondrian_effects::compile_reference_effect_graph(
        &mondrian_effects::EffectRenderPlan::default(),
    )
    .expect("compile identity graph");
    let elements = vec![ResolvedPreviewElement::Media {
        frame: media,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 7,
    }];

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("source-only media should stay on GPU input/composite path");

    match viewer_gpu_source_layer(&layers[0]) {
        Some(ViewerGpuSourceLayer::Media { frame, gpu_source, native_source, .. }) => {
            assert!(frame.is_none());
            assert!(gpu_source.is_some());
            assert!(native_source.is_none());
        }
        _ => panic!("expected media layer"),
    }
}

#[test]
fn gpu_composite_layers_preserve_native_source_only_media_frame() {
    let media = MediaPreviewFrame::from_native(
        test_native_source_frame(320, 180),
        Resolution { width: 320, height: 180 },
        Resolution { width: 320, height: 180 },
        test_preview_semantic_identity(45),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::default(),
    );
    let effect_graph = mondrian_effects::compile_reference_effect_graph(
        &mondrian_effects::EffectRenderPlan::default(),
    )
    .expect("compile identity graph");
    let elements = vec![ResolvedPreviewElement::Media {
        frame: media,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 7,
    }];

    let layers = gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709)
        .expect("native source-only media should reach GPU composite admission");

    match viewer_gpu_source_layer(&layers[0]) {
        Some(ViewerGpuSourceLayer::Media { frame, gpu_source, native_source, .. }) => {
            assert!(frame.is_none());
            assert!(gpu_source.is_none());
            let native_source = native_source.as_ref().expect("native source");
            assert_eq!(
                native_source.input_transform.backend,
                mondrian_renderer::RenderColorTransformBackend::OcioGpuShaderPlan
            );
            assert_eq!(
                native_source.native_frame.handle_kind(),
                DecodedGpuFrameHandleKind::D3D11Texture2D
            );
            assert_eq!(
                native_source.native_frame.surface_format,
                DecodedVideoSurfaceFormat::P010
            );
            assert_eq!(native_source.native_frame.handle.id().get(), 7);
            assert_eq!(
                native_source.native_frame.diagnostics.decoded_video_sampling,
                DecodedVideoSampling {
                    matrix: mondrian_media::DecodedVideoMatrix::Bt709,
                    range: DecodedVideoRange::Limited,
                    chroma_location: DecodedVideoChromaLocation::Left,
                    bit_depth: 10,
                }
            );
            let descriptor = native_source
                .native_frame
                .native_decoded_frame_source_descriptor()
                .expect("validated media payload maps to renderer source descriptor");
            assert_eq!((descriptor.width, descriptor.height), (320, 180));
            assert_eq!(
                descriptor.handle_kind,
                DecodedGpuFrameHandleKind::D3D11Texture2D
            );
            assert_eq!(
                descriptor.source_texture_format,
                GpuNativeDecodedFrameTextureFormat::P010
            );
        }
        _ => panic!("expected media layer"),
    }
}

#[test]
fn native_source_only_media_frame_fails_cpu_working_fallback() {
    let frame = MediaPreviewFrame::from_native(
        test_native_source_frame(320, 180),
        Resolution { width: 320, height: 180 },
        Resolution { width: 320, height: 180 },
        test_preview_semantic_identity(46),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::default(),
    );

    let err = match frame.working_frame() {
        Ok(_) => panic!("native source must not be reinterpreted as CPU RGBA"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("native decoded surface"));
    assert!(err.to_string().contains("requires renderer native import"));
}

#[test]
fn gpu_composite_layers_reject_singular_media_transform() {
    let media = test_media_frame_with_size(180, 320, 180, 43);
    let effect_graph = mondrian_effects::compile_reference_effect_graph(
        &mondrian_effects::EffectRenderPlan::default(),
    )
    .expect("compile identity graph");
    let elements = vec![ResolvedPreviewElement::Media {
        frame: media,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 7,
    }];

    let err = match gpu_composite_layers_for_resolved(&elements, WorkingColorSpace::LinearRec709) {
        Ok(_) => panic!("singular transform cannot stay on GPU composite path"),
        Err(err) => err,
    };

    assert_eq!(err, GpuCompositingBlockerReason::UnsupportedTransform);
}

#[test]
fn external_gpu_preview_frame_overrides_raster_preview_for_same_plan() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };
    let key = frame.external_texture_key();
    let first_candidate_id = frame.candidate_id();

    assert!(register_test_window_preview_output(
        &service,
        &frame,
        key.clone(),
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("full-frame presentation"),
    ));
    match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Current(_) => {}
        _ => panic!("expected current external GPU preview frame"),
    }
    match service.viewer_preview_for_state(&state) {
        ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame)) => {
            assert_eq!(frame.key, key);
            assert_eq!(frame.width, 960);
            assert_eq!(frame.height, 540);
            assert_eq!(
                frame.presentation,
                ViewerExternalTexturePresentation::full_frame(960, 540)
            );
        }
        other => panic!("expected external GPU preview frame, got {other:?}"),
    }
    let diagnostics = service.diagnostics();
    assert_eq!(frame.candidate_id(), first_candidate_id);
    assert_eq!(diagnostics.gpu_preview_candidate_requests, 2);
    assert_eq!(diagnostics.gpu_preview_candidate_ready, 1);
    assert_eq!(diagnostics.gpu_preview_candidate_current, 1);
    assert_eq!(diagnostics.gpu_preview_external_frames_registered, 1);
    assert_eq!(diagnostics.gpu_preview_external_frames_rejected, 0);
    assert_eq!(diagnostics.gpu_preview_external_frames_cleared, 0);

    service.clear_external_viewer_frame();
    let second_frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected new ready GPU preview candidate after external frame clear"),
    };
    assert!(second_frame.candidate_id() > first_candidate_id);
}

#[test]
fn playing_current_gpu_candidate_releases_execution_borrow_before_prefetch() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };
    assert!(register_test_window_preview_output(
        &service,
        &frame,
        frame.external_texture_key(),
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("full-frame presentation"),
    ));

    assert!(matches!(
        execute_gpu_preview_for_test_app(&service, &state),
        PreviewGpuFrameState::Current(_)
    ));
}

#[test]
fn identical_successor_pixels_still_create_an_exact_transport_preparation() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready current GPU preview candidate"),
    };
    assert!(register_test_window_preview_output(
        &service,
        &frame,
        frame.external_texture_key(),
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("full-frame presentation"),
    ));

    let successor = state
        .preview_successor_execution_request(Instant::now())
        .expect("playing state has an immediate successor");
    let successor_intent = successor.snapshot().transport().playback_intent();
    assert!(matches!(
        service.gpu_preview_frame(successor),
        PreviewGpuFrameState::Prepared
    ));
    assert!(service.has_prepared_successor_for_intent(successor_intent));
    assert!(matches!(
        state
            .preview_successor_execution_request(Instant::now())
            .map(|request| service.gpu_preview_frame(request)),
        Some(PreviewGpuFrameState::Prepared)
    ));
}

#[test]
fn farther_lookahead_cannot_claim_the_immediate_successor_slot() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready current GPU preview candidate"),
    };
    assert!(register_test_window_preview_output(
        &service,
        &frame,
        frame.external_texture_key(),
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("full-frame presentation"),
    ));

    let lookahead = state
        .preview_lookahead_execution_request(Instant::now(), 2)
        .expect("playing state has a second future frame");
    let lookahead_intent = lookahead.snapshot().transport().playback_intent();
    assert!(matches!(
        service.gpu_preview_frame(lookahead),
        PreviewGpuFrameState::Prepared
    ));
    assert!(!service.has_prepared_successor_for_intent(lookahead_intent));

    let successor = state
        .preview_successor_execution_request(Instant::now())
        .expect("playing state has an immediate successor");
    let successor_intent = successor.snapshot().transport().playback_intent();
    assert!(!service.has_prepared_successor_for_intent(successor_intent));
    assert!(matches!(
        service.gpu_preview_frame(successor),
        PreviewGpuFrameState::Prepared
    ));
    assert!(service.has_prepared_successor_for_intent(successor_intent));
}

#[test]
fn headless_output_uses_the_same_runtime_registration_and_current_lifecycle() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestHeadlessOutput {
        resource_key: String,
    }

    let service = PreviewProductionRuntime::<TestHeadlessOutput>::new_without_workers_for_test();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready Headless GPU candidate"),
    };
    let output = TestHeadlessOutput { resource_key: frame.external_texture_key() };

    service.register_gpu_output(frame.output_key.clone(), output.clone());
    assert!(matches!(
        execute_gpu_preview_for_test_app(&service, &state),
        PreviewGpuFrameState::Current(_)
    ));
    match execute_preview_presentation_for_test_app(&service, &state) {
        PreviewPresentationState::Ready(candidate) => match candidate.into_value() {
            PreviewPresentationContent::Gpu(current) => assert_eq!(current, output),
            other => panic!("expected registered Headless GPU output, got {other:?}"),
        },
        other => panic!("expected exact registered Headless output, got {other:?}"),
    }
}

#[test]
fn retained_gpu_artifact_requires_semantic_and_physical_identity() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestOutput {
        resource_key: &'static str,
    }

    let service = PreviewProductionRuntime::<TestOutput>::new_without_workers_for_test();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };
    let other_key = PreviewOutputKey {
        plan_identity: test_preview_semantic_identity(999),
        ..frame.output_key.clone()
    };

    service.register_gpu_output(
        frame.output_key.clone(),
        TestOutput { resource_key: "physical:new" },
    );

    assert!(
        service.has_gpu_output_artifact(&frame.output_key, |output| {
            output.resource_key == "physical:new"
        })
    );
    assert!(
        !service.has_gpu_output_artifact(&frame.output_key, |output| {
            output.resource_key == "physical:old"
        })
    );
    assert!(!service.has_gpu_output_artifact(&other_key, |output| {
        output.resource_key == "physical:new"
    }));
}

#[test]
fn exact_artifact_clear_cannot_erase_same_semantic_replacement() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestOutput {
        resource_key: &'static str,
    }

    let service = PreviewProductionRuntime::<TestOutput>::new_without_workers_for_test();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };

    service.register_gpu_output(
        frame.output_key.clone(),
        TestOutput { resource_key: "physical:new" },
    );

    assert!(!service
        .clear_external_viewer_frame_for_artifact(&frame.output_key, |output| output.resource_key
            == "physical:old",));
    assert!(
        service.has_gpu_output_artifact(&frame.output_key, |output| {
            output.resource_key == "physical:new"
        })
    );
    assert!(service
        .clear_external_viewer_frame_for_artifact(&frame.output_key, |output| output.resource_key
            == "physical:new",));
    assert!(!service.has_retained_gpu_output());
}

#[test]
fn pending_replacement_prefers_last_presented_gpu_frame() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected ready GPU preview candidate"),
    };
    assert!(register_test_window_preview_output(
        &service,
        &frame,
        "viewer:last-presented",
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("full-frame presentation"),
    ));
    let sequence = state.active_sequence().expect("test sequence");

    match service.stale_viewer_content_for_sequence(sequence, frame.width, frame.height) {
        Some(PreviewPresentationContent::Gpu(stale)) => {
            assert_eq!(stale.key, "viewer:last-presented");
        }
        other => panic!("expected retained external frame, got {other:?}"),
    }
}

#[test]
fn preview_diagnostics_count_ready_render_requests() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.render_requests, 0);

    let frame = service.viewer_preview_for_state(&state);
    let _ = ready_frame(frame);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.render_requests, 1);
    assert_eq!(diagnostics.ready_frames, 1);
    assert_eq!(diagnostics.loading_frames, 0);
    assert_eq!(diagnostics.stale_frames, 0);
    assert_eq!(diagnostics.unavailable_frames, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_requests, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_ready, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_current, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_loading, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_unavailable, 0);
    assert_eq!(diagnostics.gpu_preview_candidate_pixels, 0);
    assert_eq!(diagnostics.gpu_preview_external_frames_registered, 0);
    assert_eq!(diagnostics.gpu_preview_external_frames_rejected, 0);
    assert_eq!(diagnostics.gpu_preview_external_frames_cleared, 0);
    assert_eq!(diagnostics.input_color_resolution_override, 0);
    assert_eq!(diagnostics.input_color_resolution_detected_metadata, 0);
    assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 0);
    assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 0);
    assert_eq!(diagnostics.input_color_resolution_missing_rejected, 0);
    assert_eq!(diagnostics.input_color_resolution_data_texture, 0);
    assert_eq!(diagnostics.viewer_frame_cache_hits, 0);
    assert_eq!(diagnostics.viewer_frame_cache_misses, 1);
    assert_eq!(diagnostics.frame_store.viewer_entries, 1);
    assert_eq!(diagnostics.frame_store.media_entries, 0);
    assert_eq!(diagnostics.frame_store.failure_entries, 0);
    assert_eq!(diagnostics.color_input_transform_calls, 0);
    assert_eq!(diagnostics.color_input_transform_pixels, 0);
    assert_eq!(diagnostics.color_output_transform_calls, 1);
    assert_eq!(diagnostics.color_output_transform_pixels, 960_u64 * 540);
    assert_eq!(diagnostics.color_intermediate_transform_calls, 1);
    assert_eq!(
        diagnostics.color_intermediate_transform_pixels,
        960_u64 * 540
    );
    assert_eq!(diagnostics.color_rgba8_boundary_calls, 0);
    assert_eq!(diagnostics.color_stage_plans, 1);
    assert_eq!(diagnostics.color_stage_total_stages, 2);
    assert_eq!(diagnostics.color_stage_cpu_input_stages, 0);
    assert_eq!(diagnostics.color_stage_cpu_output_stages, 2);
    assert_eq!(diagnostics.color_stage_gpu_color_stages, 0);
    assert_eq!(diagnostics.color_stage_upload_stages, 0);
    assert_eq!(diagnostics.color_stage_readback_stages, 0);
    assert_eq!(diagnostics.color_stage_gpu_blockers, 0);
    assert_eq!(diagnostics.color_stage_gpu_shader_module_blockers, 0);
    assert_eq!(diagnostics.color_stage_gpu_ocio_resource_blockers, 0);
    assert_eq!(diagnostics.color_stage_gpu_wrapper_blockers, 0);
    assert_eq!(diagnostics.color_stage_gpu_render_pipeline_blockers, 0);
    assert_eq!(diagnostics.color_stage_pixels, 2 * 960_u64 * 540);
    assert_eq!(diagnostics.color_composite_plans, 1);
    assert_eq!(diagnostics.color_composite_elements, 1);
    assert_eq!(diagnostics.color_composite_float_linear, 1);
    assert_eq!(diagnostics.color_composite_legacy_rgba8, 0);
}

#[test]
fn preview_diagnostics_count_gpu_stage_blocker_breakdown() {
    let service = WindowPreviewAdapter::new();
    service.record_color_stage(RenderColorStageDiagnostics {
        total_stages: 1,
        gpu_color_stages: 1,
        gpu_blockers: 4,
        gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
            shader_module_not_prepared: 1,
            ocio_resource_bind_group_not_prepared: 1,
            fullscreen_wrapper_not_prepared: 1,
            render_pipeline_not_prepared: 1,
            ..RenderColorStageGpuBlockerBreakdown::default()
        },
        ..RenderColorStageDiagnostics::default()
    });

    let diagnostics = service.diagnostics();

    assert_eq!(diagnostics.color_stage_gpu_blockers, 4);
    assert_eq!(diagnostics.color_stage_gpu_shader_module_blockers, 1);
    assert_eq!(diagnostics.color_stage_gpu_ocio_resource_blockers, 1);
    assert_eq!(diagnostics.color_stage_gpu_wrapper_blockers, 1);
    assert_eq!(diagnostics.color_stage_gpu_render_pipeline_blockers, 1);
}

fn test_preview_decode_diagnostics(
    access_mode: PreviewDecodeAccessMode,
    hardware_decode_decision: PreviewHardwareDecodeDecision,
    hardware_decode_blocker: PreviewHardwareDecodeBlocker,
) -> PreviewDecodeDiagnostics {
    PreviewDecodeDiagnostics {
        path: PreviewDecodePath::InProcessFfmpegCpuRgba,
        elapsed_us: 1_000,
        cache_hit: false,
        access_mode,
        external_process: false,
        cpu_resident: true,
        seek_performed: false,
        requested_pts: None,
        selected_pts: None,
        selected_duration_pts: None,
        selected_temporal_extent_source: PreviewTemporalExtentSource::Unknown,
        temporal_approximation: false,
        seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
        forward_reuse_frame_window: 0,
        forward_decode_budget_frames: 0,
        any_seek_window_ms: 0,
        scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
        hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
        hardware_decode_decision,
        hardware_decode_candidate_backend: Some(HwAccelBackend::D3D11VA),
        hardware_decode_candidate_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
        hardware_decode_adapter_available: true,
        hardware_decode_ffmpeg_device_type_available: true,
        hardware_decode_ffmpeg_codec_config_available: true,
        hardware_decode_ffmpeg_hw_pixel_format: Some(HwAccelPixelFormat::D3D11),
        hardware_decode_ffmpeg_device_context_attempted: true,
        hardware_decode_ffmpeg_device_context_created: true,
        hardware_decode_ffmpeg_device_context_error_code: None,
        hardware_decode_cpu_transfer_configured: false,
        hardware_decode_cpu_transfer_observed: false,
        hardware_decode_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
        session_disposition: PreviewDecodeSessionDisposition::Opened,
        forward_reused: false,
        seek_index_available: false,
        seek_index_keyframes: 0,
        seek_index_observed_packets: 0,
        seek_index_source: PreviewSeekIndexSource::None,
        seek_index_used: false,
        seek_index_anchor_pts: None,
        decoded_frame_count: 1,
        threading_kind: PreviewDecodeThreadingKind::Frame,
        threading_count: 4,
        stage_durations: PreviewDecodeStageDurations::default(),
        hw_accel_backend: HwAccelBackend::D3D11VA,
        hardware_decode_active: false,
        zero_copy_active: false,
        decoded_frame_residency: DecodedFrameResidency::CpuRgba,
        gpu_frame_handle_kind: None,
        hardware_decode_blocker,
        native_decode_fallback: None,
        decoded_surface_format: DecodedVideoSurfaceFormat::P010,
        decoded_video_sampling: DecodedVideoSampling::default(),
    }
}

#[test]
fn nearest_keyframe_scrub_is_explicitly_degraded_until_settled() {
    let mut diagnostics = test_preview_decode_diagnostics(
        PreviewDecodeAccessMode::ScrubCursor,
        PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
        PreviewHardwareDecodeBlocker::None,
    );
    diagnostics.hardware_decode_request = PreviewHardwareDecodeRequest::Auto;
    diagnostics.requested_pts = Some(1_000);
    diagnostics.selected_pts = Some(960);
    diagnostics.temporal_approximation = true;

    assert_eq!(
        preview_decode_presentation_quality(&diagnostics),
        Ok(mondrian_playback::FramePresentationQuality::Degraded)
    );

    diagnostics.selected_pts = diagnostics.requested_pts;
    diagnostics.temporal_approximation = false;
    assert_eq!(
        preview_decode_presentation_quality(&diagnostics),
        Ok(mondrian_playback::FramePresentationQuality::Ready)
    );
}

#[test]
fn exact_preview_access_rejects_temporal_approximation() {
    for access_mode in [
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    ] {
        let mut diagnostics = test_preview_decode_diagnostics(
            access_mode,
            PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
            PreviewHardwareDecodeBlocker::None,
        );
        diagnostics.hardware_decode_request = PreviewHardwareDecodeRequest::Auto;
        diagnostics.requested_pts = Some(1_000);
        diagnostics.selected_pts = Some(1_001);
        diagnostics.temporal_approximation = true;

        assert_eq!(
            preview_decode_presentation_quality(&diagnostics),
            Err(MediaPreviewFailureReason::TemporalMismatch)
        );
    }
}

#[test]
fn preview_diagnostics_count_decode_paths_and_duration() {
    let service = WindowPreviewAdapter::new();

    service.record_preview_decode(
        PreviewDecodeDiagnostics {
            path: PreviewDecodePath::InProcessFfmpegCpuRgba,
            elapsed_us: 1_000,
            cache_hit: false,
            access_mode: PreviewDecodeAccessMode::ScrubCursor,
            external_process: false,
            cpu_resident: true,
            seek_performed: true,
            requested_pts: Some(100),
            selected_pts: Some(100),
            selected_duration_pts: Some(40),
            selected_temporal_extent_source: PreviewTemporalExtentSource::FrameDuration,
            temporal_approximation: false,
            seek_strategy: PreviewDecodeSeekStrategy::BoundedAnyFrame,
            forward_reuse_frame_window: 1,
            forward_decode_budget_frames: 8,
            any_seek_window_ms: 120,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
            hardware_decode_candidate_backend: None,
            hardware_decode_candidate_handle_kind: None,
            hardware_decode_adapter_available: false,
            hardware_decode_ffmpeg_device_type_available: false,
            hardware_decode_ffmpeg_codec_config_available: false,
            hardware_decode_ffmpeg_hw_pixel_format: None,
            hardware_decode_ffmpeg_device_context_attempted: false,
            hardware_decode_ffmpeg_device_context_created: false,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: false,
            hardware_decode_cpu_transfer_observed: false,
            hardware_decode_cpu_transfer_status:
                PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            session_disposition: PreviewDecodeSessionDisposition::Opened,
            forward_reused: false,
            seek_index_available: true,
            seek_index_keyframes: 3,
            seek_index_observed_packets: 90,
            seek_index_source: PreviewSeekIndexSource::ProbeBacked,
            seek_index_used: true,
            seek_index_anchor_pts: Some(120),
            decoded_frame_count: 48,
            threading_kind: PreviewDecodeThreadingKind::Frame,
            threading_count: 6,
            stage_durations: PreviewDecodeStageDurations {
                session_open_us: 100,
                output_lease_wait_us: 0,
                cache_lookup_us: 2,
                seek_us: 300,
                packet_decode_us: 500,
                hardware_transfer_us: 0,
                swscale_us: 70,
                rgba_copy_us: 30,
                external_process_us: 0,
            },
            hw_accel_backend: HwAccelBackend::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::P010,
            decoded_video_sampling: DecodedVideoSampling::default(),
        },
        MediaPreviewRequestPriority::Current,
        1_200,
        true,
    );
    service.record_preview_decode(
        PreviewDecodeDiagnostics {
            path: PreviewDecodePath::ExternalFfmpegCpuRgba,
            elapsed_us: 2_500,
            cache_hit: false,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            external_process: true,
            cpu_resident: true,
            seek_performed: false,
            requested_pts: None,
            selected_pts: None,
            selected_duration_pts: None,
            selected_temporal_extent_source: PreviewTemporalExtentSource::Unknown,
            temporal_approximation: false,
            seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
            forward_reuse_frame_window: 3,
            forward_decode_budget_frames: 48,
            any_seek_window_ms: 0,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary,
            hardware_decode_candidate_backend: None,
            hardware_decode_candidate_handle_kind: None,
            hardware_decode_adapter_available: false,
            hardware_decode_ffmpeg_device_type_available: false,
            hardware_decode_ffmpeg_codec_config_available: false,
            hardware_decode_ffmpeg_hw_pixel_format: None,
            hardware_decode_ffmpeg_device_context_attempted: false,
            hardware_decode_ffmpeg_device_context_created: false,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: false,
            hardware_decode_cpu_transfer_observed: false,
            hardware_decode_cpu_transfer_status:
                PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            session_disposition: PreviewDecodeSessionDisposition::Reused,
            forward_reused: false,
            seek_index_available: false,
            seek_index_keyframes: 0,
            seek_index_observed_packets: 0,
            seek_index_source: PreviewSeekIndexSource::None,
            seek_index_used: false,
            seek_index_anchor_pts: None,
            decoded_frame_count: 0,
            threading_kind: PreviewDecodeThreadingKind::None,
            threading_count: 0,
            stage_durations: PreviewDecodeStageDurations {
                session_open_us: 0,
                output_lease_wait_us: 0,
                cache_lookup_us: 0,
                seek_us: 0,
                packet_decode_us: 0,
                hardware_transfer_us: 0,
                swscale_us: 0,
                rgba_copy_us: 0,
                external_process_us: 2_450,
            },
            hw_accel_backend: HwAccelBackend::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            decoded_video_sampling: DecodedVideoSampling::default(),
        },
        MediaPreviewRequestPriority::Current,
        400,
        true,
    );
    service.record_preview_decode(
        PreviewDecodeDiagnostics {
            path: PreviewDecodePath::InProcessFfmpegCpuRgba,
            elapsed_us: 25,
            cache_hit: false,
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            external_process: false,
            cpu_resident: true,
            seek_performed: false,
            requested_pts: None,
            selected_pts: None,
            selected_duration_pts: None,
            selected_temporal_extent_source: PreviewTemporalExtentSource::Unknown,
            temporal_approximation: false,
            seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
            forward_reuse_frame_window: 0,
            forward_decode_budget_frames: 48,
            any_seek_window_ms: 0,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
            hardware_decode_candidate_backend: None,
            hardware_decode_candidate_handle_kind: None,
            hardware_decode_adapter_available: false,
            hardware_decode_ffmpeg_device_type_available: false,
            hardware_decode_ffmpeg_codec_config_available: false,
            hardware_decode_ffmpeg_hw_pixel_format: None,
            hardware_decode_ffmpeg_device_context_attempted: false,
            hardware_decode_ffmpeg_device_context_created: false,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: false,
            hardware_decode_cpu_transfer_observed: false,
            hardware_decode_cpu_transfer_status:
                PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            session_disposition: PreviewDecodeSessionDisposition::Opened,
            forward_reused: false,
            seek_index_available: true,
            seek_index_keyframes: 2,
            seek_index_observed_packets: 40,
            seek_index_source: PreviewSeekIndexSource::SessionObserved,
            seek_index_used: false,
            seek_index_anchor_pts: None,
            decoded_frame_count: 0,
            threading_kind: PreviewDecodeThreadingKind::None,
            threading_count: 0,
            stage_durations: PreviewDecodeStageDurations {
                session_open_us: 0,
                output_lease_wait_us: 0,
                cache_lookup_us: 20,
                seek_us: 0,
                packet_decode_us: 0,
                hardware_transfer_us: 0,
                swscale_us: 0,
                rgba_copy_us: 0,
                external_process_us: 0,
            },
            hw_accel_backend: HwAccelBackend::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Nv12,
            decoded_video_sampling: DecodedVideoSampling::default(),
        },
        MediaPreviewRequestPriority::Current,
        20,
        true,
    );
    service.record_preview_decode(
        PreviewDecodeDiagnostics {
            path: PreviewDecodePath::PlaybackSessionRingHit,
            elapsed_us: 40,
            cache_hit: true,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            external_process: false,
            cpu_resident: true,
            seek_performed: false,
            requested_pts: None,
            selected_pts: None,
            selected_duration_pts: None,
            selected_temporal_extent_source: PreviewTemporalExtentSource::Unknown,
            temporal_approximation: false,
            seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
            forward_reuse_frame_window: 3,
            forward_decode_budget_frames: 48,
            any_seek_window_ms: 0,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::PreferGpuResident,
            hardware_decode_decision: PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer,
            hardware_decode_candidate_backend: Some(HwAccelBackend::D3D11VA),
            hardware_decode_candidate_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            hardware_decode_adapter_available: false,
            hardware_decode_ffmpeg_device_type_available: true,
            hardware_decode_ffmpeg_codec_config_available: true,
            hardware_decode_ffmpeg_hw_pixel_format: Some(HwAccelPixelFormat::D3D11),
            hardware_decode_ffmpeg_device_context_attempted: true,
            hardware_decode_ffmpeg_device_context_created: true,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: true,
            hardware_decode_cpu_transfer_observed: true,
            hardware_decode_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus::Observed,
            session_disposition: PreviewDecodeSessionDisposition::BypassedCache,
            forward_reused: false,
            seek_index_available: true,
            seek_index_keyframes: 4,
            seek_index_observed_packets: 128,
            seek_index_source: PreviewSeekIndexSource::ProbeBacked,
            seek_index_used: false,
            seek_index_anchor_pts: None,
            decoded_frame_count: 0,
            threading_kind: PreviewDecodeThreadingKind::None,
            threading_count: 0,
            stage_durations: PreviewDecodeStageDurations {
                session_open_us: 0,
                output_lease_wait_us: 0,
                cache_lookup_us: 12,
                seek_us: 0,
                packet_decode_us: 0,
                hardware_transfer_us: 42,
                swscale_us: 0,
                rgba_copy_us: 0,
                external_process_us: 0,
            },
            hw_accel_backend: HwAccelBackend::D3D11VA,
            hardware_decode_active: true,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Nv12,
            decoded_video_sampling: DecodedVideoSampling::default(),
        },
        MediaPreviewRequestPriority::Current,
        0,
        true,
    );
    service.record_preview_decode_queue_wait(
        MediaPreviewRequestPriority::Prefetch,
        PreviewDecodeAccessMode::PlaybackCursor,
        400,
        false,
    );
    service.record_preview_decode_queue_wait(
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::ScrubCursor,
        1_200,
        true,
    );
    service.record_preview_decode_queue_wait(
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        20,
        true,
    );
    service.record_preview_decode_cancel(
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(MediaPreviewCancelReason::PrefetchDeadline),
        Some(mondrian_media::PreviewDecodeCancellation {
            checkpoint: mondrian_media::PreviewDecodeCancellationCheckpoint::PacketRead,
            source: mondrian_media::PreviewDecodeCancellationSource::FfmpegIoInterrupt,
            session_disposition: PreviewDecodeSessionDisposition::Reused,
            session_open_us: 0,
        }),
        700,
        Some(LogicalCancellationObserved {
            execution_elapsed_us: 600,
            request_elapsed_us: Some(50),
        }),
        false,
    );
    service.record_preview_decode_cancel(
        PreviewDecodeAccessMode::ScrubCursor,
        Some(MediaPreviewCancelReason::Obsolete),
        None,
        1_400,
        Some(LogicalCancellationObserved {
            execution_elapsed_us: 1_000,
            request_elapsed_us: Some(200),
        }),
        false,
    );
    service.record_preview_decode_cancel(
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        Some(MediaPreviewCancelReason::Shutdown),
        None,
        20,
        Some(LogicalCancellationObserved {
            execution_elapsed_us: 5,
            request_elapsed_us: Some(5),
        }),
        false,
    );

    let diagnostics = service.diagnostics();

    assert_eq!(diagnostics.decode_canceled_jobs, 3);
    assert_eq!(diagnostics.decode_canceled_shutdown_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_prefetch_deadline_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_unknown_jobs, 0);
    assert_eq!(diagnostics.decode_cancellation_checkpoints.total, 1);
    assert_eq!(
        diagnostics.decode_cancellation_checkpoints.ffmpeg_io_interrupt,
        1
    );
    assert_eq!(
        diagnostics.decode_cancellation_checkpoints.checkpoints.packet_read,
        1
    );
    assert_eq!(diagnostics.decode_canceled_total_duration_us, 2_120);
    assert_eq!(diagnostics.decode_canceled_max_duration_us, 1_400);
    assert_eq!(diagnostics.decode_canceled_last_duration_us, 20);
    assert_eq!(diagnostics.decode_cancel_observation_samples, 3);
    assert_eq!(diagnostics.decode_cancel_observation_total_us, 255);
    assert_eq!(diagnostics.decode_cancel_observation_max_us, 200);
    assert_eq!(diagnostics.decode_cancel_observation_last_us, 5);
    assert_eq!(diagnostics.decode_canceled_return_latency_total_us, 515);
    assert_eq!(diagnostics.decode_canceled_return_latency_max_us, 400);
    assert_eq!(diagnostics.decode_canceled_return_latency_last_us, 15);
    assert_eq!(diagnostics.decode_in_process_cpu_frames, 2);
    assert_eq!(diagnostics.decode_external_ffmpeg_cpu_rgba_frames, 1);
    assert_eq!(diagnostics.decode_playback_session_ring_hit_frames, 1);
    assert_eq!(diagnostics.decode_cache_hit_frames, 1);
    assert_eq!(diagnostics.decode_playback_cursor_frames, 2);
    assert_eq!(diagnostics.decode_scrub_cursor_frames, 1);
    assert_eq!(diagnostics.decode_random_access_still_frames, 1);
    assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_scrub_cursor_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_random_access_still_jobs, 1);
    assert_eq!(diagnostics.decode_total_duration_us, 3_565);
    assert_eq!(diagnostics.decode_max_duration_us, 2_500);
    assert_eq!(diagnostics.decode_last_duration_us, 40);
    assert_eq!(diagnostics.decode_queue_wait_total_us, 1_620);
    assert_eq!(diagnostics.decode_queue_wait_max_us, 1_200);
    assert_eq!(diagnostics.decode_queue_wait_last_us, 20);
    assert_eq!(diagnostics.decode_current_queue_wait_max_us, 1_200);
    assert_eq!(diagnostics.decode_prefetch_queue_wait_max_us, 400);
    assert_eq!(diagnostics.decode_seeked_frames, 1);
    assert_eq!(diagnostics.decode_decoded_frame_count, 48);
    assert_eq!(diagnostics.decode_max_decoded_frame_count, 48);
    assert_eq!(diagnostics.decode_threading_none_frames, 3);
    assert_eq!(diagnostics.decode_threading_frame_frames, 1);
    assert_eq!(diagnostics.decode_threading_slice_frames, 0);
    assert_eq!(diagnostics.decode_last_threading_count, 0);
    assert_eq!(diagnostics.decode_max_threading_count, 6);
    assert_eq!(diagnostics.decode_stage_durations.session_open_us, 100);
    assert_eq!(diagnostics.decode_stage_durations.cache_lookup_us, 34);
    assert_eq!(diagnostics.decode_stage_durations.seek_us, 300);
    assert_eq!(diagnostics.decode_stage_durations.packet_decode_us, 500);
    assert_eq!(diagnostics.decode_stage_durations.hardware_transfer_us, 42);
    assert_eq!(diagnostics.decode_stage_durations.swscale_us, 70);
    assert_eq!(diagnostics.decode_stage_durations.rgba_copy_us, 30);
    assert_eq!(
        diagnostics.decode_stage_durations.external_process_us,
        2_450
    );
    assert_eq!(
        diagnostics.decode_max_frame_stage_durations.external_process_us,
        2_450
    );
    assert_eq!(diagnostics.decode_max_frame_queue_wait_us, 400);
    assert_eq!(
        diagnostics.decode_max_frame_bottleneck,
        PreviewDecodeBottleneck::ExternalProcess
    );
    let playback_profile = diagnostics.decode_access_mode_profiles.playback_cursor;
    assert_eq!(playback_profile.frames, 2);
    assert_eq!(playback_profile.external_ffmpeg_cpu_rgba_frames, 1);
    assert_eq!(playback_profile.playback_session_ring_hit_frames, 1);
    assert_eq!(playback_profile.total_duration_us, 2_540);
    assert_eq!(playback_profile.max_duration_us, 2_500);
    assert_eq!(playback_profile.last_duration_us, 40);
    assert_eq!(playback_profile.latency_buckets.le_10ms, 2);
    assert_eq!(playback_profile.latency_buckets.total(), 2);
    assert_eq!(playback_profile.queue_wait_total_us, 400);
    assert_eq!(playback_profile.queue_wait_max_us, 400);
    assert_eq!(playback_profile.queue_wait_last_us, 400);
    assert_eq!(playback_profile.queue_wait_buckets.le_10ms, 1);
    assert_eq!(playback_profile.queue_wait_buckets.total(), 1);
    assert_eq!(playback_profile.queue_wait_samples, 1);
    assert_eq!(playback_profile.max_frame_queue_wait_us, 400);
    assert_eq!(
        playback_profile.max_frame_bottleneck,
        PreviewDecodeBottleneck::ExternalProcess
    );
    assert_eq!(playback_profile.canceled_jobs, 1);
    assert_eq!(playback_profile.canceled_prefetch_deadline_jobs, 1);
    assert_eq!(playback_profile.canceled_total_duration_us, 700);
    assert_eq!(playback_profile.canceled_max_duration_us, 700);
    assert_eq!(playback_profile.canceled_last_duration_us, 700);
    assert_eq!(playback_profile.cancel_observation_samples, 1);
    assert_eq!(playback_profile.cancel_observation_total_us, 50);
    assert_eq!(playback_profile.cancel_observation_max_us, 50);
    assert_eq!(playback_profile.cancel_observation_last_us, 50);
    assert_eq!(playback_profile.canceled_return_latency_total_us, 100);
    assert_eq!(playback_profile.canceled_return_latency_max_us, 100);
    assert_eq!(playback_profile.canceled_return_latency_last_us, 100);
    assert_eq!(playback_profile.canceled_shutdown_jobs, 0);
    assert_eq!(playback_profile.canceled_obsolete_jobs, 0);
    assert_eq!(playback_profile.canceled_unknown_jobs, 0);
    assert_eq!(playback_profile.canceled_session_open_attempts, 0);
    assert_eq!(playback_profile.canceled_session_reused_attempts, 1);
    assert_eq!(playback_profile.canceled_session_open_total_duration_us, 0);
    assert_eq!(playback_profile.session_reused_frames, 1);
    assert_eq!(playback_profile.session_opened_frames, 0);
    assert_eq!(playback_profile.session_bypassed_cache_frames, 1);
    assert_eq!(playback_profile.work_classes.reused_other.frames, 1);
    assert_eq!(
        playback_profile.work_classes.reused_other.total_duration_us,
        2_500
    );
    assert_eq!(playback_profile.work_classes.cache_hit.frames, 1);
    assert_eq!(
        playback_profile.work_classes.cache_hit.total_duration_us,
        40
    );
    assert_eq!(playback_profile.work_classes.session_opened.frames, 0);
    assert_eq!(playback_profile.forward_reused_frames, 0);
    assert_eq!(playback_profile.seek_index_available_frames, 1);
    assert_eq!(playback_profile.seek_index_probe_backed_frames, 1);
    assert_eq!(playback_profile.seek_index_session_observed_frames, 0);
    assert_eq!(playback_profile.hardware_decode_active_frames, 1);
    assert_eq!(playback_profile.zero_copy_active_frames, 0);
    assert_eq!(playback_profile.gpu_texture_resident_frames, 0);
    assert_eq!(playback_profile.decoded_nv12_surface_frames, 1);
    assert_eq!(playback_profile.decoded_p010_surface_frames, 0);
    assert_eq!(
        playback_profile.hardware_decode_texture_residency_blocker_frames,
        2
    );
    assert_eq!(playback_profile.hardware_decode_auto_requested_frames, 0);
    assert_eq!(
        playback_profile.hardware_decode_prefer_hardware_requested_frames,
        0
    );
    assert_eq!(
        playback_profile.hardware_decode_prefer_gpu_requested_frames,
        2
    );
    assert_eq!(
        playback_profile.hardware_decode_require_gpu_requested_frames,
        0
    );
    assert_eq!(playback_profile.hardware_decode_cpu_not_requested_frames, 0);
    assert_eq!(playback_profile.hardware_decode_cpu_unavailable_frames, 0);
    assert_eq!(
        playback_profile.hardware_decode_backend_unavailable_frames,
        0
    );
    assert_eq!(playback_profile.hardware_decode_codec_unsupported_frames, 0);
    assert_eq!(
        playback_profile.hardware_decode_device_context_attempted_frames,
        1
    );
    assert_eq!(
        playback_profile.hardware_decode_device_context_created_frames,
        1
    );
    assert_eq!(
        playback_profile.hardware_decode_device_context_unavailable_frames,
        0
    );
    assert_eq!(playback_profile.hardware_decode_cpu_transfer_frames, 1);
    assert_eq!(
        playback_profile.hardware_decode_cpu_transfer_configured_frames,
        1
    );
    assert_eq!(
        playback_profile.hardware_decode_cpu_transfer_observed_frames,
        1
    );
    assert_eq!(playback_profile.hardware_decode_backend_boundary_frames, 1);
    assert_eq!(
        playback_profile.hardware_decode_gpu_resident_native_frames,
        0
    );
    assert_eq!(playback_profile.hardware_decode_candidate_d3d12va_frames, 0);
    assert_eq!(playback_profile.hardware_decode_candidate_d3d11va_frames, 1);
    assert_eq!(playback_profile.hardware_decode_candidate_dxva2_frames, 0);
    assert_eq!(
        playback_profile.hardware_decode_candidate_videotoolbox_frames,
        0
    );
    assert_eq!(playback_profile.hardware_decode_candidate_vaapi_frames, 0);
    assert_eq!(playback_profile.hardware_decode_candidate_vdpau_frames, 0);
    assert_eq!(playback_profile.hardware_decode_candidate_cuda_frames, 0);
    assert_eq!(
        playback_profile.hardware_decode_adapter_unavailable_frames,
        1
    );
    assert_eq!(playback_profile.seek_index_used_frames, 0);
    assert_eq!(playback_profile.seek_index_keyframes_max, 4);
    assert_eq!(playback_profile.seek_index_observed_packets_max, 128);
    assert_eq!(playback_profile.stage_durations.cache_lookup_us, 12);
    assert_eq!(playback_profile.stage_durations.hardware_transfer_us, 42);
    assert_eq!(
        playback_profile.max_frame_stage_durations.external_process_us,
        2_450
    );
    let scrub_profile = diagnostics.decode_access_mode_profiles.scrub_cursor;
    assert_eq!(scrub_profile.frames, 1);
    assert_eq!(scrub_profile.in_process_cpu_frames, 1);
    assert_eq!(scrub_profile.latency_buckets.le_10ms, 1);
    assert_eq!(scrub_profile.latency_buckets.total(), 1);
    assert_eq!(scrub_profile.seeked_frames, 1);
    assert_eq!(scrub_profile.bounded_any_seek_strategy_frames, 1);
    assert_eq!(scrub_profile.keyframe_seek_strategy_frames, 0);
    assert_eq!(scrub_profile.forward_reuse_frame_window_max, 1);
    assert_eq!(scrub_profile.forward_decode_budget_frames_max, 8);
    assert_eq!(scrub_profile.any_seek_window_ms_max, 120);
    assert_eq!(scrub_profile.session_reused_frames, 0);
    assert_eq!(scrub_profile.session_opened_frames, 1);
    assert_eq!(scrub_profile.work_classes.reused_seek.frames, 0);
    assert_eq!(scrub_profile.work_classes.session_opened.frames, 1);
    assert_eq!(
        scrub_profile.work_classes.session_opened.total_duration_us,
        1_000
    );
    assert_eq!(
        scrub_profile.work_classes.session_opened.max_duration_us,
        1_000
    );
    assert_eq!(
        scrub_profile.work_classes.session_opened.latency_buckets.total(),
        1
    );
    assert_eq!(scrub_profile.forward_reused_frames, 0);
    assert_eq!(scrub_profile.seek_index_available_frames, 1);
    assert_eq!(scrub_profile.seek_index_probe_backed_frames, 1);
    assert_eq!(scrub_profile.seek_index_session_observed_frames, 0);
    assert_eq!(scrub_profile.decoded_nv12_surface_frames, 0);
    assert_eq!(scrub_profile.decoded_p010_surface_frames, 1);
    assert_eq!(scrub_profile.seek_index_used_frames, 1);
    assert_eq!(scrub_profile.seek_index_keyframes_max, 3);
    assert_eq!(scrub_profile.seek_index_observed_packets_max, 90);
    assert_eq!(scrub_profile.hardware_decode_auto_requested_frames, 1);
    assert_eq!(
        scrub_profile.hardware_decode_prefer_hardware_requested_frames,
        0
    );
    assert_eq!(scrub_profile.hardware_decode_cpu_not_requested_frames, 1);
    assert_eq!(scrub_profile.hardware_decode_prefer_gpu_requested_frames, 0);
    assert_eq!(scrub_profile.queue_wait_total_us, 1_200);
    assert_eq!(scrub_profile.queue_wait_max_us, 1_200);
    assert_eq!(scrub_profile.queue_wait_last_us, 1_200);
    assert_eq!(scrub_profile.queue_wait_buckets.le_10ms, 1);
    assert_eq!(scrub_profile.queue_wait_buckets.total(), 1);
    assert_eq!(scrub_profile.canceled_jobs, 1);
    assert_eq!(scrub_profile.canceled_obsolete_jobs, 1);
    assert_eq!(scrub_profile.canceled_total_duration_us, 1_400);
    assert_eq!(scrub_profile.canceled_max_duration_us, 1_400);
    assert_eq!(scrub_profile.canceled_last_duration_us, 1_400);
    assert_eq!(scrub_profile.cancel_observation_samples, 1);
    assert_eq!(scrub_profile.cancel_observation_total_us, 200);
    assert_eq!(scrub_profile.cancel_observation_max_us, 200);
    assert_eq!(scrub_profile.cancel_observation_last_us, 200);
    assert_eq!(scrub_profile.canceled_return_latency_total_us, 400);
    assert_eq!(scrub_profile.canceled_return_latency_max_us, 400);
    assert_eq!(scrub_profile.canceled_return_latency_last_us, 400);
    assert_eq!(scrub_profile.canceled_shutdown_jobs, 0);
    assert_eq!(scrub_profile.canceled_prefetch_deadline_jobs, 0);
    assert_eq!(scrub_profile.canceled_unknown_jobs, 0);
    assert_eq!(scrub_profile.decoded_frame_count, 48);
    assert_eq!(scrub_profile.max_decoded_frame_count, 48);
    assert_eq!(scrub_profile.stage_durations.packet_decode_us, 500);
    let still_profile = diagnostics.decode_access_mode_profiles.random_access_still;
    assert_eq!(still_profile.frames, 1);
    assert_eq!(still_profile.cache_hit_frames, 0);
    assert_eq!(still_profile.latency_buckets.le_10ms, 1);
    assert_eq!(still_profile.latency_buckets.total(), 1);
    assert_eq!(still_profile.queue_wait_total_us, 20);
    assert_eq!(still_profile.queue_wait_max_us, 20);
    assert_eq!(still_profile.queue_wait_last_us, 20);
    assert_eq!(still_profile.queue_wait_buckets.le_10ms, 1);
    assert_eq!(still_profile.queue_wait_buckets.total(), 1);
    assert_eq!(still_profile.canceled_jobs, 1);
    assert_eq!(still_profile.canceled_shutdown_jobs, 1);
    assert_eq!(still_profile.canceled_total_duration_us, 20);
    assert_eq!(still_profile.canceled_max_duration_us, 20);
    assert_eq!(still_profile.canceled_last_duration_us, 20);
    assert_eq!(still_profile.cancel_observation_samples, 1);
    assert_eq!(still_profile.cancel_observation_total_us, 5);
    assert_eq!(still_profile.cancel_observation_max_us, 5);
    assert_eq!(still_profile.cancel_observation_last_us, 5);
    assert_eq!(still_profile.canceled_return_latency_total_us, 15);
    assert_eq!(still_profile.canceled_return_latency_max_us, 15);
    assert_eq!(still_profile.canceled_return_latency_last_us, 15);
    assert_eq!(still_profile.canceled_obsolete_jobs, 0);
    assert_eq!(still_profile.canceled_prefetch_deadline_jobs, 0);
    assert_eq!(still_profile.canceled_unknown_jobs, 0);
    assert_eq!(still_profile.session_reused_frames, 0);
    assert_eq!(still_profile.session_opened_frames, 1);
    assert_eq!(still_profile.work_classes.reused_seek.frames, 0);
    assert_eq!(still_profile.work_classes.session_opened.frames, 1);
    assert_eq!(
        still_profile.work_classes.session_opened.total_duration_us,
        25
    );
    assert_eq!(
        still_profile.work_classes.session_opened.max_duration_us,
        25
    );
    assert_eq!(
        still_profile.work_classes.session_opened.latency_buckets.total(),
        1
    );
    assert_eq!(still_profile.seek_index_available_frames, 1);
    assert_eq!(still_profile.seek_index_probe_backed_frames, 0);
    assert_eq!(still_profile.seek_index_session_observed_frames, 1);
    assert_eq!(still_profile.decoded_nv12_surface_frames, 1);
    assert_eq!(still_profile.decoded_p010_surface_frames, 0);
    assert_eq!(still_profile.seek_index_used_frames, 0);
    assert_eq!(still_profile.seek_index_keyframes_max, 2);
    assert_eq!(still_profile.seek_index_observed_packets_max, 40);
    assert_eq!(still_profile.hardware_decode_auto_requested_frames, 1);
    assert_eq!(
        still_profile.hardware_decode_prefer_hardware_requested_frames,
        0
    );
    assert_eq!(still_profile.hardware_decode_cpu_not_requested_frames, 1);
    assert_eq!(still_profile.stage_durations.cache_lookup_us, 20);
    assert_eq!(
        diagnostics.decode_access_mode_profiles.slowest_access_mode(),
        Some(PreviewDecodeAccessMode::PlaybackCursor)
    );
}

#[test]
fn cache_only_current_decode_does_not_pollute_presentation_queue_wait() {
    let service = WindowPreviewAdapter::new();

    service.record_preview_decode_queue_wait(
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
        850_000,
        false,
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_queue_wait_max_us, 850_000);
    assert_eq!(diagnostics.decode_current_queue_wait_max_us, 0);
    assert_eq!(
        diagnostics.decode_access_mode_profiles.playback_cursor.queue_wait_max_us,
        850_000
    );
}

#[test]
fn preview_diagnostics_count_decode_failures_by_access_mode() {
    let service = WindowPreviewAdapter::new();

    service.record_preview_decode_failure(
        PreviewDecodeAccessMode::ScrubCursor,
        Some(MediaPreviewFailureReason::Timeout),
    );
    service.record_preview_decode_failure(
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        Some(MediaPreviewFailureReason::DecodeError),
    );
    service.record_preview_decode_failure(
        PreviewDecodeAccessMode::ScrubCursor,
        Some(MediaPreviewFailureReason::ForwardDecodeBudgetExhausted),
    );

    let diagnostics = service.diagnostics();

    assert_eq!(diagnostics.decode_failures, 3);
    assert_eq!(diagnostics.decode_timeout_failures, 1);
    assert_eq!(diagnostics.decode_budget_exhausted_failures, 1);
    let scrub_profile = diagnostics.decode_access_mode_profiles.scrub_cursor;
    assert_eq!(scrub_profile.failed_jobs, 2);
    assert_eq!(scrub_profile.timeout_failures, 1);
    assert_eq!(scrub_profile.budget_exhausted_failures, 1);
    let still_profile = diagnostics.decode_access_mode_profiles.random_access_still;
    assert_eq!(still_profile.failed_jobs, 1);
    assert_eq!(still_profile.timeout_failures, 0);
    assert_eq!(
        diagnostics
            .decode_performance_summary(50_000)
            .expect("decode evidence")
            .decode_timeout_failures,
        1
    );
}

#[test]
fn preview_diagnostics_count_prefetch_preemptions_by_access_mode() {
    let service = WindowPreviewAdapter::new();

    service.record_preview_decode_cancel(
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(MediaPreviewCancelReason::PrefetchPreemptedByCurrent),
        None,
        120,
        Some(LogicalCancellationObserved {
            execution_elapsed_us: 80,
            request_elapsed_us: Some(10),
        }),
        false,
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_canceled_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_prefetch_preempted_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_prefetch_deadline_jobs, 0);
    assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 0);
    assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
    let playback_profile = diagnostics.decode_access_mode_profiles.playback_cursor;
    assert_eq!(playback_profile.canceled_jobs, 1);
    assert_eq!(playback_profile.canceled_prefetch_preempted_jobs, 1);
    assert_eq!(playback_profile.canceled_prefetch_deadline_jobs, 0);
    assert_eq!(playback_profile.canceled_return_latency_total_us, 40);
}

#[test]
fn preview_diagnostics_count_playback_deadline_cancellations_by_access_mode() {
    let service = WindowPreviewAdapter::new();

    service.record_preview_decode_cancel(
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(MediaPreviewCancelReason::PlaybackDeadline),
        None,
        0,
        Some(LogicalCancellationObserved {
            execution_elapsed_us: 0,
            request_elapsed_us: Some(0),
        }),
        true,
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_canceled_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_playback_deadline_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_prefetch_deadline_jobs, 0);
    assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 0);
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
    assert_eq!(
        diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
        1
    );
    assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
    let playback_profile = diagnostics.decode_access_mode_profiles.playback_cursor;
    assert_eq!(playback_profile.canceled_jobs, 1);
    assert_eq!(playback_profile.canceled_playback_deadline_jobs, 1);
    assert_eq!(playback_profile.canceled_prefetch_deadline_jobs, 0);
}

#[test]
fn preview_diagnostics_count_still_preemptions_by_access_mode() {
    let service = WindowPreviewAdapter::new();

    service.record_preview_decode_cancel(
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent),
        None,
        320,
        Some(LogicalCancellationObserved {
            execution_elapsed_us: 200,
            request_elapsed_us: Some(30),
        }),
        false,
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_canceled_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_still_preempted_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_obsolete_jobs, 0);
    assert_eq!(diagnostics.decode_canceled_random_access_still_jobs, 1);
    let still_profile = diagnostics.decode_access_mode_profiles.random_access_still;
    assert_eq!(still_profile.canceled_jobs, 1);
    assert_eq!(still_profile.canceled_still_preempted_jobs, 1);
    assert_eq!(still_profile.canceled_obsolete_jobs, 0);
    assert_eq!(still_profile.canceled_return_latency_total_us, 120);
}

fn test_preview_decode_work_classes(
    work_class: PreviewDecodeWorkClass,
    frames: u64,
    total_duration_us: u64,
    max_duration_us: u64,
    latency_buckets: PreviewDecodeWorkLatencyBuckets,
) -> PreviewDecodeWorkClassProfiles {
    let profile = PreviewDecodeWorkLatencyProfile {
        frames,
        total_duration_us,
        max_duration_us,
        latency_buckets,
    };
    match work_class {
        PreviewDecodeWorkClass::CacheHit => PreviewDecodeWorkClassProfiles {
            cache_hit: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
        PreviewDecodeWorkClass::SessionOpened => PreviewDecodeWorkClassProfiles {
            session_opened: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
        PreviewDecodeWorkClass::SessionReplaced => PreviewDecodeWorkClassProfiles {
            session_replaced: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
        PreviewDecodeWorkClass::ForwardSteady => PreviewDecodeWorkClassProfiles {
            forward_steady: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
        PreviewDecodeWorkClass::ReusedSeek => PreviewDecodeWorkClassProfiles {
            reused_seek: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
        PreviewDecodeWorkClass::ReusedOther => PreviewDecodeWorkClassProfiles {
            reused_other: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
        PreviewDecodeWorkClass::Unclassified => PreviewDecodeWorkClassProfiles {
            unclassified: profile,
            ..PreviewDecodeWorkClassProfiles::default()
        },
    }
}

fn test_zero_latency_preview_decode_work_classes(
    work_class: PreviewDecodeWorkClass,
    frames: u64,
) -> PreviewDecodeWorkClassProfiles {
    test_preview_decode_work_classes(
        work_class,
        frames,
        0,
        0,
        PreviewDecodeWorkLatencyBuckets {
            le_10ms: frames,
            ..PreviewDecodeWorkLatencyBuckets::default()
        },
    )
}

fn test_decode_latency_buckets_at(duration_us: u64, samples: u64) -> PreviewDecodeLatencyBuckets {
    let mut buckets = PreviewDecodeLatencyBuckets::default();
    match duration_us {
        0..=10_000 => buckets.le_10ms = samples,
        10_001..=16_000 => buckets.le_16ms = samples,
        16_001..=25_000 => buckets.le_25ms = samples,
        25_001..=40_000 => buckets.le_40ms = samples,
        40_001..=50_000 => buckets.le_50ms = samples,
        50_001..=60_000 => buckets.le_60ms = samples,
        60_001..=80_000 => buckets.le_80ms = samples,
        _ => buckets.gt_80ms = samples,
    }
    buckets
}

fn test_complete_successful_decode_evidence(
    mut profile: PreviewDecodeAccessModeProfile,
) -> PreviewDecodeAccessModeProfile {
    assert!(
        profile.frames > 0,
        "successful decode evidence requires at least one frame"
    );
    assert_eq!(
        profile.work_classes.total_frames(),
        profile.frames,
        "test fixture must classify every successful frame"
    );
    let lifecycle_frames = profile
        .session_opened_frames
        .saturating_add(profile.session_replaced_frames)
        .saturating_add(profile.session_reused_frames)
        .saturating_add(profile.session_bypassed_cache_frames)
        .saturating_add(profile.session_unclassified_frames);
    assert_eq!(
        lifecycle_frames, profile.frames,
        "test fixture must retain lifecycle evidence for every successful frame"
    );

    let queue_histogram_samples = profile.queue_wait_buckets.total();
    match (profile.queue_wait_samples, queue_histogram_samples) {
        (0, 0) => {
            profile.queue_wait_samples = profile.frames;
            profile.queue_wait_buckets =
                test_decode_latency_buckets_at(profile.queue_wait_max_us, profile.frames);
        }
        (0, samples) => profile.queue_wait_samples = samples,
        (samples, histogram_samples) => assert_eq!(
            samples, histogram_samples,
            "test fixture queue-wait sample and histogram accounting must agree"
        ),
    }
    assert!(
        profile.queue_wait_samples >= profile.frames,
        "every successful frame needs queue-wait evidence"
    );
    profile
}

#[test]
fn preview_decode_performance_report_fails_scrub_keyframe_seek_strategy() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_scrub_cursor_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                seeked_frames: 1,
                keyframe_seek_strategy_frames: 1,
                session_reused_frames: 1,
                work_classes: test_zero_latency_preview_decode_work_classes(
                    PreviewDecodeWorkClass::ReusedSeek,
                    1,
                ),
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-scrub-seek-strategy-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_bounded_any_seek_strategy"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 0
            && check.limit == Some(1)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_scrub_cursor_not_using_low_latency_seek"
            && root.evidence.contains("scrub_frames=1")
            && root.evidence.contains("keyframe_seek_strategy_frames=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "route_scrub_decode_through_bounded_any_seek"));
}

#[test]
fn preview_decode_performance_report_fails_scrub_without_any_seek_window() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_scrub_cursor_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                bounded_any_seek_strategy_frames: 1,
                forward_reuse_frame_window_max: 1,
                forward_decode_budget_frames_max: 8,
                any_seek_window_ms_max: 0,
                seeked_frames: 1,
                session_reused_frames: 1,
                work_classes: test_zero_latency_preview_decode_work_classes(
                    PreviewDecodeWorkClass::ReusedSeek,
                    1,
                ),
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-scrub-seek-window-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_any_seek_window_ms"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 0
            && check.limit == Some(1)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_scrub_cursor_missing_any_seek_window"
            && root.evidence.contains("scrub_frames=1")
            && root.evidence.contains("any_seek_window_ms_max=0")
            && root.evidence.contains("forward_decode_budget_frames_max=8")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "restore_scrub_bounded_any_seek_window"));
}

#[test]
fn preview_decode_performance_report_fails_structured_budget_exhaustion() {
    let diagnostics = PreviewDiagnostics {
        decode_failures: 1,
        decode_budget_exhausted_failures: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                failed_jobs: 1,
                budget_exhausted_failures: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_forward_budget_exhausted_failures"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_forward_budget_exhausted"
            && root.evidence.contains("scrub_budget_exhausted_failures=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_access_mode_forward_decode_budget"));
}

#[test]
fn preview_playback_schedule_diagnostics_records_clock_contract() {
    let service = WindowPreviewAdapter::new();

    service.record_playback_current_deadline_budget(Some(33_333));
    service.record_playback_forward_prefetch_window(Some(2));

    let diagnostics = service.diagnostics().playback_schedule;
    assert_eq!(diagnostics.last_current_deadline_budget_us, Some(33_333));
    assert_eq!(diagnostics.current_deadline_assignments, 1);
    assert_eq!(diagnostics.current_deadline_missing_frame_rate, 0);
    assert_eq!(diagnostics.current_decode_decisions, 1);
    assert_eq!(diagnostics.current_drop_late_decisions, 0);
    assert_eq!(
        diagnostics.current_proxy_or_hardware_recommended_decisions,
        0
    );
    assert_eq!(
        diagnostics.forward_prefetch_horizon_us,
        MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US
    );
    assert_eq!(diagnostics.last_forward_prefetch_window_frames, Some(2));
    assert_eq!(
        diagnostics.forward_prefetch_min_frames,
        MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES
    );
    assert_eq!(
        diagnostics.forward_prefetch_max_frames,
        MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES
    );
    assert_eq!(diagnostics.forward_prefetch_window_evaluations, 1);
    assert_eq!(diagnostics.forward_prefetch_invalid_frame_rate, 0);
    service.shutdown();
}

#[test]
fn startup_preroll_decode_evidence_is_separate_from_steady_state_latency() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut decode = test_preview_decode_diagnostics(
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewHardwareDecodeDecision::GpuResidentNative,
        PreviewHardwareDecodeBlocker::None,
    );
    decode.elapsed_us = 280_000;

    service.record_startup_preroll_decode(decode, 210_000);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_startup_preroll_frames, 1);
    assert_eq!(
        diagnostics.decode_startup_preroll_total_duration_us,
        280_000
    );
    assert_eq!(diagnostics.decode_startup_preroll_max_duration_us, 280_000);
    assert_eq!(
        diagnostics.decode_startup_preroll_queue_wait_total_us,
        210_000
    );
    assert_eq!(
        diagnostics.decode_startup_preroll_queue_wait_max_us,
        210_000
    );
    assert_eq!(diagnostics.decode_total_duration_us, 0);
    assert_eq!(diagnostics.decode_queue_wait_total_us, 0);
    assert_eq!(
        diagnostics.decode_access_mode_profiles.playback_cursor.frames,
        0
    );

    service.shutdown();
}

#[test]
fn playback_video_preroll_requires_next_media_payload_and_observes_cache_residency() {
    let (mut state, asset_id, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let sequence = state.active_sequence().expect("media sequence");
    let _preroll_window =
        media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
            .expect("valid media sequence frame rate");

    assert_eq!(
        playback_video_preroll_for_state(&service, &state),
        Some(PreviewVideoPreroll { ready_media_frames: 0, preservable_media_frames: 4 })
    );
    assert_eq!(
        service.jobs.diagnostics().queued_prefetch_jobs,
        4,
        "preroll observation must actively admit its bounded future prefix; residency is charged at the decode representation extent (source raster)"
    );

    let frame = state.current_frame().saturating_add(1);
    let program = PreparedVisualProgram::prepare(sequence).expect("next frame visual program");
    let evaluation = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::preview(
            mondrian_core::FramePosition::new(frame, sequence.time_base()),
            normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
        ),
    )
    .expect("next frame render plan");
    let media = evaluation
        .elements
        .into_iter()
        .find_map(|element| match element {
            TimelineRenderPlanElement::Media(media) => Some(media),
            _ => None,
        })
        .expect("next frame media");
    assert_eq!(media.asset_id, asset_id);
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .expect("valid test context")
        .media_input(media.auto_tone_map);
    let key = service
        .media_preview_key_for_asset(
            &snapshot,
            &state,
            &media.asset_id,
            media.color_space_override,
            media.alpha_interpretation,
            media.source_sample.time(),
            width,
            height,
            mondrian_playback::PreviewResolutionScale::Full,
            &input_color,
            false,
            false,
        )
        .expect("next media cache key");
    admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        key,
        test_media_frame(7),
        MediaPreviewRequestPriority::Prefetch,
    );

    assert_eq!(
        playback_video_preroll_for_state(&service, &state),
        Some(PreviewVideoPreroll { ready_media_frames: 1, preservable_media_frames: 4 })
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

fn media_preview_key_for_simple_sequence_frame<O: Clone>(
    service: &PreviewProductionRuntime<O>,
    state: &AppState,
    frame: i64,
) -> MediaPreviewKey {
    media_preview_key_for_simple_sequence_frame_at_scale(
        service,
        state,
        frame,
        mondrian_playback::PreviewResolutionScale::Full,
    )
}

fn media_preview_key_for_simple_sequence_frame_at_scale<O: Clone>(
    service: &PreviewProductionRuntime<O>,
    state: &AppState,
    frame: i64,
    runtime_scale: mondrian_playback::PreviewResolutionScale,
) -> MediaPreviewKey {
    let sequence = state.active_sequence().expect("media sequence");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    let prepared = PreparedVisualProgram::prepare(sequence).expect("prepared visual program");
    let evaluation = evaluate_prepared_visual_program(
        &prepared,
        TimelineEvaluationRequest::preview(
            mondrian_core::FramePosition::new(frame, sequence.time_base()),
            normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
        ),
    )
    .expect("future frame");
    let media = evaluation
        .elements
        .into_iter()
        .find_map(|element| match element {
            TimelineRenderPlanElement::Media(media) => Some(media),
            _ => None,
        })
        .expect("future media");
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .expect("valid test context")
        .media_input(media.auto_tone_map);
    service
        .media_preview_key_for_asset(
            &snapshot,
            state,
            &media.asset_id,
            media.color_space_override,
            media.alpha_interpretation,
            media.source_sample.time(),
            width,
            height,
            runtime_scale,
            &input_color,
            false,
            false,
        )
        .expect("future media key")
}

fn future_media_prefix_keys_for_state<O: Clone>(
    service: &PreviewProductionRuntime<O>,
    state: &AppState,
    current_frame: i64,
    max_future_frames: usize,
    target_resolution: Resolution,
) -> Vec<MediaPreviewKey> {
    let sequence = state.active_sequence().expect("media sequence");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let color_context = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .expect("valid test context");
    service.future_media_prefix_keys_for_test(
        &snapshot,
        state,
        sequence,
        current_frame,
        max_future_frames,
        target_resolution,
        color_context,
    )
}

#[test]
fn future_media_window_reuses_sliding_semantic_and_lowered_frame_contracts() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let target_resolution = Resolution { width: 64, height: 36 };
    let window = MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES;
    let sequential_frames = 20usize;

    let first = future_media_prefix_keys_for_state(&service, &state, 0, window, target_resolution);
    let cached = future_media_prefix_keys_for_state(&service, &state, 0, window, target_resolution);
    assert_eq!(
        cached, first,
        "cached lowering must equal a fresh canonical plan"
    );

    for current_frame in 1..sequential_frames as i64 {
        let keys = future_media_prefix_keys_for_state(
            &service,
            &state,
            current_frame,
            window,
            target_resolution,
        );
        assert_eq!(
            keys.len(),
            first.len(),
            "the physically admissible window is bounded by the decode representation extent (source raster), not the frame-rate window"
        );
    }

    let diagnostics = service.diagnostics().future_media_window;
    let evaluation_ceiling = sequential_frames.saturating_add(window).saturating_sub(1) as u64;
    assert!(
        diagnostics.semantic_frame_evaluations <= evaluation_ceiling,
        "sequential playback must not re-evaluate every window frame: {} <= {evaluation_ceiling}",
        diagnostics.semantic_frame_evaluations,
    );
    assert!(
        diagnostics.media_request_lowerings <= evaluation_ceiling,
        "one-media-layer frames must not lower every window frame"
    );
    assert!(
        diagnostics.cache_hits > 0,
        "the overlapping sliding window must reuse lowered contracts instead of re-lowering every frame"
    );

    service.clear_future_media_window_for_test();
    let fresh = future_media_prefix_keys_for_state(&service, &state, 0, window, target_resolution);
    assert_eq!(
        fresh, first,
        "evicting the optimization must not change canonical lowered request identity"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn reverse_future_media_prefix_follows_transport_order_toward_sequence_start() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.seek(10).expect("reverse start");
    state
        .shuttle(mondrian_playback::PlaybackShuttleDirection::Reverse)
        .expect("reverse transport");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let target_resolution = Resolution { width: 64, height: 36 };

    let keys = future_media_prefix_keys_for_state(&service, &state, 10, 3, target_resolution);
    let expected = [9, 8, 7]
        .map(|frame| media_preview_key_for_simple_sequence_frame(&service, &state, frame))
        .into_iter()
        .collect::<Vec<_>>();

    assert!(
        keys.len() >= 2,
        "reverse prefix must include adjacent capacity"
    );
    assert_eq!(keys, expected[..keys.len()]);
    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn future_media_window_revalidates_each_physical_source_once_per_planning_turn() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let target_resolution = Resolution { width: 64, height: 36 };
    let window = MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES;

    let first = future_media_prefix_keys_for_state(&service, &state, 0, window, target_resolution);
    assert_eq!(first.len(), 4);
    let populated = service.diagnostics().future_media_window;
    assert_eq!(
        populated.source_fingerprint_observations, 0,
        "fresh lowering already binds the observed source revision and needs no cache revalidation"
    );

    let second = future_media_prefix_keys_for_state(&service, &state, 0, window, target_resolution);
    assert_eq!(second, first);
    let first_reuse = service.diagnostics().future_media_window;
    assert_eq!(
        first_reuse.source_fingerprint_observations, 1,
        "one planning turn must observe one shared physical source once, not once per cached frame"
    );

    let third = future_media_prefix_keys_for_state(&service, &state, 0, window, target_resolution);
    assert_eq!(third, first);
    let second_reuse = service.diagnostics().future_media_window;
    assert_eq!(
        second_reuse
            .source_fingerprint_observations
            .saturating_sub(first_reuse.source_fingerprint_observations),
        1,
        "a later planning turn must reobserve the physical source instead of reusing prior-turn evidence"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn future_media_window_fails_closed_when_a_retained_source_revision_drifts() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let target_resolution = Resolution { width: 64, height: 36 };

    let retained = future_media_prefix_keys_for_state(&service, &state, 0, 1, target_resolution);
    assert_eq!(retained.len(), 1);

    std::fs::write(
        root.join("source.mp4"),
        b"a physically replaced invalid media source",
    )
    .expect("replace retained source");
    let after_drift = future_media_prefix_keys_for_state(&service, &state, 0, 1, target_resolution);
    assert!(
        after_drift.is_empty(),
        "a retained decode contract must not authorize a physically replaced source"
    );
    assert_eq!(
        service.diagnostics().future_media_window.source_fingerprint_observations,
        1,
        "the failed retained-contract check must remain observable"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn future_media_window_invalidates_scale_extent_color_revision_and_library_edges() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let first_target = Resolution { width: 64, height: 36 };
    let second_target = Resolution { width: 96, height: 54 };

    {
        let sequence = state.active_sequence().expect("media sequence");
        let snapshot = state.preview_execution_snapshot(Instant::now());
        let color_context = sequence
            .settings
            .root_program_color_context(state.project_color_environment())
            .expect("valid test context");
        let full = service
            .future_media_frame_keys_at_scale_for_test(
                &snapshot,
                &state,
                sequence,
                1,
                mondrian_playback::PreviewResolutionScale::Full,
                first_target,
                color_context.clone(),
            )
            .expect("full-scale media contract");
        let full_cached = service
            .future_media_frame_keys_at_scale_for_test(
                &snapshot,
                &state,
                sequence,
                1,
                mondrian_playback::PreviewResolutionScale::Full,
                first_target,
                color_context.clone(),
            )
            .expect("cached full-scale media contract");
        assert_eq!(full_cached, full);

        let half = service
            .future_media_frame_keys_at_scale_for_test(
                &snapshot,
                &state,
                sequence,
                1,
                mondrian_playback::PreviewResolutionScale::Half,
                first_target,
                color_context.clone(),
            )
            .expect("half-scale media contract");
        assert_eq!(
            half.len(),
            full.len(),
            "runtime scale changes execution semantics, not dependency cardinality"
        );
        assert_ne!(
            half, full,
            "runtime recovery scale must rotate the media representation identity"
        );
        assert!(full.iter().all(|key| matches!(
            key.decode.representation(),
            mondrian_media::PreviewDecodeRepresentation::NativeCpu
                | mondrian_media::PreviewDecodeRepresentation::NativeSurface
                | mondrian_media::PreviewDecodeRepresentation::Proxy(_)
        )));
        assert!(half.iter().all(|key| matches!(
            key.decode.representation(),
            mondrian_media::PreviewDecodeRepresentation::Reduced { divisor }
                if divisor.get() == 2
        )));

        let resized = service
            .future_media_frame_keys_at_scale_for_test(
                &snapshot,
                &state,
                sequence,
                1,
                mondrian_playback::PreviewResolutionScale::Half,
                second_target,
                color_context.clone(),
            )
            .expect("resized media contract");
        assert_eq!(
            resized, half,
            "output extent is a composition/spatial target and never participates in the decode identity: the decode representation and cache identity are unchanged across output extents"
        );

        let alternate_environment =
            mondrian_core::ProjectColorEnvironment::new(ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            });
        let alternate_color = sequence
            .settings
            .root_program_color_context(&alternate_environment)
            .expect("valid alternate color context");
        let recolored = service
            .future_media_frame_keys_at_scale_for_test(
                &snapshot,
                &state,
                sequence,
                1,
                mondrian_playback::PreviewResolutionScale::Half,
                second_target,
                alternate_color,
            )
            .expect("alternate-color media contract");
        assert_ne!(
            recolored, resized,
            "Program color changes must produce a freshly lowered media contract"
        );
    }

    let next_revision = state
        .active_sequence()
        .expect("media sequence")
        .revision
        .checked_next()
        .expect("test Sequence revision can advance");
    state.active_sequence_mut_uncommitted().expect("media sequence").revision = next_revision;
    let _ = future_media_prefix_keys_for_state(&service, &state, 0, 1, second_target);

    state
        .asset_library()
        .expect("asset library")
        .create_folder("future-window-revision", None)
        .expect("advance Asset Library revision");
    let _ = future_media_prefix_keys_for_state(&service, &state, 0, 1, second_target);

    let diagnostics = service.diagnostics().future_media_window;
    assert_eq!(
        diagnostics.cache_hits, 1,
        "only the exact repeated full-scale request may hit"
    );
    assert!(
        diagnostics.identity_invalidations >= 5,
        "scale, extent, color, Sequence revision, and Asset Library revision must each rotate the window"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn future_media_prefix_preserves_nearest_resident_across_preroll_and_prefetch() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.frame_store.replace(PreviewFrameStoreAdapter::new(
        PreviewFrameStoreAdapterConfig {
            media_entry_capacity: 1,
            media_byte_budget: 256 * 1024 * 1024,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 1,
            current_media_working_set_byte_limit: 256 * 1024 * 1024,
            current_media_working_set_resource_unit_limit: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 256 * 1024 * 1024,
            failure_entry_capacity: 2,
        },
    ));
    let sequence = state.active_sequence().expect("media sequence");
    let nearest_key = media_preview_key_for_simple_sequence_frame(
        &service,
        &state,
        state.current_frame().saturating_add(1),
    );
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        nearest_key.clone(),
        test_media_frame(11),
        MediaPreviewRequestPriority::Prefetch,
    ));
    let evictions_before =
        service.frame_store.borrow().diagnostics().media_work_reservation_evictions;

    assert_eq!(
        playback_video_preroll_for_state(&service, &state),
        Some(PreviewVideoPreroll { ready_media_frames: 1, preservable_media_frames: 1 }),
        "preroll must expose only the physically preservable near-term prefix"
    );
    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    assert!(
        service.frame_store.borrow_mut().media_frame(&nearest_key).is_some(),
        "farther Prefetch work must not evict the accepted nearest resident frame"
    );
    assert_eq!(service.jobs.diagnostics().queued_prefetch_jobs, 0);
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_work_reservation_evictions,
        evictions_before,
        "planning must protect the resident prefix before computing speculative headroom"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn future_media_prefix_drops_a_far_guard_before_admitting_nearer_missing_work() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.frame_store.replace(PreviewFrameStoreAdapter::new(
        PreviewFrameStoreAdapterConfig {
            media_entry_capacity: 1,
            media_byte_budget: 256 * 1024 * 1024,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 1,
            current_media_working_set_byte_limit: 256 * 1024 * 1024,
            current_media_working_set_resource_unit_limit: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 256 * 1024 * 1024,
            failure_entry_capacity: 2,
        },
    ));
    let current_frame = state.current_frame();
    let near_key = media_preview_key_for_simple_sequence_frame(&service, &state, current_frame + 1);
    let far_key = media_preview_key_for_simple_sequence_frame(&service, &state, current_frame + 2);
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        far_key.clone(),
        test_media_frame(12),
        MediaPreviewRequestPriority::Prefetch,
    ));
    let evictions_before =
        service.frame_store.borrow().diagnostics().media_work_reservation_evictions;
    let sequence = state.active_sequence().expect("media sequence");

    schedule_media_prefetches_for_state(&service, &state, sequence, current_frame);

    assert!(
        service.scheduler.has_pending_key(&near_key),
        "the nearest missing frame must own the only speculative reservation"
    );
    assert_eq!(service.jobs.diagnostics().queued_prefetch_jobs, 1);
    assert!(
        service.frame_store.borrow_mut().media_frame(&far_key).is_none(),
        "a rejected farther resident guard must not block nearer missing work"
    );
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_work_reservation_evictions,
        evictions_before + 1
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn future_media_prefix_commits_nearest_first_lru_priority_after_preroll_inspection() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.frame_store.replace(PreviewFrameStoreAdapter::new(
        PreviewFrameStoreAdapterConfig {
            media_entry_capacity: 2,
            media_byte_budget: 256 * 1024 * 1024,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 2,
            current_media_working_set_byte_limit: 256 * 1024 * 1024,
            current_media_working_set_resource_unit_limit: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 256 * 1024 * 1024,
            failure_entry_capacity: 2,
        },
    ));
    let current_frame = state.current_frame();
    let near_key = media_preview_key_for_simple_sequence_frame(&service, &state, current_frame + 1);
    let far_key = media_preview_key_for_simple_sequence_frame(&service, &state, current_frame + 2);
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        far_key.clone(),
        test_media_frame(13),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        near_key.clone(),
        test_media_frame(14),
        MediaPreviewRequestPriority::Prefetch,
    ));

    assert_eq!(
        playback_video_preroll_for_state(&service, &state),
        Some(PreviewVideoPreroll { ready_media_frames: 2, preservable_media_frames: 2 })
    );
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        test_media_key(3_003),
        test_media_frame(15),
        MediaPreviewRequestPriority::Prefetch,
    ));

    assert!(
        service.frame_store.borrow_mut().media_frame(&far_key).is_none(),
        "later resource pressure should evict the farther future frame first"
    );
    assert!(
        service.frame_store.borrow_mut().media_frame(&near_key).is_some(),
        "preroll inspection must leave the nearest future frame hottest"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn playback_video_preroll_does_not_delay_procedural_future_frames() {
    let mut state = state_with_solid_color_clip(Color::WHITE);
    state.seek(0).expect("seek");
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();

    assert_eq!(
        playback_video_preroll_for_state(&service, &state),
        Some(PreviewVideoPreroll { ready_media_frames: 0, preservable_media_frames: 0 })
    );

    service.shutdown();
}

#[test]
fn preview_playback_schedule_counts_native_import_unavailable_current_frames() {
    let service = WindowPreviewAdapter::new();
    service.set_playback_hardware_decode_admission(PlaybackHardwareDecodeAdmission {
        request: PreviewHardwareDecodeRequest::PreferHardwareDecode,
        hardware_decode_device_selector: None,
        renderer_native_import_ready: false,
        renderer_import_mode: None,
        native_import_admission_ready: false,
        admission_blocker: Some(PreviewHardwareDecodeAdmissionBlocker::RendererImportUnavailable),
        renderer_supported_handle_kinds: 0,
        renderer_supported_source_texture_formats: 0,
        renderer_supports_nv12: false,
        renderer_supports_p010: false,
        renderer_supported_surface_hint_mask: 0,
    });

    service.record_preview_decode(
        test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable,
            PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
        ),
        MediaPreviewRequestPriority::Current,
        0,
        true,
    );
    service.record_preview_decode(
        test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable,
            PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
        ),
        MediaPreviewRequestPriority::Current,
        0,
        true,
    );
    let mut effective_cpu_transfer = test_preview_decode_diagnostics(
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer,
        PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
    );
    effective_cpu_transfer.hardware_decode_cpu_transfer_observed = true;
    service.record_preview_decode(
        effective_cpu_transfer,
        MediaPreviewRequestPriority::Current,
        0,
        true,
    );

    let diagnostics = service.diagnostics().playback_schedule;
    assert_eq!(diagnostics.current_native_import_unavailable_decisions, 2);
    assert_eq!(
        diagnostics.current_hardware_fallback_not_engaged_decisions,
        1
    );
    assert_eq!(
        diagnostics.current_proxy_or_hardware_recommended_decisions,
        2
    );
    service.shutdown();
}

#[test]
fn preview_decode_performance_report_warns_invalid_playback_clock_contract() {
    let diagnostics = PreviewDiagnostics {
        decode_canceled_jobs: 1,
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_deadline_missing_frame_rate: 1,
            forward_prefetch_invalid_frame_rate: 1,
            forward_prefetch_horizon_us: MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US,
            forward_prefetch_min_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
            forward_prefetch_max_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-clock-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_deadline_invalid_frame_rate"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_prefetch_window_invalid_frame_rate"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_deadline_invalid_frame_rate"
            && root.evidence.contains("current_deadline_missing_frame_rate=1")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_prefetch_window_invalid_frame_rate"
            && root.evidence.contains("forward_prefetch_invalid_frame_rate=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "fix_sequence_playback_frame_rate_contract"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "fix_sequence_prefetch_frame_rate_contract"));
}

#[test]
fn preview_decode_performance_report_fails_structured_timeout_failures() {
    let diagnostics = PreviewDiagnostics {
        decode_failures: 1,
        decode_timeout_failures: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                failed_jobs: 1,
                timeout_failures: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(
        report.checks.iter().any(|check| check.code == "preview_decode_timeout_failures"
            && check.severity == PreviewDecodePerformanceSeverity::Fail)
    );
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_timeout_failures"
            && root.evidence.contains("scrub_timeout_failures=1")));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_access_mode_decode_timeout_budget"));
}

#[test]
fn preview_decode_performance_report_defaults_to_no_required_access_modes() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    session_opened_frames: 1,
                    work_classes: test_zero_latency_preview_decode_work_classes(
                        PreviewDecodeWorkClass::SessionOpened,
                        1,
                    ),
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-default-coverage-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.required_access_modes.is_empty());
    assert!(!report
        .checks
        .iter()
        .any(|check| check.code == "preview_decode_scrub_cursor_sampled"));
}

#[test]
fn preview_decode_performance_report_fails_missing_required_access_modes() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                session_opened_frames: 1,
                work_classes: test_zero_latency_preview_decode_work_classes(
                    PreviewDecodeWorkClass::SessionOpened,
                    1,
                ),
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-required-coverage-test",
        50_000,
        &[
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ],
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert_eq!(
        report.required_access_modes,
        vec![
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ]
    );
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_sampled"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 0
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_random_access_still_sampled"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
            && check.observed == 1
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_required_access_mode_missing"
            && root.evidence.contains("access_mode=ScrubCursor")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "exercise_required_preview_access_modes"));
}

#[test]
fn preview_decode_performance_report_accepts_playback_ring_as_mode_local_evidence() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_playback_session_ring_hit_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                playback_session_ring_hit_frames: 1,
                session_bypassed_cache_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    cache_hit: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-required-playback-ring-test",
        50_000,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_cursor_mode_local_sampled"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
            && check.observed == 1
    }));
    assert!(!report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_work_class_accounting_mismatch"));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_required_work_class_missing"
            && root.evidence.contains("work_class=ForwardSteady")
    }));
}

#[test]
fn preview_decode_performance_report_classifies_codec_bound_slow_frame() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_playback_cursor_frames: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 120_000,
        decode_max_duration_us: 120_000,
        decode_last_duration_us: 120_000,
        decode_decoded_frame_count: 36,
        decode_max_decoded_frame_count: 36,
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 95_000,
            seek_us: 10_000,
            swscale_us: 8_000,
            rgba_copy_us: 2_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 95_000,
            seek_us: 10_000,
            swscale_us: 8_000,
            rgba_copy_us: 2_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                total_duration_us: 120_000,
                max_duration_us: 120_000,
                last_duration_us: 120_000,
                session_reused_frames: 1,
                forward_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    forward_steady: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        total_duration_us: 120_000,
                        max_duration_us: 120_000,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_120ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                decoded_frame_count: 36,
                max_decoded_frame_count: 36,
                stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 95_000,
                    seek_us: 10_000,
                    swscale_us: 8_000,
                    rgba_copy_us: 2_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 95_000,
                    seek_us: 10_000,
                    swscale_us: 8_000,
                    rgba_copy_us: 2_000,
                    ..PreviewDecodeStageDurations::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    let summary = report.summary.expect("decode summary");
    assert_eq!(
        summary.primary_bottleneck,
        PreviewDecodeBottleneck::PacketDecode
    );
    assert_eq!(
        summary.slowest_access_mode,
        Some(PreviewDecodeAccessMode::PlaybackCursor)
    );
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_cursor_forward_steady_max_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
            && check.observed == 120_000
            && check.limit == Some(250_000)
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_cursor_forward_steady_p95_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 120_000
            && check.limit == Some(50_000)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_work_class_over_budget"
            && root.area == PreviewDecodePerformanceArea::AccessMode
            && root.evidence.contains("access_mode=PlaybackCursor")
            && root.evidence.contains("work_class=ForwardSteady")
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_codec_or_gop_bound"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "enable_proxy_or_hardware_decode"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_decode_work_class"));
}

#[test]
fn preview_decode_report_separates_cold_session_readiness_from_steady_cadence() {
    let profile = test_complete_successful_decode_evidence(PreviewDecodeAccessModeProfile {
        frames: 2,
        total_duration_us: 1_510_000,
        max_duration_us: 1_500_000,
        latency_buckets: PreviewDecodeLatencyBuckets {
            le_10ms: 1,
            gt_80ms: 1,
            ..PreviewDecodeLatencyBuckets::default()
        },
        session_opened_frames: 1,
        session_reused_frames: 1,
        seeked_frames: 1,
        work_classes: PreviewDecodeWorkClassProfiles {
            session_opened: PreviewDecodeWorkLatencyProfile {
                frames: 1,
                total_duration_us: 1_500_000,
                max_duration_us: 1_500_000,
                latency_buckets: PreviewDecodeWorkLatencyBuckets {
                    le_2s: 1,
                    ..PreviewDecodeWorkLatencyBuckets::default()
                },
            },
            reused_seek: PreviewDecodeWorkLatencyProfile {
                frames: 1,
                total_duration_us: 10_000,
                max_duration_us: 10_000,
                latency_buckets: PreviewDecodeWorkLatencyBuckets {
                    le_10ms: 1,
                    ..PreviewDecodeWorkLatencyBuckets::default()
                },
            },
            ..PreviewDecodeWorkClassProfiles::default()
        },
        bounded_any_seek_strategy_frames: 2,
        any_seek_window_ms_max: 500,
        max_frame_stage_durations: PreviewDecodeStageDurations {
            session_open_us: 1_490_000,
            packet_decode_us: 10_000,
            ..PreviewDecodeStageDurations::default()
        },
        ..PreviewDecodeAccessModeProfile::default()
    });
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_total_duration_us: 1_510_000,
        decode_max_duration_us: 1_500_000,
        decode_last_duration_us: 10_000,
        decode_max_frame_stage_durations: profile.max_frame_stage_durations,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: profile,
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-cold-steady-partition",
        50_000,
        &[PreviewDecodeAccessMode::ScrubCursor],
    );

    assert_ne!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_reused_seek_max_worker_execution_us"
            && check.observed == 10_000
            && check.limit == Some(500_000)
            && check.severity == PreviewDecodePerformanceSeverity::Pass
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_session_opened_max_worker_execution_us"
            && check.observed == 1_500_000
            && check.limit == Some(PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US)
            && check.severity == PreviewDecodePerformanceSeverity::Pass
    }));
    assert!(!report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_work_class_over_budget"));
}

#[test]
fn preview_decode_report_fails_unbounded_cold_session_readiness() {
    let profile = PreviewDecodeAccessModeProfile {
        frames: 2,
        total_duration_us: PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US + 10_001,
        max_duration_us: PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US + 1,
        session_opened_frames: 1,
        session_reused_frames: 1,
        seeked_frames: 1,
        work_classes: PreviewDecodeWorkClassProfiles {
            session_opened: PreviewDecodeWorkLatencyProfile {
                frames: 1,
                total_duration_us: PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US + 1,
                max_duration_us: PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US + 1,
                latency_buckets: PreviewDecodeWorkLatencyBuckets {
                    gt_5s: 1,
                    ..PreviewDecodeWorkLatencyBuckets::default()
                },
            },
            reused_seek: PreviewDecodeWorkLatencyProfile {
                frames: 1,
                total_duration_us: 10_000,
                max_duration_us: 10_000,
                latency_buckets: PreviewDecodeWorkLatencyBuckets {
                    le_10ms: 1,
                    ..PreviewDecodeWorkLatencyBuckets::default()
                },
            },
            ..PreviewDecodeWorkClassProfiles::default()
        },
        ..PreviewDecodeAccessModeProfile::default()
    };
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_total_duration_us: profile.total_duration_us,
        decode_max_duration_us: profile.max_duration_us,
        decode_last_duration_us: profile.max_duration_us,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: profile,
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-unbounded-cold-open",
        50_000,
        &[PreviewDecodeAccessMode::RandomAccessStillFrame],
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_random_access_still_session_opened_max_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_work_class_over_budget"
            && root.evidence.contains("access_mode=RandomAccessStillFrame")
            && root.evidence.contains("work_class=SessionOpened")
    }));
}

#[test]
fn preview_decode_performance_report_classifies_hardware_transfer_bound_frame() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_playback_cursor_frames: 1,
        decode_total_duration_us: 90_000,
        decode_max_duration_us: 90_000,
        decode_last_duration_us: 90_000,
        decode_stage_durations: PreviewDecodeStageDurations {
            hardware_transfer_us: 70_000,
            swscale_us: 8_000,
            rgba_copy_us: 2_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            hardware_transfer_us: 70_000,
            swscale_us: 8_000,
            rgba_copy_us: 2_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                total_duration_us: 90_000,
                max_duration_us: 90_000,
                last_duration_us: 90_000,
                session_reused_frames: 1,
                work_classes: test_preview_decode_work_classes(
                    PreviewDecodeWorkClass::ReusedOther,
                    1,
                    90_000,
                    90_000,
                    PreviewDecodeWorkLatencyBuckets {
                        le_120ms: 1,
                        ..PreviewDecodeWorkLatencyBuckets::default()
                    },
                ),
                hardware_decode_cpu_transfer_observed_frames: 1,
                stage_durations: PreviewDecodeStageDurations {
                    hardware_transfer_us: 70_000,
                    swscale_us: 8_000,
                    rgba_copy_us: 2_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    hardware_transfer_us: 70_000,
                    swscale_us: 8_000,
                    rgba_copy_us: 2_000,
                    ..PreviewDecodeStageDurations::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-hw-transfer-test",
        50_000,
    );

    let summary = report.summary.expect("decode summary");
    assert_eq!(
        summary.primary_bottleneck,
        PreviewDecodeBottleneck::HardwareTransfer
    );
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_cursor_reused_other_max_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 90_000
            && check.limit == Some(50_000)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_work_class_over_budget"
            && root.evidence.contains("access_mode=PlaybackCursor")
            && root.evidence.contains("work_class=ReusedOther")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_hardware_transfer_bound"
            && root.evidence.contains("hardware_transfer_us=70000")
            && root.evidence.contains("hardware_decode_cpu_transfer_observed_frames=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "connect_native_decoder_surface_import"));
}

#[test]
fn preview_decode_performance_report_flags_hardware_cpu_transfer_setup_failures() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_playback_cursor_frames: 2,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 2,
                session_opened_frames: 1,
                session_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    session_opened: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    reused_other: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                hardware_decode_prefer_hardware_requested_frames: 2,
                hardware_decode_device_context_attempted_frames: 2,
                hardware_decode_device_context_created_frames: 1,
                hardware_decode_backend_unavailable_frames: 2,
                hardware_decode_cpu_transfer_setup_failed_frames: 1,
                hardware_decode_cpu_transfer_decoder_open_failed_frames: 1,
                hardware_decode_cpu_transfer_configured_frames: 1,
                hardware_decode_cpu_transfer_observed_frames: 0,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-hw-transfer-setup-failure-test",
        50_000,
    );

    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_hardware_cpu_transfer_setup_failed"
            && root.area == PreviewDecodePerformanceArea::CodecDecode
            && root.evidence.contains("setup_failed_frames=1")
            && root.evidence.contains("decoder_open_failed_frames=1")
            && root.evidence.contains("device_context_created_frames=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "diagnose_ffmpeg_hardware_decode_setup"));
}

#[test]
fn preview_decode_performance_report_flags_unengaged_playback_hardware_fallback() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 3,
        decode_playback_cursor_frames: 3,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 3,
                    session_opened_frames: 1,
                    session_reused_frames: 2,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        session_opened: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                            ..PreviewDecodeWorkLatencyProfile::default()
                        },
                        reused_other: PreviewDecodeWorkLatencyProfile {
                            frames: 2,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 2,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                            ..PreviewDecodeWorkLatencyProfile::default()
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    hardware_decode_prefer_hardware_requested_frames: 3,
                    hardware_decode_backend_unavailable_frames: 1,
                    hardware_decode_codec_unsupported_frames: 1,
                    hardware_decode_device_context_attempted_frames: 1,
                    hardware_decode_device_context_unavailable_frames: 1,
                    hardware_decode_cpu_transfer_setup_failed_frames: 1,
                    hardware_decode_cpu_transfer_observed_frames: 0,
                    hardware_decode_gpu_resident_native_frames: 0,
                    hardware_decode_candidate_d3d12va_frames: 1,
                    hardware_decode_candidate_d3d11va_frames: 1,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-hw-fallback-not-engaged-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_hardware_fallback_not_engaged"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 3
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_hardware_fallback_not_engaged"
            && root.area == PreviewDecodePerformanceArea::CodecDecode
            && root.evidence.contains("requested_frames=3")
            && root.evidence.contains("effective_frames=0")
            && root.evidence.contains("not_engaged_frames=3")
            && root.evidence.contains("backend_unavailable_frames=1")
            && root.evidence.contains("codec_unsupported_frames=1")
            && root.evidence.contains("device_context_unavailable_frames=1")
            && root.evidence.contains("candidate_d3d12va_frames=1")
            && root.evidence.contains("candidate_d3d11va_frames=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "recover_playback_hardware_decode_fallback"));
}

#[test]
fn preview_decode_performance_report_accepts_engaged_playback_hardware_fallback() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_playback_cursor_frames: 2,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 2,
                session_replaced_frames: 1,
                session_reused_frames: 1,
                forward_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    session_replaced: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    forward_steady: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                hardware_decode_prefer_hardware_requested_frames: 2,
                hardware_decode_cpu_transfer_observed_frames: 1,
                hardware_decode_gpu_resident_native_frames: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-hw-fallback-engaged-test",
        50_000,
    );

    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_hardware_fallback_not_engaged"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
            && check.observed == 0
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_cursor_session_replaced_max_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
            && check.observed == 0
            && check.limit == Some(PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US)
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_cursor_forward_steady_max_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
            && check.observed == 0
            && check.limit == Some(250_000)
    }));
    assert!(!report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_playback_hardware_fallback_not_engaged"));
}

#[test]
fn preview_decode_performance_report_flags_hardware_fallback_recovery_decisions() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_playback_cursor_frames: 2,
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_hardware_fallback_not_engaged_decisions: 2,
            current_proxy_or_hardware_recommended_decisions: 2,
            current_drop_late_decisions: 1,
            current_proxy_generation_requests: 1,
            current_proxy_generation_request_dedupes: 1,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 2,
                    session_opened_frames: 1,
                    session_reused_frames: 1,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        session_opened: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                            ..PreviewDecodeWorkLatencyProfile::default()
                        },
                        reused_other: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                            ..PreviewDecodeWorkLatencyProfile::default()
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    hardware_decode_prefer_hardware_requested_frames: 2,
                    hardware_decode_backend_unavailable_frames: 1,
                    hardware_decode_codec_unsupported_frames: 1,
                    hardware_decode_cpu_transfer_observed_frames: 0,
                    hardware_decode_gpu_resident_native_frames: 0,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-hw-fallback-recovery-decisions-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_hardware_fallback_not_engaged_decisions"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 2
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_hardware_fallback_recovery_decisions"
            && root.area == PreviewDecodePerformanceArea::Scheduling
            && root.evidence.contains("current_hardware_fallback_not_engaged_decisions=2")
            && root.evidence.contains("current_proxy_generation_requests=1")
            && root.evidence.contains("current_proxy_generation_request_dedupes=1")
            && root.evidence.contains("playback_requested_frames=2")
            && root.evidence.contains("playback_effective_hardware_frames=0")
            && root.evidence.contains("playback_backend_unavailable_frames=1")
            && root.evidence.contains("playback_codec_unsupported_frames=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "recover_playback_hardware_decode_fallback"));
}

#[test]
fn preview_decode_performance_report_classifies_queue_wait_bound_frame() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        decode_queue_wait_total_us: 95_000,
        decode_queue_wait_max_us: 95_000,
        decode_queue_wait_last_us: 95_000,
        decode_current_queue_wait_max_us: 95_000,
        decode_prefetch_queue_wait_max_us: 15_000,
        enqueued_jobs: 4,
        queue_evicted_prefetch_jobs: 1,
        interactive_cancel_requests: 1,
        interactive_cancel_scheduler_requests: 2,
        interactive_cancel_queued_jobs: 3,
        queue_canceled_jobs: 3,
        queue_pruned_obsolete_jobs: 2,
        queue_promoted_current_jobs: 1,
        worker_queue: MediaPreviewJobQueueDiagnostics {
            queued_jobs: 3,
            in_flight_jobs: 2,
            in_flight_completed_jobs: 0,
            in_flight_cancellation_requested_jobs: 0,
            in_flight_max_age_us: 0,
            in_flight_cancellation_max_age_us: 0,
            queued_current_jobs: 2,
            in_flight_current_jobs: 1,
            queued_prefetch_jobs: 1,
            in_flight_prefetch_jobs: 1,
            queued_playback_cursor_jobs: 1,
            in_flight_playback_cursor_jobs: 1,
            queued_expired_playback_current_jobs: 1,
            queued_expired_jobs: 1,
            dropped_expired_playback_current_jobs: 0,
            dropped_expired_jobs: 0,
            queued_scrub_cursor_jobs: 1,
            in_flight_scrub_cursor_jobs: 1,
            queued_random_access_still_jobs: 1,
            in_flight_random_access_still_jobs: 0,
            in_flight_any_lane_jobs: 0,
            in_flight_playback_lane_jobs: 1,
            in_flight_scrub_lane_jobs: 1,
            in_flight_still_lane_jobs: 0,
            in_flight_non_playback_lane_jobs: 0,
            in_flight_cross_lane_current_jobs: 0,
            queued_any_lane_eligible_jobs: 3,
            queued_playback_lane_eligible_jobs: 1,
            queued_scrub_lane_eligible_jobs: 1,
            queued_still_lane_eligible_jobs: 1,
            queued_non_playback_lane_eligible_jobs: 2,
            closed: false,
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    total_duration_us: 12_000,
                    max_duration_us: 12_000,
                    last_duration_us: 12_000,
                    session_opened_frames: 1,
                    work_classes: test_preview_decode_work_classes(
                        PreviewDecodeWorkClass::SessionOpened,
                        1,
                        12_000,
                        12_000,
                        PreviewDecodeWorkLatencyBuckets {
                            le_16ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                    ),
                    queue_wait_total_us: 95_000,
                    queue_wait_max_us: 95_000,
                    queue_wait_last_us: 95_000,
                    bounded_any_seek_strategy_frames: 1,
                    any_seek_window_ms_max: 1,
                    stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 10_000,
                        swscale_us: 1_000,
                        rgba_copy_us: 500,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_stage_durations: PreviewDecodeStageDurations {
                        packet_decode_us: 10_000,
                        swscale_us: 1_000,
                        rgba_copy_us: 500,
                        ..PreviewDecodeStageDurations::default()
                    },
                    max_frame_queue_wait_us: 95_000,
                    max_frame_bottleneck: PreviewDecodeBottleneck::QueueWait,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 10_000,
            swscale_us: 1_000,
            rgba_copy_us: 500,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 10_000,
            swscale_us: 1_000,
            rgba_copy_us: 500,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_queue_wait_us: 95_000,
        decode_max_frame_bottleneck: PreviewDecodeBottleneck::QueueWait,
        scheduler: MediaPreviewSchedulerDiagnostics {
            skipped_decode_access_mode_mismatch: 1,
            completed_stale_access_mode_mismatch: 1,
            dropped_pending_window_requests: 2,
            ..MediaPreviewSchedulerDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-queue-wait-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    let summary = report.summary.expect("decode summary");
    assert_eq!(
        summary.primary_bottleneck,
        PreviewDecodeBottleneck::QueueWait
    );
    assert_eq!(summary.enqueued_jobs, 4);
    assert_eq!(summary.queue_evicted_prefetch_jobs, 1);
    assert_eq!(summary.interactive_cancel_requests, 1);
    assert_eq!(summary.interactive_cancel_scheduler_requests, 2);
    assert_eq!(summary.interactive_cancel_queued_jobs, 3);
    assert_eq!(summary.queue_canceled_jobs, 3);
    assert_eq!(summary.queue_pruned_obsolete_jobs, 2);
    assert_eq!(summary.queue_promoted_current_jobs, 1);
    assert_eq!(summary.worker_queue.queued_jobs, 3);
    assert_eq!(summary.worker_queue.queued_current_jobs, 2);
    assert_eq!(summary.worker_queue.queued_prefetch_jobs, 1);
    assert_eq!(summary.worker_queue.queued_playback_cursor_jobs, 1);
    assert_eq!(summary.worker_queue.queued_expired_playback_current_jobs, 1);
    assert_eq!(summary.worker_queue.queued_scrub_cursor_jobs, 1);
    assert_eq!(summary.worker_queue.queued_random_access_still_jobs, 1);
    assert_eq!(summary.worker_queue.queued_any_lane_eligible_jobs, 3);
    assert_eq!(summary.worker_queue.queued_playback_lane_eligible_jobs, 1);
    assert_eq!(summary.worker_queue.queued_scrub_lane_eligible_jobs, 1);
    assert_eq!(summary.worker_queue.queued_still_lane_eligible_jobs, 1);
    assert_eq!(
        summary.worker_queue.queued_non_playback_lane_eligible_jobs,
        2
    );
    assert_eq!(summary.worker_queue.in_flight_jobs, 2);
    assert_eq!(summary.worker_queue.in_flight_current_jobs, 1);
    assert_eq!(summary.worker_queue.in_flight_prefetch_jobs, 1);
    assert_eq!(summary.worker_queue.in_flight_playback_cursor_jobs, 1);
    assert_eq!(summary.worker_queue.in_flight_scrub_cursor_jobs, 1);
    assert_eq!(summary.scheduler.dropped_pending_window_requests, 2);
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_queue_wait_bound"
            && root.evidence.contains("queued_jobs=3")
            && root.evidence.contains("queued_current_jobs=2")
            && root.evidence.contains("queued_prefetch_jobs=1")
            && root.evidence.contains("queued_playback_cursor_jobs=1")
            && root.evidence.contains("queued_expired_playback_current_jobs=1")
            && root.evidence.contains("queued_scrub_cursor_jobs=1")
            && root.evidence.contains("queued_random_access_still_jobs=1")
            && root.evidence.contains("queued_any_lane_eligible_jobs=3")
            && root.evidence.contains("queued_playback_lane_eligible_jobs=1")
            && root.evidence.contains("queued_scrub_lane_eligible_jobs=1")
            && root.evidence.contains("queued_still_lane_eligible_jobs=1")
            && root.evidence.contains("queued_non_playback_lane_eligible_jobs=2")
            && root.evidence.contains("in_flight_jobs=2")
            && root.evidence.contains("in_flight_current_jobs=1")
            && root.evidence.contains("in_flight_prefetch_jobs=1")
            && root.evidence.contains("in_flight_playback_cursor_jobs=1")
            && root.evidence.contains("in_flight_scrub_cursor_jobs=1")
            && root.evidence.contains("queue_evicted_prefetch_jobs=1")
            && root.evidence.contains("interactive_cancel_requests=1")
            && root.evidence.contains("interactive_cancel_scheduler_requests=2")
            && root.evidence.contains("interactive_cancel_queued_jobs=3")
            && root.evidence.contains("queue_canceled_jobs=3")
            && root.evidence.contains("queue_pruned_obsolete_jobs=2")
            && root.evidence.contains("queue_promoted_current_jobs=1")));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_queue_wait_max_us"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 95_000
            && check.limit == Some(50_000)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_access_mode_queue_wait_bound"
            && root.area == PreviewDecodePerformanceArea::AccessMode
            && root.evidence.contains("access_mode=ScrubCursor")
            && root.evidence.contains("queue_wait_max_us=95000")
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_access_mode_mismatch"));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_pending_window_backpressure"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "prioritize_current_preview_decode"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_access_mode_transitions"));
}

#[test]
fn preview_decode_performance_report_flags_expired_playback_current_queue() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_playback_cursor_frames: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        decode_queue_wait_total_us: 10_000,
        decode_queue_wait_max_us: 10_000,
        decode_queue_wait_last_us: 10_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    session_reused_frames: 1,
                    work_classes: test_zero_latency_preview_decode_work_classes(
                        PreviewDecodeWorkClass::ReusedOther,
                        1,
                    ),
                    queue_wait_total_us: 10_000,
                    queue_wait_max_us: 10_000,
                    queue_wait_last_us: 10_000,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        worker_queue: MediaPreviewJobQueueDiagnostics {
            queued_jobs: 3,
            queued_current_jobs: 2,
            queued_prefetch_jobs: 1,
            queued_playback_cursor_jobs: 2,
            in_flight_jobs: 1,
            in_flight_playback_cursor_jobs: 1,
            queued_expired_playback_current_jobs: 2,
            dropped_expired_playback_current_jobs: 1,
            queued_any_lane_eligible_jobs: 3,
            queued_playback_lane_eligible_jobs: 2,
            queued_non_playback_lane_eligible_jobs: 2,
            closed: false,
            ..MediaPreviewJobQueueDiagnostics::default()
        },
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_deadline_assignments: 4,
            current_decode_decisions: 3,
            current_drop_late_decisions: 1,
            current_proxy_or_hardware_recommended_decisions: 1,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-expired-playback-queue-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    let summary = report.summary.expect("decode summary");
    assert_eq!(summary.worker_queue.queued_expired_playback_current_jobs, 2);
    assert_eq!(
        summary.worker_queue.dropped_expired_playback_current_jobs,
        1
    );
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_expired_playback_current_queue"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 3
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_expired_playback_current_queue"
            && root.severity == PreviewDecodePerformanceSeverity::Warn
            && root.evidence.contains("queued_expired_playback_current_jobs=2")
            && root.evidence.contains("dropped_expired_playback_current_jobs=1")
            && root.evidence.contains("queued_playback_cursor_jobs=2")
            && root.evidence.contains("current_deadline_assignments=4")
            && root.evidence.contains("current_decode_decisions=3")
            && root.evidence.contains("current_drop_late_decisions=1")
            && root.evidence.contains("current_proxy_or_hardware_recommended_decisions=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "drop_expired_playback_queue_work"));
}

#[test]
fn preview_decode_performance_report_flags_playback_current_stall_expiration() {
    let diagnostics = PreviewDiagnostics {
        playback_current_stalled_expirations: 1,
        queue_canceled_jobs: 1,
        scheduler: MediaPreviewSchedulerDiagnostics {
            canceled_requests: 1,
            ..MediaPreviewSchedulerDiagnostics::default()
        },
        worker_queue: MediaPreviewJobQueueDiagnostics {
            in_flight_jobs: 1,
            in_flight_current_jobs: 1,
            ..MediaPreviewJobQueueDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-playback-buffering-stall-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    let summary = report.summary.expect("decode summary");
    assert_eq!(summary.playback_current_stalled_expirations, 1);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_evidence_present"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_current_stall_expirations"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_current_stall_expirations"
            && root.severity == PreviewDecodePerformanceSeverity::Warn
            && root.evidence.contains("playback_current_stalled_expirations=1")
            && root.evidence.contains("queue_canceled_jobs=1")
            && root.evidence.contains("scheduler_canceled_requests=1")
            && root.evidence.contains("in_flight_jobs=1")
            && root.evidence.contains("in_flight_current_jobs=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "diagnose_preview_current_frame_stalls"));
}

#[test]
fn preview_decode_performance_report_flags_playback_sustained_pressure() {
    let diagnostics = PreviewDiagnostics {
        decode_canceled_jobs: 2,
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_late_streak: 2,
            sustained_pressure_active: true,
            sustained_pressure_events: 1,
            sustained_pressure_recoveries: 0,
            current_drop_late_decisions: 2,
            current_proxy_or_hardware_recommended_decisions: 2,
            prefetch_skipped_sustained_pressure: 1,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        worker_queue: MediaPreviewJobQueueDiagnostics {
            queued_prefetch_jobs: 1,
            in_flight_prefetch_jobs: 1,
            ..MediaPreviewJobQueueDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-playback-pressure-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_playback_sustained_pressure_events"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_sustained_pressure"
            && root.severity == PreviewDecodePerformanceSeverity::Warn
            && root.evidence.contains("sustained_pressure_active=true")
            && root.evidence.contains("current_late_streak=2")
            && root.evidence.contains("prefetch_skipped_sustained_pressure=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "recover_playback_scheduler_pressure"));
}

#[test]
fn preview_decode_performance_report_flags_native_import_unavailable_playback_frames() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_playback_cursor_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    session_opened_frames: 1,
                    work_classes: test_zero_latency_preview_decode_work_classes(
                        PreviewDecodeWorkClass::SessionOpened,
                        1,
                    ),
                    hardware_decode_prefer_gpu_requested_frames: 1,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_native_import_unavailable_decisions: 1,
            current_proxy_or_hardware_recommended_decisions: 1,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-native-import-unavailable-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_native_import_unavailable_playback_frames"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_native_import_unavailable_playback_frames"
            && root.severity == PreviewDecodePerformanceSeverity::Warn
            && root.evidence.contains("current_native_import_unavailable_decisions=1")
            && root.evidence.contains("playback_gpu_resident_native_frames=0")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "enable_renderer_native_video_import"));
}

#[test]
fn preview_decode_performance_report_flags_hardware_decode_admission_gate() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_playback_cursor_frames: 1,
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_decode_decisions: 1,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    session_reused_frames: 1,
                    work_classes: test_zero_latency_preview_decode_work_classes(
                        PreviewDecodeWorkClass::ReusedOther,
                        1,
                    ),
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        hardware_decode_admission: PreviewHardwareDecodeAdmissionDiagnostics {
            playback_request: PreviewHardwareDecodeRequest::PreferHardwareDecode,
            renderer_native_import_support_known: true,
            renderer_native_import_ready: false,
            renderer_import_mode: None,
            native_import_admission_ready: false,
            admission_blocker: Some(
                PreviewHardwareDecodeAdmissionBlocker::RendererImportUnavailable,
            ),
            renderer_supported_handle_kinds: 0,
            renderer_supported_source_texture_formats: 0,
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-hardware-admission-gate-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_hardware_decode_admission_gated"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_hardware_decode_admission_gated"
            && root.severity == PreviewDecodePerformanceSeverity::Warn
            && root.evidence.contains("playback_hardware_decode_request=PreferHardwareDecode")
            && root.evidence.contains("admission_blocker=Some(RendererImportUnavailable)")
            && root.evidence.contains("renderer_native_import_support_known=true")
            && root.evidence.contains("renderer_native_import_ready=false")
            && root.evidence.contains("renderer_supported_handle_kinds=0")
            && root.evidence.contains("renderer_supported_source_texture_formats=0")
            && root.evidence.contains("native_import_admission_ready=false")
    }));
    assert!(report.actions.iter().any(|action| {
        action.code == "connect_native_import_before_enabling_hardware_decode_admission"
    }));
}

#[test]
fn preview_decode_performance_report_checks_access_mode_p95_upper_bounds() {
    let slow_buckets = PreviewDecodeLatencyBuckets {
        le_50ms: 1,
        le_80ms: 19,
        ..PreviewDecodeLatencyBuckets::default()
    };
    let diagnostics = PreviewDiagnostics {
        decode_successes: 20,
        decode_in_process_cpu_frames: 20,
        decode_total_duration_us: 1_250_000,
        decode_max_duration_us: 70_000,
        decode_last_duration_us: 60_000,
        decode_queue_wait_total_us: 1_200_000,
        decode_queue_wait_max_us: 70_000,
        decode_queue_wait_last_us: 60_000,
        decode_current_queue_wait_max_us: 70_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 20,
                    in_process_cpu_frames: 20,
                    total_duration_us: 1_250_000,
                    max_duration_us: 70_000,
                    last_duration_us: 60_000,
                    latency_buckets: slow_buckets,
                    queue_wait_total_us: 1_200_000,
                    queue_wait_max_us: 70_000,
                    queue_wait_last_us: 60_000,
                    queue_wait_buckets: slow_buckets,
                    bounded_any_seek_strategy_frames: 20,
                    any_seek_window_ms_max: 1,
                    session_reused_frames: 20,
                    forward_reused_frames: 20,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        forward_steady: PreviewDecodeWorkLatencyProfile {
                            frames: 20,
                            total_duration_us: 1_250_000,
                            max_duration_us: 70_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_50ms: 1,
                                le_80ms: 19,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-p95-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_forward_steady_p95_worker_execution_us"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 80_000
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_queue_wait_p95_us"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 80_000
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_work_class_over_budget"
            && root.evidence.contains("p95_upper_bound_us=Some(80000)")
            && root.evidence.contains("latency_buckets=")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_access_mode_queue_wait_bound"
            && root.evidence.contains("queue_wait_p95_upper_bound_us=Some(80000)")
            && root.evidence.contains("queue_wait_buckets=")
    }));
}

#[test]
fn preview_decode_performance_report_excludes_expired_queue_wait_from_ready_p95() {
    let ready_buckets = PreviewDecodeLatencyBuckets {
        le_10ms: 95,
        ..PreviewDecodeLatencyBuckets::default()
    };
    let expired_queue_wait = PreviewDecodeQueueWaitProfile {
        samples: 64,
        total_us: 5_760_000,
        max_us: 90_000,
        last_us: 90_000,
        current_max_us: 90_000,
        buckets: PreviewDecodeLatencyBuckets {
            gt_80ms: 64,
            ..PreviewDecodeLatencyBuckets::default()
        },
        ..PreviewDecodeQueueWaitProfile::default()
    };
    let diagnostics = PreviewDiagnostics {
        decode_successes: 95,
        decode_in_process_cpu_frames: 95,
        decode_queue_wait_total_us: 380_000,
        decode_queue_wait_max_us: 4_000,
        decode_queue_wait_last_us: 4_000,
        decode_current_queue_wait_max_us: 4_000,
        decode_expired_queue_wait: expired_queue_wait,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 95,
                in_process_cpu_frames: 95,
                queue_wait_total_us: 380_000,
                queue_wait_max_us: 4_000,
                queue_wait_last_us: 4_000,
                queue_wait_buckets: ready_buckets,
                queue_wait_samples: 95,
                expired_queue_wait,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-queue-disposition-test",
        50_000,
    );
    let ready_p95 = report
        .checks
        .iter()
        .find(|check| check.code == "preview_decode_playback_cursor_queue_wait_p95_us")
        .expect("playback Ready queue-wait p95 check");

    assert_eq!(ready_p95.observed, 10_000);
    assert_eq!(
        report.summary.expect("summary").expired_queue_wait,
        expired_queue_wait
    );
    let json = serde_json::to_value(&report).expect("serialize queue-disposition report");
    assert_eq!(
        json["schema_version"],
        PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION
    );
    assert_eq!(json["schema_version"], 37);
    assert_eq!(json["summary"]["expired_queue_wait"]["samples"], 64);
    assert_eq!(
        json["summary"]["access_mode_profiles"]["playback_cursor"]["expired_queue_wait"]["buckets"]
            ["gt_80ms"],
        64
    );
}

#[test]
fn preview_decode_performance_report_fails_invalid_access_mode_admission() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        queue_invalid_access_mode_drops: 1,
        scheduler: MediaPreviewSchedulerDiagnostics {
            dropped_invalid_access_mode_requests: 2,
            ..MediaPreviewSchedulerDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-invalid-access-mode-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_invalid_access_mode_requests"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 2
            && check.limit == Some(0)
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_queue_invalid_access_mode_drops"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_invalid_access_mode_request"
            && root.area == PreviewDecodePerformanceArea::Scheduling
            && root.evidence.contains("dropped_invalid_access_mode_requests=2")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_queue_invalid_access_mode_drop"
            && root.area == PreviewDecodePerformanceArea::Scheduling
            && root.evidence.contains("queue_invalid_access_mode_drops=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "fix_preview_access_mode_admission"));
}

#[test]
fn preview_decode_performance_report_fails_worker_transport_drops() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        enqueued_jobs: 3,
        queue_full_drops: 1,
        queue_evicted_prefetch_jobs: 1,
        interactive_cancel_requests: 1,
        interactive_cancel_scheduler_requests: 2,
        interactive_cancel_queued_jobs: 2,
        queue_canceled_jobs: 2,
        queue_pruned_obsolete_jobs: 2,
        queue_promoted_current_jobs: 1,
        worker_disconnected_drops: 1,
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 10_000,
            swscale_us: 1_000,
            rgba_copy_us: 500,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 10_000,
            swscale_us: 1_000,
            rgba_copy_us: 500,
            ..PreviewDecodeStageDurations::default()
        },
        scheduler: MediaPreviewSchedulerDiagnostics {
            dropped_pending_window_requests: 2,
            ..MediaPreviewSchedulerDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-worker-queue-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    let summary = report.summary.expect("decode summary");
    assert_eq!(summary.enqueued_jobs, 3);
    assert_eq!(summary.queue_full_drops, 1);
    assert_eq!(summary.interactive_cancel_requests, 1);
    assert_eq!(summary.interactive_cancel_scheduler_requests, 2);
    assert_eq!(summary.interactive_cancel_queued_jobs, 2);
    assert_eq!(summary.queue_canceled_jobs, 2);
    assert_eq!(summary.worker_disconnected_drops, 1);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_worker_queue_full_drops"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_worker_disconnected_drops"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_worker_queue_full_drops"
            && root.severity == PreviewDecodePerformanceSeverity::Fail
            && root.evidence.contains("queue_full_drops=1")
            && root.evidence.contains("interactive_cancel_requests=1")
            && root.evidence.contains("interactive_cancel_scheduler_requests=2")
            && root.evidence.contains("interactive_cancel_queued_jobs=2")
            && root.evidence.contains("queue_canceled_jobs=2")
            && root.evidence.contains("scheduler_dropped_pending_window_requests=2")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_worker_disconnected_drops"
            && root.severity == PreviewDecodePerformanceSeverity::Fail
            && root.evidence.contains("worker_disconnected_drops=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "reduce_preview_worker_transport_backpressure"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "restore_preview_worker_lifecycle"));
}

#[test]
fn preview_decode_performance_report_fails_broker_clock_regression() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        scheduler: MediaPreviewSchedulerDiagnostics {
            clock_regressions: 1,
            ..MediaPreviewSchedulerDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-clock-regression-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_broker_clock_regressions"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 1
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_broker_clock_regression"
            && root.evidence.contains("clock_regressions=1")
    }));
}

#[test]
fn preview_decode_performance_report_keeps_queue_wait_evidence_without_successful_frame() {
    let diagnostics = PreviewDiagnostics {
        decode_canceled_jobs: 1,
        decode_canceled_obsolete_jobs: 1,
        decode_canceled_scrub_cursor_jobs: 1,
        decode_queue_wait_total_us: 75_000,
        decode_queue_wait_max_us: 75_000,
        decode_queue_wait_last_us: 75_000,
        decode_current_queue_wait_max_us: 75_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                queue_wait_total_us: 75_000,
                queue_wait_max_us: 75_000,
                queue_wait_last_us: 75_000,
                queue_wait_buckets: PreviewDecodeLatencyBuckets {
                    le_80ms: 1,
                    ..PreviewDecodeLatencyBuckets::default()
                },
                queue_wait_samples: 1,
                canceled_jobs: 1,
                canceled_obsolete_jobs: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-canceled-queue-wait-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_scrub_cursor_queue_wait_max_us"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 75_000
            && check.limit == Some(50_000)
    }));
    assert!(!report.checks.iter().any(|check| {
        check.code.starts_with("preview_decode_scrub_cursor_")
            && check.code.ends_with("_max_worker_execution_us")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_access_mode_queue_wait_bound"
            && root.evidence.contains("access_mode=ScrubCursor")
            && root.evidence.contains("frames=0")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_obsolete_cancellations"
            && root.evidence.contains("scrub_obsolete_jobs=1")
            && root.evidence.contains("playback_obsolete_jobs=0")
    }));
}

#[test]
fn preview_decode_performance_report_accepts_long_work_before_timely_cancellation() {
    let diagnostics = PreviewDiagnostics {
        decode_cancellation: cancellation_evidence(
            mondrian_playback::FrameWorkClass::Interactive,
            mondrian_playback::FrameCancellationCause::Superseded,
            85_000,
            Some(80_000),
            Some(1_000),
        ),
        decode_canceled_jobs: 1,
        decode_canceled_obsolete_jobs: 1,
        decode_canceled_scrub_cursor_jobs: 1,
        decode_canceled_total_duration_us: 85_000,
        decode_canceled_max_duration_us: 85_000,
        decode_canceled_last_duration_us: 85_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                canceled_jobs: 1,
                canceled_obsolete_jobs: 1,
                canceled_total_duration_us: 85_000,
                canceled_max_duration_us: 85_000,
                canceled_last_duration_us: 85_000,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-slow-cancel-test",
        50_000,
    );

    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_cancellation_gate"
            && check.severity == PreviewDecodePerformanceSeverity::Pass
    }));
    assert!(!report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_cancellation_gate_failed"));
    assert!(!report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
}

#[test]
fn preview_decode_performance_report_flags_slow_cancel_return_latency() {
    let diagnostics = PreviewDiagnostics {
        decode_cancellation: cancellation_evidence(
            mondrian_playback::FrameWorkClass::Interactive,
            mondrian_playback::FrameCancellationCause::Superseded,
            90_000,
            Some(20_000),
            Some(1_000),
        ),
        decode_canceled_jobs: 1,
        decode_canceled_obsolete_jobs: 1,
        decode_canceled_scrub_cursor_jobs: 1,
        decode_canceled_total_duration_us: 90_000,
        decode_canceled_max_duration_us: 90_000,
        decode_canceled_last_duration_us: 90_000,
        decode_canceled_return_latency_total_us: 70_000,
        decode_canceled_return_latency_max_us: 70_000,
        decode_canceled_return_latency_last_us: 70_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                canceled_jobs: 1,
                canceled_obsolete_jobs: 1,
                canceled_total_duration_us: 90_000,
                canceled_max_duration_us: 90_000,
                canceled_last_duration_us: 90_000,
                canceled_return_latency_total_us: 70_000,
                canceled_return_latency_max_us: 70_000,
                canceled_return_latency_last_us: 70_000,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-cancel-return-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_interactive_cancel_return_latency_max_us"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 70_000
            && check.limit == Some(50_000)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_cancellation_gate_failed"
            && root.evidence.contains("LogicalCancellationToReturnExceeded")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
}

#[test]
fn preview_decode_performance_report_flags_slow_cancel_observation_latency() {
    let diagnostics = PreviewDiagnostics {
        decode_cancellation: cancellation_evidence(
            mondrian_playback::FrameWorkClass::Interactive,
            mondrian_playback::FrameCancellationCause::Superseded,
            9_000,
            Some(8_000),
            Some(8_000),
        ),
        decode_canceled_jobs: 1,
        decode_canceled_obsolete_jobs: 1,
        decode_canceled_scrub_cursor_jobs: 1,
        decode_canceled_total_duration_us: 9_000,
        decode_canceled_max_duration_us: 9_000,
        decode_canceled_last_duration_us: 9_000,
        decode_cancel_observation_samples: 1,
        decode_cancel_observation_total_us: 8_000,
        decode_cancel_observation_max_us: 8_000,
        decode_cancel_observation_last_us: 8_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                canceled_jobs: 1,
                canceled_obsolete_jobs: 1,
                canceled_total_duration_us: 9_000,
                canceled_max_duration_us: 9_000,
                canceled_last_duration_us: 9_000,
                cancel_observation_samples: 1,
                cancel_observation_total_us: 8_000,
                cancel_observation_max_us: 8_000,
                cancel_observation_last_us: 8_000,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-slow-cancel-observation-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_logical_cancel_observation_max_us"
            && check.severity == PreviewDecodePerformanceSeverity::Fail
            && check.observed == 8_000
            && check.limit
                == Some(
                    mondrian_playback::FrameCancellationPolicy::default()
                        .max_request_to_logical_cancellation
                        .as_micros() as u64,
                )
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_cancellation_gate_failed"
            && root.evidence.contains("RequestToLogicalCancellationExceeded")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_decode_cancellation_points"));
}

#[test]
fn preview_decode_performance_report_classifies_prefetch_deadline_cancellations() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_canceled_jobs: 3,
        decode_canceled_prefetch_deadline_jobs: 2,
        decode_canceled_prefetch_preempted_jobs: 1,
        decode_canceled_playback_cursor_jobs: 3,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        worker_queue: MediaPreviewJobQueueDiagnostics {
            queued_current_jobs: 1,
            queued_prefetch_jobs: 1,
            ..MediaPreviewJobQueueDiagnostics::default()
        },
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 10_000,
            swscale_us: 1_000,
            rgba_copy_us: 500,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 10_000,
            swscale_us: 1_000,
            rgba_copy_us: 500,
            ..PreviewDecodeStageDurations::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    session_opened_frames: 1,
                    work_classes: test_preview_decode_work_classes(
                        PreviewDecodeWorkClass::SessionOpened,
                        1,
                        12_000,
                        12_000,
                        PreviewDecodeWorkLatencyBuckets {
                            le_16ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                    ),
                    canceled_jobs: 3,
                    canceled_prefetch_deadline_jobs: 2,
                    canceled_prefetch_preempted_jobs: 1,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-cancel-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    let summary = report.summary.expect("decode summary");
    assert_eq!(summary.canceled_jobs, 3);
    assert_eq!(summary.canceled_prefetch_deadline_jobs, 2);
    assert_eq!(summary.canceled_prefetch_preempted_jobs, 1);
    assert_eq!(summary.canceled_playback_cursor_jobs, 3);
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_prefetch_deadline_cancellations"
            && root.evidence.contains("playback_prefetch_deadline_jobs=2")
            && root.evidence.contains("scrub_prefetch_deadline_jobs=0")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_prefetch_preempted_by_current"
            && root.evidence.contains("playback_prefetch_preempted_jobs=1")
            && root.evidence.contains("queued_current_jobs=1")
            && root.evidence.contains("queued_prefetch_jobs=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "tune_preview_prefetch_deadline_or_proxy"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "reduce_speculative_prefetch_pressure"));
}

#[test]
fn preview_decode_performance_report_classifies_playback_deadline_cancellations() {
    let diagnostics = PreviewDiagnostics {
        decode_canceled_jobs: 1,
        decode_canceled_playback_deadline_jobs: 1,
        decode_canceled_playback_cursor_jobs: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                canceled_jobs: 1,
                canceled_playback_deadline_jobs: 1,
                queue_wait_max_us: 55_000,
                max_duration_us: 0,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        playback_schedule: PreviewPlaybackScheduleDiagnostics {
            current_decode_decisions: 1,
            current_drop_late_decisions: 1,
            current_proxy_or_hardware_recommended_decisions: 1,
            ..PreviewPlaybackScheduleDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-playback-deadline-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    let summary = report.summary.expect("decode summary");
    assert_eq!(summary.canceled_playback_deadline_jobs, 1);
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_deadline_cancellations"
            && root.evidence.contains("canceled_playback_deadline_jobs=1")
            && root.evidence.contains("playback_cursor_deadline_jobs=1")
            && root.evidence.contains("current_decode_decisions=1")
            && root.evidence.contains("current_drop_late_decisions=1")
            && root.evidence.contains("current_proxy_or_hardware_recommended_decisions=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| { action.code == "drop_late_playback_frames_or_use_proxy_hardware_decode" }));
}

#[test]
fn preview_decode_performance_report_classifies_still_preemptions() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_canceled_jobs: 1,
        decode_canceled_still_preempted_jobs: 1,
        decode_canceled_random_access_still_jobs: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 18_000,
        decode_max_duration_us: 18_000,
        decode_last_duration_us: 18_000,
        worker_queue: MediaPreviewJobQueueDiagnostics {
            queued_current_jobs: 2,
            queued_scrub_cursor_jobs: 1,
            queued_playback_cursor_jobs: 1,
            queued_random_access_still_jobs: 1,
            ..MediaPreviewJobQueueDiagnostics::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: test_complete_successful_decode_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    session_opened_frames: 1,
                    work_classes: test_preview_decode_work_classes(
                        PreviewDecodeWorkClass::SessionOpened,
                        1,
                        18_000,
                        18_000,
                        PreviewDecodeWorkLatencyBuckets {
                            le_25ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                    ),
                    canceled_jobs: 1,
                    canceled_still_preempted_jobs: 1,
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-still-preempt-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    let summary = report.summary.expect("decode summary");
    assert_eq!(summary.canceled_still_preempted_jobs, 1);
    assert_eq!(summary.canceled_random_access_still_jobs, 1);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_decode_still_preempted_by_realtime_cancellations"
            && check.severity == PreviewDecodePerformanceSeverity::Warn
            && check.observed == 1
            && check.limit == Some(0)
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_still_preempted_by_realtime_current"
            && root.evidence.contains("random_access_still_preempted_jobs=1")
            && root.evidence.contains("queued_scrub_cursor_jobs=1")
            && root.evidence.contains("queued_playback_cursor_jobs=1")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "preempt_still_decode_for_realtime_preview"));
}

#[test]
fn preview_decode_performance_report_breaks_down_unknown_cancellations_by_access_mode() {
    let diagnostics = PreviewDiagnostics {
        decode_cancellation: cancellation_evidence(
            mondrian_playback::FrameWorkClass::Still,
            mondrian_playback::FrameCancellationCause::Unknown,
            1_000,
            None,
            None,
        ),
        decode_canceled_jobs: 1,
        decode_canceled_unknown_jobs: 1,
        decode_canceled_random_access_still_jobs: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: PreviewDecodeAccessModeProfile {
                canceled_jobs: 1,
                canceled_unknown_jobs: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-unknown-cancel-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_cancellation_gate_failed"
            && root.evidence.contains("UnknownCause")
    }));
}

#[test]
fn preview_decode_performance_report_flags_playback_without_locality() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_frames: 2,
        decode_total_duration_us: 80_000,
        decode_max_duration_us: 45_000,
        decode_last_duration_us: 35_000,
        decode_seeked_frames: 2,
        decode_decoded_frame_count: 96,
        decode_max_decoded_frame_count: 48,
        decode_stage_durations: PreviewDecodeStageDurations {
            seek_us: 20_000,
            packet_decode_us: 55_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            seek_us: 10_000,
            packet_decode_us: 30_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 2,
                in_process_cpu_frames: 2,
                total_duration_us: 80_000,
                max_duration_us: 45_000,
                last_duration_us: 35_000,
                seeked_frames: 2,
                session_opened_frames: 2,
                session_reused_frames: 0,
                forward_reused_frames: 0,
                work_classes: test_preview_decode_work_classes(
                    PreviewDecodeWorkClass::SessionOpened,
                    2,
                    80_000,
                    45_000,
                    PreviewDecodeWorkLatencyBuckets {
                        le_40ms: 1,
                        le_50ms: 1,
                        ..PreviewDecodeWorkLatencyBuckets::default()
                    },
                ),
                decoded_frame_count: 96,
                max_decoded_frame_count: 48,
                stage_durations: PreviewDecodeStageDurations {
                    seek_us: 20_000,
                    packet_decode_us: 55_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    seek_us: 10_000,
                    packet_decode_us: 30_000,
                    ..PreviewDecodeStageDurations::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-locality-test",
        50_000,
    );

    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_session_not_reused"
            && root.evidence.contains("session_opened_frames=2")
    }));
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_playback_without_locality"
            && root.evidence.contains("source_decode_frames=2")
            && root.evidence.contains("forward_reused_frames=0")
    }));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "improve_playback_decoder_residency"));
}

#[test]
fn preview_render_performance_report_classifies_output_boundary_bound_slow_frame() {
    let diagnostics = PreviewDiagnostics {
        render_timed_frames: 1,
        render_total_duration_us: 120_000,
        render_max_duration_us: 120_000,
        render_last_duration_us: 120_000,
        render_stage_durations: PreviewRenderStageDurations {
            resolve_us: 2_000,
            final_cache_lookup_us: 100,
            working_prepare_us: 7_000,
            cpu_composite_us: 20_000,
            cpu_output_boundary_us: 90_000,
            frame_packaging_us: 900,
        },
        render_max_frame_stage_durations: PreviewRenderStageDurations {
            resolve_us: 2_000,
            final_cache_lookup_us: 100,
            working_prepare_us: 7_000,
            cpu_composite_us: 20_000,
            cpu_output_boundary_us: 90_000,
            frame_packaging_us: 900,
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_render_performance_report(
        diagnostics.render_performance_summary(50_000),
        "preview-render-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewRenderPerformanceVerdict::Fail);
    let summary = report.summary.expect("render summary");
    assert_eq!(
        summary.primary_bottleneck,
        PreviewRenderBottleneck::CpuOutputBoundary
    );
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_render_frame_over_budget"));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_render_cpu_output_boundary_bound"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "move_preview_output_boundary_to_gpu"));
}

#[test]
fn preview_render_performance_report_fails_typed_execution_unavailability() {
    let diagnostics = PreviewDiagnostics {
        render_timed_frames: 1,
        render_total_duration_us: 1_000,
        render_max_duration_us: 1_000,
        render_last_duration_us: 1_000,
        unavailability: crate::app::preview_unavailability::PreviewUnavailabilityEvidenceSnapshot {
            observations: 1,
            failed: 1,
            stages: crate::app::preview_unavailability::PreviewOutputStageBreakdown {
                timeline_composite: 1,
                ..Default::default()
            },
            last_disposition: Some(PreviewUnavailabilityDisposition::Failed),
            last_stage: Some(PreviewOutputStage::TimelineComposite),
            ..Default::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_render_performance_report(
        diagnostics.render_performance_summary(50_000),
        "preview-render-failure-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewRenderPerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_render_failed_outputs"
            && check.severity == PreviewRenderPerformanceSeverity::Fail
            && check.observed == 1
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_render_execution_failed"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_unavailability"));
}

#[test]
fn preview_render_performance_report_fails_typed_correctness_blocker() {
    let diagnostics = PreviewDiagnostics {
        unavailability: crate::app::preview_unavailability::PreviewUnavailabilityEvidenceSnapshot {
            observations: 1,
            blocked: 1,
            stages: crate::app::preview_unavailability::PreviewOutputStageBreakdown {
                timeline_composite: 1,
                ..Default::default()
            },
            last_disposition: Some(PreviewUnavailabilityDisposition::Blocked),
            last_stage: Some(PreviewOutputStage::TimelineComposite),
            ..Default::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_render_performance_report(
        diagnostics.render_performance_summary(50_000),
        "preview-render-blocker-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewRenderPerformanceVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "preview_render_blocked_outputs"
            && check.severity == PreviewRenderPerformanceSeverity::Fail
            && check.observed == 1
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_render_output_blocked"));
    assert!(report
        .actions
        .iter()
        .any(|action| action.code == "inspect_preview_unavailability"));
}

#[test]
fn preview_performance_reports_classify_slowest_frame_not_aggregate_total() {
    let decode_diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_frames: 2,
        decode_total_duration_us: 160_000,
        decode_max_duration_us: 120_000,
        decode_last_duration_us: 40_000,
        decode_decoded_frame_count: 40,
        decode_max_decoded_frame_count: 36,
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 30_000,
            swscale_us: 200_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 95_000,
            swscale_us: 10_000,
            rgba_copy_us: 2_000,
            ..PreviewDecodeStageDurations::default()
        },
        ..PreviewDiagnostics::default()
    };
    let decode_report = build_preview_decode_performance_report(
        decode_diagnostics.decode_performance_summary(50_000),
        "preview-decode-max-frame-test",
        50_000,
    );

    assert_eq!(
        decode_report.summary.expect("decode summary").primary_bottleneck,
        PreviewDecodeBottleneck::PacketDecode
    );
    assert!(decode_report
        .root_causes
        .iter()
        .any(|root| root.evidence.contains("packet_decode_us=95000")));

    let render_diagnostics = PreviewDiagnostics {
        render_timed_frames: 2,
        render_total_duration_us: 160_000,
        render_max_duration_us: 120_000,
        render_last_duration_us: 40_000,
        render_stage_durations: PreviewRenderStageDurations {
            cpu_composite_us: 200_000,
            cpu_output_boundary_us: 30_000,
            ..PreviewRenderStageDurations::default()
        },
        render_max_frame_stage_durations: PreviewRenderStageDurations {
            cpu_composite_us: 20_000,
            cpu_output_boundary_us: 90_000,
            ..PreviewRenderStageDurations::default()
        },
        ..PreviewDiagnostics::default()
    };
    let render_report = build_preview_render_performance_report(
        render_diagnostics.render_performance_summary(50_000),
        "preview-render-max-frame-test",
        50_000,
    );

    assert_eq!(
        render_report.summary.expect("render summary").primary_bottleneck,
        PreviewRenderBottleneck::CpuOutputBoundary
    );
    assert!(render_report
        .root_causes
        .iter()
        .any(|root| root.evidence.contains("cpu_output_boundary_us=90000")));
}

#[test]
fn preview_decode_bottleneck_uses_queue_wait_from_same_slowest_frame() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 120_000,
        decode_max_duration_us: 120_000,
        decode_last_duration_us: 120_000,
        decode_queue_wait_total_us: 201_000,
        decode_queue_wait_max_us: 200_000,
        decode_queue_wait_last_us: 200_000,
        decode_current_queue_wait_max_us: 200_000,
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 95_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 95_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_queue_wait_us: 1_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                total_duration_us: 120_000,
                max_duration_us: 120_000,
                last_duration_us: 120_000,
                queue_wait_total_us: 201_000,
                queue_wait_max_us: 200_000,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                session_reused_frames: 1,
                forward_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    forward_steady: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        total_duration_us: 120_000,
                        max_duration_us: 120_000,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_120ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 95_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 95_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_queue_wait_us: 1_000,
                max_frame_bottleneck: PreviewDecodeBottleneck::PacketDecode,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };

    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-decode-same-frame-bottleneck-test",
        50_000,
    );

    let summary = report.summary.expect("decode summary");
    assert_eq!(
        summary.primary_bottleneck,
        PreviewDecodeBottleneck::PacketDecode
    );
    assert_eq!(summary.queue_wait_max_us, 200_000);
    assert_eq!(summary.max_frame_queue_wait_us, 1_000);
    assert!(report.root_causes.iter().any(|root| {
        root.code == "preview_decode_work_class_over_budget"
            && root.evidence.contains("max_frame_queue_wait_us=1000")
            && root.evidence.contains("primary_bottleneck=PacketDecode")
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_decode_access_mode_queue_wait_bound"));
}

#[test]
fn preview_diagnostics_count_input_color_resolution_sources() {
    let service = WindowPreviewAdapter::new();

    service.record_input_color_resolution(InputColorResolutionSource::Override);
    service.record_input_color_resolution(InputColorResolutionSource::DataTexture);
    service.record_input_color_resolution(InputColorResolutionSource::DetectedMetadata);
    service.record_input_color_resolution(InputColorResolutionSource::MissingPolicyAssumeRec709);
    service.record_input_color_resolution(InputColorResolutionSource::MissingPolicyAssumeRec709);
    service.record_input_color_resolution(InputColorResolutionSource::MissingPolicyRejectMedia);
    service.record_input_color_resolution(InputColorResolutionSource::DetectedMetadata);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.input_color_resolution_override, 1);
    assert_eq!(diagnostics.input_color_resolution_data_texture, 1);
    assert_eq!(diagnostics.input_color_resolution_detected_metadata, 2);
    assert_eq!(diagnostics.input_color_resolution_missing_assume_rec709, 2);
    assert_eq!(diagnostics.input_color_resolution_missing_rejected, 1);
}

#[test]
fn preview_diagnostics_derives_composite_color_path_summary() {
    let diagnostics = PreviewDiagnostics {
        color_composite_elements: 5,
        color_composite_float_linear: 2,
        color_composite_legacy_rgba8: 1,
        color_composite_legacy_media_transform: 1,
        color_composite_legacy_adjustment_effect: 2,
        ..PreviewDiagnostics::default()
    };

    let summary = diagnostics.composite_color_path_summary();

    assert_eq!(summary.path, TimelineCompositeColorPath::LegacyRgba8);
    assert_eq!(summary.elements, 5);
    assert_eq!(summary.composite_plans(), 3);
    assert_eq!(summary.legacy_breakdown.media_transform, 1);
    assert_eq!(summary.legacy_breakdown.adjustment_effect, 2);
    assert_eq!(summary.legacy_breakdown.total(), 3);
}

#[test]
fn preview_diagnostics_derives_color_health_summary() {
    assert_eq!(PreviewDiagnostics::default().color_health_summary(), None);

    let diagnostics = PreviewDiagnostics {
        input_color_resolution_override: 2,
        input_color_resolution_data_texture: 3,
        input_color_resolution_detected_metadata: 5,
        input_color_resolution_missing_assume_rec709: 7,
        input_color_resolution_missing_rejected: 11,
        color_stage_total_stages: 4,
        color_stage_cpu_input_stages: 1,
        color_stage_cpu_output_stages: 1,
        color_stage_gpu_color_stages: 2,
        color_stage_upload_stages: 1,
        color_stage_gpu_blockers: 2,
        color_stage_gpu_shader_module_blockers: 1,
        color_stage_gpu_render_pipeline_blockers: 1,
        color_stage_pixels: 128,
        color_rgba8_boundary_calls: 1,
        color_composite_plans: 3,
        color_composite_elements: 9,
        color_composite_float_linear: 2,
        color_composite_legacy_rgba8: 1,
        color_composite_legacy_media_transform: 1,
        ..PreviewDiagnostics::default()
    };

    let summary = diagnostics.color_health_summary().expect("preview color health");

    assert_eq!(summary.composite_plans, 3);
    assert_eq!(summary.detected_metadata, 5);
    assert_eq!(summary.override_count, 2);
    assert_eq!(summary.policy_assumptions, 7);
    assert_eq!(summary.data_textures, 3);
    assert_eq!(summary.policy_rejections, 11);
    assert_eq!(summary.explicit_metadata_or_override, 7);
    assert_eq!(summary.cpu_input_stages, 1);
    assert_eq!(summary.cpu_output_stages, 1);
    assert_eq!(summary.gpu_color_stages, 2);
    assert_eq!(summary.gpu_blockers, 2);
    assert_eq!(summary.gpu_blocker_breakdown.shader_module_not_prepared, 1);
    assert_eq!(
        summary.gpu_blocker_breakdown.render_pipeline_not_prepared,
        1
    );
    assert_eq!(summary.transfer_stages, 1);
    assert_eq!(summary.rgba8_boundary_calls, 1);
    assert_eq!(summary.float_linear_composites, 2);
    assert_eq!(summary.legacy_rgba8_composites, 1);
    assert_eq!(summary.legacy_reason_total, 1);
    assert_eq!(summary.legacy_breakdown.media_transform, 1);
    assert!(!summary.fully_float_linear);
    assert!(!summary.gpu_path_ready);
    assert_eq!(summary.cpu_output_fallback_frames, 0);
    assert_eq!(summary.cpu_output_fallback_pixels, 0);
    assert!(summary.preview_gpu_output_blocker_breakdown.is_empty());
}

fn assert_preview_export_color_health_match(
    preview: PreviewColorHealthSummary,
    export: mondrian_export::queue::ExportJobColorDiagnosticsSummary,
) {
    assert_eq!(export.diagnosed_frames, 1);
    assert_eq!(preview.detected_metadata, export.detected_metadata);
    assert_eq!(preview.override_count, export.override_count);
    assert_eq!(preview.policy_assumptions, export.policy_assumptions);
    assert_eq!(preview.data_textures, export.data_textures);
    assert_eq!(preview.policy_rejections, export.policy_rejections);
    assert_eq!(
        preview.explicit_metadata_or_override,
        export.explicit_metadata_or_override
    );
    assert_eq!(preview.cpu_input_stages, export.cpu_input_stages);
    assert_eq!(preview.cpu_output_stages, export.cpu_output_stages);
    assert_eq!(preview.gpu_color_stages, export.gpu_color_stages);
    assert_eq!(preview.gpu_blockers, export.gpu_blockers);
    assert_eq!(preview.gpu_blocker_breakdown, export.gpu_blocker_breakdown);
    assert_eq!(preview.transfer_stages, export.transfer_stages);
    assert_eq!(
        preview.float_linear_composites,
        export.float_linear_composites
    );
    assert_eq!(
        preview.legacy_rgba8_composites,
        export.legacy_rgba8_composites
    );
    assert_eq!(preview.legacy_reason_total, export.legacy_reason_total);
    assert_eq!(preview.legacy_breakdown, export.legacy_breakdown);
    assert_eq!(
        preview.blocked_color_domain_composites,
        export.blocked_color_domain_composites
    );
    assert_eq!(preview.domain_blockers, export.domain_blockers);
    assert_eq!(preview.fully_float_linear, export.fully_float_linear);
    assert_eq!(preview.gpu_path_ready, export.gpu_path_ready);
}

#[test]
fn unresolved_effect_domain_is_a_distinct_fail_closed_preview_failure() {
    let diagnostics = PreviewDiagnostics {
        color_composite_plans: 1,
        color_composite_elements: 1,
        color_composite_blocked_domains: 1,
        color_composite_blocked_media_effect_domain: 1,
        ..PreviewDiagnostics::default()
    };

    let composite = diagnostics.composite_color_path_summary();
    assert_eq!(composite.path, TimelineCompositeColorPath::Blocked);
    assert_eq!(composite.legacy_rgba8_composites, 0);
    assert_eq!(composite.blocked_composites, 1);
    assert_eq!(composite.domain_blockers.media_effect, 1);

    let summary = diagnostics.color_health_summary().expect("preview color health");
    let report = summary.health_report("effect-domain-blocker");
    assert_eq!(report.verdict, PreviewColorHealthVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == color_report_vocab::check::EFFECT_DOMAIN_BLOCKERS
            && check.severity == PreviewColorHealthSeverity::Fail
            && check.observed == 1
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| { root.code == color_report_vocab::root_cause::EFFECT_DOMAIN_UNRESOLVED }));
    assert!(report.actions.iter().any(|action| {
        action.code == color_report_vocab::action::RESOLVE_EFFECT_DOMAIN_TRANSITIONS
    }));
    assert!(!report
        .root_causes
        .iter()
        .any(|root| { root.code == color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH }));
}

fn assert_preview_export_color_reports_match(
    preview: &PreviewColorHealthReport,
    export: &mondrian_export::queue::ExportColorHealthReport,
) {
    assert_eq!(
        preview_color_report_verdict(preview.verdict),
        export_color_report_verdict(export.verdict)
    );
    assert_eq!(
        preview_shared_check_signature(preview),
        export_shared_check_signature(export)
    );
    assert_eq!(
        preview_root_cause_signature(preview),
        export_root_cause_signature(export)
    );
    assert_eq!(
        preview_action_signature(preview),
        export_action_signature(export)
    );
}

fn preview_color_report_verdict(verdict: PreviewColorHealthVerdict) -> &'static str {
    match verdict {
        PreviewColorHealthVerdict::Pass => "pass",
        PreviewColorHealthVerdict::Warn => "warn",
        PreviewColorHealthVerdict::Fail => "fail",
    }
}

fn export_color_report_verdict(
    verdict: mondrian_export::queue::ExportColorHealthVerdict,
) -> &'static str {
    match verdict {
        mondrian_export::queue::ExportColorHealthVerdict::Pass => "pass",
        mondrian_export::queue::ExportColorHealthVerdict::Warn => "warn",
        mondrian_export::queue::ExportColorHealthVerdict::Fail => "fail",
    }
}

fn preview_shared_check_signature(
    report: &PreviewColorHealthReport,
) -> Vec<(String, String, String, u64, Option<u64>)> {
    let mut signature = report
        .checks
        .iter()
        .filter(|check| is_shared_color_report_check(check.code))
        .map(|check| {
            (
                format!("{:?}", check.area),
                check.code.to_owned(),
                preview_color_report_severity(check.severity).to_owned(),
                check.observed,
                check.limit,
            )
        })
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn export_shared_check_signature(
    report: &mondrian_export::queue::ExportColorHealthReport,
) -> Vec<(String, String, String, u64, Option<u64>)> {
    let mut signature = report
        .checks
        .iter()
        .filter(|check| is_shared_color_report_check(check.code))
        .map(|check| {
            (
                format!("{:?}", check.area),
                check.code.to_owned(),
                export_color_report_severity(check.severity).to_owned(),
                check.observed,
                check.limit,
            )
        })
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn is_shared_color_report_check(code: &str) -> bool {
    matches!(
        code,
        "fully_float_linear"
            | "gpu_path_ready"
            | "gpu_blockers"
            | "transfer_stages"
            | "legacy_reason_total"
            | "policy_rejections"
    )
}

fn preview_root_cause_signature(
    report: &PreviewColorHealthReport,
) -> Vec<(String, String, String, String)> {
    let mut signature = report
        .root_causes
        .iter()
        .map(|root| {
            (
                format!("{:?}", root.area),
                normalized_color_root_cause_code(root.code).to_owned(),
                preview_color_report_severity(root.severity).to_owned(),
                root.evidence.clone(),
            )
        })
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn export_root_cause_signature(
    report: &mondrian_export::queue::ExportColorHealthReport,
) -> Vec<(String, String, String, String)> {
    let mut signature = report
        .root_causes
        .iter()
        .filter(|root| root.code != "asset_color_diagnostics_warning")
        .map(|root| {
            (
                format!("{:?}", root.area),
                normalized_color_root_cause_code(root.code).to_owned(),
                export_color_report_severity(root.severity).to_owned(),
                root.evidence.clone(),
            )
        })
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn preview_action_signature(report: &PreviewColorHealthReport) -> Vec<(String, String)> {
    let mut signature = report
        .actions
        .iter()
        .map(|action| {
            (
                format!("{:?}", action.area),
                normalized_color_action_code(action.code).to_owned(),
            )
        })
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn export_action_signature(
    report: &mondrian_export::queue::ExportColorHealthReport,
) -> Vec<(String, String)> {
    let mut signature = report
        .actions
        .iter()
        .filter(|action| action.code != "inspect_asset_color_warning_evidence")
        .map(|action| {
            (
                format!("{:?}", action.area),
                normalized_color_action_code(action.code).to_owned(),
            )
        })
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn normalized_color_root_cause_code(code: &str) -> &str {
    match code {
        "missing_preview_color_evidence" | "missing_export_color_evidence" => {
            "missing_color_evidence"
        }
        "preview_gpu_color_stage_blocked" | "export_gpu_color_stage_blocked" => {
            "gpu_color_stage_blocked"
        }
        "preview_transfer_stage_present" | "export_transfer_stage_present" => {
            "transfer_stage_present"
        }
        "legacy_rgba8_composite_path" => "legacy_rgba8_composite_path",
        "input_color_policy_rejected_source" => "input_color_policy_rejected_source",
        other => other,
    }
}

fn normalized_color_action_code(code: &str) -> &str {
    match code {
        "inspect_preview_diagnostics" | "inspect_export_render_path" => "inspect_color_evidence",
        "inspect_preview_asset_color_diagnostics" | "inspect_asset_color_diagnostics" => {
            "inspect_asset_color_diagnostics"
        }
        "inspect_preview_gpu_blockers" | "inspect_export_gpu_blockers" => "inspect_gpu_blockers",
        "remove_preview_transfer_stage" | "remove_export_transfer_stage" => "remove_transfer_stage",
        "migrate_preview_legacy_composite_reason" | "migrate_legacy_composite_reason" => {
            "migrate_legacy_composite_reason"
        }
        other => other,
    }
}

fn preview_color_report_severity(severity: PreviewColorHealthSeverity) -> &'static str {
    match severity {
        PreviewColorHealthSeverity::Pass => "pass",
        PreviewColorHealthSeverity::Warn => "warn",
        PreviewColorHealthSeverity::Fail => "fail",
    }
}

fn export_color_report_severity(
    severity: mondrian_export::queue::ExportColorHealthSeverity,
) -> &'static str {
    match severity {
        mondrian_export::queue::ExportColorHealthSeverity::Pass => "pass",
        mondrian_export::queue::ExportColorHealthSeverity::Warn => "warn",
        mondrian_export::queue::ExportColorHealthSeverity::Fail => "fail",
    }
}

fn preview_asset_issue_summary_for_sequence(
    sequence: &Sequence,
    nested_sequences: &[Sequence],
    asset_color_diagnostics: &HashMap<AssetId, VideoColorDiagnostic>,
    depth: usize,
    asset_ids: &mut std::collections::HashSet<AssetId>,
) {
    if depth > mondrian_timeline::sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return;
    }

    for track in &sequence.video_tracks {
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            if clip.is_nested_sequence() {
                let Some(nested_sequence_id) = clip.nested_sequence_id() else {
                    continue;
                };
                let Some(nested) =
                    nested_sequences.iter().find(|sequence| sequence.id == nested_sequence_id)
                else {
                    continue;
                };
                preview_asset_issue_summary_for_sequence(
                    nested,
                    nested_sequences,
                    asset_color_diagnostics,
                    depth + 1,
                    asset_ids,
                );
                continue;
            }
            if let Some(asset_id) = clip.media_asset_id()
                && asset_color_diagnostics.contains_key(&asset_id)
            {
                asset_ids.insert(asset_id);
            }
        }
    }
}

#[test]
fn preview_dimensions_clamp_invalid_resolution_scale() {
    let mut below_min = Sequence::new("below");
    below_min.settings.preview.resolution_scale = 0.0;
    assert_eq!(preview_dimensions_for_sequence(&below_min), (240, 135));

    let mut above_max = Sequence::new("above");
    above_max.settings.preview.resolution_scale = 2.0;
    assert_eq!(preview_dimensions_for_sequence(&above_max), (1920, 1080));

    let mut invalid = Sequence::new("invalid");
    invalid.settings.preview.resolution_scale = f32::NAN;
    assert_eq!(preview_dimensions_for_sequence(&invalid), (960, 540));
}

#[test]
fn playback_quality_scale_multiplies_user_preview_resolution() {
    let sequence = Sequence::new("runtime scale");

    assert_eq!(
        preview_dimensions_for_sequence_at_runtime_scale(
            &sequence,
            mondrian_playback::PreviewResolutionScale::Full,
        ),
        (960, 540)
    );
    assert_eq!(
        preview_dimensions_for_sequence_at_runtime_scale(
            &sequence,
            mondrian_playback::PreviewResolutionScale::Half,
        ),
        (480, 270)
    );
    assert_eq!(
        preview_dimensions_for_sequence_at_runtime_scale(
            &sequence,
            mondrian_playback::PreviewResolutionScale::Quarter,
        ),
        (240, 135)
    );
}

#[test]
fn nested_solid_color_sequence_returns_preview_frame() {
    let mut state = AppState::new();
    let mut child = Sequence::new("child");
    let child_id = child.id;
    let child_tb = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color::from_rgba8(48, 120, 220, 255),
                tt(0, child_tb),
                tt(24, child_tb),
            )
            .expect("valid clip"),
        )
        .expect("child solid clip");

    let mut parent = Sequence::new("parent");
    let parent_tb = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child_id,
                tt(0, parent_tb),
                tt(24, parent_tb),
                Some("child".to_owned()),
            )
            .expect("valid clip"),
        )
        .expect("parent nested clip");

    state.test_add_sequence(child);
    state.test_set_sequence(Some(parent));
    state.seek(3).expect("seek");

    let service = WindowPreviewAdapter::new();
    let frame = service.viewer_preview_for_state(&state);
    let frame = ready_frame(frame);

    assert_eq!(frame.width, 960);
    assert_eq!(frame.height, 540);
    assert_eq!(frame.rgba.len(), 960 * 540 * 4);
}

#[test]
fn unsupported_media_plan_returns_no_partial_preview() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("media");
    let tb = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("valid clip"))
        .expect("media clip should be insertable");
    state.test_set_sequence(Some(sequence));

    let service = WindowPreviewAdapter::new();

    let ViewerPreviewState::Unavailable(reason) = service.viewer_preview_for_state(&state) else {
        panic!("missing authored media dependency must be unavailable");
    };
    assert_eq!(
        reason.disposition(),
        PreviewUnavailabilityDisposition::Blocked
    );
    assert_eq!(reason.stage(), PreviewOutputStage::MediaResolution);
    assert_eq!(reason.code(), "preview.blocked.media_resolution");
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.unavailability.observations, 1);
    assert_eq!(diagnostics.unavailability.blocked, 1);
    assert_eq!(diagnostics.unavailability.stages.media_resolution, 1);
    assert_eq!(service.last_unavailability(), Some(reason));
}

#[test]
fn empty_root_timeline_is_a_transparent_presentation_and_breaks_stale_reuse() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let _ = ready_frame(service.viewer_preview_for_state(&state));
    let sequence = state.active_sequence().expect("sequence");
    let sequence_id = sequence.id;
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    assert!(service.stale_frame_for_sequence(sequence, width, height).is_some());

    let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
    sequence.video_tracks[0].clips.clear();
    sequence.revision = sequence.revision.checked_next().expect("test author revision can advance");
    assert!(matches!(
        service.viewer_preview_for_state(&state),
        ViewerPreviewState::Transparent
    ));
    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(sequence.id, sequence_id);
    assert!(service.stale_frame_for_sequence(sequence, width, height).is_none());
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.unavailability.no_content, 0);
    assert_eq!(diagnostics.unavailability.stages.timeline_evaluation, 0);
}

#[test]
fn transparent_timeline_presentation_completes_the_exact_playback_demand() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
    let time_base = sequence.time_base();
    sequence.video_tracks[0].clips[0].position = tt(20, time_base);
    state.seek(0).expect("seek");
    state.play().expect("play");
    let demand = state.pending_playback_frame_demand_identity().expect("playback demand");

    assert!(matches!(
        service.viewer_preview_for_state(&state),
        ViewerPreviewState::Transparent
    ));
    assert_eq!(
        ViewerPlaybackFeedback::from_preview_state(&ViewerPreviewState::Transparent),
        ViewerPlaybackFeedback::Ready
    );
    let ticket = playback_presentation_ticket_for_state(&service, &state)
        .expect("transparent canvas presentation ticket");
    assert_eq!(ticket.identity(), demand);
    let completion = state
        .complete_frame_presentation(ticket, Instant::now())
        .expect("accepted transparent presentation");
    assert_eq!(
        completion.delivery().kind(),
        mondrian_playback::FrameDeliveryKind::Ready
    );
    assert!(state.pending_playback_frame_demand_identity().is_none());
    assert_eq!(state.playback_evidence_report().deliveries.ready, 1);
}

#[test]
fn repeated_same_viewer_request_does_not_obsolete_in_flight_decode() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("media");
    let tb = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("valid clip"))
        .expect("media clip should be insertable");
    state.test_set_sequence(Some(sequence));
    state.seek(3).expect("seek");

    let service = WindowPreviewAdapter::new();
    let _ = service.viewer_preview_for_state(&state);
    let first_generation = service.diagnostics().scheduler.latest_generation;
    let _ = service.viewer_preview_for_state(&state);
    let second_generation = service.diagnostics().scheduler.latest_generation;

    assert_eq!(first_generation, second_generation);

    state.seek(4).expect("seek");
    let _ = service.viewer_preview_for_state(&state);
    let third_generation = service.diagnostics().scheduler.latest_generation;

    assert!(third_generation > second_generation);
}

#[test]
fn preview_raster_key_changes_when_render_plan_changes() {
    let service = WindowPreviewAdapter::new();
    let first = service.viewer_preview_for_state(&state_with_solid_color_clip(Color::from_rgba8(
        255, 0, 0, 255,
    )));
    let first = ready_frame(first);
    let second = service.viewer_preview_for_state(&state_with_solid_color_clip(Color::from_rgba8(
        0, 0, 255, 255,
    )));
    let second = ready_frame(second);

    assert_ne!(first.key, second.key);
}

#[test]
fn deterministic_solid_preview_reuses_raster_key_across_frames() {
    let service = WindowPreviewAdapter::new();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(255, 128, 0, 255));

    let first = ready_frame(service.viewer_preview_for_state(&state));
    state.seek(5).expect("seek");
    let second = ready_frame(service.viewer_preview_for_state(&state));

    assert_eq!(first.rgba, second.rgba);
    assert_eq!(first.key, second.key);
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.render_requests, 2);
    assert_eq!(diagnostics.ready_frames, 2);
    assert_eq!(
        diagnostics.visual_program_cache.author_fingerprint_evaluations,
        1
    );
    assert_eq!(
        diagnostics.visual_program_cache.author_snapshot_binding_misses,
        1
    );
    assert!(
        diagnostics.visual_program_cache.author_snapshot_binding_hits >= 1,
        "the second frame must reuse the exact author-generation Program binding"
    );
    assert!(diagnostics.frame_store.viewer_entries >= 1);
    assert!(diagnostics.color_output_transform_calls >= 1);
    assert_eq!(
        diagnostics.color_output_transform_pixels,
        diagnostics.color_output_transform_calls * 960_u64 * 540
    );
    assert_eq!(
        diagnostics.color_stage_plans,
        diagnostics.color_output_transform_calls
    );
    assert_eq!(
        diagnostics.color_intermediate_transform_calls,
        diagnostics.color_output_transform_calls
    );
    let color_stage_passes = diagnostics
        .color_output_transform_calls
        .saturating_add(diagnostics.color_intermediate_transform_calls);
    assert_eq!(diagnostics.color_stage_total_stages, color_stage_passes);
    assert_eq!(
        diagnostics.color_stage_cpu_output_stages,
        color_stage_passes
    );
    assert_eq!(
        diagnostics.color_stage_pixels,
        color_stage_passes * 960_u64 * 540
    );
    assert_eq!(
        diagnostics.color_composite_plans,
        diagnostics.color_output_transform_calls
    );
    assert_eq!(
        diagnostics.color_composite_float_linear,
        diagnostics.color_composite_plans
    );
    assert_eq!(diagnostics.color_composite_legacy_rgba8, 0);
}

#[test]
fn resolved_media_preview_cache_key_includes_media_frame_identity() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let make_plan = |identity_revision| {
        vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, identity_revision),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            prepared_heterogeneous_route: None,
            frame_seed: 12,
        }]
    };
    let sequence_id = SequenceId::new();

    let color_context = test_color_context(ColorSpace::Rec709);
    let first = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        320,
        180,
        &make_plan(100),
        &color_context,
    );
    let second = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        320,
        180,
        &make_plan(200),
        &color_context,
    );

    assert_ne!(first, second);
}

#[test]
fn uncacheable_effect_retains_semantic_identity_without_admitting_viewer_reuse() {
    let effect_graph = custom_u8_effect_graph(
        "uncacheable",
        EffectCachePolicy::Uncacheable,
        Arc::new(|_buffer, _width, _height, _params, _frame_seed| Ok(())),
    );
    let resolved = vec![ResolvedPreviewElement::SolidColor(
        TimelineSolidColorLayer {
            color: Color::from_rgba8(48, 96, 192, 255),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 12,
        },
    )];
    let sequence_id = SequenceId::new();
    let color_context = test_color_context(ColorSpace::Rec709);

    let first = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        320,
        180,
        &resolved,
        &color_context,
    );
    let second = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        320,
        180,
        &resolved,
        &color_context,
    );

    assert_eq!(first, second, "semantic identity remains available");
    assert!(
        !viewer_preview_plan_allows_cross_call_reuse(&resolved),
        "semantic identity must not become cache admission"
    );
    assert_ne!(
        first.with_execution_nonce(1),
        second.with_execution_nonce(2),
        "separate executions of an uncacheable semantic plan must receive distinct output keys"
    );
}

#[test]
fn stable_parameter_value_changes_compiled_graph_and_viewer_cache_identity() {
    let parameter_id = mondrian_core::effect_data::EffectType::GaussianBlur
        .parameter_id("radius")
        .expect("stable radius parameter ID");
    let mut effect = mondrian_core::effect_data::EffectNode::with_defaults(
        mondrian_core::effect_data::EffectType::GaussianBlur,
    );
    effect
        .set_static_value_by_parameter(
            &parameter_id,
            mondrian_core::automation::PropertyValue::Float(4.0),
        )
        .expect("set first radius");
    let first_graph = mondrian_effects::compile_clip_effect_graph(
        &[effect.clone()],
        &[],
        mondrian_core::TimelineTime::ZERO,
        WorkingColorSpace::LinearRec709,
    )
    .expect("compile first graph");
    effect
        .set_static_value_by_parameter(
            &parameter_id,
            mondrian_core::automation::PropertyValue::Float(12.0),
        )
        .expect("set second radius");
    let second_graph = mondrian_effects::compile_clip_effect_graph(
        &[effect],
        &[],
        mondrian_core::TimelineTime::ZERO,
        WorkingColorSpace::LinearRec709,
    )
    .expect("compile second graph");
    assert_ne!(
        first_graph.semantic_fingerprint(),
        second_graph.semantic_fingerprint()
    );

    let make_plan = |effect_graph| {
        vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            prepared_heterogeneous_route: None,
            frame_seed: 12,
        }]
    };
    let sequence_id = SequenceId::new();
    let color_context = test_color_context(ColorSpace::Rec709);
    let first = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        320,
        180,
        &make_plan(first_graph),
        &color_context,
    );
    let second = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        320,
        180,
        &make_plan(second_graph),
        &color_context,
    );
    assert_ne!(first, second);
}

#[test]
fn resolved_media_preview_cache_key_includes_color_context() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let resolved = vec![ResolvedPreviewElement::Media {
        frame: test_media_frame_with_size(0, 2, 2, 100),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 12,
    }];
    let sequence_id = SequenceId::new();

    let rec709 = test_color_context(ColorSpace::Rec709);
    let srgb = test_color_context(ColorSpace::Srgb);
    let first =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &rec709);
    let second =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &srgb);

    assert_ne!(first, second);
}

#[test]
fn resolved_media_preview_cache_key_includes_resolved_display_color_space() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let resolved = vec![ResolvedPreviewElement::Media {
        frame: test_media_frame_with_size(0, 2, 2, 100),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 12,
    }];
    let sequence_id = SequenceId::new();

    let sdr = test_color_context(ColorSpace::Rec709);
    let p3 = test_color_context(ColorSpace::DisplayP3);
    let first = viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &sdr);
    let second = viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &p3);

    assert_ne!(first, second);
}

#[test]
fn resolved_media_preview_cache_key_includes_versioned_output_transform_intent() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let resolved = vec![ResolvedPreviewElement::Media {
        frame: test_media_frame_with_size(0, 2, 2, 100),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 12,
    }];
    let sequence_id = SequenceId::new();

    let current = test_color_context(ColorSpace::Rec709);
    let legacy = test_color_context_with_engine(
        ColorSpace::Rec709,
        ColorEngine::MondrianStandard {
            package: mondrian_core::MondrianStandardPackageIdentity::V2,
        },
    );
    let first =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &current);
    let second =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &legacy);

    assert_ne!(first, second);
}

#[test]
fn resolved_media_preview_cache_key_includes_output_transform_intent() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let resolved = vec![ResolvedPreviewElement::Media {
        frame: test_media_frame_with_size(0, 2, 2, 100),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 12,
    }];
    let sequence_id = SequenceId::new();

    let standard = test_color_context(ColorSpace::Rec709);
    let colorimetric = test_colorimetric_context(ColorSpace::Rec709);
    let first =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &standard);
    let second =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &colorimetric);

    assert_ne!(first, second);
}

#[test]
fn resolved_media_preview_cache_key_includes_exact_standard_package() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let resolved = vec![ResolvedPreviewElement::Media {
        frame: test_media_frame_with_size(0, 2, 2, 100),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        prepared_heterogeneous_route: None,
        frame_seed: 12,
    }];
    let sequence_id = SequenceId::new();
    let current = test_color_context(ColorSpace::Rec709);
    let legacy_package = mondrian_core::MondrianStandardPackageIdentity::V2;
    let legacy = test_color_context_with_engine(
        ColorSpace::Rec709,
        ColorEngine::MondrianStandard { package: legacy_package },
    );

    let current_key =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &current);
    let legacy_key =
        viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &legacy);

    assert_ne!(current_key, legacy_key);
}

#[test]
fn preview_working_composite_boundary_uses_resolved_display_view() {
    let color_context = test_color_context(ColorSpace::Rec709);
    let mut scratch = TimelineCompositeScratch::default();

    let _output = composite_resolved_preview_working(2, 2, &[], &color_context, &mut scratch)
        .expect("empty preview composite");

    let boundary =
        output_boundary_from_color_context(&color_context).expect("encoded preview output");
    let display_view = boundary.ocio_display_view().expect("resolved display/view");
    assert_eq!(display_view.display, "Rec.1886 Rec.709 - Display");
    assert_eq!(display_view.view, "Mondrian Standard SDR v2");
}

#[test]
fn preview_boundary_uses_colorimetric_intent_when_tone_map_is_disabled() {
    let color_context = test_colorimetric_context(ColorSpace::Rec709);

    let boundary =
        output_boundary_from_color_context(&color_context).expect("encoded preview output");

    assert_eq!(boundary.ocio_display_view(), None);
    assert!(!boundary.tone_map());
}

#[test]
fn preview_input_color_resolution_honors_override_metadata_and_missing_policy() {
    let mut color_context = test_color_context(ColorSpace::Rec709).media_input(true);
    color_context.working_color_space = WorkingColorSpace::LinearRec2020;
    color_context.missing_metadata_policy = MissingColorMetadataPolicy::AssumeRec709;

    assert_eq!(
        resolve_preview_input_color_space(
            Some(ColorSpace::SonySLog3SGamut3Cine),
            AssetMediaInterpretation::default(),
            Some(ColorSpace::Srgb),
            &color_context,
        )
        .resolved,
        ResolvedInputColor::Color(ColorSpace::SonySLog3SGamut3Cine)
    );
    assert_eq!(
        resolve_preview_input_color_space(
            Some(ColorSpace::SonySLog3SGamut3Cine),
            AssetMediaInterpretation::default(),
            Some(ColorSpace::Srgb),
            &color_context,
        )
        .source,
        mondrian_timeline::sequence::InputColorResolutionSource::Override
    );
    assert_eq!(
        resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            Some(ColorSpace::Srgb),
            &color_context,
        ),
        mondrian_timeline::sequence::InputColorResolution {
            resolved: ResolvedInputColor::Color(ColorSpace::Srgb),
            source: mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata,
            override_color_space: None,
            executable_color_space: Some(ColorSpace::Srgb),
            missing_metadata_policy: color_context.missing_metadata_policy,
            working_color_space: color_context.working_color_space,
        }
    );
    assert_eq!(
        resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            None,
            &color_context,
        )
        .resolved,
        ResolvedInputColor::Color(ColorSpace::Rec709)
    );
    assert_eq!(
        resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            None,
            &color_context,
        )
        .source,
        mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeRec709
    );

    let asset_override = resolve_preview_input_color_space(
        None,
        AssetMediaInterpretation {
            color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                color_space: ColorSpace::AppleLogBt2020,
            },
            ..AssetMediaInterpretation::default()
        },
        Some(ColorSpace::Srgb),
        &color_context,
    );
    assert_eq!(
        asset_override.resolved,
        ResolvedInputColor::Color(ColorSpace::AppleLogBt2020)
    );
    assert_eq!(
        asset_override.source,
        mondrian_timeline::sequence::InputColorResolutionSource::Override
    );

    let data = resolve_preview_input_color_space(
        None,
        AssetMediaInterpretation {
            payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
            ..AssetMediaInterpretation::default()
        },
        Some(ColorSpace::Srgb),
        &color_context,
    );
    assert_eq!(data.resolved, ResolvedInputColor::Data);
    assert_eq!(
        data.source,
        mondrian_timeline::sequence::InputColorResolutionSource::DataTexture
    );

    color_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
    assert_eq!(
        resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            None,
            &color_context,
        )
        .resolved,
        ResolvedInputColor::Rejected
    );
    assert_eq!(
        resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            None,
            &color_context,
        )
        .source,
        mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyRejectMedia
    );
}

#[test]
fn preview_color_rejection_preserves_resolution_and_media_diagnostic() {
    let service = WindowPreviewAdapter::new();
    let mut color_context = test_color_context(ColorSpace::Rec709).media_input(true);
    color_context.working_color_space = WorkingColorSpace::LinearRec2020;
    color_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
    let resolution = resolve_preview_input_color_space(
        None,
        AssetMediaInterpretation::default(),
        None,
        &color_context,
    );
    let asset_id = AssetId::new();
    let path = PathBuf::from("E:/media/missing-color-tags.mov");
    let diagnostic =
        "source=MissingMetadata,method=MissingMetadata,warnings=missing_cicp".to_string();
    let issue_summary = VideoColorDiagnosticIssueSummary {
        source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
        method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
        confidence: mondrian_media::VideoColorInterpretationConfidence::None,
        missing_cicp_tags: 1,
        unsupported_cicp_tags: 0,
        has_user_visible_warnings: true,
        ..VideoColorDiagnosticIssueSummary {
            executable_color_space: None,
            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
            confidence: mondrian_media::VideoColorInterpretationConfidence::None,
            has_raw_cicp_metadata: false,
            metadata_hint_count: 0,
            evidence_count: 0,
            warning_count: 0,
            multiple_metadata_hints: 0,
            ignored_metadata_hints: 0,
            metadata_hint_overrides_cicp_tags: 0,
            lower_priority_metadata_hints: 0,
            ignored_lower_priority_metadata_hints: 0,
            partial_cicp_tags: 0,
            missing_cicp_tags: 0,
            unsupported_cicp_tags: 0,
            decoder_unavailable: 0,
            hdr_side_data_count: 0,
            has_mastering_display_metadata: false,
            has_content_light_metadata: false,
            has_dynamic_hdr10_plus: false,
            has_dolby_vision_config: false,
            has_icc_profile: false,
            icc_cicp_mismatch: 0,
            icc_profile_unmapped: 0,
            has_user_visible_warnings: false,
        }
    };

    service.record_color_rejection(PreviewColorRejection::new(
        asset_id,
        path.clone(),
        resolution,
        diagnostic.clone(),
        issue_summary,
    ));

    let rejection = service.last_color_rejection().expect("preview color rejection");
    assert_eq!(rejection.asset_id, asset_id);
    assert_eq!(rejection.path, path);
    assert_eq!(
        rejection.missing_metadata_policy,
        MissingColorMetadataPolicy::RejectMedia
    );
    assert_eq!(
        rejection.source,
        InputColorResolutionSource::MissingPolicyRejectMedia
    );
    assert_eq!(rejection.override_color_space, None);
    assert_eq!(rejection.executable_color_space, None);
    assert_eq!(
        rejection.working_color_space,
        WorkingColorSpace::LinearRec2020
    );
    assert_eq!(rejection.diagnostic_summary, diagnostic);
    assert_eq!(rejection.diagnostic_issue_summary, issue_summary);
}

#[test]
fn preview_render_request_clears_stale_color_rejection() {
    let service = WindowPreviewAdapter::new();
    let color_context = test_color_context(ColorSpace::Rec709).media_input(true);
    service.record_color_rejection(PreviewColorRejection::new(
        AssetId::new(),
        PathBuf::from("E:/media/old.mov"),
        resolve_preview_input_color_space(
            None,
            AssetMediaInterpretation::default(),
            None,
            &color_context,
        ),
        "old".to_string(),
        VideoColorDiagnosticIssueSummary {
            executable_color_space: None,
            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
            confidence: mondrian_media::VideoColorInterpretationConfidence::None,
            has_raw_cicp_metadata: false,
            metadata_hint_count: 0,
            evidence_count: 0,
            warning_count: 0,
            multiple_metadata_hints: 0,
            ignored_metadata_hints: 0,
            metadata_hint_overrides_cicp_tags: 0,
            lower_priority_metadata_hints: 0,
            ignored_lower_priority_metadata_hints: 0,
            partial_cicp_tags: 0,
            missing_cicp_tags: 0,
            unsupported_cicp_tags: 0,
            decoder_unavailable: 0,
            hdr_side_data_count: 0,
            has_mastering_display_metadata: false,
            has_content_light_metadata: false,
            has_dynamic_hdr10_plus: false,
            has_dolby_vision_config: false,
            has_icc_profile: false,
            icc_cicp_mismatch: 0,
            icc_profile_unmapped: 0,
            has_user_visible_warnings: false,
        },
    ));
    assert!(service.last_color_rejection().is_some());

    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let _ = ready_frame(service.viewer_preview_for_state(&state));

    assert_eq!(service.last_color_rejection(), None);
}

fn export_test_media_dependencies(
    asset_ids: impl IntoIterator<Item = AssetId>,
    color_spaces: &HashMap<AssetId, ColorSpace>,
    interpretations: &HashMap<AssetId, AssetMediaInterpretation>,
    diagnostics: &HashMap<AssetId, VideoColorDiagnostic>,
) -> HashMap<AssetId, mondrian_export::preset::ExportMediaDependency> {
    asset_ids
        .into_iter()
        .map(|asset_id| {
            let path = PathBuf::from(format!("preview-export-parity-{asset_id}.mov"));
            let color_diagnostic = diagnostics.get(&asset_id).cloned().or_else(|| {
                color_spaces.get(&asset_id).copied().map(trusted_test_color_diagnostic)
            });
            (
                asset_id,
                mondrian_export::preset::ExportMediaDependency {
                    source_fingerprint: MediaFileFingerprint::capture(path.as_path()),
                    path,
                    source_container: String::new(),
                    source_video_stream: None,
                    video_stream_index: Some(0),
                    picture_source_extent: Some(mondrian_timeline::PictureSourceExtent::Still),
                    source_resolution: Some(Resolution { width: 1, height: 1 }),
                    picture: Some(Default::default()),
                    audio_components: HashMap::new(),
                    interpretation: interpretations.get(&asset_id).copied().unwrap_or_default(),
                    color_diagnostic,
                },
            )
        })
        .collect()
}

fn trusted_test_color_diagnostic(color_space: ColorSpace) -> VideoColorDiagnostic {
    assert_eq!(
        color_space,
        ColorSpace::Srgb,
        "extend this fixture with closed executable evidence before using another identity"
    );
    let sampling = mondrian_media::ProvenVideoSampling {
        pixel_format: mondrian_core::PixelFormat::Rgb24,
        bit_depth: 8,
        has_alpha: false,
    };
    let metadata = mondrian_media::VideoColorMetadata {
        primaries: mondrian_media::VideoColorTag {
            code: 1,
            name: Some("bt709".to_owned()),
            specified: true,
        },
        transfer: mondrian_media::VideoColorTag {
            code: 13,
            name: Some("iec61966-2-1".to_owned()),
            specified: true,
        },
        matrix: mondrian_media::VideoColorTag {
            code: 0,
            name: Some("gbr".to_owned()),
            specified: true,
        },
    };
    let interpretation =
        mondrian_media::interpret_video_color_metadata(&metadata, Some(sampling), &[]);
    assert_eq!(interpretation.candidate_color_space, Some(color_space));
    VideoColorDiagnostic {
        color_range: DecodedVideoRange::Unknown,
        sampling: Some(sampling),
        interpretation,
        metadata: Some(metadata),
        metadata_hints: Vec::new(),
        hdr_metadata: Vec::new(),
    }
}

fn captured_title_free_export_test_snapshot(
    color_environment: mondrian_core::ProjectColorEnvironment,
    sequence: Sequence,
    sequences: Vec<Sequence>,
    media: HashMap<AssetId, mondrian_export::preset::ExportMediaDependency>,
    range: mondrian_export::preset::TimelineExportRange,
) -> mondrian_export::preset::TimelineExportSnapshot {
    let prepared =
        mondrian_export::prepare_timeline_export_dependencies(&sequence, &sequences, range, false)
            .expect("prepare immutable export execution snapshot");
    let visual = prepared.execution_snapshot().visual();
    assert!(
        visual.basic_title_font_queries().is_empty(),
        "this title-free fixture must not bypass exact Basic Title font capture"
    );
    assert!(
        visual.title_fonts().is_some(),
        "an empty Basic Title query set must be frozen as an exact empty closure"
    );
    mondrian_export::preset::TimelineExportSnapshot::captured(
        color_environment,
        sequence,
        sequences,
        media,
        range,
        prepared.execution_snapshot().clone(),
    )
}

#[test]
fn preview_and_export_input_color_resolution_counts_match_for_frame() {
    let mut sequence = Sequence::new("preview-export-color-resolution-parity");
    sequence.settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
    sequence.settings.color.input.missing_metadata_policy =
        MissingColorMetadataPolicy::AssumeRec709;
    let tb = sequence.time_base();
    let detected_id = AssetId::new();
    let override_id = AssetId::new();
    let missing_id = AssetId::new();
    let data_id = AssetId::new();

    sequence.video_tracks[0]
        .add_clip(Clip::new(detected_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
        .expect("add detected clip");
    for (name, asset_id) in [
        ("override", override_id),
        ("missing", missing_id),
        ("data", data_id),
    ] {
        let mut track = Track::new_video(name);
        track
            .add_clip(Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
            .expect("add clip");
        sequence.video_tracks.push(track);
    }

    let mut asset_color_spaces = HashMap::new();
    asset_color_spaces.insert(detected_id, ColorSpace::Srgb);
    let mut asset_interpretations = HashMap::new();
    asset_interpretations.insert(
        override_id,
        AssetMediaInterpretation {
            color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
            ..AssetMediaInterpretation::default()
        },
    );
    asset_interpretations.insert(
        data_id,
        AssetMediaInterpretation {
            payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
            ..AssetMediaInterpretation::default()
        },
    );
    let preview_counts = preview_input_color_resolution_counts_for_frame(
        &sequence,
        &[],
        &asset_color_spaces,
        &asset_interpretations,
        &mondrian_core::ProjectColorEnvironment::default(),
        0,
    )
    .expect("preview counts");
    let media = export_test_media_dependencies(
        [detected_id, override_id, missing_id, data_id],
        &asset_color_spaces,
        &asset_interpretations,
        &HashMap::new(),
    );
    let export_counts = mondrian_export::queue::export_input_color_resolution_counts_for_frame(
        &captured_title_free_export_test_snapshot(
            mondrian_core::ProjectColorEnvironment::default(),
            sequence,
            Vec::new(),
            media,
            mondrian_export::preset::TimelineExportRange::SequenceInOut,
        ),
        0,
    )
    .expect("export counts");

    assert_eq!(preview_counts, export_counts);
    assert_eq!(preview_counts.total(), 4);
    assert_eq!(
        preview_counts
            .count(mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata),
        1
    );
    assert_eq!(
        preview_counts.count(mondrian_timeline::sequence::InputColorResolutionSource::Override),
        1
    );
    assert_eq!(
        preview_counts.count(
            mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeRec709
        ),
        1
    );
    assert_eq!(
        preview_counts.count(mondrian_timeline::sequence::InputColorResolutionSource::DataTexture),
        1
    );
}

#[test]
fn preview_and_export_nested_input_color_resolution_counts_match_for_frame() {
    let mut parent = Sequence::new("parent-color-resolution-parity");
    parent.settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
    parent.settings.color.input.missing_metadata_policy = MissingColorMetadataPolicy::AssumeRec709;
    let mut nested = Sequence::new("nested-color-resolution-parity");
    nested.settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
    nested.settings.color.input.missing_metadata_policy = MissingColorMetadataPolicy::AssumeRec709;

    let parent_tb = parent.time_base();
    let nested_tb = nested.time_base();
    let parent_override_id = AssetId::new();
    let nested_detected_id = AssetId::new();
    let nested_data_id = AssetId::new();
    let nested_missing_id = AssetId::new();

    parent.video_tracks[0]
        .add_clip(
            Clip::new(parent_override_id, tt(0, parent_tb), tt(10, parent_tb)).expect("valid clip"),
        )
        .expect("add parent media clip");
    let mut nested_track = Track::new_video("nested");
    nested_track
        .add_clip(
            Clip::new_nested_sequence(
                nested.id,
                tt(0, parent_tb),
                tt(10, parent_tb),
                Some("Nested".to_owned()),
            )
            .expect("valid clip"),
        )
        .expect("add nested sequence clip");
    parent.video_tracks.push(nested_track);

    nested.video_tracks[0]
        .add_clip(
            Clip::new(nested_detected_id, tt(0, nested_tb), tt(10, nested_tb)).expect("valid clip"),
        )
        .expect("add nested detected clip");
    for (name, asset_id) in [("data", nested_data_id), ("missing", nested_missing_id)] {
        let mut track = Track::new_video(name);
        track
            .add_clip(Clip::new(asset_id, tt(0, nested_tb), tt(10, nested_tb)).expect("valid clip"))
            .expect("add nested media clip");
        nested.video_tracks.push(track);
    }

    let mut asset_color_spaces = HashMap::new();
    asset_color_spaces.insert(nested_detected_id, ColorSpace::Srgb);
    let mut asset_interpretations = HashMap::new();
    asset_interpretations.insert(
        parent_override_id,
        AssetMediaInterpretation {
            color: mondrian_core::timeline_data::MediaColorInterpretation::Override {
                color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
            ..AssetMediaInterpretation::default()
        },
    );
    asset_interpretations.insert(
        nested_data_id,
        AssetMediaInterpretation {
            payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
            ..AssetMediaInterpretation::default()
        },
    );
    let nested_sequences = vec![nested.clone()];

    let preview_counts = preview_input_color_resolution_counts_for_frame(
        &parent,
        &nested_sequences,
        &asset_color_spaces,
        &asset_interpretations,
        &mondrian_core::ProjectColorEnvironment::default(),
        0,
    )
    .expect("preview nested counts");
    let media = export_test_media_dependencies(
        [
            parent_override_id,
            nested_detected_id,
            nested_data_id,
            nested_missing_id,
        ],
        &asset_color_spaces,
        &asset_interpretations,
        &HashMap::new(),
    );
    let export_counts = mondrian_export::queue::export_input_color_resolution_counts_for_frame(
        &captured_title_free_export_test_snapshot(
            mondrian_core::ProjectColorEnvironment::default(),
            parent,
            nested_sequences,
            media,
            mondrian_export::preset::TimelineExportRange::SequenceInOut,
        ),
        0,
    )
    .expect("export nested counts");

    assert_eq!(preview_counts, export_counts);
    assert_eq!(preview_counts.total(), 4);
    assert_eq!(
        preview_counts.count(mondrian_timeline::sequence::InputColorResolutionSource::Override),
        1
    );
    assert_eq!(
        preview_counts
            .count(mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata),
        1
    );
    assert_eq!(
        preview_counts.count(mondrian_timeline::sequence::InputColorResolutionSource::DataTexture),
        1
    );
    assert_eq!(
        preview_counts.count(
            mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyAssumeRec709
        ),
        1
    );
}

#[test]
fn preview_and_export_asset_issue_summaries_match_for_referenced_assets() {
    let mut parent = Sequence::new("parent-asset-issue-parity");
    let mut nested = Sequence::new("nested-asset-issue-parity");
    let nested_id = nested.id;
    let parent_tb = parent.time_base();
    let nested_tb = nested.time_base();
    let direct_id = AssetId::new();
    let nested_asset_id = AssetId::new();
    let unused_id = AssetId::new();

    parent.video_tracks[0]
        .add_clip(Clip::new(direct_id, tt(0, parent_tb), tt(10, parent_tb)).expect("valid clip"))
        .expect("add direct media clip");
    let mut nested_track = Track::new_video("nested");
    nested_track
        .add_clip(
            Clip::new_nested_sequence(
                nested_id,
                tt(0, parent_tb),
                tt(10, parent_tb),
                Some("Nested".to_owned()),
            )
            .expect("valid clip"),
        )
        .expect("add nested sequence clip");
    parent.video_tracks.push(nested_track);

    nested.video_tracks[0]
        .add_clip(
            Clip::new(nested_asset_id, tt(0, nested_tb), tt(10, nested_tb)).expect("valid clip"),
        )
        .expect("add nested media clip");

    let mut asset_color_diagnostics = HashMap::new();
    asset_color_diagnostics.insert(
        direct_id,
        mondrian_media::VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: mondrian_media::DetectedColorInterpretation {
                candidate_color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                evidence: Vec::new(),
                warnings: vec![mondrian_media::VideoColorInterpretationWarning::MissingCicpTags],
                user_overridable: true,
            },
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        },
    );
    asset_color_diagnostics.insert(
        nested_asset_id,
        mondrian_media::VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: mondrian_media::DetectedColorInterpretation {
                candidate_color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source: mondrian_media::VideoColorSpaceSource::DecoderUnavailable,
                method: mondrian_media::VideoColorDetectionMethod::DecoderUnavailable,
                evidence: vec![
                    mondrian_media::VideoColorInterpretationEvidence::DecoderUnavailable,
                ],
                warnings: vec![mondrian_media::VideoColorInterpretationWarning::DecoderUnavailable],
                user_overridable: true,
            },
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        },
    );
    asset_color_diagnostics.insert(
        unused_id,
        mondrian_media::VideoColorDiagnostic {
            color_range: DecodedVideoRange::Limited,
            sampling: None,
            interpretation: mondrian_media::DetectedColorInterpretation {
                candidate_color_space: Some(ColorSpace::Rec709),
                confidence: mondrian_media::VideoColorInterpretationConfidence::Low,
                source: mondrian_media::VideoColorSpaceSource::Metadata,
                method: mondrian_media::VideoColorDetectionMethod::MetadataHint,
                evidence: vec![
                    mondrian_media::VideoColorInterpretationEvidence::MetadataHint {
                        scope: mondrian_media::VideoColorMetadataHintScope::FileName,
                        key: "filename".to_owned(),
                        value: "unused-rec709.mov".to_owned(),
                        detected_color_space: ColorSpace::Rec709,
                        authority:
                            mondrian_media::VideoColorMetadataHintAuthority::DiagnosticSuggestion,
                    },
                ],
                warnings: vec![
                    mondrian_media::VideoColorInterpretationWarning::PartialCicpTags {
                        detected_color_space: ColorSpace::Rec709,
                    },
                ],
                user_overridable: true,
            },
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        },
    );

    let nested_sequences = vec![nested.clone()];
    let mut preview_asset_ids = std::collections::HashSet::new();
    preview_asset_issue_summary_for_sequence(
        &parent,
        &nested_sequences,
        &asset_color_diagnostics,
        0,
        &mut preview_asset_ids,
    );
    let mut preview_summary = mondrian_media::VideoColorDiagnosticIssueAggregate::default();
    for asset_id in preview_asset_ids {
        preview_summary.observe(
            asset_color_diagnostics.get(&asset_id).expect("preview referenced diagnostic"),
        );
    }

    let media = export_test_media_dependencies(
        [direct_id, nested_asset_id, unused_id],
        &HashMap::new(),
        &HashMap::new(),
        &asset_color_diagnostics,
    );
    let prepared_visual = mondrian_export::prepare_timeline_export_dependencies(
        &parent,
        &nested_sequences,
        mondrian_export::preset::TimelineExportRange::SequenceInOut,
        false,
    )
    .expect("prepare immutable export visual snapshot");
    let export_summary = mondrian_export::queue::export_media_diagnostic_set(
        &mondrian_export::preset::TimelineExportSnapshot::captured(
            mondrian_core::ProjectColorEnvironment::default(),
            parent,
            nested_sequences,
            media,
            mondrian_export::preset::TimelineExportRange::SequenceInOut,
            prepared_visual.execution_snapshot().clone(),
        ),
    )
    .expect("prepare export media diagnostic set")
    .issue_summary;

    assert_eq!(preview_summary, export_summary);
    assert_eq!(preview_summary.diagnostics, 2);
    assert_eq!(preview_summary.method_missing_metadata, 1);
    assert_eq!(preview_summary.method_decoder_unavailable, 1);
    assert_eq!(preview_summary.method_metadata_hint, 0);
    assert_eq!(preview_summary.warning_count, 2);
}

#[test]
fn preview_and_export_composite_color_path_summaries_match_for_frame() {
    let mut sequence = Sequence::new("preview-export-composite-diagnostics-parity");
    let tb = sequence.time_base();
    let mut solid = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(64, 96, 220, 255),
        tt(0, tb),
        tt(10, tb),
    )
    .expect("valid clip");
    solid.transform.set_scale(glam::Vec2::new(0.75, 0.75));
    let mut blur: mondrian_effects::EffectNode =
        mondrian_effects::EffectNodeExt::with_defaults(mondrian_effects::EffectType::GaussianBlur);
    blur.set_static_value_by_parameter(
        &mondrian_effects::EffectType::GaussianBlur
            .parameter_id("radius")
            .expect("blur radius parameter ID"),
        mondrian_core::automation::PropertyValue::Float(1.0),
    )
    .expect("set test blur radius");
    solid.add_effect_node(blur);
    solid.masks.push(mondrian_core::mask_data::MaskComponent::new(
        "float-path-mask".to_owned(),
        mondrian_core::mask_data::MaskEvaluation {
            shape: mondrian_core::mask_data::MaskShape::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                corner_radius: 0.0,
            },
            opacity: 0.5,
            ..Default::default()
        },
    ));
    sequence.video_tracks[0]
        .add_clip(solid)
        .expect("add transformed solid color clip");

    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence.clone()));
    state.seek(0).expect("seek");
    let preview_service = WindowPreviewAdapter::new();
    let preview_frame = preview_service.viewer_preview_for_state(&state);
    let preview_frame = ready_frame(preview_frame);
    let preview_summary = preview_service.diagnostics().composite_color_path_summary();

    let export_diagnostics = mondrian_export::queue::export_composite_diagnostics_for_frame(
        &captured_title_free_export_test_snapshot(
            mondrian_core::ProjectColorEnvironment::default(),
            sequence,
            Vec::new(),
            HashMap::new(),
            mondrian_export::preset::TimelineExportRange::SequenceInOut,
        ),
        0,
        preview_frame.width,
        preview_frame.height,
    )
    .expect("export composite diagnostics");
    let export_summary = export_diagnostics.color_path_summary();

    assert_eq!(
        preview_summary.path,
        TimelineCompositeColorPath::FloatLinear
    );
    assert_eq!(preview_summary.path, export_summary.path);
    assert_eq!(preview_summary.elements, export_summary.elements);
    assert_eq!(
        preview_summary.float_linear_composites,
        export_summary.float_linear_composites
    );
    assert_eq!(
        preview_summary.legacy_rgba8_composites,
        export_summary.legacy_rgba8_composites
    );
    assert_eq!(
        preview_summary.legacy_breakdown.solid_transform,
        export_summary.legacy_breakdown.solid_transform
    );
    assert_eq!(preview_summary.legacy_breakdown.solid_effect, 0);
    assert_eq!(export_summary.legacy_breakdown.solid_effect, 0);
    assert_eq!(
        preview_summary.legacy_breakdown.total(),
        export_summary.legacy_breakdown.total()
    );
}

#[test]
fn preview_single_media_color_output_matches_export_composite_contract() {
    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let color_context = test_color_context(ColorSpace::Srgb);
    let frame = test_media_frame_rgba_in_working(
        vec![200, 100, 40, 255],
        1,
        1,
        77,
        color_context.working_color_space(),
    );
    assert_eq!(
        frame.working_color_space(),
        Some(color_context.working_color_space()),
        "Preview media fixtures must honor the Sequence working-space contract"
    );
    let resolved = vec![ResolvedPreviewElement::Media {
        frame: frame.clone(),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: Arc::clone(&effect_graph),
        prepared_heterogeneous_route: None,
        frame_seed: 0,
    }];
    let mut preview_scratch = TimelineCompositeScratch::default();
    let preview_service = WindowPreviewAdapter::new();
    let preview = composite_resolved_preview(1, 1, &resolved, &color_context, &mut preview_scratch)
        .expect("preview color composite");
    preview_service.record_cpu_execution_evidence(&preview);
    assert_eq!(
        preview.color_diagnostics.output.domain,
        ColorFrameDomain::Display
    );
    assert_eq!(preview.composite_diagnostics.float_linear_composites, 1);
    assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 0);
    assert_eq!(preview.composite_diagnostics.legacy_media_transform, 0);

    let export_working_frame = frame.working_frame().expect("export working frame");
    let export_elements = vec![TimelineCompositeElement::Media(TimelineMediaLayer {
        frame: &export_working_frame.frame,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        frame_seed: 0,
    })];
    let mut export_scratch = TimelineCompositeScratch::default();
    let expected_frame = mondrian_renderer::composite_timeline_elements_color_frame(
        1,
        1,
        &export_elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(
            color_context.engine(),
            color_context.working_color_space(),
        ),
        &mut export_scratch,
    )
    .expect("composite expected export frame");
    assert_eq!(
        expected_frame.descriptor().color_space,
        color_context.working_color_space().into()
    );
    let export_boundary = ProgramOutputBoundary::from_intent(
        mondrian_renderer::color::ProgramOutputRole::Export,
        color_context.output_color_space().color().expect("encoded export output"),
        color_context.output_transform(),
        color_context.output_tone_map(),
        color_context.engine().clone(),
    )
    .expect("resolved export intent");
    let export = ProgramOutputModule::execute_cpu_rgba8(
        &expected_frame,
        &export_boundary,
        export_scratch.color_execution_mut(),
    )
    .expect("export color transform");
    assert_eq!(
        export.color_diagnostics.output.domain,
        ColorFrameDomain::Export
    );
    assert_eq!(
        preview.color_stage_diagnostics.cpu_output_stages,
        export.stage_diagnostics.cpu_output_stages
    );
    let expected = export.rgba;

    assert_eq!(preview.rgba, expected);
}

#[test]
fn preview_camera_log_input_matches_export_frame_hash() {
    const SLOG3_TO_STANDARD_V3_SDR_V2_GOLDEN_HASH: u64 = 2_504_953_508_210_442_961;

    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let source = CpuEncodedColorFrame::source_rgba8(
        2,
        2,
        ColorSpace::SonySLog3SGamut3Cine,
        vec![
            96, 128, 160, 255, 192, 112, 64, 255, 24, 208, 144, 255, 224, 224, 224, 128,
        ],
    );
    let input_transform = RenderInputTransform::to_working(
        WorkingColorSpace::LinearRec2020,
        false,
        ColorEngine::mondrian_standard(),
    );
    let media = MediaPreviewFrame::from_source(
        MediaPreviewGpuSourceFrame::new(source.clone(), input_transform.clone()),
        Resolution { width: 2, height: 2 },
        test_preview_semantic_identity(3_003),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::from_path(PreviewDecodeExecutionPath::SoftwareCpu),
    );
    let color_context =
        test_color_context_in_working(ColorSpace::Srgb, WorkingColorSpace::LinearRec2020);
    let resolved = [ResolvedPreviewElement::Media {
        frame: media,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: Arc::clone(&effect_graph),
        prepared_heterogeneous_route: None,
        frame_seed: 3_003,
    }];
    let preview_service = WindowPreviewAdapter::new();
    let mut preview_scratch = TimelineCompositeScratch::default();
    let preview = composite_resolved_preview(2, 2, &resolved, &color_context, &mut preview_scratch)
        .expect("preview camera-log composite");
    preview_service.record_cpu_execution_evidence(&preview);

    let mut export_color_session = RenderCpuColorExecutionSession::default();
    let export_input = SourceColorModule::execute_cpu_with_intent(
        &CpuSourceColorFrame::from(source.clone()),
        &input_transform,
        &mut export_color_session,
    )
    .expect("export camera-log input transform");
    let export_elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
        frame: export_input.frame(),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph,
        frame_seed: 3_003,
    })];
    let mut export_scratch = TimelineCompositeScratch::default();
    let export_working = mondrian_renderer::composite_timeline_elements_color_frame(
        2,
        2,
        &export_elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(
            color_context.engine(),
            color_context.working_color_space(),
        ),
        &mut export_scratch,
    )
    .expect("composite camera-log export frame");
    let export_boundary = ProgramOutputBoundary::from_intent(
        mondrian_renderer::color::ProgramOutputRole::Export,
        ColorSpace::Srgb,
        color_context.output_transform(),
        color_context.output_tone_map(),
        color_context.engine().clone(),
    )
    .expect("resolved export Standard SDR intent");
    let export = ProgramOutputModule::execute_cpu_rgba8(
        &export_working,
        &export_boundary,
        export_scratch.color_execution_mut(),
    )
    .expect("export camera-log output transform")
    .rgba;

    assert_eq!(preview.rgba, export);
    assert_eq!(
        preview_service.diagnostics().color_stage_cpu_input_stages,
        1
    );
    assert_eq!(preview.color_stage_diagnostics.cpu_output_stages, 1);
    assert_eq!(preview.composite_diagnostics.float_linear_composites, 1);
    assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 0);
    let hash = stable_rgba_hash(&preview.rgba);
    assert_eq!(
        hash, SLOG3_TO_STANDARD_V3_SDR_V2_GOLDEN_HASH,
        "actual hash={hash}"
    );
}

#[test]
fn preview_multilayer_color_output_matches_export_frame_hash() {
    const REC2020_TO_STANDARD_V3_SDR_V2_SRGB_MULTILAYER_GOLDEN_HASH: u64 =
        16_678_535_327_707_552_965;

    let effect_graph =
        compile_reference_effect_graph(&EffectRenderPlan::default()).expect("default effect graph");
    let source = CpuEncodedColorFrame::source_rgba8(
        2,
        2,
        ColorSpace::Srgb,
        vec![
            200, 24, 16, 255, 40, 220, 96, 255, 12, 64, 240, 255, 240, 220, 40, 255,
        ],
    );
    let mut source_color_session = RenderCpuColorExecutionSession::default();
    let frame = SourceColorModule::execute_cpu_with_intent(
        &CpuSourceColorFrame::from(source),
        &RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec2020,
            false,
            ColorEngine::mondrian_standard(),
        ),
        &mut source_color_session,
    )
    .expect("media input transform")
    .into_frame();
    let logical_resolution = Resolution {
        width: frame.descriptor().width,
        height: frame.descriptor().height,
    };
    let media = MediaPreviewFrame::from_working(
        frame,
        logical_resolution,
        test_preview_semantic_identity(2_020),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::from_path(PreviewDecodeExecutionPath::SoftwareCpu),
    );
    let solid = TimelineSolidColorLayer {
        color: Color::from_rgba8(32, 180, 220, 255),
        opacity: 0.35,
        blend_mode: BlendMode::Screen,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: Arc::clone(&effect_graph),
        frame_seed: 14,
    };
    let color_context =
        test_color_context_in_working(ColorSpace::Srgb, WorkingColorSpace::LinearRec2020);

    let resolved = vec![
        ResolvedPreviewElement::Media {
            frame: media.clone(),
            opacity: 0.85,
            blend_mode: BlendMode::Multiply,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            prepared_heterogeneous_route: None,
            frame_seed: 7,
        },
        ResolvedPreviewElement::SolidColor(solid.clone()),
    ];
    let mut preview_scratch = TimelineCompositeScratch::default();
    let preview_service = WindowPreviewAdapter::new();
    let preview = composite_resolved_preview(2, 2, &resolved, &color_context, &mut preview_scratch)
        .expect("preview multilayer composite");
    preview_service.record_cpu_execution_evidence(&preview);

    let export_media_working = media.working_frame().expect("export media working frame");
    let export_elements = vec![
        TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &export_media_working.frame,
            opacity: 0.85,
            blend_mode: BlendMode::Multiply,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 7,
        }),
        TimelineCompositeElement::SolidColor(solid),
    ];
    let mut export_scratch = TimelineCompositeScratch::default();
    let export_working =
        mondrian_renderer::composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &export_elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(
                color_context.engine(),
                color_context.working_color_space(),
            ),
            &mut export_scratch,
        )
        .expect("composite multilayer export frame");
    let export_boundary = ProgramOutputBoundary::from_intent(
        mondrian_renderer::color::ProgramOutputRole::Export,
        color_context.output_color_space().color().expect("encoded export output"),
        color_context.output_transform(),
        color_context.output_tone_map(),
        color_context.engine().clone(),
    )
    .expect("resolved export intent");
    let export_output = ProgramOutputModule::execute_cpu_rgba8(
        &export_working.frame,
        &export_boundary,
        export_scratch.color_execution_mut(),
    )
    .expect("export multilayer color transform");
    let export = export_output.rgba;

    assert_eq!(preview.rgba, export);
    let preview_export_hash = stable_rgba_hash(&preview.rgba);
    assert_eq!(preview_export_hash, stable_rgba_hash(&export));
    assert_eq!(
        preview_export_hash,
        REC2020_TO_STANDARD_V3_SDR_V2_SRGB_MULTILAYER_GOLDEN_HASH
    );
    assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 0);
    assert_eq!(preview.composite_diagnostics.legacy_media_blend_mode, 0);
    assert_eq!(preview.composite_diagnostics.legacy_media_transform, 0);
    assert_eq!(preview.composite_diagnostics.legacy_solid_blend_mode, 0);
    assert_eq!(preview.composite_diagnostics.legacy_solid_transform, 0);

    let preview_composite_summary = preview.composite_diagnostics.color_path_summary();
    let preview_diagnostics = PreviewDiagnostics {
        color_stage_total_stages: preview.color_stage_diagnostics.total_stages,
        color_stage_cpu_input_stages: preview.color_stage_diagnostics.cpu_input_stages,
        color_stage_cpu_output_stages: preview.color_stage_diagnostics.cpu_output_stages,
        color_stage_gpu_color_stages: preview.color_stage_diagnostics.gpu_color_stages,
        color_stage_upload_stages: preview.color_stage_diagnostics.upload_stages,
        color_stage_readback_stages: preview.color_stage_diagnostics.readback_stages,
        color_stage_gpu_blockers: preview.color_stage_diagnostics.gpu_blockers,
        color_stage_gpu_shader_module_blockers: preview
            .color_stage_diagnostics
            .gpu_blocker_breakdown
            .shader_module_not_prepared,
        color_stage_gpu_ocio_resource_blockers: preview
            .color_stage_diagnostics
            .gpu_blocker_breakdown
            .ocio_resource_bind_group_not_prepared,
        color_stage_gpu_wrapper_blockers: preview
            .color_stage_diagnostics
            .gpu_blocker_breakdown
            .fullscreen_wrapper_not_prepared,
        color_stage_gpu_render_pipeline_blockers: preview
            .color_stage_diagnostics
            .gpu_blocker_breakdown
            .render_pipeline_not_prepared,
        color_stage_pixels: preview.color_stage_diagnostics.stage_pixels,
        color_rgba8_boundary_calls: u64::from(preview.color_diagnostics.used_rgba8_boundary),
        color_composite_plans: preview_composite_summary.composite_plans(),
        color_composite_elements: preview.composite_diagnostics.elements,
        color_composite_float_linear: preview.composite_diagnostics.float_linear_composites,
        color_composite_legacy_rgba8: preview.composite_diagnostics.legacy_rgba8_composites,
        color_composite_legacy_media_blend_mode: preview
            .composite_diagnostics
            .legacy_media_blend_mode,
        color_composite_legacy_media_transform: preview
            .composite_diagnostics
            .legacy_media_transform,
        color_composite_legacy_media_effect: preview.composite_diagnostics.legacy_media_effect,
        color_composite_legacy_solid_blend_mode: preview
            .composite_diagnostics
            .legacy_solid_blend_mode,
        color_composite_legacy_solid_transform: preview
            .composite_diagnostics
            .legacy_solid_transform,
        color_composite_legacy_solid_effect: preview.composite_diagnostics.legacy_solid_effect,
        color_composite_legacy_adjustment_blend_mode: preview
            .composite_diagnostics
            .legacy_adjustment_blend_mode,
        color_composite_legacy_adjustment_effect: preview
            .composite_diagnostics
            .legacy_adjustment_effect,
        ..PreviewDiagnostics::default()
    };
    let preview_health = preview_diagnostics.color_health_summary().expect("preview color health");
    assert_eq!(
        preview_health.rgba8_boundary_calls, 0,
        "float OCIO Program Output must quantize only after optional monitor adaptation"
    );

    let mut export_diagnostics = mondrian_export::queue::ExportJobColorDiagnostics::default();
    export_diagnostics.record_frame_diagnostics(
        mondrian_timeline::sequence::InputColorResolutionSourceCounts::default(),
        export_output.stage_diagnostics,
        export_working.diagnostics,
    );
    let export_health = export_diagnostics.summary().expect("export color health");
    assert_preview_export_color_health_match(preview_health, export_health);
    let preview_report =
        build_preview_color_health_report(Some(preview_health), "preview-export-golden");
    let export_report = export_diagnostics
        .health_report("preview-export-golden")
        .expect("export color report");
    assert_preview_export_color_reports_match(&preview_report, &export_report);
}

#[test]
fn stale_viewer_frame_is_scoped_to_sequence_and_dimensions() {
    let service = WindowPreviewAdapter::new();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let sequence = state.active_sequence().expect("sequence");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);

    let ready = ready_frame(service.viewer_preview_for_state(&state));
    let same_scope = service
        .stale_frame_for_sequence(sequence, width, height)
        .expect("same sequence can reuse stale frame");
    assert_eq!(same_scope.resource_key, ready.key);
    assert_eq!(same_scope.color_space, PreviewRasterColorSpace::Srgb);
    assert!(Arc::ptr_eq(&same_scope.rgba, &ready.rgba));

    let different_sequence = Sequence::new("other");
    assert!(service.stale_frame_for_sequence(&different_sequence, width, height).is_none());
    assert!(service
        .stale_frame_for_sequence(sequence, width.saturating_add(1), height)
        .is_none());
}

#[test]
fn playback_prefetch_proceeds_while_only_viewer_execution_is_pending() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, _asset_id, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");

    service.execution.borrow_mut().set_pending(true);
    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
    assert!(diagnostics.enqueued_jobs > 0);
    assert!(diagnostics.worker_queue.queued_prefetch_jobs > 0);

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn stalled_playback_current_expiration_releases_pending_and_queued_work() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let demand_identity = WindowPreviewAdapter::test_frame_demand_identity();
    service.seed_pending_playback_current_preview_work_for_test(demand_identity);

    let before = service.diagnostics();
    assert_eq!(before.scheduler.pending_requests, 1);
    assert_eq!(before.worker_queue.queued_jobs, 1);
    assert!(service.execution.borrow().is_pending());

    let outcome =
        service.expire_stalled_realtime_current_with_timeout(Duration::ZERO, Some(demand_identity));
    assert!(!outcome.visible_change);
    assert!(outcome.transport_change);
    assert_eq!(outcome.frame_delivery_candidates.len(), 1);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.playback_current_stalled_expirations, 1);
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
    assert_eq!(diagnostics.playback_schedule.current_late_streak, 1);
    assert!(!diagnostics.playback_schedule.sustained_pressure_active);
    assert_eq!(diagnostics.playback_schedule.sustained_pressure_events, 0);
    assert_eq!(
        diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
        1
    );
    assert_eq!(diagnostics.queue_canceled_jobs, 1);
    assert_eq!(diagnostics.scheduler.pending_requests, 0);
    assert_eq!(diagnostics.scheduler.canceled_requests, 1);
    assert_eq!(diagnostics.worker_queue.queued_jobs, 0);
    assert!(!service.execution.borrow().is_pending());
}

#[test]
fn stalled_playback_expiration_retains_in_flight_locality_without_queue_cancel_or_publish() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let demand_identity = WindowPreviewAdapter::test_frame_demand_identity();
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(2_501);
    let generation = service.scheduler.begin_generation();
    let deadline = Instant::now();
    assert!(matches!(
        service.scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
            Some(deadline),
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let execution_id = service
        .scheduler
        .begin_test_execution(MediaPreviewWorkerLane::Playback)
        .expect("playback execution must hold the sole in-flight lease");

    let expired =
        service.expire_stalled_realtime_current_with_timeout(Duration::ZERO, Some(demand_identity));
    assert_eq!(
        expired.frame_delivery_candidates,
        vec![mondrian_playback::FrameDeliveryCandidate::for_demand(
            demand_identity,
            mondrian_playback::FrameDeliveryKind::Late,
        )]
    );
    assert_eq!(service.diagnostics().queue_canceled_jobs, 0);
    assert_eq!(service.diagnostics().scheduler.pending_requests, 0);
    assert_eq!(service.diagnostics().worker_queue.in_flight_jobs, 1);
    assert_eq!(service.scheduler.execution_cancellation(execution_id), None);

    let mut result = test_successful_media_preview_result(&service, key.clone(), generation, 91);
    result.priority = MediaPreviewRequestPriority::Current;
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.deadline_at = Some(deadline);
    result.demand_identity = Some(demand_identity);
    result.execution_id = Some(execution_id);
    result_tx.send(result).expect("send locality completion");

    let completion = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(!completion.visible_change);
    assert!(
        completion.frame_delivery_candidates.is_empty(),
        "an expired binding must never reacquire terminal presentation authority"
    );
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.scheduler.completed_cache_only_results, 1);
    assert_eq!(diagnostics.scheduler.pending_requests, 0);
    assert_eq!(diagnostics.worker_queue.in_flight_jobs, 0);
    assert_eq!(diagnostics.queue_canceled_jobs, 0);
    assert!(
        service.frame_store.borrow_mut().media_frame(&key).is_none(),
        "a deadline-missed locality completion may warm decoder state but not the frame cache"
    );
    service.shutdown();
}

#[test]
fn expired_work_cannot_publish_late_after_its_demand_completed() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let completed_identity = WindowPreviewAdapter::test_frame_demand_identity();
    service.seed_pending_playback_current_preview_work_for_test(completed_identity);
    let mut engine = mondrian_playback::PlaybackEngine::new(
        Rational::new(1, 25),
        mondrian_playback::PlaybackPolicy::default(),
    )
    .expect("playback engine");
    engine
        .play_timeline(
            mondrian_playback::PlaybackTimelineBinding::new(None, 0, Rational::new(1, 25), 10)
                .expect("timeline binding"),
            mondrian_core::FramePosition::new(0, Rational::new(1, 25)),
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("priming demand");
    engine
        .complete_priming(
            mondrian_playback::ClockMaster::Synthetic,
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("replacement demand");
    let replacement_identity =
        engine.pending_frame_demand().expect("replacement pending demand").identity();
    assert_ne!(replacement_identity, completed_identity);

    let outcome = service
        .expire_stalled_realtime_current_with_timeout(Duration::ZERO, Some(replacement_identity));

    assert!(
        outcome.frame_delivery_candidates.is_empty(),
        "scheduler expiration must not revive an already-completed playback demand"
    );
    assert_eq!(service.diagnostics().scheduler.pending_requests, 0);
}

#[test]
fn stalled_scrub_releases_capacity_without_reporting_playback_delivery() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.seed_pending_preview_work_with_access_mode_for_test(
        PreviewDecodeAccessMode::ScrubCursor,
        None,
    );

    let outcome = service.expire_stalled_realtime_current_with_timeout(Duration::ZERO, None);

    assert!(!outcome.visible_change);
    assert!(!outcome.transport_change);
    assert!(outcome.frame_delivery_candidates.is_empty());
    let diagnostics = service.diagnostics();
    // Interactive scrub work has latest-wins cancellation through generation
    // rotation but no presentation deadline, so the playback stall window must
    // never expire it (a scrub may legitimately outlive one GOP open). Its
    // pending admission therefore survives this seam untouched.
    assert_eq!(diagnostics.scheduler.pending_requests, 1);
    assert_eq!(diagnostics.worker_queue.queued_jobs, 1);
    assert_eq!(diagnostics.playback_current_stalled_expirations, 0);
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 0);
}

#[test]
fn repeated_late_playback_current_frames_enter_pressure_recovery() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();

    let demand_identity = WindowPreviewAdapter::test_frame_demand_identity();
    service.seed_pending_playback_current_preview_work_for_test(demand_identity);
    assert!(
        service
            .expire_stalled_realtime_current_with_timeout(Duration::ZERO, Some(demand_identity),)
            .transport_change
    );
    service.seed_pending_playback_current_preview_work_for_test(demand_identity);
    assert!(
        service
            .expire_stalled_realtime_current_with_timeout(Duration::ZERO, Some(demand_identity),)
            .transport_change
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.playback_current_stalled_expirations, 2);
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 2);
    assert_eq!(diagnostics.playback_schedule.current_late_streak, 2);
    assert!(diagnostics.playback_schedule.sustained_pressure_active);
    assert_eq!(diagnostics.playback_schedule.sustained_pressure_events, 1);
    assert_eq!(
        diagnostics.playback_schedule.sustained_pressure_recoveries,
        0
    );
}

#[test]
fn playback_pressure_skips_new_current_decode_when_realtime_work_is_pending() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let key = test_media_key(200);
    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key,
            generation: service.execution.borrow().generation(),
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    service
        .record_playback_current_late_drop(MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD);

    assert_eq!(
        service.request_media_preview(
            test_media_key(201),
            crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
                test_media_work_demand(201),
            ),
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ),
        request_scheduler::MediaPreviewRequestAdmission::DeferredExecutionPressure
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.worker_queue.queued_jobs, 1);
    assert_eq!(
        diagnostics.playback_schedule.current_sustained_pressure_skips,
        1
    );
    assert_eq!(diagnostics.playback_schedule.current_decode_decisions, 0);
    assert_eq!(
        diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
        MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD + 1
    );
}

#[test]
fn playback_pressure_allows_recovery_decode_when_no_realtime_work_is_pending() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service
        .record_playback_current_late_drop(MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD);

    assert_eq!(
        service.request_media_preview(
            test_media_key(202),
            crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
                test_media_work_demand(202),
            ),
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ),
        request_scheduler::MediaPreviewRequestAdmission::Scheduled
    );

    let diagnostics = service.diagnostics();
    assert_eq!(
        diagnostics.playback_schedule.current_sustained_pressure_skips,
        0
    );
    assert_eq!(diagnostics.playback_schedule.current_decode_decisions, 1);
    assert_eq!(diagnostics.worker_queue.queued_playback_cursor_jobs, 1);
}

#[test]
fn realtime_current_preempts_queued_still_work_before_queue_is_full() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let still_key = test_media_key(203);
    let scrub_key = test_media_key(204);

    assert_eq!(
        service.request_media_preview(
            still_key.clone(),
            crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
                test_media_work_demand(203),
            ),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
        ),
        request_scheduler::MediaPreviewRequestAdmission::Scheduled
    );
    assert_eq!(
        service.request_media_preview(
            scrub_key.clone(),
            crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
                test_media_work_demand(204),
            ),
            PreviewDecodeAccessMode::ScrubCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ),
        request_scheduler::MediaPreviewRequestAdmission::Scheduled
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.scheduler.pending_requests, 1);
    assert_eq!(diagnostics.scheduler.evicted_still_requests, 1);
    assert_eq!(diagnostics.queue_canceled_jobs, 1);
    assert_eq!(diagnostics.worker_queue.queued_random_access_still_jobs, 0);
    assert_eq!(diagnostics.worker_queue.queued_scrub_cursor_jobs, 1);
    assert_eq!(diagnostics.worker_queue.queued_jobs, 1);
    assert!(!service.scheduler.has_pending_key(&still_key));
    assert!(service.scheduler.has_pending_key(&scrub_key));
}

#[test]
fn realtime_current_keeps_in_flight_still_pending_for_structured_preemption() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let still_key = test_media_key(205);
    let scrub_key = test_media_key(206);
    let generation = service.execution.borrow().generation();

    assert_eq!(
        service.scheduler.request(
            still_key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    let still_execution = service
        .scheduler
        .begin_test_execution(MediaPreviewWorkerLane::NonPlayback)
        .expect("still work should be in flight before realtime admission");
    assert_eq!(
        service.request_media_preview(
            scrub_key.clone(),
            crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
                test_media_work_demand(206),
            ),
            PreviewDecodeAccessMode::ScrubCursor,
            Some(Instant::now() + Duration::from_millis(33)),
            None,
            PreviewDecodeAdaptiveHints::default(),
        ),
        request_scheduler::MediaPreviewRequestAdmission::Scheduled
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.scheduler.pending_requests, 2);
    assert_eq!(diagnostics.scheduler.evicted_still_requests, 0);
    assert_eq!(diagnostics.queue_canceled_jobs, 0);
    assert_eq!(diagnostics.worker_queue.queued_scrub_cursor_jobs, 1);
    assert_eq!(diagnostics.worker_queue.queued_random_access_still_jobs, 0);
    assert!(service.scheduler.has_pending_key(&still_key));
    assert!(service.scheduler.has_pending_key(&scrub_key));
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            service.scheduler.execution_cancellation(still_execution),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Duration::ZERO,
            false,
        ),
        Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent)
    );
}

#[test]
fn playback_pressure_recovery_suppresses_forward_prefetch_until_current_success() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");

    service
        .record_playback_current_late_drop(MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD);
    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert!(diagnostics.playback_schedule.sustained_pressure_active);
    assert_eq!(
        diagnostics.playback_schedule.prefetch_skipped_sustained_pressure,
        1
    );
    assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
    assert_eq!(diagnostics.prefetch_skipped_current_work, 0);
    assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);

    service.record_playback_current_success(
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::PlaybackCursor,
    );
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.playback_schedule.current_late_streak, 0);
    assert!(!diagnostics.playback_schedule.sustained_pressure_active);
    assert_eq!(
        diagnostics.playback_schedule.sustained_pressure_recoveries,
        1
    );
}

#[test]
fn playback_prefetch_queues_behind_current_work() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, _asset_id, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let current_key = test_media_key(100);

    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key: current_key,
            generation: 1,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::ScrubCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
    assert_eq!(diagnostics.prefetch_skipped_current_work, 1);
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
    assert_eq!(diagnostics.worker_queue.queued_current_jobs, 1);
    assert!(diagnostics.worker_queue.queued_prefetch_jobs > 0);

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn playback_prefetch_respects_headroom_while_current_work_is_in_flight() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, _asset_id, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let generation = service.scheduler.begin_generation();
    let _current = begin_test_media_execution(
        &service,
        test_media_key(150),
        generation,
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::ScrubCursor,
        MediaPreviewWorkerLane::NonPlayback,
    );

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
    assert_eq!(diagnostics.prefetch_skipped_current_work, 1);
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
    assert_eq!(diagnostics.enqueued_jobs, 0);
    assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
    assert_eq!(diagnostics.worker_queue.in_flight_current_jobs, 1);

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}

#[test]
fn playback_prefetch_yields_when_prefetch_backlog_already_covers_window() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let prefetch_window =
        media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
            .expect("valid sequence frame rate");

    for offset in 0..prefetch_window as i64 {
        assert_eq!(
            service.jobs.enqueue(MediaPreviewJob {
                key: test_media_key(200 + offset),
                generation: 1,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
                residency_work: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
    }

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
    assert_eq!(diagnostics.prefetch_skipped_current_work, 0);
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 1);
    assert_eq!(
        diagnostics.worker_queue.queued_prefetch_jobs,
        prefetch_window
    );
}

#[test]
fn playback_prefetch_yields_when_in_flight_prefetch_covers_window() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let prefetch_window =
        media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
            .expect("valid sequence frame rate");
    let generation = service.scheduler.begin_generation();
    let _executions = (0..prefetch_window)
        .map(|offset| {
            begin_test_media_execution(
                &service,
                test_media_key(250 + offset as i64),
                generation,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                MediaPreviewWorkerLane::Playback,
            )
        })
        .collect::<Vec<_>>();

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_current_pending, 0);
    assert_eq!(diagnostics.prefetch_skipped_current_work, 0);
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 1);
    assert_eq!(diagnostics.enqueued_jobs, 0);
    assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 0);
    assert_eq!(
        diagnostics.worker_queue.in_flight_prefetch_jobs,
        prefetch_window
    );
}

#[test]
fn playback_prefetch_tops_up_only_remaining_window_slots() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let _prefetch_window =
        media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
            .expect("valid sequence frame rate");

    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key: test_media_key(300),
            generation: 1,
            priority: MediaPreviewRequestPriority::Prefetch,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
    // Residency is charged at the decode representation extent (the source
    // raster), not an output extent: a 4K source admits fewer physically
    // resident frames than an output-sized one, so the scheduled prefetch
    // window is bounded by residency, not by the frame-rate window alone.
    assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 3);
    assert_eq!(diagnostics.enqueued_jobs, 2);
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn playback_prefetch_tops_up_only_remaining_in_flight_window_slots() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let _prefetch_window =
        media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
            .expect("valid sequence frame rate");
    let generation = service.scheduler.begin_generation();
    service.execution.borrow_mut().invalidate(|| generation);
    let _prefetch = begin_test_media_execution(
        &service,
        test_media_key(350),
        generation,
        MediaPreviewRequestPriority::Prefetch,
        PreviewDecodeAccessMode::PlaybackCursor,
        MediaPreviewWorkerLane::Playback,
    );

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
    // Residency is charged at the decode representation extent (the source
    // raster), so one in-flight prefetch plus the queued window is bounded by
    // the source representation byte cost, not by the frame-rate window.
    assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 2);
    assert_eq!(diagnostics.worker_queue.in_flight_prefetch_jobs, 1);
    assert_eq!(diagnostics.enqueued_jobs, 2);
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn playback_prefetch_tops_up_by_actual_jobs_across_tracks() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, root) = state_with_two_invalid_video_assets();
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");
    let _prefetch_window =
        media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
            .expect("valid sequence frame rate");

    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key: test_media_key(400),
            generation: 1,
            priority: MediaPreviewRequestPriority::Prefetch,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.prefetch_skipped_prefetch_backlog, 0);
    assert_eq!(diagnostics.worker_queue.queued_prefetch_jobs, 3);
    assert_eq!(
        diagnostics.enqueued_jobs,
        2,
        "prefetch must fill only the remaining job slots even when a future frame has multiple active tracks"
    );
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn playback_prefetch_primes_the_next_media_activation_across_a_blank_gap() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (mut state, _, root) = state_with_invalid_video_asset();
    let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
    let steady_window = media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)
        .expect("valid sequence frame rate");
    let activation_frame = steady_window as i64 + 12;
    let time_base = sequence.time_base();
    sequence.video_tracks[0].clips[0].position = tt(activation_frame, time_base);
    state.seek(0).expect("seek");
    state.play().expect("play");
    let sequence = state.active_sequence().expect("sequence");

    schedule_media_prefetches_for_state(&service, &state, sequence, state.current_frame());

    let diagnostics = service.diagnostics();
    assert_eq!(
        diagnostics.worker_queue.queued_prefetch_jobs, 1,
        "one bounded cold-start request should cross the blank gap without enlarging the steady frame buffer"
    );
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn queued_expiry_completes_matching_demand_without_cancellation_latency_evidence() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(76);
    let generation = service.scheduler.begin_generation();
    let demand_identity = WindowPreviewAdapter::test_frame_demand_identity();
    assert_eq!(
        service.scheduler.request_with_demand_identity(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    let mut result = media_preview_canceled_result(
        MediaPreviewJob {
            key,
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: Some(Instant::now() - Duration::from_millis(1)),
            demand_identity: Some(demand_identity),
            execution_id: None,
            residency_work: None,
        },
        12_000,
        MediaPreviewCancelReason::PlaybackDeadline,
        0,
        Some(0),
        None,
    );
    result.cancellation_phase = Some(MediaPreviewCancellationPhase::Queued);
    result.queue_disposition = MediaPreviewQueueDisposition::Expired;
    result_tx.send(result).expect("send queued expiry result");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert_eq!(
        outcome.frame_delivery_candidates,
        vec![mondrian_playback::FrameDeliveryCandidate::for_demand(
            demand_identity,
            mondrian_playback::FrameDeliveryKind::Late,
        )]
    );
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.scheduler.pending_requests, 0);
    assert_eq!(diagnostics.decode_canceled_jobs, 0);
    assert_eq!(diagnostics.decode_cancellation.all.cancellations, 0);
    assert_eq!(diagnostics.decode_queue_wait_total_us, 0);
    assert_eq!(diagnostics.decode_queue_wait_max_us, 0);
    assert_eq!(diagnostics.decode_expired_queue_wait.samples, 1);
    assert_eq!(diagnostics.decode_expired_queue_wait.total_us, 12_000);
    assert_eq!(diagnostics.decode_expired_queue_wait.max_us, 12_000);
    assert_eq!(diagnostics.decode_expired_queue_wait.current_max_us, 12_000);
    assert_eq!(diagnostics.decode_expired_queue_wait.buckets.le_16ms, 1);
    assert_eq!(
        diagnostics.decode_access_mode_profiles.playback_cursor.expired_queue_wait,
        diagnostics.decode_expired_queue_wait
    );
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
    service.shutdown();
}

#[test]
fn preview_service_poll_releases_expired_playback_deadline_without_preview_refresh() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(77);
    let generation = service.scheduler.begin_generation();
    let demand_identity = WindowPreviewAdapter::test_frame_demand_identity();
    assert_eq!(
        service.scheduler.request_with_demand_identity(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(service.scheduler.diagnostics().pending_requests, 1);

    let result = media_preview_canceled_result(
        MediaPreviewJob {
            key: key.clone(),
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: Some(Instant::now() - Duration::from_millis(1)),
            demand_identity: Some(demand_identity),
            execution_id: None,
            residency_work: None,
        },
        12_000,
        MediaPreviewCancelReason::PlaybackDeadline,
        0,
        Some(0),
        None,
    );
    result_tx.send(result).expect("send canceled result");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );
    assert!(
        !outcome.visible_change,
        "deadline cancellation is scheduler/diagnostic evidence, not a new visible frame"
    );
    assert!(
        outcome.frame_delivery_candidates.is_empty(),
        "canceled decode work is not a terminal frame presentation"
    );
    assert!(!outcome.needs_follow_up_poll);
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.scheduler.pending_requests, 0);
    assert_eq!(diagnostics.decode_canceled_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_playback_deadline_jobs, 1);
    assert_eq!(diagnostics.decode_canceled_playback_cursor_jobs, 1);
    assert_eq!(diagnostics.decode_queue_wait_max_us, 12_000);
    assert_eq!(diagnostics.decode_expired_queue_wait.samples, 0);
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
    assert_eq!(
        diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
        1
    );
    service.shutdown();
}

#[test]
fn preview_service_poll_drops_successful_playback_completion_after_deadline() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let demand_identity = state
        .pending_playback_frame_demand_identity()
        .expect("playback demand identity");
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(78);
    let generation = service.scheduler.begin_generation();
    let deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        service.scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
            Some(deadline),
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    let mut result = test_successful_media_preview_result(&service, key.clone(), generation, 9);
    result.priority = MediaPreviewRequestPriority::Current;
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.deadline_at = Some(deadline);
    result.demand_identity = Some(demand_identity);
    result_tx.send(result).expect("send late successful result");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert!(
        !outcome.visible_change,
        "a successfully decoded but late playback frame must not refresh the viewer"
    );
    assert!(!outcome.needs_follow_up_poll);
    assert_eq!(
        outcome.frame_delivery_candidates,
        vec![mondrian_playback::FrameDeliveryCandidate::for_demand(
            demand_identity,
            mondrian_playback::FrameDeliveryKind::Late,
        )]
    );
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.scheduler.pending_requests, 0);
    assert_eq!(diagnostics.decode_successes, 1);
    assert_eq!(diagnostics.decode_canceled_jobs, 0);
    assert_eq!(diagnostics.playback_schedule.current_drop_late_decisions, 1);
    assert_eq!(
        diagnostics.playback_schedule.current_proxy_or_hardware_recommended_decisions,
        1
    );
    assert!(
        service.frame_store.borrow_mut().media_frame(&key).is_none(),
        "late playback completion should not enter the media preview cache"
    );
    service.shutdown();
}

#[test]
fn preview_service_stale_late_completion_cannot_publish_terminal_delivery() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let demand_identity = state
        .pending_playback_frame_demand_identity()
        .expect("playback demand identity");
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(782);
    let generation = service.scheduler.begin_generation();
    assert!(matches!(
        service.scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
            Some(Instant::now() - Duration::from_millis(1)),
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let _newer_generation = service.scheduler.begin_generation();
    let mut result = test_successful_media_preview_result(&service, key, generation, 9);
    result.priority = MediaPreviewRequestPriority::Current;
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.deadline_at = Some(Instant::now() - Duration::from_millis(1));
    result.demand_identity = Some(demand_identity);
    result_tx.send(result).expect("send stale late result");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert!(outcome.frame_delivery_candidates.is_empty());
    assert_eq!(service.diagnostics().scheduler.completed_stale_results, 1);
    service.shutdown();
}

#[test]
fn preview_service_current_late_completion_cannot_revive_replaced_playback_demand() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let completed_identity = state
        .pending_playback_frame_demand_identity()
        .expect("completed playback demand identity");
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(783);
    let generation = service.scheduler.begin_generation();
    assert!(matches!(
        service.scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(completed_identity),
            Some(Instant::now() - Duration::from_millis(1)),
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    state.seek(1).expect("seek");
    let replacement_identity = state
        .pending_playback_frame_demand_identity()
        .expect("replacement playback demand identity");
    assert_ne!(replacement_identity, completed_identity);
    let mut result = test_successful_media_preview_result(&service, key, generation, 9);
    result.priority = MediaPreviewRequestPriority::Current;
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.deadline_at = Some(Instant::now() - Duration::from_millis(1));
    result.demand_identity = Some(completed_identity);
    result_tx.send(result).expect("send replaced late result");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(replacement_identity),
    );

    assert!(outcome.frame_delivery_candidates.is_empty());
    assert_eq!(
        service.diagnostics().playback_schedule.current_drop_late_decisions,
        0,
        "completed-demand cleanup must not create pressure on its replacement"
    );
    service.shutdown();
}

#[test]
fn preview_service_deadline_uses_worker_completion_not_later_poll_time() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let demand_identity = state
        .pending_playback_frame_demand_identity()
        .expect("playback demand identity");
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(781);
    let generation = service.scheduler.begin_generation();
    let deadline = Instant::now() + Duration::from_millis(20);
    assert_eq!(
        service.scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
            Some(deadline),
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    let mut result = test_successful_media_preview_result(&service, key.clone(), generation, 9);
    result.priority = MediaPreviewRequestPriority::Current;
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.demand_identity = Some(demand_identity);
    result.deadline_at = Some(deadline);
    let execution_id = service
        .scheduler
        .begin_test_execution(MediaPreviewWorkerLane::Playback)
        .expect("worker execution lease");
    assert!(service.scheduler.mark_execution_completed(execution_id));
    result.execution_id = Some(execution_id);
    result_tx.send(result).expect("send on-time worker result");
    while Instant::now() < deadline {
        thread::yield_now();
    }

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert!(outcome.visible_change);
    assert!(outcome.frame_delivery_candidates.is_empty());
    assert!(service.frame_store.borrow_mut().media_frame(&key).is_some());
    assert_eq!(
        service.diagnostics().playback_schedule.current_drop_late_decisions,
        0
    );
    service.shutdown();
}

#[test]
fn preview_service_keeps_exact_hardware_fallback_ready_until_presentation() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    state.play().expect("play");
    let demand_identity = state
        .pending_playback_frame_demand_identity()
        .expect("playback demand identity");
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(79);
    let generation = service.scheduler.begin_generation();
    assert_eq!(
        service.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    let mut result = test_successful_media_preview_result(&service, key.clone(), generation, 9);
    result.priority = MediaPreviewRequestPriority::Current;
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.demand_identity = Some(demand_identity);
    let decode_diagnostics = test_preview_decode_diagnostics(
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable,
        PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
    );
    result.frame.as_mut().expect("decoded frame").set_presentation_quality(
        preview_decode_presentation_quality(&decode_diagnostics)
            .expect("hardware fallback remains temporally exact"),
    );
    result.decode_diagnostics = Some(decode_diagnostics);
    result_tx.send(result).expect("send exact fallback result");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert!(
        outcome.visible_change,
        "correct CPU fallback remains presentable"
    );
    assert!(
        outcome.frame_delivery_candidates.is_empty(),
        "decode readiness must not terminate the demand before presentation"
    );
    let cached_quality = service
        .frame_store
        .borrow_mut()
        .media_frame(&key)
        .expect("cached decoded frame")
        .presentation_quality();
    assert_eq!(
        cached_quality,
        mondrian_playback::FramePresentationQuality::Ready,
        "decode backend fallback must not become temporal degradation"
    );
    service.execution.borrow_mut().set_presentation_quality(cached_quality);
    let ticket =
        playback_presentation_ticket_for_state(&service, &state).expect("presentation ticket");
    let completion = state
        .complete_frame_presentation(ticket, Instant::now())
        .expect("current exact fallback presentation remains authoritative");
    assert_eq!(completion.delivery().identity(), demand_identity);
    assert_eq!(
        completion.delivery().kind(),
        mondrian_playback::FrameDeliveryKind::Ready
    );
    assert!(
        !completion.transport_changed(),
        "presentable fallback must still wait for bounded media lookahead"
    );
    assert!(state.observe_video_preroll(1, 1));
    service.shutdown();
}

#[test]
fn preview_service_poll_separates_canceled_backlog_from_visible_change() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let generation = service.scheduler.begin_generation();

    for index in 0..2 {
        let key = test_media_key(80 + index);
        assert_eq!(
            service.scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        result_tx
            .send(media_preview_canceled_result(
                MediaPreviewJob {
                    key,
                    generation,
                    priority: MediaPreviewRequestPriority::Current,
                    access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                    adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                    hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                    hardware_decode_device_selector: None,
                    enqueued_at: Instant::now(),
                    deadline_at: Some(Instant::now() - Duration::from_millis(1)),
                    demand_identity: None,
                    execution_id: None,
                    residency_work: None,
                },
                1_000,
                MediaPreviewCancelReason::PlaybackDeadline,
                0,
                Some(0),
                None,
            ))
            .expect("send canceled result");
    }

    let outcome = service.poll_finished_outcome_with_budget(1, Duration::from_millis(5), None);

    assert!(!outcome.visible_change);
    assert!(
        outcome.needs_follow_up_poll,
        "count-budget exhaustion should keep draining without forcing preview refresh"
    );
    assert!(
        !outcome.candidate_retry_required,
        "background completion cannot request a candidate retry without pending execution intent"
    );
    assert_eq!(service.scheduler.diagnostics().pending_requests, 1);
    assert_eq!(service.diagnostics().decode_canceled_jobs, 1);
    service.shutdown();
}

#[test]
fn preempted_prefetch_release_without_a_capacity_waiter_does_not_retry_current_candidate() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let generation = service.scheduler.begin_generation();
    let key = test_media_key(90);
    assert!(matches!(
        service.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let execution_id = service
        .scheduler
        .begin_test_execution(MediaPreviewWorkerLane::Playback)
        .expect("Prefetch execution lease");
    assert!(service.scheduler.request_one_in_flight_prefetch_preemption());

    let frame = test_media_frame(90);
    let residency_work = match service.frame_store.borrow_mut().reserve_media_work(
        &key,
        mondrian_playback::MediaWorkReservationIntent::Prefetch,
        frame.reserved_cpu_bytes(),
        frame.decoder_resource_units(),
    ) {
        Ok(MediaWorkReservationAdmission::Reserved(work)) => work,
        admission => panic!("Prefetch test requires a physical work lease: {admission:?}"),
    };
    service.execution.borrow_mut().set_pending(true);
    result_tx
        .send(media_preview_canceled_result(
            MediaPreviewJob {
                key: key.clone(),
                generation,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: Some(execution_id),
                residency_work: Some(residency_work),
            },
            1_000,
            MediaPreviewCancelReason::PrefetchPreemptedByCurrent,
            1_000,
            Some(1_000),
            Some(10),
        ))
        .expect("send preempted Prefetch result");

    let outcome = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);

    assert!(!outcome.visible_change);
    assert!(
        !outcome.candidate_retry_required,
        "background Prefetch retirement cannot retry an unrelated pending Viewer intent"
    );
    assert_eq!(service.diagnostics().frame_store.media_work_reservations, 0);
    assert_eq!(service.jobs.diagnostics().in_flight_jobs, 0);
    assert!(service.frame_store.borrow_mut().media_frame(&key).is_none());
    service.shutdown();
}

#[test]
fn aggregate_capacity_waiter_consumes_exactly_one_completion_edge() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let generation = service.scheduler.begin_generation();
    let key = test_media_key(91);
    assert!(matches!(
        service.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    service.media_aggregate_capacity_waiting.set(true);
    result_tx
        .send(media_preview_canceled_result(
            MediaPreviewJob {
                key,
                generation,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
                residency_work: None,
            },
            1_000,
            MediaPreviewCancelReason::PrefetchPreemptedByCurrent,
            0,
            Some(0),
            None,
        ))
        .expect("send capacity-releasing result");

    let first = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(first.candidate_retry_required);
    assert!(!service.media_aggregate_capacity_waiting.get());

    let second = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(
        !second.candidate_retry_required,
        "one capacity-release edge must not become a retry loop"
    );
    service.shutdown();
}

#[test]
fn aggregate_capacity_waiter_retries_once_when_its_observable_owner_settles_without_a_result() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let generation = service.scheduler.begin_generation();
    let key = test_media_key(92);
    assert!(matches!(
        service.scheduler.request(
            key,
            generation,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    service.media_aggregate_capacity_waiting.set(true);

    let pending = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(!pending.candidate_retry_required);
    assert!(service.media_aggregate_capacity_waiting.get());

    let (_, canceled) = service.scheduler.cancel_all();
    assert_eq!(canceled, 1);
    let settled = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(settled.candidate_retry_required);
    assert!(!service.media_aggregate_capacity_waiting.get());

    let second = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(!second.candidate_retry_required);
    service.shutdown();
}

#[test]
fn deterministic_temporal_mismatch_is_remembered_for_only_the_current_generation() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let demand_identity = PreviewProductionRuntime::<()>::test_frame_demand_identity();
    let key = test_media_key(900);
    let generation = service.scheduler.begin_generation();
    service.execution.borrow_mut().invalidate(|| generation);
    assert!(matches!(
        service.scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(demand_identity),
            None,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));

    let mut result = test_successful_media_preview_result(&service, key.clone(), generation, 9);
    result.frame = None;
    result.residency_work = None;
    result.failure_reason = Some(MediaPreviewFailureReason::TemporalMismatch);
    result.error = Some("selected frame does not cover requested source time".to_owned());
    result.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    result.demand_identity = Some(demand_identity);
    result_tx.send(result).expect("send exact temporal mismatch");

    let outcome = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert!(
        outcome.visible_change,
        "the first deterministic failure must publish the current Unavailable state"
    );
    assert_eq!(
        outcome.frame_delivery_candidates,
        vec![mondrian_playback::FrameDeliveryCandidate::for_demand(
            demand_identity,
            mondrian_playback::FrameDeliveryKind::Failed,
        )]
    );
    assert_eq!(
        service.media_execution_failures.borrow().get(&key).copied(),
        Some((generation, MediaPreviewFailureReason::TemporalMismatch))
    );
    assert!(
        service.failed_media_key(&key).is_some(),
        "the media Adapter must stop resubmitting a deterministic failure in this generation"
    );
    assert_eq!(service.scheduler.diagnostics().pending_requests, 0);
    assert!(
        !service
            .poll_finished_outcome_with_budget(8, Duration::from_millis(5), Some(demand_identity),)
            .visible_change,
        "retained failure evidence must not create an idle repaint loop"
    );

    let replacement_generation = service.scheduler.begin_generation();
    service.execution.borrow_mut().invalidate(|| replacement_generation);
    assert!(
        service.failed_media_key(&key).is_none(),
        "a new generation must remain eligible to decode the same semantic key"
    );
    service.shutdown();
}

#[test]
fn rebound_execution_failure_is_scoped_to_the_completion_binding_generation() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let key = test_media_key(901);
    let original_generation = service.scheduler.begin_generation();
    assert!(matches!(
        service.scheduler.request(
            key.clone(),
            original_generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let execution_id = service
        .scheduler
        .begin_test_execution(MediaPreviewWorkerLane::NonPlayback)
        .expect("original request must own an execution lease");

    let rebound_generation = service.scheduler.begin_generation();
    assert!(matches!(
        service.scheduler.request(
            key.clone(),
            rebound_generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));

    let mut result =
        test_successful_media_preview_result(&service, key.clone(), original_generation, 9);
    result.frame = None;
    result.failure_reason = Some(MediaPreviewFailureReason::WorkerPanicked);
    result.error = Some("contained test panic".to_owned());
    result.execution_id = Some(execution_id);
    result_tx.send(result).expect("send rebound failure result");

    let outcome = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);

    assert!(
        !outcome.visible_change,
        "an obsolete producer failure has no current presentation authority"
    );
    assert_eq!(service.scheduler.diagnostics().pending_requests, 1);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 1);
    assert_eq!(
        service.media_execution_failures.borrow().get(&key).copied(),
        None,
        "an obsolete producer must not poison its same-generation rebound"
    );

    let rebound_execution_id = service
        .scheduler
        .begin_test_execution(MediaPreviewWorkerLane::NonPlayback)
        .expect("rebound request must retain its own execution lease");
    let mut rebound_result =
        test_successful_media_preview_result(&service, key.clone(), rebound_generation, 10);
    rebound_result.frame = None;
    rebound_result.failure_reason = Some(MediaPreviewFailureReason::WorkerPanicked);
    rebound_result.error = Some("contained rebound test panic".to_owned());
    rebound_result.execution_id = Some(rebound_execution_id);
    result_tx
        .send(rebound_result)
        .expect("send authoritative rebound failure result");

    let rebound_outcome =
        service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);

    assert!(rebound_outcome.visible_change);
    assert_eq!(service.scheduler.diagnostics().pending_requests, 0);
    assert_eq!(
        service.media_execution_failures.borrow().get(&key).copied(),
        Some((
            rebound_generation,
            MediaPreviewFailureReason::WorkerPanicked
        )),
        "only the producer that owns the current binding may establish transient failure memory"
    );
    service.remember_media_execution_failure(
        &key,
        original_generation,
        MediaPreviewFailureReason::ResidencyContractViolation,
    );
    assert_eq!(
        service.media_execution_failures.borrow().get(&key).copied(),
        Some((
            rebound_generation,
            MediaPreviewFailureReason::WorkerPanicked
        )),
        "a later-arriving stale completion must not overwrite newer generation evidence"
    );
    service.shutdown();
}

#[test]
fn canceled_current_scrub_requests_follow_up_render_for_settled_frame() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let result_tx = install_preview_result_channel_for_test(&service);
    let generation = service.scheduler.begin_generation();
    let key = test_media_key(91);
    let asset_id = key.asset_id;
    let sequence = Sequence::new("canceled-producer-wait");
    let evaluation_key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: 0,
        frame: 5,
        width: 320,
        height: 180,
        runtime_scale: mondrian_playback::PreviewResolutionScale::Full,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    service.evaluation_working_set.borrow_mut().insert_waiting(
        evaluation_key,
        Arc::from([EvaluationDependency::MediaProducer(asset_id)]),
    );
    assert!(matches!(
        service.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    result_tx
        .send(media_preview_canceled_result(
            MediaPreviewJob {
                key,
                generation,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                hardware_decode_device_selector: None,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
                execution_id: None,
                residency_work: None,
            },
            1_000,
            MediaPreviewCancelReason::Obsolete,
            1_000,
            Some(1_000),
            None,
        ))
        .expect("send canceled scrub result");

    let outcome = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);

    assert!(
        outcome.visible_change,
        "settled non-playback work must get a render pass after cancellation"
    );
    assert!(
        service
            .evaluation_working_set
            .borrow()
            .waiting_for(evaluation_key)
            .is_none(),
        "a canceled producer must release retained evaluation waits so the settled frame can re-admit"
    );
    service.shutdown();
}

#[test]
fn failed_current_media_preview_cache_does_not_leave_viewer_loading() {
    let (state, asset_id, root) = state_with_invalid_video_asset();
    let service = WindowPreviewAdapter::new();
    let sequence = state.active_sequence().expect("sequence");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .expect("valid test context")
        .media_input(sequence.settings.color.input.auto_tone_map_media);
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let key = service
        .media_preview_key_for_asset(
            &snapshot,
            &state,
            &asset_id,
            None,
            AlphaInterpretation::Straight,
            mondrian_core::TimelineTime::ZERO,
            width,
            height,
            mondrian_playback::PreviewResolutionScale::Full,
            &input_color,
            true,
            false,
        )
        .expect("media preview key");
    service.frame_store.borrow_mut().remember_failure(key);

    let preview = service.viewer_preview_for_state(&state);

    assert!(matches!(preview, ViewerPreviewState::Unavailable(_)));
    assert!(
        !service.execution.borrow().is_pending(),
        "a cached decode failure is terminal evidence, not pending work"
    );
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn current_media_grant_rejection_is_blocked_without_phantom_pending_work() {
    let (state, asset_id, root) = state_with_invalid_video_asset();
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.frame_store.replace(PreviewFrameStoreAdapter::new(
        PreviewFrameStoreAdapterConfig {
            media_entry_capacity: 2,
            media_byte_budget: 256 * 1024 * 1024,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 1,
            current_media_working_set_byte_limit: 256 * 1024 * 1024,
            current_media_working_set_resource_unit_limit: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 256 * 1024 * 1024,
            failure_entry_capacity: 2,
        },
    ));
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let demand_id = mondrian_playback::MediaWorkDemandId::for_preview_generation(
        snapshot.transport().epoch(),
        service.execution.borrow().generation(),
        snapshot.transport().current_frame(),
    );
    let resident_key = test_media_key(930);
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        resident_key.clone(),
        test_media_frame(9),
        MediaPreviewRequestPriority::Prefetch,
    ));
    let guarded = service
        .frame_store
        .borrow_mut()
        .protected_media_frame(&resident_key, demand_id)
        .expect("demand protection should fit")
        .expect("resident test frame");
    let sequence = state.active_sequence().expect("sequence");
    let target_resolution = Resolution {
        width: sequence.settings.resolution.width,
        height: sequence.settings.resolution.height,
    };
    let request = crate::app::preview_timeline_execution::PreviewTimelineMediaRequest {
        asset_id,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        picture_overrides: Default::default(),
        source_sample: mondrian_core::SourceSampleTarget::covering(
            mondrian_core::TimelineTime::ZERO,
        ),
        target_resolution,
        input_color: sequence
            .settings
            .root_program_color_context(state.project_color_environment())
            .expect("valid test context")
            .media_input(sequence.settings.color.input.auto_tone_map_media),
        cpu_working_required: false,
    };

    let outcome = service.media_frame_for_plan(&snapshot, &state, request);

    let crate::app::preview_timeline_execution::PreviewTimelineMediaFrame::Unavailable { reason } =
        outcome
    else {
        panic!("per-demand capacity must be a typed blocker");
    };
    assert_eq!(
        reason.disposition(),
        PreviewUnavailabilityDisposition::Blocked
    );
    assert_eq!(reason.stage(), PreviewOutputStage::MediaDecode);
    assert!(!service.execution.borrow().is_pending());
    assert_eq!(service.scheduler.diagnostics().pending_requests, 0);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 0);
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_current_working_set_rejections,
        1
    );
    drop(guarded);
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn repeated_exact_media_request_rebinds_before_acquiring_another_physical_lease() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.frame_store.replace(PreviewFrameStoreAdapter::new(
        PreviewFrameStoreAdapterConfig {
            media_entry_capacity: 2,
            media_byte_budget: 256 * 1024 * 1024,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 1,
            current_media_working_set_byte_limit: 256 * 1024 * 1024,
            current_media_working_set_resource_unit_limit: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 256 * 1024 * 1024,
            failure_entry_capacity: 2,
        },
    ));
    let key = test_media_key(935);
    let demand_id = test_media_work_demand(935);
    let request = || {
        service.request_media_preview(
            key.clone(),
            MediaPreviewRequestIntent::Current(demand_id),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
        )
    };

    assert_eq!(request(), MediaPreviewRequestAdmission::Scheduled);
    assert_eq!(service.scheduler.diagnostics().pending_requests, 1);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 1);
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_work_reservations,
        1
    );

    assert_eq!(request(), MediaPreviewRequestAdmission::ExistingWork);
    assert_eq!(service.scheduler.diagnostics().pending_requests, 1);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 1);
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_work_reservations,
        1,
        "repeated UI evaluation must retain the queued payload instead of charging the demand twice"
    );
    assert_eq!(service.media_existing_work_waiters.borrow().len(), 1);
    service.shutdown();
}

#[test]
fn settled_existing_media_owner_retries_its_current_candidate_exactly_once() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let key = test_media_key(936);
    let demand_id = test_media_work_demand(936);
    let request = || {
        service.request_media_preview(
            key.clone(),
            MediaPreviewRequestIntent::Current(demand_id),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
        )
    };

    assert_eq!(request(), MediaPreviewRequestAdmission::Scheduled);
    assert_eq!(request(), MediaPreviewRequestAdmission::ExistingWork);
    assert_eq!(service.media_existing_work_waiters.borrow().len(), 1);

    let pending = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(!pending.candidate_retry_required);
    assert_eq!(service.media_existing_work_waiters.borrow().len(), 1);

    let (_, canceled) = service.scheduler.cancel_all();
    assert_eq!(canceled, 1);
    let before_wake = service.work_watch.revision();
    service.publish_existing_work_retry_if_actionable();
    assert_ne!(service.work_watch.revision(), before_wake);
    let settled = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(settled.candidate_retry_required);
    assert!(service.media_existing_work_waiters.borrow().is_empty());

    let retained = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(retained.candidate_retry_required);
    service.media_existing_work_retry_pending.set(false);
    let acknowledged = service.poll_finished_outcome_with_budget(8, Duration::from_millis(5), None);
    assert!(!acknowledged.candidate_retry_required);
    service.shutdown();
}

#[test]
fn unobservable_aggregate_media_owner_is_blocked_instead_of_pending_forever() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.frame_store.replace(PreviewFrameStoreAdapter::new(
        PreviewFrameStoreAdapterConfig {
            media_entry_capacity: 1,
            media_byte_budget: 16 * 1024 * 1024,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 1,
            current_media_working_set_byte_limit: 16 * 1024 * 1024,
            current_media_working_set_resource_unit_limit: 4,
            viewer_entry_capacity: 2,
            viewer_byte_budget: 16 * 1024 * 1024,
            failure_entry_capacity: 2,
        },
    ));
    let retained_key = test_media_key(940);
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        retained_key.clone(),
        test_media_frame(4),
        MediaPreviewRequestPriority::Prefetch,
    ));
    let retained = service
        .frame_store
        .borrow_mut()
        .media_frame(&retained_key)
        .expect("external frame owner");
    service.frame_store.borrow_mut().clear_media_frames();
    let demand_id = test_media_work_demand(941);

    let admission = service.request_media_preview(
        test_media_key(941),
        crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(demand_id),
        PreviewDecodeAccessMode::ScrubCursor,
        None,
        None,
        PreviewDecodeAdaptiveHints::default(),
    );

    assert_eq!(
        admission,
        request_scheduler::MediaPreviewRequestAdmission::BlockedAggregateCapacity
    );
    assert!(!service.execution.borrow().is_pending());
    assert_eq!(service.scheduler.diagnostics().pending_requests, 0);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 0);
    drop(retained);
    service.shutdown();
}

#[test]
fn completed_media_work_is_an_observable_aggregate_capacity_retry_owner() {
    let completed = MediaPreviewJobQueueDiagnostics {
        in_flight_completed_jobs: 1,
        ..MediaPreviewJobQueueDiagnostics::default()
    };

    assert!(
        request_scheduler::aggregate_pressure_has_observable_retry_owner(
            false,
            MediaPreviewSchedulerDiagnostics::default(),
            completed,
        )
    );
    assert!(
        !request_scheduler::aggregate_pressure_has_observable_retry_owner(
            false,
            MediaPreviewSchedulerDiagnostics::default(),
            MediaPreviewJobQueueDiagnostics::default(),
        )
    );
}

#[test]
fn obsolete_media_request_never_becomes_unowned_pending_work() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let execution_generation = service.execution.borrow().generation();
    while service.scheduler.diagnostics().latest_generation <= execution_generation {
        service.scheduler.begin_generation();
    }

    let admission = service.request_media_preview(
        test_media_key(950),
        crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
            test_media_work_demand(950),
        ),
        PreviewDecodeAccessMode::ScrubCursor,
        None,
        None,
        PreviewDecodeAdaptiveHints::default(),
    );

    assert_eq!(
        admission,
        request_scheduler::MediaPreviewRequestAdmission::ObsoleteGeneration
    );
    assert!(!service.execution.borrow().is_pending());
    assert_eq!(service.scheduler.diagnostics().pending_requests, 0);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 0);
    service.shutdown();
}

#[test]
fn obsolete_media_generation_is_a_retryable_timeline_wait_not_a_failure() {
    assert_eq!(
        media_adapter::media_wait_for_admission(
            request_scheduler::MediaPreviewRequestAdmission::ObsoleteGeneration,
        ),
        Some(crate::app::preview_timeline_execution::PreviewTimelineMediaWait::RetryAdmission),
    );
}

#[test]
fn terminal_media_worker_health_refuses_new_pending_admission() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.media_worker_health_failed.set(true);

    let admission = service.request_media_preview(
        test_media_key(960),
        crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
            test_media_work_demand(960),
        ),
        PreviewDecodeAccessMode::ScrubCursor,
        None,
        None,
        PreviewDecodeAdaptiveHints::default(),
    );

    assert_eq!(
        admission,
        request_scheduler::MediaPreviewRequestAdmission::WorkerUnavailable
    );
    assert!(!service.execution.borrow().is_pending());
    assert_eq!(service.scheduler.diagnostics().pending_requests, 0);
    assert_eq!(service.jobs.diagnostics().queued_jobs, 0);
    service.shutdown();
}

#[test]
fn configured_media_result_disconnect_fails_pending_demand_once_and_closes_admission() {
    let service =
        WindowPreviewAdapter::with_direct_worker_count_for_test(preview_decode_cpu_budget(), 1);
    let (disconnected_sender, disconnected_receiver) =
        mpsc::sync_channel(MEDIA_PREVIEW_COMPLETED_RESULT_QUEUE_CAPACITY);
    service.results.replace(disconnected_receiver);
    drop(disconnected_sender);
    service.execution.borrow_mut().set_pending(true);
    let demand_identity = WindowPreviewAdapter::test_frame_demand_identity();

    let first = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );

    assert!(first.visible_change);
    assert_eq!(
        first.frame_delivery_candidates,
        vec![mondrian_playback::FrameDeliveryCandidate::for_demand(
            demand_identity,
            mondrian_playback::FrameDeliveryKind::Failed,
        )]
    );
    assert!(service.media_worker_health_failed());
    assert!(!service.execution.borrow().is_pending());
    assert_eq!(
        service.request_media_preview(
            test_media_key(961),
            crate::app::preview_access_mode::MediaPreviewRequestIntent::Current(
                test_media_work_demand(961),
            ),
            PreviewDecodeAccessMode::ScrubCursor,
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
        ),
        request_scheduler::MediaPreviewRequestAdmission::WorkerUnavailable
    );

    let second = service.poll_finished_outcome_with_budget(
        8,
        Duration::from_millis(5),
        Some(demand_identity),
    );
    assert!(!second.visible_change);
    assert!(second.frame_delivery_candidates.is_empty());
    service.shutdown();
}

#[test]
fn media_preview_cache_identity_changes_with_range_override() {
    let (state, asset_id, root) = state_with_invalid_video_asset();
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let sequence = state.active_sequence().expect("sequence");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .expect("valid test context")
        .media_input(sequence.settings.color.input.auto_tone_map_media);
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let key_for_state = || {
        service
            .media_preview_key_for_asset(
                &snapshot,
                &state,
                &asset_id,
                None,
                AlphaInterpretation::Straight,
                mondrian_core::TimelineTime::ZERO,
                width,
                height,
                mondrian_playback::PreviewResolutionScale::Full,
                &input_color,
                false,
                false,
            )
            .expect("media preview key")
    };

    let auto_key = key_for_state();
    assert_eq!(
        auto_key.decode.source_color().range.baseline(),
        DecodedVideoRange::Limited
    );
    let library = state.asset_library().expect("asset library");
    let mut interpretation = library
        .get_asset(asset_id)
        .expect("read asset")
        .expect("asset exists")
        .interpretation;
    interpretation.range = mondrian_core::timeline_data::MediaRangeInterpretation::Override {
        range: mondrian_core::timeline_data::MediaSignalRange::Full,
    };
    library
        .set_asset_interpretation(asset_id, interpretation)
        .expect("persist range override");

    let override_key = key_for_state();
    assert_eq!(
        override_key.decode.source_color().range.baseline(),
        DecodedVideoRange::Full
    );
    assert_eq!(
        override_key.decode.source_color().range,
        DecodedVideoRangeContract::OverrideFull
    );
    assert_ne!(auto_key, override_key);

    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn playing_cached_media_preview_defers_sync_raster_composite() {
    let (mut state, asset_id, root) = state_with_invalid_video_asset();
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let sequence = state.active_sequence().expect("sequence");
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let (width, height) = preview_dimensions_for_snapshot(&snapshot, sequence);
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .expect("valid test context")
        .media_input(sequence.settings.color.input.auto_tone_map_media);
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let key = service
        .media_preview_key_for_asset(
            &snapshot,
            &state,
            &asset_id,
            None,
            AlphaInterpretation::Straight,
            mondrian_core::TimelineTime::ZERO,
            width,
            height,
            mondrian_playback::PreviewResolutionScale::Full,
            &input_color,
            true,
            false,
        )
        .expect("media preview key");
    admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        key,
        test_media_frame_with_size_in_working(
            80,
            width,
            height,
            123,
            input_color.working_color_space,
        ),
        MediaPreviewRequestPriority::Prefetch,
    );

    state.play().expect("play");
    let playing_preview = service.viewer_preview_for_state(&state);
    assert!(
        matches!(
            playing_preview,
            ViewerPreviewState::Loading | ViewerPreviewState::Stale(_)
        ),
        "playback must not synchronously CPU-composite cached media on the UI thread"
    );

    state.pause().expect("pause");
    let paused_preview = service.viewer_preview_for_state(&state);
    assert!(
        matches!(&paused_preview, ViewerPreviewState::Ready(_)),
        "paused still-frame preview may use the CPU correctness path, got {paused_preview:?}"
    );
    service.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn playing_generated_preview_defers_sync_raster_composite() {
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let service = WindowPreviewAdapter::new_without_workers_for_test();

    state.play().expect("play");
    let playing_preview = service.viewer_preview_for_state(&state);
    assert!(
        matches!(
            playing_preview,
            ViewerPreviewState::Loading | ViewerPreviewState::Stale(_)
        ),
        "playback UI projection must not synchronously CPU-composite generated picture"
    );
    assert!(
        matches!(
            execute_gpu_preview_for_test_app(&service, &state),
            PreviewGpuFrameState::Ready(_)
        ),
        "deferring the UI raster projection must preserve the production GPU candidate seam"
    );

    state.pause().expect("pause");
    let paused_preview = service.viewer_preview_for_state(&state);
    assert!(
        matches!(paused_preview, ViewerPreviewState::Ready(_)),
        "paused still-frame preview may use the CPU correctness path"
    );
    service.shutdown();
}

fn test_media_key(source_frame: i64) -> MediaPreviewKey {
    MediaPreviewKey::test_cpu(
        PathBuf::from(format!("E:/media/{source_frame}.mov")),
        test_media_file_fingerprint(
            source_frame.unsigned_abs().saturating_add(1),
            source_frame.unsigned_abs().saturating_add(1),
        ),
        mondrian_core::TimelineTime::new(source_frame, 1).expect("exact source time"),
        Resolution { width: 320, height: 180 },
        mondrian_media::PreviewSourceColorContract::automatic(
            ColorSpace::Rec709,
            DecodedVideoRange::Limited,
        ),
    )
}

fn test_media_key_with_source_time(
    mut key: MediaPreviewKey,
    source_time: mondrian_core::TimelineTime,
) -> MediaPreviewKey {
    key.decode = mondrian_media::PreviewDecodeKey::new(
        key.decode.source().clone(),
        mondrian_core::SourceSampleTarget::covering(source_time),
        key.decode.representation(),
        key.decode.source_color(),
    )
    .expect("valid replacement source time");
    key
}

fn test_media_key_with_physical_source(
    mut key: MediaPreviewKey,
    mut path: PathBuf,
    fingerprint: MediaFileFingerprint,
    video_stream_index: u32,
) -> MediaPreviewKey {
    if !path.is_absolute() {
        path = std::env::temp_dir().join(path);
    }
    let source = mondrian_media::PreviewDecodeSource::from_frozen_cpu_stream(
        path,
        fingerprint,
        video_stream_index,
        key.source_resolution,
    )
    .expect("complete replacement physical source");
    key.decode = mondrian_media::PreviewDecodeKey::new(
        source,
        key.source_sample(),
        key.decode.representation(),
        key.decode.source_color(),
    )
    .expect("valid replacement physical source");
    key
}

fn begin_test_media_execution(
    service: &WindowPreviewAdapter,
    key: MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    lane: MediaPreviewWorkerLane,
) -> mondrian_playback::FrameExecutionId {
    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key,
            generation,
            priority,
            access_mode,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    service.scheduler.begin_test_execution(lane).expect("test execution lease")
}

fn test_cpu_frame_store(
    media_entry_capacity: usize,
    media_byte_budget: usize,
    failure_entry_capacity: usize,
) -> PreviewFrameStoreAdapter {
    PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        media_entry_capacity,
        media_byte_budget,
        media_resource_unit_budget: 4,
        current_media_working_set_entry_limit: media_entry_capacity.max(8),
        current_media_working_set_byte_limit: media_byte_budget.saturating_mul(4).max(1_024),
        current_media_working_set_resource_unit_limit: 8,
        viewer_entry_capacity: 4,
        viewer_byte_budget: 1_024,
        failure_entry_capacity,
    })
}

fn admit_test_media_frame(
    store: &mut PreviewFrameStoreAdapter,
    key: MediaPreviewKey,
    frame: MediaPreviewFrame,
    priority: MediaPreviewRequestPriority,
) -> bool {
    let intent = match priority {
        MediaPreviewRequestPriority::Current => {
            mondrian_playback::MediaWorkReservationIntent::Current(test_media_work_demand(1))
        }
        MediaPreviewRequestPriority::Prefetch => {
            mondrian_playback::MediaWorkReservationIntent::Prefetch
        }
    };
    let reserved_bytes = frame.reserved_cpu_bytes();
    let resource_units = frame.decoder_resource_units();
    let work = match store.reserve_media_work(&key, intent, reserved_bytes, resource_units) {
        Ok(MediaWorkReservationAdmission::Reserved(work)) => work,
        Ok(
            MediaWorkReservationAdmission::AlreadyResident
            | MediaWorkReservationAdmission::RejectedCurrentDemandGrant
            | MediaWorkReservationAdmission::RejectedAggregateCapacity,
        )
        | Err(_) => return false,
    };
    store.insert_media_frame(key, frame, work).is_admitted()
}

fn test_media_work_demand(target_frame: i64) -> mondrian_playback::MediaWorkDemandId {
    mondrian_playback::MediaWorkDemandId::for_preview_generation(
        mondrian_playback::PlaybackEngine::default().snapshot().epoch,
        1,
        target_frame,
    )
}

fn install_preview_result_channel_for_test(
    service: &WindowPreviewAdapter,
) -> mpsc::SyncSender<MediaPreviewResult> {
    let (result_tx, result_rx) = mpsc::sync_channel(MEDIA_PREVIEW_COMPLETED_RESULT_QUEUE_CAPACITY);
    service.results.replace(result_rx);
    result_tx
}

fn test_successful_media_preview_result(
    service: &WindowPreviewAdapter,
    key: MediaPreviewKey,
    generation: u64,
    seed: u8,
) -> MediaPreviewResult {
    let frame = test_media_frame(seed);
    let demand_id = test_media_work_demand(0);
    let residency_work = match service.frame_store.borrow_mut().reserve_media_work(
        &key,
        mondrian_playback::MediaWorkReservationIntent::Current(demand_id),
        frame.reserved_cpu_bytes(),
        frame.decoder_resource_units(),
    ) {
        Ok(MediaWorkReservationAdmission::Reserved(work)) => work,
        admission => panic!("test result requires a physical work lease: {admission:?}"),
    };
    MediaPreviewResult {
        key,
        frame: Some(frame),
        error: None,
        failure_reason: None,
        generation,
        priority: MediaPreviewRequestPriority::Current,
        access_mode: PreviewDecodeAccessMode::ScrubCursor,
        queue_disposition: MediaPreviewQueueDisposition::Ready,
        queue_wait_us: 0,
        decode_elapsed_us: 0,
        deadline_at: None,
        logical_cancellation_observed: None,
        canceled: false,
        cancellation_phase: None,
        cancel_reason: None,
        concrete_media_checkpoint: None,
        decode_diagnostics: None,
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity: None,
        execution_id: None,
        residency_work: Some(residency_work),
    }
}

fn test_media_frame(seed: u8) -> MediaPreviewFrame {
    test_media_frame_rgba(vec![seed, 0, 0, 255], 1, 1, seed as u64)
}

fn test_media_frame_with_size(
    seed: u8,
    width: u32,
    height: u32,
    identity_revision: u64,
) -> MediaPreviewFrame {
    test_media_frame_with_size_in_working(
        seed,
        width,
        height,
        identity_revision,
        WorkingColorSpace::LinearRec709,
    )
}

fn test_media_frame_with_size_in_working(
    seed: u8,
    width: u32,
    height: u32,
    identity_revision: u64,
    working_color_space: WorkingColorSpace,
) -> MediaPreviewFrame {
    test_media_frame_rgba_in_working(
        std::iter::repeat_n([seed, 0, 0, 255], width as usize * height as usize)
            .flatten()
            .collect(),
        width,
        height,
        identity_revision,
        working_color_space,
    )
}

fn test_media_frame_rgba(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    identity_revision: u64,
) -> MediaPreviewFrame {
    test_media_frame_rgba_in_working(
        rgba,
        width,
        height,
        identity_revision,
        WorkingColorSpace::LinearRec709,
    )
}

fn test_media_frame_rgba_in_working(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    identity_revision: u64,
    working_color_space: WorkingColorSpace,
) -> MediaPreviewFrame {
    let source = CpuEncodedColorFrame::source_rgba8(width, height, ColorSpace::Rec709, rgba);
    let input_transform = RenderInputTransform::to_working(
        working_color_space,
        false,
        ColorEngine::mondrian_standard(),
    );
    MediaPreviewFrame::from_source(
        MediaPreviewGpuSourceFrame::new(source, input_transform),
        Resolution { width, height },
        test_preview_semantic_identity(identity_revision),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::from_path(PreviewDecodeExecutionPath::SoftwareCpu),
    )
}

#[derive(Debug)]
struct TestNativeDecodedFrameResource {
    kind: DecodedGpuFrameHandleKind,
    id: std::num::NonZeroU64,
}

impl mondrian_media::PreviewNativeDecodedFrameResource for TestNativeDecodedFrameResource {
    fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
        self.kind
    }

    fn handle_id(&self) -> std::num::NonZeroU64 {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn test_native_source_frame(width: u32, height: u32) -> MediaPreviewNativeSourceFrame {
    let handle = PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
        kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
        id: std::num::NonZeroU64::new(7).expect("non-zero native frame handle"),
    });
    let native_frame = PreviewNativeDecodedFrame::new(
        width,
        height,
        handle,
        DecodedVideoSurfaceFormat::P010,
        DecodedVideoSampling {
            matrix: mondrian_media::DecodedVideoMatrix::Bt709,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::Left,
            bit_depth: 10,
        },
        test_preview_decode_diagnostics(
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewHardwareDecodeDecision::GpuResidentNative,
            PreviewHardwareDecodeBlocker::None,
        ),
    )
    .expect("test native decoded frame");
    MediaPreviewNativeSourceFrame::from_native_frame(
        native_frame,
        ColorSpace::Rec709,
        RenderInputTransform::to_working_gpu(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        ),
    )
}

fn test_media_frame_rgba8(frame: &MediaPreviewFrame) -> Vec<u8> {
    let working = frame.working_frame().expect("test media working frame");
    let mut session = RenderCpuColorExecutionSession::default();
    ProgramOutputModule::execute_cpu_rgba8(
        &working.frame,
        &ProgramOutputBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        ),
        &mut session,
    )
    .expect("test media output transform")
    .rgba
}

fn stable_rgba_hash(rgba: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in rgba {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[test]
fn media_preview_gpu_source_caches_lazy_cpu_working_transform() {
    let source =
        CpuEncodedColorFrame::source_rgba8(1, 1, ColorSpace::Rec709, vec![64, 128, 192, 255]);
    let input_transform = RenderInputTransform::to_working(
        WorkingColorSpace::LinearRec709,
        false,
        ColorEngine::mondrian_standard(),
    );
    let frame = MediaPreviewFrame::from_source(
        MediaPreviewGpuSourceFrame::new(source, input_transform),
        Resolution { width: 1, height: 1 },
        test_preview_semantic_identity(42),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::from_path(PreviewDecodeExecutionPath::SoftwareCpu),
    );

    let first = frame.working_frame().expect("first lazy working transform");
    let second = frame.working_frame().expect("cached lazy working transform");

    assert!(first.color_diagnostics.is_some());
    assert!(first.stage_diagnostics.cpu_input_stages > 0);
    assert!(second.color_diagnostics.is_none());
    assert_eq!(
        second.stage_diagnostics,
        RenderColorStageDiagnostics::default()
    );
    assert_eq!(
        first.frame.rgba_f32().data,
        second.frame.rgba_f32().data,
        "cached working transform must preserve the exact CPU fallback frame"
    );
}

#[test]
fn media_preview_scene_linear_source_stays_gpu_eligible_and_lazily_caches_cpu_fallback() {
    let samples = Arc::new(vec![-0.25, 0.18, 2.0, 1.0, 4.0, 0.5, -1.0, 0.25]);
    let source =
        LinearFloatSource::new_shared(2, 1, ColorSpace::LinearRec709, Arc::clone(&samples));
    let input_transform = RenderInputTransform::to_working(
        WorkingColorSpace::LinearRec709,
        false,
        ColorEngine::mondrian_standard(),
    );
    let frame = MediaPreviewFrame::from_source(
        MediaPreviewGpuSourceFrame::new(source, input_transform),
        Resolution { width: 2, height: 1 },
        test_preview_semantic_identity(43),
        mondrian_playback::FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary::from_path(PreviewDecodeExecutionPath::SoftwareCpu),
    );

    let gpu_source = frame.gpu_source().expect("scene-linear GPU source");
    let CpuSourceColorFrame::LinearFloat(gpu_float) = gpu_source.source.as_ref() else {
        panic!("scene-linear preview must retain a float GPU source");
    };
    assert!(Arc::ptr_eq(&gpu_float.data_shared(), &samples));
    assert_eq!(frame.reserved_cpu_bytes(), 64);

    let first = frame.working_frame().expect("first float CPU fallback");
    let second = frame.working_frame().expect("cached float CPU fallback");
    assert!(first.color_diagnostics.is_some());
    assert!(second.color_diagnostics.is_none());
    assert_eq!(first.frame.rgba_f32().data[0], [-0.25, 0.18, 2.0, 1.0]);
    assert_eq!(first.frame.rgba_f32().data, second.frame.rgba_f32().data);
}

#[test]
fn gpu_viewer_hardware_decode_admission_covers_every_access_mode() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();

    assert_eq!(
        service.hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::PlaybackCursor),
        PreviewHardwareDecodeRequest::Auto
    );
    service.set_playback_hardware_decode_admission(PlaybackHardwareDecodeAdmission {
        request: PreviewHardwareDecodeRequest::PreferGpuResident,
        hardware_decode_device_selector: Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1)),
        renderer_native_import_ready: true,
        renderer_import_mode: Some(mondrian_renderer::GpuNativeDecodedFrameImportMode::ZeroCopy),
        native_import_admission_ready: true,
        admission_blocker: None,
        renderer_supported_handle_kinds: 1,
        renderer_supported_source_texture_formats: 1,
        renderer_supports_nv12: true,
        renderer_supports_p010: true,
        renderer_supported_surface_hint_mask: 3,
    });
    assert_eq!(
        service.hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::PlaybackCursor),
        PreviewHardwareDecodeRequest::PreferGpuResident
    );
    assert_eq!(
        service.hardware_decode_device_selector_for_access_mode(
            PreviewDecodeAccessMode::PlaybackCursor
        ),
        Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1))
    );
    assert_eq!(
        service.hardware_decode_request_for_access_mode(PreviewDecodeAccessMode::ScrubCursor),
        PreviewHardwareDecodeRequest::PreferGpuResident
    );
    assert_eq!(
        service
            .hardware_decode_device_selector_for_access_mode(PreviewDecodeAccessMode::ScrubCursor),
        Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1))
    );
    assert_eq!(
        service.hardware_decode_request_for_access_mode(
            PreviewDecodeAccessMode::RandomAccessStillFrame
        ),
        PreviewHardwareDecodeRequest::PreferGpuResident
    );
    assert_eq!(
        service.hardware_decode_device_selector_for_access_mode(
            PreviewDecodeAccessMode::RandomAccessStillFrame
        ),
        Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1))
    );
}

#[test]
fn gpu_viewer_hardware_decode_admission_does_not_invent_native_payload_contract() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.set_playback_hardware_decode_admission(PlaybackHardwareDecodeAdmission {
        request: PreviewHardwareDecodeRequest::PreferGpuResident,
        hardware_decode_device_selector: Some(HwAccelDeviceSelector::D3D12VaAdapterIndex(1)),
        renderer_native_import_ready: true,
        renderer_import_mode: Some(mondrian_renderer::GpuNativeDecodedFrameImportMode::ZeroCopy),
        native_import_admission_ready: true,
        admission_blocker: None,
        renderer_supported_handle_kinds: 1,
        renderer_supported_source_texture_formats: 1,
        renderer_supports_nv12: true,
        renderer_supports_p010: false,
        renderer_supported_surface_hint_mask: 1,
    });
    let key = test_media_key(0);
    assert_eq!(
        service.hardware_decode_request_for_key(PreviewDecodeAccessMode::PlaybackCursor, &key),
        PreviewHardwareDecodeRequest::PreferHardwareDecode,
        "a CPU geometry with no proven surface hint cannot be promoted by later admission"
    );
}

#[test]
fn scrub_adaptation_switches_for_hot_region_and_slow_latency() {
    let mut adaptation = PreviewScrubAdaptationState::default();
    let mut key = test_media_key(100);
    let observed_at = Instant::now();

    assert_eq!(
        adaptation
            .observe_request(key.asset_id, key.source_sample().time(), observed_at)
            .scrub_class,
        PreviewScrubAdaptiveClass::Normal
    );
    let next_source_time = key
        .source_sample()
        .time()
        .checked_add(mondrian_core::TimelineTime::new(1, 10).expect("exact source delta"))
        .expect("source time remains valid");
    key = test_media_key_with_source_time(key, next_source_time);
    assert_eq!(
        adaptation
            .observe_request(
                key.asset_id,
                key.source_sample().time(),
                observed_at + Duration::from_millis(1),
            )
            .scrub_class,
        PreviewScrubAdaptiveClass::Normal
    );
    let next_source_time = key
        .source_sample()
        .time()
        .checked_add(mondrian_core::TimelineTime::new(1, 10).expect("exact source delta"))
        .expect("source time remains valid");
    key = test_media_key_with_source_time(key, next_source_time);
    assert_eq!(
        adaptation
            .observe_request(
                key.asset_id,
                key.source_sample().time(),
                observed_at + Duration::from_millis(2),
            )
            .scrub_class,
        PreviewScrubAdaptiveClass::HotRegion
    );

    let slow_decode = PreviewDecodeDiagnostics {
        path: PreviewDecodePath::InProcessFfmpegCpuRgba,
        elapsed_us: PREVIEW_SCRUB_SLOW_LATENCY_US,
        cache_hit: false,
        access_mode: PreviewDecodeAccessMode::ScrubCursor,
        external_process: false,
        cpu_resident: true,
        seek_performed: true,
        requested_pts: Some(100),
        selected_pts: Some(100),
        selected_duration_pts: Some(40),
        selected_temporal_extent_source: PreviewTemporalExtentSource::FrameDuration,
        temporal_approximation: false,
        seek_strategy: PreviewDecodeSeekStrategy::BoundedAnyFrame,
        forward_reuse_frame_window: 0,
        forward_decode_budget_frames: 0,
        any_seek_window_ms: 0,
        scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
        hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
        hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
        hardware_decode_candidate_backend: None,
        hardware_decode_candidate_handle_kind: None,
        hardware_decode_adapter_available: false,
        hardware_decode_ffmpeg_device_type_available: false,
        hardware_decode_ffmpeg_codec_config_available: false,
        hardware_decode_ffmpeg_hw_pixel_format: None,
        hardware_decode_ffmpeg_device_context_attempted: false,
        hardware_decode_ffmpeg_device_context_created: false,
        hardware_decode_ffmpeg_device_context_error_code: None,
        hardware_decode_cpu_transfer_configured: false,
        hardware_decode_cpu_transfer_observed: false,
        hardware_decode_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
        session_disposition: PreviewDecodeSessionDisposition::Opened,
        forward_reused: false,
        seek_index_available: false,
        seek_index_keyframes: 0,
        seek_index_observed_packets: 0,
        seek_index_source: PreviewSeekIndexSource::None,
        seek_index_used: false,
        seek_index_anchor_pts: None,
        decoded_frame_count: 0,
        threading_kind: PreviewDecodeThreadingKind::None,
        threading_count: 0,
        stage_durations: PreviewDecodeStageDurations::default(),
        hw_accel_backend: HwAccelBackend::None,
        hardware_decode_active: false,
        zero_copy_active: false,
        decoded_frame_residency: DecodedFrameResidency::CpuRgba,
        gpu_frame_handle_kind: None,
        hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
        native_decode_fallback: None,
        decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
        decoded_video_sampling: DecodedVideoSampling::default(),
    };
    adaptation.observe_decode(slow_decode);
    adaptation.observe_decode(slow_decode);
    let next_source_time = key
        .source_sample()
        .time()
        .checked_add(mondrian_core::TimelineTime::new(1, 10).expect("exact source delta"))
        .expect("source time remains valid");
    key = test_media_key_with_source_time(key, next_source_time);
    assert_eq!(
        adaptation
            .observe_request(
                key.asset_id,
                key.source_sample().time(),
                observed_at + Duration::from_millis(3),
            )
            .scrub_class,
        PreviewScrubAdaptiveClass::SlowLatency
    );

    let mut failed_adaptation = PreviewScrubAdaptationState::default();
    failed_adaptation.observe_failure(
        PreviewDecodeAccessMode::ScrubCursor,
        MediaPreviewFailureReason::ForwardDecodeBudgetExhausted,
    );
    assert_eq!(
        failed_adaptation
            .observe_request(
                key.asset_id,
                key.source_sample().time(),
                observed_at + Duration::from_millis(4),
            )
            .scrub_class,
        PreviewScrubAdaptiveClass::SlowLatency
    );
}

#[test]
fn preview_frame_store_evicts_least_recently_used_media_frame() {
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        media_entry_capacity: 2,
        media_byte_budget: 1_024,
        media_resource_unit_budget: 4,
        current_media_working_set_entry_limit: 8,
        current_media_working_set_byte_limit: 4_096,
        current_media_working_set_resource_unit_limit: 8,
        viewer_entry_capacity: 2,
        viewer_byte_budget: 1_024,
        failure_entry_capacity: 2,
    });
    let first = test_media_key(1);
    let second = test_media_key(2);
    let third = test_media_key(3);

    assert!(admit_test_media_frame(
        &mut store,
        first.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(admit_test_media_frame(
        &mut store,
        second.clone(),
        test_media_frame(2),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(store.media_frame(&first).is_some());

    assert!(admit_test_media_frame(
        &mut store,
        third.clone(),
        test_media_frame(3),
        MediaPreviewRequestPriority::Prefetch,
    ));

    assert_eq!(store.diagnostics().media_entries, 2);
    assert!(store.media_frame(&first).is_some());
    assert!(store.media_frame(&second).is_none());
    assert!(store.media_frame(&third).is_some());
}

#[test]
fn preview_frame_store_preserves_existing_media_frame_on_same_key_retry() {
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        media_entry_capacity: 2,
        media_byte_budget: 1_024,
        media_resource_unit_budget: 4,
        current_media_working_set_entry_limit: 8,
        current_media_working_set_byte_limit: 4_096,
        current_media_working_set_resource_unit_limit: 8,
        viewer_entry_capacity: 2,
        viewer_byte_budget: 1_024,
        failure_entry_capacity: 2,
    });
    let key = test_media_key(1);

    assert!(admit_test_media_frame(
        &mut store,
        key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(!admit_test_media_frame(
        &mut store,
        key.clone(),
        test_media_frame(9),
        MediaPreviewRequestPriority::Prefetch,
    ));

    let frame = store.media_frame(&key).expect("original frame");
    assert_eq!(store.diagnostics().media_entries, 1);
    assert_eq!(test_media_frame_rgba8(&frame), vec![1, 0, 0, 255]);
}

#[test]
fn viewer_frame_store_does_not_alias_equal_compact_diagnostic_hashes() {
    let first_fingerprint = [0x5a; 32];
    let mut second_fingerprint = first_fingerprint;
    second_fingerprint[31] ^= 0xff;
    let first_identity = PreviewSemanticIdentity::from_test_fingerprint(first_fingerprint);
    let second_identity = PreviewSemanticIdentity::from_test_fingerprint(second_fingerprint);
    assert_eq!(
        first_identity.compact_diagnostic_hash(),
        second_identity.compact_diagnostic_hash(),
        "test precondition: the legacy 64-bit projection collides"
    );

    let sequence_id = SequenceId::new();
    let first_key = PreviewOutputKey::new(sequence_id, 1, 1, first_identity);
    let second_key = PreviewOutputKey::new(sequence_id, 1, 1, second_identity);
    assert_ne!(first_key, second_key);
    assert_ne!(
        preview_raster_resource_key(&first_key),
        preview_raster_resource_key(&second_key),
        "presentation registration must retain the complete identity"
    );

    let first_frame = PreviewRasterFrame::new(
        "first-viewer-frame",
        1,
        1,
        PreviewRasterColorSpace::Srgb,
        vec![1, 2, 3, 255],
    )
    .expect("first Viewer frame");
    let second_frame = PreviewRasterFrame::new(
        "second-viewer-frame",
        1,
        1,
        PreviewRasterColorSpace::Srgb,
        vec![4, 5, 6, 255],
    )
    .expect("second Viewer frame");
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        media_entry_capacity: 1,
        media_byte_budget: 16,
        media_resource_unit_budget: 1,
        current_media_working_set_entry_limit: 1,
        current_media_working_set_byte_limit: 16,
        current_media_working_set_resource_unit_limit: 1,
        viewer_entry_capacity: 2,
        viewer_byte_budget: 16,
        failure_entry_capacity: 1,
    });

    assert!(store.insert_viewer_frame(first_key.clone(), first_frame));
    assert!(store.insert_viewer_frame(second_key.clone(), second_frame));
    assert_eq!(
        store.viewer_frame(&first_key).expect("first frame").rgba.as_ref(),
        &[1, 2, 3, 255]
    );
    assert_eq!(
        store.viewer_frame(&second_key).expect("second frame").rgba.as_ref(),
        &[4, 5, 6, 255]
    );
}

#[test]
fn preview_frame_store_stays_within_budget_across_one_hundred_media_regions() {
    let frame_bytes = test_media_frame(0).reserved_cpu_bytes();
    let byte_budget = frame_bytes.saturating_mul(3);
    let mut store = test_cpu_frame_store(100, byte_budget, 8);

    for region in 0..100i64 {
        assert!(admit_test_media_frame(
            &mut store,
            test_media_key(region),
            test_media_frame(region as u8),
            MediaPreviewRequestPriority::Prefetch,
        ));
        let diagnostics = store.diagnostics();
        assert!(diagnostics.media_reserved_bytes <= diagnostics.media_byte_budget);
    }

    let diagnostics = store.diagnostics();
    assert_eq!(diagnostics.media_entries, 3);
    assert_eq!(diagnostics.media_evictions, 97);
    assert_eq!(diagnostics.media_reserved_bytes, byte_budget);

    store.clear_all();
    let cleared = store.diagnostics();
    assert_eq!(cleared.media_entries, 0);
    assert_eq!(cleared.media_reserved_bytes, 0);
}

#[test]
fn preview_frame_store_admits_current_beyond_optional_cache_into_bounded_working_set() {
    let current_key = test_media_key(200);
    let current_frame = test_media_frame(7);
    let frame_bytes = current_frame.reserved_cpu_bytes();
    let mut store = test_cpu_frame_store(4, frame_bytes.saturating_sub(1), 4);

    assert!(admit_test_media_frame(
        &mut store,
        current_key.clone(),
        current_frame,
        MediaPreviewRequestPriority::Current,
    ));
    assert!(store.media_frame(&current_key).is_some());
    let current = store.diagnostics();
    assert_eq!(current.media_entries, 0);
    assert_eq!(current.current_media_overflow_bytes, frame_bytes);
    assert_eq!(current.media_oversize_rejections, 0);

    let prefetch_key = test_media_key(201);
    assert!(!admit_test_media_frame(
        &mut store,
        prefetch_key.clone(),
        test_media_frame(8),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(store.media_frame(&prefetch_key).is_none());
    assert!(store.media_frame(&current_key).is_some());

    store.clear_current_media_overflow();
    assert!(store.media_frame(&current_key).is_none());
    assert_eq!(store.diagnostics().current_media_overflow_bytes, 0);
}

#[test]
fn preview_frame_store_reports_demand_capacity_without_terminal_decode_memory() {
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        media_entry_capacity: 2,
        media_byte_budget: 1_024,
        media_resource_unit_budget: 4,
        current_media_working_set_entry_limit: 1,
        current_media_working_set_byte_limit: 1_024,
        current_media_working_set_resource_unit_limit: 4,
        viewer_entry_capacity: 1,
        viewer_byte_budget: 1_024,
        failure_entry_capacity: 2,
    });
    let first = test_media_key(301);
    let second = test_media_key(302);
    assert!(admit_test_media_frame(
        &mut store,
        first.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(admit_test_media_frame(
        &mut store,
        second.clone(),
        test_media_frame(2),
        MediaPreviewRequestPriority::Prefetch,
    ));
    let demand_id = test_media_work_demand(17);
    let first_guarded = store
        .protected_media_frame(&first, demand_id)
        .expect("first demand allocation fits")
        .expect("first frame is resident");
    assert!(matches!(
        store.protected_media_frame(&second, demand_id),
        Err(crate::app::preview_frame_store::MediaFrameProtectionError::CurrentWorkingSetCapacity)
    ));
    assert!(!store.contains_failure(&second));
    assert_eq!(store.diagnostics().media_current_working_set_rejections, 1);
    drop(first_guarded);
}

#[test]
fn preview_frame_store_clear_releases_frames_failures_and_reserved_bytes() {
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        media_entry_capacity: 2,
        media_byte_budget: 1_024,
        media_resource_unit_budget: 4,
        current_media_working_set_entry_limit: 8,
        current_media_working_set_byte_limit: 4_096,
        current_media_working_set_resource_unit_limit: 8,
        viewer_entry_capacity: 2,
        viewer_byte_budget: 1_024,
        failure_entry_capacity: 2,
    });
    let key = test_media_key(1);

    assert!(admit_test_media_frame(
        &mut store,
        key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    store.remember_failure(key.clone());
    store.clear_all();

    let diagnostics = store.diagnostics();
    assert_eq!(diagnostics.media_entries, 0);
    assert_eq!(diagnostics.media_reserved_bytes, 0);
    assert_eq!(diagnostics.failure_entries, 0);
    assert!(store.media_frame(&key).is_none());
    assert!(!store.contains_failure(&key));
}

#[test]
fn media_preview_key_includes_file_length_in_identity() {
    let first = test_media_key_with_physical_source(
        test_media_key(1),
        PathBuf::from("E:/media/replaced.mov"),
        test_media_file_fingerprint(1_024, 10),
        0,
    );
    let second = test_media_key_with_physical_source(
        first.clone(),
        PathBuf::from("E:/media/replaced.mov"),
        test_media_file_fingerprint(2_048, 10),
        0,
    );

    assert_ne!(first, second);
}

#[test]
fn media_preview_cache_does_not_reuse_same_path_with_different_file_length() {
    let old_key = test_media_key_with_physical_source(
        test_media_key(1),
        PathBuf::from("E:/media/replaced.mov"),
        test_media_file_fingerprint(1_024, 10),
        0,
    );
    let new_key = test_media_key_with_physical_source(
        old_key.clone(),
        PathBuf::from("E:/media/replaced.mov"),
        test_media_file_fingerprint(2_048, 10),
        0,
    );
    let mut store = test_cpu_frame_store(2, 1_024, 2);

    assert!(admit_test_media_frame(
        &mut store,
        old_key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));

    assert!(store.media_frame(&new_key).is_none());
    assert!(store.media_frame(&old_key).is_some());
}

#[test]
fn media_preview_cache_isolates_physical_video_streams() {
    let first_stream = test_media_key(1);
    let second_stream = test_media_key_with_physical_source(
        first_stream.clone(),
        first_stream.decode.source().path().to_path_buf(),
        first_stream.decode.source().fingerprint(),
        first_stream.decode.source().video_stream_index().saturating_add(1),
    );
    let mut store = test_cpu_frame_store(2, 1_024, 2);

    assert!(admit_test_media_frame(
        &mut store,
        first_stream.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert!(store.media_frame(&second_stream).is_none());
    assert!(store.media_frame(&first_stream).is_some());
}

#[test]
fn media_preview_key_rejects_incomplete_source_revision_before_frame_store() {
    let store = test_cpu_frame_store(2, 1_024, 2);

    let source = mondrian_media::PreviewDecodeSource::from_frozen_cpu_stream(
        std::env::temp_dir().join("mondrian-preview-incomplete.mov"),
        MediaFileFingerprint::default(),
        0,
        Resolution { width: 1920, height: 1080 },
    );

    assert!(matches!(
        source,
        Err(mondrian_media::PreviewDecodeContractError::IncompleteSourceRevision { .. })
    ));
    assert_eq!(store.diagnostics().media_entries, 0);
    assert_eq!(store.diagnostics().failure_entries, 0);
}

#[test]
fn media_preview_caches_isolate_exact_color_engines() {
    let old_key = test_media_key(1);
    let mut new_key = old_key.clone();
    new_key.preparation_intent = RenderInputTransform::to_working(
        WorkingColorSpace::LinearRec709,
        false,
        ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        },
    )
    .into();
    let mut store = test_cpu_frame_store(2, 1_024, 2);

    assert!(admit_test_media_frame(
        &mut store,
        old_key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    store.remember_failure(old_key.clone());

    assert!(store.media_frame(&new_key).is_none());
    assert!(!store.contains_failure(&new_key));
    assert!(store.media_frame(&old_key).is_some());
    assert!(store.contains_failure(&old_key));
}

#[test]
fn media_preview_failure_cache_evicts_least_recently_used_key() {
    let mut store = test_cpu_frame_store(2, 1_024, 2);
    let first = test_media_key(1);
    let second = test_media_key(2);
    let third = test_media_key(3);

    store.remember_failure(first.clone());
    store.remember_failure(second.clone());
    assert!(store.contains_failure(&first));

    store.remember_failure(third.clone());

    assert_eq!(store.diagnostics().failure_entries, 2);
    assert!(store.contains_failure(&first));
    assert!(!store.contains_failure(&second));
    assert!(store.contains_failure(&third));
}

#[test]
fn media_preview_failure_cache_updates_existing_key_without_growing() {
    let mut store = test_cpu_frame_store(2, 1_024, 2);
    let key = test_media_key(1);

    store.remember_failure(key.clone());
    store.remember_failure(key.clone());

    assert_eq!(store.diagnostics().failure_entries, 1);
    assert!(store.contains_failure(&key));
}

#[test]
fn transport_intent_synchronization_retires_only_real_discontinuities() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let reusable_media_key = test_media_key(44);

    service.synchronize_transport_intent(state.preview_transport_intent());
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        reusable_media_key.clone(),
        test_media_frame(44),
        MediaPreviewRequestPriority::Prefetch,
    ));
    assert_eq!(
        service.decode_residency.diagnostics().active_family,
        Some(PreviewDecodeResidencyFamily::Interactive)
    );
    assert_eq!(service.decode_residency.diagnostics().transitions, 1);
    assert_eq!(service.diagnostics().interactive_cancel_requests, 0);

    service.seed_pending_preview_work_for_test();
    state
        .dispatch_action(mondrian_editor_state::Action::DeselectAll)
        .expect("ordinary editor action");
    service.synchronize_transport_intent(state.preview_transport_intent());
    assert_eq!(
        service.diagnostics().interactive_cancel_requests,
        0,
        "an Action that preserves transport intent must not request cancellation"
    );
    assert_eq!(service.diagnostics().scheduler.pending_requests, 1);

    state.play().expect("play");
    service.synchronize_transport_intent(state.preview_transport_intent());
    let after_play = service.diagnostics();
    assert_eq!(after_play.interactive_cancel_requests, 1);
    assert_eq!(after_play.scheduler.pending_requests, 0);
    assert_eq!(
        service.decode_residency.diagnostics().active_family,
        Some(PreviewDecodeResidencyFamily::Playback)
    );
    assert_eq!(service.decode_residency.diagnostics().transitions, 2);
    assert!(
        service.frame_store.borrow_mut().media_frame(&reusable_media_key).is_some(),
        "transport authority must not invalidate an exact semantic CPU-frame cache entry"
    );

    service.seed_pending_preview_work_with_access_mode_for_test(
        PreviewDecodeAccessMode::PlaybackCursor,
        None,
    );
    state.seek(5).expect("seek");
    service.synchronize_transport_intent(state.preview_transport_intent());
    let after_seek = service.diagnostics();
    assert_eq!(
        after_seek.interactive_cancel_requests, 2,
        "a new Playback Epoch must retire prior-family work even while play intent remains active"
    );
    assert_eq!(after_seek.scheduler.pending_requests, 0);
    assert_eq!(
        service.decode_residency.diagnostics().active_family,
        Some(PreviewDecodeResidencyFamily::Playback),
        "a seek must not needlessly rebuild the same-family decoder context"
    );
    assert_eq!(service.decode_residency.diagnostics().transitions, 2);

    service.seed_pending_preview_work_with_access_mode_for_test(
        PreviewDecodeAccessMode::PlaybackCursor,
        None,
    );
    state.pause().expect("pause");
    service.synchronize_transport_intent(state.preview_transport_intent());
    let after_pause = service.diagnostics();
    assert_eq!(after_pause.interactive_cancel_requests, 3);
    assert_eq!(after_pause.scheduler.pending_requests, 0);
    assert_eq!(
        service.decode_residency.diagnostics().active_family,
        Some(PreviewDecodeResidencyFamily::Interactive)
    );
    assert_eq!(service.decode_residency.diagnostics().transitions, 3);
    assert!(
        service.frame_store.borrow_mut().media_frame(&reusable_media_key).is_some(),
        "play/pause retirement cancels work but retains independently valid CPU residency"
    );

    state.seek(6).expect("seek");
    service.synchronize_transport_intent(state.preview_transport_intent());
    let after_stopped_seek = service.diagnostics();
    assert_eq!(after_stopped_seek.interactive_cancel_requests, 4);
    assert!(
        service.frame_store.borrow_mut().media_frame(&reusable_media_key).is_some(),
        "a stopped seek without an exact Viewer output must not evict reusable CPU residency"
    );
    service.shutdown();
}

#[test]
fn completed_decode_residency_barrier_retries_a_pending_candidate_once() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.decode_residency.register_worker(MediaPreviewWorkerLane::NonPlayback);

    assert!(service.decode_residency.activate(PreviewDecodeResidencyFamily::Interactive));
    let interactive_revision = service.decode_residency.revision();
    assert!(service.decode_residency.activate(PreviewDecodeResidencyFamily::Playback));
    let retirement = service
        .decode_residency
        .worker_directive(MediaPreviewWorkerLane::NonPlayback, interactive_revision)
        .expect("non-playback worker must retire for playback residency");
    assert!(retirement.retire_context());

    service
        .decode_residency_waiting
        .set(Some(PreviewDecodeAccessMode::PlaybackCursor));
    service
        .decode_residency
        .acknowledge_retirement(MediaPreviewWorkerLane::NonPlayback, retirement.revision());

    let mut state = AppState::new();
    state.set_playback_frame_running(0);
    let first = service.poll_playback_work(
        state.pending_playback_frame_demand_identity(),
        state.preview_transport_intent(),
    );
    assert!(
        first.candidate_retry_required,
        "the final residency acknowledgement must make the blocked candidate actionable"
    );

    let second = service.poll_playback_work(
        state.pending_playback_frame_demand_identity(),
        state.preview_transport_intent(),
    );
    assert!(
        !second.candidate_retry_required,
        "one coordination edge must not create an unbounded retry loop"
    );

    service.decode_residency.unregister_worker(MediaPreviewWorkerLane::NonPlayback);
    service.shutdown();
}

#[test]
fn completed_decode_residency_barrier_remains_actionable_for_a_late_waiter() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    service.decode_residency.register_worker(MediaPreviewWorkerLane::NonPlayback);
    assert!(service.decode_residency.activate(PreviewDecodeResidencyFamily::Playback));
    let retirement = service
        .decode_residency
        .worker_directive(MediaPreviewWorkerLane::NonPlayback, 0)
        .expect("non-playback retirement directive");
    service
        .decode_residency
        .acknowledge_retirement(MediaPreviewWorkerLane::NonPlayback, retirement.revision());

    let mut state = AppState::new();
    state.set_playback_frame_running(0);
    let before_wait = service.poll_playback_work(
        state.pending_playback_frame_demand_identity(),
        state.preview_transport_intent(),
    );
    assert!(!before_wait.candidate_retry_required);

    service
        .decode_residency_waiting
        .set(Some(PreviewDecodeAccessMode::PlaybackCursor));
    let late_wait = service.poll_playback_work(
        state.pending_playback_frame_demand_identity(),
        state.preview_transport_intent(),
    );
    assert!(late_wait.candidate_retry_required);
    assert!(service.decode_residency_waiting.get().is_none());

    let consumed = service.poll_playback_work(
        state.pending_playback_frame_demand_identity(),
        state.preview_transport_intent(),
    );
    assert!(!consumed.candidate_retry_required);
    service.decode_residency.unregister_worker(MediaPreviewWorkerLane::NonPlayback);
    service.shutdown();
}

#[test]
fn preview_service_lifecycle_cancel_clears_pending_and_cached_state() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let visual_sequence = Sequence::new("lifecycle visual cache");
    let mut visual_program_seeded = false;
    for _ in 0..64 {
        match service.visual_programs.borrow_mut().prepare(&visual_sequence) {
            Ok(_) => {
                visual_program_seeded = true;
                break;
            }
            Err(mondrian_renderer::PreparedVisualProgramError::EffectRegistryChanged {
                ..
            }) => {}
            Err(error) => panic!("unexpected visual preparation failure: {error}"),
        }
    }
    assert!(visual_program_seeded, "Effect registry must stabilize");
    let key = test_media_key(1);
    let generation = service.scheduler.begin_generation();
    assert_eq!(
        service.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key: key.clone(),
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::ScrubCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    service.frame_store.borrow_mut().remember_failure(key.clone());

    service.cancel_all_work_for_lifecycle();

    assert_eq!(service.scheduler.pending_len(), 0);
    assert!(!service.scheduler.is_decode_current(
        &key,
        generation,
        PreviewDecodeAccessMode::ScrubCursor
    ));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.interactive_cancel_requests, 1);
    assert_eq!(diagnostics.interactive_cancel_scheduler_requests, 1);
    assert_eq!(diagnostics.interactive_cancel_queued_jobs, 1);
    assert_eq!(diagnostics.queue_canceled_jobs, 1);
    let frame_store = service.frame_store.borrow().diagnostics();
    assert_eq!(frame_store.media_entries, 0);
    assert_eq!(frame_store.media_reserved_bytes, 0);
    assert_eq!(frame_store.failure_entries, 0);
    let visual_programs = service.visual_programs.borrow().diagnostics();
    assert_eq!(visual_programs.entries, 0);
    assert_eq!(visual_programs.scope_rotations, 1);
    service.shutdown();
}

#[test]
fn preview_service_idle_release_preserves_failure_memory() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let key = test_media_key(1);
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    service.frame_store.borrow_mut().remember_failure(key);

    assert!(service.try_release_idle_media_residency());

    let diagnostics = service.frame_store.borrow().diagnostics();
    assert_eq!(diagnostics.media_entries, 0);
    assert_eq!(diagnostics.media_reserved_bytes, 0);
    assert_eq!(diagnostics.failure_entries, 1);
    service.shutdown();
}

#[test]
fn decoder_device_generation_replacement_obsoletes_work_but_preserves_cpu_residency() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let key = test_media_key(1);
    let generation = service.scheduler.begin_generation();
    assert_eq!(
        service.scheduler.request(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        service.jobs.enqueue(MediaPreviewJob {
            key: key.clone(),
            generation,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::ScrubCursor,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
            residency_work: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        key.clone(),
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));

    service.retire_decoder_device_generation();

    assert_eq!(service.scheduler.pending_len(), 0);
    assert!(!service.scheduler.is_decode_current(
        &key,
        generation,
        PreviewDecodeAccessMode::ScrubCursor
    ));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.interactive_cancel_requests, 0);
    assert_eq!(diagnostics.interactive_cancel_scheduler_requests, 0);
    assert_eq!(diagnostics.interactive_cancel_queued_jobs, 0);
    let frame_store = service.frame_store.borrow().diagnostics();
    assert_eq!(frame_store.media_entries, 1);
    assert!(frame_store.media_reserved_bytes > 0);
    service.shutdown();
}

#[test]
fn zero_copy_admission_is_not_published_without_renderer_device_root() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let error = service
        .set_renderer_hardware_decode_admission(
            PlaybackHardwareDecodeAdmission {
                request: PreviewHardwareDecodeRequest::PreferGpuResident,
                hardware_decode_device_selector: Some(
                    mondrian_media::HwAccelDeviceSelector::D3D12VaAdapterIndex(0),
                ),
                renderer_native_import_ready: true,
                renderer_import_mode: Some(
                    mondrian_renderer::GpuNativeDecodedFrameImportMode::ZeroCopy,
                ),
                native_import_admission_ready: true,
                admission_blocker: None,
                renderer_supported_handle_kinds: 1,
                renderer_supported_source_texture_formats: 2,
                renderer_supports_nv12: true,
                renderer_supports_p010: true,
                renderer_supported_surface_hint_mask: 3,
            },
            None,
        )
        .expect_err("same-device admission requires the exact renderer root");

    assert!(matches!(
        error,
        super::hardware_admission::RendererHardwareDecodeAdmissionError::MissingDeviceRoot
    ));
    assert_eq!(
        service.playback_hardware_decode_request_for_test(),
        PreviewHardwareDecodeRequest::Auto
    );
    service.shutdown();
}

#[test]
fn preview_service_idle_release_fails_closed_while_intent_is_pending() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let key = test_media_key(1);
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        key,
        test_media_frame(1),
        MediaPreviewRequestPriority::Prefetch,
    ));
    service.execution.borrow_mut().set_pending(true);

    assert!(!service.try_release_idle_media_residency());
    assert_eq!(service.frame_store.borrow().diagnostics().media_entries, 1);
    service.shutdown();
}

#[test]
fn preview_service_settled_release_requires_an_exact_viewer_output() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();

    assert!(!service.try_release_settled_transport_media_residency());
    service.shutdown();
}

#[test]
fn gpu_output_registration_does_not_run_settled_media_release_inside_commit() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected stopped GPU candidate"),
    };
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        test_media_key(901),
        test_media_frame(91),
        MediaPreviewRequestPriority::Prefetch,
    ));
    let output = ViewerExternalTextureFrame::new_spatial(
        "bounded-registration",
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("valid test presentation"),
    )
    .expect("valid external texture frame");

    service.register_gpu_output(frame.output_key.clone(), output);

    assert_eq!(service.frame_store.borrow().diagnostics().media_entries, 1);
    assert!(service.try_release_settled_transport_media_residency());
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_entries,
        1,
        "bounded CPU residency must remain reusable for immediate playback"
    );
    assert!(service.try_release_settled_transport_all_media_residency());
    assert_eq!(service.frame_store.borrow().diagnostics().media_entries, 0);
    service.shutdown();
}

#[test]
fn stopped_generation_rotation_retries_media_release_after_worker_settles() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let mut state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
    let frame = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        _ => panic!("expected first stopped GPU candidate"),
    };
    let generation = service.execution.borrow().generation();
    let media_key = test_media_key(900);
    let execution_id = begin_test_media_execution(
        &service,
        media_key.clone(),
        generation,
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        MediaPreviewWorkerLane::NonPlayback,
    );
    assert!(admit_test_media_frame(
        &mut service.frame_store.borrow_mut(),
        media_key,
        test_media_frame(90),
        MediaPreviewRequestPriority::Prefetch,
    ));

    assert!(register_test_window_preview_output(
        &service,
        &frame,
        "first-output",
        ViewerExternalTexturePresentation::full_frame(frame.width, frame.height)
            .expect("valid test presentation"),
    ));
    assert_eq!(service.frame_store.borrow().diagnostics().media_entries, 1);
    assert!(service.scheduler.mark_execution_completed(execution_id));
    assert!(service.scheduler.resolve_execution(execution_id, true).status.is_current());
    assert_eq!(service.diagnostics().worker_queue.in_flight_jobs, 0);

    state.seek(5).expect("seek");
    let seeked = match execute_gpu_preview_for_test_app(&service, &state) {
        PreviewGpuFrameState::Ready(frame) => frame,
        PreviewGpuFrameState::Current(_) => {
            assert!(
                service.try_release_settled_transport_media_residency(),
                "an unchanged generation must keep the settled release authorized"
            );
            assert_eq!(service.frame_store.borrow().diagnostics().media_entries, 1);
            service.shutdown();
            return;
        }
        _ => panic!("expected stopped GPU candidate after seek"),
    };
    assert!(
        register_test_window_preview_output(
            &service,
            &seeked,
            "seeked-output",
            ViewerExternalTexturePresentation::full_frame(seeked.width, seeked.height)
                .expect("valid seeked presentation"),
        ),
        "the seeked exact output must register"
    );
    assert_eq!(
        service.frame_store.borrow().diagnostics().media_entries,
        1,
        "bounded CPU residency must remain reusable for immediate playback after the settled release"
    );
    service.shutdown();
}

#[test]
fn preview_service_completion_poll_respects_result_count_budget() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let results = install_preview_result_channel_for_test(&service);
    let generation = service.scheduler.begin_generation();
    for index in 0..3 {
        let key = test_media_key(index);
        assert_eq!(
            service.scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        results
            .send(test_successful_media_preview_result(
                &service,
                key,
                generation,
                index as u8,
            ))
            .expect("send test preview result");
    }

    assert!(service.poll_finished_with_budget(2, Duration::from_secs(1), None));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_successes, 2);
    assert_eq!(diagnostics.completion_poll_calls, 1);
    assert_eq!(diagnostics.completion_poll_results, 2);
    assert_eq!(diagnostics.completion_poll_max_results_per_poll, 2);
    assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 1);
    assert_eq!(diagnostics.completion_poll_time_budget_exhaustions, 0);
    assert_eq!(service.scheduler.pending_len(), 1);

    assert!(service.poll_finished_with_budget(2, Duration::from_secs(1), None));
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_successes, 3);
    assert_eq!(diagnostics.completion_poll_calls, 2);
    assert_eq!(diagnostics.completion_poll_results, 3);
    assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 1);
    assert_eq!(service.scheduler.pending_len(), 0);
    service.shutdown();
}

#[test]
fn preview_service_completion_poll_respects_time_budget() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let results = install_preview_result_channel_for_test(&service);
    let generation = service.scheduler.begin_generation();
    for index in 0..2 {
        let key = test_media_key(index);
        assert_eq!(
            service.scheduler.request(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
        );
        results
            .send(test_successful_media_preview_result(
                &service,
                key,
                generation,
                index as u8,
            ))
            .expect("send test preview result");
    }

    assert!(service.poll_finished_with_budget(8, Duration::ZERO, None));

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.decode_successes, 1);
    assert_eq!(diagnostics.completion_poll_calls, 1);
    assert_eq!(diagnostics.completion_poll_results, 1);
    assert_eq!(diagnostics.completion_poll_count_budget_exhaustions, 0);
    assert_eq!(diagnostics.completion_poll_time_budget_exhaustions, 1);
    assert_eq!(service.scheduler.pending_len(), 1);
    service.shutdown();
}

#[test]
fn media_preview_forward_prefetch_window_uses_sequence_frame_rate() {
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::FPS_24),
        Some(6)
    );
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::FPS_30),
        Some(8)
    );
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::FPS_60),
        Some(15)
    );
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::new(240, 1)),
        Some(MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES)
    );
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::FPS_10),
        Some(3)
    );
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::new(0, 1)),
        None
    );
    assert_eq!(
        media_preview_forward_prefetch_window_frames(Rational::new(24, 0)),
        None
    );
}

#[test]
fn steady_prefetch_reservations_do_not_consume_the_temporal_frame_buffer() {
    assert_eq!(media_preview_steady_prefetch_reservation_limit(1), 1);
    assert_eq!(media_preview_steady_prefetch_reservation_limit(2), 2);
    assert_eq!(
        media_preview_steady_prefetch_reservation_limit(15),
        MEDIA_PREVIEW_STEADY_PREFETCH_RESERVATION_LIMIT
    );
    assert_eq!(
        media_preview_steady_prefetch_reservation_limit(MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES),
        MEDIA_PREVIEW_STEADY_PREFETCH_RESERVATION_LIMIT
    );
}

#[test]
fn media_preview_decode_cancellation_keeps_current_frame_unbudgeted() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
            false,
        ),
        None,
    );
}

#[test]
fn media_preview_decode_cancellation_reports_shutdown() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(
                mondrian_playback::FrameExecutionCancellation::BrokerClosed { age: Duration::ZERO }
            ),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            Duration::ZERO,
            false,
        ),
        Some(MediaPreviewCancelReason::Shutdown),
    );
}

#[test]
fn preview_shutdown_signal_is_idempotent_without_timing_authority() {
    let shutdown = PreviewShutdownSignal::default();
    assert!(!shutdown.is_requested());

    assert!(!shutdown.request());
    assert!(shutdown.is_requested());
    assert!(shutdown.request());
}

#[test]
fn synchronous_preview_shutdown_reclaims_complete_worker_inventory() {
    let runtime = PreviewProductionRuntime::<()>::with_direct_worker_count_for_test(
        preview_decode_cpu_budget(),
        2,
    );

    let evidence = runtime.shutdown_and_wait();

    assert_eq!(evidence.schema_version, 4);
    assert_eq!(evidence.workers_started, 5);
    assert_eq!(evidence.workers_terminated, 5);
    assert_eq!(
        evidence.visual_dependency_worker,
        Some(PreviewOwnedWorkerShutdown::Terminated)
    );
    assert_eq!(evidence.worker_panics, 0);
    assert_eq!(evidence.current_thread_detachments, 0);
    assert_eq!(evidence.unverified_async_reaps, 0);
    assert!(evidence.all_workers_terminated());
}

#[test]
fn preview_shutdown_requires_explicit_healthy_dependency_observer_receipt() {
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let mut evidence = runtime.shutdown_and_wait();
    assert!(evidence.all_workers_terminated());
    for outcome in [
        None,
        Some(PreviewOwnedWorkerShutdown::NotStarted),
        Some(PreviewOwnedWorkerShutdown::Panicked),
        Some(PreviewOwnedWorkerShutdown::TimedOutDetached),
        Some(PreviewOwnedWorkerShutdown::CurrentThreadSkipped),
    ] {
        evidence.visual_dependency_worker = outcome;
        assert!(!evidence.all_workers_terminated());
    }
    evidence.visual_dependency_worker = Some(PreviewOwnedWorkerShutdown::Terminated);
    evidence.schema_version = 2;
    assert!(
        !evidence.all_workers_terminated(),
        "old schema cannot acquire new inventory proof"
    );
}

#[test]
fn preview_shutdown_requires_explicit_callback_ownership_receipt() {
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    let mut evidence = runtime.shutdown_and_wait();
    assert!(evidence.all_workers_terminated());
    let callbacks = evidence.work_callbacks.take().expect("explicit empty callback owner");
    assert!(callbacks.all_resources_released());
    assert!(!evidence.all_workers_terminated());
    evidence.work_callbacks = Some(PreviewWorkCallbackEvidence::default());
    assert!(!evidence.all_workers_terminated());
    evidence.work_callbacks = Some(callbacks);
    let started = evidence.workers_started;
    evidence.workers_started = 0;
    evidence.workers_terminated = 0;
    assert!(
        !evidence.all_workers_terminated(),
        "declared owners must be counted"
    );
    evidence.workers_started = started;
    evidence.workers_terminated = started;
    evidence.schema_version = 3;
    assert!(
        !evidence.all_workers_terminated(),
        "schema 3 cannot prove callback ownership"
    );
}

#[test]
fn preview_shutdown_counts_the_callback_retirement_worker() {
    let baseline =
        PreviewProductionRuntime::<()>::new_without_workers_for_test().shutdown_and_wait();
    assert!(baseline.all_workers_terminated());
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    runtime
        .work_watch()
        .install_waker(|| {})
        .unwrap_or_else(|failure| panic!("{}", failure.reason));
    let mut evidence = runtime.shutdown_and_wait();
    assert!(evidence.all_workers_terminated());
    assert_eq!(
        evidence.workers_started,
        baseline.workers_started + 1,
        "callback retirement adds exactly one worker to the real factory inventory"
    );
    evidence.workers_started = 1;
    evidence.workers_terminated = 1;
    assert!(!evidence.all_workers_terminated());
}

#[test]
fn preview_shutdown_retains_callback_failure_without_pumping_results() {
    for bounded in [false, true] {
        let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
        runtime
            .work_watch()
            .install_waker(|| panic!("injected callback failure"))
            .unwrap_or_else(|failure| panic!("{}", failure.reason));
        let live = runtime.diagnostics();
        assert!(live.work_callbacks.has_failure());
        assert_eq!(live.work_callbacks.invocation_panics, 1);
        let evidence = if bounded {
            runtime.shutdown_until(Instant::now() + Duration::from_secs(5))
        } else {
            runtime.shutdown_and_wait()
        };
        let callbacks = evidence.work_callbacks.expect("exact callback receipt");
        assert_eq!(callbacks.invocation_panics, 1);
        assert_eq!(callbacks.registrations_abandoned, 1);
        assert_eq!(
            callbacks.worker,
            Some(PreviewOwnedWorkerShutdown::Terminated)
        );
        assert_eq!(evidence.worker_panics, 0);
        assert!(
            !evidence.all_workers_terminated(),
            "clean workers cannot erase callback failure"
        );
    }
}

#[test]
fn preview_shutdown_rejects_required_render_cache_start_failure() {
    let runtime = PreviewProductionRuntime::<()>::with_direct_worker_count_for_test(
        preview_decode_cpu_budget(),
        1,
    );
    *runtime.timeline_render_cache.borrow_mut() =
        crate::app::preview_render_cache::PreviewTimelineRenderCache::with_start_failure_for_test(
            "intentional render-cache start failure",
        );

    let evidence = runtime.shutdown_and_wait();

    assert!(evidence.timeline_render_cache.required);
    assert!(evidence.timeline_render_cache.start_failed);
    assert!(evidence.timeline_render_cache.worker.is_none());
    assert!(!evidence.timeline_render_cache.all_resources_released());
    assert!(!evidence.all_workers_terminated());
}

#[test]
fn synchronous_preview_shutdown_rejects_panicked_worker_as_complete() {
    let worker = std::thread::spawn(|| panic!("intentional Preview shutdown test panic"));

    let evidence = join_preview_workers(vec![worker]);

    assert_eq!(evidence.workers_started, 1);
    assert_eq!(evidence.workers_terminated, 1);
    assert_eq!(evidence.worker_panics, 1);
    assert!(!evidence.all_workers_terminated());
}

#[test]
fn preview_shutdown_continues_after_opaque_worker_panic() {
    struct HostilePayload(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for HostilePayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("must not destroy opaque payload while closing Preview");
        }
    }
    for bounded in [false, true] {
        let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let payload = HostilePayload(Arc::clone(&drops));
        runtime.workers.borrow_mut().push(std::thread::spawn(move || {
            std::panic::panic_any(payload);
        }));
        let evidence = if bounded {
            runtime.shutdown_until(Instant::now() + Duration::from_secs(2))
        } else {
            runtime.shutdown_and_wait()
        };
        assert_eq!(evidence.workers_started, 4);
        assert_eq!(evidence.workers_terminated, 4);
        assert_eq!(evidence.worker_panics, 1);
        assert_eq!(evidence.worker_panic_payloads_abandoned, 1);
        assert_eq!(
            evidence.visual_dependency_worker,
            Some(PreviewOwnedWorkerShutdown::Terminated)
        );
        assert!(!evidence.all_workers_terminated());
        assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

#[test]
fn preview_dependency_worker_exit_is_visible_without_evaluation_or_result_poll() {
    let mut runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    assert!(!runtime.diagnostics().visual_dependency_health_failed);
    assert_eq!(
        runtime.visual_dependencies.shutdown_and_wait(),
        PreviewOwnedWorkerShutdown::Terminated
    );
    assert!(
        !runtime.visual_dependency_health_failed.get(),
        "no evaluation has latched the exit"
    );
    assert!(runtime.diagnostics().visual_dependency_health_failed);
    assert!(runtime.diagnostics().visual_dependency_health_failed);
    let evidence = runtime.shutdown_and_wait();
    assert!(
        !evidence.all_workers_terminated(),
        "an already consumed observer cannot invent a receipt"
    );
}

#[test]
fn bounded_preview_shutdown_detaches_a_worker_at_the_absolute_deadline() {
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_release = Arc::clone(&release);
    let worker = std::thread::spawn(move || {
        while !worker_release.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::yield_now();
        }
    });

    let (evidence, _) = join_preview_workers_until(vec![worker], Instant::now());

    assert_eq!(evidence.schema_version, 4);
    assert_eq!(evidence.workers_started, 1);
    assert_eq!(evidence.workers_terminated, 0);
    assert_eq!(evidence.worker_timeouts, 1);
    assert_eq!(evidence.worker_deadline_detachments, 1);
    assert!(!evidence.all_workers_terminated());
    release.store(true, std::sync::atomic::Ordering::Release);
}

#[test]
fn synchronous_preview_shutdown_rejects_prior_async_reap_as_unverified() {
    let runtime = PreviewProductionRuntime::<()>::with_direct_worker_count_for_test(
        preview_decode_cpu_budget(),
        1,
    );
    runtime.shutdown();

    let evidence = runtime.shutdown_and_wait();

    assert_eq!(evidence.unverified_async_reaps, 1);
    assert_eq!(evidence.workers_started, 4);
    assert_eq!(evidence.workers_terminated, 3);
    assert!(!evidence.all_workers_terminated());
}

#[test]
fn media_preview_shutdown_observation_uses_broker_close_timestamp() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let (sender, receiver) = scheduler.job_queue();
    let generation = scheduler.begin_generation();
    assert!(matches!(
        scheduler.request(
            test_media_key(500),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let execution_id = receiver
        .recv_for_worker(MediaPreviewWorkerLane::Playback)
        .and_then(|job| job.execution_id)
        .expect("worker execution lease");

    sender.close();
    let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
    assert!(matches!(
        scheduler_cancellation,
        Some(mondrian_playback::FrameExecutionCancellation::BrokerClosed { .. })
    ));
    let observed_at = Instant::now();
    let latency = media_preview_cancel_request_to_logical_observation_us(
        MediaPreviewCancelReason::Shutdown,
        scheduler_cancellation,
        observed_at,
        observed_at,
    );

    assert_eq!(
        latency,
        scheduler_cancellation
            .and_then(mondrian_playback::FrameExecutionCancellation::request_age)
            .map(app_duration_us)
    );
}

#[test]
fn media_preview_cancel_observation_uses_scheduler_invalidation_timestamp() {
    let scheduler = MediaPreviewScheduler::with_max_pending(2);
    let (_sender, receiver) = scheduler.job_queue();
    let generation = scheduler.begin_generation();
    assert!(matches!(
        scheduler.request(
            test_media_key(501),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let execution_id = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .and_then(|job| job.execution_id)
        .expect("worker execution lease");

    scheduler.begin_generation();
    let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
    let observed_at = Instant::now();
    let latency = media_preview_cancel_request_to_logical_observation_us(
        MediaPreviewCancelReason::Obsolete,
        scheduler_cancellation,
        observed_at,
        observed_at,
    );

    assert!(latency.is_some());
}

#[test]
fn media_preview_cancel_observation_uses_competing_request_timestamp() {
    let scheduler = MediaPreviewScheduler::with_max_pending(2);
    let (_sender, receiver) = scheduler.job_queue();
    let generation = scheduler.begin_generation();
    assert!(matches!(
        scheduler.request(
            test_media_key(502),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let execution_id = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .and_then(|job| job.execution_id)
        .expect("worker execution lease");
    assert!(matches!(
        scheduler.request(
            test_media_key(503),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        MediaPreviewRequestStatus::Scheduled { .. }
    ));
    let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
    let observed_at = Instant::now();
    let latency = media_preview_cancel_request_to_logical_observation_us(
        MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent,
        scheduler_cancellation,
        observed_at,
        observed_at,
    );

    assert!(latency.is_some());
}

#[test]
fn preview_service_shutdown_does_not_block_on_busy_worker() {
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        entered_tx.send(()).expect("signal worker started");
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });
    service.workers.borrow_mut().push(handle);
    entered_rx.recv_timeout(Duration::from_secs(1)).expect("worker should start");

    let started_at = Instant::now();
    service.shutdown();
    assert!(
        started_at.elapsed() < Duration::from_millis(100),
        "preview shutdown must not block the UI event loop while decode workers exit"
    );

    release_tx.send(()).expect("release worker");
}

#[test]
fn media_preview_decode_cancellation_budgets_prefetch_work() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US - 1),
            false,
        ),
        None,
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US),
            false,
        ),
        Some(MediaPreviewCancelReason::PrefetchDeadline),
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::ScrubCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
            false,
        ),
        None,
    );
}

#[test]
fn startup_preroll_prefetch_uses_session_deadline_instead_of_steady_state_budget() {
    let future_deadline = Instant::now() + Duration::from_millis(500);
    assert_eq!(
        media_preview_cancel_reason_at_logical_observation(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
            Some(future_deadline),
        ),
        None,
    );
    assert_eq!(
        media_preview_cancel_reason_at_logical_observation(
            Some(
                mondrian_playback::FrameExecutionCancellation::DeadlineExpired {
                    age: Duration::from_millis(1),
                }
            ),
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::ZERO,
            Some(future_deadline),
        ),
        Some(MediaPreviewCancelReason::PrefetchDeadline),
    );
}

#[test]
fn media_preview_decode_cancellation_preempts_prefetch_for_current_work() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(
                mondrian_playback::FrameExecutionCancellation::PrefetchPreemptedByCurrent {
                    request_age: Duration::ZERO,
                },
            ),
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::ZERO,
            false,
        ),
        Some(MediaPreviewCancelReason::PrefetchPreemptedByCurrent),
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US - 1),
            false,
        ),
        None,
    );
}

#[test]
fn media_preview_decode_cancellation_preempts_still_for_realtime_current_work() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(
                mondrian_playback::FrameExecutionCancellation::StillPreemptedByRealtimeCurrent {
                    request_age: Duration::ZERO,
                },
            ),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Duration::ZERO,
            false,
        ),
        Some(MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent),
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
            false,
        ),
        None,
    );
}

#[test]
fn media_preview_decode_cancellation_stops_stale_work() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(mondrian_playback::FrameExecutionCancellation::Superseded {
                age: Some(Duration::ZERO),
            }),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            Duration::ZERO,
            false,
        ),
        Some(MediaPreviewCancelReason::Obsolete),
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(mondrian_playback::FrameExecutionCancellation::Superseded {
                age: Some(Duration::ZERO),
            }),
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::ZERO,
            false,
        ),
        Some(MediaPreviewCancelReason::Obsolete),
    );
}

#[test]
fn media_preview_decode_cancellation_drops_late_playback_current_work() {
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(
                mondrian_playback::FrameExecutionCancellation::DeadlineExpired {
                    age: Duration::ZERO,
                },
            ),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::ZERO,
            true,
        ),
        Some(MediaPreviewCancelReason::PlaybackDeadline),
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            None,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::ZERO,
            true,
        ),
        None,
    );
    assert_eq!(
        media_preview_cancel_reason_for_test_observation(
            Some(
                mondrian_playback::FrameExecutionCancellation::DeadlineExpired {
                    age: Duration::ZERO,
                },
            ),
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            Duration::ZERO,
            true,
        ),
        Some(MediaPreviewCancelReason::Unknown),
    );
}

#[test]
fn preview_representation_quality_selects_a_reduced_decode_identity() {
    let (mut state, _, root) = state_with_invalid_video_asset();
    state.play().expect("play");
    let service = WindowPreviewAdapter::new_without_workers_for_test();
    let full_key = media_preview_key_for_simple_sequence_frame_at_scale(
        &service,
        &state,
        1,
        mondrian_playback::PreviewResolutionScale::Full,
    );
    assert_eq!(
        full_key.decode.representation(),
        mondrian_media::PreviewDecodeRepresentation::NativeCpu
    );
    assert_eq!(
        full_key.residency_resolution(),
        Resolution { width: 3840, height: 2160 }
    );

    let reduced_key = media_preview_key_for_simple_sequence_frame_at_scale(
        &service,
        &state,
        1,
        mondrian_playback::PreviewResolutionScale::Half,
    );
    assert_eq!(
        reduced_key.decode.representation(),
        mondrian_media::PreviewDecodeRepresentation::Reduced {
            divisor: std::num::NonZeroU32::new(2).expect("divisor"),
        }
    );
    assert_eq!(
        reduced_key.residency_resolution(),
        Resolution { width: 1920, height: 1080 },
        "residency must be charged at the reduced representation raster"
    );
    assert_ne!(
        full_key, reduced_key,
        "a representation-quality switch rotates the decode-policy identity; an output-extent change never does"
    );

    let full_again = media_preview_key_for_simple_sequence_frame_at_scale(
        &service,
        &state,
        1,
        mondrian_playback::PreviewResolutionScale::Full,
    );
    assert_eq!(
        full_again, full_key,
        "returning to Full must restore the exact original decode identity and cache hit"
    );

    service.shutdown();
    drop(state);
    std::fs::remove_dir_all(root).expect("remove preview test root");
}
