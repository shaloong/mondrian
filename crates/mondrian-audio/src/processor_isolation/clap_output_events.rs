//! Owned, bounded parameter output from the actual CLAP process/flush callback.
//!
//! Returning true acknowledges one retained event, never a discarded event.
//! This is an execution-source owner, not author/runtime installation authority.

use super::clap_parameter_source::{
    ClapOutputSourceBusy, ClapParameterPublication, ClapParameterSource,
    ClapParameterSourceFailure, CLAP_PARAMETER_SOURCE_EVENTS,
};
use super::ClapParameterDescriptor;
use crate::{AudioNoteEndEvent, AudioProcessorHostError};
use clack_host::events::event_types::{
    NoteEndEvent, ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_host::events::io::{OutputEventBuffer, TryPushError};
use clack_host::events::spaces::EventSpaceId;
use clack_host::events::{Event, UnknownEvent};
use clack_host::prelude::OutputEvents;
use std::alloc::Layout;
use std::sync::Arc;

/// Maximum retained callbacks before the sole non-RT native observer accepts them.
/// A full source returns false to the native producer; no retained prefix is cleared.
pub const CLAP_OUTPUT_EVENTS: usize = CLAP_PARAMETER_SOURCE_EVENTS as usize;

/// Exact callback origin. Flush has no sample clock and cannot invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapOutputOrigin {
    /// One successful process call's signed signal grid before sample offset.
    Process {
        /// Signed signal sample position, already adjusted by the sole PDC path.
        start_sample: i64,
        /// Actual prepared sample rate.
        sample_rate: u32,
        /// Actual native call extent.
        frames: u32,
    },
    /// Parameters exchanged on the actual inactive main-thread lifecycle path.
    InactiveFlush,
    /// Parameters exchanged on the actual active audio-thread lifecycle path.
    ActiveFlush,
}

impl ClapOutputOrigin {
    fn admits(self, time: u32) -> bool {
        match self {
            Self::Process { start_sample, sample_rate, frames } => {
                sample_rate != 0
                    && time < frames
                    && start_sample.checked_add(i64::from(time)).is_some()
            }
            // The specification explicitly discards sample timing in flush.
            // Preserve the raw offset without inventing a timeline or extra rejection.
            Self::InactiveFlush | Self::ActiveFlush => true,
        }
    }
}

/// Actual kind retained without a native cookie pointer or borrowed event bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClapOutputParameterKind {
    /// The exact finite plain value. Readonly and DONT_RECORD remain observable
    /// transients and may not be projected to canonical author writes.
    Value(f64),
    /// An actual native gesture start, never manufactured around a Value.
    GestureBegin,
    /// An actual native gesture finish, never manufactured during a drain.
    GestureEnd,
}

/// One accepted callback in source order with its original sample/flag evidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClapOutputParameterEvent {
    /// Non-wrapping accepted callback order in this output owner.
    pub sequence: u64,
    /// Exact process or flush call producing this callback.
    pub origin: ClapOutputOrigin,
    /// Native definition-local CLAP ID, not an authored ParameterId.
    pub parameter: u32,
    /// Original sample offset within that call.
    pub sample_offset: u32,
    /// Original official/unknown flag bits, without from_bits_truncate.
    pub flags: u32,
    /// Readonly definition fact captured before audio processing.
    pub read_only: bool,
    /// Actual callback phase and, for Value, plain scalar.
    pub kind: ClapOutputParameterKind,
}

/// A rejection is a source fact; unsupported semantics are not successful support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapOutputRefusal {
    /// This host slice does not implement the event kind/space.
    UnsupportedEvent,
    /// A known parameter event has the wrong concrete ABI extent.
    InvalidExtent,
    /// The event offset cannot belong to its actual process/flush call.
    InvalidTime,
    /// The callback list violates nondecreasing sample-offset order.
    InvalidOrder,
    /// The native parameter ID is not in the admitted definition.
    UnknownParameter,
    /// Voice-targeted parameter semantics are not implemented by this slice.
    PerVoiceParameter,
    /// The plain value violates finite/range/step definition facts.
    InvalidValue,
    /// The retained source has no already admitted event slot.
    Capacity,
    /// The accepted source sequence cannot advance without wrapping.
    SequenceExhausted,
    /// Reserving global output/main order exceeded bounded contention work.
    OrderContention,
    /// End had no matching retained Begin, or the bounded open count was full.
    InvalidBoundary,
    /// Invalid concrete Note ID or input P/C/K component (not parent pairing).
    InvalidNoteAddress,
    /// Future/unknown flags cannot authorize this bounded first Note subset.
    InvalidNoteFlags,
}

#[derive(Clone, Copy)]
struct ParameterContract {
    id: u32,
    min: f64,
    max: f64,
    stepped: bool,
    read_only: bool,
    open_gestures: u32,
}

/// One exclusive native output source. Construction allocates before processing.
///
/// Process and flush borrow this same owner; they neither allocate nor discard
/// accepted callbacks. Queue acceptance is independent of author Session approval.
pub struct ClapOutputEvents {
    events: Vec<ClapOutputParameterEvent>,
    // Copied observations from one successful native operation, never a ledger.
    note_ends: Vec<AudioNoteEndEvent>,
    note_limit: usize,
    note_failed: bool,
    parameters: Vec<ParameterContract>,
    limit: usize,
    source: Arc<ClapParameterSource>,
    pending_author: u32,
    open_gestures: u32,
    refusals: u16,
}

fn invalid(detail: &'static str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

impl ClapOutputEvents {
    /// Reserve exact bounded owners using the already validated plugin definition.
    pub fn new(
        parameters: &[ClapParameterDescriptor],
        source: Arc<ClapParameterSource>,
    ) -> Result<Self, AudioProcessorHostError> {
        Self::new_with_note_capacity(parameters, source, 0)
    }

    /// Preallocate operation-local observations before activation. Notes do not
    /// reserve parameter source slots, sequences, author writes or voice credit.
    pub fn new_with_note_capacity(
        parameters: &[ClapParameterDescriptor],
        source: Arc<ClapParameterSource>,
        note_capacity: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if note_capacity > crate::AUDIO_NOTE_ENDS_PER_BLOCK {
            return Err(invalid("CLAP NoteEnd storage exceeds the bounded subset"));
        }
        super::clap_discovery::validate_parameters(parameters)?;
        let mut contracts = Vec::new();
        contracts
            .try_reserve_exact(parameters.len())
            .map_err(|_| invalid("CLAP output parameter contract storage unavailable"))?;
        for parameter in parameters {
            contracts.push(ParameterContract {
                id: parameter.id,
                min: parameter.min_value,
                max: parameter.max_value,
                stepped: parameter.stepped,
                read_only: parameter.read_only,
                open_gestures: 0,
            });
        }
        contracts.sort_unstable_by_key(|parameter| parameter.id);
        let mut events = Vec::new();
        events
            .try_reserve_exact(CLAP_OUTPUT_EVENTS)
            .map_err(|_| invalid("CLAP output event storage unavailable"))?;
        let mut note_ends = Vec::new();
        note_ends
            .try_reserve_exact(note_capacity)
            .map_err(|_| invalid("CLAP NoteEnd observation storage unavailable"))?;
        // A reservation never turns an allocator's extra capacity into a grant.
        if events.capacity() > CLAP_OUTPUT_EVENTS
            || contracts.capacity() > parameters.len()
            || note_ends.capacity() > note_capacity
        {
            return Err(invalid("CLAP output owners exceed exact admitted capacity"));
        }
        let owner = Self {
            events,
            note_ends,
            note_limit: note_capacity,
            note_failed: false,
            parameters: contracts,
            limit: CLAP_OUTPUT_EVENTS,
            source,
            pending_author: 0,
            open_gestures: 0,
            refusals: 0,
        };
        if owner.allocated_bytes()?
            > Self::reservation_bytes_with_notes(parameters.len(), note_capacity)?
        {
            return Err(invalid(
                "CLAP output allocation exceeds the admitted reservation",
            ));
        }
        if !owner.source.claim_output_owner() {
            return Err(invalid(
                "CLAP parameter source already has its retained output owner",
            ));
        }
        Ok(owner)
    }

    /// Exact requested owner layouts; actual Vec capacity is checked at creation.
    pub fn reservation_bytes(parameter_count: usize) -> Result<usize, AudioProcessorHostError> {
        Self::reservation_bytes_with_notes(parameter_count, 0)
    }

    /// Include the actual concrete copied observation layout in preparation.
    pub fn reservation_bytes_with_notes(
        parameter_count: usize,
        note_capacity: usize,
    ) -> Result<usize, AudioProcessorHostError> {
        let events = Layout::array::<ClapOutputParameterEvent>(CLAP_OUTPUT_EVENTS)
            .map_err(|_| invalid("CLAP output event reservation overflowed"))?;
        let parameters = Layout::array::<ParameterContract>(parameter_count)
            .map_err(|_| invalid("CLAP output parameter reservation overflowed"))?;
        let notes = Layout::array::<AudioNoteEndEvent>(note_capacity)
            .map_err(|_| invalid("CLAP NoteEnd observation reservation overflowed"))?;
        events
            .size()
            .checked_add(parameters.size())
            .and_then(|bytes| bytes.checked_add(notes.size()))
            .ok_or_else(|| invalid("CLAP output owner reservation overflowed"))
    }

    /// Borrow one callback batch; ordering resets only at the native call boundary.
    pub fn writer(
        &mut self,
        origin: ClapOutputOrigin,
    ) -> Result<ClapOutputWriter<'_>, ClapOutputSourceBusy> {
        let publication = self.source.begin_output()?;
        Ok(ClapOutputWriter {
            note_ends: &mut self.note_ends,
            note_failed: &mut self.note_failed,
            note_capability: None,
            events: &mut self.events,
            parameters: &mut self.parameters,
            limit: self.limit,
            refusals: &mut self.refusals,
            pending_author: &mut self.pending_author,
            open_gestures: &mut self.open_gestures,
            source: &self.source,
            _publication: publication,
            origin,
            previous_time: None,
        })
    }

    /// Explicit Process-only capability. Configuration rejection precedes the
    /// native operation, and a busy publication cannot clear earlier observations.
    /// The caller copies/accepts prior operation ends before starting another one.
    pub fn writer_with_notes(
        &mut self,
        origin: ClapOutputOrigin,
        native_input_count: usize,
        capacity: usize,
    ) -> Result<ClapOutputWriter<'_>, AudioProcessorHostError> {
        if !matches!(origin, ClapOutputOrigin::Process { .. })
            || native_input_count == 0
            || native_input_count > crate::AUDIO_NOTE_INPUT_PORTS
            || capacity == 0
            || capacity > self.note_limit
        {
            return Err(invalid("CLAP NoteEnd operation capability is invalid"));
        }
        let publication = self
            .source
            .begin_output()
            .map_err(|_| AudioProcessorHostError::NativeParameterSourceUnavailable)?;
        self.note_ends.clear();
        self.note_failed = false;
        Ok(ClapOutputWriter {
            note_ends: &mut self.note_ends,
            note_failed: &mut self.note_failed,
            note_capability: Some((native_input_count, capacity)),
            events: &mut self.events,
            parameters: &mut self.parameters,
            limit: self.limit,
            refusals: &mut self.refusals,
            pending_author: &mut self.pending_author,
            open_gestures: &mut self.open_gestures,
            source: &self.source,
            _publication: publication,
            origin,
            previous_time: None,
        })
    }

    /// Copied original input tuple observations for the most recent notes-enabled
    /// Process. No sample/velocity evidence or worker voice ledger is invented.
    pub fn note_ends(&self) -> &[AudioNoteEndEvent] {
        &self.note_ends
    }

    /// Sticky within that operation even if the producer ignored try_push(false).
    /// The native DSP caller must fault before accepting PCM or completions.
    pub const fn note_failed(&self) -> bool {
        self.note_failed
    }

    /// Read the same callback source without consuming its retained prefix.
    pub fn source(&self) -> &ClapParameterSource {
        &self.source
    }

    /// Official event flags are interpreted only when every supplied bit is known.
    /// Unknown bits remain exact transient evidence; they cannot authorize writes.
    pub(super) fn recordable(event: &ClapOutputParameterEvent) -> bool {
        !event.read_only
            && event.flags & !clack_host::events::EventFlags::all().bits() == 0
            && event.flags & clack_host::events::EventFlags::DONT_RECORD.bits() == 0
    }

    /// Check the output lane before either lane in a common delivery retires.
    pub(super) fn prefix_matches(&self, through: Option<u64>, count: usize) -> bool {
        if count == 0 {
            return through.is_none();
        }
        count <= self.events.len() && Some(self.events[count - 1].sequence) == through
    }

    fn retire_prevalidated(&mut self, count: usize) {
        let retired_author =
            self.events[..count].iter().filter(|event| Self::recordable(event)).count() as u32;
        self.events.drain(..count);
        self.pending_author -= retired_author;
        self.source.retire_slots(count as u32);
        self.source.publish_output_count(
            self.events.len() as u32,
            self.pending_author,
            self.open_gestures,
        );
    }

    /// Retained actual events in non-wrapping callback order.
    pub fn events(&self) -> &[ClapOutputParameterEvent] {
        &self.events
    }

    /// Actual allocated owner bytes, including empty Vec capacities.
    pub fn allocated_bytes(&self) -> Result<usize, AudioProcessorHostError> {
        let events = Layout::array::<ClapOutputParameterEvent>(self.events.capacity())
            .map_err(|_| invalid("CLAP output actual event extent overflowed"))?;
        let parameters = Layout::array::<ParameterContract>(self.parameters.capacity())
            .map_err(|_| invalid("CLAP output actual parameter extent overflowed"))?;
        let notes = Layout::array::<AudioNoteEndEvent>(self.note_ends.capacity())
            .map_err(|_| invalid("CLAP NoteEnd actual observation extent overflowed"))?;
        events
            .size()
            .checked_add(parameters.size())
            .and_then(|bytes| bytes.checked_add(notes.size()))
            .ok_or_else(|| invalid("CLAP output actual owner bytes overflowed"))
    }

    /// Every observed refusal remains explicit; reading never clears accepted events.
    pub const fn refused(&self, reason: ClapOutputRefusal) -> bool {
        self.refusals & (1 << (reason as u8)) != 0
    }
}

/// Atomically retire the exact two-lane callback prefix after the caller has
/// admitted its retained binding/revision and canonical consumer acceptance.
/// A count/sequence/owner mismatch changes neither lane. Later callbacks and
/// sticky source failure survive; this function issues no author permission.
pub(super) fn acknowledge_parameter_prefix(
    output: &mut ClapOutputEvents,
    controls: &super::clap_parameter_source::ClapParameterControlSource<'_>,
    output_through: Option<u64>,
    output_count: usize,
    control_through: Option<u64>,
    control_count: usize,
) -> bool {
    let source = Arc::clone(&output.source);
    if output_count == 0 && control_count == 0 || !controls.belongs_to(&source) {
        return false;
    }
    let Ok(_publication) = source.begin_main() else {
        return false;
    };
    if !output.prefix_matches(output_through, output_count)
        || !controls.prefix_matches(control_through, control_count)
    {
        return false;
    }
    // Both owners are exclusive after the actual DSP join, and the main Cell
    // cannot be called concurrently. No SDK/user callback occurs from here to
    // the publication's release, so no half-retired acknowledgement is possible.
    output.retire_prevalidated(output_count);
    controls.retire_prevalidated(control_count);
    true
}

/// Exclusive SDK adapter over this exact owner for one native process/flush.
pub struct ClapOutputWriter<'a> {
    note_ends: &'a mut Vec<AudioNoteEndEvent>,
    note_failed: &'a mut bool,
    note_capability: Option<(usize, usize)>,
    events: &'a mut Vec<ClapOutputParameterEvent>,
    parameters: &'a mut [ParameterContract],
    limit: usize,
    refusals: &'a mut u16,
    pending_author: &'a mut u32,
    open_gestures: &'a mut u32,
    source: &'a ClapParameterSource,
    // The !Send/!Sync publication guard retires on this actual callback owner.
    _publication: ClapParameterPublication<'a>,
    origin: ClapOutputOrigin,
    previous_time: Option<u32>,
}

impl ClapOutputWriter<'_> {
    fn reject(&mut self, reason: ClapOutputRefusal) -> TryPushError {
        *self.refusals |= 1 << (reason as u8);
        TryPushError::new()
    }

    fn reject_note(&mut self, reason: ClapOutputRefusal) -> TryPushError {
        if self.note_capability.is_some() {
            *self.note_failed = true;
        }
        self.reject(reason)
    }

    fn try_push_note_end(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        // SDK checks exact extent, core space and type before a concrete borrow.
        let Some(end) = event.as_event::<NoteEndEvent>() else {
            return Err(self.reject_note(ClapOutputRefusal::InvalidExtent));
        };
        let Some((input_count, capacity)) = self.note_capability else {
            return Err(self.reject_note(ClapOutputRefusal::UnsupportedEvent));
        };
        let raw = end.as_raw();
        // Preserve and check raw bits, never from_bits_truncate. Time and
        // velocity are explicitly ignored, including u32::MAX and NaN.
        if raw.header.flags & !clack_host::events::EventFlags::all().bits() != 0 {
            return Err(self.reject_note(ClapOutputRefusal::InvalidNoteFlags));
        }
        if raw.note_id == -1 {
            // Standard-valid wildcard Note ID exceeds this first host subset.
            return Err(self.reject_note(ClapOutputRefusal::UnsupportedEvent));
        }
        let observation = AudioNoteEndEvent {
            native_note_id: raw.note_id,
            port_index: raw.port_index,
            channel: raw.channel,
            key: raw.key,
            flags: raw.header.flags,
        };
        if !observation.valid(input_count) {
            return Err(self.reject_note(ClapOutputRefusal::InvalidNoteAddress));
        }
        if self.note_ends.len() >= capacity {
            return Err(self.reject_note(ClapOutputRefusal::Capacity));
        }
        self.note_ends.push(observation);
        Ok(())
    }

    /// Borrow the standard CLAP output callback list; no intermediate event arena.
    pub fn as_output(&mut self) -> OutputEvents<'_> {
        OutputEvents::from_buffer(self)
    }
}

impl OutputEventBuffer for ClapOutputWriter<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let header = event.header().as_raw();
        if header.space_id == EventSpaceId::core().id() && header.type_ == NoteEndEvent::TYPE_ID {
            // A NoteEnd has no usable timestamp. Even refusal/invalid extent
            // cannot pollute the parameter/gesture ordering cursor.
            return self.try_push_note_end(event);
        }
        if !self.origin.admits(header.time) {
            return Err(self.reject(ClapOutputRefusal::InvalidTime));
        }
        if self.previous_time.is_some_and(|previous| header.time < previous) {
            return Err(self.reject(ClapOutputRefusal::InvalidOrder));
        }
        // Even an unsupported event occupies its actual position in the native
        // callback list; rejecting it cannot make a later older offset legal.
        self.previous_time = Some(header.time);
        // Downcast only through the SDK's exact size/space/type check. Its
        // generic as_core_event path does not itself validate concrete sizes.
        let (parameter, kind) = if let Some(value) = event.as_event::<ParamValueEvent>() {
            let raw = value.as_raw();
            if raw.note_id != -1 || raw.port_index != -1 || raw.channel != -1 || raw.key != -1 {
                return Err(self.reject(ClapOutputRefusal::PerVoiceParameter));
            }
            (raw.param_id, ClapOutputParameterKind::Value(raw.value))
        } else if let Some(begin) = event.as_event::<ParamGestureBeginEvent>() {
            (
                begin
                    .param_id()
                    .ok_or_else(|| self.reject(ClapOutputRefusal::UnknownParameter))?
                    .get(),
                ClapOutputParameterKind::GestureBegin,
            )
        } else if let Some(end) = event.as_event::<ParamGestureEndEvent>() {
            (
                end.param_id()
                    .ok_or_else(|| self.reject(ClapOutputRefusal::UnknownParameter))?
                    .get(),
                ClapOutputParameterKind::GestureEnd,
            )
        } else {
            let known_parameter_type = header.space_id == EventSpaceId::core().id()
                && matches!(
                    header.type_,
                    ParamValueEvent::TYPE_ID
                        | ParamGestureBeginEvent::TYPE_ID
                        | ParamGestureEndEvent::TYPE_ID
                );
            return Err(self.reject(if known_parameter_type {
                ClapOutputRefusal::InvalidExtent
            } else {
                ClapOutputRefusal::UnsupportedEvent
            }));
        };
        let Ok(index) = self.parameters.binary_search_by_key(&parameter, |item| item.id) else {
            return Err(self.reject(ClapOutputRefusal::UnknownParameter));
        };
        let contract = self.parameters[index];
        if let ClapOutputParameterKind::Value(value) = kind
            && (!value.is_finite()
                || value < contract.min
                || value > contract.max
                || contract.stepped && value.fract() != 0.0)
        {
            return Err(self.reject(ClapOutputRefusal::InvalidValue));
        }
        if self.events.len() >= self.limit {
            return Err(self.reject(ClapOutputRefusal::Capacity));
        }
        let (next_open, total_open) = match kind {
            ClapOutputParameterKind::GestureBegin => {
                let Some(next) = contract.open_gestures.checked_add(1) else {
                    return Err(self.reject(ClapOutputRefusal::InvalidBoundary));
                };
                let Some(total) = self
                    .open_gestures
                    .checked_add(1)
                    .filter(|total| *total <= CLAP_PARAMETER_SOURCE_EVENTS)
                else {
                    return Err(self.reject(ClapOutputRefusal::InvalidBoundary));
                };
                (next, total)
            }
            ClapOutputParameterKind::GestureEnd => {
                let Some(next) = contract.open_gestures.checked_sub(1) else {
                    return Err(self.reject(ClapOutputRefusal::InvalidBoundary));
                };
                let Some(total) = self.open_gestures.checked_sub(1) else {
                    return Err(self.reject(ClapOutputRefusal::InvalidBoundary));
                };
                (next, total)
            }
            ClapOutputParameterKind::Value(_) => (contract.open_gestures, *self.open_gestures),
        };
        // Main controls and output share this exact nonwrapping order. Every
        // false-returning admission check precedes owner/source mutation.
        let slot = self.source.reserve_slot().map_err(|failure| {
            self.reject(match failure {
                ClapParameterSourceFailure::OrderContention => ClapOutputRefusal::OrderContention,
                _ => ClapOutputRefusal::Capacity,
            })
        })?;
        let sequence = self.source.next().map_err(|failure| {
            self.reject(match failure {
                ClapParameterSourceFailure::OrderContention => ClapOutputRefusal::OrderContention,
                _ => ClapOutputRefusal::SequenceExhausted,
            })
        })?;
        *self.open_gestures = total_open;
        self.parameters[index].open_gestures = next_open;
        if !contract.read_only
            && header.flags & !clack_host::events::EventFlags::all().bits() == 0
            && header.flags & clack_host::events::EventFlags::DONT_RECORD.bits() == 0
        {
            // Total accepted events are bounded by the already admitted 4096 slots.
            *self.pending_author += 1;
        }
        self.events.push(ClapOutputParameterEvent {
            sequence,
            origin: self.origin,
            parameter,
            sample_offset: header.time,
            flags: header.flags,
            read_only: contract.read_only,
            kind,
        });
        slot.commit();
        Ok(())
    }
}

impl Drop for ClapOutputWriter<'_> {
    fn drop(&mut self) {
        self.source.publish_output_count(
            self.events.len() as u32,
            *self.pending_author,
            *self.open_gestures,
        );
        // The publication guard releases its epoch after these counts are ready.
    }
}

#[cfg(test)]
mod note_output_contract_tests {
    use super::*;
    use clack_host::events::EventFlags;
    use clack_host::prelude::{ClapId, Pckn};

    fn parameters() -> [ClapParameterDescriptor; 1] {
        [ClapParameterDescriptor {
            id: 17,
            name: "Gain".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 1.0,
            current_value: 1.0,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        }]
    }

    fn owner(capacity: usize) -> ClapOutputEvents {
        ClapOutputEvents::new_with_note_capacity(
            &parameters(),
            Arc::new(ClapParameterSource::new()),
            capacity,
        )
        .expect("preallocated output owners")
    }

    fn origin() -> ClapOutputOrigin {
        ClapOutputOrigin::Process { start_sample: -7, sample_rate: 48_000, frames: 4 }
    }

    fn parameter(time: u32) -> ParamValueEvent {
        ParamValueEvent::new(time, ClapId::new(17), Pckn::match_all(), 0.5)
    }

    fn end(id: i32, port: i16, channel: i16, key: i16, flags: u32) -> NoteEndEvent {
        let mut event = NoteEndEvent::new(u32::MAX, Pckn::match_all());
        // SAFETY: change only numeric payload/flags; the exact initialized ABI
        // extent/type/space/alignment and storage lifetime remain unchanged.
        let raw = unsafe { event.as_raw_mut() };
        raw.note_id = id;
        raw.port_index = port;
        raw.channel = channel;
        raw.key = key;
        raw.header.flags = flags;
        raw.velocity = f64::NAN; // The actual End velocity is deliberately ignored.
        event
    }

    #[test]
    fn note_end_max_between_parameters_keeps_parameter_source_order_and_copies_tuple() {
        let mut owner = owner(2);
        let bytes = owner.allocated_bytes().expect("actual owner layouts");
        assert_eq!(
            bytes,
            ClapOutputEvents::reservation_bytes_with_notes(1, 2).expect("grant")
        );
        let mut completion = end(30, 1, 3, 60, EventFlags::IS_LIVE.bits());
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 2).expect("Process capability");
            let mut output = writer.as_output();
            output.try_push(parameter(2)).expect("first parameter");
            output.try_push(completion).expect("End ignores MAX time and NaN velocity");
            // SAFETY: mutate a producer-owned stack copy after synchronous push.
            // Retained completion must remain a copied tuple, not an event borrow.
            unsafe { completion.as_raw_mut() }.note_id = 99;
            output.try_push(parameter(2)).expect("End cannot pollute parameter cursor");
        }
        assert_eq!(
            owner.note_ends(),
            [AudioNoteEndEvent {
                native_note_id: 30,
                port_index: 1,
                channel: 3,
                key: 60,
                flags: EventFlags::IS_LIVE.bits(),
            }]
        );
        assert!(!owner.note_failed());
        assert!(!owner.refused(ClapOutputRefusal::InvalidTime));
        assert!(!owner.refused(ClapOutputRefusal::InvalidOrder));
        assert_eq!(
            owner.events().iter().map(|event| event.sequence).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            owner.events().iter().map(|event| event.sample_offset).collect::<Vec<_>>(),
            [2, 2]
        );
        assert_eq!(owner.allocated_bytes().expect("no callback growth"), bytes);
    }

    #[test]
    fn note_end_concrete_id_accepts_raw_pck_wildcards_and_preserves_duplicates_as_observations() {
        let mut owner = owner(2);
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 2).expect("notes");
            let mut output = writer.as_output();
            output.try_push(end(30, -1, -1, -1, 3)).expect("concrete ID PCK wildcards");
            output
                .try_push(end(30, -1, -1, -1, 3))
                .expect("parent ledger owns duplicate policy");
        }
        assert_eq!(
            owner.note_ends(),
            [AudioNoteEndEvent {
                native_note_id: 30,
                port_index: -1,
                channel: -1,
                key: -1,
                flags: 3,
            }; 2]
        );
        assert!(!owner.note_failed());
        assert!(
            owner.events().is_empty(),
            "notes cannot consume parameter source sequence/slots"
        );
    }

    #[test]
    fn note_end_capacity_false_is_sticky_without_growth_or_discarding_accepted_prefix() {
        let mut owner = owner(1);
        let bytes = owner.allocated_bytes().expect("bytes");
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 1).expect("notes");
            let mut output = writer.as_output();
            output.try_push(end(10, 0, 0, 60, 0)).expect("first copied completion");
            assert!(output.try_push(end(11, 1, 0, 60, 0)).is_err());
            // Simulate a producer ignoring false and continuing with parameters.
            output.try_push(parameter(0)).expect("parameter evidence remains independent");
        }
        assert!(owner.note_failed(), "DSP caller must reject ignored false");
        assert!(owner.refused(ClapOutputRefusal::Capacity));
        assert_eq!(
            owner.note_ends(),
            [AudioNoteEndEvent {
                native_note_id: 10,
                port_index: 0,
                channel: 0,
                key: 60,
                flags: 0,
            }]
        );
        assert_eq!(owner.events().len(), 1);
        assert_eq!(owner.allocated_bytes().expect("no growth"), bytes);
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 1).expect("next operation");
            writer
                .as_output()
                .try_push(end(20, 1, 15, 127, 0))
                .expect("new operation owner");
        }
        assert!(
            !owner.note_failed(),
            "operation reset cannot clear global refusal evidence"
        );
        assert!(owner.refused(ClapOutputRefusal::Capacity));
        assert_eq!(owner.note_ends()[0].native_note_id, 20);
        assert_eq!(
            owner.events().len(),
            1,
            "retained parameter prefix survives operation reset"
        );
    }

    #[test]
    fn note_end_invalid_address_unknown_flags_and_wildcard_id_never_pollute_parameter_order() {
        let mut owner = owner(2);
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 2).expect("notes");
            let mut output = writer.as_output();
            output.try_push(parameter(2)).expect("first parameter");
            for invalid in [
                end(-2, 0, 0, 60, 0),
                end(10, -2, 0, 60, 0),
                end(10, 2, 0, 60, 0),
                end(10, 0, 16, 60, 0),
                end(10, 0, -2, 60, 0),
                end(10, 0, 0, 128, 0),
                end(10, 0, 0, -2, 0),
            ] {
                assert!(output.try_push(invalid).is_err());
            }
            assert!(output.try_push(end(10, 0, 0, 60, 1 << 31)).is_err());
            assert!(output.try_push(end(-1, -1, -1, -1, 0)).is_err());
            output.try_push(parameter(2)).expect("bad End cannot alter parameter order");
        }
        assert!(owner.note_failed());
        assert!(owner.refused(ClapOutputRefusal::InvalidNoteAddress));
        assert!(owner.refused(ClapOutputRefusal::InvalidNoteFlags));
        assert!(
            owner.refused(ClapOutputRefusal::UnsupportedEvent),
            "valid wildcard ID unsupported"
        );
        assert!(!owner.refused(ClapOutputRefusal::InvalidOrder));
        assert!(!owner.refused(ClapOutputRefusal::InvalidTime));
        assert!(owner.note_ends().is_empty());
        assert_eq!(owner.events().len(), 2);
    }

    #[test]
    fn note_end_wrong_extent_is_classified_before_time_and_parameter_order() {
        let mut owner = owner(1);
        let truncated = clap_sys::events::clap_event_header {
            size: size_of::<clap_sys::events::clap_event_header>() as u32,
            time: u32::MAX,
            space_id: EventSpaceId::core().id(),
            type_: NoteEndEvent::TYPE_ID,
            flags: 0,
        };
        // SAFETY: exactly the declared initialized, aligned header bytes exist.
        // Concrete downcast must reject size before accessing any Note payload.
        let unknown = unsafe { UnknownEvent::from_raw(&truncated) };
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 1).expect("notes");
            let mut output = writer.as_output();
            output.try_push(parameter(2)).expect("parameter");
            assert!(output.try_push(unknown).is_err());
            output.try_push(parameter(2)).expect("extent failure did not alter ordering");
        }
        assert!(owner.note_failed());
        assert!(owner.refused(ClapOutputRefusal::InvalidExtent));
        assert!(!owner.refused(ClapOutputRefusal::InvalidTime));
        assert!(!owner.refused(ClapOutputRefusal::InvalidOrder));
        assert!(owner.note_ends().is_empty());
    }

    #[test]
    fn no_note_and_flush_writers_refuse_max_end_without_clock_or_discarding_process_observations() {
        let mut owner = owner(1);
        {
            let mut writer = owner.writer_with_notes(origin(), 2, 1).expect("Process notes");
            writer.as_output().try_push(end(30, 1, 3, 60, 0)).expect("actual observation");
        }
        for call in [
            origin(),
            ClapOutputOrigin::InactiveFlush,
            ClapOutputOrigin::ActiveFlush,
        ] {
            let mut writer = owner.writer(call).expect("old parameter-only writer");
            let mut output = writer.as_output();
            output.try_push(parameter(2)).expect("before unsupported End");
            assert!(output.try_push(end(30, 1, 3, 60, 0)).is_err());
            output
                .try_push(parameter(2))
                .expect("unsupported End has no parameter timestamp");
        }
        assert!(owner.refused(ClapOutputRefusal::UnsupportedEvent));
        assert!(!owner.refused(ClapOutputRefusal::InvalidTime));
        assert!(!owner.refused(ClapOutputRefusal::InvalidOrder));
        assert!(!owner.note_failed());
        assert_eq!(owner.note_ends()[0].native_note_id, 30);
        assert_eq!(owner.events().len(), 6);
        assert!(owner.writer_with_notes(ClapOutputOrigin::InactiveFlush, 2, 1).is_err());
        assert_eq!(owner.note_ends()[0].native_note_id, 30);
        assert!(owner.writer_with_notes(origin(), 0, 1).is_err());
        assert!(owner.writer_with_notes(origin(), 2, 2).is_err());
        assert!(ClapOutputEvents::reservation_bytes_with_notes(0, usize::MAX).is_err());
    }
}
