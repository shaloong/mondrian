//! Bounded diagnostic I/O outside the caller's realtime execution path.

use std::io::Write;

use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};

const STDERR_BUFFERED_LINES: usize = 1_024;

/// Queue console diagnostics without waiting for the terminal or pipe reader.
///
/// Keep the guard until all emitting owners have stopped. A full queue drops
/// diagnostics instead of blocking playback; qualification callers must reject
/// evidence when the writer's error counter reports dropped lines.
pub fn buffered_stderr() -> (NonBlocking, WorkerGuard) {
    buffered_writer(std::io::stderr())
}

fn buffered_writer(writer: impl Write + Send + 'static) -> (NonBlocking, WorkerGuard) {
    NonBlockingBuilder::default()
        .buffered_lines_limit(STDERR_BUFFERED_LINES)
        .lossy(true)
        .thread_name("mondrian-stderr")
        .finish(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    struct BlockedWriter {
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).expect("announce blocked sink");
                self.release.recv().expect("release sink");
            }
            self.bytes.lock().expect("captured bytes").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_console_does_not_block_producer_and_flushes_ordered_evidence() {
        exercise_blocked_console(1, 0);
    }

    #[test]
    fn saturated_console_reports_loss_without_blocking_producer() {
        exercise_blocked_console(STDERR_BUFFERED_LINES + 1, 1);
    }

    fn exercise_blocked_console(queued_lines: usize, expected_dropped: usize) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (mut writer, guard) = buffered_writer(BlockedWriter {
            entered: Some(entered_tx),
            release: release_rx,
            bytes: Arc::clone(&bytes),
        });
        let errors = writer.error_counter();
        writer.write_all(b"first\n").expect("first line");
        entered_rx.recv_timeout(Duration::from_secs(1)).expect("sink started");
        let (done_tx, done_rx) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            for _ in 0..queued_lines {
                writer.write_all(b"next\n").expect("queue diagnostic");
            }
            done_tx.send(()).expect("producer returned");
        });
        // Release before asserting so a regression cannot strand the worker.
        let returned_while_sink_blocked = done_rx.recv_timeout(Duration::from_secs(1));
        let dropped_while_sink_blocked = errors.dropped_lines();
        release_tx.send(()).expect("release sink");
        producer.join().expect("producer joined");
        drop(guard);
        assert!(returned_while_sink_blocked.is_ok());
        assert_eq!(dropped_while_sink_blocked, expected_dropped);
        let mut expected = b"first\n".to_vec();
        for _ in 0..queued_lines - expected_dropped {
            expected.extend_from_slice(b"next\n");
        }
        assert_eq!(*bytes.lock().expect("captured bytes"), expected);
    }
}
