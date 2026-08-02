//! Native crash-consistent filesystem publication primitives.
//!
//! This crate is deliberately domain-free. It owns the single implementation
//! of sibling temporary-object identity, atomic file namespace publication,
//! postcondition classification, parent-directory durability, and durable
//! direct-child directory creation used by Project, Recovery, Export, and
//! regenerable media caches.

use anyhow::Context;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as UnixMetadataExt, OpenOptionsExt as UnixOpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as WindowsOpenOptionsExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};

/// Final namespace semantics for one atomic file publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePublicationMode {
    /// Atomically replace an existing direct file, or create it when absent.
    ReplaceExisting,
    /// Atomically create the target without replacing any existing entry.
    CreateNew,
}

/// Durable completion evidence for one exact file namespace publication.
#[derive(Debug, PartialEq, Eq)]
pub struct FilePublicationEvidence {
    published_path: PathBuf,
    mode: FilePublicationMode,
}

impl FilePublicationEvidence {
    /// Absolute target path of the durable publication.
    pub fn published_path(&self) -> &Path {
        &self.published_path
    }

    /// Namespace semantics used by the publication.
    pub fn mode(&self) -> FilePublicationMode {
        self.mode
    }
}

/// Evidence that a namespace operation completed without confirmed directory
/// durability.
#[derive(Debug)]
pub struct FilePublicationDurabilityUnconfirmed {
    published_path: PathBuf,
    mode: FilePublicationMode,
    source: std::io::Error,
}

impl FilePublicationDurabilityUnconfirmed {
    /// Absolute path currently observed to name the new object.
    pub fn published_path(&self) -> &Path {
        &self.published_path
    }

    /// Namespace semantics used by the completed operation.
    pub fn mode(&self) -> FilePublicationMode {
        self.mode
    }
}

impl fmt::Display for FilePublicationDurabilityUnconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "file namespace was published at {}, but crash durability is unconfirmed: {}",
            self.published_path.display(),
            self.source
        )
    }
}

impl std::error::Error for FilePublicationDurabilityUnconfirmed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Evidence that object-identity postconditions cannot prove either an
/// unchanged or a published namespace.
#[derive(Debug)]
pub struct FilePublicationNamespaceIndeterminate {
    intended_path: PathBuf,
    mode: FilePublicationMode,
    retained_new_path: Option<PathBuf>,
    source: std::io::Error,
}

impl FilePublicationNamespaceIndeterminate {
    /// Absolute intended publication target.
    pub fn intended_path(&self) -> &Path {
        &self.intended_path
    }

    /// Requested namespace semantics.
    pub fn mode(&self) -> FilePublicationMode {
        self.mode
    }

    /// Verified surviving source name for the new bytes, when observed.
    pub fn retained_new_path(&self) -> Option<&Path> {
        self.retained_new_path.as_deref()
    }
}

impl fmt::Display for FilePublicationNamespaceIndeterminate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "file publication at {} has an indeterminate namespace postcondition; automatic cleanup was disabled",
            self.intended_path.display()
        )?;
        if let Some(path) = &self.retained_new_path {
            write!(
                formatter,
                " and verified new bytes remain at {}",
                path.display()
            )?;
        }
        write!(formatter, ": {}", self.source)
    }
}

impl std::error::Error for FilePublicationNamespaceIndeterminate {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Exhaustive failure states for atomic file publication.
#[derive(Debug)]
pub enum FilePublicationFailure {
    /// No irreversible namespace operation is known to have completed.
    BeforeNamespace(anyhow::Error),
    /// The target names the new object, but containing-directory durability is
    /// unconfirmed.
    DurabilityUnconfirmed(FilePublicationDurabilityUnconfirmed),
    /// The namespace postcondition cannot be proven.
    NamespaceIndeterminate(FilePublicationNamespaceIndeterminate),
}

impl fmt::Display for FilePublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeNamespace(error) => write!(formatter, "{error:#}"),
            Self::DurabilityUnconfirmed(error) => error.fmt(formatter),
            Self::NamespaceIndeterminate(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for FilePublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BeforeNamespace(error) => Some(error.as_ref()),
            Self::DurabilityUnconfirmed(error) => Some(error),
            Self::NamespaceIndeterminate(error) => Some(error),
        }
    }
}

impl From<anyhow::Error> for FilePublicationFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::BeforeNamespace(error)
    }
}

impl From<std::io::Error> for FilePublicationFailure {
    fn from(error: std::io::Error) -> Self {
        Self::BeforeNamespace(error.into())
    }
}

/// Durable completion evidence for one direct-child directory creation.
#[derive(Debug, PartialEq, Eq)]
pub struct DirectoryPublicationEvidence {
    path: PathBuf,
}

impl DirectoryPublicationEvidence {
    /// Absolute path of the durable direct-child directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Exhaustive failure states for direct-child directory creation.
#[derive(Debug)]
pub enum DirectoryPublicationFailure {
    /// The final child namespace is proven not to contain the new directory.
    BeforeNamespace(anyhow::Error),
    /// A direct directory is visible at the final child, but durable
    /// construction is unconfirmed.
    DurabilityUnconfirmed {
        /// Absolute child path.
        path: PathBuf,
        /// Platform failure.
        source: std::io::Error,
    },
    /// Directory namespace postconditions are indeterminate.
    NamespaceIndeterminate {
        /// Intended absolute child path.
        intended_path: PathBuf,
        /// Verified retained staging path, when observed.
        retained_staging_path: Option<PathBuf>,
        /// Platform failure.
        source: std::io::Error,
    },
}

impl fmt::Display for DirectoryPublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeNamespace(error) => write!(formatter, "{error:#}"),
            Self::DurabilityUnconfirmed { path, source } => write!(
                formatter,
                "a direct directory is visible at {}, but durable construction is unconfirmed: {source}",
                path.display()
            ),
            Self::NamespaceIndeterminate {
                intended_path,
                retained_staging_path,
                source,
            } => {
                write!(
                    formatter,
                    "directory publication at {} has an indeterminate namespace postcondition",
                    intended_path.display()
                )?;
                if let Some(path) = retained_staging_path {
                    write!(formatter, "; staging remains at {}", path.display())?;
                }
                write!(formatter, ": {source}")
            }
        }
    }
}

impl std::error::Error for DirectoryPublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BeforeNamespace(error) => Some(error.as_ref()),
            Self::DurabilityUnconfirmed { source, .. }
            | Self::NamespaceIndeterminate { source, .. } => Some(source),
        }
    }
}

impl From<anyhow::Error> for DirectoryPublicationFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::BeforeNamespace(error)
    }
}

impl From<std::io::Error> for DirectoryPublicationFailure {
    fn from(error: std::io::Error) -> Self {
        Self::BeforeNamespace(error.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(unix)]
struct ObjectIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(windows)]
struct ObjectIdentity {
    volume_serial_number: u32,
    file_index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(not(any(unix, windows)))]
struct ObjectIdentity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectKind {
    File,
    Directory,
}

/// Owned direct file object staged next to one immutable publication target.
///
/// The guard cleans only a namespace path that still names its exact recorded
/// object at identity validation. Indeterminate postconditions disarm cleanup
/// and preserve every possible surviving copy. This is not atomic
/// delete-by-handle authority against a hostile continuous directory race.
pub struct OwnedPublicationFile {
    path: PathBuf,
    target: PathBuf,
    file: Option<File>,
    identity: ObjectIdentity,
    owns_path: bool,
    retain_source_on_failure: bool,
}

/// Identity-bound reservation temporarily released to an external writer.
///
/// The writer receives only the unique path. Reclaiming reopens that path
/// without following a leaf link and proves that it still names the originally
/// allocated file object before validation or publication may continue.
/// Reclaim is a postcondition check; it cannot make a path-only external writer
/// immune to a hostile continuous namespace race.
pub struct ExternalPublicationReservation {
    path: PathBuf,
    target: PathBuf,
    identity: ObjectIdentity,
    owns_path: bool,
}

impl OwnedPublicationFile {
    /// Create a unique direct sibling temporary file for `target`.
    pub fn create_sibling(target: &Path, purpose: &str) -> anyhow::Result<Self> {
        let target = absolute_publication_target(target)?;
        validate_existing_parent(&target)?;
        for _ in 0..16 {
            let path = temporary_sibling_path(&target, purpose);
            match create_direct_exclusive_file(&path) {
                Ok(file) => {
                    let identity = identity_for_open_file(&file, ObjectKind::File)?;
                    return Ok(Self {
                        path,
                        target,
                        file: Some(file),
                        identity,
                        owns_path: true,
                        retain_source_on_failure: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!(
            "failed to allocate a unique sibling temporary file for {}",
            target.display()
        )
    }

    /// Absolute staging path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Retained handle for consumers that must preserve exact object identity
    /// instead of reopening the staging pathname.
    pub fn file(&self) -> anyhow::Result<&File> {
        self.file.as_ref().context("owned publication handle is unavailable")
    }

    /// Retained writer handle before publication.
    pub fn file_mut(&mut self) -> anyhow::Result<&mut File> {
        self.file.as_mut().context("owned publication handle is unavailable")
    }

    /// Transfer this staging object to an external path-based writer.
    pub fn release_for_external_writer(mut self) -> ExternalPublicationReservation {
        drop(self.file.take());
        self.owns_path = false;
        ExternalPublicationReservation {
            path: self.path.clone(),
            target: self.target.clone(),
            identity: self.identity,
            owns_path: true,
        }
    }

    /// Preserve the exact staging source when publication fails before the
    /// irreversible namespace boundary.
    ///
    /// The caller must capture [`Self::path`] before consuming the guard and
    /// must surface the retained artifact explicitly. Successful publication
    /// and indeterminate outcomes still follow their typed namespace rules.
    pub fn preserve_source_on_before_namespace_failure(mut self) -> Self {
        self.retain_source_on_failure = true;
        self
    }

    /// Publish this exact file object at its immutable target.
    pub fn publish(
        self,
        mode: FilePublicationMode,
    ) -> Result<FilePublicationEvidence, FilePublicationFailure> {
        self.publish_with_parent_sync(mode, sync_parent_directory)
    }

    fn publish_with_parent_sync(
        mut self,
        mode: FilePublicationMode,
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<FilePublicationEvidence, FilePublicationFailure> {
        self.file_mut()?.sync_all()?;
        self.revalidate_namespace()?;
        let target_exists = admit_target_shape(&self.target, mode)?;
        let replaced_identity = if target_exists && mode == FilePublicationMode::ReplaceExisting {
            Some(identity_for_path(&self.target, ObjectKind::File)?)
        } else {
            None
        };
        #[cfg(unix)]
        if target_exists && mode == FilePublicationMode::ReplaceExisting {
            let permissions = fs::symlink_metadata(&self.target)?.permissions();
            fs::set_permissions(&self.path, permissions)?;
            self.file_mut()?.sync_all()?;
        }

        let outcome = publish_file_atomically(
            &self.path,
            &self.target,
            mode,
            target_exists,
            self.identity,
            replaced_identity,
        )
        .with_context(|| {
            format!(
                "failed to atomically publish {} as {}",
                self.path.display(),
                self.target.display()
            )
        })?;
        match outcome {
            AtomicPublicationOutcome::Published(disposition) => {
                self.apply_disposition(disposition);
                match sync_parent(&self.target) {
                    Ok(()) => {
                        Ok(FilePublicationEvidence { published_path: self.target.clone(), mode })
                    }
                    Err(source) => Err(FilePublicationFailure::DurabilityUnconfirmed(
                        FilePublicationDurabilityUnconfirmed {
                            published_path: self.target.clone(),
                            mode,
                            source,
                        },
                    )),
                }
            }
            AtomicPublicationOutcome::PublishedDurabilityUnconfirmed { disposition, source } => {
                self.apply_disposition(disposition);
                Err(FilePublicationFailure::DurabilityUnconfirmed(
                    FilePublicationDurabilityUnconfirmed {
                        published_path: self.target.clone(),
                        mode,
                        source,
                    },
                ))
            }
            AtomicPublicationOutcome::NamespaceIndeterminate { retained_new_path, source } => {
                self.owns_path = false;
                Err(FilePublicationFailure::NamespaceIndeterminate(
                    FilePublicationNamespaceIndeterminate {
                        intended_path: self.target.clone(),
                        mode,
                        retained_new_path,
                        source,
                    },
                ))
            }
        }
    }

    fn apply_disposition(&mut self, disposition: SourceDisposition) {
        match disposition {
            SourceDisposition::Moved => self.owns_path = false,
            #[cfg(unix)]
            SourceDisposition::LinkedNeedsCleanup => {
                self.retain_source_on_failure = false;
                self.cleanup_if_still_owned();
            }
        }
    }

    fn revalidate_namespace(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            identity_for_path(&self.path, ObjectKind::File)? == self.identity,
            "publication staging path no longer names its owned file object: {}",
            self.path.display()
        );
        Ok(())
    }

    fn path_still_names_owned_object(&self) -> anyhow::Result<bool> {
        match identity_for_path(&self.path, ObjectKind::File) {
            Ok(identity) => Ok(identity == self.identity),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn cleanup_if_still_owned(&mut self) {
        if !self.owns_path {
            return;
        }
        if !matches!(self.path_still_names_owned_object(), Ok(true)) {
            return;
        }
        drop(self.file.take());
        if matches!(self.path_still_names_owned_object(), Ok(true))
            && fs::remove_file(&self.path).is_ok()
        {
            self.owns_path = false;
        }
    }
}

impl Drop for OwnedPublicationFile {
    fn drop(&mut self) {
        if self.retain_source_on_failure {
            return;
        }
        self.cleanup_if_still_owned();
    }
}

impl ExternalPublicationReservation {
    /// Unique absolute path supplied to the external writer.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reclaim the exact staged object after the external writer exits.
    pub fn reclaim(mut self) -> anyhow::Result<OwnedPublicationFile> {
        let file = open_direct_exclusive_file(&self.path)?;
        let observed = identity_for_open_file(&file, ObjectKind::File)?;
        anyhow::ensure!(
            observed == self.identity,
            "external writer staging path no longer names its reserved file object: {}",
            self.path.display()
        );
        self.owns_path = false;
        Ok(OwnedPublicationFile {
            path: self.path.clone(),
            target: self.target.clone(),
            file: Some(file),
            identity: self.identity,
            owns_path: true,
            retain_source_on_failure: false,
        })
    }

    fn cleanup_if_still_owned(&mut self) {
        if !self.owns_path {
            return;
        }
        if identity_for_path(&self.path, ObjectKind::File).ok() != Some(self.identity) {
            return;
        }
        if fs::remove_file(&self.path).is_ok() {
            self.owns_path = false;
        }
    }
}

impl Drop for ExternalPublicationReservation {
    fn drop(&mut self) {
        self.cleanup_if_still_owned();
    }
}

/// Publish arbitrary bytes through the shared typed atomic-file boundary.
pub fn write_durable_file_atomically(
    target: &Path,
    bytes: &[u8],
) -> Result<FilePublicationEvidence, FilePublicationFailure> {
    write_durable_file_atomically_with_mode(target, bytes, FilePublicationMode::ReplaceExisting)
}

/// Publish arbitrary bytes with explicit create-or-replace namespace
/// semantics.
pub fn write_durable_file_atomically_with_mode(
    target: &Path,
    bytes: &[u8],
    mode: FilePublicationMode,
) -> Result<FilePublicationEvidence, FilePublicationFailure> {
    let mut staging = OwnedPublicationFile::create_sibling(target, "publication")?;
    staging.file_mut()?.write_all(bytes)?;
    staging.publish(mode)
}

/// Create one exact direct-child directory and durably publish its namespace.
///
/// `parent` must already exist as a direct directory and `child` must be its
/// immediate child. Existing directory presence is not durable evidence:
/// callers with domain-owned recovery authority must validate that authority
/// separately rather than asking this domain-free primitive to adopt it.
pub fn create_durable_direct_child(
    parent: &Path,
    child: &Path,
) -> Result<DirectoryPublicationEvidence, DirectoryPublicationFailure> {
    let parent = std::path::absolute(parent)
        .context("failed to make directory publication parent absolute")?;
    let child = std::path::absolute(child)
        .context("failed to make directory publication child absolute")?;
    validate_direct_directory(&parent)?;
    if child.parent() != Some(parent.as_path()) || child.file_name().is_none() {
        return Err(
            anyhow::anyhow!("durable directory publication requires one direct child").into(),
        );
    }

    match direct_directory_identity_if_present(&child) {
        Ok(Some(_)) => {
            return Err(DirectoryPublicationFailure::DurabilityUnconfirmed {
                path: child,
                source: std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "existing direct directory has no construction evidence in this attempt",
                ),
            });
        }
        Ok(None) => {}
        Err(error) => return Err(DirectoryPublicationFailure::BeforeNamespace(error)),
    }

    #[cfg(unix)]
    {
        create_durable_direct_child_unix_with(
            &parent,
            &child,
            |path| identity_for_path(path, ObjectKind::Directory),
            |parent, _| sync_directory(parent),
        )
    }

    #[cfg(windows)]
    {
        create_durable_direct_child_windows(&parent, &child)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, child);
        Err(DirectoryPublicationFailure::BeforeNamespace(
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "durable directory publication is unsupported on this platform",
            )
            .into(),
        ))
    }
}

/// Create every missing direct-child directory from one trusted existing
/// absolute anchor to an absolute target.
///
/// The anchor is caller-selected trust input and must already be a direct
/// directory. The target must be a strict descendant. Every suffix component
/// is published through [`create_durable_direct_child`], so an unexpected
/// existing node fails closed as `DurabilityUnconfirmed` rather than being
/// upgraded from pathname presence.
pub fn ensure_durable_directory_chain(
    trusted_existing_anchor: &Path,
    target: &Path,
) -> Result<DirectoryPublicationEvidence, DirectoryPublicationFailure> {
    let anchor = std::path::absolute(trusted_existing_anchor)
        .context("failed to make durable directory-chain anchor absolute")?;
    let target = std::path::absolute(target)
        .context("failed to make durable directory-chain target absolute")?;
    validate_direct_directory(&anchor)?;
    let suffix = target.strip_prefix(&anchor).map_err(|_| {
        DirectoryPublicationFailure::BeforeNamespace(anyhow::anyhow!(
            "durable directory-chain target is outside its trusted existing anchor"
        ))
    })?;
    if suffix.as_os_str().is_empty() {
        return Err(DirectoryPublicationFailure::BeforeNamespace(
            anyhow::anyhow!("durable directory-chain target must be below its trusted anchor"),
        ));
    }

    let mut parent = anchor;
    let mut evidence = None;
    for component in suffix.components() {
        let std::path::Component::Normal(leaf) = component else {
            return Err(DirectoryPublicationFailure::BeforeNamespace(
                anyhow::anyhow!("durable directory-chain suffix is not normalized"),
            ));
        };
        let child = parent.join(leaf);
        evidence = Some(create_durable_direct_child(&parent, &child)?);
        parent = child;
    }
    evidence.ok_or_else(|| {
        DirectoryPublicationFailure::BeforeNamespace(anyhow::anyhow!(
            "durable directory-chain target has no suffix"
        ))
    })
}

fn direct_directory_identity_if_present(
    path: &Path,
) -> Result<Option<ObjectIdentity>, anyhow::Error> {
    match identity_for_path(path, ObjectKind::Directory) {
        Ok(identity) => Ok(Some(identity)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn create_durable_direct_child_unix_with(
    parent: &Path,
    child: &Path,
    observe_identity: impl Fn(&Path) -> anyhow::Result<ObjectIdentity>,
    barrier: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<DirectoryPublicationEvidence, DirectoryPublicationFailure> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(child) {
        Ok(()) => {
            let identity = observe_identity(child).map_err(|error| {
                DirectoryPublicationFailure::NamespaceIndeterminate {
                    intended_path: child.to_path_buf(),
                    retained_staging_path: None,
                    source: std::io::Error::other(format!(
                        "new direct-child identity could not be observed after create_dir succeeded: {error:#}"
                    )),
                }
            })?;
            match barrier(parent, child) {
                Ok(()) => match identity_for_path(child, ObjectKind::Directory) {
                    Ok(observed) if observed == identity => {
                        Ok(DirectoryPublicationEvidence { path: child.to_path_buf() })
                    }
                    observed => Err(DirectoryPublicationFailure::NamespaceIndeterminate {
                        intended_path: child.to_path_buf(),
                        retained_staging_path: None,
                        source: std::io::Error::other(format!(
                            "new direct-child identity changed or became unobservable after its parent durability barrier: {observed:?}"
                        )),
                    }),
                },
                Err(source) => match identity_for_path(child, ObjectKind::Directory) {
                    Ok(observed) if observed == identity => {
                        Err(DirectoryPublicationFailure::DurabilityUnconfirmed {
                            path: child.to_path_buf(),
                            source,
                        })
                    }
                    observed => Err(DirectoryPublicationFailure::NamespaceIndeterminate {
                        intended_path: child.to_path_buf(),
                        retained_staging_path: None,
                        source: std::io::Error::other(format!(
                            "new direct-child identity changed or became unobservable after a failed parent durability barrier ({source}): {observed:?}"
                        )),
                    }),
                },
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match observe_identity(child) {
                Ok(_) => Err(DirectoryPublicationFailure::DurabilityUnconfirmed {
                    path: child.to_path_buf(),
                    source: error,
                }),
                Err(observe_error) => {
                    Err(DirectoryPublicationFailure::NamespaceIndeterminate {
                        intended_path: child.to_path_buf(),
                        retained_staging_path: None,
                        source: std::io::Error::other(format!(
                            "create_dir collided and the existing direct-child identity is not provable: {observe_error:#}"
                        )),
                    })
                }
            }
        }
        Err(error) => Err(DirectoryPublicationFailure::BeforeNamespace(error.into())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceDisposition {
    Moved,
    #[cfg(unix)]
    LinkedNeedsCleanup,
}

#[derive(Debug)]
enum AtomicPublicationOutcome {
    Published(SourceDisposition),
    PublishedDurabilityUnconfirmed {
        disposition: SourceDisposition,
        source: std::io::Error,
    },
    NamespaceIndeterminate {
        retained_new_path: Option<PathBuf>,
        source: std::io::Error,
    },
}

fn absolute_publication_target(target: &Path) -> anyhow::Result<PathBuf> {
    let target = std::path::absolute(target)?;
    anyhow::ensure!(
        target.file_name().is_some(),
        "publication target has no file name: {}",
        target.display()
    );
    Ok(target)
}

fn publication_parent(target: &Path) -> &Path {
    target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn validate_existing_parent(target: &Path) -> anyhow::Result<()> {
    validate_direct_directory(publication_parent(target))
}

fn validate_direct_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && metadata.is_dir(),
        "path is not a direct filesystem directory: {}",
        path.display()
    );
    Ok(())
}

fn admit_target_shape(target: &Path, mode: FilePublicationMode) -> anyhow::Result<bool> {
    match fs::symlink_metadata(target) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || (mode == FilePublicationMode::ReplaceExisting && !metadata.is_file()) =>
        {
            anyhow::bail!(
                "publication target is not a direct regular file: {}",
                target.display()
            )
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn temporary_sibling_path(target: &Path, purpose: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    static HASHER: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let hash_builder = HASHER.get_or_init(std::collections::hash_map::RandomState::new);
    let digest = |domain: u8| {
        let mut hasher = hash_builder.build_hasher();
        domain.hash(&mut hasher);
        target.hash(&mut hasher);
        purpose.hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        nonce.hash(&mut hasher);
        hasher.finish()
    };
    let leaf = format!(".m-{purpose}-{:016x}{:016x}.tmp", digest(0), digest(1));
    publication_parent(target).join(leaf)
}

#[cfg(unix)]
fn create_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn create_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn create_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn open_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

#[cfg(unix)]
fn identity_for_open_file(file: &File, kind: ObjectKind) -> anyhow::Result<ObjectIdentity> {
    identity_from_metadata(&file.metadata()?, kind)
}

#[cfg(unix)]
fn identity_for_path(path: &Path, kind: ObjectKind) -> anyhow::Result<ObjectIdentity> {
    identity_from_metadata(&fs::symlink_metadata(path)?, kind)
}

#[cfg(unix)]
fn identity_from_metadata(
    metadata: &fs::Metadata,
    kind: ObjectKind,
) -> anyhow::Result<ObjectIdentity> {
    anyhow::ensure!(!metadata.file_type().is_symlink(), "object is a symlink");
    anyhow::ensure!(
        match kind {
            ObjectKind::File => metadata.is_file(),
            ObjectKind::Directory => metadata.is_dir(),
        },
        "object has the wrong filesystem type"
    );
    anyhow::ensure!(metadata.nlink() > 0, "object is no longer linked");
    Ok(ObjectIdentity { device: metadata.dev(), inode: metadata.ino() })
}

#[cfg(windows)]
fn identity_for_open_file(file: &File, kind: ObjectKind) -> anyhow::Result<ObjectIdentity> {
    let metadata = file.metadata()?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "object is a reparse point"
    );
    anyhow::ensure!(
        match kind {
            ObjectKind::File => metadata.is_file(),
            ObjectKind::Directory => metadata.is_dir(),
        },
        "object has the wrong filesystem type"
    );
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: output storage and the borrowed raw handle remain valid.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to inspect object identity");
    }
    anyhow::ensure!(information.nNumberOfLinks > 0, "object is no longer linked");
    Ok(ObjectIdentity {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
    })
}

#[cfg(windows)]
fn identity_for_path(path: &Path, kind: ObjectKind) -> anyhow::Result<ObjectIdentity> {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if kind == ObjectKind::Directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let file = OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)?;
    identity_for_open_file(&file, kind)
}

#[cfg(not(any(unix, windows)))]
fn identity_for_open_file(_file: &File, _kind: ObjectKind) -> anyhow::Result<ObjectIdentity> {
    anyhow::bail!("filesystem object identity is unsupported on this platform")
}

#[cfg(not(any(unix, windows)))]
fn identity_for_path(_path: &Path, _kind: ObjectKind) -> anyhow::Result<ObjectIdentity> {
    anyhow::bail!("filesystem object identity is unsupported on this platform")
}

#[cfg(windows)]
fn publish_file_atomically(
    source: &Path,
    target: &Path,
    mode: FilePublicationMode,
    target_exists: bool,
    new_identity: ObjectIdentity,
    replaced_identity: Option<ObjectIdentity>,
) -> std::io::Result<AtomicPublicationOutcome> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Observed {
        Absent,
        DirectFile(ObjectIdentity),
        Other,
    }

    fn observe(path: &Path) -> std::io::Result<Observed> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Observed::Absent),
            Err(error) => Err(error),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                Ok(Observed::Other)
            }
            Ok(_) => identity_for_path(path, ObjectKind::File)
                .map(Observed::DirectFile)
                .map_err(|error| std::io::Error::other(format!("{error:#}"))),
        }
    }

    if source.parent() != target.parent() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows publication requires source and target siblings",
        ));
    }
    if mode == FilePublicationMode::ReplaceExisting && target_exists != replaced_identity.is_some()
    {
        return Err(std::io::Error::other(
            "replacement identity does not match admitted target existence",
        ));
    }
    let source_wide = windows_extended_path_wide(source)?;
    let target_wide = windows_extended_path_wide(target)?;
    let flags = MOVEFILE_WRITE_THROUGH
        | if mode == FilePublicationMode::ReplaceExisting {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: both buffers are live and NUL-terminated for the call.
    let succeeded = unsafe { MoveFileExW(source_wide.as_ptr(), target_wide.as_ptr(), flags) };
    let operation_error = (succeeded == 0).then(std::io::Error::last_os_error);
    let source_state = match observe(source) {
        Ok(value) => value,
        Err(error) => {
            return Ok(AtomicPublicationOutcome::NamespaceIndeterminate {
                retained_new_path: None,
                source: std::io::Error::other(format!(
                    "source postcondition probe failed after {operation_error:?}: {error}"
                )),
            });
        }
    };
    let retained_new_path =
        (source_state == Observed::DirectFile(new_identity)).then(|| source.to_path_buf());
    let target_state = match observe(target) {
        Ok(value) => value,
        Err(error) => {
            return Ok(AtomicPublicationOutcome::NamespaceIndeterminate {
                retained_new_path,
                source: std::io::Error::other(format!(
                    "target postcondition probe failed after {operation_error:?}: {error}"
                )),
            });
        }
    };

    if target_state == Observed::DirectFile(new_identity) {
        if source_state != Observed::Absent {
            return Ok(AtomicPublicationOutcome::NamespaceIndeterminate {
                retained_new_path,
                source: std::io::Error::other(
                    "new object is visible at target but source namespace remains",
                ),
            });
        }
        return Ok(match operation_error {
            Some(source) => AtomicPublicationOutcome::PublishedDurabilityUnconfirmed {
                disposition: SourceDisposition::Moved,
                source,
            },
            None => AtomicPublicationOutcome::Published(SourceDisposition::Moved),
        });
    }
    if operation_error.is_none() {
        return Ok(AtomicPublicationOutcome::NamespaceIndeterminate {
            retained_new_path,
            source: std::io::Error::other(
                "MoveFileExW reported success but target identity is not the new object",
            ),
        });
    }
    let proven_unchanged = source_state == Observed::DirectFile(new_identity)
        && match mode {
            FilePublicationMode::CreateNew => true,
            FilePublicationMode::ReplaceExisting => match replaced_identity {
                Some(identity) => target_state == Observed::DirectFile(identity),
                None => target_state == Observed::Absent,
            },
        };
    let operation_error = match operation_error {
        Some(error) => error,
        None => {
            return Ok(AtomicPublicationOutcome::NamespaceIndeterminate {
                retained_new_path,
                source: std::io::Error::other("unclassified Windows publication state"),
            });
        }
    };
    if proven_unchanged {
        Err(operation_error)
    } else {
        Ok(AtomicPublicationOutcome::NamespaceIndeterminate {
            retained_new_path,
            source: std::io::Error::other(format!(
                "MoveFileExW reported {operation_error}, but identities do not prove an unchanged namespace"
            )),
        })
    }
}

#[cfg(unix)]
fn publish_file_atomically(
    source: &Path,
    target: &Path,
    mode: FilePublicationMode,
    _target_exists: bool,
    _new_identity: ObjectIdentity,
    _replaced_identity: Option<ObjectIdentity>,
) -> std::io::Result<AtomicPublicationOutcome> {
    match mode {
        FilePublicationMode::ReplaceExisting => {
            fs::rename(source, target)?;
            Ok(AtomicPublicationOutcome::Published(
                SourceDisposition::Moved,
            ))
        }
        FilePublicationMode::CreateNew => {
            fs::hard_link(source, target)?;
            Ok(AtomicPublicationOutcome::Published(
                SourceDisposition::LinkedNeedsCleanup,
            ))
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn publish_file_atomically(
    _source: &Path,
    _target: &Path,
    _mode: FilePublicationMode,
    _target_exists: bool,
    _new_identity: ObjectIdentity,
    _replaced_identity: Option<ObjectIdentity>,
) -> std::io::Result<AtomicPublicationOutcome> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic file publication is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn sync_parent_directory(target: &Path) -> std::io::Result<()> {
    sync_directory(publication_parent(target))
}

#[cfg(not(unix))]
fn sync_parent_directory(_target: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    let directory = File::open(directory)?;
    loop {
        match directory.sync_all() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(windows)]
fn windows_extended_path_wide(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows publication path contains an interior NUL",
        ));
    }
    const EXTENDED_PREFIX: [u16; 4] = [92, 92, 63, 92];
    const UNC_PREFIX: [u16; 2] = [92, 92];
    const EXTENDED_UNC_PREFIX: [u16; 8] = [92, 92, 63, 92, 85, 78, 67, 92];
    let mut value = if units.starts_with(&EXTENDED_PREFIX) {
        units
    } else if units.starts_with(&UNC_PREFIX) {
        EXTENDED_UNC_PREFIX.into_iter().chain(units.into_iter().skip(2)).collect()
    } else {
        EXTENDED_PREFIX.into_iter().chain(units).collect()
    };
    value.push(0);
    Ok(value)
}

#[cfg(windows)]
fn create_durable_direct_child_windows(
    _parent: &Path,
    child: &Path,
) -> Result<DirectoryPublicationEvidence, DirectoryPublicationFailure> {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Observed {
        Absent,
        Directory(ObjectIdentity),
        Other,
    }

    fn observe(path: &Path) -> std::io::Result<Observed> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Observed::Absent),
            Err(error) => Err(error),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Ok(Observed::Other)
            }
            Ok(_) => identity_for_path(path, ObjectKind::Directory)
                .map(Observed::Directory)
                .map_err(|error| std::io::Error::other(format!("{error:#}"))),
        }
    }

    let staging = temporary_sibling_path(child, "directory");
    fs::create_dir(&staging)?;
    let identity =
        identity_for_path(&staging, ObjectKind::Directory).map_err(|error| {
            DirectoryPublicationFailure::NamespaceIndeterminate {
                intended_path: child.to_path_buf(),
                retained_staging_path: None,
                source: std::io::Error::other(format!(
                    "staging directory identity could not be observed after create_dir succeeded: {error:#}"
                )),
            }
        })?;
    let source = windows_extended_path_wide(&staging)?;
    let target = windows_extended_path_wide(child)?;
    // SAFETY: both buffers are live and NUL-terminated for the call.
    let succeeded =
        unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    let operation_error = (succeeded == 0).then(std::io::Error::last_os_error);
    let source_state = observe(&staging);
    let target_state = observe(child);
    let retained_staging_path =
        matches!(source_state, Ok(Observed::Directory(value)) if value == identity)
            .then_some(staging.clone());
    if matches!(target_state, Ok(Observed::Directory(value)) if value == identity)
        && matches!(source_state, Ok(Observed::Absent))
    {
        return match operation_error {
            None => Ok(DirectoryPublicationEvidence { path: child.to_path_buf() }),
            Some(source) => Err(DirectoryPublicationFailure::DurabilityUnconfirmed {
                path: child.to_path_buf(),
                source,
            }),
        };
    }
    let target_proves_not_new = match &target_state {
        Ok(Observed::Absent | Observed::Other) => true,
        Ok(Observed::Directory(value)) => *value != identity,
        Err(_) => false,
    };
    if operation_error.is_some()
        && matches!(source_state, Ok(Observed::Directory(value)) if value == identity)
        && target_proves_not_new
    {
        let error = operation_error.unwrap_or_else(|| {
            std::io::Error::other("directory publication failed before namespace insertion")
        });
        if identity_for_path(&staging, ObjectKind::Directory).ok() == Some(identity) {
            let _ = fs::remove_dir(&staging);
        }
        return Err(DirectoryPublicationFailure::BeforeNamespace(error.into()));
    }
    Err(DirectoryPublicationFailure::NamespaceIndeterminate {
        intended_path: child.to_path_buf(),
        retained_staging_path,
        source: std::io::Error::other(format!(
            "directory MoveFileExW postcondition mismatch after {operation_error:?}; source={source_state:?}, target={target_state:?}"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_root(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "mondrian-storage-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create test root");
        root
    }

    #[test]
    fn create_new_collision_is_proven_before_namespace_failure() {
        let root = unique_root("collision");
        let target = root.join("target.bin");
        fs::write(&target, b"old").expect("old target");
        let mut staging = OwnedPublicationFile::create_sibling(&target, "test").expect("staging");
        staging.file_mut().expect("handle").write_all(b"new").expect("new bytes");
        let error = staging.publish(FilePublicationMode::CreateNew).expect_err("collision");
        assert!(matches!(error, FilePublicationFailure::BeforeNamespace(_)));
        assert_eq!(fs::read(&target).expect("target"), b"old");
    }

    #[test]
    fn durable_bytes_create_new_never_overwrites_a_racing_entry() {
        let root = unique_root("durable-bytes-create-new-collision");
        let target = root.join("marker");
        fs::write(&target, b"foreign").expect("racing foreign marker");

        let error = write_durable_file_atomically_with_mode(
            &target,
            b"owned",
            FilePublicationMode::CreateNew,
        )
        .expect_err("create-only publication must preserve collision");

        assert!(matches!(error, FilePublicationFailure::BeforeNamespace(_)));
        assert_eq!(
            fs::read(&target).expect("foreign marker survives"),
            b"foreign"
        );
    }

    #[test]
    fn external_reservation_rejects_and_preserves_an_observed_replacement() {
        let root = unique_root("external-replacement");
        let target = root.join("target.bin");
        let staging =
            OwnedPublicationFile::create_sibling(&target, "external-test").expect("staging");
        let reservation = staging.release_for_external_writer();
        let staging_path = reservation.path().to_path_buf();
        fs::remove_file(&staging_path).expect("remove reserved object");
        fs::write(&staging_path, b"foreign").expect("install replacement");

        let error = match reservation.reclaim() {
            Ok(_) => panic!("replacement identity must not be reclaimed"),
            Err(error) => error,
        };

        assert!(
            format!("{error:#}").contains("no longer names its reserved file object"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&staging_path).expect("replacement survives failed reclaim"),
            b"foreign"
        );
    }

    #[test]
    fn injected_parent_sync_failure_is_not_reported_as_unpublished() {
        let root = unique_root("sync-failure");
        let target = root.join("target.bin");
        let mut staging = OwnedPublicationFile::create_sibling(&target, "test").expect("staging");
        staging.file_mut().expect("handle").write_all(b"new").expect("new bytes");
        let error = staging
            .publish_with_parent_sync(FilePublicationMode::ReplaceExisting, |_| {
                Err(std::io::Error::other("injected parent sync failure"))
            })
            .expect_err("durability must be unconfirmed");
        assert!(matches!(
            error,
            FilePublicationFailure::DurabilityUnconfirmed(_)
        ));
        assert_eq!(fs::read(&target).expect("visible target"), b"new");
    }

    #[test]
    fn replace_existing_publishes_exact_new_object() {
        let root = unique_root("replace");
        let target = root.join("target.bin");
        fs::write(&target, b"old").expect("old target");
        let mut staging = OwnedPublicationFile::create_sibling(&target, "test").expect("staging");
        staging.file_mut().expect("handle").write_all(b"new").expect("new bytes");
        let evidence = staging.publish(FilePublicationMode::ReplaceExisting).expect("replace");
        assert_eq!(evidence.published_path(), target);
        assert_eq!(fs::read(&target).expect("target"), b"new");
    }

    #[test]
    fn direct_child_creation_rejects_existing_non_directory_namespace() {
        let root = unique_root("directory-collision");
        let child = root.join("child");
        fs::write(&child, b"foreign").expect("existing file");
        let error = create_durable_direct_child(&root, &child).expect_err("collision");
        assert!(matches!(
            error,
            DirectoryPublicationFailure::BeforeNamespace(_)
        ));
        assert_eq!(
            fs::read(&child).expect("foreign child survives"),
            b"foreign"
        );
    }

    #[test]
    fn existing_direct_child_is_not_upgraded_to_durable_evidence() {
        let root = unique_root("directory-reentry-unconfirmed");
        let child = root.join("child");
        fs::create_dir(&child).expect("existing child");

        let error =
            create_durable_direct_child(&root, &child).expect_err("presence is not evidence");

        assert!(matches!(
            error,
            DirectoryPublicationFailure::DurabilityUnconfirmed { .. }
        ));
    }

    #[test]
    fn durable_directory_chain_creates_every_missing_private_suffix() {
        let root = unique_root("directory-chain-missing");
        let target = root.join("one").join("two").join("three");

        let evidence =
            ensure_durable_directory_chain(&root, &target).expect("publish missing chain");

        assert_eq!(evidence.path(), target);
        assert!(target.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [root.join("one"), root.join("one").join("two"), target] {
                let mode =
                    fs::symlink_metadata(path).expect("suffix metadata").permissions().mode();
                assert_eq!(mode & 0o777, 0o700);
            }
        }
    }

    #[test]
    fn durable_directory_chain_preserves_collision_without_descending() {
        let root = unique_root("directory-chain-collision");
        let collision = root.join("one");
        let target = collision.join("two");
        fs::write(&collision, b"foreign").expect("foreign collision");

        let error =
            ensure_durable_directory_chain(&root, &target).expect_err("collision fails closed");

        assert!(matches!(
            error,
            DirectoryPublicationFailure::BeforeNamespace(_)
        ));
        assert_eq!(
            fs::read(&collision).expect("foreign collision survives"),
            b"foreign"
        );
        assert!(!target.exists());
    }

    #[test]
    fn durable_directory_chain_reentry_remains_unconfirmed() {
        let root = unique_root("directory-chain-reentry");
        let existing = root.join("one");
        let target = existing.join("two");
        fs::create_dir(&existing).expect("existing suffix");

        let error = ensure_durable_directory_chain(&root, &target)
            .expect_err("path presence cannot reestablish construction evidence");

        assert!(matches!(
            error,
            DirectoryPublicationFailure::DurabilityUnconfirmed { ref path, .. }
                if path == &existing
        ));
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_post_create_parent_sync_failure_is_durability_unconfirmed() {
        let root = unique_root("directory-create-sync-failure");
        let child = root.join("child");

        let error = create_durable_direct_child_unix_with(
            &root,
            &child,
            |path| identity_for_path(path, ObjectKind::Directory),
            |_, _| Err(std::io::Error::other("injected parent sync failure")),
        )
        .expect_err("successful create with failed parent sync is not durable evidence");

        assert!(matches!(
            error,
            DirectoryPublicationFailure::DurabilityUnconfirmed { .. }
        ));
        assert!(child.is_dir(), "visible created namespace remains");
    }

    #[cfg(unix)]
    #[test]
    fn unix_post_create_identity_probe_failure_is_indeterminate() {
        let root = unique_root("directory-create-probe-failure");
        let child = root.join("child");

        let error = create_durable_direct_child_unix_with(
            &root,
            &child,
            |_| anyhow::bail!("injected post-create identity probe failure"),
            |_, _| panic!("barrier must not run without a proven identity"),
        )
        .expect_err("successful create_dir crossed the namespace boundary");

        assert!(matches!(
            error,
            DirectoryPublicationFailure::NamespaceIndeterminate { .. }
        ));
        assert!(
            child.is_dir(),
            "created namespace is conservatively retained"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_encoding_preserves_extended_and_unc_namespaces() {
        use std::os::windows::ffi::OsStringExt;

        let render = |path: &Path| {
            let mut wide = windows_extended_path_wide(path).expect("encode path");
            assert_eq!(wide.pop(), Some(0));
            std::ffi::OsString::from_wide(&wide).to_string_lossy().into_owned()
        };

        assert_eq!(
            render(Path::new(r"C:\workspace\project")),
            r"\\?\C:\workspace\project"
        );
        assert_eq!(
            render(Path::new(r"\\server\share\project")),
            r"\\?\UNC\server\share\project"
        );
        assert_eq!(
            render(Path::new(r"\\?\C:\workspace\project")),
            r"\\?\C:\workspace\project"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_publication_supports_extended_length_paths() {
        use std::os::windows::ffi::OsStrExt;

        let root = unique_root("directory-long-path");
        let mut parent = root.clone();
        while parent.as_os_str().encode_wide().count() < 280 {
            parent = parent.join("segment-0123456789abcdef");
            fs::create_dir(&parent).expect("create long-path parent segment");
        }
        let child = parent.join("child");

        let evidence =
            create_durable_direct_child(&parent, &child).expect("publish long-path child");

        assert_eq!(evidence.path(), child);
        assert!(child.is_dir());
    }
}
