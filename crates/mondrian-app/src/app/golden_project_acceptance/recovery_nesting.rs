//! Fixture-independent Golden slice for nested Sequence recovery durability.
//!
//! The slice drives only production Actions and persistence Interfaces. It
//! proves that one Project transaction creates a closed nested graph, Preview
//! and Export recurse through it, crash recovery restores the same author
//! state, and a covering manual save retires recovery authority.

use super::harness::{
    author_checkpoint, dispatch_author_transition, ensure_exact_requirement_evidence,
    new_run_directory, rooted_env_path, write_report, AuthorCheckpoint, AuthorTransitionEvidence,
    DurableReopenEvidence, ProjectAuthorTransitionEvidence,
};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
use super::{
    load_golden_contract, repository_root, sequence_settings_from_contract, GoldenExecutionSlice,
    GoldenProjectContract,
};
use crate::app::exporting::capture_timeline_export_snapshot;
use crate::app::preview_cpu_execution::composite_resolved_preview;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineExecutionFact, PreviewTimelineMediaFrame,
    PreviewTimelineResolution, PreviewTimelineTitleFrame,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::ui_actions::{
    assets_create_solid_color_action, project_recover_from_autosave_action,
    timeline_drop_asset_action, timeline_precompose_selection_action, timeline_select_clip_action,
    AssetsCreateAssetPayload, ProjectRecoverFromAutosavePayload, TimelineDropAssetPayload,
    TimelinePrecomposeSelectionPayload, TimelineSelectClipPayload,
};
use crate::app::{discover_crash_recovery_candidates, AppState};
use anyhow::{ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::{ClipId, Resolution, SequenceId, TrackId};
use mondrian_export::preset::TimelineExportRange;
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    evaluate_timeline_render_plan, RenderColorStageDiagnostics, TimelineCompositeDiagnostics,
    TimelineCompositeScratch, TimelineEvaluationRequest, TimelineRenderPlanElement,
};
use mondrian_timeline::sequence::SequenceRole;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const RECOVERY_NESTING_SLICE_ID: &str = "recovery-nesting-v1";
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_RECOVERY_NESTING_RUN_ROOT";
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_RECOVERY_NESTING_OUTPUT";
const EXECUTION_RESOLUTION: Resolution = Resolution { width: 320, height: 180 };

#[derive(Debug)]
struct GoldenRunPaths {
    directory: PathBuf,
    project: PathBuf,
    report: PathBuf,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenRecoveryNestingReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    status: &'static str,
    complete_golden_project: bool,
    setup: RecoveryNestingSetupEvidence,
    operations: Vec<OperationEvidence>,
    content: Vec<ContentEvidence>,
    execution_before_recovery: NestedExecutionEvidence,
    execution_after_recovery: NestedExecutionEvidence,
    execution_after_save_reopen: NestedExecutionEvidence,
}

#[derive(Debug, Serialize)]
struct RecoveryNestingSetupEvidence {
    project_path: PathBuf,
    stage: GoldenSequenceStageEvidence,
    solid_asset_id: String,
    video_track_id: String,
    source_clip_id: String,
    replacement_clip_id: String,
    nested_sequence_id: String,
    evaluation_frame: i64,
    drop_step: AuthorTransitionEvidence,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum OperationEvidence {
    AutosaveRecovery {
        autosave_file: PathBuf,
        autosave_sha256: String,
        saved_at_unix_ms: u64,
        manifest_snapshot_count: usize,
        before_close: AuthorCheckpoint,
        recovered_session: AuthorCheckpoint,
        session_identity_changed: bool,
        project_identity_preserved: bool,
        recovery_authority_retained: bool,
    },
    SaveReopen {
        durability: DurableReopenEvidence,
        recovery_candidate_count: usize,
        recovery_archive_removed: bool,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::AutosaveRecovery { .. } => "autosave-recovery",
            Self::SaveReopen { .. } => "save-reopen",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum ContentEvidence {
    NestedSequence {
        author_step: ProjectAuthorTransitionEvidence,
        before_recovery: NestedAuthorEvidence,
        after_recovery: NestedAuthorEvidence,
        after_save_reopen: NestedAuthorEvidence,
    },
}

impl ContentEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::NestedSequence { .. } => "nested-sequence",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NestedAuthorEvidence {
    parent_sequence_id: String,
    parent_revision: u64,
    parent_sha256: String,
    replacement_clip_id: String,
    nested_sequence_id: String,
    nested_sequence_name: String,
    nested_revision: u64,
    nested_sha256: String,
    nested_video_clip_count: usize,
    nested_audio_clip_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NestedExecutionEvidence {
    frame: i64,
    preview_width: u32,
    preview_height: u32,
    preview_elements: usize,
    preview_raster_sha256: String,
    preview_root_composite: TimelineCompositeDiagnostics,
    nested_preview_composite_count: u64,
    nested_preview_float_linear_composites: u64,
    nested_preview_legacy_rgba8_composites: u64,
    export_root_nested_elements: usize,
    export_child_solid_elements: usize,
    export_composite: TimelineCompositeDiagnostics,
    export_color_stages: RenderColorStageDiagnostics,
}

fn recovery_nesting_slice(
    slices: &[GoldenExecutionSlice],
) -> anyhow::Result<&GoldenExecutionSlice> {
    slices
        .iter()
        .find(|slice| slice.id == RECOVERY_NESTING_SLICE_ID)
        .context("Golden contract has no recovery/nesting slice")
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    Ok(sha256_bytes(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    ))
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
        .filter(|asset| !before.contains(&asset.id) && asset.kind == AssetKind::SolidColor)
        .map(|asset| asset.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "solid-color product action created {} candidate assets",
        created.len()
    );
    Ok(created[0])
}

fn new_video_clip_after(
    state: &AppState,
    track_id: TrackId,
    before: &BTreeSet<ClipId>,
) -> anyhow::Result<ClipId> {
    let track = state
        .active_sequence()
        .context("active Sequence is absent")?
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

fn nested_author_evidence(
    state: &AppState,
    parent_sequence_id: SequenceId,
    replacement_clip_id: ClipId,
    nested_sequence_id: SequenceId,
) -> anyhow::Result<NestedAuthorEvidence> {
    let parent = state.sequence_by_id(parent_sequence_id).context("parent Sequence is absent")?;
    let replacement = parent
        .video_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == replacement_clip_id)
        .context("nested replacement Clip is absent")?;
    ensure!(
        replacement.nested_sequence_id() == Some(nested_sequence_id),
        "replacement Clip no longer targets the expected nested Sequence"
    );
    let nested = state.sequence_by_id(nested_sequence_id).context("nested Sequence is absent")?;
    ensure!(
        nested.role == SequenceRole::NestedComposition,
        "precompose created a non-nested Sequence role"
    );
    let nested_video_clip_count =
        nested.video_tracks.iter().map(|track| track.clips.len()).sum::<usize>();
    let nested_audio_clip_count =
        nested.audio_tracks.iter().map(|track| track.clips.len()).sum::<usize>();
    ensure!(
        nested_video_clip_count == 1,
        "nested Sequence must retain exactly the selected generated Clip"
    );
    ensure!(
        nested
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .all(|clip| clip.position == mondrian_core::TimelineTime::ZERO),
        "precompose did not project selected content onto child-local zero"
    );
    Ok(NestedAuthorEvidence {
        parent_sequence_id: parent.id.to_string(),
        parent_revision: parent.revision.get(),
        parent_sha256: sha256_bytes(&serde_json::to_vec(parent)?),
        replacement_clip_id: replacement.id.to_string(),
        nested_sequence_id: nested.id.to_string(),
        nested_sequence_name: nested.name.clone(),
        nested_revision: nested.revision.get(),
        nested_sha256: sha256_bytes(&serde_json::to_vec(nested)?),
        nested_video_clip_count,
        nested_audio_clip_count,
    })
}

fn execute_nested_frame(state: &AppState, frame: i64) -> anyhow::Result<NestedExecutionEvidence> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let sequences = state.export_sequences_snapshot();
    let color_context =
        sequence.settings.root_program_color_context(state.project_color_environment());
    let mut media_frame = |_request| PreviewTimelineMediaFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::MediaResolution,
            "generated recovery/nesting Golden unexpectedly requested file-backed media",
        ),
    };
    let mut title_frame = |_request| PreviewTimelineTitleFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::GeneratedSource,
            "generated recovery/nesting Golden unexpectedly requested a Basic Title",
        ),
    };
    let resolved = match resolve_preview_timeline(
        sequence,
        &sequences,
        frame,
        EXECUTION_RESOLUTION,
        PreviewResolutionScale::Full,
        color_context,
        &mut media_frame,
        &mut title_frame,
    ) {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Empty => {
            anyhow::bail!("nested Golden Preview resolved as empty")
        }
        PreviewTimelineResolution::Pending { .. } => {
            anyhow::bail!("nested Golden Preview retained a pending dependency")
        }
        PreviewTimelineResolution::Unavailable { reason } => {
            anyhow::bail!("nested Golden Preview unavailable: {reason:?}")
        }
    };
    let mut scratch = TimelineCompositeScratch::default();
    let output = composite_resolved_preview(
        EXECUTION_RESOLUTION.width,
        EXECUTION_RESOLUTION.height,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )?;
    ensure!(
        output.composite_diagnostics.float_linear_composites == 1
            && output.composite_diagnostics.legacy_rgba8_composites == 0,
        "root Preview left the float-linear compositing path"
    );

    let mut nested_preview_composite_count = 0_u64;
    let mut nested_preview_float_linear_composites = 0_u64;
    let mut nested_preview_legacy_rgba8_composites = 0_u64;
    for fact in resolved.facts {
        if let PreviewTimelineExecutionFact::Composite(diagnostics) = fact {
            nested_preview_composite_count += 1;
            nested_preview_float_linear_composites += diagnostics.float_linear_composites;
            nested_preview_legacy_rgba8_composites += diagnostics.legacy_rgba8_composites;
        }
    }
    ensure!(
        nested_preview_composite_count == 1
            && nested_preview_float_linear_composites == 1
            && nested_preview_legacy_rgba8_composites == 0,
        "Preview did not execute exactly one float-linear nested composite"
    );

    let root_plan =
        evaluate_timeline_render_plan(sequence, TimelineEvaluationRequest::export(frame))?;
    let nested_plans = root_plan
        .elements
        .iter()
        .filter_map(|element| match element {
            TimelineRenderPlanElement::NestedSequence(nested) => Some(nested),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        nested_plans.len() == 1,
        "Export root plan did not contain exactly one nested Sequence"
    );
    let nested_sequence = sequences
        .iter()
        .find(|candidate| candidate.id == nested_plans[0].sequence_id)
        .context("Export root plan targets a missing nested Sequence")?;
    let nested_frame = nested_plans[0]
        .source_time
        .to_frame_position(
            nested_sequence.settings.frame_rate,
            mondrian_core::FrameRounding::Floor,
        )?
        .frame;
    let child_plan = evaluate_timeline_render_plan(
        nested_sequence,
        TimelineEvaluationRequest::export(nested_frame),
    )?;
    let export_child_solid_elements = child_plan
        .elements
        .iter()
        .filter(|element| matches!(element, TimelineRenderPlanElement::SolidColor(_)))
        .count();
    ensure!(
        export_child_solid_elements == 1,
        "Export child plan did not retain the generated Solid Color"
    );

    let export_snapshot = capture_timeline_export_snapshot(
        state,
        sequence.clone(),
        sequences,
        TimelineExportRange::EntireSequence,
    )
    .map_err(anyhow::Error::msg)?;
    let export_composite = mondrian_export::queue::export_composite_diagnostics_for_frame(
        &export_snapshot,
        frame,
        EXECUTION_RESOLUTION.width,
        EXECUTION_RESOLUTION.height,
    )
    .map_err(anyhow::Error::msg)?;
    ensure!(
        export_composite.float_linear_composites >= 2
            && export_composite.legacy_rgba8_composites == 0,
        "Export did not recursively execute parent and child on the float-linear path"
    );
    let export_color_stages = mondrian_export::queue::export_color_stage_diagnostics_for_frame(
        &export_snapshot,
        frame,
        EXECUTION_RESOLUTION.width,
        EXECUTION_RESOLUTION.height,
    )
    .map_err(anyhow::Error::msg)?;

    Ok(NestedExecutionEvidence {
        frame,
        preview_width: EXECUTION_RESOLUTION.width,
        preview_height: EXECUTION_RESOLUTION.height,
        preview_elements: resolved.plan.elements.len(),
        preview_raster_sha256: sha256_bytes(&output.rgba),
        preview_root_composite: output.composite_diagnostics,
        nested_preview_composite_count,
        nested_preview_float_linear_composites,
        nested_preview_legacy_rgba8_composites,
        export_root_nested_elements: nested_plans.len(),
        export_child_solid_elements,
        export_composite,
        export_color_stages,
    })
}

fn recovery_candidates_for(project_path: &Path) -> Vec<crate::app::CrashRecoveryCandidate> {
    discover_crash_recovery_candidates()
        .into_iter()
        .filter(|candidate| candidate.project_file == project_path)
        .collect()
}

pub(super) fn execute_recovery_nesting_stage(
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<GoldenRecoveryNestingReport> {
    let slice = recovery_nesting_slice(&contract.execution_slices)?;
    let window = slice.timeline_window.context("recovery/nesting slice has no timeline window")?;
    let evaluation_frame =
        window.start_frame + (window.end_frame_exclusive - window.start_frame) / 2;
    let stage = workflow.create_sequence_stage("recovery-nesting")?;
    let parent_sequence_id = stage.sequence_id;
    let project_path = workflow.project_path().to_path_buf();
    let project_id = workflow.project_id();
    let state = workflow.app_mut();

    let solid_asset_id = new_solid_asset(state)?;
    let video_track_id =
        state.active_sequence().context("active Sequence is absent")?.video_tracks[0].id;
    let clips_before = state.active_sequence().context("active Sequence is absent")?.video_tracks
        [0]
    .clips
    .iter()
    .map(|clip| clip.id)
    .collect::<BTreeSet<_>>();
    let drop_step = dispatch_author_transition(
        state,
        "drop-recovery-nesting-solid",
        timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: solid_asset_id,
            target_track_id: video_track_id,
            is_video_track: true,
            frame: window.start_frame,
        }),
    )?;
    let source_clip_id = new_video_clip_after(state, video_track_id, &clips_before)?;
    state.dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
        track_id: video_track_id,
        is_video_track: true,
        clip_id: source_clip_id,
    }))?;
    let ((replacement_clip_id, nested_sequence_id), precompose_step) =
        super::harness::project_author_transition(
            state,
            "precompose-recovery-nesting-selection",
            |state| {
                state.dispatch_action(timeline_precompose_selection_action(
                    TimelinePrecomposeSelectionPayload {
                        name: "Golden Recovery Nested".to_owned(),
                    },
                ))?;
                let replacement = state
                    .primary_selected_clip()
                    .context("Precompose Action did not select its replacement Clip")?;
                let nested_sequence_id = state
                    .active_sequence()
                    .and_then(|sequence| {
                        sequence
                            .video_tracks
                            .iter()
                            .flat_map(|track| &track.clips)
                            .find(|clip| clip.id == replacement.clip_id)
                    })
                    .and_then(|clip| clip.nested_sequence_id())
                    .context("Precompose Action produced no nested Sequence placement")?;
                Ok((replacement.clip_id, nested_sequence_id))
            },
        )?;
    ensure!(
        precompose_step.before.sequence_revision.checked_add(1)
            == Some(precompose_step.after.sequence_revision),
        "Precompose Project transaction did not advance the parent Sequence revision once"
    );

    let before_recovery_author = nested_author_evidence(
        state,
        parent_sequence_id,
        replacement_clip_id,
        nested_sequence_id,
    )?;
    let execution_before_recovery = execute_nested_frame(state, evaluation_frame)?;
    let before_close = author_checkpoint(state)?;
    let autosave_file = state.write_autosave_snapshot(4, 7)?;
    ensure!(
        state.has_unsaved_project_changes(),
        "Autosave incorrectly made the authoring Session clean"
    );
    let autosave_sha256 = sha256_file(&autosave_file)?;
    let candidates = recovery_candidates_for(&project_path);
    ensure!(
        candidates.len() == 1 && candidates[0].autosave_file == autosave_file,
        "published autosave is not the exact canonical recovery candidate"
    );
    let candidate_saved_at = candidates[0].saved_at_unix_ms;
    let candidate_snapshot_count = candidates[0].total_snapshots;

    state.close_project();
    ensure!(
        state.authoring_session_id().is_none(),
        "crash simulation retained the original Authoring Session"
    );
    state.dispatch_action(project_recover_from_autosave_action(
        ProjectRecoverFromAutosavePayload {
            project_file: project_path.clone(),
            autosave_file: autosave_file.clone(),
        },
    ))?;
    workflow.verify_binding()?;
    let recovered_session = author_checkpoint(workflow.app())?;
    ensure!(
        before_close.session_id != recovered_session.session_id,
        "Recovery reused the closed Authoring Session identity"
    );
    ensure!(
        recovered_session.project_id == project_id
            && recovered_session.project_path == project_path
            && recovered_session.active_sequence_id == before_close.active_sequence_id
            && recovered_session.sequence_revision == before_close.sequence_revision,
        "Recovery changed Project identity or active Sequence author state"
    );
    ensure!(
        workflow.app().has_unsaved_project_changes(),
        "Recovered authoring Session is not dirty"
    );
    let after_recovery_author = nested_author_evidence(
        workflow.app(),
        parent_sequence_id,
        replacement_clip_id,
        nested_sequence_id,
    )?;
    let execution_after_recovery = execute_nested_frame(workflow.app(), evaluation_frame)?;
    ensure!(
        before_recovery_author == after_recovery_author
            && execution_before_recovery == execution_after_recovery,
        "Autosave recovery changed nested author or execution semantics"
    );
    let recovery_authority_retained = {
        let candidates = recovery_candidates_for(&project_path);
        autosave_file.is_file()
            && candidates.len() == 1
            && candidates[0].autosave_file == autosave_file
    };
    ensure!(
        recovery_authority_retained,
        "Opening a recovery point retired recovery authority before manual save"
    );

    let durability = workflow.durable_save_reopen()?;
    let recovery_candidate_count = recovery_candidates_for(&project_path).len();
    let recovery_archive_removed = !autosave_file.exists();
    ensure!(
        recovery_candidate_count == 0 && recovery_archive_removed,
        "covering manual save did not retire recovery authority"
    );
    let after_save_reopen_author = nested_author_evidence(
        workflow.app(),
        parent_sequence_id,
        replacement_clip_id,
        nested_sequence_id,
    )?;
    let execution_after_save_reopen = execute_nested_frame(workflow.app(), evaluation_frame)?;
    ensure!(
        before_recovery_author == after_save_reopen_author
            && execution_before_recovery == execution_after_save_reopen,
        "manual save/reopen changed recovered nested author or execution semantics"
    );
    workflow.verify_binding()?;

    let operations = vec![
        OperationEvidence::AutosaveRecovery {
            autosave_file,
            autosave_sha256,
            saved_at_unix_ms: candidate_saved_at,
            manifest_snapshot_count: candidate_snapshot_count,
            before_close,
            recovered_session,
            session_identity_changed: true,
            project_identity_preserved: true,
            recovery_authority_retained,
        },
        OperationEvidence::SaveReopen {
            durability,
            recovery_candidate_count,
            recovery_archive_removed,
        },
    ];
    let content = vec![ContentEvidence::NestedSequence {
        author_step: precompose_step,
        before_recovery: before_recovery_author,
        after_recovery: after_recovery_author,
        after_save_reopen: after_save_reopen_author,
    }];
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

    Ok(GoldenRecoveryNestingReport {
        schema_version: 1,
        profile: RECOVERY_NESTING_SLICE_ID,
        contract_id: contract.id.clone(),
        status: "passed",
        complete_golden_project: false,
        setup: RecoveryNestingSetupEvidence {
            project_path,
            stage,
            solid_asset_id: solid_asset_id.to_string(),
            video_track_id: video_track_id.to_string(),
            source_clip_id: source_clip_id.to_string(),
            replacement_clip_id: replacement_clip_id.to_string(),
            nested_sequence_id: nested_sequence_id.to_string(),
            evaluation_frame,
            drop_step,
        },
        operations,
        content,
        execution_before_recovery,
        execution_after_recovery,
        execution_after_save_reopen,
    })
}

fn new_run_paths(root: &Path) -> anyhow::Result<GoldenRunPaths> {
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-recovery-nesting")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-recovery-nesting-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-recovery-nesting.mdp"),
        directory,
        report,
    })
}

fn execute_recovery_nesting_slice(
    root: &Path,
    paths: &GoldenRunPaths,
) -> anyhow::Result<GoldenRecoveryNestingReport> {
    let contract = load_golden_contract(root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let mut workflow = GoldenProductWorkflowDriver::create(
        paths.project.clone(),
        "Windows Alpha Golden Recovery + Nesting",
        settings,
        mondrian_core::ProjectColorEnvironment::default(),
        mondrian_core::ProjectSettings::default(),
    )?;
    execute_recovery_nesting_stage(&contract, &mut workflow)
}

#[test]
fn golden_project_recovery_nesting_roundtrip_gate() -> anyhow::Result<()> {
    let root = repository_root();
    let paths = new_run_paths(&root)?;
    match execute_recovery_nesting_slice(&root, &paths) {
        Ok(report) => {
            write_report(&paths.report, &report)?;
            eprintln!(
                "MONDRIAN_GOLDEN_RECOVERY_NESTING_REPORT_JSON={}",
                serde_json::to_string(&report)?
            );
            eprintln!(
                "MONDRIAN_GOLDEN_RECOVERY_NESTING_REPORT_PATH={}",
                paths.report.display()
            );
            eprintln!(
                "MONDRIAN_GOLDEN_RECOVERY_NESTING_RUN_DIRECTORY={}",
                paths.directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schema_version": 1,
                "profile": RECOVERY_NESTING_SLICE_ID,
                "status": "failed",
                "complete_golden_project": false,
                "error": format!("{error:#}")
            });
            write_report(&paths.report, &failure)?;
            Err(error)
        }
    }
}
