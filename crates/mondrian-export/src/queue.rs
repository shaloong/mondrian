//! 后台渲染队列

use crate::preset::ExportConfig;
use chrono::{DateTime, Utc};
use mondrian_core::types::JobId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    Pending,
    Rendering { frame: u64, total_frames: u64 },
    Encoding,
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct RenderJob {
    pub id: JobId,
    pub config: ExportConfig,
    pub status: JobStatus,
    pub progress: f32,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl RenderJob {
    pub fn new(config: ExportConfig) -> Self {
        Self {
            id: JobId::new(),
            config,
            status: JobStatus::Pending,
            progress: 0.0,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }
}

/// 异步后台渲染队列
pub struct RenderQueue {
    jobs: Arc<Mutex<VecDeque<RenderJob>>>,
}

impl RenderQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { jobs: Arc::new(Mutex::new(VecDeque::new())) })
    }

    pub fn enqueue(&self, job: RenderJob) -> JobId {
        let id = job.id;
        self.jobs.lock().push_back(job);
        id
    }

    pub fn list_jobs(&self) -> Vec<RenderJob> {
        self.jobs.lock().iter().cloned().collect()
    }

    pub fn cancel(&self, id: JobId) {
        let mut q = self.jobs.lock();
        if let Some(job) = q.iter_mut().find(|j| j.id == id) {
            job.status = JobStatus::Cancelled;
        }
    }

    pub fn clear_completed(&self) {
        self.jobs
            .lock()
            .retain(|j| !matches!(j.status, JobStatus::Completed | JobStatus::Cancelled));
    }
}

impl Default for RenderQueue {
    fn default() -> Self {
        Self { jobs: Arc::new(Mutex::new(VecDeque::new())) }
    }
}
