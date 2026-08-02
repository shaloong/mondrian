//! Sequence-owned visual transitions between two strong Clip endpoints.

use crate::{clip::Clip, sequence::Sequence};
use mondrian_core::{
    automation::PropertyBag, AssetId, AuthoringFootprint, AuthoringFootprintCollector,
    AuthoringFootprintError, ClipId, SequenceId, TimelineTime, TimelineTimeRange,
    VideoTransitionId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Definition selected for a two-input visual transition.
///
/// Built-ins and plugins share one persistent shape. A missing plugin remains
/// recoverable author intent; execution must report it as unavailable rather
/// than substituting another transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VideoTransitionType {
    /// Scene-linear two-input cross dissolve.
    CrossDissolve,
    /// External transition definition selected by its stable registry key.
    Plugin { definition_id: String },
}

impl AuthoringFootprint for VideoTransitionType {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::CrossDissolve => Ok(()),
            Self::Plugin { definition_id } => collector.collect(definition_id),
        }
    }
}

impl VideoTransitionType {
    /// Stable definition key used by registries, diagnostics, and fingerprints.
    pub fn definition_id(&self) -> &str {
        match self {
            Self::CrossDissolve => "mondrian.video_transition.cross_dissolve",
            Self::Plugin { definition_id } => definition_id,
        }
    }
}

/// One explicit two-input visual transition owned by a Sequence.
///
/// Track membership is intentionally derived from the two strong Clip
/// endpoints. Persisting a third `track_id` authority would permit a
/// contradictory author state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTransition {
    /// Stable instance identity.
    pub id: VideoTransitionId,
    /// Earlier editorial Clip endpoint.
    pub left: ClipId,
    /// Later editorial Clip endpoint.
    pub right: ClipId,
    /// Sole authoritative half-open Sequence-time interval.
    pub sequence_range: TimelineTimeRange,
    /// Built-in or plugin transition definition.
    pub transition_type: VideoTransitionType,
    /// Definition-described, exactly timed parameter state.
    pub properties: PropertyBag,
    /// Definition-specific non-parameter payload.
    pub params: serde_json::Value,
    /// Disabled instances remain editable but do not affect rendering.
    pub is_enabled: bool,
}

impl AuthoringFootprint for VideoTransition {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            left: _,
            right: _,
            sequence_range: _,
            transition_type,
            properties,
            params,
            is_enabled: _,
        } = self;
        collector.collect(transition_type)?;
        collector.collect(properties)?;
        collector.collect(params)
    }
}

/// Exact source intervals required to evaluate both Transition inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTransitionSourceDemand {
    /// Source interval requested from the left endpoint.
    pub left: TimelineTimeRange,
    /// Source interval requested from the right endpoint.
    pub right: TimelineTimeRange,
}

/// Authoritative picture-source extent supplied at an execution-admission seam.
///
/// `Still` is an atemporal single-picture source and can be held for arbitrary
/// before/after handle demand without fabricating a duration. A time-varying
/// source instead supplies its exact half-open source-time range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "range", rename_all = "snake_case")]
pub enum PictureSourceExtent {
    /// One physical picture that may be held indefinitely.
    Still,
    /// Exact half-open extent of a time-varying picture source.
    TimelineRange(TimelineTimeRange),
}

/// External picture identity whose extent a Transition endpoint needs.
///
/// Generated Clip content is defined by its placement and never crosses this
/// resolver seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureSourceRef {
    /// File-backed media selected by stable Asset identity.
    MediaAsset(AssetId),
    /// Child Sequence output selected by stable Sequence identity.
    NestedSequence(SequenceId),
}

/// Side of one two-input visual Transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoTransitionEndpointSide {
    /// Earlier editorial endpoint.
    Left,
    /// Later editorial endpoint.
    Right,
}

impl std::fmt::Display for VideoTransitionEndpointSide {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Left => "left",
            Self::Right => "right",
        })
    }
}

/// Failure while validating renderer-selected Transition handles against one
/// immutable set of picture-source extent facts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VideoTransitionSourceHandleValidationError {
    /// Selected identity is absent from the owner Sequence.
    #[error(
        "selected video Transition {transition_id} does not exist in owner Sequence {sequence_id}"
    )]
    MissingTransition {
        /// Owner Sequence.
        sequence_id: SequenceId,
        /// Renderer-selected Transition.
        transition_id: VideoTransitionId,
    },
    /// Selected identity refers to disabled author state.
    #[error(
        "selected video Transition {transition_id} is disabled in owner Sequence {sequence_id}"
    )]
    DisabledTransition {
        /// Owner Sequence.
        sequence_id: SequenceId,
        /// Renderer-selected Transition.
        transition_id: VideoTransitionId,
    },
    /// Strong endpoint, edit geometry, definition, or exact time mapping is
    /// invalid.
    #[error("video Transition {transition_id} cannot validate source handles: {reason}")]
    InvalidTransition {
        /// Transition being validated.
        transition_id: VideoTransitionId,
        /// Exact validation failure.
        reason: String,
    },
    /// The caller did not provide the selected external source's exact extent.
    #[error(
        "video Transition {transition_id} {endpoint} endpoint has no resolved picture extent for {picture_source:?}"
    )]
    MissingPictureExtent {
        /// Transition being validated.
        transition_id: VideoTransitionId,
        /// Endpoint side.
        endpoint: VideoTransitionEndpointSide,
        /// Unresolved source identity.
        picture_source: PictureSourceRef,
    },
    /// A resolver returned an extent shape that contradicts the source domain.
    #[error(
        "video Transition {transition_id} {endpoint} endpoint has invalid picture extent for {picture_source:?}: {reason}"
    )]
    InvalidPictureExtent {
        /// Transition being validated.
        transition_id: VideoTransitionId,
        /// Endpoint side.
        endpoint: VideoTransitionEndpointSide,
        /// Source whose extent is contradictory.
        picture_source: PictureSourceRef,
        /// Exact contradiction.
        reason: String,
    },
    /// The exact time-varying extent cannot satisfy the unclamped source
    /// demand. A still source never reaches this variant because it is
    /// indefinitely holdable.
    #[error(
        "video Transition {transition_id} {endpoint} endpoint demand {demand:?} exceeds picture extent {available:?} for {picture_source:?}"
    )]
    InsufficientSourceHandles {
        /// Transition being validated.
        transition_id: VideoTransitionId,
        /// Endpoint side.
        endpoint: VideoTransitionEndpointSide,
        /// Exact unclamped endpoint demand.
        demand: TimelineTimeRange,
        /// Exact resolved availability.
        available: PictureSourceExtent,
        /// Source that supplied the availability.
        picture_source: PictureSourceRef,
    },
}

/// Validate renderer-selected visual Transition handles against immutable
/// picture-source extent facts.
///
/// `selected` remains authoritative for range reachability: unrelated
/// Transitions and sources are not inspected. The resolver must project one
/// internally consistent dependency capture; this function performs no Asset
/// Library, filesystem, FFmpeg, registry, or live Project access.
pub fn validate_selected_video_transition_source_handles(
    owner: &Sequence,
    selected: &BTreeSet<VideoTransitionId>,
    mut resolve: impl FnMut(PictureSourceRef) -> Option<PictureSourceExtent>,
) -> Result<(), VideoTransitionSourceHandleValidationError> {
    for transition_id in selected {
        let transition = owner
            .video_transitions
            .iter()
            .find(|transition| transition.id == *transition_id)
            .ok_or(
                VideoTransitionSourceHandleValidationError::MissingTransition {
                    sequence_id: owner.id,
                    transition_id: *transition_id,
                },
            )?;
        if !transition.is_enabled {
            return Err(
                VideoTransitionSourceHandleValidationError::DisabledTransition {
                    sequence_id: owner.id,
                    transition_id: *transition_id,
                },
            );
        }
        crate::sequence::validate_video_transition(&owner.video_tracks, transition).map_err(
            |error| VideoTransitionSourceHandleValidationError::InvalidTransition {
                transition_id: *transition_id,
                reason: error.to_string(),
            },
        )?;
        let (left, right) = transition_endpoint_clips(owner, transition).ok_or_else(|| {
            VideoTransitionSourceHandleValidationError::InvalidTransition {
                transition_id: *transition_id,
                reason: "validated strong endpoints could not be resolved".to_owned(),
            }
        })?;
        let demand = transition.source_demand(left, right).map_err(|error| {
            VideoTransitionSourceHandleValidationError::InvalidTransition {
                transition_id: *transition_id,
                reason: error.to_string(),
            }
        })?;
        validate_endpoint_source_handles(
            *transition_id,
            VideoTransitionEndpointSide::Left,
            left,
            demand.left,
            &mut resolve,
        )?;
        validate_endpoint_source_handles(
            *transition_id,
            VideoTransitionEndpointSide::Right,
            right,
            demand.right,
            &mut resolve,
        )?;
    }
    Ok(())
}

impl VideoTransition {
    /// Construct an enabled Cross Dissolve with no definition-specific payload.
    pub fn cross_dissolve(left: ClipId, right: ClipId, sequence_range: TimelineTimeRange) -> Self {
        Self {
            id: VideoTransitionId::new(),
            left,
            right,
            sequence_range,
            transition_type: VideoTransitionType::CrossDissolve,
            properties: PropertyBag::default(),
            params: serde_json::Value::Object(serde_json::Map::new()),
            is_enabled: true,
        }
    }

    /// Validate definition-local state independently of endpoint geometry.
    pub fn validate_definition_state(&self) -> mondrian_core::Result<()> {
        if self.left == self.right {
            return Err(invalid_transition(self.id, "endpoints must be distinct"));
        }
        if self.sequence_range.is_empty() {
            return Err(invalid_transition(self.id, "range must not be empty"));
        }
        if self.transition_type.definition_id().trim().is_empty() {
            return Err(invalid_transition(
                self.id,
                "definition identity must not be blank",
            ));
        }
        self.properties.validate()?;
        Ok(())
    }

    /// Project the Sequence-time Transition interval into both endpoint source
    /// domains without clamping to the visible Clip ranges.
    ///
    /// The media/nested-source adapter must compare this demand with probed
    /// source extents before admitting execution. This separation prevents the
    /// timeline model from guessing external media duration.
    pub fn source_demand(
        &self,
        left: &Clip,
        right: &Clip,
    ) -> mondrian_core::Result<VideoTransitionSourceDemand> {
        let end = self.sequence_range.end()?;
        Ok(VideoTransitionSourceDemand {
            left: source_demand_for_clip(left, self.sequence_range.start, end)?,
            right: source_demand_for_clip(right, self.sequence_range.start, end)?,
        })
    }

    /// Reject source extents that cannot satisfy the exact two-input demand.
    pub fn validate_source_extents(
        &self,
        left: &Clip,
        right: &Clip,
        left_available: TimelineTimeRange,
        right_available: TimelineTimeRange,
    ) -> mondrian_core::Result<()> {
        let demand = self.source_demand(left, right)?;
        if !range_contains(left_available, demand.left)? {
            return Err(invalid_transition(
                self.id,
                "left endpoint has insufficient source handles",
            ));
        }
        if !range_contains(right_available, demand.right)? {
            return Err(invalid_transition(
                self.id,
                "right endpoint has insufficient source handles",
            ));
        }
        Ok(())
    }

    /// Fork the transition instance and property identities for duplication.
    pub(crate) fn fork_author_identities(&mut self) {
        self.id = VideoTransitionId::new();
        self.properties.fork_author_identities();
    }
}

fn source_demand_for_clip(
    clip: &Clip,
    start: TimelineTime,
    end: TimelineTime,
) -> mondrian_core::Result<TimelineTimeRange> {
    let source_start = clip.timeline_to_source_time(start)?;
    let source_end = clip.timeline_to_source_time(end)?;
    let start = source_start.min(source_end);
    TimelineTimeRange::new(start, source_start.max(source_end).checked_sub(start)?)
        .map_err(Into::into)
}

fn transition_endpoint_clips<'a>(
    owner: &'a Sequence,
    transition: &VideoTransition,
) -> Option<(&'a Clip, &'a Clip)> {
    owner.video_tracks.iter().find_map(|track| {
        let left = track.clips.iter().find(|clip| clip.id == transition.left)?;
        let right = track.clips.iter().find(|clip| clip.id == transition.right)?;
        Some((left, right))
    })
}

fn validate_endpoint_source_handles(
    transition_id: VideoTransitionId,
    endpoint: VideoTransitionEndpointSide,
    clip: &Clip,
    demand: TimelineTimeRange,
    resolve: &mut impl FnMut(PictureSourceRef) -> Option<PictureSourceExtent>,
) -> Result<(), VideoTransitionSourceHandleValidationError> {
    let picture_source = match &clip.content {
        mondrian_core::timeline_data::ClipContent::Media { asset_id, .. } => {
            PictureSourceRef::MediaAsset(*asset_id)
        }
        mondrian_core::timeline_data::ClipContent::NestedSequence { sequence_id, .. } => {
            PictureSourceRef::NestedSequence(*sequence_id)
        }
        mondrian_core::timeline_data::ClipContent::SolidColor { .. }
        | mondrian_core::timeline_data::ClipContent::BasicTitle { .. } => return Ok(()),
        mondrian_core::timeline_data::ClipContent::AdjustmentLayer { .. } => {
            return Err(
                VideoTransitionSourceHandleValidationError::InvalidTransition {
                    transition_id,
                    reason: format!(
                        "{endpoint} endpoint Clip {} is an Adjustment Layer",
                        clip.id
                    ),
                },
            );
        }
    };
    let available = resolve(picture_source).ok_or(
        VideoTransitionSourceHandleValidationError::MissingPictureExtent {
            transition_id,
            endpoint,
            picture_source,
        },
    )?;
    if let PictureSourceExtent::TimelineRange(range) = available {
        if range.is_empty() {
            return Err(
                VideoTransitionSourceHandleValidationError::InvalidPictureExtent {
                    transition_id,
                    endpoint,
                    picture_source,
                    reason: "a time-varying picture extent must not be empty".to_owned(),
                },
            );
        }
        if matches!(picture_source, PictureSourceRef::NestedSequence(_))
            && range.start != TimelineTime::ZERO
        {
            return Err(
                VideoTransitionSourceHandleValidationError::InvalidPictureExtent {
                    transition_id,
                    endpoint,
                    picture_source,
                    reason: "a nested Sequence picture extent must begin at Timeline Time zero"
                        .to_owned(),
                },
            );
        }
    } else if matches!(picture_source, PictureSourceRef::NestedSequence(_)) {
        return Err(
            VideoTransitionSourceHandleValidationError::InvalidPictureExtent {
                transition_id,
                endpoint,
                picture_source,
                reason: "a nested Sequence must publish an exact Timeline range, not Still"
                    .to_owned(),
            },
        );
    }
    let satisfies = match available {
        PictureSourceExtent::Still => true,
        PictureSourceExtent::TimelineRange(range) => {
            range_contains(range, demand).map_err(|error| {
                VideoTransitionSourceHandleValidationError::InvalidPictureExtent {
                    transition_id,
                    endpoint,
                    picture_source,
                    reason: error.to_string(),
                }
            })?
        }
    };
    if satisfies {
        Ok(())
    } else {
        Err(
            VideoTransitionSourceHandleValidationError::InsufficientSourceHandles {
                transition_id,
                endpoint,
                demand,
                available,
                picture_source,
            },
        )
    }
}

fn range_contains(
    available: TimelineTimeRange,
    requested: TimelineTimeRange,
) -> mondrian_core::Result<bool> {
    let available_end = available.end()?;
    let requested_end = requested.end()?;
    if requested.is_empty() {
        return Ok(requested.start >= available.start && requested.start < available_end);
    }
    Ok(requested.start >= available.start && requested_end <= available_end)
}

pub(crate) fn invalid_transition(
    id: VideoTransitionId,
    reason: impl Into<String>,
) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "validate_video_transition".to_owned(),
        reason: format!("video Transition {id}: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AssetId, FramePosition, Rational};
    use std::collections::BTreeSet;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, Rational::new(1, 25)))
            .expect("test time")
    }

    #[test]
    fn source_handle_validation_uses_unclamped_two_input_demands() {
        let left = Clip::new(AssetId::new(), tt(0), tt(10)).expect("left");
        let mut right = Clip::new(AssetId::new(), tt(10), tt(10)).expect("right");
        right.set_source_origin(tt(5)).expect("set source origin");
        let transition = VideoTransition::cross_dissolve(
            left.id,
            right.id,
            TimelineTimeRange::new(tt(8), tt(4)).expect("transition range"),
        );

        let demand = transition.source_demand(&left, &right).expect("source demand");
        assert_eq!(
            demand.left,
            TimelineTimeRange::new(tt(8), tt(4)).expect("left demand")
        );
        assert_eq!(
            demand.right,
            TimelineTimeRange::new(tt(3), tt(4)).expect("right demand")
        );
        assert!(transition
            .validate_source_extents(
                &left,
                &right,
                TimelineTimeRange::new(tt(0), tt(10)).expect("short left"),
                TimelineTimeRange::new(tt(0), tt(20)).expect("right source"),
            )
            .is_err());
        transition
            .validate_source_extents(
                &left,
                &right,
                TimelineTimeRange::new(tt(0), tt(20)).expect("left source"),
                TimelineTimeRange::new(tt(0), tt(20)).expect("right source"),
            )
            .expect("sufficient handles");
    }

    fn adjacent_media_transition() -> (Sequence, VideoTransitionId, AssetId, AssetId) {
        let left_asset = AssetId::new();
        let right_asset = AssetId::new();
        let mut owner = Sequence::new("picture source handles");
        let left = Clip::new(left_asset, tt(0), tt(10)).expect("left");
        let right = Clip::new(right_asset, tt(10), tt(10)).expect("right");
        let transition = VideoTransition::cross_dissolve(
            left.id,
            right.id,
            TimelineTimeRange::new(tt(8), tt(4)).expect("transition range"),
        );
        let transition_id = transition.id;
        owner.video_tracks[0].add_clip(left).expect("left");
        owner.video_tracks[0].add_clip(right).expect("right");
        owner.video_transitions.push(transition);
        (owner, transition_id, left_asset, right_asset)
    }

    #[test]
    fn still_picture_extent_satisfies_both_before_and_after_handle_demand() {
        let (owner, transition_id, left_asset, right_asset) = adjacent_media_transition();
        let selected = BTreeSet::from([transition_id]);
        let transition = &owner.video_transitions[0];
        let left = &owner.video_tracks[0].clips[0];
        let right = &owner.video_tracks[0].clips[1];
        let demand = transition.source_demand(left, right).expect("source demand");
        assert_eq!(
            demand.left,
            TimelineTimeRange::new(tt(8), tt(4)).expect("left after-handle demand")
        );
        assert_eq!(
            demand.right,
            TimelineTimeRange::new(tt(-2), tt(4)).expect("right before-handle demand")
        );

        validate_selected_video_transition_source_handles(&owner, &selected, |source| {
            matches!(
                source,
                PictureSourceRef::MediaAsset(asset_id)
                    if asset_id == left_asset || asset_id == right_asset
            )
            .then_some(PictureSourceExtent::Still)
        })
        .expect("one physical still may be held for arbitrary handles");
    }

    #[test]
    fn time_varying_picture_extent_reports_the_exact_insufficient_endpoint() {
        let (owner, transition_id, left_asset, right_asset) = adjacent_media_transition();
        let selected = BTreeSet::from([transition_id]);
        let short_left = TimelineTimeRange::new(tt(0), tt(10)).expect("short left extent");
        let long = TimelineTimeRange::new(tt(0), tt(20)).expect("long extent");
        let error =
            validate_selected_video_transition_source_handles(&owner, &selected, |source| {
                match source {
                    PictureSourceRef::MediaAsset(asset_id) if asset_id == left_asset => {
                        Some(PictureSourceExtent::TimelineRange(short_left))
                    }
                    PictureSourceRef::MediaAsset(asset_id) if asset_id == right_asset => {
                        Some(PictureSourceExtent::TimelineRange(long))
                    }
                    PictureSourceRef::MediaAsset(_) | PictureSourceRef::NestedSequence(_) => None,
                }
            })
            .expect_err("left source has no post-cut handle");
        assert!(matches!(
            error,
            VideoTransitionSourceHandleValidationError::InsufficientSourceHandles {
                transition_id: actual,
                endpoint: VideoTransitionEndpointSide::Left,
                demand,
                available: PictureSourceExtent::TimelineRange(available),
                picture_source: PictureSourceRef::MediaAsset(actual_asset),
            } if actual == transition_id
                && actual_asset == left_asset
                && demand == TimelineTimeRange::new(tt(8), tt(4)).expect("left demand")
                && available == short_left
        ));

        let error =
            validate_selected_video_transition_source_handles(&owner, &selected, |source| {
                match source {
                    PictureSourceRef::MediaAsset(asset_id) if asset_id == left_asset => {
                        Some(PictureSourceExtent::TimelineRange(long))
                    }
                    PictureSourceRef::MediaAsset(asset_id) if asset_id == right_asset => {
                        Some(PictureSourceExtent::TimelineRange(long))
                    }
                    PictureSourceRef::MediaAsset(_) | PictureSourceRef::NestedSequence(_) => None,
                }
            })
            .expect_err("right source has no pre-zero handle");
        assert!(matches!(
            error,
            VideoTransitionSourceHandleValidationError::InsufficientSourceHandles {
                transition_id: actual,
                endpoint: VideoTransitionEndpointSide::Right,
                demand,
                picture_source: PictureSourceRef::MediaAsset(actual_asset),
                ..
            } if actual == transition_id
                && actual_asset == right_asset
                && demand == TimelineTimeRange::new(tt(-2), tt(4)).expect("right demand")
        ));
    }

    #[test]
    fn nested_sequence_requires_a_zero_based_time_varying_extent() {
        let left_child = SequenceId::new();
        let right_child = SequenceId::new();
        let mut owner = Sequence::new("nested source handles");
        let left = Clip::new_nested_sequence(left_child, tt(0), tt(10), None).expect("nested left");
        let mut right =
            Clip::new_nested_sequence(right_child, tt(10), tt(10), None).expect("nested right");
        right.set_source_origin(tt(2)).expect("incoming child handle");
        let transition = VideoTransition::cross_dissolve(
            left.id,
            right.id,
            TimelineTimeRange::new(tt(8), tt(4)).expect("transition range"),
        );
        let transition_id = transition.id;
        owner.video_tracks[0].add_clip(left).expect("left");
        owner.video_tracks[0].add_clip(right).expect("right");
        owner.video_transitions.push(transition);
        let selected = BTreeSet::from([transition_id]);

        let error = validate_selected_video_transition_source_handles(&owner, &selected, |_| {
            Some(PictureSourceExtent::Still)
        })
        .expect_err("nested output is a timed source even when its pixels are static");
        assert!(matches!(
            error,
            VideoTransitionSourceHandleValidationError::InvalidPictureExtent {
                endpoint: VideoTransitionEndpointSide::Left,
                picture_source: PictureSourceRef::NestedSequence(sequence_id),
                ..
            } if sequence_id == left_child
        ));

        let shifted = TimelineTimeRange::new(tt(1), tt(20)).expect("shifted child Timeline range");
        let error = validate_selected_video_transition_source_handles(&owner, &selected, |_| {
            Some(PictureSourceExtent::TimelineRange(shifted))
        })
        .expect_err("child Timeline source starts at zero");
        assert!(matches!(
            error,
            VideoTransitionSourceHandleValidationError::InvalidPictureExtent {
                endpoint: VideoTransitionEndpointSide::Left,
                picture_source: PictureSourceRef::NestedSequence(sequence_id),
                ..
            } if sequence_id == left_child
        ));

        let complete = TimelineTimeRange::new(tt(0), tt(20)).expect("complete child extent");
        validate_selected_video_transition_source_handles(&owner, &selected, |source| {
            matches!(
                source,
                PictureSourceRef::NestedSequence(sequence_id)
                    if sequence_id == left_child || sequence_id == right_child
            )
            .then_some(PictureSourceExtent::TimelineRange(complete))
        })
        .expect("both frozen child extents satisfy the transition");
    }

    #[test]
    fn selected_identity_set_is_the_only_reachability_authority() {
        let (mut owner, transition_id, _, _) = adjacent_media_transition();
        validate_selected_video_transition_source_handles(
            &owner,
            &BTreeSet::new(),
            |_| -> Option<PictureSourceExtent> {
                panic!("an unselected source must not be resolved")
            },
        )
        .expect("unselected Transition is irrelevant");

        owner.video_transitions[0].is_enabled = false;
        let error = validate_selected_video_transition_source_handles(
            &owner,
            &BTreeSet::from([transition_id]),
            |_| -> Option<PictureSourceExtent> {
                panic!("a disabled selected Transition fails before source lookup")
            },
        )
        .expect_err("renderer evidence cannot select disabled author state");
        assert!(matches!(
            error,
            VideoTransitionSourceHandleValidationError::DisabledTransition {
                sequence_id,
                transition_id: actual,
            } if sequence_id == owner.id && actual == transition_id
        ));
    }
}
