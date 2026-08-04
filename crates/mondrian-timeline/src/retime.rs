//! Atomic constant-retime operations over canonical Clip source-time maps.

use crate::clip::{Clip, ClipSourceTimeMap};
use crate::sequence::Sequence;
use mondrian_core::{ClipId, MondrianError, TimeScale, TimelineTime};
use std::collections::{HashMap, HashSet};

/// One constant-retime intent applied without changing Timeline placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipConstantRetime {
    /// Replace the nonzero source-time rate while preserving the visible source span direction.
    ///
    /// The rate must be nonzero. A direction change anchors the replacement at
    /// the old exclusive terminal boundary so the same source span reverses
    /// instead of jumping to unrelated media.
    SetRate { rate: TimeScale },
    /// Hold the exact source sample visible at one Sequence-local time.
    HoldAtSequenceTime { sequence_time: TimelineTime },
}

/// Complete target set and intent for one atomic constant-retime operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipConstantRetimeRequest {
    /// Stable Clip identities that must all resolve or the operation fails.
    pub clip_ids: Vec<ClipId>,
    /// Constant source-time transform to apply.
    pub retime: ClipConstantRetime,
}

/// Receipt from one atomic constant-retime operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipConstantRetimeOutcome {
    /// Requested Clips whose canonical source-time map changed.
    pub changed_clip_ids: Vec<ClipId>,
}

/// Apply one constant retime atomically to an already-cloned Sequence candidate.
///
/// Every target must exist exactly once and every owning Track must be
/// unlocked. The operation validates all replacement maps before mutating any
/// Clip. Placement, duration, Clip-local visual time, and audio edit origins
/// remain unchanged.
pub fn apply_clip_constant_retime(
    sequence: &mut Sequence,
    request: &ClipConstantRetimeRequest,
) -> mondrian_core::Result<ClipConstantRetimeOutcome> {
    if request.clip_ids.is_empty() {
        return Err(retime_error("retime target set must not be empty"));
    }
    let requested = request.clip_ids.iter().copied().collect::<HashSet<_>>();
    if requested.len() != request.clip_ids.len() {
        return Err(retime_error(
            "retime target set contains duplicate Clip identities",
        ));
    }
    if matches!(
        request.retime,
        ClipConstantRetime::SetRate { rate } if rate.numerator() == 0
    ) {
        return Err(retime_error(
            "constant retime requires a nonzero exact rate; use a hold intent for zero",
        ));
    }

    let mut replacements = HashMap::<ClipId, ClipSourceTimeMap>::new();
    for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
        for clip in &track.clips {
            if !requested.contains(&clip.id) {
                continue;
            }
            if track.is_locked {
                return Err(MondrianError::TrackLocked { track_id: track.id.to_string() });
            }
            if replacements.contains_key(&clip.id) {
                return Err(retime_error(format!(
                    "Clip identity {} resolves more than once",
                    clip.id
                )));
            }
            let replacement = replacement_map(clip, request.retime)?;
            replacement.map(clip.duration)?;
            replacements.insert(clip.id, replacement);
        }
    }

    if replacements.len() != requested.len() {
        let missing = request
            .clip_ids
            .iter()
            .find(|clip_id| !replacements.contains_key(clip_id))
            .ok_or_else(|| retime_error("retime target resolution is inconsistent"))?;
        return Err(MondrianError::ClipNotFound { clip_id: missing.to_string() });
    }

    let mut changed_clip_ids = Vec::new();
    for track in sequence.video_tracks.iter_mut().chain(&mut sequence.audio_tracks) {
        for clip in &mut track.clips {
            let Some(replacement) = replacements.remove(&clip.id) else {
                continue;
            };
            if clip.source_time_map() == &replacement {
                continue;
            }
            clip.replace_source_time_map(replacement)?;
            changed_clip_ids.push(clip.id);
        }
    }
    changed_clip_ids.sort_unstable();
    Ok(ClipConstantRetimeOutcome { changed_clip_ids })
}

fn replacement_map(
    clip: &Clip,
    retime: ClipConstantRetime,
) -> mondrian_core::Result<ClipSourceTimeMap> {
    Ok(match retime {
        ClipConstantRetime::SetRate { rate } => {
            let current = clip.source_time_scale().numerator();
            let replacement = rate.numerator();
            let direction_changed = current != 0 && current.signum() != replacement.signum();
            let source_origin = if direction_changed {
                clip.source_terminal_boundary()?
            } else {
                clip.source_origin()
            };
            ClipSourceTimeMap::constant(source_origin, rate)
        }
        ClipConstantRetime::HoldAtSequenceTime { sequence_time } => {
            if !clip.contains(sequence_time)? {
                return Err(retime_error(format!(
                    "hold time {sequence_time} is outside Clip {}",
                    clip.id
                )));
            }
            let source_sample = clip.timeline_to_source_sample(sequence_time)?;
            ClipSourceTimeMap::hold(source_sample)
        }
    })
}

fn retime_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "clip_constant_retime".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::Sequence;
    use mondrian_core::{AssetId, ClipLinkGroupId, TimelineTime};

    fn tt(frames: i64) -> TimelineTime {
        TimelineTime::new(frames, 25).expect("exact test time")
    }

    #[test]
    fn forward_rate_updates_complete_target_set_without_moving_placement() {
        let mut sequence = Sequence::new("retime");
        let group = ClipLinkGroupId::new();
        let mut video = Clip::new(AssetId::new(), tt(10), tt(20)).expect("video Clip");
        video.link_group = Some(group);
        let video_id = video.id;
        let mut audio = Clip::new(AssetId::new(), tt(10), tt(20)).expect("audio Clip");
        audio.link_group = Some(group);
        let audio_id = audio.id;
        sequence.video_tracks[0].add_clip(video).expect("add video");
        sequence.audio_tracks[0].add_clip(audio).expect("add audio");

        let outcome = apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![video_id, audio_id],
                retime: ClipConstantRetime::SetRate { rate: TimeScale::new(2, 1).expect("2x") },
            },
        )
        .expect("retime");

        assert_eq!(outcome.changed_clip_ids.len(), 2);
        assert!(outcome.changed_clip_ids.contains(&video_id));
        assert!(outcome.changed_clip_ids.contains(&audio_id));
        for clip_id in [video_id, audio_id] {
            let clip = sequence
                .video_tracks
                .iter()
                .chain(&sequence.audio_tracks)
                .flat_map(|track| &track.clips)
                .find(|clip| clip.id == clip_id)
                .expect("retimed Clip");
            assert_eq!(clip.position, tt(10));
            assert_eq!(clip.duration, tt(20));
            assert_eq!(clip.clip_time_in, TimelineTime::ZERO);
            assert_eq!(clip.source_time_scale(), TimeScale::new(2, 1).expect("2x"));
            assert_eq!(clip.source_terminal_boundary().expect("terminal"), tt(40));
        }
    }

    #[test]
    fn hold_samples_original_map_and_rejects_locked_or_partial_targets_atomically() {
        let mut sequence = Sequence::new("hold");
        let mut clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("Clip");
        clip.set_source_origin(tt(100)).expect("source origin");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");

        apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![clip_id],
                retime: ClipConstantRetime::HoldAtSequenceTime { sequence_time: tt(15) },
            },
        )
        .expect("hold");
        let held = &sequence.video_tracks[0].clips[0];
        assert_eq!(held.source_origin(), tt(105));
        assert_eq!(held.source_time_scale().numerator(), 0);

        let before = held.source_time_map().clone();
        sequence.video_tracks[0].is_locked = true;
        assert!(apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![clip_id, ClipId::new()],
                retime: ClipConstantRetime::SetRate { rate: TimeScale::ONE },
            },
        )
        .is_err());
        assert_eq!(sequence.video_tracks[0].clips[0].source_time_map(), &before);
    }

    #[test]
    fn direction_changes_reverse_the_existing_source_span_at_its_exclusive_boundary() {
        let mut sequence = Sequence::new("direction change");
        let mut clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("Clip");
        clip.set_source_origin(tt(100)).expect("source origin");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");

        apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![clip_id],
                retime: ClipConstantRetime::SetRate { rate: TimeScale::NEGATIVE_ONE },
            },
        )
        .expect("reverse");
        let reversed = &sequence.video_tracks[0].clips[0];
        assert_eq!(reversed.source_origin(), tt(120));
        assert_eq!(
            reversed.source_terminal_boundary().expect("terminal"),
            tt(100)
        );
        assert_eq!(
            reversed.timeline_to_source_sample(reversed.position).expect("first sample"),
            mondrian_core::SourceSampleTarget::strict_predecessor(tt(120))
        );

        apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![clip_id],
                retime: ClipConstantRetime::SetRate { rate: TimeScale::ONE },
            },
        )
        .expect("restore forward");
        let restored = &sequence.video_tracks[0].clips[0];
        assert_eq!(restored.source_origin(), tt(100));
        assert_eq!(
            restored.source_terminal_boundary().expect("terminal"),
            tt(120)
        );
    }

    #[test]
    fn hold_of_reverse_content_preserves_strict_predecessor_sampling() {
        let mut sequence = Sequence::new("reverse hold");
        let mut clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("Clip");
        clip.set_constant_source_time_map(tt(120), TimeScale::NEGATIVE_ONE)
            .expect("reverse map");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");

        apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![clip_id],
                retime: ClipConstantRetime::HoldAtSequenceTime { sequence_time: tt(15) },
            },
        )
        .expect("hold reverse content");

        let held = &sequence.video_tracks[0].clips[0];
        assert_eq!(held.source_origin(), tt(115));
        assert_eq!(held.source_time_scale(), TimeScale::ZERO);
        assert_eq!(
            held.timeline_to_source_sample(held.position).expect("held sample"),
            mondrian_core::SourceSampleTarget::strict_predecessor(tt(115))
        );
    }

    #[test]
    fn set_rate_rejects_zero_without_mutating_author_state() {
        let mut sequence = Sequence::new("zero rate");
        let clip = Clip::new(AssetId::new(), tt(0), tt(20)).expect("Clip");
        let clip_id = clip.id;
        let original = clip.source_time_map().clone();
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");

        assert!(apply_clip_constant_retime(
            &mut sequence,
            &ClipConstantRetimeRequest {
                clip_ids: vec![clip_id],
                retime: ClipConstantRetime::SetRate { rate: TimeScale::new(0, 1).expect("zero") },
            },
        )
        .is_err());
        assert_eq!(
            sequence.video_tracks[0].clips[0].source_time_map(),
            &original
        );
    }
}
