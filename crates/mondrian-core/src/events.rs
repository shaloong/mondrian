//! 全局事件总线（发布/订阅模式）
//!
//! 基于 `crossbeam-channel` 实现的同步广播总线。
//! 所有跨模块通信通过此总线传递，保证模块解耦。

use crate::types::*;
use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::RwLock;
use std::sync::Arc;

// ─── 应用事件枚举 ─────────────────────────────────────────────────────────────

/// 全应用事件类型
///
/// 新增事件时，在此枚举中添加变体。
#[derive(Debug, Clone)]
pub enum AppEvent {
    // ── 播放控制 ──────────────────────────────────────────────────────────────
    Play,
    Pause,
    Stop,
    SeekTo {
        timecode: FramePosition,
    },
    PlayheadMoved {
        timecode: FramePosition,
    },

    // ── 时间线编辑 ────────────────────────────────────────────────────────────
    TimelineModified {
        sequence_id: SequenceId,
    },
    ClipAdded {
        sequence_id: SequenceId,
        clip_id: ClipId,
    },
    ClipRemoved {
        sequence_id: SequenceId,
        clip_id: ClipId,
    },
    ClipMoved {
        clip_id: ClipId,
        new_position: FramePosition,
    },
    ClipTrimmed {
        clip_id: ClipId,
    },
    TrackAdded {
        sequence_id: SequenceId,
        track_id: TrackId,
    },
    TrackRemoved {
        sequence_id: SequenceId,
        track_id: TrackId,
    },
    KeyframeChanged {
        clip_id: ClipId,
        property: String,
    },

    // ── 素材库 ────────────────────────────────────────────────────────────────
    AssetImported {
        asset_id: AssetId,
    },
    AssetDeleted {
        asset_id: AssetId,
    },
    AssetLibraryReloaded,

    // ── AI 工作流 ─────────────────────────────────────────────────────────────
    WorkflowStarted {
        workflow_name: String,
    },
    WorkflowStepStarted {
        step_id: String,
        step_name: String,
    },
    WorkflowStepCompleted {
        step_id: String,
    },
    WorkflowStepFailed {
        step_id: String,
        error: String,
    },
    WorkflowCompleted {
        workflow_name: String,
    },
    AiGenerationProgress {
        step_id: String,
        progress: f32,
        message: String,
    },

    // ── 渲染 / 导出 ───────────────────────────────────────────────────────────
    RenderJobStarted {
        job_id: JobId,
    },
    RenderJobProgress {
        job_id: JobId,
        progress: f32,
    },
    RenderJobCompleted {
        job_id: JobId,
        output_path: String,
    },
    RenderJobFailed {
        job_id: JobId,
        error: String,
    },

    // ── 项目管理 ──────────────────────────────────────────────────────────────
    ProjectOpened {
        project_id: ProjectId,
    },
    ProjectSaved {
        project_id: ProjectId,
    },
    ProjectClosed,

    // ── UI ────────────────────────────────────────────────────────────────────
    PanelResized {
        panel: String,
        size: f32,
    },
    ThemeChanged {
        theme: String,
    },
    UndoPerformed,
    RedoPerformed,

    // ── GPU 状态 ────────────────────────────────────────────────────────────────
    /// GPU 可用性变化通知。`available=false` 时 `reason` 给出回退到 CPU 的原因。
    GpuStatusChanged {
        available: bool,
        reason: String,
    },
}

// ─── 事件总线 ─────────────────────────────────────────────────────────────────

type Subscriber = Sender<AppEvent>;

/// 轻量广播事件总线
///
/// 通过 `subscribe()` 获取接收端，通过 `publish()` 向所有订阅者广播事件。
#[derive(Default)]
pub struct EventBus {
    subscribers: RwLock<Vec<Subscriber>>,
}

impl EventBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 订阅事件，返回接收端 `Receiver`
    pub fn subscribe(&self) -> Receiver<AppEvent> {
        let (tx, rx) = unbounded();
        self.subscribers.write().push(tx);
        rx
    }

    /// 向所有活跃订阅者广播事件（失败的订阅者自动清理）
    pub fn publish(&self, event: AppEvent) {
        let mut subs = self.subscribers.write();
        subs.retain(|s| s.send(event.clone()).is_ok());
    }

    /// 当前活跃订阅者数量
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_bus_broadcast() {
        let bus = EventBus::new();
        let rx1 = bus.subscribe();
        let rx2 = bus.subscribe();

        bus.publish(AppEvent::Play);

        assert!(matches!(rx1.try_recv().unwrap(), AppEvent::Play));
        assert!(matches!(rx2.try_recv().unwrap(), AppEvent::Play));
    }

    #[test]
    fn event_bus_cleanup_disconnected() {
        let bus = EventBus::new();
        {
            let _rx = bus.subscribe(); // 离开作用域后 rx 被 drop
        }
        // 广播时自动清理已断开的订阅者
        bus.publish(AppEvent::Stop);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
