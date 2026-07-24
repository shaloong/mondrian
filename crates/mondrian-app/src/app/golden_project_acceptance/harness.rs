//! Shared Headless adapters for independent Golden execution slices.

use super::fixture::sha256_file;
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_core::JobId;
use mondrian_editor_state::Action;
use mondrian_export::queue::ExportJobSnapshot;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

/// Author-state identity before or after one product command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct AuthorCheckpoint {
    pub(super) session_id: String,
    pub(super) author_generation: u64,
    pub(super) sequence_revision: u64,
}

/// Proof that one product intent committed exactly one author transaction.
#[derive(Debug, Clone, Serialize)]
pub(super) struct AuthorTransitionEvidence {
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
    pub(super) project_archive_sha256: String,
}

/// Remove one project runtime directory only after its `AppState` has dropped.
#[derive(Debug, Default)]
pub(super) struct DirectoryCleanup {
    path: Option<PathBuf>,
}

impl DirectoryCleanup {
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
    Ok(AuthorCheckpoint {
        session_id: state
            .authoring_session_id()
            .context("Authoring Session is absent")?
            .to_string(),
        author_generation: state.project_author_generation(),
        sequence_revision: state
            .active_sequence()
            .context("active Sequence is absent")?
            .revision
            .get(),
    })
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
    ensure!(
        before.session_id == after.session_id,
        "{intent} replaced the Authoring Session"
    );
    ensure!(
        before.author_generation.checked_add(1) == Some(after.author_generation),
        "{intent} did not advance Author Generation exactly once"
    );
    ensure!(
        before.sequence_revision.checked_add(1) == Some(after.sequence_revision),
        "{intent} did not advance Sequence Author Revision exactly once"
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
    state.close_project();
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
    ensure!(
        reopened_session.sequence_revision == saved_session.sequence_revision,
        "save/reopen changed persisted Sequence Author Revision"
    );
    Ok(DurableReopenEvidence {
        persistence_request_id: persistence_request_id.get(),
        saved_session,
        reopened_session,
        session_identity_changed: true,
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
    let deadline = Instant::now() + IMPORT_TIMEOUT;
    while state.pending_media_import_batches() > 0 {
        state.poll_media_imports();
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
    let deadline = Instant::now() + timeout;
    loop {
        state.poll_export_queue();
        let snapshot = state
            .export_jobs_snapshot()
            .into_iter()
            .find(|snapshot| snapshot.id == job_id)
            .with_context(|| format!("export job disappeared: {job_id}"))?;
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

pub(super) fn write_report<T: Serialize>(path: &Path, report: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report)?;
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}
