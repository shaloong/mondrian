//! Portable Project dependency inventory, byte-complete export, and verification.
//!
//! Inventory is read-only preflight. Export revalidates each source while
//! copying and publishes a complete directory atomically. Opening a moved
//! package additionally requires rebinding its exact source references.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};

use anyhow::Context;
use mondrian_assets::AssetLibrary;
use mondrian_core::automation::{
    ParameterResourceReference, PropertyBag, PropertyHost, PropertyMutation, PropertyValue,
};
use mondrian_core::grade_graph::GradeGraphNodeKind;
use mondrian_core::{AssetId, AssetSource, ColorEngine, OcioConfigSource};
use mondrian_editor_state::AuthoringSnapshot;
use mondrian_project::ProjectDocument;
use mondrian_project::{
    save_project_archive_from_open_library_with_publication, PreparedProjectArchive,
    ProjectArchivePublication, ProjectArchiveReadBudget,
};
use mondrian_storage::OwnedPublicationDirectory;
use mondrian_timeline::clip::Clip;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::AppState;

pub(super) struct PortablePackageExportTask {
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    phase: Arc<AtomicU8>,
    last_phase: u8,
    last_percent: u8,
    result: mpsc::Receiver<anyhow::Result<()>>,
    target: PathBuf,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug, thiserror::Error)]
#[error("portable package export canceled")]
struct PortableExportCanceled;

fn ensure_export_active(cancel: &AtomicBool) -> anyhow::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(PortableExportCanceled.into());
    }
    Ok(())
}

impl Drop for PortablePackageExportTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// One canonical regular file and all author references requiring its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDependencyFile {
    /// Canonical source path at inventory time; copy must revalidate it.
    pub path: PathBuf,
    /// File length at inventory time, useful for a package size preview.
    pub size_bytes: u64,
    /// Stable author identities referring to this file.
    pub owners: Vec<String>,
    /// Exact persisted spellings that refer to this canonical source file.
    pub source_paths: Vec<PathBuf>,
}

/// An immutable package file and its exact source-reference bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortablePackageFile {
    /// Safe path relative to the package root.
    pub bundled_path: PathBuf,
    /// Paths to replace when opening the package on another machine.
    pub source_paths: Vec<PathBuf>,
    /// Complete SHA-256 digest of the copied file.
    pub sha256: String,
    /// Number of copied bytes.
    pub size_bytes: u64,
}

/// Versioned manifest for a complete portable Project directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortablePackageManifest {
    /// Package contract version.
    pub version: u32,
    /// Project identity in the nested `.mdp` archive.
    pub project_id: mondrian_core::ProjectId,
    /// SHA-256 digest of `project.mdp`.
    pub project_sha256: String,
    /// Copied media and resource files.
    pub files: Vec<PortablePackageFile>,
}

/// One dependency that cannot currently be copied into a complete package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDependencyIssue {
    /// Stable author identity requiring the dependency.
    pub owner: String,
    /// Actionable reason for rejecting a complete package.
    pub reason: String,
}

/// Read-only dependency preflight for one Project and Library snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDependencyInventory {
    /// Deduplicated regular files admitted at inventory time.
    pub files: Vec<PortableDependencyFile>,
    /// Missing, remote, or not yet portable dependency contracts.
    pub issues: Vec<PortableDependencyIssue>,
}

impl PortableDependencyInventory {
    /// Whether every discovered dependency has a local copy candidate.
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty()
    }

    /// Total currently observed source bytes, with overflow treated as unknown.
    pub fn observed_size_bytes(&self) -> Option<u64> {
        self.files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.size_bytes))
    }
}

impl AppState {
    /// Inventory all currently authored Project package dependencies.
    pub fn portable_project_dependency_inventory(
        &self,
    ) -> anyhow::Result<PortableDependencyInventory> {
        let session = self.authoring.as_ref().context("no open Project")?;
        collect_project_dependencies(session.document(), session.asset_library())
    }

    /// Export one self-contained `.mdpkg` directory from an author snapshot.
    ///
    /// An incomplete dependency graph or changed source rejects publication.
    /// The destination must be absent; this never overwrites an existing package.
    pub fn export_portable_project_package(&self, target: &Path) -> anyhow::Result<()> {
        let session = self.authoring.as_ref().context("no open Project")?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        export_snapshot_package(snapshot, target)
    }

    /// Start a cancellable portable export without blocking the editor.
    pub fn request_portable_project_export(&mut self, target: PathBuf) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.portable_package_export.is_none(),
            "a portable package export is already running"
        );
        let session = self.authoring.as_ref().context("no open Project")?;
        let snapshot = session.snapshot().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let target = target.with_extension("mdpkg");
        match fs::symlink_metadata(&target) {
            Ok(_) => anyhow::bail!(
                "portable package target already exists: {}",
                target.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect portable package destination"),
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let phase = Arc::new(AtomicU8::new(0));
        let (sender, result) = mpsc::sync_channel(1);
        let worker_target = target.clone();
        let worker_cancel = Arc::clone(&cancel);
        let worker_progress = Arc::clone(&progress);
        let worker_total = Arc::clone(&total);
        let worker_phase = Arc::clone(&phase);
        let worker = std::thread::Builder::new().name("portable-project-export".to_owned()).spawn(
            move || {
                let outcome = export_snapshot_package_with_progress(
                    snapshot,
                    &worker_target,
                    &worker_cancel,
                    &worker_progress,
                    &worker_total,
                    &worker_phase,
                );
                let _ = sender.send(outcome);
            },
        )?;
        self.portable_package_export = Some(PortablePackageExportTask {
            cancel,
            progress,
            total,
            phase,
            last_phase: 0,
            last_percent: 0,
            result,
            target,
            worker: Some(worker),
        });
        self.set_status_hint("正在打包项目…", false);
        Ok(())
    }

    /// Request cancellation; the worker owns staging cleanup.
    pub fn cancel_portable_project_export(&mut self) -> bool {
        let Some(task) = &self.portable_package_export else {
            return false;
        };
        task.cancel.store(true, Ordering::Release);
        self.set_status_hint("正在取消项目打包…", false);
        true
    }

    /// Whether one portable package export is still awaiting a terminal result.
    pub fn portable_project_export_active(&self) -> bool {
        self.portable_package_export.is_some()
    }

    /// Consume progress and terminal evidence from the portable export worker.
    pub fn poll_portable_project_export(&mut self) -> bool {
        let Some(task) = self.portable_package_export.as_mut() else {
            return false;
        };
        match task.result.try_recv() {
            Ok(result) => {
                let target = task.target.clone();
                self.portable_package_export = None;
                match result {
                    Ok(()) => {
                        self.set_status_hint(format!("项目打包完成：{}", target.display()), false);
                        self.notifications.publish(
                            format!("portable-package:{}", target.display()),
                            super::notifications::AppNotificationSeverity::Success,
                            super::notifications::AppNotificationMessage::new(
                                "notification-package-complete",
                            )
                            .with_text("path", target.display().to_string()),
                        );
                    }
                    Err(error) if error.is::<PortableExportCanceled>() => {
                        self.set_status_hint("已取消项目打包", false)
                    }
                    Err(error) => {
                        let reason = format!("{error:#}");
                        self.set_status_hint(format!("项目打包未完成：{reason}"), true);
                        self.notifications.publish(
                            format!("portable-package:{}", target.display()),
                            super::notifications::AppNotificationSeverity::Error,
                            super::notifications::AppNotificationMessage::new(
                                "notification-package-failed",
                            )
                            .with_text("reason", reason),
                        );
                    }
                }
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let target = task.target.clone();
                self.portable_package_export = None;
                self.set_status_hint("项目打包工作线程意外终止", true);
                self.notifications.publish(
                    format!("portable-package:{}", target.display()),
                    super::notifications::AppNotificationSeverity::Error,
                    super::notifications::AppNotificationMessage::new(
                        "notification-package-failed",
                    )
                    .with_text("reason", "工作线程意外终止"),
                );
                true
            }
            Err(mpsc::TryRecvError::Empty) => {
                if task.cancel.load(Ordering::Acquire) {
                    return false;
                }
                let total = task.total.load(Ordering::Acquire);
                let phase = task.phase.load(Ordering::Acquire);
                let copied = task.progress.load(Ordering::Acquire).min(total);
                let percent = if total > 0 {
                    (u128::from(copied) * 100 / u128::from(total)).min(99) as u8
                } else {
                    0
                };
                if percent == task.last_percent && phase == task.last_phase {
                    return false;
                }
                task.last_percent = percent;
                task.last_phase = phase;
                let status = match phase {
                    1 => format!("正在复制项目资源… {percent}%"),
                    2 => format!("正在校验项目资源… {percent}%"),
                    3 => "正在写入工程快照…".to_owned(),
                    4 => "正在发布项目包…".to_owned(),
                    _ => "正在分析项目依赖…".to_owned(),
                };
                self.set_status_hint(status, false);
                true
            }
        }
    }
}

fn export_snapshot_package(snapshot: AuthoringSnapshot, target: &Path) -> anyhow::Result<()> {
    export_snapshot_package_with_progress(
        snapshot,
        target,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
        &AtomicU64::new(0),
        &AtomicU8::new(0),
    )
}

fn export_snapshot_package_with_progress(
    snapshot: AuthoringSnapshot,
    target: &Path,
    cancel: &AtomicBool,
    progress: &AtomicU64,
    total: &AtomicU64,
    phase: &AtomicU8,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        snapshot.asset_library.database_revision()? == snapshot.asset_library_revision,
        "Project Library changed after portable author snapshot"
    );
    let inventory = collect_project_dependencies(&snapshot.document, &snapshot.asset_library)?;
    if !inventory.is_complete() {
        let detail = inventory
            .issues
            .iter()
            .map(|issue| format!("{}: {}", issue.owner, issue.reason))
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("portable package has unresolved dependencies: {detail}");
    }
    total.store(
        inventory.observed_size_bytes().unwrap_or(0),
        Ordering::Release,
    );
    let staging = OwnedPublicationDirectory::create_sibling(target, "portable-project")?;
    let files_root = staging.path().join("files");
    fs::create_dir(&files_root).context("create portable package file directory")?;
    let mut package_files = Vec::with_capacity(inventory.files.len());
    for (index, dependency) in inventory.files.iter().enumerate() {
        phase.store(1, Ordering::Release);
        let extension = dependency
            .path
            .extension()
            .and_then(|part| part.to_str())
            .filter(|part| {
                !part.is_empty()
                    && part.len() <= 12
                    && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .unwrap_or("bin");
        let name = format!("{index:08}.{extension}");
        let bundled_path = PathBuf::from("files").join(name);
        let mut source = fs::File::open(&dependency.path)
            .with_context(|| format!("open package source {}", dependency.path.display()))?;
        let mut destination = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(staging.path().join(&bundled_path))?;
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            ensure_export_active(cancel)?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            destination.write_all(&buffer[..count])?;
            copied = copied.checked_add(count as u64).context("package copy exceeds u64 length")?;
            let _ = progress.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_add(count as u64))
            });
        }
        anyhow::ensure!(
            copied == dependency.size_bytes,
            "source length changed during package copy: {}",
            dependency.path.display()
        );
        destination.sync_all()?;
        destination.seek(SeekFrom::Start(0))?;
        source.seek(SeekFrom::Start(0))?;
        phase.store(2, Ordering::Release);
        let (copied_hash, copied_len) = hash_reader_with_cancel(&mut destination, cancel)?;
        let (source_hash, source_len) = hash_reader_with_cancel(&mut source, cancel)?;
        anyhow::ensure!(
            copied_hash == source_hash && copied_len == source_len,
            "source content changed during package copy: {}",
            dependency.path.display()
        );
        package_files.push(PortablePackageFile {
            bundled_path,
            source_paths: dependency.source_paths.clone(),
            sha256: copied_hash,
            size_bytes: copied_len,
        });
    }
    phase.store(3, Ordering::Release);
    let archive_path = staging.path().join("project.mdp");
    ensure_export_active(cancel)?;
    anyhow::ensure!(
        snapshot.asset_library.database_revision()? == snapshot.asset_library_revision,
        "Project Library changed during portable package copy"
    );
    let database_snapshot = snapshot
        .asset_library
        .snapshot_database(snapshot.asset_library_revision, &archive_path)?;
    anyhow::ensure!(
        snapshot.asset_library.database_revision()? == snapshot.asset_library_revision,
        "Project Library changed during portable package snapshot"
    );
    let mut database_reader = database_snapshot.try_clone_reader()?;
    save_project_archive_from_open_library_with_publication(
        &snapshot.document,
        &mut database_reader,
        &archive_path,
        ProjectArchivePublication::CreateNew,
    )?;
    drop(database_reader);
    drop(database_snapshot);
    let (project_sha256, _) = hash_reader_with_cancel(&mut fs::File::open(&archive_path)?, cancel)?;
    let manifest = PortablePackageManifest {
        version: 1,
        project_id: snapshot.document.project_id,
        project_sha256,
        files: package_files,
    };
    fs::write(
        staging.path().join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    ensure_export_active(cancel)?;
    phase.store(4, Ordering::Release);
    staging.publish_create_new()?;
    Ok(())
}

fn hash_reader(reader: &mut impl Read) -> anyhow::Result<(String, u64)> {
    hash_reader_with_cancel(reader, &AtomicBool::new(false))
}

fn hash_reader_with_cancel(
    reader: &mut impl Read,
    cancel: &AtomicBool,
) -> anyhow::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        ensure_export_active(cancel)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total = total.checked_add(count as u64).context("package file exceeds u64 length")?;
    }
    let digest = hasher.finalize();
    Ok((
        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        total,
    ))
}

/// Validate a portable directory and every bundled byte before its Project is opened.
pub fn verify_portable_project_package(root: &Path) -> anyhow::Result<PortablePackageManifest> {
    let root = fs::canonicalize(root).context("portable package directory is unavailable")?;
    anyhow::ensure!(root.is_dir(), "portable package root is not a directory");
    let manifest_path = root.join("manifest.json");
    reject_link_or_non_file(&manifest_path)?;
    let manifest_bytes = fs::read(&manifest_path)?;
    anyhow::ensure!(
        manifest_bytes.len() <= 16 * 1024 * 1024,
        "portable package manifest is too large"
    );
    let manifest: PortablePackageManifest = serde_json::from_slice(&manifest_bytes)?;
    anyhow::ensure!(
        manifest.version == 1,
        "unsupported portable package version {}",
        manifest.version
    );
    anyhow::ensure!(
        fs::symlink_metadata(root.join("files"))?.file_type().is_dir(),
        "portable package files entry is not a real directory"
    );
    let mut source_paths = BTreeSet::new();
    let mut bundled_paths = BTreeSet::new();
    for file in &manifest.files {
        let relative = &file.bundled_path;
        let mut parts = relative.components();
        anyhow::ensure!(
            matches!(parts.next(), Some(std::path::Component::Normal(part)) if part == "files")
                && matches!(parts.next(), Some(std::path::Component::Normal(_)))
                && parts.next().is_none(),
            "unsafe portable package file path: {}",
            relative.display()
        );
        anyhow::ensure!(
            bundled_paths.insert(relative.clone()),
            "duplicate bundled file path"
        );
        anyhow::ensure!(
            !file.source_paths.is_empty(),
            "bundled file has no source references"
        );
        for source in &file.source_paths {
            anyhow::ensure!(
                source.is_absolute() && source_paths.insert(source.clone()),
                "invalid or duplicate source reference: {}",
                source.display()
            );
        }
        let bundled = root.join(relative);
        reject_link_or_non_file(&bundled)?;
        let (sha256, length) = hash_reader(&mut fs::File::open(&bundled)?)?;
        anyhow::ensure!(
            sha256 == file.sha256 && length == file.size_bytes,
            "portable package file checksum mismatch: {}",
            relative.display()
        );
    }
    let archive_path = root.join("project.mdp");
    reject_link_or_non_file(&archive_path)?;
    let mut archive = fs::File::open(&archive_path)?;
    let (sha256, _) = hash_reader(&mut archive)?;
    anyhow::ensure!(
        sha256 == manifest.project_sha256,
        "portable package Project checksum mismatch"
    );
    archive.seek(SeekFrom::Start(0))?;
    let prepared =
        PreparedProjectArchive::from_open_file(&mut archive, ProjectArchiveReadBudget::default())?;
    anyhow::ensure!(
        prepared.project_id() == manifest.project_id,
        "portable package Project identity mismatch"
    );
    Ok(manifest)
}

fn reject_link_or_non_file(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect portable package entry {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "portable package entry is not a regular file: {}",
        path.display()
    );
    Ok(())
}

pub(super) fn package_source_bindings(
    root: &Path,
    manifest: &PortablePackageManifest,
) -> anyhow::Result<BTreeMap<PathBuf, PathBuf>> {
    let root = fs::canonicalize(root)?;
    let mut bindings = BTreeMap::new();
    for file in &manifest.files {
        let bundled = mondrian_assets::canonical_native_path(&root.join(&file.bundled_path))?;
        for source in &file.source_paths {
            anyhow::ensure!(
                bindings.insert(source.clone(), bundled.clone()).is_none(),
                "duplicate package source binding"
            );
        }
    }
    Ok(bindings)
}

pub(super) fn ensure_package_asset_sources_rebound(
    library: &AssetLibrary,
    bindings: &BTreeMap<PathBuf, PathBuf>,
) -> anyhow::Result<()> {
    let bundled_sources = bindings.values().collect::<BTreeSet<_>>();
    for asset in library.list_assets()? {
        match &asset.source {
            AssetSource::File(path) => anyhow::ensure!(
                bundled_sources.contains(path),
                "packaged Asset {} retains an unbound file source: {}",
                asset.id,
                path.display()
            ),
            AssetSource::Remote(uri) => {
                anyhow::bail!("packaged Asset {} retains a remote source: {uri}", asset.id)
            }
            AssetSource::Generated(_) => {}
        }
    }
    Ok(())
}

pub(super) fn rebind_document_resources(
    document: &mut ProjectDocument,
    bindings: &BTreeMap<PathBuf, PathBuf>,
) -> anyhow::Result<()> {
    for sequence in &mut document.sequences.sequences {
        for track in sequence.video_tracks.iter_mut().chain(sequence.audio_tracks.iter_mut()) {
            for clip in &mut track.clips {
                for effect in &mut clip.effects {
                    rebind_property_bag(&mut effect.properties, bindings)?;
                }
                for mask in &mut clip.masks {
                    rebind_property_bag(&mut mask.properties, bindings)?;
                }
                let properties =
                    clip.property_bag().map_err(|error| anyhow::anyhow!(error.to_string()))?;
                ensure_no_old_references(&properties, bindings)?;
            }
        }
        for definition in &mut sequence.grade_definitions {
            for version in &mut definition.versions {
                for node in &mut version.graph.nodes {
                    if let GradeGraphNodeKind::Effect { effect, .. } = &mut node.kind {
                        rebind_property_bag(&mut effect.properties, bindings)?;
                    }
                }
            }
        }
    }
    document.validate()?;
    Ok(())
}

fn rebind_property_bag(
    bag: &mut PropertyBag,
    bindings: &BTreeMap<PathBuf, PathBuf>,
) -> anyhow::Result<()> {
    let mut mutations = Vec::new();
    for (path, property) in bag.iter() {
        if let Some(value) = relocated_resource_value(property.static_value(), bindings)? {
            mutations.push(PropertyMutation::SetStaticValue { path: path.to_owned(), value });
        }
        for time in property.keyframe_times() {
            if let Some(key) = property.keyframe_at(time)
                && let Some(value) = relocated_resource_value(&key.value, bindings)?
            {
                mutations.push(PropertyMutation::EditKeyframe {
                    path: path.to_owned(),
                    keyframe_id: key.id,
                    time,
                    value,
                });
            }
        }
    }
    for mutation in mutations {
        bag.apply_mutation(mutation)?;
    }
    Ok(())
}

fn relocated_resource_value(
    value: &PropertyValue,
    bindings: &BTreeMap<PathBuf, PathBuf>,
) -> anyhow::Result<Option<PropertyValue>> {
    let PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }) = value else {
        return Ok(None);
    };
    let bundled = bindings.get(path).ok_or_else(|| {
        anyhow::anyhow!(
            "Project resource is absent from portable manifest: {}",
            path.display()
        )
    })?;
    Ok(Some(PropertyValue::Resource(
        ParameterResourceReference::ExternalFile { path: bundled.clone() },
    )))
}

fn ensure_no_old_references(
    bag: &PropertyBag,
    bindings: &BTreeMap<PathBuf, PathBuf>,
) -> anyhow::Result<()> {
    for (_, property) in bag.iter() {
        for value in std::iter::once(property.static_value().clone()).chain(
            property
                .keyframe_times()
                .iter()
                .filter_map(|time| property.keyframe_at(*time).map(|key| key.value)),
        ) {
            if let PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }) =
                value
            {
                anyhow::ensure!(
                    !bindings.contains_key(&path),
                    "Project retained an unbound original resource: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn collect_project_dependencies(
    document: &ProjectDocument,
    library: &AssetLibrary,
) -> anyhow::Result<PortableDependencyInventory> {
    let library_revision = library.database_revision()?;
    let mut collector = DependencyCollector::new(library);
    for asset in library.list_assets()? {
        collector.add_asset(asset.id)?;
    }
    for sequence in &document.sequences.sequences {
        for track in sequence.video_tracks.iter().chain(sequence.audio_tracks.iter()) {
            for clip in &track.clips {
                if let Some(asset_id) = clip.library_asset_id() {
                    collector.add_asset(asset_id)?;
                }
                collector.add_clip_properties(clip)?;
            }
        }
        // Every saved grade version remains user-selectable, including a
        // currently inactive one. Collecting only the active graph loses LUTs.
        for definition in &sequence.grade_definitions {
            for version in &definition.versions {
                for node in &version.graph.nodes {
                    if let GradeGraphNodeKind::Effect { effect, .. } = &node.kind {
                        collector.add_property_bag(
                            &effect.properties,
                            &format!(
                                "sequence:{}/grade:{}/version:{}/node:{}",
                                sequence.id, definition.id, version.id, node.id
                            ),
                        )?;
                    }
                }
            }
        }
    }
    if let ColorEngine::CustomOcio { identity } = document.color_environment.engine() {
        match identity.source() {
            OcioConfigSource::Path { path } => {
                collector.add_file("project:custom-ocio-config".to_owned(), path);
                collector.issue(
                    "project:custom-ocio-config",
                    "Custom OCIO may reference additional search-path or environment resources; complete dependency collection is not yet available",
                );
            }
            OcioConfigSource::Environment => collector.issue(
                "project:custom-ocio-config",
                "environment-selected OCIO config is not portable; pin an explicit config and its resources",
            ),
            OcioConfigSource::Builtin { .. } | OcioConfigSource::MondrianStandard { .. } => {}
        }
    }
    if library.database_revision()? != library_revision {
        anyhow::bail!("Project Library changed during portable dependency inventory");
    }
    Ok(collector.finish())
}

struct DependencyCollector<'a> {
    library: &'a AssetLibrary,
    files: BTreeMap<PathBuf, (u64, BTreeSet<String>, BTreeSet<PathBuf>)>,
    seen_assets: BTreeSet<AssetId>,
    issues: Vec<PortableDependencyIssue>,
}

impl<'a> DependencyCollector<'a> {
    fn new(library: &'a AssetLibrary) -> Self {
        Self {
            library,
            files: BTreeMap::new(),
            seen_assets: BTreeSet::new(),
            issues: Vec::new(),
        }
    }

    fn add_asset(&mut self, asset_id: AssetId) -> anyhow::Result<()> {
        if !self.seen_assets.insert(asset_id) {
            return Ok(());
        }
        let owner = format!("asset:{asset_id}");
        let Some(asset) = self.library.get_asset(asset_id)? else {
            self.issue(
                &owner,
                "referenced Asset is missing from the Project Library",
            );
            return Ok(());
        };
        match &asset.source {
            AssetSource::File(path) => self.add_file(owner, path),
            AssetSource::Generated(_) => {}
            AssetSource::Remote(uri) => self.issue(
                &owner,
                format!("remote Asset source is not portable: {uri}"),
            ),
        }
        Ok(())
    }

    fn add_clip_properties(&mut self, clip: &Clip) -> anyhow::Result<()> {
        let bag = clip.property_bag().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        self.add_property_bag(&bag, &format!("clip:{}", clip.id))
    }

    fn add_property_bag(&mut self, bag: &PropertyBag, owner: &str) -> anyhow::Result<()> {
        for (_, property) in bag.iter() {
            let parameter_owner =
                format!("{owner}/parameter:{}", property.descriptor.parameter_id());
            self.add_value(property.static_value(), &parameter_owner)?;
            for time in property.keyframe_times() {
                if let Some(key) = property.keyframe_at(time) {
                    self.add_value(&key.value, &parameter_owner)?;
                }
            }
        }
        Ok(())
    }

    fn add_value(&mut self, value: &PropertyValue, owner: &str) -> anyhow::Result<()> {
        match value {
            PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }) => {
                self.add_file(owner.to_owned(), path);
            }
            PropertyValue::Resource(ParameterResourceReference::ProjectAsset { asset_id }) => {
                self.add_asset(*asset_id)?;
            }
            PropertyValue::Resource(ParameterResourceReference::Uri { uri }) => {
                self.issue(
                    owner,
                    format!("external resource URI is not portable: {uri}"),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn add_file(&mut self, owner: String, path: &Path) {
        if !path.is_absolute() {
            self.issue(
                &owner,
                format!("file path is not absolute: {}", path.display()),
            );
            return;
        }
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) => {
                self.issue(
                    &owner,
                    format!("file is unavailable: {} ({error})", path.display()),
                );
                return;
            }
        };
        let metadata = match fs::metadata(&canonical) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                self.issue(
                    &owner,
                    format!("dependency is not a regular file: {}", path.display()),
                );
                return;
            }
            Err(error) => {
                self.issue(
                    &owner,
                    format!("file cannot be inspected: {} ({error})", path.display()),
                );
                return;
            }
        };
        let entry = self
            .files
            .entry(canonical)
            .or_insert_with(|| (metadata.len(), BTreeSet::new(), BTreeSet::new()));
        entry.1.insert(owner);
        entry.2.insert(path.to_path_buf());
    }

    fn issue(&mut self, owner: impl Into<String>, reason: impl Into<String>) {
        self.issues
            .push(PortableDependencyIssue { owner: owner.into(), reason: reason.into() });
    }

    fn finish(self) -> PortableDependencyInventory {
        PortableDependencyInventory {
            files: self
                .files
                .into_iter()
                .map(
                    |(path, (size_bytes, owners, source_paths))| PortableDependencyFile {
                        path,
                        size_bytes,
                        owners: owners.into_iter().collect(),
                        source_paths: source_paths.into_iter().collect(),
                    },
                )
                .collect(),
            issues: self.issues,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetMediaProbeCandidate;
    use mondrian_core::automation::PropertyMutation;
    use mondrian_core::{
        AudioCodec, AudioSourceComponentId, AudioStreamInfo, ChannelLayout, Color, ColorSpace,
        GradeDefinition, GradeGraph, GradeGraphNode, GradeGraphNodeId, GradeVersion,
        MediaFileFingerprint, MediaInfo, ProjectColorEnvironment, ProjectSettings, TimelineTime,
    };
    use mondrian_editor_state::{Action, AuthoringSession};
    use mondrian_effects::{
        build_effect_render_graph, compile_reference_render_graph, EffectGraphNodeKind,
        EffectNodeExt, EffectRenderOp, EffectType,
    };
    use mondrian_export::preset::TimelineExportRange;
    use mondrian_media::{
        clear_thread_local_preview_decode_session, decode_preview_frame_cancellable,
        DecodedVideoRangeContract, PreviewDecodeAccessMode, PreviewDecodeOutcome,
        PreviewDecodeRequest, PreviewSourceColorContract,
    };
    use mondrian_renderer::{
        color::{ProgramOutputModule, ProgramOutputRole},
        composite_timeline_elements_color_frame, TimelineCompositeElement,
        TimelineCompositeOptions, TimelineCompositeScratch, TimelineEffectColorRuntime,
        TimelineSolidColorLayer,
    };
    use mondrian_timeline::{Clip, Sequence, SequenceCollection};
    use std::time::Duration;

    const RED_INVERT_CUBE_2: &str = "LUT_3D_SIZE 2\n\
1 0 0\n\
0 0 0\n\
1 1 0\n\
0 1 0\n\
1 0 1\n\
0 0 1\n\
1 1 1\n\
0 1 1\n";

    fn lut_effect(path: PathBuf) -> mondrian_effects::EffectNode {
        let mut effect: mondrian_effects::EffectNode =
            EffectNodeExt::with_defaults(EffectType::Lut3D);
        let resource_path = effect
            .properties
            .iter()
            .find_map(|(path, property)| {
                matches!(property.static_value(), PropertyValue::Resource(_))
                    .then(|| path.to_owned())
            })
            .expect("LUT resource parameter");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: resource_path,
                value: PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }),
            })
            .expect("bind LUT");
        let processing_space_id =
            EffectType::Lut3D.parameter_id("processing_space").expect("processing space ID");
        effect
            .set_static_value_by_parameter(
                &processing_space_id,
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("bind LUT processing space");
        effect
    }

    fn sample_document_lut(document: &ProjectDocument) -> [f32; 3] {
        let sequence = &document.sequences.sequences[0];
        let clip = &sequence.video_tracks[0].clips[0];
        let graph = build_effect_render_graph(
            &clip.effects,
            TimelineTime::ZERO,
            sequence.settings.color.working_color_space,
        )
        .expect("prepare authored LUT for visual execution");
        graph
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: EffectRenderOp::Lut3D { lut, .. }, ..
                }
                | EffectGraphNodeKind::DomainEffect {
                    op: EffectRenderOp::Lut3D { lut, .. }, ..
                } => Some(lut.sample([0.25, 0.5, 0.75])),
                _ => None,
            })
            .expect("prepared LUT operation")
    }

    fn render_document_lut_preview_and_export(document: &ProjectDocument) -> (Vec<u8>, Vec<u8>) {
        mondrian_core::ensure_mondrian_default_ocio_loaded().expect("default color config");
        let sequence = &document.sequences.sequences[0];
        let clip = &sequence.video_tracks[0].clips[0];
        let graph = build_effect_render_graph(
            &clip.effects,
            TimelineTime::ZERO,
            sequence.settings.color.working_color_space,
        )
        .expect("prepare authored LUT");
        let compiled = compile_reference_render_graph(graph).expect("compile LUT execution");
        let mut settings = sequence.settings.clone();
        settings.color.program_output.color_space = ColorSpace::Srgb;
        let color_context = settings
            .root_program_color_context(&document.color_environment)
            .expect("color context");
        let layer = TimelineSolidColorLayer {
            color: Color::from_rgba8(64, 128, 192, 255),
            opacity: 1.0,
            blend_mode: mondrian_core::types::BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: compiled,
            frame_seed: 0,
        };
        let mut preview_scratch = TimelineCompositeScratch::default();
        let preview = crate::app::preview_cpu_execution::composite_resolved_preview(
            2,
            2,
            &[crate::app::preview_viewer_plan::ResolvedPreviewElement::SolidColor(layer.clone())],
            &color_context,
            &mut preview_scratch,
        )
        .expect("render Viewer LUT frame")
        .rgba;
        let mut export_scratch = TimelineCompositeScratch::default();
        let working = composite_timeline_elements_color_frame(
            2,
            2,
            &[TimelineCompositeElement::SolidColor(layer)],
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(
                color_context.engine(),
                color_context.working_color_space(),
            ),
            &mut export_scratch,
        )
        .expect("render Export LUT frame");
        let boundary = ProgramOutputModule::boundary(ProgramOutputRole::Export, &color_context)
            .expect("Export output boundary");
        let export = ProgramOutputModule::execute_cpu_rgba8(
            &working,
            &boundary,
            export_scratch.color_execution_mut(),
        )
        .expect("encode Export LUT frame")
        .rgba;
        (preview, export)
    }

    fn document_with_lut(
        library: &AssetLibrary,
        path: PathBuf,
        include_inactive_grade: bool,
    ) -> ProjectDocument {
        let mut sequence = Sequence::new("portable");
        let settings = sequence.settings.clone();
        let asset_id =
            library.create_solid_color_asset(Some("generated")).expect("generated asset");
        let mut clip = Clip::new(
            asset_id,
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("time"),
        )
        .expect("clip");
        clip.add_effect_node(lut_effect(path.clone()));
        sequence.video_tracks[0].add_clip(clip).expect("place clip");
        if include_inactive_grade {
            let mut definition = GradeDefinition::new("shared grade");
            let mut graph = GradeGraph::identity();
            let node_id = GradeGraphNodeId::new();
            graph.nodes.push(GradeGraphNode {
                id: node_id,
                kind: GradeGraphNodeKind::Effect { input: graph.output, effect: lut_effect(path) },
            });
            graph.output = node_id;
            definition.versions.push(GradeVersion::new("Inactive LUT", graph));
            sequence.grade_definitions.push(definition);
        }
        ProjectDocument::new(
            "portable",
            SequenceCollection::new(sequence),
            ProjectColorEnvironment::default(),
            settings,
            ProjectSettings::default(),
        )
    }

    #[test]
    fn inventory_collects_clip_and_inactive_grade_lut_once() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let lut = root.path().join("look.cube");
        fs::write(&lut, RED_INVERT_CUBE_2).expect("LUT fixture");
        let document = document_with_lut(&library, lut.clone(), true);

        let inventory = collect_project_dependencies(&document, &library).expect("inventory");

        assert!(inventory.is_complete());
        assert_eq!(inventory.files.len(), 1);
        assert_eq!(
            inventory.files[0].path,
            fs::canonicalize(lut).expect("canonical LUT")
        );
        assert_eq!(
            inventory.files[0].size_bytes,
            inventory.observed_size_bytes().expect("size")
        );
        assert_eq!(inventory.files[0].owners.len(), 2);
        assert!(inventory.files[0].owners.iter().any(|owner| owner.contains("/grade:")));
    }

    #[test]
    fn inventory_reports_missing_lut_and_asset_without_silent_partial_success() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let missing = root.path().join("missing.cube");
        let mut document = document_with_lut(&library, missing, false);
        let absent = Clip::new(
            AssetId::new(),
            TimelineTime::new(2, 1).expect("time"),
            TimelineTime::new(1, 1).expect("time"),
        )
        .expect("clip");
        document.sequences.sequences[0].video_tracks[0]
            .add_clip(absent)
            .expect("place missing asset clip");

        let inventory = collect_project_dependencies(&document, &library).expect("inventory");

        assert!(!inventory.is_complete());
        assert!(inventory.files.is_empty());
        assert!(inventory.issues.iter().any(|issue| issue.reason.contains("unavailable")));
        assert!(inventory.issues.iter().any(|issue| issue.reason.contains("missing")));
    }

    #[test]
    fn package_survives_source_removal_and_detects_tampering() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let lut = root.path().join("look.cube");
        fs::write(&lut, RED_INVERT_CUBE_2).expect("LUT fixture");
        let document = document_with_lut(&library, lut.clone(), true);
        let expected = sample_document_lut(&document);
        let (original_preview, original_export) = render_document_lut_preview_and_export(&document);
        assert_eq!(original_preview, original_export);
        let mut without_lut = document.clone();
        without_lut.sequences.sequences[0].video_tracks[0].clips[0].effects.clear();
        let (identity_preview, identity_export) =
            render_document_lut_preview_and_export(&without_lut);
        assert_eq!(identity_preview, identity_export);
        assert_ne!(
            original_preview, identity_preview,
            "LUT must change rendered pixels"
        );
        assert!(
            (expected[0] - 0.75).abs() < 1.0e-6,
            "LUT must change red: {expected:?}"
        );
        let session = AuthoringSession::new_unsaved(
            document,
            root.path().join("original.mdp"),
            root.path().join("runtime"),
            library,
        )
        .expect("session");
        let package = root.path().join("portable.mdpkg");
        export_snapshot_package(session.snapshot().expect("snapshot"), &package)
            .expect("export package");
        fs::remove_file(&lut).expect("remove original LUT");
        let moved = root.path().join("moved.mdpkg");
        fs::rename(&package, &moved).expect("move package");
        let manifest = verify_portable_project_package(&moved).expect("complete package");
        assert_eq!(fs::read_dir(&moved).expect("package entries").count(), 3);
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].source_paths, vec![lut]);
        assert!(export_snapshot_package(session.snapshot().expect("snapshot"), &package).is_err());

        fs::write(moved.with_extension("mdp"), b"existing project").expect("occupied sibling");
        let mut imported = AppState::new();
        imported
            .dispatch_action(Action::OpenProject(moved.join("project.mdp")))
            .expect("open moved package from dialog selection");
        let rebound = imported.portable_project_dependency_inventory().expect("rebound inventory");
        let imported_document = imported.authoring.as_ref().expect("Session").document();
        let actual = sample_document_lut(imported_document);
        let (moved_preview, moved_export) =
            render_document_lut_preview_and_export(imported_document);
        assert_eq!(moved_preview, moved_export);
        assert_eq!(moved_preview, original_preview);
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-6);
        }
        assert!(rebound.is_complete());
        assert_eq!(
            rebound.files[0].path,
            fs::canonicalize(moved.join(&manifest.files[0].bundled_path)).expect("bundled LUT")
        );
        assert!(imported.has_unsaved_project_changes());
        assert_eq!(
            fs::read(moved.with_extension("mdp")).expect("existing sibling"),
            b"existing project"
        );
        assert_eq!(
            imported.authoring.as_ref().expect("Session").project_file(),
            moved.with_file_name("moved-imported-1.mdp")
        );
        assert!(!moved.with_file_name("moved-imported-1.mdp").exists());
        drop(imported);

        let mut omitted_lut = manifest.clone();
        omitted_lut.files.clear();
        fs::write(
            moved.join("manifest.json"),
            serde_json::to_vec(&omitted_lut).expect("encode omitted LUT"),
        )
        .expect("omit LUT binding");
        verify_portable_project_package(&moved).expect("byte-valid missing LUT binding");
        let mut rejected_missing_lut = AppState::new();
        let error = rejected_missing_lut
            .open_portable_project_package(moved.clone())
            .expect_err("an unbound authored LUT must block import");
        assert!(error.to_string().contains("absent from portable manifest"));
        assert!(!rejected_missing_lut.has_open_project());
        fs::write(
            moved.join("manifest.json"),
            serde_json::to_vec(&manifest).expect("encode restored manifest"),
        )
        .expect("restore manifest");

        let mut unsafe_manifest = manifest.clone();
        unsafe_manifest.files[0].bundled_path = PathBuf::from("files").join("..").join("escape");
        fs::write(
            moved.join("manifest.json"),
            serde_json::to_vec(&unsafe_manifest).expect("encode"),
        )
        .expect("tamper manifest");
        assert!(verify_portable_project_package(&moved).is_err());
        fs::write(
            moved.join("manifest.json"),
            serde_json::to_vec(&manifest).expect("encode"),
        )
        .expect("restore manifest");
        fs::write(moved.join(&manifest.files[0].bundled_path), b"corrupt").expect("tamper package");
        assert!(verify_portable_project_package(&moved).is_err());
        let mut rejected = AppState::new();
        assert!(rejected.open_portable_project_package(moved).is_err());
        assert!(!rejected.has_open_project());
    }

    #[test]
    fn unresolved_dependency_leaves_no_package() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let document = document_with_lut(&library, root.path().join("missing.cube"), false);
        let session = AuthoringSession::new_unsaved(
            document,
            root.path().join("original.mdp"),
            root.path().join("runtime"),
            library,
        )
        .expect("session");
        let package = root.path().join("incomplete.mdpkg");
        assert!(export_snapshot_package(session.snapshot().expect("snapshot"), &package).is_err());
        assert!(!package.exists());
    }

    #[test]
    fn canceled_package_export_removes_its_staging_directory() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let lut = root.path().join("look.cube");
        fs::write(&lut, RED_INVERT_CUBE_2).expect("LUT fixture");
        let document = document_with_lut(&library, lut, false);
        let session = AuthoringSession::new_unsaved(
            document,
            root.path().join("original.mdp"),
            root.path().join("runtime"),
            library,
        )
        .expect("session");
        let before = fs::read_dir(root.path()).expect("before entries").count();
        let target = root.path().join("cancel.mdpkg");
        let result = export_snapshot_package_with_progress(
            session.snapshot().expect("snapshot"),
            &target,
            &AtomicBool::new(true),
            &AtomicU64::new(0),
            &AtomicU64::new(0),
            &AtomicU8::new(0),
        );
        assert!(result.is_err_and(|error| error.is::<PortableExportCanceled>()));
        assert!(!target.exists());
        assert_eq!(
            fs::read_dir(root.path()).expect("after entries").count(),
            before
        );
    }

    #[test]
    fn background_package_export_reports_completion() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let document = document_with_lut(&library, root.path().join("unused.cube"), false);
        let mut state = AppState::new();
        state.authoring = Some(
            AuthoringSession::new_unsaved(
                document,
                root.path().join("original.mdp"),
                root.path().join("runtime"),
                library,
            )
            .expect("session"),
        );
        let target = root.path().join("background.mdpkg");
        // An unresolved LUT fails in the worker without blocking the editor.
        state
            .request_portable_project_export(target.clone())
            .expect("start failed export");
        for _ in 0..1000 {
            state.poll_portable_project_export();
            if !state.portable_project_export_active() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!state.portable_project_export_active());
        assert!(!target.exists());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        assert!(state
            .notifications
            .iter()
            .any(|fact| fact.message.id == "notification-package-failed"));

        let lut = root.path().join("look.cube");
        fs::write(&lut, RED_INVERT_CUBE_2).expect("LUT fixture");
        let library = AssetLibrary::open(root.path().join("second-library")).expect("library");
        let document = document_with_lut(&library, lut, false);
        state.authoring = Some(
            AuthoringSession::new_unsaved(
                document,
                root.path().join("second.mdp"),
                root.path().join("second-runtime"),
                library,
            )
            .expect("second session"),
        );
        state
            .dispatch_action(
                crate::app::ui_actions::app_shell_export_portable_package_action(target.clone()),
            )
            .expect("start export action");
        assert!(state.portable_project_export_active());
        assert!(state.request_portable_project_export(target.clone()).is_err());
        for _ in 0..1000 {
            state.poll_portable_project_export();
            if !state.portable_project_export_active() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !state.portable_project_export_active(),
            "worker did not complete"
        );
        assert!(target.is_dir());
        verify_portable_project_package(&target).expect("complete background package");
        assert!(state
            .notifications
            .iter()
            .any(|fact| fact.message.id == "notification-package-complete"));
        assert!(state.request_portable_project_export(target).is_err());
    }

    #[test]
    fn moved_package_rebinds_sqlite_file_asset_without_losing_probe() {
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let media = root.path().join("voice.m4a");
        fs::write(&media, [1u8]).expect("media fixture");
        let source = fs::canonicalize(&media).expect("canonical media");
        let info = MediaInfo {
            duration: Duration::from_secs(1),
            file_size: 1,
            container: "m4a".to_owned(),
            video_streams: Vec::new(),
            audio_streams: vec![AudioStreamInfo {
                index: 0,
                stream_id: Some(1),
                language: None,
                title: None,
                is_default: true,
                codec: AudioCodec::Aac,
                duration: Some(Duration::from_secs(1)),
                sample_rate: 48_000,
                channels: 2,
                channel_layout: ChannelLayout::Stereo,
                bit_depth: 24,
                avg_bitrate: 192_000,
            }],
            has_video: false,
            has_audio: true,
        };
        let candidate = AssetMediaProbeCandidate::new(
            source.clone(),
            MediaFileFingerprint::capture(&source),
            info,
        )
        .expect("probe candidate");
        let asset_id = library.commit_media_probe(candidate, None).expect("commit media");
        let stored_source = library
            .get_asset(asset_id)
            .expect("lookup")
            .expect("Asset")
            .file_path()
            .expect("file source")
            .to_path_buf();
        let lut = root.path().join("look.cube");
        fs::write(&lut, RED_INVERT_CUBE_2).expect("LUT fixture");
        let document = document_with_lut(&library, lut, false);
        let session = AuthoringSession::new_unsaved(
            document,
            root.path().join("original.mdp"),
            root.path().join("runtime"),
            library,
        )
        .expect("session");
        let package = root.path().join("portable.mdpkg");
        export_snapshot_package(session.snapshot().expect("snapshot"), &package).expect("export");
        fs::remove_file(&media).expect("remove original media");
        let moved = root.path().join("moved.mdpkg");
        fs::rename(&package, &moved).expect("move package");
        let manifest = verify_portable_project_package(&moved).expect("package");
        let packaged = manifest
            .files
            .iter()
            .find(|file| file.source_paths.contains(&stored_source))
            .map(|file| moved.join(&file.bundled_path))
            .expect("media binding");

        let mut imported = AppState::new();
        imported
            .open_portable_project_package(moved.clone())
            .expect("open moved package");
        let asset = imported
            .authoring
            .as_ref()
            .expect("Session")
            .asset_library()
            .get_asset(asset_id)
            .expect("lookup")
            .expect("Asset");
        assert_eq!(
            asset.file_path(),
            Some(
                mondrian_assets::canonical_native_path(&packaged)
                    .expect("packaged media")
                    .as_path()
            )
        );
        assert!(asset.media_probe().is_some());
        assert!(asset.source_fingerprint().is_some());
        assert!(asset
            .admitted_audio_source_selection(AudioSourceComponentId::primary())
            .is_some());
        drop(imported);

        let mut omitted = manifest;
        omitted.files.retain(|file| !file.source_paths.contains(&stored_source));
        fs::write(
            moved.join("manifest.json"),
            serde_json::to_vec(&omitted).expect("encode tampered manifest"),
        )
        .expect("omit media binding");
        // All remaining files and the nested Project are byte-valid. Import
        // must still reject a Library row that would keep its original path.
        verify_portable_project_package(&moved).expect("byte-valid subset");
        let mut rejected = AppState::new();
        let error = rejected
            .open_portable_project_package(moved)
            .expect_err("omitted SQLite media source must reject import");
        assert!(
            error.to_string().contains("unbound file source"),
            "{error:#}"
        );
        assert!(!rejected.has_open_project());
    }

    #[test]
    fn moved_package_decodes_real_video_and_admits_export_from_bundled_media() {
        const VIDEO: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
        let root = tempfile::tempdir().expect("root");
        let library = AssetLibrary::open(root.path().join("library")).expect("library");
        let media = root.path().join("source.mp4");
        fs::write(&media, VIDEO).expect("video fixture");
        let source = fs::canonicalize(&media).expect("canonical video");
        let info = mondrian_media::probe_media_info(&source).expect("probe real video");
        let video = info.primary_video().expect("video stream");
        let request_color = PreviewSourceColorContract::new(
            video.executable_color_space().expect("admitted source color"),
            DecodedVideoRangeContract::Automatic { probed_range: video.color_range },
        );
        let stream_index = video.index;
        let fingerprint = MediaFileFingerprint::capture(&source);
        let candidate = AssetMediaProbeCandidate::new(source.clone(), fingerprint, info)
            .expect("probe candidate");
        let asset_id = library.commit_media_probe(candidate, None).expect("commit video");
        let stored_source = library
            .get_asset(asset_id)
            .expect("lookup video")
            .expect("video Asset")
            .file_path()
            .expect("file source")
            .to_path_buf();
        let original = decode_preview_frame_cancellable(
            PreviewDecodeRequest::new(
                &source,
                mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                request_color,
            )
            .with_video_stream_index(stream_index)
            .with_fingerprint(fingerprint),
            || false,
        )
        .expect("decode source video");
        let PreviewDecodeOutcome::Frame(original) = original else {
            panic!("real video must yield a CPU RGBA preview frame");
        };
        clear_thread_local_preview_decode_session();

        let mut sequence = Sequence::new("portable video");
        sequence.video_tracks[0]
            .add_clip(
                Clip::new(
                    asset_id,
                    TimelineTime::ZERO,
                    TimelineTime::new(1, 1).expect("time"),
                )
                .expect("video clip"),
            )
            .expect("place video");
        let settings = sequence.settings.clone();
        let document = ProjectDocument::new(
            "portable video",
            SequenceCollection::new(sequence),
            ProjectColorEnvironment::default(),
            settings,
            ProjectSettings::default(),
        );
        let session = AuthoringSession::new_unsaved(
            document,
            root.path().join("original.mdp"),
            root.path().join("runtime"),
            library,
        )
        .expect("session");
        let package = root.path().join("portable.mdpkg");
        export_snapshot_package(session.snapshot().expect("snapshot"), &package).expect("export");
        fs::remove_file(&source).expect("remove source video");
        let moved = root.path().join("moved.mdpkg");
        fs::rename(&package, &moved).expect("move package");
        let manifest = verify_portable_project_package(&moved).expect("package");
        let bundled = manifest
            .files
            .iter()
            .find(|file| file.source_paths.contains(&stored_source))
            .map(|file| {
                mondrian_assets::canonical_native_path(&moved.join(&file.bundled_path))
                    .expect("bundled video")
            })
            .expect("video binding");

        let mut imported = AppState::new();
        imported.open_portable_project_package(moved).expect("open moved package");
        let asset = imported
            .authoring
            .as_ref()
            .expect("Session")
            .asset_library()
            .get_asset(asset_id)
            .expect("lookup")
            .expect("Asset");
        assert_eq!(asset.file_path(), Some(bundled.as_path()));
        let rebound_fingerprint = asset.source_fingerprint().expect("rebound fingerprint");
        let decoded = decode_preview_frame_cancellable(
            PreviewDecodeRequest::new(
                &bundled,
                mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                request_color,
            )
            .with_video_stream_index(stream_index)
            .with_fingerprint(rebound_fingerprint),
            || false,
        )
        .expect("decode bundled video");
        let PreviewDecodeOutcome::Frame(decoded) = decoded else {
            panic!("bundled video must yield a CPU RGBA preview frame");
        };
        assert_eq!(
            (decoded.width, decoded.height),
            (original.width, original.height)
        );
        assert_eq!(decoded.rgba(), original.rgba());
        clear_thread_local_preview_decode_session();

        let sequence = imported.active_sequence().expect("sequence").clone();
        let export = super::super::exporting::capture_timeline_export_snapshot(
            &imported,
            sequence.clone(),
            vec![sequence],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("admit export from bundled video");
        let dependency = export.media.get(&asset_id).expect("video export dependency");
        assert_eq!(dependency.path, bundled);
        assert_eq!(dependency.video_stream_index, Some(stream_index));
        assert_eq!(dependency.source_fingerprint, rebound_fingerprint);
    }
}
