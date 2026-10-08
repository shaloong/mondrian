//! Fixed admitted storage for the worker's current parameter-value event stream.

use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::InputEventBuffer;
use clack_host::events::{Event, UnknownEvent};
use clack_host::prelude::InputEvents;

/// Own typed SDK events and one fixed ordinal list, preserving equal-time order.
///
/// The stream currently contains parameter values only. Later heterogeneous
/// event admission must retain explicit typed ownership and a bounded lifetime;
/// it must not reinterpret this storage as a generic byte arena.
pub(super) struct ClapParameterEvents {
    events: Vec<ParamValueEvent>,
    ordinals: Vec<usize>,
    limit: usize,
}

impl ClapParameterEvents {
    /// Reserve both owners before processing; `limit` is the admitted event count.
    pub(super) fn with_capacity(limit: usize) -> Self {
        Self {
            events: Vec::with_capacity(limit),
            ordinals: Vec::with_capacity(limit),
            limit,
        }
    }

    /// Clear only the current contents, retaining both allocated capacities.
    pub(super) fn clear(&mut self) {
        self.events.clear();
        self.ordinals.clear();
    }

    /// Append within the exact admission limit without growing either owner.
    pub(super) fn try_push(&mut self, event: ParamValueEvent) -> bool {
        if self.events.len() >= self.limit {
            return false;
        }
        self.ordinals.push(self.events.len());
        self.events.push(event);
        true
    }

    /// Sort fixed ordinals by time and original insertion position, without scratch.
    pub(super) fn sort(&mut self) {
        let events = &self.events;
        self.ordinals.sort_unstable_by_key(|&ordinal| (events[ordinal].time(), ordinal));
    }

    /// Borrow SDK's standard input adapter over the ordered, typed event owners.
    pub(super) fn as_input(&self) -> InputEvents<'_> {
        InputEvents::from_buffer(self)
    }
}

impl InputEventBuffer for ClapParameterEvents {
    fn len(&self) -> u32 {
        // The worker admits at most 1,048,576 events before constructing storage.
        self.ordinals.len() as u32
    }

    fn get(&self, index: u32) -> Option<&UnknownEvent> {
        let ordinal = *self.ordinals.get(index as usize)?;
        self.events.as_slice().get(ordinal).map(|event| event.as_unknown())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clack_host::prelude::{ClapId, Pckn};

    #[test]
    fn typed_parameter_storage_preserves_equal_time_order_and_exact_capacity() {
        let mut storage = ClapParameterEvents::with_capacity(4);
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
            event_capacity * size_of::<ParamValueEvent>() + ordinal_capacity * size_of::<usize>(),
            4 * (size_of::<ParamValueEvent>() + size_of::<usize>()),
        );
        let mut empty = ClapParameterEvents::with_capacity(0);
        assert!(!empty.try_push(ParamValueEvent::new(
            0,
            ClapId::new(17),
            Pckn::match_all(),
            0.0,
        )));
        assert!(empty.as_input().is_empty());
    }
}
