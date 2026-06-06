//! 控制台日志捕获
//!
//! 通过 tracing 事件回调写入环形缓冲区。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use tracing::Level;

/// 日志条目
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Local>,
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// 线程安全的日志缓冲区
pub type LogBuffer = Arc<Mutex<VecDeque<LogEntry>>>;

/// 日志捕获 —— 将 tracing 事件写入共享缓冲区
pub struct ConsoleLogLayer {
    buffer: LogBuffer,
    max_lines: usize,
}

impl ConsoleLogLayer {
    pub fn new(max_lines: usize) -> (Self, LogBuffer) {
        let buffer = Arc::new(Mutex::new(VecDeque::with_capacity(max_lines)));
        (
            Self {
                buffer: buffer.clone(),
                max_lines,
            },
            buffer,
        )
    }

    /// 处理一个 tracing 事件
    pub fn on_event(
        &self,
        metadata: &tracing::Metadata<'_>,
        message: &str,
    ) {
        let entry = LogEntry {
            timestamp: Local::now(),
            level: *metadata.level(),
            target: metadata.target().to_string(),
            message: message.to_string(),
        };

        let mut buf = self.buffer.lock().unwrap();
        if buf.len() >= self.max_lines {
            buf.pop_front();
        }
        buf.push_back(entry);
    }
}
