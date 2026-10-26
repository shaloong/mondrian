use super::*;
use mondrian_core::ColorFrameAlpha;
use mondrian_effects::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct SourceFacts {
    calls: AtomicUsize,
    created: AtomicUsize,
    dropped: AtomicUsize,
    cancel: std::sync::Mutex<Option<ExecutionCancellationToken>>,
}

#[derive(Debug)]
struct HistoryFactory(Arc<SourceFacts>);

impl EffectFloatProcessorFactory for HistoryFactory {
    fn requires_input_service(&self) -> bool {
        true
    }
    fn alpha_contract(&self) -> EffectAlphaContract {
        EffectAlphaContract::IDENTITY
    }
    fn feedback_budget_bytes(&self) -> usize {
        0
    }
    fn storage(
        &self,
        _: EffectFloatProcessorSizing,
    ) -> mondrian_core::Result<EffectFloatProcessorStorage> {
        EffectFloatProcessorStorage::new(0, 0)
    }
    fn prepare(
        &self,
        _: EffectFloatProcessorPreparation,
        control: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        control.check()?;
        self.0.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(HistoryProcessor(self.0.clone())))
    }
}

#[derive(Debug)]
struct HistoryProcessor(Arc<SourceFacts>);

impl Drop for HistoryProcessor {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl EffectFloatProcessorInstance for HistoryProcessor {
    fn process(
        &mut self,
        frame: &mut EffectFloatFrame,
        params: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
        resources: &mut EffectProcessorResources<'_>,
    ) -> Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(token) = self.0.cancel.lock().unwrap().take() {
            token.cancel();
        }
        let time = TimelineTime::new(params["n"].as_i64().unwrap(), params["d"].as_i64().unwrap())
            .unwrap()
            .checked_sub(TimelineTime::new(1, 1).unwrap())
            .unwrap();
        let (input, origin) = resources
            .resolve_input_f32(
                EffectProcessorInputRequest {
                    time,
                    frame_extent: frame.extent(),
                    format: frame.format().with_alpha(ColorFrameAlpha::StraightCoverage),
                    sampling: frame.sampling(),
                    deadline: Instant::now() + Duration::from_secs(5),
                },
                0,
                control,
            )
            .map_err(|error| {
                EffectProcessorInvocationError::InputFailed(Box::new(
                    EffectExecutionError::TemporalInput { source: Box::new(error) },
                ))
            })?;
        assert_eq!(origin, [0, 0]);
        assert!(control.resolves_processor_input());
        *frame = EffectFloatFrame::from_pixels(
            frame.extent(),
            frame.format(),
            frame.sampling(),
            input.into_tile().try_into_pixels().unwrap(),
        )
        .unwrap();
        Ok(None)
    }
}

#[test]
fn export_processor_source_real_media_history_uses_original_job_without_replaying_dsp() {
    render_history(false, SourceScenario::Pixels);
}

#[test]
fn export_processor_source_preplanned_temporal_history_keeps_original_job() {
    render_history(false, SourceScenario::Temporal);
}

#[test]
fn export_processor_source_preplanned_temporal_cancellation_releases_original_job() {
    render_history(false, SourceScenario::TemporalCancel);
}

#[test]
#[ignore = "requires actual wgpu GPU suffix execution"]
fn export_processor_source_mixed_real_media_history_uses_original_job_and_gpu_suffix() {
    render_history(true, SourceScenario::Pixels);
}

#[test]
fn export_processor_source_caller_cancellation_releases_original_job() {
    render_history(false, SourceScenario::Cancel);
}

#[test]
fn export_processor_source_budget_refusal_releases_original_job() {
    render_history(false, SourceScenario::Budget);
}

#[test]
#[ignore = "requires actual FFmpeg PNG artifact publication"]
fn export_processor_source_production_queue_validates_history_png_artifact() {
    render_history(false, SourceScenario::Artifact);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceScenario {
    Pixels,
    Temporal,
    TemporalCancel,
    Cancel,
    Budget,
    Artifact,
}

fn render_history(mixed: bool, scenario: SourceScenario) {
    mondrian_core::ensure_mondrian_default_ocio_loaded().expect("production OCIO config");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.y4m");
    let mut y4m = b"YUV4MPEG2 W2 H2 F1:1 Ip A1:1 C444\n".to_vec();
    for [y, u, v] in [[76u8, 84, 255], [29, 255, 107]] {
        y4m.extend_from_slice(b"FRAME\n");
        y4m.extend([y; 4]);
        y4m.extend([u; 4]);
        y4m.extend([v; 4]);
    }
    std::fs::write(&path, y4m).unwrap();
    let mut dependency = test_media_dependency(
        path.clone(),
        None,
        AssetMediaInterpretation {
            range: mondrian_core::timeline_data::MediaRangeInterpretation::Override {
                range: mondrian_core::timeline_data::MediaSignalRange::Full,
            },
            ..Default::default()
        },
        None,
    );
    let stream = mondrian_media::probe_media_info(&path).unwrap().video_streams[0].clone();
    dependency.video_stream_index = Some(stream.index);
    dependency.picture = Some(stream.picture);
    dependency.color_diagnostic = Some(mondrian_media::VideoColorDiagnostic::from_stream(&stream));
    dependency.source_video_stream = Some(stream);
    dependency.source_resolution = Some(Resolution { width: 2, height: 2 });
    dependency.picture_source_extent = Some(mondrian_timeline::PictureSourceExtent::TimelineRange(
        mondrian_core::TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(2, 1).unwrap())
            .unwrap(),
    ));
    let facts = Arc::new(SourceFacts::default());
    let reader = EffectType::External(format!("test.export.source-reader.{}", AssetId::new()));
    register_effect_definition(
        EffectDefinition::new(
            reader.key(),
            "History source",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_custom_float_processor_factory(
            Arc::new(|_, context| {
                Ok(Some(serde_json::json!({
                    "n": context.time.numerator(), "d": context.time.denominator(),
                })))
            }),
            None,
            EffectCachePolicy::Deterministic,
            Arc::new(HistoryFactory(facts.clone())),
        ),
    )
    .unwrap();
    let gain = EffectType::External(format!("test.export.source-gain.{}", AssetId::new()));
    register_effect_definition(
        EffectDefinition::new(
            gain.key(),
            "Upstream gain",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_custom_float_render_backend(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            EffectCachePolicy::Deterministic,
            Arc::new(|frame, _, _| {
                for pixel in frame.pixels_mut() {
                    for channel in &mut pixel[..3] {
                        *channel *= 2.0;
                    }
                }
                Ok(())
            }),
        ),
    )
    .unwrap();
    let asset = AssetId::new();
    let mut sequence = Sequence::new("Export history source");
    sequence.settings.resolution = Resolution { width: 2, height: 2 };
    sequence.settings.frame_rate = mondrian_core::Rational::new(24, 1);
    sequence.settings.delivery.bit_depth = DeliveryBitDepth::Eight;
    // The untagged Y4M deliberately selects the existing authored missing policy,
    // which also authorizes BT.709 matrix fallback; range remains independent.
    sequence.settings.color.input.missing_metadata_policy =
        MissingColorMetadataPolicy::AssumeRec709;
    let mut clip = Clip::new(asset, TimelineTime::ZERO, TimelineTime::new(2, 1).unwrap()).unwrap();
    let gain_node = EffectNode::new(gain);
    clip.add_effect_node(gain_node.clone());
    let temporal = matches!(
        scenario,
        SourceScenario::Temporal | SourceScenario::TemporalCancel
    )
    .then(|| {
        let kind = EffectType::External(format!("test.export.source-temporal.{}", AssetId::new()));
        let offset = TimelineTime::new(1, 4).unwrap();
        register_effect_definition(
            EffectDefinition::new(
                kind.key(),
                "Finite source time",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                temporal_input: EffectTemporalInputExtent {
                    past: EffectTemporalSpan::None,
                    future: EffectTemporalSpan::Finite(offset),
                },
                roi_propagation: EffectRoiPropagation::PixelLocal,
                ..EffectExecutionContract::IDENTITY
            })
            .with_graph_builder(Arc::new(move |_, _, graph| {
                graph.append_unary(EffectRenderOp::TemporalFrameBlend {
                    sample_offset: offset,
                    mix: 0.25,
                });
                Ok(())
            })),
        )
        .unwrap();
        EffectNode::new(kind)
    });
    if let Some(temporal) = &temporal {
        clip.add_effect_node(temporal.clone());
    }
    let tail = mixed.then(|| {
        let kind = EffectType::External(format!("test.export.source-gpu-tail.{}", AssetId::new()));
        register_effect_definition(
            EffectDefinition::new(
                kind.key(),
                "GPU source tail",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::GPU_F32,
                ..EffectExecutionContract::IDENTITY
            })
            .with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: test_working_color_space(),
                });
                Ok(())
            })),
        )
        .unwrap();
        EffectNode::new(kind)
    });
    if let Some(tail) = &tail {
        clip.add_effect_node(tail.clone());
    }
    sequence.video_tracks[0].add_clip(clip).unwrap();
    let mut timeline = TimelineExportSnapshot {
        sequence,
        sequences: Vec::new(),
        media: HashMap::from([(asset, dependency)]),
        color_environment: Default::default(),
        prepared_execution: None,
        range: TimelineExportRange::EntireSequence,
    };
    let render = |timeline: &TimelineExportSnapshot,
                  frame,
                  small_budget: bool,
                  cancellation: &ExecutionCancellationToken| {
        let mut policy = service::ExportExecutionResourcePolicy::default();
        if small_budget {
            policy.effect_working_bytes = 256;
        }
        let mut session = ExportVisualRenderSession::for_timeline(701, policy, timeline)?;
        let color = timeline
            .sequence
            .settings
            .root_program_color_context(&timeline.color_environment)
            .map_err(|error| error.to_string())?;
        let delivery = ExportDeliveryPixelContract::unmodified(
            ExportFrameContract::from_bit_depth(timeline.sequence.settings.delivery.bit_depth),
        );
        let mut canvas = Vec::new();
        let result = render_timeline_frame_into_with_session_cancellable(
            timeline,
            frame,
            2,
            2,
            ExportAlphaMode::Preserve,
            color,
            delivery,
            &mut canvas,
            None,
            None,
            None,
            None,
            &mut session,
            cancellation,
        );
        let diagnostics = session.visual_diagnostics();
        session.release_decode_sessions()?;
        result.map(|()| (canvas, diagnostics))
    };
    let expected = render(&timeline, 0, false, &ExecutionCancellationToken::new())
        .expect("actual baseline historical pixels")
        .0;
    let current = render(&timeline, 24, false, &ExecutionCancellationToken::new())
        .expect("actual current pixels")
        .0;
    assert_ne!(
        expected, current,
        "history and current must be distinguishable"
    );
    let baseline_timeline = timeline.clone();
    let mut history_clip =
        Clip::new(asset, TimelineTime::ZERO, TimelineTime::new(2, 1).unwrap()).unwrap();
    history_clip.add_effect_node(gain_node);
    if let Some(temporal) = temporal {
        history_clip.add_effect_node(temporal);
    }
    history_clip.add_effect_node(EffectNode::new(reader));
    if let Some(tail) = tail {
        history_clip.add_effect_node(tail);
    }
    timeline.sequence.video_tracks[0].clips[0] = history_clip;
    let cancellation = ExecutionCancellationToken::new();
    if matches!(
        scenario,
        SourceScenario::Cancel | SourceScenario::TemporalCancel
    ) {
        *facts.cancel.lock().unwrap() = Some(cancellation.clone());
    }
    let result = (scenario != SourceScenario::Artifact).then(|| {
        render(
            &timeline,
            24,
            scenario == SourceScenario::Budget,
            &cancellation,
        )
    });
    match scenario {
        SourceScenario::Pixels | SourceScenario::Temporal => {
            let (actual, diagnostics) =
                result.unwrap().expect("actual Export job must resolve valid historical source");
            assert_eq!(
                actual, expected,
                "source must include upstream processing at historical time"
            );
            if mixed {
                assert_eq!(diagnostics.heterogeneous_frames_started, 1);
                assert_eq!(diagnostics.heterogeneous_frames_completed, 1);
                assert_eq!(diagnostics.heterogeneous_terminal_failures, 0);
                assert!(diagnostics.heterogeneous_upload_bytes > 0);
                assert!(diagnostics.heterogeneous_readback_bytes > 0);
                assert!(diagnostics.last_heterogeneous_completion.is_some());
            }
        }
        SourceScenario::Cancel | SourceScenario::TemporalCancel => {
            let error =
                result.unwrap().expect_err("original caller cancellation cannot publish pixels");
            assert!(cancellation.is_canceled());
            assert!(error.to_ascii_lowercase().contains("cancel"), "{error}");
        }
        SourceScenario::Budget => {
            let error = result.unwrap().expect_err("late source peak must fit the original grant");
            assert!(error.contains("WorkingSetBudgetExceeded"), "{error}");
            assert!(
                !cancellation.is_canceled(),
                "budget refusal cannot cancel the parent"
            );
        }
        SourceScenario::Artifact => {
            assert!(
                ffmpeg_is_available_for_test(),
                "real artifact qualification requires FFmpeg"
            );
            let queue = RenderQueue::new();
            let publish = |timeline: &TimelineExportSnapshot, frame, label: &str| {
                let output = directory.path().join(format!("{label}.pngseq"));
                let mut config = dummy_config(&output.to_string_lossy());
                *config.timeline = timeline.clone();
                config.timeline.sequence.settings.resolution = Resolution { width: 16, height: 16 };
                config.timeline.range = TimelineExportRange::WorkArea {
                    start_frame: frame,
                    end_frame_exclusive: frame + 1,
                };
                config.preset = crate::preset::ExportPreset::png_sequence();
                config.preset.resolution =
                    Some(crate::preset::Resolution { width: 16, height: 16 });
                refresh_test_execution_snapshot(&mut config.timeline, false);
                let id =
                    queue.enqueue(RenderJob::new(config)).expect("admit actual source Export job");
                assert!(
                    wait_until(30_000, || queue
                        .list_jobs()
                        .iter()
                        .any(|job| job.id == id && job.status.is_terminal())),
                    "actual source job did not settle"
                );
                let job = queue.list_jobs().into_iter().find(|job| job.id == id).unwrap();
                assert!(matches!(job.status, JobStatus::Completed), "{job:?}");
                let manifest: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(output.join(crate::image_sequence::MANIFEST_FILE_NAME)).unwrap(),
                )
                .unwrap();
                assert_eq!(manifest["frame_count"], 1);
                let image = image::open(output.join(crate::image_sequence::frame_file_name(
                    0,
                    crate::preset::ImageSequenceFormat::Png8,
                )))
                .expect("independently decode published PNG")
                .to_rgba8();
                assert_eq!(image.dimensions(), (16, 16));
                image.into_raw()
            };
            let baseline = publish(&baseline_timeline, 0, "baseline");
            let current = publish(&baseline_timeline, 24, "current");
            assert_ne!(
                baseline, current,
                "published controls must distinguish history"
            );
            let actual = publish(&timeline, 24, "history");
            assert_eq!(
                actual, baseline,
                "complete job must publish historical upstream pixels"
            );
        }
    }
    assert_eq!(
        facts.calls.load(Ordering::SeqCst),
        1,
        "do not replay waiting DSP"
    );
    assert!(facts.created.load(Ordering::SeqCst) > 0);
    assert_eq!(
        facts.created.load(Ordering::SeqCst),
        facts.dropped.load(Ordering::SeqCst)
    );
}
