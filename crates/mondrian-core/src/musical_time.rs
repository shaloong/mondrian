//! Exact Sequence musical reference and consumer-declared timeline activity.

use crate::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, TimelineTime,
};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// Logical timeline activity declared by the consumer of a rendered window.
///
/// This is not an audio-device state, scheduling mode, or another clock. A
/// prerendered playback/export window can be Playing while device consumption
/// is disabled. An absent observation must remain `None`, not become Stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineTransportActivity {
    /// The consumer explicitly declares a stopped timeline transport.
    Stopped,
    /// The window is rendered for an advancing timeline transport.
    Playing,
}

/// One constant quarter-note reference in the owning Sequence's time domain.
///
/// This value stores a seconds coordinate, a positive seconds duration and an
/// optional constant time signature. When supplied, bar zero starts at quarter
/// note zero. Absence never implies 4/4. This is not another playback clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MusicalTimeReference {
    quarter_note_zero: TimelineTime,
    quarter_note_duration: TimelineTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_signature: Option<MusicalTimeSignature>,
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
        Ok(Self {
            quarter_note_zero,
            quarter_note_duration,
            time_signature: None,
        })
    }

    /// Sequence-local seconds coordinate of quarter note zero.
    pub const fn quarter_note_zero(self) -> TimelineTime {
        self.quarter_note_zero
    }

    /// Strictly positive duration of one quarter note, in seconds.
    pub const fn quarter_note_duration(self) -> TimelineTime {
        self.quarter_note_duration
    }

    /// Author or remove a constant meter, anchored at this reference's zero.
    pub const fn with_time_signature(mut self, signature: Option<MusicalTimeSignature>) -> Self {
        self.time_signature = signature;
        self
    }

    /// Explicit authored meter; unknown remains absent.
    pub const fn time_signature(self) -> Option<MusicalTimeSignature> {
        self.time_signature
    }
}

/// Constant meter: `numerator` notes of length `1 / denominator` per bar.
///
/// Both scalars must be positive. Non-power-of-two denominators are retained,
/// not rounded into another meter. One bar occupies `4 * numerator / denominator`
/// quarter notes. Bar indices are zero-based and extend backwards using floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MusicalTimeSignature {
    numerator: u16,
    denominator: u16,
}

impl MusicalTimeSignature {
    /// Validate the two authored meter scalars.
    pub fn new(numerator: u16, denominator: u16) -> Result<Self, MusicalTimeReferenceError> {
        if numerator == 0 || denominator == 0 {
            return Err(MusicalTimeReferenceError::NonPositiveTimeSignature);
        }
        Ok(Self { numerator, denominator })
    }

    /// Number of denominator-sized notes per bar.
    pub const fn numerator(self) -> u16 {
        self.numerator
    }

    /// Note length denominator, in whole notes.
    pub const fn denominator(self) -> u16 {
        self.denominator
    }
}

impl<'de> Deserialize<'de> for MusicalTimeSignature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Repr {
            numerator: u16,
            denominator: u16,
        }
        let value = Repr::deserialize(deserializer)?;
        Self::new(value.numerator, value.denominator).map_err(D::Error::custom)
    }
}

impl AuthoringFootprint for MusicalTimeSignature {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { numerator, denominator } = self;
        collector.collect(numerator)?;
        collector.collect(denominator)
    }
}

/// Invalid authored musical reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MusicalTimeReferenceError {
    /// A quarter note must occupy a strictly positive seconds duration.
    #[error("quarter-note duration must be strictly positive")]
    NonPositiveQuarterNoteDuration,
    /// A meter cannot contain zero notes or a zero note-length denominator.
    #[error("time-signature numerator and denominator must be strictly positive")]
    NonPositiveTimeSignature,
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
            #[serde(default)]
            time_signature: Option<MusicalTimeSignature>,
        }
        let value = Repr::deserialize(deserializer)?;
        Self::new(value.quarter_note_zero, value.quarter_note_duration)
            .map(|reference| reference.with_time_signature(value.time_signature))
            .map_err(D::Error::custom)
    }
}

impl AuthoringFootprint for MusicalTimeReference {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        // The owning author allocation already charges these inline values.
        let Self {
            quarter_note_zero,
            quarter_note_duration,
            time_signature,
        } = self;
        collector.collect(quarter_note_zero)?;
        collector.collect(quarter_note_duration)?;
        collector.collect(time_signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_meter_is_explicit_validated_and_preserved_in_reference_identity() {
        let original =
            MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap())
                .unwrap();
        assert_eq!(original.time_signature(), None);
        assert!(serde_json::to_value(original).unwrap().get("time_signature").is_none());
        for (n, d) in [(7, 8), (5, 7), (u16::MAX, u16::MAX)] {
            let signature = MusicalTimeSignature::new(n, d).unwrap();
            let authored = original.with_time_signature(Some(signature));
            assert_ne!(authored, original);
            assert_eq!(authored.time_signature(), Some(signature));
            let json = serde_json::to_value(authored).unwrap();
            assert_eq!(
                json["time_signature"],
                serde_json::json!({"numerator":n,"denominator":d})
            );
            assert_eq!(
                serde_json::from_value::<MusicalTimeReference>(json).unwrap(),
                authored
            );
            assert_eq!(authored.with_time_signature(None), original);
        }
        for invalid in [
            serde_json::json!({"numerator":0,"denominator":4}),
            serde_json::json!({"numerator":4,"denominator":0}),
            serde_json::json!({"numerator":4,"denominator":65536}),
            serde_json::json!({"numerator":-1,"denominator":4}),
            serde_json::json!({"numerator":3}),
            serde_json::json!({"numerator":3,"denominator":4,"bar_zero":1}),
        ] {
            let mut value = serde_json::to_value(original).unwrap();
            value["time_signature"] = invalid;
            assert!(serde_json::from_value::<MusicalTimeReference>(value).is_err());
        }
        for (n, d) in [(0, 1), (1, 0), (0, 0)] {
            assert_eq!(
                MusicalTimeSignature::new(n, d),
                Err(MusicalTimeReferenceError::NonPositiveTimeSignature)
            );
        }
    }

    #[test]
    fn transport_activity_keeps_explicit_values_distinct_from_absence() {
        for (activity, spelling) in [
            (TimelineTransportActivity::Playing, "playing"),
            (TimelineTransportActivity::Stopped, "stopped"),
        ] {
            let value = serde_json::to_value(activity).unwrap();
            assert_eq!(value, spelling);
            assert_eq!(
                serde_json::from_value::<TimelineTransportActivity>(value).unwrap(),
                activity
            );
        }
        assert_eq!(
            serde_json::from_str::<Option<TimelineTransportActivity>>("null").unwrap(),
            None
        );
        assert!(serde_json::from_str::<TimelineTransportActivity>("null").is_err());
        assert!(serde_json::from_str::<TimelineTransportActivity>("\"realtime\"").is_err());
    }

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
