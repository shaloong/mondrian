//! Product Adapter for direct Asset placement on the active Timeline.
//!
//! The Widget supplies stable Asset/Track identity plus an explicitly gridded
//! coordinate. This Module resolves current Track media kind, lowers time once,
//! prepares the current Asset binding, and owns status plus execution routing.

use super::product_action::{AssetTargetPayload, TimelineDropAssetPayload};
use super::timeline_position::lower_exact_sequence_frame;
use super::AppState;
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::MondrianError;
use mondrian_timeline::TrackType;

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
        let (frame, track_kind) = resolve_placement_target(sequence, payload)?;
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
    payload: TimelineDropAssetPayload,
) -> mondrian_core::Result<(i64, PlacementTrackKind)> {
    let frame = lower_exact_sequence_frame(sequence, payload.position, "timeline_place_asset")?;

    let track = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .find(|track| track.id == payload.target_track_id)
        .ok_or_else(|| MondrianError::TrackNotFound {
            track_id: payload.target_track_id.to_string(),
        })?;
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
        (PlacementTrackKind::Video, AssetKind::Video | AssetKind::StillImage) => asset
            .media_probe()
            .is_some_and(|probe| !probe.duration.is_zero() && probe.primary_video().is_some()),
        (PlacementTrackKind::Video, AssetKind::AdjustmentLayer | AssetKind::SolidColor) => true,
        (PlacementTrackKind::Audio, AssetKind::Audio) => asset
            .media_probe()
            .is_some_and(|probe| !probe.duration.is_zero() && probe.primary_audio().is_some()),
        _ => false,
    };
    if compatible {
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

fn placement_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_place_asset".to_owned(),
        reason: reason.into(),
    }
}
