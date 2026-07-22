use super::project_persistence::{ProjectPersistenceCompletion, ProjectPersistenceRequestId};
use super::*;
use mondrian_project::{load_project_archive, ProjectDocument};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistenceCompletionDisposition {
    Applied,
    IgnoredStaleSession,
}

impl AppState {
    fn project_runtime_root(project_file: &Path) -> PathBuf {
        let stem = project_file.file_stem().and_then(|s| s.to_str()).unwrap_or("project");
        let mut hasher = DefaultHasher::new();
        project_file.to_string_lossy().hash(&mut hasher);
        let hash = hasher.finish();
        let dir_name = format!("mondrian_{stem}_{hash:x}");
        std::env::temp_dir().join("mondrian-runtime").join(dir_name)
    }

    pub(super) fn autosave_manifest_path(runtime_root: &Path) -> PathBuf {
        runtime_root.join("autosave").join("manifest.json")
    }

    pub fn open_project_from_autosave_snapshot(
        &mut self,
        project_file: PathBuf,
        autosave_file: PathBuf,
    ) -> anyhow::Result<()> {
        if !autosave_file.exists() {
            anyhow::bail!("未找到自动保存文件：{}", autosave_file.display());
        }

        let staged = std::env::temp_dir().join(format!(
            "mondrian-autosave-recover-{}-{}.mdp",
            std::process::id(),
            unix_now_ms()
        ));
        fs::copy(&autosave_file, &staged)?;

        let open_result = self.open_project_archive(project_file.clone(), staged.as_path());
        let _ = fs::remove_file(&staged);
        open_result?;

        // A recovered snapshot is intentionally dirty until the user explicitly
        // saves it. Keep all recovery points until that durable save succeeds.
        Ok(())
    }

    pub fn open_project_from_autosave(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        let candidate = discover_crash_recovery_candidates()
            .into_iter()
            .find(|c| c.project_file == project_file)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "未找到自动保存文件：{}",
                    Self::autosave_manifest_path(
                        Self::project_runtime_root(project_file.as_path()).as_path()
                    )
                    .display()
                )
            })?;

        self.open_project_from_autosave_snapshot(project_file, candidate.autosave_file)
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
    ) -> anyhow::Result<()> {
        let runtime_root = Self::project_runtime_root(&project_file);
        let library_root = runtime_root.join("library");
        if library_root.exists() {
            fs::remove_dir_all(&library_root)?;
        }
        let loaded = load_project_archive(archive_file, library_root.as_path())?;
        if loaded.library_schema_version > mondrian_assets::ASSET_LIBRARY_SCHEMA_VERSION {
            anyhow::bail!(
                "项目素材库 schema v{} 高于当前支持的 v{}",
                loaded.library_schema_version,
                mondrian_assets::ASSET_LIBRARY_SCHEMA_VERSION
            );
        }
        let asset_library = AssetLibrary::open(library_root)?;
        let session = if archive_file == project_file.as_path() {
            AuthoringSession::open_saved(
                loaded.document,
                project_file.clone(),
                runtime_root,
                asset_library,
            )
        } else {
            AuthoringSession::new_unsaved(
                loaded.document,
                project_file.clone(),
                runtime_root,
                asset_library,
            )
        }
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let project_id = session.project_id();
        self.authoring = Some(session);
        self.autosave_in_flight_request = None;
        self.proxy_generation.bind_project(Some(project_id));
        self.stop();
        self.settle_preview_access_source();
        self.dragging_asset = None;
        self.ensure_minimum_tracks();
        Ok(())
    }

    pub fn open_project_file(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        self.open_project_archive(project_file.clone(), project_file.as_path())
    }

    /// Enqueue a manual save and return without waiting for filesystem I/O.
    pub fn request_project_save(&mut self) -> anyhow::Result<ProjectPersistenceRequestId> {
        self.submit_project_save(None)
    }

    /// Enqueue Save As and return without waiting for filesystem I/O.
    pub fn request_project_save_as(
        &mut self,
        target_file: PathBuf,
    ) -> anyhow::Result<ProjectPersistenceRequestId> {
        self.submit_project_save(Some(super::ensure_project_extension(target_file)))
    }

    fn submit_project_save(
        &mut self,
        target_file: Option<PathBuf>,
    ) -> anyhow::Result<ProjectPersistenceRequestId> {
        let session =
            self.authoring.as_ref().ok_or_else(|| anyhow::anyhow!("当前没有打开的项目"))?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let target = target_file.clone().unwrap_or_else(|| session.project_file().to_path_buf());
        self.project_persistence
            .submit(
                snapshot,
                target,
                ProjectPersistencePurpose::Manual { update_project_path: target_file.is_some() },
            )
            .map_err(anyhow::Error::msg)
    }

    fn submit_autosave(
        &mut self,
        max_recovery_points: usize,
        retention_days: u32,
    ) -> anyhow::Result<(ProjectPersistenceRequestId, PathBuf)> {
        let session = self
            .authoring
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("当前无可自动保存的项目"))?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let project_file = session.project_file().to_path_buf();
        let runtime_root = session.runtime_root().to_path_buf();
        let saved_at_unix_ms = unix_now_ms();
        let autosave_file = runtime_root.join("autosave").join(format!(
            "project-{saved_at_unix_ms}-g{}.autosave.mdp",
            snapshot.generation.get()
        ));
        let request_id = self
            .project_persistence
            .submit(
                snapshot,
                autosave_file.clone(),
                ProjectPersistencePurpose::Autosave {
                    original_project_file: project_file,
                    runtime_root,
                    max_recovery_points: max_recovery_points.max(1),
                    retention_days: retention_days.max(1),
                    saved_at_unix_ms,
                },
            )
            .map_err(anyhow::Error::msg)?;
        self.autosave_in_flight_request = Some(request_id);
        self.autosave_last_requested_at = Instant::now();
        Ok((request_id, autosave_file))
    }

    /// Poll durable persistence and schedule due autosaves.
    pub fn poll_project_persistence(&mut self) -> bool {
        let completions = self.project_persistence.poll_completions();
        let mut changed = false;
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
                    ProjectPersistencePurpose::Autosave { .. },
                    Ok(PersistenceCompletionDisposition::Applied),
                )
                | (_, Ok(PersistenceCompletionDisposition::IgnoredStaleSession)) => {}
                (ProjectPersistencePurpose::Manual { .. }, Err(error)) => {
                    self.set_status_hint(format!("保存项目失败：{error}"), true);
                }
                (ProjectPersistencePurpose::Autosave { .. }, Err(error)) => {
                    self.set_status_hint(format!("自动保存失败：{error}"), true);
                }
            }
        }
        if self.maybe_request_autosave() {
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
        let same_session = self
            .authoring
            .as_ref()
            .is_some_and(|session| session.session_id() == completion.session_id);
        if !same_session {
            return Ok(PersistenceCompletionDisposition::IgnoredStaleSession);
        }
        let persisted = completion.result?;
        let Some(session) = self.authoring.as_mut() else {
            return Ok(PersistenceCompletionDisposition::IgnoredStaleSession);
        };
        match completion.purpose {
            ProjectPersistencePurpose::Manual { update_project_path } => session
                .mark_saved(
                    completion.generation,
                    persisted.document_revision,
                    persisted.asset_library_revision,
                    persisted.meta,
                    update_project_path.then_some(completion.target_file),
                )
                .map_err(|error| error.to_string())?,
            ProjectPersistencePurpose::Autosave { .. } => {
                session
                    .mark_autosaved(completion.generation, persisted.asset_library_revision)
                    .map_err(|error| error.to_string())?;
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

    fn wait_for_persistence_request(
        &mut self,
        request_id: ProjectPersistenceRequestId,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + std::time::Duration::from_secs(300);
        loop {
            for completion in self.project_persistence.poll_completions() {
                let is_requested = completion.request_id == request_id;
                let result =
                    self.apply_persistence_completion(completion).map_err(anyhow::Error::msg);
                if is_requested {
                    return match result? {
                        PersistenceCompletionDisposition::Applied => Ok(()),
                        PersistenceCompletionDisposition::IgnoredStaleSession => {
                            anyhow::bail!("项目会话在耐久保存完成前已被替换")
                        }
                    };
                }
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
        let Some(session) = self.authoring.as_mut() else {
            return;
        };
        let _ = session.edit_active_sequence("补齐基础轨道", |sequence| {
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
            ProjectSettings::default(),
        )
    }

    pub fn create_new_project_with_settings_at(
        &mut self,
        project_file: PathBuf,
        name: &str,
        settings: SequenceSettings,
        project_settings: ProjectSettings,
    ) -> anyhow::Result<()> {
        if project_file.exists() {
            anyhow::bail!("项目文件已存在：{}", project_file.display());
        }
        settings.validate_with_project_color_management(&project_settings.color_management)?;

        if let Some(parent) = project_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut sequence = Sequence::new(name);
        sequence.settings = settings;
        sequence.playhead = TimelineTime::ZERO;

        let runtime_root = Self::project_runtime_root(&project_file);
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }
        fs::create_dir_all(runtime_root.join("library"))?;

        let library_root = runtime_root.join("library");
        let library = AssetLibrary::open(library_root)?;
        library.clear_assets()?;
        let document =
            ProjectDocument::new(name, SequenceCollection::new(sequence), project_settings);
        let session = AuthoringSession::new_unsaved(document, project_file, runtime_root, library)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let project_id = session.project_id();
        self.authoring = Some(session);
        self.autosave_in_flight_request = None;
        self.proxy_generation.bind_project(Some(project_id));
        self.stop();
        self.settle_preview_access_source();

        self.save_project_file()?;
        Ok(())
    }

    pub fn save_project(&mut self) -> anyhow::Result<()> {
        self.save_project_file()
    }

    /// Atomically replace the project color engine after validating every
    /// inheriting sequence against the proposed project color contract.
    pub fn set_project_color_engine(
        &mut self,
        engine: mondrian_core::ColorEngine,
    ) -> mondrian_core::Result<()> {
        let Some(session) = self.authoring.as_mut() else {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_project_color_engine".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            });
        };
        if session.document().settings.color_management.engine == engine {
            return Ok(());
        }
        let before = session.document().clone();
        let mut after = before.clone();
        let mut next_color_management = after.settings.color_management.clone();
        next_color_management.engine = engine.clone();
        for sequence in &after.sequences.sequences {
            sequence
                .settings
                .validate_with_project_color_management(&next_color_management)?;
        }
        engine.ensure_loaded().map_err(|reason| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_project_color_engine".to_owned(),
                reason,
            }
        })?;

        after.settings.color_management.engine = engine;
        session.commit_project_snapshot("修改项目颜色引擎", before, after)?;
        self.stop();
        self.settle_preview_access_source();
        Ok(())
    }
}

#[cfg(test)]
mod persistence_lifecycle_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn unique_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mondrian-persistence-lifecycle-{name}-{}-{}",
            std::process::id(),
            unix_now_ms()
        ))
    }

    fn test_session(root: &Path) -> AuthoringSession {
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let document = ProjectDocument::new(
            "Persistence Lifecycle",
            SequenceCollection::new(Sequence::new("Sequence")),
            ProjectSettings::default(),
        );
        AuthoringSession::new_unsaved(
            document,
            root.join("project.mdp"),
            root.join("runtime"),
            library,
        )
        .expect("authoring session")
    }

    #[test]
    fn failed_autosave_releases_the_scheduler_for_retry() {
        let root = unique_root("retry");
        let runtime_root = root.join("runtime");
        fs::create_dir_all(&runtime_root).expect("runtime root");
        fs::write(runtime_root.join("autosave"), b"blocks-directory")
            .expect("blocking autosave path");
        let mut state = AppState::new();
        state.authoring = Some(test_session(&root));

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
    }

    #[test]
    fn completion_from_a_closed_session_cannot_clean_or_rebind_a_reopen() {
        let root = unique_root("stale-session");
        let mut state = AppState::new();
        state.authoring = Some(test_session(&root));
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
