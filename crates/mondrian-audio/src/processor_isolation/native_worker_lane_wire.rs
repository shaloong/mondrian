//! Current preparation codec for the concrete native lane request.
//! This is storage metadata, never a complete Session grant or install lease.

use crate::{
    AudioProcessorHostError, NativeHostBufferLayout, NativeWorkerControlOverlapLayout,
    NativeWorkerLaneOwnedLayouts, NativeWorkerLaneResourceProfile,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum NativeWorkerLaneWire {
    Inline,
    Scoped {
        owners: [(u64, u64); 7],
        control: [(u64, u64); 3],
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
            return Ok(Self::Inline);
        };
        let owners = profile.owning_layouts();
        let control = profile.control_overlap();
        Ok(Self::Scoped {
            owners: capture_layouts([
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
        let Self::Scoped { owners, control } = self else {
            return Ok(None);
        };
        let [reader_slots, processing_slots, main_inline, runtime_main_role, runtime_dsp_role, sdk_main_role, sdk_dsp_role] =
            realize_layouts(owners)?;
        let [decoded_input, reply_storage, staged_seed] = realize_layouts(control)?;
        Ok(Some(NativeWorkerLaneResourceProfile::scoped_main_dsp(
            NativeWorkerLaneOwnedLayouts {
                reader_slots,
                processing_slots,
                main_inline,
                runtime_main_role,
                runtime_dsp_role,
                sdk_main_role,
                sdk_dsp_role,
            },
            NativeWorkerControlOverlapLayout { decoded_input, reply_storage, staged_seed },
        )?))
    }
}

fn invalid() -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(
        "native lane layout is not wire/address-space representable".to_owned(),
    )
}
