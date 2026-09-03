//! Strict external-source admission for commercial endurance fixtures.
//!
//! This validation-only seam compares a machine-plan-bound inventory with the
//! production Export dependency closure. It then retains one read-only native
//! file object per reachable source, using that same object for content hashing
//! and filesystem revision evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use mondrian_assets::AssetKind;
use mondrian_core::{AssetId, MediaFileFingerprint, ProjectId, SequenceId, SequenceRevision};
use mondrian_export::{
    prepare_timeline_export_dependencies_with_audio_selection, ExportAudioProgramSelection,
    ExportPreset, TimelineExportRange,
};
use mondrian_platform::{EndurancePhaseKind, EndurancePhaseRequirement};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_machine_plan::{
    EnduranceMachineExportPlan, EnduranceMachineFileBinding, PreparedCommercialEnduranceMachinePlan,
};
use super::{AppState, PreparedEnduranceProjectFixture};

const SOURCE_INVENTORY_SCHEMA_VERSION: u32 = 1;
const SOURCE_INVENTORY_ID: &str = "mondrian-col047/external-source-inventory/v1";
const MAXIMUM_SOURCE_INVENTORY_BYTES: u64 = 1024 * 1024;
const MAXIMUM_EXPORT_PRESET_BYTES: u64 = 1024 * 1024;
const MAXIMUM_BROADCAST_QC_PROFILE_BYTES: u64 = 1024 * 1024;
const MAXIMUM_PHASE_COUNT: usize = 16;
const MAXIMUM_SEQUENCE_COUNT_PER_PHASE: usize = 1024;
const MAXIMUM_SOURCE_COUNT: usize = 4096;
const MAXIMUM_SOURCE_BYTES: u64 = 1 << 40;
const MAXIMUM_TOTAL_SOURCE_BYTES: u64 = 8 << 40;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSourceInventory {
    schema_version: u32,
    inventory_id: String,
    project_archive_sha256: String,
    project_id: ProjectId,
    document_revision: u64,
    root_sequence_id: SequenceId,
    root_sequence_revision: SequenceRevision,
    phase_closures: Vec<ExternalSourcePhaseClosure>,
    sources: Vec<ExternalSourceBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSourcePhaseClosure {
    phase_id: String,
    kind: EndurancePhaseKind,
    sequence_ids: Vec<SequenceId>,
    media_asset_ids: Vec<AssetId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSourceBinding {
    asset_id: AssetId,
    canonical_path: PathBuf,
    byte_length: u64,
    sha256: String,
}

/// One exact source file retained for an admitted endurance phase.
#[derive(Debug)]
pub struct PreparedEnduranceSource {
    asset_id: AssetId,
    canonical_path: PathBuf,
    byte_length: u64,
    sha256: String,
    source_fingerprint: MediaFileFingerprint,
    _lease: File,
}

impl PreparedEnduranceSource {
    /// Strong Asset identity selected by the production dependency closure.
    pub const fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    /// Canonical direct source path whose native object remains retained.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Exact source length admitted and hashed from the retained object.
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Lowercase SHA-256 of the retained source object's complete bytes.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Complete same-object filesystem revision evidence.
    pub const fn source_fingerprint(&self) -> MediaFileFingerprint {
        self.source_fingerprint
    }
}

/// Canonical source and Sequence closure for one profile phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEndurancePhaseSourceClosure {
    phase_id: String,
    kind: EndurancePhaseKind,
    sequence_ids: BTreeSet<SequenceId>,
    media_asset_ids: BTreeSet<AssetId>,
}

impl PreparedEndurancePhaseSourceClosure {
    /// Exact profile phase identity.
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    /// Product workload family used to derive this closure.
    pub const fn kind(&self) -> EndurancePhaseKind {
        self.kind
    }

    /// Root and nested Sequence identities selected by production lowering.
    pub fn sequence_ids(&self) -> &BTreeSet<SequenceId> {
        &self.sequence_ids
    }

    /// Exact file-backed media identities selected by production lowering.
    pub fn media_asset_ids(&self) -> &BTreeSet<AssetId> {
        &self.media_asset_ids
    }
}

/// Strict inventory receipt plus retained exact source-file objects.
#[derive(Debug)]
pub struct PreparedEnduranceSourceInventory {
    inventory_sha256: String,
    project_id: ProjectId,
    document_revision: u64,
    root_sequence_id: SequenceId,
    root_sequence_revision: SequenceRevision,
    phase_closures: Vec<PreparedEndurancePhaseSourceClosure>,
    export_presets: BTreeMap<String, PreparedEnduranceExportPreset>,
    broadcast_qc_profiles: BTreeMap<String, PreparedEnduranceBroadcastQcProfile>,
    sources: BTreeMap<AssetId, PreparedEnduranceSource>,
    project_fixture: PreparedEnduranceProjectFixture,
    _inventory_lease: File,
}

/// Exact parsed Export preset retained from its machine-plan-bound file object.
#[derive(Debug)]
pub struct PreparedEnduranceExportPreset {
    phase_id: String,
    sha256: String,
    preset: ExportPreset,
    _lease: File,
}

/// Exact parsed Broadcast QC profile retained from its machine-plan-bound object.
#[derive(Debug)]
pub struct PreparedEnduranceBroadcastQcProfile {
    phase_id: String,
    sha256: String,
    profile: mondrian_broadcast::BroadcastQcProfile,
    _lease: File,
}

impl PreparedEnduranceBroadcastQcProfile {
    /// Profile phase whose Export uses this QC contract.
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    /// SHA-256 of the exact retained profile bytes.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Typed QC profile parsed and validated from the retained bytes.
    pub const fn profile(&self) -> &mondrian_broadcast::BroadcastQcProfile {
        &self.profile
    }
}

impl PreparedEnduranceExportPreset {
    /// Profile phase whose Export uses this preset.
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    /// SHA-256 of the exact retained preset bytes.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Typed preset parsed from the same bytes whose digest was admitted.
    pub const fn preset(&self) -> &ExportPreset {
        &self.preset
    }
}

impl PreparedEnduranceSourceInventory {
    /// Load, recompute, and retain the complete external-source inventory.
    ///
    /// The Project receipt must come from
    /// `AppState::open_endurance_project_fixture` and still describe the App's
    /// unchanged current authoring Session. Phase topology comes only from the
    /// profile sealed into the prepared machine plan.
    pub fn prepare(
        app: &AppState,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
        project_fixture: &PreparedEnduranceProjectFixture,
    ) -> Result<Self, EnduranceSourceInventoryError> {
        #[cfg(not(windows))]
        {
            let _ = (app, machine_plan, project_fixture);
            return Err(EnduranceSourceInventoryError::UnsupportedPlatform);
        }

        #[cfg(windows)]
        {
            prepare_windows(app, machine_plan, project_fixture)
        }
    }

    /// Revalidate this prepared authority immediately before phase-owner use.
    ///
    /// Source and preset file objects remain immutable while retained, but the
    /// App Session and Asset Library are independently mutable authorities.
    /// Machine factories must call this at owner admission.
    pub fn validate_current(
        &self,
        app: &AppState,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
    ) -> Result<(), EnduranceSourceInventoryError> {
        #[cfg(not(windows))]
        {
            let _ = (self, app, machine_plan);
            Err(EnduranceSourceInventoryError::UnsupportedPlatform)
        }
        #[cfg(windows)]
        {
            validate_project_receipt(app, machine_plan, &self.project_fixture)
        }
    }

    /// SHA-256 of the exact inventory bytes parsed from the retained object.
    pub fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }

    /// Project identity closed by this inventory.
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    /// Exact Project document revision closed by this inventory.
    pub const fn document_revision(&self) -> u64 {
        self.document_revision
    }

    /// Root Sequence identity closed by every phase.
    pub const fn root_sequence_id(&self) -> SequenceId {
        self.root_sequence_id
    }

    /// Root Sequence revision closed by every phase.
    pub const fn root_sequence_revision(&self) -> SequenceRevision {
        self.root_sequence_revision
    }

    /// Complete ordered profile-phase closure inventory.
    pub fn phase_closures(&self) -> &[PreparedEndurancePhaseSourceClosure] {
        &self.phase_closures
    }

    /// Exact retained source selected for one Asset identity.
    pub fn source(&self, asset_id: AssetId) -> Option<&PreparedEnduranceSource> {
        self.sources.get(&asset_id)
    }

    /// Exact retained Export preset for one Export-owning phase.
    pub fn export_preset(&self, phase_id: &str) -> Option<&PreparedEnduranceExportPreset> {
        self.export_presets.get(phase_id)
    }

    /// Exact retained Broadcast QC profile for one Export-owning phase, when planned.
    pub fn broadcast_qc_profile(
        &self,
        phase_id: &str,
    ) -> Option<&PreparedEnduranceBroadcastQcProfile> {
        self.broadcast_qc_profiles.get(phase_id)
    }

    /// Number of retained source-file objects.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
}

/// Stable failure while closing an endurance fixture over external media.
#[derive(Debug, Error)]
pub enum EnduranceSourceInventoryError {
    /// This platform has no qualified immutable-object source Adapter yet.
    #[error(
        "exact endurance source admission requires a qualified native immutable-object Adapter on this platform"
    )]
    UnsupportedPlatform,
    /// Exact Project receipt no longer describes the live App Session.
    #[error("exact endurance Project receipt is stale or belongs to another App Session: {0}")]
    ProjectReceipt(String),
    /// Inventory or preset binding could not be read as one bounded direct file.
    #[error("endurance {field} binding is invalid: {detail}")]
    InvalidBinding {
        /// Machine-plan field whose binding failed.
        field: &'static str,
        /// Stable lower-level diagnostic.
        detail: String,
    },
    /// Inventory JSON does not match the strict schema.
    #[error("endurance external-source inventory schema is invalid: {0}")]
    InvalidSchema(#[from] serde_json::Error),
    /// Inventory Project identity differs from the exact installed fixture.
    #[error("endurance external-source inventory Project binding mismatch: {0}")]
    ProjectBinding(&'static str),
    /// Inventory phase topology or canonical ordering is invalid.
    #[error("endurance external-source phase closure is invalid: {0}")]
    PhaseClosure(String),
    /// Production dependency lowering failed.
    #[error("endurance phase {phase_id} dependency closure failed: {detail}")]
    DependencyClosure {
        /// Exact profile phase.
        phase_id: String,
        /// Production lowering diagnostic.
        detail: String,
    },
    /// An exact Asset record is missing or cannot authorize execution.
    #[error("endurance source Asset {asset_id} is invalid: {detail}")]
    Asset {
        /// Strong Asset identity.
        asset_id: AssetId,
        /// Exact violated invariant.
        detail: String,
    },
    /// A retained source file disagrees with its inventory or Asset record.
    #[error("endurance source file for Asset {asset_id} is invalid: {detail}")]
    Source {
        /// Strong Asset identity.
        asset_id: AssetId,
        /// Exact violated invariant.
        detail: String,
    },
    /// Checked aggregate source-byte accounting overflowed or exceeded policy.
    #[error("endurance external-source inventory exceeds the aggregate byte budget")]
    AggregateSourceBytes,
}

#[cfg(windows)]
fn prepare_windows(
    app: &AppState,
    machine_plan: &PreparedCommercialEnduranceMachinePlan,
    project_fixture: &PreparedEnduranceProjectFixture,
) -> Result<PreparedEnduranceSourceInventory, EnduranceSourceInventoryError> {
    validate_project_receipt(app, machine_plan, project_fixture)?;
    let binding = &machine_plan.plan().project.external_source_inventory;
    let (mut inventory_lease, bytes, inventory_sha256) =
        read_bound_file(binding, MAXIMUM_SOURCE_INVENTORY_BYTES, "source inventory")?;
    let inventory: ExternalSourceInventory = serde_json::from_slice(&bytes)?;
    validate_inventory_header(&inventory, machine_plan, project_fixture)?;
    let requirements = machine_plan.phase_requirements();
    let DerivedEndurancePhaseArtifacts {
        closures: expected_closures,
        export_presets,
        broadcast_qc_profiles,
    } = derive_phase_closures(app, machine_plan, requirements)?;
    validate_phase_closures(&inventory.phase_closures, &expected_closures, requirements)?;
    let expected_assets = expected_closures
        .iter()
        .flat_map(|closure| closure.media_asset_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    validate_source_shape(&inventory.sources, &expected_assets)?;

    let library = app.asset_library().ok_or_else(|| {
        EnduranceSourceInventoryError::ProjectReceipt("App has no Asset Library".to_owned())
    })?;
    let proxy_assets = app.proxy_mode_assets();
    let mut sources = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for source in inventory.sources {
        if proxy_assets.contains(&source.asset_id) {
            return Err(asset_error(
                source.asset_id,
                "proxy-mode media is not admitted by the exact-source campaign",
            ));
        }
        let record = library
            .get_asset(source.asset_id)
            .map_err(|error| asset_error(source.asset_id, error.to_string()))?
            .ok_or_else(|| asset_error(source.asset_id, "strong Asset record is missing"))?;
        if !matches!(
            record.kind,
            AssetKind::Video | AssetKind::StillImage | AssetKind::Audio
        ) {
            return Err(asset_error(
                source.asset_id,
                "reachable Asset kind is not file-backed picture or audio media",
            ));
        }
        let record_path = record.file_path().ok_or_else(|| {
            asset_error(
                source.asset_id,
                "reachable Asset is not a file-backed source",
            )
        })?;
        if record_path != source.canonical_path {
            return Err(asset_error(
                source.asset_id,
                "Asset Library path differs from the inventory canonical path",
            ));
        }
        let admitted_fingerprint = record.source_fingerprint().ok_or_else(|| {
            asset_error(
                source.asset_id,
                "Asset has no current admitted probe revision",
            )
        })?;
        let retained = prepare_source(source)?;
        if retained.source_fingerprint != admitted_fingerprint {
            return Err(source_error(
                retained.asset_id,
                "same-object revision differs from the Asset Library probe revision",
            ));
        }
        total_bytes = total_bytes
            .checked_add(retained.byte_length)
            .filter(|total| *total <= MAXIMUM_TOTAL_SOURCE_BYTES)
            .ok_or(EnduranceSourceInventoryError::AggregateSourceBytes)?;
        sources.insert(retained.asset_id, retained);
    }
    inventory_lease
        .seek(SeekFrom::Start(0))
        .map_err(|error| invalid_binding("source inventory", error))?;
    validate_project_receipt(app, machine_plan, project_fixture)?;
    Ok(PreparedEnduranceSourceInventory {
        inventory_sha256,
        project_id: inventory.project_id,
        document_revision: inventory.document_revision,
        root_sequence_id: inventory.root_sequence_id,
        root_sequence_revision: inventory.root_sequence_revision,
        phase_closures: expected_closures,
        export_presets,
        broadcast_qc_profiles,
        sources,
        project_fixture: project_fixture.clone(),
        _inventory_lease: inventory_lease,
    })
}

#[cfg(windows)]
fn validate_project_receipt(
    app: &AppState,
    machine_plan: &PreparedCommercialEnduranceMachinePlan,
    receipt: &PreparedEnduranceProjectFixture,
) -> Result<(), EnduranceSourceInventoryError> {
    let session = app.authoring.as_ref().ok_or_else(|| {
        EnduranceSourceInventoryError::ProjectReceipt("App has no authoring Session".to_owned())
    })?;
    let sequence = session.active_sequence().ok_or_else(|| {
        EnduranceSourceInventoryError::ProjectReceipt("App has no active Sequence".to_owned())
    })?;
    let project = &machine_plan.plan().project;
    if receipt.machine_plan_sha256() != machine_plan.sha256()
        || receipt.project_path() != project.project.path
        || session.project_file() != receipt.project_path()
        || receipt.project_archive_sha256() != project.project.sha256
        || receipt.project_id() != session.project_id()
        || receipt.document_revision() != session.document().document_revision
        || receipt.asset_library_revision()
            != session
                .asset_library()
                .database_revision()
                .map_err(|error| EnduranceSourceInventoryError::ProjectReceipt(error.to_string()))?
        || receipt.root_sequence_id() != sequence.id
        || receipt.root_sequence_revision() != sequence.revision
        || receipt.authoring_session_id() != session.session_id()
        || receipt.author_generation() != session.author_generation().get()
        || sequence.id != project.sequence_id
    {
        return Err(EnduranceSourceInventoryError::ProjectReceipt(
            "machine plan, receipt, and live author snapshot differ".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_inventory_header(
    inventory: &ExternalSourceInventory,
    machine_plan: &PreparedCommercialEnduranceMachinePlan,
    receipt: &PreparedEnduranceProjectFixture,
) -> Result<(), EnduranceSourceInventoryError> {
    if inventory.schema_version != SOURCE_INVENTORY_SCHEMA_VERSION {
        return Err(EnduranceSourceInventoryError::ProjectBinding(
            "schema_version",
        ));
    }
    if inventory.inventory_id != SOURCE_INVENTORY_ID {
        return Err(EnduranceSourceInventoryError::ProjectBinding(
            "inventory_id",
        ));
    }
    if inventory.project_archive_sha256 != machine_plan.plan().project.project.sha256 {
        return Err(EnduranceSourceInventoryError::ProjectBinding(
            "project_archive_sha256",
        ));
    }
    if inventory.project_id != receipt.project_id() {
        return Err(EnduranceSourceInventoryError::ProjectBinding("project_id"));
    }
    if inventory.document_revision != receipt.document_revision() {
        return Err(EnduranceSourceInventoryError::ProjectBinding(
            "document_revision",
        ));
    }
    if inventory.root_sequence_id != receipt.root_sequence_id() {
        return Err(EnduranceSourceInventoryError::ProjectBinding(
            "root_sequence_id",
        ));
    }
    if inventory.root_sequence_revision != receipt.root_sequence_revision() {
        return Err(EnduranceSourceInventoryError::ProjectBinding(
            "root_sequence_revision",
        ));
    }
    Ok(())
}

#[cfg(windows)]
struct DerivedEndurancePhaseArtifacts {
    closures: Vec<PreparedEndurancePhaseSourceClosure>,
    export_presets: BTreeMap<String, PreparedEnduranceExportPreset>,
    broadcast_qc_profiles: BTreeMap<String, PreparedEnduranceBroadcastQcProfile>,
}

#[cfg(windows)]
fn derive_phase_closures(
    app: &AppState,
    machine_plan: &PreparedCommercialEnduranceMachinePlan,
    requirements: &[EndurancePhaseRequirement],
) -> Result<DerivedEndurancePhaseArtifacts, EnduranceSourceInventoryError> {
    if requirements.is_empty() || requirements.len() > MAXIMUM_PHASE_COUNT {
        return Err(EnduranceSourceInventoryError::PhaseClosure(
            "profile phase count is zero or exceeds policy".to_owned(),
        ));
    }
    let root_id = machine_plan.plan().project.sequence_id;
    let root = app.sequences().iter().find(|sequence| sequence.id == root_id).ok_or_else(|| {
        EnduranceSourceInventoryError::ProjectReceipt(
            "root Sequence is absent from the live Project".to_owned(),
        )
    })?;
    let mut seen_phase_ids = BTreeSet::new();
    let mut closures = Vec::with_capacity(requirements.len());
    let mut export_presets = BTreeMap::new();
    let mut broadcast_qc_profiles = BTreeMap::new();
    for requirement in requirements {
        if !seen_phase_ids.insert(requirement.phase_id.as_str()) {
            return Err(EnduranceSourceInventoryError::PhaseClosure(format!(
                "duplicate profile phase {}",
                requirement.phase_id
            )));
        }
        let mut closure = match requirement.kind {
            EndurancePhaseKind::PlaybackReference => dependency_sets(
                root,
                app.sequences(),
                TimelineExportRange::EntireSequence,
                ExportAudioProgramSelection::Primary,
                &requirement.phase_id,
            )?,
            EndurancePhaseKind::ContinuousExport => {
                let export = export_plan(machine_plan, requirement)?;
                let preset = load_export_preset(&requirement.phase_id, &export.preset)?;
                let closure = dependency_sets(
                    root,
                    app.sequences(),
                    export.range.timeline_range(),
                    preset.preset.audio_program_selection(),
                    &requirement.phase_id,
                )?;
                export_presets.insert(requirement.phase_id.clone(), preset);
                if let Some(profile) =
                    load_broadcast_qc_profile(&requirement.phase_id, export.broadcast_qc.as_ref())?
                {
                    broadcast_qc_profiles.insert(requirement.phase_id.clone(), profile);
                }
                closure
            }
            EndurancePhaseKind::ConcurrentRecovery => {
                let mut playback = dependency_sets(
                    root,
                    app.sequences(),
                    TimelineExportRange::EntireSequence,
                    ExportAudioProgramSelection::Primary,
                    &requirement.phase_id,
                )?;
                let export = export_plan(machine_plan, requirement)?;
                let preset = load_export_preset(&requirement.phase_id, &export.preset)?;
                let export = dependency_sets(
                    root,
                    app.sequences(),
                    export.range.timeline_range(),
                    preset.preset.audio_program_selection(),
                    &requirement.phase_id,
                )?;
                export_presets.insert(requirement.phase_id.clone(), preset);
                if let Some(profile) = load_broadcast_qc_profile(
                    &requirement.phase_id,
                    export_plan(machine_plan, requirement)?.broadcast_qc.as_ref(),
                )? {
                    broadcast_qc_profiles.insert(requirement.phase_id.clone(), profile);
                }
                playback.0.extend(export.0);
                playback.1.extend(export.1);
                playback
            }
        };
        if closure.1.is_empty() {
            return Err(EnduranceSourceInventoryError::PhaseClosure(format!(
                "phase {} has no reachable file-backed media",
                requirement.phase_id
            )));
        }
        closures.push(PreparedEndurancePhaseSourceClosure {
            phase_id: requirement.phase_id.clone(),
            kind: requirement.kind,
            sequence_ids: std::mem::take(&mut closure.0),
            media_asset_ids: std::mem::take(&mut closure.1),
        });
    }
    Ok(DerivedEndurancePhaseArtifacts { closures, export_presets, broadcast_qc_profiles })
}

#[cfg(windows)]
fn dependency_sets(
    root: &mondrian_timeline::Sequence,
    sequences: &[mondrian_timeline::Sequence],
    range: TimelineExportRange,
    audio_selection: ExportAudioProgramSelection,
    phase_id: &str,
) -> Result<(BTreeSet<SequenceId>, BTreeSet<AssetId>), EnduranceSourceInventoryError> {
    let dependencies = prepare_timeline_export_dependencies_with_audio_selection(
        root,
        sequences,
        range,
        audio_selection,
    )
    .map_err(|error| EnduranceSourceInventoryError::DependencyClosure {
        phase_id: phase_id.to_owned(),
        detail: error.to_string(),
    })?;
    Ok((
        dependencies.sequence_ids().clone(),
        dependencies.media_components().keys().copied().collect(),
    ))
}

#[cfg(windows)]
fn export_plan<'a>(
    machine_plan: &'a PreparedCommercialEnduranceMachinePlan,
    requirement: &EndurancePhaseRequirement,
) -> Result<&'a EnduranceMachineExportPlan, EnduranceSourceInventoryError> {
    machine_plan
        .plan()
        .exports
        .iter()
        .find(|export| export.phase_id == requirement.phase_id)
        .ok_or_else(|| {
            EnduranceSourceInventoryError::PhaseClosure(format!(
                "phase {} has no exact machine Export plan",
                requirement.phase_id
            ))
        })
}

#[cfg(windows)]
fn load_export_preset(
    phase_id: &str,
    binding: &EnduranceMachineFileBinding,
) -> Result<PreparedEnduranceExportPreset, EnduranceSourceInventoryError> {
    let (mut lease, bytes, sha256) =
        read_bound_file(binding, MAXIMUM_EXPORT_PRESET_BYTES, "Export preset")?;
    validate_export_preset_fields(&bytes)?;
    let preset =
        serde_json::from_slice(&bytes).map_err(EnduranceSourceInventoryError::InvalidSchema)?;
    lease
        .seek(SeekFrom::Start(0))
        .map_err(|error| invalid_binding("Export preset", error))?;
    Ok(PreparedEnduranceExportPreset {
        phase_id: phase_id.to_owned(),
        sha256,
        preset,
        _lease: lease,
    })
}

#[cfg(windows)]
fn load_broadcast_qc_profile(
    phase_id: &str,
    binding: Option<&EnduranceMachineFileBinding>,
) -> Result<Option<PreparedEnduranceBroadcastQcProfile>, EnduranceSourceInventoryError> {
    let Some(binding) = binding else {
        return Ok(None);
    };
    let (mut lease, bytes, sha256) = read_bound_file(
        binding,
        MAXIMUM_BROADCAST_QC_PROFILE_BYTES,
        "Broadcast QC profile",
    )?;
    let profile: mondrian_broadcast::BroadcastQcProfile = serde_json::from_slice(&bytes)?;
    profile
        .validate()
        .map_err(|error| EnduranceSourceInventoryError::InvalidBinding {
            field: "Broadcast QC profile",
            detail: error.to_string(),
        })?;
    lease
        .seek(SeekFrom::Start(0))
        .map_err(|error| invalid_binding("Broadcast QC profile", error))?;
    Ok(Some(PreparedEnduranceBroadcastQcProfile {
        phase_id: phase_id.to_owned(),
        sha256,
        profile,
        _lease: lease,
    }))
}

#[cfg(windows)]
fn validate_export_preset_fields(bytes: &[u8]) -> Result<(), EnduranceSourceInventoryError> {
    const FIELDS: &[&str] = &[
        "name",
        "artifact",
        "resolution",
        "frame_rate",
        "frame_sampling",
        "video_signal",
        "alpha_mode",
        "color_target",
        "legalizer",
    ];
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let object = value.as_object().ok_or_else(|| {
        EnduranceSourceInventoryError::PhaseClosure(
            "Export preset root must be an object".to_owned(),
        )
    })?;
    if let Some(field) = object.keys().find(|field| !FIELDS.contains(&field.as_str())) {
        return Err(EnduranceSourceInventoryError::PhaseClosure(format!(
            "Export preset contains unknown field {field}"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_phase_closures(
    actual: &[ExternalSourcePhaseClosure],
    expected: &[PreparedEndurancePhaseSourceClosure],
    requirements: &[EndurancePhaseRequirement],
) -> Result<(), EnduranceSourceInventoryError> {
    if actual.len() != requirements.len() || expected.len() != requirements.len() {
        return Err(EnduranceSourceInventoryError::PhaseClosure(
            "phase count differs from the complete profile".to_owned(),
        ));
    }
    for ((actual, expected), requirement) in actual.iter().zip(expected).zip(requirements.iter()) {
        if actual.sequence_ids.is_empty()
            || actual.sequence_ids.len() > MAXIMUM_SEQUENCE_COUNT_PER_PHASE
        {
            return Err(EnduranceSourceInventoryError::PhaseClosure(format!(
                "phase {} Sequence count is zero or exceeds policy",
                requirement.phase_id
            )));
        }
        require_strictly_sorted(&actual.sequence_ids, "sequence_ids")?;
        require_strictly_sorted(&actual.media_asset_ids, "media_asset_ids")?;
        if actual.phase_id != requirement.phase_id
            || actual.kind != requirement.kind
            || actual.phase_id != expected.phase_id
            || actual.kind != expected.kind
            || actual.sequence_ids.iter().copied().collect::<BTreeSet<_>>() != expected.sequence_ids
            || actual.media_asset_ids.iter().copied().collect::<BTreeSet<_>>()
                != expected.media_asset_ids
        {
            return Err(EnduranceSourceInventoryError::PhaseClosure(format!(
                "phase {} differs from production dependency lowering",
                requirement.phase_id
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn validate_source_shape(
    sources: &[ExternalSourceBinding],
    expected_assets: &BTreeSet<AssetId>,
) -> Result<(), EnduranceSourceInventoryError> {
    if sources.is_empty() || sources.len() > MAXIMUM_SOURCE_COUNT {
        return Err(EnduranceSourceInventoryError::PhaseClosure(
            "source count is zero or exceeds policy".to_owned(),
        ));
    }
    let ids = sources.iter().map(|source| source.asset_id).collect::<Vec<_>>();
    require_strictly_sorted(&ids, "sources.asset_id")?;
    if ids.iter().copied().collect::<BTreeSet<_>>() != *expected_assets {
        return Err(EnduranceSourceInventoryError::PhaseClosure(
            "sources are not the exact union of phase media identities".to_owned(),
        ));
    }
    let mut paths = BTreeSet::new();
    let mut total_bytes = 0_u64;
    for source in sources {
        validate_lower_sha256(&source.sha256).map_err(|detail| {
            source_error(source.asset_id, format!("invalid SHA-256: {detail}"))
        })?;
        if source.byte_length == 0 || source.byte_length > MAXIMUM_SOURCE_BYTES {
            return Err(source_error(source.asset_id, "byte length exceeds policy"));
        }
        total_bytes = total_bytes
            .checked_add(source.byte_length)
            .filter(|total| *total <= MAXIMUM_TOTAL_SOURCE_BYTES)
            .ok_or(EnduranceSourceInventoryError::AggregateSourceBytes)?;
        validate_canonical_path_shape(&source.canonical_path)
            .map_err(|detail| source_error(source.asset_id, detail))?;
        if !paths.insert(source.canonical_path.clone()) {
            return Err(source_error(
                source.asset_id,
                "canonical path is assigned to more than one Asset",
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_source(
    source: ExternalSourceBinding,
) -> Result<PreparedEnduranceSource, EnduranceSourceInventoryError> {
    let link = fs::symlink_metadata(&source.canonical_path)
        .map_err(|error| source_error(source.asset_id, error.to_string()))?;
    if link.file_type().is_symlink() || !link.file_type().is_file() {
        return Err(source_error(
            source.asset_id,
            "path is not a direct regular file",
        ));
    }
    let canonical = mondrian_assets::canonical_native_path(&source.canonical_path)
        .map_err(|error| source_error(source.asset_id, error.to_string()))?;
    if canonical != source.canonical_path {
        return Err(source_error(source.asset_id, "path is not canonical"));
    }
    let mut lease = open_exact_read_handle(&canonical)
        .map_err(|error| source_error(source.asset_id, error.to_string()))?;
    let metadata = lease
        .metadata()
        .map_err(|error| source_error(source.asset_id, error.to_string()))?;
    if !metadata.is_file() || metadata.len() != source.byte_length {
        return Err(source_error(
            source.asset_id,
            "opened object length differs from inventory",
        ));
    }
    let fingerprint = MediaFileFingerprint::capture_open_file(&lease);
    if !fingerprint.authorizes_reuse() || fingerprint.len != Some(source.byte_length) {
        return Err(source_error(
            source.asset_id,
            "opened object has incomplete revision evidence",
        ));
    }
    let sha256 = hash_open_file(&mut lease, source.byte_length)
        .map_err(|error| source_error(source.asset_id, error.to_string()))?;
    if sha256 != source.sha256 {
        return Err(source_error(
            source.asset_id,
            "content SHA-256 differs from inventory",
        ));
    }
    lease
        .seek(SeekFrom::Start(0))
        .map_err(|error| source_error(source.asset_id, error.to_string()))?;
    Ok(PreparedEnduranceSource {
        asset_id: source.asset_id,
        canonical_path: source.canonical_path,
        byte_length: source.byte_length,
        sha256,
        source_fingerprint: fingerprint,
        _lease: lease,
    })
}

#[cfg(windows)]
fn read_bound_file(
    binding: &EnduranceMachineFileBinding,
    maximum_bytes: u64,
    field: &'static str,
) -> Result<(File, Vec<u8>, String), EnduranceSourceInventoryError> {
    validate_lower_sha256(&binding.sha256).map_err(|detail| invalid_binding(field, detail))?;
    validate_canonical_path_shape(&binding.path)
        .map_err(|detail| invalid_binding(field, detail))?;
    let link =
        fs::symlink_metadata(&binding.path).map_err(|error| invalid_binding(field, error))?;
    if link.file_type().is_symlink() || !link.file_type().is_file() {
        return Err(invalid_binding(field, "path is not a direct regular file"));
    }
    let canonical = mondrian_assets::canonical_native_path(&binding.path)
        .map_err(|error| invalid_binding(field, error))?;
    if canonical != binding.path {
        return Err(invalid_binding(field, "path is not canonical"));
    }
    let mut file =
        open_exact_read_handle(&canonical).map_err(|error| invalid_binding(field, error))?;
    let metadata = file.metadata().map_err(|error| invalid_binding(field, error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(invalid_binding(
            field,
            "opened object length exceeds policy",
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| invalid_binding(field, "opened object length does not fit memory"))?;
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| invalid_binding(field, error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len() {
        return Err(invalid_binding(
            field,
            "opened object changed during bounded read",
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if sha256 != binding.sha256 {
        return Err(invalid_binding(
            field,
            "content SHA-256 differs from machine plan",
        ));
    }
    Ok((file, bytes, sha256))
}

#[cfg(windows)]
fn open_exact_read_handle(path: &Path) -> std::io::Result<File> {
    super::project_runtime::open_direct_read_file(path, "endurance input")
        .map_err(std::io::Error::other)
}

#[cfg(windows)]
fn hash_open_file(file: &mut File, expected_bytes: u64) -> std::io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut observed = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("source byte count overflowed"))?;
        if observed > expected_bytes {
            return Err(std::io::Error::other("source grew during bounded hash"));
        }
        digest.update(&buffer[..read]);
    }
    if observed != expected_bytes {
        return Err(std::io::Error::other(
            "source length changed during bounded hash",
        ));
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(windows)]
fn require_strictly_sorted<T: Ord>(
    values: &[T],
    field: &'static str,
) -> Result<(), EnduranceSourceInventoryError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(EnduranceSourceInventoryError::PhaseClosure(format!(
            "{field} must be strictly sorted and unique"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_canonical_path_shape(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("path is not absolute and normalized".to_owned());
    }
    let text = path.to_str().ok_or_else(|| "path is not lossless UTF-8".to_owned())?;
    if text.len() > 4096 || text.contains('\0') {
        return Err("path text exceeds policy".to_owned());
    }
    for component in path.components() {
        if let Component::Normal(name) = component {
            if !super::endurance_run_request::is_portable_ordinary_path_component(name) {
                return Err("path contains a non-portable ordinary component".to_owned());
            }
            let name =
                name.to_str().ok_or_else(|| "path component is not lossless UTF-8".to_owned())?;
            if name.is_empty()
                || name.ends_with(['.', ' '])
                || name.contains(':')
                || matches!(name, "." | "..")
            {
                return Err("path contains a non-ordinary component".to_owned());
            }
        }
    }
    Ok(())
}

fn validate_lower_sha256(value: &str) -> Result<(), &'static str> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("digest is not 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn invalid_binding(
    field: &'static str,
    detail: impl std::fmt::Display,
) -> EnduranceSourceInventoryError {
    EnduranceSourceInventoryError::InvalidBinding { field, detail: detail.to_string() }
}

fn asset_error(asset_id: AssetId, detail: impl Into<String>) -> EnduranceSourceInventoryError {
    EnduranceSourceInventoryError::Asset { asset_id, detail: detail.into() }
}

fn source_error(asset_id: AssetId, detail: impl Into<String>) -> EnduranceSourceInventoryError {
    EnduranceSourceInventoryError::Source { asset_id, detail: detail.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::{AssetLibrary, AssetMediaProbeCandidate};
    use mondrian_broadcast::{
        BroadcastQcObservationTap, BroadcastQcProfile, BroadcastQcRule, BroadcastQcSeverity,
        QcActivePicture,
    };
    use mondrian_core::ColorSpace;
    use mondrian_project::{save_project_archive, ProjectDocument};
    use mondrian_timeline::{Clip, Sequence, SequenceCollection, SequenceSettings};
    use std::fs::OpenOptions;
    use std::sync::Arc;

    #[cfg(windows)]
    struct ExactFixture {
        _root: tempfile::TempDir,
        plan: PreparedCommercialEnduranceMachinePlan,
        app: AppState,
        receipt: PreparedEnduranceProjectFixture,
        source_path: PathBuf,
        broadcast_qc_path: PathBuf,
        source_id: AssetId,
    }

    fn profile() -> mondrian_platform::EnduranceQualificationProfile {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        serde_json::from_slice(
            &fs::read(root.join("tests/validation/commercial-endurance-qualification.json"))
                .expect("read profile"),
        )
        .expect("parse profile")
    }

    #[cfg(windows)]
    fn exact_fixture(retire_source: bool, proxy_source: bool) -> ExactFixture {
        const VIDEO: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");

        let root = tempfile::tempdir().expect("fixture root");
        let source_path = root.path().join("source.mp4");
        fs::write(&source_path, VIDEO).expect("write source fixture");
        let source_path =
            mondrian_assets::canonical_native_path(&source_path).expect("canonical source path");
        let library = AssetLibrary::open(root.path().join("fixture-library"))
            .expect("open fixture Asset Library");
        let media_info = mondrian_media::probe_media_info(&source_path).expect("probe fixture");
        let fingerprint = MediaFileFingerprint::capture(&source_path);
        let source_id = library
            .commit_media_probe(
                AssetMediaProbeCandidate::new(source_path.clone(), fingerprint, media_info)
                    .expect("prepare fixture probe"),
                None,
            )
            .expect("commit fixture Asset");
        if retire_source {
            assert_eq!(
                library.retire_assets(&[source_id]).expect("retire fixture Asset"),
                1
            );
        }

        let mut sequence = Sequence::new("Exact source inventory");
        let duration = crate::app::tt(20, sequence.time_base());
        sequence.video_tracks[0]
            .add_clip(
                Clip::new(source_id, mondrian_core::TimelineTime::ZERO, duration).expect("clip"),
            )
            .expect("place source clip");
        let sequence_id = sequence.id;
        let sequence_revision = sequence.revision;
        let mut document = ProjectDocument::new(
            "Exact source inventory",
            SequenceCollection::new(sequence),
            mondrian_core::ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            mondrian_core::ProjectSettings::default(),
        );
        if proxy_source {
            document.proxy_mode_assets.insert(source_id);
        }
        let project_id = document.project_id;
        let document_revision = document.document_revision;
        let project_path = root.path().join("project.mdp");
        save_project_archive(&document, &library.database_path(), &project_path)
            .expect("save exact Project archive");
        let project_path =
            mondrian_assets::canonical_native_path(&project_path).expect("canonical Project path");
        let project_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(&project_path).expect("read Project bytes"))
        );

        let profile = profile();
        let preset = ExportPreset::h264_aac_sdr_1080p();
        let preset_bytes = serde_json::to_vec_pretty(&preset).expect("serialize Export preset");
        let broadcast_qc = BroadcastQcProfile {
            id: "fixture-qc".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [7; 32],
            signal_color_space: ColorSpace::Rec709,
            observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: QcActivePicture::full(1_920, 1_080),
            rules: vec![BroadcastQcRule::SignalExcursion {
                rule_id: "legal-range".to_owned(),
                tolerance_per_mille: 0,
                maximum_coverage_ppm: 0,
                severity: BroadcastQcSeverity::Fail,
            }],
            maximum_retained_findings: 32,
            require_regulatory_flash_analysis: false,
            require_encoded_artifact_revalidation: true,
        };
        let broadcast_qc_bytes =
            serde_json::to_vec_pretty(&broadcast_qc).expect("serialize Broadcast QC profile");
        for requirement in profile.phases.iter().filter(|requirement| {
            matches!(
                requirement.kind,
                EndurancePhaseKind::ContinuousExport | EndurancePhaseKind::ConcurrentRecovery
            )
        }) {
            fs::write(
                root.path().join(format!("{}-preset.json", requirement.phase_id)),
                &preset_bytes,
            )
            .expect("write exact Export preset");
            fs::write(
                root.path().join(format!("{}-qc.json", requirement.phase_id)),
                &broadcast_qc_bytes,
            )
            .expect("write exact Broadcast QC profile");
            fs::create_dir(root.path().join(format!("{}-output", requirement.phase_id)))
                .expect("create Export output root");
        }

        let source_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(&source_path).expect("read source bytes"))
        );
        let phase_closures = profile
            .phases
            .iter()
            .map(|requirement| {
                serde_json::json!({
                    "phase_id": requirement.phase_id,
                    "kind": requirement.kind,
                    "sequence_ids": [sequence_id],
                    "media_asset_ids": [source_id]
                })
            })
            .collect::<Vec<_>>();
        let inventory_path = root.path().join("external-sources.json");
        fs::write(
            &inventory_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": SOURCE_INVENTORY_SCHEMA_VERSION,
                "inventory_id": SOURCE_INVENTORY_ID,
                "project_archive_sha256": project_sha256,
                "project_id": project_id,
                "document_revision": document_revision,
                "root_sequence_id": sequence_id,
                "root_sequence_revision": sequence_revision,
                "phase_closures": phase_closures,
                "sources": [{
                    "asset_id": source_id,
                    "canonical_path": source_path,
                    "byte_length": VIDEO.len(),
                    "sha256": source_sha256
                }]
            }))
            .expect("serialize source inventory"),
        )
        .expect("write source inventory");
        let inventory_path = mondrian_assets::canonical_native_path(&inventory_path)
            .expect("canonical inventory path");
        let inventory_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(&inventory_path).expect("read inventory bytes"))
        );

        let plan_path = root.path().join("machine-plan.json");
        crate::app::endurance_machine_plan::write_test_machine_plan(
            &plan_path,
            root.path(),
            &profile,
            24,
        );
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&plan_path).expect("read machine plan"))
                .expect("parse machine plan");
        value["project"]["project"]["path"] = serde_json::json!(project_path);
        value["project"]["project"]["sha256"] = serde_json::json!(project_sha256);
        value["project"]["sequence_id"] = serde_json::json!(sequence_id);
        value["project"]["external_source_inventory"]["path"] = serde_json::json!(inventory_path);
        value["project"]["external_source_inventory"]["sha256"] =
            serde_json::json!(inventory_sha256);
        for export in value["exports"].as_array_mut().expect("Export plans") {
            export["sequence_id"] = serde_json::json!(sequence_id);
            let phase_id = export["phase_id"].as_str().expect("phase id").to_owned();
            let preset_path = mondrian_assets::canonical_native_path(
                &root.path().join(format!("{phase_id}-preset.json")),
            )
            .expect("canonical preset path");
            export["preset"]["path"] = serde_json::json!(preset_path);
            export["preset"]["sha256"] = serde_json::json!(format!(
                "{:x}",
                Sha256::digest(fs::read(&preset_path).expect("read preset bytes"))
            ));
            let broadcast_qc_path = mondrian_assets::canonical_native_path(
                &root.path().join(format!("{phase_id}-qc.json")),
            )
            .expect("canonical Broadcast QC path");
            export["broadcast_qc"] = serde_json::json!({
                "path": broadcast_qc_path,
                "sha256": format!(
                    "{:x}",
                    Sha256::digest(
                        fs::read(&broadcast_qc_path).expect("read Broadcast QC bytes")
                    )
                )
            });
        }
        fs::write(
            &plan_path,
            serde_json::to_vec_pretty(&value).expect("serialize machine plan"),
        )
        .expect("write machine plan");
        let plan = PreparedCommercialEnduranceMachinePlan::load(&plan_path, &profile, 24)
            .expect("prepare machine plan");
        let mut app = AppState::new();
        let receipt =
            app.open_endurance_project_fixture(&plan).expect("open exact Project fixture");
        let broadcast_qc_path = mondrian_assets::canonical_native_path(
            &root.path().join("02-continuous-export-24h-qc.json"),
        )
        .expect("canonical retained Broadcast QC path");

        ExactFixture {
            _root: root,
            plan,
            app,
            receipt,
            source_path,
            broadcast_qc_path,
            source_id,
        }
    }

    #[cfg(windows)]
    #[test]
    fn exact_inventory_closes_production_dependencies_and_retains_source_object() {
        let mut fixture = exact_fixture(false, false);
        let prepared = PreparedEnduranceSourceInventory::prepare(
            &fixture.app,
            &fixture.plan,
            &fixture.receipt,
        )
        .expect("prepare exact source inventory");
        assert_eq!(prepared.project_id(), fixture.receipt.project_id());
        assert_eq!(prepared.source_count(), 1);
        assert_eq!(prepared.phase_closures().len(), 3);
        assert!(prepared.source(fixture.source_id).is_some());
        assert_eq!(
            prepared
                .export_preset("02-continuous-export-24h")
                .expect("prepared Continuous Export preset")
                .preset()
                .audio_program_selection(),
            ExportAudioProgramSelection::Primary
        );
        assert_eq!(
            prepared
                .broadcast_qc_profile("02-continuous-export-24h")
                .expect("prepared Continuous Export QC profile")
                .profile(),
            &BroadcastQcProfile {
                id: "fixture-qc".to_owned(),
                edition: "1".to_owned(),
                source_sha256: [7; 32],
                signal_color_space: ColorSpace::Rec709,
                observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
                active_picture: QcActivePicture::full(1_920, 1_080),
                rules: vec![BroadcastQcRule::SignalExcursion {
                    rule_id: "legal-range".to_owned(),
                    tolerance_per_mille: 0,
                    maximum_coverage_ppm: 0,
                    severity: BroadcastQcSeverity::Fail,
                }],
                maximum_retained_findings: 32,
                require_regulatory_flash_analysis: false,
                require_encoded_artifact_revalidation: true,
            }
        );

        assert!(OpenOptions::new().write(true).open(&fixture.source_path).is_err());
        assert!(fs::remove_file(&fixture.source_path).is_err());
        assert!(OpenOptions::new().write(true).open(&fixture.broadcast_qc_path).is_err());

        fixture
            .app
            .commit_active_sequence_edit("invalidate exact source receipt", |sequence| {
                sequence.name.push_str(" changed");
                Ok(())
            })
            .expect("commit author edit");
        assert!(matches!(
            PreparedEnduranceSourceInventory::prepare(
                &fixture.app,
                &fixture.plan,
                &fixture.receipt,
            ),
            Err(EnduranceSourceInventoryError::ProjectReceipt(_))
        ));

        drop(prepared);
        OpenOptions::new()
            .write(true)
            .open(&fixture.source_path)
            .expect("source write access returns after lease drop");
        let plan = fixture.plan.clone();
        drop(fixture.app);

        let requirement = profile()
            .phases
            .into_iter()
            .find(|phase| phase.kind == EndurancePhaseKind::ContinuousExport)
            .expect("Continuous Export requirement");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let workload = super::super::endurance_workload::PreparedEnduranceWorkload::load(
            &requirement,
            &workspace.join("tests/validation/endurance-workloads/continuous-export-v1.json"),
        )
        .expect("prepare Continuous Export workload");
        let mut factory =
            super::super::endurance_machine_factory::ContinuousExportEnduranceMachineFactory::test(
                &plan,
            );
        let inventory = super::super::endurance_product_runtime::FreshEndurancePhaseFactory::pre_start_capability_inventory(
            &mut factory,
            &plan,
            &requirement,
            &workload,
        )
        .expect("pre-start exact Continuous Export factory");
        let token = workload.prepare_start(&inventory).expect("admit Continuous Export factory");
        let build =
            super::super::endurance_product_runtime::FreshEndurancePhaseFactory::build_phase(
                &mut factory,
                Arc::new(plan),
                &requirement,
                &workload,
                token,
            );
        assert!(matches!(
            build,
            super::super::endurance_product_runtime::FreshEndurancePhaseBuild::Ready(_)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn reachable_retired_source_is_resolved_but_proxy_mode_is_rejected() {
        let retired = exact_fixture(true, false);
        let prepared = PreparedEnduranceSourceInventory::prepare(
            &retired.app,
            &retired.plan,
            &retired.receipt,
        )
        .expect("reachable retired Asset remains a strong exact source");
        assert!(prepared.source(retired.source_id).is_some());
        drop(prepared);
        drop(retired.app);

        let proxy = exact_fixture(false, true);
        assert!(matches!(
            PreparedEnduranceSourceInventory::prepare(
                &proxy.app,
                &proxy.plan,
                &proxy.receipt,
            ),
            Err(EnduranceSourceInventoryError::Asset { asset_id, .. })
                if asset_id == proxy.source_id
        ));
        drop(proxy.app);

        let library_drift = exact_fixture(false, false);
        library_drift
            .app
            .asset_library()
            .expect("exact Asset Library")
            .retire_assets(&[library_drift.source_id])
            .expect("mutate exact Library after receipt");
        assert!(matches!(
            PreparedEnduranceSourceInventory::prepare(
                &library_drift.app,
                &library_drift.plan,
                &library_drift.receipt,
            ),
            Err(EnduranceSourceInventoryError::ProjectReceipt(_))
        ));
        drop(library_drift.app);

        let mut path_drift = exact_fixture(false, false);
        let session = path_drift.app.authoring.as_mut().expect("exact Session");
        let generation = session.author_generation();
        let revision = session.document().document_revision;
        let library_revision =
            session.asset_library().database_revision().expect("Library revision");
        let meta = session.document().meta.clone();
        session
            .mark_saved(
                generation,
                revision,
                library_revision,
                meta,
                Some(path_drift.source_path.with_file_name("save-as.mdp")),
            )
            .expect("simulate completed Save As");
        assert!(matches!(
            PreparedEnduranceSourceInventory::prepare(
                &path_drift.app,
                &path_drift.plan,
                &path_drift.receipt,
            ),
            Err(EnduranceSourceInventoryError::ProjectReceipt(_))
        ));
        drop(path_drift.app);

        let changed = exact_fixture(false, false);
        let mut bytes = fs::read(&changed.source_path).expect("read mutable source");
        bytes[0] ^= 0xff;
        fs::write(&changed.source_path, bytes).expect("replace source bytes at the same length");
        assert!(matches!(
            PreparedEnduranceSourceInventory::prepare(
                &changed.app,
                &changed.plan,
                &changed.receipt,
            ),
            Err(EnduranceSourceInventoryError::Source { asset_id, .. })
                if asset_id == changed.source_id
        ));
        drop(changed.app);
    }

    #[test]
    fn strict_schema_and_portable_path_rules_reject_ambiguous_inputs() {
        let duplicate = br#"{
            "schema_version":1,
            "schema_version":1,
            "inventory_id":"mondrian-col047/external-source-inventory/v1",
            "project_archive_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "project_id":"00000000-0000-0000-0000-000000000001",
            "document_revision":1,
            "root_sequence_id":"00000000-0000-0000-0000-000000000002",
            "root_sequence_revision":1,
            "phase_closures":[],
            "sources":[]
        }"#;
        assert!(serde_json::from_slice::<ExternalSourceInventory>(duplicate).is_err());

        let unknown = br#"{
            "schema_version":1,
            "inventory_id":"mondrian-col047/external-source-inventory/v1",
            "project_archive_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "project_id":"00000000-0000-0000-0000-000000000001",
            "document_revision":1,
            "root_sequence_id":"00000000-0000-0000-0000-000000000002",
            "root_sequence_revision":1,
            "phase_closures":[],
            "sources":[],
            "extra":true
        }"#;
        assert!(serde_json::from_slice::<ExternalSourceInventory>(unknown).is_err());

        #[cfg(windows)]
        {
            assert!(validate_canonical_path_shape(Path::new(r"C:\media\NUL.mov")).is_err());
            assert!(validate_canonical_path_shape(Path::new(r"C:\media\clip.mov.")).is_err());
            assert!(validate_canonical_path_shape(Path::new(r"C:\media\clip.mov:stream")).is_err());
            assert!(validate_export_preset_fields(
                br#"{"name":"x","artifact":{"kind":"image_sequence","format":"png8"},"resolution":null,"unknown":true}"#
            )
            .is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn inventory_shape_rejects_noncanonical_source_union() {
        let first = AssetId::new();
        let second = AssetId::new();
        let expected = [first].into_iter().collect::<BTreeSet<_>>();
        let mut ids = [first, second];
        ids.sort();
        let sources = ids
            .into_iter()
            .map(|asset_id| ExternalSourceBinding {
                asset_id,
                canonical_path: PathBuf::from(format!(r"C:\media\{asset_id}.mov")),
                byte_length: 1,
                sha256: "a".repeat(64),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_source_shape(&sources, &expected),
            Err(EnduranceSourceInventoryError::PhaseClosure(_))
        ));

        let profile = profile();
        let sequence_id = SequenceId::new();
        let asset_id = AssetId::new();
        let expected = PreparedEndurancePhaseSourceClosure {
            phase_id: profile.phases[0].phase_id.clone(),
            kind: profile.phases[0].kind,
            sequence_ids: [sequence_id].into_iter().collect(),
            media_asset_ids: [asset_id].into_iter().collect(),
        };
        let wrong_phase = ExternalSourcePhaseClosure {
            phase_id: profile.phases[1].phase_id.clone(),
            kind: profile.phases[0].kind,
            sequence_ids: vec![sequence_id],
            media_asset_ids: vec![asset_id],
        };
        assert!(matches!(
            validate_phase_closures(&[wrong_phase], &[expected], &profile.phases[..1]),
            Err(EnduranceSourceInventoryError::PhaseClosure(_))
        ));
    }
}
