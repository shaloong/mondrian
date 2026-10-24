//! Actual normal Product: restored zero-input topology, canonical Note DSP and
//! a cold Ready shape refusal. Retained topology changes/state replacement,
//! third-party DSP, RT, whole-heap and 100ms natural shutdown remain unqualified.
use super::native_vst3_note_worker::{audit_with_mode, NativeCall};
use super::*;
use crate::{
    AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioExecutionError, AudioMediaResolver,
    AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease, AudioNoteCompletionPolicy,
    AudioProcessorCursorEvidence, AudioProcessorCursorState, AudioProcessorFactory,
    AudioProgramRuntime, AudioRuntimeResourceGrant, AudioSessionCursorEvidence,
    AudioSessionCursorIdentity, AudioStateEntry, NativeAudioObjectObservation, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource, AudioSourceComponentId,
    AuthoringList, ExecutionCancellationToken, MusicalTimeReference, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorDefinitionRef,
};
use mondrian_timeline::Sequence;
use std::collections::BTreeMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const TOTAL: usize = 14;
const BOUND: usize = 2 * 1024 * 1024;
struct Materials {
    helper: PathBuf,
    binary: PathBuf,
    directory: PathBuf,
    catalog: Arc<DiscoveredVst3AudioProcessorSpecResolver>,
}
impl Materials {
    fn load() -> Self {
        let file = |name| {
            let path =
                PathBuf::from(std::env::var_os(name).expect("explicit actual native material"));
            assert!(path.is_absolute() && path.is_file(), "{name}");
            path
        };
        let helper = file("MONDRIAN_VST3_TEST_HELPER");
        let binary = file("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN");
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR")
                .expect("caller supplies an existing absolute audit directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [binary.clone()])
                .expect("actual current Discovery4, never relabeled older JSON"),
        );
        Self { helper, binary, directory, catalog }
    }
    fn probe(&self, render: AudioRenderContract) -> Vst3PluginRegistration {
        let registration = probe_vst3_plugin_registration(
            &self.helper,
            &self.binary,
            CLASS,
            render,
            Some(&configuration(6)),
        )
        .expect("fresh restored MNE6 actual normal Product probe");
        assert_eq!(registration.main_audio_input, Vst3MainAudioInput::Absent);
        assert!(registration.parameters.is_empty());
        assert!(registration.auxiliary_inputs.buses.is_empty());
        assert_eq!(registration.event_bus_contract.inputs.len(), 2);
        assert!(registration.event_bus_contract.outputs.is_empty());
        let rows = registration.event_bus_contract.inputs.as_slice();
        assert_eq!(
            (
                rows[0].native_index,
                rows[0].channel_count,
                rows[0].host_active
            ),
            (0, 16, true)
        );
        assert!(!rows[1].host_active);
        registration
    }
    fn authored(&self, render: AudioRenderContract) -> Sequence {
        let instance = self.catalog.create_instance(CLASS, render, Some(configuration(6))).unwrap();
        assert!(instance.parameters.is_empty());
        let processor_id = instance.id;
        let mut sequence = Sequence::new("actual restored VST3 zero-input Note Runtime");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        sequence.audio_program.musical_time_reference = Some(reference(render.sample_rate));
        let track = sequence.audio_tracks[0].id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(instance);
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id,
                input_port_key: "vst3.note-input.0".into(),
            },
            notes: AuthoringList::from(
                [(0, 3, 0.5), (4, 8, 0.25)]
                    .into_iter()
                    .map(|(start, end, velocity)| AudioNote {
                        id: AudioNoteId::new(),
                        start: time(start, render.sample_rate),
                        end: time(end, render.sample_rate),
                        channel: 3,
                        key: 61,
                        on_velocity: velocity,
                        off_velocity: 0.125,
                    })
                    .collect::<Vec<_>>(),
            ),
        });
        sequence
    }
}
fn render(rate: u32) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: rate,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        // Finite single-fixture Session allowance, not a budget-edge test.
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..test_render_contract()
    }
}
fn configuration(mode: u32) -> Vec<u8> {
    assert!(matches!(mode, 4 | 6));
    let mut component = if mode == 6 { b"MNE6" } else { b"MNE4" }.to_vec();
    component.extend_from_slice(&mode.to_le_bytes());
    component.extend_from_slice(&0_u32.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 12, 12] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}
fn time(sample: i64, rate: u32) -> TimelineTime {
    TimelineTime::new(sample, i64::from(rate)).unwrap()
}
fn reference(rate: u32) -> MusicalTimeReference {
    MusicalTimeReference::new(time(-3, rate), time(1024, rate)).unwrap()
}
struct NoMedia(Arc<AtomicUsize>);
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("owned zero-input score has no media or substitute PCM source".into())
    }
}
#[derive(Default)]
struct Cursors {
    identity: Option<AudioSessionCursorIdentity>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        assert!(self.identity.replace(evidence.identity).is_none());
        assert_eq!(evidence.public_output_lookahead_frames, 0);
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
    let mut evidence = Cursors::default();
    runtime.visit_continuity_evidence(&mut evidence).unwrap();
    assert_eq!(evidence.processors.len(), 1);
    let processor = evidence.processors[0];
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        state => panic!("no exact current zero-input owner: {state:?}"),
    };
    runtime
        .native_owner_lease(AudioNativeProcessorAddress {
            root: evidence.identity.unwrap(),
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch,
        })
        .unwrap()
}
fn install(runtime: &mut AudioProgramRuntime) -> AudioOwnerInstallationSource {
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .unwrap();
    runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .unwrap();
    source
}
// Query/synchronize HANDLE only. This never owns process lifetime or creates a
// second processor; the caller keeps the real Runtime/processor until Drop.
struct ObservedChild {
    object: NativeAudioObjectObservation,
    handle: OwnedHandle,
    path: PathBuf,
    prefix: usize,
}
impl ObservedChild {
    fn new(object: NativeAudioObjectObservation, directory: &std::path::Path) -> Self {
        let pid = object.process_id().get();
        assert_ne!(pid, std::process::id());
        // SAFETY: the real authenticated native owner supplied the live PID.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
            258
        );
        let path = directory.join(format!("mne6-{pid}.audit"));
        let prefix = match std::fs::metadata(&path) {
            Ok(metadata) => usize::try_from(metadata.len()).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("actual audit prefix: {error}"),
        };
        assert!(prefix <= BOUND);
        Self { object, handle, path, prefix }
    }
    fn finish(self, rate: u32, calls: &[NativeCall], tuples: &[[u8; 48]]) {
        assert_eq!(
            unsafe { WaitForSingleObject(self.handle.as_raw_handle().cast(), 2_000) },
            0
        );
        let mut exit = u32::MAX;
        assert_ne!(
            unsafe { GetExitCodeProcess(self.handle.as_raw_handle().cast(), &mut exit) },
            0
        );
        assert_eq!(exit, 0);
        // This is same-HANDLE OS completion, not the original 100ms shutdown result.
        let length = std::fs::metadata(&self.path).unwrap().len();
        assert!(length >= self.prefix as u64 && length <= BOUND as u64);
        let bytes = std::fs::read(self.path).unwrap();
        assert!(bytes.len() >= self.prefix && bytes.len() <= BOUND);
        let actual = audit_with_mode(&bytes[self.prefix..], self.object.process_id().get(), 6);
        assert_eq!(actual.0, f64::from(rate).to_bits());
        assert_eq!(actual.1, calls);
        assert_eq!(actual.2, tuples);
    }
}
fn expected_tuple(call: u32, sample: i64, offset: usize) -> [u8; 48] {
    let (on, id, velocity) = match sample {
        0 => (true, 0_i32, 0.5_f32),
        3 => (false, 0, 0.125),
        4 => (true, 1, 0.25),
        8 => (false, 1, 0.125),
        _ => unreachable!(),
    };
    let mut row = [0_u8; 48];
    row[..4].copy_from_slice(&call.to_le_bytes());
    row[8..12].copy_from_slice(&(offset as i32).to_le_bytes());
    // Exact small integer divided by a power of two, independent from Core/SDK
    // lowering and from any accumulated PPQ: Q=(absolute sample+3)/1024.
    row[12..20].copy_from_slice(&((sample + 3) as f64 / 1024.0).to_bits().to_le_bytes());
    row[22..24].copy_from_slice(&(if on { 0_u16 } else { 1 }).to_le_bytes());
    row[24..26].copy_from_slice(&3_i16.to_le_bytes());
    row[26..28].copy_from_slice(&61_i16.to_le_bytes());
    row[32..36].copy_from_slice(&velocity.to_bits().to_le_bytes());
    row[40..44].copy_from_slice(&id.to_le_bytes());
    row
}

#[test]
#[ignore = "fresh normal Product Discovery4/Payload3 and MNE6 DLL/audit dir; canonical true zero audio input"]
fn installed_vst3_zero_input_runtime_notes_produce_literal_pcm_ppq_without_media() {
    let materials = Materials::load();
    let processors = IsolatedAudioProcessorResolver::new(materials.catalog.clone());
    let expected = [
        0.25_f32, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ];
    for rate in [44_100, 48_000] {
        let render = render(rate);
        materials.probe(render);
        let sequence = materials.authored(render);
        for partition in [&[4_usize][..], &[3, 2, 4]] {
            let resolves = Arc::new(AtomicUsize::new(0));
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &NoMedia(resolves.clone()),
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .expect("canonical zero-input Note score must reach normal Product DSP");
            let source = install(&mut runtime);
            let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
            let child = ObservedChild::new(object, &materials.directory);
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .unwrap();
            assert_eq!(
                runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                object
            );
            let mut calls = Vec::new();
            let mut tuples = Vec::new();
            let mut start = 0;
            let mut empty_blocks = 0;
            for &size in partition.iter().cycle() {
                let frames = size.min(TOTAL - start);
                let mut pcm = vec![99.0; frames * 2];
                runtime
                    .render_into(
                        AudioRenderRequest { transport_activity: None, start_sample: start as i64, frames },
                        &mut pcm,
                    )
                    .unwrap();
                for (pair, &left) in pcm.chunks_exact(2).zip(&expected[start..start + frames]) {
                    let right = if left == 0.0 { 0.0 } else { -left * 0.5 };
                    assert_eq!(
                        [pair[0].to_bits(), pair[1].to_bits()],
                        [left.to_bits(), right.to_bits()]
                    );
                }
                let first = tuples.len();
                for sample in [0_i64, 3, 4, 8] {
                    if (start..start + frames).contains(&(sample as usize)) {
                        tuples.push(expected_tuple(
                            calls.len() as u32 + 1,
                            sample,
                            sample as usize - start,
                        ));
                    }
                }
                let count = tuples.len() - first;
                empty_blocks += usize::from(count == 0);
                calls.push((frames as i32, 0, first as u32, count as u32));
                start += frames;
                if start == TOTAL {
                    break;
                }
            }
            assert_eq!(start, TOTAL);
            assert_eq!(tuples.len(), 4);
            assert!(
                empty_blocks > 0,
                "Note-enabled trailing empty batches still reached checked native processing"
            );
            assert_eq!(
                runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                object
            );
            assert_eq!(resolves.load(Ordering::SeqCst), 0);
            drop(runtime);
            child.finish(rate, &calls, &tuples);
            drop(source);
        }
    }
}

#[test]
#[ignore = "fresh normal Product and owned MNE6; cold seeded actual Ready shape refusal, not live state replacement"]
fn installed_vst3_zero_input_factory_rejects_actual_ready_shape_drift() {
    let materials = Materials::load();
    let render = render(48_000);
    let registration = materials.probe(render);
    let definition = AudioProcessorDefinitionRef::Vst3 {
        class_id: registration.class_id,
        vendor: registration.vendor,
        schema_version: 1,
        binary_sha256: Some(registration.binary_sha256),
    };
    let state = configuration(6);
    let parameters = BTreeMap::new();
    let spec = materials
        .catalog
        .resolve(
            AudioProcessorPrepareRequest::new(
                AudioProcessorOccurrence {
                    instance_id: mondrian_core::AudioProcessorInstanceId::new(),
                    owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                    insertion: AudioProcessorInsertionPoint::PreFader,
                },
                &definition,
                &parameters,
                Some(&state),
                render,
            )
            .with_note_input_requested(true)
            .with_musical_time_reference(Some(reference(48_000))),
        )
        .expect("real fresh probe evidence, not a manually fabricated snapshot");
    let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
    assert_eq!(
        factory.note_input_contract().completion,
        AudioNoteCompletionPolicy::SuccessfulOffBlock
    );
    let mut original = factory.create(None).expect("actual original MNE6 Ready owner");
    let object = original.native_audio_object_observation().unwrap().unwrap();
    let child = ObservedChild::new(object, &materials.directory);
    let seeded = factory
        .with_native_configuration_seed(
            NativeConfigurationFormat::Vst3ComponentAndController,
            &configuration(4),
        )
        .expect("pure state seed accepts a real valid MNE4 envelope")
        .expect("the original Factory implements its existing seed capability");
    assert_eq!(seeded.execution_contract(), factory.execution_contract());
    assert_eq!(seeded.note_input_contract(), factory.note_input_contract());
    let error = match seeded.create(None) {
        Ok(_) => panic!("actual restored Present input escaped the expected Absent contract"),
        Err(error) => error,
    };
    assert!(
        matches!(
            &error, AudioProcessorHostError::WorkerFailed { operation: "startup", detail }
                if detail == "invalid audio processor execution contract: VST3 main audio input changed since probe"
        ),
        "only the actual create/startup failure is this gate's evidence; actual={error:?}"
    );
    assert_eq!(
        original.native_audio_object_observation().unwrap(),
        Some(object)
    );
    drop(original);
    child.finish(48_000, &[], &[]);
    // The failed create's sole child is settled by the unchanged supervisor and
    // external runner Job owner. Its teardown audit may be preempted by kill.
    // No live Note state replacement or retained topology change was requested.
}
