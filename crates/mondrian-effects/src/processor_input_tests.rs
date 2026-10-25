use super::*;
use crate::*;
use mondrian_core::{effect_data::EffectType, TimelineTime, WorkingColorSpace};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex, OnceLock, Weak,
};

const SOURCE: [f32; 4] = [0.5, -0.25, 2.0, 1.0];
const BUDGET: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
struct Facts {
    mode: AtomicUsize,
    created: [AtomicUsize; 2],
    dropped: [AtomicUsize; 2],
    calls: [AtomicUsize; 2],
    nested_peak: AtomicUsize,
    after_source: AtomicBool,
    progress: Mutex<Vec<EffectProcessorProgressReceipt>>,
    cancel: mondrian_core::ExecutionCancellationToken,
}
impl EffectProcessorObserver for Facts {
    fn maximum_storage_bytes(&self) -> usize {
        8 * EffectProcessorProgressReceipt::maximum_retained_bytes()
    }
    fn question_service(&self) -> Option<&dyn EffectProcessorQuestionService> {
        None
    }
    fn observe(&self, receipt: &EffectProcessorProgressReceipt) {
        self.progress.lock().unwrap().push(receipt.clone());
    }
}

#[derive(Debug)]
struct Factory {
    parent: bool,
    facts: Arc<Facts>,
    // Test bootstrap only. Weak breaks the program->factory->input->program
    // ownership cycle; production input admission must be invocation-borrowed.
    input: Weak<OnceLock<PreparedEffectInput>>,
}
#[derive(Debug)]
struct Instance {
    parent: bool,
    facts: Arc<Facts>,
    input: Weak<OnceLock<PreparedEffectInput>>,
    owner: mondrian_core::EffectProcessorOwnerId,
    invocation: u64,
}
impl Drop for Instance {
    fn drop(&mut self) {
        self.facts.dropped[usize::from(self.parent)].fetch_add(1, Ordering::SeqCst);
    }
}
impl EffectFloatProcessorFactory for Factory {
    fn alpha_contract(&self) -> EffectAlphaContract {
        EffectAlphaContract::COVERAGE_OPERATION
    }
    fn feedback_budget_bytes(&self) -> usize {
        EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
    }
    fn storage(
        &self,
        _: EffectFloatProcessorSizing,
    ) -> mondrian_core::Result<EffectFloatProcessorStorage> {
        EffectFloatProcessorStorage::new(self.feedback_budget_bytes() + 256, 128)
    }
    fn prepare(
        &self,
        _: EffectFloatProcessorPreparation,
        control: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        control.check()?;
        self.facts.created[usize::from(self.parent)].fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Instance {
            parent: self.parent,
            facts: self.facts.clone(),
            input: self.input.clone(),
            owner: mondrian_core::EffectProcessorOwnerId::new(),
            invocation: 0,
        }))
    }
}

struct Source {
    inner: PreparedTemporalFrameSet,
    facts: Arc<Facts>,
}
impl EffectTemporalFrameProvider for Source {
    fn source_identity(&self) -> EffectTemporalSourceIdentity {
        self.inner.source_identity()
    }
    fn retained_coverage_bytes(&self) -> usize {
        self.inner.retained_coverage_bytes()
    }
    fn copy_frame(
        &mut self,
        request: EffectTemporalFrameRequest,
        cancel: &mondrian_core::ExecutionCancellationToken,
        pixels: &mut Vec<[f32; 4]>,
    ) -> std::result::Result<(), EffectTemporalFrameProviderError> {
        self.inner.copy_frame(request, cancel, pixels)?;
        self.facts.after_source.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl EffectFloatProcessorInstance for Instance {
    fn process(
        &mut self,
        pixels: &mut EffectFloatFrame,
        params: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
        resources: &mut EffectProcessorResources<'_>,
    ) -> std::result::Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        control.check()?;
        self.invocation += 1;
        self.facts.calls[usize::from(self.parent)].fetch_add(1, Ordering::SeqCst);
        if self.parent {
            let parent_binding = control.current_occurrence.unwrap();
            let binding = self.input.upgrade().unwrap();
            let input = binding.get().unwrap();
            let time = TimelineTime::new(-1, 1).unwrap();
            let extent = pixels.extent();
            let request = EffectTemporalExecutionRequest::new(
                19,
                EffectExecutionContinuity::Discontinuous,
                time,
                extent,
                pixels.sampling(),
                extent.full_frame_roi(),
                self.facts.cancel.clone(),
            );
            let execution = input.prepare_execution(&request, |_| Ok(0)).unwrap();
            let batch = execution.demands().clone();
            let frames = batch
                .requests()
                .iter()
                .copied()
                .map(|r| {
                    (
                        r,
                        EffectFrameTileF32::new(
                            r.time(),
                            extent,
                            pixels.sampling(),
                            extent.full_frame_roi(),
                            0,
                            vec![SOURCE; pixels.len()],
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            let mut provider = Source {
                inner: PreparedTemporalFrameSet::prepare(
                    EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([79; 32]),
                    batch,
                    frames,
                )
                .unwrap(),
                facts: self.facts.clone(),
            };
            self.facts.after_source.store(false, Ordering::SeqCst);
            let held = if self.facts.mode.load(Ordering::SeqCst) == 1 {
                // After a successful warm call, leave exactly one byte less
                // than its measured same-bank execution peak for the input.
                resources.available_bytes
                    - resources.active_bytes
                    - self.facts.nested_peak.load(Ordering::SeqCst)
                    + 1
            } else {
                128
            };
            let result =
                resources.execute_prepared_input_f32(&execution, &mut provider, held, control);
            assert_eq!(control.current_occurrence, Some(parent_binding));
            let output = result.map_err(|source| {
                EffectProcessorInvocationError::InputFailed(Box::new(
                    EffectExecutionError::TemporalInput { source: Box::new(source) },
                ))
            })?;
            self.facts.nested_peak.store(output.peak_working_bytes(), Ordering::SeqCst);
            pixels.pixels_mut().copy_from_slice(output.tile().pixels());
        } else {
            let gain = params["gain"].as_f64().unwrap() as f32;
            if gain == 1.0 {
                match self.facts.mode.load(Ordering::SeqCst) {
                    2 => panic!("upstream processor fault"),
                    3 => self.facts.cancel.cancel(),
                    4 => {
                        control.observation.deadline =
                            Some(std::time::Instant::now() - std::time::Duration::from_millis(1))
                    }
                    _ => {}
                }
                control.check()?;
            }
            for p in pixels.pixels_mut() {
                for lane in &mut p[..3] {
                    *lane *= gain;
                }
            }
        }
        control.observe_progress(EffectProcessorLiveProgress {
            owner: self.owner,
            origin: EffectProcessorExecutionOrigin::InProcess,
            native_owner: 1,
            invocation: self.invocation,
            revision: self.invocation,
            progress: EffectProcessorProgress::new(
                1,
                true,
                0.5,
                EffectProcessorNotice::new(
                    EffectProcessorNoticeKind::Information,
                    "nested input".into(),
                    "input".into(),
                )
                .unwrap(),
            )
            .unwrap(),
        })?;
        Ok(Some(EffectProcessorFeedback {
            owner: self.owner,
            origin: EffectProcessorExecutionOrigin::InProcess,
            snapshot: EffectProcessorFeedbackSnapshot::new(
                1,
                self.invocation,
                self.invocation,
                (!self.parent
                    && params["gain"] == 2.0
                    && self.facts.mode.load(Ordering::SeqCst) == 6)
                    .then(|| {
                        EffectProcessorNotice::new(
                            EffectProcessorNoticeKind::Information,
                            "x".repeat(1024),
                            "prior".into(),
                        )
                        .unwrap()
                    }),
                None,
                vec![],
            )
            .unwrap(),
        }))
    }
}

struct Fixture {
    facts: Arc<Facts>,
    input: Arc<OnceLock<PreparedEffectInput>>,
    graph: Arc<CompiledEffectGraph>,
    session: EffectExecutionSession,
    child_key: String,
    parent_key: String,
    child_id: mondrian_core::EffectId,
    parent_id: mondrian_core::EffectId,
}
fn fixture(builtin: bool, count: usize) -> Fixture {
    let facts = Arc::new(Facts::default());
    facts.progress.lock().unwrap().reserve_exact(8);
    let input = Arc::new(OnceLock::new());
    let child_key = format!("fixture.borrowed-child.{}", mondrian_core::EffectId::new());
    let parent_key = format!("fixture.borrowed-parent.{}", mondrian_core::EffectId::new());
    for (parent, key) in [(false, &child_key), (true, &parent_key)] {
        register_effect_definition(
            EffectDefinition::new(
                key,
                "Borrowed input",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_extension_contract(EffectExtensionContract::new("1.0"))
            .with_custom_float_processor_factory(
                Arc::new(move |_, c| {
                    if parent && c.time != TimelineTime::ZERO {
                        return Err(EffectGraphBuildError::InvalidGraph);
                    }
                    Ok(Some(serde_json::json!({"gain":2.0 + c.time.to_f64()})))
                }),
                None,
                EffectCachePolicy::Uncacheable,
                Arc::new(Factory {
                    parent,
                    facts: facts.clone(),
                    input: Arc::downgrade(&input),
                }),
            ),
        )
        .unwrap();
    }
    let child = if builtin {
        let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
        let parameter = EffectType::BasicCorrection.parameter_id("exposure").unwrap();
        effect
            .set_static_value_by_parameter(&parameter, mondrian_core::PropertyValue::Float(1.0))
            .unwrap();
        effect
    } else {
        EffectNode::new(EffectType::External(child_key.clone()))
    };
    let parent = EffectNode::new(EffectType::External(parent_key.clone()));
    let child_id = child.id;
    let parent_id = parent.id;
    let program =
        PreparedEffectProgram::prepare(&[child, parent], &[], WorkingColorSpace::LinearRec709)
            .unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    input.set(program.input_for_stage(&graph.stage_bindings()[1]).unwrap()).unwrap();
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: count,
        max_processor_bytes: count * 128,
        ..EffectExecutionSessionConfig::uncached(BUDGET)
    });
    session.bind_generation(19);
    Fixture {
        facts,
        input,
        graph,
        session,
        child_key,
        parent_key,
        child_id,
        parent_id,
    }
}
fn frame(width: u32) -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(width, 1),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        vec![SOURCE; width as usize],
    )
    .unwrap()
}
fn control<'a>(
    facts: &Arc<Facts>,
    callback: &'a mut dyn FnMut() -> Option<EffectProcessorStopReason>,
) -> EffectProcessorControl<'a> {
    let mut control = EffectProcessorControl::new(callback);
    control
        .bind_observation(EffectProcessorObservation { sink: Some(facts.clone()), deadline: None });
    control
}

#[test]
fn upstream_uses_same_bank_control_feedback_and_original_generation() {
    let mut f = fixture(false, 2);
    for _ in 0..2 {
        let mut check = || None;
        let mut control = control(&f.facts, &mut check);
        let output = f
            .session
            .apply_compiled_rgba_f32(frame(1).view(), &f.graph, 0, &mut control)
            .unwrap();
        assert_eq!(output.pixels(), &[SOURCE]);
        assert!(control.current_occurrence.is_none());
        let receipts = control.take_feedback().unwrap();
        assert_eq!(
            receipts.iter().map(|r| r.effect_id).collect::<Vec<_>>(),
            [f.child_id, f.child_id, f.parent_id]
        );
        let receipts = receipts.iter().collect::<Vec<_>>();
        assert_eq!(receipts[0].feedback.owner, receipts[1].feedback.owner);
        assert_ne!(receipts[0].feedback.owner, receipts[2].feedback.owner);
        assert_eq!(f.session.diagnostics().processor_instances, 2);
        assert_eq!(f.session.diagnostics().cache_entries, 0);
        assert_eq!(f.session.diagnostics().generation, Some(19));
    }
    assert_eq!(f.facts.created[0].load(Ordering::SeqCst), 1);
    assert_eq!(f.facts.created[1].load(Ordering::SeqCst), 1);
    assert_eq!(f.facts.calls[0].load(Ordering::SeqCst), 4);
    assert_eq!(f.facts.calls[1].load(Ordering::SeqCst), 2);
    let progress = f.facts.progress.lock().unwrap();
    assert_eq!(
        progress.iter().map(|r| r.preparation.effect_id).collect::<Vec<_>>(),
        [
            f.child_id,
            f.child_id,
            f.parent_id,
            f.child_id,
            f.child_id,
            f.parent_id
        ]
    );
    drop(progress);
    drop(f.session);
    assert_eq!(f.facts.dropped[0].load(Ordering::SeqCst), 1);
    assert_eq!(f.facts.dropped[1].load(Ordering::SeqCst), 1);
    assert!(
        Arc::strong_count(&f.input) == 1,
        "no program/factory ownership cycle"
    );
}

#[test]
fn nested_input_respects_parent_peak_and_retention_ceiling() {
    for count in [0, 1, 2] {
        let mut f = fixture(false, count);
        f.session
            .apply_compiled_rgba_f32(
                frame(1).view(),
                &f.graph,
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert!(f.session.diagnostics().processor_instances <= count);
        assert!(f.session.diagnostics().processor_bytes <= count * 128);
        f.facts.mode.store(1, Ordering::SeqCst);
        let original = frame(1);
        let error = f
            .session
            .apply_compiled_rgba_f32(
                original.view(),
                &f.graph,
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            EffectFloatExecutionError::CustomProcessor(EffectExecutionError::TemporalInput { .. })
        ));
        assert_eq!(original.pixels(), &[SOURCE]);
        assert_eq!(
            f.facts.calls[0].load(Ordering::SeqCst),
            3,
            "one byte short stops before upstream DSP"
        );
        assert!(!effect_extension_runtime_status(&f.parent_key).unwrap().disabled);
        assert!(!effect_extension_runtime_status(&f.child_key).unwrap().disabled);
    }
}

#[test]
fn already_charged_parent_feedback_does_not_inflate_nested_working_peak() {
    let mut f = fixture(false, 2);
    let mut small = EffectProcessorControl::uncancelled();
    f.session
        .apply_compiled_rgba_f32(frame(1).view(), &f.graph, 0, &mut small)
        .unwrap();
    let peak = f.facts.nested_peak.load(Ordering::SeqCst);
    let prior_bytes = small.feedback_retained_bytes();
    f.facts.mode.store(6, Ordering::SeqCst);
    let mut large = EffectProcessorControl::uncancelled();
    f.session
        .apply_compiled_rgba_f32(frame(1).view(), &f.graph, 0, &mut large)
        .unwrap();
    assert!(large.feedback_retained_bytes() > prior_bytes + 1023);
    assert_eq!(
        f.facts.nested_peak.load(Ordering::SeqCst),
        peak,
        "prior root feedback is already outside the nested grant; charge only new nested facts"
    );
    assert_eq!(large.take_feedback().unwrap().iter().count(), 3);
}

#[test]
fn nested_fault_and_owner_stops_keep_attribution_and_publish_no_parent_feedback() {
    for mode in [2, 3, 4] {
        let mut f = fixture(false, 2);
        f.facts.mode.store(mode, Ordering::SeqCst);
        let token = f.facts.cancel.clone();
        let mut check = || token.is_canceled().then_some(EffectProcessorStopReason::Canceled);
        let mut control = control(&f.facts, &mut check);
        let original = frame(1);
        let error = f
            .session
            .apply_compiled_rgba_f32(original.view(), &f.graph, 0, &mut control)
            .unwrap_err();
        if mode == 2 {
            assert!(matches!(
                error,
                EffectFloatExecutionError::CustomProcessor(
                    EffectExecutionError::TemporalInput { .. }
                )
            ));
        } else {
            assert!(
                matches!(error, EffectFloatExecutionError::Stopped { reason } if reason == if mode == 3 { EffectProcessorStopReason::Canceled } else { EffectProcessorStopReason::DeadlineExpired })
            );
        }
        assert_eq!(original.pixels(), &[SOURCE]);
        assert!(!effect_extension_runtime_status(&f.parent_key).unwrap().disabled);
        assert_eq!(
            effect_extension_runtime_status(&f.child_key).unwrap().disabled,
            mode == 2
        );
        assert!(control.current_occurrence.is_none());
        assert!(control.take_feedback().unwrap().iter().all(|r| r.effect_id == f.child_id));
        assert_eq!(f.facts.dropped[0].load(Ordering::SeqCst), 1);
        assert_eq!(f.facts.dropped[1].load(Ordering::SeqCst), 1);
    }
}

#[test]
fn ordinary_upstream_kernels_poll_the_original_control_inside_pixel_chunks() {
    let mut f = fixture(true, 1);
    let facts = f.facts.clone();
    let mut after_copy_polls = 0;
    let mut check = || {
        if facts.after_source.load(Ordering::SeqCst) {
            after_copy_polls += 1;
            if after_copy_polls >= 5 {
                return Some(EffectProcessorStopReason::DeadlineExpired);
            }
        }
        None
    };
    let mut control = control(&f.facts, &mut check);
    let original = frame(8193);
    let error = f
        .session
        .apply_compiled_rgba_f32(original.view(), &f.graph, 0, &mut control)
        .unwrap_err();
    assert!(matches!(
        error,
        EffectFloatExecutionError::Stopped { reason: EffectProcessorStopReason::DeadlineExpired }
    ));
    assert_eq!(
        control.stop_reason(),
        Some(EffectProcessorStopReason::DeadlineExpired)
    );
    assert_eq!(original.pixels(), vec![SOURCE; 8193]);
    assert!(control.take_feedback().unwrap().is_empty());
    assert_eq!(f.session.diagnostics().processor_instances, 0);
    assert!(!effect_extension_runtime_status(&f.parent_key).unwrap().disabled);
}
