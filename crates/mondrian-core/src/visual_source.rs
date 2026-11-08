//! Authored references to picture inputs, independent of decoded raster ownership.
//!
//! A reference selects a source and a constant offset in Sequence time. Hosts
//! resolve it into the consuming graph's color domain and resolution policy;
//! decoded frames and cache revisions must never be serialized into this value.

use crate::{
    AssetId, ClipId, SequenceId, SourceSampleTarget, TimelineTime, TimelineTimeError,
    TimelineTimeRange,
};
use serde::{Deserialize, Serialize};

/// Persistent source selection for a visual Effect's auxiliary input.
///
/// The host admits Clip and Sequence dependencies in the consumer's visual
/// program before materializing their frames.
/// Unknown addresses or fields are errors, never an implicit unbound reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum VisualSourceReference {
    /// No source has been selected. The consuming Effect decides whether required.
    #[default]
    Unbound,
    /// Resolve a picture Asset through the Project's media authority.
    Asset {
        /// Stable Project Asset identity, not a file path or decoder handle.
        asset_id: AssetId,
        /// Constant Sequence-time offset and explicit source-boundary behavior.
        retime: VisualSourceRetime,
    },
    /// Read a specific picture Clip in the consumer's owning Sequence.
    Clip {
        /// Stable Clip identity, independent of its current track and position.
        clip_id: ClipId,
        /// Constant Sequence-time offset and explicit source-boundary behavior.
        retime: VisualSourceRetime,
    },
    /// Read the composited picture output of a Project Sequence.
    Sequence {
        /// Stable identity of the source Sequence.
        sequence_id: SequenceId,
        /// Constant Sequence-time offset and explicit source-boundary behavior.
        retime: VisualSourceRetime,
    },
}

impl VisualSourceReference {
    /// Shared time mapping for a bound address.
    pub const fn retime(self) -> Option<VisualSourceRetime> {
        match self {
            Self::Unbound => None,
            Self::Asset { retime, .. }
            | Self::Clip { retime, .. }
            | Self::Sequence { retime, .. } => Some(retime),
        }
    }
}

/// Behavior outside the selected source's half-open available interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VisualSourceBoundary {
    /// Request the first/last available sample at the corresponding boundary.
    #[default]
    Clamp,
    /// Extend the first/last sample. This is stateless endpoint extension,
    /// independent of playback direction or a previously displayed frame.
    Hold,
    /// Supply transparent pixels while preserving the authored reference.
    Transparent,
}

/// Static time mapping for one visual source reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualSourceRetime {
    /// Added to the consumer's exact Sequence time before media sampling.
    pub time_offset: TimelineTime,
    /// Authored response to a sample outside the source's available interval.
    pub mode: VisualSourceBoundary,
}

impl Default for VisualSourceRetime {
    fn default() -> Self {
        Self {
            time_offset: TimelineTime::ZERO,
            mode: VisualSourceBoundary::Clamp,
        }
    }
}

/// A source request retaining enough evidence for out-of-range diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VisualSourceTimeSample {
    /// Exact requested time before applying the boundary policy.
    pub requested_time: TimelineTime,
    /// None explicitly asks for transparency; it does not mean a missing Asset.
    pub source_sample: Option<SourceSampleTarget>,
    /// Whether the requested instant lies outside the available source interval.
    pub out_of_range: bool,
}

/// An authored request cannot be resolved on the source's exact time interval.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VisualSourceTimeError {
    /// Exact arithmetic failed; never round or saturate a source request.
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
    /// No sample exists to extend, even for a Clamp/Hold reference.
    #[error("visual source has no available time interval")]
    EmptySourceRange,
}

impl VisualSourceRetime {
    /// Map Sequence time onto a source interval without frame-grid quantization.
    ///
    /// The end is exclusive. Endpoint extension therefore uses the canonical
    /// strict-predecessor request, rather than inventing an epsilon or assuming
    /// a constant frame rate. Clamp and Hold share this endpoint behavior.
    pub fn sample(
        self,
        sequence_time: TimelineTime,
        available: TimelineTimeRange,
    ) -> Result<VisualSourceTimeSample, VisualSourceTimeError> {
        if available.duration <= TimelineTime::ZERO {
            return Err(VisualSourceTimeError::EmptySourceRange);
        }
        let end = available.end()?;
        let requested_time = sequence_time.checked_add(self.time_offset)?;
        let out_of_range = requested_time < available.start || requested_time >= end;
        let source_sample = if !out_of_range {
            Some(SourceSampleTarget::covering(requested_time))
        } else {
            match self.mode {
                VisualSourceBoundary::Transparent => None,
                VisualSourceBoundary::Clamp | VisualSourceBoundary::Hold => {
                    Some(if requested_time < available.start {
                        SourceSampleTarget::covering(available.start)
                    } else {
                        SourceSampleTarget::strict_predecessor(end)
                    })
                }
            }
        };
        Ok(VisualSourceTimeSample { requested_time, source_sample, out_of_range })
    }
}

impl crate::AuthoringFootprint for VisualSourceReference {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Unbound => Ok(()),
            Self::Asset { asset_id, retime } => {
                collector.collect(asset_id)?;
                collector.collect(retime)
            }
            Self::Clip { clip_id, retime } => {
                collector.collect(clip_id)?;
                collector.collect(retime)
            }
            Self::Sequence { sequence_id, retime } => {
                collector.collect(sequence_id)?;
                collector.collect(retime)
            }
        }
    }
}

impl crate::AuthoringFootprint for VisualSourceRetime {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        let Self { time_offset, mode } = self;
        collector.collect(time_offset)?;
        collector.collect(mode)
    }
}

impl crate::AuthoringFootprint for VisualSourceBoundary {
    fn collect_authoring_footprint(
        &self,
        _: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Clamp | Self::Hold | Self::Transparent => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnimatedProperty, InterpolationType, ParameterCacheImpact, ParameterId,
        ParameterInterpolation, ParameterNumericContract, ParameterSchemaError, ParameterUnit,
        PropertyDescriptor, PropertyValue, PropertyValueType,
    };

    fn tt(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).expect("exact test time")
    }

    fn range() -> TimelineTimeRange {
        TimelineTimeRange::new(tt(1, 3), tt(7, 6)).expect("available range")
    }

    fn reference() -> VisualSourceReference {
        VisualSourceReference::Asset {
            asset_id: AssetId(uuid::Uuid::from_u128(42)),
            retime: VisualSourceRetime {
                time_offset: tt(-1001, 24000),
                mode: VisualSourceBoundary::Transparent,
            },
        }
    }

    fn descriptor() -> PropertyDescriptor {
        PropertyDescriptor::new(
            "matte.source",
            "Matte source",
            PropertyValue::VisualSource(VisualSourceReference::Unbound),
        )
        .with_parameter_id(ParameterId::new("mondrian.effect.test.matte_source").unwrap())
    }

    #[test]
    fn visual_source_property_round_trips_without_losing_exact_retime() {
        let mut property = AnimatedProperty::from_descriptor(descriptor());
        property.set_static_value(PropertyValue::VisualSource(reference())).unwrap();
        let encoded = serde_json::to_value(&property).unwrap();
        let decoded: AnimatedProperty = serde_json::from_value(encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, property);
        assert_eq!(
            decoded.evaluate(tt(250, 3)),
            PropertyValue::VisualSource(reference())
        );
        assert_eq!(decoded.channel_count(), 0);
        assert!(decoded.channels().is_empty());
        assert_eq!(decoded.value_type(), PropertyValueType::VisualSource);
        assert_eq!(decoded.static_value().kind(), "visual_source");
    }

    #[test]
    fn clip_and_sequence_sources_persist_their_distinct_addresses_and_exact_retime() {
        let retime = reference().retime().unwrap();
        let id = uuid::Uuid::from_u128(42);
        let clip = VisualSourceReference::Clip { clip_id: ClipId(id), retime };
        let sequence = VisualSourceReference::Sequence { sequence_id: SequenceId(id), retime };
        let asset = reference();
        for (tag, address) in [("asset", asset), ("clip", clip), ("sequence", sequence)] {
            let encoded = serde_json::to_value(address).unwrap();
            assert!(encoded.get(tag).is_some());
            assert_eq!(
                serde_json::from_value::<VisualSourceReference>(encoded).unwrap(),
                address
            );
            assert_eq!(address.retime(), Some(retime));
        }
        assert_eq!(
            std::collections::HashSet::from([asset, clip, sequence]).len(),
            3
        );
        assert_eq!(VisualSourceReference::Unbound.retime(), None);
    }

    #[test]
    fn visual_source_schema_requires_static_topology_invalidation() {
        let schema = descriptor().schema;
        schema.validate().unwrap();
        assert_eq!(
            schema.allowed_interpolations,
            [ParameterInterpolation::Hold]
        );
        assert_eq!(schema.cache_impact, ParameterCacheImpact::Topology);
        assert!(!schema.is_animatable);
        for impact in [
            ParameterCacheImpact::None,
            ParameterCacheImpact::Value,
            ParameterCacheImpact::Resource,
        ] {
            assert_eq!(
                schema.clone().with_cache_impact(impact).validate(),
                Err(ParameterSchemaError::VisualSourceCacheImpactRequired)
            );
        }
        assert_eq!(
            schema.clone().with_animatable(true).validate(),
            Err(ParameterSchemaError::UnsupportedAnimationType)
        );
        let mut interpolated = schema.clone();
        interpolated.allowed_interpolations.push(ParameterInterpolation::Linear);
        assert_eq!(
            interpolated.validate(),
            Err(ParameterSchemaError::InterpolationTypeMismatch)
        );
        assert_eq!(
            schema
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        0.0,
                        1.0,
                        Some(1.0),
                        crate::ParameterInvalidValuePolicy::Reject
                    )
                    .unwrap()
                )
                .validate(),
            Err(ParameterSchemaError::NumericContractTypeMismatch)
        );
    }

    #[test]
    fn visual_source_cannot_be_animated_or_written_as_a_resource() {
        let mut property = AnimatedProperty::from_descriptor(descriptor());
        let before = property.clone();
        assert!(property
            .set_exact_keyframe(crate::Keyframe::from_preset(
                TimelineTime::ZERO,
                PropertyValue::VisualSource(reference()),
                InterpolationType::Hold
            ))
            .is_err());
        assert_eq!(property, before);
        assert!(property
            .set_static_value(PropertyValue::Resource(
                crate::ParameterResourceReference::ProjectAsset { asset_id: AssetId::new() }
            ))
            .is_err());
        assert_eq!(property, before);
    }

    #[test]
    fn unknown_visual_source_data_is_rejected_instead_of_becoming_unbound() {
        for value in [
            serde_json::json!({"VisualSource": "unknown"}),
            serde_json::json!({"VisualSource": {"sequence": {"sequence_id": "missing"}}}),
            serde_json::json!({"VisualSource": {"asset": {"asset_id": AssetId::new()}}}),
            serde_json::json!({"VisualSource": {"asset": {
                "asset_id": AssetId::new(), "retime": {
                    "time_offset": {"numerator": 0, "denominator": 1}, "mode": "loop"}
            }}}),
            serde_json::json!({"VisualSource": {"asset": {
                "asset_id": AssetId::new(), "retime": {
                    "time_offset": {"numerator": 1, "denominator": 0}, "mode": "clamp"}
            }}}),
        ] {
            assert!(serde_json::from_value::<PropertyValue>(value).is_err());
        }
        let mut extra = serde_json::to_value(reference()).unwrap();
        extra["asset"]["unexpected_policy"] = serde_json::json!("discard");
        assert!(serde_json::from_value::<VisualSourceReference>(extra).is_err());
        let mut extra = serde_json::to_value(reference()).unwrap();
        extra["asset"]["retime"]["rate"] = serde_json::json!(2);
        assert!(serde_json::from_value::<VisualSourceReference>(extra).is_err());
    }

    #[test]
    fn existing_property_representations_keep_their_serialized_shape() {
        for (wire, expected) in [
            (r#"{"Float":0.5}"#, PropertyValue::Float(0.5)),
            (r#"{"Text":"legacy"}"#, PropertyValue::Text("legacy".into())),
            (
                r#"{"Resource":{"kind":"unbound"}}"#,
                PropertyValue::Resource(crate::ParameterResourceReference::Unbound),
            ),
        ] {
            let decoded: PropertyValue = serde_json::from_str(wire).unwrap();
            assert_eq!(decoded, expected);
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::from_str::<serde_json::Value>(wire).unwrap()
            );
        }
        // A complete old property has no visual-source fields to migrate.
        let old = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "legacy.opacity",
            "Opacity",
            PropertyValue::Float(0.5),
        ));
        let wire = serde_json::to_string(&old).unwrap();
        assert!(!wire.contains("VisualSource"));
        let restored: AnimatedProperty = serde_json::from_str(&wire).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, old);
    }

    #[test]
    fn effect_visual_source_lookup_uses_stable_identity_and_validates_the_property() {
        let mut effect = crate::effect_data::EffectNode::new(crate::effect_data::EffectType::Fill);
        let descriptor = descriptor();
        let parameter_id = descriptor.schema.parameter_id.clone();
        effect.define_property(descriptor);
        effect
            .set_static_value_by_parameter(&parameter_id, PropertyValue::VisualSource(reference()))
            .unwrap();
        assert_eq!(
            effect.evaluate_visual_source_parameter(&parameter_id),
            Some(reference())
        );
        assert_eq!(
            effect.evaluate_visual_source_parameter(
                &ParameterId::new("mondrian.effect.test.other_source").unwrap()
            ),
            None
        );
        // An unrelated Resource address must not be accepted as a visual source.
        let other = PropertyDescriptor::new(
            "other",
            "Resource",
            PropertyValue::Resource(crate::ParameterResourceReference::Unbound),
        );
        let other_id = other.schema.parameter_id.clone();
        effect.define_property(other);
        assert_eq!(effect.evaluate_visual_source_parameter(&other_id), None);
        let mut corrupt = effect.properties.property("matte.source").unwrap().clone();
        corrupt.set_animation_enabled(true);
        effect.properties.upsert(corrupt);
        assert!(effect.validate_author_state().is_err());
        assert_eq!(effect.evaluate_visual_source_parameter(&parameter_id), None);
    }

    #[test]
    fn visual_source_time_sampling_is_exact_and_keeps_boundary_evidence() {
        let retime = VisualSourceRetime {
            time_offset: tt(-1001, 24000),
            mode: VisualSourceBoundary::Transparent,
        };
        let sample = retime.sample(tt(24001, 24000), range()).unwrap();
        assert_eq!(sample.requested_time, tt(23, 24));
        assert_eq!(
            sample.source_sample,
            Some(SourceSampleTarget::covering(tt(23, 24)))
        );
        assert!(!sample.out_of_range);
    }

    #[test]
    fn visual_source_clamp_and_hold_use_half_open_source_sampling() {
        for mode in [VisualSourceBoundary::Clamp, VisualSourceBoundary::Hold] {
            let retime = VisualSourceRetime { mode, ..VisualSourceRetime::default() };
            for (requested, expected, out_of_range) in [
                (tt(-20, 1), SourceSampleTarget::covering(tt(1, 3)), true),
                (tt(1, 3), SourceSampleTarget::covering(tt(1, 3)), false),
                (
                    tt(1499, 1000),
                    SourceSampleTarget::covering(tt(1499, 1000)),
                    false,
                ),
                (
                    tt(3, 2),
                    SourceSampleTarget::strict_predecessor(tt(3, 2)),
                    true,
                ),
                (
                    tt(200, 1),
                    SourceSampleTarget::strict_predecessor(tt(3, 2)),
                    true,
                ),
            ] {
                let sample = retime.sample(requested, range()).unwrap();
                assert_eq!(sample.requested_time, requested);
                assert_eq!(sample.source_sample, Some(expected));
                assert_eq!(sample.out_of_range, out_of_range);
            }
        }
    }

    #[test]
    fn visual_source_transparency_is_distinct_from_a_missing_reference() {
        let retime = VisualSourceRetime {
            mode: VisualSourceBoundary::Transparent,
            ..VisualSourceRetime::default()
        };
        for time in [tt(-1, 1), tt(3, 2), tt(10, 1)] {
            let sample = retime.sample(time, range()).unwrap();
            assert_eq!(sample.requested_time, time);
            assert_eq!(sample.source_sample, None);
            assert!(sample.out_of_range);
        }
        assert!(retime.sample(tt(1, 3), range()).unwrap().source_sample.is_some());
    }

    #[test]
    fn visual_source_time_arithmetic_and_empty_ranges_fail_closed() {
        let maximum = tt(i64::MAX, 1);
        let retime = VisualSourceRetime {
            time_offset: TimelineTime::ONE,
            ..VisualSourceRetime::default()
        };
        assert_eq!(
            retime.sample(maximum, range()),
            Err(VisualSourceTimeError::Time(TimelineTimeError::Overflow))
        );
        let bad_range = TimelineTimeRange::new(maximum, TimelineTime::ONE).unwrap();
        assert_eq!(
            retime.sample(TimelineTime::ZERO, bad_range),
            Err(VisualSourceTimeError::Time(TimelineTimeError::Overflow))
        );
        for duration in [TimelineTime::ZERO, TimelineTime::NEGATIVE_ONE] {
            assert_eq!(
                retime.sample(
                    TimelineTime::ZERO,
                    TimelineTimeRange { start: TimelineTime::ZERO, duration }
                ),
                Err(VisualSourceTimeError::EmptySourceRange)
            );
        }
    }

    #[test]
    fn visual_source_fixed_values_add_no_dynamic_footprint_claims() {
        let mut collector = crate::AuthoringFootprintCollector::new();
        collector.collect(&PropertyValue::VisualSource(reference())).unwrap();
        let manifest = collector.finish();
        assert_eq!(manifest.claims().count(), 0);
    }
}
