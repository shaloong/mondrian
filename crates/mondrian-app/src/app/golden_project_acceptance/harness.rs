//! Shared Headless adapters for independent Golden execution slices.

use super::fixture::sha256_file;
use crate::app::ui_actions::{export_enqueue_action, TimelineExportRequest};
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_core::{ExecutionTerminalDisposition, JobId, ProjectId, SequenceId};
use mondrian_editor_state::{Action, AuthoringSessionId};
use mondrian_export::preset::{ExportOutputPolicy, ExportPreset, TimelineExportRange};
use mondrian_export::queue::{ExportJobDiagnostics, ExportJobSnapshot, JobStatus};
use serde::{Serialize, Serializer};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

/// Author-state identity before or after one product command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct AuthorCheckpoint {
    pub(super) project_id: ProjectId,
    pub(super) project_path: PathBuf,
    #[serde(serialize_with = "serialize_display")]
    pub(super) session_id: AuthoringSessionId,
    pub(super) author_generation: u64,
    pub(super) active_sequence_id: SequenceId,
    pub(super) sequence_revision: u64,
}

fn serialize_display<T: std::fmt::Display, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// Proof that one product intent committed exactly one author transaction.
#[derive(Debug, Clone, Serialize)]
pub(super) struct AuthorTransitionEvidence {
    pub(super) intent: &'static str,
    pub(super) before: AuthorCheckpoint,
    pub(super) after: AuthorCheckpoint,
}

/// Proof that one Project-scoped product intent committed exactly once.
///
/// Project transactions may change the active Sequence and therefore cannot
/// require one particular Sequence revision to advance.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ProjectAuthorTransitionEvidence {
    pub(super) intent: &'static str,
    pub(super) before: AuthorCheckpoint,
    pub(super) after: AuthorCheckpoint,
}

/// Durable project-boundary evidence shared by every Golden authoring slice.
#[derive(Debug, Clone, Serialize)]
pub(super) struct DurableReopenEvidence {
    pub(super) persistence_request_id: u64,
    pub(super) saved_session: AuthorCheckpoint,
    pub(super) reopened_session: AuthorCheckpoint,
    pub(super) session_identity_changed: bool,
    pub(super) project_identity_preserved: bool,
    pub(super) project_archive_sha256: String,
}

/// Terminal evidence for one production export admitted through the App Interface.
#[derive(Debug, Clone, Serialize)]
pub(super) struct CompletedExportEvidence {
    pub(super) job_id: JobId,
    pub(super) generation: u64,
    pub(super) executed: bool,
    pub(super) terminal_disposition: ExecutionTerminalDisposition,
    pub(super) diagnostics: ExportJobDiagnostics,
    pub(super) output_path: PathBuf,
}

/// Remove one project runtime directory only after its `AppState` has dropped.
#[derive(Debug, Default)]
pub(super) struct DirectoryCleanup {
    path: Option<PathBuf>,
}

impl DirectoryCleanup {
    /// Leave a directory intact when its execution owners did not close.
    pub(super) fn retain(&mut self) {
        self.path = None;
    }

    pub(super) fn track(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }
}

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub(super) fn author_checkpoint(state: &AppState) -> anyhow::Result<AuthorCheckpoint> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    Ok(AuthorCheckpoint {
        project_id: state.project_id().context("Project identity is absent")?,
        project_path: state.current_project_path().context("Project path is absent")?.to_path_buf(),
        session_id: state.authoring_session_id().context("Authoring Session is absent")?,
        author_generation: state.project_author_generation(),
        active_sequence_id: sequence.id,
        sequence_revision: sequence.revision.get(),
    })
}

fn ensure_same_project(before: &AuthorCheckpoint, after: &AuthorCheckpoint) -> anyhow::Result<()> {
    ensure!(
        before.project_id == after.project_id,
        "author transaction changed Project identity"
    );
    ensure!(
        before.project_path == after.project_path,
        "author transaction changed Project path"
    );
    Ok(())
}

/// Execute one production authoring Interface and enforce the transaction boundary.
pub(super) fn author_transition<T>(
    state: &mut AppState,
    intent: &'static str,
    edit: impl FnOnce(&mut AppState) -> anyhow::Result<T>,
) -> anyhow::Result<(T, AuthorTransitionEvidence)> {
    let before = author_checkpoint(state)?;
    let output = edit(state)?;
    let after = author_checkpoint(state)?;
    ensure_same_project(&before, &after)?;
    ensure!(
        before.session_id == after.session_id,
        "{intent} replaced the Authoring Session"
    );
    ensure!(
        before.author_generation.checked_add(1) == Some(after.author_generation),
        "{intent} did not advance Author Generation exactly once: before {}, after {}",
        before.author_generation,
        after.author_generation
    );
    ensure!(
        before.active_sequence_id == after.active_sequence_id,
        "{intent} changed the active Sequence in a Sequence-scoped transaction"
    );
    ensure!(
        before.sequence_revision.checked_add(1) == Some(after.sequence_revision),
        "{intent} did not advance Sequence Author Revision exactly once: before {}, after {}",
        before.sequence_revision,
        after.sequence_revision
    );
    Ok((output, AuthorTransitionEvidence { intent, before, after }))
}

pub(super) fn dispatch_author_transition(
    state: &mut AppState,
    intent: &'static str,
    action: Action,
) -> anyhow::Result<AuthorTransitionEvidence> {
    author_transition(state, intent, |state| {
        state.dispatch_action(action)?;
        Ok(())
    })
    .map(|(_, evidence)| evidence)
}

/// Execute one Project-scoped authoring Interface and enforce one transaction.
pub(super) fn project_author_transition<T>(
    state: &mut AppState,
    intent: &'static str,
    edit: impl FnOnce(&mut AppState) -> anyhow::Result<T>,
) -> anyhow::Result<(T, ProjectAuthorTransitionEvidence)> {
    let before = author_checkpoint(state)?;
    let output = edit(state)?;
    let after = author_checkpoint(state)?;
    ensure_same_project(&before, &after)?;
    ensure!(
        before.session_id == after.session_id,
        "{intent} replaced the Authoring Session"
    );
    ensure!(
        before.author_generation.checked_add(1) == Some(after.author_generation),
        "{intent} did not advance Author Generation exactly once"
    );
    Ok((
        output,
        ProjectAuthorTransitionEvidence { intent, before, after },
    ))
}

/// Save, close, and reopen one project through the production persistence boundary.
pub(super) fn durable_save_reopen(
    state: &mut AppState,
    project_path: &Path,
) -> anyhow::Result<DurableReopenEvidence> {
    let persistence_request_id = state.request_project_save()?;
    state.wait_for_persistence_request(persistence_request_id)?;
    ensure!(
        !state.has_unsaved_project_changes(),
        "durable save completion did not cover current author and Asset Library state"
    );
    let saved_session = author_checkpoint(state)?;
    let project_archive_sha256 = sha256_file(project_path)?;
    state.close_project()?;
    ensure!(
        state.authoring_session_id().is_none(),
        "close retained the saved Authoring Session"
    );
    state.dispatch_action(Action::OpenProject(project_path.to_path_buf()))?;
    let reopened_session = author_checkpoint(state)?;
    ensure!(
        saved_session.session_id != reopened_session.session_id,
        "save/reopen reused an Authoring Session identity"
    );
    ensure!(
        reopened_session.author_generation == 1,
        "freshly reopened Authoring Session did not begin at generation one"
    );
    ensure_same_project(&saved_session, &reopened_session)?;
    ensure!(
        reopened_session.active_sequence_id == saved_session.active_sequence_id,
        "save/reopen changed the active Sequence identity"
    );
    ensure!(
        reopened_session.sequence_revision == saved_session.sequence_revision,
        "save/reopen changed persisted Sequence Author Revision"
    );
    Ok(DurableReopenEvidence {
        persistence_request_id: persistence_request_id.get(),
        saved_session,
        reopened_session,
        session_identity_changed: true,
        project_identity_preserved: true,
        project_archive_sha256,
    })
}

/// Require one slice report to cover exactly, and only, its declared obligations.
pub(super) fn ensure_exact_requirement_evidence<'a>(
    required: &[String],
    evidence_ids: impl IntoIterator<Item = &'a str>,
    kind: &str,
) -> anyhow::Result<()> {
    let required = required.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let observed = evidence_ids.into_iter().collect::<BTreeSet<_>>();
    ensure!(
        required == observed,
        "{kind} evidence does not exactly cover its slice contract"
    );
    Ok(())
}

pub(super) fn rooted_env_path(
    repository_root: &Path,
    variable: &str,
    default: impl FnOnce() -> PathBuf,
) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repository_root.join(path)
            }
        })
        .unwrap_or_else(default)
}

pub(super) fn fixture_root(repository_root: &Path) -> PathBuf {
    rooted_env_path(repository_root, "MONDRIAN_GOLDEN_FIXTURE_ROOT", || {
        repository_root.join("tests/fixtures")
    })
}

pub(super) fn new_run_directory(
    repository_root: &Path,
    root_env: &str,
    prefix: &str,
) -> anyhow::Result<PathBuf> {
    let run_root = rooted_env_path(repository_root, root_env, || {
        repository_root.join("target").join("validation").join("runs")
    });
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let directory = run_root.join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create Golden run directory {}", directory.display()))?;
    Ok(directory)
}

pub(super) fn wait_for_media_imports(state: &mut AppState) -> anyhow::Result<()> {
    wait_for_media_imports_until(state, Instant::now() + IMPORT_TIMEOUT)
}

pub(super) fn wait_for_media_imports_until(
    state: &mut AppState,
    deadline: Instant,
) -> anyhow::Result<()> {
    ensure!(
        Instant::now() < deadline,
        "media import deadline elapsed before waiting"
    );
    while state.pending_media_import_batches() > 0 {
        state.poll_media_imports();
        ensure!(
            Instant::now() < deadline,
            "media import completed after its original deadline"
        );
        if state.pending_media_import_batches() == 0 {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for media import"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

pub(super) fn wait_for_export_job(
    state: &mut AppState,
    job_id: JobId,
    timeout: Duration,
) -> anyhow::Result<ExportJobSnapshot> {
    wait_for_export_job_until(state, job_id, Instant::now() + timeout)
}

pub(super) fn wait_for_export_job_until(
    state: &mut AppState,
    job_id: JobId,
    deadline: Instant,
) -> anyhow::Result<ExportJobSnapshot> {
    loop {
        ensure!(
            Instant::now() < deadline,
            "export deadline elapsed before polling job {job_id}"
        );
        state.poll_export_queue();
        let snapshot = state
            .export_jobs_snapshot()
            .into_iter()
            .find(|snapshot| snapshot.id == job_id)
            .with_context(|| format!("export job disappeared: {job_id}"))?;
        ensure!(
            Instant::now() < deadline,
            "export job {job_id} completed after its original deadline"
        );
        if snapshot.status.is_terminal() {
            return Ok(snapshot);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for export job {job_id}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Admit one export through the same product action used by the Window and
/// require a completed worker execution with a durable output artifact.
pub(super) fn execute_export_job(
    state: &mut AppState,
    preset: ExportPreset,
    sequence_id: Option<SequenceId>,
    range: TimelineExportRange,
    output_path: PathBuf,
    timeout: Duration,
) -> anyhow::Result<CompletedExportEvidence> {
    let before = state
        .export_jobs_snapshot()
        .into_iter()
        .map(|snapshot| snapshot.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(export_enqueue_action(TimelineExportRequest {
        preset,
        sequence_id,
        range,
        output_path: output_path.clone(),
        output_policy: ExportOutputPolicy::CreateNew,
        broadcast_qc: None,
        regulatory_pse: None,
        frozen_ancillary: None,
    }))?;
    let created = state
        .export_jobs_snapshot()
        .into_iter()
        .filter(|snapshot| !before.contains(&snapshot.id))
        .map(|snapshot| snapshot.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "export action admitted {} jobs instead of one",
        created.len()
    );
    let job_id = created[0];
    let snapshot = wait_for_export_job(state, job_id, timeout)?;
    ensure!(
        matches!(snapshot.status, JobStatus::Completed),
        "export ended as {:?}",
        snapshot.status
    );
    ensure!(
        snapshot.executed,
        "export never crossed the worker boundary"
    );
    let terminal = snapshot
        .terminal_evidence
        .context("completed export has no terminal evidence")?;
    ensure!(
        terminal.generation == snapshot.generation
            && terminal.disposition == ExecutionTerminalDisposition::Completed,
        "export terminal evidence disagrees with completed queue state"
    );
    let output_path =
        mondrian_assets::canonical_asset_file_path(&output_path).with_context(|| {
            format!(
                "completed export is not present at {}",
                output_path.display()
            )
        })?;
    Ok(CompletedExportEvidence {
        job_id,
        generation: snapshot.generation,
        executed: snapshot.executed,
        terminal_disposition: terminal.disposition,
        diagnostics: snapshot.diagnostics,
        output_path,
    })
}

pub(super) fn write_report<T: Serialize>(path: &Path, report: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report)?;
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}
