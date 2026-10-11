//! Owned, bounded parameter output from the actual CLAP process/flush callback.
//!
//! Returning true acknowledges one retained event, never a discarded event.
//! This is an execution-source owner, not author/runtime installation authority.

use super::clap_parameter_source::{
    ClapOutputSourceBusy, ClapParameterPublication, ClapParameterSource,
    ClapParameterSourceFailure, CLAP_PARAMETER_SOURCE_EVENTS,
};
use super::ClapParameterDescriptor;
use crate::AudioProcessorHostError;
use clack_host::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
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
        // A reservation never turns an allocator's extra capacity into a grant.
        if events.capacity() > CLAP_OUTPUT_EVENTS || contracts.capacity() > parameters.len() {
            return Err(invalid("CLAP output owners exceed exact admitted capacity"));
        }
        let owner = Self {
            events,
            parameters: contracts,
            limit: CLAP_OUTPUT_EVENTS,
            source,
            pending_author: 0,
            open_gestures: 0,
            refusals: 0,
        };
        if owner.allocated_bytes()? > Self::reservation_bytes(parameters.len())? {
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
        let events = Layout::array::<ClapOutputParameterEvent>(CLAP_OUTPUT_EVENTS)
            .map_err(|_| invalid("CLAP output event reservation overflowed"))?;
        let parameters = Layout::array::<ParameterContract>(parameter_count)
            .map_err(|_| invalid("CLAP output parameter reservation overflowed"))?;
        events
            .size()
            .checked_add(parameters.size())
            .ok_or_else(|| invalid("CLAP output owner reservation overflowed"))
    }

    /// Borrow one callback batch; ordering resets only at the native call boundary.
    pub fn writer(
        &mut self,
        origin: ClapOutputOrigin,
    ) -> Result<ClapOutputWriter<'_>, ClapOutputSourceBusy> {
        let publication = self.source.begin_output()?;
        Ok(ClapOutputWriter {
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
        events
            .size()
            .checked_add(parameters.size())
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

    /// Borrow the standard CLAP output callback list; no intermediate event arena.
    pub fn as_output(&mut self) -> OutputEvents<'_> {
        OutputEvents::from_buffer(self)
    }
}

impl OutputEventBuffer for ClapOutputWriter<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let header = event.header().as_raw();
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
