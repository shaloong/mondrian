use super::*;
use crate::{
    AudioKernelBackend, AudioParameterEvent, AudioProcessorAudioIo, AudioProcessorInputBus,
    AudioProcessorMainAndInputBuses, AudioProcessorTail, AudioRenderRequest,
};
use mondrian_core::{AudioSampleRate, ParameterId};
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
