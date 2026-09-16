//! Non-intrusive process-memory sampling for realtime acceptance runs.
//!
//! Native process-tree discovery may enumerate processes and query multiple
//! handles. It must never execute on the thread that advances transport,
//! submits Viewer work, or pumps an audio Adapter.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use mondrian_platform::{ProcessMemoryProbe, ProcessMemoryProbeResult, SystemPlatformService};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct TimedProcessMemorySample {
    pub(crate) observed_at_us: u64,
    pub(crate) sample: ProcessMemoryProbeResult,
}

pub(crate) struct ProfessionalProcessMemorySampler {
    stop_sender: mpsc::Sender<()>,
    sample_receiver: mpsc::Receiver<TimedProcessMemorySample>,
    worker: Option<JoinHandle<()>>,
}

impl ProfessionalProcessMemorySampler {
    pub(crate) fn start(observation_origin: Instant) -> anyhow::Result<Self> {
        Self::start_with_probe(observation_origin, SAMPLE_INTERVAL, SystemPlatformService)
    }

    fn start_with_probe<P>(
        observation_origin: Instant,
        interval: Duration,
        probe: P,
    ) -> anyhow::Result<Self>
    where
        P: ProcessMemoryProbe + 'static,
    {
        anyhow::ensure!(
            !interval.is_zero(),
            "process-memory sample interval must be positive"
        );
        let (stop_sender, stop_receiver) = mpsc::channel();
        let (sample_sender, sample_receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("playback-memory-evidence".to_owned())
            .spawn(move || {
                let mut next_sample_at = Instant::now();
                loop {
                    if stop_receiver.try_recv().is_ok() {
                        break;
                    }
                    let sample_started = Instant::now();
                    let observed_at_us = sample_started
                        .saturating_duration_since(observation_origin)
                        .as_micros()
                        .min(u128::from(u64::MAX)) as u64;
                    if sample_sender
                        .send(TimedProcessMemorySample {
                            observed_at_us,
                            sample: probe.product_process_tree_memory(),
                        })
                        .is_err()
                    {
                        break;
                    }
                    next_sample_at =
                        next_sample_at.checked_add(interval).unwrap_or_else(Instant::now);
                    let wait = next_sample_at.saturating_duration_since(Instant::now());
                    match stop_receiver.recv_timeout(wait) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .context("start playback process-memory evidence worker")?;
        Ok(Self { stop_sender, sample_receiver, worker: Some(worker) })
    }

    pub(super) fn finish(mut self) -> anyhow::Result<Vec<TimedProcessMemorySample>> {
        self.stop_worker()?;
        Ok(self.sample_receiver.try_iter().collect())
    }

    /// Drain samples already completed by the native probe worker without
    /// blocking the realtime coordinator.
    pub(crate) fn drain_ready(&self) -> Vec<TimedProcessMemorySample> {
        self.sample_receiver.try_iter().collect()
    }

    fn stop_worker(&mut self) -> anyhow::Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let _ = self.stop_sender.send(());
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("playback process-memory evidence worker panicked"))
    }
}

impl Drop for ProfessionalProcessMemorySampler {
    fn drop(&mut self) {
        let _ = self.stop_worker();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use mondrian_platform::ProcessMemoryScope;

    use super::*;

    struct SlowProbe {
        observations: Arc<AtomicUsize>,
    }

    impl ProcessMemoryProbe for SlowProbe {
        fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
            std::thread::sleep(Duration::from_millis(20));
            self.observations.fetch_add(1, Ordering::Relaxed);
            ProcessMemoryProbeResult::unsupported(scope, "test probe")
        }
    }

    #[test]
    fn slow_native_probe_runs_outside_the_realtime_caller() {
        let observations = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();
        let sampler = ProfessionalProcessMemorySampler::start_with_probe(
            started,
            Duration::from_millis(5),
            SlowProbe { observations: Arc::clone(&observations) },
        )
        .expect("start sampler");

        assert!(started.elapsed() < Duration::from_millis(20));
        while observations.load(Ordering::Relaxed) == 0 {
            std::thread::yield_now();
        }
        let samples = sampler.finish().expect("finish sampler");

        assert!(!samples.is_empty());
        assert_eq!(samples.len(), observations.load(Ordering::Relaxed));
    }
}
