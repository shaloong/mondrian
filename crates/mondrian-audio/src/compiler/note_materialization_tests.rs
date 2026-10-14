//! Actual compiler Note owners and public metadata/error controls.
//! No Runtime grant, public nonempty executor, natural allocator excess or TLS claim.
use super::*;
use crate::{
    AudioKernelBackend, AudioProcessingMode, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorResolver, AudioRenderContract, PreparedAudioPlan,
};
use mondrian_core::{AudioChannelLayout, AuthoringList, TimelineTime};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID,
};
use std::alloc::Layout;
use std::cell::RefCell;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LaneOwner {
    id: AudioNoteLaneId,
    notes_pointer: usize,
    notes_length: usize,
    notes_capacity: usize,
    key_pointer: usize,
    key_length: usize,
    key_capacity: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct Observation {
    requests: usize,
    deliveries: usize,
    requested_lanes: usize,
    requested_bytes: usize,
    rows_pointer: usize,
    rows_length: usize,
    rows_capacity: usize,
    owners: [Option<LaneOwner>; 2],
    owner_count: usize,
}

thread_local! {
    static OBSERVATION: RefCell<Option<Observation>> = const { RefCell::new(None) };
}

pub(super) fn observe_request(lanes: usize, bytes: usize) {
    OBSERVATION.with(|observation| {
        if let Some(value) = observation.borrow_mut().as_mut() {
            value.requests += 1;
            value.requested_lanes = lanes;
            value.requested_bytes = bytes;
        }
    });
}

pub(super) fn observe_owners(lanes: &Vec<CompiledNoteLane>) {
    OBSERVATION.with(|observation| {
        let mut observation = observation.borrow_mut();
        let Some(value) = observation.as_mut() else {
            return;
        };
        value.deliveries += 1;
        value.rows_pointer = lanes.as_ptr() as usize;
        value.rows_length = lanes.len();
        value.rows_capacity = lanes.capacity();
        for lane in lanes {
            if value.owner_count < value.owners.len() {
                value.owners[value.owner_count] = Some(LaneOwner {
                    id: lane.id,
                    notes_pointer: lane.notes.as_ptr() as usize,
                    notes_length: lane.notes.len(),
                    notes_capacity: lane.notes.capacity(),
                    key_pointer: lane.target.input_port_key.as_ptr() as usize,
                    key_length: lane.target.input_port_key.len(),
                    key_capacity: lane.target.input_port_key.capacity(),
                });
            }
            value.owner_count += 1;
        }
    });
}

fn observed<R>(run: impl FnOnce() -> R) -> (R, Observation) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            OBSERVATION.with(|value| {
                value.borrow_mut().take();
            });
        }
    }
    OBSERVATION.with(|value| {
        let mut value = value.borrow_mut();
        assert!(value.is_none());
        *value = Some(Observation::default());
    });
    let clear = Clear;
    let result = run();
    let observation = OBSERVATION.with(|value| value.borrow_mut().take().unwrap());
    drop(clear);
    (result, observation)
}

// The same existing TLS observer brackets the real public Runtime producer.
pub(super) fn observe_counts<R>(run: impl FnOnce() -> R) -> (R, usize, usize) {
    let (result, observation) = observed(run);
    (result, observation.requests, observation.deliveries)
}

fn time(n: i64, d: i64) -> TimelineTime {
    TimelineTime::new(n, d).unwrap()
}
fn lane_id(value: u128) -> AudioNoteLaneId {
    AudioNoteLaneId(Uuid::from_u128(value))
}
fn note_id(value: u128) -> AudioNoteId {
    AudioNoteId(Uuid::from_u128(value))
}
fn note(id: u128, start: i64, end: i64) -> AudioNote {
    AudioNote {
        id: note_id(id),
        start: time(start, 48_000),
        end: time(end, 48_000),
        channel: 3,
        key: 60,
        on_velocity: -0.0,
        off_velocity: f64::from_bits(0x3fd3_3333_3333_3335),
    }
}
fn sequence() -> Sequence {
    let mut sequence = Sequence::new("compiler-owned score");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track_id = sequence.audio_tracks[0].id;
    let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let processor_id = processor.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(processor);
    for (id, key, notes) in [
        (20, "a", vec![note(301, 2, 3), note(201, 0, 1)]),
        (10, "é", vec![note(101, 1, 2)]),
    ] {
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: lane_id(id),
            target: AudioNoteTarget {
                track_id,
                processor_id,
                input_port_key: key.to_owned(),
            },
            notes: AuthoringList::from(notes),
        });
    }
    sequence
}
fn request(sequence: &Sequence) -> AudioCompileRequest {
    AudioCompileRequest::program(sequence.audio_program.outputs[0].id)
}
fn compile(sequence: &Sequence) -> CompiledAudioProgram {
    compile_audio_program(sequence, request(sequence)).unwrap()
}
fn independent_expected() -> usize {
    Layout::array::<CompiledNoteLane>(2).unwrap().size()
        + Layout::array::<AudioNote>(2).unwrap().size()
        + Layout::array::<AudioNote>(1).unwrap().size()
        + 3
}
fn is_capacity_refusal(error: AudioCompileError) {
    assert!(
        matches!(error, AudioCompileError::InvalidPreparedGraph(detail)
        if detail == "compiled Note storage capacity exceeds request")
    );
}
fn is_payload_refusal(error: AudioCompileError) {
    assert!(
        matches!(error, AudioCompileError::InvalidPreparedGraph(detail)
        if detail == "compiled Note payload exceeds request")
    );
}

#[test]
fn real_compiler_owners_use_unequal_target_layouts_without_cloning_author_spares() {
    let mut source = sequence();
    source.audio_program.note_lanes[0].target.input_port_key.reserve_exact(23);
    source.audio_program.note_lanes[1].target.input_port_key.reserve_exact(31);
    let original: Vec<_> = source
        .audio_program
        .note_lanes
        .iter()
        .map(|lane| {
            (
                lane.id,
                &lane.notes[0] as *const AudioNote as usize,
                lane.target.input_port_key.as_ptr() as usize,
                lane.target.input_port_key.capacity(),
            )
        })
        .collect();
    let (program, observation) = observed(|| compile(&source));
    assert_eq!((observation.requests, observation.deliveries), (1, 1));
    assert_eq!(observation.requested_lanes, 2);
    assert_eq!(observation.requested_bytes, independent_expected());
    assert_eq!(
        (
            observation.rows_length,
            observation.rows_capacity,
            observation.owner_count
        ),
        (2, 2, 2)
    );
    assert_eq!(
        observation.rows_pointer,
        program.note_lanes.as_ptr() as usize
    );
    for (index, owner) in observation.owners.into_iter().flatten().enumerate() {
        let lane = &program.note_lanes[index];
        let source_owner = original.iter().find(|value| value.0 == lane.id).unwrap();
        assert_eq!(owner.id, lane.id);
        assert_eq!(
            (
                owner.notes_pointer,
                owner.notes_length,
                owner.notes_capacity
            ),
            (
                lane.notes.as_ptr() as usize,
                lane.notes.len(),
                lane.notes.len()
            )
        );
        assert_eq!(
            (owner.key_pointer, owner.key_length, owner.key_capacity),
            (
                lane.target.input_port_key.as_ptr() as usize,
                lane.target.input_port_key.len(),
                lane.target.input_port_key.len()
            )
        );
        assert_ne!(owner.notes_pointer, source_owner.1);
        assert_ne!(owner.key_pointer, source_owner.2);
        assert!(source_owner.3 > owner.key_capacity);
        let authored = source
            .audio_program
            .note_lanes
            .iter()
            .find(|value| value.id == lane.id)
            .unwrap();
        assert_eq!(authored.target.input_port_key.capacity(), source_owner.3);
        assert_eq!(
            authored.target.input_port_key.as_ptr() as usize,
            source_owner.2
        );
    }
    assert_eq!(
        program.compiled_note_payload_bytes().unwrap(),
        independent_expected()
    );
    assert_eq!(program.note_lanes[0].id, lane_id(10));
    assert_eq!(program.note_lanes[1].id, lane_id(20));
}

#[test]
fn unique_note_sorts_keep_literal_ids_endpoints_and_velocity_bits_under_permutation() {
    let mut source = sequence();
    let first = compile(&source);
    source.audio_program.note_lanes.reverse();
    for lane in &mut source.audio_program.note_lanes {
        lane.notes.reverse();
    }
    let second = compile(&source);
    assert_eq!(first, second);
    let actual: Vec<_> = second
        .note_lanes
        .iter()
        .flat_map(|lane| {
            lane.notes.iter().map(move |note| {
                (
                    lane.id,
                    note.id,
                    note.start,
                    note.end,
                    note.channel,
                    note.key,
                    note.on_velocity.to_bits(),
                    note.off_velocity.to_bits(),
                )
            })
        })
        .collect();
    let expected = [(10, 101, 1, 2), (20, 201, 0, 1), (20, 301, 2, 3)]
        .into_iter()
        .map(|(lane, note, start, end)| {
            (
                lane_id(lane),
                note_id(note),
                time(start, 48_000),
                time(end, 48_000),
                3_u8,
                60_u8,
                0x8000_0000_0000_0000_u64,
                0x3fd3_3333_3333_3335_u64,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn author_identity_and_existing_output_errors_precede_any_note_materialization() {
    let mut duplicate_lane = sequence();
    let duplicate = duplicate_lane.audio_program.note_lanes[0].clone();
    duplicate_lane.audio_program.note_lanes.push(duplicate);
    let absent_output = ProgramOutputId(Uuid::from_u128(999));
    let (error, observation) = observed(|| {
        compile_audio_program(&duplicate_lane, AudioCompileRequest::program(absent_output))
            .unwrap_err()
    });
    assert_eq!(
        error,
        AudioCompileError::InvalidAuthoring(AudioAuthoringError::DuplicateNoteLane(lane_id(20)))
    );
    assert_eq!((observation.requests, observation.deliveries), (0, 0));

    let mut duplicate_note = sequence();
    duplicate_note.audio_program.note_lanes[0].notes[1].id = note_id(301);
    let (error, observation) =
        observed(|| compile_audio_program(&duplicate_note, request(&duplicate_note)).unwrap_err());
    assert_eq!(
        error,
        AudioCompileError::InvalidAuthoring(AudioAuthoringError::DuplicateNote(note_id(301)))
    );
    assert_eq!((observation.requests, observation.deliveries), (0, 0));

    let source = sequence();
    let (error, observation) = observed(|| {
        compile_audio_program(&source, AudioCompileRequest::program(absent_output)).unwrap_err()
    });
    assert_eq!(error, AudioCompileError::OutputNotFound(absent_output));
    assert_eq!((observation.requests, observation.deliveries), (0, 0));

    let mut unreachable_invalid = sequence();
    unreachable_invalid.audio_program.routes.clear();
    unreachable_invalid.audio_program.note_lanes[0].target.input_port_key = "\n".to_owned();
    let (error, observation) = observed(|| {
        compile_audio_program(&unreachable_invalid, request(&unreachable_invalid)).unwrap_err()
    });
    assert_eq!(
        error,
        AudioCompileError::InvalidAuthoring(AudioAuthoringError::InvalidNotePortKey(lane_id(20)))
    );
    assert_eq!((observation.requests, observation.deliveries), (0, 0));
}

#[test]
fn real_empty_muted_bypassed_unreachable_and_solo_selection_has_zero_note_owners() {
    for case in 0..5 {
        let mut source = sequence();
        let track_id = source.audio_tracks[0].id;
        match case {
            0 => { for lane in &mut source.audio_program.note_lanes { lane.notes.clear(); } },
            1 => source.audio_tracks[0].is_muted = true,
            2 => source.audio_program.track_channels.get_mut(&track_id).unwrap().strip.pre_fader.processors[0].bypassed = true,
            3 => source.audio_program.routes.retain(|route|
                !matches!(route.source, AudioRouteSource::Track { track_id: id, .. } if id == track_id)),
            _ => {},
        }
        let mut selected_request = request(&source);
        if case == 4 {
            selected_request.audition.soloed_tracks.insert(source.audio_tracks[1].id);
        }
        let (program, observation) =
            observed(|| compile_audio_program(&source, selected_request).unwrap());
        assert_eq!((observation.requests, observation.deliveries), (1, 1));
        assert_eq!(
            (
                observation.requested_lanes,
                observation.requested_bytes,
                observation.rows_capacity,
                observation.owner_count
            ),
            (0, 0, 0, 0)
        );
        assert!(program.note_lanes.is_empty());
        assert_eq!(program.compiled_note_payload_bytes().unwrap(), 0);
        assert_eq!(source.audio_program.note_lanes.len(), 2);
    }
    let mut source = sequence();
    let track_id = source.audio_tracks[0].id;
    let post = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    source.audio_program.note_lanes[0].target.processor_id = post.id;
    source
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .unwrap()
        .strip
        .post_fader
        .processors
        .push(post);
    source.audio_tracks[0].is_muted = true;
    for route in &mut source.audio_program.routes {
        if let AudioRouteSource::Track { track_id: id, port } = &mut route.source
            && *id == track_id
        {
            *port = AudioChannelStripOutputPort::PreFader;
        }
    }
    let program = compile(&source);
    assert_eq!(
        program.note_lanes.iter().map(|lane| lane.id).collect::<Vec<_>>(),
        vec![lane_id(10)]
    );
    assert!(program.track_channels[&track_id].strip.post_fader.processors.is_empty());
    assert_eq!(source.audio_program.note_lanes.len(), 2);
}

#[test]
fn genuine_overreserved_compiled_owners_are_refused_by_live_acceptance_seams() {
    for component in 0..3 {
        let mut program = compile(&sequence());
        match component {
            0 => program.note_lanes.reserve_exact(7),
            1 => program.note_lanes[0].notes.reserve_exact(7),
            _ => program.note_lanes[0].target.input_port_key.reserve_exact(23),
        }
        let (actual, requested, pointer) = match component {
            0 => (
                program.note_lanes.capacity(),
                program.note_lanes.len(),
                program.note_lanes.as_ptr() as usize,
            ),
            1 => (
                program.note_lanes[0].notes.capacity(),
                program.note_lanes[0].notes.len(),
                program.note_lanes[0].notes.as_ptr() as usize,
            ),
            _ => (
                program.note_lanes[0].target.input_port_key.capacity(),
                program.note_lanes[0].target.input_port_key.len(),
                program.note_lanes[0].target.input_port_key.as_ptr() as usize,
            ),
        };
        assert!(actual > requested);
        is_capacity_refusal(require_compiled_note_capacity(actual, requested).unwrap_err());
        let bytes = program.compiled_note_payload_bytes().unwrap();
        assert!(bytes > independent_expected());
        is_payload_refusal(
            require_compiled_note_payload(bytes, independent_expected()).unwrap_err(),
        );
        let retained_pointer = match component {
            0 => program.note_lanes.as_ptr() as usize,
            1 => program.note_lanes[0].notes.as_ptr() as usize,
            _ => program.note_lanes[0].target.input_port_key.as_ptr() as usize,
        };
        assert_eq!(retained_pointer, pointer);
        assert!(require_compiled_note_capacity(actual, actual).is_ok());
    }
}

#[test]
fn checked_scalar_layout_and_sum_overflow_never_requests_enormous_note_owners() {
    assert!(matches!(
        compiled_note_array_bytes::<CompiledNoteLane>(usize::MAX),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
    assert!(matches!(
        compiled_note_array_bytes::<AudioNote>(usize::MAX),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
    assert!(matches!(
        compiled_note_payload_bytes(1, usize::MAX, 0),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
    assert!(matches!(
        compiled_note_payload_bytes(1, 0, usize::MAX),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
    assert_eq!(compiled_note_payload_bytes(0, 0, 0).unwrap(), 0);
    assert!(matches!(
        reserve_compiled_note_rows::<AudioNote>(usize::MAX),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
}

#[derive(Default)]
struct NeverResolver {
    calls: AtomicUsize,
}
impl AudioProcessorResolver for NeverResolver {
    fn prepare(
        &self,
        _: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(AudioProcessorHostError::Unavailable(
            "public Note guard must refuse first".to_owned(),
        ))
    }
}
fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 16,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}

#[test]
fn public_contract_endpoint_and_canonical_compiled_guard_priorities_remain_unchanged() {
    let source = sequence();
    let expected_processor = source.audio_program.note_lanes[1].target.processor_id;
    let resolver = NeverResolver::default();
    let mut invalid = render();
    invalid.sample_rate = 0;
    assert!(matches!(
        PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(compile(&source)),
            invalid,
            AudioKernelBackend::ScalarReference,
            &resolver
        ),
        Err(AudioCompileError::InvalidRenderContract)
    ));
    let mut collapsed = source.clone();
    collapsed.audio_program.note_lanes[0].notes[0].start = time(1, 192_000);
    collapsed.audio_program.note_lanes[0].notes[0].end = time(1, 96_000);
    assert_eq!(
        PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(compile(&collapsed)),
            render(),
            AudioKernelBackend::ScalarReference,
            &resolver
        )
        .unwrap_err(),
        AudioCompileError::CollapsedNoteDuration {
            lane_id: lane_id(20),
            note_id: note_id(301),
            start_sample: 1,
            end_sample: 1,
        }
    );
    let mut overflow = source.clone();
    overflow.audio_program.note_lanes[0].notes[0].start = time(i64::MAX - 1, 1);
    overflow.audio_program.note_lanes[0].notes[0].end = time(i64::MAX, 1);
    assert!(
        matches!(PreparedAudioPlan::prepare_with_processor_resolver(Arc::new(compile(&overflow)),
        render(), AudioKernelBackend::ScalarReference, &resolver), Err(AudioCompileError::NoteSampleProjection {
            lane_id: id, note_id: note, ..
        }) if id == lane_id(20) && note == note_id(301))
    );
    assert_eq!(
        PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(compile(&source)),
            render(),
            AudioKernelBackend::ScalarReference,
            &resolver
        )
        .unwrap_err(),
        AudioCompileError::NoteExecutionNotSupported {
            lane_id: lane_id(10),
            processor_id: expected_processor,
        }
    );
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
}
