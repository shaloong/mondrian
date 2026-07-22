//! Sequence-owned visual transitions between two strong Clip endpoints.

use crate::clip::Clip;
use mondrian_core::{
    automation::PropertyBag, ClipId, TimelineTime, TimelineTimeRange, VideoTransitionId,
};
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Exact source intervals required to evaluate both Transition inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTransitionSourceDemand {
    /// Source interval requested from the left endpoint.
    pub left: TimelineTimeRange,
    /// Source interval requested from the right endpoint.
    pub right: TimelineTimeRange,
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

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, Rational::new(1, 25)))
            .expect("test time")
    }

    #[test]
    fn source_handle_validation_uses_unclamped_two_input_demands() {
        let left = Clip::new(AssetId::new(), tt(0), tt(10)).expect("left");
        let mut right = Clip::new(AssetId::new(), tt(10), tt(10)).expect("right");
        right.source_in = tt(5);
        right.source_out = tt(15);
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
}
