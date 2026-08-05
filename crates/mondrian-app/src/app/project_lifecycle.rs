use super::project_library_generation::{
    collect_retired_project_libraries, protected_project_library_paths,
    retained_project_runtime_lease, sweep_orphaned_project_libraries,
    ProjectLibraryGenerationCandidate, RetiredProjectLibraryGeneration,
};
use super::project_persistence::{
    ProjectPersistenceCompletion, ProjectPersistencePauseTicket, ProjectPersistencePauseToken,
    ProjectPersistenceRequestId,
};
#[cfg(test)]
use super::project_recovery::recovery_manifest_path;
use super::project_recovery::{
    cleanup_recovery_runtime_artifacts, copy_recovery_selection_under_lease,
    preflight_recovery_selection, reconcile_recovery_after_manual_save,
};
#[cfg(test)]
use super::project_runtime::project_runtime_roots_for_path_for_test;
use super::project_runtime::{
    claim_project_runtime, claim_project_runtime_sharing_logical_authority,
    lease_existing_project_runtime, lease_existing_project_runtime_sharing_logical_authority,
    project_runtime_root_for_project, ProjectRuntimeLease,
};
use super::*;
use anyhow::Context;
#[cfg(test)]
use mondrian_project::load_project_archive;
use mondrian_project::{
    PreparedProjectArchive, ProjectArchivePublication, ProjectArchiveReadBudget, ProjectDocument,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum PersistenceCompletionDisposition {
    Applied,
    AppliedWithRecoveryWarning { reason: String },
    SatisfiedByNewerPublication,
    IgnoredSupersededDestination,
    IgnoredStaleSession,
}

/// App-owned pending close operation over one exact persistence generation.
pub(super) struct PendingProjectClose {
    session_id: AuthoringSessionId,
    ticket: ProjectPersistencePauseTicket,
    required_manual_save: Option<ProjectPersistenceRequestId>,
}

/// Retained fail-closed authority after Project Session Handoff failure.
pub(super) struct ProjectCloseFault {
    session_id: AuthoringSessionId,
    reason: String,
}

/// Result of one bounded Project-close lifecycle poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectClosePoll {
    /// No asynchronous close operation exists.
    Inactive,
    /// Earlier persistence work still owns the FIFO barrier.
    Pending,
    /// The exact Session was quiesced, retired, and removed from AppState.
    Closed,
    /// A required manual save was not durable, so admission was resumed and
    /// the still-open Project may be corrected or retried.
    SaveRejected(String),
    /// Ownership could not be proven; author state is retained fail-closed.
    Faulted(String),
}

fn autosave_archive_leaf(saved_at_unix_ms: u64, generation: u64) -> String {
    format!(
        "project-{saved_at_unix_ms}-g{}-{}.autosave.mdp",
        generation,
        uuid::Uuid::new_v4()
    )
}

impl AppState {
    #[cfg(test)]
    pub(super) fn autosave_manifest_path(runtime_root: &Path) -> PathBuf {
        recovery_manifest_path(runtime_root)
    }

    fn claim_or_reuse_project_runtime(
        &mut self,
        project_file: &Path,
        project_id: ProjectId,
    ) -> Result<Arc<ProjectRuntimeLease>, String> {
        if let Some(lease) = self
            .project_runtime_lease
            .as_ref()
            .filter(|lease| lease.project_id() == project_id)
        {
            if lease.allocation_target_matches(project_file)? {
                lease.retain_publication_target(project_file)?;
                return Ok(Arc::clone(lease));
            }
        }
        let expected_runtime_root = project_runtime_root_for_project(project_file, project_id)?;
        let shared_logical_authority = self
            .project_runtime_lease
            .as_ref()
            .filter(|lease| lease.project_id() == project_id)
            .map(Arc::clone);
        collect_retired_project_libraries(&mut self.retired_project_libraries);
        if let Some(lease) = retained_project_runtime_lease(
            &self.retired_project_libraries,
            project_id,
            Some(&expected_runtime_root),
        ) {
            lease.validate()?;
            lease.retain_publication_target(project_file)?;
            return Ok(lease);
        }
        let shared_logical_authority = shared_logical_authority.or_else(|| {
            retained_project_runtime_lease(&self.retired_project_libraries, project_id, None)
        });
        match shared_logical_authority {
            Some(authority) => claim_project_runtime_sharing_logical_authority(
                project_file,
                project_id,
                &authority,
            ),
            None => claim_project_runtime(project_file, project_id),
        }
    }

    fn lease_or_reuse_existing_runtime(
        &mut self,
        runtime_root: &Path,
        project_id: ProjectId,
        publication_target: &Path,
    ) -> Result<Arc<ProjectRuntimeLease>, String> {
        if let Some(lease) = self.project_runtime_lease.as_ref().filter(|lease| {
            lease.runtime_root() == runtime_root && lease.project_id() == project_id
        }) {
            lease.validate()?;
            lease.retain_publication_target(publication_target)?;
            return Ok(Arc::clone(lease));
        }
        collect_retired_project_libraries(&mut self.retired_project_libraries);
        if let Some(lease) = retained_project_runtime_lease(
            &self.retired_project_libraries,
            project_id,
            Some(runtime_root),
        ) {
            lease.validate()?;
            lease.retain_publication_target(publication_target)?;
            return Ok(lease);
        }
        let shared_logical_authority = self
            .project_runtime_lease
            .as_ref()
            .filter(|lease| lease.project_id() == project_id)
            .map(Arc::clone)
            .or_else(|| {
                retained_project_runtime_lease(&self.retired_project_libraries, project_id, None)
            });
        match shared_logical_authority {
            Some(authority) => lease_existing_project_runtime_sharing_logical_authority(
                runtime_root,
                project_id,
                publication_target,
                &authority,
            ),
            None => lease_existing_project_runtime(runtime_root, project_id, publication_target),
        }
    }

    fn ensure_open_project_runtime_lease(&mut self) -> anyhow::Result<Arc<ProjectRuntimeLease>> {
        let (runtime_root, project_id, project_file) = {
            let session =
                self.authoring.as_ref().ok_or_else(|| anyhow::anyhow!("当前没有打开的项目"))?;
            (
                session.runtime_root().to_path_buf(),
                session.project_id(),
                session.project_file().to_path_buf(),
            )
        };
        let lease = self
            .lease_or_reuse_existing_runtime(&runtime_root, project_id, &project_file)
            .map_err(anyhow::Error::msg)?;
        self.project_runtime_lease = Some(Arc::clone(&lease));
        Ok(lease)
    }

    fn prepare_project_library_generation(
        &mut self,
        runtime_lease: Arc<ProjectRuntimeLease>,
    ) -> anyhow::Result<ProjectLibraryGenerationCandidate> {
        collect_retired_project_libraries(&mut self.retired_project_libraries);
        let protected = protected_project_library_paths(
            self.authoring.as_ref().map(AuthoringSession::asset_library),
            runtime_lease.runtime_root(),
            &self.retired_project_libraries,
        );
        sweep_orphaned_project_libraries(&runtime_lease, protected).map_err(anyhow::Error::msg)?;
        ProjectLibraryGenerationCandidate::create(runtime_lease).map_err(anyhow::Error::msg)
    }

    fn retain_current_project_library_generation(&mut self) {
        let Some(session) = self.authoring.as_ref() else {
            return;
        };
        let Some(runtime_lease) = self.project_runtime_lease.as_ref() else {
            return;
        };
        if runtime_lease.runtime_root() != session.runtime_root()
            || runtime_lease.project_id() != session.project_id()
        {
            return;
        }
        if let Some(retired) = RetiredProjectLibraryGeneration::capture(
            Arc::clone(runtime_lease),
            session.asset_library(),
        ) {
            self.retired_project_libraries.push(retired);
        }
    }

    fn retire_uninstalled_project_session(
        &mut self,
        session: AuthoringSession,
        runtime_lease: Arc<ProjectRuntimeLease>,
    ) -> Result<(), String> {
        let session_id = session.session_id();
        let admission_result = self
            .project_persistence
            .pause_and_quiesce(session_id)
            .and_then(|token| self.project_persistence.retire(token));
        let retired =
            RetiredProjectLibraryGeneration::capture(runtime_lease, session.asset_library());
        drop(session);
        if let Some(retired) = retired {
            self.retired_project_libraries.push(retired);
        }
        collect_retired_project_libraries(&mut self.retired_project_libraries);
        admission_result
    }

    fn discard_prepared_project_session_after_error(
        &mut self,
        session: AuthoringSession,
        runtime_lease: Arc<ProjectRuntimeLease>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        match self.retire_uninstalled_project_session(session, runtime_lease) {
            Ok(()) => error,
            Err(cleanup_error) => anyhow::anyhow!(
                "{error:#}; additionally failed to retire the prepared Project persistence generation: {cleanup_error}"
            ),
        }
    }

    fn begin_project_session_handoff(
        &mut self,
    ) -> anyhow::Result<Option<ProjectPersistencePauseToken>> {
        let Some(session_id) = self.authoring.as_ref().map(AuthoringSession::session_id) else {
            return Ok(None);
        };
        let token = self
            .project_persistence
            .pause_and_quiesce(session_id)
            .map_err(anyhow::Error::msg)?;
        self.drain_quiesced_project_persistence_completions();
        Ok(Some(token))
    }

    fn drain_quiesced_project_persistence_completions(&mut self) {
        loop {
            let completions = self.project_persistence.poll_completions();
            if completions.is_empty() {
                break;
            }
            for completion in completions {
                if let Err(error) = self.apply_persistence_completion(completion) {
                    self.set_status_hint(format!("项目持久化完成失败：{error}"), true);
                }
            }
        }
    }

    fn resume_project_session_handoff(
        &mut self,
        handoff: &mut Option<ProjectPersistencePauseToken>,
    ) -> Result<(), String> {
        let Some(token) = handoff.take() else {
            return Ok(());
        };
        self.project_persistence.resume(token)
    }

    fn retire_project_session_handoff(
        &mut self,
        handoff: &mut Option<ProjectPersistencePauseToken>,
    ) -> Result<(), String> {
        let Some(token) = handoff.as_ref().copied() else {
            return Ok(());
        };
        self.project_persistence.retire(token)?;
        *handoff = None;
        Ok(())
    }

    fn resume_handoff_after_error(
        &mut self,
        mut handoff: Option<ProjectPersistencePauseToken>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        match self.resume_project_session_handoff(&mut handoff) {
            Ok(()) => error,
            Err(resume_error) => anyhow::anyhow!(
                "{error:#}; additionally failed to resume the previous Project persistence generation: {resume_error}"
            ),
        }
    }

    pub(super) fn prepare_project_session_close(&mut self) -> anyhow::Result<()> {
        let mut handoff = self.begin_project_session_handoff()?;
        self.retire_project_session_handoff(&mut handoff).map_err(anyhow::Error::msg)?;
        self.retain_current_project_library_generation();
        Ok(())
    }

    /// Start closing the current Project without waiting for durable work on
    /// the caller's thread.
    ///
    /// `Ok(false)` means no Project was open. Once admitted, the exact
    /// Authoring Session remains installed only as a read-only projection
    /// source until [`Self::poll_project_close`] observes the FIFO barrier.
    pub fn begin_project_close(&mut self) -> anyhow::Result<bool> {
        self.begin_project_close_with_requirement(None)
    }

    /// Start Project close and require one exact already-admitted manual save
    /// to become the applied durable baseline before the Session may retire.
    pub(crate) fn begin_project_close_after_save(
        &mut self,
        request_id: ProjectPersistenceRequestId,
    ) -> anyhow::Result<bool> {
        self.begin_project_close_with_requirement(Some(request_id))
    }

    fn begin_project_close_with_requirement(
        &mut self,
        required_manual_save: Option<ProjectPersistenceRequestId>,
    ) -> anyhow::Result<bool> {
        if let Some(fault) = &self.project_close_fault {
            anyhow::bail!(
                "Project lifecycle is fail-closed after an earlier persistence handoff failure: {}",
                fault.reason
            );
        }
        if self.pending_project_close.is_some() {
            anyhow::bail!("Project close is already waiting for persistence quiescence");
        }
        let Some(session_id) = self.authoring.as_ref().map(AuthoringSession::session_id) else {
            return Ok(false);
        };
        self.stop().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let ticket = self
            .project_persistence
            .begin_pause_and_quiesce(session_id)
            .map_err(anyhow::Error::msg)?;
        self.pending_project_close =
            Some(PendingProjectClose { session_id, ticket, required_manual_save });
        self.set_status_hint("正在完成后台保存并安全关闭项目…", false);
        Ok(true)
    }

    /// Whether the current Project is frozen behind an asynchronous close
    /// handoff or a fail-closed ownership fault.
    pub fn project_close_blocks_actions(&self) -> bool {
        self.pending_project_close.is_some() || self.project_close_fault.is_some()
    }

    /// Whether a failed handoff awaits an explicit user-authorized detach.
    pub(crate) fn has_project_close_fault(&self) -> bool {
        self.project_close_fault.is_some()
    }

    /// Abandon a failed persistence handoff without claiming quiescence.
    ///
    /// The worker retains its own payload/lease ownership. Retiring admission
    /// makes every eventual completion stale before author state is removed.
    pub(crate) fn force_close_project_after_fault(&mut self) -> anyhow::Result<()> {
        let fault = self
            .project_close_fault
            .take()
            .ok_or_else(|| anyhow::anyhow!("Project close has no retained lifecycle fault"))?;
        if self.authoring.as_ref().map(AuthoringSession::session_id) != Some(fault.session_id) {
            self.project_close_fault = Some(fault);
            anyhow::bail!("failed Project close authority belongs to another Authoring Session");
        }
        if let Err(reason) = self.project_persistence.abandon_session(fault.session_id) {
            self.project_close_fault = Some(fault);
            anyhow::bail!(reason);
        }
        self.retain_current_project_library_generation();
        self.finalize_project_close_state();
        Ok(())
    }

    fn retain_project_close_fault(&mut self, session_id: AuthoringSessionId, reason: String) {
        self.project_close_fault = Some(ProjectCloseFault { session_id, reason });
    }

    /// Poll the asynchronous Project-close handoff once without blocking.
    pub(crate) fn poll_project_close(&mut self) -> ProjectClosePoll {
        let Some(pending) = self.pending_project_close.take() else {
            return ProjectClosePoll::Inactive;
        };
        if self.authoring.as_ref().map(AuthoringSession::session_id) != Some(pending.session_id) {
            self.project_persistence.poison_pause_ticket(&pending.ticket);
            let reason =
                "active Authoring Session changed while Project close was quiescing".to_owned();
            self.retain_project_close_fault(pending.session_id, reason.clone());
            self.set_status_hint(format!("项目关闭失败：{reason}"), true);
            return ProjectClosePoll::Faulted(reason);
        }

        match self.project_persistence.poll_pause_and_quiesce(&pending.ticket) {
            Ok(None) => {
                self.pending_project_close = Some(pending);
                ProjectClosePoll::Pending
            }
            Ok(Some(token)) => {
                // The barrier proves all earlier requests have destroyed their
                // heavy payloads and queued any scalar completion. Apply those
                // completions while the exact Session generation is still the
                // current authority, then retire it permanently.
                self.drain_quiesced_project_persistence_completions();
                if let Some(required_request) = pending.required_manual_save {
                    let save_is_durable = self
                        .manual_project_file_applied_request
                        .as_ref()
                        .is_some_and(|(_, applied_request)| *applied_request == required_request);
                    if !save_is_durable {
                        let reason = format!(
                            "required manual save request {} did not establish the durable baseline; Project remains open",
                            required_request.get()
                        );
                        if let Err(resume_error) = self.project_persistence.resume(token) {
                            let combined = format!(
                                "{reason}; additionally failed to resume persistence admission: {resume_error}"
                            );
                            self.retain_project_close_fault(pending.session_id, combined.clone());
                            self.set_status_hint(format!("项目关闭失败：{combined}"), true);
                            return ProjectClosePoll::Faulted(combined);
                        }
                        self.set_status_hint(format!("保存后关闭已取消：{reason}"), true);
                        return ProjectClosePoll::SaveRejected(reason);
                    }
                }
                if let Err(reason) = self.project_persistence.retire(token) {
                    self.retain_project_close_fault(pending.session_id, reason.clone());
                    self.set_status_hint(format!("项目关闭失败：{reason}"), true);
                    return ProjectClosePoll::Faulted(reason);
                }
                self.retain_current_project_library_generation();
                self.finalize_project_close_state();
                ProjectClosePoll::Closed
            }
            Err(reason) => {
                self.retain_project_close_fault(pending.session_id, reason.clone());
                self.set_status_hint(format!("项目关闭失败：{reason}"), true);
                ProjectClosePoll::Faulted(reason)
            }
        }
    }

    pub(super) fn collect_released_project_libraries(&mut self) {
        collect_retired_project_libraries(&mut self.retired_project_libraries);
    }

    #[cfg(test)]
    pub(crate) fn test_poison_project_persistence_admission(&mut self) {
        let session_id = self
            .authoring
            .as_ref()
            .expect("test requires an open Authoring Session")
            .session_id();
        self.project_persistence.poison_session_admission_for_test(session_id);
    }

    pub fn open_project_from_autosave_snapshot(
        &mut self,
        candidate: CrashRecoveryCandidate,
    ) -> anyhow::Result<()> {
        let mut handoff = self.begin_project_session_handoff()?;
        let result = self.open_project_from_autosave_snapshot_in_handoff(candidate, &mut handoff);
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.resume_handoff_after_error(handoff, error)),
        }
    }

    fn open_project_from_autosave_snapshot_in_handoff(
        &mut self,
        candidate: CrashRecoveryCandidate,
        handoff: &mut Option<ProjectPersistencePauseToken>,
    ) -> anyhow::Result<()> {
        if !candidate.autosave_file.exists() {
            anyhow::bail!("未找到自动保存文件：{}", candidate.autosave_file.display());
        }
        let selection = preflight_recovery_selection(&candidate).map_err(anyhow::Error::msg)?;
        let runtime_lease = self
            .lease_or_reuse_existing_runtime(
                &selection.runtime_root,
                selection.project_id,
                &candidate.project_file,
            )
            .map_err(anyhow::Error::msg)?;
        let cleanup =
            cleanup_recovery_runtime_artifacts(&runtime_lease).map_err(anyhow::Error::msg)?;
        if cleanup.removed_staging_count > 0 || cleanup.removed_unreferenced_snapshot_count > 0 {
            tracing::info!(
                runtime_root = %runtime_lease.runtime_root().display(),
                removed_staging = cleanup.removed_staging_count,
                removed_unreferenced_snapshots = cleanup.removed_unreferenced_snapshot_count,
                "removed abandoned Project recovery artifacts"
            );
        }

        let staged = runtime_lease.runtime_root().join(format!(
            ".recovery-open-{}.staging.mdp",
            uuid::Uuid::new_v4()
        ));
        let mut verified_archive =
            copy_recovery_selection_under_lease(&selection, &runtime_lease, &staged)
                .map_err(anyhow::Error::msg)?;
        let prepared = PreparedProjectArchive::from_open_file(
            verified_archive.file_mut().map_err(anyhow::Error::msg)?,
            ProjectArchiveReadBudget::default(),
        )?;
        self.open_prepared_project_archive_in_runtime(
            candidate.project_file,
            prepared,
            false,
            runtime_lease,
            handoff,
        )?;

        // A recovered snapshot is intentionally dirty until the user explicitly
        // saves it. Keep all recovery points until that durable save succeeds.
        Ok(())
    }

    pub fn has_open_project(&self) -> bool {
        self.authoring.is_some()
    }

    /// Whether the open authoring Session has been durably published at least once.
    pub fn has_saved_project(&self) -> bool {
        self.authoring.as_ref().is_some_and(AuthoringSession::has_durable_baseline)
    }

    pub(crate) fn has_unsaved_project_changes(&self) -> bool {
        self.authoring.as_ref().is_some_and(AuthoringSession::is_dirty)
    }

    fn open_project_archive(
        &mut self,
        project_file: PathBuf,
        archive_file: &Path,
        handoff: &mut Option<ProjectPersistencePauseToken>,
    ) -> anyhow::Result<()> {
        let mut archive_handle = fs::File::open(archive_file)?;
        let prepared = PreparedProjectArchive::from_open_file(
            &mut archive_handle,
            ProjectArchiveReadBudget::default(),
        )?;
        if self.active_sequence().is_some() {
            self.stop().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        let runtime_lease = self
            .claim_or_reuse_project_runtime(&project_file, prepared.project_id())
            .map_err(anyhow::Error::msg)?;
        let cleanup =
            cleanup_recovery_runtime_artifacts(&runtime_lease).map_err(anyhow::Error::msg)?;
        if cleanup.removed_staging_count > 0 || cleanup.removed_unreferenced_snapshot_count > 0 {
            tracing::info!(
                runtime_root = %runtime_lease.runtime_root().display(),
                removed_staging = cleanup.removed_staging_count,
                removed_unreferenced_snapshots = cleanup.removed_unreferenced_snapshot_count,
                "removed abandoned Project recovery artifacts"
            );
        }
        self.open_prepared_project_archive_in_runtime(
            project_file,
            prepared,
            true,
            runtime_lease,
            handoff,
        )
    }

    fn open_prepared_project_archive_in_runtime(
        &mut self,
        project_file: PathBuf,
        prepared: PreparedProjectArchive<'_>,
        opens_canonical_project: bool,
        runtime_lease: Arc<ProjectRuntimeLease>,
        handoff: &mut Option<ProjectPersistencePauseToken>,
    ) -> anyhow::Result<()> {
        let prepared_project_id = prepared.project_id();
        if runtime_lease.project_id() != prepared_project_id {
            anyhow::bail!("Project runtime lease belongs to another Project");
        }
        runtime_lease.validate().map_err(anyhow::Error::msg)?;
        let runtime_root = runtime_lease.runtime_root().to_path_buf();
        let mut library_generation =
            self.prepare_project_library_generation(Arc::clone(&runtime_lease))?;
        let loaded = prepared.load_into(library_generation.root())?;
        if loaded.document.project_id != prepared_project_id {
            anyhow::bail!("项目归属在打开期间发生变化，拒绝安装新的素材库代际与项目会话");
        }
        if loaded.library_schema_version > mondrian_assets::ASSET_LIBRARY_SCHEMA_VERSION {
            anyhow::bail!(
                "项目素材库 schema v{} 高于当前支持的 v{}",
                loaded.library_schema_version,
                mondrian_assets::ASSET_LIBRARY_SCHEMA_VERSION
            );
        }
        let asset_library = library_generation.open()?;
        let session = if opens_canonical_project {
            AuthoringSession::open_saved(
                loaded.document,
                project_file.clone(),
                runtime_root.clone(),
                asset_library,
            )
        } else {
            AuthoringSession::new_unsaved(
                loaded.document,
                project_file.clone(),
                runtime_root.clone(),
                asset_library,
            )
        }
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let project_id = session.project_id();
        self.retire_project_session_handoff(handoff).map_err(anyhow::Error::msg)?;
        self.retain_current_project_library_generation();
        self.clear_timeline_targeting();
        // Commit filesystem ownership before the live Arc escapes into App
        // state. A panic after this point can leave only an owner-recognized
        // orphan for the next sweep; it can never delete an installed SQLite
        // directory out from under a live Session.
        library_generation.commit();
        self.authoring = Some(session);
        self.audio_monitoring.reset();
        self.synchronize_audio_idle_warmup_binding();
        self.project_runtime_lease = Some(runtime_lease);
        self.manual_project_file_destination = None;
        self.manual_project_file_applied_request = None;
        self.autosave_in_flight_request = None;
        self.proxy_generation.bind_project(Some(project_id));
        self.media_import.bind_project(Some(project_id));
        self.media_import_batches.clear();
        self.media_asset_mutations.bind_project(Some(project_id));
        self.settle_preview_access_source();
        self.dragging_asset = None;
        self.ensure_minimum_tracks();
        Ok(())
    }

    pub fn open_project_file(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        let mut handoff = self.begin_project_session_handoff()?;
        let result =
            self.open_project_archive(project_file.clone(), project_file.as_path(), &mut handoff);
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.resume_handoff_after_error(handoff, error)),
        }
    }

    /// Enqueue a manual save and return without waiting for filesystem I/O.
    pub fn request_project_save(&mut self) -> anyhow::Result<ProjectPersistenceRequestId> {
        self.submit_project_save(None)
    }

    /// Enqueue Save As and return without waiting for filesystem I/O.
    ///
    /// The supplied path is a user-confirmed file-dialog or Headless target.
    /// An absent entry retains create-only intent through final publication;
    /// an entry that already existed when the user confirmed the target may be
    /// replaced. A later race can therefore never turn a new-target choice
    /// into an implicit overwrite.
    pub fn request_project_save_as(
        &mut self,
        target_file: PathBuf,
    ) -> anyhow::Result<ProjectPersistenceRequestId> {
        let target_file = super::ensure_project_extension(target_file);
        let publication = match fs::symlink_metadata(&target_file) {
            Ok(_) => ProjectArchivePublication::ReplaceExisting,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ProjectArchivePublication::CreateNew
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法检查另存为目标：{}", target_file.display()));
            }
        };
        self.submit_project_save(Some((target_file, publication)))
    }

    fn submit_project_save(
        &mut self,
        target: Option<(PathBuf, ProjectArchivePublication)>,
    ) -> anyhow::Result<ProjectPersistenceRequestId> {
        let runtime_lease = self.ensure_open_project_runtime_lease()?;
        let session =
            self.authoring.as_ref().ok_or_else(|| anyhow::anyhow!("当前没有打开的项目"))?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let current_project_file = session.project_file().to_path_buf();
        let destination = match self
            .manual_project_file_destination
            .as_ref()
            .filter(|destination| destination.session_id() == snapshot.session_id)
        {
            Some(current) => match target {
                Some((target_file, publication)) => current.retarget(target_file, publication),
                None => Ok(current.clone()),
            },
            None => match target {
                Some((target_file, ProjectArchivePublication::CreateNew)) => {
                    ManualProjectFileDestination::initial_create(snapshot.session_id, target_file)
                }
                Some((target_file, ProjectArchivePublication::ReplaceExisting)) => {
                    ManualProjectFileDestination::initial(snapshot.session_id, target_file)
                }
                None => {
                    ManualProjectFileDestination::initial(snapshot.session_id, current_project_file)
                }
            },
        }
        .map_err(anyhow::Error::msg)?;
        let request_id = self
            .project_persistence
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual { destination: destination.clone() },
                runtime_lease,
            )
            .map_err(anyhow::Error::msg)?;
        // The destination becomes current only after the exact request was
        // admitted. Queue rejection must not redirect a later ordinary Save.
        if self.manual_project_file_destination.as_ref() != Some(&destination) {
            self.manual_project_file_applied_request = None;
        }
        self.manual_project_file_destination = Some(destination);
        Ok(request_id)
    }

    fn submit_autosave(
        &mut self,
        max_recovery_points: usize,
        retention_days: u32,
    ) -> anyhow::Result<(ProjectPersistenceRequestId, PathBuf)> {
        let runtime_lease = self.ensure_open_project_runtime_lease()?;
        let session = self
            .authoring
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("当前无可自动保存的项目"))?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let project_file = session.project_file().to_path_buf();
        let runtime_root = runtime_lease.runtime_root().to_path_buf();
        let saved_at_unix_ms = unix_now_ms();
        let autosave_file = runtime_root.join("autosave").join(autosave_archive_leaf(
            saved_at_unix_ms,
            snapshot.generation.get(),
        ));
        let request_id = self
            .project_persistence
            .submit(
                snapshot,
                ProjectPersistencePurpose::Autosave {
                    destination: AutosaveArchiveDestination::new(
                        autosave_file.clone(),
                        project_file,
                    )
                    .map_err(anyhow::Error::msg)?,
                    max_recovery_points: max_recovery_points.max(1),
                    retention_days: retention_days.max(1),
                    saved_at_unix_ms,
                },
                runtime_lease,
            )
            .map_err(anyhow::Error::msg)?;
        self.autosave_in_flight_request = Some(request_id);
        self.autosave_last_requested_at = Instant::now();
        Ok((request_id, autosave_file))
    }

    /// Poll durable persistence and schedule due autosaves.
    pub fn poll_project_persistence(&mut self) -> bool {
        let retired_before = self.retired_project_libraries.len();
        collect_retired_project_libraries(&mut self.retired_project_libraries);
        let completions = self.project_persistence.poll_completions();
        let mut changed = self.retired_project_libraries.len() != retired_before;
        for completion in completions {
            changed = true;
            let purpose = completion.purpose.clone();
            let result = self.apply_persistence_completion(completion);
            match (purpose, result) {
                (
                    ProjectPersistencePurpose::Manual { .. },
                    Ok(PersistenceCompletionDisposition::Applied),
                ) => {
                    self.set_status_hint("项目已耐久保存", false);
                }
                (
                    ProjectPersistencePurpose::Manual { .. },
                    Ok(PersistenceCompletionDisposition::AppliedWithRecoveryWarning { reason }),
                ) => {
                    self.set_status_hint(
                        format!("项目已耐久保存，但恢复点清理失败：{reason}"),
                        true,
                    );
                }
                (
                    ProjectPersistencePurpose::Autosave { .. },
                    Ok(PersistenceCompletionDisposition::Applied),
                )
                | (_, Ok(PersistenceCompletionDisposition::SatisfiedByNewerPublication))
                | (_, Ok(PersistenceCompletionDisposition::IgnoredSupersededDestination))
                | (_, Ok(PersistenceCompletionDisposition::IgnoredStaleSession)) => {}
                (
                    ProjectPersistencePurpose::Autosave { .. },
                    Ok(PersistenceCompletionDisposition::AppliedWithRecoveryWarning { reason }),
                ) => {
                    self.set_status_hint(
                        format!("自动保存完成，但恢复点清理状态异常：{reason}"),
                        true,
                    );
                }
                (ProjectPersistencePurpose::Manual { .. }, Err(error)) => {
                    self.set_status_hint(format!("保存项目失败：{error}"), true);
                }
                (ProjectPersistencePurpose::Autosave { .. }, Err(error)) => {
                    self.set_status_hint(format!("自动保存失败：{error}"), true);
                }
            }
        }
        if !self.project_close_blocks_actions() && self.maybe_request_autosave() {
            changed = true;
        }
        changed
    }

    fn apply_persistence_completion(
        &mut self,
        completion: ProjectPersistenceCompletion,
    ) -> Result<PersistenceCompletionDisposition, String> {
        if self.autosave_in_flight_request == Some(completion.request_id) {
            self.autosave_in_flight_request = None;
        }
        if !self.project_persistence.accepts_completion(&completion) {
            return Ok(PersistenceCompletionDisposition::IgnoredStaleSession);
        }
        let same_session = self
            .authoring
            .as_ref()
            .is_some_and(|session| session.session_id() == completion.session_id);
        if !same_session {
            return Ok(PersistenceCompletionDisposition::IgnoredStaleSession);
        }
        let Some(active_lease) = self.project_runtime_lease.as_ref() else {
            return Err("open Project Session has no runtime lease".to_owned());
        };
        if active_lease.id() != completion.runtime_lease_id {
            return Err(
                "persistence completion carries a different Project runtime lease identity"
                    .to_owned(),
            );
        }
        if let ProjectPersistencePurpose::Manual { destination } = &completion.purpose {
            if destination.session_id() != completion.session_id {
                return Err(
                    "manual persistence completion has an inconsistent destination binding"
                        .to_owned(),
                );
            }
            if self.manual_project_file_destination.as_ref() != Some(destination) {
                return Ok(PersistenceCompletionDisposition::IgnoredSupersededDestination);
            }
            let Some(session) = self.authoring.as_ref() else {
                return Ok(PersistenceCompletionDisposition::IgnoredStaleSession);
            };
            let covered_by_applied_request = self
                .manual_project_file_applied_request
                .as_ref()
                .is_some_and(|(applied_destination, applied_request_id)| {
                    applied_destination == destination
                        && applied_request_id.get() >= completion.request_id.get()
                });
            if covered_by_applied_request
                && session.project_file() == destination.project_file()
                && session.manual_save_baseline_covers(
                    completion.generation,
                    completion.asset_library_revision,
                )
            {
                let current_project_file = session.project_file().to_path_buf();
                let retire_all = !session.is_dirty();
                if let Err(reason) = reconcile_recovery_after_manual_save(
                    active_lease,
                    &current_project_file,
                    retire_all,
                ) {
                    let reason = reason.to_string();
                    return Ok(
                        PersistenceCompletionDisposition::AppliedWithRecoveryWarning { reason },
                    );
                }
                return Ok(PersistenceCompletionDisposition::SatisfiedByNewerPublication);
            }
        }
        let publication_context = completion.publication_failure.as_ref().map(|failure| {
            format!(
                "publication phase={:?}, state={:?}: {}",
                failure.phase, failure.kind, failure.reason
            )
        });
        let failure_context = completion.failure.as_ref().map(|failure| {
            format!(
                "failure category={:?}: {}",
                failure.category, failure.reason
            )
        });
        if completion.result.is_ok() && (publication_context.is_some() || failure_context.is_some())
        {
            return Err(
                "successful persistence completion carries contradictory terminal-failure evidence"
                    .to_owned(),
            );
        }
        let persisted = completion.result.map_err(|reason| {
            [failure_context, publication_context]
                .into_iter()
                .flatten()
                .fold(reason, |message, context| format!("{message}; {context}"))
        })?;
        if persisted.asset_library_revision != completion.asset_library_revision {
            return Err(
                "persistence completion result does not match its captured Asset Library revision"
                    .to_owned(),
            );
        }
        let Some(session) = self.authoring.as_mut() else {
            return Ok(PersistenceCompletionDisposition::IgnoredStaleSession);
        };
        let mut applied_manual_destination = None;
        let recovery_reconciliation = match completion.purpose {
            ProjectPersistencePurpose::Manual { destination } => {
                session
                    .mark_saved(
                        completion.generation,
                        persisted.document_revision,
                        persisted.asset_library_revision,
                        persisted.meta,
                        Some(destination.project_file().to_path_buf()),
                    )
                    .map_err(|error| error.to_string())?;
                applied_manual_destination = Some(destination.clone());
                let current_project_file = session.project_file().to_path_buf();
                let retire_all = !session.is_dirty();
                Some((current_project_file, retire_all))
            }
            ProjectPersistencePurpose::Autosave { .. } => {
                session
                    .mark_autosaved(completion.generation, persisted.asset_library_revision)
                    .map_err(|error| error.to_string())?;
                (session.has_durable_baseline() && !session.is_dirty())
                    .then(|| (session.project_file().to_path_buf(), true))
            }
        };
        if let Some(destination) = applied_manual_destination {
            let retain_newer_receipt = self
                .manual_project_file_applied_request
                .as_ref()
                .is_some_and(|(applied_destination, applied_request_id)| {
                    applied_destination == &destination
                        && applied_request_id.get() > completion.request_id.get()
                });
            if !retain_newer_receipt {
                self.manual_project_file_applied_request =
                    Some((destination, completion.request_id));
            }
        }
        if let Some((current_project_file, retire_all)) = recovery_reconciliation {
            if let Err(reason) = reconcile_recovery_after_manual_save(
                active_lease,
                &current_project_file,
                retire_all,
            ) {
                tracing::warn!(
                    project_id = %active_lease.project_id(),
                    runtime_root = %active_lease.runtime_root().display(),
                    %reason,
                    "manual Project save succeeded but recovery authority reconciliation failed"
                );
                return Ok(
                    PersistenceCompletionDisposition::AppliedWithRecoveryWarning {
                        reason: reason.to_string(),
                    },
                );
            }
        }
        Ok(PersistenceCompletionDisposition::Applied)
    }

    fn maybe_request_autosave(&mut self) -> bool {
        let Some(session) = self.authoring.as_ref() else {
            return false;
        };
        let interval = session.document().settings.auto_save_interval;
        if interval == 0
            || !session.is_dirty()
            || session.is_current_autosaved()
            || self.autosave_in_flight_request.is_some()
            || self.autosave_last_requested_at.elapsed()
                < std::time::Duration::from_secs(u64::from(interval))
        {
            return false;
        }
        match self.submit_autosave(10, 7) {
            Ok(_) => true,
            Err(error) => {
                self.set_status_hint(format!("无法启动自动保存：{error}"), true);
                true
            }
        }
    }

    pub(crate) fn wait_for_persistence_request(
        &mut self,
        request_id: ProjectPersistenceRequestId,
    ) -> anyhow::Result<()> {
        let completion = self.wait_for_persistence_completion(request_id)?;
        match self.apply_persistence_completion(completion).map_err(anyhow::Error::msg)? {
            PersistenceCompletionDisposition::Applied => Ok(()),
            PersistenceCompletionDisposition::AppliedWithRecoveryWarning { reason } => {
                self.set_status_hint(format!("项目已耐久保存，但恢复点清理失败：{reason}"), true);
                Ok(())
            }
            PersistenceCompletionDisposition::SatisfiedByNewerPublication => Ok(()),
            PersistenceCompletionDisposition::IgnoredSupersededDestination => {
                anyhow::bail!("项目耐久保存目标在完成前已被较新的 Save As 请求取代")
            }
            PersistenceCompletionDisposition::IgnoredStaleSession => {
                anyhow::bail!("项目持久化完成不再属于当前会话代际")
            }
        }
    }

    fn wait_for_persistence_completion(
        &mut self,
        request_id: ProjectPersistenceRequestId,
    ) -> anyhow::Result<ProjectPersistenceCompletion> {
        let deadline = Instant::now() + std::time::Duration::from_secs(300);
        loop {
            let mut requested = None;
            for completion in self.project_persistence.poll_completions() {
                if completion.request_id == request_id {
                    requested = Some(completion);
                } else {
                    self.apply_persistence_completion(completion).map_err(anyhow::Error::msg)?;
                }
            }
            if let Some(completion) = requested {
                return Ok(completion);
            }
            if Instant::now() >= deadline {
                anyhow::bail!("等待项目耐久保存超时");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub fn write_autosave_snapshot(
        &mut self,
        max_recovery_points: usize,
        retention_days: u32,
    ) -> anyhow::Result<PathBuf> {
        let (request_id, autosave_file) =
            self.submit_autosave(max_recovery_points, retention_days)?;
        self.wait_for_persistence_request(request_id)?;
        Ok(autosave_file)
    }

    pub fn save_project_file_as(&mut self, target_file: PathBuf) -> anyhow::Result<()> {
        let request_id = self.request_project_save_as(target_file)?;
        self.wait_for_persistence_request(request_id)
    }

    pub fn save_project_file(&mut self) -> anyhow::Result<()> {
        let request_id = self.request_project_save()?;
        self.wait_for_persistence_request(request_id)
    }

    pub fn ensure_minimum_tracks(&mut self) {
        let needs_tracks = self.active_sequence().is_some_and(|sequence| {
            sequence.video_tracks.is_empty() || sequence.audio_tracks.is_empty()
        });
        if !needs_tracks {
            return;
        }
        let _ = self.commit_active_sequence_edit("补齐基础轨道", |sequence| {
            if sequence.video_tracks.is_empty() {
                sequence.video_tracks.push(mondrian_timeline::track::Track::new_video("V1"));
            }
            if sequence.audio_tracks.is_empty() {
                sequence.add_audio_track();
            }
            sequence.normalize_track_names();
            Ok(())
        });
    }

    pub fn create_new_project_at(
        &mut self,
        project_file: PathBuf,
        name: &str,
        width: u32,
        height: u32,
        frame_rate: Rational,
    ) -> anyhow::Result<()> {
        let settings = SequenceSettings {
            resolution: Resolution { width, height },
            frame_rate,
            ..SequenceSettings::default()
        };
        self.create_new_project_with_settings_at(
            project_file,
            name,
            settings,
            mondrian_core::ProjectColorEnvironment::default(),
            ProjectSettings::default(),
        )
    }

    pub fn create_new_project_with_settings_at(
        &mut self,
        project_file: PathBuf,
        name: &str,
        settings: SequenceSettings,
        color_environment: mondrian_core::ProjectColorEnvironment,
        project_settings: ProjectSettings,
    ) -> anyhow::Result<()> {
        let mut handoff = self.begin_project_session_handoff()?;
        let result = self.create_new_project_with_settings_at_in_handoff(
            project_file,
            name,
            settings,
            color_environment,
            project_settings,
            &mut handoff,
        );
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.resume_handoff_after_error(handoff, error)),
        }
    }

    fn create_new_project_with_settings_at_in_handoff(
        &mut self,
        project_file: PathBuf,
        name: &str,
        settings: SequenceSettings,
        color_environment: mondrian_core::ProjectColorEnvironment,
        project_settings: ProjectSettings,
        handoff: &mut Option<ProjectPersistencePauseToken>,
    ) -> anyhow::Result<()> {
        if project_file.exists() {
            anyhow::bail!("项目文件已存在：{}", project_file.display());
        }
        settings.validate_with_color_environment(&color_environment)?;
        color_environment.engine().ensure_loaded().map_err(anyhow::Error::msg)?;

        if let Some(parent) = project_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut sequence = Sequence::with_settings(name, settings.clone())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        sequence.playhead = TimelineTime::ZERO;

        let document = ProjectDocument::new(
            name,
            SequenceCollection::new(sequence),
            color_environment,
            settings,
            project_settings,
        );
        if self.active_sequence().is_some() {
            self.stop().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        let runtime_lease = self
            .claim_or_reuse_project_runtime(&project_file, document.project_id)
            .map_err(anyhow::Error::msg)?;
        runtime_lease.validate().map_err(anyhow::Error::msg)?;
        let runtime_root = runtime_lease.runtime_root().to_path_buf();
        let mut library_generation =
            self.prepare_project_library_generation(Arc::clone(&runtime_lease))?;
        let library = library_generation.open()?;
        let mut session =
            AuthoringSession::new_unsaved(document, project_file, runtime_root, library)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let project_id = session.project_id();
        let session_id = session.session_id();
        let destination = ManualProjectFileDestination::initial_create(
            session_id,
            session.project_file().to_path_buf(),
        )
        .map_err(anyhow::Error::msg)?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let expected_generation = snapshot.generation;
        let expected_asset_library_revision = snapshot.asset_library_revision;
        // The persistence worker may now receive a shared library Arc.
        // Disable candidate-drop deletion before submission; every failure
        // below retires it through weak lifetime evidence instead.
        library_generation.commit();
        let request_id = match self.project_persistence.submit(
            snapshot,
            ProjectPersistencePurpose::Manual { destination: destination.clone() },
            Arc::clone(&runtime_lease),
        ) {
            Ok(request_id) => request_id,
            Err(error) => {
                let error = self.discard_prepared_project_session_after_error(
                    session,
                    runtime_lease,
                    anyhow::Error::msg(error),
                );
                return Err(error);
            }
        };
        let completion = match self.wait_for_persistence_completion(request_id) {
            Ok(completion) => completion,
            Err(error) => {
                let error = self.discard_prepared_project_session_after_error(
                    session,
                    runtime_lease,
                    error,
                );
                return Err(error);
            }
        };
        let completion_is_exact = self.project_persistence.accepts_completion(&completion)
            && completion.session_id == session_id
            && completion.runtime_lease_id == runtime_lease.id()
            && completion.generation == expected_generation
            && completion.asset_library_revision == expected_asset_library_revision
            && matches!(
                &completion.purpose,
                ProjectPersistencePurpose::Manual {
                    destination: completed_destination
                } if completed_destination == &destination
            );
        if !completion_is_exact {
            let error = self.discard_prepared_project_session_after_error(
                session,
                runtime_lease,
                anyhow::anyhow!(
                    "initial Project publication returned inconsistent Session evidence"
                ),
            );
            return Err(error);
        }
        let persisted = match completion.result {
            Ok(persisted) => persisted,
            Err(error) => {
                let error = self.discard_prepared_project_session_after_error(
                    session,
                    runtime_lease,
                    anyhow::Error::msg(error),
                );
                return Err(error);
            }
        };
        if persisted.asset_library_revision != expected_asset_library_revision {
            let error = self.discard_prepared_project_session_after_error(
                session,
                runtime_lease,
                anyhow::anyhow!(
                    "initial Project publication returned a mismatched Asset Library revision"
                ),
            );
            return Err(error);
        }
        if let Err(error) = session.mark_saved(
            completion.generation,
            persisted.document_revision,
            persisted.asset_library_revision,
            persisted.meta,
            Some(destination.project_file().to_path_buf()),
        ) {
            let error = self.discard_prepared_project_session_after_error(
                session,
                runtime_lease,
                anyhow::anyhow!(error.to_string()),
            );
            return Err(error);
        }
        if let Err(error) = self.retire_project_session_handoff(handoff) {
            let error = self.discard_prepared_project_session_after_error(
                session,
                runtime_lease,
                anyhow::Error::msg(error),
            );
            return Err(error);
        }
        self.retain_current_project_library_generation();
        self.clear_timeline_targeting();
        self.authoring = Some(session);
        self.audio_monitoring.reset();
        self.synchronize_audio_idle_warmup_binding();
        self.project_runtime_lease = Some(runtime_lease);
        let replacement_destination = destination.replacement_binding();
        self.manual_project_file_destination = Some(replacement_destination.clone());
        self.manual_project_file_applied_request = Some((replacement_destination, request_id));
        self.autosave_in_flight_request = None;
        self.proxy_generation.bind_project(Some(project_id));
        self.media_import.bind_project(Some(project_id));
        self.media_import_batches.clear();
        self.media_asset_mutations.bind_project(Some(project_id));
        self.settle_preview_access_source();
        Ok(())
    }

    pub fn save_project(&mut self) -> anyhow::Result<()> {
        self.save_project_file()
    }

    /// Atomically replace the complete template copied into future Sequences.
    ///
    /// Existing Sequences are independent author aggregates and are never
    /// mutated by changing this Project template.
    pub fn update_new_sequence_defaults(
        &mut self,
        settings: SequenceSettings,
    ) -> mondrian_core::Result<()> {
        let Some(session) = self.authoring.as_ref() else {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_new_sequence_defaults".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            });
        };
        if session.document().new_sequence_defaults == settings {
            return Ok(());
        }
        settings.validate_with_color_environment(&session.document().color_environment)?;
        let before = session.document().clone();
        let mut after = before.clone();
        after.new_sequence_defaults = settings;
        self.commit_project_snapshot_command("修改新建序列默认设置", before, after)?;
        Ok(())
    }

    /// Atomically replace the Project-wide color engine.
    ///
    /// Every existing Sequence and the future-Sequence template must be valid
    /// in the proposed environment. The transaction is rejected as a whole
    /// rather than silently rewriting working or output spaces.
    pub fn update_project_color_environment(
        &mut self,
        color_environment: mondrian_core::ProjectColorEnvironment,
    ) -> mondrian_core::Result<()> {
        color_environment.engine().ensure_loaded().map_err(|reason| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_project_color_environment".to_owned(),
                reason,
            }
        })?;
        let Some(session) = self.authoring.as_mut() else {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_project_color_environment".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            });
        };
        if session.document().color_environment == color_environment {
            return Ok(());
        }
        session
            .document()
            .new_sequence_defaults
            .validate_with_color_environment(&color_environment)?;
        for sequence in &session.document().sequences.sequences {
            sequence.settings.validate_with_color_environment(&color_environment)?;
        }

        let before = session.document().clone();
        let mut after = before.clone();
        after.color_environment = color_environment;
        self.stop()?;
        self.commit_project_snapshot_command("修改项目色彩引擎", before, after)?;
        self.settle_preview_access_source();
        Ok(())
    }
}

#[cfg(test)]
mod persistence_lifecycle_tests {
    use super::*;
    use crate::app::project_runtime::{
        claim_project_runtime_lease_for_test, TEST_PROJECT_RUNTIME_NAMESPACE_ENV,
    };
    use mondrian_project::save_project_archive;
    use std::collections::BTreeSet;
    use std::collections::HashMap;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn project_manifest_and_asset_library_schema_versions_match() {
        assert_eq!(
            mondrian_project::PROJECT_LIBRARY_SCHEMA_VERSION,
            mondrian_assets::ASSET_LIBRARY_SCHEMA_VERSION,
            "the App composition root may not publish a manifest for a different embedded-library schema"
        );
    }

    #[test]
    fn autosave_archive_names_remain_unique_with_identical_time_and_generation() {
        let saved_at_unix_ms = 1_900_000_000_123_u64;
        let generation = 1;
        let first = autosave_archive_leaf(saved_at_unix_ms, generation);
        let second = autosave_archive_leaf(saved_at_unix_ms, generation);

        assert_ne!(
            first, second,
            "same-millisecond autosaves must never claim the same archive object"
        );
        assert!(first.starts_with("project-1900000000123-g1-"));
        assert!(first.ends_with(".autosave.mdp"));
        assert!(second.starts_with("project-1900000000123-g1-"));
        assert!(second.ends_with(".autosave.mdp"));
    }

    fn unique_root(_name: &str) -> PathBuf {
        static NEXT_ROOT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "mpl-{}-{}-{}",
            std::process::id(),
            unix_now_ms(),
            NEXT_ROOT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn test_state(root: &Path) -> AppState {
        let document = ProjectDocument::new(
            "Persistence Lifecycle",
            SequenceCollection::new(Sequence::new("Sequence")),
            mondrian_core::ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        let project_file = root.join("project.mdp");
        let lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            document.project_id,
        )
        .expect("claim runtime owner");
        let runtime_root = lease.runtime_root().to_path_buf();
        let library = AssetLibrary::open(runtime_root.join("library")).expect("asset library");
        let session = AuthoringSession::new_unsaved(document, project_file, runtime_root, library)
            .expect("authoring session");
        let mut state = AppState::new();
        state.authoring = Some(session);
        state.project_runtime_lease = Some(lease);
        state
    }

    #[test]
    fn app_project_close_keeps_the_caller_responsive_and_freezes_author_actions() {
        let root = unique_root("nonblocking-app-close");
        let mut state = test_state(&root);
        let gate = state.project_persistence.gate_next_request();
        state.request_project_save().expect("admit slow save");
        gate.wait_until_running();

        let started = Instant::now();
        assert!(state.begin_project_close().expect("begin asynchronous close"));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "Project close initiation must never wait for durable publication"
        );
        assert!(state.has_open_project());
        assert!(state.project_close_blocks_actions());
        assert!(matches!(
            state.poll_project_close(),
            ProjectClosePoll::Pending
        ));
        let action_error = state
            .dispatch_action(mondrian_editor_state::Action::DeselectAll)
            .expect_err("author actions must remain frozen during close");
        assert!(action_error.to_string().contains("安全关闭"));

        gate.release();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match state.poll_project_close() {
                ProjectClosePoll::Pending => {
                    assert!(Instant::now() < deadline, "Project close timed out");
                    std::thread::yield_now();
                }
                ProjectClosePoll::Closed => break,
                unexpected => panic!("unexpected Project close result: {unexpected:?}"),
            }
        }
        assert!(!state.has_open_project());
        assert!(!state.project_close_blocks_actions());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_before_close_never_retires_the_project_when_required_publication_fails() {
        let root = unique_root("required-save-close-failure");
        let mut state = test_state(&root);
        let original_path = state.current_project_path().expect("Project path").to_path_buf();
        let target = root.join("new-save-as-target.mdp");
        let competing_bytes = b"must survive failed save-before-close";
        let gate = state.project_persistence.gate_next_request();
        let required_save =
            state.request_project_save_as(target.clone()).expect("admit required Save As");
        gate.wait_until_running();
        assert!(state
            .begin_project_close_after_save(required_save)
            .expect("begin save-before-close"));
        fs::write(&target, competing_bytes).expect("create competing destination");
        gate.release();

        let deadline = Instant::now() + Duration::from_secs(10);
        let failure = loop {
            match state.poll_project_close() {
                ProjectClosePoll::Pending => {
                    assert!(Instant::now() < deadline, "Project close timed out");
                    std::thread::yield_now();
                }
                ProjectClosePoll::SaveRejected(reason) => break reason,
                unexpected => panic!("unexpected Project close result: {unexpected:?}"),
            }
        };
        assert!(failure.contains("did not establish the durable baseline"));
        assert!(
            state.has_open_project(),
            "failed required save keeps Project open"
        );
        assert!(!state.project_close_blocks_actions());
        assert_eq!(state.current_project_path(), Some(original_path.as_path()));
        assert_eq!(
            fs::read(&target).expect("competing file survives"),
            competing_bytes
        );

        fs::remove_file(&target).expect("remove competing target");
        let retry = state
            .request_project_save()
            .expect("resumed persistence admission accepts retry");
        state
            .wait_for_persistence_request(retry)
            .expect("retry establishes the retained Save As destination");
        assert_eq!(state.current_project_path(), Some(target.as_path()));
        state.close_project().expect("close retried Project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_force_close_retires_a_faulted_session_without_accepting_completion() {
        let root = unique_root("force-close-faulted-handoff");
        let mut state = test_state(&root);
        assert!(state.begin_project_close().expect("begin Project close"));
        state.test_poison_project_persistence_admission();

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match state.poll_project_close() {
                ProjectClosePoll::Pending => {
                    assert!(Instant::now() < deadline, "faulted close timed out");
                    std::thread::yield_now();
                }
                ProjectClosePoll::Faulted(reason) => {
                    assert!(reason.contains("changed during quiescence"));
                    break;
                }
                unexpected => panic!("unexpected faulted close result: {unexpected:?}"),
            }
        }
        assert!(state.has_open_project());
        assert!(state.has_project_close_fault());
        state
            .force_close_project_after_fault()
            .expect("explicit force close abandons poisoned Session");
        assert!(!state.has_open_project());
        assert!(!state.project_close_blocks_actions());
        let _ = fs::remove_dir_all(root);
    }

    fn wait_for_completions(
        state: &AppState,
        request_ids: &[ProjectPersistenceRequestId],
    ) -> HashMap<u64, ProjectPersistenceCompletion> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut completions = HashMap::new();
        while completions.len() < request_ids.len() {
            for completion in state.project_persistence.poll_completions() {
                if request_ids.contains(&completion.request_id) {
                    completions.insert(completion.request_id.get(), completion);
                }
            }
            assert!(
                Instant::now() < deadline,
                "persistence requests did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        completions
    }

    fn library_generation_roots(runtime_root: &Path) -> BTreeSet<PathBuf> {
        fs::read_dir(runtime_root)
            .expect("read runtime root")
            .map(|entry| entry.expect("runtime entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("library-generation-"))
            })
            .collect()
    }

    fn discovered_candidate(project_file: &Path, autosave_file: &Path) -> CrashRecoveryCandidate {
        discover_crash_recovery_candidates()
            .into_iter()
            .find(|candidate| {
                candidate.project_file == project_file && candidate.autosave_file == autosave_file
            })
            .expect("exact recovery candidate")
    }

    const CRASH_HELPER_MODE_ENV: &str = "MONDRIAN_TEST_RECOVERY_CRASH_HELPER";
    const CRASH_HELPER_PROJECT_ENV: &str = "MONDRIAN_TEST_RECOVERY_CRASH_PROJECT";
    const CRASH_HELPER_STAGE_ENV: &str = "MONDRIAN_TEST_RECOVERY_CRASH_STAGE";
    const CRASH_HELPER_PREVIOUS_NAME_ENV: &str = "MONDRIAN_TEST_RECOVERY_PREVIOUS_NAME";
    const CRASH_HELPER_TEST: &str = concat!(
        "app::project_lifecycle::persistence_lifecycle_tests::",
        "recovery_crash_writer_subprocess_helper"
    );

    fn recovery_candidates_for_project(project_file: &Path) -> Vec<CrashRecoveryCandidate> {
        discover_crash_recovery_candidates()
            .into_iter()
            .filter(|candidate| candidate.project_file == project_file)
            .collect()
    }

    fn run_recovery_crash_writer(project_file: &Path, stage: u32, previous_name: Option<&str>) {
        let mut command = Command::new(std::env::current_exe().expect("current test binary"));
        command
            .arg(CRASH_HELPER_TEST)
            .arg("--exact")
            .arg("--ignored")
            .arg("--nocapture")
            .env(CRASH_HELPER_MODE_ENV, "1")
            .env(CRASH_HELPER_PROJECT_ENV, project_file)
            .env(CRASH_HELPER_STAGE_ENV, stage.to_string())
            .env(
                TEST_PROJECT_RUNTIME_NAMESPACE_ENV,
                std::process::id().to_string(),
            );
        if let Some(previous_name) = previous_name {
            command.env(CRASH_HELPER_PREVIOUS_NAME_ENV, previous_name);
        } else {
            command.env_remove(CRASH_HELPER_PREVIOUS_NAME_ENV);
        }
        let status = command.status().expect("launch crash-writer subprocess");
        assert!(
            !status.success(),
            "crash-writer subprocess returned normally instead of terminating abnormally"
        );
        assert!(
            !recovery_candidates_for_project(project_file).is_empty(),
            "abnormal termination did not leave a discoverable Recovery Authority"
        );
    }

    #[test]
    #[ignore = "subprocess crash/recovery qualification; intentionally aborts three child processes"]
    fn repeated_process_crashes_preserve_latest_recovery_and_final_save_retires_authority() {
        let root = unique_root("repeated-process-crash-recovery");
        fs::create_dir_all(&root).expect("create crash qualification root");
        let project_file = root.join("repeated-crash.mdp");

        run_recovery_crash_writer(&project_file, 1, None);
        run_recovery_crash_writer(&project_file, 2, Some("Crash Stage 1"));
        run_recovery_crash_writer(&project_file, 3, Some("Crash Stage 2"));

        let candidate = recovery_candidates_for_project(&project_file)
            .into_iter()
            .next()
            .expect("latest recovery candidate");
        let mut recovered = AppState::new();
        recovered
            .open_project_from_autosave_snapshot(candidate)
            .expect("recover after repeated abnormal termination");
        assert_eq!(
            recovered.active_sequence().map(|sequence| sequence.name.as_str()),
            Some("Crash Stage 3")
        );
        assert!(recovered.has_unsaved_project_changes());
        let runtime_root = recovered
            .authoring
            .as_ref()
            .expect("recovered session")
            .runtime_root()
            .to_path_buf();

        recovered
            .save_project_file()
            .expect("cover repeated-crash recovery with a durable manual save");
        assert!(
            recovery_candidates_for_project(&project_file).is_empty(),
            "covering manual save must retire every repeated-crash recovery point"
        );
        recovered.close_project().expect("close recovered Project");
        let _ = fs::remove_dir_all(runtime_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "child entrypoint for repeated process-crash qualification"]
    fn recovery_crash_writer_subprocess_helper() {
        if std::env::var_os(CRASH_HELPER_MODE_ENV).as_deref() != Some(std::ffi::OsStr::new("1")) {
            return;
        }
        let project_file = PathBuf::from(
            std::env::var_os(CRASH_HELPER_PROJECT_ENV).expect("crash helper Project path"),
        );
        let stage = std::env::var(CRASH_HELPER_STAGE_ENV)
            .expect("crash helper stage")
            .parse::<u32>()
            .expect("numeric crash helper stage");
        let mut state = AppState::new();
        if stage == 1 {
            state
                .create_new_project_at(
                    project_file.clone(),
                    "Repeated Crash Recovery",
                    1920,
                    1080,
                    Rational::new(25, 1),
                )
                .expect("create crash qualification Project");
        } else {
            let candidate = recovery_candidates_for_project(&project_file)
                .into_iter()
                .next()
                .expect("crash helper recovery candidate");
            state
                .open_project_from_autosave_snapshot(candidate)
                .expect("crash helper recovery open");
            let previous_name = std::env::var(CRASH_HELPER_PREVIOUS_NAME_ENV)
                .expect("previous recovered Sequence name");
            assert_eq!(
                state.active_sequence().map(|sequence| sequence.name.as_str()),
                Some(previous_name.as_str()),
                "each crash cycle must resume the preceding durable Recovery Authority"
            );
        }
        let sequence_id = state.active_sequence().expect("active Sequence").id;
        state
            .rename_sequence(sequence_id, format!("Crash Stage {stage}"))
            .expect("commit crash-stage edit");
        state
            .write_autosave_snapshot(8, 30)
            .expect("durably publish crash-stage recovery point");

        std::process::abort();
    }

    #[test]
    fn same_project_reopen_keeps_retired_library_until_the_last_arc_drops() {
        let root = unique_root("same-project-reopen");
        let mut state = test_state(&root);
        state.save_project_file().expect("establish saved archive");
        let project_file = state.authoring.as_ref().expect("session").project_file().to_path_buf();
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();
        let lease_id = state.project_runtime_lease.as_ref().expect("lease").id();
        let old_library = Arc::clone(state.authoring.as_ref().expect("session").asset_library());
        let old_library_root =
            old_library.database_path().parent().expect("library root").to_path_buf();

        state.open_project_file(project_file).expect("reopen the same Project");

        let new_library_root = state
            .authoring
            .as_ref()
            .expect("reopened session")
            .asset_library()
            .database_path()
            .parent()
            .expect("new library root")
            .to_path_buf();
        assert_ne!(new_library_root, old_library_root);
        assert_eq!(
            state.project_runtime_lease.as_ref().expect("reused lease").id(),
            lease_id
        );
        assert_eq!(
            state.authoring.as_ref().expect("reopened session").runtime_root(),
            runtime_root
        );

        state.collect_released_project_libraries();
        assert!(old_library_root.is_dir());
        drop(old_library);
        state.collect_released_project_libraries();
        assert!(!old_library_root.exists());
        assert!(new_library_root.is_dir());

        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_library_candidate_resumes_previous_session_and_persistence_generation() {
        let root = unique_root("failed-library-candidate");
        let mut state = test_state(&root);
        state.save_project_file().expect("establish saved archive");
        let sequence_id = state.active_sequence().expect("active Sequence").id;
        state
            .rename_sequence(sequence_id, "Undoable Name")
            .expect("create authoring history");
        let original_session_id = state.authoring.as_ref().expect("session").session_id();
        let original_project_file =
            state.authoring.as_ref().expect("session").project_file().to_path_buf();
        let original_history = state.authoring_history().expect("history").diagnostics();
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();
        let generations_before = library_generation_roots(&runtime_root);

        let invalid_library = root.join("invalid-library.db");
        fs::write(&invalid_library, b"not a SQLite database").expect("invalid library fixture");
        let invalid_archive = root.join("invalid-library.mdp");
        save_project_archive(
            state.authoring.as_ref().expect("session").document(),
            &invalid_library,
            &invalid_archive,
        )
        .expect("package structurally valid archive");

        let error = state
            .open_project_file(invalid_archive)
            .expect_err("invalid embedded SQLite must reject the candidate");
        assert!(!error.to_string().is_empty());
        let session = state.authoring.as_ref().expect("original session remains");
        assert_eq!(session.session_id(), original_session_id);
        assert_eq!(session.project_file(), original_project_file);
        assert_eq!(
            session.active_sequence().expect("active Sequence").name,
            "Undoable Name"
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            original_history
        );
        assert_eq!(
            library_generation_roots(&runtime_root),
            generations_before,
            "failed candidate must leave no untracked immutable generation"
        );

        state
            .save_project_file()
            .expect("resumed persistence generation accepts a later save");
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn closed_project_can_be_reopened_immediately_without_a_runtime_lease_race() {
        let root = unique_root("close-immediate-reopen");
        let project_file = root.join("project.mdp");
        let mut first = AppState::new();
        first
            .create_new_project_at(
                project_file.clone(),
                "Immediate Reopen",
                1920,
                1080,
                Rational::new(25, 1),
            )
            .expect("create project");
        let project_id = first.authoring.as_ref().expect("session").project_id();
        let first_session_id = first.authoring.as_ref().expect("session").session_id();
        let runtime_root = first.authoring.as_ref().expect("session").runtime_root().to_path_buf();

        first.close_project().expect("close project");
        let mut second = AppState::new();
        second
            .open_project_file(project_file)
            .expect("immediate reopen in a fresh AppState");
        let reopened = second.authoring.as_ref().expect("reopened session");
        assert_eq!(reopened.project_id(), project_id);
        assert_eq!(reopened.runtime_root(), runtime_root);
        assert_ne!(reopened.session_id(), first_session_id);

        second.close_project().expect("close reopened project");
        drop(second);
        drop(first);
        let _ = fs::remove_dir_all(&runtime_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ordinary_open_of_a_same_id_copy_uses_its_own_path_paired_runtime_root() {
        let root = unique_root("same-id-copy-open");
        let source_file = root.join("source.mdp");
        let copy_file = root.join("copy.mdp");
        let mut state = AppState::new();
        state
            .create_new_project_at(
                source_file.clone(),
                "Source",
                1920,
                1080,
                Rational::new(25, 1),
            )
            .expect("create source Project");
        let project_id = state.authoring.as_ref().expect("source session").project_id();
        let source_runtime =
            state.authoring.as_ref().expect("source session").runtime_root().to_path_buf();
        let retained_source_library =
            Arc::clone(state.authoring.as_ref().expect("source session").asset_library());
        fs::copy(&source_file, &copy_file).expect("copy Project archive with the same ProjectId");

        state
            .open_project_file(copy_file.clone())
            .expect("open same-ID filesystem copy in the coordinated process");

        let opened = state.authoring.as_ref().expect("copy session");
        let copy_runtime = opened.runtime_root().to_path_buf();
        assert_eq!(opened.project_id(), project_id);
        assert_eq!(opened.project_file(), copy_file);
        assert_ne!(copy_runtime, source_runtime);
        assert_eq!(
            copy_runtime,
            project_runtime_root_for_project(&copy_file, project_id)
                .expect("derive copy paired root")
        );
        assert!(
            retained_source_library.database_path().is_file(),
            "the retired source generation remains immutable while external Arcs exist"
        );

        state.close_project().expect("close copy");
        drop(retained_source_library);
        drop(state);
        let mut reopened = AppState::new();
        reopened
            .open_project_file(copy_file)
            .expect("logical authority releases after every coordinated root lease drops");
        assert_eq!(
            reopened.authoring.as_ref().expect("reopened copy").runtime_root(),
            copy_runtime
        );
        reopened.close_project().expect("close reopened copy");

        let _ = fs::remove_dir_all(source_runtime);
        let _ = fs::remove_dir_all(copy_runtime);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_initial_publication_leaves_the_previous_project_fully_active() {
        let root = unique_root("atomic-project-create");
        let mut state = test_state(&root);
        state.save_project_file().expect("establish old baseline");
        let sequence_id = state.active_sequence().expect("active Sequence").id;
        state
            .rename_sequence(sequence_id, "Previous Project State")
            .expect("create old history");
        let old_session_id = state.authoring.as_ref().expect("old session").session_id();
        let old_project_file =
            state.authoring.as_ref().expect("old session").project_file().to_path_buf();
        let old_lease_id = state.project_runtime_lease.as_ref().expect("old lease").id();
        let old_history = state.authoring_history().expect("old history").diagnostics();
        let new_project_file = root.join("new-project.mdp");
        state
            .project_persistence
            .fail_next_submission_for_test("injected initial publication failure");

        let error = state
            .create_new_project_at(
                new_project_file.clone(),
                "Must Not Install",
                1920,
                1080,
                Rational::new(25, 1),
            )
            .expect_err("failed first publication must abort Project replacement");

        assert!(error.to_string().contains("injected initial publication failure"));
        let session = state.authoring.as_ref().expect("old session remains");
        assert_eq!(session.session_id(), old_session_id);
        assert_eq!(session.project_file(), old_project_file);
        assert_eq!(
            session.active_sequence().expect("active Sequence").name,
            "Previous Project State"
        );
        assert_eq!(
            state.project_runtime_lease.as_ref().expect("old lease").id(),
            old_lease_id
        );
        assert_eq!(
            state.authoring_history().expect("old history").diagnostics(),
            old_history
        );
        assert!(!new_project_file.exists());
        let candidate_runtime_roots = project_runtime_roots_for_path_for_test(&new_project_file)
            .expect("enumerate prepared runtime payloads");
        assert!(!candidate_runtime_roots.is_empty());
        for runtime_root in &candidate_runtime_roots {
            assert!(
                library_generation_roots(runtime_root).is_empty(),
                "failed prepared Session must not retain a library generation"
            );
        }
        state
            .save_project_file()
            .expect("old persistence admission was resumed exactly");

        state.close_project().expect("close old project");
        for runtime_root in candidate_runtime_roots {
            let _ = fs::remove_dir_all(runtime_root);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_entry_creation_cannot_be_overwritten_by_initial_project_publication() {
        let root = unique_root("atomic-project-create-race");
        let mut state = test_state(&root);
        state.save_project_file().expect("establish old baseline");
        let sequence_id = state.active_sequence().expect("active Sequence").id;
        state
            .rename_sequence(sequence_id, "Previous Project State")
            .expect("create old history");
        let old_session_id = state.authoring.as_ref().expect("old session").session_id();
        let old_project_file =
            state.authoring.as_ref().expect("old session").project_file().to_path_buf();
        let old_lease_id = state.project_runtime_lease.as_ref().expect("old lease").id();
        let old_history = state.authoring_history().expect("old history").diagnostics();
        let new_project_file = root.join("concurrently-created.mdp");
        let external_bytes = b"external directory entry must survive".to_vec();
        let gate = state.project_persistence.gate_next_request();
        let racer_target = new_project_file.clone();
        let racer_bytes = external_bytes.clone();
        let racer = std::thread::spawn(move || {
            gate.wait_until_running();
            fs::write(&racer_target, racer_bytes).expect("create competing directory entry");
            gate.release();
        });

        let error = state
            .create_new_project_at(
                new_project_file.clone(),
                "Must Not Replace",
                1920,
                1080,
                Rational::new(25, 1),
            )
            .expect_err("create-only publication must reject the competing entry");
        racer.join().expect("competing publisher");

        assert!(
            !error.to_string().is_empty(),
            "publication rejection must preserve a diagnostic"
        );
        assert_eq!(
            fs::read(&new_project_file).expect("read competing entry"),
            external_bytes,
            "the final publication operation must never replace a newly created entry"
        );
        let session = state.authoring.as_ref().expect("old session remains");
        assert_eq!(session.session_id(), old_session_id);
        assert_eq!(session.project_file(), old_project_file);
        assert_eq!(
            session.active_sequence().expect("active Sequence").name,
            "Previous Project State"
        );
        assert_eq!(
            state.project_runtime_lease.as_ref().expect("old lease").id(),
            old_lease_id
        );
        assert_eq!(
            state.authoring_history().expect("old history").diagnostics(),
            old_history
        );
        let candidate_runtime_roots = project_runtime_roots_for_path_for_test(&new_project_file)
            .expect("enumerate prepared runtime payloads");
        assert!(!candidate_runtime_roots.is_empty());
        for runtime_root in &candidate_runtime_roots {
            assert!(
                library_generation_roots(runtime_root).is_empty(),
                "rejected publication must not retain a library generation"
            );
        }
        state
            .save_project_file()
            .expect("old persistence generation resumes after rejection");

        state.close_project().expect("close old project");
        let _ = fs::remove_file(new_project_file);
        for runtime_root in candidate_runtime_roots {
            let _ = fs::remove_dir_all(runtime_root);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_as_establishment_blocks_following_saves_from_overwriting_a_competing_entry() {
        let root = unique_root("save-as-establishment-race");
        let mut state = test_state(&root);
        let original_project_file =
            state.authoring.as_ref().expect("session").project_file().to_path_buf();
        let target = root.join("new-canonical.mdp");
        let external_bytes = b"competing Save As entry must survive".to_vec();
        let gate = state.project_persistence.gate_next_request();

        let establish_id = state
            .request_project_save_as(target.clone())
            .expect("admit Save As establishment");
        let following_id =
            state.request_project_save().expect("admit ordinary Save behind establishment");
        let racer_target = target.clone();
        let racer_bytes = external_bytes.clone();
        let racer = std::thread::spawn(move || {
            gate.wait_until_running();
            fs::write(&racer_target, racer_bytes).expect("create competing entry");
            gate.release();
        });
        let mut completions = wait_for_completions(&state, &[establish_id, following_id]);
        racer.join().expect("competing publisher");

        assert!(
            completions
                .remove(&establish_id.get())
                .expect("establishment completion")
                .result
                .is_err(),
            "the establishment request must reject the competing entry"
        );
        assert!(
            completions
                .remove(&following_id.get())
                .expect("following Save completion")
                .result
                .is_err(),
            "a dependent Save must not downgrade failed establishment to replacement"
        );
        assert_eq!(
            fs::read(&target).expect("read competing entry"),
            external_bytes
        );
        assert_eq!(
            state.authoring.as_ref().expect("session").project_file(),
            original_project_file
        );

        fs::remove_file(&target).expect("remove competing entry");
        let retry_id =
            state.request_project_save().expect("retry retained create-only destination");
        state
            .wait_for_persistence_request(retry_id)
            .expect("retry establishes the destination after it becomes free");
        assert_eq!(
            state.authoring.as_ref().expect("session").project_file(),
            target
        );

        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_autosave_releases_the_scheduler_for_retry() {
        let root = unique_root("retry");
        let mut state = test_state(&root);
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();
        fs::write(runtime_root.join("autosave"), b"blocks-directory")
            .expect("blocking autosave path");

        state.submit_autosave(2, 7).expect("admit failing autosave");
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.autosave_in_flight_request.is_some() {
            state.poll_project_persistence();
            assert!(
                Instant::now() < deadline,
                "failed autosave did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!state.authoring.as_ref().expect("session").is_current_autosaved());

        fs::remove_file(runtime_root.join("autosave")).expect("remove blocker");
        state.submit_autosave(2, 7).expect("retry autosave");
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.autosave_in_flight_request.is_some() {
            state.poll_project_persistence();
            assert!(Instant::now() < deadline, "retry autosave did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(state.authoring.as_ref().expect("session").is_current_autosaved());
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_manual_save_durably_retires_recovery_points() {
        let root = unique_root("retire-current");
        let mut state = test_state(&root);
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();

        let autosave = state.write_autosave_snapshot(4, 7).expect("write autosave");
        assert!(autosave.is_file());
        assert!(AppState::autosave_manifest_path(&runtime_root).is_file());

        state.save_project_file().expect("manual save");
        assert!(!state.has_unsaved_project_changes());
        assert!(
            !autosave.exists(),
            "current manual save must retire covered recovery archive"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(AppState::autosave_manifest_path(&runtime_root)).expect("retired manifest"),
        )
        .expect("valid retired manifest");
        assert_eq!(
            manifest["snapshots"].as_array().map(Vec::len),
            Some(0),
            "retirement must first publish an empty canonical manifest"
        );
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn storage_exhaustion_keeps_project_open_and_preserves_recovery_authority() {
        let root = unique_root("retain-recovery-on-storage-exhaustion");
        let mut state = test_state(&root);
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();
        let autosave = state.write_autosave_snapshot(4, 7).expect("write autosave");
        let manifest_path = AppState::autosave_manifest_path(&runtime_root);
        let manifest_before = fs::read(&manifest_path).expect("recovery manifest");
        state
            .project_persistence
            .fail_next_worker_io_for_test(std::io::ErrorKind::StorageFull);

        let request = state.request_project_save().expect("submit manual save");
        let error = state
            .wait_for_persistence_request(request)
            .expect_err("storage exhaustion must fail manual save");

        assert!(error.to_string().contains("StorageExhausted"));
        assert!(state.has_open_project());
        assert!(state.has_unsaved_project_changes());
        assert!(autosave.is_file());
        assert_eq!(
            fs::read(&manifest_path).expect("preserved recovery manifest"),
            manifest_before,
            "failed manual save must not rewrite or retire Recovery Authority"
        );
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_manual_completion_cannot_retire_newer_recovery_authority() {
        let root = unique_root("retain-on-stale-save");
        let mut state = test_state(&root);
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();
        let autosave = state.write_autosave_snapshot(4, 7).expect("write autosave");
        let request_id = state.request_project_save().expect("submit manual save");

        state
            .authoring
            .as_mut()
            .expect("session")
            .edit_active_sequence("edit-after-save-snapshot", |sequence| {
                sequence.name = "Newer Author State".to_owned();
                Ok(())
            })
            .expect("commit newer author state");
        state.wait_for_persistence_request(request_id).expect("wait stale manual save");

        assert!(state.has_unsaved_project_changes());
        assert!(
            autosave.is_file(),
            "a stale manual completion must not remove recovery authority"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(AppState::autosave_manifest_path(&runtime_root)).expect("recovery manifest"),
        )
        .expect("valid recovery manifest");
        assert_eq!(manifest["snapshots"].as_array().map(Vec::len), Some(1));
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_save_as_rebinds_recovery_without_moving_the_live_runtime() {
        let root = unique_root("rebind-stale-save-as");
        let target = root.join("save-as.mdp");
        let mut state = AppState::new();
        state
            .create_new_project_at(
                root.join("project.mdp"),
                "Recovery Rebind",
                1920,
                1080,
                Rational::new(25, 1),
            )
            .expect("create production-lifecycle Project");
        let sequence_id = state.active_sequence().expect("active Sequence").id;
        state
            .rename_sequence(sequence_id, "Dirty Before Recovery")
            .expect("create recoverable author state");
        let runtime_root = state.authoring.as_ref().expect("session").runtime_root().to_path_buf();
        let first_autosave = state.write_autosave_snapshot(4, 7).expect("first autosave");
        let request_id = state.request_project_save_as(target.clone()).expect("submit save as");

        state
            .authoring
            .as_mut()
            .expect("session")
            .edit_active_sequence("edit-after-save-as-snapshot", |sequence| {
                sequence.name = "Unsaved After Save As".to_owned();
                Ok(())
            })
            .expect("commit newer author state");
        state.wait_for_persistence_request(request_id).expect("wait stale save as");

        let session = state.authoring.as_ref().expect("session");
        assert_eq!(session.project_file(), target);
        assert_eq!(session.runtime_root(), runtime_root);
        assert!(session.is_dirty());
        let manifest_path = AppState::autosave_manifest_path(&runtime_root);
        let rebound: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("rebound manifest"))
                .expect("valid rebound manifest");
        assert_eq!(rebound["project_file"], target.to_string_lossy().as_ref());
        assert_eq!(rebound["snapshots"].as_array().map(Vec::len), Some(1));
        assert!(first_autosave.is_file());

        let second_autosave = state.write_autosave_snapshot(4, 7).expect("second autosave");
        assert!(second_autosave.is_file());
        let candidate = discovered_candidate(&target, &second_autosave);
        let mut blocked = AppState::new();
        let contention = blocked
            .open_project_from_autosave_snapshot(candidate.clone())
            .expect_err("a live Project Session must exclude recovery in another AppState");
        assert!(contention.to_string().contains("already leased"));
        state.close_project().expect("close project");

        let mut recovered = AppState::new();
        recovered
            .open_project_from_autosave_snapshot(candidate)
            .expect("recover through rebound authority");
        let recovered_session = recovered.authoring.as_ref().expect("recovered session");
        assert_eq!(recovered_session.runtime_root(), runtime_root);
        assert!(recovered_session.is_dirty());

        recovered.save_project_file().expect("save recovered project");
        assert!(!first_autosave.exists());
        assert!(!second_autosave.exists());
        let retired: serde_json::Value =
            serde_json::from_slice(&fs::read(manifest_path).expect("retired manifest"))
                .expect("valid retired manifest");
        assert_eq!(retired["project_file"], target.to_string_lossy().as_ref());
        assert_eq!(retired["snapshots"].as_array().map(Vec::len), Some(0));
        recovered.close_project().expect("close recovered project");
        let _ = fs::remove_dir_all(runtime_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_admitted_during_save_as_targets_the_new_destination_and_cleans_exactly_that_file() {
        let root = unique_root("save-during-save-as");
        let target = root.join("new-canonical.mdp");
        let mut state = test_state(&root);
        let old_project_file =
            state.authoring.as_ref().expect("session").project_file().to_path_buf();
        let autosave = state.write_autosave_snapshot(4, 7).expect("recovery point");
        let save_as_id = state.request_project_save_as(target.clone()).expect("admit Save As");

        state
            .authoring
            .as_mut()
            .expect("session")
            .edit_active_sequence("edit-while-save-as-is-in-flight", |sequence| {
                sequence.name = "Saved Only At New Destination".to_owned();
                Ok(())
            })
            .expect("commit interleaved edit");
        let save_id = state.request_project_save().expect("admit following Save");

        let mut completions = wait_for_completions(&state, &[save_as_id, save_id]);
        let save_as_completion = completions.remove(&save_as_id.get()).expect("Save As completion");
        let save_completion = completions.remove(&save_id.get()).expect("Save completion");
        let (save_as_destination, save_destination) =
            match (&save_as_completion.purpose, &save_completion.purpose) {
                (
                    ProjectPersistencePurpose::Manual { destination: save_as_destination },
                    ProjectPersistencePurpose::Manual { destination: save_destination },
                ) => (save_as_destination, save_destination),
                _ => panic!("both requests must be manual publications"),
            };
        assert_eq!(save_as_destination, save_destination);
        assert_eq!(save_destination.project_file(), target);

        assert_eq!(
            state
                .apply_persistence_completion(save_as_completion)
                .expect("apply Save As completion"),
            PersistenceCompletionDisposition::Applied
        );
        assert!(state.has_unsaved_project_changes());
        assert!(autosave.is_file(), "stale Save As may only rebind recovery");
        assert_eq!(
            state.authoring.as_ref().expect("session").project_file(),
            target
        );

        assert_eq!(
            state
                .apply_persistence_completion(save_completion)
                .expect("apply covering Save completion"),
            PersistenceCompletionDisposition::Applied
        );
        assert!(!state.has_unsaved_project_changes());
        assert!(
            !autosave.exists(),
            "only the completion that covers the interleaved edit may retire recovery"
        );
        assert!(
            !old_project_file.exists(),
            "the following Save must never publish the newer snapshot to the pre-Save-As path"
        );

        let extracted = root.join("verify-new-canonical");
        let loaded = load_project_archive(&target, &extracted).expect("open new canonical archive");
        assert_eq!(
            loaded.document.sequences.active().expect("active Sequence").name,
            "Saved Only At New Destination"
        );
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reverse_completion_application_cannot_regress_a_newer_same_destination_save() {
        let root = unique_root("reverse-save-completions");
        let target = root.join("new-canonical.mdp");
        let mut state = test_state(&root);
        let save_as_id = state.request_project_save_as(target.clone()).expect("admit Save As");
        state
            .authoring
            .as_mut()
            .expect("session")
            .edit_active_sequence("edit-before-following-save", |sequence| {
                sequence.name = "Newer Same Destination".to_owned();
                Ok(())
            })
            .expect("commit newer state");
        let save_id = state.request_project_save().expect("admit following Save");
        let mut completions = wait_for_completions(&state, &[save_as_id, save_id]);
        let older = completions.remove(&save_as_id.get()).expect("older completion");
        let newer = completions.remove(&save_id.get()).expect("newer completion");

        assert_eq!(
            state.apply_persistence_completion(newer).expect("apply newer completion first"),
            PersistenceCompletionDisposition::Applied
        );
        assert_eq!(
            state
                .apply_persistence_completion(older)
                .expect("older completion is already covered"),
            PersistenceCompletionDisposition::SatisfiedByNewerPublication
        );
        let session = state.authoring.as_ref().expect("session");
        assert_eq!(session.project_file(), target);
        assert!(!session.is_dirty());
        assert_eq!(
            session.active_sequence().expect("active Sequence").name,
            "Newer Same Destination"
        );
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_save_as_keeps_its_admitted_destination_for_an_ordinary_save_retry() {
        let root = unique_root("failed-save-as-retry");
        let blocked_parent = root.join("blocked-parent");
        fs::create_dir_all(&root).expect("create test root");
        fs::write(&blocked_parent, b"not-a-directory").expect("create path blocker");
        let target = blocked_parent.join("new-canonical.mdp");
        let mut state = test_state(&root);
        let original = state.authoring.as_ref().expect("session").project_file().to_path_buf();

        let failed_id =
            state.request_project_save_as(target.clone()).expect("admit failing Save As");
        let failure = state
            .wait_for_persistence_request(failed_id)
            .expect_err("blocked destination must fail");
        assert!(!failure.to_string().is_empty());
        assert_eq!(
            state.authoring.as_ref().expect("session").project_file(),
            original
        );
        let admitted = state
            .manual_project_file_destination
            .as_ref()
            .expect("admitted destination")
            .clone();
        assert_eq!(admitted.project_file(), target);

        fs::remove_file(&blocked_parent).expect("remove path blocker");
        fs::create_dir_all(&blocked_parent).expect("create destination directory");
        let retry_id = state.request_project_save().expect("ordinary Save retries Save As target");
        let mut completions = wait_for_completions(&state, &[retry_id]);
        let retry = completions.remove(&retry_id.get()).expect("retry completion");
        let ProjectPersistencePurpose::Manual { destination } = &retry.purpose else {
            panic!("retry must be a manual publication");
        };
        assert_eq!(destination, &admitted);
        assert_eq!(destination.project_file(), target);
        assert_eq!(
            state.apply_persistence_completion(retry).expect("apply retry completion"),
            PersistenceCompletionDisposition::Applied
        );
        assert_eq!(
            state.authoring.as_ref().expect("session").project_file(),
            target
        );
        assert!(!state.has_unsaved_project_changes());
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_existing_baseline_cannot_hide_a_newer_manual_request_failure() {
        let root = unique_root("newer-manual-failure");
        let mut state = test_state(&root);
        state.save_project_file().expect("establish manual baseline");
        let request_id = state.request_project_save().expect("admit newer Save");
        let mut completions = wait_for_completions(&state, &[request_id]);
        let mut completion = completions.remove(&request_id.get()).expect("newer Save completion");
        completion.result = Err("modeled later publication failure".to_owned());

        let error = state
            .apply_persistence_completion(completion)
            .expect_err("an older applied receipt cannot satisfy a newer request");

        assert!(error.contains("modeled later publication failure"));
        assert!(!state.has_unsaved_project_changes());
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn superseded_destination_failure_cannot_rebind_or_dirty_the_current_destination() {
        let root = unique_root("superseded-save-as");
        let first_target = root.join("first-target.mdp");
        let final_target = root.join("final-target.mdp");
        let mut state = test_state(&root);
        let first_id = state.request_project_save_as(first_target.clone()).expect("first Save As");
        let final_id = state.request_project_save_as(final_target.clone()).expect("final Save As");
        let mut completions = wait_for_completions(&state, &[first_id, final_id]);
        let mut first = completions.remove(&first_id.get()).expect("first completion");
        let final_completion = completions.remove(&final_id.get()).expect("final completion");

        assert_eq!(
            state
                .apply_persistence_completion(final_completion)
                .expect("apply final destination"),
            PersistenceCompletionDisposition::Applied
        );
        first.result = Err("obsolete destination failed".to_owned());
        assert_eq!(
            state
                .apply_persistence_completion(first)
                .expect("obsolete failure is non-authoritative"),
            PersistenceCompletionDisposition::IgnoredSupersededDestination
        );
        let session = state.authoring.as_ref().expect("session");
        assert_eq!(session.project_file(), final_target);
        assert!(!session.is_dirty());
        assert!(
            first_target.is_file(),
            "completed obsolete artifact remains an ordinary copy"
        );
        state.close_project().expect("close project");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_from_a_closed_session_cannot_clean_or_rebind_a_reopen() {
        let root = unique_root("stale-session");
        let mut state = test_state(&root);
        let old_document = state.authoring.as_ref().expect("old session").document().clone();
        let old_library =
            Arc::clone(state.authoring.as_ref().expect("old session").asset_library());
        let target = root.join("save-as.mdp");
        let request_id = state.request_project_save_as(target.clone()).expect("submit save as");

        let deadline = Instant::now() + Duration::from_secs(10);
        let completion = loop {
            if let Some(completion) = state
                .project_persistence
                .poll_completions()
                .into_iter()
                .find(|completion| completion.request_id == request_id)
            {
                break completion;
            }
            assert!(Instant::now() < deadline, "save-as did not complete");
            std::thread::sleep(Duration::from_millis(1));
        };
        completion.result.as_ref().expect("old save succeeded");
        let mut stale_failure = completion.clone();
        stale_failure.result = Err("old session write failed".to_owned());

        let reopened = AuthoringSession::new_unsaved(
            old_document,
            root.join("project.mdp"),
            root.join("runtime-reopened"),
            old_library,
        )
        .expect("reopened session");
        let reopened_id = reopened.session_id();
        state.authoring = Some(reopened);
        assert_eq!(
            state.apply_persistence_completion(stale_failure).expect("ignore stale failure"),
            PersistenceCompletionDisposition::IgnoredStaleSession
        );
        assert_eq!(
            state.apply_persistence_completion(completion).expect("ignore stale success"),
            PersistenceCompletionDisposition::IgnoredStaleSession
        );

        let current = state.authoring.as_ref().expect("current session");
        assert_eq!(current.session_id(), reopened_id);
        assert_eq!(current.project_file(), root.join("project.mdp"));
        assert!(current.is_dirty());
    }
}
