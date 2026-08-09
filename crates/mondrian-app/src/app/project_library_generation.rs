//! Immutable filesystem generations for the project-local Asset Library.
//!
//! A live SQLite database directory is never renamed, replaced, or removed.
//! Project open/recovery prepares a fresh direct child of the leased runtime,
//! opens SQLite only after archive extraction has completed, and then installs
//! the resulting `Arc<AssetLibrary>`. Retired generations remain named until
//! the last strong library reference has disappeared.

use super::project_runtime::{
    create_ephemeral_owned_runtime_child_directory, remove_owned_runtime_child_directory_if_exists,
    ProjectRuntimeLease,
};
use mondrian_assets::AssetLibrary;
use mondrian_core::ProjectId;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

const LIBRARY_GENERATION_PREFIX: &str = "library-generation-";
const LEGACY_LIBRARY_DIRECTORY: &str = "library";
const LEGACY_LIBRARY_STAGING_PREFIX: &str = ".library-open-";
const LEGACY_LIBRARY_BACKUP_PREFIX: &str = ".library-backup-";

/// Process-local liveness evidence for every library generation opened here.
///
/// This is not ownership authority or a cache: the runtime lease remains the
/// only filesystem authority. The weak registry only prevents a leased orphan
/// sweep from unlinking a SQLite directory while an `AssetLibrary` Arc still
/// exists outside an uncommitted candidate.
static OPEN_PROJECT_LIBRARIES: OnceLock<Mutex<HashMap<PathBuf, Weak<AssetLibrary>>>> =
    OnceLock::new();

/// A unique runtime directory that has not yet been opened as a live library.
///
/// Dropping an uninstalled candidate removes it under the same runtime lease.
/// A successful [`Self::commit`] permanently disables candidate cleanup after
/// the caller has built the replacement Authoring Session.
pub(super) struct ProjectLibraryGenerationCandidate {
    lease: Arc<ProjectRuntimeLease>,
    root: PathBuf,
    opened_library: Option<Weak<AssetLibrary>>,
    installed: bool,
}

impl ProjectLibraryGenerationCandidate {
    /// Allocate one fresh, owner-validated direct runtime child.
    pub(super) fn create(lease: Arc<ProjectRuntimeLease>) -> Result<Self, String> {
        lease.validate()?;
        let root = lease.runtime_root().join(format!(
            "{LIBRARY_GENERATION_PREFIX}{}",
            uuid::Uuid::new_v4()
        ));
        create_ephemeral_owned_runtime_child_directory(&lease, &root)?;
        Ok(Self {
            lease,
            root,
            opened_library: None,
            installed: false,
        })
    }

    /// Directory into which a validated archive may extract its SQLite snapshot.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Open the completed generation without ever renaming its directory.
    ///
    /// The candidate remains rollback-capable until [`Self::commit`]. On a
    /// later Session-construction failure, local `Arc` values drop before the
    /// candidate and the unopened generation can still be removed safely.
    pub(super) fn open(&mut self) -> mondrian_core::Result<Arc<AssetLibrary>> {
        if let Some(library) = self.opened_library.as_ref().and_then(Weak::upgrade) {
            return Ok(library);
        }
        let library = AssetLibrary::open(self.root.clone())?;
        self.opened_library = Some(Arc::downgrade(&library));
        open_project_libraries().insert(self.root.clone(), Arc::downgrade(&library));
        Ok(library)
    }

    /// Mark this generation as installed in the live Authoring Session.
    pub(super) fn commit(mut self) {
        self.installed = true;
    }
}

impl Drop for ProjectLibraryGenerationCandidate {
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        if self.opened_library.as_ref().is_some_and(|library| library.strong_count() != 0) {
            // A caller must never lose an open SQLite directory through
            // rollback cleanup. Preserve the owner-recognized orphan for the
            // next leased sweep if an Arc escaped before commit.
            tracing::warn!(
                path = %self.root.display(),
                "preserving an uninstalled Project library generation while a live AssetLibrary reference exists"
            );
            return;
        }
        open_project_libraries().remove(&self.root);
        if let Err(error) = remove_owned_runtime_child_directory_if_exists(&self.lease, &self.root)
        {
            tracing::warn!(
                path = %self.root.display(),
                %error,
                "failed to remove an uninstalled Project library generation"
            );
        }
    }
}

/// Weak lifetime evidence for one library generation replaced by another.
pub(super) struct RetiredProjectLibraryGeneration {
    lease: Arc<ProjectRuntimeLease>,
    runtime_root: PathBuf,
    root: PathBuf,
    library: Weak<AssetLibrary>,
}

impl RetiredProjectLibraryGeneration {
    /// Capture a live project library before its owning Session is replaced.
    ///
    /// Non-production test libraries outside the leased runtime are ignored.
    pub(super) fn capture(
        lease: Arc<ProjectRuntimeLease>,
        library: &Arc<AssetLibrary>,
    ) -> Option<Self> {
        let runtime_root = lease.runtime_root();
        let database_path = library.database_path();
        let root = database_path.parent()?.to_path_buf();
        if !library_directory_belongs_to_runtime(runtime_root, &root)
            || !is_managed_library_directory(&root)
        {
            return None;
        }
        let runtime_root = runtime_root.to_path_buf();
        Some(Self {
            lease,
            runtime_root,
            root,
            library: Arc::downgrade(library),
        })
    }

    fn is_live(&self) -> bool {
        self.library.strong_count() != 0
    }

    fn reusable_lease(
        &self,
        project_id: ProjectId,
        runtime_root: Option<&Path>,
    ) -> Option<Arc<ProjectRuntimeLease>> {
        (self.lease.project_id() == project_id
            && runtime_root.is_none_or(|root| self.runtime_root == root))
        .then(|| Arc::clone(&self.lease))
    }
}

/// Reuse process-local authority retained solely for a still-live old library.
///
/// This lets the owning App replace or reopen the same logical Project while
/// an external analysis/UI consumer is finishing with a prior immutable
/// generation. Another process remains excluded until every such consumer is
/// gone and the retired record is collected.
pub(super) fn retained_project_runtime_lease(
    retired: &[RetiredProjectLibraryGeneration],
    project_id: ProjectId,
    runtime_root: Option<&Path>,
) -> Option<Arc<ProjectRuntimeLease>> {
    retired
        .iter()
        .find_map(|generation| generation.reusable_lease(project_id, runtime_root))
}

/// Remove retired generations whose final strong `AssetLibrary` reference is gone.
pub(super) fn collect_retired_project_libraries(
    retired: &mut Vec<RetiredProjectLibraryGeneration>,
) {
    retired.retain(|generation| {
        if generation.is_live() {
            return true;
        }
        match remove_owned_runtime_child_directory_if_exists(&generation.lease, &generation.root) {
            Ok(()) => false,
            Err(error) => {
                tracing::warn!(
                    path = %generation.root.display(),
                    %error,
                    "failed to collect a retired Project library generation"
                );
                true
            }
        }
    });
}

/// Remove abandoned library generations while preserving every live local Arc.
///
/// Callers must first quiesce persistence for the active Authoring Session.
/// The runtime lease excludes other processes; `protected` contains the active
/// library plus weakly tracked retired generations that still have strong
/// references in Window/analysis execution domains.
pub(super) fn sweep_orphaned_project_libraries(
    lease: &ProjectRuntimeLease,
    protected: impl IntoIterator<Item = PathBuf>,
) -> Result<(), String> {
    lease.validate()?;
    let mut protected = protected.into_iter().collect::<BTreeSet<_>>();
    {
        let mut opened = open_project_libraries();
        opened.retain(|path, library| {
            let live = library.strong_count() != 0;
            if live && library_directory_belongs_to_runtime(lease.runtime_root(), path) {
                protected.insert(path.clone());
            }
            live
        });
    }
    let entries = std::fs::read_dir(lease.runtime_root())
        .map_err(|error| format!("failed to enumerate Project runtime libraries: {error}"))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to inspect Project runtime entry: {error}"))?;
        let path = entry.path();
        let canonical_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if protected.contains(&path)
            || protected.contains(&canonical_path)
            || !is_managed_library_directory(&path)
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect Project runtime entry type: {error}"))?;
        if file_type.is_symlink() || !file_type.is_dir() {
            return Err(format!(
                "managed Project library entry is not a direct directory: {}",
                path.display()
            ));
        }
        remove_owned_runtime_child_directory_if_exists(lease, &path)?;
        open_project_libraries().remove(&path);
    }
    Ok(())
}

/// Return every tracked directory that must not be swept yet.
pub(super) fn protected_project_library_paths(
    active: Option<&Arc<AssetLibrary>>,
    runtime_root: &Path,
    retired: &[RetiredProjectLibraryGeneration],
) -> Vec<PathBuf> {
    let mut protected = retired
        .iter()
        .filter(|generation| generation.runtime_root == runtime_root && generation.is_live())
        .map(|generation| generation.root.clone())
        .collect::<Vec<_>>();
    if let Some(active) = active {
        let database_path = active.database_path();
        if let Some(root) = database_path
            .parent()
            .filter(|root| library_directory_belongs_to_runtime(runtime_root, root))
        {
            protected.push(root.to_path_buf());
        }
    }
    protected
}

fn is_managed_library_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name == LEGACY_LIBRARY_DIRECTORY
        || name.starts_with(LIBRARY_GENERATION_PREFIX)
        || name.starts_with(LEGACY_LIBRARY_STAGING_PREFIX)
        || name.starts_with(LEGACY_LIBRARY_BACKUP_PREFIX)
}

/// Whether `library_root` is a direct child of the leased runtime root.
///
/// `AssetLibrary` freezes its root with `canonicalize` while the lease keeps
/// the caller-supplied spelling. On macOS the system temp directory resolves
/// through a `/var` -> `/private/var` symlink, so compare the resolved forms.
fn library_directory_belongs_to_runtime(runtime_root: &Path, library_root: &Path) -> bool {
    match (library_root.parent(), std::fs::canonicalize(runtime_root)) {
        (Some(parent), Ok(resolved)) => parent == resolved,
        _ => false,
    }
}

fn open_project_libraries() -> MutexGuard<'static, HashMap<PathBuf, Weak<AssetLibrary>>> {
    OPEN_PROJECT_LIBRARIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::project_runtime::claim_project_runtime_lease_for_test;
    use mondrian_core::ProjectId;

    fn unique_root(_label: &str) -> PathBuf {
        static NEXT_ROOT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "mg-{:x}-{:x}",
            std::process::id(),
            NEXT_ROOT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn candidate_failure_removes_unopened_generation() {
        let root = unique_root("candidate-cleanup");
        let project_file = root.join("project.mdp");
        let lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            ProjectId::new(),
        )
        .expect("runtime lease");
        let generation = ProjectLibraryGenerationCandidate::create(Arc::clone(&lease))
            .expect("library generation");
        let generation_root = generation.root().to_path_buf();
        assert!(generation_root.is_dir());

        drop(generation);

        assert!(!generation_root.exists());
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn candidate_drop_never_removes_a_generation_with_an_escaped_library_arc() {
        let root = unique_root("candidate-open-arc");
        let project_file = root.join("project.mdp");
        let lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            ProjectId::new(),
        )
        .expect("runtime lease");
        let mut generation = ProjectLibraryGenerationCandidate::create(Arc::clone(&lease))
            .expect("library generation");
        let generation_root = generation.root().to_path_buf();
        let escaped = generation.open().expect("open candidate library");

        drop(generation);

        assert!(
            generation_root.is_dir(),
            "rollback must not unlink an open SQLite generation"
        );
        sweep_orphaned_project_libraries(&lease, []).expect("sweep live escaped orphan");
        assert!(
            generation_root.is_dir(),
            "orphan sweep must retain an open SQLite generation"
        );
        drop(escaped);
        sweep_orphaned_project_libraries(&lease, []).expect("collect escaped orphan");
        assert!(!generation_root.exists());
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retired_generation_is_collected_only_after_the_last_library_arc() {
        let root = unique_root("retired-lifetime");
        let project_file = root.join("project.mdp");
        let lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            ProjectId::new(),
        )
        .expect("runtime lease");
        let mut generation = ProjectLibraryGenerationCandidate::create(Arc::clone(&lease))
            .expect("library generation");
        let generation_root = generation.root().to_path_buf();
        let library = generation.open().expect("open library");
        generation.commit();
        let retained = Arc::clone(&library);
        let mut retired =
            vec![
                RetiredProjectLibraryGeneration::capture(Arc::clone(&lease), &library)
                    .expect("managed retired generation"),
            ];
        drop(library);

        collect_retired_project_libraries(&mut retired);
        assert_eq!(retired.len(), 1);
        assert!(generation_root.is_dir());

        drop(retained);
        collect_retired_project_libraries(&mut retired);
        assert!(retired.is_empty());
        assert!(!generation_root.exists());

        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_sweep_preserves_explicitly_protected_generation() {
        let root = unique_root("orphan-sweep");
        let project_file = root.join("project.mdp");
        let lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            ProjectId::new(),
        )
        .expect("runtime lease");
        let protected_root =
            lease.runtime_root().join(format!("{LIBRARY_GENERATION_PREFIX}protected"));
        let orphan_root = lease.runtime_root().join(format!("{LIBRARY_GENERATION_PREFIX}orphan"));
        create_ephemeral_owned_runtime_child_directory(&lease, &protected_root)
            .expect("protected generation");
        create_ephemeral_owned_runtime_child_directory(&lease, &orphan_root)
            .expect("orphan generation");

        sweep_orphaned_project_libraries(&lease, [protected_root.clone()]).expect("sweep orphan");

        assert!(protected_root.is_dir());
        assert!(!orphan_root.exists());
        remove_owned_runtime_child_directory_if_exists(&lease, &protected_root)
            .expect("remove protected fixture");
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }
}
