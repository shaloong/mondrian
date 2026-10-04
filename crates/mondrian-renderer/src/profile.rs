//! GPU execution profiling with hardware timestamp queries.
//!
//! Timestamp queries measure work on the GPU timeline. They intentionally stay
//! separate from CPU command recording, queue submission, and completion-wait
//! latency so callers can attribute stalls instead of treating wall time as GPU
//! render time.

use crate::gpu_command::{
    GpuLeadingCopyTimestampAnchor, GpuPassTimestampIntervalReceipt, GpuPassTimestampLayout,
    GpuPassTimestampReceipt, GpuPassTimestampRecorder,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

static ENABLED: AtomicBool = AtomicBool::new(false);
const TIMESTAMP_READBACK_BYTES: u64 = 2 * wgpu::QUERY_SIZE as u64;
const VIEWER_STAGE_TIMESTAMP_COUNT: u32 = 8;

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

/// Frame timing uses encoder boundaries when supported, otherwise actual passes.
/// This does not establish complete coverage; the consuming recording receipt does.
pub fn gpu_frame_timestamp_query_device_features(supported: wgpu::Features) -> wgpu::Features {
    (supported & wgpu::Features::TIMESTAMP_QUERY) | gpu_timestamp_query_device_features(supported)
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
    queue: wgpu::Queue,
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
            queue: queue.clone(),
        })
    }

    /// Record the timestamp immediately before the measured frame commands.
    pub fn begin(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, 0);
    }

    /// Record the end timestamp; resolve follows measured submission completion.
    pub fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, 1);
    }

    fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.query_set, 0..2, &self.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            TIMESTAMP_READBACK_BYTES,
        );
    }

    /// Begin an asynchronous map request for the resolved counters.
    fn map_async(&self) -> Receiver<Result<(), wgpu::BufferAsyncError>> {
        let slice = self.readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        receiver
    }

    /// Read and unmap counters after a successful map callback.
    fn read_mapped_elapsed_us(&self) -> Result<u64, GpuTimestampReadError> {
        let slice = self.readback_buffer.slice(..);
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

    /// Wait for `submission`, map its counters, and return hardware GPU time.
    pub fn read_elapsed_us_after_submission(
        &self,
        device: &wgpu::Device,
        submission: wgpu::SubmissionIndex,
    ) -> Result<u64, GpuTimestampReadError> {
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .map_err(|error| GpuTimestampReadError::DevicePoll(error.to_string()))?;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian_gpu_frame_timestamp_resolve_after_completion"),
        });
        self.resolve(&mut encoder);
        let resolve_submission = self.queue.submit([encoder.finish()]);
        let receiver = self.map_async();
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(resolve_submission),
                timeout: None,
            })
            .map_err(|error| GpuTimestampReadError::DevicePoll(error.to_string()))?;
        receiver
            .recv()
            .map_err(|_| GpuTimestampReadError::MapCallbackDropped)?
            .map_err(|error| GpuTimestampReadError::BufferMap(error.to_string()))?;

        self.read_mapped_elapsed_us()
    }
}

/// Opaque identity for one timestamp sample in a bounded query ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuTimestampToken {
    id: u64,
    slot: usize,
}

impl GpuTimestampToken {
    /// Stable run-local identity used to associate deferred samples.
    pub const fn id(self) -> u64 {
        self.id
    }
}

/// Completed asynchronous GPU timestamp sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuTimestampSample {
    /// Token returned when recording began.
    pub token: GpuTimestampToken,
    /// Hardware GPU duration across the measured encoder or complete pass scope.
    pub elapsed_us: u64,
    /// Hardware intervals for the commands grouped by Viewer stage markers.
    /// Pass intervals may overlap and do not partition `elapsed_us`.
    pub stages: GpuTimestampStageDurations,
}

/// Ordered intermediate marker written inside one Viewer command encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuTimestampStageMarker {
    /// Working composite commands have been recorded.
    AfterWorkingComposite,
    /// Viewer spatial commands have been recorded.
    AfterSpatial,
    /// Program Output boundary commands have been recorded.
    AfterProgramOutputBoundary,
    /// Preview-only monitor-adaptation commands have been recorded.
    AfterMonitorAdaptation,
    /// Optional Program/Monitor scopes commands have been recorded.
    AfterProgramScopes,
    /// Optional fused false-color/zebra/gamut commands have been recorded.
    AfterSignalMonitoring,
}

impl GpuTimestampStageMarker {
    pub(crate) const fn query_index(self) -> u32 {
        match self {
            Self::AfterWorkingComposite => 1,
            Self::AfterSpatial => 2,
            Self::AfterProgramOutputBoundary => 3,
            Self::AfterMonitorAdaptation => 4,
            Self::AfterProgramScopes => 5,
            Self::AfterSignalMonitoring => 6,
        }
    }
}

/// Hardware GPU interval attributed to each ordered Viewer stage.
///
/// With pass timestamps, each nonempty stage brackets its actual passes. GPU
/// stages may overlap, and gaps between stages are included only in the complete
/// frame interval. These fields must not be summed to reconstruct frame time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuTimestampStageDurations {
    /// Frame start through the working composite marker.
    pub through_working_composite_us: u64,
    /// Viewer spatial processing after the working composite.
    pub spatial_us: u64,
    /// Program Output color boundary after spatial processing.
    pub program_output_boundary_us: u64,
    /// Demand-driven Program/Monitor scopes after monitor adaptation.
    pub program_scopes_us: u64,
    /// Preview-only monitor adaptation after Program Output.
    pub monitor_adaptation_us: u64,
    /// Fused signal-monitoring pass after analysis and before calibration.
    pub signal_monitoring_us: u64,
    /// Optional display calibration after monitor adaptation.
    pub display_calibration_us: u64,
}

fn encoder_timestamp_layout() -> GpuPassTimestampLayout {
    GpuPassTimestampLayout {
        query_count: VIEWER_STAGE_TIMESTAMP_COUNT,
        boundaries: [0, 1, 2, 3, 4, 5, 6, 7],
    }
}

struct GpuTimestampStageTimer {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    timestamp_period_ns: f64,
}

impl GpuTimestampStageTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        Self::with_capacity(device, queue, VIEWER_STAGE_TIMESTAMP_COUNT)
    }

    fn with_capacity(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        query_capacity: u32,
    ) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("mondrian_viewer_stage_timestamp_queries"),
            ty: wgpu::QueryType::Timestamp,
            count: query_capacity,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_viewer_stage_timestamp_resolve"),
            size: u64::from(query_capacity) * u64::from(wgpu::QUERY_SIZE),
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_viewer_stage_timestamp_readback"),
            size: u64::from(query_capacity) * u64::from(wgpu::QUERY_SIZE),
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

    fn begin(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, 0);
    }

    fn mark(
        &self,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        marker: GpuTimestampStageMarker,
    ) -> Result<(), String> {
        encoder.write_timestamp(&self.query_set, marker.query_index())
    }

    fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, VIEWER_STAGE_TIMESTAMP_COUNT - 1);
    }

    fn resolve(&self, encoder: &mut wgpu::CommandEncoder, layout: GpuPassTimestampLayout) {
        encoder.resolve_query_set(
            &self.query_set,
            0..layout.query_count,
            &self.resolve_buffer,
            0,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            u64::from(layout.query_count) * u64::from(wgpu::QUERY_SIZE),
        );
    }

    fn map_async(&self) -> Receiver<Result<(), wgpu::BufferAsyncError>> {
        let slice = self.readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        receiver
    }

    fn read_mapped_query_bytes<R>(
        &self,
        query_count: u32,
        read: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, GpuTimestampReadError> {
        let slice = self.readback_buffer.slice(..);
        let mapped = match slice.get_mapped_range() {
            Ok(mapped) => mapped,
            Err(error) => {
                self.readback_buffer.unmap();
                return Err(GpuTimestampReadError::MappedRange(error.to_string()));
            }
        };
        let byte_count = query_count as usize * wgpu::QUERY_SIZE as usize;
        if mapped.len() < byte_count {
            drop(mapped);
            self.readback_buffer.unmap();
            return Err(GpuTimestampReadError::ShortReadback);
        }
        let values = read(&mapped[..byte_count]);
        drop(mapped);
        self.readback_buffer.unmap();
        Ok(values)
    }

    fn read_mapped_elapsed_us(
        &self,
        layout: GpuPassTimestampLayout,
        pass_timestamps: bool,
    ) -> Result<(u64, GpuTimestampStageDurations), GpuTimestampReadError> {
        self.read_mapped_query_bytes(layout.query_count, |bytes| {
            timestamp_stage_durations(bytes, layout, pass_timestamps, self.timestamp_period_ns)
        })?
    }
}

fn timestamp_stage_durations(
    bytes: &[u8],
    layout: GpuPassTimestampLayout,
    pass_timestamps: bool,
    timestamp_period_ns: f64,
) -> Result<(u64, GpuTimestampStageDurations), GpuTimestampReadError> {
    let counter = |index: u32| {
        let offset = index as usize * wgpu::QUERY_SIZE as usize;
        u64::from_ne_bytes(bytes[offset..offset + 8].try_into().expect("eight-byte query counter"))
    };
    let interval = |start: u32, end: u32| {
        if !pass_timestamps {
            return timestamp_elapsed_us(counter(start), counter(end), timestamp_period_ns);
        }
        if start == end {
            return Ok(0);
        }
        pass_timestamp_elapsed_us(
            (start..end).step_by(2).map(|index| [counter(index), counter(index + 1)]),
            timestamp_period_ns,
        )
    };
    let boundaries = if pass_timestamps {
        // Zero denotes no recorded passes; all populated boundaries name an
        // odd end-query index. Convert them to exclusive pass-pair ranges.
        layout.boundaries.map(|index| if index == 0 { 0 } else { index + 1 })
    } else {
        layout.boundaries
    };
    let segment = |start: usize| interval(boundaries[start], boundaries[start + 1]);
    let stages = GpuTimestampStageDurations {
        through_working_composite_us: segment(0)?,
        spatial_us: segment(1)?,
        program_output_boundary_us: segment(2)?,
        monitor_adaptation_us: segment(3)?,
        program_scopes_us: segment(4)?,
        signal_monitoring_us: segment(5)?,
        display_calibration_us: segment(6)?,
    };
    Ok((interval(boundaries[0], boundaries[7])?, stages))
}

fn timestamp_elapsed_us(
    start: u64,
    end: u64,
    timestamp_period_ns: f64,
) -> Result<u64, GpuTimestampReadError> {
    let ticks = end
        .checked_sub(start)
        .ok_or(GpuTimestampReadError::CounterRegression { start, end })?;
    let elapsed_us = ((ticks as f64 * timestamp_period_ns) / 1_000.0).ceil();
    Ok(elapsed_us.min(u64::MAX as f64) as u64)
}

fn pass_timestamp_elapsed_us(
    passes: impl IntoIterator<Item = [u64; 2]>,
    timestamp_period_ns: f64,
) -> Result<u64, GpuTimestampReadError> {
    let mut span: Option<(u64, u64)> = None;
    for [start, end] in passes {
        // A pass's own clock must not regress. Different passes can overlap
        // and complete in a different order, including across Viewer stages.
        timestamp_elapsed_us(start, end, timestamp_period_ns)?;
        span = Some(match span {
            Some((first, last)) => (first.min(start), last.max(end)),
            None => (start, end),
        });
    }
    let (first, last) = span.ok_or(GpuTimestampReadError::ShortReadback)?;
    timestamp_elapsed_us(first, last, timestamp_period_ns)
}

/// Complete hardware interval and each actual pass's raw counter boundaries.
///
/// Adjacent pass pairs expose intervening copies and scheduling gaps. A pass
/// span may itself include preemption; none of these values imply isolated GPU
/// active compute time or replace the complete elapsed-time performance gate.
#[derive(Debug)]
pub struct GpuTimestampPassIntervalSample {
    pub elapsed_us: u64,
    pub timestamp_period_ns: f64,
    pub pass_counters: Vec<[u64; 2]>,
}

impl GpuTimestampPassIntervalSample {
    fn from_counters(
        counters: Vec<u64>,
        timestamp_period_ns: f64,
    ) -> Result<Self, GpuTimestampReadError> {
        let pass_counters: Vec<_> =
            counters.chunks_exact(2).map(|pair| [pair[0], pair[1]]).collect();
        let elapsed_us =
            pass_timestamp_elapsed_us(pass_counters.iter().copied(), timestamp_period_ns)?;
        Ok(Self { elapsed_us, timestamp_period_ns, pass_counters })
    }
}

/// Synchronous hardware timer for a complete bounded command interval.
///
/// Actual render/compute pass boundaries work on Metal without encoder timestamp
/// writes. Copies before the first pass receive the shared measured anchor;
/// uncovered trailing commands, conflicting writes and exhausted query capacity
/// fail closed. This timer reports elapsed time, never Viewer stage attribution.
pub struct GpuTimestampPassIntervalTimer {
    timer: GpuTimestampStageTimer,
    queue: wgpu::Queue,
    leading_copy_anchor: GpuLeadingCopyTimestampAnchor,
    recording_in_flight: bool,
}

impl GpuTimestampPassIntervalTimer {
    /// Allocate exactly two counters per allowed pass, including any copy anchor.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, max_passes: u32) -> Option<Self> {
        let capacity = max_passes.checked_mul(2)?;
        if capacity == 0 || capacity > wgpu::QUERY_SET_MAX_QUERIES {
            return None;
        }
        Some(Self {
            timer: GpuTimestampStageTimer::with_capacity(device, queue, capacity)?,
            queue: queue.clone(),
            leading_copy_anchor: GpuLeadingCopyTimestampAnchor::new(device),
            recording_in_flight: false,
        })
    }

    /// Record the exact production commands and prove complete interval coverage.
    pub fn record<R>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        record: impl FnOnce(&mut dyn crate::gpu_command::GpuCommandEncoder) -> R,
    ) -> Result<
        (R, GpuPassTimestampIntervalReceipt),
        crate::gpu_command::GpuPassTimestampCoverageError,
    > {
        let (result, receipt) = self.record_optional(encoder, record)?;
        Ok((
            result,
            receipt.ok_or(crate::gpu_command::GpuPassTimestampCoverageError::NoPass)?,
        ))
    }

    /// Record a production transform which may be an exact resource alias.
    ///
    /// `None` proves this borrowed scope recorded no GPU commands or completion
    /// map; it is not a measured zero-duration sample. Any real work requires a
    /// consuming receipt, with the same coverage and query ownership rules.
    pub fn record_optional<R>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        record: impl FnOnce(&mut dyn crate::gpu_command::GpuCommandEncoder) -> R,
    ) -> Result<
        (R, Option<GpuPassTimestampIntervalReceipt>),
        crate::gpu_command::GpuPassTimestampCoverageError,
    > {
        if self.recording_in_flight {
            return Err(crate::gpu_command::GpuPassTimestampCoverageError::RecordingInFlight);
        }
        self.recording_in_flight = true;
        let mut measured = GpuPassTimestampRecorder::new_interval(
            encoder,
            self.timer.query_set.clone(),
            self.leading_copy_anchor.clone(),
        );
        let result = record(&mut measured);
        let receipt = measured.finish_optional_interval()?;
        if receipt.is_none() {
            self.recording_in_flight = false;
        }
        Ok((result, receipt))
    }

    /// Consume this timer's receipt only after the exact submission completed.
    pub fn read_sample_after_submission(
        &mut self,
        device: &wgpu::Device,
        submission: wgpu::SubmissionIndex,
        receipt: GpuPassTimestampIntervalReceipt,
    ) -> Result<GpuTimestampPassIntervalSample, GpuTimestampReadError> {
        if receipt.query_set != self.timer.query_set {
            return Err(GpuTimestampReadError::ForeignInterval);
        }
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .map_err(|error| GpuTimestampReadError::DevicePoll(error.to_string()))?;
        // Resolve the bounded actual pass counters without Viewer stage attribution.
        let mut boundaries = [0; 8];
        boundaries[7] = receipt.query_count - 1;
        let layout = GpuPassTimestampLayout { query_count: receipt.query_count, boundaries };
        let mut resolve = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian_gpu_pass_interval_resolve_after_completion"),
        });
        self.timer.resolve(&mut resolve, layout);
        let resolve_submission = self.queue.submit([resolve.finish()]);
        let receiver = self.timer.map_async();
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(resolve_submission),
                timeout: None,
            })
            .map_err(|error| GpuTimestampReadError::DevicePoll(error.to_string()))?;
        receiver
            .recv()
            .map_err(|_| GpuTimestampReadError::MapCallbackDropped)?
            .map_err(|error| GpuTimestampReadError::BufferMap(error.to_string()))?;
        let sample = GpuTimestampPassIntervalSample::from_counters(
            self.timer.read_mapped_query_bytes(receipt.query_count, |bytes| {
                bytes
                    .chunks_exact(wgpu::QUERY_SIZE as usize)
                    .map(|counter| {
                        u64::from_ne_bytes(counter.try_into().expect("eight-byte query counter"))
                    })
                    .collect()
            })?,
            self.timer.timestamp_period_ns,
        )?;
        self.recording_in_flight = false;
        Ok(sample)
    }
}

/// Bounded asynchronous timestamp-query ring.
///
/// Polling never waits. If every slot is in flight, `begin_frame` discards the
/// telemetry sample and increments `discarded_samples`; render submission can
/// continue without backpressure. `finish_all` is intended for offline gates
/// after the measured playback interval, not the realtime presentation loop.
pub struct GpuTimestampQueryRing {
    device: wgpu::Device,
    queue: wgpu::Queue,
    slots: Vec<GpuTimestampQuerySlot>,
    completed: Vec<GpuTimestampSample>,
    next_id: u64,
    discarded_samples: u64,
    pass_timestamps: bool,
    pass_leading_copy_anchor: Option<GpuLeadingCopyTimestampAnchor>,
}

struct GpuTimestampQuerySlot {
    timer: GpuTimestampStageTimer,
    layout: Option<GpuPassTimestampLayout>,
    state: GpuTimestampSlotState,
}

enum GpuTimestampSlotState {
    Free,
    Recording {
        token: GpuTimestampToken,
        next_query_index: u32,
    },
    Recorded {
        token: GpuTimestampToken,
    },
    AwaitingWork {
        token: GpuTimestampToken,
        receiver: Receiver<()>,
    },
    Pending {
        token: GpuTimestampToken,
        receiver: Receiver<Result<(), wgpu::BufferAsyncError>>,
    },
}

impl GpuTimestampQueryRing {
    /// Allocate a ring when timestamps are supported and capacity is non-zero.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, capacity: usize) -> Option<Self> {
        if capacity == 0 || gpu_timestamp_query_device_features(device.features()).is_empty() {
            return None;
        }
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(GpuTimestampQuerySlot {
                timer: GpuTimestampStageTimer::new(device, queue)?,
                layout: Some(encoder_timestamp_layout()),
                state: GpuTimestampSlotState::Free,
            });
        }
        Some(Self {
            device: device.clone(),
            queue: queue.clone(),
            slots,
            completed: Vec::new(),
            next_id: 1,
            discarded_samples: 0,
            pass_timestamps: false,
            pass_leading_copy_anchor: None,
        })
    }

    /// Allocate bounded pass queries without requiring encoder timestamp writes.
    /// Each actual pass consumes two queries. Complete coverage is established
    /// by its consuming receipt, never inferred from feature availability.
    pub fn new_for_passes(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        capacity: usize,
        max_passes_per_frame: u32,
    ) -> Option<Self> {
        let query_capacity = max_passes_per_frame.checked_mul(2)?;
        if capacity == 0 || query_capacity == 0 || query_capacity > wgpu::QUERY_SET_MAX_QUERIES {
            return None;
        }
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(GpuTimestampQuerySlot {
                timer: GpuTimestampStageTimer::with_capacity(device, queue, query_capacity)?,
                layout: None,
                state: GpuTimestampSlotState::Free,
            });
        }
        Some(Self {
            device: device.clone(),
            queue: queue.clone(),
            slots,
            completed: Vec::new(),
            next_id: 1,
            discarded_samples: 0,
            pass_timestamps: true,
            pass_leading_copy_anchor: Some(GpuLeadingCopyTimestampAnchor::new(device)),
        })
    }

    /// Borrow a scope before recording frame commands; polling never waits.
    pub fn begin_pass_frame<'a>(
        &mut self,
        device: &wgpu::Device,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> Result<Option<(GpuTimestampToken, GpuPassTimestampRecorder<'a>)>, GpuTimestampRingError>
    {
        if !self.pass_timestamps {
            return Err(GpuTimestampRingError::RecordingMode);
        }
        let Some(token) = self.acquire_slot(device)? else {
            return Ok(None);
        };
        self.slots[token.slot].layout = None;
        let query_set = self.slots[token.slot].timer.query_set.clone();
        Ok(Some((
            token,
            GpuPassTimestampRecorder::new(
                encoder,
                query_set,
                self.pass_leading_copy_anchor.as_ref().expect("pass anchor").clone(),
                token,
            ),
        )))
    }

    /// Retain even an incomplete scope until its submitted work really finishes.
    /// Returns false for unavailable coverage; such a scope produces no sample.
    pub fn finish_pass_frame(
        &mut self,
        receipt: GpuPassTimestampReceipt,
    ) -> Result<bool, GpuTimestampRingError> {
        if !self.pass_timestamps {
            return Err(GpuTimestampRingError::RecordingMode);
        }
        let token = receipt.token;
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        if slot.timer.query_set != receipt.query_set
            || !matches!(slot.state, GpuTimestampSlotState::Recording { token: active, .. } if active == token)
        {
            return Err(GpuTimestampRingError::InvalidToken);
        }
        slot.layout = receipt.layout.ok();
        slot.state = GpuTimestampSlotState::Recorded { token };
        if slot.layout.is_none() {
            self.discarded_samples = self.discarded_samples.saturating_add(1);
        }
        Ok(slot.layout.is_some())
    }

    /// Whether frame work must be recorded through actual pass boundaries.
    pub const fn uses_pass_timestamps(&self) -> bool {
        self.pass_timestamps
    }

    /// Poll completed maps and begin one frame timestamp without waiting.
    pub fn begin_frame(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<Option<GpuTimestampToken>, GpuTimestampRingError> {
        if self.pass_timestamps {
            return Err(GpuTimestampRingError::RecordingMode);
        }
        let Some(token) = self.acquire_slot(device)? else {
            return Ok(None);
        };
        self.slots[token.slot].layout = Some(encoder_timestamp_layout());
        self.slots[token.slot].timer.begin(encoder);
        Ok(Some(token))
    }

    fn acquire_slot(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<Option<GpuTimestampToken>, GpuTimestampRingError> {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| GpuTimestampRingError::DevicePoll(error.to_string()))?;
        self.collect_ready()?;
        let Some(slot) = self
            .slots
            .iter()
            .position(|slot| matches!(slot.state, GpuTimestampSlotState::Free))
        else {
            self.discarded_samples = self.discarded_samples.saturating_add(1);
            return Ok(None);
        };
        let token = GpuTimestampToken { id: self.next_id, slot };
        self.next_id = self.next_id.saturating_add(1);
        self.slots[slot].state = GpuTimestampSlotState::Recording { token, next_query_index: 1 };
        Ok(Some(token))
    }

    /// Record one ordered Viewer stage marker without submitting or waiting.
    pub fn mark_stage(
        &mut self,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        token: GpuTimestampToken,
        marker: GpuTimestampStageMarker,
    ) -> Result<(), GpuTimestampRingError> {
        if self.pass_timestamps {
            return Err(GpuTimestampRingError::RecordingMode);
        }
        let expected = marker.query_index();
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        match &mut slot.state {
            GpuTimestampSlotState::Recording { token: active, next_query_index }
                if *active == token && *next_query_index == expected =>
            {
                slot.timer
                    .mark(encoder, marker)
                    .map_err(GpuTimestampRingError::EncoderCommand)?;
                *next_query_index = next_query_index.saturating_add(1);
                Ok(())
            }
            _ => Err(GpuTimestampRingError::InvalidStageMarker),
        }
    }

    /// Finish the timestamp commands for the matching active slot.
    pub fn finish_frame(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        token: GpuTimestampToken,
    ) -> Result<(), GpuTimestampRingError> {
        if self.pass_timestamps {
            return Err(GpuTimestampRingError::RecordingMode);
        }
        self.validate_recording_complete(token)?;
        self.slots[token.slot].timer.finish(encoder);
        self.slots[token.slot].state = GpuTimestampSlotState::Recorded { token };
        Ok(())
    }

    /// Abandon a frame that failed before submission and free its query slot.
    pub fn abandon_frame(&mut self, token: GpuTimestampToken) -> Result<(), GpuTimestampRingError> {
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        if matches!(
            slot.state,
            GpuTimestampSlotState::Recording { token: active, .. }
                | GpuTimestampSlotState::Recorded { token: active } if active == token
        ) {
            slot.state = GpuTimestampSlotState::Free;
            Ok(())
        } else {
            Err(GpuTimestampRingError::InvalidToken)
        }
    }

    /// Retain the query slot until measured GPU work completes, then resolve it.
    pub fn after_submit(&mut self, token: GpuTimestampToken) -> Result<(), GpuTimestampRingError> {
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        if !matches!(slot.state, GpuTimestampSlotState::Recorded { token: active } if active == token)
        {
            return Err(GpuTimestampRingError::InvalidToken);
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.queue.on_submitted_work_done(move || {
            let _ = sender.send(());
        });
        slot.state = GpuTimestampSlotState::AwaitingWork { token, receiver };
        Ok(())
    }

    /// Drain samples already completed by non-blocking polls.
    pub fn take_completed(&mut self) -> Vec<GpuTimestampSample> {
        std::mem::take(&mut self.completed)
    }

    /// Drain measured work and deferred resolve submissions after an offline interval.
    pub fn finish_all(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<Vec<GpuTimestampSample>, GpuTimestampRingError> {
        // The first wait retires measured work. The second retires the resolve
        // commands submitted only after those completion callbacks are observed.
        for _ in 0..2 {
            device
                .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
                .map_err(|error| GpuTimestampRingError::DevicePoll(error.to_string()))?;
            self.collect_ready()?;
        }
        if self.slots.iter().any(|slot| !matches!(slot.state, GpuTimestampSlotState::Free)) {
            return Err(GpuTimestampRingError::PendingAfterWait);
        }
        Ok(self.take_completed())
    }

    /// Number of samples discarded because every ring slot was in flight.
    pub const fn discarded_samples(&self) -> u64 {
        self.discarded_samples
    }

    fn validate_recording_complete(
        &self,
        token: GpuTimestampToken,
    ) -> Result<(), GpuTimestampRingError> {
        let Some(slot) = self.slots.get(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        if matches!(
            slot.state,
            GpuTimestampSlotState::Recording {
                token: active,
                next_query_index,
            } if active == token && next_query_index == VIEWER_STAGE_TIMESTAMP_COUNT - 1
        ) {
            Ok(())
        } else {
            Err(GpuTimestampRingError::InvalidToken)
        }
    }

    fn collect_ready(&mut self) -> Result<(), GpuTimestampRingError> {
        for index in 0..self.slots.len() {
            let completed_work = match &self.slots[index].state {
                GpuTimestampSlotState::AwaitingWork { token, receiver } => {
                    match receiver.try_recv() {
                        Ok(()) => Some(*token),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => {
                            return Err(GpuTimestampRingError::WorkCallbackDropped);
                        }
                    }
                }
                _ => None,
            };
            if let Some(token) = completed_work {
                let Some(layout) = self.slots[index].layout else {
                    self.slots[index].state = GpuTimestampSlotState::Free;
                    continue;
                };
                let mut encoder =
                    self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("mondrian_viewer_timestamp_resolve_after_completion"),
                    });
                self.slots[index].timer.resolve(&mut encoder, layout);
                self.queue.submit([encoder.finish()]);
                let receiver = self.slots[index].timer.map_async();
                self.slots[index].state = GpuTimestampSlotState::Pending { token, receiver };
            }
            let callback = match &self.slots[index].state {
                GpuTimestampSlotState::Pending { receiver, .. } => match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        return Err(GpuTimestampRingError::MapCallbackDropped)
                    }
                },
                GpuTimestampSlotState::Free
                | GpuTimestampSlotState::Recording { .. }
                | GpuTimestampSlotState::Recorded { .. }
                | GpuTimestampSlotState::AwaitingWork { .. } => None,
            };
            let Some(callback) = callback else { continue };
            callback.map_err(|error| GpuTimestampRingError::BufferMap(error.to_string()))?;
            let state =
                std::mem::replace(&mut self.slots[index].state, GpuTimestampSlotState::Free);
            let GpuTimestampSlotState::Pending { token, .. } = state else {
                return Err(GpuTimestampRingError::InvalidToken);
            };
            let layout = self.slots[index].layout.ok_or(GpuTimestampRingError::InvalidToken)?;
            let (elapsed_us, stages) =
                self.slots[index].timer.read_mapped_elapsed_us(layout, self.pass_timestamps)?;
            self.completed.push(GpuTimestampSample { token, elapsed_us, stages });
        }
        Ok(())
    }
}

/// Failure in the bounded asynchronous timestamp-query ring.
#[derive(Debug, thiserror::Error)]
pub enum GpuTimestampRingError {
    /// A requested encoder command is unavailable in this recording scope.
    #[error("GPU timestamp encoder command rejected: {0}")]
    EncoderCommand(String),
    /// The caller used an encoder boundary with a pass ring, or vice versa.
    #[error("GPU timestamp recording API does not match the ring mode")]
    RecordingMode,
    /// Non-blocking or final device polling failed.
    #[error("GPU timestamp ring device poll failed: {0}")]
    DevicePoll(String),
    /// A token did not identify the slot currently being recorded.
    #[error("GPU timestamp ring received an invalid or stale token")]
    InvalidToken,
    /// Stage markers were missing, duplicated, or written out of order.
    #[error("GPU timestamp ring received an out-of-order stage marker")]
    InvalidStageMarker,
    /// Measured-work completion was lost before query resolution.
    #[error("GPU timestamp ring work completion callback was dropped")]
    WorkCallbackDropped,
    /// The map callback channel closed before reporting a result.
    #[error("GPU timestamp ring map callback was dropped")]
    MapCallbackDropped,
    /// Mapping a query readback failed.
    #[error("GPU timestamp ring readback mapping failed: {0}")]
    BufferMap(String),
    /// A slot remained active after the offline final wait.
    #[error("GPU timestamp ring retained pending slots after final queue wait")]
    PendingAfterWait,
    /// Reading mapped timestamp counters failed.
    #[error(transparent)]
    Readback(#[from] GpuTimestampReadError),
}

/// Failure reading a completed hardware timestamp query.
#[derive(Debug, thiserror::Error)]
pub enum GpuTimestampReadError {
    /// The consuming interval receipt belongs to a different timer.
    #[error("GPU timestamp interval receipt belongs to another timer")]
    ForeignInterval,
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

#[cfg(test)]
mod tests {
    use super::{
        gpu_frame_timestamp_query_device_features, gpu_timestamp_query_device_features,
        GpuTimestampQueryRing, GpuTimestampRingError, GpuTimestampStageMarker,
    };

    #[test]
    fn interval_sample_preserves_gaps_and_legal_overlap_without_undermeasurement() {
        let sample =
            super::GpuTimestampPassIntervalSample::from_counters(vec![100, 200, 800, 900], 10.0)
                .unwrap();
        assert_eq!(sample.elapsed_us, 8);
        assert_eq!(sample.pass_counters, vec![[100, 200], [800, 900]]);
        assert_eq!(sample.timestamp_period_ns, 10.0);
        let overlap =
            super::GpuTimestampPassIntervalSample::from_counters(vec![100, 900, 150, 200], 10.0)
                .unwrap();
        assert_eq!(overlap.elapsed_us, 8);
        assert_eq!(overlap.pass_counters, vec![[100, 900], [150, 200]]);
        // Recording order does not imply GPU stage execution order either.
        let reordered =
            super::GpuTimestampPassIntervalSample::from_counters(vec![500, 700, 100, 200], 10.0)
                .unwrap();
        assert_eq!(reordered.elapsed_us, 6);
        assert!(matches!(
            super::GpuTimestampPassIntervalSample::from_counters(vec![100, 200, 900, 150], 10.0),
            Err(super::GpuTimestampReadError::CounterRegression { start: 900, end: 150 })
        ));
    }

    #[test]
    fn stage_pass_intervals_preserve_overlap_and_reordered_completion() {
        use crate::gpu_command::GpuPassTimestampLayout;
        let bytes: Vec<_> = [100_u64, 900, 150, 200, 1_000, 1_200]
            .into_iter()
            .flat_map(u64::to_ne_bytes)
            .collect();
        let layout = GpuPassTimestampLayout {
            query_count: 6,
            boundaries: [0, 1, 3, 3, 5, 5, 5, 5],
        };
        let (elapsed, stages) = super::timestamp_stage_durations(&bytes, layout, true, 1_000.0)
            .expect("different passes can legally overlap and finish out of order");
        assert_eq!(elapsed, 1_100);
        assert_eq!(stages.through_working_composite_us, 800);
        assert_eq!(stages.spatial_us, 50);
        assert_eq!(stages.program_output_boundary_us, 0);
        assert_eq!(stages.monitor_adaptation_us, 200);
        assert_eq!(stages.program_scopes_us, 0);
        assert_eq!(stages.signal_monitoring_us, 0);
        assert_eq!(stages.display_calibration_us, 0);

        // Multiple passes in one stage must also retain its longest interval,
        // rather than using the last recorded pass's earlier completion.
        let layout = GpuPassTimestampLayout {
            query_count: 4,
            boundaries: [0, 0, 3, 3, 3, 3, 3, 3],
        };
        let (elapsed, stages) =
            super::timestamp_stage_durations(&bytes[..32], layout, true, 1_000.0)
                .expect("earlier pass remains part of the complete hardware interval");
        assert_eq!(elapsed, 800);
        assert_eq!(stages.through_working_composite_us, 0);
        assert_eq!(stages.spatial_us, 800);
    }

    #[test]
    fn stage_pass_intervals_reject_a_pass_own_counter_regression() {
        use crate::gpu_command::GpuPassTimestampLayout;
        let bytes: Vec<_> =
            [100_u64, 900, 200, 150].into_iter().flat_map(u64::to_ne_bytes).collect();
        let layout = GpuPassTimestampLayout {
            query_count: 4,
            boundaries: [0, 1, 3, 3, 3, 3, 3, 3],
        };
        assert!(matches!(
            super::timestamp_stage_durations(&bytes, layout, true, 1_000.0),
            Err(super::GpuTimestampReadError::CounterRegression { start: 200, end: 150 })
        ));
    }

    #[test]
    fn encoder_stage_intervals_keep_their_ordered_boundary_contract() {
        let bytes: Vec<_> = [0_u64, 100, 300, 600, 600, 1_000, 1_500, 2_100]
            .into_iter()
            .flat_map(u64::to_ne_bytes)
            .collect();
        let (elapsed, stages) = super::timestamp_stage_durations(
            &bytes,
            super::encoder_timestamp_layout(),
            false,
            1_000.0,
        )
        .unwrap();
        assert_eq!(elapsed, 2_100);
        assert_eq!(stages.through_working_composite_us, 100);
        assert_eq!(stages.spatial_us, 200);
        assert_eq!(stages.program_output_boundary_us, 300);
        assert_eq!(stages.monitor_adaptation_us, 0);
        assert_eq!(stages.program_scopes_us, 400);
        assert_eq!(stages.signal_monitoring_us, 500);
        assert_eq!(stages.display_calibration_us, 600);
    }

    #[test]
    fn timestamp_features_are_enabled_only_as_a_complete_pair() {
        assert!(gpu_timestamp_query_device_features(wgpu::Features::empty()).is_empty());
        assert!(gpu_timestamp_query_device_features(wgpu::Features::TIMESTAMP_QUERY).is_empty());
        let required =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        assert_eq!(gpu_timestamp_query_device_features(required), required);
    }

    #[test]
    fn frame_timestamp_features_admit_actual_pass_support() {
        let pass = wgpu::Features::TIMESTAMP_QUERY;
        let encoder = wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        assert!(gpu_frame_timestamp_query_device_features(wgpu::Features::empty()).is_empty());
        assert!(gpu_frame_timestamp_query_device_features(encoder).is_empty());
        assert_eq!(gpu_frame_timestamp_query_device_features(pass), pass);
        assert_eq!(
            gpu_frame_timestamp_query_device_features(pass | encoder),
            pass | encoder
        );
    }

    #[test]
    fn timestamp_ring_discards_when_full_and_collects_after_one_final_wait() {
        let _gpu_permit = crate::context::TestGpuContextPermit::acquire();
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..wgpu::RequestAdapterOptions::default()
            }))
        else {
            eprintln!("skipping timestamp ring test: no GPU adapter available");
            return;
        };
        let required = gpu_timestamp_query_device_features(adapter.features());
        if required.is_empty() {
            eprintln!("skipping timestamp ring test: timestamp features unavailable");
            return;
        }
        let descriptor = wgpu::DeviceDescriptor {
            required_features: required,
            ..wgpu::DeviceDescriptor::default()
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&descriptor)) else {
            eprintln!("skipping timestamp ring test: device creation failed");
            return;
        };
        let mut ring = GpuTimestampQueryRing::new(&device, &queue, 1).expect("timestamp ring");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("timestamp-ring-test"),
        });
        let token = ring
            .begin_frame(&device, &mut encoder)
            .expect("begin timestamp")
            .expect("free timestamp slot");
        assert!(matches!(
            ring.mark_stage(&mut encoder, token, GpuTimestampStageMarker::AfterSpatial,),
            Err(GpuTimestampRingError::InvalidStageMarker)
        ));
        let mut competing_encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("timestamp-ring-competing-test"),
            });
        assert_eq!(
            ring.begin_frame(&device, &mut competing_encoder)
                .expect("discard instead of wait"),
            None
        );
        for marker in [
            GpuTimestampStageMarker::AfterWorkingComposite,
            GpuTimestampStageMarker::AfterSpatial,
            GpuTimestampStageMarker::AfterProgramOutputBoundary,
            GpuTimestampStageMarker::AfterMonitorAdaptation,
            GpuTimestampStageMarker::AfterProgramScopes,
            GpuTimestampStageMarker::AfterSignalMonitoring,
        ] {
            ring.mark_stage(&mut encoder, token, marker).expect("ordered stage marker");
        }
        ring.finish_frame(&mut encoder, token).expect("finish timestamp");
        queue.submit(std::iter::once(encoder.finish()));
        ring.after_submit(token).expect("track timestamp map");

        let samples = ring.finish_all(&device).expect("collect timestamp sample");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].token, token);
        assert_eq!(ring.discarded_samples(), 1);
    }

    #[test]
    fn timestamp_ring_abandon_releases_recording_slot_without_waiting() {
        let _gpu_permit = crate::context::TestGpuContextPermit::acquire();
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..wgpu::RequestAdapterOptions::default()
            }))
        else {
            eprintln!("skipping timestamp ring test: no GPU adapter available");
            return;
        };
        let required = gpu_timestamp_query_device_features(adapter.features());
        if required.is_empty() {
            eprintln!("skipping timestamp ring test: timestamp features unavailable");
            return;
        }
        let descriptor = wgpu::DeviceDescriptor {
            required_features: required,
            ..wgpu::DeviceDescriptor::default()
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&descriptor)) else {
            eprintln!("skipping timestamp ring test: device creation failed");
            return;
        };
        let mut ring = GpuTimestampQueryRing::new(&device, &queue, 1).expect("timestamp ring");
        let mut failed_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("timestamp-ring-abandoned-test"),
        });
        let abandoned = ring
            .begin_frame(&device, &mut failed_encoder)
            .expect("begin abandoned timestamp")
            .expect("free timestamp slot");
        ring.abandon_frame(abandoned).expect("abandon recording slot");

        let mut next_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("timestamp-ring-after-abandon-test"),
        });
        assert!(ring
            .begin_frame(&device, &mut next_encoder)
            .expect("reuse abandoned slot without waiting")
            .is_some());
        assert_eq!(ring.discarded_samples(), 0);
    }
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires real Metal; verifies query completion/resolve ownership using pass counters"]
    fn timestamp_ring_resolves_only_after_work_completion() -> anyhow::Result<()> {
        use super::GpuTimestampSlotState;
        use crate::gpu_command::GpuCommandEncoder;
        use std::time::{Duration, Instant};
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: false,
            ..Default::default()
        }))?;
        anyhow::ensure!(adapter.get_info().backend == wgpu::Backend::Metal);
        anyhow::ensure!(adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY));
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_features: wgpu::Features::TIMESTAMP_QUERY,
                ..Default::default()
            }))?;
        eprintln!(
            "adapter={:?} features={:?}",
            adapter.get_info(),
            adapter.features()
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("metal-pass-timestamp-probe"),
            source: wgpu::ShaderSource::Wgsl(
                "@group(0) @binding(0) var<storage, read_write> result: array<u32>;
                 @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                     result[id.x] = id.x * 3u + 7u;
                 }"
                    .into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("metal-pass-timestamp-probe"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("metal-pass-output"),
            size: 65536 * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("metal-pass-binding"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: output.as_entire_binding() }],
        });
        let mut ring = GpuTimestampQueryRing::new_for_passes(&device, &queue, 1, 4)
            .expect("bounded actual-pass ring");
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("metal-pass-readback"),
            size: 65536 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for sample in 0..10 {
            anyhow::ensure!(matches!(ring.slots[0].state, GpuTimestampSlotState::Free));
            let mut encoder = device.create_command_encoder(&Default::default());
            let (token, mut recorder) = ring.begin_pass_frame(&device, &mut encoder)?.unwrap();
            for marker in [
                GpuTimestampStageMarker::AfterWorkingComposite,
                GpuTimestampStageMarker::AfterSpatial,
                GpuTimestampStageMarker::AfterProgramOutputBoundary,
                GpuTimestampStageMarker::AfterMonitorAdaptation,
            ] {
                {
                    let mut pass = recorder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("metal-real-compute-pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &binding, &[]);
                    pass.dispatch_workgroups(1024, 1, 1);
                }
                recorder.mark_stage(marker);
            }
            recorder.mark_stage(GpuTimestampStageMarker::AfterProgramScopes);
            recorder.mark_stage(GpuTimestampStageMarker::AfterSignalMonitoring);
            let incomplete = sample == 9;
            if incomplete {
                recorder.copy_buffer_to_buffer(&output, 0, &readback, 0, Some(65536 * 4));
            }
            let receipt = recorder.finish();
            if incomplete {
                anyhow::ensure!(
                    receipt.coverage()
                        == Err(crate::gpu_command::GpuPassTimestampCoverageError::TrailingCommands)
                );
            } else {
                anyhow::ensure!(receipt.coverage().is_ok());
            }
            anyhow::ensure!(ring.finish_pass_frame(receipt)? != incomplete);
            anyhow::ensure!(matches!(
                ring.slots[0].state,
                GpuTimestampSlotState::Recorded { .. }
            ));
            // Oracle readback is outside the measured compute scope.
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 65536 * 4);
            queue.submit([encoder.finish()]);
            ring.after_submit(token)?;
            anyhow::ensure!(matches!(
                ring.slots[0].state,
                GpuTimestampSlotState::AwaitingWork { .. }
            ));
            anyhow::ensure!(matches!(
                ring.after_submit(token),
                Err(GpuTimestampRingError::InvalidToken)
            ));
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            readback.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            let mut saw_resolve_pending = false;
            let samples = if sample >= 8 {
                ring.finish_all(&device)?
            } else {
                let deadline = Instant::now() + Duration::from_secs(5);
                while ring.completed.is_empty() {
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "asynchronous query retirement timed out"
                    );
                    device.poll(wgpu::PollType::Poll)?;
                    ring.collect_ready()?;
                    saw_resolve_pending |=
                        matches!(ring.slots[0].state, GpuTimestampSlotState::Pending { .. });
                    std::thread::yield_now();
                }
                ring.take_completed()
            };
            anyhow::ensure!(matches!(ring.slots[0].state, GpuTimestampSlotState::Free));
            if incomplete {
                anyhow::ensure!(samples.is_empty() && ring.discarded_samples() == 1);
                eprintln!(
                    "sample={sample} token={} incomplete=true retired=true",
                    token.id()
                );
            } else {
                anyhow::ensure!(samples.len() == 1 && samples[0].token == token);
                anyhow::ensure!(samples[0].elapsed_us > 0 && (sample == 8 || saw_resolve_pending));
                eprintln!(
                    "sample={sample} token={} gpu_elapsed_us={} retired=true",
                    token.id(),
                    samples[0].elapsed_us
                );
            }
            rx.recv_timeout(Duration::from_secs(5))??;
            let mapped = readback.slice(..).get_mapped_range()?;
            for (index, bytes) in mapped.chunks_exact(4).enumerate() {
                anyhow::ensure!(u32::from_ne_bytes(bytes.try_into()?) == index as u32 * 3 + 7);
            }
            drop(mapped);
            readback.unmap();
        }
        let mut interval_timer =
            super::GpuTimestampPassIntervalTimer::new(&device, &queue, 2).unwrap();
        for _ in 0..3 {
            let mut empty = device.create_command_encoder(&Default::default());
            let (alias, receipt) = interval_timer
                .record_optional(&mut empty, |_| 42_u64)
                .map_err(|error| anyhow::anyhow!("empty interval: {error:?}"))?;
            assert_eq!(alias, 42);
            assert!(receipt.is_none());
            assert!(!interval_timer.recording_in_flight);
        }
        let mut map_timer = super::GpuTimestampPassIntervalTimer::new(&device, &queue, 1).unwrap();
        let mut map_only = device.create_command_encoder(&Default::default());
        assert!(matches!(
            map_timer.record_optional(&mut map_only, |measured| {
                measured.map_buffer_on_submit(&readback, wgpu::MapMode::Read, Box::new(|_| {}));
            }),
            Err(crate::gpu_command::GpuPassTimestampCoverageError::UnmeasuredStageBoundary)
        ));
        assert!(map_timer.recording_in_flight);
        drop(map_only);
        for _ in 0..3 {
            let mut encoder = device.create_command_encoder(&Default::default());
            let (_, receipt) = interval_timer
                .record(&mut encoder, |measured| {
                    measured.clear_buffer(&output, 0, None);
                    let mut pass = measured.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("metal-interval-real-compute"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &binding, &[]);
                    pass.dispatch_workgroups(1024, 1, 1);
                })
                .map_err(|error| anyhow::anyhow!("interval coverage: {error:?}"))?;
            let mut rejected = device.create_command_encoder(&Default::default());
            let mut replacement_executed = false;
            assert!(matches!(
                interval_timer.record(&mut rejected, |_| replacement_executed = true),
                Err(crate::gpu_command::GpuPassTimestampCoverageError::RecordingInFlight)
            ));
            assert!(!replacement_executed);
            // The oracle copy is explicitly outside the measured interval.
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 65536 * 4);
            let submission = queue.submit([encoder.finish()]);
            let interval =
                interval_timer.read_sample_after_submission(&device, submission, receipt)?;
            anyhow::ensure!(interval.elapsed_us > 0 && interval.pass_counters.len() == 2);
            let elapsed_us = interval.elapsed_us;
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            readback.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
            rx.recv_timeout(Duration::from_secs(5))??;
            let mapped = readback.slice(..).get_mapped_range()?;
            for (index, bytes) in mapped.chunks_exact(4).enumerate() {
                anyhow::ensure!(u32::from_ne_bytes(bytes.try_into()?) == index as u32 * 3 + 7);
            }
            drop(mapped);
            readback.unmap();
            eprintln!(
                "complete pass interval gpu_elapsed_us={elapsed_us} query_reuse_after_read=true"
            );
        }
        let mut foreign_timer =
            super::GpuTimestampPassIntervalTimer::new(&device, &queue, 1).unwrap();
        let mut encoder = device.create_command_encoder(&Default::default());
        let (_, receipt) = interval_timer
            .record(&mut encoder, |measured| {
                let mut pass = measured.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &binding, &[]);
                pass.dispatch_workgroups(1024, 1, 1);
            })
            .map_err(|error| anyhow::anyhow!("interval coverage: {error:?}"))?;
        let submission = queue.submit([encoder.finish()]);
        assert!(matches!(
            foreign_timer.read_sample_after_submission(&device, submission.clone(), receipt),
            Err(super::GpuTimestampReadError::ForeignInterval)
        ));
        assert!(interval_timer.recording_in_flight);
        device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })?;
        Ok(())
    }
}
