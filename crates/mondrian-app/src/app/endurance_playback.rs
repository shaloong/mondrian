//! Persistent production Timeline picture/audio execution for endurance phases.

use std::time::{Duration, Instant};

use mondrian_core::{FramePosition, Rational, SequenceRevision};
use mondrian_platform::{EndurancePhaseKind, EndurancePhaseRequirement};
use mondrian_playback::ClockMaster;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_campaign::{
    EnduranceCachePressureObservation, EnduranceExecutionOwners,
    EnduranceRealtimeIntervalObservation,
};
use super::endurance_recovery::EnduranceRecoveryOperationReceipt;
use super::endurance_workload::PreparedEnduranceWorkload;
use super::execution_resource_coordination::{ExecutionResourcePressure, ResourceTrimRequest};
use super::product_action::{TimelineSeekPayload, TimelineSeekSource};
use super::AppState;

/// Persistent Timeline binding retained across every realtime observation window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PersistentTimelineBinding {
    sequence_id: mondrian_core::SequenceId,
    sequence_revision: SequenceRevision,
    author_generation: u64,
}

/// Owner-derived facts passed to the central recovery receipt sealer.
///
/// Fields stay private to this operation owner so sibling modules cannot
/// manufacture a successful seek from caller-authored primitive values.
pub(super) struct SeekRecoveryFacts {
    cycle_index: u32,
    operation_id: String,
    sequence_binding_sha256: String,
    from_frame: i64,
    target_frame: i64,
    before_epoch: u64,
    after_epoch: u64,
}

impl SeekRecoveryFacts {
    pub(super) const fn cycle_index(&self) -> u32 {
        self.cycle_index
    }

    pub(super) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(super) fn sequence_binding_sha256(&self) -> &str {
        &self.sequence_binding_sha256
    }

    pub(super) const fn source_frame(&self) -> i64 {
        self.from_frame
    }

    pub(super) const fn target_frame(&self) -> i64 {
        self.target_frame
    }

    pub(super) const fn before_epoch(&self) -> u64 {
        self.before_epoch
    }

    pub(super) const fn after_epoch(&self) -> u64 {
        self.after_epoch
    }
}

/// Owner-derived cache-pressure facts passed to the central receipt sealer.
pub(super) struct CachePressureRecoveryFacts {
    cycle_index: u32,
    operation_id: String,
    decision_generation_before: u64,
    pressure_decision_generation: u64,
    recovered_decision_generation: u64,
    cache_bytes_before_pressure: u64,
    cache_bytes_after_pressure: u64,
    pressure_trimmed_bytes: u64,
    residual_owned_resources: u64,
    exact_picture_ready: bool,
    gpu_device_losses_before: u64,
    gpu_device_losses_after: u64,
    fatal_errors_before: u64,
    fatal_errors_after: u64,
    export_failures_before: u64,
    export_failures_after: u64,
    pressure_decision_sha256: String,
    recovered_decision_sha256: String,
}

impl CachePressureRecoveryFacts {
    pub(super) const fn cycle_index(&self) -> u32 {
        self.cycle_index
    }

    pub(super) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(super) const fn decision_generation_before(&self) -> u64 {
        self.decision_generation_before
    }

    pub(super) const fn pressure_decision_generation(&self) -> u64 {
        self.pressure_decision_generation
    }

    pub(super) const fn recovered_decision_generation(&self) -> u64 {
        self.recovered_decision_generation
    }

    pub(super) const fn cache_bytes_before_pressure(&self) -> u64 {
        self.cache_bytes_before_pressure
    }

    pub(super) const fn cache_bytes_after_pressure(&self) -> u64 {
        self.cache_bytes_after_pressure
    }

    pub(super) const fn pressure_trimmed_bytes(&self) -> u64 {
        self.pressure_trimmed_bytes
    }

    pub(super) const fn residual_owned_resources(&self) -> u64 {
        self.residual_owned_resources
    }

    pub(super) const fn exact_picture_ready(&self) -> bool {
        self.exact_picture_ready
    }

    pub(super) const fn gpu_device_losses_before(&self) -> u64 {
        self.gpu_device_losses_before
    }

    pub(super) const fn gpu_device_losses_after(&self) -> u64 {
        self.gpu_device_losses_after
    }

    pub(super) const fn fatal_errors_before(&self) -> u64 {
        self.fatal_errors_before
    }

    pub(super) const fn fatal_errors_after(&self) -> u64 {
        self.fatal_errors_after
    }

    pub(super) const fn export_failures_before(&self) -> u64 {
        self.export_failures_before
    }

    pub(super) const fn export_failures_after(&self) -> u64 {
        self.export_failures_after
    }

    pub(super) fn pressure_decision_sha256(&self) -> &str {
        &self.pressure_decision_sha256
    }

    pub(super) fn recovered_decision_sha256(&self) -> &str {
        &self.recovered_decision_sha256
    }
}

/// Phase-scoped driver for the shared production Preview and physical Audio path.
#[must_use = "a persistent Timeline phase must be explicitly closed before its execution owners"]
pub struct PersistentTimelinePlaybackPhase {
    binding: PersistentTimelineBinding,
    expected_epoch: u64,
    expected_frame: i64,
    accepted_intervals: u64,
    interval_timeout: Duration,
    realtime_active: bool,
    closing: bool,
    fault: Option<String>,
}

impl PersistentTimelinePlaybackPhase {
    /// Validate one long-form fixture, start product Playback, and enter realtime residency.
    ///
    /// `app` and `owners` remain with the caller even when startup fails so the
    /// campaign runtime can execute its consuming cleanup contract exactly once.
    pub fn start(
        app: &mut AppState,
        owners: &mut EnduranceExecutionOwners,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        absolute_deadline: Option<Instant>,
        interval_timeout: Duration,
    ) -> Result<Self, PersistentTimelinePlaybackError> {
        if interval_timeout.is_zero() {
            return Err(PersistentTimelinePlaybackError::InvalidPlan(
                "GPU completion timeout must be nonzero".to_owned(),
            ));
        }
        validate_phase_contract(requirement, workload)?;
        if app.is_playing() || app.current_frame() != 0 {
            return Err(PersistentTimelinePlaybackError::InvalidPlan(
                "persistent Timeline playback requires a fresh stopped frame-zero transport"
                    .to_owned(),
            ));
        }
        let binding = capture_fixture_binding(app, requirement)?;
        app.play().map_err(|error| {
            PersistentTimelinePlaybackError::Startup(format!(
                "start production Timeline Playback: {error}"
            ))
        })?;
        owners.begin_realtime_window(app, absolute_deadline).map_err(|error| {
            PersistentTimelinePlaybackError::Startup(format!(
                "enter Headless realtime residency: {error}"
            ))
        })?;
        Ok(Self {
            binding,
            expected_epoch: app.playback_epoch().get(),
            expected_frame: app.current_frame(),
            accepted_intervals: 0,
            interval_timeout,
            realtime_active: true,
            closing: false,
            fault: None,
        })
    }

    /// Advance one exact production A/V interval through the shared coordinator.
    pub fn pump_interval(
        &mut self,
        app: &mut AppState,
        owners: &mut EnduranceExecutionOwners,
    ) -> Result<(), PersistentTimelinePlaybackError> {
        self.require_healthy_active()?;
        self.validate_binding(app)?;
        let observation = owners
            .run_realtime_interval(app, self.interval_timeout)
            .map_err(|error| self.latch_fault(error.to_string()))?;
        validate_interval_transition(
            self.expected_epoch,
            self.expected_frame,
            observation,
            app.playback_clock_master(),
        )
        .map_err(|detail| self.latch_fault(detail))?;
        self.validate_binding(app)?;
        self.expected_epoch = observation.after_epoch;
        self.expected_frame = observation.after_frame;
        self.accepted_intervals = self.accepted_intervals.checked_add(1).ok_or_else(|| {
            self.latch_fault("persistent Timeline interval counter overflow".to_owned())
        })?;
        Ok(())
    }

    /// Execute and prove one settled product Timeline seek without replacing
    /// the persistent Preview/GPU/Audio owners.
    pub fn recover_seek(
        &mut self,
        app: &mut AppState,
        owners: &mut EnduranceExecutionOwners,
        cycle_index: u32,
        target_frame: i64,
        absolute_deadline: Option<Instant>,
    ) -> Result<EnduranceRecoveryOperationReceipt, PersistentTimelinePlaybackError> {
        self.require_healthy_active()?;
        self.validate_binding(app)?;
        validate_expected_coordinate(self.expected_epoch, self.expected_frame, app)
            .map_err(|detail| self.latch_fault(detail))?;
        let last_content_frame = app
            .last_content_frame()
            .map_err(|error| self.latch_fault(format!("inspect seek recovery extent: {error}")))?;
        if target_frame < 0
            || target_frame == self.expected_frame
            || target_frame >= last_content_frame
        {
            return Err(self.latch_fault(format!(
                "seek recovery target {target_frame} must be non-negative, distinct from frame {}, and precede terminal guard frame {last_content_frame}",
                self.expected_frame
            )));
        }

        let from_frame = self.expected_frame;
        let before_epoch = self.expected_epoch;
        let evidence_before = app.playback_evidence_report();
        let target_position = {
            let sequence = app.active_sequence().ok_or_else(|| {
                self.latch_fault("seek recovery lost its active Sequence".to_owned())
            })?;
            FramePosition::new(target_frame, sequence.time_base())
        };

        self.settle_window(app, owners)?;
        app.seek_from_product_action(TimelineSeekPayload {
            position: target_position,
            source: TimelineSeekSource::Settled,
        })
        .map_err(|error| self.latch_fault(format!("execute product seek recovery: {error}")))?;
        self.validate_binding(app)?;

        let after_epoch = app.playback_epoch().get();
        if after_epoch <= before_epoch || app.current_frame() != target_frame || !app.is_playing() {
            return Err(self.latch_fault(
                "product seek recovery did not commit the exact target on a newer running epoch"
                    .to_owned(),
            ));
        }
        self.expected_epoch = after_epoch;
        self.expected_frame = target_frame;
        self.resume_window(app, owners, absolute_deadline)?;
        let sample = owners
            .complete_current_picture(app, self.interval_timeout)
            .map_err(|error| self.latch_fault(error.to_string()))?;
        if !sample.current_gpu_ready || sample.unavailable {
            return Err(self.latch_fault(
                "seek recovery did not prove its exact target picture Ready".to_owned(),
            ));
        }
        if app.playback_clock_master() != Some(ClockMaster::AudioDevice) {
            return Err(
                self.latch_fault("seek recovery was not governed by Audio Device Clock".to_owned())
            );
        }
        self.validate_binding(app)?;
        validate_expected_coordinate(after_epoch, target_frame, app)
            .map_err(|detail| self.latch_fault(detail))?;

        let evidence_after = app.playback_evidence_report();
        if evidence_before.accurate_seek_latency.count.checked_add(1)
            != Some(evidence_after.accurate_seek_latency.count)
            || evidence_after.latest_epoch != Some(after_epoch)
        {
            return Err(self.latch_fault(
                "seek recovery did not close exactly one accurate-seek evidence interval"
                    .to_owned(),
            ));
        }

        let facts = SeekRecoveryFacts {
            cycle_index,
            operation_id: format!("seek.c{cycle_index}.e{after_epoch}"),
            sequence_binding_sha256: sequence_binding_sha256(self.binding),
            from_frame,
            target_frame,
            before_epoch,
            after_epoch,
        };
        EnduranceRecoveryOperationReceipt::from_seek_facts(facts)
            .map_err(|error| self.latch_fault(format!("seal seek recovery receipt: {error}")))
    }

    /// Apply real bounded cache pressure, restore Nominal policy, and prove
    /// the same current picture remains exactly usable on the persistent owners.
    pub fn recover_cache_pressure(
        &mut self,
        app: &mut AppState,
        owners: &mut EnduranceExecutionOwners,
        cycle_index: u32,
        absolute_deadline: Option<Instant>,
    ) -> Result<EnduranceRecoveryOperationReceipt, PersistentTimelinePlaybackError> {
        self.require_healthy_active()?;
        self.validate_binding(app)?;
        validate_expected_coordinate(self.expected_epoch, self.expected_frame, app)
            .map_err(|detail| self.latch_fault(detail))?;
        self.settle_window(app, owners)?;
        let before = owners
            .capture_cache_pressure_observation(app)
            .map_err(|error| self.latch_fault(error.to_string()))?;
        let pressure_result = owners.apply_cache_pressure(app, ExecutionResourcePressure::Critical);
        let recovered_result = owners.apply_cache_pressure(app, ExecutionResourcePressure::Nominal);
        let (pressure, recovered) = match (pressure_result, recovered_result) {
            (Ok(pressure), Ok(recovered)) => (pressure, recovered),
            (Err(primary), Ok(_)) => {
                return Err(
                    self.latch_fault(format!("apply critical cache-pressure policy: {primary}"))
                );
            }
            (Ok(_), Err(cleanup)) => {
                return Err(
                    self.latch_fault(format!("restore Nominal cache-pressure policy: {cleanup}"))
                );
            }
            (Err(primary), Err(cleanup)) => {
                return Err(self.latch_fault(format!(
                    "apply critical cache-pressure policy: {primary}; restore Nominal policy: {cleanup}"
                )));
            }
        };
        validate_cache_pressure_policy_transition(&before, &pressure, &recovered)
            .map_err(|detail| self.latch_fault(detail))?;

        self.resume_window(app, owners, absolute_deadline)?;
        let sample = owners
            .complete_current_picture(app, self.interval_timeout)
            .map_err(|error| self.latch_fault(error.to_string()))?;
        if !sample.current_gpu_ready || sample.unavailable {
            return Err(self.latch_fault(
                "cache-pressure recovery did not prove the exact current picture Ready".to_owned(),
            ));
        }
        if app.playback_clock_master() != Some(ClockMaster::AudioDevice) {
            return Err(self.latch_fault(
                "cache-pressure recovery was not governed by Audio Device Clock".to_owned(),
            ));
        }
        self.validate_binding(app)?;
        validate_expected_coordinate(self.expected_epoch, self.expected_frame, app)
            .map_err(|detail| self.latch_fault(detail))?;

        self.settle_window(app, owners)?;
        let after_exact_picture = owners
            .capture_cache_pressure_observation(app)
            .map_err(|error| self.latch_fault(error.to_string()))?;
        validate_cache_pressure_terminal_health(&before, &recovered, &after_exact_picture)
            .map_err(|detail| self.latch_fault(detail))?;
        self.resume_window(app, owners, absolute_deadline)?;

        let pressure_trimmed_bytes = before
            .media_cache_bytes
            .checked_sub(pressure.media_cache_bytes)
            .ok_or_else(|| {
                self.latch_fault("cache-pressure byte accounting regressed".to_owned())
            })?;
        let residual_owned_resources = pressure
            .media_cache_entries
            .checked_add(pressure.media_cache_resource_units)
            .ok_or_else(|| {
                self.latch_fault("cache-pressure residual ownership overflowed".to_owned())
            })?;
        let facts = CachePressureRecoveryFacts {
            cycle_index,
            operation_id: format!(
                "cache.c{cycle_index}.d{}.r{}",
                pressure.decision_revision, recovered.decision_revision
            ),
            decision_generation_before: before.decision_revision,
            pressure_decision_generation: pressure.decision_revision,
            recovered_decision_generation: recovered.decision_revision,
            cache_bytes_before_pressure: before.media_cache_bytes,
            cache_bytes_after_pressure: pressure.media_cache_bytes,
            pressure_trimmed_bytes,
            residual_owned_resources,
            exact_picture_ready: true,
            gpu_device_losses_before: before.gpu_device_losses,
            gpu_device_losses_after: after_exact_picture.gpu_device_losses,
            fatal_errors_before: before.fatal_errors,
            fatal_errors_after: after_exact_picture.fatal_errors,
            export_failures_before: before.export_failures,
            export_failures_after: after_exact_picture.export_failures,
            pressure_decision_sha256: pressure.decision_sha256,
            recovered_decision_sha256: recovered.decision_sha256,
        };
        EnduranceRecoveryOperationReceipt::from_cache_pressure_facts(facts).map_err(|error| {
            self.latch_fault(format!("seal cache-pressure recovery receipt: {error}"))
        })
    }

    /// Leave native realtime scheduling at a cadence boundary without stopping Playback.
    pub fn settle_window(
        &mut self,
        app: &AppState,
        owners: &mut EnduranceExecutionOwners,
    ) -> Result<(), PersistentTimelinePlaybackError> {
        let finish_result = if self.realtime_active {
            let result = owners
                .finish_realtime_window()
                .map_err(|error| self.latch_fault(error.to_string()));
            if result.is_ok() {
                self.realtime_active = false;
            }
            result
        } else {
            Ok(())
        };
        let binding_result = self.validate_binding(app);
        finish_result?;
        binding_result?;
        if let Some(detail) = &self.fault {
            return Err(PersistentTimelinePlaybackError::Faulted(detail.clone()));
        }
        Ok(())
    }

    /// Re-enter native realtime scheduling after one settled owner snapshot.
    pub fn resume_window(
        &mut self,
        app: &AppState,
        owners: &mut EnduranceExecutionOwners,
        absolute_deadline: Option<Instant>,
    ) -> Result<(), PersistentTimelinePlaybackError> {
        if let Some(detail) = &self.fault {
            return Err(PersistentTimelinePlaybackError::Faulted(detail.clone()));
        }
        if self.closing {
            return Err(PersistentTimelinePlaybackError::Faulted(
                "persistent Timeline phase is closing".to_owned(),
            ));
        }
        if self.realtime_active {
            return Err(self.latch_fault(
                "persistent Timeline realtime residency is already active".to_owned(),
            ));
        }
        self.validate_binding(app)?;
        validate_expected_coordinate(self.expected_epoch, self.expected_frame, app)
            .map_err(|detail| self.latch_fault(detail))?;
        owners
            .begin_realtime_window(app, absolute_deadline)
            .map_err(|error| self.latch_fault(error.to_string()))?;
        self.realtime_active = true;
        Ok(())
    }

    /// Stop new realtime work, leave scheduling, and pause the product transport.
    pub fn begin_close(
        &mut self,
        app: &mut AppState,
        owners: &mut EnduranceExecutionOwners,
    ) -> Result<(), PersistentTimelinePlaybackError> {
        self.closing = true;
        let settle_result = self.settle_window(app, owners);
        let pause_result = if app.is_playing() {
            app.pause().map_err(|error| {
                self.latch_fault(format!("pause persistent Timeline Playback: {error}"))
            })
        } else {
            Ok(())
        };
        settle_result.and(pause_result)
    }

    /// Number of exact unit-frame intervals accepted by this phase owner.
    pub const fn accepted_intervals(&self) -> u64 {
        self.accepted_intervals
    }

    /// Whether native scheduling is inactive and no further work may be admitted.
    pub const fn is_closed(&self) -> bool {
        self.closing && !self.realtime_active
    }

    fn require_healthy_active(&self) -> Result<(), PersistentTimelinePlaybackError> {
        if let Some(detail) = &self.fault {
            return Err(PersistentTimelinePlaybackError::Faulted(detail.clone()));
        }
        if self.closing || !self.realtime_active {
            return Err(PersistentTimelinePlaybackError::Faulted(
                "persistent Timeline realtime residency is not active".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_binding(&mut self, app: &AppState) -> Result<(), PersistentTimelinePlaybackError> {
        let current = app.active_sequence().map(|sequence| PersistentTimelineBinding {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: app.project_author_generation(),
        });
        if current != Some(self.binding) {
            return Err(self.latch_fault(
                "persistent Timeline author binding changed during qualification".to_owned(),
            ));
        }
        Ok(())
    }

    fn latch_fault(&mut self, detail: String) -> PersistentTimelinePlaybackError {
        if self.fault.is_none() {
            self.fault = Some(detail.clone());
        }
        PersistentTimelinePlaybackError::Faulted(self.fault.as_ref().cloned().unwrap_or(detail))
    }
}

fn sequence_binding_sha256(binding: PersistentTimelineBinding) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.endurance.sequence-binding.v1\0");
    hasher.update(binding.sequence_id.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(binding.sequence_revision.get().to_le_bytes());
    hasher.update(binding.author_generation.to_le_bytes());
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Canonical binding digest shared with the real Window recovery owner.
#[cfg(feature = "validation")]
pub(crate) fn current_sequence_binding_sha256(app: &AppState) -> Option<String> {
    app.active_sequence()
        .map(|sequence| PersistentTimelineBinding {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: app.project_author_generation(),
        })
        .map(sequence_binding_sha256)
}

fn validate_cache_pressure_policy_transition(
    before: &EnduranceCachePressureObservation,
    pressure: &EnduranceCachePressureObservation,
    recovered: &EnduranceCachePressureObservation,
) -> Result<(), String> {
    if before.pressure != ExecutionResourcePressure::Nominal
        || pressure.pressure != ExecutionResourcePressure::Critical
        || pressure.trim != ResourceTrimRequest::Aggressive
        || recovered.pressure != ExecutionResourcePressure::Nominal
        || recovered.trim != ResourceTrimRequest::None
    {
        return Err(
            "cache-pressure recovery did not apply Critical then Nominal policy".to_owned(),
        );
    }
    if pressure.decision_revision <= before.decision_revision
        || recovered.decision_revision <= pressure.decision_revision
        || before.resource_decision_applications.checked_add(1)
            != Some(pressure.resource_decision_applications)
        || pressure.resource_decision_applications.checked_add(1)
            != Some(recovered.resource_decision_applications)
    {
        return Err(
            "cache-pressure decisions were not applied exactly once in monotonic order".to_owned(),
        );
    }
    if before.media_cache_bytes == 0
        || pressure.media_cache_bytes >= before.media_cache_bytes
        || pressure.media_cache_entries != 0
        || pressure.media_cache_resource_units != 0
    {
        return Err(
            "cache-pressure recovery did not trim nonzero optional media residency".to_owned(),
        );
    }
    Ok(())
}

fn validate_cache_pressure_terminal_health(
    before: &EnduranceCachePressureObservation,
    recovered: &EnduranceCachePressureObservation,
    after_exact_picture: &EnduranceCachePressureObservation,
) -> Result<(), String> {
    if after_exact_picture.pressure != ExecutionResourcePressure::Nominal
        || after_exact_picture.decision_revision != recovered.decision_revision
        || after_exact_picture.gpu_device_losses != before.gpu_device_losses
        || after_exact_picture.fatal_errors != before.fatal_errors
        || after_exact_picture.export_failures != before.export_failures
    {
        return Err(
            "cache-pressure recovery changed policy identity or cumulative failure evidence"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_phase_contract(
    requirement: &EndurancePhaseRequirement,
    workload: &PreparedEnduranceWorkload,
) -> Result<(), PersistentTimelinePlaybackError> {
    if requirement.phase_id != workload.phase_id() || requirement.kind != workload.kind() {
        return Err(PersistentTimelinePlaybackError::InvalidPlan(
            "prepared workload does not match the phase requirement".to_owned(),
        ));
    }
    if !matches!(
        requirement.kind,
        EndurancePhaseKind::PlaybackReference | EndurancePhaseKind::ConcurrentRecovery
    ) || requirement.counters.minimum_playback_presented_frames == 0
    {
        return Err(PersistentTimelinePlaybackError::InvalidPlan(
            "persistent Timeline playback requires a realtime qualification phase".to_owned(),
        ));
    }
    Ok(())
}

fn capture_fixture_binding(
    app: &AppState,
    requirement: &EndurancePhaseRequirement,
) -> Result<PersistentTimelineBinding, PersistentTimelinePlaybackError> {
    let sequence = app.active_sequence().ok_or_else(|| {
        PersistentTimelinePlaybackError::InvalidPlan(
            "persistent Timeline playback requires an active Sequence".to_owned(),
        )
    })?;
    validate_fixture_extent(
        sequence.settings.frame_rate,
        app.last_content_frame()
            .map_err(|error| PersistentTimelinePlaybackError::InvalidPlan(error.to_string()))?,
        requirement.counters.minimum_playback_presented_frames,
    )?;
    Ok(PersistentTimelineBinding {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: app.project_author_generation(),
    })
}

fn validate_fixture_extent(
    frame_rate: Rational,
    last_content_frame: i64,
    minimum_presented_frames: u64,
) -> Result<(), PersistentTimelinePlaybackError> {
    let required_rate = Rational::new(60, 1);
    if frame_rate != required_rate {
        return Err(PersistentTimelinePlaybackError::InvalidPlan(
            "persistent Timeline fixture must use the workload's exact 60/1 frame rate".to_owned(),
        ));
    }
    let available_frames = u64::try_from(last_content_frame)
        .ok()
        .and_then(|frame| frame.checked_add(1))
        .ok_or_else(|| {
            PersistentTimelinePlaybackError::InvalidPlan(
                "persistent Timeline fixture has no non-negative content extent".to_owned(),
            )
        })?;
    let required_frames = minimum_presented_frames.checked_add(1).ok_or_else(|| {
        PersistentTimelinePlaybackError::InvalidPlan(
            "persistent Timeline presentation requirement overflowed its guard frame".to_owned(),
        )
    })?;
    if available_frames < required_frames {
        return Err(PersistentTimelinePlaybackError::InvalidPlan(format!(
            "persistent Timeline fixture has {available_frames} frames, requires at least {required_frames} including the terminal guard frame"
        )));
    }
    Ok(())
}

fn validate_expected_coordinate(
    expected_epoch: u64,
    expected_frame: i64,
    app: &AppState,
) -> Result<(), String> {
    if app.playback_epoch().get() != expected_epoch || app.current_frame() != expected_frame {
        return Err("persistent Timeline coordinate changed outside the phase owner".to_owned());
    }
    if !app.is_playing() {
        return Err("persistent Timeline transport stopped outside the phase owner".to_owned());
    }
    Ok(())
}

fn validate_interval_transition(
    expected_epoch: u64,
    expected_frame: i64,
    observation: EnduranceRealtimeIntervalObservation,
    clock_master: Option<ClockMaster>,
) -> Result<(), String> {
    if observation.before_epoch != expected_epoch || observation.before_frame != expected_frame {
        return Err("persistent Timeline interval began from an unexpected coordinate".to_owned());
    }
    let expected_after_frame = expected_frame
        .checked_add(1)
        .ok_or_else(|| "persistent Timeline frame coordinate overflow".to_owned())?;
    if observation.after_epoch != expected_epoch || observation.after_frame != expected_after_frame
    {
        return Err("persistent Timeline interval did not advance exactly one frame".to_owned());
    }
    if !observation.sample.current_gpu_ready || observation.sample.unavailable {
        return Err(
            "persistent Timeline interval did not prove its exact picture ready".to_owned(),
        );
    }
    if clock_master != Some(ClockMaster::AudioDevice) {
        return Err(
            "persistent Timeline interval was not governed by Audio Device Clock".to_owned(),
        );
    }
    Ok(())
}

/// Stable persistent Timeline phase failure.
#[derive(Debug, Error)]
pub enum PersistentTimelinePlaybackError {
    /// Workload, fixture, transport, or duration admission was invalid.
    #[error("invalid persistent Timeline playback plan: {0}")]
    InvalidPlan(String),
    /// Product Playback or realtime scheduling could not start.
    #[error("persistent Timeline playback startup failed: {0}")]
    Startup(String),
    /// A started interval violated exact continuity or execution authority.
    #[error("persistent Timeline playback failed: {0}")]
    Faulted(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::headless_realtime_playback::HeadlessPreviewSample;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn observation(
        before_epoch: u64,
        before_frame: i64,
        after_epoch: u64,
        after_frame: i64,
        ready: bool,
    ) -> EnduranceRealtimeIntervalObservation {
        EnduranceRealtimeIntervalObservation {
            before_epoch,
            before_frame,
            after_epoch,
            after_frame,
            sample: HeadlessPreviewSample {
                current_gpu_ready: ready,
                stale_output_available: !ready,
                unavailable: false,
            },
        }
    }

    fn cache_observation(
        decision_revision: u64,
        pressure: ExecutionResourcePressure,
        trim: ResourceTrimRequest,
        applications: u64,
        media_cache_bytes: u64,
        media_cache_entries: u64,
        media_cache_resource_units: u64,
    ) -> EnduranceCachePressureObservation {
        EnduranceCachePressureObservation {
            decision_revision,
            pressure,
            trim,
            decision_sha256: SHA.to_owned(),
            resource_decision_applications: applications,
            media_cache_bytes,
            media_cache_entries,
            media_cache_resource_units,
            gpu_device_losses: 2,
            fatal_errors: 3,
            export_failures: 5,
        }
    }

    #[test]
    fn fixture_requires_exact_rate_complete_extent_and_terminal_guard_frame() {
        assert!(validate_fixture_extent(Rational::new(60, 1), 100, 100).is_ok());
        assert!(validate_fixture_extent(Rational::new(60, 1), 99, 100).is_err());
        assert!(validate_fixture_extent(Rational::new(30, 1), 100, 100).is_err());
        assert!(validate_fixture_extent(Rational::new(60, 1), -1, 1).is_err());
        assert!(validate_fixture_extent(Rational::new(60, 1), i64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn interval_accepts_only_exact_ready_audio_clocked_unit_progress() {
        assert!(validate_interval_transition(
            7,
            41,
            observation(7, 41, 7, 42, true),
            Some(ClockMaster::AudioDevice),
        )
        .is_ok());

        for invalid in [
            observation(8, 41, 8, 42, true),
            observation(7, 41, 7, 43, true),
            observation(7, 41, 8, 42, true),
            observation(7, 41, 7, 42, false),
        ] {
            assert!(
                validate_interval_transition(7, 41, invalid, Some(ClockMaster::AudioDevice),)
                    .is_err()
            );
        }
        assert!(validate_interval_transition(
            7,
            41,
            observation(7, 41, 7, 42, true),
            Some(ClockMaster::Synthetic),
        )
        .is_err());
    }

    #[test]
    fn cache_pressure_transition_requires_real_trim_exact_policy_order_and_clean_recovery() {
        let before = cache_observation(
            10,
            ExecutionResourcePressure::Nominal,
            ResourceTrimRequest::None,
            7,
            4096,
            2,
            1,
        );
        let pressure = cache_observation(
            11,
            ExecutionResourcePressure::Critical,
            ResourceTrimRequest::Aggressive,
            8,
            0,
            0,
            0,
        );
        let recovered = cache_observation(
            12,
            ExecutionResourcePressure::Nominal,
            ResourceTrimRequest::None,
            9,
            0,
            0,
            0,
        );
        let after_exact_picture = cache_observation(
            12,
            ExecutionResourcePressure::Nominal,
            ResourceTrimRequest::None,
            9,
            1024,
            1,
            0,
        );

        assert!(validate_cache_pressure_policy_transition(&before, &pressure, &recovered).is_ok());
        assert!(
            validate_cache_pressure_terminal_health(&before, &recovered, &after_exact_picture)
                .is_ok()
        );

        let mut no_trim = pressure.clone();
        no_trim.media_cache_bytes = before.media_cache_bytes;
        assert!(validate_cache_pressure_policy_transition(&before, &no_trim, &recovered).is_err());

        let mut skipped_application = recovered.clone();
        skipped_application.resource_decision_applications =
            pressure.resource_decision_applications;
        assert!(validate_cache_pressure_policy_transition(
            &before,
            &pressure,
            &skipped_application
        )
        .is_err());

        let mut failed = after_exact_picture;
        failed.fatal_errors += 1;
        assert!(validate_cache_pressure_terminal_health(&before, &recovered, &failed).is_err());
    }
}
