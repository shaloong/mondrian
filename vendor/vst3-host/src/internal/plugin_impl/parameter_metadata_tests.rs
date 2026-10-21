//! Real COM controller compatibility queries; no DLL, DSP or whole-heap qualification.
use super::*;
use crate::controller_parameters::{ControllerParameterQueryFrame, ControllerParameterRef};
use vst3::Class;

struct Probe {
    count: i32,
    skip: Option<i32>,
    calls: Vec<(&'static str, u32)>,
    title: [u16; 128],
    units: [u16; 128],
    value: f64,
    full_writable: bool,
}
impl Probe {
    fn new() -> Self {
        let mut title = [0; 128];
        title[..4].copy_from_slice(&[71, 97, 105, 110]);
        let mut units = [0; 128];
        units[..2].copy_from_slice(&[100, 66]);
        Self {
            count: 2,
            skip: Some(0),
            calls: Vec::with_capacity(2 * 1024 + 2),
            title,
            units,
            value: 0.25,
            full_writable: false,
        }
    }
}
struct Controller(Arc<Mutex<Probe>>);
impl Class for Controller {
    type Interfaces = (IEditController,);
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
        let mut p = self.0.lock().unwrap();
        p.calls.push(("count", 0));
        p.count
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let mut p = self.0.lock().unwrap();
        p.calls.push(("info", index as u32));
        if p.skip == Some(index) {
            return kResultFalse;
        }
        *info = std::mem::zeroed();
        (*info).id = 100 + index as u32;
        (*info).title = p.title;
        (*info).units = p.units;
        (*info).defaultNormalizedValue = -0.0;
        (*info).stepCount = 0;
        (*info).flags = if p.full_writable {
            if index % 2 == 0 {
                ParameterInfo_::ParameterFlags_::kCanAutomate
            } else {
                0
            }
        } else {
            ParameterInfo_::ParameterFlags_::kCanAutomate
                | ParameterInfo_::ParameterFlags_::kIsReadOnly
                | (1 << 30)
        };
        kResultOk
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        let mut p = self.0.lock().unwrap();
        p.calls.push(("value", id));
        p.value
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        let mut p = self.0.lock().unwrap();
        p.calls.push(("set", id));
        p.value = value;
        kResultOk
    }
    unsafe fn normalizedParamToPlain(&self, _: u32, _: f64) -> f64 {
        panic!("metadata cannot convert")
    }
    unsafe fn plainParamToNormalized(&self, _: u32, _: f64) -> f64 {
        panic!("metadata cannot convert")
    }
    unsafe fn getParamStringByValue(&self, _: u32, _: f64, _: *mut String128) -> tresult {
        panic!("metadata cannot format")
    }
    unsafe fn getParamValueByString(&self, _: u32, _: *mut TChar, _: *mut f64) -> tresult {
        panic!("metadata cannot parse")
    }
    unsafe fn setComponentHandler(&self, _: *mut IComponentHandler) -> tresult {
        panic!("query cannot rebind")
    }
    unsafe fn createView(&self, _: FIDString) -> *mut IPlugView {
        panic!("query cannot open")
    }
}
fn attach(plugin: &mut PluginImpl) -> Arc<Mutex<Probe>> {
    let probe = Arc::new(Mutex::new(Probe::new()));
    plugin.controller = ComWrapper::new(Controller(Arc::clone(&probe))).to_com_ptr();
    probe
}
fn assert_row(dense: usize, row: ControllerParameterRef<'_>, owned: &[Parameter]) {
    let expected = &owned[dense];
    assert_eq!(row.id(), expected.id);
    assert!(row.title_matches(&expected.name));
    assert!(
        char::decode_utf16(row.units_utf16().iter().take_while(|c| **c != 0).copied())
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .eq(expected.unit.chars())
    );
    assert_eq!(row.normalized().to_bits(), expected.value.to_bits());
    assert_eq!(
        row.default_normalized().to_bits(),
        expected.default.to_bits()
    );
    assert_eq!(row.step_count(), expected.step_count);
    assert_eq!(row.flags(), expected.flags);
    assert_eq!(row.can_automate(), expected.can_automate);
    assert_eq!(row.is_read_only(), expected.is_read_only);
}

pub(super) fn compatibility(plugin: &mut PluginImpl) {
    let probe = attach(plugin);
    plugin.deferred_controller_sync.push((101, 0.75)).unwrap();
    let owned = plugin.get_all_parameters().unwrap();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].id, 101);
    let expected = probe.lock().unwrap().calls.clone();
    assert_eq!(
        expected,
        [
            ("set", 101),
            ("count", 0),
            ("info", 0),
            ("info", 1),
            ("value", 101)
        ]
    );
    assert!(plugin.deferred_controller_sync.is_empty());
    probe.lock().unwrap().calls.clear();
    plugin.deferred_controller_sync.push((101, 0.75)).unwrap();
    let mut seen = 0;
    assert_eq!(
        plugin
            .visit_parameters_bounded(
                1024,
                &mut ControllerParameterQueryFrame::default(),
                &mut |dense, row| {
                    assert_eq!(dense, seen);
                    seen += 1;
                    assert_row(dense, row, &owned);
                }
            )
            .unwrap(),
        1
    );
    assert_eq!(seen, 1);
    assert_eq!(probe.lock().unwrap().calls, expected);
    assert!(plugin.deferred_controller_sync.is_empty());
    // Failed INFO is skipped only by compatibility enumeration; keep the
    // strict author API's independent rejection and no-drain policy unchanged.
    assert!(plugin
        .visit_controller_parameters_bounded(
            1024,
            &mut ControllerParameterQueryFrame::default(),
            &mut |_, _| Ok(())
        )
        .is_err());
    for title in [[0xd800; 128], [0; 128], [0x03a9; 128]] {
        {
            let mut p = probe.lock().unwrap();
            p.title = title;
            p.units = [0xdfff; 128];
            p.value = f64::NAN;
            p.calls.clear();
        }
        let owned = plugin.get_all_parameters().unwrap();
        assert!(owned[0].value.is_nan());
        let expected = probe.lock().unwrap().calls.clone();
        probe.lock().unwrap().calls.clear();
        assert_eq!(
            plugin
                .visit_parameters_bounded(
                    1024,
                    &mut ControllerParameterQueryFrame::default(),
                    &mut |dense, row| assert_row(dense, row, &owned)
                )
                .unwrap(),
            1
        );
        assert_eq!(probe.lock().unwrap().calls, expected);
    }
    let controller = plugin.controller.take();
    let before = probe.lock().unwrap().calls.len();
    assert!(plugin.get_all_parameters().unwrap().is_empty());
    assert_eq!(
        plugin
            .visit_parameters_bounded(
                1024,
                &mut ControllerParameterQueryFrame::default(),
                &mut |_, _| panic!("missing controller")
            )
            .unwrap(),
        0
    );
    assert_eq!(probe.lock().unwrap().calls.len(), before);
    plugin.controller = controller;
    {
        let mut p = probe.lock().unwrap();
        p.count = -1;
        p.calls.clear();
    }
    assert!(plugin.get_all_parameters().unwrap().is_empty());
    assert_eq!(
        plugin
            .visit_parameters_bounded(
                1024,
                &mut ControllerParameterQueryFrame::default(),
                &mut |_, _| panic!("negative count")
            )
            .unwrap(),
        0
    );
    assert_eq!(probe.lock().unwrap().calls, [("count", 0), ("count", 0)]);
}

pub(super) fn bounds_and_allocations(plugin: &mut PluginImpl) {
    let probe = attach(plugin);
    {
        let mut p = probe.lock().unwrap();
        p.count = 1024;
        p.skip = None;
    }
    let mut frame = ControllerParameterQueryFrame::default();
    // The COM trace is preallocated. One original observer records Rust host
    // allocator operations only, without a second allocator or SDK-heap claim.
    for mismatch in [false, true] {
        probe.lock().unwrap().calls.clear();
        let mut seen = 0;
        let mut matches = true;
        let (result, operations) =
            crate::internal::data_exchange::measure_host_allocator_operations_for_test(|| {
                plugin.visit_parameters_bounded(1024, &mut frame, &mut |dense, row| {
                    assert_eq!(dense, seen);
                    seen += 1;
                    matches &= row.id() == 100 + dense as u32 + u32::from(mismatch);
                    assert!(row.title_matches("Gain"));
                    assert_eq!(row.normalized().to_bits(), 0.25_f64.to_bits());
                    assert!(row.can_automate() && row.is_read_only());
                    assert_eq!(row.flags() & (1 << 30), 1 << 30);
                })
            });
        assert_eq!(result.unwrap(), 1024);
        assert_eq!(seen, 1024);
        assert_eq!(matches, !mismatch);
        assert_eq!(
            operations, 0,
            "successful scan and legal mismatch retain no metadata containers"
        );
        let expected = std::iter::once(("count", 0))
            .chain((0..1024).flat_map(|index| [("info", index), ("value", 100 + index)]))
            .collect::<Vec<_>>();
        assert_eq!(probe.lock().unwrap().calls, expected);
    }
    for count in [1025, 2855] {
        {
            let mut p = probe.lock().unwrap();
            p.count = count;
            p.skip = Some(0);
            p.calls.clear();
        }
        plugin.deferred_controller_sync.push((101, 0.75)).unwrap();
        let mut seen = 0;
        let error = plugin
            .visit_parameters_bounded(1024, &mut frame, &mut |_, _| seen += 1)
            .unwrap_err();
        assert!(matches!(error, Error::InvalidParameter(detail)
            if detail == format!("native parameter count exceeds admitted bound: reported={count} maximum=1024")));
        assert_eq!(seen, 0);
        assert_eq!(
            probe.lock().unwrap().calls,
            [("set", 101), ("count", 0)],
            "original drain precedes the single count; no INFO/value query after over-limit rejection"
        );
        assert!(plugin.deferred_controller_sync.is_empty());
    }
    // The product's complete initial-directory policy is independently bounded
    // from callers that deliberately keep the original 1024 limit above.
    let maximum = crate::initial_parameter_input::MAX_INITIAL_PARAMETERS;
    for count in [2855, 4096] {
        {
            let mut p = probe.lock().unwrap();
            p.count = count;
            p.skip = None;
            p.full_writable = true;
            p.value = 0.625;
            p.calls.clear();
            p.calls.reserve(2 * count as usize + 2);
        }
        let mut seen = 0;
        let mut nonautomatable = 0;
        let (result, operations) =
            crate::internal::data_exchange::measure_host_allocator_operations_for_test(|| {
                plugin.visit_parameters_bounded(maximum, &mut frame, &mut |dense, row| {
                    assert_eq!(dense, seen);
                    assert_eq!(row.id(), 100 + dense as u32);
                    assert!(!row.is_read_only());
                    assert_eq!(row.can_automate(), dense % 2 == 0);
                    assert_eq!(row.normalized().to_bits(), 0.625_f64.to_bits());
                    nonautomatable += usize::from(!row.can_automate());
                    seen += 1;
                })
            });
        assert_eq!(result.unwrap(), count as usize);
        assert_eq!(seen, count as usize);
        assert_eq!(
            nonautomatable,
            count as usize / 2,
            "nonautomatable writable rows cannot be filtered"
        );
        assert_eq!(
            operations, 0,
            "trace and one query frame were prepared before observation"
        );
        let expected = std::iter::once(("count", 0))
            .chain((0..count as u32).flat_map(|index| [("info", index), ("value", 100 + index)]))
            .collect::<Vec<_>>();
        assert_eq!(probe.lock().unwrap().calls, expected);
    }
    for count in [4097, i32::MAX] {
        {
            let mut p = probe.lock().unwrap();
            p.count = count;
            p.skip = Some(0);
            p.calls.clear();
        }
        plugin.deferred_controller_sync.push((101, 0.75)).unwrap();
        let mut seen = 0;
        let error = plugin
            .visit_parameters_bounded(maximum, &mut frame, &mut |_, _| seen += 1)
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidParameter(detail) if detail == format!("native parameter count exceeds admitted bound: reported={count} maximum=4096"))
        );
        assert_eq!(seen, 0);
        assert_eq!(
            probe.lock().unwrap().calls,
            [("set", 101), ("count", 0)],
            "over-limit native count must precede every Info/value/visitor"
        );
        assert!(plugin.deferred_controller_sync.is_empty());
    }
}
