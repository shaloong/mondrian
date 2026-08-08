//! Stable-identity authoring for properties owned directly by a Timeline Clip.
//!
//! Inspector, Viewer, Headless, scripting, and future plugin Adapters all use
//! this Module. It derives current Track placement, resolves persistent
//! parameters, prepares exact mutations, rejects stale/no-op requests before a
//! mutable candidate exists, and commits one Author Transaction per gesture.

use std::collections::HashSet;

use mondrian_core::automation::{AnimationParameterAddress, PropertyHost, PropertyMutation};
use mondrian_core::{ClipId, Color, MondrianError, Result};

use super::animation_authoring::{ClipNumericCurveEdit, NormalizedCurvePoint};
use super::product_action::{
    ClipCurveEditPayload, ClipEditNumericCurvePayload, ClipWriteParameterValuesPayload,
};
use super::*;

const MAX_ATOMIC_PARAMETER_WRITES: usize = 32;

struct PreparedClipParameterWrites {
    sequence_id: SequenceId,
    mutations: Vec<PropertyMutation>,
}

impl AppState {
    pub(super) fn clip_enabled_write_would_change(
        &self,
        clip_id: ClipId,
        enabled: bool,
    ) -> Result<bool> {
        let (_, clip) = authorable_clip(self, clip_id, false, "clip_set_enabled")?;
        Ok(clip.is_disabled == enabled)
    }

    pub(super) fn set_clip_enabled_by_id(
        &mut self,
        clip_id: ClipId,
        enabled: bool,
    ) -> Result<bool> {
        if !self.clip_enabled_write_would_change(clip_id, enabled)? {
            return Ok(false);
        }
        let sequence_id = self
            .active_sequence_id()
            .ok_or_else(|| clip_authoring_error("clip_set_enabled", "no active Sequence"))?;
        self.commit_sequence_edit(sequence_id, "切换片段启用状态", |sequence| {
            let clip = sequence
                .find_clip_mut(clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            clip.is_disabled = !enabled;
            Ok(())
        })?;
        Ok(true)
    }

    pub(super) fn clip_solid_color_write_would_change(
        &self,
        clip_id: ClipId,
        color: Color,
    ) -> Result<bool> {
        let (_, clip) = authorable_clip(self, clip_id, true, "clip_set_solid_color")?;
        let current = clip.content.solid_color().ok_or_else(|| {
            clip_authoring_error("clip_set_solid_color", "target is not a Solid Color Clip")
        })?;
        Ok(current != color)
    }

    pub(super) fn set_clip_solid_color_by_id(
        &mut self,
        clip_id: ClipId,
        color: Color,
    ) -> Result<bool> {
        if !self.clip_solid_color_write_would_change(clip_id, color)? {
            return Ok(false);
        }
        let sequence_id = self
            .active_sequence_id()
            .ok_or_else(|| clip_authoring_error("clip_set_solid_color", "no active Sequence"))?;
        self.commit_sequence_edit(sequence_id, "调整片段颜色", |sequence| {
            let clip = sequence
                .find_clip_mut(clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            if !clip.set_solid_color(color)? {
                return Err(clip_authoring_error(
                    "clip_set_solid_color",
                    "Solid Color Clip already has the requested source color",
                ));
            }
            Ok(())
        })?;
        Ok(true)
    }

    pub(super) fn clip_parameter_writes_would_change(
        &self,
        payload: &ClipWriteParameterValuesPayload,
    ) -> Result<bool> {
        Ok(!prepare_clip_parameter_writes(self, payload)?.mutations.is_empty())
    }

    pub(super) fn write_clip_parameter_values(
        &mut self,
        payload: ClipWriteParameterValuesPayload,
    ) -> Result<bool> {
        let prepared = prepare_clip_parameter_writes(self, &payload)?;
        if prepared.mutations.is_empty() {
            return Ok(false);
        }
        let clip_id = payload.clip_id;
        self.commit_sequence_edit(prepared.sequence_id, "调整片段属性", move |sequence| {
            let clip = sequence
                .find_clip_mut(clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            for mutation in prepared.mutations {
                clip.apply_property_mutation(mutation)?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    pub(super) fn clip_numeric_curve_payload_would_change(
        &self,
        payload: &ClipEditNumericCurvePayload,
    ) -> Result<bool> {
        self.clip_numeric_curve_edit_would_change(
            payload.clip_id,
            &payload.parameter,
            numeric_curve_edit_from_payload(&payload.edit)?,
        )
    }

    pub(super) fn edit_clip_numeric_curve_payload(
        &mut self,
        payload: ClipEditNumericCurvePayload,
    ) -> Result<super::animation_authoring::ClipNumericCurveEditOutcome> {
        let edit = numeric_curve_edit_from_payload(&payload.edit)?;
        self.edit_clip_numeric_curve(payload.clip_id, payload.parameter, edit)
    }
}

fn prepare_clip_parameter_writes(
    state: &AppState,
    payload: &ClipWriteParameterValuesPayload,
) -> Result<PreparedClipParameterWrites> {
    if payload.writes.is_empty() {
        return Err(clip_authoring_error(
            "clip_write_parameter_values",
            "parameter write set is empty",
        ));
    }
    if payload.writes.len() > MAX_ATOMIC_PARAMETER_WRITES {
        return Err(clip_authoring_error(
            "clip_write_parameter_values",
            format!(
                "parameter write set exceeds the {MAX_ATOMIC_PARAMETER_WRITES}-entry gesture limit"
            ),
        ));
    }

    let (sequence, clip) =
        authorable_clip(state, payload.clip_id, true, "clip_write_parameter_values")?;
    let sequence_id = sequence.id;
    let author_time = clip
        .clamped_visual_author_time(state.current_timeline_time()?.unwrap_or(sequence.playhead))?;
    let parameters = clip.intrinsic_parameter_bag();
    let mut seen = HashSet::<AnimationParameterAddress>::with_capacity(payload.writes.len());
    let mut mutations = Vec::with_capacity(payload.writes.len());
    for write in &payload.writes {
        if !seen.insert(write.parameter.clone()) {
            return Err(clip_authoring_error(
                "clip_write_parameter_values",
                format!(
                    "duplicate parameter instance {} / {}",
                    write.parameter.animation_track_id, write.parameter.parameter_id
                ),
            ));
        }
        if let Some(mutation) = parameters.prepare_value_write_by_address(
            &write.parameter,
            author_time,
            write.value.clone(),
        )? {
            mutations.push(mutation);
        }
    }
    Ok(PreparedClipParameterWrites { sequence_id, mutations })
}

fn authorable_clip<'a>(
    state: &'a AppState,
    clip_id: ClipId,
    require_video: bool,
    step_id: &'static str,
) -> Result<(&'a Sequence, &'a Clip)> {
    let sequence = state
        .active_sequence()
        .ok_or_else(|| clip_authoring_error(step_id, "no active Sequence"))?;
    let Some(location) = sequence.clip_track_location(clip_id) else {
        return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
    };
    if require_video && !location.is_video_track {
        return Err(clip_authoring_error(
            step_id,
            "visual Clip authoring requires a video Clip",
        ));
    }
    if location.is_locked {
        return Err(MondrianError::TrackLocked { track_id: location.track_id.to_string() });
    }
    let clip = sequence
        .find_clip(clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
    Ok((sequence, clip))
}

fn numeric_curve_edit_from_payload(edit: &ClipCurveEditPayload) -> Result<ClipNumericCurveEdit> {
    match edit {
        ClipCurveEditPayload::Upsert { keyframe_id, point } => Ok(ClipNumericCurveEdit::Upsert {
            keyframe_id: *keyframe_id,
            point: NormalizedCurvePoint::new(point.time_ratio, point.value_ratio)?,
        }),
        ClipCurveEditPayload::Remove { keyframe_id } => {
            Ok(ClipNumericCurveEdit::Remove { keyframe_id: *keyframe_id })
        }
    }
}

fn clip_authoring_error(step_id: &'static str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}
