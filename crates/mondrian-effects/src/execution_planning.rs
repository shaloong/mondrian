//! Checked execution-demand and linear-stage placement planning.
//!
//! This Module does not introduce another render graph. It derives temporal,
//! spatial, state, resource, exact-mode, and placement obligations directly from
//! the definition-bound [`EffectExecutionEnvelope`] retained by
//! [`crate::CompiledEffectGraph`].

use crate::{
    EffectExecutionAdmissionError, EffectExecutionContract, EffectExecutionEnvelope,
    EffectExecutionModes, EffectGraphTopology, EffectProcessingBackend, EffectResourceLifetime,
    EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectTemporalSpan,
    EffectWorkingPrecision,
};
use mondrian_core::TimelineTime;
use std::{collections::HashMap, sync::Arc};

/// Complete pixel extent of one effect input/output frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectFrameExtent {
    width: u32,
    height: u32,
}

impl EffectFrameExtent {
    /// Construct one frame extent.
    ///
    /// Zero-sized extents are retained as valid empty work. This lets
    /// schedulers short-circuit empty tiles without inventing a one-pixel
    /// allocation.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Frame width in pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Frame height in pixels.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Exact full-frame region.
    pub const fn full_frame_roi(self) -> EffectPixelRoi {
        EffectPixelRoi::new(0, 0, self.width, self.height)
    }

    /// Whether the frame contains no pixels.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A raster extent and its origin relative to the original layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectRasterBounds {
    extent: EffectFrameExtent,
    origin_half_pixels: [i64; 2],
}

/// Exact uniform upscale ratio for an authored Float32 percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectRasterScale(u32);

impl EffectRasterScale {
    // From 100 through 30000, every f32 percentage is an exact multiple of
    // 2^-17 and its scaled numerator fits u32.
    const PERCENT_UNIT: u32 = 1 << 17;
    const DENOMINATOR: u32 = 100 * Self::PERCENT_UNIT;

    /// Admit a finite percentage in the supported 100–30000 range.
    pub fn from_percent(percent: f32) -> Option<Self> {
        (100.0..=30_000.0)
            .contains(&percent)
            .then(|| Self((f64::from(percent) * f64::from(Self::PERCENT_UNIT)) as u32))
    }

    /// Whether the exact ratio is one.
    pub const fn is_identity(self) -> bool {
        self.0 == Self::DENOMINATOR
    }

    /// Input texels per output texel, for the shared CPU/GPU sampling map.
    pub fn inverse_factor(self) -> f32 {
        (f64::from(Self::DENOMINATOR) / f64::from(self.0)) as f32
    }

    /// Scale the extent and round each axis up, keeping an empty grid empty.
    pub fn checked_extent(self, extent: EffectFrameExtent) -> Option<EffectFrameExtent> {
        if extent.is_empty() {
            return Some(extent);
        }
        let scale = |size| {
            let product = u64::from(size) * u64::from(self.0);
            u32::try_from(product.div_ceil(u64::from(Self::DENOMINATOR))).ok()
        };
        Some(EffectFrameExtent::new(
            scale(extent.width())?,
            scale(extent.height())?,
        ))
    }

    /// Resolve a bounded pyramid whose stages upscale by at most 2×.
    ///
    /// The integral levels never round. The final level uses the total
    /// authored ratio and the original input extent to round exactly once.
    pub fn stages(self, input: EffectFrameExtent) -> Option<Vec<EffectRasterUpscaleStage>> {
        if self.is_identity() || input.is_empty() {
            return Some(Vec::new());
        }
        let output = self.checked_extent(input)?;
        let mut stages = Vec::new();
        let mut extent = input;
        let mut denominator = Self::DENOMINATOR;
        while denominator < self.0.div_ceil(2) {
            let doubled = EffectFrameExtent::new(
                extent.width().checked_mul(2)?,
                extent.height().checked_mul(2)?,
            );
            stages.push(EffectRasterUpscaleStage { input: extent, output: doubled, ratio: [2, 1] });
            extent = doubled;
            denominator *= 2;
        }
        stages.push(EffectRasterUpscaleStage {
            input: extent,
            output,
            ratio: [self.0, denominator],
        });
        Some(stages)
    }
}

/// The input and output geometry of one centered upscale level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectRasterUpscaleStage {
    input: EffectFrameExtent,
    output: EffectFrameExtent,
    ratio: [u32; 2],
}

impl EffectRasterUpscaleStage {
    /// Grid read by this level.
    pub const fn input_extent(self) -> EffectFrameExtent {
        self.input
    }

    /// Grid written by this level.
    pub const fn output_extent(self) -> EffectFrameExtent {
        self.output
    }

    /// Source positions for each output column, in order.
    pub fn column_samples(self) -> impl ExactSizeIterator<Item = EffectRasterAxisSample> {
        self.axis_samples(self.input.width(), self.output.width())
    }

    /// Source positions for each output row, in order.
    pub fn row_samples(self) -> impl ExactSizeIterator<Item = EffectRasterAxisSample> {
        self.axis_samples(self.input.height(), self.output.height())
    }

    fn axis_samples(
        self,
        input: u32,
        output: u32,
    ) -> impl ExactSizeIterator<Item = EffectRasterAxisSample> {
        let [num, den] = self.ratio.map(i128::from);
        let divisor = 2 * num;
        let last = i128::from(input - 1);
        let bias = (1 - i128::from(output)) * den + last * num;
        (0..output).map(move |index| {
            let numerator = (2 * i128::from(index) * den + bias).clamp(0, last * divisor);
            let base = (numerator / divisor) as u32;
            let fraction = ((numerator % divisor) as f64 / divisor as f64) as f32;
            if fraction == 1.0 {
                EffectRasterAxisSample { base: base + 1, fraction: 0.0 }
            } else {
                EffectRasterAxisSample { base, fraction }
            }
        })
    }
}

/// One clamped source texel position, split for shared CPU and GPU sampling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectRasterAxisSample {
    /// Index of the source texel at or below the sample position.
    pub base: u32,
    /// Fraction of a texel toward the next source position.
    pub fraction: f32,
}

/// A stage's effect on the raster presented to the next stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectRasterChange {
    /// Retain the entire incoming grid.
    Preserve,
    /// Add an integer border on both sides of each axis.
    Outset([u32; 2]),
    /// Uniformly enlarge the grid about its center.
    Upscale(EffectRasterScale),
}

impl EffectRasterChange {
    /// Whether this stage can keep using its incoming grid.
    pub const fn is_identity(self) -> bool {
        match self {
            Self::Preserve | Self::Outset([0, 0]) => true,
            Self::Outset(_) => false,
            Self::Upscale(scale) => scale.is_identity(),
        }
    }
}

impl EffectRasterBounds {
    /// The original layer grid, before any effect grows it.
    pub const fn from_extent(extent: EffectFrameExtent) -> Self {
        Self { extent, origin_half_pixels: [0, 0] }
    }

    /// Recover the current origin after any combination of centered grid changes.
    pub fn centered_on(primary: EffectFrameExtent, extent: EffectFrameExtent) -> Option<Self> {
        Self::from_extent(primary).with_extent_and_shift(
            extent,
            [
                i64::from(primary.width()) - i64::from(extent.width()),
                i64::from(primary.height()) - i64::from(extent.height()),
            ],
        )
    }

    /// Dimensions of the addressable raster.
    pub const fn extent(self) -> EffectFrameExtent {
        self.extent
    }

    /// Integer floor of the raster's top-left origin in layer coordinates.
    pub const fn origin(self) -> [i32; 2] {
        [
            self.origin_half_pixels[0].div_euclid(2) as i32,
            self.origin_half_pixels[1].div_euclid(2) as i32,
        ]
    }

    /// Exact layer origin in half-pixel units.
    pub const fn origin_half_pixels(self) -> [i64; 2] {
        self.origin_half_pixels
    }

    /// Resolve the next stage in the same layer coordinate system.
    pub fn checked_apply(self, change: EffectRasterChange) -> Option<Self> {
        match change {
            EffectRasterChange::Preserve => Some(self),
            EffectRasterChange::Outset(outset) => self.checked_outset(outset),
            EffectRasterChange::Upscale(scale) => {
                let extent = scale.checked_extent(self.extent)?;
                self.with_extent_and_shift(
                    extent,
                    [
                        i64::from(self.extent.width()) - i64::from(extent.width()),
                        i64::from(self.extent.height()) - i64::from(extent.height()),
                    ],
                )
            }
        }
    }

    /// Grow symmetrically along both axes, preserving the original pixel positions.
    pub fn checked_outset(self, [x, y]: [u32; 2]) -> Option<Self> {
        let width = self.extent.width().checked_add(x.checked_mul(2)?)?;
        let height = self.extent.height().checked_add(y.checked_mul(2)?)?;
        self.with_extent_and_shift(
            EffectFrameExtent::new(width, height),
            [-2 * i64::from(x), -2 * i64::from(y)],
        )
    }

    fn with_extent_and_shift(self, extent: EffectFrameExtent, [x, y]: [i64; 2]) -> Option<Self> {
        let origin_half_pixels = [
            self.origin_half_pixels[0] + x,
            self.origin_half_pixels[1] + y,
        ];
        i32::try_from(origin_half_pixels[0].div_euclid(2)).ok()?;
        i32::try_from(origin_half_pixels[1].div_euclid(2)).ok()?;
        Some(Self { extent, origin_half_pixels })
    }

    /// Move the layer affine to the new origin without changing its linear part.
    pub fn project_affine(self, [a, b, tx, c, d, ty]: [f32; 6]) -> [f32; 6] {
        if self.origin_half_pixels == [0, 0] {
            return [a, b, tx, c, d, ty];
        }
        let [x, y] = self.origin_half_pixels.map(|value| value as f32 * 0.5);
        [a, b, tx + a * x + b * y, c, d, ty + c * x + d * y]
    }
}

/// Half-open pixel region `[x, x + width) × [y, y + height)`.
///
/// Construction accepts any `u32` values. Demand planning performs
/// overflow-free intersection with the complete frame before the region can
/// enter an executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectPixelRoi {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl EffectPixelRoi {
    /// Construct a pixel region.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// Left edge.
    pub const fn x(self) -> u32 {
        self.x
    }

    /// Top edge.
    pub const fn y(self) -> u32 {
        self.y
    }

    /// Region width.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Region height.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Whether this region contains no pixels.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    fn clamp_to(self, frame: EffectFrameExtent) -> Self {
        let x = self.x.min(frame.width);
        let y = self.y.min(frame.height);
        let right = (u64::from(self.x) + u64::from(self.width)).min(u64::from(frame.width));
        let bottom = (u64::from(self.y) + u64::from(self.height)).min(u64::from(frame.height));
        let width = bounded_u64_to_u32(right.saturating_sub(u64::from(x)));
        let height = bounded_u64_to_u32(bottom.saturating_sub(u64::from(y)));
        Self { x, y, width, height }
    }

    fn expand_and_clamp(
        self,
        horizontal_pixels: u32,
        vertical_pixels: u32,
        frame: EffectFrameExtent,
    ) -> Self {
        if self.is_empty() {
            return self;
        }
        let x = self.x.saturating_sub(horizontal_pixels);
        let y = self.y.saturating_sub(vertical_pixels);
        let right = (u64::from(self.x) + u64::from(self.width) + u64::from(horizontal_pixels))
            .min(u64::from(frame.width));
        let bottom = (u64::from(self.y) + u64::from(self.height) + u64::from(vertical_pixels))
            .min(u64::from(frame.height));
        Self {
            x,
            y,
            width: bounded_u64_to_u32(right.saturating_sub(u64::from(x))),
            height: bounded_u64_to_u32(bottom.saturating_sub(u64::from(y))),
        }
    }
}

fn bounded_u64_to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Required source region and the strength of the ROI evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectInputRoi {
    /// An exact region smaller than the complete frame, including empty work.
    Exact(EffectPixelRoi),
    /// The ROI law proves that the complete frame is exactly required.
    ExactFullFrame(EffectPixelRoi),
    /// No trustworthy ROI law exists, so the complete frame is requested
    /// conservatively. Tile executors must not advertise this as exact ROI
    /// support.
    UnknownConservativeFullFrame(EffectPixelRoi),
}

impl EffectInputRoi {
    /// Concrete clamped source pixels required by this demand.
    pub const fn region(self) -> EffectPixelRoi {
        match self {
            Self::Exact(region)
            | Self::ExactFullFrame(region)
            | Self::UnknownConservativeFullFrame(region) => region,
        }
    }

    /// Exact finite halo around the requested output, when the definition owns
    /// a trustworthy ROI law.
    ///
    /// Unknown-conservative full-frame demand deliberately returns `None`;
    /// callers must not relabel a fallback full-frame request as exact tiling
    /// evidence.
    pub fn exact_halo(self, output: EffectPixelRoi) -> Option<EffectRoiHalo> {
        let input = match self {
            Self::Exact(input) | Self::ExactFullFrame(input) => input,
            Self::UnknownConservativeFullFrame(_) => return None,
        };
        let input_right = u64::from(input.x).saturating_add(u64::from(input.width));
        let input_bottom = u64::from(input.y).saturating_add(u64::from(input.height));
        let output_right = u64::from(output.x).saturating_add(u64::from(output.width));
        let output_bottom = u64::from(output.y).saturating_add(u64::from(output.height));
        if input.x > output.x
            || input.y > output.y
            || input_right < output_right
            || input_bottom < output_bottom
        {
            return None;
        }
        Some(EffectRoiHalo {
            left: output.x - input.x,
            top: output.y - input.y,
            right: bounded_u64_to_u32(input_right - output_right),
            bottom: bounded_u64_to_u32(input_bottom - output_bottom),
        })
    }
}

/// Exact finite source pixels retained around one output tile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct EffectRoiHalo {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl EffectRoiHalo {
    /// Pixels required left of the output.
    pub const fn left(self) -> u32 {
        self.left
    }

    /// Pixels required above the output.
    pub const fn top(self) -> u32 {
        self.top
    }

    /// Pixels required right of the output.
    pub const fn right(self) -> u32 {
        self.right
    }

    /// Pixels required below the output.
    pub const fn bottom(self) -> u32 {
        self.bottom
    }
}

/// One directional boundary of an exact temporal input window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectTemporalBoundary {
    /// Exact finite boundary.
    Finite(TimelineTime),
    /// The definition cannot promise a finite bound in this direction.
    Unbounded,
}

/// Exact input-time window required for one output instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectTemporalWindow {
    earliest: EffectTemporalBoundary,
    latest: EffectTemporalBoundary,
}

impl EffectTemporalWindow {
    /// Earliest required input instant.
    pub const fn earliest(self) -> EffectTemporalBoundary {
        self.earliest
    }

    /// Latest required input instant.
    pub const fn latest(self) -> EffectTemporalBoundary {
        self.latest
    }
}

/// Direction whose exact temporal-boundary arithmetic overflowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectTemporalDirection {
    /// History before the output instant.
    Past,
    /// Lookahead after the output instant.
    Future,
}

/// Why execution demand cannot be represented safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectExecutionDemandError {
    /// A definition supplied a negative duration in one temporal direction.
    #[error("effect temporal {direction:?} extent cannot be negative")]
    NegativeTemporalExtent {
        /// Invalid direction.
        direction: EffectTemporalDirection,
    },
    /// Exact rational boundary arithmetic exceeded canonical `TimelineTime`.
    #[error("effect temporal {direction:?} boundary overflowed exact timeline time")]
    TemporalBoundaryOverflow {
        /// Direction that overflowed.
        direction: EffectTemporalDirection,
    },
}

/// Exact backend/representation obligations for a complete program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectExecutionModeObligation {
    homogeneous: EffectExecutionModes,
    stages: Arc<[EffectExecutionModes]>,
}

impl EffectExecutionModeObligation {
    /// Exact modes shared by every stage. An empty set means explicit
    /// backend and/or representation transitions are required.
    pub const fn homogeneous(&self) -> EffectExecutionModes {
        self.homogeneous
    }

    /// Ordered definition-stage exact mode sets.
    pub fn stages(&self) -> &[EffectExecutionModes] {
        &self.stages
    }
}

/// State, resource, and exact-mode ownership required by one execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectExecutionObligations {
    state_model: EffectStateModel,
    resource_lifetime: EffectResourceLifetime,
    execution_modes: EffectExecutionModeObligation,
}

impl EffectExecutionObligations {
    /// Mutable-state ownership model.
    pub const fn state_model(&self) -> EffectStateModel {
        self.state_model
    }

    /// Longest resource ownership scope.
    pub const fn resource_lifetime(&self) -> EffectResourceLifetime {
        self.resource_lifetime
    }

    /// Complete and per-stage exact execution modes.
    pub const fn execution_modes(&self) -> &EffectExecutionModeObligation {
        &self.execution_modes
    }
}

/// Checked demand derived from one immutable execution envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectExecutionDemand {
    output_time: TimelineTime,
    temporal_extent: EffectTemporalInputExtent,
    temporal_window: EffectTemporalWindow,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    input_roi: EffectInputRoi,
    obligations: EffectExecutionObligations,
}

impl EffectExecutionDemand {
    /// Exact requested output instant.
    pub const fn output_time(&self) -> TimelineTime {
        self.output_time
    }

    /// Definition-owned temporal extent before source-boundary clamping.
    ///
    /// A single-frame executor must inspect this value, not infer temporal
    /// locality from a window whose finite history happened to clamp to zero.
    pub const fn temporal_extent(&self) -> EffectTemporalInputExtent {
        self.temporal_extent
    }

    /// Checked exact input-time window.
    pub const fn temporal_window(&self) -> EffectTemporalWindow {
        self.temporal_window
    }

    /// Complete frame extent used for spatial clamping.
    pub const fn frame_extent(&self) -> EffectFrameExtent {
        self.frame_extent
    }

    /// Requested output ROI after overflow-free frame intersection.
    pub const fn output_roi(&self) -> EffectPixelRoi {
        self.output_roi
    }

    /// Concrete input ROI and its exact/unknown evidence.
    pub const fn input_roi(&self) -> EffectInputRoi {
        self.input_roi
    }

    /// Exact finite ROI halo, or `None` when ROI propagation is unknown.
    pub fn exact_halo(&self) -> Option<EffectRoiHalo> {
        self.input_roi.exact_halo(self.output_roi)
    }

    /// State, resource, and precision obligations.
    pub const fn obligations(&self) -> &EffectExecutionObligations {
        &self.obligations
    }
}

/// Opaque set of exact working precisions supported by one lane.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectWorkingPrecisions(u8);

impl EffectWorkingPrecisions {
    const U8_BIT: u8 = 1 << 0;
    const F16_BIT: u8 = 1 << 1;
    const F32_BIT: u8 = 1 << 2;

    /// No precision.
    pub const NONE: Self = Self(0);
    /// Encoded normalized 8-bit samples.
    pub const NORMALIZED_U8: Self = Self(Self::U8_BIT);
    /// Float16 samples.
    pub const FLOAT16: Self = Self(Self::F16_BIT);
    /// Float32 samples.
    pub const FLOAT32: Self = Self(Self::F32_BIT);
    /// Every modeled working precision.
    pub const ALL: Self = Self(Self::U8_BIT | Self::F16_BIT | Self::F32_BIT);

    /// Return the union of two precision sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether this set contains one exact precision.
    pub const fn contains(self, precision: EffectWorkingPrecision) -> bool {
        self.0 & Self::only(precision).0 != 0
    }

    /// Whether no exact precision is supported.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Build a singleton precision set.
    pub const fn only(precision: EffectWorkingPrecision) -> Self {
        match precision {
            EffectWorkingPrecision::NormalizedU8 => Self::NORMALIZED_U8,
            EffectWorkingPrecision::Float16 => Self::FLOAT16,
            EffectWorkingPrecision::Float32 => Self::FLOAT32,
        }
    }
}

impl std::fmt::Debug for EffectWorkingPrecisions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut set = formatter.debug_set();
        for precision in ordered_precisions() {
            if self.contains(precision) {
                set.entry(&precision);
            }
        }
        set.finish()
    }
}

/// Stable caller-owned identity of one execution lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectExecutionLaneId(u16);

impl EffectExecutionLaneId {
    /// Construct a lane identity.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Numeric identity for diagnostics or persistence-free caches.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// One concrete dispatch lane and its exact sample representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectExecutionLane {
    id: EffectExecutionLaneId,
    backend: EffectProcessingBackend,
    supported_precisions: EffectWorkingPrecisions,
    dispatch_cost: u32,
}

impl EffectExecutionLane {
    /// Construct one lane.
    ///
    /// `dispatch_cost` is a deterministic relative scheduling cost, not an
    /// observed duration. Environment construction rejects an empty precision
    /// set.
    pub const fn new(
        id: EffectExecutionLaneId,
        backend: EffectProcessingBackend,
        supported_precisions: EffectWorkingPrecisions,
        dispatch_cost: u32,
    ) -> Self {
        Self { id, backend, supported_precisions, dispatch_cost }
    }

    /// Lane identity.
    pub const fn id(self) -> EffectExecutionLaneId {
        self.id
    }

    /// Concrete processing backend.
    pub const fn backend(self) -> EffectProcessingBackend {
        self.backend
    }

    /// Exact sample representations this lane can dispatch.
    pub const fn supported_precisions(self) -> EffectWorkingPrecisions {
        self.supported_precisions
    }

    /// Relative per-stage dispatch cost.
    pub const fn dispatch_cost(self) -> u32 {
        self.dispatch_cost
    }
}

/// One explicit directed backend and/or precision transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectExecutionTransfer {
    from_lane: EffectExecutionLaneId,
    from_precision: EffectWorkingPrecision,
    to_lane: EffectExecutionLaneId,
    to_precision: EffectWorkingPrecision,
    cost: u32,
}

impl EffectExecutionTransfer {
    /// Construct one exact directed transfer capability.
    pub const fn new(
        from_lane: EffectExecutionLaneId,
        from_precision: EffectWorkingPrecision,
        to_lane: EffectExecutionLaneId,
        to_precision: EffectWorkingPrecision,
        cost: u32,
    ) -> Self {
        Self {
            from_lane,
            from_precision,
            to_lane,
            to_precision,
            cost,
        }
    }

    /// Source lane.
    pub const fn from_lane(self) -> EffectExecutionLaneId {
        self.from_lane
    }

    /// Source representation.
    pub const fn from_precision(self) -> EffectWorkingPrecision {
        self.from_precision
    }

    /// Destination lane.
    pub const fn to_lane(self) -> EffectExecutionLaneId {
        self.to_lane
    }

    /// Destination representation.
    pub const fn to_precision(self) -> EffectWorkingPrecision {
        self.to_precision
    }

    /// Relative transfer cost.
    pub const fn cost(self) -> u32 {
        self.cost
    }
}

/// Invalid execution-environment description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectExecutionEnvironmentError {
    /// Two lanes used the same stable identity.
    #[error("duplicate effect execution lane {lane:?}")]
    DuplicateLane {
        /// Duplicated lane.
        lane: EffectExecutionLaneId,
    },
    /// One lane advertised no sample representation.
    #[error("effect execution lane {lane:?} supports no working precision")]
    EmptyLanePrecisionSet {
        /// Invalid lane.
        lane: EffectExecutionLaneId,
    },
    /// A transfer names a lane absent from the environment.
    #[error("effect execution transfer references unknown lane {lane:?}")]
    UnknownTransferLane {
        /// Missing endpoint.
        lane: EffectExecutionLaneId,
    },
    /// A transfer endpoint precision is not supported by its lane.
    #[error("effect execution transfer uses unsupported {precision:?} on lane {lane:?}")]
    UnsupportedTransferPrecision {
        /// Endpoint lane.
        lane: EffectExecutionLaneId,
        /// Unsupported representation.
        precision: EffectWorkingPrecision,
    },
    /// Two transfers describe the same exact directed conversion.
    #[error("duplicate effect execution transfer from {from_lane:?} to {to_lane:?}")]
    DuplicateTransfer {
        /// Source lane.
        from_lane: EffectExecutionLaneId,
        /// Source precision.
        from_precision: EffectWorkingPrecision,
        /// Destination lane.
        to_lane: EffectExecutionLaneId,
        /// Destination precision.
        to_precision: EffectWorkingPrecision,
    },
}

/// Validated lanes and exact directed transfers available to one scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectExecutionEnvironment {
    lanes: Arc<[EffectExecutionLane]>,
    transfers: Arc<[EffectExecutionTransfer]>,
}

impl EffectExecutionEnvironment {
    /// Validate and retain one deterministic scheduling environment.
    ///
    /// Lane order is the stable tie-break after total cost; callers should list
    /// preferred lanes first. Duplicate implicit capabilities are rejected
    /// instead of allowing map insertion order to choose a placement.
    pub fn new(
        lanes: impl Into<Arc<[EffectExecutionLane]>>,
        transfers: impl Into<Arc<[EffectExecutionTransfer]>>,
    ) -> Result<Self, EffectExecutionEnvironmentError> {
        let lanes = lanes.into();
        let transfers = transfers.into();
        let mut lane_by_id = HashMap::with_capacity(lanes.len());
        for (index, lane) in lanes.iter().enumerate() {
            if lane.supported_precisions.is_empty() {
                return Err(EffectExecutionEnvironmentError::EmptyLanePrecisionSet {
                    lane: lane.id,
                });
            }
            if lane_by_id.insert(lane.id, index).is_some() {
                return Err(EffectExecutionEnvironmentError::DuplicateLane { lane: lane.id });
            }
        }
        let mut transfer_keys = Vec::with_capacity(transfers.len());
        for transfer in transfers.iter().copied() {
            let from = lane_by_id.get(&transfer.from_lane).copied().ok_or(
                EffectExecutionEnvironmentError::UnknownTransferLane { lane: transfer.from_lane },
            )?;
            let to = lane_by_id.get(&transfer.to_lane).copied().ok_or(
                EffectExecutionEnvironmentError::UnknownTransferLane { lane: transfer.to_lane },
            )?;
            if !lanes[from].supported_precisions.contains(transfer.from_precision) {
                return Err(
                    EffectExecutionEnvironmentError::UnsupportedTransferPrecision {
                        lane: transfer.from_lane,
                        precision: transfer.from_precision,
                    },
                );
            }
            if !lanes[to].supported_precisions.contains(transfer.to_precision) {
                return Err(
                    EffectExecutionEnvironmentError::UnsupportedTransferPrecision {
                        lane: transfer.to_lane,
                        precision: transfer.to_precision,
                    },
                );
            }
            let key = (
                transfer.from_lane,
                transfer.from_precision,
                transfer.to_lane,
                transfer.to_precision,
            );
            if transfer_keys.contains(&key) {
                return Err(EffectExecutionEnvironmentError::DuplicateTransfer {
                    from_lane: transfer.from_lane,
                    from_precision: transfer.from_precision,
                    to_lane: transfer.to_lane,
                    to_precision: transfer.to_precision,
                });
            }
            transfer_keys.push(key);
        }
        Ok(Self { lanes, transfers })
    }

    /// Concrete dispatch lanes in deterministic preference order.
    pub fn lanes(&self) -> &[EffectExecutionLane] {
        &self.lanes
    }

    /// Exact directed transfers.
    pub fn transfers(&self) -> &[EffectExecutionTransfer] {
        &self.transfers
    }
}

/// One proposed dispatch or inter-stage transfer in a linear placement plan.
///
/// This is scheduling evidence, not an executable graph-value route. Input and
/// output residency are deliberately absent, and General DAGs are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectLinearStagePlacementStep {
    /// Execute one definition stage.
    Execute {
        /// Zero-based stage index in the immutable envelope.
        stage_index: usize,
        /// Selected lane.
        lane: EffectExecutionLaneId,
        /// Selected backend.
        backend: EffectProcessingBackend,
        /// Exact working representation.
        precision: EffectWorkingPrecision,
    },
    /// Transfer output before executing `before_stage`.
    Transfer {
        /// Destination stage index.
        before_stage: usize,
        /// Source lane.
        from_lane: EffectExecutionLaneId,
        /// Source representation.
        from_precision: EffectWorkingPrecision,
        /// Destination lane.
        to_lane: EffectExecutionLaneId,
        /// Destination representation.
        to_precision: EffectWorkingPrecision,
    },
}

/// Deterministically selected placement for a definition-level linear chain.
///
/// The plan proves only that each stage has an available lane and every
/// adjacent placement change has a declared transfer. It does not own graph
/// values, fan-out/join lifetimes, endpoint residency, or transfer execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectLinearStagePlacement {
    steps: Arc<[EffectLinearStagePlacementStep]>,
    total_cost: u64,
}

impl EffectLinearStagePlacement {
    /// Ordered stage placements and required adjacent transfers.
    pub fn steps(&self) -> &[EffectLinearStagePlacementStep] {
        &self.steps
    }

    /// Total caller-defined relative placement cost.
    pub const fn total_cost(&self) -> u64 {
        self.total_cost
    }
}

/// Why no semantically admitted linear stage placement can be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectLinearStagePlacementError {
    /// A general DAG needs graph-value-aware fan-out/join and lifetime
    /// planning; definition stages cannot be flattened into a data route.
    #[error("general-DAG effect execution requires a graph-value-aware planner")]
    GeneralDagRequiresValuePlanner,
    /// No lane supports any backend/precision admitted by one stage.
    #[error("effect stage {stage_index} has no execution lane for exact modes {modes:?}")]
    NoLaneForStage {
        /// First impossible stage.
        stage_index: usize,
        /// Stage exact-mode contract.
        modes: EffectExecutionModes,
    },
    /// Stage-local lanes exist, but no declared directed transfer connects
    /// them to the preceding reachable route.
    #[error("effect stage {stage_index} cannot be placed through declared adjacent transfers")]
    NoTransferPlacement {
        /// First unreachable stage.
        stage_index: usize,
    },
    /// Caller-supplied relative costs overflowed the route accumulator.
    #[error("effect linear-stage placement cost overflowed")]
    CostOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlacementState {
    lane_index: usize,
    precision: EffectWorkingPrecision,
}

#[derive(Debug, Clone, Copy)]
struct PlacementCandidate {
    state: PlacementState,
    total_cost: u64,
    predecessor: Option<usize>,
    transfer: Option<usize>,
}

impl EffectExecutionEnvelope {
    /// Derive exact temporal/spatial and ownership demand for one output
    /// request.
    pub fn plan_execution_demand(
        &self,
        output_time: TimelineTime,
        frame_extent: EffectFrameExtent,
        output_roi: EffectPixelRoi,
    ) -> Result<EffectExecutionDemand, EffectExecutionDemandError> {
        let aggregate = self.aggregate();
        let temporal_window = plan_temporal_window(output_time, aggregate.temporal_input)?;
        let output_roi = output_roi.clamp_to(frame_extent);
        let input_roi = plan_input_roi(output_roi, frame_extent, aggregate.roi_propagation);
        let execution_modes = EffectExecutionModeObligation {
            homogeneous: aggregate.execution_modes,
            stages: self
                .stages()
                .iter()
                .map(|contract| contract.execution_modes)
                .collect::<Vec<_>>()
                .into(),
        };
        Ok(EffectExecutionDemand {
            output_time,
            temporal_extent: aggregate.temporal_input,
            temporal_window,
            frame_extent,
            output_roi,
            input_roi,
            obligations: EffectExecutionObligations {
                state_model: aggregate.state_model,
                resource_lifetime: aggregate.resource_lifetime,
                execution_modes,
            },
        })
    }

    /// Place a definition-level linear chain using a small deterministic
    /// dynamic program over `(lane, precision)` states.
    ///
    /// Remaining on the same lane and representation needs no transfer.
    /// Every other transition must be declared exactly by the environment.
    ///
    /// This intentionally rejects `GeneralDag`: a real DAG planner must track
    /// every graph value, branch, join, lifetime, and endpoint residency rather
    /// than pretending topological order is a serial data path.
    pub fn plan_linear_stage_placement(
        &self,
        environment: &EffectExecutionEnvironment,
    ) -> Result<EffectLinearStagePlacement, EffectLinearStagePlacementError> {
        if self.aggregate().topology == EffectGraphTopology::GeneralDag {
            return Err(EffectLinearStagePlacementError::GeneralDagRequiresValuePlanner);
        }
        if self.stages().is_empty() {
            return Ok(EffectLinearStagePlacement { steps: Arc::from([]), total_cost: 0 });
        }
        let mut layers = Vec::<Vec<PlacementCandidate>>::with_capacity(self.stages().len());
        for (stage_index, contract) in self.stages().iter().copied().enumerate() {
            let states = placement_states_for_stage(contract, environment);
            if states.is_empty() {
                return Err(EffectLinearStagePlacementError::NoLaneForStage {
                    stage_index,
                    modes: contract.execution_modes,
                });
            }
            if stage_index == 0 {
                layers.push(
                    states
                        .into_iter()
                        .map(|state| PlacementCandidate {
                            state,
                            total_cost: u64::from(
                                environment.lanes[state.lane_index].dispatch_cost,
                            ),
                            predecessor: None,
                            transfer: None,
                        })
                        .collect(),
                );
                continue;
            }

            let previous = layers.last().expect("route has a preceding stage");
            let mut candidates = Vec::with_capacity(states.len());
            let mut cost_overflowed = false;
            for state in states {
                let mut best = None::<PlacementCandidate>;
                for (predecessor, prior) in previous.iter().enumerate() {
                    let transfer = required_transfer(prior.state, state, environment);
                    let Some(transfer_cost) = transfer
                        .map(|index| u64::from(environment.transfers[index].cost))
                        .or_else(|| (prior.state == state).then_some(0))
                    else {
                        continue;
                    };
                    let Some(total_cost) =
                        prior.total_cost.checked_add(transfer_cost).and_then(|cost| {
                            cost.checked_add(u64::from(
                                environment.lanes[state.lane_index].dispatch_cost,
                            ))
                        })
                    else {
                        cost_overflowed = true;
                        continue;
                    };
                    let candidate = PlacementCandidate {
                        state,
                        total_cost,
                        predecessor: Some(predecessor),
                        transfer,
                    };
                    if best.as_ref().is_none_or(|current| candidate.total_cost < current.total_cost)
                    {
                        best = Some(candidate);
                    }
                }
                if let Some(best) = best {
                    candidates.push(best);
                }
            }
            if candidates.is_empty() {
                return Err(if cost_overflowed {
                    EffectLinearStagePlacementError::CostOverflow
                } else {
                    EffectLinearStagePlacementError::NoTransferPlacement { stage_index }
                });
            }
            layers.push(candidates);
        }

        let last_layer = layers.last().expect("non-empty route has a final stage");
        let mut selected = last_layer
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| candidate.total_cost)
            .map(|(index, _)| index)
            .expect("each route layer has a candidate");
        let total_cost = last_layer[selected].total_cost;
        let mut selected_by_stage = vec![0usize; layers.len()];
        for stage_index in (0..layers.len()).rev() {
            selected_by_stage[stage_index] = selected;
            if let Some(predecessor) = layers[stage_index][selected].predecessor {
                selected = predecessor;
            }
        }

        let mut steps = Vec::with_capacity(self.stages().len().saturating_mul(2));
        for (stage_index, candidate_index) in selected_by_stage.into_iter().enumerate() {
            let candidate = layers[stage_index][candidate_index];
            if let Some(transfer_index) = candidate.transfer {
                let transfer = environment.transfers[transfer_index];
                steps.push(EffectLinearStagePlacementStep::Transfer {
                    before_stage: stage_index,
                    from_lane: transfer.from_lane,
                    from_precision: transfer.from_precision,
                    to_lane: transfer.to_lane,
                    to_precision: transfer.to_precision,
                });
            }
            let lane = environment.lanes[candidate.state.lane_index];
            steps.push(EffectLinearStagePlacementStep::Execute {
                stage_index,
                lane: lane.id,
                backend: lane.backend,
                precision: candidate.state.precision,
            });
        }
        Ok(EffectLinearStagePlacement { steps: steps.into(), total_cost })
    }

    /// Admit this program's static demand to one current-frame backend without
    /// implicit transfers or continuity state. A declared invocation input service
    /// remains a separate execution prerequisite; this does not lend that service.
    ///
    /// This proof scans every retained stage directly. It does not call the
    /// linear placement planner, so a General DAG remains admissible when all
    /// of its nodes execute in the same exact mode without transfers.
    pub fn admit_single_frame_backend(
        &self,
        backend: EffectProcessingBackend,
        available_precision: EffectWorkingPrecision,
    ) -> Result<(), EffectExecutionAdmissionError> {
        for (stage_index, contract) in self.stages().iter().copied().enumerate() {
            if !contract.execution_modes.contains(backend, available_precision) {
                return Err(EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                    stage_index,
                    backend,
                    precision: available_precision,
                    admitted: contract.execution_modes,
                });
            }
        }
        let aggregate = self.aggregate();
        if aggregate.state_model != EffectStateModel::Stateless
            || aggregate.resource_lifetime == EffectResourceLifetime::ContinuitySession
        {
            return Err(EffectExecutionAdmissionError::ContinuitySessionRequired);
        }
        if aggregate.temporal_input != EffectTemporalInputExtent::CURRENT_FRAME {
            return Err(EffectExecutionAdmissionError::TemporalInputRequired {
                extent: aggregate.temporal_input,
            });
        }
        Ok(())
    }
}

pub(crate) fn plan_temporal_window(
    output_time: TimelineTime,
    extent: EffectTemporalInputExtent,
) -> Result<EffectTemporalWindow, EffectExecutionDemandError> {
    if matches!(extent.past, EffectTemporalSpan::Finite(duration) if duration.is_negative()) {
        return Err(EffectExecutionDemandError::NegativeTemporalExtent {
            direction: EffectTemporalDirection::Past,
        });
    }
    if matches!(extent.future, EffectTemporalSpan::Finite(duration) if duration.is_negative()) {
        return Err(EffectExecutionDemandError::NegativeTemporalExtent {
            direction: EffectTemporalDirection::Future,
        });
    }
    let earliest = match extent.past {
        EffectTemporalSpan::None => EffectTemporalBoundary::Finite(output_time),
        EffectTemporalSpan::Finite(duration) => {
            EffectTemporalBoundary::Finite(output_time.checked_sub(duration).map_err(|_| {
                EffectExecutionDemandError::TemporalBoundaryOverflow {
                    direction: EffectTemporalDirection::Past,
                }
            })?)
        }
        EffectTemporalSpan::Unbounded => EffectTemporalBoundary::Unbounded,
    };
    let latest = match extent.future {
        EffectTemporalSpan::None => EffectTemporalBoundary::Finite(output_time),
        EffectTemporalSpan::Finite(duration) => {
            EffectTemporalBoundary::Finite(output_time.checked_add(duration).map_err(|_| {
                EffectExecutionDemandError::TemporalBoundaryOverflow {
                    direction: EffectTemporalDirection::Future,
                }
            })?)
        }
        EffectTemporalSpan::Unbounded => EffectTemporalBoundary::Unbounded,
    };
    Ok(EffectTemporalWindow { earliest, latest })
}

fn plan_input_roi(
    output: EffectPixelRoi,
    frame: EffectFrameExtent,
    propagation: EffectRoiPropagation,
) -> EffectInputRoi {
    if output.is_empty() || frame.is_empty() {
        return EffectInputRoi::Exact(output);
    }
    let full = frame.full_frame_roi();
    match propagation {
        EffectRoiPropagation::PixelLocal => classify_exact_roi(output, full),
        EffectRoiPropagation::Expand { horizontal_pixels, vertical_pixels } => classify_exact_roi(
            output.expand_and_clamp(horizontal_pixels, vertical_pixels, frame),
            full,
        ),
        EffectRoiPropagation::FullFrame => EffectInputRoi::ExactFullFrame(full),
        EffectRoiPropagation::UnknownRequiresFullFrame => {
            EffectInputRoi::UnknownConservativeFullFrame(full)
        }
    }
}

fn classify_exact_roi(region: EffectPixelRoi, full: EffectPixelRoi) -> EffectInputRoi {
    if region == full && !region.is_empty() {
        EffectInputRoi::ExactFullFrame(region)
    } else {
        EffectInputRoi::Exact(region)
    }
}

fn ordered_precisions() -> [EffectWorkingPrecision; 3] {
    [
        EffectWorkingPrecision::Float32,
        EffectWorkingPrecision::Float16,
        EffectWorkingPrecision::NormalizedU8,
    ]
}

fn placement_states_for_stage(
    contract: EffectExecutionContract,
    environment: &EffectExecutionEnvironment,
) -> Vec<PlacementState> {
    environment
        .lanes
        .iter()
        .enumerate()
        .flat_map(|(lane_index, lane)| {
            ordered_precisions().into_iter().filter_map(move |precision| {
                (contract.execution_modes.contains(lane.backend, precision)
                    && lane.supported_precisions.contains(precision))
                .then_some(PlacementState { lane_index, precision })
            })
        })
        .collect()
}

fn required_transfer(
    from: PlacementState,
    to: PlacementState,
    environment: &EffectExecutionEnvironment,
) -> Option<usize> {
    if from == to {
        return None;
    }
    let from_lane = environment.lanes[from.lane_index].id;
    let to_lane = environment.lanes[to.lane_index].id;
    environment.transfers.iter().position(|transfer| {
        transfer.from_lane == from_lane
            && transfer.from_precision == from.precision
            && transfer.to_lane == to_lane
            && transfer.to_precision == to.precision
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectDeterminism, EffectGraphTopology};

    #[test]
    fn fractional_upscale_keeps_the_layer_center_on_odd_input_grids() {
        let input = EffectRasterBounds::from_extent(EffectFrameExtent::new(3, 5));
        let scale = EffectRasterScale::from_percent(125.0).unwrap();
        let output = input.checked_apply(EffectRasterChange::Upscale(scale)).unwrap();
        assert_eq!(output.extent(), EffectFrameExtent::new(4, 7));
        assert_eq!(output.origin_half_pixels(), [-1, -2]);
        assert_eq!(output.origin(), [-1, -1]);
        assert_eq!(scale.inverse_factor(), 0.8);
        assert_eq!(
            output.project_affine([2.0, 0.5, 10.0, -0.25, 3.0, 8.0]),
            [2.0, 0.5, 8.5, -0.25, 3.0, 5.125],
        );
    }

    #[test]
    fn upscaling_and_integer_outsets_compose_in_stage_order() {
        let input = EffectRasterBounds::from_extent(EffectFrameExtent::new(3, 5));
        let scale = EffectRasterChange::Upscale(EffectRasterScale::from_percent(150.0).unwrap());
        let outset = EffectRasterChange::Outset([2, 3]);
        let outset_first = input.checked_apply(outset).unwrap().checked_apply(scale).unwrap();
        let upscale_first = input.checked_apply(scale).unwrap().checked_apply(outset).unwrap();
        assert_eq!(outset_first.extent(), EffectFrameExtent::new(11, 17));
        assert_eq!(outset_first.origin_half_pixels(), [-8, -12]);
        assert_eq!(upscale_first.extent(), EffectFrameExtent::new(9, 14));
        assert_eq!(upscale_first.origin_half_pixels(), [-6, -9]);
        for grid in [outset_first, upscale_first] {
            let [x, y] = grid.origin_half_pixels();
            assert_eq!(
                [
                    x + i64::from(grid.extent().width()),
                    y + i64::from(grid.extent().height())
                ],
                [3, 5]
            );
        }
    }

    #[test]
    fn upscale_ratio_checks_author_range_empty_extents_and_overflow() {
        for percent in [
            0.0,
            100.0_f32.next_down(),
            30_000.0_f32.next_up(),
            f32::NAN,
            f32::INFINITY,
        ] {
            assert!(EffectRasterScale::from_percent(percent).is_none());
        }
        let identity = EffectRasterScale::from_percent(100.0).unwrap();
        let maximum = EffectRasterScale::from_percent(30_000.0).unwrap();
        let large = EffectFrameExtent::new(u32::MAX, 1);
        assert_eq!(identity.checked_extent(large), Some(large));
        assert_eq!(maximum.checked_extent(large), None);
        let empty = EffectFrameExtent::new(0, u32::MAX);
        assert_eq!(maximum.checked_extent(empty), Some(empty));
        let next = EffectRasterScale::from_percent(100.0_f32.next_up()).unwrap();
        assert_eq!(
            next.checked_extent(EffectFrameExtent::new(1, 3)),
            Some(EffectFrameExtent::new(2, 4))
        );
    }

    #[test]
    fn upscale_levels_round_only_the_final_extent() {
        let scale = EffectRasterScale::from_percent(350.0).unwrap();
        let input = EffectFrameExtent::new(3, 5);
        let stages = scale.stages(input).unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].input_extent(), input);
        assert_eq!(stages[0].output_extent(), EffectFrameExtent::new(6, 10));
        assert_eq!(stages[1].input_extent(), stages[0].output_extent());
        assert_eq!(stages[1].output_extent(), EffectFrameExtent::new(11, 18));

        let unit = EffectFrameExtent::new(1, 1);
        let maximum = EffectRasterScale::from_percent(30_000.0).unwrap();
        let levels = maximum.stages(unit).unwrap();
        assert_eq!(levels.len(), 9);
        assert_eq!(levels[7].output_extent(), EffectFrameExtent::new(256, 256));
        assert_eq!(levels[8].output_extent(), EffectFrameExtent::new(300, 300));
        let identity = EffectRasterScale::from_percent(100.0).unwrap();
        assert!(identity.stages(unit).unwrap().is_empty());
    }

    #[test]
    fn upscale_axis_samples_use_exact_centers_and_clamp_single_pixel_axes() {
        let scale = EffectRasterScale::from_percent(125.0).unwrap();
        let levels = scale.stages(EffectFrameExtent::new(3, 1)).unwrap();
        let stage = levels[0];
        let columns = stage.column_samples().collect::<Vec<_>>();
        assert_eq!(
            columns,
            [
                EffectRasterAxisSample { base: 0, fraction: 0.0 },
                EffectRasterAxisSample { base: 0, fraction: 0.6 },
                EffectRasterAxisSample { base: 1, fraction: 0.4 },
                EffectRasterAxisSample { base: 2, fraction: 0.0 },
            ]
        );
        assert_eq!(
            stage.row_samples().collect::<Vec<_>>(),
            [EffectRasterAxisSample { base: 0, fraction: 0.0 }; 2],
        );
    }

    fn tt(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).expect("valid test time")
    }

    fn contract(execution_modes: EffectExecutionModes) -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        }
    }

    fn envelope(stages: Vec<EffectExecutionContract>) -> EffectExecutionEnvelope {
        let aggregate = stages
            .iter()
            .copied()
            .try_fold(
                EffectExecutionContract::IDENTITY,
                EffectExecutionContract::compose,
            )
            .expect("valid stage contracts");
        EffectExecutionEnvelope::new(aggregate, Arc::<[EffectExecutionContract]>::from(stages))
    }

    #[test]
    fn temporal_window_preserves_signed_history_and_keeps_unbounded_explicit() {
        let temporal = EffectTemporalInputExtent {
            past: EffectTemporalSpan::Finite(tt(1, 1)),
            future: EffectTemporalSpan::Unbounded,
        };
        let stage = EffectExecutionContract {
            temporal_input: temporal,
            ..EffectExecutionContract::IDENTITY
        };
        let demand = envelope(vec![stage])
            .plan_execution_demand(
                tt(1, 4),
                EffectFrameExtent::new(1920, 1080),
                EffectPixelRoi::new(0, 0, 1, 1),
            )
            .expect("bounded demand");
        assert_eq!(
            demand.temporal_window(),
            EffectTemporalWindow {
                earliest: EffectTemporalBoundary::Finite(tt(-3, 4)),
                latest: EffectTemporalBoundary::Unbounded,
            }
        );
        assert_eq!(demand.temporal_extent(), temporal);
    }

    #[test]
    fn temporal_window_accepts_negative_output_and_rejects_future_overflow() {
        let identity = EffectExecutionEnvelope::identity();
        assert_eq!(
            identity
                .plan_execution_demand(
                    TimelineTime::NEGATIVE_ONE,
                    EffectFrameExtent::new(1, 1),
                    EffectPixelRoi::new(0, 0, 1, 1),
                )
                .expect("signed owner-domain output")
                .temporal_window(),
            EffectTemporalWindow {
                earliest: EffectTemporalBoundary::Finite(TimelineTime::NEGATIVE_ONE),
                latest: EffectTemporalBoundary::Finite(TimelineTime::NEGATIVE_ONE),
            }
        );

        let stage = EffectExecutionContract {
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::None,
                future: EffectTemporalSpan::Finite(TimelineTime::ONE),
            },
            ..EffectExecutionContract::IDENTITY
        };
        assert_eq!(
            envelope(vec![stage]).plan_execution_demand(
                tt(i64::MAX, 1),
                EffectFrameExtent::new(1, 1),
                EffectPixelRoi::new(0, 0, 1, 1),
            ),
            Err(EffectExecutionDemandError::TemporalBoundaryOverflow {
                direction: EffectTemporalDirection::Future,
            })
        );

        let negative_extent = EffectExecutionContract {
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(TimelineTime::NEGATIVE_ONE),
                future: EffectTemporalSpan::None,
            },
            ..EffectExecutionContract::IDENTITY
        };
        assert_eq!(
            EffectExecutionEnvelope::new(negative_extent, Arc::from([negative_extent]),)
                .plan_execution_demand(
                    TimelineTime::ZERO,
                    EffectFrameExtent::new(1, 1),
                    EffectPixelRoi::new(0, 0, 1, 1),
                ),
            Err(EffectExecutionDemandError::NegativeTemporalExtent {
                direction: EffectTemporalDirection::Past,
            })
        );

        let past_overflow = EffectExecutionContract {
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(tt(1, i64::MAX)),
                future: EffectTemporalSpan::None,
            },
            ..EffectExecutionContract::IDENTITY
        };
        assert_eq!(
            envelope(vec![past_overflow]).plan_execution_demand(
                tt(1, 2),
                EffectFrameExtent::new(1, 1),
                EffectPixelRoi::new(0, 0, 1, 1),
            ),
            Err(EffectExecutionDemandError::TemporalBoundaryOverflow {
                direction: EffectTemporalDirection::Past,
            })
        );
    }

    #[test]
    fn roi_expansion_clamps_edges_and_never_wraps() {
        let stage = EffectExecutionContract {
            roi_propagation: EffectRoiPropagation::Expand {
                horizontal_pixels: u32::MAX,
                vertical_pixels: u32::MAX,
            },
            ..EffectExecutionContract::IDENTITY
        };
        let demand = envelope(vec![stage])
            .plan_execution_demand(
                TimelineTime::ZERO,
                EffectFrameExtent::new(16, 9),
                EffectPixelRoi::new(u32::MAX - 2, u32::MAX - 2, u32::MAX, u32::MAX),
            )
            .expect("overflow-free ROI");
        assert_eq!(demand.output_roi(), EffectPixelRoi::new(16, 9, 0, 0));
        assert_eq!(
            demand.input_roi(),
            EffectInputRoi::Exact(EffectPixelRoi::new(16, 9, 0, 0))
        );

        let demand = envelope(vec![stage])
            .plan_execution_demand(
                TimelineTime::ZERO,
                EffectFrameExtent::new(16, 9),
                EffectPixelRoi::new(15, 8, u32::MAX, u32::MAX),
            )
            .expect("expanded ROI");
        assert_eq!(
            demand.input_roi(),
            EffectInputRoi::ExactFullFrame(EffectPixelRoi::new(0, 0, 16, 9))
        );
    }

    #[test]
    fn empty_and_unknown_roi_do_not_claim_the_same_evidence() {
        let full = EffectExecutionContract {
            roi_propagation: EffectRoiPropagation::FullFrame,
            ..EffectExecutionContract::IDENTITY
        };
        let unknown = EffectExecutionContract {
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        };
        let extent = EffectFrameExtent::new(100, 50);
        let output = EffectPixelRoi::new(10, 10, 5, 5);
        assert_eq!(
            envelope(vec![full])
                .plan_execution_demand(TimelineTime::ZERO, extent, output)
                .expect("full-frame demand")
                .input_roi(),
            EffectInputRoi::ExactFullFrame(extent.full_frame_roi())
        );
        assert_eq!(
            envelope(vec![unknown])
                .plan_execution_demand(TimelineTime::ZERO, extent, output)
                .expect("unknown demand")
                .input_roi(),
            EffectInputRoi::UnknownConservativeFullFrame(extent.full_frame_roi())
        );
        assert_eq!(
            envelope(vec![unknown])
                .plan_execution_demand(TimelineTime::ZERO, extent, EffectPixelRoi::new(4, 4, 0, 7),)
                .expect("empty demand")
                .input_roi(),
            EffectInputRoi::Exact(EffectPixelRoi::new(4, 4, 0, 7))
        );
    }

    #[test]
    fn heterogeneous_linear_placement_requires_and_uses_explicit_transfer() {
        let cpu = EffectExecutionLaneId::new(1);
        let gpu = EffectExecutionLaneId::new(2);
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    cpu,
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::FLOAT32,
                    10,
                ),
                EffectExecutionLane::new(
                    gpu,
                    EffectProcessingBackend::Gpu,
                    EffectWorkingPrecisions::FLOAT32,
                    1,
                ),
            ]),
            Arc::from([EffectExecutionTransfer::new(
                cpu,
                EffectWorkingPrecision::Float32,
                gpu,
                EffectWorkingPrecision::Float32,
                3,
            )]),
        )
        .expect("valid environment");
        let placement = envelope(vec![
            contract(EffectExecutionModes::CPU_F32),
            contract(EffectExecutionModes::GPU_F32),
        ])
        .plan_linear_stage_placement(&environment)
        .expect("explicit heterogeneous placement");
        assert_eq!(
            placement.steps(),
            &[
                EffectLinearStagePlacementStep::Execute {
                    stage_index: 0,
                    lane: cpu,
                    backend: EffectProcessingBackend::Cpu,
                    precision: EffectWorkingPrecision::Float32,
                },
                EffectLinearStagePlacementStep::Transfer {
                    before_stage: 1,
                    from_lane: cpu,
                    from_precision: EffectWorkingPrecision::Float32,
                    to_lane: gpu,
                    to_precision: EffectWorkingPrecision::Float32,
                },
                EffectLinearStagePlacementStep::Execute {
                    stage_index: 1,
                    lane: gpu,
                    backend: EffectProcessingBackend::Gpu,
                    precision: EffectWorkingPrecision::Float32,
                },
            ]
        );
        assert_eq!(placement.total_cost(), 14);
    }

    #[test]
    fn heterogeneous_linear_placement_fails_closed_without_transfer() {
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    EffectExecutionLaneId::new(1),
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::FLOAT32,
                    0,
                ),
                EffectExecutionLane::new(
                    EffectExecutionLaneId::new(2),
                    EffectProcessingBackend::Gpu,
                    EffectWorkingPrecisions::FLOAT32,
                    0,
                ),
            ]),
            Arc::from([]),
        )
        .expect("valid environment");
        assert_eq!(
            envelope(vec![
                contract(EffectExecutionModes::CPU_F32),
                contract(EffectExecutionModes::GPU_F32),
            ])
            .plan_linear_stage_placement(&environment),
            Err(EffectLinearStagePlacementError::NoTransferPlacement { stage_index: 1 })
        );
    }

    #[test]
    fn linear_placement_honors_exact_modes_and_representation_transfer() {
        let cpu_u8 = EffectExecutionLaneId::new(1);
        let cpu_float = EffectExecutionLaneId::new(2);
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    cpu_u8,
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::NORMALIZED_U8,
                    0,
                ),
                EffectExecutionLane::new(
                    cpu_float,
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::FLOAT32,
                    0,
                ),
            ]),
            Arc::from([EffectExecutionTransfer::new(
                cpu_u8,
                EffectWorkingPrecision::NormalizedU8,
                cpu_float,
                EffectWorkingPrecision::Float32,
                1,
            )]),
        )
        .expect("valid precision environment");
        let program = envelope(vec![
            contract(EffectExecutionModes::CPU_U8),
            contract(EffectExecutionModes::CPU_F32),
        ]);
        let demand = program
            .plan_execution_demand(
                TimelineTime::ZERO,
                EffectFrameExtent::new(1, 1),
                EffectPixelRoi::new(0, 0, 1, 1),
            )
            .expect("precision demand");
        assert!(demand.obligations().execution_modes().homogeneous().is_empty());
        assert!(program.plan_linear_stage_placement(&environment).is_ok());
        assert_eq!(
            program.admit_single_frame_backend(
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecision::Float32,
            ),
            Err(EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                stage_index: 0,
                backend: EffectProcessingBackend::Cpu,
                precision: EffectWorkingPrecision::Float32,
                admitted: EffectExecutionModes::CPU_U8,
            })
        );
    }

    #[test]
    fn general_dag_never_masquerades_as_a_linear_data_route() {
        let mut stage = contract(EffectExecutionModes::CPU_F32);
        stage.topology = EffectGraphTopology::GeneralDag;
        let program = envelope(vec![stage]);
        let environment = EffectExecutionEnvironment::new(
            Arc::from([EffectExecutionLane::new(
                EffectExecutionLaneId::new(1),
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecisions::FLOAT32,
                0,
            )]),
            Arc::from([]),
        )
        .expect("valid environment");

        assert_eq!(
            program.plan_linear_stage_placement(&environment),
            Err(EffectLinearStagePlacementError::GeneralDagRequiresValuePlanner)
        );
        assert!(program
            .admit_single_frame_backend(
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecision::Float32,
            )
            .is_ok());
    }
}
