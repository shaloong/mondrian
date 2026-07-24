//! Generated visual authoring Golden slice with real Headless execution.

use super::harness::{
    author_transition, dispatch_author_transition, ensure_exact_requirement_evidence,
    new_run_directory, rooted_env_path, write_report, AuthorTransitionEvidence,
    DurableReopenEvidence,
};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
use super::{
    load_golden_contract, repository_root, sequence_settings_from_contract, GoldenExecutionSlice,
    GoldenProjectContract,
};
use crate::app::preview_cpu_execution::composite_resolved_preview;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineMediaFrame, PreviewTimelineResolution,
    PreviewTimelineTitleFrame,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::preview_viewer_plan::{ResolvedPreviewElement, ResolvedPreviewTransitionInput};
use crate::app::selection::SelectedClipRef;
use crate::app::ui_actions::{
    assets_create_solid_color_action, effects_add_to_clip_action, inspector_edit_clip_curve_action,
    inspector_set_clip_property_action, inspector_set_clip_tint_action,
    inspector_set_effect_property_action, timeline_create_basic_title_action,
    timeline_create_cross_dissolve_action, timeline_drop_asset_action, timeline_seek_action,
    timeline_trim_clips_action, AssetsCreateAssetPayload, EffectsAddToClipPayload,
    InspectorClipRefPayload, InspectorCurveEditPayload, InspectorCurvePointPayload,
    InspectorEditClipCurvePayload, InspectorSetClipPropertyPayload, InspectorSetClipTintPayload,
    InspectorSetEffectPropertyPayload, TimelineCreateCrossDissolvePayload,
    TimelineDropAssetPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
};
use crate::app::AppState;
use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::automation::{
    InterpolationType, Keyframe, KeyframeInterpolation, ParameterResourceReference,
    PropertyMutation, PropertyValue,
};
use mondrian_core::{
    BasicTitle, ClipId, Color, EffectId, EvaluatedBasicTitle, FramePosition, FrameRounding,
    PropertyHost, Resolution, TimelineTime, TrackId,
};
use mondrian_editor_state::Action;
use mondrian_effects::{EffectNode, EffectType};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    evaluate_timeline_render_plan, BasicTitleRasterizer, TimelineCompositeScratch,
    TimelineEvaluationRequest, TimelineRenderPlanElement,
};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const VISUAL_SLICE_ID: &str = "visual-authoring-roundtrip-v1";
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_VISUAL_RUN_ROOT";
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_VISUAL_OUTPUT";
const TITLE_TEXT: &str = "Mondrian Golden";
const PREVIEW_RESOLUTION: Resolution = Resolution { width: 640, height: 360 };
const LUT_PROCESSING_SPACE: &str = "rec709";
const GENERATED_LUT: &str = "TITLE \"Mondrian Golden Rec709 Look\"\n\
LUT_3D_SIZE 2\n\
DOMAIN_MIN 0 0 0\n\
DOMAIN_MAX 1 1 1\n\
0.02 0.00 0.01\n\
0.92 0.04 0.02\n\
0.03 0.90 0.02\n\
0.95 0.94 0.03\n\
0.02 0.03 0.88\n\
0.91 0.05 0.92\n\
0.04 0.89 0.90\n\
0.96 0.95 0.94\n";

#[derive(Debug)]
struct GoldenRunPaths {
    directory: PathBuf,
    project: PathBuf,
    report: PathBuf,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenVisualReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    status: &'static str,
    complete_golden_project: bool,
    setup: VisualSetupEvidence,
    operations: Vec<OperationEvidence>,
    content: Vec<ContentEvidence>,
    execution_before_save: VisualExecutionEvidence,
    execution_after_reopen: VisualExecutionEvidence,
}

#[derive(Debug, Serialize)]
struct VisualSetupEvidence {
    project_path: PathBuf,
    stage: GoldenSequenceStageEvidence,
    solid_asset_id: String,
    video_track_id: String,
    left_clip_id: String,
    right_clip_id: String,
    title_clip_id: String,
    transition_id: String,
    start_frame: i64,
    edit_frame: i64,
    end_frame_exclusive: i64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum OperationEvidence {
    UndoRedo {
        undo: AuthorTransitionEvidence,
        keyframes_after_undo: usize,
        redo: AuthorTransitionEvidence,
        keyframes_after_redo: usize,
    },
    SaveReopen {
        durability: DurableReopenEvidence,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::UndoRedo { .. } => "undo-redo",
            Self::SaveReopen { .. } => "save-reopen",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum ContentEvidence {
    PrimaryColor {
        author_steps: Vec<AuthorTransitionEvidence>,
        clip_id: String,
        effect_id: String,
        working_color_space: &'static str,
        exposure: f32,
        contrast: f32,
        saturation: f32,
    },
    Lut {
        author_steps: Vec<AuthorTransitionEvidence>,
        clip_id: String,
        effect_id: String,
        processing_space: &'static str,
        resource_path: PathBuf,
        resource_sha256: String,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
        interpolation: &'static str,
        intensity: f32,
    },
    CrossDissolve {
        author_step: AuthorTransitionEvidence,
        transition_id: String,
        left_clip_id: String,
        right_clip_id: String,
        start_frame: i64,
        end_frame_exclusive: i64,
    },
    BasicTitle {
        create_step: AuthorTransitionEvidence,
        text_step: AuthorTransitionEvidence,
        clip_id: String,
        text: String,
        font_family: String,
    },
    HoldKeyframe {
        author_steps: Vec<AuthorTransitionEvidence>,
        property_path: &'static str,
        keyframes: usize,
        midpoint_value: f32,
    },
    LinearKeyframe {
        author_steps: Vec<AuthorTransitionEvidence>,
        property_path: &'static str,
        keyframes: usize,
        midpoint_value: f32,
    },
    BezierKeyframe {
        author_steps: Vec<AuthorTransitionEvidence>,
        curve_edit_step: AuthorTransitionEvidence,
        edited_keyframe_id: String,
        property_path: &'static str,
        keyframes: usize,
        midpoint_value: f32,
    },
}

impl ContentEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::PrimaryColor { .. } => "primary-color",
            Self::Lut { .. } => "lut",
            Self::CrossDissolve { .. } => "cross-dissolve",
            Self::BasicTitle { .. } => "basic-title",
            Self::HoldKeyframe { .. } => "hold-keyframe",
            Self::LinearKeyframe { .. } => "linear-keyframe",
            Self::BezierKeyframe { .. } => "bezier-keyframe",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EvaluatedTitleEvidence {
    text: String,
    font_family: String,
    font_weight: u16,
    font_size: f32,
    fill: Color,
    tracking_em: f32,
    line_height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct VisualExecutionEvidence {
    frame: i64,
    preview_width: u32,
    preview_height: u32,
    preview_elements: usize,
    export_elements: usize,
    cross_dissolve_progress: f32,
    left_effect_graph_signature: u64,
    title_raster_signatures: Vec<u64>,
    title: EvaluatedTitleEvidence,
    rgba_sha256: String,
    float_linear_composites: u64,
}

fn new_run_paths(root: &Path) -> anyhow::Result<GoldenRunPaths> {
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-visual")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-visual-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-visual.mdp"),
        directory,
        report,
    })
}

fn visual_slice(slices: &[GoldenExecutionSlice]) -> anyhow::Result<&GoldenExecutionSlice> {
    let slice = slices
        .iter()
        .find(|slice| slice.id == VISUAL_SLICE_ID)
        .context("visual Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles.is_empty() && slice.required_exports.is_empty(),
        "visual authoring slice must not claim fixture or encoded-export coverage"
    );
    ensure!(
        slice.required_operations == ["undo-redo", "save-reopen"],
        "visual slice operation contract drifted"
    );
    ensure!(
        slice.required_content
            == [
                "primary-color",
                "lut",
                "cross-dissolve",
                "basic-title",
                "hold-keyframe",
                "linear-keyframe",
                "bezier-keyframe",
            ],
        "visual slice content contract drifted"
    );
    Ok(slice)
}

fn find_video_clip(sequence: &Sequence, clip_id: ClipId) -> anyhow::Result<(&Clip, TrackId)> {
    for track in &sequence.video_tracks {
        if let Some(clip) = track.clips.iter().find(|clip| clip.id == clip_id) {
            return Ok((clip, track.id));
        }
    }
    bail!("video Clip does not exist: {clip_id}")
}

fn find_clip_effect(
    sequence: &Sequence,
    clip_id: ClipId,
    effect_id: EffectId,
) -> anyhow::Result<&EffectNode> {
    find_video_clip(sequence, clip_id)?
        .0
        .effects
        .iter()
        .find(|effect| effect.id == effect_id)
        .with_context(|| format!("Effect {effect_id} is absent from Clip {clip_id}"))
}

fn add_effect(
    state: &mut AppState,
    clip: InspectorClipRefPayload,
    effect_type: EffectType,
    intent: &'static str,
) -> anyhow::Result<(EffectId, AuthorTransitionEvidence)> {
    let before = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        clip.clip_id,
    )?
    .0
    .effects
    .iter()
    .map(|effect| effect.id)
    .collect::<BTreeSet<_>>();
    let step = dispatch_author_transition(
        state,
        intent,
        effects_add_to_clip_action(EffectsAddToClipPayload {
            clip,
            effect_type: effect_type.clone(),
        }),
    )?;
    let created = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        clip.clip_id,
    )?
    .0
    .effects
    .iter()
    .filter(|effect| !before.contains(&effect.id) && effect.effect_type == effect_type)
    .map(|effect| effect.id)
    .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "{intent} created {} candidate Effects",
        created.len()
    );
    Ok((created[0], step))
}

fn set_effect_parameter(
    state: &mut AppState,
    clip: InspectorClipRefPayload,
    effect_id: EffectId,
    parameter: &'static str,
    value: PropertyValue,
    intent: &'static str,
) -> anyhow::Result<AuthorTransitionEvidence> {
    let effect = find_clip_effect(
        state.active_sequence().context("active Sequence is absent")?,
        clip.clip_id,
        effect_id,
    )?;
    let parameter_id = effect
        .effect_type
        .parameter_id(parameter)
        .with_context(|| format!("invalid parameter name: {parameter}"))?;
    let path = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter_id)
        .map(|(path, _)| path.to_owned())
        .with_context(|| format!("Effect {effect_id} has no parameter {parameter_id}"))?;
    dispatch_author_transition(
        state,
        intent,
        inspector_set_effect_property_action(InspectorSetEffectPropertyPayload {
            clip,
            effect_id,
            path,
            value,
        }),
    )
}

fn assert_effect_parameter(
    sequence: &Sequence,
    clip_id: ClipId,
    effect_id: EffectId,
    effect_type: EffectType,
    parameter: &str,
    expected: PropertyValue,
) -> anyhow::Result<()> {
    let effect = find_clip_effect(sequence, clip_id, effect_id)?;
    ensure!(
        effect.effect_type == effect_type,
        "save/reopen changed Effect {effect_id} type"
    );
    let parameter_id = effect_type
        .parameter_id(parameter)
        .with_context(|| format!("invalid parameter name: {parameter}"))?;
    ensure!(
        effect.evaluate_parameter(&parameter_id, TimelineTime::ZERO) == Some(expected),
        "save/reopen changed Effect {effect_id} parameter {parameter_id}"
    );
    Ok(())
}

fn new_solid_asset(state: &mut AppState) -> anyhow::Result<mondrian_core::AssetId> {
    let before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(assets_create_solid_color_action(AssetsCreateAssetPayload {
        folder_id: None,
    }))?;
    let created = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !before.contains(&asset.id))
        .filter(|asset| asset.kind == AssetKind::SolidColor)
        .map(|asset| asset.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "solid-color product action created {} candidate assets",
        created.len()
    );
    Ok(created[0])
}

fn new_clip_after(
    state: &AppState,
    track_id: TrackId,
    before: &BTreeSet<ClipId>,
) -> anyhow::Result<ClipId> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let track = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == track_id)
        .context("target video Track is absent")?;
    let created = track
        .clips
        .iter()
        .filter(|clip| !before.contains(&clip.id))
        .map(|clip| clip.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "timeline action created {} candidate Clips",
        created.len()
    );
    Ok(created[0])
}

fn keyframe_observation(
    sequence: &Sequence,
    title_clip_id: ClipId,
    path: &str,
    midpoint: TimelineTime,
    expected: KeyframeInterpolationKind,
) -> anyhow::Result<(usize, f32)> {
    let (clip, _) = find_video_clip(sequence, title_clip_id)?;
    let bag = clip.property_bag()?;
    let property = bag.property(path).with_context(|| format!("property is absent: {path}"))?;
    let times = property.keyframe_times();
    ensure!(
        times.len() == 2,
        "{path} does not have exactly two keyframes"
    );
    for time in &times {
        let keyframe = property
            .keyframe_at(*time)
            .with_context(|| format!("{path} lost keyframe at {time}"))?;
        ensure!(
            KeyframeInterpolationKind::from(keyframe.interp_out) == expected,
            "{path} interpolation differs from authored intent"
        );
    }
    let value = property
        .evaluate(midpoint)
        .as_f32()
        .with_context(|| format!("{path} is not scalar"))?;
    Ok((times.len(), value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyframeInterpolationKind {
    Hold,
    Linear,
    Bezier,
}

impl From<KeyframeInterpolation> for KeyframeInterpolationKind {
    fn from(value: KeyframeInterpolation) -> Self {
        match value {
            KeyframeInterpolation::Hold => Self::Hold,
            KeyframeInterpolation::Linear => Self::Linear,
            KeyframeInterpolation::Bezier(_) => Self::Bezier,
        }
    }
}

fn add_keyframe(
    state: &mut AppState,
    selection: SelectedClipRef,
    intent: &'static str,
    path: &'static str,
    time: TimelineTime,
    value: f32,
    interpolation: InterpolationType,
) -> anyhow::Result<AuthorTransitionEvidence> {
    let mutation = PropertyMutation::SetKeyframe {
        path: path.to_owned(),
        keyframe: Keyframe::from_preset(time, PropertyValue::Float(value), interpolation),
    };
    author_transition(state, intent, move |state| {
        ensure!(
            state.mutate_clip_property(selection, mutation, intent)?,
            "{intent} reported no author mutation"
        );
        Ok(())
    })
    .map(|(_, evidence)| evidence)
}

fn frame_of(time: TimelineTime, sequence: &Sequence) -> anyhow::Result<i64> {
    Ok(time
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
        .frame)
}

fn title_evidence(title: &EvaluatedBasicTitle) -> EvaluatedTitleEvidence {
    EvaluatedTitleEvidence {
        text: title.text.clone(),
        font_family: title.font_family.clone(),
        font_weight: title.font_weight,
        font_size: title.font_size,
        fill: title.fill,
        tracking_em: title.tracking_em,
        line_height: title.line_height,
    }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn export_transition_input_effect_signature(
    input: &mondrian_renderer::TimelineTransitionInputPlan,
) -> anyhow::Result<u64> {
    match input {
        mondrian_renderer::TimelineTransitionInputPlan::SolidColor(layer) => {
            Ok(layer.effect_graph.signature_hash)
        }
        other => bail!("visual Golden expected a Solid Color transition input, got {other:?}"),
    }
}

fn preview_transition_input_effect_signature(
    input: &ResolvedPreviewTransitionInput,
) -> anyhow::Result<u64> {
    match input {
        ResolvedPreviewTransitionInput::SolidColor(layer) => Ok(layer.effect_graph.signature_hash),
        _ => bail!("visual Golden expected a resolved Solid Color transition input"),
    }
}

fn execute_visual_frame(state: &AppState, frame: i64) -> anyhow::Result<VisualExecutionEvidence> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let color_context =
        sequence.settings.root_program_color_context(state.project_color_environment());
    let export_plan =
        evaluate_timeline_render_plan(sequence, TimelineEvaluationRequest::export(frame))?;
    let export_transition = export_plan
        .elements
        .iter()
        .find_map(|element| match element {
            TimelineRenderPlanElement::CrossDissolve(transition) => Some(transition),
            _ => None,
        })
        .context("Export render plan contains no Cross Dissolve")?;
    let export_title = export_plan
        .elements
        .iter()
        .find_map(|element| match element {
            TimelineRenderPlanElement::BasicTitle(title) => Some(&title.title),
            _ => None,
        })
        .context("Export render plan contains no Basic Title")?;
    let export_left_effect_signature =
        export_transition_input_effect_signature(&export_transition.left)?;

    let mut rasterizer = BasicTitleRasterizer::new();
    let mut raster_signatures = Vec::new();
    let mut preview_title = None;
    let mut media_frame = |_request| PreviewTimelineMediaFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::MediaResolution,
            "generated visual Golden frame unexpectedly requested file-backed media",
        ),
    };
    let mut title_frame =
        |request: crate::app::preview_timeline_execution::PreviewTimelineTitleRequest| {
            preview_title = Some(request.title.clone());
            match rasterizer.rasterize(
                &request.title,
                request.author_resolution,
                request.title_safe_margin,
                request.target_resolution,
                request.working_color_space,
            ) {
                Ok(frame) => {
                    raster_signatures.push(frame.signature);
                    PreviewTimelineTitleFrame::Ready(frame)
                }
                Err(error) => PreviewTimelineTitleFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::GeneratedSource,
                        error.to_string(),
                    ),
                },
            }
        };
    let resolved = match resolve_preview_timeline(
        sequence,
        std::slice::from_ref(sequence),
        frame,
        PREVIEW_RESOLUTION,
        PreviewResolutionScale::Full,
        color_context.clone(),
        &mut media_frame,
        &mut title_frame,
    ) {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Empty => bail!("visual Golden Preview resolved as empty"),
        PreviewTimelineResolution::Pending { .. } => {
            bail!("visual Golden Preview retained a pending dependency")
        }
        PreviewTimelineResolution::Unavailable { reason } => {
            bail!("visual Golden Preview unavailable: {reason:?}")
        }
    };
    ensure!(
        resolved
            .plan
            .elements
            .iter()
            .any(|element| matches!(element, ResolvedPreviewElement::CrossDissolve { .. })),
        "Headless Preview lost the Cross Dissolve"
    );
    let preview_title = preview_title.context("Headless Preview did not request Basic Title")?;
    ensure!(
        preview_title == *export_title,
        "Preview and Export evaluated different Basic Title semantics"
    );
    let preview_progress = resolved
        .plan
        .elements
        .iter()
        .find_map(|element| match element {
            ResolvedPreviewElement::CrossDissolve { progress, .. } => Some(*progress),
            _ => None,
        })
        .context("resolved Preview contains no Cross Dissolve")?;
    let preview_left_effect_signature = resolved
        .plan
        .elements
        .iter()
        .find_map(|element| match element {
            ResolvedPreviewElement::CrossDissolve { left, .. } => Some(left),
            _ => None,
        })
        .context("resolved Preview contains no Cross Dissolve input")
        .and_then(preview_transition_input_effect_signature)?;
    ensure!(
        preview_left_effect_signature == export_left_effect_signature,
        "Preview and Export compiled different Clip effect graphs"
    );
    ensure!(
        preview_progress.to_bits() == export_transition.progress.to_bits(),
        "Preview and Export evaluated different Cross Dissolve progress"
    );
    let mut scratch = TimelineCompositeScratch::default();
    let output = composite_resolved_preview(
        PREVIEW_RESOLUTION.width,
        PREVIEW_RESOLUTION.height,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )?;
    ensure!(
        output.rgba.len()
            == PREVIEW_RESOLUTION.width as usize * PREVIEW_RESOLUTION.height as usize * 4,
        "Headless Preview produced an invalid raster extent"
    );
    ensure!(
        output.composite_diagnostics.float_linear_composites == 1,
        "generated visual Preview left the float-linear compositing path"
    );
    Ok(VisualExecutionEvidence {
        frame,
        preview_width: PREVIEW_RESOLUTION.width,
        preview_height: PREVIEW_RESOLUTION.height,
        preview_elements: resolved.plan.elements.len(),
        export_elements: export_plan.elements.len(),
        cross_dissolve_progress: preview_progress,
        left_effect_graph_signature: export_left_effect_signature,
        title_raster_signatures: raster_signatures,
        title: title_evidence(&preview_title),
        rgba_sha256: sha256_bytes(&output.rgba),
        float_linear_composites: output.composite_diagnostics.float_linear_composites,
    })
}

pub(super) fn execute_visual_stage(
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<GoldenVisualReport> {
    let slice = visual_slice(&contract.execution_slices)?;
    let window = slice.timeline_window.context("visual slice has no timeline window")?;
    let edit_frame = window.start_frame + (window.end_frame_exclusive - window.start_frame) / 2;
    let stage = workflow.create_sequence_stage("visual-authoring")?;
    let lut_path = workflow
        .project_path()
        .parent()
        .context("Golden Project path has no parent")?
        .join("mondrian-golden-rec709-look.cube");
    std::fs::write(&lut_path, GENERATED_LUT).context("write generated Golden LUT")?;
    let parsed_lut =
        mondrian_effects::Lut3D::from_cube_file(&lut_path).context("parse generated Golden LUT")?;
    let lut_sha256 = sha256_bytes(GENERATED_LUT.as_bytes());
    let state = workflow.app_mut();

    let solid_asset_id = new_solid_asset(state)?;
    let video_track_id =
        state.active_sequence().context("active Sequence is absent")?.video_tracks[0].id;
    let before = state.active_sequence().context("active Sequence is absent")?.video_tracks[0]
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    dispatch_author_transition(
        state,
        "drop-left-solid",
        timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: solid_asset_id,
            target_track_id: video_track_id,
            is_video_track: true,
            frame: window.start_frame,
        }),
    )?;
    let left_clip_id = new_clip_after(state, video_track_id, &before)?;
    dispatch_author_transition(
        state,
        "trim-left-solid",
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![left_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            frame: edit_frame,
        }),
    )?;

    let before = state.active_sequence().context("active Sequence is absent")?.video_tracks[0]
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    dispatch_author_transition(
        state,
        "drop-right-solid",
        timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: solid_asset_id,
            target_track_id: video_track_id,
            is_video_track: true,
            frame: edit_frame,
        }),
    )?;
    let right_clip_id = new_clip_after(state, video_track_id, &before)?;
    dispatch_author_transition(
        state,
        "trim-right-solid",
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![right_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            frame: window.end_frame_exclusive,
        }),
    )?;

    for (intent, clip_id, color) in [
        (
            "tint-left-solid",
            left_clip_id,
            Color::from_rgba8(220, 36, 48, 255),
        ),
        (
            "tint-right-solid",
            right_clip_id,
            Color::from_rgba8(32, 96, 224, 255),
        ),
    ] {
        dispatch_author_transition(
            state,
            intent,
            inspector_set_clip_tint_action(InspectorSetClipTintPayload {
                clip: InspectorClipRefPayload {
                    track_id: video_track_id,
                    is_video_track: true,
                    clip_id,
                },
                color,
            }),
        )?;
    }

    ensure!(
        state
            .active_sequence()
            .context("active Sequence is absent")?
            .settings
            .color
            .working_color_space
            == mondrian_core::WorkingColorSpace::LinearRec2020,
        "visual Golden Primary Color requires the contract's Linear Rec.2020 working space"
    );
    let left_clip = InspectorClipRefPayload {
        track_id: video_track_id,
        is_video_track: true,
        clip_id: left_clip_id,
    };
    let (primary_effect_id, primary_add_step) = add_effect(
        state,
        left_clip,
        EffectType::BasicCorrection,
        "add-primary-color",
    )?;
    let primary_exposure = 0.35;
    let primary_contrast = 1.2;
    let primary_saturation = 0.72;
    let primary_steps = vec![
        primary_add_step,
        set_effect_parameter(
            state,
            left_clip,
            primary_effect_id,
            "exposure",
            PropertyValue::Float(primary_exposure),
            "set-primary-exposure",
        )?,
        set_effect_parameter(
            state,
            left_clip,
            primary_effect_id,
            "contrast",
            PropertyValue::Float(primary_contrast),
            "set-primary-contrast",
        )?,
        set_effect_parameter(
            state,
            left_clip,
            primary_effect_id,
            "saturation",
            PropertyValue::Float(primary_saturation),
            "set-primary-saturation",
        )?,
    ];
    let primary_content = ContentEvidence::PrimaryColor {
        author_steps: primary_steps,
        clip_id: left_clip_id.to_string(),
        effect_id: primary_effect_id.to_string(),
        working_color_space: "linear_rec2020",
        exposure: primary_exposure,
        contrast: primary_contrast,
        saturation: primary_saturation,
    };

    let (lut_effect_id, lut_add_step) = add_effect(state, left_clip, EffectType::Lut3D, "add-lut")?;
    let lut_intensity = 0.65;
    let lut_steps = vec![
        lut_add_step,
        set_effect_parameter(
            state,
            left_clip,
            lut_effect_id,
            "processing_space",
            PropertyValue::Enum(LUT_PROCESSING_SPACE.to_owned()),
            "set-lut-processing-space",
        )?,
        set_effect_parameter(
            state,
            left_clip,
            lut_effect_id,
            "path",
            PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                path: lut_path.clone(),
            }),
            "bind-lut-resource",
        )?,
        set_effect_parameter(
            state,
            left_clip,
            lut_effect_id,
            "intensity",
            PropertyValue::Float(lut_intensity),
            "set-lut-intensity",
        )?,
    ];
    let lut_content = ContentEvidence::Lut {
        author_steps: lut_steps,
        clip_id: left_clip_id.to_string(),
        effect_id: lut_effect_id.to_string(),
        processing_space: LUT_PROCESSING_SPACE,
        resource_path: lut_path.clone(),
        resource_sha256: lut_sha256.clone(),
        domain_min: parsed_lut.domain_min,
        domain_max: parsed_lut.domain_max,
        interpolation: "tetrahedral",
        intensity: lut_intensity,
    };

    let transitions_before = state
        .active_sequence()
        .context("active Sequence is absent")?
        .video_transitions
        .iter()
        .map(|transition| transition.id)
        .collect::<BTreeSet<_>>();
    let transition_step = dispatch_author_transition(
        state,
        "create-cross-dissolve",
        timeline_create_cross_dissolve_action(TimelineCreateCrossDissolvePayload {
            left_clip_id,
            right_clip_id,
        }),
    )?;
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let created_transitions = sequence
        .video_transitions
        .iter()
        .filter(|transition| !transitions_before.contains(&transition.id))
        .collect::<Vec<_>>();
    ensure!(
        created_transitions.len() == 1,
        "Cross Dissolve action created {} transitions",
        created_transitions.len()
    );
    let transition_id = created_transitions[0].id;
    let transition_start = frame_of(created_transitions[0].sequence_range.start, sequence)?;
    let transition_end = frame_of(created_transitions[0].sequence_range.end()?, sequence)?;
    let transition_content = ContentEvidence::CrossDissolve {
        author_step: transition_step,
        transition_id: transition_id.to_string(),
        left_clip_id: left_clip_id.to_string(),
        right_clip_id: right_clip_id.to_string(),
        start_frame: transition_start,
        end_frame_exclusive: transition_end,
    };

    state.dispatch_action(timeline_seek_action(window.start_frame))?;
    let title_create_step = dispatch_author_transition(
        state,
        "create-basic-title",
        timeline_create_basic_title_action(),
    )?;
    let title_selection = state
        .primary_selected_clip()
        .filter(|selection| selection.is_video_track)
        .context("Basic Title action did not select its created Clip")?;
    let title_clip_id = title_selection.clip_id;
    let title_text_step = dispatch_author_transition(
        state,
        "set-basic-title-text",
        inspector_set_clip_property_action(InspectorSetClipPropertyPayload {
            clip: InspectorClipRefPayload {
                track_id: title_selection.track_id,
                is_video_track: true,
                clip_id: title_clip_id,
            },
            path: BasicTitle::TEXT_PATH.to_owned(),
            value: PropertyValue::Text(TITLE_TEXT.to_owned()),
        }),
    )?;
    let title = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        title_clip_id,
    )?
    .0
    .content
    .basic_title()
    .context("created Clip is not Basic Title")?
    .evaluate(TimelineTime::ZERO)?;
    let title_content = ContentEvidence::BasicTitle {
        create_step: title_create_step,
        text_step: title_text_step,
        clip_id: title_clip_id.to_string(),
        text: title.text,
        font_family: title.font_family,
    };

    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let time_base = sequence.time_base();
    let start_time =
        TimelineTime::from_frame_position(FramePosition::new(window.start_frame, time_base))?;
    let end_time = TimelineTime::from_frame_position(FramePosition::new(
        window.end_frame_exclusive,
        time_base,
    ))?;
    let midpoint = TimelineTime::from_frame_position(FramePosition::new(edit_frame, time_base))?;
    let hold_steps = vec![
        add_keyframe(
            state,
            title_selection,
            "hold-line-height-start",
            BasicTitle::LINE_HEIGHT_PATH,
            start_time,
            1.0,
            InterpolationType::Hold,
        )?,
        add_keyframe(
            state,
            title_selection,
            "hold-line-height-end",
            BasicTitle::LINE_HEIGHT_PATH,
            end_time,
            2.0,
            InterpolationType::Hold,
        )?,
    ];
    let linear_steps = vec![
        add_keyframe(
            state,
            title_selection,
            "linear-tracking-start",
            BasicTitle::TRACKING_PATH,
            start_time,
            0.0,
            InterpolationType::Linear,
        )?,
        add_keyframe(
            state,
            title_selection,
            "linear-tracking-end",
            BasicTitle::TRACKING_PATH,
            end_time,
            1.0,
            InterpolationType::Linear,
        )?,
    ];
    let mut bezier_steps = vec![add_keyframe(
        state,
        title_selection,
        "bezier-font-size-start",
        BasicTitle::FONT_SIZE_PATH,
        start_time,
        80.0,
        InterpolationType::Bezier,
    )?];
    let bezier_end = add_keyframe(
        state,
        title_selection,
        "bezier-font-size-end",
        BasicTitle::FONT_SIZE_PATH,
        end_time,
        160.0,
        InterpolationType::Bezier,
    )?;
    bezier_steps.push(bezier_end);

    let undo = dispatch_author_transition(state, "undo-bezier-font-size-end", Action::Undo)?;
    let after_undo = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        title_clip_id,
    )?
    .0
    .property_bag()?
    .property(BasicTitle::FONT_SIZE_PATH)
    .context("font size property is absent after Undo")?
    .keyframe_times()
    .len();
    ensure!(
        after_undo == 1,
        "Undo did not remove the last Bezier keyframe"
    );
    let redo = dispatch_author_transition(state, "redo-bezier-font-size-end", Action::Redo)?;
    let after_redo = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        title_clip_id,
    )?
    .0
    .property_bag()?
    .property(BasicTitle::FONT_SIZE_PATH)
    .context("font size property is absent after Redo")?
    .keyframe_times()
    .len();
    ensure!(after_redo == 2, "Redo did not restore the Bezier keyframe");

    let (bezier_address, bezier_end_id, bezier_end_value_ratio) = {
        let bag = find_video_clip(
            state.active_sequence().context("active Sequence is absent")?,
            title_clip_id,
        )?
        .0
        .property_bag()?;
        let property = bag
            .property(BasicTitle::FONT_SIZE_PATH)
            .context("font size property is absent before Curve Editor edit")?;
        let keyframe = property
            .keyframe_at(end_time)
            .context("Bezier endpoint is absent before Curve Editor edit")?;
        let numeric = property
            .descriptor
            .schema
            .numeric
            .context("font size property has no numeric editor contract")?;
        let value = f64::from(keyframe.value.as_f32().context("font size endpoint is not scalar")?);
        let ratio = ((value - numeric.soft_range.min)
            / (numeric.soft_range.max - numeric.soft_range.min))
            .clamp(0.0, 1.0) as f32;
        (property.address(), keyframe.id, ratio)
    };
    let curve_edit_step = dispatch_author_transition(
        state,
        "curve-editor-move-bezier-font-size-end",
        inspector_edit_clip_curve_action(InspectorEditClipCurvePayload {
            clip: InspectorClipRefPayload {
                track_id: title_selection.track_id,
                is_video_track: true,
                clip_id: title_clip_id,
            },
            property: bezier_address,
            edit: InspectorCurveEditPayload::Upsert {
                keyframe_id: Some(bezier_end_id),
                point: InspectorCurvePointPayload { x: 0.875, y: bezier_end_value_ratio },
            },
        }),
    )?;
    let (edited_bezier_time, edited_bezier_value) = {
        let bag = find_video_clip(
            state.active_sequence().context("active Sequence is absent")?,
            title_clip_id,
        )?
        .0
        .property_bag()?;
        let keyframe = bag
            .property(BasicTitle::FONT_SIZE_PATH)
            .and_then(|property| property.keyframe_by_id(bezier_end_id))
            .context("Curve Editor edit changed the Bezier key identity")?;
        ensure!(
            KeyframeInterpolationKind::from(keyframe.interp_out)
                == KeyframeInterpolationKind::Bezier,
            "Curve Editor edit changed Bezier interpolation"
        );
        ensure!(
            keyframe.time != end_time,
            "Curve Editor edit did not move the addressed Bezier key"
        );
        (keyframe.time, keyframe.value.clone())
    };

    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let (hold_count, hold_midpoint) = keyframe_observation(
        sequence,
        title_clip_id,
        BasicTitle::LINE_HEIGHT_PATH,
        midpoint,
        KeyframeInterpolationKind::Hold,
    )?;
    let (linear_count, linear_midpoint) = keyframe_observation(
        sequence,
        title_clip_id,
        BasicTitle::TRACKING_PATH,
        midpoint,
        KeyframeInterpolationKind::Linear,
    )?;
    let (bezier_count, bezier_midpoint) = keyframe_observation(
        sequence,
        title_clip_id,
        BasicTitle::FONT_SIZE_PATH,
        midpoint,
        KeyframeInterpolationKind::Bezier,
    )?;
    ensure!(
        (hold_midpoint - 1.0).abs() <= f32::EPSILON,
        "Hold interpolation changed before the next key"
    );
    ensure!(
        (linear_midpoint - 0.5).abs() < 1.0e-5,
        "Linear interpolation midpoint is incorrect"
    );
    ensure!(
        bezier_midpoint > 80.0 && bezier_midpoint < 160.0,
        "Bezier interpolation midpoint is outside its authored endpoints"
    );
    let content = vec![
        primary_content,
        lut_content,
        transition_content,
        title_content,
        ContentEvidence::HoldKeyframe {
            author_steps: hold_steps,
            property_path: BasicTitle::LINE_HEIGHT_PATH,
            keyframes: hold_count,
            midpoint_value: hold_midpoint,
        },
        ContentEvidence::LinearKeyframe {
            author_steps: linear_steps,
            property_path: BasicTitle::TRACKING_PATH,
            keyframes: linear_count,
            midpoint_value: linear_midpoint,
        },
        ContentEvidence::BezierKeyframe {
            author_steps: bezier_steps,
            curve_edit_step,
            edited_keyframe_id: bezier_end_id.to_string(),
            property_path: BasicTitle::FONT_SIZE_PATH,
            keyframes: bezier_count,
            midpoint_value: bezier_midpoint,
        },
    ];

    let execution_before_save = execute_visual_frame(state, edit_frame)?;
    let durability = workflow.durable_save_reopen()?;
    let state = workflow.app();
    let sequence = state.active_sequence().context("reopened Sequence is absent")?;
    let transition = sequence
        .video_transitions
        .iter()
        .find(|transition| transition.id == transition_id)
        .context("save/reopen changed Cross Dissolve identity")?;
    ensure!(
        transition.left == left_clip_id && transition.right == right_clip_id,
        "save/reopen changed Cross Dissolve endpoints"
    );
    let (reopened_left, reopened_left_track) = find_video_clip(sequence, left_clip_id)?;
    ensure!(
        reopened_left_track == video_track_id,
        "save/reopen changed Primary Color/LUT Clip placement"
    );
    let reopened_effect_order = reopened_left
        .effects
        .iter()
        .filter(|effect| effect.id == primary_effect_id || effect.id == lut_effect_id)
        .map(|effect| effect.id)
        .collect::<Vec<_>>();
    ensure!(
        reopened_effect_order == [primary_effect_id, lut_effect_id],
        "save/reopen changed Primary Color/LUT stack order"
    );
    for (parameter, value) in [
        ("exposure", primary_exposure),
        ("contrast", primary_contrast),
        ("saturation", primary_saturation),
    ] {
        assert_effect_parameter(
            sequence,
            left_clip_id,
            primary_effect_id,
            EffectType::BasicCorrection,
            parameter,
            PropertyValue::Float(value),
        )?;
    }
    assert_effect_parameter(
        sequence,
        left_clip_id,
        lut_effect_id,
        EffectType::Lut3D,
        "processing_space",
        PropertyValue::Enum(LUT_PROCESSING_SPACE.to_owned()),
    )?;
    assert_effect_parameter(
        sequence,
        left_clip_id,
        lut_effect_id,
        EffectType::Lut3D,
        "path",
        PropertyValue::Resource(ParameterResourceReference::ExternalFile {
            path: lut_path.clone(),
        }),
    )?;
    assert_effect_parameter(
        sequence,
        left_clip_id,
        lut_effect_id,
        EffectType::Lut3D,
        "intensity",
        PropertyValue::Float(lut_intensity),
    )?;
    let reopened_lut_bytes = std::fs::read(&lut_path).context("read reopened Golden LUT")?;
    ensure!(
        sha256_bytes(&reopened_lut_bytes) == lut_sha256,
        "save/reopen changed the bound LUT dependency"
    );
    let (reopened_title, reopened_track) = find_video_clip(sequence, title_clip_id)?;
    ensure!(
        reopened_track == title_selection.track_id && reopened_title.is_basic_title(),
        "save/reopen changed Basic Title placement or content kind"
    );
    for (path, kind) in [
        (
            BasicTitle::LINE_HEIGHT_PATH,
            KeyframeInterpolationKind::Hold,
        ),
        (BasicTitle::TRACKING_PATH, KeyframeInterpolationKind::Linear),
        (
            BasicTitle::FONT_SIZE_PATH,
            KeyframeInterpolationKind::Bezier,
        ),
    ] {
        keyframe_observation(sequence, title_clip_id, path, midpoint, kind)?;
    }
    let reopened_title_properties = reopened_title.property_bag()?;
    let reopened_bezier = reopened_title_properties
        .property(BasicTitle::FONT_SIZE_PATH)
        .and_then(|property| property.keyframe_by_id(bezier_end_id))
        .context("save/reopen changed the Curve Editor key identity")?;
    ensure!(
        reopened_bezier.time == edited_bezier_time
            && reopened_bezier.value == edited_bezier_value
            && KeyframeInterpolationKind::from(reopened_bezier.interp_out)
                == KeyframeInterpolationKind::Bezier,
        "save/reopen changed the incrementally edited Bezier key"
    );
    let execution_after_reopen = execute_visual_frame(state, edit_frame)?;
    ensure!(
        execution_before_save == execution_after_reopen,
        "save/reopen changed Headless Preview or Export semantics"
    );
    workflow.verify_binding()?;

    let operations = vec![
        OperationEvidence::UndoRedo {
            undo,
            keyframes_after_undo: after_undo,
            redo,
            keyframes_after_redo: after_redo,
        },
        OperationEvidence::SaveReopen { durability },
    ];
    ensure_exact_requirement_evidence(
        &slice.required_operations,
        operations.iter().map(OperationEvidence::id),
        "operation",
    )?;
    ensure_exact_requirement_evidence(
        &slice.required_content,
        content.iter().map(ContentEvidence::id),
        "content",
    )?;

    Ok(GoldenVisualReport {
        schema_version: 7,
        profile: VISUAL_SLICE_ID,
        contract_id: contract.id.clone(),
        status: "passed",
        complete_golden_project: false,
        setup: VisualSetupEvidence {
            project_path: workflow.project_path().to_path_buf(),
            stage,
            solid_asset_id: solid_asset_id.to_string(),
            video_track_id: video_track_id.to_string(),
            left_clip_id: left_clip_id.to_string(),
            right_clip_id: right_clip_id.to_string(),
            title_clip_id: title_clip_id.to_string(),
            transition_id: transition_id.to_string(),
            start_frame: window.start_frame,
            edit_frame,
            end_frame_exclusive: window.end_frame_exclusive,
        },
        operations,
        content,
        execution_before_save,
        execution_after_reopen,
    })
}

fn execute_visual_slice(root: &Path, paths: &GoldenRunPaths) -> anyhow::Result<GoldenVisualReport> {
    let contract = load_golden_contract(root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let mut workflow = GoldenProductWorkflowDriver::create(
        paths.project.clone(),
        "Windows Alpha Golden Visual",
        settings,
        mondrian_core::ProjectColorEnvironment::default(),
        mondrian_core::ProjectSettings::default(),
    )?;
    execute_visual_stage(&contract, &mut workflow)
}

#[test]
#[ignore = "Golden visual gate requires the Windows Basic Title font dependency"]
fn golden_project_visual_authoring_roundtrip_gate() -> anyhow::Result<()> {
    let root = repository_root();
    let paths = new_run_paths(&root)?;
    match execute_visual_slice(&root, &paths) {
        Ok(report) => {
            write_report(&paths.report, &report)?;
            eprintln!(
                "MONDRIAN_GOLDEN_VISUAL_REPORT_JSON={}",
                serde_json::to_string(&report)?
            );
            eprintln!(
                "MONDRIAN_GOLDEN_VISUAL_REPORT_PATH={}",
                paths.report.display()
            );
            eprintln!(
                "MONDRIAN_GOLDEN_VISUAL_RUN_DIRECTORY={}",
                paths.directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schema_version": 7,
                "profile": VISUAL_SLICE_ID,
                "status": "failed",
                "complete_golden_project": false,
                "error": format!("{error:#}")
            });
            write_report(&paths.report, &failure)?;
            Err(error)
        }
    }
}
