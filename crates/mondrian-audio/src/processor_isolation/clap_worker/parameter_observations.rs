//! Main-thread parameter facts over the same native observation result.
//! Queries never create host automation or substitute a schema default.

use super::super::clap_parameter_prefix::{ClapParameterRecord, PreparedClapParameterPrefix};
use super::super::clap_parameter_source::ClapParameterControlKind;
use super::*;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use mondrian_core::{
    NativeAudioParameterFact as Fact, NativeAudioParameterObservationOrigin as Origin,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ParameterSignature {
    id: u32,
    critical_flags: u32,
    min_bits: u64,
    max_bits: u64,
    default_bits: u64,
}

fn signature(info: &clack_extensions::params::ParamInfo<'_>) -> ParameterSignature {
    ParameterSignature {
        id: info.id.get(),
        critical_flags: info.flags.bits()
            & !(ParamInfoFlags::IS_PERIODIC | ParamInfoFlags::IS_HIDDEN).bits(),
        min_bits: info.min_value.to_bits(),
        max_bits: info.max_value.to_bits(),
        default_bits: info.default_value.to_bits(),
    }
}

impl ParameterSignature {
    /// Reuse the admitted native processing schema; presentation-only bits may change.
    pub(super) fn matches(&self, info: &clack_extensions::params::ParamInfo<'_>) -> bool {
        info.flags.bits() & !ParamInfoFlags::all().bits() == 0 && *self == signature(info)
    }

    pub(super) const fn id(self) -> u32 {
        self.id
    }
}

pub(super) fn capture_signatures(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
) -> Result<Vec<ParameterSignature>, AudioProcessorHostError> {
    let Some(params) = handle.get_extension::<PluginParams>() else {
        return Ok(Vec::new());
    };
    let count = params.count(handle) as usize;
    if count > super::super::clap_discovery::MAX_PARAMETERS as usize {
        return Err(invalid(
            "CLAP actual parameter signature count exceeds its bound",
        ));
    }
    let mut signatures = Vec::new();
    signatures
        .try_reserve_exact(count)
        .map_err(|_| invalid("CLAP parameter signature storage unavailable"))?;
    if signatures.capacity() > super::super::clap_discovery::MAX_PARAMETERS as usize {
        return Err(invalid(
            "CLAP parameter signature capacity exceeds its bound",
        ));
    }
    for index in 0..count {
        let mut buffer = ParamInfoBuffer::new();
        let info = params
            .get_info(handle, index as u32, &mut buffer)
            .ok_or_else(|| invalid("CLAP actual parameter info unavailable"))?;
        if info.flags.bits() & !ParamInfoFlags::all().bits() != 0 {
            return Err(invalid(
                "CLAP actual parameter info contains unknown flag bits",
            ));
        }
        signatures.push(signature(&info));
    }
    Ok(signatures)
}

// CLAP plain values already use the canonical scalar domain. A current
// descriptor must still admit the exact value; no clamp, rounding or fallback
// turns an unavailable or invalid native query into an author observation.
fn author_value_bits(
    value: Option<f64>,
    minimum: f64,
    maximum: f64,
    flags: ParamInfoFlags,
) -> Option<u64> {
    if flags.contains(ParamInfoFlags::IS_READONLY)
        || !minimum.is_finite()
        || !maximum.is_finite()
        || minimum > maximum
    {
        return None;
    }
    value
        .filter(|value| {
            value.is_finite()
                && *value >= minimum
                && *value <= maximum
                && (!flags.contains(ParamInfoFlags::IS_STEPPED)
                    || (value.fract() == 0.0
                        && value.abs() <= (1_u64 << f64::MANTISSA_DIGITS) as f64))
        })
        .map(f64::to_bits)
}

fn output_fact(
    output: super::super::clap_output_events::ClapOutputParameterEvent,
) -> Result<Fact, AudioProcessorHostError> {
    use super::super::clap_output_events::{ClapOutputOrigin, ClapOutputParameterKind};
    let origin = match output.origin {
        ClapOutputOrigin::Process { start_sample, sample_rate, frames } => Origin::Process {
            start_sample,
            sample_rate,
            frames,
            offset: output.sample_offset,
        },
        ClapOutputOrigin::InactiveFlush => {
            Origin::InactiveFlush { raw_offset: output.sample_offset }
        }
        ClapOutputOrigin::ActiveFlush => Origin::ActiveFlush { raw_offset: output.sample_offset },
    };
    let value_bits = match output.kind {
        ClapOutputParameterKind::Value(value) => Some(value.to_bits()),
        _ => None,
    };
    Ok(Fact {
        revision: output.sequence,
        parameter: mondrian_core::ParameterId::new(format!("clap.param.{}", output.parameter))
            .map_err(|_| invalid("CLAP output stable parameter identity unavailable"))?,
        event_flags: output.flags,
        value_bits,
        author_value_bits: None,
        origin,
        hidden: None,
        periodic: None,
        name: None,
        module: None,
        info_flags: None,
        display: None,
    })
}

impl ClapWorker {
    /// Full main owner after the same DSP role joined. Every requested scan is
    /// actually executed; its flags/revision remain attached to the result.
    pub(super) fn prepare_parameter_observations(
        &mut self,
        prefix: Option<&PreparedClapParameterPrefix>,
    ) -> Result<Vec<Fact>, AudioProcessorHostError> {
        let before = self.shared.parameter_source.snapshot();
        if before.publishing_output || before.incomplete != 0 {
            return Err(AudioProcessorHostError::NativeParameterSourceUnavailable);
        }
        // Native callbacks are the original invalidation source. Retain every
        // requested rescan/accepted callback; skip only an empty source whose
        // actual revision was already queried on this same object. No fresh
        // current-value observation or author permission is manufactured.
        if prefix.is_none() && self.parameter_observation_revision == Some(before.revision) {
            return Ok(Vec::new());
        }
        let mut facts = Vec::new();
        let mut rescans = 0u32;
        let mut query_revision = before.revision;
        if let Some(prefix) = prefix {
            // No callback is cleared while allocating its actual presentation
            // owner. Recordable Values are included too, without a second write.
            facts
                .try_reserve_exact(prefix.records.len())
                .map_err(|_| invalid("CLAP observation record allocation unavailable"))?;
            for record in &prefix.records {
                match *record {
                    ClapParameterRecord::Output(output) => facts.push(output_fact(output)?),
                    ClapParameterRecord::Control(control) => {
                        if let ClapParameterControlKind::Rescan { flags } = control.kind {
                            if flags & !15 != 0 {
                                return Err(
                                    AudioProcessorHostError::NativeParameterSourceUnavailable,
                                );
                            }
                            rescans |= flags;
                            query_revision = query_revision.max(control.sequence);
                        }
                    }
                }
            }
        }
        // Current plain values are a main query, explicitly not Process events,
        // defaults, author values or a capture of private DSP state.
        let handle = self.instance.plugin_handle();
        let Some(params) = handle.get_extension::<PluginParams>() else {
            if !facts.is_empty() || rescans != 0 {
                return Err(AudioProcessorHostError::NativeParameterSourceUnavailable);
            }
            return Ok(facts);
        };
        let count = params.count(&handle) as usize;
        if count != self.parameter_signatures.len() {
            return Err(AudioProcessorHostError::NativeParameterSourceUnavailable);
        }
        let reserve = facts
            .len()
            .checked_add(count)
            .ok_or_else(|| invalid("CLAP observation count overflow"))?;
        if reserve > mondrian_core::NATIVE_AUDIO_PARAMETER_FACTS {
            return Err(AudioProcessorHostError::NativeParameterSourceUnavailable);
        }
        facts
            .try_reserve_exact(count)
            .map_err(|_| invalid("CLAP observation query allocation unavailable"))?;
        for index in 0..count {
            let mut buffer = ParamInfoBuffer::new();
            let info = params
                .get_info(&handle, index as u32, &mut buffer)
                .ok_or(AudioProcessorHostError::NativeParameterSourceUnavailable)?;
            let expected = self.parameter_signatures[index];
            // Cookie invalidation after ALL is safe because all production
            // input events carry no cached native cookie. Critical schema
            // changes require a real canonical candidate, retained unacknowledged.
            if !expected.matches(&info) {
                return Err(AudioProcessorHostError::NativeParameterSourceUnavailable);
            }
            let id = info.id;
            // Every current-value query carries the actual current metadata.
            // Replacing an ephemeral observation must not erase an earlier INFO
            // result or restore stale discovery captions/visibility.
            let name = Some(
                std::str::from_utf8(info.name)
                    .map_err(|_| invalid("CLAP INFO caption is not UTF-8"))?
                    .to_owned(),
            );
            let module = Some(
                std::str::from_utf8(info.module)
                    .map_err(|_| invalid("CLAP INFO module is not UTF-8"))?
                    .to_owned(),
            );
            let info_flags = Some(info.flags.bits());
            let value = params.get_value(&handle, id).filter(|value| value.is_finite());
            let display = value.and_then(|value| {
                let mut buffer = [0u8; 1024];
                params
                    .value_to_text(&handle, id, value, &mut buffer)
                    .ok()
                    .filter(|bytes| bytes.len() < 1024)
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .map(str::to_owned)
            });
            facts.push(Fact {
                revision: query_revision,
                parameter: mondrian_core::ParameterId::new(format!("clap.param.{}", id.get()))
                    .map_err(|_| invalid("CLAP current-value identity is invalid"))?,
                event_flags: 0,
                value_bits: value.map(f64::to_bits),
                author_value_bits: author_value_bits(
                    value,
                    info.min_value,
                    info.max_value,
                    info.flags,
                ),
                origin: Origin::MainQuery { rescan_flags: rescans },
                hidden: Some(info.flags.contains(ParamInfoFlags::IS_HIDDEN)),
                periodic: Some(info.flags.contains(ParamInfoFlags::IS_PERIODIC)),
                name,
                module,
                info_flags,
                display,
            });
        }
        mondrian_core::NativeAudioParameterObservation::validate_facts(&facts)
            .map_err(|_| AudioProcessorHostError::NativeParameterSourceUnavailable)?;
        if self.shared.parameter_source.snapshot() != before {
            return Err(AudioProcessorHostError::NativeParameterSourceUnavailable);
        }
        self.parameter_observation_revision = Some(before.revision);
        Ok(facts)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::clap_output_events::{
        ClapOutputOrigin, ClapOutputParameterEvent, ClapOutputParameterKind,
    };
    use super::*;

    #[test]
    fn clap_main_query_canonical_scalar_keeps_exact_plain_units_and_refuses_invalid_projection() {
        let continuous = ParamInfoFlags::empty();
        assert_eq!(
            author_value_bits(Some(250.0), 20.0, 20_000.0, continuous),
            Some(250.0_f64.to_bits())
        );
        assert_eq!(author_value_bits(None, 20.0, 20_000.0, continuous), None);
        for value in [f64::NAN, f64::INFINITY, 19.0, 20_001.0] {
            assert_eq!(
                author_value_bits(Some(value), 20.0, 20_000.0, continuous),
                None
            );
        }
        assert_eq!(
            author_value_bits(Some(250.0), 20.0, 20_000.0, ParamInfoFlags::IS_READONLY),
            None
        );
        let stepped = ParamInfoFlags::IS_STEPPED;
        assert_eq!(
            author_value_bits(Some(2.0), -4.0, 4.0, stepped),
            Some(2.0_f64.to_bits())
        );
        assert_eq!(author_value_bits(Some(2.25), -4.0, 4.0, stepped), None);
        assert_eq!(
            author_value_bits(Some(9_007_199_254_740_994.0), 0.0, f64::MAX, stepped),
            None
        );
        for (minimum, maximum) in [(f64::NAN, 4.0), (-4.0, f64::INFINITY), (4.0, -4.0)] {
            assert_eq!(
                author_value_bits(Some(2.0), minimum, maximum, stepped),
                None
            );
        }
    }

    #[test]
    fn clap_raw_output_facts_keep_original_flags_time_and_plain_without_author_projection() {
        for kind in [
            ClapOutputParameterKind::Value(0.75),
            ClapOutputParameterKind::GestureBegin,
            ClapOutputParameterKind::GestureEnd,
        ] {
            let actual = output_fact(ClapOutputParameterEvent {
                sequence: 3,
                origin: ClapOutputOrigin::Process {
                    start_sample: -7,
                    sample_rate: 48000,
                    frames: 4,
                },
                parameter: 17,
                sample_offset: 3,
                flags: 0x8000_0000,
                read_only: false,
                kind,
            })
            .unwrap();
            actual.validate().unwrap();
            assert_eq!(actual.author_value_bits, None);
            assert_eq!(
                actual.value_bits,
                match kind {
                    ClapOutputParameterKind::Value(value) => Some(value.to_bits()),
                    _ => None,
                }
            );
            assert_eq!(actual.event_flags, 0x8000_0000);
            assert_eq!(
                actual.origin,
                Origin::Process {
                    start_sample: -7,
                    sample_rate: 48000,
                    frames: 4,
                    offset: 3,
                }
            );
        }
    }
}
