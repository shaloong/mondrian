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
        let registration = probe_vst3_plugin_registration(
            &self.helper,
            &self.binary,
            CLASS,
            render,
            Some(&configuration(outputs, channels, input, nan_call)),
        )
        .expect("actual restored Product multi-output probe");
        assert_eq!(registration.audio_output_count, outputs as u8);
        assert_eq!(
            registration.main_audio_input,
            if input {
                Vst3MainAudioInput::Present
            } else {
                Vst3MainAudioInput::Absent
            }
        );
        assert_eq!(
            registration.execution_contract.algorithmic_latency_frames(),
            3
        );
        assert!(registration.parameters.is_empty());
        assert!(registration.auxiliary_inputs.buses.is_empty());
        assert_eq!(registration.event_bus_contract.inputs.len(), 2);
        assert!(registration.event_bus_contract.outputs.is_empty());
        assert_eq!(registration.event_bus_contract.inputs[0].channel_count, 16);
        assert!(registration.event_bus_contract.inputs[0].host_active);
        assert!(!registration.event_bus_contract.inputs[1].host_active);
        registration
    }
    fn authored(
        &self,
        render: AudioRenderContract,
        case: u8,
    ) -> (Sequence, AudioProcessorInstanceId) {
        let instance = self
            .catalog
            .create_instance(
                CLASS,
                render,
                Some(configuration(3, render.channel_count() as u32, false, 0)),
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
}
impl Child {
    fn observe(object: NativeAudioObjectObservation, directory: Option<&std::path::Path>) -> Self {
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
            let path = directory.join(format!("mne7-{pid}.audit"));
            let prefix = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or_else(|e| {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
                0
            });
            assert!(prefix <= BOUND);
            (path, prefix)
        });
        Self { object, handle, audit }
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
                (4, bytes.len(), 7, self.object.process_id().get(), 1)
            );
            assert_ne!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()), 0);
            assert_eq!((u32_at(32), u32_at(36)), (1, 6));
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
                let (sequence, id) = materials.authored(render, case);
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
                                AudioRenderRequest { start_sample: start as i64, frames },
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
