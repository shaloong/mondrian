//! Shared geometry for the Mask Path Stroke effect (not the source-alpha contour Stroke).
//!
//! Layout, arc length and trim are in physical f64 pixels. Only the final primitives,
//! pixel scale and acceleration structure are rounded to the Float32 render contract.
//! Shader and scalar consumers must use the same recorded payload and absolute pixel
//! centers; the source is sampled only at the destination pixel.

use crate::mask::{MaskShape, MAX_MASK_PATH_POINTS};
use crate::stroke::StrokeGrade;
use crate::{EffectFrameExtent, EffectPixelRoi};
use glam::DVec2;
use mondrian_core::{ExecutionCancellationToken, ImageSampling};
use std::mem::{size_of, size_of_val};

/// Maximum number of resolved shapes, including empty ones.
pub const MASK_PATH_STROKE_MAX_PATHS: usize = 1_024;
/// Aggregate authored Bezier point limit, in addition to the per-mask limit.
pub const MASK_PATH_STROKE_MAX_AUTHORED_POINTS: usize = 65_536;
/// Maximum total number of f64 polyline vertices before trim.
pub const MASK_PATH_STROKE_MAX_FLATTENED_VERTICES: usize = 262_144;
/// Maximum total number of finalized capsules or dabs.
pub const MASK_PATH_STROKE_MAX_PRIMITIVES: usize = 262_144;
/// Maximum node count in the balanced, eight-primitive-leaf index.
pub const MASK_PATH_STROKE_MAX_BVH_NODES: usize = 65_535;
/// Largest admitted physical coordinate magnitude, including the frame and handles.
/// Keeps Float32 endpoint quantization and squared-distance arithmetic bounded.
pub const MASK_PATH_STROKE_MAX_COORDINATE: f64 = 1_048_576.0;
/// Subdivision error target (physical pixels), not an exact arc-length tolerance.
pub const MASK_PATH_STROKE_FLATNESS: f64 = 0.125;
/// Maximum number of physical subdivisions down any one curve branch.
pub const MASK_PATH_STROKE_MAX_SUBDIVISION_DEPTH: u8 = 20;
/// Bit-exact f64 integer range used to derive fixed-phase dab positions with headroom
/// for the adjacent representable positions. The budget is independent of trim size.
pub const MASK_PATH_STROKE_MAX_DAB_PHASE_INDEX: u64 = 1 << 48;
/// Each storage binding is at most 4 MiB (the node binding is less than 2 MiB).
pub const MASK_PATH_STROKE_MAX_BINDING_BYTES: u64 = 4 * 1024 * 1024;
/// Conservative peak retained logical allocation, including the layout itself.
pub const MASK_PATH_STROKE_MAX_RETAINED_BYTES: usize = size_of::<MaskPathStrokeLayout>()
    + MASK_PATH_STROKE_MAX_PRIMITIVES * size_of::<MaskPathStrokePrimitive>()
    + MASK_PATH_STROKE_MAX_BVH_NODES * size_of::<MaskPathStrokeBvhNode>();
/// Conservative peak extra preparation heap bytes (not including input shapes or GPU
/// staging): f64 vertices, path descriptors, and a relocating vector allocation.
/// Curve and BVH recursion use only bounded stack depth; the query is stackless.
pub const MASK_PATH_STROKE_MAX_PREPARE_SCRATCH_BYTES: usize =
    2 * MASK_PATH_STROKE_MAX_FLATTENED_VERTICES * size_of::<Vertex>()
        + MASK_PATH_STROKE_MAX_PATHS * size_of::<FlatPath>();

const LEAF_PRIMITIVES: usize = 8;
const CHECKPOINT_STRIDE: usize = 4_096;

/// Validated authored layout controls. All three Float values use percentages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskPathStrokeControls {
    sequential: bool,
    start_percent: f32,
    end_percent: f32,
    spacing_percent: f32,
}

impl MaskPathStrokeControls {
    /// Accept finite controls in `0..=100`. `end <= start` is a valid empty trim.
    pub fn new(
        sequential: bool,
        start_percent: f32,
        end_percent: f32,
        spacing_percent: f32,
    ) -> Result<Self, MaskPathStrokeError> {
        for (name, value) in [
            ("start", start_percent),
            ("end", end_percent),
            ("spacing", spacing_percent),
        ] {
            if !(0.0..=100.0).contains(&value) {
                return Err(MaskPathStrokeError::InvalidControl { name });
            }
        }
        Ok(Self {
            sequential,
            start_percent,
            end_percent,
            spacing_percent,
        })
    }

    /// Whether the authored paths share one trim axis.
    pub const fn sequential(self) -> bool {
        self.sequential
    }

    /// Authored start percentage.
    pub const fn start_percent(self) -> f32 {
        self.start_percent
    }

    /// Authored end percentage.
    pub const fn end_percent(self) -> f32 {
        self.end_percent
    }

    /// Dab spacing as a percentage of the diameter, or zero for capsules.
    pub const fn spacing_percent(self) -> f32 {
        self.spacing_percent
    }
}

impl Default for MaskPathStrokeControls {
    fn default() -> Self {
        Self {
            sequential: true,
            start_percent: 0.0,
            end_percent: 100.0,
            spacing_percent: 15.0,
        }
    }
}

/// Observable geometry, resource, or raster-data failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MaskPathStrokeError {
    /// Token-based preparation or execution was canceled.
    #[error("mask path stroke was canceled")]
    Canceled,
    /// One layout control was non-finite or out of range.
    #[error("mask path stroke {name} must be finite and within 0..=100")]
    InvalidControl { name: &'static str },
    /// A resolved shape did not satisfy the mask/numerical contract.
    #[error("invalid mask path stroke geometry: {reason}")]
    InvalidGeometry { reason: &'static str },
    /// Layout exceeded an explicitly bounded resource.
    #[error("mask path stroke {resource} budget exceeded (limit {limit})")]
    BudgetExceeded {
        resource: &'static str,
        limit: usize,
    },
    /// The requested lattice phase is too large to evaluate reliably.
    #[error("mask path stroke dab phase exceeds the supported lattice index")]
    DabPhaseLimitExceeded,
    /// A bounded allocation could not be made.
    #[error("mask path stroke could not allocate {resource}")]
    AllocationFailed { resource: &'static str },
    /// The ROI is not contained in the prepared full extent.
    #[error("mask path stroke region is outside its prepared frame extent")]
    RegionOutsideExtent,
    /// The provided buffer does not contain exactly one straight RGBA per ROI pixel.
    #[error("mask path stroke RGBA buffer does not match its region")]
    InvalidPixelBuffer,
}

/// Preserve the exact stop issued by the execution attempt rather than folding
/// cancellation, deadline, or any other scheduling decision together.
#[derive(Debug, PartialEq, Eq)]
pub enum ControlledMaskPathStrokeError<E> {
    /// Geometry or raster-data failure.
    Layout(MaskPathStrokeError),
    /// The caller's original checkpoint error.
    Checkpoint(E),
}

impl<E> ControlledMaskPathStrokeError<E> {
    pub(crate) fn into_execution_error(
        self,
        map_layout: impl FnOnce(MaskPathStrokeError) -> E,
    ) -> E {
        match self {
            Self::Layout(error) => map_layout(error),
            Self::Checkpoint(error) => error,
        }
    }
}

impl<E> From<MaskPathStrokeError> for ControlledMaskPathStrokeError<E> {
    fn from(error: MaskPathStrokeError) -> Self {
        Self::Layout(error)
    }
}

/// One 16-byte storage record (`vec4<f32>`): start.xy, end.zw. A dab has equal ends.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskPathStrokePrimitive {
    /// Float32 physical endpoint coordinates.
    pub endpoints: [f32; 4],
}

impl MaskPathStrokePrimitive {
    fn new(a: DVec2, b: DVec2) -> Self {
        Self {
            endpoints: [a.x as f32, a.y as f32, b.x as f32, b.y as f32],
        }
    }

    fn center(self, axis: usize) -> f32 {
        (self.endpoints[axis] + self.endpoints[axis + 2]) * 0.5
    }

    fn distance(self, [px, py]: [f32; 2]) -> f32 {
        let [ax, ay, bx, by] = self.endpoints;
        let dx = bx - ax;
        let dy = by - ay;
        let length_squared = dx * dx + dy * dy;
        // Use a normal-float threshold so subnormal differences need not survive
        // GPU arithmetic. Clamp the computed projection to the endpoint AABB.
        let t = if length_squared < f32::MIN_POSITIVE {
            0.0
        } else {
            (((px - ax) * dx + (py - ay) * dy) / length_squared).clamp(0.0, 1.0)
        };
        let x = px - (ax + t * dx).clamp(ax.min(bx), ax.max(bx));
        let y = py - (ay + t * dy).clamp(ay.min(by), ay.max(by));
        (x * x + y * y).sqrt()
    }
}

/// One 32-byte storage record (`vec4<f32>`, `vec4<u32>`), with no padding bytes.
/// Nodes are in preorder, root at index zero. If the point is outside the
/// inclusive bound, advance to `escape`. Otherwise advance by one on a branch,
/// or inspect the contiguous primitive range and advance to `escape` on a leaf.
/// The escape of the root is the node count; an empty layout has no nodes.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskPathStrokeBvhNode {
    /// Conservative support bounds: [min x, min y, max x, max y].
    pub bounds: [f32; 4],
    /// `[escape, first_primitive, primitive_count, 0]`; count zero denotes a branch.
    pub metadata: [u32; 4],
}

impl MaskPathStrokeBvhNode {
    /// Encode one node for serialization as two 16-byte GPU vectors.
    pub fn storage_words(self) -> [u32; 8] {
        let [x0, y0, x1, y1] = self.bounds.map(f32::to_bits);
        let [escape, first, count, reserved] = self.metadata;
        [x0, y0, x1, y1, escape, first, count, reserved]
    }
}

const _: () = assert!(size_of::<MaskPathStrokePrimitive>() == 16);
const _: () = assert!(size_of::<MaskPathStrokeBvhNode>() == 32);
const _: () = assert!(
    MASK_PATH_STROKE_MAX_PRIMITIVES as u64 * size_of::<MaskPathStrokePrimitive>() as u64
        <= MASK_PATH_STROKE_MAX_BINDING_BYTES
);
const _: () = assert!(
    MASK_PATH_STROKE_MAX_BVH_NODES as u64 * size_of::<MaskPathStrokeBvhNode>() as u64
        <= MASK_PATH_STROKE_MAX_BINDING_BYTES
);

/// Immutable layout for one full extent and sampling declaration.
#[derive(Debug, PartialEq)]
pub struct MaskPathStrokeLayout {
    extent: EffectFrameExtent,
    pixel_scale: [f32; 2],
    grade: StrokeGrade,
    primitives: Vec<MaskPathStrokePrimitive>,
    nodes: Vec<MaskPathStrokeBvhNode>,
}

impl MaskPathStrokeLayout {
    /// Prepare with a cancellation token; use the controlled form to retain a
    /// scheduler's richer stop signaling.
    pub fn prepare(
        shapes: &[MaskShape],
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        grade: StrokeGrade,
        controls: MaskPathStrokeControls,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Self, MaskPathStrokeError> {
        Self::prepare_controlled(shapes, extent, sampling, grade, controls, &mut || {
            token_checkpoint(cancellation)
        })
        .map_err(flatten_token_error)
    }

    /// Prepare resolved shapes in author order, using complete image geometry.
    /// No source pixels are read and no coverage image is created.
    pub fn prepare_controlled<E>(
        shapes: &[MaskShape],
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        grade: StrokeGrade,
        controls: MaskPathStrokeControls,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, ControlledMaskPathStrokeError<E>> {
        let mut work = Work::new(checkpoint);
        work.check()?;
        let [sx, sy] = sampling.render_scale();
        let par = sampling.sample_aspect_ratio();
        let scale = DVec2::new(
            (f64::from(par.numerator()) / f64::from(par.denominator())) / sx.to_f64(),
            1.0 / sy.to_f64(),
        );
        let physical_extent =
            scale * DVec2::new(f64::from(extent.width()), f64::from(extent.height()));
        let pixel_scale = [scale.x as f32, scale.y as f32];
        if !pixel_scale.into_iter().all(|axis| axis.is_finite() && axis > 0.0) {
            return Err(MaskPathStrokeError::InvalidGeometry {
                reason: "physical pixel scale is not positive finite Float32",
            }
            .into());
        }
        let mut layout = Self {
            extent,
            pixel_scale,
            grade,
            primitives: Vec::new(),
            nodes: Vec::new(),
        };
        if extent.is_empty()
            || grade.is_identity()
            || controls.end_percent <= controls.start_percent
            || shapes.is_empty()
        {
            return Ok(layout);
        }
        checked_position(physical_extent)?;
        admit_shapes(shapes, &mut work)?;
        layout.primitives = make_primitives(shapes, physical_extent, grade, controls, &mut work)?;
        if !layout.primitives.is_empty() {
            let leaf_capacity =
                (layout.primitives.len().next_power_of_two() / LEAF_PRIMITIVES).max(1);
            let node_capacity = 2 * leaf_capacity - 1;
            layout
                .nodes
                .try_reserve_exact(node_capacity)
                .map_err(|_| MaskPathStrokeError::AllocationFailed { resource: "BVH nodes" })?;
            build_bvh(
                &mut layout.primitives,
                &mut layout.nodes,
                0,
                grade.radius(),
                &mut work,
            )?;
        }
        work.check()?;
        Ok(layout)
    }

    /// Full input extent, not the current tile or ROI dimensions.
    pub const fn extent(&self) -> EffectFrameExtent {
        self.extent
    }

    /// Multiply `(Float32(absolute_xy) + 0.5)` by these physical pixel scales.
    pub const fn pixel_scale(&self) -> [f32; 2] {
        self.pixel_scale
    }

    /// The same Float32 profile and straight-alpha blend used by contour Stroke.
    pub const fn grade(&self) -> StrokeGrade {
        self.grade
    }

    /// A passthrough layout requires no raster computation or GPU bindings.
    pub fn is_empty(&self) -> bool {
        self.primitives.is_empty()
    }

    /// Final capsules/dabs, reordered into consecutive index leaf ranges.
    pub fn primitives(&self) -> &[MaskPathStrokePrimitive] {
        &self.primitives
    }

    /// Preorder spatial nodes; index zero is the root when not empty.
    pub fn nodes(&self) -> &[MaskPathStrokeBvhNode] {
        &self.nodes
    }

    /// Actual payload bytes (primitive buffer, node buffer) before device alignment.
    pub fn gpu_storage_bytes(&self) -> [u64; 2] {
        [
            size_of_val(self.primitives()) as u64,
            size_of_val(self.nodes()) as u64,
        ]
    }

    /// Logical heap capacity plus the layout value. No pixel or traversal heap is needed.
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.primitives.capacity() * size_of::<MaskPathStrokePrimitive>()
            + self.nodes.capacity() * size_of::<MaskPathStrokeBvhNode>()
    }

    /// Maximum pre-opacity coverage at one absolute pixel, or zero outside the extent.
    /// The controlled ROI method also checks for stops within a dense query.
    pub fn coverage_at_pixel(&self, x: u32, y: u32) -> f32 {
        if x >= self.extent.width() || y >= self.extent.height() {
            return 0.0;
        }
        match self.coverage(self.pixel_center(x, y), || {
            Ok::<(), std::convert::Infallible>(())
        }) {
            Ok(value) => value,
            Err(never) => match never {},
        }
    }

    /// Composite over the caller's tightly packed, straight Float32 ROI buffer.
    pub fn apply_region_f32(
        &self,
        pixels: &mut [[f32; 4]],
        region: EffectPixelRoi,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), MaskPathStrokeError> {
        self.apply_region_f32_controlled(pixels, region, &mut || token_checkpoint(cancellation))
            .map_err(flatten_token_error)
    }

    /// Composite in place; discard the output buffer on failure or a checkpoint stop.
    /// Source and destination are pixel-local, so no source snapshot is necessary.
    pub fn apply_region_f32_controlled<E>(
        &self,
        pixels: &mut [[f32; 4]],
        region: EffectPixelRoi,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), ControlledMaskPathStrokeError<E>> {
        let mut work = Work::new(checkpoint);
        work.check()?;
        let x_end = region.x().checked_add(region.width()).filter(|&x| x <= self.extent.width());
        let y_end = region.y().checked_add(region.height()).filter(|&y| y <= self.extent.height());
        let (Some(x_end), Some(y_end)) = (x_end, y_end) else {
            return Err(MaskPathStrokeError::RegionOutsideExtent.into());
        };
        let count = u64::from(region.width()) * u64::from(region.height());
        if usize::try_from(count).ok() != Some(pixels.len()) {
            return Err(MaskPathStrokeError::InvalidPixelBuffer.into());
        }
        if self.is_empty() || pixels.is_empty() {
            return Ok(());
        }
        let mut pixels = pixels.iter_mut();
        for y in region.y()..y_end {
            for x in region.x()..x_end {
                work.tick()?;
                let coverage = self.coverage(self.pixel_center(x, y), || work.tick())?;
                let pixel = pixels.next().expect("ROI pixel count checked");
                if coverage > 0.0 {
                    *pixel = self.grade.blend(*pixel, coverage);
                }
            }
        }
        work.check()
    }

    fn pixel_center(&self, x: u32, y: u32) -> [f32; 2] {
        [
            (x as f32 + 0.5) * self.pixel_scale[0],
            (y as f32 + 0.5) * self.pixel_scale[1],
        ]
    }

    fn coverage<E>(
        &self,
        point: [f32; 2],
        mut tick: impl FnMut() -> Result<(), E>,
    ) -> Result<f32, E> {
        let mut best = 0.0_f32;
        let mut index = 0;
        while let Some(node) = self.nodes.get(index) {
            tick()?;
            let [escape, start, count, _] = node.metadata;
            let [x0, y0, x1, y1] = node.bounds;
            if point[0] < x0 || point[0] > x1 || point[1] < y0 || point[1] > y1 {
                index = escape as usize;
            } else if count == 0 {
                index += 1;
            } else {
                for primitive in &self.primitives[start as usize..(start + count) as usize] {
                    tick()?;
                    best = best.max(self.grade.coverage_at_distance(primitive.distance(point)));
                    if best == 1.0 {
                        return Ok(best);
                    }
                }
                index = escape as usize;
            }
        }
        Ok(best)
    }
}

fn token_checkpoint(cancellation: &ExecutionCancellationToken) -> Result<(), MaskPathStrokeError> {
    if cancellation.is_canceled() {
        Err(MaskPathStrokeError::Canceled)
    } else {
        Ok(())
    }
}

fn flatten_token_error(
    error: ControlledMaskPathStrokeError<MaskPathStrokeError>,
) -> MaskPathStrokeError {
    match error {
        ControlledMaskPathStrokeError::Layout(error)
        | ControlledMaskPathStrokeError::Checkpoint(error) => error,
    }
}

struct Work<'a, F> {
    checkpoint: &'a mut F,
    since_checkpoint: usize,
}

impl<'a, F> Work<'a, F> {
    fn new(checkpoint: &'a mut F) -> Self {
        Self { checkpoint, since_checkpoint: 0 }
    }
}

impl<F> Work<'_, F> {
    fn tick<E>(&mut self) -> Result<(), ControlledMaskPathStrokeError<E>>
    where
        F: FnMut() -> Result<(), E>,
    {
        self.since_checkpoint += 1;
        if self.since_checkpoint >= CHECKPOINT_STRIDE {
            self.check()?;
        }
        Ok(())
    }

    fn check<E>(&mut self) -> Result<(), ControlledMaskPathStrokeError<E>>
    where
        F: FnMut() -> Result<(), E>,
    {
        self.since_checkpoint = 0;
        (self.checkpoint)().map_err(ControlledMaskPathStrokeError::Checkpoint)
    }
}

fn push_bounded<T>(
    items: &mut Vec<T>,
    item: T,
    limit: usize,
    resource: &'static str,
) -> Result<(), MaskPathStrokeError> {
    if items.len() >= limit {
        return Err(MaskPathStrokeError::BudgetExceeded { resource, limit });
    }
    if items.len() == items.capacity() {
        let target = (items.len() * 2).clamp(1, limit);
        items
            .try_reserve_exact(target - items.len())
            .map_err(|_| MaskPathStrokeError::AllocationFailed { resource })?;
    }
    items.push(item);
    Ok(())
}

fn admit_shapes<E>(
    shapes: &[MaskShape],
    work: &mut Work<'_, impl FnMut() -> Result<(), E>>,
) -> Result<(), ControlledMaskPathStrokeError<E>> {
    if shapes.len() > MASK_PATH_STROKE_MAX_PATHS {
        return Err(MaskPathStrokeError::BudgetExceeded {
            resource: "resolved shapes",
            limit: MASK_PATH_STROKE_MAX_PATHS,
        }
        .into());
    }
    let mut total_points = 0;
    for shape in shapes {
        work.tick()?;
        if let MaskShape::Path { points, .. } = shape {
            if points.len() > MAX_MASK_PATH_POINTS {
                return Err(MaskPathStrokeError::BudgetExceeded {
                    resource: "points per Mask Path",
                    limit: MAX_MASK_PATH_POINTS,
                }
                .into());
            }
            total_points += points.len();
            if total_points > MASK_PATH_STROKE_MAX_AUTHORED_POINTS {
                return Err(MaskPathStrokeError::BudgetExceeded {
                    resource: "authored points",
                    limit: MASK_PATH_STROKE_MAX_AUTHORED_POINTS,
                }
                .into());
            }
        }
    }
    Ok(())
}

fn checked_position(point: DVec2) -> Result<DVec2, MaskPathStrokeError> {
    if point.is_finite() && point.abs().max_element() <= MASK_PATH_STROKE_MAX_COORDINATE {
        Ok(point)
    } else {
        Err(MaskPathStrokeError::InvalidGeometry {
            reason: "frame or shape exceeds the finite physical coordinate range",
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Vertex {
    point: DVec2,
    distance: f64,
}

struct FlatPath {
    start: usize,
    end: usize,
    closed: bool,
}

struct Flattener<'a> {
    vertices: &'a mut Vec<Vertex>,
    first: usize,
}

impl Flattener<'_> {
    fn line_to(&mut self, point: DVec2) -> Result<(), MaskPathStrokeError> {
        let point = checked_position(point)?;
        let distance = if self.first == self.vertices.len() {
            0.0
        } else {
            let previous = self.vertices[self.vertices.len() - 1];
            let distance = previous.distance + point.distance(previous.point);
            // A coincident (or f64-indistinguishable) edge adds no arc length.
            if distance == previous.distance {
                return Ok(());
            }
            distance
        };
        push_bounded(
            self.vertices,
            Vertex { point, distance },
            MASK_PATH_STROKE_MAX_FLATTENED_VERTICES,
            "flattened vertices",
        )
    }

    fn cubic_to<E>(
        &mut self,
        [a, b, c, d]: [DVec2; 4],
        depth: u8,
        work: &mut Work<'_, impl FnMut() -> Result<(), E>>,
    ) -> Result<(), ControlledMaskPathStrokeError<E>> {
        work.tick()?;
        let chord = d - a;
        let chord_length = chord.length();
        let control_length = a.distance(b) + b.distance(c) + c.distance(d);
        let distance_to_chord = |point: DVec2| {
            let t = if chord_length > 0.0 {
                ((point - a).dot(chord) / chord.length_squared()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            point.distance(a + chord * t)
        };
        // Check both deviation and control-polygon excess, so collinear curves
        // with genuine backtracking cannot be mistaken for a direct segment.
        if (b == a && c == d)
            || (control_length - chord_length <= MASK_PATH_STROKE_FLATNESS
                && distance_to_chord(b) <= MASK_PATH_STROKE_FLATNESS
                && distance_to_chord(c) <= MASK_PATH_STROKE_FLATNESS)
        {
            return Ok(self.line_to(d)?);
        }
        check_subdivision(depth)?;
        let ab = (a + b) * 0.5;
        let bc = (b + c) * 0.5;
        let cd = (c + d) * 0.5;
        let abc = (ab + bc) * 0.5;
        let bcd = (bc + cd) * 0.5;
        let middle = (abc + bcd) * 0.5;
        self.cubic_to([a, ab, abc, middle], depth + 1, work)?;
        self.cubic_to([middle, bcd, cd, d], depth + 1, work)
    }

    fn quarter_arc<E>(
        &mut self,
        arc: QuarterArc,
        work: &mut Work<'_, impl FnMut() -> Result<(), E>>,
    ) -> Result<(), ControlledMaskPathStrokeError<E>> {
        let angle = std::f64::consts::FRAC_PI_2;
        // For an analytic ellipse, |p''(angle)| <= max(radius), bounding the
        // linear interpolation deviation by max(radius) * theta^2 / 8.
        // Subdivide in equal powers of two and emit exact cardinal endpoints.
        let mut steps = 1_u32;
        let mut depth = 0;
        while arc.radii.max_element() * (angle / f64::from(steps)).powi(2) * 0.125
            > MASK_PATH_STROKE_FLATNESS
        {
            check_subdivision(depth)?;
            depth += 1;
            steps *= 2;
        }
        for step in 1..steps {
            work.tick()?;
            let (sin, cos) = (angle * f64::from(step) / f64::from(steps)).sin_cos();
            self.line_to(arc.center + arc.radii * (arc.from * cos + arc.to * sin))?;
        }
        Ok(self.line_to(arc.end)?)
    }
}

struct QuarterArc {
    center: DVec2,
    radii: DVec2,
    from: DVec2,
    to: DVec2,
    end: DVec2,
}

fn check_subdivision(depth: u8) -> Result<(), MaskPathStrokeError> {
    if depth >= MASK_PATH_STROKE_MAX_SUBDIVISION_DEPTH {
        Err(MaskPathStrokeError::BudgetExceeded {
            resource: "curve subdivision depth",
            limit: usize::from(MASK_PATH_STROKE_MAX_SUBDIVISION_DEPTH),
        })
    } else {
        Ok(())
    }
}

fn flatten_shape<E>(
    shape: &MaskShape,
    extent: DVec2,
    vertices: &mut Vec<Vertex>,
    work: &mut Work<'_, impl FnMut() -> Result<(), E>>,
) -> Result<FlatPath, ControlledMaskPathStrokeError<E>> {
    let start = vertices.len();
    let mut path = Flattener { vertices, first: start };
    let closed = match shape {
        MaskShape::Rectangle { x, y, width, height, corner_radius } => {
            if ![*x, *y, *width, *height, *corner_radius].into_iter().all(f32::is_finite)
                || *width < 0.0
                || *height < 0.0
                || *corner_radius < 0.0
            {
                return Err(MaskPathStrokeError::InvalidGeometry {
                    reason: "Rectangle values must be finite and sizes non-negative",
                }
                .into());
            }
            let [x0, y0] = (DVec2::new(f64::from(*x), f64::from(*y)) * extent).to_array();
            let [x1, y1] = checked_position(DVec2::new(
                (f64::from(*x) + f64::from(*width)) * extent.x,
                (f64::from(*y) + f64::from(*height)) * extent.y,
            ))?
            .to_array();
            checked_position(DVec2::new(x0, y0))?;
            let radius = f64::from(*corner_radius)
                .min(f64::from(*width) * 0.5)
                .min(f64::from(*height) * 0.5);
            let radii = extent * radius;
            let [rx, ry] = radii.to_array();
            // Clockwise in the image's y-down coordinates, seam at the top-left
            // top tangent (the top-left corner for an unrounded rectangle).
            let seam = DVec2::new(x0 + rx, y0);
            path.line_to(seam)?;
            for (tangent, center, from, to, end) in [
                (
                    [x1 - rx, y0],
                    [x1 - rx, y0 + ry],
                    DVec2::NEG_Y,
                    DVec2::X,
                    [x1, y0 + ry],
                ),
                (
                    [x1, y1 - ry],
                    [x1 - rx, y1 - ry],
                    DVec2::X,
                    DVec2::Y,
                    [x1 - rx, y1],
                ),
                (
                    [x0 + rx, y1],
                    [x0 + rx, y1 - ry],
                    DVec2::Y,
                    DVec2::NEG_X,
                    [x0, y1 - ry],
                ),
                (
                    [x0, y0 + ry],
                    [x0 + rx, y0 + ry],
                    DVec2::NEG_X,
                    DVec2::NEG_Y,
                    seam.to_array(),
                ),
            ] {
                path.line_to(DVec2::from_array(tangent))?;
                if radius > 0.0 {
                    path.quarter_arc(
                        QuarterArc {
                            center: DVec2::from_array(center),
                            radii,
                            from,
                            to,
                            end: DVec2::from_array(end),
                        },
                        work,
                    )?;
                }
            }
            true
        }
        MaskShape::Ellipse { center, radii } => {
            if !center.is_finite() || !radii.is_finite() || radii.x < 0.0 || radii.y < 0.0 {
                return Err(MaskPathStrokeError::InvalidGeometry {
                    reason: "Ellipse values must be finite and radii non-negative",
                }
                .into());
            }
            let center = center.as_dvec2() * extent;
            let radii = radii.as_dvec2() * extent;
            checked_position(center - radii)?;
            checked_position(center + radii)?;
            // Clockwise from the rightmost extremum. A zero minor radius is a
            // retraced line, and two zero radii reduce to a point.
            path.line_to(center + radii * DVec2::X)?;
            for (from, to) in [
                (DVec2::X, DVec2::Y),
                (DVec2::Y, DVec2::NEG_X),
                (DVec2::NEG_X, DVec2::NEG_Y),
                (DVec2::NEG_Y, DVec2::X),
            ] {
                path.quarter_arc(
                    QuarterArc { center, radii, from, to, end: center + radii * to },
                    work,
                )?;
            }
            true
        }
        MaskShape::Path { points, closed } => {
            for point in points {
                work.tick()?;
                if !point.position.is_finite()
                    || !point.control_in.is_finite()
                    || !point.control_out.is_finite()
                {
                    return Err(MaskPathStrokeError::InvalidGeometry {
                        reason: "Path control points must be finite",
                    }
                    .into());
                }
            }
            if let Some(first) = points.first() {
                path.line_to(first.position.as_dvec2() * extent)?;
                let pairs = if *closed && points.len() > 1 {
                    points.len()
                } else {
                    points.len() - 1
                };
                for index in 0..pairs {
                    let first = points[index];
                    let second = points[(index + 1) % points.len()];
                    let a = first.position.as_dvec2();
                    let d = second.position.as_dvec2();
                    // Mask handles are offsets from their anchor, not absolute positions.
                    let b = checked_position((a + first.control_out.as_dvec2()) * extent)?;
                    let c = checked_position((d + second.control_in.as_dvec2()) * extent)?;
                    let d = checked_position(d * extent)?;
                    path.cubic_to([a * extent, b, c, d], 0, work)?;
                }
            }
            *closed
        }
    };
    Ok(FlatPath { start, end: vertices.len(), closed })
}

fn make_primitives<E>(
    shapes: &[MaskShape],
    extent: DVec2,
    grade: StrokeGrade,
    controls: MaskPathStrokeControls,
    work: &mut Work<'_, impl FnMut() -> Result<(), E>>,
) -> Result<Vec<MaskPathStrokePrimitive>, ControlledMaskPathStrokeError<E>> {
    let mut vertices = Vec::new();
    let mut paths = Vec::new();
    paths
        .try_reserve_exact(shapes.len())
        .map_err(|_| MaskPathStrokeError::AllocationFailed { resource: "path descriptors" })?;
    let mut total = 0.0;
    for shape in shapes {
        work.tick()?;
        let path = flatten_shape(shape, extent, &mut vertices, work)?;
        if path.start < path.end {
            total += vertices[path.end - 1].distance;
            paths.push(path);
        }
    }
    let start = f64::from(controls.start_percent);
    let end = f64::from(controls.end_percent);
    // Multiply before division so, for example, 29% of exactly 100 px is
    // exactly 29 px and a dab at that edge is eligible.
    let trim_distance = |length: f64, percent: f64| {
        if percent == 100.0 {
            length
        } else {
            length * percent / 100.0
        }
    };
    let start_total = trim_distance(total, start);
    let end_total = trim_distance(total, end);
    let spacing = f64::from(grade.brush_size()) * f64::from(controls.spacing_percent) / 100.0;
    let mut prefix = 0.0;
    let mut out = Vec::new();
    for path in paths {
        work.tick()?;
        let ordered = &vertices[path.start..path.end];
        let length = ordered[ordered.len() - 1].distance;
        let path_end = prefix + length;
        let (lo, hi) = if controls.sequential {
            if path_end < start_total || prefix > end_total {
                prefix = path_end;
                continue;
            }
            let lo = if start_total <= prefix {
                0.0
            } else {
                start_total - prefix
            };
            let hi = if end_total >= path_end {
                length
            } else {
                end_total - prefix
            };
            (lo.min(length), hi.min(length))
        } else {
            (trim_distance(length, start), trim_distance(length, end))
        };
        prefix = path_end;
        if length == 0.0 {
            // In independent mode each degenerate path owns its entire local trim;
            // in sequential mode it is eligible exactly at its prefix (also for a
            // wholly degenerate collection). Empty shapes were removed above.
            record_primitive(&mut out, ordered[0].point, ordered[0].point)?;
        } else if spacing == 0.0 {
            if hi <= lo {
                continue;
            }
            let index = ordered.partition_point(|vertex| vertex.distance <= lo).saturating_sub(1);
            for pair in ordered[index..].windows(2) {
                work.tick()?;
                let [a, b] = [pair[0], pair[1]];
                if a.distance >= hi {
                    break;
                }
                record_primitive(
                    &mut out,
                    interpolate(a, b, lo.max(a.distance)),
                    interpolate(a, b, hi.min(b.distance)),
                )?;
            }
        } else if hi >= lo && (!path.closed || lo < length) {
            let first = dab_phase_at_or_after(lo, spacing)?;
            if fits_dab(first, spacing, hi, length, path.closed) {
                let remaining = (MASK_PATH_STROKE_MAX_PRIMITIVES - out.len()) as u64;
                let outside_budget = first + remaining;
                if fits_dab(outside_budget, spacing, hi, length, path.closed) {
                    return Err(MaskPathStrokeError::BudgetExceeded {
                        resource: "stroke primitives",
                        limit: MASK_PATH_STROKE_MAX_PRIMITIVES,
                    }
                    .into());
                }
                for index in first..outside_budget {
                    if !fits_dab(index, spacing, hi, length, path.closed) {
                        break;
                    }
                    if index > MASK_PATH_STROKE_MAX_DAB_PHASE_INDEX {
                        return Err(MaskPathStrokeError::DabPhaseLimitExceeded.into());
                    }
                    work.tick()?;
                    let distance = index as f64 * spacing;
                    let right = ordered.partition_point(|vertex| vertex.distance <= distance);
                    let point = if right == ordered.len() {
                        ordered[right - 1].point
                    } else {
                        interpolate(ordered[right - 1], ordered[right], distance)
                    };
                    record_primitive(&mut out, point, point)?;
                }
            }
        }
    }
    Ok(out)
}

fn interpolate(a: Vertex, b: Vertex, distance: f64) -> DVec2 {
    if distance <= a.distance {
        a.point
    } else if distance >= b.distance {
        b.point
    } else {
        a.point.lerp(b.point, (distance - a.distance) / (b.distance - a.distance))
    }
}

fn dab_phase_at_or_after(lo: f64, spacing: f64) -> Result<u64, MaskPathStrokeError> {
    // Floor the quotient, then compare using exactly the multiplication also used
    // to generate dabs. Correct an adjacent rounding at an inclusive trim edge.
    if lo > MASK_PATH_STROKE_MAX_DAB_PHASE_INDEX as f64 * spacing {
        return Err(MaskPathStrokeError::DabPhaseLimitExceeded);
    }
    let mut index = (lo / spacing).floor() as u64;
    if index > 0 && (index - 1) as f64 * spacing >= lo {
        index -= 1;
    } else if index as f64 * spacing < lo {
        index += 1;
    }
    Ok(index)
}

fn fits_dab(index: u64, spacing: f64, hi: f64, length: f64, closed: bool) -> bool {
    let position = index as f64 * spacing;
    position <= hi && (!closed || position < length)
}

fn record_primitive(
    out: &mut Vec<MaskPathStrokePrimitive>,
    a: DVec2,
    b: DVec2,
) -> Result<(), MaskPathStrokeError> {
    push_bounded(
        out,
        MaskPathStrokePrimitive::new(a, b),
        MASK_PATH_STROKE_MAX_PRIMITIVES,
        "stroke primitives",
    )
}

fn build_bvh<E>(
    primitives: &mut [MaskPathStrokePrimitive],
    nodes: &mut Vec<MaskPathStrokeBvhNode>,
    first: usize,
    radius: f32,
    work: &mut Work<'_, impl FnMut() -> Result<(), E>>,
) -> Result<(), ControlledMaskPathStrokeError<E>> {
    work.tick()?;
    let index = nodes.len();
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for primitive in primitives.iter() {
        work.tick()?;
        let [ax, ay, bx, by] = primitive.endpoints;
        bounds = [
            bounds[0].min(ax.min(bx)),
            bounds[1].min(ay.min(by)),
            bounds[2].max(ax.max(bx)),
            bounds[3].max(ay.max(by)),
        ];
    }
    // Outward guard covers Float32 subtraction/sqrt and rounding in radius
    // expansion, even if a CPU/GPU contracts multiply-adds differently.
    let magnitude = bounds.iter().fold(1.0_f32.max(radius), |m, value| m.max(value.abs()));
    let padding = radius + 8.0 * f32::EPSILON * magnitude;
    let [x0, y0, x1, y1] = bounds;
    nodes.push(MaskPathStrokeBvhNode {
        bounds: [x0 - padding, y0 - padding, x1 + padding, y1 + padding],
        metadata: [0; 4],
    });
    if primitives.len() <= LEAF_PRIMITIVES {
        nodes[index].metadata = [nodes.len() as u32, first as u32, primitives.len() as u32, 0];
    } else {
        let axis = usize::from(y1 - y0 > x1 - x0);
        let middle = primitives.len() / 2;
        // Balanced median split; in-place selection needs no index scratch.
        // The linear-time selection is bounded by MAX_PRIMITIVES, with stop
        // checks on both sides; even its largest bounds scan checks in chunks.
        if primitives.len() >= CHECKPOINT_STRIDE {
            work.check()?;
        }
        primitives.select_nth_unstable_by(middle, |a, b| {
            a.center(axis).total_cmp(&b.center(axis)).then_with(|| {
                a.endpoints
                    .iter()
                    .zip(b.endpoints)
                    .map(|(a, b)| a.total_cmp(&b))
                    .find(|order| !order.is_eq())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        if primitives.len() >= CHECKPOINT_STRIDE {
            work.check()?;
        }
        let (left, right) = primitives.split_at_mut(middle);
        build_bvh(left, nodes, first, radius, work)?;
        build_bvh(right, nodes, first + middle, radius, work)?;
        nodes[index].metadata[0] = nodes.len() as u32;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mask::BezierPoint, stroke::StrokePaintStyle};
    use glam::Vec2;
    use mondrian_core::{Rational, SampleAspectRatio};

    const EXTENT: EffectFrameExtent = EffectFrameExtent::new(256, 128);
    const INPUT: [f32; 4] = [2.0, -1.0, 4.0, 0.5];

    fn point(x: f32, y: f32) -> BezierPoint {
        BezierPoint::new(Vec2::new(
            x / EXTENT.width() as f32,
            y / EXTENT.height() as f32,
        ))
    }

    fn path(points: Vec<BezierPoint>, closed: bool) -> MaskShape {
        MaskShape::Path { points, closed }
    }

    fn line(x0: f32, x1: f32, y: f32) -> MaskShape {
        path(vec![point(x0, y), point(x1, y)], false)
    }

    fn controls(sequential: bool, start: f32, end: f32, spacing: f32) -> MaskPathStrokeControls {
        MaskPathStrokeControls::new(sequential, start, end, spacing).unwrap()
    }

    fn grade(style: StrokePaintStyle, size: f32) -> StrokeGrade {
        StrokeGrade::new(style, [0.5, 0.25, 1.0], size, 0.0, 0.5).unwrap()
    }

    fn prepare(
        shapes: &[MaskShape],
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        grade: StrokeGrade,
        controls: MaskPathStrokeControls,
    ) -> Result<MaskPathStrokeLayout, MaskPathStrokeError> {
        MaskPathStrokeLayout::prepare(
            shapes,
            extent,
            sampling,
            grade,
            controls,
            &ExecutionCancellationToken::new(),
        )
    }

    fn layout(
        shapes: &[MaskShape],
        size: f32,
        controls: MaskPathStrokeControls,
    ) -> MaskPathStrokeLayout {
        prepare(
            shapes,
            EXTENT,
            ImageSampling::UNIT_SQUARE,
            grade(StrokePaintStyle::OnOriginal, size),
            controls,
        )
        .unwrap()
    }

    fn endpoints(layout: &MaskPathStrokeLayout) -> Vec<[f32; 4]> {
        layout.primitives().iter().map(|p| p.endpoints).collect()
    }

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() <= 1e-6, "{a} != {b}");
    }

    #[test]
    fn straight_path_is_one_capsule_with_exact_round_cap_profile() {
        let stroke = layout(
            &[line(16.5, 80.5, 32.5)],
            4.0,
            controls(true, 0.0, 100.0, 0.0),
        );
        assert_eq!(endpoints(&stroke), [[16.5, 32.5, 80.5, 32.5]]);
        assert_eq!(stroke.coverage_at_pixel(16, 32), 1.0);
        assert_eq!(stroke.coverage_at_pixel(15, 32), 0.5);
        assert_eq!(stroke.coverage_at_pixel(81, 32), 0.5);
        assert_eq!(stroke.coverage_at_pixel(50, 33), 0.5);
        assert_eq!(stroke.coverage_at_pixel(50, 34), 0.0);
        assert_eq!(stroke.coverage_at_pixel(EXTENT.width(), 32), 0.0);
        // The existing profile has a hard cutoff at radius for subpixel brushes.
        let thin = layout(
            &[line(16.5, 80.5, 32.625)],
            0.25,
            controls(true, 0.0, 100.0, 0.0),
        );
        assert_eq!(thin.coverage_at_pixel(20, 32), 0.75);
    }

    #[test]
    fn sequential_cuts_original_lengths_without_a_join_and_independent_repeats_the_trim() {
        let shapes = [line(16.5, 48.5, 32.5), line(80.5, 144.5, 32.5)];
        let sequential = layout(&shapes, 2.0, controls(true, 25.0, 75.0, 0.0));
        let independent = layout(&shapes, 2.0, controls(false, 25.0, 75.0, 0.0));
        assert_eq!(
            endpoints(&sequential),
            [[40.5, 32.5, 48.5, 32.5], [80.5, 32.5, 120.5, 32.5]]
        );
        assert_eq!(
            endpoints(&independent),
            [[24.5, 32.5, 40.5, 32.5], [96.5, 32.5, 128.5, 32.5]]
        );
        assert_eq!(sequential.coverage_at_pixel(64, 32), 0.0);
        let reordered = layout(
            &[shapes[1].clone(), shapes[0].clone()],
            2.0,
            controls(true, 25.0, 75.0, 0.0),
        );
        assert_eq!(
            endpoints(&reordered),
            [[104.5, 32.5, 144.5, 32.5], [16.5, 32.5, 24.5, 32.5]]
        );
        let boundary = layout(
            &[line(16.5, 32.5, 32.5), line(80.5, 96.5, 32.5)],
            2.0,
            controls(true, 50.0, 100.0, 0.0),
        );
        assert_eq!(endpoints(&boundary), [[80.5, 32.5, 96.5, 32.5]]);
    }

    #[test]
    fn dabs_keep_untrimmed_path_phase_and_include_exact_trim_edges() {
        let shapes = [line(16.5, 80.5, 32.5), line(100.5, 164.5, 32.5)];
        let clipped = layout(&shapes, 8.0, controls(true, 12.5, 62.5, 100.0));
        let mut x: Vec<_> = endpoints(&clipped)
            .into_iter()
            .map(|[a, y, b, z]| {
                assert_eq!([a, y], [b, z]);
                a
            })
            .collect();
        x.sort_by(f32::total_cmp);
        assert_eq!(
            x,
            [32.5, 40.5, 48.5, 56.5, 64.5, 72.5, 80.5, 100.5, 108.5, 116.5]
        );
        let decimal_edge = layout(
            &[line(16.5, 116.5, 32.5)],
            1.0,
            controls(false, 28.0, 29.0, 100.0),
        );
        assert_eq!(
            endpoints(&decimal_edge),
            [[44.5, 32.5, 44.5, 32.5], [45.5, 32.5, 45.5, 32.5]]
        );
        let no_forced_end = layout(
            &[line(16.5, 78.5, 32.5)],
            8.0,
            controls(false, 0.0, 100.0, 100.0),
        );
        assert_eq!(no_forced_end.primitives().len(), 8);
        assert!(endpoints(&no_forced_end).iter().all(|p| p[0] <= 72.5));
    }

    #[test]
    fn clockwise_rectangle_emits_one_closed_seam_and_rounded_corners_use_normalized_radii() {
        let box_shape = |corner_radius| MaskShape::Rectangle {
            x: 0.25,
            y: 0.25,
            width: 0.0625,
            height: 0.125,
            corner_radius,
        };
        let continuous = controls(true, 0.0, 100.0, 0.0);
        let sharp = layout(&[box_shape(0.0)], 2.0, continuous);
        assert_eq!(
            endpoints(&sharp),
            [
                [64.0, 32.0, 80.0, 32.0],
                [80.0, 32.0, 80.0, 48.0],
                [80.0, 48.0, 64.0, 48.0],
                [64.0, 48.0, 64.0, 32.0],
            ]
        );
        let dabs = layout(&[box_shape(0.0)], 8.0, controls(true, 0.0, 100.0, 100.0));
        assert_eq!(dabs.primitives().len(), 8);
        assert_eq!(
            endpoints(&dabs).iter().filter(|p| p[..2] == [64.0, 32.0]).count(),
            1
        );
        let rounded = layout(&[box_shape(0.015625)], 2.0, continuous);
        assert!(sharp.coverage_at_pixel(63, 31) > 0.0);
        assert_eq!(
            rounded.coverage_at_pixel(63, 31),
            0.0,
            "outside the rounded corner"
        );
        assert!(endpoints(&rounded).contains(&[68.0, 32.0, 76.0, 32.0]));
        assert!(endpoints(&rounded).iter().any(|p| p[2..] == [80.0, 34.0]));
        let oversized = layout(&[box_shape(10.0)], 2.0, continuous);
        assert_eq!(
            endpoints(&oversized),
            endpoints(&layout(&[box_shape(0.03125)], 2.0, continuous))
        );
    }

    #[test]
    fn ellipse_seam_and_direction_are_stable() {
        let shape = MaskShape::Ellipse {
            center: Vec2::splat(0.5),
            radii: Vec2::new(0.125, 0.125),
        };
        let mut vertices = Vec::new();
        flatten_shape(
            &shape,
            DVec2::new(256.0, 128.0),
            &mut vertices,
            &mut Work::new(&mut || Ok::<_, ()>(())),
        )
        .unwrap();
        let positions: Vec<_> = vertices.iter().map(|v| v.point.to_array()).collect();
        assert_eq!(positions.first(), Some(&[160.0, 64.0]));
        assert_eq!(positions.first(), positions.last());
        assert!(positions[1][1] > 64.0);
        assert!(positions.contains(&[128.0, 80.0]));
        assert!(positions.contains(&[96.0, 64.0]));
        assert!(positions.contains(&[128.0, 48.0]));
    }

    #[test]
    fn cubic_handles_are_relative_and_only_closed_paths_get_the_closing_edge() {
        let mut a = point(32.5, 32.5);
        let mut b = point(96.5, 32.5);
        a.control_out.y = 0.25;
        b.control_in.y = 0.25;
        let points = vec![a, b];
        let open = layout(
            &[path(points.clone(), false)],
            4.0,
            controls(true, 0.0, 100.0, 0.0),
        );
        let closed = layout(&[path(points, true)], 4.0, controls(true, 0.0, 100.0, 0.0));
        assert!(open.primitives().len() > 1);
        assert_eq!(open.coverage_at_pixel(64, 56), 1.0);
        assert_eq!(open.coverage_at_pixel(64, 32), 0.0);
        assert_eq!(closed.coverage_at_pixel(64, 32), 1.0);
        assert!(endpoints(&closed).contains(&[96.5, 32.5, 32.5, 32.5]));
        let mut a = point(64.5, 32.5);
        let mut b = a;
        a.control_out.x = 0.25;
        b.control_in.x = 0.25;
        let returning = layout(
            &[path(vec![a, b], false)],
            4.0,
            controls(true, 0.0, 100.0, 0.0),
        );
        assert_eq!(returning.coverage_at_pixel(112, 32), 1.0);
    }

    #[test]
    fn empty_and_zero_length_paths_have_defined_trim_behavior() {
        let empty = path(vec![], false);
        let p = point(64.5, 40.5);
        let zero = path(vec![p, p], true);
        let p1 = path(vec![point(64.5, 60.5)], false);
        let shapes = [empty, zero.clone(), p1];
        for spacing in [0.0, 25.0] {
            let strokes = layout(&shapes, 2.0, controls(true, 50.0, 60.0, spacing));
            assert_eq!(
                endpoints(&strokes),
                [[64.5, 40.5, 64.5, 40.5], [64.5, 60.5, 64.5, 60.5]]
            );
            assert!(layout(&shapes, 2.0, controls(true, 60.0, 60.0, spacing)).is_empty());
            assert!(layout(&shapes, 2.0, controls(false, 70.0, 60.0, spacing)).is_empty());
        }
        let mixed = [zero, line(16.5, 80.5, 32.5)];
        assert_eq!(
            layout(&mixed, 2.0, controls(true, 0.0, 10.0, 0.0)).primitives().len(),
            2
        );
        assert_eq!(
            layout(&mixed, 2.0, controls(true, 1.0, 10.0, 0.0)).primitives().len(),
            1
        );
    }

    #[test]
    fn physical_sampling_sets_geometry_spacing_and_pixel_centers() {
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(2, 1).unwrap(),
            [Rational::new(1, 2), Rational::new(1, 4)],
        )
        .unwrap();
        assert_eq!(
            prepare(
                &[],
                EXTENT,
                sampling,
                grade(StrokePaintStyle::OnOriginal, 4.0),
                MaskPathStrokeControls::default()
            )
            .unwrap()
            .pixel_scale(),
            [4.0, 4.0]
        );
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(3, 2).unwrap(),
            [Rational::new(3, 4), Rational::new(1, 4)],
        )
        .unwrap();
        let shape = line(16.5, 24.5, 32.5);
        let stroke = prepare(
            &[shape],
            EXTENT,
            sampling,
            grade(StrokePaintStyle::OnOriginal, 8.0),
            controls(true, 0.0, 100.0, 100.0),
        )
        .unwrap();
        assert_eq!(stroke.pixel_scale(), [2.0, 4.0]);
        assert_eq!(
            endpoints(&stroke),
            [
                [33.0, 130.0, 33.0, 130.0],
                [41.0, 130.0, 41.0, 130.0],
                [49.0, 130.0, 49.0, 130.0]
            ]
        );
        assert_eq!(stroke.coverage_at_pixel(17, 32), 0.5);
        assert_eq!(stroke.coverage_at_pixel(16, 33), 0.0);
    }

    #[test]
    fn overlaps_take_maximum_not_additive_coverage_and_composite_with_straight_alpha() {
        let shape = line(16.5, 80.5, 32.5);
        let trim = controls(true, 0.0, 100.0, 0.0);
        for style in StrokePaintStyle::ALL {
            let grade = grade(style, 4.0);
            let stroke = prepare(
                &[shape.clone(), shape.clone()],
                EXTENT,
                ImageSampling::UNIT_SQUARE,
                grade,
                trim,
            )
            .unwrap();
            assert_eq!(stroke.coverage_at_pixel(16, 33), 0.5);
            let mut pixels = [INPUT, [4.0, -1.0, 2.0, 0.0]];
            stroke
                .apply_region_f32(
                    &mut pixels,
                    EffectPixelRoi::new(16, 32, 2, 1),
                    &ExecutionCancellationToken::new(),
                )
                .unwrap();
            match style {
                StrokePaintStyle::OnOriginal => {
                    for (actual, expected) in
                        pixels[0].into_iter().zip([1.0, -1.0 / 6.0, 2.0, 0.75])
                    {
                        close(actual, expected);
                    }
                    assert_eq!(pixels[1], [0.5, 0.25, 1.0, 0.5]);
                }
                StrokePaintStyle::OnTransparent => assert_eq!(pixels, [[0.5, 0.25, 1.0, 0.5]; 2]),
                StrokePaintStyle::RevealOriginal => {
                    assert_eq!(pixels, [[2.0, -1.0, 4.0, 0.25], [4.0, -1.0, 2.0, 0.0]])
                }
            }
            let original = [-0.0_f32, f32::from_bits(0x7fc0_0011), 3.0, 0.0];
            let mut outside = [original];
            stroke
                .apply_region_f32(
                    &mut outside,
                    EffectPixelRoi::new(2, 2, 1, 1),
                    &ExecutionCancellationToken::new(),
                )
                .unwrap();
            assert_eq!(outside[0].map(f32::to_bits), original.map(f32::to_bits));
        }
    }

    #[test]
    fn non_origin_roi_is_bit_identity_with_the_full_frame_crop() {
        let stroke = layout(
            &[line(12.5, 224.5, 32.5)],
            10.0,
            controls(true, 0.0, 100.0, 15.0),
        );
        let token = ExecutionCancellationToken::new();
        let source = |x, y| [x as f32 * 0.03, y as f32 * -0.04, 2.0, 0.2];
        let roi = EffectPixelRoi::new(37, 29, 43, 10);
        let mut full: Vec<_> = (0..EXTENT.height())
            .flat_map(|y| (0..EXTENT.width()).map(move |x| source(x, y)))
            .collect();
        let mut tile: Vec<_> = (roi.y()..roi.y() + roi.height())
            .flat_map(|y| (roi.x()..roi.x() + roi.width()).map(move |x| source(x, y)))
            .collect();
        stroke.apply_region_f32(&mut full, EXTENT.full_frame_roi(), &token).unwrap();
        stroke.apply_region_f32(&mut tile, roi, &token).unwrap();
        for y in 0..roi.height() as usize {
            let begin = (roi.y() as usize + y) * EXTENT.width() as usize + roi.x() as usize;
            let width = roi.width() as usize;
            assert_eq!(tile[y * width..(y + 1) * width], full[begin..begin + width]);
        }
        assert_eq!(
            stroke.apply_region_f32(&mut [], EffectPixelRoi::new(u32::MAX, 0, 2, 1), &token),
            Err(MaskPathStrokeError::RegionOutsideExtent)
        );
        assert_eq!(
            stroke.apply_region_f32(&mut [], roi, &token),
            Err(MaskPathStrokeError::InvalidPixelBuffer)
        );
    }

    #[test]
    fn gpu_records_are_deterministic_and_spatial_index_matches_brute_force() {
        let mut shapes = vec![MaskShape::Ellipse {
            center: Vec2::splat(0.5),
            radii: Vec2::new(0.45, 0.2),
        }];
        let mut rng = 1_u32;
        let mut random = || {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            (rng >> 24) as f32 + 0.5
        };
        for _ in 0..45 {
            let x = random();
            let y = random() * 0.5;
            shapes.push(path(
                vec![
                    point(x, y),
                    point(x + random() * 0.125, y - random() * 0.125),
                ],
                false,
            ));
        }
        for spacing in [0.0, 41.0] {
            let trim = controls(false, 11.0, 87.0, spacing);
            let stroke = layout(&shapes, 3.0, trim);
            let duplicate = layout(&shapes, 3.0, trim);
            assert_eq!(stroke.primitives(), duplicate.primitives());
            assert_eq!(stroke.nodes(), duplicate.nodes());
            let nodes = stroke.nodes();
            assert_eq!(nodes[0].metadata[0] as usize, nodes.len());
            let mut next_primitive = 0;
            for (i, node) in nodes.iter().enumerate() {
                let [escape, first, count, reserved] = node.metadata;
                assert!(escape as usize > i && escape as usize <= nodes.len());
                assert_eq!(reserved, 0);
                assert_eq!(&node.storage_words()[4..], &node.metadata);
                if count > 0 {
                    assert_eq!(escape as usize, i + 1);
                    assert_eq!(first, next_primitive);
                    assert!(count as usize <= LEAF_PRIMITIVES);
                    next_primitive += count;
                }
            }
            assert_eq!(next_primitive as usize, stroke.primitives().len());
            let [primitives, nodes] = stroke.gpu_storage_bytes();
            assert_eq!(primitives as usize, stroke.primitives().len() * 16);
            assert_eq!(nodes as usize, stroke.nodes().len() * 32);
            assert!(stroke.retained_bytes() <= MASK_PATH_STROKE_MAX_RETAINED_BYTES);
            for y in (0..EXTENT.height()).step_by(2) {
                for x in (0..EXTENT.width()).step_by(2) {
                    let point = stroke.pixel_center(x, y);
                    let expected = stroke
                        .primitives()
                        .iter()
                        .map(|p| stroke.grade().coverage_at_distance(p.distance(point)))
                        .fold(0.0_f32, f32::max);
                    assert_eq!(
                        stroke.coverage_at_pixel(x, y),
                        expected,
                        "{x}, {y}; spacing={spacing}"
                    );
                }
            }
        }
    }

    #[test]
    fn geometry_and_dab_budgets_are_observable_without_degradation() {
        let grade = grade(StrokePaintStyle::OnOriginal, 2.0);
        let run = |shapes: &[MaskShape], extent, trim| {
            prepare(shapes, extent, ImageSampling::UNIT_SQUARE, grade, trim)
        };
        let full = controls(true, 0.0, 100.0, 0.0);
        let p = point(64.5, 32.5);
        for (shapes, resource, limit) in [
            (
                vec![path(vec![], false); MASK_PATH_STROKE_MAX_PATHS + 1],
                "resolved shapes",
                MASK_PATH_STROKE_MAX_PATHS,
            ),
            (
                vec![path(vec![p; MAX_MASK_PATH_POINTS + 1], false)],
                "points per Mask Path",
                MAX_MASK_PATH_POINTS,
            ),
            (
                vec![
                    path(vec![p; MAX_MASK_PATH_POINTS], false);
                    MASK_PATH_STROKE_MAX_AUTHORED_POINTS / MAX_MASK_PATH_POINTS + 1
                ],
                "authored points",
                MASK_PATH_STROKE_MAX_AUTHORED_POINTS,
            ),
        ] {
            assert_eq!(
                run(&shapes, EXTENT, full).unwrap_err(),
                MaskPathStrokeError::BudgetExceeded { resource, limit }
            );
        }
        // One huge ellipse contributes 8193 vertices; 32 exceed the aggregate limit.
        let large_extent = EffectFrameExtent::new(1 << 20, 1 << 20);
        let ellipse = MaskShape::Ellipse { center: Vec2::splat(0.5), radii: Vec2::splat(0.5) };
        assert_eq!(
            run(&vec![ellipse; 32], large_extent, full).unwrap_err(),
            MaskPathStrokeError::BudgetExceeded {
                resource: "flattened vertices",
                limit: MASK_PATH_STROKE_MAX_FLATTENED_VERTICES,
            }
        );
        let shapes = [line(16.5, 216.5, 32.5)];
        assert_eq!(
            run(&shapes, EXTENT, controls(true, 0.0, 100.0, 0.01)).unwrap_err(),
            MaskPathStrokeError::BudgetExceeded {
                resource: "stroke primitives",
                limit: MASK_PATH_STROKE_MAX_PRIMITIVES,
            }
        );
        assert_eq!(
            run(
                &shapes,
                EXTENT,
                controls(true, 50.0, 100.0, f32::MIN_POSITIVE)
            )
            .unwrap_err(),
            MaskPathStrokeError::DabPhaseLimitExceeded
        );
        let exact = [line(
            0.0,
            MASK_PATH_STROKE_MAX_PRIMITIVES as f32 - 1.0,
            32.5,
        )];
        // A complete layout exactly at the GPU binding cap is admitted.
        let exact = run(&exact, EXTENT, controls(true, 0.0, 100.0, 50.0)).unwrap();
        assert_eq!(exact.primitives().len(), MASK_PATH_STROKE_MAX_PRIMITIVES);
        assert_eq!(exact.nodes().len(), MASK_PATH_STROKE_MAX_BVH_NODES);
        assert_eq!(
            exact.gpu_storage_bytes()[0],
            MASK_PATH_STROKE_MAX_BINDING_BYTES
        );
    }

    #[test]
    fn malformed_geometry_and_nonfinite_controls_are_typed_errors() {
        assert_eq!(
            MaskPathStrokeControls::new(false, f32::NAN, 100.0, 0.0),
            Err(MaskPathStrokeError::InvalidControl { name: "start" })
        );
        assert_eq!(
            MaskPathStrokeControls::new(false, 0.0, 100.0, -1.0),
            Err(MaskPathStrokeError::InvalidControl { name: "spacing" })
        );
        let run = |shapes: &[MaskShape], extent| {
            prepare(
                shapes,
                extent,
                ImageSampling::UNIT_SQUARE,
                grade(StrokePaintStyle::OnOriginal, 2.0),
                controls(false, 0.0, 100.0, 0.0),
            )
        };
        assert!(matches!(
            run(&[line(0.0, f32::NAN, 32.0)], EXTENT),
            Err(MaskPathStrokeError::InvalidGeometry { .. })
        ));
        assert!(matches!(
            run(
                &[line(0.0, 16.0, 32.0)],
                EffectFrameExtent::new(u32::MAX, 128)
            ),
            Err(MaskPathStrokeError::InvalidGeometry { .. })
        ));
    }

    #[test]
    fn checkpoints_preserve_stops_before_noops_and_inside_dense_work() {
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();
        let grade = grade(StrokePaintStyle::OnOriginal, 4.0);
        let trim = controls(true, 0.0, 100.0, 0.001);
        assert_eq!(
            MaskPathStrokeLayout::prepare(
                &[],
                EXTENT,
                ImageSampling::UNIT_SQUARE,
                grade,
                trim,
                &cancellation
            )
            .unwrap_err(),
            MaskPathStrokeError::Canceled
        );
        let shapes = [line(16.5, 17.0, 32.5)];
        let mut calls = 0;
        let stop = "deadline";
        let preparation = MaskPathStrokeLayout::prepare_controlled(
            &shapes,
            EXTENT,
            ImageSampling::UNIT_SQUARE,
            grade,
            trim,
            &mut || {
                calls += 1;
                if calls == 2 {
                    Err(stop)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            preparation.unwrap_err(),
            ControlledMaskPathStrokeError::Checkpoint(stop)
        );
        let stroke = layout(&shapes, 4.0, trim);
        assert!(stroke.primitives().len() > CHECKPOINT_STRIDE);
        let mut pixel = [INPUT];
        let mut calls = 0;
        let output = stroke.apply_region_f32_controlled(
            &mut pixel,
            EffectPixelRoi::new(16, 33, 1, 1),
            &mut || {
                calls += 1;
                if calls == 2 {
                    Err(stop)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(output, Err(ControlledMaskPathStrokeError::Checkpoint(stop)));
        assert_eq!(pixel, [INPUT]);
    }
}
