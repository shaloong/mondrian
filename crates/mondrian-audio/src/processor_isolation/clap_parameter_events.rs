//! Fixed admitted typed parameter and concrete On/Off owners for one native call.

use crate::{AudioNoteEvent, AudioNoteEventKind, AudioProcessorHostError};
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::io::InputEventBuffer;
use clack_host::events::{Event, UnknownEvent};
use clack_host::prelude::{InputEvents, Pckn};
use std::alloc::Layout;

const PARAMETER_PHASE: u8 = 0;
const OFF_PHASE: u8 = 1;
const ON_PHASE: u8 = 2;
const CLAP_PARAMETER_INPUT_EVENTS: usize = 1_048_576;

/// Explicit 12-byte representation; never charge an assumed usize ordinal.
#[derive(Clone, Copy)]
#[repr(C)]
struct InputOrdinal {
    stream_index: u32,
    stream_ordinal: u32,
    phase: u8,
    reserved: [u8; 3],
}
const _: () = assert!(size_of::<InputOrdinal>() == 12);

/// SDK owners retain actual concrete type/size, without an untyped byte arena.
enum OwnedNoteEvent {
    Off(NoteOffEvent),
    On(NoteOnEvent),
}

impl OwnedNoteEvent {
    fn as_unknown(&self) -> &UnknownEvent {
        match self {
            Self::Off(event) => event.as_unknown(),
            Self::On(event) => event.as_unknown(),
        }
    }

    fn time(&self) -> u32 {
        match self {
            Self::Off(event) => event.time(),
            Self::On(event) => event.time(),
        }
    }
}

/// Own both typed streams and one fixed ordinal list. No mutable SDK owner escapes.
pub(super) struct ClapInputEvents {
    events: Vec<ParamValueEvent>,
    notes: Vec<OwnedNoteEvent>,
    ordinals: Vec<InputOrdinal>,
    parameter_limit: usize,
    note_limit: usize,
}

// SAFETY: parameter admission rejects every nonnull native cookie. Notes contain
// only owned numeric SDK data, and private ordinals hold no borrowed references.
// This grants Send only to this specific owned carrier, never generic SDK events.
unsafe impl Send for ClapInputEvents {}

fn invalid(detail: &'static str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

fn owner_bytes<P, N, O>(
    parameters: usize,
    notes: usize,
    ordinals: usize,
) -> Result<usize, AudioProcessorHostError> {
    let p = Layout::array::<P>(parameters)
        .map_err(|_| invalid("CLAP typed parameter reservation overflowed"))?;
    let n =
        Layout::array::<N>(notes).map_err(|_| invalid("CLAP typed note reservation overflowed"))?;
    let o = Layout::array::<O>(ordinals)
        .map_err(|_| invalid("CLAP typed ordinal reservation overflowed"))?;
    p.size()
        .checked_add(n.size())
        .and_then(|bytes| bytes.checked_add(o.size()))
        .ok_or_else(|| invalid("CLAP typed input reservation overflowed"))
}

impl ClapInputEvents {
    /// The original parameter-only constructor explicitly admits zero notes.
    #[cfg(test)]
    pub(super) fn with_capacity(limit: usize) -> Self {
        Self::try_with_note_capacity(limit, 0)
            .expect("CLAP parameter input storage must be admitted before processing")
    }

    /// Reserve all concrete owners before activation, never inside DSP callbacks.
    pub(super) fn try_with_note_capacity(
        parameter_limit: usize,
        note_limit: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if parameter_limit > CLAP_PARAMETER_INPUT_EVENTS {
            return Err(invalid(
                "CLAP typed parameter input exceeds its worker bound",
            ));
        }
        let ordinal_limit = parameter_limit
            .checked_add(note_limit)
            .filter(|count| *count <= u32::MAX as usize)
            .ok_or_else(|| invalid("CLAP typed input count exceeds u32"))?;
        if note_limit > crate::AUDIO_NOTE_EVENTS_PER_BLOCK {
            return Err(invalid("CLAP typed note input exceeds its bounded subset"));
        }
        let mut events = Vec::new();
        let mut notes = Vec::new();
        let mut ordinals = Vec::new();
        events
            .try_reserve_exact(parameter_limit)
            .map_err(|_| invalid("CLAP typed parameter storage unavailable"))?;
        notes
            .try_reserve_exact(note_limit)
            .map_err(|_| invalid("CLAP typed note storage unavailable"))?;
        ordinals
            .try_reserve_exact(ordinal_limit)
            .map_err(|_| invalid("CLAP typed ordinal storage unavailable"))?;
        if events.capacity() > parameter_limit
            || notes.capacity() > note_limit
            || ordinals.capacity() > ordinal_limit
        {
            return Err(invalid("CLAP typed input owner exceeds admitted capacity"));
        }
        let owner = Self {
            events,
            notes,
            ordinals,
            parameter_limit,
            note_limit,
        };
        if owner.allocated_bytes()? > Self::reservation_bytes(parameter_limit, note_limit)? {
            return Err(invalid("CLAP typed input allocation exceeds reservation"));
        }
        Ok(owner)
    }

    pub(super) fn reservation_bytes(
        parameters: usize,
        notes: usize,
    ) -> Result<usize, AudioProcessorHostError> {
        let ordinals = parameters
            .checked_add(notes)
            .ok_or_else(|| invalid("CLAP typed input ordinal count overflowed"))?;
        owner_bytes::<ParamValueEvent, OwnedNoteEvent, InputOrdinal>(parameters, notes, ordinals)
    }

    pub(super) fn allocated_bytes(&self) -> Result<usize, AudioProcessorHostError> {
        owner_bytes::<ParamValueEvent, OwnedNoteEvent, InputOrdinal>(
            self.events.capacity(),
            self.notes.capacity(),
            self.ordinals.capacity(),
        )
    }

    /// Clears contents without releasing or growing any admitted owner.
    pub(super) fn clear(&mut self) {
        self.events.clear();
        self.notes.clear();
        self.ordinals.clear();
    }

    /// Parameter insertion order remains its original flattened parameter stream.
    pub(super) fn try_push(&mut self, event: ParamValueEvent) -> bool {
        if !event.cookie().as_raw().is_null()
            || self.events.len() >= self.parameter_limit
            || self.ordinals.len() >= self.ordinals.capacity()
        {
            return false;
        }
        let ordinal = self.events.len() as u32;
        self.ordinals.push(InputOrdinal {
            stream_index: ordinal,
            stream_ordinal: ordinal,
            phase: PARAMETER_PHASE,
            reserved: [0; 3],
        });
        self.events.push(event);
        true
    }

    /// Scalar/extent rejection precedes every mutation. Native On velocity zero
    /// remains On; no wildcard input or inferred termination is manufactured.
    pub(super) fn try_push_note(
        &mut self,
        event: AudioNoteEvent,
        frames: usize,
        ports: usize,
    ) -> bool {
        if ports == 0
            || ports > crate::AUDIO_NOTE_INPUT_PORTS
            || !event.valid(frames, ports)
            || self.notes.len() >= self.note_limit
            || self.ordinals.len() >= self.ordinals.capacity()
        {
            return false;
        }
        let pckn = Pckn::new(
            event.port_index,
            u16::from(event.channel),
            u16::from(event.key),
            event.native_note_id as u32,
        );
        let (note, phase) = match event.kind {
            AudioNoteEventKind::Off => (
                OwnedNoteEvent::Off(NoteOffEvent::new(event.sample_offset, pckn, event.velocity)),
                OFF_PHASE,
            ),
            AudioNoteEventKind::On => (
                OwnedNoteEvent::On(NoteOnEvent::new(event.sample_offset, pckn, event.velocity)),
                ON_PHASE,
            ),
        };
        let ordinal = self.notes.len() as u32;
        self.ordinals.push(InputOrdinal {
            stream_index: ordinal,
            stream_ordinal: ordinal,
            phase,
            reserved: [0; 3],
        });
        self.notes.push(note);
        true
    }

    /// In-place unstable sort has a complete unique key, preserving source order
    /// inside each phase without auxiliary allocation: offset, Param/Off/On, ordinal.
    pub(super) fn sort(&mut self) {
        let events = &self.events;
        let notes = &self.notes;
        self.ordinals.sort_unstable_by_key(|ordinal| {
            let time = if ordinal.phase == PARAMETER_PHASE {
                events[ordinal.stream_index as usize].time()
            } else {
                notes[ordinal.stream_index as usize].time()
            };
            (time, ordinal.phase, ordinal.stream_ordinal)
        });
    }

    pub(super) fn as_input(&self) -> InputEvents<'_> {
        InputEvents::from_buffer(self)
    }
}

impl InputEventBuffer for ClapInputEvents {
    fn len(&self) -> u32 {
        self.ordinals.len() as u32
    }

    fn get(&self, index: u32) -> Option<&UnknownEvent> {
        let ordinal = self.ordinals.get(index as usize)?;
        if ordinal.phase == PARAMETER_PHASE {
            self.events.as_slice().get(ordinal.stream_index as usize).map(Event::as_unknown)
        } else {
            self.notes.get(ordinal.stream_index as usize).map(OwnedNoteEvent::as_unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clack_host::prelude::{ClapId, Pckn};

    #[test]
    fn typed_parameter_storage_preserves_equal_time_order_and_exact_capacity() {
        let mut storage = ClapInputEvents::with_capacity(4);
        let event_capacity = storage.events.capacity();
        let ordinal_capacity = storage.ordinals.capacity();
        for (time, id, value) in [(3, 17, 0.5), (0, 17, 0.25), (0, 3, 0.75), (3, 3, 1.0)] {
            assert!(storage.try_push(ParamValueEvent::new(
                time,
                ClapId::new(id),
                Pckn::match_all(),
                value,
            )));
        }
        assert!(!storage.try_push(ParamValueEvent::new(
            0,
            ClapId::new(17),
            Pckn::match_all(),
            0.0,
        )));
        storage.sort();
        let observed: Vec<_> = storage
            .as_input()
            .iter()
            .map(|event| {
                let value = event.as_event::<ParamValueEvent>().expect("typed SDK event");
                (
                    value.time(),
                    value.param_id().expect("valid ID").get(),
                    value.value(),
                )
            })
            .collect();
        assert_eq!(
            observed,
            [(0, 17, 0.25), (0, 3, 0.75), (3, 17, 0.5), (3, 3, 1.0)]
        );
        assert!(storage.as_input().get(4).is_none());
        assert_eq!(storage.events.capacity(), event_capacity);
        assert_eq!(storage.ordinals.capacity(), ordinal_capacity);
        storage.clear();
        assert!(storage.as_input().is_empty());
        assert_eq!(storage.events.capacity(), event_capacity);
        assert_eq!(storage.ordinals.capacity(), ordinal_capacity);
        assert_eq!(
            event_capacity * size_of::<ParamValueEvent>()
                + ordinal_capacity * size_of::<InputOrdinal>(),
            4 * (size_of::<ParamValueEvent>() + size_of::<InputOrdinal>()),
        );
        let mut empty = ClapInputEvents::with_capacity(0);
        assert!(!empty.try_push(ParamValueEvent::new(
            0,
            ClapId::new(17),
            Pckn::match_all(),
            0.0,
        )));
        assert!(empty.as_input().is_empty());
    }
}

#[cfg(test)]
mod cross_thread_contract_tests {
    use super::*;
    use clack_host::prelude::{ClapId, Pckn};
    use clack_host::utils::Cookie;
    #[test]
    fn null_cookie_typed_carrier_moves_only_owned_events_and_refuses_native_cookie() {
        fn require_send<T: Send>() {}
        require_send::<ClapInputEvents>();
        let mut storage = ClapInputEvents::with_capacity(2);
        let mut borrowed_cookie = 3_u8;
        let event = ParamValueEvent::new(0, ClapId::new(17), Pckn::match_all(), 0.25);
        // SAFETY: this never leaves the test or reaches a plugin. The specific
        // carrier must reject this live nonnull cookie before owning the event.
        let borrowed = unsafe {
            event.with_cookie(Cookie::from_raw((&mut borrowed_cookie as *mut u8).cast()))
        };
        assert!(!storage.try_push(borrowed));
        assert!(storage.as_input().is_empty());
        assert!(storage.try_push(event));
        storage.sort();
        std::thread::spawn(move || {
            let input = storage.as_input();
            let event = input
                .get(0)
                .expect("owned event")
                .as_event::<ParamValueEvent>()
                .expect("SDK value");
            assert!(event.cookie().as_raw().is_null());
            assert_eq!(event.value(), 0.25);
        })
        .join()
        .expect("typed carrier actual move");
        assert_eq!(borrowed_cookie, 3);
    }
}

#[cfg(test)]
mod note_input_contract_tests {
    use super::*;
    use clack_host::prelude::ClapId;

    fn note(time: u32, kind: AudioNoteEventKind, id: i32, velocity: f64) -> AudioNoteEvent {
        AudioNoteEvent {
            sample_offset: time,
            kind,
            port_index: 1,
            channel: 3,
            key: 60,
            native_note_id: id,
            velocity,
        }
    }

    #[test]
    fn typed_mixed_input_orders_offset_phase_original_stream_and_keeps_zero_on() {
        let mut owner = ClapInputEvents::try_with_note_capacity(3, 4).expect("fixed owners");
        let bytes = owner.allocated_bytes().expect("actual layouts");
        assert_eq!(
            bytes,
            ClapInputEvents::reservation_bytes(3, 4).expect("reservation")
        );
        assert_eq!(size_of::<InputOrdinal>(), 12);
        assert!(owner.try_push_note(note(2, AudioNoteEventKind::On, 30, 0.0), 4, 2));
        assert!(owner.try_push_note(note(2, AudioNoteEventKind::Off, 31, 0.25), 4, 2));
        assert!(owner.try_push(ParamValueEvent::new(
            2,
            ClapId::new(17),
            Pckn::match_all(),
            0.25
        )));
        assert!(owner.try_push_note(note(2, AudioNoteEventKind::On, 32, 0.5), 4, 2));
        assert!(owner.try_push(ParamValueEvent::new(
            0,
            ClapId::new(17),
            Pckn::match_all(),
            0.75
        )));
        assert!(owner.try_push_note(note(2, AudioNoteEventKind::Off, 33, 0.75), 4, 2));
        assert!(owner.try_push(ParamValueEvent::new(
            2,
            ClapId::new(3),
            Pckn::match_all(),
            1.0
        )));
        assert!(!owner.try_push_note(note(0, AudioNoteEventKind::On, 34, 1.0), 4, 2));
        assert!(!owner.try_push(ParamValueEvent::new(
            0,
            ClapId::new(17),
            Pckn::match_all(),
            0.0
        )));
        owner.sort();
        let observed: Vec<_> = owner
            .as_input()
            .iter()
            .map(|event| {
                if let Some(parameter) = event.as_event::<ParamValueEvent>() {
                    (
                        parameter.time(),
                        0,
                        parameter.param_id().expect("ID").get() as i32,
                        parameter.value(),
                    )
                } else if let Some(off) = event.as_event::<NoteOffEvent>() {
                    let raw = off.as_raw();
                    assert_eq!((raw.port_index, raw.channel, raw.key), (1, 3, 60));
                    (raw.header.time, 1, raw.note_id, raw.velocity)
                } else {
                    let on = event.as_event::<NoteOnEvent>().expect("actual On owner");
                    let raw = on.as_raw();
                    assert_eq!((raw.port_index, raw.channel, raw.key), (1, 3, 60));
                    (raw.header.time, 2, raw.note_id, raw.velocity)
                }
            })
            .collect();
        assert_eq!(
            observed,
            [
                (0, 0, 17, 0.75),
                (2, 0, 17, 0.25),
                (2, 0, 3, 1.0),
                (2, 1, 31, 0.25),
                (2, 1, 33, 0.75),
                (2, 2, 30, 0.0),
                (2, 2, 32, 0.5),
            ]
        );
        assert!(owner.as_input().get(7).is_none());
        assert_eq!(
            owner.allocated_bytes().expect("no growth on full/sort"),
            bytes
        );
        owner.clear();
        assert!(owner.as_input().is_empty());
        assert_eq!(
            owner.allocated_bytes().expect("empty owners still charged"),
            bytes
        );
    }

    #[test]
    fn typed_note_input_refuses_bad_scalars_and_zero_note_default_without_mutation() {
        let mut owner = ClapInputEvents::try_with_note_capacity(0, 1).expect("fixed notes");
        let bytes = owner.allocated_bytes().expect("owner bytes");
        let good = note(0, AudioNoteEventKind::On, 10, 0.0);
        for bad in [
            AudioNoteEvent { sample_offset: 4, ..good },
            AudioNoteEvent { port_index: 2, ..good },
            AudioNoteEvent { channel: 16, ..good },
            AudioNoteEvent { key: 128, ..good },
            AudioNoteEvent { native_note_id: -1, ..good },
            AudioNoteEvent { velocity: f64::NAN, ..good },
            AudioNoteEvent { velocity: -0.1, ..good },
        ] {
            assert!(!owner.try_push_note(bad, 4, 2));
            assert!(owner.as_input().is_empty());
        }
        assert!(!owner.try_push_note(good, 4, 0));
        assert!(owner.try_push_note(good, 4, 2));
        assert_eq!(
            owner.allocated_bytes().expect("no rejected-input growth"),
            bytes
        );
        let mut parameter_only = ClapInputEvents::with_capacity(1);
        assert!(!parameter_only.try_push_note(good, 4, 2));
        assert!(parameter_only.as_input().is_empty());
        assert!(
            ClapInputEvents::try_with_note_capacity(0, crate::AUDIO_NOTE_EVENTS_PER_BLOCK + 1)
                .is_err()
        );
        assert!(
            ClapInputEvents::try_with_note_capacity(CLAP_PARAMETER_INPUT_EVENTS + 1, 0).is_err()
        );
        assert!(ClapInputEvents::reservation_bytes(usize::MAX, 1).is_err());
    }
}
