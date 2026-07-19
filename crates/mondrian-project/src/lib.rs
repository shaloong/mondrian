//! Project document and `.mdp` container contract.
//!
//! Mondrian uses a Premiere-style lightweight project file. The `.mdp`
//! archive stores the canonical project document and the project asset-library
//! index; generated caches, proxies, waveforms, and preview renders live
//! outside the project archive.

use anyhow::Context;
use mondrian_core::{automation::PropertyHost, AssetId, ProjectId, ProjectMeta, ProjectSettings};
use mondrian_timeline::SequenceCollection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

mod migration;

use migration::JsonMigrationRegistry;

/// Current `.mdp` container format version.
pub const PROJECT_FORMAT_VERSION: u32 = 1;
/// Current canonical project document schema version.
pub const PROJECT_DOCUMENT_SCHEMA_VERSION: u32 = 11;
/// Current embedded asset-library SQLite schema version.
pub const PROJECT_LIBRARY_SCHEMA_VERSION: u32 = 1;

/// Entry name for the archive manifest.
pub const MANIFEST_ENTRY: &str = "manifest.json";
/// Entry name for the canonical project document.
pub const PROJECT_ENTRY: &str = "project.json";
/// Entry name for the embedded project asset-library SQLite database.
pub const LIBRARY_ENTRY: &str = "library/index.db";

const PROJECT_FORMAT_NAME: &str = "mondrian-project";
const DOCUMENT_LAYOUT_SINGLE_JSON: &str = "single-project-json";
const ARCHIVE_MIGRATIONS: JsonMigrationRegistry = JsonMigrationRegistry::new(
    "project archive",
    "format_version",
    PROJECT_FORMAT_VERSION,
    &[],
);
const DOCUMENT_MIGRATIONS: JsonMigrationRegistry = JsonMigrationRegistry::new(
    "project document",
    "schema_version",
    PROJECT_DOCUMENT_SCHEMA_VERSION,
    &[],
);

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
    /// Expected SQLite schema version after runtime-copy migration.
    pub library_schema_version: u32,
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            format: PROJECT_FORMAT_NAME.to_string(),
            format_version: PROJECT_FORMAT_VERSION,
            document_layout: DOCUMENT_LAYOUT_SINGLE_JSON.to_string(),
            project_entry: PROJECT_ENTRY.to_string(),
            library_entry: LIBRARY_ENTRY.to_string(),
            library_schema_version: PROJECT_LIBRARY_SCHEMA_VERSION,
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
        if self.library_schema_version > PROJECT_LIBRARY_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported project library schema version: {}",
                self.library_schema_version
            );
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
        for sequence in &self.sequences.sequences {
            if sequence.revision.get() == 0 {
                anyhow::bail!(
                    "sequence '{}' has invalid author revision zero",
                    sequence.name
                );
            }
            sequence
                .settings
                .validate_with_project_color_management(&self.settings.color_management)
                .with_context(|| {
                    format!("sequence '{}' color management is invalid", sequence.name)
                })?;
            for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
                track.property_bag()?.validate().with_context(|| {
                    format!("track '{}' parameter schema is invalid", track.name)
                })?;
                for clip in &track.clips {
                    clip.property_bag()?.validate().with_context(|| {
                        format!("clip '{}' parameter schema is invalid", clip.id)
                    })?;
                    for effect in &clip.effects {
                        effect.validate_author_state().with_context(|| {
                            format!("effect '{}' author state is invalid", effect.id)
                        })?;
                    }
                }
            }
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

/// Fully loaded archive payload with independently versioned SQLite evidence.
pub struct LoadedProjectArchive {
    /// Migrated and validated canonical project document.
    pub document: ProjectDocument,
    /// SQLite schema version declared by the archive manifest.
    pub library_schema_version: u32,
}

/// Open an `.mdp` archive, validate the document, and extract the library DB.
pub fn load_project_archive(
    archive_file: &Path,
    runtime_library_root: &Path,
) -> anyhow::Result<LoadedProjectArchive> {
    fs::create_dir_all(runtime_library_root)?;

    let file = fs::File::open(archive_file)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let (manifest, document) = read_project_archive_metadata_from_zip(&mut archive)?;
    validate_project_color_engines(&document)?;

    let mut db_entry = archive
        .by_name(LIBRARY_ENTRY)
        .with_context(|| format!("missing project archive entry: {LIBRARY_ENTRY}"))?;
    let db_path = runtime_library_root.join("index.db");
    let db_tmp_path = temporary_sibling_path(&db_path, "extract");
    let mut db_file = fs::File::create(&db_tmp_path)?;
    std::io::copy(&mut db_entry, &mut db_file)?;
    db_file.flush()?;
    db_file.sync_all()?;
    replace_file_preserving_original(&db_tmp_path, &db_path)?;

    Ok(LoadedProjectArchive {
        document,
        library_schema_version: manifest.library_schema_version,
    })
}

fn validate_project_color_engines(document: &ProjectDocument) -> anyhow::Result<()> {
    let mut engines = vec![document.settings.color_management.engine.clone()];
    engines.extend(
        document
            .sequences
            .sequences
            .iter()
            .filter(|sequence| !sequence.settings.color_management.inherit)
            .map(|sequence| sequence.settings.color_management.engine.clone()),
    );
    let mut validated = Vec::new();
    for engine in engines {
        if validated.contains(&engine) {
            continue;
        }
        engine.ensure_loaded().map_err(|reason| {
            anyhow::anyhow!(
                "project color engine '{}' failed validation: {reason}",
                engine.name()
            )
        })?;
        validated.push(engine);
    }
    Ok(())
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
    read_project_document_from_archive(&tmp_path)
        .context("new project archive failed reopen validation")?;
    replace_file_preserving_original(&tmp_path, target_file)?;
    Ok(())
}

fn read_project_document_from_zip<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<ProjectDocument> {
    Ok(read_project_archive_metadata_from_zip(archive)?.1)
}

fn read_project_archive_metadata_from_zip<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<(ProjectManifest, ProjectDocument)> {
    let manifest = {
        let mut manifest_json = String::new();
        archive
            .by_name(MANIFEST_ENTRY)
            .with_context(|| format!("missing project archive entry: {MANIFEST_ENTRY}"))?
            .read_to_string(&mut manifest_json)?;
        let value = serde_json::from_str(&manifest_json)?;
        serde_json::from_value::<ProjectManifest>(ARCHIVE_MIGRATIONS.migrate(value)?)?
    };
    manifest.validate()?;

    let mut project_json = String::new();
    archive
        .by_name(PROJECT_ENTRY)
        .with_context(|| format!("missing project archive entry: {PROJECT_ENTRY}"))?
        .read_to_string(&mut project_json)?;
    let value = serde_json::from_str(&project_json)?;
    let document = serde_json::from_value::<ProjectDocument>(DOCUMENT_MIGRATIONS.migrate(value)?)?;
    document.validate()?;
    Ok((manifest, document.normalized()))
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
    temporary_sibling_path(target_file, "archive")
}

fn temporary_sibling_path(target_file: &Path, purpose: &str) -> PathBuf {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    target_file.to_string_lossy().hash(&mut hasher);
    let pid = std::process::id();
    let nonce = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("{purpose}.{}.{nonce}.tmp", hasher.finish());
    target_file.with_extension(format!("mdp.{pid}.{suffix}"))
}

fn replace_file_preserving_original(temp_file: &Path, target_file: &Path) -> anyhow::Result<()> {
    replace_file_preserving_original_with(temp_file, target_file, |source, target| {
        fs::rename(source, target)
    })
}

fn replace_file_preserving_original_with(
    temp_file: &Path,
    target_file: &Path,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    if !temp_file.is_file() {
        anyhow::bail!("replacement source is not a file: {}", temp_file.display());
    }
    if !target_file.exists() {
        replace(temp_file, target_file)?;
        return Ok(());
    }
    let backup = temporary_sibling_path(target_file, "backup");
    if backup.exists() {
        fs::remove_file(&backup)?;
    }
    fs::rename(target_file, &backup)
        .with_context(|| format!("failed to stage original file: {}", target_file.display()))?;
    match replace(temp_file, target_file) {
        Ok(()) => {
            fs::remove_file(&backup)?;
            Ok(())
        }
        Err(replace_error) => {
            let restore_result = fs::rename(&backup, target_file);
            let _ = fs::remove_file(temp_file);
            match restore_result {
                Ok(()) => Err(replace_error).context("failed to replace file; original restored"),
                Err(restore_error) => Err(anyhow::anyhow!(
                    "failed to replace {} ({replace_error}) and restore backup {} ({restore_error})",
                    target_file.display(),
                    backup.display()
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
    use mondrian_core::effect_data::{EffectNode, EffectType};
    use mondrian_core::{ExactAutomationKeyframe, ParameterId, TimelineTime};
    use mondrian_timeline::audio::{
        AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
    };
    use mondrian_timeline::{Clip, Sequence};

    fn missing_custom_engine(path: PathBuf) -> mondrian_core::ColorEngine {
        mondrian_core::ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    mondrian_core::OcioConfigSource::Path { path },
                    "0".repeat(64),
                    "missing-config".to_owned(),
                    "0".repeat(64),
                    "Linear Rec.2020".to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        mondrian_core::ColorSpace::Rec709,
                        "Test Display".to_owned(),
                        "Test View".to_owned(),
                        "Test Display Color Space".to_owned(),
                        mondrian_core::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Custom OCIO output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid missing Custom OCIO identity"),
            ),
        }
    }

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
        assert_eq!(
            manifest.library_schema_version,
            PROJECT_LIBRARY_SCHEMA_VERSION
        );
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
        let opened_sequence = opened.sequences.active().expect("active sequence after reopen");
        assert_eq!(
            opened_sequence.revision,
            document.sequences.active().expect("source sequence").revision
        );
        assert_eq!(
            opened_sequence.settings.color_management.workflow,
            mondrian_timeline::sequence::ColorWorkflow::SceneReferred
        );
        let opened_context = opened_sequence
            .settings
            .root_program_color_context(&opened.settings.color_management);
        assert_eq!(
            opened_context.engine,
            mondrian_core::ColorEngine::mondrian_standard()
        );
        assert_eq!(
            opened_context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );

        let runtime_library = root.join("runtime-library");
        let loaded =
            load_project_archive(&project_path, &runtime_library).expect("load project archive");
        assert_eq!(loaded.document.project_id, document.project_id);
        assert_eq!(
            loaded.library_schema_version,
            PROJECT_LIBRARY_SCHEMA_VERSION
        );
        let loaded_sequence =
            loaded.document.sequences.active().expect("active sequence after archive load");
        assert_eq!(
            loaded_sequence.settings.color_management.workflow,
            mondrian_timeline::sequence::ColorWorkflow::SceneReferred
        );
        assert_eq!(
            fs::read(runtime_library.join("index.db")).expect("read extracted db"),
            b"sqlite placeholder"
        );
    }

    #[test]
    fn document_validation_rejects_zero_sequence_revision() {
        let mut value = serde_json::to_value(test_document()).expect("serialize document");
        value["sequences"]["sequences"][0]["revision"] = serde_json::json!(0);
        let document = serde_json::from_value::<ProjectDocument>(value)
            .expect("zero revision remains structurally deserializable");

        let error = document.validate().expect_err("zero revision must fail");
        assert!(error.to_string().contains("author revision zero"));
    }

    #[test]
    fn parameter_schema_and_instance_address_round_trip_without_identity_drift() {
        let root = unique_temp_dir("parameter-schema-round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("parameter-schema.mdp");

        let mut document = test_document();
        let parameter_id = ParameterId::new_static("mondrian.effect.builtin.gaussian_blur.radius");
        let mut effect = EffectNode::new(EffectType::GaussianBlur);
        effect.define_property(
            PropertyDescriptor::new(
                "effect.gaussian_blur.radius",
                "Radius",
                PropertyValue::Float(12.0),
            )
            .with_parameter_id(parameter_id.clone()),
        );
        let mut clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(5, 1).expect("duration"),
        )
        .expect("clip");
        let effect_id = clip.add_effect_node(effect);
        document.sequences.active_mut().expect("active sequence").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");

        save_project_archive(&document, &db_path, &project_path).expect("save project");
        let reopened = read_project_document_from_archive(&project_path).expect("reopen project");
        let reopened_effect =
            &reopened.sequences.active().expect("active sequence").video_tracks[0].clips[0].effects
                [0];
        let (address, property) = reopened_effect.properties.iter().next().expect("property");

        assert_eq!(reopened_effect.id, effect_id);
        assert_eq!(property.descriptor.parameter_id(), &parameter_id);
        assert!(address.contains(&effect_id.to_string()));
        assert_eq!(property.descriptor.schema.schema_version, 1);
        assert_eq!(
            property.descriptor.schema.message_id,
            "mondrian.effect.builtin.gaussian_blur.radius.label"
        );
    }

    #[test]
    fn audio_processor_schema_and_exact_curve_round_trip_without_plugin_resolution() {
        let root = unique_temp_dir("audio-parameter-schema-round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("audio-parameter-schema.mdp");

        let mut document = test_document();
        let sequence = document.sequences.active_mut().expect("active sequence");
        let track_id = sequence.audio_tracks[0].id;
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        let mut curve = processor.parameters[&parameter_id].automation.clone();
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, -6.0))
            .expect("gain key");
        processor.set_parameter_automation(curve).expect("schema-compatible curve");
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(processor);

        save_project_archive(&document, &db_path, &project_path).expect("save project");
        let reopened = read_project_document_from_archive(&project_path).expect("reopen project");
        let sequence = reopened.sequences.active().expect("active reopened sequence");
        let parameter =
            &sequence.audio_program.track_channels[&track_id].strip.pre_fader.processors[0]
                .parameters[&parameter_id];

        assert_eq!(parameter.schema.parameter_id, parameter_id);
        assert_eq!(parameter.schema.default_value, PropertyValue::Double(0.0));
        assert_eq!(parameter.automation.keyframes[0].value, -6.0);
    }

    #[test]
    fn project_validation_rejects_audio_processor_values_outside_schema() {
        let mut document = test_document();
        let sequence = document.sequences.active_mut().expect("active sequence");
        let track_id = sequence.audio_tracks[0].id;
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        processor
            .parameters
            .get_mut(&parameter_id)
            .expect("gain parameter")
            .automation
            .default_value = 25.0;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(processor);

        let error = document
            .validate()
            .expect_err("invalid audio parameter must not enter a snapshot");
        assert!(format!("{error:#}").contains("violates its schema"));
    }

    #[test]
    fn project_validation_rejects_deserialized_parameter_state_outside_hard_range() {
        let mut encoded = serde_json::to_value(test_document()).expect("serialize document");
        encoded["sequences"]["sequences"][0]["video_tracks"][0]["opacity"]["static_value"]
            ["Float"] = serde_json::json!(2.0);
        let document: ProjectDocument =
            serde_json::from_value(encoded).expect("structurally decodable document");

        let error = document
            .validate()
            .expect_err("invalid author value must not enter a project snapshot");
        assert!(format!("{error:#}").contains("outside [0, 1]"));
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

    #[test]
    fn document_fingerprint_includes_exact_standard_package_identity() {
        let current = test_document();
        let mut legacy = current.clone();
        legacy.settings.color_management.engine = mondrian_core::ColorEngine::MondrianStandard {
            package: mondrian_core::MondrianStandardPackageIdentity::V2,
        };

        assert_ne!(
            project_document_fingerprint(current).expect("current package fingerprint"),
            project_document_fingerprint(legacy).expect("legacy package fingerprint")
        );
    }

    fn write_current_fixture_archive(path: &Path, library: &[u8]) {
        let file = fs::File::create(path).expect("create fixture archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file(MANIFEST_ENTRY, options).expect("manifest entry");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/manifest.json"))
            .expect("manifest fixture");
        writer.start_file(PROJECT_ENTRY, options).expect("project entry");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/project.json"))
            .expect("project fixture");
        writer.start_file(LIBRARY_ENTRY, options).expect("library entry");
        writer.write_all(library).expect("library fixture");
        writer.finish().expect("finish fixture archive");
    }

    #[test]
    fn current_fixture_open_is_idempotent_and_save_reopen_preserves_semantics() {
        let root = unique_temp_dir("current-fixture");
        let source = root.join("current.mdp");
        write_current_fixture_archive(&source, b"sqlite-current-fixture");

        let first = read_project_document_from_archive(&source).expect("first open");
        let second = read_project_document_from_archive(&source).expect("second open");
        assert_eq!(
            first.settings.color_management.engine,
            mondrian_core::ColorEngine::mondrian_standard()
        );
        let first_context = first
            .sequences
            .active()
            .expect("active current-fixture sequence")
            .settings
            .root_program_color_context(&first.settings.color_management);
        assert_eq!(
            first_context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert_eq!(
            project_document_fingerprint(first.clone()).expect("first fingerprint"),
            project_document_fingerprint(second).expect("second fingerprint")
        );

        let runtime = root.join("runtime");
        let loaded = load_project_archive(&source, &runtime).expect("load fixture");
        assert_eq!(loaded.library_schema_version, 1);
        let resaved = root.join("resaved.mdp");
        save_project_archive(&loaded.document, &runtime.join("index.db"), &resaved)
            .expect("resave fixture");
        let reopened = read_project_document_from_archive(&resaved).expect("reopen saved fixture");
        assert_eq!(
            project_document_fingerprint(loaded.document).expect("loaded fingerprint"),
            project_document_fingerprint(reopened).expect("reopened fingerprint")
        );
    }

    #[test]
    fn legacy_standard_v2_archive_reopens_without_visual_identity_drift() {
        let root = unique_temp_dir("legacy-standard-v2");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite legacy Standard v2").expect("write legacy library");
        let project_path = root.join("legacy-standard-v2.mdp");
        let mut document = test_document();
        let v2_engine = mondrian_core::ColorEngine::MondrianStandard {
            package: mondrian_core::MondrianStandardPackageIdentity::V2,
        };
        document.settings.color_management.engine = v2_engine.clone();
        document
            .sequences
            .active_mut()
            .expect("active legacy sequence")
            .settings
            .color_management
            .engine = v2_engine.clone();

        save_project_archive(&document, &db_path, &project_path)
            .expect("save legacy Standard v2 archive");
        let reopened = read_project_document_from_archive(&project_path)
            .expect("reopen legacy Standard v2 archive");
        assert_eq!(reopened.settings.color_management.engine, v2_engine);
        let context = reopened
            .sequences
            .active()
            .expect("active reopened legacy sequence")
            .settings
            .root_program_color_context(&reopened.settings.color_management);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard_package(
                mondrian_core::MondrianStandardPackageIdentity::V2,
            )
        );
        let (display, view) = context
            .output_transform
            .resolve_display_view(
                mondrian_core::ColorSpace::Rec709,
                &reopened.settings.color_management.engine,
            )
            .expect("resolve legacy Standard v2 output")
            .expect("legacy Standard v2 display/view");
        assert_eq!(display, "Rec.1886 Rec.709 - Display");
        assert_eq!(view, "Mondrian Standard SDR v1");
    }

    #[test]
    fn older_schemas_are_rejected_without_an_alpha_compatibility_migration() {
        let mut legacy = serde_json::to_value(test_document()).expect("serialize document");
        for version in [5, 6, 10] {
            legacy["schema_version"] = serde_json::json!(version);
            let err = DOCUMENT_MIGRATIONS
                .migrate(legacy.clone())
                .expect_err("older schemas must not migrate implicitly");
            assert!(err.to_string().contains("missing project document migration"));
        }
    }

    #[test]
    fn schema_v7_requires_complete_project_color_identity() {
        let value = serde_json::to_value(test_document()).expect("serialize document");

        let mut missing_engine = value.clone();
        missing_engine["settings"]["color_management"]
            .as_object_mut()
            .expect("color-management object")
            .remove("engine");
        assert!(serde_json::from_value::<ProjectDocument>(missing_engine).is_err());

        let mut missing_color_management = value;
        missing_color_management["settings"]
            .as_object_mut()
            .expect("settings object")
            .remove("color_management");
        assert!(serde_json::from_value::<ProjectDocument>(missing_color_management).is_err());

        let mut missing_hdr_view =
            serde_json::to_value(test_document()).expect("serialize document");
        missing_hdr_view["settings"]["color_management"]["engine"]["package"]
            .as_object_mut()
            .expect("Standard package identity")
            .remove("hdr_view_transform_id");
        assert!(serde_json::from_value::<ProjectDocument>(missing_hdr_view).is_err());
    }

    #[test]
    fn current_schema_rejects_removed_aces_sequence_workflow() {
        let mut value = serde_json::to_value(test_document()).expect("serialize document");
        value["sequences"]["sequences"][0]["settings"]["color_management"]["workflow"] =
            serde_json::json!("Aces");

        let error = serde_json::from_value::<ProjectDocument>(value)
            .expect_err("project mode must not be duplicated by an ACES sequence workflow");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn project_open_validation_rejects_missing_sequence_custom_ocio_config() {
        let mut document = test_document();
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-project-custom-ocio-{}.ocio",
            std::process::id()
        ));
        let active = document.sequences.active_mut().expect("active sequence");
        active.settings.color_management.inherit = false;
        active.settings.color_management.engine = missing_custom_engine(missing_path);

        let error = validate_project_color_engines(&document)
            .expect_err("missing sequence Custom OCIO config must fail project open");
        assert!(
            format!("{error:#}").contains("OCIO config file not found"),
            "{error:#}"
        );
    }

    #[test]
    fn document_validation_rejects_custom_ocio_working_space_mismatch() {
        let mut document = test_document();
        document.settings.color_management.engine =
            missing_custom_engine(PathBuf::from("E:/studio/config.ocio"));
        document
            .sequences
            .active_mut()
            .expect("active sequence")
            .settings
            .working_color_space = mondrian_core::WorkingColorSpace::AcesCg;

        let error = document
            .validate()
            .expect_err("document must reject an unpinned Custom OCIO working space");

        assert!(format!("{error:#}").contains("pins working space 'Linear Rec.2020'"));
    }

    #[test]
    fn document_validation_rejects_standard_working_space_mismatch() {
        let mut document = test_document();
        document
            .sequences
            .active_mut()
            .expect("active sequence")
            .settings
            .working_color_space = mondrian_core::WorkingColorSpace::LinearP3D65;

        let error = document
            .validate()
            .expect_err("Standard project must reject a non-versioned working space");

        assert!(format!("{error:#}").contains("Mondrian Standard"));
        assert!(format!("{error:#}").contains("Linear Rec.2020"));
    }

    #[test]
    fn failed_file_replacement_restores_original() {
        let root = unique_temp_dir("replace-restore");
        let target = root.join("project.mdp");
        fs::write(&target, b"original").expect("original");
        let temp = root.join("replacement.tmp");
        fs::write(&temp, b"replacement").expect("replacement");

        assert!(
            replace_file_preserving_original_with(&temp, &target, |_source, _target| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected replacement failure",
                ))
            })
            .is_err()
        );

        assert_eq!(fs::read(&target).expect("restored original"), b"original");
    }

    #[test]
    fn failed_archive_open_does_not_modify_source_or_existing_runtime_library() {
        let root = unique_temp_dir("failed-open-preserves-source");
        let source = root.join("invalid.mdp");
        let file = fs::File::create(&source).expect("archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file(MANIFEST_ENTRY, options).expect("manifest");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/manifest.json"))
            .expect("manifest fixture");
        writer.start_file(PROJECT_ENTRY, options).expect("project");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/project.json"))
            .expect("project fixture");
        writer.finish().expect("finish invalid archive");
        let source_before = fs::read(&source).expect("source before");
        let runtime = root.join("runtime");
        fs::create_dir_all(&runtime).expect("runtime");
        fs::write(runtime.join("index.db"), b"existing-runtime").expect("runtime db");

        assert!(load_project_archive(&source, &runtime).is_err());

        assert_eq!(fs::read(&source).expect("source after"), source_before);
        assert_eq!(
            fs::read(runtime.join("index.db")).expect("runtime after"),
            b"existing-runtime"
        );
    }
}
