//! Bounded project-wide Undo/Redo history for committed authoring transactions.
//!
//! Common single-Sequence edits retain only that Sequence. Structural edits that
//! alter the Sequence collection or project settings retain a complete canonical
//! document snapshot. Both forms cross the same project-document Interface.

use mondrian_core::{MondrianError, Result, SequenceId};
use mondrian_project::ProjectDocument;
use mondrian_timeline::Sequence;
use std::collections::VecDeque;
use std::mem::size_of;

/// Default maximum number of retained authoring transactions.
pub const DEFAULT_AUTHORING_HISTORY_MAX_ENTRIES: usize = 200;

/// Default retained payload budget for Undo and Redo combined.
pub const DEFAULT_AUTHORING_HISTORY_RETAINED_BYTES: usize = 128 * 1024 * 1024;

/// Hard retention limits for one authoring history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringHistoryDiagnostics {
    /// Number of retained Undo entries.
    pub undo_entries: usize,
    /// Number of retained Redo entries.
    pub redo_entries: usize,
    /// Retained command payload bytes.
    pub retained_bytes: usize,
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
}

/// Effect returned after applying an Undo or Redo entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthoringHistoryApplication {
    pub(crate) affected_sequence_ids: Vec<SequenceId>,
    pub(crate) project_wide: bool,
}

#[derive(Debug)]
enum AuthoringCommand {
    Sequence {
        description: Box<str>,
        sequence_id: SequenceId,
        before: Box<[u8]>,
        after: Box<[u8]>,
    },
    Project {
        description: Box<str>,
        affected_sequence_ids: Box<[SequenceId]>,
        before: Box<[u8]>,
        after: Box<[u8]>,
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
            before: serde_json::to_vec(before)?.into_boxed_slice(),
            after: serde_json::to_vec(after)?.into_boxed_slice(),
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
        Ok(Self::Project {
            description: description.into().into_boxed_str(),
            affected_sequence_ids: affected_sequence_ids.to_vec().into_boxed_slice(),
            before: serde_json::to_vec(before)?.into_boxed_slice(),
            after: serde_json::to_vec(after)?.into_boxed_slice(),
        })
    }

    fn description(&self) -> &str {
        match self {
            Self::Sequence { description, .. } | Self::Project { description, .. } => description,
        }
    }

    fn retained_bytes(&self) -> usize {
        let (description, before, after, ids) = match self {
            Self::Sequence { description, before, after, .. } => {
                (description.len(), before.len(), after.len(), 0)
            }
            Self::Project { description, affected_sequence_ids, before, after } => (
                description.len(),
                before.len(),
                after.len(),
                affected_sequence_ids.len().saturating_mul(size_of::<SequenceId>()),
            ),
        };
        size_of::<Self>()
            .saturating_add(description)
            .saturating_add(before)
            .saturating_add(after)
            .saturating_add(ids)
    }

    fn restore_before(
        &self,
        document: &mut ProjectDocument,
    ) -> Result<AuthoringHistoryApplication> {
        self.restore(document, false)
    }

    fn restore_after(&self, document: &mut ProjectDocument) -> Result<AuthoringHistoryApplication> {
        self.restore(document, true)
    }

    fn restore(
        &self,
        document: &mut ProjectDocument,
        use_after: bool,
    ) -> Result<AuthoringHistoryApplication> {
        match self {
            Self::Sequence { sequence_id, before, after, .. } => {
                let bytes = if use_after { after } else { before };
                let restored: Sequence = serde_json::from_slice(bytes)?;
                if restored.id != *sequence_id {
                    return Err(authoring_error(
                        "restore_sequence_authoring_command",
                        format!(
                            "snapshot identity changed from {sequence_id} to {}",
                            restored.id
                        ),
                    ));
                }
                let target = document.sequences.sequence_mut(*sequence_id).ok_or_else(|| {
                    authoring_error(
                        "restore_sequence_authoring_command",
                        format!("target Sequence no longer exists: {sequence_id}"),
                    )
                })?;
                *target = restored;
                Ok(AuthoringHistoryApplication {
                    affected_sequence_ids: vec![*sequence_id],
                    project_wide: false,
                })
            }
            Self::Project { affected_sequence_ids, before, after, .. } => {
                let bytes = if use_after { after } else { before };
                let mut restored: ProjectDocument = serde_json::from_slice(bytes)?;
                if restored.project_id != document.project_id {
                    return Err(authoring_error(
                        "restore_project_authoring_command",
                        format!(
                            "snapshot Project identity changed from {} to {}",
                            document.project_id, restored.project_id
                        ),
                    ));
                }
                // Persistence revision is not authored content and never rolls back with Undo.
                restored.document_revision = document.document_revision;
                *document = restored;
                Ok(AuthoringHistoryApplication {
                    affected_sequence_ids: affected_sequence_ids.to_vec(),
                    project_wide: true,
                })
            }
        }
    }
}

#[derive(Debug)]
struct HistoryEntry {
    retained_bytes: usize,
    command: AuthoringCommand,
}

impl HistoryEntry {
    fn new(command: AuthoringCommand) -> Self {
        let retained_bytes = command.retained_bytes().max(size_of::<Self>());
        Self { retained_bytes, command }
    }
}

/// One project-wide bounded Undo/Redo history.
#[derive(Debug)]
pub struct AuthoringHistory {
    undo_stack: VecDeque<HistoryEntry>,
    redo_stack: VecDeque<HistoryEntry>,
    budget: AuthoringHistoryBudget,
    retained_bytes: usize,
    budget_evicted_entries: u64,
    budget_evicted_bytes: u64,
    branch_discarded_entries: u64,
    oversize_dropped_entries: u64,
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
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            budget,
            retained_bytes: 0,
            budget_evicted_entries: 0,
            budget_evicted_bytes: 0,
            branch_discarded_entries: 0,
            oversize_dropped_entries: 0,
        }
    }

    pub(crate) fn record_sequence(
        &mut self,
        description: impl Into<String>,
        before: &Sequence,
        after: &Sequence,
    ) -> Result<AuthoringHistoryRecordOutcome> {
        self.record(AuthoringCommand::sequence(description, before, after)?)
    }

    pub(crate) fn record_project(
        &mut self,
        description: impl Into<String>,
        affected_sequence_ids: &[SequenceId],
        before: &ProjectDocument,
        after: &ProjectDocument,
    ) -> Result<AuthoringHistoryRecordOutcome> {
        self.record(AuthoringCommand::project(
            description,
            affected_sequence_ids,
            before,
            after,
        )?)
    }

    fn record(&mut self, command: AuthoringCommand) -> Result<AuthoringHistoryRecordOutcome> {
        let entry = HistoryEntry::new(command);
        let branch_discarded_entries = self.redo_stack.len();
        let branch_discarded_bytes =
            self.redo_stack.iter().map(|entry| entry.retained_bytes).sum::<usize>();
        self.redo_stack.clear();
        self.retained_bytes = self.retained_bytes.saturating_sub(branch_discarded_bytes);
        self.branch_discarded_entries =
            self.branch_discarded_entries.saturating_add(branch_discarded_entries as u64);

        if self.budget.max_entries == 0 || entry.retained_bytes > self.budget.max_retained_bytes {
            self.oversize_dropped_entries = self.oversize_dropped_entries.saturating_add(1);
            tracing::warn!(
                command = entry.command.description(),
                command_retained_bytes = entry.retained_bytes,
                history_budget_bytes = self.budget.max_retained_bytes,
                "committed authoring transaction exceeds the Undo budget"
            );
            return Ok(AuthoringHistoryRecordOutcome {
                retained: false,
                budget_evicted_entries: 0,
                budget_evicted_bytes: 0,
                branch_discarded_entries,
            });
        }

        self.retained_bytes = self.retained_bytes.saturating_add(entry.retained_bytes);
        self.undo_stack.push_back(entry);
        let mut budget_evicted_entries = 0usize;
        let mut budget_evicted_bytes = 0usize;
        while self.undo_stack.len() + self.redo_stack.len() > self.budget.max_entries
            || self.retained_bytes > self.budget.max_retained_bytes
        {
            let Some(evicted) = self.undo_stack.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(evicted.retained_bytes);
            budget_evicted_entries = budget_evicted_entries.saturating_add(1);
            budget_evicted_bytes = budget_evicted_bytes.saturating_add(evicted.retained_bytes);
        }
        self.budget_evicted_entries =
            self.budget_evicted_entries.saturating_add(budget_evicted_entries as u64);
        self.budget_evicted_bytes =
            self.budget_evicted_bytes.saturating_add(budget_evicted_bytes as u64);
        Ok(AuthoringHistoryRecordOutcome {
            retained: true,
            budget_evicted_entries,
            budget_evicted_bytes,
            branch_discarded_entries,
        })
    }

    pub(crate) fn undo(
        &mut self,
        document: &mut ProjectDocument,
    ) -> Result<Option<AuthoringHistoryApplication>> {
        let Some(entry) = self.undo_stack.pop_back() else {
            return Ok(None);
        };
        match entry.command.restore_before(document) {
            Ok(application) => {
                self.redo_stack.push_back(entry);
                Ok(Some(application))
            }
            Err(error) => {
                self.undo_stack.push_back(entry);
                Err(error)
            }
        }
    }

    pub(crate) fn redo(
        &mut self,
        document: &mut ProjectDocument,
    ) -> Result<Option<AuthoringHistoryApplication>> {
        let Some(entry) = self.redo_stack.pop_back() else {
            return Ok(None);
        };
        match entry.command.restore_after(document) {
            Ok(application) => {
                self.undo_stack.push_back(entry);
                Ok(Some(application))
            }
            Err(error) => {
                self.redo_stack.push_back(entry);
                Err(error)
            }
        }
    }

    /// Whether an Undo entry exists.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether a Redo entry exists.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// User-facing label for the next Undo operation.
    pub fn undo_description(&self) -> Option<&str> {
        self.undo_stack.back().map(|entry| entry.command.description())
    }

    /// User-facing label for the next Redo operation.
    pub fn redo_description(&self) -> Option<&str> {
        self.redo_stack.back().map(|entry| entry.command.description())
    }

    /// Return bounded-history evidence.
    pub fn diagnostics(&self) -> AuthoringHistoryDiagnostics {
        AuthoringHistoryDiagnostics {
            undo_entries: self.undo_stack.len(),
            redo_entries: self.redo_stack.len(),
            retained_bytes: self.retained_bytes,
            budget: self.budget,
            undo_description: self.undo_description().map(ToOwned::to_owned),
            redo_description: self.redo_description().map(ToOwned::to_owned),
            budget_evicted_entries: self.budget_evicted_entries,
            budget_evicted_bytes: self.budget_evicted_bytes,
            branch_discarded_entries: self.branch_discarded_entries,
            oversize_dropped_entries: self.oversize_dropped_entries,
        }
    }
}

fn authoring_error(step_id: &str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}
