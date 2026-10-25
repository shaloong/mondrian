use super::*;
use mondrian_core::effect_data::{EffectNode, EffectType};
use mondrian_core::{
    ExecutionCancellationToken, Keyframe, PropertyValue, TimelineTime, WorkingColorSpace,
};
use mondrian_effects::*;
use sha2::{Digest, Sha256};

fn signal(w: u32, h: u32, seed: f32) -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(w, h),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        (0..w * h)
            .map(|i| {
                let v = seed + i as f32 / 16.0;
                [v, -2.0 * v, 4.0 * v, 1.0]
            })
            .collect(),
    )
    .unwrap()
}

#[derive(Clone, Copy)]
enum Failure {
    None,
    GraphBudget,
    Dependency,
    Cancel,
    CopyBudget,
}

struct GraphOwner {
    thread: thread::ThreadId,
    cancellation: ExecutionCancellationToken,
    graph: EffectExecutionSession,
    input: PreparedEffectInput,
    generation: u64,
    seed: f32,
    requests: Vec<(f64, usize)>,
    grants: Vec<usize>,
    failure: Failure,
    copy_ready: bool,
    expected_native: [[f32; 4]; 6],
}

impl GraphOwner {
    fn new(pass: u64, failure: Failure) -> Self {
        let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
        let parameter = EffectType::BasicCorrection.parameter_id("exposure").unwrap();
        let path = effect
            .properties
            .iter()
            .find(|(_, p)| p.descriptor.parameter_id() == &parameter)
            .unwrap()
            .1
            .descriptor
            .path
            .clone();
        for (time, stops) in [
            (TimelineTime::new(-1, 12).unwrap(), -1.0),
            (TimelineTime::new(-1, 16).unwrap(), -1.0),
            (TimelineTime::new(1, 96).unwrap(), 0.0),
            (TimelineTime::new(7, 96).unwrap(), 1.0),
            (TimelineTime::new(3, 32).unwrap(), 1.0),
        ] {
            effect
                .properties
                .set_keyframe(&path, Keyframe::linear(time, PropertyValue::Float(stops)))
                .unwrap();
        }
        // Use the existing executor's actual finite temporal contract, not a
        // single-frame graph disguised as temporal execution. Its upstream
        // animated correction is evaluated at both exact source instants.
        let temporal_type = EffectType::External(format!(
            "fixture.source-input.{}",
            mondrian_core::EffectId::new()
        ));
        let offset = TimelineTime::new(1, 48).unwrap();
        register_effect_definition(
            EffectDefinition::new(
                temporal_type.key(),
                "Finite upstream graph calibration",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent {
                    past: EffectTemporalSpan::Finite(offset),
                    future: EffectTemporalSpan::None,
                },
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(std::sync::Arc::new(move |_, _, graph| {
                graph.append_unary(EffectRenderOp::TemporalFrameBlend {
                    sample_offset: TimelineTime::ZERO.checked_sub(offset).unwrap(),
                    mix: 0.25,
                });
                Ok(())
            })),
        )
        .unwrap();
        let waiting_type = EffectType::External(format!(
            "fixture.waiting-input.{}",
            mondrian_core::EffectId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                waiting_type.key(),
                "Waiting/downstream boundary",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(std::sync::Arc::new(|_, context, graph| {
                if context.time != TimelineTime::ZERO {
                    return Err(EffectGraphBuildError::InvalidGraph);
                }
                graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.75 });
                Ok(())
            })),
        )
        .unwrap();
        let program = PreparedEffectProgram::prepare(
            &[
                effect,
                EffectNode::new(temporal_type),
                EffectNode::new(waiting_type.clone()),
                EffectNode::new(waiting_type),
            ],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .unwrap();
        let graph = program.evaluate(TimelineTime::ZERO).unwrap();
        let input = program.input_for_stage(&graph.stage_bindings()[2]).unwrap();
        Self {
            thread: thread::current().id(),
            cancellation: ExecutionCancellationToken::new(),
            graph: EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(64 * 1024)),
            input,
            generation: pass + 1,
            seed: 1.0 + pass as f32,
            requests: Vec::new(),
            grants: Vec::new(),
            failure,
            copy_ready: false,
            expected_native: signal(3, 2, 0.5).pixels().try_into().unwrap(),
        }
    }
}

impl RenderOwner for GraphOwner {
    fn observe(
        &mut self,
        _: OpenFxRenderObservation<'_>,
    ) -> Result<EffectProcessorQuestionState, OpenFxHostError> {
        assert_eq!(
            self.thread,
            thread::current().id(),
            "only the original graph owner may execute callbacks"
        );
        if self.cancellation.is_canceled() {
            Err(OpenFxHostError::Canceled)
        } else {
            Ok(EffectProcessorQuestionState::Unavailable)
        }
    }
    fn acquire_memory(&mut self, bytes: usize) -> Result<bool, OpenFxHostError> {
        self.observe(OpenFxRenderObservation::Checkpoint)?;
        self.grants.push(bytes);
        Ok(bytes <= storage_bytes() + 320 + 128 * 1024
            && !(matches!(self.failure, Failure::CopyBudget) && self.copy_ready))
    }
    fn resolves_input(&self) -> bool {
        true
    }
    fn resolve_input(&mut self, request: InputRequest) -> Result<ResolvedInput, OpenFxHostError> {
        self.observe(OpenFxRenderObservation::Checkpoint)?;
        self.requests.push((request.time, request.held_native_bytes));
        if matches!(self.failure, Failure::Cancel) {
            self.cancellation.cancel();
            return Err(delivery_error(
                "dependency stop must defer to the owning generation",
            ));
        }
        if matches!(self.failure, Failure::Dependency) {
            return Err(OpenFxHostError::InputFailed(Box::new(
                EffectExecutionError::InputUnavailable {
                    reason: "original upstream dependency failed".into(),
                },
            )));
        }
        // Exact mapping for this finite 24fps fixture; no nearest-frame rounding.
        let (time, w, h, x, y, seed) = match request.time {
            -1.5 => (TimelineTime::new(-1, 16).unwrap(), 2, 3, -2, 3, self.seed),
            2.25 => (
                TimelineTime::new(3, 32).unwrap(),
                4,
                1,
                4,
                -3,
                self.seed + 1.0,
            ),
            other => panic!("unrequested source time {other}"),
        };
        assert!(request.held_native_bytes >= storage_bytes() + 320);
        let graph_budget = if matches!(self.failure, Failure::GraphBudget) {
            0
        } else {
            64 * 1024
        };
        // The one owner admits held native backing + upstream working storage,
        // before creating pixels. No allocation authority runs in the child.
        assert!(self.acquire_memory(request.held_native_bytes + 64 * 1024)?);
        self.graph.reconfigure(EffectExecutionSessionConfig::uncached(graph_budget));
        let extent = EffectFrameExtent::new(w, h);
        let temporal = EffectTemporalExecutionRequest::new(
            self.generation,
            EffectExecutionContinuity::Discontinuous,
            time,
            extent,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            extent.full_frame_roi(),
            self.cancellation.with_deadline(request.deadline),
        );
        let prepared = self.input.prepare_execution(&temporal, |_| Ok(0)).map_err(graph_error)?;
        let batch = prepared.demands().clone();
        assert_eq!(batch.requests().len(), 2);
        let previous = time.checked_sub(TimelineTime::new(1, 48).unwrap()).unwrap();
        let raw = signal(w, h, seed);
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"source-input-fixture/unit-square/linear-rec709/coverage/v1");
        fingerprint.update(w.to_le_bytes());
        fingerprint.update(h.to_le_bytes());
        fingerprint.update(seed.to_le_bytes());
        fingerprint.update(request.time.to_le_bytes());
        // This finite source mapping is closed: current signal(seed), previous
        // signal(seed - 0.5), identical geometry/coverage and explicit seed0.
        let resolved = batch
            .requests()
            .iter()
            .copied()
            .map(|source_request| {
                let pixels = if source_request.time() == time {
                    raw.pixels().to_vec()
                } else {
                    assert_eq!(source_request.time(), previous);
                    signal(w, h, seed - 0.5).pixels().to_vec()
                };
                fingerprint.update(bytemuck::cast_slice::<_, u8>(&pixels));
                let tile = EffectFrameTileF32::new(
                    source_request.time(),
                    extent,
                    raw.sampling(),
                    extent.full_frame_roi(),
                    0,
                    pixels,
                )
                .unwrap();
                (source_request, tile)
            })
            .collect::<Vec<_>>();
        let identity = EffectTemporalSourceIdentity::from_complete_semantic_fingerprint(
            fingerprint.finalize().into(),
        );
        let mut provider = PreparedTemporalFrameSet::prepare(identity, batch, resolved).unwrap();
        let output = self
            .graph
            .execute_prepared_temporal_f32(&prepared, &mut provider)
            .map_err(graph_error)?;
        assert!(output.peak_working_bytes() <= graph_budget);
        let pointer = output.tile().pixels().as_ptr();
        drop(provider);
        let pixels = output
            .into_tile()
            .try_into_pixels()
            .expect("uncached output must transfer without a hidden copy");
        assert_eq!(pointer, pixels.as_ptr());
        // Preserve exact graph values for a separate bitwise IPC/native oracle.
        for (i, expected) in self.expected_native.iter_mut().enumerate() {
            let (index, factor) = if request.time == -1.5 {
                ((i + 1) % 6, 2.0)
            } else {
                ((i + 2) % 4, 3.0)
            };
            for lane in 0..4 {
                expected[lane] += factor * pixels[index][lane];
            }
        }
        self.copy_ready = true;
        Ok(ResolvedInput {
            frame: EffectFloatFrame::from_pixels(extent, raw.format(), raw.sampling(), pixels)
                .unwrap(),
            x,
            y,
        })
    }
}

#[test]
fn bounded_graph_owner_evaluates_exact_animated_temporal_inputs() {
    let mut owner = GraphOwner::new(0, Failure::None);
    for (time, width, height, seed, scale) in [(-1.5, 2, 3, 0.875, 0.5), (2.25, 4, 1, 1.875, 2.0)] {
        let output = owner
            .resolve_input(InputRequest {
                time,
                held_native_bytes: storage_bytes() + 320,
                deadline: Instant::now() + Duration::from_secs(3),
            })
            .unwrap();
        let raw = signal(width, height, seed);
        for (actual, raw) in output.frame.pixels().iter().zip(raw.pixels()) {
            let expected = [raw[0] * scale, raw[1] * scale, raw[2] * scale, 1.0];
            // Primary correction evaluates pivot/luma/saturation in Float32
            // (~12 operations/sample); the algebraic reference omits those
            // intermediate roundings. This bound applies only to DSP math.
            // Native transport/publication below still compares exact bits.
            for lane in 0..4 {
                let bound = 32.0 * f32::EPSILON * expected[lane].abs().max(1.0);
                assert!(
                    (actual[lane] - expected[lane]).abs() <= bound,
                    "time {time}, lane {lane}: {} != {} within {bound}",
                    actual[lane],
                    expected[lane]
                );
            }
        }
    }
}

#[test]
fn input_work_expiry_preserves_parent_stop_and_typed_dependency_provenance() {
    let mut owner = GraphOwner::new(0, Failure::None);
    assert!(matches!(check_input_owner(&mut owner, Instant::now()),
        Err(OpenFxHostError::InputFailed(error)) if matches!(*error,
            EffectExecutionError::CustomProcessorStopped { reason: EffectProcessorStopReason::DeadlineExpired })));
    owner.cancellation.cancel();
    assert!(matches!(
        check_input_owner(&mut owner, Instant::now()),
        Err(OpenFxHostError::Canceled)
    ));
    let error = graph_error(EffectTemporalExecutionError::CustomProcessor(
        EffectExecutionError::InputUnavailable { reason: "original".into() },
    ));
    assert!(
        matches!(error, OpenFxHostError::InputFailed(error) if matches!(*error,
        EffectExecutionError::InputUnavailable { ref reason } if reason == "original"))
    );
}

#[test]
#[ignore = "requires fresh normal IPC17 worker and SourceDeliveryProbe; finite graph/transport calibration only"]
fn actual_resident_resolves_late_sources_on_original_bounded_graph_owner() {
    const TEST: &str = "openfx_host::resident::source_input::tests::actual_resident_resolves_late_sources_on_original_bounded_graph_owner";
    if !super::super::super::shared_frame_tests::qualification_child(TEST, &[]) {
        return;
    }
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SOURCE_DELIVERY_BINARY").unwrap());
    let log = tempfile::NamedTempFile::new().unwrap();
    // Single isolated qualification process, serial workers. Concurrent native
    // readers do not access these environment variables after fixture admission.
    unsafe {
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_LOG", log.path());
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_THREADS", "1");
    }
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let current = signal(3, 2, 0.5);
    let timing = OpenFxRenderTiming {
        frame: 0.25,
        frame_rate: 24.0,
        first_frame: -10.0,
        last_frame: 10.0,
    };
    let prepare = || {
        OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "org.mondrian.fixture.source-delivery",
            3,
            2,
            current.format(),
            EffectProjectGeometry::new(3, 2, mondrian_core::SampleAspectRatio::SQUARE).unwrap(),
            &mut || Ok(()),
        )
        .unwrap()
    };
    let mut session = prepare();
    let worker = session.worker_process_id();
    let mut native_owner = None;
    for pass in 0..4 {
        let mut owner = GraphOwner::new(pass, Failure::None);
        let output = session
            .render_with_owner(
                &timing,
                &BTreeMap::new(),
                &current,
                &[],
                None,
                EffectProcessorGuiAdmission::CLOSED,
                &mut owner,
            )
            .unwrap();
        assert_eq!(session.worker_process_id(), worker);
        assert!(session.pending_inputs.is_empty());
        let feedback = session.take_feedback().unwrap();
        let actual_owner = feedback.snapshot.native_owner();
        assert_eq!(*native_owner.get_or_insert(actual_owner), actual_owner);
        assert_eq!(
            owner.requests.iter().map(|r| r.0).collect::<Vec<_>>(),
            [-1.5, 2.25],
            "eight readers coalesce two exact dependencies"
        );
        assert_eq!(owner.requests[0].1, storage_bytes() + 320);
        assert_eq!(owner.requests[1].1, storage_bytes() + 320 + 96);
        for i in 0..6 {
            assert_eq!(
                output.pixels()[i].map(f32::to_bits),
                owner.expected_native[i].map(f32::to_bits)
            );
        }
    }
    session.shutdown().unwrap();
    assert_eq!(session.process.state, ProcessState::Settled);
    for failure in [
        Failure::GraphBudget,
        Failure::Dependency,
        Failure::Cancel,
        Failure::CopyBudget,
    ] {
        let mut session = prepare();
        let mut owner = GraphOwner::new(0, failure);
        let error = session
            .render_with_owner(
                &timing,
                &BTreeMap::new(),
                &current,
                &[],
                None,
                EffectProcessorGuiAdmission::CLOSED,
                &mut owner,
            )
            .unwrap_err();
        match failure {
            Failure::Cancel => assert!(matches!(error, OpenFxHostError::Canceled)),
            Failure::GraphBudget => assert!(matches!(error,
                OpenFxHostError::InputFailed(error) if matches!(&*error, EffectExecutionError::TemporalInput { source }
                    if matches!(**source, EffectTemporalExecutionError::WorkingSetBudgetExceeded { budget_bytes: 0, .. })))),
            Failure::Dependency => assert!(matches!(error,
                OpenFxHostError::InputFailed(error) if matches!(&*error, EffectExecutionError::InputUnavailable { reason }
                    if reason == "original upstream dependency failed"))),
            Failure::CopyBudget => assert!(matches!(error, OpenFxHostError::InputFailed(_))),
            Failure::None => unreachable!(),
        }
        assert_eq!(session.process.state, ProcessState::Settled);
        assert!(session.pending_inputs.is_empty());
    }
    assert_eq!(current.pixels(), signal(3, 2, 0.5).pixels());
    println!("Late Source RPC PASS: 4 exact full-RGBA graph renders/same native owner; 8 readers coalesced; held Output/current leases; graph/dependency/cancel/copy failures settle without publication");
}
