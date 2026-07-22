//! Canonical project authoring session.
//!
//! This Module owns the sole mutable `ProjectDocument`, project-wide Undo/Redo,
//! editor navigation, and the generation relation between authoring and durable
//! persistence. UI and execution Adapters receive read-only or immutable snapshots.

use crate::history::{AuthoringHistory, AuthoringHistoryRecordOutcome};
use mondrian_assets::AssetLibrary;
use mondrian_core::{
    AssetId, MondrianError, ProjectId, ProjectMeta, Result, SequenceId, SequenceRevision,
};
use mondrian_project::ProjectDocument;
use mondrian_timeline::Sequence;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Process-local identity of one open authoring lifetime.
///
/// Project identity is intentionally insufficient: closing and reopening the
/// same Project must reject completions produced by the previous in-memory
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthoringSessionId(Uuid);

impl AuthoringSessionId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Monotonic identity of one committed in-memory author state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorGeneration(u64);

impl AuthorGeneration {
    /// First valid author generation in an open session.
    pub const INITIAL: Self = Self(1);

    /// Return the numeric generation for diagnostics and persistence manifests.
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self> {
        self.0.checked_add(1).map(Self).ok_or_else(|| {
            session_error("advance_author_generation", "author generation exhausted")
        })
    }
}

/// Immutable input to a persistence request.
#[derive(Clone)]
pub struct AuthoringSnapshot {
    /// Open-session identity that owns this snapshot.
    pub session_id: AuthoringSessionId,
    /// Exact author generation represented by this snapshot.
    pub generation: AuthorGeneration,
    /// Exact SQLite connection revision captured with the author document.
    pub asset_library_revision: u64,
    /// Validated canonical project document.
    pub document: ProjectDocument,
    /// Project asset library whose SQLite state must be snapshotted consistently.
    pub asset_library: Arc<AssetLibrary>,
}

/// Result of one committed authoring transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringCommit {
    /// New canonical author generation.
    pub generation: AuthorGeneration,
    /// Sequence identities whose execution snapshots must be invalidated.
    pub changed_sequence_ids: Vec<SequenceId>,
    /// Whether the transaction changed project-wide semantics or collection structure.
    pub project_wide: bool,
    /// Whether the transaction was retained by the bounded Undo history.
    pub undo_retained: bool,
}

/// Sole mutable authority for one open project's authored state.
pub struct AuthoringSession {
    session_id: AuthoringSessionId,
    document: ProjectDocument,
    project_file: PathBuf,
    runtime_root: PathBuf,
    asset_library: Arc<AssetLibrary>,
    navigation_stack: Vec<SequenceId>,
    history: AuthoringHistory,
    author_generation: AuthorGeneration,
    saved_generation: Option<AuthorGeneration>,
    saved_asset_library_revision: Option<u64>,
    autosaved_generation: Option<AuthorGeneration>,
    autosaved_asset_library_revision: Option<u64>,
}

impl AuthoringSession {
    /// Open a validated document whose current generation is already durably saved.
    pub fn open_saved(
        document: ProjectDocument,
        project_file: PathBuf,
        runtime_root: PathBuf,
        asset_library: Arc<AssetLibrary>,
    ) -> Result<Self> {
        Self::new(document, project_file, runtime_root, asset_library, true)
    }

    /// Create a validated new document that has not yet been durably published.
    pub fn new_unsaved(
        document: ProjectDocument,
        project_file: PathBuf,
        runtime_root: PathBuf,
        asset_library: Arc<AssetLibrary>,
    ) -> Result<Self> {
        Self::new(document, project_file, runtime_root, asset_library, false)
    }

    fn new(
        document: ProjectDocument,
        project_file: PathBuf,
        runtime_root: PathBuf,
        asset_library: Arc<AssetLibrary>,
        saved: bool,
    ) -> Result<Self> {
        validate_document(&document)?;
        if project_file.as_os_str().is_empty() {
            return Err(session_error(
                "open_authoring_session",
                "project file path is empty",
            ));
        }
        let generation = AuthorGeneration::INITIAL;
        let asset_library_revision = asset_library.database_revision()?;
        Ok(Self {
            session_id: AuthoringSessionId::new(),
            document,
            project_file,
            runtime_root,
            asset_library,
            navigation_stack: Vec::new(),
            history: AuthoringHistory::default(),
            author_generation: generation,
            saved_generation: saved.then_some(generation),
            saved_asset_library_revision: saved.then_some(asset_library_revision),
            autosaved_generation: None,
            autosaved_asset_library_revision: None,
        })
    }

    /// Canonical read-only project document.
    pub fn document(&self) -> &ProjectDocument {
        &self.document
    }

    /// Direct mutable access reserved for test-fixture construction.
    ///
    /// Production authoring must use the closure or snapshot transaction APIs;
    /// mutating this value directly bypasses validation, generation, and history.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn document_mut_for_test_fixture(&mut self) -> &mut ProjectDocument {
        &mut self.document
    }

    /// Current project identity.
    pub fn project_id(&self) -> ProjectId {
        self.document.project_id
    }

    /// Identity of this specific open authoring lifetime.
    pub fn session_id(&self) -> AuthoringSessionId {
        self.session_id
    }

    /// Current project file path.
    pub fn project_file(&self) -> &Path {
        &self.project_file
    }

    /// Whether this Session has a successfully published durable baseline.
    pub const fn has_durable_baseline(&self) -> bool {
        self.saved_generation.is_some()
    }

    /// Runtime root containing the extracted asset-library state and recovery data.
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    /// Project asset-library authority.
    pub fn asset_library(&self) -> &Arc<AssetLibrary> {
        &self.asset_library
    }

    /// Active Sequence from the canonical collection.
    pub fn active_sequence(&self) -> Option<&Sequence> {
        self.document.sequences.active()
    }

    /// Sequence lookup in the canonical collection.
    pub fn sequence(&self, id: SequenceId) -> Option<&Sequence> {
        self.document.sequences.sequence(id)
    }

    /// Current Sequence navigation stack.
    pub fn navigation_stack(&self) -> &[SequenceId] {
        &self.navigation_stack
    }

    /// Switch the editor's active Sequence without changing authored media semantics.
    pub fn switch_active_sequence(&mut self, id: SequenceId, push_current: bool) -> Result<()> {
        if push_current {
            let current = self.document.sequences.active_sequence_id;
            if current != id {
                self.navigation_stack.push(current);
            }
        }
        self.document.sequences.set_active(id)
    }

    /// Return to the previously visited parent Sequence.
    pub fn return_to_parent_sequence(&mut self) -> Result<bool> {
        let Some(parent) = self.navigation_stack.pop() else {
            return Ok(false);
        };
        self.document.sequences.set_active(parent)?;
        Ok(true)
    }

    /// Replace navigation state after structural operations remove Sequences.
    pub fn retain_navigation_sequences(&mut self) {
        self.navigation_stack
            .retain(|id| self.document.sequences.sequence(*id).is_some());
    }

    /// Current author generation.
    pub fn author_generation(&self) -> AuthorGeneration {
        self.author_generation
    }

    /// Generation last published by an explicit manual save.
    pub fn saved_generation(&self) -> Option<AuthorGeneration> {
        self.saved_generation
    }

    /// Generation last published as an autosave recovery point.
    pub fn autosaved_generation(&self) -> Option<AuthorGeneration> {
        self.autosaved_generation
    }

    /// Whether the newest author document and asset database are both covered
    /// by the latest recovery point.
    pub fn is_current_autosaved(&self) -> bool {
        self.asset_library.database_revision().is_ok_and(|revision| {
            self.autosaved_generation == Some(self.author_generation)
                && self.autosaved_asset_library_revision == Some(revision)
        })
    }

    /// Whether committed author state is newer than the last successful manual save.
    pub fn is_dirty(&self) -> bool {
        self.asset_library.database_revision().map_or(true, |revision| {
            self.saved_generation != Some(self.author_generation)
                || self.saved_asset_library_revision != Some(revision)
        })
    }

    /// Bounded project-wide Undo/Redo history.
    pub fn history(&self) -> &AuthoringHistory {
        &self.history
    }

    /// Snapshot the current state for background persistence.
    pub fn snapshot(&self) -> Result<AuthoringSnapshot> {
        Ok(AuthoringSnapshot {
            session_id: self.session_id,
            generation: self.author_generation,
            asset_library_revision: self.asset_library.database_revision()?,
            document: self.document.clone(),
            asset_library: Arc::clone(&self.asset_library),
        })
    }

    /// Record an already-produced single-Sequence before/after pair atomically.
    pub fn commit_sequence_snapshot(
        &mut self,
        description: impl Into<String>,
        before: Sequence,
        mut after: Sequence,
    ) -> Result<AuthoringCommit> {
        if before.id != after.id {
            return Err(session_error(
                "commit_sequence_snapshot",
                format!(
                    "Sequence identity changed from {} to {}",
                    before.id, after.id
                ),
            ));
        }
        let current = self.document.sequences.sequence(before.id).ok_or_else(|| {
            session_error(
                "commit_sequence_snapshot",
                format!("target Sequence does not exist: {}", before.id),
            )
        })?;
        if current.revision != before.revision {
            return Err(session_error(
                "commit_sequence_snapshot",
                format!(
                    "Sequence {} changed outside the transaction (expected revision {}, current {})",
                    before.id,
                    before.revision.get(),
                    current.revision.get()
                ),
            ));
        }
        after.revision = next_sequence_revision(before.revision)?;
        let mut candidate = self.document.clone();
        *candidate.sequences.sequence_mut(before.id).ok_or_else(|| {
            session_error("commit_sequence_snapshot", "candidate lost target Sequence")
        })? = after.clone();
        validate_document(&candidate)?;
        let description = description.into();
        let outcome = self.history.record_sequence(description, &before, &after)?;
        self.document = candidate;
        self.author_generation = self.author_generation.next()?;
        Ok(sequence_commit(self.author_generation, before.id, outcome))
    }

    /// Record a complete project-level before/after transaction atomically.
    pub fn commit_project_snapshot(
        &mut self,
        description: impl Into<String>,
        before: ProjectDocument,
        mut after: ProjectDocument,
    ) -> Result<AuthoringCommit> {
        if before.project_id != self.document.project_id || after.project_id != before.project_id {
            return Err(session_error(
                "commit_project_snapshot",
                "Project identity changed during the authoring transaction",
            ));
        }
        if serde_json::to_vec(&before)? != serde_json::to_vec(&self.document)? {
            return Err(session_error(
                "commit_project_snapshot",
                "Project changed outside the transaction",
            ));
        }

        let changed_sequence_ids = changed_sequence_ids(&before, &after)?;
        for sequence_id in &changed_sequence_ids {
            let Some(sequence) = after.sequences.sequence_mut(*sequence_id) else {
                continue;
            };
            sequence.revision = match before.sequences.sequence(*sequence_id) {
                Some(previous) => next_sequence_revision(previous.revision)?,
                None => SequenceRevision::INITIAL,
            };
        }
        after.document_revision = self.document.document_revision;
        validate_document(&after)?;
        let outcome =
            self.history
                .record_project(description, &changed_sequence_ids, &before, &after)?;
        self.document = after;
        self.retain_navigation_sequences();
        self.author_generation = self.author_generation.next()?;
        Ok(AuthoringCommit {
            generation: self.author_generation,
            changed_sequence_ids,
            project_wide: true,
            undo_retained: outcome.retained,
        })
    }

    /// Execute one mutation against a cloned active Sequence and commit only after validation.
    pub fn edit_active_sequence<T>(
        &mut self,
        description: impl Into<String>,
        edit: impl FnOnce(&mut Sequence) -> Result<T>,
    ) -> Result<(T, AuthoringCommit)> {
        let before = self
            .active_sequence()
            .cloned()
            .ok_or_else(|| session_error("edit_active_sequence", "no active Sequence"))?;
        let mut after = before.clone();
        let value = edit(&mut after)?;
        let commit = self.commit_sequence_snapshot(description, before, after)?;
        Ok((value, commit))
    }

    /// Undo the newest project-wide authoring transaction.
    pub fn undo(&mut self) -> Result<Option<AuthoringCommit>> {
        let previous_revisions = self
            .document
            .sequences
            .sequences
            .iter()
            .map(|sequence| (sequence.id, sequence.revision))
            .collect::<std::collections::BTreeMap<_, _>>();
        let Some(application) = self.history.undo(&mut self.document)? else {
            return Ok(None);
        };
        advance_restored_revisions(
            &mut self.document,
            &application.affected_sequence_ids,
            &previous_revisions,
        )?;
        validate_document(&self.document)?;
        self.retain_navigation_sequences();
        self.author_generation = self.author_generation.next()?;
        Ok(Some(AuthoringCommit {
            generation: self.author_generation,
            changed_sequence_ids: application.affected_sequence_ids,
            project_wide: application.project_wide,
            undo_retained: true,
        }))
    }

    /// Redo the newest undone project-wide authoring transaction.
    pub fn redo(&mut self) -> Result<Option<AuthoringCommit>> {
        let previous_revisions = self
            .document
            .sequences
            .sequences
            .iter()
            .map(|sequence| (sequence.id, sequence.revision))
            .collect::<std::collections::BTreeMap<_, _>>();
        let Some(application) = self.history.redo(&mut self.document)? else {
            return Ok(None);
        };
        advance_restored_revisions(
            &mut self.document,
            &application.affected_sequence_ids,
            &previous_revisions,
        )?;
        validate_document(&self.document)?;
        self.retain_navigation_sequences();
        self.author_generation = self.author_generation.next()?;
        Ok(Some(AuthoringCommit {
            generation: self.author_generation,
            changed_sequence_ids: application.affected_sequence_ids,
            project_wide: application.project_wide,
            undo_retained: true,
        }))
    }

    /// Mark a completed manual save without hiding edits committed after its snapshot.
    pub fn mark_saved(
        &mut self,
        generation: AuthorGeneration,
        document_revision: u64,
        asset_library_revision: u64,
        meta: ProjectMeta,
        project_file: Option<PathBuf>,
    ) -> Result<()> {
        if generation > self.author_generation {
            return Err(session_error(
                "mark_project_saved",
                "save generation is from the future",
            ));
        }
        self.document.document_revision = self.document.document_revision.max(document_revision);
        if generation == self.author_generation
            && asset_library_revision == self.asset_library.database_revision()?
        {
            self.document.meta = meta;
        }
        if snapshot_is_newer(
            self.saved_generation,
            self.saved_asset_library_revision,
            generation,
            asset_library_revision,
        ) {
            self.saved_generation = Some(generation);
            self.saved_asset_library_revision = Some(asset_library_revision);
        }
        if let Some(project_file) = project_file {
            self.project_file = project_file;
        }
        Ok(())
    }

    /// Mark a completed autosave recovery point.
    pub fn mark_autosaved(
        &mut self,
        generation: AuthorGeneration,
        asset_library_revision: u64,
    ) -> Result<()> {
        if generation > self.author_generation {
            return Err(session_error(
                "mark_project_autosaved",
                "autosave generation is from the future",
            ));
        }
        if snapshot_is_newer(
            self.autosaved_generation,
            self.autosaved_asset_library_revision,
            generation,
            asset_library_revision,
        ) {
            self.autosaved_generation = Some(generation);
            self.autosaved_asset_library_revision = Some(asset_library_revision);
        }
        Ok(())
    }

    /// Whether one asset is explicitly using proxy playback in the author model.
    pub fn is_asset_proxy_mode(&self, asset_id: AssetId) -> bool {
        self.document.proxy_mode_assets.contains(&asset_id)
    }

    /// Change explicit proxy mode as a project authoring transaction.
    pub fn set_asset_proxy_mode(&mut self, asset_id: AssetId, enabled: bool) -> Result<bool> {
        let before = self.document.clone();
        let changed = if enabled {
            self.document.proxy_mode_assets.insert(asset_id)
        } else {
            self.document.proxy_mode_assets.remove(&asset_id)
        };
        if !changed {
            return Ok(false);
        }
        let after = self.document.clone();
        self.document = before.clone();
        self.commit_project_snapshot("切换素材代理模式", before, after)?;
        Ok(true)
    }
}

fn snapshot_is_newer(
    current_generation: Option<AuthorGeneration>,
    current_asset_revision: Option<u64>,
    candidate_generation: AuthorGeneration,
    candidate_asset_revision: u64,
) -> bool {
    match current_generation {
        None => true,
        Some(current) if candidate_generation > current => true,
        Some(current) if candidate_generation == current => {
            current_asset_revision.is_none_or(|revision| candidate_asset_revision > revision)
        }
        Some(_) => false,
    }
}

fn sequence_commit(
    generation: AuthorGeneration,
    sequence_id: SequenceId,
    outcome: AuthoringHistoryRecordOutcome,
) -> AuthoringCommit {
    AuthoringCommit {
        generation,
        changed_sequence_ids: vec![sequence_id],
        project_wide: false,
        undo_retained: outcome.retained,
    }
}

fn validate_document(document: &ProjectDocument) -> Result<()> {
    document
        .validate()
        .map_err(|error| session_error("validate_authoring_document", error.to_string()))
}

fn next_sequence_revision(revision: SequenceRevision) -> Result<SequenceRevision> {
    revision.checked_next().ok_or_else(|| {
        session_error(
            "advance_sequence_revision",
            format!("Sequence author revision {} is exhausted", revision.get()),
        )
    })
}

fn changed_sequence_ids(
    before: &ProjectDocument,
    after: &ProjectDocument,
) -> Result<Vec<SequenceId>> {
    let project_execution_semantics_changed =
        serde_json::to_vec(&before.settings)? != serde_json::to_vec(&after.settings)?;
    let ids = before
        .sequences
        .sequences
        .iter()
        .chain(&after.sequences.sequences)
        .map(|sequence| sequence.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut changed = Vec::new();
    for id in ids {
        let differs = match (before.sequences.sequence(id), after.sequences.sequence(id)) {
            (Some(before), Some(after)) => {
                project_execution_semantics_changed
                    || sequence_authoring_bytes(before)? != sequence_authoring_bytes(after)?
            }
            _ => true,
        };
        if differs {
            changed.push(id);
        }
    }
    Ok(changed)
}

fn sequence_authoring_bytes(sequence: &Sequence) -> Result<Vec<u8>> {
    let mut normalized = sequence.clone();
    normalized.revision = SequenceRevision::INITIAL;
    Ok(serde_json::to_vec(&normalized)?)
}

fn advance_restored_revisions(
    document: &mut ProjectDocument,
    affected: &[SequenceId],
    previous: &std::collections::BTreeMap<SequenceId, SequenceRevision>,
) -> Result<()> {
    for id in affected {
        let Some(sequence) = document.sequences.sequence_mut(*id) else {
            continue;
        };
        let base = previous.get(id).copied().unwrap_or(sequence.revision);
        sequence.revision = next_sequence_revision(base)?;
    }
    Ok(())
}

fn session_error(step_id: &str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}
#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::ProjectSettings;
    use mondrian_timeline::SequenceCollection;

    fn session_with_two_sequences(saved: bool) -> AuthoringSession {
        let root = tempfile::tempdir().expect("runtime tempdir").keep();
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let primary = Sequence::new("Primary");
        let secondary = Sequence::new("Secondary");
        let mut sequences = SequenceCollection::new(primary);
        sequences.add_sequence(secondary).expect("secondary Sequence");
        let document = ProjectDocument::new("Project", sequences, ProjectSettings::default());
        let project_file = root.join("project.mdp");
        if saved {
            AuthoringSession::open_saved(document, project_file, root, library)
                .expect("saved session")
        } else {
            AuthoringSession::new_unsaved(document, project_file, root, library)
                .expect("unsaved session")
        }
    }

    #[test]
    fn asset_only_edits_participate_in_dirty_and_save_identity() {
        let mut session = session_with_two_sequences(true);
        let generation = session.author_generation();
        assert!(!session.is_dirty());

        session
            .asset_library()
            .create_adjustment_layer_asset(Some("Adjustment"))
            .expect("asset edit");
        assert_eq!(session.author_generation(), generation);
        assert!(session.is_dirty());

        let snapshot = session.snapshot().expect("snapshot");
        session
            .mark_saved(
                snapshot.generation,
                snapshot.document.document_revision + 1,
                snapshot.asset_library_revision,
                snapshot.document.meta,
                None,
            )
            .expect("mark saved");
        assert!(!session.is_dirty());
    }

    #[test]
    fn stale_save_completion_cannot_hide_or_overwrite_newer_author_state() {
        let mut session = session_with_two_sequences(true);
        let stale = session.snapshot().expect("stale snapshot");
        let before = session.document().clone();
        let mut after = before.clone();
        after.meta.name = "New name".to_owned();
        session
            .commit_project_snapshot("rename project", before, after)
            .expect("rename transaction");

        session
            .mark_saved(
                stale.generation,
                stale.document.document_revision + 1,
                stale.asset_library_revision,
                stale.document.meta,
                None,
            )
            .expect("stale completion");

        assert_eq!(session.document().meta.name, "New name");
        assert!(session.is_dirty());
    }

    #[test]
    fn project_wide_undo_survives_active_sequence_navigation() {
        let mut session = session_with_two_sequences(true);
        let primary_id = session.document().sequences.default_sequence_id;
        let secondary_id = session
            .document()
            .sequences
            .sequences
            .iter()
            .find(|sequence| sequence.id != primary_id)
            .expect("secondary")
            .id;
        session
            .edit_active_sequence("rename Sequence", |sequence| {
                sequence.name = "Renamed".to_owned();
                Ok(())
            })
            .expect("rename");
        session.switch_active_sequence(secondary_id, true).expect("switch Sequence");

        let commit = session.undo().expect("undo").expect("undo entry");

        assert_eq!(
            session.document().sequences.active_sequence_id,
            secondary_id
        );
        assert_eq!(
            session.sequence(primary_id).expect("primary").name,
            "Primary"
        );
        assert_eq!(commit.changed_sequence_ids, vec![primary_id]);
    }

    #[test]
    fn project_execution_settings_invalidate_every_sequence_revision() {
        let mut session = session_with_two_sequences(true);
        let revisions = session
            .document()
            .sequences
            .sequences
            .iter()
            .map(|sequence| (sequence.id, sequence.revision))
            .collect::<std::collections::BTreeMap<_, _>>();
        let before = session.document().clone();
        let mut after = before.clone();
        after.settings.auto_save_interval += 1;

        let commit = session
            .commit_project_snapshot("change execution settings", before, after)
            .expect("project transaction");

        assert_eq!(commit.changed_sequence_ids.len(), revisions.len());
        for (sequence_id, previous) in revisions {
            assert_eq!(
                session.sequence(sequence_id).expect("Sequence").revision,
                previous.checked_next().expect("next revision")
            );
        }
    }

    #[test]
    fn failed_sequence_transaction_leaves_document_generation_and_history_unchanged() {
        let mut session = session_with_two_sequences(true);
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();

        let error = session
            .edit_active_sequence("failing edit", |sequence| {
                sequence.name = "must not escape".to_owned();
                Err::<(), _>(session_error("test_edit", "deliberate failure"))
            })
            .expect_err("candidate edit must fail");

        assert!(error.to_string().contains("deliberate failure"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }
}
