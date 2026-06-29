use super::*;
use mondrian_project::{
    load_project_archive, project_document_fingerprint, read_project_document_from_archive,
    save_project_archive, ProjectDocument, PROJECT_DOCUMENT_SCHEMA_VERSION,
};

impl AppState {
    pub(super) fn project_file_path(&self) -> anyhow::Result<&Path> {
        self.current_project_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("未打开项目文件"))
    }

    fn project_runtime_root(project_file: &Path) -> PathBuf {
        let stem = project_file.file_stem().and_then(|s| s.to_str()).unwrap_or("project");
        let mut hasher = DefaultHasher::new();
        project_file.to_string_lossy().hash(&mut hasher);
        let hash = hasher.finish();
        let dir_name = format!("mondrian_{stem}_{hash:x}");
        std::env::temp_dir().join("mondrian-runtime").join(dir_name)
    }

    fn runtime_library_root(&self) -> anyhow::Result<PathBuf> {
        let root = self
            .project_runtime_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("项目运行目录未初始化"))?;
        Ok(root.join("library"))
    }

    fn save_project_container(&self, project_data: &ProjectDocument) -> anyhow::Result<()> {
        let project_file = self.project_file_path()?.to_path_buf();
        self.save_project_container_to(project_data, project_file.as_path())
    }

    fn save_project_container_to(
        &self,
        project_data: &ProjectDocument,
        target_file: &Path,
    ) -> anyhow::Result<()> {
        let started_at = std::time::Instant::now();
        let runtime_library_root = self.runtime_library_root()?;
        let db_path = runtime_library_root.join("index.db");

        save_project_archive(project_data, db_path.as_path(), target_file)?;

        if ui_diag_enabled() {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            if elapsed_ms >= ui_diag_slow_threshold_ms() {
                tracing::warn!("[ui-diag] save_project_container slow: {}ms", elapsed_ms);
            }
        }
        Ok(())
    }

    pub(super) fn current_project_data(&self) -> Option<ProjectDocument> {
        let sequence = self.sequence.as_ref()?;
        let project_id = self.project_id?;
        let mut proxy_mode_assets: Vec<AssetId> = self.proxy_mode_assets.iter().copied().collect();
        proxy_mode_assets.sort_by_key(|id| id.to_string());
        let active_sequence_id = self.active_sequence_id.unwrap_or(sequence.id);
        let default_sequence_id = self.default_sequence_id.unwrap_or(active_sequence_id);
        let mut sequences = self.sequences.clone();
        if let Some(active) = sequences.iter_mut().find(|seq| seq.id == active_sequence_id) {
            *active = sequence.clone();
        } else {
            sequences.push(sequence.clone());
        }
        sequences.sort_by_key(|seq| seq.name.clone());
        let collection = SequenceCollection { sequences, default_sequence_id, active_sequence_id };
        let mut meta = self
            .project_meta
            .clone()
            .unwrap_or_else(|| ProjectMeta::new(sequence.name.clone()));
        if meta.name.trim().is_empty() {
            meta.name = sequence.name.clone();
        }
        Some(ProjectDocument {
            schema_version: PROJECT_DOCUMENT_SCHEMA_VERSION,
            project_id,
            document_revision: self.project_document_revision.max(1),
            meta,
            sequences: collection,
            settings: self.project_settings.clone(),
            proxy_mode_assets,
        })
    }

    fn autosave_root(&self) -> anyhow::Result<PathBuf> {
        let runtime_root = self
            .project_runtime_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("项目运行目录未初始化"))?;
        Ok(runtime_root.join("autosave"))
    }

    pub(super) fn autosave_manifest_path(runtime_root: &Path) -> PathBuf {
        runtime_root.join("autosave").join("manifest.json")
    }

    fn load_autosave_manifest(
        runtime_root: &Path,
        project_file: &Path,
    ) -> anyhow::Result<AutosaveManifest> {
        let manifest_path = Self::autosave_manifest_path(runtime_root);
        if !manifest_path.exists() {
            let mut manifest = AutosaveManifest {
                project_file: project_file.to_path_buf(),
                snapshots: Vec::new(),
            };
            manifest.normalize();
            return Ok(manifest);
        }

        let bytes = fs::read(&manifest_path)?;
        let mut manifest = serde_json::from_slice::<AutosaveManifest>(&bytes)?;
        if manifest.project_file.as_os_str().is_empty() {
            manifest.project_file = project_file.to_path_buf();
        }
        manifest.normalize();
        Ok(manifest)
    }

    pub fn write_autosave_snapshot(
        &self,
        max_recovery_points: usize,
        retention_days: u32,
    ) -> anyhow::Result<PathBuf> {
        let project_file = self.project_file_path()?.to_path_buf();
        let data = self
            .current_project_data()
            .ok_or_else(|| anyhow::anyhow!("当前无可自动保存的项目"))?;

        let autosave_root = self.autosave_root()?;
        fs::create_dir_all(&autosave_root)?;

        let runtime_root = self
            .project_runtime_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("项目运行目录未初始化"))?;
        let mut manifest = Self::load_autosave_manifest(runtime_root, project_file.as_path())
            .unwrap_or(AutosaveManifest {
                project_file: project_file.clone(),
                snapshots: Vec::new(),
            });

        let saved_at = unix_now_ms();
        let autosave_file = autosave_root.join(format!("project-{saved_at}.autosave.mdp"));
        self.save_project_container_to(&data, autosave_file.as_path())?;

        manifest.project_file = project_file;
        manifest.snapshots.push(AutosaveSnapshotEntry {
            file: autosave_file.clone(),
            saved_at_unix_ms: saved_at,
        });
        apply_autosave_retention(
            &mut manifest,
            max_recovery_points.max(1),
            retention_days.max(1),
        );
        manifest.normalize();

        write_json_atomic(
            Self::autosave_manifest_path(runtime_root).as_path(),
            &manifest,
        )?;
        Ok(autosave_file)
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

        self.save_project_file()?;
        let autosave_root = Self::project_runtime_root(project_file.as_path()).join("autosave");
        let _ = fs::remove_dir_all(autosave_root);
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
        self.sequence.is_some() && self.current_project_path.is_some()
    }

    pub(crate) fn has_unsaved_project_changes(&self) -> bool {
        if !self.has_open_project() {
            return false;
        }

        let Some(current) = self.current_project_data() else {
            return false;
        };

        let current_fingerprint = match project_document_fingerprint(current) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!("计算当前项目指纹失败，按未保存处理: {err}");
                return true;
            }
        };

        let project_file = match self.project_file_path() {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("读取当前项目路径失败，按未保存处理: {err}");
                return true;
            }
        };

        let saved = match read_project_document_from_archive(project_file) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!("读取磁盘项目数据失败，按未保存处理: {err}");
                return true;
            }
        };

        let saved_fingerprint = match project_document_fingerprint(saved) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!("计算磁盘项目指纹失败，按未保存处理: {err}");
                return true;
            }
        };

        current_fingerprint != saved_fingerprint
    }

    fn open_project_archive(
        &mut self,
        project_file: PathBuf,
        archive_file: &Path,
    ) -> anyhow::Result<()> {
        if let Some(prev_runtime) = self.project_runtime_dir.as_ref() {
            let _ = fs::remove_dir_all(prev_runtime);
        }

        let runtime_root = Self::project_runtime_root(&project_file);
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }

        let library_root = runtime_root.join("library");
        let saved = load_project_archive(archive_file, library_root.as_path())?;

        let project_sequences = saved.sequences;
        project_sequences.validate_nested_sequences()?;
        let active_sequence = project_sequences
            .active()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("项目缺少活动序列"))?;

        self.sequence = Some(active_sequence);
        self.sequences = project_sequences.sequences;
        self.active_sequence_id = Some(project_sequences.active_sequence_id);
        self.default_sequence_id = Some(project_sequences.default_sequence_id);
        self.sequence_navigation_stack.clear();
        self.project_id = Some(saved.project_id);
        self.project_meta = Some(saved.meta);
        self.project_document_revision = saved.document_revision.max(1);
        self.current_project_path = Some(project_file.clone());
        self.project_runtime_dir = Some(runtime_root.clone());
        self.proxy_mode_assets = saved.proxy_mode_assets.into_iter().collect();
        self.project_settings = saved.settings;
        self.playback = PlaybackState::Stopped;
        self.dragging_asset = None;
        self.cmd_history = mondrian_timeline::command::CommandHistory::new(200);
        self.ensure_minimum_tracks();

        self.asset_library = Some(AssetLibrary::open(library_root)?);
        Ok(())
    }

    pub fn open_project_file(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        self.open_project_archive(project_file.clone(), project_file.as_path())
    }

    pub fn save_project_file_as(&mut self, target_file: PathBuf) -> anyhow::Result<()> {
        let target_file = super::ensure_project_extension(target_file);
        let previous = self.current_project_path.clone();
        self.current_project_path = Some(target_file.clone());
        if let Err(err) = self.save_project_file() {
            self.current_project_path = previous;
            return Err(err);
        }
        Ok(())
    }

    pub fn save_project_file(&mut self) -> anyhow::Result<()> {
        let next_revision = self.project_document_revision.saturating_add(1).max(1);
        let Some(mut data) = self.current_project_data() else {
            return Ok(());
        };
        data.document_revision = next_revision;
        data.meta.touch();

        self.save_project_container(&data)?;
        self.project_id = Some(data.project_id);
        self.project_meta = Some(data.meta);
        self.project_document_revision = data.document_revision;
        Ok(())
    }

    pub fn ensure_minimum_tracks(&mut self) {
        if let Some(seq) = self.sequence.as_mut() {
            if seq.video_tracks.is_empty() {
                seq.video_tracks.push(mondrian_timeline::track::Track::new_video("V1"));
            }
            if seq.audio_tracks.is_empty() {
                seq.audio_tracks.push(mondrian_timeline::track::Track::new_audio("A1"));
            }
            seq.normalize_track_names();
        }
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
        settings.validate()?;

        if let Some(parent) = project_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut sequence = Sequence::new(name);
        sequence.settings = settings;
        sequence.playhead = TimeCode::new(0, sequence.time_base());

        let runtime_root = Self::project_runtime_root(&project_file);
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }
        fs::create_dir_all(runtime_root.join("library"))?;

        self.sequence = Some(sequence);
        self.sequences = self.sequence.iter().cloned().collect();
        self.active_sequence_id = self.sequence.as_ref().map(|seq| seq.id);
        self.default_sequence_id = self.active_sequence_id;
        self.sequence_navigation_stack.clear();
        self.project_id = Some(ProjectId::new());
        self.project_meta = Some(ProjectMeta::new(name));
        self.project_document_revision = 0;
        self.current_project_path = Some(project_file.clone());
        self.project_runtime_dir = Some(runtime_root.clone());
        self.project_settings = project_settings;
        self.playback = PlaybackState::Stopped;
        self.cmd_history = mondrian_timeline::command::CommandHistory::new(200);
        self.proxy_mode_assets.clear();

        let library_root = runtime_root.join("library");
        let library = AssetLibrary::open(library_root)?;
        library.clear_assets()?;
        self.asset_library = Some(library);

        self.save_project_file()?;
        Ok(())
    }

    pub fn save_project(&mut self) -> anyhow::Result<()> {
        self.save_project_file()
    }
}
