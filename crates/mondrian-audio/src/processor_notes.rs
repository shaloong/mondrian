//! Note input and completion facts for the existing processor block IO.
//!
//! Author IDs and time projection stay in the prepared plan/Session. These
//! borrowed events contain only admitted native delivery data. They do not own
//! a clock, author state, voice ledger or a second processor invocation path.

use crate::AudioProcessorHostError;
use std::collections::BTreeSet;

/// Bounded first Note input subset. Storage is charged during preparation.
pub const AUDIO_NOTE_INPUT_PORTS: usize = 32;
/// Maximum input events in one native operation, independent of parameter lanes.
pub const AUDIO_NOTE_EVENTS_PER_BLOCK: usize = 4096;
/// Maximum copied native completions before the actual operation returns.
pub const AUDIO_NOTE_ENDS_PER_BLOCK: usize = 4096;
/// Maximum retained Off-pairing records per native processor occurrence.
pub const AUDIO_NOTE_RECORDS: usize = 1024;

/// Definition-owned input key bound to one restored native instance's port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProcessorNoteInputPort {
    /// Stable author key; display text and port order are not identity.
    pub port_key: String,
    /// Definition-local stable unsigned ID, interpreted by its format adapter.
    pub native_id: u32,
    /// Concrete index in that instance's negotiated input list.
    pub native_index: u16,
}

/// Finite event and voice extents offered by the prepared adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioNoteExecutionLimits {
    /// Input events in each operation, including On and Off.
    pub input_events: usize,
    /// Copied completion callbacks in each operation, including duplicates.
    pub output_ends: usize,
    /// Retained pairing records, including early-ended notes awaiting Off.
    pub retained_records: usize,
    /// Pressed/releasing native voices; Off alone returns no capacity credit.
    pub live_voices: usize,
}

/// Exact port/capacity contract. Its default explicitly admits no Note input.
///
/// The first executable consumer uses CLAP concrete On/Off IDs and native End
/// callbacks. This contract does not imply VST3 completion equivalence, wildcard
/// input, MIDI, expression, polyphonic modulation or arbitrary-entry replay.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProcessorNoteInputContract {
    /// In native ABI order, revalidated after restoring state and on creation.
    pub ports: Vec<AudioProcessorNoteInputPort>,
    /// Exact admitted storage extents; all zero when unsupported.
    pub limits: AudioNoteExecutionLimits,
}

impl AudioProcessorNoteInputContract {
    /// Validate immutable factory metadata outside processing.
    pub fn validate(&self) -> Result<(), AudioProcessorHostError> {
        let invalid =
            || AudioProcessorHostError::InvalidContract("invalid Note input contract".to_owned());
        if self.ports.is_empty() {
            return if self.limits == AudioNoteExecutionLimits::default() {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        let limits = self.limits;
        if self.ports.len() > AUDIO_NOTE_INPUT_PORTS
            || !(1..=AUDIO_NOTE_EVENTS_PER_BLOCK).contains(&limits.input_events)
            || !(1..=AUDIO_NOTE_ENDS_PER_BLOCK).contains(&limits.output_ends)
            || !(1..=AUDIO_NOTE_RECORDS).contains(&limits.retained_records)
            || !(1..=limits.retained_records).contains(&limits.live_voices)
        {
            return Err(invalid());
        }
        let mut keys = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for (index, port) in self.ports.iter().enumerate() {
            if port.port_key.is_empty()
                || port.port_key.len() > 128
                || port.port_key.chars().any(char::is_control)
                || port.native_id == u32::MAX
                || usize::from(port.native_index) != index
                || !keys.insert(port.port_key.as_str())
                || !ids.insert(port.native_id)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

/// Input phase. Off precedes On at the same prepared sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioNoteEventKind {
    /// Deliver the original On tuple once; this does not prove native termination.
    Off,
    /// Start a concrete voice, including when velocity is zero.
    On,
}

/// One exact event in the same signal window as the processor's parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioNoteEvent {
    /// Offset in this operation's half-open sample window.
    pub sample_offset: u32,
    /// Admitted phase, independently ordered from the parameter stream.
    pub kind: AudioNoteEventKind,
    /// Concrete input index in the exact prepared native port contract.
    pub port_index: u16,
    /// MIDI-style channel 0..15; no input wildcard is admitted.
    pub channel: u8,
    /// Pitch 0..127; no input wildcard is admitted.
    pub key: u8,
    /// Nonnegative occurrence-local ID, not reused while the producer is alive.
    pub native_note_id: i32,
    /// Exact authored f64 velocity; zero On is never converted to Off.
    pub velocity: f64,
}

impl AudioNoteEvent {
    /// Read-only scalar/extent check before any native delivery.
    pub(crate) fn valid(self, frames: usize, ports: usize) -> bool {
        (self.sample_offset as usize) < frames
            && usize::from(self.port_index) < ports
            && self.channel <= 15
            && self.key <= 127
            && self.native_note_id >= 0
            && self.velocity.is_finite()
            && (0.0..=1.0).contains(&self.velocity)
    }
}

/// Copied completion address from the actual native operation.
///
/// CLAP End has no usable sample timestamp or velocity. These fields preserve
/// its original input tuple; -1 is a P/C/K wildcard with a concrete Note ID.
/// A wildcard Note ID is standard-valid but unsupported by this first subset.
/// The parent derives operation/object/epoch origin from its retained command,
/// never from an event payload, and applies completions only after native return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioNoteEndEvent {
    /// Concrete nonnegative native ID; wildcard IDs are refused by this subset.
    pub native_note_id: i32,
    /// Original input index or -1, never an output-port index.
    pub port_index: i16,
    /// Original channel or -1.
    pub channel: i16,
    /// Original pitch or -1.
    pub key: i16,
    /// Raw native flags, without truncation of future bits.
    pub flags: u32,
}

impl AudioNoteEndEvent {
    /// Scalar subset check; native extent/type and actual origin are separate.
    pub(crate) fn valid(self, ports: usize) -> bool {
        self.native_note_id >= 0
            && (self.port_index == -1 || self.port_index >= 0 && (self.port_index as usize) < ports)
            && (-1..=15).contains(&self.channel)
            && (-1..=127).contains(&self.key)
            && self.flags & !3 == 0
    }
}
