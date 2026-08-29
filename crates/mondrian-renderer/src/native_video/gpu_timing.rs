//! Bounded asynchronous GPU evidence for the native-video import prefix.
//!
//! Native import is submitted before the ordinary Viewer command buffer. Its
//! YUV decode and source-to-working color stages therefore need their own
//! timestamp lifecycle; the semantically fixed Viewer suffix timestamps cannot
//! describe this work.

#[cfg(target_os = "windows")]
use crate::profile::gpu_timestamp_query_device_features;
#[cfg(target_os = "windows")]
use std::sync::mpsc::{Receiver, TryRecvError};

#[cfg(target_os = "windows")]
const NATIVE_IMPORT_TIMESTAMP_COUNT: u32 = 3;
#[cfg(target_os = "windows")]
const NATIVE_IMPORT_TIMESTAMP_READBACK_BYTES: u64 =
    NATIVE_IMPORT_TIMESTAMP_COUNT as u64 * wgpu::QUERY_SIZE as u64;

/// Schema version for serialized native-import GPU timing evidence.
pub const NATIVE_VIDEO_IMPORT_GPU_TIMING_SCHEMA_VERSION: u32 = 1;

/// Hard upper bound for explicitly activated native-import timestamp slots.
pub const NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY: usize = 256;

/// Product policy for native-import GPU timing evidence.
///
/// Capability discovery never activates profiling by itself. The default is
/// disabled so normal product execution allocates no query/readback resources.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NativeVideoImportGpuTimingPolicy {
    /// Do not allocate or record native-import timestamps.
    #[default]
    Disabled,
    /// Activate a bounded asynchronous query ring with the exact slot count.
    Enabled {
        /// Maximum native imports awaiting asynchronous readback.
        capacity: usize,
    },
}

/// Backend-runtime-local identity of one Viewer candidate.
///
/// Reports that combine multiple renderer runtimes must pair this token with
/// their own execution-session identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NativeVideoImportCandidateToken(u64);

impl NativeVideoImportCandidateToken {
    /// Stable run-local numeric identity.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Backend-runtime-local identity of one native source import.
///
/// Reports that combine multiple renderer runtimes must pair this token with
/// their own execution-session identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NativeVideoImportToken(u64);

impl NativeVideoImportToken {
    /// Stable run-local numeric identity.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Move-only terminal accounting for one successful Viewer candidate scope.
///
/// This receipt exists only when timing was activated and the surrounding
/// Viewer record succeeded. Its counts come from exact probe terminal
/// consumption inside that candidate scope; callers must not reconstruct them
/// from layer counts or completed asynchronous samples.
///
/// The counts always satisfy
/// `submitted_imports == scheduled_samples + missing_samples + dropped_samples`.
/// Scheduling proves only that asynchronous readback was registered; it does
/// not prove that the corresponding hardware sample has completed.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[must_use = "native-import candidate timing receipts are exact move-only evidence"]
pub struct NativeVideoImportCandidateTimingReceipt {
    candidate_token: NativeVideoImportCandidateToken,
    submitted_imports: u64,
    scheduled_samples: u64,
    missing_samples: u64,
    dropped_samples: u64,
}

impl NativeVideoImportCandidateTimingReceipt {
    /// Backend-runtime-local candidate identity.
    pub const fn candidate_token(&self) -> NativeVideoImportCandidateToken {
        self.candidate_token
    }

    /// Imports that returned valid working-frame outputs.
    pub const fn submitted_imports(&self) -> u64 {
        self.submitted_imports
    }

    /// Samples successfully admitted to asynchronous readback.
    pub const fn scheduled_samples(&self) -> u64 {
        self.scheduled_samples
    }

    /// Successful imports that could not schedule a timestamp sample.
    pub const fn missing_samples(&self) -> u64 {
        self.missing_samples
    }

    /// Successful imports intentionally unsampled because the ring was full.
    pub const fn dropped_samples(&self) -> u64 {
        self.dropped_samples
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        candidate_token: u64,
        submitted_imports: u64,
        scheduled_samples: u64,
        missing_samples: u64,
        dropped_samples: u64,
    ) -> Self {
        assert_eq!(
            submitted_imports,
            scheduled_samples + missing_samples + dropped_samples
        );
        Self {
            candidate_token: NativeVideoImportCandidateToken(candidate_token),
            submitted_imports,
            scheduled_samples,
            missing_samples,
            dropped_samples,
        }
    }
}

/// One asynchronously completed native-import hardware timestamp sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct NativeVideoImportGpuTimingSample {
    /// Viewer candidate that requested this import.
    pub candidate_token: NativeVideoImportCandidateToken,
    /// Exact native import within the candidate.
    pub import_token: NativeVideoImportToken,
    /// GPU-timeline attribution bracketed by the post-acquire/start and
    /// after-YUV markers.
    ///
    /// The interval can include implicit barriers, scheduler gaps, and backend
    /// command placement within those markers; it is not pure shader time.
    pub yuv_decode_marker_bracket_us: u64,
    /// GPU-timeline attribution bracketed by the after-YUV and
    /// after-input-color markers.
    ///
    /// The interval can include implicit barriers, scheduler gaps, and backend
    /// command placement within those markers; it is not pure shader time.
    pub input_color_marker_bracket_us: u64,
    /// Whether the decoder fence was already complete immediately before the
    /// GPU-queue wait/copy/acquire chain was published.
    ///
    /// `None` means the native API could not provide a trustworthy completion
    /// observation. This fact is diagnostic only and never changes rendering.
    pub decode_fence_ready_at_admission: Option<bool>,
}

/// Cumulative health and coverage of bounded native-import GPU evidence.
///
/// The accounting categories are disjoint. At every observation point:
///
/// `submitted_imports == samples + pending + missing + dropped`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeVideoImportGpuTimingDiagnostics {
    /// Evidence schema interpreted by readers.
    pub schema_version: u32,
    /// Whether the active device exposes the complete timestamp capability.
    pub capability_supported: bool,
    /// Whether policy enabled a healthy query lifecycle.
    pub activated: bool,
    /// Stable policy, capability, or runtime-failure explanation when inactive.
    pub inactive_reason: Option<String>,
    /// Native imports that returned a valid working-frame output.
    ///
    /// A native-import failure after ambiguous GPU queue acceptance is intentionally
    /// excluded because it formed no usable Viewer native-import output. This
    /// is output coverage, not an inventory of every possibly accepted GPU
    /// command buffer.
    pub submitted_imports: u64,
    /// Completed hardware timestamp samples.
    pub samples: u64,
    /// Submitted samples whose asynchronous readback has not completed.
    pub pending: u64,
    /// Submitted imports with no sample because timing was unsupported or its
    /// lifecycle failed.
    pub missing: u64,
    /// Submitted imports intentionally not sampled because the bounded ring
    /// had no free slot.
    pub dropped: u64,
}

impl NativeVideoImportGpuTimingDiagnostics {
    pub(crate) fn inactive(capability_supported: bool, reason: impl Into<String>) -> Self {
        Self {
            schema_version: NATIVE_VIDEO_IMPORT_GPU_TIMING_SCHEMA_VERSION,
            capability_supported,
            activated: false,
            inactive_reason: Some(reason.into()),
            submitted_imports: 0,
            samples: 0,
            pending: 0,
            missing: 0,
            dropped: 0,
        }
    }
}

#[derive(Debug)]
#[cfg(target_os = "windows")]
pub(super) struct NativeVideoImportGpuTimingProbe {
    candidate_token: Option<NativeVideoImportCandidateToken>,
    import_token: Option<NativeVideoImportToken>,
    decode_fence_ready_at_admission: Option<bool>,
    disposition: NativeVideoImportGpuTimingProbeDisposition,
}

#[derive(Debug)]
#[cfg(target_os = "windows")]
enum NativeVideoImportGpuTimingProbeDisposition {
    Recording(NativeVideoImportGpuTimestampToken),
    Missing,
    Dropped,
}

#[derive(Debug, Clone, Copy)]
#[cfg(target_os = "windows")]
enum NativeVideoImportCandidateSampleDisposition {
    Scheduled,
    Missing,
    Dropped,
}

#[cfg(target_os = "windows")]
pub(super) struct NativeVideoImportGpuTimingRuntime {
    ring: Option<NativeVideoImportGpuTimestampRing>,
    capability_supported: bool,
    inactive_reason: Option<String>,
    active_candidate: Option<NativeVideoImportActiveCandidate>,
    next_candidate_id: u64,
    next_import_id: u64,
    submitted_imports: u64,
    completed_samples: u64,
    dropped_samples: u64,
    completed: Vec<NativeVideoImportGpuTimingSample>,
}

#[cfg(target_os = "windows")]
struct NativeVideoImportActiveCandidate {
    token: NativeVideoImportCandidateToken,
    submitted_imports: u64,
    scheduled_samples: u64,
    missing_samples: u64,
    dropped_samples: u64,
}

#[cfg(target_os = "windows")]
impl NativeVideoImportActiveCandidate {
    const fn new(token: NativeVideoImportCandidateToken) -> Self {
        Self {
            token,
            submitted_imports: 0,
            scheduled_samples: 0,
            missing_samples: 0,
            dropped_samples: 0,
        }
    }

    fn into_receipt(self) -> NativeVideoImportCandidateTimingReceipt {
        debug_assert_eq!(
            self.submitted_imports,
            self.scheduled_samples
                .saturating_add(self.missing_samples)
                .saturating_add(self.dropped_samples)
        );
        NativeVideoImportCandidateTimingReceipt {
            candidate_token: self.token,
            submitted_imports: self.submitted_imports,
            scheduled_samples: self.scheduled_samples,
            missing_samples: self.missing_samples,
            dropped_samples: self.dropped_samples,
        }
    }
}

#[cfg(target_os = "windows")]
impl NativeVideoImportGpuTimingRuntime {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        policy: NativeVideoImportGpuTimingPolicy,
    ) -> Self {
        let capability_supported =
            !gpu_timestamp_query_device_features(device.features()).is_empty();
        let (ring, inactive_reason) = match activated_capacity(capability_supported, policy) {
            Ok(capacity) => (
                Some(NativeVideoImportGpuTimestampRing::new(
                    device, queue, capacity,
                )),
                None,
            ),
            Err(reason) => (None, Some(reason)),
        };
        Self {
            ring,
            capability_supported,
            inactive_reason,
            active_candidate: None,
            next_candidate_id: 1,
            next_import_id: 1,
            submitted_imports: 0,
            completed_samples: 0,
            dropped_samples: 0,
            completed: Vec::new(),
        }
    }

    pub(super) fn begin_candidate(&mut self) -> Option<NativeVideoImportCandidateToken> {
        if self.active_candidate.is_some() {
            self.disable(
                "native-import GPU timing rejected an overlapping candidate scope".to_owned(),
            );
            self.active_candidate = None;
            return None;
        }
        self.ring.as_ref()?;
        let token =
            allocate_token(&mut self.next_candidate_id).map(NativeVideoImportCandidateToken);
        if token.is_none() {
            self.disable("native-import candidate timing identity exhausted".to_owned());
        }
        self.active_candidate = token.map(NativeVideoImportActiveCandidate::new);
        token
    }

    pub(super) fn end_candidate(
        &mut self,
        candidate: Option<NativeVideoImportCandidateToken>,
        viewer_record_succeeded: bool,
    ) -> Option<NativeVideoImportCandidateTimingReceipt> {
        match (self.active_candidate.take(), candidate) {
            (None, None) => None,
            (Some(active), Some(candidate)) if active.token == candidate => {
                viewer_record_succeeded.then(|| active.into_receipt())
            }
            _ => {
                self.disable(
                    "native-import GPU timing candidate scope ended with a foreign token"
                        .to_owned(),
                );
                None
            }
        }
    }

    pub(super) fn begin_import(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        decode_fence_ready_at_admission: Option<bool>,
    ) -> NativeVideoImportGpuTimingProbe {
        let candidate_token = self.active_candidate.as_ref().map(|active| active.token);
        let import_token = allocate_token(&mut self.next_import_id).map(NativeVideoImportToken);
        if import_token.is_none() {
            self.disable("native-import timing identity exhausted".to_owned());
        }
        let mut probe = NativeVideoImportGpuTimingProbe {
            candidate_token,
            import_token,
            decode_fence_ready_at_admission,
            disposition: NativeVideoImportGpuTimingProbeDisposition::Missing,
        };
        if candidate_token.is_none() || import_token.is_none() {
            return probe;
        }
        let Some(mut ring) = self.ring.take() else {
            return probe;
        };
        match ring.begin(encoder) {
            Ok(NativeVideoImportGpuTimestampAdmission::Recording(token)) => {
                probe.disposition = NativeVideoImportGpuTimingProbeDisposition::Recording(token);
                self.ring = Some(ring);
            }
            Ok(NativeVideoImportGpuTimestampAdmission::Dropped) => {
                probe.disposition = NativeVideoImportGpuTimingProbeDisposition::Dropped;
                self.ring = Some(ring);
            }
            Err(error) => {
                self.disable_with_ring(ring, error);
            }
        }
        probe
    }

    pub(super) fn mark_after_yuv(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        probe: &mut NativeVideoImportGpuTimingProbe,
    ) {
        self.mark(
            encoder,
            probe,
            NativeVideoImportGpuTimestampMarker::AfterYuv,
        );
    }

    pub(super) fn mark_after_input_color(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        probe: &mut NativeVideoImportGpuTimingProbe,
    ) {
        self.mark(
            encoder,
            probe,
            NativeVideoImportGpuTimestampMarker::AfterInputColor,
        );
    }

    pub(super) fn finish_recording(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        probe: &mut NativeVideoImportGpuTimingProbe,
    ) {
        let NativeVideoImportGpuTimingProbeDisposition::Recording(token) = &probe.disposition
        else {
            return;
        };
        let Some(mut ring) = self.ring.take() else {
            probe.disposition = NativeVideoImportGpuTimingProbeDisposition::Missing;
            return;
        };
        if let Err(error) = ring.finish(encoder, *token) {
            probe.disposition = NativeVideoImportGpuTimingProbeDisposition::Missing;
            self.disable_with_ring(ring, error);
            return;
        }
        self.ring = Some(ring);
    }

    pub(super) fn abandon_before_submit(&mut self, probe: NativeVideoImportGpuTimingProbe) {
        let disposition = probe.disposition;
        let NativeVideoImportGpuTimingProbeDisposition::Recording(token) = disposition else {
            return;
        };
        let Some(mut ring) = self.ring.take() else {
            return;
        };
        if let Err(error) = ring.abandon(token) {
            self.disable_with_ring(ring, error);
            return;
        }
        self.ring = Some(ring);
    }

    pub(super) fn submission_failed_after_queue(
        &mut self,
        probe: NativeVideoImportGpuTimingProbe,
        reason: String,
    ) {
        if matches!(
            &probe.disposition,
            NativeVideoImportGpuTimingProbeDisposition::Recording(_)
        ) {
            // Native import can report a raw release failure after the wgpu
            // command buffer was accepted. Never recycle its query resources
            // when submission is ambiguous. It is not added to
            // `submitted_imports`: no usable native working-frame output was
            // formed, and these diagnostics measure output coverage rather
            // than every command buffer the GPU may have accepted.
            self.disable(format!(
                "native-import timestamp submission became ambiguous: {reason}"
            ));
        }
    }

    pub(super) fn after_submit(&mut self, probe: NativeVideoImportGpuTimingProbe) {
        self.submitted_imports = self.submitted_imports.saturating_add(1);
        let NativeVideoImportGpuTimingProbe {
            candidate_token,
            import_token,
            decode_fence_ready_at_admission,
            disposition,
        } = probe;
        match disposition {
            NativeVideoImportGpuTimingProbeDisposition::Recording(token) => {
                let Some(import_token) = import_token else {
                    self.disable(
                        "native-import timestamp probe lost its evidence identity".to_owned(),
                    );
                    self.account_candidate_terminal(
                        candidate_token,
                        NativeVideoImportCandidateSampleDisposition::Missing,
                    );
                    return;
                };
                let Some(candidate_token_value) = candidate_token else {
                    self.disable(
                        "native-import timestamp probe lost its candidate identity".to_owned(),
                    );
                    return;
                };
                let metadata = NativeVideoImportGpuTimestampMetadata {
                    candidate_token: candidate_token_value,
                    import_token,
                    decode_fence_ready_at_admission,
                };
                let Some(mut ring) = self.ring.take() else {
                    self.account_candidate_terminal(
                        candidate_token,
                        NativeVideoImportCandidateSampleDisposition::Missing,
                    );
                    return;
                };
                if let Err(error) = ring.after_submit(token, metadata) {
                    self.disable_with_ring(ring, error);
                    self.account_candidate_terminal(
                        candidate_token,
                        NativeVideoImportCandidateSampleDisposition::Missing,
                    );
                    return;
                }
                self.ring = Some(ring);
                self.account_candidate_terminal(
                    candidate_token,
                    NativeVideoImportCandidateSampleDisposition::Scheduled,
                );
            }
            NativeVideoImportGpuTimingProbeDisposition::Missing => {
                self.account_candidate_terminal(
                    candidate_token,
                    NativeVideoImportCandidateSampleDisposition::Missing,
                );
            }
            NativeVideoImportGpuTimingProbeDisposition::Dropped => {
                self.dropped_samples = self.dropped_samples.saturating_add(1);
                self.account_candidate_terminal(
                    candidate_token,
                    NativeVideoImportCandidateSampleDisposition::Dropped,
                );
            }
        }
    }

    pub(super) fn collect_after_device_poll(&mut self) {
        let Some(mut ring) = self.ring.take() else {
            return;
        };
        match ring.collect_after_device_poll() {
            Ok(samples) => {
                self.accept_samples(samples);
                self.ring = Some(ring);
            }
            Err(error) => self.disable_with_ring(ring, error),
        }
    }

    pub(super) fn take_completed(&mut self) -> Vec<NativeVideoImportGpuTimingSample> {
        std::mem::take(&mut self.completed)
    }

    pub(super) fn diagnostics(&self) -> NativeVideoImportGpuTimingDiagnostics {
        let pending =
            self.ring.as_ref().map_or(0, NativeVideoImportGpuTimestampRing::pending_count);
        let classified = self
            .completed_samples
            .saturating_add(pending)
            .saturating_add(self.dropped_samples);
        NativeVideoImportGpuTimingDiagnostics {
            schema_version: NATIVE_VIDEO_IMPORT_GPU_TIMING_SCHEMA_VERSION,
            capability_supported: self.capability_supported,
            activated: self.ring.is_some(),
            inactive_reason: self.inactive_reason.clone(),
            submitted_imports: self.submitted_imports,
            samples: self.completed_samples,
            pending,
            missing: self.submitted_imports.saturating_sub(classified),
            dropped: self.dropped_samples,
        }
    }

    fn mark(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        probe: &mut NativeVideoImportGpuTimingProbe,
        marker: NativeVideoImportGpuTimestampMarker,
    ) {
        let NativeVideoImportGpuTimingProbeDisposition::Recording(token) = &probe.disposition
        else {
            return;
        };
        let Some(mut ring) = self.ring.take() else {
            probe.disposition = NativeVideoImportGpuTimingProbeDisposition::Missing;
            return;
        };
        if let Err(error) = ring.mark(encoder, *token, marker) {
            probe.disposition = NativeVideoImportGpuTimingProbeDisposition::Missing;
            self.disable_with_ring(ring, error);
            return;
        }
        self.ring = Some(ring);
    }

    fn accept_samples(&mut self, samples: Vec<NativeVideoImportGpuTimingSample>) {
        self.completed_samples = self.completed_samples.saturating_add(samples.len() as u64);
        self.completed.extend(samples);
    }

    fn account_candidate_terminal(
        &mut self,
        candidate_token: Option<NativeVideoImportCandidateToken>,
        disposition: NativeVideoImportCandidateSampleDisposition,
    ) {
        let Some(candidate_token) = candidate_token else {
            return;
        };
        let Some(active) = self.active_candidate.as_mut() else {
            self.disable(
                "native-import probe completed outside an active candidate scope".to_owned(),
            );
            return;
        };
        if active.token != candidate_token {
            self.active_candidate = None;
            self.disable("native-import probe completed for a foreign candidate scope".to_owned());
            return;
        }
        active.submitted_imports = active.submitted_imports.saturating_add(1);
        match disposition {
            NativeVideoImportCandidateSampleDisposition::Scheduled => {
                active.scheduled_samples = active.scheduled_samples.saturating_add(1);
            }
            NativeVideoImportCandidateSampleDisposition::Missing => {
                active.missing_samples = active.missing_samples.saturating_add(1);
            }
            NativeVideoImportCandidateSampleDisposition::Dropped => {
                active.dropped_samples = active.dropped_samples.saturating_add(1);
            }
        }
    }

    fn disable_with_ring(&mut self, _ring: NativeVideoImportGpuTimestampRing, reason: String) {
        self.disable(reason);
    }

    fn disable(&mut self, reason: String) {
        self.ring = None;
        if self.inactive_reason.is_none() {
            self.inactive_reason = Some(reason);
        }
    }
}

#[cfg(target_os = "windows")]
fn activated_capacity(
    capability_supported: bool,
    policy: NativeVideoImportGpuTimingPolicy,
) -> Result<usize, String> {
    match policy {
        NativeVideoImportGpuTimingPolicy::Disabled => {
            Err("native-import GPU timing is disabled by product policy".to_owned())
        }
        NativeVideoImportGpuTimingPolicy::Enabled { .. } if !capability_supported => Err(
            "native-import GPU timing requires TIMESTAMP_QUERY and TIMESTAMP_QUERY_INSIDE_ENCODERS"
                .to_owned(),
        ),
        NativeVideoImportGpuTimingPolicy::Enabled { capacity: 0 } => {
            Err("native-import GPU timing capacity must be greater than zero".to_owned())
        }
        NativeVideoImportGpuTimingPolicy::Enabled { capacity }
            if capacity > NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY =>
        {
            Err(format!(
                "native-import GPU timing capacity {capacity} exceeds hard limit {}",
                NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY
            ))
        }
        NativeVideoImportGpuTimingPolicy::Enabled { capacity } => Ok(capacity),
    }
}

#[cfg(target_os = "windows")]
fn allocate_token(next: &mut u64) -> Option<u64> {
    let token = *next;
    let successor = token.checked_add(1)?;
    *next = successor;
    Some(token)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(target_os = "windows")]
struct NativeVideoImportGpuTimestampToken {
    id: u64,
    slot: usize,
}

#[derive(Debug, Clone, Copy)]
#[cfg(target_os = "windows")]
struct NativeVideoImportGpuTimestampMetadata {
    candidate_token: NativeVideoImportCandidateToken,
    import_token: NativeVideoImportToken,
    decode_fence_ready_at_admission: Option<bool>,
}

#[cfg(target_os = "windows")]
enum NativeVideoImportGpuTimestampAdmission {
    Recording(NativeVideoImportGpuTimestampToken),
    Dropped,
}

#[derive(Debug, Clone, Copy)]
#[cfg(target_os = "windows")]
enum NativeVideoImportGpuTimestampMarker {
    AfterYuv,
    AfterInputColor,
}

#[cfg(target_os = "windows")]
impl NativeVideoImportGpuTimestampMarker {
    const fn query_index(self) -> u32 {
        match self {
            Self::AfterYuv => 1,
            Self::AfterInputColor => 2,
        }
    }
}

#[cfg(target_os = "windows")]
struct NativeVideoImportGpuTimestampRing {
    slots: Vec<NativeVideoImportGpuTimestampSlot>,
    completed: Vec<NativeVideoImportGpuTimingSample>,
    next_id: u64,
}

#[cfg(target_os = "windows")]
struct NativeVideoImportGpuTimestampSlot {
    timer: NativeVideoImportGpuTimestampTimer,
    state: NativeVideoImportGpuTimestampSlotState,
}

#[cfg(target_os = "windows")]
enum NativeVideoImportGpuTimestampSlotState {
    Free,
    Recording {
        token: NativeVideoImportGpuTimestampToken,
        next_query_index: u32,
    },
    Pending {
        token: NativeVideoImportGpuTimestampToken,
        metadata: NativeVideoImportGpuTimestampMetadata,
        receiver: Receiver<Result<(), wgpu::BufferAsyncError>>,
    },
}

#[cfg(target_os = "windows")]
impl NativeVideoImportGpuTimestampRing {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, capacity: usize) -> Self {
        debug_assert!(capacity > 0);
        debug_assert!(!gpu_timestamp_query_device_features(device.features()).is_empty());
        let mut slots = Vec::with_capacity(capacity);
        for slot in 0..capacity {
            slots.push(NativeVideoImportGpuTimestampSlot {
                timer: NativeVideoImportGpuTimestampTimer::new(device, queue, slot),
                state: NativeVideoImportGpuTimestampSlotState::Free,
            });
        }
        Self { slots, completed: Vec::new(), next_id: 1 }
    }

    fn begin(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<NativeVideoImportGpuTimestampAdmission, String> {
        let Some(slot) = self
            .slots
            .iter()
            .position(|slot| matches!(slot.state, NativeVideoImportGpuTimestampSlotState::Free))
        else {
            return Ok(NativeVideoImportGpuTimestampAdmission::Dropped);
        };
        let Some(next_id) = self.next_id.checked_add(1) else {
            return Err("native-import GPU timestamp query identity exhausted".to_owned());
        };
        let token = NativeVideoImportGpuTimestampToken { id: self.next_id, slot };
        self.next_id = next_id;
        self.slots[slot].timer.begin(encoder);
        self.slots[slot].state =
            NativeVideoImportGpuTimestampSlotState::Recording { token, next_query_index: 1 };
        Ok(NativeVideoImportGpuTimestampAdmission::Recording(token))
    }

    fn mark(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        token: NativeVideoImportGpuTimestampToken,
        marker: NativeVideoImportGpuTimestampMarker,
    ) -> Result<(), String> {
        let expected = marker.query_index();
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err("native-import GPU timestamp token addressed no slot".to_owned());
        };
        match &mut slot.state {
            NativeVideoImportGpuTimestampSlotState::Recording {
                token: active,
                next_query_index,
            } if *active == token && *next_query_index == expected => {
                slot.timer.mark(encoder, marker);
                *next_query_index = next_query_index.saturating_add(1);
                Ok(())
            }
            _ => Err(
                "native-import GPU timestamp stage marker was missing, stale, or out of order"
                    .to_owned(),
            ),
        }
    }

    fn finish(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        token: NativeVideoImportGpuTimestampToken,
    ) -> Result<(), String> {
        self.validate_recording_complete(token)?;
        self.slots[token.slot].timer.finish(encoder);
        Ok(())
    }

    fn abandon(&mut self, token: NativeVideoImportGpuTimestampToken) -> Result<(), String> {
        let Some(slot) = self.slots.get_mut(token.slot) else {
            return Err("native-import GPU timestamp token addressed no slot".to_owned());
        };
        if matches!(
            slot.state,
            NativeVideoImportGpuTimestampSlotState::Recording {
                token: active,
                ..
            } if active == token
        ) {
            slot.state = NativeVideoImportGpuTimestampSlotState::Free;
            Ok(())
        } else {
            Err("native-import GPU timestamp abandon received a stale token".to_owned())
        }
    }

    fn after_submit(
        &mut self,
        token: NativeVideoImportGpuTimestampToken,
        metadata: NativeVideoImportGpuTimestampMetadata,
    ) -> Result<(), String> {
        self.validate_recording_complete(token)?;
        let receiver = self.slots[token.slot].timer.map_async();
        self.slots[token.slot].state =
            NativeVideoImportGpuTimestampSlotState::Pending { token, metadata, receiver };
        Ok(())
    }

    fn collect_after_device_poll(
        &mut self,
    ) -> Result<Vec<NativeVideoImportGpuTimingSample>, String> {
        self.collect_ready()?;
        Ok(self.take_completed())
    }

    fn take_completed(&mut self) -> Vec<NativeVideoImportGpuTimingSample> {
        std::mem::take(&mut self.completed)
    }

    fn pending_count(&self) -> u64 {
        self.slots
            .iter()
            .filter(|slot| {
                matches!(
                    slot.state,
                    NativeVideoImportGpuTimestampSlotState::Pending { .. }
                )
            })
            .count() as u64
    }

    fn validate_recording_complete(
        &self,
        token: NativeVideoImportGpuTimestampToken,
    ) -> Result<(), String> {
        let Some(slot) = self.slots.get(token.slot) else {
            return Err("native-import GPU timestamp token addressed no slot".to_owned());
        };
        if matches!(
            slot.state,
            NativeVideoImportGpuTimestampSlotState::Recording {
                token: active,
                next_query_index,
            } if active == token && next_query_index == NATIVE_IMPORT_TIMESTAMP_COUNT
        ) {
            Ok(())
        } else {
            Err("native-import GPU timestamp frame was not fully recorded".to_owned())
        }
    }

    fn collect_ready(&mut self) -> Result<(), String> {
        for index in 0..self.slots.len() {
            let callback = match &self.slots[index].state {
                NativeVideoImportGpuTimestampSlotState::Pending { receiver, .. } => {
                    match receiver.try_recv() {
                        Ok(result) => Some(result),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => {
                            return Err(
                                "native-import GPU timestamp map callback was dropped".to_owned()
                            );
                        }
                    }
                }
                NativeVideoImportGpuTimestampSlotState::Free
                | NativeVideoImportGpuTimestampSlotState::Recording { .. } => None,
            };
            let Some(callback) = callback else {
                continue;
            };
            callback.map_err(|error| {
                format!("native-import GPU timestamp readback mapping failed: {error}")
            })?;
            let state = std::mem::replace(
                &mut self.slots[index].state,
                NativeVideoImportGpuTimestampSlotState::Free,
            );
            let NativeVideoImportGpuTimestampSlotState::Pending { token, metadata, .. } = state
            else {
                return Err("native-import GPU timestamp slot changed before collection".to_owned());
            };
            if token.slot != index {
                return Err(
                    "native-import GPU timestamp token no longer matched its slot".to_owned(),
                );
            }
            let (yuv_decode_marker_bracket_us, input_color_marker_bracket_us) =
                self.slots[index].timer.read_mapped()?;
            self.completed.push(NativeVideoImportGpuTimingSample {
                candidate_token: metadata.candidate_token,
                import_token: metadata.import_token,
                yuv_decode_marker_bracket_us,
                input_color_marker_bracket_us,
                decode_fence_ready_at_admission: metadata.decode_fence_ready_at_admission,
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
struct NativeVideoImportGpuTimestampTimer {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    timestamp_period_ns: f64,
}

#[cfg(target_os = "windows")]
impl NativeVideoImportGpuTimestampTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, slot: usize) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some(&format!(
                "mondrian.native-video.import-gpu-timestamps.slot-{slot}"
            )),
            ty: wgpu::QueryType::Timestamp,
            count: NATIVE_IMPORT_TIMESTAMP_COUNT,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!(
                "mondrian.native-video.import-gpu-timestamp-resolve.slot-{slot}"
            )),
            size: NATIVE_IMPORT_TIMESTAMP_READBACK_BYTES,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!(
                "mondrian.native-video.import-gpu-timestamp-readback.slot-{slot}"
            )),
            size: NATIVE_IMPORT_TIMESTAMP_READBACK_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            timestamp_period_ns: f64::from(queue.get_timestamp_period()),
        }
    }

    fn begin(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.query_set, 0);
    }

    fn mark(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        marker: NativeVideoImportGpuTimestampMarker,
    ) {
        encoder.write_timestamp(&self.query_set, marker.query_index());
    }

    fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(
            &self.query_set,
            0..NATIVE_IMPORT_TIMESTAMP_COUNT,
            &self.resolve_buffer,
            0,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            NATIVE_IMPORT_TIMESTAMP_READBACK_BYTES,
        );
    }

    fn map_async(&self) -> Receiver<Result<(), wgpu::BufferAsyncError>> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.readback_buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        receiver
    }

    fn read_mapped(&self) -> Result<(u64, u64), String> {
        let slice = self.readback_buffer.slice(..);
        let mapped = match slice.get_mapped_range() {
            Ok(mapped) => mapped,
            Err(error) => {
                self.readback_buffer.unmap();
                return Err(format!(
                    "native-import GPU timestamp mapped range failed: {error}"
                ));
            }
        };
        if mapped.len() < NATIVE_IMPORT_TIMESTAMP_READBACK_BYTES as usize {
            drop(mapped);
            self.readback_buffer.unmap();
            return Err(
                "native-import GPU timestamp readback contained fewer than three counters"
                    .to_owned(),
            );
        }
        let mut counters = [0_u64; NATIVE_IMPORT_TIMESTAMP_COUNT as usize];
        for (index, counter) in counters.iter_mut().enumerate() {
            let offset = index * wgpu::QUERY_SIZE as usize;
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(&mapped[offset..offset + 8]);
            *counter = u64::from_ne_bytes(bytes);
        }
        drop(mapped);
        self.readback_buffer.unmap();
        Ok((
            timestamp_elapsed_us(counters[0], counters[1], self.timestamp_period_ns)?,
            timestamp_elapsed_us(counters[1], counters[2], self.timestamp_period_ns)?,
        ))
    }
}

#[cfg(target_os = "windows")]
fn timestamp_elapsed_us(start: u64, end: u64, timestamp_period_ns: f64) -> Result<u64, String> {
    let ticks = end.checked_sub(start).ok_or_else(|| {
        format!("native-import GPU timestamp counter regressed from {start} to {end}")
    })?;
    let elapsed_us = ((ticks as f64 * timestamp_period_ns) / 1_000.0).ceil();
    Ok(elapsed_us.min(u64::MAX as f64) as u64)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    fn inactive_runtime() -> NativeVideoImportGpuTimingRuntime {
        NativeVideoImportGpuTimingRuntime {
            ring: None,
            capability_supported: true,
            inactive_reason: Some("test-inactive".to_owned()),
            active_candidate: None,
            next_candidate_id: 1,
            next_import_id: 1,
            submitted_imports: 0,
            completed_samples: 0,
            dropped_samples: 0,
            completed: Vec::new(),
        }
    }

    #[test]
    fn failed_lifecycle_accounting_moves_pending_work_to_missing() {
        let mut runtime = inactive_runtime();
        runtime.submitted_imports = 9;
        runtime.completed_samples = 3;
        runtime.dropped_samples = 2;

        let diagnostics = runtime.diagnostics();
        assert!(diagnostics.capability_supported);
        assert!(!diagnostics.activated);
        assert_eq!(diagnostics.samples, 3);
        assert_eq!(diagnostics.pending, 0);
        assert_eq!(diagnostics.missing, 4);
        assert_eq!(diagnostics.dropped, 2);
        assert_eq!(
            diagnostics.submitted_imports,
            diagnostics.samples + diagnostics.pending + diagnostics.missing + diagnostics.dropped
        );

        runtime.submitted_imports = 1;
        runtime.completed_samples = 2;
        let saturated = runtime.diagnostics();
        assert_eq!(saturated.missing, 0);
    }

    #[test]
    fn move_only_terminal_probe_accounts_one_missing_submission() {
        let mut runtime = inactive_runtime();
        let candidate_token = runtime.begin_candidate();
        assert!(candidate_token.is_none());
        let probe = NativeVideoImportGpuTimingProbe {
            candidate_token,
            import_token: Some(NativeVideoImportToken(19)),
            decode_fence_ready_at_admission: Some(true),
            disposition: NativeVideoImportGpuTimingProbeDisposition::Missing,
        };

        runtime.after_submit(probe);
        assert!(runtime.end_candidate(candidate_token, true).is_none());

        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.submitted_imports, 1);
        assert_eq!(diagnostics.missing, 1);
        assert_eq!(diagnostics.samples, 0);
        assert_eq!(diagnostics.pending, 0);
        assert_eq!(diagnostics.dropped, 0);
    }

    #[test]
    fn inactive_candidate_scope_never_fabricates_an_expected_sample_token() {
        let mut runtime = inactive_runtime();
        let candidate = runtime.begin_candidate();
        assert!(runtime.end_candidate(candidate, true).is_none());
        assert!(runtime.active_candidate.is_none());
        assert!(candidate.is_none());
        assert_eq!(runtime.next_candidate_id, 1);
    }

    #[test]
    fn overlapping_candidate_scope_fails_closed_instead_of_rebinding_imports() {
        let mut runtime = inactive_runtime();
        runtime.inactive_reason = None;
        runtime.active_candidate = Some(NativeVideoImportActiveCandidate::new(
            NativeVideoImportCandidateToken(41),
        ));

        assert!(runtime.begin_candidate().is_none());
        assert!(runtime.active_candidate.is_none());
        assert!(!runtime.diagnostics().activated);
        assert!(runtime
            .diagnostics()
            .inactive_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("overlapping candidate scope")));
    }

    #[test]
    fn zero_import_candidate_receipt_is_explicit_and_balanced() {
        let mut runtime = inactive_runtime();
        let token = NativeVideoImportCandidateToken(51);
        runtime.active_candidate = Some(NativeVideoImportActiveCandidate::new(token));

        let receipt = runtime
            .end_candidate(Some(token), true)
            .expect("activated successful scope receipt");

        assert_eq!(receipt.candidate_token(), token);
        assert_eq!(receipt.submitted_imports(), 0);
        assert_eq!(receipt.scheduled_samples(), 0);
        assert_eq!(receipt.missing_samples(), 0);
        assert_eq!(receipt.dropped_samples(), 0);
    }

    #[test]
    fn multi_import_receipt_accounts_terminal_missing_and_dropped_exactly_once() {
        let mut runtime = inactive_runtime();
        let token = NativeVideoImportCandidateToken(61);
        runtime.active_candidate = Some(NativeVideoImportActiveCandidate::new(token));

        for import_token in [71, 72] {
            runtime.after_submit(NativeVideoImportGpuTimingProbe {
                candidate_token: Some(token),
                import_token: Some(NativeVideoImportToken(import_token)),
                decode_fence_ready_at_admission: Some(true),
                disposition: NativeVideoImportGpuTimingProbeDisposition::Missing,
            });
        }
        runtime.after_submit(NativeVideoImportGpuTimingProbe {
            candidate_token: Some(token),
            import_token: Some(NativeVideoImportToken(73)),
            decode_fence_ready_at_admission: Some(true),
            disposition: NativeVideoImportGpuTimingProbeDisposition::Dropped,
        });

        let receipt =
            runtime.end_candidate(Some(token), true).expect("successful candidate receipt");
        assert_eq!(receipt.submitted_imports(), 3);
        assert_eq!(receipt.scheduled_samples(), 0);
        assert_eq!(receipt.missing_samples(), 2);
        assert_eq!(receipt.dropped_samples(), 1);
        assert_eq!(
            receipt.submitted_imports(),
            receipt.scheduled_samples() + receipt.missing_samples() + receipt.dropped_samples()
        );
    }

    #[test]
    fn failed_viewer_record_scope_emits_no_candidate_receipt() {
        let mut runtime = inactive_runtime();
        let token = NativeVideoImportCandidateToken(81);
        runtime.active_candidate = Some(NativeVideoImportActiveCandidate::new(token));
        runtime.after_submit(NativeVideoImportGpuTimingProbe {
            candidate_token: Some(token),
            import_token: Some(NativeVideoImportToken(91)),
            decode_fence_ready_at_admission: Some(false),
            disposition: NativeVideoImportGpuTimingProbeDisposition::Missing,
        });

        assert!(runtime.end_candidate(Some(token), false).is_none());
        assert!(runtime.active_candidate.is_none());
        assert_eq!(runtime.diagnostics().submitted_imports, 1);
    }

    #[test]
    fn timing_policy_is_disabled_by_default_and_never_activates_from_capability_alone() {
        assert_eq!(
            NativeVideoImportGpuTimingPolicy::default(),
            NativeVideoImportGpuTimingPolicy::Disabled
        );
        assert!(
            activated_capacity(true, NativeVideoImportGpuTimingPolicy::Disabled)
                .expect_err("capability must not activate disabled policy")
                .contains("disabled by product policy")
        );
        assert_eq!(
            activated_capacity(
                true,
                NativeVideoImportGpuTimingPolicy::Enabled { capacity: 16 }
            )
            .expect("explicit policy"),
            16
        );
        assert!(activated_capacity(
            false,
            NativeVideoImportGpuTimingPolicy::Enabled { capacity: 16 }
        )
        .expect_err("unsupported capability must remain inactive")
        .contains("TIMESTAMP_QUERY"));
        assert!(activated_capacity(
            true,
            NativeVideoImportGpuTimingPolicy::Enabled {
                capacity: NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY + 1,
            }
        )
        .expect_err("oversized policy must not allocate an unbounded ring")
        .contains("exceeds hard limit"));
    }

    #[test]
    fn run_local_tokens_fail_closed_instead_of_aliasing_at_exhaustion() {
        let mut next = u64::MAX - 1;
        assert_eq!(allocate_token(&mut next), Some(u64::MAX - 1));
        assert_eq!(next, u64::MAX);
        assert_eq!(allocate_token(&mut next), None);
        assert_eq!(next, u64::MAX);
    }

    #[test]
    fn timestamp_segments_reject_counter_regression() {
        assert_eq!(timestamp_elapsed_us(10, 10, 1.0).expect("zero interval"), 0);
        assert!(timestamp_elapsed_us(11, 10, 1.0)
            .expect_err("regression must fail")
            .contains("regressed"));
    }

    #[test]
    fn activated_runtime_reports_scheduled_samples_bounded_drop_and_failed_scope_tokens() {
        let instance = wgpu::Instance::default();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            eprintln!("skipping native-import timestamp test: no GPU adapter available");
            return;
        };
        let required = gpu_timestamp_query_device_features(adapter.features());
        if required.is_empty() {
            eprintln!("skipping native-import timestamp test: timestamp features unavailable");
            return;
        }
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("native-import-timestamp-test"),
                required_features: required,
                ..wgpu::DeviceDescriptor::default()
            }))
        else {
            eprintln!("skipping native-import timestamp test: device creation failed");
            return;
        };
        let mut runtime = NativeVideoImportGpuTimingRuntime::new(
            &device,
            &queue,
            NativeVideoImportGpuTimingPolicy::Enabled { capacity: 1 },
        );
        let candidate_a = runtime.begin_candidate().expect("candidate A");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("native-import-timestamp-test"),
        });
        let mut scheduled = runtime.begin_import(&mut encoder, Some(false));
        runtime.mark_after_yuv(&mut encoder, &mut scheduled);
        runtime.mark_after_input_color(&mut encoder, &mut scheduled);
        runtime.finish_recording(&mut encoder, &mut scheduled);
        queue.submit(std::iter::once(encoder.finish()));
        runtime.after_submit(scheduled);

        let mut competing = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("native-import-timestamp-competing-test"),
        });
        let dropped = runtime.begin_import(&mut competing, Some(true));
        assert!(matches!(
            &dropped.disposition,
            NativeVideoImportGpuTimingProbeDisposition::Dropped
        ));
        queue.submit(std::iter::once(competing.finish()));
        runtime.after_submit(dropped);
        runtime.after_submit(NativeVideoImportGpuTimingProbe {
            candidate_token: Some(candidate_a),
            import_token: Some(NativeVideoImportToken(47)),
            decode_fence_ready_at_admission: None,
            disposition: NativeVideoImportGpuTimingProbeDisposition::Missing,
        });

        let receipt_a = runtime
            .end_candidate(Some(candidate_a), true)
            .expect("activated candidate A receipt");
        assert_eq!(receipt_a.candidate_token(), candidate_a);
        assert_eq!(receipt_a.submitted_imports(), 3);
        assert_eq!(receipt_a.scheduled_samples(), 1);
        assert_eq!(receipt_a.missing_samples(), 1);
        assert_eq!(receipt_a.dropped_samples(), 1);

        let candidate_b = runtime.begin_candidate().expect("candidate B");
        let receipt_b = runtime
            .end_candidate(Some(candidate_b), true)
            .expect("activated candidate B receipt");
        assert_ne!(candidate_a, candidate_b);
        assert_eq!(candidate_a.get(), 1);
        assert_eq!(candidate_b.get(), 2);
        assert_eq!(receipt_b.submitted_imports(), 0);
        assert!(runtime.active_candidate.is_none());

        let before_poll = runtime.diagnostics();
        assert_eq!(before_poll.submitted_imports, 3);
        assert_eq!(before_poll.pending, 1);
        assert_eq!(before_poll.missing, 1);
        assert_eq!(before_poll.dropped, 1);

        device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .expect("test-only candidate A wait");
        runtime.collect_after_device_poll();
        let samples = runtime.take_completed();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].candidate_token, candidate_a);
        assert_eq!(samples[0].import_token.get(), 1);
        assert_eq!(samples[0].decode_fence_ready_at_admission, Some(false));

        let failed_candidate = runtime.begin_candidate().expect("failed candidate");
        let mut failed_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("native-import-timestamp-failed-candidate-test"),
        });
        let mut failed_sample = runtime.begin_import(&mut failed_encoder, Some(true));
        runtime.mark_after_yuv(&mut failed_encoder, &mut failed_sample);
        runtime.mark_after_input_color(&mut failed_encoder, &mut failed_sample);
        runtime.finish_recording(&mut failed_encoder, &mut failed_sample);
        queue.submit(std::iter::once(failed_encoder.finish()));
        runtime.after_submit(failed_sample);
        assert!(runtime.end_candidate(Some(failed_candidate), false).is_none());

        device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .expect("test-only failed candidate wait");
        runtime.collect_after_device_poll();
        let unmatched = runtime.take_completed();
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].candidate_token, failed_candidate);
        assert_ne!(unmatched[0].candidate_token, receipt_a.candidate_token());
    }
}
