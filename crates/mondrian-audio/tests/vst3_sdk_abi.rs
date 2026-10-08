//! Canonical generated API compared with a separate unmodified official C++ SDK.
//! Layout/GUID/signature checks do not qualify plugin semantics or COM call-through.

use std::mem::{align_of, offset_of, size_of};
use vst3::Steinberg::Vst::NoteOnOrchestralArticulation::{
    ClassificationVariations, IInfo, IInfoVtbl, IInfo_iid, Variations,
};
use vst3::Steinberg::Vst::{
    BusDirections, BusIndex, CtrlNumber, IMidiLearn2, IMidiLearn2Vtbl, IMidiLearn2_iid,
    IMidiMapping2, IMidiMapping2Vtbl, IMidiMapping2_iid, ITransportControl, ITransportControlVtbl,
    ITransportControl_iid, Midi1ControllerParamIDAssignment, Midi1ControllerParamIDAssignmentList,
    Midi2Controller, Midi2ControllerParamIDAssignment, Midi2ControllerParamIDAssignmentList,
    MidiChannel, TransportPosition, TransportPosition__type0,
};
use vst3::Steinberg::{int16, int32, tresult, uint32, TUID};

fn iid_hex(iid: TUID) -> String {
    iid.iter().map(|byte| format!("{:02x}", *byte as u8)).collect()
}

fn assert_layout(report: &serde_json::Value, name: &str, fields: &[(&str, usize)]) {
    for (field, actual) in fields {
        assert_eq!(
            report["layouts"][name][field], *actual,
            "official ABI {name}.{field}"
        );
    }
}

#[test]
#[ignore = "requires independent official VST3 3.8.1 C++ ABI report"]
fn latest_sdk_midi2_abi_matches_official_cpp() {
    let path = std::env::var_os("MONDRIAN_VST3_LATEST_ABI_REPORT")
        .expect("set MONDRIAN_VST3_LATEST_ABI_REPORT to the independent official C++ report");
    let bytes = std::fs::read(path).expect("read independent ABI report");
    assert!(bytes.len() <= 16_384, "unexpected ABI report extent");
    let report: serde_json::Value = serde_json::from_slice(&bytes).expect("parse ABI report");
    assert_eq!(report["schema"], 1);
    assert_eq!(
        report["sdk_commit"],
        "3cdf9ca5d1f5b1b21e0a86832aa4abe55607bd96"
    );
    assert_eq!(
        report["pluginterfaces_commit"],
        "4f547e8e102b47de4a8b8aaf343c73b700786372"
    );
    assert_eq!(report["sdk_version"], "VST 3.8.1");
    assert_eq!(report["pointer_bytes"], size_of::<usize>());
    assert_eq!(
        report["com_compatible"],
        u8::from(cfg!(target_os = "windows"))
    );
    assert_layout(
        &report,
        "Midi2Controller",
        &[
            ("size", size_of::<Midi2Controller>()),
            ("align", align_of::<Midi2Controller>()),
            ("byte1", offset_of!(Midi2Controller, byte1)),
            ("byte2", offset_of!(Midi2Controller, byte2)),
        ],
    );
    assert_layout(
        &report,
        "Midi2ControllerParamIDAssignment",
        &[
            ("size", size_of::<Midi2ControllerParamIDAssignment>()),
            ("align", align_of::<Midi2ControllerParamIDAssignment>()),
            ("pId", offset_of!(Midi2ControllerParamIDAssignment, pId)),
            (
                "busIndex",
                offset_of!(Midi2ControllerParamIDAssignment, busIndex),
            ),
            (
                "channel",
                offset_of!(Midi2ControllerParamIDAssignment, channel),
            ),
            (
                "controller",
                offset_of!(Midi2ControllerParamIDAssignment, controller),
            ),
        ],
    );
    assert_layout(
        &report,
        "Midi2ControllerParamIDAssignmentList",
        &[
            ("size", size_of::<Midi2ControllerParamIDAssignmentList>()),
            ("align", align_of::<Midi2ControllerParamIDAssignmentList>()),
            (
                "count",
                offset_of!(Midi2ControllerParamIDAssignmentList, count),
            ),
            ("map", offset_of!(Midi2ControllerParamIDAssignmentList, map)),
        ],
    );
    assert_layout(
        &report,
        "Midi1ControllerParamIDAssignment",
        &[
            ("size", size_of::<Midi1ControllerParamIDAssignment>()),
            ("align", align_of::<Midi1ControllerParamIDAssignment>()),
            ("pId", offset_of!(Midi1ControllerParamIDAssignment, pId)),
            (
                "busIndex",
                offset_of!(Midi1ControllerParamIDAssignment, busIndex),
            ),
            (
                "channel",
                offset_of!(Midi1ControllerParamIDAssignment, channel),
            ),
            (
                "controller",
                offset_of!(Midi1ControllerParamIDAssignment, controller),
            ),
        ],
    );
    assert_layout(
        &report,
        "Midi1ControllerParamIDAssignmentList",
        &[
            ("size", size_of::<Midi1ControllerParamIDAssignmentList>()),
            ("align", align_of::<Midi1ControllerParamIDAssignmentList>()),
            (
                "count",
                offset_of!(Midi1ControllerParamIDAssignmentList, count),
            ),
            ("map", offset_of!(Midi1ControllerParamIDAssignmentList, map)),
        ],
    );
    assert_layout(
        &report,
        "TransportPosition",
        &[
            ("size", size_of::<TransportPosition>()),
            ("align", align_of::<TransportPosition>()),
            ("type", offset_of!(TransportPosition, r#type)),
            (
                "samples",
                offset_of!(TransportPosition, __field0)
                    + offset_of!(TransportPosition__type0, samples),
            ),
            (
                "quarterNotes",
                offset_of!(TransportPosition, __field0)
                    + offset_of!(TransportPosition__type0, quarterNotes),
            ),
        ],
    );
    assert_layout(
        &report,
        "Variations",
        &[
            ("size", size_of::<Variations>()),
            ("align", align_of::<Variations>()),
            ("variation", offset_of!(Variations, variation)),
            ("variation_count", 16),
        ],
    );
    assert_layout(
        &report,
        "ClassificationVariations",
        &[
            ("size", size_of::<ClassificationVariations>()),
            ("align", align_of::<ClassificationVariations>()),
            (
                "classification",
                offset_of!(ClassificationVariations, classification),
            ),
            ("classification_count", 17),
        ],
    );
    for (name, size, align, iid) in [
        (
            "IMidiMapping2",
            size_of::<IMidiMapping2>(),
            align_of::<IMidiMapping2>(),
            IMidiMapping2_iid,
        ),
        (
            "IMidiLearn2",
            size_of::<IMidiLearn2>(),
            align_of::<IMidiLearn2>(),
            IMidiLearn2_iid,
        ),
        (
            "ITransportControl",
            size_of::<ITransportControl>(),
            align_of::<ITransportControl>(),
            ITransportControl_iid,
        ),
        (
            "NoteOnOrchestralArticulation::IInfo",
            size_of::<IInfo>(),
            align_of::<IInfo>(),
            IInfo_iid,
        ),
    ] {
        assert_eq!(
            report["interfaces"][name]["size"], size,
            "interface {name} size"
        );
        assert_eq!(
            report["interfaces"][name]["align"], align,
            "interface {name} align"
        );
        assert_eq!(report["iids"][name], iid_hex(iid), "interface {name} IID");
    }
    // The C++ oracle separately checks the member signatures from official
    // headers. These assignments type-check the actual generated Rust ABI,
    // including the changed two-byte MIDI2 argument passed by value.
    let _signatures = |mapping: &IMidiMapping2Vtbl,
                       learn: &IMidiLearn2Vtbl,
                       transport: &ITransportControlVtbl,
                       info: &IInfoVtbl| {
        let _: unsafe extern "system" fn(*mut IMidiMapping2, BusDirections) -> uint32 =
            mapping.getNumMidi2ControllerAssignments;
        let _: unsafe extern "system" fn(
            *mut IMidiMapping2,
            BusDirections,
            *const Midi2ControllerParamIDAssignmentList,
        ) -> tresult = mapping.getMidi2ControllerAssignments;
        let _: unsafe extern "system" fn(*mut IMidiMapping2, BusDirections) -> uint32 =
            mapping.getNumMidi1ControllerAssignments;
        let _: unsafe extern "system" fn(
            *mut IMidiMapping2,
            BusDirections,
            *const Midi1ControllerParamIDAssignmentList,
        ) -> tresult = mapping.getMidi1ControllerAssignments;
        let _: unsafe extern "system" fn(
            *mut IMidiLearn2,
            BusIndex,
            MidiChannel,
            Midi2Controller,
        ) -> tresult = learn.onLiveMidi2ControllerInput;
        let _: unsafe extern "system" fn(
            *mut IMidiLearn2,
            BusIndex,
            MidiChannel,
            CtrlNumber,
        ) -> tresult = learn.onLiveMidi1ControllerInput;
        let _: unsafe extern "system" fn(
            *mut ITransportControl,
            int32,
            *mut TransportPosition,
        ) -> tresult = transport.requestAction;
        let _: unsafe extern "system" fn(*mut ITransportControl, int32) -> tresult =
            transport.isActionSupported;
        let _: unsafe extern "system" fn(
            *mut IInfo,
            int32,
            int16,
            *mut ClassificationVariations,
        ) -> tresult = info.getVariationsInfo;
    };
}
