//! Exact Sequence-owned musical reference, expressed only in seconds.

use crate::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, TimelineTime,
};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// One constant quarter-note reference in the owning Sequence's time domain.
///
/// This value stores a seconds coordinate and a positive seconds duration. It
/// does not define a playback clock, time signature, or format-specific position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MusicalTimeReference {
    quarter_note_zero: TimelineTime,
    quarter_note_duration: TimelineTime,
}

impl MusicalTimeReference {
    /// Construct a reference whose quarter-note duration is strictly positive.
    pub fn new(
        quarter_note_zero: TimelineTime,
        quarter_note_duration: TimelineTime,
    ) -> Result<Self, MusicalTimeReferenceError> {
        if quarter_note_duration <= TimelineTime::ZERO {
            return Err(MusicalTimeReferenceError::NonPositiveQuarterNoteDuration);
        }
        Ok(Self { quarter_note_zero, quarter_note_duration })
    }

    /// Sequence-local seconds coordinate of quarter note zero.
    pub const fn quarter_note_zero(self) -> TimelineTime {
        self.quarter_note_zero
    }

    /// Strictly positive duration of one quarter note, in seconds.
    pub const fn quarter_note_duration(self) -> TimelineTime {
        self.quarter_note_duration
    }
}

/// Invalid authored musical reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MusicalTimeReferenceError {
    /// A quarter note must occupy a strictly positive seconds duration.
    #[error("quarter-note duration must be strictly positive")]
    NonPositiveQuarterNoteDuration,
}

impl<'de> Deserialize<'de> for MusicalTimeReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Repr {
            quarter_note_zero: TimelineTime,
            quarter_note_duration: TimelineTime,
        }
        let value = Repr::deserialize(deserializer)?;
        Self::new(value.quarter_note_zero, value.quarter_note_duration).map_err(D::Error::custom)
    }
}

impl AuthoringFootprint for MusicalTimeReference {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        // The owning author allocation already charges these inline values.
        let Self { quarter_note_zero, quarter_note_duration } = self;
        collector.collect(quarter_note_zero)?;
        collector.collect(quarter_note_duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musical_time_reference_requires_positive_duration_and_preserves_exact_origin() {
        let zero = TimelineTime::new(-2002, 60000).expect("negative exact origin");
        let duration = TimelineTime::new(1001, 30000).expect("fractional duration");
        let reference = MusicalTimeReference::new(zero, duration).expect("positive duration");
        assert_eq!(
            reference.quarter_note_zero(),
            TimelineTime::new(-1001, 30000).unwrap()
        );
        assert_eq!(reference.quarter_note_duration(), duration);
        let copied = reference;
        assert_eq!(copied, reference);
        for invalid in [TimelineTime::ZERO, TimelineTime::NEGATIVE_ONE] {
            assert_eq!(
                MusicalTimeReference::new(zero, invalid),
                Err(MusicalTimeReferenceError::NonPositiveQuarterNoteDuration)
            );
        }
        assert!(MusicalTimeReference::new(
            TimelineTime::new(i64::MIN, 1).unwrap(),
            TimelineTime::new(1, i64::MAX).unwrap(),
        )
        .is_ok());
    }

    #[test]
    fn musical_time_reference_serde_validates_required_exact_fields() {
        let value = serde_json::json!({
            "quarter_note_zero": {"numerator": -2002, "denominator": 60000},
            "quarter_note_duration": {"numerator": -2, "denominator": -4}
        });
        let reference: MusicalTimeReference = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            reference.quarter_note_zero(),
            TimelineTime::new(-1001, 30000).unwrap()
        );
        assert_eq!(
            reference.quarter_note_duration(),
            TimelineTime::new(1, 2).unwrap()
        );
        let canonical = serde_json::to_value(reference).unwrap();
        assert_eq!(
            canonical["quarter_note_duration"],
            serde_json::json!({"numerator": 1, "denominator": 2})
        );
        assert_eq!(
            serde_json::from_value::<MusicalTimeReference>(canonical).unwrap(),
            reference
        );
        for field in ["quarter_note_zero", "quarter_note_duration"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<MusicalTimeReference>(missing).is_err());
        }
        let mut unknown = value.clone();
        unknown["tempo"] = serde_json::json!(120);
        assert!(serde_json::from_value::<MusicalTimeReference>(unknown).is_err());
        for duration in [
            serde_json::json!({"numerator": 0, "denominator": 1}),
            serde_json::json!({"numerator": -1, "denominator": 2}),
            serde_json::json!({"numerator": 1, "denominator": 0}),
            serde_json::Value::Null,
        ] {
            let mut invalid = value.clone();
            invalid["quarter_note_duration"] = duration;
            assert!(serde_json::from_value::<MusicalTimeReference>(invalid).is_err());
        }
        let mut invalid_zero = value;
        invalid_zero["quarter_note_zero"]["denominator"] = serde_json::json!(0);
        assert!(serde_json::from_value::<MusicalTimeReference>(invalid_zero).is_err());
    }
}
