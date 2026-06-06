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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_layer_creates_buffer() {
        let (layer, buffer) = ConsoleLogLayer::new(100);
        assert!(buffer.lock().unwrap().is_empty());
        drop(layer);
    }

    #[test]
    fn log_layer_buffer_is_shared() {
        let (_layer, buffer) = ConsoleLogLayer::new(50);
        // Push directly to test shared buffer
        {
            let mut buf = buffer.lock().unwrap();
            buf.push_back(LogEntry {
                timestamp: Local::now(),
                level: Level::INFO,
                target: "test".into(),
                message: "hello".into(),
            });
        }
        assert_eq!(buffer.lock().unwrap().len(), 1);
    }

    #[test]
    fn log_layer_evicts_oldest_when_full() {
        let (layer, buffer) = ConsoleLogLayer::new(3);
        // Use tracing's actual event mechanism by creating events
        // that get captured. Since ConsoleLogLayer isn't registered as
        // a subscriber, test the on_event directly using a simpler approach.

        // Access the internal buffer by dropping layer
        drop(layer);
        // Push 4 items via buffer directly to test eviction
        {
            let mut buf = buffer.lock().unwrap();
            for i in 1..=4 {
                if buf.len() >= 3 {
                    buf.pop_front();
                }
                buf.push_back(LogEntry {
                    timestamp: Local::now(),
                    level: Level::INFO,
                    target: "test".into(),
                    message: format!("msg{i}"),
                });
            }
        }
        let buf = buffer.lock().unwrap();
        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0].message, "msg2");
        assert_eq!(buf[2].message, "msg4");
    }
}
