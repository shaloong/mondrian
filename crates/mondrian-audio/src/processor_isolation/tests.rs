use super::*;
use crate::{
    AudioKernelBackend, AudioParameterEvent, AudioProcessorAudioIo, AudioProcessorInputBus,
    AudioProcessorInsertionPoint, AudioProcessorMainAndInputBuses, AudioProcessorOccurrence,
    AudioProcessorOccurrenceOwner, AudioProcessorTail, AudioRenderRequest,
};
use mondrian_core::{AudioSampleRate, ParameterId, ProgramOutputId};
use std::ops::Range;
use std::time::Duration;

const CHILD_TEST_NAME: &str = "processor_isolation::tests::isolated_worker_child_entry";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestBehavior {
    Normal,
    Abort,
    Hang,
    ContractDrift,
}

impl TestBehavior {
    fn payload(self) -> Vec<u8> {
        vec![match self {
            Self::Normal => 0,
            Self::Abort => 1,
            Self::Hang => 2,
            Self::ContractDrift => 3,
        }]
    }

    fn decode(payload: &[u8]) -> Result<Self, AudioProcessorHostError> {
        match payload {
            [0] => Ok(Self::Normal),
            [1] => Ok(Self::Abort),
            [2] => Ok(Self::Hang),
            [3] => Ok(Self::ContractDrift),
            _ => Err(AudioProcessorHostError::InstanceCreation(
                "unknown isolated test behavior".to_owned(),
            )),
        }
    }
}

struct TestWorkerFactory;

impl IsolatedAudioProcessorWorkerFactory for TestWorkerFactory {
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        let behavior = TestBehavior::decode(request.payload())?;
        let contract = if behavior == TestBehavior::ContractDrift {
            AudioProcessorExecutionContract::stateless()
        } else {
            request.plugin_execution_contract()
        };
        Ok(Box::new(TestWorker {
            behavior,
            contract,
            auxiliary: request.auxiliary_inputs().clone(),
            entered: false,
        }))
    }
}

struct TestWorker {
    behavior: TestBehavior,
    contract: AudioProcessorExecutionContract,
    auxiliary: AudioProcessorAuxiliaryInputContract,
    entered: bool,
}

impl IsolatedAudioProcessorWorker for TestWorker {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary.clone()
    }

    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        self.entered = true;
        Ok(())
    }

    fn process(
        &mut self,
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        match self.behavior {
            TestBehavior::Abort => std::process::abort(),
            TestBehavior::Hang => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            TestBehavior::Normal | TestBehavior::ContractDrift => {}
        }
        if self.contract.requires_state_entry() && !self.entered {
            return Err(AudioProcessorHostError::StateEntry(
                "test Worker was not entered".to_owned(),
            ));
        }
        let gain = block
            .parameter_events(0)
            .and_then(Iterator::last)
            .map_or(1.0_f32, |event| event.value as f32);
        if let Some((main, detector)) = block.main_and_auxiliary_input("detector") {
            for (sample, detector) in main.iter_mut().zip(detector) {
                *sample = (*sample + *detector) * gain;
            }
        } else {
            for sample in block.main_interleaved() {
                *sample *= gain;
            }
        }
        Ok(())
    }
}

#[test]
fn isolated_worker_child_entry() {
    if std::env::var_os(ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV).is_none() {
        return;
    }
    run_isolated_audio_processor_worker(&TestWorkerFactory).expect("run isolated test Worker");
}

#[test]
fn isolated_worker_transports_state_sidechain_parameters_and_pcm() {
    let parameter_id = ParameterId::new_static("mondrian.test.isolated-gain");
    let contract = stateful_contract();
    let auxiliary = AudioProcessorAuxiliaryInputContract {
        buses: vec![crate::AudioProcessorAuxiliaryInputBusContract {
            bus_key: "detector".to_owned(),
            channel_layout: AudioChannelLayout::Stereo,
        }],
    };
    let factory = prepare_test_factory(
        TestBehavior::Normal,
        contract,
        auxiliary,
        vec![parameter_id.clone()],
        Duration::from_secs(2),
    );
    assert!(factory.execution_contract().session_scratch_bytes() > 0);
    let mut processor = factory.create().expect("create isolated processor");
    processor.enter_state(100).expect("enter isolated state");
    let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: vec![0.5; 8] };
    let parameter_ids = [parameter_id];
    let ranges = [Range { start: 0, end: 2 }];
    let events = [
        AudioParameterEvent { sample_offset: 0, value: 2.0 },
        AudioParameterEvent { sample_offset: 2, value: 3.0 },
    ];
    let batch = AudioParameterEventBatch::new(100, 4, &parameter_ids, &ranges, &events);
    processor
        .process(test_context(100), &mut audio, batch)
        .expect("process isolated block");
    assert_eq!(audio.main, vec![4.5; 8]);
}

#[test]
#[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
fn installed_clap_reference_processes_through_isolated_worker() {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
    let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
    let state = 0.5_f32.to_le_bytes();
    let registration = probe_clap_plugin_registration(
        &helper,
        &plugin,
        "org.rust-audio.clack.gain",
        test_render_contract(),
        Some(&state),
    )
    .expect("probe installed gain contract");
    let payload = serde_json::to_vec(&serde_json::json!({
        "library_path": registration.library_path,
        "plugin_id": registration.plugin_id,
        "binary_sha256": registration.binary_sha256,
        "state": state,
        "parameters": registration.parameters,
        "parameter_ids": [1],
    }))
    .expect("CLAP worker payload");
    let parameter_id = ParameterId::new("clap.param.1").expect("volume parameter ID");
    let spec = IsolatedAudioProcessorWorkerSpec::new(
        helper,
        payload,
        registration.execution_contract,
        AudioProcessorAuxiliaryInputContract::default(),
        vec![parameter_id.clone()],
        test_render_contract(),
    )
    .expect("CLAP worker specification");
    let factory = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("prepare installed CLAP through child process");
    let mut processor = factory.create().expect("create CLAP worker instance");
    processor.enter_state(100).expect("enter CLAP state");
    let input = vec![0.125, -0.25, 0.5, -0.75, 0.2, -0.4, 0.8, -1.0];
    let mut audio = TestAudioIo { main: input.clone(), auxiliary: Vec::new() };
    let empty_ranges = [Range { start: 0, end: 0 }];
    processor
        .process(
            test_context(100),
            &mut audio,
            AudioParameterEventBatch::new(
                100,
                4,
                std::slice::from_ref(&parameter_id),
                &empty_ranges,
                &[],
            ),
        )
        .expect("process Clack gain through child process");
    for (actual, source) in audio.main.iter().zip(input) {
        assert!((actual - source * 0.5).abs() < 1.0e-6);
    }
    let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
    let event_ranges = [Range { start: 0, end: 1 }];
    processor
        .process(
            test_context(104),
            &mut audio,
            AudioParameterEventBatch::new(
                104,
                4,
                std::slice::from_ref(&parameter_id),
                &event_ranges,
                &[AudioParameterEvent { sample_offset: 0, value: 0.25 }],
            ),
        )
        .expect("send automated volume through isolated worker");
    assert!(audio.main.iter().all(|sample| (*sample - 0.25).abs() < 1.0e-6));
}

#[test]
#[ignore = "requires a built Mondrian executable and the VST3 SDK gain reference DLL"]
fn installed_vst3_reference_processes_through_isolated_worker() {
    let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
        .map(PathBuf::from)
        .expect("set MONDRIAN_VST3_TEST_HELPER to the app executable");
    let binary = std::env::var_os("MONDRIAN_VST3_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("set MONDRIAN_VST3_TEST_PLUGIN to the VST3 gain DLL");
    let descriptors =
        scan_vst3_binary_descriptors(&helper, &binary).expect("scan VST3 reference classes");
    assert_eq!(descriptors.len(), 1);
    let registration = probe_vst3_plugin_registration(
        &helper,
        &binary,
        &descriptors[0].class_id,
        test_render_contract(),
        None,
    )
    .expect("probe VST3 reference contract");
    assert_eq!(registration.parameters.len(), 1);
    let parameter = &registration.parameters[0];
    assert!(!parameter.read_only);
    let id = parameter.parameter_id().expect("VST3 stable parameter ID");
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary])
        .expect("discover VST3 reference catalog");
    let instance = catalog
        .create_instance(&descriptors[0].class_id, test_render_contract(), None)
        .expect("capture VST3 authoring instance");
    assert!(instance.parameters.contains_key(&id));
    let spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: instance.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            test_render_contract(),
        ))
        .expect("resolve VST3 author instance");
    let factory = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("prepare VST3 reference through child process");
    let mut processor = factory.create().expect("create VST3 worker instance");
    processor.enter_state(100).expect("enter VST3 state");
    let source = vec![0.125, -0.25, 0.5, -0.75, 0.2, -0.4, 0.8, -1.0];
    let mut audio = TestAudioIo { main: source.clone(), auxiliary: Vec::new() };
    let ranges = [Range { start: 0, end: 1 }];
    processor
        .process(
            test_context(100),
            &mut audio,
            AudioParameterEventBatch::new(
                100,
                4,
                std::slice::from_ref(&id),
                &ranges,
                &[AudioParameterEvent { sample_offset: 0, value: 0.5 }],
            ),
        )
        .expect("process automated VST3 gain through child process");
    for (actual, source) in audio.main.iter().zip(source) {
        assert!((actual - source * 0.5).abs() < 1.0e-6);
    }
    processor.enter_state(100).expect("reset VST3 continuity");
    let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
    processor
        .process(
            test_context(100),
            &mut audio,
            AudioParameterEventBatch::new(
                100,
                4,
                std::slice::from_ref(&id),
                &ranges,
                &[AudioParameterEvent { sample_offset: 0, value: 0.25 }],
            ),
        )
        .expect("process after VST3 state re-entry");
    assert!(audio.main.iter().all(|sample| (*sample - 0.25).abs() < 1.0e-6));
    processor.enter_state(100).expect("reset before invalid VST3 event");
    let mut audio = TestAudioIo { main: vec![0.75; 8], auxiliary: Vec::new() };
    assert!(processor
        .process(
            test_context(100),
            &mut audio,
            AudioParameterEventBatch::new(
                100,
                4,
                std::slice::from_ref(&id),
                &ranges,
                &[AudioParameterEvent { sample_offset: 0, value: 1.5 }],
            ),
        )
        .is_err());
    assert_eq!(audio.main, vec![0.75; 8]);

    let offline_render = AudioRenderContract {
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let offline_instance = catalog
        .create_instance(&descriptors[0].class_id, offline_render, None)
        .expect("capture offline VST3 instance");
    let offline_spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: offline_instance.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &offline_instance.definition,
            &offline_instance.parameters,
            offline_instance.opaque_state.as_ref().map(AsRef::as_ref),
            offline_render,
        ))
        .expect("resolve offline VST3 instance");
    let offline_factory =
        IsolatedAudioProcessorFactory::prepare(offline_spec).expect("prepare offline VST3 worker");
    let mut offline = offline_factory.create().expect("create offline VST3 instance");
    offline.enter_state(100).expect("enter offline VST3 continuity");
    let mut audio = TestAudioIo {
        main: vec![0.5, -0.25, 1.0, -0.75, 0.4, -0.2, 0.8, -0.6],
        auxiliary: Vec::new(),
    };
    let original = audio.main.clone();
    let offline_context = AudioProcessorProcessContext::new(
        AudioRenderRequest { start_sample: 100, frames: 4 },
        AudioSampleRate::new(48_000).expect("sample rate"),
        AudioChannelLayout::Stereo,
        AudioProcessingMode::Offline,
        AudioKernelBackend::ScalarReference,
    );
    offline
        .process(
            offline_context,
            &mut audio,
            AudioParameterEventBatch::new(
                100,
                4,
                std::slice::from_ref(&id),
                &ranges,
                &[AudioParameterEvent { sample_offset: 0, value: 0.5 }],
            ),
        )
        .expect("process offline VST3 gain");
    for (actual, source) in audio.main.iter().zip(original) {
        assert!((actual - source * 0.5).abs() < 1.0e-6);
    }
}

#[test]
#[ignore = "requires MONDRIAN_VST3_TEST_HELPER and MONDRIAN_VST3_STATEFUL_TEST_PLUGIN"]
fn stateful_vst3_snapshot_round_trips_and_restores_isolated_worker_audio() {
    use super::vst3_discovery::probe_vst3_plugin_registration_with_state;

    let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
        .map(PathBuf::from)
        .expect("built Mondrian helper executable");
    let plugin = std::env::var_os("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("built stateful VST3 Gain fixture");
    let descriptors =
        scan_vst3_binary_descriptors(&helper, &plugin).expect("scan stateful Gain class");
    assert_eq!(descriptors.len(), 1);
    let class_id = &descriptors[0].class_id;
    let authored_state = stateful_gain_snapshot(0.37);
    let probe = probe_vst3_plugin_registration_with_state(
        &helper,
        &plugin,
        class_id,
        test_render_contract(),
        Some(&authored_state),
        true,
    )
    .expect("restore and recapture nondefault state");
    assert_eq!(
        probe.captured_state.as_deref(),
        Some(authored_state.as_slice())
    );
    assert_eq!(probe.current_values.len(), 1);
    assert!((probe.current_values[0].1 - 0.37).abs() < 1.0e-9);
    let id = probe.registration.parameters[0]
        .parameter_id()
        .expect("stateful VST3 stable parameter ID");

    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("discover stateful Gain");
    let instance = catalog
        .create_instance(class_id, test_render_contract(), probe.captured_state)
        .expect("capture stateful authoring instance");
    let spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: instance.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            test_render_contract(),
        ))
        .expect("prepare exact stateful revision");
    let factory = IsolatedAudioProcessorFactory::prepare(spec).expect("prepare worker");
    let mut worker = factory.create().expect("create isolated stateful Gain");
    let empty_range = [Range { start: 0, end: 0 }];
    for _ in 0..2 {
        worker.enter_state(100).expect("enter saved state");
        let source = vec![0.5, -0.25, 1.0, -0.75, 0.4, -0.2, 0.8, -0.6];
        let mut audio = TestAudioIo { main: source.clone(), auxiliary: Vec::new() };
        worker
            .process(
                test_context(100),
                &mut audio,
                AudioParameterEventBatch::new(100, 4, std::slice::from_ref(&id), &empty_range, &[]),
            )
            .expect("process restored state without parameter events");
        for (actual, original) in audio.main.iter().zip(source) {
            assert!((actual - original * 0.37).abs() < 1.0e-6);
        }
    }
}

fn stateful_gain_snapshot(gain: f64) -> Vec<u8> {
    let mut component = Vec::with_capacity(16);
    component.extend_from_slice(b"MSG1");
    component.extend_from_slice(&1_u32.to_le_bytes());
    component.extend_from_slice(&gain.to_le_bytes());
    let mut snapshot = Vec::with_capacity(28 + 2 * component.len());
    snapshot.extend_from_slice(b"VST3HOST_STATE\0\0");
    snapshot.extend_from_slice(&1_u32.to_le_bytes());
    snapshot.extend_from_slice(&(component.len() as u32).to_le_bytes());
    snapshot.extend_from_slice(&(component.len() as u32).to_le_bytes());
    snapshot.extend_from_slice(&component);
    snapshot.extend_from_slice(&component);
    snapshot
}

#[test]
fn isolated_worker_abort_poisoning_does_not_terminate_parent() {
    let factory = prepare_test_factory(
        TestBehavior::Abort,
        AudioProcessorExecutionContract::stateless(),
        AudioProcessorAuxiliaryInputContract::default(),
        Vec::new(),
        Duration::from_millis(250),
    );
    let mut processor = factory.create().expect("create crash Worker");
    let error = process_empty_parameter_block(processor.as_mut()).expect_err("Worker must abort");
    assert!(
        matches!(
            error,
            AudioProcessorHostError::WorkerFailed { .. }
                | AudioProcessorHostError::WorkerDeadlineExceeded { .. }
        ),
        "unexpected abort classification: {error:?}"
    );
    assert_eq!(
        process_empty_parameter_block(processor.as_mut()).expect_err("instance remains poisoned"),
        AudioProcessorHostError::PoisonedInstance
    );
    let mut replacement = factory.create().expect("create replacement Worker");
    assert!(process_empty_parameter_block(replacement.as_mut()).is_err());
}

#[test]
fn isolated_worker_contract_drift_is_rejected_during_preparation() {
    let spec = test_spec(
        TestBehavior::ContractDrift,
        stateful_contract(),
        AudioProcessorAuxiliaryInputContract::default(),
        Vec::new(),
        Duration::from_millis(250),
    );
    let error = IsolatedAudioProcessorFactory::prepare(spec)
        .expect_err("Worker contract drift must fail preparation");
    assert!(matches!(
        error,
        AudioProcessorHostError::WorkerFailed { operation: "startup", .. }
    ));
}

#[test]
fn isolated_worker_hang_is_terminated_at_the_block_deadline() {
    let factory = prepare_test_factory(
        TestBehavior::Hang,
        AudioProcessorExecutionContract::stateless(),
        AudioProcessorAuxiliaryInputContract::default(),
        Vec::new(),
        Duration::from_millis(50),
    );
    let mut processor = factory.create().expect("create hanging Worker");
    let started = std::time::Instant::now();
    let error =
        process_empty_parameter_block(processor.as_mut()).expect_err("Worker must time out");
    assert_eq!(
        error,
        AudioProcessorHostError::WorkerDeadlineExceeded { operation: "block processing" }
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        process_empty_parameter_block(processor.as_mut()).expect_err("instance remains poisoned"),
        AudioProcessorHostError::PoisonedInstance
    );
}

fn prepare_test_factory(
    behavior: TestBehavior,
    contract: AudioProcessorExecutionContract,
    auxiliary: AudioProcessorAuxiliaryInputContract,
    parameter_ids: Vec<ParameterId>,
    operation_timeout: Duration,
) -> Arc<IsolatedAudioProcessorFactory> {
    IsolatedAudioProcessorFactory::prepare(test_spec(
        behavior,
        contract,
        auxiliary,
        parameter_ids,
        operation_timeout,
    ))
    .expect("prepare isolated factory")
}

fn test_spec(
    behavior: TestBehavior,
    contract: AudioProcessorExecutionContract,
    auxiliary: AudioProcessorAuxiliaryInputContract,
    parameter_ids: Vec<ParameterId>,
    operation_timeout: Duration,
) -> IsolatedAudioProcessorWorkerSpec {
    let executable = std::env::current_exe().expect("current test executable");
    IsolatedAudioProcessorWorkerSpec::new(
        executable,
        behavior.payload(),
        contract,
        auxiliary,
        parameter_ids,
        test_render_contract(),
    )
    .expect("valid Worker spec")
    .with_helper_arguments(vec![
        OsString::from(CHILD_TEST_NAME),
        OsString::from("--exact"),
        OsString::from("--nocapture"),
    ])
    .with_dispatch_argument(None)
    .with_deadlines(Duration::from_secs(5), operation_timeout)
    .expect("valid Worker deadlines")
}

fn stateful_contract() -> AudioProcessorExecutionContract {
    AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, true, true, true, 128)
        .expect("valid stateful contract")
}

fn test_render_contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}

fn test_context(start_sample: i64) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { start_sample, frames: 4 },
        AudioSampleRate::new(48_000).expect("test sample rate"),
        AudioChannelLayout::Stereo,
        AudioProcessingMode::Realtime,
        AudioKernelBackend::ScalarReference,
    )
}

fn process_empty_parameter_block(
    processor: &mut dyn AudioProcessor,
) -> Result<(), AudioProcessorHostError> {
    let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
    processor.process(
        test_context(0),
        &mut audio,
        AudioParameterEventBatch::new(0, 4, &[], &[], &[]),
    )
}

struct TestAudioIo {
    main: Vec<f32>,
    auxiliary: Vec<f32>,
}

impl AudioProcessorAudioIo for TestAudioIo {
    fn main_layout(&self) -> AudioChannelLayout {
        AudioChannelLayout::Stereo
    }

    fn frames(&self) -> usize {
        self.main.len() / 2
    }

    fn main_interleaved(&mut self) -> &mut [f32] {
        &mut self.main
    }

    fn auxiliary_input(&self, bus_key: &str) -> Option<AudioProcessorInputBus<'_>> {
        (bus_key == "detector" && !self.auxiliary.is_empty()).then_some(AudioProcessorInputBus {
            bus_key: "detector",
            channel_layout: AudioChannelLayout::Stereo,
            frames: self.auxiliary.len() / 2,
            interleaved: &self.auxiliary,
        })
    }

    fn main_and_auxiliary_input(
        &mut self,
        bus_key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        if bus_key != "detector" || self.auxiliary.is_empty() {
            return None;
        }
        Some(AudioProcessorMainAndInputBuses {
            main_layout: AudioChannelLayout::Stereo,
            frames: self.main.len() / 2,
            main_interleaved: &mut self.main,
            auxiliary: AudioProcessorInputBus {
                bus_key: "detector",
                channel_layout: AudioChannelLayout::Stereo,
                frames: self.auxiliary.len() / 2,
                interleaved: &self.auxiliary,
            },
        })
    }
}
