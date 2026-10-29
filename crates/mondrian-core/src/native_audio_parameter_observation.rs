//! Native parameter presentation facts on the existing observation completion.
//! They are neither automation, persisted author state nor installed permission.

use crate::{NativeAudioEditorTarget, ParameterId};

/// Maximum facts in a single bounded main/control observation reply.
pub const NATIVE_AUDIO_PARAMETER_FACTS: usize = 1152;
/// Maximum aggregate owned bytes of the fact vector and its strings.
pub const NATIVE_AUDIO_PARAMETER_FACT_BYTES: usize = 256 * 1024;

/// One explicit presentation query for an existing installed object and schema ID.
/// This control request grants neither an owner lease nor author permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioParameterQuery {
    /// Exact current installed address; the Runtime revalidates its own lease.
    pub target: NativeAudioEditorTarget,
    /// One canonical writable ID from that occurrence's existing processing plan.
    pub parameter: ParameterId,
}

/// A safe presentation refusal leaves the original native owner and callbacks intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioParameterQueryRefusal {
    /// This adapter has no explicit parameter query implementation.
    UnsupportedAdapter,
    /// The requested occurrence is not currently installed or available.
    Unavailable,
    /// The actual installed binding or native object no longer matches.
    OwnerChanged,
    /// Original author, configuration or native cleanup work must finish first.
    Busy,
    /// The selected ID or its coherent current presentation is unavailable.
    ParameterUnavailable,
}

/// Result of one explicit control query, never a parameter transaction or cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAudioParameterQueryOutcome {
    /// Exactly one current fact for the requested installed target and ID.
    /// The parent control completion owns this allocation in addition to the
    /// bounded fact vector and strings; it is not part of the child lane fee.
    Observed(Box<NativeAudioParameterObservation>),
    /// Safe control refusal without substituting a value or descriptor default.
    Refused(NativeAudioParameterQueryRefusal),
}
/// Actual origin of a parameter observation; flush has no sample clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioParameterObservationOrigin {
    /// Actual signal grid before the original callback offset; not UI edit time.
    Process {
        /// Signed signal sample position after the sole PDC calculation.
        start_sample: i64,
        /// Actual sample rate of the native call.
        sample_rate: u32,
        /// Actual frame extent of that call.
        frames: u32,
        /// Original callback offset within the native call.
        offset: u32,
    },
    /// A flush on the actual inactive main role; its raw offset is not a clock.
    InactiveFlush {
        /// Original raw offset, explicitly without sample timing.
        raw_offset: u32,
    },
    /// A flush on the actual active audio role; its raw offset is not a clock.
    ActiveFlush {
        /// Original raw offset, explicitly without sample timing.
        raw_offset: u32,
    },
    /// An actual main-thread get_value/get_info/value_to_text query.
    MainQuery {
        /// The original rescan mask, or zero for a current-value query.
        rescan_flags: u32,
    },
}

/// One bounded parameter presentation record in source/query order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioParameterFact {
    /// Original callback/control revision, or current revision for a main query.
    pub revision: u64,
    /// Stable schema identity translated from the original native parameter ID.
    pub parameter: ParameterId,
    /// Original native output event flag bits; unknown bits remain observable.
    pub event_flags: u32,
    /// Exact optional finite plain value; no descriptor default is substituted.
    pub value_bits: Option<u64>,
    /// Optional finite scalar in the current descriptor's canonical author domain,
    /// projected from this same successful main query. This ephemeral observation
    /// grants no author permission and never substitutes a descriptor default.
    pub author_value_bits: Option<u64>,
    /// Original origin and sample/flush evidence.
    pub origin: NativeAudioParameterObservationOrigin,
    /// Actual queried UI visibility, independent of persisted schema metadata.
    pub hidden: Option<bool>,
    /// Actual periodic/meter presentation hint, never an author write request.
    pub periodic: Option<bool>,
    /// Actual current INFO caption, absent on raw output-only records.
    pub name: Option<String>,
    /// Actual current INFO module path, absent on raw output-only records.
    pub module: Option<String>,
    /// Original current INFO parameter flags, absent on raw output-only records.
    pub info_flags: Option<u32>,
    /// Actual value_to_text result. None is explicitly unavailable.
    pub display: Option<String>,
}

impl NativeAudioParameterFact {
    /// Shape admission only; callers must still compare the actual installed owner.
    pub fn validate(&self) -> Result<(), NativeAudioParameterObservationError> {
        if self.parameter.storage_owner().capacity() > 255
            || self.value_bits.is_some_and(|bits| !f64::from_bits(bits).is_finite())
            || self.author_value_bits.is_some_and(|bits| {
                !f64::from_bits(bits).is_finite()
                    || !matches!(
                        self.origin,
                        NativeAudioParameterObservationOrigin::MainQuery { .. }
                    )
            })
            || self.name.as_ref().is_some_and(|text| text.len() > 255 || text.contains('\0'))
            || self
                .module
                .as_ref()
                .is_some_and(|text| text.len() > 1023 || text.contains('\0'))
            || self
                .display
                .as_ref()
                .is_some_and(|text| text.len() > 1023 || text.contains('\0'))
            || match self.origin {
                NativeAudioParameterObservationOrigin::Process {
                    start_sample,
                    sample_rate,
                    frames,
                    offset,
                } => {
                    sample_rate == 0
                        || offset >= frames
                        || start_sample.checked_add(i64::from(offset)).is_none()
                }
                NativeAudioParameterObservationOrigin::MainQuery { rescan_flags } => {
                    rescan_flags & !15 != 0
                }
                _ => false,
            }
        {
            return Err(NativeAudioParameterObservationError::InvalidFact);
        }
        Ok(())
    }
    /// Actual string allocation capacities, separately from inline vector storage.
    pub fn string_capacity_bytes(&self) -> Result<usize, NativeAudioParameterObservationError> {
        [
            Some(self.parameter.storage_owner().capacity()),
            self.name.as_ref().map(String::capacity),
            self.module.as_ref().map(String::capacity),
            self.display.as_ref().map(String::capacity),
        ]
        .into_iter()
        .flatten()
        .try_fold(0usize, |sum, bytes| {
            sum.checked_add(bytes).ok_or(NativeAudioParameterObservationError::Extent)
        })
    }
}

/// Bounded shape/resource refusal; no value or unknown flag is fabricated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeAudioParameterObservationError {
    /// A record carries inadmissible bytes, non-finite value or impossible origin.
    #[error("native parameter presentation fact is invalid")]
    InvalidFact,
    /// Aggregate actual owned vector/string storage exceeds this response bound.
    #[error("native parameter observation storage exceeds its bound")]
    Extent,
}

/// Same-owner presentation facts retained only by the current UI execution context.
/// Public DTO construction does not grant an author, runtime or child lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioParameterObservation {
    target: NativeAudioEditorTarget,
    process_id: std::num::NonZeroU32,
    object_serial: std::num::NonZeroU64,
    source_revision: u64,
    facts: Vec<NativeAudioParameterFact>,
}

impl NativeAudioParameterObservation {
    /// Project an already authenticated installed object; shape checks are separate
    /// from Runtime authority and cannot turn a copied target into permission.
    pub fn project(
        target: NativeAudioEditorTarget,
        process_id: std::num::NonZeroU32,
        object_serial: std::num::NonZeroU64,
        source_revision: u64,
        facts: Vec<NativeAudioParameterFact>,
    ) -> Result<Self, NativeAudioParameterObservationError> {
        Self::validate_facts(&facts)?;
        if facts.iter().any(|fact| fact.revision > source_revision) {
            return Err(NativeAudioParameterObservationError::InvalidFact);
        }
        Ok(Self {
            target,
            process_id,
            object_serial,
            source_revision,
            facts,
        })
    }
    /// The exact installed root/local occurrence/binding at Runtime projection.
    pub const fn target(&self) -> NativeAudioEditorTarget {
        self.target
    }
    /// Actual authenticated child process identity.
    pub const fn process_id(&self) -> std::num::NonZeroU32 {
        self.process_id
    }
    /// Actual process-local native load incarnation, not a definition/class ID.
    pub const fn object_serial(&self) -> std::num::NonZeroU64 {
        self.object_serial
    }
    /// Actual coherent native source revision when these records were observed.
    pub const fn source_revision(&self) -> u64 {
        self.source_revision
    }
    /// Exact retained source/query order. Values do not write host curves.
    pub fn facts(&self) -> &[NativeAudioParameterFact] {
        &self.facts
    }
    /// Check actual owned capacities before native response or Core publication.
    pub fn validate_facts(
        facts: &Vec<NativeAudioParameterFact>,
    ) -> Result<usize, NativeAudioParameterObservationError> {
        if facts.capacity() > NATIVE_AUDIO_PARAMETER_FACTS {
            return Err(NativeAudioParameterObservationError::Extent);
        }
        let storage = std::alloc::Layout::array::<NativeAudioParameterFact>(facts.capacity())
            .map_err(|_| NativeAudioParameterObservationError::Extent)?
            .size();
        let total = facts.iter().try_fold(storage, |total, fact| {
            fact.validate()?;
            total
                .checked_add(fact.string_capacity_bytes()?)
                .ok_or(NativeAudioParameterObservationError::Extent)
        })?;
        if total > NATIVE_AUDIO_PARAMETER_FACT_BYTES {
            return Err(NativeAudioParameterObservationError::Extent);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact() -> NativeAudioParameterFact {
        NativeAudioParameterFact {
            revision: 3,
            parameter: ParameterId::new("clap.param.17").unwrap(),
            event_flags: 0,
            value_bits: Some(250.0_f64.to_bits()),
            author_value_bits: Some(0.25_f64.to_bits()),
            origin: NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 },
            hidden: None,
            periodic: None,
            name: None,
            module: None,
            info_flags: None,
            display: None,
        }
    }

    #[test]
    fn canonical_observation_is_finite_main_query_data_without_changing_plain_facts() {
        let mut actual = fact();
        actual.validate().unwrap();
        assert_ne!(actual.value_bits, actual.author_value_bits);
        actual.value_bits = None;
        actual.validate().unwrap();
        actual.author_value_bits = Some(f64::INFINITY.to_bits());
        assert_eq!(
            actual.validate(),
            Err(NativeAudioParameterObservationError::InvalidFact)
        );
        for origin in [
            NativeAudioParameterObservationOrigin::Process {
                start_sample: -4,
                sample_rate: 48000,
                frames: 4,
                offset: 3,
            },
            NativeAudioParameterObservationOrigin::InactiveFlush { raw_offset: 77 },
            NativeAudioParameterObservationOrigin::ActiveFlush { raw_offset: 77 },
        ] {
            actual = fact();
            actual.origin = origin;
            assert_eq!(
                actual.validate(),
                Err(NativeAudioParameterObservationError::InvalidFact)
            );
            actual.author_value_bits = None;
            actual.validate().unwrap();
            assert_eq!(actual.value_bits, Some(250.0_f64.to_bits()));
        }
    }

    #[test]
    fn canonical_observation_uses_the_complete_actual_fact_layout_and_original_storage_bound() {
        let mut facts = Vec::with_capacity(3);
        facts.push(fact());
        let inline = std::alloc::Layout::array::<NativeAudioParameterFact>(facts.capacity())
            .unwrap()
            .size();
        let expected = inline + facts[0].string_capacity_bytes().unwrap();
        assert_eq!(
            NativeAudioParameterObservation::validate_facts(&facts),
            Ok(expected)
        );
        let mut large = fact();
        large.module = Some("m".repeat(1023));
        large.display = Some("d".repeat(1023));
        large.validate().unwrap();
        assert_eq!(
            NativeAudioParameterObservation::validate_facts(&vec![large; 128]),
            Err(NativeAudioParameterObservationError::Extent),
            "individually valid strings plus the complete inline layout exceed 256KiB"
        );
        assert_eq!(
            NativeAudioParameterObservation::validate_facts(&Vec::with_capacity(
                NATIVE_AUDIO_PARAMETER_FACTS + 1
            )),
            Err(NativeAudioParameterObservationError::Extent)
        );
    }
}
