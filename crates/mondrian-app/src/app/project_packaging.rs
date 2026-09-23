//! Inventory of every file dependency needed by a portable Project package.
//!
//! This is a read-only preflight over the complete author snapshot and Asset
//! Library. A later packager must revalidate each file object while copying;
//! this inventory alone is not publication or content-identity evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use mondrian_assets::AssetLibrary;
use mondrian_core::automation::{
    ParameterResourceReference, PropertyBag, PropertyHost, PropertyValue,
};
use mondrian_core::grade_graph::GradeGraphNodeKind;
use mondrian_core::{AssetId, AssetSource, ColorEngine, OcioConfigSource};
use mondrian_project::ProjectDocument;
use mondrian_timeline::clip::Clip;

use super::AppState;

/// One canonical regular file and all author references requiring its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDependencyFile {
    /// Canonical source path at inventory time; copy must revalidate it.
    pub path: PathBuf,
    /// File length at inventory time, useful for a package size preview.
    pub size_bytes: u64,
    /// Stable author identities referring to this file.
    pub owners: Vec<String>,
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
    files: BTreeMap<PathBuf, (u64, BTreeSet<String>)>,
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
        let entry =
            self.files.entry(canonical).or_insert_with(|| (metadata.len(), BTreeSet::new()));
        entry.1.insert(owner);
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
                .map(|(path, (size_bytes, owners))| PortableDependencyFile {
                    path,
                    size_bytes,
                    owners: owners.into_iter().collect(),
                })
                .collect(),
            issues: self.issues,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::PropertyMutation;
    use mondrian_core::{
        GradeDefinition, GradeGraph, GradeGraphNode, GradeGraphNodeId, GradeVersion,
        ProjectColorEnvironment, ProjectSettings, TimelineTime,
    };
    use mondrian_effects::{EffectNodeExt, EffectType};
    use mondrian_timeline::{Clip, Sequence, SequenceCollection};

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
        effect
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
        fs::write(&lut, b"TITLE \"look\"\nLUT_3D_SIZE 2\n").expect("LUT fixture");
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
}
