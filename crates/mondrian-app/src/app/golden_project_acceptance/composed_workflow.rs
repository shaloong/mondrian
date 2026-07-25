//! Composed partial Golden gates over one production Project.

use super::harness::{new_run_directory, DirectoryCleanup};
use super::workflow::GoldenProductWorkflowDriver;
use super::{
    color_media_roundtrip, editorial_transport, foundation_audio, generated_delivery,
    load_golden_contract, proxy_relink, recovery_nesting, repository_root,
    sequence_settings_from_contract, visual_authoring, GoldenProjectContract,
};
use anyhow::{ensure, Context};
use mondrian_core::{ProjectColorEnvironment, ProjectSettings, SequenceId};
use mondrian_media::info::VideoCodecProfile;
use serde_json::Value;
use std::path::{Path, PathBuf};

struct ComposedRun {
    // Field order is intentional: close App/SQLite/export handles before deleting the run root.
    workflow: GoldenProductWorkflowDriver,
    _cleanup: DirectoryCleanup,
    directory: PathBuf,
}

impl ComposedRun {
    fn create(root: &Path, name: &str) -> anyhow::Result<(GoldenProjectContract, Self)> {
        let contract = load_golden_contract(root)?;
        let settings = sequence_settings_from_contract(&contract.timeline)?;
        let directory =
            new_run_directory(root, "MONDRIAN_GOLDEN_COMPOSED_RUN_ROOT", "golden-composed")?;
        let mut cleanup = DirectoryCleanup::default();
        cleanup.track(Some(directory.clone()));
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

fn execute_foundation_and_visual(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<(SequenceId, SequenceId)> {
    let _foundation = foundation_audio::execute_foundation_stage(root, contract, workflow)?;
    let foundation_sequence_id =
        workflow.app().active_sequence().context("foundation Sequence is absent")?.id;
    ensure!(
        workflow.app().active_sequence().is_some_and(|sequence| sequence
            .audio_tracks
            .iter()
            .any(|track| !track.clips.is_empty())),
        "foundation stage did not retain its authored PCM Clip"
    );

    let _visual = visual_authoring::execute_visual_stage(contract, workflow)?;
    workflow.verify_binding()?;
    ensure!(
        workflow.app().sequences().len() == 2,
        "foundation and visual stages must occupy exactly two Sequences in one Project"
    );
    let foundation_sequence = workflow
        .app()
        .sequences()
        .iter()
        .find(|sequence| sequence.id == foundation_sequence_id)
        .context("visual stage removed the foundation Sequence")?;
    ensure!(
        foundation_sequence.audio_tracks.iter().any(|track| !track.clips.is_empty()),
        "visual save/reopen discarded foundation audio authoring"
    );
    let visual_sequence = workflow
        .app()
        .active_sequence()
        .context("visual Sequence is absent after durable reopen")?;
    ensure!(
        visual_sequence.id != foundation_sequence_id
            && visual_sequence.video_transitions.len() == 1
            && visual_sequence
                .video_tracks
                .iter()
                .flat_map(|track| &track.clips)
                .any(|clip| clip.is_basic_title()),
        "visual stage did not retain its distinct Transition and Basic Title Sequence"
    );
    Ok((foundation_sequence_id, visual_sequence.id))
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

#[test]
#[ignore = "composed Golden stages require the canonical PCM fixture and Windows Basic Title font"]
fn golden_foundation_and_visual_stages_share_one_project() -> anyhow::Result<()> {
    let root = repository_root();
    let (contract, mut run) =
        ComposedRun::create(&root, "Windows Alpha Golden Foundation + Visual")?;
    let project_id = run.workflow.project_id();
    let project_path = run.workflow.project_path().to_path_buf();

    execute_foundation_and_visual(&root, &contract, &mut run.workflow)?;
    ensure!(
        run.workflow.project_id() == project_id
            && run.workflow.project_path() == project_path
            && run.workflow.app().project_id() == Some(project_id),
        "composed stages changed the Golden Project binding"
    );
    Ok(())
}

#[test]
#[ignore = "seven-stage Golden composition requires generated PCM/AAC/H.264/HLG/PNG fixtures, Windows Basic Title font, and production FFmpeg encoders"]
fn golden_current_stages_share_one_project() -> anyhow::Result<()> {
    let root = repository_root();
    let (contract, mut run) = ComposedRun::create(&root, "Windows Alpha Golden Existing Stages")?;
    let project_id = run.workflow.project_id();
    let project_path = run.workflow.project_path().to_path_buf();
    let (foundation_sequence_id, visual_sequence_id) =
        execute_foundation_and_visual(&root, &contract, &mut run.workflow)?;

    let _editorial =
        editorial_transport::execute_editorial_stage(&root, &contract, &mut run.workflow)?;
    let editorial_sequence_id =
        run.workflow.app().active_sequence().context("editorial Sequence is absent")?.id;
    ensure!(
        ![foundation_sequence_id, visual_sequence_id].contains(&editorial_sequence_id),
        "editorial did not create a distinct stage Sequence"
    );
    let _proxy_relink = proxy_relink::execute_proxy_relink_stage(
        &root,
        &contract,
        &mut run.workflow,
        &run.directory,
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
    let pre_delivery_snapshots = [
        (
            foundation_sequence_id,
            sequence_snapshot(&run.workflow, foundation_sequence_id)?,
        ),
        (
            visual_sequence_id,
            sequence_snapshot(&run.workflow, visual_sequence_id)?,
        ),
        (
            editorial_sequence_id,
            sequence_snapshot(&run.workflow, editorial_sequence_id)?,
        ),
        (
            proxy_relink_sequence_id,
            sequence_snapshot(&run.workflow, proxy_relink_sequence_id)?,
        ),
    ];

    let _delivery = generated_delivery::execute_delivery_stage(
        &root,
        &contract,
        &mut run.workflow,
        &run.directory,
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
    let delivery_snapshot = sequence_snapshot(&run.workflow, delivery_sequence_id)?;
    let _recovery_nesting =
        recovery_nesting::execute_recovery_nesting_stage(&contract, &mut run.workflow)?;
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
    let recovery_nesting_snapshot = sequence_snapshot(&run.workflow, recovery_nesting_sequence_id)?;
    let _color_media = color_media_roundtrip::execute_color_media_stage(
        &root,
        &contract,
        &mut run.workflow,
        &run.directory,
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
    let color_media_snapshot = sequence_snapshot(&run.workflow, color_media_sequence_id)?;
    run.workflow.durable_save_reopen()?;
    run.workflow.verify_binding()?;
    ensure!(
        run.workflow.project_id() == project_id
            && run.workflow.project_path() == project_path
            && run.workflow.app().project_id() == Some(project_id),
        "seven-stage workflow changed the Golden Project binding"
    );
    ensure!(
        run.workflow.app().sequences().len() == 7,
        "seven current Golden stages must retain exactly seven Sequences"
    );
    for (sequence_id, expected) in pre_delivery_snapshots.into_iter().chain([
        (delivery_sequence_id, delivery_snapshot),
        (recovery_nesting_sequence_id, recovery_nesting_snapshot),
        (color_media_sequence_id, color_media_snapshot),
    ]) {
        ensure!(
            sequence_snapshot(&run.workflow, sequence_id)? == expected,
            "final durable reopen changed stage Sequence {sequence_id}"
        );
    }

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
    Ok(())
}
