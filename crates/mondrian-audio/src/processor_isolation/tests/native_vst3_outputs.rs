//! Owned actual Product VST3 multiple outputs. The SDK fixture is independent
//! of this host graph: one native Note/latency3 advance feeds every output.
//! Offline software PCM only; no device, retained topology change, GUI, third-
//! party algorithm, complete heap, realtime or 100ms graceful-exit claim.
use super::*;
use crate::{
    AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioExecutionError, AudioMediaResolver,
    AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease, AudioProcessorCursorEvidence,
    AudioProcessorCursorState, AudioProgramRuntime, AudioRuntimeResourceGrant,
    AudioSessionCursorEvidence, AudioSessionCursorIdentity, AudioStateEntry,
    NativeAudioObjectObservation, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource, AudioProcessorInstanceId,
    AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, MusicalTimeReference,
    TimelineTime,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::{
    AudioChannelStripOutputPort, AudioMixBus, AudioProcessorOutputPort, AudioRoute,
    AudioRouteDestination, AudioRouteSource, Sequence,
};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const SIDECHAIN_CLASS: &str = "7486AF22234A480B9DF13173F12EB620";
const TOTAL: usize = 14;
const BOUND: usize = 2 * 1024 * 1024;
type NativeCall = (i32, i32, u32, u32);
const SIGNAL: [f32; TOTAL] = [
    0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
];
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
        let sidechain = file("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN");
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR")
                .expect("existing absolute audit directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(
                helper.clone(),
                [binary.clone(), sidechain],
            )
            .expect("actual current normal Product discovery, never a relabeled JSON snapshot"),
        );
        Self { helper, binary, directory, catalog }
    }
    fn probe(
        &self,
        render: AudioRenderContract,
        outputs: u32,
        input: bool,
        nan_call: u32,
    ) -> Vst3PluginRegistration {
        let channels = render.channel_count() as u32;
        let registration =
            self.probe_state(render, &configuration(outputs, channels, input, nan_call));
        assert_eq!(registration.audio_output_count, outputs as u8);
        assert_eq!(registration.main_audio_output_count, 1);
        assert_eq!(
            registration.main_audio_input,
            if input {
                Vst3MainAudioInput::Present
            } else {
                Vst3MainAudioInput::Absent
            }
        );
        assert!(registration.auxiliary_inputs.buses.is_empty());
        registration
    }
    fn probe_state(&self, render: AudioRenderContract, state: &[u8]) -> Vst3PluginRegistration {
        let registration =
            probe_vst3_plugin_registration(&self.helper, &self.binary, CLASS, render, Some(state))
                .expect("actual restored Product multi-output probe");
        assert_eq!(
            registration.execution_contract.algorithmic_latency_frames(),
            3
        );
        assert!(registration.parameters.is_empty());
        assert_eq!(registration.event_bus_contract.inputs.len(), 2);
        assert!(registration.event_bus_contract.outputs.is_empty());
        assert_eq!(
            registration.event_bus_contract.inputs.as_slice()[0].channel_count,
            16
        );
        assert!(registration.event_bus_contract.inputs.as_slice()[0].host_active);
        assert!(!registration.event_bus_contract.inputs.as_slice()[1].host_active);
        registration
    }
    fn authored(
        &self,
        render: AudioRenderContract,
        case: u8,
        nan_call: u32,
    ) -> (Sequence, AudioProcessorInstanceId) {
        let instance = self
            .catalog
            .create_instance(
                CLASS,
                render,
                Some(configuration(
                    3,
                    render.channel_count() as u32,
                    false,
                    nan_call,
                )),
            )
            .unwrap();
        assert!(instance.parameters.is_empty());
        let id = instance.id;
        let mut sequence = Sequence::new("actual native VST3 multiple outputs");
        sequence.settings.audio_channel_layout = render.channel_layout;
        sequence.settings.audio_sample_rate = render.sample_rate;
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
                processor_id: id,
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
        let output = sequence.audio_program.outputs[0].id;
        sequence.audio_program.routes.clear();
        let port = match case {
            0 => AudioProcessorOutputPort::Main,
            1 | 4 => auxiliary(1),
            _ => auxiliary(2),
        };
        sequence.audio_program.routes.push(AudioRoute::new(
            raw(id, port),
            AudioRouteDestination::Output(output),
        ));
        if case == 3 {
            for name in ["fanout A", "fanout B"] {
                let bus = mondrian_core::MixBusId::new();
                sequence.audio_program.buses.push(AudioMixBus {
                    id: bus,
                    name: name.into(),
                    strip: Default::default(),
                });
                sequence.audio_program.routes.push(AudioRoute::new(
                    raw(id, auxiliary(1)),
                    AudioRouteDestination::Bus(bus),
                ));
                sequence.audio_program.routes.push(AudioRoute::new(
                    AudioRouteSource::Bus {
                        bus_id: bus,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    AudioRouteDestination::Output(output),
                ));
            }
        }
        if case == 4 {
            assert_eq!(render.channel_layout, AudioChannelLayout::Stereo);
            let effect = self
                .catalog
                .create_instance(
                    SIDECHAIN_CLASS,
                    render,
                    Some(super::super::vst3_sidechain_snapshot(0.5, 0)),
                )
                .expect(
                    "real existing stereo unity gain, native delay3, zero-filled detector inputs",
                );
            let mut bus = AudioMixBus {
                id: mondrian_core::MixBusId::new(),
                name: "actual second native delay3".into(),
                strip: Default::default(),
            };
            bus.strip.pre_fader.processors.push(effect);
            sequence.audio_program.routes.push(AudioRoute::new(
                raw(id, AudioProcessorOutputPort::Main),
                AudioRouteDestination::Bus(bus.id),
            ));
            sequence.audio_program.routes.push(AudioRoute::new(
                AudioRouteSource::Bus {
                    bus_id: bus.id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Output(output),
            ));
            sequence.audio_program.buses.push(bus);
        }
        let bytes = serde_json::to_vec(&sequence).unwrap();
        let reopened: Sequence = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&sequence).unwrap(),
            serde_json::to_value(&reopened).unwrap()
        );
        (reopened, id)
    }
}
fn configuration(outputs: u32, channels: u32, input: bool, nan_call: u32) -> Vec<u8> {
    let mut component = b"MNE7".to_vec();
    for value in [
        7_u32,
        0,
        outputs | (channels << 8) | (u32::from(input) << 16),
        nan_call,
    ] {
        component.extend_from_slice(&value.to_le_bytes());
    }
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 20, 20] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}
fn render(rate: u32, layout: AudioChannelLayout) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: rate,
        channel_layout: layout,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..test_render_contract()
    }
}
fn time(sample: i64, rate: u32) -> TimelineTime {
    TimelineTime::new(sample, i64::from(rate)).unwrap()
}
fn reference(rate: u32) -> MusicalTimeReference {
    MusicalTimeReference::new(time(-3, rate), time(1024, rate)).unwrap()
}
fn auxiliary(index: u8) -> AudioProcessorOutputPort {
    AudioProcessorOutputPort::Auxiliary { bus_key: format!("vst3.audio-output.{index}") }
}
fn raw(id: AudioProcessorInstanceId, port: AudioProcessorOutputPort) -> AudioRouteSource {
    AudioRouteSource::ProcessorOutput { processor_id: id, port }
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
        Err("owned native Note producer has no media carrier or substituted PCM".into())
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
fn owner(
    runtime: &AudioProgramRuntime,
    id: AudioProcessorInstanceId,
) -> AudioNativeProcessorOwnerLease {
    let mut evidence = Cursors::default();
    runtime.visit_continuity_evidence(&mut evidence).unwrap();
    let processor = *evidence.processors.iter().find(|p| p.occurrence.instance_id == id).unwrap();
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        state => panic!("no exact current native owner: {state:?}"),
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
        .and_then(|r| source.begin_commit(r))
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
struct Child {
    object: NativeAudioObjectObservation,
    handle: OwnedHandle,
    audit: Option<(PathBuf, usize)>,
    active: u32,
    mode: u32,
}
impl Child {
    fn observe(object: NativeAudioObjectObservation, directory: Option<&std::path::Path>) -> Self {
        Self::observe_mode(object, directory, 7)
    }
    fn observe_mode(
        object: NativeAudioObjectObservation,
        directory: Option<&std::path::Path>,
        mode: u32,
    ) -> Self {
        assert!(matches!(mode, 7 | 8));
        let pid = object.process_id().get();
        assert_ne!(pid, std::process::id());
        // Query/synchronize rights only; this is never a second lifetime owner.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
            258
        );
        let audit = directory.map(|directory| {
            let path = directory.join(format!("mne{mode}-{pid}.audit"));
            let prefix = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or_else(|e| {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
                0
            });
            assert!(prefix <= BOUND);
            (path, prefix)
        });
        Self {
            object,
            handle,
            audit,
            active: if mode == 8 { 7 } else { 6 },
            mode,
        }
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
        assert_eq!(
            exit, 0,
            "complete actual Drop, without a claimed 100ms graceful receipt"
        );
        if let Some((path, prefix)) = self.audit {
            let bytes =
                std::fs::read(path).expect("same native component termination publishes MNA4");
            assert!(bytes.len() >= prefix && bytes.len() <= BOUND);
            let bytes = &bytes[prefix..];
            let u32_at = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            assert!(bytes.len() >= 80);
            assert_eq!(&bytes[..4], b"MNA4");
            assert_eq!(
                (
                    u32_at(4),
                    u32_at(8) as usize,
                    u32_at(12),
                    u32_at(16),
                    u32_at(20)
                ),
                (4, bytes.len(), self.mode, self.object.process_id().get(), 1)
            );
            assert_ne!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()), 0);
            assert_eq!((u32_at(32), u32_at(36)), (1, self.active));
            assert_eq!(
                u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
                f64::from(rate).to_bits()
            );
            assert_eq!(
                (u32_at(48), u32_at(60), u32_at(64), u32_at(76)),
                (4, 1024, 8192, 0)
            );
            assert!(u32_at(68) >= 16 && u32_at(72) >= 48);
            let count = u32_at(52) as usize;
            let events = u32_at(56) as usize;
            assert!(count <= 1024 && events <= 8192);
            assert_eq!(bytes.len(), 80 + count * 16 + events * 48);
            let actual = bytes[80..80 + count * 16]
                .chunks_exact(16)
                .map(|r| {
                    (
                        i32::from_le_bytes(r[..4].try_into().unwrap()),
                        i32::from_le_bytes(r[4..8].try_into().unwrap()),
                        u32::from_le_bytes(r[8..12].try_into().unwrap()),
                        u32::from_le_bytes(r[12..16].try_into().unwrap()),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                actual, calls,
                "each actual producer callback, including PDC priming; fanout cannot execute twice"
            );
            let actual = bytes[80 + count * 16..]
                .chunks_exact(48)
                .map(|r| <[u8; 48]>::try_from(r).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                actual, tuples,
                "full same-PID independent Note/PPQ/ID history"
            );
        }
    }
}
fn append_expected(
    calls: &mut Vec<NativeCall>,
    tuples: &mut Vec<[u8; 48]>,
    start: usize,
    frames: usize,
) {
    let call = calls.len() as u32 + 1;
    let first = tuples.len();
    for sample in [0_usize, 3, 4, 8] {
        if (start..start + frames).contains(&sample) {
            let (on, id, velocity) = match sample {
                0 => (true, 0_i32, 0.5_f32),
                3 => (false, 0, 0.125),
                4 => (true, 1, 0.25),
                8 => (false, 1, 0.125),
                _ => unreachable!(),
            };
            let mut row = [0_u8; 48];
            row[..4].copy_from_slice(&call.to_le_bytes());
            row[8..12].copy_from_slice(&((sample - start) as i32).to_le_bytes());
            row[12..20].copy_from_slice(&((sample + 3) as f64 / 1024.0).to_bits().to_le_bytes());
            row[22..24].copy_from_slice(&(if on { 0_u16 } else { 1 }).to_le_bytes());
            row[24..26].copy_from_slice(&3_i16.to_le_bytes());
            row[26..28].copy_from_slice(&61_i16.to_le_bytes());
            row[32..36].copy_from_slice(&velocity.to_bits().to_le_bytes());
            row[40..44].copy_from_slice(&id.to_le_bytes());
            tuples.push(row);
        }
    }
    calls.push((
        frames as i32,
        0,
        first as u32,
        (tuples.len() - first) as u32,
    ));
}
#[test]
#[ignore = "fresh normal Product, MNE7 DLL, existing sidechain DLL and absolute audit directory; owned canonical multi-output/PDC PCM only"]
fn installed_vst3_mne7_outputs_canonical_routes_ports_fanout_and_native_pdc_without_media() {
    let materials = Materials::load();
    let processors = IsolatedAudioProcessorResolver::new(materials.catalog.clone());
    for rate in [44_100, 48_000] {
        for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
            let render = render(rate, layout);
            materials.probe(render, 3, false, 0);
            let count = if layout == AudioChannelLayout::Stereo {
                5
            } else {
                4
            };
            for case in 0..count {
                let (sequence, id) = materials.authored(render, case, 0);
                let authored = serde_json::to_value(&sequence).unwrap();
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
                        .expect("actual normal Product multi-output graph");
                    let lookahead = if case == 4 { 6 } else { 3 };
                    assert_eq!(
                        runtime.public_output_lookahead_frames(),
                        lookahead,
                        "real native latency3 plus real downstream native latency3"
                    );
                    let source = install(&mut runtime);
                    let mut evidence = Cursors::default();
                    runtime.visit_continuity_evidence(&mut evidence).unwrap();
                    assert_eq!(evidence.processors.len(), if case == 4 { 2 } else { 1 });
                    let children = evidence
                        .processors
                        .iter()
                        .map(|p| {
                            let object = runtime
                                .observe_native_owner_object(owner(
                                    &runtime,
                                    p.occurrence.instance_id,
                                ))
                                .unwrap();
                            (
                                p.occurrence.instance_id,
                                Child::observe(
                                    object,
                                    if p.occurrence.instance_id == id {
                                        Some(&materials.directory)
                                    } else {
                                        None
                                    },
                                ),
                            )
                        })
                        .collect::<Vec<_>>();
                    runtime
                        .enter_state(AudioStateEntry {
                            epoch: AudioContinuityEpoch::new(1),
                            start_sample: 0,
                        })
                        .unwrap();
                    let mut calls = Vec::new();
                    let mut tuples = Vec::new();
                    let mut execution = 0;
                    while execution < lookahead {
                        let frames = (lookahead - execution).min(4);
                        append_expected(&mut calls, &mut tuples, execution, frames);
                        execution += frames;
                    }
                    let factor = match case {
                        0 => 1.0,
                        1 => -0.5,
                        2 => 0.25,
                        3 => -0.75,
                        _ => 0.5,
                    };
                    let mut start = 0;
                    for &size in partition.iter().cycle() {
                        let frames = size.min(TOTAL - start);
                        let mut pcm = vec![99.0; frames * render.channel_count()];
                        runtime
                            .render_into(
                                AudioRenderRequest {
                                    transport_activity: None,
                                    start_sample: start as i64,
                                    frames,
                                },
                                &mut pcm,
                            )
                            .unwrap();
                        for (offset, pair) in pcm.chunks_exact(render.channel_count()).enumerate() {
                            for (channel, &actual) in pair.iter().enumerate() {
                                let value = SIGNAL[start + offset]
                                    * factor
                                    * if channel == 0 { 1.0 } else { -0.5 };
                                assert_eq!(
                                    actual.to_bits(),
                                    if value == 0.0 {
                                        0.0_f32.to_bits()
                                    } else {
                                        value.to_bits()
                                    },
                                    "case={case} channel={channel} sample={}",
                                    start + offset
                                );
                            }
                        }
                        append_expected(&mut calls, &mut tuples, execution, frames);
                        execution += frames;
                        start += frames;
                        if start == TOTAL {
                            break;
                        }
                    }
                    assert_eq!(tuples.len(), 4);
                    assert_eq!(resolves.load(Ordering::SeqCst), 0);
                    assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
                    assert_eq!(serde_json::to_value(&sequence).unwrap(), authored);
                    for (child_id, child) in &children {
                        assert_eq!(
                            runtime
                                .observe_native_owner_object(owner(&runtime, *child_id))
                                .unwrap(),
                            child.object
                        );
                    }
                    let mut evidence = Cursors::default();
                    runtime.visit_continuity_evidence(&mut evidence).unwrap();
                    for p in evidence.processors {
                        assert!(
                            matches!(p.state,AudioProcessorCursorState::Active {next_input_signal_sample,..}
                            if next_input_signal_sample.sample()==(TOTAL+lookahead-if p.occurrence.instance_id==id {0}else{3}) as i64)
                        );
                    }
                    runtime
                        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                        .unwrap();
                    drop(runtime);
                    for (_, child) in children {
                        child.finish(rate, &calls, &tuples);
                    }
                    drop(source);
                }
            }
        }
    }
}

fn factory(
    materials: &Materials,
    render: AudioRenderContract,
    outputs: u32,
    input: bool,
    nan_call: u32,
) -> Arc<IsolatedAudioProcessorFactory> {
    let registration = materials.probe(render, outputs, input, nan_call);
    let state = configuration(outputs, render.channel_count() as u32, input, nan_call);
    registered_factory(materials, render, registration, &state, true)
}
fn registered_factory(
    materials: &Materials,
    render: AudioRenderContract,
    registration: Vst3PluginRegistration,
    state: &[u8],
    notes: bool,
) -> Arc<IsolatedAudioProcessorFactory> {
    use crate::{AudioProcessorFactory, AudioProcessorNoteInputContract};
    use mondrian_timeline::audio::AudioProcessorDefinitionRef;
    let outputs = registration.audio_output_count;
    let definition = AudioProcessorDefinitionRef::Vst3 {
        class_id: registration.class_id.clone(),
        vendor: registration.vendor.clone(),
        schema_version: 1,
        binary_sha256: Some(registration.binary_sha256),
    };
    let resolver =
        Vst3AudioProcessorSpecResolver::new(materials.helper.clone(), [registration]).unwrap();
    let parameters = std::collections::BTreeMap::new();
    let spec = resolver
        .resolve(
            AudioProcessorPrepareRequest::new(
                AudioProcessorOccurrence {
                    instance_id: AudioProcessorInstanceId::new(),
                    owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                    insertion: AudioProcessorInsertionPoint::PreFader,
                },
                &definition,
                &parameters,
                Some(state),
                render,
            )
            .with_note_input_requested(notes)
            .with_musical_time_reference(notes.then(|| reference(render.sample_rate))),
        )
        .unwrap();
    let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
    assert_eq!(factory.execution_contract().algorithmic_latency_frames(), 3);
    assert_eq!(
        factory.note_input_contract() != AudioProcessorNoteInputContract::default(),
        notes
    );
    let contract = factory.auxiliary_output_contract();
    assert_eq!(contract.buses.len(), outputs as usize - 1);
    for (index, bus) in contract.buses.iter().enumerate() {
        assert_eq!(bus.bus_key, format!("vst3.audio-output.{}", index + 1));
        assert_eq!(bus.channel_layout, render.channel_layout);
    }
    factory
}
struct Io {
    layout: AudioChannelLayout,
    main: Vec<f32>,
    auxiliary: Vec<(String, Vec<f32>)>,
    notes: Vec<crate::AudioNoteEvent>,
    input_pcm: Option<Vec<f32>>,
}
impl Io {
    fn new(
        render: AudioRenderContract,
        outputs: usize,
        start: usize,
        frames: usize,
        input: bool,
    ) -> Self {
        use crate::{AudioNoteEvent, AudioNoteEventKind};
        let mut main = vec![99.0; frames * render.channel_count()];
        if input {
            for (offset, pair) in main.chunks_exact_mut(render.channel_count()).enumerate() {
                for (channel, sample) in pair.iter_mut().enumerate() {
                    *sample = (start + offset + 1) as f32 * if channel == 0 { 1.0 } else { -2.0 };
                }
            }
        }
        let notes = [0_usize, 3, 4, 8]
            .into_iter()
            .filter(|sample| (start..start + frames).contains(sample))
            .map(|sample| {
                let (on, id, velocity) = match sample {
                    0 => (true, 0, 0.5),
                    3 => (false, 0, 0.125),
                    4 => (true, 1, 0.25),
                    8 => (false, 1, 0.125),
                    _ => unreachable!(),
                };
                AudioNoteEvent {
                    sample_offset: (sample - start) as u32,
                    kind: if on {
                        AudioNoteEventKind::On
                    } else {
                        AudioNoteEventKind::Off
                    },
                    port_index: 0,
                    channel: 3,
                    key: 61,
                    native_note_id: id,
                    velocity,
                }
            })
            .collect();
        Self {
            layout: render.channel_layout,
            main,
            auxiliary: (1..outputs)
                .map(|index| {
                    (
                        format!("vst3.audio-output.{index}"),
                        vec![99.0; frames * render.channel_count()],
                    )
                })
                .collect(),
            notes,
            input_pcm: None,
        }
    }
}
impl AudioProcessorAudioIo for Io {
    fn note_events(&self) -> &[crate::AudioNoteEvent] {
        &self.notes
    }
    fn main_layout(&self) -> AudioChannelLayout {
        self.layout
    }
    fn frames(&self) -> usize {
        self.main.len() / self.layout.channel_count()
    }
    fn main_interleaved(&mut self) -> &mut [f32] {
        &mut self.main
    }
    fn auxiliary_output(&mut self, key: &str) -> Option<crate::AudioProcessorOutputBus<'_>> {
        let layout = self.layout;
        let frames = self.frames();
        self.auxiliary.iter_mut().find(|(name, _)| name == key).map(|(name, pcm)| {
            crate::AudioProcessorOutputBus {
                bus_key: name,
                channel_layout: layout,
                frames,
                interleaved: pcm,
            }
        })
    }
    fn auxiliary_input(&self, key: &str) -> Option<AudioProcessorInputBus<'_>> {
        if key != "vst3.audio-input.0" {
            return None;
        }
        Some(AudioProcessorInputBus {
            bus_key: "vst3.audio-input.0",
            channel_layout: self.layout,
            frames: self.frames(),
            interleaved: self.input_pcm.as_deref()?,
        })
    }
    fn main_and_auxiliary_input(
        &mut self,
        key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        if key != "vst3.audio-input.0" {
            return None;
        }
        let frames = self.frames();
        Some(AudioProcessorMainAndInputBuses {
            main_layout: self.layout,
            frames,
            main_interleaved: &mut self.main,
            auxiliary: AudioProcessorInputBus {
                bus_key: "vst3.audio-input.0",
                channel_layout: self.layout,
                frames,
                interleaved: self.input_pcm.as_deref()?,
            },
        })
    }
}
fn process_io(
    processor: &mut dyn crate::AudioProcessor,
    render: AudioRenderContract,
    start: usize,
    io: &mut Io,
    musical_time_reference: Option<MusicalTimeReference>,
) -> Result<(), AudioProcessorHostError> {
    let frames = io.frames();
    let context = crate::AudioProcessorProcessContext::new(
        AudioRenderRequest {
            transport_activity: None,
            start_sample: start as i64,
            frames,
        },
        AudioSampleRate::new(render.sample_rate).unwrap(),
        render.channel_layout,
        render.processing_mode,
        AudioKernelBackend::ScalarReference,
    )
    // This must equal the factory's frozen reference, independently of
    // whether the current audio block contains any Note events.
    .with_musical_time_reference(musical_time_reference);
    processor.process(
        context,
        io,
        crate::AudioParameterEventBatch::new(start as i64, frames, &[], &[], &[]),
    )
}
#[test]
#[ignore = "fresh normal Product and MNE7; actual mono/stereo/present/32-output Ready plus per-bus literal native PCM and complete audit"]
fn installed_vst3_mne7_outputs_product_covers_present_input_mono_and_32_bus_storage() {
    use crate::AudioProcessorFactory;
    const ABSENT_L: [f32; 14] = [
        0.0, 0.0, 0.0, 0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0, 0.0, 0.0,
    ];
    const ABSENT_R: [f32; 14] = [
        0.0, 0.0, 0.0, -0.125, -0.125, -0.125, 0.0, -0.0625, -0.0625, -0.0625, -0.0625, 0.0, 0.0,
        0.0,
    ];
    const PRESENT_L: [f32; 14] = [
        0.0, 0.0, 0.0, 0.5, 0.75, 1.0, 1.0, 1.375, 1.625, 1.875, 2.125, 2.25, 2.5, 2.75,
    ];
    const PRESENT_R: [f32; 14] = [
        0.0, 0.0, 0.0, -0.625, -1.125, -1.625, -2.0, -2.5625, -3.0625, -3.5625, -4.0625, -4.5,
        -5.0, -5.5,
    ];
    let materials = Materials::load();
    for rate in [44_100, 48_000] {
        for (outputs, layout, input) in [
            (3, AudioChannelLayout::Stereo, true),
            (32, AudioChannelLayout::Mono, true),
            (32, AudioChannelLayout::Stereo, false),
            (1, AudioChannelLayout::Mono, false),
        ] {
            let render = render(rate, layout);
            let factory = factory(&materials, render, outputs, input, 0);
            let mut processor = factory.create(None).expect("actual bounded complete-output Ready");
            let object = processor.native_audio_object_observation().unwrap().unwrap();
            let mut child = Child::observe(object, Some(&materials.directory));
            child.active = if input { 7 } else { 6 };
            processor.enter_state(0).unwrap();
            let mut calls = Vec::new();
            let mut tuples = Vec::new();
            let mut start = 0;
            for frames in [3_usize, 2, 4, 3, 2] {
                let mut io = Io::new(render, outputs as usize, start, frames, input);
                process_io(
                    processor.as_mut(),
                    render,
                    start,
                    &mut io,
                    Some(reference(render.sample_rate)),
                )
                .unwrap();
                for (index, pcm) in std::iter::once((0, &io.main))
                    .chain(io.auxiliary.iter().enumerate().map(|(i, (_, pcm))| (i + 1, pcm)))
                {
                    // Independent arithmetic expression, unlike the native bit encoder.
                    let factor =
                        (if index % 2 == 0 { 1.0 } else { -1.0 }) / 2_f32.powi(index as i32);
                    for (offset, pair) in pcm.chunks_exact(render.channel_count()).enumerate() {
                        for (channel, &actual) in pair.iter().enumerate() {
                            let literal = match (input, channel) {
                                (false, 0) => ABSENT_L,
                                (false, _) => ABSENT_R,
                                (true, 0) => PRESENT_L,
                                (true, _) => PRESENT_R,
                            };
                            let value = literal[start + offset] * factor;
                            assert_eq!(
                                actual.to_bits(),
                                if value == 0.0 {
                                    0.0_f32.to_bits()
                                } else {
                                    value.to_bits()
                                },
                                "outputs={outputs} bus={index} channel={channel} sample={}",
                                start + offset
                            );
                        }
                    }
                }
                assert_eq!(
                    processor.native_audio_object_observation().unwrap(),
                    Some(object)
                );
                append_expected(&mut calls, &mut tuples, start, frames);
                start += frames;
            }
            assert_eq!(start, TOTAL);
            assert_eq!(tuples.len(), 4);
            drop(processor);
            child.finish(rate, &calls, &tuples);
        }
    }
}
#[test]
#[ignore = "fresh normal Product and MNE7; actual cold restored count mismatch on create, not activation or retained topology drift"]
fn installed_vst3_mne7_outputs_product_refuses_cold_native_shape_drift() {
    use crate::{AudioProcessorFactory, NativeConfigurationFormat};
    let materials = Materials::load();
    let render = render(48_000, AudioChannelLayout::Stereo);
    let factory = factory(&materials, render, 3, false, 0);
    let mut original = factory.create(None).unwrap();
    let object = original.native_audio_object_observation().unwrap().unwrap();
    let child = Child::observe(object, Some(&materials.directory));
    let seeded = factory
        .with_native_configuration_seed(
            NativeConfigurationFormat::Vst3ComponentAndController,
            &configuration(2, 2, false, 0),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        seeded.auxiliary_output_contract(),
        factory.auxiliary_output_contract()
    );
    assert_eq!(seeded.execution_contract(), factory.execution_contract());
    let error = seeded
        .create(None)
        .err()
        .expect("actual two native outputs cannot satisfy original three-output Ready");
    assert!(
        matches!(error,AudioProcessorHostError::WorkerFailed {operation:"startup",detail}
        if detail=="invalid audio processor execution contract: VST3 audio output shape changed since probe")
    );
    assert_eq!(
        original.native_audio_object_observation().unwrap(),
        Some(object)
    );
    drop(original);
    child.finish(48_000, &[], &[]);
}
#[test]
#[ignore = "fresh normal Product and MNE7; last unrouted Aux NaN after successful native priming must poison without publishing a public window"]
fn installed_vst3_mne7_outputs_unrouted_aux_nan_never_publishes_a_partial_window() {
    let materials = Materials::load();
    let render = render(48_000, AudioChannelLayout::Stereo);
    // Only Main has an authored route. The native Aux2 still has its actual
    // same-slot storage and writes NaN only on callback2 after complete DSP.
    let (sequence, id) = materials.authored(render, 0, 2);
    let resolves = Arc::new(AtomicUsize::new(0));
    let processors = IsolatedAudioProcessorResolver::new(materials.catalog.clone());
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &NoMedia(resolves.clone()),
        &processors,
        render,
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .unwrap();
    let source = install(&mut runtime);
    let object = runtime.observe_native_owner_object(owner(&runtime, id)).unwrap();
    let child = Child::observe(object, None);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let mut pcm = [99.0; 8];
    let error = runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 4,
            },
            &mut pcm,
        )
        .unwrap_err();
    assert!(
        matches!(error,AudioExecutionError::ProcessorHost(AudioProcessorHostError::WorkerFailed {
        operation:"block processing",detail
    }) if detail.contains("VST3 output contains invalid extents or nonfinite PCM"))
    );
    assert_eq!(
        pcm, [0.0; 8],
        "finite Main and Aux1 from the failed call never become public PCM"
    );
    let mut evidence = Cursors::default();
    runtime.visit_continuity_evidence(&mut evidence).unwrap();
    assert_eq!(evidence.processors.len(), 1);
    assert!(matches!(
        evidence.processors[0].state,
        AudioProcessorCursorState::Failed
    ));
    for _ in 0..2 {
        pcm.fill(99.0);
        let before = pcm;
        assert!(matches!(
            runtime.render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 4
                },
                &mut pcm
            ),
            Err(AudioExecutionError::ProcessorHost(
                AudioProcessorHostError::PoisonedInstance
            ))
        ));
        assert_eq!(
            pcm, before,
            "terminal preflight refuses before admitted rendering and preserves every caller sample"
        );
    }
    assert_eq!(resolves.load(Ordering::SeqCst), 0);
    drop(runtime);
    // Terminal worker failure may be killed before MNA4 termination; this gate
    // claims settlement and non-publication, never complete failed-tail tuples.
    assert_eq!(
        unsafe { WaitForSingleObject(child.handle.as_raw_handle().cast(), 2_000) },
        0
    );
    let mut exit = u32::MAX;
    assert_ne!(
        unsafe { GetExitCodeProcess(child.handle.as_raw_handle().cast(), &mut exit) },
        0
    );
    drop(source);
}

// MNE8 changes roles only: all 20B envelopes are independent literal native
// configurations, not a relabeled Product registration or CLAP state.
fn roles_configuration(auxiliary_input: bool, main_outputs: u32) -> Vec<u8> {
    let mut state = configuration(3, 2, true, 0);
    let shape =
        3 | (2 << 8) | (1 << 16) | (u32::from(auxiliary_input) << 17) | (main_outputs << 18);
    for offset in [28, 48] {
        state[offset..offset + 4].copy_from_slice(b"MNE8");
        state[offset + 4..offset + 8].copy_from_slice(&8_u32.to_le_bytes());
        state[offset + 12..offset + 16].copy_from_slice(&shape.to_le_bytes());
    }
    state
}
fn roles_factory(materials: &Materials, notes: bool) -> Arc<IsolatedAudioProcessorFactory> {
    use crate::AudioProcessorFactory;
    let render = render(48_000, AudioChannelLayout::Stereo);
    let state = roles_configuration(true, 3);
    let registration = materials.probe_state(render, &state);
    assert_eq!(registration.main_audio_input, Vst3MainAudioInput::Absent);
    assert_eq!(
        (
            registration.audio_output_count,
            registration.main_audio_output_count
        ),
        (3, 3)
    );
    assert_eq!(registration.auxiliary_inputs.buses.len(), 1);
    assert_eq!(
        registration.auxiliary_inputs.buses[0].bus_key,
        "vst3.audio-input.0"
    );
    assert_eq!(
        registration.auxiliary_inputs.buses[0].channel_layout,
        AudioChannelLayout::Stereo
    );
    let factory = registered_factory(materials, render, registration, &state, notes);
    assert_eq!(factory.auxiliary_input_contract().buses.len(), 1);
    assert_eq!(
        factory.auxiliary_input_contract().buses[0].bus_key,
        "vst3.audio-input.0"
    );
    factory
}
fn roles_expected(notes: bool, routed: bool, channel: usize) -> &'static [f32; TOTAL] {
    const ZERO: [f32; TOTAL] = [0.0; TOTAL];
    const VOICE: [[f32; TOTAL]; 2] = [
        [
            0.0, 0.0, 0.0, 0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0, 0.0, 0.0,
        ],
        [
            0.0, 0.0, 0.0, -0.125, -0.125, -0.125, 0.0, -0.0625, -0.0625, -0.0625, -0.0625, 0.0,
            0.0, 0.0,
        ],
    ];
    const INPUT: [[f32; TOTAL]; 2] = [
        [
            0.0, 0.0, 0.0, 0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 2.75,
        ],
        [
            0.0, 0.0, 0.0, -0.5, -1.0, -1.5, -2.0, -2.5, -3.0, -3.5, -4.0, -4.5, -5.0, -5.5,
        ],
    ];
    const BOTH: [[f32; TOTAL]; 2] = [
        [
            0.0, 0.0, 0.0, 0.5, 0.75, 1.0, 1.0, 1.375, 1.625, 1.875, 2.125, 2.25, 2.5, 2.75,
        ],
        [
            0.0, 0.0, 0.0, -0.625, -1.125, -1.625, -2.0, -2.5625, -3.0625, -3.5625, -4.0625, -4.5,
            -5.0, -5.5,
        ],
    ];
    match (notes, routed) {
        (false, false) => &ZERO,
        (true, false) => &VOICE[channel],
        (false, true) => &INPUT[channel],
        (true, true) => &BOTH[channel],
    }
}
fn append_roles_expected(
    calls: &mut Vec<NativeCall>,
    tuples: &mut Vec<[u8; 48]>,
    start: usize,
    frames: usize,
    notes: bool,
) {
    if notes {
        append_expected(calls, tuples, start, frames);
    } else {
        calls.push((frames as i32, 0, 0, 0));
    }
}
#[test]
#[ignore = "fresh normal Product and owned MNE8; real Aux0 mapping, None/Some Note, literal three-Main PCM, independent L/R and no working-Main leakage"]
fn installed_vst3_mne8_roles_product_routes_aux0_for_none_and_some_without_main_leakage() {
    use crate::AudioProcessorFactory;
    let materials = Materials::load();
    let render = render(48_000, AudioChannelLayout::Stereo);
    for notes in [false, true] {
        let factory = roles_factory(&materials, notes);
        for routed in [false, true] {
            let mut processor = factory.create(None).expect("actual MNE8 complete-output Ready");
            let object = processor.native_audio_object_observation().unwrap().unwrap();
            let child = Child::observe_mode(object, Some(&materials.directory), 8);
            processor.enter_state(0).unwrap();
            let mut calls = Vec::new();
            let mut tuples = Vec::new();
            let mut start = 0;
            for frames in [3_usize, 2, 4, 3, 2] {
                let mut io = Io::new(render, 3, start, frames, false);
                for pair in io.main.chunks_exact_mut(2) {
                    pair.copy_from_slice(&[99.0, -77.0]);
                }
                if !notes {
                    io.notes.clear();
                }
                io.input_pcm = Some(
                    (0..frames)
                        .flat_map(|frame| {
                            let value = if routed {
                                (start + frame + 1) as f32
                            } else {
                                0.0
                            };
                            [value, -value * 2.0]
                        })
                        .collect(),
                );
                process_io(
                    processor.as_mut(),
                    render,
                    start,
                    &mut io,
                    notes.then(|| reference(render.sample_rate)),
                )
                .unwrap();
                for (index, pcm) in std::iter::once((0, &io.main))
                    .chain(io.auxiliary.iter().enumerate().map(|(i, (_, pcm))| (i + 1, pcm)))
                {
                    let factor = [1.0_f32, -0.5, 0.25][index];
                    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
                        for (channel, &actual) in pair.iter().enumerate() {
                            let expected =
                                roles_expected(notes, routed, channel)[start + offset] * factor;
                            assert_eq!(
                                actual.to_bits(),
                                if expected == 0.0 {
                                    0_f32.to_bits()
                                } else {
                                    expected.to_bits()
                                },
                                "notes={notes} routed={routed} bus={index} ch={channel} sample={}",
                                start + offset
                            );
                        }
                    }
                }
                assert_eq!(
                    processor.native_audio_object_observation().unwrap(),
                    Some(object)
                );
                append_roles_expected(&mut calls, &mut tuples, start, frames, notes);
                start += frames;
            }
            assert_eq!(start, TOTAL);
            assert_eq!(tuples.len(), if notes { 4 } else { 0 });
            drop(processor);
            child.finish(48_000, &calls, &tuples);
        }
    }
}

#[test]
#[ignore = "fresh normal Product, owned MNE7/MNE8 and audit directory; two actual delay3 stages, Aux0 route/unroute, three outputs, fanout and complete Source/owner lifecycle"]
fn installed_vst3_mne8_roles_canonical_preserves_aux_routing_and_native_pdc() {
    use mondrian_timeline::AudioProcessorSidechainRoute;
    // Source Main + consumer Main + two copies of output1 + output2.
    // Distinct literal public PCM, independent of the native bus-factor encoder.
    const ROUTED: [f32; TOTAL] = [
        0.265625, 0.265625, 0.265625, 0.0, 0.1328125, 0.1328125, 0.1328125, 0.1328125, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0,
    ];
    let materials = Materials::load();
    let processors = IsolatedAudioProcessorResolver::new(materials.catalog.clone());
    let render = render(48_000, AudioChannelLayout::Stereo);
    for routed in [false, true] {
        let (mut sequence, producer) = materials.authored(render, 0, 0);
        let consumer = materials
            .catalog
            .create_instance(CLASS, render, Some(roles_configuration(true, 3)))
            .expect("real Aux0/three-Main instance");
        let target = consumer.id;
        let track = sequence.audio_tracks[0].id;
        // The earlier producer supplies nonzero working Main even when Aux0
        // has no Route. The native consumer must ignore this graph carrier.
        sequence
            .audio_program
            .track_channels
            .get_mut(&track)
            .unwrap()
            .strip
            .post_fader
            .processors
            .push(consumer);
        let output = sequence.audio_program.outputs[0].id;
        for port in [AudioProcessorOutputPort::Main, auxiliary(2)] {
            sequence.audio_program.routes.push(AudioRoute::new(
                raw(target, port),
                AudioRouteDestination::Output(output),
            ));
        }
        for name in ["role output1 fanout A", "role output1 fanout B"] {
            let bus = AudioMixBus {
                id: mondrian_core::MixBusId::new(),
                name: name.into(),
                strip: Default::default(),
            };
            sequence.audio_program.routes.push(AudioRoute::new(
                raw(target, auxiliary(1)),
                AudioRouteDestination::Bus(bus.id),
            ));
            sequence.audio_program.routes.push(AudioRoute::new(
                AudioRouteSource::Bus {
                    bus_id: bus.id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Output(output),
            ));
            sequence.audio_program.buses.push(bus);
        }
        if routed {
            sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
                raw(producer, AudioProcessorOutputPort::Main),
                target,
                "vst3.audio-input.0",
            ));
        }
        let authored = serde_json::to_value(&sequence).unwrap();
        let sequence: Sequence = serde_json::from_value(authored.clone()).unwrap();
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
                .expect("actual Aux-only/leading-Main canonical DAG");
            assert_eq!(
                runtime.public_output_lookahead_frames(),
                6,
                "native producer delay3 plus native Aux-only consumer delay3"
            );
            let source = install(&mut runtime);
            let producer_object =
                runtime.observe_native_owner_object(owner(&runtime, producer)).unwrap();
            let target_object =
                runtime.observe_native_owner_object(owner(&runtime, target)).unwrap();
            assert_ne!(producer_object, target_object);
            let producer_child =
                Child::observe_mode(producer_object, Some(&materials.directory), 7);
            let target_child = Child::observe_mode(target_object, Some(&materials.directory), 8);
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .unwrap();
            let mut producer_calls = Vec::new();
            let mut producer_tuples = Vec::new();
            let mut target_calls = Vec::new();
            let mut target_tuples = Vec::new();
            let mut execution = 0;
            while execution < 6 {
                let frames = (6 - execution).min(4);
                append_expected(&mut producer_calls, &mut producer_tuples, execution, frames);
                append_roles_expected(
                    &mut target_calls,
                    &mut target_tuples,
                    execution,
                    frames,
                    false,
                );
                execution += frames;
            }
            let mut start = 0;
            for &size in partition.iter().cycle() {
                let frames = size.min(TOTAL - start);
                let mut pcm = vec![99.0; frames * 2];
                runtime
                    .render_into(
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start as i64,
                            frames,
                        },
                        &mut pcm,
                    )
                    .unwrap();
                for (offset, pair) in pcm.chunks_exact(2).enumerate() {
                    let expected = if routed {
                        ROUTED[start + offset]
                    } else {
                        SIGNAL[start + offset]
                    };
                    for (channel, &actual) in pair.iter().enumerate() {
                        let value = expected * if channel == 0 { 1.0 } else { -0.5 };
                        assert_eq!(
                            actual.to_bits(),
                            if value == 0.0 {
                                0_f32.to_bits()
                            } else {
                                value.to_bits()
                            },
                            "routed={routed} ch={channel} sample={}",
                            start + offset
                        );
                    }
                }
                append_expected(&mut producer_calls, &mut producer_tuples, execution, frames);
                append_roles_expected(
                    &mut target_calls,
                    &mut target_tuples,
                    execution,
                    frames,
                    false,
                );
                execution += frames;
                start += frames;
                if start == TOTAL {
                    break;
                }
            }
            assert_eq!(producer_tuples.len(), 4);
            assert!(
                target_tuples.is_empty(),
                "Aux-only consumer has None Note admission"
            );
            assert_eq!(resolves.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
            assert_eq!(serde_json::to_value(&sequence).unwrap(), authored);
            assert_eq!(
                runtime.observe_native_owner_object(owner(&runtime, producer)).unwrap(),
                producer_object
            );
            assert_eq!(
                runtime.observe_native_owner_object(owner(&runtime, target)).unwrap(),
                target_object
            );
            let mut evidence = Cursors::default();
            runtime.visit_continuity_evidence(&mut evidence).unwrap();
            assert_eq!(evidence.processors.len(), 2);
            for processor in evidence.processors {
                let expected = if processor.occurrence.instance_id == producer {
                    TOTAL + 6
                } else {
                    TOTAL + 3
                };
                assert!(
                    matches!(processor.state, AudioProcessorCursorState::Active { next_input_signal_sample, .. }
                    if next_input_signal_sample.sample() == expected as i64)
                );
            }
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
            drop(runtime);
            producer_child.finish(48_000, &producer_calls, &producer_tuples);
            target_child.finish(48_000, &target_calls, &target_tuples);
            drop(source);
        }
    }
}

#[test]
#[ignore = "fresh normal Product and owned MNE8; two separately legal same-count cold role changes must refuse frozen Ready identity, not a native live-fault claim"]
fn installed_vst3_mne8_roles_product_refuses_same_count_role_drift() {
    use crate::{AudioProcessorFactory, NativeConfigurationFormat};
    let materials = Materials::load();
    let render = render(48_000, AudioChannelLayout::Stereo);
    let factory = roles_factory(&materials, false);
    // Cold plug-in declarations and prepared host storage are distinct complete contracts.
    let plugin_contract = factory.spec.plugin_execution_contract;
    let prepared_contract = factory.execution_contract();
    let mut original = factory.create(None).expect("original Aux-only/three-Main Ready");
    let object = original.native_audio_object_observation().unwrap().unwrap();
    let child = Child::observe_mode(object, Some(&materials.directory), 8);
    for (auxiliary_input, main_outputs, expected) in [
        (false, 3, "invalid audio processor execution contract: VST3 main audio input changed since probe"),
        (true, 1, "invalid audio processor execution contract: VST3 audio output shape changed since probe"),
    ] {
        let state = roles_configuration(auxiliary_input, main_outputs);
        // Native discovery independently accepts each alternative, with the
        // exact same bus counts/layout/latency. Rejection below is identity.
        let alternative = materials.probe_state(render, &state);
        assert_eq!(alternative.audio_output_count, 3);
        assert_eq!(alternative.main_audio_output_count, main_outputs as u8);
        assert_eq!(alternative.main_audio_input,
            if auxiliary_input { Vst3MainAudioInput::Absent } else { Vst3MainAudioInput::Present });
        assert_eq!(alternative.auxiliary_inputs.buses.len(), usize::from(auxiliary_input));
        assert_eq!(alternative.execution_contract, plugin_contract);
        let seeded = factory.with_native_configuration_seed(
            NativeConfigurationFormat::Vst3ComponentAndController, &state,
        ).unwrap().unwrap();
        assert_eq!(seeded.execution_contract(), prepared_contract);
        assert_eq!(seeded.auxiliary_input_contract(), factory.auxiliary_input_contract());
        assert_eq!(seeded.auxiliary_output_contract(), factory.auxiliary_output_contract());
        assert_eq!(seeded.note_input_contract(), factory.note_input_contract());
        let error = seeded.create(None).err().expect("same count cannot conceal a cold role change");
        assert!(matches!(error, AudioProcessorHostError::WorkerFailed { operation: "startup", detail }
            if detail == expected));
        assert_eq!(factory.execution_contract(), prepared_contract);
        assert_eq!(original.native_audio_object_observation().unwrap(), Some(object));
    }
    // The rejected candidates cannot mutate or poison the original live owner.
    original.enter_state(0).unwrap();
    let mut io = Io::new(render, 3, 0, 4, false);
    for pair in io.main.chunks_exact_mut(2) {
        pair.copy_from_slice(&[99.0, -77.0]);
    }
    io.notes.clear();
    io.input_pcm = Some(vec![1.0, -2.0, 2.0, -4.0, 3.0, -6.0, 4.0, -8.0]);
    process_io(original.as_mut(), render, 0, &mut io, None).unwrap();
    for (index, pcm) in std::iter::once((0, &io.main))
        .chain(io.auxiliary.iter().enumerate().map(|(i, (_, pcm))| (i + 1, pcm)))
    {
        for (offset, pair) in pcm.chunks_exact(2).enumerate() {
            for (channel, &actual) in pair.iter().enumerate() {
                let value =
                    roles_expected(false, true, channel)[offset] * [1.0_f32, -0.5, 0.25][index];
                assert_eq!(
                    actual.to_bits(),
                    if value == 0.0 {
                        0_f32.to_bits()
                    } else {
                        value.to_bits()
                    }
                );
            }
        }
    }
    assert_eq!(factory.execution_contract(), prepared_contract);
    assert_eq!(
        original.native_audio_object_observation().unwrap(),
        Some(object)
    );
    drop(original);
    child.finish(48_000, &[(4, 0, 0, 0)], &[]);
}
