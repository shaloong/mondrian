//! Project archive preparation outside the UI thread.
//!
//! A candidate owns only validated author data and a rollback-capable library
//! generation. AppState alone creates and installs the Authoring Session.

use super::project_library_generation::{
    sweep_orphaned_project_libraries, ProjectLibraryGenerationCandidate,
};
use super::project_recovery::cleanup_recovery_runtime_artifacts;
use super::project_runtime::{
    claim_project_runtime, claim_project_runtime_sharing_logical_authority,
    project_runtime_root_for_project, ProjectRuntimeLease,
};
use anyhow::Context;
use mondrian_assets::AssetLibrary;
use mondrian_core::ProjectId;
#[cfg(any(test, feature = "validation"))]
use mondrian_core::SequenceId;
use mondrian_project::{LoadedProjectArchive, PreparedProjectArchive, ProjectArchiveReadBudget};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectArchiveInstallPolicy {
    CanonicalProduct,
    RecoveredProduct,
    PortableImport,
    #[cfg(any(test, feature = "validation"))]
    #[cfg_attr(
        not(windows),
        expect(
            dead_code,
            reason = "Exact immutable fixture admission requires a native provider."
        )
    )]
    ExactEnduranceFixture {
        expected_sequence_id: SequenceId,
    },
}

impl ProjectArchiveInstallPolicy {
    pub(super) const fn opens_canonical_project(self) -> bool {
        !matches!(self, Self::RecoveredProduct | Self::PortableImport)
    }

    pub(super) const fn repairs_minimum_tracks(self) -> bool {
        matches!(self, Self::CanonicalProduct | Self::RecoveredProduct)
    }
}

pub(super) fn available_package_import_destination(package_root: &Path) -> anyhow::Result<PathBuf> {
    let stem = package_root.file_stem().context("portable package has no name")?;
    let parent = package_root.parent().context("portable package has no parent")?;
    for index in 0..1000 {
        let candidate = if index == 0 {
            package_root.with_extension("mdp")
        } else {
            parent.join(format!("{}-imported-{index}.mdp", stem.to_string_lossy()))
        };
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("no available sibling Project filename for portable package")
}

/// Reuse only kernel-backed authority retained by this App's live generations.
pub(super) fn claim_or_reuse_runtime(
    leases: &[Arc<ProjectRuntimeLease>],
    project_file: &Path,
    project_id: ProjectId,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    for lease in leases.iter().filter(|lease| lease.project_id() == project_id) {
        if lease.allocation_target_matches(project_file)? {
            lease.validate()?;
            lease.retain_publication_target(project_file)?;
            return Ok(Arc::clone(lease));
        }
    }
    let runtime_root = project_runtime_root_for_project(project_file, project_id)?;
    if let Some(lease) = leases
        .iter()
        .find(|lease| lease.project_id() == project_id && lease.runtime_root() == runtime_root)
    {
        lease.validate()?;
        lease.retain_publication_target(project_file)?;
        return Ok(Arc::clone(lease));
    }
    match leases.iter().find(|lease| lease.project_id() == project_id) {
        Some(authority) => {
            claim_project_runtime_sharing_logical_authority(project_file, project_id, authority)
        }
        None => claim_project_runtime(project_file, project_id),
    }
}

pub(super) struct PreparedProductProjectOpen {
    // Field drop order matters: release SQLite before rolling back its directory.
    pub(super) asset_library: Arc<AssetLibrary>,
    pub(super) library_generation: ProjectLibraryGenerationCandidate,
    pub(super) loaded: LoadedProjectArchive,
    pub(super) project_file: PathBuf,
    pub(super) install_policy: ProjectArchiveInstallPolicy,
    pub(super) runtime_lease: Arc<ProjectRuntimeLease>,
}

impl PreparedProductProjectOpen {
    pub(super) fn prepare(
        project_file: PathBuf,
        prepared: PreparedProjectArchive<'_>,
        install_policy: ProjectArchiveInstallPolicy,
        package_bindings: Option<&BTreeMap<PathBuf, PathBuf>>,
        runtime_lease: Arc<ProjectRuntimeLease>,
        mut library_generation: ProjectLibraryGenerationCandidate,
    ) -> anyhow::Result<Self> {
        let prepared_project_id = prepared.project_id();
        if runtime_lease.project_id() != prepared_project_id {
            anyhow::bail!("Project runtime lease belongs to another Project");
        }
        runtime_lease.validate().map_err(anyhow::Error::msg)?;
        let mut loaded = prepared.load_into(library_generation.root())?;
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
        #[cfg(any(test, feature = "validation"))]
        if let ProjectArchiveInstallPolicy::ExactEnduranceFixture { expected_sequence_id } =
            install_policy
        {
            super::project_lifecycle::validate_exact_endurance_project(
                &loaded,
                expected_sequence_id,
            )?;
        }
        let asset_library = library_generation.open()?;
        if let Some(bindings) = package_bindings {
            super::project_packaging::rebind_document_resources(&mut loaded.document, bindings)?;
            asset_library.rebind_verified_package_sources(
                &bindings
                    .iter()
                    .map(|(source, target)| (source.clone(), target.clone()))
                    .collect::<Vec<_>>(),
            )?;
            super::project_packaging::ensure_package_asset_sources_rebound(
                &asset_library,
                bindings,
            )?;
        }
        Ok(Self {
            asset_library,
            library_generation,
            loaded,
            project_file,
            install_policy,
            runtime_lease,
        })
    }
}

/// Immutable preparation input. It contains no App or authoring authority.
pub(super) struct ProjectOpenPreparation {
    pub(super) path: PathBuf,
    pub(super) runtime_leases: Vec<Arc<ProjectRuntimeLease>>,
    pub(super) protected_libraries: Vec<PathBuf>,
}

impl ProjectOpenPreparation {
    pub(super) fn prepare(
        self,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<PreparedProductProjectOpen> {
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("Project open was cancelled before preparation");
        }
        let package_root =
            if self.path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mdpkg")) {
                Some(self.path.clone())
            } else if self
                .path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("project.mdp"))
            {
                self.path
                    .parent()
                    .filter(|parent| {
                        parent.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mdpkg"))
                    })
                    .map(Path::to_path_buf)
            } else {
                None
            };
        let (project_file, archive_file, policy, bindings) = match package_root {
            Some(root) => {
                let manifest = super::project_packaging::verify_portable_project_package(&root)?;
                let root = mondrian_assets::canonical_native_path(&root)?;
                let bindings = super::project_packaging::package_source_bindings(&root, &manifest)?;
                (
                    available_package_import_destination(&root)?,
                    root.join("project.mdp"),
                    ProjectArchiveInstallPolicy::PortableImport,
                    Some(bindings),
                )
            }
            None => (
                self.path.clone(),
                self.path,
                ProjectArchiveInstallPolicy::CanonicalProduct,
                None,
            ),
        };
        let mut archive = fs::File::open(archive_file)?;
        let prepared = PreparedProjectArchive::from_open_file(
            &mut archive,
            ProjectArchiveReadBudget::default(),
        )?;
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("Project open was cancelled before runtime acquisition");
        }
        let runtime_lease =
            claim_or_reuse_runtime(&self.runtime_leases, &project_file, prepared.project_id())
                .map_err(anyhow::Error::msg)?;
        cleanup_recovery_runtime_artifacts(&runtime_lease).map_err(anyhow::Error::msg)?;
        // The shared weak library registry protects all live generations,
        // including the frozen current Session and external UI/analysis Arcs.
        sweep_orphaned_project_libraries(&runtime_lease, self.protected_libraries)
            .map_err(anyhow::Error::msg)?;
        let generation = ProjectLibraryGenerationCandidate::create(Arc::clone(&runtime_lease))
            .map_err(anyhow::Error::msg)?;
        let candidate = PreparedProductProjectOpen::prepare(
            project_file,
            prepared,
            policy,
            bindings.as_ref(),
            runtime_lease,
            generation,
        )?;
        if cancelled.load(Ordering::Acquire) {
            // Candidate field order closes SQLite before owned-directory cleanup.
            anyhow::bail!("Project open was cancelled after preparation");
        }
        Ok(candidate)
    }
}

/// Sole non-blocking consumer of one bounded worker result.
pub(super) struct ProjectOpenTicket {
    pub(super) result_rx: Receiver<Result<PreparedProductProjectOpen, String>>,
    pub(super) cancelled: Arc<AtomicBool>,
}

impl ProjectOpenTicket {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn poll(
        &self,
    ) -> Result<Option<Result<PreparedProductProjectOpen, String>>, String> {
        match self.result_rx.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("Project preparation worker disconnected".to_owned())
            }
        }
    }
}

impl Drop for ProjectOpenTicket {
    fn drop(&mut self) {
        self.cancel();
    }
}
