use mondrian_core::TimelineTime;
use std::sync::Arc;

/// One execution backend that can preserve an effect definition's semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectProcessingBackend {
    /// Scalar or SIMD CPU execution over the admitted working precision.
    Cpu,
    /// GPU execution over the admitted working precision.
    Gpu,
    /// A hosted external processor whose ABI owns the concrete execution.
    ExternalProcessor,
}

/// A compact set of processing backends.
///
/// The set is intentionally opaque: callers can query and combine supported
/// backends, but cannot construct contradictory boolean combinations.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectProcessingBackends(u8);

impl EffectProcessingBackends {
    const CPU_BIT: u8 = 1 << 0;
    const GPU_BIT: u8 = 1 << 1;
    const EXTERNAL_PROCESSOR_BIT: u8 = 1 << 2;

    /// No admitted execution backend.
    pub const NONE: Self = Self(0);
    /// CPU execution only.
    pub const CPU: Self = Self(Self::CPU_BIT);
    /// GPU execution only.
    pub const GPU: Self = Self(Self::GPU_BIT);
    /// Hosted external-processor execution only.
    pub const EXTERNAL_PROCESSOR: Self = Self(Self::EXTERNAL_PROCESSOR_BIT);
    /// Every currently modeled processing backend.
    pub const ALL: Self = Self(Self::CPU_BIT | Self::GPU_BIT | Self::EXTERNAL_PROCESSOR_BIT);

    /// Build a set containing exactly one backend.
    pub const fn only(backend: EffectProcessingBackend) -> Self {
        match backend {
            EffectProcessingBackend::Cpu => Self::CPU,
            EffectProcessingBackend::Gpu => Self::GPU,
            EffectProcessingBackend::ExternalProcessor => Self::EXTERNAL_PROCESSOR,
        }
    }

    /// Return the union of two backend sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Return the backends supported by both sets.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Whether this set contains a backend.
    pub const fn contains(self, backend: EffectProcessingBackend) -> bool {
        self.0 & Self::only(backend).0 != 0
    }

    /// Whether no backend is admitted.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every backend in this set is also present in `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }
}

impl std::fmt::Debug for EffectProcessingBackends {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut set = formatter.debug_set();
        if self.contains(EffectProcessingBackend::Cpu) {
            set.entry(&EffectProcessingBackend::Cpu);
        }
        if self.contains(EffectProcessingBackend::Gpu) {
            set.entry(&EffectProcessingBackend::Gpu);
        }
        if self.contains(EffectProcessingBackend::ExternalProcessor) {
            set.entry(&EffectProcessingBackend::ExternalProcessor);
        }
        set.finish()
    }
}

/// Whether equal inputs and explicit frame identity reproduce equal output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectDeterminism {
    /// Equal input pixels and parameters always reproduce equal output.
    Deterministic,
    /// Output additionally depends on the explicit frame seed.
    FrameSeeded,
    /// The definition cannot promise reproducible output.
    Nondeterministic,
}

/// Mutable execution state required by one effect occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectStateModel {
    /// Frames are independently evaluable.
    Stateless,
    /// Frames require one ordered continuity session.
    StatefulSequential,
}

/// Input history required on one side of the requested output instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectTemporalSpan {
    /// Only the output instant is required.
    None,
    /// A finite exact duration in the effect's evaluation time domain.
    Finite(TimelineTime),
    /// No finite bound can be promised.
    Unbounded,
}

impl EffectTemporalSpan {
    fn validate(self) -> Result<(), EffectExecutionContractError> {
        if matches!(self, Self::Finite(duration) if duration.is_negative()) {
            return Err(EffectExecutionContractError::NegativeTemporalExtent);
        }
        Ok(())
    }

    pub(crate) fn accumulate(self, other: Self) -> Result<Self, EffectExecutionContractError> {
        match (self, other) {
            (Self::Unbounded, _) | (_, Self::Unbounded) => Ok(Self::Unbounded),
            (Self::None, span) | (span, Self::None) => Ok(span),
            (Self::Finite(left), Self::Finite(right)) => left
                .checked_add(right)
                .map(Self::Finite)
                .map_err(|_| EffectExecutionContractError::TemporalExtentOverflow),
        }
    }

    pub(crate) fn covers(self, required: Self) -> bool {
        match (self, required) {
            (Self::Unbounded, _) => true,
            (_, Self::Unbounded) => false,
            (Self::None, Self::None) => true,
            (Self::Finite(_), Self::None) => true,
            (Self::None, Self::Finite(_)) => false,
            (Self::Finite(declared), Self::Finite(required)) => declared >= required,
        }
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unbounded, _) | (_, Self::Unbounded) => Self::Unbounded,
            (Self::None, span) | (span, Self::None) => span,
            (Self::Finite(left), Self::Finite(right)) => Self::Finite(left.max(right)),
        }
    }
}

/// Past and future input demand for one output instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectTemporalInputExtent {
    /// Required history before the output instant.
    pub past: EffectTemporalSpan,
    /// Required lookahead after the output instant.
    pub future: EffectTemporalSpan,
}

impl EffectTemporalInputExtent {
    /// Current-frame-only execution.
    pub const CURRENT_FRAME: Self = Self {
        past: EffectTemporalSpan::None,
        future: EffectTemporalSpan::None,
    };

    /// An intentionally conservative, unbounded temporal contract.
    pub const UNBOUNDED: Self = Self {
        past: EffectTemporalSpan::Unbounded,
        future: EffectTemporalSpan::Unbounded,
    };

    fn validate(self) -> Result<(), EffectExecutionContractError> {
        self.past.validate()?;
        self.future.validate()
    }

    pub(crate) fn accumulate(self, other: Self) -> Result<Self, EffectExecutionContractError> {
        Ok(Self {
            past: self.past.accumulate(other.past)?,
            future: self.future.accumulate(other.future)?,
        })
    }

    pub(crate) fn covers(self, required: Self) -> bool {
        self.past.covers(required.past) && self.future.covers(required.future)
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            past: self.past.merge(other.past),
            future: self.future.merge(other.future),
        }
    }
}

/// How an output region maps to required input pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectRoiPropagation {
    /// Every output pixel depends only on the corresponding input pixel.
    PixelLocal,
    /// The requested input region expands by a finite pixel radius.
    Expand {
        /// Horizontal radius on each side.
        horizontal_pixels: u32,
        /// Vertical radius on each side.
        vertical_pixels: u32,
    },
    /// Every output request requires the complete input frame.
    FullFrame,
    /// The definition has not supplied a trustworthy ROI law; callers must
    /// conservatively request a full frame and may not advertise ROI support.
    UnknownRequiresFullFrame,
}

impl EffectRoiPropagation {
    /// Whether this declaration is at least as conservative as one
    /// implementation requirement.
    ///
    /// `UnknownRequiresFullFrame` remains distinct from `FullFrame`: the
    /// former cannot promise a usable ROI law even though both force a
    /// full-frame request.
    pub const fn covers(self, required: Self) -> bool {
        match (self, required) {
            (Self::UnknownRequiresFullFrame, _) => true,
            (_, Self::UnknownRequiresFullFrame) => false,
            (Self::FullFrame, _) => true,
            (_, Self::FullFrame) => false,
            (
                Self::Expand {
                    horizontal_pixels: declared_horizontal,
                    vertical_pixels: declared_vertical,
                },
                Self::Expand {
                    horizontal_pixels: required_horizontal,
                    vertical_pixels: required_vertical,
                },
            ) => {
                declared_horizontal >= required_horizontal && declared_vertical >= required_vertical
            }
            (Self::Expand { .. }, Self::PixelLocal) => true,
            (Self::PixelLocal, Self::PixelLocal) => true,
            (Self::PixelLocal, Self::Expand { .. }) => false,
        }
    }

    fn accumulate(self, other: Self) -> Result<Self, EffectExecutionContractError> {
        match (self, other) {
            (Self::UnknownRequiresFullFrame, _) | (_, Self::UnknownRequiresFullFrame) => {
                Ok(Self::UnknownRequiresFullFrame)
            }
            (Self::FullFrame, _) | (_, Self::FullFrame) => Ok(Self::FullFrame),
            (Self::PixelLocal, roi) | (roi, Self::PixelLocal) => Ok(roi),
            (
                Self::Expand {
                    horizontal_pixels: left_horizontal,
                    vertical_pixels: left_vertical,
                },
                Self::Expand {
                    horizontal_pixels: right_horizontal,
                    vertical_pixels: right_vertical,
                },
            ) => Ok(Self::Expand {
                horizontal_pixels: left_horizontal
                    .checked_add(right_horizontal)
                    .ok_or(EffectExecutionContractError::RoiExtentOverflow)?,
                vertical_pixels: left_vertical
                    .checked_add(right_vertical)
                    .ok_or(EffectExecutionContractError::RoiExtentOverflow)?,
            }),
        }
    }
}

/// Exact sample representation at an effect processing seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectWorkingPrecision {
    /// Encoded eight-bit normalized samples.
    NormalizedU8,
    /// IEEE binary16 or an equivalent sixteen-bit float representation.
    Float16,
    /// IEEE binary32 representation.
    Float32,
}

/// One exact backend/representation execution mode.
///
/// Keeping the pair intact is essential: `{CPU, GPU} × {U8, F32}` would
/// otherwise falsely advertise GPU-U8 and any precision between two endpoints
/// when only CPU-U8, CPU-F32, and GPU-F32 executors exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectExecutionMode {
    backend: EffectProcessingBackend,
    precision: EffectWorkingPrecision,
}

impl EffectExecutionMode {
    /// Construct one exact execution mode.
    pub const fn new(backend: EffectProcessingBackend, precision: EffectWorkingPrecision) -> Self {
        Self { backend, precision }
    }

    /// Concrete processing backend.
    pub const fn backend(self) -> EffectProcessingBackend {
        self.backend
    }

    /// Exact sample representation.
    pub const fn precision(self) -> EffectWorkingPrecision {
        self.precision
    }
}

/// Compact set of exact backend/representation execution modes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectExecutionModes(u16);

impl EffectExecutionModes {
    const CPU_U8_BIT: u16 = 1 << 0;
    const CPU_F16_BIT: u16 = 1 << 1;
    const CPU_F32_BIT: u16 = 1 << 2;
    const GPU_U8_BIT: u16 = 1 << 3;
    const GPU_F16_BIT: u16 = 1 << 4;
    const GPU_F32_BIT: u16 = 1 << 5;
    const EXTERNAL_U8_BIT: u16 = 1 << 6;
    const EXTERNAL_F16_BIT: u16 = 1 << 7;
    const EXTERNAL_F32_BIT: u16 = 1 << 8;

    /// No executable mode.
    pub const NONE: Self = Self(0);
    /// CPU encoded RGBA8 execution.
    pub const CPU_U8: Self = Self(Self::CPU_U8_BIT);
    /// CPU Float16 execution.
    pub const CPU_F16: Self = Self(Self::CPU_F16_BIT);
    /// CPU Float32 execution.
    pub const CPU_F32: Self = Self(Self::CPU_F32_BIT);
    /// Every modeled CPU representation.
    pub const CPU_ALL: Self = Self(Self::CPU_U8_BIT | Self::CPU_F16_BIT | Self::CPU_F32_BIT);
    /// GPU encoded RGBA8 execution.
    pub const GPU_U8: Self = Self(Self::GPU_U8_BIT);
    /// GPU Float16 execution.
    pub const GPU_F16: Self = Self(Self::GPU_F16_BIT);
    /// GPU Float32 execution.
    pub const GPU_F32: Self = Self(Self::GPU_F32_BIT);
    /// Every modeled GPU representation.
    pub const GPU_ALL: Self = Self(Self::GPU_U8_BIT | Self::GPU_F16_BIT | Self::GPU_F32_BIT);
    /// External-processor encoded RGBA8 execution.
    pub const EXTERNAL_U8: Self = Self(Self::EXTERNAL_U8_BIT);
    /// External-processor Float16 execution.
    pub const EXTERNAL_F16: Self = Self(Self::EXTERNAL_F16_BIT);
    /// External-processor Float32 execution.
    pub const EXTERNAL_F32: Self = Self(Self::EXTERNAL_F32_BIT);
    /// Every modeled external-processor representation.
    pub const EXTERNAL_ALL: Self =
        Self(Self::EXTERNAL_U8_BIT | Self::EXTERNAL_F16_BIT | Self::EXTERNAL_F32_BIT);
    /// Every modeled backend/representation pair.
    pub const ALL: Self = Self(Self::CPU_ALL.0 | Self::GPU_ALL.0 | Self::EXTERNAL_ALL.0);

    /// Build a singleton exact mode.
    pub const fn only(backend: EffectProcessingBackend, precision: EffectWorkingPrecision) -> Self {
        match (backend, precision) {
            (EffectProcessingBackend::Cpu, EffectWorkingPrecision::NormalizedU8) => Self::CPU_U8,
            (EffectProcessingBackend::Cpu, EffectWorkingPrecision::Float16) => Self::CPU_F16,
            (EffectProcessingBackend::Cpu, EffectWorkingPrecision::Float32) => Self::CPU_F32,
            (EffectProcessingBackend::Gpu, EffectWorkingPrecision::NormalizedU8) => Self::GPU_U8,
            (EffectProcessingBackend::Gpu, EffectWorkingPrecision::Float16) => Self::GPU_F16,
            (EffectProcessingBackend::Gpu, EffectWorkingPrecision::Float32) => Self::GPU_F32,
            (EffectProcessingBackend::ExternalProcessor, EffectWorkingPrecision::NormalizedU8) => {
                Self::EXTERNAL_U8
            }
            (EffectProcessingBackend::ExternalProcessor, EffectWorkingPrecision::Float16) => {
                Self::EXTERNAL_F16
            }
            (EffectProcessingBackend::ExternalProcessor, EffectWorkingPrecision::Float32) => {
                Self::EXTERNAL_F32
            }
        }
    }

    /// Return the union of two exact mode sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Return modes shared by both sets.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Whether this set contains one exact mode.
    pub const fn contains(
        self,
        backend: EffectProcessingBackend,
        precision: EffectWorkingPrecision,
    ) -> bool {
        self.0 & Self::only(backend, precision).0 != 0
    }

    /// Whether no exact mode is admitted.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every mode in this set is also present in `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    /// Project the exact pairs to their backend identities.
    pub const fn processing_backends(self) -> EffectProcessingBackends {
        let mut backends = EffectProcessingBackends::NONE;
        if self.0 & Self::CPU_ALL.0 != 0 {
            backends = backends.union(EffectProcessingBackends::CPU);
        }
        if self.0 & Self::GPU_ALL.0 != 0 {
            backends = backends.union(EffectProcessingBackends::GPU);
        }
        if self.0 & Self::EXTERNAL_ALL.0 != 0 {
            backends = backends.union(EffectProcessingBackends::EXTERNAL_PROCESSOR);
        }
        backends
    }

    /// Project modes to one backend without inventing another backend pair.
    pub const fn for_backend(self, backend: EffectProcessingBackend) -> Self {
        match backend {
            EffectProcessingBackend::Cpu => Self(self.0 & Self::CPU_ALL.0),
            EffectProcessingBackend::Gpu => Self(self.0 & Self::GPU_ALL.0),
            EffectProcessingBackend::ExternalProcessor => Self(self.0 & Self::EXTERNAL_ALL.0),
        }
    }
}

impl std::fmt::Debug for EffectExecutionModes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut set = formatter.debug_set();
        for backend in [
            EffectProcessingBackend::Cpu,
            EffectProcessingBackend::Gpu,
            EffectProcessingBackend::ExternalProcessor,
        ] {
            for precision in [
                EffectWorkingPrecision::NormalizedU8,
                EffectWorkingPrecision::Float16,
                EffectWorkingPrecision::Float32,
            ] {
                if self.contains(backend, precision) {
                    set.entry(&EffectExecutionMode::new(backend, precision));
                }
            }
        }
        set.finish()
    }
}

/// Longest resource lifetime and ownership scope required by an occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectResourceLifetime {
    /// All resources are frame-local and require no preparation.
    Frame,
    /// Immutable resources must be resolved once and retained by the prepared
    /// program.
    PreparedProgram,
    /// Mutable resources belong to one ordered continuity session.
    ContinuitySession,
}

/// Maximum topology an evaluator may append for one effect instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectGraphTopology {
    /// Zero or more unary operations extending the current output.
    LinearChain,
    /// A validated directed acyclic graph.
    GeneralDag,
}

/// Complete execution contract owned by one effect definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectExecutionContract {
    /// Exact backend/representation pairs that preserve this definition's
    /// semantics end to end.
    pub execution_modes: EffectExecutionModes,
    /// Reproducibility contract.
    pub determinism: EffectDeterminism,
    /// Mutable state contract.
    pub state_model: EffectStateModel,
    /// Required temporal input.
    pub temporal_input: EffectTemporalInputExtent,
    /// Required spatial input.
    pub roi_propagation: EffectRoiPropagation,
    /// Resource/session lifetime.
    pub resource_lifetime: EffectResourceLifetime,
    /// Maximum graph topology emitted by the evaluator.
    pub topology: EffectGraphTopology,
}

impl EffectExecutionContract {
    /// Contract for a source-only identity program.
    pub const IDENTITY: Self = Self {
        execution_modes: EffectExecutionModes::ALL,
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::LinearChain,
    };

    /// Conservative plugin default.
    ///
    /// It deliberately admits no backend and assumes unbounded, stateful,
    /// full-frame execution. A plugin must replace this complete contract before
    /// a prepared program can execute it.
    pub const CONSERVATIVE_PLUGIN_DEFAULT: Self = Self {
        execution_modes: EffectExecutionModes::NONE,
        determinism: EffectDeterminism::Nondeterministic,
        state_model: EffectStateModel::StatefulSequential,
        temporal_input: EffectTemporalInputExtent::UNBOUNDED,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        resource_lifetime: EffectResourceLifetime::ContinuitySession,
        topology: EffectGraphTopology::GeneralDag,
    };

    /// Validate one definition-owned contract.
    pub fn validate(self) -> Result<(), EffectExecutionContractError> {
        self.validate_composable_obligations()
    }

    fn validate_composable_obligations(self) -> Result<(), EffectExecutionContractError> {
        self.temporal_input.validate()?;
        if self.state_model == EffectStateModel::StatefulSequential
            && self.resource_lifetime != EffectResourceLifetime::ContinuitySession
        {
            return Err(EffectExecutionContractError::StateWithoutContinuitySession);
        }
        Ok(())
    }

    /// Compose two sequential effect contracts.
    pub fn compose(self, next: Self) -> Result<Self, EffectExecutionContractError> {
        self.validate_composable_obligations()?;
        next.validate()?;
        Ok(Self {
            execution_modes: self.execution_modes.intersection(next.execution_modes),
            determinism: self.determinism.max(next.determinism),
            state_model: self.state_model.max(next.state_model),
            temporal_input: self.temporal_input.accumulate(next.temporal_input)?,
            roi_propagation: self.roi_propagation.accumulate(next.roi_propagation)?,
            resource_lifetime: self.resource_lifetime.max(next.resource_lifetime),
            topology: self.topology.max(next.topology),
        })
    }
}

impl Default for EffectExecutionContract {
    fn default() -> Self {
        Self::CONSERVATIVE_PLUGIN_DEFAULT
    }
}

/// Stack-level execution evidence retaining every stage contract.
///
/// `aggregate.execution_modes` is the homogeneous exact-mode intersection and
/// may be empty for a valid heterogeneous chain. A future execution planner
/// may then insert explicit representation/backend transfers, or fail closed
/// when the active renderer cannot realize them; preparation never
/// misclassifies the authored stack itself as invalid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectExecutionEnvelope {
    aggregate: EffectExecutionContract,
    stages: Arc<[EffectExecutionContract]>,
}

impl EffectExecutionEnvelope {
    /// Build an envelope from compiler-validated aggregate and stage evidence.
    ///
    /// This constructor stays crate-private so external callers cannot forge an
    /// aggregate that understates the retained stage obligations.
    pub(crate) fn new(
        aggregate: EffectExecutionContract,
        stages: impl Into<Arc<[EffectExecutionContract]>>,
    ) -> Self {
        Self { aggregate, stages: stages.into() }
    }

    /// Identity envelope with no effect stages.
    pub fn identity() -> Self {
        Self::new(EffectExecutionContract::IDENTITY, Arc::from([]))
    }

    /// Aggregate temporal, ROI, exact-mode, lifetime, and topology evidence.
    pub const fn aggregate(&self) -> EffectExecutionContract {
        self.aggregate
    }

    /// Ordered per-stage contracts required for heterogeneous lowering.
    pub fn stages(&self) -> &[EffectExecutionContract] {
        &self.stages
    }

    /// Exact backend/representation pairs capable of executing the complete
    /// stack without a transfer.
    pub const fn homogeneous_execution_modes(&self) -> EffectExecutionModes {
        self.aggregate.execution_modes
    }

    /// Backends with at least one homogeneous exact representation.
    pub const fn homogeneous_processing_backends(&self) -> EffectProcessingBackends {
        self.aggregate.execution_modes.processing_backends()
    }

    /// Whether the stack is valid but requires a backend and/or representation
    /// transition between definition stages.
    pub fn requires_execution_transitions(&self) -> bool {
        !self.stages.is_empty() && self.homogeneous_execution_modes().is_empty()
    }
}

/// Why a complete Effect program cannot enter one single-frame executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectExecutionAdmissionError {
    /// One retained stage does not admit the executor's exact mode.
    #[error(
        "effect stage {stage_index} does not admit execution mode ({backend:?}, {precision:?}); admitted modes are {admitted:?}"
    )]
    ExecutionModeNotAdmitted {
        /// First incompatible stage.
        stage_index: usize,
        /// Requested backend.
        backend: EffectProcessingBackend,
        /// Requested exact representation.
        precision: EffectWorkingPrecision,
        /// Exact modes declared by the stage.
        admitted: EffectExecutionModes,
    },
    /// Ordered mutable state or continuity-owned resources need a session.
    #[error("effect program requires an ordered continuity session and resource owner")]
    ContinuitySessionRequired,
    /// The executor was given only the current frame.
    #[error("effect program requires temporal input {extent:?}")]
    TemporalInputRequired {
        /// Past/future demand declared by the complete stack.
        extent: EffectTemporalInputExtent,
    },
}

/// Invalid or unrepresentable effect execution contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectExecutionContractError {
    /// A temporal extent must be a non-negative duration.
    #[error("temporal input extent cannot be negative")]
    NegativeTemporalExtent,
    /// Sequential temporal extents overflowed exact author time.
    #[error("composed temporal input extent overflowed")]
    TemporalExtentOverflow,
    /// Sequential ROI expansions exceeded the representable pixel range.
    #[error("composed ROI expansion overflowed")]
    RoiExtentOverflow,
    /// Stateful effects require continuity-session resource ownership.
    #[error("stateful effect execution requires continuity-session resources")]
    StateWithoutContinuitySession,
}

/// A definition promised execution semantics its emitted implementation cannot
/// preserve.
///
/// Contract declarations may be more conservative than an implementation, but
/// never more optimistic. Preparation validates this boundary before a graph
/// can enter Preview, Export, or a compiled-graph cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectExecutionContractViolation {
    /// The declaration admitted a backend/representation pair absent from the
    /// emitted graph.
    #[error(
        "declared execution modes {declared:?} are not a subset of implemented modes {implemented:?}"
    )]
    ExecutionModesTooOptimistic {
        /// Definition-owned exact modes.
        declared: EffectExecutionModes,
        /// Exact modes implemented by the emitted graph.
        implemented: EffectExecutionModes,
    },
    /// The declaration promised stronger reproducibility than the graph.
    #[error(
        "declared determinism {declared:?} is stronger than implementation requirement {required:?}"
    )]
    DeterminismTooOptimistic {
        /// Definition-owned determinism.
        declared: EffectDeterminism,
        /// Minimum conservatism required by the graph.
        required: EffectDeterminism,
    },
    /// The declaration promised a smaller spatial input region than required.
    #[error(
        "declared ROI law {declared:?} does not cover implementation requirement {required:?}"
    )]
    RoiTooOptimistic {
        /// Definition-owned ROI law.
        declared: EffectRoiPropagation,
        /// Spatial demand derived from the graph.
        required: EffectRoiPropagation,
    },
    /// The declared temporal window omits input frames required by the emitted
    /// operation.
    #[error(
        "effect execution contract declares temporal input {declared:?}, but emitted operations require {required:?}"
    )]
    TemporalExtentTooOptimistic {
        /// Definition-owned declaration.
        declared: EffectTemporalInputExtent,
        /// Requirements derived from emitted operations.
        required: EffectTemporalInputExtent,
    },
    /// The implementation retains resources longer than declared.
    #[error(
        "declared resource lifetime {declared:?} is shorter than implementation requirement {required:?}"
    )]
    ResourceLifetimeTooShort {
        /// Definition-owned lifetime.
        declared: EffectResourceLifetime,
        /// Minimum lifetime required by emitted operations or dependencies.
        required: EffectResourceLifetime,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_contracts_intersect_exact_modes_and_accumulate_roi() {
        let first = EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: 4,
                vertical_pixels: 2,
            },
            ..EffectExecutionContract::IDENTITY
        };
        let second = EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::FrameSeeded,
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: 3,
                vertical_pixels: 5,
            },
            ..EffectExecutionContract::IDENTITY
        };

        let composed = first.compose(second).expect("valid composition");
        assert_eq!(composed.execution_modes, EffectExecutionModes::CPU_F32);
        assert_eq!(composed.determinism, EffectDeterminism::FrameSeeded);
        assert_eq!(
            composed.roi_propagation,
            EffectRoiPropagation::Expand { horizontal_pixels: 7, vertical_pixels: 7 }
        );
    }

    #[test]
    fn single_frame_admission_rejects_backend_precision_and_temporal_mismatch() {
        let cpu_float = EffectExecutionEnvelope::new(
            EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                ..EffectExecutionContract::IDENTITY
            },
            Arc::from([EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                ..EffectExecutionContract::IDENTITY
            }]),
        );
        assert_eq!(
            cpu_float.admit_single_frame_backend(
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecision::NormalizedU8,
            ),
            Err(EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                stage_index: 0,
                backend: EffectProcessingBackend::Cpu,
                precision: EffectWorkingPrecision::NormalizedU8,
                admitted: EffectExecutionModes::CPU_F32,
            })
        );
        assert_eq!(
            cpu_float.admit_single_frame_backend(
                EffectProcessingBackend::Gpu,
                EffectWorkingPrecision::Float32,
            ),
            Err(EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                stage_index: 0,
                backend: EffectProcessingBackend::Gpu,
                precision: EffectWorkingPrecision::Float32,
                admitted: EffectExecutionModes::CPU_F32,
            })
        );

        let temporal_extent = EffectTemporalInputExtent {
            past: EffectTemporalSpan::Finite(
                TimelineTime::new(1, 25).expect("positive temporal extent"),
            ),
            future: EffectTemporalSpan::None,
        };
        let temporal = EffectExecutionEnvelope::new(
            EffectExecutionContract {
                temporal_input: temporal_extent,
                ..EffectExecutionContract::IDENTITY
            },
            Arc::from([EffectExecutionContract {
                temporal_input: temporal_extent,
                ..EffectExecutionContract::IDENTITY
            }]),
        );
        assert_eq!(
            temporal.admit_single_frame_backend(
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecision::Float32,
            ),
            Err(EffectExecutionAdmissionError::TemporalInputRequired { extent: temporal_extent })
        );

        let continuity_resource = EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            resource_lifetime: EffectResourceLifetime::ContinuitySession,
            ..EffectExecutionContract::IDENTITY
        };
        let continuity_resource =
            EffectExecutionEnvelope::new(continuity_resource, Arc::from([continuity_resource]));
        assert_eq!(
            continuity_resource.admit_single_frame_backend(
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecision::Float32,
            ),
            Err(EffectExecutionAdmissionError::ContinuitySessionRequired)
        );
    }
}
