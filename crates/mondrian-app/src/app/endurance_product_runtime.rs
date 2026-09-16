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
    EnduranceRunManifest, EnduranceRunOwnerClosureEvidence, ProcessMemoryProbe,
};
use mondrian_reference_output::{ReferenceOutputDeviceDescriptor, ReferenceOutputOpenRequest};

use super::endurance_campaign::{
    run_endurance_campaign, EnduranceCampaignClock, EnduranceCampaignError, EnduranceCampaignEvent,
    EnduranceCampaignRuntime, EnduranceExecutionOwners, EndurancePhaseTerminalEvidence,
    EnduranceRunOwnerShutdownFailure, EnduranceRuntimeClosure, EnduranceRuntimeSnapshot,
    EnduranceTerminalOwners,
};
use super::endurance_export::{FrozenRepeatedExportPhase, FrozenRepeatedExportRequest};
use super::endurance_ffmpeg_toolchain::PreparedEnduranceFfmpegToolchain;
use super::endurance_machine_plan::PreparedCommercialEnduranceMachinePlan;
use super::endurance_playback::PersistentTimelinePlaybackPhase;
use super::endurance_qualification::EnduranceCaptureFacts;
use super::endurance_recovery::EnduranceRecoveryOperationReceipt;
use super::endurance_reference_output::PersistentReferenceOutputPump;
use super::endurance_run_request::PreparedEnduranceRunRequest;
use super::endurance_source_inventory::{
    EnduranceSourceInventoryError, PreparedEnduranceSourceInventory,
};
use super::endurance_workload::{
    EndurancePhaseAdmission, EndurancePreStartCapabilityInventory, PreparedEndurancePhaseStart,
    PreparedEnduranceWorkload,
};
use super::AppState;

/// Fixed latency bounds for one product endurance runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceProductRuntimeTimeouts {
    startup: Duration,
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
        Ok(Self {
            startup: Duration::from_secs(120),
            interval,
            recovery,
            surface_reopen,
            shutdown,
        })
    }
}

/// Exact Reference Output composition selected before a phase starts.
pub struct EnduranceReferenceOutputPlan {
    device: ReferenceOutputDeviceDescriptor,
    request: ReferenceOutputOpenRequest,
    first_frame_index: u64,
    wire_correlation: Option<mondrian_broadcast::AncillaryWireCorrelation>,
    ancillary_program: Option<Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>,
}

impl EnduranceReferenceOutputPlan {
    /// Bind one discovered device and exact signal request to the first frame.
    pub fn new(
        device: ReferenceOutputDeviceDescriptor,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Self {
        Self {
            device,
            request,
            first_frame_index,
            wire_correlation: None,
            ancillary_program: None,
        }
    }
    /// Carry the retained canonical program into physical output.
    pub fn with_ancillary_program(
        mut self,
        program: Option<Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>,
    ) -> Self {
        self.ancillary_program = program;
        self
    }
    /// Carry the phase-owned canonical validation marker into the physical pump.
    pub fn with_wire_correlation(
        mut self,
        correlation: Option<mondrian_broadcast::AncillaryWireCorrelation>,
    ) -> Self {
        self.wire_correlation = correlation;
        self
    }
}

enum FreshEndurancePhaseInputs {
    PlaybackReference(Box<EnduranceReferenceOutputPlan>),
    ContinuousExport(Box<FrozenRepeatedExportRequest>),
    ConcurrentRecovery(Box<FreshConcurrentRecoveryInputs>),
    #[cfg(test)]
    Test(EndurancePhaseKind),
}

struct FreshConcurrentRecoveryInputs {
    reference: EnduranceReferenceOutputPlan,
    export: FrozenRepeatedExportRequest,
    seek_targets: Vec<i64>,
}

enum EndurancePhaseSourceAuthority {
    Qualified(Box<PreparedEnduranceSourceInventory>),
    #[cfg(test)]
    Test,
}

/// Exact machine-plan and external-source authority retained by one phase.
///
/// Production construction requires the inventory derived from the same live
/// App and prepared machine plan. The runtime additionally requires pointer
/// identity with its bound plan before any phase owner may start.
pub struct PreparedEndurancePhaseAuthority {
    machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
    sources: EndurancePhaseSourceAuthority,
}

impl PreparedEndurancePhaseAuthority {
    /// Bind retained source objects to the exact plan and unchanged live App.
    pub fn new(
        app_state: &AppState,
        machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
        sources: PreparedEnduranceSourceInventory,
    ) -> Result<Self, EnduranceSourceInventoryError> {
        sources.validate_current(app_state, machine_plan.as_ref())?;
        Ok(Self {
            machine_plan,
            sources: EndurancePhaseSourceAuthority::Qualified(Box::new(sources)),
        })
    }

    /// Exact prepared plan retained for the complete phase lifetime.
    pub fn machine_plan(&self) -> &Arc<PreparedCommercialEnduranceMachinePlan> {
        &self.machine_plan
    }

    /// Exact retained external-source inventory.
    pub fn source_inventory(&self) -> &PreparedEnduranceSourceInventory {
        match &self.sources {
            EndurancePhaseSourceAuthority::Qualified(sources) => sources,
            #[cfg(test)]
            EndurancePhaseSourceAuthority::Test => {
                unreachable!("test-only phase authority has no source inventory")
            }
        }
    }

    fn validate_current(&self, app_state: &AppState) -> Result<(), EnduranceSourceInventoryError> {
        match &self.sources {
            EndurancePhaseSourceAuthority::Qualified(sources) => {
                sources.validate_current(app_state, self.machine_plan.as_ref())
            }
            #[cfg(test)]
            EndurancePhaseSourceAuthority::Test => Ok(()),
        }
    }

    #[cfg(test)]
    fn test(machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>) -> Self {
        Self {
            machine_plan,
            sources: EndurancePhaseSourceAuthority::Test,
        }
    }
}

/// Fresh App owner plus all machine-composed inputs for one exact phase.
pub struct FreshEndurancePhase {
    app_state: AppState,
    authority: PreparedEndurancePhaseAuthority,
    inputs: FreshEndurancePhaseInputs,
}

impl FreshEndurancePhase {
    /// Compose a fresh Playback + physical Reference phase.
    pub fn playback_reference(
        app_state: AppState,
        authority: PreparedEndurancePhaseAuthority,
        reference: EnduranceReferenceOutputPlan,
    ) -> Self {
        Self {
            app_state,
            authority,
            inputs: FreshEndurancePhaseInputs::PlaybackReference(Box::new(reference)),
        }
    }

    /// Compose a fresh repeated Export phase.
    pub fn continuous_export(
        app_state: AppState,
        authority: PreparedEndurancePhaseAuthority,
        export: FrozenRepeatedExportRequest,
    ) -> Self {
        Self {
            app_state,
            authority,
            inputs: FreshEndurancePhaseInputs::ContinuousExport(Box::new(export)),
        }
    }

    /// Compose a fresh overlapping Playback/Reference/Export recovery phase.
    pub fn concurrent_recovery(
        app_state: AppState,
        authority: PreparedEndurancePhaseAuthority,
        reference: EnduranceReferenceOutputPlan,
        export: FrozenRepeatedExportRequest,
        seek_targets: Vec<i64>,
    ) -> Self {
        Self {
            app_state,
            authority,
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
            #[cfg(test)]
            FreshEndurancePhaseInputs::Test(kind) => kind,
        }
    }
}

/// Factory result distinguishing pre-owner rejection from retained setup failure.
pub enum FreshEndurancePhaseBuild {
    /// Exact authority changed before any App or worker owner was created.
    Rejected {
        /// Stable operator-facing failure detail.
        detail: String,
    },
    /// Every machine-specific input was prepared.
    Ready(Box<FreshEndurancePhase>),
    /// Setup failed after fresh App creation; the runtime must still consume it.
    Failed {
        /// Fresh owner retained for exactly-once shutdown.
        app_state: Box<AppState>,
        /// Any exact source authority already prepared before later setup failed.
        authority: Option<Box<PreparedEndurancePhaseAuthority>>,
        /// Stable operator-facing failure detail.
        detail: String,
    },
}

impl FreshEndurancePhaseBuild {
    /// Reject a build before creating an App or worker owner.
    pub fn rejected(detail: impl Into<String>) -> Self {
        Self::Rejected { detail: detail.into() }
    }

    /// Retain a fully composed fresh phase behind one bounded owner payload.
    pub fn ready(phase: FreshEndurancePhase) -> Self {
        Self::Ready(Box::new(phase))
    }

    /// Preserve a fresh App owner when machine-specific setup fails.
    pub fn failed(app_state: AppState, detail: impl Into<String>) -> Self {
        Self::Failed {
            app_state: Box::new(app_state),
            authority: None,
            detail: detail.into(),
        }
    }

    /// Preserve a fresh App and its exact source leases after later setup fails.
    pub fn failed_with_authority(
        app_state: AppState,
        authority: PreparedEndurancePhaseAuthority,
        detail: impl Into<String>,
    ) -> Self {
        Self::Failed {
            app_state: Box::new(app_state),
            authority: Some(Box::new(authority)),
            detail: detail.into(),
        }
    }
}

/// Factory authority prepared before any machine-specific App or worker owner.
///
/// Construction installs and retains the exact FFmpeg closure first, then
/// invokes the supplied side-effect-free factory builder. The public campaign
/// entrypoint accepts only this prepared wrapper.
pub struct PreparedEnduranceMachinePhaseFactory<F> {
    inner: F,
    ffmpeg: PreparedEnduranceFfmpegToolchain,
}

impl<F> PreparedEnduranceMachinePhaseFactory<F> {
    /// Prepare exact process media authority before constructing the phase factory.
    pub fn prepare<B>(
        request: &PreparedEnduranceRunRequest,
        build: B,
    ) -> Result<Self, EnduranceCampaignError>
    where
        B: FnOnce(&PreparedEnduranceFfmpegToolchain) -> Result<F, EnduranceCampaignError>,
    {
        #[cfg(windows)]
        if let Some(binding) = &request.machine_plan().plan().verifier_tools.preloader {
            let launcher = mondrian_validation_launcher::FileBinding {
                path: binding.path.clone(),
                sha256: binding.sha256.clone(),
            };
            let runtime_files = request
                .machine_plan()
                .plan()
                .verifier_tools
                .runtime_files
                .iter()
                .map(|binding| mondrian_validation_launcher::FileBinding {
                    path: binding.path.clone(),
                    sha256: binding.sha256.clone(),
                })
                .collect::<Vec<_>>();
            mondrian_validation_launcher::prepare_process_authority(
                mondrian_validation_launcher::AttestationExpectation {
                    launcher: &launcher,
                    application_sha256: request.runtime_image_sha256(),
                    request_sha256: request.request_sha256(),
                    machine_plan_sha256: request.machine_plan().sha256(),
                    runtime_files: &runtime_files,
                },
            )
            .map_err(|error| runtime_error(error.to_string()))?;
        }
        let ffmpeg = PreparedEnduranceFfmpegToolchain::prepare_and_install(request.machine_plan())
            .map_err(|error| {
                runtime_error(format!("prepare exact endurance FFmpeg authority: {error}"))
            })?;
        let inner = match build(&ffmpeg) {
            Ok(inner) => inner,
            Err(primary) => {
                let receipt = ffmpeg.shutdown_until(Instant::now() + Duration::from_secs(5));
                let mut shutdown = EnduranceRunOwnerShutdownFailure::new(
                    "factory construction failed; exact-runtime closure retained",
                );
                shutdown.attach_ffmpeg_closure(receipt);
                return Err(EnduranceCampaignError::RunOwnerShutdownAfterFailure {
                    primary: Box::new(primary),
                    shutdown,
                });
            }
        };
        Ok(Self { inner, ffmpeg })
    }

    /// Explicit preflight-only closure when no campaign runtime was constructed.
    pub fn shutdown_ffmpeg_capsule_until(
        &self,
        deadline: Instant,
    ) -> mondrian_media::QualifiedFfmpegShutdownReceipt {
        self.ffmpeg.shutdown_until(deadline)
    }

    /// Canonical exact-runtime receipt retained by this campaign factory.
    pub fn ffmpeg_toolchain_receipt_sha256(&self) -> &str {
        self.ffmpeg.toolchain_receipt_sha256()
    }

    fn validate_machine_plan(
        &self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
    ) -> Result<(), EnduranceCampaignError> {
        self.ffmpeg.validate_machine_plan(machine_plan).map_err(|error| {
            runtime_error(format!(
                "revalidate exact endurance FFmpeg authority: {error}"
            ))
        })
    }
}

/// Machine composition seam for fresh phase owners.
///
/// Capability inventory is observed before `build_phase`; a `NotRun` result
/// therefore creates no App, worker, device, Queue, or child-process owner.
pub trait FreshEndurancePhaseFactory {
    /// Consume a retained exact-runtime capsule after all phase and Surface owners.
    fn shutdown_ffmpeg_capsule(
        &mut self,
        _deadline: Instant,
    ) -> Option<mondrian_media::QualifiedFfmpegShutdownReceipt> {
        None
    }

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
        machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        prepared_start: PreparedEndurancePhaseStart,
    ) -> FreshEndurancePhaseBuild;
}

impl<F> FreshEndurancePhaseFactory for PreparedEnduranceMachinePhaseFactory<F>
where
    F: FreshEndurancePhaseFactory,
{
    fn shutdown_ffmpeg_capsule(
        &mut self,
        deadline: Instant,
    ) -> Option<mondrian_media::QualifiedFfmpegShutdownReceipt> {
        Some(self.ffmpeg.shutdown_until(deadline))
    }

    fn pre_start_capability_inventory(
        &mut self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
    ) -> Result<EndurancePreStartCapabilityInventory, EnduranceCampaignError> {
        self.validate_machine_plan(machine_plan)?;
        let mut inventory =
            self.inner.pre_start_capability_inventory(machine_plan, requirement, workload)?;
        // The capability is derived only from the completed native handshake.
        inventory.revoke(super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared);
        #[cfg(windows)]
        if mondrian_validation_launcher::process_authority().is_some() {
            inventory.admit(super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared);
        }
        Ok(inventory)
    }

    fn build_phase(
        &mut self,
        machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        prepared_start: PreparedEndurancePhaseStart,
    ) -> FreshEndurancePhaseBuild {
        if let Err(error) = self.validate_machine_plan(machine_plan.as_ref()) {
            return FreshEndurancePhaseBuild::rejected(error.to_string());
        }
        self.inner.build_phase(machine_plan, requirement, workload, prepared_start)
    }
}

/// App-consuming Surface/Device reopen result.
pub struct EnduranceSurfaceReopenRun {
    /// Exact App owner returned by the Window event loop.
    pub app_state: AppState,
    /// Sealed product receipt or operation failure.
    pub result: Result<EnduranceRecoveryOperationReceipt, String>,
    /// Canonical outer Window-run receipt retained alongside the compatibility receipt.
    pub window_receipt: Option<(String, String)>,
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

    /// Consume the process-local Surface/EventLoop driver after all phase owners.
    fn shutdown(
        self,
    ) -> Result<EnduranceSurfaceDriverShutdownEvidence, EnduranceRunOwnerShutdownFailure>;
}

/// Typed final closure returned by one consumed Surface reopen driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnduranceSurfaceDriverShutdownEvidence {
    /// This driver owned no physical Surface or EventLoop authority.
    NotApplicable,
    /// The process-local Rust EventLoop owner was dropped and returned.
    EventLoop(crate::app_ui::window::AppUiEventLoopShutdownReceipt),
}

impl EnduranceSurfaceDriverShutdownEvidence {
    fn into_run_owner_closure(self) -> EnduranceRunOwnerClosureEvidence {
        match self {
            Self::NotApplicable => EnduranceRunOwnerClosureEvidence::not_applicable(),
            Self::EventLoop(receipt) => {
                EnduranceRunOwnerClosureEvidence::event_loop(receipt.owner_closure())
            }
        }
    }
}

/// Production winit Window driver. It is intentionally not `Send` and owns
/// exactly one process-local event loop across every recovery cycle.
pub struct WindowEnduranceSurfaceReopenDriver {
    event_loop: crate::app_ui::window::AppUiReusableEventLoop,
    _main_thread: PhantomData<Rc<()>>,
}

impl WindowEnduranceSurfaceReopenDriver {
    /// Create the process-local event loop before campaign admission.
    pub fn new() -> Result<Self, crate::app_ui::window::AppUiEventLoopConstructionFailure> {
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
        // The Window receipt Module is the sole qualification and sealing authority.
        let outer_evidence_returned = run.shutdown.is_some();
        let outer_authority_released = run
            .shutdown
            .as_ref()
            .is_some_and(|evidence| evidence.all_owned_authority_released());
        let (result, window_receipt) = match run.result {
            Ok(receipt) if receipt.all_owned_authority_released() => {
                let outer = Some((
                    receipt.canonical_json().to_owned(),
                    receipt.sha256().to_owned(),
                ));
                (Ok(receipt.into_recovery_receipt()), outer)
            }
            Ok(_) => (
                Err("Window Surface recovery receipt lost owned authority".to_owned()),
                None,
            ),
            Err(primary) if outer_authority_released => (Err(primary), None),
            Err(primary) if outer_evidence_returned => (
                Err(format!(
                    "{primary}; Window Surface recovery outer shutdown was incomplete"
                )),
                None,
            ),
            Err(primary) => (
                Err(format!(
                    "{primary}; Window Surface recovery returned no typed outer shutdown evidence"
                )),
                None,
            ),
        };
        EnduranceSurfaceReopenRun {
            app_state: run.app_state,
            result,
            window_receipt,
            recovery_pump: run.recovery_pump.expect("Window driver supplied a recovery pump"),
        }
    }

    fn shutdown(
        self,
    ) -> Result<EnduranceSurfaceDriverShutdownEvidence, EnduranceRunOwnerShutdownFailure> {
        let evidence = self.event_loop.shutdown();
        let receipt = crate::app_ui::window::AppUiEventLoopShutdownReceipt::seal(evidence)
            .map_err(|error| EnduranceRunOwnerShutdownFailure::new(error.to_string()))?;
        Ok(EnduranceSurfaceDriverShutdownEvidence::EventLoop(receipt))
    }
}

enum RuntimeState {
    Empty,
    Owned(Box<PhaseOwners>),
    Terminal {
        snapshot: Option<Box<EnduranceRuntimeSnapshot>>,
        evidence: Option<Box<EndurancePhaseTerminalEvidence>>,
    },
}

struct PhaseOwners {
    prepared_export: Option<super::endurance_export::PreparedFrozenRepeatedExportPhase>,
    measurement_timing: Option<mondrian_platform::EndurancePhaseMeasurementTiming>,
    startup_failure: Option<super::endurance_campaign::EnduranceExecutionStartFailure>,
    kind: EndurancePhaseKind,
    phase_id: String,
    app: Option<AppState>,
    authority: Option<PreparedEndurancePhaseAuthority>,
    execution: Option<EnduranceExecutionOwners>,
    timeline: Option<PersistentTimelinePlaybackPhase>,
    reference: Option<PersistentReferenceOutputPump>,
    export: Option<FrozenRepeatedExportPhase>,
    reference_plan: Option<EnduranceReferenceOutputPlan>,
    export_request: Option<FrozenRepeatedExportRequest>,
    bmx_runtime: Option<mondrian_media::PreparedBmxRuntime>,
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
        machine_plan: &Arc<PreparedCommercialEnduranceMachinePlan>,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        phase_started_us: u64,
        fresh: FreshEndurancePhase,
    ) -> Result<Box<Self>, (Box<Self>, String)> {
        let actual = fresh.kind();
        let authority_failure = if !Arc::ptr_eq(machine_plan, fresh.authority.machine_plan()) {
            Some("fresh endurance phase did not retain the runtime-bound machine plan".to_owned())
        } else {
            fresh
                .authority
                .validate_current(&fresh.app_state)
                .err()
                .map(|error| format!("fresh endurance source authority is stale: {error}"))
        };
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
            #[cfg(test)]
            FreshEndurancePhaseInputs::Test(_) => (None, None, VecDeque::new()),
        };
        let ancillary_matches = reference_plan.as_ref().is_none_or(|plan| {
            super::endurance_ancillary::same_program(
                machine_plan.ancillary_program(),
                plan.ancillary_program.as_ref(),
            )
        }) && export_request.as_ref().is_none_or(|plan| {
            plan.phase_id == requirement.phase_id
                && super::endurance_ancillary::same_program(
                    machine_plan.ancillary_program(),
                    plan.frozen_ancillary.as_ref(),
                )
        });
        let owners = Self {
            prepared_export: None,
            measurement_timing: None,
            startup_failure: None,
            kind: requirement.kind,
            phase_id: requirement.phase_id.clone(),
            app: Some(fresh.app_state),
            authority: Some(fresh.authority),
            execution: None,
            timeline: None,
            reference: None,
            export: None,
            reference_plan,
            export_request,
            bmx_runtime: None,
            seek_targets,
            phase_started_us,
            minimum_duration_us: requirement.minimum_duration_us,
            recovery_cycle_count: workload.recovery_cycle_count(),
            next_recovery_cycle: 0,
            settled: false,
            fault: None,
        };
        if !ancillary_matches {
            return Err((
                Box::new(owners),
                "phase ANC owner differs from retained machine-plan source".to_owned(),
            ));
        }
        if let Some(detail) = authority_failure {
            return Err((Box::new(owners), detail));
        }
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
        phase_id: String,
        kind: EndurancePhaseKind,
        app_state: AppState,
        authority: Option<PreparedEndurancePhaseAuthority>,
        phase_started_us: u64,
        minimum_duration_us: u64,
        recovery_cycle_count: u32,
        detail: String,
    ) -> Self {
        Self {
            phase_id,
            prepared_export: None,
            measurement_timing: None,
            kind,
            startup_failure: None,
            app: Some(app_state),
            authority,
            execution: None,
            timeline: None,
            reference: None,
            export: None,
            reference_plan: None,
            export_request: None,
            bmx_runtime: None,
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
        phase_deadline: Instant,
    ) -> Result<(), String> {
        if let (Some(authority), Some(app)) = (&self.authority, &self.app) {
            authority
                .validate_current(app)
                .map_err(|error| format!("endurance source authority is stale: {error}"))?;
        }
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
                self.install_execution_result(EnduranceExecutionOwners::start(self.app_ref()?))?;
                let app = self.app.as_mut().ok_or("phase App owner is missing")?;
                let execution = self.execution.as_mut().ok_or("execution owner disappeared")?;
                let timeline = PersistentTimelinePlaybackPhase::start(
                    app,
                    execution,
                    requirement,
                    workload,
                    Some(phase_deadline),
                    timeouts.interval,
                )
                .map_err(|error| error.to_string())?;
                self.timeline = Some(timeline);

                let plan = self
                    .reference_plan
                    .take()
                    .ok_or("realtime endurance phase is missing Reference Output composition")?;
                let mut reference = PersistentReferenceOutputPump::prepare(
                    self.app_ref()?,
                    plan.request,
                    plan.first_frame_index,
                )
                .map_err(|error| error.to_string())?;
                reference
                    .set_wire_correlation(plan.wire_correlation)
                    .map_err(|error| error.to_string())?;
                reference
                    .set_ancillary_program(plan.ancillary_program)
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
            let export = super::endurance_export::PreparedFrozenRepeatedExportPhase::prepare(
                self.app_ref()?,
                request,
            )
            .map_err(|error| error.to_string())?;
            self.prepared_export = Some(export);
        }
        if let (Some(timeline), Some(app), Some(execution)) =
            (&mut self.timeline, &mut self.app, &mut self.execution)
        {
            timeline
                .refresh_pre_measurement_picture(app, execution, phase_deadline)
                .map_err(|error| error.to_string())?;
            self.settled = true;
        }
        Ok(())
    }

    fn app_ref(&self) -> Result<&AppState, String> {
        self.app.as_ref().ok_or_else(|| "phase App owner is missing".to_owned())
    }

    fn install_execution_result(
        &mut self,
        result: Result<
            EnduranceExecutionOwners,
            super::endurance_campaign::EnduranceExecutionStartFailure,
        >,
    ) -> Result<(), String> {
        match result {
            Ok(execution) => {
                self.execution = Some(execution);
                Ok(())
            }
            Err(failure) => {
                let detail = format!("start execution owners: {:#}", failure.diagnostic());
                self.startup_failure = Some(failure);
                Err(detail)
            }
        }
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
        let app = self.app.as_mut().ok_or("phase App owner is missing")?;
        let execution = self.execution.as_mut().ok_or("execution owner is missing")?;
        self.timeline
            .as_mut()
            .ok_or("Timeline owner is missing")?
            .resume_audio_device_window(app, execution, Some(deadline))
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
            self.app.as_mut().ok_or("phase App owner is missing")?.poll_export_queue();
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
        let (window_receipt_json, window_receipt_sha256) =
            run.window_receipt.ok_or("Window Surface recovery lost its outer run receipt")?;
        events.push(EnduranceCampaignEvent::window_recovery_step_completed(
            self.phase_elapsed_us(clock)?,
            &surface_receipt,
            window_receipt_json,
            window_receipt_sha256,
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
            EndurancePhaseKind::PlaybackReference | EndurancePhaseKind::ConcurrentRecovery => self
                .execution
                .as_ref()
                .ok_or("execution owner is missing")?
                .runtime_snapshot(app, self.kind)
                .map_err(|error| error.to_string()),
        }
    }
}

/// Concrete runtime over fresh App owners and the production phase drivers.
#[derive(Debug, Clone, Copy, serde::Serialize)]
struct EndurancePhaseStartupTiming {
    startup_started_at_run_us: u64,
    startup_deadline_at_run_us: u64,
    owners_ready_at_run_us: Option<u64>,
}

pub(crate) struct ProductEnduranceCampaignRuntime<F, S, C> {
    phase_startup: Option<EndurancePhaseStartupTiming>,
    startup_deadline: Option<Instant>,
    phase_hard_deadline: Option<Instant>,
    terminal_measurement: Option<mondrian_platform::EndurancePhaseMeasurementTiming>,
    state: RuntimeState,
    surface: Option<S>,
    run_owner_closure: Option<EnduranceRunOwnerClosureEvidence>,
    run_owner_shutdown_failure: Option<EnduranceRunOwnerShutdownFailure>,
    factory: F,
    clock: Arc<C>,
    timeouts: Option<EnduranceProductRuntimeTimeouts>,
    machine_plan: Option<Arc<PreparedCommercialEnduranceMachinePlan>>,
    owner_report_location: Option<(String, std::path::PathBuf)>,
    phase_owner_history: Vec<mondrian_platform::EndurancePhaseOwnerReceipt>,
    terminal_ancillary: Option<mondrian_platform::EnduranceAncillaryPhaseEvidence>,
    regulatory_pse_prerequisites:
        Vec<super::endurance_source_inventory::PreparedEndurancePsePrerequisite>,
    pending_bmx: Option<mondrian_media::PreparedBmxRuntime>,
    bmx_pre_start_closures: Vec<mondrian_media::ApprovedProviderRuntimeCleanupReceipt>,
    bmx_pre_start_failures: Vec<super::endurance_source_inventory::EnduranceBmxPrepareFailure>,
    #[cfg(test)]
    phase_close_checkpoint: Option<fn()>,
}

impl<F, S, C> ProductEnduranceCampaignRuntime<F, S, C> {
    fn new(factory: F, surface: S, clock: Arc<C>) -> Self {
        Self {
            phase_startup: None,
            startup_deadline: None,
            phase_hard_deadline: None,
            terminal_measurement: None,
            state: RuntimeState::Empty,
            surface: Some(surface),
            run_owner_closure: None,
            run_owner_shutdown_failure: None,
            factory,
            clock,
            timeouts: None,
            machine_plan: None,
            owner_report_location: None,
            phase_owner_history: Vec::new(),
            terminal_ancillary: None,
            regulatory_pse_prerequisites: Vec::new(),
            pending_bmx: None,
            bmx_pre_start_closures: Vec::new(),
            bmx_pre_start_failures: Vec::new(),
            #[cfg(test)]
            phase_close_checkpoint: None,
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
    factory: PreparedEnduranceMachinePhaseFactory<F>,
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
    let mut runtime = ProductEnduranceCampaignRuntime::new(factory, surface, Arc::clone(&clock));
    runtime.owner_report_location = Some((
        prepared_request.request().identity.run_id.clone(),
        prepared_request.request().evidence_directory.clone(),
    ));
    let result = runtime.run_with_terminal_evidence(|runtime| {
        runtime.factory.validate_machine_plan(prepared_request.machine_plan())?;
        let request = prepared_request.into_campaign_request();
        run_endurance_campaign(request, runtime, process_memory, clock.as_ref())
    });
    let result = runtime.finish_run(result);
    runtime.publish_failed_run(result)
}

impl<F, S, C> ProductEnduranceCampaignRuntime<F, S, C> {
    fn publish_phase_owner_receipt(
        &mut self,
        phase_id: &str,
    ) -> Result<(), EnduranceCampaignError> {
        use sha2::{Digest, Sha256};
        let Some((run_id, directory)) = &self.owner_report_location else {
            return Ok(());
        };
        let RuntimeState::Terminal { evidence: Some(terminal), .. } = &self.state else {
            return Err(runtime_error(
                "phase publication lost its actual terminal owner receipt",
            ));
        };
        if self.phase_owner_history.len() >= 8
            || self.phase_owner_history.iter().any(|receipt| receipt.phase_id == phase_id)
        {
            return Err(runtime_error(
                "phase owner history exceeds its bounded unique inventory",
            ));
        }
        let mut report = serde_json::json!({
            "schema_version": 2, "run_id": run_id, "phase_id": phase_id,
            "ordinal": self.phase_owner_history.len(), "terminal": terminal,
        });
        if let Some(timing) = self.terminal_measurement {
            report["measurement_timing"] =
                serde_json::to_value(timing).map_err(|error| runtime_error(error.to_string()))?;
        }
        if let Some(ancillary) = &self.terminal_ancillary {
            let fields = serde_json::to_value(ancillary)
                .map_err(|error| runtime_error(error.to_string()))?;
            let fields = fields
                .as_object()
                .ok_or_else(|| runtime_error("ANC evidence omitted its typed object"))?;
            report
                .as_object_mut()
                .ok_or_else(|| runtime_error("phase owner report omitted its object"))?
                .extend(fields.clone());
        }
        let canonical_json =
            serde_json::to_string(&report).map_err(|error| runtime_error(error.to_string()))?;
        let path = directory.join(format!(
            "phase-owner-{:02}.json",
            self.phase_owner_history.len()
        ));
        let receipt = mondrian_platform::EndurancePhaseOwnerReceipt {
            phase_id: phase_id.to_owned(),
            report_path: path.to_string_lossy().into_owned(),
            sha256: format!("{:x}", Sha256::digest(canonical_json.as_bytes())),
            canonical_json,
        };
        // Retain the exact attempted publication even if the filesystem rejects it.
        self.phase_owner_history.push(receipt.clone());
        super::endurance_qualification::write_json_create_new(&path, &receipt)?;
        Ok(())
    }

    fn publish_failed_run<T>(
        &self,
        result: Result<T, EnduranceCampaignError>,
    ) -> Result<T, EnduranceCampaignError> {
        let primary = match result {
            Ok(value) => return Ok(value),
            Err(primary) => primary,
        };
        let Some((run_id, directory)) = &self.owner_report_location else {
            return Err(primary);
        };
        let current_terminal =
            terminal_receipt_from_error(&primary).or_else(|| match &self.state {
                RuntimeState::Terminal { evidence: Some(terminal), .. } => Some(terminal.as_ref()),
                _ => None,
            });
        let report = serde_json::json!({
            "schema_version": 1, "qualifying": false, "run_id": run_id,
            "diagnostic": primary.to_string(), "phase_owner_history": self.phase_owner_history,
            "current_terminal": current_terminal, "run_owner_closure": self.run_owner_closure,
            "phase_startup": self.phase_startup,
            "run_owner_shutdown_failure": self.run_owner_shutdown_failure,
            "bmx_pre_start_closures": self.bmx_pre_start_closures,
            "bmx_pre_start_failures": self.bmx_pre_start_failures,
        });
        let path = directory.join("run-failure.json");
        match super::endurance_qualification::write_json_create_new(&path, &report) {
            Ok(()) => Err(EnduranceCampaignError::WithFailureReport {
                primary: Box::new(primary),
                report_path: path.to_string_lossy().into_owned(),
            }),
            Err(error) => Err(EnduranceCampaignError::FailureReportPublication {
                primary: Box::new(primary),
                canonical_report_json: report.to_string(),
                publication: error.to_string(),
            }),
        }
    }
    /// Attach the current receipt once, including failures after successful cleanup.
    fn run_with_terminal_evidence<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, EnduranceCampaignError>,
    ) -> Result<T, EnduranceCampaignError>
    where
        F: FreshEndurancePhaseFactory,
        S: EnduranceSurfaceReopenDriver,
        C: EnduranceCampaignClock,
    {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)))
            .unwrap_or_else(|payload| {
                Err(runtime_error(
                    super::execution_panic_diagnostic::execution_panic_diagnostic(
                        payload,
                        "endurance campaign operation",
                    )
                    .to_string(),
                ))
            });
        let result = result.map_err(|primary| {
            if matches!(self.state, RuntimeState::Owned(_)) {
                super::endurance_campaign::cleanup_started_phase(self, primary)
            } else {
                primary
            }
        });
        result.map_err(|primary| {
            let RuntimeState::Terminal { evidence, .. } = &mut self.state else {
                return primary;
            };
            match evidence.take() {
                Some(terminal) => EnduranceCampaignError::WithTerminalEvidence {
                    primary: Box::new(primary),
                    terminal,
                },
                None => primary,
            }
        })
    }

    fn shutdown_run_owner(
        &mut self,
    ) -> Result<EnduranceRunOwnerClosureEvidence, EnduranceCampaignError>
    where
        S: EnduranceSurfaceReopenDriver,
        F: FreshEndurancePhaseFactory,
    {
        if matches!(self.state, RuntimeState::Owned(_)) {
            return Err(runtime_error(
                "cannot close the campaign Surface driver while phase owners remain",
            ));
        }
        self.regulatory_pse_prerequisites.clear();
        if let Some(closure) = &self.run_owner_closure {
            return Ok(closure.clone());
        }
        if let Some(failure) = &self.run_owner_shutdown_failure {
            return Err(EnduranceCampaignError::RunOwnerShutdown(failure.clone()));
        }
        let surface = self.surface.take().ok_or_else(|| {
            runtime_error("campaign Surface driver was consumed without terminal evidence")
        })?;
        let deadline = Instant::now()
            + self.timeouts.map_or(Duration::from_secs(5), |timeouts| timeouts.shutdown);
        let surface_result = surface.shutdown();
        if let Some(owner) = self.pending_bmx.take() {
            self.bmx_pre_start_closures.push(owner.close_until(deadline));
        }
        let ffmpeg = self.factory.shutdown_ffmpeg_capsule(deadline);
        let result = match surface_result {
            Ok(evidence) => {
                let surface = evidence.into_run_owner_closure();
                if let Some(ffmpeg) = ffmpeg {
                    let closure = EnduranceRunOwnerClosureEvidence::WithFfmpeg {
                        surface: Box::new(surface),
                        ffmpeg,
                    };
                    if closure.all_owned_authority_released() {
                        Ok(closure)
                    } else {
                        Err(EnduranceRunOwnerShutdownFailure::with_closure(
                            "campaign exact-runtime closure was incomplete",
                            closure,
                        ))
                    }
                } else if surface.all_owned_authority_released() {
                    Ok(surface)
                } else {
                    Err(EnduranceRunOwnerShutdownFailure::with_closure(
                        "campaign Surface driver returned incomplete owner closure",
                        surface,
                    ))
                }
            }
            Err(mut failure) => {
                if let Some(ffmpeg) = ffmpeg {
                    failure.attach_ffmpeg_closure(ffmpeg);
                }
                Err(failure)
            }
        };
        let result = result.and_then(|closure| {
            if self.bmx_pre_start_closures.iter().any(|receipt| !receipt.all_resources_released())
                || self.bmx_pre_start_failures.iter().any(|failure| failure.cleanup.as_ref().is_some_and(|receipt| !receipt.all_resources_released()))
            {
                Err(EnduranceRunOwnerShutdownFailure::with_closure(
                    "campaign BMX pre-start owner closure was incomplete; raw receipts retained in failure report", closure,
                ))
            } else { Ok(closure) }
        });
        match result {
            Ok(closure) => {
                self.run_owner_closure = Some(closure.clone());
                Ok(closure)
            }
            Err(failure) => {
                self.run_owner_shutdown_failure = Some(failure.clone());
                Err(EnduranceCampaignError::RunOwnerShutdown(failure))
            }
        }
    }
    fn finish_run<T>(
        &mut self,
        result: Result<T, EnduranceCampaignError>,
    ) -> Result<T, EnduranceCampaignError>
    where
        S: EnduranceSurfaceReopenDriver,
        F: FreshEndurancePhaseFactory,
    {
        match result {
            Ok(value) if self.run_owner_closure.is_some() => Ok(value),
            Ok(_) => {
                let primary = runtime_error(
                    "campaign operation completed before run-owner closure was captured",
                );
                match self.shutdown_run_owner() {
                    Ok(closure) => Err(EnduranceCampaignError::WithRunOwnerClosureEvidence {
                        primary: Box::new(primary),
                        closure,
                    }),
                    Err(EnduranceCampaignError::RunOwnerShutdown(shutdown)) => {
                        Err(EnduranceCampaignError::RunOwnerShutdownAfterFailure {
                            primary: Box::new(primary),
                            shutdown,
                        })
                    }
                    Err(shutdown) => Err(EnduranceCampaignError::StartedPhaseCleanup {
                        primary: Box::new(primary),
                        cleanup: Box::new(shutdown),
                    }),
                }
            }
            Err(primary) => {
                if let Some(closure) = &self.run_owner_closure {
                    return Err(EnduranceCampaignError::WithRunOwnerClosureEvidence {
                        primary: Box::new(primary),
                        closure: closure.clone(),
                    });
                }
                if let Some(shutdown) = &self.run_owner_shutdown_failure {
                    return if retains_run_owner_shutdown(&primary) {
                        Err(primary)
                    } else {
                        Err(EnduranceCampaignError::RunOwnerShutdownAfterFailure {
                            primary: Box::new(primary),
                            shutdown: shutdown.clone(),
                        })
                    };
                }
                match self.shutdown_run_owner() {
                    Ok(closure) => Err(EnduranceCampaignError::WithRunOwnerClosureEvidence {
                        primary: Box::new(primary),
                        closure,
                    }),
                    Err(EnduranceCampaignError::RunOwnerShutdown(shutdown)) => {
                        Err(EnduranceCampaignError::RunOwnerShutdownAfterFailure {
                            primary: Box::new(primary),
                            shutdown,
                        })
                    }
                    Err(shutdown) => Err(EnduranceCampaignError::StartedPhaseCleanup {
                        primary: Box::new(primary),
                        cleanup: Box::new(shutdown),
                    }),
                }
            }
        }
    }
}

impl<F, S, C> EnduranceCampaignRuntime for ProductEnduranceCampaignRuntime<F, S, C>
where
    F: FreshEndurancePhaseFactory,
    S: EnduranceSurfaceReopenDriver,
    C: EnduranceCampaignClock,
{
    fn phase_owner_history(&self) -> Vec<mondrian_platform::EndurancePhaseOwnerReceipt> {
        self.phase_owner_history.clone()
    }
    fn phase_ancillary_evidence(
        &self,
    ) -> Option<mondrian_platform::EnduranceAncillaryPhaseEvidence> {
        self.terminal_ancillary.clone()
    }
    fn shutdown_run_owner(
        &mut self,
    ) -> Result<EnduranceRunOwnerClosureEvidence, EnduranceCampaignError> {
        ProductEnduranceCampaignRuntime::shutdown_run_owner(self)
    }

    fn begin_phase_preparation(&mut self) {
        self.phase_startup = None;
        self.startup_deadline = None;
        self.phase_hard_deadline = None;
        self.terminal_measurement = None;
        self.terminal_ancillary = None;
        if let RuntimeState::Terminal { evidence, .. } = &mut self.state {
            *evidence = None;
        }
    }

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
        let mut timeouts = EnduranceProductRuntimeTimeouts::new(
            Duration::from_millis(planned.interval_ms),
            Duration::from_millis(planned.recovery_ms),
            Duration::from_millis(planned.surface_reopen_ms),
            Duration::from_millis(planned.shutdown_ms),
        )?;
        timeouts.startup = Duration::from_millis(planned.startup_ms);
        self.machine_plan = Some(Arc::new(machine_plan));
        self.timeouts = Some(timeouts);
        Ok(())
    }

    fn begin_phase(
        &mut self,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        phase_started_at_run_us: u64,
    ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError> {
        if self.pending_bmx.is_some()
            || self.bmx_pre_start_failures.len() >= 16
            || self.bmx_pre_start_closures.len() >= 16
        {
            return Err(runtime_error(
                "BMX admission retained a prior owner or exhausted bounded failure history",
            ));
        }
        match self.state {
            RuntimeState::Owned(_) => {
                return Err(runtime_error(
                    "cannot begin an endurance phase while another phase owns resources",
                ));
            }
            RuntimeState::Terminal { snapshot: None, .. } => {
                return Err(runtime_error(
                    "cannot begin after terminal snapshot construction failed",
                ));
            }
            RuntimeState::Empty | RuntimeState::Terminal { snapshot: Some(_), .. } => {}
        }
        self.state = RuntimeState::Empty;
        let timeouts = self.timeouts.ok_or_else(|| {
            runtime_error("commercial endurance machine-plan timeouts are missing")
        })?;
        let machine_plan = Arc::clone(
            self.machine_plan
                .as_ref()
                .ok_or_else(|| runtime_error("commercial endurance machine plan is not bound"))?,
        );

        // Startup and measured work are separate leases. Freeze the original
        // startup limit and maximum capsule lifetime before any factory work.
        let startup_us = u64::try_from(timeouts.startup.as_micros())
            .map_err(|_| runtime_error("startup budget overflow"))?;
        let startup_end_run_us = phase_started_at_run_us
            .checked_add(startup_us)
            .ok_or_else(|| runtime_error("startup clock coordinate overflow"))?;
        let startup_deadline = Instant::now()
            .checked_add(Duration::from_micros(
                startup_end_run_us.saturating_sub(self.clock.elapsed_us()),
            ))
            .ok_or_else(|| runtime_error("startup absolute deadline overflow"))?;
        let phase_deadline = startup_deadline
            .checked_add(Duration::from_micros(requirement.minimum_duration_us))
            .and_then(|deadline| deadline.checked_add(timeouts.shutdown))
            .ok_or_else(|| runtime_error("phase capsule horizon overflow"))?;
        self.phase_startup = Some(EndurancePhaseStartupTiming {
            startup_started_at_run_us: phase_started_at_run_us,
            startup_deadline_at_run_us: startup_end_run_us,
            owners_ready_at_run_us: None,
        });
        self.startup_deadline = Some(startup_deadline);
        self.phase_hard_deadline = Some(phase_deadline);
        if Instant::now() >= startup_deadline || self.clock.elapsed_us() >= startup_end_run_us {
            return Err(EnduranceCampaignError::PreStartRuntime(
                "startup deadline elapsed before factory admission".to_owned(),
            ));
        }

        if workload.program_frame_rate().is_some_and(|rate| {
            rate != machine_plan.plan().reference_output.open_request.signal.frame_rate
        }) {
            return Err(EnduranceCampaignError::PreStartRuntime(
                "machine Reference cadence differs from the workload".to_owned(),
            ));
        }
        if machine_plan.ancillary_program_missing() {
            return Ok(EndurancePhaseAdmission::NotRun(
                super::endurance_workload::EnduranceNotRunAdmission::missing_ancillary(
                    requirement.phase_id.clone(),
                    workload.workload_id().to_owned(),
                ),
            ));
        }
        let inventory = self
            .factory
            .pre_start_capability_inventory(machine_plan.as_ref(), requirement, workload)
            .map_err(|error| EnduranceCampaignError::PreStartRuntime(error.to_string()))?;
        let prepared_start = match workload.prepare_start(&inventory) {
            Ok(prepared_start) => prepared_start,
            Err(not_run) => return Ok(EndurancePhaseAdmission::NotRun(not_run)),
        };

        if let Some(prerequisite) =
            super::endurance_source_inventory::prepare_regulatory_pse_prerequisite(
                machine_plan.as_ref(),
                &requirement.phase_id,
            )
            .map_err(EnduranceCampaignError::PreStartRuntime)?
        {
            if matches!(
                prerequisite.outcome,
                mondrian_export::RegulatoryPseAdmission::NotRun(_)
            ) {
                return Ok(EndurancePhaseAdmission::NotRun(
                    super::endurance_workload::EnduranceNotRunAdmission::missing_regulatory_pse(
                        requirement.phase_id.clone(),
                        workload.workload_id().to_owned(),
                    ),
                ));
            }
            if self.regulatory_pse_prerequisites.len() >= 16 {
                return Err(EnduranceCampaignError::PreStartRuntime(
                    "PSE preparation history exceeds the bounded phase inventory".to_owned(),
                ));
            }
            self.regulatory_pse_prerequisites.push(prerequisite);
        }
        match super::endurance_source_inventory::prepare_bmx_prerequisite(
            machine_plan.as_ref(),
            &requirement.phase_id,
            startup_deadline,
            phase_deadline,
            &mondrian_core::ExecutionCancellationToken::new(),
        ) {
            Ok(super::endurance_source_inventory::EnduranceBmxAdmission::NotRequired) => {}
            Ok(super::endurance_source_inventory::EnduranceBmxAdmission::NotRun) => {
                return Ok(EndurancePhaseAdmission::NotRun(
                    super::endurance_workload::EnduranceNotRunAdmission::missing_bmx(
                        requirement.phase_id.clone(),
                        workload.workload_id().to_owned(),
                    ),
                ));
            }
            Ok(super::endurance_source_inventory::EnduranceBmxAdmission::Available(owner)) => {
                self.pending_bmx = Some(owner)
            }
            Err(failure) => {
                let detail = failure.detail.clone();
                self.bmx_pre_start_failures.push(failure);
                return Err(EnduranceCampaignError::PreStartRuntime(detail));
            }
        }
        let build = self.factory.build_phase(
            Arc::clone(&machine_plan),
            requirement,
            workload,
            prepared_start,
        );
        let mut owners = match build {
            FreshEndurancePhaseBuild::Rejected { detail } => {
                if let Some(owner) = self.pending_bmx.take() {
                    self.bmx_pre_start_closures.push(owner.close_until(phase_deadline));
                }
                return Err(EnduranceCampaignError::PreStartRuntime(detail));
            }
            FreshEndurancePhaseBuild::Ready(fresh) => {
                match PhaseOwners::from_fresh(
                    &machine_plan,
                    requirement,
                    workload,
                    phase_started_at_run_us,
                    *fresh,
                ) {
                    Ok(owners) => owners,
                    Err((mut owners, detail)) => {
                        owners.bmx_runtime = self.pending_bmx.take();
                        owners.fault = Some(detail.clone());
                        self.state = RuntimeState::Owned(owners);
                        return Err(runtime_error(detail));
                    }
                }
            }
            FreshEndurancePhaseBuild::Failed { app_state, authority, detail } => {
                let mut owners = PhaseOwners::failed_before_start(
                    requirement.phase_id.clone(),
                    requirement.kind,
                    *app_state,
                    authority.map(|authority| *authority),
                    phase_started_at_run_us,
                    requirement.minimum_duration_us,
                    workload.recovery_cycle_count(),
                    detail.clone(),
                );
                owners.bmx_runtime = self.pending_bmx.take();
                self.state = RuntimeState::Owned(Box::new(owners));
                return Err(runtime_error(detail));
            }
        };
        owners.bmx_runtime = self.pending_bmx.take();
        if let Some(request) = &mut owners.export_request {
            request.approved_bmx = owners.bmx_runtime.as_ref().map(|owner| owner.handle());
        }
        if Instant::now() >= startup_deadline || self.clock.elapsed_us() >= startup_end_run_us {
            owners.fault =
                Some("cold factory preparation exceeded the original startup lease".to_owned());
            self.state = RuntimeState::Owned(owners);
            return Err(runtime_error(
                "cold factory preparation exceeded the original startup lease",
            ));
        }
        if let Err(detail) = owners.start(requirement, workload, timeouts, startup_deadline) {
            owners.fault = Some(detail.clone());
            self.state = RuntimeState::Owned(owners);
            return Err(runtime_error(detail));
        }
        let ready_at = self.clock.elapsed_us();
        if ready_at >= startup_end_run_us || Instant::now() >= startup_deadline {
            owners.fault = Some("phase startup exceeded its original deadline".to_owned());
            self.state = RuntimeState::Owned(owners);
            return Err(runtime_error(
                "phase startup exceeded its original deadline",
            ));
        }
        if let Some(startup) = &mut self.phase_startup {
            startup.owners_ready_at_run_us = Some(ready_at);
        }
        self.state = RuntimeState::Owned(owners);
        Ok(EndurancePhaseAdmission::Started)
    }

    fn begin_measurement(
        &mut self,
        requirement: &EndurancePhaseRequirement,
        measurement_started_at_run_us: u64,
    ) -> Result<mondrian_platform::EndurancePhaseMeasurementTiming, EnduranceCampaignError> {
        let startup = self.phase_startup.ok_or_else(|| runtime_error("missing startup lease"))?;
        let owners_ready_at_run_us = startup
            .owners_ready_at_run_us
            .ok_or_else(|| runtime_error("phase owners have not completed cold preparation"))?;
        let timing = mondrian_platform::EndurancePhaseMeasurementTiming {
            startup_started_at_run_us: startup.startup_started_at_run_us,
            startup_deadline_at_run_us: startup.startup_deadline_at_run_us,
            owners_ready_at_run_us,
            measurement_started_at_run_us,
            measurement_deadline_at_run_us: measurement_started_at_run_us
                .checked_add(requirement.minimum_duration_us)
                .ok_or_else(|| runtime_error("measurement clock horizon overflow"))?,
        };
        let startup_deadline =
            self.startup_deadline.ok_or_else(|| runtime_error("missing startup deadline"))?;
        if !timing.validates()
            || Instant::now() >= startup_deadline
            || self.clock.elapsed_us() >= timing.startup_deadline_at_run_us
            || measurement_started_at_run_us > self.clock.elapsed_us()
        {
            return Err(runtime_error(
                "measurement activation exceeded the original startup lease",
            ));
        }
        let timeouts = self.timeouts.ok_or_else(|| runtime_error("missing phase timeouts"))?;
        let horizon = Instant::now()
            .checked_add(Duration::from_micros(
                timing.measurement_deadline_at_run_us.saturating_sub(self.clock.elapsed_us()),
            ))
            .and_then(|deadline| deadline.checked_add(timeouts.shutdown))
            .ok_or_else(|| runtime_error("measurement close horizon overflow"))?
            .min(
                self.phase_hard_deadline
                    .ok_or_else(|| runtime_error("missing original capsule horizon"))?,
            );
        let RuntimeState::Owned(owners) = &mut self.state else {
            return Err(runtime_error("measurement activation has no phase owners"));
        };
        if owners.measurement_timing.is_some()
            || owners.phase_id != requirement.phase_id
            || owners.minimum_duration_us != requirement.minimum_duration_us
            || owners.fault.is_some()
        {
            return Err(runtime_error(
                "measurement activation is duplicate or has incompatible owners",
            ));
        }
        if owners.timeline.is_some() {
            let app = owners.app_ref().map_err(runtime_error)?;
            let remaining = app
                .last_content_frame()
                .map_err(|error| runtime_error(error.to_string()))?
                .checked_sub(app.current_frame())
                .and_then(|value| u64::try_from(value).ok());
            if remaining.is_none_or(|frames| {
                frames < requirement.counters.minimum_playback_presented_frames
            }) {
                return Err(runtime_error("Timeline fixture lacks the complete measured extent after Audio startup; extend the actual fixture before admission"));
            }
        }
        owners.phase_started_us = measurement_started_at_run_us;
        owners.measurement_timing = Some(timing);
        if let Some(prepared) = owners.prepared_export.take() {
            match prepared.activate_until(horizon) {
                Ok(export) => {
                    owners.export = Some(export);
                    // Export activation changes the App-owned queue after the
                    // startup resource snapshot. Publish that exact demand now
                    // so Concurrent Recovery can receive its bounded realtime slot.
                    owners
                        .app_ref()
                        .map_err(runtime_error)?
                        .refresh_internal_execution_resource_decision();
                }
                Err(error) => {
                    owners.fault = Some(error.to_string());
                    return Err(runtime_error(error.to_string()));
                }
            }
        }
        if Instant::now() >= startup_deadline
            || self.clock.elapsed_us() >= timing.startup_deadline_at_run_us
        {
            owners.fault = Some("Export activation exceeded the original startup lease".to_owned());
            return Err(runtime_error(
                "Export activation exceeded the original startup lease",
            ));
        }
        // Retire the startup driver (not the paired Preview/GPU owners). Its
        // absolute deadline must never be renewed into the measured residency.
        owners.settle().map_err(runtime_error)?;
        Ok(timing)
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
            let surface = self.surface.as_mut().ok_or_else(|| {
                runtime_error("campaign Surface driver closed before recovery completed")
            })?;
            match owners.execute_recovery_cycle(surface, self.clock.as_ref(), timeouts) {
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
        Ok(events)
    }

    fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError> {
        match &self.state {
            RuntimeState::Owned(owners) => owners.live_snapshot().map_err(runtime_error),
            RuntimeState::Terminal { snapshot: Some(snapshot), .. } => Ok((**snapshot).clone()),
            RuntimeState::Empty | RuntimeState::Terminal { snapshot: None, .. } => {
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
        let state = std::mem::replace(
            &mut self.state,
            RuntimeState::Terminal { snapshot: None, evidence: None },
        );
        let RuntimeState::Owned(mut owners) = state else {
            self.state = state;
            return Err(runtime_error(
                "no owned endurance phase is available to shut down",
            ));
        };
        // Keep the BMX consuming owner on the runtime during fallible App
        // closure, so even an early return/panic leaves it for run-owner close.
        self.pending_bmx = owners.bmx_runtime.take();
        drop(owners.export_request.take());
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

        // Keep every actual owner outside fallible phase close and verifier work.
        let preparation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if let Some(checkpoint) = self.phase_close_checkpoint.take() {
                checkpoint();
            }
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
                    failures.push(
                        "repeated Export close exceeded the shared shutdown deadline".to_owned(),
                    );
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

            match owners.live_snapshot() {
                Ok(snapshot) => Some(snapshot),
                Err(detail) => {
                    failures.push(detail);
                    None
                }
            }
        }));
        let live_snapshot = match preparation {
            Ok(snapshot) => snapshot,
            Err(payload) => {
                failures.push(
                    super::execution_panic_diagnostic::execution_panic_diagnostic(
                        payload,
                        "endurance phase close preparation",
                    )
                    .to_string(),
                );
                None
            }
        };
        let export_verifier = owners.export.take().map(|export| export.shutdown_until(deadline));
        drop(owners.prepared_export.take());
        if export_verifier
            .as_ref()
            .is_some_and(|receipt| !receipt.all_resources_released())
        {
            failures
                .push("independent Export verifier retained resources or lost evidence".to_owned());
        }
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

        let (terminal_owners, capture_facts, playback_workers_terminated) =
            if let Some(execution) = owners.execution.take() {
                let closure = execution.shutdown_until(app, deadline)?;
                failures.extend(closure.owner_snapshot_failure.clone());
                failures.extend(closure.terminal_projection_failure.clone());
                let capture_facts = closure.capture_facts();
                let workers = closure.all_workers_terminated();
                (
                    EnduranceTerminalOwners::Realtime(Box::new(closure)),
                    capture_facts,
                    workers,
                )
            } else if let Some(failure) = owners.startup_failure.take() {
                let (diagnostic, closure) = failure.shutdown_until(app, deadline);
                failures.push(format!("execution startup failed: {diagnostic:#}"));
                let workers = closure.all_created_resources_released();
                (
                    EnduranceTerminalOwners::Startup(Box::new(closure)),
                    EnduranceCaptureFacts::failed_continuous_export(),
                    workers,
                )
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
                (
                    EnduranceTerminalOwners::AppOnly(Box::new(app_shutdown)),
                    capture_facts,
                    workers,
                )
            };
        let bmx_runtime = self.pending_bmx.take().map(|owner| owner.close_until(deadline));
        if bmx_runtime.as_ref().is_some_and(|receipt| !receipt.all_resources_released()) {
            failures.push("approved BMX runtime consuming closure failed".to_owned());
        }
        let app_shutdown = terminal_owners.app();
        let export_shutdown = app_shutdown.export;
        let supervised_child_processes_remaining =
            supervised_child_processes_remaining(app_shutdown);
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
        // Read only identities minted by the joined Export verifier and the
        // consumed native wire owners; never infer journals by scanning files.
        self.terminal_ancillary = self
            .machine_plan
            .as_ref()
            .and_then(|plan| plan.ancillary_program())
            .map(|program| {
                let exports = program.verified_exports(&owners.phase_id).unwrap_or_else(|error| {
                    failures.push(format!("ANC Export owner inventory failed: {error}"));
                    Vec::new()
                });
                let journals =
                    program.closed_wire_journals(&owners.phase_id).unwrap_or_else(|error| {
                        failures.push(format!("ANC native wire owner inventory failed: {error}"));
                        Vec::new()
                    });
                let evidence = mondrian_platform::EnduranceAncillaryPhaseEvidence {
                    ancillary_program_sha256: program.sha256().to_owned(),
                    ancillary_export_artifacts: exports,
                    wire_journals: journals
                        .into_iter()
                        .map(|journal| mondrian_platform::EnduranceAncillaryWireJournal {
                            path: journal.path,
                            sha256: journal.sha256,
                        })
                        .collect(),
                };
                if !evidence.validates_inventory() {
                    failures.push(
                        "ANC phase owner inventory violates bounded unique identity contract"
                            .to_owned(),
                    );
                }
                evidence
            });
        self.terminal_measurement = owners.measurement_timing;
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
        let phase_id = owners.phase_id.clone();
        self.state = RuntimeState::Terminal {
            snapshot: terminal_snapshot.map(Box::new),
            evidence: Some(Box::new(EndurancePhaseTerminalEvidence {
                phase_kind: owners.kind,
                closure,
                owners: terminal_owners,
                export_verifier,
                bmx_runtime,
                failures,
            })),
        };
        self.publish_phase_owner_receipt(&phase_id)?;
        Ok((closure, events))
    }
}

fn runtime_error(detail: impl Into<String>) -> EnduranceCampaignError {
    EnduranceCampaignError::Runtime(detail.into())
}

fn terminal_receipt_from_error(
    error: &EnduranceCampaignError,
) -> Option<&EndurancePhaseTerminalEvidence> {
    match error {
        EnduranceCampaignError::WithTerminalEvidence { terminal, .. } => Some(terminal),
        EnduranceCampaignError::WithRunOwnerClosureEvidence { primary, .. }
        | EnduranceCampaignError::WithFailureReport { primary, .. }
        | EnduranceCampaignError::FailureReportPublication { primary, .. }
        | EnduranceCampaignError::RunOwnerShutdownAfterFailure { primary, .. }
        | EnduranceCampaignError::StartedPhaseCleanup { primary, .. } => {
            terminal_receipt_from_error(primary)
        }
        _ => None,
    }
}
fn retains_run_owner_shutdown(error: &EnduranceCampaignError) -> bool {
    match error {
        EnduranceCampaignError::RunOwnerShutdown(_) => true,
        EnduranceCampaignError::WithTerminalEvidence { primary, .. }
        | EnduranceCampaignError::WithRunOwnerClosureEvidence { primary, .. } => {
            retains_run_owner_shutdown(primary)
        }
        _ => false,
    }
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

        fn shutdown(
            self,
        ) -> Result<EnduranceSurfaceDriverShutdownEvidence, EnduranceRunOwnerShutdownFailure>
        {
            Ok(EnduranceSurfaceDriverShutdownEvidence::NotApplicable)
        }
    }

    struct ProbeSurface {
        shutdown_calls: Rc<Cell<u32>>,
        phase_terminal: Option<Rc<Cell<bool>>>,
        fail_shutdown: bool,
    }

    impl EnduranceSurfaceReopenDriver for ProbeSurface {
        fn reopen(
            &mut self,
            _app_state: AppState,
            _recovery_pump: EnduranceSurfaceRecoveryPump,
            _cycle_index: u32,
            _operation_id: String,
            _timeout: Duration,
        ) -> EnduranceSurfaceReopenRun {
            panic!("Surface reopen is outside run-owner closure tests")
        }

        fn shutdown(
            self,
        ) -> Result<EnduranceSurfaceDriverShutdownEvidence, EnduranceRunOwnerShutdownFailure>
        {
            if let Some(phase_terminal) = &self.phase_terminal {
                assert!(
                    phase_terminal.get(),
                    "Surface closed before phase terminalization"
                );
            }
            self.shutdown_calls.set(self.shutdown_calls.get() + 1);
            if self.fail_shutdown {
                Err(EnduranceRunOwnerShutdownFailure::new(
                    "injected Surface close failure",
                ))
            } else {
                Ok(EnduranceSurfaceDriverShutdownEvidence::NotApplicable)
            }
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
            assert_eq!(machine_plan.plan().schema_version, 2);
            Ok(self.inventory.clone())
        }

        fn build_phase(
            &mut self,
            machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
            requirement: &EndurancePhaseRequirement,
            workload: &PreparedEnduranceWorkload,
            prepared_start: PreparedEndurancePhaseStart,
        ) -> FreshEndurancePhaseBuild {
            self.build_calls.set(self.build_calls.get() + 1);
            assert_eq!(machine_plan.plan().schema_version, 2);
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
        let (temporary, prepared) = prepared_test_machine_plan();
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

    fn prepared_test_machine_plan() -> (tempfile::TempDir, PreparedCommercialEnduranceMachinePlan) {
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
        (temporary, prepared)
    }

    #[test]
    fn fresh_phase_rejects_a_cloned_plan_authority_and_retains_it_for_cleanup() {
        let (requirement, workload) = continuous_export_contract();
        let (_temporary, prepared) = prepared_test_machine_plan();
        let runtime_plan = Arc::new(prepared);
        let cloned_plan = Arc::new((*runtime_plan).clone());
        let fresh = FreshEndurancePhase {
            app_state: AppState::new(),
            authority: PreparedEndurancePhaseAuthority::test(Arc::clone(&cloned_plan)),
            inputs: FreshEndurancePhaseInputs::Test(requirement.kind),
        };

        let (owners, detail) =
            match PhaseOwners::from_fresh(&runtime_plan, &requirement, &workload, 0, fresh) {
                Err(failure) => failure,
                Ok(_) => panic!("a digest-equal clone is not the runtime-bound authority"),
            };

        assert!(detail.contains("runtime-bound machine plan"));
        assert!(owners.authority.is_some());
        assert!(Arc::ptr_eq(
            owners.authority.as_ref().expect("retained failed authority").machine_plan(),
            &cloned_plan
        ));

        let matching = FreshEndurancePhase {
            app_state: AppState::new(),
            authority: PreparedEndurancePhaseAuthority::test(Arc::clone(&runtime_plan)),
            inputs: FreshEndurancePhaseInputs::Test(requirement.kind),
        };
        let owners = PhaseOwners::from_fresh(&runtime_plan, &requirement, &workload, 0, matching)
            .unwrap_or_else(|(_, detail)| panic!("matching authority rejected: {detail}"));
        assert!(Arc::ptr_eq(
            owners.authority.as_ref().expect("retained ready authority").machine_plan(),
            &runtime_plan
        ));
    }

    #[test]
    fn not_run_admission_creates_no_app_or_product_owner() {
        let (requirement, workload) = continuous_export_contract();
        let build_calls = Rc::new(Cell::new(0));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,]),
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
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
                EndurancePreStartCapability::FrozenExportFixtureDeclared,
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

    fn failed_setup_runtime() -> (
        ProductEnduranceCampaignRuntime<TestFactory, ForbiddenSurface, TestClock>,
        tempfile::TempDir,
    ) {
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
                EndurancePreStartCapability::FrozenExportFixtureDeclared,
                EndurancePreStartCapability::IndependentExportVerifierPrepared,
            ]),
            build_calls: Rc::new(Cell::new(0)),
            fail_build: true,
        };
        let mut runtime = ProductEnduranceCampaignRuntime::new(
            factory,
            ForbiddenSurface,
            Arc::new(TestClock::default()),
        );
        let temporary = bind_test_machine_plan(&mut runtime);
        (runtime, temporary)
    }

    #[cfg(windows)]
    #[test]
    fn required_regulatory_pse_missing_provider_is_notrun_before_phase_factory() {
        use sha2::{Digest, Sha256};
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, temporary) = failed_setup_runtime();
        let qc = mondrian_broadcast::BroadcastQcProfile {
            id: "synthetic-prestart".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [1; 32],
            signal_color_space: mondrian_core::ColorSpace::Rec709,
            observation_tap:
                mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: mondrian_broadcast::QcActivePicture::full(1, 1),
            rules: vec![mondrian_broadcast::BroadcastQcRule::LumaFlashCandidate {
                rule_id: "triage-only".to_owned(),
                minimum_mean_luma_delta: 0.5,
                severity: mondrian_broadcast::BroadcastQcSeverity::Info,
            }],
            maximum_retained_findings: 1,
            require_regulatory_flash_analysis: true,
            require_encoded_artifact_revalidation: true,
        };
        let qc_bytes = serde_json::to_vec(&qc).expect("QC JSON");
        let qc_path = temporary.path().join("required-pse-qc.json");
        std::fs::write(&qc_path, &qc_bytes).expect("QC fixture");
        let mut plan = runtime.machine_plan.as_ref().expect("bound plan").plan().clone();
        let export = plan
            .exports
            .iter_mut()
            .find(|export| export.phase_id == requirement.phase_id)
            .expect("phase export");
        export.broadcast_qc = Some(
            crate::app::endurance_machine_plan::EnduranceMachineFileBinding {
                path: mondrian_assets::canonical_native_path(&qc_path).expect("QC canonical path"),
                sha256: format!("{:x}", Sha256::digest(&qc_bytes)),
            },
        );
        export.regulatory_pse = None;
        let plan_path = temporary.path().join("pse-machine-plan.json");
        std::fs::write(&plan_path, serde_json::to_vec(&plan).expect("plan JSON")).expect("plan");
        runtime.machine_plan = Some(Arc::new(
            PreparedCommercialEnduranceMachinePlan::load(&plan_path, &qualification_profile(), 24)
                .expect("new exact plan"),
        ));
        let EndurancePhaseAdmission::NotRun(not_run) =
            runtime.begin_phase(&requirement, &workload, 0).expect("pre-start admission")
        else {
            panic!("missing provider must never start phase")
        };
        assert_eq!(
            not_run.missing_capabilities(),
            &[EndurancePreStartCapability::RegulatoryPseProviderPrepared]
        );
        assert_eq!(runtime.factory.build_calls.get(), 0);
        assert!(matches!(runtime.state, RuntimeState::Empty));
        assert!(runtime.phase_owner_history.is_empty());
        assert!(runtime.regulatory_pse_prerequisites.is_empty());
    }
    #[test]
    fn public_failure_retains_real_clean_shutdown_without_inventing_cleanup_error() {
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, _temporary) = failed_setup_runtime();
        let error = runtime
            .run_with_terminal_evidence::<()>(|runtime| {
                let primary = runtime.begin_phase(&requirement, &workload, 0).unwrap_err();
                Err(super::super::endurance_campaign::cleanup_started_phase(
                    runtime, primary,
                ))
            })
            .unwrap_err();
        let EnduranceCampaignError::WithTerminalEvidence { primary, terminal } = error else {
            panic!("public failure lost consuming evidence");
        };
        assert!(
            matches!(*primary, EnduranceCampaignError::Runtime(ref detail)
            if detail == "injected phase setup failure")
        );
        assert_eq!(terminal.phase_kind, requirement.kind);
        assert_eq!(
            terminal.closure.status,
            EndurancePhaseTerminalStatus::Failed
        );
        assert!(terminal.owners.app().all_resources_released());
        assert!(matches!(
            terminal.owners,
            EnduranceTerminalOwners::AppOnly(_)
        ));
        assert_eq!(runtime.factory.build_calls.get(), 1);
        assert!(runtime.snapshot().is_ok());
        assert!(runtime.shutdown_phase().is_err());
        let second = runtime
            .run_with_terminal_evidence::<()>(|_| Err(runtime_error("second failure")))
            .unwrap_err();
        assert!(matches!(second, EnduranceCampaignError::Runtime(_)));
    }

    #[test]
    fn operation_error_panic_and_close_panic_retain_actual_export_and_app_receipts() {
        for path in 0..3 {
            let (requirement, workload) = continuous_export_contract();
            let (mut runtime, temporary) = failed_setup_runtime();
            let directory = temporary.path().join("owners");
            std::fs::create_dir(&directory).expect("create owner evidence directory");
            runtime.owner_report_location =
                Some(("bounded-owner-run".to_owned(), directory.clone()));
            let error = runtime
                .run_with_terminal_evidence::<()>(|runtime| {
                    assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
                    match path {
                        0 => Err(runtime_error("injected operation error before cleanup")),
                        1 => panic!("injected campaign operation panic"),
                        _ => {
                            runtime.phase_close_checkpoint =
                                Some(|| panic!("injected close verifier panic"));
                            let (closure, _) = runtime.shutdown_phase()?;
                            assert_eq!(closure.status, EndurancePhaseTerminalStatus::Failed);
                            Err(runtime_error("close preparation failed"))
                        }
                    }
                })
                .expect_err("the injected failure cannot become a successful campaign");
            assert_eq!(runtime.phase_owner_history.len(), 1);
            let durable: mondrian_platform::EndurancePhaseOwnerReceipt = serde_json::from_slice(
                &std::fs::read(directory.join("phase-owner-00.json"))
                    .expect("durable phase report"),
            )
            .expect("phase owner schema");
            assert_eq!(durable, runtime.phase_owner_history[0]);
            let failure = runtime
                .publish_failed_run::<()>(Err(error))
                .expect_err("failed run cannot be accepted");
            let EnduranceCampaignError::WithFailureReport { primary, .. } = failure else {
                panic!("path {path} did not durably publish complete failure report");
            };
            let published: serde_json::Value = serde_json::from_slice(
                &std::fs::read(directory.join("run-failure.json")).expect("durable run failure"),
            )
            .expect("failure JSON");
            assert_eq!(
                published["phase_owner_history"].as_array().expect("history").len(),
                1
            );
            assert!(!published["current_terminal"].is_null());
            let EnduranceCampaignError::WithTerminalEvidence { terminal, .. } = *primary else {
                panic!("path {path} lost actual App/Export closure");
            };
            assert!(
                terminal.owners.app().all_resources_released(),
                "path {path}: {terminal:?}"
            );
            assert!(terminal.owners.app().export.all_resources_released());
            assert!(terminal.owners.app().export_terminal_snapshot.shutdown_requested);
            if path == 2 {
                assert!(terminal
                    .failures
                    .iter()
                    .any(|failure| failure.contains("injected close verifier panic")));
            }
            assert!(
                runtime.shutdown_phase().is_err(),
                "actual owner consumed exactly once"
            );
        }
    }

    #[test]
    fn durable_history_survives_phase_boundary_and_failure_report_collision() {
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, temporary) = failed_setup_runtime();
        let directory = temporary.path().join("owners");
        std::fs::create_dir(&directory).expect("create owner directory");
        runtime.owner_report_location = Some(("history-run".to_owned(), directory.clone()));
        assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
        if let RuntimeState::Owned(owners) = &mut runtime.state {
            owners.fault = None;
        }
        let (closure, _) = runtime.shutdown_phase().expect("consume successful phase owners");
        assert_eq!(closure.status, EndurancePhaseTerminalStatus::Completed);
        let original = runtime.phase_owner_history.clone();
        runtime.begin_phase_preparation();
        std::fs::write(directory.join("run-failure.json"), b"existing evidence")
            .expect("occupy create-only report path");
        let primary = runtime
            .run_with_terminal_evidence::<()>(|_| Err(runtime_error("next phase admission failed")))
            .expect_err("next phase failure");
        let error = runtime
            .publish_failed_run::<()>(Err(primary))
            .expect_err("create-only collision");
        let EnduranceCampaignError::FailureReportPublication { canonical_report_json, .. } = error
        else {
            panic!("publication error lost exact report bytes")
        };
        let report: serde_json::Value =
            serde_json::from_str(&canonical_report_json).expect("retained complete report");
        assert_eq!(
            report["phase_owner_history"],
            serde_json::to_value(&original).expect("history JSON")
        );
        assert_eq!(
            std::fs::read(directory.join("run-failure.json")).expect("existing evidence"),
            b"existing evidence"
        );
        assert_eq!(runtime.phase_owner_history, original);
    }

    #[test]
    fn run_owner_shutdown_is_exactly_once_and_retained_on_primary_failure() {
        let shutdown_calls = Rc::new(Cell::new(0));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,]),
            build_calls: Rc::new(Cell::new(0)),
            fail_build: false,
        };
        let mut runtime = ProductEnduranceCampaignRuntime::new(
            factory,
            ProbeSurface {
                shutdown_calls: Rc::clone(&shutdown_calls),
                phase_terminal: None,
                fail_shutdown: false,
            },
            Arc::new(TestClock::default()),
        );

        let error = runtime
            .finish_run::<()>(Err(runtime_error("injected preflight failure")))
            .expect_err("primary failure must remain a failure");
        assert!(matches!(
            error,
            EnduranceCampaignError::WithRunOwnerClosureEvidence { primary, closure }
                if matches!(*primary, EnduranceCampaignError::Runtime(ref detail)
                    if detail == "injected preflight failure")
                    && matches!(closure, EnduranceRunOwnerClosureEvidence::NotApplicable)
        ));
        assert_eq!(shutdown_calls.get(), 1);

        let second = runtime
            .finish_run::<()>(Err(runtime_error("later publication failure")))
            .expect_err("later failure must retain the same closure");
        assert!(matches!(
            second,
            EnduranceCampaignError::WithRunOwnerClosureEvidence { closure, .. }
                if matches!(closure, EnduranceRunOwnerClosureEvidence::NotApplicable)
        ));
        assert_eq!(shutdown_calls.get(), 1);
    }

    #[test]
    fn phase_terminal_precedes_run_owner_shutdown_and_success_requires_closure() {
        let shutdown_calls = Rc::new(Cell::new(0));
        let phase_terminal = Rc::new(Cell::new(false));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,]),
            build_calls: Rc::new(Cell::new(0)),
            fail_build: false,
        };
        let mut runtime = ProductEnduranceCampaignRuntime::new(
            factory,
            ProbeSurface {
                shutdown_calls: Rc::clone(&shutdown_calls),
                phase_terminal: Some(Rc::clone(&phase_terminal)),
                fail_shutdown: false,
            },
            Arc::new(TestClock::default()),
        );
        runtime.state = RuntimeState::Terminal { snapshot: None, evidence: None };
        phase_terminal.set(true);

        runtime.shutdown_run_owner().expect("close run owner before publication");
        runtime.finish_run(Ok(())).expect("clean owner closure");

        assert_eq!(shutdown_calls.get(), 1);
        assert!(matches!(
            runtime.run_owner_closure,
            Some(EnduranceRunOwnerClosureEvidence::NotApplicable)
        ));
    }

    #[test]
    fn run_owner_shutdown_failure_does_not_overwrite_primary_failure() {
        let shutdown_calls = Rc::new(Cell::new(0));
        let factory = TestFactory {
            inventory: EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,]),
            build_calls: Rc::new(Cell::new(0)),
            fail_build: false,
        };
        let mut runtime = ProductEnduranceCampaignRuntime::new(
            factory,
            ProbeSurface {
                shutdown_calls: Rc::clone(&shutdown_calls),
                phase_terminal: None,
                fail_shutdown: true,
            },
            Arc::new(TestClock::default()),
        );

        let error = runtime
            .finish_run::<()>(Err(runtime_error("injected campaign failure")))
            .expect_err("both failures must be retained");

        assert!(matches!(
            error,
            EnduranceCampaignError::RunOwnerShutdownAfterFailure { primary, shutdown }
                if matches!(*primary, EnduranceCampaignError::Runtime(ref detail)
                    if detail == "injected campaign failure")
                    && shutdown.diagnostic() == "injected Surface close failure"
        ));
        assert_eq!(shutdown_calls.get(), 1);
        let second = runtime
            .finish_run::<()>(Err(runtime_error("second campaign failure")))
            .expect_err("stored shutdown failure must remain stable");
        assert!(matches!(
            second,
            EnduranceCampaignError::RunOwnerShutdownAfterFailure { shutdown, .. }
                if shutdown.diagnostic() == "injected Surface close failure"
        ));
        assert_eq!(shutdown_calls.get(), 1);
    }

    #[test]
    fn public_failure_retains_primary_cleanup_error_and_real_unconsumed_cache_receipt() {
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, _temporary) = failed_setup_runtime();
        let mut retained_cache = None;
        let error = runtime
            .run_with_terminal_evidence::<()>(|runtime| {
                let primary = runtime.begin_phase(&requirement, &workload, 0).unwrap_err();
                let RuntimeState::Owned(owners) = &runtime.state else {
                    panic!("failed setup must own its App");
                };
                // Real external ownership prevents consuming the cache at shutdown.
                retained_cache = Some(Arc::clone(
                    &owners.app.as_ref().expect("real App").audio_source_cache,
                ));
                Err(super::super::endurance_campaign::cleanup_started_phase(
                    runtime, primary,
                ))
            })
            .unwrap_err();
        let EnduranceCampaignError::WithTerminalEvidence { primary, terminal } = error else {
            panic!("public failure lost consuming evidence");
        };
        let EnduranceCampaignError::StartedPhaseCleanup { primary, cleanup } = *primary else {
            panic!("incomplete real cleanup must retain both errors");
        };
        assert!(
            matches!(*primary, EnduranceCampaignError::Runtime(ref detail)
            if detail == "injected phase setup failure")
        );
        assert!(matches!(
            *cleanup,
            EnduranceCampaignError::IncompletePhaseCleanup { .. }
        ));
        let cache = &terminal.owners.app().audio_source_cache;
        assert!(cache.strong_references_remaining > 0);
        assert!(cache.cache.is_none());
        assert!(!terminal.owners.app().all_resources_released());
        assert!(terminal.failures.iter().any(|failure| failure.contains("retained resources")));
        let cache = Arc::try_unwrap(retained_cache.take().expect("test-owned cache"))
            .unwrap_or_else(|_| panic!("App must have released its own cache reference"));
        let receipt = cache.shutdown_until(Instant::now() + Duration::from_secs(10));
        assert_eq!(receipt.decoder_resource_handles_remaining, 0);
    }

    #[test]
    fn post_shutdown_publication_failure_retains_the_successful_owner_receipt() {
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, _temporary) = failed_setup_runtime();
        let error = runtime
            .run_with_terminal_evidence::<()>(|runtime| {
                assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
                let RuntimeState::Owned(owners) = &mut runtime.state else {
                    panic!("real App missing");
                };
                owners.fault = None;
                let (closure, _) = runtime.shutdown_phase()?;
                assert_eq!(closure.status, EndurancePhaseTerminalStatus::Completed);
                Err(runtime_error("injected final publication failure"))
            })
            .unwrap_err();
        let EnduranceCampaignError::WithTerminalEvidence { primary, terminal } = error else {
            panic!("post-shutdown error lost the actual clean receipt");
        };
        assert!(
            matches!(*primary, EnduranceCampaignError::Runtime(ref detail)
            if detail == "injected final publication failure")
        );
        assert!(terminal.owners.app().all_resources_released());
        assert_eq!(
            terminal.closure.status,
            EndurancePhaseTerminalStatus::Completed
        );
        assert!(terminal.failures.is_empty());
    }

    #[test]
    fn missing_terminal_snapshot_keeps_evidence_but_never_grants_next_phase_admission() {
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, _temporary) = failed_setup_runtime();
        runtime.state = RuntimeState::Owned(Box::new(PhaseOwners::failed_before_start(
            "test-phase".to_owned(),
            EndurancePhaseKind::PlaybackReference,
            AppState::new(),
            None,
            0,
            1,
            0,
            "injected pre-playback failure".to_owned(),
        )));
        runtime.shutdown_phase().expect("consume real App without a playback snapshot");
        assert!(matches!(
            runtime.state,
            RuntimeState::Terminal { snapshot: None, evidence: Some(_) }
        ));
        assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
        runtime.begin_phase_preparation();
        assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
        assert_eq!(runtime.factory.build_calls.get(), 0);
        assert!(runtime.snapshot().is_err());
    }

    #[test]
    #[ignore = "requires a real local GPU"]
    fn headless_startup_bind_failure_retains_exact_phase_receipt_through_public_error_seam() {
        use crate::app::headless_preview_presentation::HeadlessPreviewRuntime;
        use crate::app::headless_realtime_playback::{
            configure_headless_gpu_decode_admission, HeadlessRealtimePlaybackSession,
        };
        use crate::app::headless_viewer_gpu::HeadlessViewerGpuAdapter;
        let (mut runtime, _temporary) = failed_setup_runtime();
        let mut owners = PhaseOwners::failed_before_start(
            "test-phase".to_owned(),
            EndurancePhaseKind::PlaybackReference,
            AppState::new(),
            None,
            0,
            1,
            0,
            "startup failure".to_owned(),
        );
        let result = EnduranceExecutionOwners::start_with_headless(
            owners.app_ref().expect("retained App"),
            || {
                let gpu = HeadlessViewerGpuAdapter::new()?;
                HeadlessRealtimePlaybackSession::bind(
                    HeadlessPreviewRuntime::new(),
                    gpu,
                    |preview, gpu| {
                        configure_headless_gpu_decode_admission(preview, gpu)?;
                        anyhow::bail!("injected phase decoder bind failure");
                    },
                )
            },
        );
        let detail = owners.install_execution_result(result).expect_err("injected bind failure");
        assert!(detail.contains("injected phase decoder bind failure"));
        assert!(owners.startup_failure.is_some());
        assert!(
            owners.execution.is_none()
                && owners.timeline.is_none()
                && owners.reference.is_none()
                && owners.export.is_none()
        );
        owners.fault = Some(detail.clone());
        runtime.state = RuntimeState::Owned(Box::new(owners));
        let error = runtime
            .run_with_terminal_evidence::<()>(|runtime| {
                let (closure, _) = runtime.shutdown_phase()?;
                assert_eq!(closure.status, EndurancePhaseTerminalStatus::Failed);
                assert!(closure.playback_workers_terminated);
                Err(runtime_error(detail))
            })
            .expect_err("original failure with owner-free receipt");
        let EnduranceCampaignError::WithTerminalEvidence { primary, terminal } = error else {
            panic!("lost terminal receipt")
        };
        assert!(primary.to_string().contains("injected phase decoder bind failure"));
        let EnduranceTerminalOwners::Startup(closure) = terminal.owners else {
            panic!("startup misreported as normal/App-only")
        };
        assert!(closure.all_created_resources_released(), "{closure:?}");
        assert!(closure.waveform.is_none());
        assert!(!closure.waveform_construction_unverified);
        assert!(closure.headless.preview.is_some());
        assert!(matches!(
            closure.headless.gpu,
            crate::app::headless_execution_startup::HeadlessStartupGpuShutdownEvidence::Adapter(_)
        ));
        assert!(runtime.shutdown_phase().is_err());
        assert!(runtime.snapshot().is_err());
    }

    #[test]
    fn next_phase_preparation_error_cannot_receive_the_previous_receipt() {
        let (requirement, workload) = continuous_export_contract();
        let (mut runtime, _temporary) = failed_setup_runtime();
        let error = runtime
            .run_with_terminal_evidence::<()>(|runtime| {
                assert!(runtime.begin_phase(&requirement, &workload, 0).is_err());
                runtime.shutdown_phase()?;
                assert!(matches!(
                    runtime.state,
                    RuntimeState::Terminal { evidence: Some(_), .. }
                ));
                runtime.begin_phase_preparation();
                // The coordinator loads the next workload before begin_phase.
                Err(runtime_error("next workload could not be loaded"))
            })
            .unwrap_err();
        assert!(matches!(error, EnduranceCampaignError::Runtime(ref detail)
            if detail == "next workload could not be loaded"));
        runtime.factory.inventory = EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,]);
        assert!(matches!(
            runtime.begin_phase(&requirement, &workload, 0),
            Ok(EndurancePhaseAdmission::NotRun(_))
        ));
        assert_eq!(runtime.factory.build_calls.get(), 1);
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
            "test-phase".to_owned(),
            EndurancePhaseKind::ContinuousExport,
            AppState::new(),
            None,
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
            inventory: EndurancePreStartCapabilityInventory::new([
            super::super::endurance_workload::EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,]),
            build_calls: Rc::new(Cell::new(0)),
            fail_build: false,
        };
        let mut owners = PhaseOwners::failed_before_start(
            "test-phase".to_owned(),
            EndurancePhaseKind::ContinuousExport,
            AppState::new(),
            None,
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
