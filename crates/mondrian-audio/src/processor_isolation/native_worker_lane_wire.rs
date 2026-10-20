//! Current preparation codec for the concrete native lane request.
//! This is storage metadata, never a complete Session grant or install lease.

use crate::{
    AudioProcessorHostError, NativeHostBufferLayout, NativeWorkerControlOverlapLayout,
    NativeWorkerLaneOwnedLayouts, NativeWorkerLaneResourceProfile,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "bounded cold preparation DTO keeps fixed layouts inline without another heap owner"
)]
pub(super) enum NativeWorkerLaneWire {
    Inline {},
    Scoped {
        owners: [(u64, u64); 8],
        control: [(u64, u64); 3],
        processor_input_storage: [u64; 2],
        adapter_note_input_storage: (u64, u64),
        processing_audio_buffer_peak_bytes: u64,
    },
}

fn capture_layouts<const N: usize>(
    layouts: [NativeHostBufferLayout; N],
) -> Result<[(u64, u64); N], AudioProcessorHostError> {
    let mut result = [(0, 1); N];
    for (destination, layout) in result.iter_mut().zip(layouts) {
        *destination = (
            u64::try_from(layout.bytes).map_err(|_| invalid())?,
            u64::try_from(layout.alignment).map_err(|_| invalid())?,
        );
    }
    Ok(result)
}

fn realize_layouts<const N: usize>(
    wire: [(u64, u64); N],
) -> Result<[NativeHostBufferLayout; N], AudioProcessorHostError> {
    let mut result = [NativeHostBufferLayout { bytes: 0, alignment: 1 }; N];
    for (destination, (bytes, alignment)) in result.iter_mut().zip(wire) {
        *destination = NativeHostBufferLayout {
            bytes: usize::try_from(bytes).map_err(|_| invalid())?,
            alignment: usize::try_from(alignment).map_err(|_| invalid())?,
        };
    }
    Ok(result)
}

impl NativeWorkerLaneWire {
    pub(super) fn capture(
        profile: Option<NativeWorkerLaneResourceProfile>,
    ) -> Result<Self, AudioProcessorHostError> {
        let Some(profile) = profile else {
            return Ok(Self::Inline {});
        };
        let owners = profile.owning_layouts();
        let control = profile.control_overlap();
        let (vectors, com_payloads) = profile.processor_input_storage();
        Ok(Self::Scoped {
            processing_audio_buffer_peak_bytes: u64::try_from(
                profile.processing_audio_buffer_peak_bytes(),
            )
            .map_err(|_| invalid())?,
            adapter_note_input_storage: capture_layouts([profile.adapter_note_input_storage()])?[0],
            processor_input_storage: [
                u64::try_from(vectors).map_err(|_| invalid())?,
                u64::try_from(com_payloads).map_err(|_| invalid())?,
            ],
            owners: capture_layouts([
                owners.adapter_resident,
                owners.reader_slots,
                owners.processing_slots,
                owners.main_inline,
                owners.runtime_main_role,
                owners.runtime_dsp_role,
                owners.sdk_main_role,
                owners.sdk_dsp_role,
            ])?,
            control: capture_layouts([
                control.decoded_input,
                control.reply_storage,
                control.staged_seed,
            ])?,
        })
    }
    pub(super) fn realize(
        self,
    ) -> Result<Option<NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        let Self::Scoped {
            owners,
            control,
            processor_input_storage,
            adapter_note_input_storage,
            processing_audio_buffer_peak_bytes,
        } = self
        else {
            return Ok(None);
        };
        let [adapter_resident, reader_slots, processing_slots, main_inline, runtime_main_role, runtime_dsp_role, sdk_main_role, sdk_dsp_role] =
            realize_layouts(owners)?;
        let [decoded_input, reply_storage, staged_seed] = realize_layouts(control)?;
        Ok(Some(
            NativeWorkerLaneResourceProfile::scoped_main_dsp(
                NativeWorkerLaneOwnedLayouts {
                    adapter_resident,
                    reader_slots,
                    processing_slots,
                    main_inline,
                    runtime_main_role,
                    runtime_dsp_role,
                    sdk_main_role,
                    sdk_dsp_role,
                },
                NativeWorkerControlOverlapLayout { decoded_input, reply_storage, staged_seed },
            )?
            .with_adapter_note_input_storage(realize_layouts([adapter_note_input_storage])?[0])?
            .with_processor_input_storage(
                usize::try_from(processor_input_storage[0]).map_err(|_| invalid())?,
                usize::try_from(processor_input_storage[1]).map_err(|_| invalid())?,
            )?
            .with_processing_audio_buffer_peak_bytes(
                usize::try_from(processing_audio_buffer_peak_bytes).map_err(|_| invalid())?,
            )?,
        ))
    }
}

fn invalid() -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(
        "native lane layout is not wire/address-space representable".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_lane_wire_preserves_independent_persistent_input_and_rejects_legacy_shape() {
        let wire = NativeWorkerLaneWire::Scoped {
            owners: [(0, 1); 8],
            control: [(0, 1); 3],
            processor_input_storage: [24, 64],
            adapter_note_input_storage: (0, 1),
            processing_audio_buffer_peak_bytes: 0,
        };
        let profile = wire.clone().realize().unwrap().unwrap();
        assert_eq!(profile.processor_input_storage(), (24, 64));
        assert_eq!(profile.known_storage_limit_bytes(), 88);
        assert_eq!(NativeWorkerLaneWire::capture(Some(profile)).unwrap(), wire);
        let mut legacy = serde_json::to_value(&wire).unwrap();
        legacy.as_object_mut().unwrap().remove("processor_input_storage");
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(legacy).is_err());
        assert!(NativeWorkerLaneWire::Scoped {
            owners: [(0, 1); 8],
            control: [(0, 1); 3],
            processor_input_storage: [u64::MAX, 1],
            adapter_note_input_storage: (0, 1),
            processing_audio_buffer_peak_bytes: 0,
        }
        .realize()
        .is_err());
        assert_eq!((NativeWorkerLaneWire::Inline {}).realize().unwrap(), None);
    }

    #[test]
    fn native_lane_wire_resident_owner_requires_eight_layouts_and_refuses_old_peers() {
        // Frozen IPC12 profile shape; neither side may silently omit the new owner.
        #[derive(Serialize, Deserialize)]
        #[serde(tag = "kind", deny_unknown_fields)]
        enum LegacyWire {
            Scoped {
                owners: [(u64, u64); 7],
                control: [(u64, u64); 3],
                processor_input_storage: [u64; 2],
            },
        }
        let old = serde_json::to_value(LegacyWire::Scoped {
            owners: [(0, 1); 7],
            control: [(0, 1); 3],
            processor_input_storage: [0, 0],
        })
        .unwrap();
        assert!(serde_json::from_value::<LegacyWire>(old.clone()).is_ok());
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(old).is_err());
        let mut owners = [(0, 1); 8];
        owners[0] = (1032, 8);
        let wire = NativeWorkerLaneWire::Scoped {
            owners,
            control: [(0, 1); 3],
            processor_input_storage: [0, 0],
            adapter_note_input_storage: (0, 1),
            processing_audio_buffer_peak_bytes: 0,
        };
        let profile = wire.clone().realize().unwrap().unwrap();
        assert_eq!(
            profile.owning_layouts().adapter_resident,
            NativeHostBufferLayout { bytes: 1032, alignment: 8 }
        );
        assert_eq!(profile.known_storage_limit_bytes(), 1032);
        assert_eq!(NativeWorkerLaneWire::capture(Some(profile)).unwrap(), wire);
        let current = serde_json::to_value(wire).unwrap();
        assert!(serde_json::from_value::<LegacyWire>(current.clone()).is_err());
        let mut extra = current.clone();
        extra["owners"].as_array_mut().unwrap().push(serde_json::json!([0, 1]));
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(extra).is_err());
        let mut unknown = current;
        unknown["unaccounted_owner"] = serde_json::json!([1, 1]);
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(unknown).is_err());
        let inline = serde_json::json!({ "kind": "Inline" });
        assert_eq!(
            serde_json::to_value(NativeWorkerLaneWire::capture(None).unwrap()).unwrap(),
            inline
        );
        assert_eq!(
            serde_json::from_value::<NativeWorkerLaneWire>(inline.clone())
                .unwrap()
                .realize()
                .unwrap(),
            None
        );
        for (name, value) in [
            ("owners", serde_json::to_value([(0_u64, 1_u64); 8]).unwrap()),
            ("unaccounted_owner", serde_json::json!([1, 1])),
        ] {
            let mut unknown = inline.clone();
            unknown[name] = value;
            assert!(serde_json::from_value::<NativeWorkerLaneWire>(unknown).is_err());
        }
    }
}

#[cfg(test)]
mod note_backing_tests {
    use super::*;

    #[test]
    fn native_lane_processing_buffer_peak_roundtrips_and_refuses_missing_or_overflow() {
        let original = NativeWorkerLaneWire::Scoped {
            owners: [(0, 1); 8],
            control: [(0, 1); 3],
            processor_input_storage: [24, 64],
            adapter_note_input_storage: (16, 8),
            processing_audio_buffer_peak_bytes: 4096,
        };
        let profile = original.clone().realize().unwrap().unwrap();
        assert_eq!(profile.processing_audio_buffer_peak_bytes(), 4096);
        assert_eq!(profile.known_storage_limit_bytes(), 24 + 64 + 16 + 4096);
        assert_eq!(
            NativeWorkerLaneWire::capture(Some(profile)).unwrap(),
            original
        );
        let current = serde_json::to_value(&original).unwrap();
        let mut old_shape = current.clone();
        old_shape.as_object_mut().unwrap().remove("processing_audio_buffer_peak_bytes");
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(old_shape).is_err());
        let mut overflow = current;
        overflow["processing_audio_buffer_peak_bytes"] = serde_json::json!(u64::MAX);
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(overflow)
            .unwrap()
            .realize()
            .is_err());
        let mut unaccounted_inline = serde_json::json!({ "kind": "Inline" });
        unaccounted_inline["processing_audio_buffer_peak_bytes"] = serde_json::json!(4096);
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(unaccounted_inline).is_err());
    }

    #[test]
    fn native_lane_note_backing_requires_new_shape_and_retains_typed_zero_alignment() {
        let alignment = std::mem::align_of::<vst3_host::PluginEvent>() as u64;
        let wire = NativeWorkerLaneWire::Scoped {
            owners: [(0, 1); 8],
            control: [(0, 1); 3],
            processor_input_storage: [0, 0],
            adapter_note_input_storage: (0, alignment),
            processing_audio_buffer_peak_bytes: 0,
        };
        let profile = wire.clone().realize().unwrap().unwrap();
        assert_eq!(
            profile.adapter_note_input_storage(),
            NativeHostBufferLayout { bytes: 0, alignment: alignment as usize }
        );
        assert_eq!(profile.known_storage_limit_bytes(), 0);
        assert_eq!(NativeWorkerLaneWire::capture(Some(profile)).unwrap(), wire);
        let current = serde_json::to_value(&wire).unwrap();
        let mut legacy = current.clone();
        legacy.as_object_mut().unwrap().remove("adapter_note_input_storage");
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(legacy.clone()).is_err());

        #[derive(Serialize, Deserialize)]
        #[serde(tag = "kind", deny_unknown_fields)]
        enum V15 {
            Scoped {
                owners: [(u64, u64); 8],
                control: [(u64, u64); 3],
                processor_input_storage: [u64; 2],
            },
        }
        // Historical bytes contain neither the later Note backing field nor
        // the processing peak. Keep both old/current decoder assertions.
        legacy.as_object_mut().unwrap().remove("processing_audio_buffer_peak_bytes");
        assert!(serde_json::from_value::<NativeWorkerLaneWire>(legacy.clone()).is_err());
        assert!(serde_json::from_value::<V15>(legacy).is_ok());
        assert!(serde_json::from_value::<V15>(current).is_err());
        let invalid = NativeWorkerLaneWire::Scoped {
            owners: [(0, 1); 8],
            control: [(0, 1); 3],
            processor_input_storage: [0, 0],
            adapter_note_input_storage: (0, 0),
            processing_audio_buffer_peak_bytes: 0,
        };
        assert!(invalid.realize().is_err());
    }
}
