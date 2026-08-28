//! GPU execution profiling with hardware timestamp queries.
//!
//! Timestamp queries measure work on the GPU timeline. They intentionally stay
//! separate from CPU command recording, queue submission, and completion-wait
//! latency so callers can attribute stalls instead of treating wall time as GPU
//! render time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

static ENABLED: AtomicBool = AtomicBool::new(false);
const TIMESTAMP_READBACK_BYTES: u64 = 2 * wgpu::QUERY_SIZE as u64;
const VIEWER_STAGE_TIMESTAMP_COUNT: u32 = 7;
const VIEWER_STAGE_TIMESTAMP_READBACK_BYTES: u64 =
    VIEWER_STAGE_TIMESTAMP_COUNT as u64 * wgpu::QUERY_SIZE as u64;

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

    /// Begin an asynchronous map request for the resolved counters.
    pub fn map_async(&self) -> Receiver<Result<(), wgpu::BufferAsyncError>> {
        let slice = self.readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        receiver
    }

    /// Read and unmap counters after a successful map callback.
    pub fn read_mapped_elapsed_us(&self) -> Result<u64, GpuTimestampReadError> {
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
        let receiver = self.map_async();
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
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
    /// Hardware GPU duration between the two encoder timestamps.
    pub elapsed_us: u64,
    /// Hardware GPU attribution between ordered Viewer stage markers.
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
}

impl GpuTimestampStageMarker {
    const fn query_index(self) -> u32 {
        match self {
            Self::AfterWorkingComposite => 1,
            Self::AfterSpatial => 2,
            Self::AfterProgramOutputBoundary => 3,
            Self::AfterMonitorAdaptation => 4,
            Self::AfterProgramScopes => 5,
        }
    }
}

/// Hardware GPU duration attributed to each ordered Viewer stage.
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
    /// Optional display calibration after monitor adaptation.
    pub display_calibration_us: u64,
}

struct GpuTimestampStageTimer {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    timestamp_period_ns: f64,
}

impl GpuTimestampStageTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let required = gpu_timestamp_query_device_features(device.features());
        if required.is_empty() {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("mondrian_viewer_stage_timestamp_queries"),
            ty: wgpu::QueryType::Timestamp,
            count: VIEWER_STAGE_TIMESTAMP_COUNT,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_viewer_stage_timestamp_resolve"),
            size: VIEWER_STAGE_TIMESTAMP_READBACK_BYTES,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_viewer_stage_timestamp_readback"),
            size: VIEWER_STAGE_TIMESTAMP_READBACK_BYTES,
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

    fn mark(&self, encoder: &mut wgpu::CommandEncoder, marker: GpuTimestampStageMarker) {
        encoder.write_timestamp(&self.query_set, marker.query_index());
    }

    fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, VIEWER_STAGE_TIMESTAMP_COUNT - 1);
        encoder.resolve_query_set(
            &self.query_set,
            0..VIEWER_STAGE_TIMESTAMP_COUNT,
            &self.resolve_buffer,
            0,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            VIEWER_STAGE_TIMESTAMP_READBACK_BYTES,
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

    fn read_mapped_elapsed_us(
        &self,
    ) -> Result<(u64, GpuTimestampStageDurations), GpuTimestampReadError> {
        let slice = self.readback_buffer.slice(..);
        let mapped = match slice.get_mapped_range() {
            Ok(mapped) => mapped,
            Err(error) => {
                self.readback_buffer.unmap();
                return Err(GpuTimestampReadError::MappedRange(error.to_string()));
            }
        };
        if mapped.len() < VIEWER_STAGE_TIMESTAMP_READBACK_BYTES as usize {
            drop(mapped);
            self.readback_buffer.unmap();
            return Err(GpuTimestampReadError::ShortReadback);
        }
        let mut values = [0_u64; VIEWER_STAGE_TIMESTAMP_COUNT as usize];
        for (index, value) in values.iter_mut().enumerate() {
            let offset = index * wgpu::QUERY_SIZE as usize;
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(&mapped[offset..offset + 8]);
            *value = u64::from_ne_bytes(bytes);
        }
        drop(mapped);
        self.readback_buffer.unmap();

        let segment = |start: usize, end: usize| {
            timestamp_elapsed_us(values[start], values[end], self.timestamp_period_ns)
        };
        let stages = GpuTimestampStageDurations {
            through_working_composite_us: segment(0, 1)?,
            spatial_us: segment(1, 2)?,
            program_output_boundary_us: segment(2, 3)?,
            monitor_adaptation_us: segment(3, 4)?,
            program_scopes_us: segment(4, 5)?,
            display_calibration_us: segment(5, 6)?,
        };
        Ok((segment(0, 6)?, stages))
    }
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

/// Bounded asynchronous timestamp-query ring.
///
/// Polling never waits. If every slot is in flight, `begin_frame` discards the
/// telemetry sample and increments `discarded_samples`; render submission can
/// continue without backpressure. `finish_all` is intended for offline gates
/// after the measured playback interval, not the realtime presentation loop.
pub struct GpuTimestampQueryRing {
    slots: Vec<GpuTimestampQuerySlot>,
    completed: Vec<GpuTimestampSample>,
    next_id: u64,
    discarded_samples: u64,
}

struct GpuTimestampQuerySlot {
    timer: GpuTimestampStageTimer,
    state: GpuTimestampSlotState,
}

enum GpuTimestampSlotState {
    Free,
    Recording {
        token: GpuTimestampToken,
        next_query_index: u32,
    },
    Pending {
        token: GpuTimestampToken,
        receiver: Receiver<Result<(), wgpu::BufferAsyncError>>,
    },
}

impl GpuTimestampQueryRing {
    /// Allocate a ring when timestamps are supported and capacity is non-zero.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, capacity: usize) -> Option<Self> {
        if capacity == 0 {
            return None;
        }
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(GpuTimestampQuerySlot {
                timer: GpuTimestampStageTimer::new(device, queue)?,
                state: GpuTimestampSlotState::Free,
            });
        }
        Some(Self {
            slots,
            completed: Vec::new(),
            next_id: 1,
            discarded_samples: 0,
        })
    }

    /// Poll completed maps and begin one frame timestamp without waiting.
    pub fn begin_frame(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
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
        self.slots[slot].timer.begin(encoder);
        self.slots[slot].state = GpuTimestampSlotState::Recording { token, next_query_index: 1 };
        Ok(Some(token))
    }

    /// Record one ordered Viewer stage marker without submitting or waiting.
    pub fn mark_stage(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        token: GpuTimestampToken,
        marker: GpuTimestampStageMarker,
    ) -> Result<(), GpuTimestampRingError> {
        let expected = marker.query_index();
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        match &mut slot.state {
            GpuTimestampSlotState::Recording { token: active, next_query_index }
                if *active == token && *next_query_index == expected =>
            {
                slot.timer.mark(encoder, marker);
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
        self.validate_recording_complete(token)?;
        self.slots[token.slot].timer.finish(encoder);
        Ok(())
    }

    /// Abandon a frame that failed before submission and free its query slot.
    pub fn abandon_frame(&mut self, token: GpuTimestampToken) -> Result<(), GpuTimestampRingError> {
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err(GpuTimestampRingError::InvalidToken);
        };
        if matches!(
            slot.state,
            GpuTimestampSlotState::Recording { token: active, .. } if active == token
        ) {
            slot.state = GpuTimestampSlotState::Free;
            Ok(())
        } else {
            Err(GpuTimestampRingError::InvalidToken)
        }
    }

    /// Start map completion tracking after the frame command buffer is submitted.
    pub fn after_submit(&mut self, token: GpuTimestampToken) -> Result<(), GpuTimestampRingError> {
        self.validate_recording_complete(token)?;
        let receiver = self.slots[token.slot].timer.map_async();
        self.slots[token.slot].state = GpuTimestampSlotState::Pending { token, receiver };
        Ok(())
    }

    /// Drain samples already completed by non-blocking polls.
    pub fn take_completed(&mut self) -> Vec<GpuTimestampSample> {
        std::mem::take(&mut self.completed)
    }

    /// Wait once after an offline measurement interval and return all samples.
    pub fn finish_all(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<Vec<GpuTimestampSample>, GpuTimestampRingError> {
        device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|error| GpuTimestampRingError::DevicePoll(error.to_string()))?;
        self.collect_ready()?;
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
            let callback = match &self.slots[index].state {
                GpuTimestampSlotState::Pending { receiver, .. } => match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        return Err(GpuTimestampRingError::MapCallbackDropped)
                    }
                },
                GpuTimestampSlotState::Free | GpuTimestampSlotState::Recording { .. } => None,
            };
            let Some(callback) = callback else { continue };
            callback.map_err(|error| GpuTimestampRingError::BufferMap(error.to_string()))?;
            let state =
                std::mem::replace(&mut self.slots[index].state, GpuTimestampSlotState::Free);
            let GpuTimestampSlotState::Pending { token, .. } = state else {
                return Err(GpuTimestampRingError::InvalidToken);
            };
            let (elapsed_us, stages) = self.slots[index].timer.read_mapped_elapsed_us()?;
            self.completed.push(GpuTimestampSample { token, elapsed_us, stages });
        }
        Ok(())
    }
}

/// Failure in the bounded asynchronous timestamp-query ring.
#[derive(Debug, thiserror::Error)]
pub enum GpuTimestampRingError {
    /// Non-blocking or final device polling failed.
    #[error("GPU timestamp ring device poll failed: {0}")]
    DevicePoll(String),
    /// A token did not identify the slot currently being recorded.
    #[error("GPU timestamp ring received an invalid or stale token")]
    InvalidToken,
    /// Stage markers were missing, duplicated, or written out of order.
    #[error("GPU timestamp ring received an out-of-order stage marker")]
    InvalidStageMarker,
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
        gpu_timestamp_query_device_features, GpuTimestampQueryRing, GpuTimestampRingError,
        GpuTimestampStageMarker,
    };

    #[test]
    fn timestamp_features_are_enabled_only_as_a_complete_pair() {
        assert!(gpu_timestamp_query_device_features(wgpu::Features::empty()).is_empty());
        assert!(gpu_timestamp_query_device_features(wgpu::Features::TIMESTAMP_QUERY).is_empty());
        let required =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        assert_eq!(gpu_timestamp_query_device_features(required), required);
    }

    #[test]
    fn timestamp_ring_discards_when_full_and_collects_after_one_final_wait() {
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
}
