//! AAC editorial and Transport Golden slice over production App Interfaces.

use super::audio_authoring_evidence::{
    capture_audio_track_authoring, GoldenAudioTrackAuthoringAnchor,
};
use super::fixture::{resolve_fixture, CorpusManifest, FixtureEvidence};
use super::harness::{
    author_transition, dispatch_author_transition, ensure_exact_requirement_evidence, fixture_root,
    wait_for_media_imports, AuthorTransitionEvidence, DurableReopenEvidence,
};
use super::headless_preview::{
    GoldenHeadlessPreview, GoldenHeadlessViewerEvidence, GoldenViewerPresentationEvidence,
};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
use super::{load_json, GoldenProjectContract};
use crate::app::playback::PlaybackAdvanceStatus;
use crate::app::ui_actions::{
    assets_prepare_drag_action, timeline_extract_range_action, timeline_lift_range_action,
    timeline_seek_with_source_action, timeline_set_in_out_point_action, timeline_trim_clips_action,
    track_set_edit_policy_action, AssetsPrepareDragPayload, TimelineInOutPointKind,
    TimelineInsertAssetPayload, TimelineSeekSource, TimelineSetInOutPointPayload,
    TimelineTrimClipsPayload, TimelineTrimPayloadEdge, TrackEditPolicyControl,
    TrackSetEditPolicyPayload,
};
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::{
    AssetId, ClipId, FramePosition, FrameRounding, SequenceId, TimelineTime, TrackId,
};
use mondrian_editor_state::action::SelectionTarget;
use mondrian_editor_state::Action;
use mondrian_media::info::{AudioCodec, ChannelLayout};
use mondrian_playback::ClockMaster;
use mondrian_timeline::{
    audio::{
        AudioChannelStripOutputPort, AudioRouteDestination, AudioRouteSource,
        AudioTrackMixerChannel,
    },
    InsertAutomationPolicy, InsertTimelineStatePolicy, InsertTransitionPolicy,
};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

pub(super) const EDITORIAL_SLICE_ID: &str = "editorial-transport-v2";
const VIEWER_PRESENTATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ClipRangeObservation {
    start_frame: i64,
    end_frame_exclusive: i64,
}

#[derive(Debug, Clone, Serialize)]
struct RangeEditScopeEvidence {
    targeted_track_ids: Vec<TrackId>,
    ripple_track_ids: Vec<TrackId>,
    unchanged_author_generation: u64,
    unchanged_sequence_revision: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "id")]
enum OperationEvidence {
    #[serde(rename = "insert")]
    Insert {
        author_step: AuthorTransitionEvidence,
        inserted_clip_id: ClipId,
        inserted_range: ClipRangeObservation,
        ripple_track_ids: Vec<TrackId>,
        primary_downstream_clip_id: ClipId,
        primary_before: ClipRangeObservation,
        primary_after: ClipRangeObservation,
        secondary_downstream_clip_id: ClipId,
        secondary_before: ClipRangeObservation,
        secondary_after: ClipRangeObservation,
    },
    #[serde(rename = "overwrite")]
    Overwrite {
        author_step: AuthorTransitionEvidence,
        retained_left_clip_id: ClipId,
        retained_left_range: ClipRangeObservation,
        replacement_clip_id: ClipId,
        replacement_range: ClipRangeObservation,
    },
    #[serde(rename = "split")]
    Split {
        author_step: AuthorTransitionEvidence,
        left_clip_id: ClipId,
        left_range: ClipRangeObservation,
        right_clip_id: ClipId,
        right_range: ClipRangeObservation,
    },
    #[serde(rename = "ripple")]
    Ripple {
        author_step: AuthorTransitionEvidence,
        removed_clip_id: ClipId,
        downstream_clip_id: ClipId,
        downstream_before: ClipRangeObservation,
        downstream_after: ClipRangeObservation,
    },
    #[serde(rename = "lift")]
    Lift {
        range_setup_steps: Vec<AuthorTransitionEvidence>,
        author_step: AuthorTransitionEvidence,
        undo_step: AuthorTransitionEvidence,
        redo_step: AuthorTransitionEvidence,
        scope: RangeEditScopeEvidence,
        removed_clip_id: ClipId,
        removed_range: ClipRangeObservation,
        primary_downstream_clip_id: ClipId,
        downstream_before: ClipRangeObservation,
        downstream_after: ClipRangeObservation,
    },
    #[serde(rename = "extract")]
    Extract {
        range_setup_steps: Vec<AuthorTransitionEvidence>,
        author_step: AuthorTransitionEvidence,
        undo_step: AuthorTransitionEvidence,
        redo_step: AuthorTransitionEvidence,
        scope: RangeEditScopeEvidence,
        trimmed_clip_id: ClipId,
        trimmed_before: ClipRangeObservation,
        trimmed_after: ClipRangeObservation,
        primary_downstream_clip_id: ClipId,
        primary_before: ClipRangeObservation,
        primary_after: ClipRangeObservation,
        secondary_downstream_clip_id: ClipId,
        secondary_before: ClipRangeObservation,
        secondary_after: ClipRangeObservation,
    },
    #[serde(rename = "scrub")]
    Scrub {
        target_frames: Vec<i64>,
        final_frame: i64,
        ready_deliveries: u64,
        warm_seek_count: u64,
        presentations: Vec<GoldenViewerPresentationEvidence>,
    },
    #[serde(rename = "accurate-seek")]
    AccurateSeek {
        target_frame: i64,
        final_frame: i64,
        ready_deliveries: u64,
        accurate_seek_count: u64,
        presentation: GoldenViewerPresentationEvidence,
    },
    #[serde(rename = "play")]
    Play {
        start_frame: i64,
        final_frame: i64,
        frames_advanced: i64,
        clock_master: &'static str,
        synthetic_clock_residency_us: u64,
        presentation: GoldenViewerPresentationEvidence,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Insert { .. } => "insert",
            Self::Overwrite { .. } => "overwrite",
            Self::Split { .. } => "split",
            Self::Ripple { .. } => "ripple",
            Self::Lift { .. } => "lift",
            Self::Extract { .. } => "extract",
            Self::Scrub { .. } => "scrub",
            Self::AccurateSeek { .. } => "accurate-seek",
            Self::Play { .. } => "play",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ImportedAacObservation {
    stream_index: u32,
    sample_rate: u32,
    channels: u8,
    channel_layout: ChannelLayout,
    average_bitrate: u64,
}

#[derive(Debug, Clone, Serialize)]
struct EditorialSetupEvidence {
    stage: GoldenSequenceStageEvidence,
    primary_audio_track_id: TrackId,
    secondary_audio_track_id: TrackId,
    asset_id: AssetId,
    imported_audio: ImportedAacObservation,
    setup_steps: Vec<AuthorTransitionEvidence>,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenEditorialReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: GoldenRunStatus,
    complete_golden_project: bool,
    fixture: FixtureEvidence,
    setup: EditorialSetupEvidence,
    operations: Vec<OperationEvidence>,
    viewer: GoldenHeadlessViewerEvidence,
    persistence: DurableReopenEvidence,
    authoring: GoldenAudioTrackAuthoringAnchor,
}

impl GoldenEditorialReport {
    pub(super) fn primary_sequence_id(&self) -> SequenceId {
        self.setup.stage.sequence_id()
    }

    pub(super) fn capture_authoring_anchor(
        &self,
        state: &AppState,
    ) -> anyhow::Result<GoldenAudioTrackAuthoringAnchor> {
        capture_editorial_authoring_anchor(
            state,
            self.primary_sequence_id(),
            self.setup.asset_id,
            [
                self.setup.primary_audio_track_id,
                self.setup.secondary_audio_track_id,
            ],
        )
    }

    pub(super) fn verify_retained_authoring(&self, state: &AppState) -> anyhow::Result<()> {
        ensure!(
            self.capture_authoring_anchor(state)? == self.authoring,
            "Editorial Track-owned audio authoring changed after the stage"
        );
        Ok(())
    }
}

fn capture_editorial_authoring_anchor(
    state: &AppState,
    sequence_id: SequenceId,
    asset_id: AssetId,
    track_ids: [TrackId; 2],
) -> anyhow::Result<GoldenAudioTrackAuthoringAnchor> {
    let sequence =
        state.sequence_by_id(sequence_id).context("Editorial Hero Sequence is absent")?;
    for track_id in track_ids {
        let track = sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == track_id)
            .with_context(|| format!("Editorial audio Track is absent: {track_id}"))?;
        ensure!(
            !track.clips.is_empty()
                && track.clips.iter().all(|clip| clip.media_asset_id() == Some(asset_id)),
            "Editorial audio Track changed AAC Asset identity"
        );
    }
    ensure!(
        state
            .asset_library()
            .context("Editorial Asset Library is absent")?
            .get_asset(asset_id)?
            .is_some(),
        "Editorial AAC Asset is absent"
    );
    capture_audio_track_authoring(state, sequence.id, &track_ids)
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum GoldenRunStatus {
    Passed,
}

fn audio_track(state: &AppState, track_id: TrackId) -> anyhow::Result<&mondrian_timeline::Track> {
    state
        .active_sequence()
        .context("active Sequence is absent")?
        .audio_tracks
        .iter()
        .find(|track| track.id == track_id)
        .context("Golden audio Track is absent")
}

fn clip_range(
    state: &AppState,
    track_id: TrackId,
    clip_id: ClipId,
) -> anyhow::Result<ClipRangeObservation> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let clip = audio_track(state, track_id)?
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .with_context(|| format!("audio Clip is absent: {clip_id}"))?;
    let start_frame = clip
        .position
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
        .frame;
    let end_frame_exclusive = clip
        .end_position()?
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
        .frame;
    Ok(ClipRangeObservation { start_frame, end_frame_exclusive })
}

fn sequence_in_out_range(state: &AppState) -> anyhow::Result<Option<ClipRangeObservation>> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    match (sequence.in_point, sequence.out_point) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => Ok(Some(ClipRangeObservation {
            start_frame: start
                .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
                .frame,
            end_frame_exclusive: end
                .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
                .frame,
        })),
        _ => anyhow::bail!("active Sequence has only one In/Out endpoint"),
    }
}

fn set_in_out_range(
    state: &mut AppState,
    range: ClipRangeObservation,
    in_intent: &'static str,
    out_intent: &'static str,
) -> anyhow::Result<Vec<AuthorTransitionEvidence>> {
    ensure!(
        sequence_in_out_range(state)?.is_none(),
        "previous Golden range edit did not consume its In/Out range"
    );
    let time_base = state.active_sequence().context("active Sequence is absent")?.time_base();
    let steps = vec![
        dispatch_author_transition(
            state,
            in_intent,
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: TimelineInOutPointKind::In,
                position: FramePosition::new(range.start_frame, time_base),
            }),
        )?,
        dispatch_author_transition(
            state,
            out_intent,
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: TimelineInOutPointKind::Out,
                position: FramePosition::new(range.end_frame_exclusive, time_base),
            }),
        )?,
    ];
    ensure!(
        sequence_in_out_range(state)? == Some(range),
        "Golden range setup did not preserve the exact half-open frame range"
    );
    Ok(steps)
}

fn ensure_clip_absent(
    state: &AppState,
    track_id: TrackId,
    clip_id: ClipId,
    operation: &str,
) -> anyhow::Result<()> {
    ensure!(
        audio_track(state, track_id)?.clips.iter().all(|clip| clip.id != clip_id),
        "{operation} retained Clip {clip_id}"
    );
    Ok(())
}

fn configure_range_edit_scope(
    state: &mut AppState,
    sequence_id: SequenceId,
    primary_track_id: TrackId,
    secondary_track_id: TrackId,
) -> anyhow::Result<RangeEditScopeEvidence> {
    let sequence =
        state.sequence_by_id(sequence_id).context("Editorial Hero Sequence is absent")?;
    let all_track_ids = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let author_generation = state.project_author_generation();
    let sequence_revision = sequence.revision.get();

    for track_id in all_track_ids {
        converge_track_edit_policy(
            state,
            sequence_id,
            track_id,
            TrackEditPolicyControl::Target,
            track_id == primary_track_id,
        )?;
        converge_track_edit_policy(
            state,
            sequence_id,
            track_id,
            TrackEditPolicyControl::SyncLock,
            track_id == primary_track_id || track_id == secondary_track_id,
        )?;
    }

    let sequence =
        state.sequence_by_id(sequence_id).context("Editorial Hero Sequence is absent")?;
    let targets = state.timeline_edit_targets(sequence);
    let expected_content_tracks = BTreeSet::from([primary_track_id]);
    let expected_ripple_tracks = BTreeSet::from([primary_track_id, secondary_track_id]);
    ensure!(
        targets.content_tracks == expected_content_tracks
            && targets.ripple_tracks == expected_ripple_tracks,
        "Track Targeting/Sync-Lock did not resolve the exact Golden range-edit scope"
    );
    ensure!(
        state.project_author_generation() == author_generation
            && sequence.revision.get() == sequence_revision,
        "Track Targeting/Sync-Lock polluted durable author state"
    );

    Ok(RangeEditScopeEvidence {
        targeted_track_ids: targets.content_tracks.into_iter().collect(),
        ripple_track_ids: targets.ripple_tracks.into_iter().collect(),
        unchanged_author_generation: author_generation,
        unchanged_sequence_revision: sequence_revision,
    })
}

fn converge_track_edit_policy(
    state: &mut AppState,
    sequence_id: SequenceId,
    track_id: TrackId,
    control: TrackEditPolicyControl,
    enabled: bool,
) -> anyhow::Result<()> {
    let current = match control {
        TrackEditPolicyControl::Target => state.timeline_track_targeted(sequence_id, track_id),
        TrackEditPolicyControl::SyncLock => state.timeline_track_sync_locked(sequence_id, track_id),
    };
    if current == enabled {
        return Ok(());
    }
    state.dispatch_action(track_set_edit_policy_action(TrackSetEditPolicyPayload {
        track_id,
        control,
        enabled,
    }))?;
    Ok(())
}

#[cfg(test)]
mod range_edit_scope_tests {
    use super::*;
    use mondrian_timeline::Sequence;
    use std::collections::BTreeSet;

    fn assert_expected_scope(
        evidence: RangeEditScopeEvidence,
        primary_track_id: TrackId,
        secondary_track_id: TrackId,
    ) {
        assert_eq!(
            evidence.targeted_track_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([primary_track_id])
        );
        assert_eq!(
            evidence.ripple_track_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([primary_track_id, secondary_track_id])
        );
    }

    #[test]
    fn range_edit_scope_converges_from_default_session_policy() -> anyhow::Result<()> {
        let sequence = Sequence::new("Golden range-edit scope");
        let sequence_id = sequence.id;
        let primary_track_id = sequence.video_tracks[0].id;
        let secondary_track_id = sequence.audio_tracks[0].id;
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));

        let evidence = configure_range_edit_scope(
            &mut state,
            sequence_id,
            primary_track_id,
            secondary_track_id,
        )?;

        assert_expected_scope(evidence, primary_track_id, secondary_track_id);
        Ok(())
    }

    #[test]
    fn range_edit_scope_converges_from_opposite_session_overrides() -> anyhow::Result<()> {
        let sequence = Sequence::new("Golden range-edit overrides");
        let sequence_id = sequence.id;
        let primary_track_id = sequence.video_tracks[0].id;
        let secondary_track_id = sequence.audio_tracks[0].id;
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.set_timeline_track_targeted(primary_track_id, false)?;
        state.set_timeline_track_sync_locked(primary_track_id, false)?;
        state.set_timeline_track_sync_locked(secondary_track_id, false)?;

        let evidence = configure_range_edit_scope(
            &mut state,
            sequence_id,
            primary_track_id,
            secondary_track_id,
        )?;

        assert_expected_scope(evidence, primary_track_id, secondary_track_id);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct EditorialAudioTrackBinding {
    primary: TrackId,
    secondary: TrackId,
}

fn bind_editorial_audio_tracks(
    state: &AppState,
    sequence_id: SequenceId,
) -> anyhow::Result<EditorialAudioTrackBinding> {
    let sequence =
        state.sequence_by_id(sequence_id).context("Editorial Hero Sequence is absent")?;
    ensure!(
        state.active_sequence_id() == Some(sequence_id),
        "Editorial Hero Sequence is not active"
    );
    let main_output_id = sequence
        .audio_program
        .outputs
        .first()
        .context("Editorial Hero has no Program Output")?
        .id;
    let default_track_channel = AudioTrackMixerChannel::default();
    let candidates = sequence
        .audio_tracks
        .iter()
        .filter(|track| {
            if track.is_locked || track.is_muted || !track.is_visible || !track.clips.is_empty() {
                return false;
            }
            if sequence.audio_program.track_channels.get(&track.id) != Some(&default_track_channel)
            {
                return false;
            }
            let routes = sequence
                .audio_program
                .routes
                .iter()
                .filter(|route| {
                    matches!(
                        route.source,
                        AudioRouteSource::Track { track_id, .. } if track_id == track.id
                    )
                })
                .collect::<Vec<_>>();
            routes.len() == 1
                && routes[0].enabled
                && routes[0].gain_db == 0.0
                && routes[0].gain_automation.is_none()
                && routes[0].source
                    == (AudioRouteSource::Track {
                        track_id: track.id,
                        port: AudioChannelStripOutputPort::PostMute,
                    })
                && routes[0].destination == AudioRouteDestination::Output(main_output_id)
        })
        .map(|track| track.id)
        .take(2)
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 2,
        "Editorial Hero requires two complete pristine audio Tracks"
    );
    Ok(EditorialAudioTrackBinding { primary: candidates[0], secondary: candidates[1] })
}

fn drop_audio(
    state: &mut AppState,
    asset_id: mondrian_core::AssetId,
    track_id: TrackId,
    frame: i64,
    intent: &'static str,
) -> anyhow::Result<(ClipId, AuthorTransitionEvidence)> {
    state.dispatch_action(assets_prepare_drag_action(AssetsPrepareDragPayload {
        asset_id,
    }))?;
    author_transition(state, intent, |state| {
        Ok(state.drop_dragging_asset_to_audio_track(track_id, frame)?)
    })
}

fn trim_audio_out(
    state: &mut AppState,
    clip_id: ClipId,
    end_frame_exclusive: i64,
    intent: &'static str,
) -> anyhow::Result<AuthorTransitionEvidence> {
    let time_base = state.active_sequence().context("active Sequence is absent")?.time_base();
    dispatch_author_transition(
        state,
        intent,
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            position: FramePosition::new(end_frame_exclusive, time_base),
        }),
    )
}

pub(super) fn execute_editorial_stage(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<GoldenEditorialReport> {
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == EDITORIAL_SLICE_ID)
        .context("editorial Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == ["aac-audio"],
        "editorial slice fixture contract drifted"
    );
    ensure!(
        slice.required_operations
            == [
                "play",
                "accurate-seek",
                "scrub",
                "insert",
                "overwrite",
                "ripple",
                "split",
                "lift",
                "extract"
            ],
        "editorial slice operation contract drifted"
    );
    ensure!(
        slice.required_content.is_empty() && slice.required_exports.is_empty(),
        "editorial slice may not claim content or export coverage"
    );
    let window = slice.timeline_window.context("editorial slice has no timeline window")?;
    ensure!(
        window.start_frame == 0 && window.end_frame_exclusive == 150,
        "editorial slice timeline window drifted"
    );

    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let fixture = resolve_fixture(root, &fixture_root(root), contract, &manifest, "aac-audio")?;
    let stage = workflow.bind_slice_primary_sequence(contract, EDITORIAL_SLICE_ID)?;
    let state = workflow.app_mut();
    state.dispatch_action(Action::ImportMedia(vec![fixture.path.clone()]))?;
    wait_for_media_imports(state)?;
    let asset = state
        .asset_library()
        .context("Asset Library is absent after AAC import")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.file_path() == Some(fixture.path.as_path()))
        .context("imported AAC fixture is absent from the Asset Library")?;
    ensure!(
        asset.kind == AssetKind::Audio,
        "AAC fixture imported as non-audio media"
    );
    let audio = asset
        .media_probe()
        .context("AAC fixture has no coherent media probe")?
        .primary_audio()
        .context("AAC fixture has no audio stream")?;
    ensure!(
        audio.codec == AudioCodec::Aac
            && audio.sample_rate == contract.timeline.audio_sample_rate
            && audio.channels == 2
            && audio.channel_layout == ChannelLayout::Stereo,
        "AAC stream contract differs from the Golden timeline"
    );
    let imported_audio = ImportedAacObservation {
        stream_index: audio.index,
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        channel_layout: audio.channel_layout.clone(),
        average_bitrate: audio.avg_bitrate,
    };
    let track_binding = bind_editorial_audio_tracks(state, stage.sequence_id())?;
    let track_id = track_binding.primary;
    let secondary_track_id = track_binding.secondary;
    let mut setup_steps = Vec::new();

    let (left_clip_id, left_drop) = drop_audio(state, asset.id, track_id, 0, "drop-left-aac")?;
    setup_steps.push(left_drop);
    setup_steps.push(trim_audio_out(state, left_clip_id, 100, "trim-left-aac")?);

    let (replacement_clip_id, overwrite_step) =
        drop_audio(state, asset.id, track_id, 25, "overwrite-left-aac")?;
    setup_steps.push(trim_audio_out(
        state,
        replacement_clip_id,
        75,
        "trim-overwrite-aac",
    )?);
    let retained_left_range = clip_range(state, track_id, left_clip_id)?;
    let replacement_range = clip_range(state, track_id, replacement_clip_id)?;
    ensure!(
        retained_left_range == (ClipRangeObservation { start_frame: 0, end_frame_exclusive: 25 })
            && replacement_range
                == (ClipRangeObservation { start_frame: 25, end_frame_exclusive: 75 }),
        "overwrite did not retain only the non-overlapped left fragment"
    );
    let overwrite_evidence = OperationEvidence::Overwrite {
        author_step: overwrite_step,
        retained_left_clip_id: left_clip_id,
        retained_left_range,
        replacement_clip_id,
        replacement_range,
    };

    let (downstream_clip_id, downstream_drop) =
        drop_audio(state, asset.id, track_id, 100, "drop-downstream-aac")?;
    setup_steps.push(downstream_drop);
    setup_steps.push(trim_audio_out(
        state,
        downstream_clip_id,
        125,
        "trim-downstream-aac",
    )?);
    let downstream_before = clip_range(state, track_id, downstream_clip_id)?;

    let (split_outcome, split_step) = author_transition(state, "split-overwrite-aac", |state| {
        state
            .split_clip_at_frame(track_id, false, replacement_clip_id, 50)?
            .context("targeted AAC Clip was not splittable at frame 50")
    })?;
    ensure!(
        split_outcome.primary().left_clip_id == replacement_clip_id
            && split_outcome.linked_members().is_empty(),
        "targeted AAC split changed undeclared linked placements"
    );
    let replacement_range_after_split = clip_range(state, track_id, replacement_clip_id)?;
    let right_clip_id = split_outcome.primary().right_clip_id;
    let right_range = clip_range(state, track_id, right_clip_id)?;
    ensure!(
        replacement_range_after_split
            == (ClipRangeObservation { start_frame: 25, end_frame_exclusive: 50 }),
        "split did not retain the expected left range"
    );
    let split_evidence = OperationEvidence::Split {
        author_step: split_step,
        left_clip_id: replacement_clip_id,
        left_range: replacement_range_after_split,
        right_clip_id,
        right_range,
    };

    state.dispatch_action(Action::Select(SelectionTarget::Clip(right_clip_id)))?;
    let ripple_step = dispatch_author_transition(
        state,
        "ripple-delete-split-aac",
        Action::RippleDeleteSelection,
    )?;
    ensure!(
        audio_track(state, track_id)?.clips.iter().all(|clip| clip.id != right_clip_id),
        "Ripple Delete retained the selected right Clip"
    );
    let downstream_after = clip_range(state, track_id, downstream_clip_id)?;
    ensure!(
        downstream_before == (ClipRangeObservation { start_frame: 100, end_frame_exclusive: 125 })
            && downstream_after
                == (ClipRangeObservation { start_frame: 75, end_frame_exclusive: 100 }),
        "Ripple Delete did not shift downstream material by the removed duration"
    );
    let ripple_evidence = OperationEvidence::Ripple {
        author_step: ripple_step,
        removed_clip_id: right_clip_id,
        downstream_clip_id,
        downstream_before,
        downstream_after,
    };

    let (secondary_downstream_clip_id, secondary_drop) = drop_audio(
        state,
        asset.id,
        secondary_track_id,
        75,
        "drop-secondary-insert-aac",
    )?;
    setup_steps.push(secondary_drop);
    setup_steps.push(trim_audio_out(
        state,
        secondary_downstream_clip_id,
        100,
        "trim-secondary-insert-aac",
    )?);
    let primary_before_insert = clip_range(state, track_id, downstream_clip_id)?;
    let secondary_before_insert =
        clip_range(state, secondary_track_id, secondary_downstream_clip_id)?;
    let (insert_outcome, insert_step) =
        author_transition(state, "insert-aac-multitrack", |state| {
            let time_base = state
                .active_sequence()
                .context("active Sequence is absent before Insert")?
                .time_base();
            Ok(state.insert_asset_from_ui(TimelineInsertAssetPayload {
                asset_id: asset.id,
                at: TimelineTime::from_frame_position(FramePosition::new(60, time_base))?,
                source_in: TimelineTime::ZERO,
                duration: TimelineTime::from_frame_position(FramePosition::new(10, time_base))?,
                video_target_track_id: None,
                audio_target_track_id: Some(track_id),
                ripple_track_ids: vec![track_id, secondary_track_id],
                automation_policy: InsertAutomationPolicy::FollowEditorialContent,
                transition_policy: InsertTransitionPolicy::RejectAffected,
                timeline_state_policy: InsertTimelineStatePolicy::FollowEdit,
            })?)
        })?;
    ensure!(
        insert_outcome.inserted_clip_ids.len() == 1
            && insert_outcome.shifted_clip_ids.iter().copied().collect::<BTreeSet<_>>()
                == [downstream_clip_id, secondary_downstream_clip_id]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
            && insert_outcome.split_clips.is_empty()
            && insert_outcome.removed_video_transition_ids.is_empty()
            && insert_outcome.removed_audio_transition_ids.is_empty(),
        "Insert Edit outcome changed undeclared placements or Transitions"
    );
    let inserted_clip_id = insert_outcome
        .inserted_clip_ids
        .first()
        .copied()
        .context("Insert Edit returned no placed AAC Clip")?;
    let inserted_range = clip_range(state, track_id, inserted_clip_id)?;
    let primary_after_insert = clip_range(state, track_id, downstream_clip_id)?;
    let secondary_after_insert =
        clip_range(state, secondary_track_id, secondary_downstream_clip_id)?;
    ensure!(
        inserted_range == (ClipRangeObservation { start_frame: 60, end_frame_exclusive: 70 })
            && primary_before_insert
                == (ClipRangeObservation { start_frame: 75, end_frame_exclusive: 100 })
            && primary_after_insert
                == (ClipRangeObservation { start_frame: 85, end_frame_exclusive: 110 })
            && secondary_before_insert
                == (ClipRangeObservation { start_frame: 75, end_frame_exclusive: 100 })
            && secondary_after_insert
                == (ClipRangeObservation { start_frame: 85, end_frame_exclusive: 110 }),
        "Insert Edit did not open one exact ten-frame gap across both ripple Tracks"
    );
    let insert_evidence = OperationEvidence::Insert {
        author_step: insert_step,
        inserted_clip_id,
        inserted_range,
        ripple_track_ids: vec![track_id, secondary_track_id],
        primary_downstream_clip_id: downstream_clip_id,
        primary_before: primary_before_insert,
        primary_after: primary_after_insert,
        secondary_downstream_clip_id,
        secondary_before: secondary_before_insert,
        secondary_after: secondary_after_insert,
    };

    let range_edit_scope =
        configure_range_edit_scope(state, stage.sequence_id(), track_id, secondary_track_id)?;

    let lift_range = ClipRangeObservation { start_frame: 60, end_frame_exclusive: 70 };
    let lift_downstream_before = clip_range(state, track_id, downstream_clip_id)?;
    let lift_range_setup_steps =
        set_in_out_range(state, lift_range, "set-lift-in-aac", "set-lift-out-aac")?;
    let lift_step =
        dispatch_author_transition(state, "lift-inserted-aac", timeline_lift_range_action())?;
    ensure_clip_absent(state, track_id, inserted_clip_id, "Lift")?;
    let lift_downstream_after = clip_range(state, track_id, downstream_clip_id)?;
    ensure!(
        sequence_in_out_range(state)?.is_none()
            && state.current_frame() == lift_range.start_frame
            && lift_downstream_before == primary_after_insert
            && lift_downstream_after == lift_downstream_before,
        "Lift did not remove only the targeted content while preserving program time"
    );

    let lift_undo_step = dispatch_author_transition(state, "undo-lift-aac", Action::Undo)?;
    ensure!(
        clip_range(state, track_id, inserted_clip_id)? == lift_range
            && clip_range(state, track_id, downstream_clip_id)? == lift_downstream_before
            && sequence_in_out_range(state)? == Some(lift_range),
        "Undo did not restore the complete pre-Lift author state in one step"
    );
    let lift_redo_step = dispatch_author_transition(state, "redo-lift-aac", Action::Redo)?;
    ensure_clip_absent(state, track_id, inserted_clip_id, "Redo Lift")?;
    ensure!(
        clip_range(state, track_id, downstream_clip_id)? == lift_downstream_after
            && sequence_in_out_range(state)?.is_none(),
        "Redo did not restore the complete post-Lift author state in one step"
    );
    let lift_evidence = OperationEvidence::Lift {
        range_setup_steps: lift_range_setup_steps,
        author_step: lift_step,
        undo_step: lift_undo_step,
        redo_step: lift_redo_step,
        scope: range_edit_scope.clone(),
        removed_clip_id: inserted_clip_id,
        removed_range: lift_range,
        primary_downstream_clip_id: downstream_clip_id,
        downstream_before: lift_downstream_before,
        downstream_after: lift_downstream_after,
    };

    let extract_range = ClipRangeObservation { start_frame: 45, end_frame_exclusive: 50 };
    let extract_trimmed_before = clip_range(state, track_id, replacement_clip_id)?;
    let extract_primary_before = clip_range(state, track_id, downstream_clip_id)?;
    let extract_secondary_before =
        clip_range(state, secondary_track_id, secondary_downstream_clip_id)?;
    let extract_range_setup_steps = set_in_out_range(
        state,
        extract_range,
        "set-extract-in-aac",
        "set-extract-out-aac",
    )?;
    let extract_step = dispatch_author_transition(
        state,
        "extract-aac-multitrack",
        timeline_extract_range_action(),
    )?;
    let extract_trimmed_after = clip_range(state, track_id, replacement_clip_id)?;
    let extract_primary_after = clip_range(state, track_id, downstream_clip_id)?;
    let extract_secondary_after =
        clip_range(state, secondary_track_id, secondary_downstream_clip_id)?;
    ensure!(
        extract_trimmed_before
            == (ClipRangeObservation { start_frame: 25, end_frame_exclusive: 50 })
            && extract_trimmed_after
                == (ClipRangeObservation { start_frame: 25, end_frame_exclusive: 45 })
            && extract_primary_before
                == (ClipRangeObservation { start_frame: 85, end_frame_exclusive: 110 })
            && extract_primary_after
                == (ClipRangeObservation { start_frame: 80, end_frame_exclusive: 105 })
            && extract_secondary_before
                == (ClipRangeObservation { start_frame: 85, end_frame_exclusive: 110 })
            && extract_secondary_after
                == (ClipRangeObservation { start_frame: 80, end_frame_exclusive: 105 })
            && sequence_in_out_range(state)?.is_none()
            && state.current_frame() == extract_range.start_frame,
        "Extract did not trim targeted content and close exactly five frames on Sync-Locked Tracks"
    );

    let extract_undo_step = dispatch_author_transition(state, "undo-extract-aac", Action::Undo)?;
    ensure!(
        clip_range(state, track_id, replacement_clip_id)? == extract_trimmed_before
            && clip_range(state, track_id, downstream_clip_id)? == extract_primary_before
            && clip_range(state, secondary_track_id, secondary_downstream_clip_id)?
                == extract_secondary_before
            && sequence_in_out_range(state)? == Some(extract_range),
        "Undo did not restore the complete pre-Extract author state in one step"
    );
    let extract_redo_step = dispatch_author_transition(state, "redo-extract-aac", Action::Redo)?;
    ensure!(
        clip_range(state, track_id, replacement_clip_id)? == extract_trimmed_after
            && clip_range(state, track_id, downstream_clip_id)? == extract_primary_after
            && clip_range(state, secondary_track_id, secondary_downstream_clip_id)?
                == extract_secondary_after
            && sequence_in_out_range(state)?.is_none(),
        "Redo did not restore the complete post-Extract author state in one step"
    );
    let extract_evidence = OperationEvidence::Extract {
        range_setup_steps: extract_range_setup_steps,
        author_step: extract_step,
        undo_step: extract_undo_step,
        redo_step: extract_redo_step,
        scope: range_edit_scope,
        trimmed_clip_id: replacement_clip_id,
        trimmed_before: extract_trimmed_before,
        trimmed_after: extract_trimmed_after,
        primary_downstream_clip_id: downstream_clip_id,
        primary_before: extract_primary_before,
        primary_after: extract_primary_after,
        secondary_downstream_clip_id,
        secondary_before: extract_secondary_before,
        secondary_after: extract_secondary_after,
    };

    let mut viewer =
        GoldenHeadlessPreview::new(std::time::Instant::now() + VIEWER_PRESENTATION_TIMEOUT)?;
    let time_base = state.active_sequence().context("active Sequence is absent")?.time_base();
    let scrub_before = state.playback_evidence_report();
    let target_frames = vec![5, 15, 30];
    let mut scrub_presentations = Vec::with_capacity(target_frames.len());
    for frame in &target_frames {
        state.dispatch_action(timeline_seek_with_source_action(
            FramePosition::new(*frame, time_base),
            TimelineSeekSource::PointerDrag,
        ))?;
        scrub_presentations.push(viewer.present_current(state, VIEWER_PRESENTATION_TIMEOUT)?);
    }
    let scrub_report = state.playback_evidence_report();
    let scrub_ready_deliveries = scrub_report
        .deliveries
        .ready
        .checked_sub(scrub_before.deliveries.ready)
        .context("scrub ready-delivery counter regressed")?;
    let warm_seek_count = scrub_report
        .warm_seek_latency
        .count
        .checked_sub(scrub_before.warm_seek_latency.count)
        .context("warm-seek counter regressed")?;
    ensure!(
        state.current_frame() == 30
            && warm_seek_count == target_frames.len() as u64
            && scrub_ready_deliveries == target_frames.len() as u64,
        "pointer-drag seeks did not produce exact stage-local scrub evidence"
    );
    let scrub_evidence = OperationEvidence::Scrub {
        target_frames,
        final_frame: state.current_frame(),
        ready_deliveries: scrub_ready_deliveries,
        warm_seek_count,
        presentations: scrub_presentations,
    };

    let accurate_target = 40;
    let accurate_before = state.playback_evidence_report();
    state.dispatch_action(timeline_seek_with_source_action(
        FramePosition::new(accurate_target, time_base),
        TimelineSeekSource::Settled,
    ))?;
    let accurate_presentation = viewer.present_current(state, VIEWER_PRESENTATION_TIMEOUT)?;
    let accurate_report = state.playback_evidence_report();
    let accurate_ready_deliveries = accurate_report
        .deliveries
        .ready
        .checked_sub(accurate_before.deliveries.ready)
        .context("accurate-seek ready-delivery counter regressed")?;
    let accurate_seek_count = accurate_report
        .accurate_seek_latency
        .count
        .checked_sub(accurate_before.accurate_seek_latency.count)
        .context("accurate-seek counter regressed")?;
    ensure!(
        state.current_frame() == accurate_target
            && accurate_seek_count == 1
            && accurate_ready_deliveries == 1,
        "settled seek did not produce exact stage-local accurate-seek evidence"
    );
    let accurate_evidence = OperationEvidence::AccurateSeek {
        target_frame: accurate_target,
        final_frame: state.current_frame(),
        ready_deliveries: accurate_ready_deliveries,
        accurate_seek_count,
        presentation: accurate_presentation,
    };

    let play_start = state.current_frame();
    let play_before = state.playback_evidence_report();
    state.dispatch_action(Action::Play)?;
    let play_presentation = viewer.present_current(state, VIEWER_PRESENTATION_TIMEOUT)?;
    ensure!(
        !state.is_playback_priming(),
        "production Preview preroll did not release the Playback clock anchor"
    );
    let advance = state.advance_playback_clock(Duration::from_millis(80));
    ensure!(
        advance.status == PlaybackAdvanceStatus::Advanced
            && advance.current_frame > play_start
            && state.playback_clock_master() == Some(ClockMaster::Synthetic),
        "production Transport did not advance from the Synthetic Clock Master"
    );
    state.dispatch_action(Action::Pause)?;
    let play_report = state.playback_evidence_report();
    let synthetic_clock_residency_us = play_report
        .clock_residency
        .synthetic_us
        .checked_sub(play_before.clock_residency.synthetic_us)
        .context("Synthetic Clock residency counter regressed")?;
    ensure!(
        synthetic_clock_residency_us > 0,
        "playback evidence retained no stage-local Synthetic Clock residency"
    );
    let play_evidence = OperationEvidence::Play {
        start_frame: play_start,
        final_frame: advance.current_frame,
        frames_advanced: advance.frames_advanced,
        clock_master: "synthetic",
        synthetic_clock_residency_us,
        presentation: play_presentation,
    };

    let operations = vec![
        play_evidence,
        accurate_evidence,
        scrub_evidence,
        insert_evidence,
        overwrite_evidence,
        ripple_evidence,
        split_evidence,
        lift_evidence,
        extract_evidence,
    ];
    ensure_exact_requirement_evidence(
        &slice.required_operations,
        operations.iter().map(OperationEvidence::id),
        "operation",
    )?;
    let viewer = viewer.evidence();
    ensure!(
        viewer.presentations == 5
            && viewer.completed_demands == 5
            && viewer.gpu_executions
                + viewer.current_gpu_presentations
                + viewer.cpu_raster_presentations
                == 5,
        "Editorial transport did not present exactly five real Hero Viewer outputs"
    );
    let persistence = workflow.durable_save_reopen_for(&stage)?;
    workflow.verify_binding()?;
    let authoring = capture_editorial_authoring_anchor(
        workflow.app(),
        stage.sequence_id(),
        asset.id,
        [track_id, secondary_track_id],
    )?;

    Ok(GoldenEditorialReport {
        schema_version: 4,
        profile: EDITORIAL_SLICE_ID,
        contract_id: contract.id.clone(),
        corpus_revision: manifest.corpus_revision,
        status: GoldenRunStatus::Passed,
        complete_golden_project: false,
        fixture,
        setup: EditorialSetupEvidence {
            stage,
            primary_audio_track_id: track_id,
            secondary_audio_track_id: secondary_track_id,
            asset_id: asset.id,
            imported_audio,
            setup_steps,
        },
        operations,
        viewer,
        persistence,
        authoring,
    })
}
