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
    ObservedPending,
}

impl TestBehavior {
    fn payload(self) -> Vec<u8> {
        vec![match self {
            Self::Normal => 0,
            Self::Abort => 1,
            Self::Hang => 2,
            Self::ContractDrift => 3,
            Self::ObservedPending => 5,
        }]
    }

    fn decode(payload: &[u8]) -> Result<Self, AudioProcessorHostError> {
        match payload {
            [0] => Ok(Self::Normal),
            [1] => Ok(Self::Abort),
            [2] => Ok(Self::Hang),
            [3] => Ok(Self::ContractDrift),
            [5] => Ok(Self::ObservedPending),
            _ => Err(AudioProcessorHostError::InstanceCreation(
                "unknown isolated test behavior".to_owned(),
            )),
        }
    }
}

// A test-only extension of the original child entry. Each actual preparation
// creates one PID marker; optional rendezvous retains an observation handle
// before Ready refusal, without changing the production launch/owner path.
#[derive(serde::Serialize, serde::Deserialize)]
struct ColdTestPreparation {
    directory: PathBuf,
    input_names: Vec<crate::AudioProcessorInputName>,
    wait_for_observer: bool,
}

impl ColdTestPreparation {
    fn record(&self) -> Result<(), AudioProcessorHostError> {
        let pid = std::process::id();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.directory.join(format!("{pid}.prepared")))
            .map_err(|error| invalid_contract(&format!("record test preparation: {error}")))?;
        if self.wait_for_observer {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !self.directory.join(format!("{pid}.observed")).exists() {
                if std::time::Instant::now() >= deadline {
                    return Err(invalid_contract("test preparation observer did not arrive"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }
}

struct TestWorkerFactory;

impl IsolatedAudioProcessorWorkerFactory for TestWorkerFactory {
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        let observation = request
            .payload()
            .strip_prefix(&[4])
            .map(serde_json::from_slice::<ColdTestPreparation>)
            .transpose()
            .map_err(|error| invalid_contract(&format!("test preparation payload: {error}")))?;
        let (behavior, input_names) = if let Some(observation) = observation {
            observation.record()?;
            (TestBehavior::Normal, observation.input_names)
        } else {
            (TestBehavior::decode(request.payload())?, Vec::new())
        };
        let contract = if behavior == TestBehavior::ContractDrift {
            AudioProcessorExecutionContract::stateless()
        } else {
            request.plugin_execution_contract()
        };
        Ok(Box::new(TestWorker {
            behavior,
            contract,
            auxiliary: request.auxiliary_inputs().clone(),
            input_names,
            musical_time_reference: request.musical_time_reference(),
            entered: false,
            pending_reads: std::cell::Cell::new(0),
        }))
    }
}

struct TestWorker {
    behavior: TestBehavior,
    contract: AudioProcessorExecutionContract,
    auxiliary: AudioProcessorAuxiliaryInputContract,
    input_names: Vec<crate::AudioProcessorInputName>,
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    entered: bool,
    pending_reads: std::cell::Cell<u32>,
}

impl IsolatedAudioProcessorWorker for TestWorker {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary.clone()
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        self.entered = true;
        Ok(())
    }

    fn pending_reprepare(&self) -> Option<crate::NativeObservedRequest> {
        if self.behavior != TestBehavior::ObservedPending || self.pending_reads.get() == 0 {
            return None;
        }
        let step = self.pending_reads.get();
        self.pending_reads.set(step + 1);
        let request = mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 };
        match step {
            1 | 2 => Some(crate::NativeObservedRequest::Cohort(
                NativeRequestCohort::new(100 + step, request).unwrap(),
            )),
            4 => Some(crate::NativeObservedRequest::RequestOnly(request)),
            _ => None,
        }
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
            TestBehavior::Normal | TestBehavior::ContractDrift | TestBehavior::ObservedPending => {}
        }
        if self.contract.requires_state_entry() && !self.entered {
            return Err(AudioProcessorHostError::StateEntry(
                "test Worker was not entered".to_owned(),
            ));
        }
        if block.musical_time_reference() != self.musical_time_reference {
            return Err(invalid_contract(
                "test worker music differs from prepared request",
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
        if self.behavior == TestBehavior::ObservedPending && self.pending_reads.get() == 0 {
            self.pending_reads.set(1);
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

fn cold_input_names() -> Vec<crate::AudioProcessorInputName> {
    [("detector", "Detector"), ("monitor", "Monitor")]
        .into_iter()
        .map(|(bus_key, name)| crate::AudioProcessorInputName {
            bus_key: bus_key.to_owned(),
            name: name.to_owned(),
        })
        .collect()
}

fn cold_test_spec(
    directory: &std::path::Path,
    actual_names: Vec<crate::AudioProcessorInputName>,
    wait_for_observer: bool,
) -> IsolatedAudioProcessorWorkerSpec {
    let auxiliary = AudioProcessorAuxiliaryInputContract {
        buses: ["detector", "monitor"]
            .into_iter()
            .map(|key| crate::AudioProcessorAuxiliaryInputBusContract {
                bus_key: key.to_owned(),
                channel_layout: AudioChannelLayout::Stereo,
            })
            .collect(),
    };
    let mut spec = test_spec(
        TestBehavior::Normal,
        stateful_contract(),
        auxiliary,
        Vec::new(),
        Duration::from_secs(2),
    );
    spec.preparation_payload = vec![4];
    spec.preparation_payload.extend(
        serde_json::to_vec(&ColdTestPreparation {
            directory: directory.to_owned(),
            input_names: actual_names,
            wait_for_observer,
        })
        .unwrap(),
    );
    spec
}

fn cold_prepared_pids(directory: &std::path::Path) -> Vec<u32> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter_map(|name| name.to_str().unwrap().strip_suffix(".prepared").map(str::to_owned))
        .map(|pid| pid.parse().unwrap())
        .collect()
}

#[test]
fn native_probe_evidence_consumes_names_once_and_keeps_static_validation() {
    let directory = tempfile::tempdir().unwrap();
    let spec = cold_test_spec(directory.path(), cold_input_names(), false);
    for names in [
        vec![crate::AudioProcessorInputName { bus_key: "absent".into(), name: "Valid".into() }],
        vec![cold_input_names()[0].clone(), cold_input_names()[0].clone()],
        vec![crate::AudioProcessorInputName { bus_key: "detector".into(), name: "\n".into() }],
    ] {
        assert!(spec.clone().with_native_probe_input_names(names).is_err());
    }
    let mut names = Vec::with_capacity(7);
    names.extend(cold_input_names());
    let pointer = names.as_ptr();
    let capacity = names.capacity();
    let evidenced = spec.clone().with_native_probe_input_names(names).unwrap();
    let mut invalid = evidenced.clone();
    invalid.render_contract.max_block_frames = usize::MAX;
    assert!(IsolatedAudioProcessorFactory::prepare(invalid).is_err());
    let mut invalid = evidenced.clone();
    invalid.plugin_execution_contract = AudioProcessorExecutionContract::new(
        0,
        AudioProcessorTail::None,
        true,
        true,
        true,
        usize::MAX,
    )
    .unwrap();
    assert!(IsolatedAudioProcessorFactory::prepare(invalid).is_err());
    assert!(cold_prepared_pids(directory.path()).is_empty());
    let factory = IsolatedAudioProcessorFactory::prepare(evidenced).unwrap();
    assert_eq!(factory.input_names.as_ptr(), pointer);
    assert_eq!(factory.input_names.capacity(), capacity);
    assert!(matches!(
        factory.spec.preparation_evidence,
        PreparationEvidence::Unprobed
    ));
    assert!(cold_prepared_pids(directory.path()).is_empty());
    assert_eq!(
        factory.execution_contract,
        IsolatedAudioProcessorFactory::requested_execution_contract(&spec, factory.layout).unwrap()
    );
    let inventory = factory_storage_inventory(&factory);
    let names_owner = inventory
        .iter()
        .filter(|storage| storage.address() == pointer as usize)
        .collect::<Vec<_>>();
    assert_eq!(names_owner.len(), 1);
    assert_eq!(
        names_owner[0].layout().size(),
        capacity * size_of::<crate::AudioProcessorInputName>()
    );
    let erased: Arc<dyn AudioProcessorFactory> = factory;
    let observed = crate::AudioProcessorFactoryStorageBorrow::arc_payload(&erased);
    assert_eq!(
        observed.layout(),
        std::alloc::Layout::new::<IsolatedAudioProcessorFactory>()
    );
}

#[test]
fn native_probe_factory_starts_one_worker_and_unprobed_keeps_validation_worker() {
    for native_probed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut actual_names = cold_input_names();
        actual_names.reverse();
        let mut spec = cold_test_spec(directory.path(), actual_names.clone(), false);
        if native_probed {
            spec = spec.with_native_probe_input_names(cold_input_names()).unwrap();
        }
        let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
        let prepared = cold_prepared_pids(directory.path());
        assert_eq!(prepared.len(), usize::from(!native_probed));
        let processor = factory.create().expect("Ready accepts equivalent keyed name order");
        let created = cold_prepared_pids(directory.path());
        assert_eq!(created.len(), prepared.len() + 1);
        assert_eq!(
            created.iter().collect::<std::collections::BTreeSet<_>>().len(),
            created.len()
        );
        assert_eq!(
            factory.input_names,
            if native_probed {
                cold_input_names()
            } else {
                actual_names
            }
        );
        drop(processor);
        assert_eq!(cold_prepared_pids(directory.path()).len(), created.len());
    }
}

#[cfg(windows)]
#[test]
fn native_probe_ready_name_drift_preserves_startup_error_and_reaps_original_child() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    let mut changed = cold_input_names();
    changed[0].name = "Changed detector".into();
    for actual_names in [changed, vec![cold_input_names()[0].clone()]] {
        let directory = tempfile::tempdir().unwrap();
        let spec = cold_test_spec(directory.path(), actual_names, true)
            .with_native_probe_input_names(cold_input_names())
            .unwrap();
        let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
        assert!(cold_prepared_pids(directory.path()).is_empty());
        std::thread::scope(|scope| {
            let creation = scope.spawn(|| match factory.create() {
                Ok(_) => panic!("presentation drift must refuse before publishing an instance"),
                Err(error) => error,
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let pid = loop {
                let prepared = cold_prepared_pids(directory.path());
                if let [pid] = prepared.as_slice() {
                    break *pid;
                }
                assert!(prepared.is_empty());
                assert!(
                    std::time::Instant::now() < deadline,
                    "actual child did not prepare"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            // SAFETY: The test child remains in its bounded pre-Ready rendezvous.
            // This handle only observes that exact process; ProcessOwner retains
            // all kill/reap authority. Adopt the checked nonnull handle once.
            let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            assert!(
                !raw.is_null(),
                "retain actual child observation: {}",
                std::io::Error::last_os_error()
            );
            let observed = unsafe { OwnedHandle::from_raw_handle(raw) };
            std::fs::write(directory.path().join(format!("{pid}.observed")), []).unwrap();
            let error = creation.join().unwrap();
            assert!(
                matches!(error, AudioProcessorHostError::WorkerFailed { operation: "startup", detail }
                if detail == "Worker input presentation metadata changed since preparation")
            );
            // SAFETY: This same-process observation handle remains live. Ready
            // refusal has already completed the original owner's cleanup.
            assert_eq!(
                unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
                WAIT_OBJECT_0
            );
            assert_eq!(cold_prepared_pids(directory.path()), [pid]);
        });
    }
}

#[test]
fn native_probe_seed_rebind_preserves_name_snapshot_without_rearming_evidence() {
    let mut original = pure_seed_factory(NativeConfigurationFormat::ClapState, Some(&[1, 2]));
    original.spec.auxiliary_inputs.buses =
        cold_test_spec(std::path::Path::new("unused"), Vec::new(), false)
            .auxiliary_inputs
            .buses;
    original.input_names = cold_input_names();
    let next = original
        .rebind_configuration_seed(NativeConfigurationFormat::ClapState, &[3, 4])
        .unwrap();
    assert_eq!(next.input_names, original.input_names);
    assert_ne!(next.input_names.as_ptr(), original.input_names.as_ptr());
    assert!(matches!(
        original.spec.preparation_evidence,
        PreparationEvidence::Unprobed
    ));
    assert!(matches!(
        next.spec.preparation_evidence,
        PreparationEvidence::Unprobed
    ));
    assert_eq!(next.execution_contract, original.execution_contract);
    assert_eq!(next.layout, original.layout);
    for factory in [&original, &next] {
        let inventory = factory_storage_inventory(factory);
        for (address, bytes) in [
            (
                factory.input_names.as_ptr() as usize,
                factory.input_names.capacity() * size_of::<crate::AudioProcessorInputName>(),
            ),
            (
                factory.input_names[0].name.as_ptr() as usize,
                factory.input_names[0].name.capacity(),
            ),
        ] {
            let owner =
                inventory.iter().filter(|owner| owner.address() == address).collect::<Vec<_>>();
            assert_eq!(owner.len(), 1);
            assert_eq!(owner[0].layout().size(), bytes);
        }
    }
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
    assert_installed_clap_gain(
        "MONDRIAN_CLAP_TEST_PLUGIN",
        "org.rust-audio.clack.gain",
        false,
        AudioProcessingMode::Realtime,
    );
}

#[test]
#[ignore = "requires a built Mondrian executable and the owned sample-accurate CLAP gain DLL"]
fn installed_clap_sample_gain_preserves_event_ranges() {
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        assert_installed_clap_gain(
            "MONDRIAN_CLAP_SAMPLE_TEST_PLUGIN",
            "org.mondrian.fixture.sample-gain",
            true,
            mode,
        );
    }
}

#[test]
#[ignore = "requires current product and owned CLAP sidechain native fixture"]
fn installed_clap_sidechain_preserves_native_buses_state_delay_and_partitions() {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current packaged worker");
    let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("owned sidechain fixture");
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("discover native sidechain DLL");
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(
        catalog.descriptors()[0].plugin_id,
        "org.mondrian.fixture.sidechain"
    );
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        for port_mode in 0..=3 {
            let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
            let mut state = vec![port_mode];
            state.extend(0.5_f32.to_le_bytes());
            let instance = catalog
                .create_instance(
                    "org.mondrian.fixture.sidechain",
                    render,
                    Some(state.clone()),
                )
                .expect("capture restored native port configuration");
            assert!(instance.parameters.is_empty());
            assert_eq!(
                instance.opaque_state.as_ref().map(AsRef::as_ref),
                Some(state.as_slice())
            );
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
                    render,
                ))
                .expect("resolve restored port metadata");
            let offline_quality = port_mode == 3 && mode == AudioProcessingMode::Offline;
            let delay = if offline_quality { 5 } else { 3 };
            let gain = if offline_quality { 0.75 } else { 0.5 };
            let expected_ids: &[u32] = match port_mode {
                0 | 3 => &[11, 7],
                1 => &[7, 11],
                2 => &[11],
                _ => unreachable!("bounded fixture state"),
            };
            assert_eq!(
                spec.plugin_execution_contract.realtime_capable(),
                mode == AudioProcessingMode::Realtime
            );
            assert_eq!(
                spec.plugin_execution_contract.offline_capable(),
                mode == AudioProcessingMode::Offline
            );
            assert_eq!(
                spec.auxiliary_inputs
                    .buses
                    .iter()
                    .map(|bus| bus.bus_key.as_str())
                    .collect::<Vec<_>>(),
                expected_ids
                    .iter()
                    .map(|id| format!("clap.audio-input.{id}"))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                spec.plugin_execution_contract.algorithmic_latency_frames(),
                delay
            );
            assert_eq!(
                spec.plugin_execution_contract.tail(),
                AudioProcessorTail::Finite(delay)
            );
            for partitions in [&[4, 4, 4, 2][..], &[3, 2, 1, 4, 4][..]] {
                let factory = IsolatedAudioProcessorFactory::prepare(spec.clone())
                    .expect("prepare native sidechain worker");
                let expected_names = if port_mode == 2 {
                    Vec::new()
                } else {
                    expected_ids
                        .iter()
                        .map(|id| crate::AudioProcessorInputName {
                            bus_key: format!("clap.audio-input.{id}"),
                            name: if port_mode == 1 || *id == 11 {
                                "压缩探测 🙂"
                            } else {
                                "备用探测"
                            }
                            .into(),
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    factory.auxiliary_input_names(),
                    expected_names,
                    "Restored native metadata must preserve keys and Unicode text"
                );
                let mut processor = factory.create().expect("create isolated instance");
                processor.enter_state(100).expect("enter native continuity");
                let mut output = Vec::new();
                let mut offset = 0;
                for &frames in partitions {
                    let mut io = NativeSidechainAudioIo {
                        main: fixture_sidechain_samples(0, offset, frames),
                        inputs: expected_ids
                            .iter()
                            .map(|id| {
                                (
                                    format!("clap.audio-input.{id}"),
                                    fixture_sidechain_samples(*id, offset, frames),
                                )
                            })
                            .collect(),
                    };
                    let start = 100 + offset as i64;
                    let context = AudioProcessorProcessContext::new(
                        AudioRenderRequest { start_sample: start, frames },
                        AudioSampleRate::new(48_000).expect("sample rate"),
                        AudioChannelLayout::Stereo,
                        mode,
                        AudioKernelBackend::ScalarReference,
                    );
                    processor
                        .process(
                            context,
                            &mut io,
                            AudioParameterEventBatch::new(start, frames, &[], &[], &[]),
                        )
                        .expect("process independent native auxiliary PCM");
                    output.extend(io.main);
                    offset += frames;
                }
                for (frame, actual) in output.chunks_exact(2).enumerate() {
                    for (channel, &value) in actual.iter().enumerate() {
                        let expected = if frame < delay {
                            0.0
                        } else {
                            let sample = frame - delay;
                            let main = fixture_sidechain_samples(0, sample, 1)[channel];
                            let first = fixture_sidechain_samples(11, sample, 1)[channel];
                            let second = if port_mode == 2 {
                                0.0
                            } else {
                                fixture_sidechain_samples(7, sample, 1)[channel]
                            };
                            (2.0 * main + 3.0 * first + 5.0 * second) * gain
                        };
                        assert!((value - expected).abs() <= 1e-6,
                            "{mode:?}, ports{port_mode}, partitions{partitions:?}, frame{frame}, channel{channel}: {value} != {expected}");
                    }
                }
                processor.enter_state(0).expect("reset native delay");
                let mut io = NativeSidechainAudioIo {
                    main: vec![0.25; 8],
                    inputs: expected_ids
                        .iter()
                        .map(|id| (format!("clap.audio-input.{id}"), vec![0.0; 8]))
                        .collect(),
                };
                let context = AudioProcessorProcessContext::new(
                    AudioRenderRequest { start_sample: 0, frames: 4 },
                    AudioSampleRate::new(48_000).expect("sample rate"),
                    AudioChannelLayout::Stereo,
                    mode,
                    AudioKernelBackend::ScalarReference,
                );
                processor
                    .process(
                        context,
                        &mut io,
                        AudioParameterEventBatch::new(0, 4, &[], &[], &[]),
                    )
                    .expect("silent auxiliary input following active native signal");
                let expected_reset = if delay == 5 {
                    [0.0; 8]
                } else {
                    [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.25, 0.25]
                };
                assert_eq!(io.main, expected_reset);
            }
            if port_mode == 0 {
                let mut stale = spec.clone();
                let mut payload: serde_json::Value =
                    serde_json::from_slice(&stale.preparation_payload).expect("worker payload");
                let mut changed_state = vec![1_u8];
                changed_state.extend(0.5_f32.to_le_bytes());
                payload["state"] = serde_json::json!(changed_state);
                stale.preparation_payload =
                    serde_json::to_vec(&payload).expect("changed-state payload");
                let stale = IsolatedAudioProcessorFactory::prepare(stale)
                    .expect("probe evidence permits metadata-only factory preparation");
                let error = stale
                    .create()
                    .err()
                    .expect("changed native order must refuse actual instance admission");
                assert_eq!(
                    error,
                    AudioProcessorHostError::WorkerFailed {
                        operation: "startup",
                        detail: "invalid audio processor execution contract: CLAP audio ports changed since contract probe".to_owned(),
                    }
                );
            }
        }
    }
}

#[test]
#[ignore = "requires current product and owned CLAP render/thread-check fixture"]
fn installed_clap_render_refuses_hard_realtime_and_declined_offline() {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current worker");
    let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("owned render fixture");
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("native catalog");
    for state_mode in [4, 5] {
        let mut state = vec![state_mode];
        state.extend(0.5_f32.to_le_bytes());
        let realtime = AudioRenderContract {
            processing_mode: AudioProcessingMode::Realtime,
            ..test_render_contract()
        };
        let offline = AudioRenderContract {
            processing_mode: AudioProcessingMode::Offline,
            ..test_render_contract()
        };
        catalog
            .create_instance(
                "org.mondrian.fixture.sidechain",
                realtime,
                Some(state.clone()),
            )
            .expect("realtime remains supported");
        assert!(
            catalog
                .create_instance("org.mondrian.fixture.sidechain", offline, Some(state))
                .is_err(),
            "state{state_mode}: unavailable offline mode must be rejected before author capture"
        );
    }
}

#[test]
#[ignore = "requires current product and owned VST3 sidechain native fixture"]
fn installed_vst3_sidechain_preserves_native_buses_state_delay_and_partitions() {
    let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current packaged worker");
    let plugin = std::env::var_os("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("owned sidechain fixture");
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("discover native sidechain DLL");
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].name, "Mondrian Sidechain Fixture");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        for port_mode in 0..=2 {
            let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
            let state = vst3_sidechain_snapshot(0.5, port_mode);
            let mut instance = catalog
                .create_instance(
                    &catalog.descriptors()[0].class_id,
                    render,
                    Some(state.clone()),
                )
                .expect("capture restored native port configuration");
            // Static restored state is the signal oracle; automation has its own native gate.
            instance.parameters.clear();
            assert_eq!(
                instance.opaque_state.as_ref().map(AsRef::as_ref),
                Some(state.as_slice())
            );
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
                    render,
                ))
                .expect("resolve restored port metadata");
            let expected_ids: &[u32] = if port_mode == 2 { &[1] } else { &[1, 2] };
            assert_eq!(
                spec.auxiliary_inputs
                    .buses
                    .iter()
                    .map(|bus| bus.bus_key.as_str())
                    .collect::<Vec<_>>(),
                expected_ids
                    .iter()
                    .map(|id| format!("vst3.audio-input.{id}"))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                spec.plugin_execution_contract.algorithmic_latency_frames(),
                3
            );
            assert_eq!(
                spec.plugin_execution_contract.tail(),
                AudioProcessorTail::Finite(3)
            );
            for partitions in [&[4, 4, 4, 2][..], &[3, 2, 1, 4, 4][..]] {
                let factory = IsolatedAudioProcessorFactory::prepare(spec.clone())
                    .expect("prepare native sidechain worker");
                let expected_names = if port_mode == 2 {
                    Vec::new()
                } else {
                    expected_ids
                        .iter()
                        .map(|id| crate::AudioProcessorInputName {
                            bus_key: format!("vst3.audio-input.{id}"),
                            name: if port_mode == 1 || *id == 1 {
                                "压缩探测 🙂"
                            } else {
                                "备用探测"
                            }
                            .into(),
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    factory.auxiliary_input_names(),
                    expected_names,
                    "Restored native metadata must preserve keys and Unicode text"
                );
                let mut processor = factory.create().expect("create isolated instance");
                processor.enter_state(100).expect("enter native continuity");
                let mut output = Vec::new();
                let mut offset = 0;
                for &frames in partitions {
                    let mut io = NativeSidechainAudioIo {
                        main: fixture_sidechain_samples(0, offset, frames),
                        inputs: expected_ids
                            .iter()
                            .map(|id| {
                                (
                                    format!("vst3.audio-input.{id}"),
                                    vst3_sidechain_samples(*id, offset, frames),
                                )
                            })
                            .collect(),
                    };
                    let start = 100 + offset as i64;
                    let context = AudioProcessorProcessContext::new(
                        AudioRenderRequest { start_sample: start, frames },
                        AudioSampleRate::new(48_000).expect("sample rate"),
                        AudioChannelLayout::Stereo,
                        mode,
                        AudioKernelBackend::ScalarReference,
                    );
                    processor
                        .process(
                            context,
                            &mut io,
                            AudioParameterEventBatch::new(start, frames, &[], &[], &[]),
                        )
                        .expect("process independent native auxiliary PCM");
                    output.extend(io.main);
                    offset += frames;
                }
                for (frame, actual) in output.chunks_exact(2).enumerate() {
                    for (channel, &value) in actual.iter().enumerate() {
                        let expected = if frame < 3 {
                            0.0
                        } else {
                            let sample = frame - 3;
                            let main = fixture_sidechain_samples(0, sample, 1)[channel];
                            let first = fixture_sidechain_samples(11, sample, 1)[channel];
                            let second = if port_mode == 2 {
                                0.0
                            } else {
                                fixture_sidechain_samples(7, sample, 1)[channel]
                            };
                            let (first_weight, second_weight) = if port_mode == 1 {
                                (5.0, 3.0)
                            } else {
                                (3.0, 5.0)
                            };
                            (2.0 * main + first_weight * first + second_weight * second) * 0.5
                        };
                        assert!((value - expected).abs() <= 1e-6,
                            "{mode:?}, ports{port_mode}, partitions{partitions:?}, frame{frame}, channel{channel}: {value} != {expected}");
                    }
                }
                processor.enter_state(0).expect("reset native delay");
                let mut io = NativeSidechainAudioIo {
                    main: vec![0.25; 8],
                    inputs: expected_ids
                        .iter()
                        .map(|id| (format!("vst3.audio-input.{id}"), vec![0.0; 8]))
                        .collect(),
                };
                let context = AudioProcessorProcessContext::new(
                    AudioRenderRequest { start_sample: 0, frames: 4 },
                    AudioSampleRate::new(48_000).expect("sample rate"),
                    AudioChannelLayout::Stereo,
                    mode,
                    AudioKernelBackend::ScalarReference,
                );
                processor
                    .process(
                        context,
                        &mut io,
                        AudioParameterEventBatch::new(0, 4, &[], &[], &[]),
                    )
                    .expect("silent auxiliary input following active native signal");
                assert_eq!(io.main, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.25, 0.25]);
            }
            if port_mode == 0 {
                let mut stale = spec.clone();
                let mut payload: serde_json::Value =
                    serde_json::from_slice(&stale.preparation_payload).expect("worker payload");
                let changed_state = vst3_sidechain_snapshot(0.5, 2);
                payload["state"] = serde_json::json!(changed_state);
                stale.preparation_payload =
                    serde_json::to_vec(&payload).expect("changed-state payload");
                let stale = IsolatedAudioProcessorFactory::prepare(stale)
                    .expect("probe evidence permits metadata-only factory preparation");
                let error = stale
                    .create()
                    .err()
                    .expect("changed restored native count must refuse actual instance admission");
                assert_eq!(
                    error,
                    AudioProcessorHostError::WorkerFailed {
                        operation: "startup",
                        detail: "invalid audio processor execution contract: VST3 auxiliary bus contract changed since probe".to_owned(),
                    }
                );
            }
        }
    }
}

fn vst3_sidechain_samples(index: u32, start: usize, frames: usize) -> Vec<f32> {
    fixture_sidechain_samples(if index == 1 { 11 } else { 7 }, start, frames)
}

fn vst3_sidechain_snapshot(gain: f64, mode: u32) -> Vec<u8> {
    let mut component = Vec::with_capacity(20);
    component.extend_from_slice(b"MSS1");
    component.extend_from_slice(&1_u32.to_le_bytes());
    component.extend_from_slice(&gain.to_le_bytes());
    component.extend_from_slice(&mode.to_le_bytes());
    let mut snapshot = Vec::with_capacity(28 + 2 * component.len());
    snapshot.extend_from_slice(b"VST3HOST_STATE\0\0");
    snapshot.extend_from_slice(&1_u32.to_le_bytes());
    snapshot.extend_from_slice(&(component.len() as u32).to_le_bytes());
    snapshot.extend_from_slice(&(component.len() as u32).to_le_bytes());
    snapshot.extend_from_slice(&component);
    snapshot.extend_from_slice(&component);
    snapshot
}

fn fixture_sidechain_samples(id: u32, start: usize, frames: usize) -> Vec<f32> {
    (start..start + frames)
        .flat_map(|sample| {
            if sample >= 11 {
                [0.0; 2]
            } else {
                let gain = match id {
                    0 => 0.01,
                    11 => 0.02,
                    _ => -0.03,
                };
                let value = (sample + 1) as f32 * gain;
                [value, -value * 0.5]
            }
        })
        .collect()
}

struct NativeSidechainAudioIo {
    main: Vec<f32>,
    inputs: Vec<(String, Vec<f32>)>,
}

impl AudioProcessorAudioIo for NativeSidechainAudioIo {
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
        let (key, samples) = self.inputs.iter().find(|(key, _)| key == bus_key)?;
        Some(AudioProcessorInputBus {
            bus_key: key,
            channel_layout: AudioChannelLayout::Stereo,
            frames: samples.len() / 2,
            interleaved: samples,
        })
    }
    fn main_and_auxiliary_input(
        &mut self,
        bus_key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        let (key, samples) = self.inputs.iter().find(|(key, _)| key == bus_key)?;
        Some(AudioProcessorMainAndInputBuses {
            main_layout: AudioChannelLayout::Stereo,
            frames: self.main.len() / 2,
            main_interleaved: &mut self.main,
            auxiliary: AudioProcessorInputBus {
                bus_key: key,
                channel_layout: AudioChannelLayout::Stereo,
                frames: samples.len() / 2,
                interleaved: samples,
            },
        })
    }
}

fn assert_installed_clap_gain(
    plugin_env: &str,
    plugin_id: &str,
    sample_accurate: bool,
    mode: AudioProcessingMode,
) {
    let render_contract = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
    let context = |start_sample| {
        AudioProcessorProcessContext::new(
            AudioRenderRequest { start_sample, frames: 4 },
            AudioSampleRate::new(48_000).expect("sample rate"),
            AudioChannelLayout::Stereo,
            mode,
            AudioKernelBackend::ScalarReference,
        )
    };
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
    let plugin = std::env::var_os(plugin_env)
        .map(PathBuf::from)
        .expect("set the CLAP test plugin environment variable to its native binary");
    let state = 0.5_f32.to_le_bytes();
    let registration =
        probe_clap_plugin_registration(&helper, &plugin, plugin_id, render_contract, Some(&state))
            .expect("probe installed gain contract");
    let payload = serde_json::to_vec(&serde_json::json!({
        "library_path": registration.library_path,
        "plugin_id": registration.plugin_id,
        "binary_sha256": registration.binary_sha256,
        "state": state,
        "parameters": registration.parameters,
        "parameter_ids": [1],
        "main_audio_input": registration.main_audio_input,
        "main_audio_output_id":registration.main_audio_output_id,
        "auxiliary_outputs":super::protocol::outputs_to_wire(&registration.auxiliary_outputs),
    }))
    .expect("CLAP worker payload");
    let parameter_id = ParameterId::new("clap.param.1").expect("volume parameter ID");
    let spec = IsolatedAudioProcessorWorkerSpec::new(
        helper,
        payload,
        registration.execution_contract,
        AudioProcessorAuxiliaryInputContract::default(),
        vec![parameter_id.clone()],
        render_contract,
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
            context(100),
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
            context(104),
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

    if !sample_accurate {
        return;
    }
    // Independent SDK signal oracle: event values are held from their timestamp.
    // Include an initial unchanged range, a last-sample event, and no-event carry.
    assert_native_parameter_signal(
        processor.as_mut(),
        &parameter_id,
        mode,
        &[
            NativeParameterSignalBlock {
                start_sample: 108,
                points: &[(0, 0.25), (2, 0.5)],
                gains: &[0.25, 0.25, 0.5, 0.5],
            },
            NativeParameterSignalBlock {
                start_sample: 112,
                points: &[(1, 0.75), (3, 0.125)],
                gains: &[0.5, 0.75, 0.75, 0.125],
            },
            NativeParameterSignalBlock { start_sample: 116, points: &[], gains: &[0.125; 4] },
        ],
    );
}

#[test]
#[ignore = "requires a built Mondrian executable and the VST3 SDK gain reference DLL"]
fn installed_vst3_reference_processes_through_isolated_worker() {
    assert_installed_vst3_gain("MONDRIAN_VST3_TEST_PLUGIN", false);
}

#[test]
#[ignore = "requires a built Mondrian executable and the owned stateful VST3 gain DLL"]
fn installed_vst3_stateful_gain_preserves_linear_parameter_queues() {
    assert_installed_vst3_gain("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN", true);
}

fn assert_installed_vst3_gain(plugin_env: &str, sample_accurate: bool) {
    let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
        .map(PathBuf::from)
        .expect("set MONDRIAN_VST3_TEST_HELPER to the app executable");
    let binary = std::env::var_os(plugin_env)
        .map(PathBuf::from)
        .expect("set the VST3 test plugin environment variable to its native binary");
    let descriptors =
        scan_vst3_binary_descriptors(&helper, &binary).expect("scan VST3 reference classes");
    let descriptor = if sample_accurate {
        let mut classes = descriptors
            .iter()
            .map(|descriptor| descriptor.class_id.as_str())
            .collect::<Vec<_>>();
        classes.sort_unstable();
        assert_eq!(
            classes,
            [
                "6E4F05A12D8B4C639F017B42A4C2E611",
                "AE4C8B1910234F819CA26D315F8702A1",
            ]
        );
        descriptors
            .iter()
            .find(|descriptor| descriptor.class_id == "AE4C8B1910234F819CA26D315F8702A1")
            .expect("select the original Gain class explicitly")
    } else {
        assert_eq!(descriptors.len(), 1);
        &descriptors[0]
    };
    let registration = probe_vst3_plugin_registration(
        &helper,
        &binary,
        &descriptor.class_id,
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
        .create_instance(&descriptor.class_id, test_render_contract(), None)
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
    if sample_accurate {
        assert_vst3_parameter_queue_signal(processor.as_mut(), &id, AudioProcessingMode::Realtime);
    }
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
        .create_instance(&descriptor.class_id, offline_render, None)
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
    if sample_accurate {
        assert_vst3_parameter_queue_signal(offline.as_mut(), &id, AudioProcessingMode::Offline);
    }
}

fn assert_vst3_parameter_queue_signal(
    processor: &mut dyn AudioProcessor,
    parameter_id: &ParameterId,
    mode: AudioProcessingMode,
) {
    // VST3 queue points describe linear segments. An omitted sample-zero point
    // interpolates from the previous block's final value at implicit offset -1.
    assert_native_parameter_signal(
        processor,
        parameter_id,
        mode,
        &[
            NativeParameterSignalBlock {
                start_sample: 104,
                points: &[(0, 0.25), (2, 0.75)],
                gains: &[0.25, 0.5, 0.75, 0.75],
            },
            NativeParameterSignalBlock {
                start_sample: 108,
                points: &[(3, 0.25)],
                gains: &[0.625, 0.5, 0.375, 0.25],
            },
            NativeParameterSignalBlock { start_sample: 112, points: &[], gains: &[0.25; 4] },
            NativeParameterSignalBlock {
                start_sample: 116,
                points: &[(0, 0.25), (1, 0.75)],
                gains: &[0.25, 0.75, 0.75, 0.75],
            },
        ],
    );
}

struct NativeParameterSignalBlock<'a> {
    start_sample: i64,
    points: &'a [(u32, f64)],
    gains: &'a [f32],
}

fn assert_native_parameter_signal(
    processor: &mut dyn AudioProcessor,
    parameter_id: &ParameterId,
    mode: AudioProcessingMode,
    blocks: &[NativeParameterSignalBlock<'_>],
) {
    for NativeParameterSignalBlock { start_sample, points, gains } in blocks {
        let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
        let ranges = [Range { start: 0, end: points.len() }];
        let events: Vec<_> = points
            .iter()
            .map(|(sample_offset, value)| AudioParameterEvent {
                sample_offset: *sample_offset,
                value: *value,
            })
            .collect();
        let context = AudioProcessorProcessContext::new(
            AudioRenderRequest { start_sample: *start_sample, frames: 4 },
            AudioSampleRate::new(48_000).expect("sample rate"),
            AudioChannelLayout::Stereo,
            mode,
            AudioKernelBackend::ScalarReference,
        );
        processor
            .process(
                context,
                &mut audio,
                AudioParameterEventBatch::new(
                    *start_sample,
                    4,
                    std::slice::from_ref(parameter_id),
                    &ranges,
                    &events,
                ),
            )
            .expect("deliver native parameter events");
        let expected: Vec<_> = gains.iter().flat_map(|gain| [*gain; 2]).collect();
        assert_eq!(
            audio.main, expected,
            "native {mode:?} signal at block {start_sample}"
        );
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
    let mut classes = descriptors
        .iter()
        .map(|descriptor| descriptor.class_id.as_str())
        .collect::<Vec<_>>();
    classes.sort_unstable();
    assert_eq!(
        classes,
        [
            "6E4F05A12D8B4C639F017B42A4C2E611",
            "AE4C8B1910234F819CA26D315F8702A1",
        ]
    );
    let class_id = &descriptors
        .iter()
        .find(|descriptor| descriptor.class_id == "AE4C8B1910234F819CA26D315F8702A1")
        .expect("select the original Gain class explicitly")
        .class_id;
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

mod native_performance;

mod native_clap_note_metadata;
mod native_transport;
#[cfg(target_os = "windows")]
mod native_vst3_canonical_notes;
#[cfg(target_os = "windows")]
mod native_vst3_note_worker;
#[cfg(target_os = "windows")]
mod native_vst3_rescan;
#[cfg(target_os = "windows")]
mod native_vst3_zero_input_runtime;

mod native_clap_restart;
mod native_restart;

mod native_configuration_service;

mod native_owner_binding;

#[cfg(target_os = "windows")]
mod external_vendor;

#[path = "tests/native_host_log.rs"]
mod native_host_log_tests;

#[cfg(target_os = "windows")]
mod native_editor_window;

pub(super) fn pure_seed_factory(
    format: NativeConfigurationFormat,
    state: Option<&[u8]>,
) -> IsolatedAudioProcessorFactory {
    let payload = match format {
        NativeConfigurationFormat::Vst3ComponentAndController => serde_json::json!({
            "schema_version":3,
            "main_audio_input":Vst3MainAudioInput::Present,
            "event_bus_contract":{"schema_version":1,"inputs":[],"outputs":[]},
            "binary_path":"nonexistent-owned-plugin.vst3", "class_id":"00112233445566778899AABBCCDDEEFF",
            "vendor":"Owned", "binary_sha256":vec![7_u8; 32], "state":state,
            "parameters":[], "parameter_ids":[],
        }),
        NativeConfigurationFormat::ClapState => serde_json::json!({
            "library_path":"nonexistent-owned-plugin.clap", "plugin_id":"owned-pure-seed",
            "binary_sha256":vec![7_u8; 32], "state":state, "parameters":[], "parameter_ids":[],
            "main_audio_input":ClapMainAudioInput::Present,
            "main_audio_output_id":0,"auxiliary_outputs":[],
        }),
    };
    let mut spec = IsolatedAudioProcessorWorkerSpec::new(
        PathBuf::from("a-worker-which-does-not-exist"),
        serde_json::to_vec(&payload).expect("owned payload"),
        AudioProcessorExecutionContract::new(
            3,
            AudioProcessorTail::Finite(17),
            true,
            true,
            true,
            71,
        )
        .expect("owned fixed contract"),
        AudioProcessorAuxiliaryInputContract::default(),
        Vec::new(),
        test_render_contract(),
    )
    .expect("pure spec validation");
    spec.helper_arguments = vec![OsString::from("preserve-helper-argument")];
    spec.dispatch_argument = Some(OsString::from("preserve-dispatch"));
    spec.startup_timeout = Duration::from_millis(301);
    spec.state_entry_timeout = Duration::from_millis(401);
    spec.operation_timeout = Duration::from_millis(501);
    let layout = SharedBlockLayout::new(4, 2, 0, 0).expect("fixed mapping layout");
    let execution_contract = spec
        .plugin_execution_contract
        .with_additional_session_scratch_bytes(layout.total_bytes)
        .expect("fixed total contract");
    IsolatedAudioProcessorFactory {
        input_names: Vec::new(),
        spec,
        execution_contract,
        layout,
        retained_create_test_hook: None,
    }
}

#[test]
fn native_factory_seed_pure_rebind_preserves_all_prepared_facts_without_loading() {
    for format in [
        NativeConfigurationFormat::Vst3ComponentAndController,
        NativeConfigurationFormat::ClapState,
    ] {
        let original = pure_seed_factory(format, Some(&[11, 22, 33]));
        let old_bytes = original.spec.preparation_payload.clone();
        let replacement =
            original.rebind_configuration_seed(format, &[44, 55, 255]).expect("pure rebind");
        assert_eq!(replacement.execution_contract, original.execution_contract);
        assert_eq!(replacement.layout, original.layout);
        assert_eq!(replacement.input_names, original.input_names);
        assert_eq!(
            replacement.spec.helper_executable,
            original.spec.helper_executable
        );
        assert_eq!(
            replacement.spec.helper_arguments,
            original.spec.helper_arguments
        );
        assert_eq!(
            replacement.spec.dispatch_argument,
            original.spec.dispatch_argument
        );
        assert_eq!(
            replacement.spec.plugin_execution_contract,
            original.spec.plugin_execution_contract
        );
        assert_eq!(
            replacement.spec.auxiliary_inputs,
            original.spec.auxiliary_inputs
        );
        assert_eq!(replacement.spec.parameter_ids, original.spec.parameter_ids);
        assert_eq!(
            replacement.spec.render_contract,
            original.spec.render_contract
        );
        assert_eq!(
            replacement.spec.startup_timeout,
            original.spec.startup_timeout
        );
        assert_eq!(
            replacement.spec.state_entry_timeout,
            original.spec.state_entry_timeout
        );
        assert_eq!(
            replacement.spec.operation_timeout,
            original.spec.operation_timeout
        );
        assert_eq!(replacement.spec.native_lane, original.spec.native_lane);
        let mut before: serde_json::Value =
            serde_json::from_slice(&old_bytes).expect("before payload");
        let mut after: serde_json::Value =
            serde_json::from_slice(&replacement.spec.preparation_payload).expect("after payload");
        assert_eq!(after["state"], serde_json::json!([44, 55, 255]));
        before.as_object_mut().expect("object").remove("state");
        after.as_object_mut().expect("object").remove("state");
        assert_eq!(before, after);
        assert_eq!(original.spec.preparation_payload, old_bytes);
        // This helper path cannot launch. The public trait path must share it,
        // and succeeds with deliberately nonexistent helper and plugin paths.
        let rebound = original
            .with_native_configuration_seed(format, &[44, 55, 255])
            .expect("public pure path")
            .expect("native capability");
        assert_eq!(rebound.execution_contract(), original.execution_contract);
        assert_eq!(
            rebound.auxiliary_input_contract(),
            original.spec.auxiliary_inputs
        );
    }
}

#[test]
fn native_factory_seed_pure_rebind_keeps_explicit_empty_configuration() {
    for format in [
        NativeConfigurationFormat::Vst3ComponentAndController,
        NativeConfigurationFormat::ClapState,
    ] {
        let original = pure_seed_factory(format, Some(&[9; 32]));
        let replacement =
            original.rebind_configuration_seed(format, &[]).expect("empty native seed");
        let after: serde_json::Value =
            serde_json::from_slice(&replacement.spec.preparation_payload).expect("payload");
        assert_eq!(after["state"], serde_json::json!([]));
        assert_ne!(
            replacement.spec.preparation_payload,
            original.spec.preparation_payload
        );
    }
}

#[test]
fn native_factory_seed_pure_rebind_refuses_wrong_codec_and_extent_without_mutating_original() {
    let original = pure_seed_factory(NativeConfigurationFormat::Vst3ComponentAndController, None);
    let old_bytes = original.spec.preparation_payload.clone();
    assert!(original
        .rebind_configuration_seed(NativeConfigurationFormat::ClapState, &[1])
        .is_err());
    assert!(original
        .rebind_configuration_seed(
            NativeConfigurationFormat::Vst3ComponentAndController,
            &vec![0; vst3_worker::MAX_STATE_BYTES + 1]
        )
        .is_err());
    assert_eq!(original.spec.preparation_payload, old_bytes);
    let original = pure_seed_factory(NativeConfigurationFormat::ClapState, None);
    assert!(original
        .rebind_configuration_seed(
            NativeConfigurationFormat::ClapState,
            &vec![255; MAX_CONTROL_FRAME_BYTES / 2]
        )
        .is_err());
    assert!(
        encode_native_configuration_payload(&vec![255_u8; MAX_CONTROL_FRAME_BYTES / 2]).is_err()
    );
}

fn factory_storage_inventory(
    factory: &IsolatedAudioProcessorFactory,
) -> Vec<crate::AudioProcessorFactoryStorageBorrow<'_>> {
    let mut values = Vec::with_capacity(64);
    factory
        .visit_host_owned_storage(&mut |storage| {
            values.push(storage);
            Ok(())
        })
        .expect("complete concrete nested inventory");
    values
}

#[test]
fn native_factory_storage_isolated_reports_each_actual_nested_capacity_and_coverage() {
    use crate::AudioProcessorFactoryStorageKind as Kind;
    let mut factory = pure_seed_factory(NativeConfigurationFormat::ClapState, Some(&[4, 8]));
    let mut bus_key = String::with_capacity(71);
    bus_key.push_str("sidechain");
    let mut name = String::with_capacity(89);
    name.push_str("Sidechain native label");
    factory.input_names = Vec::with_capacity(5);
    factory
        .input_names
        .push(crate::AudioProcessorInputName { bus_key: bus_key.clone(), name });
    factory.spec.auxiliary_inputs.buses = Vec::with_capacity(3);
    factory
        .spec
        .auxiliary_inputs
        .buses
        .push(crate::AudioProcessorAuxiliaryInputBusContract {
            bus_key,
            channel_layout: AudioChannelLayout::Stereo,
        });
    factory.spec.parameter_ids = Vec::with_capacity(7);
    let mut id = String::with_capacity(113);
    id.push_str("owned.gain");
    let id_capacity = id.capacity();
    factory.spec.parameter_ids.push(ParameterId::new(id).expect("stable exact ID"));
    let inventory = factory_storage_inventory(&factory);
    // Predict the complete owners from independent public container identity
    // and actual capacity, not logical schema size or the production visitor.
    let expected = [
        (
            factory.input_names.as_ptr().cast::<()>() as usize,
            factory.input_names.capacity() * size_of::<crate::AudioProcessorInputName>(),
        ),
        (
            factory.input_names[0].bus_key.as_ptr() as usize,
            factory.input_names[0].bus_key.capacity(),
        ),
        (
            factory.input_names[0].name.as_ptr() as usize,
            factory.input_names[0].name.capacity(),
        ),
        (
            factory.spec.helper_arguments.as_ptr().cast::<()>() as usize,
            factory.spec.helper_arguments.capacity() * size_of::<OsString>(),
        ),
        (
            factory.spec.preparation_payload.as_ptr() as usize,
            factory.spec.preparation_payload.capacity(),
        ),
        (
            factory.spec.auxiliary_inputs.buses.as_ptr().cast::<()>() as usize,
            factory.spec.auxiliary_inputs.buses.capacity()
                * size_of::<crate::AudioProcessorAuxiliaryInputBusContract>(),
        ),
        (
            factory.spec.auxiliary_inputs.buses[0].bus_key.as_ptr() as usize,
            factory.spec.auxiliary_inputs.buses[0].bus_key.capacity(),
        ),
        (
            factory.spec.parameter_ids.as_ptr().cast::<()>() as usize,
            factory.spec.parameter_ids.capacity() * size_of::<ParameterId>(),
        ),
        (
            factory.spec.parameter_ids[0].as_str().as_ptr() as usize,
            id_capacity,
        ),
    ];
    for (address, bytes) in expected {
        let found = inventory
            .iter()
            .filter(|value| value.address() == address && value.kind() == Kind::CompleteAllocation)
            .collect::<Vec<_>>();
        assert_eq!(found.len(), 1, "every concrete owner appears once");
        assert_eq!(found[0].layout().size(), bytes);
        assert!(found[0].complete_allocation().is_some());
    }
    let encoded = inventory
        .iter()
        .filter(|value| value.kind() == Kind::StdPlatformEncodedBuffer)
        .collect::<Vec<_>>();
    assert_eq!(
        encoded.len(),
        3,
        "real helper path, helper argument and optional dispatch"
    );
    assert!(encoded.iter().all(|value| value.complete_allocation().is_none()));
}

#[test]
fn native_factory_storage_seeded_old_new_overlap_uses_real_distinct_payload_and_alias_union() {
    let old = pure_seed_factory(NativeConfigurationFormat::ClapState, Some(&[4, 8]));
    let next = old
        .rebind_configuration_seed(NativeConfigurationFormat::ClapState, &[7; 63])
        .expect("same concrete pure seed operation");
    assert_ne!(
        old.spec.preparation_payload.as_ptr(),
        next.spec.preparation_payload.as_ptr()
    );
    let old_storage = factory_storage_inventory(&old);
    let next_storage = factory_storage_inventory(&next);
    let complete = old_storage
        .iter()
        .chain(next_storage.iter())
        .filter_map(|storage| storage.complete_allocation())
        .collect::<Vec<_>>();
    let mut ledger = crate::NativeHostAllocationLedger::new(64, usize::MAX).expect("known union");
    for &storage in &complete {
        ledger.record(storage).expect("actual complete owner");
    }
    for &storage in &complete {
        ledger.record(storage).expect("aliases charge once");
    }
    let result = ledger.finish().expect("known subset result");
    let mut identities = std::collections::BTreeMap::new();
    for storage in complete {
        if storage.layout().size() != 0 {
            identities.insert(storage.address(), storage.layout().size());
        }
    }
    let expected = identities.values().sum::<usize>()
        + crate::NativeHostAllocationLedger::storage_layout(64)
            .expect("bookkeeping")
            .bytes()
            .expect("checked sum");
    assert_eq!(result.allocation_bytes(), expected);
    assert!(old_storage
        .iter()
        .any(|storage| storage.address() == old.spec.preparation_payload.as_ptr() as usize));
    assert!(next_storage
        .iter()
        .any(|storage| storage.address() == next.spec.preparation_payload.as_ptr() as usize));
}

#[test]
fn native_factory_storage_missing_adapter_is_explicit_and_cannot_report_successful_zero() {
    struct Missing;
    impl AudioProcessorFactory for Missing {
        fn execution_contract(&self) -> AudioProcessorExecutionContract {
            AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, true, true, true, 0)
                .expect("contract")
        }
        fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
            Err(invalid_contract(
                "observation must never create a processor",
            ))
        }
    }
    let factory = Missing;
    let mut calls = 0;
    let result = factory.visit_host_owned_storage(&mut |_| {
        calls += 1;
        Ok(())
    });
    assert!(matches!(
        result,
        Err(crate::AudioProcessorFactoryStorageError::Unavailable)
    ));
    assert_eq!(calls, 0);
}

#[test]
fn native_factory_storage_visitor_refusal_stops_without_loading_or_silently_omitting_owners() {
    let factory = pure_seed_factory(NativeConfigurationFormat::ClapState, None);
    let mut calls = 0;
    let result = factory.visit_host_owned_storage(&mut |_| {
        calls += 1;
        Err(crate::NativeHostLedgerError::OwnerLimitExceeded { limit: 0 }.into())
    });
    assert!(matches!(
        result,
        Err(crate::AudioProcessorFactoryStorageError::Owner(
            crate::NativeHostLedgerError::OwnerLimitExceeded { limit: 0 }
        ))
    ));
    assert_eq!(calls, 1);
}

#[cfg(all(test, windows))]
#[path = "tests/native_clap_public_notes.rs"]
mod native_clap_public_notes;

#[cfg(all(test, windows))]
#[path = "tests/native_clap_public_note_refusals.rs"]
mod native_clap_public_note_refusals;

#[test]
fn isolated_music_context_mismatch_preserves_shared_pcm_parameters_and_live_child() {
    use mondrian_core::{MusicalTimeReference, TimelineTime};
    let reference = |zero| {
        MusicalTimeReference::new(
            TimelineTime::new(zero, 7).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
        .unwrap()
    };
    let parameter = ParameterId::new_static("test.gain");
    let spec = test_spec(
        TestBehavior::Normal,
        stateful_contract(),
        AudioProcessorAuxiliaryInputContract::default(),
        vec![parameter.clone()],
        Duration::from_secs(2),
    )
    .with_musical_time_reference(Some(reference(1)))
    .unwrap();
    let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
    let (worker, _) = SupervisedAudioWorker::launch(
        &factory.spec,
        factory.spec.render_contract,
        factory.layout,
        Some(&factory.input_names),
    )
    .unwrap();
    let mut processor = IsolatedAudioProcessor {
        musical_time_reference: factory.spec.musical_time_reference,
        worker,
        parameter_ids: factory.spec.parameter_ids.clone(),
        auxiliary_inputs: factory.spec.auxiliary_inputs.clone(),
        auxiliary_outputs: factory.spec.auxiliary_outputs.clone(),
        note_input_contract: factory.spec.note_input_contract.clone(),
        render_contract: factory.spec.render_contract,
        failed: false,
        pending_reprepare: None,
        staged_configuration_seed: None,
        native_plugin_contract: factory.spec.plugin_execution_contract,
    };
    processor.enter_state(-17).unwrap();
    let before = processor.worker.shared_mut().to_vec();
    let mut io = TestAudioIo { main: vec![2.0; 8], auxiliary: Vec::new() };
    let ids = [parameter];
    let event_range = 0..1;
    let ranges = [event_range];
    let events = [AudioParameterEvent { sample_offset: 0, value: 3.0 }];
    for wrong in [None, Some(reference(2))] {
        let error = processor
            .process(
                test_context(-17).with_musical_time_reference(wrong),
                &mut io,
                AudioParameterEventBatch::new(-17, 4, &ids, &ranges, &events),
            )
            .unwrap_err();
        assert_eq!(
            error,
            invalid_contract("isolated processor musical reference changed")
        );
        assert_eq!(io.main, vec![2.0; 8]);
        assert_eq!(processor.worker.shared_mut(), before.as_slice());
        assert!(!processor.failed);
    }
    processor
        .process(
            test_context(-17).with_musical_time_reference(Some(reference(1))),
            &mut io,
            AudioParameterEventBatch::new(-17, 4, &ids, &ranges, &events),
        )
        .unwrap();
    assert_eq!(io.main, vec![6.0; 8]);
    assert!(
        processor.worker.shutdown(),
        "original bounded shutdown must still succeed"
    );
}

#[test]
fn isolated_observed_request_completed_then_poll_retains_each_original_identity_and_none_hold() {
    let parameter = ParameterId::new_static("test.gain");
    let spec = test_spec(
        TestBehavior::ObservedPending,
        AudioProcessorExecutionContract::stateless(),
        AudioProcessorAuxiliaryInputContract::default(),
        vec![parameter.clone()],
        Duration::from_secs(2),
    );
    let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
    let (worker, _) = SupervisedAudioWorker::launch(
        &factory.spec,
        factory.spec.render_contract,
        factory.layout,
        Some(&factory.input_names),
    )
    .unwrap();
    let mut processor = IsolatedAudioProcessor {
        musical_time_reference: factory.spec.musical_time_reference,
        worker,
        parameter_ids: factory.spec.parameter_ids.clone(),
        auxiliary_inputs: factory.spec.auxiliary_inputs.clone(),
        auxiliary_outputs: factory.spec.auxiliary_outputs.clone(),
        note_input_contract: factory.spec.note_input_contract.clone(),
        render_contract: factory.spec.render_contract,
        failed: false,
        pending_reprepare: None,
        staged_configuration_seed: None,
        native_plugin_contract: factory.spec.plugin_execution_contract,
    };
    let raw = mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 };
    let a = crate::NativeObservedRequest::Cohort(NativeRequestCohort::new(101, raw).unwrap());
    let b = crate::NativeObservedRequest::Cohort(NativeRequestCohort::new(102, raw).unwrap());
    let ids = [parameter];
    let events = [AudioParameterEvent { sample_offset: 0, value: 3.0 }];
    let event_range = 0..1;
    let ranges = [event_range];
    let mut io = TestAudioIo { main: vec![2.0; 8], auxiliary: Vec::new() };
    processor
        .process(
            test_context(0),
            &mut io,
            AudioParameterEventBatch::new(0, 4, &ids, &ranges, &events),
        )
        .unwrap();
    assert_eq!(
        io.main,
        vec![6.0; 8],
        "CompletedWithPending is the already completed DSP, not Deferred"
    );
    assert_eq!(
        processor.pending_reprepare,
        Some(a),
        "first reply keeps its original serial before any later query"
    );
    assert_eq!(processor.preflight_reprepare().unwrap(), Some(b));
    assert_ne!(
        a, b,
        "equal raw bits do not identify the same callback generation"
    );
    assert_eq!(
        processor.preflight_reprepare().unwrap(),
        Some(b),
        "None retains the original old hold"
    );
    let request_only = crate::NativeObservedRequest::RequestOnly(raw);
    assert_eq!(processor.preflight_reprepare().unwrap(), Some(request_only));
    assert_ne!(
        request_only, b,
        "a raw-only observation cannot borrow the preceding serial"
    );
    assert_eq!(processor.preflight_reprepare().unwrap(), Some(request_only));
    let shared_before = processor.worker.shared_mut().to_vec();
    io.main.fill(73.0);
    assert_eq!(
        processor.process(
            test_context(4),
            &mut io,
            AudioParameterEventBatch::new(4, 4, &ids, &ranges, &events)
        ),
        Err(AudioProcessorHostError::ReprepareRequired(request_only))
    );
    assert_eq!(io.main, vec![73.0; 8]);
    assert_eq!(processor.worker.shared_mut(), shared_before.as_slice());
    assert!(!processor.failed);
    assert!(
        processor.worker.shutdown(),
        "retain the original bounded child shutdown assertion"
    );
}
