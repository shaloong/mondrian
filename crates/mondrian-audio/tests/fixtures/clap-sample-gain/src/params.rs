// Adapted from Clack gain; see lib.rs and LICENSES/CLACK-MIT.txt.
//! Contains all types and implementations related to parameter management.

use crate::{GainPluginAudioProcessor, GainPluginMainThread};
use clack_extensions::params::*;
use clack_extensions::state::PluginStateImpl;
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

/// The unique identifier for the Volume parameter.
pub const PARAM_VOLUME_ID: ClapId = ClapId::new(1);

/// The default value of the volume parameter.
const DEFAULT_VOLUME: f32 = 1.0;

/// The single fixed native GUI gesture value; transport is not another value store.
const GUI_VOLUME: f32 = 0.75;
/// No native gesture is awaiting delivery.
const GUI_IDLE: u8 = 0;
/// The next undelivered event is Begin.
const GUI_BEGIN: u8 = 1;
/// The next undelivered event is Value.
const GUI_VALUE: u8 = 2;
/// The next undelivered event is End.
const GUI_END: u8 = 3;
/// A GUI publication or output callback currently owns the phase.
const GUI_BUSY: u8 = 4;

/// A struct that manages the parameters for our plugin.
///
/// For now, it only manages a single, `volume` parameter.
///
/// This struct will be used both by the [`GainPluginMainThread`] (which the host will use
/// to query the value of our parameters), and by the [`GainPluginAudioProcessor`], which will
/// actually modulate the audio samples.
pub struct GainParams {
    /// The current value of the volume parameter.
    volume: AtomicF32,
    /// Delivery phase only. One native gesture is retained until End is accepted.
    gui_phase: AtomicU8,
}

impl GainParams {
    /// Initializes the shared parameter value.
    pub fn new() -> Self {
        Self {
            volume: AtomicF32::new(DEFAULT_VOLUME),
            gui_phase: AtomicU8::new(GUI_IDLE),
        }
    }

    /// Start one native edit. A repeated G cannot overwrite an unfinished gesture.
    #[cfg(any(target_os = "windows", test))]
    pub fn begin_gui_gesture(&self) -> bool {
        if self
            .gui_phase
            .compare_exchange(GUI_IDLE, GUI_BUSY, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        self.set_volume(GUI_VOLUME);
        self.gui_phase.store(GUI_BEGIN, Ordering::Release);
        true
    }

    /// Whether a flush retry is still required; this does not mutate parameters.
    pub fn gui_events_pending(&self) -> bool {
        self.gui_phase.load(Ordering::Acquire) != GUI_IDLE
    }

    /// Send at most the three events of the one retained native gesture.
    ///
    /// Main/audio flush and process share this phase. Publication owns BUSY across
    /// each foreign try_push, so reentrant output cannot duplicate an event. Only
    /// accepted events advance; capacity refusal keeps the exact Value/End pending.
    /// The caller requests a main-thread retry if this returns true.
    pub fn flush_gui_events(&self, output: &mut OutputEvents) -> bool {
        for _ in 0..3 {
            let phase = self.gui_phase.load(Ordering::Acquire);
            if phase == GUI_IDLE {
                return false;
            }
            if phase == GUI_BUSY
                || self
                    .gui_phase
                    .compare_exchange(phase, GUI_BUSY, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
            {
                return true;
            }
            let result = match phase {
                GUI_BEGIN => output.try_push(ParamGestureBeginEvent::new(0, PARAM_VOLUME_ID)),
                GUI_VALUE => output.try_push(ParamValueEvent::new(
                    0,
                    PARAM_VOLUME_ID,
                    Pckn::match_all(),
                    f64::from(GUI_VOLUME),
                )),
                GUI_END => output.try_push(ParamGestureEndEvent::new(0, PARAM_VOLUME_ID)),
                _ => unreachable!("private GUI phase"),
            };
            let next = if result.is_err() {
                phase
            } else if phase == GUI_END {
                GUI_IDLE
            } else {
                phase + 1
            };
            self.gui_phase.store(next, Ordering::Release);
            if result.is_err() {
                return true;
            }
        }
        self.gui_events_pending()
    }

    /// Returns the current volume.
    #[inline]
    pub fn get_volume(&self) -> f32 {
        self.volume.load()
    }

    /// Sets a new value for the value parameter.
    /// The value is clamped, as it should only be in the `0..=1` range.
    #[inline]
    pub fn set_volume(&self, new_volume: f32) {
        let new_volume = new_volume.clamp(0., 1.);
        self.volume.store(new_volume)
    }

    /// Handles incoming events.
    ///
    /// If the given event is a matching parameter change event, the volume parameter will be
    /// updated accordingly.
    pub fn handle_event(&self, event: &UnknownEvent) {
        if let Some(CoreEventSpace::ParamValue(event)) = event.as_core_event()
            && event.param_id() == PARAM_VOLUME_ID
        {
            self.set_volume(event.value() as f32)
        }
    }
}

/// Implementation of the State extension.
///
/// Our state "serialization" is extremely simple and basic: we only have the value of the
/// volume parameter to store, so we just store its bytes (in little-endian) and call it a day.
impl PluginStateImpl for GainPluginMainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        let volume_param = self.shared.params.get_volume();

        output.write_all(&volume_param.to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut buf = [0; 4];
        input.read_exact(&mut buf)?;
        let volume_value = f32::from_le_bytes(buf);
        self.shared.params.set_volume(volume_value);
        Ok(())
    }
}

impl PluginMainThreadParams for GainPluginMainThread<'_> {
    fn count(&self) -> u32 {
        1
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
        if param_index != 0 {
            return;
        }
        info.set(&ParamInfo {
            id: 1.into(),
            flags: ParamInfoFlags::IS_AUTOMATABLE,
            cookie: Default::default(),
            name: b"Volume",
            module: b"",
            min_value: 0.0,
            max_value: 1.0,
            default_value: DEFAULT_VOLUME as f64,
        })
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        if param_id == 1 {
            Some(self.shared.params.get_volume() as f64)
        } else {
            None
        }
    }

    fn value_to_text(
        &self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        if param_id == 1 {
            write!(writer, "{0:.2} %", value * 100.0)
        } else {
            Err(std::fmt::Error)
        }
    }

    fn text_to_value(&self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let text = text.to_str().ok()?;
        if param_id == 1 {
            let text = text.strip_suffix('%').unwrap_or(text).trim();
            let percentage: f64 = text.parse().ok()?;

            Some(percentage / 100.0)
        } else {
            None
        }
    }

    fn flush(
        &self,
        input_parameter_changes: &InputEvents,
        output_parameter_changes: &mut OutputEvents,
    ) {
        for event in input_parameter_changes {
            self.shared.params.handle_event(event)
        }
        if self.shared.params.flush_gui_events(output_parameter_changes) {
            self.request_gui_flush();
        }
    }
}

impl PluginAudioProcessorParams for GainPluginAudioProcessor<'_> {
    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        output_parameter_changes: &mut OutputEvents,
    ) {
        for event in input_parameter_changes {
            self.shared.params.handle_event(event)
        }
        if self.shared.params.flush_gui_events(output_parameter_changes) {
            // request_flush is not legal on the audio thread. Retry on main.
            self.host.request_callback();
        }
    }
}

/// A small helper to atomically load and store an `f32` value.
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    /// Creates a new atomic `f32`.
    #[inline]
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    /// Stores the given `value` using the given `order`ing.
    #[inline]
    fn store(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed)
    }

    /// Loads the contained `value` using the given `order`ing.
    #[inline]
    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Deterministic transport controls; these do not open a native window.
#[cfg(test)]
mod tests {
    use super::*;
    use clack_plugin::events::io::{OutputEventBuffer, TryPushError};

    /// Small host output accepting a bounded number of actual typed events.
    struct BoundedOutput {
        /// Space available in this flush, replenished by the test host.
        remaining: usize,
        /// Accepted event kind, native ID and Value bits, in delivery order.
        events: Vec<(u8, u32, u64)>,
    }

    impl OutputEventBuffer for BoundedOutput {
        fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
            if self.remaining == 0 {
                return Err(TryPushError::new());
            }
            let fact = match event.as_core_event() {
                Some(CoreEventSpace::ParamGestureBegin(event)) => {
                    (GUI_BEGIN, event.param_id().unwrap().get(), 0)
                }
                Some(CoreEventSpace::ParamValue(event)) => (
                    GUI_VALUE,
                    event.param_id().unwrap().get(),
                    event.value().to_bits(),
                ),
                Some(CoreEventSpace::ParamGestureEnd(event)) => {
                    (GUI_END, event.param_id().unwrap().get(), 0)
                }
                _ => panic!("only the actual native parameter gesture is emitted"),
            };
            self.remaining -= 1;
            self.events.push(fact);
            Ok(())
        }
    }

    #[test]
    fn gui_partial_flush_preserves_order_and_undelivered_end() {
        let params = GainParams::new();
        assert_eq!(params.get_volume(), DEFAULT_VOLUME);
        let mut sink = BoundedOutput { remaining: 3, events: Vec::new() };
        assert!(!params.flush_gui_events(&mut OutputEvents::from_buffer(&mut sink)));
        assert!(sink.events.is_empty(), "headless path emits no new events");
        assert!(params.begin_gui_gesture());
        assert_eq!(params.get_volume(), GUI_VOLUME);
        for (capacity, accepted, pending) in [
            (0, 0, true),
            (1, 1, true),
            (1, 2, true),
            (0, 2, true),
            (1, 3, false),
        ] {
            assert!(
                !params.begin_gui_gesture(),
                "unfinished G cannot be overwritten"
            );
            sink.remaining = capacity;
            assert_eq!(
                params.flush_gui_events(&mut OutputEvents::from_buffer(&mut sink)),
                pending
            );
            assert_eq!(params.gui_events_pending(), pending);
            assert_eq!(sink.events.len(), accepted);
        }
        assert_eq!(
            sink.events,
            [
                (GUI_BEGIN, 1, 0),
                (GUI_VALUE, 1, 0.75_f64.to_bits()),
                (GUI_END, 1, 0),
            ]
        );
        sink.remaining = 3;
        assert!(!params.flush_gui_events(&mut OutputEvents::from_buffer(&mut sink)));
        assert_eq!(sink.events.len(), 3, "successful End is not replayed");
        assert!(
            params.begin_gui_gesture(),
            "a later independent G is allowed"
        );
    }

    /// Host callback attempting to reenter output and publish another G.
    struct ReentrantOutput<'a> {
        /// The same instance, not a copied queue or parameter model.
        params: &'a GainParams,
        /// Original accepted outer events.
        sink: BoundedOutput,
    }

    impl OutputEventBuffer for ReentrantOutput<'_> {
        fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
            assert!(!self.params.begin_gui_gesture());
            let mut nested = BoundedOutput { remaining: 3, events: Vec::new() };
            assert!(self.params.flush_gui_events(&mut OutputEvents::from_buffer(&mut nested)));
            assert!(
                nested.events.is_empty(),
                "in-flight output owns its one event"
            );
            self.sink.try_push(event)
        }
    }

    #[test]
    fn gui_reentrant_output_cannot_duplicate_or_replace_gesture() {
        let params = GainParams::new();
        assert!(params.begin_gui_gesture());
        let mut output = ReentrantOutput {
            params: &params,
            sink: BoundedOutput { remaining: 3, events: Vec::new() },
        };
        assert!(!params.flush_gui_events(&mut OutputEvents::from_buffer(&mut output)));
        assert_eq!(
            output.sink.events,
            [
                (GUI_BEGIN, 1, 0),
                (GUI_VALUE, 1, 0.75_f64.to_bits()),
                (GUI_END, 1, 0),
            ]
        );
        assert!(!params.gui_events_pending());
    }
}
