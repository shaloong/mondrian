//! Project document and `.mdp` container contract.
//!
//! Mondrian uses a Premiere-style lightweight project file. The `.mdp`
//! archive stores the canonical project document and the project asset-library
//! index; generated caches, proxies, waveforms, and preview renders live
//! outside the project archive.

use anyhow::Context;
use mondrian_core::{AssetId, ProjectId, ProjectMeta, ProjectSettings};
use mondrian_timeline::SequenceCollection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Current `.mdp` container format version.
pub const PROJECT_FORMAT_VERSION: u32 = 1;
/// Current canonical project document schema version.
pub const PROJECT_DOCUMENT_SCHEMA_VERSION: u32 = 1;

/// Entry name for the archive manifest.
pub const MANIFEST_ENTRY: &str = "manifest.json";
/// Entry name for the canonical project document.
pub const PROJECT_ENTRY: &str = "project.json";
/// Entry name for the embedded project asset-library SQLite database.
pub const LIBRARY_ENTRY: &str = "library/index.db";

const PROJECT_FORMAT_NAME: &str = "mondrian-project";
const DOCUMENT_LAYOUT_SINGLE_JSON: &str = "single-project-json";

/// `.mdp` archive manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifest {
    /// Archive family identifier. Current value is `mondrian-project`.
    pub format: String,
    /// Container-level format version.
    pub format_version: u32,
    /// Document layout strategy within the archive.
    pub document_layout: String,
    /// Archive entry containing the canonical project document.
    pub project_entry: String,
    /// Archive entry containing the project asset-library SQLite database.
    pub library_entry: String,
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            format: PROJECT_FORMAT_NAME.to_string(),
            format_version: PROJECT_FORMAT_VERSION,
            document_layout: DOCUMENT_LAYOUT_SINGLE_JSON.to_string(),
            project_entry: PROJECT_ENTRY.to_string(),
            library_entry: LIBRARY_ENTRY.to_string(),
        }
    }
}

impl ProjectManifest {
    /// Validate that the archive is a supported alpha project format.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format != PROJECT_FORMAT_NAME {
            anyhow::bail!("unsupported project archive format: {}", self.format);
        }
        if self.format_version != PROJECT_FORMAT_VERSION {
            anyhow::bail!(
                "unsupported project archive version: {}",
                self.format_version
            );
        }
        if self.document_layout != DOCUMENT_LAYOUT_SINGLE_JSON {
            anyhow::bail!(
                "unsupported project document layout: {}",
                self.document_layout
            );
        }
        if self.project_entry != PROJECT_ENTRY {
            anyhow::bail!("unsupported project entry: {}", self.project_entry);
        }
        if self.library_entry != LIBRARY_ENTRY {
            anyhow::bail!("unsupported library entry: {}", self.library_entry);
        }
        Ok(())
    }
}

/// Canonical persisted project document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDocument {
    /// JSON document schema version.
    pub schema_version: u32,
    /// Stable project identity used by caches and external references.
    pub project_id: ProjectId,
    /// Monotonic revision advanced on explicit project saves.
    pub document_revision: u64,
    /// User-facing project metadata.
    pub meta: ProjectMeta,
    /// Project-wide settings inherited by sequences where applicable.
    pub settings: ProjectSettings,
    /// Complete sequence collection for the current single-document layout.
    pub sequences: SequenceCollection,
    /// Assets currently forced into proxy playback mode.
    pub proxy_mode_assets: Vec<AssetId>,
}

impl ProjectDocument {
    /// Build a new canonical project document.
    pub fn new(
        name: impl Into<String>,
        sequences: SequenceCollection,
        settings: ProjectSettings,
    ) -> Self {
        Self {
            schema_version: PROJECT_DOCUMENT_SCHEMA_VERSION,
            project_id: ProjectId::new(),
            document_revision: 1,
            meta: ProjectMeta::new(name),
            settings,
            sequences,
            proxy_mode_assets: Vec::new(),
        }
    }

    /// Validate document-level invariants before opening or saving.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != PROJECT_DOCUMENT_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported project document schema version: {}",
                self.schema_version
            );
        }
        if self.meta.name.trim().is_empty() {
            anyhow::bail!("project name cannot be empty");
        }
        self.sequences.validate_nested_sequences()?;
        if self.sequences.active().is_none() {
            anyhow::bail!("project document has no active sequence");
        }
        Ok(())
    }

    /// Keep ID-list ordering deterministic for stable fingerprints.
    pub fn normalize_for_save(&mut self) {
        self.proxy_mode_assets.sort_by_key(|id| id.to_string());
        self.proxy_mode_assets.dedup();
    }

    /// Return a copy with deterministic ordering applied.
    pub fn normalized(mut self) -> Self {
        self.normalize_for_save();
        self
    }
}

/// Compute a stable serialized fingerprint for dirty checks.
pub fn project_document_fingerprint(document: ProjectDocument) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&document.normalized())?)
}

/// Read and validate only the canonical project document from an archive.
pub fn read_project_document_from_archive(project_file: &Path) -> anyhow::Result<ProjectDocument> {
    let file = fs::File::open(project_file)?;
    let mut archive = zip::ZipArchive::new(file)?;
    read_project_document_from_zip(&mut archive)
}

/// Open an `.mdp` archive, validate the document, and extract the library DB.
pub fn load_project_archive(
    archive_file: &Path,
    runtime_library_root: &Path,
) -> anyhow::Result<ProjectDocument> {
    fs::create_dir_all(runtime_library_root)?;

    let file = fs::File::open(archive_file)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let document = read_project_document_from_zip(&mut archive)?;

    let mut db_entry = archive
        .by_name(LIBRARY_ENTRY)
        .with_context(|| format!("missing project archive entry: {LIBRARY_ENTRY}"))?;
    let db_path = runtime_library_root.join("index.db");
    let mut db_file = fs::File::create(db_path)?;
    std::io::copy(&mut db_entry, &mut db_file)?;
    db_file.flush()?;

    Ok(document)
}

/// Save an `.mdp` archive atomically next to the target file.
pub fn save_project_archive(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &Path,
) -> anyhow::Result<()> {
    let document = document.clone().normalized();
    document.validate()?;

    if !library_db_path.exists() {
        anyhow::bail!(
            "asset library database does not exist: {}",
            library_db_path.display()
        );
    }

    if let Some(parent) = target_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = temporary_archive_path(target_file);
    let result = write_project_archive(&document, library_db_path, tmp_path.as_path());
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if target_file.exists() {
        fs::remove_file(target_file)?;
    }
    fs::rename(&tmp_path, target_file)?;
    Ok(())
}

fn read_project_document_from_zip<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<ProjectDocument> {
    let manifest = {
        let mut manifest_json = String::new();
        archive
            .by_name(MANIFEST_ENTRY)
            .with_context(|| format!("missing project archive entry: {MANIFEST_ENTRY}"))?
            .read_to_string(&mut manifest_json)?;
        serde_json::from_str::<ProjectManifest>(&manifest_json)?
    };
    manifest.validate()?;

    let mut project_json = String::new();
    archive
        .by_name(PROJECT_ENTRY)
        .with_context(|| format!("missing project archive entry: {PROJECT_ENTRY}"))?
        .read_to_string(&mut project_json)?;
    let document = serde_json::from_str::<ProjectDocument>(&project_json)?;
    document.validate()?;
    Ok(document.normalized())
}

fn write_project_archive(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &Path,
) -> anyhow::Result<()> {
    let tmp_file = fs::File::create(target_file)?;
    let mut writer = zip::ZipWriter::new(tmp_file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    writer.start_file(MANIFEST_ENTRY, options)?;
    writer.write_all(&serde_json::to_vec_pretty(&ProjectManifest::default())?)?;

    writer.start_file(PROJECT_ENTRY, options)?;
    writer.write_all(&serde_json::to_vec_pretty(document)?)?;

    writer.start_file(LIBRARY_ENTRY, options)?;
    let mut db_file = fs::File::open(library_db_path)?;
    std::io::copy(&mut db_file, &mut writer)?;

    writer.finish()?;
    Ok(())
}

fn temporary_archive_path(target_file: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    target_file.to_string_lossy().hash(&mut hasher);
    let pid = std::process::id();
    let suffix = format!("{}.tmp", hasher.finish());
    target_file.with_extension(format!("mdp.{pid}.{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_timeline::Sequence;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mondrian-project-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create temp dir");
        root
    }

    fn test_document() -> ProjectDocument {
        let sequence = Sequence::new("Main");
        let collection = SequenceCollection::new(sequence);
        ProjectDocument::new("Main", collection, ProjectSettings::default())
    }

    #[test]
    fn manifest_default_describes_current_archive_layout() {
        let manifest = ProjectManifest::default();

        assert_eq!(manifest.format, "mondrian-project");
        assert_eq!(manifest.format_version, PROJECT_FORMAT_VERSION);
        assert_eq!(manifest.document_layout, "single-project-json");
        assert_eq!(manifest.project_entry, PROJECT_ENTRY);
        assert_eq!(manifest.library_entry, LIBRARY_ENTRY);
        manifest.validate().expect("default manifest should validate");
    }

    #[test]
    fn project_archive_round_trips_document_and_library() {
        let root = unique_temp_dir("round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("project.mdp");

        let document = test_document();
        save_project_archive(&document, &db_path, &project_path).expect("save archive");

        let opened = read_project_document_from_archive(&project_path).expect("read document");
        assert_eq!(opened.project_id, document.project_id);
        assert_eq!(opened.meta.name, "Main");
        assert_eq!(opened.sequences.sequences.len(), 1);

        let runtime_library = root.join("runtime-library");
        let loaded =
            load_project_archive(&project_path, &runtime_library).expect("load project archive");
        assert_eq!(loaded.project_id, document.project_id);
        assert_eq!(
            fs::read(runtime_library.join("index.db")).expect("read extracted db"),
            b"sqlite placeholder"
        );
    }

    #[test]
    fn archive_without_manifest_is_rejected() {
        let root = unique_temp_dir("missing-manifest");
        let project_path = root.join("legacy.mdp");
        let file = fs::File::create(&project_path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file(PROJECT_ENTRY, options).expect("start project");
        writer.write_all(b"{}").expect("write project");
        writer.finish().expect("finish archive");

        let err = read_project_document_from_archive(&project_path)
            .expect_err("legacy archive should be rejected");
        assert!(err.to_string().contains("manifest"));
    }

    #[test]
    fn document_fingerprint_is_stable_for_proxy_asset_order() {
        let mut first = test_document();
        let mut second = first.clone();
        let a = AssetId::new();
        let b = AssetId::new();
        first.proxy_mode_assets = vec![a, b];
        second.proxy_mode_assets = vec![b, a];

        assert_eq!(
            project_document_fingerprint(first).expect("first fingerprint"),
            project_document_fingerprint(second).expect("second fingerprint")
        );
    }
}
