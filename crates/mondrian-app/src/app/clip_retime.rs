//! Product adapter for constant Clip playback rate and picture holds.

use std::{collections::HashMap, time::Duration};

use mondrian_assets::AssetKind;
use mondrian_core::{timeline_data::ClipContent, ClipId, MondrianError, TimeScale, TimelineTime};
use mondrian_timeline::{
    apply_clip_constant_retime, ClipConstantRetime, ClipConstantRetimeRequest, Sequence,
};

use super::timeline_editing::{clip_link_group_member_ids, find_clip, find_clip_track_lock};
use super::video_transitions::validate_retimed_transition_handles_with_extents;
use super::AppState;

impl AppState {
    /// Apply one exact positive forward playback rate as one Author Transaction.
    ///
    /// Timeline duration and Clip-local visual time are preserved. When
    /// requested, the complete Sequence-local link group is retimed atomically.
    pub(super) fn set_clip_forward_rate_from_action(
        &mut self,
        clip_id: ClipId,
        rate: TimeScale,
        include_linked: bool,
    ) -> mondrian_core::Result<()> {
        if rate.numerator() <= 0 {
            return Err(retime_error(
                "forward playback rate must be a strictly positive exact ratio",
            ));
        }
        let before = self.active_sequence().ok_or_else(no_active_sequence)?;
        let clip_ids = if include_linked {
            clip_link_group_member_ids(before, clip_id)
        } else {
            find_clip(before, clip_id)
                .map(|_| vec![clip_id])
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?
        };
        validate_retime_content(before, &clip_ids, false)?;
        let sequence_id = before.id;
        let source_extents = self.resolved_retime_source_extents(before, &clip_ids)?;
        let transition_extents =
            self.resolved_retimed_transition_source_extents(before, &clip_ids)?;
        // Retime invalidates current media/audio execution. A failed stop must
        // reject the author transaction instead of leaving old transport bound
        // to newly retimed content.
        self.stop()?;
        let changed =
            self.commit_sequence_edit(sequence_id, "调整片段播放速率", move |sequence| {
                let outcome = apply_clip_constant_retime(
                    sequence,
                    &ClipConstantRetimeRequest {
                        clip_ids,
                        retime: ClipConstantRetime::ForwardRate { rate },
                    },
                )?;
                if outcome.changed_clip_ids.is_empty() {
                    return Ok(false);
                }
                validate_retimed_clip_extents(
                    sequence,
                    &outcome.changed_clip_ids,
                    &source_extents,
                )?;
                validate_retimed_transition_handles_with_extents(
                    sequence,
                    &outcome.changed_clip_ids,
                    &transition_extents,
                )?;
                Ok(true)
            })?;
        if changed {
            self.settle_preview_access_source();
        }
        Ok(())
    }

    /// Freeze one video Clip at the picture visible at a Sequence-local time.
    ///
    /// Linked audio is intentionally not modified: repeating one audio sample
    /// is not a valid freeze-frame operation.
    pub(super) fn freeze_video_clip_from_action(
        &mut self,
        clip_id: ClipId,
        sequence_time: mondrian_core::FramePosition,
    ) -> mondrian_core::Result<()> {
        let before = self.active_sequence().ok_or_else(no_active_sequence)?;
        let (_, is_video, _) = find_clip_track_lock(before, clip_id)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
        if !is_video {
            return Err(retime_error(
                "freeze frame requires a Clip on a video Track",
            ));
        }
        validate_retime_content(before, &[clip_id], true)?;
        if sequence_time.time_base != before.time_base() {
            return Err(retime_error(
                "freeze-frame position must use the active Sequence time base",
            ));
        }
        let sequence_time = TimelineTime::from_frame_position(sequence_time)?;
        let sequence_id = before.id;
        let source_extents = self.resolved_retime_source_extents(before, &[clip_id])?;
        let transition_extents =
            self.resolved_retimed_transition_source_extents(before, &[clip_id])?;
        self.stop()?;
        let changed =
            self.commit_sequence_edit(sequence_id, "创建定格帧", move |sequence| {
                let outcome = apply_clip_constant_retime(
                    sequence,
                    &ClipConstantRetimeRequest {
                        clip_ids: vec![clip_id],
                        retime: ClipConstantRetime::HoldAtSequenceTime { sequence_time },
                    },
                )?;
                if outcome.changed_clip_ids.is_empty() {
                    return Ok(false);
                }
                validate_retimed_clip_extents(
                    sequence,
                    &outcome.changed_clip_ids,
                    &source_extents,
                )?;
                validate_retimed_transition_handles_with_extents(
                    sequence,
                    &outcome.changed_clip_ids,
                    &transition_extents,
                )?;
                Ok(true)
            })?;
        if changed {
            self.settle_preview_access_source();
        }
        Ok(())
    }

    fn resolved_retime_source_extents(
        &self,
        sequence: &Sequence,
        clip_ids: &[ClipId],
    ) -> mondrian_core::Result<HashMap<ClipId, TimelineTime>> {
        let mut extents = HashMap::with_capacity(clip_ids.len());
        for clip_id in clip_ids {
            let clip = find_clip(sequence, *clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            let (_, is_video, _) = find_clip_track_lock(sequence, *clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            extents.insert(*clip_id, self.known_retime_source_duration(clip, is_video)?);
        }
        Ok(extents)
    }

    fn known_retime_source_duration(
        &self,
        clip: &mondrian_timeline::Clip,
        is_video: bool,
    ) -> mondrian_core::Result<TimelineTime> {
        match &clip.content {
            ClipContent::NestedSequence { sequence_id, .. } => self
                .sequence_by_id(*sequence_id)
                .ok_or_else(|| {
                    retime_error(format!(
                        "nested Sequence {sequence_id} is unavailable for retime validation"
                    ))
                })?
                .total_duration(),
            ClipContent::Media { asset_id, .. } => {
                let library = self.asset_library().ok_or_else(|| {
                    retime_error("Asset Library is unavailable for retime validation")
                })?;
                let asset = library.get_asset(*asset_id)?.ok_or_else(|| {
                    retime_error(format!(
                        "Asset {asset_id} is unavailable for retime validation"
                    ))
                })?;
                if asset.kind == AssetKind::StillImage {
                    return Err(retime_error(format!(
                        "still-image Clip {} cannot use a nonzero playback rate",
                        clip.id
                    )));
                }
                let media_probe = asset.media_probe().ok_or_else(|| {
                    retime_error(format!(
                        "Asset {asset_id} has no coherent media probe for retime validation"
                    ))
                })?;
                let stream_duration = if is_video {
                    media_probe.primary_video().and_then(|stream| stream.duration)
                } else {
                    media_probe.primary_audio().and_then(|stream| stream.duration)
                };
                duration_to_timeline_time(
                    stream_duration
                        .filter(|duration| !duration.is_zero())
                        .or((!media_probe.duration.is_zero()).then_some(media_probe.duration)),
                )
            }
            ClipContent::SolidColor { .. }
            | ClipContent::BasicTitle { .. }
            | ClipContent::AdjustmentLayer { .. } => Err(retime_error(format!(
                "Clip {} has no finite source extent for retime validation",
                clip.id
            ))),
        }
    }
}

fn validate_retimed_clip_extents(
    sequence: &Sequence,
    clip_ids: &[ClipId],
    source_extents: &HashMap<ClipId, TimelineTime>,
) -> mondrian_core::Result<()> {
    for clip_id in clip_ids {
        let clip = find_clip(sequence, *clip_id)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
        let extent = source_extents
            .get(clip_id)
            .copied()
            .ok_or_else(|| retime_error(format!("Clip {clip_id} has no captured source extent")))?;
        let origin = clip.source_origin();
        if origin.is_negative() {
            return Err(retime_error(format!(
                "Clip {} source origin is before the source extent",
                clip.id
            )));
        }
        if clip.source_time_scale().numerator() == 0 {
            if origin >= extent {
                return Err(retime_error(format!(
                    "Clip {} hold sample is outside the source extent",
                    clip.id
                )));
            }
        } else if clip.source_terminal_boundary()? > extent {
            return Err(retime_error(format!(
                "Clip {} forward retime exceeds the source extent",
                clip.id
            )));
        }
    }
    Ok(())
}

fn validate_retime_content(
    sequence: &Sequence,
    clip_ids: &[ClipId],
    freeze: bool,
) -> mondrian_core::Result<()> {
    for clip_id in clip_ids {
        let clip = find_clip(sequence, *clip_id)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
        match clip.content {
            ClipContent::Media { .. } | ClipContent::NestedSequence { .. } => {}
            ClipContent::SolidColor { .. }
            | ClipContent::BasicTitle { .. }
            | ClipContent::AdjustmentLayer { .. } => {
                return Err(retime_error(format!(
                    "{} has no source-time dependency to {}",
                    clip.id,
                    if freeze { "freeze" } else { "retime" }
                )));
            }
        }
    }
    Ok(())
}

fn duration_to_timeline_time(duration: Option<Duration>) -> mondrian_core::Result<TimelineTime> {
    let duration = duration.ok_or_else(|| retime_error("retime source duration is unknown"))?;
    let nanos = i64::try_from(duration.as_nanos())
        .map_err(|_| retime_error("source duration exceeds exact Timeline Time range"))?;
    Ok(TimelineTime::new(nanos, 1_000_000_000)?)
}

fn no_active_sequence() -> MondrianError {
    retime_error("there is no active Sequence")
}

fn retime_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "clip_retime".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::{AssetId, ClipLinkGroupId, ColorSpace, FramePosition, Rational};
    use mondrian_media::info::{AudioCodec, ChannelLayout, PixelFormat, VideoCodec};
    use mondrian_media::{
        AudioStreamInfo, DecodedVideoRange, DetectedColorInterpretation, MediaInfo,
        VideoCodecProfile, VideoColorDetectionMethod, VideoColorInterpretationConfidence,
        VideoColorSpaceSource, VideoStreamInfo,
    };
    use mondrian_timeline::Clip;

    #[test]
    fn linked_forward_rate_and_video_hold_are_atomic_undoable_product_actions() {
        let (root, asset_library, asset_id) = retime_media_fixture();
        let mut state = AppState::new();
        let mut sequence = Sequence::new("retime");
        let time_base = sequence.time_base();
        let group = ClipLinkGroupId::new();
        let mut video =
            Clip::new(asset_id, TimelineTime::ZERO, frame_time(20, time_base)).expect("video Clip");
        video.link_group = Some(group);
        let video_id = video.id;
        let mut audio = Clip::new(
            video.media_asset_id().expect("media Asset"),
            TimelineTime::ZERO,
            frame_time(20, time_base),
        )
        .expect("audio Clip");
        audio.link_group = Some(group);
        let audio_id = audio.id;
        sequence.video_tracks[0].add_clip(video).expect("add video");
        sequence.audio_tracks[0].add_clip(audio).expect("add audio");
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(asset_library));
        let history_before_noop =
            state.authoring_history().expect("history").diagnostics().undo_entries;
        state
            .dispatch_action(mondrian_editor_state::Action::SetClipForwardRate {
                clip_id: video_id,
                rate: TimeScale::ONE,
                include_linked: true,
            })
            .expect("no-op rate");
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            history_before_noop
        );

        state
            .dispatch_action(mondrian_editor_state::Action::SetClipForwardRate {
                clip_id: video_id,
                rate: TimeScale::new(2, 1).expect("2x"),
                include_linked: true,
            })
            .expect("linked retime");
        let sequence = state.active_sequence().expect("Sequence");
        assert_eq!(
            find_clip(sequence, video_id).expect("video").source_time_scale(),
            TimeScale::new(2, 1).expect("2x")
        );
        assert_eq!(
            find_clip(sequence, audio_id).expect("audio").source_time_scale(),
            TimeScale::new(2, 1).expect("2x")
        );

        state
            .dispatch_action(mondrian_editor_state::Action::FreezeVideoClipAt {
                clip_id: video_id,
                sequence_time: FramePosition::new(5, time_base),
            })
            .expect("freeze");
        let sequence = state.active_sequence().expect("Sequence");
        let video = find_clip(sequence, video_id).expect("video");
        assert_eq!(video.source_origin(), frame_time(10, time_base));
        assert_eq!(video.source_time_scale().numerator(), 0);
        assert_eq!(
            find_clip(sequence, audio_id).expect("audio").source_time_scale(),
            TimeScale::new(2, 1).expect("2x")
        );

        assert!(state.undo_timeline().expect("undo"));
        assert_eq!(
            find_clip(state.active_sequence().expect("Sequence"), video_id)
                .expect("video")
                .source_time_scale(),
            TimeScale::new(2, 1).expect("2x")
        );
        drop(state);
        std::fs::remove_dir_all(root).expect("remove media fixture");
    }

    #[test]
    fn invalid_rate_audio_freeze_and_locked_link_member_reject_without_history() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("retime rejection");
        let time_base = sequence.time_base();
        let group = ClipLinkGroupId::new();
        let mut video = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            frame_time(20, time_base),
        )
        .expect("video Clip");
        video.link_group = Some(group);
        let video_id = video.id;
        let mut audio = Clip::new(
            video.media_asset_id().expect("media Asset"),
            TimelineTime::ZERO,
            frame_time(20, time_base),
        )
        .expect("audio Clip");
        audio.link_group = Some(group);
        let audio_id = audio.id;
        sequence.video_tracks[0].add_clip(video).expect("add video");
        sequence.audio_tracks[0].add_clip(audio).expect("add audio");
        sequence.audio_tracks[0].is_locked = true;
        state.test_set_sequence(Some(sequence));
        let history_before = state.authoring_history().expect("history").diagnostics().undo_entries;

        assert!(state
            .dispatch_action(mondrian_editor_state::Action::SetClipForwardRate {
                clip_id: video_id,
                rate: TimeScale::new(0, 1).expect("zero rate"),
                include_linked: true,
            })
            .is_err());
        assert!(state
            .dispatch_action(mondrian_editor_state::Action::FreezeVideoClipAt {
                clip_id: audio_id,
                sequence_time: FramePosition::new(5, time_base),
            })
            .is_err());
        assert!(state
            .dispatch_action(mondrian_editor_state::Action::FreezeVideoClipAt {
                clip_id: video_id,
                sequence_time: FramePosition::new(5, Rational::new(1, 30)),
            })
            .is_err());
        assert!(state
            .dispatch_action(mondrian_editor_state::Action::SetClipForwardRate {
                clip_id: video_id,
                rate: TimeScale::new(2, 1).expect("2x"),
                include_linked: true,
            })
            .is_err());

        let sequence = state.active_sequence().expect("Sequence");
        assert_eq!(
            find_clip(sequence, video_id).expect("video").source_time_scale(),
            TimeScale::ONE
        );
        assert_eq!(
            find_clip(sequence, audio_id).expect("audio").source_time_scale(),
            TimeScale::ONE
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            history_before
        );

        let mut unresolved_state = AppState::new();
        let mut unresolved_sequence = Sequence::new("retime unavailable source");
        let unresolved_clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            frame_time(20, time_base),
        )
        .expect("unresolved Clip");
        let unresolved_clip_id = unresolved_clip.id;
        unresolved_sequence.video_tracks[0]
            .add_clip(unresolved_clip)
            .expect("add unresolved Clip");
        unresolved_state.test_set_sequence(Some(unresolved_sequence));
        let unresolved_history_before = unresolved_state
            .authoring_history()
            .expect("history")
            .diagnostics()
            .undo_entries;
        assert!(unresolved_state
            .dispatch_action(mondrian_editor_state::Action::SetClipForwardRate {
                clip_id: unresolved_clip_id,
                rate: TimeScale::new(2, 1).expect("2x"),
                include_linked: false,
            })
            .is_err());
        assert_eq!(
            find_clip(
                unresolved_state.active_sequence().expect("Sequence"),
                unresolved_clip_id
            )
            .expect("unresolved Clip")
            .source_time_scale(),
            TimeScale::ONE
        );
        assert_eq!(
            unresolved_state
                .authoring_history()
                .expect("history")
                .diagnostics()
                .undo_entries,
            unresolved_history_before
        );
    }

    #[test]
    fn resolved_transition_handle_overflow_rejects_retime_atomically() {
        let (root, asset_library, asset_id) = retime_media_fixture();
        let mut state = AppState::new();
        let mut sequence = Sequence::new("retime transition");
        let time_base = sequence.time_base();
        let mut left =
            Clip::new(asset_id, TimelineTime::ZERO, frame_time(20, time_base)).expect("left Clip");
        let left_id = left.id;
        left.set_source_origin(TimelineTime::ZERO).expect("left source origin");
        let mut right = Clip::new(
            asset_id,
            frame_time(20, time_base),
            frame_time(20, time_base),
        )
        .expect("right Clip");
        let right_id = right.id;
        right.set_source_origin(frame_time(20, time_base)).expect("right source origin");
        sequence.video_tracks[0].add_clip(left).expect("add left");
        sequence.video_tracks[0].add_clip(right).expect("add right");
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(asset_library));
        state
            .create_default_cross_dissolve(left_id, right_id)
            .expect("admit initial Transition");
        let history_before = state.authoring_history().expect("history").diagnostics().undo_entries;

        assert!(state
            .dispatch_action(mondrian_editor_state::Action::SetClipForwardRate {
                clip_id: left_id,
                rate: TimeScale::new(2, 1).expect("2x"),
                include_linked: false,
            })
            .is_err());

        assert_eq!(
            find_clip(state.active_sequence().expect("Sequence"), left_id)
                .expect("left")
                .source_time_scale(),
            TimeScale::ONE
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            history_before
        );
        drop(state);
        std::fs::remove_dir_all(root).expect("remove media fixture");
    }

    fn frame_time(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("exact frame time")
    }

    fn retime_media_fixture() -> (std::path::PathBuf, std::sync::Arc<AssetLibrary>, AssetId) {
        let root = std::env::temp_dir().join(format!(
            "mondrian-clip-retime-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let path = root.join("source.mov");
        std::fs::write(&path, [0u8]).expect("media fixture");
        let path = std::fs::canonicalize(path).expect("canonical media fixture");
        let fingerprint = mondrian_media::MediaFileFingerprint::capture(&path);
        let candidate = mondrian_assets::AssetMediaProbeCandidate::new(
            path.clone(),
            fingerprint,
            MediaInfo {
                duration: Duration::from_secs(2),
                file_size: 1,
                container: "mov".to_owned(),
                video_streams: vec![VideoStreamInfo {
                    index: 0,
                    codec: VideoCodec::H264,
                    duration: Some(Duration::from_secs(2)),
                    codec_profile: VideoCodecProfile::H264High,
                    width: 1920,
                    height: 1080,
                    frame_rate: Rational::FPS_25,
                    frame_rate_proven: true,
                    pixel_format: PixelFormat::Yuv420p,
                    pixel_format_proven: true,
                    color_range: DecodedVideoRange::Limited,
                    color_interpretation: DetectedColorInterpretation {
                        candidate_color_space: Some(ColorSpace::Rec709),
                        confidence: VideoColorInterpretationConfidence::High,
                        source: VideoColorSpaceSource::Metadata,
                        method: VideoColorDetectionMethod::MetadataHint,
                        evidence: vec![
                            mondrian_media::VideoColorInterpretationEvidence::MetadataHint {
                                scope: mondrian_media::VideoColorMetadataHintScope::Stream,
                                key: "source_color_space".to_owned(),
                                value: "Rec709".to_owned(),
                                detected_color_space: ColorSpace::Rec709,
                                authority: mondrian_media::VideoColorMetadataHintAuthority::SourceDeclaration(
                                    mondrian_media::VideoColorMetadataDeclaration::SourceColorSpace,
                                ),
                            },
                        ],
                        warnings: Vec::new(),
                        user_overridable: true,
                    },
                    color_metadata: None,
                    color_metadata_hints: Vec::new(),
                    hdr_metadata: Vec::new(),
                    bit_depth: 8,
                    has_alpha: false,
                    avg_bitrate: 8_000_000,
                    total_frames: Some(50),
                }],
                audio_streams: vec![AudioStreamInfo {
                    index: 1,
                    stream_id: Some(1),
                    language: None,
                    title: None,
                    is_default: true,
                    codec: AudioCodec::Pcm { bit_depth: 24 },
                    duration: Some(Duration::from_secs(2)),
                    sample_rate: 48_000,
                    channels: 2,
                    channel_layout: ChannelLayout::Stereo,
                    bit_depth: 24,
                    avg_bitrate: 2_304_000,
                }],
                has_video: true,
                has_audio: true,
            },
        )
        .expect("valid media probe candidate");
        let asset_id = library.commit_media_probe(candidate, None).expect("register media Asset");
        (root, library, asset_id)
    }
}
