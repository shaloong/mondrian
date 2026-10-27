//! Published temporal Filter through the ordinary Timeline, worker, and Export paths.

use super::*;
use crate::app::product_action::*;
use mondrian_core::automation::{
    AnimationChannelValue, KeyframeInterpolation, ParameterValueWrite,
};
use mondrian_core::{FramePosition, PropertyValue, Rational, TimelineTime};

fn acquire_working(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    frame: i64,
) -> (
    mondrian_renderer::CpuColorFrame,
    mondrian_effects::EffectProcessorFeedbackBatch,
) {
    state.seek(frame).unwrap();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let target = sequence.settings.resolution;
    let color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: snapshot.authoring().unwrap().author_generation(),
        frame,
        width: target.width,
        height: target.height,
        runtime_scale: snapshot.transport().runtime_scale(),
        raster_requirement: PreviewRasterRequirement::Viewer,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let evaluation = loop {
        runtime.poll_playback_work(None, snapshot.transport().intent());
        match runtime.acquire_frame_evaluation(
            &snapshot,
            state,
            sequence,
            frame,
            target.width,
            target.height,
            color.clone(),
            key,
        ) {
            FrameResolutionOutcome::Ready(ready) => break ready,
            FrameResolutionOutcome::Pending(_) => {}
            FrameResolutionOutcome::Unavailable(reason) => {
                panic!("actual temporal Preview frame {frame}: {}", reason.detail())
            }
            FrameResolutionOutcome::Empty => panic!("actual media Timeline is not empty"),
        }
        assert!(Instant::now() < deadline, "temporal Preview did not settle");
        std::thread::sleep(Duration::from_millis(1));
    };
    // Ready means the immutable Timeline plan is ready. Ordinary current-frame
    // Custom processors still run in the existing CPU composition task, which
    // owns the late-source client. The synchronous no-input test helper cannot
    // execute a temporal native Filter.
    assert_eq!(evaluation.reuse_policy, EvaluationReusePolicy::Transient);
    let resolved = ResolvedPlanView {
        sampling: evaluation.sampling,
        cpu_materialization_active_bytes: evaluation.cpu_materialization_active_bytes,
        elements: Arc::clone(&evaluation.elements),
        cache_key: evaluation.output_key.clone(),
        cpu_cache_key: evaluation.output_key.clone(),
        cache_reusable: false,
        color_context: evaluation.color_context.clone(),
        render_cache_identity: evaluation.render_cache_identity,
    };
    assert!(matches!(
        runtime.schedule_cpu_fallback(
            &snapshot,
            &resolved,
            target.width,
            target.height,
            false,
            crate::app::preview_cpu_fallback_task::PreviewCpuOutputTarget::WorkingGpu,
        ),
        PreviewGpuFrameState::Loading
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut feedback = None;
    loop {
        if let Some(update) = runtime
            .poll_playback_work(None, snapshot.transport().intent())
            .processor_feedback
        {
            feedback = Some(update.feedback);
        }
        if let Some((_, failure)) = runtime.cpu_fallback_failure.borrow().as_ref() {
            panic!("actual CPU Preview frame {frame}: {}", failure.detail);
        }
        if let Some(pending) = runtime.cpu_fallback_in_flight.borrow().as_ref()
            && pending.completed
        {
            let working = runtime
                .frame_store
                .borrow_mut()
                .viewer_working_frame(&pending.attempt.output_key)
                .expect("original CPU task published its admitted working frame");
            return (
                (*working).clone(),
                feedback.expect("CPU result delivered native feedback through the production poll"),
            );
        }
        assert!(
            Instant::now() < deadline,
            "actual CPU Preview did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn export_snapshot(state: &AppState) -> mondrian_export::preset::TimelineExportSnapshot {
    crate::app::exporting::capture_timeline_export_snapshot(
        state,
        state.active_sequence().unwrap().clone(),
        vec![],
        mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 0,
            end_frame_exclusive: 8,
        },
        false,
    )
    .unwrap()
}

fn reference_source_samples(
    state: &AppState,
    varying_media: bool,
) -> std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame> {
    let ticks = [6, 7, 8, 9, 14, 15, 16, 17];
    let target = state.active_sequence().unwrap().settings.resolution;
    if !varying_media {
        let base = mondrian_export::queue::export_visual_frame_validation(
            &export_snapshot(state),
            2,
            target,
        )
        .unwrap()
        .working_frame;
        assert_eq!(base.rgba_f32().data.len(), 256);
        assert!(base.rgba_f32().data.iter().all(|p| p[0] > 0.01 && p[3] == 1.0));
        return ticks.into_iter().map(|tick| (tick, base.clone())).collect();
    }
    // This source-only reference uses a finer evaluation grid to express exact
    // quarter-frame author times. The real 24fps program is unchanged. Original
    // media metadata/Source Sample Target/decoder and color conversion still
    // decide the physical samples; the test introduces no rounding policy.
    let mut reference = state.active_sequence().unwrap().clone();
    reference.settings.frame_rate = Rational::new(96, 1);
    let snapshot = crate::app::exporting::capture_timeline_export_snapshot(
        state,
        reference,
        vec![],
        mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 0,
            end_frame_exclusive: 32,
        },
        false,
    )
    .unwrap();
    let sources: std::collections::BTreeMap<_, _> = ticks
        .into_iter()
        .map(|tick| {
            let frame =
                mondrian_export::queue::export_visual_frame_validation(&snapshot, tick, target)
                    .unwrap()
                    .working_frame;
            assert_eq!(frame.rgba_f32().data.len(), 256);
            assert!(frame
                .rgba_f32()
                .data
                .iter()
                .all(|p| p[3] == 1.0 && p.iter().all(|v| v.is_finite())));
            (tick, frame)
        })
        .collect();
    // Millisecond Matroska PTS: frame 4 starts at .167, after 16/96.
    // Keep the preceding picture until that actual boundary; 17/96 crosses it.
    for (left, right) in [(6, 7), (8, 9), (14, 15), (15, 16)] {
        assert!(
            sources[&left] == sources[&right],
            "same decoded presentation interval {left}/96 and {right}/96"
        );
    }
    assert!(
        sources[&16] != sources[&17],
        "fractional request must cross the actual .167s presentation boundary"
    );
    assert!(
        sources[&6] != sources[&8],
        "varying-media control must distinguish source frames"
    );
    assert!(
        sources[&14] != sources[&17],
        "later source frames must also differ"
    );
    sources
}

fn with_preview_runtime<R>(run: impl FnOnce(&PreviewProductionRuntime<()>) -> R) -> R {
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&runtime)));
    runtime.shutdown_and_wait();
    result.unwrap_or_else(|error| std::panic::resume_unwind(error))
}

fn verify_time_blur_frame(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    sources: &std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame>,
    frame: i64,
    offsets: &[i64],
) -> mondrian_effects::EffectProcessorFeedbackBatch {
    let (output, feedback) = acquire_working(runtime, state, frame);
    assert_eq!(output.rgba_f32().data.len(), 256);
    for (pixel_index, pixel) in output.rgba_f32().data.iter().enumerate() {
        for channel in 0..3 {
            let expected = offsets
                .iter()
                .map(|offset| {
                    let tick = frame * 4 + offset;
                    sources[&tick].rgba_f32().data[pixel_index][channel]
                        * 2.0_f32.powf(tick as f32 / 16.0)
                })
                .sum::<f32>()
                / offsets.len() as f32;
            assert!((pixel[channel] - expected).abs() < 2e-5,
                "frame {frame} pixel {pixel_index} channel {channel}: {pixel:?}, expected={expected}");
        }
        assert_eq!(pixel[3], 1.0);
    }
    assert_eq!(feedback.len(), 1);
    assert_ne!(
        feedback.iter().next().unwrap().feedback.snapshot.native_owner(),
        0
    );
    let exported = mondrian_export::queue::export_visual_frame_validation(
        &export_snapshot(state),
        frame,
        state.active_sequence().unwrap().settings.resolution,
    )
    .unwrap();
    assert_eq!(
        output, exported.working_frame,
        "real Preview/Export working-frame parity"
    );
    feedback
}

fn install_time_blur_catalog(
    state: &mut AppState,
    binary: &std::path::Path,
    kind: &mondrian_effects::EffectType,
    preferences_path: &std::path::Path,
) {
    use crate::app::openfx_catalog::OpenFxBundleSelection;
    use crate::app_ui::preferences_store::{persist_app_ui_preferences_to, AppUiPreferences};
    let bundle = binary.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    assert!(bundle.file_name().unwrap().to_string_lossy().ends_with(".ofx.bundle"));
    let selection = OpenFxBundleSelection {
        path: bundle.clone(),
        context: crate::openfx_host::OpenFxContext::Filter,
    };
    let generation = state.project_author_generation();
    state
        .dispatch_action(
            ProductAction::VisualEffect(VisualEffectProductAction::InstallOpenFxBundle(
                VisualEffectInstallOpenFxBundlePayload { path: bundle, context: selection.context },
            ))
            .into_external_action(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let completed = loop {
        let changed = state.poll_openfx_catalog();
        let completed = state.take_completed_openfx_installs();
        if !completed.is_empty() {
            break completed;
        }
        assert!(
            !(changed && state.status_hint.as_ref().is_some_and(|(_, failed)| *failed)),
            "catalog installation failed: {:?}",
            state.status_hint
        );
        assert!(
            Instant::now() < deadline,
            "catalog installation did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(completed, vec![selection.clone()]);
    assert_eq!(state.project_author_generation(), generation);
    assert!(
        mondrian_effects::effect_library_types().contains(kind),
        "catalog must publish TimeBlur"
    );
    let mut preferences = AppUiPreferences::default();
    preferences.record_openfx_bundle(selection);
    persist_app_ui_preferences_to(preferences_path, &preferences).unwrap();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeBlurNleCase {
    Uniform,
    Varying,
    AuthorHistory,
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published OpenFX-Misc TimeBlur 1.0, FFmpeg and OCIO"]
fn time_blur_real_media_preview_export_evaluate_upstream_animation_at_requested_times() {
    qualify_time_blur_nle(TimeBlurNleCase::Uniform);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published OpenFX-Misc TimeBlur 1.0, FFmpeg and OCIO"]
fn time_blur_varying_media_preserves_fractional_source_selection_and_animation() {
    qualify_time_blur_nle(TimeBlurNleCase::Varying);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published TimeBlur, catalog restore, FFmpeg and OCIO"]
fn time_blur_public_parameter_history_and_project_reopen_preserve_temporal_output() {
    qualify_time_blur_nle(TimeBlurNleCase::AuthorHistory);
}

fn qualify_time_blur_nle(case: TimeBlurNleCase) {
    let varying_media = case != TimeBlurNleCase::Uniform;
    let history = case == TimeBlurNleCase::AuthorHistory;
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    ensure_test_ocio_loaded();
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").unwrap());
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let description =
        crate::openfx_host::describe_openfx_filter(&helper, &inspection, "net.sf.openfx.TimeBlur")
            .unwrap();
    let kind = if history {
        use sha2::{Digest, Sha256};
        assert_eq!(
            inspection
                .plugins
                .iter()
                .filter(|p| p.identifier == "net.sf.openfx.TimeBlur"
                    && p.version_major == 1
                    && p.version_minor == 0)
                .count(),
            1
        );
        let identity = format!(
            "{}\0{}.{}\0{}",
            "net.sf.openfx.TimeBlur", 1, 0, inspection.binary_sha256
        );
        mondrian_effects::EffectType::External(format!(
            "openfx.{:x}",
            Sha256::digest(identity.as_bytes())
        ))
    } else {
        crate::openfx_effect::register_selected_openfx_filter(
            &helper,
            &inspection,
            "net.sf.openfx.TimeBlur",
        )
        .unwrap()
    };
    let directory = tempfile::tempdir().unwrap();
    // The uniform control isolates animation. The varying control supplies
    // distinct Y/U/V planes per frame and independently samples source time.
    let raw = directory.path().join("source.y4m");
    let source = directory.path().join("source.mkv");
    let mut media = b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C444 XCOLORRANGE=LIMITED\n".to_vec();
    for frame in 0..(if varying_media { 12_u8 } else { 8_u8 }) {
        media.extend_from_slice(b"FRAME\n");
        media.extend_from_slice(&[if varying_media { 48 + frame * 12 } else { 80 }; 256]);
        media.extend_from_slice(&[if varying_media { 112 + frame * 2 } else { 128 }; 256]);
        media.extend_from_slice(&[if varying_media { 144 - frame * 2 } else { 128 }; 256]);
    }
    std::fs::write(&raw, media).unwrap();
    // Y4M cannot carry all required color metadata. Encode losslessly rather
    // than inventing probe metadata or relaxing the production decode contract.
    let encoded = std::process::Command::new("ffmpeg.exe")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(&raw)
        .args([
            "-c:v",
            "ffv1",
            "-level",
            "3",
            "-color_range",
            "tv",
            "-colorspace",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_primaries",
            "bt709",
            "-an",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    if varying_media {
        let timing = std::process::Command::new("ffprobe.exe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_frames",
                "-show_entries",
                "stream=time_base:frame=best_effort_timestamp",
                "-of",
                "json",
            ])
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            timing.status.success(),
            "{}",
            String::from_utf8_lossy(&timing.stderr)
        );
        let timing: serde_json::Value = serde_json::from_slice(&timing.stdout).unwrap();
        assert_eq!(timing["streams"][0]["time_base"], "1/1000");
        let pts: Vec<_> = timing["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| frame["best_effort_timestamp"].as_i64().unwrap())
            .collect();
        assert_eq!(pts.len(), 12);
        assert_eq!(&pts[..5], &[0, 42, 83, 125, 167], "controlled fixture PTS");
    }
    let info = mondrian_media::probe_media_info(&source).unwrap();
    assert_eq!(info.primary_video().unwrap().frame_rate, Rational::FPS_24);
    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state
        .create_new_project_at(
            directory.path().join("temporal.mdp"),
            "Temporal media",
            16,
            16,
            Rational::FPS_24,
        )
        .unwrap();
    let asset = commit_preview_test_media(state.asset_library().unwrap(), source, info);
    let sequence = state.active_sequence().unwrap();
    state
        .dispatch_action(
            ProductAction::Timeline(TimelineProductAction::PlaceAsset(
                TimelineDropAssetPayload {
                    asset_id: asset,
                    target_track_id: sequence.video_tracks[0].id,
                    position: FramePosition::new(0, sequence.time_base()),
                },
            ))
            .into_external_action(),
        )
        .unwrap();
    let clip_id = state.active_sequence().unwrap().video_tracks[0].clips[0].id;
    let preferences_path = directory.path().join("preferences.json");
    if history {
        assert_eq!(
            crate::app::packaged_worker::discover_openfx_worker().unwrap(),
            helper
        );
        install_time_blur_catalog(&mut state, &binary, &kind, &preferences_path);
    }
    let sources = reference_source_samples(&state, varying_media);
    let saved = with_preview_runtime(|runtime| {
        state
            .add_effect_to_clip(clip_id, mondrian_effects::EffectType::BasicCorrection)
            .unwrap();
        let correction = &state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects[0];
        let correction_id = correction.id;
        let exposure_id =
            mondrian_effects::EffectType::BasicCorrection.parameter_id("exposure").unwrap();
        let exposure = correction
            .properties
            .iter()
            .find(|(_, p)| p.descriptor.parameter_id() == &exposure_id)
            .expect("canonical BasicCorrection exposure definition")
            .1
            .address();
        for (frame, value) in [(0, 0.0), (7, 1.75)] {
            state
                .dispatch_action(
                    ProductAction::VisualEffect(VisualEffectProductAction::EditNumericCurve(
                        Box::new(VisualEffectEditNumericCurvePayload {
                            clip_id,
                            effect_id: correction_id,
                            parameter: exposure.clone(),
                            edit: ClipCurveEditPayload::Upsert {
                                keyframe_id: None,
                                point: ClipCurveAuthorPoint {
                                    time: TimelineTime::new(frame, 24).unwrap(),
                                    value: AnimationChannelValue::Number(value),
                                },
                            },
                        }),
                    ))
                    .into_external_action(),
                )
                .unwrap();
        }
        let exposure_curve = state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects
            [0]
        .properties
        .property_by_address(&exposure)
        .unwrap()
        .1
        .channel(0)
        .unwrap();
        assert_eq!(exposure_curve.keyframes().len(), 2);
        assert!(exposure_curve
            .keyframes()
            .iter()
            .all(|key| key.interp_in == KeyframeInterpolation::Linear
                && key.interp_out == KeyframeInterpolation::Linear));
        state.add_effect_to_clip(clip_id, kind.clone()).unwrap();
        let effect =
            state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects[1].clone();
        let centered = &description
            .parameters
            .iter()
            .find(|p| p.name == "shutterOffset")
            .unwrap()
            .choices[0]
            .key;
        for (name, value) in [
            ("division", PropertyValue::Int(4)),
            ("shutter", PropertyValue::Double(1.0)),
            ("shutterOffset", PropertyValue::Enum(centered.clone())),
        ] {
            let label = &description.parameters.iter().find(|p| p.name == name).unwrap().label;
            let property = effect
                .properties
                .iter()
                .find(|(_, p)| p.descriptor.display_name == *label)
                .unwrap()
                .1;
            let address = property.address();
            if property.static_value() == &value {
                continue;
            }
            state
                .dispatch_action(
                    ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(
                        Box::new(VisualEffectSetParameterValuePayload {
                            clip_id,
                            effect_id: effect.id,
                            write: ParameterValueWrite::Value { parameter: address, value },
                        }),
                    ))
                    .into_external_action(),
                )
                .unwrap();
        }
        let author_before =
            serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap();
        let mut previous = None;
        for frame in [2, 4, 2] {
            let feedback =
                verify_time_blur_frame(runtime, &mut state, &sources, frame, &[-2, -1, 0, 1]);
            let receipt = feedback.iter().next().unwrap();
            let identity = (
                receipt.feedback.owner,
                receipt.feedback.origin,
                receipt.feedback.snapshot.native_owner(),
            );
            assert_ne!(identity.2, 0);
            if let Some((owner, invocation)) = previous {
                if owner == identity {
                    assert!(receipt.feedback.snapshot.invocation() > invocation);
                }
            }
            previous = Some((identity, receipt.feedback.snapshot.invocation()));
            assert_eq!(
                serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                    .unwrap(),
                author_before
            );
        }
        if !history {
            return None;
        }
        let shutter_label =
            &description.parameters.iter().find(|p| p.name == "shutter").unwrap().label;
        let shutter = effect
            .properties
            .iter()
            .find(|(_, p)| p.descriptor.display_name == *shutter_label)
            .unwrap()
            .1
            .address();
        state
            .dispatch_action(
                ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(
                    Box::new(VisualEffectSetParameterValuePayload {
                        clip_id,
                        effect_id: effect.id,
                        write: ParameterValueWrite::Value {
                            parameter: shutter,
                            value: PropertyValue::Double(0.0),
                        },
                    }),
                ))
                .into_external_action(),
            )
            .unwrap();
        verify_time_blur_frame(runtime, &mut state, &sources, 4, &[0]);
        assert!(state.undo_timeline().unwrap());
        verify_time_blur_frame(runtime, &mut state, &sources, 4, &[-2, -1, 0, 1]);
        assert!(state.redo_timeline().unwrap());
        verify_time_blur_frame(runtime, &mut state, &sources, 4, &[0]);
        assert!(state.undo_timeline().unwrap());
        let feedback = verify_time_blur_frame(runtime, &mut state, &sources, 4, &[-2, -1, 0, 1]);
        let owner = feedback.iter().next().unwrap().feedback.owner;
        let authored =
            serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap();
        assert_eq!(
            authored, author_before,
            "undo returns to exact authored Clip state"
        );
        let saved_path = directory.path().join("saved.mdp");
        state.save_project_file_as(saved_path.clone()).unwrap();
        Some((saved_path, authored, owner))
    });
    if let Some((saved_path, authored, old_owner)) = saved {
        state.close_project().unwrap();
        drop(state);
        let installed = mondrian_effects::effect_definition(&kind).unwrap();
        let mut reopened = AppState::new();
        pin_standard_execution_resources(&mut reopened);
        let preferences =
            crate::app_ui::preferences_store::load_app_ui_preferences_from(&preferences_path);
        assert_eq!(preferences.installed_openfx_bundles.len(), 1);
        let generation = reopened.project_author_generation();
        reopened.schedule_openfx_catalog_restore(preferences.installed_openfx_bundles);
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            reopened.poll_openfx_catalog();
            if let Some(current) = mondrian_effects::effect_definition(&kind)
                && !Arc::ptr_eq(&current, &installed)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "catalog restore did not publish a fresh definition"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(reopened.take_completed_openfx_installs().is_empty());
        assert_eq!(reopened.project_author_generation(), generation);
        reopened.open_project_file(saved_path).unwrap();
        assert_eq!(
            serde_json::to_value(reopened.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap(),
            authored
        );
        with_preview_runtime(|runtime| {
            for frame in [2, 4, 2] {
                let feedback = verify_time_blur_frame(
                    runtime,
                    &mut reopened,
                    &sources,
                    frame,
                    &[-2, -1, 0, 1],
                );
                assert_ne!(
                    feedback.iter().next().unwrap().feedback.owner,
                    old_owner,
                    "reopened processing must use a fresh bank owner"
                );
                assert_eq!(
                    serde_json::to_value(
                        reopened.active_sequence().unwrap().find_clip(clip_id).unwrap()
                    )
                    .unwrap(),
                    authored
                );
            }
        });
        reopened.close_project().unwrap();
    }
}
