//! GPU execution profiling with hardware timestamp queries.
//!
//! Timestamp queries measure work on the GPU timeline. They intentionally stay
//! separate from CPU command recording, queue submission, and completion-wait
//! latency so callers can attribute stalls instead of treating wall time as GPU
//! render time.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
const TIMESTAMP_READBACK_BYTES: u64 = 2 * wgpu::QUERY_SIZE as u64;

/// Initialize opt-in production GPU profiling from `MONDRIAN_RENDER_PROFILE`.
pub fn init_profiling() {
    let enabled = std::env::var("MONDRIAN_RENDER_PROFILE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        tracing::info!("GPU render profiling enabled (MONDRIAN_RENDER_PROFILE=1)");
    }
}

/// Whether opt-in production GPU profiling is active.
pub fn profiling_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Device features required to write timestamps around a command encoder.
///
/// The pair is all-or-nothing: `TIMESTAMP_QUERY` without encoder timestamp
/// writes cannot measure a complete Viewer frame at this boundary.
pub fn gpu_timestamp_query_device_features(supported: wgpu::Features) -> wgpu::Features {
    let required =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if supported.contains(required) {
        required
    } else {
        wgpu::Features::empty()
    }
}

/// Reusable two-point GPU timer for a caller that waits for one submission.
///
/// This timer owns separate resolve and CPU-readback buffers. `begin` and
/// `finish` only record commands; `read_elapsed_us_after_submission` performs
/// the explicit synchronization requested by an execution gate. Realtime
/// presentation should use a bounded asynchronous query ring instead of this
/// synchronous read boundary.
pub struct GpuTimestampFrameTimer {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    timestamp_period_ns: f64,
}

impl GpuTimestampFrameTimer {
    /// Create a timer when the device enabled both required timestamp features.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let required = gpu_timestamp_query_device_features(device.features());
        if required.is_empty() {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("mondrian_gpu_frame_timestamp_queries"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_gpu_frame_timestamp_resolve"),
            size: TIMESTAMP_READBACK_BYTES,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_gpu_frame_timestamp_readback"),
            size: TIMESTAMP_READBACK_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            timestamp_period_ns: f64::from(queue.get_timestamp_period()),
        })
    }

    /// Record the timestamp immediately before the measured frame commands.
    pub fn begin(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, 0);
    }

    /// Record the end timestamp and copy both counters into the readback buffer.
    pub fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, 1);
        encoder.resolve_query_set(&self.query_set, 0..2, &self.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            TIMESTAMP_READBACK_BYTES,
        );
    }

    /// Wait for `submission`, map its counters, and return hardware GPU time.
    pub fn read_elapsed_us_after_submission(
        &self,
        device: &wgpu::Device,
        submission: wgpu::SubmissionIndex,
    ) -> Result<u64, GpuTimestampReadError> {
        let slice = self.readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .map_err(|error| GpuTimestampReadError::DevicePoll(error.to_string()))?;
        receiver
            .recv()
            .map_err(|_| GpuTimestampReadError::MapCallbackDropped)?
            .map_err(|error| GpuTimestampReadError::BufferMap(error.to_string()))?;

        let mapped = match slice.get_mapped_range() {
            Ok(mapped) => mapped,
            Err(error) => {
                self.readback_buffer.unmap();
                return Err(GpuTimestampReadError::MappedRange(error.to_string()));
            }
        };
        if mapped.len() < TIMESTAMP_READBACK_BYTES as usize {
            drop(mapped);
            self.readback_buffer.unmap();
            return Err(GpuTimestampReadError::ShortReadback);
        }
        let mut start_bytes = [0_u8; 8];
        let mut end_bytes = [0_u8; 8];
        start_bytes.copy_from_slice(&mapped[0..8]);
        end_bytes.copy_from_slice(&mapped[8..16]);
        drop(mapped);
        self.readback_buffer.unmap();

        let start = u64::from_ne_bytes(start_bytes);
        let end = u64::from_ne_bytes(end_bytes);
        let ticks = end
            .checked_sub(start)
            .ok_or(GpuTimestampReadError::CounterRegression { start, end })?;
        let elapsed_us = ((ticks as f64 * self.timestamp_period_ns) / 1_000.0).ceil();
        Ok(elapsed_us.min(u64::MAX as f64) as u64)
    }
}

/// Failure reading a completed hardware timestamp query.
#[derive(Debug, thiserror::Error)]
pub enum GpuTimestampReadError {
    /// Waiting for the submitted command buffer failed.
    #[error("GPU timestamp submission wait failed: {0}")]
    DevicePoll(String),
    /// The map callback channel closed before reporting a result.
    #[error("GPU timestamp map callback was dropped")]
    MapCallbackDropped,
    /// Mapping the readback buffer failed.
    #[error("GPU timestamp readback mapping failed: {0}")]
    BufferMap(String),
    /// Reading the successfully mapped range failed.
    #[error("GPU timestamp mapped range could not be read: {0}")]
    MappedRange(String),
    /// The mapped buffer did not contain both 64-bit timestamp counters.
    #[error("GPU timestamp readback did not contain two counters")]
    ShortReadback,
    /// A frame-local end counter preceded its start counter.
    #[error("GPU timestamp counter regressed from {start} to {end}")]
    CounterRegression { start: u64, end: u64 },
}

/// Aggregate timing for one completed frame.
#[derive(Debug, Default, Clone)]
pub struct FrameProfile {
    /// Label and elapsed nanoseconds for each measured pass.
    pub passes: Vec<(String, f64)>,
    /// Total measured GPU frame time in nanoseconds.
    pub total_ns: f64,
}

/// Bounded history for optional human-readable GPU profiling.
pub struct FrameProfiler {
    frame_count: u64,
    report_interval: u64,
    history: Vec<FrameProfile>,
    max_history: usize,
}

impl Default for FrameProfiler {
    fn default() -> Self {
        Self {
            frame_count: 0,
            report_interval: 60,
            history: Vec::new(),
            max_history: 120,
        }
    }
}

impl FrameProfiler {
    /// Create a profiler with the default bounded history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed frame and periodically emit an aggregate trace.
    pub fn record(&mut self, profile: FrameProfile) {
        self.frame_count = self.frame_count.saturating_add(1);
        self.history.push(profile);
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }
        if self.frame_count.is_multiple_of(self.report_interval) {
            self.report();
        }
    }

    fn report(&self) {
        if self.history.is_empty() {
            return;
        }
        let count = self.history.len() as f64;
        let avg_total = self.history.iter().map(|profile| profile.total_ns).sum::<f64>() / count;
        tracing::info!(
            frame = self.frame_count,
            samples = self.history.len(),
            average_gpu_ms = avg_total / 1_000_000.0,
            "GPU frame profile"
        );
    }
}

/// Process-wide optional frame-profiler history.
pub fn global_profiler() -> &'static std::sync::Mutex<FrameProfiler> {
    static PROFILER: std::sync::OnceLock<std::sync::Mutex<FrameProfiler>> =
        std::sync::OnceLock::new();
    PROFILER.get_or_init(|| std::sync::Mutex::new(FrameProfiler::new()))
}

#[cfg(test)]
mod tests {
    use super::gpu_timestamp_query_device_features;

    #[test]
    fn timestamp_features_are_enabled_only_as_a_complete_pair() {
        assert!(gpu_timestamp_query_device_features(wgpu::Features::empty()).is_empty());
        assert!(gpu_timestamp_query_device_features(wgpu::Features::TIMESTAMP_QUERY).is_empty());
        let required =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        assert_eq!(gpu_timestamp_query_device_features(required), required);
    }
}
