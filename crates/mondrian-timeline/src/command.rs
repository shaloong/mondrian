//! Bounded undo/redo history for Sequence authoring transactions.
//!
//! Commands declare both their target Sequence and their command-owned retained
//! bytes. The history therefore has one enforceable memory contract rather than
//! an entry-count limit that hides arbitrarily large snapshot allocations.

use crate::sequence::Sequence;
use mondrian_core::{MondrianError, Result, SequenceId};
use std::collections::VecDeque;
use std::mem::size_of;

/// Default maximum number of retained undo/redo commands.
pub const DEFAULT_COMMAND_HISTORY_MAX_ENTRIES: usize = 200;

/// Default command-owned retained-memory budget (128 MiB).
///
/// This budget covers command objects and their owned snapshot payloads. It
/// intentionally excludes allocator bookkeeping and the small `VecDeque`
/// pointer buffers, which are bounded by [`DEFAULT_COMMAND_HISTORY_MAX_ENTRIES`].
pub const DEFAULT_COMMAND_HISTORY_RETAINED_BYTES: usize = 128 * 1024 * 1024;

/// Hard retention limits for one Sequence command history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandHistoryBudget {
    /// Maximum total commands across the undo and redo stacks.
    pub max_entries: usize,
    /// Maximum command-owned bytes across the undo and redo stacks.
    pub max_retained_bytes: usize,
}

impl CommandHistoryBudget {
    /// Construct an explicit history budget.
    pub const fn new(max_entries: usize, max_retained_bytes: usize) -> Self {
        Self { max_entries, max_retained_bytes }
    }
}

impl Default for CommandHistoryBudget {
    fn default() -> Self {
        Self::new(
            DEFAULT_COMMAND_HISTORY_MAX_ENTRIES,
            DEFAULT_COMMAND_HISTORY_RETAINED_BYTES,
        )
    }
}

/// Immutable current and cumulative evidence for one command history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandHistoryDiagnostics {
    /// Sequence identity accepted by this history, if it has accepted a command.
    pub sequence_id: Option<SequenceId>,
    /// Current undo stack length.
    pub undo_entries: usize,
    /// Current redo stack length.
    pub redo_entries: usize,
    /// Current command-owned bytes retained by both stacks.
    pub retained_bytes: usize,
    /// Active hard limits.
    pub budget: CommandHistoryBudget,
    /// Oldest commands evicted to satisfy an entry or byte limit.
    pub budget_evicted_entries: u64,
    /// Command-owned bytes released by budget eviction.
    pub budget_evicted_bytes: u64,
    /// Redo commands discarded because a new authoring branch was committed.
    pub branch_discarded_entries: u64,
    /// Command-owned bytes released by branch replacement.
    pub branch_discarded_bytes: u64,
    /// New commands too large to fit even in an otherwise empty history.
    pub oversize_dropped_entries: u64,
    /// Command-owned bytes represented by oversize dropped commands.
    pub oversize_dropped_bytes: u64,
}

/// Evidence returned when an already executed command is offered to history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRecordOutcome {
    /// Whether the new command remains available for undo.
    pub retained: bool,
    /// Commands evicted from the oldest end to admit this command.
    pub budget_evicted_entries: usize,
    /// Command-owned bytes released by this admission's budget eviction.
    pub budget_evicted_bytes: usize,
    /// Redo commands discarded by the newly committed branch.
    pub branch_discarded_entries: usize,
    /// Command-owned bytes released from the redo stack.
    pub branch_discarded_bytes: usize,
    /// Current evidence after admission.
    pub diagnostics: CommandHistoryDiagnostics,
}

/// Reversible authoring command admitted by [`CommandHistory`].
pub trait Command: Send + Sync + std::fmt::Debug {
    /// Apply the command to its target Sequence.
    fn execute(&mut self, sequence: &mut Sequence) -> Result<()>;
    /// Restore the state preceding the command.
    fn undo(&mut self, sequence: &mut Sequence) -> Result<()>;
    /// Stable target identity. A history never applies a command to another Sequence.
    fn target_sequence_id(&self) -> SequenceId;
    /// Command-owned bytes retained while this command is in either stack.
    fn retained_bytes(&self) -> usize;
    /// User-facing operation label.
    fn description(&self) -> &str;
}

/// Complete Sequence snapshots stored as exact, bounded byte payloads.
///
/// Snapshot bytes are deserialized only at Undo/Redo time. This removes the
/// unaccounted heap graph previously retained by two cloned Sequences while
/// keeping the history Interface open to smaller delta commands later.
#[derive(Debug, Clone)]
pub struct SequenceSnapshotCommand {
    description: Box<str>,
    sequence_id: SequenceId,
    before: Box<[u8]>,
    after: Box<[u8]>,
}

impl SequenceSnapshotCommand {
    /// Serialize a validated before/after pair into an exactly measurable command.
    pub fn new(
        description: impl Into<String>,
        before: &Sequence,
        after: &Sequence,
    ) -> Result<Self> {
        if before.id != after.id {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "create_sequence_snapshot_command".to_owned(),
                reason: format!(
                    "snapshot identity changed from {} to {}",
                    before.id, after.id
                ),
            });
        }
        Ok(Self {
            description: description.into().into_boxed_str(),
            sequence_id: before.id,
            before: serde_json::to_vec(before)?.into_boxed_slice(),
            after: serde_json::to_vec(after)?.into_boxed_slice(),
        })
    }

    fn restore(&self, sequence: &mut Sequence, snapshot: &[u8]) -> Result<()> {
        if sequence.id != self.sequence_id {
            return Err(sequence_identity_error(self.sequence_id, sequence.id));
        }
        let restored: Sequence = serde_json::from_slice(snapshot)?;
        if restored.id != self.sequence_id {
            return Err(sequence_identity_error(self.sequence_id, restored.id));
        }
        *sequence = restored;
        Ok(())
    }
}

impl Command for SequenceSnapshotCommand {
    fn execute(&mut self, sequence: &mut Sequence) -> Result<()> {
        self.restore(sequence, &self.after)
    }

    fn undo(&mut self, sequence: &mut Sequence) -> Result<()> {
        self.restore(sequence, &self.before)
    }

    fn target_sequence_id(&self) -> SequenceId {
        self.sequence_id
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.description.len())
            .saturating_add(self.before.len())
            .saturating_add(self.after.len())
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[derive(Debug)]
struct CommandEntry {
    retained_bytes: usize,
    command: Box<dyn Command>,
}

impl CommandEntry {
    fn new(command: Box<dyn Command>) -> Self {
        let retained_bytes = command.retained_bytes().max(size_of::<Self>());
        Self { retained_bytes, command }
    }
}

/// One target-scoped undo/redo history with hard entry and retained-byte limits.
#[derive(Debug)]
pub struct CommandHistory {
    undo_stack: VecDeque<CommandEntry>,
    redo_stack: VecDeque<CommandEntry>,
    budget: CommandHistoryBudget,
    sequence_id: Option<SequenceId>,
    retained_bytes: usize,
    budget_evicted_entries: u64,
    budget_evicted_bytes: u64,
    branch_discarded_entries: u64,
    branch_discarded_bytes: u64,
    oversize_dropped_entries: u64,
    oversize_dropped_bytes: u64,
}

impl Default for CommandHistory {
    fn default() -> Self {
        Self::with_budget(CommandHistoryBudget::default())
    }
}

impl CommandHistory {
    /// Construct an empty history with explicit hard limits.
    pub fn with_budget(budget: CommandHistoryBudget) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            budget,
            sequence_id: None,
            retained_bytes: 0,
            budget_evicted_entries: 0,
            budget_evicted_bytes: 0,
            branch_discarded_entries: 0,
            branch_discarded_bytes: 0,
            oversize_dropped_entries: 0,
            oversize_dropped_bytes: 0,
        }
    }

    /// Execute and then retain a command. Execution errors leave history unchanged.
    pub fn execute(
        &mut self,
        mut command: Box<dyn Command>,
        sequence: &mut Sequence,
    ) -> Result<CommandRecordOutcome> {
        self.validate_target(command.target_sequence_id(), sequence.id)?;
        command.execute(sequence)?;
        self.record_executed(command)
    }

    /// Record a command whose authoring mutation has already committed.
    pub fn record_executed(&mut self, command: Box<dyn Command>) -> Result<CommandRecordOutcome> {
        let target = command.target_sequence_id();
        self.bind_or_validate_target(target)?;
        let entry = CommandEntry::new(command);

        let (branch_discarded_entries, branch_discarded_bytes) = self.clear_redo_branch();
        self.branch_discarded_entries =
            self.branch_discarded_entries.saturating_add(branch_discarded_entries as u64);
        self.branch_discarded_bytes =
            self.branch_discarded_bytes.saturating_add(branch_discarded_bytes as u64);

        if self.budget.max_entries == 0 || entry.retained_bytes > self.budget.max_retained_bytes {
            self.oversize_dropped_entries = self.oversize_dropped_entries.saturating_add(1);
            self.oversize_dropped_bytes =
                self.oversize_dropped_bytes.saturating_add(entry.retained_bytes as u64);
            tracing::warn!(
                sequence_id = %target,
                command = entry.command.description(),
                command_retained_bytes = entry.retained_bytes,
                history_budget_bytes = self.budget.max_retained_bytes,
                history_budget_entries = self.budget.max_entries,
                "undo command exceeds the configured history budget and was not retained"
            );
            return Ok(CommandRecordOutcome {
                retained: false,
                budget_evicted_entries: 0,
                budget_evicted_bytes: 0,
                branch_discarded_entries,
                branch_discarded_bytes,
                diagnostics: self.diagnostics(),
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
        if budget_evicted_entries > 0 {
            tracing::info!(
                sequence_id = %target,
                budget_evicted_entries,
                budget_evicted_bytes,
                history_retained_bytes = self.retained_bytes,
                "evicted oldest undo commands to satisfy history budget"
            );
        }

        Ok(CommandRecordOutcome {
            retained: true,
            budget_evicted_entries,
            budget_evicted_bytes,
            branch_discarded_entries,
            branch_discarded_bytes,
            diagnostics: self.diagnostics(),
        })
    }

    /// Undo the newest command. A failed restore is returned to the same stack.
    pub fn undo(&mut self, sequence: &mut Sequence) -> Result<bool> {
        self.validate_bound_sequence(sequence.id)?;
        let Some(mut entry) = self.undo_stack.pop_back() else {
            return Ok(false);
        };
        tracing::debug!(
            sequence_id = %sequence.id,
            command = entry.command.description(),
            "undo"
        );
        if let Err(error) = entry.command.undo(sequence) {
            self.undo_stack.push_back(entry);
            return Err(error);
        }
        self.redo_stack.push_back(entry);
        Ok(true)
    }

    /// Redo the newest undone command. A failed restore is returned to the same stack.
    pub fn redo(&mut self, sequence: &mut Sequence) -> Result<bool> {
        self.validate_bound_sequence(sequence.id)?;
        let Some(mut entry) = self.redo_stack.pop_back() else {
            return Ok(false);
        };
        tracing::debug!(
            sequence_id = %sequence.id,
            command = entry.command.description(),
            "redo"
        );
        if let Err(error) = entry.command.execute(sequence) {
            self.redo_stack.push_back(entry);
            return Err(error);
        }
        self.undo_stack.push_back(entry);
        Ok(true)
    }

    /// Whether at least one command can be undone.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether at least one command can be redone.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// User-facing label for the newest undo command.
    pub fn undo_description(&self) -> Option<&str> {
        self.undo_stack.back().map(|entry| entry.command.description())
    }

    /// User-facing label for the newest redo command.
    pub fn redo_description(&self) -> Option<&str> {
        self.redo_stack.back().map(|entry| entry.command.description())
    }

    /// Return current and cumulative retention evidence.
    pub fn diagnostics(&self) -> CommandHistoryDiagnostics {
        CommandHistoryDiagnostics {
            sequence_id: self.sequence_id,
            undo_entries: self.undo_stack.len(),
            redo_entries: self.redo_stack.len(),
            retained_bytes: self.retained_bytes,
            budget: self.budget,
            budget_evicted_entries: self.budget_evicted_entries,
            budget_evicted_bytes: self.budget_evicted_bytes,
            branch_discarded_entries: self.branch_discarded_entries,
            branch_discarded_bytes: self.branch_discarded_bytes,
            oversize_dropped_entries: self.oversize_dropped_entries,
            oversize_dropped_bytes: self.oversize_dropped_bytes,
        }
    }

    fn bind_or_validate_target(&mut self, target: SequenceId) -> Result<()> {
        match self.sequence_id {
            Some(bound) if bound != target => Err(sequence_identity_error(bound, target)),
            Some(_) => Ok(()),
            None => {
                self.sequence_id = Some(target);
                Ok(())
            }
        }
    }

    fn validate_target(&self, command_target: SequenceId, actual: SequenceId) -> Result<()> {
        if command_target != actual {
            return Err(sequence_identity_error(command_target, actual));
        }
        self.validate_bound_sequence(actual)
    }

    fn validate_bound_sequence(&self, actual: SequenceId) -> Result<()> {
        if let Some(bound) = self.sequence_id {
            if bound != actual {
                return Err(sequence_identity_error(bound, actual));
            }
        }
        Ok(())
    }

    fn clear_redo_branch(&mut self) -> (usize, usize) {
        let entries = self.redo_stack.len();
        let bytes = self.redo_stack.iter().fold(0usize, |total, entry| {
            total.saturating_add(entry.retained_bytes)
        });
        self.redo_stack.clear();
        self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
        (entries, bytes)
    }
}

fn sequence_identity_error(expected: SequenceId, actual: SequenceId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "sequence_command_target".to_owned(),
        reason: format!("command targets Sequence {expected}, received {actual}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FailingUndoCommand {
        target: SequenceId,
    }

    impl Command for FailingUndoCommand {
        fn execute(&mut self, _sequence: &mut Sequence) -> Result<()> {
            Ok(())
        }

        fn undo(&mut self, _sequence: &mut Sequence) -> Result<()> {
            Err(MondrianError::WorkflowStepFailed {
                step_id: "failing_test_command".to_owned(),
                reason: "injected undo failure".to_owned(),
            })
        }

        fn target_sequence_id(&self) -> SequenceId {
            self.target
        }

        fn retained_bytes(&self) -> usize {
            size_of::<Self>()
        }

        fn description(&self) -> &str {
            "failing undo"
        }
    }

    fn renamed_command(before: &Sequence, name: &str) -> SequenceSnapshotCommand {
        let mut after = before.clone();
        after.name = name.to_owned();
        SequenceSnapshotCommand::new("rename", before, &after).expect("serializable sequence")
    }

    #[test]
    fn snapshot_command_round_trips_through_bounded_history() {
        let before = Sequence::new("Before");
        let mut after = before.clone();
        after.name = "After".to_owned();
        let command =
            SequenceSnapshotCommand::new("rename", &before, &after).expect("serializable sequence");
        let command_bytes = command.retained_bytes();
        let mut history = CommandHistory::with_budget(CommandHistoryBudget::new(4, command_bytes));

        let outcome = history.execute(Box::new(command), &mut after).expect("execute command");
        assert!(outcome.retained);
        assert_eq!(history.diagnostics().retained_bytes, command_bytes);
        assert!(history.undo(&mut after).expect("undo"));
        assert_eq!(after.name, "Before");
        assert!(history.redo(&mut after).expect("redo"));
        assert_eq!(after.name, "After");
        assert_eq!(history.diagnostics().retained_bytes, command_bytes);
    }

    #[test]
    fn byte_budget_evicts_the_oldest_command_and_reports_exact_payload() {
        let mut sequence = Sequence::new("A");
        let first = renamed_command(&sequence, "B");
        let first_bytes = first.retained_bytes();
        let mut second_before = sequence.clone();
        second_before.name = "B".to_owned();
        let second = renamed_command(&second_before, "C");
        let second_bytes = second.retained_bytes();
        let budget = first_bytes.max(second_bytes);
        let mut history = CommandHistory::with_budget(CommandHistoryBudget::new(8, budget));

        sequence.name = "B".to_owned();
        history.record_executed(Box::new(first)).expect("record first");
        sequence.name = "C".to_owned();
        let outcome = history.record_executed(Box::new(second)).expect("record second");

        assert!(outcome.retained);
        assert_eq!(outcome.budget_evicted_entries, 1);
        assert_eq!(outcome.budget_evicted_bytes, first_bytes);
        assert_eq!(outcome.diagnostics.undo_entries, 1);
        assert_eq!(outcome.diagnostics.retained_bytes, second_bytes);
        assert!(outcome.diagnostics.retained_bytes <= budget);
        assert!(history.undo(&mut sequence).expect("undo newest"));
        assert_eq!(sequence.name, "B");
        assert!(!history.undo(&mut sequence).expect("oldest was evicted"));
    }

    #[test]
    fn oversize_command_is_not_retained_and_cannot_break_the_hard_limit() {
        let sequence = Sequence::new("A");
        let command = renamed_command(&sequence, "B");
        let command_bytes = command.retained_bytes();
        let mut history = CommandHistory::with_budget(CommandHistoryBudget::new(
            8,
            command_bytes.saturating_sub(1),
        ));

        let outcome = history.record_executed(Box::new(command)).expect("valid target");

        assert!(!outcome.retained);
        assert_eq!(outcome.diagnostics.retained_bytes, 0);
        assert_eq!(outcome.diagnostics.oversize_dropped_entries, 1);
        assert_eq!(
            outcome.diagnostics.oversize_dropped_bytes,
            command_bytes as u64
        );
        assert!(!history.can_undo());
    }

    #[test]
    fn new_branch_discards_redo_bytes_without_changing_the_budget_total() {
        let mut sequence = Sequence::new("A");
        let first = renamed_command(&sequence, "B");
        let first_bytes = first.retained_bytes();
        sequence.name = "B".to_owned();
        let second = renamed_command(&sequence, "C");
        let second_bytes = second.retained_bytes();
        let mut history = CommandHistory::with_budget(CommandHistoryBudget::new(
            8,
            first_bytes.saturating_add(second_bytes).saturating_mul(2),
        ));
        history.record_executed(Box::new(first)).expect("record first");
        sequence.name = "C".to_owned();
        history.record_executed(Box::new(second)).expect("record second");
        history.undo(&mut sequence).expect("undo second");

        let branch = renamed_command(&sequence, "D");
        let branch_bytes = branch.retained_bytes();
        sequence.name = "D".to_owned();
        let outcome = history.record_executed(Box::new(branch)).expect("record branch");

        assert_eq!(outcome.branch_discarded_entries, 1);
        assert_eq!(outcome.branch_discarded_bytes, second_bytes);
        assert_eq!(outcome.diagnostics.redo_entries, 0);
        assert_eq!(
            outcome.diagnostics.retained_bytes,
            first_bytes.saturating_add(branch_bytes)
        );
    }

    #[test]
    fn target_mismatch_fails_without_consuming_the_command() {
        let first = Sequence::new("First");
        let second = Sequence::new("Second");
        let command = renamed_command(&first, "Renamed");
        let mut history = CommandHistory::default();
        history.record_executed(Box::new(command)).expect("record first target");

        let error = history.undo(&mut second.clone()).expect_err("wrong Sequence must fail");
        assert!(error.to_string().contains(&first.id.to_string()));
        assert!(history.can_undo());
    }

    #[test]
    fn snapshot_constructor_rejects_identity_replacement() {
        let before = Sequence::new("Before");
        let after = Sequence::new("After");
        let error = SequenceSnapshotCommand::new("replace", &before, &after)
            .expect_err("identity replacement is not one Sequence command");
        assert!(error.to_string().contains("snapshot identity changed"));
    }

    #[test]
    fn failed_undo_returns_the_command_to_its_original_stack() {
        let mut sequence = Sequence::new("failure");
        let mut history = CommandHistory::default();
        history
            .record_executed(Box::new(FailingUndoCommand { target: sequence.id }))
            .expect("record command");
        let retained = history.diagnostics().retained_bytes;

        let error = history.undo(&mut sequence).expect_err("injected failure");

        assert!(error.to_string().contains("injected undo failure"));
        assert_eq!(history.diagnostics().undo_entries, 1);
        assert_eq!(history.diagnostics().redo_entries, 0);
        assert_eq!(history.diagnostics().retained_bytes, retained);
    }
}
