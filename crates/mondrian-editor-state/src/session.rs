//! Canonical project authoring session.
//!
//! This Module owns the sole mutable `ProjectDocument`, project-wide Undo/Redo,
//! editor navigation, and the generation relation between authoring and durable
//! persistence. UI and execution Adapters receive read-only or immutable snapshots.

use crate::history::{
    AuthoringHistory, AuthoringHistoryRecordOutcome, PreparedAuthoringHistoryRestore,
};
use mondrian_assets::AssetLibrary;
use mondrian_core::{
    AssetId, MondrianError, ProjectId, ProjectMeta, Result, SequenceId, SequenceRevision,
};
use mondrian_project::{ProjectAuthoringValidationCertificate, ProjectDocument};
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

impl std::fmt::Display for AuthoringSessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
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

/// Explicit editor navigation operation for a Sequence target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceNavigationIntent {
    /// Select a top-level editing target and discard any nested return path.
    ReplaceRoot,
    /// Enter a nested Sequence and retain the current target as its parent.
    EnterNested,
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
    authoring_validation: ProjectAuthoringValidationCertificate,
    #[cfg(any(test, feature = "test-support"))]
    authoring_validation_fixture_dirty: bool,
    sequence_revision_high_water: std::collections::BTreeMap<SequenceId, SequenceRevision>,
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
        let authoring_validation =
            document.prepare_authoring_validation_certificate().map_err(|error| {
                session_context_error("prepare_authoring_validation_certificate", error)
            })?;
        if project_file.as_os_str().is_empty() {
            return Err(session_error(
                "open_authoring_session",
                "project file path is empty",
            ));
        }
        let generation = AuthorGeneration::INITIAL;
        let asset_library_revision = asset_library.database_revision()?;
        let sequence_revision_high_water = sequence_revisions(&document);
        Ok(Self {
            session_id: AuthoringSessionId::new(),
            document,
            project_file,
            runtime_root,
            asset_library,
            navigation_stack: Vec::new(),
            history: AuthoringHistory::default(),
            authoring_validation,
            #[cfg(any(test, feature = "test-support"))]
            authoring_validation_fixture_dirty: false,
            sequence_revision_high_water,
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
    /// The next test transaction fully rebuilds the opaque Project authoring
    /// validation certificate before it evaluates a candidate.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn document_mut_for_test_fixture(&mut self) -> &mut ProjectDocument {
        self.authoring_validation_fixture_dirty = true;
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
    pub fn switch_active_sequence(
        &mut self,
        id: SequenceId,
        intent: SequenceNavigationIntent,
    ) -> Result<()> {
        let current = self.document.sequences.active_sequence_id;
        if self.document.sequences.sequence(id).is_none() {
            return self.document.sequences.set_active(id);
        }
        let should_push = intent == SequenceNavigationIntent::EnterNested && current != id;
        if should_push {
            self.navigation_stack.try_reserve(1).map_err(|error| {
                session_error(
                    "switch_active_sequence",
                    format!("could not reserve Sequence navigation state: {error}"),
                )
            })?;
        }
        self.document.sequences.set_active(id)?;
        match intent {
            SequenceNavigationIntent::ReplaceRoot => self.navigation_stack.clear(),
            SequenceNavigationIntent::EnterNested if should_push => {
                self.navigation_stack.push(current);
            }
            SequenceNavigationIntent::EnterNested => {}
        }
        Ok(())
    }

    /// Return to the previously visited parent Sequence.
    pub fn return_to_parent_sequence(&mut self) -> Result<bool> {
        let Some(parent) = self.navigation_stack.last().copied() else {
            return Ok(false);
        };
        self.document.sequences.set_active(parent)?;
        self.navigation_stack.pop();
        Ok(true)
    }

    /// Replace navigation state after structural operations remove Sequences.
    pub fn retain_navigation_sequences(&mut self) {
        self.navigation_stack
            .retain(|id| self.document.sequences.sequence(*id).is_some());
        let active = self.document.sequences.active_sequence_id;
        while self.navigation_stack.last().copied() == Some(active) {
            self.navigation_stack.pop();
        }
    }

    /// Current author generation.
    pub fn author_generation(&self) -> AuthorGeneration {
        self.author_generation
    }

    /// Generation last published by an explicit manual save.
    pub fn saved_generation(&self) -> Option<AuthorGeneration> {
        self.saved_generation
    }

    /// Whether the current manual durable baseline covers this exact snapshot
    /// or a newer snapshot from the same open Authoring Session.
    ///
    /// Persistence completion may be delivered after a later request for the
    /// same canonical file was already applied. Callers use this monotonic
    /// query to treat that older completion as satisfied without regressing
    /// metadata or surfacing an obsolete failure.
    pub fn manual_save_baseline_covers(
        &self,
        generation: AuthorGeneration,
        asset_library_revision: u64,
    ) -> bool {
        match self.saved_generation {
            Some(saved_generation) if saved_generation > generation => true,
            Some(saved_generation) if saved_generation == generation => self
                .saved_asset_library_revision
                .is_some_and(|saved_revision| saved_revision >= asset_library_revision),
            Some(_) | None => false,
        }
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

    /// Test-only compatibility seam for verifying forged snapshot rejection.
    ///
    /// Production code must enter through [`Self::edit_sequence`], which
    /// creates the candidate from canonical state while holding this Session's
    /// exclusive transaction authority.
    #[cfg(test)]
    fn commit_sequence_snapshot(
        &mut self,
        description: impl Into<String>,
        before: Sequence,
        after: Sequence,
    ) -> Result<AuthoringCommit> {
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
        if current != &before {
            return Err(session_error(
                "commit_sequence_snapshot",
                format!(
                    "Sequence {} content changed outside the transaction",
                    before.id
                ),
            ));
        }
        self.commit_sequence_candidate(description, before, after)?.ok_or_else(|| {
            session_error(
                "commit_sequence_snapshot",
                "snapshot pair contains no authored change",
            )
        })
    }

    fn commit_sequence_candidate(
        &mut self,
        description: impl Into<String>,
        before: Sequence,
        mut after: Sequence,
    ) -> Result<Option<AuthoringCommit>> {
        self.refresh_test_fixture_authoring_validation()?;
        if before.id != after.id {
            return Err(session_error(
                "commit_sequence_edit",
                format!(
                    "Sequence identity changed from {} to {}",
                    before.id, after.id
                ),
            ));
        }
        let current = self.document.sequences.sequence(before.id).ok_or_else(|| {
            session_error(
                "commit_sequence_edit",
                format!("target Sequence does not exist: {}", before.id),
            )
        })?;
        if current.revision != before.revision
            || !current.author_state_eq_ignoring_revision(&before)
        {
            return Err(session_error(
                "commit_sequence_edit",
                format!("Sequence {} changed outside the transaction", before.id),
            ));
        }

        // Sequence revision is Session authority. A closure that happens to
        // write the public persistence field cannot smuggle a revision into the
        // canonical document or turn that write into authored content.
        after.revision = before.revision;
        if before.author_state_eq_ignoring_revision(&after) {
            return Ok(None);
        }

        let next_generation = self.author_generation.next()?;
        after.revision = next_session_sequence_revision(
            &self.sequence_revision_high_water,
            before.id,
            before.revision,
        )?;
        let installed_revision = after.revision;
        let prepared_replacement = self
            .authoring_validation
            .prepare_sequence_replacement(&self.document, after)
            .map_err(|error| session_context_error("validate_sequence_replacement", error))?;
        let prepared_history = self.history.prepare_sequence_record(
            description.into(),
            &before,
            prepared_replacement.replacement(),
        )?;
        let outcome = self.history.commit_prepared_record(prepared_history)?;
        let (document, authoring_validation) = prepared_replacement.into_installation();
        self.document = document;
        self.authoring_validation = authoring_validation;
        self.sequence_revision_high_water.insert(before.id, installed_revision);
        self.author_generation = next_generation;
        Ok(Some(sequence_commit(
            self.author_generation,
            before.id,
            outcome,
        )))
    }

    /// Record a complete project-level before/after transaction atomically.
    pub fn commit_project_snapshot(
        &mut self,
        description: impl Into<String>,
        before: ProjectDocument,
        mut after: ProjectDocument,
    ) -> Result<Option<AuthoringCommit>> {
        self.refresh_test_fixture_authoring_validation()?;
        if before.project_id != self.document.project_id || after.project_id != before.project_id {
            return Err(session_error(
                "commit_project_snapshot",
                "Project identity changed during the authoring transaction",
            ));
        }
        if before != self.document {
            return Err(session_error(
                "commit_project_snapshot",
                "Project changed outside the transaction",
            ));
        }

        let changed_sequence_ids = changed_sequence_ids(&before, &after)?;
        let changed_sequence_ids_set =
            changed_sequence_ids.iter().copied().collect::<std::collections::BTreeSet<_>>();
        let before_sequences = before
            .sequences
            .sequences
            .iter()
            .map(|sequence| (sequence.id, sequence))
            .collect::<std::collections::BTreeMap<_, _>>();
        for sequence in &mut after.sequences.sequences {
            sequence.revision = match before_sequences.get(&sequence.id).copied() {
                Some(previous) if changed_sequence_ids_set.contains(&sequence.id) => {
                    next_session_sequence_revision(
                        &self.sequence_revision_high_water,
                        sequence.id,
                        previous.revision,
                    )?
                }
                // Sequence revisions are Session authority. A Project
                // transaction may supply an arbitrary candidate revision, but
                // unchanged content can never smuggle that value into the
                // canonical document.
                Some(previous) => previous.revision,
                None => match self.sequence_revision_high_water.get(&sequence.id).copied() {
                    Some(previous) => next_sequence_revision(previous)?,
                    None => SequenceRevision::INITIAL,
                },
            };
        }
        let next_revision_high_water =
            revision_high_water_after_document(&self.sequence_revision_high_water, &after);
        after.document_revision = self.document.document_revision;
        // Save completion owns persistence evidence. Authoring transactions
        // cannot forge or roll back the last durably published timestamp.
        after.meta.updated_at = self.document.meta.updated_at;
        let current_active_sequence_id = self.document.sequences.active_sequence_id;
        if after.sequences.sequence(current_active_sequence_id).is_some() {
            after.sequences.active_sequence_id = current_active_sequence_id;
        }
        let authored_noop = after == self.document;
        if authored_noop {
            return Ok(None);
        }
        let next_authoring_validation = self
            .authoring_validation
            .prepare_project_replacement(&self.document, &after)
            .map_err(|error| session_context_error("validate_project_replacement", error))?;
        let next_generation = self.author_generation.next()?;
        let prepared = self.history.prepare_project_record(
            description,
            &changed_sequence_ids,
            &before,
            &after,
        )?;
        let outcome = self.history.commit_prepared_record(prepared)?;
        self.document = after;
        self.authoring_validation = next_authoring_validation;
        self.sequence_revision_high_water = next_revision_high_water;
        self.retain_navigation_sequences();
        self.author_generation = next_generation;
        Ok(Some(AuthoringCommit {
            generation: self.author_generation,
            changed_sequence_ids,
            project_wide: true,
            undo_retained: outcome.retained,
        }))
    }

    /// Execute one trusted mutation against a cloned Sequence and commit only
    /// after complete replacement validation.
    ///
    /// The canonical Sequence is the sole candidate source. A no-op returns no
    /// commit, does not advance generations, and creates no Undo entry.
    pub fn edit_sequence<T>(
        &mut self,
        sequence_id: SequenceId,
        description: impl Into<String>,
        edit: impl FnOnce(&mut Sequence) -> Result<T>,
    ) -> Result<(T, Option<AuthoringCommit>)> {
        let before = self.sequence(sequence_id).cloned().ok_or_else(|| {
            session_error(
                "edit_sequence",
                format!("target Sequence does not exist: {sequence_id}"),
            )
        })?;
        let mut after = before.clone();
        let value = edit(&mut after)?;
        let commit = self.commit_sequence_candidate(description, before, after)?;
        Ok((value, commit))
    }

    /// Execute one mutation against the active Sequence.
    pub fn edit_active_sequence<T>(
        &mut self,
        description: impl Into<String>,
        edit: impl FnOnce(&mut Sequence) -> Result<T>,
    ) -> Result<(T, Option<AuthoringCommit>)> {
        let sequence_id = self
            .active_sequence()
            .map(|sequence| sequence.id)
            .ok_or_else(|| session_error("edit_active_sequence", "no active Sequence"))?;
        self.edit_sequence(sequence_id, description, edit)
    }

    /// Undo the newest project-wide authoring transaction.
    pub fn undo(&mut self) -> Result<Option<AuthoringCommit>> {
        self.refresh_test_fixture_authoring_validation()?;
        if !self.history.can_undo() {
            return Ok(None);
        }
        let next_generation = self.author_generation.next()?;
        let Some(restore) = self.history.prepare_undo(&self.document)? else {
            return Ok(None);
        };
        let (changed_sequence_ids, project_wide) = match restore {
            PreparedAuthoringHistoryRestore::Sequence { mut sequence, application } => {
                let sequence_id = sequence.id;
                let target_index = self
                    .document
                    .sequences
                    .sequences
                    .iter()
                    .position(|candidate| candidate.id == sequence.id)
                    .ok_or_else(|| {
                        session_error(
                            "prepare_authoring_undo",
                            format!("target Sequence no longer exists: {}", sequence.id),
                        )
                    })?;
                sequence.revision = next_session_sequence_revision(
                    &self.sequence_revision_high_water,
                    sequence.id,
                    self.document.sequences.sequences[target_index].revision,
                )?;
                let installed_revision = sequence.revision;
                let prepared_replacement = self
                    .authoring_validation
                    .prepare_sequence_replacement(&self.document, sequence)
                    .map_err(|error| {
                        session_context_error("validate_sequence_replacement", error)
                    })?;
                self.history.commit_prepared_undo()?;
                let (document, authoring_validation) = prepared_replacement.into_installation();
                self.document = document;
                self.authoring_validation = authoring_validation;
                self.sequence_revision_high_water.insert(sequence_id, installed_revision);
                (application.affected_sequence_ids, application.project_wide)
            }
            PreparedAuthoringHistoryRestore::Project { mut document, application } => {
                preserve_editor_navigation(&self.document, &mut document);
                let (changed_sequence_ids, next_revision_high_water) = prepare_restored_project(
                    &self.document,
                    &mut document,
                    &self.sequence_revision_high_water,
                    "prepare_authoring_undo",
                    &application.affected_sequence_ids,
                )?;
                let next_authoring_validation = self
                    .authoring_validation
                    .prepare_project_replacement(&self.document, &document)
                    .map_err(|error| {
                        session_context_error("validate_project_replacement", error)
                    })?;
                self.history.commit_prepared_undo()?;
                self.document = document;
                self.authoring_validation = next_authoring_validation;
                self.sequence_revision_high_water = next_revision_high_water;
                self.retain_navigation_sequences();
                (changed_sequence_ids, application.project_wide)
            }
        };
        self.author_generation = next_generation;
        Ok(Some(AuthoringCommit {
            generation: self.author_generation,
            changed_sequence_ids,
            project_wide,
            undo_retained: true,
        }))
    }

    /// Redo the newest undone project-wide authoring transaction.
    pub fn redo(&mut self) -> Result<Option<AuthoringCommit>> {
        self.refresh_test_fixture_authoring_validation()?;
        if !self.history.can_redo() {
            return Ok(None);
        }
        let next_generation = self.author_generation.next()?;
        let Some(restore) = self.history.prepare_redo(&self.document)? else {
            return Ok(None);
        };
        let (changed_sequence_ids, project_wide) = match restore {
            PreparedAuthoringHistoryRestore::Sequence { mut sequence, application } => {
                let sequence_id = sequence.id;
                let target_index = self
                    .document
                    .sequences
                    .sequences
                    .iter()
                    .position(|candidate| candidate.id == sequence.id)
                    .ok_or_else(|| {
                        session_error(
                            "prepare_authoring_redo",
                            format!("target Sequence no longer exists: {}", sequence.id),
                        )
                    })?;
                sequence.revision = next_session_sequence_revision(
                    &self.sequence_revision_high_water,
                    sequence.id,
                    self.document.sequences.sequences[target_index].revision,
                )?;
                let installed_revision = sequence.revision;
                let prepared_replacement = self
                    .authoring_validation
                    .prepare_sequence_replacement(&self.document, sequence)
                    .map_err(|error| {
                        session_context_error("validate_sequence_replacement", error)
                    })?;
                self.history.commit_prepared_redo()?;
                let (document, authoring_validation) = prepared_replacement.into_installation();
                self.document = document;
                self.authoring_validation = authoring_validation;
                self.sequence_revision_high_water.insert(sequence_id, installed_revision);
                (application.affected_sequence_ids, application.project_wide)
            }
            PreparedAuthoringHistoryRestore::Project { mut document, application } => {
                preserve_editor_navigation(&self.document, &mut document);
                let (changed_sequence_ids, next_revision_high_water) = prepare_restored_project(
                    &self.document,
                    &mut document,
                    &self.sequence_revision_high_water,
                    "prepare_authoring_redo",
                    &application.affected_sequence_ids,
                )?;
                let next_authoring_validation = self
                    .authoring_validation
                    .prepare_project_replacement(&self.document, &document)
                    .map_err(|error| {
                        session_context_error("validate_project_replacement", error)
                    })?;
                self.history.commit_prepared_redo()?;
                self.document = document;
                self.authoring_validation = next_authoring_validation;
                self.sequence_revision_high_water = next_revision_high_water;
                self.retain_navigation_sequences();
                (changed_sequence_ids, application.project_wide)
            }
        };
        self.author_generation = next_generation;
        Ok(Some(AuthoringCommit {
            generation: self.author_generation,
            changed_sequence_ids,
            project_wide,
            undo_retained: true,
        }))
    }

    fn refresh_test_fixture_authoring_validation(&mut self) -> Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        if self.authoring_validation_fixture_dirty {
            let next_validation =
                self.document.prepare_authoring_validation_certificate().map_err(|error| {
                    session_context_error("refresh_test_fixture_authoring_validation", error)
                })?;
            self.authoring_validation = next_validation;
            self.sequence_revision_high_water = revision_high_water_after_document(
                &self.sequence_revision_high_water,
                &self.document,
            );
            self.authoring_validation_fixture_dirty = false;
        }
        Ok(())
    }

    /// Mark a completed manual save without hiding edits committed after its snapshot.
    pub fn mark_saved(
        &mut self,
        generation: AuthorGeneration,
        document_revision: u64,
        asset_library_revision: u64,
        persisted_meta: ProjectMeta,
        project_file: Option<PathBuf>,
    ) -> Result<()> {
        if generation > self.author_generation {
            return Err(session_error(
                "mark_project_saved",
                "save generation is from the future",
            ));
        }
        let current_asset_library_revision = self.asset_library.database_revision()?;
        let completion_matches_current_state = generation == self.author_generation
            && asset_library_revision == current_asset_library_revision;
        if completion_matches_current_state
            && !persisted_meta.author_state_eq_ignoring_updated_at(&self.document.meta)
        {
            return Err(session_error(
                "mark_project_saved",
                "persistence completion attempted to change authored Project metadata",
            ));
        }
        self.document.document_revision = self.document.document_revision.max(document_revision);
        if completion_matches_current_state {
            // A successful publication owns only its timestamp evidence.
            // User-authored metadata remains transaction and History authority.
            self.document.meta.updated_at = persisted_meta.updated_at;
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
    pub fn set_asset_proxy_mode(
        &mut self,
        asset_id: AssetId,
        enabled: bool,
    ) -> Result<Option<AuthoringCommit>> {
        if self.document.proxy_mode_assets.contains(&asset_id) == enabled {
            return Ok(None);
        }
        let before = self.document.clone();
        let mut after = before.clone();
        let changed = if enabled {
            after.proxy_mode_assets.insert(asset_id)
        } else {
            after.proxy_mode_assets.remove(&asset_id)
        };
        debug_assert!(changed);
        self.commit_project_snapshot("切换素材代理模式", before, after)
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
    let before_sequences = before
        .sequences
        .sequences
        .iter()
        .map(|sequence| (sequence.id, sequence))
        .collect::<std::collections::BTreeMap<_, _>>();
    let after_sequences = after
        .sequences
        .sequences
        .iter()
        .map(|sequence| (sequence.id, sequence))
        .collect::<std::collections::BTreeMap<_, _>>();
    let ids = before_sequences
        .keys()
        .chain(after_sequences.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut changed = Vec::new();
    for id in ids {
        let differs = match (before_sequences.get(&id), after_sequences.get(&id)) {
            (Some(before), Some(after)) => !before.author_state_eq_ignoring_revision(after),
            _ => true,
        };
        if differs {
            changed.push(id);
        }
    }
    Ok(changed)
}

fn sequence_revisions(
    document: &ProjectDocument,
) -> std::collections::BTreeMap<SequenceId, SequenceRevision> {
    document
        .sequences
        .sequences
        .iter()
        .map(|sequence| (sequence.id, sequence.revision))
        .collect()
}

fn next_session_sequence_revision(
    high_water: &std::collections::BTreeMap<SequenceId, SequenceRevision>,
    sequence_id: SequenceId,
    live_revision: SequenceRevision,
) -> Result<SequenceRevision> {
    let base = high_water
        .get(&sequence_id)
        .copied()
        .unwrap_or(live_revision)
        .max(live_revision);
    next_sequence_revision(base)
}

fn revision_high_water_after_document(
    previous: &std::collections::BTreeMap<SequenceId, SequenceRevision>,
    document: &ProjectDocument,
) -> std::collections::BTreeMap<SequenceId, SequenceRevision> {
    let mut next = previous.clone();
    for sequence in &document.sequences.sequences {
        next.entry(sequence.id)
            .and_modify(|revision| *revision = (*revision).max(sequence.revision))
            .or_insert(sequence.revision);
    }
    next
}

fn preserve_editor_navigation(current: &ProjectDocument, restored: &mut ProjectDocument) {
    let current_active = current.sequences.active_sequence_id;
    if restored.sequences.sequence(current_active).is_some() {
        restored.sequences.active_sequence_id = current_active;
    }
}

fn prepare_restored_project(
    current: &ProjectDocument,
    restored: &mut ProjectDocument,
    high_water: &std::collections::BTreeMap<SequenceId, SequenceRevision>,
    step_id: &str,
    expected_affected_sequence_ids: &[SequenceId],
) -> Result<(
    Vec<SequenceId>,
    std::collections::BTreeMap<SequenceId, SequenceRevision>,
)> {
    let (changed_sequence_ids, next_revision_high_water) =
        normalize_restored_project_revisions(current, restored, high_water)?;
    if changed_sequence_ids != expected_affected_sequence_ids {
        return Err(session_error(
            step_id,
            "Project History affected-Sequence evidence does not match restored content",
        ));
    }
    Ok((changed_sequence_ids, next_revision_high_water))
}

fn normalize_restored_project_revisions(
    current: &ProjectDocument,
    restored: &mut ProjectDocument,
    high_water: &std::collections::BTreeMap<SequenceId, SequenceRevision>,
) -> Result<(
    Vec<SequenceId>,
    std::collections::BTreeMap<SequenceId, SequenceRevision>,
)> {
    let changed_sequence_ids = changed_sequence_ids(current, restored)?;
    let changed = changed_sequence_ids.iter().copied().collect::<std::collections::BTreeSet<_>>();
    let current_sequences = current
        .sequences
        .sequences
        .iter()
        .map(|sequence| (sequence.id, sequence))
        .collect::<std::collections::BTreeMap<_, _>>();

    for sequence in &mut restored.sequences.sequences {
        sequence.revision = match current_sequences.get(&sequence.id).copied() {
            Some(live) if changed.contains(&sequence.id) => {
                next_session_sequence_revision(high_water, sequence.id, live.revision)?
            }
            Some(live) => live.revision,
            None => {
                let base = high_water
                    .get(&sequence.id)
                    .copied()
                    .unwrap_or(sequence.revision)
                    .max(sequence.revision);
                next_sequence_revision(base)?
            }
        };
    }

    let next_high_water = revision_high_water_after_document(high_water, restored);
    Ok((changed_sequence_ids, next_high_water))
}

fn session_error(step_id: &str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}

fn session_context_error(step_id: &str, error: impl std::fmt::Display) -> MondrianError {
    session_error(step_id, format!("{error:#}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ProgramOutputId, ProjectColorEnvironment, ProjectSettings, TimelineTime};
    use mondrian_timeline::{AudioRouteDestination, Clip, SequenceCollection, SequenceSettings};

    fn session_with_two_sequences(saved: bool) -> AuthoringSession {
        let root = tempfile::tempdir().expect("runtime tempdir").keep();
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let primary = Sequence::new("Primary");
        let secondary = Sequence::new("Secondary");
        let mut sequences = SequenceCollection::new(primary);
        sequences.add_sequence(secondary).expect("secondary Sequence");
        let document = ProjectDocument::new(
            "Project",
            sequences,
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        let project_file = root.join("project.mdp");
        if saved {
            AuthoringSession::open_saved(document, project_file, root, library)
                .expect("saved session")
        } else {
            AuthoringSession::new_unsaved(document, project_file, root, library)
                .expect("unsaved session")
        }
    }

    fn session_with_nested_audio_binding(
    ) -> (AuthoringSession, SequenceId, SequenceId, ProgramOutputId) {
        let root = tempfile::tempdir().expect("runtime tempdir").keep();
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let child = Sequence::new("Child");
        let child_id = child.id;
        let child_output_id = child.audio_program.outputs[0].id;
        let mut parent = Sequence::new("Parent");
        let parent_id = parent.id;
        let nested = Clip::new_nested_sequence(
            child_id,
            TimelineTime::ZERO,
            TimelineTime::ONE,
            Some("Child".to_owned()),
        )
        .expect("nested Clip");
        parent
            .add_nested_audio_clip(parent.audio_tracks[0].id, nested, child_output_id)
            .expect("nested output binding");
        let mut sequences = SequenceCollection::new(parent);
        sequences.add_sequence(child).expect("child Sequence");
        let document = ProjectDocument::new(
            "Nested Project",
            sequences,
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        let session =
            AuthoringSession::open_saved(document, root.join("project.mdp"), root, library)
                .expect("nested session");
        (session, parent_id, child_id, child_output_id)
    }

    fn replace_main_output_identity(sequence: &mut Sequence) {
        let previous = sequence.audio_program.outputs[0].id;
        let replacement = ProgramOutputId::new();
        sequence.audio_program.outputs[0].id = replacement;
        for route in &mut sequence.audio_program.routes {
            if route.destination == AudioRouteDestination::Output(previous) {
                route.destination = AudioRouteDestination::Output(replacement);
            }
        }
    }

    #[test]
    fn dependency_validation_failure_changes_no_session_authority() {
        let mut session = session_with_two_sequences(true);
        let sequence_id = session.document().sequences.default_sequence_id;
        let document_before = session.document().clone();
        let history_before = session.history().diagnostics();
        let generation_before = session.author_generation();

        let error = session
            .edit_sequence(sequence_id, "invalid nested edge", |sequence| {
                sequence.video_tracks[0].add_clip(
                    Clip::new_nested_sequence(
                        SequenceId::new(),
                        TimelineTime::ZERO,
                        TimelineTime::ONE,
                        None,
                    )
                    .expect("nested Clip"),
                )?;
                Ok(())
            })
            .expect_err("unknown child Sequence must fail");

        assert!(error.to_string().contains("嵌套序列不存在"));
        assert_eq!(session.document(), &document_before);
        assert_eq!(session.history().diagnostics(), history_before);
        assert_eq!(session.author_generation(), generation_before);
        session
            .edit_sequence(sequence_id, "valid edit after rejection", |sequence| {
                sequence.name = "Still Valid".to_owned();
                Ok(())
            })
            .expect("failed preparation must leave the certificate installable")
            .1
            .expect("valid edit commits");
    }

    #[test]
    fn stale_validation_certificate_fails_before_document_or_history_commit() {
        let mut session = session_with_two_sequences(true);
        let target_id = session.document().sequences.default_sequence_id;
        let other_id = session
            .document()
            .sequences
            .sequences
            .iter()
            .find(|sequence| sequence.id != target_id)
            .expect("other Sequence")
            .id;
        session
            .document
            .sequences
            .sequence_mut(other_id)
            .expect("other Sequence")
            .revision = SequenceRevision::new(2).expect("revision");
        let document_before = session.document().clone();
        let history_before = session.history().diagnostics();
        let generation_before = session.author_generation();

        let error = session
            .edit_sequence(target_id, "rename with stale certificate", |sequence| {
                sequence.name = "Renamed".to_owned();
                Ok(())
            })
            .expect_err("stale certificate must fail closed");

        assert!(error.to_string().contains("author baseline"));
        assert_eq!(session.document(), &document_before);
        assert_eq!(session.history().diagnostics(), history_before);
        assert_eq!(session.author_generation(), generation_before);
    }

    #[test]
    fn sequence_undo_and_redo_install_validation_certificate_atomically() {
        let (mut session, parent_id, child_id, _) = session_with_nested_audio_binding();

        session
            .edit_sequence(parent_id, "remove nested binding", |parent| {
                parent.audio_tracks[0].clips.clear();
                Ok(())
            })
            .expect("remove nested binding")
            .1
            .expect("committed removal");
        session.undo().expect("Undo").expect("restored binding");

        let history_before_failed_edit = session.history().diagnostics();
        session
            .edit_sequence(child_id, "remove bound output", |child| {
                replace_main_output_identity(child);
                Ok(())
            })
            .expect_err("Undo-restored inbound obligation must reject output removal");
        assert_eq!(session.history().diagnostics(), history_before_failed_edit);

        session.redo().expect("Redo").expect("removed binding again");
        session
            .edit_sequence(child_id, "replace unbound output", |child| {
                replace_main_output_identity(child);
                Ok(())
            })
            .expect("Redo-installed certificate must have removed the inbound obligation")
            .1
            .expect("output replacement commit");
    }

    #[test]
    fn structural_project_redo_rebuilds_the_validation_certificate() {
        let mut session = session_with_two_sequences(true);
        let parent_id = session.document().sequences.default_sequence_id;
        let child = Sequence::new("Added Child");
        let child_id = child.id;
        let before = session.document().clone();
        let mut after = before.clone();
        after.sequences.add_sequence(child).expect("add child candidate");
        session
            .commit_project_snapshot("add child Sequence", before, after)
            .expect("Project transaction")
            .expect("Project commit");

        session.undo().expect("Undo add").expect("removed child");
        assert!(session.sequence(child_id).is_none());
        session.redo().expect("Redo add").expect("restored child");

        session
            .edit_sequence(parent_id, "nest restored child", |parent| {
                parent.video_tracks[0].add_clip(
                    Clip::new_nested_sequence(
                        child_id,
                        TimelineTime::ZERO,
                        TimelineTime::ONE,
                        None,
                    )
                    .expect("nested Clip"),
                )?;
                Ok(())
            })
            .expect("rebuilt certificate must admit the restored child")
            .1
            .expect("nested edge commit");
    }

    #[test]
    fn asset_only_edits_participate_in_dirty_and_save_identity() {
        let mut session = session_with_two_sequences(true);
        let generation = session.author_generation();
        let initial_revision =
            session.asset_library().database_revision().expect("initial library revision");
        assert!(!session.is_dirty());
        assert!(session.manual_save_baseline_covers(generation, initial_revision));

        session
            .asset_library()
            .create_adjustment_layer_asset(Some("Adjustment"))
            .expect("asset edit");
        assert_eq!(session.author_generation(), generation);
        assert!(session.is_dirty());

        let snapshot = session.snapshot().expect("snapshot");
        assert!(
            !session
                .manual_save_baseline_covers(snapshot.generation, snapshot.asset_library_revision),
            "the previous baseline may not cover a same-generation Asset Library edit"
        );
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
        assert!(session
            .manual_save_baseline_covers(snapshot.generation, snapshot.asset_library_revision));
    }

    #[test]
    fn large_proxy_set_detaches_once_and_history_reuses_immutable_allocations() {
        let mut session = session_with_two_sequences(true);
        session.document.proxy_mode_assets.extend((0..4_096).map(|_| AssetId::new()));
        let before_allocation = session.document.proxy_mode_assets.allocation_id();
        let added = AssetId::new();

        let commit = session
            .set_asset_proxy_mode(added, true)
            .expect("enable proxy mode")
            .expect("proxy mode commit");
        let after_allocation = session.document.proxy_mode_assets.allocation_id();

        assert!(commit.project_wide);
        assert_ne!(after_allocation, before_allocation);
        assert!(session.is_asset_proxy_mode(added));
        assert!(session.set_asset_proxy_mode(added, true).expect("repeat proxy mode").is_none());
        assert_eq!(
            session.document.proxy_mode_assets.allocation_id(),
            after_allocation
        );

        session.undo().expect("undo proxy mode").expect("undo commit");
        assert_eq!(
            session.document.proxy_mode_assets.allocation_id(),
            before_allocation
        );
        assert!(!session.is_asset_proxy_mode(added));

        session.redo().expect("redo proxy mode").expect("redo commit");
        assert_eq!(
            session.document.proxy_mode_assets.allocation_id(),
            after_allocation
        );
        assert!(session.is_asset_proxy_mode(added));
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
            .expect("rename transaction")
            .expect("rename commit");

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
    fn current_save_completion_cannot_forge_authored_project_metadata() {
        let mut session = session_with_two_sequences(true);
        let snapshot = session.snapshot().expect("current snapshot");
        let before_document = session.document().clone();
        let before_history = session.history().diagnostics();
        let mut forged_meta = snapshot.document.meta.clone();
        forged_meta.name = "Persistence-forged name".to_owned();

        let error = session
            .mark_saved(
                snapshot.generation,
                snapshot.document.document_revision + 1,
                snapshot.asset_library_revision,
                forged_meta,
                None,
            )
            .expect_err("persistence may not mutate authored metadata");

        assert!(error.to_string().contains("authored Project metadata"));
        assert_eq!(session.document(), &before_document);
        assert_eq!(session.history().diagnostics(), before_history);
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
        session
            .switch_active_sequence(secondary_id, SequenceNavigationIntent::EnterNested)
            .expect("switch Sequence");

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
    fn failed_navigation_changes_neither_active_sequence_nor_stack() {
        let mut session = session_with_two_sequences(true);
        let active_before = session.document().sequences.active_sequence_id;
        let stack_before = session.navigation_stack().to_vec();

        session
            .switch_active_sequence(SequenceId::new(), SequenceNavigationIntent::EnterNested)
            .expect_err("unknown Sequence must reject navigation");

        assert_eq!(
            session.document().sequences.active_sequence_id,
            active_before
        );
        assert_eq!(session.navigation_stack(), stack_before);

        let missing_parent = SequenceId::new();
        session.navigation_stack.push(missing_parent);
        let stack_with_missing_parent = session.navigation_stack().to_vec();
        session
            .return_to_parent_sequence()
            .expect_err("missing parent must reject return");
        assert_eq!(
            session.document().sequences.active_sequence_id,
            active_before
        );
        assert_eq!(session.navigation_stack(), stack_with_missing_parent);
    }

    #[test]
    fn replacing_the_root_navigation_target_discards_the_nested_return_path() {
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
            .switch_active_sequence(secondary_id, SequenceNavigationIntent::EnterNested)
            .expect("enter nested Sequence");
        assert_eq!(session.navigation_stack(), &[primary_id]);

        session
            .switch_active_sequence(primary_id, SequenceNavigationIntent::ReplaceRoot)
            .expect("replace navigation root");

        assert_eq!(session.document().sequences.active_sequence_id, primary_id);
        assert!(session.navigation_stack().is_empty());
        assert!(!session.return_to_parent_sequence().expect("empty navigation return"));
    }

    #[test]
    fn project_history_preserves_current_navigation_when_target_still_exists() {
        let mut session = session_with_two_sequences(true);
        let secondary_id = session
            .document()
            .sequences
            .sequences
            .iter()
            .find(|sequence| sequence.id != session.document().sequences.default_sequence_id)
            .expect("secondary")
            .id;
        let before = session.document().clone();
        let mut after = before.clone();
        after.settings.auto_save_interval += 1;
        session
            .commit_project_snapshot("change Project setting", before, after)
            .expect("Project transaction")
            .expect("Project commit");
        session
            .switch_active_sequence(secondary_id, SequenceNavigationIntent::EnterNested)
            .expect("switch Sequence");

        session.undo().expect("Undo Project").expect("Project history");
        assert_eq!(
            session.document().sequences.active_sequence_id,
            secondary_id
        );
        assert_eq!(session.navigation_stack().len(), 1);

        session.redo().expect("Redo Project").expect("Project history");
        assert_eq!(
            session.document().sequences.active_sequence_id,
            secondary_id
        );
        assert_eq!(session.navigation_stack().len(), 1);
    }

    #[test]
    fn structural_navigation_fallback_removes_noop_parent_tail() {
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
            .switch_active_sequence(secondary_id, SequenceNavigationIntent::EnterNested)
            .expect("switch Sequence");
        assert_eq!(session.navigation_stack(), &[primary_id]);

        let before = session.document().clone();
        let mut after = before.clone();
        let secondary_index = after
            .sequences
            .sequences
            .iter()
            .position(|sequence| sequence.id == secondary_id)
            .expect("secondary index");
        after.sequences.sequences.remove(secondary_index);
        after.sequences.active_sequence_id = primary_id;
        session
            .commit_project_snapshot("remove active Sequence", before, after)
            .expect("Project transaction")
            .expect("Project commit");

        assert_eq!(session.document().sequences.active_sequence_id, primary_id);
        assert!(session.navigation_stack().is_empty());
        assert!(!session.return_to_parent_sequence().expect("empty navigation return"));
    }

    #[test]
    fn project_only_settings_advance_generation_without_rewriting_sequences() {
        let mut session = session_with_two_sequences(true);
        let generation = session.author_generation();
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
            .commit_project_snapshot("change Project settings", before, after)
            .expect("project transaction")
            .expect("Project commit");

        assert!(commit.project_wide);
        assert!(commit.changed_sequence_ids.is_empty());
        assert_eq!(
            commit.generation,
            generation.next().expect("next author generation")
        );
        assert_eq!(session.author_generation(), commit.generation);
        for (sequence_id, previous) in revisions {
            assert_eq!(
                session.sequence(sequence_id).expect("Sequence").revision,
                previous
            );
        }
    }

    #[test]
    fn project_candidate_without_authored_change_is_a_true_noop() {
        let mut session = session_with_two_sequences(true);
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let document_before = session.document().clone();
        let sequence_id = document_before.sequences.default_sequence_id;
        let mut forged = document_before.clone();
        forged.sequences.sequence_mut(sequence_id).expect("default Sequence").revision =
            SequenceRevision::new(u64::MAX).expect("forged revision");
        forged.document_revision = u64::MAX;
        forged.meta.updated_at += chrono::Duration::seconds(1);
        forged.sequences.active_sequence_id = forged
            .sequences
            .sequences
            .iter()
            .find(|sequence| sequence.id != sequence_id)
            .expect("secondary")
            .id;

        let commit = session
            .commit_project_snapshot("forged no-op", document_before.clone(), forged)
            .expect("no-op detection");

        assert!(commit.is_none());
        assert_eq!(session.document(), &document_before);
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn interleaved_project_and_sequence_history_never_rolls_back_sequence_revision() {
        let mut session = session_with_two_sequences(true);
        let sequence_id = session.document().sequences.default_sequence_id;

        let project_before = session.document().clone();
        let mut project_after = project_before.clone();
        project_after.settings.auto_save_interval += 1;
        session
            .commit_project_snapshot("change Project setting", project_before, project_after)
            .expect("Project transaction")
            .expect("Project commit");

        session
            .edit_sequence(sequence_id, "rename Sequence", |sequence| {
                sequence.name = "Renamed".to_owned();
                Ok(())
            })
            .expect("Sequence transaction");
        assert_eq!(
            session.sequence(sequence_id).expect("Sequence").revision.get(),
            2
        );

        session.undo().expect("Undo Sequence").expect("Sequence history");
        let after_sequence_undo =
            session.sequence(sequence_id).expect("Sequence after Undo").revision;
        assert_eq!(after_sequence_undo.get(), 3);

        let project_undo = session.undo().expect("Undo Project").expect("Project history");
        assert!(project_undo.changed_sequence_ids.is_empty());
        assert_eq!(
            session.sequence(sequence_id).expect("Sequence after Project Undo").revision,
            after_sequence_undo,
            "Project-only History must preserve the live Sequence revision"
        );

        let project_redo = session.redo().expect("Redo Project").expect("Project history");
        assert!(project_redo.changed_sequence_ids.is_empty());
        assert_eq!(
            session.sequence(sequence_id).expect("Sequence after Project Redo").revision,
            after_sequence_undo
        );

        session.redo().expect("Redo Sequence").expect("Sequence history");
        assert_eq!(
            session.sequence(sequence_id).expect("Sequence after Redo").revision.get(),
            4
        );
        assert_eq!(
            session.sequence(sequence_id).expect("Sequence after Redo").name,
            "Renamed"
        );
    }

    #[test]
    fn repeatedly_restored_sequence_identity_uses_session_high_water_revision() {
        let mut session = session_with_two_sequences(true);
        let restored = Sequence::new("Restored");
        let restored_id = restored.id;
        let before = session.document().clone();
        let mut after = before.clone();
        after.sequences.add_sequence(restored).expect("add Sequence candidate");
        session
            .commit_project_snapshot("add Sequence", before, after)
            .expect("add Sequence transaction")
            .expect("add Sequence commit");
        assert_eq!(
            session.sequence(restored_id).expect("added Sequence").revision.get(),
            1
        );

        session.undo().expect("Undo add").expect("add history");
        assert!(session.sequence(restored_id).is_none());
        session.redo().expect("Redo add").expect("add history");
        assert_eq!(
            session.sequence(restored_id).expect("first restore").revision.get(),
            2
        );

        session.undo().expect("Undo add again").expect("add history");
        assert!(session.sequence(restored_id).is_none());
        session.redo().expect("Redo add again").expect("add history");
        assert_eq!(
            session.sequence(restored_id).expect("second restore").revision.get(),
            3
        );
    }

    #[test]
    fn project_commit_cannot_forge_revision_for_unchanged_sequence_content() {
        let mut session = session_with_two_sequences(true);
        let sequence_id = session.document().sequences.default_sequence_id;
        let previous_revision = session.sequence(sequence_id).expect("default Sequence").revision;
        let before = session.document().clone();
        let mut after = before.clone();
        after.settings.auto_save_interval += 1;
        after.sequences.sequence_mut(sequence_id).expect("default Sequence").revision =
            SequenceRevision::new(u64::MAX).expect("forged revision");

        let commit = session
            .commit_project_snapshot("Project setting with forged revision", before, after)
            .expect("Project-only transaction")
            .expect("Project commit");

        assert!(commit.project_wide);
        assert!(commit.changed_sequence_ids.is_empty());
        assert_eq!(
            session.sequence(sequence_id).expect("default Sequence").revision,
            previous_revision
        );
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

    #[test]
    fn failed_sequence_overlay_validation_is_atomic() {
        let mut session = session_with_two_sequences(true);
        let primary_id = session.document.sequences.default_sequence_id;
        let secondary_id = session
            .document
            .sequences
            .sequences
            .iter()
            .find(|sequence| sequence.id != primary_id)
            .expect("secondary Sequence")
            .id;
        let secondary = session
            .document_mut_for_test_fixture()
            .sequences
            .sequence_mut(secondary_id)
            .expect("secondary Sequence");
        secondary.video_tracks[0]
            .add_clip(
                mondrian_timeline::Clip::new_nested_sequence(
                    primary_id,
                    TimelineTime::ZERO,
                    TimelineTime::ONE,
                    Some("Primary".to_owned()),
                )
                .expect("valid primary placement"),
            )
            .expect("add primary placement");
        session.document().validate().expect("canonical Project before edit");

        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let before = session.sequence(primary_id).expect("primary Sequence").clone();
        let mut after = before.clone();
        after.video_tracks[0]
            .add_clip(
                mondrian_timeline::Clip::new_nested_sequence(
                    secondary_id,
                    TimelineTime::ZERO,
                    TimelineTime::ONE,
                    Some("Secondary".to_owned()),
                )
                .expect("valid secondary placement"),
            )
            .expect("add secondary placement");

        let error = session
            .commit_sequence_snapshot("introduce nesting cycle", before, after)
            .expect_err("incremental dependency validation must reject the cycle");

        assert!(error.to_string().contains("检测到序列嵌套循环"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn sequence_commit_and_undo_preserve_unrelated_sequence_exactly() {
        let mut session = session_with_two_sequences(true);
        let primary_id = session.document.sequences.default_sequence_id;
        let secondary_id = session
            .document
            .sequences
            .sequences
            .iter()
            .find(|sequence| sequence.id != primary_id)
            .expect("secondary Sequence")
            .id;
        let unrelated_before =
            serde_json::to_vec(session.sequence(secondary_id).expect("unrelated Sequence"))
                .expect("serialize unrelated Sequence");

        let (_, commit) = session
            .edit_active_sequence("rename primary", |sequence| {
                sequence.name = "Renamed Primary".to_owned();
                Ok(())
            })
            .expect("commit Sequence edit");

        assert_eq!(
            commit.expect("non-empty edit commit").changed_sequence_ids,
            vec![primary_id]
        );
        assert_eq!(
            serde_json::to_vec(
                session.sequence(secondary_id).expect("unrelated Sequence after commit")
            )
            .expect("serialize unrelated Sequence after commit"),
            unrelated_before
        );

        let undo = session.undo().expect("Undo request").expect("Undo entry");

        assert_eq!(undo.changed_sequence_ids, vec![primary_id]);
        assert_eq!(
            session.sequence(primary_id).expect("restored primary Sequence").name,
            "Primary"
        );
        assert_eq!(
            serde_json::to_vec(
                session.sequence(secondary_id).expect("unrelated Sequence after Undo")
            )
            .expect("serialize unrelated Sequence after Undo"),
            unrelated_before
        );
    }

    #[test]
    fn forged_same_revision_sequence_before_snapshot_is_rejected_atomically() {
        let mut session = session_with_two_sequences(true);
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let mut forged_before = session.active_sequence().expect("active Sequence").clone();
        forged_before.name = "state that never committed".to_owned();
        let mut after = forged_before.clone();
        after.name = "proposed edit".to_owned();

        let error = session
            .commit_sequence_snapshot("forged before snapshot", forged_before, after)
            .expect_err("same revision is insufficient without exact before content");

        assert!(error.to_string().contains("content changed outside the transaction"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_author_generation_rejects_sequence_commit_before_history_or_document_changes() {
        let mut session = session_with_two_sequences(true);
        session.author_generation = AuthorGeneration(u64::MAX);
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let history_before = session.history().diagnostics();
        let before = session.active_sequence().expect("active Sequence").clone();
        let mut after = before.clone();
        after.name = "must not commit".to_owned();

        let error = session
            .commit_sequence_snapshot("generation exhaustion", before, after)
            .expect_err("exhausted generation must reject commit");

        assert!(error.to_string().contains("author generation exhausted"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), AuthorGeneration(u64::MAX));
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_sequence_revision_rejects_commit_before_history_or_document_changes() {
        let mut session = session_with_two_sequences(true);
        let sequence = session.document.sequences.active_mut().expect("active Sequence");
        sequence.revision = SequenceRevision::new(u64::MAX).expect("maximum revision");
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let before = session.active_sequence().expect("active Sequence").clone();
        let mut after = before.clone();
        after.name = "must not commit".to_owned();

        let error = session
            .commit_sequence_snapshot("revision exhaustion", before, after)
            .expect_err("exhausted revision must reject commit");

        assert!(error.to_string().contains("author revision"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_author_generation_rejects_project_commit_before_history_or_document_changes() {
        let mut session = session_with_two_sequences(true);
        session.author_generation = AuthorGeneration(u64::MAX);
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let history_before = session.history().diagnostics();
        let before = session.document().clone();
        let mut after = before.clone();
        after.settings.auto_save_interval += 1;

        let error = session
            .commit_project_snapshot("generation exhaustion", before, after)
            .expect_err("exhausted generation must reject commit");

        assert!(error.to_string().contains("author generation exhausted"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), AuthorGeneration(u64::MAX));
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_sequence_revision_rejects_multi_sequence_project_commit_atomically() {
        let mut session = session_with_two_sequences(true);
        let sequence_ids = session
            .document
            .sequences
            .sequences
            .iter()
            .map(|sequence| sequence.id)
            .collect::<Vec<_>>();
        session
            .document
            .sequences
            .sequence_mut(sequence_ids[1])
            .expect("second Sequence")
            .revision = SequenceRevision::new(u64::MAX).expect("maximum revision");
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let before = session.document().clone();
        let mut after = before.clone();
        after.sequences.sequence_mut(sequence_ids[0]).expect("first Sequence").name =
            "First changed".to_owned();
        after.sequences.sequence_mut(sequence_ids[1]).expect("second Sequence").name =
            "Second changed".to_owned();

        let error = session
            .commit_project_snapshot("multi-Sequence revision exhaustion", before, after)
            .expect_err("one exhausted Sequence must reject the whole Project transaction");

        assert!(error.to_string().contains("author revision"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_author_generation_rejects_undo_without_moving_history_or_document() {
        let mut session = session_with_two_sequences(true);
        session
            .edit_active_sequence("rename", |sequence| {
                sequence.name = "Renamed".to_owned();
                Ok(())
            })
            .expect("rename");
        session.author_generation = AuthorGeneration(u64::MAX);
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let history_before = session.history().diagnostics();

        let error = session.undo().expect_err("exhausted generation must reject Undo");

        assert!(error.to_string().contains("author generation exhausted"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), AuthorGeneration(u64::MAX));
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_sequence_revision_rejects_prepared_undo_without_moving_history_or_document() {
        let mut session = session_with_two_sequences(true);
        session
            .edit_active_sequence("rename", |sequence| {
                sequence.name = "Renamed".to_owned();
                Ok(())
            })
            .expect("rename");
        session.document.sequences.active_mut().expect("active Sequence").revision =
            SequenceRevision::new(u64::MAX).expect("maximum revision");
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();

        let error = session.undo().expect_err("exhausted revision must reject Undo");

        assert!(error.to_string().contains("author revision"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_sequence_revision_rejects_redo_without_moving_history_or_document() {
        let mut session = session_with_two_sequences(true);
        session
            .edit_active_sequence("rename", |sequence| {
                sequence.name = "Renamed".to_owned();
                Ok(())
            })
            .expect("rename");
        session.undo().expect("Undo").expect("Undo entry");
        session.document.sequences.active_mut().expect("active Sequence").revision =
            SequenceRevision::new(u64::MAX).expect("maximum revision");
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();

        let error = session.redo().expect_err("exhausted revision must reject Redo");

        assert!(error.to_string().contains("author revision"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
    }

    #[test]
    fn exhausted_high_water_rejects_project_undo_atomically() {
        let mut session = session_with_two_sequences(true);
        let sequence_id = session.document().sequences.default_sequence_id;
        let before = session.document().clone();
        let mut after = before.clone();
        after.sequences.sequence_mut(sequence_id).expect("default Sequence").name =
            "Project-renamed".to_owned();
        session
            .commit_project_snapshot("Project Sequence rename", before, after)
            .expect("Project transaction")
            .expect("Project commit");
        session.sequence_revision_high_water.insert(
            sequence_id,
            SequenceRevision::new(u64::MAX).expect("maximum revision"),
        );
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let high_water_before = session.sequence_revision_high_water.clone();
        let navigation_before = session.navigation_stack.clone();

        let error = session.undo().expect_err("exhausted high-water must reject Project Undo");

        assert!(error.to_string().contains("author revision"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
        assert_eq!(session.sequence_revision_high_water, high_water_before);
        assert_eq!(session.navigation_stack, navigation_before);
    }

    #[test]
    fn exhausted_tombstone_high_water_rejects_project_redo_atomically() {
        let mut session = session_with_two_sequences(true);
        let restored = Sequence::new("Restored");
        let restored_id = restored.id;
        let before = session.document().clone();
        let mut after = before.clone();
        after.sequences.add_sequence(restored).expect("add Sequence candidate");
        session
            .commit_project_snapshot("add Sequence", before, after)
            .expect("Project transaction")
            .expect("Project commit");
        session.undo().expect("Undo add").expect("add History");
        assert!(session.sequence(restored_id).is_none());
        session.sequence_revision_high_water.insert(
            restored_id,
            SequenceRevision::new(u64::MAX).expect("maximum revision"),
        );
        let document_before = serde_json::to_vec(session.document()).expect("serialize document");
        let generation_before = session.author_generation();
        let history_before = session.history().diagnostics();
        let high_water_before = session.sequence_revision_high_water.clone();
        let navigation_before = session.navigation_stack.clone();

        let error = session.redo().expect_err("exhausted tombstone must reject Project Redo");

        assert!(error.to_string().contains("author revision"));
        assert_eq!(
            serde_json::to_vec(session.document()).expect("serialize unchanged document"),
            document_before
        );
        assert_eq!(session.author_generation(), generation_before);
        assert_eq!(session.history().diagnostics(), history_before);
        assert_eq!(session.sequence_revision_high_water, high_water_before);
        assert_eq!(session.navigation_stack, navigation_before);
    }
}
