//! Actual installed factory + invocation-borrowed service; no weak bootstrap.
use super::*;
use mondrian_core::{TimelineTime, TimelineTimeRange, WorkingColorSpace};
use mondrian_effects::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

#[derive(Debug, Default)]
struct Facts {
    pass: AtomicUsize,
    created: AtomicUsize,
    dropped: AtomicUsize,
    dsp: AtomicUsize,
    requests: Mutex<Vec<TimelineTime>>,
    cancellation: mondrian_core::ExecutionCancellationToken,
}
#[derive(Debug)]
struct GainFactory(Arc<Facts>);
#[derive(Debug)]
struct Gain(Arc<Facts>);
impl Drop for Gain {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
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
        c: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        c.check()?;
        self.0.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Gain(self.0.clone())))
    }
}
impl EffectFloatProcessorInstance for Gain {
    fn process(
        &mut self,
        frame: &mut EffectFloatFrame,
        p: &serde_json::Value,
        _: i64,
        control: &mut EffectProcessorControl<'_>,
        _: &mut EffectProcessorResources<'_>,
    ) -> Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError> {
        control.check()?;
        for pixel in frame.pixels_mut() {
            for lane in &mut pixel[..3] {
                *lane *= p["gain"].as_f64().unwrap() as f32;
            }
        }
        self.0.dsp.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
fn raw(w: u32, h: u32, seed: f32) -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(w, h),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
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
struct Source<'a> {
    program: &'a PreparedEffectProgram,
    facts: &'a Facts,
}
impl EffectProcessorInputService for Source<'_> {
    fn prepare_input(
        &mut self,
        stage: &CompiledEffectStageBinding,
        input: EffectProcessorInputRequest,
        admit: &mut dyn FnMut(usize) -> Result<(), EffectTemporalExecutionError>,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<PreparedEffectProcessorInput, EffectTemporalExecutionError> {
        control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
        assert_eq!(stage.stage_index(), 1, "actual installed native node input");
        self.facts.requests.lock().unwrap().push(input.time);
        let (w, h, origin, seed) = if input.time == TimelineTime::new(-1, 16).unwrap() {
            (2, 3, [-2, 3], 1.0)
        } else if input.time == TimelineTime::new(3, 32).unwrap() {
            (4, 1, [4, -3], 2.0)
        } else {
            panic!("outside finite calibration source");
        };
        let extent = EffectFrameExtent::new(w, h);
        let request = EffectTemporalExecutionRequest::new(
            19,
            EffectExecutionContinuity::Continuous,
            input.time,
            extent,
            input.sampling,
            extent.full_frame_roi(),
            self.facts.cancellation.with_deadline(input.deadline),
        );
        let execution = self
            .program
            .input_for_stage(stage)
            .expect("borrow original program, no factory capture")
            .prepare_execution(&request, |_| Ok(0))?;
        assert_eq!(
            execution.graph().stage_bindings().len(),
            1,
            "only the actual upstream gain"
        );
        admit(execution.demands().coverage_bytes())?;
        let data = raw(w, h, seed + self.facts.pass.load(Ordering::SeqCst) as f32);
        let r = execution.demands().requests()[0];
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"owned-normal-factory-source/unit-square/linear709/coverage/v1");
        fingerprint.update(input.time.numerator().to_le_bytes());
        fingerprint.update(input.time.denominator().to_le_bytes());
        fingerprint.update(bytemuck::cast_slice::<_, u8>(data.pixels()));
        let frames = PreparedTemporalFrameSet::prepare(
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint(
                fingerprint.finalize().into(),
            ),
            execution.demands().clone(),
            [(
                r,
                EffectFrameTileF32::new(
                    input.time,
                    extent,
                    input.sampling,
                    extent.full_frame_roi(),
                    0,
                    data.pixels().to_vec(),
                )
                .unwrap(),
            )],
        )
        .unwrap();
        Ok(PreparedEffectProcessorInput { execution, frames, origin })
    }
}

#[test]
#[ignore = "requires SourceDeliveryProbe/current compatible worker; normal factory finite source calibration"]
fn normal_openfx_factory_borrows_original_input_service_without_factory_program_cycle() {
    const TEST:&str="openfx_effect::input_owner_native_tests::normal_openfx_factory_borrows_original_input_service_without_factory_program_cycle";
    if !crate::openfx_host::shared_frame_tests::qualification_child(TEST, &[]) {
        return;
    }
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SOURCE_DELIVERY_BINARY").unwrap());
    let log = tempfile::NamedTempFile::new().unwrap();
    // This exact qualification has its own process; no concurrent test can
    // observe these environment values. The child exits after bank teardown.
    unsafe {
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_LOG", log.path());
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_THREADS", "1");
    }
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let plugin = inspection
        .unique_plugin("org.mondrian.fixture.source-delivery")
        .unwrap()
        .clone();
    let native_type =
        register_selected_openfx_filter(&helper, &inspection, &plugin.identifier).unwrap();
    let facts = Arc::new(Facts::default());
    let gain_type = EffectType::External(format!(
        "test.normal-native-gain.{}",
        mondrian_core::EffectId::new()
    ));
    register_effect_definition(EffectDefinition::new(gain_type.key(),"Original gain",Default::default(),EffectColorDomainContract::SCENE_LINEAR)
        .with_execution_contract(EffectExecutionContract { execution_modes:EffectExecutionModes::CPU_F32,
            roi_propagation:EffectRoiPropagation::UnknownRequiresFullFrame,..EffectExecutionContract::IDENTITY })
        .with_custom_float_processor_factory(Arc::new(|_,c| Ok(Some(serde_json::json!({"gain":if c.time<TimelineTime::ZERO {0.5} else if c.time>TimelineTime::ZERO {2.0} else {1.0}})))),
            None,EffectCachePolicy::Deterministic,Arc::new(GainFactory(facts.clone())))).unwrap();
    let gain = EffectNode::new(gain_type);
    let native = EffectNode::new(native_type);
    let native_id = native.id;
    let context = EffectFrameContext::new(
        mondrian_core::Rational::new(24, 1),
        EffectProjectGeometry::new(3, 2, mondrian_core::SampleAspectRatio::SQUARE).unwrap(),
        // The probe's -1.5-frame demand must be inside the authored source
        // range. Keep the production out-of-range refusal intact.
        TimelineTimeRange::new(
            TimelineTime::new(-10, 24).unwrap(),
            TimelineTime::new(20, 24).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(context.available_frames().unwrap(), (-10, 9));
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[gain, native],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        context,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let graph = program.evaluate(TimelineTime::new(1, 96).unwrap()).unwrap();
    // Size the actual production recipe; this is an ample finite calibration
    // grant, not a guessed minimum or a measurement of SDK heap/RSS.
    let recipe = OpenFxProcessorFactory {
        helper: helper.clone(),
        inspection: inspection.clone(),
        plugin,
        context: OpenFxContext::Filter,
    };
    let storage = recipe
        .storage(EffectFloatProcessorSizing {
            effect_id: native_id,
            width: 3,
            height: 2,
            alpha_formats: mondrian_core::ColorFrameAlphaFormats::only(
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
        })
        .unwrap();
    let retained = storage.retained_bytes() + 128;
    let working = 4 * storage.active_peak_bytes()
        + 4 * recipe.feedback_budget_bytes()
        + 128 * 1024
        + EFFECT_PROCESSOR_FEEDBACK_SLOTS * std::mem::size_of::<EffectProcessorFeedbackReceipt>();
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 2,
        max_processor_bytes: retained,
        ..EffectExecutionSessionConfig::uncached(working)
    });
    session.bind_generation(19);
    let mut identity = None;
    for pass in 0..4 {
        facts.pass.store(pass, Ordering::SeqCst);
        let current = raw(3, 2, 0.5 + pass as f32);
        let mut source = Source { program: &program, facts: &facts };
        let mut checkpoint =
            || facts.cancellation.is_canceled().then_some(EffectProcessorStopReason::Canceled);
        let mut control =
            EffectProcessorControl::new(&mut checkpoint).with_input_service(&mut source);
        let output = session
            .apply_compiled_rgba_f32(current.view(), &graph, 0, &mut control)
            .unwrap_or_else(|error| {
                panic!(
                    "normal factory failed: {error:?}; source requests={:?}; upstream DSP={}; actions={}",
                    facts.requests.lock().unwrap(),
                    facts.dsp.load(Ordering::SeqCst),
                    std::fs::read_to_string(log.path()).unwrap_or_default(),
                )
            });
        let past = raw(2, 3, 1.0 + pass as f32);
        let future = raw(4, 1, 2.0 + pass as f32);
        for (i, pixel) in output.pixels().iter().enumerate() {
            let mut expected = current.pixels()[i];
            for component in &mut expected[..3] {
                *component *= 2.0;
            }
            for (input, index, factor, gain) in [
                (&past, (i + 1) % 6, 2.0, 0.5),
                (&future, (i + 2) % 4, 3.0, 2.0),
            ] {
                for (lane, component) in expected.iter_mut().enumerate() {
                    *component +=
                        factor * input.pixels()[index][lane] * if lane < 3 { gain } else { 1.0 };
                }
            }
            assert_eq!(pixel.map(f32::to_bits), expected.map(f32::to_bits));
        }
        assert_eq!(
            output.alpha(),
            mondrian_core::ColorFrameAlpha::StraightFloat,
            "native alpha6 must remain Float"
        );
        let receipts = control.take_feedback().unwrap();
        let receipt = receipts.iter().next().unwrap();
        assert_eq!(receipts.iter().count(), 1);
        assert_eq!(receipt.effect_id, native_id);
        let actual = (
            receipt.feedback.owner,
            receipt.feedback.snapshot.native_owner(),
        );
        assert_eq!(*identity.get_or_insert(actual), actual);
        assert_eq!(receipt.feedback.snapshot.invocation(), pass as u64 + 1);
        assert_eq!(session.diagnostics().processor_instances, 2);
        assert_eq!(session.diagnostics().processor_bytes, retained);
        assert_eq!(session.diagnostics().generation, Some(19));
        assert_eq!(session.diagnostics().cache_entries, 0);
        assert_eq!(facts.created.load(Ordering::SeqCst), 3 * (pass + 1));
        assert_eq!(facts.dropped.load(Ordering::SeqCst), 3 * (pass + 1) - 1);
    }
    assert_eq!(facts.dsp.load(Ordering::SeqCst), 12);
    assert_eq!(
        *facts.requests.lock().unwrap(),
        [
            TimelineTime::new(-1, 16).unwrap(),
            TimelineTime::new(3, 32).unwrap()
        ]
        .repeat(4)
    );
    drop(session);
    assert_eq!(facts.dropped.load(Ordering::SeqCst), 12);
    let actions = std::fs::read_to_string(log.path()).unwrap();
    assert_eq!(
        actions.lines().filter(|l| *l == "OfxImageEffectActionRender").count(),
        4
    );
    assert_eq!(actions.lines().filter(|l| *l == "sources-held").count(), 4);
    println!("Normal installed OpenFX factory PASS: borrowed original service, four same-owner renders, eight exact source services, twelve upstream configuration retirements, complete RGBA including Float alpha6; no factory/program bootstrap");
}
