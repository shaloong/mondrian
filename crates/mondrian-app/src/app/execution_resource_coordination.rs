//! Product-level execution resource policy.
//!
//! This Module turns coarse machine capacity, current product demand, and
//! optional measured pressure into one immutable decision snapshot. It never
//! owns domain jobs, worker threads, cancellation, queue ordering, or terminal
//! evidence. Preview, Proxy, Thumbnail, Waveform, Import, and Export remain
//! independently scheduled execution Modules.

use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_audio::AudioRuntimeResourceGrant;
use mondrian_effects::{
    EffectExecutionSessionConfig, EffectGraphExecutionBudget, LutPreparationCacheConfig,
};
use mondrian_export::{
    ExportExecutionResourcePolicy, EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
};
use mondrian_media::{HwDeviceContextPoolPolicy, PreviewSeekIndexCachePolicy};
use mondrian_platform::{
    ExecutionMemoryProbe, PhysicalMemoryCapacityProbe, ProcessMemoryProbeResult,
    ProcessMemoryScope, SystemMemoryProbeResult, SystemPlatformService,
};
#[cfg(test)]
use mondrian_playback::MAX_BOUNDED_VIDEO_PREROLL_FRAMES;
use mondrian_playback::{PreviewFrameStoreConfig, PreviewResolutionScale};
use mondrian_renderer::{
    GpuVisualFrameExecutionResourceGrant, HeterogeneousCpuPrefixBatchGrant,
    HeterogeneousGpuResourceGrant, PreparedVisualProgramCacheConfig,
    RenderGpuOutputExecutionResourceGrant, TimelineCpuWorkingSetGrant,
    ViewerGpuExecutionResourceGrant, ViewerGpuExecutionRuntime,
};
use parking_lot::Mutex;

use super::execution_resource_slots::{
    ExecutionResourceSlotAllocator, ExecutionResourceSlotDecision,
    ExecutionResourceSlotDemandSnapshot, ExecutionResourceSlotDomain,
    ExecutionResourceSlotDomainFacts, ExecutionResourceSlotDomains, ExecutionResourceSlotInput,
};
use super::AppState;

const MIB: usize = 1024 * 1024;

/// Schema revision of the immutable execution-resource decision.
pub(crate) const EXECUTION_RESOURCE_DECISION_VERSION: u32 = 15;
const PROCESS_PRESSURE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(1);
const PROCESS_PRESSURE_RESULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PREVIEW_HETEROGENEOUS_MAX_BATCH_ITEMS: usize = 5;
const PREVIEW_HETEROGENEOUS_MAX_MATERIALIZATIONS: usize = 64;
const PREVIEW_HETEROGENEOUS_MAX_STEPS: usize = 192;

/// Coarse product machine class used before workload-specific evidence exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachineResourceClass {
    /// Platform capacity was unavailable; use the minimum policy without
    /// claiming that the machine was measured as an 8 GiB system.
    UnknownConservative,
    /// A measured machine below the supported 8 GiB floor.
    BelowMinimum,
    /// A measured 8 GiB-or-greater machine below the ordinary 16 GiB class.
    MinimumSupported,
    /// The ordinary 16 GiB playback class.
    Standard,
    /// The 32 GiB-or-greater large-project class.
    Professional,
}

/// Explicit machine facts supplied to the policy Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MachineResourceProfile {
    pub(crate) class: MachineResourceClass,
    pub(crate) installed_memory_bytes: Option<u64>,
    pub(crate) logical_cpu_count: usize,
}

impl MachineResourceProfile {
    pub(crate) fn from_capacity(
        installed_memory_bytes: Option<u64>,
        logical_cpu_count: usize,
    ) -> Self {
        const GIB: u64 = 1024 * 1024 * 1024;
        let class = match installed_memory_bytes {
            Some(bytes) if bytes >= 32 * GIB => MachineResourceClass::Professional,
            Some(bytes) if bytes >= 16 * GIB => MachineResourceClass::Standard,
            Some(bytes) if bytes >= 8 * GIB => MachineResourceClass::MinimumSupported,
            Some(_) => MachineResourceClass::BelowMinimum,
            None => MachineResourceClass::UnknownConservative,
        };
        Self {
            class,
            installed_memory_bytes,
            logical_cpu_count: logical_cpu_count.max(1),
        }
    }
}

impl Default for MachineResourceProfile {
    fn default() -> Self {
        Self::detect()
    }
}

impl MachineResourceProfile {
    /// Detect reliable physical-memory capacity where the platform Adapter
    /// exposes it. Unsupported platforms remain explicitly conservative.
    fn detect() -> Self {
        let memory = SystemPlatformService.physical_memory_capacity();
        Self::from_capacity(
            memory.installed_physical_bytes,
            std::thread::available_parallelism().ok().map(usize::from).unwrap_or(1),
        )
    }
}

/// Optional product-level pressure observation.
///
/// Domain-local backpressure remains owned by each execution Module. This
/// value only represents measured whole-process or platform pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionResourcePressure {
    /// No whole-process resource pressure is currently observed.
    #[default]
    Nominal,
    /// Residency or platform evidence requests conservative background work.
    Elevated,
    /// Continuing optional work risks process or device instability.
    Critical,
}

/// Authority that selected the currently published pressure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionResourcePressureSource {
    /// Startup policy before any explicit or native observation.
    Baseline,
    /// Explicit product/test override.
    Manual,
    /// Process and whole-system native memory observation.
    NativeMemory,
}

/// Current demand facts for one independently scheduled execution Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionDomainDemand {
    pub(crate) queued: usize,
    pub(crate) running: usize,
    pub(crate) user_initiated: usize,
    /// Monotonic domain-local terminal boundary used only for fair slot rotation.
    pub(crate) terminal_generation: u64,
}

/// Declarative product demand sampled at one coordination revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionResourceDemandSnapshot {
    pub(crate) preview_realtime: bool,
    pub(crate) audio_realtime: bool,
    pub(crate) proxy: ExecutionDomainDemand,
    pub(crate) thumbnail: ExecutionDomainDemand,
    pub(crate) waveform: ExecutionDomainDemand,
    pub(crate) audio_warmup: ExecutionDomainDemand,
    pub(crate) media_import: ExecutionDomainDemand,
    pub(crate) media_asset_mutation: ExecutionDomainDemand,
    pub(crate) export: ExecutionDomainDemand,
}

/// Demand owned by execution Modules composed outside `AppState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExternalExecutionResourceDemand {
    pub(crate) thumbnail: ExecutionDomainDemand,
    pub(crate) waveform: ExecutionDomainDemand,
}

/// How aggressively a domain should release optional residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceTrimRequest {
    None,
    Speculative,
    Aggressive,
}

/// Product policy projected onto Proxy's domain-owned scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProxyExecutionDecision {
    /// Whether queued work may cross Proxy's dispatch Seam.
    pub(crate) dispatch_enabled: bool,
    /// Whether automatic Import/Playback Recovery work may dispatch.
    pub(crate) automatic_dispatch_enabled: bool,
    /// Product-requested concurrency; Proxy may enforce a lower physical cap.
    pub(crate) max_parallelism: usize,
}

/// Product policy projected onto the Thumbnail execution Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThumbnailExecutionDecision {
    /// Whether lookup may admit new automatic derived-media work.
    pub(crate) automatic_admission_enabled: bool,
    /// Whether queued work may cross Thumbnail's dispatch Seam.
    pub(crate) dispatch_enabled: bool,
    /// Bounded successful-raster residency.
    pub(crate) cache_budget_bytes: usize,
}

/// Product policy projected onto the Waveform execution Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaveformExecutionDecision {
    /// Whether lookup may admit new automatic derived-media work.
    pub(crate) automatic_admission_enabled: bool,
    /// Whether queued work may cross Waveform's dispatch Seam.
    pub(crate) dispatch_enabled: bool,
    /// Aggregate envelope plus private decoded-PCM residency.
    pub(crate) cache_budget_bytes: usize,
}

/// Product policy projected onto Media Import's domain-owned worker set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaImportExecutionDecision {
    /// Whether queued import files may enter a probe worker.
    pub(crate) dispatch_enabled: bool,
    /// Product-requested probe-worker concurrency.
    pub(crate) max_parallelism: usize,
}

/// Product policy projected onto ordered existing-Asset mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaAssetMutationExecutionDecision {
    /// Whether the next queued mutation may enter its ordered probe worker.
    pub(crate) dispatch_enabled: bool,
}

/// Product policy projected onto the Export execution queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExportExecutionDecision {
    /// Whether a reversible export attempt may advance at its queue-owned gate.
    pub(crate) dispatch_enabled: bool,
    /// Job-local resource grant frozen by the next dispatched attempt.
    pub(crate) resource_policy: ExportExecutionResourcePolicy,
}

/// Product policy for process-level UI raster residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiExecutionDecision {
    /// Maximum exact source-and-size SVG raster entries retained by widgets.
    pub(crate) vector_icon_cache_entries: usize,
    /// Maximum RGBA payload bytes retained by the SVG raster cache.
    pub(crate) vector_icon_cache_bytes: usize,
}

/// Immutable policy for the realtime Preview Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewExecutionDecision {
    /// Coarsest temporary runtime scale permitted by product resource policy.
    ///
    /// The Playback Quality Policy may independently request a coarser scale.
    /// Sequence settings, source choice, color, and proxy semantics are never
    /// changed by this value.
    pub(crate) minimum_runtime_scale: PreviewResolutionScale,
    pub(crate) trim: ResourceTrimRequest,
    /// Persistent decoded-media and Viewer residency limits.
    pub(crate) frame_store: PreviewFrameStoreConfig,
    /// Narrow resource projection for the active Viewer GPU owner.
    pub(crate) viewer_gpu: PreviewViewerGpuExecutionDecision,
    /// Preview's one retained Basic Title result-cache budget.
    pub(crate) title_cache_budget_bytes: usize,
    /// Preview-owned Effect execution cache residency.
    pub(crate) effect_cache: PreviewEffectCacheDecision,
    /// Hard compositor-owned transient and retained CPU working-set grant.
    ///
    /// This depends only on machine class. Pressure may request a coarser
    /// future Preview frame but cannot reinterpret an admitted candidate.
    pub(crate) cpu_composite_working_set: TimelineCpuWorkingSetGrant,
    /// Frozen resource authority for one CPU-F32 -> GPU-F32 Effect attempt.
    ///
    /// Unlike optional cache residency, this decision is derived only from the
    /// machine class. A pressure or trim observation cannot reinterpret an
    /// already-admitted heterogeneous route.
    pub(crate) heterogeneous_effects: PreviewHeterogeneousEffectExecutionDecision,
    /// Maximum OCIO CPU processors retained by the Preview execution owner.
    pub(crate) cpu_color_processor_capacity: usize,
    /// Prepared Sequence visual-program residency owned by Preview.
    pub(crate) visual_program_cache: PreparedVisualProgramCacheConfig,
    /// Aggregate seek-index residency shared by Preview's decode worker family.
    pub(crate) seek_index_cache: PreviewSeekIndexCachePolicy,
    /// Idle hardware-device roots shared by Preview's decode worker family.
    pub(crate) hardware_device_contexts: HwDeviceContextPoolPolicy,
}

/// Product cache grant for Preview's instance-owned Effect execution Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewEffectCacheDecision {
    /// Maximum retained Effect output identities.
    pub(crate) max_entries: usize,
    /// Maximum retained Effect output bytes.
    pub(crate) max_bytes: usize,
    /// Maximum transient bytes admitted by one scalar visual execution.
    ///
    /// This request-local ceiling follows physical machine class and is not
    /// divided by realtime/cache pressure. Pressure may trim optional
    /// residency or request a coarser future frame; it cannot reinterpret an
    /// already-admitted frame or invalidate its frozen execution route.
    pub(crate) max_working_bytes: usize,
    /// Maximum retained GPU lowering plans or deterministic blockers.
    pub(crate) max_gpu_plan_entries: usize,
    /// Maximum conservatively estimated bytes retained by GPU planning.
    pub(crate) max_gpu_plan_bytes: usize,
}

/// Frozen renderer grants for one Preview heterogeneous Effect batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewHeterogeneousEffectExecutionDecision {
    cpu_prefix: HeterogeneousCpuPrefixBatchGrant,
    gpu_continuation: HeterogeneousGpuResourceGrant,
}

impl PreviewHeterogeneousEffectExecutionDecision {
    /// CPU-prefix Session, graph-plan, cardinality, and retained-pixel grant.
    pub(crate) const fn cpu_prefix_grant(self) -> HeterogeneousCpuPrefixBatchGrant {
        self.cpu_prefix
    }

    /// Per-continuation upload/device grant. Preview never grants readback;
    /// the GPU value proceeds directly into the Viewer composite.
    pub(crate) const fn gpu_continuation_grant(self) -> HeterogeneousGpuResourceGrant {
        self.gpu_continuation
    }

    /// Conservative authority available from Preview construction until the
    /// composition root applies its first detected machine decision.
    ///
    /// Preview is a complete execution Module immediately after construction;
    /// a missing product-policy tick must not make otherwise valid empty,
    /// generated, or CPU-only Timelines unavailable. The first coordinated
    /// resource decision replaces this grant before admitting machine-sized
    /// heterogeneous work.
    pub(crate) fn conservative_baseline() -> Self {
        preview_heterogeneous_effect_execution_decision(
            MachineResourceClass::UnknownConservative,
            128 * MIB,
        )
    }
}

/// Product resource projection consumed only by a Viewer GPU execution owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewViewerGpuExecutionDecision {
    /// Idle renderer-owned color-frame texture grant.
    pub(crate) grant: ViewerGpuExecutionResourceGrant,
    /// Whether currently idle textures should be retired at this tick.
    pub(crate) clear_idle: bool,
}

/// Renderer-owned recipient of one frozen Preview Viewer GPU projection.
///
/// This narrow Interface keeps product policy application testable without
/// giving the Host access to renderer resource tables.
pub(crate) trait PreviewViewerGpuResourceOwner {
    /// Reconfigure idle Viewer resources for subsequent work.
    fn reconfigure_resource_grant(&mut self, grant: ViewerGpuExecutionResourceGrant);

    /// Retire idle Viewer resources while preserving active candidates.
    fn clear_idle_resources(&self);
}

impl PreviewViewerGpuResourceOwner for ViewerGpuExecutionRuntime {
    fn reconfigure_resource_grant(&mut self, grant: ViewerGpuExecutionResourceGrant) {
        ViewerGpuExecutionRuntime::reconfigure_resource_grant(self, grant);
    }

    fn clear_idle_resources(&self) {
        ViewerGpuExecutionRuntime::clear_idle_resources(self);
    }
}

/// Apply Preview's Viewer projection at the renderer owner's execution Seam.
///
/// Window and Headless consumers call this same function immediately before
/// bounded Viewer work. Critical trim retires idle textures even when the
/// immutable grant did not change between adjacent production ticks.
pub(crate) fn apply_preview_viewer_gpu_resource_decision(
    runtime: &mut impl PreviewViewerGpuResourceOwner,
    decision: &PreviewViewerGpuExecutionDecision,
) {
    runtime.reconfigure_resource_grant(decision.grant);
    if decision.clear_idle {
        runtime.clear_idle_resources();
    }
}

/// Immutable realtime-audio residency and warmup policy.
///
/// These fields may change cache residency and speculative preparation only.
/// Audio graph compilation, sample coordinates, channel mapping, automation,
/// and mix output are outside resource-policy authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioExecutionDecision {
    /// Maximum retained decoded source windows.
    pub(crate) source_entry_capacity: usize,
    /// Maximum retained decoded source PCM payload bytes.
    pub(crate) source_byte_budget: usize,
    /// Maximum persistent FFmpeg audio source sessions.
    pub(crate) decoder_session_capacity: usize,
    /// Number of causal audio blocks ending at the paused playhead to prepare.
    pub(crate) idle_warmup_windows: usize,
    /// Whether the dedicated idle-warmup worker may cross its dispatch Seam.
    pub(crate) idle_warmup_dispatch_enabled: bool,
    /// Closure-wide hard grant frozen by each playback/warmup generation.
    ///
    /// This depends only on measured machine class. Runtime pressure may defer
    /// a new generation but cannot reinterpret an admitted Audio closure.
    pub(crate) runtime_grant: AudioRuntimeResourceGrant,
}

/// One auditable native memory observation used to derive product pressure.
///
/// `observed_at` is monotonic time relative to the owning Coordinator. The
/// scoped product-process-tree result remains intact even when unavailable, so
/// diagnostics can distinguish conservative degradation from complete native
/// evidence without introducing wall-clock authority into execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionMemoryObservation {
    pub(crate) observed_at: Duration,
    pub(crate) product_process_tree: ProcessMemoryProbeResult,
    pub(crate) system: SystemMemoryProbeResult,
}

/// Versioned immutable product resource decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionResourceDecisionSnapshot {
    pub(crate) schema_version: u32,
    /// Monotonic diagnostic publication counter.
    ///
    /// This value may saturate and is not decision identity or a consumer
    /// change-detection authority. Consumers compare the decision fields they
    /// actually apply.
    pub(crate) revision: u64,
    pub(crate) profile: MachineResourceProfile,
    pub(crate) pressure: ExecutionResourcePressure,
    pub(crate) pressure_source: ExecutionResourcePressureSource,
    /// Latest completed native sample, or `None` before the first cadence.
    pub(crate) memory_observation: Option<ExecutionMemoryObservation>,
    pub(crate) demand: ExecutionResourceDemandSnapshot,
    /// Fair bounded cross-domain allocation and two-phase handoff evidence.
    pub(crate) heavy_slots: ExecutionResourceSlotDecision,
    pub(crate) preview: PreviewExecutionDecision,
    pub(crate) audio: AudioExecutionDecision,
    pub(crate) proxy: ProxyExecutionDecision,
    pub(crate) thumbnail: ThumbnailExecutionDecision,
    pub(crate) waveform: WaveformExecutionDecision,
    pub(crate) media_import: MediaImportExecutionDecision,
    pub(crate) media_asset_mutation: MediaAssetMutationExecutionDecision,
    pub(crate) export: ExportExecutionDecision,
    pub(crate) ui: UiExecutionDecision,
}

struct ExecutionResourceCoordinationState {
    profile: MachineResourceProfile,
    pressure: ExecutionResourcePressure,
    pressure_source: ExecutionResourcePressureSource,
    last_pressure_observation_at: Option<Duration>,
    last_pressure_request_at: Option<Duration>,
    native_pressure_request_pending: bool,
    memory_observation: Option<ExecutionMemoryObservation>,
    demand: ExecutionResourceDemandSnapshot,
    heavy_slot_allocator: ExecutionResourceSlotAllocator,
    heavy_slots: ExecutionResourceSlotDecision,
    decision: Arc<ExecutionResourceDecisionSnapshot>,
}

/// Thread-safe owner of the latest immutable product resource decision.
pub(crate) struct ExecutionResourceCoordinator {
    state: Mutex<ExecutionResourceCoordinationState>,
    native_memory_runtime: Mutex<Option<NativeMemoryObservationRuntime>>,
    observation_origin: Instant,
}

struct NativeMemoryObservation {
    observed_at: Duration,
    product_process_tree: ProcessMemoryProbeResult,
    system: SystemMemoryProbeResult,
}

enum NativeMemoryObservationCommand {
    Observe(Duration),
    Stop,
}

struct NativeMemoryObservationRuntime {
    command_sender: mpsc::Sender<NativeMemoryObservationCommand>,
    observation_receiver: mpsc::Receiver<NativeMemoryObservation>,
    worker: Option<JoinHandle<()>>,
}

impl NativeMemoryObservationRuntime {
    fn start() -> std::io::Result<Self> {
        Self::start_with_probe(SystemPlatformService)
    }

    fn start_with_probe<P>(probe: P) -> std::io::Result<Self>
    where
        P: ExecutionMemoryProbe + 'static,
    {
        let (command_sender, command_receiver) = mpsc::channel();
        let (observation_sender, observation_receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("execution-memory-observer".to_owned())
            .spawn(move || {
                while let Ok(command) = command_receiver.recv() {
                    match command {
                        NativeMemoryObservationCommand::Observe(observed_at) => {
                            let observation = NativeMemoryObservation {
                                observed_at,
                                product_process_tree: probe.product_process_tree_memory(),
                                system: probe.current_system_memory(),
                            };
                            if observation_sender.send(observation).is_err() {
                                break;
                            }
                        }
                        NativeMemoryObservationCommand::Stop => break,
                    }
                }
            })?;
        Ok(Self {
            command_sender,
            observation_receiver,
            worker: Some(worker),
        })
    }

    fn request(&self, observed_at: Duration) -> bool {
        self.command_sender
            .send(NativeMemoryObservationCommand::Observe(observed_at))
            .is_ok()
    }

    fn drain_latest(&self) -> (Option<NativeMemoryObservation>, bool) {
        let mut latest = None;
        loop {
            match self.observation_receiver.try_recv() {
                Ok(observation) => latest = Some(observation),
                Err(mpsc::TryRecvError::Empty) => return (latest, false),
                Err(mpsc::TryRecvError::Disconnected) => return (latest, true),
            }
        }
    }

    fn stop_worker(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        let _ = self.command_sender.send(NativeMemoryObservationCommand::Stop);
        let _ = worker.join();
    }
}

impl Drop for NativeMemoryObservationRuntime {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

impl ExecutionResourceCoordinator {
    pub(crate) fn new(profile: MachineResourceProfile) -> Arc<Self> {
        let observation_origin = Instant::now();
        let pressure = ExecutionResourcePressure::Nominal;
        let pressure_source = ExecutionResourcePressureSource::Baseline;
        let demand = ExecutionResourceDemandSnapshot::default();
        let mut heavy_slot_allocator = ExecutionResourceSlotAllocator::default();
        let heavy_slots = update_heavy_slots(
            &mut heavy_slot_allocator,
            observation_origin,
            profile,
            pressure,
            demand,
            ExecutionResourceSlotDomains::all(),
        );
        let decision = Arc::new(derive_decision(
            1,
            profile,
            pressure,
            pressure_source,
            None,
            demand,
            heavy_slots,
        ));
        Arc::new(Self {
            state: Mutex::new(ExecutionResourceCoordinationState {
                profile,
                pressure,
                pressure_source,
                last_pressure_observation_at: None,
                last_pressure_request_at: None,
                native_pressure_request_pending: false,
                memory_observation: None,
                demand,
                heavy_slot_allocator,
                heavy_slots,
                decision,
            }),
            native_memory_runtime: Mutex::new(None),
            observation_origin,
        })
    }

    /// Observe optional whole-process pressure.
    pub(crate) fn observe_pressure(
        &self,
        pressure: ExecutionResourcePressure,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        let mut state = self.state.lock();
        if state.pressure != pressure
            || state.pressure_source != ExecutionResourcePressureSource::Manual
        {
            state.pressure = pressure;
            state.pressure_source = ExecutionResourcePressureSource::Manual;
            refresh_heavy_slots(
                &mut state,
                Instant::now(),
                ExecutionResourceSlotDomains::empty(),
            );
            publish_decision(&mut state);
        }
        Arc::clone(&state.decision)
    }

    /// Publish current product demand and return its resulting decision.
    #[cfg(test)]
    pub(crate) fn update_demand(
        &self,
        demand: ExecutionResourceDemandSnapshot,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        self.update_demand_with_fresh_observations(demand, ExecutionResourceSlotDomains::all())
    }

    fn update_demand_with_fresh_observations(
        &self,
        demand: ExecutionResourceDemandSnapshot,
        fresh_demand_observations: ExecutionResourceSlotDomains,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        let mut state = self.state.lock();
        let demand_changed = state.demand != demand;
        state.demand = demand;
        let slots_changed =
            refresh_heavy_slots(&mut state, Instant::now(), fresh_demand_observations);
        if demand_changed || slots_changed {
            publish_decision(&mut state);
        }
        Arc::clone(&state.decision)
    }

    pub(crate) fn decision(&self) -> Arc<ExecutionResourceDecisionSnapshot> {
        Arc::clone(&self.state.lock().decision)
    }

    fn acknowledge_heavy_slot_close(
        &self,
        decision: &ExecutionResourceDecisionSnapshot,
        domain: ExecutionResourceSlotDomain,
    ) -> bool {
        if !decision.heavy_slots.domains_to_close().contains(domain) {
            return false;
        }
        let Some(close_epoch) = decision.heavy_slots.close_epoch() else {
            return false;
        };
        self.state.lock().heavy_slot_allocator.acknowledge_close(close_epoch, domain)
    }

    /// Sample process and whole-system pressure no more than once per fixed
    /// monotonic cadence. The explicit instant is the test/Headless scheduling
    /// Seam.
    #[cfg(test)]
    fn observe_pressure_from_probe_at(
        &self,
        probe: &dyn ExecutionMemoryProbe,
        observed_at: Duration,
    ) -> bool {
        {
            let mut state = self.state.lock();
            let due = state.last_pressure_observation_at.is_none_or(|last| {
                observed_at < last
                    || observed_at.saturating_sub(last) >= PROCESS_PRESSURE_OBSERVATION_INTERVAL
            });
            if !due {
                return false;
            }
            state.last_pressure_observation_at = Some(observed_at);
        }
        let product_process_tree = probe.product_process_tree_memory();
        let system = probe.current_system_memory();
        self.apply_native_memory_observation(observed_at, product_process_tree, system);
        true
    }

    fn apply_native_memory_observation(
        &self,
        observed_at: Duration,
        product_process_tree: ProcessMemoryProbeResult,
        system: SystemMemoryProbeResult,
    ) {
        let mut state = self.state.lock();
        state.last_pressure_observation_at = Some(observed_at);
        state.native_pressure_request_pending = false;
        let process_tree_complete =
            product_process_tree.is_complete_for(ProcessMemoryScope::ProductProcessTree);
        let product_private_memory_bytes = if process_tree_complete {
            product_process_tree.private_memory_bytes
        } else {
            None
        };
        let has_dynamic_evidence = product_private_memory_bytes.is_some()
            || system.available_physical_bytes.is_some()
            || system.memory_load_percent.is_some();
        let process_tree_unavailable = !process_tree_complete;
        let supported_system_probe_failed = system.discovery_available
            && system.available_physical_bytes.is_none()
            && system.memory_load_percent.is_none();
        let conservative_degradation = process_tree_unavailable || supported_system_probe_failed;
        let pressure = if !has_dynamic_evidence && conservative_degradation {
            match state.pressure {
                ExecutionResourcePressure::Critical => ExecutionResourcePressure::Critical,
                ExecutionResourcePressure::Nominal | ExecutionResourcePressure::Elevated => {
                    ExecutionResourcePressure::Elevated
                }
            }
        } else {
            classify_memory_pressure(
                state.pressure,
                product_private_memory_bytes,
                state.profile.installed_memory_bytes,
                system.total_physical_bytes,
                system.available_physical_bytes,
                system.memory_load_percent,
            )
        };
        state.pressure = if conservative_degradation {
            match pressure {
                ExecutionResourcePressure::Nominal => ExecutionResourcePressure::Elevated,
                ExecutionResourcePressure::Elevated | ExecutionResourcePressure::Critical => {
                    pressure
                }
            }
        } else {
            pressure
        };
        state.pressure_source = ExecutionResourcePressureSource::NativeMemory;
        state.memory_observation =
            Some(ExecutionMemoryObservation { observed_at, product_process_tree, system });
        refresh_heavy_slots(
            &mut state,
            Instant::now(),
            ExecutionResourceSlotDomains::empty(),
        );
        // Evidence is diagnostic state in its own right. Publish every due
        // completed observation even when the derived pressure class is stable.
        publish_decision(&mut state);
    }

    fn poll_native_memory_observation(&self, observed_at: Duration) {
        let (completed, observer_disconnected) = {
            let mut runtime = self.native_memory_runtime.lock();
            if runtime.is_none() {
                match NativeMemoryObservationRuntime::start() {
                    Ok(started) => *runtime = Some(started),
                    Err(error) => {
                        drop(runtime);
                        self.state.lock().last_pressure_request_at = Some(observed_at);
                        self.apply_native_memory_observation(
                            observed_at,
                            ProcessMemoryProbeResult::unsupported(
                                ProcessMemoryScope::ProductProcessTree,
                                format!("failed to start native memory observer: {error}"),
                            ),
                            SystemMemoryProbeResult::unsupported(format!(
                                "failed to start native memory observer: {error}"
                            )),
                        );
                        return;
                    }
                }
            }
            let result = runtime
                .as_ref()
                .map(NativeMemoryObservationRuntime::drain_latest)
                .unwrap_or((None, true));
            if result.1 {
                runtime.take();
            }
            result
        };
        let completed_missing = completed.is_none();
        if let Some(completed) = completed {
            self.apply_native_memory_observation(
                completed.observed_at,
                completed.product_process_tree,
                completed.system,
            );
        }
        if observer_disconnected && completed_missing {
            self.state.lock().last_pressure_request_at = Some(observed_at);
            self.apply_native_memory_observation(
                observed_at,
                ProcessMemoryProbeResult::unsupported(
                    ProcessMemoryScope::ProductProcessTree,
                    "native memory observer stopped before publishing its requested sample",
                ),
                SystemMemoryProbeResult::unsupported(
                    "native memory observer stopped before publishing its requested sample",
                ),
            );
            return;
        }

        let should_request = {
            let mut state = self.state.lock();
            let due = !state.native_pressure_request_pending
                && state.last_pressure_request_at.is_none_or(|last| {
                    observed_at < last
                        || observed_at.saturating_sub(last) >= PROCESS_PRESSURE_OBSERVATION_INTERVAL
                });
            if due {
                state.last_pressure_request_at = Some(observed_at);
                state.native_pressure_request_pending = true;
            }
            due
        };
        if !should_request {
            return;
        }
        let request_accepted = self
            .native_memory_runtime
            .lock()
            .as_ref()
            .is_some_and(|runtime| runtime.request(observed_at));
        if !request_accepted {
            self.apply_native_memory_observation(
                observed_at,
                ProcessMemoryProbeResult::unsupported(
                    ProcessMemoryScope::ProductProcessTree,
                    "native memory observer stopped before accepting a request",
                ),
                SystemMemoryProbeResult::unsupported(
                    "native memory observer stopped before accepting a request",
                ),
            );
        }
    }

    fn observation_now(&self) -> Duration {
        self.observation_origin.elapsed()
    }

    fn next_observation_deadline(&self) -> Instant {
        let state = self.state.lock();
        if state.native_pressure_request_pending {
            return Instant::now()
                .checked_add(PROCESS_PRESSURE_RESULT_POLL_INTERVAL)
                .unwrap_or_else(Instant::now);
        }
        let next_offset = state
            .last_pressure_request_at
            .or(state.last_pressure_observation_at)
            .map(|last| last.saturating_add(PROCESS_PRESSURE_OBSERVATION_INTERVAL))
            .unwrap_or_default();
        self.observation_origin.checked_add(next_offset).unwrap_or_else(Instant::now)
    }
}

impl Default for ExecutionResourceCoordinator {
    fn default() -> Self {
        let observation_origin = Instant::now();
        Self {
            state: {
                let profile = MachineResourceProfile::default();
                let pressure = ExecutionResourcePressure::Nominal;
                let pressure_source = ExecutionResourcePressureSource::Baseline;
                let demand = ExecutionResourceDemandSnapshot::default();
                let mut heavy_slot_allocator = ExecutionResourceSlotAllocator::default();
                let heavy_slots = update_heavy_slots(
                    &mut heavy_slot_allocator,
                    observation_origin,
                    profile,
                    pressure,
                    demand,
                    ExecutionResourceSlotDomains::all(),
                );
                Mutex::new(ExecutionResourceCoordinationState {
                    profile,
                    pressure,
                    pressure_source,
                    last_pressure_observation_at: None,
                    last_pressure_request_at: None,
                    native_pressure_request_pending: false,
                    memory_observation: None,
                    demand,
                    heavy_slot_allocator,
                    heavy_slots,
                    decision: Arc::new(derive_decision(
                        1,
                        profile,
                        pressure,
                        pressure_source,
                        None,
                        demand,
                        heavy_slots,
                    )),
                })
            },
            native_memory_runtime: Mutex::new(None),
            observation_origin,
        }
    }
}

impl AppState {
    /// Apply optional whole-process pressure and immediately propagate the
    /// resulting policy to AppState-owned execution Modules.
    pub fn observe_execution_resource_pressure(&self, pressure: ExecutionResourcePressure) {
        self.execution_resources.observe_pressure(pressure);
        let _ = self.refresh_internal_execution_resource_decision();
    }

    /// Advance the UI-independent resource-policy cadence, sample all
    /// AppState-owned demand plus external Window/Headless demand, and apply
    /// the resulting typed projections at each domain Seam.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn refresh_execution_resource_decision(
        &self,
        external: ExternalExecutionResourceDemand,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        let observed_at = self.execution_resources.observation_now();
        self.execution_resources.poll_native_memory_observation(observed_at);
        let decision = self.update_execution_resource_demand(external, true);
        self.apply_internal_execution_resource_projection(&decision);
        decision
    }

    /// Compute one Window composition-root decision without opening any
    /// AppState-owned dispatch Seam.
    ///
    /// The Window first closes external Thumbnail/Waveform seams named by the
    /// transition, then asks AppState to close/apply its internal projections,
    /// and only then opens the final external allocation.
    pub(crate) fn coordinate_execution_resource_decision(
        &self,
        external: ExternalExecutionResourceDemand,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        let observed_at = self.execution_resources.observation_now();
        self.execution_resources.poll_native_memory_observation(observed_at);
        self.update_execution_resource_demand(external, true)
    }

    /// Refresh realtime priority immediately on transport transitions while
    /// retaining the most recently observed external demand.
    pub(super) fn refresh_internal_execution_resource_decision(
        &self,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        let prior = self.execution_resources.decision();
        let decision = self.update_execution_resource_demand(
            ExternalExecutionResourceDemand {
                thumbnail: prior.demand.thumbnail,
                waveform: prior.demand.waveform,
            },
            false,
        );
        self.apply_internal_execution_resource_projection(&decision);
        decision
    }

    pub(crate) fn execution_resource_decision(&self) -> Arc<ExecutionResourceDecisionSnapshot> {
        self.execution_resources.decision()
    }

    /// Earliest monotonic instant at which the next native memory sample is
    /// due. Window adapters must include this in their `WaitUntil` selection so
    /// a completely idle UI cannot silently stop pressure observation.
    pub(crate) fn next_execution_resource_observation_deadline(&self) -> Instant {
        self.execution_resources.next_observation_deadline()
    }

    fn update_execution_resource_demand(
        &self,
        external: ExternalExecutionResourceDemand,
        external_demand_is_fresh: bool,
    ) -> Arc<ExecutionResourceDecisionSnapshot> {
        let proxy = self.proxy_generation.diagnostics();
        let audio_warmup = self.audio_idle_warmup.diagnostics();
        let media_import = self.media_import_diagnostics();
        let media_asset_mutation = self.media_asset_mutation_diagnostics();
        let export = self.render_queue.diagnostics();
        let demand = ExecutionResourceDemandSnapshot {
            preview_realtime: self.is_playing(),
            audio_realtime: self.is_playing(),
            proxy: ExecutionDomainDemand {
                queued: proxy.queued,
                running: proxy.running,
                user_initiated: proxy.queued_user + proxy.running_user,
                terminal_generation: proxy.latest_terminal_sequence,
            },
            thumbnail: external.thumbnail,
            waveform: external.waveform,
            audio_warmup: ExecutionDomainDemand {
                queued: audio_warmup.queued,
                running: usize::from(audio_warmup.running.is_some()),
                user_initiated: 0,
                terminal_generation: audio_warmup.terminal_count,
            },
            media_import: ExecutionDomainDemand {
                queued: media_import.queued_files,
                running: media_import.running_files,
                user_initiated: media_import.outstanding_files,
                terminal_generation: media_import
                    .imported_files
                    .saturating_add(media_import.failed_files)
                    .saturating_add(media_import.canceled_files)
                    .saturating_add(media_import.superseded_files),
            },
            media_asset_mutation: ExecutionDomainDemand {
                queued: media_asset_mutation.queued,
                running: media_asset_mutation.running,
                user_initiated: media_asset_mutation.outstanding,
                terminal_generation: media_asset_mutation
                    .terminals
                    .last()
                    .map_or(0, |terminal| terminal.operation_id),
            },
            export: ExecutionDomainDemand {
                queued: export.pending,
                running: export.running + export.cancelling,
                user_initiated: export.pending + export.running + export.cancelling,
                terminal_generation: export
                    .completions
                    .saturating_add(export.failures)
                    .saturating_add(export.cancellations),
            },
        };
        let fresh_demand_observations = if external_demand_is_fresh {
            ExecutionResourceSlotDomains::all()
        } else {
            app_internal_heavy_slot_domains()
        };
        self.execution_resources
            .update_demand_with_fresh_observations(demand, fresh_demand_observations)
    }

    /// Apply the internal half of a composition-root resource transition.
    ///
    /// Callers that also own external domains must close those domains first.
    pub(crate) fn apply_internal_execution_resource_projection(
        &self,
        decision: &ExecutionResourceDecisionSnapshot,
    ) {
        let closing = decision.heavy_slots.domains_to_close();
        self.close_internal_execution_resource_domains(closing, decision);
        for domain in [
            ExecutionResourceSlotDomain::Proxy,
            ExecutionResourceSlotDomain::AudioWarmup,
            ExecutionResourceSlotDomain::MediaImport,
            ExecutionResourceSlotDomain::MediaAssetMutation,
            ExecutionResourceSlotDomain::Export,
        ] {
            if closing.contains(domain) {
                let acknowledged =
                    self.execution_resources.acknowledge_heavy_slot_close(decision, domain);
                debug_assert!(
                    acknowledged,
                    "internal close acknowledgement lost its resource transition"
                );
            }
        }
        self.apply_internal_execution_resource_decision(decision);
    }

    /// Confirm that a Window-owned execution domain has synchronously closed
    /// its dispatch seam for this exact resource transition.
    ///
    /// The allocator still requires a later fresh `running == 0` observation
    /// before it can open a replacement domain.
    pub(crate) fn acknowledge_external_execution_resource_domain_closed(
        &self,
        decision: &ExecutionResourceDecisionSnapshot,
        domain: ExecutionResourceSlotDomain,
    ) -> bool {
        debug_assert!(matches!(
            domain,
            ExecutionResourceSlotDomain::Thumbnail | ExecutionResourceSlotDomain::Waveform
        ));
        self.execution_resources.acknowledge_heavy_slot_close(decision, domain)
    }

    fn close_internal_execution_resource_domains(
        &self,
        domains: super::execution_resource_slots::ExecutionResourceSlotDomains,
        decision: &ExecutionResourceDecisionSnapshot,
    ) {
        if domains.contains(ExecutionResourceSlotDomain::Proxy) {
            self.proxy_generation.set_resource_policy(
                false,
                decision.proxy.max_parallelism,
                decision.proxy.automatic_dispatch_enabled,
            );
        }
        if domains.contains(ExecutionResourceSlotDomain::AudioWarmup) {
            self.audio_idle_warmup.set_dispatch_enabled(false);
        }
        if domains.contains(ExecutionResourceSlotDomain::MediaImport) {
            self.media_import
                .set_resource_policy(false, decision.media_import.max_parallelism);
        }
        if domains.contains(ExecutionResourceSlotDomain::MediaAssetMutation) {
            self.media_asset_mutations.set_resource_policy(false);
        }
        if domains.contains(ExecutionResourceSlotDomain::Export) {
            self.render_queue.set_dispatch_enabled(false);
        }
    }

    fn apply_internal_execution_resource_decision(
        &self,
        decision: &ExecutionResourceDecisionSnapshot,
    ) {
        self.proxy_generation.set_resource_policy(
            decision.proxy.dispatch_enabled,
            decision.proxy.max_parallelism,
            decision.proxy.automatic_dispatch_enabled,
        );
        self.media_import.set_resource_policy(
            decision.media_import.dispatch_enabled,
            decision.media_import.max_parallelism,
        );
        self.media_asset_mutations
            .set_resource_policy(decision.media_asset_mutation.dispatch_enabled);
        self.render_queue.set_resource_policy(decision.export.resource_policy);
        self.render_queue.set_dispatch_enabled(decision.export.dispatch_enabled);
        self.audio_idle_warmup
            .set_automatic_policy_enabled(decision.audio.idle_warmup_windows > 0);
        self.audio_idle_warmup
            .set_dispatch_enabled(decision.audio.idle_warmup_dispatch_enabled);
        self.audio_source_cache.reconfigure(mondrian_media::AudioSourceCacheConfig::new(
            decision.audio.source_entry_capacity,
            decision.audio.source_byte_budget,
            decision.audio.decoder_session_capacity,
        ));
    }
}

fn publish_decision(state: &mut ExecutionResourceCoordinationState) {
    let revision = state.decision.revision.saturating_add(1);
    state.decision = Arc::new(derive_decision(
        revision,
        state.profile,
        state.pressure,
        state.pressure_source,
        state.memory_observation.clone(),
        state.demand,
        state.heavy_slots,
    ));
}

fn slot_facts(demand: ExecutionDomainDemand) -> ExecutionResourceSlotDomainFacts {
    ExecutionResourceSlotDomainFacts {
        queued: demand.queued,
        running: demand.running,
        user_initiated: demand.user_initiated,
        terminal_generation: demand.terminal_generation,
    }
}

fn heavy_slot_demand(
    demand: ExecutionResourceDemandSnapshot,
) -> ExecutionResourceSlotDemandSnapshot {
    ExecutionResourceSlotDemandSnapshot {
        proxy: slot_facts(demand.proxy),
        thumbnail: slot_facts(demand.thumbnail),
        waveform: slot_facts(demand.waveform),
        audio_warmup: slot_facts(demand.audio_warmup),
        media_import: slot_facts(demand.media_import),
        media_asset_mutation: slot_facts(demand.media_asset_mutation),
        export: slot_facts(demand.export),
    }
}

fn heavy_slot_capacity(
    profile: MachineResourceProfile,
    pressure: ExecutionResourcePressure,
    demand: ExecutionResourceDemandSnapshot,
) -> usize {
    if demand.preview_realtime
        || demand.audio_realtime
        || pressure == ExecutionResourcePressure::Critical
    {
        return 0;
    }
    if pressure == ExecutionResourcePressure::Elevated {
        return 1;
    }
    match profile.class {
        MachineResourceClass::BelowMinimum
        | MachineResourceClass::UnknownConservative
        | MachineResourceClass::MinimumSupported => 1,
        MachineResourceClass::Standard => 2,
        MachineResourceClass::Professional => 4,
    }
}

fn app_internal_heavy_slot_domains() -> ExecutionResourceSlotDomains {
    ExecutionResourceSlotDomains::from_domains([
        ExecutionResourceSlotDomain::Proxy,
        ExecutionResourceSlotDomain::AudioWarmup,
        ExecutionResourceSlotDomain::MediaImport,
        ExecutionResourceSlotDomain::MediaAssetMutation,
        ExecutionResourceSlotDomain::Export,
    ])
}

fn update_heavy_slots(
    allocator: &mut ExecutionResourceSlotAllocator,
    now: Instant,
    profile: MachineResourceProfile,
    pressure: ExecutionResourcePressure,
    demand: ExecutionResourceDemandSnapshot,
    fresh_demand_observations: ExecutionResourceSlotDomains,
) -> ExecutionResourceSlotDecision {
    let capacity = heavy_slot_capacity(profile, pressure, demand);
    allocator.update(ExecutionResourceSlotInput {
        now,
        machine_slot_capacity: capacity,
        dispatch_allowed: capacity > 0,
        demand: heavy_slot_demand(demand),
        fresh_demand_observations,
    })
}

fn refresh_heavy_slots(
    state: &mut ExecutionResourceCoordinationState,
    now: Instant,
    fresh_demand_observations: ExecutionResourceSlotDomains,
) -> bool {
    let next = update_heavy_slots(
        &mut state.heavy_slot_allocator,
        now,
        state.profile,
        state.pressure,
        state.demand,
        fresh_demand_observations,
    );
    if next == state.heavy_slots {
        false
    } else {
        state.heavy_slots = next;
        true
    }
}

fn derive_decision(
    revision: u64,
    profile: MachineResourceProfile,
    pressure: ExecutionResourcePressure,
    pressure_source: ExecutionResourcePressureSource,
    memory_observation: Option<ExecutionMemoryObservation>,
    demand: ExecutionResourceDemandSnapshot,
    heavy_slots: ExecutionResourceSlotDecision,
) -> ExecutionResourceDecisionSnapshot {
    let realtime = demand.preview_realtime || demand.audio_realtime;
    let critical = pressure == ExecutionResourcePressure::Critical;
    let elevated = pressure == ExecutionResourcePressure::Elevated;
    let explicit_heavy_demand = demand.proxy.user_initiated > 0
        || demand.media_import.user_initiated > 0
        || demand.media_asset_mutation.user_initiated > 0
        || demand.export.user_initiated > 0;
    let explicit_dispatch = !realtime && !critical;
    let automatic_dispatch = explicit_dispatch && !elevated && !explicit_heavy_demand;
    let allocation = heavy_slots.allocation();
    let proxy_dispatch = allocation.admits(ExecutionResourceSlotDomain::Proxy);
    let thumbnail_dispatch = allocation.admits(ExecutionResourceSlotDomain::Thumbnail);
    let waveform_dispatch = allocation.admits(ExecutionResourceSlotDomain::Waveform);
    let audio_warmup_dispatch = allocation.admits(ExecutionResourceSlotDomain::AudioWarmup);
    let media_import_dispatch = allocation.admits(ExecutionResourceSlotDomain::MediaImport);
    let media_asset_mutation_dispatch =
        allocation.admits(ExecutionResourceSlotDomain::MediaAssetMutation);
    let export_dispatch = allocation.admits(ExecutionResourceSlotDomain::Export);
    let trim = if critical {
        ResourceTrimRequest::Aggressive
    } else if elevated
        || (realtime
            && matches!(
                profile.class,
                MachineResourceClass::UnknownConservative
                    | MachineResourceClass::BelowMinimum
                    | MachineResourceClass::MinimumSupported
            ))
    {
        ResourceTrimRequest::Speculative
    } else {
        ResourceTrimRequest::None
    };
    let minimum_runtime_scale = if critical
        || (realtime && profile.class == MachineResourceClass::BelowMinimum)
    {
        PreviewResolutionScale::Quarter
    } else if elevated
        || (realtime
            && matches!(
                profile.class,
                MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported
            ))
    {
        PreviewResolutionScale::Half
    } else {
        PreviewResolutionScale::Full
    };
    let (
        title_cache,
        thumbnail_cache,
        waveform_cache,
        audio_entries,
        audio_bytes,
        audio_sessions,
        audio_idle_warmup,
        proxy_parallelism,
        import_parallelism,
        vector_icon_entries,
        vector_icon_bytes,
    ) = match profile.class {
        MachineResourceClass::BelowMinimum => (
            16 * MIB,
            16 * MIB,
            16 * MIB,
            8,
            24 * MIB,
            1,
            0,
            1,
            1,
            64,
            8 * MIB,
        ),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => (
            32 * MIB,
            32 * MIB,
            32 * MIB,
            16,
            64 * MIB,
            2,
            1,
            1,
            1,
            96,
            16 * MIB,
        ),
        MachineResourceClass::Standard => (
            64 * MIB,
            64 * MIB,
            64 * MIB,
            32,
            128 * MIB,
            4,
            2,
            2,
            2,
            128,
            32 * MIB,
        ),
        MachineResourceClass::Professional => (
            128 * MIB,
            128 * MIB,
            128 * MIB,
            64,
            256 * MIB,
            8,
            3,
            4,
            2,
            256,
            64 * MIB,
        ),
    };
    let cpu_parallelism = profile.logical_cpu_count.max(1);
    let cache_divisor = if critical {
        4
    } else if elevated || realtime {
        2
    } else {
        1
    };
    let bounded_parallelism = |requested: usize| requested.min(cpu_parallelism).max(1);

    let frame_store = preview_frame_store_config(profile.class, trim);
    let (
        effect_cache_entries,
        effect_cache_bytes,
        effect_gpu_plan_entries,
        effect_gpu_plan_bytes,
        cpu_color_processors,
    ) = match profile.class {
        MachineResourceClass::BelowMinimum => (8, 32 * MIB, 8, MIB / 2, 8),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (16, 64 * MIB, 16, MIB, 16)
        }
        MachineResourceClass::Standard => (32, 192 * MIB, 32, 2 * MIB, 32),
        MachineResourceClass::Professional => (64, 384 * MIB, 64, 4 * MIB, 64),
    };
    let effect_working_bytes = match profile.class {
        MachineResourceClass::BelowMinimum | MachineResourceClass::UnknownConservative => 128 * MIB,
        MachineResourceClass::MinimumSupported => 256 * MIB,
        MachineResourceClass::Standard => 384 * MIB,
        MachineResourceClass::Professional => 768 * MIB,
    };
    let heterogeneous_effects =
        preview_heterogeneous_effect_execution_decision(profile.class, effect_working_bytes);
    let (seek_index_entries, seek_index_anchors, seek_index_bytes, hardware_idle_contexts) =
        match profile.class {
            MachineResourceClass::BelowMinimum => (8, 131_072, 2 * MIB, 0),
            MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
                (16, 262_144, 4 * MIB, 1)
            }
            MachineResourceClass::Standard => (32, 524_288, 8 * MIB, 2),
            MachineResourceClass::Professional => (64, 1_048_576, 16 * MIB, 3),
        };
    let (visual_program_entries, visual_program_bytes, lut_cache_entries, lut_cache_bytes) =
        match profile.class {
            MachineResourceClass::BelowMinimum => (8, 16 * MIB, 2, 8 * MIB),
            MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
                (16, 32 * MIB, 4, 16 * MIB)
            }
            MachineResourceClass::Standard => (32, 64 * MIB, 8, 32 * MIB),
            MachineResourceClass::Professional => (64, 128 * MIB, 16, 64 * MIB),
        };
    let audio_trim_divisor = match pressure {
        ExecutionResourcePressure::Nominal => 1,
        ExecutionResourcePressure::Elevated => 2,
        ExecutionResourcePressure::Critical => 4,
    };
    // Warmup residency and dispatch are deliberately separate facts. Keeping
    // one bounded latest demand queued while another explicit domain owns the
    // heavy slot lets admission resume without reinterpreting the audio
    // Program. Realtime playback has no paused-playhead demand, Elevated
    // pressure reduces that demand to one causal window, and Critical pressure
    // disables it entirely.
    let audio_idle_warmup_windows = if realtime {
        0
    } else {
        match pressure {
            ExecutionResourcePressure::Nominal => audio_idle_warmup,
            ExecutionResourcePressure::Elevated => audio_idle_warmup.min(1),
            ExecutionResourcePressure::Critical => 0,
        }
    };

    ExecutionResourceDecisionSnapshot {
        schema_version: EXECUTION_RESOURCE_DECISION_VERSION,
        revision,
        profile,
        pressure,
        pressure_source,
        memory_observation,
        demand,
        heavy_slots,
        preview: PreviewExecutionDecision {
            minimum_runtime_scale,
            trim,
            frame_store,
            viewer_gpu: PreviewViewerGpuExecutionDecision {
                grant: preview_viewer_gpu_resource_grant(profile.class, trim),
                clear_idle: trim == ResourceTrimRequest::Aggressive,
            },
            title_cache_budget_bytes: (title_cache / cache_divisor).max(1),
            effect_cache: PreviewEffectCacheDecision {
                max_entries: (effect_cache_entries / cache_divisor).max(1),
                max_bytes: (effect_cache_bytes / cache_divisor).max(1),
                max_working_bytes: effect_working_bytes,
                max_gpu_plan_entries: (effect_gpu_plan_entries / cache_divisor).max(1),
                max_gpu_plan_bytes: (effect_gpu_plan_bytes / cache_divisor).max(1),
            },
            cpu_composite_working_set: cpu_composite_working_set_grant(profile.class),
            heterogeneous_effects,
            cpu_color_processor_capacity: (cpu_color_processors / cache_divisor).max(1),
            visual_program_cache: PreparedVisualProgramCacheConfig::new(
                (visual_program_entries / cache_divisor).max(1),
                (visual_program_bytes / cache_divisor).max(1),
            )
            .with_lut_cache(LutPreparationCacheConfig::new(
                (lut_cache_entries / cache_divisor).max(1),
                (lut_cache_bytes / cache_divisor).max(1),
            )),
            seek_index_cache: PreviewSeekIndexCachePolicy::new(
                (seek_index_entries / cache_divisor).max(1),
                (seek_index_anchors / cache_divisor).max(1),
                (seek_index_bytes / cache_divisor).max(1),
            ),
            hardware_device_contexts: HwDeviceContextPoolPolicy::new(
                hardware_idle_contexts / cache_divisor,
            ),
        },
        audio: AudioExecutionDecision {
            source_entry_capacity: (audio_entries / audio_trim_divisor).max(1),
            source_byte_budget: (audio_bytes / audio_trim_divisor).max(1),
            decoder_session_capacity: (audio_sessions / audio_trim_divisor).max(1),
            idle_warmup_windows: audio_idle_warmup_windows,
            idle_warmup_dispatch_enabled: audio_warmup_dispatch,
            runtime_grant: audio_runtime_resource_grant(profile.class),
        },
        proxy: ProxyExecutionDecision {
            dispatch_enabled: proxy_dispatch,
            automatic_dispatch_enabled: automatic_dispatch,
            max_parallelism: bounded_parallelism(proxy_parallelism),
        },
        thumbnail: ThumbnailExecutionDecision {
            automatic_admission_enabled: automatic_dispatch,
            dispatch_enabled: thumbnail_dispatch,
            cache_budget_bytes: (thumbnail_cache / cache_divisor).max(1),
        },
        waveform: WaveformExecutionDecision {
            automatic_admission_enabled: automatic_dispatch,
            dispatch_enabled: waveform_dispatch,
            cache_budget_bytes: (waveform_cache / cache_divisor).max(1),
        },
        media_import: MediaImportExecutionDecision {
            dispatch_enabled: media_import_dispatch,
            max_parallelism: bounded_parallelism(import_parallelism),
        },
        media_asset_mutation: MediaAssetMutationExecutionDecision {
            dispatch_enabled: media_asset_mutation_dispatch,
        },
        export: ExportExecutionDecision {
            dispatch_enabled: export_dispatch,
            resource_policy: export_resource_policy(profile.class, cache_divisor),
        },
        ui: UiExecutionDecision {
            vector_icon_cache_entries: (vector_icon_entries / cache_divisor).max(1),
            vector_icon_cache_bytes: (vector_icon_bytes / cache_divisor).max(1),
        },
    }
}

fn preview_heterogeneous_effect_execution_decision(
    class: MachineResourceClass,
    effect_working_bytes: usize,
) -> PreviewHeterogeneousEffectExecutionDecision {
    // `max_batch_pixel_bytes` is a policy ceiling. Renderer admission counts
    // each immutable CPU input plus every Effects-owned frontier value for the
    // prepared route. One UHD input plus one frontier is 253.125 MiB, so
    // MinimumSupported admits that baseline shape; wider frontiers consume
    // proportionally more authority rather than being hidden by this policy.
    let max_batch_pixel_bytes = match class {
        MachineResourceClass::BelowMinimum | MachineResourceClass::UnknownConservative => {
            128 * MIB as u64
        }
        MachineResourceClass::MinimumSupported => 256 * MIB as u64,
        MachineResourceClass::Standard => 768 * MIB as u64,
        MachineResourceClass::Professional => 1_280 * MIB as u64,
    };
    let effect_working_bytes = effect_working_bytes as u64;
    PreviewHeterogeneousEffectExecutionDecision {
        cpu_prefix: HeterogeneousCpuPrefixBatchGrant::new(
            EffectExecutionSessionConfig::uncached(effect_working_bytes as usize),
            EffectGraphExecutionBudget::new(
                effect_working_bytes,
                effect_working_bytes,
                effect_working_bytes,
                PREVIEW_HETEROGENEOUS_MAX_MATERIALIZATIONS,
                PREVIEW_HETEROGENEOUS_MAX_STEPS,
            ),
            PREVIEW_HETEROGENEOUS_MAX_BATCH_ITEMS,
            max_batch_pixel_bytes,
        ),
        gpu_continuation: HeterogeneousGpuResourceGrant::new(
            effect_working_bytes,
            effect_working_bytes,
            PREVIEW_HETEROGENEOUS_MAX_MATERIALIZATIONS as u64,
            0,
        ),
    }
}

fn audio_runtime_resource_grant(class: MachineResourceClass) -> AudioRuntimeResourceGrant {
    match class {
        MachineResourceClass::BelowMinimum => {
            AudioRuntimeResourceGrant::new(16, 192 * MIB, 32 * MIB)
        }
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            AudioRuntimeResourceGrant::new(32, 384 * MIB, 64 * MIB)
        }
        MachineResourceClass::Standard => AudioRuntimeResourceGrant::new(64, 768 * MIB, 128 * MIB),
        MachineResourceClass::Professional => {
            AudioRuntimeResourceGrant::new(128, 1_536 * MIB, 256 * MIB)
        }
    }
}

fn cpu_composite_working_set_grant(class: MachineResourceClass) -> TimelineCpuWorkingSetGrant {
    match class {
        MachineResourceClass::BelowMinimum => TimelineCpuWorkingSetGrant {
            max_active_bytes: 256 * MIB as u64,
            max_retained_scratch_bytes: 128 * MIB as u64,
        },
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            TimelineCpuWorkingSetGrant {
                // One UHD Cross Dissolve plus one endpoint Effect result is
                // 506.25 MiB in RGBA32F.
                max_active_bytes: 512 * MIB as u64,
                max_retained_scratch_bytes: 256 * MIB as u64,
            }
        }
        MachineResourceClass::Standard => TimelineCpuWorkingSetGrant {
            max_active_bytes: 1024 * MIB as u64,
            max_retained_scratch_bytes: 512 * MIB as u64,
        },
        MachineResourceClass::Professional => TimelineCpuWorkingSetGrant {
            max_active_bytes: 2048 * MIB as u64,
            max_retained_scratch_bytes: 1024 * MIB as u64,
        },
    }
}

fn preview_viewer_gpu_resource_grant(
    class: MachineResourceClass,
    trim: ResourceTrimRequest,
) -> ViewerGpuExecutionResourceGrant {
    if class == MachineResourceClass::Professional {
        let professional = ViewerGpuExecutionResourceGrant::professional_realtime();
        return match trim {
            ResourceTrimRequest::None => professional,
            ResourceTrimRequest::Speculative => {
                professional.with_idle_limits(1, professional.max_idle_bytes())
            }
            ResourceTrimRequest::Aggressive => professional.with_idle_limits(0, 0),
        };
    }
    let (max_idle_per_contract, max_idle_bytes) = match class {
        MachineResourceClass::BelowMinimum => (1, 48 * MIB),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (2, 128 * MIB)
        }
        MachineResourceClass::Standard => (3, 256 * MIB),
        MachineResourceClass::Professional => unreachable!("handled above"),
    };
    let (max_active_texture_bytes, max_active_textures) = match class {
        MachineResourceClass::BelowMinimum => (384 * MIB as u64, 48),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (768 * MIB as u64, 64)
        }
        MachineResourceClass::Standard => (2 * 1024 * MIB as u64, 96),
        MachineResourceClass::Professional => unreachable!("handled above"),
    };
    let idle_grant = match trim {
        ResourceTrimRequest::None => {
            ViewerGpuExecutionResourceGrant::new(max_idle_per_contract, max_idle_bytes as u64)
        }
        ResourceTrimRequest::Speculative => {
            // Speculative pressure may retire duplicate textures, but the
            // byte grant must still hold one complete steady-state contract
            // set. Halving this bound made UHD CPU-YUV playback evict its
            // 126.6 MiB float working texture as the later 63.3 MiB display
            // textures returned, forcing synchronous device allocations back
            // into successor preparation. `max_per_contract = 1` removes the
            // optional duplicates without turning frame-to-frame reuse into a
            // disposable cache.
            ViewerGpuExecutionResourceGrant::new(1, max_idle_bytes as u64)
        }
        ResourceTrimRequest::Aggressive => ViewerGpuExecutionResourceGrant::new(0, 0),
    };
    idle_grant.with_active_limits(max_active_texture_bytes, max_active_textures)
}

fn export_gpu_output_active_grant(
    class: MachineResourceClass,
) -> RenderGpuOutputExecutionResourceGrant {
    let max_active_bytes = match class {
        MachineResourceClass::BelowMinimum => 384 * MIB as u64,
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            512 * MIB as u64
        }
        MachineResourceClass::Standard => 1024 * MIB as u64,
        MachineResourceClass::Professional => 2048 * MIB as u64,
    };
    // One CPU-origin output boundary owns an input texture, output texture,
    // and padded readback buffer. The fourth slot is a conservative contract
    // allowance; any future additional resource must still fit the byte grant.
    RenderGpuOutputExecutionResourceGrant::new(max_active_bytes, 4)
}

fn export_gpu_visual_active_grant(
    class: MachineResourceClass,
) -> GpuVisualFrameExecutionResourceGrant {
    let (max_active_bytes, max_active_textures) = match class {
        MachineResourceClass::BelowMinimum => (384 * MIB as u64, 48),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (768 * MIB as u64, 64)
        }
        MachineResourceClass::Standard => (2 * 1024 * MIB as u64, 96),
        MachineResourceClass::Professional => (4 * 1024 * MIB as u64, 160),
    };
    GpuVisualFrameExecutionResourceGrant::new(max_active_bytes, max_active_textures)
}

fn export_resource_policy(
    class: MachineResourceClass,
    cache_divisor: usize,
) -> ExportExecutionResourcePolicy {
    let (
        visual_program_entries,
        visual_program_bytes,
        lut_cache_entries,
        lut_cache_bytes,
        effect_cache_entries,
        effect_cache_bytes,
        effect_working_bytes,
        effect_gpu_plan_entries,
        effect_gpu_plan_bytes,
        heterogeneous_route_contract_entries,
        heterogeneous_route_contract_bytes,
        cpu_color_processor_capacity,
        gpu_output_idle_per_contract,
        gpu_output_idle_bytes,
        title_cache_entries,
        title_cache_bytes,
    ) = match class {
        MachineResourceClass::BelowMinimum => (
            8,
            16 * MIB,
            2,
            8 * MIB,
            8,
            32 * MIB,
            128 * MIB,
            8,
            MIB / 2,
            8,
            8 * EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
            8,
            0,
            0,
            4,
            16 * MIB,
        ),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => (
            16,
            32 * MIB,
            4,
            16 * MIB,
            16,
            64 * MIB,
            256 * MIB,
            16,
            MIB,
            16,
            16 * EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
            16,
            1,
            64 * MIB,
            8,
            32 * MIB,
        ),
        MachineResourceClass::Standard => (
            32,
            64 * MIB,
            8,
            32 * MIB,
            32,
            96 * MIB,
            384 * MIB,
            32,
            2 * MIB,
            32,
            32 * EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
            32,
            1,
            96 * MIB,
            16,
            64 * MIB,
        ),
        MachineResourceClass::Professional => (
            64,
            128 * MIB,
            16,
            64 * MIB,
            64,
            192 * MIB,
            768 * MIB,
            64,
            4 * MIB,
            64,
            64 * EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
            64,
            2,
            192 * MIB,
            32,
            128 * MIB,
        ),
    };
    let (audio_source_entries, audio_source_bytes, audio_decoder_sessions) = match class {
        MachineResourceClass::BelowMinimum => (8, 24 * MIB, 1),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (16, 64 * MIB, 2)
        }
        MachineResourceClass::Standard => (32, 128 * MIB, 4),
        MachineResourceClass::Professional => (64, 256 * MIB, 8),
    };
    let title_font_bytes = match class {
        MachineResourceClass::BelowMinimum => 64 * MIB,
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            128 * MIB
        }
        MachineResourceClass::Standard => 256 * MIB,
        MachineResourceClass::Professional => 512 * MIB,
    };
    ExportExecutionResourcePolicy {
        // Reachable-closure and per-frame working limits are correctness
        // admission grants, not optional residency. Pressure may delay the
        // next attempt and trim caches, but cannot make the same valid Export
        // snapshot fail merely because it was dispatched one tick later.
        visual_program_entries,
        visual_program_bytes,
        lut_cache_entries: (lut_cache_entries / cache_divisor).max(1),
        lut_cache_bytes: (lut_cache_bytes / cache_divisor).max(1),
        effect_cache_entries: (effect_cache_entries / cache_divisor).max(1),
        effect_cache_bytes: (effect_cache_bytes / cache_divisor).max(1),
        effect_working_bytes,
        cpu_composite_working_set: cpu_composite_working_set_grant(class),
        effect_gpu_plan_entries: (effect_gpu_plan_entries / cache_divisor).max(1),
        effect_gpu_plan_bytes: (effect_gpu_plan_bytes / cache_divisor).max(1),
        heterogeneous_route_contract_entries,
        heterogeneous_route_contract_bytes,
        cpu_color_processor_capacity: (cpu_color_processor_capacity / cache_divisor).max(1),
        gpu_output_idle_per_contract: gpu_output_idle_per_contract / cache_divisor,
        gpu_output_idle_bytes: (gpu_output_idle_bytes / cache_divisor) as u64,
        gpu_visual_active: export_gpu_visual_active_grant(class),
        gpu_output_active: export_gpu_output_active_grant(class),
        // The resident pool is a correctness grant frozen for the complete
        // attempt, not optional idle residency that pressure may trim.
        resident_encoder_surfaces: 8,
        resident_encoder_surface_bytes: 512 * MIB as u64,
        title_cache_entries: (title_cache_entries / cache_divisor).max(1),
        title_cache_bytes: (title_cache_bytes / cache_divisor).max(1),
        // Font bytes are immutable snapshot dependencies, not evictable cache
        // residency. Pressure may delay Export admission, but must not shrink
        // this correctness grant after the attempt starts.
        title_font_bytes,
        audio_source_cache: mondrian_media::AudioSourceCacheConfig::new(
            (audio_source_entries / cache_divisor).max(1),
            (audio_source_bytes / cache_divisor).max(1),
            (audio_decoder_sessions / cache_divisor).max(1),
        ),
        audio_runtime_grant: audio_runtime_resource_grant(class),
    }
}

fn preview_frame_store_config(
    class: MachineResourceClass,
    trim: ResourceTrimRequest,
) -> PreviewFrameStoreConfig {
    let (media_entries, media_bytes, resource_units, viewer_entries, viewer_bytes) = match class {
        MachineResourceClass::BelowMinimum => (24, 96 * MIB, 4, 12, 48 * MIB),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (48, 256 * MIB, 6, 24, 96 * MIB)
        }
        // Twenty compact 4K 10-bit 4:2:2 CPU frames fit in 640 MiB: the
        // current frame, the complete bounded future horizon, and three
        // physical decode reservations. This makes the temporal policy a
        // realizable Standard-machine contract instead of an abstract count
        // that byte admission silently shortens.
        MachineResourceClass::Standard => (96, 640 * MIB, 8, 48, 192 * MIB),
        MachineResourceClass::Professional => (128, 1024 * MIB, 12, 64, 256 * MIB),
    };
    let (current_entries, current_bytes, current_resource_units) = match class {
        MachineResourceClass::BelowMinimum => (4, 256 * MIB, 4),
        MachineResourceClass::UnknownConservative | MachineResourceClass::MinimumSupported => {
            (8, 512 * MIB, 8)
        }
        MachineResourceClass::Standard => (16, 1024 * MIB, 16),
        MachineResourceClass::Professional => (24, 2048 * MIB, 24),
    };
    let divisor = match trim {
        ResourceTrimRequest::None => 1,
        ResourceTrimRequest::Speculative => 2,
        ResourceTrimRequest::Aggressive => 4,
    };
    PreviewFrameStoreConfig {
        media_entry_capacity: (media_entries / divisor).max(1),
        media_byte_budget: (media_bytes / divisor).max(1),
        media_resource_unit_budget: (resource_units / divisor).max(1),
        current_media_working_set_entry_limit: current_entries,
        current_media_working_set_byte_limit: current_bytes,
        current_media_working_set_resource_unit_limit: current_resource_units,
        viewer_entry_capacity: (viewer_entries / divisor).max(1),
        viewer_byte_budget: (viewer_bytes / divisor).max(1),
        failure_entry_capacity: 192,
    }
}

fn classify_memory_pressure(
    previous: ExecutionResourcePressure,
    private_committed_bytes: Option<u64>,
    installed_memory_bytes: Option<u64>,
    total_physical_bytes: Option<u64>,
    available_physical_bytes: Option<u64>,
    memory_load_percent: Option<u32>,
) -> ExecutionResourcePressure {
    let process_observed = private_committed_bytes
        .zip(installed_memory_bytes)
        .filter(|(_, installed)| *installed != 0);
    let system_observed = total_physical_bytes
        .zip(available_physical_bytes)
        .filter(|(total, available)| *total != 0 && available <= total);
    let load_observed = memory_load_percent.filter(|load| *load <= 100);
    if process_observed.is_none() && system_observed.is_none() && load_observed.is_none() {
        return previous;
    }

    let ratio_at_least = |value: u64, total: u64, numerator: u64, denominator: u64| {
        u128::from(value) * u128::from(denominator) >= u128::from(total) * u128::from(numerator)
    };
    let process_at_least = |numerator: u64, denominator: u64| {
        process_observed.is_some_and(|(private, installed)| {
            ratio_at_least(private, installed, numerator, denominator)
        })
    };
    let available_threshold = |total: u64, numerator: u64, denominator: u64, floor: u64| {
        let ratio = (u128::from(total) * u128::from(numerator) / u128::from(denominator))
            .min(u128::from(u64::MAX)) as u64;
        ratio.max(floor)
    };
    let available_at_most = |numerator: u64, denominator: u64, floor: u64| {
        system_observed.is_some_and(|(total, available)| {
            available <= available_threshold(total, numerator, denominator, floor)
        })
    };
    let load_at_least = |threshold: u32| load_observed.is_some_and(|load| load >= threshold);

    const MIB_U64: u64 = 1024 * 1024;
    let critical_enter =
        process_at_least(3, 4) || available_at_most(1, 20, 512 * MIB_U64) || load_at_least(95);
    let critical_hold =
        process_at_least(2, 3) || available_at_most(2, 25, 768 * MIB_U64) || load_at_least(90);
    let elevated_enter =
        process_at_least(3, 5) || available_at_most(1, 10, 1024 * MIB_U64) || load_at_least(88);
    let elevated_hold =
        process_at_least(1, 2) || available_at_most(3, 20, 1536 * MIB_U64) || load_at_least(80);

    match previous {
        ExecutionResourcePressure::Critical if critical_hold => ExecutionResourcePressure::Critical,
        ExecutionResourcePressure::Critical if elevated_hold => ExecutionResourcePressure::Elevated,
        ExecutionResourcePressure::Elevated if critical_enter => {
            ExecutionResourcePressure::Critical
        }
        ExecutionResourcePressure::Elevated if elevated_hold => ExecutionResourcePressure::Elevated,
        _ if critical_enter => ExecutionResourcePressure::Critical,
        _ if elevated_enter => ExecutionResourcePressure::Elevated,
        _ => ExecutionResourcePressure::Nominal,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mondrian_platform::{ProcessMemoryProbe, ProcessMemoryScope, SystemMemoryProbe};

    use super::*;

    struct CountingMemoryProbe {
        calls: AtomicUsize,
        private_committed_bytes: u64,
    }

    impl ProcessMemoryProbe for CountingMemoryProbe {
        fn process_memory(
            &self,
            scope: ProcessMemoryScope,
        ) -> mondrian_platform::ProcessMemoryProbeResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let backend = match scope {
                ProcessMemoryScope::CurrentProcess => {
                    mondrian_platform::ProcessMemoryProbeBackend::WindowsCurrentProcessStatus
                }
                ProcessMemoryScope::ProductProcessTree => {
                    mondrian_platform::ProcessMemoryProbeBackend::WindowsToolhelpProcessTree
                }
            };
            mondrian_platform::ProcessMemoryProbeResult::observed(
                scope,
                backend,
                1,
                1,
                self.private_committed_bytes,
                self.private_committed_bytes,
                self.private_committed_bytes,
            )
        }
    }

    impl SystemMemoryProbe for CountingMemoryProbe {
        fn current_system_memory(&self) -> mondrian_platform::SystemMemoryProbeResult {
            mondrian_platform::SystemMemoryProbeResult::unsupported(
                "test supplies process-only evidence",
            )
        }
    }

    fn profile(class: MachineResourceClass) -> MachineResourceProfile {
        MachineResourceProfile {
            class,
            installed_memory_bytes: None,
            logical_cpu_count: 8,
        }
    }

    #[test]
    fn unknown_capacity_is_fail_safe_minimum() {
        let profile = MachineResourceProfile::from_capacity(None, 0);
        assert_eq!(profile.class, MachineResourceClass::UnknownConservative);
        assert_eq!(profile.logical_cpu_count, 1);
    }

    #[test]
    fn measured_capacity_does_not_claim_support_below_eight_gib() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            MachineResourceProfile::from_capacity(Some(8 * GIB), 4).class,
            MachineResourceClass::MinimumSupported
        );
        assert_eq!(
            MachineResourceProfile::from_capacity(Some(8 * GIB - 1), 4).class,
            MachineResourceClass::BelowMinimum
        );
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::BelowMinimum));
        let decision = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            preview_realtime: true,
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert_eq!(
            decision.preview.minimum_runtime_scale,
            PreviewResolutionScale::Quarter
        );
        assert_eq!(decision.preview.effect_cache.max_working_bytes, 128 * MIB);
        assert_eq!(
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::UnknownConservative))
                .decision()
                .preview
                .effect_cache
                .max_working_bytes,
            128 * MIB
        );
    }

    #[test]
    fn heterogeneous_preview_grants_cover_one_single_frontier_uhd_route_at_eight_gib() {
        const UHD_RGBA_F32_FRAME_BYTES: u64 = 3_840 * 2_160 * 16;
        const UHD_CPU_PREFIX_RETAINED_BYTES: u64 = UHD_RGBA_F32_FRAME_BYTES * 2;
        let minimum =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::MinimumSupported))
                .decision();
        let standard =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard)).decision();

        let minimum_cpu = minimum.preview.heterogeneous_effects.cpu_prefix_grant();
        assert!(minimum_cpu.max_batch_items() >= 1);
        assert!(
            minimum_cpu.max_batch_pixel_bytes() >= UHD_CPU_PREFIX_RETAINED_BYTES,
            "8 GiB must admit one UHD Float32 input plus one CPU frontier value"
        );
        assert!(
            minimum_cpu.effect_session().max_working_bytes >= 256 * MIB,
            "Gaussian's two-frame transient working set must fit"
        );
        assert!(minimum_cpu.graph_execution().max_host_bytes() >= UHD_CPU_PREFIX_RETAINED_BYTES);
        let minimum_gpu = minimum.preview.heterogeneous_effects.gpu_continuation_grant();
        assert!(minimum_gpu.max_upload_bytes() >= UHD_RGBA_F32_FRAME_BYTES);
        assert!(
            minimum_gpu.max_device_bytes() >= UHD_CPU_PREFIX_RETAINED_BYTES,
            "the GPU suffix must retain both sides of one point-effect pass"
        );
        assert_eq!(minimum_gpu.max_readback_bytes(), 0);

        assert!(
            standard
                .preview
                .heterogeneous_effects
                .cpu_prefix_grant()
                .max_batch_pixel_bytes()
                > minimum_cpu.max_batch_pixel_bytes()
        );
    }

    #[test]
    fn viewer_gpu_grants_cover_uhd_main10_float16_steady_state() {
        const UHD_PIXELS: u64 = 3_840 * 2_160;
        // The renderer estimator's conservative identity-effect P010 path:
        // padded native bridge (2x visible P010), encoded RGB intermediate,
        // working-linear input, two working accumulators, one RGBA16F Program
        // Output, and one RGBA16F continuity reserve for the current lease.
        // Monitor adaptation, spatial filtering, calibration, scopes,
        // Transitions, and non-identity Effects are separate larger contracts.
        const NATIVE_BRIDGE_BYTES: u64 =
            UHD_PIXELS * 3 * mondrian_renderer::GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO;
        const ENCODED_RGB_BYTES: u64 = UHD_PIXELS * 8;
        const WORKING_INPUT_BYTES: u64 = UHD_PIXELS * 16;
        const WORKING_COMPOSITE_BYTES: u64 = UHD_PIXELS * 16 * 2;
        const PROGRAM_OUTPUT_BYTES: u64 = UHD_PIXELS * 8;
        const PRESENTATION_CONTINUITY_BYTES: u64 = UHD_PIXELS * 8;
        const REQUIRED_BYTES: u64 = NATIVE_BRIDGE_BYTES
            + ENCODED_RGB_BYTES
            + WORKING_INPUT_BYTES
            + WORKING_COMPOSITE_BYTES
            + PROGRAM_OUTPUT_BYTES
            + PRESENTATION_CONTINUITY_BYTES;
        const REQUIRED_TEXTURES: u64 = 3 + 2 + 1 + 1;

        for class in [
            MachineResourceClass::MinimumSupported,
            MachineResourceClass::Standard,
        ] {
            let grant = preview_viewer_gpu_resource_grant(class, ResourceTrimRequest::None);
            assert!(
                grant.max_active_texture_bytes() >= REQUIRED_BYTES,
                "{class:?} must admit the basic UHD Main10 Float16 steady state"
            );
            assert!(
                grant.max_active_textures() >= REQUIRED_TEXTURES,
                "{class:?} must admit every basic UHD Main10 texture owner"
            );
        }
    }

    #[test]
    fn pressure_cannot_reinterpret_a_frozen_heterogeneous_preview_route() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let nominal = coordinator.decision();
        let critical = coordinator.observe_pressure(ExecutionResourcePressure::Critical);

        assert_eq!(
            critical.preview.heterogeneous_effects,
            nominal.preview.heterogeneous_effects
        );
    }

    #[test]
    fn pressure_trims_export_residency_without_shrinking_correctness_grants() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::MinimumSupported));
        let nominal = coordinator.decision();
        let elevated = coordinator.observe_pressure(ExecutionResourcePressure::Elevated);

        assert_eq!(
            elevated.export.resource_policy.visual_program_entries,
            nominal.export.resource_policy.visual_program_entries
        );
        assert_eq!(
            elevated.export.resource_policy.visual_program_bytes,
            nominal.export.resource_policy.visual_program_bytes
        );
        assert_eq!(
            elevated.export.resource_policy.effect_working_bytes,
            nominal.export.resource_policy.effect_working_bytes
        );
        assert_eq!(
            elevated.export.resource_policy.cpu_composite_working_set,
            nominal.export.resource_policy.cpu_composite_working_set
        );
        assert_eq!(
            elevated.preview.cpu_composite_working_set,
            nominal.preview.cpu_composite_working_set
        );
        assert_eq!(
            elevated.preview.viewer_gpu.grant.max_active_texture_bytes(),
            nominal.preview.viewer_gpu.grant.max_active_texture_bytes()
        );
        assert_eq!(
            elevated.preview.viewer_gpu.grant.max_active_textures(),
            nominal.preview.viewer_gpu.grant.max_active_textures()
        );
        assert_eq!(
            elevated.export.resource_policy.gpu_visual_active,
            nominal.export.resource_policy.gpu_visual_active
        );
        assert_eq!(
            elevated.export.resource_policy.gpu_output_active,
            nominal.export.resource_policy.gpu_output_active
        );
        assert_eq!(
            elevated.export.resource_policy.heterogeneous_route_contract_entries,
            nominal.export.resource_policy.heterogeneous_route_contract_entries
        );
        assert_eq!(
            elevated.export.resource_policy.heterogeneous_route_contract_bytes,
            nominal.export.resource_policy.heterogeneous_route_contract_bytes
        );
        assert_eq!(
            elevated.export.resource_policy.audio_runtime_grant,
            nominal.export.resource_policy.audio_runtime_grant
        );
        assert_eq!(elevated.audio.runtime_grant, nominal.audio.runtime_grant);
        assert!(
            elevated.export.resource_policy.effect_cache_bytes
                < nominal.export.resource_policy.effect_cache_bytes
        );
        assert!(
            elevated.export.resource_policy.effect_gpu_plan_entries
                < nominal.export.resource_policy.effect_gpu_plan_entries
        );
        assert!(
            elevated.export.resource_policy.effect_gpu_plan_bytes
                < nominal.export.resource_policy.effect_gpu_plan_bytes
        );
        assert!(
            elevated.export.resource_policy.title_cache_bytes
                < nominal.export.resource_policy.title_cache_bytes
        );
        assert!(
            elevated.export.resource_policy.audio_source_cache.byte_budget
                < nominal.export.resource_policy.audio_source_cache.byte_budget
        );
    }

    #[test]
    fn realtime_demand_always_admits_preview_and_audio_and_pauses_background_dispatch() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let decision = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            preview_realtime: true,
            audio_realtime: true,
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });

        assert!(!decision.proxy.dispatch_enabled);
        assert!(!decision.proxy.automatic_dispatch_enabled);
        assert!(!decision.media_import.dispatch_enabled);
        assert!(!decision.media_asset_mutation.dispatch_enabled);
        assert!(!decision.export.dispatch_enabled);
        assert_eq!(decision.audio.idle_warmup_windows, 0);
        assert!(!decision.audio.idle_warmup_dispatch_enabled);
    }

    #[test]
    fn minimum_realtime_class_degrades_only_runtime_preview_scale() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::MinimumSupported));
        let decision = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            preview_realtime: true,
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert_eq!(
            decision.preview.minimum_runtime_scale,
            PreviewResolutionScale::Half
        );
        assert_eq!(decision.preview.trim, ResourceTrimRequest::Speculative);
        assert_eq!(decision.audio.source_entry_capacity, 16);
        assert_eq!(decision.audio.source_byte_budget, 64 * MIB);
        assert_eq!(decision.audio.decoder_session_capacity, 2);
        assert_eq!(decision.audio.idle_warmup_windows, 0);
        assert!(!decision.audio.idle_warmup_dispatch_enabled);
        assert_eq!(decision.preview.viewer_gpu.grant.max_idle_per_contract(), 1);
        assert_eq!(
            decision.preview.viewer_gpu.grant.max_idle_bytes(),
            128 * MIB as u64
        );
        assert!(!decision.preview.viewer_gpu.clear_idle);
    }

    #[test]
    fn eight_sixteen_and_thirty_two_gib_classes_publish_explicit_audio_budgets() {
        let minimum =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::MinimumSupported))
                .decision();
        let standard =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard)).decision();
        let professional =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Professional))
                .decision();

        assert_eq!(
            (
                minimum.audio.source_entry_capacity,
                minimum.audio.source_byte_budget,
                minimum.audio.decoder_session_capacity,
                minimum.audio.idle_warmup_windows,
            ),
            (16, 64 * MIB, 2, 1)
        );
        assert_eq!(
            (
                standard.audio.source_entry_capacity,
                standard.audio.source_byte_budget,
                standard.audio.decoder_session_capacity,
                standard.audio.idle_warmup_windows,
            ),
            (32, 128 * MIB, 4, 2)
        );
        assert_eq!(
            (
                professional.audio.source_entry_capacity,
                professional.audio.source_byte_budget,
                professional.audio.decoder_session_capacity,
                professional.audio.idle_warmup_windows,
            ),
            (64, 256 * MIB, 8, 3)
        );
        assert_eq!(minimum.preview.title_cache_budget_bytes, 32 * MIB);
        assert_eq!(standard.preview.title_cache_budget_bytes, 64 * MIB);
        assert_eq!(professional.preview.title_cache_budget_bytes, 128 * MIB);
        assert_eq!(minimum.preview.effect_cache.max_bytes, 64 * MIB);
        assert_eq!(standard.preview.effect_cache.max_bytes, 192 * MIB);
        assert_eq!(professional.preview.effect_cache.max_bytes, 384 * MIB);
        assert_eq!(minimum.preview.effect_cache.max_working_bytes, 256 * MIB);
        assert_eq!(standard.preview.effect_cache.max_working_bytes, 384 * MIB);
        assert_eq!(
            professional.preview.effect_cache.max_working_bytes,
            768 * MIB
        );
        assert_eq!(minimum.preview.effect_cache.max_gpu_plan_entries, 16);
        assert_eq!(standard.preview.effect_cache.max_gpu_plan_bytes, 2 * MIB);
        assert_eq!(
            professional.preview.effect_cache.max_gpu_plan_bytes,
            4 * MIB
        );
        assert_eq!(minimum.preview.viewer_gpu.grant.max_idle_per_contract(), 2);
        assert_eq!(
            minimum.preview.viewer_gpu.grant.max_idle_bytes(),
            128 * MIB as u64
        );
        assert_eq!(standard.preview.viewer_gpu.grant.max_idle_per_contract(), 3);
        assert_eq!(
            standard.preview.viewer_gpu.grant.max_idle_bytes(),
            256 * MIB as u64
        );
        assert_eq!(
            professional.preview.viewer_gpu.grant.max_idle_per_contract(),
            3
        );
        assert_eq!(
            professional.preview.viewer_gpu.grant.max_idle_bytes(),
            mondrian_renderer::PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_TEXTURE_BYTES
        );
        assert_eq!(
            minimum.preview.viewer_gpu.grant.max_active_texture_bytes(),
            768 * MIB as u64
        );
        assert_eq!(
            standard.preview.viewer_gpu.grant.max_active_texture_bytes(),
            2 * 1024 * MIB as u64
        );
        assert_eq!(
            professional.preview.viewer_gpu.grant.max_active_texture_bytes(),
            4 * 1024 * MIB as u64
        );
        assert_eq!(
            (
                minimum.preview.viewer_gpu.grant.max_active_textures(),
                standard.preview.viewer_gpu.grant.max_active_textures(),
                professional.preview.viewer_gpu.grant.max_active_textures(),
            ),
            (64, 96, 160)
        );
        assert_eq!(
            minimum.export.resource_policy.gpu_visual_active.max_active_bytes(),
            768 * MIB as u64
        );
        assert_eq!(
            standard.export.resource_policy.gpu_visual_active.max_active_bytes(),
            2 * 1024 * MIB as u64
        );
        assert_eq!(
            professional.export.resource_policy.gpu_visual_active.max_active_bytes(),
            4 * 1024 * MIB as u64
        );
        assert_eq!(
            (
                minimum.export.resource_policy.gpu_visual_active.max_active_textures(),
                standard.export.resource_policy.gpu_visual_active.max_active_textures(),
                professional.export.resource_policy.gpu_visual_active.max_active_textures(),
            ),
            (64, 96, 160)
        );
        assert_eq!(
            minimum.export.resource_policy.gpu_output_active.max_active_bytes(),
            512 * MIB as u64
        );
        assert_eq!(
            standard.export.resource_policy.gpu_output_active.max_active_bytes(),
            1024 * MIB as u64
        );
        assert_eq!(
            professional.export.resource_policy.gpu_output_active.max_active_bytes(),
            2048 * MIB as u64
        );
        assert_eq!(
            (
                minimum.export.resource_policy.gpu_output_active.max_active_resources(),
                standard.export.resource_policy.gpu_output_active.max_active_resources(),
                professional.export.resource_policy.gpu_output_active.max_active_resources(),
            ),
            (4, 4, 4)
        );
        assert_eq!(minimum.export.resource_policy.resident_encoder_surfaces, 8);
        assert_eq!(standard.export.resource_policy.resident_encoder_surfaces, 8);
        assert_eq!(
            professional.export.resource_policy.resident_encoder_surfaces,
            8
        );
        assert_eq!(
            minimum.export.resource_policy.resident_encoder_surface_bytes,
            512 * MIB as u64
        );
        assert_eq!(
            standard.export.resource_policy.resident_encoder_surface_bytes,
            512 * MIB as u64
        );
        assert_eq!(
            professional.export.resource_policy.resident_encoder_surface_bytes,
            512 * MIB as u64
        );
    }

    #[test]
    fn machine_classes_publish_exact_one_two_and_four_domain_slot_capacity() {
        let demand = ExecutionResourceDemandSnapshot {
            proxy: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            thumbnail: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            waveform: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            audio_warmup: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            media_import: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            media_asset_mutation: ExecutionDomainDemand {
                queued: 1,
                ..ExecutionDomainDemand::default()
            },
            export: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            ..ExecutionResourceDemandSnapshot::default()
        };

        for (class, expected) in [
            (MachineResourceClass::MinimumSupported, 1),
            (MachineResourceClass::Standard, 2),
            (MachineResourceClass::Professional, 4),
        ] {
            let coordinator = ExecutionResourceCoordinator::new(profile(class));
            assert_eq!(
                coordinator.update_demand(demand).heavy_slots.allocation().len(),
                expected,
                "unexpected cross-domain capacity for {class:?}"
            );
        }
    }

    #[test]
    fn pressure_reduces_audio_residency_and_idle_warmup_not_mix_admission() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let nominal = coordinator.decision();
        let elevated = coordinator.observe_pressure(ExecutionResourcePressure::Elevated);

        assert_eq!(
            elevated.audio.source_byte_budget,
            nominal.audio.source_byte_budget / 2
        );
        assert_eq!(elevated.audio.idle_warmup_windows, 1);
        assert!(!elevated.audio.idle_warmup_dispatch_enabled);

        let admitted = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            audio_warmup: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert_eq!(admitted.audio.idle_warmup_windows, 1);
        assert!(admitted.audio.idle_warmup_dispatch_enabled);

        let critical = coordinator.observe_pressure(ExecutionResourcePressure::Critical);
        assert_eq!(
            critical.audio.source_byte_budget,
            nominal.audio.source_byte_budget / 4
        );
        assert_eq!(critical.audio.idle_warmup_windows, 0);
        assert!(!critical.audio.idle_warmup_dispatch_enabled);
    }

    #[test]
    fn app_applies_audio_budget_changes_to_the_live_source_cache() {
        let mut app = AppState::new();
        app.execution_resources =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));

        app.refresh_internal_execution_resource_decision();
        let nominal = app.audio_source_cache_diagnostics();
        assert_eq!(nominal.entry_capacity, 32);
        assert_eq!(nominal.byte_budget, 128 * MIB);
        assert_eq!(nominal.decoder_session_capacity, 4);

        app.observe_execution_resource_pressure(ExecutionResourcePressure::Critical);
        let critical = app.audio_source_cache_diagnostics();
        assert_eq!(critical.entry_capacity, 8);
        assert_eq!(critical.byte_budget, 32 * MIB);
        assert_eq!(critical.decoder_session_capacity, 1);
        assert_eq!(critical.budget_reconfigurations, 2);
        assert_eq!(critical.decoder_capacity_reconfigurations, 2);
    }

    #[test]
    fn explicit_heavy_demand_receives_a_slot_and_suppresses_new_speculation() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Professional));
        let decision = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            media_import: ExecutionDomainDemand {
                queued: 2,
                running: 1,
                user_initiated: 3,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });

        assert!(decision
            .heavy_slots
            .allocation()
            .admits(ExecutionResourceSlotDomain::MediaImport));
        assert!(decision.media_import.dispatch_enabled);
        assert!(!decision.export.dispatch_enabled);
        assert!(!decision.proxy.dispatch_enabled);
        assert!(!decision.proxy.automatic_dispatch_enabled);
        assert!(!decision.thumbnail.automatic_admission_enabled);
        assert!(!decision.thumbnail.dispatch_enabled);
        assert!(!decision.waveform.automatic_admission_enabled);
        assert!(!decision.waveform.dispatch_enabled);
    }

    #[test]
    fn one_slot_class_retains_running_domain_then_hands_off_without_overlap() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::MinimumSupported));
        let both = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            media_import: ExecutionDomainDemand {
                queued: 1,
                running: 1,
                user_initiated: 2,
                ..ExecutionDomainDemand::default()
            },
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(both.media_import.dispatch_enabled);
        assert!(!both.export.dispatch_enabled);

        let rotated = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(!rotated.media_import.dispatch_enabled);
        assert!(!rotated.export.dispatch_enabled);
        assert!(rotated
            .heavy_slots
            .domains_to_close()
            .contains(ExecutionResourceSlotDomain::MediaImport));
        assert!(rotated.heavy_slots.domains_to_open_after_close().is_empty());
        assert!(coordinator
            .acknowledge_heavy_slot_close(&rotated, ExecutionResourceSlotDomain::MediaImport));

        let raced = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            media_import: ExecutionDomainDemand {
                running: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(!raced.media_import.dispatch_enabled);
        assert!(!raced.export.dispatch_enabled);

        let handed_off = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(handed_off.export.dispatch_enabled);
    }

    #[test]
    fn window_owned_domain_close_ack_reaches_the_allocator_and_still_requires_fresh_demand() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::MinimumSupported));
        let mut app = AppState::new();
        app.execution_resources = Arc::clone(&coordinator);
        let thumbnail_demand = ExecutionResourceDemandSnapshot {
            thumbnail: ExecutionDomainDemand { queued: 1, ..ExecutionDomainDemand::default() },
            ..ExecutionResourceDemandSnapshot::default()
        };
        let admitted = coordinator.update_demand(thumbnail_demand);
        assert!(admitted.thumbnail.dispatch_enabled);

        let closing = coordinator.observe_pressure(ExecutionResourcePressure::Critical);
        assert!(closing
            .heavy_slots
            .domains_to_close()
            .contains(ExecutionResourceSlotDomain::Thumbnail));
        assert!(app.acknowledge_external_execution_resource_domain_closed(
            &closing,
            ExecutionResourceSlotDomain::Thumbnail,
        ));

        let stale = coordinator.observe_pressure(ExecutionResourcePressure::Nominal);
        assert!(!stale.thumbnail.dispatch_enabled);
        let internal_only = app.refresh_internal_execution_resource_decision();
        assert!(
            !internal_only.thumbnail.dispatch_enabled,
            "App-internal diagnostics cannot stand in for a fresh Window-owned observation"
        );
        let fresh = coordinator.update_demand(thumbnail_demand);
        assert!(fresh.thumbnail.dispatch_enabled);
    }

    #[test]
    fn critical_pressure_is_bounded_and_recovers_without_losing_explicit_work() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Professional));
        coordinator.update_demand(ExecutionResourceDemandSnapshot {
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        let critical = coordinator.observe_pressure(ExecutionResourcePressure::Critical);
        assert!(!critical.export.dispatch_enabled);
        assert!(!critical.thumbnail.automatic_admission_enabled);
        assert!(!critical.thumbnail.dispatch_enabled);
        assert_eq!(
            critical.preview.minimum_runtime_scale,
            PreviewResolutionScale::Quarter
        );
        assert_eq!(critical.preview.trim, ResourceTrimRequest::Aggressive);
        assert_eq!(critical.preview.viewer_gpu.grant.max_idle_per_contract(), 0);
        assert_eq!(critical.preview.viewer_gpu.grant.max_idle_bytes(), 0);
        assert!(critical.preview.viewer_gpu.clear_idle);
        assert!(coordinator
            .acknowledge_heavy_slot_close(&critical, ExecutionResourceSlotDomain::Export));

        let pressure_recovered = coordinator.observe_pressure(ExecutionResourcePressure::Nominal);
        assert!(
            !pressure_recovered.export.dispatch_enabled,
            "cached pre-close demand cannot prove that Export drained"
        );
        assert!(!pressure_recovered.preview.viewer_gpu.clear_idle);

        let fresh = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            export: ExecutionDomainDemand {
                queued: 1,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(fresh.export.dispatch_enabled);
        assert!(!fresh.media_import.dispatch_enabled);
        assert_eq!(fresh.preview.viewer_gpu.grant.max_idle_per_contract(), 3);
        assert_eq!(
            fresh.preview.viewer_gpu.grant.max_idle_bytes(),
            mondrian_renderer::PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_TEXTURE_BYTES
        );
        assert!(fresh.revision > critical.revision);
    }

    #[test]
    fn preview_residency_budget_remains_constrained_for_the_complete_pressure_state() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let nominal = coordinator.decision();
        let critical = coordinator.observe_pressure(ExecutionResourcePressure::Critical);

        assert!(
            critical.preview.frame_store.media_byte_budget
                < nominal.preview.frame_store.media_byte_budget
        );
        assert!(
            critical.preview.frame_store.viewer_byte_budget
                < nominal.preview.frame_store.viewer_byte_budget
        );
        assert!(
            critical.preview.frame_store.media_resource_unit_budget
                < nominal.preview.frame_store.media_resource_unit_budget
        );
        assert_eq!(
            critical.preview.frame_store.current_media_working_set_entry_limit,
            nominal.preview.frame_store.current_media_working_set_entry_limit
        );
        assert_eq!(
            critical.preview.frame_store.current_media_working_set_byte_limit,
            nominal.preview.frame_store.current_media_working_set_byte_limit
        );
        assert_eq!(
            critical.preview.frame_store.current_media_working_set_resource_unit_limit,
            nominal.preview.frame_store.current_media_working_set_resource_unit_limit
        );
        assert_eq!(
            critical.preview.title_cache_budget_bytes,
            nominal.preview.title_cache_budget_bytes / 4
        );
        assert_eq!(
            critical.preview.effect_cache.max_bytes,
            nominal.preview.effect_cache.max_bytes / 4
        );
        assert_eq!(
            critical.preview.effect_cache.max_gpu_plan_bytes,
            nominal.preview.effect_cache.max_gpu_plan_bytes / 4
        );
        assert_eq!(
            critical.preview.effect_cache.max_working_bytes,
            nominal.preview.effect_cache.max_working_bytes,
            "pressure trims optional cache residency, not an admitted frame working set"
        );
        assert_eq!(
            coordinator.decision().preview.frame_store,
            critical.preview.frame_store,
            "unchanged Critical pressure must retain its lower residency limits"
        );
    }

    #[test]
    fn current_preview_working_set_grants_are_machine_bound_and_never_trimmed() {
        let cases = [
            (MachineResourceClass::BelowMinimum, 4, 256 * MIB, 4),
            (MachineResourceClass::UnknownConservative, 8, 512 * MIB, 8),
            (MachineResourceClass::MinimumSupported, 8, 512 * MIB, 8),
            (MachineResourceClass::Standard, 16, 1024 * MIB, 16),
            (MachineResourceClass::Professional, 24, 2048 * MIB, 24),
        ];
        for (class, entries, bytes, resource_units) in cases {
            let nominal = preview_frame_store_config(class, ResourceTrimRequest::None);
            let aggressive = preview_frame_store_config(class, ResourceTrimRequest::Aggressive);
            assert_eq!(nominal.current_media_working_set_entry_limit, entries);
            assert_eq!(nominal.current_media_working_set_byte_limit, bytes);
            assert_eq!(
                nominal.current_media_working_set_resource_unit_limit,
                resource_units
            );
            assert_eq!(
                aggressive.current_media_working_set_entry_limit,
                nominal.current_media_working_set_entry_limit
            );
            assert_eq!(
                aggressive.current_media_working_set_byte_limit,
                nominal.current_media_working_set_byte_limit
            );
            assert_eq!(
                aggressive.current_media_working_set_resource_unit_limit,
                nominal.current_media_working_set_resource_unit_limit
            );
        }
    }

    #[test]
    fn standard_preview_residency_physically_closes_the_4k_422_10_bit_horizon() {
        const UHD_WIDTH: usize = 3840;
        const UHD_HEIGHT: usize = 2160;
        const YUV_422_10_BIT_BYTES_PER_PIXEL: usize = 4;
        const STANDARD_DECODE_RESERVATIONS: usize = 3;

        let compact_frame_bytes = UHD_WIDTH
            .saturating_mul(UHD_HEIGHT)
            .saturating_mul(YUV_422_10_BIT_BYTES_PER_PIXEL);
        let required_owners = 1usize
            .saturating_add(MAX_BOUNDED_VIDEO_PREROLL_FRAMES)
            .saturating_add(STANDARD_DECODE_RESERVATIONS);
        let standard =
            preview_frame_store_config(MachineResourceClass::Standard, ResourceTrimRequest::None);

        assert!(standard.media_byte_budget >= compact_frame_bytes.saturating_mul(required_owners));
    }

    #[test]
    fn process_memory_pressure_uses_hysteresis_instead_of_threshold_flapping() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let installed = 16 * GIB;
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Nominal,
                Some(10 * GIB),
                Some(installed),
                None,
                None,
                None,
            ),
            ExecutionResourcePressure::Elevated
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Elevated,
                Some(9 * GIB),
                Some(installed),
                None,
                None,
                None,
            ),
            ExecutionResourcePressure::Elevated
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Elevated,
                Some(7 * GIB),
                Some(installed),
                None,
                None,
                None,
            ),
            ExecutionResourcePressure::Nominal
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Nominal,
                Some(13 * GIB),
                Some(installed),
                None,
                None,
                None,
            ),
            ExecutionResourcePressure::Critical
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Critical,
                Some(11 * GIB),
                Some(installed),
                None,
                None,
                None,
            ),
            ExecutionResourcePressure::Critical
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Critical,
                Some(9 * GIB),
                Some(installed),
                None,
                None,
                None,
            ),
            ExecutionResourcePressure::Elevated
        );
    }

    #[test]
    fn system_available_memory_catches_child_and_external_pressure_with_hysteresis() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let total = 16 * GIB;
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Nominal,
                Some(GIB),
                Some(total),
                Some(total),
                Some(400 * 1024 * 1024),
                Some(96),
            ),
            ExecutionResourcePressure::Critical
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Critical,
                Some(GIB),
                Some(total),
                Some(total),
                Some(GIB),
                Some(89),
            ),
            ExecutionResourcePressure::Critical
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Critical,
                Some(GIB),
                Some(total),
                Some(total),
                Some(2 * GIB),
                Some(85),
            ),
            ExecutionResourcePressure::Elevated
        );
        assert_eq!(
            classify_memory_pressure(
                ExecutionResourcePressure::Elevated,
                Some(GIB),
                Some(total),
                Some(total),
                Some(4 * GIB),
                Some(70),
            ),
            ExecutionResourcePressure::Nominal
        );
    }

    #[test]
    fn process_pressure_probe_runs_on_one_injectable_monotonic_cadence() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let coordinator = ExecutionResourceCoordinator::new(MachineResourceProfile {
            class: MachineResourceClass::Standard,
            installed_memory_bytes: Some(16 * GIB),
            logical_cpu_count: 8,
        });
        let probe = CountingMemoryProbe {
            calls: AtomicUsize::new(0),
            private_committed_bytes: 10 * GIB,
        };

        assert!(coordinator.observe_pressure_from_probe_at(&probe, Duration::ZERO));
        let first = coordinator.decision();
        assert_eq!(first.pressure, ExecutionResourcePressure::Elevated);
        assert!(!coordinator.observe_pressure_from_probe_at(&probe, Duration::from_millis(999),));
        assert_eq!(coordinator.decision().revision, first.revision);
        assert!(coordinator.observe_pressure_from_probe_at(&probe, Duration::from_secs(1),));
        assert_eq!(probe.calls.load(Ordering::Relaxed), 2);
        let second = coordinator.decision();
        assert!(
            second.revision > first.revision,
            "a fresh native sample remains auditable even when policy is stable"
        );
        assert_eq!(
            second.memory_observation.as_ref().map(|observation| observation.observed_at),
            Some(Duration::from_secs(1))
        );
        assert!(coordinator.next_observation_deadline() > Instant::now());
    }

    #[test]
    fn native_memory_runtime_never_runs_a_blocking_probe_on_its_caller() {
        struct BlockingMemoryProbe {
            entered: std::sync::mpsc::SyncSender<()>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
        }

        impl ProcessMemoryProbe for BlockingMemoryProbe {
            fn process_memory(
                &self,
                scope: ProcessMemoryScope,
            ) -> mondrian_platform::ProcessMemoryProbeResult {
                self.entered.send(()).expect("publish probe entry");
                self.release.lock().recv().expect("release blocking probe");
                mondrian_platform::ProcessMemoryProbeResult::observed(
                    scope,
                    mondrian_platform::ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                    1,
                    1,
                    1024,
                    1024,
                    1024,
                )
            }
        }

        impl SystemMemoryProbe for BlockingMemoryProbe {
            fn current_system_memory(&self) -> mondrian_platform::SystemMemoryProbeResult {
                mondrian_platform::SystemMemoryProbeResult::unsupported("test system probe")
            }
        }

        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let mut runtime = NativeMemoryObservationRuntime::start_with_probe(BlockingMemoryProbe {
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        })
        .expect("start native memory runtime");

        assert!(runtime.request(Duration::from_secs(7)));
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker entered blocking probe after caller returned");
        let (observation, disconnected) = runtime.drain_latest();
        assert!(observation.is_none());
        assert!(!disconnected);
        release_sender.send(()).expect("release native probe");
        let deadline = Instant::now() + Duration::from_secs(1);
        let observation = loop {
            if let (Some(observation), _) = runtime.drain_latest() {
                break observation;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for native observation"
            );
            std::thread::yield_now();
        };

        assert_eq!(observation.observed_at, Duration::from_secs(7));
        runtime.stop_worker();
    }

    #[test]
    fn supported_probe_failure_is_elevated_and_preserved_as_evidence() {
        struct FailedMemoryProbe;

        impl ProcessMemoryProbe for FailedMemoryProbe {
            fn process_memory(
                &self,
                scope: ProcessMemoryScope,
            ) -> mondrian_platform::ProcessMemoryProbeResult {
                mondrian_platform::ProcessMemoryProbeResult::failed(
                    scope,
                    mondrian_platform::ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                    1,
                    4,
                    "query failed",
                )
            }
        }

        impl SystemMemoryProbe for FailedMemoryProbe {
            fn current_system_memory(&self) -> mondrian_platform::SystemMemoryProbeResult {
                mondrian_platform::SystemMemoryProbeResult::failed(
                    mondrian_platform::SystemMemoryProbeBackend::WindowsGlobalMemoryStatus,
                    "query failed",
                )
            }
        }

        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        assert!(coordinator.observe_pressure_from_probe_at(&FailedMemoryProbe, Duration::ZERO));
        let decision = coordinator.decision();
        assert_eq!(decision.pressure, ExecutionResourcePressure::Elevated);
        let evidence =
            decision.memory_observation.as_ref().expect("completed sample must be retained");
        assert_eq!(
            evidence.product_process_tree.error.as_deref(),
            Some("query failed")
        );
        assert_eq!(evidence.system.error.as_deref(), Some("query failed"));
    }

    #[test]
    fn current_process_sample_cannot_substitute_for_product_process_tree_pressure() {
        struct CurrentOnlyMemoryProbe;

        impl ProcessMemoryProbe for CurrentOnlyMemoryProbe {
            fn process_memory(
                &self,
                _scope: ProcessMemoryScope,
            ) -> mondrian_platform::ProcessMemoryProbeResult {
                mondrian_platform::ProcessMemoryProbeResult::observed(
                    ProcessMemoryScope::CurrentProcess,
                    mondrian_platform::ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
                    1,
                    1,
                    64 * MIB as u64,
                    64 * MIB as u64,
                    64 * MIB as u64,
                )
            }
        }

        impl SystemMemoryProbe for CurrentOnlyMemoryProbe {
            fn current_system_memory(&self) -> mondrian_platform::SystemMemoryProbeResult {
                mondrian_platform::SystemMemoryProbeResult::observed(
                    mondrian_platform::SystemMemoryProbeBackend::WindowsGlobalMemoryStatus,
                    16 * 1024 * MIB as u64,
                    8 * 1024 * MIB as u64,
                    50,
                )
            }
        }

        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        coordinator.observe_pressure_from_probe_at(&CurrentOnlyMemoryProbe, Duration::ZERO);
        let decision = coordinator.decision();

        assert_eq!(decision.pressure, ExecutionResourcePressure::Elevated);
        let process_tree = &decision
            .memory_observation
            .as_ref()
            .expect("scoped observation retained")
            .product_process_tree;
        assert_eq!(process_tree.scope, ProcessMemoryScope::CurrentProcess);
        assert!(!process_tree.is_complete_for(ProcessMemoryScope::ProductProcessTree));
    }

    #[test]
    fn partial_supported_probe_failure_cannot_publish_nominal_pressure() {
        struct PartialMemoryProbe;

        impl ProcessMemoryProbe for PartialMemoryProbe {
            fn process_memory(
                &self,
                scope: ProcessMemoryScope,
            ) -> mondrian_platform::ProcessMemoryProbeResult {
                mondrian_platform::ProcessMemoryProbeResult::observed(
                    scope,
                    mondrian_platform::ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                    1,
                    1,
                    64 * MIB as u64,
                    64 * MIB as u64,
                    64 * MIB as u64,
                )
            }
        }

        impl SystemMemoryProbe for PartialMemoryProbe {
            fn current_system_memory(&self) -> mondrian_platform::SystemMemoryProbeResult {
                mondrian_platform::SystemMemoryProbeResult::failed(
                    mondrian_platform::SystemMemoryProbeBackend::WindowsGlobalMemoryStatus,
                    "system query failed",
                )
            }
        }

        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        coordinator.observe_pressure_from_probe_at(&PartialMemoryProbe, Duration::ZERO);
        let decision = coordinator.decision();
        assert_eq!(decision.pressure, ExecutionResourcePressure::Elevated);
        assert_eq!(
            decision.pressure_source,
            ExecutionResourcePressureSource::NativeMemory
        );
        assert_eq!(
            decision
                .memory_observation
                .as_ref()
                .and_then(|sample| sample.system.error.as_deref()),
            Some("system query failed")
        );
    }

    #[test]
    fn manual_pressure_is_not_attributed_to_the_previous_native_sample() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let probe = CountingMemoryProbe {
            calls: AtomicUsize::new(0),
            private_committed_bytes: GIB,
        };
        coordinator.observe_pressure_from_probe_at(&probe, Duration::ZERO);
        let native = coordinator.decision();
        assert_eq!(
            native.pressure_source,
            ExecutionResourcePressureSource::NativeMemory
        );

        let manual = coordinator.observe_pressure(ExecutionResourcePressure::Critical);
        assert_eq!(
            manual.pressure_source,
            ExecutionResourcePressureSource::Manual
        );
        assert_eq!(
            manual.memory_observation, native.memory_observation,
            "native evidence remains available but no longer owns pressure authority"
        );
    }

    #[test]
    fn unchanged_input_preserves_snapshot_revision() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let before = coordinator.decision();
        let after = coordinator.update_demand(ExecutionResourceDemandSnapshot::default());
        assert_eq!(before.revision, after.revision);
        assert_eq!(before.schema_version, EXECUTION_RESOURCE_DECISION_VERSION);
    }

    #[test]
    fn asset_mutation_is_explicit_heavy_demand_and_never_bypasses_realtime() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        let idle = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            media_asset_mutation: ExecutionDomainDemand {
                queued: 1,
                running: 0,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(idle.media_asset_mutation.dispatch_enabled);
        assert!(!idle.thumbnail.automatic_admission_enabled);

        let realtime = coordinator.update_demand(ExecutionResourceDemandSnapshot {
            preview_realtime: true,
            media_asset_mutation: ExecutionDomainDemand {
                queued: 1,
                running: 0,
                user_initiated: 1,
                ..ExecutionDomainDemand::default()
            },
            ..ExecutionResourceDemandSnapshot::default()
        });
        assert!(!realtime.media_asset_mutation.dispatch_enabled);
    }

    #[test]
    fn diagnostic_revision_saturates_instead_of_reusing_an_old_value() {
        let coordinator =
            ExecutionResourceCoordinator::new(profile(MachineResourceClass::Standard));
        {
            let mut state = coordinator.state.lock();
            let mut exhausted = (*state.decision).clone();
            exhausted.revision = u64::MAX;
            state.decision = Arc::new(exhausted);
        }

        let decision = coordinator.observe_pressure(ExecutionResourcePressure::Elevated);

        assert_eq!(decision.revision, u64::MAX);
        assert_eq!(decision.pressure, ExecutionResourcePressure::Elevated);
    }
}
