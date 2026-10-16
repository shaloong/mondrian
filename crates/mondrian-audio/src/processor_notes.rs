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
    /// Admitted concrete input channels, independently of raw format declarations.
    pub channel_count: u8,
}

/// Immutable completion rule for one exact prepared native Note contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioNoteCompletionPolicy {
    /// Capacity returns only after an actual matching native End callback.
    #[default]
    NativeEnd,
    /// Pairing records retire after the actual Off block succeeds. This makes
    /// no claim that a native DSP voice or release envelope has ended.
    SuccessfulOffBlock,
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
    /// NativeEnd pressed/releasing voices; zero for SuccessfulOffBlock.
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
    /// Required immutable completion declaration; never inferred from a format.
    pub completion: AudioNoteCompletionPolicy,
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
            return if self.completion == AudioNoteCompletionPolicy::NativeEnd
                && self.limits == AudioNoteExecutionLimits::default()
            {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        let limits = self.limits;
        if self.ports.len() > AUDIO_NOTE_INPUT_PORTS
            || !(1..=AUDIO_NOTE_EVENTS_PER_BLOCK).contains(&limits.input_events)
            || !(1..=AUDIO_NOTE_RECORDS).contains(&limits.retained_records)
            || match self.completion {
                AudioNoteCompletionPolicy::NativeEnd => {
                    !(1..=AUDIO_NOTE_ENDS_PER_BLOCK).contains(&limits.output_ends)
                        || !(1..=limits.retained_records).contains(&limits.live_voices)
                }
                AudioNoteCompletionPolicy::SuccessfulOffBlock => {
                    limits.output_ends != 0 || limits.live_voices != 0
                }
            }
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
                || !(1..=16).contains(&port.channel_count)
                || !keys.insert(port.port_key.as_str())
                || !ids.insert(port.native_id)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub(crate) fn accepts_event(&self, event: AudioNoteEvent, frames: usize) -> bool {
        event.valid(frames, self.ports.len())
            && self
                .ports
                .get(usize::from(event.port_index))
                .is_some_and(|port| event.channel < port.channel_count)
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

#[cfg(test)]
mod completion_contract_tests {
    use super::*;

    fn contract() -> AudioProcessorNoteInputContract {
        AudioProcessorNoteInputContract {
            completion: AudioNoteCompletionPolicy::SuccessfulOffBlock,
            ports: vec![AudioProcessorNoteInputPort {
                port_key: "event.main".to_owned(),
                native_id: 0,
                native_index: 0,
                channel_count: 1,
            }],
            limits: AudioNoteExecutionLimits {
                input_events: 2,
                output_ends: 0,
                retained_records: 1,
                live_voices: 0,
            },
        }
    }

    #[test]
    fn completion_and_channel_are_required_and_policy_limits_are_exact() {
        let contract = contract();
        assert_eq!(contract.validate(), Ok(()));
        let value = serde_json::to_value(&contract).unwrap();
        for key in ["completion", "ports"] {
            let mut invalid = value.clone();
            invalid.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<AudioProcessorNoteInputContract>(invalid).is_err());
        }
        let mut missing_channel = value.clone();
        missing_channel["ports"][0].as_object_mut().unwrap().remove("channel_count");
        assert!(
            serde_json::from_value::<AudioProcessorNoteInputContract>(missing_channel).is_err()
        );
        let mut unknown = value;
        unknown["completion"] = serde_json::json!("future_completion");
        assert!(serde_json::from_value::<AudioProcessorNoteInputContract>(unknown).is_err());
        for width in [0, 17] {
            let mut invalid = contract.clone();
            invalid.ports[0].channel_count = width;
            assert!(invalid.validate().is_err());
        }
        for voices in [false, true] {
            let mut invalid = contract.clone();
            if voices {
                invalid.limits.live_voices = 1;
            } else {
                invalid.limits.output_ends = 1;
            }
            assert!(invalid.validate().is_err());
        }
        let mut native = contract.clone();
        native.completion = AudioNoteCompletionPolicy::NativeEnd;
        assert!(native.validate().is_err());
        native.limits.output_ends = 1;
        native.limits.live_voices = 1;
        assert_eq!(native.validate(), Ok(()));
        let mut empty = AudioProcessorNoteInputContract::default();
        assert_eq!(empty.validate(), Ok(()));
        empty.completion = AudioNoteCompletionPolicy::SuccessfulOffBlock;
        assert!(empty.validate().is_err());
        let event = AudioNoteEvent {
            sample_offset: 0,
            kind: AudioNoteEventKind::On,
            port_index: 0,
            channel: 0,
            key: 60,
            native_note_id: 0,
            velocity: 1.0,
        };
        assert!(contract.accepts_event(event, 1));
        assert!(!contract.accepts_event(AudioNoteEvent { channel: 1, ..event }, 1));
    }
}
