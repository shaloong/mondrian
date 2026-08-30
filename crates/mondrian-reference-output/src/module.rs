use std::collections::VecDeque;

use crate::{
    ReferenceOutputAdapter, ReferenceOutputAdapterError, ReferenceOutputAdapterEvent,
    ReferenceOutputAdapterSession, ReferenceOutputBundle, ReferenceOutputDeviceDescriptor,
    ReferenceOutputDeviceId, ReferenceOutputOpenRequest, ReferenceOutputProviderEvidence,
    ReferenceOutputReferencePolicy,
};

/// Product-visible lifecycle of one Reference Output Module instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceOutputDiagnostics {
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
    /// Exact embedded-audio sample frames accepted with video.
    pub scheduled_audio_frames: u64,
    /// Highest simultaneous scheduled queue depth.
    pub scheduled_high_water: u32,
    /// Latest continuous external reference status.
    pub reference_locked: Option<bool>,
    /// Most recent stable blocker/failure detail.
    pub last_error: Option<String>,
}

impl Default for ReferenceOutputDiagnostics {
    fn default() -> Self {
        Self {
            state: ReferenceOutputState::Disabled,
            provider: None,
            device_id: None,
            device_generation: None,
            scheduled_frames: 0,
            completed_frames: 0,
            late_frames: 0,
            dropped_frames: 0,
            flushed_frames: 0,
            scheduled_audio_frames: 0,
            scheduled_high_water: 0,
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
    scheduled: VecDeque<u64>,
    next_frame_index: u64,
    diagnostics: ReferenceOutputDiagnostics,
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
        let audio_frames = bundle.audio.sample_frames() as u64;
        self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.schedule(bundle)?;
        self.scheduled.push_back(self.next_frame_index);
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
        self.diagnostics.scheduled_high_water =
            self.diagnostics.scheduled_high_water.max(self.scheduled.len() as u32);
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
            return Err(error.into());
        }
        self.session = None;
        self.request = None;
        self.scheduled.clear();
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
        match event {
            ReferenceOutputAdapterEvent::FrameCompleted { frame_index, .. } => {
                self.consume_scheduled(frame_index)?;
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

    fn consume_scheduled(&mut self, actual: u64) -> Result<(), ReferenceOutputError> {
        let expected = self
            .scheduled
            .pop_front()
            .ok_or(ReferenceOutputError::UnexpectedCompletion { actual })?;
        if actual != expected {
            self.record_failed_detail(format!(
                "out-of-order provider completion: expected {expected}, got {actual}"
            ));
            return Err(ReferenceOutputError::OutOfOrderCompletion { expected, actual });
        }
        Ok(())
    }

    fn block_active(&mut self, detail: &str) -> Result<(), ReferenceOutputError> {
        if let Some(session) = self.session.as_mut() {
            session.stop()?;
        }
        self.scheduled.clear();
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
        ReferenceOutputBundle { video, audio }
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
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_scripted_events(events);
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
}
