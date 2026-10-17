//! Actual Product/Worker Note qualification, not canonical score/ledger execution.
//! Native IDs are supplied explicitly by this local caller. No Source, Note-slot
//! retirement, zero-input, whole-heap, RT or 100ms graceful-exit claim is made.
use super::*;
use crate::{
    AudioNoteCompletionPolicy, AudioNoteEndEvent, AudioNoteEvent, AudioNoteEventKind,
    AudioProcessor, AudioProcessorFactory, AudioProcessorNoteInputContract,
    NativeAudioObjectObservation,
};
use mondrian_core::{AudioProcessorInstanceId, MusicalTimeReference, TimelineTime};
use mondrian_timeline::audio::AudioProcessorDefinitionRef;
use std::collections::BTreeMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const BLOCK: usize = 4;
const AUDIT_BOUND: usize = 2 * 1024 * 1024;

struct Materials {
    helper: PathBuf,
    binary: PathBuf,
    directory: PathBuf,
}
impl Materials {
    fn load() -> Self {
        let file = |name| {
            let path = PathBuf::from(std::env::var_os(name).expect("explicit native material"));
            assert!(path.is_absolute() && path.is_file(), "{name}");
            path
        };
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR")
                .expect("external caller supplies a preexisting absolute audit directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        Self {
            helper: file("MONDRIAN_VST3_TEST_HELPER"),
            binary: file("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN"),
            directory,
        }
    }
    fn probe(&self, render: AudioRenderContract, fail: u32) -> Vst3PluginRegistration {
        let registration = probe_vst3_plugin_registration(
            &self.helper,
            &self.binary,
            CLASS,
            render,
            Some(&configuration(fail)),
        )
        .expect("actual public restored-instance probe through normal Product");
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
        assert!(!rows[1].host_active, "Aux Event stays inactive");
        registration
    }
}
fn render(rate: u32) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: rate,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    }
}
fn configuration(fail: u32) -> Vec<u8> {
    let mut component = b"MNE4".to_vec();
    component.extend_from_slice(&4_u32.to_le_bytes());
    component.extend_from_slice(&fail.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 12, 12] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}
fn reference(rate: u32, near_max: bool) -> MusicalTimeReference {
    let (zero, duration) = if near_max {
        (
            TimelineTime::new(i64::MAX - 17, i64::from(rate)).unwrap(),
            TimelineTime::new(1, 2).unwrap(),
        )
    } else {
        (
            TimelineTime::new(-19, 7).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
    };
    MusicalTimeReference::new(zero, duration).unwrap()
}
fn spec(
    materials: &Materials,
    registration: &Vst3PluginRegistration,
    render: AudioRenderContract,
    fail: u32,
    reference: Option<MusicalTimeReference>,
    notes: bool,
) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
    let resolver =
        Vst3AudioProcessorSpecResolver::new(materials.helper.clone(), [registration.clone()])
            .unwrap();
    let definition = AudioProcessorDefinitionRef::Vst3 {
        class_id: registration.class_id.clone(),
        vendor: registration.vendor.clone(),
        schema_version: 1,
        binary_sha256: Some(registration.binary_sha256),
    };
    let state = configuration(fail);
    resolver.resolve(
        AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: AudioProcessorInstanceId::new(),
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &definition,
            &BTreeMap::new(),
            Some(&state),
            render,
        )
        .with_note_input_requested(notes)
        .with_musical_time_reference(reference),
    )
}
fn assert_contract(spec: &IsolatedAudioProcessorWorkerSpec, enabled: bool) {
    let contract = &spec.note_input_contract;
    contract.validate().unwrap();
    let slots = if enabled {
        assert_eq!(
            contract.completion,
            AudioNoteCompletionPolicy::SuccessfulOffBlock
        );
        assert_eq!(contract.ports.len(), 1);
        let port = &contract.ports[0];
        assert_eq!(port.port_key, "vst3.note-input.0");
        assert_eq!(
            (port.native_id, port.native_index, port.channel_count),
            (0, 0, 16)
        );
        assert_eq!(
            (contract.limits.output_ends, contract.limits.live_voices),
            (0, 0)
        );
        assert_eq!(contract.limits.input_events, 4096);
        contract.limits.input_events
    } else {
        assert_eq!(*contract, AudioProcessorNoteInputContract::default());
        0
    };
    let layout = std::alloc::Layout::array::<vst3_host::midi::PluginEvent>(slots).unwrap();
    let actual = spec.native_lane.expect("actual native lane").adapter_note_input_storage();
    assert_eq!(
        (actual.bytes, actual.alignment),
        (layout.size(), layout.align()),
        "VST owns a typed Vec even at capacity zero; this is not CLAP's neutral zero layout"
    );
}

struct Io {
    main: Vec<f32>,
    notes: Vec<AudioNoteEvent>,
    end_attempts: usize,
}
impl Io {
    fn new(frames: usize, notes: &[AudioNoteEvent]) -> Self {
        Self {
            main: vec![99.0; frames * 2],
            notes: notes.to_vec(),
            end_attempts: 0,
        }
    }
}
impl AudioProcessorAudioIo for Io {
    fn note_events(&self) -> &[AudioNoteEvent] {
        &self.notes
    }
    fn try_push_note_end(&mut self, _: AudioNoteEndEvent) -> bool {
        self.end_attempts += 1;
        false
    }
    fn main_layout(&self) -> AudioChannelLayout {
        AudioChannelLayout::Stereo
    }
    fn frames(&self) -> usize {
        self.main.len() / 2
    }
    fn main_interleaved(&mut self) -> &mut [f32] {
        &mut self.main
    }
    fn auxiliary_input(&self, _: &str) -> Option<AudioProcessorInputBus<'_>> {
        None
    }
    fn main_and_auxiliary_input(&mut self, _: &str) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        None
    }
}
fn note(on: bool, offset: u32, id: i32, velocity: f64) -> AudioNoteEvent {
    AudioNoteEvent {
        sample_offset: offset,
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
}
fn context(
    start: i64,
    frames: usize,
    rate: u32,
    reference: Option<MusicalTimeReference>,
) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { start_sample: start, frames },
        AudioSampleRate::new(rate).unwrap(),
        AudioChannelLayout::Stereo,
        AudioProcessingMode::Offline,
        AudioKernelBackend::ScalarReference,
    )
    .with_musical_time_reference(reference)
}
fn process(
    processor: &mut dyn AudioProcessor,
    start: i64,
    frames: usize,
    rate: u32,
    reference: Option<MusicalTimeReference>,
    io: &mut Io,
) -> Result<(), AudioProcessorHostError> {
    processor.process(
        context(start, frames, rate, reference),
        io,
        AudioParameterEventBatch::new(start, frames, &[], &[], &[]),
    )
}
fn assert_pcm(io: &Io, left: &[f32]) {
    assert_eq!(
        io.end_attempts, 0,
        "SuccessfulOffBlock emits no invented native End"
    );
    assert_eq!(io.main.len(), left.len() * 2);
    for (pair, &left) in io.main.chunks_exact(2).zip(left) {
        let right = if left == 0.0 { 0.0 } else { -left * 0.5 };
        assert_eq!(
            [pair[0].to_bits(), pair[1].to_bits()],
            [left.to_bits(), right.to_bits()]
        );
    }
}

// The sole processor remains the real Factory-created object. This helper only
// retains its read-only observation HANDLE and an audit byte-prefix boundary.
struct Subject {
    processor: Box<dyn AudioProcessor>,
    object: NativeAudioObjectObservation,
    handle: OwnedHandle,
    path: PathBuf,
    prefix: usize,
}
impl Subject {
    fn create(materials: &Materials, spec: IsolatedAudioProcessorWorkerSpec, start: i64) -> Self {
        assert_contract(&spec, true);
        let expected = spec.note_input_contract.clone();
        let factory = IsolatedAudioProcessorFactory::prepare(spec)
            .expect("real validation launch and original strict shutdown");
        assert_eq!(factory.note_input_contract(), expected);
        let mut processor = factory.create().expect("actual Product Note Worker Ready");
        let object = processor.native_audio_object_observation().unwrap().unwrap();
        assert_ne!(object.process_id().get(), std::process::id());
        // SAFETY: this authenticated live supervised object supplies PID. Only
        // query/synchronize rights are retained through Drop; no reaper/kill owner.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
            258
        );
        let path = materials.directory.join(format!("mne4-{}.audit", object.process_id().get()));
        let prefix = match std::fs::metadata(&path) {
            Ok(metadata) => usize::try_from(metadata.len()).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("audit metadata: {error}"),
        };
        assert!(prefix <= AUDIT_BOUND);
        processor
            .enter_state(start)
            .expect("first local signal entry, not a canonical epoch");
        assert_eq!(
            processor.native_audio_object_observation().unwrap(),
            Some(object),
            "the first Note entry preserves its actual Ready object"
        );
        Self { processor, object, handle, path, prefix }
    }
    fn same_object(&mut self) {
        assert_eq!(
            self.processor.native_audio_object_observation().unwrap(),
            Some(self.object)
        );
    }
    fn finish(self, calls: &[(i32, i32, u32, u32)], tuples: &[[u8; 48]], rate: u32) {
        let Self { processor, object, handle, path, prefix } = self;
        drop(processor);
        observe_exit(&handle, true);
        let length = std::fs::metadata(&path)
            .expect("actual component termination published audit")
            .len();
        assert!(length <= AUDIT_BOUND as u64 && length >= prefix as u64);
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.len() <= AUDIT_BOUND && bytes.len() >= prefix);
        let actual = audit(&bytes[prefix..], object.process_id().get());
        assert_eq!(actual.0, f64::from(rate).to_bits());
        assert_eq!(
            actual.1, calls,
            "full exact native calls; pure refusal performed none"
        );
        assert_eq!(
            actual.2, tuples,
            "literal raw tuples from this same Product child"
        );
    }
}
fn observe_exit(handle: &OwnedHandle, success: bool) {
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) },
        0
    );
    let mut exit = u32::MAX;
    assert_ne!(
        unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
        0
    );
    if success {
        assert_eq!(exit, 0);
    }
    // Only same-HANDLE OS completion within 2s, never 100ms graceful evidence.
}
fn u32_at(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index..index + 4].try_into().unwrap())
}
pub(super) type NativeCall = (i32, i32, u32, u32);
pub(super) fn audit(bytes: &[u8], pid: u32) -> (u64, Vec<NativeCall>, Vec<[u8; 48]>) {
    audit_with_mode(bytes, pid, 4)
}
pub(super) fn audit_with_mode(
    bytes: &[u8],
    pid: u32,
    expected_mode: u32,
) -> (u64, Vec<NativeCall>, Vec<[u8; 48]>) {
    assert!(bytes.len() >= 80 && bytes.len() <= AUDIT_BOUND);
    assert_eq!(&bytes[..4], b"MNA4");
    assert_eq!(u32_at(bytes, 4), 4);
    assert_eq!(
        u32_at(bytes, 8) as usize,
        bytes.len(),
        "exactly one complete physical component lifetime"
    );
    match expected_mode {
        4 => assert_eq!(u32_at(bytes, 12), 4),
        6 => assert_eq!(u32_at(bytes, 12), 6),
        _ => panic!("unsupported owned audit mode"),
    }
    assert_eq!(u32_at(bytes, 16), pid);
    assert_eq!(u32_at(bytes, 20), 1);
    assert_ne!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()), 0);
    assert_eq!(u32_at(bytes, 32), 1);
    match expected_mode {
        4 => assert_eq!(u32_at(bytes, 36), 7),
        6 => assert_eq!(u32_at(bytes, 36), 6),
        _ => unreachable!(),
    }
    assert_eq!(u32_at(bytes, 48), BLOCK as u32);
    let (calls, events) = (u32_at(bytes, 52) as usize, u32_at(bytes, 56) as usize);
    assert_eq!((u32_at(bytes, 60), u32_at(bytes, 64)), (1024, 8192));
    assert!(u32_at(bytes, 68) >= 16 && u32_at(bytes, 72) >= 48);
    assert_eq!(u32_at(bytes, 76), 0);
    assert!(calls <= 1024 && events <= 8192);
    assert_eq!(bytes.len(), 80 + calls * 16 + events * 48);
    let history = bytes[80..80 + calls * 16]
        .chunks_exact(16)
        .map(|row| {
            (
                i32::from_le_bytes(row[..4].try_into().unwrap()),
                i32::from_le_bytes(row[4..8].try_into().unwrap()),
                u32_at(row, 8),
                u32_at(row, 12),
            )
        })
        .collect();
    let tuples = bytes[80 + calls * 16..]
        .chunks_exact(48)
        .map(|row| row.try_into().unwrap())
        .collect();
    (
        u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
        history,
        tuples,
    )
}
fn tuple(call: u32, note: AudioNoteEvent, ppq_bits: u64) -> [u8; 48] {
    // Independent literal raw Event layout, not either production conversion.
    let mut row = [0_u8; 48];
    row[..4].copy_from_slice(&call.to_le_bytes()); // bus index0 at4..8
    row[8..12].copy_from_slice(&(note.sample_offset as i32).to_le_bytes());
    row[12..20].copy_from_slice(&ppq_bits.to_le_bytes()); // flags0 at20..22
    let kind = if note.kind == AudioNoteEventKind::On {
        0_u16
    } else {
        1
    };
    row[22..24].copy_from_slice(&kind.to_le_bytes());
    row[24..26].copy_from_slice(&i16::from(note.channel).to_le_bytes());
    row[26..28].copy_from_slice(&i16::from(note.key).to_le_bytes()); // tuning0 at28..32
    row[32..36].copy_from_slice(&(note.velocity as f32).to_bits().to_le_bytes()); // length0 at36..40
    row[40..44].copy_from_slice(&note.native_note_id.to_le_bytes());
    row
}

#[test]
#[ignore = "requires current normal Product IPC17, owned MNE4 DLL and existing absolute audit directory; local Worker only"]
fn installed_vst3_note_worker_delivers_literal_ppq_pcm_and_complete_same_pid_audit() {
    let materials = Materials::load();
    // Independent exact-Fraction binary64 results for offsets0,3,4,8.
    const Q: [[[u64; 4]; 2]; 2] = [
        [
            [
                0x400cf2c1c345a6f2,
                0x400cf2f151373595,
                0x400cf3012b326521,
                0x400cf340931f2351,
            ],
            [
                0x3f07c6f8c751f177,
                0x3f27c6f8c751f177,
                0x3f2db8b6f9266dd5,
                0x3f3abfd7e03c2fa6,
            ],
        ],
        [
            [
                0x400cf2d7a85bcd6b,
                0x400cf303592b5545,
                0x400cf311e970828d,
                0x400cf34c2a8537b0,
            ],
            [
                0x3f05d867c3ece2a5,
                0x3f25d867c3ece2a5,
                0x3f2b4e81b4e81b4f,
                0x3f389374bc6a7efa,
            ],
        ],
    ];
    for (r, rate) in [44_100, 48_000].into_iter().enumerate() {
        let render = render(rate);
        let registration = materials.probe(render, 0);
        for near_max in [false, true] {
            let reference = reference(rate, near_max);
            let start = if near_max { i64::MAX - 16 } else { -17 };
            let spec = spec(&materials, &registration, render, 0, Some(reference), true)
                .expect("local VST3 Note worker must admit explicit anchored Main bus zero");
            let mut subject = Subject::create(&materials, spec, start);
            let events = [
                note(true, 0, 10, 0.5),
                note(false, 3, 10, 0.125),
                note(true, 0, 11, 0.25),
                note(false, 0, 11, 0.125),
            ];
            let mut first = Io::new(4, &events[..2]);
            process(
                subject.processor.as_mut(),
                start,
                4,
                rate,
                Some(reference),
                &mut first,
            )
            .unwrap();
            assert_pcm(&first, &[0.25, 0.25, 0.25, 0.0]);
            subject.same_object();
            let mut second = Io::new(4, &events[2..3]);
            process(
                subject.processor.as_mut(),
                start + 4,
                4,
                rate,
                Some(reference),
                &mut second,
            )
            .unwrap();
            assert_pcm(&second, &[0.125; 4]);
            subject.same_object();
            let mut third = Io::new(1, &events[3..]);
            process(
                subject.processor.as_mut(),
                start + 8,
                1,
                rate,
                Some(reference),
                &mut third,
            )
            .unwrap();
            assert_pcm(&third, &[0.0]);
            subject.same_object();
            let mut empty = Io::new(1, &[]);
            process(
                subject.processor.as_mut(),
                start + 9,
                1,
                rate,
                Some(reference),
                &mut empty,
            )
            .unwrap();
            assert_pcm(&empty, &[0.0]);
            subject.same_object();
            let q = Q[r][usize::from(near_max)];
            subject.finish(
                &[(4, 0, 0, 2), (4, 0, 2, 1), (1, 0, 3, 1), (1, 0, 4, 0)],
                &[
                    tuple(1, events[0], q[0]),
                    tuple(1, events[1], q[1]),
                    tuple(2, events[2], q[2]),
                    tuple(3, events[3], q[3]),
                ],
                rate,
            );
        }
    }
}

#[test]
#[ignore = "requires current normal Product IPC17 and owned MNE4 DLL; cold contracts and pure parent refusals only"]
fn installed_vst3_note_worker_refuses_incompatible_contracts_before_consumption() {
    let materials = Materials::load();
    let render = render(48_000);
    let registration = materials.probe(render, 0);
    let reference = reference(48_000, false);
    assert!(
        matches!(spec(&materials, &registration, render, 0, None, true),
        Err(AudioProcessorHostError::Unavailable(detail)) if detail == "VST3 Note input requires a musical time reference")
    );
    for reference in [None, Some(reference)] {
        let ordinary = spec(&materials, &registration, render, 0, reference, false).unwrap();
        assert_contract(&ordinary, false); // NoNote is explicit prepare state, not empty block.
    }
    for output in [false, true] {
        let mut invalid = registration.clone();
        if output {
            invalid.event_bus_contract.outputs =
                Vst3EventBusContractRows::try_from_slice(&[Vst3EventBusContractRow {
                    native_index: 0,
                    role: Vst3EventBusRole::Main,
                    channel_count: 16,
                    default_active: false,
                    host_active: false,
                }])
                .unwrap();
        } else {
            invalid.event_bus_contract.inputs.as_mut_slice()[0].host_active = false;
        }
        assert!(
            matches!(spec(&materials, &invalid, render, 0, Some(reference), true),
            Err(AudioProcessorHostError::Unavailable(detail)) if detail == "VST3 Note input requires active Main event bus zero and no event outputs")
        );
    }
    let admitted = spec(&materials, &registration, render, 0, Some(reference), true).unwrap();
    let mut subject = Subject::create(&materials, admitted, -17);
    let valid = note(true, 0, 10, 0.5);
    let wrong = MusicalTimeReference::new(
        TimelineTime::new(-20, 7).unwrap(),
        TimelineTime::new(3, 4).unwrap(),
    )
    .unwrap();
    for reference in [None, Some(wrong)] {
        let mut io = Io::new(4, &[valid]);
        let before = io.main.clone();
        assert!(
            matches!(process(subject.processor.as_mut(), -17, 4, 48_000, reference, &mut io),
            Err(AudioProcessorHostError::InvalidContract(detail)) if detail == "isolated processor musical reference changed")
        );
        assert_eq!(io.main, before);
        subject.same_object();
    }
    let mut bad_batches = Vec::new();
    for field in 0..5 {
        let mut last = note(false, 3, 10, 0.125);
        match field {
            0 => last.port_index = 1,
            1 => last.channel = 16,
            2 => last.sample_offset = 4,
            3 => last.velocity = f64::NAN,
            _ => last.native_note_id = -1,
        }
        bad_batches.push(vec![valid, last]);
    }
    bad_batches.push(vec![note(true, 1, 10, 0.5), note(false, 0, 10, 0.125)]);
    bad_batches.push(vec![valid; 4097]);
    for notes in bad_batches {
        let mut io = Io::new(4, &notes);
        let before = io.main.clone();
        assert!(matches!(
            process(
                subject.processor.as_mut(),
                -17,
                4,
                48_000,
                Some(reference),
                &mut io
            ),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
        assert_eq!(io.main, before);
        assert_eq!(io.end_attempts, 0);
        subject.same_object();
    }
    let events = [valid, note(false, 3, 10, 0.125)];
    let mut io = Io::new(4, &events);
    process(
        subject.processor.as_mut(),
        -17,
        4,
        48_000,
        Some(reference),
        &mut io,
    )
    .unwrap();
    assert_pcm(&io, &[0.25, 0.25, 0.25, 0.0]);
    subject.same_object();
    subject.finish(
        &[(4, 0, 0, 2)],
        &[
            tuple(1, events[0], 0x400cf2d7a85bcd6b),
            tuple(1, events[1], 0x400cf303592b5545),
        ],
        48_000,
    );
}

#[test]
#[ignore = "requires current normal Product IPC17 and owned MNE4 DLL; parent fail-closed, no failed-tail audit guarantee"]
fn installed_vst3_note_worker_failures_do_not_replay_or_rebuild() {
    let materials = Materials::load();
    let render = render(48_000);
    let reference = reference(48_000, false);
    for native_failure in [true, false] {
        let fail = if native_failure { 2 } else { 0 };
        let registration = materials.probe(render, fail);
        let admitted = spec(
            &materials,
            &registration,
            render,
            fail,
            Some(reference),
            true,
        )
        .unwrap();
        let mut subject = Subject::create(&materials, admitted, -17);
        let mut prefix = Io::new(4, &[note(true, 0, 10, 0.5), note(false, 3, 10, 0.125)]);
        process(
            subject.processor.as_mut(),
            -17,
            4,
            48_000,
            Some(reference),
            &mut prefix,
        )
        .unwrap();
        assert_pcm(&prefix, &[0.25, 0.25, 0.25, 0.0]);
        subject.same_object();
        let mut failed = Io::new(4, &[note(true, 0, 11, 0.25)]);
        let original = failed.main.clone();
        if native_failure {
            let error = process(
                subject.processor.as_mut(),
                -13,
                4,
                48_000,
                Some(reference),
                &mut failed,
            )
            .unwrap_err();
            assert!(matches!(
                error,
                AudioProcessorHostError::WorkerFailed { operation: "block processing", detail }
                    if detail == "audio processor block execution failed: VST3 processing failed: Plugin process() failed: 0x1"
            ));
        } else {
            let error = subject.processor.enter_state(-17).unwrap_err();
            assert!(
                matches!(error, AudioProcessorHostError::WorkerFailed { operation: "state entry", detail }
                if detail == "audio processor state entry failed: VST3 Note input supports only initial state entry")
            );
        }
        assert_eq!(failed.main, original);
        for _ in 0..2 {
            assert!(matches!(
                process(
                    subject.processor.as_mut(),
                    -13,
                    4,
                    48_000,
                    Some(reference),
                    &mut failed
                ),
                Err(AudioProcessorHostError::PoisonedInstance)
            ));
            assert!(matches!(
                subject.processor.native_audio_object_observation(),
                Err(AudioProcessorHostError::PoisonedInstance)
            ));
            assert!(matches!(
                subject.processor.enter_state(-17),
                Err(AudioProcessorHostError::PoisonedInstance)
            ));
            assert_eq!(failed.main, original);
        }
        // Failed is transmitted before scope join/termination. The parent can
        // kill first, so no mandatory MNA4/failed-tail tuple claim is possible.
        // We prove parent terminal non-reuse, not child teardown completion.
        let Subject { processor, handle, .. } = subject;
        drop(processor);
        observe_exit(&handle, false);
    }
}
