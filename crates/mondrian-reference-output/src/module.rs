use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::{
    ReferenceOutputAdapter, ReferenceOutputAdapterError, ReferenceOutputAdapterEvent,
    ReferenceOutputAdapterSession, ReferenceOutputBundle, ReferenceOutputDeviceDescriptor,
    ReferenceOutputDeviceId, ReferenceOutputHardwareTime, ReferenceOutputOpenRequest,
    ReferenceOutputProviderEvidence, ReferenceOutputReferencePolicy,
};

/// Product-visible lifecycle of one Reference Output Module instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceOutputState {
    /// No device ownership or scheduled output.
    #[default]
    Disabled,
    /// Device is open and accumulating complete preroll.
    Priming,
    /// Scheduled playout is active.
    Running,
    /// Exact capability or external runtime condition prevents output.
    Blocked,
    /// Provider execution failed after admission.
    Failed,
    /// Session stopped cleanly and released ownership.
    Stopped,
}

/// Cumulative bounded-session evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputDiagnostics {
    /// Diagnostics schema version.
    pub schema_version: u32,
    /// Current lifecycle state.
    pub state: ReferenceOutputState,
    /// Provider/runtime evidence, when discovery has occurred.
    pub provider: Option<ReferenceOutputProviderEvidence>,
    /// Open stable device identity.
    pub device_id: Option<ReferenceOutputDeviceId>,
    /// Captured device/profile generation.
    pub device_generation: Option<u64>,
    /// Bundles accepted into the provider queue.
    pub scheduled_frames: u64,
    /// Bundles completed by the provider.
    pub completed_frames: u64,
    /// Provider late-frame callbacks.
    pub late_frames: u64,
    /// Provider dropped-frame callbacks.
    pub dropped_frames: u64,
    /// Provider flushed-frame callbacks.
    pub flushed_frames: u64,
    /// Queued frames explicitly aborted by stop, block, or failure.
    pub aborted_frames: u64,
    /// Current frames awaiting one terminal provider callback.
    pub outstanding_frames: u64,
    /// Total provider callback/status events consumed.
    pub callback_events: u64,
    /// Exact embedded-audio sample frames accepted with video.
    pub scheduled_audio_frames: u64,
    /// Canonical ancillary packets accepted atomically with video/audio.
    pub scheduled_ancillary_packets: u64,
    /// Complete ST 291 words accepted, including ADF/checksum overhead.
    pub scheduled_ancillary_words: u64,
    /// Completed frames whose actual ANC inventory digest matched the schedule.
    pub verified_ancillary_readbacks: u64,
    /// Highest simultaneous scheduled queue depth.
    pub scheduled_high_water: u32,
    /// External-reference lock-loss transitions after a positive lock.
    pub reference_lock_losses: u64,
    /// Completion callbacks carrying valid hardware-clock evidence.
    pub hardware_timestamp_callbacks: u64,
    /// Invalid, regressing, or rate-changing hardware timestamps.
    pub hardware_time_failures: u64,
    /// First valid provider hardware timestamp.
    pub first_hardware_time: Option<ReferenceOutputHardwareTime>,
    /// Latest valid provider hardware timestamp.
    pub last_hardware_time: Option<ReferenceOutputHardwareTime>,
    /// Largest adjacent hardware-clock gap in ticks.
    pub maximum_hardware_time_gap_ticks: u64,
    /// Latest continuous external reference status.
    pub reference_locked: Option<bool>,
    /// Most recent stable blocker/failure detail.
    pub last_error: Option<String>,
}

impl Default for ReferenceOutputDiagnostics {
    fn default() -> Self {
        Self {
            schema_version: 1,
            state: ReferenceOutputState::Disabled,
            provider: None,
            device_id: None,
            device_generation: None,
            scheduled_frames: 0,
            completed_frames: 0,
            late_frames: 0,
            dropped_frames: 0,
            flushed_frames: 0,
            aborted_frames: 0,
            outstanding_frames: 0,
            callback_events: 0,
            scheduled_audio_frames: 0,
            scheduled_ancillary_packets: 0,
            scheduled_ancillary_words: 0,
            verified_ancillary_readbacks: 0,
            scheduled_high_water: 0,
            reference_lock_losses: 0,
            hardware_timestamp_callbacks: 0,
            hardware_time_failures: 0,
            first_hardware_time: None,
            last_hardware_time: None,
            maximum_hardware_time_gap_ticks: 0,
            reference_locked: None,
            last_error: None,
        }
    }
}

/// Deep scheduler/lifecycle Module over one physical-provider Adapter.
pub struct ReferenceOutputModule<A> {
    adapter: A,
    session: Option<Box<dyn ReferenceOutputAdapterSession>>,
    request: Option<ReferenceOutputOpenRequest>,
    scheduled: VecDeque<ScheduledBundleEvidence>,
    next_frame_index: u64,
    diagnostics: ReferenceOutputDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledBundleEvidence {
    frame_index: u64,
    ancillary_sha256: [u8; 32],
}

impl<A> ReferenceOutputModule<A>
where
    A: ReferenceOutputAdapter,
{
    /// Construct an inactive Module. No hardware is acquired at startup.
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            session: None,
            request: None,
            scheduled: VecDeque::new(),
            next_frame_index: 0,
            diagnostics: ReferenceOutputDiagnostics::default(),
        }
    }

    /// Enumerate exact device modes without changing an active Session.
    pub fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputError> {
        match self.adapter.discover() {
            Ok(devices) => {
                self.diagnostics.provider = Some(self.adapter.evidence().clone());
                Ok(devices)
            }
            Err(error) => {
                self.record_blocked(&error);
                Err(error.into())
            }
        }
    }

    /// Open one exact device generation. Active Sessions must be stopped first
    /// so hardware ownership never changes under queued callbacks.
    pub fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Result<(), ReferenceOutputError> {
        if self.session.is_some() {
            return Err(ReferenceOutputError::AlreadyOpen);
        }
        request.validate()?;
        device.admit(&request)?;
        match self.adapter.open(device, &request) {
            Ok(session) => {
                self.diagnostics = ReferenceOutputDiagnostics {
                    state: ReferenceOutputState::Priming,
                    provider: Some(session.evidence().clone()),
                    device_id: Some(device.id.clone()),
                    device_generation: Some(session.device_generation()),
                    reference_locked: None,
                    ..ReferenceOutputDiagnostics::default()
                };
                self.request = Some(request);
                self.session = Some(session);
                self.next_frame_index = first_frame_index;
                self.scheduled.clear();
                Ok(())
            }
            Err(error) => {
                self.record_blocked(&error);
                Err(error.into())
            }
        }
    }

    /// Schedule one complete clean-feed bundle in exact frame order.
    pub fn schedule(&mut self, bundle: ReferenceOutputBundle) -> Result<(), ReferenceOutputError> {
        let request = self.request.as_ref().ok_or(ReferenceOutputError::NotOpen)?;
        if !matches!(
            self.diagnostics.state,
            ReferenceOutputState::Priming | ReferenceOutputState::Running
        ) {
            return Err(ReferenceOutputError::NotSchedulable { state: self.diagnostics.state });
        }
        bundle.validate()?;
        if bundle.video.signal() != &request.signal {
            return Err(ReferenceOutputError::PayloadSignalMismatch);
        }
        if bundle.frame_index() != self.next_frame_index {
            return Err(ReferenceOutputError::NonContiguousFrame {
                expected: self.next_frame_index,
                actual: bundle.frame_index(),
            });
        }
        if !request.ancillary_policy.is_required() && !bundle.ancillary.packets().is_empty() {
            return Err(ReferenceOutputError::AncillaryNotEnabled);
        }
        let audio_frames = bundle.audio.sample_frames() as u64;
        let ancillary_packets = bundle.ancillary.packets().len() as u64;
        let ancillary_sha256 = bundle.ancillary.sha256();
        let ancillary_words = bundle
            .ancillary
            .packets()
            .iter()
            .try_fold(0u64, |total, packet| {
                total.checked_add(packet.packet.encoded_word_count() as u64)
            })
            .ok_or(ReferenceOutputError::AncillaryWordCountOverflow)?;
        self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.schedule(bundle)?;
        self.scheduled.push_back(ScheduledBundleEvidence {
            frame_index: self.next_frame_index,
            ancillary_sha256,
        });
        self.next_frame_index = self
            .next_frame_index
            .checked_add(1)
            .ok_or(ReferenceOutputError::FrameIndexOverflow)?;
        self.diagnostics.scheduled_frames += 1;
        self.diagnostics.scheduled_audio_frames = self
            .diagnostics
            .scheduled_audio_frames
            .checked_add(audio_frames)
            .ok_or(ReferenceOutputError::AudioFrameCountOverflow)?;
        self.diagnostics.scheduled_ancillary_packets = self
            .diagnostics
            .scheduled_ancillary_packets
            .checked_add(ancillary_packets)
            .ok_or(ReferenceOutputError::AncillaryPacketCountOverflow)?;
        self.diagnostics.scheduled_ancillary_words = self
            .diagnostics
            .scheduled_ancillary_words
            .checked_add(ancillary_words)
            .ok_or(ReferenceOutputError::AncillaryWordCountOverflow)?;
        self.diagnostics.scheduled_high_water =
            self.diagnostics.scheduled_high_water.max(self.scheduled.len() as u32);
        self.diagnostics.outstanding_frames = self.scheduled.len() as u64;
        Ok(())
    }

    /// Start provider playback after complete bounded preroll.
    pub fn start(&mut self) -> Result<(), ReferenceOutputError> {
        if self.diagnostics.state != ReferenceOutputState::Priming {
            return Err(ReferenceOutputError::NotPriming { state: self.diagnostics.state });
        }
        if self.request.as_ref().is_some_and(|request| {
            request.reference_policy == ReferenceOutputReferencePolicy::RequireExternalLock
        }) && self.diagnostics.reference_locked != Some(true)
        {
            return Err(ReferenceOutputError::ExternalReferenceNotLocked);
        }
        self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.start()?;
        self.diagnostics.state = ReferenceOutputState::Running;
        Ok(())
    }

    /// Drain at most `limit` provider callbacks on the controlling thread.
    pub fn poll(&mut self, limit: usize) -> Result<usize, ReferenceOutputError> {
        let mut processed = 0;
        while processed < limit {
            let event = match self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.poll() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.record_failed(&error);
                    self.abort_outstanding()?;
                    return Err(error.into());
                }
            };
            processed += 1;
            self.handle_event(event)?;
            if matches!(
                self.diagnostics.state,
                ReferenceOutputState::Blocked | ReferenceOutputState::Failed
            ) {
                break;
            }
        }
        Ok(processed)
    }

    /// Stop and release the exact provider Session.
    pub fn stop(&mut self) -> Result<(), ReferenceOutputError> {
        if let Some(session) = self.session.as_mut()
            && let Err(error) = session.stop()
        {
            self.record_failed(&error);
            self.abort_outstanding()?;
            return Err(error.into());
        }
        self.abort_outstanding()?;
        self.session = None;
        self.request = None;
        self.diagnostics.state = ReferenceOutputState::Stopped;
        Ok(())
    }

    /// Current immutable diagnostic snapshot.
    pub const fn diagnostics(&self) -> &ReferenceOutputDiagnostics {
        &self.diagnostics
    }

    fn handle_event(
        &mut self,
        event: ReferenceOutputAdapterEvent,
    ) -> Result<(), ReferenceOutputError> {
        self.diagnostics.callback_events = self
            .diagnostics
            .callback_events
            .checked_add(1)
            .ok_or(ReferenceOutputError::CallbackCountOverflow)?;
        match event {
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index,
                ancillary_readback_sha256,
                hardware_time,
            } => {
                let expected = self.consume_scheduled(frame_index)?;
                if let Some(hardware_time) = hardware_time
                    && let Err(error) = self.record_hardware_time(hardware_time)
                {
                    self.abort_consumed_frame()?;
                    self.abort_outstanding()?;
                    return Err(error);
                }
                if self
                    .request
                    .as_ref()
                    .is_some_and(|request| request.ancillary_policy.requires_readback())
                {
                    let Some(actual) = ancillary_readback_sha256 else {
                        self.record_failed_detail(format!(
                            "provider omitted required ancillary readback for frame {frame_index}"
                        ));
                        self.abort_consumed_frame()?;
                        self.abort_outstanding()?;
                        return Err(ReferenceOutputError::AncillaryReadbackMissing { frame_index });
                    };
                    if actual != expected.ancillary_sha256 {
                        self.record_failed_detail(format!(
                            "provider ancillary readback differed for frame {frame_index}"
                        ));
                        self.abort_consumed_frame()?;
                        self.abort_outstanding()?;
                        return Err(ReferenceOutputError::AncillaryReadbackMismatch {
                            frame_index,
                        });
                    }
                    self.diagnostics.verified_ancillary_readbacks = self
                        .diagnostics
                        .verified_ancillary_readbacks
                        .checked_add(1)
                        .ok_or(ReferenceOutputError::AncillaryReadbackCountOverflow)?;
                }
                self.diagnostics.completed_frames += 1;
            }
            ReferenceOutputAdapterEvent::FrameLate { frame_index } => {
                self.consume_scheduled(frame_index)?;
                self.diagnostics.late_frames += 1;
            }
            ReferenceOutputAdapterEvent::FrameDropped { frame_index } => {
                self.consume_scheduled(frame_index)?;
                self.diagnostics.dropped_frames += 1;
            }
            ReferenceOutputAdapterEvent::FrameFlushed { frame_index } => {
                self.consume_scheduled(frame_index)?;
                self.diagnostics.flushed_frames += 1;
            }
            ReferenceOutputAdapterEvent::ReferenceLockChanged { locked } => {
                if !locked && self.diagnostics.reference_locked == Some(true) {
                    self.diagnostics.reference_lock_losses = self
                        .diagnostics
                        .reference_lock_losses
                        .checked_add(1)
                        .ok_or(ReferenceOutputError::ReferenceLockCountOverflow)?;
                }
                self.diagnostics.reference_locked = Some(locked);
                if !locked
                    && self.request.as_ref().is_some_and(|request| {
                        request.reference_policy
                            == ReferenceOutputReferencePolicy::RequireExternalLock
                    })
                {
                    self.block_active("required external reference lock was lost")?;
                }
            }
            ReferenceOutputAdapterEvent::DeviceLost => {
                self.block_active("reference output device was removed")?;
            }
            ReferenceOutputAdapterEvent::ProfileChanged => {
                self.block_active("reference output device profile changed")?;
            }
        }
        Ok(())
    }

    fn consume_scheduled(
        &mut self,
        actual: u64,
    ) -> Result<ScheduledBundleEvidence, ReferenceOutputError> {
        let expected = self
            .scheduled
            .front()
            .copied()
            .ok_or(ReferenceOutputError::UnexpectedCompletion { actual })?;
        if actual != expected.frame_index {
            self.record_failed_detail(format!(
                "out-of-order provider completion: expected {}, got {actual}",
                expected.frame_index
            ));
            self.abort_outstanding()?;
            return Err(ReferenceOutputError::OutOfOrderCompletion {
                expected: expected.frame_index,
                actual,
            });
        }
        self.scheduled.pop_front();
        self.diagnostics.outstanding_frames = self.scheduled.len() as u64;
        Ok(expected)
    }

    fn abort_consumed_frame(&mut self) -> Result<(), ReferenceOutputError> {
        self.diagnostics.aborted_frames = self
            .diagnostics
            .aborted_frames
            .checked_add(1)
            .ok_or(ReferenceOutputError::AbortedFrameCountOverflow)?;
        Ok(())
    }

    fn record_hardware_time(
        &mut self,
        current: ReferenceOutputHardwareTime,
    ) -> Result<(), ReferenceOutputError> {
        if current.ticks_per_second == 0 {
            self.diagnostics.hardware_time_failures =
                self.diagnostics.hardware_time_failures.saturating_add(1);
            self.record_failed_detail(
                "provider hardware timestamp has a zero tick rate".to_owned(),
            );
            return Err(ReferenceOutputError::InvalidHardwareTime);
        }
        if let Some(previous) = self.diagnostics.last_hardware_time {
            if current.ticks_per_second != previous.ticks_per_second
                || current.ticks <= previous.ticks
            {
                self.diagnostics.hardware_time_failures =
                    self.diagnostics.hardware_time_failures.saturating_add(1);
                self.record_failed_detail(
                    "provider hardware timestamp regressed or changed tick rate".to_owned(),
                );
                return Err(ReferenceOutputError::InvalidHardwareTime);
            }
            self.diagnostics.maximum_hardware_time_gap_ticks = self
                .diagnostics
                .maximum_hardware_time_gap_ticks
                .max(current.ticks - previous.ticks);
        } else {
            self.diagnostics.first_hardware_time = Some(current);
        }
        self.diagnostics.last_hardware_time = Some(current);
        self.diagnostics.hardware_timestamp_callbacks = self
            .diagnostics
            .hardware_timestamp_callbacks
            .checked_add(1)
            .ok_or(ReferenceOutputError::HardwareTimestampCountOverflow)?;
        Ok(())
    }

    fn abort_outstanding(&mut self) -> Result<(), ReferenceOutputError> {
        let outstanding = u64::try_from(self.scheduled.len())
            .map_err(|_| ReferenceOutputError::AbortedFrameCountOverflow)?;
        self.diagnostics.aborted_frames = self
            .diagnostics
            .aborted_frames
            .checked_add(outstanding)
            .ok_or(ReferenceOutputError::AbortedFrameCountOverflow)?;
        self.scheduled.clear();
        self.diagnostics.outstanding_frames = 0;
        Ok(())
    }

    fn block_active(&mut self, detail: &str) -> Result<(), ReferenceOutputError> {
        if let Some(session) = self.session.as_mut()
            && let Err(error) = session.stop()
        {
            self.record_failed(&error);
            self.abort_outstanding()?;
            return Err(error.into());
        }
        self.abort_outstanding()?;
        self.diagnostics.state = ReferenceOutputState::Blocked;
        self.diagnostics.last_error = Some(detail.to_owned());
        Ok(())
    }

    fn record_blocked(&mut self, error: &ReferenceOutputAdapterError) {
        self.diagnostics.provider = Some(self.adapter.evidence().clone());
        self.diagnostics.state = ReferenceOutputState::Blocked;
        self.diagnostics.last_error = Some(error.to_string());
    }

    fn record_failed(&mut self, error: &ReferenceOutputAdapterError) {
        self.record_failed_detail(error.to_string());
    }

    fn record_failed_detail(&mut self, detail: String) {
        self.diagnostics.state = ReferenceOutputState::Failed;
        self.diagnostics.last_error = Some(detail);
    }
}

/// Reference Output Module failure.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceOutputError {
    /// Provider/Session operation failed.
    #[error(transparent)]
    Adapter(#[from] ReferenceOutputAdapterError),
    /// Signal/mode request is invalid.
    #[error(transparent)]
    Mode(#[from] crate::ReferenceOutputModeError),
    /// Bundle payload is invalid.
    #[error(transparent)]
    Payload(#[from] crate::ReferenceOutputPayloadError),
    /// A Session already owns the device.
    #[error("reference output Session is already open; stop it before reconfiguration")]
    AlreadyOpen,
    /// Operation requires an open Session.
    #[error("reference output Session is not open")]
    NotOpen,
    /// Scheduling is disallowed in the current lifecycle state.
    #[error("reference output is not schedulable in state {state:?}")]
    NotSchedulable { state: ReferenceOutputState },
    /// Start requires Priming state.
    #[error("reference output cannot start in state {state:?}")]
    NotPriming { state: ReferenceOutputState },
    /// Required external reference must be positively observed before start.
    #[error("reference output requires proven external reference lock before start")]
    ExternalReferenceNotLocked,
    /// Bundle signal differs from the open request.
    #[error("reference output bundle signal differs from the open Session")]
    PayloadSignalMismatch,
    /// Frame schedule must be contiguous.
    #[error("reference output frame schedule expected {expected}, got {actual}")]
    NonContiguousFrame { expected: u64, actual: u64 },
    /// Frame coordinate overflowed.
    #[error("reference output frame index overflow")]
    FrameIndexOverflow,
    /// Audio accounting overflowed.
    #[error("reference output audio frame accounting overflow")]
    AudioFrameCountOverflow,
    /// A non-empty ANC inventory was supplied to a Session opened without ANC.
    #[error("reference output ancillary packets were not enabled for this Session")]
    AncillaryNotEnabled,
    /// ANC packet accounting overflowed.
    #[error("reference output ancillary packet accounting overflow")]
    AncillaryPacketCountOverflow,
    /// ANC word accounting overflowed.
    #[error("reference output ancillary word accounting overflow")]
    AncillaryWordCountOverflow,
    /// Required provider readback was absent for a completed frame.
    #[error("reference output provider omitted ancillary readback for frame {frame_index}")]
    AncillaryReadbackMissing { frame_index: u64 },
    /// Provider readback did not match the scheduled packet inventory.
    #[error("reference output provider ancillary readback mismatch for frame {frame_index}")]
    AncillaryReadbackMismatch { frame_index: u64 },
    /// Verified readback accounting overflowed.
    #[error("reference output ancillary readback accounting overflow")]
    AncillaryReadbackCountOverflow,
    /// Provider callback accounting overflowed.
    #[error("reference output callback accounting overflow")]
    CallbackCountOverflow,
    /// External-reference transition accounting overflowed.
    #[error("reference output reference-lock accounting overflow")]
    ReferenceLockCountOverflow,
    /// Hardware timestamp accounting overflowed.
    #[error("reference output hardware timestamp accounting overflow")]
    HardwareTimestampCountOverflow,
    /// Provider hardware timestamp was invalid or non-monotonic.
    #[error("reference output provider hardware timestamp is invalid")]
    InvalidHardwareTime,
    /// Aborted outstanding-frame accounting overflowed.
    #[error("reference output aborted-frame accounting overflow")]
    AbortedFrameCountOverflow,
    /// Provider completed a frame that was never scheduled.
    #[error("reference output provider completed unscheduled frame {actual}")]
    UnexpectedCompletion { actual: u64 },
    /// Provider callback order violated scheduled playout identity.
    #[error("reference output completion expected {expected}, got {actual}")]
    OutOfOrderCompletion { expected: u64, actual: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pack_encoded_rgb_to_v210, ReferenceAudioFrame, ReferenceOutputMode,
        ReferenceOutputPixelFormat, ReferenceOutputRange, ReferenceOutputScan,
        ReferenceOutputSignal, ReferenceVideoFrame, SimulatedReferenceOutputAdapter,
    };
    use mondrian_broadcast::{
        ActiveFormatDescription, AncillaryField, AncillaryOrigin, AncillaryPacket,
        AncillaryPlacement, AncillarySpace, AncillaryValidationLevel,
    };
    use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};

    fn request(reference_policy: ReferenceOutputReferencePolicy) -> ReferenceOutputOpenRequest {
        ReferenceOutputOpenRequest {
            signal: ReferenceOutputSignal {
                width: 6,
                height: 1,
                frame_rate: Rational::FPS_25,
                scan: ReferenceOutputScan::Progressive,
                pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
                color_space: ColorSpace::Rec709,
                range: ReferenceOutputRange::Legal,
                hdr: None,
                audio_layout: AudioChannelLayout::Stereo,
            },
            reference_policy,
            ancillary_policy: crate::ReferenceOutputAncillaryPolicy::Disabled,
            preroll_frames: 2,
            max_scheduled_frames: 3,
        }
    }

    fn bundle(request: &ReferenceOutputOpenRequest, frame_index: u64) -> ReferenceOutputBundle {
        let rgba = [[0.0, 0.0, 0.0, 1.0]; 6];
        let (row_bytes, bytes) = pack_encoded_rgb_to_v210(&request.signal, &rgba).expect("pack");
        let video = ReferenceVideoFrame::from_program_output(
            &request.signal,
            frame_index,
            row_bytes,
            bytes,
        )
        .expect("video");
        let sample_frames =
            request.signal.audio_frames_for_video_frame(frame_index).expect("audio cadence")
                as usize;
        let audio =
            ReferenceAudioFrame::new(&request.signal, frame_index, vec![0; sample_frames * 2])
                .expect("audio");
        ReferenceOutputBundle {
            video,
            audio,
            ancillary: crate::AncillaryFrame::empty(frame_index),
        }
    }

    fn module(
        request: &ReferenceOutputOpenRequest,
        events: impl IntoIterator<Item = ReferenceOutputAdapterEvent>,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_scripted_events(events);
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    fn module_with_stop_failure(
        request: &ReferenceOutputOpenRequest,
        events: impl IntoIterator<Item = ReferenceOutputAdapterEvent>,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_scripted_events(events)
            .with_stop_failure();
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    #[test]
    fn simulated_path_prerolls_and_accounts_exact_audio() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");
        assert_eq!(module.poll(8).expect("poll"), 2);
        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Running);
        assert_eq!(diagnostics.completed_frames, 2);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(
            diagnostics.scheduled_frames,
            diagnostics.completed_frames
                + diagnostics.late_frames
                + diagnostics.dropped_frames
                + diagnostics.flushed_frames
                + diagnostics.aborted_frames
                + diagnostics.outstanding_frames
        );
        assert_eq!(diagnostics.scheduled_audio_frames, 3_840);
        assert_eq!(diagnostics.scheduled_high_water, 2);
        assert!(!diagnostics.provider.as_ref().expect("evidence").hardware_backed);
    }

    #[test]
    fn required_reference_loss_stops_and_blocks() {
        let request = request(ReferenceOutputReferencePolicy::RequireExternalLock);
        let (mut module, device) = module(
            &request,
            [
                ReferenceOutputAdapterEvent::ReferenceLockChanged { locked: true },
                ReferenceOutputAdapterEvent::ReferenceLockChanged { locked: false },
            ],
        );
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.poll(1).expect("poll initial lock");
        module.start().expect("start");
        module.poll(1).expect("poll loss");
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Blocked);
        assert_eq!(module.diagnostics().reference_locked, Some(false));
        assert!(module
            .diagnostics()
            .last_error
            .as_deref()
            .is_some_and(|detail| detail.contains("reference lock")));
    }

    #[test]
    fn required_reference_cannot_start_without_positive_lock_evidence() {
        let request = request(ReferenceOutputReferencePolicy::RequireExternalLock);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");

        assert!(matches!(
            module.start(),
            Err(ReferenceOutputError::ExternalReferenceNotLocked)
        ));
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Priming);
        assert_eq!(module.diagnostics().reference_locked, None);
    }

    #[test]
    fn schedule_rejects_frame_gap_before_adapter_call() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 7).expect("open");
        let error = module.schedule(bundle(&request, 8)).expect_err("gap");
        assert!(matches!(
            error,
            ReferenceOutputError::NonContiguousFrame { expected: 7, actual: 8 }
        ));
    }

    #[test]
    fn ancillary_inventory_is_scheduled_atomically_and_accounted() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.ancillary_policy = crate::ReferenceOutputAncillaryPolicy::RequiredWithReadback;
        request.preroll_frames = 1;
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open ANC Session");
        let placement =
            AncillaryPlacement::new(AncillarySpace::Vanc, AncillaryField::Progressive, 9, 0)
                .expect("placement");
        let packet = ActiveFormatDescription::new(8, true, None)
            .expect("AFD")
            .packet()
            .expect("ST 291 packet");
        let expected_words = packet.encoded_word_count() as u64;
        let ancillary = crate::AncillaryFrame::new(
            0,
            vec![AncillaryPacket {
                placement,
                packet,
                origin: AncillaryOrigin::Derived,
                validation: AncillaryValidationLevel::Semantic,
            }],
        )
        .expect("ANC frame");
        let mut first = bundle(&request, 0);
        first.ancillary = ancillary;
        module.schedule(first).expect("atomic ANC bundle");
        module.start().expect("start ANC Session");
        assert_eq!(module.poll(1).expect("ANC completion"), 1);
        assert_eq!(module.diagnostics().scheduled_frames, 1);
        assert_eq!(module.diagnostics().completed_frames, 1);
        assert_eq!(module.diagnostics().scheduled_ancillary_packets, 1);
        assert_eq!(
            module.diagnostics().scheduled_ancillary_words,
            expected_words
        );
        assert_eq!(module.diagnostics().verified_ancillary_readbacks, 1);
    }

    #[test]
    fn required_ancillary_readback_fails_closed_when_provider_omits_digest() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.ancillary_policy = crate::ReferenceOutputAncillaryPolicy::RequiredWithReadback;
        request.preroll_frames = 1;
        let (mut module, device) = module(
            &request,
            [ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 0,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 123,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            }],
        );
        module.open(&device, request.clone(), 0).expect("open ANC Session");
        module.schedule(bundle(&request, 0)).expect("schedule frame");
        module.start().expect("start Session");
        assert!(matches!(
            module.poll(1),
            Err(ReferenceOutputError::AncillaryReadbackMissing { frame_index: 0 })
        ));
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Failed);
        assert_eq!(module.diagnostics().completed_frames, 0);
        assert_eq!(module.diagnostics().aborted_frames, 1);
        assert_eq!(module.diagnostics().outstanding_frames, 0);
    }

    #[test]
    fn hardware_time_is_typed_monotonic_and_bounded() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.preroll_frames = 2;
        let events = [
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 0,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 1_000,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 1,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 2_000,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
        ];
        let (mut module, device) = module(&request, events);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");
        assert_eq!(module.poll(2).expect("poll"), 2);

        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.hardware_timestamp_callbacks, 2);
        assert_eq!(diagnostics.hardware_time_failures, 0);
        assert_eq!(diagnostics.maximum_hardware_time_gap_ticks, 1_000);
        assert_eq!(
            diagnostics.last_hardware_time,
            Some(ReferenceOutputHardwareTime { ticks: 2_000, ticks_per_second: 25_000 })
        );
    }

    #[test]
    fn regressing_hardware_time_fails_and_closes_frame_accounting() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.preroll_frames = 2;
        let events = [
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 0,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 1_000,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 1,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 999,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
        ];
        let (mut module, device) = module(&request, events);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");
        assert!(matches!(
            module.poll(2),
            Err(ReferenceOutputError::InvalidHardwareTime)
        ));

        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Failed);
        assert_eq!(diagnostics.hardware_time_failures, 1);
        assert_eq!(diagnostics.completed_frames, 1);
        assert_eq!(diagnostics.aborted_frames, 1);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(diagnostics.scheduled_frames, 2);
    }

    #[test]
    fn out_of_order_callback_fails_and_aborts_the_entire_queue() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(
            &request,
            [ReferenceOutputAdapterEvent::FrameDropped { frame_index: 1 }],
        );
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");

        assert!(matches!(
            module.poll(1),
            Err(ReferenceOutputError::OutOfOrderCompletion { expected: 0, actual: 1 })
        ));
        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Failed);
        assert_eq!(diagnostics.aborted_frames, 2);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(
            diagnostics.scheduled_frames,
            diagnostics.completed_frames
                + diagnostics.late_frames
                + diagnostics.dropped_frames
                + diagnostics.flushed_frames
                + diagnostics.aborted_frames
                + diagnostics.outstanding_frames
        );
    }

    #[test]
    fn device_loss_stop_failure_still_fails_and_closes_accounting() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) =
            module_with_stop_failure(&request, [ReferenceOutputAdapterEvent::DeviceLost]);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");

        assert!(matches!(
            module.poll(1),
            Err(ReferenceOutputError::Adapter(
                ReferenceOutputAdapterError::Vendor { operation: "stop", .. }
            ))
        ));
        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Failed);
        assert_eq!(diagnostics.aborted_frames, 2);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(diagnostics.scheduled_frames, diagnostics.aborted_frames);
    }
}
