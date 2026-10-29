//! Actual official C++ virtual calls into the canonical Rust-generated COM object.
//! This qualifies this ABI slice, not optional host features or third-party plugins.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use vst3::Steinberg::Vst::NoteOnOrchestralArticulation::{
    ClassificationVariations, IInfo, IInfoTrait,
};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::{
    int16, int32, kInvalidArgument, kResultFalse, kResultOk, tresult, uint32, FUnknown,
};
use vst3::{Class, ComWrapper};

struct CallThrough {
    seen: Arc<AtomicU32>,
    dropped: Arc<AtomicU32>,
}

impl Class for CallThrough {
    type Interfaces = (IMidiMapping2, IMidiLearn2, ITransportControl, IInfo);
}

impl Drop for CallThrough {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

impl IMidiMapping2Trait for CallThrough {
    unsafe fn getNumMidi2ControllerAssignments(&self, direction: BusDirections) -> uint32 {
        if direction != BusDirections_::kInput {
            return 0;
        }
        self.seen.fetch_or(1 << 0, Ordering::Relaxed);
        2
    }

    unsafe fn getMidi2ControllerAssignments(
        &self,
        direction: BusDirections,
        list: *const Midi2ControllerParamIDAssignmentList,
    ) -> tresult {
        if direction != BusDirections_::kInput || list.is_null() {
            return kInvalidArgument;
        }
        let list = unsafe { &*list };
        if list.count != 2 || list.map.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the independent C++ fixture owns an array of exactly list.count
        // writable assignments for the complete duration of this virtual call.
        let assignments = unsafe { std::slice::from_raw_parts_mut(list.map, 2) };
        assignments[0] = Midi2ControllerParamIDAssignment {
            pId: 0xF0FA0011,
            busIndex: -3,
            channel: 12,
            controller: Midi2Controller { byte1: 0x93, byte2: 0x22 },
        };
        assignments[1] = Midi2ControllerParamIDAssignment {
            pId: 0x80000001,
            busIndex: 19,
            channel: 15,
            controller: Midi2Controller { byte1: 0xFF, byte2: 0x00 },
        };
        self.seen.fetch_or(1 << 1, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn getNumMidi1ControllerAssignments(&self, direction: BusDirections) -> uint32 {
        if direction != BusDirections_::kOutput {
            return 0;
        }
        self.seen.fetch_or(1 << 2, Ordering::Relaxed);
        1
    }

    unsafe fn getMidi1ControllerAssignments(
        &self,
        direction: BusDirections,
        list: *const Midi1ControllerParamIDAssignmentList,
    ) -> tresult {
        if direction != BusDirections_::kOutput || list.is_null() {
            return kInvalidArgument;
        }
        let list = unsafe { &*list };
        if list.count != 1 || list.map.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: this known C++ fixture supplies one writable stack assignment.
        unsafe {
            list.map.write(Midi1ControllerParamIDAssignment {
                pId: 0xABCD1234,
                busIndex: -1,
                channel: 3,
                controller: 63,
            });
        }
        self.seen.fetch_or(1 << 3, Ordering::Relaxed);
        kResultOk
    }
}

impl IMidiLearn2Trait for CallThrough {
    unsafe fn onLiveMidi2ControllerInput(
        &self,
        index: BusIndex,
        channel: MidiChannel,
        midi_cc: Midi2Controller,
    ) -> tresult {
        if index != -7 || channel != 13 || midi_cc.byte1 != 0xA6 || midi_cc.byte2 != 0xD3 {
            return kInvalidArgument;
        }
        self.seen.fetch_or(1 << 4, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn onLiveMidi1ControllerInput(
        &self,
        index: BusIndex,
        channel: MidiChannel,
        midi_cc: CtrlNumber,
    ) -> tresult {
        if index != 11 || channel != 7 || midi_cc != 64 {
            return kInvalidArgument;
        }
        self.seen.fetch_or(1 << 5, Ordering::Relaxed);
        kResultOk
    }
}

impl ITransportControlTrait for CallThrough {
    unsafe fn requestAction(&self, action: int32, position: *mut TransportPosition) -> tresult {
        let expected = match action {
            ITransportControl_::Action_::Locate => {
                !position.is_null()
                    && unsafe {
                        (*position).r#type == TransportPositionType_::Samples
                            && (*position).__field0.samples == -1234567890123
                    }
            }
            ITransportControl_::Action_::SetCycleEnd => {
                !position.is_null()
                    && unsafe {
                        (*position).r#type == TransportPositionType_::QuarterNotes
                            && (*position).__field0.quarterNotes == -123.25
                    }
            }
            ITransportControl_::Action_::PlaybackStop => position.is_null(),
            _ => false,
        };
        if !expected {
            return kInvalidArgument;
        }
        self.seen.fetch_or(1 << 6, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn isActionSupported(&self, action: int32) -> tresult {
        self.seen.fetch_or(1 << 7, Ordering::Relaxed);
        if action == ITransportControl_::Action_::Locate {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IInfoTrait for CallThrough {
    unsafe fn getVariationsInfo(
        &self,
        bus_index: int32,
        channel: int16,
        info: *mut ClassificationVariations,
    ) -> tresult {
        if bus_index != -9 || channel != 14 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: C++ supplies a live, correctly aligned official output struct.
        let info = unsafe { &mut *info };
        for (classification, variations) in info.classification.iter_mut().enumerate() {
            for (variation, output) in variations.variation.iter_mut().enumerate() {
                *output = ((classification * 17 + variation * 3) & 255) as u8;
            }
        }
        self.seen.fetch_or(1 << 8, Ordering::Relaxed);
        kResultOk
    }
}

fn sha(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(std::fs::read(path).expect("read receipt artifact"))
    )
}

fn verified_helper() -> PathBuf {
    let receipt_path = std::env::var_os("MONDRIAN_VST3_COM_HELPER_RECEIPT")
        .expect("supply fresh independent official C++ helper receipt");
    let bytes = std::fs::read(receipt_path).expect("read helper receipt");
    assert!(bytes.len() <= 64 * 1024);
    let receipt: serde_json::Value = serde_json::from_slice(&bytes).expect("parse helper receipt");
    assert_eq!(receipt["schema"], 1);
    assert_eq!(
        receipt["sdk_commit"],
        "3cdf9ca5d1f5b1b21e0a86832aa4abe55607bd96"
    );
    assert_eq!(
        receipt["pluginterfaces_commit"],
        "4f547e8e102b47de4a8b8aaf343c73b700786372"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("root");
    assert_eq!(
        receipt["cpp_source_sha256"],
        sha(&root.join("tests/vst3_reference/com-callthrough.cpp"))
    );
    assert!(receipt["compiler"].as_str().is_some_and(|value| !value.is_empty()));
    assert!(receipt["build_command"].as_str().is_some_and(|value| !value.is_empty()));
    for field in ["build_transcript", "dependency_file"] {
        let path = Path::new(receipt[field]["path"].as_str().expect("receipt artifact path"));
        assert_eq!(
            receipt[field]["sha256"],
            sha(path),
            "receipt {field} identity"
        );
    }
    let dependency = std::fs::read_to_string(
        receipt["dependency_file"]["path"].as_str().expect("dependency path"),
    )
    .expect("read actual compiler include dependencies")
    .replace("\\ ", " ")
    .replace('\\', "/");
    let include_root =
        PathBuf::from(receipt["official_include_root"].as_str().expect("official include root"));
    let source_manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("vendor/vst3-sdk/GENERATION-SOURCES.json"))
            .expect("canonical sources"),
    )
    .expect("parse canonical sources");
    let mut official_count = 0;
    for source in source_manifest["sources"].as_array().expect("source records") {
        if source["role"].as_str().is_some_and(|value| value.starts_with("official-")) {
            let relative = source["path"].as_str().expect("official relative path");
            assert!(
                !Path::new(relative).is_absolute()
                    && !Path::new(relative)
                        .components()
                        .any(|component| component == std::path::Component::ParentDir)
            );
            assert_eq!(
                source["sha256"],
                sha(&include_root.join("pluginterfaces").join(relative)),
                "official include {relative}"
            );
            official_count += 1;
        }
    }
    assert_eq!(official_count, 69);
    for header in [
        "ivstmidimapping2.h",
        "ivsttransportcontrol.h",
        "ivstnoteonorchestralarticulationinfo.h",
    ] {
        let actual = include_root
            .join("pluginterfaces/vst")
            .join(header)
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            dependency.contains(&actual),
            "actual compiler dependency must use official {header}"
        );
    }
    let helper =
        PathBuf::from(receipt["helper_binary"]["path"].as_str().expect("helper library path"));
    assert_eq!(receipt["helper_binary"]["sha256"], sha(&helper));
    helper
}

#[test]
#[ignore = "requires independently built official C++ SDK helper and current artifact receipt"]
fn latest_sdk_com_calls_through_all_new_interfaces() {
    let helper = verified_helper();
    let seen = Arc::new(AtomicU32::new(0));
    let dropped = Arc::new(AtomicU32::new(0));
    let wrapper = ComWrapper::new(CallThrough { seen: seen.clone(), dropped: dropped.clone() });
    let unknown = wrapper.to_com_ptr::<FUnknown>().expect("real generated FUnknown identity");
    // SAFETY: verified test helper exports this C ABI, borrows the real generated
    // FUnknown during the synchronous call, and releases every query/addRef.
    let library = unsafe { libloading::Library::new(helper) }.expect("load verified SDK helper");
    let call = unsafe {
        library
            .get::<unsafe extern "C" fn(*mut FUnknown) -> int32>(b"mondrian_sdk381_callthrough\0")
    }
    .expect("official call-through export");
    assert_eq!(
        unsafe { call(unknown.as_ptr()) },
        0,
        "C++ helper stage code"
    );
    assert_eq!(
        seen.load(Ordering::Relaxed),
        (1 << 9) - 1,
        "all nine actual method slots called"
    );
    drop(unknown);
    assert_eq!(
        dropped.load(Ordering::Relaxed),
        0,
        "wrapper owner remains live"
    );
    drop(wrapper);
    assert_eq!(
        dropped.load(Ordering::Relaxed),
        1,
        "balanced foreign query/addRef/release permits one final Rust Drop"
    );
}
