//! Owned SDK bootstrap plus counted COM interfaces; no DSP/third-party qualification.
use super::*;
use crate::midi::PluginEventData;
use crate::{PluginInterface as InterfaceKind, PluginInterfaceSupport};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vst3::Class;

#[derive(Default)]
struct Calls {
    values: [AtomicUsize; 9],
    mapped_program: AtomicBool,
    drops: AtomicUsize,
}
impl Calls {
    fn hit(&self, kind: usize) {
        self.values[kind].fetch_add(1, Ordering::Relaxed);
    }
    fn snapshot(&self) -> [usize; 9] {
        std::array::from_fn(|i| self.values[i].load(Ordering::Relaxed))
    }
}
struct CountedNative {
    calls: Arc<Calls>,
}
impl Class for CountedNative {
    type Interfaces = (
        IComponent,
        IEditController,
        IMidiMapping,
        IUnitInfo,
        IProgramListData,
        IUnitData,
        INoteExpressionController,
    );
}
impl Drop for CountedNative {
    fn drop(&mut self) {
        self.calls.drops.fetch_add(1, Ordering::Relaxed);
    }
}
impl IPluginBaseTrait for CountedNative {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        self.calls.hit(8);
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        self.calls.hit(8);
        kResultOk
    }
}
impl IComponentTrait for CountedNative {
    unsafe fn getControllerClassId(&self, arg0: *mut TUID) -> tresult {
        let _ = arg0;
        self.calls.hit(0);
        kNotImplemented
    }
    unsafe fn setIoMode(&self, arg0: IoMode) -> tresult {
        let _ = arg0;
        self.calls.hit(0);
        kNotImplemented
    }
    // SDK enum constants use signed storage on Windows and unsigned elsewhere.
    #[allow(clippy::unnecessary_cast)]
    unsafe fn getBusCount(&self, arg0: MediaType, arg1: BusDirection) -> int32 {
        let _ = (arg0, arg1);
        self.calls.hit(0);
        if arg0 == kEvent as i32 && arg1 == kInput as i32 {
            1
        } else {
            0
        }
    }
    unsafe fn getBusInfo(
        &self,
        arg0: MediaType,
        arg1: BusDirection,
        arg2: int32,
        arg3: *mut BusInfo,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3);
        self.calls.hit(0);
        kNotImplemented
    }
    unsafe fn getRoutingInfo(&self, arg0: *mut RoutingInfo, arg1: *mut RoutingInfo) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(0);
        kNotImplemented
    }
    unsafe fn activateBus(
        &self,
        arg0: MediaType,
        arg1: BusDirection,
        arg2: int32,
        arg3: TBool,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3);
        self.calls.hit(0);
        kNotImplemented
    }
    unsafe fn setActive(&self, arg0: TBool) -> tresult {
        let _ = arg0;
        self.calls.hit(0);
        kNotImplemented
    }
    unsafe fn setState(&self, arg0: *mut IBStream) -> tresult {
        let _ = arg0;
        self.calls.hit(0);
        kNotImplemented
    }
    unsafe fn getState(&self, arg0: *mut IBStream) -> tresult {
        let _ = arg0;
        self.calls.hit(0);
        kNotImplemented
    }
}
impl IEditControllerTrait for CountedNative {
    unsafe fn setComponentState(&self, arg0: *mut IBStream) -> tresult {
        let _ = arg0;
        self.calls.hit(3);
        kNotImplemented
    }
    unsafe fn setState(&self, arg0: *mut IBStream) -> tresult {
        let _ = arg0;
        self.calls.hit(3);
        kNotImplemented
    }
    unsafe fn getState(&self, arg0: *mut IBStream) -> tresult {
        let _ = arg0;
        self.calls.hit(3);
        kNotImplemented
    }
    unsafe fn getParameterCount(&self) -> int32 {
        self.calls.hit(3);
        1
    }
    unsafe fn getParameterInfo(&self, arg0: int32, arg1: *mut ParameterInfo) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(3);
        if arg0 != 0 || arg1.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the host supplies one valid fixed SDK output slot.
        unsafe {
            *arg1 = std::mem::zeroed();
            (*arg1).id = 42;
            (*arg1).unitId = 0;
            (*arg1).stepCount = 3;
            (*arg1).flags = ParameterInfo_::ParameterFlags_::kIsProgramChange;
        }
        kResultOk
    }
    unsafe fn getParamStringByValue(
        &self,
        arg0: ParamID,
        arg1: ParamValue,
        arg2: *mut String128,
    ) -> tresult {
        let _ = (arg0, arg1, arg2);
        self.calls.hit(3);
        kNotImplemented
    }
    unsafe fn getParamValueByString(
        &self,
        arg0: ParamID,
        arg1: *mut TChar,
        arg2: *mut ParamValue,
    ) -> tresult {
        let _ = (arg0, arg1, arg2);
        self.calls.hit(3);
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, arg0: ParamID, arg1: ParamValue) -> ParamValue {
        let _ = (arg0, arg1);
        self.calls.hit(3);
        arg1
    }
    unsafe fn plainParamToNormalized(&self, arg0: ParamID, arg1: ParamValue) -> ParamValue {
        let _ = (arg0, arg1);
        self.calls.hit(3);
        arg1
    }
    unsafe fn getParamNormalized(&self, arg0: ParamID) -> ParamValue {
        let _ = arg0;
        self.calls.hit(3);
        0.5
    }
    unsafe fn setParamNormalized(&self, arg0: ParamID, arg1: ParamValue) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(7);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, arg0: *mut IComponentHandler) -> tresult {
        let _ = arg0;
        self.calls.hit(3);
        kResultOk
    }
    unsafe fn createView(&self, arg0: FIDString) -> *mut IPlugView {
        let _ = arg0;
        self.calls.hit(3);
        std::ptr::null_mut()
    }
}
impl IMidiMappingTrait for CountedNative {
    unsafe fn getMidiControllerAssignment(
        &self,
        arg0: int32,
        arg1: int16,
        arg2: CtrlNumber,
        arg3: *mut ParamID,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3);
        self.calls.hit(1);
        let id = match arg2 as u16 {
            130 if self.calls.mapped_program.load(Ordering::Relaxed) => 41,
            10 | 120 | 121 | 123 | 128 | 129 => 40,
            _ => return kResultFalse,
        };
        if arg3.is_null() {
            return kInvalidArgument;
        }
        unsafe {
            *arg3 = id;
        }
        kResultOk
    }
}
impl IUnitInfoTrait for CountedNative {
    unsafe fn getUnitCount(&self) -> int32 {
        self.calls.hit(2);
        1
    }
    unsafe fn getUnitInfo(&self, arg0: int32, arg1: *mut UnitInfo) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(2);
        if arg0 != 0 || arg1.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the host supplies one valid fixed SDK output slot.
        unsafe {
            *arg1 = std::mem::zeroed();
            (*arg1).id = 0;
            (*arg1).parentUnitId = -1;
            (*arg1).programListId = 11;
        }
        kResultOk
    }
    unsafe fn getProgramListCount(&self) -> int32 {
        self.calls.hit(2);
        1
    }
    unsafe fn getProgramListInfo(&self, arg0: int32, arg1: *mut ProgramListInfo) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(2);
        if arg0 != 0 || arg1.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the host supplies one valid fixed SDK output slot.
        unsafe {
            *arg1 = std::mem::zeroed();
            (*arg1).id = 11;
            (*arg1).programCount = 4;
        }
        kResultOk
    }
    unsafe fn getProgramName(
        &self,
        arg0: ProgramListID,
        arg1: int32,
        arg2: *mut String128,
    ) -> tresult {
        let _ = (arg0, arg1, arg2);
        self.calls.hit(2);
        if arg2.is_null() {
            return kInvalidArgument;
        }
        unsafe {
            *arg2 = [0; 128];
        }
        kResultOk
    }
    unsafe fn getProgramInfo(
        &self,
        arg0: ProgramListID,
        arg1: int32,
        arg2: vst3::Steinberg::Vst::CString,
        arg3: *mut String128,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3);
        self.calls.hit(2);
        kNotImplemented
    }
    unsafe fn hasProgramPitchNames(&self, arg0: ProgramListID, arg1: int32) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(2);
        kResultFalse
    }
    unsafe fn getProgramPitchName(
        &self,
        arg0: ProgramListID,
        arg1: int32,
        arg2: int16,
        arg3: *mut String128,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3);
        self.calls.hit(2);
        kNotImplemented
    }
    unsafe fn getSelectedUnit(&self) -> UnitID {
        self.calls.hit(2);
        0
    }
    unsafe fn selectUnit(&self, arg0: UnitID) -> tresult {
        let _ = arg0;
        self.calls.hit(2);
        kResultOk
    }
    unsafe fn getUnitByBus(
        &self,
        arg0: MediaType,
        arg1: BusDirection,
        arg2: int32,
        arg3: int32,
        arg4: *mut UnitID,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3, arg4);
        self.calls.hit(2);
        kNotImplemented
    }
    unsafe fn setUnitProgramData(&self, arg0: int32, arg1: int32, arg2: *mut IBStream) -> tresult {
        let _ = (arg0, arg1, arg2);
        self.calls.hit(2);
        kNotImplemented
    }
}
impl IProgramListDataTrait for CountedNative {
    unsafe fn programDataSupported(&self, arg0: ProgramListID) -> tresult {
        let _ = arg0;
        self.calls.hit(4);
        kResultOk
    }
    unsafe fn getProgramData(
        &self,
        arg0: ProgramListID,
        arg1: int32,
        arg2: *mut IBStream,
    ) -> tresult {
        let _ = (arg0, arg1, arg2);
        self.calls.hit(4);
        kResultOk
    }
    unsafe fn setProgramData(
        &self,
        arg0: ProgramListID,
        arg1: int32,
        arg2: *mut IBStream,
    ) -> tresult {
        let _ = (arg0, arg1, arg2);
        self.calls.hit(4);
        kResultOk
    }
}
impl IUnitDataTrait for CountedNative {
    unsafe fn unitDataSupported(&self, arg0: UnitID) -> tresult {
        let _ = arg0;
        self.calls.hit(5);
        kResultOk
    }
    unsafe fn getUnitData(&self, arg0: UnitID, arg1: *mut IBStream) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(5);
        kResultOk
    }
    unsafe fn setUnitData(&self, arg0: UnitID, arg1: *mut IBStream) -> tresult {
        let _ = (arg0, arg1);
        self.calls.hit(5);
        kResultOk
    }
}
impl INoteExpressionControllerTrait for CountedNative {
    unsafe fn getNoteExpressionCount(&self, arg0: int32, arg1: int16) -> int32 {
        let _ = (arg0, arg1);
        self.calls.hit(6);
        1
    }
    unsafe fn getNoteExpressionInfo(
        &self,
        arg0: int32,
        arg1: int16,
        arg2: int32,
        arg3: *mut NoteExpressionTypeInfo,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3);
        self.calls.hit(6);
        if arg2 != 0 || arg3.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the host supplies one valid fixed SDK output slot.
        unsafe {
            *arg3 = std::mem::zeroed();
            (*arg3).typeId = 0;
            (*arg3).valueDesc.maximum = 1.0;
        }
        kResultOk
    }
    unsafe fn getNoteExpressionStringByValue(
        &self,
        arg0: int32,
        arg1: int16,
        arg2: NoteExpressionTypeID,
        arg3: NoteExpressionValue,
        arg4: *mut String128,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3, arg4);
        self.calls.hit(6);
        kNotImplemented
    }
    unsafe fn getNoteExpressionValueByString(
        &self,
        arg0: int32,
        arg1: int16,
        arg2: NoteExpressionTypeID,
        arg3: *const TChar,
        arg4: *mut NoteExpressionValue,
    ) -> tresult {
        let _ = (arg0, arg1, arg2, arg3, arg4);
        self.calls.hit(6);
        kNotImplemented
    }
}

/// Restore both original native interfaces before PluginImpl's normal Drop,
/// including unwinding. The processor, connection and module are never replaced.
struct Probe {
    native: PluginImpl,
    component: Option<ComPtr<IComponent>>,
    controller: Option<ComPtr<IEditController>>,
    calls: Arc<Calls>,
}
impl Probe {
    fn new(policy: PluginInterfaceSupport) -> Self {
        let path = std::env::var_os("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN")
            .expect("owned existing SDK fixture DLL");
        let path = std::path::PathBuf::from(path);
        assert!(path.is_absolute());
        let native =
            PluginImpl::load_with_class(&path, Some("61921CF43A2B4A68AE5AE08241D85E22"), policy)
                .expect("actual SDK component/module bootstrap");
        assert!(native.controller.is_some());
        let calls = Arc::new(Calls::default());
        calls.mapped_program.store(true, Ordering::Relaxed);
        let counted = ComWrapper::new(CountedNative {
            calls: calls.clone(),
        });
        let component = counted.to_com_ptr::<IComponent>().unwrap();
        let controller = counted.to_com_ptr::<IEditController>().unwrap();
        let mut probe = Self {
            native,
            component: None,
            controller: None,
            calls,
        };
        probe.component = Some(std::mem::replace(&mut probe.native.component, component));
        probe.controller = probe.native.controller.replace(controller);
        probe.native.refresh_midi_mapping_cache();
        probe.native.refresh_program_change_cache();
        probe
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        // No fallible operation or native callback before both restores.
        let counted_component = self
            .component
            .take()
            .map(|original| std::mem::replace(&mut self.native.component, original));
        let counted_controller = self
            .controller
            .take()
            .map(|original| self.native.controller.replace(original));
        drop(counted_controller);
        drop(counted_component);
    }
}
fn refused<T>(result: Result<T>, expected: InterfaceKind) {
    assert!(matches!(result,Err(Error::InterfaceNotConsumed(actual)) if actual==expected));
}
fn program() -> MidiEvent {
    MidiEvent::ProgramChange {
        channel: MidiChannel::Ch1,
        program: 2,
    }
}
fn mapped_events() -> [MidiEvent; 3] {
    [
        MidiEvent::ControlChange {
            channel: MidiChannel::Ch1,
            controller: 10,
            value: 64,
        },
        MidiEvent::PitchBend {
            channel: MidiChannel::Ch1,
            value: 8192,
        },
        MidiEvent::ChannelAftertouch {
            channel: MidiChannel::Ch1,
            pressure: 64,
        },
    ]
}

fn generic_note_expressions() -> [PluginEvent; 3] {
    [
        PluginEventData::NoteExpressionValue {
            type_id: 0,
            note_id: 1,
            value: 0.5,
        },
        PluginEventData::NoteExpressionText {
            type_id: 0,
            note_id: 1,
            text: vec![65, 66],
        },
        PluginEventData::NoteExpressionIntValue {
            type_id: 0,
            note_id: 1,
            value: 123,
        },
    ]
    .map(|data| PluginEvent {
        bus_index: 0,
        sample_offset: 3,
        ppq_position: 0.0,
        flags: 0,
        data,
    })
}

#[test]
#[ignore = "owned SDK fixture/native Job required; no Process"]
fn native_cache_policy_disabled_refuses_consumers_without_optional_queries_or_mutation() {
    use crate::plugin::{NativeEditorEvent, ParameterEdit, ParameterEditKind};

    let mut p = Probe::new(PluginInterfaceSupport::none());
    assert_eq!(p.calls.snapshot(), [0; 9]);
    // Nonempty real callback/cohort obligations cannot be erased by policy.
    let handler = p.native.component_handler.as_ref().unwrap();
    unsafe {
        assert_eq!(handler.beginEdit(5), kResultOk);
        assert_eq!(handler.performEdit(5, 0.25), kResultOk);
        assert_eq!(handler.endEdit(5), kResultOk);
        assert_eq!(handler.restartComponent(20), kResultOk);
    }
    let mut source_events = Vec::with_capacity(4);
    let mut source_feedback = Vec::with_capacity(1);
    let source = handler
        .observe_editor_source_bounded(4, &mut source_events, &mut source_feedback)
        .unwrap();
    let cohort = p.native.pending_restart_cohort();
    assert_eq!(source.pending_events(), 4);
    assert_eq!(source.pending_dsp_feedback_values(), 1);
    assert_eq!(cohort.unwrap().flags().bits(), 20);
    assert_eq!(
        source_events,
        [
            NativeEditorEvent::Parameter(ParameterEdit {
                id: 5,
                kind: ParameterEditKind::BeginGesture,
                value: None,
            }),
            NativeEditorEvent::Parameter(ParameterEdit {
                id: 5,
                kind: ParameterEditKind::ValueChange,
                value: Some(0.25),
            }),
            NativeEditorEvent::Parameter(ParameterEdit {
                id: 5,
                kind: ParameterEditKind::EndGesture,
                value: None,
            }),
            NativeEditorEvent::ParameterRescanRequested {
                flags: 20,
                cohort: cohort.unwrap(),
            },
        ]
    );
    assert_eq!(source_feedback, [(5, 0.25)]);
    p.native.dirty_caches = DirtyCaches {
        midi_mapping: true,
        program_change: true,
    };
    p.native.service_control_thread_caches();
    p.native.refresh_midi_mapping_cache();
    p.native.refresh_program_change_cache();
    refused(
        p.native.checked_midi_cc_to_parameter(0, 0, 130),
        InterfaceKind::MidiMapping,
    );
    assert_eq!(p.native.midi_cc_to_parameter(0, 0, 10), None);
    for event in mapped_events() {
        refused(
            p.native.send_midi_event_at(event, 3),
            InterfaceKind::MidiMapping,
        );
    }
    refused(
        p.native.send_midi_event_at(program(), 3),
        InterfaceKind::MidiMapping,
    );
    refused(p.native.get_units(), InterfaceKind::UnitInfo);
    refused(p.native.select_program(0, 2), InterfaceKind::UnitInfo);
    refused(p.native.selected_unit(), InterfaceKind::UnitInfo);
    refused(p.native.select_unit(0), InterfaceKind::UnitInfo);
    refused(p.native.program_pitch_names(11, 0), InterfaceKind::UnitInfo);
    refused(
        p.native.get_program_data(11, 0),
        InterfaceKind::ProgramListData,
    );
    refused(
        p.native.set_program_data(11, 0, &[1]),
        InterfaceKind::ProgramListData,
    );
    refused(p.native.get_unit_data(0), InterfaceKind::UnitData);
    refused(p.native.set_unit_data(0, &[1]), InterfaceKind::UnitData);
    refused(
        p.native.note_expressions(0, 0),
        InterfaceKind::NoteExpressionController,
    );
    refused(
        p.native.send_note_expression(
            crate::midi::NoteId(1),
            crate::midi::NoteExpressionType::Volume,
            0.5,
            0,
        ),
        InterfaceKind::NoteExpressionController,
    );
    for event in generic_note_expressions() {
        refused(
            p.native.send_plugin_event(event),
            InterfaceKind::NoteExpressionController,
        );
    }
    assert_eq!(p.calls.snapshot(), [0; 9]);
    assert!(p.native.input_events.is_empty());
    assert!(p.native.pending_param_changes.is_empty());
    assert_eq!(p.native.editor_event_source_status().unwrap(), source);
    assert_eq!(p.native.pending_restart_cohort(), cohort);
    let mut retained_events = Vec::with_capacity(4);
    let mut retained_feedback = Vec::with_capacity(1);
    assert_eq!(
        p.native
            .component_handler
            .as_ref()
            .unwrap()
            .observe_editor_source_bounded(4, &mut retained_events, &mut retained_feedback)
            .unwrap(),
        source
    );
    assert_eq!(retained_events, source_events);
    assert_eq!(retained_feedback, source_feedback);
    // Generic SysEx is a core event and remains independent of expression policy.
    p.native
        .send_plugin_event(PluginEvent::sysex(vec![0xf0, 0x7d, 0xf7]))
        .unwrap();
    assert_eq!(p.native.input_events.events.lock().unwrap().len(), 1);
    p.native.input_events.clear();
    // Mapping policy must not disable ordinary notes or true poly pressure.
    p.native.note_on(MidiChannel::Ch1, 60, 100, 0).unwrap();
    p.native
        .send_midi_event(MidiEvent::NoteOn {
            channel: MidiChannel::Ch2,
            note: 64,
            velocity: 100,
        })
        .unwrap();
    p.native
        .send_midi_event(MidiEvent::PolyAftertouch {
            channel: MidiChannel::Ch2,
            note: 64,
            pressure: 64,
        })
        .unwrap();
    p.native.input_events.clear();
    p.native.midi_panic().unwrap();
    let events = p.native.input_events.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| matches!(event.data, PluginEventData::NoteOff { .. })));
    drop(events);
    assert!(p.native.active_notes.is_empty());
    assert!(p
        .native
        .ordinary_note_counts
        .iter()
        .all(|count| *count == 0));
    assert_eq!(p.calls.snapshot(), [0; 9]);
    assert!(p.native.pending_param_changes.is_empty());
    let calls = p.calls.clone();
    drop(p);
    assert_eq!(
        calls.values[8].load(Ordering::Relaxed),
        0,
        "counted interfaces were restored before native terminate"
    );
    assert_eq!(calls.drops.load(Ordering::Relaxed), 1);
}

#[test]
#[ignore = "owned SDK fixture/native Job required; no Process"]
fn native_cache_policy_default_preserves_mapping_fallback_and_panic_consumers() {
    let mut p = Probe::new(PluginInterfaceSupport::default());
    let calls = p.calls.snapshot();
    assert_eq!(calls[0], 1);
    assert_eq!(calls[1], MIDI_CHANNEL_COUNT * MIDI_CONTROLLER_COUNT);
    assert!(calls[2] > 0 && calls[3] > 0);
    assert_eq!(
        p.native.checked_midi_cc_to_parameter(0, 0, 130).unwrap(),
        Some(41)
    );
    p.native.send_midi_event_at(program(), 3).unwrap();
    assert_eq!(p.native.pending_param_changes.last().unwrap().id, 41);
    assert_eq!(
        p.native.pending_param_changes.last().unwrap().sample_offset,
        3
    );
    p.native.pending_param_changes.clear();
    p.calls.mapped_program.store(false, Ordering::Relaxed);
    p.native.refresh_midi_mapping_cache();
    p.native.send_midi_event_at(program(), 3).unwrap();
    let change = p.native.pending_param_changes.last().unwrap();
    assert_eq!(change.id, 42);
    assert_eq!(change.value, 2.0 / 3.0);
    p.native.pending_param_changes.clear();
    assert_eq!(p.native.get_units().unwrap().len(), 1);
    assert_eq!(p.native.selected_unit().unwrap(), Some(0));
    p.native.select_unit(0).unwrap();
    assert!(p.native.program_pitch_names(11, 0).unwrap().is_empty());
    assert_eq!(p.native.note_expressions(0, 0).unwrap().len(), 1);
    p.native
        .send_note_expression(
            crate::midi::NoteId(1),
            crate::midi::NoteExpressionType::Volume,
            0.5,
            0,
        )
        .unwrap();
    let generic = generic_note_expressions();
    for event in generic.iter().cloned() {
        p.native.send_plugin_event(event).unwrap();
    }
    assert!(p
        .native
        .input_events
        .events
        .lock()
        .unwrap()
        .ends_with(&generic));
    p.native.select_program(0, 1).unwrap();
    assert_eq!(p.native.pending_param_changes.last().unwrap().id, 42);
    p.native.pending_param_changes.clear();
    p.native.input_events.clear();
    p.native.note_on(MidiChannel::Ch1, 60, 100, 0).unwrap();
    p.native.input_events.clear();
    p.native.midi_panic().unwrap();
    assert_eq!(p.native.input_events.events.lock().unwrap().len(), 1);
    assert_eq!(p.native.pending_param_changes.len(), MIDI_CHANNEL_COUNT * 3);
}

#[test]
#[ignore = "owned SDK fixture/native Job required; no Process"]
fn native_cache_policy_independent_alternatives_and_stream_interfaces() {
    let mut midi = Probe::new(PluginInterfaceSupport::none().with(InterfaceKind::MidiMapping));
    assert_eq!(midi.calls.snapshot()[2], 0);
    midi.native.send_midi_event(program()).unwrap();
    assert_eq!(midi.native.pending_param_changes.last().unwrap().id, 41);
    midi.native.pending_param_changes.clear();
    midi.calls.mapped_program.store(false, Ordering::Relaxed);
    midi.native.refresh_midi_mapping_cache();
    midi.native.send_midi_event(program()).unwrap();
    assert!(
        midi.native.pending_param_changes.is_empty(),
        "declared but unmapped MIDI remains a legal no-op"
    );
    assert_eq!(midi.calls.snapshot()[2], 0);
    let mut units = Probe::new(PluginInterfaceSupport::none().with(InterfaceKind::UnitInfo));
    assert_eq!(units.calls.snapshot()[0], 0);
    assert_eq!(units.calls.snapshot()[1], 0);
    units.native.send_midi_event(program()).unwrap();
    assert_eq!(units.native.pending_param_changes.last().unwrap().id, 42);
    assert_eq!(units.calls.snapshot()[1], 0);
    for (interface, other) in [
        (InterfaceKind::ProgramListData, InterfaceKind::UnitData),
        (InterfaceKind::UnitData, InterfaceKind::ProgramListData),
    ] {
        let mut streams = Probe::new(PluginInterfaceSupport::none().with(interface));
        match interface {
            InterfaceKind::ProgramListData => {
                assert_eq!(
                    streams.native.get_program_data(11, 0).unwrap(),
                    Some(Vec::new())
                );
                streams.native.set_program_data(11, 0, &[1, 2]).unwrap();
                refused(streams.native.get_unit_data(0), other);
                refused(streams.native.set_unit_data(0, &[1]), other);
                assert!(streams.calls.snapshot()[4] > 0);
                assert_eq!(streams.calls.snapshot()[5], 0);
            }
            InterfaceKind::UnitData => {
                assert_eq!(streams.native.get_unit_data(0).unwrap(), Some(Vec::new()));
                streams.native.set_unit_data(0, &[1, 2]).unwrap();
                refused(streams.native.get_program_data(11, 0), other);
                refused(streams.native.set_program_data(11, 0, &[1]), other);
                assert!(streams.calls.snapshot()[5] > 0);
                assert_eq!(streams.calls.snapshot()[4], 0);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            streams.calls.snapshot()[2],
            0,
            "successful stream writes never query undeclared UnitInfo"
        );
    }
}

#[test]
#[ignore = "owned SDK fixture/native Job required; no Process"]
fn native_parameter_info_witness_refuses_enabled_cache_consumers_before_retirement() {
    use crate::plugin::{NativeEditorEventDrain, RestartSourceError as Failure};
    for policy in [
        PluginInterfaceSupport::none(),
        PluginInterfaceSupport::none().with(InterfaceKind::MidiMapping),
        PluginInterfaceSupport::none().with(InterfaceKind::UnitInfo),
        PluginInterfaceSupport::none()
            .with(InterfaceKind::MidiMapping)
            .with(InterfaceKind::UnitInfo),
    ] {
        let mut p = Probe::new(policy);
        let blocked = p
            .native
            ._host_app
            .consumes_plugin_interface(InterfaceKind::MidiMapping)
            || p.native
                ._host_app
                .consumes_plugin_interface(InterfaceKind::UnitInfo);
        let handler = p.native.component_handler.as_ref().unwrap();
        assert!(handler.editor_event_source_status().closed_empty());
        unsafe {
            assert_eq!(handler.restartComponent(16), kResultOk);
        }
        // This internal witness is setup for the negative settle control; the
        // public PluginImpl wrapper must independently enforce its own policy.
        let witness = handler
            .capture_restart_source_witness(p.native.native_editor_owner)
            .unwrap();
        let source = handler.editor_event_source_status();
        let cohort = handler.pending_restart_cohort();
        let calls = p.calls.snapshot();
        let captured = p.native.capture_restart_source_witness();
        if blocked {
            assert_eq!(captured, Err(Failure::Unsupported));
        } else {
            assert_eq!(captured, Ok(witness));
        }
        let handler = p.native.component_handler.as_ref().unwrap();
        assert_eq!(handler.editor_event_source_status(), source);
        assert_eq!(handler.pending_restart_cohort(), cohort);
        assert!(
            matches!(handler.take_editor_events(), NativeEditorEventDrain::Complete { events }
            if events.len() == 1)
        );
        let source = handler.editor_event_source_status();
        let settled = p.native.settle_restart_source_prefix(witness);
        if blocked {
            assert_eq!(settled, Err(Failure::Unsupported));
            assert_eq!(p.native.pending_restart_cohort(), Some(cohort));
        } else {
            assert_eq!(settled, Ok(crate::plugin::RestartSourceSettlement::Retired));
            assert!(!p.native.pending_restart_cohort().unwrap().pending());
        }
        assert_eq!(p.native.editor_event_source_status().unwrap(), source);
        assert_eq!(
            p.calls.snapshot(),
            calls,
            "no cache getter, setter or lifecycle service"
        );
    }
}
