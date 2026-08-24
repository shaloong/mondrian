//! Product Adapter for Asset-backed professional Insert Edit.

use super::product_action::TimelineInsertAssetPayload;
use super::AppState;
use mondrian_assets::AssetKind;
use mondrian_core::{
    events::AppEvent, AudioSourceComponentId, ClipLinkGroupId, FramePosition, FrameRounding,
    MondrianError, TimelineTime,
};
use mondrian_timeline::{
    apply_insert_edit, Clip, InsertEditOutcome, InsertEditPlacement, InsertEditRequest,
    InsertTimelineStatePolicy,
};
use std::collections::BTreeSet;

/// Product default content color for newly authored Solid Color clips.
pub(crate) const DEFAULT_SOLID_COLOR_CLIP_COLOR: mondrian_core::Color =
    mondrian_core::Color::from_hex(0x808080);

impl AppState {
    /// Whether one exact Asset Insert can execute against current author state.
    pub fn can_insert_asset_from_product_action(
        &self,
        payload: &TimelineInsertAssetPayload,
    ) -> bool {
        self.prepare_insert_asset(payload).is_ok()
    }

    /// Insert one Asset source selection through explicit target/ripple scope.
    ///
    /// The Product payload already carries exact canonical author time. Scope
    /// registration, structural editing, validation, revision advancement, and
    /// Undo publication form one Author Transaction.
    pub fn insert_asset_from_ui(
        &mut self,
        payload: TimelineInsertAssetPayload,
    ) -> mondrian_core::Result<InsertEditOutcome> {
        let asset = self.prepare_insert_asset(&payload)?;

        let timeline_state_policy = payload.timeline_state_policy;
        let insert_at = payload.at;
        let duration = payload.duration;
        let (playhead_time_before, frame_rate) = {
            let sequence = self.active_sequence().ok_or_else(|| insert_error("当前无序列"))?;
            (
                TimelineTime::from_frame_position(FramePosition::new(
                    self.current_frame(),
                    sequence.time_base(),
                ))?,
                sequence.settings.frame_rate,
            )
        };
        let (sequence_id, outcome) =
            self.commit_active_sequence_edit("插入编辑", move |sequence| {
                let at = payload.at;
                let source_in = payload.source_in;
                let duration = payload.duration;
                let media_probe = asset.media_probe();

                let mut placements = Vec::with_capacity(2);
                let mut link_group = None;
                if payload.video_target_track_id.is_some()
                    && payload.audio_target_track_id.is_some()
                {
                    link_group = Some(ClipLinkGroupId::new());
                }

                if let Some(track_id) = payload.video_target_track_id {
                    let mut clip = create_asset_clip(
                        asset.kind.clone(),
                        asset.id,
                        TimelineTime::ZERO,
                        duration,
                    )?;
                    clip.set_source_origin(source_in)?;
                    clip.label = Some(asset.name.clone());
                    clip.link_group = link_group;
                    if let Some(video) = media_probe.and_then(|probe| probe.primary_video()) {
                        auto_fit_picture(sequence, &mut clip, video.width, video.height);
                    }
                    placements.push(InsertEditPlacement {
                        track_id,
                        offset: TimelineTime::ZERO,
                        clip,
                    });
                }

                if let Some(track_id) = payload.audio_target_track_id {
                    let mut clip = Clip::new(asset.id, TimelineTime::ZERO, duration)?;
                    clip.set_source_origin(source_in)?;
                    clip.label = Some(asset.name.clone());
                    clip.link_group = link_group;
                    sequence.attach_default_media_audio_component(
                        &mut clip,
                        AudioSourceComponentId::primary(),
                    )?;
                    placements.push(InsertEditPlacement {
                        track_id,
                        offset: TimelineTime::ZERO,
                        clip,
                    });
                }

                let request = InsertEditRequest {
                    at,
                    duration,
                    placements,
                    ripple_tracks: payload.ripple_track_ids.into_iter().collect::<BTreeSet<_>>(),
                    automation_policy: payload.automation_policy,
                    transition_policy: payload.transition_policy,
                    timeline_state_policy: payload.timeline_state_policy,
                };
                let outcome = apply_insert_edit(sequence, &request)
                    .map_err(|error| insert_error(error.to_string()))?;
                Ok((sequence.id, outcome))
            })?;

        for clip_id in &outcome.inserted_clip_ids {
            self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id: *clip_id });
        }
        if timeline_state_policy == InsertTimelineStatePolicy::FollowEdit
            && playhead_time_before >= insert_at
        {
            let next = playhead_time_before.checked_add(duration)?;
            let frame = next.to_frame_position(frame_rate, FrameRounding::Nearest)?.frame.max(0);
            self.reconcile_playhead_after_committed_authoring_change(frame, "timeline_insert");
        }
        Ok(outcome)
    }

    fn prepare_insert_asset(
        &self,
        payload: &TimelineInsertAssetPayload,
    ) -> mondrian_core::Result<mondrian_assets::AssetRecord> {
        if payload.at.is_negative()
            || payload.source_in.is_negative()
            || payload.duration <= TimelineTime::ZERO
        {
            return Err(insert_error(
                "insert/source time must be non-negative and duration must be positive",
            ));
        }
        let sequence = self.active_sequence().ok_or_else(|| insert_error("当前无序列"))?;
        validate_insert_tracks(sequence, payload)?;
        let asset = self
            .asset_library()
            .ok_or_else(|| insert_error("Asset Library is not connected"))?
            .get_asset(payload.asset_id)?
            .ok_or_else(|| MondrianError::AssetNotFound {
                asset_id: payload.asset_id.to_string(),
            })?;
        validate_asset_targets(&asset, payload)?;
        validate_source_interval(
            asset.kind.clone(),
            asset.media_probe().map(|probe| probe.duration),
            payload.source_in,
            payload.duration,
            sequence.time_base().to_f64(),
        )?;
        Ok(asset)
    }
}

fn validate_insert_tracks(
    sequence: &mondrian_timeline::Sequence,
    payload: &TimelineInsertAssetPayload,
) -> mondrian_core::Result<()> {
    let ripple_tracks = payload.ripple_track_ids.iter().copied().collect::<BTreeSet<_>>();
    if ripple_tracks.is_empty() {
        return Err(insert_error(
            "Insert requires an explicit non-empty ripple Track closure",
        ));
    }
    let targets = [payload.video_target_track_id, payload.audio_target_track_id]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if targets.is_empty() || targets.iter().any(|track_id| !ripple_tracks.contains(track_id)) {
        return Err(insert_error(
            "every target Track must belong to the ripple closure",
        ));
    }
    if let Some(track_id) = payload.video_target_track_id {
        let track = sequence
            .video_tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })?;
        if track.is_locked {
            return Err(MondrianError::TrackLocked { track_id: track_id.to_string() });
        }
    }
    if let Some(track_id) = payload.audio_target_track_id {
        let track = sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })?;
        if track.is_locked {
            return Err(MondrianError::TrackLocked { track_id: track_id.to_string() });
        }
    }
    for track_id in ripple_tracks {
        let track = sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .find(|track| track.id == track_id)
            .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })?;
        if track.is_locked {
            return Err(MondrianError::TrackLocked { track_id: track_id.to_string() });
        }
    }
    Ok(())
}

fn validate_asset_targets(
    asset: &mondrian_assets::AssetRecord,
    payload: &TimelineInsertAssetPayload,
) -> mondrian_core::Result<()> {
    match asset.kind {
        AssetKind::Audio => {
            if payload.video_target_track_id.is_some() || payload.audio_target_track_id.is_none() {
                return Err(insert_error(
                    "an audio Asset requires exactly one audio target Track",
                ));
            }
            let media_probe = asset.media_probe().ok_or_else(|| {
                insert_error("the selected audio Asset has no coherent media probe")
            })?;
            if media_probe.primary_audio().is_none() {
                return Err(insert_error(
                    "the selected audio Asset has no audio stream to insert",
                ));
            }
        }
        AssetKind::Video => {
            if payload.video_target_track_id.is_none() {
                return Err(insert_error("a video Asset requires a video target Track"));
            }
            let media_probe = asset.media_probe().ok_or_else(|| {
                insert_error("the selected video Asset has no coherent media probe")
            })?;
            if media_probe.primary_video().is_none() {
                return Err(insert_error(
                    "the selected video Asset has no video stream to insert",
                ));
            }
            if payload.audio_target_track_id.is_some() && !media_probe.has_audio {
                return Err(insert_error(
                    "the selected video Asset has no audio component to insert",
                ));
            }
        }
        AssetKind::StillImage => {
            if payload.video_target_track_id.is_none() || payload.audio_target_track_id.is_some() {
                return Err(insert_error(
                    "a still-image Asset requires exactly one video target Track",
                ));
            }
            if asset.media_probe().and_then(|probe| probe.primary_video()).is_none() {
                return Err(insert_error(
                    "the selected still-image Asset has no coherent picture probe",
                ));
            }
        }
        AssetKind::AdjustmentLayer | AssetKind::SolidColor => {
            if payload.video_target_track_id.is_none() || payload.audio_target_track_id.is_some() {
                return Err(insert_error(
                    "generated visual Assets require exactly one video target Track",
                ));
            }
        }
    }
    Ok(())
}

fn validate_source_interval(
    kind: AssetKind,
    available: Option<std::time::Duration>,
    source_in: TimelineTime,
    duration: TimelineTime,
    frame_seconds: f64,
) -> mondrian_core::Result<()> {
    if matches!(
        kind,
        AssetKind::StillImage | AssetKind::AdjustmentLayer | AssetKind::SolidColor
    ) {
        if !source_in.is_zero() {
            return Err(insert_error(
                "still and generated visual Assets do not admit a non-zero source in",
            ));
        }
        return Ok(());
    }
    let available = available.ok_or_else(|| {
        insert_error("file-backed media has no coherent probe for source-range validation")
    })?;
    let requested_end = source_in.checked_add(duration)?.to_f64();
    let tolerance = frame_seconds.abs() * 0.5;
    if available.is_zero() || requested_end > available.as_secs_f64() + tolerance {
        return Err(insert_error(format!(
            "selected source interval ends at {requested_end:.6}s but probed media ends at {:.6}s",
            available.as_secs_f64()
        )));
    }
    Ok(())
}

fn create_asset_clip(
    kind: AssetKind,
    asset_id: mondrian_core::AssetId,
    position: TimelineTime,
    duration: TimelineTime,
) -> mondrian_core::Result<Clip> {
    match kind {
        AssetKind::Video => Clip::new(asset_id, position, duration),
        AssetKind::StillImage => Clip::new_still_image(asset_id, position, duration),
        AssetKind::AdjustmentLayer => Clip::new_adjustment_layer(asset_id, position, duration),
        AssetKind::SolidColor => {
            Clip::new_solid_color(asset_id, DEFAULT_SOLID_COLOR_CLIP_COLOR, position, duration)
        }
        AssetKind::Audio => Err(insert_error(
            "audio-only Assets cannot create a video placement",
        )),
    }
}

pub(super) fn auto_fit_picture(
    sequence: &mondrian_timeline::Sequence,
    clip: &mut Clip,
    media_width: u32,
    media_height: u32,
) {
    if media_width == 0 || media_height == 0 {
        return;
    }
    let sequence_width = sequence.settings.resolution.width.max(1) as f32;
    let sequence_height = sequence.settings.resolution.height.max(1) as f32;
    let fit_scale =
        (sequence_width / media_width as f32).min(sequence_height / media_height as f32);
    clip.transform.set_anchor_point(glam::Vec2::new(
        media_width as f32 * 0.5,
        media_height as f32 * 0.5,
    ));
    clip.transform.set_scale(glam::Vec2::new(fit_scale, fit_scale));
    clip.transform
        .set_position(glam::Vec2::new(sequence_width * 0.5, sequence_height * 0.5));
}

fn insert_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_insert_asset".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::AssetId;

    #[test]
    fn auto_fit_same_sized_picture_fills_canvas_with_centered_anchor() {
        let sequence = mondrian_timeline::Sequence::new("same-sized auto fit");
        let resolution = sequence.settings.resolution;
        let mut clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("clip");

        auto_fit_picture(&sequence, &mut clip, resolution.width, resolution.height);

        let anchor = clip.transform.get_anchor_point(TimelineTime::ZERO);
        let position = clip.transform.get_position(TimelineTime::ZERO);
        let scale = clip.transform.get_scale(TimelineTime::ZERO);
        let matrix = clip.transform.evaluate_matrix(TimelineTime::ZERO);
        let expected_center = glam::Vec2::new(
            resolution.width as f32 * 0.5,
            resolution.height as f32 * 0.5,
        );
        assert_eq!(anchor, expected_center);
        assert_eq!(position, expected_center);
        assert_eq!(scale, glam::Vec2::ONE);
        assert_eq!(matrix, glam::Mat3::IDENTITY);
    }
}
