//! Command 模式撤销/重做系统

use crate::sequence::Sequence;
use mondrian_core::Result;

/// 可撤销命令 Trait
pub trait Command: Send + Sync + std::fmt::Debug {
    fn execute(&mut self, seq: &mut Sequence) -> Result<()>;
    fn undo(&mut self, seq: &mut Sequence) -> Result<()>;
    fn description(&self) -> &str;
}

/// 基于 Sequence 前后快照的最小命令实现
#[derive(Debug, Clone)]
pub struct SequenceSnapshotCommand {
    description: String,
    before: Sequence,
    after: Sequence,
}

impl SequenceSnapshotCommand {
    pub fn new(description: impl Into<String>, before: Sequence, after: Sequence) -> Self {
        Self { description: description.into(), before, after }
    }
}

impl Command for SequenceSnapshotCommand {
    fn execute(&mut self, seq: &mut Sequence) -> Result<()> {
        *seq = self.after.clone();
        Ok(())
    }

    fn undo(&mut self, seq: &mut Sequence) -> Result<()> {
        *seq = self.before.clone();
        Ok(())
    }

    fn description(&self) -> &str {
        &self.description
    }
}

/// 命令历史管理器（最大 200 步）
pub struct CommandHistory {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    max_history: usize,
}

impl Default for CommandHistory {
    fn default() -> Self {
        Self::new(200)
    }
}

impl CommandHistory {
    pub fn new(max_history: usize) -> Self {
        Self {
            undo_stack: vec![],
            redo_stack: vec![],
            max_history,
        }
    }

    /// 执行命令并推入撤销栈
    pub fn execute(&mut self, mut cmd: Box<dyn Command>, seq: &mut Sequence) -> Result<()> {
        cmd.execute(seq)?;
        self.push_executed(cmd);
        Ok(())
    }

    /// 记录一个已执行完成的命令（例如外部先修改了 Sequence，再补记历史）
    pub fn record_executed(&mut self, cmd: Box<dyn Command>) {
        self.push_executed(cmd);
    }

    /// 撤销最后一个命令
    pub fn undo(&mut self, seq: &mut Sequence) -> Result<bool> {
        if let Some(mut cmd) = self.undo_stack.pop() {
            tracing::debug!("Undo: {}", cmd.description());
            cmd.undo(seq)?;
            self.redo_stack.push(cmd);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 重做
    pub fn redo(&mut self, seq: &mut Sequence) -> Result<bool> {
        if let Some(mut cmd) = self.redo_stack.pop() {
            tracing::debug!("Redo: {}", cmd.description());
            cmd.execute(seq)?;
            self.undo_stack.push(cmd);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_description(&self) -> Option<&str> {
        self.undo_stack.last().map(|c| c.description())
    }
    pub fn redo_description(&self) -> Option<&str> {
        self.redo_stack.last().map(|c| c.description())
    }

    fn push_executed(&mut self, cmd: Box<dyn Command>) {
        self.undo_stack.push(cmd);
        self.redo_stack.clear(); // 新命令后清空重做栈

        // 超出最大历史步数时，丢弃最旧的命令
        if self.undo_stack.len() > self.max_history {
            self.undo_stack.remove(0);
        }
    }
}
