//! File-backed color and Alpha Golden slice over production product Interfaces.
//!
//! The fixtures are deterministic, project-authored stimuli. Mondrian owns
//! import interpretation, Timeline placement, Preview execution, Export, and
//! reimport. Small analytic checks remain independent of the production color
//! processor so a self-consistent but wrong transform cannot pass unnoticed.

mod authoring;
mod delivery;
mod media_execution;
mod picture_validation;

use super::fixture::{resolve_fixture, CorpusManifest, FixtureEvidence};
use super::harness::{ensure_exact_requirement_evidence, fixture_root};
#[cfg(test)]
use super::harness::{new_run_directory, rooted_env_path, write_report};
use super::workflow::GoldenProductWorkflowDriver;
#[cfg(test)]
use super::{load_golden_contract, repository_root, sequence_settings_from_contract};
use super::{load_json, GoldenProjectContract};
use anyhow::{ensure, Context};
use mondrian_core::Resolution;
use mondrian_media::PreviewDecodeSessionContext;
use serde::Serialize;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;

pub(super) const COLOR_MEDIA_SLICE_ID: &str = "color-media-roundtrip-v1";
#[cfg(test)]
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_COLOR_MEDIA_RUN_ROOT";
#[cfg(test)]
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_COLOR_MEDIA_OUTPUT";
pub(super) const PREVIEW_RESOLUTION: Resolution = Resolution { width: 1920, height: 1080 };
pub(super) const EXPORT_TIMEOUT: Duration = Duration::from_secs(600);
pub(super) const HLG_ROLE: &str = "hlg-main10-picture";
pub(super) const SRGB_ALPHA_ROLE: &str = "srgb-alpha-still";

#[derive(Debug)]
#[cfg(test)]
struct GoldenRunPaths {
    directory: PathBuf,
    project: PathBuf,
    report: PathBuf,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenColorMediaReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: &'static str,
    complete_golden_project: bool,
    fixtures: Vec<FixtureEvidence>,
    setup: authoring::SetupEvidence,
    source_patches: picture_validation::SourcePatchEvidence,
    preview: picture_validation::PreviewEvidence,
    export_roundtrip: delivery::ExportRoundtripEvidence,
}

#[cfg(test)]
fn new_run_paths(root: &Path) -> anyhow::Result<GoldenRunPaths> {
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-color-media")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-color-media-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-color-media.mdp"),
        directory,
        report,
    })
}

pub(super) fn execute_color_media_stage(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
    output_directory: &Path,
) -> anyhow::Result<GoldenColorMediaReport> {
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == COLOR_MEDIA_SLICE_ID)
        .context("color-media Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == [HLG_ROLE, SRGB_ALPHA_ROLE]
            && slice.required_operations.is_empty()
            && slice.required_content.is_empty()
            && slice.required_exports.is_empty(),
        "color-media slice contract drifted"
    );
    let window = slice.timeline_window.context("color-media slice has no timeline window")?;
    ensure!(
        window.start_frame == 0 && window.end_frame_exclusive == 25,
        "color-media slice timeline window drifted"
    );
    ensure_exact_requirement_evidence(
        &slice.required_fixture_roles,
        [HLG_ROLE, SRGB_ALPHA_ROLE],
        "fixture role",
    )?;
    ensure_exact_requirement_evidence(&slice.required_operations, std::iter::empty(), "operation")?;
    ensure_exact_requirement_evidence(&slice.required_content, std::iter::empty(), "content")?;
    ensure_exact_requirement_evidence(&slice.required_exports, std::iter::empty(), "export")?;

    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let hlg_fixture = resolve_fixture(root, &fixture_root(root), contract, &manifest, HLG_ROLE)?;
    let alpha_fixture = resolve_fixture(
        root,
        &fixture_root(root),
        contract,
        &manifest,
        SRGB_ALPHA_ROLE,
    )?;
    let corpus_revision = manifest.corpus_revision.clone();
    let authoring = authoring::setup_stage(
        workflow,
        &hlg_fixture,
        &alpha_fixture,
        window.end_frame_exclusive,
    )?;
    // Headless Golden execution owns the same explicit decoder residency
    // scope as a production Preview worker. Never defer FFmpeg session
    // teardown to the test thread's TLS destructor.
    let mut decode_context = PreviewDecodeSessionContext::new();
    let picture = picture_validation::execute_picture_stage(
        workflow.app(),
        authoring.hlg_asset_id,
        authoring.alpha_asset_id,
        &mut decode_context,
    )?;
    let export_roundtrip = delivery::execute_export_roundtrip(
        workflow.app_mut(),
        output_directory,
        &picture.program_output_rgba,
        &mut decode_context,
    )?;
    decode_context.clear();

    Ok(GoldenColorMediaReport {
        schema_version: 1,
        profile: "windows-alpha-color-media-roundtrip",
        contract_id: contract.id.clone(),
        corpus_revision,
        status: "pass",
        complete_golden_project: false,
        fixtures: vec![hlg_fixture, alpha_fixture],
        setup: authoring.evidence,
        source_patches: picture.source_patches,
        preview: picture.preview,
        export_roundtrip,
    })
}

#[test]
#[ignore = "requires generated HLG Main10/sRGB Alpha fixtures and production FFmpeg encoders"]
fn golden_color_media_roundtrip_executes_production_interfaces() -> anyhow::Result<()> {
    let root = repository_root();
    let contract = load_golden_contract(&root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let paths = new_run_paths(&root)?;
    let mut workflow = GoldenProductWorkflowDriver::create(
        paths.project.clone(),
        "Windows Alpha Golden Color Media",
        settings,
        mondrian_core::ProjectColorEnvironment::default(),
        mondrian_core::ProjectSettings {
            cache_dir: Some(paths.directory.join("cache")),
            ..mondrian_core::ProjectSettings::default()
        },
    )?;
    let report = execute_color_media_stage(&root, &contract, &mut workflow, &paths.directory)?;
    write_report(&paths.report, &report)?;
    Ok(())
}
