//! Owned Event-metadata admission through the normal Product child.
//! No canonical Note, instrument, multi-output, realtime or third-party claim.
#![cfg(windows)]

use mondrian_audio::{
    compile_audio_program, probe_vst3_plugin_registration, AudioCompileRequest,
    AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioExecutionError, AudioKernelBackend,
    AudioMediaResolver, AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease,
    AudioProcessingMode, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioProcessorFactory, AudioProcessorHostError, AudioProcessorPrepareRequest,
    AudioProcessorResolver, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    AudioRuntimeBuildError, AudioRuntimeResourceGrant, AudioSessionCursorEvidence, AudioStateEntry,
    IsolatedAudioProcessorResolver, NativeConfigurationControlOutcome,
    NativeConfigurationControlRequest, NativeConfigurationFormat, NativeConfigurationRefusal,
    PreparedAudioPlan, ResolvedAudioSource, Vst3AudioProcessorSpecResolver, Vst3EventBusRole,
    Vst3PluginRegistration,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioProcessorInstanceId,
    AudioSourceComponentId, AuthoringMap, ExecutionCancellationToken, NativeAudioReprepareRequest,
};
use mondrian_timeline::audio::{AudioProcessorDefinitionRef, AudioProcessorInstance};
use mondrian_timeline::Sequence;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::num::NonZeroU64;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use vst3_host::Vst3Host;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE,
};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const TRACE_ENV: &str = "MONDRIAN_VST3_EVENT_ADMISSION_TRACE";
const BLOCK: usize = 16;

fn required_file(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).expect("explicit native gate input"));
    assert!(path.is_absolute() && path.is_file(), "{name}");
    path
}
fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 0,
        compensation_delay_scratch_budget_bytes: 0,
    }
}
fn configuration(auxiliary_default: bool) -> Vec<u8> {
    // Independent literal MNE2 codec: failure-call 0, one restored default flag.
    let mut component = b"MNE2".to_vec();
    component.extend_from_slice(&2_u32.to_le_bytes());
    component.extend_from_slice(&0_u32.to_le_bytes());
    component.extend_from_slice(&u32::from(auxiliary_default).to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    state.extend_from_slice(&1_u32.to_le_bytes());
    state.extend_from_slice(&16_u32.to_le_bytes());
    state.extend_from_slice(&16_u32.to_le_bytes());
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}
struct TraceEnvironment(Option<OsString>);
impl TraceEnvironment {
    fn select(path: &Path) -> Self {
        assert!(path.is_absolute());
        File::create_new(path).expect("explicit precreated fixture-only trace");
        let old = std::env::var_os(TRACE_ENV);
        // SAFETY: this integration target is Windows-only, where environment
        // mutation is supported across threads. Its exact native selectors run
        // serially with sequential child lifetimes; the production host has no trace switch.
        unsafe { std::env::set_var(TRACE_ENV, path) };
        Self(old)
    }
}
impl Drop for TraceEnvironment {
    fn drop(&mut self) {
        // SAFETY: same Windows-only environment contract as select().
        unsafe {
            if let Some(old) = &self.0 {
                std::env::set_var(TRACE_ENV, old);
            } else {
                std::env::remove_var(TRACE_ENV);
            }
        }
    }
}
struct NoMedia;
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        panic!("this zero-media Main effect must not request a media source")
    }
}
fn build(
    helper: &Path,
    registration: Vst3PluginRegistration,
    state: Vec<u8>,
) -> Result<AudioProgramRuntime, AudioRuntimeBuildError> {
    let processor = AudioProcessorInstance {
        id: AudioProcessorInstanceId::new(),
        definition: AudioProcessorDefinitionRef::Vst3 {
            class_id: registration.class_id.clone(),
            vendor: registration.vendor.clone(),
            schema_version: 1,
            binary_sha256: Some(registration.binary_sha256),
        },
        bypassed: false,
        parameters: AuthoringMap::new(),
        opaque_state: Some(state.into()),
    };
    let mut sequence = Sequence::new("Owned VST3 Event metadata admission");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
    assert!(sequence.audio_program.note_lanes.is_empty());
    let resolver = IsolatedAudioProcessorResolver::new(Arc::new(
        Vst3AudioProcessorSpecResolver::new(helper.to_owned(), [registration])
            .expect("original validated registry"),
    ));
    AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &NoMedia,
        &resolver,
        render(),
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
    )
}
fn assert_contract(registration: &Vst3PluginRegistration, auxiliary_default: bool) {
    let contract = &registration.event_bus_contract;
    contract.validate().expect("strict versioned event contract");
    assert_eq!(contract.schema_version, 1);
    let inputs = contract.inputs.as_slice();
    assert_eq!(inputs.len(), 2);
    assert!(contract.outputs.is_empty());
    assert_eq!(inputs[0].native_index, 0);
    assert_eq!(inputs[0].role, Vst3EventBusRole::Main);
    assert_eq!(inputs[1].native_index, 1);
    assert_eq!(inputs[1].role, Vst3EventBusRole::Auxiliary);
    assert_eq!([inputs[0].channel_count, inputs[1].channel_count], [16, 16]);
    assert_eq!(
        [inputs[0].default_active, inputs[1].default_active],
        [true, auxiliary_default]
    );
    assert_eq!(
        [inputs[0].host_active, inputs[1].host_active],
        [true, false],
        "successful load-time host activation is independent of restored default flags",
    );
}
#[derive(Default)]
struct Cursors {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
fn owner(runtime: &AudioProgramRuntime) -> AudioNativeProcessorOwnerLease {
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).unwrap();
    assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
    let processor = cursors.processors[0];
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        state => panic!("unexpected native cursor: {state:?}"),
    };
    runtime
        .native_owner_lease(AudioNativeProcessorAddress {
            root: cursors.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch,
        })
        .expect("original installed runtime lease")
}
#[derive(Debug, PartialEq, Eq)]
struct TraceRow {
    pid: u32,
    instance: u64,
    stage: String,
    index: i32,
    default: bool,
    active: bool,
    calls: usize,
}
fn trace(path: &Path) -> Vec<TraceRow> {
    assert!(fs::metadata(path).unwrap().len() <= 16 * 1024);
    let bytes = fs::read_to_string(path).unwrap();
    assert!(!bytes.is_empty() && bytes.ends_with('\n'));
    bytes
        .lines()
        .map(|line| {
            let mut tokens = line.split_ascii_whitespace();
            assert_eq!(tokens.next(), Some("MNE2"));
            let mut fields = BTreeMap::new();
            for token in tokens {
                let (key, value) = token.split_once('=').unwrap();
                assert!(fields.insert(key, value).is_none(), "duplicate trace field");
            }
            assert_eq!(fields.len(), 7);
            TraceRow {
                pid: fields["pid"].parse().unwrap(),
                instance: fields["instance"].parse().unwrap(),
                stage: fields["stage"].into(),
                index: fields["index"].parse().unwrap(),
                default: match fields["aux_default"] {
                    "0" => false,
                    "1" => true,
                    _ => panic!("flag"),
                },
                active: match fields["aux_active"] {
                    "0" => false,
                    "1" => true,
                    _ => panic!("flag"),
                },
                calls: fields["calls"].parse().unwrap(),
            }
        })
        .collect()
}
// DLL-local instance IDs can restart when the old component's module unloads.
// Correlate this sequential gate by complete ordered lifetimes, without treating
// the fixture ID as a process-wide or production native-object identity.
fn lifetimes(rows: &[TraceRow]) -> Vec<&[TraceRow]> {
    let mut result = Vec::new();
    let mut start = None;
    for (index, row) in rows.iter().enumerate() {
        match row.stage.as_str() {
            "restore" => {
                assert!(
                    start.is_none(),
                    "restore before the prior lifetime terminated"
                );
                start = Some(index);
            }
            "census" => assert!(start.is_some(), "census outside a restored lifetime"),
            "terminate" => {
                let begin = start.take().expect("duplicate or unpaired component termination");
                result.push(&rows[begin..=index]);
            }
            _ => panic!("unexpected fixture stage"),
        }
        if let Some(begin) = start {
            assert_eq!(
                (row.pid, row.instance),
                (rows[begin].pid, rows[begin].instance),
                "one component within the open lifetime",
            );
        }
    }
    assert!(start.is_none(), "incomplete restored component lifetime");
    result
}
fn assert_census_and_exit(rows: &[TraceRow], calls: usize, default: bool) {
    let restored = rows.first().expect("complete component lifetime");
    assert_ne!(
        restored.pid,
        std::process::id(),
        "normal Product child, not test-only host execution",
    );
    assert!(restored.instance > 0);
    assert_eq!(restored.stage, "restore");
    assert_eq!(restored.default, default);
    assert!(
        rows.iter()
            .all(|row| (row.pid, row.instance) == (restored.pid, restored.instance)),
        "same native component within this lifetime"
    );
    assert_eq!(rows.iter().filter(|row| row.stage == "restore").count(), 1);
    for index in [0, 1] {
        assert!(
            rows.iter().any(|row| row.stage == "census"
                && row.index == index
                && row.default == default
                && !row.active),
            "actual same-component restored census {index}",
        );
    }
    let exits: Vec<_> = rows.iter().filter(|row| row.stage == "terminate").collect();
    assert_eq!(
        exits.len(),
        1,
        "one actual component termination per lifetime"
    );
    assert_eq!(rows.last().unwrap().stage, "terminate");
    assert_eq!(exits[0].calls, calls, "fixture actual SDK process count");
}
fn roundtrip(binary: &Path) {
    // Separate direct-SDK codec control, never evidence of the Product path.
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(BLOCK)
        .input_channels(2)
        .output_channels(2)
        .build()
        .unwrap();
    let mut plugin = host.load_plugin_class(binary, CLASS).unwrap();
    plugin.load_state(&configuration(true)).unwrap();
    let saved = plugin.save_state().expect("actual MNE2 component/controller capture");
    assert_eq!(&saved[..16], b"VST3HOST_STATE\0\0");
    assert_eq!(u32::from_le_bytes(saved[16..20].try_into().unwrap()), 1);
    let component = u32::from_le_bytes(saved[20..24].try_into().unwrap()) as usize;
    let controller = u32::from_le_bytes(saved[24..28].try_into().unwrap()) as usize;
    assert_eq!((component, controller), (60, 16));
    assert_eq!(saved.len(), 28 + component + controller);
    let expected = configuration(true);
    let config = &expected[28..44];
    assert_eq!(&saved[28..44], config);
    assert_eq!(&saved[28 + component..], config);
    assert_eq!(&saved[44..48], b"MNA1");
    let mut restored = host.load_plugin_class(binary, CLASS).unwrap();
    restored.load_state(&saved).expect("actual fresh MNE2 restore");
    let info = restored.native_event_bus_info().unwrap();
    assert!(info.inputs[1].default_active);
    assert!(!info.inputs[1].host_active);
}

#[test]
#[ignore = "fresh normal Product, rebuilt owned MNE2 DLL, explicit empty result directory; no canonical Notes"]
fn installed_vst3_event_metadata_admission_revalidates_restored_default_and_activation() {
    let helper = required_file("MONDRIAN_VST3_TEST_HELPER");
    let binary = required_file("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN");
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_EVENT_ADMISSION_RESULTS")
            .expect("fresh explicit result directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);

    {
        let path = directory.join("roundtrip.trace");
        let _environment = TraceEnvironment::select(&path);
        roundtrip(&binary);
    }
    let path = directory.join("accepted.trace");
    let pid = {
        let _environment = TraceEnvironment::select(&path);
        let state = configuration(true);
        let registration =
            probe_vst3_plugin_registration(&helper, &binary, CLASS, render(), Some(&state))
                .expect("public same-restored-instance Event contract probe");
        assert_contract(&registration, true);
        assert!(
            registration.parameters.is_empty() && registration.auxiliary_inputs.buses.is_empty()
        );
        let mut runtime =
            build(&helper, registration, state).expect("real Factory validation/create");
        let installation = AudioOwnerInstallationSource::new();
        let (registration, verifier) = installation.register().unwrap();
        runtime.bind_native_owner_installation(verifier).unwrap();
        let claim = installation
            .reserve_install(&registration, None)
            .and_then(|reservation| installation.begin_commit(reservation))
            .unwrap()
            .commit()
            .unwrap();
        runtime
            .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
            .unwrap();
        runtime
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        let before = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: 0,
            })
            .expect("actual first signal entry");
        assert_eq!(
            runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
            before
        );
        for start_sample in [0, BLOCK as i64] {
            let mut pcm = vec![f32::NAN; BLOCK * 2];
            runtime
                .render_into(AudioRenderRequest { start_sample, frames: BLOCK }, &mut pcm)
                .expect("normal worker ordinary Main effect with no Note input");
            assert!(pcm.iter().all(|value| value.is_finite()));
            for frame in pcm.chunks_exact(2) {
                assert_eq!(
                    frame,
                    [0.125, -0.0625],
                    "independent MNE2 literal effect oracle"
                );
            }
        }
        runtime
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(2),
                start_sample: 128,
            })
            .expect("same-child cold reload must revalidate the unchanged restored Event contract");
        let after = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
        assert_eq!(after.process_id(), before.process_id());
        assert!(after.object_serial() > before.object_serial());
        let mut pcm = vec![f32::NAN; BLOCK * 2];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: 128, frames: BLOCK },
                &mut pcm,
            )
            .expect("actual fresh component after cold reload");
        for frame in pcm.chunks_exact(2) {
            assert_eq!(frame, [0.125, -0.0625], "cold-restored ordinary Main PCM");
        }
        runtime
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        let pid = before.process_id().get();
        // SAFETY: authenticated installed runtime observation; retained HANDLE
        // only observes this child and cannot be confused by later PID reuse.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        drop(runtime);
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) },
            0
        );
        let mut exit = u32::MAX;
        assert_ne!(
            unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
            0
        );
        assert_eq!(exit, 0, "natural worker shutdown remains a strong gate");
        pid
    };
    let rows = trace(&path);
    let completed = lifetimes(&rows);
    let processed: Vec<_> = completed
        .iter()
        .copied()
        .filter(|segment| segment.last().unwrap().calls != 0)
        .collect();
    assert_eq!(processed.len(), 2, "old component and actual cold reload");
    assert!(processed.iter().all(|segment| segment[0].pid == pid));
    assert_eq!(
        processed
            .iter()
            .map(|segment| segment.last().unwrap().calls)
            .collect::<Vec<_>>(),
        [2, 1],
        "ordered original component then actual cold reload",
    );
    for component in completed {
        let calls = component.last().unwrap().calls;
        assert_census_and_exit(component, calls, true);
    }

    for (label, activation_tamper) in [("restored-default", false), ("host-activation", true)] {
        let path = directory.join(format!("{label}.trace"));
        let _environment = TraceEnvironment::select(&path);
        let old = configuration(false);
        let mut expected =
            probe_vst3_plugin_registration(&helper, &binary, CLASS, render(), Some(&old))
                .expect("actual old/default-inactive event contract");
        assert_contract(&expected, false);
        if activation_tamper {
            expected.event_bus_contract.inputs.as_mut_slice()[1].host_active = true;
            expected
                .event_bus_contract
                .validate()
                .expect("structurally valid but false activation claim");
        }
        let probe_trace = trace(&path);
        let error = build(&helper, expected, configuration(!activation_tamper))
            .err()
            .expect("real Factory restore must reject changed expected event metadata");
        assert!(
            error.to_string().contains("VST3 event-bus contract changed since probe"),
            "specific event-contract refusal: {error:?}"
        );
        let rows = trace(&path);
        assert!(rows.iter().filter(|row| row.stage == "terminate").all(|row| row.calls == 0));
        assert!(
            rows.starts_with(&probe_trace),
            "completed public Probe trace must remain an unchanged prefix",
        );
        let factory_lifetimes = lifetimes(&rows[probe_trace.len()..]);
        assert!(
            !factory_lifetimes.is_empty(),
            "Factory must append a complete actual lifetime after the public Probe",
        );
        for component in factory_lifetimes {
            assert_census_and_exit(component, 0, !activation_tamper);
        }
    }
}

// Observe the original resolver result, not a substitute Factory or processor.
// No request borrow or mutex guard crosses the real prepare/create operation.
struct FactoryObservation {
    inner: IsolatedAudioProcessorResolver,
    expected_instance: AudioProcessorInstanceId,
    factory: Mutex<Option<Arc<dyn AudioProcessorFactory>>>,
}
impl AudioProcessorResolver for FactoryObservation {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        assert_eq!(request.instance_id(), self.expected_instance);
        assert!(!request.note_input_requested());
        let factory = self.inner.prepare(request)?;
        let mut observed = self.factory.lock().unwrap();
        assert!(observed.is_none(), "exactly one real processor preparation");
        *observed = Some(Arc::clone(&factory));
        Ok(factory)
    }
}
fn service_configuration(drift: bool) -> Vec<u8> {
    // Independent literal codecs: component owns the current Event default and
    // first-stop action; controller owns only an input-only restart command.
    let mut component = b"MNE3".to_vec();
    for value in [3_u32, 0, 0, if drift { 2 } else { 1 }] {
        component.extend_from_slice(&value.to_le_bytes());
    }
    let controller = b"MNC3\x03\0\0\0\x01\0\0\0";
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 20, 12] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(controller);
    state
}
fn observe_child(pid: u32) -> OwnedHandle {
    // SAFETY: pid comes from the actual sole supervised child's authenticated
    // object observation. This read-only HANDLE observes that same process.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(!raw.is_null());
    unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
}
fn assert_child_exit(handle: OwnedHandle) {
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) },
        0,
    );
    let mut exit = u32::MAX;
    assert_ne!(
        unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
        0,
    );
    // Drop does not expose the supervisor shutdown result. This same-HANDLE
    // observation proves exit within 2s, not the original 100ms graceful budget.
    assert_eq!(exit, 0, "same observed child OS exit0 after Drop within 2s");
}

#[test]
#[ignore = "fresh normal Product, rebuilt owned MNE3 DLL, explicit empty result directory; startup metadata safety only"]
fn installed_vst3_retained_event_metadata_refuses_unannounced_default_drift() {
    let helper = required_file("MONDRIAN_VST3_TEST_HELPER");
    let binary = required_file("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN");
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_EVENT_ADMISSION_RESULTS")
            .expect("fresh explicit result directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);

    for drift in [false, true] {
        let path = directory.join(if drift {
            "retained-drift.trace"
        } else {
            "retained-stable.trace"
        });
        let _environment = TraceEnvironment::select(&path);
        let state = service_configuration(drift);
        let registration =
            probe_vst3_plugin_registration(&helper, &binary, CLASS, render(), Some(&state))
                .expect("real restored nonempty Event metadata probe");
        assert_contract(&registration, false);
        assert!(registration.parameters.is_empty());
        assert!(registration.auxiliary_inputs.buses.is_empty());
        let instance = AudioProcessorInstance {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Vst3 {
                class_id: registration.class_id.clone(),
                vendor: registration.vendor.clone(),
                schema_version: 1,
                binary_sha256: Some(registration.binary_sha256),
            },
            bypassed: false,
            parameters: AuthoringMap::new(),
            opaque_state: Some(state.into()),
        };
        let mut sequence = Sequence::new("Retained nonempty Event metadata safety");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        let instance_id = instance.id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        assert!(sequence.audio_program.note_lanes.is_empty());
        let observer = FactoryObservation {
            inner: IsolatedAudioProcessorResolver::new(Arc::new(
                Vst3AudioProcessorSpecResolver::new(helper.clone(), [registration.clone()])
                    .expect("original public Probe registration"),
            )),
            expected_instance: instance_id,
            factory: Mutex::new(None),
        };
        let program = compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .unwrap();
        let prepared = PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(program),
            render(),
            AudioKernelBackend::default(),
            &observer,
        )
        .expect("canonical prepare and actual Factory validation");
        let factory = observer.factory.lock().unwrap().take().expect("same returned Factory Arc");
        let mut subject = factory.create().expect("actual retained processor, not a wrapper");
        let before = subject.native_audio_object_observation().unwrap().unwrap();
        let handle = observe_child(before.process_id().get());
        let prefix = trace(&path);
        let begin = prefix
            .iter()
            .rposition(|row| row.stage == "restore" && row.pid == before.process_id().get())
            .expect("this subject's restored lifetime");
        let first = &prefix[begin];
        assert!(!first.default && !first.active);
        let identity = (first.pid, first.instance);
        assert!(prefix[begin..]
            .iter()
            .all(|row| (row.pid, row.instance) == identity && row.calls == 0));
        assert_eq!(
            prefix[begin..].iter().filter(|row| row.stage == "start").count(),
            1
        );
        assert!(!prefix[begin..]
            .iter()
            .any(|row| matches!(row.stage.as_str(), "stop" | "consume" | "terminate")));

        // Service the genuine startup callback before any Enter/PCM. Neither
        // an empty parameter profile nor a Factory observation can mint an ACK.
        let cohort =
            subject.native_configuration_cohort().unwrap().expect("actual restart callback");
        assert_eq!(
            cohort.request,
            NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
        );
        assert_ne!(cohort.serial, 0);
        let token = NonZeroU64::new(17).unwrap();
        let outcome = subject
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[],
                configuration_limit_bytes: 128 * 1024,
                control_buffer_grant_bytes: 1024 * 1024,
            })
            .expect("actual stopped same-object configuration service");
        assert_eq!(
            subject.native_audio_object_observation().unwrap(),
            Some(before)
        );
        let captured = if drift {
            assert_eq!(
                outcome,
                NativeConfigurationControlOutcome::CapabilityRefused(
                    NativeConfigurationRefusal::MetadataChanged,
                )
            );
            assert_eq!(subject.native_configuration_cohort().unwrap(), Some(cohort));
            None
        } else {
            let NativeConfigurationControlOutcome::ReadyForInstall(candidate) = outcome else {
                panic!("unchanged nonempty Event contract must provide a candidate: {outcome:?}");
            };
            assert_eq!(candidate.serviced_cohort, cohort);
            assert_eq!(candidate.control_token, token);
            assert_eq!(candidate.observed_contract, registration.execution_contract);
            assert_eq!(
                candidate.format,
                NativeConfigurationFormat::Vst3ComponentAndController
            );
            assert!(candidate.buffers.peak_bytes().unwrap() <= 1024 * 1024);
            assert_eq!(subject.native_configuration_cohort().unwrap(), None);
            let bytes = &candidate.configuration;
            assert_eq!(bytes.len(), 28 + 64 + 12);
            assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
            assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
            assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 64);
            assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 12);
            assert_eq!(&bytes[28..48], b"MNE3\x03\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
            assert_eq!(&bytes[48..52], b"MNA1");
            assert_eq!(
                u32::from_le_bytes(bytes[64..68].try_into().unwrap()),
                0,
                "zero actual DSP calls before service"
            );
            assert_eq!(&bytes[92..], b"MNC3\x03\0\0\0\0\0\0\0");
            Some(candidate.configuration)
        };
        // A candidate is not an installation; both retained outcomes keep old
        // PCM blocked. This does not exercise the not-yet-admitted Note path.
        assert_eq!(subject.preflight_reprepare().unwrap(), Some(cohort.request));
        assert!(
            matches!(subject.enter_state(0), Err(AudioProcessorHostError::ReprepareRequired(request)) if request == cohort.request)
        );
        drop(subject);
        assert_child_exit(handle);
        drop(factory);
        drop(prepared);
        let rows = trace(&path);
        assert!(rows.starts_with(&prefix));
        let segment = &rows[begin..];
        assert!(segment.iter().all(|row| (row.pid, row.instance) == identity && row.calls == 0));
        assert_eq!(
            segment.iter().filter(|row| row.stage == "restore").count(),
            1
        );
        for stage in ["start", "stop", "consume", "terminate"] {
            assert_eq!(
                segment.iter().filter(|row| row.stage == stage).count(),
                1,
                "{stage}"
            );
        }
        let start = segment.iter().position(|row| row.stage == "start").unwrap();
        let stop = segment.iter().position(|row| row.stage == "stop").unwrap();
        let consume = segment.iter().position(|row| row.stage == "consume").unwrap();
        assert!(start < stop && stop < consume);
        assert!(!segment[start].default && !segment[stop].default);
        assert_eq!(segment[consume].default, drift);
        assert!(segment.iter().all(|row| !row.active));
        assert_eq!(segment.last().unwrap().stage, "terminate");
        for index in [0, 1] {
            assert!(
                segment[consume + 1..].iter().any(|row| row.stage == "census"
                    && row.index == index
                    && row.default == drift
                    && !row.active)
            );
        }

        if let Some(configuration) = captured {
            // Fresh canonical consumer of actual captured bytes and the ORIGINAL
            // expected registration. No re-Probe that could bless changed rows.
            let path = directory.join("captured-stable-pcm.trace");
            let _environment = TraceEnvironment::select(&path);
            let mut runtime = build(&helper, registration, configuration)
                .expect("nonarming captured state must prepare a fresh canonical Runtime");
            let installation = AudioOwnerInstallationSource::new();
            let (registration, verifier) = installation.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = installation
                .reserve_install(&registration, None)
                .and_then(|reservation| installation.begin_commit(reservation))
                .unwrap()
                .commit()
                .unwrap();
            runtime
                .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
                .unwrap();
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
            let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
            let handle = observe_child(object.process_id().get());
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .expect("captured controller state must not restart a cohort");
            let mut pcm = vec![f32::NAN; BLOCK * 2];
            runtime
                .render_into(
                    AudioRenderRequest { start_sample: 0, frames: BLOCK },
                    &mut pcm,
                )
                .expect("ordinary canonical Main PCM from the nonarming captured configuration");
            for frame in pcm.chunks_exact(2) {
                assert_eq!(
                    frame,
                    [0.125, -0.0625],
                    "independent ordinary-effect literal PCM"
                );
            }
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
            drop(runtime);
            assert_child_exit(handle);
            let rows = trace(&path);
            assert!(
                !rows.iter().any(|row| row.stage == "consume"),
                "captured commands must not rearm"
            );
            let ordinary: Vec<_> = rows
                .into_iter()
                .filter(|row| !matches!(row.stage.as_str(), "start" | "stop"))
                .collect();
            let processed: Vec<_> = lifetimes(&ordinary)
                .into_iter()
                .filter(|segment| segment.last().unwrap().calls != 0)
                .collect();
            assert_eq!(processed.len(), 1);
            assert_eq!(processed[0][0].pid, object.process_id().get());
            assert_census_and_exit(processed[0], 1, false);
        }
    }
}
