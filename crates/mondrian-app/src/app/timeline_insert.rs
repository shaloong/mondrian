//! Product Adapter for Asset-backed professional Insert Edit.

use super::ui_actions::TimelineInsertAssetPayload;
use super::AppState;
use mondrian_assets::AssetKind;
use mondrian_core::{
    events::AppEvent, AudioSourceComponentId, ClipLinkGroupId, FramePosition, MondrianError,
    TimelineTime,
};
use mondrian_timeline::{
    apply_insert_edit, Clip, InsertEditOutcome, InsertEditPlacement, InsertEditRequest,
    InsertTimelineStatePolicy,
};
use std::collections::BTreeSet;

impl AppState {
    /// Insert one Asset source selection through explicit target/ripple scope.
    ///
    /// The UI payload is converted from the Sequence video grid into exact
    /// author time once. Scope registration, structural editing, validation,
    /// revision advancement, and Undo publication form one Author Transaction.
    pub fn insert_asset_from_ui(
        &mut self,
        payload: TimelineInsertAssetPayload,
    ) -> mondrian_core::Result<InsertEditOutcome> {
        if payload.insert_frame < 0 || payload.source_in_frame < 0 || payload.duration_frames <= 0 {
            return Err(insert_error(
                "insert/source frames must be non-negative and duration must be positive",
            ));
        }
        let asset = self
            .asset_library()
            .ok_or_else(|| insert_error("Asset Library is not connected"))?
            .get_asset(payload.asset_id)?
            .ok_or_else(|| MondrianError::AssetNotFound {
                asset_id: payload.asset_id.to_string(),
            })?;
        validate_asset_targets(&asset, &payload)?;

        let timeline_state_policy = payload.timeline_state_policy;
        let insert_frame = payload.insert_frame;
        let duration_frames = payload.duration_frames;
        let playhead_frame_before = self.current_frame();
        let (sequence_id, outcome) =
            self.commit_active_sequence_edit("插入编辑", move |sequence| {
                let time_base = sequence.time_base();
                let at = TimelineTime::from_frame_position(FramePosition::new(
                    payload.insert_frame,
                    time_base,
                ))?;
                let source_in = TimelineTime::from_frame_position(FramePosition::new(
                    payload.source_in_frame,
                    time_base,
                ))?;
                let duration = TimelineTime::from_frame_position(FramePosition::new(
                    payload.duration_frames,
                    time_base,
                ))?;
                let media_probe = asset.media_probe();
                validate_source_interval(
                    asset.kind.clone(),
                    media_probe.map(|probe| probe.duration),
                    source_in,
                    duration,
                    time_base.to_f64(),
                )?;

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
            && playhead_frame_before >= insert_frame
        {
            if let Some(frame) = playhead_frame_before.checked_add(duration_frames) {
                self.reconcile_playhead_after_committed_authoring_change(frame, "timeline_insert");
            } else {
                tracing::error!(
                    playhead_frame_before,
                    duration_frames,
                    "failed to move playhead after committed Insert: frame arithmetic overflow"
                );
            }
        }
        Ok(outcome)
    }
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
        AssetKind::SolidColor => Clip::new_solid_color(
            asset_id,
            mondrian_core::Color::from_hex(0x808080),
            position,
            duration,
        ),
        AssetKind::Audio => Err(insert_error(
            "audio-only Assets cannot create a video placement",
        )),
    }
}

fn auto_fit_picture(
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
