//! Complete Golden coordination and focused composition gates over one Project.

use super::harness::{new_run_directory, write_report, DirectoryCleanup, DurableReopenEvidence};
use super::plan::{GoldenAcceptancePlan, GoldenAcceptancePlanStatus};
use super::workflow::GoldenProductWorkflowDriver;
use super::{
    assert_export_contract, color_media_roundtrip, editorial_transport, foundation_audio,
    generated_delivery, load_golden_contract, proxy_relink, recovery_nesting, repository_root,
    sequence_settings_from_contract, visual_authoring, GoldenProjectContract,
};
use anyhow::{ensure, Context};
use mondrian_core::{FrameRounding, ProjectColorEnvironment, ProjectSettings, SequenceId};
use mondrian_media::info::VideoCodecProfile;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ComposedRun {
    // Field order is intentional: close App/SQLite/export handles before deleting the run root.
    workflow: GoldenProductWorkflowDriver,
    _cleanup: DirectoryCleanup,
    directory: PathBuf,
}

impl ComposedRun {
    fn create(
        root: &Path,
        name: &str,
        retain_artifacts: bool,
    ) -> anyhow::Result<(GoldenProjectContract, Self)> {
        let contract = load_golden_contract(root)?;
        let settings = sequence_settings_from_contract(&contract.timeline)?;
        for export in &contract.exports {
            assert_export_contract(export, &settings)?;
        }
        let directory =
            new_run_directory(root, "MONDRIAN_GOLDEN_COMPOSED_RUN_ROOT", "golden-composed")?;
        let mut cleanup = DirectoryCleanup::default();
        if !retain_artifacts {
            cleanup.track(Some(directory.clone()));
        }
        let project_settings = ProjectSettings {
            cache_dir: Some(directory.join("cache")),
            ..ProjectSettings::default()
        };
        let workflow = GoldenProductWorkflowDriver::create(
            directory.join("windows-alpha-golden-composed.mdp"),
            name,
            settings,
            ProjectColorEnvironment::default(),
            project_settings,
        )?;
        Ok((contract, Self { workflow, _cleanup: cleanup, directory }))
    }
}

#[derive(Debug, Serialize)]
struct GoldenExecutedStage {
    id: String,
    report: Value,
}

#[derive(Debug, Serialize)]
struct GoldenFinalProjectEvidence {
    project_id: mondrian_core::ProjectId,
    project_path: PathBuf,
    hero_sequence_role: String,
    hero_sequence_id: SequenceId,
    sequence_ids: BTreeMap<String, Vec<SequenceId>>,
    primary_sequence_ids: BTreeMap<String, SequenceId>,
    sequence_count: usize,
    asset_count: usize,
    relinked_asset_path: PathBuf,
    relinked_proxy_mode: bool,
    exported_profiles: Vec<VideoCodecProfile>,
    proxy_queued: usize,
    proxy_running: usize,
    proxy_completions: u64,
    proxy_failures: u64,
    proxy_cancellations: u64,
    durable_reopen: DurableReopenEvidence,
}

#[derive(Debug, Serialize)]
struct GoldenCompleteRunReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    status: &'static str,
    complete_golden_project: bool,
    run_id: String,
    started_at_unix_ms: u64,
    elapsed_ms: u64,
    required_consecutive_passes: u32,
    execution_plan: GoldenAcceptancePlan,
    stages: Vec<GoldenExecutedStage>,
    final_project: GoldenFinalProjectEvidence,
}

#[derive(Debug, Serialize)]
struct GoldenCompleteFailureReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    status: &'static str,
    complete_golden_project: bool,
    run_id: String,
    error: String,
}

fn run_identity(directory: &Path) -> anyhow::Result<String> {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .context("Golden run directory has no UTF-8 identity")
}

fn capture_stage<T: Serialize>(
    stages: &mut BTreeMap<String, Value>,
    contract: &GoldenProjectContract,
    slice_id: &str,
    report: &T,
) -> anyhow::Result<()> {
    let value = serde_json::to_value(report)?;
    ensure!(
        value.get("contract_id").and_then(Value::as_str) == Some(contract.id.as_str()),
        "{slice_id} report changed Golden contract identity"
    );
    ensure!(
        value.get("complete_golden_project").and_then(Value::as_bool) == Some(false),
        "{slice_id} slice report claimed complete Golden status"
    );
    ensure!(
        matches!(
            value.get("status").and_then(Value::as_str),
            Some("pass" | "passed")
        ),
        "{slice_id} stage did not report a passing status"
    );
    ensure!(
        stages.insert(slice_id.to_owned(), value).is_none(),
        "{slice_id} executed more than once"
    );
    Ok(())
}

fn wait_for_proxy_quiescence(
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<crate::app::proxy_generation::ProxyGenerationDiagnostics> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        workflow.app_mut().poll_proxy_generation();
        let diagnostics = workflow.app().proxy_generation_diagnostics();
        if diagnostics.queued == 0 && diagnostics.running == 0 {
            ensure!(
                diagnostics.failures == 0,
                "background proxy execution retained {} failures",
                diagnostics.failures
            );
            return Ok(diagnostics);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for Golden proxy execution to quiesce"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn execute_foundation_and_visual(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<(
    SequenceId,
    SequenceId,
    foundation_audio::GoldenFoundationReport,
    visual_authoring::GoldenVisualReport,
)> {
    let foundation = foundation_audio::execute_foundation_stage(root, contract, workflow)?;
    let foundation_sequence_id = foundation.primary_sequence_id();
    let foundation_audio_before_visual = foundation.capture_audio_anchor(workflow.app())?;

    let visual = visual_authoring::execute_visual_stage(contract, workflow)?;
    let visual_sequence_id = visual.primary_sequence_id();
    workflow.verify_binding()?;
    ensure!(
        foundation_sequence_id == visual_sequence_id
            && foundation_sequence_id == workflow.hero_sequence_id(),
        "foundation and visual stages did not use the one Hero Sequence"
    );
    ensure!(
        workflow.app().sequences().len() == 1,
        "foundation and visual stages created an undeclared auxiliary Sequence"
    );
    let foundation_audio_after_visual = foundation.capture_audio_anchor(workflow.app())?;
    ensure!(
        foundation_audio_before_visual == foundation_audio_after_visual,
        "visual authoring changed the Foundation audio Track, Clip, Component Edit, or Audio Program"
    );
    visual.verify_retained_authoring(workflow.app())?;
    Ok((
        foundation_sequence_id,
        visual_sequence_id,
        foundation,
        visual,
    ))
}

fn sequence_snapshot(
    workflow: &GoldenProductWorkflowDriver,
    sequence_id: SequenceId,
) -> anyhow::Result<Value> {
    let sequence = workflow
        .app()
        .sequences()
        .iter()
        .find(|sequence| sequence.id == sequence_id)
        .with_context(|| format!("stage Sequence is absent: {sequence_id}"))?;
    Ok(serde_json::to_value(sequence)?)
}

fn sequence_content_end_frame(sequence: &mondrian_timeline::Sequence) -> anyhow::Result<i64> {
    sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .map(|clip| {
            clip.end_position()?
                .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)
                .map(|position| position.frame)
                .map_err(anyhow::Error::from)
        })
        .try_fold(0_i64, |end, candidate| {
            candidate.map(|candidate| end.max(candidate))
        })
}

fn resolve_hero_sequence_id(
    contract: &GoldenProjectContract,
    stage_primary_sequence_ids: &BTreeMap<String, SequenceId>,
) -> anyhow::Result<SequenceId> {
    let hero_sequence_ids = contract
        .execution_slices
        .iter()
        .filter(|slice| slice.sequence_role == contract.hero_sequence.role)
        .map(|slice| {
            stage_primary_sequence_ids.get(&slice.id).copied().with_context(|| {
                format!("Hero-assigned slice has no primary Sequence: {}", slice.id)
            })
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    ensure!(
        hero_sequence_ids.len() == 1,
        "Hero-assigned Golden slices used {} primary Sequences instead of one",
        hero_sequence_ids.len()
    );
    hero_sequence_ids
        .first()
        .copied()
        .context("Golden execution produced no Hero Sequence identity")
}

#[test]
#[ignore = "composed Golden stages require the canonical PCM fixture and Windows Basic Title font"]
fn golden_foundation_and_visual_stages_share_one_hero_sequence() -> anyhow::Result<()> {
    let root = repository_root();
    let (contract, mut run) =
        ComposedRun::create(&root, "Windows Alpha Golden Foundation + Visual", false)?;
    let project_id = run.workflow.project_id();
    let project_path = run.workflow.project_path().to_path_buf();

    let _ = execute_foundation_and_visual(&root, &contract, &mut run.workflow)?;
    ensure!(
        run.workflow.project_id() == project_id
            && run.workflow.project_path() == project_path
            && run.workflow.app().project_id() == Some(project_id),
        "composed stages changed the Golden Project binding"
    );
    Ok(())
}

fn execute_complete_golden_project(
    root: &Path,
    contract: &GoldenProjectContract,
    run: &mut ComposedRun,
) -> anyhow::Result<GoldenCompleteRunReport> {
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let started = Instant::now();
    let execution_plan = GoldenAcceptancePlan::compile(contract);
    ensure!(
        execution_plan.status == GoldenAcceptancePlanStatus::Complete
            && !execution_plan.complete_golden_project,
        "Golden execution plan is blocked because required obligations are not all assigned to \
         Hero Sequence role `{}`: {:?}",
        execution_plan.hero_sequence_role,
        execution_plan.hero_missing
    );
    let project_id = run.workflow.project_id();
    let project_path = run.workflow.project_path().to_path_buf();
    let mut stage_reports = BTreeMap::new();
    let mut stage_sequence_ids = BTreeMap::new();
    let mut stage_primary_sequence_ids = BTreeMap::new();
    let (foundation_sequence_id, visual_sequence_id, foundation, visual) =
        execute_foundation_and_visual(root, contract, &mut run.workflow)?;
    capture_stage(
        &mut stage_reports,
        contract,
        foundation_audio::FOUNDATION_SLICE_ID,
        &foundation,
    )?;
    capture_stage(
        &mut stage_reports,
        contract,
        visual_authoring::VISUAL_SLICE_ID,
        &visual,
    )?;
    stage_sequence_ids.insert(
        foundation_audio::FOUNDATION_SLICE_ID.to_owned(),
        vec![foundation_sequence_id],
    );
    stage_primary_sequence_ids.insert(
        foundation_audio::FOUNDATION_SLICE_ID.to_owned(),
        foundation_sequence_id,
    );
    stage_sequence_ids.insert(
        visual_authoring::VISUAL_SLICE_ID.to_owned(),
        vec![visual_sequence_id],
    );
    stage_primary_sequence_ids.insert(
        visual_authoring::VISUAL_SLICE_ID.to_owned(),
        visual_sequence_id,
    );

    let editorial =
        editorial_transport::execute_editorial_stage(root, contract, &mut run.workflow)?;
    capture_stage(
        &mut stage_reports,
        contract,
        editorial_transport::EDITORIAL_SLICE_ID,
        &editorial,
    )?;
    let editorial_sequence_id =
        run.workflow.app().active_sequence().context("editorial Sequence is absent")?.id;
    ensure!(
        ![foundation_sequence_id, visual_sequence_id].contains(&editorial_sequence_id),
        "editorial did not create a distinct stage Sequence"
    );
    stage_sequence_ids.insert(
        editorial_transport::EDITORIAL_SLICE_ID.to_owned(),
        vec![editorial_sequence_id],
    );
    stage_primary_sequence_ids.insert(
        editorial_transport::EDITORIAL_SLICE_ID.to_owned(),
        editorial_sequence_id,
    );
    let proxy_relink = proxy_relink::execute_proxy_relink_stage(
        root,
        contract,
        &mut run.workflow,
        &run.directory,
    )?;
    capture_stage(
        &mut stage_reports,
        contract,
        proxy_relink::PROXY_RELINK_SLICE_ID,
        &proxy_relink,
    )?;
    let proxy_relink_sequence_id = run
        .workflow
        .app()
        .active_sequence()
        .context("proxy/relink Sequence is absent")?
        .id;
    ensure!(
        ![
            foundation_sequence_id,
            visual_sequence_id,
            editorial_sequence_id
        ]
        .contains(&proxy_relink_sequence_id),
        "proxy/relink did not create a distinct stage Sequence"
    );
    stage_sequence_ids.insert(
        proxy_relink::PROXY_RELINK_SLICE_ID.to_owned(),
        vec![proxy_relink_sequence_id],
    );
    stage_primary_sequence_ids.insert(
        proxy_relink::PROXY_RELINK_SLICE_ID.to_owned(),
        proxy_relink_sequence_id,
    );
    let proxy_relink_asset_id = run
        .workflow
        .app()
        .active_sequence()
        .and_then(|sequence| sequence.video_tracks.first())
        .and_then(|track| track.clips.first())
        .and_then(|clip| clip.library_asset_id())
        .context("proxy/relink stage retained no media-backed video Clip")?;
    let proxy_relink_asset_path = run
        .workflow
        .app()
        .asset_library()
        .context("Asset Library is absent after proxy/relink stage")?
        .get_asset(proxy_relink_asset_id)?
        .context("relinked asset is absent after proxy/relink stage")?
        .path;
    let delivery = generated_delivery::execute_delivery_stage(
        root,
        contract,
        &mut run.workflow,
        &run.directory,
    )?;
    capture_stage(
        &mut stage_reports,
        contract,
        generated_delivery::DELIVERY_SLICE_ID,
        &delivery,
    )?;
    let delivery_sequence_id =
        run.workflow.app().active_sequence().context("delivery Sequence is absent")?.id;
    ensure!(
        ![
            foundation_sequence_id,
            visual_sequence_id,
            editorial_sequence_id,
            proxy_relink_sequence_id
        ]
        .contains(&delivery_sequence_id),
        "delivery did not create a distinct stage Sequence"
    );
    stage_sequence_ids.insert(
        generated_delivery::DELIVERY_SLICE_ID.to_owned(),
        vec![delivery_sequence_id],
    );
    stage_primary_sequence_ids.insert(
        generated_delivery::DELIVERY_SLICE_ID.to_owned(),
        delivery_sequence_id,
    );
    let before_recovery_sequence_ids = run
        .workflow
        .app()
        .sequences()
        .iter()
        .map(|sequence| sequence.id)
        .collect::<Vec<_>>();
    let recovery_nesting =
        recovery_nesting::execute_recovery_nesting_stage(contract, &mut run.workflow)?;
    capture_stage(
        &mut stage_reports,
        contract,
        recovery_nesting::RECOVERY_NESTING_SLICE_ID,
        &recovery_nesting,
    )?;
    let recovery_sequence_ids = run
        .workflow
        .app()
        .sequences()
        .iter()
        .map(|sequence| sequence.id)
        .filter(|sequence_id| !before_recovery_sequence_ids.contains(sequence_id))
        .collect::<Vec<_>>();
    ensure!(
        recovery_sequence_ids.len() == 2,
        "recovery/nesting must retain one parent and one nested Sequence"
    );
    let recovery_nesting_sequence_id = run
        .workflow
        .app()
        .active_sequence()
        .context("recovery/nesting Sequence is absent")?
        .id;
    ensure!(
        ![
            foundation_sequence_id,
            visual_sequence_id,
            editorial_sequence_id,
            proxy_relink_sequence_id,
            delivery_sequence_id
        ]
        .contains(&recovery_nesting_sequence_id),
        "recovery/nesting did not create a distinct stage Sequence"
    );
    ensure!(
        recovery_sequence_ids.contains(&recovery_nesting_sequence_id),
        "recovery/nesting active parent is not owned by the stage"
    );
    stage_sequence_ids.insert(
        recovery_nesting::RECOVERY_NESTING_SLICE_ID.to_owned(),
        recovery_sequence_ids,
    );
    stage_primary_sequence_ids.insert(
        recovery_nesting::RECOVERY_NESTING_SLICE_ID.to_owned(),
        recovery_nesting_sequence_id,
    );
    let color_media = color_media_roundtrip::execute_color_media_stage(
        root,
        contract,
        &mut run.workflow,
        &run.directory,
    )?;
    capture_stage(
        &mut stage_reports,
        contract,
        color_media_roundtrip::COLOR_MEDIA_SLICE_ID,
        &color_media,
    )?;
    let color_media_sequence_id = run
        .workflow
        .app()
        .active_sequence()
        .context("color-media Sequence is absent")?
        .id;
    ensure!(
        ![
            foundation_sequence_id,
            visual_sequence_id,
            editorial_sequence_id,
            proxy_relink_sequence_id,
            delivery_sequence_id,
            recovery_nesting_sequence_id
        ]
        .contains(&color_media_sequence_id),
        "color-media did not create a distinct stage Sequence"
    );
    stage_sequence_ids.insert(
        color_media_roundtrip::COLOR_MEDIA_SLICE_ID.to_owned(),
        vec![color_media_sequence_id],
    );
    stage_primary_sequence_ids.insert(
        color_media_roundtrip::COLOR_MEDIA_SLICE_ID.to_owned(),
        color_media_sequence_id,
    );
    let hero_sequence_id = resolve_hero_sequence_id(contract, &stage_primary_sequence_ids)?;
    ensure!(
        run.workflow
            .app()
            .sequences()
            .iter()
            .any(|sequence| sequence.id == hero_sequence_id),
        "Hero Sequence is absent before final durable reopen"
    );
    let proxy_diagnostics = wait_for_proxy_quiescence(&mut run.workflow)?;
    let final_sequence_snapshots = run
        .workflow
        .app()
        .sequences()
        .iter()
        .map(|sequence| Ok((sequence.id, serde_json::to_value(sequence)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let expected_sequence_ids = stage_sequence_ids
        .values()
        .flatten()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let expected_sequence_id_count = expected_sequence_ids.len();
    let actual_sequence_ids = final_sequence_snapshots
        .iter()
        .map(|(sequence_id, _)| sequence_id.to_string())
        .collect::<BTreeSet<_>>();
    ensure!(
        actual_sequence_ids == expected_sequence_ids,
        "final Project Sequence set does not exactly match stage-owned authoring"
    );
    let durable_reopen = run.workflow.durable_save_reopen()?;
    run.workflow.verify_binding()?;
    ensure!(
        run.workflow.project_id() == project_id
            && run.workflow.project_path() == project_path
            && run.workflow.app().project_id() == Some(project_id),
        "seven-stage workflow changed the Golden Project binding"
    );
    ensure!(
        run.workflow.app().sequences().len() == expected_sequence_id_count,
        "durable reopen changed the stage-owned Sequence count"
    );
    for (sequence_id, expected) in final_sequence_snapshots {
        ensure!(
            sequence_snapshot(&run.workflow, sequence_id)? == expected,
            "final durable reopen changed stage Sequence {sequence_id}"
        );
    }
    let hero_sequence = run
        .workflow
        .app()
        .sequences()
        .iter()
        .find(|sequence| sequence.id == hero_sequence_id)
        .context("final durable reopen lost the Hero Sequence")?;
    ensure!(
        sequence_content_end_frame(hero_sequence)? == contract.hero_sequence.duration_frames,
        "Hero Sequence content boundary does not equal the required {} frames",
        contract.hero_sequence.duration_frames
    );

    let assets = run
        .workflow
        .app()
        .asset_library()
        .context("Asset Library is absent after final durable reopen")?
        .list_assets()?;
    let reopened_proxy_asset = assets
        .iter()
        .find(|asset| asset.id == proxy_relink_asset_id)
        .context("final Project library lost the relinked H.264 asset")?;
    ensure!(
        reopened_proxy_asset.path == proxy_relink_asset_path
            && reopened_proxy_asset.path.is_file()
            && run.workflow.app().is_asset_proxy_mode(proxy_relink_asset_id),
        "final durable reopen lost relinked source identity or proxy author intent"
    );
    let exported_profiles = assets
        .iter()
        .filter(|asset| {
            asset
                .path
                .extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("mp4"))
        })
        .filter_map(|asset| asset.media_info.primary_video().map(|video| video.codec_profile))
        .collect::<Vec<_>>();
    ensure!(
        exported_profiles.contains(&VideoCodecProfile::H264High)
            && exported_profiles.contains(&VideoCodecProfile::HevcMain10),
        "final Project library lost the reimported H.264 High or HEVC Main10 deliverable"
    );

    let required_stage_ids = contract
        .execution_slices
        .iter()
        .map(|slice| slice.id.clone())
        .collect::<BTreeSet<_>>();
    let executed_stage_ids = stage_reports.keys().cloned().collect::<BTreeSet<_>>();
    ensure!(
        executed_stage_ids == required_stage_ids,
        "complete Golden run did not execute exactly the declared slice set"
    );
    let stages = contract
        .execution_slices
        .iter()
        .map(|slice| {
            let report = stage_reports
                .remove(&slice.id)
                .with_context(|| format!("missing executed stage report: {}", slice.id))?;
            Ok(GoldenExecutedStage { id: slice.id.clone(), report })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        stage_reports.is_empty(),
        "complete Golden run retained undeclared stage reports"
    );

    let run_id = run_identity(&run.directory)?;
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    Ok(GoldenCompleteRunReport {
        schema_version: 1,
        profile: "windows-alpha-complete-golden-project",
        contract_id: contract.id.clone(),
        status: "pass",
        complete_golden_project: true,
        run_id,
        started_at_unix_ms,
        elapsed_ms,
        required_consecutive_passes: contract.acceptance.consecutive_passes,
        execution_plan,
        stages,
        final_project: GoldenFinalProjectEvidence {
            project_id,
            project_path,
            hero_sequence_role: contract.hero_sequence.role.clone(),
            hero_sequence_id,
            sequence_ids: stage_sequence_ids,
            primary_sequence_ids: stage_primary_sequence_ids,
            sequence_count: run.workflow.app().sequences().len(),
            asset_count: assets.len(),
            relinked_asset_path: reopened_proxy_asset.path.clone(),
            relinked_proxy_mode: run.workflow.app().is_asset_proxy_mode(proxy_relink_asset_id),
            exported_profiles,
            proxy_queued: proxy_diagnostics.queued,
            proxy_running: proxy_diagnostics.running,
            proxy_completions: proxy_diagnostics.completions,
            proxy_failures: proxy_diagnostics.failures,
            proxy_cancellations: proxy_diagnostics.cancellations,
            durable_reopen,
        },
    })
}

pub(super) fn run_complete_golden_project(output: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let root = repository_root();
    let (contract, mut run) =
        ComposedRun::create(&root, "Windows Alpha Complete Golden Project", true)?;
    let output = output.unwrap_or_else(|| run.directory.join("golden-complete-run-report.json"));
    match execute_complete_golden_project(&root, &contract, &mut run) {
        Ok(report) => {
            write_report(&output, &report)?;
            Ok(output)
        }
        Err(error) => {
            let failure = GoldenCompleteFailureReport {
                schema_version: 1,
                profile: "windows-alpha-complete-golden-project",
                contract_id: contract.id.clone(),
                status: "fail",
                complete_golden_project: false,
                run_id: run_identity(&run.directory)?,
                error: format!("{error:#}"),
            };
            write_report(&output, &failure)?;
            Err(error)
        }
    }
}

#[test]
fn complete_coordinator_accepts_only_partial_unique_slice_reports() -> anyhow::Result<()> {
    let contract = load_golden_contract(&repository_root())?;
    let slice_id = contract
        .execution_slices
        .first()
        .context("Golden contract has no execution slices")?
        .id
        .as_str();
    let mut stages = BTreeMap::new();
    let partial = serde_json::json!({
        "contract_id": contract.id.clone(),
        "status": "passed",
        "complete_golden_project": false
    });
    capture_stage(&mut stages, &contract, slice_id, &partial)?;
    ensure!(
        capture_stage(&mut stages, &contract, slice_id, &partial).is_err(),
        "duplicate slice report was accepted"
    );

    let mut stages = BTreeMap::new();
    let overclaim = serde_json::json!({
        "contract_id": contract.id.clone(),
        "status": "passed",
        "complete_golden_project": true
    });
    ensure!(
        capture_stage(&mut stages, &contract, slice_id, &overclaim).is_err(),
        "slice report was allowed to claim complete Golden status"
    );
    Ok(())
}

#[test]
fn complete_coordinator_requires_one_primary_hero_sequence_identity() -> anyhow::Result<()> {
    let mut contract = load_golden_contract(&repository_root())?;
    for slice in &mut contract.execution_slices {
        slice.sequence_role.clone_from(&contract.hero_sequence.role);
    }
    let shared = SequenceId::new();
    let mut primary_ids = contract
        .execution_slices
        .iter()
        .map(|slice| (slice.id.clone(), shared))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(resolve_hero_sequence_id(&contract, &primary_ids)?, shared);

    let first_slice = contract.execution_slices.first().context("Golden contract has no slices")?;
    primary_ids.insert(first_slice.id.clone(), SequenceId::new());
    let error = resolve_hero_sequence_id(&contract, &primary_ids)
        .expect_err("multiple primary Hero identities must fail closed");
    assert!(error.to_string().contains("primary Sequences instead of one"));

    primary_ids.remove(&first_slice.id);
    let error = resolve_hero_sequence_id(&contract, &primary_ids)
        .expect_err("missing Hero primary identity must fail closed");
    assert!(error.to_string().contains("has no primary Sequence"));
    Ok(())
}

#[test]
#[ignore = "complete Golden execution runs through the dedicated mondrian-golden process entrypoint"]
fn golden_current_stages_share_one_project() -> anyhow::Result<()> {
    let root = repository_root();
    let (contract, mut run) =
        ComposedRun::create(&root, "Windows Alpha Golden Existing Stages", false)?;
    let report = execute_complete_golden_project(&root, &contract, &mut run)?;
    ensure!(
        report.complete_golden_project && report.stages.len() == contract.execution_slices.len(),
        "complete composed Golden report did not close every declared stage"
    );
    Ok(())
}
