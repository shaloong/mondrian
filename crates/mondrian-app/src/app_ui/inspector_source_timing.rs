//! Inspector projection and typed intent mapping for constant source timing.
//!
//! This module owns no author state. It projects the canonical Clip source map
//! and converts product inputs into typed Actions; the App authoring Module
//! remains the commit and validation authority.

use mondrian_assets::AssetKind;
use mondrian_core::timeline_data::ClipContent;
use mondrian_core::{FramePosition, TimeScale, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_timeline::{Clip, Sequence};

use crate::app::{AppState, SelectedClipRef};

/// Product-facing projection of one Clip's canonical source-time mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InspectorSourceTimingModel {
    /// Current source-sampling mode. Negative maps remain inspectable but are
    /// not editable until reverse playback has direction-aware execution.
    pub mode: InspectorSourceTimingMode,
    /// Whether this source kind supports a positive constant playback rate.
    pub can_set_forward_rate: bool,
    /// Whether the selected Clip can own a picture hold.
    pub supports_picture_hold: bool,
    /// Exact active-Sequence frame sampled by a hold command, when the
    /// playhead currently lies inside the selected video Clip.
    pub freeze_at_playhead: Option<FramePosition>,
}

/// Closed set of source-time modes presented by the Inspector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InspectorSourceTimingMode {
    /// Exact positive source-time scale, displayed in percentage units.
    Forward { rate_percent: f32 },
    /// One constant source sample across the complete Clip placement.
    Hold,
    /// Persisted reverse mapping. Editing remains unavailable until the
    /// execution stack defines strict predecessor semantics end to end.
    ReverseUnsupported { rate_percent: f32 },
}

pub(crate) fn inspector_source_timing_model(
    state: &AppState,
    sequence: &Sequence,
    selection: SelectedClipRef,
    clip: &Clip,
    timeline_time: TimelineTime,
) -> Option<InspectorSourceTimingModel> {
    let source_available = match &clip.content {
        ClipContent::Media { asset_id, .. } => {
            let asset = state
                .asset_library()
                .and_then(|library| library.get_asset(*asset_id).ok().flatten());
            if asset.as_ref().is_some_and(|asset| asset.kind == AssetKind::StillImage) {
                return None;
            }
            asset.is_some()
        }
        ClipContent::NestedSequence { sequence_id, .. } => {
            state.sequence_by_id(*sequence_id).is_some()
        }
        ClipContent::AdjustmentLayer { .. }
        | ClipContent::SolidColor { .. }
        | ClipContent::BasicTitle { .. } => return None,
    };

    let scale = clip.source_time_scale();
    let signed_rate_percent = scale.numerator() as f64 * 100.0 / scale.denominator() as f64;
    let mode = if scale.numerator() > 0 {
        InspectorSourceTimingMode::Forward { rate_percent: signed_rate_percent as f32 }
    } else if scale.numerator() == 0 {
        InspectorSourceTimingMode::Hold
    } else {
        InspectorSourceTimingMode::ReverseUnsupported {
            rate_percent: signed_rate_percent.abs() as f32,
        }
    };
    let map_is_product_editable = scale.numerator() >= 0;
    let supports_picture_hold =
        selection.is_video_track && source_available && map_is_product_editable;
    let freeze_at_playhead =
        (supports_picture_hold && clip.contains(timeline_time).unwrap_or(false)).then_some(
            FramePosition::new(state.current_frame().max(0), sequence.time_base()),
        );

    Some(InspectorSourceTimingModel {
        mode,
        can_set_forward_rate: source_available && map_is_product_editable,
        supports_picture_hold,
        freeze_at_playhead,
    })
}

const RATE_PERCENT_QUANTIZATION: f64 = 100.0;
const RATE_SCALE_DENOMINATOR: i64 = 10_000;
const RATE_PERCENT_MAX: f64 = 10_000.0;

pub(crate) fn inspector_forward_rate_action(
    selection: Option<SelectedClipRef>,
    rate_percent: f32,
) -> Action {
    let rate_percent = f64::from(rate_percent);
    if !rate_percent.is_finite() || rate_percent <= 0.0 || rate_percent > RATE_PERCENT_MAX {
        return Action::NoOp;
    }
    let basis_points = (rate_percent * RATE_PERCENT_QUANTIZATION).round();
    if basis_points < 1.0 || basis_points > i64::MAX as f64 {
        return Action::NoOp;
    }
    let Ok(rate) = TimeScale::new(basis_points as i64, RATE_SCALE_DENOMINATOR) else {
        return Action::NoOp;
    };
    selection
        .map(|selection| Action::SetClipForwardRate {
            clip_id: selection.clip_id,
            rate,
            include_linked: true,
        })
        .unwrap_or(Action::NoOp)
}

pub(crate) fn inspector_freeze_action(
    selection: Option<SelectedClipRef>,
    sequence_time: Option<FramePosition>,
) -> Action {
    let (Some(selection), Some(sequence_time)) = (selection, sequence_time) else {
        return Action::NoOp;
    };
    if !selection.is_video_track {
        return Action::NoOp;
    }
    Action::FreezeVideoClipAt { clip_id: selection.clip_id, sequence_time }
}
