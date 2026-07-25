//! Product-authoring setup and durable identity evidence for the color slice.

use super::{COLOR_MEDIA_SLICE_ID, HLG_ROLE, SRGB_ALPHA_ROLE};
use crate::app::golden_project_acceptance::fixture::FixtureEvidence;
use crate::app::golden_project_acceptance::harness::{
    dispatch_author_transition, wait_for_media_imports, AuthorTransitionEvidence,
    DurableReopenEvidence,
};
use crate::app::golden_project_acceptance::workflow::{
    GoldenProductWorkflowDriver, GoldenSequenceStageEvidence,
};
use crate::app::golden_project_acceptance::GoldenProjectContract;
use crate::app::ui_actions::{
    timeline_add_track_action, timeline_drop_asset_action, timeline_trim_clips_action,
    TimelineAddTrackKind, TimelineAddTrackPayload, TimelineDropAssetPayload,
    TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
};
use crate::app::AppState;
use anyhow::{bail, ensure, Context};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::{AssetId, ClipId, ColorSpace, FramePosition, TimelineTime, TrackId};
use mondrian_editor_state::Action;
use mondrian_media::info::{
    PixelFormat, VideoCodec, VideoCodecProfile, VideoColorDetectionMethod,
    VideoColorInterpretationConfidence,
};
use mondrian_media::DecodedVideoRange;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize)]
struct ImportedVideoEvidence {
    role: &'static str,
    asset_id: AssetId,
    codec: VideoCodec,
    codec_profile: VideoCodecProfile,
    pixel_format: PixelFormat,
    bit_depth: u8,
    width: u32,
    height: u32,
    frame_rate: String,
    has_alpha: bool,
    detected_color_space: ColorSpace,
    color_confidence: VideoColorInterpretationConfidence,
    color_detection_method: VideoColorDetectionMethod,
    range: DecodedVideoRange,
}

#[derive(Debug, Serialize)]
pub(super) struct SetupEvidence {
    stage: GoldenSequenceStageEvidence,
    hlg_track_id: TrackId,
    alpha_track_id: TrackId,
    hlg_clip_id: ClipId,
    alpha_clip_id: ClipId,
    add_alpha_track: AuthorTransitionEvidence,
    place_hlg: AuthorTransitionEvidence,
    place_alpha: AuthorTransitionEvidence,
    trim_to_window: AuthorTransitionEvidence,
    durable_reopen: DurableReopenEvidence,
    imports: Vec<ImportedVideoEvidence>,
}

pub(super) struct AuthoringStageResult {
    pub(super) evidence: SetupEvidence,
    pub(super) hlg_asset_id: AssetId,
    pub(super) alpha_asset_id: AssetId,
}

fn import_fixture(state: &mut AppState, fixture: &FixtureEvidence) -> anyhow::Result<AssetRecord> {
    if let Some(asset) = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.path == fixture.path)
    {
        return Ok(asset);
    }
    state.dispatch_action(Action::ImportMedia(vec![fixture.path.clone()]))?;
    wait_for_media_imports(state)?;
    state
        .asset_library()
        .context("Asset Library is absent after import")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.path == fixture.path)
        .with_context(|| format!("imported fixture is absent: {}", fixture.path.display()))
}

fn validate_import(
    role: &'static str,
    asset: &AssetRecord,
) -> anyhow::Result<ImportedVideoEvidence> {
    let expected_kind = if role == SRGB_ALPHA_ROLE {
        AssetKind::StillImage
    } else {
        AssetKind::Video
    };
    ensure!(
        asset.kind == expected_kind,
        "{role} did not import with the expected picture-media kind"
    );
    let video = asset
        .media_info
        .primary_video()
        .context("imported picture has no video stream")?;
    let expected_color = match role {
        HLG_ROLE => ColorSpace::Rec2100Hlg,
        SRGB_ALPHA_ROLE => ColorSpace::Srgb,
        _ => bail!("unsupported color-media role: {role}"),
    };
    ensure!(
        video.width == 1920
            && video.height == 1080
            && video.detected_color_space == Some(expected_color)
            && video.color_interpretation.confidence == VideoColorInterpretationConfidence::High
            && video.color_detection_method == VideoColorDetectionMethod::CicpTags,
        "{role} import metadata differs from the fixture contract"
    );
    match role {
        HLG_ROLE => ensure!(
            video.codec == VideoCodec::H265
                && video.codec_profile == VideoCodecProfile::HevcMain10
                && video.pixel_format == PixelFormat::Yuv420p10le
                && video.pixel_format_proven
                && video.bit_depth == 10
                && video.frame_rate == mondrian_core::Rational::FPS_25
                && video.frame_rate_proven
                && video.color_range == DecodedVideoRange::Limited
                && !video.has_alpha,
            "HLG fixture did not retain HEVC Main10 limited-range identity"
        ),
        SRGB_ALPHA_ROLE => ensure!(
            video.pixel_format == PixelFormat::Rgba
                && video.pixel_format_proven
                && video.bit_depth == 8
                && video.color_range == DecodedVideoRange::Full
                && video.has_alpha,
            "sRGB still did not retain full-range straight-Alpha identity"
        ),
        _ => unreachable!(),
    }
    Ok(ImportedVideoEvidence {
        role,
        asset_id: asset.id,
        codec: video.codec.clone(),
        codec_profile: video.codec_profile,
        pixel_format: video.pixel_format,
        bit_depth: video.bit_depth,
        width: video.width,
        height: video.height,
        frame_rate: video.frame_rate.to_string(),
        has_alpha: video.has_alpha,
        detected_color_space: expected_color,
        color_confidence: video.color_interpretation.confidence,
        color_detection_method: video.color_detection_method,
        range: video.color_range,
    })
}

fn find_new_clip(
    state: &AppState,
    track_id: TrackId,
    before: &BTreeSet<ClipId>,
) -> anyhow::Result<ClipId> {
    let clips = state
        .active_sequence()
        .context("active Sequence is absent")?
        .video_tracks
        .iter()
        .find(|track| track.id == track_id)
        .context("target video Track is absent")?
        .clips
        .iter()
        .filter(|clip| !before.contains(&clip.id))
        .map(|clip| clip.id)
        .collect::<Vec<_>>();
    ensure!(
        clips.len() == 1,
        "timeline drop created {} Clips instead of one",
        clips.len()
    );
    Ok(clips[0])
}

pub(super) fn setup_stage(
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
    hlg_fixture: &FixtureEvidence,
    alpha_fixture: &FixtureEvidence,
    end_frame_exclusive: i64,
) -> anyhow::Result<AuthoringStageResult> {
    let stage = workflow.bind_slice_primary_sequence(contract, COLOR_MEDIA_SLICE_ID)?;
    let state = workflow.app_mut();
    let hlg_asset = import_fixture(state, hlg_fixture)?;
    let alpha_asset = import_fixture(state, alpha_fixture)?;
    let imports = vec![
        validate_import(HLG_ROLE, &hlg_asset)?,
        validate_import(SRGB_ALPHA_ROLE, &alpha_asset)?,
    ];

    let hlg_track_id = state
        .active_sequence()
        .and_then(|sequence| sequence.video_tracks.first())
        .context("color-media Sequence has no base video Track")?
        .id;
    let add_alpha_track = dispatch_author_transition(
        state,
        "add-color-media-alpha-track",
        timeline_add_track_action(TimelineAddTrackPayload { kind: TimelineAddTrackKind::Video }),
    )?;
    let alpha_track_id = state
        .active_sequence()
        .and_then(|sequence| sequence.video_tracks.last())
        .context("color-media Sequence has no Alpha video Track")?
        .id;
    ensure!(
        alpha_track_id != hlg_track_id,
        "video Track creation reused the base Track"
    );

    let hlg_before = state
        .active_sequence()
        .context("color-media Sequence is absent")?
        .video_tracks
        .iter()
        .find(|track| track.id == hlg_track_id)
        .context("HLG Track is absent")?
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    let place_hlg = dispatch_author_transition(
        state,
        "place-hlg-main10-picture",
        timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: hlg_asset.id,
            target_track_id: hlg_track_id,
            is_video_track: true,
            frame: 0,
        }),
    )?;
    let hlg_clip_id = find_new_clip(state, hlg_track_id, &hlg_before)?;

    let alpha_before = state
        .active_sequence()
        .context("color-media Sequence is absent")?
        .video_tracks
        .iter()
        .find(|track| track.id == alpha_track_id)
        .context("Alpha Track is absent")?
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    let place_alpha = dispatch_author_transition(
        state,
        "place-srgb-alpha-still",
        timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: alpha_asset.id,
            target_track_id: alpha_track_id,
            is_video_track: true,
            frame: 0,
        }),
    )?;
    let alpha_clip_id = find_new_clip(state, alpha_track_id, &alpha_before)?;
    let trim_to_window = dispatch_author_transition(
        state,
        "trim-color-media-window",
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![hlg_clip_id, alpha_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            frame: end_frame_exclusive,
        }),
    )?;
    let sequence = state.active_sequence().context("color-media Sequence is absent")?;
    let expected_end = TimelineTime::from_frame_position(FramePosition::new(
        end_frame_exclusive,
        sequence.time_base(),
    ))?;
    for clip_id in [hlg_clip_id, alpha_clip_id] {
        let clip = sequence
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .find(|clip| clip.id == clip_id)
            .context("trimmed color-media Clip is absent")?;
        ensure!(
            clip.position == TimelineTime::ZERO && clip.end_position()? == expected_end,
            "color-media Clip {clip_id} occupies {}..{} instead of 0..{}",
            clip.position,
            clip.end_position()?,
            expected_end
        );
        if clip_id == alpha_clip_id {
            ensure!(
                clip.speed.scale().numerator() == 0 && clip.source_out == clip.source_in,
                "sRGB still placement is not a zero-rate source hold"
            );
        }
    }

    let durable_reopen = workflow.durable_save_reopen_for(&stage)?;
    workflow.verify_binding()?;
    let sequence = workflow.app().active_sequence().context("reopened Sequence is absent")?;
    ensure!(
        sequence.video_tracks.iter().any(|track| {
            track.id == hlg_track_id && track.clips.iter().any(|clip| clip.id == hlg_clip_id)
        }) && sequence.video_tracks.iter().any(|track| {
            track.id == alpha_track_id && track.clips.iter().any(|clip| clip.id == alpha_clip_id)
        }),
        "durable reopen changed color-media Track/Clip identity"
    );

    Ok(AuthoringStageResult {
        hlg_asset_id: hlg_asset.id,
        alpha_asset_id: alpha_asset.id,
        evidence: SetupEvidence {
            stage,
            hlg_track_id,
            alpha_track_id,
            hlg_clip_id,
            alpha_clip_id,
            add_alpha_track,
            place_hlg,
            place_alpha,
            trim_to_window,
            durable_reopen,
            imports,
        },
    })
}
