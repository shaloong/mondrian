//! Counted actual COM controller queries; no DLL or DSP qualification.
use super::*;
use crate::controller_parameters::{
    ControllerParameterPresentationFrame, ControllerParameterQueryFrame,
};
use crate::plugin::PluginEditorRole;
use vst3::Class;

struct Probe {
    count: i32,
    calls: Vec<(&'static str, u32)>,
    value: f64,
    plain: f64,
    format_ok: bool,
    callback: u8,
    allow_projection: bool,
    begin_status: tresult,
    set_status: tresult,
    end_status: tresult,
}
impl Default for Probe {
    fn default() -> Self {
        Self {
            count: 2,
            calls: Vec::new(),
            value: 0.25,
            plain: 250.0,
            format_ok: true,
            callback: 0,
            allow_projection: false,
            begin_status: kResultOk,
            set_status: kResultOk,
            end_status: kResultOk,
        }
    }
}
struct Controller {
    probe: Arc<Mutex<Probe>>,
    handler: ComPtr<IComponentHandler>,
    deferred: Arc<ArrayQueue<(u32, f64)>>,
}
impl Class for Controller {
    type Interfaces = (IEditController, IEditControllerHostEditing);
}
impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _: *mut FUnknown) -> tresult {
        panic!("query cannot initialize")
    }
    unsafe fn terminate(&self) -> tresult {
        panic!("query cannot terminate")
    }
}
impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, _: *mut IBStream) -> tresult {
        panic!("query cannot restore")
    }
    unsafe fn setState(&self, _: *mut IBStream) -> tresult {
        panic!("query cannot restore")
    }
    unsafe fn getState(&self, _: *mut IBStream) -> tresult {
        panic!("query cannot capture")
    }
    unsafe fn getParameterCount(&self) -> i32 {
        let mut p = self.probe.lock().unwrap();
        p.calls.push(("count", 0));
        p.count
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let mut p = self.probe.lock().unwrap();
        p.calls.push(("info", index as u32));
        unsafe {
            *info = std::mem::zeroed();
            (*info).id = index as u32;
            // Unselected parameters deliberately have unusable author metadata.
            (*info).defaultNormalizedValue = if index == 0 { 0.5 } else { f64::NAN };
            (*info).title[0] = b'G' as u16;
        }
        kResultOk
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        assert_eq!(id, 0, "unselected values must never be queried");
        let mut p = self.probe.lock().unwrap();
        p.calls.push(("value", id));
        p.value
    }
    unsafe fn normalizedParamToPlain(&self, id: u32, value: f64) -> f64 {
        assert_eq!(id, 0);
        let mut p = self.probe.lock().unwrap();
        assert_eq!(value.to_bits(), p.value.to_bits());
        p.calls.push(("plain", id));
        p.plain
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, output: *mut String128) -> tresult {
        assert_eq!(id, 0);
        let (ok, callback) = {
            let mut p = self.probe.lock().unwrap();
            assert_eq!(value.to_bits(), p.value.to_bits());
            p.calls.push(("format", id));
            (p.format_ok, p.callback)
        };
        unsafe {
            *output = [0; 128];
            (&mut *output)[..6].copy_from_slice(&[50, 53, 48, 32, 72, 122]);
            if callback == 1 {
                assert_eq!(self.handler.performEdit(id, 0.75), kResultOk);
            }
            if callback == 2 {
                assert_eq!(self.handler.restartComponent(4), kResultOk);
            }
        }
        if callback == 3 {
            self.deferred.push((id, 0.75)).unwrap();
        }
        if ok {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getParamValueByString(&self, _: u32, _: *mut TChar, _: *mut f64) -> tresult {
        panic!("query cannot parse")
    }
    unsafe fn plainParamToNormalized(&self, _: u32, _: f64) -> f64 {
        panic!("query cannot author")
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        let (result, callback) = {
            let mut p = self.probe.lock().unwrap();
            assert!(p.allow_projection, "query cannot drain a setter");
            p.calls.push(("set", id));
            if p.set_status == kResultOk {
                p.value = value;
            }
            (p.set_status, p.callback)
        };
        if callback == 1 {
            unsafe {
                assert_eq!(self.handler.performEdit(id, value), kResultOk);
            }
        }
        result
    }
    unsafe fn setComponentHandler(&self, _: *mut IComponentHandler) -> tresult {
        panic!("query cannot rebind")
    }
    unsafe fn createView(&self, _: FIDString) -> *mut IPlugView {
        panic!("query cannot create a view")
    }
}
impl IEditControllerHostEditingTrait for Controller {
    unsafe fn beginEditFromHost(&self, id: u32) -> tresult {
        let mut p = self.probe.lock().unwrap();
        assert!(p.allow_projection);
        p.calls.push(("begin", id));
        p.begin_status
    }
    unsafe fn endEditFromHost(&self, id: u32) -> tresult {
        let mut p = self.probe.lock().unwrap();
        assert!(p.allow_projection);
        p.calls.push(("end", id));
        p.end_status
    }
}
type ObservedPresentation = (u64, Option<u64>, Option<Vec<u16>>);

struct Fixture {
    controller: Option<ComPtr<IEditController>>,
    probe: Arc<Mutex<Probe>>,
    handler: Option<ComWrapper<ComponentHandler>>,
    owner: crate::plugin::NativeEditorAttachmentOwner,
    view: EditorAttachment,
    scale: f32,
    frame: ComWrapper<HostPlugFrame>,
    resize: Arc<Mutex<Option<(i32, i32)>>>,
    deferred: Arc<ArrayQueue<(u32, f64)>>,
    host: HostApplication,
    #[cfg(target_os = "linux")]
    run_loop: Arc<Mutex<RunLoopRegistry>>,
}
impl Fixture {
    fn new() -> Self {
        let probe = Arc::new(Mutex::new(Probe::default()));
        let handler = ComWrapper::new(ComponentHandler::new(Arc::new(Mutex::new(Vec::new()))));
        let deferred = Arc::new(ArrayQueue::new(4));
        let controller = ComWrapper::new(Controller {
            probe: Arc::clone(&probe),
            handler: handler.to_com_ptr::<IComponentHandler>().unwrap(),
            deferred: Arc::clone(&deferred),
        });
        let resize = Arc::new(Mutex::new(None));
        #[cfg(target_os = "linux")]
        let run_loop = Arc::new(Mutex::new(RunLoopRegistry::new()));
        #[cfg(target_os = "linux")]
        let frame = create_host_plug_frame(Arc::clone(&resize), Arc::clone(&run_loop));
        #[cfg(not(target_os = "linux"))]
        let frame = create_host_plug_frame(Arc::clone(&resize));
        Self {
            controller: controller.to_com_ptr::<IEditController>(),
            probe,
            handler: Some(handler),
            owner: crate::plugin::NativeEditorAttachmentOwner::issue().unwrap(),
            view: EditorAttachment::Empty,
            scale: 1.0,
            frame,
            resize,
            deferred,
            host: HostApplication::default(),
            #[cfg(target_os = "linux")]
            run_loop,
        }
    }
    fn fields(&mut self) -> PluginEditorFields<'_> {
        PluginEditorFields {
            controller: &self.controller,
            component_handler: &self.handler,
            native_editor_owner: self.owner,
            plugin_view: &mut self.view,
            editor_scale_factor: &mut self.scale,
            plug_frame: &self.frame,
            editor_resize: &self.resize,
            deferred_controller_sync: &self.deferred,
            host_app: &self.host,
            control_thread: thread::current().id(),
            #[cfg(target_os = "linux")]
            run_loop: &self.run_loop,
            #[cfg(target_os = "linux")]
            factory_run_loop: None,
        }
    }
    fn query(&mut self, maximum: usize, ids: &[u32]) -> Result<Vec<ObservedPresentation>> {
        let mut values = Vec::new();
        let count = self
            .fields()
            .visit_controller_parameter_presentations_bounded(
                maximum,
                ids,
                &mut ControllerParameterPresentationFrame::default(),
                &mut |_, value| {
                    values.push((
                        value.parameter().normalized().to_bits(),
                        value.plain().map(f64::to_bits),
                        value.display_utf16().map(|text| text[..6].to_vec()),
                    ));
                    Ok(())
                },
            )?;
        assert_eq!(
            count,
            usize::try_from(self.probe.lock().unwrap().count).unwrap()
        );
        Ok(values)
    }
}
#[test]
fn bounded_presentations_use_native_plain_and_format_only_for_selected_ids() {
    let mut f = Fixture::new();
    let owner = f.owner;
    let source = f.handler.as_ref().unwrap().editor_event_source_status();
    let cohort = f.handler.as_ref().unwrap().pending_restart_cohort();
    let values = f.query(2, &[0]).unwrap();
    assert_eq!(
        values,
        vec![(
            0.25_f64.to_bits(),
            Some(250.0_f64.to_bits()),
            Some(vec![50, 53, 48, 32, 72, 122])
        )]
    );
    assert_eq!(
        f.probe.lock().unwrap().calls,
        [
            ("count", 0),
            ("info", 0),
            ("value", 0),
            ("plain", 0),
            ("format", 0),
            ("info", 1)
        ]
    );
    assert_eq!(f.fields().attachment_owner().unwrap(), owner);
    assert_eq!(
        f.handler.as_ref().unwrap().editor_event_source_status(),
        source
    );
    assert_eq!(f.handler.as_ref().unwrap().pending_restart_cohort(), cohort);
    assert!(f.deferred.is_empty());
}
#[test]
fn bounded_presentations_keep_unavailable_native_plain_and_format_explicit() {
    let mut f = Fixture::new();
    {
        let mut p = f.probe.lock().unwrap();
        p.format_ok = false;
        p.plain = f64::INFINITY;
    }
    assert_eq!(
        f.query(2, &[0]).unwrap(),
        vec![(0.25_f64.to_bits(), None, None)]
    );
    for value in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
        let mut f = Fixture::new();
        f.probe.lock().unwrap().value = value;
        assert!(f.query(2, &[0]).is_err());
        assert_eq!(
            f.probe.lock().unwrap().calls,
            [("count", 0), ("info", 0), ("value", 0)]
        );
    }
}
#[test]
fn bounded_presentations_reject_selection_count_thread_and_pending_sync_before_queries() {
    for (maximum, ids) in [(1, vec![0, 1]), (2, vec![0, 0])] {
        let mut f = Fixture::new();
        assert!(f.query(maximum, &ids).is_err());
        assert!(f.probe.lock().unwrap().calls.is_empty());
    }
    for count in [-1, 3] {
        let mut f = Fixture::new();
        f.probe.lock().unwrap().count = count;
        assert!(f.query(2, &[0]).is_err());
        assert_eq!(f.probe.lock().unwrap().calls, [("count", 0)]);
    }
    let mut f = Fixture::new();
    assert!(f.query(2, &[7]).is_err(), "missing selected native ID");
    let mut f = Fixture::new();
    f.deferred.push((0, 0.75)).unwrap();
    assert!(f.query(2, &[0]).is_err());
    assert!(f.probe.lock().unwrap().calls.is_empty());
    assert_eq!(f.deferred.pop(), Some((0, 0.75)));
    let mut f = Fixture::new();
    let other_thread = thread::spawn(|| thread::current().id()).join().unwrap();
    {
        let mut fields = f.fields();
        fields.control_thread = other_thread;
        assert!(fields
            .visit_controller_parameter_presentations_bounded(
                2,
                &[0],
                &mut ControllerParameterPresentationFrame::default(),
                &mut |_, _| panic!("wrong thread visit")
            )
            .is_err());
    }
    assert!(f.probe.lock().unwrap().calls.is_empty());
}
#[test]
fn bounded_presentations_preserve_reentrant_source_restart_and_feedback() {
    for callback in 1..=3 {
        let mut f = Fixture::new();
        f.probe.lock().unwrap().callback = callback;
        let source = f.handler.as_ref().unwrap().editor_event_source_status();
        let cohort = f.handler.as_ref().unwrap().pending_restart_cohort();
        let result = f.query(2, &[0]);
        let handler = f.handler.as_ref().unwrap();
        match callback {
            1 => {
                result.unwrap();
                assert_eq!(
                    handler.editor_event_source_status().revision(),
                    source.revision() + 1
                );
                assert_eq!(handler.editor_event_source_status().pending_events(), 1);
                assert_eq!(
                    handler
                        .editor_event_source_status()
                        .pending_dsp_feedback_values(),
                    1
                );
                assert_eq!(
                    handler.parameter_changes.lock().unwrap().as_slice(),
                    [(0, 0.75)]
                );
            }
            2 => {
                result.unwrap();
                assert_eq!(
                    handler.pending_restart_cohort().serial(),
                    cohort.serial() + 1
                );
                assert_eq!(handler.pending_restart_cohort().flags().bits(), 4);
                assert_eq!(handler.editor_event_source_status().pending_events(), 1);
            }
            3 => {
                assert!(result.is_err());
                assert_eq!(f.deferred.pop(), Some((0, 0.75)));
                assert_eq!(handler.editor_event_source_status(), source);
                assert_eq!(handler.pending_restart_cohort(), cohort);
            }
            _ => unreachable!(),
        }
    }
}

// Invoked by the existing real PluginImpl COM fixture, without loading a DLL.
pub(super) fn assert_original_value_query_sequence(plugin: &mut PluginImpl) {
    let f = Fixture::new();
    f.probe.lock().unwrap().count = 1;
    plugin.controller = f.controller.clone();
    plugin
        .visit_controller_parameters_bounded(
            1,
            &mut ControllerParameterQueryFrame::default(),
            &mut |index, parameter| {
                assert_eq!(index, 0);
                assert_eq!(parameter.id(), 0);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        f.probe.lock().unwrap().calls,
        [("count", 0), ("info", 0), ("value", 0)]
    );
    plugin.deferred_controller_sync.push((0, 0.75)).unwrap();
    assert!(plugin
        .visit_controller_parameters_bounded(
            1,
            &mut ControllerParameterQueryFrame::default(),
            &mut |_, _| panic!("pending synchronization cannot be drained")
        )
        .is_err());
    assert_eq!(plugin.deferred_controller_sync.pop(), Some((0, 0.75)));
    assert_eq!(
        f.probe.lock().unwrap().calls,
        [("count", 0), ("info", 0), ("value", 0)]
    );
    let owner = plugin.native_editor_owner;
    {
        let editor = plugin.borrow_editor_control().unwrap();
        assert_eq!(editor.attachment_owner().unwrap(), owner);
        assert_eq!(
            editor
                .visit_controller_parameter_presentations_bounded(
                    1,
                    &[0],
                    &mut ControllerParameterPresentationFrame::default(),
                    &mut |_, value| {
                        assert_eq!(value.plain(), Some(250.0));
                        Ok(())
                    }
                )
                .unwrap(),
            1
        );
    }
    assert_eq!(
        f.probe.lock().unwrap().calls,
        [
            ("count", 0),
            ("info", 0),
            ("value", 0),
            ("count", 0),
            ("info", 0),
            ("value", 0),
            ("plain", 0),
            ("format", 0)
        ]
    );
    // Fixture teardown must not invoke its controller's intentionally forbidden
    // lifecycle methods through the unrelated component's PluginImpl Drop.
    plugin.controller = None;
}

#[test]
fn controller_only_projection_uses_strict_setter_optional_bracket_and_keeps_callbacks() {
    for bracket in [false, true] {
        let mut f = Fixture::new();
        f.probe.lock().unwrap().allow_projection = true;
        let source = f.handler.as_ref().unwrap().editor_event_source_status();
        let cohort = f.handler.as_ref().unwrap().pending_restart_cohort();
        let owner = f.owner;
        f.fields()
            .project_controller_parameter(0, -0.0, bracket)
            .unwrap();
        assert_eq!(
            f.probe.lock().unwrap().value.to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(
            f.probe.lock().unwrap().calls,
            if bracket {
                vec![("begin", 0), ("set", 0), ("end", 0)]
            } else {
                vec![("set", 0)]
            }
        );
        assert_eq!(f.fields().attachment_owner().unwrap(), owner);
        assert_eq!(
            f.handler.as_ref().unwrap().editor_event_source_status(),
            source
        );
        assert_eq!(f.handler.as_ref().unwrap().pending_restart_cohort(), cohort);
        assert!(
            f.deferred.is_empty(),
            "no deferred DSP/controller input is enqueued"
        );
    }
    let mut f = Fixture::new();
    {
        let mut p = f.probe.lock().unwrap();
        p.allow_projection = true;
        p.callback = 1;
    }
    let before = f.handler.as_ref().unwrap().editor_event_source_status();
    f.fields()
        .project_controller_parameter(0, 0.75, false)
        .unwrap();
    let after = f.handler.as_ref().unwrap().editor_event_source_status();
    assert_eq!(after.revision(), before.revision() + 1);
    assert_eq!(
        after.pending_events(),
        1,
        "actual reentrant callback remains for original Poll"
    );
    assert_eq!(after.pending_dsp_feedback_values(), 1);
    assert_eq!(
        f.handler
            .as_ref()
            .unwrap()
            .parameter_changes
            .lock()
            .unwrap()
            .as_slice(),
        [(0, 0.75)]
    );
    assert!(f.deferred.is_empty());
}

#[test]
fn controller_only_projection_refuses_invalid_pending_wrong_thread_and_pairs_failed_brackets() {
    for value in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
        let mut f = Fixture::new();
        assert!(f
            .fields()
            .project_controller_parameter(0, value, true)
            .is_err());
        assert!(f.probe.lock().unwrap().calls.is_empty());
    }
    let mut f = Fixture::new();
    f.deferred.push((0, 0.875)).unwrap();
    assert!(f
        .fields()
        .project_controller_parameter(0, 0.25, true)
        .is_err());
    assert!(f.probe.lock().unwrap().calls.is_empty());
    assert_eq!(f.deferred.pop(), Some((0, 0.875)));
    let foreign = std::thread::spawn(|| std::thread::current().id())
        .join()
        .unwrap();
    {
        let mut fields = f.fields();
        fields.control_thread = foreign;
        assert!(fields.project_controller_parameter(0, 0.25, true).is_err());
    }
    assert!(f.probe.lock().unwrap().calls.is_empty());
    for failed in ["begin", "set", "end"] {
        let mut f = Fixture::new();
        {
            let mut p = f.probe.lock().unwrap();
            p.allow_projection = true;
            match failed {
                "begin" => p.begin_status = kResultFalse,
                "set" => p.set_status = kResultFalse,
                "end" => p.end_status = kResultFalse,
                _ => unreachable!(),
            }
        }
        assert!(f
            .fields()
            .project_controller_parameter(0, 0.25, true)
            .is_err());
        assert_eq!(
            f.probe.lock().unwrap().calls,
            if failed == "begin" {
                vec![("begin", 0)]
            } else {
                vec![("begin", 0), ("set", 0), ("end", 0)]
            }
        );
        assert!(f.deferred.is_empty());
    }
}
