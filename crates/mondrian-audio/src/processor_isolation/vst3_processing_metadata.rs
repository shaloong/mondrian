//! Expected VST3 event-bus constraints, rechecked on each stopped native owner.
//!
//! The encoded payload remains covered by the existing payload visitor/limit.
//! This inline cold metadata is not processor-input storage and does not claim
//! that the native lane profile covers every decoded worker/header allocation.

use crate::AudioProcessorHostError;
use serde::{
    de::{self, DeserializeSeed, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::fmt;
use vst3_host::{
    audio::{BusDirection, NativeEventBusRole},
    plugin::Plugin,
};

const CAPACITY: usize = 32;

/// Native event-bus purpose; names are display metadata, not identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vst3EventBusRole {
    /// Primary event stream.
    Main,
    /// Additional event stream.
    Auxiliary,
}

/// One direction-local native event-bus constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vst3EventBusContractRow {
    /// Position in the native direction's bus array, never a synthetic port ID.
    pub native_index: u16,
    /// Native declared purpose.
    pub role: Vst3EventBusRole,
    /// Declared channels, including a legal zero-channel declaration.
    pub channel_count: u32,
    /// Native default-active declaration.
    pub default_active: bool,
    /// Same host owner's retained successful activateBus ACK, not GetActive.
    pub host_active: bool,
}

impl Vst3EventBusContractRow {
    const EMPTY: Self = Self {
        native_index: 0,
        role: Vst3EventBusRole::Main,
        channel_count: 0,
        default_active: false,
        host_active: false,
    };
}

/// At most 32 native rows without a persistent heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vst3EventBusContractRows {
    rows: [Vst3EventBusContractRow; CAPACITY],
    len: usize,
}

impl Vst3EventBusContractRows {
    /// Construct a bounded list; the complete contract validates index order.
    pub fn try_from_slice(
        rows: &[Vst3EventBusContractRow],
    ) -> Result<Self, AudioProcessorHostError> {
        if rows.len() > CAPACITY {
            return Err(invalid("VST3 event-bus capacity exceeded"));
        }
        let mut result = Self::empty();
        result.rows[..rows.len()].copy_from_slice(rows);
        result.len = rows.len();
        Ok(result)
    }
    const fn empty() -> Self {
        Self {
            rows: [Vst3EventBusContractRow::EMPTY; CAPACITY],
            len: 0,
        }
    }
    /// Actual rows in native order.
    pub fn as_slice(&self) -> &[Vst3EventBusContractRow] {
        &self.rows[..self.len]
    }
    /// Edit expected constraints; consumers validate the complete contract.
    pub fn as_mut_slice(&mut self) -> &mut [Vst3EventBusContractRow] {
        &mut self.rows[..self.len]
    }
    /// Number of native rows.
    pub fn len(&self) -> usize {
        self.len
    }
    /// Whether this direction has no event buses.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Serialize for Vst3EventBusContractRows {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.as_slice())
    }
}
impl<'de> Deserialize<'de> for Vst3EventBusContractRows {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Rows;
        impl<'de> Visitor<'de> for Rows {
            type Value = Vst3EventBusContractRows;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("at most 32 native event-bus rows")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut result = Vst3EventBusContractRows::empty();
                while result.len < CAPACITY {
                    let Some(row) = seq.next_element()? else {
                        return Ok(result);
                    };
                    result.rows[result.len] = row;
                    result.len += 1;
                }
                struct RejectExtra;
                impl<'de> DeserializeSeed<'de> for RejectExtra {
                    type Value = ();
                    fn deserialize<D: Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
                        Err(de::Error::custom("VST3 event-bus capacity exceeded"))
                    }
                }
                // Reject the 33rd element before decoding even its contents.
                seq.next_element_seed(RejectExtra)?;
                Ok(result)
            }
        }
        deserializer.deserialize_seq(Rows)
    }
}

/// Expected event-bus contract, not a certificate of historical state or DSP.
///
/// The outer registration/payload supplies the sole binary, class, state and
/// render identity. Changing a configuration seed preserves these constraints;
/// only a successful actual same-instance census permits preparation to proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ContractWire")]
pub struct Vst3EventBusContractV1 {
    /// Required contract version, currently one.
    pub schema_version: u32,
    /// Native input order.
    pub inputs: Vst3EventBusContractRows,
    /// Native output order.
    pub outputs: Vst3EventBusContractRows,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractWire {
    schema_version: u32,
    inputs: Vst3EventBusContractRows,
    outputs: Vst3EventBusContractRows,
}
impl TryFrom<ContractWire> for Vst3EventBusContractV1 {
    type Error = AudioProcessorHostError;
    fn try_from(wire: ContractWire) -> Result<Self, Self::Error> {
        let contract = Self {
            schema_version: wire.schema_version,
            inputs: wire.inputs,
            outputs: wire.outputs,
        };
        contract.validate()?;
        Ok(contract)
    }
}

impl Vst3EventBusContractV1 {
    /// Expected declaration for a component with no event buses.
    pub const fn empty() -> Self {
        Self {
            schema_version: 1,
            inputs: Vst3EventBusContractRows::empty(),
            outputs: Vst3EventBusContractRows::empty(),
        }
    }
    /// Validate shape without loading a plugin or changing activation.
    pub fn validate(&self) -> Result<(), AudioProcessorHostError> {
        if self.schema_version != 1 {
            return Err(invalid("unsupported VST3 event-bus contract version"));
        }
        for rows in [self.inputs.as_slice(), self.outputs.as_slice()] {
            if rows.iter().enumerate().any(|(index, row)| {
                usize::from(row.native_index) != index || row.channel_count > i32::MAX as u32
            }) {
                return Err(invalid(
                    "VST3 event-bus indices or channel count are invalid",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn observe(plugin: &Plugin) -> Result<Self, AudioProcessorHostError> {
        let mut result = Self::empty();
        plugin
            .visit_native_event_bus_constraints(
                &mut |direction, index, role, channels, default_active, host_active| {
                    result.push_native_row(
                        direction,
                        index,
                        role,
                        channels,
                        default_active,
                        host_active,
                    )
                },
            )
            .map_err(|error| invalid(format!("VST3 stopped event-bus query failed: {error}")))?;
        result.validate()?;
        Ok(result)
    }
    fn push_native_row(
        &mut self,
        direction: BusDirection,
        index: usize,
        role: NativeEventBusRole,
        channels: usize,
        default_active: bool,
        host_active: bool,
    ) -> vst3_host::Result<()> {
        let rows = match direction {
            BusDirection::Input => &mut self.inputs,
            BusDirection::Output => &mut self.outputs,
        };
        if index != rows.len || index >= CAPACITY || channels > i32::MAX as usize {
            return Err(vst3_host::Error::InvalidParameter(
                "VST3 event-bus constraint exceeds native order or extent".into(),
            ));
        }
        rows.rows[index] = Vst3EventBusContractRow {
            native_index: index as u16,
            role: match role {
                NativeEventBusRole::Main => Vst3EventBusRole::Main,
                NativeEventBusRole::Auxiliary => Vst3EventBusRole::Auxiliary,
            },
            channel_count: channels as u32,
            default_active,
            host_active,
        };
        rows.len += 1;
        Ok(())
    }
}
fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vst3_host::audio::{NativeEventBusInfo, NativeEventBusInfoSet};

    fn from_native(
        facts: &NativeEventBusInfoSet,
    ) -> Result<Vst3EventBusContractV1, AudioProcessorHostError> {
        let mut result = Vst3EventBusContractV1::empty();
        for (direction, rows) in [
            (BusDirection::Input, &facts.inputs),
            (BusDirection::Output, &facts.outputs),
        ] {
            for (index, row) in rows.iter().enumerate() {
                result
                    .push_native_row(
                        direction,
                        index,
                        row.role,
                        row.channel_count,
                        row.default_active,
                        row.host_active,
                    )
                    .map_err(|error| invalid(error.to_string()))?;
            }
        }
        result.validate()?;
        Ok(result)
    }

    fn native() -> NativeEventBusInfoSet {
        let main = NativeEventBusInfo {
            role: NativeEventBusRole::Main,
            name: "Display only".into(),
            channel_count: 0,
            default_active: true,
            host_active: false,
        };
        let aux = NativeEventBusInfo {
            role: NativeEventBusRole::Auxiliary,
            name: "Display only".into(),
            channel_count: 16,
            default_active: false,
            host_active: true,
        };
        NativeEventBusInfoSet {
            inputs: vec![main.clone(), aux],
            outputs: vec![main],
        }
    }

    #[test]
    fn visitor_rows_reject_order_and_extent_before_mutating_the_contract() {
        let mut contract = Vst3EventBusContractV1::empty();
        let empty = contract;
        for (index, channels) in [(1, 0), (0, i32::MAX as usize + 1)] {
            assert!(contract
                .push_native_row(
                    BusDirection::Input,
                    index,
                    NativeEventBusRole::Main,
                    channels,
                    false,
                    false
                )
                .is_err());
            assert_eq!(contract, empty);
        }
        contract
            .push_native_row(
                BusDirection::Input,
                0,
                NativeEventBusRole::Main,
                0,
                true,
                false,
            )
            .unwrap();
        let first = contract;
        assert!(contract
            .push_native_row(
                BusDirection::Input,
                0,
                NativeEventBusRole::Main,
                0,
                false,
                true
            )
            .is_err());
        assert_eq!(contract, first);
        contract
            .push_native_row(
                BusDirection::Output,
                0,
                NativeEventBusRole::Auxiliary,
                i32::MAX as usize,
                false,
                true,
            )
            .unwrap();
        for index in 1..CAPACITY {
            contract
                .push_native_row(
                    BusDirection::Input,
                    index,
                    NativeEventBusRole::Auxiliary,
                    16,
                    false,
                    false,
                )
                .unwrap();
        }
        let full = contract;
        assert!(contract
            .push_native_row(
                BusDirection::Input,
                CAPACITY,
                NativeEventBusRole::Main,
                16,
                true,
                true
            )
            .is_err());
        assert_eq!(contract, full);
        contract.validate().unwrap();
    }

    #[test]
    fn event_contract_retains_direction_order_zero_channels_and_host_ack() {
        let mut facts = native();
        let expected = from_native(&facts).unwrap();
        assert_eq!(expected.inputs.len(), 2);
        assert_eq!(expected.outputs.len(), 1);
        assert_eq!(expected.inputs.as_slice()[0].channel_count, 0);
        assert_eq!(
            expected.inputs.as_slice()[1],
            Vst3EventBusContractRow {
                native_index: 1,
                role: Vst3EventBusRole::Auxiliary,
                channel_count: 16,
                default_active: false,
                host_active: true,
            }
        );
        facts.inputs[0].name = "Different display".into();
        assert_eq!(from_native(&facts).unwrap(), expected);
        for field in 0..5 {
            let mut changed = native();
            match field {
                0 => changed.inputs.swap(0, 1),
                1 => changed.inputs[1].role = NativeEventBusRole::Main,
                2 => changed.inputs[1].channel_count = 1,
                3 => changed.inputs[1].default_active = true,
                4 => changed.inputs[1].host_active = false,
                _ => unreachable!(),
            }
            assert_ne!(from_native(&changed).unwrap(), expected);
        }
        facts.outputs.clear();
        assert_ne!(from_native(&facts).unwrap(), expected);
    }

    #[test]
    fn event_contract_wire_requires_version_rows_and_bounded_native_order() {
        let contract = from_native(&native()).unwrap();
        let wire = serde_json::to_value(contract).unwrap();
        assert_eq!(
            serde_json::from_value::<Vst3EventBusContractV1>(wire.clone()).unwrap(),
            contract
        );
        for field in ["schema_version", "inputs", "outputs"] {
            let mut missing = wire.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Vst3EventBusContractV1>(missing).is_err());
        }
        for (field, value) in [("schema_version", json!(0)), ("unknown", json!(0))] {
            let mut bad = wire.clone();
            bad[field] = value;
            assert!(serde_json::from_value::<Vst3EventBusContractV1>(bad).is_err());
        }
        for (field, value) in [
            ("native_index", json!(7)),
            ("channel_count", json!(2147483648_u32)),
            ("role", json!("unknown")),
            ("unknown", json!(false)),
        ] {
            let mut bad = wire.clone();
            bad["inputs"][0][field] = value;
            assert!(serde_json::from_value::<Vst3EventBusContractV1>(bad).is_err());
        }
        let mut rows = vec![contract.inputs.as_slice()[0]; 32];
        for (index, row) in rows.iter_mut().enumerate() {
            row.native_index = index as u16;
        }
        let mut maximum = Vst3EventBusContractV1::empty();
        maximum.inputs = Vst3EventBusContractRows::try_from_slice(&rows).unwrap();
        maximum.outputs = maximum.inputs;
        let mut wire = serde_json::to_value(maximum).unwrap();
        assert_eq!(
            serde_json::from_value::<Vst3EventBusContractV1>(wire.clone()).unwrap(),
            maximum
        );
        rows.push(rows[0]);
        assert!(Vst3EventBusContractRows::try_from_slice(&rows).is_err());
        wire["inputs"].as_array_mut().unwrap().push(json!({}));
        assert!(serde_json::from_value::<Vst3EventBusContractV1>(wire)
            .unwrap_err()
            .to_string()
            .contains("capacity exceeded"));
        assert!(
            !std::mem::needs_drop::<Vst3EventBusContractV1>(),
            "no persistent heap owners"
        );
        assert!(
            std::mem::size_of::<Vst3EventBusContractV1>() <= 1024,
            "bounded inline cold metadata; not charged to parameter-input N"
        );
    }
}
