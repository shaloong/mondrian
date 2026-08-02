//! 效果节点抽象

use crate::execution::CustomEffectRenderProcessor;
use crate::graph::{CompiledEffectGraph, EffectGraphBuilderState, EffectRenderGraph};
use crate::lut::{Lut3D, LutPreparationCache};
use crate::mask::MaskComponent;
use crate::plugin_contract::{
    effect_plugin_is_library_visible, record_plugin_runtime_failure, EffectPluginContract,
};
use crate::{
    EffectExecutionContract, EffectExecutionContractViolation, EffectGraphTopology,
    EffectResourceLifetime, EffectRoiPropagation,
};
use mondrian_core::{
    automation::{
        AnimatablePropertyUiMetadata, ParameterCacheImpact, ParameterEnumOption,
        ParameterInvalidValuePolicy, ParameterNumericContract, ParameterNumericRange,
        ParameterResourceReference, ParameterUnit, PropertyBag, PropertyDescriptor, PropertyValue,
    },
    types::{Color, ColorSpace, EffectId, WorkingColorSpace},
    ParameterId, TimelineTime,
};
// Re-export effect data types from mondrian-core.
pub use mondrian_core::effect_data::{namespaced_effect_path, EffectNode, EffectType};

pub(crate) const GAUSSIAN_BLUR_AUTHOR_MAX_RADIUS_PIXELS: u32 = 200;
pub(crate) const SHARPEN_BLUR_RADIUS_PIXELS: f32 = 1.0;
const OPAQUE_EFFECT_EVALUATOR_BASE_CHARGE_BYTES: usize = 512;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock, RwLock,
    },
};

#[derive(Debug, Clone, Copy)]
pub struct EffectEvalContext {
    pub time: TimelineTime,
    /// Sequence working identity used by scene-linear effect algorithms.
    pub working_color_space: WorkingColorSpace,
}

/// Recovery owner for one unresolved effect resource.
///
/// The distinction prevents production callers from polling immutable author
/// errors as though the outside world could repair them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectResourceRecovery {
    /// The authored effect must change before preparation can succeed.
    AuthorEdit,
    /// The authored intent is valid and an external change may make the same
    /// immutable state executable.
    ExternalChange,
}

/// Failure to turn an enabled authored effect into an executable render graph.
///
/// Enabled effects never silently degrade to identity. Callers may present the
/// error, disable or repair the effect explicitly, or abort export.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectGraphBuildError {
    /// The project references an effect key that has no registered definition.
    #[error("effect `{effect_key}` ({effect_id}) has no registered definition")]
    DefinitionUnavailable {
        effect_key: String,
        effect_id: EffectId,
    },
    /// A definition exists for authoring/serialization but has no evaluator.
    #[error("effect `{effect_key}` ({effect_id}) has no executable render graph")]
    EvaluationUnsupported {
        effect_key: String,
        effect_id: EffectId,
    },
    /// The registered plugin cannot execute in the current process.
    #[error("effect runtime `{effect_key}` ({effect_id}) is unavailable")]
    RuntimeUnavailable {
        effect_key: String,
        effect_id: EffectId,
    },
    /// A required resource is unbound, unreadable, or invalid.
    #[error(
        "effect `{effect_key}` ({effect_id}) cannot resolve resource `{parameter_id}`: {reason}"
    )]
    ResourceUnavailable {
        effect_key: String,
        effect_id: EffectId,
        parameter_id: ParameterId,
        recovery: EffectResourceRecovery,
        reason: String,
    },
    /// The definition has not admitted any processing backend.
    #[error("effect `{effect_key}` ({effect_id}) has no admitted execution backend")]
    ExecutionContractUnavailable {
        effect_key: String,
        effect_id: EffectId,
    },
    /// A prepared-program resource contract was paired with a frame evaluator.
    #[error("effect `{effect_key}` ({effect_id}) requires a prepared resource evaluator")]
    ResourcePreparationUnsupported {
        effect_key: String,
        effect_id: EffectId,
    },
    /// Authored parameter schemas do not exactly match the bound definition.
    #[error("effect `{effect_key}` ({effect_id}) has invalid author state: {reason}")]
    InvalidAuthorState {
        effect_key: String,
        effect_id: EffectId,
        reason: String,
    },
    /// A frame evaluator emitted topology beyond its declared contract.
    #[error("effect `{effect_key}` ({effect_id}) violated its graph topology contract")]
    TopologyContractViolation {
        effect_key: String,
        effect_id: EffectId,
    },
    /// The emitted graph or prepared dependency is more demanding than the
    /// definition-owned execution contract.
    #[error("effect `{effect_key}` ({effect_id}) violated its execution contract: {violation}")]
    ExecutionContractViolation {
        effect_key: String,
        effect_id: EffectId,
        violation: Box<EffectExecutionContractViolation>,
    },
    /// Sequential temporal, ROI, state, or lifetime contracts cannot be
    /// represented. A heterogeneous backend chain is valid and is retained in
    /// its ordered execution envelope.
    #[error("effect stack execution contract is invalid: {reason}")]
    InvalidExecutionContract { reason: String },
    /// Definitions changed while one immutable stack was being bound.
    #[error("effect definition registry changed during preparation ({before} -> {after})")]
    DefinitionRegistryChanged {
        /// Revision sampled before definition/resource binding.
        before: u64,
        /// Revision sampled after initial graph validation.
        after: u64,
    },
    /// Third-party evaluator code panicked while producing its graph.
    #[error("effect graph builder `{effect_key}` ({effect_id}) panicked")]
    BuilderPanicked {
        effect_key: String,
        effect_id: EffectId,
    },
    /// The produced graph violated graph or color-domain invariants.
    #[error("compiled effect graph is invalid")]
    InvalidGraph,
}

impl EffectGraphBuildError {
    /// Report whether unchanged author state may recover after an external
    /// dependency changes.
    ///
    /// This is intended for a low-frequency dependency observer, never for a
    /// per-frame retry loop.
    #[must_use]
    pub const fn dependency_refresh_retryable(&self) -> bool {
        matches!(
            self,
            Self::ResourceUnavailable {
                recovery: EffectResourceRecovery::ExternalChange,
                ..
            }
        )
    }
}

/// Fallible graph builder shared by built-in and plugin effect definitions.
pub type EffectGraphBuilder = Arc<
    dyn Fn(
            &EffectNode,
            EffectEvalContext,
            &mut EffectGraphBuilderState,
        ) -> Result<(), EffectGraphBuildError>
        + Send
        + Sync,
>;
/// Lower a custom effect instance into backend parameters.
///
/// `Ok(None)` is an intentional identity result, such as a zero-strength
/// processor. Missing resources or invalid state must return `Err`.
pub type EffectRenderParamsBuilder = Arc<
    dyn Fn(
            &EffectNode,
            EffectEvalContext,
        ) -> Result<Option<serde_json::Value>, EffectGraphBuildError>
        + Send
        + Sync,
>;
pub type EffectCacheKeyBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext) -> Option<String> + Send + Sync>;

/// One immutable dependency retained by a prepared effect evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectResourceDependency {
    /// A parsed `.cube` file identified by its complete semantic payload.
    CubeLut {
        /// Author-resolved source path.
        path: PathBuf,
        /// Stable fingerprint of the complete parsed LUT semantics.
        semantic_fingerprint: [u8; 32],
    },
    /// Plugin-owned immutable resource identity.
    ///
    /// The plugin must re-register its definition when this identity changes.
    /// Registration advances the global definition revision and invalidates
    /// prepared programs conservatively.
    PluginManaged {
        /// Stable plugin-defined resource key.
        identity: String,
    },
}

/// Evaluator and immutable dependencies produced once during preparation.
#[derive(Clone)]
pub struct PreparedEffectEvaluator {
    evaluator: EffectGraphBuilder,
    dependencies: Vec<EffectResourceDependency>,
    retained_resource_bytes: usize,
}

impl PreparedEffectEvaluator {
    /// Bind a frame evaluator without immutable dependencies.
    pub fn new(evaluator: EffectGraphBuilder) -> Self {
        Self {
            evaluator,
            dependencies: Vec::new(),
            retained_resource_bytes: 0,
        }
    }

    /// Retain one dependency in the prepared program identity.
    pub fn with_dependency(mut self, dependency: EffectResourceDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }

    /// Add conservative logical bytes for immutable resources captured by the
    /// evaluator.
    ///
    /// This charge is used by Prepared Program cache admission. It is not
    /// allocator or process-resident-memory evidence.
    pub fn with_retained_resource_bytes(mut self, retained_bytes: usize) -> Self {
        self.retained_resource_bytes = self.retained_resource_bytes.saturating_add(retained_bytes);
        self
    }

    pub(crate) fn evaluator(&self) -> &EffectGraphBuilder {
        &self.evaluator
    }

    pub(crate) fn dependencies(&self) -> &[EffectResourceDependency] {
        &self.dependencies
    }

    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(OPAQUE_EFFECT_EVALUATOR_BASE_CHARGE_BYTES)
            .saturating_add(
                self.dependencies
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectResourceDependency>()),
            )
            .saturating_add(
                self.dependencies
                    .iter()
                    .map(effect_resource_dependency_retained_bytes)
                    .fold(0_usize, usize::saturating_add),
            )
            .saturating_add(self.retained_resource_bytes)
    }
}

fn effect_resource_dependency_retained_bytes(dependency: &EffectResourceDependency) -> usize {
    match dependency {
        EffectResourceDependency::CubeLut { path, .. } => path.as_os_str().len(),
        EffectResourceDependency::PluginManaged { identity } => identity.capacity(),
    }
}

/// Owner-scoped immutable resource preparation seam.
///
/// The context may be used only while building a `PreparedEffectEvaluator`.
/// Evaluators retain the returned immutable resource `Arc`, never this
/// borrowed context or its cache owner.
#[derive(Clone, Copy)]
pub struct EffectPreparationContext<'a> {
    lut_cache: &'a LutPreparationCache,
}

impl<'a> EffectPreparationContext<'a> {
    pub(crate) const fn new(lut_cache: &'a LutPreparationCache) -> Self {
        Self { lut_cache }
    }

    /// Prepare one external `.cube` LUT through the caller-owned bounded cache.
    pub fn prepare_cube_lut(
        self,
        path: &std::path::Path,
    ) -> mondrian_core::Result<Arc<PreparedLut3D>> {
        self.lut_cache.load_cube(path)
    }
}

/// Prepare immutable resources and bind one effect evaluator.
pub type EffectGraphPreparer = Arc<
    dyn for<'a> Fn(
            &EffectNode,
            WorkingColorSpace,
            EffectPreparationContext<'a>,
        ) -> Result<PreparedEffectEvaluator, EffectGraphBuildError>
        + Send
        + Sync,
>;

/// Cross-call reuse contract for one effect operation or compiled subtree.
///
/// This is execution evidence, not merely a cache hint: callers must never
/// construct a reusable key for [`Self::Uncacheable`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectCachePolicy {
    /// Equal input pixels and parameters reproduce equal output.
    #[default]
    Deterministic,
    /// Output additionally depends on the explicit frame seed.
    FrameDependent,
    /// The operation cannot promise reproducible output across invocations.
    Uncacheable,
}

impl EffectCachePolicy {
    pub(crate) const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Uncacheable, _) | (_, Self::Uncacheable) => Self::Uncacheable,
            (Self::FrameDependent, _) | (_, Self::FrameDependent) => Self::FrameDependent,
            (Self::Deterministic, Self::Deterministic) => Self::Deterministic,
        }
    }

    /// Whether a caller may create a key that can be reused by a later
    /// invocation.
    pub const fn permits_cross_call_reuse(self) -> bool {
        !matches!(self, Self::Uncacheable)
    }

    /// Whether a reusable key must include the explicit frame seed.
    pub const fn requires_frame_seed(self) -> bool {
        matches!(self, Self::FrameDependent)
    }
}

/// Color domain in which an effect consumes or produces pixel values.
///
/// RGB domains are explicit so the render planner can insert stock-OCIO
/// processors instead of silently evaluating display- or log-referred math in
/// the scene-linear working space. Data and alpha/mask payloads are deliberately
/// non-convertible color domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum EffectColorDomain {
    /// The sequence's scene-linear RGB working space.
    SceneLinearRgb,
    /// A named log or perceptual RGB encoding resolved through OCIO.
    LogPerceptualRgb {
        /// Exact OCIO-backed color-space identity used by the effect.
        color_space: ColorSpace,
    },
    /// Linear-light RGB in an explicit display-primary space.
    DisplayLinearRgb {
        /// Exact linear display-primary color-space identity.
        color_space: ColorSpace,
    },
    /// Display-encoded RGB in an explicit output color space.
    DisplayEncodedRgb {
        /// Exact encoded display/output color-space identity.
        color_space: ColorSpace,
    },
    /// Non-color data such as depth, normals, or motion vectors.
    Data,
    /// A scalar alpha or mask payload.
    AlphaMask,
}

impl EffectColorDomain {
    /// Whether this domain represents color-managed RGB values.
    pub const fn is_rgb(self) -> bool {
        matches!(
            self,
            Self::SceneLinearRgb
                | Self::LogPerceptualRgb { .. }
                | Self::DisplayLinearRgb { .. }
                | Self::DisplayEncodedRgb { .. }
        )
    }
}

/// Input and output color-domain contract for one effect operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectColorDomainContract {
    /// Domain required at the operation input.
    pub input: EffectColorDomain,
    /// Domain produced by the operation output.
    pub output: EffectColorDomain,
}

impl EffectColorDomainContract {
    /// Declare an effect that preserves one processing domain.
    pub const fn preserving(domain: EffectColorDomain) -> Self {
        Self { input: domain, output: domain }
    }

    /// Scene-linear preserving contract used by built-in working-domain ops.
    pub const SCENE_LINEAR: Self = Self::preserving(EffectColorDomain::SceneLinearRgb);
}

impl Default for EffectColorDomainContract {
    fn default() -> Self {
        Self::SCENE_LINEAR
    }
}

/// One immutable, prepared LUT payload shared by every evaluated frame graph.
///
/// The semantic fingerprint is computed once when the resource enters a
/// prepared program. Graph signatures therefore never clone or hash the full
/// cube table on the frame path.
#[derive(Debug, Clone)]
pub struct PreparedLut3D {
    lut: Arc<Lut3D>,
    semantic_fingerprint: [u8; 32],
}

impl PreparedLut3D {
    /// Prepare one parsed LUT for shared frame execution.
    pub fn new(lut: Lut3D) -> Self {
        let semantic_fingerprint = lut_semantic_fingerprint(&lut);
        Self { lut: Arc::new(lut), semantic_fingerprint }
    }

    /// Borrow the parsed immutable LUT payload.
    pub fn lut(&self) -> &Lut3D {
        &self.lut
    }

    /// Stable digest of the complete parsed LUT semantics.
    pub const fn semantic_fingerprint(&self) -> &[u8; 32] {
        &self.semantic_fingerprint
    }

    /// Conservative logical bytes retained by this prepared payload.
    ///
    /// This includes the complete allocated cube table and name storage plus
    /// fixed Rust values. It is a scheduling/cache charge, not allocator or
    /// process-resident-memory evidence.
    pub fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(std::mem::size_of::<Lut3D>())
            .saturating_add(self.lut.name.capacity())
            .saturating_add(
                self.lut.data.capacity().saturating_mul(std::mem::size_of::<[f32; 3]>()),
            )
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(2))
    }
}

impl From<Lut3D> for PreparedLut3D {
    fn from(lut: Lut3D) -> Self {
        Self::new(lut)
    }
}

impl Serialize for PreparedLut3D {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.lut.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PreparedLut3D {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Lut3D::deserialize(deserializer).map(Self::new)
    }
}

impl std::ops::Deref for PreparedLut3D {
    type Target = Lut3D;

    fn deref(&self) -> &Self::Target {
        self.lut()
    }
}

fn lut_semantic_fingerprint(lut: &Lut3D) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.prepared-lut-3d.v1");
    hasher.update((lut.name.len() as u64).to_le_bytes());
    hasher.update(lut.name.as_bytes());
    hasher.update(lut.size.to_le_bytes());
    for value in lut.domain_min.into_iter().chain(lut.domain_max) {
        hasher.update(value.to_bits().to_le_bytes());
    }
    hasher.update((lut.data.len() as u64).to_le_bytes());
    for rgb in &lut.data {
        for value in rgb {
            hasher.update(value.to_bits().to_le_bytes());
        }
    }
    hasher.finalize().into()
}

#[derive(Clone)]
pub enum EffectRenderOp {
    ColorAdjust {
        exposure: f32,
        contrast: f32,
        saturation: f32,
        working_color_space: WorkingColorSpace,
    },
    GaussianBlur {
        radius: f32,
    },
    Sharpen {
        amount: f32,
    },
    Vignette {
        intensity: f32,
        feather: f32,
    },
    ChromaticAberration {
        amount: f32,
    },
    Grain {
        amount: f32,
    },
    /// Deterministic finite-history proof processor.
    ///
    /// The output coverage-correctly mixes the current upstream frame and the
    /// upstream frame at `time - past_offset`: straight-alpha inputs are
    /// premultiplied for interpolation, then returned as straight alpha. At the
    /// non-negative effect-domain boundary, the past sample holds exact time
    /// zero. A temporal executor must provide both frames; single-frame
    /// executors fail admission before reaching this operation.
    TemporalFrameMix {
        /// Exact non-negative history offset in the Clip visual author domain.
        past_offset: TimelineTime,
        /// Weight of the historical frame in `[0, 1]`.
        mix: f32,
    },
    Lut3D {
        lut: Arc<PreparedLut3D>,
        intensity: f32,
    },
    Custom {
        key: String,
        params: serde_json::Value,
        cache_key: Option<String>,
        cache_policy: EffectCachePolicy,
        /// Immutable implementation bound during Definition preparation.
        ///
        /// Definition/Builder evaluation is the only supported binding path.
        /// Raw graph utilities may leave this empty only to exercise rejection:
        /// compilation fails closed when a reachable Custom node is unbound.
        processor: Option<CustomEffectProcessorBinding>,
    },
}

impl std::fmt::Debug for EffectRenderOp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ColorAdjust {
                exposure,
                contrast,
                saturation,
                working_color_space,
            } => formatter
                .debug_struct("ColorAdjust")
                .field("exposure", exposure)
                .field("contrast", contrast)
                .field("saturation", saturation)
                .field("working_color_space", working_color_space)
                .finish(),
            Self::GaussianBlur { radius } => {
                formatter.debug_struct("GaussianBlur").field("radius", radius).finish()
            }
            Self::Sharpen { amount } => {
                formatter.debug_struct("Sharpen").field("amount", amount).finish()
            }
            Self::Vignette { intensity, feather } => formatter
                .debug_struct("Vignette")
                .field("intensity", intensity)
                .field("feather", feather)
                .finish(),
            Self::ChromaticAberration { amount } => {
                formatter.debug_struct("ChromaticAberration").field("amount", amount).finish()
            }
            Self::Grain { amount } => {
                formatter.debug_struct("Grain").field("amount", amount).finish()
            }
            Self::TemporalFrameMix { past_offset, mix } => formatter
                .debug_struct("TemporalFrameMix")
                .field("past_offset", past_offset)
                .field("mix", mix)
                .finish(),
            Self::Lut3D { lut, intensity } => formatter
                .debug_struct("Lut3D")
                .field("semantic_fingerprint", lut.semantic_fingerprint())
                .field("intensity", intensity)
                .finish(),
            Self::Custom { key, params, cache_key, cache_policy, processor } => formatter
                .debug_struct("Custom")
                .field("key", key)
                .field("params", params)
                .field("cache_key", cache_key)
                .field("cache_policy", cache_policy)
                .field(
                    "processor_revision",
                    &processor.as_ref().map(CustomEffectProcessorBinding::revision),
                )
                .finish(),
        }
    }
}

/// Immutable in-process processor implementation retained by executable IR.
#[derive(Clone)]
pub struct CustomEffectProcessorBinding {
    revision: u64,
    processor: CustomEffectRenderProcessor,
    runtime_owner: Option<CustomEffectRuntimeOwner>,
}

#[derive(Clone)]
struct CustomEffectRuntimeOwner {
    effect_key: String,
    definition_registry_revision: u64,
    contract: EffectPluginContract,
}

impl CustomEffectProcessorBinding {
    pub(crate) fn new(processor: CustomEffectRenderProcessor) -> Self {
        static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
        Self {
            revision: NEXT_REVISION.fetch_add(1, Ordering::AcqRel),
            processor,
            runtime_owner: None,
        }
    }

    /// Process-local implementation revision used by graph/cache identity.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn processor(&self) -> &CustomEffectRenderProcessor {
        &self.processor
    }

    pub(crate) fn with_runtime_owner(
        &self,
        effect_key: &str,
        definition_registry_revision: u64,
        contract: Option<&EffectPluginContract>,
    ) -> Self {
        Self {
            revision: self.revision,
            processor: Arc::clone(&self.processor),
            runtime_owner: contract.cloned().map(|contract| CustomEffectRuntimeOwner {
                effect_key: effect_key.to_owned(),
                definition_registry_revision,
                contract,
            }),
        }
    }

    pub(crate) fn record_runtime_failure(&self, reason: impl Into<String>) {
        let Some(owner) = &self.runtime_owner else {
            return;
        };
        record_plugin_runtime_failure(
            owner.effect_key.as_str(),
            owner.definition_registry_revision,
            Some(&owner.contract),
            reason,
        );
    }

    const fn runtime_owner_revision(&self) -> Option<u64> {
        match &self.runtime_owner {
            Some(owner) => Some(owner.definition_registry_revision),
            None => None,
        }
    }
}

impl std::fmt::Debug for CustomEffectProcessorBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CustomEffectProcessorBinding")
            .field("revision", &self.revision)
            .field("runtime_owner_revision", &self.runtime_owner_revision())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct EffectRenderPlan {
    pub ops: Vec<EffectRenderOp>,
}

impl EffectRenderPlan {
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn signature_hash(&self) -> u64 {
        use std::hash::Hasher;

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for op in &self.ops {
            op.hash_signature(&mut hasher);
        }
        hasher.finish()
    }
}

impl EffectRenderOp {
    pub fn hash_signature<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;

        match self {
            EffectRenderOp::ColorAdjust {
                exposure,
                contrast,
                saturation,
                working_color_space,
            } => {
                0u8.hash(state);
                exposure.to_bits().hash(state);
                contrast.to_bits().hash(state);
                saturation.to_bits().hash(state);
                working_color_space.hash(state);
            }
            EffectRenderOp::GaussianBlur { radius } => {
                1u8.hash(state);
                radius.to_bits().hash(state);
            }
            EffectRenderOp::Sharpen { amount } => {
                2u8.hash(state);
                amount.to_bits().hash(state);
            }
            EffectRenderOp::Vignette { intensity, feather } => {
                3u8.hash(state);
                intensity.to_bits().hash(state);
                feather.to_bits().hash(state);
            }
            EffectRenderOp::ChromaticAberration { amount } => {
                4u8.hash(state);
                amount.to_bits().hash(state);
            }
            EffectRenderOp::Grain { amount } => {
                5u8.hash(state);
                amount.to_bits().hash(state);
            }
            EffectRenderOp::TemporalFrameMix { past_offset, mix } => {
                8u8.hash(state);
                past_offset.hash(state);
                mix.to_bits().hash(state);
            }
            EffectRenderOp::Lut3D { lut, intensity } => {
                6u8.hash(state);
                lut.semantic_fingerprint().hash(state);
                intensity.to_bits().hash(state);
            }
            EffectRenderOp::Custom { key, params, cache_key, cache_policy, processor } => {
                7u8.hash(state);
                key.hash(state);
                processor.as_ref().map(CustomEffectProcessorBinding::revision).hash(state);
                processor
                    .as_ref()
                    .and_then(CustomEffectProcessorBinding::runtime_owner_revision)
                    .hash(state);
                cache_policy.hash(state);
                // `cache_key` identifies stable external/custom implementation
                // semantics; it never replaces the evaluated frame parameters.
                // Omitting `params` here would let animated values reuse a
                // compiled graph containing an earlier frame's payload.
                hash_json_value(params, state);
                if let Some(cache_key) = cache_key {
                    1u8.hash(state);
                    cache_key.hash(state);
                } else {
                    0u8.hash(state);
                }
            }
        }
    }

    pub fn cache_policy(&self) -> EffectCachePolicy {
        match self {
            EffectRenderOp::Grain { .. } => EffectCachePolicy::FrameDependent,
            EffectRenderOp::Custom { cache_policy, .. } => *cache_policy,
            _ => EffectCachePolicy::Deterministic,
        }
    }

    pub fn estimated_cost(&self) -> u32 {
        match self {
            EffectRenderOp::ColorAdjust { .. } => 1,
            EffectRenderOp::Vignette { .. } => 1,
            EffectRenderOp::Grain { .. } => 2,
            EffectRenderOp::Lut3D { .. } => 2,
            EffectRenderOp::GaussianBlur { .. } => 4,
            EffectRenderOp::Sharpen { .. } => 4,
            EffectRenderOp::ChromaticAberration { .. } => 4,
            EffectRenderOp::TemporalFrameMix { .. } => 2,
            EffectRenderOp::Custom { .. } => 5,
        }
    }
}

fn hash_json_value<H: std::hash::Hasher>(value: &serde_json::Value, state: &mut H) {
    use std::hash::Hash;

    match value {
        serde_json::Value::Null => {
            0u8.hash(state);
        }
        serde_json::Value::Bool(boolean) => {
            1u8.hash(state);
            boolean.hash(state);
        }
        serde_json::Value::Number(number) => {
            2u8.hash(state);
            number.to_string().hash(state);
        }
        serde_json::Value::String(text) => {
            3u8.hash(state);
            text.hash(state);
        }
        serde_json::Value::Array(items) => {
            4u8.hash(state);
            items.len().hash(state);
            for item in items {
                hash_json_value(item, state);
            }
        }
        serde_json::Value::Object(map) => {
            5u8.hash(state);
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            entries.len().hash(state);
            for (key, value) in entries {
                key.hash(state);
                hash_json_value(value, state);
            }
        }
    }
}

#[derive(Clone)]
enum EffectEvaluatorFactory {
    Frame(EffectGraphBuilder),
    Prepared(EffectGraphPreparer),
}

#[derive(Clone)]
pub struct EffectDefinition {
    key: String,
    display_name: String,
    category_path: Vec<String>,
    default_properties: PropertyBag,
    evaluator_factory: Option<EffectEvaluatorFactory>,
    execution_contract: EffectExecutionContract,
    plugin_contract: Option<EffectPluginContract>,
    color_domain_contract: EffectColorDomainContract,
    definition_registry_revision: u64,
}

/// Invalid effect definition rejected before it can enter the registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectDefinitionError {
    /// Effect keys are persistent machine identities and cannot be empty.
    #[error("effect definition key cannot be empty")]
    EmptyKey,
    /// A malformed parameter contract cannot enter the global registry.
    #[error("effect `{effect_key}` parameter `{parameter_id}` has invalid schema: {reason}")]
    InvalidParameterSchema {
        effect_key: String,
        parameter_id: ParameterId,
        reason: String,
    },
    /// A definition cannot resolve one stable parameter ID to two addresses.
    #[error(
        "effect `{effect_key}` parameter `{parameter_id}` is duplicated at `{first_address}` and `{second_address}`"
    )]
    DuplicateParameterId {
        effect_key: String,
        parameter_id: ParameterId,
        first_address: String,
        second_address: String,
    },
    /// The typed execution contract is internally inconsistent.
    #[error("effect `{effect_key}` has an invalid execution contract: {reason}")]
    InvalidExecutionContract { effect_key: String, reason: String },
    /// Process-local registry generation can no longer advance safely.
    #[error("effect definition registry revision exhausted")]
    RegistryRevisionExhausted,
}

impl EffectDefinition {
    /// Create an effect definition with an explicit processing-domain contract.
    ///
    /// Callers must select the domain intentionally; definitions never infer a
    /// display, log, data, or mask contract from the render operation.
    pub fn new(
        key: impl Into<String>,
        display_name: impl Into<String>,
        default_properties: PropertyBag,
        color_domain_contract: EffectColorDomainContract,
    ) -> Self {
        Self {
            key: key.into(),
            display_name: display_name.into(),
            category_path: Vec::new(),
            default_properties,
            evaluator_factory: None,
            execution_contract: EffectExecutionContract::default(),
            plugin_contract: None,
            color_domain_contract,
            definition_registry_revision: 0,
        }
    }

    pub fn with_category(mut self, category_path: Vec<String>) -> Self {
        self.category_path = category_path;
        self
    }

    pub fn with_property(mut self, descriptor: PropertyDescriptor) -> Self {
        self.default_properties.define(descriptor);
        self
    }

    pub fn with_properties(mut self, properties: PropertyBag) -> Self {
        for (_, property) in properties.iter() {
            self.default_properties.upsert(property.clone());
        }
        self
    }

    pub fn with_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self {
        self.evaluator_factory = Some(EffectEvaluatorFactory::Frame(graph_builder));
        self.execution_contract.topology = EffectGraphTopology::LinearChain;
        self
    }

    pub fn with_branching_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self {
        self.evaluator_factory = Some(EffectEvaluatorFactory::Frame(graph_builder));
        self.execution_contract.topology = EffectGraphTopology::GeneralDag;
        self
    }

    /// Bind a preparation function that resolves immutable resources once.
    pub fn with_prepared_graph_builder(mut self, graph_preparer: EffectGraphPreparer) -> Self {
        self.evaluator_factory = Some(EffectEvaluatorFactory::Prepared(graph_preparer));
        self
    }

    /// Declare the complete typed execution contract.
    pub fn with_execution_contract(mut self, execution_contract: EffectExecutionContract) -> Self {
        self.execution_contract = execution_contract;
        self
    }

    pub fn with_custom_render_processor(
        self,
        params_builder: EffectRenderParamsBuilder,
        processor: CustomEffectRenderProcessor,
    ) -> Self {
        self.with_custom_render_backend(
            params_builder,
            None,
            EffectCachePolicy::Deterministic,
            processor,
        )
    }

    pub fn with_custom_render_backend(
        mut self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Self {
        let effect_key = self.key.clone();
        let processor = CustomEffectProcessorBinding::new(processor);
        let params_builder_for_graph = Arc::clone(&params_builder);
        let effect_key_for_graph = effect_key.clone();
        let cache_key_builder_for_graph = cache_key_builder.clone();
        self.evaluator_factory = Some(EffectEvaluatorFactory::Frame(Arc::new(
            move |effect, context, graph| {
                if let Some(params) = params_builder_for_graph(effect, context)? {
                    let cache_key = cache_key_builder_for_graph
                        .as_ref()
                        .and_then(|builder| builder(effect, context));
                    graph.append_unary(EffectRenderOp::Custom {
                        key: effect_key_for_graph.clone(),
                        params,
                        cache_key,
                        cache_policy,
                        processor: Some(processor.clone()),
                    });
                }
                Ok(())
            },
        )));
        self.execution_contract.topology = EffectGraphTopology::LinearChain;
        self
    }

    pub fn with_plugin_contract(mut self, plugin_contract: EffectPluginContract) -> Self {
        self.plugin_contract = Some(plugin_contract);
        self
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn category_path(&self) -> &[String] {
        &self.category_path
    }

    pub(crate) fn default_properties(&self) -> &PropertyBag {
        &self.default_properties
    }

    pub(crate) fn has_evaluator(&self) -> bool {
        self.evaluator_factory.is_some()
    }

    /// Complete execution contract owned by this definition.
    pub fn execution_contract(&self) -> EffectExecutionContract {
        self.execution_contract
    }

    /// Exact input/output processing domain declared by this definition.
    pub fn color_domain_contract(&self) -> EffectColorDomainContract {
        self.color_domain_contract
    }

    pub fn supports_visual_evaluation(&self) -> bool {
        self.evaluator_factory.is_some()
            && !self.execution_contract.execution_modes.is_empty()
            && self.execution_contract.state_model == crate::EffectStateModel::Stateless
            && effect_plugin_is_library_visible(
                self.key(),
                self.definition_registry_revision,
                self.plugin_contract(),
            )
    }

    pub fn plugin_contract(&self) -> Option<&EffectPluginContract> {
        self.plugin_contract.as_ref()
    }

    pub(crate) const fn definition_registry_revision(&self) -> u64 {
        self.definition_registry_revision
    }

    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        let category_bytes = self
            .category_path
            .iter()
            .map(String::capacity)
            .fold(0_usize, usize::saturating_add);
        let property_floor = self.default_properties.iter().count().saturating_mul(512);
        let property_bytes = serde_json::to_vec(&self.default_properties)
            .map_or(property_floor, |bytes| bytes.len().max(property_floor));
        let plugin_bytes = self
            .plugin_contract
            .as_ref()
            .map_or(0, |contract| contract.plugin_version.capacity());
        std::mem::size_of::<Self>()
            .saturating_add(self.key.capacity())
            .saturating_add(self.display_name.capacity())
            .saturating_add(
                self.category_path.capacity().saturating_mul(std::mem::size_of::<String>()),
            )
            .saturating_add(category_bytes)
            .saturating_add(property_bytes)
            .saturating_add(plugin_bytes)
            .saturating_add(OPAQUE_EFFECT_EVALUATOR_BASE_CHARGE_BYTES)
    }

    /// Validate stable schema identity before registry publication.
    pub fn validate(&self) -> Result<(), EffectDefinitionError> {
        if self.key.trim().is_empty() {
            return Err(EffectDefinitionError::EmptyKey);
        }
        self.execution_contract.validate().map_err(|error| {
            EffectDefinitionError::InvalidExecutionContract {
                effect_key: self.key.clone(),
                reason: error.to_string(),
            }
        })?;
        let mut parameter_addresses = HashMap::<ParameterId, String>::new();
        for (address, property) in self.default_properties.iter() {
            let schema = &property.descriptor.schema;
            property
                .validate()
                .map_err(|error| EffectDefinitionError::InvalidParameterSchema {
                    effect_key: self.key.clone(),
                    parameter_id: schema.parameter_id.clone(),
                    reason: error.to_string(),
                })?;
            if let Some(first_address) =
                parameter_addresses.insert(schema.parameter_id.clone(), address.to_string())
            {
                return Err(EffectDefinitionError::DuplicateParameterId {
                    effect_key: self.key.clone(),
                    parameter_id: schema.parameter_id.clone(),
                    first_address,
                    second_address: address.to_string(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_evaluator(
        &self,
        effect: &EffectNode,
        working_color_space: WorkingColorSpace,
        resources: EffectPreparationContext<'_>,
    ) -> Result<PreparedEffectEvaluator, EffectGraphBuildError> {
        let factory = self.evaluator_factory.as_ref().ok_or_else(|| {
            EffectGraphBuildError::EvaluationUnsupported {
                effect_key: self.key.clone(),
                effect_id: effect.id,
            }
        })?;
        if self.execution_contract.resource_lifetime == EffectResourceLifetime::PreparedProgram
            && matches!(factory, EffectEvaluatorFactory::Frame(_))
        {
            return Err(EffectGraphBuildError::ResourcePreparationUnsupported {
                effect_key: self.key.clone(),
                effect_id: effect.id,
            });
        }
        match factory {
            EffectEvaluatorFactory::Frame(evaluator) => {
                Ok(PreparedEffectEvaluator::new(Arc::clone(evaluator)))
            }
            EffectEvaluatorFactory::Prepared(preparer) => {
                match catch_unwind(AssertUnwindSafe(|| {
                    preparer(effect, working_color_space, resources)
                })) {
                    Ok(result) => result,
                    Err(_) => {
                        record_plugin_runtime_failure(
                            self.key(),
                            self.definition_registry_revision,
                            self.plugin_contract(),
                            "effect resource preparer panicked",
                        );
                        Err(EffectGraphBuildError::BuilderPanicked {
                            effect_key: self.key.clone(),
                            effect_id: effect.id,
                        })
                    }
                }
            }
        }
    }
}

fn builtin_effect_types() -> [EffectType; 13] {
    [
        EffectType::BasicCorrection,
        EffectType::WhiteBalance,
        EffectType::Lut3D,
        EffectType::ColorWheel,
        EffectType::Curves,
        EffectType::HueSaturationLightness,
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::Vignette,
        EffectType::ChromaticAberration,
        EffectType::Grain,
        EffectType::ChromaKey,
        EffectType::LumaKey,
    ]
}

/// Global effect definition registry. Entries are never evicted — if plugins
/// are installed and later removed, their definitions persist until restart.
/// This is acceptable for a desktop NLE where plugin install/uninstall is rare.
fn effect_registry() -> &'static RwLock<HashMap<String, Arc<EffectDefinition>>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, Arc<EffectDefinition>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut definitions = HashMap::new();
        for effect_type in builtin_effect_types() {
            let mut definition = builtin_effect_definition(effect_type);
            definition.definition_registry_revision = 1;
            definition
                .validate()
                .unwrap_or_else(|error| panic!("invalid built-in effect definition: {error}"));
            definitions.insert(definition.key.clone(), Arc::new(definition));
        }
        RwLock::new(definitions)
    })
}

fn effect_registry_revision_counter() -> &'static AtomicU64 {
    static REVISION: AtomicU64 = AtomicU64::new(1);
    &REVISION
}

/// Process-local revision of the bound definition/plugin registry.
///
/// Prepared-program cache identity includes this value so replacing any plugin
/// definition conservatively invalidates previously bound code and resources.
pub fn effect_registry_revision() -> u64 {
    effect_registry_revision_counter().load(Ordering::Acquire)
}

pub fn register_effect_definition(
    mut definition: EffectDefinition,
) -> Result<(), EffectDefinitionError> {
    definition.validate()?;
    let key = definition.key.clone();
    let mut registry = effect_registry().write().unwrap_or_else(|e| e.into_inner());
    let previous_revision = effect_registry_revision_counter()
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |revision| {
            revision.checked_add(1)
        })
        .map_err(|_| EffectDefinitionError::RegistryRevisionExhausted)?;
    definition.definition_registry_revision = previous_revision + 1;
    registry.insert(key, Arc::new(definition));
    Ok(())
}

pub fn effect_definition(effect_type: &EffectType) -> Option<Arc<EffectDefinition>> {
    let key = effect_type.key();
    effect_registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(key.as_str())
        .cloned()
}

pub fn effect_library_types() -> Vec<EffectType> {
    let registry = effect_registry().read().unwrap_or_else(|e| e.into_inner());
    let mut effects = registry
        .values()
        .filter(|definition| definition.supports_visual_evaluation())
        .map(|definition| EffectType::from_key(definition.key()))
        .collect::<Vec<_>>();
    effects.sort_by(|a, b| a.display_name().cmp(b.display_name()));
    effects
}

/// A node in the effect category tree.
#[derive(Debug, Clone)]
pub struct EffectCategoryNode {
    pub name: String,
    pub children: Vec<EffectCategoryNode>,
    pub effects: Vec<EffectType>,
}

/// Build a hierarchical category tree from all registered effects.
pub fn effect_category_tree() -> Vec<EffectCategoryNode> {
    let registry = effect_registry().read().unwrap_or_else(|e| e.into_inner());
    let mut roots: Vec<EffectCategoryNode> = Vec::new();

    for definition in registry.values() {
        if !definition.supports_visual_evaluation() {
            continue;
        }
        let effect_type = EffectType::from_key(definition.key());
        let path = definition.category_path();

        if path.is_empty() {
            // No category: add to a default "其他" root
            insert_effect_into_tree(&mut roots, &["其他".to_string()], &effect_type);
        } else {
            insert_effect_into_tree(&mut roots, path, &effect_type);
        }
    }

    // Sort each level alphabetically
    sort_category_tree(&mut roots);
    roots
}

fn insert_effect_into_tree(
    nodes: &mut Vec<EffectCategoryNode>,
    path: &[String],
    effect_type: &EffectType,
) {
    if path.is_empty() {
        return;
    }
    let head = &path[0];
    let tail = &path[1..];

    let node = nodes.iter_mut().find(|n| n.name == *head);
    if tail.is_empty() {
        // Leaf: add effect to this category
        if let Some(node) = node {
            node.effects.push(effect_type.clone());
        } else {
            nodes.push(EffectCategoryNode {
                name: head.clone(),
                children: Vec::new(),
                effects: vec![effect_type.clone()],
            });
        }
    } else if let Some(node) = node {
        insert_effect_into_tree(&mut node.children, tail, effect_type);
    } else {
        let mut new_node = EffectCategoryNode {
            name: head.clone(),
            children: Vec::new(),
            effects: Vec::new(),
        };
        insert_effect_into_tree(&mut new_node.children, tail, effect_type);
        nodes.push(new_node);
    }
}

fn sort_category_tree(nodes: &mut [EffectCategoryNode]) {
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    for node in nodes.iter_mut() {
        node.effects.sort_by(|a, b| a.display_name().cmp(b.display_name()));
        sort_category_tree(&mut node.children);
    }
}

/// Evaluate enabled authored effects into one render graph.
///
/// Disabled effects are explicit identity operations. Every enabled effect
/// must have an available evaluator and all required resources.
pub fn build_effect_render_graph(
    effects: &[EffectNode],
    time: TimelineTime,
    working_color_space: WorkingColorSpace,
) -> Result<EffectRenderGraph, EffectGraphBuildError> {
    crate::PreparedEffectStack::prepare(effects, working_color_space)?.evaluate_graph(time)
}

/// Build, mask-inject, and compile the effect graph for a clip.
///
/// This is the entry point used by the render plan builder. It replaces the
/// former `Clip::evaluate_compiled_effect_graph()` method, which lived in
/// `mondrian-timeline` and constituted an architecture violation (P-ARCH1).
pub fn compile_clip_effect_graph(
    effects: &[EffectNode],
    masks: &[MaskComponent],
    time: TimelineTime,
    working_color_space: WorkingColorSpace,
) -> Result<Arc<CompiledEffectGraph>, EffectGraphBuildError> {
    crate::PreparedEffectProgram::prepare(effects, masks, working_color_space)?.evaluate(time)
}

/// Extension trait for EffectNode methods that require the effect registry.
pub trait EffectNodeExt {
    /// Construct an author instance from the currently registered
    /// definition's canonical parameter defaults.
    fn with_defaults(effect_type: EffectType) -> Self;
}

impl EffectNodeExt for EffectNode {
    fn with_defaults(effect_type: EffectType) -> Self {
        let default_properties = effect_definition(&effect_type)
            .map(|definition| definition.default_properties.clone())
            .unwrap_or_default();
        Self {
            id: EffectId::new(),
            properties: default_properties,
            effect_type,
            params: serde_json::json!({}),
            is_enabled: true,
        }
    }
}

// PropertyHost impl for EffectNode moved to mondrian_core::effect_data

fn default_properties_for(effect_type: EffectType) -> PropertyBag {
    let mut properties = PropertyBag::default();

    match &effect_type {
        EffectType::BasicCorrection => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "exposure",
                "基础校正",
                "曝光",
                PropertyValue::Float(0.0),
                Some(-4.0),
                Some(4.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "contrast",
                "基础校正",
                "对比度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(3.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "saturation",
                "基础校正",
                "饱和度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(3.0),
                Some(0.01),
            );
        }
        EffectType::WhiteBalance => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "temperature",
                "白平衡",
                "色温",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint",
                "白平衡",
                "色调",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Lut3D => {
            define_builtin_enum_property(
                &mut properties,
                &effect_type,
                "processing_space",
                "LUT",
                "处理色彩空间",
                "unassigned",
                lut_processing_space_options(),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "path",
                "LUT",
                "LUT 文件",
                PropertyValue::Resource(ParameterResourceReference::Unbound),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "intensity",
                "LUT",
                "LUT 强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::ColorWheel => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "lift",
                "色轮",
                "Lift",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "gamma",
                "色轮",
                "Gamma",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "gain",
                "色轮",
                "Gain",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
        }
        EffectType::Curves => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "master",
                "曲线",
                "主曲线强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
        }
        EffectType::HueSaturationLightness => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "hue",
                "HSL",
                "色相",
                PropertyValue::Float(0.0),
                Some(-180.0),
                Some(180.0),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "saturation",
                "HSL",
                "饱和度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "lightness",
                "HSL",
                "明度",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::GaussianBlur => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "radius",
                "模糊",
                "模糊半径",
                PropertyValue::Float(12.0),
                Some(0.0),
                Some(GAUSSIAN_BLUR_AUTHOR_MAX_RADIUS_PIXELS as f64),
                Some(0.1),
            );
        }
        EffectType::Sharpen => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "锐化",
                "锐化强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::Vignette => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "intensity",
                "暗角",
                "暗角强度",
                PropertyValue::Float(0.35),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "feather",
                "暗角",
                "暗角羽化",
                PropertyValue::Float(0.6),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::ChromaticAberration => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "色差",
                "色差强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Grain => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "颗粒",
                "颗粒强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "size",
                "颗粒",
                "颗粒尺寸",
                PropertyValue::Float(1.0),
                Some(0.1),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::ChromaKey => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "key_color",
                "抠像",
                "抠像颜色",
                PropertyValue::Color(Color::from_hex(0x00FF00)),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "similarity",
                "抠像",
                "相似度",
                PropertyValue::Float(0.2),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend",
                "抠像",
                "边缘混合",
                PropertyValue::Float(0.1),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::LumaKey => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "threshold",
                "亮度键",
                "阈值",
                PropertyValue::Float(0.5),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "softness",
                "亮度键",
                "柔化",
                PropertyValue::Float(0.1),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Plugin(_) => {}
    }

    properties
}

fn define_builtin_property(
    properties: &mut PropertyBag,
    effect_type: &EffectType,
    parameter: &str,
    group: &str,
    name: &str,
    value: PropertyValue,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
) {
    let path = effect_type.property_path(parameter);
    let mut descriptor = PropertyDescriptor::new(path, name, value)
        .with_parameter_id(builtin_parameter_id(effect_type, parameter));
    if let (Some(min), Some(max)) = (min, max) {
        let hard_range = ParameterNumericRange::new(min, max).unwrap_or_else(|error| {
            panic!(
                "invalid built-in hard range for {}.{parameter}: {error}",
                effect_type.key()
            )
        });
        let numeric = ParameterNumericContract::new(
            hard_range,
            hard_range,
            step,
            ParameterInvalidValuePolicy::Reject,
        )
        .unwrap_or_else(|error| {
            panic!(
                "invalid built-in numeric contract for {}.{parameter}: {error}",
                effect_type.key()
            )
        });
        descriptor = descriptor
            .with_numeric_contract(builtin_parameter_unit(effect_type, parameter), numeric);
    }
    if matches!(effect_type, EffectType::Lut3D) && parameter == "path" {
        descriptor = descriptor.with_cache_impact(ParameterCacheImpact::Resource);
    }
    descriptor.ui_metadata = AnimatablePropertyUiMetadata {
        group_name: Some(group.to_string()),
        supports_spatial: false,
    };
    properties.define(descriptor);
}

fn define_builtin_enum_property(
    properties: &mut PropertyBag,
    effect_type: &EffectType,
    parameter: &str,
    group: &str,
    name: &str,
    default_key: &str,
    options: Vec<ParameterEnumOption>,
) {
    let path = effect_type.property_path(parameter);
    let mut descriptor =
        PropertyDescriptor::new(path, name, PropertyValue::Enum(default_key.to_owned()))
            .with_parameter_id(builtin_parameter_id(effect_type, parameter))
            .with_enum_options(options)
            .with_animatable(false);
    descriptor.ui_metadata = AnimatablePropertyUiMetadata {
        group_name: Some(group.to_owned()),
        supports_spatial: false,
    };
    properties.define(descriptor);
}

fn lut_processing_space_options() -> Vec<ParameterEnumOption> {
    [
        "unassigned",
        "scene_linear",
        "rec709",
        "srgb",
        "rec2020",
        "display_p3",
        "rec2100_hlg",
        "rec2100_pq",
        "aces_cct",
        "apple_log_bt2020",
        "sony_slog2_sgamut",
        "sony_slog3_sgamut3",
        "sony_slog3_sgamut3_cine",
        "arri_logc3_wide_gamut3",
        "arri_logc4_wide_gamut4",
        "canon_log2_cinema_gamut_d55",
        "canon_log3_cinema_gamut_d55",
        "panasonic_vlog_vgamut",
        "red_log3g10_wide_gamut_rgb",
        "blackmagic_film_wide_gamut_gen5",
        "dji_dlog_dgamut",
        "davinci_intermediate_wide_gamut",
    ]
    .into_iter()
    .map(|key| ParameterEnumOption::new(key, format!("builtin.lut_3d.processing_space.{key}")))
    .collect()
}

fn lut_processing_domain(key: &str) -> Option<EffectColorDomain> {
    let display_encoded = |color_space| EffectColorDomain::DisplayEncodedRgb { color_space };
    let log = |color_space| EffectColorDomain::LogPerceptualRgb { color_space };
    match key {
        "scene_linear" => Some(EffectColorDomain::SceneLinearRgb),
        "rec709" => Some(display_encoded(ColorSpace::Rec709)),
        "srgb" => Some(display_encoded(ColorSpace::Srgb)),
        "rec2020" => Some(display_encoded(ColorSpace::Rec2020)),
        "display_p3" => Some(display_encoded(ColorSpace::DisplayP3)),
        "rec2100_hlg" => Some(display_encoded(ColorSpace::Rec2100Hlg)),
        "rec2100_pq" => Some(display_encoded(ColorSpace::Rec2100Pq)),
        "aces_cct" => Some(log(ColorSpace::AcesCct)),
        "apple_log_bt2020" => Some(log(ColorSpace::AppleLogBt2020)),
        "sony_slog2_sgamut" => Some(log(ColorSpace::SonySLog2SGamut)),
        "sony_slog3_sgamut3" => Some(log(ColorSpace::SonySLog3SGamut3)),
        "sony_slog3_sgamut3_cine" => Some(log(ColorSpace::SonySLog3SGamut3Cine)),
        "arri_logc3_wide_gamut3" => Some(log(ColorSpace::ArriLogC3WideGamut3)),
        "arri_logc4_wide_gamut4" => Some(log(ColorSpace::ArriLogC4WideGamut4)),
        "canon_log2_cinema_gamut_d55" => Some(log(ColorSpace::CanonLog2CinemaGamutD55)),
        "canon_log3_cinema_gamut_d55" => Some(log(ColorSpace::CanonLog3CinemaGamutD55)),
        "panasonic_vlog_vgamut" => Some(log(ColorSpace::PanasonicVLogVGamut)),
        "red_log3g10_wide_gamut_rgb" => Some(log(ColorSpace::RedLog3G10WideGamutRgb)),
        "blackmagic_film_wide_gamut_gen5" => Some(log(ColorSpace::BlackmagicFilmWideGamutGen5)),
        "dji_dlog_dgamut" => Some(log(ColorSpace::DjiDLogDGamut)),
        "davinci_intermediate_wide_gamut" => Some(log(ColorSpace::DavinciIntermediateWideGamut)),
        _ => None,
    }
}

fn builtin_parameter_unit(effect_type: &EffectType, parameter: &str) -> ParameterUnit {
    match (effect_type, parameter) {
        (EffectType::BasicCorrection, "exposure") => ParameterUnit::Stops,
        (EffectType::HueSaturationLightness, "hue") => ParameterUnit::Degrees,
        (EffectType::GaussianBlur, "radius") => ParameterUnit::Pixels,
        _ => ParameterUnit::Unitless,
    }
}

fn builtin_parameter_id(effect_type: &EffectType, parameter: &str) -> ParameterId {
    effect_type.parameter_id(parameter).unwrap_or_else(|error| {
        panic!(
            "invalid built-in parameter ID for `{}` / `{parameter}`: {error}",
            effect_type.key()
        )
    })
}

fn builtin_effect_category(effect_type: &EffectType) -> Vec<String> {
    match effect_type {
        EffectType::BasicCorrection
        | EffectType::WhiteBalance
        | EffectType::ColorWheel
        | EffectType::Curves
        | EffectType::HueSaturationLightness => vec!["颜色".to_string()],
        EffectType::Lut3D => vec!["颜色".to_string(), "LUT".to_string()],
        EffectType::GaussianBlur | EffectType::Sharpen => vec!["模糊与锐化".to_string()],
        EffectType::Vignette | EffectType::ChromaticAberration | EffectType::Grain => {
            vec!["风格化".to_string()]
        }
        EffectType::ChromaKey | EffectType::LumaKey => vec!["抠像".to_string()],
        EffectType::Plugin(_) => vec!["插件".to_string()],
    }
}

fn builtin_effect_definition(effect_type: EffectType) -> EffectDefinition {
    let category = builtin_effect_category(&effect_type);
    let definition = EffectDefinition::new(
        effect_type.key(),
        builtin_display_name(&effect_type),
        default_properties_for(effect_type.clone()),
        EffectColorDomainContract::SCENE_LINEAR,
    )
    .with_category(category)
    .with_execution_contract(builtin_effect_execution_contract(&effect_type));
    if let Some(graph_preparer) = builtin_graph_preparer_for(&effect_type) {
        definition.with_prepared_graph_builder(graph_preparer)
    } else if let Some(graph_builder) = builtin_graph_builder_for(&effect_type) {
        definition.with_graph_builder(graph_builder)
    } else {
        definition
    }
}

fn builtin_effect_execution_contract(effect_type: &EffectType) -> EffectExecutionContract {
    use crate::{
        EffectDeterminism, EffectExecutionModes, EffectRoiPropagation, EffectStateModel,
        EffectTemporalInputExtent,
    };

    let cpu_linear = EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_F32,
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::LinearChain,
    };
    match effect_type {
        EffectType::BasicCorrection | EffectType::Vignette => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::Grain => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            determinism: EffectDeterminism::FrameSeeded,
            ..cpu_linear
        },
        EffectType::Lut3D => EffectExecutionContract {
            resource_lifetime: EffectResourceLifetime::PreparedProgram,
            ..cpu_linear
        },
        EffectType::GaussianBlur => EffectExecutionContract {
            roi_propagation: finite_kernel_roi_contract(
                GAUSSIAN_BLUR_AUTHOR_MAX_RADIUS_PIXELS as f32,
            ),
            ..cpu_linear
        },
        EffectType::Sharpen => EffectExecutionContract {
            roi_propagation: finite_kernel_roi_contract(SHARPEN_BLUR_RADIUS_PIXELS),
            ..cpu_linear
        },
        EffectType::ChromaticAberration => EffectExecutionContract {
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::WhiteBalance
        | EffectType::ColorWheel
        | EffectType::Curves
        | EffectType::HueSaturationLightness
        | EffectType::ChromaKey
        | EffectType::LumaKey
        | EffectType::Plugin(_) => EffectExecutionContract {
            execution_modes: EffectExecutionModes::NONE,
            ..cpu_linear
        },
    }
}

fn finite_kernel_roi_contract(radius: f32) -> EffectRoiPropagation {
    let halo = crate::adjustment::gaussian_blur_input_halo(radius)
        .expect("built-in Gaussian radius must have a finite implementation halo");
    EffectRoiPropagation::Expand { horizontal_pixels: halo, vertical_pixels: halo }
}

fn builtin_graph_preparer_for(effect_type: &EffectType) -> Option<EffectGraphPreparer> {
    if !matches!(effect_type, EffectType::Lut3D) {
        return None;
    }
    let processing_space_id = builtin_parameter_id(effect_type, "processing_space");
    let path_id = builtin_parameter_id(effect_type, "path");
    let intensity_id = builtin_parameter_id(effect_type, "intensity");
    Some(Arc::new(move |effect, _working_color_space, resources| {
        let processing_key = effect
            .evaluate_enum_parameter(&processing_space_id, TimelineTime::ZERO)
            .unwrap_or_else(|| "unassigned".to_owned());
        let Some(processing_domain) = lut_processing_domain(&processing_key) else {
            return Err(EffectGraphBuildError::ResourceUnavailable {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                parameter_id: processing_space_id.clone(),
                recovery: EffectResourceRecovery::AuthorEdit,
                reason: "LUT processing color space is unassigned".to_owned(),
            });
        };
        let Some(ParameterResourceReference::ExternalFile { path }) =
            effect.evaluate_resource_parameter(&path_id, TimelineTime::ZERO)
        else {
            return Err(EffectGraphBuildError::ResourceUnavailable {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                parameter_id: path_id.clone(),
                recovery: EffectResourceRecovery::AuthorEdit,
                reason: "resource is not bound to an external LUT file".to_string(),
            });
        };
        let prepared_lut = resources.prepare_cube_lut(&path).map_err(|error| {
            EffectGraphBuildError::ResourceUnavailable {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                parameter_id: path_id.clone(),
                recovery: EffectResourceRecovery::ExternalChange,
                reason: format!("{}: {error}", path.display()),
            }
        })?;
        let fingerprint = *prepared_lut.semantic_fingerprint();
        let evaluator_lut = Arc::clone(&prepared_lut);
        let evaluator_intensity_id = intensity_id.clone();
        let evaluator: EffectGraphBuilder = Arc::new(move |effect, context, graph| {
            let intensity =
                effect.evaluate_f32_parameter(&evaluator_intensity_id, context.time, 1.0);
            if intensity > 1.0e-4 {
                graph.append_unary_in_domain(
                    EffectRenderOp::Lut3D { lut: Arc::clone(&evaluator_lut), intensity },
                    EffectColorDomainContract::preserving(processing_domain),
                );
            }
            Ok(())
        });
        Ok(PreparedEffectEvaluator::new(evaluator)
            .with_retained_resource_bytes(prepared_lut.retained_bytes_estimate())
            .with_dependency(EffectResourceDependency::CubeLut {
                path,
                semantic_fingerprint: fingerprint,
            }))
    }))
}

fn builtin_graph_builder_for(effect_type: &EffectType) -> Option<EffectGraphBuilder> {
    match effect_type {
        EffectType::BasicCorrection => {
            let exposure_id = builtin_parameter_id(effect_type, "exposure");
            let contrast_id = builtin_parameter_id(effect_type, "contrast");
            let saturation_id = builtin_parameter_id(effect_type, "saturation");
            Some(Arc::new(move |effect, context, graph| {
                let exposure = effect.evaluate_f32_parameter(&exposure_id, context.time, 0.0);
                let contrast = effect.evaluate_f32_parameter(&contrast_id, context.time, 1.0);
                let saturation = effect.evaluate_f32_parameter(&saturation_id, context.time, 1.0);
                if exposure.abs() > 1.0e-4
                    || (contrast - 1.0).abs() > 1.0e-4
                    || (saturation - 1.0).abs() > 1.0e-4
                {
                    graph.append_unary(EffectRenderOp::ColorAdjust {
                        exposure,
                        contrast,
                        saturation,
                        working_color_space: context.working_color_space,
                    });
                }
                Ok(())
            }))
        }
        EffectType::GaussianBlur => {
            let radius_id = builtin_parameter_id(effect_type, "radius");
            Some(Arc::new(move |effect, context, graph| {
                let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 0.0);
                if radius.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::GaussianBlur { radius });
                }
                Ok(())
            }))
        }
        EffectType::Sharpen => {
            let amount_id = builtin_parameter_id(effect_type, "amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::Sharpen { amount });
                }
                Ok(())
            }))
        }
        EffectType::Vignette => {
            let intensity_id = builtin_parameter_id(effect_type, "intensity");
            let feather_id = builtin_parameter_id(effect_type, "feather");
            Some(Arc::new(move |effect, context, graph| {
                let intensity = effect.evaluate_f32_parameter(&intensity_id, context.time, 0.0);
                let feather = effect.evaluate_f32_parameter(&feather_id, context.time, 0.65);
                if intensity.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::Vignette { intensity, feather });
                }
                Ok(())
            }))
        }
        EffectType::ChromaticAberration => {
            let amount_id = builtin_parameter_id(effect_type, "amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::ChromaticAberration { amount });
                }
                Ok(())
            }))
        }
        EffectType::Grain => {
            let amount_id = builtin_parameter_id(effect_type, "amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::Grain { amount });
                }
                Ok(())
            }))
        }
        _ => None,
    }
}

fn builtin_display_name(effect_type: &EffectType) -> &'static str {
    match effect_type {
        EffectType::BasicCorrection => "基础调色",
        EffectType::WhiteBalance => "白平衡",
        EffectType::Lut3D => "LUT",
        EffectType::ColorWheel => "色轮",
        EffectType::Curves => "曲线",
        EffectType::HueSaturationLightness => "HSL",
        EffectType::GaussianBlur => "模糊",
        EffectType::Sharpen => "锐化",
        EffectType::Vignette => "暗角",
        EffectType::ChromaticAberration => "色差",
        EffectType::Grain => "颗粒",
        EffectType::ChromaKey => "色度抠像",
        EffectType::LumaKey => "亮度键",
        EffectType::Plugin(_) => "插件特效",
    }
}

// EffectType methods (key, from_key, display_name, etc.) are now in mondrian_core::effect_data

/// Registry-aware display name — prefers the registered definition's display_name over the static fallback.
pub fn effect_display_name(effect_type: &EffectType) -> String {
    if let Some(definition) = effect_definition(effect_type) {
        return definition.display_name().to_string();
    }
    if let EffectType::Plugin(key) = effect_type {
        return key.clone();
    }
    effect_type.display_name().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EffectGraphNodeKind;
    use mondrian_core::{
        automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue},
        TimelineTime, WorkingColorSpace,
    };

    const TEST_WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec709;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("valid test time")
    }

    fn test_plugin_execution_contract() -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: crate::EffectExecutionModes::CPU_F32,
            determinism: crate::EffectDeterminism::Deterministic,
            state_model: crate::EffectStateModel::Stateless,
            temporal_input: crate::EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: crate::EffectRoiPropagation::PixelLocal,
            resource_lifetime: crate::EffectResourceLifetime::Frame,
            topology: crate::EffectGraphTopology::LinearChain,
        }
    }

    fn test_custom_execution_contract() -> EffectExecutionContract {
        EffectExecutionContract {
            roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
            execution_modes: crate::EffectExecutionModes::CPU_U8,
            ..test_plugin_execution_contract()
        }
    }

    fn emitted_op_contract_violation(
        key: &str,
        contract: EffectExecutionContract,
        op: EffectRenderOp,
    ) -> crate::EffectExecutionContractViolation {
        let effect_type = EffectType::Plugin(key.to_owned());
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Contract Validation",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(contract)
            .with_graph_builder(Arc::new(move |_, _, graph| {
                graph.append_unary(op.clone());
                Ok(())
            })),
        )
        .expect("register contract validation definition");
        match build_effect_render_graph(
            &[EffectNode::new(effect_type)],
            TimelineTime::ZERO,
            TEST_WORKING_SPACE,
        )
        .expect_err("optimistic declaration must fail closed")
        {
            EffectGraphBuildError::ExecutionContractViolation { violation, .. } => *violation,
            other => panic!("unexpected graph error: {other:?}"),
        }
    }

    #[test]
    fn compile_clip_effect_graph_reuses_static_identity_for_empty_clip() {
        let first =
            compile_clip_effect_graph(&[], &[], tt(0), TEST_WORKING_SPACE).expect("identity graph");
        let second = compile_clip_effect_graph(&[], &[], tt(100), TEST_WORKING_SPACE)
            .expect("identity graph");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.graph().is_identity());
    }

    #[test]
    fn compile_clip_effect_graph_reuses_static_identity_for_disabled_effects() {
        let mut effect = EffectNode::with_defaults(EffectType::GaussianBlur);
        effect.is_enabled = false;

        let first = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("identity graph");
        let second =
            compile_clip_effect_graph(&[], &[], tt(0), TEST_WORKING_SPACE).expect("identity graph");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.graph().is_identity());
    }

    #[test]
    fn builtin_effect_properties_are_animatable() {
        let mut effect = EffectNode::with_defaults(EffectType::GaussianBlur);
        let radius_path = EffectType::GaussianBlur.property_path("radius");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: radius_path.clone(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(8.0)),
            })
            .expect("set start keyframe");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: radius_path.clone(),
                keyframe: Keyframe::linear(tt(10), PropertyValue::Float(28.0)),
            })
            .expect("set end keyframe");

        let value = effect
            .evaluate_property(&radius_path, tt(5))
            .and_then(|value| value.as_f32())
            .expect("evaluate blur");
        assert!((value - 18.0).abs() < 0.01);
    }

    #[test]
    fn builtin_effect_keys_and_property_namespaces_are_canonical() {
        let builtins = builtin_effect_types();
        for effect_type in builtins {
            let key = effect_type.key();
            let namespace = effect_type.property_namespace();
            assert_eq!(
                key.strip_prefix("builtin."),
                Some(namespace.as_str()),
                "builtin key and property namespace should stay in lockstep for {effect_type:?}"
            );

            let properties = default_properties_for(effect_type.clone());
            let mut parameter_ids = std::collections::BTreeSet::new();
            for (path, property) in properties.iter() {
                assert!(
                    path.starts_with(&format!("effect.{namespace}.")),
                    "property path {path} should use canonical namespace {namespace}"
                );
                assert!(
                    parameter_ids.insert(property.descriptor.parameter_id().clone()),
                    "parameter IDs must be unique within {effect_type:?}"
                );
                assert!(
                    property
                        .descriptor
                        .parameter_id()
                        .as_str()
                        .starts_with(&format!("mondrian.effect.{key}.")),
                    "built-in parameter identity must be definition-stable"
                );
            }
        }
    }

    #[test]
    fn effect_library_exposes_only_builtins_with_executable_graphs() {
        let expected = [
            EffectType::BasicCorrection,
            EffectType::Lut3D,
            EffectType::GaussianBlur,
            EffectType::Sharpen,
            EffectType::Vignette,
            EffectType::ChromaticAberration,
            EffectType::Grain,
        ]
        .into_iter()
        .map(|effect_type| effect_type.key())
        .collect::<std::collections::BTreeSet<_>>();
        let actual = effect_library_types()
            .into_iter()
            .filter(|effect_type| !matches!(effect_type, EffectType::Plugin(_)))
            .map(|effect_type| effect_type.key())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(actual, expected);
        for modeled_only in [
            EffectType::WhiteBalance,
            EffectType::ColorWheel,
            EffectType::Curves,
            EffectType::HueSaturationLightness,
            EffectType::ChromaKey,
            EffectType::LumaKey,
        ] {
            let definition = effect_definition(&modeled_only).expect("built-in definition");
            assert!(!definition.supports_visual_evaluation());
        }
    }

    #[test]
    fn builtin_spatial_roi_contract_tracks_author_and_kernel_bounds() {
        let blur = effect_definition(&EffectType::GaussianBlur).expect("blur definition");
        let radius_id = builtin_parameter_id(&EffectType::GaussianBlur, "radius");
        let authored_max = blur
            .default_properties()
            .iter()
            .find(|(_, property)| property.descriptor.parameter_id() == &radius_id)
            .and_then(|(_, property)| property.descriptor.schema.numeric)
            .map(|numeric| numeric.hard_range.max)
            .expect("blur radius hard maximum");
        assert_eq!(
            blur.execution_contract().roi_propagation,
            crate::EffectRoiPropagation::Expand {
                horizontal_pixels: crate::adjustment::gaussian_blur_input_halo(authored_max as f32)
                    .expect("finite maximum blur halo"),
                vertical_pixels: crate::adjustment::gaussian_blur_input_halo(authored_max as f32)
                    .expect("finite maximum blur halo"),
            }
        );

        let sharpen = effect_definition(&EffectType::Sharpen).expect("sharpen definition");
        assert_eq!(
            sharpen.execution_contract().roi_propagation,
            crate::EffectRoiPropagation::Expand {
                horizontal_pixels: crate::adjustment::gaussian_blur_input_halo(
                    SHARPEN_BLUR_RADIUS_PIXELS
                )
                .expect("finite sharpen halo"),
                vertical_pixels: crate::adjustment::gaussian_blur_input_halo(
                    SHARPEN_BLUR_RADIUS_PIXELS
                )
                .expect("finite sharpen halo"),
            }
        );
    }

    #[test]
    fn enabled_modeled_only_effect_fails_instead_of_rendering_identity() {
        let effect = EffectNode::with_defaults(EffectType::ColorWheel);

        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("modeled-only effect must not render as identity");

        assert!(matches!(
            error,
            EffectGraphBuildError::EvaluationUnsupported { effect_key, .. }
                if effect_key == "builtin.color_wheel"
        ));
    }

    #[test]
    fn enabled_lut_without_processing_space_fails_instead_of_guessing_from_file() {
        let effect = EffectNode::with_defaults(EffectType::Lut3D);
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");

        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("unassigned LUT processing space must fail");

        assert!(matches!(
            &error,
            EffectGraphBuildError::ResourceUnavailable {
                effect_key,
                parameter_id,
                recovery: EffectResourceRecovery::AuthorEdit,
                ..
            } if effect_key == "builtin.lut_3d" && *parameter_id == processing_space_id
        ));
        assert!(!error.dependency_refresh_retryable());
    }

    #[test]
    fn enabled_lut_with_explicit_space_but_unbound_resource_fails_closed() {
        let mut effect = EffectNode::with_defaults(EffectType::Lut3D);
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");
        let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
        effect
            .set_static_value_by_parameter(
                &processing_space_id,
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("set processing space");

        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("unbound LUT resource must fail");

        assert!(matches!(
            &error,
            EffectGraphBuildError::ResourceUnavailable {
                parameter_id,
                recovery: EffectResourceRecovery::AuthorEdit,
                ..
            }
                if *parameter_id == path_id
        ));
        assert!(!error.dependency_refresh_retryable());
    }

    #[test]
    fn bound_missing_lut_is_retryable_external_dependency_failure() {
        let mut effect = EffectNode::with_defaults(EffectType::Lut3D);
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");
        let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
        let path = std::env::temp_dir().join(format!("mondrian-missing-{}.cube", effect.id));
        let _ = std::fs::remove_file(&path);
        effect
            .set_static_value_by_parameter(
                &processing_space_id,
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("set processing space");
        effect
            .set_static_value_by_parameter(
                &path_id,
                PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }),
            )
            .expect("bind missing LUT path");

        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("bound missing LUT must fail closed");

        assert!(matches!(
            &error,
            EffectGraphBuildError::ResourceUnavailable {
                parameter_id,
                recovery: EffectResourceRecovery::ExternalChange,
                ..
            } if *parameter_id == path_id
        ));
        assert!(error.dependency_refresh_retryable());
    }

    #[test]
    fn primary_color_graph_identity_includes_sequence_working_space() {
        let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
        let saturation_id = EffectType::BasicCorrection
            .parameter_id("saturation")
            .expect("saturation parameter ID");
        effect
            .set_static_value_by_parameter(&saturation_id, PropertyValue::Float(0.5))
            .expect("set saturation");

        let rec709 = compile_clip_effect_graph(
            &[effect.clone()],
            &[],
            tt(0),
            WorkingColorSpace::LinearRec709,
        )
        .expect("compile Rec.709 graph");
        let rec2020 =
            compile_clip_effect_graph(&[effect], &[], tt(0), WorkingColorSpace::LinearRec2020)
                .expect("compile Rec.2020 graph");

        assert_ne!(rec709.signature_hash(), rec2020.signature_hash());
    }

    #[test]
    fn enabled_unknown_plugin_effect_fails_instead_of_rendering_identity() {
        let effect = EffectNode::new(EffectType::Plugin(
            "plugin.missing.persisted-definition".to_string(),
        ));

        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("missing plugin definition must not render as identity");

        assert!(matches!(
            error,
            EffectGraphBuildError::DefinitionUnavailable { effect_key, .. }
                if effect_key == "plugin.missing.persisted-definition"
        ));
    }

    #[test]
    fn builtin_execution_uses_parameter_identity_and_value_invalidates_graph_signature() {
        let mut effect = EffectNode::with_defaults(EffectType::GaussianBlur);
        let radius_id = builtin_parameter_id(&EffectType::GaussianBlur, "radius");

        let mut addressed = PropertyBag::default();
        for (_, property) in effect.properties.iter() {
            let mut property = property.clone();
            property.descriptor.path = "effect.instance.alias_changed".to_string();
            addressed.upsert(property);
        }
        effect.properties = addressed;
        effect
            .set_static_value_by_parameter(&radius_id, PropertyValue::Float(4.0))
            .expect("set radius by stable ID");
        let first = compile_clip_effect_graph(&[effect.clone()], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile first graph");

        effect
            .set_static_value_by_parameter(&radius_id, PropertyValue::Float(12.0))
            .expect("set changed radius by stable ID");
        let second = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile second graph");

        assert_ne!(first.signature_hash(), second.signature_hash());
        let blur_radius = |graph: &CompiledEffectGraph| {
            graph.graph().nodes.iter().find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: EffectRenderOp::GaussianBlur { radius },
                    ..
                }
                | EffectGraphNodeKind::DomainEffect {
                    op: EffectRenderOp::GaussianBlur { radius },
                    ..
                } => Some(*radius),
                _ => None,
            })
        };
        assert_eq!(blur_radius(&first), Some(4.0));
        assert_eq!(blur_radius(&second), Some(12.0));
    }

    #[test]
    fn builtin_lut_effect_builds_render_op_from_cube_path() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("mondrian-effect-lut-{unique}.cube"));
        std::fs::write(
            &path,
            "LUT_3D_SIZE 2
0 0 0
1 0 0
0 1 0
1 1 0
0 0 1
1 0 1
0 1 1
1 1 1
",
        )
        .expect("cube");

        let mut effect = EffectNode::with_defaults(EffectType::Lut3D);
        let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
        let intensity_id =
            EffectType::Lut3D.parameter_id("intensity").expect("intensity parameter ID");
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");
        effect
            .set_static_value_by_parameter(
                &processing_space_id,
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("set LUT processing space");
        effect
            .set_static_value_by_parameter(
                &path_id,
                PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                    path: path.clone(),
                }),
            )
            .expect("set lut path");
        effect
            .set_static_value_by_parameter(&intensity_id, PropertyValue::Float(0.75))
            .expect("set intensity");

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("build LUT graph");
        assert!(graph.nodes.iter().any(|n| matches!(
            &n.kind,
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Lut3D { .. }, .. }
                | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Lut3D { .. }, .. }
        )));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plugin_can_register_custom_effect_property() {
        let plugin_type = EffectType::Plugin("plugin.ai.auto_exposure".to_string());
        let exposure_path = plugin_type.property_path("exposure");
        let exposure_id = plugin_type.parameter_id("exposure").expect("parameter ID");
        let mut properties = PropertyBag::default();
        properties.define(
            PropertyDescriptor::new(
                exposure_path.clone(),
                "AI 自动曝光",
                PropertyValue::Float(0.0),
            )
            .with_parameter_id(exposure_id.clone()),
        );
        register_effect_definition(
            EffectDefinition::new(
                plugin_type.key(),
                "AI 自动曝光",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(test_plugin_execution_contract())
            .with_graph_builder(Arc::new(move |effect, context, graph| {
                let exposure = effect.evaluate_f32_parameter(&exposure_id, context.time, 0.0);
                if exposure.abs() > 1e-4 {
                    graph.append_unary(EffectRenderOp::ColorAdjust {
                        exposure,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: context.working_color_space,
                    });
                }
                Ok(())
            })),
        )
        .expect("register auto exposure definition");

        let mut effect = EffectNode::with_defaults(plugin_type.clone());
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: exposure_path.clone(),
                value: PropertyValue::Float(0.85),
            })
            .expect("set plugin property");

        let value = effect
            .evaluate_property(&exposure_path, tt(0))
            .and_then(|value| value.as_f32())
            .expect("read plugin property");
        assert!((value - 0.85).abs() < 0.001);

        // Verify the effect produces a graph node
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("build plugin graph");
        assert!(!graph.nodes.is_empty());
        assert_eq!(effect_display_name(&plugin_type), "AI 自动曝光".to_string());
    }

    #[test]
    fn plugin_can_build_custom_render_op_plan() {
        let plugin_type = EffectType::Plugin("plugin.render.glow".to_string());
        let amount_id = plugin_type.parameter_id("amount").expect("parameter ID");
        let mut properties = PropertyBag::default();
        properties.define(
            PropertyDescriptor::new(
                "plugin.render.glow.amount",
                "Glow Amount",
                PropertyValue::Float(0.4),
            )
            .with_parameter_id(amount_id.clone()),
        );
        register_effect_definition(
            EffectDefinition::new(
                plugin_type.key(),
                "Glow",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(test_custom_execution_contract())
            .with_custom_render_processor(
                Arc::new(move |effect, context| {
                    let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.0);
                    if amount > 0.0 {
                        Ok(Some(serde_json::json!({ "amount": amount })))
                    } else {
                        Ok(None)
                    }
                }),
                Arc::new(|_, _, _, _, _| Ok(())),
            ),
        )
        .expect("register custom render definition");

        let effect = EffectNode::with_defaults(plugin_type);
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("build custom graph");
        let custom_node = graph.nodes.iter().find(|n| {
            matches!(
                &n.kind,
                EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Custom { .. }, .. }
            )
        });
        assert!(custom_node.is_some(), "expected custom render op node");
        if let EffectGraphNodeKind::UnaryEffect {
            op: EffectRenderOp::Custom { key, params, cache_key, cache_policy, .. },
            ..
        } = &custom_node.unwrap().kind
        {
            assert_eq!(key, "plugin.render.glow");
            assert_eq!(*cache_key, None);
            assert_eq!(*cache_policy, EffectCachePolicy::Deterministic);
            assert!(
                (params["amount"].as_f64().expect("amount should be numeric") - 0.4).abs() < 1.0e-6
            );
        }
    }

    #[test]
    fn emitted_graph_rejects_optimistic_mode_determinism_and_roi_contracts() {
        let backend = emitted_op_contract_violation(
            "plugin.contract.backend",
            EffectExecutionContract {
                execution_modes: crate::EffectExecutionModes::GPU_F32,
                roi_propagation: crate::EffectRoiPropagation::Expand {
                    horizontal_pixels: 1,
                    vertical_pixels: 1,
                },
                ..test_plugin_execution_contract()
            },
            EffectRenderOp::GaussianBlur { radius: 1.0 },
        );
        assert!(matches!(
            backend,
            crate::EffectExecutionContractViolation::ExecutionModesTooOptimistic { .. }
        ));

        let determinism = emitted_op_contract_violation(
            "plugin.contract.determinism",
            EffectExecutionContract {
                execution_modes: crate::EffectExecutionModes::CPU_F32
                    .union(crate::EffectExecutionModes::GPU_F32),
                ..test_plugin_execution_contract()
            },
            EffectRenderOp::Grain { amount: 0.5 },
        );
        assert!(matches!(
            determinism,
            crate::EffectExecutionContractViolation::DeterminismTooOptimistic { .. }
        ));

        let roi = emitted_op_contract_violation(
            "plugin.contract.roi",
            test_plugin_execution_contract(),
            EffectRenderOp::GaussianBlur { radius: 7.25 },
        );
        assert!(matches!(
            roi,
            crate::EffectExecutionContractViolation::RoiTooOptimistic { .. }
        ));

        let unsupported_gpu_representation = emitted_op_contract_violation(
            "plugin.contract.gpu_u8",
            EffectExecutionContract {
                execution_modes: crate::EffectExecutionModes::GPU_U8,
                ..test_plugin_execution_contract()
            },
            EffectRenderOp::ColorAdjust {
                exposure: 0.0,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: TEST_WORKING_SPACE,
            },
        );
        assert!(matches!(
            unsupported_gpu_representation,
            crate::EffectExecutionContractViolation::ExecutionModesTooOptimistic { .. }
        ));

        let unsupported_custom_representation = emitted_op_contract_violation(
            "plugin.contract.custom_precision",
            EffectExecutionContract {
                execution_modes: crate::EffectExecutionModes::CPU_F32,
                roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
                ..test_plugin_execution_contract()
            },
            EffectRenderOp::Custom {
                key: "plugin.contract.custom_precision".to_owned(),
                params: serde_json::json!({}),
                cache_key: None,
                cache_policy: EffectCachePolicy::Deterministic,
                processor: None,
            },
        );
        assert!(matches!(
            unsupported_custom_representation,
            crate::EffectExecutionContractViolation::ExecutionModesTooOptimistic { .. }
        ));
    }

    #[test]
    fn plugin_can_build_branching_render_graph() {
        let plugin_type = EffectType::Plugin("plugin.graph.glow_mix".to_string());
        let radius_id = plugin_type.parameter_id("radius").expect("radius ID");
        let opacity_id = plugin_type.parameter_id("opacity").expect("opacity ID");
        let mut properties = PropertyBag::default();
        properties.define(
            PropertyDescriptor::new(
                "plugin.graph.glow_mix.radius",
                "Glow Radius",
                PropertyValue::Float(4.0),
            )
            .with_parameter_id(radius_id.clone()),
        );
        properties.define(
            PropertyDescriptor::new(
                "plugin.graph.glow_mix.opacity",
                "Glow Opacity",
                PropertyValue::Float(0.35),
            )
            .with_parameter_id(opacity_id.clone()),
        );
        register_effect_definition(
            EffectDefinition::new(
                plugin_type.key(),
                "Glow Mix",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                roi_propagation: crate::EffectRoiPropagation::Expand {
                    horizontal_pixels: 4,
                    vertical_pixels: 4,
                },
                ..test_plugin_execution_contract()
            })
            .with_branching_graph_builder(Arc::new(move |effect, context, graph| {
                let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 0.0);
                let opacity =
                    effect.evaluate_f32_parameter(&opacity_id, context.time, 0.0).clamp(0.0, 1.0);
                if radius <= 1.0e-4 || opacity <= 1.0e-4 {
                    return Ok(());
                }

                graph.blend_current_with(
                    mondrian_core::types::BlendMode::Screen,
                    opacity,
                    |graph, source| {
                        graph.add_unary_from(source, EffectRenderOp::GaussianBlur { radius })
                    },
                );
                Ok(())
            })),
        )
        .expect("register branching definition");

        let effect = EffectNode::with_defaults(plugin_type.clone());
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("build branching graph");
        assert_eq!(graph.nodes.len(), 3);
        assert!(matches!(
            graph.node(crate::graph::EffectGraphNodeId(1)).map(|node| &node.kind),
            Some(crate::graph::EffectGraphNodeKind::UnaryEffect { .. })
        ));
        assert!(matches!(
            graph.node(crate::graph::EffectGraphNodeId(2)).map(|node| &node.kind),
            Some(crate::graph::EffectGraphNodeKind::Blend { .. })
        ));

        let definition = effect_definition(&plugin_type).expect("effect definition");
        let contract = definition.execution_contract();
        assert_eq!(contract.topology, EffectGraphTopology::GeneralDag);
        assert!(contract.execution_modes.contains(
            crate::EffectProcessingBackend::Cpu,
            crate::EffectWorkingPrecision::Float32,
        ));
    }

    #[test]
    fn custom_render_backend_can_provide_stable_cache_key_contract() {
        let plugin_type = EffectType::Plugin("plugin.render.lut_loader".to_string());
        let path_id = plugin_type.parameter_id("asset_path").expect("asset path ID");
        let mut properties = PropertyBag::default();
        properties.define(
            PropertyDescriptor::new(
                "plugin.render.lut_loader.asset_path",
                "LUT Path",
                PropertyValue::Text("looks/teal_orange.cube".to_string()),
            )
            .with_parameter_id(path_id.clone())
            .with_cache_impact(mondrian_core::automation::ParameterCacheImpact::Resource),
        );
        let params_path_id = path_id.clone();
        let cache_path_id = path_id;
        register_effect_definition(
            EffectDefinition::new(
                plugin_type.key(),
                "LUT Loader",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(test_custom_execution_contract())
            .with_custom_render_backend(
                Arc::new(move |effect, context| {
                    let path = effect
                        .evaluate_parameter(&params_path_id, context.time)
                        .and_then(|value| match value {
                            PropertyValue::Text(text) => Some(text),
                            _ => None,
                        })
                        .unwrap_or_default();
                    Ok(Some(serde_json::json!({ "asset_path": path })))
                }),
                Some(Arc::new(move |effect, context| {
                    effect
                        .evaluate_parameter(&cache_path_id, context.time)
                        .and_then(|value| match value {
                            PropertyValue::Text(text) => Some(text),
                            _ => None,
                        })
                        .map(|path| format!("lut:{path}"))
                })),
                EffectCachePolicy::Deterministic,
                Arc::new(|_, _, _, _, _| Ok(())),
            ),
        )
        .expect("register cached render definition");

        let effect = EffectNode::with_defaults(plugin_type.clone());
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("build cached graph");
        let custom_node = graph.nodes.iter().find_map(|n| match &n.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { cache_key, cache_policy, .. },
                ..
            } => Some((cache_key.clone(), *cache_policy)),
            _ => None,
        });
        let (cache_key, cache_policy) = custom_node.expect("expected custom render op node");
        assert_eq!(cache_key.as_deref(), Some("lut:looks/teal_orange.cube"));
        assert_eq!(cache_policy, EffectCachePolicy::Deterministic);

        let contract =
            effect_definition(&plugin_type).expect("effect definition").execution_contract();
        assert_eq!(contract.topology, EffectGraphTopology::LinearChain);
        assert!(contract.execution_modes.contains(
            crate::EffectProcessingBackend::Cpu,
            crate::EffectWorkingPrecision::NormalizedU8,
        ));
    }

    #[test]
    fn custom_cache_key_does_not_hide_dynamic_parameters_from_graph_identity() {
        let plan = |amount| EffectRenderPlan {
            ops: vec![EffectRenderOp::Custom {
                key: "plugin.render.animated".to_owned(),
                params: serde_json::json!({ "amount": amount }),
                cache_key: Some("stable-resource-v1".to_owned()),
                cache_policy: EffectCachePolicy::Deterministic,
                processor: None,
            }],
        };

        assert_ne!(
            plan(0.25).signature_hash(),
            plan(0.75).signature_hash(),
            "a stable external-resource key must not freeze animated parameters"
        );
    }

    #[test]
    fn failing_plugin_graph_builder_isolated_and_hidden_after_disable_policy() {
        let plugin_type = EffectType::Plugin("plugin.graph.unstable".to_string());
        register_effect_definition(
            EffectDefinition::new(
                plugin_type.key(),
                "Unstable Graph",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(test_plugin_execution_contract())
            .with_plugin_contract(
                crate::EffectPluginContract::new("1.0.0")
                    .with_runtime_failure_policy(
                        crate::EffectPluginRuntimeFailurePolicy::DisableDefinition,
                    )
                    .with_library_policy(crate::EffectPluginLibraryPolicy::HideWhenUnavailable),
            )
            .with_graph_builder(Arc::new(|_, _, _| {
                panic!("unstable graph builder");
            })),
        )
        .expect("register unstable definition");

        let effect = EffectNode::new(plugin_type.clone());
        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("builder must fail");
        assert!(matches!(
            error,
            EffectGraphBuildError::BuilderPanicked { .. }
        ));

        let status =
            crate::effect_plugin_runtime_status(&plugin_type.key()).expect("plugin runtime status");
        assert!(status.disabled);
        assert_eq!(
            status.last_error.as_deref(),
            Some("effect graph builder panicked")
        );
        assert!(!effect_library_types().contains(&plugin_type));
    }
}
