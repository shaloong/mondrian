//! GPU render profiling — optional timestamp-based GPU pass timing.
//!
//! Controlled by `MONDRIAN_RENDER_PROFILE=1`. When enabled, each frame's
//! GPU passes are measured using wgpu timestamp queries and reported
//! to the console every N frames.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Initialize GPU profiling. Call once at startup.
pub fn init_profiling() {
    let enabled = std::env::var("MONDRIAN_RENDER_PROFILE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        tracing::info!("GPU render profiling enabled (MONDRIAN_RENDER_PROFILE=1)");
    }
}

/// Check whether GPU profiling is active.
pub fn profiling_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A single GPU profiling span — measures elapsed GPU time for one pass.
///
/// Usage:
/// ```ignore
/// let span = GpuProfileSpan::begin(&device, &mut encoder, "composite");
/// // ... record GPU work ...
/// span.end(&mut encoder);
/// // Later: span.elapsed_ns(&device)
/// ```
pub struct GpuProfileSpan {
    #[allow(dead_code)]
    label: String,
    query_set: Option<wgpu::QuerySet>,
    #[allow(dead_code)]
    start_index: u32,
    end_index: u32,
    #[allow(dead_code)]
    resolution: Option<f64>,
}

impl GpuProfileSpan {
    /// Begin a new GPU profiling span. Returns a no-op span if profiling is disabled
    /// or timestamp queries are not supported.
    pub fn begin(device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, label: &str) -> Self {
        if !profiling_enabled() {
            let _ = (device, encoder, label);
            return Self::disabled();
        }
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            tracing::warn!("GPU timestamp queries not supported — profiling disabled");
            return Self::disabled();
        }

        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some(&format!("profile_{label}")),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        // Resolution for converting timestamp ticks to nanoseconds.
        // Use a reasonable default; actual readback requires async buffer mapping.
        let resolution = Some(1.0); // Deferred: use queue.get_timestamp_period()

        encoder.write_timestamp(&query_set, 0);
        Self {
            label: label.to_string(),
            query_set: Some(query_set),
            start_index: 0,
            end_index: 1,
            resolution,
        }
    }

    fn disabled() -> Self {
        Self {
            label: String::new(),
            query_set: None,
            start_index: 0,
            end_index: 0,
            resolution: None,
        }
    }

    /// End the profiling span — records the end timestamp.
    pub fn end(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(ref qs) = self.query_set {
            encoder.write_timestamp(qs, self.end_index);
        }
    }

    /// Resolve and return the elapsed time in nanoseconds.
    /// Full async readback is deferred — returns None for now.
    /// When implemented, this reads back the query set via buffer mapping.
    pub fn elapsed_ns(&self) -> Option<f64> {
        // Deferred: full async readback with buffer mapping.
        // Requires resolve_query_set + map_async + poll.
        None
    }
}

/// Aggregate profiling statistics for a frame.
#[derive(Debug, Default, Clone)]
pub struct FrameProfile {
    /// Label → elapsed nanoseconds for each pass.
    pub passes: Vec<(String, f64)>,
    /// Total frame GPU time in nanoseconds.
    pub total_ns: f64,
}

/// Global frame profiler — accumulates per-frame stats.
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
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed frame's profile.
    pub fn record(&mut self, profile: FrameProfile) {
        self.frame_count += 1;
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
        let avg_total = self.history.iter().map(|p| p.total_ns).sum::<f64>() / count;
        eprintln!(
            "[gpu-profile] frame {} — avg {:.1}ms ({} samples)",
            self.frame_count,
            avg_total / 1_000_000.0,
            self.history.len()
        );
        // Show per-pass breakdown
        if let Some(last) = self.history.last() {
            for (label, ns) in &last.passes {
                eprintln!("  {label}: {:.2}ms", ns / 1_000_000.0);
            }
        }
    }
}

/// Global profiler instance.
pub fn global_profiler() -> &'static std::sync::Mutex<FrameProfiler> {
    static PROFILER: std::sync::OnceLock<std::sync::Mutex<FrameProfiler>> =
        std::sync::OnceLock::new();
    PROFILER.get_or_init(|| std::sync::Mutex::new(FrameProfiler::new()))
}
