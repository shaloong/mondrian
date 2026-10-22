//! Actual-frame admission controls; synthetic providers never interpret native IDs.

use crate::*;
use mondrian_core::{
    ColorFrameAlpha, EffectId, ExecutionCancellationToken, ImageSampling, Rational,
    SampleAspectRatio, TimelineTime, WorkingColorSpace,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Debug, Default)]
struct Counts {
    sized: AtomicUsize,
    prepared: AtomicUsize,
    processed: AtomicUsize,
    dropped: AtomicUsize,
    last_owner: AtomicUsize,
}

#[derive(Debug)]
struct Factory {
    counts: Arc<Counts>,
    role: EffectFloatProcessorInputRole,
    requirement: EffectImageScaleRequirement,
}

impl EffectFloatProcessorFactory for Factory {
    fn input_role(&self) -> EffectFloatProcessorInputRole {
        self.role
    }
    fn image_scale_requirement(&self) -> EffectImageScaleRequirement {
        self.requirement
    }
    fn alpha_contract(&self) -> EffectAlphaContract {
        match self.role {
            EffectFloatProcessorInputRole::SingleImage => EffectAlphaContract::IDENTITY,
            EffectFloatProcessorInputRole::NoImage => EffectAlphaContract::SOURCE_FLOAT,
        }
    }
    fn feedback_budget_bytes(&self) -> usize {
        0
    }
    fn storage(
        &self,
        _: EffectFloatProcessorSizing,
    ) -> mondrian_core::Result<EffectFloatProcessorStorage> {
        self.counts.sized.fetch_add(1, Ordering::SeqCst);
        EffectFloatProcessorStorage::new(256, 64)
    }
    fn prepare(
        &self,
        _: EffectFloatProcessorPreparation,
        _: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        let owner = self.counts.prepared.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::new(Instance {
            counts: self.counts.clone(),
            owner,
            role: self.role,
        }))
    }
}

#[derive(Debug)]
struct Instance {
    counts: Arc<Counts>,
    owner: usize,
    role: EffectFloatProcessorInputRole,
}
impl Drop for Instance {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl EffectFloatProcessorInstance for Instance {
    fn process(
        &mut self,
        frame: &mut EffectFloatFrame,
        _: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        control.check()?;
        self.counts.processed.fetch_add(1, Ordering::SeqCst);
        self.counts.last_owner.store(self.owner, Ordering::SeqCst);
        if self.role == EffectFloatProcessorInputRole::NoImage {
            frame.pixels_mut().fill([0.25, 0.5, 0.75, 1.0]);
        }
        Ok(None)
    }
}

fn fixture(
    role: EffectFloatProcessorInputRole,
    requirement: EffectImageScaleRequirement,
) -> (Arc<CompiledEffectGraph>, Arc<Counts>, String) {
    fixture_with_policy(role, requirement, EffectCachePolicy::Uncacheable)
}

fn fixture_with_policy(
    role: EffectFloatProcessorInputRole,
    requirement: EffectImageScaleRequirement,
    policy: EffectCachePolicy,
) -> (Arc<CompiledEffectGraph>, Arc<Counts>, String) {
    let counts = Arc::new(Counts::default());
    let key = format!("test.actual-scale.{}", EffectId::new());
    register_effect_definition(
        EffectDefinition::new(
            &key,
            "Actual scale",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: if policy == EffectCachePolicy::Deterministic {
                EffectDeterminism::Deterministic
            } else {
                EffectDeterminism::Nondeterministic
            },
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_extension_contract(EffectExtensionContract::new("1.0.0"))
        .with_custom_float_processor_factory(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            policy,
            Arc::new(Factory { counts: counts.clone(), role, requirement }),
        ),
    )
    .unwrap();
    let effect = instantiate_effect_node(EffectType::External(key.clone())).unwrap();
    let graph = PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    (graph, counts, key)
}

fn format() -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightFloat,
    )
}
fn sampling(x: Rational, y: Rational) -> ImageSampling {
    ImageSampling::new(SampleAspectRatio::new(4, 3).unwrap(), [x, y]).unwrap()
}
fn unit() -> ImageSampling {
    sampling(Rational::new(1, 1), Rational::new(1, 1))
}
fn reduced() -> ImageSampling {
    sampling(Rational::new(1, 2), Rational::new(1, 2))
}
fn nonuniform() -> ImageSampling {
    sampling(Rational::new(1, 1), Rational::new(2, 3))
}
fn frame(sampling: ImageSampling) -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(1, 1),
        format(),
        sampling,
        vec![[0.25, 0.5, 0.75, 1.0]],
    )
    .unwrap()
}
fn session() -> EffectExecutionSession {
    EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 2,
        max_processor_bytes: 1024,
        ..EffectExecutionSessionConfig::uncached(1024 * 1024)
    })
}
fn assert_refused(error: EffectFloatExecutionError, actual: ImageSampling) {
    assert_eq!(
        error,
        EffectFloatExecutionError::CustomProcessor(EffectExecutionError::ImageScaleNotAdmitted {
            requirement: EffectImageScaleRequirement::Unit,
            actual_sampling: actual,
        })
    );
}
fn assert_healthy(key: &str) {
    let status = effect_extension_runtime_status(key).unwrap();
    assert!(!status.disabled);
    assert!(status.last_error.is_none());
}

#[test]
fn unit_scale_refusal_preserves_owner_and_accepts_non_square_unit_pixels() {
    let (graph, counts, key) = fixture(
        EffectFloatProcessorInputRole::SingleImage,
        EffectImageScaleRequirement::Unit,
    );
    let mut session = session();
    let input = frame(unit());
    let first = session
        .apply_compiled_rgba_f32(
            input.view(),
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_eq!(first.sampling(), unit());
    assert_eq!(first.pixels(), input.pixels());
    let sized = counts.sized.load(Ordering::SeqCst);
    for actual in [reduced(), nonuniform()] {
        let input = frame(actual);
        let mut control = EffectProcessorControl::uncancelled();
        assert_refused(
            session
                .apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control)
                .unwrap_err(),
            actual,
        );
        assert_eq!(control.feedback_count(), 0);
        assert_eq!(counts.sized.load(Ordering::SeqCst), sized);
        assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(counts.processed.load(Ordering::SeqCst), 1);
        assert_eq!(counts.dropped.load(Ordering::SeqCst), 0);
        assert_eq!(session.diagnostics().processor_instances, 1);
        assert_healthy(&key);
    }
    session
        .apply_compiled_rgba_f32(
            frame(unit()).view(),
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(counts.last_owner.load(Ordering::SeqCst), 1);
    assert_eq!(counts.processed.load(Ordering::SeqCst), 2);
}

#[test]
fn no_image_scale_refusal_uses_explicit_target_before_storage_or_prepare() {
    let (graph, counts, key) = fixture(
        EffectFloatProcessorInputRole::NoImage,
        EffectImageScaleRequirement::Unit,
    );
    let mut session = session();
    assert_refused(
        session
            .generate_compiled_rgba_f32(
                EffectFrameExtent::new(1, 1),
                format(),
                nonuniform(),
                &graph,
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap_err(),
        nonuniform(),
    );
    assert_eq!(counts.sized.load(Ordering::SeqCst), 0);
    assert_eq!(counts.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(session.diagnostics().processor_instances, 0);
    assert_healthy(&key);
    let output = session
        .generate_compiled_rgba_f32(
            EffectFrameExtent::new(1, 1),
            format(),
            unit(),
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_eq!(output.sampling(), unit());
    assert_eq!(output.pixels(), &[[0.25, 0.5, 0.75, 1.0]]);
}

#[test]
fn any_scale_provider_keeps_reduced_sampling_for_single_image_and_no_image() {
    for role in [
        EffectFloatProcessorInputRole::SingleImage,
        EffectFloatProcessorInputRole::NoImage,
    ] {
        let (graph, counts, _) = fixture(role, EffectImageScaleRequirement::Any);
        let mut session = session();
        for actual in [reduced(), nonuniform()] {
            let output = match role {
                EffectFloatProcessorInputRole::SingleImage => session
                    .apply_compiled_rgba_f32(
                        frame(actual).view(),
                        &graph,
                        0,
                        &mut EffectProcessorControl::uncancelled(),
                    )
                    .unwrap(),
                EffectFloatProcessorInputRole::NoImage => session
                    .generate_compiled_rgba_f32(
                        EffectFrameExtent::new(1, 1),
                        format(),
                        actual,
                        &graph,
                        0,
                        &mut EffectProcessorControl::uncancelled(),
                    )
                    .unwrap(),
            };
            assert_eq!(output.sampling(), actual);
        }
        assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(counts.processed.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn zero_opacity_pass_does_not_apply_an_unexecuted_unit_requirement() {
    let (graph, counts, _) = fixture(
        EffectFloatProcessorInputRole::SingleImage,
        EffectImageScaleRequirement::Unit,
    );
    let input = frame(reduced());
    let output = session()
        .apply_compiled_pass_rgba_f32(
            input.view(),
            &graph,
            0.0,
            mondrian_core::EffectPassBlendPolicy::Mix,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_eq!(output, input);
    assert_eq!(counts.sized.load(Ordering::SeqCst), 0);
    assert_eq!(counts.prepared.load(Ordering::SeqCst), 0);
}

#[derive(Default)]
struct Provider {
    copied: usize,
}
impl EffectTemporalFrameProvider for Provider {
    fn source_identity(&self) -> EffectTemporalSourceIdentity {
        EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([21; 32])
    }
    fn retained_coverage_bytes(&self) -> usize {
        0
    }
    fn copy_frame(
        &mut self,
        request: EffectTemporalFrameRequest,
        _: &ExecutionCancellationToken,
        destination: &mut Vec<[f32; 4]>,
    ) -> Result<(), EffectTemporalFrameProviderError> {
        self.copied += 1;
        let roi = request.input_roi().region();
        destination.resize(
            (roi.width() * roi.height()) as usize,
            [0.25, 0.5, 0.75, 1.0],
        );
        Ok(())
    }
}

#[test]
fn temporal_unit_admission_precedes_source_and_sizing_and_recovers_same_owner() {
    let (graph, counts, key) = fixture(
        EffectFloatProcessorInputRole::SingleImage,
        EffectImageScaleRequirement::Unit,
    );
    let extent = EffectFrameExtent::new(1, 1);
    let request = |actual| {
        EffectTemporalExecutionRequest::new(
            9,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ZERO,
            extent,
            actual,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
    };
    let mut session = session();
    let mut provider = Provider::default();
    session.execute_temporal_f32(&graph, &request(unit()), &mut provider).unwrap();
    let sized = counts.sized.load(Ordering::SeqCst);
    let copied = provider.copied;
    assert_eq!(
        session
            .execute_temporal_f32(&graph, &request(reduced()), &mut provider)
            .unwrap_err(),
        EffectTemporalExecutionError::CustomProcessor(
            EffectExecutionError::ImageScaleNotAdmitted {
                requirement: EffectImageScaleRequirement::Unit,
                actual_sampling: reduced(),
            }
        )
    );
    assert_eq!(counts.sized.load(Ordering::SeqCst), sized);
    assert_eq!(provider.copied, copied);
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(session.diagnostics().processor_instances, 1);
    assert_healthy(&key);
    session.execute_temporal_f32(&graph, &request(unit()), &mut provider).unwrap();
    assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(counts.processed.load(Ordering::SeqCst), 2);
}

#[test]
fn heterogeneous_unit_admission_checks_only_actual_cpu_work_before_runtime_sizing() {
    let (graph, counts, key) = fixture(
        EffectFloatProcessorInputRole::SingleImage,
        EffectImageScaleRequirement::Unit,
    );
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
    let request = EffectGraphExecutionRequest::new(
        EffectFrameExtent::new(1, 1),
        EffectValueResidency::new(cpu, format()),
        EffectValueResidency::new(gpu, format()),
        EffectGraphExecutionBudget::new(1024 * 1024, 1024 * 1024, 2 * 1024 * 1024, 32, 64),
    );
    let work = PreparedHeterogeneousEffectWork::prepare(graph, &environment, request).unwrap();
    let mut session = session();
    session.bind_generation(11);
    work.execute_cpu_prefix_uncancelled(
        &mut session,
        11,
        frame(unit()).view(),
        0,
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    let sized = counts.sized.load(Ordering::SeqCst);
    assert!(
        matches!(work.execute_cpu_prefix_uncancelled(&mut session, 11,
        frame(nonuniform()).view(), 0, WorkingColorSpace::LinearRec709),
        Err(PreparedHeterogeneousEffectWorkError::CustomProcessor(
            EffectExecutionError::ImageScaleNotAdmitted { requirement: EffectImageScaleRequirement::Unit,
                actual_sampling })) if actual_sampling == nonuniform())
    );
    assert_eq!(
        counts.sized.load(Ordering::SeqCst),
        sized,
        "cold pure planning may size; rejected actual work cannot size again"
    );
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(session.diagnostics().processor_instances, 1);
    assert_healthy(&key);
    work.execute_cpu_prefix_uncancelled(
        &mut session,
        11,
        frame(unit()).view(),
        0,
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    assert_eq!(counts.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(counts.last_owner.load(Ordering::SeqCst), 1);
}

pub(crate) fn cacheable_unit_graph() -> Arc<CompiledEffectGraph> {
    fixture_with_policy(
        EffectFloatProcessorInputRole::SingleImage,
        EffectImageScaleRequirement::Unit,
        EffectCachePolicy::Deterministic,
    )
    .0
}

#[test]
fn direct_custom_dispatch_refuses_actual_scale_before_staging_or_bank_mutation() {
    for role in [
        EffectFloatProcessorInputRole::SingleImage,
        EffectFloatProcessorInputRole::NoImage,
    ] {
        let (graph, counts, key) = fixture(role, EffectImageScaleRequirement::Unit);
        let operation = graph
            .graph()
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect { op, .. }
                | EffectGraphNodeKind::DomainEffect { op, .. }
                | EffectGraphNodeKind::CustomSource { op, .. }
                    if matches!(op, EffectRenderOp::Custom { .. }) =>
                {
                    Some(op)
                }
                _ => None,
            })
            .unwrap();
        let mut session = session();
        let mut actual = frame(nonuniform());
        let before = actual.clone();
        let mut control = EffectProcessorControl::uncancelled();
        let result = match role {
            EffectFloatProcessorInputRole::SingleImage => {
                crate::adjustment::apply_custom_render_op_f32(
                    &mut actual,
                    operation,
                    0,
                    Some(&mut session),
                    0,
                    &mut control,
                )
            }
            EffectFloatProcessorInputRole::NoImage => {
                crate::adjustment::apply_custom_source_op_f32(
                    &mut actual,
                    operation,
                    0,
                    &mut session,
                    0,
                    &mut control,
                )
            }
        };
        assert_eq!(
            result.unwrap_err(),
            EffectExecutionError::ImageScaleNotAdmitted {
                requirement: EffectImageScaleRequirement::Unit,
                actual_sampling: nonuniform(),
            }
        );
        assert_eq!(actual, before);
        assert_eq!(control.feedback_count(), 0);
        assert_eq!(counts.sized.load(Ordering::SeqCst), 0);
        assert_eq!(counts.prepared.load(Ordering::SeqCst), 0);
        assert_eq!(session.diagnostics().processor_instances, 0);
        assert_healthy(&key);
    }
}
