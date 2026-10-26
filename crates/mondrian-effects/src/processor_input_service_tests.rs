use super::*;
use crate::*;
use mondrian_core::{
    effect_data::EffectType, ExecutionCancellationToken, TimelineTime, WorkingColorSpace,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

fn time(n: i64, d: i64) -> TimelineTime {
    TimelineTime::new(n, d).unwrap()
}
#[derive(Debug, Default)]
struct Facts {
    mode: AtomicUsize,
    epoch: AtomicUsize,
    created: AtomicUsize,
    dropped: AtomicUsize,
    dsp: AtomicUsize,
    calls: Mutex<Vec<(usize, TimelineTime)>>,
    cancellation: ExecutionCancellationToken,
}
struct Factory {
    facts: Arc<Facts>,
    parent: bool,
}
impl std::fmt::Debug for Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BorrowedInputFactory")
    }
}
#[derive(Debug)]
struct Processor {
    facts: Arc<Facts>,
    parent: bool,
}
impl Drop for Processor {
    fn drop(&mut self) {
        self.facts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl EffectFloatProcessorFactory for Factory {
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
        EffectFloatProcessorStorage::new(128, 128)
    }
    fn prepare(
        &self,
        _: EffectFloatProcessorPreparation,
        c: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        c.check()?;
        self.facts.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Processor {
            facts: self.facts.clone(),
            parent: self.parent,
        }))
    }
}
fn input_error(e: EffectTemporalExecutionError) -> EffectProcessorInvocationError {
    let e = match e {
        EffectTemporalExecutionError::CustomProcessor(e) => e,
        other => EffectExecutionError::TemporalInput { source: Box::new(other) },
    };
    EffectProcessorInvocationError::InputFailed(Box::new(e))
}
impl EffectFloatProcessorInstance for Processor {
    fn process(
        &mut self,
        frame: &mut EffectFloatFrame,
        p: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
        resources: &mut EffectProcessorResources<'_>,
    ) -> std::result::Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        assert!(
            control.resolves_processor_input(),
            "nested DSP must see the restored service"
        );
        // A valid processor may consume only its current image. This mode
        // isolates the static frontier grant from additional Source requests.
        if self.facts.mode.load(Ordering::SeqCst) == 7 {
            control.check()?;
            self.facts.dsp.fetch_add(1, Ordering::SeqCst);
            return Ok(None);
        }
        let current = time(p["n"].as_i64().unwrap(), p["d"].as_i64().unwrap());
        let deadline = if self.facts.mode.load(Ordering::SeqCst) == 5 {
            Instant::now()
        } else {
            Instant::now() + Duration::from_secs(30)
        };
        let (output, origin) = resources
            .resolve_input_f32(
                EffectProcessorInputRequest {
                    time: current
                        .checked_add(if self.parent { time(1, 2) } else { time(1, 4) })
                        .unwrap(),
                    frame_extent: frame.extent(),
                    format: frame.format(),
                    sampling: frame.sampling(),
                    deadline,
                },
                7,
                control,
            )
            .map_err(input_error)?;
        assert_eq!(origin, [-2, 3]);
        assert!(control.resolves_processor_input());
        let pixels = output.into_tile().try_into_pixels().unwrap();
        let mut resolved =
            EffectFloatFrame::from_pixels(frame.extent(), frame.format(), frame.sampling(), pixels)
                .unwrap();
        if !self.parent {
            for pixel in resolved.pixels_mut() {
                pixel[0] *= 2.0;
            }
        }
        self.facts.dsp.fetch_add(1, Ordering::SeqCst);
        *frame = resolved;
        Ok(None)
    }
}

fn program(facts: &Arc<Facts>) -> (PreparedEffectProgram, Arc<CompiledEffectGraph>) {
    program_with_gpu_tail(facts, false)
}

fn program_with_gpu_tail(
    facts: &Arc<Facts>,
    gpu_tail: bool,
) -> (PreparedEffectProgram, Arc<CompiledEffectGraph>) {
    let mut effects = Vec::new();
    for parent in [false, true] {
        let kind = EffectType::External(format!(
            "test.scoped-input.{}",
            mondrian_core::EffectId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                kind.key(),
                "Scoped source",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_extension_contract(EffectExtensionContract::new("1.0"))
            .with_custom_float_processor_factory(
                Arc::new(|_, c| {
                    Ok(Some(
                        serde_json::json!({"n":c.time.numerator(),"d":c.time.denominator()}),
                    ))
                }),
                None,
                EffectCachePolicy::Deterministic,
                Arc::new(Factory { facts: facts.clone(), parent }),
            ),
        )
        .unwrap();
        effects.push(EffectNode::new(kind));
    }
    if gpu_tail {
        let mut tail = EffectNode::with_defaults(EffectType::BasicCorrection);
        tail.set_static_value_by_parameter(
            &EffectType::BasicCorrection.parameter_id("exposure").unwrap(),
            mondrian_core::automation::PropertyValue::Float(0.25),
        )
        .unwrap();
        effects.push(tail);
    }
    let program =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    (program, graph)
}
struct Source<'a> {
    program: &'a PreparedEffectProgram,
    facts: &'a Arc<Facts>,
}
impl EffectProcessorInputService for Source<'_> {
    fn prepare_input(
        &mut self,
        stage: &CompiledEffectStageBinding,
        request: EffectProcessorInputRequest,
        admit: &mut dyn FnMut(usize) -> std::result::Result<(), EffectTemporalExecutionError>,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<PreparedEffectProcessorInput, EffectTemporalExecutionError> {
        assert!(
            !control.resolves_processor_input(),
            "source preparation cannot recursively execute DSP"
        );
        self.facts.calls.lock().unwrap().push((stage.stage_index(), request.time));
        let mode = self.facts.mode.load(Ordering::SeqCst);
        if mode == 1 {
            panic!("owned source scheduler failure");
        }
        if mode == 3 {
            admit(usize::MAX)?;
        }
        if mode == 4 {
            self.facts.cancellation.cancel();
        }
        let output_time = if mode == 6 {
            request.time.checked_add(time(1, 1)).unwrap()
        } else {
            request.time
        };
        let execution = self
            .program
            .input_for_stage(stage)
            .expect("original sealed binding")
            .prepare_execution(
                &EffectTemporalExecutionRequest::new(
                    77,
                    EffectExecutionContinuity::Continuous,
                    output_time,
                    request.frame_extent,
                    request.sampling,
                    request.frame_extent.full_frame_roi(),
                    self.facts.cancellation.with_deadline(request.deadline),
                ),
                |_| Ok(0),
            )?;
        if mode != 2 {
            admit(execution.demands().coverage_bytes())?;
        }
        let tiles = execution
            .demands()
            .requests()
            .iter()
            .copied()
            .map(|r| {
                (
                    r,
                    EffectFrameTileF32::new(
                        r.time(),
                        request.frame_extent,
                        request.sampling,
                        request.frame_extent.full_frame_roi(),
                        0,
                        vec![[
                            1.0 + r.time().to_f64() as f32
                                + self.facts.epoch.load(Ordering::SeqCst) as f32,
                            0.0,
                            0.0,
                            1.0,
                        ]],
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let frames = PreparedTemporalFrameSet::prepare(
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint(
                [31 + self.facts.epoch.load(Ordering::SeqCst) as u8; 32],
            ),
            execution.demands().clone(),
            tiles,
        )
        .unwrap();
        Ok(PreparedEffectProcessorInput { execution, frames, origin: [-2, 3] })
    }
}
fn frame() -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(1, 1),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        vec![[0.25, 0.0, 0.0, 1.0]],
    )
    .unwrap()
}
fn session() -> EffectExecutionSession {
    let mut config = EffectExecutionSessionConfig::uncached(64 * 1024);
    config.max_cache_entries = 8;
    config.max_cache_bytes = 64 * 1024;
    config.max_processor_instances = 2;
    config.max_processor_bytes = 256;
    let mut session = EffectExecutionSession::new(config);
    session.bind_generation(77);
    session
}

#[test]
fn borrowed_service_reenters_original_bank_and_restores_the_current_node() {
    let facts = Arc::new(Facts::default());
    let (program, graph) = program(&facts);
    let mut source = Source { program: &program, facts: &facts };
    let mut checkpoint = || None;
    let mut control = EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
    let mut session = session();
    let input = frame();
    for epoch in 0..3 {
        facts.epoch.store(epoch, Ordering::SeqCst);
        let output =
            session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control).unwrap();
        assert_eq!(
            output.pixels(),
            &[[3.5 + 2.0 * epoch as f32, 0.0, 0.0, 1.0]]
        );
        assert_eq!(
            session.diagnostics().cache_entries,
            0,
            "no input-only output identity can cover dynamic Source dependencies"
        );
        assert!(control.input_stage.is_none() && control.current_occurrence.is_none());
        assert!(control.input_service.is_some());
        assert_eq!(control.deadline(), None);
        assert_eq!(session.diagnostics().generation, Some(77));
    }
    assert_eq!(
        facts.created.load(Ordering::SeqCst),
        2,
        "same original resident instances"
    );
    assert_eq!(facts.dsp.load(Ordering::SeqCst), 9);
    assert_eq!(
        *facts.calls.lock().unwrap(),
        [(0, time(1, 4)), (1, time(1, 2)), (0, time(3, 4))].repeat(3)
    );
    assert_eq!(input.pixels()[0][0], 0.25, "root input stays immutable");
    drop(session);
    assert_eq!(facts.dropped.load(Ordering::SeqCst), 2);
}

#[test]
fn source_panic_missing_admission_bad_time_and_budget_refusal_do_not_poison_the_provider() {
    for mode in [1, 2, 3, 6] {
        let facts = Arc::new(Facts::default());
        facts.mode.store(mode, Ordering::SeqCst);
        let (program, graph) = program(&facts);
        let mut source = Source { program: &program, facts: &facts };
        let mut checkpoint = || None;
        let mut control =
            EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
        let mut session = session();
        let input = frame();
        assert!(session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control).is_err());
        assert_eq!(facts.dsp.load(Ordering::SeqCst), 0);
        assert_eq!(control.stop_reason(), None);
        assert!(
            control.input_stage.is_none()
                && control.current_occurrence.is_none()
                && control.input_service.is_some()
        );
        assert_eq!(control.deadline(), None);
        facts.mode.store(0, Ordering::SeqCst);
        let output = session
            .apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control)
            .expect("same provider remains usable");
        assert_eq!(output.pixels()[0][0], 3.5);
    }
}

#[test]
fn input_deadline_and_original_cancellation_remain_sticky_without_renewing_the_owner() {
    for mode in [4, 5] {
        let facts = Arc::new(Facts::default());
        facts.mode.store(mode, Ordering::SeqCst);
        let (program, graph) = program(&facts);
        let mut source = Source { program: &program, facts: &facts };
        let mut checkpoint =
            || facts.cancellation.is_canceled().then_some(EffectProcessorStopReason::Canceled);
        let mut control =
            EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
        let mut session = session();
        let input = frame();
        assert!(session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control).is_err());
        assert_eq!(
            control.stop_reason(),
            Some(if mode == 4 {
                EffectProcessorStopReason::Canceled
            } else {
                EffectProcessorStopReason::DeadlineExpired
            })
        );
        assert_eq!(facts.dsp.load(Ordering::SeqCst), 0);
        assert_eq!(
            facts.calls.lock().unwrap().len(),
            if mode == 4 { 1 } else { 0 }
        );
        assert!(
            control.input_service.is_some()
                && control.input_stage.is_none()
                && control.current_occurrence.is_none()
        );
        assert_eq!(
            control.deadline(),
            None,
            "restore the owner's absolute observation value"
        );
    }
}

fn mixed_work(graph: Arc<CompiledEffectGraph>) -> PreparedHeterogeneousEffectWork {
    let cpu = EffectExecutionLaneId::new(1);
    let gpu = EffectExecutionLaneId::new(2);
    let environment = EffectExecutionEnvironment::new(
        Arc::from([
            EffectExecutionLane::new(
                cpu,
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecisions::FLOAT32,
                10,
            ),
            EffectExecutionLane::new(
                gpu,
                EffectProcessingBackend::Gpu,
                EffectWorkingPrecisions::FLOAT32,
                1,
            ),
        ]),
        Arc::from([EffectExecutionTransfer::new(
            cpu,
            EffectWorkingPrecision::Float32,
            gpu,
            EffectWorkingPrecision::Float32,
            2,
        )]),
    )
    .unwrap();
    let format = frame().format();
    let request = EffectGraphExecutionRequest::new(
        EffectFrameExtent::new(1, 1),
        EffectValueResidency::new(cpu, format),
        EffectValueResidency::new(gpu, format),
        EffectGraphExecutionBudget::new(1024 * 1024, 1024 * 1024, 2 * 1024 * 1024, 128, 256),
    );
    let work = PreparedHeterogeneousEffectWork::prepare(graph, &environment, request).unwrap();
    assert_eq!(work.cpu_nodes().len(), 2);
    assert_eq!(work.gpu_suffix().node_ids().len(), 1);
    work
}

#[test]
fn heterogeneous_prefix_preserves_borrowed_service_and_original_bank() {
    let facts = Arc::new(Facts::default());
    let (program, graph) = program_with_gpu_tail(&facts, true);
    let work = mixed_work(graph);
    let mut source = Source { program: &program, facts: &facts };
    let mut checkpoint = || None;
    let mut control = EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
    let mut session = session();
    for epoch in 0..3 {
        facts.epoch.store(epoch, Ordering::SeqCst);
        let input = frame();
        let output = work
            .execute_cpu_prefix_with_control(
                &mut session,
                77,
                input.view(),
                0,
                WorkingColorSpace::LinearRec709,
                &mut control,
            )
            .unwrap();
        assert_eq!(
            output.boundary_values()[0].pixels(),
            &[[3.5 + 2.0 * epoch as f32, 0.0, 0.0, 1.0]]
        );
        assert_eq!(output.evidence().completed_cpu_nodes(), work.cpu_nodes());
        assert!(output.processor_feedback().next().is_none());
        assert!(control.input_stage.is_none() && control.current_occurrence.is_none());
        assert!(control.input_service.is_some());
        assert_eq!(control.deadline(), None);
        assert_eq!(session.diagnostics().generation, Some(77));
        assert_eq!(session.diagnostics().cache_entries, 0);
    }
    assert_eq!(facts.created.load(Ordering::SeqCst), 2);
    assert_eq!(facts.dsp.load(Ordering::SeqCst), 9);
    assert_eq!(
        *facts.calls.lock().unwrap(),
        [(0, time(1, 4)), (1, time(1, 2)), (0, time(3, 4))].repeat(3)
    );
    drop(session);
    assert_eq!(facts.dropped.load(Ordering::SeqCst), 2);
}

#[test]
fn heterogeneous_input_failures_restore_scope_and_original_stop_reason() {
    for mode in [1, 2, 3, 4, 5, 6] {
        let facts = Arc::new(Facts::default());
        facts.mode.store(mode, Ordering::SeqCst);
        let (program, graph) = program_with_gpu_tail(&facts, true);
        let work = mixed_work(graph);
        let mut source = Source { program: &program, facts: &facts };
        let mut checkpoint =
            || facts.cancellation.is_canceled().then_some(EffectProcessorStopReason::Canceled);
        let mut control =
            EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
        let mut session = session();
        let input = frame();
        let result = work.execute_cpu_prefix_with_control(
            &mut session,
            77,
            input.view(),
            0,
            WorkingColorSpace::LinearRec709,
            &mut control,
        );
        assert!(result.is_err());
        assert_eq!(facts.dsp.load(Ordering::SeqCst), 0);
        assert!(
            control.input_stage.is_none()
                && control.current_occurrence.is_none()
                && control.input_service.is_some()
        );
        assert_eq!(control.deadline(), None);
        assert_eq!(control.feedback_count(), 0);
        if matches!(mode, 4 | 5) {
            let reason = if mode == 4 {
                HeterogeneousCpuExecutionStopReason::Canceled
            } else {
                HeterogeneousCpuExecutionStopReason::DeadlineExpired
            };
            assert!(
                matches!(result, Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped { generation:77, reason:r }) if r==reason)
            );
            assert_eq!(
                control.stop_reason(),
                Some(if mode == 4 {
                    EffectProcessorStopReason::Canceled
                } else {
                    EffectProcessorStopReason::DeadlineExpired
                })
            );
        } else {
            assert_eq!(control.stop_reason(), None);
            facts.mode.store(0, Ordering::SeqCst);
            let output = work
                .execute_cpu_prefix_with_control(
                    &mut session,
                    77,
                    input.view(),
                    0,
                    WorkingColorSpace::LinearRec709,
                    &mut control,
                )
                .expect("same provider recovers without quarantine or DSP replay");
            assert_eq!(output.boundary_values()[0].pixels()[0][0], 3.5);
        }
    }
}

fn prior_feedback() -> EffectProcessorFeedbackReceipt {
    let effect_id = mondrian_core::EffectId::new();
    EffectProcessorFeedbackReceipt {
        effect_id,
        preparation: EffectFloatProcessorPreparation {
            effect_id,
            width: 1,
            height: 1,
            format: frame().format(),
            project_geometry: None,
            input_role: EffectFloatProcessorInputRole::SingleImage,
        },
        provider_revision: 19,
        retention: EffectProcessorRetention::Evicted,
        feedback: EffectProcessorFeedback {
            owner: mondrian_core::EffectProcessorOwnerId::new(),
            origin: EffectProcessorExecutionOrigin::InProcess,
            snapshot: EffectProcessorFeedbackSnapshot::new(
                19,
                1,
                1,
                Some(
                    EffectProcessorNotice::new(
                        EffectProcessorNoticeKind::Warning,
                        "caller-owned earlier receipt".into(),
                        "prior".into(),
                    )
                    .unwrap(),
                ),
                None,
                Vec::new(),
            )
            .unwrap(),
        },
    }
}

#[test]
fn heterogeneous_prefix_preserves_caller_feedback_at_the_static_frontier_grant() {
    for short in [true, false] {
        let facts = Arc::new(Facts::default());
        facts.mode.store(7, Ordering::SeqCst);
        let (program, graph) = program_with_gpu_tail(&facts, true);
        let work = mixed_work(graph);
        let mut source = Source { program: &program, facts: &facts };
        let mut checkpoint = || None;
        let mut control =
            EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
        let prior = prior_feedback();
        control.feedback.push(prior.clone());
        let owned = control.feedback_retained_bytes() - control.feedback_slot_bytes();
        assert!(owned > 0);
        let mut config = EffectExecutionSessionConfig::uncached(
            work.cpu_required_working_bytes() + owned - usize::from(short),
        );
        config.max_processor_instances = 2;
        config.max_processor_bytes = 256;
        let mut session = EffectExecutionSession::new(config);
        session.bind_generation(77);
        let input = frame();
        let result = work.execute_cpu_prefix_with_control(
            &mut session,
            77,
            input.view(),
            0,
            WorkingColorSpace::LinearRec709,
            &mut control,
        );
        if short {
            assert!(matches!(
                result,
                Err(PreparedHeterogeneousEffectWorkError::WorkingBudgetExceeded { .. })
            ));
            assert_eq!(facts.created.load(Ordering::SeqCst), 0);
            assert!(facts.calls.lock().unwrap().is_empty());
        } else {
            let output = result.expect("admitted static caller backing uses the same ledger once");
            assert_eq!(output.boundary_values()[0].pixels()[0][0], 0.25);
            assert!(output.processor_feedback().next().is_none());
            assert_eq!(facts.dsp.load(Ordering::SeqCst), 2);
            assert!(facts.calls.lock().unwrap().is_empty());
        }
        let receipts = control.take_feedback().unwrap();
        assert_eq!(receipts.iter().collect::<Vec<_>>(), vec![&prior]);
    }
}
