//! 效果节点抽象

use crate::execution::{CustomEffectFloatRenderProcessor, CustomEffectRenderProcessor};
use crate::extension_contract::{
    effect_extension_is_library_visible, record_extension_runtime_failure, EffectExtensionContract,
};
use crate::graph::{CompiledEffectGraph, EffectGraphBuilderState, EffectRenderGraph};
use crate::lut::{Lut3D, LutPreparationCache};
use crate::mask::MaskComponent;
use crate::{
    EffectExecutionContract, EffectExecutionContractViolation, EffectGraphTopology,
    EffectResourceLifetime, EffectRoiPropagation,
};
#[cfg(test)]
use mondrian_core::NormalizedCurvePoint;
use mondrian_core::{
    automation::{
        AnimatablePropertyUiMetadata, NormalizedCurve, ParameterCacheImpact, ParameterEnumOption,
        ParameterInvalidValuePolicy, ParameterNumericContract, ParameterNumericRange,
        ParameterResourceReference, ParameterUnit, PropertyBag, PropertyDescriptor, PropertyValue,
    },
    types::{Color, ColorSpace, EffectId, WorkingColorSpace},
    FrameRounding, ParameterId, Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange,
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

/// Full, zero-offset owner project window, independent of any image raster.
///
/// A project window describes coordinates, not an allocation. Raster capacity
/// and native image-bound limits must be checked against the actual image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "EffectProjectGeometryWire")]
pub struct EffectProjectGeometry {
    width: u32,
    height: u32,
    pixel_aspect_ratio: SampleAspectRatio,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectProjectGeometryWire {
    width: u32,
    height: u32,
    pixel_aspect_ratio: SampleAspectRatio,
}

/// Invalid owner project coordinates, rejected before provider preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectProjectGeometryError {
    /// A full project window must have positive pixel dimensions.
    #[error("effect project window must have positive dimensions")]
    EmptyExtent,
    /// Deserialized sample ratios must satisfy the same rule as constructed ones.
    #[error("effect project sample aspect ratio must be positive")]
    InvalidPixelAspectRatio,
}

impl EffectProjectGeometry {
    /// Bind the complete project window. Partial or offset windows are not admitted.
    pub fn new(
        width: u32,
        height: u32,
        pixel_aspect_ratio: SampleAspectRatio,
    ) -> Result<Self, EffectProjectGeometryError> {
        if width == 0 || height == 0 {
            return Err(EffectProjectGeometryError::EmptyExtent);
        }
        // SampleAspectRatio itself is deserializable. Revalidate and reduce it
        // here so malformed external state cannot enter the execution contract.
        let pixel_aspect_ratio = SampleAspectRatio::new(
            pixel_aspect_ratio.numerator(),
            pixel_aspect_ratio.denominator(),
        )
        .ok_or(EffectProjectGeometryError::InvalidPixelAspectRatio)?;
        Ok(Self { width, height, pixel_aspect_ratio })
    }

    /// Project width in pixels, unrelated to staging image width.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Project height in pixels, unrelated to staging image height.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Exact project sample aspect ratio.
    pub const fn pixel_aspect_ratio(self) -> SampleAspectRatio {
        self.pixel_aspect_ratio
    }

    /// Full canonical extent, lowered only at the spatial ABI boundary.
    pub fn canonical_size(self) -> [f64; 2] {
        [
            f64::from(self.width) * self.pixel_aspect_ratio.to_f64(),
            f64::from(self.height),
        ]
    }
}

impl TryFrom<EffectProjectGeometryWire> for EffectProjectGeometry {
    type Error = EffectProjectGeometryError;

    fn try_from(value: EffectProjectGeometryWire) -> Result<Self, Self::Error> {
        Self::new(value.width, value.height, value.pixel_aspect_ratio)
    }
}

/// Exact sequence facts needed by effects that execute in a frame-based ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectFrameContext {
    frame_rate: Rational,
    project_geometry: EffectProjectGeometry,
    available_range: TimelineTimeRange,
}

/// An owner range cannot be represented as a frame-based effect contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectFrameContextError {
    /// The sequence frame rate must be positive.
    #[error("effect frame rate must be positive")]
    InvalidFrameRate,
    /// The effect owner needs a nonempty available interval.
    #[error("effect frame range is empty")]
    EmptyRange,
    /// Exact interval conversion failed.
    #[error("effect frame range cannot be mapped to its sequence grid: {0}")]
    Time(#[from] mondrian_core::TimelineTimeError),
}

impl EffectFrameContext {
    /// Bind one exact half-open owner range to a validated sequence grid.
    pub fn new(
        frame_rate: Rational,
        project_geometry: EffectProjectGeometry,
        available_range: TimelineTimeRange,
    ) -> Result<Self, EffectFrameContextError> {
        if frame_rate.num <= 0 || frame_rate.den <= 0 {
            return Err(EffectFrameContextError::InvalidFrameRate);
        }
        if available_range.is_empty() {
            return Err(EffectFrameContextError::EmptyRange);
        }
        let value = Self { frame_rate, project_geometry, available_range };
        value.available_frames()?;
        Ok(value)
    }

    /// Sequence frame rate as an exact rational.
    pub const fn frame_rate(self) -> Rational {
        self.frame_rate
    }

    /// Exact width-to-height ratio of a sample.
    pub const fn pixel_aspect_ratio(self) -> SampleAspectRatio {
        self.project_geometry.pixel_aspect_ratio()
    }

    /// Immutable owner window used by project-aware providers and reuse identity.
    pub const fn project_geometry(self) -> EffectProjectGeometry {
        self.project_geometry
    }

    /// Half-open availability interval in the effect owner's time domain.
    pub const fn available_range(self) -> TimelineTimeRange {
        self.available_range
    }

    /// Inclusive frame coordinates covering the entire half-open owner interval.
    /// The first frame may begin before the interval when the owner starts between frames.
    pub fn available_frames(self) -> Result<(i64, i64), EffectFrameContextError> {
        let first = self
            .available_range
            .start
            .to_frame_position(self.frame_rate, FrameRounding::Floor)?
            .frame;
        let last = self
            .available_range
            .end()?
            .to_frame_position(self.frame_rate, FrameRounding::Ceil)?
            .frame
            .checked_sub(1)
            .ok_or(mondrian_core::TimelineTimeError::Overflow)?;
        Ok((first, last))
    }

    /// Lower an exact owner time to the OpenFX-style double frame coordinate.
    pub fn frame_coordinate(self, time: TimelineTime) -> f64 {
        time.to_f64() * self.frame_rate.to_f64()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EffectEvalContext {
    pub time: TimelineTime,
    /// Sequence working identity used by scene-linear effect algorithms.
    pub working_color_space: WorkingColorSpace,
    /// Explicit frame-grid facts; absent only for standalone effect evaluation.
    pub frame_context: Option<EffectFrameContext>,
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
    /// The registered effect definition cannot execute in the current process.
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
    /// The closed grade authoring DAG is invalid before Definition binding.
    #[error("grade graph rooted at {output_node_id} has invalid author state: {reason}")]
    GradeGraphInvalidAuthorState {
        /// Stable selected output identity from the authored graph.
        output_node_id: mondrian_core::GradeGraphNodeId,
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

/// Fallible graph builder shared by built-in and external effect definitions.
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
    /// Definition-owned immutable resource identity.
    ///
    /// The provider must re-register its definition when this identity changes.
    /// Registration advances the global definition revision and invalidates
    /// prepared programs conservatively.
    ExtensionManaged {
        /// Stable provider-defined resource key.
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
        EffectResourceDependency::ExtensionManaged { identity } => identity.capacity(),
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

impl PartialEq for PreparedLut3D {
    fn eq(&self, other: &Self) -> bool {
        self.semantic_fingerprint == other.semantic_fingerprint
    }
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
    /// Working-space-aware Bradford white-balance adaptation.
    WhiteBalance {
        grade: crate::WhiteBalanceGrade,
    },
    /// Lift/Gamma/Gain/Offset primary correction.
    Primaries {
        grade: crate::PrimariesGrade,
    },
    /// Sampled six-zone scene-linear HDR correction.
    HdrGrading {
        grade: Arc<crate::PreparedHdrGrading>,
    },
    /// ASC CDL v1.2 no-clamp SOP/Saturation correction.
    AscCdl {
        grade: crate::AscCdlGrade,
    },
    /// ACES 1.3 Reference Gamut Compression in the Sequence working space.
    GamutCompression {
        grade: crate::GamutCompressionGrade,
    },
    /// Scene-linear clipped-channel chroma reconstruction.
    HighlightRecovery {
        grade: crate::HighlightRecoveryGrade,
    },
    /// Compiled RGB/YRGB and secondary color curves.
    ColorCurves {
        curves: Arc<crate::PreparedColorCurves>,
    },
    /// Unclamped scene-linear RGB hue, saturation, and lightness adjustment.
    HueSaturationLightness {
        grade: crate::HslGrade,
    },
    /// Scene-linear working RGB to refined AlphaMask qualification.
    Qualifier {
        qualifier: Arc<crate::PreparedQualifier>,
    },
    /// Scene-linear RGB chromaticity selection into an AlphaMask.
    ChromaKey {
        keyer: crate::PreparedChromaKey,
    },
    /// AlphaMask to opaque black/white working-RGB observation transform.
    MattePreview {
        invert: bool,
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
    /// Channel-scope invert evaluated on scene-linear RGB(A).
    ///
    /// `blend_with_original` is the retained fraction of the input pixel in
    /// `[0, 1]`: `0.0` keeps the inverted result and `1.0` is the identity. The
    /// graph builder never emits this operation for the identity, and pixels
    /// without positive coverage are left untouched by every scope.
    Invert {
        channels: crate::InvertChannels,
        blend_with_original: f32,
    },
    /// Separable box blur repeated over the selected axes.
    ///
    /// One pass is a normalized box average over `radius` pixels; repeating it
    /// `iterations` times approximates a Gaussian kernel with a triangular
    /// response. `radius` is the authored pixel radius, so the read extent is
    /// `ceil(radius) * iterations` and the graph declares that halo. Samples the
    /// kernel reaches beyond the frame either repeat the boundary pixel or count
    /// as transparent coverage, matching `repeat_edge_pixels`.
    BoxBlur {
        radius: f32,
        iterations: u32,
        direction: crate::BoxBlurDirection,
        repeat_edge_pixels: bool,
    },
    /// Unsharp mask: amplifies the difference against a Gaussian-blurred copy.
    ///
    /// `gain` is the dimensionless amount (`amount_percent / 100`, so `0` is
    /// identity and `5` is the authored maximum of 500%). `radius` is the
    /// Gaussian radius in pixels and `threshold` is the difference dead-zone in
    /// the effect working domain: per-channel differences whose magnitude is at
    /// or below it are left alone. Colors are not clamped, so HDR values keep
    /// their range.
    UnsharpMask {
        gain: f32,
        radius: f32,
        threshold: f32,
    },
    /// Hue, saturation, and lightness adjustment scoped to one authored hue band.
    ///
    /// The grade carries the band selection, the hue turn, chroma scale, and
    /// lightness offset, plus the optional colorize stage. It is evaluated on
    /// unclamped working-space RGB, so extended-range and negative channels keep
    /// their meaning and neutral pixels only receive the lightness offset.
    HueSaturation {
        grade: crate::HueSaturationGrade,
    },
    /// Hue-keyed black and white conversion.
    ///
    /// Six per-hue weights scale the working-space lightness of every pixel, and
    /// the authored tint then ramps the gray toward the tint color. The weighting
    /// is linear in lightness, so extended-range and negative channels keep their
    /// (unclamped) value, and pixels without a hue keep their own lightness.
    BlackAndWhite {
        grade: crate::BlackAndWhiteGrade,
    },
    /// Channel rewiring driven by ten per-pixel scalars (CHN-07).
    ///
    /// Each output channel copies one scalar sampled from the incoming pixel:
    /// a color channel, alpha, the working-space CIE Y, an HSL component, or
    /// the pixel's maximum/minimum. Values are copied without clamping, and
    /// neutral pixels report hue 0 and saturation 0 because neither is defined
    /// once chroma vanishes.
    ShiftChannels {
        grade: crate::ShiftChannelsGrade,
    },
    /// Directional (motion) blur: averages the frame along `direction_degrees` over a
    /// `length` pixel long smear. Angles rotate clockwise in screen coordinates, so 0
    /// smears to the right and 90 downwards. Colour is filtered with its coverage and
    /// values are never clamped.
    DirectionalBlur {
        length: f32,
        direction_degrees: f32,
    },
    /// Linear channel mixer: every output channel is a weighted sum of the three input
    /// channels plus a constant, authored in percent. Mixing is linear, so it runs in the
    /// scene-linear working domain without clamping.
    ChannelMixer {
        grade: crate::ChannelMixerGrade,
    },
    /// Drop shadow: the source coverage is offset, softened and tinted, then
    /// composited underneath (or instead of) the source. The shadow reads complete
    /// coverage from outside the frame as transparent, and the grade carries the
    /// final scene-linear shadow color.
    FindEdges {
        grade: crate::FindEdgesGrade,
    },
    /// Posterize: quantise each channel into the authored number of levels.
    Posterize {
        grade: crate::PosterizeGrade,
    },
    /// Mosaic: one colour per block of the authored pixel grid.
    Mosaic {
        grade: crate::MosaicGrade,
    },
    /// Motion tile: repeats the frame over a tiled, optionally mirrored field.
    MotionTile {
        grade: crate::MotionTileGrade,
    },
    /// Kaleidoscope: folds the frame into radial mirror wedges.
    Kaleidoscope {
        grade: crate::KaleidoscopeGrade,
    },
    /// Bevel: shades the coverage edge into a lit relief.
    Bevel {
        grade: crate::BevelGrade,
    },
    /// Scatter: replicates the frame with position-hashed jitter.
    Scatter {
        grade: crate::ScatterGrade,
    },
    /// Brush strokes: averages a short line of samples running along the
    /// authored stroke direction, with position-hashed phase and jitter.
    BrushStrokes {
        grade: crate::BrushStrokesGrade,
    },
    /// Threshold: black or white depending on the working-space luminance.
    Threshold {
        grade: crate::ThresholdGrade,
    },
    /// Tint: duotone remap of the working-space luminance between two authored
    /// targets.
    Tint {
        grade: crate::TintGrade,
    },
    /// Vibrance: a saturation scale that protects already saturated colours.
    Vibrance {
        grade: crate::VibranceGrade,
    },
    /// Tritone: a three-colour ramp sampled by the pixel's luminance.
    Tritone {
        grade: crate::TritoneGrade,
    },
    /// ColorEmboss: per-channel relief computed from the Sobel gradient of
    /// the working-space colour.
    ColorEmboss {
        grade: crate::ColorEmbossGrade,
    },
    /// Emboss: directional relief computed from the Sobel gradient of the
    /// scene-linear luminance, blended over the source. The gradient reads the
    /// unmodified frame, so repeated passes are order independent.
    Emboss {
        grade: crate::EmbossGrade,
    },
    DropShadow {
        grade: crate::DropShadowGrade,
    },
    /// Source-relative hard crop expressed as normalized edge insets.
    ///
    /// Pixels outside the remaining rectangle become transparent black before
    /// the Clip spatial transform. Insets are evaluated in the complete source
    /// frame coordinate system, including tiled/ROI execution.
    Crop {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
    /// Deterministic finite temporal-sample proof processor.
    ///
    /// The output coverage-correctly mixes the current upstream frame and the
    /// source frame at `time + sample_offset`: straight-alpha inputs are
    /// premultiplied for interpolation, then returned as straight alpha. At the
    /// effect-domain zero crossing the signed time remains exact; source-handle
    /// policy belongs to the prepared placement Adapter. A temporal executor
    /// must provide both frames; single-frame executors fail admission before
    /// reaching this operation.
    TemporalFrameBlend {
        /// Exact signed sample offset in the Clip visual author domain.
        /// Negative values request history and positive values request
        /// lookahead.
        sample_offset: TimelineTime,
        /// Weight of the offset sample in `[0, 1]`.
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
            Self::WhiteBalance { grade } => {
                formatter.debug_struct("WhiteBalance").field("grade", grade).finish()
            }
            Self::Primaries { grade } => {
                formatter.debug_struct("Primaries").field("grade", grade).finish()
            }
            Self::HdrGrading { grade } => formatter
                .debug_struct("HdrGrading")
                .field("semantic_fingerprint", grade.semantic_fingerprint())
                .finish(),
            Self::AscCdl { grade } => {
                formatter.debug_struct("AscCdl").field("grade", grade).finish()
            }
            Self::GamutCompression { grade } => {
                formatter.debug_struct("GamutCompression").field("grade", grade).finish()
            }
            Self::HighlightRecovery { grade } => {
                formatter.debug_struct("HighlightRecovery").field("grade", grade).finish()
            }
            Self::ColorCurves { curves } => {
                formatter.debug_struct("ColorCurves").field("curves", curves).finish()
            }
            Self::HueSaturationLightness { grade } => {
                formatter.debug_struct("HueSaturationLightness").field("grade", grade).finish()
            }
            Self::Qualifier { qualifier } => formatter
                .debug_struct("Qualifier")
                .field("semantic_fingerprint", qualifier.semantic_fingerprint())
                .finish(),
            Self::ChromaKey { keyer } => {
                formatter.debug_struct("ChromaKey").field("keyer", keyer).finish()
            }
            Self::MattePreview { invert } => {
                formatter.debug_struct("MattePreview").field("invert", invert).finish()
            }
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
            Self::Invert { channels, blend_with_original } => formatter
                .debug_struct("Invert")
                .field("channels", channels)
                .field("blend_with_original", blend_with_original)
                .finish(),
            Self::BoxBlur { radius, iterations, direction, repeat_edge_pixels } => formatter
                .debug_struct("BoxBlur")
                .field("radius", radius)
                .field("iterations", iterations)
                .field("direction", direction)
                .field("repeat_edge_pixels", repeat_edge_pixels)
                .finish(),
            Self::BlackAndWhite { grade } => {
                formatter.debug_struct("BlackAndWhite").field("grade", grade).finish()
            }
            Self::ShiftChannels { grade } => {
                formatter.debug_struct("ShiftChannels").field("grade", grade).finish()
            }
            Self::FindEdges { grade } => {
                formatter.debug_struct("FindEdges").field("grade", grade).finish()
            }
            Self::Posterize { grade } => {
                formatter.debug_struct("Posterize").field("grade", grade).finish()
            }
            Self::MotionTile { grade } => {
                formatter.debug_struct("MotionTile").field("grade", grade).finish()
            }
            Self::Kaleidoscope { grade } => {
                formatter.debug_struct("Kaleidoscope").field("grade", grade).finish()
            }
            Self::Bevel { grade } => formatter.debug_struct("Bevel").field("grade", grade).finish(),
            Self::Scatter { grade } => {
                formatter.debug_struct("Scatter").field("grade", grade).finish()
            }
            Self::BrushStrokes { grade } => {
                formatter.debug_struct("BrushStrokes").field("grade", grade).finish()
            }
            Self::Mosaic { grade } => {
                formatter.debug_struct("Mosaic").field("grade", grade).finish()
            }
            Self::Threshold { grade } => {
                formatter.debug_struct("Threshold").field("grade", grade).finish()
            }
            Self::Tint { grade } => formatter.debug_struct("Tint").field("grade", grade).finish(),
            Self::Vibrance { grade } => {
                formatter.debug_struct("Vibrance").field("grade", grade).finish()
            }
            Self::Tritone { grade } => {
                formatter.debug_struct("Tritone").field("grade", grade).finish()
            }
            Self::ColorEmboss { grade } => {
                formatter.debug_struct("ColorEmboss").field("grade", grade).finish()
            }
            Self::Emboss { grade } => {
                formatter.debug_struct("Emboss").field("grade", grade).finish()
            }
            Self::DropShadow { grade } => {
                formatter.debug_struct("DropShadow").field("grade", grade).finish()
            }
            Self::ChannelMixer { grade } => {
                formatter.debug_struct("ChannelMixer").field("grade", grade).finish()
            }
            Self::DirectionalBlur { length, direction_degrees } => formatter
                .debug_struct("DirectionalBlur")
                .field("length", length)
                .field("direction_degrees", direction_degrees)
                .finish(),
            Self::HueSaturation { grade } => {
                formatter.debug_struct("HueSaturation").field("grade", grade).finish()
            }
            Self::UnsharpMask { gain, radius, threshold } => formatter
                .debug_struct("UnsharpMask")
                .field("gain", gain)
                .field("radius", radius)
                .field("threshold", threshold)
                .finish(),
            Self::Crop { left, top, right, bottom } => formatter
                .debug_struct("Crop")
                .field("left", left)
                .field("top", top)
                .field("right", right)
                .field("bottom", bottom)
                .finish(),
            Self::TemporalFrameBlend { sample_offset, mix } => formatter
                .debug_struct("TemporalFrameBlend")
                .field("sample_offset", sample_offset)
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
    alpha_contract: crate::EffectAlphaContract,
    input_role: crate::EffectFloatProcessorInputRole,
    image_scale_requirement: crate::EffectImageScaleRequirement,
    requires_input_service: bool,
    revision: u64,
    processor: CustomEffectProcessor,
    runtime_owner: Option<CustomEffectRuntimeOwner>,
    occurrence_id: Option<EffectId>,
    project_geometry: Option<EffectProjectGeometry>,
}

#[derive(Clone)]
enum CustomEffectProcessor {
    Rgba8(CustomEffectRenderProcessor),
    Float32(CustomEffectFloatRenderProcessor),
    Float32Factory(Arc<dyn crate::EffectFloatProcessorFactory>),
}

static NEXT_CUSTOM_PROCESSOR_REVISION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct CustomEffectRuntimeOwner {
    effect_key: String,
    definition_registry_revision: u64,
    contract: EffectExtensionContract,
}

impl CustomEffectProcessorBinding {
    pub(crate) fn new(processor: CustomEffectRenderProcessor) -> Self {
        Self {
            alpha_contract: crate::EffectAlphaContract::COVERAGE_OPERATION,
            input_role: crate::EffectFloatProcessorInputRole::SingleImage,
            image_scale_requirement: crate::EffectImageScaleRequirement::Any,
            requires_input_service: false,
            revision: NEXT_CUSTOM_PROCESSOR_REVISION.fetch_add(1, Ordering::AcqRel),
            processor: CustomEffectProcessor::Rgba8(processor),
            runtime_owner: None,
            occurrence_id: None,
            project_geometry: None,
        }
    }

    pub(crate) fn new_f32(processor: CustomEffectFloatRenderProcessor) -> Self {
        Self {
            alpha_contract: crate::EffectAlphaContract::COVERAGE_OPERATION,
            input_role: crate::EffectFloatProcessorInputRole::SingleImage,
            image_scale_requirement: crate::EffectImageScaleRequirement::Any,
            requires_input_service: false,
            revision: NEXT_CUSTOM_PROCESSOR_REVISION.fetch_add(1, Ordering::AcqRel),
            processor: CustomEffectProcessor::Float32(processor),
            runtime_owner: None,
            occurrence_id: None,
            project_geometry: None,
        }
    }

    /// Immutable alpha declaration retained by the compiled provider binding.
    pub const fn alpha_contract(&self) -> crate::EffectAlphaContract {
        self.alpha_contract
    }

    /// Immutable image arity retained with the provider declaration.
    pub const fn input_role(&self) -> crate::EffectFloatProcessorInputRole {
        self.input_role
    }

    /// Immutable render-scale restriction retained with the provider declaration.
    pub const fn image_scale_requirement(&self) -> crate::EffectImageScaleRequirement {
        self.image_scale_requirement
    }

    /// Process-local implementation revision used by graph/cache identity.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Frozen additional-input dependency declaration of this provider.
    pub const fn requires_input_service(&self) -> bool {
        self.requires_input_service
    }

    pub(crate) fn rgba8_processor(&self) -> Option<&CustomEffectRenderProcessor> {
        match &self.processor {
            CustomEffectProcessor::Rgba8(processor) => Some(processor),
            CustomEffectProcessor::Float32(_) | CustomEffectProcessor::Float32Factory(_) => None,
        }
    }

    pub(crate) fn float32_processor(&self) -> Option<&CustomEffectFloatRenderProcessor> {
        match &self.processor {
            CustomEffectProcessor::Rgba8(_) | CustomEffectProcessor::Float32Factory(_) => None,
            CustomEffectProcessor::Float32(processor) => Some(processor),
        }
    }

    /// Author identity of the exact compiled occurrence, when compiler-bound.
    pub const fn occurrence_id(&self) -> Option<EffectId> {
        self.occurrence_id
    }

    /// Compiler-bound owner coordinates; absent for standalone literal effects.
    pub const fn project_geometry(&self) -> Option<EffectProjectGeometry> {
        self.project_geometry
    }

    pub(crate) fn float32_factory(&self) -> Option<&Arc<dyn crate::EffectFloatProcessorFactory>> {
        if let CustomEffectProcessor::Float32Factory(factory) = &self.processor {
            Some(factory)
        } else {
            None
        }
    }

    /// A provider cannot change any declaration after its binding is frozen.
    pub(crate) fn factory_contract_matches(&self) -> bool {
        self.float32_factory().is_none_or(|factory| {
            factory.input_role() == self.input_role
                && factory.alpha_contract() == self.alpha_contract
                && factory.image_scale_requirement() == self.image_scale_requirement
                && factory.requires_input_service() == self.requires_input_service
        })
    }

    pub(crate) fn supports_float32(&self) -> bool {
        !matches!(self.processor, CustomEffectProcessor::Rgba8(_))
    }

    pub(crate) fn execution_modes(&self) -> crate::EffectExecutionModes {
        match self.processor {
            CustomEffectProcessor::Rgba8(_) => crate::EffectExecutionModes::CPU_U8,
            CustomEffectProcessor::Float32(_) | CustomEffectProcessor::Float32Factory(_) => {
                crate::EffectExecutionModes::CPU_F32
            }
        }
    }

    pub(crate) fn with_runtime_owner(
        &self,
        effect_key: &str,
        occurrence_id: EffectId,
        definition_registry_revision: u64,
        contract: Option<&EffectExtensionContract>,
        project_geometry: Option<EffectProjectGeometry>,
    ) -> Self {
        Self {
            alpha_contract: self.alpha_contract,
            input_role: self.input_role,
            image_scale_requirement: self.image_scale_requirement,
            requires_input_service: self.requires_input_service,
            revision: self.revision,
            processor: self.processor.clone(),
            occurrence_id: Some(occurrence_id),
            project_geometry,
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
        record_extension_runtime_failure(
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
            .field("alpha_contract", &self.alpha_contract)
            .field("image_scale_requirement", &self.image_scale_requirement)
            .field("requires_input_service", &self.requires_input_service)
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
            EffectRenderOp::WhiteBalance { grade } => {
                10u8.hash(state);
                for value in grade.matrix().into_iter().flatten() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::Primaries { grade } => {
                11u8.hash(state);
                for value in grade
                    .offset()
                    .into_iter()
                    .chain(grade.lift_delta())
                    .chain(grade.inverse_gamma())
                    .chain(grade.gain())
                {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::HdrGrading { grade } => {
                18u8.hash(state);
                grade.semantic_fingerprint().hash(state);
            }
            EffectRenderOp::AscCdl { grade } => {
                12u8.hash(state);
                for value in grade
                    .slope()
                    .into_iter()
                    .chain(grade.offset())
                    .chain(grade.power())
                    .chain([grade.saturation()])
                {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::GamutCompression { grade } => {
                16u8.hash(state);
                grade.amount().to_bits().hash(state);
                grade.working_color_space().hash(state);
            }
            EffectRenderOp::HighlightRecovery { grade } => {
                17u8.hash(state);
                grade.threshold().to_bits().hash(state);
                grade.rolloff().to_bits().hash(state);
                grade.strength().to_bits().hash(state);
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::ColorCurves { curves } => {
                13u8.hash(state);
                curves.semantic_fingerprint().hash(state);
            }
            EffectRenderOp::HueSaturationLightness { grade } => {
                19u8.hash(state);
                for value in grade.controls() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::Qualifier { qualifier } => {
                14u8.hash(state);
                qualifier.semantic_fingerprint().hash(state);
            }
            EffectRenderOp::ChromaKey { keyer } => {
                20u8.hash(state);
                for value in keyer.controls() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::MattePreview { invert } => {
                15u8.hash(state);
                invert.hash(state);
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
            EffectRenderOp::Invert { channels, blend_with_original } => {
                21u8.hash(state);
                channels.hash(state);
                blend_with_original.to_bits().hash(state);
            }
            EffectRenderOp::BoxBlur { radius, iterations, direction, repeat_edge_pixels } => {
                22u8.hash(state);
                radius.to_bits().hash(state);
                iterations.hash(state);
                direction.hash(state);
                u8::from(*repeat_edge_pixels).hash(state);
            }
            EffectRenderOp::BlackAndWhite { grade } => {
                25u8.hash(state);
                for value in grade.gpu_controls() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::FindEdges { grade } => {
                30u8.hash(state);
                grade.blend_with_original().to_bits().hash(state);
                u8::from(grade.invert()).hash(state);
            }
            EffectRenderOp::Posterize { grade } => {
                35u8.hash(state);
                grade.levels().hash(state);
            }
            EffectRenderOp::Scatter { grade } => {
                39u8.hash(state);
                grade.amount_pixels().to_bits().hash(state);
                grade.copies().hash(state);
                grade.randomness().to_bits().hash(state);
            }
            EffectRenderOp::BrushStrokes { grade } => {
                40u8.hash(state);
                grade.angle_degrees().to_bits().hash(state);
                grade.size().to_bits().hash(state);
                grade.length().to_bits().hash(state);
                grade.density().to_bits().hash(state);
                grade.randomness().to_bits().hash(state);
                grade.surface().hash(state);
                grade.blend_with_original().to_bits().hash(state);
            }
            EffectRenderOp::Bevel { grade } => {
                38u8.hash(state);
                grade.thickness().to_bits().hash(state);
                grade.angle_degrees().to_bits().hash(state);
                for value in grade.light_color() {
                    value.to_bits().hash(state);
                }
                grade.intensity().to_bits().hash(state);
            }
            EffectRenderOp::Kaleidoscope { grade } => {
                37u8.hash(state);
                for value in grade.center() {
                    value.to_bits().hash(state);
                }
                grade.size_percent().to_bits().hash(state);
                grade.mirroring().hash(state);
                grade.rotation_degrees().to_bits().hash(state);
                u8::from(grade.floating_center()).hash(state);
            }
            EffectRenderOp::MotionTile { grade } => {
                36u8.hash(state);
                for value in grade.center() {
                    value.to_bits().hash(state);
                }
                for value in [
                    grade.tile_width_percent(),
                    grade.tile_height_percent(),
                    grade.output_width_percent(),
                    grade.output_height_percent(),
                    grade.phase(),
                    grade.horizontal_phase(),
                ] {
                    value.to_bits().hash(state);
                }
                u8::from(grade.mirror_edges()).hash(state);
            }
            EffectRenderOp::Mosaic { grade } => {
                34u8.hash(state);
                grade.horizontal().hash(state);
                grade.vertical().hash(state);
                u8::from(grade.sharp_colors()).hash(state);
            }
            EffectRenderOp::Threshold { grade } => {
                33u8.hash(state);
                grade.level().to_bits().hash(state);
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::Tint { grade } => {
                41u8.hash(state);
                for value in grade.black() {
                    value.to_bits().hash(state);
                }
                for value in grade.white() {
                    value.to_bits().hash(state);
                }
                grade.amount().to_bits().hash(state);
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::Vibrance { grade } => {
                42u8.hash(state);
                grade.vibrance().to_bits().hash(state);
                grade.saturation().to_bits().hash(state);
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::Tritone { grade } => {
                43u8.hash(state);
                for value in grade.shadows() {
                    value.to_bits().hash(state);
                }
                for value in grade.midtones() {
                    value.to_bits().hash(state);
                }
                for value in grade.highlights() {
                    value.to_bits().hash(state);
                }
                grade.blend_with_original().to_bits().hash(state);
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::ColorEmboss { grade } => {
                32u8.hash(state);
                for value in grade.gpu_controls() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::Emboss { grade } => {
                31u8.hash(state);
                for value in grade.gpu_controls() {
                    value.to_bits().hash(state);
                }
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::DropShadow { grade } => {
                29u8.hash(state);
                for value in grade.color() {
                    value.to_bits().hash(state);
                }
                grade.opacity().to_bits().hash(state);
                grade.direction_degrees().to_bits().hash(state);
                grade.distance().to_bits().hash(state);
                grade.softness().to_bits().hash(state);
                u8::from(grade.shadow_only()).hash(state);
            }
            EffectRenderOp::ChannelMixer { grade } => {
                28u8.hash(state);
                for value in grade.weights() {
                    value.to_bits().hash(state);
                }
                u8::from(grade.monochrome()).hash(state);
            }
            EffectRenderOp::DirectionalBlur { length, direction_degrees } => {
                27u8.hash(state);
                length.to_bits().hash(state);
                direction_degrees.to_bits().hash(state);
            }
            EffectRenderOp::ShiftChannels { grade } => {
                26u8.hash(state);
                grade.packed_sources().hash(state);
                for value in grade.luminance_coefficients() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::HueSaturation { grade } => {
                24u8.hash(state);
                grade.band().key().hash(state);
                for value in grade.author_controls() {
                    value.to_bits().hash(state);
                }
            }
            EffectRenderOp::UnsharpMask { gain, radius, threshold } => {
                23u8.hash(state);
                gain.to_bits().hash(state);
                radius.to_bits().hash(state);
                threshold.to_bits().hash(state);
            }
            EffectRenderOp::Crop { left, top, right, bottom } => {
                9u8.hash(state);
                left.to_bits().hash(state);
                top.to_bits().hash(state);
                right.to_bits().hash(state);
                bottom.to_bits().hash(state);
            }
            EffectRenderOp::TemporalFrameBlend { sample_offset, mix } => {
                8u8.hash(state);
                sample_offset.hash(state);
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
                processor.as_ref().map(CustomEffectProcessorBinding::alpha_contract).hash(state);
                processor
                    .as_ref()
                    .map(CustomEffectProcessorBinding::image_scale_requirement)
                    .hash(state);
                processor
                    .as_ref()
                    .and_then(CustomEffectProcessorBinding::project_geometry)
                    .hash(state);
                processor
                    .as_ref()
                    .and_then(CustomEffectProcessorBinding::runtime_owner_revision)
                    .hash(state);
                processor
                    .as_ref()
                    .map(CustomEffectProcessorBinding::requires_input_service)
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
            EffectRenderOp::Custom { processor: Some(binding), .. }
                if binding.requires_input_service() =>
            {
                EffectCachePolicy::Uncacheable
            }
            EffectRenderOp::Custom { cache_policy, .. } => *cache_policy,
            _ => EffectCachePolicy::Deterministic,
        }
    }

    pub fn estimated_cost(&self) -> u32 {
        match self {
            EffectRenderOp::ColorAdjust { .. } => 1,
            EffectRenderOp::WhiteBalance { .. }
            | EffectRenderOp::Primaries { .. }
            | EffectRenderOp::AscCdl { .. }
            | EffectRenderOp::GamutCompression { .. }
            | EffectRenderOp::HighlightRecovery { .. } => 1,
            EffectRenderOp::HdrGrading { .. } => 2,
            EffectRenderOp::ColorCurves { .. } => 3,
            EffectRenderOp::HueSaturationLightness { .. } => 2,
            EffectRenderOp::Qualifier { qualifier } => {
                4 + qualifier.denoise_radius() + qualifier.input_halo()
            }
            EffectRenderOp::ChromaKey { .. } => 2,
            EffectRenderOp::MattePreview { .. } => 1,
            EffectRenderOp::Vignette { .. } => 1,
            EffectRenderOp::Grain { .. } => 2,
            EffectRenderOp::Crop { .. } => 1,
            EffectRenderOp::Invert { .. } => 1,
            EffectRenderOp::BoxBlur { .. } => 4,
            EffectRenderOp::BlackAndWhite { .. } => 2,
            EffectRenderOp::ShiftChannels { .. } => 1,
            EffectRenderOp::DirectionalBlur { .. } => 4,
            EffectRenderOp::ChannelMixer { .. } => 2,
            EffectRenderOp::DropShadow { .. } => 6,
            EffectRenderOp::FindEdges { .. } => 4,
            EffectRenderOp::Threshold { .. } => 1,
            EffectRenderOp::Tint { .. } => 1,
            EffectRenderOp::Vibrance { .. } => 1,
            EffectRenderOp::Tritone { .. } => 1,
            EffectRenderOp::Mosaic { .. } => 2,
            EffectRenderOp::Posterize { .. } => 1,
            EffectRenderOp::MotionTile { .. } => 3,
            EffectRenderOp::Kaleidoscope { .. } => 3,
            EffectRenderOp::Bevel { .. } => 2,
            EffectRenderOp::Scatter { .. } => 3,
            EffectRenderOp::BrushStrokes { .. } => 5,
            EffectRenderOp::ColorEmboss { .. } => 4,
            EffectRenderOp::Emboss { .. } => 4,
            EffectRenderOp::HueSaturation { .. } => 2,
            EffectRenderOp::UnsharpMask { .. } => 4,
            EffectRenderOp::Lut3D { .. } => 2,
            EffectRenderOp::GaussianBlur { .. } => 4,
            EffectRenderOp::Sharpen { .. } => 4,
            EffectRenderOp::ChromaticAberration { .. } => 4,
            EffectRenderOp::TemporalFrameBlend { .. } => 2,
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
            entries.sort_by_key(|(key, _)| *key);
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
    project_property_initializer: Option<crate::EffectProjectPropertyInitializer>,
    project_property_initializer_retained_bytes: usize,
    evaluator_factory: Option<EffectEvaluatorFactory>,
    execution_contract: EffectExecutionContract,
    extension_contract: Option<EffectExtensionContract>,
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
    /// A contextual definition cannot materialize schemas without its owner.
    #[error("effect `{effect_key}` requires owner project geometry")]
    ProjectGeometryRequired { effect_key: String },
    /// A project-dependent template did not produce a valid concrete schema.
    #[error("effect `{effect_key}` project properties are invalid: {reason}")]
    InvalidProjectProperties { effect_key: String, reason: String },
}

/// Failure to instantiate one author-visible Effect from the live registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectInstantiationError {
    /// The persistent definition identity is not currently registered.
    #[error("effect definition is not registered: {effect_key}")]
    DefinitionUnavailable { effect_key: String },
    /// The definition exists but cannot currently provide visual execution.
    #[error("effect definition is not currently authorable: {effect_key}")]
    DefinitionNotAuthorable { effect_key: String },
    #[error(transparent)]
    Properties(#[from] EffectDefinitionError),
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
            project_property_initializer: None,
            project_property_initializer_retained_bytes: 0,
            evaluator_factory: None,
            execution_contract: EffectExecutionContract::default(),
            extension_contract: None,
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

    /// Add contextual properties before author instantiation and exact schema
    /// admission. Literal base properties must remain unchanged. The byte charge
    /// covers immutable data captured by the initializer, excluding its Arc.
    pub fn with_project_properties(
        mut self,
        initializer: crate::EffectProjectPropertyInitializer,
        retained_bytes: usize,
    ) -> Self {
        self.project_property_initializer = Some(initializer);
        self.project_property_initializer_retained_bytes = retained_bytes;
        self
    }

    pub(crate) fn resolved_properties(
        &self,
        geometry: Option<EffectProjectGeometry>,
    ) -> Result<std::borrow::Cow<'_, PropertyBag>, EffectDefinitionError> {
        let Some(initializer) = &self.project_property_initializer else {
            return Ok(std::borrow::Cow::Borrowed(&self.default_properties));
        };
        let geometry = geometry.ok_or_else(|| EffectDefinitionError::ProjectGeometryRequired {
            effect_key: self.key.clone(),
        })?;
        let invalid = |reason: String| EffectDefinitionError::InvalidProjectProperties {
            effect_key: self.key.clone(),
            reason,
        };
        let mut properties = self.default_properties.clone();
        catch_unwind(AssertUnwindSafe(|| initializer(geometry, &mut properties)))
            .map_err(|_| invalid("initializer panicked".into()))??;
        for (path, property) in self.default_properties.iter() {
            if properties.property(path) != Some(property) {
                return Err(invalid(format!(
                    "initializer changed literal property `{path}`"
                )));
            }
        }
        self.validate_properties(&properties)?;
        Ok(std::borrow::Cow::Owned(properties))
    }

    pub(crate) fn project_geometry_for_instance(
        &self,
        effect: &EffectNode,
        current: Option<EffectProjectGeometry>,
    ) -> Result<Option<EffectProjectGeometry>, String> {
        if self.project_property_initializer.is_none() {
            if crate::project_properties::has_binding(&effect.params) {
                return Err("definition no longer declares project-dependent defaults".into());
            }
            return Ok(None);
        }
        let bound = crate::project_properties::decode_binding(&effect.params)?;
        if current != Some(bound) {
            return Err("effect defaults require the original owner project geometry".into());
        }
        Ok(Some(bound))
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
        self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Self {
        self.with_custom_processor_binding(
            params_builder,
            cache_key_builder,
            cache_policy,
            CustomEffectProcessorBinding::new(processor),
        )
    }

    /// Bind a Float32 processor to the same compiled graph used by preview and export.
    pub fn with_custom_float_render_backend(
        self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectFloatRenderProcessor,
    ) -> Self {
        self.with_custom_processor_binding(
            params_builder,
            cache_key_builder,
            cache_policy,
            CustomEffectProcessorBinding::new_f32(processor),
        )
    }

    /// Bind execution-owned reusable Float32 instances. The factory is
    /// immutable; its instances never belong to the global definition registry.
    pub fn with_custom_float_processor_factory(
        self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        factory: Arc<dyn crate::EffectFloatProcessorFactory>,
    ) -> Self {
        self.with_custom_processor_binding(
            params_builder,
            cache_key_builder,
            cache_policy,
            CustomEffectProcessorBinding {
                revision: NEXT_CUSTOM_PROCESSOR_REVISION.fetch_add(1, Ordering::AcqRel),
                alpha_contract: factory.alpha_contract(),
                input_role: factory.input_role(),
                image_scale_requirement: factory.image_scale_requirement(),
                requires_input_service: factory.requires_input_service(),
                processor: CustomEffectProcessor::Float32Factory(factory),
                runtime_owner: None,
                occurrence_id: None,
                project_geometry: None,
            },
        )
    }

    fn with_custom_processor_binding(
        mut self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectProcessorBinding,
    ) -> Self {
        let effect_key = self.key.clone();
        let role = processor.input_role();
        self.evaluator_factory = Some(EffectEvaluatorFactory::Frame(Arc::new(
            move |effect, context, graph| {
                if let Some(params) = params_builder(effect, context)? {
                    let cache_key =
                        cache_key_builder.as_ref().and_then(|builder| builder(effect, context));
                    let op = EffectRenderOp::Custom {
                        key: effect_key.clone(),
                        params,
                        cache_key,
                        cache_policy,
                        processor: Some(processor.clone()),
                    };
                    match role {
                        crate::EffectFloatProcessorInputRole::SingleImage => {
                            graph.append_unary(op);
                        }
                        crate::EffectFloatProcessorInputRole::NoImage => {
                            graph.append_custom_source(op);
                        }
                    }
                }
                Ok(())
            },
        )));
        self.execution_contract.topology = match role {
            crate::EffectFloatProcessorInputRole::SingleImage => EffectGraphTopology::LinearChain,
            crate::EffectFloatProcessorInputRole::NoImage => EffectGraphTopology::GeneralDag,
        };
        self
    }

    /// Bind the Effect Extension version and availability contract.
    pub fn with_extension_contract(mut self, extension_contract: EffectExtensionContract) -> Self {
        self.extension_contract = Some(extension_contract);
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

    #[cfg(test)]
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
            && effect_extension_is_library_visible(
                self.key(),
                self.definition_registry_revision,
                self.extension_contract(),
            )
    }

    /// Contract bound to this registered effect definition, if any.
    pub fn extension_contract(&self) -> Option<&EffectExtensionContract> {
        self.extension_contract.as_ref()
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
        let extension_bytes = self
            .extension_contract
            .as_ref()
            .map_or(0, |contract| contract.extension_version.capacity());
        std::mem::size_of::<Self>()
            .saturating_add(self.key.capacity())
            .saturating_add(self.display_name.capacity())
            .saturating_add(
                self.category_path.capacity().saturating_mul(std::mem::size_of::<String>()),
            )
            .saturating_add(category_bytes)
            .saturating_add(property_bytes)
            .saturating_add(self.project_property_initializer_retained_bytes)
            .saturating_add(if self.project_property_initializer.is_some() {
                OPAQUE_EFFECT_EVALUATOR_BASE_CHARGE_BYTES
            } else {
                0
            })
            .saturating_add(extension_bytes)
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
        self.validate_properties(&self.default_properties)
    }

    fn validate_properties(&self, properties: &PropertyBag) -> Result<(), EffectDefinitionError> {
        let mut parameter_addresses = HashMap::<ParameterId, String>::new();
        for (address, property) in properties.iter() {
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
                        record_extension_runtime_failure(
                            self.key(),
                            self.definition_registry_revision,
                            self.extension_contract(),
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

fn builtin_effect_types() -> [EffectType; 42] {
    [
        EffectType::BasicCorrection,
        EffectType::WhiteBalance,
        EffectType::Lut3D,
        EffectType::ColorWheel,
        EffectType::HdrGrading,
        EffectType::AscCdl,
        EffectType::Curves,
        EffectType::GamutCompression,
        EffectType::HighlightRecovery,
        EffectType::Qualifier,
        EffectType::HueSaturationLightness,
        EffectType::Crop,
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::Vignette,
        EffectType::ChromaticAberration,
        EffectType::Grain,
        EffectType::ChromaKey,
        EffectType::LumaKey,
        EffectType::Invert,
        EffectType::BoxBlur,
        EffectType::UnsharpMask,
        EffectType::HueSaturation,
        EffectType::BlackAndWhite,
        EffectType::ShiftChannels,
        EffectType::DirectionalBlur,
        EffectType::ChannelMixer,
        EffectType::DropShadow,
        EffectType::FindEdges,
        EffectType::Emboss,
        EffectType::ColorEmboss,
        EffectType::Threshold,
        EffectType::Mosaic,
        EffectType::Posterize,
        EffectType::MotionTile,
        EffectType::Kaleidoscope,
        EffectType::Bevel,
        EffectType::Scatter,
        EffectType::BrushStrokes,
        EffectType::Tint,
        EffectType::Vibrance,
        EffectType::Tritone,
    ]
}

/// Global effect definition registry. Entries are retained until process exit;
/// replacing a definition advances its registry revision. This registry does
/// not implement the future Extension installation or unload lifecycle.
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

/// Process-local revision of the effect definition registry.
///
/// Prepared-program cache identity includes this value so replacing any effect
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

/// Instantiate one Effect author node from the current registered definition.
///
/// Production authoring must use this fallible constructor. It captures the
/// definition's validated Parameter Schema defaults and fails closed when a
/// stale browser action names a missing or currently unavailable definition;
/// it never authors an empty-property placeholder that could later render with
/// different semantics.
pub fn instantiate_effect_node(
    effect_type: EffectType,
) -> Result<EffectNode, EffectInstantiationError> {
    instantiate_effect_node_in_project(effect_type, None)
}

/// Instantiate concrete defaults against the immutable owner project window.
/// Literal definitions keep their original schema and do not capture a binding.
pub fn instantiate_effect_node_with_project_geometry(
    effect_type: EffectType,
    project_geometry: EffectProjectGeometry,
) -> Result<EffectNode, EffectInstantiationError> {
    instantiate_effect_node_in_project(effect_type, Some(project_geometry))
}

fn instantiate_effect_node_in_project(
    effect_type: EffectType,
    project_geometry: Option<EffectProjectGeometry>,
) -> Result<EffectNode, EffectInstantiationError> {
    let effect_key = effect_type.key();
    let definition = effect_definition(&effect_type).ok_or_else(|| {
        EffectInstantiationError::DefinitionUnavailable { effect_key: effect_key.clone() }
    })?;
    if !definition.supports_visual_evaluation() {
        return Err(EffectInstantiationError::DefinitionNotAuthorable { effect_key });
    }
    let mut properties = definition.resolved_properties(project_geometry)?.into_owned();
    let params = if definition.project_property_initializer.is_some() {
        crate::project_properties::encode_binding(project_geometry.ok_or_else(|| {
            EffectDefinitionError::ProjectGeometryRequired { effect_key: effect_key.clone() }
        })?)
        .map_err(|reason| EffectDefinitionError::InvalidProjectProperties {
            effect_key: effect_key.clone(),
            reason,
        })?
    } else {
        serde_json::json!({})
    };
    properties.fork_author_identities();
    Ok(EffectNode {
        id: EffectId::new(),
        effect_type,
        properties,
        params,
        is_enabled: true,
    })
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
        let mut default_properties = effect_definition(&effect_type)
            .map(|definition| {
                assert!(
                    definition.project_property_initializer.is_none(),
                    "contextual definitions require instantiate_effect_node_with_project_geometry"
                );
                definition.default_properties.clone()
            })
            .unwrap_or_default();
        default_properties.fork_author_identities();
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
                "offset",
                "Primaries",
                "Offset",
                PropertyValue::Vec3(glam::Vec3::ZERO),
                Some(-2.0),
                Some(2.0),
                Some(0.001),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "lift",
                "Primaries",
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
                "Primaries",
                "Gamma",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.01),
                Some(4.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "gain",
                "Primaries",
                "Gain",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::HdrGrading => {
            for (parameter, label, value, minimum, maximum, step) in [
                ("global_exposure", "曝光", 0.0, -8.0, 8.0, 0.01),
                ("global_saturation", "饱和度", 1.0, 0.0, 4.0, 0.01),
            ] {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "HDR · Global",
                    label,
                    PropertyValue::Float(value),
                    Some(minimum),
                    Some(maximum),
                    Some(step),
                );
            }
            define_builtin_property(
                &mut properties,
                &effect_type,
                "global_balance",
                "HDR · Global",
                "色彩平衡",
                PropertyValue::Vec3(glam::Vec3::ZERO),
                Some(-1.0),
                Some(1.0),
                Some(0.001),
            );
            for zone in crate::HdrGradingZone::ALL {
                let prefix = zone.parameter_prefix();
                let group = format!("HDR · {}", hdr_zone_display_name(zone));
                for (suffix, label, value, minimum, maximum, step) in [
                    (
                        "center",
                        "中心",
                        zone.default_center_stops(),
                        -16.0,
                        16.0,
                        0.1,
                    ),
                    ("width", "范围", zone.default_width_stops(), 0.25, 12.0, 0.1),
                    ("exposure", "曝光", 0.0, -8.0, 8.0, 0.01),
                    ("saturation", "饱和度", 1.0, 0.0, 4.0, 0.01),
                ] {
                    define_builtin_property(
                        &mut properties,
                        &effect_type,
                        &format!("{prefix}_{suffix}"),
                        &group,
                        label,
                        PropertyValue::Float(value),
                        Some(minimum),
                        Some(maximum),
                        Some(step),
                    );
                }
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    &format!("{prefix}_balance"),
                    &group,
                    "色彩平衡",
                    PropertyValue::Vec3(glam::Vec3::ZERO),
                    Some(-1.0),
                    Some(1.0),
                    Some(0.001),
                );
            }
        }
        EffectType::AscCdl => {
            for (parameter, label, default, min, max) in [
                ("slope", "Slope", glam::Vec3::ONE, 0.0, 4.0),
                ("offset", "Offset", glam::Vec3::ZERO, -2.0, 2.0),
                ("power", "Power", glam::Vec3::ONE, 0.01, 4.0),
            ] {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "ASC CDL",
                    label,
                    PropertyValue::Vec3(default),
                    Some(min),
                    Some(max),
                    Some(0.001),
                );
            }
            define_builtin_property(
                &mut properties,
                &effect_type,
                "saturation",
                "ASC CDL",
                "Saturation",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::Curves => {
            define_builtin_enum_property(
                &mut properties,
                &effect_type,
                "mode",
                "曲线",
                "Master 模式",
                "rgb",
                vec![
                    ParameterEnumOption::new("rgb", "RGB", "mondrian.effect.curves.mode.rgb"),
                    ParameterEnumOption::new("yrgb", "YRGB", "mondrian.effect.curves.mode.yrgb"),
                ],
            );
            for (parameter, label, neutral_delta) in [
                ("master", "Master", false),
                ("red", "Red", false),
                ("green", "Green", false),
                ("blue", "Blue", false),
                ("hue_vs_hue", "Hue vs Hue", true),
                ("hue_vs_saturation", "Hue vs Saturation", true),
                ("hue_vs_luma", "Hue vs Luma", true),
                ("luma_vs_saturation", "Luma vs Saturation", true),
                ("saturation_vs_saturation", "Saturation vs Saturation", true),
                ("saturation_vs_luma", "Saturation vs Luma", true),
            ] {
                define_builtin_curve_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "曲线",
                    label,
                    if neutral_delta {
                        NormalizedCurve::flat(0.5).expect("valid neutral curve")
                    } else {
                        NormalizedCurve::identity()
                    },
                );
            }
        }
        EffectType::GamutCompression => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "色域压缩",
                "强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::HighlightRecovery => {
            for (parameter, label, default, minimum, maximum, step) in [
                ("threshold", "起始阈值", 1.0, 0.0, 16.0, 0.01),
                ("rolloff", "过渡宽度", 1.0, 0.01, 16.0, 0.01),
                ("strength", "恢复强度", 1.0, 0.0, 1.0, 0.01),
            ] {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "高光恢复",
                    label,
                    PropertyValue::Float(default),
                    Some(minimum),
                    Some(maximum),
                    Some(step),
                );
            }
        }
        EffectType::Qualifier => {
            define_builtin_enum_property(
                &mut properties,
                &effect_type,
                "mode",
                "Qualifier",
                "选择模式",
                "hsl",
                vec![
                    ParameterEnumOption::new("hsl", "HSL", "mondrian.effect.qualifier.mode.hsl"),
                    ParameterEnumOption::new(
                        "three_dimensional",
                        "3D",
                        "mondrian.effect.qualifier.mode.three_dimensional",
                    ),
                ],
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "samples",
                "Qualifier · 3D",
                "采样颜色",
                PropertyValue::QualifierSamples(
                    mondrian_core::automation::QualifierSampleSet::green_screen(),
                ),
                None,
                None,
                None,
            );
            for (parameter, label, default, minimum, maximum, step) in [
                ("hue_center", "色相中心", 120.0, 0.0, 360.0, 1.0),
                ("hue_width", "色相宽度", 80.0, 0.0, 360.0, 1.0),
                ("hue_softness", "色相柔化", 20.0, 0.0, 180.0, 1.0),
                ("saturation_low", "饱和度低", 0.2, 0.0, 1.0, 0.01),
                ("saturation_high", "饱和度高", 1.0, 0.0, 1.0, 0.01),
                ("saturation_softness", "饱和度柔化", 0.1, 0.0, 1.0, 0.01),
                ("luminance_low", "亮度低", 0.0, 0.0, 1.0, 0.01),
                ("luminance_high", "亮度高", 1.0, 0.0, 1.0, 0.01),
                ("luminance_softness", "亮度柔化", 0.1, 0.0, 1.0, 0.01),
                ("three_d_tolerance", "3D 容差", 0.12, 0.0, 2.0, 0.01),
                ("three_d_softness", "3D 柔化", 0.08, 0.0, 2.0, 0.01),
                (
                    "blur_radius",
                    "Matte 羽化",
                    0.0,
                    0.0,
                    crate::MAX_QUALIFIER_BLUR_RADIUS as f64,
                    0.1,
                ),
                ("clean_black", "Clean Black", 0.0, 0.0, 0.49, 0.01),
                ("clean_white", "Clean White", 0.0, 0.0, 0.49, 0.01),
            ] {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "Qualifier",
                    label,
                    PropertyValue::Float(default as f32),
                    Some(minimum),
                    Some(maximum),
                    Some(step),
                );
            }
            define_builtin_property(
                &mut properties,
                &effect_type,
                "denoise_radius",
                "Qualifier · Matte",
                "Denoise 半径",
                PropertyValue::Int(0),
                Some(0.0),
                Some(crate::MAX_QUALIFIER_DENOISE_RADIUS as f64),
                Some(1.0),
            );
            for (parameter, label) in [("invert", "反转 Matte"), ("matte_preview", "Matte 预览")]
            {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "Qualifier · Matte",
                    label,
                    PropertyValue::Bool(false),
                    None,
                    None,
                    None,
                );
            }
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
        EffectType::Crop => {
            for (parameter, label) in [
                ("left", "左侧"),
                ("top", "顶部"),
                ("right", "右侧"),
                ("bottom", "底部"),
            ] {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "裁切",
                    label,
                    PropertyValue::Float(0.0),
                    Some(0.0),
                    Some(100.0),
                    Some(0.1),
                );
            }
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
            define_builtin_property(
                &mut properties,
                &effect_type,
                "invert",
                "抠像",
                "保留键色",
                PropertyValue::Bool(false),
                None,
                None,
                None,
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
            define_builtin_property(
                &mut properties,
                &effect_type,
                "invert",
                "亮度键",
                "保留暗部",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
        }
        EffectType::Invert => {
            let channels = crate::InvertChannels::ALL
                .into_iter()
                .map(|scope| {
                    ParameterEnumOption::new(
                        scope.key(),
                        scope.label(),
                        format!("builtin.invert.channels.{}", scope.key()),
                    )
                })
                .collect();
            define_builtin_enum_property(
                &mut properties,
                &effect_type,
                "channels",
                "反转",
                "通道",
                "rgb",
                channels,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend_with_original",
                "反转",
                "与原始图像混合",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(100.0),
                Some(1.0),
            );
        }
        EffectType::BoxBlur => {
            let directions = crate::BoxBlurDirection::ALL
                .into_iter()
                .map(|direction| {
                    ParameterEnumOption::new(
                        direction.key(),
                        direction.label(),
                        format!("builtin.box_blur.direction.{}", direction.key()),
                    )
                })
                .collect();
            define_builtin_property(
                &mut properties,
                &effect_type,
                "radius",
                "方框模糊",
                "模糊半径",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(crate::BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "iterations",
                "方框模糊",
                "迭代",
                PropertyValue::Int(3),
                Some(crate::box_blur::BOX_BLUR_AUTHOR_MIN_ITERATIONS as f64),
                Some(crate::box_blur::BOX_BLUR_AUTHOR_MAX_ITERATIONS as f64),
                Some(1.0),
            );
            define_builtin_enum_property(
                &mut properties,
                &effect_type,
                "direction",
                "方框模糊",
                "模糊方向",
                "horizontal_and_vertical",
                directions,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "repeat_edge_pixels",
                "方框模糊",
                "重复边缘像素",
                PropertyValue::Bool(true),
                None,
                None,
                None,
            );
        }
        EffectType::UnsharpMask => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "钝化蒙版",
                "数量",
                PropertyValue::Float(50.0),
                Some(0.0),
                Some(crate::unsharp_mask::UNSHARP_MASK_AUTHOR_MAX_AMOUNT_PERCENT as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "radius",
                "钝化蒙版",
                "半径",
                PropertyValue::Float(1.0),
                Some(crate::unsharp_mask::UNSHARP_MASK_AUTHOR_MIN_RADIUS_PIXELS as f64),
                Some(crate::unsharp_mask::UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "threshold",
                "钝化蒙版",
                "阈值",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::HueSaturation => {
            define_builtin_enum_property(
                &mut properties,
                &effect_type,
                "band",
                "色相饱和度",
                "通道控制",
                "master",
                crate::HueBand::ALL
                    .into_iter()
                    .map(|band| {
                        ParameterEnumOption::new(
                            band.key(),
                            band.label(),
                            format!("builtin.hue_saturation.band.{}", band.key()),
                        )
                    })
                    .collect(),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "hue",
                "色相饱和度",
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
                "色相饱和度",
                "饱和度",
                PropertyValue::Float(0.0),
                Some(-100.0),
                Some(100.0),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "lightness",
                "色相饱和度",
                "亮度",
                PropertyValue::Float(0.0),
                Some(-100.0),
                Some(100.0),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "colorize",
                "色相饱和度",
                "彩色化",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint_hue",
                "色相饱和度",
                "着色色相",
                PropertyValue::Float(0.0),
                Some(-180.0),
                Some(180.0),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint_saturation",
                "色相饱和度",
                "着色饱和度",
                PropertyValue::Float(25.0),
                Some(0.0),
                Some(100.0),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint_lightness",
                "色相饱和度",
                "着色亮度",
                PropertyValue::Float(0.0),
                Some(-100.0),
                Some(100.0),
                Some(1.0),
            );
        }
        EffectType::FindEdges => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend_with_original",
                "查找边缘",
                "与原始图像混合",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "invert",
                "查找边缘",
                "反转",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
        }
        EffectType::Posterize => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "levels",
                "色调分离",
                "级别",
                PropertyValue::Int(7),
                Some(crate::POSTERIZE_MIN_LEVELS as f64),
                Some(crate::POSTERIZE_MAX_LEVELS as f64),
                Some(1.0),
            );
        }
        EffectType::Mosaic => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "horizontal",
                "马赛克",
                "水平块",
                PropertyValue::Int(10),
                Some(crate::MOSAIC_MIN_BLOCK_PIXELS as f64),
                Some(crate::MOSAIC_MAX_BLOCK_PIXELS as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "vertical",
                "马赛克",
                "垂直块",
                PropertyValue::Int(10),
                Some(crate::MOSAIC_MIN_BLOCK_PIXELS as f64),
                Some(crate::MOSAIC_MAX_BLOCK_PIXELS as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "sharp_colors",
                "马赛克",
                "使用颜色",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
        }
        EffectType::Kaleidoscope => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "center_x",
                "万花筒",
                "Center X",
                PropertyValue::Float(128.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "center_y",
                "万花筒",
                "Center Y",
                PropertyValue::Float(128.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "size",
                "万花筒",
                "尺寸",
                PropertyValue::Float(20.0),
                Some(crate::KALEIDOSCOPE_MIN_SIZE as f64),
                Some(crate::KALEIDOSCOPE_MAX_SIZE as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "mirroring",
                "万花筒",
                "镜像",
                PropertyValue::Int(6),
                Some(crate::KALEIDOSCOPE_MIN_MIRRORING as f64),
                Some(crate::KALEIDOSCOPE_MAX_MIRRORING as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "rotation",
                "万花筒",
                "旋转",
                PropertyValue::Float(0.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "floating_center",
                "万花筒",
                "浮动中心",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
        }
        EffectType::Bevel => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "thickness",
                "斜面与浮雕",
                "边缘厚度",
                PropertyValue::Float(2.0),
                Some(0.0),
                Some(crate::BEVEL_MAX_THICKNESS as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "angle",
                "斜面与浮雕",
                "灯光角度",
                PropertyValue::Float(-60.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "light_color",
                "斜面与浮雕",
                "灯光颜色",
                PropertyValue::Color(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "intensity",
                "斜面与浮雕",
                "灯光强度",
                PropertyValue::Float(0.4),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Scatter => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "散射",
                "散射数量",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(crate::SCATTER_MAX_AMOUNT_PIXELS as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "copies",
                "散射",
                "副本",
                PropertyValue::Int(1),
                Some(1.0),
                Some(crate::SCATTER_MAX_COPIES as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "randomness",
                "散射",
                "散射随机性",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::BrushStrokes => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "angle",
                "画笔描边",
                "描边方向",
                PropertyValue::Float(135.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "size",
                "画笔描边",
                "画笔大小",
                PropertyValue::Float(2.0),
                Some(0.0),
                Some(crate::BRUSH_STROKES_MAX_SIZE as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "length",
                "画笔描边",
                "画笔长度",
                PropertyValue::Float(5.0),
                Some(0.0),
                Some(crate::BRUSH_STROKES_MAX_LENGTH as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "density",
                "画笔描边",
                "画笔密度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(crate::BRUSH_STROKES_MAX_DENSITY as f64),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "randomness",
                "画笔描边",
                "随机性",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(crate::BRUSH_STROKES_MAX_RANDOMNESS as f64),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "surface",
                "画笔描边",
                "画笔表面",
                PropertyValue::Int(1),
                Some(1.0),
                Some(crate::BRUSH_STROKES_SURFACE_COUNT as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend_with_original",
                "画笔描边",
                "与原始图像混合",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::MotionTile => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "center_x",
                "拼贴",
                "拼贴中心 X",
                PropertyValue::Float(128.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "center_y",
                "拼贴",
                "拼贴中心 Y",
                PropertyValue::Float(128.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tile_width",
                "拼贴",
                "拼贴宽度",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(crate::MOTION_TILE_MAX_PERCENT as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tile_height",
                "拼贴",
                "拼贴高度",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(crate::MOTION_TILE_MAX_PERCENT as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "output_width",
                "输出",
                "输出宽度",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(crate::MOTION_TILE_MAX_PERCENT as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "output_height",
                "输出",
                "输出高度",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(crate::MOTION_TILE_MAX_PERCENT as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "mirror_edges",
                "拼贴",
                "镜像边缘",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "phase",
                "拼贴",
                "相位",
                PropertyValue::Float(0.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "horizontal_phase",
                "拼贴",
                "水平位移",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Threshold => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "level",
                "阈值",
                "阈值",
                PropertyValue::Float(0.5),
                Some(crate::THRESHOLD_MIN_LEVEL as f64),
                Some(crate::THRESHOLD_MAX_LEVEL as f64),
                Some(0.01),
            );
        }
        EffectType::Tint => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "map_black",
                "色调",
                "将黑色映射到",
                PropertyValue::Color(Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "map_white",
                "色调",
                "将白色映射到",
                PropertyValue::Color(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "色调",
                "着色数量",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(100.0),
                Some(0.1),
            );
        }
        EffectType::Vibrance => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "vibrance",
                "自然饱和度",
                "自然饱和度",
                PropertyValue::Float(0.0),
                Some(-100.0),
                Some(100.0),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "saturation",
                "自然饱和度",
                "饱和度",
                PropertyValue::Float(0.0),
                Some(-100.0),
                Some(100.0),
                Some(0.1),
            );
        }
        EffectType::Tritone => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "highlights",
                "三色调",
                "高光",
                PropertyValue::Color(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "midtones",
                "三色调",
                "中间调",
                PropertyValue::Color(Color { r: 0.5, g: 0.39, b: 0.27, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "shadows",
                "三色调",
                "阴影",
                PropertyValue::Color(Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend_with_original",
                "三色调",
                "与原始图像混合",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(100.0),
                Some(0.1),
            );
        }
        EffectType::ColorEmboss => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "direction",
                "色彩浮雕",
                "方向",
                PropertyValue::Float(45.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "relief",
                "色彩浮雕",
                "幅度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(crate::COLOR_EMBOSS_MAX_RELIEF as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "contrast",
                "色彩浮雕",
                "对比度",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(crate::COLOR_EMBOSS_MAX_CONTRAST_PERCENT as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend_with_original",
                "色彩浮雕",
                "与原图像混合",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(100.0),
                Some(1.0),
            );
        }
        EffectType::Emboss => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "direction",
                "浮雕",
                "方向",
                PropertyValue::Float(45.0),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "relief",
                "浮雕",
                "凸出",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(crate::EMBOSS_MAX_RELIEF as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "contrast",
                "浮雕",
                "对比度",
                PropertyValue::Float(100.0),
                Some(0.0),
                Some(crate::EMBOSS_MAX_CONTRAST_PERCENT as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend_with_original",
                "浮雕",
                "与原始图像混合",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::DropShadow => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "color",
                "投影",
                "阴影颜色",
                PropertyValue::Color(mondrian_core::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "opacity",
                "投影",
                "不透明度",
                PropertyValue::Float(127.5),
                Some(0.0),
                Some(crate::DROP_SHADOW_AUTHOR_MAX_OPACITY as f64),
                Some(0.5),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "direction",
                "投影",
                "方向",
                PropertyValue::Float(135.0),
                None,
                None,
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "distance",
                "投影",
                "距离",
                PropertyValue::Float(5.0),
                Some(0.0),
                Some(crate::DROP_SHADOW_AUTHOR_MAX_DISTANCE_PIXELS as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "softness",
                "投影",
                "柔和度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(crate::DROP_SHADOW_AUTHOR_MAX_SOFTNESS_PIXELS as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "shadow_only",
                "投影",
                "仅阴影",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
        }
        EffectType::ChannelMixer => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "red_red",
                "通道混合器",
                "红 - 红",
                PropertyValue::Float(100.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "red_green",
                "通道混合器",
                "红 - 绿",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "red_blue",
                "通道混合器",
                "红 - 蓝",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "red_constant",
                "通道混合器",
                "红 - 常量",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "green_red",
                "通道混合器",
                "绿 - 红",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "green_green",
                "通道混合器",
                "绿 - 绿",
                PropertyValue::Float(100.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "green_blue",
                "通道混合器",
                "绿 - 蓝",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "green_constant",
                "通道混合器",
                "绿 - 常量",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blue_red",
                "通道混合器",
                "蓝 - 红",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blue_green",
                "通道混合器",
                "蓝 - 绿",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blue_blue",
                "通道混合器",
                "蓝 - 蓝",
                PropertyValue::Float(100.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blue_constant",
                "通道混合器",
                "蓝 - 常量",
                PropertyValue::Float(0.0),
                Some(crate::CHANNEL_MIXER_WEIGHT_MIN as f64),
                Some(crate::CHANNEL_MIXER_WEIGHT_MAX as f64),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "monochrome",
                "通道混合器",
                "单色",
                PropertyValue::Bool(false),
                None,
                None,
                None,
            );
        }
        EffectType::DirectionalBlur => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "length",
                "方向模糊",
                "模糊长度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(crate::DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS as f64),
                Some(0.1),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "direction",
                "方向模糊",
                "方向",
                PropertyValue::Float(0.0),
                None,
                None,
                None,
            );
        }
        EffectType::ShiftChannels => {
            for (parameter, label, default) in [
                ("red", "红色取自", "red"),
                ("green", "绿色取自", "green"),
                ("blue", "蓝色取自", "blue"),
                ("alpha", "Alpha 取自", "alpha"),
            ] {
                define_builtin_enum_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "转换通道",
                    label,
                    default,
                    crate::ChannelSource::ALL
                        .into_iter()
                        .map(|source| {
                            ParameterEnumOption::new(
                                source.key(),
                                source.label(),
                                format!("builtin.shift_channels.{parameter}.{}", source.key()),
                            )
                        })
                        .collect(),
                );
            }
        }
        EffectType::BlackAndWhite => {
            for (parameter, label, default) in [
                ("red", "红色", 40.0),
                ("yellow", "黄色", 60.0),
                ("green", "绿色", 40.0),
                ("cyan", "青色", 60.0),
                ("blue", "蓝色", 20.0),
                ("magenta", "洋红", 80.0),
            ] {
                define_builtin_property(
                    &mut properties,
                    &effect_type,
                    parameter,
                    "黑白",
                    label,
                    PropertyValue::Float(default),
                    Some(crate::BLACK_WHITE_WEIGHT_MIN as f64),
                    Some(crate::BLACK_WHITE_WEIGHT_MAX as f64),
                    Some(1.0),
                );
            }
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint_amount",
                "黑白",
                "淡色",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint_color",
                "黑白",
                "色调颜色",
                PropertyValue::Color(Color { r: 0.88, g: 0.83, b: 0.70, a: 1.0 }),
                None,
                None,
                None,
            );
        }
        EffectType::External(_) => {}
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
        descriptor = descriptor
            .with_cache_impact(ParameterCacheImpact::Resource)
            .with_resource_contract(mondrian_core::ParameterResourceContract::external_file(
                vec!["cube".to_owned()],
            ));
    }
    if matches!(effect_type, EffectType::Qualifier) && parameter == "matte_preview" {
        descriptor = descriptor.with_cache_impact(ParameterCacheImpact::Topology);
    }
    descriptor.ui_metadata = AnimatablePropertyUiMetadata {
        group_name: Some(group.to_string()),
        supports_spatial: false,
        display_order: Some(properties.iter().count() as u32),
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
        display_order: Some(properties.iter().count() as u32),
    };
    properties.define(descriptor);
}

fn define_builtin_curve_property(
    properties: &mut PropertyBag,
    effect_type: &EffectType,
    parameter: &str,
    group: &str,
    name: &str,
    default_curve: NormalizedCurve,
) {
    let path = effect_type.property_path(parameter);
    let mut descriptor = PropertyDescriptor::new(path, name, PropertyValue::Curve(default_curve))
        .with_parameter_id(builtin_parameter_id(effect_type, parameter))
        .with_animatable(false);
    descriptor.ui_metadata = AnimatablePropertyUiMetadata {
        group_name: Some(group.to_owned()),
        supports_spatial: false,
        display_order: Some(properties.iter().count() as u32),
    };
    properties.define(descriptor);
}

fn lut_processing_space_options() -> Vec<ParameterEnumOption> {
    [
        ("unassigned", "Unassigned"),
        ("scene_linear", "Scene linear"),
        ("rec709", "Rec. 709"),
        ("srgb", "sRGB"),
        ("rec2020", "Rec. 2020"),
        ("display_p3", "Display P3"),
        ("rec2100_hlg", "Rec. 2100 HLG"),
        ("rec2100_pq", "Rec. 2100 PQ"),
        ("aces_cct", "ACEScct"),
        ("apple_log_bt2020", "Apple Log / BT.2020"),
        ("sony_slog2_sgamut", "Sony S-Log2 / S-Gamut"),
        ("sony_slog3_sgamut3", "Sony S-Log3 / S-Gamut3"),
        ("sony_slog3_sgamut3_cine", "Sony S-Log3 / S-Gamut3.Cine"),
        ("arri_logc3_wide_gamut3", "ARRI LogC3 / Wide Gamut 3"),
        ("arri_logc4_wide_gamut4", "ARRI LogC4 / Wide Gamut 4"),
        (
            "canon_log2_cinema_gamut_d55",
            "Canon Log 2 / Cinema Gamut D55",
        ),
        (
            "canon_log3_cinema_gamut_d55",
            "Canon Log 3 / Cinema Gamut D55",
        ),
        ("panasonic_vlog_vgamut", "Panasonic V-Log / V-Gamut"),
        ("red_log3g10_wide_gamut_rgb", "RED Log3G10 / Wide Gamut RGB"),
        (
            "blackmagic_film_wide_gamut_gen5",
            "Blackmagic Film / Wide Gamut Gen 5",
        ),
        ("dji_dlog_dgamut", "DJI D-Log / D-Gamut"),
        (
            "davinci_intermediate_wide_gamut",
            "DaVinci Intermediate / Wide Gamut",
        ),
    ]
    .into_iter()
    .map(|(key, caption)| {
        ParameterEnumOption::new(
            key,
            caption,
            format!("builtin.lut_3d.processing_space.{key}"),
        )
    })
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
        (EffectType::HdrGrading, parameter)
            if parameter == "global_exposure"
                || parameter.ends_with("_center")
                || parameter.ends_with("_width")
                || parameter.ends_with("_exposure") =>
        {
            ParameterUnit::Stops
        }
        (EffectType::HueSaturationLightness, "hue") => ParameterUnit::Degrees,
        (EffectType::Qualifier, "hue_center" | "hue_width" | "hue_softness") => {
            ParameterUnit::Degrees
        }
        (EffectType::Qualifier, "blur_radius" | "denoise_radius") => ParameterUnit::Pixels,
        (EffectType::Crop, "left" | "top" | "right" | "bottom") => ParameterUnit::Percent,
        (EffectType::GaussianBlur, "radius") => ParameterUnit::Pixels,
        (EffectType::Invert, "blend_with_original") => ParameterUnit::Percent,
        (EffectType::BoxBlur, "radius") => ParameterUnit::Pixels,
        (EffectType::DirectionalBlur, "length") => ParameterUnit::Pixels,
        (EffectType::DropShadow, "direction") => ParameterUnit::Degrees,
        (EffectType::DropShadow, "distance" | "softness") => ParameterUnit::Pixels,
        (EffectType::ChannelMixer, "red_red" | "red_green" | "red_blue" | "red_constant")
        | (
            EffectType::ChannelMixer,
            "green_red" | "green_green" | "green_blue" | "green_constant",
        )
        | (EffectType::ChannelMixer, "blue_red" | "blue_green" | "blue_blue" | "blue_constant") => {
            ParameterUnit::Percent
        }
        (EffectType::DirectionalBlur, "direction") => ParameterUnit::Degrees,
        (EffectType::UnsharpMask, "amount") => ParameterUnit::Percent,
        (EffectType::UnsharpMask, "radius") => ParameterUnit::Pixels,
        (EffectType::HueSaturation, "hue") => ParameterUnit::Degrees,
        (EffectType::HueSaturation, "saturation") => ParameterUnit::Percent,
        (EffectType::HueSaturation, "lightness") => ParameterUnit::Percent,
        (EffectType::HueSaturation, "tint_hue") => ParameterUnit::Degrees,
        (EffectType::HueSaturation, "tint_saturation") => ParameterUnit::Percent,
        (EffectType::HueSaturation, "tint_lightness") => ParameterUnit::Percent,
        (EffectType::Kaleidoscope, "center_x") => ParameterUnit::Pixels,
        (EffectType::Kaleidoscope, "center_y") => ParameterUnit::Pixels,
        (EffectType::Kaleidoscope, "size") => ParameterUnit::Percent,
        (EffectType::Kaleidoscope, "rotation") => ParameterUnit::Degrees,
        (EffectType::Scatter, "amount") => ParameterUnit::Pixels,
        (EffectType::BrushStrokes, "size") | (EffectType::BrushStrokes, "length") => {
            ParameterUnit::Pixels
        }
        (EffectType::BrushStrokes, "angle") => ParameterUnit::Degrees,
        (EffectType::BrushStrokes, "blend_with_original") => ParameterUnit::Normalized,
        (EffectType::Tint, "amount") => ParameterUnit::Percent,
        (EffectType::Vibrance, "vibrance") => ParameterUnit::Percent,
        (EffectType::Vibrance, "saturation") => ParameterUnit::Percent,
        (EffectType::Tritone, "blend_with_original") => ParameterUnit::Percent,
        (EffectType::Bevel, "thickness") => ParameterUnit::Pixels,
        (EffectType::Bevel, "angle") => ParameterUnit::Degrees,
        (EffectType::Bevel, "intensity") => ParameterUnit::Normalized,
        (EffectType::MotionTile, "center_x") => ParameterUnit::Pixels,
        (EffectType::MotionTile, "center_y") => ParameterUnit::Pixels,
        (EffectType::MotionTile, "tile_width") => ParameterUnit::Percent,
        (EffectType::MotionTile, "tile_height") => ParameterUnit::Percent,
        (EffectType::MotionTile, "output_width") => ParameterUnit::Percent,
        (EffectType::MotionTile, "output_height") => ParameterUnit::Percent,
        (EffectType::MotionTile, "phase") => ParameterUnit::Pixels,
        (EffectType::MotionTile, "horizontal_phase") => ParameterUnit::Normalized,
        (EffectType::BlackAndWhite, "red")
        | (EffectType::BlackAndWhite, "yellow")
        | (EffectType::BlackAndWhite, "green")
        | (EffectType::BlackAndWhite, "cyan")
        | (EffectType::BlackAndWhite, "blue")
        | (EffectType::BlackAndWhite, "magenta") => ParameterUnit::Percent,
        (EffectType::BlackAndWhite, "tint_amount") => ParameterUnit::Normalized,
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

fn builtin_effect_definition(effect_type: EffectType) -> EffectDefinition {
    let category = effect_type.category_path().into_iter().map(str::to_owned).collect();
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
        if matches!(
            effect_type,
            EffectType::Qualifier | EffectType::LumaKey | EffectType::ChromaKey
        ) {
            definition.with_branching_graph_builder(graph_builder)
        } else {
            definition.with_graph_builder(graph_builder)
        }
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
        EffectType::BasicCorrection
        | EffectType::WhiteBalance
        | EffectType::ColorWheel
        | EffectType::HdrGrading
        | EffectType::AscCdl
        | EffectType::Curves
        | EffectType::GamutCompression
        | EffectType::HighlightRecovery
        | EffectType::Qualifier
        | EffectType::Vignette
        | EffectType::Crop => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            roi_propagation: if matches!(effect_type, EffectType::Qualifier) {
                EffectRoiPropagation::Expand {
                    horizontal_pixels: crate::MAX_QUALIFIER_DENOISE_RADIUS
                        + (crate::MAX_QUALIFIER_BLUR_RADIUS * 3.0) as u32,
                    vertical_pixels: crate::MAX_QUALIFIER_DENOISE_RADIUS
                        + (crate::MAX_QUALIFIER_BLUR_RADIUS * 3.0) as u32,
                }
            } else {
                cpu_linear.roi_propagation
            },
            topology: if matches!(effect_type, EffectType::Qualifier) {
                EffectGraphTopology::GeneralDag
            } else {
                cpu_linear.topology
            },
            ..cpu_linear
        },
        EffectType::Grain => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            determinism: EffectDeterminism::FrameSeeded,
            ..cpu_linear
        },
        EffectType::Lut3D => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
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
        EffectType::LumaKey => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: 0,
                vertical_pixels: 0,
            },
            topology: EffectGraphTopology::GeneralDag,
            ..cpu_linear
        },
        EffectType::ChromaKey => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            topology: EffectGraphTopology::GeneralDag,
            ..cpu_linear
        },
        EffectType::External(_) => EffectExecutionContract {
            execution_modes: EffectExecutionModes::NONE,
            ..cpu_linear
        },
        EffectType::HueSaturationLightness => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            ..cpu_linear
        },
        EffectType::Invert => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::BoxBlur => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            // The authored radius can exceed any bounded expansion, so the
            // definition-level envelope is a full-frame input. The graph
            // planner still narrows it per instance from the authored radius.
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::FindEdges => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: crate::FIND_EDGES_HALO_PIXELS,
                vertical_pixels: crate::FIND_EDGES_HALO_PIXELS,
            },
            ..cpu_linear
        },
        EffectType::Posterize => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::Tint => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::Vibrance => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::Tritone => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::Mosaic => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            ..cpu_linear
        },
        EffectType::Kaleidoscope => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::Scatter => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::BrushStrokes => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::Bevel => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: crate::BEVEL_HALO_PIXELS,
                vertical_pixels: crate::BEVEL_HALO_PIXELS,
            },
            ..cpu_linear
        },
        EffectType::MotionTile => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::Threshold => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::ColorEmboss => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: crate::COLOR_EMBOSS_HALO_PIXELS,
                vertical_pixels: crate::COLOR_EMBOSS_HALO_PIXELS,
            },
            ..cpu_linear
        },
        EffectType::Emboss => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: crate::EMBOSS_HALO_PIXELS,
                vertical_pixels: crate::EMBOSS_HALO_PIXELS,
            },
            ..cpu_linear
        },
        EffectType::DropShadow => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::ChannelMixer => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::DirectionalBlur => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            // The authored length can exceed any bounded expansion, so the
            // definition-level envelope is a full-frame input. The graph planner
            // still narrows it per instance from the authored length.
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..cpu_linear
        },
        EffectType::ShiftChannels => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::BlackAndWhite => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::HueSaturation => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
            ..cpu_linear
        },
        EffectType::UnsharpMask => EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            // Same bounded-expansion envelope as the Gaussian blur it compares
            // against; the radius parameter is capped at 500 pixels.
            roi_propagation: finite_kernel_roi_contract(
                crate::unsharp_mask::UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS,
            ),
            ..cpu_linear
        },
    }
}

fn finite_kernel_roi_contract(radius: f32) -> EffectRoiPropagation {
    let Some(halo) = crate::adjustment::gaussian_blur_input_halo(radius) else {
        return EffectRoiPropagation::FullFrame;
    };
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
        let path = path.into_native_path().map_err(|error| {
            EffectGraphBuildError::ResourceUnavailable {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                parameter_id: path_id.clone(),
                recovery: EffectResourceRecovery::AuthorEdit,
                reason: error.to_string(),
            }
        })?;
        if !path.is_absolute() {
            return Err(EffectGraphBuildError::ResourceUnavailable {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                parameter_id: path_id.clone(),
                recovery: EffectResourceRecovery::AuthorEdit,
                reason: "LUT resource has no absolute Project directory owner".into(),
            });
        }
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
        EffectType::WhiteBalance => {
            let temperature_id = builtin_parameter_id(effect_type, "temperature");
            let tint_id = builtin_parameter_id(effect_type, "tint");
            Some(Arc::new(move |effect, context, graph| {
                let temperature = effect.evaluate_f32_parameter(&temperature_id, context.time, 0.0);
                let tint = effect.evaluate_f32_parameter(&tint_id, context.time, 0.0);
                let grade =
                    crate::WhiteBalanceGrade::new(temperature, tint, context.working_color_space)
                        .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: error.to_string(),
                    })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::WhiteBalance { grade });
                }
                Ok(())
            }))
        }
        EffectType::ColorWheel => {
            let offset_id = builtin_parameter_id(effect_type, "offset");
            let lift_id = builtin_parameter_id(effect_type, "lift");
            let gamma_id = builtin_parameter_id(effect_type, "gamma");
            let gain_id = builtin_parameter_id(effect_type, "gain");
            Some(Arc::new(move |effect, context, graph| {
                let offset = effect
                    .evaluate_vec3_parameter(&offset_id, context.time, glam::Vec3::ZERO)
                    .to_array();
                let lift = effect
                    .evaluate_vec3_parameter(&lift_id, context.time, glam::Vec3::ONE)
                    .to_array();
                let gamma = effect
                    .evaluate_vec3_parameter(&gamma_id, context.time, glam::Vec3::ONE)
                    .to_array();
                let gain = effect
                    .evaluate_vec3_parameter(&gain_id, context.time, glam::Vec3::ONE)
                    .to_array();
                let grade =
                    crate::PrimariesGrade::new(offset, lift, gamma, gain).map_err(|error| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: error.to_string(),
                        }
                    })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Primaries { grade });
                }
                Ok(())
            }))
        }
        EffectType::HdrGrading => {
            let global_exposure_id = builtin_parameter_id(effect_type, "global_exposure");
            let global_saturation_id = builtin_parameter_id(effect_type, "global_saturation");
            let global_balance_id = builtin_parameter_id(effect_type, "global_balance");
            let zone_ids = crate::HdrGradingZone::ALL.map(|zone| {
                let prefix = zone.parameter_prefix();
                [
                    builtin_parameter_id(effect_type, &format!("{prefix}_center")),
                    builtin_parameter_id(effect_type, &format!("{prefix}_width")),
                    builtin_parameter_id(effect_type, &format!("{prefix}_exposure")),
                    builtin_parameter_id(effect_type, &format!("{prefix}_saturation")),
                    builtin_parameter_id(effect_type, &format!("{prefix}_balance")),
                ]
            });
            Some(Arc::new(move |effect, context, graph| {
                let zones = std::array::from_fn(|index| {
                    let zone = crate::HdrGradingZone::ALL[index];
                    let ids = &zone_ids[index];
                    crate::HdrZoneControl {
                        center_stops: effect.evaluate_f32_parameter(
                            &ids[0],
                            context.time,
                            zone.default_center_stops(),
                        ),
                        width_stops: effect.evaluate_f32_parameter(
                            &ids[1],
                            context.time,
                            zone.default_width_stops(),
                        ),
                        exposure_stops: effect.evaluate_f32_parameter(&ids[2], context.time, 0.0),
                        saturation: effect.evaluate_f32_parameter(&ids[3], context.time, 1.0),
                        balance: effect
                            .evaluate_vec3_parameter(&ids[4], context.time, glam::Vec3::ZERO)
                            .to_array(),
                    }
                });
                let authoring = crate::HdrGradingAuthoring {
                    global_exposure_stops: effect.evaluate_f32_parameter(
                        &global_exposure_id,
                        context.time,
                        0.0,
                    ),
                    global_saturation: effect.evaluate_f32_parameter(
                        &global_saturation_id,
                        context.time,
                        1.0,
                    ),
                    global_balance: effect
                        .evaluate_vec3_parameter(&global_balance_id, context.time, glam::Vec3::ZERO)
                        .to_array(),
                    zones,
                };
                let grade = Arc::new(
                    crate::PreparedHdrGrading::new(authoring, context.working_color_space)
                        .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: error.to_string(),
                        })?,
                );
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::HdrGrading { grade });
                }
                Ok(())
            }))
        }
        EffectType::AscCdl => {
            let slope_id = builtin_parameter_id(effect_type, "slope");
            let offset_id = builtin_parameter_id(effect_type, "offset");
            let power_id = builtin_parameter_id(effect_type, "power");
            let saturation_id = builtin_parameter_id(effect_type, "saturation");
            Some(Arc::new(move |effect, context, graph| {
                let slope = effect
                    .evaluate_vec3_parameter(&slope_id, context.time, glam::Vec3::ONE)
                    .to_array();
                let offset = effect
                    .evaluate_vec3_parameter(&offset_id, context.time, glam::Vec3::ZERO)
                    .to_array();
                let power = effect
                    .evaluate_vec3_parameter(&power_id, context.time, glam::Vec3::ONE)
                    .to_array();
                let saturation = effect.evaluate_f32_parameter(&saturation_id, context.time, 1.0);
                let grade =
                    crate::AscCdlGrade::new(slope, offset, power, saturation).map_err(|error| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: error.to_string(),
                        }
                    })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::AscCdl { grade });
                }
                Ok(())
            }))
        }
        EffectType::Curves => {
            let mode_id = builtin_parameter_id(effect_type, "mode");
            let ids = [
                "master",
                "red",
                "green",
                "blue",
                "hue_vs_hue",
                "hue_vs_saturation",
                "hue_vs_luma",
                "luma_vs_saturation",
                "saturation_vs_saturation",
                "saturation_vs_luma",
            ]
            .map(|parameter| builtin_parameter_id(effect_type, parameter));
            Some(Arc::new(move |effect, context, graph| {
                let identity = NormalizedCurve::identity();
                let neutral = NormalizedCurve::flat(0.5).map_err(|error| {
                    EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: error.to_string(),
                    }
                })?;
                let values = std::array::from_fn::<_, 10, _>(|index| {
                    effect.evaluate_curve_parameter(&ids[index], context.time).unwrap_or_else(
                        || {
                            if index < 4 {
                                identity.clone()
                            } else {
                                neutral.clone()
                            }
                        },
                    )
                });
                let mode = match effect.evaluate_enum_parameter(&mode_id, context.time).as_deref() {
                    Some("yrgb") => crate::ColorCurvesMode::YRgb,
                    _ => crate::ColorCurvesMode::Rgb,
                };
                let curves = Arc::new(crate::PreparedColorCurves::new(
                    crate::ColorCurvesAuthoring {
                        mode,
                        master: &values[0],
                        red: &values[1],
                        green: &values[2],
                        blue: &values[3],
                        hue_vs_hue: &values[4],
                        hue_vs_saturation: &values[5],
                        hue_vs_luma: &values[6],
                        luma_vs_saturation: &values[7],
                        saturation_vs_saturation: &values[8],
                        saturation_vs_luma: &values[9],
                    },
                    context.working_color_space,
                ));
                if !curves.is_identity() {
                    graph.append_unary(EffectRenderOp::ColorCurves { curves });
                }
                Ok(())
            }))
        }
        EffectType::GamutCompression => {
            let amount_id = builtin_parameter_id(effect_type, "amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 1.0);
                let grade = crate::GamutCompressionGrade::new(amount, context.working_color_space)
                    .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: error.to_string(),
                    })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::GamutCompression { grade });
                }
                Ok(())
            }))
        }
        EffectType::HighlightRecovery => {
            let threshold_id = builtin_parameter_id(effect_type, "threshold");
            let rolloff_id = builtin_parameter_id(effect_type, "rolloff");
            let strength_id = builtin_parameter_id(effect_type, "strength");
            Some(Arc::new(move |effect, context, graph| {
                let threshold = effect.evaluate_f32_parameter(&threshold_id, context.time, 1.0);
                let rolloff = effect.evaluate_f32_parameter(&rolloff_id, context.time, 1.0);
                let strength = effect.evaluate_f32_parameter(&strength_id, context.time, 1.0);
                let grade = crate::HighlightRecoveryGrade::new(
                    threshold,
                    rolloff,
                    strength,
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::HighlightRecovery { grade });
                }
                Ok(())
            }))
        }
        EffectType::Qualifier => {
            let mode_id = builtin_parameter_id(effect_type, "mode");
            let samples_id = builtin_parameter_id(effect_type, "samples");
            let hue_center_id = builtin_parameter_id(effect_type, "hue_center");
            let hue_width_id = builtin_parameter_id(effect_type, "hue_width");
            let hue_softness_id = builtin_parameter_id(effect_type, "hue_softness");
            let saturation_low_id = builtin_parameter_id(effect_type, "saturation_low");
            let saturation_high_id = builtin_parameter_id(effect_type, "saturation_high");
            let saturation_softness_id = builtin_parameter_id(effect_type, "saturation_softness");
            let luminance_low_id = builtin_parameter_id(effect_type, "luminance_low");
            let luminance_high_id = builtin_parameter_id(effect_type, "luminance_high");
            let luminance_softness_id = builtin_parameter_id(effect_type, "luminance_softness");
            let tolerance_id = builtin_parameter_id(effect_type, "three_d_tolerance");
            let three_d_softness_id = builtin_parameter_id(effect_type, "three_d_softness");
            let denoise_radius_id = builtin_parameter_id(effect_type, "denoise_radius");
            let blur_radius_id = builtin_parameter_id(effect_type, "blur_radius");
            let clean_black_id = builtin_parameter_id(effect_type, "clean_black");
            let clean_white_id = builtin_parameter_id(effect_type, "clean_white");
            let invert_id = builtin_parameter_id(effect_type, "invert");
            let matte_preview_id = builtin_parameter_id(effect_type, "matte_preview");
            Some(Arc::new(move |effect, context, graph| {
                let samples = effect
                    .evaluate_qualifier_samples_parameter(&samples_id, context.time)
                    .unwrap_or_else(mondrian_core::automation::QualifierSampleSet::green_screen);
                let mode = match effect.evaluate_enum_parameter(&mode_id, context.time).as_deref() {
                    Some("three_dimensional") => crate::QualifierMode::ThreeDimensional,
                    _ => crate::QualifierMode::Hsl,
                };
                let denoise_radius =
                    match effect.evaluate_parameter(&denoise_radius_id, context.time) {
                        Some(PropertyValue::Int(value)) => {
                            value.clamp(0, i64::from(crate::MAX_QUALIFIER_DENOISE_RADIUS)) as u32
                        }
                        _ => 0,
                    };
                let prepared = crate::PreparedQualifier::new(
                    crate::QualifierAuthoring {
                        mode,
                        hue_center_degrees: effect.evaluate_f32_parameter(
                            &hue_center_id,
                            context.time,
                            120.0,
                        ),
                        hue_width_degrees: effect.evaluate_f32_parameter(
                            &hue_width_id,
                            context.time,
                            80.0,
                        ),
                        hue_softness_degrees: effect.evaluate_f32_parameter(
                            &hue_softness_id,
                            context.time,
                            20.0,
                        ),
                        saturation_low: effect.evaluate_f32_parameter(
                            &saturation_low_id,
                            context.time,
                            0.2,
                        ),
                        saturation_high: effect.evaluate_f32_parameter(
                            &saturation_high_id,
                            context.time,
                            1.0,
                        ),
                        saturation_softness: effect.evaluate_f32_parameter(
                            &saturation_softness_id,
                            context.time,
                            0.1,
                        ),
                        luminance_low: effect.evaluate_f32_parameter(
                            &luminance_low_id,
                            context.time,
                            0.0,
                        ),
                        luminance_high: effect.evaluate_f32_parameter(
                            &luminance_high_id,
                            context.time,
                            1.0,
                        ),
                        luminance_softness: effect.evaluate_f32_parameter(
                            &luminance_softness_id,
                            context.time,
                            0.1,
                        ),
                        samples: (mode == crate::QualifierMode::ThreeDimensional)
                            .then_some(&samples),
                        three_d_tolerance: effect.evaluate_f32_parameter(
                            &tolerance_id,
                            context.time,
                            0.12,
                        ),
                        three_d_softness: effect.evaluate_f32_parameter(
                            &three_d_softness_id,
                            context.time,
                            0.08,
                        ),
                        denoise_radius,
                        blur_radius: effect.evaluate_f32_parameter(
                            &blur_radius_id,
                            context.time,
                            0.0,
                        ),
                        clean_black: effect.evaluate_f32_parameter(
                            &clean_black_id,
                            context.time,
                            0.0,
                        ),
                        clean_white: effect.evaluate_f32_parameter(
                            &clean_white_id,
                            context.time,
                            0.0,
                        ),
                    },
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                let invert = matches!(
                    effect.evaluate_parameter(&invert_id, context.time),
                    Some(PropertyValue::Bool(true))
                );
                let matte_preview = matches!(
                    effect.evaluate_parameter(&matte_preview_id, context.time),
                    Some(PropertyValue::Bool(true))
                );
                let source = graph.current_output();
                let matte = graph.append_unary_in_domain(
                    EffectRenderOp::Qualifier { qualifier: Arc::new(prepared) },
                    EffectColorDomainContract {
                        input: EffectColorDomain::SceneLinearRgb,
                        output: EffectColorDomain::AlphaMask,
                    },
                );
                let output = if matte_preview {
                    graph.append_unary_in_domain(
                        EffectRenderOp::MattePreview { invert },
                        EffectColorDomainContract {
                            input: EffectColorDomain::AlphaMask,
                            output: EffectColorDomain::SceneLinearRgb,
                        },
                    )
                } else {
                    graph.add_mask(source, matte, invert, crate::MaskOp::Add)
                };
                graph.set_current_output(output);
                Ok(())
            }))
        }
        EffectType::LumaKey => {
            let threshold_id = builtin_parameter_id(effect_type, "threshold");
            let softness_id = builtin_parameter_id(effect_type, "softness");
            let invert_id = builtin_parameter_id(effect_type, "invert");
            Some(Arc::new(move |effect, context, graph| {
                let threshold = effect.evaluate_f32_parameter(&threshold_id, context.time, 0.5);
                let softness = effect.evaluate_f32_parameter(&softness_id, context.time, 0.1);
                let qualifier = crate::PreparedQualifier::new(
                    crate::QualifierAuthoring {
                        mode: crate::QualifierMode::Hsl,
                        hue_center_degrees: 0.0,
                        hue_width_degrees: 360.0,
                        hue_softness_degrees: 0.0,
                        saturation_low: 0.0,
                        saturation_high: 1.0,
                        saturation_softness: 0.0,
                        luminance_low: threshold,
                        luminance_high: 1.0,
                        luminance_softness: softness,
                        samples: None,
                        three_d_tolerance: 0.0,
                        three_d_softness: 0.0,
                        denoise_radius: 0,
                        blur_radius: 0.0,
                        clean_black: 0.0,
                        clean_white: 0.0,
                    },
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                let invert = matches!(
                    effect.evaluate_parameter(&invert_id, context.time),
                    Some(PropertyValue::Bool(true))
                );
                let source = graph.current_output();
                let matte = graph.append_unary_in_domain(
                    EffectRenderOp::Qualifier { qualifier: Arc::new(qualifier) },
                    EffectColorDomainContract {
                        input: EffectColorDomain::SceneLinearRgb,
                        output: EffectColorDomain::AlphaMask,
                    },
                );
                let output = graph.add_mask(source, matte, invert, crate::MaskOp::Add);
                graph.set_current_output(output);
                Ok(())
            }))
        }
        EffectType::ChromaKey => {
            let color_id = builtin_parameter_id(effect_type, "key_color");
            let similarity_id = builtin_parameter_id(effect_type, "similarity");
            let blend_id = builtin_parameter_id(effect_type, "blend");
            let invert_id = builtin_parameter_id(effect_type, "invert");
            Some(Arc::new(move |effect, context, graph| {
                let Some(PropertyValue::Color(color)) =
                    effect.evaluate_parameter(&color_id, context.time)
                else {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: "Chroma Key requires a color parameter".to_owned(),
                    });
                };
                let keyer = crate::PreparedChromaKey::new(
                    color,
                    effect.evaluate_f32_parameter(&similarity_id, context.time, 0.2),
                    effect.evaluate_f32_parameter(&blend_id, context.time, 0.1),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                let keep_key_color = matches!(
                    effect.evaluate_parameter(&invert_id, context.time),
                    Some(PropertyValue::Bool(true))
                );
                let source = graph.current_output();
                let matte = graph.append_unary_in_domain(
                    EffectRenderOp::ChromaKey { keyer },
                    EffectColorDomainContract {
                        input: EffectColorDomain::SceneLinearRgb,
                        output: EffectColorDomain::AlphaMask,
                    },
                );
                let output = graph.add_mask(source, matte, !keep_key_color, crate::MaskOp::Add);
                graph.set_current_output(output);
                Ok(())
            }))
        }
        EffectType::HueSaturationLightness => {
            let hue_id = builtin_parameter_id(effect_type, "hue");
            let saturation_id = builtin_parameter_id(effect_type, "saturation");
            let lightness_id = builtin_parameter_id(effect_type, "lightness");
            Some(Arc::new(move |effect, context, graph| {
                let grade = crate::HslGrade::new(
                    effect.evaluate_f32_parameter(&hue_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&saturation_id, context.time, 1.0),
                    effect.evaluate_f32_parameter(&lightness_id, context.time, 0.0),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::HueSaturationLightness { grade });
                }
                Ok(())
            }))
        }
        EffectType::Crop => {
            let left_id = builtin_parameter_id(effect_type, "left");
            let top_id = builtin_parameter_id(effect_type, "top");
            let right_id = builtin_parameter_id(effect_type, "right");
            let bottom_id = builtin_parameter_id(effect_type, "bottom");
            Some(Arc::new(move |effect, context, graph| {
                let left = effect.evaluate_f32_parameter(&left_id, context.time, 0.0) / 100.0;
                let top = effect.evaluate_f32_parameter(&top_id, context.time, 0.0) / 100.0;
                let right = effect.evaluate_f32_parameter(&right_id, context.time, 0.0) / 100.0;
                let bottom = effect.evaluate_f32_parameter(&bottom_id, context.time, 0.0) / 100.0;
                if [left, top, right, bottom].into_iter().any(|inset| inset > 1.0e-6) {
                    graph.append_unary(EffectRenderOp::Crop { left, top, right, bottom });
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
        EffectType::Invert => {
            let channels_id = builtin_parameter_id(effect_type, "channels");
            let blend_id = builtin_parameter_id(effect_type, "blend_with_original");
            Some(Arc::new(move |effect, context, graph| {
                let channels_key = effect
                    .evaluate_enum_parameter(&channels_id, context.time)
                    .ok_or_else(|| EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: "invert channel scope is not authored".to_owned(),
                    })?;
                let channels = crate::InvertChannels::from_key(&channels_key).ok_or_else(|| {
                    EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!("unknown invert channel scope `{channels_key}`"),
                    }
                })?;
                let blend_with_original =
                    effect.evaluate_f32_parameter(&blend_id, context.time, 0.0) / 100.0;
                if blend_with_original < 1.0 - 1.0e-6 {
                    graph.append_unary(EffectRenderOp::Invert { channels, blend_with_original });
                }
                Ok(())
            }))
        }
        EffectType::BoxBlur => {
            let radius_id = builtin_parameter_id(effect_type, "radius");
            let iterations_id = builtin_parameter_id(effect_type, "iterations");
            let direction_id = builtin_parameter_id(effect_type, "direction");
            let repeat_edge_pixels_id = builtin_parameter_id(effect_type, "repeat_edge_pixels");
            Some(Arc::new(move |effect, context, graph| {
                let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 0.0);
                if radius.abs() <= 1.0e-4 {
                    return Ok(());
                }
                let Some(radius) = crate::box_blur::valid_box_blur_radius(radius) else {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!(
                            "box blur radius {radius} is outside 0..={}",
                            crate::BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS
                        ),
                    });
                };
                let iterations = match effect.evaluate_parameter(&iterations_id, context.time) {
                    Some(PropertyValue::Int(iterations)) => iterations,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "box blur iterations must be an integer".to_owned(),
                        });
                    }
                    None => 3,
                };
                if !(crate::box_blur::BOX_BLUR_AUTHOR_MIN_ITERATIONS
                    ..=crate::box_blur::BOX_BLUR_AUTHOR_MAX_ITERATIONS)
                    .contains(&iterations)
                {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!("box blur iterations {iterations} is outside 1..=50"),
                    });
                }
                let direction_key = effect
                    .evaluate_enum_parameter(&direction_id, context.time)
                    .ok_or_else(|| EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: "box blur direction is not authored".to_owned(),
                    })?;
                let direction =
                    crate::BoxBlurDirection::from_key(&direction_key).ok_or_else(|| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: format!("unknown box blur direction `{direction_key}`"),
                        }
                    })?;
                let repeat_edge_pixels =
                    match effect.evaluate_parameter(&repeat_edge_pixels_id, context.time) {
                        Some(PropertyValue::Bool(repeat_edge_pixels)) => repeat_edge_pixels,
                        Some(_) => {
                            return Err(EffectGraphBuildError::InvalidAuthorState {
                                effect_key: effect.effect_type.key(),
                                effect_id: effect.id,
                                reason: "box blur repeat edge pixels must be a boolean".to_owned(),
                            });
                        }
                        None => true,
                    };
                graph.append_unary(EffectRenderOp::BoxBlur {
                    radius,
                    iterations: iterations as u32,
                    direction,
                    repeat_edge_pixels,
                });
                Ok(())
            }))
        }
        EffectType::UnsharpMask => {
            let amount_id = builtin_parameter_id(effect_type, "amount");
            let radius_id = builtin_parameter_id(effect_type, "radius");
            let threshold_id = builtin_parameter_id(effect_type, "threshold");
            Some(Arc::new(move |effect, context, graph| {
                let gain = crate::unsharp_mask::unsharp_mask_gain(effect.evaluate_f32_parameter(
                    &amount_id,
                    context.time,
                    50.0,
                ));
                if gain <= 1.0e-4 {
                    return Ok(());
                }
                let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 1.0);
                let Some(radius) = crate::unsharp_mask::valid_unsharp_mask_radius(radius) else {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!(
                            "unsharp mask radius {radius} is outside {}..={}",
                            crate::unsharp_mask::UNSHARP_MASK_AUTHOR_MIN_RADIUS_PIXELS,
                            crate::unsharp_mask::UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS
                        ),
                    });
                };
                let threshold = effect.evaluate_f32_parameter(&threshold_id, context.time, 0.0);
                if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!("unsharp mask threshold {threshold} is outside 0..=1"),
                    });
                }
                graph.append_unary(EffectRenderOp::UnsharpMask { gain, radius, threshold });
                Ok(())
            }))
        }
        EffectType::FindEdges => {
            let blend_id = builtin_parameter_id(effect_type, "blend_with_original");
            let invert_id = builtin_parameter_id(effect_type, "invert");
            Some(Arc::new(move |effect, context, graph| {
                let blend = effect.evaluate_f32_parameter(&blend_id, context.time, 0.0);
                let invert = match effect.evaluate_parameter(&invert_id, context.time) {
                    Some(PropertyValue::Bool(value)) => value,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "find edges invert must be a boolean".to_owned(),
                        });
                    }
                    None => false,
                };
                let grade = crate::FindEdgesGrade::new(blend, invert).map_err(|error| {
                    EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: error.to_string(),
                    }
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::FindEdges { grade });
                }
                Ok(())
            }))
        }
        EffectType::Posterize => {
            let levels_id = builtin_parameter_id(effect_type, "levels");
            Some(Arc::new(move |effect, context, graph| {
                let levels = match effect.evaluate_parameter(&levels_id, context.time) {
                    Some(PropertyValue::Int(value)) => value,
                    _ => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "posterize levels must be an integer".to_owned(),
                        });
                    }
                };
                let levels = u32::try_from(levels).map_err(|_| {
                    EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: "posterize levels must be positive".to_owned(),
                    }
                })?;
                let grade = crate::PosterizeGrade::new(levels).map_err(|error| {
                    EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: error.to_string(),
                    }
                })?;
                graph.append_unary(EffectRenderOp::Posterize { grade });
                Ok(())
            }))
        }
        EffectType::Mosaic => {
            let horizontal_id = builtin_parameter_id(effect_type, "horizontal");
            let vertical_id = builtin_parameter_id(effect_type, "vertical");
            let sharp_id = builtin_parameter_id(effect_type, "sharp_colors");
            Some(Arc::new(move |effect, context, graph| {
                let horizontal = match effect.evaluate_parameter(&horizontal_id, context.time) {
                    Some(PropertyValue::Int(value)) => value,
                    _ => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "mosaic horizontal block must be an integer".to_owned(),
                        });
                    }
                };
                let vertical = match effect.evaluate_parameter(&vertical_id, context.time) {
                    Some(PropertyValue::Int(value)) => value,
                    _ => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "mosaic vertical block must be an integer".to_owned(),
                        });
                    }
                };
                let sharp_colors = match effect.evaluate_parameter(&sharp_id, context.time) {
                    Some(PropertyValue::Bool(value)) => value,
                    _ => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "mosaic sharp colours flag must be a boolean".to_owned(),
                        });
                    }
                };
                let grade = crate::MosaicGrade::new(
                    u32::try_from(horizontal).map_err(|_| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "mosaic horizontal block must be positive".to_owned(),
                        }
                    })?,
                    u32::try_from(vertical).map_err(|_| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "mosaic vertical block must be positive".to_owned(),
                        }
                    })?,
                    sharp_colors,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Mosaic { grade });
                }
                Ok(())
            }))
        }
        EffectType::Kaleidoscope => {
            let center_x_id = builtin_parameter_id(effect_type, "center_x");
            let center_y_id = builtin_parameter_id(effect_type, "center_y");
            let size_id = builtin_parameter_id(effect_type, "size");
            let mirroring_id = builtin_parameter_id(effect_type, "mirroring");
            let rotation_id = builtin_parameter_id(effect_type, "rotation");
            let floating_id = builtin_parameter_id(effect_type, "floating_center");
            Some(Arc::new(move |effect, context, graph| {
                let mirroring = match effect.evaluate_parameter(&mirroring_id, context.time) {
                    Some(PropertyValue::Int(value)) => u32::try_from(value).map_err(|_| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "kaleidoscope mirroring must be a positive integer".to_owned(),
                        }
                    })?,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "kaleidoscope mirroring must be an integer".to_owned(),
                        })
                    }
                    None => 6,
                };
                let floating_center = match effect.evaluate_parameter(&floating_id, context.time) {
                    Some(PropertyValue::Bool(value)) => value,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "kaleidoscope floating centre must be a boolean".to_owned(),
                        })
                    }
                    None => false,
                };
                let grade = crate::KaleidoscopeGrade::new(
                    [
                        effect.evaluate_f32_parameter(&center_x_id, context.time, 128.0),
                        effect.evaluate_f32_parameter(&center_y_id, context.time, 128.0),
                    ],
                    effect.evaluate_f32_parameter(&size_id, context.time, 20.0),
                    mirroring,
                    effect.evaluate_f32_parameter(&rotation_id, context.time, 0.0),
                    floating_center,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Kaleidoscope { grade });
                }
                Ok(())
            }))
        }
        EffectType::Bevel => {
            let thickness_id = builtin_parameter_id(effect_type, "thickness");
            let angle_id = builtin_parameter_id(effect_type, "angle");
            let light_color_id = builtin_parameter_id(effect_type, "light_color");
            let intensity_id = builtin_parameter_id(effect_type, "intensity");
            Some(Arc::new(move |effect, context, graph| {
                let light_color = match effect.evaluate_parameter(&light_color_id, context.time) {
                    Some(PropertyValue::Color(color)) => [color.r, color.g, color.b],
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "bevel light colour must be a colour".to_owned(),
                        })
                    }
                    None => [1.0, 1.0, 1.0],
                };
                let grade = crate::BevelGrade::new(
                    effect.evaluate_f32_parameter(&thickness_id, context.time, 2.0),
                    effect.evaluate_f32_parameter(&angle_id, context.time, -60.0),
                    light_color,
                    effect.evaluate_f32_parameter(&intensity_id, context.time, 0.4),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Bevel { grade });
                }
                Ok(())
            }))
        }
        EffectType::Scatter => {
            let amount_id = builtin_parameter_id(effect_type, "amount");
            let copies_id = builtin_parameter_id(effect_type, "copies");
            let randomness_id = builtin_parameter_id(effect_type, "randomness");
            Some(Arc::new(move |effect, context, graph| {
                let copies = match effect.evaluate_parameter(&copies_id, context.time) {
                    Some(PropertyValue::Int(value)) => u32::try_from(value).map_err(|_| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "scatter copies must be a positive integer".to_owned(),
                        }
                    })?,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "scatter copies must be an integer".to_owned(),
                        })
                    }
                    None => 1,
                };
                let grade = crate::ScatterGrade::new(
                    effect.evaluate_f32_parameter(&amount_id, context.time, 0.0),
                    copies,
                    effect.evaluate_f32_parameter(&randomness_id, context.time, 0.0),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Scatter { grade });
                }
                Ok(())
            }))
        }
        EffectType::BrushStrokes => {
            let angle_id = builtin_parameter_id(effect_type, "angle");
            let size_id = builtin_parameter_id(effect_type, "size");
            let length_id = builtin_parameter_id(effect_type, "length");
            let density_id = builtin_parameter_id(effect_type, "density");
            let randomness_id = builtin_parameter_id(effect_type, "randomness");
            let surface_id = builtin_parameter_id(effect_type, "surface");
            let blend_id = builtin_parameter_id(effect_type, "blend_with_original");
            Some(Arc::new(move |effect, context, graph| {
                let surface = match effect.evaluate_parameter(&surface_id, context.time) {
                    Some(PropertyValue::Int(value)) => u32::try_from(value).map_err(|_| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "brush strokes surface must be a positive integer".to_owned(),
                        }
                    })?,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "brush strokes surface must be an integer".to_owned(),
                        })
                    }
                    None => 1,
                };
                let grade = crate::BrushStrokesGrade::new(
                    effect.evaluate_f32_parameter(&angle_id, context.time, 135.0),
                    effect.evaluate_f32_parameter(&size_id, context.time, 2.0),
                    effect.evaluate_f32_parameter(&length_id, context.time, 5.0),
                    effect.evaluate_f32_parameter(&density_id, context.time, 1.0),
                    effect.evaluate_f32_parameter(&randomness_id, context.time, 1.0),
                    surface,
                    effect.evaluate_f32_parameter(&blend_id, context.time, 0.0),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::BrushStrokes { grade });
                }
                Ok(())
            }))
        }
        EffectType::MotionTile => {
            let center_x_id = builtin_parameter_id(effect_type, "center_x");
            let center_y_id = builtin_parameter_id(effect_type, "center_y");
            let tile_width_id = builtin_parameter_id(effect_type, "tile_width");
            let tile_height_id = builtin_parameter_id(effect_type, "tile_height");
            let output_width_id = builtin_parameter_id(effect_type, "output_width");
            let output_height_id = builtin_parameter_id(effect_type, "output_height");
            let mirror_id = builtin_parameter_id(effect_type, "mirror_edges");
            let phase_id = builtin_parameter_id(effect_type, "phase");
            let horizontal_phase_id = builtin_parameter_id(effect_type, "horizontal_phase");
            Some(Arc::new(move |effect, context, graph| {
                let mirror_edges = match effect.evaluate_parameter(&mirror_id, context.time) {
                    Some(PropertyValue::Bool(value)) => value,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "motion tile mirror edges must be a boolean".to_owned(),
                        })
                    }
                    None => false,
                };
                let grade = crate::MotionTileGrade::new(
                    [
                        effect.evaluate_f32_parameter(&center_x_id, context.time, 128.0),
                        effect.evaluate_f32_parameter(&center_y_id, context.time, 128.0),
                    ],
                    effect.evaluate_f32_parameter(&tile_width_id, context.time, 100.0),
                    effect.evaluate_f32_parameter(&tile_height_id, context.time, 100.0),
                    effect.evaluate_f32_parameter(&output_width_id, context.time, 100.0),
                    effect.evaluate_f32_parameter(&output_height_id, context.time, 100.0),
                    mirror_edges,
                    effect.evaluate_f32_parameter(&phase_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&horizontal_phase_id, context.time, 0.0),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::MotionTile { grade });
                }
                Ok(())
            }))
        }
        EffectType::Threshold => {
            let level_id = builtin_parameter_id(effect_type, "level");
            Some(Arc::new(move |effect, context, graph| {
                let grade = crate::ThresholdGrade::for_space(
                    effect.evaluate_f32_parameter(&level_id, context.time, 0.5),
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                graph.append_unary(EffectRenderOp::Threshold { grade });
                Ok(())
            }))
        }
        EffectType::Tint => {
            let black_id = builtin_parameter_id(effect_type, "map_black");
            let white_id = builtin_parameter_id(effect_type, "map_white");
            let amount_id = builtin_parameter_id(effect_type, "amount");
            Some(Arc::new(move |effect, context, graph| {
                let target = |parameter_id: &ParameterId,
                              name: &str,
                              fallback: [f32; 3]|
                 -> Result<[f32; 3], EffectGraphBuildError> {
                    match effect.evaluate_parameter(parameter_id, context.time) {
                        Some(PropertyValue::Color(color)) => Ok([color.r, color.g, color.b]),
                        Some(_) => Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: format!("tint {name} must be a colour"),
                        }),
                        None => Ok(fallback),
                    }
                };
                let black = target(&black_id, "black target", [0.0, 0.0, 0.0])?;
                let white = target(&white_id, "white target", [1.0, 1.0, 1.0])?;
                let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 100.0) / 100.0;
                let grade =
                    crate::TintGrade::for_space(black, white, amount, context.working_color_space)
                        .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: error.to_string(),
                        })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Tint { grade });
                }
                Ok(())
            }))
        }
        EffectType::Vibrance => {
            let vibrance_id = builtin_parameter_id(effect_type, "vibrance");
            let saturation_id = builtin_parameter_id(effect_type, "saturation");
            Some(Arc::new(move |effect, context, graph| {
                let grade = crate::VibranceGrade::for_space(
                    effect.evaluate_f32_parameter(&vibrance_id, context.time, 0.0) / 100.0,
                    effect.evaluate_f32_parameter(&saturation_id, context.time, 0.0) / 100.0,
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Vibrance { grade });
                }
                Ok(())
            }))
        }
        EffectType::Tritone => {
            let highlights_id = builtin_parameter_id(effect_type, "highlights");
            let midtones_id = builtin_parameter_id(effect_type, "midtones");
            let shadows_id = builtin_parameter_id(effect_type, "shadows");
            let blend_id = builtin_parameter_id(effect_type, "blend_with_original");
            Some(Arc::new(move |effect, context, graph| {
                let colour = |parameter_id: &ParameterId,
                              name: &str,
                              fallback: [f32; 3]|
                 -> Result<[f32; 3], EffectGraphBuildError> {
                    match effect.evaluate_parameter(parameter_id, context.time) {
                        Some(PropertyValue::Color(color)) => Ok([color.r, color.g, color.b]),
                        Some(_) => Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: format!("tritone {name} must be a colour"),
                        }),
                        None => Ok(fallback),
                    }
                };
                let highlights = colour(&highlights_id, "highlight", [1.0, 1.0, 1.0])?;
                let midtones = colour(&midtones_id, "midtone", [0.5, 0.39, 0.27])?;
                let shadows = colour(&shadows_id, "shadow", [0.0, 0.0, 0.0])?;
                let blend = effect.evaluate_f32_parameter(&blend_id, context.time, 0.0) / 100.0;
                let grade = crate::TritoneGrade::for_space(
                    shadows,
                    midtones,
                    highlights,
                    blend,
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Tritone { grade });
                }
                Ok(())
            }))
        }
        EffectType::ColorEmboss => {
            let direction_id = builtin_parameter_id(effect_type, "direction");
            let relief_id = builtin_parameter_id(effect_type, "relief");
            let contrast_id = builtin_parameter_id(effect_type, "contrast");
            let blend_id = builtin_parameter_id(effect_type, "blend_with_original");
            Some(Arc::new(move |effect, context, graph| {
                let grade = crate::ColorEmbossGrade::new(
                    effect.evaluate_f32_parameter(&direction_id, context.time, 45.0),
                    effect.evaluate_f32_parameter(&relief_id, context.time, 1.0),
                    effect.evaluate_f32_parameter(&contrast_id, context.time, 100.0),
                    effect.evaluate_f32_parameter(&blend_id, context.time, 0.0) / 100.0,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::ColorEmboss { grade });
                }
                Ok(())
            }))
        }
        EffectType::Emboss => {
            let direction_id = builtin_parameter_id(effect_type, "direction");
            let relief_id = builtin_parameter_id(effect_type, "relief");
            let contrast_id = builtin_parameter_id(effect_type, "contrast");
            let blend_id = builtin_parameter_id(effect_type, "blend_with_original");
            Some(Arc::new(move |effect, context, graph| {
                let grade = crate::EmbossGrade::for_space(
                    effect.evaluate_f32_parameter(&direction_id, context.time, 45.0),
                    effect.evaluate_f32_parameter(&relief_id, context.time, 1.0),
                    effect.evaluate_f32_parameter(&contrast_id, context.time, 100.0),
                    effect.evaluate_f32_parameter(&blend_id, context.time, 0.0),
                    context.working_color_space,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::Emboss { grade });
                }
                Ok(())
            }))
        }
        EffectType::DropShadow => {
            let color_id = builtin_parameter_id(effect_type, "color");
            let opacity_id = builtin_parameter_id(effect_type, "opacity");
            let direction_id = builtin_parameter_id(effect_type, "direction");
            let distance_id = builtin_parameter_id(effect_type, "distance");
            let softness_id = builtin_parameter_id(effect_type, "softness");
            let shadow_only_id = builtin_parameter_id(effect_type, "shadow_only");
            Some(Arc::new(move |effect, context, graph| {
                let Some(PropertyValue::Color(color)) =
                    effect.evaluate_parameter(&color_id, context.time)
                else {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: "drop shadow requires a shadow color".to_owned(),
                    });
                };
                let opacity = effect.evaluate_f32_parameter(&opacity_id, context.time, 127.5);
                let direction = effect.evaluate_f32_parameter(&direction_id, context.time, 135.0);
                let distance = effect.evaluate_f32_parameter(&distance_id, context.time, 5.0);
                let softness = effect.evaluate_f32_parameter(&softness_id, context.time, 0.0);
                let shadow_only = match effect.evaluate_parameter(&shadow_only_id, context.time) {
                    Some(PropertyValue::Bool(value)) => value,
                    None => false,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "drop shadow shadow_only must be a boolean".to_owned(),
                        });
                    }
                };
                let grade = crate::DropShadowGrade::new(
                    color,
                    opacity,
                    direction,
                    distance,
                    softness,
                    shadow_only,
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::DropShadow { grade });
                }
                Ok(())
            }))
        }
        EffectType::ChannelMixer => {
            let weights = [
                builtin_parameter_id(effect_type, "red_red"),
                builtin_parameter_id(effect_type, "red_green"),
                builtin_parameter_id(effect_type, "red_blue"),
                builtin_parameter_id(effect_type, "red_constant"),
                builtin_parameter_id(effect_type, "green_red"),
                builtin_parameter_id(effect_type, "green_green"),
                builtin_parameter_id(effect_type, "green_blue"),
                builtin_parameter_id(effect_type, "green_constant"),
                builtin_parameter_id(effect_type, "blue_red"),
                builtin_parameter_id(effect_type, "blue_green"),
                builtin_parameter_id(effect_type, "blue_blue"),
                builtin_parameter_id(effect_type, "blue_constant"),
            ];
            let defaults = [
                100.0_f32, 0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32, 100.0_f32, 0.0_f32, 0.0_f32,
                0.0_f32, 0.0_f32, 100.0_f32, 0.0_f32,
            ];
            let monochrome_id = builtin_parameter_id(effect_type, "monochrome");
            Some(Arc::new(move |effect, context, graph| {
                let mut authored = [0.0_f32; crate::CHANNEL_MIXER_WEIGHT_COUNT];
                for (index, weight_id) in weights.iter().enumerate() {
                    authored[index] =
                        effect.evaluate_f32_parameter(weight_id, context.time, defaults[index]);
                }
                let monochrome = match effect.evaluate_parameter(&monochrome_id, context.time) {
                    Some(PropertyValue::Bool(value)) => value,
                    None => false,
                    Some(_) => {
                        return Err(EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "channel mixer monochrome must be a boolean".to_owned(),
                        });
                    }
                };
                let grade =
                    crate::ChannelMixerGrade::new(authored, monochrome).map_err(|error| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: error.to_string(),
                        }
                    })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::ChannelMixer { grade });
                }
                Ok(())
            }))
        }
        EffectType::DirectionalBlur => {
            let length_id = builtin_parameter_id(effect_type, "length");
            let direction_id = builtin_parameter_id(effect_type, "direction");
            Some(Arc::new(move |effect, context, graph| {
                let length = effect.evaluate_f32_parameter(&length_id, context.time, 0.0);
                if !length.is_finite()
                    || !(0.0..=crate::DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS).contains(&length)
                {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!(
                            "directional blur length {length} is outside 0..={}",
                            crate::DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS
                        ),
                    });
                }
                let direction_degrees =
                    effect.evaluate_f32_parameter(&direction_id, context.time, 0.0);
                if !direction_degrees.is_finite() {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!(
                            "directional blur direction {direction_degrees} is not a finite angle"
                        ),
                    });
                }
                if length > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::DirectionalBlur {
                        length,
                        direction_degrees,
                    });
                }
                Ok(())
            }))
        }
        EffectType::ShiftChannels => {
            let ids = [
                builtin_parameter_id(effect_type, "red"),
                builtin_parameter_id(effect_type, "green"),
                builtin_parameter_id(effect_type, "blue"),
                builtin_parameter_id(effect_type, "alpha"),
            ];
            Some(Arc::new(move |effect, context, graph| {
                let mut outputs = [crate::ChannelSource::Red; 4];
                for (index, id) in ids.iter().enumerate() {
                    let key =
                        effect.evaluate_enum_parameter(id, context.time).ok_or_else(|| {
                            EffectGraphBuildError::InvalidAuthorState {
                                effect_key: effect.effect_type.key(),
                                effect_id: effect.id,
                                reason: "shift channels source is not authored".to_owned(),
                            }
                        })?;
                    outputs[index] = crate::ChannelSource::from_key(&key).ok_or_else(|| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: format!("unknown shift channels source `{key}`"),
                        }
                    })?;
                }
                let grade = crate::ShiftChannelsGrade::new(
                    outputs[0],
                    outputs[1],
                    outputs[2],
                    outputs[3],
                    context.working_color_space.luminance_coefficients(),
                );
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::ShiftChannels { grade });
                }
                Ok(())
            }))
        }
        EffectType::BlackAndWhite => {
            let weights = [
                builtin_parameter_id(effect_type, "red"),
                builtin_parameter_id(effect_type, "yellow"),
                builtin_parameter_id(effect_type, "green"),
                builtin_parameter_id(effect_type, "cyan"),
                builtin_parameter_id(effect_type, "blue"),
                builtin_parameter_id(effect_type, "magenta"),
            ];
            let defaults = [40.0, 60.0, 40.0, 60.0, 20.0, 80.0];
            let tint_amount_id = builtin_parameter_id(effect_type, "tint_amount");
            let tint_color_id = builtin_parameter_id(effect_type, "tint_color");
            Some(Arc::new(move |effect, context, graph| {
                let mut authored = [0.0; crate::BLACK_WHITE_HUE_STOPS];
                for (index, weight_id) in weights.iter().enumerate() {
                    authored[index] =
                        effect.evaluate_f32_parameter(weight_id, context.time, defaults[index]);
                }
                let Some(PropertyValue::Color(color)) =
                    effect.evaluate_parameter(&tint_color_id, context.time)
                else {
                    return Err(EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: "black and white requires a tint color".to_owned(),
                    });
                };
                let rgba = color.to_rgba();
                let grade = crate::BlackAndWhiteGrade::new(
                    authored,
                    effect.evaluate_f32_parameter(&tint_amount_id, context.time, 0.0),
                    [rgba.r, rgba.g, rgba.b],
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                graph.append_unary(EffectRenderOp::BlackAndWhite { grade });
                Ok(())
            }))
        }
        EffectType::HueSaturation => {
            let band_id = builtin_parameter_id(effect_type, "band");
            let hue_id = builtin_parameter_id(effect_type, "hue");
            let saturation_id = builtin_parameter_id(effect_type, "saturation");
            let lightness_id = builtin_parameter_id(effect_type, "lightness");
            let colorize_id = builtin_parameter_id(effect_type, "colorize");
            let tint_hue_id = builtin_parameter_id(effect_type, "tint_hue");
            let tint_saturation_id = builtin_parameter_id(effect_type, "tint_saturation");
            let tint_lightness_id = builtin_parameter_id(effect_type, "tint_lightness");
            Some(Arc::new(move |effect, context, graph| {
                let band_key =
                    effect.evaluate_enum_parameter(&band_id, context.time).ok_or_else(|| {
                        EffectGraphBuildError::InvalidAuthorState {
                            effect_key: effect.effect_type.key(),
                            effect_id: effect.id,
                            reason: "hue/saturation band is not authored".to_owned(),
                        }
                    })?;
                let band = crate::HueBand::from_key(&band_key).ok_or_else(|| {
                    EffectGraphBuildError::InvalidAuthorState {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        reason: format!("unknown hue/saturation band `{band_key}`"),
                    }
                })?;
                let grade = crate::HueSaturationGrade::new(
                    band,
                    effect.evaluate_f32_parameter(&hue_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&saturation_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&lightness_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&colorize_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&tint_hue_id, context.time, 0.0),
                    effect.evaluate_f32_parameter(&tint_saturation_id, context.time, 25.0),
                    effect.evaluate_f32_parameter(&tint_lightness_id, context.time, 0.0),
                )
                .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    reason: error.to_string(),
                })?;
                if !grade.is_identity() {
                    graph.append_unary(EffectRenderOp::HueSaturation { grade });
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
        EffectType::ColorWheel => "Primaries",
        EffectType::HdrGrading => "HDR Grading",
        EffectType::AscCdl => "ASC CDL",
        EffectType::Curves => "曲线",
        EffectType::GamutCompression => "色域压缩",
        EffectType::HighlightRecovery => "高光恢复",
        EffectType::Qualifier => "HSL / 3D Qualifier",
        EffectType::HueSaturationLightness => "HSL",
        EffectType::Crop => "裁切",
        EffectType::GaussianBlur => "模糊",
        EffectType::Sharpen => "锐化",
        EffectType::Vignette => "暗角",
        EffectType::ChromaticAberration => "色差",
        EffectType::Grain => "颗粒",
        EffectType::ChromaKey => "色度抠像",
        EffectType::LumaKey => "亮度键",
        EffectType::Invert => "反转",
        EffectType::BoxBlur => "方框模糊",
        EffectType::UnsharpMask => "钝化蒙版",
        EffectType::HueSaturation => "色相饱和度",
        EffectType::BlackAndWhite => "黑白",
        EffectType::ShiftChannels => "转换通道",
        EffectType::DirectionalBlur => "方向模糊",
        EffectType::ChannelMixer => "通道混合器",
        EffectType::DropShadow => "投影",
        EffectType::FindEdges => "查找边缘",
        EffectType::Threshold => "阈值",
        EffectType::Mosaic => "马赛克",
        EffectType::Posterize => "色调分离",
        EffectType::MotionTile => "动态拼贴",
        EffectType::Kaleidoscope => "万花筒",
        EffectType::Bevel => "斜面与浮雕",
        EffectType::Scatter => "散射",
        EffectType::BrushStrokes => "画笔描边",
        EffectType::Tint => "色调",
        EffectType::Vibrance => "自然饱和度",
        EffectType::Tritone => "三色调",
        EffectType::ColorEmboss => "色彩浮雕",
        EffectType::Emboss => "浮雕",
        EffectType::External(_) => "外部效果",
    }
}

fn hdr_zone_display_name(zone: crate::HdrGradingZone) -> &'static str {
    match zone {
        crate::HdrGradingZone::Blacks => "Blacks",
        crate::HdrGradingZone::Dark => "Dark",
        crate::HdrGradingZone::Shadows => "Shadows",
        crate::HdrGradingZone::Light => "Light",
        crate::HdrGradingZone::Highlights => "Highlights",
        crate::HdrGradingZone::Specular => "Specular",
    }
}

// EffectType methods (key, from_key, display_name, etc.) are now in mondrian_core::effect_data

/// Registry-aware display name — prefers the registered definition's display_name over the static fallback.
pub fn effect_display_name(effect_type: &EffectType) -> String {
    if let Some(definition) = effect_definition(effect_type) {
        return definition.display_name().to_string();
    }
    if let EffectType::External(key) = effect_type {
        return key.clone();
    }
    effect_type.display_name().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EffectGraphNodeKind;
    use mondrian_core::{
        automation::{
            Keyframe, PropertyHost, PropertyMutation, PropertyValue, QualifierSample,
            QualifierSampleOperation, QualifierSampleSet,
        },
        TimelineTime, WorkingColorSpace,
    };

    const TEST_WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec709;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("valid test time")
    }

    #[test]
    fn project_geometry_validates_external_coordinates_without_raster_limits() {
        assert_eq!(
            EffectProjectGeometry::new(0, 7, SampleAspectRatio::SQUARE),
            Err(EffectProjectGeometryError::EmptyExtent)
        );
        assert_eq!(
            EffectProjectGeometry::new(10, 0, SampleAspectRatio::SQUARE),
            Err(EffectProjectGeometryError::EmptyExtent)
        );
        let parse = |width, height, numerator, denominator| {
            serde_json::from_value::<EffectProjectGeometry>(serde_json::json!({
                "width": width, "height": height,
                "pixel_aspect_ratio": {"numerator": numerator, "denominator": denominator}
            }))
        };
        assert!(parse(10, 7, 0, 1).is_err());
        assert!(parse(10, 7, 1, 0).is_err());
        assert!(parse(0, 7, 1, 1).is_err());
        let geometry = parse(10, 7, 4, 2).expect("equivalent ratios normalize");
        assert_eq!(geometry.canonical_size(), [20.0, 7.0]);
        assert_eq!(
            geometry.pixel_aspect_ratio(),
            SampleAspectRatio::new(2, 1).unwrap()
        );
        let mut wire = serde_json::to_value(geometry).expect("geometry wire");
        assert_eq!(
            serde_json::from_value::<EffectProjectGeometry>(wire.clone()).unwrap(),
            geometry
        );
        wire["offset"] = serde_json::json!([1, 0]);
        assert!(serde_json::from_value::<EffectProjectGeometry>(wire).is_err());
        let huge = EffectProjectGeometry::new(
            u32::MAX,
            u32::MAX,
            SampleAspectRatio::new(u32::MAX, 1).unwrap(),
        )
        .expect("coordinates are not allocation");
        assert!(huge.canonical_size().iter().all(|v| v.is_finite()));
        let invalid_ratio: SampleAspectRatio =
            serde_json::from_value(serde_json::json!({"numerator": 1, "denominator": 0})).unwrap();
        assert_eq!(
            EffectProjectGeometry::new(10, 7, invalid_ratio),
            Err(EffectProjectGeometryError::InvalidPixelAspectRatio)
        );
    }

    #[test]
    fn frame_context_covers_subframe_clip_range_without_dropping_it() {
        let range = TimelineTimeRange::new(
            TimelineTime::new(1, 100).expect("start"),
            TimelineTime::new(1, 100).expect("duration"),
        )
        .expect("short range");
        let context = EffectFrameContext::new(
            Rational::FPS_25,
            EffectProjectGeometry::new(10, 7, SampleAspectRatio::SQUARE).unwrap(),
            range,
        )
        .expect("subframe range remains renderable");
        assert_eq!(context.available_frames().expect("frame bounds"), (0, 0));
        assert_eq!(context.frame_coordinate(range.start), 0.25);

        let ntsc = EffectFrameContext::new(
            Rational::FPS_23976,
            EffectProjectGeometry::new(10, 7, SampleAspectRatio::new(4, 3).expect("sample aspect"))
                .unwrap(),
            TimelineTimeRange::new(
                TimelineTime::ZERO,
                TimelineTime::new(1001, 24000).expect("one frame"),
            )
            .expect("frame range"),
        )
        .expect("NTSC context");
        assert_eq!(ntsc.available_frames().expect("NTSC bounds"), (0, 0));
        assert_eq!(
            ntsc.frame_coordinate(ntsc.available_range().end().expect("end")),
            1.0
        );
        assert_eq!(ntsc.pixel_aspect_ratio().to_f64(), 4.0 / 3.0);
    }

    fn test_extension_execution_contract() -> EffectExecutionContract {
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
            ..test_extension_execution_contract()
        }
    }

    #[test]
    fn authored_float_custom_backend_executes_through_compiled_graph() {
        let extension_type = EffectType::External("plugin.test.float.custom_backend".to_owned());
        register_effect_definition(
            EffectDefinition::new(
                extension_type.key(),
                "Float Custom",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
                ..test_extension_execution_contract()
            })
            .with_custom_float_render_backend(
                Arc::new(|_, _| Ok(Some(serde_json::json!({ "gain": 0.25 })))),
                None,
                EffectCachePolicy::Deterministic,
                Arc::new(|pixels, params, _| {
                    pixels[0][0] += params["gain"].as_f64().unwrap_or_default() as f32;
                    Ok(())
                }),
            ),
        )
        .expect("register float custom effect");
        let effect = EffectNode::with_defaults(extension_type);
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile float custom effect");
        assert!(crate::compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(
            crate::apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[2.0, -0.25, 0.5, 1.0]]
                ),
                &compiled,
                0
            )
            .expect("execute float custom effect")
            .pixels(),
            vec![[2.25, -0.25, 0.5, 1.0]]
        );
    }

    #[test]
    fn production_instantiation_requires_a_registered_definition_and_forks_owner_identity() {
        let first =
            instantiate_effect_node(EffectType::GaussianBlur).expect("registered built-in Effect");
        let second = instantiate_effect_node(EffectType::GaussianBlur)
            .expect("second registered built-in Effect");
        let first_property = first.properties.iter().next().expect("built-in property").1;
        let second_property = second.properties.iter().next().expect("built-in property").1;

        assert_ne!(first.id, second.id);
        assert_ne!(first_property.track_id, second_property.track_id);
        assert_eq!(first_property.descriptor, second_property.descriptor);
        assert_eq!(
            first_property.static_value(),
            second_property.static_value()
        );

        let missing = EffectType::External("plugin.test.not_registered".to_owned());
        assert!(matches!(
            instantiate_effect_node(missing),
            Err(EffectInstantiationError::DefinitionUnavailable { .. })
        ));
    }

    fn emitted_op_contract_violation(
        key: &str,
        contract: EffectExecutionContract,
        op: EffectRenderOp,
    ) -> crate::EffectExecutionContractViolation {
        let effect_type = EffectType::External(key.to_owned());
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

    fn hue_saturation_op(graph: &EffectRenderGraph) -> Option<(crate::HueBand, [f32; 7])> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::HueSaturation { grade },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::HueSaturation { grade },
                ..
            } => Some((grade.band(), grade.author_controls())),
            _ => None,
        })
    }

    fn black_and_white_op(graph: &EffectRenderGraph) -> Option<crate::BlackAndWhiteGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::BlackAndWhite { grade },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::BlackAndWhite { grade },
                ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn black_and_white_defaults_compile_to_the_listed_weights() {
        let effect = instantiate_effect_node(EffectType::BlackAndWhite)
            .expect("registered Black and White definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Black and White");

        let grade = black_and_white_op(&graph).expect("black and white defaults reach the op");
        assert_eq!(grade.weights(), [40.0, 60.0, 40.0, 60.0, 20.0, 80.0]);
        assert_eq!(grade.tint_amount(), 0.0);
        assert_eq!(grade.tint_color(), [0.88, 0.83, 0.70]);
    }

    #[test]
    fn black_and_white_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::BlackAndWhite)
            .expect("registered Black and White definition");
        for (parameter, value) in [
            ("red", PropertyValue::Float(120.0)),
            ("magenta", PropertyValue::Float(-40.0)),
            ("tint_amount", PropertyValue::Float(0.5)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BlackAndWhite.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Black and White {parameter}: {error}"));
        }
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BlackAndWhite.property_path("tint_color"),
                value: PropertyValue::Color(mondrian_core::Color {
                    r: 0.5,
                    g: 0.25,
                    b: 0.75,
                    a: 1.0,
                }),
            })
            .expect("author the tint color");

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Black and White");
        let grade = black_and_white_op(&graph).expect("authored state reaches the op");
        assert_eq!(grade.weights(), [120.0, 60.0, 40.0, 60.0, 20.0, -40.0]);
        assert_eq!(grade.tint_amount(), 0.5);
        assert_eq!(grade.tint_color(), [0.5, 0.25, 0.75]);
    }

    #[test]
    fn black_and_white_rejects_author_values_outside_the_declared_ranges() {
        for (parameter, value) in [
            ("red", PropertyValue::Float(301.0)),
            ("cyan", PropertyValue::Float(-201.0)),
            ("tint_amount", PropertyValue::Float(1.5)),
        ] {
            let mut effect = instantiate_effect_node(EffectType::BlackAndWhite)
                .expect("registered Black and White definition");
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::BlackAndWhite.property_path(parameter),
                        value,
                    })
                    .is_err(),
                "{parameter} must reject values outside the authored contract"
            );
        }
    }

    #[test]
    fn black_and_white_converts_hue_weighted_lightness_without_clamping() {
        let mut effect = instantiate_effect_node(EffectType::BlackAndWhite)
            .expect("registered Black and White definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BlackAndWhite.property_path("red"),
                value: PropertyValue::Float(300.0),
            })
            .expect("author Black and White");

        // A saturated red keeps the red weight (lightness 0.5 * 3 = 1.5), a
        // neutral gray keeps its own lightness, and an extended-range red maps to
        // 3.75 instead of being clamped into the scene-linear range.
        let input = [
            [1.0, 0.0, 0.0, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [2.0, 0.5, 0.5, 1.0],
        ];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Black and White");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");

        assert_eq!(output[0], [1.5, 1.5, 1.5, 1.0]);
        assert_eq!(output[1], [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(output[2], [3.75, 3.75, 3.75, 1.0]);
    }

    #[test]
    fn black_and_white_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::BlackAndWhite)
            .expect("registered Black and White definition");
        for (parameter, value) in [
            ("yellow", PropertyValue::Float(10.0)),
            ("blue", PropertyValue::Float(240.0)),
            ("tint_amount", PropertyValue::Float(0.4)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BlackAndWhite.property_path(parameter),
                    value,
                })
                .expect("author Black and White");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Black and White node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Black and White node");
        assert_eq!(restored.effect_type, EffectType::BlackAndWhite);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Black and White node");
        let grade = black_and_white_op(&graph).expect("restored state reaches the op");
        assert_eq!(grade.weights(), [40.0, 10.0, 40.0, 60.0, 240.0, 80.0]);
        assert_eq!(grade.tint_amount(), 0.4);
    }

    fn shift_channels_op(graph: &EffectRenderGraph) -> Option<crate::ShiftChannelsGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::ShiftChannels { grade },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::ShiftChannels { grade },
                ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn shift_channels_defaults_compile_to_identity() {
        let effect = instantiate_effect_node(EffectType::ShiftChannels)
            .expect("registered Shift Channels definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Shift Channels");

        assert!(
            shift_channels_op(&graph).is_none(),
            "the listed defaults are the identity rewire"
        );
    }

    #[test]
    fn shift_channels_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::ShiftChannels)
            .expect("registered Shift Channels definition");
        for (parameter, source) in [("red", "green"), ("blue", "alpha"), ("alpha", "luminance")] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ShiftChannels.property_path(parameter),
                    value: PropertyValue::Enum(source.to_owned()),
                })
                .unwrap_or_else(|error| panic!("author Shift Channels {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Shift Channels");
        let grade = shift_channels_op(&graph).expect("shift channels state reaches the op");
        assert_eq!(
            grade.outputs(),
            [
                crate::ChannelSource::Green,
                crate::ChannelSource::Green,
                crate::ChannelSource::Alpha,
                crate::ChannelSource::Luminance,
            ]
        );
        assert_eq!(
            grade.luminance_coefficients(),
            TEST_WORKING_SPACE.luminance_coefficients()
        );
    }

    #[test]
    fn shift_channels_rejects_an_unknown_source_at_authoring() {
        let mut effect = instantiate_effect_node(EffectType::ShiftChannels)
            .expect("registered Shift Channels definition");
        let rejected = effect.apply_property_mutation(PropertyMutation::SetStaticValue {
            path: EffectType::ShiftChannels.property_path("red"),
            value: PropertyValue::Enum("magenta_plus".to_owned()),
        });
        assert!(
            rejected.is_err(),
            "unknown enum keys must not be authorable"
        );
    }

    #[test]
    fn shift_channels_swaps_channels_on_scene_linear_float_frames() {
        let mut effect = instantiate_effect_node(EffectType::ShiftChannels)
            .expect("registered Shift Channels definition");
        for (parameter, source) in [
            ("red", "green"),
            ("green", "red"),
            ("blue", "lightness"),
            ("alpha", "blue"),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ShiftChannels.property_path(parameter),
                    value: PropertyValue::Enum(source.to_owned()),
                })
                .expect("author Shift Channels");
        }

        // Channel copies are exact and unclamped: the extended-range pixel keeps
        // -0.25 and 1.5 in its color channels, while HSL lightness feeds blue.
        let input = [[0.25, 0.5, 0.75, 0.5], [1.5, -0.25, 0.5, 1.0]];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Shift Channels");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");

        assert_eq!(output[0], [0.5, 0.25, 0.5, 0.75]);
        assert_eq!(output[1], [-0.25, 1.5, 0.625, 0.5]);
    }

    #[test]
    fn shift_channels_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::ShiftChannels)
            .expect("registered Shift Channels definition");
        for (parameter, source) in [("red", "blue"), ("alpha", "saturation")] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ShiftChannels.property_path(parameter),
                    value: PropertyValue::Enum(source.to_owned()),
                })
                .expect("author Shift Channels");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Shift Channels node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Shift Channels node");
        assert_eq!(restored.effect_type, EffectType::ShiftChannels);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Shift Channels node");
        let grades = shift_channels_op(&graph).expect("restored state reaches the op");
        assert_eq!(
            grades.outputs(),
            [
                crate::ChannelSource::Blue,
                crate::ChannelSource::Green,
                crate::ChannelSource::Blue,
                crate::ChannelSource::Saturation,
            ]
        );
    }

    fn drop_shadow_op(graph: &EffectRenderGraph) -> Option<crate::DropShadowGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::DropShadow { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::DropShadow { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn drop_shadow_defaults_compile_to_the_listed_shadow() {
        let effect = instantiate_effect_node(EffectType::DropShadow)
            .expect("registered Drop Shadow definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Drop Shadow");
        let grade = drop_shadow_op(&graph).expect("the listed defaults produce a shadow");

        assert_eq!(grade.color(), [0.0, 0.0, 0.0]);
        assert!((grade.opacity() - 0.5).abs() < 1.0e-6);
        assert_eq!(grade.direction_degrees(), 135.0);
        assert_eq!(grade.distance(), 5.0);
        assert_eq!(grade.softness(), 0.0);
        assert!(!grade.shadow_only());
    }

    #[test]
    fn drop_shadow_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::DropShadow)
            .expect("registered Drop Shadow definition");
        for (parameter, value) in [
            (
                "color",
                PropertyValue::Color(mondrian_core::Color { r: 0.25, g: 0.5, b: 0.75, a: 1.0 }),
            ),
            ("opacity", PropertyValue::Float(51.0)),
            ("direction", PropertyValue::Float(90.0)),
            ("distance", PropertyValue::Float(12.5)),
            ("softness", PropertyValue::Float(8.0)),
            ("shadow_only", PropertyValue::Bool(true)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::DropShadow.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Drop Shadow {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Drop Shadow");
        let grade = drop_shadow_op(&graph).expect("drop shadow state reaches the op");
        assert_eq!(grade.color(), [0.25, 0.5, 0.75]);
        assert!((grade.opacity() - 0.2).abs() < 1.0e-6);
        assert_eq!(grade.direction_degrees(), 90.0);
        assert_eq!(grade.distance(), 12.5);
        assert_eq!(grade.softness(), 8.0);
        assert!(grade.shadow_only());
    }

    #[test]
    fn drop_shadow_rejects_author_values_outside_the_declared_ranges() {
        let mut effect = instantiate_effect_node(EffectType::DropShadow)
            .expect("registered Drop Shadow definition");
        for (parameter, value) in [
            ("opacity", PropertyValue::Float(255.5)),
            ("distance", PropertyValue::Float(4_001.0)),
            ("softness", PropertyValue::Float(-1.0)),
        ] {
            let rejected = effect.apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::DropShadow.property_path(parameter),
                value,
            });
            assert!(
                rejected.is_err(),
                "Drop Shadow {parameter} outside its range must not be authorable"
            );
        }
    }

    #[test]
    fn drop_shadow_composites_the_offset_shadow_under_the_source() {
        let mut effect = instantiate_effect_node(EffectType::DropShadow)
            .expect("registered Drop Shadow definition");
        for (parameter, value) in [
            ("opacity", PropertyValue::Float(255.0)),
            ("direction", PropertyValue::Float(90.0)),
            ("distance", PropertyValue::Float(1.0)),
            ("softness", PropertyValue::Float(0.0)),
            (
                "color",
                PropertyValue::Color(mondrian_core::Color { r: 2.0, g: 0.5, b: 0.0, a: 1.0 }),
            ),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::DropShadow.property_path(parameter),
                    value,
                })
                .expect("author Drop Shadow");
        }

        // The white pixel keeps its color on top of the shadow; the empty second pixel
        // receives the offset shadow, whose extended-range red proves the op does not clamp.
        let input = [
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Drop Shadow");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");

        assert_eq!(output[0], [1.0, 1.0, 1.0, 1.0]);
        for (channel, expected) in [2.0_f32, 0.5, 0.0, 1.0].into_iter().enumerate() {
            assert!(
                (output[1][channel] - expected).abs() < 1.0e-5,
                "channel {channel}: {} != {expected}",
                output[1][channel]
            );
        }
        assert_eq!(output[2], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn drop_shadow_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::DropShadow)
            .expect("registered Drop Shadow definition");
        for (parameter, value) in [
            ("distance", PropertyValue::Float(21.0)),
            ("softness", PropertyValue::Float(6.0)),
            ("shadow_only", PropertyValue::Bool(true)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::DropShadow.property_path(parameter),
                    value,
                })
                .expect("author Drop Shadow");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Drop Shadow node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Drop Shadow node");
        assert_eq!(restored.effect_type, EffectType::DropShadow);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Drop Shadow node");
        let grade = drop_shadow_op(&graph).expect("restored state reaches the op");
        assert_eq!(grade.distance(), 21.0);
        assert_eq!(grade.softness(), 6.0);
        assert!(grade.shadow_only());
    }

    fn channel_mixer_op(graph: &EffectRenderGraph) -> Option<crate::ChannelMixerGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::ChannelMixer { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::ChannelMixer { grade },
                ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn channel_mixer_defaults_compile_to_identity() {
        let effect = instantiate_effect_node(EffectType::ChannelMixer)
            .expect("registered Channel Mixer definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Channel Mixer");

        assert!(
            channel_mixer_op(&graph).is_none(),
            "the listed diagonal weights are the identity"
        );
    }

    #[test]
    fn channel_mixer_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::ChannelMixer)
            .expect("registered Channel Mixer definition");
        for (parameter, value) in [
            ("red_red", PropertyValue::Float(0.0)),
            ("red_green", PropertyValue::Float(100.0)),
            ("blue_constant", PropertyValue::Float(25.0)),
            ("monochrome", PropertyValue::Bool(true)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ChannelMixer.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Channel Mixer {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Channel Mixer");
        let grade = channel_mixer_op(&graph).expect("channel mixer state reaches the op");
        let mut expected = [0.0_f32; 12];
        expected[1] = 100.0;
        expected[5] = 100.0;
        expected[10] = 100.0;
        expected[11] = 25.0;
        assert_eq!(grade.weights(), expected);
        assert!(grade.monochrome());
    }

    #[test]
    fn channel_mixer_rejects_author_values_outside_the_declared_range() {
        let mut effect = instantiate_effect_node(EffectType::ChannelMixer)
            .expect("registered Channel Mixer definition");
        for weight in [201.0_f32, -201.0] {
            let rejected = effect.apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::ChannelMixer.property_path("green_blue"),
                value: PropertyValue::Float(weight),
            });
            assert!(
                rejected.is_err(),
                "weights outside -200..=200 must not be authorable"
            );
        }
    }

    #[test]
    fn channel_mixer_mixes_channels_on_scene_linear_float_frames() {
        let mut effect = instantiate_effect_node(EffectType::ChannelMixer)
            .expect("registered Channel Mixer definition");
        for (parameter, value) in [
            ("red_red", PropertyValue::Float(0.0)),
            ("red_green", PropertyValue::Float(100.0)),
            ("blue_blue", PropertyValue::Float(0.0)),
            ("blue_constant", PropertyValue::Float(50.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ChannelMixer.property_path(parameter),
                    value,
                })
                .expect("author Channel Mixer");
        }

        // Red comes from green, blue is a constant offset, and the extended-range
        // second pixel proves the linear mix never clamps to the unit interval.
        let input = [[0.25, 0.5, 0.75, 1.0], [2.0, -1.0, 0.25, 0.5]];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Channel Mixer");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");

        assert_eq!(output[0], [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(output[1], [-1.0, -1.0, 0.5, 0.5]);
    }

    #[test]
    fn channel_mixer_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::ChannelMixer)
            .expect("registered Channel Mixer definition");
        for (parameter, value) in [
            ("red_green", PropertyValue::Float(100.0)),
            ("blue_constant", PropertyValue::Float(25.0)),
            ("monochrome", PropertyValue::Bool(true)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ChannelMixer.property_path(parameter),
                    value,
                })
                .expect("author Channel Mixer");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Channel Mixer node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Channel Mixer node");
        assert_eq!(restored.effect_type, EffectType::ChannelMixer);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Channel Mixer node");
        let grade = channel_mixer_op(&graph).expect("restored state reaches the op");
        assert_eq!(grade.weights()[1], 100.0);
        assert_eq!(grade.weights()[11], 25.0);
        assert!(grade.monochrome());
    }

    #[test]
    fn hue_saturation_defaults_compile_to_identity() {
        let effect = instantiate_effect_node(EffectType::HueSaturation)
            .expect("registered Hue/Saturation definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Hue/Saturation");

        assert_eq!(hue_saturation_op(&graph), None);
    }

    #[test]
    fn hue_saturation_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::HueSaturation)
            .expect("registered Hue/Saturation definition");
        for (parameter, value) in [
            ("band", PropertyValue::Enum("greens".to_owned())),
            ("hue", PropertyValue::Float(90.0)),
            ("saturation", PropertyValue::Float(50.0)),
            ("lightness", PropertyValue::Float(-20.0)),
            ("tint_hue", PropertyValue::Float(-30.0)),
            ("tint_saturation", PropertyValue::Float(40.0)),
            ("tint_lightness", PropertyValue::Float(10.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::HueSaturation.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Hue/Saturation {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Hue/Saturation");
        assert_eq!(
            hue_saturation_op(&graph),
            Some((
                crate::HueBand::Greens,
                [90.0, 50.0, -20.0, 0.0, -30.0, 40.0, 10.0]
            ))
        );
    }

    #[test]
    fn hue_saturation_rejects_an_unknown_band_key_at_authoring() {
        let mut effect = instantiate_effect_node(EffectType::HueSaturation)
            .expect("registered Hue/Saturation definition");

        // The typed parameter schema rejects out-of-menu keys before the graph
        // builder runs; the builder keeps the same check as a defensive backstop
        // for state restored from an older or hand-edited project.
        assert!(
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::HueSaturation.property_path("band"),
                    value: PropertyValue::Enum("teal".to_owned()),
                })
                .is_err(),
            "an unregistered band key must fail closed instead of grading every hue"
        );
        assert_eq!(
            hue_saturation_op(
                &build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
                    .expect("default state still compiles")
            ),
            None
        );
    }

    #[test]
    fn hue_saturation_rejects_author_values_outside_the_declared_ranges() {
        let effect = instantiate_effect_node(EffectType::HueSaturation)
            .expect("registered Hue/Saturation definition");
        for (parameter, value) in [
            ("hue", PropertyValue::Float(181.0)),
            ("saturation", PropertyValue::Float(101.0)),
            ("lightness", PropertyValue::Float(-101.0)),
            ("colorize", PropertyValue::Float(1.5)),
            ("tint_saturation", PropertyValue::Float(-1.0)),
        ] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::HueSaturation.property_path(parameter),
                        value,
                    })
                    .is_err(),
                "the numeric contract rejects {parameter} outside its authored range"
            );
        }
    }

    #[test]
    fn hue_saturation_master_band_executes_on_scene_linear_float_frames() {
        let mut effect = instantiate_effect_node(EffectType::HueSaturation)
            .expect("registered Hue/Saturation definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::HueSaturation.property_path("hue"),
                value: PropertyValue::Float(180.0),
            })
            .expect("author a half-turn hue rotation");

        let input = [[0.8, 0.3, 0.1, 1.0], [1.6, 0.6, 0.2, 0.5]];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Hue/Saturation");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");
        let pixels = output.pixels();
        assert_eq!(pixels.len(), 2);

        // The master band must agree with the established HSL rotation, and the
        // extended-range pixel must keep its coverage and stay off the [0, 1] clamp.
        let grade = crate::HslGrade::new(180.0, 1.0, 0.0).expect("hsl");
        for (pixel, source) in pixels.iter().zip(input.iter()) {
            let expected = grade.apply([source[0], source[1], source[2]]);
            for channel in 0..3 {
                assert!(
                    (pixel[channel] - expected[channel]).abs() <= 1.0e-6,
                    "master band reproduced {pixel:?} instead of {expected:?}"
                );
            }
            assert_eq!(pixel[3], source[3]);
        }
    }

    #[test]
    fn hue_saturation_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::HueSaturation)
            .expect("registered Hue/Saturation definition");
        for (parameter, value) in [
            ("band", PropertyValue::Enum("blues".to_owned())),
            ("hue", PropertyValue::Float(-45.0)),
            ("saturation", PropertyValue::Float(35.0)),
            ("lightness", PropertyValue::Float(12.0)),
            ("colorize", PropertyValue::Float(0.25)),
            ("tint_hue", PropertyValue::Float(120.0)),
            ("tint_saturation", PropertyValue::Float(60.0)),
            ("tint_lightness", PropertyValue::Float(-15.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::HueSaturation.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Hue/Saturation {parameter}: {error}"));
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Hue/Saturation node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Hue/Saturation node");
        assert_eq!(restored.effect_type, EffectType::HueSaturation);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Hue/Saturation node");
        assert_eq!(
            hue_saturation_op(&graph),
            Some((
                crate::HueBand::Blues,
                [-45.0, 35.0, 12.0, 0.25, 120.0, 60.0, -15.0]
            ))
        );
    }

    fn find_edges_op(graph: &EffectRenderGraph) -> Option<crate::FindEdgesGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::FindEdges { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::FindEdges { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn find_edges_defaults_compile_to_the_listed_mix() {
        let effect = instantiate_effect_node(EffectType::FindEdges)
            .expect("registered Find Edges definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Find Edges");
        let grade = find_edges_op(&graph).expect("the listed defaults produce edges");
        assert_eq!(grade.blend_with_original(), 0.0);
        assert!(!grade.invert());
    }

    #[test]
    fn find_edges_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::FindEdges)
            .expect("registered Find Edges definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::FindEdges.property_path("blend_with_original"),
                value: PropertyValue::Float(0.25),
            })
            .expect("set blend");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::FindEdges.property_path("invert"),
                value: PropertyValue::Bool(true),
            })
            .expect("set invert");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Find Edges");
        let grade = find_edges_op(&graph).expect("authored state reaches the op");
        assert!((grade.blend_with_original() - 0.25).abs() < 1.0e-6);
        assert!(grade.invert());
    }

    #[test]
    fn find_edges_full_mix_compiles_to_identity() {
        let mut effect = instantiate_effect_node(EffectType::FindEdges)
            .expect("registered Find Edges definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::FindEdges.property_path("blend_with_original"),
                value: PropertyValue::Float(1.0),
            })
            .expect("set blend");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate identity Find Edges");
        assert!(find_edges_op(&graph).is_none());
    }

    #[test]
    fn find_edges_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::FindEdges)
            .expect("registered Find Edges definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::FindEdges.property_path("blend_with_original"),
                value: PropertyValue::Float(0.4),
            })
            .expect("set blend");
        let wire = serde_json::to_string(&effect).expect("serialize effect");
        let restored: crate::EffectNode = serde_json::from_str(&wire).expect("deserialize effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate restored Find Edges");
        let grade = find_edges_op(&graph).expect("restored state reaches the op");
        assert!((grade.blend_with_original() - 0.4).abs() < 1.0e-6);
    }

    #[test]
    fn effect_library_exposes_only_builtins_with_executable_graphs() {
        let expected = [
            EffectType::BasicCorrection,
            EffectType::WhiteBalance,
            EffectType::Lut3D,
            EffectType::ColorWheel,
            EffectType::AscCdl,
            EffectType::Curves,
            EffectType::HdrGrading,
            EffectType::GamutCompression,
            EffectType::HighlightRecovery,
            EffectType::Qualifier,
            EffectType::LumaKey,
            EffectType::HueSaturationLightness,
            EffectType::ChromaKey,
            EffectType::Crop,
            EffectType::GaussianBlur,
            EffectType::Sharpen,
            EffectType::Vignette,
            EffectType::ChromaticAberration,
            EffectType::Grain,
            EffectType::Invert,
            EffectType::BoxBlur,
            EffectType::UnsharpMask,
            EffectType::HueSaturation,
            EffectType::BlackAndWhite,
            EffectType::ShiftChannels,
            EffectType::DirectionalBlur,
            EffectType::ChannelMixer,
            EffectType::DropShadow,
            EffectType::FindEdges,
            EffectType::Emboss,
            EffectType::ColorEmboss,
            EffectType::Threshold,
            EffectType::Mosaic,
            EffectType::Posterize,
            EffectType::MotionTile,
            EffectType::Kaleidoscope,
            EffectType::Bevel,
            EffectType::Scatter,
            EffectType::BrushStrokes,
            EffectType::Tint,
            EffectType::Vibrance,
            EffectType::Tritone,
        ]
        .into_iter()
        .map(|effect_type| effect_type.key())
        .collect::<std::collections::BTreeSet<_>>();
        let actual = effect_library_types()
            .into_iter()
            .filter(|effect_type| !matches!(effect_type, EffectType::External(_)))
            .map(|effect_type| effect_type.key())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(actual, expected);
        for effect_type in builtin_effect_types() {
            assert!(
                effect_definition(&effect_type)
                    .expect("built-in definition")
                    .supports_visual_evaluation(),
                "{effect_type:?} must have an executable graph"
            );
        }
    }

    #[test]
    fn luma_key_keeps_bright_or_dark_pixels_with_original_alpha() {
        let mut effect = instantiate_effect_node(EffectType::LumaKey).expect("Luma Key definition");
        let input = [[0.02, 0.02, 0.02, 0.5], [2.0, 2.0, 2.0, 0.5]];
        let compiled = compile_clip_effect_graph(&[effect.clone()], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Luma Key");
        assert!(crate::compiled_effect_graph_supports_rgba_f32(&compiled));
        let output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            0,
        )
        .expect("execute Luma Key");
        assert!(output[0][3] <= 1.0e-6);
        assert!((output[1][3] - 0.5).abs() <= 1.0e-6);

        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::LumaKey.property_path("invert"),
                value: PropertyValue::Bool(true),
            })
            .expect("retain dark pixels");
        let inverted = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile inverted Luma Key");
        let output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &inverted,
            0,
        )
        .expect("execute inverted Luma Key");
        assert!((output[0][3] - 0.5).abs() <= 1.0e-6);
        assert!(output[1][3] <= 1.0e-6);
    }

    #[test]
    fn chroma_key_removes_bright_and_shadowed_green_without_keying_black() {
        let mut effect =
            instantiate_effect_node(EffectType::ChromaKey).expect("Chroma Key definition");
        let input = [
            [0.0, 0.1, 0.0, 0.5],
            [0.0, 2.0, 0.0, 0.5],
            [2.0, 0.0, 0.0, 0.5],
            [0.0, 0.0, 0.0, 0.5],
        ];
        let compiled = compile_clip_effect_graph(&[effect.clone()], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Chroma Key");
        assert!(crate::compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!(compiled
            .execution_envelope()
            .admit_single_frame_backend(
                crate::EffectProcessingBackend::Gpu,
                crate::EffectWorkingPrecision::Float32,
            )
            .is_err());
        let output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(4, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            0,
        )
        .expect("execute Chroma Key");
        assert!(output[0][3] <= 1.0e-6);
        assert!(output[1][3] <= 1.0e-6);
        assert!((output[2][3] - 0.5).abs() <= 1.0e-6);
        assert!((output[3][3] - 0.5).abs() <= 1.0e-6);

        let saved = serde_json::to_vec(&effect).expect("save Chroma Key author state");
        let reopened: EffectNode = serde_json::from_slice(&saved).expect("reopen Chroma Key");
        let reopened_graph = compile_clip_effect_graph(&[reopened], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile reopened Chroma Key");
        assert_eq!(
            crate::apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(4, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &input
                ),
                &reopened_graph,
                0
            )
            .expect("execute reopened Chroma Key"),
            output,
        );

        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::ChromaKey.property_path("invert"),
                value: PropertyValue::Bool(true),
            })
            .expect("keep key color");
        let inverted = compile_clip_effect_graph(&[effect.clone()], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile inverted Chroma Key");
        let output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(4, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &inverted,
            0,
        )
        .expect("execute inverted Chroma Key");
        assert!((output[0][3] - 0.5).abs() <= 1.0e-6);
        assert!(output[2][3] <= 1.0e-6);

        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::ChromaKey.property_path("key_color"),
                value: PropertyValue::Color(Color::BLACK),
            })
            .expect("author black key color");
        assert!(matches!(
            compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE),
            Err(EffectGraphBuildError::InvalidAuthorState { .. })
        ));
    }

    #[test]
    fn authored_hsl_runs_in_float_without_clipping_hdr_or_alpha() {
        let mut effect =
            instantiate_effect_node(EffectType::HueSaturationLightness).expect("HSL definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::HueSaturationLightness.property_path("hue"),
                value: PropertyValue::Float(180.0),
            })
            .expect("author HSL hue");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile HSL");
        assert!(crate::compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!(compiled
            .execution_envelope()
            .admit_single_frame_backend(
                crate::EffectProcessingBackend::Gpu,
                crate::EffectWorkingPrecision::Float32,
            )
            .is_err());
        assert_eq!(
            crate::apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[2.0, -1.0, -1.0, 0.4]]
                ),
                &compiled,
                0
            )
            .expect("execute HSL")
            .pixels(),
            vec![[-1.0, 2.0, 2.0, 0.4]],
        );
    }

    #[test]
    fn color_curves_serialize_rebuild_and_lower_with_stable_structured_parameters() {
        let mut effect = instantiate_effect_node(EffectType::Curves).expect("Curves definition");
        assert_eq!(effect.properties.iter().count(), 11);
        assert!(effect
            .properties
            .iter()
            .map(|(_, property)| property)
            .filter(|property| matches!(property.static_value(), PropertyValue::Curve(_)))
            .all(|property| !property.descriptor.schema.is_animatable));

        let mode_path = EffectType::Curves.property_path("mode");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: mode_path,
                value: PropertyValue::Enum("yrgb".to_owned()),
            })
            .expect("set YRGB mode");
        let master = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.02),
            NormalizedCurvePoint::new(0.4, 0.31),
            NormalizedCurvePoint::new(1.0, 0.96),
        ])
        .expect("master curve");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Curves.property_path("master"),
                value: PropertyValue::Curve(master.clone()),
            })
            .expect("set master curve");
        let hue_vs_saturation = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.5),
            NormalizedCurvePoint::new(0.5, 0.58),
            NormalizedCurvePoint::new(1.0, 0.5),
        ])
        .expect("secondary curve");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Curves.property_path("hue_vs_saturation"),
                value: PropertyValue::Curve(hue_vs_saturation.clone()),
            })
            .expect("set secondary curve");

        let original_addresses = effect
            .properties
            .iter()
            .map(|(_, property)| property)
            .map(|property| {
                (
                    property.track_id,
                    property.descriptor.parameter_id().clone(),
                    property.static_value().clone(),
                )
            })
            .collect::<Vec<_>>();
        let graph = build_effect_render_graph(&[effect.clone()], tt(0), TEST_WORKING_SPACE)
            .expect("Curves graph");
        let curves = graph
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: EffectRenderOp::ColorCurves { curves },
                    ..
                }
                | EffectGraphNodeKind::DomainEffect {
                    op: EffectRenderOp::ColorCurves { curves },
                    ..
                } => Some(curves),
                _ => None,
            })
            .expect("compiled ColorCurves op");
        assert_eq!(curves.mode(), crate::ColorCurvesMode::YRgb);
        assert!(!curves.secondary_identity());

        let encoded = serde_json::to_vec(&effect).expect("serialize Curves effect");
        let decoded: EffectNode = serde_json::from_slice(&encoded).expect("deserialize Curves");
        let decoded_addresses = decoded
            .properties
            .iter()
            .map(|(_, property)| property)
            .map(|property| {
                (
                    property.track_id,
                    property.descriptor.parameter_id().clone(),
                    property.static_value().clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(decoded_addresses, original_addresses);

        let rebuilt = build_effect_render_graph(&[decoded], tt(0), TEST_WORKING_SPACE)
            .expect("rebuilt Curves graph");
        let compiled = crate::compile_reference_render_graph(rebuilt).expect("compiled graph");
        let gpu_plan = crate::lower_effect_graph_to_gpu_plan(&compiled).expect("Curves GPU plan");
        assert!(matches!(
            gpu_plan.operations(),
            [crate::EffectGpuPointOp::ColorCurves { curves }]
                if curves.mode() == crate::ColorCurvesMode::YRgb
                    && !curves.secondary_identity()
        ));
        let rebuilt_curves = match &gpu_plan.operations()[0] {
            crate::EffectGpuPointOp::ColorCurves { curves } => curves,
            _ => unreachable!("asserted ColorCurves operation"),
        };
        assert_eq!(
            rebuilt_curves.semantic_fingerprint(),
            curves.semantic_fingerprint()
        );
        assert!(decoded_addresses
            .iter()
            .any(|(_, _, value)| value == &PropertyValue::Curve(master.clone())));
        assert!(decoded_addresses
            .iter()
            .any(|(_, _, value)| value == &PropertyValue::Curve(hue_vs_saturation.clone())));
    }

    #[test]
    fn qualifier_is_authorable_domain_typed_topology_sensitive_and_cpu_executable() {
        let mut effect = instantiate_effect_node(EffectType::Qualifier)
            .expect("registered Qualifier definition");
        let samples_id = EffectType::Qualifier.parameter_id("samples").expect("samples ID");
        let preview_id = EffectType::Qualifier.parameter_id("matte_preview").expect("preview ID");
        let samples_path = EffectType::Qualifier.property_path("samples");
        let preview_path = EffectType::Qualifier.property_path("matte_preview");
        let samples = QualifierSampleSet::new(vec![
            QualifierSample::new([0.0, 1.0, 0.0], QualifierSampleOperation::Include),
            QualifierSample::new([1.0, 0.0, 0.0], QualifierSampleOperation::Exclude),
        ])
        .expect("valid Include/Exclude samples");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: samples_path.clone(),
                value: PropertyValue::QualifierSamples(samples.clone()),
            })
            .expect("author Qualifier samples");
        let sample_property = effect.properties.property(&samples_path).expect("samples property");
        let sample_track_id = sample_property.track_id;
        assert_eq!(sample_property.descriptor.parameter_id(), &samples_id);
        assert!(!sample_property.descriptor.schema.is_animatable);
        assert_eq!(
            sample_property.descriptor.schema.cache_impact,
            ParameterCacheImpact::Value
        );
        let preview_property = effect.properties.property(&preview_path).expect("preview property");
        assert_eq!(preview_property.descriptor.parameter_id(), &preview_id);
        assert_eq!(
            preview_property.descriptor.schema.cache_impact,
            ParameterCacheImpact::Topology
        );

        let normal =
            build_effect_render_graph(&[effect.clone()], TimelineTime::ZERO, TEST_WORKING_SPACE)
                .expect("normal Qualifier graph");
        let qualifier = normal
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::DomainEffect {
                    op: EffectRenderOp::Qualifier { qualifier },
                    domain_contract,
                    ..
                } => Some((*qualifier.semantic_fingerprint(), *domain_contract, node.id)),
                _ => None,
            })
            .expect("Qualifier domain node");
        assert_ne!(qualifier.0, [0; 32]);
        assert_eq!(
            qualifier.1,
            EffectColorDomainContract {
                input: EffectColorDomain::SceneLinearRgb,
                output: EffectColorDomain::AlphaMask,
            }
        );
        assert!(matches!(
            normal.output.and_then(|output| normal.nodes.get(output.0 as usize)),
            Some(crate::graph::EffectGraphNode {
                kind: EffectGraphNodeKind::Mask { mask, .. },
                ..
            }) if *mask == qualifier.2
        ));
        let normal_compiled =
            crate::compile_reference_render_graph(normal).expect("compiled normal Qualifier graph");
        assert_eq!(
            normal_compiled.domain_plan().node_output_domains[&qualifier.2],
            EffectColorDomain::AlphaMask
        );
        assert!(normal_compiled.domain_plan().blockers.is_empty());

        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: preview_path,
                value: PropertyValue::Bool(true),
            })
            .expect("enable Matte preview");
        let preview =
            build_effect_render_graph(&[effect.clone()], TimelineTime::ZERO, TEST_WORKING_SPACE)
                .expect("Matte preview graph");
        assert!(matches!(
            preview.output.and_then(|output| preview.nodes.get(output.0 as usize)),
            Some(crate::graph::EffectGraphNode {
                kind: EffectGraphNodeKind::DomainEffect {
                    op: EffectRenderOp::MattePreview { .. },
                    domain_contract: EffectColorDomainContract {
                        input: EffectColorDomain::AlphaMask,
                        output: EffectColorDomain::SceneLinearRgb,
                    },
                    ..
                },
                ..
            })
        ));
        let preview_compiled =
            crate::compile_reference_render_graph(preview).expect("compiled Matte preview graph");
        assert_ne!(
            normal_compiled.semantic_fingerprint(),
            preview_compiled.semantic_fingerprint(),
            "Matte preview is a topology-changing observation route"
        );
        assert!(preview_compiled.domain_plan().blockers.is_empty());

        let input = [
            [0.0, 1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [-0.25, 4.0, 0.15, 0.5],
        ];
        let normal_output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &normal_compiled,
            0,
        )
        .expect("CPU Mask output");
        assert!(normal_output[0][3] > normal_output[1][3]);
        assert!(normal_output.iter().flatten().all(|channel| channel.is_finite()));
        let preview_output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &preview_compiled,
            0,
        )
        .expect("CPU Matte preview");
        assert!(preview_output
            .iter()
            .all(|pixel| { pixel[0] == pixel[1] && pixel[1] == pixel[2] && pixel[3] == 1.0 }));

        let encoded = serde_json::to_vec(&effect).expect("serialize Qualifier effect");
        let decoded: EffectNode = serde_json::from_slice(&encoded).expect("deserialize Qualifier");
        let decoded_property = decoded.properties.property(&samples_path).expect("decoded samples");
        assert_eq!(decoded.id, effect.id);
        assert_eq!(decoded_property.track_id, sample_track_id);
        assert_eq!(decoded_property.descriptor.parameter_id(), &samples_id);
        assert_eq!(
            decoded_property.static_value(),
            &PropertyValue::QualifierSamples(samples)
        );
        decoded.validate_author_state().expect("valid decoded Qualifier state");
        let rebuilt_graph =
            build_effect_render_graph(&[decoded], TimelineTime::ZERO, TEST_WORKING_SPACE)
                .expect("rebuilt Qualifier graph");
        let rebuilt = crate::compile_reference_render_graph(rebuilt_graph)
            .expect("compiled rebuilt Qualifier graph");
        assert_eq!(
            rebuilt.semantic_fingerprint(),
            preview_compiled.semantic_fingerprint()
        );
    }

    #[test]
    fn primary_grade_effects_animate_serialize_and_rebuild_identically() {
        let mut white_balance =
            instantiate_effect_node(EffectType::WhiteBalance).expect("White Balance definition");
        let temperature_path = EffectType::WhiteBalance.property_path("temperature");
        white_balance
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: temperature_path.clone(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(0.0)),
            })
            .expect("White Balance start");
        white_balance
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: temperature_path,
                keyframe: Keyframe::linear(tt(10), PropertyValue::Float(1.0)),
            })
            .expect("White Balance end");

        let mut primaries =
            instantiate_effect_node(EffectType::ColorWheel).expect("Primaries definition");
        let offset_path = EffectType::ColorWheel.property_path("offset");
        primaries
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: offset_path.clone(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Vec3(glam::Vec3::ZERO)),
            })
            .expect("Primaries start");
        primaries
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: offset_path,
                keyframe: Keyframe::linear(
                    tt(10),
                    PropertyValue::Vec3(glam::Vec3::new(0.2, -0.1, 0.4)),
                ),
            })
            .expect("Primaries end");

        let mut cdl = instantiate_effect_node(EffectType::AscCdl).expect("ASC CDL definition");
        let saturation_path = EffectType::AscCdl.property_path("saturation");
        cdl.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: saturation_path.clone(),
            keyframe: Keyframe::linear(tt(0), PropertyValue::Float(1.0)),
        })
        .expect("CDL start");
        cdl.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: saturation_path,
            keyframe: Keyframe::linear(tt(10), PropertyValue::Float(0.5)),
        })
        .expect("CDL end");

        let effects = vec![white_balance, primaries, cdl];
        let graph = build_effect_render_graph(&effects, tt(5), TEST_WORKING_SPACE)
            .expect("animated primary grade graph");
        let operations = graph
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect { op, .. }
                | EffectGraphNodeKind::DomainEffect { op, .. } => Some(op),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(operations[0], EffectRenderOp::WhiteBalance { .. }));
        let EffectRenderOp::Primaries { grade } = operations[1] else {
            panic!("expected animated Primaries operation");
        };
        assert_eq!(grade.offset(), [0.1, -0.05, 0.2]);
        let EffectRenderOp::AscCdl { grade } = operations[2] else {
            panic!("expected animated ASC CDL operation");
        };
        assert!((grade.saturation() - 0.75).abs() <= 1.0e-6);

        let encoded = serde_json::to_vec(&effects).expect("serialize primary effects");
        let decoded: Vec<EffectNode> =
            serde_json::from_slice(&encoded).expect("deserialize primary effects");
        let rebuilt = build_effect_render_graph(&decoded, tt(5), TEST_WORKING_SPACE)
            .expect("rebuild serialized primary grade graph");
        assert_eq!(
            EffectRenderPlan { ops: operations.into_iter().cloned().collect() }.signature_hash(),
            EffectRenderPlan {
                ops: rebuilt
                    .nodes
                    .iter()
                    .filter_map(|node| match &node.kind {
                        EffectGraphNodeKind::UnaryEffect { op, .. }
                        | EffectGraphNodeKind::DomainEffect { op, .. } => Some(op.clone()),
                        _ => None,
                    })
                    .collect(),
            }
            .signature_hash()
        );
    }

    #[test]
    fn gamut_and_highlight_effects_animate_serialize_and_execute_float32() {
        let mut gamut =
            instantiate_effect_node(EffectType::GamutCompression).expect("gamut definition");
        let amount_path = EffectType::GamutCompression.property_path("amount");
        gamut
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: amount_path.clone(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(0.0)),
            })
            .expect("gamut start");
        gamut
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: amount_path,
                keyframe: Keyframe::linear(tt(10), PropertyValue::Float(1.0)),
            })
            .expect("gamut end");

        let mut highlight =
            instantiate_effect_node(EffectType::HighlightRecovery).expect("highlight definition");
        let strength_path = EffectType::HighlightRecovery.property_path("strength");
        highlight
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: strength_path,
                value: PropertyValue::Float(0.75),
            })
            .expect("highlight strength");

        let effects = vec![gamut, highlight];
        let graph = build_effect_render_graph(&effects, tt(5), WorkingColorSpace::LinearRec2020)
            .expect("gamut/highlight graph");
        let compiled = crate::compile_reference_render_graph(graph).expect("compiled graph");
        let input = [[3.0, -0.4, 0.2, 0.35], [0.5, 0.25, 0.1, 0.8]];
        let output = crate::apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            0,
        )
        .expect("float32 execution");
        assert_eq!(output[0][3], input[0][3]);
        assert_eq!(output[1][3], input[1][3]);
        assert_ne!(output[0][..3], input[0][..3]);
        assert_eq!(output[1], input[1], "below-threshold pixel stays exact");
        assert!(output.iter().flatten().all(|value| value.is_finite()));

        let encoded = serde_json::to_vec(&effects).expect("serialize effects");
        let decoded: Vec<EffectNode> =
            serde_json::from_slice(&encoded).expect("deserialize effects");
        let rebuilt = crate::compile_reference_render_graph(
            build_effect_render_graph(&decoded, tt(5), WorkingColorSpace::LinearRec2020)
                .expect("rebuilt graph"),
        )
        .expect("compiled rebuilt graph");
        assert_eq!(
            compiled.semantic_fingerprint(),
            rebuilt.semantic_fingerprint()
        );
    }

    #[test]
    fn crop_author_parameters_are_animatable_and_compile_to_normalized_insets() {
        let mut effect =
            instantiate_effect_node(EffectType::Crop).expect("registered Crop definition");
        let left_path = EffectType::Crop.property_path("left");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: left_path.clone(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(0.0)),
            })
            .expect("set Crop start keyframe");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: left_path,
                keyframe: Keyframe::linear(tt(10), PropertyValue::Float(50.0)),
            })
            .expect("set Crop end keyframe");

        let graph = build_effect_render_graph(&[effect], tt(5), TEST_WORKING_SPACE)
            .expect("evaluate animated Crop");
        let crop = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Crop { left, top, right, bottom },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Crop { left, top, right, bottom },
                ..
            } => Some((*left, *top, *right, *bottom)),
            _ => None,
        });

        assert_eq!(crop, Some((0.25, 0.0, 0.0, 0.0)));
    }

    #[test]
    fn invert_defaults_compile_to_rgb_channel_inversion() {
        let effect =
            instantiate_effect_node(EffectType::Invert).expect("registered Invert definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Invert");

        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Invert { channels, blend_with_original },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Invert { channels, blend_with_original },
                ..
            } => Some((*channels, *blend_with_original)),
            _ => None,
        });

        assert_eq!(op, Some((crate::InvertChannels::Rgb, 0.0)));
    }

    #[test]
    fn invert_channel_scope_is_authorable_and_reaches_the_op() {
        let mut effect =
            instantiate_effect_node(EffectType::Invert).expect("registered Invert definition");
        let channels_path = EffectType::Invert.property_path("channels");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: channels_path,
                value: PropertyValue::Enum("hue".to_string()),
            })
            .expect("author the hue channel scope");

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate Invert with the hue scope");
        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Invert { channels, .. },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Invert { channels, .. },
                ..
            } => Some(*channels),
            _ => None,
        });

        assert_eq!(op, Some(crate::InvertChannels::Hue));
    }

    #[test]
    fn invert_mix_is_authored_in_percent_and_animates_into_the_op() {
        let mut effect =
            instantiate_effect_node(EffectType::Invert).expect("registered Invert definition");
        let blend_path = EffectType::Invert.property_path("blend_with_original");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: blend_path.clone(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(0.0)),
            })
            .expect("set Invert blend start keyframe");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: blend_path,
                keyframe: Keyframe::linear(tt(10), PropertyValue::Float(100.0)),
            })
            .expect("set Invert blend end keyframe");

        let graph = build_effect_render_graph(&[effect], tt(5), TEST_WORKING_SPACE)
            .expect("evaluate animated Invert blend");
        let blend = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Invert { blend_with_original, .. },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Invert { blend_with_original, .. },
                ..
            } => Some(*blend_with_original),
            _ => None,
        });

        assert_eq!(blend, Some(0.5));
    }

    #[test]
    fn invert_full_original_blend_compiles_to_identity() {
        let mut effect =
            instantiate_effect_node(EffectType::Invert).expect("registered Invert definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Invert.property_path("blend_with_original"),
                value: PropertyValue::Float(100.0),
            })
            .expect("author a full original-image blend");

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate fully blended Invert");

        assert!(
            !graph.nodes.iter().any(|node| matches!(
                &node.kind,
                EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Invert { .. }, .. }
            )),
            "a fully retained original image must not emit an Invert operation"
        );
    }

    #[test]
    fn invert_channel_menu_exposes_sixteen_distinct_scopes() {
        let definition = effect_definition(&EffectType::Invert).expect("Invert definition");
        let channels_id = builtin_parameter_id(&EffectType::Invert, "channels");
        let channels = definition
            .default_properties()
            .iter()
            .find(|(_, property)| property.descriptor.parameter_id() == &channels_id)
            .expect("Invert channel property");

        let options = &channels.1.descriptor.schema.enum_options;
        let keys: Vec<&str> = options.iter().map(|option| option.key.as_str()).collect();

        assert_eq!(keys.len(), crate::InvertChannels::ALL.len());
        assert_eq!(keys.len(), 16);
        for scope in crate::InvertChannels::ALL {
            assert!(
                keys.contains(&scope.key()),
                "missing invert channel scope `{}`",
                scope.key()
            );
        }
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "invert channel scopes must be unique"
        );
    }

    #[test]
    fn invert_executes_channel_inversion_on_scene_linear_float_frames() {
        let mut effect =
            instantiate_effect_node(EffectType::Invert).expect("registered Invert definition");
        let input = [[0.25, 2.0, -0.5, 0.75], [0.0, 0.0, 0.0, 0.0]];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect.clone()], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile default Invert");
        assert!(crate::compiled_effect_graph_supports_rgba_f32(&compiled));
        let output = crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0)
            .expect("execute default Invert");
        assert!((output[0][0] - 0.75).abs() <= 1.0e-6);
        assert!((output[0][1] + 1.0).abs() <= 1.0e-6);
        assert!((output[0][2] - 1.5).abs() <= 1.0e-6);
        assert!((output[0][3] - 0.75).abs() <= 1.0e-6);
        assert_eq!(
            output[1],
            [0.0, 0.0, 0.0, 0.0],
            "pixels without positive coverage stay untouched"
        );

        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Invert.property_path("channels"),
                value: PropertyValue::Enum("alpha".to_string()),
            })
            .expect("author the alpha channel scope");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Invert.property_path("blend_with_original"),
                value: PropertyValue::Float(50.0),
            })
            .expect("author a half blend");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile alpha Invert");
        let output = crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0)
            .expect("execute alpha Invert");
        assert!((output[0][0] - 0.25).abs() <= 1.0e-6);
        assert!((output[0][3] - 0.5).abs() <= 1.0e-6);
    }

    #[test]
    fn invert_authored_state_survives_project_serialization_round_trip() {
        let mut effect =
            instantiate_effect_node(EffectType::Invert).expect("registered Invert definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Invert.property_path("channels"),
                value: PropertyValue::Enum("green_blue".to_string()),
            })
            .expect("author the green/blue channel scope");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Invert.property_path("blend_with_original"),
                value: PropertyValue::Float(40.0),
            })
            .expect("author a 40 percent blend");

        let wire = serde_json::to_string(&effect).expect("serialize the Invert node");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restore the Invert node");
        assert_eq!(restored.effect_type, EffectType::Invert);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Invert node");
        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Invert { channels, blend_with_original },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Invert { channels, blend_with_original },
                ..
            } => Some((*channels, (*blend_with_original * 100.0).round() as i32)),
            _ => None,
        });
        assert_eq!(op, Some((crate::InvertChannels::GreenBlue, 40)));
    }

    #[test]
    fn box_blur_defaults_compile_to_identity() {
        let effect =
            instantiate_effect_node(EffectType::BoxBlur).expect("registered Box Blur definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Box Blur");

        assert!(
            !graph.nodes.iter().any(|node| matches!(
                &node.kind,
                EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::BoxBlur { .. }, .. }
            )),
            "a zero authored radius is the identity and must not emit an operation"
        );
    }

    #[test]
    fn box_blur_authored_state_reaches_the_operation() {
        let mut effect =
            instantiate_effect_node(EffectType::BoxBlur).expect("registered Box Blur definition");
        for (parameter, value) in [
            ("radius", PropertyValue::Float(4.0)),
            ("iterations", PropertyValue::Int(2)),
            ("direction", PropertyValue::Enum("horizontal".to_string())),
            ("repeat_edge_pixels", PropertyValue::Bool(false)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BoxBlur.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Box Blur {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Box Blur");
        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::BoxBlur { radius, iterations, direction, repeat_edge_pixels },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::BoxBlur { radius, iterations, direction, repeat_edge_pixels },
                ..
            } => Some((*radius, *iterations, *direction, *repeat_edge_pixels)),
            _ => None,
        });

        assert_eq!(
            op,
            Some((4.0, 2, crate::BoxBlurDirection::Horizontal, false))
        );
    }

    #[test]
    fn box_blur_rejects_author_values_outside_the_declared_ranges() {
        let mut effect =
            instantiate_effect_node(EffectType::BoxBlur).expect("registered Box Blur definition");
        assert!(
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BoxBlur.property_path("radius"),
                    value: PropertyValue::Float(crate::BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS + 1.0),
                })
                .is_err(),
            "the numeric contract rejects a radius above the authored maximum"
        );
        assert!(
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BoxBlur.property_path("iterations"),
                    value: PropertyValue::Int(0),
                })
                .is_err(),
            "the numeric contract rejects fewer than one iteration"
        );
    }

    #[test]
    fn box_blur_executes_a_normalized_average_on_scene_linear_float_frames() {
        let mut effect =
            instantiate_effect_node(EffectType::BoxBlur).expect("registered Box Blur definition");
        for (parameter, value) in [
            ("radius", PropertyValue::Float(1.0)),
            ("iterations", PropertyValue::Int(1)),
            ("direction", PropertyValue::Enum("horizontal".to_string())),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BoxBlur.property_path(parameter),
                    value,
                })
                .expect("author Box Blur");
        }

        let input = [
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 2.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 4.0, 0.0, 1.0],
        ];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(4, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Box Blur");
        assert!(crate::compiled_effect_graph_supports_rgba_f32(&compiled));
        let output = crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0)
            .expect("execute Box Blur");

        // A radius-one horizontal pass averages three samples with replicated
        // edges, and values above one pass through unclamped.
        for (index, expected) in [2.0 / 3.0, 2.0 / 3.0, 2.0, 8.0 / 3.0].into_iter().enumerate() {
            assert!(
                (output[index][1] - expected).abs() <= 1.0e-6,
                "{index}: {:?}",
                output[index]
            );
            assert!(
                (output[index][3] - 1.0).abs() <= 1.0e-6,
                "{index}: {:?}",
                output[index]
            );
        }
    }

    #[test]
    fn box_blur_authored_state_survives_project_serialization_round_trip() {
        let mut effect =
            instantiate_effect_node(EffectType::BoxBlur).expect("registered Box Blur definition");
        for (parameter, value) in [
            ("radius", PropertyValue::Float(6.5)),
            ("iterations", PropertyValue::Int(5)),
            ("direction", PropertyValue::Enum("vertical".to_string())),
            ("repeat_edge_pixels", PropertyValue::Bool(false)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BoxBlur.property_path(parameter),
                    value,
                })
                .expect("author Box Blur");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Box Blur node");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restore the Box Blur node");
        assert_eq!(restored.effect_type, EffectType::BoxBlur);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Box Blur node");
        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::BoxBlur { radius, iterations, direction, repeat_edge_pixels },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::BoxBlur { radius, iterations, direction, repeat_edge_pixels },
                ..
            } => Some((*radius, *iterations, *direction, *repeat_edge_pixels)),
            _ => None,
        });
        assert_eq!(op, Some((6.5, 5, crate::BoxBlurDirection::Vertical, false)));
    }

    fn unsharp_mask_op(graph: &EffectRenderGraph) -> Option<(f32, f32, f32)> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::UnsharpMask { gain, radius, threshold },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::UnsharpMask { gain, radius, threshold },
                ..
            } => Some((*gain, *radius, *threshold)),
            _ => None,
        })
    }

    #[test]
    fn directional_blur_defaults_compile_to_identity() {
        let effect = instantiate_effect_node(EffectType::DirectionalBlur)
            .expect("registered Directional Blur definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Directional Blur");

        assert!(
            !graph.nodes.iter().any(|node| matches!(
                &node.kind,
                EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::DirectionalBlur { .. }, .. }
            )),
            "a zero authored length is the identity and must not emit an operation"
        );
    }

    #[test]
    fn directional_blur_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::DirectionalBlur)
            .expect("registered Directional Blur definition");
        for (parameter, value) in [
            ("length", PropertyValue::Float(6.5)),
            ("direction", PropertyValue::Float(30.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::DirectionalBlur.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Directional Blur {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Directional Blur");
        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::DirectionalBlur { length, direction_degrees },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::DirectionalBlur { length, direction_degrees },
                ..
            } => Some((*length, *direction_degrees)),
            _ => None,
        });

        assert_eq!(op, Some((6.5, 30.0)));
    }

    #[test]
    fn directional_blur_rejects_author_values_outside_the_declared_range() {
        let mut effect = instantiate_effect_node(EffectType::DirectionalBlur)
            .expect("registered Directional Blur definition");
        for length in [crate::DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS + 1.0, -1.0] {
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::DirectionalBlur.property_path("length"),
                        value: PropertyValue::Float(length),
                    })
                    .is_err(),
                "the numeric contract rejects length {length}"
            );
        }
    }

    #[test]
    fn directional_blur_smears_along_the_authored_direction_only() {
        let mut effect = instantiate_effect_node(EffectType::DirectionalBlur)
            .expect("registered Directional Blur definition");
        for (parameter, value) in [
            ("length", PropertyValue::Float(2.0)),
            ("direction", PropertyValue::Float(90.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::DirectionalBlur.property_path(parameter),
                    value,
                })
                .expect("author Directional Blur");
        }

        let mut input = [[0.0_f32, 0.0, 0.0, 1.0]; 25];
        input[2 * 5 + 2] = [1.0, 1.0, 1.0, 1.0];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(5, 5),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Directional Blur");
        let output = crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0)
            .expect("execute Directional Blur");
        let pixels = output.pixels();

        // A vertical smear of length two reads half a pixel above and below each sample, so the
        // impulse spreads over its own column and never into the columns beside it.
        assert!(pixels[2 * 5 + 2][0] > 0.0, "the impulse stays in place");
        for row in 1..=3 {
            assert!(
                pixels[row * 5 + 2][0] > 0.0,
                "row {row} of the smeared column"
            );
        }
        assert_eq!(pixels[2][0], 0.0, "the smear stops halfway into row zero");
        assert_eq!(pixels[4 * 5 + 2][0], 0.0, "the smear stops before row four");
        for row in 0..5 {
            for column in [0, 1, 3, 4] {
                assert_eq!(
                    pixels[row * 5 + column][0],
                    0.0,
                    "row {row} column {column} must not receive the vertical smear"
                );
            }
        }
        assert!(
            (pixels[2 * 5 + 2][3] - 1.0).abs() < 1.0e-6,
            "fully covered columns keep their coverage"
        );
    }

    #[test]
    fn directional_blur_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::DirectionalBlur)
            .expect("registered Directional Blur definition");
        for (parameter, value) in [
            ("length", PropertyValue::Float(12.5)),
            ("direction", PropertyValue::Float(135.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::DirectionalBlur.property_path(parameter),
                    value,
                })
                .expect("author Directional Blur");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Directional Blur node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Directional Blur node");
        assert_eq!(restored.effect_type, EffectType::DirectionalBlur);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Directional Blur node");
        let op = graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::DirectionalBlur { length, direction_degrees },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::DirectionalBlur { length, direction_degrees },
                ..
            } => Some((*length, *direction_degrees)),
            _ => None,
        });
        assert_eq!(op, Some((12.5, 135.0)));
    }

    #[test]
    fn unsharp_mask_defaults_compile_to_the_authored_default_gain() {
        let effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default Unsharp Mask");

        // The list default is 50% amount at a 1 px radius and no threshold, so
        // the default state is a real operation rather than the identity.
        assert_eq!(unsharp_mask_op(&graph), Some((0.5, 1.0, 0.0)));
    }

    #[test]
    fn unsharp_mask_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        for (parameter, value) in [
            ("amount", PropertyValue::Float(150.0)),
            ("radius", PropertyValue::Float(3.5)),
            ("threshold", PropertyValue::Float(0.25)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::UnsharpMask.property_path(parameter),
                    value,
                })
                .unwrap_or_else(|error| panic!("author Unsharp Mask {parameter}: {error}"));
        }

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored Unsharp Mask");
        assert_eq!(unsharp_mask_op(&graph), Some((1.5, 3.5, 0.25)));
    }

    #[test]
    fn unsharp_mask_zero_amount_compiles_to_identity() {
        let mut effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::UnsharpMask.property_path("amount"),
                value: PropertyValue::Float(0.0),
            })
            .expect("author zero amount");

        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate zero-amount Unsharp Mask");
        assert_eq!(unsharp_mask_op(&graph), None);
    }

    #[test]
    fn unsharp_mask_rejects_author_values_outside_the_declared_ranges() {
        let effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        for (parameter, value) in [
            ("amount", PropertyValue::Float(501.0)),
            ("radius", PropertyValue::Float(600.0)),
            ("threshold", PropertyValue::Float(1.5)),
        ] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::UnsharpMask.property_path(parameter),
                        value,
                    })
                    .is_err(),
                "the numeric contract rejects {parameter} outside its authored range"
            );
        }
    }

    #[test]
    fn unsharp_mask_leaves_a_flat_frame_unchanged() {
        let mut effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        for (parameter, value) in [
            ("amount", PropertyValue::Float(100.0)),
            ("radius", PropertyValue::Float(6.0)),
            ("threshold", PropertyValue::Float(0.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::UnsharpMask.property_path(parameter),
                    value,
                })
                .expect("author Unsharp Mask");
        }

        let input = [[0.25, 0.25, 0.25, 1.0]; 8];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(8, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Unsharp Mask");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");
        for pixel in output.pixels() {
            assert!(
                (pixel[0] - 0.25).abs() <= 1.0e-6,
                "a flat frame has no difference to amplify: {pixel:?}"
            );
            assert!((pixel[3] - 1.0).abs() <= 1.0e-6, "{pixel:?}");
        }
    }

    #[test]
    fn unsharp_mask_amplifies_the_peak_of_an_impulse_without_clamping() {
        let mut effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        for (parameter, value) in [
            ("amount", PropertyValue::Float(100.0)),
            ("radius", PropertyValue::Float(6.0)),
            ("threshold", PropertyValue::Float(0.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::UnsharpMask.property_path(parameter),
                    value,
                })
                .expect("author Unsharp Mask");
        }

        let mut input = [[0.0, 0.0, 0.0, 1.0]; 9];
        input[4] = [1.0, 1.0, 1.0, 1.0];
        let view = || {
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(9, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            )
        };

        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile Unsharp Mask");
        let output =
            crate::apply_compiled_effect_graph_rgba_f32(view(), &compiled, 0).expect("execute");

        // The Gaussian-blurred peak stays below the source peak, so a 100%
        // amount pushes the sharpened peak above the scene-linear range instead
        // of clamping it back to one.
        assert!(
            output[4][0] > 1.0 + 1.0e-3,
            "the isolated peak must be amplified: {:?}",
            output[4]
        );
        for pixel in output.pixels() {
            assert!(pixel[0].is_finite() && pixel[3] == 1.0, "{pixel:?}");
        }
    }

    #[test]
    fn unsharp_mask_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::UnsharpMask)
            .expect("registered Unsharp Mask definition");
        for (parameter, value) in [
            ("amount", PropertyValue::Float(150.0)),
            ("radius", PropertyValue::Float(3.5)),
            ("threshold", PropertyValue::Float(0.25)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::UnsharpMask.property_path(parameter),
                    value,
                })
                .expect("author Unsharp Mask");
        }

        let wire = serde_json::to_string(&effect).expect("serialize the Unsharp Mask node");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restore the Unsharp Mask node");
        assert_eq!(restored.effect_type, EffectType::UnsharpMask);
        assert_eq!(restored, effect);

        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate the restored Unsharp Mask node");
        assert_eq!(unsharp_mask_op(&graph), Some((1.5, 3.5, 0.25)));
    }

    #[test]
    fn builtin_spatial_roi_contract_tracks_author_and_kernel_bounds() {
        let blur = effect_definition(&EffectType::GaussianBlur).expect("blur definition");
        let radius_id = builtin_parameter_id(&EffectType::GaussianBlur, "radius");
        let authored_max = blur
            .default_properties()
            .iter()
            .find(|(_, property)| property.descriptor.parameter_id() == &radius_id)
            .and_then(|(_, property)| {
                property
                    .descriptor
                    .schema
                    .numeric
                    .as_ref()
                    .and_then(|contracts| contracts.first())
                    .copied()
            })
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
    fn foreign_and_ownerless_lut_paths_require_author_repair() {
        #[cfg(windows)]
        let foreign = mondrian_core::NativePath::from_unix_bytes(b"/studio/look.cube".to_vec());
        #[cfg(not(windows))]
        let foreign = mondrian_core::NativePath::from_windows_units(
            r"C:\studio\look.cube".encode_utf16().collect(),
        );
        for path in [foreign, std::path::PathBuf::from("relative.cube").into()] {
            let mut effect = EffectNode::with_defaults(EffectType::Lut3D);
            effect
                .set_static_value_by_parameter(
                    &EffectType::Lut3D.parameter_id("processing_space").expect("id"),
                    PropertyValue::Enum("scene_linear".into()),
                )
                .expect("space");
            effect
                .set_static_value_by_parameter(
                    &EffectType::Lut3D.parameter_id("path").expect("id"),
                    PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }),
                )
                .expect("preserve author intent");
            let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
                .expect_err("must not open local CWD");
            assert!(matches!(
                error,
                EffectGraphBuildError::ResourceUnavailable {
                    recovery: EffectResourceRecovery::AuthorEdit,
                    ..
                }
            ));
            assert!(!error.dependency_refresh_retryable());
        }
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
                PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                    path: path.into(),
                }),
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
    fn enabled_unknown_extension_effect_fails_instead_of_rendering_identity() {
        let effect = EffectNode::new(EffectType::External(
            "plugin.missing.persisted-definition".to_string(),
        ));

        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("missing extension definition must not render as identity");

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
                    path: path.clone().into(),
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
    fn extension_can_register_custom_effect_property() {
        let extension_type = EffectType::External("plugin.ai.auto_exposure".to_string());
        let exposure_path = extension_type.property_path("exposure");
        let exposure_id = extension_type.parameter_id("exposure").expect("parameter ID");
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
                extension_type.key(),
                "AI 自动曝光",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(test_extension_execution_contract())
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

        let mut effect = EffectNode::with_defaults(extension_type.clone());
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: exposure_path.clone(),
                value: PropertyValue::Float(0.85),
            })
            .expect("set extension property");

        let value = effect
            .evaluate_property(&exposure_path, tt(0))
            .and_then(|value| value.as_f32())
            .expect("read extension property");
        assert!((value - 0.85).abs() < 0.001);

        // Verify the effect produces a graph node
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("build extension graph");
        assert!(!graph.nodes.is_empty());
        assert_eq!(
            effect_display_name(&extension_type),
            "AI 自动曝光".to_string()
        );
    }

    #[test]
    fn extension_can_build_custom_render_op_plan() {
        let extension_type = EffectType::External("plugin.render.glow".to_string());
        let amount_id = extension_type.parameter_id("amount").expect("parameter ID");
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
                extension_type.key(),
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

        let effect = EffectNode::with_defaults(extension_type);
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
                ..test_extension_execution_contract()
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
                ..test_extension_execution_contract()
            },
            EffectRenderOp::Grain { amount: 0.5 },
        );
        assert!(matches!(
            determinism,
            crate::EffectExecutionContractViolation::DeterminismTooOptimistic { .. }
        ));

        let roi = emitted_op_contract_violation(
            "plugin.contract.roi",
            test_extension_execution_contract(),
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
                ..test_extension_execution_contract()
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
                ..test_extension_execution_contract()
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
    fn extension_can_build_branching_render_graph() {
        let extension_type = EffectType::External("plugin.graph.glow_mix".to_string());
        let radius_id = extension_type.parameter_id("radius").expect("radius ID");
        let opacity_id = extension_type.parameter_id("opacity").expect("opacity ID");
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
                extension_type.key(),
                "Glow Mix",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                roi_propagation: crate::EffectRoiPropagation::Expand {
                    horizontal_pixels: 4,
                    vertical_pixels: 4,
                },
                ..test_extension_execution_contract()
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

        let effect = EffectNode::with_defaults(extension_type.clone());
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

        let definition = effect_definition(&extension_type).expect("effect definition");
        let contract = definition.execution_contract();
        assert_eq!(contract.topology, EffectGraphTopology::GeneralDag);
        assert!(contract.execution_modes.contains(
            crate::EffectProcessingBackend::Cpu,
            crate::EffectWorkingPrecision::Float32,
        ));
    }

    #[test]
    fn custom_render_backend_can_provide_stable_cache_key_contract() {
        let extension_type = EffectType::External("plugin.render.lut_loader".to_string());
        let path_id = extension_type.parameter_id("asset_path").expect("asset path ID");
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
                extension_type.key(),
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

        let effect = EffectNode::with_defaults(extension_type.clone());
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

        let contract = effect_definition(&extension_type)
            .expect("effect definition")
            .execution_contract();
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
    fn failing_extension_graph_builder_isolated_and_hidden_after_disable_policy() {
        let extension_type = EffectType::External("plugin.graph.unstable".to_string());
        register_effect_definition(
            EffectDefinition::new(
                extension_type.key(),
                "Unstable Graph",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(test_extension_execution_contract())
            .with_extension_contract(
                crate::EffectExtensionContract::new("1.0.0")
                    .with_runtime_failure_policy(
                        crate::EffectExtensionRuntimeFailurePolicy::DisableDefinition,
                    )
                    .with_library_policy(crate::EffectExtensionLibraryPolicy::HideWhenUnavailable),
            )
            .with_graph_builder(Arc::new(|_, _, _| {
                panic!("unstable graph builder");
            })),
        )
        .expect("register unstable definition");

        let effect = EffectNode::new(extension_type.clone());
        let error = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect_err("builder must fail");
        assert!(matches!(
            error,
            EffectGraphBuildError::BuilderPanicked { .. }
        ));

        let status = crate::effect_extension_runtime_status(&extension_type.key())
            .expect("extension runtime status");
        assert!(status.disabled);
        assert_eq!(
            status.last_error.as_deref(),
            Some("effect graph builder panicked")
        );
        assert!(!effect_library_types().contains(&extension_type));
    }

    fn emboss_view(input: &[[f32; 4]]) -> crate::EffectFloatFrameView<'_> {
        crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(input.len() as u32, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            input,
        )
    }

    #[test]
    fn emboss_defaults_compile_to_a_visible_relief() {
        let effect =
            instantiate_effect_node(EffectType::Emboss).expect("registered emboss definition");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile emboss");
        let input = [[0.25, 0.25, 0.25, 1.0]; 3];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute emboss");
        let pixels = output.pixels();
        assert_eq!(pixels.len(), 3);
        // A flat frame has no gradient, so the default relief is the neutral mid grey.
        for pixel in pixels {
            for channel in 0..3 {
                assert!(
                    (pixel[channel] - 0.5).abs() <= 1.0e-6,
                    "flat frame produced {pixel:?}"
                );
            }
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn emboss_full_mix_compiles_to_identity() {
        let mut effect =
            instantiate_effect_node(EffectType::Emboss).expect("registered emboss definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Emboss.property_path("blend_with_original"),
                value: PropertyValue::Float(1.0),
            })
            .expect("author a full source mix");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile emboss identity");
        let input = [[0.1, 0.4, 0.9, 0.5], [1.5, 0.2, 0.3, 1.0]];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute emboss identity");
        assert_eq!(output.pixels(), &input[..]);
    }

    #[test]
    fn emboss_rejects_author_values_outside_the_declared_ranges() {
        let effect =
            instantiate_effect_node(EffectType::Emboss).expect("registered emboss definition");
        for (parameter, value) in [
            ("relief", PropertyValue::Float(1_001.0)),
            ("relief", PropertyValue::Float(-1.0)),
            ("contrast", PropertyValue::Float(-1.0)),
            ("blend_with_original", PropertyValue::Float(1.5)),
        ] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::Emboss.property_path(parameter),
                        value,
                    })
                    .is_err(),
                "the numeric contract rejects {parameter} outside its authored range"
            );
        }
    }

    #[test]
    fn emboss_relief_follows_the_authored_direction_without_clamping() {
        let mut effect =
            instantiate_effect_node(EffectType::Emboss).expect("registered emboss definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Emboss.property_path("blend_with_original"),
                value: PropertyValue::Float(0.5),
            })
            .expect("author a half source mix");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile emboss");
        let input = [
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [2.0, 2.0, 2.0, 1.0],
        ];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute emboss");
        let pixels = output.pixels();
        // A step from black to an extended-range white lights the middle pixel above the
        // neutral 0.5 relief, and half of the 2.0 source keeps the result above 1.0.
        assert!(
            pixels[1][0] > 0.5 + 1.0e-3,
            "expected a lit ramp, got {:?}",
            pixels[1]
        );
        assert!(
            pixels[2][0] > 1.0,
            "extended-range source must not be clamped, got {:?}",
            pixels[2]
        );
        assert_eq!(pixels[2][3], 1.0);
    }

    #[test]
    fn emboss_authored_state_survives_project_serialization_round_trip() {
        let mut effect =
            instantiate_effect_node(EffectType::Emboss).expect("registered emboss definition");
        for (parameter, value) in [
            ("direction", PropertyValue::Float(135.0)),
            ("relief", PropertyValue::Float(12.5)),
            ("contrast", PropertyValue::Float(240.0)),
            ("blend_with_original", PropertyValue::Float(0.35)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Emboss.property_path(parameter),
                    value,
                })
                .expect("author emboss state");
        }
        let encoded = serde_json::to_string(&effect).expect("serialize emboss state");
        let restored: EffectNode =
            serde_json::from_str(&encoded).expect("deserialize emboss state");
        assert_eq!(restored, effect);
    }
    fn color_emboss_op(graph: &EffectRenderGraph) -> Option<crate::ColorEmbossGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::ColorEmboss { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::ColorEmboss { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn color_emboss_defaults_compile_to_a_visible_relief() {
        let effect = instantiate_effect_node(EffectType::ColorEmboss)
            .expect("registered color emboss definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default color emboss");
        let grade = color_emboss_op(&graph).expect("the listed defaults produce a relief");
        assert_eq!(grade.direction_degrees(), 45.0);
        assert_eq!(grade.relief(), 1.0);
        assert_eq!(grade.contrast_percent(), 100.0);
        assert_eq!(grade.blend_with_original(), 0.0);
    }

    #[test]
    fn color_emboss_full_mix_compiles_to_identity() {
        let mut effect = instantiate_effect_node(EffectType::ColorEmboss)
            .expect("registered color emboss definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::ColorEmboss.property_path("blend_with_original"),
                value: PropertyValue::Float(100.0),
            })
            .expect("author a full source mix");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate identity color emboss");
        assert!(color_emboss_op(&graph).is_none());
    }

    #[test]
    fn color_emboss_rejects_author_values_outside_the_declared_ranges() {
        let effect = instantiate_effect_node(EffectType::ColorEmboss)
            .expect("registered color emboss definition");
        for (parameter, value) in [
            ("relief", PropertyValue::Float(1_001.0)),
            ("relief", PropertyValue::Float(-1.0)),
            ("contrast", PropertyValue::Float(-1.0)),
            ("blend_with_original", PropertyValue::Float(101.0)),
        ] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::ColorEmboss.property_path(parameter),
                        value,
                    })
                    .is_err(),
                "the numeric contract rejects {parameter} outside its authored range"
            );
        }
    }

    #[test]
    fn color_emboss_relief_keeps_channels_independent() {
        let mut effect = instantiate_effect_node(EffectType::ColorEmboss)
            .expect("registered color emboss definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::ColorEmboss.property_path("relief"),
                value: PropertyValue::Float(4.0),
            })
            .expect("author a taller relief");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile color emboss");
        let input = [
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        ];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute color emboss");
        let pixels = output.pixels();
        // Only the red channel carries the step, so green and blue stay neutral.
        assert!(
            pixels[1][0] > 0.5,
            "expected a lit red relief, got {:?}",
            pixels[1]
        );
        assert_eq!(pixels[1][1], 0.5);
        assert_eq!(pixels[1][2], 0.5);
        assert_eq!(pixels[1][3], 1.0);
    }

    #[test]
    fn color_emboss_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::ColorEmboss)
            .expect("registered color emboss definition");
        for (parameter, value) in [
            ("direction", PropertyValue::Float(120.0)),
            ("relief", PropertyValue::Float(2.5)),
            ("contrast", PropertyValue::Float(150.0)),
            ("blend_with_original", PropertyValue::Float(25.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::ColorEmboss.property_path(parameter),
                    value,
                })
                .expect("author color emboss");
        }
        let encoded = serde_json::to_string(&effect).expect("serialize color emboss");
        let restored: crate::EffectNode =
            serde_json::from_str(&encoded).expect("deserialize color emboss");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate restored color emboss");
        let grade = color_emboss_op(&graph).expect("the restored state produces a relief");
        assert_eq!(grade.direction_degrees(), 120.0);
        assert_eq!(grade.relief(), 2.5);
        assert_eq!(grade.contrast_percent(), 150.0);
        assert_eq!(grade.blend_with_original(), 0.25);
    }
    fn threshold_op(graph: &EffectRenderGraph) -> Option<crate::ThresholdGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Threshold { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Threshold { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn threshold_defaults_compile_to_the_listed_level() {
        let effect = instantiate_effect_node(EffectType::Threshold)
            .expect("registered threshold definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default threshold");
        let grade = threshold_op(&graph).expect("threshold always quantises");
        assert_eq!(grade.level(), 0.5);
        assert_eq!(
            grade.luminance_coefficients(),
            TEST_WORKING_SPACE.luminance_coefficients()
        );
    }

    #[test]
    fn threshold_rejects_levels_outside_the_declared_window() {
        let effect = instantiate_effect_node(EffectType::Threshold)
            .expect("registered threshold definition");
        for level in [30_001.0, -30_001.0] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::Threshold.property_path("level"),
                        value: PropertyValue::Float(level),
                    })
                    .is_err(),
                "the numeric contract rejects level {level}"
            );
        }
    }

    #[test]
    fn threshold_quantizes_scene_linear_float_frames() {
        let effect = instantiate_effect_node(EffectType::Threshold)
            .expect("registered threshold definition");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile threshold");
        let input = [[0.2, 0.2, 0.2, 1.0], [0.8, 0.8, 0.8, 0.5]];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute threshold");
        let pixels = output.pixels();
        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(pixels[1], [1.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn threshold_reads_the_working_space_luminance() {
        let effect = instantiate_effect_node(EffectType::Threshold)
            .expect("registered threshold definition");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile threshold");
        let input = [[0.0, 0.8, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute threshold");
        let pixels = output.pixels();
        assert_eq!(pixels[0][0], 1.0, "bright green passes the level");
        assert_eq!(pixels[1][0], 0.0, "bright blue does not");
    }

    #[test]
    fn threshold_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::Threshold)
            .expect("registered threshold definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Threshold.property_path("level"),
                value: PropertyValue::Float(0.25),
            })
            .expect("author a level");
        let encoded = serde_json::to_string(&effect).expect("serialize threshold");
        let restored: crate::EffectNode =
            serde_json::from_str(&encoded).expect("deserialize threshold");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate restored threshold");
        assert_eq!(threshold_op(&graph).expect("threshold op").level(), 0.25);
    }
    fn mosaic_op(graph: &EffectRenderGraph) -> Option<crate::MosaicGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Mosaic { grade }, .. }
            | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Mosaic { grade }, .. } => {
                Some(*grade)
            }
            _ => None,
        })
    }

    #[test]
    fn mosaic_defaults_compile_to_the_listed_grid() {
        let effect =
            instantiate_effect_node(EffectType::Mosaic).expect("registered mosaic definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default mosaic");
        let grade = mosaic_op(&graph).expect("the default grid quantises");
        assert_eq!(grade.horizontal(), 10);
        assert_eq!(grade.vertical(), 10);
        assert!(!grade.sharp_colors());
    }

    #[test]
    fn mosaic_authored_state_reaches_the_operation() {
        let mut effect =
            instantiate_effect_node(EffectType::Mosaic).expect("registered mosaic definition");
        for (parameter, value) in [
            ("horizontal", PropertyValue::Int(4)),
            ("vertical", PropertyValue::Int(8)),
            ("sharp_colors", PropertyValue::Bool(true)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Mosaic.property_path(parameter),
                    value,
                })
                .expect("author the mosaic grid");
        }
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate authored mosaic");
        let grade = mosaic_op(&graph).expect("mosaic op");
        assert_eq!(grade.horizontal(), 4);
        assert_eq!(grade.vertical(), 8);
        assert!(grade.sharp_colors());
    }

    #[test]
    fn mosaic_rejects_grids_outside_the_declared_window() {
        let effect =
            instantiate_effect_node(EffectType::Mosaic).expect("registered mosaic definition");
        for value in [0_i64, 4_001] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::Mosaic.property_path("horizontal"),
                        value: PropertyValue::Int(value),
                    })
                    .is_err(),
                "the numeric contract rejects a {value} pixel block"
            );
        }
    }

    #[test]
    fn mosaic_averages_blocks_on_scene_linear_float_frames() {
        let mut effect =
            instantiate_effect_node(EffectType::Mosaic).expect("registered mosaic definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Mosaic.property_path("horizontal"),
                value: PropertyValue::Int(2),
            })
            .expect("author horizontal block");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Mosaic.property_path("vertical"),
                value: PropertyValue::Int(2),
            })
            .expect("author vertical block");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile mosaic");
        let input = [[0.2, 0.2, 0.2, 1.0], [0.6, 0.6, 0.6, 1.0]];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute mosaic");
        let pixels = output.pixels();
        assert!((pixels[0][0] - 0.4).abs() < 1.0e-6, "{:?}", pixels[0]);
        assert!((pixels[1][0] - 0.4).abs() < 1.0e-6, "{:?}", pixels[1]);
    }

    #[test]
    fn mosaic_authored_state_survives_project_serialization_round_trip() {
        let mut effect =
            instantiate_effect_node(EffectType::Mosaic).expect("registered mosaic definition");
        for (parameter, value) in [
            ("horizontal", PropertyValue::Int(3)),
            ("vertical", PropertyValue::Int(5)),
            ("sharp_colors", PropertyValue::Bool(true)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Mosaic.property_path(parameter),
                    value,
                })
                .expect("author the mosaic grid");
        }
        let encoded = serde_json::to_string(&effect).expect("serialize mosaic");
        let restored: crate::EffectNode =
            serde_json::from_str(&encoded).expect("deserialize mosaic");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate restored mosaic");
        let grade = mosaic_op(&graph).expect("mosaic op");
        assert_eq!((grade.horizontal(), grade.vertical()), (3, 5));
        assert!(grade.sharp_colors());
    }
    fn motion_tile_op(graph: &EffectRenderGraph) -> Option<crate::MotionTileGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::MotionTile { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::MotionTile { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn motion_tile_defaults_compile_to_identity() {
        let effect = instantiate_effect_node(EffectType::MotionTile)
            .expect("registered motion tile definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds motion tile graph");
        assert!(motion_tile_op(&graph).is_none());
    }

    #[test]
    fn motion_tile_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::MotionTile)
            .expect("registered motion tile definition");
        for (parameter, value) in [
            ("center_x", 0.0_f32),
            ("center_y", 0.5),
            ("tile_width", 50.0),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::MotionTile.property_path(parameter),
                    value: PropertyValue::Float(value),
                })
                .expect("authors motion tile parameter");
        }
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::MotionTile.property_path("mirror_edges"),
                value: PropertyValue::Bool(true),
            })
            .expect("authors mirror edges");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds motion tile graph");
        let grade = motion_tile_op(&graph).expect("motion tile op");
        assert_eq!(grade.tile_width_percent(), 50.0);
        assert!(grade.mirror_edges());
        assert_eq!(grade.source_pixel(2, 0, 8, 1), (7, 0));
    }

    #[test]
    fn motion_tile_rejects_author_values_outside_the_declared_ranges() {
        for (parameter, value) in [("tile_width", 30001.0_f32), ("horizontal_phase", 1.5)] {
            let mut effect = instantiate_effect_node(EffectType::MotionTile)
                .expect("registered motion tile definition");
            let applied = effect.apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::MotionTile.property_path(parameter),
                value: PropertyValue::Float(value),
            });
            assert!(applied.is_err(), "{parameter} must be rejected");
        }
    }

    #[test]
    fn motion_tile_repeats_the_frame_across_the_tiled_field() {
        let grade =
            crate::MotionTileGrade::new([0.0, 0.5], 50.0, 100.0, 100.0, 100.0, false, 0.0, 0.0)
                .expect("tiled grade");
        let mut frame: Vec<[f32; 4]> = (0..8).map(|index| [index as f32, 0.0, 0.0, 1.0]).collect();
        assert!(crate::motion_tile_frame_f32(&mut frame, 8, 1, &grade));
        let reds: Vec<f32> = frame.iter().map(|pixel| pixel[0]).collect();
        assert_eq!(reds, vec![5.0, 7.0, 1.0, 3.0, 5.0, 7.0, 1.0, 3.0]);
    }

    #[test]
    fn motion_tile_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::MotionTile)
            .expect("registered motion tile definition");
        for (parameter, value) in [("output_width", 200.0_f32), ("phase", 3.0_f32)] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::MotionTile.property_path(parameter),
                    value: PropertyValue::Float(value),
                })
                .expect("authors motion tile parameter");
        }
        let wire = serde_json::to_string(&effect).expect("serializes");
        let restored: EffectNode = serde_json::from_str(&wire).expect("deserializes");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored motion tile graph");
        let grade = motion_tile_op(&graph).expect("motion tile op");
        assert_eq!(grade.output_width_percent(), 200.0);
        assert_eq!(grade.phase(), 3.0);
    }

    fn posterize_op(graph: &EffectRenderGraph) -> Option<crate::PosterizeGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Posterize { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Posterize { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn posterize_defaults_compile_to_the_listed_levels() {
        let effect = instantiate_effect_node(EffectType::Posterize)
            .expect("registered posterize definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate default posterize");
        assert_eq!(posterize_op(&graph).expect("posterize op").levels(), 7);
    }

    #[test]
    fn posterize_rejects_level_counts_outside_the_declared_window() {
        let effect = instantiate_effect_node(EffectType::Posterize)
            .expect("registered posterize definition");
        for levels in [1_i64, 256] {
            let mut effect = effect.clone();
            assert!(
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::Posterize.property_path("levels"),
                        value: PropertyValue::Int(levels),
                    })
                    .is_err(),
                "the numeric contract rejects {levels} levels"
            );
        }
    }

    #[test]
    fn posterize_quantises_scene_linear_float_frames() {
        let effect = instantiate_effect_node(EffectType::Posterize)
            .expect("registered posterize definition");
        let compiled = compile_clip_effect_graph(&[effect], &[], tt(0), TEST_WORKING_SPACE)
            .expect("compile posterize");
        let input = [[0.1, 0.5, 0.9, 1.0], [2.0, -0.5, 1.75, 0.25]];
        let output = crate::apply_compiled_effect_graph_rgba_f32(emboss_view(&input), &compiled, 0)
            .expect("execute posterize");
        let pixels = output.pixels();
        // Seven levels means six steps: 1/6, 5/6 and 11/6 are the nearest levels.
        for (index, expected) in [
            (0, [1.0 / 6.0_f32, 0.5, 5.0 / 6.0, 1.0]),
            (1, [2.0, -0.5, 11.0 / 6.0, 0.25]),
        ] {
            for channel in 0..4 {
                assert!(
                    (pixels[index][channel] - expected[channel]).abs() < 1.0e-6,
                    "pixel {index} channel {channel}: {:?}",
                    pixels[index]
                );
            }
        }
    }

    #[test]
    fn posterize_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::Posterize)
            .expect("registered posterize definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Posterize.property_path("levels"),
                value: PropertyValue::Int(3),
            })
            .expect("author the level count");
        let encoded = serde_json::to_string(&effect).expect("serialize posterize");
        let restored: crate::EffectNode =
            serde_json::from_str(&encoded).expect("deserialize posterize");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("evaluate restored posterize");
        assert_eq!(posterize_op(&graph).expect("posterize op").levels(), 3);
    }
    fn kaleidoscope_op(graph: &EffectRenderGraph) -> Option<crate::KaleidoscopeGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Kaleidoscope { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Kaleidoscope { grade },
                ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn kaleidoscope_defaults_compile_to_the_listed_wedges() {
        let effect = instantiate_effect_node(EffectType::Kaleidoscope)
            .expect("registered kaleidoscope definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds kaleidoscope graph");
        let grade = kaleidoscope_op(&graph).expect("kaleidoscope op");
        assert_eq!(grade.center(), [128.0, 128.0]);
        assert_eq!(grade.size_percent(), 20.0);
        assert_eq!(grade.mirroring(), 6);
        assert_eq!(grade.rotation_degrees(), 0.0);
        assert!(!grade.floating_center());
    }

    #[test]
    fn kaleidoscope_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::Kaleidoscope)
            .expect("registered kaleidoscope definition");
        for (parameter, value) in [("size", 50.0_f32), ("rotation", 30.0)] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Kaleidoscope.property_path(parameter),
                    value: PropertyValue::Float(value),
                })
                .expect("authors kaleidoscope parameter");
        }
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Kaleidoscope.property_path("mirroring"),
                value: PropertyValue::Int(4),
            })
            .expect("authors kaleidoscope mirroring");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds kaleidoscope graph");
        let grade = kaleidoscope_op(&graph).expect("kaleidoscope op");
        assert_eq!(grade.mirroring(), 4);
        assert_eq!(grade.size_percent(), 50.0);
        assert_eq!(grade.rotation_degrees(), 30.0);
    }

    #[test]
    fn kaleidoscope_rejects_author_values_outside_the_declared_ranges() {
        for (parameter, value) in [
            ("size", PropertyValue::Float(0.5)),
            ("mirroring", PropertyValue::Int(10)),
        ] {
            let mut effect = instantiate_effect_node(EffectType::Kaleidoscope)
                .expect("registered kaleidoscope definition");
            let error = effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Kaleidoscope.property_path(parameter),
                    value,
                })
                .expect_err("rejects out of range kaleidoscope parameter");
            assert!(
                format!("{error}").contains(parameter),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn kaleidoscope_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::Kaleidoscope)
            .expect("registered kaleidoscope definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Kaleidoscope.property_path("mirroring"),
                value: PropertyValue::Int(3),
            })
            .expect("authors kaleidoscope mirroring");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Kaleidoscope.property_path("size"),
                value: PropertyValue::Float(75.0),
            })
            .expect("authors kaleidoscope size");
        let wire = serde_json::to_string(&effect).expect("serializes kaleidoscope effect");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restores kaleidoscope effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored kaleidoscope graph");
        let grade = kaleidoscope_op(&graph).expect("kaleidoscope op");
        assert_eq!(grade.mirroring(), 3);
        assert_eq!(grade.size_percent(), 75.0);
    }
    fn bevel_op(graph: &EffectRenderGraph) -> Option<crate::BevelGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Bevel { grade }, .. }
            | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Bevel { grade }, .. } => {
                Some(*grade)
            }
            _ => None,
        })
    }

    #[test]
    fn bevel_defaults_compile_to_the_listed_light() {
        let effect =
            instantiate_effect_node(EffectType::Bevel).expect("registered bevel definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds bevel graph");
        let grade = bevel_op(&graph).expect("bevel op");
        assert_eq!(grade.thickness(), 2.0);
        assert_eq!(grade.angle_degrees(), -60.0);
        assert_eq!(grade.light_color(), [1.0, 1.0, 1.0]);
        assert_eq!(grade.intensity(), 0.4);
    }

    #[test]
    fn bevel_authored_state_reaches_the_operation() {
        let mut effect =
            instantiate_effect_node(EffectType::Bevel).expect("registered bevel definition");
        for (parameter, value) in [("thickness", 8.0_f32), ("angle", 45.0), ("intensity", 0.9)] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Bevel.property_path(parameter),
                    value: PropertyValue::Float(value),
                })
                .expect("authors bevel parameter");
        }
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Bevel.property_path("light_color"),
                value: PropertyValue::Color(Color { r: 1.0, g: 0.5, b: 0.25, a: 1.0 }),
            })
            .expect("authors bevel colour");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds bevel graph");
        let grade = bevel_op(&graph).expect("bevel op");
        assert_eq!(grade.thickness(), 8.0);
        assert_eq!(grade.angle_degrees(), 45.0);
        assert_eq!(grade.intensity(), 0.9);
        assert_eq!(grade.light_color(), [1.0, 0.5, 0.25]);
    }

    #[test]
    fn bevel_rejects_author_values_outside_the_declared_ranges() {
        for (parameter, value) in [
            ("thickness", PropertyValue::Float(201.0)),
            ("intensity", PropertyValue::Float(1.5)),
        ] {
            let mut effect =
                instantiate_effect_node(EffectType::Bevel).expect("registered bevel definition");
            let error = effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Bevel.property_path(parameter),
                    value,
                })
                .expect_err("rejects out of range bevel parameter");
            assert!(
                format!("{error}").contains(parameter),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn bevel_authored_state_survives_project_serialization_round_trip() {
        let mut effect =
            instantiate_effect_node(EffectType::Bevel).expect("registered bevel definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Bevel.property_path("thickness"),
                value: PropertyValue::Float(12.0),
            })
            .expect("authors bevel thickness");
        let wire = serde_json::to_string(&effect).expect("serializes bevel effect");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restores bevel effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored bevel graph");
        let grade = bevel_op(&graph).expect("bevel op");
        assert_eq!(grade.thickness(), 12.0);
    }
    fn scatter_op(graph: &EffectRenderGraph) -> Option<crate::ScatterGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Scatter { grade }, .. }
            | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Scatter { grade }, .. } => {
                Some(*grade)
            }
            _ => None,
        })
    }

    #[test]
    fn scatter_defaults_compile_to_identity() {
        let effect =
            instantiate_effect_node(EffectType::Scatter).expect("registered scatter definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds scatter graph");
        assert!(scatter_op(&graph).is_none());
    }

    #[test]
    fn scatter_authored_state_reaches_the_operation() {
        let mut effect =
            instantiate_effect_node(EffectType::Scatter).expect("registered scatter definition");
        for (parameter, value) in [("amount", 12.0_f32), ("randomness", 0.5)] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Scatter.property_path(parameter),
                    value: PropertyValue::Float(value),
                })
                .expect("authors scatter parameter");
        }
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Scatter.property_path("copies"),
                value: PropertyValue::Int(2),
            })
            .expect("authors scatter copies");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds scatter graph");
        let grade = scatter_op(&graph).expect("scatter op");
        assert_eq!(grade.amount_pixels(), 12.0);
        assert_eq!(grade.copies(), 2);
        assert_eq!(grade.randomness(), 0.5);
    }

    #[test]
    fn scatter_rejects_author_values_outside_the_declared_ranges() {
        for (parameter, value) in [
            ("amount", PropertyValue::Float(1001.0)),
            ("copies", PropertyValue::Int(4)),
            ("randomness", PropertyValue::Float(1.5)),
        ] {
            let mut effect = instantiate_effect_node(EffectType::Scatter)
                .expect("registered scatter definition");
            let error = effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Scatter.property_path(parameter),
                    value,
                })
                .expect_err("rejects out of range scatter parameter");
            assert!(
                format!("{error}").contains(parameter),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn scatter_authored_state_survives_project_serialization_round_trip() {
        let mut effect =
            instantiate_effect_node(EffectType::Scatter).expect("registered scatter definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Scatter.property_path("amount"),
                value: PropertyValue::Float(30.0),
            })
            .expect("authors scatter amount");
        let wire = serde_json::to_string(&effect).expect("serializes scatter effect");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restores scatter effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored scatter graph");
        let grade = scatter_op(&graph).expect("scatter op");
        assert_eq!(grade.amount_pixels(), 30.0);
    }

    fn brush_strokes_op(graph: &EffectRenderGraph) -> Option<crate::BrushStrokesGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::BrushStrokes { grade }, ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::BrushStrokes { grade },
                ..
            } => Some(*grade),
            _ => None,
        })
    }

    #[test]
    fn brush_strokes_defaults_reach_the_operation() {
        let effect = instantiate_effect_node(EffectType::BrushStrokes)
            .expect("registered brush strokes definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds brush strokes graph");
        let grade = brush_strokes_op(&graph).expect("brush strokes op");
        assert_eq!(grade.angle_degrees(), 135.0);
        assert_eq!(grade.size(), 2.0);
        assert_eq!(grade.length(), 5.0);
        assert_eq!(grade.density(), 1.0);
        assert_eq!(grade.randomness(), 1.0);
        assert_eq!(grade.surface(), 1);
        assert_eq!(grade.blend_with_original(), 0.0);
    }

    #[test]
    fn brush_strokes_authored_state_reaches_the_operation() {
        let mut effect = instantiate_effect_node(EffectType::BrushStrokes)
            .expect("registered brush strokes definition");
        for (parameter, value) in [
            ("angle", 30.0_f32),
            ("size", 4.0),
            ("length", 24.0),
            ("density", 1.5),
            ("randomness", 0.25),
            ("blend_with_original", 0.5),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BrushStrokes.property_path(parameter),
                    value: PropertyValue::Float(value),
                })
                .expect("authors brush strokes parameter");
        }
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BrushStrokes.property_path("surface"),
                value: PropertyValue::Int(3),
            })
            .expect("authors brush strokes surface");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds brush strokes graph");
        let grade = brush_strokes_op(&graph).expect("brush strokes op");
        assert_eq!(grade.angle_degrees(), 30.0);
        assert_eq!(grade.size(), 4.0);
        assert_eq!(grade.length(), 24.0);
        assert_eq!(grade.density(), 1.5);
        assert_eq!(grade.randomness(), 0.25);
        assert_eq!(grade.surface(), 3);
        assert_eq!(grade.blend_with_original(), 0.5);
    }

    #[test]
    fn brush_strokes_full_original_blend_compiles_to_identity() {
        let mut effect = instantiate_effect_node(EffectType::BrushStrokes)
            .expect("registered brush strokes definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BrushStrokes.property_path("blend_with_original"),
                value: PropertyValue::Float(1.0),
            })
            .expect("authors brush strokes blend");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds brush strokes graph");
        assert!(brush_strokes_op(&graph).is_none());
    }

    #[test]
    fn brush_strokes_reject_author_values_outside_the_declared_ranges() {
        for (parameter, value) in [
            ("size", PropertyValue::Float(6.0)),
            ("length", PropertyValue::Float(41.0)),
            ("density", PropertyValue::Float(2.5)),
            ("blend_with_original", PropertyValue::Float(1.5)),
        ] {
            let mut effect = instantiate_effect_node(EffectType::BrushStrokes)
                .expect("registered brush strokes definition");
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BrushStrokes.property_path(parameter),
                    value,
                })
                .expect_err("rejects out of range brush strokes parameter");
        }
        for value in [0, 5] {
            let mut effect = instantiate_effect_node(EffectType::BrushStrokes)
                .expect("registered brush strokes definition");
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::BrushStrokes.property_path("surface"),
                    value: PropertyValue::Int(value),
                })
                .expect_err("rejects out of range brush strokes surface");
        }
    }

    #[test]
    fn brush_strokes_authored_state_survives_project_serialization_round_trip() {
        let mut effect = instantiate_effect_node(EffectType::BrushStrokes)
            .expect("registered brush strokes definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BrushStrokes.property_path("length"),
                value: PropertyValue::Float(18.0),
            })
            .expect("authors brush strokes length");
        let wire = serde_json::to_string(&effect).expect("serializes brush strokes effect");
        let restored: EffectNode =
            serde_json::from_str(&wire).expect("restores brush strokes effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored brush strokes graph");
        let grade = brush_strokes_op(&graph).expect("brush strokes op");
        assert_eq!(grade.length(), 18.0);
    }

    fn tint_op(graph: &EffectRenderGraph) -> Option<crate::TintGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Tint { grade }, .. }
            | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Tint { grade }, .. } => {
                Some(*grade)
            }
            _ => None,
        })
    }

    fn author_tint(parameter: &str, value: PropertyValue) -> EffectNode {
        let mut effect =
            instantiate_effect_node(EffectType::Tint).expect("registered tint definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Tint.property_path(parameter),
                value,
            })
            .expect("authors tint parameter");
        effect
    }

    #[test]
    fn tint_defaults_reach_the_operation() {
        // The declared defaults are the neutral duotone: black stays black and
        // white stays white, so a 100% amount renders a luminance monochrome.
        let effect = instantiate_effect_node(EffectType::Tint).expect("registered tint definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds tint graph");
        let grade = tint_op(&graph).expect("tint op");
        assert_eq!(grade.black(), [0.0, 0.0, 0.0]);
        assert_eq!(grade.white(), [1.0, 1.0, 1.0]);
        assert_eq!(grade.amount(), 1.0);
    }

    #[test]
    fn tint_authored_state_reaches_the_operation() {
        let mut effect = author_tint(
            "map_black",
            PropertyValue::Color(Color { r: 0.1, g: 0.05, b: 0.3, a: 1.0 }),
        );
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Tint.property_path("map_white"),
                value: PropertyValue::Color(Color { r: 0.9, g: 0.9, b: 0.8, a: 1.0 }),
            })
            .expect("authors tint white target");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Tint.property_path("amount"),
                value: PropertyValue::Float(60.0),
            })
            .expect("authors tint amount");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds tint graph");
        let grade = tint_op(&graph).expect("tint op");
        assert_eq!(grade.black(), [0.1, 0.05, 0.3]);
        assert_eq!(grade.white(), [0.9, 0.9, 0.8]);
        assert_eq!(grade.amount(), 0.6);
    }

    #[test]
    fn tint_zero_amount_compiles_to_identity() {
        let mut effect = author_tint(
            "map_black",
            PropertyValue::Color(Color { r: 0.2, g: 0.4, b: 0.1, a: 1.0 }),
        );
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Tint.property_path("amount"),
                value: PropertyValue::Float(0.0),
            })
            .expect("authors tint amount");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds tint graph");
        assert!(tint_op(&graph).is_none());
    }

    #[test]
    fn tint_rejects_author_values_outside_the_declared_ranges() {
        for value in [PropertyValue::Float(-1.0), PropertyValue::Float(101.0)] {
            let mut effect =
                instantiate_effect_node(EffectType::Tint).expect("registered tint definition");
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Tint.property_path("amount"),
                    value,
                })
                .expect_err("rejects out of range tint amount");
        }
    }

    #[test]
    fn tint_authored_state_survives_project_serialization_round_trip() {
        let mut effect = author_tint(
            "map_white",
            PropertyValue::Color(Color { r: 0.7, g: 0.2, b: 0.1, a: 1.0 }),
        );
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Tint.property_path("amount"),
                value: PropertyValue::Float(35.0),
            })
            .expect("authors tint amount");
        let wire = serde_json::to_string(&effect).expect("serializes tint effect");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restores tint effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored tint graph");
        let grade = tint_op(&graph).expect("tint op");
        assert_eq!(grade.white(), [0.7, 0.2, 0.1]);
        assert_eq!(grade.amount(), 0.35);
    }

    fn vibrance_op(graph: &EffectRenderGraph) -> Option<crate::VibranceGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Vibrance { grade }, .. }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Vibrance { grade }, ..
            } => Some(*grade),
            _ => None,
        })
    }

    fn author_vibrance(parameter: &str, value: PropertyValue) -> EffectNode {
        let mut effect =
            instantiate_effect_node(EffectType::Vibrance).expect("registered vibrance definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Vibrance.property_path(parameter),
                value,
            })
            .expect("authors vibrance parameter");
        effect
    }

    #[test]
    fn vibrance_defaults_compile_to_identity() {
        // Both controls default to zero, so the effect is a no-op and must not
        // reach the render graph at all.
        let effect =
            instantiate_effect_node(EffectType::Vibrance).expect("registered vibrance definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds vibrance graph");
        assert!(vibrance_op(&graph).is_none());
    }

    #[test]
    fn vibrance_authored_state_reaches_the_operation() {
        let mut effect = author_vibrance("vibrance", PropertyValue::Float(55.0));
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Vibrance.property_path("saturation"),
                value: PropertyValue::Float(-20.0),
            })
            .expect("authors vibrance saturation");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds vibrance graph");
        let grade = vibrance_op(&graph).expect("vibrance op");
        assert_eq!(grade.vibrance(), 0.55);
        assert_eq!(grade.saturation(), -0.2);
    }

    #[test]
    fn vibrance_rejects_author_values_outside_the_declared_ranges() {
        for parameter in ["vibrance", "saturation"] {
            for value in [PropertyValue::Float(-101.0), PropertyValue::Float(101.0)] {
                let effect =
                    instantiate_effect_node(EffectType::Vibrance).expect("vibrance definition");
                let mut effect = effect;
                effect
                    .apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: EffectType::Vibrance.property_path(parameter),
                        value,
                    })
                    .expect_err("rejects out of range vibrance parameter");
            }
        }
    }

    #[test]
    fn vibrance_scale_keeps_luminance_and_stays_positive() {
        let grade =
            crate::VibranceGrade::for_space(1.0, -1.0, TEST_WORKING_SPACE).expect("valid vibrance");
        // The combined scale can reach zero (all vibrance and saturation
        // removed) but never crosses it, so no colour can invert its hue.
        for rgb in [[0.9_f32, 0.1, 0.05], [0.2, 0.6, 0.1], [1.5, -0.2, 0.3]] {
            let out = grade.apply_rgb(rgb);
            let luma = |value: [f32; 3]| {
                let coefficients = grade.luminance_coefficients();
                value[0] * coefficients[0] + value[1] * coefficients[1] + value[2] * coefficients[2]
            };
            assert!((luma(out) - luma(rgb)).abs() <= 1.0e-5);
        }
    }

    #[test]
    fn vibrance_authored_state_survives_project_serialization_round_trip() {
        let effect = author_vibrance("vibrance", PropertyValue::Float(-40.0));
        let wire = serde_json::to_string(&effect).expect("serializes vibrance effect");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restores vibrance effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored vibrance graph");
        let grade = vibrance_op(&graph).expect("vibrance op");
        assert_eq!(grade.vibrance(), -0.4);
    }

    fn tritone_op(graph: &EffectRenderGraph) -> Option<crate::TritoneGrade> {
        graph.nodes.iter().find_map(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Tritone { grade }, .. }
            | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Tritone { grade }, .. } => {
                Some(*grade)
            }
            _ => None,
        })
    }

    fn author_tritone(parameter: &str, value: PropertyValue) -> EffectNode {
        let mut effect =
            instantiate_effect_node(EffectType::Tritone).expect("registered tritone definition");
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Tritone.property_path(parameter),
                value,
            })
            .expect("authors tritone parameter");
        effect
    }

    #[test]
    fn tritone_defaults_reach_the_operation() {
        // The declared defaults are the neutral ramp: black stays black, mid grey
        // picks up the sepia midtone and white stays white, fully replacing the
        // source because the blend with the original starts at zero.
        let effect =
            instantiate_effect_node(EffectType::Tritone).expect("registered tritone definition");
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds tritone graph");
        let grade = tritone_op(&graph).expect("tritone op");
        assert_eq!(grade.highlights(), [1.0, 1.0, 1.0]);
        assert_eq!(grade.midtones(), [0.5, 0.39, 0.27]);
        assert_eq!(grade.shadows(), [0.0, 0.0, 0.0]);
        assert_eq!(grade.blend_with_original(), 0.0);
        assert!(!grade.is_identity());
    }

    #[test]
    fn tritone_authored_state_reaches_the_operation() {
        let mut effect = author_tritone(
            "shadows",
            PropertyValue::Color(Color { r: 0.05, g: 0.1, b: 0.3, a: 1.0 }),
        );
        for (parameter, value) in [
            (
                "highlights",
                PropertyValue::Color(Color { r: 1.0, g: 0.95, b: 0.8, a: 1.0 }),
            ),
            ("blend_with_original", PropertyValue::Float(40.0)),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Tritone.property_path(parameter),
                    value,
                })
                .expect("authors tritone parameter");
        }
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds tritone graph");
        let grade = tritone_op(&graph).expect("tritone op");
        assert_eq!(grade.shadows(), [0.05, 0.1, 0.3]);
        assert_eq!(grade.highlights(), [1.0, 0.95, 0.8]);
        assert!((grade.blend_with_original() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn tritone_full_blend_compiles_to_identity() {
        let effect = author_tritone("blend_with_original", PropertyValue::Float(100.0));
        let graph = build_effect_render_graph(&[effect], tt(0), TEST_WORKING_SPACE)
            .expect("builds tritone graph");
        assert!(tritone_op(&graph).is_none());
    }

    #[test]
    fn tritone_rejects_author_values_outside_the_declared_ranges() {
        for value in [PropertyValue::Float(-1.0), PropertyValue::Float(101.0)] {
            let mut effect = instantiate_effect_node(EffectType::Tritone)
                .expect("registered tritone definition");
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: EffectType::Tritone.property_path("blend_with_original"),
                    value,
                })
                .expect_err("rejects out of range tritone blend");
        }
    }

    #[test]
    fn tritone_authored_state_survives_project_serialization_round_trip() {
        let effect = author_tritone(
            "midtones",
            PropertyValue::Color(Color { r: 0.2, g: 0.5, b: 0.1, a: 1.0 }),
        );
        let wire = serde_json::to_string(&effect).expect("serializes tritone effect");
        let restored: EffectNode = serde_json::from_str(&wire).expect("restores tritone effect");
        assert_eq!(restored, effect);
        let graph = build_effect_render_graph(&[restored], tt(0), TEST_WORKING_SPACE)
            .expect("builds restored tritone graph");
        let grade = tritone_op(&graph).expect("tritone op");
        assert_eq!(grade.midtones(), [0.2, 0.5, 0.1]);
    }
}

#[cfg(test)]
mod image_scale_requirement_tests {
    use super::*;
    use crate::{
        EffectAlphaContract, EffectExecutionModes, EffectFloatProcessorFactory,
        EffectFloatProcessorInputRole, EffectFloatProcessorInstance,
        EffectFloatProcessorPreparation, EffectFloatProcessorSizing, EffectFloatProcessorStorage,
        EffectImageScaleRequirement, EffectProcessorControl,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[derive(Debug)]
    struct Factory {
        role: EffectFloatProcessorInputRole,
        unit: AtomicBool,
        source_service: AtomicBool,
        storage_calls: AtomicUsize,
        prepare_calls: AtomicUsize,
    }

    impl Factory {
        fn new(role: EffectFloatProcessorInputRole, unit: bool) -> Arc<Self> {
            Arc::new(Self {
                role,
                unit: AtomicBool::new(unit),
                source_service: AtomicBool::new(false),
                storage_calls: AtomicUsize::new(0),
                prepare_calls: AtomicUsize::new(0),
            })
        }
    }

    impl EffectFloatProcessorFactory for Factory {
        fn requires_input_service(&self) -> bool {
            self.source_service.load(Ordering::SeqCst)
        }
        fn input_role(&self) -> EffectFloatProcessorInputRole {
            self.role
        }
        fn image_scale_requirement(&self) -> EffectImageScaleRequirement {
            if self.unit.load(Ordering::SeqCst) {
                EffectImageScaleRequirement::Unit
            } else {
                EffectImageScaleRequirement::Any
            }
        }
        fn alpha_contract(&self) -> EffectAlphaContract {
            if self.role == EffectFloatProcessorInputRole::NoImage {
                EffectAlphaContract::SOURCE_FLOAT
            } else {
                EffectAlphaContract::IDENTITY
            }
        }
        fn feedback_budget_bytes(&self) -> usize {
            0
        }
        fn storage(
            &self,
            _: EffectFloatProcessorSizing,
        ) -> mondrian_core::Result<EffectFloatProcessorStorage> {
            self.storage_calls.fetch_add(1, Ordering::SeqCst);
            EffectFloatProcessorStorage::new(0, 0)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            _: &mut EffectProcessorControl<'_>,
        ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            panic!("snapshot rejection must happen before prepare")
        }
    }

    fn binding(factory: Arc<Factory>) -> CustomEffectProcessorBinding {
        CustomEffectProcessorBinding {
            alpha_contract: factory.alpha_contract(),
            input_role: factory.input_role(),
            image_scale_requirement: factory.image_scale_requirement(),
            requires_input_service: factory.requires_input_service(),
            revision: NEXT_CUSTOM_PROCESSOR_REVISION.fetch_add(1, Ordering::AcqRel),
            processor: CustomEffectProcessor::Float32Factory(factory),
            runtime_owner: None,
            occurrence_id: Some(EffectId::new()),
            project_geometry: None,
        }
    }

    fn op(binding: CustomEffectProcessorBinding) -> EffectRenderOp {
        EffectRenderOp::Custom {
            key: "test.image-scale".to_owned(),
            params: serde_json::json!({}),
            cache_key: Some("same-implementation".to_owned()),
            cache_policy: EffectCachePolicy::Uncacheable,
            processor: Some(binding),
        }
    }

    #[test]
    fn frozen_image_scale_changes_complete_and_subtree_identity_without_revision_change() {
        let factory = Factory::new(EffectFloatProcessorInputRole::SingleImage, false);
        let any = binding(factory.clone());
        let any_graph =
            crate::compile_reference_effect_graph(&EffectRenderPlan { ops: vec![op(any.clone())] })
                .unwrap();
        factory.unit.store(true, Ordering::SeqCst);
        let mut unit = any.clone();
        unit.image_scale_requirement = EffectImageScaleRequirement::Unit;
        let unit_graph = crate::compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![op(unit.clone())],
        })
        .unwrap();
        assert_eq!(
            any.revision(),
            unit.revision(),
            "isolate the capability identity field"
        );
        assert_eq!(
            any_graph.image_scale_requirement(),
            EffectImageScaleRequirement::Any
        );
        assert_eq!(
            unit_graph.image_scale_requirement(),
            EffectImageScaleRequirement::Unit
        );
        assert_ne!(
            any_graph.semantic_fingerprint(),
            unit_graph.semantic_fingerprint()
        );
        let output = any_graph.graph().output.unwrap();
        assert_ne!(
            any_graph.node_profiles()[&output].subtree_signature,
            unit_graph.node_profiles()[&output].subtree_signature
        );
        let owned = unit.with_runtime_owner("test.image-scale", EffectId::new(), 1, None, None);
        assert_eq!(
            owned.image_scale_requirement(),
            EffectImageScaleRequirement::Unit
        );
        assert!(owned.factory_contract_matches());
        let literal = CustomEffectProcessorBinding::new(Arc::new(|_, _, _, _, _| Ok(())));
        let literal_f32 = CustomEffectProcessorBinding::new_f32(Arc::new(|_, _, _| Ok(())));
        assert_eq!(
            literal.image_scale_requirement(),
            EffectImageScaleRequirement::Any
        );
        assert_eq!(
            literal_f32.image_scale_requirement(),
            EffectImageScaleRequirement::Any
        );
    }

    #[test]
    fn frozen_source_service_changes_identity_and_disables_incomplete_output_caching() {
        let factory = Factory::new(EffectFloatProcessorInputRole::SingleImage, false);
        let plain = binding(factory.clone());
        let mut plain_op = op(plain.clone());
        if let EffectRenderOp::Custom { cache_policy, .. } = &mut plain_op {
            *cache_policy = EffectCachePolicy::Deterministic;
        }
        assert_eq!(plain_op.cache_policy(), EffectCachePolicy::Deterministic);
        let plain_graph =
            crate::compile_reference_effect_graph(&EffectRenderPlan { ops: vec![plain_op] })
                .unwrap();
        factory.source_service.store(true, Ordering::SeqCst);
        assert!(
            !plain.factory_contract_matches(),
            "provider cannot mutate a sealed contract"
        );
        let mut source = plain.clone();
        source.requires_input_service = true;
        assert_eq!(plain.revision(), source.revision());
        assert!(source.factory_contract_matches());
        let mut source_op = op(source.clone());
        if let EffectRenderOp::Custom { cache_policy, .. } = &mut source_op {
            *cache_policy = EffectCachePolicy::Deterministic;
        }
        assert_eq!(source_op.cache_policy(), EffectCachePolicy::Uncacheable);
        let source_graph =
            crate::compile_reference_effect_graph(&EffectRenderPlan { ops: vec![source_op] })
                .unwrap();
        assert_ne!(
            plain_graph.semantic_fingerprint(),
            source_graph.semantic_fingerprint()
        );
        let output = plain_graph.graph().output.unwrap();
        assert_ne!(
            plain_graph.node_profiles()[&output].subtree_signature,
            source_graph.node_profiles()[&output].subtree_signature
        );
        let owned =
            source.with_runtime_owner("test.source-service", EffectId::new(), 1, None, None);
        assert!(owned.requires_input_service());
        assert!(owned.factory_contract_matches());
    }

    #[test]
    fn source_service_requirement_is_owned_by_reachable_prepared_definition_inputs() {
        let key = format!("test.source-service.{}", EffectId::new());
        let factory = Factory::new(EffectFloatProcessorInputRole::SingleImage, false);
        factory.source_service.store(true, Ordering::SeqCst);
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Source service",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                EffectCachePolicy::Deterministic,
                factory,
            ),
        )
        .unwrap();
        let mut effect = instantiate_effect_node(EffectType::External(key)).unwrap();
        let active = crate::PreparedEffectProgram::prepare(
            &[effect.clone()],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
        assert!(active.requires_processor_input_service());
        effect.is_enabled = false;
        let disabled =
            crate::PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
                .unwrap()
                .evaluate(TimelineTime::ZERO)
                .unwrap();
        assert!(!disabled.requires_processor_input_service());
    }

    #[test]
    fn image_scale_aggregation_uses_reachable_nodes_including_no_image_providers() {
        let unit = binding(Factory::new(
            EffectFloatProcessorInputRole::SingleImage,
            true,
        ));
        let mut dead = EffectGraphBuilderState::new();
        dead.add_unary_from(dead.source(), op(unit.clone()));
        let dead = crate::compile_reference_render_graph(dead.finish()).unwrap();
        assert_eq!(
            dead.image_scale_requirement(),
            EffectImageScaleRequirement::Any
        );
        let mut reachable = EffectGraphBuilderState::new();
        reachable.append_unary(op(unit));
        let reachable = crate::compile_reference_render_graph(reachable.finish()).unwrap();
        assert_eq!(
            reachable.image_scale_requirement(),
            EffectImageScaleRequirement::Unit
        );
        let mut source = EffectGraphBuilderState::new();
        source.append_custom_source(op(binding(Factory::new(
            EffectFloatProcessorInputRole::NoImage,
            true,
        ))));
        let source = crate::compile_reference_render_graph(source.finish()).unwrap();
        assert_eq!(
            source.image_scale_requirement(),
            EffectImageScaleRequirement::Unit
        );
        assert!(source
            .graph()
            .nodes
            .iter()
            .any(|node| matches!(node.kind, crate::EffectGraphNodeKind::CustomSource { .. })));
    }

    #[test]
    fn disabled_registered_unit_provider_does_not_constrain_compiled_graph() {
        let key = format!("test.disabled-scale.{}", EffectId::new());
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Unit source",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: crate::EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                EffectCachePolicy::Uncacheable,
                Factory::new(EffectFloatProcessorInputRole::NoImage, true),
            ),
        )
        .unwrap();
        let mut effect = instantiate_effect_node(EffectType::External(key)).unwrap();
        let active = compile_clip_effect_graph(
            &[effect.clone()],
            &[],
            TimelineTime::ZERO,
            WorkingColorSpace::LinearRec709,
        )
        .unwrap();
        assert_eq!(
            active.image_scale_requirement(),
            EffectImageScaleRequirement::Unit
        );
        effect.is_enabled = false;
        let disabled = compile_clip_effect_graph(
            &[effect],
            &[],
            TimelineTime::ZERO,
            WorkingColorSpace::LinearRec709,
        )
        .unwrap();
        assert!(disabled.graph().is_identity());
        assert_eq!(
            disabled.image_scale_requirement(),
            EffectImageScaleRequirement::Any
        );
    }

    #[test]
    fn provider_scale_drift_is_rejected_before_sizing_prepare_or_pixel_mutation() {
        let factory = Factory::new(EffectFloatProcessorInputRole::SingleImage, false);
        let operation = op(binding(factory.clone()));
        let plan = EffectRenderPlan { ops: vec![operation.clone()] };
        let graph = crate::compile_reference_effect_graph(&plan).unwrap();
        let topology = crate::prepare_effect_graph_topology(graph.graph()).unwrap();
        factory.unit.store(true, Ordering::SeqCst);
        assert_eq!(
            graph.image_scale_requirement(),
            EffectImageScaleRequirement::Any,
            "a compiled requirement never reads mutable provider state"
        );
        assert!(crate::compile_reference_effect_graph(&plan).is_none());
        assert!(topology.bind(graph.graph().clone()).is_none());
        assert!(matches!(
            crate::processor_residency::frame_processor_charge(&operation, 1, 1),
            Err(crate::EffectExecutionError::CustomProcessorUnavailable { .. })
        ));
        let mut pixels = crate::EffectFloatFrame::from_pixels(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, 0.5, 0.75, 1.0]],
        )
        .unwrap();
        assert!(matches!(
            crate::adjustment::apply_custom_render_op_f32(
                &mut pixels,
                &operation,
                0,
                None,
                0,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(crate::EffectExecutionError::CustomProcessorUnavailable { .. })
        ));
        assert_eq!(pixels.pixels(), &[[0.25, 0.5, 0.75, 1.0]]);
        assert_eq!(factory.storage_calls.load(Ordering::SeqCst), 0);
        assert_eq!(factory.prepare_calls.load(Ordering::SeqCst), 0);
        factory.unit.store(false, Ordering::SeqCst);
        let restored = topology.bind(graph.graph().clone()).unwrap();
        assert_eq!(
            restored.semantic_fingerprint(),
            graph.semantic_fingerprint()
        );
    }
}
