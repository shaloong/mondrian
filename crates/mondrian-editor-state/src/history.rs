//! Bounded project-wide Undo/Redo history for committed authoring transactions.
//!
//! Common single-Sequence edits retain only that Sequence. Structural edits that
//! alter the Sequence collection or Project-owned state retain a scoped, typed
//! Project restore point. Both forms cross the same project-document Interface.

use mondrian_core::authoring::{
    AuthoringAllocationFootprint, AuthoringFootprintDescriptorCache,
    AuthoringFootprintDescriptorCollector,
};
use mondrian_core::{
    AssetId, AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintVersion,
    AuthoringList, AuthoringSet, AuthoringSnapshot, MondrianError, ProjectColorEnvironment,
    ProjectGallery, ProjectId, ProjectMeta, ProjectSettings, Result, SequenceId,
    AUTHORING_FOOTPRINT_VERSION,
};
use mondrian_project::ProjectDocument;
use mondrian_timeline::{Sequence, SequenceCollection, SequenceSettings};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::mem::size_of;
use std::sync::Arc;

/// Default maximum number of retained authoring transactions.
pub const DEFAULT_AUTHORING_HISTORY_MAX_ENTRIES: usize = 200;

/// Default retained logical-payload budget for Undo and Redo combined.
///
/// The 256 MiB ceiling remains a small, deterministic fraction of the
/// supported 8 GiB host class while allowing one conservatively charged
/// 120-minute authoring snapshot to remain undoable. It is not an allocator or
/// process-RSS reservation.
pub const DEFAULT_AUTHORING_HISTORY_RETAINED_BYTES: usize = 256 * 1024 * 1024;

// This is the same conservative Arc control-block and allocator/alignment
// allowance used by AUTHORING_FOOTPRINT_VERSION V1 in mondrian-core.
const AUTHORING_HISTORY_ARC_ALLOWANCE_WORDS: usize = 4;

/// Hard retention limits for one authoring history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct AuthoringHistoryBudget {
    /// Maximum entries across the Undo and Redo stacks.
    pub max_entries: usize,
    /// Maximum command-owned bytes across both stacks.
    pub max_retained_bytes: usize,
}

impl AuthoringHistoryBudget {
    /// Construct explicit retention limits.
    pub const fn new(max_entries: usize, max_retained_bytes: usize) -> Self {
        Self { max_entries, max_retained_bytes }
    }
}

impl Default for AuthoringHistoryBudget {
    fn default() -> Self {
        Self::new(
            DEFAULT_AUTHORING_HISTORY_MAX_ENTRIES,
            DEFAULT_AUTHORING_HISTORY_RETAINED_BYTES,
        )
    }
}

/// Current and cumulative bounded-history evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuthoringHistoryDiagnostics {
    /// Number of retained Undo entries.
    pub undo_entries: usize,
    /// Number of retained Redo entries.
    pub redo_entries: usize,
    /// Versioned conservative logical allocation charge retained by commands.
    pub retained_bytes: usize,
    /// Accounting formula used to produce `retained_bytes`.
    pub retained_charge_version: AuthoringFootprintVersion,
    /// Active retention limits.
    pub budget: AuthoringHistoryBudget,
    /// Description of the next Undo operation.
    pub undo_description: Option<String>,
    /// Description of the next Redo operation.
    pub redo_description: Option<String>,
    /// Commands evicted to satisfy the bounded-history contract.
    pub budget_evicted_entries: u64,
    /// Bytes released through budget eviction.
    pub budget_evicted_bytes: u64,
    /// Redo commands discarded by a new authoring branch.
    pub branch_discarded_entries: u64,
    /// Older Undo entries discarded because an unretained transaction creates
    /// a snapshot-history gap that cannot be crossed safely.
    pub barrier_discarded_entries: u64,
    /// Commands not retained because the configured entry capacity is zero.
    pub retention_disabled_entries: u64,
    /// Oversize commands that committed but could not be retained.
    pub oversize_dropped_entries: u64,
}

/// Outcome of admitting one already-committed transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringHistoryRecordOutcome {
    /// Whether the command remains available for Undo.
    pub retained: bool,
    /// Entries evicted by this admission.
    pub budget_evicted_entries: usize,
    /// Bytes released by this admission.
    pub budget_evicted_bytes: usize,
    /// Redo entries discarded by the new branch.
    pub branch_discarded_entries: usize,
    /// Older Undo entries discarded to establish an unretained-command
    /// correctness barrier.
    pub barrier_discarded_entries: usize,
}

/// Effect returned after applying an Undo or Redo entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthoringHistoryApplication {
    pub(crate) affected_sequence_ids: Vec<SequenceId>,
    pub(crate) project_wide: bool,
}

/// Detached History payload prepared before canonical author state changes.
///
/// Sequence commands restore only one Sequence. Project commands materialize a
/// complete validated document from a scoped typed endpoint plus the
/// unaffected Sequences in the current contiguous History state.
#[derive(Debug)]
pub(crate) enum PreparedAuthoringHistoryRestore {
    Sequence {
        sequence: Sequence,
        application: AuthoringHistoryApplication,
    },
    Project {
        document: ProjectDocument,
        application: AuthoringHistoryApplication,
    },
}

#[derive(Debug, Clone)]
struct ProjectSequenceRestorePoint {
    sequence_id: SequenceId,
    sequence: Option<Sequence>,
}

impl AuthoringFootprint for ProjectSequenceRestorePoint {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), mondrian_core::AuthoringFootprintError> {
        let Self { sequence_id: _, sequence } = self;
        collector.collect(sequence)
    }
}

#[derive(Debug, Clone)]
struct ProjectMetaRestorePoint {
    name: String,
    description: String,
    author: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl ProjectMetaRestorePoint {
    fn capture(meta: &ProjectMeta) -> Self {
        let ProjectMeta {
            name,
            description,
            author,
            created_at,
            updated_at: _,
        } = meta;
        Self {
            name: name.clone(),
            description: description.clone(),
            author: author.clone(),
            created_at: *created_at,
        }
    }

    fn materialize(&self, current: &ProjectMeta) -> ProjectMeta {
        let ProjectMeta {
            name: _,
            description: _,
            author: _,
            created_at: _,
            updated_at,
        } = current;
        ProjectMeta {
            name: self.name.clone(),
            description: self.description.clone(),
            author: self.author.clone(),
            created_at: self.created_at,
            updated_at: *updated_at,
        }
    }

    fn matches_author_state(&self, current: &ProjectMeta) -> bool {
        let ProjectMeta {
            name,
            description,
            author,
            created_at,
            updated_at: _,
        } = current;
        self.name == *name
            && self.description == *description
            && self.author == *author
            && self.created_at == *created_at
    }
}

impl AuthoringFootprint for ProjectMetaRestorePoint {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), mondrian_core::AuthoringFootprintError> {
        let Self { name, description, author, created_at: _ } = self;
        collector.collect(name)?;
        collector.collect(description)?;
        collector.collect(author)
    }
}

/// Complete authored Project state whose large Sequence payload is limited to
/// the identities changed by one structural transaction.
///
/// Unaffected Sequences are reconstructed from the current contiguous History
/// state in the captured canonical order. This is a typed state endpoint, not
/// a replayable edit command.
#[derive(Debug, Clone)]
struct ProjectRestorePoint {
    schema_version: u32,
    project_id: ProjectId,
    meta: ProjectMetaRestorePoint,
    settings: ProjectSettings,
    color_environment: ProjectColorEnvironment,
    new_sequence_defaults: SequenceSettings,
    proxy_mode_assets: AuthoringSet<AssetId>,
    gallery: ProjectGallery,
    sequence_order: Vec<SequenceId>,
    default_sequence_id: SequenceId,
    active_sequence_id: SequenceId,
    affected_sequences: Vec<ProjectSequenceRestorePoint>,
}

impl ProjectRestorePoint {
    fn capture(document: &ProjectDocument, affected_sequence_ids: &[SequenceId]) -> Result<Self> {
        let ProjectDocument {
            schema_version,
            project_id,
            document_revision: _,
            meta,
            settings,
            color_environment,
            new_sequence_defaults,
            sequences,
            proxy_mode_assets,
            gallery,
        } = document;
        let SequenceCollection {
            sequences: sequence_values,
            default_sequence_id,
            active_sequence_id,
        } = sequences;
        let mut seen = BTreeSet::new();
        let mut affected_sequences = Vec::new();
        affected_sequences.try_reserve(affected_sequence_ids.len()).map_err(|error| {
            authoring_error(
                "capture_project_history_restore_point",
                format!("could not reserve affected Sequence state: {error}"),
            )
        })?;
        for sequence_id in affected_sequence_ids {
            if !seen.insert(*sequence_id) {
                return Err(authoring_error(
                    "capture_project_history_restore_point",
                    format!("affected Sequence identity is duplicated: {sequence_id}"),
                ));
            }
            affected_sequences.push(ProjectSequenceRestorePoint {
                sequence_id: *sequence_id,
                sequence: sequences.sequence(*sequence_id).cloned(),
            });
        }
        Ok(Self {
            schema_version: *schema_version,
            project_id: *project_id,
            meta: ProjectMetaRestorePoint::capture(meta),
            settings: settings.clone(),
            color_environment: color_environment.clone(),
            new_sequence_defaults: new_sequence_defaults.clone(),
            proxy_mode_assets: proxy_mode_assets.clone(),
            gallery: gallery.clone(),
            sequence_order: sequence_values.iter().map(|sequence| sequence.id).collect(),
            default_sequence_id: *default_sequence_id,
            active_sequence_id: *active_sequence_id,
            affected_sequences,
        })
    }

    fn validate_project_identity(&self, current: &ProjectDocument) -> Result<()> {
        if self.project_id != current.project_id {
            return Err(authoring_error(
                "restore_project_authoring_command",
                format!(
                    "snapshot Project identity changed from {} to {}",
                    current.project_id, self.project_id
                ),
            ));
        }
        Ok(())
    }

    fn affected_sequence_index(&self) -> Result<HashMap<SequenceId, Option<&Sequence>>> {
        let mut affected = HashMap::new();
        affected.try_reserve(self.affected_sequences.len()).map_err(|error| {
            authoring_error(
                "restore_project_authoring_command",
                format!("could not reserve affected Sequence index: {error}"),
            )
        })?;
        for restore in &self.affected_sequences {
            if restore
                .sequence
                .as_ref()
                .is_some_and(|sequence| sequence.id != restore.sequence_id)
            {
                return Err(authoring_error(
                    "restore_project_authoring_command",
                    format!(
                        "affected Sequence key {} does not match snapshot identity",
                        restore.sequence_id
                    ),
                ));
            }
            if affected.insert(restore.sequence_id, restore.sequence.as_ref()).is_some() {
                return Err(authoring_error(
                    "restore_project_authoring_command",
                    format!(
                        "affected Sequence identity is duplicated: {}",
                        restore.sequence_id
                    ),
                ));
            }
        }
        Ok(affected)
    }

    fn validate_source(&self, current: &ProjectDocument) -> Result<()> {
        self.validate_project_identity(current)?;
        let current_order_matches = self.sequence_order.len() == current.sequences.sequences.len()
            && self
                .sequence_order
                .iter()
                .zip(&current.sequences.sequences)
                .all(|(expected, sequence)| *expected == sequence.id);
        if self.schema_version != current.schema_version
            || !self.meta.matches_author_state(&current.meta)
            || self.settings != current.settings
            || self.color_environment != current.color_environment
            || self.new_sequence_defaults != current.new_sequence_defaults
            || self.proxy_mode_assets != current.proxy_mode_assets
            || self.gallery != current.gallery
            || !current_order_matches
            || self.default_sequence_id != current.sequences.default_sequence_id
        {
            return Err(authoring_error(
                "restore_project_authoring_command",
                "current Project does not match the contiguous source endpoint",
            ));
        }

        // Active Sequence is editor navigation state. It may legitimately
        // differ after a direct switch without changing author History.
        for (sequence_id, expected) in self.affected_sequence_index()? {
            let current_sequence = current.sequences.sequence(sequence_id);
            let matches = match (expected, current_sequence) {
                (Some(expected), Some(current)) => {
                    expected.author_state_eq_ignoring_revision(current)
                }
                (None, None) => true,
                _ => false,
            };
            if !matches {
                return Err(authoring_error(
                    "restore_project_authoring_command",
                    format!(
                        "affected Sequence {sequence_id} does not match the contiguous source endpoint"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn materialize(&self, current: &ProjectDocument) -> Result<ProjectDocument> {
        self.validate_project_identity(current)?;
        let affected = self.affected_sequence_index()?;
        let ProjectDocument {
            schema_version: _,
            project_id,
            document_revision,
            meta,
            settings: _,
            color_environment: _,
            new_sequence_defaults: _,
            sequences: current_collection,
            proxy_mode_assets: _,
            gallery: _,
        } = current;
        let SequenceCollection {
            sequences: current_sequences,
            default_sequence_id: _,
            active_sequence_id,
        } = current_collection;

        let mut target_ids = BTreeSet::new();
        let mut sequences = Vec::new();
        sequences.try_reserve(self.sequence_order.len()).map_err(|error| {
            authoring_error(
                "restore_project_authoring_command",
                format!("could not reserve restored Sequence collection: {error}"),
            )
        })?;
        for sequence_id in &self.sequence_order {
            if !target_ids.insert(*sequence_id) {
                return Err(authoring_error(
                    "restore_project_authoring_command",
                    format!("snapshot Sequence order duplicates {sequence_id}"),
                ));
            }
            let sequence = match affected.get(sequence_id) {
                Some(Some(sequence)) => (*sequence).clone(),
                Some(None) => {
                    return Err(authoring_error(
                        "restore_project_authoring_command",
                        format!(
                            "snapshot Sequence order retains removed identity {sequence_id}"
                        ),
                    ));
                }
                None => current_collection
                    .sequence(*sequence_id)
                    .cloned()
                    .ok_or_else(|| {
                        authoring_error(
                            "restore_project_authoring_command",
                            format!(
                                "unaffected Sequence {sequence_id} is absent from current contiguous History state"
                            ),
                        )
                    })?,
            };
            sequences.push(sequence);
        }

        for sequence in current_sequences {
            if target_ids.contains(&sequence.id) {
                continue;
            }
            if !matches!(affected.get(&sequence.id), Some(None)) {
                return Err(authoring_error(
                    "restore_project_authoring_command",
                    format!(
                        "restoring the scoped Project endpoint would discard unaffected Sequence {}",
                        sequence.id
                    ),
                ));
            }
        }
        for (sequence_id, sequence) in &affected {
            if sequence.is_some() != target_ids.contains(sequence_id) {
                return Err(authoring_error(
                    "restore_project_authoring_command",
                    format!(
                        "affected Sequence {sequence_id} presence disagrees with snapshot order"
                    ),
                ));
            }
        }
        if !target_ids.contains(&self.default_sequence_id) {
            return Err(authoring_error(
                "restore_project_authoring_command",
                "snapshot default Sequence is absent from its canonical order",
            ));
        }
        let restored_active_sequence_id = if target_ids.contains(active_sequence_id) {
            *active_sequence_id
        } else if target_ids.contains(&self.active_sequence_id) {
            self.active_sequence_id
        } else {
            return Err(authoring_error(
                "restore_project_authoring_command",
                "neither the current nor snapshot active Sequence exists in the restored order",
            ));
        };

        Ok(ProjectDocument {
            schema_version: self.schema_version,
            project_id: *project_id,
            document_revision: *document_revision,
            meta: self.meta.materialize(meta),
            settings: self.settings.clone(),
            color_environment: self.color_environment.clone(),
            new_sequence_defaults: self.new_sequence_defaults.clone(),
            sequences: SequenceCollection {
                sequences: AuthoringList::from(sequences),
                default_sequence_id: self.default_sequence_id,
                active_sequence_id: restored_active_sequence_id,
            },
            proxy_mode_assets: self.proxy_mode_assets.clone(),
            gallery: self.gallery.clone(),
        })
    }
}

impl AuthoringFootprint for ProjectRestorePoint {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), mondrian_core::AuthoringFootprintError> {
        let Self {
            schema_version: _,
            project_id: _,
            meta,
            settings,
            color_environment,
            new_sequence_defaults,
            proxy_mode_assets,
            gallery,
            sequence_order,
            default_sequence_id: _,
            active_sequence_id: _,
            affected_sequences,
        } = self;
        collector.collect(meta)?;
        collector.collect(settings)?;
        collector.collect(color_environment)?;
        collector.collect(new_sequence_defaults)?;
        collector.collect(proxy_mode_assets)?;
        collector.collect(gallery)?;
        collector.collect(sequence_order)?;
        collector.collect(affected_sequences)
    }
}

#[derive(Debug)]
enum AuthoringCommand {
    Sequence {
        description: Box<str>,
        sequence_id: SequenceId,
        before: AuthoringSnapshot<Sequence>,
        after: AuthoringSnapshot<Sequence>,
    },
    Project {
        description: Box<str>,
        affected_sequence_ids: Box<[SequenceId]>,
        before: AuthoringSnapshot<ProjectRestorePoint>,
        after: AuthoringSnapshot<ProjectRestorePoint>,
    },
}

impl AuthoringCommand {
    fn sequence(
        description: impl Into<String>,
        before: &Sequence,
        after: &Sequence,
    ) -> Result<Self> {
        if before.id != after.id {
            return Err(authoring_error(
                "create_sequence_authoring_command",
                format!(
                    "Sequence identity changed from {} to {}",
                    before.id, after.id
                ),
            ));
        }
        Ok(Self::Sequence {
            description: description.into().into_boxed_str(),
            sequence_id: before.id,
            before: AuthoringSnapshot::new(before.clone()),
            after: AuthoringSnapshot::new(after.clone()),
        })
    }

    fn project(
        description: impl Into<String>,
        affected_sequence_ids: &[SequenceId],
        before: &ProjectDocument,
        after: &ProjectDocument,
    ) -> Result<Self> {
        if before.project_id != after.project_id {
            return Err(authoring_error(
                "create_project_authoring_command",
                format!(
                    "Project identity changed from {} to {}",
                    before.project_id, after.project_id
                ),
            ));
        }
        let declared = affected_sequence_ids.iter().copied().collect::<BTreeSet<_>>();
        if declared.len() != affected_sequence_ids.len() {
            return Err(authoring_error(
                "create_project_authoring_command",
                "affected Sequence identities contain duplicates",
            ));
        }
        let changed = project_changed_sequence_ids(before, after)?;
        if declared != changed {
            return Err(authoring_error(
                "create_project_authoring_command",
                format!(
                    "affected Sequence scope does not match authored changes: declared={declared:?}, changed={changed:?}"
                ),
            ));
        }
        Ok(Self::Project {
            description: description.into().into_boxed_str(),
            affected_sequence_ids: affected_sequence_ids.to_vec().into_boxed_slice(),
            before: AuthoringSnapshot::new(ProjectRestorePoint::capture(
                before,
                affected_sequence_ids,
            )?),
            after: AuthoringSnapshot::new(ProjectRestorePoint::capture(
                after,
                affected_sequence_ids,
            )?),
        })
    }

    fn description(&self) -> &str {
        match self {
            Self::Sequence { description, .. } | Self::Project { description, .. } => description,
        }
    }

    fn conservative_dynamic_metadata_bytes(&self) -> Result<u64> {
        let description_bytes = usize_to_u64(self.description().len())?;
        let affected_id_bytes = match self {
            Self::Sequence { .. } => 0,
            Self::Project { affected_sequence_ids, .. } => {
                checked_mul_usize(affected_sequence_ids.len(), size_of::<SequenceId>())?
            }
        };
        checked_add_u64(description_bytes, affected_id_bytes)
    }

    fn collect_endpoint_descriptors(
        &self,
        collector: &mut AuthoringFootprintDescriptorCollector,
    ) -> std::result::Result<(), mondrian_core::AuthoringFootprintError> {
        match self {
            Self::Sequence { before, after, .. } => {
                collector.collect(before)?;
                collector.collect(after)
            }
            Self::Project { before, after, .. } => {
                collector.collect(before)?;
                collector.collect(after)
            }
        }
    }

    fn prepare_restore_before(
        &self,
        document: &ProjectDocument,
    ) -> Result<PreparedAuthoringHistoryRestore> {
        self.prepare_restore(document, false)
    }

    fn prepare_restore_after(
        &self,
        document: &ProjectDocument,
    ) -> Result<PreparedAuthoringHistoryRestore> {
        self.prepare_restore(document, true)
    }

    fn prepare_restore(
        &self,
        document: &ProjectDocument,
        use_after: bool,
    ) -> Result<PreparedAuthoringHistoryRestore> {
        match self {
            Self::Sequence { sequence_id, before, after, .. } => {
                let (source, target) = if use_after {
                    (before.value(), after.value())
                } else {
                    (after.value(), before.value())
                };
                let restored = target.clone();
                if restored.id != *sequence_id {
                    return Err(authoring_error(
                        "restore_sequence_authoring_command",
                        format!(
                            "snapshot identity changed from {sequence_id} to {}",
                            restored.id
                        ),
                    ));
                }
                let current = document.sequences.sequence(*sequence_id).ok_or_else(|| {
                    authoring_error(
                        "restore_sequence_authoring_command",
                        format!("target Sequence no longer exists: {sequence_id}"),
                    )
                })?;
                if source.id != *sequence_id || !source.author_state_eq_ignoring_revision(current) {
                    return Err(authoring_error(
                        "restore_sequence_authoring_command",
                        format!(
                            "Sequence {sequence_id} does not match the contiguous source endpoint"
                        ),
                    ));
                }
                Ok(PreparedAuthoringHistoryRestore::Sequence {
                    sequence: restored,
                    application: AuthoringHistoryApplication {
                        affected_sequence_ids: vec![*sequence_id],
                        project_wide: false,
                    },
                })
            }
            Self::Project { affected_sequence_ids, before, after, .. } => {
                let (source, target) = if use_after {
                    (before.value(), after.value())
                } else {
                    (after.value(), before.value())
                };
                source.validate_source(document)?;
                let restored = target.materialize(document)?;
                Ok(PreparedAuthoringHistoryRestore::Project {
                    document: restored,
                    application: AuthoringHistoryApplication {
                        affected_sequence_ids: affected_sequence_ids.to_vec(),
                        project_wide: true,
                    },
                })
            }
        }
    }
}

#[derive(Debug)]
struct AuthoringHistoryEntry {
    command: AuthoringCommand,
    footprint_roots: [AuthoringAllocationFootprint; 2],
    metadata_bytes: u64,
}

impl AuthoringHistoryEntry {
    fn conservative_metadata_bytes(command: &AuthoringCommand) -> Result<u64> {
        let entry_allocation_bytes = checked_add_u64(
            usize_to_u64(size_of::<Self>())?,
            checked_mul_usize(AUTHORING_HISTORY_ARC_ALLOWANCE_WORDS, size_of::<usize>())?,
        )?;
        let retained_stack_slot_bytes = usize_to_u64(size_of::<Arc<Self>>())?;
        checked_add_u64(
            checked_add_u64(entry_allocation_bytes, retained_stack_slot_bytes)?,
            command.conservative_dynamic_metadata_bytes()?,
        )
    }

    fn compile(
        command: AuthoringCommand,
        descriptor_cache: AuthoringFootprintDescriptorCache,
    ) -> Result<Self> {
        let metadata_bytes = Self::conservative_metadata_bytes(&command)?;
        let mut collector =
            AuthoringFootprintDescriptorCollector::with_descriptor_cache(descriptor_cache);
        command
            .collect_endpoint_descriptors(&mut collector)
            .map_err(|error| footprint_error("compile_authoring_history_entry", error))?;
        let roots = collector.finish_roots();
        let footprint_roots: [AuthoringAllocationFootprint; 2] =
            roots.try_into().map_err(|roots: Vec<_>| {
                authoring_error(
                    "compile_authoring_history_entry",
                    format!(
                        "History command produced {} allocation roots instead of two",
                        roots.len()
                    ),
                )
            })?;
        Ok(Self { command, footprint_roots, metadata_bytes })
    }

    fn description(&self) -> &str {
        self.command.description()
    }

    fn prepare_restore_before(
        &self,
        document: &ProjectDocument,
    ) -> Result<PreparedAuthoringHistoryRestore> {
        self.command.prepare_restore_before(document)
    }

    fn prepare_restore_after(
        &self,
        document: &ProjectDocument,
    ) -> Result<PreparedAuthoringHistoryRestore> {
        self.command.prepare_restore_after(document)
    }
}

#[derive(Debug)]
struct AuthoringHistoryState {
    revision: u64,
    undo_stack: VecDeque<Arc<AuthoringHistoryEntry>>,
    redo_stack: VecDeque<Arc<AuthoringHistoryEntry>>,
    retained_bytes: usize,
    budget_evicted_entries: u64,
    budget_evicted_bytes: u64,
    branch_discarded_entries: u64,
    barrier_discarded_entries: u64,
    retention_disabled_entries: u64,
    oversize_dropped_entries: u64,
}

impl AuthoringHistoryState {
    fn empty() -> Self {
        Self {
            revision: 0,
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            retained_bytes: 0,
            budget_evicted_entries: 0,
            budget_evicted_bytes: 0,
            branch_discarded_entries: 0,
            barrier_discarded_entries: 0,
            retention_disabled_entries: 0,
            oversize_dropped_entries: 0,
        }
    }
}

#[derive(Debug)]
struct RetainedFootprintIndex {
    reference_counts: HashMap<mondrian_core::AuthoringAllocationId, u32>,
    allocation_bytes: u64,
    metadata_bytes: u64,
}

impl RetainedFootprintIndex {
    fn empty() -> Self {
        Self {
            reference_counts: HashMap::new(),
            allocation_bytes: 0,
            metadata_bytes: 0,
        }
    }

    fn total_bytes(&self) -> Result<usize> {
        usize::try_from(checked_add_u64(self.allocation_bytes, self.metadata_bytes)?).map_err(
            |_| {
                authoring_error(
                    "measure_authoring_history",
                    "History retained bytes do not fit usize",
                )
            },
        )
    }

    fn try_reserve_for_plan(&mut self, plan: &PreparedFootprintPlan) -> Result<()> {
        let new_entries = plan
            .reference_counts
            .iter()
            .filter(|entry| {
                let (allocation_id, final_count) = **entry;
                final_count > 0 && !self.reference_counts.contains_key(&allocation_id)
            })
            .count();
        self.reference_counts.try_reserve(new_entries).map_err(|error| {
            authoring_error(
                "reserve_authoring_history_footprint",
                format!("could not reserve retained-allocation index storage: {error}"),
            )
        })
    }
}

#[derive(Debug)]
struct FootprintOverlay<'a> {
    base: &'a RetainedFootprintIndex,
    final_counts: HashMap<mondrian_core::AuthoringAllocationId, u32>,
    descriptors: HashMap<mondrian_core::AuthoringAllocationId, AuthoringAllocationFootprint>,
    allocation_bytes: u64,
    metadata_bytes: u64,
}

impl<'a> FootprintOverlay<'a> {
    fn new(base: &'a RetainedFootprintIndex) -> Self {
        Self {
            base,
            final_counts: HashMap::new(),
            descriptors: HashMap::new(),
            allocation_bytes: base.allocation_bytes,
            metadata_bytes: base.metadata_bytes,
        }
    }

    fn add_entry(&mut self, entry: &AuthoringHistoryEntry) -> Result<()> {
        self.metadata_bytes = checked_add_u64(self.metadata_bytes, entry.metadata_bytes)?;
        for root in &entry.footprint_roots {
            self.add_allocation(root)?;
        }
        Ok(())
    }

    fn remove_entry(&mut self, entry: &AuthoringHistoryEntry) -> Result<()> {
        self.metadata_bytes =
            self.metadata_bytes.checked_sub(entry.metadata_bytes).ok_or_else(|| {
                authoring_error(
                    "remove_authoring_history_entry_footprint",
                    "History metadata charge underflowed",
                )
            })?;
        for root in &entry.footprint_roots {
            self.remove_allocation(root)?;
        }
        Ok(())
    }

    fn total_bytes(&self) -> Result<usize> {
        usize::try_from(checked_add_u64(self.allocation_bytes, self.metadata_bytes)?).map_err(
            |_| {
                authoring_error(
                    "measure_authoring_history",
                    "History retained bytes do not fit usize",
                )
            },
        )
    }

    fn add_allocation(&mut self, descriptor: &AuthoringAllocationFootprint) -> Result<()> {
        self.remember_descriptor(descriptor)?;
        let allocation_id = descriptor.allocation_id();
        let current = self.current_count(allocation_id);
        let next = current.checked_add(1).ok_or_else(|| {
            authoring_error(
                "add_authoring_history_allocation",
                format!("allocation reference count exhausted for {allocation_id:?}"),
            )
        })?;
        self.set_count(allocation_id, next)?;
        if current == 0 {
            self.allocation_bytes =
                checked_add_u64(self.allocation_bytes, descriptor.local_bytes())?;
            for child in descriptor.children() {
                self.add_allocation(child)?;
            }
        }
        Ok(())
    }

    fn remove_allocation(&mut self, descriptor: &AuthoringAllocationFootprint) -> Result<()> {
        self.remember_descriptor(descriptor)?;
        let allocation_id = descriptor.allocation_id();
        let current = self.current_count(allocation_id);
        let next = current.checked_sub(1).ok_or_else(|| {
            authoring_error(
                "remove_authoring_history_allocation",
                format!("allocation {allocation_id:?} has no retained reference"),
            )
        })?;
        self.set_count(allocation_id, next)?;
        if next == 0 {
            self.allocation_bytes =
                self.allocation_bytes.checked_sub(descriptor.local_bytes()).ok_or_else(|| {
                    authoring_error(
                        "remove_authoring_history_allocation",
                        format!("allocation charge underflowed for {allocation_id:?}"),
                    )
                })?;
            for child in descriptor.children() {
                self.remove_allocation(child)?;
            }
        }
        Ok(())
    }

    fn current_count(&self, allocation_id: mondrian_core::AuthoringAllocationId) -> u32 {
        self.final_counts
            .get(&allocation_id)
            .copied()
            .or_else(|| self.base.reference_counts.get(&allocation_id).copied())
            .unwrap_or(0)
    }

    fn set_count(
        &mut self,
        allocation_id: mondrian_core::AuthoringAllocationId,
        count: u32,
    ) -> Result<()> {
        if !self.final_counts.contains_key(&allocation_id) {
            self.final_counts.try_reserve(1).map_err(|error| {
                authoring_error(
                    "prepare_authoring_history_footprint",
                    format!("could not reserve footprint overlay storage: {error}"),
                )
            })?;
        }
        self.final_counts.insert(allocation_id, count);
        Ok(())
    }

    fn remember_descriptor(&mut self, descriptor: &AuthoringAllocationFootprint) -> Result<()> {
        let allocation_id = descriptor.allocation_id();
        if let Some(existing) = self.descriptors.get(&allocation_id) {
            if !descriptors_match(existing, descriptor) {
                return Err(authoring_error(
                    "prepare_authoring_history_footprint",
                    format!("allocation {allocation_id:?} has conflicting descriptors"),
                ));
            }
            return Ok(());
        }
        self.descriptors.try_reserve(1).map_err(|error| {
            authoring_error(
                "prepare_authoring_history_footprint",
                format!("could not reserve descriptor overlay storage: {error}"),
            )
        })?;
        self.descriptors.insert(allocation_id, descriptor.clone());
        Ok(())
    }

    fn finish(self) -> Result<PreparedFootprintPlan> {
        let mut reference_counts = Vec::new();
        let mut cache_insertions = Vec::new();
        let mut cache_removals = Vec::new();
        reference_counts.try_reserve(self.final_counts.len()).map_err(|error| {
            authoring_error(
                "prepare_authoring_history_footprint",
                format!("could not reserve footprint plan storage: {error}"),
            )
        })?;
        cache_insertions.try_reserve(self.final_counts.len()).map_err(|error| {
            authoring_error(
                "prepare_authoring_history_footprint",
                format!("could not reserve descriptor insertion plan: {error}"),
            )
        })?;
        cache_removals.try_reserve(self.final_counts.len()).map_err(|error| {
            authoring_error(
                "prepare_authoring_history_footprint",
                format!("could not reserve descriptor removal plan: {error}"),
            )
        })?;
        for (allocation_id, final_count) in self.final_counts {
            let initial_count =
                self.base.reference_counts.get(&allocation_id).copied().unwrap_or(0);
            if final_count == initial_count {
                continue;
            }
            reference_counts.push((allocation_id, final_count));
            if initial_count == 0 && final_count > 0 {
                let descriptor =
                    self.descriptors.get(&allocation_id).cloned().ok_or_else(|| {
                        authoring_error(
                            "prepare_authoring_history_footprint",
                            format!("new allocation {allocation_id:?} has no descriptor"),
                        )
                    })?;
                cache_insertions.push(descriptor);
            } else if initial_count > 0 && final_count == 0 {
                cache_removals.push(allocation_id);
            }
        }
        Ok(PreparedFootprintPlan {
            reference_counts,
            allocation_bytes: self.allocation_bytes,
            metadata_bytes: self.metadata_bytes,
            cache_insertions,
            cache_removals,
        })
    }
}

#[derive(Debug)]
struct PreparedFootprintPlan {
    reference_counts: Vec<(mondrian_core::AuthoringAllocationId, u32)>,
    allocation_bytes: u64,
    metadata_bytes: u64,
    cache_insertions: Vec<AuthoringAllocationFootprint>,
    cache_removals: Vec<mondrian_core::AuthoringAllocationId>,
}

impl PreparedFootprintPlan {
    fn retained_bytes(&self) -> Result<usize> {
        usize::try_from(checked_add_u64(self.allocation_bytes, self.metadata_bytes)?).map_err(
            |_| {
                authoring_error(
                    "measure_authoring_history",
                    "History retained bytes do not fit usize",
                )
            },
        )
    }

    fn apply(
        self,
        index: &mut RetainedFootprintIndex,
        descriptor_cache: &AuthoringFootprintDescriptorCache,
    ) {
        for (allocation_id, final_count) in self.reference_counts {
            if final_count == 0 {
                index.reference_counts.remove(&allocation_id);
            } else {
                index.reference_counts.insert(allocation_id, final_count);
            }
        }
        index.allocation_bytes = self.allocation_bytes;
        index.metadata_bytes = self.metadata_bytes;
        descriptor_cache.apply_prepared(self.cache_insertions, self.cache_removals);
    }
}

fn descriptors_match(
    left: &AuthoringAllocationFootprint,
    right: &AuthoringAllocationFootprint,
) -> bool {
    left.allocation_id() == right.allocation_id()
        && left.direct_bytes() == right.direct_bytes()
        && left.exclusive_bytes() == right.exclusive_bytes()
        && left.local_bytes() == right.local_bytes()
        && left.children().len() == right.children().len()
        && left
            .children()
            .iter()
            .zip(right.children())
            .all(|(left, right)| left.allocation_id() == right.allocation_id())
}

/// Why an already-committed authoring transaction cannot be retained.
#[derive(Debug)]
enum UnretainedHistoryCommandReason {
    RetentionDisabled,
    ExceedsRetainedBytes { command_retained_bytes: usize },
}

#[derive(Debug)]
struct UnretainedHistoryCommand {
    description: Box<str>,
    reason: UnretainedHistoryCommandReason,
}

/// Fully measured History candidate whose commit performs one state replacement.
#[derive(Debug)]
pub(crate) struct PreparedAuthoringHistoryRecord {
    expected_revision: u64,
    candidate: AuthoringHistoryState,
    footprint_plan: PreparedFootprintPlan,
    outcome: AuthoringHistoryRecordOutcome,
    unretained_command: Option<UnretainedHistoryCommand>,
}

/// One project-wide bounded Undo/Redo history.
#[derive(Debug)]
pub struct AuthoringHistory {
    budget: AuthoringHistoryBudget,
    state: AuthoringHistoryState,
    footprint_index: RetainedFootprintIndex,
    descriptor_cache: AuthoringFootprintDescriptorCache,
}

impl Default for AuthoringHistory {
    fn default() -> Self {
        Self::with_budget(AuthoringHistoryBudget::default())
    }
}

impl AuthoringHistory {
    /// Construct an empty history with explicit hard limits.
    pub fn with_budget(budget: AuthoringHistoryBudget) -> Self {
        Self {
            budget,
            state: AuthoringHistoryState::empty(),
            footprint_index: RetainedFootprintIndex::empty(),
            descriptor_cache: AuthoringFootprintDescriptorCache::new(),
        }
    }

    /// Prepare a typed Sequence command without mutating either History stack.
    pub(crate) fn prepare_sequence_record(
        &mut self,
        description: impl Into<String>,
        before: &Sequence,
        after: &Sequence,
    ) -> Result<PreparedAuthoringHistoryRecord> {
        self.prepare_record(AuthoringCommand::sequence(description, before, after)?)
    }

    /// Prepare a typed Project command without mutating either History stack.
    pub(crate) fn prepare_project_record(
        &mut self,
        description: impl Into<String>,
        affected_sequence_ids: &[SequenceId],
        before: &ProjectDocument,
        after: &ProjectDocument,
    ) -> Result<PreparedAuthoringHistoryRecord> {
        self.prepare_record(AuthoringCommand::project(
            description,
            affected_sequence_ids,
            before,
            after,
        )?)
    }

    #[cfg(test)]
    pub(crate) fn record_sequence(
        &mut self,
        description: impl Into<String>,
        before: &Sequence,
        after: &Sequence,
    ) -> Result<AuthoringHistoryRecordOutcome> {
        let prepared = self.prepare_sequence_record(description, before, after)?;
        self.commit_prepared_record(prepared)
    }

    #[cfg(test)]
    pub(crate) fn record_project(
        &mut self,
        description: impl Into<String>,
        affected_sequence_ids: &[SequenceId],
        before: &ProjectDocument,
        after: &ProjectDocument,
    ) -> Result<AuthoringHistoryRecordOutcome> {
        let prepared =
            self.prepare_project_record(description, affected_sequence_ids, before, after)?;
        self.commit_prepared_record(prepared)
    }

    fn prepare_record(
        &mut self,
        command: AuthoringCommand,
    ) -> Result<PreparedAuthoringHistoryRecord> {
        let expected_revision = self.state.revision;
        let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
            authoring_error(
                "prepare_authoring_history_record",
                "History revision exhausted",
            )
        })?;
        let entry = Arc::new(AuthoringHistoryEntry::compile(
            command,
            self.descriptor_cache.clone(),
        )?);
        let branch_discarded_entries = self.state.redo_stack.len();
        let mut undo_stack = clone_history_stack(&self.state.undo_stack)?;
        let redo_stack = VecDeque::new();
        let next_branch_discarded_entries = checked_counter_add(
            self.state.branch_discarded_entries,
            branch_discarded_entries,
            "account_authoring_history_branch_discard",
        )?;

        if self.budget.max_entries == 0 {
            let mut overlay = FootprintOverlay::new(&self.footprint_index);
            for discarded in &self.state.undo_stack {
                overlay.remove_entry(discarded)?;
            }
            for discarded in &self.state.redo_stack {
                overlay.remove_entry(discarded)?;
            }
            let footprint_plan = overlay.finish()?;
            self.reserve_footprint_plan(&footprint_plan)?;
            let retained_bytes = footprint_plan.retained_bytes()?;
            let barrier_discarded_entries = self.state.undo_stack.len();
            let candidate = AuthoringHistoryState {
                revision: next_revision,
                undo_stack: VecDeque::new(),
                redo_stack,
                retained_bytes,
                budget_evicted_entries: self.state.budget_evicted_entries,
                budget_evicted_bytes: self.state.budget_evicted_bytes,
                branch_discarded_entries: next_branch_discarded_entries,
                barrier_discarded_entries: checked_counter_add(
                    self.state.barrier_discarded_entries,
                    barrier_discarded_entries,
                    "account_authoring_history_barrier",
                )?,
                retention_disabled_entries: self
                    .state
                    .retention_disabled_entries
                    .checked_add(1)
                    .ok_or_else(|| {
                        authoring_error(
                            "account_authoring_history_retention_disabled",
                            "disabled-retention History counter exhausted",
                        )
                    })?,
                oversize_dropped_entries: self.state.oversize_dropped_entries,
            };
            return Ok(PreparedAuthoringHistoryRecord {
                expected_revision,
                candidate,
                footprint_plan,
                outcome: AuthoringHistoryRecordOutcome {
                    retained: false,
                    budget_evicted_entries: 0,
                    budget_evicted_bytes: 0,
                    branch_discarded_entries,
                    barrier_discarded_entries,
                },
                unretained_command: Some(UnretainedHistoryCommand {
                    description: entry.description().to_owned().into_boxed_str(),
                    reason: UnretainedHistoryCommandReason::RetentionDisabled,
                }),
            });
        }

        undo_stack.try_reserve(1).map_err(|error| {
            authoring_error(
                "prepare_authoring_history_record",
                format!("could not reserve Undo history storage: {error}"),
            )
        })?;
        undo_stack.push_back(Arc::clone(&entry));
        let mut overlay = FootprintOverlay::new(&self.footprint_index);
        // Add the new roots before removing Redo roots. Shared descendants then
        // stay active across a branch instead of being torn down and rebuilt.
        overlay.add_entry(&entry)?;
        for discarded in &self.state.redo_stack {
            overlay.remove_entry(discarded)?;
        }
        let pre_eviction_charge = overlay.total_bytes()?;
        let mut retained_bytes = pre_eviction_charge;
        let mut budget_evicted_entries = 0usize;
        while undo_stack.len() + redo_stack.len() > self.budget.max_entries
            || retained_bytes > self.budget.max_retained_bytes
        {
            // Reaching one entry means the candidate stack contains only the
            // newly appended command, so its current charge is also the exact
            // standalone charge. A snapshot History cannot safely retain
            // older entries across an unretained command: crossing that gap
            // would restore a snapshot that predates committed author state.
            if undo_stack.len() == 1 {
                // The overlay now contains only the new entry and no Redo
                // roots, so this is the exact standalone command charge. Do
                // not rebuild a second manifest over the same descriptor DAG.
                let command_only_charge = retained_bytes;
                let barrier_discarded_entries = self.state.undo_stack.len();
                let mut barrier_overlay = FootprintOverlay::new(&self.footprint_index);
                for discarded in &self.state.undo_stack {
                    barrier_overlay.remove_entry(discarded)?;
                }
                for discarded in &self.state.redo_stack {
                    barrier_overlay.remove_entry(discarded)?;
                }
                let footprint_plan = barrier_overlay.finish()?;
                self.reserve_footprint_plan(&footprint_plan)?;
                let retained_bytes = footprint_plan.retained_bytes()?;
                let candidate = AuthoringHistoryState {
                    revision: next_revision,
                    undo_stack: VecDeque::new(),
                    redo_stack,
                    retained_bytes,
                    budget_evicted_entries: self.state.budget_evicted_entries,
                    budget_evicted_bytes: self.state.budget_evicted_bytes,
                    branch_discarded_entries: next_branch_discarded_entries,
                    barrier_discarded_entries: checked_counter_add(
                        self.state.barrier_discarded_entries,
                        barrier_discarded_entries,
                        "account_authoring_history_barrier",
                    )?,
                    retention_disabled_entries: self.state.retention_disabled_entries,
                    oversize_dropped_entries: self
                        .state
                        .oversize_dropped_entries
                        .checked_add(1)
                        .ok_or_else(|| {
                            authoring_error(
                                "account_authoring_history_oversize",
                                "oversize History counter exhausted",
                            )
                        })?,
                };
                return Ok(PreparedAuthoringHistoryRecord {
                    expected_revision,
                    candidate,
                    footprint_plan,
                    outcome: AuthoringHistoryRecordOutcome {
                        retained: false,
                        budget_evicted_entries: 0,
                        budget_evicted_bytes: 0,
                        branch_discarded_entries,
                        barrier_discarded_entries,
                    },
                    unretained_command: Some(UnretainedHistoryCommand {
                        description: entry.description().to_owned().into_boxed_str(),
                        reason: UnretainedHistoryCommandReason::ExceedsRetainedBytes {
                            command_retained_bytes: command_only_charge,
                        },
                    }),
                });
            }
            let Some(evicted) = undo_stack.pop_front() else {
                return Err(authoring_error(
                    "prepare_authoring_history_record",
                    "History budget could not retain a command that fits by itself",
                ));
            };
            overlay.remove_entry(&evicted)?;
            budget_evicted_entries = budget_evicted_entries.checked_add(1).ok_or_else(|| {
                authoring_error(
                    "account_authoring_history_eviction",
                    "History eviction count overflowed",
                )
            })?;
            retained_bytes = overlay.total_bytes()?;
        }
        let footprint_plan = overlay.finish()?;
        self.reserve_footprint_plan(&footprint_plan)?;
        debug_assert_eq!(footprint_plan.retained_bytes().ok(), Some(retained_bytes));
        let budget_evicted_bytes =
            pre_eviction_charge.checked_sub(retained_bytes).ok_or_else(|| {
                authoring_error(
                    "account_authoring_history_eviction",
                    "History charge increased after removing an entry",
                )
            })?;
        let candidate = AuthoringHistoryState {
            revision: next_revision,
            undo_stack,
            redo_stack,
            retained_bytes,
            budget_evicted_entries: checked_counter_add(
                self.state.budget_evicted_entries,
                budget_evicted_entries,
                "account_authoring_history_eviction",
            )?,
            budget_evicted_bytes: checked_counter_add(
                self.state.budget_evicted_bytes,
                budget_evicted_bytes,
                "account_authoring_history_eviction",
            )?,
            branch_discarded_entries: next_branch_discarded_entries,
            barrier_discarded_entries: self.state.barrier_discarded_entries,
            retention_disabled_entries: self.state.retention_disabled_entries,
            oversize_dropped_entries: self.state.oversize_dropped_entries,
        };
        Ok(PreparedAuthoringHistoryRecord {
            expected_revision,
            candidate,
            footprint_plan,
            outcome: AuthoringHistoryRecordOutcome {
                retained: true,
                budget_evicted_entries,
                budget_evicted_bytes,
                branch_discarded_entries,
                barrier_discarded_entries: 0,
            },
            unretained_command: None,
        })
    }

    fn reserve_footprint_plan(&mut self, plan: &PreparedFootprintPlan) -> Result<()> {
        self.footprint_index.try_reserve_for_plan(plan)?;
        self.descriptor_cache
            .validate_and_reserve(&plan.cache_insertions)
            .map_err(|error| footprint_error("reserve_authoring_history_descriptors", error))
    }

    /// Admit an already prepared entry with one state replacement.
    ///
    /// A stale candidate fails before replacement, keeping canonical author
    /// state and History atomic.
    pub(crate) fn commit_prepared_record(
        &mut self,
        prepared: PreparedAuthoringHistoryRecord,
    ) -> Result<AuthoringHistoryRecordOutcome> {
        if self.state.revision != prepared.expected_revision {
            return Err(authoring_error(
                "commit_prepared_authoring_history_record",
                format!(
                    "prepared History revision {} is stale; current revision is {}",
                    prepared.expected_revision, self.state.revision
                ),
            ));
        }
        if let Some(unretained) = &prepared.unretained_command {
            match unretained.reason {
                UnretainedHistoryCommandReason::RetentionDisabled => {
                    tracing::warn!(
                        command = unretained.description.as_ref(),
                        history_max_entries = self.budget.max_entries,
                        "committed authoring transaction while Undo retention is disabled"
                    );
                }
                UnretainedHistoryCommandReason::ExceedsRetainedBytes { command_retained_bytes } => {
                    tracing::warn!(
                        command = unretained.description.as_ref(),
                        command_retained_bytes,
                        history_budget_bytes = self.budget.max_retained_bytes,
                        "committed authoring transaction exceeds the Undo byte budget"
                    );
                }
            }
        }
        let outcome = prepared.outcome;
        prepared.footprint_plan.apply(&mut self.footprint_index, &self.descriptor_cache);
        self.state = prepared.candidate;
        debug_assert_eq!(
            self.footprint_index.total_bytes().ok(),
            Some(self.state.retained_bytes)
        );
        Ok(outcome)
    }

    /// Prepare the next Undo payload without moving History.
    pub(crate) fn prepare_undo(
        &self,
        document: &ProjectDocument,
    ) -> Result<Option<PreparedAuthoringHistoryRestore>> {
        let Some(entry) = self.state.undo_stack.back() else {
            return Ok(None);
        };
        entry.prepare_restore_before(document).map(Some)
    }

    /// Prepare the next Redo payload without moving History.
    pub(crate) fn prepare_redo(
        &self,
        document: &ProjectDocument,
    ) -> Result<Option<PreparedAuthoringHistoryRestore>> {
        let Some(entry) = self.state.redo_stack.back() else {
            return Ok(None);
        };
        entry.prepare_restore_after(document).map(Some)
    }

    /// Move the entry whose detached Undo payload has already validated.
    pub(crate) fn commit_prepared_undo(&mut self) -> Result<()> {
        let next_revision = self.state.revision.checked_add(1).ok_or_else(|| {
            authoring_error(
                "commit_prepared_authoring_undo",
                "History revision exhausted",
            )
        })?;
        self.state.redo_stack.try_reserve(1).map_err(|error| {
            authoring_error(
                "commit_prepared_authoring_undo",
                format!("could not reserve Redo history storage: {error}"),
            )
        })?;
        let entry = self.state.undo_stack.pop_back().ok_or_else(|| {
            authoring_error(
                "commit_prepared_authoring_undo",
                "prepared Undo entry is no longer available",
            )
        })?;
        self.state.redo_stack.push_back(entry);
        self.state.revision = next_revision;
        Ok(())
    }

    /// Move the entry whose detached Redo payload has already validated.
    pub(crate) fn commit_prepared_redo(&mut self) -> Result<()> {
        let next_revision = self.state.revision.checked_add(1).ok_or_else(|| {
            authoring_error(
                "commit_prepared_authoring_redo",
                "History revision exhausted",
            )
        })?;
        self.state.undo_stack.try_reserve(1).map_err(|error| {
            authoring_error(
                "commit_prepared_authoring_redo",
                format!("could not reserve Undo history storage: {error}"),
            )
        })?;
        let entry = self.state.redo_stack.pop_back().ok_or_else(|| {
            authoring_error(
                "commit_prepared_authoring_redo",
                "prepared Redo entry is no longer available",
            )
        })?;
        self.state.undo_stack.push_back(entry);
        self.state.revision = next_revision;
        Ok(())
    }

    /// Whether an Undo entry exists.
    pub fn can_undo(&self) -> bool {
        !self.state.undo_stack.is_empty()
    }

    /// Whether a Redo entry exists.
    pub fn can_redo(&self) -> bool {
        !self.state.redo_stack.is_empty()
    }

    /// User-facing label for the next Undo operation.
    pub fn undo_description(&self) -> Option<&str> {
        self.state.undo_stack.back().map(|entry| entry.description())
    }

    /// User-facing label for the next Redo operation.
    pub fn redo_description(&self) -> Option<&str> {
        self.state.redo_stack.back().map(|entry| entry.description())
    }

    /// Return bounded-history evidence.
    pub fn diagnostics(&self) -> AuthoringHistoryDiagnostics {
        AuthoringHistoryDiagnostics {
            undo_entries: self.state.undo_stack.len(),
            redo_entries: self.state.redo_stack.len(),
            retained_bytes: self.state.retained_bytes,
            retained_charge_version: AUTHORING_FOOTPRINT_VERSION,
            budget: self.budget,
            undo_description: self.undo_description().map(ToOwned::to_owned),
            redo_description: self.redo_description().map(ToOwned::to_owned),
            budget_evicted_entries: self.state.budget_evicted_entries,
            budget_evicted_bytes: self.state.budget_evicted_bytes,
            branch_discarded_entries: self.state.branch_discarded_entries,
            barrier_discarded_entries: self.state.barrier_discarded_entries,
            retention_disabled_entries: self.state.retention_disabled_entries,
            oversize_dropped_entries: self.state.oversize_dropped_entries,
        }
    }
}

fn clone_history_stack(
    stack: &VecDeque<Arc<AuthoringHistoryEntry>>,
) -> Result<VecDeque<Arc<AuthoringHistoryEntry>>> {
    let mut cloned = VecDeque::new();
    cloned.try_reserve(stack.len()).map_err(|error| {
        authoring_error(
            "prepare_authoring_history_record",
            format!("could not reserve cloned History stack storage: {error}"),
        )
    })?;
    cloned.extend(stack.iter().cloned());
    Ok(cloned)
}

#[cfg(test)]
fn retained_bytes_for_stacks(
    undo_stack: &VecDeque<Arc<AuthoringHistoryEntry>>,
    redo_stack: &VecDeque<Arc<AuthoringHistoryEntry>>,
) -> Result<usize> {
    let base = RetainedFootprintIndex::empty();
    let mut overlay = FootprintOverlay::new(&base);
    for entry in undo_stack.iter().chain(redo_stack) {
        overlay.add_entry(entry)?;
    }
    overlay.total_bytes()
}

fn checked_counter_add(current: u64, delta: usize, step_id: &str) -> Result<u64> {
    current
        .checked_add(usize_to_u64(delta)?)
        .ok_or_else(|| authoring_error(step_id, "cumulative History diagnostic counter exhausted"))
}

fn project_changed_sequence_ids(
    before: &ProjectDocument,
    after: &ProjectDocument,
) -> Result<BTreeSet<SequenceId>> {
    let before_ids = before
        .sequences
        .sequences
        .iter()
        .map(|sequence| sequence.id)
        .collect::<BTreeSet<_>>();
    let after_ids = after
        .sequences
        .sequences
        .iter()
        .map(|sequence| sequence.id)
        .collect::<BTreeSet<_>>();
    if before_ids.len() != before.sequences.sequences.len()
        || after_ids.len() != after.sequences.sequences.len()
    {
        return Err(authoring_error(
            "create_project_authoring_command",
            "Project Sequence collection contains duplicate identities",
        ));
    }
    Ok(before_ids
        .union(&after_ids)
        .copied()
        .filter(|sequence_id| {
            match (
                before.sequences.sequence(*sequence_id),
                after.sequences.sequence(*sequence_id),
            ) {
                (Some(before), Some(after)) => !before.author_state_eq_ignoring_revision(after),
                _ => true,
            }
        })
        .collect())
}

fn checked_mul_usize(left: usize, right: usize) -> Result<u64> {
    let product = left.checked_mul(right).ok_or_else(|| {
        authoring_error(
            "measure_authoring_history",
            "History retained-byte multiplication overflowed",
        )
    })?;
    usize_to_u64(product)
}

fn checked_add_u64(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right).ok_or_else(|| {
        authoring_error(
            "measure_authoring_history",
            "History retained-byte addition overflowed",
        )
    })
}

fn usize_to_u64(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        authoring_error(
            "measure_authoring_history",
            "History retained bytes do not fit u64",
        )
    })
}

fn footprint_error(step_id: &str, error: mondrian_core::AuthoringFootprintError) -> MondrianError {
    authoring_error(step_id, error.to_string())
}

fn authoring_error(step_id: &str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        AuthoringFootprint, AuthoringList, ProjectColorEnvironment, ProjectSettings,
    };
    use mondrian_timeline::{SequenceCollection, SequenceSettings};

    #[derive(Debug, Clone)]
    struct RepeatedDiamondOwner {
        label: String,
        shared_child: AuthoringList<String>,
    }

    impl AuthoringFootprint for RepeatedDiamondOwner {
        fn collect_authoring_footprint(
            &self,
            collector: &mut AuthoringFootprintCollector,
        ) -> std::result::Result<(), mondrian_core::AuthoringFootprintError> {
            collector.collect(&self.label)?;
            collector.collect(&self.shared_child)?;
            collector.collect(&self.shared_child)
        }
    }

    #[derive(Debug, Clone)]
    struct MutableDescriptorOwner {
        label: String,
        children: Vec<AuthoringList<String>>,
    }

    impl AuthoringFootprint for MutableDescriptorOwner {
        fn collect_authoring_footprint(
            &self,
            collector: &mut AuthoringFootprintCollector,
        ) -> std::result::Result<(), mondrian_core::AuthoringFootprintError> {
            collector.collect(&self.label)?;
            collector.collect(&self.children)
        }
    }

    fn apply_test_footprint_plan(
        index: &mut RetainedFootprintIndex,
        descriptor_cache: &AuthoringFootprintDescriptorCache,
        plan: PreparedFootprintPlan,
    ) {
        index.try_reserve_for_plan(&plan).expect("reserve retained footprint index");
        descriptor_cache
            .validate_and_reserve(&plan.cache_insertions)
            .expect("reserve descriptor cache");
        plan.apply(index, descriptor_cache);
    }

    fn cold_diamond_charge(values: &[&AuthoringSnapshot<RepeatedDiamondOwner>]) -> usize {
        let mut collector = AuthoringFootprintCollector::new();
        for value in values {
            collector.collect(*value).expect("collect cold diamond root");
        }
        usize::try_from(collector.finish().total_bytes()).expect("cold charge fits usize")
    }

    fn project_with_sequence() -> ProjectDocument {
        ProjectDocument::new(
            "History",
            SequenceCollection::new(Sequence::new("Sequence")),
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        )
    }

    fn renamed(sequence: &Sequence, name: &str) -> Sequence {
        let mut after = sequence.clone();
        after.name = name.to_owned();
        after
    }

    fn command_retained_bytes(command: AuthoringCommand) -> usize {
        let entry =
            AuthoringHistoryEntry::compile(command, AuthoringFootprintDescriptorCache::new())
                .expect("compile command footprint");
        let mut undo_stack = VecDeque::new();
        undo_stack.push_back(Arc::new(entry));
        retained_bytes_for_stacks(&undo_stack, &VecDeque::new()).expect("measure command")
    }

    fn assert_incremental_matches_cold(history: &AuthoringHistory) {
        let cold = retained_bytes_for_stacks(&history.state.undo_stack, &history.state.redo_stack)
            .expect("cold History footprint");
        assert_eq!(history.state.retained_bytes, cold);
        assert_eq!(
            history.footprint_index.total_bytes().expect("indexed History footprint"),
            cold
        );
    }

    fn apply_restore(
        document: &mut ProjectDocument,
        restore: PreparedAuthoringHistoryRestore,
    ) -> AuthoringHistoryApplication {
        match restore {
            PreparedAuthoringHistoryRestore::Sequence { sequence, application } => {
                let target =
                    document.sequences.sequence_mut(sequence.id).expect("restore target Sequence");
                *target = sequence;
                application
            }
            PreparedAuthoringHistoryRestore::Project { document: restored, application } => {
                *document = restored;
                application
            }
        }
    }

    #[test]
    fn entry_metadata_charge_covers_inline_arc_slot_and_dynamic_payload() {
        let before = project_with_sequence();
        let mut after = before.clone();
        after.meta.name = "After".to_owned();
        let description = "project metadata change";
        let command =
            AuthoringCommand::project(description, &[], &before, &after).expect("project command");
        let expected = size_of::<AuthoringHistoryEntry>()
            + AUTHORING_HISTORY_ARC_ALLOWANCE_WORDS * size_of::<usize>()
            + size_of::<Arc<AuthoringHistoryEntry>>()
            + description.len();

        assert_eq!(
            AuthoringHistoryEntry::conservative_metadata_bytes(&command).expect("metadata charge"),
            expected as u64
        );
        let entry =
            AuthoringHistoryEntry::compile(command, AuthoringFootprintDescriptorCache::new())
                .expect("compile entry");
        assert_eq!(entry.metadata_bytes, expected as u64);
        let mut undo_stack = VecDeque::new();
        undo_stack.push_back(Arc::new(entry));
        let cold = retained_bytes_for_stacks(&undo_stack, &VecDeque::new()).expect("cold charge");
        let empty_index = RetainedFootprintIndex::empty();
        let mut overlay = FootprintOverlay::new(&empty_index);
        overlay.add_entry(undo_stack.front().expect("entry")).expect("index entry");
        assert_eq!(overlay.total_bytes().expect("indexed charge"), cold);
    }

    #[test]
    fn entry_budget_evicts_oldest_undo_commands() {
        let document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        let third = renamed(&first, "Third");
        let fourth = renamed(&first, "Fourth");
        let mut history = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(2, usize::MAX));

        history.record_sequence("second", &first, &second).expect("record second");
        history.record_sequence("third", &second, &third).expect("record third");
        let outcome = history.record_sequence("fourth", &third, &fourth).expect("record fourth");

        assert!(outcome.retained);
        assert_eq!(outcome.budget_evicted_entries, 1);
        assert_eq!(history.diagnostics().undo_entries, 2);
        assert_eq!(history.undo_description(), Some("fourth"));
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn eviction_reports_the_actual_candidate_charge_difference() {
        let document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        let third = renamed(&second, "Third");
        let fourth = renamed(&third, "Fourth");
        let mut unlimited = AuthoringHistory::default();
        unlimited.record_sequence("second", &first, &second).expect("record second");
        unlimited.record_sequence("third", &second, &third).expect("record third");
        unlimited.record_sequence("fourth", &third, &fourth).expect("record fourth");
        let full_candidate_charge = unlimited.diagnostics().retained_bytes;
        let mut bounded = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(2, usize::MAX));
        bounded.record_sequence("second", &first, &second).expect("record second");
        bounded.record_sequence("third", &second, &third).expect("record third");

        let outcome = bounded.record_sequence("fourth", &third, &fourth).expect("record fourth");
        let retained_charge = bounded.diagnostics().retained_bytes;

        assert_eq!(outcome.budget_evicted_entries, 1);
        assert_eq!(
            outcome.budget_evicted_bytes,
            full_candidate_charge - retained_charge
        );
        assert_eq!(
            bounded.diagnostics().budget_evicted_bytes,
            outcome.budget_evicted_bytes as u64
        );
        assert_incremental_matches_cold(&bounded);
    }

    #[test]
    fn oversize_command_commits_without_retaining_undo() {
        let document = project_with_sequence();
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "A deliberately larger renamed Sequence");
        let command = AuthoringCommand::sequence("rename", &before, &after).expect("command");
        let retained_bytes = command_retained_bytes(command);
        let mut history = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(
            10,
            retained_bytes.saturating_sub(1),
        ));

        let prepared = history.prepare_sequence_record("rename", &before, &after).expect("prepare");
        assert!(prepared.footprint_plan.cache_insertions.is_empty());
        let outcome = history.commit_prepared_record(prepared).expect("commit");

        assert!(!outcome.retained);
        assert!(!history.can_undo());
        assert_eq!(history.diagnostics().oversize_dropped_entries, 1);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn zero_entry_budget_never_publishes_command_descriptors() {
        let document = project_with_sequence();
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "After");
        let mut history = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(0, usize::MAX));

        let prepared = history.prepare_sequence_record("rename", &before, &after).expect("prepare");
        assert!(prepared.footprint_plan.cache_insertions.is_empty());
        let outcome = history.commit_prepared_record(prepared).expect("commit");

        assert!(!outcome.retained);
        assert!(history.footprint_index.reference_counts.is_empty());
        let diagnostics = history.diagnostics();
        assert_eq!(diagnostics.retained_bytes, 0);
        assert_eq!(diagnostics.retention_disabled_entries, 1);
        assert_eq!(diagnostics.oversize_dropped_entries, 0);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn oversize_new_command_establishes_a_snapshot_history_barrier() {
        let document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        let first_command =
            AuthoringCommand::sequence("second", &first, &second).expect("first command");
        let retained_bytes = command_retained_bytes(first_command);
        let mut history =
            AuthoringHistory::with_budget(AuthoringHistoryBudget::new(10, retained_bytes));
        history.record_sequence("second", &first, &second).expect("record first");
        let third = renamed(&second, &"oversize".repeat(retained_bytes));

        let outcome =
            history.record_sequence("oversize", &second, &third).expect("record oversize");

        assert!(!outcome.retained);
        assert_eq!(outcome.budget_evicted_entries, 0);
        assert_eq!(outcome.barrier_discarded_entries, 1);
        assert_eq!(history.diagnostics().undo_entries, 0);
        assert_eq!(history.undo_description(), None);
        assert!(!history.can_undo());
        assert_eq!(history.diagnostics().budget_evicted_entries, 0);
        assert_eq!(history.diagnostics().barrier_discarded_entries, 1);
        assert_eq!(history.diagnostics().oversize_dropped_entries, 1);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn unretained_barrier_discards_both_stacks_and_never_undoes_across_the_gap() {
        let mut document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        let third = renamed(&second, "Third");

        let mut measuring = AuthoringHistory::default();
        measuring.record_sequence("second", &first, &second).expect("measure second");
        measuring.record_sequence("third", &second, &third).expect("measure third");
        let retained_two_command_charge = measuring.diagnostics().retained_bytes;

        let mut history = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(
            10,
            retained_two_command_charge,
        ));
        history.record_sequence("second", &first, &second).expect("record second");
        history.record_sequence("third", &second, &third).expect("record third");
        *document.sequences.active_mut().expect("active Sequence") = third;

        let restore = history.prepare_undo(&document).expect("prepare Undo").expect("Undo entry");
        apply_restore(&mut document, restore);
        history.commit_prepared_undo().expect("commit Undo");
        assert_eq!(history.diagnostics().undo_entries, 1);
        assert_eq!(history.diagnostics().redo_entries, 1);
        assert_incremental_matches_cold(&history);

        let barrier_before = document.sequences.active().expect("active Sequence").clone();
        let barrier_after = renamed(
            &barrier_before,
            &"unretained-barrier".repeat(retained_two_command_charge),
        );
        let outcome = history
            .record_sequence("unretained barrier", &barrier_before, &barrier_after)
            .expect("record unretained barrier");
        *document.sequences.active_mut().expect("active Sequence") = barrier_after.clone();

        assert!(!outcome.retained);
        assert_eq!(outcome.branch_discarded_entries, 1);
        assert_eq!(outcome.barrier_discarded_entries, 1);
        let diagnostics = history.diagnostics();
        assert_eq!(diagnostics.undo_entries, 0);
        assert_eq!(diagnostics.redo_entries, 0);
        assert_eq!(diagnostics.retained_bytes, 0);
        assert!(history.footprint_index.reference_counts.is_empty());
        assert!(history.descriptor_cache.is_empty());
        assert_incremental_matches_cold(&history);

        history.budget = AuthoringHistoryBudget::new(10, usize::MAX);
        let after_barrier = renamed(&barrier_after, "After barrier");
        let outcome = history
            .record_sequence("after barrier", &barrier_after, &after_barrier)
            .expect("record post-barrier command");
        assert!(outcome.retained);
        *document.sequences.active_mut().expect("active Sequence") = after_barrier;
        let restore = history.prepare_undo(&document).expect("prepare Undo").expect("Undo entry");
        apply_restore(&mut document, restore);
        history.commit_prepared_undo().expect("commit Undo");

        assert_eq!(
            document.sequences.active().expect("active Sequence").name,
            barrier_after.name
        );
        assert_ne!(
            document.sequences.active().expect("active Sequence").name,
            barrier_before.name
        );
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn preparing_an_oversize_command_is_atomic_until_commit() {
        let document = project_with_sequence();
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "A deliberately larger renamed Sequence");
        let command = AuthoringCommand::sequence("rename", &before, &after).expect("command");
        let retained_bytes = command_retained_bytes(command);
        let mut history = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(
            10,
            retained_bytes.saturating_sub(1),
        ));
        let diagnostics_before = history.diagnostics();

        let prepared = history
            .prepare_sequence_record("rename", &before, &after)
            .expect("prepare command");

        assert_eq!(history.diagnostics(), diagnostics_before);
        let outcome = history.commit_prepared_record(prepared).expect("commit command");
        assert!(!outcome.retained);
        assert_eq!(history.diagnostics().oversize_dropped_entries, 1);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn sequence_restore_is_detached_and_preserves_unrelated_sequences() {
        let mut document = project_with_sequence();
        let unrelated = Sequence::new("Unrelated");
        let unrelated_id = unrelated.id;
        document.sequences.sequences.push(unrelated.clone());
        let unrelated_before = serde_json::to_vec(&unrelated).expect("serialize unrelated");
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "After");
        *document.sequences.active_mut().expect("active Sequence") = after.clone();
        let mut history = AuthoringHistory::default();
        history.record_sequence("rename", &before, &after).expect("record");

        let document_before_prepare = serde_json::to_vec(&document).expect("serialize document");
        let history_before_prepare = history.diagnostics();
        let restore = history.prepare_undo(&document).expect("prepare Undo").expect("Undo entry");
        assert_eq!(
            serde_json::to_vec(&document).expect("serialize prepared document"),
            document_before_prepare
        );
        assert_eq!(history.diagnostics(), history_before_prepare);
        let retained_before_move = history.diagnostics().retained_bytes;
        let retained_references = history.footprint_index.reference_counts.clone();
        let retained_entry =
            Arc::as_ptr(history.state.undo_stack.back().expect("retained Undo entry"));
        let application = apply_restore(&mut document, restore);
        history.commit_prepared_undo().expect("commit Undo");
        assert_eq!(history.diagnostics().retained_bytes, retained_before_move);
        assert_eq!(
            history.footprint_index.reference_counts,
            retained_references
        );
        assert_eq!(
            Arc::as_ptr(history.state.redo_stack.back().expect("retained Redo entry")),
            retained_entry
        );
        assert_incremental_matches_cold(&history);

        assert_eq!(
            document.sequences.active().expect("active Sequence").name,
            before.name
        );
        let preserved = document.sequences.sequence(unrelated_id).expect("unrelated Sequence");
        assert_eq!(
            serde_json::to_vec(preserved).expect("serialize preserved unrelated Sequence"),
            unrelated_before
        );
        assert_eq!(application.affected_sequence_ids, vec![before.id]);
        assert!(!application.project_wide);
        assert!(history.can_redo());

        let restore = history.prepare_redo(&document).expect("prepare Redo").expect("Redo entry");
        apply_restore(&mut document, restore);
        history.commit_prepared_redo().expect("commit Redo");
        assert_eq!(history.diagnostics().retained_bytes, retained_before_move);
        assert_eq!(
            history.footprint_index.reference_counts,
            retained_references
        );
        assert_eq!(
            Arc::as_ptr(history.state.undo_stack.back().expect("retained Undo entry")),
            retained_entry
        );
        assert_incremental_matches_cold(&history);
        assert_eq!(
            document.sequences.active().expect("active Sequence").name,
            after.name
        );
    }

    #[test]
    fn a_new_branch_discards_prepared_redo_history() {
        let mut document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        *document.sequences.active_mut().expect("active Sequence") = second.clone();
        let mut history = AuthoringHistory::default();
        history.record_sequence("second", &first, &second).expect("record second");

        let restore = history.prepare_undo(&document).expect("prepare Undo").expect("Undo entry");
        apply_restore(&mut document, restore);
        history.commit_prepared_undo().expect("commit Undo");
        let branch_before = document.sequences.active().expect("active Sequence").clone();
        let branch_after = renamed(&branch_before, "Branch");
        let outcome = history
            .record_sequence("branch", &branch_before, &branch_after)
            .expect("record branch");

        assert_eq!(outcome.branch_discarded_entries, 1);
        assert!(!history.can_redo());
        assert_eq!(history.diagnostics().branch_discarded_entries, 1);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn failed_prepare_does_not_move_history_or_mutate_document() {
        let mut document = project_with_sequence();
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "After");
        *document.sequences.active_mut().expect("active Sequence") = after.clone();
        let mut history = AuthoringHistory::default();
        history.record_sequence("rename", &before, &after).expect("record");

        let replacement = Sequence::new("Replacement");
        let replacement_id = replacement.id;
        document.sequences.sequences = vec![replacement].into();
        document.sequences.active_sequence_id = replacement_id;
        document.sequences.default_sequence_id = replacement_id;
        let history_before = history.diagnostics();
        let document_before = serde_json::to_vec(&document).expect("serialize document");

        history.prepare_undo(&document).expect_err("missing target must fail");

        assert_eq!(history.diagnostics(), history_before);
        assert_eq!(
            serde_json::to_vec(&document).expect("serialize document"),
            document_before
        );
    }

    #[test]
    fn sequence_restore_rejects_diverged_source_without_moving_history() {
        let mut document = project_with_sequence();
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "After");
        *document.sequences.active_mut().expect("active Sequence") = after.clone();
        let mut history = AuthoringHistory::default();
        history.record_sequence("rename", &before, &after).expect("record Sequence");
        document.sequences.active_mut().expect("active Sequence").name =
            "Bypassed History".to_owned();
        let history_before = history.diagnostics();
        let document_before = serde_json::to_vec(&document).expect("serialize document");

        let error =
            history.prepare_undo(&document).expect_err("diverged source endpoint must fail");

        assert!(error.to_string().contains("contiguous source endpoint"));
        assert_eq!(history.diagnostics(), history_before);
        assert_eq!(
            serde_json::to_vec(&document).expect("serialize document"),
            document_before
        );
    }

    #[test]
    fn project_restore_preserves_current_persistence_revision() {
        let before = project_with_sequence();
        let mut after = before.clone();
        let second = Sequence::new("Second");
        let second_id = second.id;
        after.sequences.sequences.push(second);
        after.document_revision = 9;
        let affected = vec![second_id];
        let mut history = AuthoringHistory::default();
        history
            .record_project("structure", &affected, &before, &after)
            .expect("record Project");
        let mut current = after;
        current.document_revision = 42;
        let mut expected = before;
        expected.document_revision = 42;

        let restore = history.prepare_undo(&current).expect("prepare Undo").expect("Undo entry");
        let application = apply_restore(&mut current, restore);

        assert_eq!(current, expected);
        assert_eq!(application.affected_sequence_ids, affected);
        assert!(application.project_wide);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn project_restore_preserves_save_timestamp_across_undo_and_redo() {
        let before = project_with_sequence();
        let mut after = before.clone();
        after.meta.name = "After".to_owned();
        let mut history = AuthoringHistory::default();
        history
            .record_project("rename Project", &[], &before, &after)
            .expect("record Project");
        let mut current = after.clone();
        let saved_at = current.meta.updated_at + chrono::Duration::seconds(1);
        current.meta.updated_at = saved_at;

        let restore = history.prepare_undo(&current).expect("prepare Undo").expect("Undo entry");
        apply_restore(&mut current, restore);
        history.commit_prepared_undo().expect("commit Undo");
        assert_eq!(current.meta.name, before.meta.name);
        assert_eq!(current.meta.updated_at, saved_at);
        let saved_after_undo_at = saved_at + chrono::Duration::seconds(1);
        current.meta.updated_at = saved_after_undo_at;

        let restore = history.prepare_redo(&current).expect("prepare Redo").expect("Redo entry");
        apply_restore(&mut current, restore);
        history.commit_prepared_redo().expect("commit Redo");
        assert_eq!(current.meta.name, after.meta.name);
        assert_eq!(current.meta.updated_at, saved_after_undo_at);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn project_restore_preserves_valid_current_navigation_state() {
        let mut before = project_with_sequence();
        let alternate = Sequence::new("Alternate");
        let alternate_id = alternate.id;
        before.sequences.add_sequence(alternate).expect("add alternate Sequence");
        let mut after = before.clone();
        after.meta.name = "After".to_owned();
        let mut history = AuthoringHistory::default();
        history
            .record_project("rename Project", &[], &before, &after)
            .expect("record Project");
        let mut current = after;
        current.sequences.active_sequence_id = alternate_id;

        let restore = history.prepare_undo(&current).expect("prepare Undo").expect("Undo entry");
        apply_restore(&mut current, restore);

        assert_eq!(current.meta.name, before.meta.name);
        assert_eq!(current.sequences.active_sequence_id, alternate_id);
    }

    #[test]
    fn project_restore_rejects_diverged_source_without_moving_history() {
        let before = project_with_sequence();
        let sequence_id = before.sequences.active_sequence_id;
        let mut after = before.clone();
        after.sequences.sequence_mut(sequence_id).expect("active Sequence").name =
            "After".to_owned();
        let mut history = AuthoringHistory::default();
        history
            .record_project("change Sequence", &[sequence_id], &before, &after)
            .expect("record Project");
        let mut current = after;
        current.sequences.sequence_mut(sequence_id).expect("active Sequence").name =
            "Bypassed History".to_owned();
        let history_before = history.diagnostics();
        let document_before = serde_json::to_vec(&current).expect("serialize document");

        let error = history.prepare_undo(&current).expect_err("diverged source endpoint must fail");

        assert!(error.to_string().contains("contiguous source endpoint"));
        assert_eq!(history.diagnostics(), history_before);
        assert_eq!(
            serde_json::to_vec(&current).expect("serialize document"),
            document_before
        );
    }

    #[test]
    fn project_restore_rejects_diverged_project_state_without_moving_history() {
        let before = project_with_sequence();
        let mut after = before.clone();
        after.meta.name = "After".to_owned();
        let mut history = AuthoringHistory::default();
        history
            .record_project("rename Project", &[], &before, &after)
            .expect("record Project");
        let mut current = after;
        current.meta.author = "Bypassed History".to_owned();
        let history_before = history.diagnostics();

        let error = history
            .prepare_undo(&current)
            .expect_err("diverged Project source endpoint must fail");

        assert!(error.to_string().contains("contiguous source endpoint"));
        assert_eq!(history.diagnostics(), history_before);
    }

    #[test]
    fn project_restore_roundtrips_add_remove_order_and_default() {
        let mut before = project_with_sequence();
        let primary_id = before.sequences.active_sequence_id;
        let second = Sequence::new("Second");
        let second_id = second.id;
        let removed = Sequence::new("Removed");
        let removed_id = removed.id;
        before.sequences.add_sequence(second).expect("add second Sequence");
        before.sequences.add_sequence(removed).expect("add removable Sequence");
        let primary = before.sequences.sequence(primary_id).expect("primary Sequence").clone();
        let second = before.sequences.sequence(second_id).expect("second Sequence").clone();
        let removed = before.sequences.sequence(removed_id).expect("removable Sequence").clone();
        before.sequences.sequences =
            AuthoringList::from(vec![removed, primary.clone(), second.clone()]);
        before.sequences.default_sequence_id = removed_id;
        before.sequences.active_sequence_id = primary_id;

        let mut after = before.clone();
        let added = Sequence::new("Added");
        let added_id = added.id;
        after.sequences.sequences = AuthoringList::from(vec![added, second, primary]);
        after.sequences.default_sequence_id = added_id;
        let mut history = AuthoringHistory::default();
        history
            .record_project("replace Sequence", &[removed_id, added_id], &before, &after)
            .expect("record structural Project change");
        let mut current = after.clone();

        let restore = history.prepare_undo(&current).expect("prepare Undo").expect("Undo entry");
        apply_restore(&mut current, restore);
        history.commit_prepared_undo().expect("commit Undo");
        assert_eq!(current, before);

        let restore = history.prepare_redo(&current).expect("prepare Redo").expect("Redo entry");
        apply_restore(&mut current, restore);
        history.commit_prepared_redo().expect("commit Redo");
        assert_eq!(current, after);
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn project_restore_point_excludes_unaffected_sequence_payload() {
        let mut light_before = project_with_sequence();
        let unrelated = Sequence::new("Unrelated");
        let unrelated_id = unrelated.id;
        light_before.sequences.add_sequence(unrelated).expect("add unrelated Sequence");
        let mut light_after = light_before.clone();
        light_after.meta.name = "After".to_owned();
        let light_charge = command_retained_bytes(
            AuthoringCommand::project("metadata", &[], &light_before, &light_after)
                .expect("light Project command"),
        );

        let mut heavy_before = light_before;
        heavy_before
            .sequences
            .sequence_mut(unrelated_id)
            .expect("unrelated Sequence")
            .name = "unrelated".repeat(512 * 1024);
        let mut heavy_after = heavy_before.clone();
        heavy_after.meta.name = "After".to_owned();
        let heavy_charge = command_retained_bytes(
            AuthoringCommand::project("metadata", &[], &heavy_before, &heavy_after)
                .expect("heavy Project command"),
        );

        assert_eq!(heavy_charge, light_charge);
    }

    #[test]
    fn project_restore_reuses_unaffected_sequence_allocations() {
        let mut before = project_with_sequence();
        let primary_id = before.sequences.active_sequence_id;
        let unrelated = Sequence::new("Unrelated");
        let unrelated_id = unrelated.id;
        before.sequences.add_sequence(unrelated).expect("add unrelated Sequence");

        let mut after = before.clone();
        after.sequences.sequence_mut(primary_id).expect("primary Sequence").name =
            "Primary changed".to_owned();
        let nested = Sequence::new("Nested");
        let nested_id = nested.id;
        after.sequences.add_sequence(nested).expect("add nested Sequence");
        let affected = vec![primary_id, nested_id];
        let unrelated_allocation = after
            .sequences
            .sequence(unrelated_id)
            .expect("unrelated Sequence")
            .video_tracks
            .allocation_id();
        let mut history = AuthoringHistory::default();
        history
            .record_project("precompose shape", &affected, &before, &after)
            .expect("record scoped Project command");

        let restore = history.prepare_undo(&after).expect("prepare Undo").expect("Undo entry");
        let PreparedAuthoringHistoryRestore::Project { document: restored, .. } = restore else {
            panic!("expected Project restore");
        };
        assert_eq!(restored, before);
        assert_eq!(
            restored
                .sequences
                .sequence(unrelated_id)
                .expect("restored unrelated Sequence")
                .video_tracks
                .allocation_id(),
            unrelated_allocation
        );
    }

    #[test]
    fn project_restore_scope_must_match_changed_sequences() {
        let before = project_with_sequence();
        let primary_id = before.sequences.active_sequence_id;
        let mut after = before.clone();
        after.sequences.sequence_mut(primary_id).expect("primary Sequence").name =
            "Changed".to_owned();

        let error = AuthoringCommand::project("forged scope", &[], &before, &after)
            .expect_err("missing changed Sequence must fail");
        assert!(error.to_string().contains("scope does not match"));
    }

    #[test]
    fn sequence_snapshots_deduplicate_shared_cow_allocations() {
        let document = project_with_sequence();
        let before = document.sequences.active().expect("active Sequence").clone();
        let after = renamed(&before, "After");
        assert!(before.video_tracks.shares_allocation_with(&after.video_tracks));
        let command = AuthoringCommand::sequence("rename", &before, &after).expect("command");
        let AuthoringCommand::Sequence { before, after, .. } = &command else {
            panic!("expected Sequence command");
        };

        let mut before_collector = AuthoringFootprintCollector::new();
        before_collector.collect(before).expect("before footprint");
        let before_bytes = before_collector.finish().total_bytes();
        let mut after_collector = AuthoringFootprintCollector::new();
        after_collector.collect(after).expect("after footprint");
        let after_bytes = after_collector.finish().total_bytes();
        let mut combined_collector = AuthoringFootprintCollector::new();
        combined_collector.collect(before).expect("combined before footprint");
        combined_collector.collect(after).expect("combined after footprint");
        let combined = combined_collector.finish();

        assert!(combined.total_bytes() < before_bytes + after_bytes);
        assert_eq!(
            before.value().video_tracks.allocation_id(),
            after.value().video_tracks.allocation_id()
        );
    }

    #[test]
    fn thirty_two_light_edits_share_heavy_sequence_lists() {
        let document = project_with_sequence();
        let mut before = document.sequences.active().expect("active Sequence").clone();
        let shared_video_tracks = before.video_tracks.allocation_id();
        let mut history = AuthoringHistory::default();

        for index in 0..32 {
            let after = renamed(&before, &format!("Edit {index}"));
            let outcome = history
                .record_sequence(format!("edit {index}"), &before, &after)
                .expect("record light edit");
            assert!(outcome.retained);
            before = after;
            assert_incremental_matches_cold(&history);
        }

        assert_eq!(history.diagnostics().undo_entries, 32);
        for command in &history.state.undo_stack {
            let AuthoringCommand::Sequence { before, after, .. } = &command.command else {
                panic!("expected Sequence command");
            };
            assert_eq!(
                before.value().video_tracks.allocation_id(),
                shared_video_tracks
            );
            assert_eq!(
                after.value().video_tracks.allocation_id(),
                shared_video_tracks
            );
        }
        assert_eq!(
            history.footprint_index.reference_counts.get(&shared_video_tracks).copied(),
            Some(64)
        );
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn eviction_reclaims_only_descriptors_whose_reference_count_reaches_zero() {
        let document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        let third = renamed(&second, "Third");
        let shared_video_tracks = first.video_tracks.allocation_id();
        let mut history = AuthoringHistory::with_budget(AuthoringHistoryBudget::new(1, usize::MAX));
        history.record_sequence("second", &first, &second).expect("record first");
        let evicted_root_ids = history.state.undo_stack[0]
            .footprint_roots
            .each_ref()
            .map(AuthoringAllocationFootprint::allocation_id);

        let prepared = history
            .prepare_sequence_record("third", &second, &third)
            .expect("prepare eviction");

        assert!(evicted_root_ids
            .iter()
            .all(|allocation_id| prepared.footprint_plan.cache_removals.contains(allocation_id)));
        assert!(!prepared.footprint_plan.cache_removals.contains(&shared_video_tracks));
        history.commit_prepared_record(prepared).expect("commit eviction");
        assert!(evicted_root_ids.iter().all(|allocation_id| !history
            .footprint_index
            .reference_counts
            .contains_key(allocation_id)));
        assert_eq!(
            history.footprint_index.reference_counts.get(&shared_video_tracks).copied(),
            Some(2)
        );
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn stale_prepared_record_cannot_replace_newer_history_state() {
        let document = project_with_sequence();
        let first = document.sequences.active().expect("active Sequence").clone();
        let second = renamed(&first, "Second");
        let third = renamed(&second, "Third");
        let mut history = AuthoringHistory::default();
        let stale = history
            .prepare_sequence_record("second", &first, &second)
            .expect("stale candidate");
        let stale_allocation_ids = stale
            .footprint_plan
            .cache_insertions
            .iter()
            .map(AuthoringAllocationFootprint::allocation_id)
            .collect::<Vec<_>>();
        let current = history
            .prepare_sequence_record("third", &second, &third)
            .expect("current candidate");
        let current_allocation_ids = current
            .footprint_plan
            .cache_insertions
            .iter()
            .map(AuthoringAllocationFootprint::allocation_id)
            .collect::<Vec<_>>();
        history.commit_prepared_record(current).expect("commit current candidate");
        let diagnostics_before_stale_commit = history.diagnostics();

        let error = history
            .commit_prepared_record(stale)
            .expect_err("stale candidate must be rejected");

        assert!(error.to_string().contains("stale"));
        assert_eq!(history.diagnostics(), diagnostics_before_stale_commit);
        assert!(stale_allocation_ids.iter().any(|allocation_id| {
            !current_allocation_ids.contains(allocation_id)
                && !history.footprint_index.reference_counts.contains_key(allocation_id)
        }));
        assert_incremental_matches_cold(&history);
    }

    #[test]
    fn repeated_edge_diamond_index_matches_cold_through_addition_and_reclamation() {
        let shared_child = AuthoringList::from(vec![String::from("shared leaf payload")]);
        let shared_child_id = shared_child.allocation_id();
        let left = AuthoringSnapshot::new(RepeatedDiamondOwner {
            label: String::from("left"),
            shared_child: shared_child.clone(),
        });
        let right = AuthoringSnapshot::new(RepeatedDiamondOwner {
            label: String::from("right"),
            shared_child,
        });
        let mut collector = AuthoringFootprintCollector::new();
        collector.collect(&left).expect("collect left descriptor");
        collector.collect(&right).expect("collect right descriptor");
        let roots: [AuthoringAllocationFootprint; 2] = collector
            .finish_graph()
            .into_roots()
            .try_into()
            .expect("diamond traversal has two roots");
        let [left_root, right_root] = roots;
        assert_eq!(left_root.children().len(), 2);
        assert_eq!(right_root.children().len(), 2);
        assert!(left_root
            .children()
            .iter()
            .chain(right_root.children())
            .all(|child| child.allocation_id() == shared_child_id));

        let descriptor_cache = AuthoringFootprintDescriptorCache::new();
        let mut index = RetainedFootprintIndex::empty();

        let mut add_left = FootprintOverlay::new(&index);
        add_left.add_allocation(&left_root).expect("add left root");
        let add_left = add_left.finish().expect("finish left addition");
        assert_eq!(add_left.cache_insertions.len(), 2);
        apply_test_footprint_plan(&mut index, &descriptor_cache, add_left);
        assert_eq!(
            index.total_bytes().expect("left indexed charge"),
            cold_diamond_charge(&[&left])
        );
        assert_eq!(index.reference_counts.get(&left.allocation_id()), Some(&1));
        assert_eq!(index.reference_counts.get(&shared_child_id), Some(&2));

        let mut add_right = FootprintOverlay::new(&index);
        add_right.add_allocation(&right_root).expect("add right root");
        let add_right = add_right.finish().expect("finish right addition");
        assert_eq!(add_right.cache_insertions.len(), 1);
        apply_test_footprint_plan(&mut index, &descriptor_cache, add_right);
        assert_eq!(
            index.total_bytes().expect("diamond indexed charge"),
            cold_diamond_charge(&[&left, &right])
        );
        assert_eq!(index.reference_counts.get(&left.allocation_id()), Some(&1));
        assert_eq!(index.reference_counts.get(&right.allocation_id()), Some(&1));
        assert_eq!(index.reference_counts.get(&shared_child_id), Some(&4));

        let mut evict_left = FootprintOverlay::new(&index);
        evict_left.remove_allocation(&left_root).expect("evict left root");
        let evict_left = evict_left.finish().expect("finish left eviction");
        assert_eq!(evict_left.cache_removals, vec![left.allocation_id()]);
        apply_test_footprint_plan(&mut index, &descriptor_cache, evict_left);
        assert_eq!(
            index.total_bytes().expect("right-only indexed charge"),
            cold_diamond_charge(&[&right])
        );
        assert!(!index.reference_counts.contains_key(&left.allocation_id()));
        assert_eq!(index.reference_counts.get(&right.allocation_id()), Some(&1));
        assert_eq!(index.reference_counts.get(&shared_child_id), Some(&2));

        let mut reclaim_right = FootprintOverlay::new(&index);
        reclaim_right.remove_allocation(&right_root).expect("reclaim right root");
        let reclaim_right = reclaim_right.finish().expect("finish final reclamation");
        assert_eq!(reclaim_right.cache_removals.len(), 2);
        assert!(reclaim_right.cache_removals.contains(&right.allocation_id()));
        assert!(reclaim_right.cache_removals.contains(&shared_child_id));
        apply_test_footprint_plan(&mut index, &descriptor_cache, reclaim_right);
        assert_eq!(index.total_bytes().expect("empty indexed charge"), 0);
        assert!(index.reference_counts.is_empty());
    }

    #[test]
    fn active_cached_root_updates_only_its_root_count_until_final_release() {
        let shared_child = AuthoringList::from(vec![String::from("shared leaf payload")]);
        let shared_child_id = shared_child.allocation_id();
        let snapshot = AuthoringSnapshot::new(RepeatedDiamondOwner {
            label: String::from("root"),
            shared_child,
        });
        let mut collector = AuthoringFootprintCollector::new();
        collector.collect(&snapshot).expect("collect descriptor");
        let mut roots = collector.finish_graph().into_roots();
        let root = roots.pop().expect("root descriptor");
        assert!(roots.is_empty());

        let descriptor_cache = AuthoringFootprintDescriptorCache::new();
        let mut index = RetainedFootprintIndex::empty();
        let mut first_retain = FootprintOverlay::new(&index);
        first_retain.add_allocation(&root).expect("retain root");
        let first_retain = first_retain.finish().expect("finish first retention");
        apply_test_footprint_plan(&mut index, &descriptor_cache, first_retain);
        let retained_bytes = index.total_bytes().expect("initial retained bytes");
        assert_eq!(index.reference_counts.get(&shared_child_id), Some(&2));

        let mut duplicate_retain = FootprintOverlay::new(&index);
        duplicate_retain.add_allocation(&root).expect("retain cached root again");
        let duplicate_retain = duplicate_retain.finish().expect("finish duplicate retention");
        assert_eq!(
            duplicate_retain.reference_counts,
            vec![(snapshot.allocation_id(), 2)]
        );
        assert!(duplicate_retain.cache_insertions.is_empty());
        assert!(duplicate_retain.cache_removals.is_empty());
        apply_test_footprint_plan(&mut index, &descriptor_cache, duplicate_retain);
        assert_eq!(
            index.total_bytes().expect("duplicate retained bytes"),
            retained_bytes
        );
        assert_eq!(index.reference_counts.get(&shared_child_id), Some(&2));

        let mut partial_release = FootprintOverlay::new(&index);
        partial_release.remove_allocation(&root).expect("release one root handle");
        let partial_release = partial_release.finish().expect("finish partial release");
        assert_eq!(
            partial_release.reference_counts,
            vec![(snapshot.allocation_id(), 1)]
        );
        assert!(partial_release.cache_removals.is_empty());
        apply_test_footprint_plan(&mut index, &descriptor_cache, partial_release);
        assert_eq!(
            index.total_bytes().expect("partially retained bytes"),
            retained_bytes
        );
        assert_eq!(index.reference_counts.get(&shared_child_id), Some(&2));

        let mut final_release = FootprintOverlay::new(&index);
        final_release.remove_allocation(&root).expect("release final root handle");
        let final_release = final_release.finish().expect("finish final release");
        assert!(final_release.cache_removals.contains(&snapshot.allocation_id()));
        assert!(final_release.cache_removals.contains(&shared_child_id));
        apply_test_footprint_plan(&mut index, &descriptor_cache, final_release);
        assert!(index.reference_counts.is_empty());
        assert_eq!(index.total_bytes().expect("released bytes"), 0);
    }

    #[test]
    fn retained_index_rejects_removing_an_inactive_root() {
        let snapshot = AuthoringSnapshot::new(String::from("unretained"));
        let mut collector = AuthoringFootprintCollector::new();
        collector.collect(&snapshot).expect("collect descriptor");
        let root = collector.finish_graph().into_roots().pop().expect("root descriptor");
        let index = RetainedFootprintIndex::empty();
        let mut overlay = FootprintOverlay::new(&index);

        let error =
            overlay.remove_allocation(&root).expect_err("inactive removal must be rejected");

        assert!(error.to_string().contains("has no retained reference"));
        assert_eq!(index.total_bytes().expect("unchanged retained bytes"), 0);
        assert!(index.reference_counts.is_empty());
    }

    #[test]
    fn zero_reference_cache_retirement_allows_same_id_unique_mutation_to_recompile() {
        let existing_child = AuthoringList::from(vec![String::from("old child payload")]);
        let existing_child_id = existing_child.allocation_id();
        let mut owner = AuthoringList::from(vec![MutableDescriptorOwner {
            label: String::from("old owner"),
            children: vec![existing_child],
        }]);
        let owner_id = owner.allocation_id();
        let mut initial_collector = AuthoringFootprintCollector::new();
        initial_collector.collect(&owner).expect("collect initial owner");
        let initial_roots = initial_collector.finish_graph().into_roots();
        let initial_root = initial_roots.first().expect("initial owner root").clone();
        assert_eq!(initial_roots.len(), 1);
        assert_eq!(initial_root.children().len(), 1);

        let descriptor_cache = AuthoringFootprintDescriptorCache::new();
        let mut index = RetainedFootprintIndex::empty();
        let mut retain = FootprintOverlay::new(&index);
        retain.add_allocation(&initial_root).expect("retain initial owner");
        let retain = retain.finish().expect("finish initial retention");
        apply_test_footprint_plan(&mut index, &descriptor_cache, retain);
        assert_eq!(index.reference_counts.get(&owner_id), Some(&1));
        assert_eq!(index.reference_counts.get(&existing_child_id), Some(&1));

        let mut release = FootprintOverlay::new(&index);
        release.remove_allocation(&initial_root).expect("release initial owner");
        let release = release.finish().expect("finish initial release");
        assert!(release.cache_removals.contains(&owner_id));
        assert!(release.cache_removals.contains(&existing_child_id));
        apply_test_footprint_plan(&mut index, &descriptor_cache, release);
        assert!(index.reference_counts.is_empty());
        assert_eq!(index.total_bytes().expect("released index charge"), 0);

        owner.reserve(32);
        {
            let value = &mut owner[0];
            value.label.reserve(128);
            value.children[0].reserve(16);
            value.children[0][0].reserve(128);
            value
                .children
                .push(AuthoringList::from(vec![String::from("new child payload")]));
        }
        assert_eq!(owner.allocation_id(), owner_id);
        assert_eq!(owner[0].children[0].allocation_id(), existing_child_id);

        let mut cached_collector =
            AuthoringFootprintCollector::with_descriptor_cache(descriptor_cache);
        cached_collector
            .collect(&owner)
            .expect("recompile mutated same-ID owner without stale descriptor");
        let cached_graph = cached_collector.finish_graph();
        let cached_manifest = cached_graph.manifest().clone();
        let cached_roots = cached_graph.into_roots();
        let mutated_root = cached_roots.first().expect("mutated owner root");
        assert_eq!(cached_roots.len(), 1);
        assert_eq!(mutated_root.allocation_id(), owner_id);
        assert_ne!(mutated_root.direct_bytes(), initial_root.direct_bytes());
        assert_ne!(
            mutated_root.exclusive_bytes(),
            initial_root.exclusive_bytes()
        );
        assert_eq!(mutated_root.children().len(), 2);
        let mutated_existing_child = mutated_root
            .children()
            .iter()
            .find(|child| child.allocation_id() == existing_child_id)
            .expect("mutated existing child descriptor");
        let initial_existing_child = &initial_root.children()[0];
        assert_ne!(
            mutated_existing_child.direct_bytes(),
            initial_existing_child.direct_bytes()
        );
        assert_ne!(
            mutated_existing_child.exclusive_bytes(),
            initial_existing_child.exclusive_bytes()
        );

        let mut cold_collector = AuthoringFootprintCollector::new();
        cold_collector.collect(&owner).expect("cold collect mutated owner");
        assert_eq!(cached_manifest, cold_collector.finish());
    }
}
