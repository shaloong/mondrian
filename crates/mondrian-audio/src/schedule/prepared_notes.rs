//! Immutable score-grid lowering and cached factory-port binding.
//!
//! Projection consumes exact author endpoints once. Later phases contain only
//! integer samples and cannot quantize again. This is metadata, not permission
//! to process notes, assign native IDs, enter state or create a Session ledger.

use super::{AudioResourceFootprintError, PreparedProcessor};
use crate::plan::{CompiledAudioProgram, CompiledNoteLane};
use crate::{
    AudioCompileError, AudioNoteExecutionLimits, AudioProcessorFactoryStorageBorrow,
    AudioProcessorFactoryStorageError, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
    AudioProcessorOccurrenceOwner,
};
use mondrian_core::{
    AudioNoteId, AudioNoteLaneId, AudioSamplePosition, AudioSampleRate, AudioSampleRounding,
};
use mondrian_timeline::audio::{AudioNoteTarget, AudioProcessorDefinitionRef};
use std::mem::size_of;

/// Pure format admission on the actual selected target, before resolver IO.
/// A VST Note always needs an authored musical reference; CLAP keeps None.
pub(super) fn admits_note_format(
    definition: &AudioProcessorDefinitionRef,
    reference: Option<mondrian_core::MusicalTimeReference>,
) -> bool {
    matches!(definition, AudioProcessorDefinitionRef::Clap { .. })
        || reference.is_some() && matches!(definition, AudioProcessorDefinitionRef::Vst3 { .. })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum PreparedNotePhase {
    Off = 0,
    On = 1,
}

/// Complete author identity plus once-projected absolute Sequence sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PreparedNoteEvent {
    pub(crate) sample: i64,
    pub(crate) phase: PreparedNotePhase,
    pub(crate) lane_id: AudioNoteLaneId,
    pub(crate) note_id: AudioNoteId,
    /// Stable lane-ID rank into this score's target array, never a native ID.
    pub(crate) target_slot: usize,
    pub(crate) channel: u8,
    pub(crate) key: u8,
    pub(crate) velocity: f64,
    /// Mutual endpoint index in this one immutable, sorted score.
    pub(crate) mate_event_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectedLane {
    lane_id: AudioNoteLaneId,
    target: AudioNoteTarget,
}

/// No factory query, native port or mutable execution state exists in this IR.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProjectedNoteScore {
    sample_rate: u32,
    lanes: Vec<ProjectedLane>,
    events: Vec<PreparedNoteEvent>,
    on_events_by_author: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestedTarget {
    lane_id: AudioNoteLaneId,
    occurrence: AudioProcessorOccurrence,
    input_port_key: String,
}

/// Purely resolved Track/rack requests; factory creation remains a separate step.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestedNoteScore {
    sample_rate: u32,
    targets: Vec<RequestedTarget>,
    events: Vec<PreparedNoteEvent>,
    on_events_by_author: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedNoteTarget {
    pub(crate) lane_id: AudioNoteLaneId,
    pub(crate) processor_slot: usize,
    pub(crate) occurrence: AudioProcessorOccurrence,
    pub(crate) input_port_key: String,
    /// Stable actual input-port ID supplied by the frozen factory contract.
    pub(crate) native_port_id: u32,
    /// Current restored input ABI index, not a UUID hash or author list position.
    pub(crate) native_port_index: u16,
    pub(crate) channel_count: u8,
    pub(crate) completion: crate::AudioNoteCompletionPolicy,
    pub(crate) limits: AudioNoteExecutionLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedNoteScore {
    sample_rate: u32,
    targets: Vec<PreparedNoteTarget>,
    events: Vec<PreparedNoteEvent>,
    on_events_by_author: Vec<usize>,
}

fn invalid(detail: &str) -> AudioCompileError {
    AudioCompileError::InvalidPreparedGraph(detail.to_owned())
}

// One accepted-capacity predicate for actual fresh arrays and cloned port keys.
// It is logical payload policy, not a grant or a physical allocation ceiling.
fn require_requested_capacity(actual: usize, requested: usize) -> Result<(), AudioCompileError> {
    if actual > requested {
        return Err(invalid("prepared Note storage capacity exceeds request"));
    }
    Ok(())
}

// The real consumers are ProjectedLane, PreparedNoteEvent, RequestedTarget and
// PreparedNoteTarget; all contain nonzero-sized identity/scalar fields.
fn reserve<T>(count: usize) -> Result<Vec<T>, AudioCompileError> {
    count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(AudioCompileError::NoteEventCountOverflow)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| invalid("prepared Note storage allocation refused"))?;
    require_requested_capacity(values.capacity(), count)?;
    Ok(values)
}

// One private lowering model, consumed by all four real IR phases. This counts
// logical requested payload, not a Root grant, retained compiled A or an OS peak.
// Consuming transitions include both row arrays while IntoIter owns its backing.
enum NotePhaseRows {
    Project { lanes: usize },
    Resolve { projected: usize, requested: usize },
    Bind { requested: usize, prepared: usize },
    Retained { prepared: usize },
}

fn note_phase_payload_bytes(
    rows: NotePhaseRows,
    events: usize,
    key_bytes: usize,
) -> Result<usize, AudioCompileError> {
    let project = matches!(&rows, NotePhaseRows::Project { .. });
    let index_entries = events
        .div_ceil(2)
        .checked_mul(if project { 3 } else { 1 })
        .ok_or(AudioCompileError::NoteEventCountOverflow)?;
    note_phase_payload_with_index(rows, events, key_bytes, index_entries)
}

fn note_phase_payload_with_index(
    rows: NotePhaseRows,
    events: usize,
    key_bytes: usize,
    index_entries: usize,
) -> Result<usize, AudioCompileError> {
    use std::alloc::Layout;

    let overflow = || AudioCompileError::NoteEventCountOverflow;
    let row_bytes = match rows {
        NotePhaseRows::Project { lanes } => {
            Layout::array::<ProjectedLane>(lanes).map_err(|_| overflow())?.size()
        }
        NotePhaseRows::Resolve { projected, requested } => {
            let source = Layout::array::<ProjectedLane>(projected).map_err(|_| overflow())?.size();
            let destination =
                Layout::array::<RequestedTarget>(requested).map_err(|_| overflow())?.size();
            source.checked_add(destination).ok_or_else(overflow)?
        }
        NotePhaseRows::Bind { requested, prepared } => {
            let source =
                Layout::array::<RequestedTarget>(requested).map_err(|_| overflow())?.size();
            let destination =
                Layout::array::<PreparedNoteTarget>(prepared).map_err(|_| overflow())?.size();
            source.checked_add(destination).ok_or_else(overflow)?
        }
        NotePhaseRows::Retained { prepared } => {
            Layout::array::<PreparedNoteTarget>(prepared).map_err(|_| overflow())?.size()
        }
    };
    let event_bytes = Layout::array::<PreparedNoteEvent>(events).map_err(|_| overflow())?.size();
    let index_bytes = Layout::array::<usize>(index_entries).map_err(|_| overflow())?.size();
    row_bytes
        .checked_add(event_bytes)
        .and_then(|bytes| bytes.checked_add(index_bytes))
        .and_then(|bytes| bytes.checked_add(key_bytes))
        .ok_or_else(overflow)
}

/// Reuse each real phase's accepted requested payload for pre-clone admission.
pub(crate) fn requested_note_lowering_peak_bytes(
    lanes: usize,
    events: usize,
    key_bytes: usize,
) -> Result<usize, AudioCompileError> {
    [
        NotePhaseRows::Project { lanes },
        NotePhaseRows::Resolve { projected: lanes, requested: lanes },
        NotePhaseRows::Bind { requested: lanes, prepared: lanes },
        NotePhaseRows::Retained { prepared: lanes },
    ]
    .into_iter()
    .try_fold(0_usize, |peak, rows| {
        note_phase_payload_bytes(rows, events, key_bytes).map(|bytes| peak.max(bytes))
    })
}

fn require_phase_payload(actual: usize, requested: usize) -> Result<(), AudioCompileError> {
    if actual > requested {
        return Err(invalid("prepared Note phase payload exceeds request"));
    }
    Ok(())
}

impl ProjectedNoteScore {
    pub(crate) fn project(
        lanes: &[CompiledNoteLane],
        rate: AudioSampleRate,
    ) -> Result<Self, AudioCompileError> {
        let mut event_count = 0_usize;
        let mut lane_count = 0_usize;
        for lane in lanes {
            event_count = event_count
                .checked_add(
                    lane.notes
                        .len()
                        .checked_mul(2)
                        .ok_or(AudioCompileError::NoteEventCountOverflow)?,
                )
                .ok_or(AudioCompileError::NoteEventCountOverflow)?;
            if !lane.notes.is_empty() {
                lane_count =
                    lane_count.checked_add(1).ok_or(AudioCompileError::NoteEventCountOverflow)?;
            }
        }
        // Compute the model before allocating. Defer a new model error until
        // the original projection/identity checks finish, preserving priority.
        let requested_payload = lanes
            .iter()
            .filter(|lane| !lane.notes.is_empty())
            .try_fold(0_usize, |bytes, lane| {
                bytes
                    .checked_add(lane.target.input_port_key.len())
                    .ok_or(AudioCompileError::NoteEventCountOverflow)
            })
            .and_then(|keys| {
                note_phase_payload_bytes(
                    NotePhaseRows::Project { lanes: lane_count },
                    event_count,
                    keys,
                )
            });
        let mut projected_lanes = reserve::<ProjectedLane>(lane_count)?;
        for lane in lanes.iter().filter(|lane| !lane.notes.is_empty()) {
            let target = lane.target.clone();
            require_requested_capacity(
                target.input_port_key.capacity(),
                lane.target.input_port_key.len(),
            )?;
            projected_lanes.push(ProjectedLane { lane_id: lane.id, target });
        }
        projected_lanes.sort_unstable_by_key(|lane| lane.lane_id);
        if projected_lanes.windows(2).any(|pair| pair[0].lane_id == pair[1].lane_id) {
            return Err(invalid("prepared Note lane identities are not unique"));
        }
        let mut events = reserve::<PreparedNoteEvent>(event_count)?;
        // All selected endpoints finish (or report their exact error) before a
        // target-format decision or any caller can begin factory resolution.
        for lane in lanes {
            if lane.notes.is_empty() {
                continue;
            }
            let target_slot = projected_lanes
                .binary_search_by_key(&lane.id, |value| value.lane_id)
                .map_err(|_| invalid("prepared Note lane rank is absent"))?;
            for note in &lane.notes {
                let pair = events.len() / 2;
                let project = |time| {
                    AudioSamplePosition::from_timeline_time(time, rate, AudioSampleRounding::Ceil)
                        .map(|position| position.sample())
                        .map_err(|source| AudioCompileError::NoteSampleProjection {
                            lane_id: lane.id,
                            note_id: note.id,
                            source,
                        })
                };
                let start = project(note.start)?;
                let end = project(note.end)?;
                if end <= start {
                    return Err(AudioCompileError::CollapsedNoteDuration {
                        lane_id: lane.id,
                        note_id: note.id,
                        start_sample: start,
                        end_sample: end,
                    });
                }
                events.push(PreparedNoteEvent {
                    sample: start,
                    phase: PreparedNotePhase::On,
                    lane_id: lane.id,
                    note_id: note.id,
                    target_slot,
                    channel: note.channel,
                    key: note.key,
                    velocity: note.on_velocity,
                    mate_event_index: pair,
                });
                events.push(PreparedNoteEvent {
                    sample: end,
                    phase: PreparedNotePhase::Off,
                    lane_id: lane.id,
                    note_id: note.id,
                    target_slot,
                    channel: note.channel,
                    key: note.key,
                    velocity: note.off_velocity,
                    mate_event_index: pair,
                });
            }
        }
        events.sort_unstable_by_key(|event| {
            (event.sample, event.phase, event.lane_id, event.note_id)
        });
        let mut pairs = reserve::<[usize; 2]>(event_count / 2)?;
        pairs.resize(event_count / 2, [usize::MAX; 2]);
        let mut on_events_by_author = reserve::<usize>(event_count / 2)?;
        for (index, event) in events.iter().enumerate() {
            pairs[event.mate_event_index][usize::from(event.phase == PreparedNotePhase::Off)] =
                index;
            if event.phase == PreparedNotePhase::On {
                on_events_by_author.push(index);
            }
        }
        for event in &mut events {
            event.mate_event_index =
                pairs[event.mate_event_index][usize::from(event.phase == PreparedNotePhase::On)];
        }
        on_events_by_author
            .sort_unstable_by_key(|&index| (events[index].lane_id, events[index].note_id));
        // These capacities overlap physically until pair projection finishes.
        require_requested_capacity(pairs.capacity(), event_count / 2)?;
        require_requested_capacity(on_events_by_author.capacity(), event_count / 2)?;
        #[cfg(test)]
        phase_envelope_tests::observe_pair_projection(&pairs, &on_events_by_author, &events);
        let project_index_entries = pairs
            .capacity()
            .checked_mul(2)
            .and_then(|count| count.checked_add(on_events_by_author.capacity()))
            .ok_or(AudioCompileError::NoteEventCountOverflow)?;
        drop(pairs);
        let requested_payload = requested_payload?;
        let key_capacity = projected_lanes.iter().try_fold(0_usize, |bytes, lane| {
            bytes
                .checked_add(lane.target.input_port_key.capacity())
                .ok_or(AudioCompileError::NoteEventCountOverflow)
        })?;
        let actual_payload = note_phase_payload_with_index(
            NotePhaseRows::Project { lanes: projected_lanes.capacity() },
            events.capacity(),
            key_capacity,
            project_index_entries,
        )?;
        require_phase_payload(actual_payload, requested_payload)?;
        #[cfg(test)]
        phase_envelope_tests::observe_project(&projected_lanes, &events, &on_events_by_author);
        Ok(Self {
            sample_rate: rate.hz(),
            lanes: projected_lanes,
            events,
            on_events_by_author,
        })
    }

    /// Deliberate current runtime boundary. Removing this guard alone is not an
    /// execution grant: Root must first connect whole-root admission and ledger.
    pub(crate) fn require_runtime_support(&self) -> Result<(), AudioCompileError> {
        if let Some(lane) = self.lanes.first() {
            return Err(AudioCompileError::NoteExecutionNotSupported {
                lane_id: lane.lane_id,
                processor_id: lane.target.processor_id,
            });
        }
        Ok(())
    }

    /// Resolve every selected target's format/occurrence before factory IO. Bus,
    /// Output, Scope and nested IDs cannot substitute for the exact Track rack.
    pub(crate) fn resolve_targets(
        self,
        program: &CompiledAudioProgram,
    ) -> Result<RequestedNoteScore, AudioCompileError> {
        require_requested_capacity(self.on_events_by_author.capacity(), self.events.len() / 2)?;
        let source_capacity = self.lanes.capacity();
        let lane_count = self.lanes.len();
        let requested_payload = self
            .lanes
            .iter()
            .try_fold(0_usize, |bytes, lane| {
                bytes
                    .checked_add(lane.target.input_port_key.len())
                    .ok_or(AudioCompileError::NoteEventCountOverflow)
            })
            .and_then(|keys| {
                note_phase_payload_bytes(
                    NotePhaseRows::Resolve { projected: lane_count, requested: lane_count },
                    self.events.len(),
                    keys,
                )
            });
        let key_capacity = self.lanes.iter().try_fold(0_usize, |bytes, lane| {
            bytes
                .checked_add(lane.target.input_port_key.capacity())
                .ok_or(AudioCompileError::NoteEventCountOverflow)
        });
        let mut targets = reserve::<RequestedTarget>(self.lanes.len())?;
        let mut source_lanes = self.lanes.into_iter();
        #[cfg(test)]
        phase_envelope_tests::observe_resolve(
            &source_lanes,
            source_capacity,
            &targets,
            &self.events,
            &self.on_events_by_author,
        );
        for _ in 0..lane_count {
            let Some(lane) = source_lanes.next() else {
                break;
            };
            let channel = program
                .track_channels
                .get(&lane.target.track_id)
                .ok_or_else(|| invalid("prepared Note target Track is absent"))?;
            let mut matched = None;
            for (insertion, rack) in [
                (
                    AudioProcessorInsertionPoint::PreFader,
                    &channel.strip.pre_fader,
                ),
                (
                    AudioProcessorInsertionPoint::PostFader,
                    &channel.strip.post_fader,
                ),
            ] {
                for processor in &rack.processors {
                    if processor.instance_id != lane.target.processor_id {
                        continue;
                    }
                    if matched.is_some() {
                        return Err(invalid("prepared Note target occurrence is ambiguous"));
                    }
                    matched = Some((insertion, processor));
                }
            }
            let (insertion, processor) = matched.ok_or_else(|| {
                invalid("prepared Note target processor is absent from its Track rack")
            })?;
            if !admits_note_format(&processor.definition, program.musical_time_reference) {
                return Err(AudioCompileError::NoteExecutionNotSupported {
                    lane_id: lane.lane_id,
                    processor_id: lane.target.processor_id,
                });
            }
            targets.push(RequestedTarget {
                lane_id: lane.lane_id,
                occurrence: AudioProcessorOccurrence {
                    instance_id: processor.instance_id,
                    owner: AudioProcessorOccurrenceOwner::Track(lane.target.track_id),
                    insertion,
                },
                input_port_key: lane.target.input_port_key,
            });
            #[cfg(test)]
            phase_envelope_tests::observe_resolve(
                &source_lanes,
                source_capacity,
                &targets,
                &self.events,
                &self.on_events_by_author,
            );
        }
        // The emptied iterator still owns its original allocation. Count its
        // captured backing capacity, not the remaining slice length, until Drop.
        let requested_payload = requested_payload?;
        let actual_payload = note_phase_payload_with_index(
            NotePhaseRows::Resolve {
                projected: source_capacity,
                requested: targets.capacity(),
            },
            self.events.capacity(),
            key_capacity?,
            self.on_events_by_author.capacity(),
        )?;
        require_phase_payload(actual_payload, requested_payload)?;
        drop(source_lanes);
        Ok(RequestedNoteScore {
            sample_rate: self.sample_rate,
            targets,
            events: self.events,
            on_events_by_author: self.on_events_by_author,
        })
    }
}

impl RequestedNoteScore {
    /// Future prepare_rack opt-in must consult this exact occurrence. Current
    /// ordinary prepare keeps the runtime guard and passes false to all racks.
    pub(crate) fn requests_input_for(&self, occurrence: AudioProcessorOccurrence) -> bool {
        self.targets.iter().any(|target| target.occurrence == occurrence)
    }

    /// Only cached factory metadata is read; no resolver/create/native query.
    /// Moves the already quantized events unchanged into the immutable Plan.
    pub(crate) fn bind(
        self,
        processors: &[PreparedProcessor],
        rate: AudioSampleRate,
    ) -> Result<PreparedNoteScore, AudioCompileError> {
        if self.sample_rate != rate.hz() {
            return Err(AudioCompileError::InvalidRenderContract);
        }
        require_requested_capacity(self.on_events_by_author.capacity(), self.events.len() / 2)?;
        let source_capacity = self.targets.capacity();
        let lane_count = self.targets.len();
        let event_count = self.events.len();
        let key_length = self.targets.iter().try_fold(0_usize, |bytes, target| {
            bytes
                .checked_add(target.input_port_key.len())
                .ok_or(AudioCompileError::NoteEventCountOverflow)
        });
        let requested_payload = key_length
            .as_ref()
            .map_err(|_| AudioCompileError::NoteEventCountOverflow)
            .and_then(|keys| {
                note_phase_payload_bytes(
                    NotePhaseRows::Bind { requested: lane_count, prepared: lane_count },
                    event_count,
                    *keys,
                )
            });
        let key_capacity = self.targets.iter().try_fold(0_usize, |bytes, target| {
            bytes
                .checked_add(target.input_port_key.capacity())
                .ok_or(AudioCompileError::NoteEventCountOverflow)
        });
        let mut targets = reserve::<PreparedNoteTarget>(self.targets.len())?;
        let mut source_targets = self.targets.into_iter();
        #[cfg(test)]
        phase_envelope_tests::observe_bind(
            &source_targets,
            source_capacity,
            &targets,
            &self.events,
            &self.on_events_by_author,
        );
        for _ in 0..lane_count {
            let Some(request) = source_targets.next() else {
                break;
            };
            let mut matches = processors
                .iter()
                .enumerate()
                .filter(|(_, processor)| processor.occurrence == request.occurrence);
            let (processor_slot, processor) = matches
                .next()
                .ok_or_else(|| invalid("prepared Note factory occurrence is absent"))?;
            if matches.next().is_some() {
                return Err(invalid("prepared Note factory occurrence is ambiguous"));
            }
            let contract = processor.factory.note_inputs();
            let port = contract
                .ports
                .iter()
                .find(|port| port.port_key == request.input_port_key)
                .ok_or_else(|| {
                invalid(
                    "prepared Note stable input-port key is absent from the exact factory contract",
                )
            })?;
            targets.push(PreparedNoteTarget {
                lane_id: request.lane_id,
                processor_slot,
                occurrence: request.occurrence,
                input_port_key: request.input_port_key,
                native_port_id: port.native_id,
                native_port_index: port.native_index,
                channel_count: port.channel_count,
                completion: contract.completion,
                limits: contract.limits,
            });
            #[cfg(test)]
            phase_envelope_tests::observe_bind(
                &source_targets,
                source_capacity,
                &targets,
                &self.events,
                &self.on_events_by_author,
            );
        }
        for event in &self.events {
            let target = targets
                .get(event.target_slot)
                .ok_or_else(|| invalid("prepared Note event target rank is absent"))?;
            if target.lane_id != event.lane_id {
                return Err(invalid("prepared Note event target identity differs"));
            }
            if event.channel >= target.channel_count {
                return Err(invalid(
                    "prepared Note channel exceeds admitted input channels",
                ));
            }
        }
        // Existing grid/occurrence/port/rank errors win over a new phase refusal.
        let requested_payload = requested_payload?;
        let actual_payload = note_phase_payload_with_index(
            NotePhaseRows::Bind {
                requested: source_capacity,
                prepared: targets.capacity(),
            },
            self.events.capacity(),
            key_capacity?,
            self.on_events_by_author.capacity(),
        )?;
        require_phase_payload(actual_payload, requested_payload)?;
        drop(source_targets);
        let bound = PreparedNoteScore {
            sample_rate: self.sample_rate,
            targets,
            events: self.events,
            on_events_by_author: self.on_events_by_author,
        };
        bound.validate_pairs().map_err(invalid)?;
        let retained_upper = note_phase_payload_bytes(
            NotePhaseRows::Retained { prepared: lane_count },
            event_count,
            key_length?,
        )?;
        let retained_actual =
            bound.storage_bytes().map_err(|_| AudioCompileError::NoteEventCountOverflow)?;
        require_phase_payload(retained_actual, retained_upper)?;
        #[cfg(test)]
        phase_envelope_tests::observe_retained(&bound);
        Ok(bound)
    }
}

impl PreparedNoteScore {
    /// Compare the cached immutable binding, without resolver/native queries or
    /// allocating another contract. Called before Session storage and dispatch.
    pub(crate) fn matches_factory_contracts(
        &self,
        processors: &[super::PreparedProcessor],
    ) -> bool {
        self.targets.iter().all(|target| {
            let Some(processor) = processors.get(target.processor_slot) else {
                return false;
            };
            let contract = processor.factory.note_inputs();
            let Some(port) = contract.ports.get(usize::from(target.native_port_index)) else {
                return false;
            };
            processor.occurrence == target.occurrence
                && port.native_index == target.native_port_index
                && port.native_id == target.native_port_id
                && port.port_key == target.input_port_key
                && port.channel_count == target.channel_count
                && contract.completion == target.completion
                && contract.limits == target.limits
        })
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        targets: Vec<PreparedNoteTarget>,
        mut events: Vec<PreparedNoteEvent>,
    ) -> Result<Self, AudioCompileError> {
        events.sort_unstable_by_key(|event| {
            (event.sample, event.phase, event.lane_id, event.note_id)
        });
        for index in 0..events.len() {
            let event = events[index];
            events[index].mate_event_index = events
                .iter()
                .enumerate()
                .find_map(|(mate, other)| {
                    (other.phase != event.phase
                        && other.lane_id == event.lane_id
                        && other.note_id == event.note_id
                        && other.target_slot == event.target_slot)
                        .then_some(mate)
                })
                .unwrap_or(usize::MAX);
        }
        let mut on_events_by_author = events
            .iter()
            .enumerate()
            .filter_map(|(i, event)| (event.phase == PreparedNotePhase::On).then_some(i))
            .collect::<Vec<_>>();
        on_events_by_author.sort_unstable_by_key(|&i| (events[i].lane_id, events[i].note_id));
        let score = Self {
            sample_rate: 48_000,
            targets,
            events,
            on_events_by_author,
        };
        score.validate_pairs().map_err(invalid)?;
        Ok(score)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub(crate) const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    #[cfg(test)]
    pub(crate) fn events(&self) -> &[PreparedNoteEvent] {
        &self.events
    }
    pub(crate) fn targets(&self) -> &[PreparedNoteTarget] {
        &self.targets
    }

    /// Validate the one paired IR once at its construction/bind boundary.
    /// Private immutable fields then carry that invariant into every window.
    /// Orphan/ambiguous test IR remains refused; it cannot become a chase grant.
    pub(crate) fn validate_pairs(&self) -> Result<(), &'static str> {
        if !self.events.len().is_multiple_of(2)
            || self.on_events_by_author.len() != self.events.len() / 2
        {
            return Err("Off has no retained original On");
        }
        let mut prior = None;
        for &index in &self.on_events_by_author {
            let on = self.events.get(index).ok_or("Off has no retained original On")?;
            let identity = (on.lane_id, on.note_id);
            if on.phase != PreparedNotePhase::On {
                return Err("Off has no retained original On");
            }
            if prior.is_some_and(|old| old >= identity) {
                return Err("duplicate authored Note On");
            }
            prior = Some(identity);
        }
        for (index, event) in self.events.iter().enumerate() {
            let target = self
                .targets
                .get(event.target_slot)
                .ok_or("prepared Note event target rank is absent")?;
            if event.lane_id != target.lane_id {
                return Err("prepared Note event target identity differs");
            }
            let mate = self
                .events
                .get(event.mate_event_index)
                .ok_or("Off has no retained original On")?;
            if mate.mate_event_index != index
                || mate.phase == event.phase
                || mate.note_id != event.note_id
                || mate.lane_id != event.lane_id
            {
                return Err("Off has no retained original On");
            }
            if mate.target_slot != event.target_slot
                || mate.channel != event.channel
                || mate.key != event.key
            {
                return Err("Off changed its original input port");
            }
            if event.phase == PreparedNotePhase::On && event.sample >= mate.sample {
                return Err("Off changed its original input port");
            }
            if index > 0 && event_order(self.events[index - 1]) > event_order(*event) {
                return Err("prepared Note event order differs");
            }
        }
        Ok(())
    }

    pub(crate) fn window(
        &self,
        start: i64,
        end: i64,
        cutoff: i64,
    ) -> Result<PreparedNoteWindow<'_>, &'static str> {
        if end < start {
            return Err("Note window overflow");
        }
        let begin = self.events.partition_point(|event| event.sample < start.max(cutoff));
        let finish = self.events.partition_point(|event| event.sample < end);
        Ok(PreparedNoteWindow {
            score: self,
            cutoff,
            normal: begin.min(finish),
            finish,
            chase: 0,
            include_chase: start <= cutoff && cutoff < end,
        })
    }

    /// The pure cold batch is a point at S, not a guessed future window.
    /// Inclusive lookup also preserves the signed entry domain at i64::MAX.
    pub(crate) fn entry_events(&self, cutoff: i64) -> PreparedNoteWindow<'_> {
        PreparedNoteWindow {
            score: self,
            cutoff,
            normal: self.events.partition_point(|event| event.sample < cutoff),
            finish: self.events.partition_point(|event| event.sample <= cutoff),
            chase: 0,
            include_chase: true,
        }
    }

    pub(crate) fn effective_on_sample(&self, event: PreparedNoteEvent, cutoff: i64) -> i64 {
        let on = if event.phase == PreparedNotePhase::On {
            event
        } else {
            self.events[event.mate_event_index]
        };
        on.sample.max(cutoff)
    }

    /// Actual retained allocation-requested payload, not len estimates or
    /// allocator overhead. The enclosing Plan header has its existing coverage.
    pub(crate) fn storage_bytes(&self) -> Result<usize, AudioResourceFootprintError> {
        let event_bytes = self
            .events
            .capacity()
            .checked_mul(std::mem::size_of::<PreparedNoteEvent>())
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        let target_bytes = self
            .targets
            .capacity()
            .checked_mul(std::mem::size_of::<PreparedNoteTarget>())
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        let index_bytes = self
            .on_events_by_author
            .capacity()
            .checked_mul(size_of::<usize>())
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        self.targets.iter().try_fold(
            event_bytes
                .checked_add(index_bytes)
                .and_then(|bytes| bytes.checked_add(target_bytes))
                .ok_or(AudioResourceFootprintError::ExtentOverflow)?,
            |bytes, target| {
                bytes
                    .checked_add(target.input_port_key.capacity())
                    .ok_or(AudioResourceFootprintError::ExtentOverflow)
            },
        )
    }

    pub(crate) fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::complete(&self.events)?)?;
        visitor(AudioProcessorFactoryStorageBorrow::complete(&self.targets)?)?;
        visitor(AudioProcessorFactoryStorageBorrow::complete(
            &self.on_events_by_author,
        )?)?;
        for target in &self.targets {
            visitor(AudioProcessorFactoryStorageBorrow::complete(
                &target.input_port_key,
            )?)?;
        }
        Ok(())
    }
}

fn event_order(event: PreparedNoteEvent) -> (i64, PreparedNotePhase, AudioNoteLaneId, AudioNoteId) {
    (event.sample, event.phase, event.lane_id, event.note_id)
}

/// One allocation-free merge of paired original events and deterministic cold
/// retriggers. Both prospective preflight and admitted delivery consume it.
/// The sole ledger owns cutoff; this view retains no epoch or delivery state.
#[derive(Clone)]
pub(crate) struct PreparedNoteWindow<'a> {
    score: &'a PreparedNoteScore,
    cutoff: i64,
    normal: usize,
    finish: usize,
    chase: usize,
    include_chase: bool,
}
impl PreparedNoteWindow<'_> {
    fn normal(&mut self) -> Option<PreparedNoteEvent> {
        while self.normal < self.finish {
            let event = self.score.events[self.normal];
            if event.phase == PreparedNotePhase::Off
                && event.sample == self.cutoff
                && self.score.events[event.mate_event_index].sample < self.cutoff
            {
                self.normal += 1;
            } else {
                return Some(event);
            }
        }
        None
    }
    fn chased(&mut self) -> Option<PreparedNoteEvent> {
        if !self.include_chase {
            return None;
        }
        while let Some(&index) = self.score.on_events_by_author.get(self.chase) {
            let mut on = self.score.events[index];
            let off = self.score.events[on.mate_event_index];
            if on.sample < self.cutoff && self.cutoff < off.sample {
                on.sample = self.cutoff;
                return Some(on);
            }
            self.chase += 1;
        }
        None
    }
}
impl Iterator for PreparedNoteWindow<'_> {
    type Item = PreparedNoteEvent;
    fn next(&mut self) -> Option<Self::Item> {
        match (self.normal(), self.chased()) {
            (None, None) => None,
            (Some(event), None) => {
                self.normal += 1;
                Some(event)
            }
            (None, Some(event)) => {
                self.chase += 1;
                Some(event)
            }
            (Some(normal), Some(chased)) => {
                if event_order(normal) <= event_order(chased) {
                    self.normal += 1;
                    Some(normal)
                } else {
                    self.chase += 1;
                    Some(chased)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{
        CompiledChannelStrip, CompiledProcessor, CompiledRack, CompiledTrackChannel,
    };
    use crate::{
        AudioProcessingMode, AudioProcessor, AudioProcessorAuxiliaryInputContract,
        AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
        AudioProcessorPrepareRequest, AudioProcessorResolver, AudioProcessorTail,
        AudioRenderContract,
    };
    use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId, TimelineTime, TrackId};
    use mondrian_timeline::audio::AudioNote;
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn rate(value: u32) -> AudioSampleRate {
        AudioSampleRate::new(value).unwrap()
    }
    fn time(n: i64, d: i64) -> TimelineTime {
        TimelineTime::new(n, d).unwrap()
    }
    fn target() -> AudioNoteTarget {
        AudioNoteTarget {
            track_id: TrackId::new(),
            processor_id: AudioProcessorInstanceId::new(),
            input_port_key: " notes.é ".to_owned(),
        }
    }
    fn note(start: TimelineTime, end: TimelineTime) -> AudioNote {
        AudioNote {
            id: AudioNoteId::new(),
            start,
            end,
            channel: 3,
            key: 60,
            on_velocity: -0.0,
            off_velocity: f64::from_bits(0x3fd3_3333_3333_3335),
        }
    }
    fn lane(target: &AudioNoteTarget, notes: Vec<AudioNote>) -> CompiledNoteLane {
        CompiledNoteLane {
            id: AudioNoteLaneId::new(),
            target: target.clone(),
            notes,
        }
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
    fn program(
        lanes: Vec<CompiledNoteLane>,
        definition: AudioProcessorDefinitionRef,
        insertion: AudioProcessorInsertionPoint,
    ) -> CompiledAudioProgram {
        let mut channels = BTreeMap::new();
        for lane in &lanes {
            let channel = channels
                .entry(lane.target.track_id)
                .or_insert_with(|| CompiledTrackChannel { muted: false, strip: strip() });
            let rack = if insertion == AudioProcessorInsertionPoint::PreFader {
                &mut channel.strip.pre_fader
            } else {
                &mut channel.strip.post_fader
            };
            if !rack
                .processors
                .iter()
                .any(|processor| processor.instance_id == lane.target.processor_id)
            {
                rack.processors.push(CompiledProcessor {
                    instance_id: lane.target.processor_id,
                    definition: definition.clone(),
                    parameters: BTreeMap::new(),
                    opaque_state: None,
                });
            }
        }
        // Pure semantic fixture for projection/target checks. No routing/audio
        // execution or real compiled-author validation is claimed by it.
        CompiledAudioProgram {
            musical_time_reference: None,
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
    fn port_contract() -> crate::AudioProcessorNoteInputContract {
        crate::AudioProcessorNoteInputContract {
            completion: crate::AudioNoteCompletionPolicy::NativeEnd,
            ports: vec![
                crate::AudioProcessorNoteInputPort {
                    channel_count: 16,
                    port_key: "other".to_owned(),
                    native_id: 7,
                    native_index: 0,
                },
                crate::AudioProcessorNoteInputPort {
                    channel_count: 16,
                    port_key: " notes.é ".to_owned(),
                    native_id: 123,
                    native_index: 1,
                },
            ],
            limits: AudioNoteExecutionLimits {
                input_events: 8,
                output_ends: 8,
                retained_records: 8,
                live_voices: 8,
            },
        }
    }

    struct MetadataFactory {
        notes: crate::AudioProcessorNoteInputContract,
        queries: Arc<AtomicUsize>,
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
        fn create(
            &self,
            _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
        ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
            Err(AudioProcessorHostError::Unavailable(
                "metadata fixture cannot create/DSP".to_owned(),
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
    fn prepared(
        occurrence: AudioProcessorOccurrence,
        contract: crate::AudioProcessorNoteInputContract,
        queries: Arc<AtomicUsize>,
    ) -> PreparedProcessor {
        let resolver = MetadataResolver {
            factory: Arc::new(MetadataFactory { notes: contract, queries }),
        };
        let definition = clap();
        let parameters = BTreeMap::new();
        let factory = crate::processor_host::PreparedProcessorFactoryBinding::resolve(
            &resolver,
            AudioProcessorPrepareRequest::new(occurrence, &definition, &parameters, None, render())
                .with_note_input_requested(true),
        )
        .unwrap();
        PreparedProcessor {
            occurrence,
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
    }

    #[test]
    fn endpoints_use_one_selected_ceil_and_keep_original_uuid_and_velocity_bits() {
        let target = target();
        let note = note(time(1001, 30000), time(2002, 30000));
        let id = note.id;
        let lane = lane(&target, vec![note]);
        for (hz, start, end) in [(44_100, 1472, 2943), (48_000, 1602, 3204)] {
            let score = ProjectedNoteScore::project(std::slice::from_ref(&lane), rate(hz)).unwrap();
            assert_eq!(score.events.len(), 2);
            assert_eq!(
                (score.events[0].sample, score.events[1].sample),
                (start, end)
            );
            assert_eq!((score.events[0].note_id, score.events[1].note_id), (id, id));
            assert_eq!(score.events[0].velocity.to_bits(), (-0.0_f64).to_bits());
            assert_eq!(score.events[1].velocity.to_bits(), 0x3fd3_3333_3333_3335);
        }
    }

    #[test]
    fn stable_sample_phase_lane_and_note_id_order_ignores_author_list_order() {
        let target = target();
        let mut lanes = vec![
            lane(
                &target,
                vec![
                    note(time(0, 1), time(1, 48000)),
                    note(time(0, 1), time(1, 48000)),
                ],
            ),
            lane(&target, vec![note(time(1, 48000), time(2, 48000))]),
        ];
        let original = ProjectedNoteScore::project(&lanes, rate(48000)).unwrap();
        lanes.reverse();
        for lane in &mut lanes {
            lane.notes.reverse();
        }
        let reversed = ProjectedNoteScore::project(&lanes, rate(48000)).unwrap();
        assert_eq!(reversed, original);
        let keys: Vec<_> = original
            .events
            .iter()
            .map(|event| (event.sample, event.phase, event.lane_id, event.note_id))
            .collect();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        let at_one: Vec<_> = original
            .events
            .iter()
            .filter(|event| event.sample == 1)
            .map(|event| event.phase)
            .collect();
        assert_eq!(
            at_one,
            [
                PreparedNotePhase::Off,
                PreparedNotePhase::Off,
                PreparedNotePhase::On
            ]
        );
    }

    #[test]
    fn collapse_and_overflow_are_checked_before_runtime_or_target_refusal() {
        let target = target();
        let mut lanes = vec![
            lane(&target, vec![note(time(0, 1), time(1, 1))]),
            lane(&target, vec![note(time(1, 192000), time(1, 96000))]),
        ];
        let bad_lane = lanes[1].id;
        let bad_note = lanes[1].notes[0].id;
        assert_eq!(
            ProjectedNoteScore::project(&lanes, rate(48000)).unwrap_err(),
            AudioCompileError::CollapsedNoteDuration {
                lane_id: bad_lane,
                note_id: bad_note,
                start_sample: 1,
                end_sample: 1
            }
        );
        lanes[1].notes[0].start = time(i64::MAX - 1, 1);
        lanes[1].notes[0].end = time(i64::MAX, 1);
        assert!(matches!(
            ProjectedNoteScore::project(&lanes, rate(48000)),
            Err(AudioCompileError::NoteSampleProjection {
                source: mondrian_core::AudioTimeError::PositionOverflow,
                ..
            })
        ));
    }

    #[test]
    fn builtin_and_vst_targets_keep_the_original_typed_unsupported_result() {
        let target = target();
        let lane = lane(&target, vec![note(time(0, 1), time(1, 1))]);
        let expected = AudioCompileError::NoteExecutionNotSupported {
            lane_id: lane.id,
            processor_id: target.processor_id,
        };
        for definition in [
            AudioProcessorDefinitionRef::BuiltIn {
                definition_id: "gain".to_owned(),
                schema_version: 1,
            },
            AudioProcessorDefinitionRef::Vst3 {
                class_id: "owned".to_owned(),
                vendor: None,
                schema_version: 1,
                binary_sha256: Some([1; 32]),
            },
        ] {
            let program = program(
                vec![lane.clone()],
                definition,
                AudioProcessorInsertionPoint::PreFader,
            );
            let projected = ProjectedNoteScore::project(&program.note_lanes, rate(48000)).unwrap();
            assert_eq!(projected.resolve_targets(&program).unwrap_err(), expected);
        }
    }

    #[test]
    fn exact_track_pre_post_occurrence_and_cached_port_bind_without_extra_factory_query() {
        for insertion in [
            AudioProcessorInsertionPoint::PreFader,
            AudioProcessorInsertionPoint::PostFader,
        ] {
            let target = target();
            let lane = lane(&target, vec![note(time(0, 1), time(1, 1))]);
            let program = program(vec![lane.clone()], clap(), insertion);
            let projected = ProjectedNoteScore::project(&program.note_lanes, rate(48000)).unwrap();
            let before = projected.events.clone();
            let requested = projected.resolve_targets(&program).unwrap();
            let occurrence = AudioProcessorOccurrence {
                instance_id: target.processor_id,
                owner: AudioProcessorOccurrenceOwner::Track(target.track_id),
                insertion,
            };
            assert!(requested.requests_input_for(occurrence));
            assert!(!requested.requests_input_for(AudioProcessorOccurrence {
                owner: AudioProcessorOccurrenceOwner::Track(TrackId::new()),
                ..occurrence
            }));
            let queries = Arc::new(AtomicUsize::new(0));
            let processor = prepared(occurrence, port_contract(), queries.clone());
            assert_eq!(queries.load(Ordering::Relaxed), 1); // actual binding capture
            let bound = requested.bind(&[processor], rate(48000)).unwrap();
            assert_eq!(queries.load(Ordering::Relaxed), 1); // no getter/native query in bind
            assert_eq!(bound.events(), before.as_slice());
            assert_eq!(bound.sample_rate(), 48000);
            assert_eq!(bound.targets()[0].occurrence, occurrence);
            assert_eq!(bound.targets()[0].native_port_id, 123);
            assert_eq!(bound.targets()[0].native_port_index, 1);
            assert_eq!(bound.targets()[0].input_port_key, " notes.é ");
            assert_eq!(bound.targets()[0].limits, port_contract().limits);
        }
    }

    #[test]
    fn binding_refuses_wrong_grid_occurrence_and_nonexact_port_key() {
        let target = target();
        let program = program(
            vec![lane(&target, vec![note(time(0, 1), time(1, 1))])],
            clap(),
            AudioProcessorInsertionPoint::PreFader,
        );
        let requested = ProjectedNoteScore::project(&program.note_lanes, rate(48000))
            .unwrap()
            .resolve_targets(&program)
            .unwrap();
        let occurrence = requested.targets[0].occurrence;
        let queries = Arc::new(AtomicUsize::new(0));
        let processor = prepared(occurrence, port_contract(), queries);
        assert!(matches!(
            requested.clone().bind(std::slice::from_ref(&processor), rate(44100)),
            Err(AudioCompileError::InvalidRenderContract)
        ));
        let mut wrong_key = requested.clone();
        wrong_key.targets[0].input_port_key = "notes.é".to_owned();
        assert!(wrong_key.bind(std::slice::from_ref(&processor), rate(48000)).is_err());
        let wrong = prepared(
            AudioProcessorOccurrence {
                owner: AudioProcessorOccurrenceOwner::Bus(mondrian_core::MixBusId::new()),
                ..occurrence
            },
            port_contract(),
            Arc::new(AtomicUsize::new(0)),
        );
        assert!(requested.clone().bind(&[wrong], rate(48000)).is_err());
        assert!(requested.bind(&[], rate(48000)).is_err());
    }

    #[test]
    fn retained_capacity_and_actual_note_storage_visits_include_vec_and_key_owners() {
        let target = target();
        let program = program(
            vec![lane(&target, vec![note(time(0, 1), time(1, 1))])],
            clap(),
            AudioProcessorInsertionPoint::PreFader,
        );
        let requested = ProjectedNoteScore::project(&program.note_lanes, rate(48000))
            .unwrap()
            .resolve_targets(&program)
            .unwrap();
        let processor = prepared(
            requested.targets[0].occurrence,
            port_contract(),
            Arc::new(AtomicUsize::new(0)),
        );
        let mut bound = requested.bind(&[processor], rate(48000)).unwrap();
        bound.events.reserve_exact(5);
        bound.targets.reserve_exact(3);
        bound.targets[0].input_port_key.reserve_exact(11);
        let expected = bound.events.capacity() * std::mem::size_of::<PreparedNoteEvent>()
            + bound.targets.capacity() * std::mem::size_of::<PreparedNoteTarget>()
            + bound.on_events_by_author.capacity() * size_of::<usize>()
            + bound.targets[0].input_port_key.capacity();
        assert_eq!(bound.storage_bytes().unwrap(), expected);
        assert!(
            expected
                > bound.events.len() * std::mem::size_of::<PreparedNoteEvent>()
                    + bound.targets.len() * std::mem::size_of::<PreparedNoteTarget>()
                    + bound.targets[0].input_port_key.len()
        );
        let mut visited = Vec::new();
        bound
            .visit_host_owned_storage(&mut |storage| {
                visited.push(storage);
                Ok(())
            })
            .unwrap();
        assert_eq!(visited.len(), 4);
        assert_eq!(visited[0].address(), bound.events.as_ptr() as usize);
        assert_eq!(visited[1].address(), bound.targets.as_ptr() as usize);
        assert_eq!(
            visited[2].address(),
            bound.on_events_by_author.as_ptr() as usize
        );
        assert_eq!(
            visited[3].address(),
            bound.targets[0].input_port_key.as_ptr() as usize
        );
        assert_eq!(
            visited.iter().map(|storage| storage.layout().size()).sum::<usize>(),
            expected
        );
        assert!(visited
            .iter()
            .all(|storage| storage.kind()
                == crate::AudioProcessorFactoryStorageKind::CompleteAllocation));
    }

    #[test]
    fn public_cutoff_merges_held_and_original_ons_by_author_identity() {
        let target = target();
        let notes = [
            (60_u128, 1, 8),
            (40, 2, 8),
            (30, 5, 7),
            (10, 0, 5),
            (20, 3, 4),
        ]
        .into_iter()
        .map(|(id, on, off)| {
            let mut value = note(time(on, 48000), time(off, 48000));
            value.id = AudioNoteId(uuid::Uuid::from_u128(id));
            value
        })
        .collect();
        let program = program(
            vec![lane(&target, notes)],
            clap(),
            AudioProcessorInsertionPoint::PreFader,
        );
        let requested = ProjectedNoteScore::project(&program.note_lanes, rate(48000))
            .unwrap()
            .resolve_targets(&program)
            .unwrap();
        let processor = prepared(
            requested.targets[0].occurrence,
            port_contract(),
            Arc::new(AtomicUsize::new(0)),
        );
        let mut score = requested.bind(&[processor], rate(48000)).unwrap();
        assert_eq!(
            score.window(2, 5, 5).unwrap().count(),
            0,
            "PDC priming is not the public cutoff"
        );
        let actual: Vec<_> = score
            .window(2, 8, 5)
            .unwrap()
            .map(|e| (e.sample, e.phase, e.note_id.0.as_u128()))
            .collect();
        assert_eq!(
            actual,
            [
                (5, PreparedNotePhase::On, 30),
                (5, PreparedNotePhase::On, 40),
                (5, PreparedNotePhase::On, 60),
                (7, PreparedNotePhase::Off, 30)
            ]
        );
        assert_eq!(score.window(-5, 0, -5).unwrap().count(), 0);
        assert_eq!(score.entry_events(i64::MAX).count(), 0);
        assert_eq!(
            score
                .window(-5, 1, -5)
                .unwrap()
                .filter(|e| e.phase == PreparedNotePhase::On)
                .count(),
            1
        );
        // Corruption never turns a missing mate/index into an executable Off.
        let mate = score.events[0].mate_event_index;
        score.events[0].mate_event_index = usize::MAX;
        assert_eq!(
            score.validate_pairs(),
            Err("Off has no retained original On")
        );
        score.events[0].mate_event_index = mate;
        score.on_events_by_author.pop();
        assert_eq!(
            score.validate_pairs(),
            Err("Off has no retained original On")
        );
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
                "must be refused before factory".to_owned(),
            ))
        }
    }

    #[test]
    fn ordinary_prepare_keeps_endpoint_errors_and_valid_clap_runtime_refusal_before_factory() {
        let target = target();
        let mut program = program(
            vec![lane(&target, vec![note(time(1, 192000), time(1, 96000))])],
            clap(),
            AudioProcessorInsertionPoint::PreFader,
        );
        let resolver = NeverResolver::default();
        let prepare = |program: CompiledAudioProgram| {
            super::super::PreparedAudioPlan::prepare_with_processor_resolver(
                Arc::new(program),
                render(),
                super::super::AudioKernelBackend::ScalarReference,
                &resolver,
            )
        };
        assert!(matches!(
            prepare(program.clone()),
            Err(AudioCompileError::CollapsedNoteDuration { .. })
        ));
        program.note_lanes[0].notes[0].start = time(i64::MAX - 1, 1);
        program.note_lanes[0].notes[0].end = time(i64::MAX, 1);
        assert!(matches!(
            prepare(program.clone()),
            Err(AudioCompileError::NoteSampleProjection {
                source: mondrian_core::AudioTimeError::PositionOverflow,
                ..
            })
        ));
        program.note_lanes[0].notes[0].start = TimelineTime::ZERO;
        program.note_lanes[0].notes[0].end = time(1, 1);
        assert_eq!(
            prepare(program.clone()).unwrap_err(),
            AudioCompileError::NoteExecutionNotSupported {
                lane_id: program.note_lanes[0].id,
                processor_id: target.processor_id
            }
        );
        assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn empty_scores_keep_zero_payload_and_do_not_request_a_factory_port() {
        let program = program(Vec::new(), clap(), AudioProcessorInsertionPoint::PreFader);
        let projected = ProjectedNoteScore::project(&[], rate(48000)).unwrap();
        assert!(projected.require_runtime_support().is_ok());
        let requested = projected.resolve_targets(&program).unwrap();
        let bound = requested.bind(&[], rate(48000)).unwrap();
        assert!(bound.is_empty());
        assert!(bound.targets().is_empty());
        assert_eq!(bound.storage_bytes().unwrap(), 0);
    }

    #[test]
    fn impossible_event_extent_refuses_before_any_storage_request() {
        assert!(matches!(
            reserve::<PreparedNoteEvent>(usize::MAX),
            Err(AudioCompileError::NoteEventCountOverflow)
        ));
        assert!(reserve::<PreparedNoteEvent>(0).unwrap().is_empty());
    }

    // Actual IR owner/capacity controls; no execution or allocator ceiling grant.
    mod accepted_capacity_controls {
        use super::*;

        fn observed_vec_excess<T>() {
            assert!(
                std::mem::size_of::<T>() > 0,
                "real reserve consumer is not ZST"
            );
            let values = Vec::<T>::with_capacity(8);
            let actual = values.capacity();
            assert!(actual > 1);
            assert!(values.is_empty(), "length is not allocated capacity");
            assert!(matches!(
                require_requested_capacity(actual, 1),
                Err(AudioCompileError::InvalidPreparedGraph(detail))
                    if detail == "prepared Note storage capacity exceeds request"
            ));
            assert!(require_requested_capacity(actual, actual).is_ok());
            assert_eq!(
                values.capacity(),
                actual,
                "refusal does not resize an owner"
            );
        }

        #[test]
        fn real_overreserved_arrays_use_capacity_for_every_ir_reserve_consumer() {
            observed_vec_excess::<ProjectedLane>();
            observed_vec_excess::<PreparedNoteEvent>();
            observed_vec_excess::<RequestedTarget>();
            observed_vec_excess::<PreparedNoteTarget>();
            for count in [0, 1, 3] {
                assert!(reserve::<ProjectedLane>(count).unwrap().capacity() <= count);
                assert!(reserve::<PreparedNoteEvent>(count).unwrap().capacity() <= count);
                assert!(reserve::<RequestedTarget>(count).unwrap().capacity() <= count);
                assert!(reserve::<PreparedNoteTarget>(count).unwrap().capacity() <= count);
            }
        }

        #[test]
        fn real_overreserved_string_is_refused_against_byte_length_without_mutation() {
            let mut key = String::with_capacity(64);
            key.push_str(" notes.é ");
            let requested = key.len();
            let actual = key.capacity();
            let pointer = key.as_ptr();
            assert!(requested > key.chars().count(), "request is UTF-8 bytes");
            assert!(actual > requested);
            assert!(matches!(
                require_requested_capacity(actual, requested),
                Err(AudioCompileError::InvalidPreparedGraph(detail))
                    if detail == "prepared Note storage capacity exceeds request"
            ));
            assert_eq!(key.as_ptr(), pointer);
            assert_eq!(key.capacity(), actual);
            assert_eq!(key, " notes.é ");
            assert!(require_requested_capacity(actual, actual).is_ok());
        }

        #[test]
        fn project_checks_its_actual_owned_key_against_original_bytes() {
            let target = target();
            let mut program = program(
                vec![lane(&target, vec![note(time(0, 1), time(1, 1))])],
                clap(),
                AudioProcessorInsertionPoint::PreFader,
            );
            let original = &mut program.note_lanes[0].target.input_port_key;
            original.reserve_exact(64);
            let original_capacity = original.capacity();
            let original_pointer = original.as_ptr();
            let original_length = original.len();
            assert!(original_capacity > original_length);
            let projected = ProjectedNoteScore::project(&program.note_lanes, rate(48000)).unwrap();
            let cloned = &projected.lanes[0].target.input_port_key;
            assert_eq!(cloned, &program.note_lanes[0].target.input_port_key);
            assert!(cloned.capacity() <= original_length);
            assert_ne!(
                cloned.as_ptr(),
                original_pointer,
                "a new live String owns its clone"
            );
            assert_eq!(
                program.note_lanes[0].target.input_port_key.capacity(),
                original_capacity
            );
            assert_eq!(
                program.note_lanes[0].target.input_port_key.as_ptr(),
                original_pointer
            );
        }

        #[test]
        fn consuming_ir_transitions_move_events_and_each_key_once_with_literal_bits() {
            let target = target();
            let first = note(time(0, 1), time(1, 1));
            let second = note(time(1, 1), time(2, 1));
            let first_id = first.id;
            let second_id = second.id;
            let program = program(
                vec![lane(&target, vec![first]), lane(&target, vec![second])],
                clap(),
                AudioProcessorInsertionPoint::PostFader,
            );
            let projected = ProjectedNoteScore::project(&program.note_lanes, rate(48000)).unwrap();
            assert_eq!(projected.lanes.len(), 2);
            assert_eq!(projected.events.len(), 4);
            assert!(projected.lanes.capacity() <= 2);
            assert!(projected.events.capacity() <= 4);
            let event_pointer = projected.events.as_ptr();
            let event_capacity = projected.events.capacity();
            let keys: Vec<_> = projected
                .lanes
                .iter()
                .map(|lane| {
                    (
                        lane.lane_id,
                        lane.target.input_port_key.as_ptr(),
                        lane.target.input_port_key.capacity(),
                    )
                })
                .collect();
            let expected = [
                (
                    0,
                    PreparedNotePhase::On,
                    program.note_lanes[0].id,
                    first_id,
                    0x8000_0000_0000_0000,
                ),
                (
                    48000,
                    PreparedNotePhase::Off,
                    program.note_lanes[0].id,
                    first_id,
                    0x3FD3_3333_3333_3335,
                ),
                (
                    48000,
                    PreparedNotePhase::On,
                    program.note_lanes[1].id,
                    second_id,
                    0x8000_0000_0000_0000,
                ),
                (
                    96000,
                    PreparedNotePhase::Off,
                    program.note_lanes[1].id,
                    second_id,
                    0x3FD3_3333_3333_3335,
                ),
            ];
            for (event, &(sample, phase, lane_id, note_id, velocity)) in
                projected.events.iter().zip(&expected)
            {
                assert_eq!(
                    (
                        event.sample,
                        event.phase,
                        event.lane_id,
                        event.note_id,
                        event.velocity.to_bits()
                    ),
                    (sample, phase, lane_id, note_id, velocity)
                );
                assert_eq!((event.channel, event.key), (3, 60));
            }
            let requested = projected.resolve_targets(&program).unwrap();
            assert_eq!(requested.targets.len(), 2);
            assert!(requested.targets.capacity() <= 2);
            assert_eq!(requested.events.as_ptr(), event_pointer);
            assert_eq!(requested.events.capacity(), event_capacity);
            for target in &requested.targets {
                let &(_, pointer, capacity) =
                    keys.iter().find(|key| key.0 == target.lane_id).unwrap();
                assert_eq!(target.input_port_key.as_ptr(), pointer);
                assert_eq!(target.input_port_key.capacity(), capacity);
            }
            let queries = Arc::new(AtomicUsize::new(0));
            let processor = prepared(
                requested.targets[0].occurrence,
                port_contract(),
                queries.clone(),
            );
            let frozen_queries = queries.load(Ordering::Relaxed);
            let bound = requested.bind(&[processor], rate(48000)).unwrap();
            assert_eq!(bound.targets.len(), 2);
            assert!(bound.targets.capacity() <= 2);
            assert_eq!(bound.events.as_ptr(), event_pointer);
            assert_eq!(bound.events.capacity(), event_capacity);
            assert_eq!(
                queries.load(Ordering::Relaxed),
                frozen_queries,
                "bind only borrows cached metadata"
            );
            for target in &bound.targets {
                let &(_, pointer, capacity) =
                    keys.iter().find(|key| key.0 == target.lane_id).unwrap();
                assert_eq!(target.input_port_key.as_ptr(), pointer);
                assert_eq!(target.input_port_key.capacity(), capacity);
                assert_eq!(
                    (
                        target.processor_slot,
                        target.native_port_id,
                        target.native_port_index
                    ),
                    (0, 123, 1)
                );
            }
            assert_eq!(bound.events.len(), expected.len());
            for (event, &(sample, phase, lane_id, note_id, velocity)) in
                bound.events.iter().zip(&expected)
            {
                assert_eq!(
                    (
                        event.sample,
                        event.phase,
                        event.lane_id,
                        event.note_id,
                        event.velocity.to_bits()
                    ),
                    (sample, phase, lane_id, note_id, velocity)
                );
            }
        }

        #[test]
        fn empty_ir_stays_zero_capacity_through_all_consuming_stages() {
            let target = target();
            let program = program(
                vec![lane(&target, Vec::new())],
                clap(),
                AudioProcessorInsertionPoint::PreFader,
            );
            let projected = ProjectedNoteScore::project(&program.note_lanes, rate(48000)).unwrap();
            assert_eq!(
                (projected.lanes.capacity(), projected.events.capacity()),
                (0, 0)
            );
            assert!(projected.require_runtime_support().is_ok());
            let requested = projected.resolve_targets(&program).unwrap();
            assert_eq!(
                (requested.targets.capacity(), requested.events.capacity()),
                (0, 0)
            );
            let bound = requested.bind(&[], rate(48000)).unwrap();
            assert_eq!((bound.targets.capacity(), bound.events.capacity()), (0, 0));
            assert_eq!(bound.storage_bytes().unwrap(), 0);
        }

        #[test]
        fn accepted_capacity_checks_keep_existing_projection_and_binding_error_order() {
            let target = target();
            let bad = lane(&target, vec![note(time(1, 192000), time(1, 96000))]);
            assert_eq!(
                ProjectedNoteScore::project(std::slice::from_ref(&bad), rate(48000)).unwrap_err(),
                AudioCompileError::CollapsedNoteDuration {
                    lane_id: bad.id,
                    note_id: bad.notes[0].id,
                    start_sample: 1,
                    end_sample: 1
                }
            );
            let program = program(
                vec![lane(&target, vec![note(time(0, 1), time(1, 1))])],
                clap(),
                AudioProcessorInsertionPoint::PreFader,
            );
            let requested = ProjectedNoteScore::project(&program.note_lanes, rate(48000))
                .unwrap()
                .resolve_targets(&program)
                .unwrap();
            assert_eq!(
                requested.bind(&[], rate(44100)).unwrap_err(),
                AudioCompileError::InvalidRenderContract,
                "grid refusal precedes target/factory absence"
            );
            assert!(matches!(
                reserve::<PreparedNoteEvent>(usize::MAX),
                Err(AudioCompileError::NoteEventCountOverflow)
            ));
        }

        #[test]
        fn public_nonempty_guard_still_refuses_before_any_factory_resolution() {
            let target = target();
            let program = program(
                vec![lane(&target, vec![note(time(0, 1), time(1, 1))])],
                clap(),
                AudioProcessorInsertionPoint::PreFader,
            );
            let expected = AudioCompileError::NoteExecutionNotSupported {
                lane_id: program.note_lanes[0].id,
                processor_id: target.processor_id,
            };
            let resolver = NeverResolver::default();
            let error = super::super::super::PreparedAudioPlan::prepare_with_processor_resolver(
                Arc::new(program),
                render(),
                super::super::super::AudioKernelBackend::ScalarReference,
                &resolver,
            )
            .unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(resolver.calls.load(Ordering::Relaxed), 0);
        }
    }
}
#[cfg(test)]
#[path = "prepared_notes/phase_envelope_tests.rs"]
mod phase_envelope_tests;

#[cfg(test)]
#[path = "prepared_notes/admission_peak_tests.rs"]
mod admission_peak_tests;
