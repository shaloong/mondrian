//! Product Adapter for direct Asset placement on the active Timeline.
//!
//! The Widget supplies stable Asset/Track identity plus an explicitly gridded
//! coordinate. This Module resolves current Track media kind, lowers time once,
//! prepares the current Asset binding, and owns status plus execution routing.

use super::media_import::MediaImportPreparedCandidate;
use super::product_action::{
    AssetTargetPayload, TimelineDropAssetPayload, TimelineDropFilePayload,
};
use super::timeline_position::lower_exact_sequence_frame;
use super::{resolve_track_conflicts, Clip};
use super::{AppState, DraggingAsset};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::types::{AssetId, AudioSourceComponentId, ClipId, ClipLinkGroupId, TrackId};
use mondrian_core::{FramePosition, MondrianError, ResolvedPictureGeometry, TimelineTime};
use mondrian_timeline::{ClipOverlapMode, Sequence, TrackType};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementTrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreparedAssetPlacement {
    frame: i64,
    track_kind: PlacementTrackKind,
}

impl AppState {
    /// Whether an external file can be queued for this Track without changing
    /// the Asset Library or authoring state.
    pub fn can_queue_file_on_timeline(&self, payload: &TimelineDropFilePayload) -> bool {
        !payload.path.as_os_str().is_empty()
            && self.asset_library_handle().is_some()
            && self.active_sequence().is_some_and(|sequence| {
                resolve_placement_target(sequence, payload.target_track_id, payload.position)
                    .is_ok()
            })
    }

    /// Queue isolated media probing after the pointer has committed a valid
    /// Track/body target. No library mutation happens until the probe returns.
    pub fn queue_file_on_timeline(
        &mut self,
        payload: TimelineDropFilePayload,
    ) -> mondrian_core::Result<()> {
        if !self.can_queue_file_on_timeline(&payload) {
            return Err(placement_error("文件或目标轨道不可用"));
        }
        let admission = self
            .media_import
            .admit_deferred_file(payload.path.clone())
            .map_err(|error| placement_error(format!("无法排队探测外部文件：{error}")))?;
        self.pending_timeline_file_drops.insert(admission.batch_id, payload);
        let _ = self.refresh_internal_execution_resource_decision();
        self.set_status_hint("正在探测拖入轨道的媒体文件...".to_owned(), false);
        Ok(())
    }

    pub(super) fn commit_staged_file_drop(
        &mut self,
        payload: TimelineDropFilePayload,
        candidate: MediaImportPreparedCandidate,
    ) -> mondrian_core::Result<AssetId> {
        let sequence = self.active_sequence().ok_or_else(|| placement_error("当前无序列"))?;
        let sequence_id = sequence.id;
        let (frame, track_kind) =
            resolve_placement_target(sequence, payload.target_track_id, payload.position)?;
        let still_duration = self.default_visual_placement_drag_duration()?;
        let library = self.asset_library_handle().ok_or_else(|| placement_error("素材库未连接"))?;
        let probe = candidate.into_probe_candidate()?;
        let authoring = std::cell::RefCell::new(
            self.authoring.as_mut().ok_or_else(|| placement_error("当前没有打开的项目"))?,
        );
        let (asset_id, (clip_id, start_frame, commit)) = library.commit_media_probe_with_install(
            probe,
            None,
            |asset| {
                validate_placement_asset(track_kind, asset)?;
                let dragging = staged_dragging_asset(asset, still_duration)?;
                let picture = staged_picture(asset)?;
                let ((clip_id, start_frame), prepared) =
                    authoring.borrow_mut().prepare_sequence_edit(
                        sequence_id,
                        "导入文件并添加片段",
                        |sequence| match track_kind {
                            PlacementTrackKind::Video => add_staged_video_clip(
                                sequence,
                                &dragging,
                                picture,
                                payload.target_track_id,
                                frame,
                            ),
                            PlacementTrackKind::Audio => add_staged_audio_clip(
                                sequence,
                                &dragging,
                                payload.target_track_id,
                                frame,
                            ),
                        },
                    )?;
                let prepared = prepared.ok_or_else(|| placement_error("轨道放置没有产生编辑"))?;
                Ok((clip_id, start_frame, prepared))
            },
            |(clip_id, start_frame, prepared)| {
                let commit = authoring.borrow_mut().install_prepared_sequence_edit(prepared);
                (clip_id, start_frame, commit)
            },
        )?;
        self.consume_authoring_commit(commit);
        self.event_bus
            .publish(mondrian_core::events::AppEvent::AssetImported { asset_id });
        self.event_bus
            .publish(mondrian_core::events::AppEvent::ClipAdded { sequence_id, clip_id });
        self.reconcile_playhead_after_committed_authoring_change(
            start_frame,
            "drop_external_file_to_track",
        );
        let _ = self.configure_imported_asset(asset_id);
        self.set_status_hint("已将文件导入素材库并添加到轨道".to_owned(), false);
        Ok(asset_id)
    }
    /// Whether one direct Asset placement is coherent with current author state.
    pub fn can_place_asset_on_timeline(&self, payload: TimelineDropAssetPayload) -> bool {
        self.prepare_timeline_asset_placement(payload).is_ok()
    }

    /// Place one Asset on the authoritative current Track at exact frame time.
    pub fn place_asset_on_timeline(
        &mut self,
        payload: TimelineDropAssetPayload,
    ) -> mondrian_core::Result<()> {
        let prepared = match self.prepare_timeline_asset_placement(payload) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.set_status_hint(format!("素材放置失败：{error}"), true);
                return Err(error);
            }
        };

        let needs_prepare = self
            .dragging_asset()
            .is_none_or(|dragging| dragging.asset_id != payload.asset_id);
        if needs_prepare {
            self.prepare_asset_drag(AssetTargetPayload { asset_id: payload.asset_id })?;
        }

        let result = match prepared.track_kind {
            PlacementTrackKind::Video => {
                self.drop_dragging_asset_to_video_track(payload.target_track_id, prepared.frame)
            }
            PlacementTrackKind::Audio => {
                self.drop_dragging_asset_to_audio_track(payload.target_track_id, prepared.frame)
            }
        };

        match result {
            Ok(_) => {
                self.set_status_hint("已添加素材到时间线".to_owned(), false);
                Ok(())
            }
            Err(error) => {
                self.set_status_hint(format!("素材放置失败：{error}"), true);
                Err(error)
            }
        }
    }

    fn prepare_timeline_asset_placement(
        &self,
        payload: TimelineDropAssetPayload,
    ) -> mondrian_core::Result<PreparedAssetPlacement> {
        let sequence = self.active_sequence().ok_or_else(|| placement_error("当前无序列"))?;
        let (frame, track_kind) =
            resolve_placement_target(sequence, payload.target_track_id, payload.position)?;
        let asset = self
            .asset_library_handle()
            .ok_or_else(|| placement_error("素材库未连接"))?
            .get_asset(payload.asset_id)?
            .ok_or_else(|| MondrianError::AssetNotFound {
                asset_id: payload.asset_id.to_string(),
            })?;
        validate_placement_asset(track_kind, &asset)?;
        Ok(PreparedAssetPlacement { frame, track_kind })
    }
}

fn resolve_placement_target(
    sequence: &mondrian_timeline::Sequence,
    target_track_id: TrackId,
    position: FramePosition,
) -> mondrian_core::Result<(i64, PlacementTrackKind)> {
    let frame = lower_exact_sequence_frame(sequence, position, "timeline_place_asset")?;

    let track = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .find(|track| track.id == target_track_id)
        .ok_or_else(|| MondrianError::TrackNotFound { track_id: target_track_id.to_string() })?;
    if track.is_locked {
        return Err(MondrianError::TrackLocked { track_id: track.id.to_string() });
    }
    let kind = match track.track_type {
        TrackType::Video => PlacementTrackKind::Video,
        TrackType::Audio => PlacementTrackKind::Audio,
        TrackType::Subtitle => {
            return Err(MondrianError::UnsupportedFormat {
                format: "Asset placement on Subtitle Tracks is not supported".to_owned(),
            });
        }
    };
    Ok((frame, kind))
}

fn validate_placement_asset(
    track_kind: PlacementTrackKind,
    asset: &AssetRecord,
) -> mondrian_core::Result<()> {
    let compatible = match (track_kind, asset.kind.clone()) {
        (PlacementTrackKind::Video, AssetKind::Video) => asset
            .media_probe()
            .is_some_and(|probe| !probe.duration.is_zero() && probe.primary_video().is_some()),
        (PlacementTrackKind::Video, AssetKind::StillImage) => {
            asset.media_probe().is_some_and(|probe| probe.primary_video().is_some())
        }
        (PlacementTrackKind::Video, AssetKind::AdjustmentLayer | AssetKind::SolidColor) => true,
        (PlacementTrackKind::Audio, AssetKind::Audio) => asset
            .media_probe()
            .is_some_and(|probe| !probe.duration.is_zero() && probe.primary_audio().is_some()),
        _ => false,
    };
    if compatible {
        if track_kind == PlacementTrackKind::Video
            && matches!(asset.kind, AssetKind::Video | AssetKind::StillImage)
        {
            let video = asset
                .media_probe()
                .and_then(|probe| probe.primary_video())
                .ok_or_else(|| placement_error("素材没有可执行的视频流"))?;
            mondrian_core::ResolvedPictureGeometry::resolve(
                mondrian_core::Resolution { width: video.width, height: video.height },
                video.picture,
                None,
                None,
            )
            .map_err(|error| placement_error(format!("素材图片解释不受支持：{error}")))?;
        }
        Ok(())
    } else {
        Err(MondrianError::UnsupportedFormat {
            format: format!(
                "Asset {} has no coherent {:?} placement for the target Track",
                asset.id, track_kind
            ),
        })
    }
}

fn staged_dragging_asset(
    asset: &AssetRecord,
    still_duration: Duration,
) -> mondrian_core::Result<DraggingAsset> {
    let probe = asset.media_probe().ok_or_else(|| placement_error("媒体探测证据已失效"))?;
    let duration = if asset.kind == AssetKind::StillImage {
        still_duration
    } else {
        probe.duration
    };
    if duration.is_zero() {
        return Err(placement_error("媒体没有可放置的正时长"));
    }
    Ok(DraggingAsset {
        asset_id: asset.id,
        name: asset.name.clone(),
        kind: asset.kind.clone(),
        duration,
        has_linked_audio: asset.kind == AssetKind::Video && probe.has_audio,
    })
}

fn staged_picture(asset: &AssetRecord) -> mondrian_core::Result<Option<ResolvedPictureGeometry>> {
    if !matches!(asset.kind, AssetKind::Video | AssetKind::StillImage) {
        return Ok(None);
    }
    let video = asset
        .media_probe()
        .and_then(|probe| probe.primary_video())
        .ok_or_else(|| placement_error("素材没有可执行的视频流"))?;
    ResolvedPictureGeometry::resolve(
        mondrian_core::Resolution { width: video.width, height: video.height },
        video.picture,
        None,
        None,
    )
    .map(Some)
    .map_err(|error| placement_error(format!("素材图片解释不受支持：{error}")))
}

fn staged_time(frame: i64, sequence: &Sequence) -> mondrian_core::Result<TimelineTime> {
    Ok(TimelineTime::from_frame_position(FramePosition::new(
        frame,
        sequence.time_base(),
    ))?)
}

fn staged_duration_frames(sequence: &Sequence, dragging: &DraggingAsset) -> i64 {
    ((dragging.duration.as_secs_f64() * sequence.settings.frame_rate.to_f64()).ceil() as i64).max(1)
}

fn add_staged_video_clip(
    sequence: &mut Sequence,
    dragging: &DraggingAsset,
    picture: Option<ResolvedPictureGeometry>,
    track_id: TrackId,
    frame: i64,
) -> mondrian_core::Result<(ClipId, i64)> {
    let start_frame = frame.max(0);
    let start_time = staged_time(start_frame, sequence)?;
    let duration = staged_time(staged_duration_frames(sequence, dragging), sequence)?;
    let mut clip = if dragging.kind == AssetKind::StillImage {
        Clip::new_still_image(dragging.asset_id, start_time, duration)?
    } else {
        Clip::new(dragging.asset_id, start_time, duration)?
    };
    clip.label = Some(dragging.name.clone());
    if let Some(picture) = picture {
        super::timeline_insert::auto_fit_picture(sequence, &mut clip, picture)?;
    }
    let clip_id = clip.id;
    let linked_audio = if dragging.kind == AssetKind::Video && dragging.has_linked_audio {
        let mut audio_clip = Clip::new(dragging.asset_id, start_time, duration)?;
        audio_clip.label = Some(dragging.name.clone());
        let link_group = ClipLinkGroupId::new();
        clip.link_group = Some(link_group);
        audio_clip.link_group = Some(link_group);
        Some(audio_clip)
    } else {
        None
    };
    let track = sequence
        .video_track_mut(track_id)
        .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })?;
    track.add_clip(clip)?;
    resolve_track_conflicts(track, clip_id, ClipOverlapMode::Overwrite)?;
    if let Some(audio_clip) = linked_audio {
        let audio_clip_id = audio_clip.id;
        let video_index = sequence
            .video_tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })?;
        sequence.ensure_audio_track_index(video_index);
        if let Some(audio_track_id) = sequence.audio_tracks.get(video_index).map(|track| track.id) {
            sequence.add_media_audio_clip(
                audio_track_id,
                audio_clip,
                AudioSourceComponentId::primary(),
            )?;
            let audio_track = sequence.audio_track_mut(audio_track_id).ok_or_else(|| {
                MondrianError::TrackNotFound { track_id: audio_track_id.to_string() }
            })?;
            resolve_track_conflicts(audio_track, audio_clip_id, ClipOverlapMode::Overwrite)?;
        }
    }
    sequence.compact_structural_references();
    Ok((clip_id, start_frame))
}

fn add_staged_audio_clip(
    sequence: &mut Sequence,
    dragging: &DraggingAsset,
    track_id: TrackId,
    frame: i64,
) -> mondrian_core::Result<(ClipId, i64)> {
    let start_frame = frame.max(0);
    let mut clip = Clip::new(
        dragging.asset_id,
        staged_time(start_frame, sequence)?,
        staged_time(staged_duration_frames(sequence, dragging), sequence)?,
    )?;
    clip.label = Some(dragging.name.clone());
    let clip_id = clip.id;
    sequence.add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())?;
    let track = sequence
        .audio_track_mut(track_id)
        .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })?;
    resolve_track_conflicts(track, clip_id, ClipOverlapMode::Overwrite)?;
    sequence.compact_structural_references();
    Ok((clip_id, start_frame))
}

fn placement_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_place_asset".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared_png(path: &std::path::Path) -> MediaImportPreparedCandidate {
        image::RgbaImage::from_pixel(2, 2, image::Rgba([127, 31, 63, 255]))
            .save(path)
            .expect("write actual PNG");
        let path = path.canonicalize().expect("canonical PNG");
        MediaImportPreparedCandidate {
            source_fingerprint: mondrian_core::MediaFileFingerprint::capture(&path),
            info: mondrian_media::probe_media_info(&path).expect("probe PNG"),
            canonical_path: path,
            folder_id: None,
        }
    }

    #[test]
    fn external_png_drop_commits_one_asset_and_one_still_clip() {
        let root = tempfile::tempdir().expect("fixture directory");
        let library =
            mondrian_assets::AssetLibrary::open(root.path().join("library")).expect("library");
        let mut sequence = Sequence::new("drop");
        let track_id = sequence.add_video_track();
        let time_base = sequence.time_base();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(std::sync::Arc::clone(&library)));
        let candidate = prepared_png(&root.path().join("still.png"));
        let old_revision = library.database_revision().expect("old revision");
        let asset_id = state
            .commit_staged_file_drop(
                TimelineDropFilePayload {
                    path: candidate.canonical_path.clone(),
                    target_track_id: track_id,
                    position: FramePosition::new(12, time_base),
                },
                candidate,
            )
            .expect("atomic placement");

        assert_eq!(library.list_assets().expect("assets").len(), 1);
        let sequence = state.active_sequence().expect("active sequence");
        let target = sequence
            .video_tracks
            .iter()
            .find(|track| track.id == track_id)
            .expect("target track");
        assert_eq!(target.clips.len(), 1);
        assert_eq!(target.clips[0].library_asset_id(), Some(asset_id));
        assert_eq!(
            target.clips[0].position,
            TimelineTime::from_frame_position(FramePosition::new(12, time_base)).expect("time")
        );
        assert!(state.can_undo_action());
        assert!(library
            .snapshot_database(old_revision, &root.path().join("old-save.db"))
            .is_err());
    }

    #[test]
    fn incompatible_external_drop_rolls_back_library_and_authoring() {
        let root = tempfile::tempdir().expect("fixture directory");
        let library =
            mondrian_assets::AssetLibrary::open(root.path().join("library")).expect("library");
        let mut sequence = Sequence::new("drop");
        let track_id = sequence.add_audio_track();
        let time_base = sequence.time_base();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(std::sync::Arc::clone(&library)));
        let candidate = prepared_png(&root.path().join("still.png"));
        let generation = state.project_author_generation();
        let result = state.commit_staged_file_drop(
            TimelineDropFilePayload {
                path: candidate.canonical_path.clone(),
                target_track_id: track_id,
                position: FramePosition::new(12, time_base),
            },
            candidate,
        );
        assert!(result.is_err());
        assert!(library.list_assets().expect("assets after rejection").is_empty());
        assert!(state
            .active_sequence()
            .expect("sequence")
            .audio_tracks
            .iter()
            .find(|track| track.id == track_id)
            .expect("target track")
            .clips
            .is_empty());
        assert_eq!(state.project_author_generation(), generation);
    }

    #[test]
    fn locked_track_rejects_prepared_file_without_importing_it() {
        let root = tempfile::tempdir().expect("fixture directory");
        let library =
            mondrian_assets::AssetLibrary::open(root.path().join("library")).expect("library");
        let mut sequence = Sequence::new("drop");
        let track_id = sequence.add_video_track();
        sequence.video_track_mut(track_id).expect("target").is_locked = true;
        let time_base = sequence.time_base();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(std::sync::Arc::clone(&library)));
        let candidate = prepared_png(&root.path().join("still.png"));

        assert!(state
            .commit_staged_file_drop(
                TimelineDropFilePayload {
                    path: candidate.canonical_path.clone(),
                    target_track_id: track_id,
                    position: FramePosition::new(12, time_base),
                },
                candidate
            )
            .is_err());
        assert!(library.list_assets().expect("assets").is_empty());
        assert!(state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .find(|track| track.id == track_id)
            .expect("target")
            .clips
            .is_empty());
    }

    #[test]
    fn changed_source_after_probe_rejects_drop_without_importing_it() {
        let root = tempfile::tempdir().expect("fixture directory");
        let library =
            mondrian_assets::AssetLibrary::open(root.path().join("library")).expect("library");
        let mut sequence = Sequence::new("drop");
        let track_id = sequence.add_video_track();
        let time_base = sequence.time_base();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(std::sync::Arc::clone(&library)));
        let candidate = prepared_png(&root.path().join("still.png"));
        std::fs::write(&candidate.canonical_path, b"changed after probe").expect("change source");

        assert!(state
            .commit_staged_file_drop(
                TimelineDropFilePayload {
                    path: candidate.canonical_path.clone(),
                    target_track_id: track_id,
                    position: FramePosition::new(12, time_base),
                },
                candidate
            )
            .is_err());
        assert!(library.list_assets().expect("assets").is_empty());
        assert!(state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .find(|track| track.id == track_id)
            .expect("target")
            .clips
            .is_empty());
    }

    #[test]
    fn zero_duration_png_remains_placeable_without_inventing_video_duration() {
        let root = tempfile::tempdir().expect("fixture directory");
        let path = root.path().join("still.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([127, 31, 63, 0]))
            .save(&path)
            .expect("write actual PNG");
        let path = path.canonicalize().expect("canonical fixture");
        let mut probe = mondrian_media::probe_media_info(&path).expect("probe actual PNG");
        assert_eq!(
            probe.primary_video().and_then(|video| video.total_frames),
            Some(1)
        );
        // Demuxers may report a nominal one-frame duration for a still. This
        // placement regression specifically exercises an admitted zero-duration
        // snapshot, independent of that FFmpeg-version-dependent estimate.
        probe.duration = std::time::Duration::ZERO;
        let fingerprint = mondrian_media::MediaFileFingerprint::capture(&path);
        let candidate = mondrian_assets::AssetMediaProbeCandidate::new(path, fingerprint, probe)
            .expect("admitted picture probe");
        let library = mondrian_assets::AssetLibrary::open(root.path().join("library"))
            .expect("fixture library");
        let id = library.commit_media_probe(candidate, None).expect("commit actual probe");
        let mut asset = library.get_asset(id).expect("read asset").expect("committed asset");
        assert_eq!(asset.kind, AssetKind::StillImage);
        assert!(validate_placement_asset(PlacementTrackKind::Video, &asset).is_ok());
        assert!(validate_placement_asset(PlacementTrackKind::Audio, &asset).is_err());
        asset.kind = AssetKind::Video;
        assert!(validate_placement_asset(PlacementTrackKind::Video, &asset).is_err());
    }
}
