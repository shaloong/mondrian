//! Persisted pairings between positions on two closed mask contours.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Maximum authored pairs admitted by the bounded deformation solve.
pub const MAX_MASK_CORRESPONDENCE_POINTS: usize = 32;

/// Spacing from a pair to the next pair on the closed contour.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskCorrespondenceInterpolation {
    /// Use a linear mapping of distance within the interval.
    #[default]
    Linear,
    /// Use monotone cubic distance interpolation at the interval boundaries.
    Smooth,
}

/// One correspondence between source and destination mask arc lengths.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskCorrespondencePoint {
    /// Stable local identity, independent of the point's editable position.
    pub id: u32,
    /// Fraction of the source contour length in authored winding, in `[0, 1)`.
    pub source: f32,
    /// Fraction of the destination contour length in the source winding, in `[0, 1)`.
    /// Both contours start at their first authored vertex.
    pub destination: f32,
    /// Interpolation from this pair toward its next source neighbour.
    pub interpolation: MaskCorrespondenceInterpolation,
}

/// Validated author correspondence list. Empty means uniform arc-length matching.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<MaskCorrespondencePoint>")]
pub struct MaskCorrespondencePoints(Vec<MaskCorrespondencePoint>);

impl MaskCorrespondencePoints {
    /// Validate the stored identities and both cyclic coordinate sequences.
    pub fn new(points: Vec<MaskCorrespondencePoint>) -> Result<Self, MaskCorrespondenceError> {
        if points.len() > MAX_MASK_CORRESPONDENCE_POINTS {
            return Err(MaskCorrespondenceError::PointCount(points.len()));
        }
        let mut identities = HashSet::with_capacity(points.len());
        for point in &points {
            if point.id == 0 || !identities.insert(point.id) {
                return Err(MaskCorrespondenceError::InvalidIdentity(point.id));
            }
            if !(0.0..1.0).contains(&point.source) || !(0.0..1.0).contains(&point.destination) {
                return Err(MaskCorrespondenceError::InvalidPosition(point.id));
            }
        }
        let mut source_order = points.iter().collect::<Vec<_>>();
        source_order.sort_unstable_by(|a, b| a.source.total_cmp(&b.source));
        if source_order.windows(2).any(|pair| pair[0].source == pair[1].source) {
            return Err(MaskCorrespondenceError::DuplicateSource);
        }
        let mut targets = points.iter().map(|point| point.destination).collect::<Vec<_>>();
        targets.sort_unstable_by(f32::total_cmp);
        if targets.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(MaskCorrespondenceError::DuplicateDestination);
        }
        // The two coordinates use a common winding, with one wrap at the seam.
        if source_order.len() > 2 {
            let descents = source_order
                .iter()
                .zip(source_order.iter().cycle().skip(1))
                .filter(|(left, right)| left.destination > right.destination)
                .count();
            if descents != 1 {
                return Err(MaskCorrespondenceError::CrossingPairs);
            }
        }
        Ok(Self(points))
    }

    /// Points in persisted author order.
    pub fn points(&self) -> &[MaskCorrespondencePoint] {
        &self.0
    }
}

impl TryFrom<Vec<MaskCorrespondencePoint>> for MaskCorrespondencePoints {
    type Error = MaskCorrespondenceError;

    fn try_from(points: Vec<MaskCorrespondencePoint>) -> Result<Self, Self::Error> {
        Self::new(points)
    }
}

/// An invalid authored correspondence list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MaskCorrespondenceError {
    /// The list exceeds the bounded landmark budget.
    #[error("mask correspondence point count {0} exceeds {MAX_MASK_CORRESPONDENCE_POINTS}")]
    PointCount(usize),
    /// Identities must be nonzero and unique in the list.
    #[error("mask correspondence identity {0} is zero or repeated")]
    InvalidIdentity(u32),
    /// Both arc coordinates must be finite and less than one.
    #[error("mask correspondence {0} must have both positions in [0, 1)")]
    InvalidPosition(u32),
    /// Two pairs occupy the same position on the source mask.
    #[error("mask correspondence source positions must be distinct")]
    DuplicateSource,
    /// Two pairs occupy the same position on the destination mask.
    #[error("mask correspondence destination positions must be distinct")]
    DuplicateDestination,
    /// A mapping that crosses itself cannot define a cyclic pairing.
    #[error("mask correspondence pairs must follow one cycle in the shared winding")]
    CrossingPairs,
}

impl crate::AuthoringFootprint for MaskCorrespondencePoint {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        Ok(())
    }
}

impl crate::AuthoringFootprint for MaskCorrespondencePoints {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        collector.collect(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PropertyDescriptor, PropertyValue};

    fn point(id: u32, source: f32, destination: f32) -> MaskCorrespondencePoint {
        MaskCorrespondencePoint {
            id,
            source,
            destination,
            interpolation: MaskCorrespondenceInterpolation::Smooth,
        }
    }

    #[test]
    fn correspondences_round_trip_as_an_independent_author_value() {
        let points =
            MaskCorrespondencePoints::new(vec![point(7, 0.25, 0.75), point(21, 0.0, 0.5)]).unwrap();
        let value = PropertyValue::MaskCorrespondencePoints(points.clone());
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<PropertyValue>(&json).unwrap(), value);
        assert_eq!(points.points()[0].id, 7);
        let descriptor = PropertyDescriptor::new("test.correspondence", "Correspondence", value);
        assert!(!descriptor.schema.is_animatable);
        assert_eq!(descriptor.schema.value_type.channel_count(), 0);
    }

    #[test]
    fn correspondence_validation_refuses_ambiguous_identity_and_crossing_pairs() {
        assert_eq!(
            MaskCorrespondencePoints::new(vec![point(4, 0.1, 0.2), point(4, 0.2, 0.4)]),
            Err(MaskCorrespondenceError::InvalidIdentity(4))
        );
        assert_eq!(
            MaskCorrespondencePoints::new(vec![point(2, 1.0, 0.2)]),
            Err(MaskCorrespondenceError::InvalidPosition(2))
        );
        let crossing = vec![
            point(1, 0.1, 0.1),
            point(2, 0.2, 0.3),
            point(3, 0.3, 0.2),
            point(4, 0.4, 0.4),
        ];
        assert_eq!(
            MaskCorrespondencePoints::new(crossing.clone()),
            Err(MaskCorrespondenceError::CrossingPairs)
        );
        assert!(serde_json::from_value::<MaskCorrespondencePoints>(
            serde_json::to_value(crossing).unwrap()
        )
        .is_err());
    }
}
