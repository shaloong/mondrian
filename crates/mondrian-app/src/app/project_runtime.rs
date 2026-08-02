//! Durable ownership and live-process exclusion for one Project runtime root.
//!
//! A runtime directory is selected by both the complete SHA-256 identity of
//! the normalized Project publication target that first allocated it and the
//! durable `ProjectId`. The pair is immutable: replacing a closed Project at
//! one publication target allocates a different root and leaves the previous
//! Project payload untouched. Save As deliberately keeps the already leased
//! root and allocation identity stable; the Recovery Manifest separately owns
//! the mutable canonical Project path and recovery-point authority.
//! Recovery-bearing roots live below stable per-user state, never process
//! temporary/runtime directories. The root and owner manifest must both cross
//! typed durable publication before any payload is admitted.
//!
//! Live authority is intentionally stronger than the runtime-local
//! `session.lock`: one logical-Project lock excludes copied archives with the
//! same `ProjectId`, while publication-target locks exclude filesystem aliases.
//! Every retained kernel handle is revalidated against its namespace entry
//! before mutation, so Unix unlink-and-replace cannot silently split authority.

use mondrian_core::ProjectId;
use mondrian_storage::{
    create_durable_direct_child, ensure_durable_directory_chain, write_durable_file_atomically,
    write_durable_file_atomically_with_mode, DirectoryPublicationFailure,
    FilePublicationDurabilityUnconfirmed, FilePublicationFailure, FilePublicationMode,
    FilePublicationNamespaceIndeterminate,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as UnixMetadataExt, OpenOptionsExt as UnixOpenOptionsExt};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as WindowsOpenOptionsExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};

#[cfg(not(any(unix, windows)))]
compile_error!("Project runtime path identity requires a native Unix or Windows path encoding");

const PROJECT_PATH_IDENTITY_DOMAIN: &[u8] =
    b"mondrian.project-runtime.publication-path-identity.v2\0";
const PROJECT_RUNTIME_STATE_DIRECTORY: &str = "mondrian-project-runtime-v4";
const PROJECT_RUNTIME_DIRECTORY_PREFIX: &str = "mondrian_";
const PROJECT_RUNTIME_DIRECTORY_IDENTITY_SEPARATOR: char = '_';
const PROJECT_RUNTIME_OWNER_FILE: &str = "owner.manifest.json";
const PROJECT_RUNTIME_SESSION_LOCK_FILE: &str = "session.lock";
const PROJECT_RUNTIME_PARENT_MARKER: &str = ".mondrian-runtime-parent-v1";
const PROJECT_RUNTIME_PARENT_MARKER_BYTES: &[u8] = b"mondrian-runtime-parent-v1\n";
#[cfg(test)]
const PROJECT_RUNTIME_AUTHORITY_DIRECTORY: &str = ".authority";
const PROJECT_RUNTIME_OWNER_SCHEMA_VERSION: u32 = 3;
const SHA256_HEX_LENGTH: usize = 64;

static PROJECT_RUNTIME_OWNER_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static NEXT_PROJECT_RUNTIME_LEASE_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local identity of one exact kernel-backed runtime lease instance.
///
/// Reacquiring authority for the same logical Project creates a new identity.
/// This scalar may cross asynchronous completion boundaries without extending
/// the lifetime of the kernel handles held by [`ProjectRuntimeLease`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ProjectRuntimeLeaseId(u64);

fn next_project_runtime_lease_id() -> Result<ProjectRuntimeLeaseId, String> {
    NEXT_PROJECT_RUNTIME_LEASE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map(ProjectRuntimeLeaseId)
        .map_err(|_| "Project runtime lease identity exhausted".to_owned())
}

/// Kernel-backed exclusive authority for one live logical Project.
///
/// Cloning the surrounding [`Arc`] shares one exact lease across the App
/// Session, persistence worker, autosave/recovery publication, and directory
/// generation lifecycle. The operating system releases authority after normal
/// Drop or process termination. Project, publication-target, and `session.lock` directory
/// entries are deliberately persistent and are never interpreted as evidence
/// that a Session is alive.
pub(super) struct ProjectRuntimeLease {
    id: ProjectRuntimeLeaseId,
    runtime_root: PathBuf,
    session_lock: ExclusiveNamespaceLock,
    logical_authority: Arc<ProjectLogicalAuthority>,
}

/// Process-shared authority for one logical Project across concrete payload roots.
///
/// Filesystem copies retain `ProjectId`, so the logical lock remains unique
/// while an ordinary Open may move to another path-paired runtime root. Old
/// immutable library generations can keep their exact root lease without
/// blocking the same process from preparing the new root.
struct ProjectLogicalAuthority {
    authority_root: PathBuf,
    project_id: ProjectId,
    project_lock: ExclusiveNamespaceLock,
    publication_authorities: Mutex<BTreeMap<ProjectPathIdentity, PublicationTargetAuthority>>,
}

impl ProjectRuntimeLease {
    /// Exact process-local identity of this lease instance.
    pub(super) const fn id(&self) -> ProjectRuntimeLeaseId {
        self.id
    }

    /// Exact runtime root protected by this live lease.
    pub(super) fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    /// Durable Project identity protected by this live lease.
    pub(super) fn project_id(&self) -> ProjectId {
        self.logical_authority.project_id
    }

    /// Retain exclusive publication authority for a Project target.
    ///
    /// The guard remains in this lease until the complete Project Session is
    /// gone. This deliberately keeps both the source and every successful or
    /// attempted Save-As target excluded while queued publishers may still
    /// exist.
    pub(super) fn retain_publication_target(&self, target_file: &Path) -> Result<(), String> {
        self.logical_authority.retain_publication_target(target_file)
    }

    /// Whether this lease's immutable allocation identity names `target_file`.
    ///
    /// This is stronger than sharing a `ProjectId` or already retaining a
    /// publication lock. Save As may retain several publication targets while
    /// the runtime root remains paired with exactly the target stored in its
    /// owner manifest.
    pub(super) fn allocation_target_matches(&self, target_file: &Path) -> Result<bool, String> {
        let absolute_target = absolute_project_file(target_file)?;
        let identity = ProjectPathIdentity::from_absolute_project_file(&absolute_target)?;
        let _guard = project_runtime_owner_guard();
        validate_logical_authority_unlocked(&self.logical_authority)?;
        self.session_lock.validate_namespace_binding()?;
        let manifest = validate_runtime_owner_unlocked(self.runtime_root(), self.project_id())?;
        Ok(manifest.allocation_target_identity_sha256 == identity)
    }

    /// Revalidate durable owner metadata while retaining kernel authority.
    pub(super) fn validate(&self) -> Result<(), String> {
        let _guard = project_runtime_owner_guard();
        validate_live_lease_unlocked(self)
    }
}

impl ProjectLogicalAuthority {
    fn retain_publication_target(&self, target_file: &Path) -> Result<(), String> {
        let absolute_target = absolute_project_file(target_file)?;
        let identity = ProjectPathIdentity::from_absolute_project_file(&absolute_target)?;
        let _guard = project_runtime_owner_guard();
        validate_logical_authority_unlocked(self)?;
        self.retain_publication_target_unlocked(absolute_target, identity)
    }

    fn retain_publication_target_unlocked(
        &self,
        absolute_target: PathBuf,
        identity: ProjectPathIdentity,
    ) -> Result<(), String> {
        let mut publication_authorities = self.publication_authorities.lock().map_err(|_| {
            "Project publication authority registry was poisoned; refusing to weaken exclusion"
                .to_owned()
        })?;
        if let Some(authority) = publication_authorities.get_mut(&identity) {
            return authority.retain_target_path(absolute_target);
        }
        ensure_runtime_authority_directory(&self.authority_root)?;
        let authority =
            PublicationTargetAuthority::acquire(&self.authority_root, absolute_target, identity)?;
        publication_authorities.insert(identity, authority);
        Ok(())
    }
}

impl fmt::Debug for ProjectRuntimeLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectRuntimeLease")
            .field("id", &self.id)
            .field("runtime_root", &self.runtime_root)
            .field("authority_root", &self.logical_authority.authority_root)
            .field("project_id", &self.logical_authority.project_id)
            .finish_non_exhaustive()
    }
}

/// One kernel lock plus proof that its current namespace entry still names the
/// exact object held by this process.
///
/// Windows denies write/delete sharing for the authority handle. Unix `flock`
/// is advisory and an open file may still be unlinked, so every authorized
/// mutation rechecks device/inode/link identity and fails closed after any
/// namespace replacement.
struct ExclusiveNamespaceLock {
    path: PathBuf,
    file: File,
    identity: NamespaceFileIdentity,
    description: &'static str,
}

/// Kernel exclusion plus every admitted namespace route to one publication
/// entry.
///
/// The route list matters even when two spellings originally normalize to the
/// same identity. If a symlinked ancestor is retargeted later, revalidating only
/// the first spelling would let a queued Save As escape its admitted authority.
struct PublicationTargetAuthority {
    identity: ProjectPathIdentity,
    target_paths: BTreeSet<PathBuf>,
    lock: ExclusiveNamespaceLock,
}

impl PublicationTargetAuthority {
    fn acquire(
        authority_root: &Path,
        absolute_target: PathBuf,
        identity: ProjectPathIdentity,
    ) -> Result<Self, String> {
        let lock = ExclusiveNamespaceLock::acquire(
            publication_lock_path(authority_root, identity),
            "Project publication target",
        )?;
        let authority = Self {
            identity,
            target_paths: BTreeSet::from([absolute_target]),
            lock,
        };
        authority.validate()?;
        Ok(authority)
    }

    fn retain_target_path(&mut self, absolute_target: PathBuf) -> Result<(), String> {
        self.validate()?;
        let current = ProjectPathIdentity::from_absolute_project_file(&absolute_target)?;
        if current != self.identity {
            return Err(
                "Project publication target changed while its authority was being retained"
                    .to_owned(),
            );
        }
        self.target_paths.insert(absolute_target);
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        self.lock.validate_namespace_binding()?;
        if self.target_paths.is_empty() {
            return Err("Project publication authority has no target path".to_owned());
        }
        for target in &self.target_paths {
            let current = ProjectPathIdentity::from_absolute_project_file(target)?;
            if current != self.identity {
                return Err(format!(
                    "Project publication target namespace changed while Session authority was live: {}",
                    target.display()
                ));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for PublicationTargetAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicationTargetAuthority")
            .field("identity", &self.identity)
            .field("target_paths", &self.target_paths)
            .field("lock", &self.lock)
            .finish()
    }
}

impl ExclusiveNamespaceLock {
    fn acquire(path: PathBuf, description: &'static str) -> Result<Self, String> {
        let file = open_exclusive_namespace_lock(&path, description)?;
        let identity = namespace_identity_for_open_file(&file, description)?;
        let lock = Self { path, file, identity, description };
        lock.validate_namespace_binding()?;
        Ok(lock)
    }

    fn validate_namespace_binding(&self) -> Result<(), String> {
        let held = namespace_identity_for_open_file(&self.file, self.description)?;
        if held != self.identity {
            return Err(format!(
                "{} kernel handle identity changed unexpectedly",
                self.description
            ));
        }
        let named = namespace_identity_for_path(&self.path, self.description)?;
        if named != self.identity {
            return Err(format!(
                "{} namespace entry no longer names the held kernel object",
                self.description
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ExclusiveNamespaceLock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExclusiveNamespaceLock")
            .field("path", &self.path)
            .field("identity", &self.identity)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamespaceFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamespaceFileIdentity {
    volume_serial_number: u32,
    file_index: u64,
}

/// Complete domain-separated identity of one normalized publication target.
///
/// Existing parent-namespace aliases are resolved through the longest
/// canonical directory prefix. The leaf remains the directory entry replaced
/// by atomic publication rather than the referent of a leaf symlink.
/// Unresolved Windows components are compared case-insensitively and normalize
/// Win32-ignored trailing dots/spaces conservatively. A separate ProjectId
/// authority still excludes copied aliases of the same logical Project even
/// when they intentionally publish through different entries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ProjectPathIdentity([u8; 32]);

impl ProjectPathIdentity {
    fn from_project_file(project_file: &Path) -> Result<Self, String> {
        let absolute = absolute_project_file(project_file)?;
        Self::from_absolute_project_file(&absolute)
    }

    fn from_absolute_project_file(absolute: &Path) -> Result<Self, String> {
        let normalized = normalize_publication_path(absolute)?;
        Ok(Self::from_normalized_native_path(&normalized))
    }

    fn from_normalized_native_path(path: &Path) -> Self {
        let mut digest = Sha256::new();
        digest.update(PROJECT_PATH_IDENTITY_DOMAIN);

        #[cfg(unix)]
        {
            let bytes = path.as_os_str().as_bytes();
            digest.update(b"unix-bytes\0");
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }

        #[cfg(windows)]
        {
            let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
            digest.update(b"windows-invariant-casefolded-utf16\0");
            for item in char::decode_utf16(units) {
                match item {
                    Ok(character) => {
                        for folded in character.to_uppercase() {
                            digest.update([0]);
                            digest.update((folded as u32).to_le_bytes());
                        }
                    }
                    Err(error) => {
                        // Ill-formed native paths remain representable and
                        // distinct. Valid Unicode characters are case-folded;
                        // unpaired surrogate units retain their exact value.
                        digest.update([1]);
                        digest.update(u32::from(error.unpaired_surrogate()).to_le_bytes());
                    }
                }
            }
        }

        Self(digest.finalize().into())
    }

    fn parse_hex(value: &str) -> Result<Self, String> {
        if value.len() != SHA256_HEX_LENGTH
            || value
                .bytes()
                .any(|byte| !(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return Err("Project runtime path identity is not canonical SHA-256 hex".to_owned());
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (decode_hex_nibble(pair[0])? << 4) | decode_hex_nibble(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Debug for ProjectPathIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ProjectPathIdentity").field(&self.to_string()).finish()
    }
}

impl fmt::Display for ProjectPathIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for ProjectPathIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ProjectPathIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse_hex(&value).map_err(serde::de::Error::custom)
    }
}

fn absolute_project_file(project_file: &Path) -> Result<PathBuf, String> {
    if project_file.as_os_str().is_empty() {
        return Err("Project path is empty".to_owned());
    }
    std::path::absolute(project_file)
        .map_err(|error| format!("failed to make Project path absolute: {error}"))
}

fn normalize_publication_path(absolute: &Path) -> Result<PathBuf, String> {
    let leaf = absolute
        .file_name()
        .ok_or_else(|| "Project publication target has no file name".to_owned())?;
    let mut cursor = absolute
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Project publication target has no parent directory".to_owned())?;
    let mut unresolved = Vec::new();
    loop {
        match fs::canonicalize(cursor) {
            Ok(mut canonical) => {
                for component in unresolved.into_iter().rev() {
                    canonical.push(component);
                }
                canonical.push(normalize_unresolved_component(leaf));
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let file_name = cursor.file_name().ok_or_else(|| {
                    format!(
                        "Project publication target has no existing canonical ancestor: {}",
                        absolute.display()
                    )
                })?;
                unresolved.push(normalize_unresolved_component(file_name));
                cursor = cursor.parent().ok_or_else(|| {
                    format!(
                        "Project publication target has no canonical parent: {}",
                        absolute.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to resolve Project publication target namespace: {error}"
                ));
            }
        }
    }
}

#[cfg(unix)]
fn normalize_unresolved_component(component: &OsStr) -> std::ffi::OsString {
    component.to_os_string()
}

#[cfg(windows)]
fn normalize_unresolved_component(component: &OsStr) -> std::ffi::OsString {
    let mut units = component.encode_wide().collect::<Vec<_>>();
    // The ordinary Win32 namespace ignores trailing spaces and dots. Treating
    // the rarer verbatim/case-sensitive spelling as the same authority is a
    // conservative over-exclusion, never a split-brain.
    while units
        .last()
        .is_some_and(|unit| *unit == u16::from(b' ') || *unit == u16::from(b'.'))
    {
        units.pop();
    }
    if units.is_empty() {
        component.to_os_string()
    } else {
        std::ffi::OsString::from_wide(&units)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRuntimeOwnerManifest {
    schema_version: u32,
    allocation_target_identity_sha256: ProjectPathIdentity,
    project_id: ProjectId,
}

impl ProjectRuntimeOwnerManifest {
    fn new(allocation_target_identity: ProjectPathIdentity, project_id: ProjectId) -> Self {
        Self {
            schema_version: PROJECT_RUNTIME_OWNER_SCHEMA_VERSION,
            allocation_target_identity_sha256: allocation_target_identity,
            project_id,
        }
    }

    fn validate_schema(&self) -> Result<(), String> {
        if self.schema_version != PROJECT_RUNTIME_OWNER_SCHEMA_VERSION {
            return Err(format!(
                "unsupported Project runtime owner schema v{} (expected v{})",
                self.schema_version, PROJECT_RUNTIME_OWNER_SCHEMA_VERSION
            ));
        }
        Ok(())
    }
}

/// Typed owner-publication failure retained until runtime admission decides
/// whether any namespace rollback is safe.
///
/// No variant grants payload authority. In particular, an unconfirmed or
/// indeterminate namespace leaves the root inert for this attempt and is never
/// collapsed into an ordinary pre-publication failure.
#[derive(Debug)]
enum ProjectRuntimeOwnerPublicationFailure {
    BeforeNamespace(anyhow::Error),
    DurabilityUnconfirmed(FilePublicationDurabilityUnconfirmed),
    NamespaceIndeterminate(FilePublicationNamespaceIndeterminate),
}

impl fmt::Display for ProjectRuntimeOwnerPublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeNamespace(error) => {
                write!(formatter, "owner publication failed before namespace commit: {error:#}")
            }
            Self::DurabilityUnconfirmed(error) => write!(
                formatter,
                "owner namespace is visible but crash durability is unconfirmed: {error}"
            ),
            Self::NamespaceIndeterminate(error) => write!(
                formatter,
                "owner namespace postcondition is indeterminate and automatic cleanup is disabled: {error}"
            ),
        }
    }
}

impl From<FilePublicationFailure> for ProjectRuntimeOwnerPublicationFailure {
    fn from(error: FilePublicationFailure) -> Self {
        match error {
            FilePublicationFailure::BeforeNamespace(error) => Self::BeforeNamespace(error),
            FilePublicationFailure::DurabilityUnconfirmed(error) => {
                Self::DurabilityUnconfirmed(error)
            }
            FilePublicationFailure::NamespaceIndeterminate(error) => {
                Self::NamespaceIndeterminate(error)
            }
        }
    }
}

/// Derive the exact path-paired payload root for an ordinary Project Open.
pub(super) fn project_runtime_root_for_project(
    project_file: &Path,
    project_id: ProjectId,
) -> Result<PathBuf, String> {
    project_runtime_root_under(&project_runtime_parent()?, project_file, project_id)
}

/// Enumerate the payload roots currently allocated to a publication path.
///
/// Tests use this instead of treating the path-only family locator as one
/// mutable payload authority. Production discovery validates owner manifests
/// independently and never grants authority from this enumeration.
#[cfg(test)]
pub(crate) fn project_runtime_roots_for_path_for_test(
    project_file: &Path,
) -> Result<Vec<PathBuf>, String> {
    let parent = project_runtime_parent()?;
    let family = project_runtime_family_root_under(&parent, project_file)?;
    let Some(family_name) = family.file_name().and_then(OsStr::to_str) else {
        return Err("Project runtime family name is not valid Unicode".to_owned());
    };
    let prefix = format!("{family_name}{PROJECT_RUNTIME_DIRECTORY_IDENTITY_SEPARATOR}");
    let entries = match fs::read_dir(&parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "failed to enumerate Project runtime family: {error}"
            ));
        }
    };
    let mut roots = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to inspect Project runtime family: {error}"))?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| name.starts_with(&prefix)) {
            roots.push(entry.path());
        }
    }
    roots.sort();
    Ok(roots)
}

/// Claim and exclusively lease the runtime selected by a Project target.
pub(super) fn claim_project_runtime(
    project_file: &Path,
    project_id: ProjectId,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    let runtime_parent = project_runtime_parent()?;
    claim_project_runtime_under_with_authority(
        &runtime_parent,
        &project_authority_root()?,
        project_file,
        project_id,
    )
}

/// Claim a path-paired payload root while sharing the exact logical-Project lock.
pub(super) fn claim_project_runtime_sharing_logical_authority(
    project_file: &Path,
    project_id: ProjectId,
    existing: &ProjectRuntimeLease,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    let runtime_parent = project_runtime_parent()?;
    claim_project_runtime_under_with_authority_and_logical(
        &runtime_parent,
        &project_authority_root()?,
        project_file,
        project_id,
        Some(Arc::clone(&existing.logical_authority)),
    )
}

/// Exclusively lease an already selected and durably owned runtime root.
pub(super) fn lease_existing_project_runtime(
    runtime_root: &Path,
    project_id: ProjectId,
    publication_target: &Path,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    let runtime_parent = project_runtime_parent()?;
    validate_direct_runtime_root(&runtime_parent, runtime_root)?;
    let authority_root = project_authority_root()?;
    let _guard = project_runtime_owner_guard();
    acquire_existing_project_runtime_unlocked(
        runtime_root,
        &authority_root,
        project_id,
        publication_target,
        None,
    )
}

/// Lease one exact Recovery root while sharing the current logical-Project lock.
pub(super) fn lease_existing_project_runtime_sharing_logical_authority(
    runtime_root: &Path,
    project_id: ProjectId,
    publication_target: &Path,
    existing: &ProjectRuntimeLease,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    let runtime_parent = project_runtime_parent()?;
    validate_direct_runtime_root(&runtime_parent, runtime_root)?;
    let authority_root = project_authority_root()?;
    let _guard = project_runtime_owner_guard();
    acquire_existing_project_runtime_unlocked(
        runtime_root,
        &authority_root,
        project_id,
        publication_target,
        Some(Arc::clone(&existing.logical_authority)),
    )
}

/// Stable private per-user state namespace containing every recovery-bearing
/// Project runtime root.
///
/// This location is deliberately independent from process `TEMP`, `TMPDIR`,
/// and `XDG_RUNTIME_DIR`: operating-system cleanup of ephemeral runtime files
/// must not silently destroy Recovery Authority.
#[cfg(all(windows, not(test)))]
pub(super) fn project_runtime_parent() -> Result<PathBuf, String> {
    Ok(windows_local_app_data()?.join(PROJECT_RUNTIME_STATE_DIRECTORY))
}

#[cfg(all(target_os = "macos", not(test)))]
pub(super) fn project_runtime_parent() -> Result<PathBuf, String> {
    Ok(macos_application_support_directory()?.join(PROJECT_RUNTIME_STATE_DIRECTORY))
}

#[cfg(all(unix, not(target_os = "macos"), not(test)))]
pub(super) fn project_runtime_parent() -> Result<PathBuf, String> {
    let state_home = unix_state_home(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    Ok(state_home.join(PROJECT_RUNTIME_STATE_DIRECTORY))
}

#[cfg(test)]
pub(super) fn project_runtime_parent() -> Result<PathBuf, String> {
    Ok(std::env::temp_dir().join(format!(
        "mondrian-project-runtime-state-tests-{}",
        std::process::id()
    )))
}

/// Stable per-user namespace for live cross-process Project authority.
///
/// This lock namespace and the separate recovery-bearing state namespace are
/// both independent of process `TEMP`/`TMPDIR`; neither authority may split
/// merely because two processes inherited different temporary environments.
#[cfg(all(windows, not(test)))]
fn project_authority_root() -> Result<PathBuf, String> {
    Ok(windows_local_app_data()?
        .join("Mondrian")
        .join("authority")
        .join("project-runtime-v2"))
}

#[cfg(all(windows, not(test)))]
fn windows_local_app_data() -> Result<PathBuf, String> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};

    let mut raw_path: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: the API initializes `raw_path` with a COM-task allocation on
    // success. The current user token is selected by a null token handle and
    // the allocation is released exactly once below.
    let result = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_LocalAppData,
            0,
            std::ptr::null_mut(),
            &mut raw_path,
        )
    };
    if result < 0 {
        return Err(format!(
            "failed to locate stable per-user Project authority root (HRESULT 0x{:08x})",
            result as u32
        ));
    }
    if raw_path.is_null() {
        return Err("stable per-user Project authority root is unavailable".to_owned());
    }
    let mut length = 0_usize;
    // SAFETY: `SHGetKnownFolderPath` returns one NUL-terminated UTF-16 string.
    while unsafe { *raw_path.add(length) } != 0 {
        if length >= 32_768 {
            // SAFETY: `raw_path` is the exact COM-task allocation returned above.
            unsafe { CoTaskMemFree(raw_path.cast()) };
            return Err("Project authority path exceeds the Windows path limit".to_owned());
        }
        length += 1;
    }
    // SAFETY: the preceding scan proved that the first `length` units are
    // initialized and precede the terminating NUL.
    let units = unsafe { std::slice::from_raw_parts(raw_path, length) };
    let local_app_data = PathBuf::from(std::ffi::OsString::from_wide(units));
    // SAFETY: `raw_path` is the exact COM-task allocation returned above.
    unsafe { CoTaskMemFree(raw_path.cast()) };
    if !local_app_data.is_absolute() {
        return Err("stable per-user LocalAppData path is not absolute".to_owned());
    }
    Ok(local_app_data)
}

#[cfg(all(unix, not(test)))]
fn project_authority_root() -> Result<PathBuf, String> {
    // This authority must remain identical even when two processes belonging
    // to the same user inherited different environment variables. Runtime
    // payload prefers XDG, but exclusion deliberately uses the platform-fixed
    // temporary namespace partitioned by effective UID.
    let effective_uid = effective_user_id();
    let system_tmp = fs::canonicalize("/tmp")
        .map_err(|error| format!("failed to resolve system Project authority root: {error}"))?;
    Ok(system_tmp.join(format!("mondrian-project-authority-v2-{effective_uid}")))
}

#[cfg(test)]
fn project_authority_root() -> Result<PathBuf, String> {
    Ok(std::env::temp_dir().join(format!(
        "mondrian-project-authority-tests-{}",
        std::process::id()
    )))
}

#[cfg(unix)]
fn effective_user_id() -> libc::uid_t {
    // SAFETY: `geteuid` has no preconditions and reads process identity only.
    unsafe { libc::geteuid() }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_state_home(
    xdg_state_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if let Some(candidate) = xdg_state_home.map(Path::new).filter(|path| path.is_absolute()) {
        return Ok(candidate.to_path_buf());
    }
    let home = home.map(Path::new).filter(|path| path.is_absolute()).ok_or_else(|| {
        "stable per-user Project state root requires absolute XDG_STATE_HOME or HOME".to_owned()
    })?;
    Ok(home.join(".local").join("state"))
}

#[cfg(all(target_os = "macos", not(test)))]
fn stable_home_directory() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "stable per-user Project state root requires an absolute HOME".to_owned())?;
    Ok(home)
}

#[cfg(all(target_os = "macos", not(test)))]
fn macos_application_support_directory() -> Result<PathBuf, String> {
    Ok(stable_home_directory()?.join("Library").join("Application Support"))
}

/// Read-only verification of immutable runtime ownership.
///
/// The allocation path remains stable across Save As. Callers that already
/// possess the runtime root therefore validate the complete stored path
/// identity structurally and bind it to the exact `ProjectId`. This does not
/// authorize mutation; mutation requires a live [`ProjectRuntimeLease`].
pub(super) fn validate_runtime_owner_readonly(
    runtime_root: &Path,
    expected_project_id: ProjectId,
) -> Result<(), String> {
    let _guard = project_runtime_owner_guard();
    validate_runtime_owner_unlocked(runtime_root, expected_project_id).map(|_| ())
}

/// Read-only validation for discovery before a live lease is requested.
///
/// This never authorizes a mutation. Production mutation seams require a
/// [`ProjectRuntimeLease`] instead of a root/identity tuple.
pub(super) fn validate_runtime_child_directory_readonly(
    runtime_root: &Path,
    child: &Path,
    expected_project_id: ProjectId,
) -> Result<(), String> {
    let _guard = project_runtime_owner_guard();
    validate_runtime_owner_unlocked(runtime_root, expected_project_id)?;
    validate_direct_runtime_child(runtime_root, child)?;
    validate_existing_runtime_child_directory(child)
}

/// Create one explicitly ephemeral direct child while the root owner is valid.
///
/// Library generations are replaceable runtime copies and intentionally do
/// not pay a parent-directory durability cost. Recovery-bearing children must
/// use [`ensure_owned_runtime_child_directory`] instead.
pub(super) fn create_ephemeral_owned_runtime_child_directory(
    lease: &ProjectRuntimeLease,
    child: &Path,
) -> Result<(), String> {
    let _guard = project_runtime_owner_guard();
    validate_live_lease_unlocked(lease)?;
    validate_direct_runtime_child(lease.runtime_root(), child)?;
    fs::create_dir(child)
        .map_err(|error| format!("failed to create Project runtime child directory: {error}"))?;
    validate_existing_runtime_child_directory(child)
}

/// Ensure one idempotent, crash-durable direct child while the root owner is
/// valid.
///
/// This is the creation seam for Recovery Authority such as `autosave/`. A
/// namespace that is visible but not proven durable remains inadmissible for
/// the current attempt. Existing symlinks, files, or other non-directory
/// entries fail closed and are never replaced.
pub(super) fn ensure_owned_runtime_child_directory(
    lease: &ProjectRuntimeLease,
    child: &Path,
) -> Result<(), String> {
    let _guard = project_runtime_owner_guard();
    validate_live_lease_unlocked(lease)?;
    validate_direct_runtime_child(lease.runtime_root(), child)?;
    ensure_durable_direct_child_directory(
        lease.runtime_root(),
        child,
        "Project runtime recovery child",
        validate_existing_runtime_child_directory,
    )
}

/// Validate one existing direct child directory and its root owner.
pub(super) fn validate_owned_runtime_child_directory(
    lease: &ProjectRuntimeLease,
    child: &Path,
) -> Result<(), String> {
    let _guard = project_runtime_owner_guard();
    validate_live_lease_unlocked(lease)?;
    validate_direct_runtime_child(lease.runtime_root(), child)?;
    validate_existing_runtime_child_directory(child)
}

/// Remove one direct child directory only while the root owner is valid.
pub(super) fn remove_owned_runtime_child_directory_if_exists(
    lease: &ProjectRuntimeLease,
    child: &Path,
) -> Result<(), String> {
    let _guard = project_runtime_owner_guard();
    validate_live_lease_unlocked(lease)?;
    validate_direct_runtime_child(lease.runtime_root(), child)?;
    let metadata = match fs::symlink_metadata(child) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!("failed to inspect Project runtime child: {error}"));
        }
    };
    validate_existing_runtime_child_metadata(&metadata)?;
    fs::remove_dir_all(child)
        .map_err(|error| format!("failed to remove Project runtime child directory: {error}"))
}

#[cfg(test)]
fn project_runtime_family_root_under(base: &Path, project_file: &Path) -> Result<PathBuf, String> {
    let identity = ProjectPathIdentity::from_project_file(project_file)?;
    Ok(base.join(runtime_family_directory_name(identity)))
}

fn project_runtime_root_under(
    base: &Path,
    project_file: &Path,
    project_id: ProjectId,
) -> Result<PathBuf, String> {
    let identity = ProjectPathIdentity::from_project_file(project_file)?;
    Ok(base.join(runtime_directory_name(identity, project_id)))
}

#[cfg(test)]
fn claim_project_runtime_under(
    base: &Path,
    project_file: &Path,
    project_id: ProjectId,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    claim_project_runtime_under_with_authority(
        base,
        &base.join(PROJECT_RUNTIME_AUTHORITY_DIRECTORY),
        project_file,
        project_id,
    )
}

fn claim_project_runtime_under_with_authority(
    base: &Path,
    authority_root: &Path,
    project_file: &Path,
    project_id: ProjectId,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    claim_project_runtime_under_with_authority_and_logical(
        base,
        authority_root,
        project_file,
        project_id,
        None,
    )
}

fn claim_project_runtime_under_with_authority_and_logical(
    base: &Path,
    authority_root: &Path,
    project_file: &Path,
    project_id: ProjectId,
    existing_logical_authority: Option<Arc<ProjectLogicalAuthority>>,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    let absolute_target = absolute_project_file(project_file)?;
    let identity = ProjectPathIdentity::from_absolute_project_file(&absolute_target)?;
    let runtime_root = base.join(runtime_directory_name(identity, project_id));
    let _guard = project_runtime_owner_guard();
    ensure_runtime_parent_directory(base)?;
    ensure_runtime_authority_directory(authority_root)?;
    let logical_authority = match existing_logical_authority {
        Some(logical_authority) => {
            validate_shared_logical_authority_unlocked(
                &logical_authority,
                authority_root,
                project_id,
            )?;
            logical_authority
                .retain_publication_target_unlocked(absolute_target.clone(), identity)?;
            logical_authority
        }
        None => {
            let project_lock = ExclusiveNamespaceLock::acquire(
                project_lock_path(authority_root, project_id),
                "logical Project",
            )?;
            let publication_authority =
                PublicationTargetAuthority::acquire(authority_root, absolute_target, identity)?;
            Arc::new(ProjectLogicalAuthority {
                authority_root: authority_root.to_path_buf(),
                project_id,
                project_lock,
                publication_authorities: Mutex::new(BTreeMap::from([(
                    identity,
                    publication_authority,
                )])),
            })
        }
    };

    ensure_durable_runtime_root(base, &runtime_root)?;

    let session_lock = ExclusiveNamespaceLock::acquire(
        session_lock_path(&runtime_root),
        "Project runtime Session",
    )?;
    let manifest = if owner_manifest_is_missing(&runtime_root)? {
        if runtime_contains_only_session_lock(&runtime_root)? {
            ProjectRuntimeOwnerManifest::new(identity, project_id)
        } else {
            drop(session_lock);
            return Err(
                "Project runtime owner manifest is missing from a non-empty runtime".to_owned(),
            );
        }
    } else {
        validate_runtime_owner_unlocked(&runtime_root, project_id)?
    };
    if manifest.allocation_target_identity_sha256 != identity {
        drop(session_lock);
        return Err("Project runtime root allocation-target identity does not match".to_owned());
    }
    // Even a previously visible manifest is republished through the typed
    // durability boundary. This is the safe retry for an earlier
    // DurabilityUnconfirmed result; a plain reopen can never upgrade unknown
    // publication state into payload authority.
    if let Err(error) = publish_owner_manifest(&runtime_root, &manifest) {
        drop(session_lock);
        return Err(format!(
            "failed to durably establish Project runtime owner; the root remains inert: {error}"
        ));
    }
    let published_manifest = validate_runtime_owner_unlocked(&runtime_root, project_id)?;
    if published_manifest != manifest {
        drop(session_lock);
        return Err(
            "Project runtime owner changed during durable admission; the root remains inert"
                .to_owned(),
        );
    }
    Ok(Arc::new(ProjectRuntimeLease {
        id: next_project_runtime_lease_id()?,
        runtime_root,
        session_lock,
        logical_authority,
    }))
}

fn acquire_existing_project_runtime_unlocked(
    runtime_root: &Path,
    authority_root: &Path,
    project_id: ProjectId,
    publication_target: &Path,
    existing_logical_authority: Option<Arc<ProjectLogicalAuthority>>,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    validate_runtime_root_directory(runtime_root)?;
    ensure_runtime_authority_directory(authority_root)?;
    let absolute_target = absolute_project_file(publication_target)?;
    let publication_identity = ProjectPathIdentity::from_absolute_project_file(&absolute_target)?;
    let logical_authority = match existing_logical_authority {
        Some(logical_authority) => {
            validate_shared_logical_authority_unlocked(
                &logical_authority,
                authority_root,
                project_id,
            )?;
            logical_authority
                .retain_publication_target_unlocked(absolute_target, publication_identity)?;
            logical_authority
        }
        None => {
            let project_lock = ExclusiveNamespaceLock::acquire(
                project_lock_path(authority_root, project_id),
                "logical Project",
            )?;
            let publication_authority = PublicationTargetAuthority::acquire(
                authority_root,
                absolute_target,
                publication_identity,
            )?;
            Arc::new(ProjectLogicalAuthority {
                authority_root: authority_root.to_path_buf(),
                project_id,
                project_lock,
                publication_authorities: Mutex::new(BTreeMap::from([(
                    publication_identity,
                    publication_authority,
                )])),
            })
        }
    };
    let session_lock = ExclusiveNamespaceLock::acquire(
        session_lock_path(runtime_root),
        "Project runtime Session",
    )?;
    let manifest = match validate_runtime_owner_unlocked(runtime_root, project_id) {
        Ok(manifest) => manifest,
        Err(error) => {
            drop(session_lock);
            return Err(error);
        }
    };
    if let Err(error) = publish_owner_manifest(runtime_root, &manifest) {
        drop(session_lock);
        return Err(format!(
            "failed to reconfirm durable Project runtime owner; the root remains inert: {error}"
        ));
    }
    let republished = match validate_runtime_owner_unlocked(runtime_root, project_id) {
        Ok(manifest) => manifest,
        Err(error) => {
            drop(session_lock);
            return Err(error);
        }
    };
    if republished != manifest {
        drop(session_lock);
        return Err(
            "Project runtime owner changed during durable lease admission; the root remains inert"
                .to_owned(),
        );
    }
    Ok(Arc::new(ProjectRuntimeLease {
        id: next_project_runtime_lease_id()?,
        runtime_root: runtime_root.to_path_buf(),
        session_lock,
        logical_authority,
    }))
}

fn validate_runtime_owner_unlocked(
    runtime_root: &Path,
    expected_project_id: ProjectId,
) -> Result<ProjectRuntimeOwnerManifest, String> {
    validate_runtime_root_directory(runtime_root)?;
    let manifest = read_owner_manifest(runtime_root)?;
    manifest.validate_schema()?;
    if manifest.project_id != expected_project_id {
        return Err("Project runtime root belongs to another Project".to_owned());
    }
    let expected_directory = runtime_directory_name(
        manifest.allocation_target_identity_sha256,
        manifest.project_id,
    );
    if runtime_root.file_name() != Some(expected_directory.as_os_str()) {
        return Err(
            "Project runtime directory does not match its complete allocation-target identity"
                .to_owned(),
        );
    }
    Ok(manifest)
}

fn validate_live_lease_unlocked(lease: &ProjectRuntimeLease) -> Result<(), String> {
    validate_logical_authority_unlocked(&lease.logical_authority)?;
    lease.session_lock.validate_namespace_binding()?;
    validate_runtime_owner_unlocked(lease.runtime_root(), lease.project_id()).map(|_| ())
}

fn validate_shared_logical_authority_unlocked(
    authority: &ProjectLogicalAuthority,
    authority_root: &Path,
    project_id: ProjectId,
) -> Result<(), String> {
    if authority.project_id != project_id {
        return Err("shared logical Project authority belongs to another Project".to_owned());
    }
    if authority.authority_root != authority_root {
        return Err(
            "shared logical Project authority belongs to another authority namespace".to_owned(),
        );
    }
    validate_logical_authority_unlocked(authority)
}

fn validate_logical_authority_unlocked(authority: &ProjectLogicalAuthority) -> Result<(), String> {
    authority.project_lock.validate_namespace_binding()?;
    let publication_authorities = authority.publication_authorities.lock().map_err(|_| {
        "Project publication authority registry was poisoned; refusing to weaken exclusion"
            .to_owned()
    })?;
    if publication_authorities.is_empty() {
        return Err("Project runtime lease has no publication authority".to_owned());
    }
    for authority in publication_authorities.values() {
        authority.validate()?;
    }
    Ok(())
}

fn ensure_durable_runtime_root(runtime_parent: &Path, runtime_root: &Path) -> Result<(), String> {
    ensure_durable_direct_child_directory(
        runtime_parent,
        runtime_root,
        "Project runtime root",
        validate_runtime_root_directory,
    )
}

fn ensure_durable_direct_child_directory(
    parent: &Path,
    child: &Path,
    description: &str,
    validate_existing: impl Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    match fs::symlink_metadata(child) {
        Ok(_) => {
            // Existing namespace presence is not Storage durability evidence.
            // This domain seam admits it only provisionally: runtime roots
            // must still publish and validate their owner manifest before any
            // payload authority is granted, while recovery children are
            // already constrained by the exact live root lease.
            return validate_existing(child);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect existing {description} before durable admission: {error}"
            ));
        }
    }
    match create_durable_direct_child(parent, child) {
        Ok(evidence) => {
            let expected = std::path::absolute(child).map_err(|error| {
                format!("failed to normalize {description} publication path: {error}")
            })?;
            if evidence.path() != expected {
                return Err(format!(
                    "{description} publication returned mismatched path evidence"
                ));
            }
            validate_existing(child)
        }
        Err(DirectoryPublicationFailure::BeforeNamespace(error)) => Err(format!(
            "failed to publish {description} before namespace commit: {error:#}"
        )),
        Err(DirectoryPublicationFailure::DurabilityUnconfirmed { path, source }) => Err(format!(
            "{description} is visible at {} but crash durability is unconfirmed; it remains inert for this attempt: {source}",
            path.display()
        )),
        Err(DirectoryPublicationFailure::NamespaceIndeterminate {
            intended_path,
            retained_staging_path,
            source,
        }) => {
            let retained = retained_staging_path
                .as_deref()
                .map(|path| format!("; verified staging remains at {}", path.display()))
                .unwrap_or_default();
            Err(format!(
                "{description} publication at {} has an indeterminate namespace postcondition{retained}; automatic cleanup is disabled: {source}",
                intended_path.display()
            ))
        }
    }
}

fn validate_runtime_root_directory(runtime_root: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(runtime_root)
        .map_err(|error| format!("failed to inspect Project runtime root: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Project runtime root is not a direct filesystem directory".to_owned());
    }
    Ok(())
}

fn validate_direct_runtime_root(runtime_parent: &Path, runtime_root: &Path) -> Result<(), String> {
    let Some(actual_parent) = runtime_root.parent() else {
        return Err(
            "Project runtime root is outside the canonical runtime authority namespace".to_owned(),
        );
    };
    if runtime_root.file_name().is_none() {
        return Err(
            "Project runtime root is outside the canonical runtime authority namespace".to_owned(),
        );
    }
    if actual_parent == runtime_parent {
        return Ok(());
    }
    let expected = match fs::canonicalize(runtime_parent) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "Project runtime root is outside the canonical runtime authority namespace"
                    .to_owned(),
            );
        }
        Err(error) => {
            return Err(format!(
                "failed to resolve canonical Project runtime authority parent: {error}"
            ));
        }
    };
    let actual = match fs::canonicalize(actual_parent) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "Project runtime root is outside the canonical runtime authority namespace"
                    .to_owned(),
            );
        }
        Err(error) => {
            return Err(format!(
                "failed to resolve selected Project runtime authority parent: {error}"
            ));
        }
    };
    if actual != expected {
        return Err(
            "Project runtime root is outside the canonical runtime authority namespace".to_owned(),
        );
    }
    Ok(())
}

fn owner_manifest_is_missing(runtime_root: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(owner_manifest_path(runtime_root)) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!(
            "failed to inspect Project runtime owner manifest: {error}"
        )),
    }
}

fn runtime_contains_only_session_lock(runtime_root: &Path) -> Result<bool, String> {
    let mut entries = fs::read_dir(runtime_root)
        .map_err(|error| format!("failed to inspect incomplete Project runtime: {error}"))?;
    let Some(first) = entries.next() else {
        return Ok(false);
    };
    let first =
        first.map_err(|error| format!("failed to inspect incomplete Project runtime: {error}"))?;
    if first.file_name() != OsStr::new(PROJECT_RUNTIME_SESSION_LOCK_FILE) {
        return Ok(false);
    }
    match entries.next() {
        None => Ok(true),
        Some(Ok(_)) => Ok(false),
        Some(Err(error)) => Err(format!(
            "failed to inspect incomplete Project runtime: {error}"
        )),
    }
}

fn validate_direct_runtime_child(runtime_root: &Path, child: &Path) -> Result<(), String> {
    if child.parent() != Some(runtime_root)
        || child.file_name().is_none()
        || child.file_name() == Some(OsStr::new(PROJECT_RUNTIME_OWNER_FILE))
        || child.file_name() == Some(OsStr::new(PROJECT_RUNTIME_SESSION_LOCK_FILE))
    {
        return Err("Project runtime mutation target is not a permitted direct child".to_owned());
    }
    Ok(())
}

fn validate_existing_runtime_child_directory(child: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(child)
        .map_err(|error| format!("failed to inspect Project runtime child: {error}"))?;
    validate_existing_runtime_child_metadata(&metadata)
}

fn validate_existing_runtime_child_metadata(metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Project runtime child is not a direct filesystem directory".to_owned());
    }
    Ok(())
}

fn read_owner_manifest(runtime_root: &Path) -> Result<ProjectRuntimeOwnerManifest, String> {
    let path = owner_manifest_path(runtime_root);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("Project runtime owner manifest is unavailable: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Project runtime owner manifest is not a direct file".to_owned());
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("failed to read Project runtime owner: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid Project runtime owner manifest: {error}"))
}

fn publish_owner_manifest(
    runtime_root: &Path,
    manifest: &ProjectRuntimeOwnerManifest,
) -> Result<(), ProjectRuntimeOwnerPublicationFailure> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        ProjectRuntimeOwnerPublicationFailure::BeforeNamespace(anyhow::Error::new(error))
    })?;
    write_durable_file_atomically(&owner_manifest_path(runtime_root), &bytes)
        .map(|_| ())
        .map_err(ProjectRuntimeOwnerPublicationFailure::from)
}

fn owner_manifest_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join(PROJECT_RUNTIME_OWNER_FILE)
}

fn session_lock_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join(PROJECT_RUNTIME_SESSION_LOCK_FILE)
}

fn project_lock_path(authority_root: &Path, project_id: ProjectId) -> PathBuf {
    authority_root.join(format!("project-{project_id}.lock"))
}

fn publication_lock_path(authority_root: &Path, identity: ProjectPathIdentity) -> PathBuf {
    authority_root.join(format!("publication-{identity}.lock"))
}

fn ensure_runtime_parent_directory(runtime_parent: &Path) -> Result<(), String> {
    let runtime_parent = std::path::absolute(runtime_parent)
        .map_err(|error| format!("failed to normalize Project runtime parent: {error}"))?;
    let created_chain = match fs::symlink_metadata(&runtime_parent) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let anchor = trusted_runtime_parent_anchor(&runtime_parent)?;
            ensure_durable_directory_chain(&anchor, &runtime_parent).map_err(|error| {
                format!(
                    "failed to durably establish Project runtime parent chain from {}: {error}",
                    anchor.display()
                )
            })?;
            true
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect Project runtime parent before durable establishment: {error}"
            ));
        }
    };
    validate_existing_runtime_parent(&runtime_parent)?;
    ensure_runtime_parent_marker(&runtime_parent, created_chain)
}

fn validate_existing_runtime_parent(runtime_parent: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        ensure_owned_private_directory(runtime_parent, "Project runtime parent")
    }
    #[cfg(windows)]
    {
        let metadata = fs::symlink_metadata(runtime_parent)
            .map_err(|error| format!("failed to inspect Project runtime parent: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("Project runtime parent is not a direct filesystem directory".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
fn trusted_runtime_parent_anchor(runtime_parent: &Path) -> Result<PathBuf, String> {
    nearest_existing_direct_ancestor(runtime_parent)
}

#[cfg(all(windows, not(test)))]
fn trusted_runtime_parent_anchor(_runtime_parent: &Path) -> Result<PathBuf, String> {
    windows_local_app_data()
}

#[cfg(all(target_os = "macos", not(test)))]
fn trusted_runtime_parent_anchor(_runtime_parent: &Path) -> Result<PathBuf, String> {
    existing_direct_anchor_or_ancestor(&macos_application_support_directory()?)
}

#[cfg(all(unix, not(target_os = "macos"), not(test)))]
fn trusted_runtime_parent_anchor(_runtime_parent: &Path) -> Result<PathBuf, String> {
    let state_home = unix_state_home(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    existing_direct_anchor_or_ancestor(&state_home)
}

#[cfg(all(unix, not(test)))]
fn existing_direct_anchor_or_ancestor(path: &Path) -> Result<PathBuf, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
            Ok(path.to_path_buf())
        }
        Ok(_) => Err(format!(
            "trusted Project runtime state anchor is not a direct directory: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            nearest_existing_direct_ancestor(path)
        }
        Err(error) => Err(format!(
            "failed to inspect trusted Project runtime state anchor {}: {error}",
            path.display()
        )),
    }
}

#[cfg(any(test, unix))]
fn nearest_existing_direct_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut candidate = path.parent().ok_or_else(|| {
        "Project runtime parent has no ancestor that can act as a trusted anchor".to_owned()
    })?;
    loop {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "Project runtime parent ancestor is not a direct directory: {}",
                        candidate.display()
                    ));
                }
                return Ok(candidate.to_path_buf());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or_else(|| {
                    "Project runtime parent has no existing trusted directory anchor".to_owned()
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect Project runtime parent ancestor {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
}

fn ensure_runtime_parent_marker(
    runtime_parent: &Path,
    allow_initial_publication: bool,
) -> Result<(), String> {
    let marker = runtime_parent.join(PROJECT_RUNTIME_PARENT_MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(
                    "Project runtime parent durability marker is not a direct file".to_owned(),
                );
            }
            let bytes = fs::read(&marker).map_err(|error| {
                format!("failed to read Project runtime parent durability marker: {error}")
            })?;
            if bytes != PROJECT_RUNTIME_PARENT_MARKER_BYTES {
                return Err(
                    "Project runtime parent durability marker has unknown contents".to_owned(),
                );
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !allow_initial_publication {
                return Err(
                    "existing Project runtime parent has no durable ownership marker; it remains inert"
                        .to_owned(),
                );
            }
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect Project runtime parent durability marker: {error}"
            ));
        }
    }

    write_durable_file_atomically_with_mode(
        &marker,
        PROJECT_RUNTIME_PARENT_MARKER_BYTES,
        FilePublicationMode::CreateNew,
    )
        .map_err(
        |error| match error {
            FilePublicationFailure::BeforeNamespace(error) => format!(
                "failed to publish Project runtime parent durability marker before namespace commit: {error:#}"
            ),
            FilePublicationFailure::DurabilityUnconfirmed(error) => format!(
                "Project runtime parent durability marker is visible but crash durability is unconfirmed: {error}"
            ),
            FilePublicationFailure::NamespaceIndeterminate(error) => format!(
                "Project runtime parent durability marker has an indeterminate namespace postcondition: {error}"
            ),
        },
    )?;
    let published = fs::read(&marker).map_err(|error| {
        format!("failed to verify Project runtime parent durability marker: {error}")
    })?;
    if published != PROJECT_RUNTIME_PARENT_MARKER_BYTES {
        return Err(
            "Project runtime parent durability marker changed after publication".to_owned(),
        );
    }
    Ok(())
}

fn ensure_runtime_authority_directory(authority_root: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        ensure_owned_private_directory(authority_root, "Project authority directory")
    }
    #[cfg(windows)]
    {
        let parent = authority_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| "Project authority directory has no parent".to_owned())?;
        ensure_direct_directory(parent, "Project runtime parent")?;
        match fs::create_dir(authority_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "failed to create Project authority directory: {error}"
                ));
            }
        }
        ensure_direct_directory(authority_root, "Project authority directory")
    }
}

#[cfg(unix)]
fn ensure_owned_private_directory(path: &Path, description: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("{description} has no parent"))?;
    ensure_direct_directory(parent, &format!("{description} parent"))?;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    let create_result = builder.create(path);
    match create_result {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!("failed to create {description}: {error}"));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {description}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{description} is not a direct filesystem directory"
        ));
    }
    if metadata.uid() != effective_user_id() {
        return Err(format!(
            "{description} belongs to another operating-system user"
        ));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("failed to protect {description}: {error}"))?;
        let protected = fs::symlink_metadata(path)
            .map_err(|error| format!("failed to re-inspect protected {description}: {error}"))?;
        if protected.file_type().is_symlink()
            || !protected.is_dir()
            || protected.uid() != effective_user_id()
            || protected.permissions().mode() & 0o777 != 0o700
        {
            return Err(format!(
                "{description} could not be secured for the current operating-system user"
            ));
        }
    }
    Ok(())
}

fn ensure_direct_directory(path: &Path, description: &str) -> Result<(), String> {
    match fs::create_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!("failed to create {description}: {error}"));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {description}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{description} is not a direct filesystem directory"
        ));
    }
    Ok(())
}

/// Open one existing direct regular file without following a leaf reparse
/// point/symlink.
///
/// Windows permits concurrent readers but denies write/delete sharing while
/// the handle is live. Unix uses `O_NOFOLLOW`; callers retain the handle when
/// object identity must survive a later namespace replacement.
#[cfg(windows)]
pub(super) fn open_direct_read_file(path: &Path, description: &str) -> Result<File, String> {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("failed to open direct {description}: {error}"))?;
    validate_direct_file_metadata(
        &file
            .metadata()
            .map_err(|error| format!("failed to inspect direct {description}: {error}"))?,
        description,
    )?;
    Ok(file)
}

#[cfg(unix)]
pub(super) fn open_direct_read_file(path: &Path, description: &str) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("failed to open direct {description}: {error}"))?;
    validate_direct_file_metadata(
        &file
            .metadata()
            .map_err(|error| format!("failed to inspect direct {description}: {error}"))?,
        description,
    )?;
    Ok(file)
}

/// Exclusively create one direct regular file and retain its file object.
#[cfg(windows)]
pub(super) fn create_direct_exclusive_file(path: &Path, description: &str) -> Result<File, String> {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("failed to create exclusive direct {description}: {error}"))?;
    validate_direct_file_metadata(
        &file
            .metadata()
            .map_err(|error| format!("failed to inspect direct {description}: {error}"))?,
        description,
    )?;
    Ok(file)
}

#[cfg(unix)]
pub(super) fn create_direct_exclusive_file(path: &Path, description: &str) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("failed to create exclusive direct {description}: {error}"))?;
    validate_direct_file_metadata(
        &file
            .metadata()
            .map_err(|error| format!("failed to inspect direct {description}: {error}"))?,
        description,
    )?;
    Ok(file)
}

#[cfg(windows)]
fn open_exclusive_namespace_lock(path: &Path, description: &'static str) -> Result<File, String> {
    // `share_mode(0)` denies read/write/delete opens by every other process.
    // A zero-access probe can still prove namespace identity without weakening
    // that exclusion. Windows releases authority when the handle closes.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    reject_existing_non_file_or_link(path, description)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| {
            format!(
                "{description} is already leased by another live Session or could not be opened exclusively: {error}"
            )
        })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect {description} authority file: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{description} authority is not a direct file"));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_exclusive_namespace_lock(path: &Path, description: &'static str) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("failed to open {description} authority file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect {description} authority file: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{description} authority is not a direct file"));
    }
    // SAFETY: `file` owns a valid descriptor for the lifetime of this call.
    // `flock` changes only the advisory lock state of that descriptor.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        return Err(format!(
            "{description} is already leased by another live Session or could not be locked exclusively: {error}"
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn reject_existing_non_file_or_link(path: &Path, description: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(format!("{description} authority is not a direct file"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to inspect {description} authority file: {error}"
        )),
    }
}

#[cfg(unix)]
fn namespace_identity_for_open_file(
    file: &File,
    description: &str,
) -> Result<NamespaceFileIdentity, String> {
    namespace_identity_from_metadata(
        &file.metadata().map_err(|error| {
            format!("failed to inspect held {description} authority file: {error}")
        })?,
        description,
    )
}

#[cfg(windows)]
fn namespace_identity_for_open_file(
    file: &File,
    description: &str,
) -> Result<NamespaceFileIdentity, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect held {description} authority file: {error}"))?;
    validate_direct_file_metadata(&metadata, description)?;
    namespace_identity_from_windows_handle(file, description)
}

#[cfg(unix)]
fn namespace_identity_for_path(
    path: &Path,
    description: &str,
) -> Result<NamespaceFileIdentity, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!("failed to inspect named {description} authority file: {error}")
    })?;
    namespace_identity_from_metadata(&metadata, description)
}

#[cfg(windows)]
fn namespace_identity_for_path(
    path: &Path,
    description: &str,
) -> Result<NamespaceFileIdentity, String> {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    let probe = OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("failed to open named {description} authority file: {error}"))?;
    namespace_identity_for_open_file(&probe, description)
}

#[cfg(unix)]
fn namespace_identity_from_metadata(
    metadata: &fs::Metadata,
    description: &str,
) -> Result<NamespaceFileIdentity, String> {
    validate_direct_file_metadata(metadata, description)?;
    if metadata.nlink() == 0 {
        return Err(format!(
            "{description} authority file is no longer linked into its namespace"
        ));
    }
    Ok(NamespaceFileIdentity { device: metadata.dev(), inode: metadata.ino() })
}

#[cfg(windows)]
fn namespace_identity_from_windows_handle(
    file: &File,
    description: &str,
) -> Result<NamespaceFileIdentity, String> {
    // SAFETY: `information` is an output-only POD Win32 structure and
    // `file.as_raw_handle()` remains valid for the complete call.
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: the handle belongs to the borrowed live `File`, and the output
    // pointer names initialized writable storage of the required type.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(format!(
            "failed to inspect {description} Windows authority identity: {}",
            std::io::Error::last_os_error()
        ));
    }
    if information.nNumberOfLinks == 0 {
        return Err(format!(
            "{description} authority file is no longer linked into its namespace"
        ));
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok(NamespaceFileIdentity {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index,
    })
}

fn validate_direct_file_metadata(metadata: &fs::Metadata, description: &str) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{description} is not a direct regular file"));
    }
    Ok(())
}

#[cfg(test)]
fn runtime_family_directory_name(identity: ProjectPathIdentity) -> std::ffi::OsString {
    format!("{PROJECT_RUNTIME_DIRECTORY_PREFIX}{identity}").into()
}

fn runtime_directory_name(
    identity: ProjectPathIdentity,
    project_id: ProjectId,
) -> std::ffi::OsString {
    format!(
        "{}{}{PROJECT_RUNTIME_DIRECTORY_IDENTITY_SEPARATOR}{project_id}",
        PROJECT_RUNTIME_DIRECTORY_PREFIX, identity
    )
    .into()
}

fn project_runtime_owner_guard() -> MutexGuard<'static, ()> {
    match PROJECT_RUNTIME_OWNER_LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "Project runtime owner lock was poisoned; recovering from durable authority"
            );
            poisoned.into_inner()
        }
    }
}

fn decode_hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("Project runtime path identity contains invalid hex".to_owned()),
    }
}

#[cfg(test)]
pub(super) fn claim_project_runtime_lease_for_test(
    base: &Path,
    project_file: &Path,
    project_id: ProjectId,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    claim_project_runtime_under(base, project_file, project_id)
}

#[cfg(test)]
pub(super) fn lease_existing_project_runtime_for_test(
    runtime_root: &Path,
    project_id: ProjectId,
    publication_target: &Path,
) -> Result<Arc<ProjectRuntimeLease>, String> {
    let base = runtime_root
        .parent()
        .ok_or_else(|| "test Project runtime root has no parent".to_owned())?;
    validate_direct_runtime_root(base, runtime_root)?;
    let authority_root = base.join(PROJECT_RUNTIME_AUTHORITY_DIRECTORY);
    let _guard = project_runtime_owner_guard();
    acquire_existing_project_runtime_unlocked(
        runtime_root,
        &authority_root,
        project_id,
        publication_target,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mondrian-project-runtime-{name}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).expect("system time").as_nanos()
        ))
    }

    #[test]
    fn different_native_paths_never_share_a_root_when_legacy_u64_is_forced_equal() {
        let base = unique_root("forced-legacy-collision");
        let left = base.join("left").join("project.mdp");
        let right = base.join("right").join("project.mdp");
        let project_id = ProjectId::new();

        // This deliberately models the old truncated authority as collided.
        let forced_legacy_u64 = 0x0123_4567_89ab_cdef_u64;
        let old_left = format!("mondrian_project_{forced_legacy_u64:x}");
        let old_right = format!("mondrian_project_{forced_legacy_u64:x}");
        assert_eq!(old_left, old_right);

        let new_left = project_runtime_root_under(&base, &left, project_id).expect("left identity");
        let new_right =
            project_runtime_root_under(&base, &right, project_id).expect("right identity");
        assert_ne!(new_left, new_right);
        assert_eq!(
            new_left.file_name().and_then(OsStr::to_str).map(str::len),
            Some(
                PROJECT_RUNTIME_DIRECTORY_PREFIX.len()
                    + SHA256_HEX_LENGTH
                    + PROJECT_RUNTIME_DIRECTORY_IDENTITY_SEPARATOR.len_utf8()
                    + project_id.to_string().len()
            )
        );
    }

    #[test]
    fn production_runtime_parent_leaf_is_one_product_owned_component() {
        let components =
            Path::new(PROJECT_RUNTIME_STATE_DIRECTORY).components().collect::<Vec<_>>();
        assert_eq!(
            components,
            vec![std::path::Component::Normal(OsStr::new(
                PROJECT_RUNTIME_STATE_DIRECTORY
            ))]
        );
    }

    #[test]
    fn lease_allocation_target_match_uses_owner_identity_not_retained_publications() {
        let base = unique_root("allocation-target-match");
        let original = base.join("original").join("project.mdp");
        let save_as = base.join("save-as").join("project.mdp");
        let lease =
            claim_project_runtime_under(&base, &original, ProjectId::new()).expect("claim runtime");

        assert!(lease.allocation_target_matches(&original).expect("validate allocation target"));
        lease
            .retain_publication_target(&save_as)
            .expect("retain Save As publication authority");
        assert!(
            !lease
                .allocation_target_matches(&save_as)
                .expect("validate retained non-allocation target"),
            "a retained Save As lock must not rewrite immutable runtime allocation identity"
        );

        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn closed_project_replaced_at_same_path_gets_a_distinct_root_without_removing_old_payload() {
        let base = unique_root("same-path-project-replacement");
        let project_file = base.join("project.mdp");
        let first_project = ProjectId::new();
        let other_project = ProjectId::new();
        let first_lease = claim_project_runtime_under(&base, &project_file, first_project)
            .expect("claim runtime");
        let first_runtime_root = first_lease.runtime_root().to_path_buf();
        let library_root = first_runtime_root.join("library");
        fs::create_dir_all(&library_root).expect("create library");
        let sentinel = library_root.join("foreign-project.db");
        fs::write(&sentinel, b"must survive").expect("write sentinel");

        let busy_error = claim_project_runtime_under(&base, &project_file, other_project)
            .expect_err("a second live lease must fail before any mutation");
        assert!(
            busy_error.contains("publication target") && busy_error.contains("already leased"),
            "unexpected same-path contention error: {busy_error}"
        );
        assert_eq!(
            fs::read(&sentinel).expect("sentinel survives contention"),
            b"must survive"
        );
        drop(first_lease);

        let replacement_lease = claim_project_runtime_under(&base, &project_file, other_project)
            .expect("closed publication path admits the replacement Project");
        let replacement_runtime_root = replacement_lease.runtime_root().to_path_buf();
        assert_ne!(replacement_runtime_root, first_runtime_root);
        assert_eq!(
            replacement_runtime_root,
            project_runtime_root_under(&base, &project_file, other_project)
                .expect("replacement runtime identity")
        );
        assert!(!replacement_runtime_root.join("library").exists());
        assert_eq!(
            fs::read(&sentinel).expect("old payload survives replacement allocation"),
            b"must survive"
        );
        validate_runtime_owner_readonly(&first_runtime_root, first_project)
            .expect("old immutable root retains its original owner");
        validate_runtime_owner_readonly(&replacement_runtime_root, other_project)
            .expect("replacement root has its own owner");

        drop(replacement_lease);
        let reopened_first = claim_project_runtime_under(&base, &project_file, first_project)
            .expect("the original Project can reacquire its exact immutable root");
        assert_eq!(reopened_first.runtime_root(), first_runtime_root.as_path());
        assert_eq!(
            fs::read(&sentinel).expect("old payload remains reusable by its owner"),
            b"must survive"
        );
        drop(reopened_first);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn kernel_lease_excludes_a_second_session_and_drop_releases_authority() {
        let base = unique_root("exclusive-lease");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        let first =
            claim_project_runtime_under(&base, &project_file, project_id).expect("first lease");
        let runtime_root = first.runtime_root().to_path_buf();
        assert!(runtime_root.join(PROJECT_RUNTIME_SESSION_LOCK_FILE).is_file());

        let error = claim_project_runtime_under(&base, &project_file, project_id)
            .expect_err("second lease must fail while first is live");
        assert!(
            error.contains("already leased"),
            "unexpected contention error: {error}"
        );

        drop(first);
        let reacquired = claim_project_runtime_under(&base, &project_file, project_id)
            .expect("kernel releases authority after Drop");
        assert_eq!(reacquired.runtime_root(), runtime_root.as_path());
        drop(reacquired);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn lock_only_incomplete_root_is_revalidated_and_durably_completed() {
        let base = unique_root("incomplete-root-retry");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        ensure_runtime_parent_directory(&base).expect("runtime parent");
        let runtime_root =
            project_runtime_root_under(&base, &project_file, project_id).expect("runtime root");
        create_durable_direct_child(&base, &runtime_root).expect("durable incomplete root");
        drop(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(session_lock_path(&runtime_root))
                .expect("persistent lock entry"),
        );

        let lease = claim_project_runtime_under(&base, &project_file, project_id)
            .expect("safe retry completes owner");

        assert_eq!(lease.runtime_root(), runtime_root);
        validate_runtime_owner_readonly(&runtime_root, project_id)
            .expect("retry publishes exact durable owner");
        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn malformed_owner_root_remains_inert_and_is_never_broadly_deleted() {
        let base = unique_root("inert-invalid-owner");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        ensure_runtime_parent_directory(&base).expect("runtime parent");
        let runtime_root =
            project_runtime_root_under(&base, &project_file, project_id).expect("runtime root");
        create_durable_direct_child(&base, &runtime_root).expect("durable root");
        fs::write(owner_manifest_path(&runtime_root), b"not-json").expect("malformed owner");
        let sentinel = runtime_root.join("unadmitted-payload");
        fs::write(&sentinel, b"preserve").expect("sentinel");

        let error = claim_project_runtime_under(&base, &project_file, project_id)
            .expect_err("malformed owner cannot admit payload");

        assert!(error.contains("invalid Project runtime owner manifest"));
        assert_eq!(
            fs::read(&sentinel).expect("inert payload survives"),
            b"preserve"
        );
        assert!(runtime_root.is_dir(), "inert root must not be rolled back");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn existing_foreign_runtime_directory_is_not_adopted_as_owned_state() {
        let base = unique_root("foreign-runtime-directory");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        ensure_runtime_parent_directory(&base).expect("runtime parent");
        let runtime_root =
            project_runtime_root_under(&base, &project_file, project_id).expect("runtime root");
        fs::create_dir(&runtime_root).expect("foreign direct directory");
        let sentinel = runtime_root.join("foreign-payload");
        fs::write(&sentinel, b"preserve").expect("foreign payload");

        let error = claim_project_runtime_under(&base, &project_file, project_id)
            .expect_err("directory durability evidence cannot establish domain ownership");

        assert!(
            error.contains("owner manifest is missing from a non-empty runtime"),
            "unexpected ownership rejection: {error}"
        );
        assert_eq!(
            fs::read(&sentinel).expect("foreign payload survives"),
            b"preserve"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn owner_publication_preserves_before_namespace_classification() {
        let base = unique_root("owner-before-namespace");
        fs::create_dir_all(&base).expect("base");
        let runtime_root = base.join("runtime");
        fs::create_dir(&runtime_root).expect("runtime");
        fs::create_dir(owner_manifest_path(&runtime_root)).expect("owner-path collision");
        let manifest =
            ProjectRuntimeOwnerManifest::new(ProjectPathIdentity([7; 32]), ProjectId::new());

        let error =
            publish_owner_manifest(&runtime_root, &manifest).expect_err("collision must fail");

        assert!(matches!(
            error,
            ProjectRuntimeOwnerPublicationFailure::BeforeNamespace(_)
        ));
        assert!(
            owner_manifest_path(&runtime_root).is_dir(),
            "pre-namespace failure must not replace or remove the colliding entry"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn recovery_child_collision_preserves_non_directory_entry() {
        let base = unique_root("durable-child-collision");
        let project_file = base.join("project.mdp");
        let lease =
            claim_project_runtime_under(&base, &project_file, ProjectId::new()).expect("lease");
        let autosave = lease.runtime_root().join("autosave");
        fs::write(&autosave, b"foreign").expect("foreign child");

        let error = ensure_owned_runtime_child_directory(&lease, &autosave)
            .expect_err("a recovery directory cannot replace a file");

        assert!(error.contains("not a direct filesystem directory"));
        assert_eq!(
            fs::read(&autosave).expect("foreign child survives"),
            b"foreign"
        );
        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn missing_runtime_parent_chain_is_durable_private_and_marked() {
        let container = unique_root("runtime-parent-chain");
        let runtime_parent = container.join("one").join("two").join("runtime");

        ensure_runtime_parent_directory(&runtime_parent).expect("durable runtime parent chain");

        assert!(runtime_parent.is_dir());
        assert_eq!(
            fs::read(runtime_parent.join(PROJECT_RUNTIME_PARENT_MARKER))
                .expect("durable parent marker"),
            PROJECT_RUNTIME_PARENT_MARKER_BYTES
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                container.join("one"),
                container.join("one").join("two"),
                runtime_parent.clone(),
            ] {
                let mode = fs::symlink_metadata(path).expect("private suffix").permissions().mode();
                assert_eq!(mode & 0o777, 0o700);
            }
        }
        let _ = fs::remove_dir_all(container);
    }

    #[test]
    fn runtime_parent_chain_collision_is_preserved() {
        let container = unique_root("runtime-parent-chain-collision");
        let collision = container.join("one");
        let runtime_parent = collision.join("runtime");
        fs::create_dir_all(&container).expect("collision parent");
        fs::write(&collision, b"foreign").expect("foreign collision");

        let error = ensure_runtime_parent_directory(&runtime_parent)
            .expect_err("non-directory chain collision fails closed");

        assert!(
            error.contains("wrong filesystem type")
                || error.contains("not a direct filesystem directory")
                || error.contains("not a direct directory"),
            "unexpected collision classification: {error}"
        );
        assert_eq!(
            fs::read(&collision).expect("foreign collision survives"),
            b"foreign"
        );
        assert!(!runtime_parent.exists());
        let _ = fs::remove_dir_all(container);
    }

    #[test]
    fn runtime_parent_marker_authorizes_idempotent_reentry() {
        let container = unique_root("runtime-parent-marker-reentry");
        let runtime_parent = container.join("runtime");
        ensure_runtime_parent_directory(&runtime_parent).expect("first establishment");

        ensure_runtime_parent_directory(&runtime_parent).expect("marker-backed reentry");

        assert_eq!(
            fs::read(runtime_parent.join(PROJECT_RUNTIME_PARENT_MARKER)).expect("refreshed marker"),
            PROJECT_RUNTIME_PARENT_MARKER_BYTES
        );
        let _ = fs::remove_dir_all(container);
    }

    #[test]
    fn unknown_runtime_parent_marker_never_grants_ownership() {
        let container = unique_root("runtime-parent-marker-collision");
        let runtime_parent = container.join("runtime");
        fs::create_dir_all(&container).expect("runtime parent container");
        fs::create_dir(&runtime_parent).expect("existing runtime parent");
        let marker = runtime_parent.join(PROJECT_RUNTIME_PARENT_MARKER);
        fs::write(&marker, b"foreign").expect("foreign marker");

        let error = ensure_runtime_parent_directory(&runtime_parent)
            .expect_err("unknown marker cannot be replaced");

        assert!(error.contains("unknown contents"));
        assert_eq!(
            fs::read(&marker).expect("foreign marker survives"),
            b"foreign"
        );
        let _ = fs::remove_dir_all(container);
    }

    #[test]
    fn markerless_existing_runtime_parent_remains_inert() {
        let container = unique_root("runtime-parent-marker-missing");
        let runtime_parent = container.join("runtime");
        fs::create_dir_all(&runtime_parent).expect("markerless runtime parent");
        let sentinel = runtime_parent.join("foreign-payload");
        fs::write(&sentinel, b"preserve").expect("foreign payload");

        let error = ensure_runtime_parent_directory(&runtime_parent)
            .expect_err("pathname presence cannot establish runtime-parent ownership");

        assert!(error.contains("no durable ownership marker"));
        assert_eq!(
            fs::read(&sentinel).expect("foreign payload survives"),
            b"preserve"
        );
        let _ = fs::remove_dir_all(container);
    }

    #[test]
    fn copied_paths_share_one_logical_lock_but_use_distinct_coordinated_payload_roots() {
        let base = unique_root("logical-project-authority");
        let left = base.join("left").join("project.mdp");
        let right = base.join("right").join("project-copy.mdp");
        let project_id = ProjectId::new();
        let first =
            claim_project_runtime_under(&base, &left, project_id).expect("first Project lease");

        let error = claim_project_runtime_under(&base, &right, project_id)
            .expect_err("copied aliases of one ProjectId must not run concurrently");
        assert!(
            error.contains("logical Project") && error.contains("already leased"),
            "unexpected logical-authority error: {error}"
        );

        let coordinated = claim_project_runtime_under_with_authority_and_logical(
            &base,
            &base.join(PROJECT_RUNTIME_AUTHORITY_DIRECTORY),
            &right,
            project_id,
            Some(Arc::clone(&first.logical_authority)),
        )
        .expect("the owning process may prepare the second path-paired root");
        assert_ne!(coordinated.runtime_root(), first.runtime_root());
        first.validate().expect("first root remains valid");
        coordinated.validate().expect("second root is independently leased");

        drop(coordinated);
        drop(first);
        let second = claim_project_runtime_under(&base, &right, project_id)
            .expect("logical authority releases after the first Session");
        drop(second);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn distinct_runtime_parents_share_one_stable_project_authority() {
        let container = unique_root("stable-cross-runtime-authority");
        let first_runtime_parent = container.join("runtime-a");
        let second_runtime_parent = container.join("runtime-b");
        let authority_root = container.join("stable-authority");
        let project_file = container.join("project.mdp");
        fs::create_dir_all(&container).expect("create test container");
        let project_id = ProjectId::new();
        let first = claim_project_runtime_under_with_authority(
            &first_runtime_parent,
            &authority_root,
            &project_file,
            project_id,
        )
        .expect("first stable authority");

        let error = claim_project_runtime_under_with_authority(
            &second_runtime_parent,
            &authority_root,
            &project_file,
            project_id,
        )
        .expect_err("different runtime parents must not split logical Project authority");
        assert!(
            error.contains("logical Project") && error.contains("already leased"),
            "unexpected stable-authority error: {error}"
        );
        drop(first);
        let _ = fs::remove_dir_all(container);
    }

    #[test]
    fn existing_runtime_lease_rejects_a_second_authority_parent() {
        let base = unique_root("foreign-authority-parent");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        let lease = claim_project_runtime_under(&base, &project_file, project_id)
            .expect("claim test lease");
        let runtime_root = lease.runtime_root().to_path_buf();
        drop(lease);

        let error = lease_existing_project_runtime(&runtime_root, project_id, &project_file)
            .expect_err("production lease must not create authority under an arbitrary parent");
        assert!(error.contains("outside the canonical runtime authority namespace"));
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_absolute_xdg_state_home_is_preferred_for_recovery_payload() {
        let container = unique_root("xdg-state-home");
        let xdg_state_home = container.join("xdg-state");
        let home = container.join("home");
        assert_eq!(
            unix_state_home(Some(xdg_state_home.as_os_str()), Some(home.as_os_str()))
                .expect("absolute XDG state home"),
            xdg_state_home
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_relative_or_missing_xdg_state_home_falls_back_to_home_state() {
        let container = unique_root("xdg-state-fallback");
        let home = container.join("home");
        let expected = home.join(".local").join("state");
        assert_eq!(
            unix_state_home(Some(OsStr::new("relative-state")), Some(home.as_os_str()))
                .expect("HOME fallback"),
            expected
        );
        assert_eq!(
            unix_state_home(None, Some(home.as_os_str())).expect("HOME fallback"),
            expected
        );
        assert!(
            unix_state_home(None, Some(OsStr::new("relative-home"))).is_err(),
            "a relative fallback must not create process-working-directory recovery state"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_runtime_parent_is_repaired_and_held_at_mode_0700() {
        use std::os::unix::fs::PermissionsExt;

        let container = unique_root("runtime-parent-mode");
        let runtime_parent = container.join("runtime");
        ensure_runtime_parent_directory(&runtime_parent).expect("establish runtime parent");
        fs::set_permissions(&runtime_parent, fs::Permissions::from_mode(0o755))
            .expect("make runtime parent too permissive");

        ensure_runtime_parent_directory(&runtime_parent)
            .expect("current user's runtime parent can be secured");

        let metadata = fs::symlink_metadata(&runtime_parent).expect("inspect runtime parent");
        assert_eq!(metadata.uid(), effective_user_id());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        let _ = fs::remove_dir_all(container);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_runtime_parent_alias_is_not_a_second_authority_namespace() {
        use std::os::unix::fs::symlink;

        let container = unique_root("runtime-parent-alias");
        let runtime_parent = container.join("runtime-parent");
        let alias_parent = container.join("runtime-parent-alias");
        fs::create_dir_all(&runtime_parent).expect("create canonical runtime parent");
        symlink(&runtime_parent, &alias_parent).expect("create runtime parent alias");

        validate_direct_runtime_root(&runtime_parent, &alias_parent.join("mondrian_identity"))
            .expect("an alias of the canonical parent must retain one authority namespace");
        let _ = fs::remove_dir_all(container);
    }

    #[cfg(unix)]
    #[test]
    fn unix_symlinked_parent_aliases_share_publication_authority() {
        use std::os::unix::fs::symlink;

        let base = unique_root("unix-publication-alias");
        let real_parent = base.join("real");
        let alias_parent = base.join("alias");
        fs::create_dir_all(&real_parent).expect("create real publication parent");
        symlink(&real_parent, &alias_parent).expect("create publication parent alias");
        let real_target = real_parent.join("project.mdp");
        let alias_target = alias_parent.join("project.mdp");
        assert_eq!(
            ProjectPathIdentity::from_project_file(&real_target).expect("real identity"),
            ProjectPathIdentity::from_project_file(&alias_target).expect("alias identity")
        );

        let first = claim_project_runtime_under(&base, &real_target, ProjectId::new())
            .expect("first publication authority");
        let error = claim_project_runtime_under(&base, &alias_target, ProjectId::new())
            .expect_err("symlink aliases must share one publication authority");
        assert!(
            error.contains("publication target") && error.contains("already leased"),
            "unexpected publication-authority error: {error}"
        );
        drop(first);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn unix_retargeted_publication_parent_invalidates_the_live_lease() {
        use std::os::unix::fs::symlink;

        let base = unique_root("unix-publication-retarget");
        let first_parent = base.join("first");
        let second_parent = base.join("second");
        let alias_parent = base.join("alias");
        fs::create_dir_all(&first_parent).expect("create first publication parent");
        fs::create_dir_all(&second_parent).expect("create second publication parent");
        symlink(&first_parent, &alias_parent).expect("create publication parent alias");
        let alias_target = alias_parent.join("project.mdp");
        let lease = claim_project_runtime_under(&base, &alias_target, ProjectId::new())
            .expect("claim aliased publication authority");

        fs::remove_file(&alias_parent).expect("remove first alias");
        symlink(&second_parent, &alias_parent).expect("retarget publication parent alias");
        let error = lease
            .validate()
            .expect_err("retargeted namespace route must invalidate publication authority");
        assert!(
            error.contains("publication target namespace changed"),
            "unexpected retarget error: {error}"
        );
        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_and_trailing_dot_aliases_share_publication_authority() {
        let base = unique_root("windows-publication-alias");
        let upper = base.join("EDIT").join("Project.MDP");
        let lower = base.join("edit").join("project.mdp.");
        let upper_identity =
            ProjectPathIdentity::from_project_file(&upper).expect("upper identity");
        let lower_identity =
            ProjectPathIdentity::from_project_file(&lower).expect("lower identity");
        assert_eq!(upper_identity, lower_identity);

        let first_project = ProjectId::new();
        let other_project = ProjectId::new();
        let first = claim_project_runtime_under(&base, &upper, first_project)
            .expect("first publication authority");
        let error = claim_project_runtime_under(&base, &lower, other_project)
            .expect_err("another ProjectId must not claim the same Win32 target alias");
        assert!(
            error.contains("publication target") && error.contains("already leased"),
            "unexpected publication-authority error: {error}"
        );
        drop(first);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn unix_unlinked_and_replaced_lock_entry_invalidates_the_live_lease() {
        let base = unique_root("unix-lock-replacement");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        let lease =
            claim_project_runtime_under(&base, &project_file, project_id).expect("claim lease");
        let project_lock = lease.logical_authority.project_lock.path.clone();

        fs::remove_file(&project_lock).expect("Unix permits unlink of a flocked file");
        fs::write(&project_lock, b"replacement inode").expect("replace lock namespace entry");

        let error = lease
            .validate()
            .expect_err("a replaced namespace entry must invalidate held authority");
        assert!(
            error.contains("no longer names the held kernel object")
                || error.contains("no longer linked"),
            "unexpected replacement error: {error}"
        );
        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn windows_authority_handle_denies_namespace_entry_deletion() {
        let base = unique_root("windows-lock-delete-denied");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        let lease =
            claim_project_runtime_under(&base, &project_file, project_id).expect("claim lease");

        fs::remove_file(&lease.logical_authority.project_lock.path)
            .expect_err("delete sharing must remain denied while authority is live");
        lease.validate().expect("failed deletion must not weaken authority");

        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn leased_mutation_rejects_a_child_from_another_root() {
        let base = unique_root("wrong-root");
        let project_file = base.join("project.mdp");
        let project_id = ProjectId::new();
        let lease =
            claim_project_runtime_under(&base, &project_file, project_id).expect("lease runtime");
        let foreign_root = base.join("foreign-runtime");
        fs::create_dir_all(&foreign_root).expect("create foreign root");
        let foreign_child = foreign_root.join("library");

        let error = create_ephemeral_owned_runtime_child_directory(&lease, &foreign_child)
            .expect_err("lease must not authorize another root");
        assert!(error.contains("not a permitted direct child"));
        assert!(!foreign_child.exists());
        drop(lease);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_paths_use_exact_native_bytes_not_lossy_text() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let base = unique_root("unix-native");
        let left = base.join(OsString::from_vec(vec![b'p', 0xff]));
        let right = base.join(OsString::from_vec(vec![b'p', 0xfe]));
        assert_eq!(left.to_string_lossy(), right.to_string_lossy());
        assert_ne!(
            ProjectPathIdentity::from_normalized_native_path(&left),
            ProjectPathIdentity::from_normalized_native_path(&right)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ill_formed_utf16_paths_use_exact_native_units_not_lossy_text() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let base = unique_root("windows-native");
        let left = base.join(OsString::from_wide(&[b'p' as u16, 0xd800]));
        let right = base.join(OsString::from_wide(&[b'p' as u16, 0xd801]));
        assert_eq!(left.to_string_lossy(), right.to_string_lossy());
        assert_ne!(
            ProjectPathIdentity::from_normalized_native_path(&left),
            ProjectPathIdentity::from_normalized_native_path(&right)
        );
    }
}
