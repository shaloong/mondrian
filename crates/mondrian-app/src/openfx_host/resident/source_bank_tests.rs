//! Native calibration of the invocation-borrowed bank, not product temporal admission.
use super::*;
use mondrian_core::{
    effect_data::{EffectNode, EffectType},
    TimelineTime, WorkingColorSpace,
};
use mondrian_effects::*;
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex, OnceLock, Weak,
};

fn failure(e: impl std::fmt::Display) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "native_bank_fixture".into(),
        reason: e.to_string(),
    }
}
fn source(w: u32, h: u32, seed: f32) -> EffectFloatFrame {
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
#[derive(Debug, Default)]
struct Facts {
    child_created: AtomicUsize,
    child_dropped: AtomicUsize,
    child_calls: AtomicUsize,
    parent_created: AtomicUsize,
    parent_closed: AtomicBool,
    pass: AtomicUsize,
    cancellation: mondrian_core::ExecutionCancellationToken,
    requests: Mutex<Vec<(f64, usize)>>,
    native_owner: Mutex<Option<u64>>,
}
#[derive(Debug)]
struct GainFactory(Arc<Facts>);
#[derive(Debug)]
struct Gain(Arc<Facts>);
impl Drop for Gain {
    fn drop(&mut self) {
        self.0.child_dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl EffectFloatProcessorFactory for GainFactory {
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
        control: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        control.check()?;
        self.0.child_created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Gain(self.0.clone())))
    }
}
impl EffectFloatProcessorInstance for Gain {
    fn process(
        &mut self,
        frame: &mut EffectFloatFrame,
        params: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
        _: &mut EffectProcessorResources<'_>,
    ) -> Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        control.check()?;
        self.0.child_calls.fetch_add(1, Ordering::SeqCst);
        let gain = params["gain"].as_f64().unwrap() as f32;
        for pixel in frame.pixels_mut() {
            for lane in &mut pixel[..3] {
                *lane *= gain;
            }
        }
        Ok(None)
    }
}

#[derive(Debug)]
struct NativeFactory {
    helper: PathBuf,
    inspection: crate::openfx_adapter::OpenFxBinaryInspection,
    facts: Arc<Facts>,
    // Only the fixture bootstraps a capability after freezing its full program.
    // Weak avoids storing a program->factory->program ownership cycle.
    input: Weak<OnceLock<PreparedEffectInput>>,
}
#[derive(Debug)]
struct Native {
    session: OpenFxFilterSession,
    facts: Arc<Facts>,
    input: Weak<OnceLock<PreparedEffectInput>>,
}
impl Drop for Native {
    fn drop(&mut self) {
        self.session.shutdown().expect("original native owner must settle");
        assert_eq!(self.session.process.state, ProcessState::Settled);
        self.facts.parent_closed.store(true, Ordering::SeqCst);
    }
}
impl EffectFloatProcessorFactory for NativeFactory {
    fn alpha_contract(&self) -> EffectAlphaContract {
        EffectAlphaContract::NATIVE_FLOAT
    }
    fn feedback_budget_bytes(&self) -> usize {
        EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
    }
    fn storage(
        &self,
        r: EffectFloatProcessorSizing,
    ) -> mondrian_core::Result<EffectFloatProcessorStorage> {
        let native =
            OpenFxFilterSession::storage_for_context(r.width, r.height, OpenFxContext::Filter)
                .map_err(failure)?;
        let extra = std::mem::size_of::<Native>();
        EffectFloatProcessorStorage::new(
            native.active_peak_bytes() + extra,
            native.retained_bytes() + extra,
        )
    }
    fn prepare(
        &self,
        r: EffectFloatProcessorPreparation,
        control: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        let session = OpenFxFilterSession::prepare(
            &self.helper,
            &self.inspection,
            "org.mondrian.fixture.source-delivery",
            r.width,
            r.height,
            r.format,
            EffectProjectGeometry::new(r.width, r.height, mondrian_core::SampleAspectRatio::SQUARE)
                .map_err(failure)?,
            &mut || control.check().map_err(|_| OpenFxHostError::Canceled),
        )
        .map_err(failure)?;
        self.facts.parent_created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Native {
            session,
            facts: self.facts.clone(),
            input: self.input.clone(),
        }))
    }
}
struct Owner<'a, 'bank, 'callback> {
    input: &'a PreparedEffectInput,
    resources: &'a mut EffectProcessorResources<'bank>,
    control: &'a mut EffectProcessorControl<'callback>,
    facts: &'a Facts,
    expected: [[f32; 4]; 6],
    thread: thread::ThreadId,
}
impl RenderOwner for Owner<'_, '_, '_> {
    fn observe(
        &mut self,
        _: OpenFxRenderObservation<'_>,
    ) -> Result<EffectProcessorQuestionState, OpenFxHostError> {
        assert_eq!(self.thread, thread::current().id());
        self.control.check().map_err(|_| match self.control.stop_reason() {
            Some(EffectProcessorStopReason::DeadlineExpired) => OpenFxHostError::DeadlineExceeded,
            _ => OpenFxHostError::Canceled,
        })?;
        Ok(EffectProcessorQuestionState::Unavailable)
    }
    fn acquire_memory(&mut self, bytes: usize) -> Result<bool, OpenFxHostError> {
        self.observe(OpenFxRenderObservation::Checkpoint)?;
        Ok(self.resources.acquire_transient(bytes).is_ok())
    }
    fn resolves_input(&self) -> bool {
        true
    }
    fn resolve_input(&mut self, r: InputRequest) -> Result<ResolvedInput, OpenFxHostError> {
        self.observe(OpenFxRenderObservation::Checkpoint)?;
        self.facts.requests.lock().unwrap().push((r.time, r.held_native_bytes));
        let (time, w, h, x, y, gain, seed) = match r.time {
            -1.5 => (TimelineTime::new(-1, 16).unwrap(), 2, 3, -2, 3, 0.5, 1.0),
            2.25 => (TimelineTime::new(3, 32).unwrap(), 4, 1, 4, -3, 2.0, 2.0),
            _ => panic!("unadmitted native source time"),
        };
        let raw = source(w, h, seed + self.facts.pass.load(Ordering::SeqCst) as f32);
        // Source data and preparation are included in this admitted temporary
        // envelope before constructing the owned provider (tiny finite fixture).
        assert!(self.acquire_memory(r.held_native_bytes + 128 * 1024)?);
        let request = EffectTemporalExecutionRequest::new(
            19,
            EffectExecutionContinuity::Discontinuous,
            time,
            raw.extent(),
            raw.sampling(),
            raw.extent().full_frame_roi(),
            self.facts.cancellation.with_deadline(r.deadline),
        );
        let prepared = self.input.prepare_execution(&request, |_| Ok(0)).map_err(graph_error)?;
        let batch = prepared.demands().clone();
        assert_eq!(batch.requests().len(), 1);
        let input_request = batch.requests()[0];
        let pixels = raw.pixels().to_vec();
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"same-bank-finite-source/unit-square/rec709/raw/v1");
        fingerprint.update(r.time.to_le_bytes());
        fingerprint.update(w.to_le_bytes());
        fingerprint.update(h.to_le_bytes());
        fingerprint.update(bytemuck::cast_slice::<_, u8>(&pixels));
        let tile = EffectFrameTileF32::new(
            time,
            raw.extent(),
            raw.sampling(),
            raw.extent().full_frame_roi(),
            0,
            pixels,
        )
        .unwrap();
        let mut provider = PreparedTemporalFrameSet::prepare(
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint(
                fingerprint.finalize().into(),
            ),
            batch,
            [(input_request, tile)],
        )
        .unwrap();
        let output = self
            .resources
            .execute_prepared_input_f32(&prepared, &mut provider, r.held_native_bytes, self.control)
            .map_err(graph_error)?;
        assert!(output.peak_working_bytes() <= 128 * 1024);
        for (actual, raw) in output.tile().pixels().iter().zip(raw.pixels()) {
            assert_eq!(
                actual.map(f32::to_bits),
                [raw[0] * gain, raw[1] * gain, raw[2] * gain, 1.0].map(f32::to_bits)
            );
        }
        let pointer = output.tile().pixels().as_ptr();
        let pixels = output.into_tile().try_into_pixels().unwrap();
        assert_eq!(pointer, pixels.as_ptr());
        for (i, expected) in self.expected.iter_mut().enumerate() {
            let (index, factor) = if r.time == -1.5 {
                ((i + 1) % 6, 2.0)
            } else {
                ((i + 2) % 4, 3.0)
            };
            for lane in 0..4 {
                expected[lane] += factor * pixels[index][lane];
            }
        }
        Ok(ResolvedInput {
            frame: EffectFloatFrame::from_pixels(
                raw.extent(),
                raw.format(),
                raw.sampling(),
                pixels,
            )
            .unwrap(),
            x,
            y,
        })
    }
}
impl EffectFloatProcessorInstance for Native {
    fn process(
        &mut self,
        frame: &mut EffectFloatFrame,
        _: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
        resources: &mut EffectProcessorResources<'_>,
    ) -> Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        let binding = self.input.upgrade().unwrap();
        let deadline = control.deadline();
        let timing = OpenFxRenderTiming {
            frame: 0.25,
            frame_rate: 24.0,
            first_frame: -10.0,
            last_frame: 10.0,
        };
        let mut owner = Owner {
            input: binding.get().unwrap(),
            resources,
            control,
            facts: &self.facts,
            expected: frame.pixels().try_into().unwrap(),
            thread: thread::current().id(),
        };
        let result = self.session.render_with_owner(
            &timing,
            &BTreeMap::new(),
            frame,
            &[],
            deadline,
            EffectProcessorGuiAdmission::CLOSED,
            &mut owner,
        );
        let expected = owner.expected;
        control.check()?;
        let output = match result {
            Ok(o) => o,
            Err(OpenFxHostError::InputFailed(e)) => {
                return Err(EffectProcessorInvocationError::InputFailed(e))
            }
            Err(e) => return Err(failure(e).into()),
        };
        for (actual, expected) in output.pixels().iter().zip(expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
        let feedback = self.session.take_feedback().unwrap();
        let native_owner = feedback.snapshot.native_owner();
        let mut observed = self.facts.native_owner.lock().unwrap();
        assert_eq!(*observed.get_or_insert(native_owner), native_owner);
        *frame = output;
        Ok(Some(feedback))
    }
}

#[test]
#[ignore = "requires current ordinary worker and SourceDeliveryProbe; original bank calibration only"]
fn actual_native_source_requests_reenter_original_processor_bank() {
    const TEST: &str = "openfx_host::resident::source_input::bank_tests::actual_native_source_requests_reenter_original_processor_bank";
    if !super::super::super::shared_frame_tests::qualification_child(TEST, &[]) {
        return;
    }
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SOURCE_DELIVERY_BINARY").unwrap());
    let log = tempfile::NamedTempFile::new().unwrap();
    unsafe {
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_LOG", log.path());
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_THREADS", "1");
    }
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let facts = Arc::new(Facts::default());
    facts.requests.lock().unwrap().reserve_exact(8);
    let input = Arc::new(OnceLock::new());
    let child_key = format!(
        "fixture.bank-native-gain.{}",
        mondrian_core::EffectId::new()
    );
    let parent_key = format!(
        "fixture.bank-native-parent.{}",
        mondrian_core::EffectId::new()
    );
    let contract = EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_F32,
        determinism: EffectDeterminism::Nondeterministic,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        ..EffectExecutionContract::IDENTITY
    };
    register_effect_definition(EffectDefinition::new(&child_key, "Upstream gain", Default::default(), EffectColorDomainContract::SCENE_LINEAR).with_execution_contract(contract).with_custom_float_processor_factory(Arc::new(|_, c| Ok(Some(serde_json::json!({"gain": if c.time < TimelineTime::ZERO { 0.5 } else if c.time > TimelineTime::ZERO { 2.0 } else { 1.0 }})))), None, EffectCachePolicy::Uncacheable, Arc::new(GainFactory(facts.clone())))).unwrap();
    register_effect_definition(
        EffectDefinition::new(
            &parent_key,
            "Native bank consumer",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(contract)
        .with_custom_float_processor_factory(
            Arc::new(|_, c| {
                if c.time != TimelineTime::ZERO {
                    return Err(EffectGraphBuildError::InvalidGraph);
                }
                Ok(Some(serde_json::json!({})))
            }),
            None,
            EffectCachePolicy::Uncacheable,
            Arc::new(NativeFactory {
                helper,
                inspection,
                facts: facts.clone(),
                input: Arc::downgrade(&input),
            }),
        ),
    )
    .unwrap();
    let child = EffectNode::new(EffectType::External(child_key));
    let parent = EffectNode::new(EffectType::External(parent_key));
    let parent_id = parent.id;
    let program =
        PreparedEffectProgram::prepare(&[child, parent], &[], WorkingColorSpace::LinearRec709)
            .unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    input.set(program.input_for_stage(&graph.stage_bindings()[1]).unwrap()).unwrap();
    let native = OpenFxFilterSession::storage_for_context(3, 2, OpenFxContext::Filter).unwrap();
    let retained = native.retained_bytes() + std::mem::size_of::<Native>() + 128;
    // Admit one retained parent beside the existing cold-route reservation.
    // A guessed32MiB optional grant is smaller than this native recipe. The
    // test asks for reuse, so derive its grant from the actual declaration.
    let working = 2 * (native.active_peak_bytes() + std::mem::size_of::<Native>())
        + storage_bytes()
        + 128 * 1024
        + 2 * EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
        + EFFECT_PROCESSOR_FEEDBACK_SLOTS * std::mem::size_of::<EffectProcessorFeedbackReceipt>();
    let mut bank = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 2,
        max_processor_bytes: retained,
        ..EffectExecutionSessionConfig::uncached(working)
    });
    bank.bind_generation(19);
    let current = source(3, 2, 0.5);
    for pass in 0..4 {
        facts.pass.store(pass, Ordering::SeqCst);
        let token = facts.cancellation.clone();
        let mut checkpoint = || token.is_canceled().then_some(EffectProcessorStopReason::Canceled);
        let mut control = EffectProcessorControl::new(&mut checkpoint);
        bank.apply_compiled_rgba_f32(current.view(), &graph, 0, &mut control).unwrap();
        let receipts = control.take_feedback().unwrap();
        assert_eq!(receipts.iter().count(), 1);
        assert_eq!(receipts.iter().next().unwrap().effect_id, parent_id);
        assert_eq!(bank.diagnostics().generation, Some(19));
        // The bank deliberately keeps one configuration per occurrence.
        // Root3x2/raw -> historical2x3/coverage -> historical4x1/coverage
        // retires the obsolete upstream configuration at each transition.
        // A separate nested bank would leave the root child alive and fail
        // these exact create/drop counts; it cannot fake this proof of sharing.
        assert_eq!(bank.diagnostics().processor_instances, 2);
        assert_eq!(bank.diagnostics().processor_bytes, retained);
        assert_eq!(facts.child_created.load(Ordering::SeqCst), 3 * (pass + 1));
        assert_eq!(
            facts.child_dropped.load(Ordering::SeqCst),
            3 * (pass + 1) - 1
        );
        assert!(!facts.parent_closed.load(Ordering::SeqCst));
        assert_eq!(bank.diagnostics().cache_entries, 0);
    }
    assert_eq!(
        facts.child_created.load(Ordering::SeqCst),
        12,
        "all configuration changes use the original occurrence's retirement policy"
    );
    assert_eq!(facts.parent_created.load(Ordering::SeqCst), 1);
    assert_eq!(facts.child_calls.load(Ordering::SeqCst), 12);
    assert_eq!(
        facts.requests.lock().unwrap().iter().map(|r| r.0).collect::<Vec<_>>(),
        [-1.5, 2.25].repeat(4)
    );
    drop(bank);
    assert_eq!(facts.child_dropped.load(Ordering::SeqCst), 12);
    assert!(facts.parent_closed.load(Ordering::SeqCst));
    assert_eq!(Arc::strong_count(&input), 1);
    println!("Native same-bank PASS: four renders, one reused native parent, twelve exact upstream configuration creations/retirements, eight Source services, original Control feedback/generation, strict child shutdown");
}
