//! Concrete validation runtime for the three commercial endurance phases.
//!
//! Machine-specific composition stays behind [`FreshEndurancePhaseFactory`].
//! This Module owns the production phase objects, their exact pump order, the
//! four-step recovery cycle, and consuming terminal closure.

use std::collections::{BTreeSet, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_platform::{
    EndurancePhaseKind, EndurancePhaseRequirement, EndurancePhaseTerminalStatus,
    EnduranceRunManifest, ProcessMemoryProbe,
};
use mondrian_reference_output::{ReferenceOutputDeviceDescriptor, ReferenceOutputOpenRequest};

use super::endurance_campaign::{
    run_endurance_campaign, EnduranceCampaignClock, EnduranceCampaignError, EnduranceCampaignEvent,
    EnduranceCampaignRuntime, EnduranceExecutionOwners, EnduranceRuntimeClosure,
    EnduranceRuntimeSnapshot,
};
use super::endurance_export::{FrozenRepeatedExportPhase, FrozenRepeatedExportRequest};
use super::endurance_machine_plan::PreparedCommercialEnduranceMachinePlan;
use super::endurance_playback::PersistentTimelinePlaybackPhase;
use super::endurance_qualification::EnduranceCaptureFacts;
use super::endurance_recovery::EnduranceRecoveryOperationReceipt;
use super::endurance_reference_output::PersistentReferenceOutputPump;
use super::endurance_run_request::PreparedEnduranceRunRequest;
use super::endurance_workload::{
    EndurancePhaseAdmission, EndurancePreStartCapabilityInventory, PreparedEndurancePhaseStart,
    PreparedEnduranceWorkload,
};
use super::AppState;

/// Fixed latency bounds for one product endurance runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceProductRuntimeTimeouts {
    interval: Duration,
    recovery: Duration,
    surface_reopen: Duration,
    shutdown: Duration,
}

impl EnduranceProductRuntimeTimeouts {
    /// Validate non-renewing bounds for interval, recovery, Window, and close work.
    pub fn new(
        interval: Duration,
        recovery: Duration,
        surface_reopen: Duration,
        shutdown: Duration,
    ) -> Result<Self, EnduranceCampaignError> {
        if interval.is_zero()
            || recovery.is_zero()
            || surface_reopen.is_zero()
            || shutdown.is_zero()
        {
            return Err(runtime_error("endurance runtime timeouts must be nonzero"));
        }
        Ok(Self { interval, recovery, surface_reopen, shutdown })
    }
}

/// Exact Reference Output composition selected before a phase starts.
pub struct EnduranceReferenceOutputPlan {
    device: ReferenceOutputDeviceDescriptor,
    request: ReferenceOutputOpenRequest,
    first_frame_index: u64,
}

impl EnduranceReferenceOutputPlan {
    /// Bind one discovered device and exact signal request to the first frame.
    pub fn new(
        device: ReferenceOutputDeviceDescriptor,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Self {
        Self { device, request, first_frame_index }
    }
}

enum FreshEndurancePhaseInputs {
    PlaybackReference(Box<EnduranceReferenceOutputPlan>),
    ContinuousExport(Box<FrozenRepeatedExportRequest>),
    ConcurrentRecovery(Box<FreshConcurrentRecoveryInputs>),
}

struct FreshConcurrentRecoveryInputs {
    reference: EnduranceReferenceOutputPlan,
    export: FrozenRepeatedExportRequest,
    seek_targets: Vec<i64>,
}

/// Fresh App owner plus all machine-composed inputs for one exact phase.
pub struct FreshEndurancePhase {
    app_state: AppState,
    inputs: FreshEndurancePhaseInputs,
}

impl FreshEndurancePhase {
    /// Compose a fresh Playback + physical Reference phase.
    pub fn playback_reference(
        app_state: AppState,
        reference: EnduranceReferenceOutputPlan,
    ) -> Self {
        Self {
            app_state,
            inputs: FreshEndurancePhaseInputs::PlaybackReference(Box::new(reference)),
        }
    }

    /// Compose a fresh repeated Export phase.
    pub fn continuous_export(app_state: AppState, export: FrozenRepeatedExportRequest) -> Self {
        Self {
            app_state,
            inputs: FreshEndurancePhaseInputs::ContinuousExport(Box::new(export)),
        }
    }

    /// Compose a fresh overlapping Playback/Reference/Export recovery phase.
    pub fn concurrent_recovery(
        app_state: AppState,
        reference: EnduranceReferenceOutputPlan,
        export: FrozenRepeatedExportRequest,
        seek_targets: Vec<i64>,
    ) -> Self {
        Self {
            app_state,
            inputs: FreshEndurancePhaseInputs::ConcurrentRecovery(Box::new(
                FreshConcurrentRecoveryInputs { reference, export, seek_targets },
            )),
        }
    }

    fn kind(&self) -> EndurancePhaseKind {
        match self.inputs {
            FreshEndurancePhaseInputs::PlaybackReference(_) => {
                EndurancePhaseKind::PlaybackReference
            }
            FreshEndurancePhaseInputs::ContinuousExport(_) => EndurancePhaseKind::ContinuousExport,
            FreshEndurancePhaseInputs::ConcurrentRecovery(_) => {
                EndurancePhaseKind::ConcurrentRecovery
            }
        }
    }
}

/// Factory result that always returns an App owner, including setup failure.
pub enum FreshEndurancePhaseBuild {
    /// Every machine-specific input was prepared.
    Ready(Box<FreshEndurancePhase>),
    /// Setup failed after fresh App creation; the runtime must still consume it.
    Failed {
        /// Fresh owner retained for exactly-once shutdown.
        app_state: Box<AppState>,
        /// Stable operator-facing failure detail.
        detail: String,
    },
}

impl FreshEndurancePhaseBuild {
    /// Retain a fully composed fresh phase behind one bounded owner payload.
    pub fn ready(phase: FreshEndurancePhase) -> Self {
        Self::Ready(Box::new(phase))
    }

    /// Preserve a fresh App owner when machine-specific setup fails.
    pub fn failed(app_state: AppState, detail: impl Into<String>) -> Self {
        Self::Failed {
            app_state: Box::new(app_state),
            detail: detail.into(),
        }
    }
}

/// Machine composition seam for fresh phase owners.
///
/// Capability inventory is observed before `build_phase`; a `NotRun` result
/// therefore creates no App, worker, device, Queue, or child-process owner.
pub trait FreshEndurancePhaseFactory {
    /// Observe all exact prerequisites without starting phase owners.
    fn pre_start_capability_inventory(
        &mut self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
    ) -> Result<EndurancePreStartCapabilityInventory, EnduranceCampaignError>;

    /// Create one fresh App and machine composition after successful admission.
    ///
    /// The one-use token is constructed by the runtime and binds this call to
    /// the exact phase/workload whose complete pre-start inventory was checked.
    fn build_phase(
        &mut self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        prepared_start: PreparedEndurancePhaseStart,
    ) -> FreshEndurancePhaseBuild;
}

/// App-consuming Surface/Device reopen result.
pub struct EnduranceSurfaceReopenRun {
    /// Exact App owner returned by the Window event loop.
    pub app_state: AppState,
    /// Sealed product receipt or operation failure.
    pub result: Result<EnduranceRecoveryOperationReceipt, String>,
    /// Reference/Export owners returned after Window-scoped concurrent pumping.
    pub recovery_pump: EnduranceSurfaceRecoveryPump,
}

/// Phase owners that must continue while the main thread drives real Window recovery.
#[must_use = "the Surface recovery pump must be returned to its phase owner"]
pub struct EnduranceSurfaceRecoveryPump {
    reference: PersistentReferenceOutputPump,
    export: FrozenRepeatedExportPhase,
    events: Vec<EnduranceCampaignEvent>,
    phase_elapsed_at_entry_us: u64,
    entered_at: Instant,
    cadence_interval: Duration,
    next_pump_at: Instant,
    fault: Option<String>,
}

impl EnduranceSurfaceRecoveryPump {
    fn new(
        reference: PersistentReferenceOutputPump,
        export: FrozenRepeatedExportPhase,
        phase_elapsed_at_entry_us: u64,
    ) -> Result<
        Self,
        Box<(
            PersistentReferenceOutputPump,
            FrozenRepeatedExportPhase,
            String,
        )>,
    > {
        let cadence_interval = match reference.cadence_interval() {
            Ok(interval) => interval,
            Err(detail) => return Err(Box::new((reference, export, detail))),
        };
        let entered_at = Instant::now();
        Ok(Self {
            reference,
            export,
            events: Vec::new(),
            phase_elapsed_at_entry_us,
            entered_at,
            cadence_interval,
            next_pump_at: entered_at,
            fault: None,
        })
    }

    pub(crate) fn pump_window(&mut self, app: &mut AppState) -> Result<(), String> {
        if let Some(detail) = &self.fault {
            return Err(detail.clone());
        }
        let now = Instant::now();
        if now < self.next_pump_at {
            return Ok(());
        }
        let result: Result<(), String> = (|| {
            self.reference.pump_next(app).map_err(|error| error.to_string())?;
            let elapsed = u64::try_from(now.saturating_duration_since(self.entered_at).as_micros())
                .unwrap_or(u64::MAX);
            let completed_at_us = self
                .phase_elapsed_at_entry_us
                .checked_add(elapsed)
                .ok_or("Surface recovery event time overflowed")?;
            self.events
                .extend(self.export.poll(completed_at_us).map_err(|error| error.to_string())?);
            self.next_pump_at = now
                .checked_add(self.cadence_interval)
                .ok_or("Surface recovery pump deadline overflowed")?;
            Ok(())
        })();
        if let Err(detail) = &result {
            self.fault = Some(detail.clone());
        }
        result
    }

    pub(crate) const fn next_pump_at(&self) -> Instant {
        self.next_pump_at
    }

    fn into_parts(
        self,
    ) -> (
        PersistentReferenceOutputPump,
        FrozenRepeatedExportPhase,
        Vec<EnduranceCampaignEvent>,
        Option<String>,
    ) {
        (self.reference, self.export, self.events, self.fault)
    }
}

/// Main-thread driver for the real Window Surface/Device generation owner.
pub trait EnduranceSurfaceReopenDriver {
    /// Consume and return the same App owner around one real reopen operation.
    fn reopen(
        &mut self,
        app_state: AppState,
        recovery_pump: EnduranceSurfaceRecoveryPump,
        cycle_index: u32,
        operation_id: String,
        timeout: Duration,
    ) -> EnduranceSurfaceReopenRun;
}

/// Production winit Window driver. It is intentionally not `Send` and owns
/// exactly one process-local event loop across every recovery cycle.
pub struct WindowEnduranceSurfaceReopenDriver {
    event_loop: crate::app_ui::window::AppUiReusableEventLoop,
    _main_thread: PhantomData<Rc<()>>,
}

impl WindowEnduranceSurfaceReopenDriver {
    /// Create the process-local event loop before campaign admission.
    pub fn new() -> Result<Self, winit::error::EventLoopError> {
        Ok(Self {
            event_loop: crate::app_ui::window::AppUiReusableEventLoop::new()?,
            _main_thread: PhantomData,
        })
    }
}

impl EnduranceSurfaceReopenDriver for WindowEnduranceSurfaceReopenDriver {
    fn reopen(
        &mut self,
        app_state: AppState,
        recovery_pump: EnduranceSurfaceRecoveryPump,
        cycle_index: u32,
        operation_id: String,
        timeout: Duration,
    ) -> EnduranceSurfaceReopenRun {
        let run = self.event_loop.reopen_surface_device_with_pump(
            app_state,
            recovery_pump,
            cycle_index,
            operation_id,
            timeout,
        );
        EnduranceSurfaceReopenRun {
            app_state: run.app_state,
            result: run.result,
            recovery_pump: run.recovery_pump.expect("Window driver supplied a recovery pump"),
        }
    }
}

enum RuntimeState {
    Empty,
    Owned(Box<PhaseOwners>),
    Terminal(Option<Box<EnduranceRuntimeSnapshot>>),
}

struct PhaseOwners {
    kind: EndurancePhaseKind,
    app: Option<AppState>,
    execution: Option<EnduranceExecutionOwners>,
    timeline: Option<PersistentTimelinePlaybackPhase>,
    reference: Option<PersistentReferenceOutputPump>,
    export: Option<FrozenRepeatedExportPhase>,
    reference_plan: Option<EnduranceReferenceOutputPlan>,
    export_request: Option<FrozenRepeatedExportRequest>,
    seek_targets: VecDeque<i64>,
    phase_started_us: u64,
    minimum_duration_us: u64,
    recovery_cycle_count: u32,
    next_recovery_cycle: u32,
    settled: bool,
    fault: Option<String>,
}

impl PhaseOwners {
    fn from_fresh(
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        phase_started_us: u64,
        fresh: FreshEndurancePhase,
    ) -> Result<Box<Self>, (Box<Self>, String)> {
        let actual = fresh.kind();
        let (reference_plan, export_request, seek_targets) = match fresh.inputs {
            FreshEndurancePhaseInputs::PlaybackReference(reference) => {
                (Some(*reference), None, VecDeque::new())
            }
            FreshEndurancePhaseInputs::ContinuousExport(export) => {
                (None, Some(*export), VecDeque::new())
            }
            FreshEndurancePhaseInputs::ConcurrentRecovery(inputs) => {
                let FreshConcurrentRecoveryInputs { reference, export, seek_targets } = *inputs;
                (Some(reference), Some(export), seek_targets.into())
            }
        };
        let owners = Self {
            kind: requirement.kind,
            app: Some(fresh.app_state),
            execution: None,
            timeline: None,
            reference: None,
            export: None,
            reference_plan,
            export_request,
            seek_targets,
            phase_started_us,
            minimum_duration_us: requirement.minimum_duration_us,
            recovery_cycle_count: workload.recovery_cycle_count(),
            next_recovery_cycle: 0,
            settled: false,
            fault: None,
        };
        if actual != requirement.kind {
            return Err((
                Box::new(owners),
                format!(
                    "fresh endurance phase kind mismatch: expected {:?}, got {:?}",
                    requirement.kind, actual
                ),
            ));
        }
        if actual == EndurancePhaseKind::ConcurrentRecovery
            && owners.seek_targets.len()
                != usize::try_from(workload.recovery_cycle_count()).unwrap_or(usize::MAX)
        {
            return Err((
                Box::new(owners),
                "concurrent recovery requires exactly one factory-bound seek target per cycle"
                    .to_owned(),
            ));
        }
        Ok(Box::new(owners))
    }

    fn failed_before_start(
        kind: EndurancePhaseKind,
        app_state: AppState,
        phase_started_us: u64,
        minimum_duration_us: u64,
        recovery_cycle_count: u32,
        detail: String,
    ) -> Self {
        Self {
            kind,
            app: Some(app_state),
            execution: None,
            timeline: None,
            reference: None,
            export: None,
            reference_plan: None,
            export_request: None,
            seek_targets: VecDeque::new(),
            phase_started_us,
            minimum_duration_us,
            recovery_cycle_count,
            next_recovery_cycle: 0,
            settled: false,
            fault: Some(detail),
        }
    }

    fn start(
        &mut self,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        timeouts: EnduranceProductRuntimeTimeouts,
    ) -> Result<(), String> {
        if self.kind == EndurancePhaseKind::ConcurrentRecovery {
            let last_content_frame = self
                .app_ref()?
                .last_content_frame()
                .map_err(|error| format!("inspect recovery fixture extent: {error}"))?;
            let unique_targets = self.seek_targets.iter().copied().collect::<BTreeSet<_>>();
            if unique_targets.len() != self.seek_targets.len()
                || self
                    .seek_targets
                    .iter()
                    .any(|target| *target <= 0 || *target >= last_content_frame)
            {
                return Err(
                    "recovery seek targets must be unique, positive, and precede the terminal guard frame"
                        .to_owned(),
                );
            }
        }
        match self.kind {
            EndurancePhaseKind::PlaybackReference | EndurancePhaseKind::ConcurrentRecovery => {
                let execution =
                    EnduranceExecutionOwners::start().map_err(|error| error.to_string())?;
                self.execution = Some(execution);
                let app = self.app.as_mut().ok_or("phase App owner is missing")?;
                let execution = self.execution.as_mut().ok_or("execution owner disappeared")?;
                let timeline = PersistentTimelinePlaybackPhase::start(
                    app,
                    execution,
                    requirement,
                    workload,
                    None,
                    timeouts.interval,
                )
                .map_err(|error| error.to_string())?;
                self.timeline = Some(timeline);

                let plan = self
                    .reference_plan
                    .take()
                    .ok_or("realtime endurance phase is missing Reference Output composition")?;
                let reference = PersistentReferenceOutputPump::prepare(
                    self.app_ref()?,
                    plan.request,
                    plan.first_frame_index,
                )
                .map_err(|error| error.to_string())?;
                self.reference = Some(reference);
                let app = self.app.as_mut().ok_or("phase App owner is missing")?;
                let _physical_start = self
                    .reference
                    .as_mut()
                    .ok_or("Reference owner disappeared")?
                    .open_preroll_and_start(app, &plan.device)
                    .map_err(|error| error.to_string())?;
            }
            EndurancePhaseKind::ContinuousExport => {}
        }

        if matches!(
            self.kind,
            EndurancePhaseKind::ContinuousExport | EndurancePhaseKind::ConcurrentRecovery
        ) {
            let request = self
                .export_request
                .take()
                .ok_or("Export endurance phase is missing frozen Export composition")?;
            let export = FrozenRepeatedExportPhase::start(self.app_ref()?, request)
                .map_err(|error| error.to_string())?;
            self.export = Some(export);
        }
        Ok(())
    }

    fn app_ref(&self) -> Result<&AppState, String> {
        self.app.as_ref().ok_or_else(|| "phase App owner is missing".to_owned())
    }

    fn recovery_due(&self, deadline_run_us: u64) -> Result<bool, String> {
        if self.kind != EndurancePhaseKind::ConcurrentRecovery
            || self.next_recovery_cycle >= self.recovery_cycle_count
        {
            return Ok(false);
        }
        let elapsed = deadline_run_us
            .checked_sub(self.phase_started_us)
            .ok_or_else(|| "campaign deadline preceded phase start".to_owned())?;
        let numerator = u128::from(self.minimum_duration_us)
            .checked_mul(u128::from(self.next_recovery_cycle) + 1)
            .ok_or_else(|| "recovery cadence overflowed".to_owned())?;
        let denominator = u128::from(self.recovery_cycle_count) + 1;
        let threshold = numerator / denominator;
        Ok(u128::from(elapsed) >= threshold)
    }

    fn phase_elapsed_us(&self, clock: &dyn EnduranceCampaignClock) -> Result<u64, String> {
        clock
            .elapsed_us()
            .checked_sub(self.phase_started_us)
            .ok_or_else(|| "runtime clock preceded the phase origin".to_owned())
    }

    fn resume_if_settled(&mut self, deadline: Instant) -> Result<(), String> {
        if !self.settled || self.timeline.is_none() {
            return Ok(());
        }
        let app = self.app.as_ref().ok_or("phase App owner is missing")?;
        let execution = self.execution.as_mut().ok_or("execution owner is missing")?;
        self.timeline
            .as_mut()
            .ok_or("Timeline owner is missing")?
            .resume_window(app, execution, Some(deadline))
            .map_err(|error| error.to_string())?;
        self.settled = false;
        Ok(())
    }

    fn pump_one(&mut self, completed_at_us: u64) -> Result<Vec<EnduranceCampaignEvent>, String> {
        if self.timeline.is_some() {
            let app = self.app.as_mut().ok_or("phase App owner is missing")?;
            let execution = self.execution.as_mut().ok_or("execution owner is missing")?;
            self.timeline
                .as_mut()
                .ok_or("Timeline owner is missing")?
                .pump_interval(app, execution)
                .map_err(|error| error.to_string())?;
        }
        if self.reference.is_some() {
            let app = self.app.as_mut().ok_or("phase App owner is missing")?;
            self.reference
                .as_mut()
                .ok_or("Reference owner is missing")?
                .pump_next(app)
                .map_err(|error| error.to_string())?;
        }
        if let Some(export) = self.export.as_mut() {
            return export.poll(completed_at_us).map_err(|error| error.to_string());
        }
        Ok(Vec::new())
    }

    fn settle(&mut self) -> Result<(), String> {
        if self.timeline.is_none() || self.settled {
            return Ok(());
        }
        let app = self.app.as_ref().ok_or("phase App owner is missing")?;
        let execution = self.execution.as_mut().ok_or("execution owner is missing")?;
        self.timeline
            .as_mut()
            .ok_or("Timeline owner is missing")?
            .settle_window(app, execution)
            .map_err(|error| error.to_string())?;
        self.settled = true;
        Ok(())
    }

    fn execute_recovery_cycle<S: EnduranceSurfaceReopenDriver>(
        &mut self,
        surface: &mut S,
        clock: &dyn EnduranceCampaignClock,
        timeouts: EnduranceProductRuntimeTimeouts,
    ) -> Result<Vec<EnduranceCampaignEvent>, String> {
        let cycle_index = self.next_recovery_cycle;
        let target = self
            .seek_targets
            .pop_front()
            .ok_or("recovery cycle lost its factory-bound seek target")?;
        let deadline = Instant::now()
            .checked_add(timeouts.recovery)
            .ok_or("recovery operation deadline overflow")?;
        let mut events = Vec::new();

        {
            let app = self.app.as_mut().ok_or("phase App owner is missing")?;
            let execution = self.execution.as_mut().ok_or("execution owner is missing")?;
            let receipt = self
                .timeline
                .as_mut()
                .ok_or("Timeline owner is missing")?
                .recover_seek(app, execution, cycle_index, target, Some(deadline))
                .map_err(|error| error.to_string())?;
            events.push(EnduranceCampaignEvent::recovery_step_completed(
                self.phase_elapsed_us(clock)?,
                &receipt,
            ));
        }

        self.settle()?;
        let reference =
            self.reference.take().ok_or("Concurrent Recovery lost its Reference owner")?;
        let export = self.export.take().ok_or("Concurrent Recovery lost its Export owner")?;
        let phase_elapsed_at_entry_us = self.phase_elapsed_us(clock)?;
        let recovery_pump =
            match EnduranceSurfaceRecoveryPump::new(reference, export, phase_elapsed_at_entry_us) {
                Ok(pump) => pump,
                Err(error) => {
                    let (reference, export, detail) = *error;
                    self.reference = Some(reference);
                    self.export = Some(export);
                    return Err(detail);
                }
            };
        let app_state = self.app.take().ok_or("phase App owner is missing")?;
        let surface_timeout =
            timeouts.surface_reopen.min(deadline.saturating_duration_since(Instant::now()));
        if surface_timeout.is_zero() {
            self.app = Some(app_state);
            let (reference, export, _, pump_fault) = recovery_pump.into_parts();
            self.reference = Some(reference);
            self.export = Some(export);
            if let Some(detail) = pump_fault {
                return Err(detail);
            }
            return Err("recovery deadline expired before Surface reopen".to_owned());
        }
        let run = surface.reopen(
            app_state,
            recovery_pump,
            cycle_index,
            format!("surface.c{cycle_index}"),
            surface_timeout,
        );
        self.app = Some(run.app_state);
        let (reference, export, pump_events, pump_fault) = run.recovery_pump.into_parts();
        self.reference = Some(reference);
        self.export = Some(export);
        events.extend(pump_events);
        if let Some(detail) = pump_fault {
            return Err(detail);
        }
        let surface_receipt = run.result?;
        events.push(EnduranceCampaignEvent::recovery_step_completed(
            self.phase_elapsed_us(clock)?,
            &surface_receipt,
        ));
        self.resume_if_settled(deadline)?;

        let export_time = self.phase_elapsed_us(clock)?;
        let export = self.export.as_mut().ok_or("Concurrent Recovery lost its Export owner")?;
        events.extend(export.poll(export_time).map_err(|error| error.to_string())?);
        if Instant::now() >= deadline {
            return Err(
                "recovery deadline elapsed while observing Export before cancellation".to_owned(),
            );
        }
        export
            .ensure_attempt_admitted_for_recovery()
            .map_err(|error| error.to_string())?;
        if Instant::now() >= deadline {
            return Err(
                "recovery deadline elapsed while admitting the Export cancellation target"
                    .to_owned(),
            );
        }
        export
            .begin_cancel_retry_recovery(cycle_index)
            .map_err(|error| error.to_string())?;
        loop {
            if Instant::now() >= deadline {
                return Err("Export cancel/retry exceeded the recovery deadline".to_owned());
            }
            events.extend(self.pump_one(self.phase_elapsed_us(clock)?)?);
            if !self
                .export
                .as_ref()
                .ok_or("Concurrent Recovery lost its Export owner")?
                .cancel_retry_recovery_in_progress()
            {
                break;
            }
            std::thread::yield_now();
        }
        if Instant::now() >= deadline {
            return Err("recovery deadline elapsed before Cache Pressure could begin".to_owned());
        }

        {
            let app = self.app.as_mut().ok_or("phase App owner is missing")?;
            let execution = self.execution.as_mut().ok_or("execution owner is missing")?;
            let receipt = self
                .timeline
                .as_mut()
                .ok_or("Timeline owner is missing")?
                .recover_cache_pressure(app, execution, cycle_index, Some(deadline))
                .map_err(|error| error.to_string())?;
            events.push(EnduranceCampaignEvent::recovery_step_completed(
                self.phase_elapsed_us(clock)?,
                &receipt,
            ));
        }
        self.next_recovery_cycle = self
            .next_recovery_cycle
            .checked_add(1)
            .ok_or("recovery cycle counter overflow")?;
        let completed_at_us = self.phase_elapsed_us(clock)?;
        events.extend(
            self.export
                .as_mut()
                .ok_or("Concurrent Recovery lost its Export owner")?
                .poll(completed_at_us)
                .map_err(|error| error.to_string())?,
        );
        Ok(events)
    }

    fn live_snapshot(&self) -> Result<EnduranceRuntimeSnapshot, String> {
        let app = self.app_ref()?;
        match self.kind {
            EndurancePhaseKind::ContinuousExport => {
                let capture_facts = EnduranceCaptureFacts::from_app_background(
                    app.background_endurance_snapshot()
                        .map_err(|error| format!("capture App background owners: {error}"))?,
                )?;
                Ok(EnduranceRuntimeSnapshot::continuous_export(
                    app.playback_evidence_report(),
                    app.reference_output_diagnostics().cloned().unwrap_or_default(),
                    app.export_endurance_snapshot(0),
                )
                .with_capture_facts(capture_facts))
            }
            EndurancePhaseKind::PlaybackReference | EndurancePhaseKind::ConcurrentRecovery => {
                if !self.settled {
                    return Err(
                        "realtime endurance snapshot requires a settled boundary".to_owned()
                    );
                }
                self.execution
                    .as_ref()
                    .ok_or("execution owner is missing")?
                    .runtime_snapshot(app, self.kind)
                    .map_err(|error| error.to_string())
            }
        }
    }
}

/// Concrete runtime over fresh App owners and the production phase drivers.
pub(crate) struct ProductEnduranceCampaignRuntime<F, S, C> {
    factory: F,
    surface: S,
    clock: Arc<C>,
    timeouts: Option<EnduranceProductRuntimeTimeouts>,
    machine_plan: Option<PreparedCommercialEnduranceMachinePlan>,
    state: RuntimeState,
}

impl<F, S, C> ProductEnduranceCampaignRuntime<F, S, C> {
    fn new(factory: F, surface: S, clock: Arc<C>) -> Self {
        Self {
            factory,
            surface,
            clock,
            timeouts: None,
            machine_plan: None,
            state: RuntimeState::Empty,
        }
    }
}

/// Run the serial supervisor and concrete product runtime against one shared
/// monotonic clock authority.
///
/// A real Window driver must be invoked from the platform UI/main thread. The
/// current real-Window operation has local Windows evidence only; macOS and
/// Linux native behavior remain transfer qualification cells.
pub fn run_product_endurance_campaign<F, S, C, P>(
    prepared_request: PreparedEnduranceRunRequest,
    factory: F,
    surface: S,
    clock: Arc<C>,
    process_memory: &P,
) -> Result<EnduranceRunManifest, EnduranceCampaignError>
where
    F: FreshEndurancePhaseFactory,
    S: EnduranceSurfaceReopenDriver,
    C: EnduranceCampaignClock,
    P: ProcessMemoryProbe,
{
    let request = prepared_request.into_campaign_request();
    let mut runtime = ProductEnduranceCampaignRuntime::new(factory, surface, Arc::clone(&clock));
    run_endurance_campaign(request, &mut runtime, process_memory, clock.as_ref())
}

impl<F, S, C> EnduranceCampaignRuntime for ProductEnduranceCampaignRuntime<F, S, C>
where
    F: FreshEndurancePhaseFactory,
    S: EnduranceSurfaceReopenDriver,
    C: EnduranceCampaignClock,
{
    fn bind_machine_plan(
        &mut self,
        machine_plan: PreparedCommercialEnduranceMachinePlan,
    ) -> Result<(), EnduranceCampaignError> {
        if !matches!(self.state, RuntimeState::Empty) || self.machine_plan.is_some() {
            return Err(runtime_error(
                "commercial endurance machine plan must be bound exactly once before phase work",
            ));
        }
        let planned = machine_plan.plan().timeouts;
        let timeouts = EnduranceProductRuntimeTimeouts::new(
            Duration::from_millis(planned.interval_ms),
            Duration::from_millis(planned.recovery_ms),
            Duration::from_millis(planned.surface_reopen_ms),
            Duration::from_millis(planned.shutdown_ms),
        )?;
        self.machine_plan = Some(machine_plan);
        self.timeouts = Some(timeouts);
        Ok(())
    }

    fn begin_phase(
        &mut self,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        phase_started_at_run_us: u64,
    ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError> {
        match self.state {
            RuntimeState::Owned(_) => {
                return Err(runtime_error(
                    "cannot begin an endurance phase while another phase owns resources",
                ));
            }
            RuntimeState::Terminal(None) => {
                return Err(runtime_error(
                    "cannot begin after terminal snapshot construction failed",
                ));
            }
            RuntimeState::Empty | RuntimeState::Terminal(Some(_)) => {}
        }
        self.state = RuntimeState::Empty;
        let timeouts = self.timeouts.ok_or_else(|| {
            runtime_error("commercial endurance machine-plan timeouts are missing")
        })?;
        let machine_plan = self
            .machine_plan
            .as_ref()
            .ok_or_else(|| runtime_error("commercial endurance machine plan is not bound"))?;

        let inventory = self
            .factory
            .pre_start_capability_inventory(machine_plan, requirement, workload)
            .map_err(|error| EnduranceCampaignError::PreStartRuntime(error.to_string()))?;
        let prepared_start = match workload.prepare_start(&inventory) {
            Ok(prepared_start) => prepared_start,
            Err(not_run) => return Ok(EndurancePhaseAdmission::NotRun(not_run)),
        };

        let build = self.factory.build_phase(machine_plan, requirement, workload, prepared_start);
        let mut owners = match build {
            FreshEndurancePhaseBuild::Ready(fresh) => {
                match PhaseOwners::from_fresh(
                    requirement,
                    workload,
                    phase_started_at_run_us,
                    *fresh,
                ) {
                    Ok(owners) => owners,
                    Err((mut owners, detail)) => {
                        owners.fault = Some(detail.clone());
                        self.state = RuntimeState::Owned(owners);
                        return Err(runtime_error(detail));
                    }
                }
            }
            FreshEndurancePhaseBuild::Failed { app_state, detail } => {
                let owners = PhaseOwners::failed_before_start(
                    requirement.kind,
                    *app_state,
                    phase_started_at_run_us,
                    requirement.minimum_duration_us,
                    workload.recovery_cycle_count(),
                    detail.clone(),
                );
                self.state = RuntimeState::Owned(Box::new(owners));
                return Err(runtime_error(detail));
            }
        };
        if let Err(detail) = owners.start(requirement, workload, timeouts) {
            owners.fault = Some(detail.clone());
            self.state = RuntimeState::Owned(owners);
            return Err(runtime_error(detail));
        }
        self.state = RuntimeState::Owned(owners);
        Ok(EndurancePhaseAdmission::Started)
    }

    fn pump_until(
        &mut self,
        deadline_run_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, EnduranceCampaignError> {
        let timeouts = self.timeouts.ok_or_else(|| {
            runtime_error("commercial endurance machine-plan timeouts are missing")
        })?;
        let RuntimeState::Owned(owners) = &mut self.state else {
            return Err(runtime_error(
                "no started endurance phase is available to pump",
            ));
        };
        if let Some(detail) = &owners.fault {
            return Err(runtime_error(format!(
                "endurance phase is faulted: {detail}"
            )));
        }
        let now_run_us = self.clock.elapsed_us();
        let remaining_us = deadline_run_us.saturating_sub(now_run_us);
        let residency_deadline = Instant::now()
            .checked_add(Duration::from_micros(remaining_us))
            .ok_or_else(|| runtime_error("realtime residency deadline overflow"))?;
        owners.resume_if_settled(residency_deadline).map_err(|detail| {
            owners.fault = Some(detail.clone());
            runtime_error(detail)
        })?;

        let mut events = Vec::new();
        while owners.recovery_due(deadline_run_us).map_err(runtime_error)? {
            match owners.execute_recovery_cycle(&mut self.surface, self.clock.as_ref(), timeouts) {
                Ok(recovery_events) => events.extend(recovery_events),
                Err(detail) => {
                    owners.fault = Some(detail.clone());
                    return Err(runtime_error(detail));
                }
            }
        }
        while self.clock.elapsed_us() < deadline_run_us {
            let completed_at_us =
                owners.phase_elapsed_us(self.clock.as_ref()).map_err(runtime_error)?;
            match owners.pump_one(completed_at_us) {
                Ok(pump_events) => events.extend(pump_events),
                Err(detail) => {
                    owners.fault = Some(detail.clone());
                    return Err(runtime_error(detail));
                }
            }
            if owners.timeline.is_none() {
                std::thread::yield_now();
            }
        }
        if let Err(detail) = owners.settle() {
            owners.fault = Some(detail.clone());
            return Err(runtime_error(detail));
        }
        Ok(events)
    }

    fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError> {
        match &self.state {
            RuntimeState::Owned(owners) => owners.live_snapshot().map_err(runtime_error),
            RuntimeState::Terminal(Some(snapshot)) => Ok((**snapshot).clone()),
            RuntimeState::Empty | RuntimeState::Terminal(None) => {
                Err(runtime_error("endurance runtime has no snapshot authority"))
            }
        }
    }

    fn shutdown_phase(
        &mut self,
    ) -> Result<(EnduranceRuntimeClosure, Vec<EnduranceCampaignEvent>), EnduranceCampaignError>
    {
        let timeouts = self.timeouts.ok_or_else(|| {
            runtime_error("commercial endurance machine-plan timeouts are missing")
        })?;
        let state = std::mem::replace(&mut self.state, RuntimeState::Terminal(None));
        let RuntimeState::Owned(mut owners) = state else {
            self.state = state;
            return Err(runtime_error(
                "no owned endurance phase is available to shut down",
            ));
        };
        let (deadline, deadline_failure) = match Instant::now().checked_add(timeouts.shutdown) {
            Some(deadline) => (deadline, None),
            None => (
                Instant::now(),
                Some("endurance shutdown deadline overflow".to_owned()),
            ),
        };
        let mut failures = owners.fault.take().into_iter().collect::<Vec<_>>();
        failures.extend(deadline_failure);
        if owners.kind == EndurancePhaseKind::ConcurrentRecovery
            && owners.next_recovery_cycle != owners.recovery_cycle_count
        {
            failures.push(format!(
                "Concurrent Recovery closed after {} of {} required cycles",
                owners.next_recovery_cycle, owners.recovery_cycle_count
            ));
        }
        let mut events = Vec::new();

        if let Some(export) = owners.export.as_mut() {
            export.begin_close();
        }
        if owners.timeline.is_some() {
            let close = {
                let app = owners.app.as_mut().ok_or("phase App owner is missing");
                let execution = owners.execution.as_mut().ok_or("execution owner is missing");
                match (app, execution, owners.timeline.as_mut()) {
                    (Ok(app), Ok(execution), Some(timeline)) => {
                        timeline.begin_close(app, execution).map_err(|error| error.to_string())
                    }
                    (Err(detail), _, _) | (_, Err(detail), _) => Err(detail.to_owned()),
                    (_, _, None) => Ok(()),
                }
            };
            if let Err(detail) = close {
                failures.push(detail);
            } else {
                owners.settled = true;
            }
        }
        if owners.reference.is_some() {
            let close = match (owners.app.as_mut(), owners.reference.as_mut()) {
                (Some(app), Some(reference)) => {
                    reference.begin_close(app).map_err(|error| error.to_string())
                }
                _ => Err("Reference close lost its App or phase owner".to_owned()),
            };
            if let Err(detail) = close {
                failures.push(detail);
            }
        }

        while owners.export.as_ref().is_some_and(|export| !export.is_quiescent()) {
            if Instant::now() >= deadline {
                failures
                    .push("repeated Export close exceeded the shared shutdown deadline".to_owned());
                break;
            }
            let completed_at_us = match owners.phase_elapsed_us(self.clock.as_ref()) {
                Ok(completed_at_us) => completed_at_us,
                Err(detail) => {
                    failures.push(detail);
                    0
                }
            };
            let Some(export) = owners.export.as_mut() else {
                failures.push("repeated Export owner disappeared during close".to_owned());
                break;
            };
            let polled = export.poll(completed_at_us);
            match polled {
                Ok(close_events) => events.extend(close_events),
                Err(error) => {
                    failures.push(error.to_string());
                    break;
                }
            }
            std::thread::yield_now();
        }

        let live_snapshot = match owners.live_snapshot() {
            Ok(snapshot) => Some(snapshot),
            Err(detail) => {
                failures.push(detail);
                None
            }
        };
        drop(owners.export.take());
        drop(owners.reference.take());
        drop(owners.timeline.take());
        let continuous_background = if owners.execution.is_none() {
            let background = match owners.app.as_ref() {
                Some(app) => app.background_endurance_snapshot(),
                None => Err("shutdown lost the phase App owner".to_owned()),
            };
            match background {
                Ok(background) => Some(background),
                Err(detail) => {
                    failures.push(format!(
                        "capture pre-shutdown App background owners: {detail}"
                    ));
                    None
                }
            }
        } else {
            None
        };
        let app = owners
            .app
            .take()
            .ok_or_else(|| runtime_error("shutdown lost the phase App owner"))?;

        let (app_shutdown, capture_facts, playback_workers_terminated) =
            if let Some(execution) = owners.execution.take() {
                let closure = execution.shutdown_until(app, deadline)?;
                failures.extend(closure.owner_snapshot_failure.clone());
                failures.extend(closure.terminal_projection_failure.clone());
                let capture_facts = closure.capture_facts();
                let workers = closure.all_workers_terminated();
                (closure.app, capture_facts, workers)
            } else {
                let app_shutdown = app.shutdown_for_endurance(deadline);
                let workers = app_shutdown.all_resources_released();
                let capture_facts = match (
                    continuous_background,
                    app_shutdown.background_terminal_snapshot(),
                ) {
                    (Some(running), Ok(terminal)) => {
                        EnduranceCaptureFacts::from_app_background(running.merge_terminal(terminal))
                            .unwrap_or_else(|detail| {
                                failures.push(detail);
                                EnduranceCaptureFacts::failed_continuous_export()
                            })
                    }
                    (_, Err(detail)) => {
                        failures.push(detail);
                        EnduranceCaptureFacts::failed_continuous_export()
                    }
                    (None, Ok(_)) => EnduranceCaptureFacts::failed_continuous_export(),
                };
                (app_shutdown, capture_facts, workers)
            };
        let export_shutdown = app_shutdown.export;
        let supervised_child_processes_remaining =
            supervised_child_processes_remaining(&app_shutdown);
        let all_app_resources_released = app_shutdown.all_resources_released();
        if !all_app_resources_released {
            failures.push("App endurance shutdown retained resources".to_owned());
        }
        let terminal_snapshot = live_snapshot.map(|snapshot| {
            snapshot.terminalize(
                app_shutdown.reference_output.diagnostics.clone(),
                app_shutdown.export_terminal_snapshot,
                capture_facts,
            )
        });
        let status = if failures.is_empty() {
            EndurancePhaseTerminalStatus::Completed
        } else {
            EndurancePhaseTerminalStatus::Failed
        };
        let closure = EnduranceRuntimeClosure {
            status,
            playback_workers_terminated,
            supervised_child_processes_remaining,
            export: export_shutdown,
        };
        self.state = RuntimeState::Terminal(terminal_snapshot.map(Box::new));
        Ok((closure, events))
    }
}

fn runtime_error(detail: impl Into<String>) -> EnduranceCampaignError {
    EnduranceCampaignError::Runtime(detail.into())
}

fn supervised_child_processes_remaining(
    shutdown: &super::endurance_shutdown::AppEnduranceShutdownEvidence,
) -> u32 {
    let source = &shutdown.audio_source_cache;
    let remaining = match source.cache.as_ref() {
        Some(cache) => cache
            .child_processes_observed
            .saturating_sub(cache.child_processes_terminated)
            .max(cache.child_process_termination_failures)
            .max(usize::from(cache.decoder_resource_handles_remaining > 0)),
        None => 1,
    }
    .max(usize::from(source.strong_references_remaining > 0));
    u32::try_from(remaining).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mondrian_platform::{EndurancePhaseTerminalStatus, EnduranceQualificationProfile};

    use super::*;
    use crate::app::endurance_workload::EndurancePreStartCapability;

    #[derive(Default)]
    struct TestClock(AtomicU64);

    impl EnduranceCampaignClock for TestClock {
        fn elapsed_us(&self) -> u64 {
            self.0.load(Ordering::Acquire)
        }
    }

    #[derive(Default)]
    struct ForbiddenSurface;

    impl EnduranceSurfaceReopenDriver for ForbiddenSurface {
        fn reopen(
            &mut self,
            _app_state: AppState,
            _recovery_pump: EnduranceSurfaceRecoveryPump,
            _cycle_index: u32,
            _operation_id: String,
            _timeout: Duration,
        ) -> EnduranceSurfaceReopenRun {
            panic!("Surface reopen must not run in admission/partial-start tests")
        }
    }

    struct TestFactory {
        inventory: EndurancePreStartCapabilityInventory,
        build_calls: Rc<Cell<u32>>,
        fail_build: bool,
    }

    impl FreshEndurancePhaseFactory for TestFactory {
        fn pre_start_capability_inventory(
            &mut self,
            machine_plan: &PreparedCommercialEnduranceMachinePlan,
            _requirement: &EndurancePhaseRequirement,
            _workload: &PreparedEnduranceWorkload,
        ) -> Result<EndurancePreStartCapabilityInventory, EnduranceCampaignError> {
            assert_eq!(machine_plan.plan().schema_version, 1);
            Ok(self.inventory.clone())
        }

        fn build_phase(
            &mut self,
            machine_plan: &PreparedCommercialEnduranceMachinePlan,
            requirement: &EndurancePhaseRequirement,
            workload: &PreparedEnduranceWorkload,
            prepared_start: PreparedEndurancePhaseStart,
        ) -> FreshEndurancePhaseBuild {
            self.build_calls.set(self.build_calls.get() + 1);
            assert_eq!(machine_plan.plan().schema_version, 1);
            assert_eq!(prepared_start.phase_id(), requirement.phase_id);
            assert_eq!(prepared_start.workload_id(), workload.workload_id());
            assert_eq!(prepared_start.kind(), requirement.kind);
            assert!(self.fail_build, "test factory has no success composition");
            FreshEndurancePhaseBuild::failed(AppState::new(), "injected phase setup failure")
        }
    }

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn qualification_profile() -> EnduranceQualificationProfile {
        serde_json::from_slice(
            &fs::read(root().join("tests/validation/commercial-endurance-qualification.json"))
                .expect("checked-in endurance profile"),
        )
        .expect("endurance profile schema")
    }

    fn continuous_export_contract() -> (EndurancePhaseRequirement, PreparedEnduranceWorkload) {
        let profile = qualification_profile();
        let requirement = profile
            .phases
            .into_iter()
            .find(|phase| phase.kind == EndurancePhaseKind::ContinuousExport)
            .expect("continuous Export requirement");
        let workload = PreparedEnduranceWorkload::load(
            &requirement,
            &root().join("tests/validation/endurance-workloads/continuous-export-v1.json"),
        )
        .expect("typed continuous Export workload");
        (requirement, workload)
    }

    fn bind_test_machine_plan(
        runtime: &mut ProductEnduranceCampaignRuntime<TestFactory, ForbiddenSurface, TestClock>,
    ) -> tempfile::TempDir {
        let temporary = tempfile::tempdir().expect("temporary machine plan");
        let path = temporary.path().join("machine-plan.json");
        let profile = qualification_profile();
        crate::app::endurance_machine_plan::write_test_machine_plan(
            &path,
            temporary.path(),
            &profile,
            24,
        );
        let prepared = PreparedCommercialEnduranceMachinePlan::load(&path, &profile, 24)
            .expect("prepare test machine plan");
        runtime.bind_machine_plan(prepared).expect("bind exact test plan");
        assert_eq!(
            runtime.timeouts,
            Some(
                EnduranceProductRuntimeTimeouts::new(
                    Duration::from_millis(1_000),
                    Duration::from_millis(60_000),
                    Duration::from_millis(30_000),
                    Duration::from_millis(60_000),
                )
                .expect("planned timeouts")
            )
        );
        temporary
    }

    #[test]
    fn not_run_admission_creates_no_app_or_product_owner() {
        let (requirement, workload) = continuous_export_contract();
        let build_calls = Rc::new(Cell::new(0));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([]),
            build_calls: Rc::clone(&build_calls),
            fail_build: false,
        };
        let mut runtime = ProductEnduranceCampaignRuntime::new(
            factory,
            ForbiddenSurface,
            Arc::new(TestClock::default()),
        );
        let _machine_plan = bind_test_machine_plan(&mut runtime);

        let admission =
            runtime.begin_phase(&requirement, &workload, 0).expect("typed NotRun admission");

        assert!(matches!(admission, EndurancePhaseAdmission::NotRun(_)));
        assert_eq!(build_calls.get(), 0);
        assert!(runtime.shutdown_phase().is_err());
    }

    #[test]
    fn failed_fresh_setup_retains_app_for_exactly_once_consuming_shutdown() {
        let (requirement, workload) = continuous_export_contract();
        let build_calls = Rc::new(Cell::new(0));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([
                EndurancePreStartCapability::FrozenExportFixturePrepared,
                EndurancePreStartCapability::IndependentExportVerifierPrepared,
            ]),
            build_calls: Rc::clone(&build_calls),
            fail_build: true,
        };
        let mut runtime = ProductEnduranceCampaignRuntime::new(
            factory,
            ForbiddenSurface,
            Arc::new(TestClock::default()),
        );
        let _machine_plan = bind_test_machine_plan(&mut runtime);

        assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
        assert_eq!(build_calls.get(), 1);
        let (closure, events) = runtime.shutdown_phase().expect("consuming cleanup");
        assert_eq!(closure.status, EndurancePhaseTerminalStatus::Failed);
        assert!(closure.playback_workers_terminated);
        assert!(closure.export.all_resources_released());
        assert!(events.is_empty());
        assert!(runtime.snapshot().is_ok());
        assert!(runtime.shutdown_phase().is_err());
    }

    #[test]
    fn runtime_timeouts_reject_zero_without_owner_creation() {
        assert!(EnduranceProductRuntimeTimeouts::new(
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .is_err());
    }

    #[test]
    fn phase_event_time_uses_the_phase_origin() {
        let clock = TestClock(AtomicU64::new(10_000));
        let owners = PhaseOwners::failed_before_start(
            EndurancePhaseKind::ContinuousExport,
            AppState::new(),
            9_500,
            1,
            0,
            "test owner".to_owned(),
        );

        assert_eq!(owners.phase_elapsed_us(&clock), Ok(500));
    }

    #[test]
    fn overdue_zero_cadence_settles_and_snapshots_instead_of_failing() {
        let clock = Arc::new(TestClock(AtomicU64::new(10_500)));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([]),
            build_calls: Rc::new(Cell::new(0)),
            fail_build: false,
        };
        let mut owners = PhaseOwners::failed_before_start(
            EndurancePhaseKind::ContinuousExport,
            AppState::new(),
            10_000,
            1,
            0,
            "temporary test state".to_owned(),
        );
        owners.fault = None;
        let mut runtime =
            ProductEnduranceCampaignRuntime::new(factory, ForbiddenSurface, Arc::clone(&clock));
        let _machine_plan = bind_test_machine_plan(&mut runtime);
        runtime.state = RuntimeState::Owned(Box::new(owners));

        assert!(runtime.pump_until(10_000).expect("late zero cadence").is_empty());
        assert!(runtime.snapshot().is_ok());
        let (closure, _) = runtime.shutdown_phase().expect("consume test App");
        assert!(closure.playback_workers_terminated);
        assert_eq!(closure.supervised_child_processes_remaining, 0);
        assert!(closure.export.all_resources_released());
    }
}
