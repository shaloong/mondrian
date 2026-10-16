//! Owned private lowering controls. These do not grant public Note execution,
//! prove natural reserve/clone excess, or measure a whole-build/physical peak.
use super::*;
use crate::plan::{CompiledChannelStrip, CompiledProcessor, CompiledRack, CompiledTrackChannel};
use crate::{
    AudioProcessingMode, AudioProcessor, AudioProcessorAuxiliaryInputContract,
    AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorResolver, AudioProcessorTail, AudioRenderContract,
};
use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId, TimelineTime, TrackId};
use mondrian_timeline::audio::AudioNote;
use std::alloc::Layout;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Project,
    Resolve,
    Bind,
    Retained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyOwner {
    lane_id: AudioNoteLaneId,
    pointer: usize,
    length: usize,
    capacity: usize,
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    phase: Phase,
    source_pointer: usize,
    source_remaining: usize,
    source_capacity: usize,
    destination_pointer: usize,
    destination_length: usize,
    destination_capacity: usize,
    event_pointer: usize,
    event_length: usize,
    event_capacity: usize,
    keys: [Option<KeyOwner>; 2],
    key_count: usize,
    key_capacity: usize,
}

thread_local! {
    static TRACE: RefCell<Option<Vec<Observation>>> = const { RefCell::new(None) };
}

fn key(lane_id: AudioNoteLaneId, value: &String) -> KeyOwner {
    KeyOwner {
        lane_id,
        pointer: value.as_ptr() as usize,
        length: value.len(),
        capacity: value.capacity(),
    }
}

fn capture(mut observation: Observation, keys: impl Iterator<Item = KeyOwner>) {
    TRACE.with(|trace| {
        let mut trace = trace.borrow_mut();
        let Some(records) = trace.as_mut() else {
            return;
        };
        for owner in keys {
            if observation.key_count < observation.keys.len() {
                observation.keys[observation.key_count] = Some(owner);
            }
            observation.key_count += 1;
            observation.key_capacity =
                observation.key_capacity.checked_add(owner.capacity).unwrap();
        }
        records.push(observation);
    });
}

fn observation<T>(
    phase: Phase,
    source_pointer: usize,
    source_remaining: usize,
    source_capacity: usize,
    destination: &Vec<T>,
    events: &Vec<PreparedNoteEvent>,
) -> Observation {
    Observation {
        phase,
        source_pointer,
        source_remaining,
        source_capacity,
        destination_pointer: destination.as_ptr() as usize,
        destination_length: destination.len(),
        destination_capacity: destination.capacity(),
        event_pointer: events.as_ptr() as usize,
        event_length: events.len(),
        event_capacity: events.capacity(),
        keys: [None; 2],
        key_count: 0,
        key_capacity: 0,
    }
}

pub(super) fn observe_project(lanes: &Vec<ProjectedLane>, events: &Vec<PreparedNoteEvent>) {
    capture(
        observation(Phase::Project, 0, 0, 0, lanes, events),
        lanes.iter().map(|lane| key(lane.lane_id, &lane.target.input_port_key)),
    );
}

pub(super) fn observe_resolve(
    source: &std::vec::IntoIter<ProjectedLane>,
    backing_capacity: usize,
    destination: &Vec<RequestedTarget>,
    events: &Vec<PreparedNoteEvent>,
) {
    capture(
        observation(
            Phase::Resolve,
            source.as_slice().as_ptr() as usize,
            source.len(),
            backing_capacity,
            destination,
            events,
        ),
        destination
            .iter()
            .map(|target| key(target.lane_id, &target.input_port_key))
            .chain(
                source
                    .as_slice()
                    .iter()
                    .map(|lane| key(lane.lane_id, &lane.target.input_port_key)),
            ),
    );
}

pub(super) fn observe_bind(
    source: &std::vec::IntoIter<RequestedTarget>,
    backing_capacity: usize,
    destination: &Vec<PreparedNoteTarget>,
    events: &Vec<PreparedNoteEvent>,
) {
    capture(
        observation(
            Phase::Bind,
            source.as_slice().as_ptr() as usize,
            source.len(),
            backing_capacity,
            destination,
            events,
        ),
        destination
            .iter()
            .map(|target| key(target.lane_id, &target.input_port_key))
            .chain(
                source
                    .as_slice()
                    .iter()
                    .map(|target| key(target.lane_id, &target.input_port_key)),
            ),
    );
}

pub(super) fn observe_retained(score: &PreparedNoteScore) {
    capture(
        observation(Phase::Retained, 0, 0, 0, &score.targets, &score.events),
        score.targets.iter().map(|target| key(target.lane_id, &target.input_port_key)),
    );
}

fn traced<R>(run: impl FnOnce() -> R) -> (R, Vec<Observation>) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            TRACE.with(|trace| {
                trace.borrow_mut().take();
            });
        }
    }
    TRACE.with(|trace| {
        let mut trace = trace.borrow_mut();
        assert!(trace.is_none(), "no nested listener");
        *trace = Some(Vec::with_capacity(32));
    });
    let clear = Clear;
    let result = run();
    let records = TRACE.with(|trace| trace.borrow_mut().take().unwrap());
    drop(clear);
    (result, records)
}

fn rate() -> AudioSampleRate {
    AudioSampleRate::new(48_000).unwrap()
}
fn time(n: i64, d: i64) -> TimelineTime {
    TimelineTime::new(n, d).unwrap()
}
fn target(port_key: &str) -> AudioNoteTarget {
    AudioNoteTarget {
        track_id: TrackId::new(),
        processor_id: AudioProcessorInstanceId::new(),
        input_port_key: port_key.to_owned(),
    }
}
fn note(start: i64, end: i64) -> AudioNote {
    AudioNote {
        id: AudioNoteId::new(),
        start: time(start, 48_000),
        end: time(end, 48_000),
        channel: 3,
        key: 60,
        on_velocity: -0.0,
        off_velocity: f64::from_bits(0x3fd3_3333_3333_3335),
    }
}
fn lane(target: AudioNoteTarget, notes: Vec<AudioNote>) -> CompiledNoteLane {
    CompiledNoteLane { id: AudioNoteLaneId::new(), target, notes }
}
fn strip() -> CompiledChannelStrip {
    CompiledChannelStrip {
        input_trim_db: 0.0,
        pre_fader: CompiledRack::default(),
        fader_db: 0.0,
        fader_automation: None,
        post_fader: CompiledRack::default(),
    }
}
fn clap() -> AudioProcessorDefinitionRef {
    AudioProcessorDefinitionRef::Clap {
        plugin_id: "owned-metadata-only".to_owned(),
        schema_version: 1,
        binary_sha256: Some([1; 32]),
    }
}
fn program(lanes: Vec<CompiledNoteLane>) -> CompiledAudioProgram {
    let mut channels = BTreeMap::new();
    for lane in &lanes {
        let channel = channels
            .entry(lane.target.track_id)
            .or_insert_with(|| CompiledTrackChannel { muted: false, strip: strip() });
        channel.strip.pre_fader.processors.push(CompiledProcessor {
            instance_id: lane.target.processor_id,
            definition: clap(),
            parameters: BTreeMap::new(),
            opaque_state: None,
        });
    }
    // Owned semantic fixture, not author validation, routing, Session or DSP.
    CompiledAudioProgram {
        output_id: ProgramOutputId::new(),
        contributions: Vec::new(),
        processing_scopes: BTreeMap::new(),
        transitions: Vec::new(),
        track_channels: channels,
        buses: BTreeMap::new(),
        bus_order: Vec::new(),
        output: strip(),
        routes: Vec::new(),
        sidechain_routes: Vec::new(),
        note_lanes: lanes,
    }
}
fn unequal() -> CompiledAudioProgram {
    program(vec![
        lane(target("a"), vec![note(0, 1), note(2, 3)]),
        lane(target("é"), vec![note(1, 2)]),
    ])
}
fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: mondrian_core::AudioChannelLayout::Mono,
        max_block_frames: 16,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}
struct MetadataFactory {
    notes: crate::AudioProcessorNoteInputContract,
    queries: Arc<AtomicUsize>,
    creates: Arc<AtomicUsize>,
}
impl AudioProcessorFactory for MetadataFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, false, true, true, 0)
            .unwrap()
    }
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        AudioProcessorAuxiliaryInputContract::default()
    }
    fn note_input_contract(&self) -> crate::AudioProcessorNoteInputContract {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.notes.clone()
    }
    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.creates.fetch_add(1, Ordering::Relaxed);
        Err(AudioProcessorHostError::Unavailable(
            "owned metadata only".to_owned(),
        ))
    }
}
struct MetadataResolver {
    factory: Arc<MetadataFactory>,
}
impl AudioProcessorResolver for MetadataResolver {
    fn prepare(
        &self,
        _: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        Ok(self.factory.clone())
    }
}
fn owned_processors(
    requested: &RequestedNoteScore,
    queries: &Arc<AtomicUsize>,
    creates: &Arc<AtomicUsize>,
) -> Vec<PreparedProcessor> {
    requested
        .targets
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let notes = crate::AudioProcessorNoteInputContract {
                ports: vec![crate::AudioProcessorNoteInputPort {
                    port_key: target.input_port_key.clone(),
                    native_id: 120 + index as u32,
                    native_index: 0,
                }],
                limits: AudioNoteExecutionLimits {
                    input_events: 8,
                    output_ends: 8,
                    retained_records: 8,
                    live_voices: 8,
                },
            };
            let resolver = MetadataResolver {
                factory: Arc::new(MetadataFactory {
                    notes,
                    queries: queries.clone(),
                    creates: creates.clone(),
                }),
            };
            let definition = clap();
            let parameters = BTreeMap::new();
            let factory = crate::processor_host::PreparedProcessorFactoryBinding::resolve(
                &resolver,
                AudioProcessorPrepareRequest::new(
                    target.occurrence,
                    &definition,
                    &parameters,
                    None,
                    render(),
                )
                .with_note_input_requested(true),
            )
            .unwrap();
            PreparedProcessor {
                occurrence: target.occurrence,
                parameter_ids: Vec::new(),
                parameter_curves: Vec::new(),
                owner_time_offset: TimelineTime::ZERO,
                main_compensation_delay_frames: 0,
                input_signal_delay_frames: 0,
                auxiliary_buses: 0..0,
                output_buses: 0..0,
                main_output_buffer: None,
                factory,
            }
        })
        .collect()
}
fn requested(program: &CompiledAudioProgram) -> RequestedNoteScore {
    ProjectedNoteScore::project(&program.note_lanes, rate())
        .unwrap()
        .resolve_targets(program)
        .unwrap()
}
fn excess<T>(values: Vec<T>) -> Vec<T> {
    let mut owner = Vec::with_capacity(values.len() + 7);
    owner.extend(values);
    assert!(owner.capacity() > owner.len());
    owner
}
fn is_phase_refusal(error: AudioCompileError) {
    assert!(
        matches!(error, AudioCompileError::InvalidPreparedGraph(detail) if detail == "prepared Note phase payload exceeds request")
    );
}
fn is_detail(error: AudioCompileError, expected: &str) {
    assert!(matches!(error, AudioCompileError::InvalidPreparedGraph(detail) if detail == expected));
}

#[test]
fn target_layout_model_uses_unequal_shape_and_existing_p1_a_once() {
    let mut program = unequal();
    let mut empty = lane(target("empty"), Vec::with_capacity(5));
    empty.target.input_port_key.reserve_exact(20);
    program.note_lanes.push(empty);
    program.note_lanes.reserve_exact(4);
    let a = program.compiled_note_payload_bytes().unwrap(); // the existing sole A reader
    let independent_a =
        Layout::array::<CompiledNoteLane>(program.note_lanes.capacity()).unwrap().size()
            + program
                .note_lanes
                .iter()
                .map(|lane| {
                    Layout::array::<AudioNote>(lane.notes.capacity()).unwrap().size()
                        + lane.target.input_port_key.capacity()
                })
                .sum::<usize>();
    assert_eq!(
        a, independent_a,
        "P1 covers mixed empty retained owners, not only lowering L"
    );
    assert_eq!(
        program.note_lanes.iter().filter(|lane| !lane.notes.is_empty()).count(),
        2
    );
    let p = Layout::array::<ProjectedLane>(2).unwrap().size();
    let q = Layout::array::<RequestedTarget>(2).unwrap().size();
    let b = Layout::array::<PreparedNoteTarget>(2).unwrap().size();
    let e = Layout::array::<PreparedNoteEvent>(6).unwrap().size();
    for size in [
        std::mem::size_of::<ProjectedLane>(),
        std::mem::size_of::<RequestedTarget>(),
        std::mem::size_of::<PreparedNoteTarget>(),
        std::mem::size_of::<PreparedNoteEvent>(),
    ] {
        assert!(size > 0);
    }
    let expected = [p + e + 3, p + q + e + 3, q + b + e + 3, b + e + 3];
    let actual = [
        note_phase_payload_bytes(NotePhaseRows::Project { lanes: 2 }, 6, 3).unwrap(),
        note_phase_payload_bytes(NotePhaseRows::Resolve { projected: 2, requested: 2 }, 6, 3)
            .unwrap(),
        note_phase_payload_bytes(NotePhaseRows::Bind { requested: 2, prepared: 2 }, 6, 3).unwrap(),
        note_phase_payload_bytes(NotePhaseRows::Retained { prepared: 2 }, 6, 3).unwrap(),
    ];
    assert_eq!(actual, expected);
    assert_eq!(
        a.checked_add(actual[..3].iter().copied().max().unwrap()).unwrap(),
        independent_a + (p + e + 3).max(p + q + e + 3).max(q + b + e + 3)
    );
    // The combined expression is a control observation, not a new stored grant.
    let projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
    assert_eq!(projected.lanes.len(), 2);
    assert_eq!(projected.events.len(), 6);
}

#[test]
fn real_intoiter_backing_coexists_and_event_key_owners_move_once() {
    let program = unequal();
    let queries = Arc::new(AtomicUsize::new(0));
    let creates = Arc::new(AtomicUsize::new(0));
    let ((bound, initial_keys, source_project, source_requested), trace) = traced(|| {
        let projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
        let source_project = (
            projected.lanes.as_ptr() as usize,
            projected.lanes.capacity(),
        );
        let initial_keys: Vec<_> = projected
            .lanes
            .iter()
            .map(|lane| key(lane.lane_id, &lane.target.input_port_key))
            .collect();
        let requested = projected.resolve_targets(&program).unwrap();
        let source_requested = (
            requested.targets.as_ptr() as usize,
            requested.targets.capacity(),
        );
        let processors = owned_processors(&requested, &queries, &creates);
        assert_eq!(queries.load(Ordering::Relaxed), 2);
        (
            requested.bind(&processors, rate()).unwrap(),
            initial_keys,
            source_project,
            source_requested,
        )
    });
    assert_eq!(trace.len(), 8);
    assert_eq!(trace[0].phase, Phase::Project);
    assert_eq!(trace[7].phase, Phase::Retained);
    for (phase, source, source_width, destination_width) in [
        (
            Phase::Resolve,
            source_project,
            std::mem::size_of::<ProjectedLane>(),
            std::mem::size_of::<RequestedTarget>(),
        ),
        (
            Phase::Bind,
            source_requested,
            std::mem::size_of::<RequestedTarget>(),
            std::mem::size_of::<PreparedNoteTarget>(),
        ),
    ] {
        let records: Vec<_> = trace.iter().filter(|record| record.phase == phase).collect();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].source_pointer, source.0);
        assert_ne!(
            records[0].destination_pointer, source.0,
            "two real live nonempty row allocations"
        );
        assert_eq!(
            records[0].destination_pointer,
            records[2].destination_pointer
        );
        for (consumed, record) in records.iter().enumerate() {
            assert_eq!(record.source_remaining, 2 - consumed);
            assert_eq!(record.destination_length, consumed);
            assert_eq!(record.source_capacity, source.1);
            assert_eq!(record.destination_capacity, 2);
            let simultaneous = source.1 * source_width
                + record.destination_capacity * destination_width
                + record.event_capacity * std::mem::size_of::<PreparedNoteEvent>()
                + record.key_capacity;
            let remaining_only = record.source_remaining * source_width
                + record.destination_capacity * destination_width
                + record.event_capacity * std::mem::size_of::<PreparedNoteEvent>()
                + record.key_capacity;
            if consumed > 0 {
                assert!(
                    simultaneous > remaining_only,
                    "empty/partial iterator keeps backing owned"
                );
            }
        }
    }
    let mut expected_keys = initial_keys;
    expected_keys.sort_by_key(|owner| owner.lane_id);
    for record in &trace {
        assert_eq!(record.event_pointer, bound.events.as_ptr() as usize);
        assert_eq!(record.event_length, 6);
        assert_eq!(record.event_capacity, 6);
        assert_eq!(record.key_count, 2);
        assert_eq!(record.key_capacity, 3);
        let mut actual_keys: Vec<_> = record.keys.into_iter().flatten().collect();
        actual_keys.sort_by_key(|owner| owner.lane_id);
        assert_eq!(
            actual_keys, expected_keys,
            "real destination plus remaining source owns each key once"
        );
    }
    let mut expected_events = Vec::new();
    let mut sorted_ids: Vec<_> = program.note_lanes.iter().map(|lane| lane.id).collect();
    sorted_ids.sort_unstable();
    for (lane_index, lane) in program.note_lanes.iter().enumerate() {
        let literal_pairs: &[(i64, i64)] = if lane_index == 0 {
            &[(0, 1), (2, 3)]
        } else {
            &[(1, 2)]
        };
        for (note, &(start, end)) in lane.notes.iter().zip(literal_pairs) {
            let slot = sorted_ids.binary_search(&lane.id).unwrap();
            expected_events.push((
                start,
                PreparedNotePhase::On,
                lane.id,
                note.id,
                slot,
                3,
                60,
                0x8000_0000_0000_0000_u64,
            ));
            expected_events.push((
                end,
                PreparedNotePhase::Off,
                lane.id,
                note.id,
                slot,
                3,
                60,
                0x3fd3_3333_3333_3335_u64,
            ));
        }
    }
    expected_events.sort_unstable_by_key(|event| (event.0, event.1, event.2, event.3));
    let actual_events: Vec<_> = bound
        .events
        .iter()
        .map(|event| {
            (
                event.sample,
                event.phase,
                event.lane_id,
                event.note_id,
                event.target_slot,
                event.channel,
                event.key,
                event.velocity.to_bits(),
            )
        })
        .collect();
    assert_eq!(actual_events, expected_events);
    for (slot, target) in bound.targets.iter().enumerate() {
        let authored = program.note_lanes.iter().find(|lane| lane.id == target.lane_id).unwrap();
        assert_eq!(target.lane_id, sorted_ids[slot]);
        assert_eq!(target.processor_slot, slot);
        assert_eq!(target.occurrence.instance_id, authored.target.processor_id);
        assert_eq!(
            target.occurrence.owner,
            AudioProcessorOccurrenceOwner::Track(authored.target.track_id)
        );
        assert_eq!(
            target.occurrence.insertion,
            AudioProcessorInsertionPoint::PreFader
        );
        assert_eq!(target.input_port_key, authored.target.input_port_key);
        assert_eq!(target.native_port_id, 120 + slot as u32);
        assert_eq!(
            target.native_port_index, 0,
            "actual one-port cached contract index"
        );
        assert_eq!(
            target.limits,
            AudioNoteExecutionLimits {
                input_events: 8,
                output_ends: 8,
                retained_records: 8,
                live_voices: 8
            }
        );
    }
    let final_expected = 2 * std::mem::size_of::<PreparedNoteTarget>()
        + 6 * std::mem::size_of::<PreparedNoteEvent>()
        + 3;
    assert_eq!(bound.storage_bytes().unwrap(), final_expected);
    assert_eq!(
        queries.load(Ordering::Relaxed),
        2,
        "bind reads only the frozen cached contract"
    );
    assert_eq!(creates.load(Ordering::Relaxed), 0);
}

#[test]
fn overreserved_projected_backing_is_refused_before_requested_delivery() {
    let program = unequal();
    let mut projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
    projected.lanes = excess(projected.lanes);
    let backing = projected.lanes.capacity();
    let pointer = projected.lanes.as_ptr() as usize;
    let (result, trace) = traced(|| projected.resolve_targets(&program));
    is_phase_refusal(result.unwrap_err());
    assert_eq!(trace.len(), 3);
    assert_eq!(trace[0].source_pointer, pointer);
    assert!(trace.iter().all(|record| record.source_capacity == backing));
    assert_eq!(trace[2].source_remaining, 0);
    assert_eq!(trace[2].destination_length, 2);
    assert!(backing > 2, "real owned allocation, not an allocator mock");
}

#[test]
fn overreserved_requested_event_and_key_owners_are_refused_before_final_delivery() {
    let program = unequal();
    for component in 0..3 {
        let mut requested = requested(&program);
        match component {
            0 => requested.targets = excess(requested.targets),
            1 => requested.events = excess(requested.events),
            2 => {
                requested.targets[0].input_port_key.reserve_exact(20);
            }
            _ => unreachable!(),
        }
        let queries = Arc::new(AtomicUsize::new(0));
        let creates = Arc::new(AtomicUsize::new(0));
        let processors = owned_processors(&requested, &queries, &creates);
        let source_capacity = requested.targets.capacity();
        let event_capacity = requested.events.capacity();
        let key_capacity: usize =
            requested.targets.iter().map(|target| target.input_port_key.capacity()).sum();
        assert!(match component {
            0 => source_capacity > 2,
            1 => event_capacity > 6,
            _ => key_capacity > 3,
        });
        let (result, trace) = traced(|| requested.bind(&processors, rate()));
        is_phase_refusal(result.unwrap_err());
        assert_eq!(trace.len(), 3, "no successful retained observation");
        assert_eq!(trace[2].source_remaining, 0);
        assert_eq!(trace[2].source_capacity, source_capacity);
        assert_eq!(trace[2].event_capacity, event_capacity);
        assert_eq!(trace[2].key_capacity, key_capacity);
        assert_eq!(queries.load(Ordering::Relaxed), 2);
        assert_eq!(creates.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn phase_scalar_overflow_uses_real_layouts_without_enormous_allocation() {
    for rows in [
        NotePhaseRows::Project { lanes: usize::MAX },
        NotePhaseRows::Resolve { projected: usize::MAX, requested: 1 },
        NotePhaseRows::Resolve { projected: 1, requested: usize::MAX },
        NotePhaseRows::Bind { requested: usize::MAX, prepared: 1 },
        NotePhaseRows::Bind { requested: 1, prepared: usize::MAX },
        NotePhaseRows::Retained { prepared: usize::MAX },
    ] {
        assert!(matches!(
            note_phase_payload_bytes(rows, 0, 0),
            Err(AudioCompileError::NoteEventCountOverflow)
        ));
    }
    assert!(matches!(
        note_phase_payload_bytes(NotePhaseRows::Project { lanes: 0 }, usize::MAX, 0),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
    assert!(matches!(
        note_phase_payload_bytes(NotePhaseRows::Project { lanes: 1 }, 0, usize::MAX),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
    // Layout-valid row terms can still overflow their sum; no actual container is requested.
    let projected = (isize::MAX as usize) / std::mem::size_of::<ProjectedLane>();
    let requested = (isize::MAX as usize) / std::mem::size_of::<RequestedTarget>();
    assert!(Layout::array::<ProjectedLane>(projected).is_ok());
    assert!(Layout::array::<RequestedTarget>(requested).is_ok());
    assert!(matches!(
        note_phase_payload_bytes(
            NotePhaseRows::Resolve { projected, requested },
            0,
            usize::MAX
        ),
        Err(AudioCompileError::NoteEventCountOverflow)
    ));
}

#[test]
fn canonical_empty_stages_remain_zero_while_caller_spares_are_outside_a_policy() {
    let mut empty = lane(target("spare"), Vec::with_capacity(8));
    empty.target.input_port_key.reserve_exact(20);
    let mut lanes = Vec::with_capacity(8);
    lanes.push(empty);
    let program = program(lanes);
    assert_eq!(program.compiled_note_payload_bytes().unwrap(), 0);
    assert!(program.note_lanes.capacity() > 0);
    assert!(program.note_lanes[0].notes.capacity() > 0);
    let (bound, trace) = traced(|| {
        let projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
        projected.require_runtime_support().unwrap();
        let requested = projected.resolve_targets(&program).unwrap();
        requested.bind(&[], rate()).unwrap()
    });
    assert_eq!(trace.len(), 4);
    assert!(trace.iter().all(|record| record.source_capacity == 0
        && record.destination_capacity == 0
        && record.event_capacity == 0
        && record.key_count == 0
        && record.key_capacity == 0));
    assert!(bound.is_empty());
    assert_eq!(bound.storage_bytes().unwrap(), 0);
    for rows in [
        NotePhaseRows::Project { lanes: 0 },
        NotePhaseRows::Resolve { projected: 0, requested: 0 },
        NotePhaseRows::Bind { requested: 0, prepared: 0 },
        NotePhaseRows::Retained { prepared: 0 },
    ] {
        assert_eq!(note_phase_payload_bytes(rows, 0, 0).unwrap(), 0);
    }
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
            "public boundary must refuse first".to_owned(),
        ))
    }
}

#[test]
fn public_contract_endpoint_and_nonempty_guard_priority_is_unchanged() {
    let mut program = unequal();
    let resolver = NeverResolver::default();
    let mut contract = render();
    contract.sample_rate = 0;
    assert!(matches!(
        super::super::PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(program.clone()),
            contract,
            super::super::AudioKernelBackend::ScalarReference,
            &resolver
        ),
        Err(AudioCompileError::InvalidRenderContract)
    ));
    program.note_lanes[0].notes[0].start = time(1, 192_000);
    program.note_lanes[0].notes[0].end = time(1, 96_000);
    assert!(matches!(
        super::super::PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(program.clone()),
            render(),
            super::super::AudioKernelBackend::ScalarReference,
            &resolver
        ),
        Err(AudioCompileError::CollapsedNoteDuration { .. })
    ));
    program.note_lanes[0].notes[0].start = time(i64::MAX - 1, 1);
    program.note_lanes[0].notes[0].end = time(i64::MAX, 1);
    assert!(matches!(
        super::super::PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(program.clone()),
            render(),
            super::super::AudioKernelBackend::ScalarReference,
            &resolver
        ),
        Err(AudioCompileError::NoteSampleProjection { .. })
    ));
    program.note_lanes[0].notes[0].start = TimelineTime::ZERO;
    program.note_lanes[0].notes[0].end = time(1, 48_000);
    let expected = AudioCompileError::NoteExecutionNotSupported {
        lane_id: program.note_lanes[0].id,
        processor_id: program.note_lanes[0].target.processor_id,
    };
    let mut tight = render();
    tight.processor_session_scratch_budget_bytes = 1;
    assert_eq!(
        super::super::PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(program),
            tight,
            super::super::AudioKernelBackend::ScalarReference,
            &resolver
        )
        .unwrap_err(),
        expected
    );
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn original_private_errors_win_over_malformed_owner_phase_refusal() {
    let program = unequal();
    let mut missing_track = program.clone();
    missing_track.track_channels.clear();
    let mut projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
    projected.lanes = excess(projected.lanes);
    is_detail(
        projected.resolve_targets(&missing_track).unwrap_err(),
        "prepared Note target Track is absent",
    );
    for ambiguous in [false, true] {
        let mut malformed = program.clone();
        for channel in malformed.track_channels.values_mut() {
            if ambiguous {
                let duplicate = channel.strip.pre_fader.processors[0].clone();
                channel.strip.post_fader.processors.push(duplicate);
            } else {
                channel.strip.pre_fader.processors.clear();
            }
        }
        let mut projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
        projected.lanes = excess(projected.lanes);
        let expected = if ambiguous {
            "prepared Note target occurrence is ambiguous"
        } else {
            "prepared Note target processor is absent from its Track rack"
        };
        is_detail(projected.resolve_targets(&malformed).unwrap_err(), expected);
    }
    let mut wrong_format = program.clone();
    for channel in wrong_format.track_channels.values_mut() {
        for processor in &mut channel.strip.pre_fader.processors {
            processor.definition = AudioProcessorDefinitionRef::BuiltIn {
                definition_id: "gain".to_owned(),
                schema_version: 1,
            };
        }
    }
    let mut projected = ProjectedNoteScore::project(&program.note_lanes, rate()).unwrap();
    let first = &projected.lanes[0];
    let expected = AudioCompileError::NoteExecutionNotSupported {
        lane_id: first.lane_id,
        processor_id: first.target.processor_id,
    };
    projected.lanes = excess(projected.lanes);
    assert_eq!(
        projected.resolve_targets(&wrong_format).unwrap_err(),
        expected
    );
    for failure in 0..6 {
        let mut request = requested(&program);
        request.targets = excess(request.targets);
        let queries = Arc::new(AtomicUsize::new(0));
        let creates = Arc::new(AtomicUsize::new(0));
        let mut processors = owned_processors(&request, &queries, &creates);
        let error = match failure {
            0 => request.bind(&processors, AudioSampleRate::new(44_100).unwrap()).unwrap_err(),
            1 => request.bind(&[], rate()).unwrap_err(),
            2 => {
                let duplicated = owned_processors(&request, &queries, &creates).remove(0);
                processors.push(duplicated);
                request.bind(&processors, rate()).unwrap_err()
            }
            3 => {
                request.targets[0].input_port_key = "not-present".to_owned();
                request.bind(&processors, rate()).unwrap_err()
            }
            4 => {
                request.events[0].target_slot = usize::MAX;
                request.bind(&processors, rate()).unwrap_err()
            }
            _ => {
                request.events[0].lane_id = AudioNoteLaneId::new();
                request.bind(&processors, rate()).unwrap_err()
            }
        };
        match failure {
            0 => assert_eq!(error, AudioCompileError::InvalidRenderContract),
            1 => is_detail(error, "prepared Note factory occurrence is absent"),
            2 => is_detail(error, "prepared Note factory occurrence is ambiguous"),
            3 => is_detail(
                error,
                "prepared Note stable input-port key is absent from the exact factory contract",
            ),
            4 => is_detail(error, "prepared Note event target rank is absent"),
            _ => is_detail(error, "prepared Note event target identity differs"),
        }
        assert_eq!(creates.load(Ordering::Relaxed), 0);
    }
}
