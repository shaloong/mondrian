//! 控制台日志捕获
//!
//! 通过 tracing_subscriber::Layer 将 tracing 事件写入环形缓冲区。

use std::collections::VecDeque;
use std::fmt::Write as FmtWrite;
use std::sync::{Arc, Mutex};

use chrono::Local;
use tracing::Level;
use tracing_subscriber::Layer;

/// 日志条目
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// 线程安全的日志缓冲区
pub type LogBuffer = Arc<Mutex<VecDeque<LogEntry>>>;

/// 日志捕获 Layer —— 将 tracing 事件写入共享缓冲区
///
/// ## 用法
///
/// ```ignore
/// let (layer, buffer) = ConsoleLogLayer::new(500);
/// tracing_subscriber::registry().with(layer).init();
/// let panel = ConsolePanel::new(buffer, 500);
/// ```
pub struct ConsoleLogLayer {
    buffer: LogBuffer,
    max_lines: usize,
}

impl ConsoleLogLayer {
    pub fn new(max_lines: usize) -> (Self, LogBuffer) {
        let buffer = Arc::new(Mutex::new(VecDeque::with_capacity(max_lines)));
        (Self { buffer: buffer.clone(), max_lines }, buffer)
    }
}

impl<S> Layer<S> for ConsoleLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let mut message = String::new();
        {
            let mut visitor = StringVisitor(&mut message);
            event.record(&mut visitor);
        }

        let entry = LogEntry {
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            level: *metadata.level(),
            target: metadata.target().to_string(),
            message,
        };

        let mut buf = self.buffer.lock().unwrap();
        if buf.len() >= self.max_lines {
            buf.pop_front();
        }
        buf.push_back(entry);
    }
}

/// Helper to extract formatted message from a tracing Event
struct StringVisitor<'a>(&'a mut String);

impl tracing::field::Visit for StringVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_layer_creates_buffer() {
        let (_layer, buffer) = ConsoleLogLayer::new(100);
        assert!(buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn log_layer_buffer_is_shared() {
        let (_layer, buffer) = ConsoleLogLayer::new(50);
        {
            let mut buf = buffer.lock().unwrap();
            buf.push_back(LogEntry {
                timestamp: "12:00:00".into(),
                level: Level::INFO,
                target: "test".into(),
                message: "hello".into(),
            });
        }
        assert_eq!(buffer.lock().unwrap().len(), 1);
    }

    #[test]
    fn log_layer_evicts_oldest_when_full() {
        let (_layer, buffer) = ConsoleLogLayer::new(3);
        {
            let mut buf = buffer.lock().unwrap();
            for i in 1..=4 {
                if buf.len() >= 3 {
                    buf.pop_front();
                }
                buf.push_back(LogEntry {
                    timestamp: "12:00:00".into(),
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
