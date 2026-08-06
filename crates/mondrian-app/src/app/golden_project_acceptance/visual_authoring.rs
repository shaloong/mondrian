//! Generated visual authoring Golden slice with real Headless execution.

use super::harness::{
    author_transition, dispatch_author_transition, ensure_exact_requirement_evidence,
    AuthorTransitionEvidence, DurableReopenEvidence,
};
#[cfg(test)]
use super::harness::{new_run_directory, rooted_env_path, write_report};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
#[cfg(test)]
use super::{load_golden_contract, repository_root, sequence_settings_from_contract};
use super::{GoldenExecutionSlice, GoldenProjectContract};
use crate::app::preview_cpu_execution::composite_resolved_preview;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineMediaFrame, PreviewTimelineResolution,
    PreviewTimelineTitleFrame,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::preview_viewer_plan::{ResolvedPreviewElement, ResolvedPreviewTransitionInput};
use crate::app::selection::SelectedClipRef;
use crate::app::ui_actions::{
    assets_create_solid_color_action, clip_edit_numeric_curve_action, clip_set_solid_color_action,
    clip_write_parameter_values_action, timeline_create_basic_title_action,
    timeline_drop_asset_action, timeline_seek_action, timeline_trim_clips_action,
    video_transition_create_cross_dissolve_action, visual_effect_add_to_clip_action,
    visual_effect_set_parameter_value_action, AssetsCreateAssetPayload, ClipCurveEditPayload,
    ClipEditNumericCurvePayload, ClipNormalizedCurvePointPayload, ClipParameterValueWrite,
    ClipSetSolidColorPayload, ClipWriteParameterValuesPayload, TimelineDropAssetPayload,
    TimelineTrimClipsPayload, TimelineTrimPayloadEdge, VideoTransitionCreateCrossDissolvePayload,
    VideoTransitionHandlePolicy, VisualEffectAddToClipPayload,
    VisualEffectSetParameterValuePayload,
};
use crate::app::AppState;
use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::automation::{
    AnimationParameterAddress, InterpolationType, Keyframe, KeyframeInterpolation,
    ParameterResourceReference, PropertyMutation, PropertyValue,
};
use mondrian_core::{
    AssetId, BasicTitle, ClipId, Color, EffectId, EvaluatedBasicTitle, FramePosition,
    FrameRounding, PropertyHost, Resolution, TimelineTime, TrackId, VideoTransitionId,
};
use mondrian_editor_state::Action;
use mondrian_effects::{EffectGraphNodeKind, EffectNode, EffectRenderOp, EffectType};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    evaluate_prepared_visual_program, BasicTitleRasterizer, PreparedVisualProgram,
    TimelineCompositeScratch, TimelineEvaluationRequest, TimelineRenderPlanElement,
};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

pub(super) const VISUAL_SLICE_ID: &str = "visual-authoring-roundtrip-v1";
#[cfg(test)]
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_VISUAL_RUN_ROOT";
#[cfg(test)]
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
#[cfg(test)]
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
    solid_asset_id: AssetId,
    video_track_id: TrackId,
    title_track_id: TrackId,
    left_clip_id: ClipId,
    right_clip_id: ClipId,
    title_clip_id: ClipId,
    transition_id: VideoTransitionId,
    start_frame: i64,
    edit_frame: i64,
    end_frame_exclusive: i64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum OperationEvidence {
    UndoRedo {
        effect_undo: Box<AuthorTransitionEvidence>,
        sharpen_amount_after_undo: f32,
        effect_redo: Box<AuthorTransitionEvidence>,
        sharpen_amount_after_redo: f32,
        keyframe_undo: Box<AuthorTransitionEvidence>,
        keyframes_after_undo: usize,
        keyframe_redo: Box<AuthorTransitionEvidence>,
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
        clip_id: ClipId,
        effect_id: EffectId,
        working_color_space: &'static str,
        exposure: f32,
        contrast: f32,
        saturation: f32,
    },
    Lut {
        author_steps: Vec<AuthorTransitionEvidence>,
        clip_id: ClipId,
        effect_id: EffectId,
        processing_space: &'static str,
        resource_path: PathBuf,
        resource_sha256: String,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
        interpolation: &'static str,
        intensity: f32,
    },
    GaussianBlur {
        author_steps: Vec<AuthorTransitionEvidence>,
        clip_id: ClipId,
        effect_id: EffectId,
        radius_pixels: f32,
    },
    Sharpen {
        author_steps: Vec<AuthorTransitionEvidence>,
        clip_id: ClipId,
        effect_id: EffectId,
        amount: f32,
    },
    CrossDissolve {
        author_step: AuthorTransitionEvidence,
        transition_id: VideoTransitionId,
        left_clip_id: ClipId,
        right_clip_id: ClipId,
        start_frame: i64,
        end_frame_exclusive: i64,
    },
    BasicTitle {
        create_step: AuthorTransitionEvidence,
        text_step: AuthorTransitionEvidence,
        clip_id: ClipId,
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
            Self::GaussianBlur { .. } => "gaussian-blur",
            Self::Sharpen { .. } => "sharpen",
            Self::CrossDissolve { .. } => "cross-dissolve",
            Self::BasicTitle { .. } => "basic-title",
            Self::HoldKeyframe { .. } => "hold-keyframe",
            Self::LinearKeyframe { .. } => "linear-keyframe",
            Self::BezierKeyframe { .. } => "bezier-keyframe",
        }
    }
}

impl GoldenVisualReport {
    pub(super) fn primary_sequence_id(&self) -> mondrian_core::SequenceId {
        self.setup.stage.sequence_id()
    }

    pub(super) fn verify_retained_authoring(&self, state: &AppState) -> anyhow::Result<()> {
        let sequence = state
            .sequence_by_id(self.primary_sequence_id())
            .context("Visual Hero Sequence is absent")?;
        let (left, left_track) = find_video_clip(sequence, self.setup.left_clip_id)?;
        let (right, right_track) = find_video_clip(sequence, self.setup.right_clip_id)?;
        let (title, title_track) = find_video_clip(sequence, self.setup.title_clip_id)?;
        ensure!(
            left_track == self.setup.video_track_id
                && right_track == self.setup.video_track_id
                && title_track == self.setup.title_track_id
                && title.is_basic_title(),
            "Visual Hero Clips changed Track membership or content identity"
        );
        let transition = sequence
            .video_transitions
            .iter()
            .find(|transition| transition.id == self.setup.transition_id)
            .context("Visual Hero Cross Dissolve is absent")?;
        ensure!(
            transition.left == left.id && transition.right == right.id,
            "Visual Hero Cross Dissolve changed its strong endpoints"
        );
        let primary_effect_id = self
            .content
            .iter()
            .find_map(|content| match content {
                ContentEvidence::PrimaryColor { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .context("Visual report has no Primary Color evidence")?;
        let lut_effect_id = self
            .content
            .iter()
            .find_map(|content| match content {
                ContentEvidence::Lut { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .context("Visual report has no LUT evidence")?;
        let blur_effect_id = self
            .content
            .iter()
            .find_map(|content| match content {
                ContentEvidence::GaussianBlur { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .context("Visual report has no Gaussian Blur evidence")?;
        let sharpen_effect_id = self
            .content
            .iter()
            .find_map(|content| match content {
                ContentEvidence::Sharpen { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .context("Visual report has no Sharpen evidence")?;
        ensure!(
            left.effects.iter().map(|effect| effect.id).collect::<Vec<_>>().windows(4).any(
                |stack| {
                    stack
                        == [
                            primary_effect_id,
                            lut_effect_id,
                            blur_effect_id,
                            sharpen_effect_id,
                        ]
                }
            ),
            "Visual Hero Clip lost the ordered Primary Color/LUT/Blur/Sharpen stack"
        );
        Ok(())
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
    left_gaussian_blur_nodes: usize,
    left_sharpen_nodes: usize,
    title_raster_identities: Vec<String>,
    title: EvaluatedTitleEvidence,
    rgba_sha256: String,
    float_linear_composites: u64,
}

#[cfg(test)]
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
                "gaussian-blur",
                "sharpen",
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
    clip_id: ClipId,
    effect_type: EffectType,
    intent: &'static str,
) -> anyhow::Result<(EffectId, AuthorTransitionEvidence)> {
    let before = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        clip_id,
    )?
    .0
    .effects
    .iter()
    .map(|effect| effect.id)
    .collect::<BTreeSet<_>>();
    let step = dispatch_author_transition(
        state,
        intent,
        visual_effect_add_to_clip_action(VisualEffectAddToClipPayload {
            clip_id,
            effect_type: effect_type.clone(),
        }),
    )?;
    let created = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        clip_id,
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
    clip_id: ClipId,
    effect_id: EffectId,
    parameter: &'static str,
    value: PropertyValue,
    intent: &'static str,
) -> anyhow::Result<AuthorTransitionEvidence> {
    let effect = find_clip_effect(
        state.active_sequence().context("active Sequence is absent")?,
        clip_id,
        effect_id,
    )?;
    let parameter_id = effect
        .effect_type
        .parameter_id(parameter)
        .with_context(|| format!("invalid parameter name: {parameter}"))?;
    let property = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter_id)
        .map(|(_, property)| property)
        .with_context(|| format!("Effect {effect_id} has no parameter {parameter_id}"))?;
    let parameter = AnimationParameterAddress {
        animation_track_id: property.track_id,
        parameter_id,
    };
    dispatch_author_transition(
        state,
        intent,
        visual_effect_set_parameter_value_action(VisualEffectSetParameterValuePayload {
            clip_id,
            effect_id,
            parameter,
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

fn evaluated_effect_f32_parameter(
    sequence: &Sequence,
    clip_id: ClipId,
    effect_id: EffectId,
    effect_type: EffectType,
    parameter: &str,
) -> anyhow::Result<f32> {
    let effect = find_clip_effect(sequence, clip_id, effect_id)?;
    ensure!(
        effect.effect_type == effect_type,
        "Effect {effect_id} type changed while evaluating {parameter}"
    );
    let parameter_id = effect_type
        .parameter_id(parameter)
        .with_context(|| format!("invalid parameter name: {parameter}"))?;
    effect
        .evaluate_parameter(&parameter_id, TimelineTime::ZERO)
        .and_then(|value| value.as_f32())
        .with_context(|| format!("Effect {effect_id} parameter {parameter_id} is not scalar"))
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
) -> anyhow::Result<(u64, usize, usize)> {
    match input {
        mondrian_renderer::TimelineTransitionInputPlan::SolidColor(layer) => {
            let (blur_nodes, sharpen_nodes) = effect_kernel_node_counts(&layer.effect_graph);
            Ok((
                layer.effect_graph.signature_hash(),
                blur_nodes,
                sharpen_nodes,
            ))
        }
        other => bail!("visual Golden expected a Solid Color transition input, got {other:?}"),
    }
}

fn preview_transition_input_effect_signature(
    input: &ResolvedPreviewTransitionInput,
) -> anyhow::Result<(u64, usize, usize)> {
    match input {
        ResolvedPreviewTransitionInput::SolidColor(layer) => {
            let (blur_nodes, sharpen_nodes) = effect_kernel_node_counts(&layer.effect_graph);
            Ok((
                layer.effect_graph.signature_hash(),
                blur_nodes,
                sharpen_nodes,
            ))
        }
        _ => bail!("visual Golden expected a resolved Solid Color transition input"),
    }
}

fn effect_kernel_node_counts(graph: &mondrian_effects::CompiledEffectGraph) -> (usize, usize) {
    graph.graph().nodes.iter().fold((0, 0), |(blur, sharpen), node| {
        let op = match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op, .. }
            | EffectGraphNodeKind::DomainEffect { op, .. } => Some(op),
            _ => None,
        };
        match op {
            Some(EffectRenderOp::GaussianBlur { .. }) => (blur + 1, sharpen),
            Some(EffectRenderOp::Sharpen { .. }) => (blur, sharpen + 1),
            _ => (blur, sharpen),
        }
    })
}

fn execute_visual_frame(state: &AppState, frame: i64) -> anyhow::Result<VisualExecutionEvidence> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let color_context =
        sequence.settings.root_program_color_context(state.project_color_environment());
    let program = PreparedVisualProgram::prepare(sequence)?;
    let export_plan = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(FramePosition::new(frame, sequence.time_base())),
    )?;
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
    let export_left_effect_evidence =
        export_transition_input_effect_signature(&export_transition.left)?;
    ensure!(
        export_left_effect_evidence.1 == 1 && export_left_effect_evidence.2 == 1,
        "Export graph must contain exactly one Gaussian Blur and one Sharpen node"
    );

    let mut rasterizer = BasicTitleRasterizer::new();
    let mut raster_identities = Vec::new();
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
                    raster_identities.push(frame.identity().to_string());
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
    let preview_left_effect_evidence = resolved
        .plan
        .elements
        .iter()
        .find_map(|element| match element {
            ResolvedPreviewElement::CrossDissolve { left, .. } => Some(left),
            _ => None,
        })
        .context("resolved Preview contains no Cross Dissolve input")
        .and_then(|input| preview_transition_input_effect_signature(input.as_ref()))?;
    ensure!(
        preview_left_effect_evidence == export_left_effect_evidence,
        "Preview and Export compiled different Clip effect graphs or kernel nodes"
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
        left_effect_graph_signature: export_left_effect_evidence.0,
        left_gaussian_blur_nodes: export_left_effect_evidence.1,
        left_sharpen_nodes: export_left_effect_evidence.2,
        title_raster_identities: raster_identities,
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
    let stage = workflow.bind_slice_primary_sequence(contract, VISUAL_SLICE_ID)?;
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
    let time_base = state.active_sequence().context("active Sequence is absent")?.time_base();
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
            position: FramePosition::new(window.start_frame, time_base),
        }),
    )?;
    let left_clip_id = new_clip_after(state, video_track_id, &before)?;
    dispatch_author_transition(
        state,
        "trim-left-solid",
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![left_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            position: FramePosition::new(edit_frame, time_base),
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
            position: FramePosition::new(edit_frame, time_base),
        }),
    )?;
    let right_clip_id = new_clip_after(state, video_track_id, &before)?;
    dispatch_author_transition(
        state,
        "trim-right-solid",
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![right_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            position: FramePosition::new(window.end_frame_exclusive, time_base),
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
            clip_set_solid_color_action(ClipSetSolidColorPayload { clip_id, color }),
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
    let left_clip = left_clip_id;
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
        clip_id: left_clip_id,
        effect_id: primary_effect_id,
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
        clip_id: left_clip_id,
        effect_id: lut_effect_id,
        processing_space: LUT_PROCESSING_SPACE,
        resource_path: lut_path.clone(),
        resource_sha256: lut_sha256.clone(),
        domain_min: parsed_lut.domain_min,
        domain_max: parsed_lut.domain_max,
        interpolation: "tetrahedral",
        intensity: lut_intensity,
    };

    let blur_radius = 2.75;
    let (blur_effect_id, blur_add_step) = add_effect(
        state,
        left_clip,
        EffectType::GaussianBlur,
        "add-gaussian-blur",
    )?;
    let blur_steps = vec![
        blur_add_step,
        set_effect_parameter(
            state,
            left_clip,
            blur_effect_id,
            "radius",
            PropertyValue::Float(blur_radius),
            "set-gaussian-blur-radius",
        )?,
    ];
    let blur_content = ContentEvidence::GaussianBlur {
        author_steps: blur_steps,
        clip_id: left_clip_id,
        effect_id: blur_effect_id,
        radius_pixels: blur_radius,
    };

    let sharpen_amount = 0.45;
    let (sharpen_effect_id, sharpen_add_step) =
        add_effect(state, left_clip, EffectType::Sharpen, "add-sharpen")?;
    let sharpen_set_step = set_effect_parameter(
        state,
        left_clip,
        sharpen_effect_id,
        "amount",
        PropertyValue::Float(sharpen_amount),
        "set-sharpen-amount",
    )?;
    let sharpen_content = ContentEvidence::Sharpen {
        author_steps: vec![sharpen_add_step, sharpen_set_step],
        clip_id: left_clip_id,
        effect_id: sharpen_effect_id,
        amount: sharpen_amount,
    };
    let effect_undo = dispatch_author_transition(state, "undo-sharpen-amount", Action::Undo)?;
    let sharpen_amount_after_undo = evaluated_effect_f32_parameter(
        state.active_sequence().context("active Sequence is absent")?,
        left_clip_id,
        sharpen_effect_id,
        EffectType::Sharpen,
        "amount",
    )?;
    ensure!(
        sharpen_amount_after_undo.to_bits() == 0.0f32.to_bits(),
        "Undo did not restore the canonical Sharpen default"
    );
    let effect_redo = dispatch_author_transition(state, "redo-sharpen-amount", Action::Redo)?;
    let sharpen_amount_after_redo = evaluated_effect_f32_parameter(
        state.active_sequence().context("active Sequence is absent")?,
        left_clip_id,
        sharpen_effect_id,
        EffectType::Sharpen,
        "amount",
    )?;
    ensure!(
        sharpen_amount_after_redo.to_bits() == sharpen_amount.to_bits(),
        "Redo did not restore the authored Sharpen amount"
    );

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
        video_transition_create_cross_dissolve_action(VideoTransitionCreateCrossDissolvePayload {
            left_clip_id,
            right_clip_id,
            handle_policy: VideoTransitionHandlePolicy::Reject,
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
        transition_id,
        left_clip_id,
        right_clip_id,
        start_frame: transition_start,
        end_frame_exclusive: transition_end,
    };

    state.dispatch_action(timeline_seek_action(FramePosition::new(
        window.start_frame,
        state.active_sequence().context("active Sequence is absent")?.time_base(),
    )))?;
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
    let title_text_parameter = find_video_clip(
        state.active_sequence().context("active Sequence is absent")?,
        title_clip_id,
    )?
    .0
    .intrinsic_parameter_bag()
    .address_for_path(BasicTitle::TEXT_PATH)
    .context("Basic Title text parameter is absent")?;
    let title_text_step = dispatch_author_transition(
        state,
        "set-basic-title-text",
        clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
            clip_id: title_clip_id,
            writes: vec![ClipParameterValueWrite {
                parameter: title_text_parameter,
                value: PropertyValue::Text(TITLE_TEXT.to_owned()),
            }],
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
        clip_id: title_clip_id,
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

    let keyframe_undo =
        dispatch_author_transition(state, "undo-bezier-font-size-end", Action::Undo)?;
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
    let keyframe_redo =
        dispatch_author_transition(state, "redo-bezier-font-size-end", Action::Redo)?;
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
        clip_edit_numeric_curve_action(ClipEditNumericCurvePayload {
            clip_id: title_clip_id,
            parameter: bezier_address,
            edit: ClipCurveEditPayload::Upsert {
                keyframe_id: Some(bezier_end_id),
                point: ClipNormalizedCurvePointPayload {
                    time_ratio: 0.875,
                    value_ratio: f64::from(bezier_end_value_ratio),
                },
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
        blur_content,
        sharpen_content,
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
    let durability = workflow.durable_save_reopen_for(&stage)?;
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
        "save/reopen changed visual Effect Clip placement"
    );
    let reopened_effect_order = reopened_left
        .effects
        .iter()
        .filter(|effect| {
            [
                primary_effect_id,
                lut_effect_id,
                blur_effect_id,
                sharpen_effect_id,
            ]
            .contains(&effect.id)
        })
        .map(|effect| effect.id)
        .collect::<Vec<_>>();
    ensure!(
        reopened_effect_order
            == [
                primary_effect_id,
                lut_effect_id,
                blur_effect_id,
                sharpen_effect_id,
            ],
        "save/reopen changed Primary Color/LUT/Blur/Sharpen stack order"
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
    assert_effect_parameter(
        sequence,
        left_clip_id,
        blur_effect_id,
        EffectType::GaussianBlur,
        "radius",
        PropertyValue::Float(blur_radius),
    )?;
    assert_effect_parameter(
        sequence,
        left_clip_id,
        sharpen_effect_id,
        EffectType::Sharpen,
        "amount",
        PropertyValue::Float(sharpen_amount),
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
            effect_undo: Box::new(effect_undo),
            sharpen_amount_after_undo,
            effect_redo: Box::new(effect_redo),
            sharpen_amount_after_redo,
            keyframe_undo: Box::new(keyframe_undo),
            keyframes_after_undo: after_undo,
            keyframe_redo: Box::new(keyframe_redo),
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
        schema_version: 9,
        profile: VISUAL_SLICE_ID,
        contract_id: contract.id.clone(),
        status: "passed",
        complete_golden_project: false,
        setup: VisualSetupEvidence {
            project_path: workflow.project_path().to_path_buf(),
            stage,
            solid_asset_id,
            video_track_id,
            title_track_id: title_selection.track_id,
            left_clip_id,
            right_clip_id,
            title_clip_id,
            transition_id,
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

#[cfg(test)]
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
                "schema_version": 9,
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
