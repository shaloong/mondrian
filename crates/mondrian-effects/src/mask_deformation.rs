//! Bounded inverse sampling for a mask-defined spatial deformation.
//!
//! Author geometry and the forward landmark solve use f64. Raster consumers
//! share a compact fixed-point inverse lattice and integer scanline spans. Spans restrict
//! every sample to the authored boundary, including concave cutouts.

use crate::mask::{MaskShape, MAX_MASK_PATH_POINTS};
use crate::mask_raster::cubic_bezier;
use crate::{EffectFrameExtent, EffectProcessorControl, EffectProcessorStopReason};
use glam::{DMat2, DVec2};
use mondrian_core::ImageSampling;
use std::sync::Arc;

const PATH_STEPS: usize = 8;
const ELLIPSE_STEPS: usize = 64;
const CORNER_STEPS: usize = 8;
const CROSSING_LEAF_EDGES: usize = 8;
const MAX_SPAN_BYTES: usize = 16 * 1024 * 1024;
const INVERSE_ITERATIONS: usize = 16;
const INVERSE_TOLERANCE: f64 = 1e-7;
const MIN_DETERMINANT: f64 = 1e-10;
const LATTICE_WEIGHT_BITS: u32 = 16;
const LATTICE_WEIGHT_ONE: u32 = 1 << LATTICE_WEIGHT_BITS;

/// Refusal while preparing an authored mask deformation for an actual raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MaskDeformationError {
    /// Geometry cannot define a closed finite region.
    #[error("invalid deformation contour: {0}")]
    InvalidContour(&'static str),
    /// The requested transform or lattice density is outside the admitted range.
    #[error("invalid deformation controls: {0}")]
    InvalidControls(&'static str),
    /// The two contours cannot supply a well-conditioned landmark system.
    #[error("deformation landmark system is singular")]
    SingularLandmarks,
    /// Raster lookup tables exceed the bounded staging payload.
    #[error("deformation raster lookup storage exceeds the available budget")]
    SpanBudget,
    /// Source or destination storage does not match the prepared raster.
    #[error("deformation buffer length does not match the actual extent")]
    BufferLength,
    /// Execution was stopped by the frame owner.
    #[error("deformation preparation stopped: {0:?}")]
    Stopped(EffectProcessorStopReason),
}

impl From<EffectProcessorStopReason> for MaskDeformationError {
    fn from(value: EffectProcessorStopReason) -> Self {
        Self::Stopped(value)
    }
}

/// Evaluated source transform and boundary for a Smear-style mask deformation.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskDeformation {
    source: Arc<Contour>,
    boundary: Arc<Contour>,
    target: DVec2,
    angle_radians: f64,
    scale: f64,
    strength: f64,
    elasticity: u32,
    density: u32,
}

/// Author controls for the evaluated source transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskDeformationControls {
    /// Target center in normalized coordinates on the current clip raster.
    pub target: [f64; 2],
    /// Clockwise rotation in image coordinates.
    pub angle_degrees: f64,
    /// Positive uniform scale at full strength.
    pub scale: f64,
    /// Fractional strength in `[0, 1]`.
    pub strength: f64,
    /// Landmark regularization level in `[1, 9]`.
    pub elasticity: u32,
    /// Lattice resolution level in `[1, 3]`.
    pub density: u32,
}

impl MaskDeformation {
    pub(crate) fn hash_state(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;

        for contour in [&self.source, &self.boundary] {
            contour.points.len().hash(state);
            for point in &contour.points {
                point.to_array().map(f64::to_bits).hash(state);
            }
        }
        self.target.to_array().map(f64::to_bits).hash(state);
        self.angle_radians.to_bits().hash(state);
        self.scale.to_bits().hash(state);
        self.strength.to_bits().hash(state);
        self.elasticity.hash(state);
        self.density.hash(state);
    }

    /// Upper bound on the packed GPU lattice and scanline buffer.
    pub fn gpu_payload_byte_limit(&self, extent: EffectFrameExtent) -> u64 {
        let side = 17 + self.density as u64 * 16;
        self.lookup_byte_limit(extent) + side * side * 8 + 32
    }

    fn lookup_byte_limit(&self, extent: EffectFrameExtent) -> u64 {
        let width = u64::from(extent.width());
        let height = u64::from(extent.height());
        let spans_per_row = width.min(self.boundary.points.len() as u64 / 2);
        let axes_and_rows = (width + height) * 8 + (height + 1) * 4;
        (axes_and_rows + height * spans_per_row * 8).min(MAX_SPAN_BYTES as u64)
    }

    pub(crate) fn preparation_scratch_bytes(&self, extent: EffectFrameExtent) -> usize {
        // The scanline Vec can grow geometrically. Leave room for both thin
        // plate solves, flattened-path crossings and axis/lattice metadata.
        self.lookup_byte_limit(extent) as usize * 2 + 1024 * 1024
    }

    pub(crate) fn apply_controlled<E>(
        &self,
        working: &mut Vec<[f32; 4]>,
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut stopped = None;
        let mut poll = || match checkpoint() {
            Ok(()) => None,
            Err(error) => {
                stopped = Some(error);
                Some(EffectProcessorStopReason::Canceled)
            }
        };
        let control = &mut EffectProcessorControl::new(&mut poll);
        let result = self.prepare(extent, sampling, control).and_then(|raster| {
            if raster.lattice_side() != 0 {
                let mut output = vec![[0.0; 4]; working.len()];
                raster.apply_f32(working, &mut output, control)?;
                *working = output;
            }
            Ok(())
        });
        match stopped {
            Some(error) => Err(error),
            None => Ok(result.is_ok()),
        }
    }

    /// Flatten the two author contours once. Open Bézier contours are closed
    /// using their authored endpoint handles.
    pub fn new(
        source: &MaskShape,
        boundary: &MaskShape,
        controls: MaskDeformationControls,
    ) -> Result<Self, MaskDeformationError> {
        let target = DVec2::from_array(controls.target);
        if !target.is_finite()
            || !controls.angle_degrees.is_finite()
            || !(0.1..=2.0).contains(&controls.scale)
            || !(0.0..=1.0).contains(&controls.strength)
            || !(1..=9).contains(&controls.elasticity)
            || !(1..=3).contains(&controls.density)
        {
            return Err(MaskDeformationError::InvalidControls(
                "transform or density",
            ));
        }
        Ok(Self {
            source: Arc::new(Contour::from_shape(source)?),
            boundary: Arc::new(Contour::from_shape(boundary)?),
            target,
            angle_radians: controls.angle_degrees.to_radians(),
            scale: controls.scale,
            strength: controls.strength,
            elasticity: controls.elasticity,
            density: controls.density,
        })
    }

    /// Whether the authored transform can use the source directly.
    pub fn is_identity(&self) -> bool {
        self.strength == 0.0
            || (self.source.center == self.target
                && (self.angle_radians * self.strength).rem_euclid(std::f64::consts::TAU) == 0.0
                && self.scale == 1.0)
    }

    /// Build the fixed-point lattice and exact scanline windows for the current
    /// sampled raster. The solve uses canonical distance, including pixel aspect
    /// and the actual X/Y render scales.
    pub fn prepare(
        &self,
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<MaskDeformationRaster, MaskDeformationError> {
        control.checkpoint()?;
        if extent.width() == 0 || extent.height() == 0 || self.is_identity() {
            return Ok(MaskDeformationRaster::identity(extent));
        }
        let min = self.boundary.min.max(DVec2::ZERO);
        let max = self.boundary.max.min(DVec2::ONE);
        if min.cmpge(max).any() {
            return Ok(MaskDeformationRaster::identity(extent));
        }
        let (row_offsets, spans) = self.boundary.raster_spans(extent, control)?;
        if spans.is_empty() {
            return Ok(MaskDeformationRaster::identity(extent));
        }
        let metric = physical_extent(extent, sampling);
        let source_center = self.source.center * metric;
        let target_center = self.source.center.lerp(self.target, self.strength) * metric;
        let rotation = DMat2::from_angle(self.angle_radians * self.strength);
        let scale = 1.0 + (self.scale - 1.0) * self.strength;
        let transform = Similarity { source_center, target_center, rotation, scale };

        let landmark_scale = (self.boundary.max - self.boundary.min) * metric;
        let normalizer = landmark_scale.max_element();
        let base = self.boundary.min * metric;
        let boundary_landmarks =
            self.boundary.uniform_landmarks(24 + self.density as usize * 8, metric);
        let source_landmarks =
            self.source.uniform_landmarks(12 + self.density as usize * 4, metric);
        let mut landmarks = Vec::with_capacity(boundary_landmarks.len() + source_landmarks.len());
        for p in boundary_landmarks {
            let p = (p * metric - base) / normalizer;
            landmarks.push((p, p));
        }
        for p in source_landmarks {
            let from = p * metric;
            let to = transform.forward(from);
            landmarks.push(((from - base) / normalizer, (to - base) / normalizer));
        }
        let regularization = 1e-6 * f64::from(10 - self.elasticity).powi(2);
        let forward = ThinPlate::fit(&landmarks, regularization, control)?;
        for pair in &mut landmarks {
            std::mem::swap(&mut pair.0, &mut pair.1);
        }
        let inverse = ThinPlate::fit(&landmarks, regularization, control)?;

        let side = 17 + self.density * 16;
        let bounds = (max - min).max(DVec2::new(
            1.0 / f64::from(extent.width()),
            1.0 / f64::from(extent.height()),
        ));
        let origin = min.min(DVec2::ONE - bounds);
        let extent_bits = u32::BITS - (extent.width().max(extent.height()) - 1).leading_zeros();
        // Reserve two signed bits so any difference between two offsets fits i32.
        let offset_bits = 29_u32.saturating_sub(extent_bits).min(16);
        let units_per_uv = DVec2::new(f64::from(extent.width()), f64::from(extent.height()))
            * f64::from(1_u32 << offset_bits);
        let mut coordinates = Vec::with_capacity((side * side) as usize);
        for y in 0..side {
            control.checkpoint()?;
            for x in 0..side {
                let p =
                    origin + bounds * DVec2::new(f64::from(x), f64::from(y)) / f64::from(side - 1);
                let mapped = if !self.boundary.contains(p) {
                    p
                } else {
                    let ideal = transform.inverse(p * metric) / metric;
                    if self.source.contains(ideal) && self.boundary.contains(ideal) {
                        ideal
                    } else {
                        let q = (p * metric - base) / normalizer;
                        let seed = inverse.evaluate(q).0;
                        let solved = forward.invert(q, seed);
                        let result = (solved * normalizer + base) / metric;
                        if self.boundary.contains(result) {
                            result
                        } else {
                            p
                        }
                    }
                };
                let fixed = ((mapped.clamp(DVec2::ZERO, DVec2::ONE) - p) * units_per_uv).round();
                coordinates.push([fixed.x as i32, fixed.y as i32]);
            }
        }
        let axes = [
            build_lattice_axis(extent.width(), origin.x, bounds.x, side, control)?,
            build_lattice_axis(extent.height(), origin.y, bounds.y, side, control)?,
        ];
        Ok(MaskDeformationRaster {
            extent,
            side,
            offset_bits,
            axes,
            coordinates,
            row_offsets,
            spans,
        })
    }
}

/// Shared CPU/GPU deformation payload. Scanline intervals are half-open and
/// reference integer destination pixels. The lattice stores texel displacements
/// with a raster-specific fixed-point scale.
#[derive(Debug)]
pub struct MaskDeformationRaster {
    extent: EffectFrameExtent,
    side: u32,
    offset_bits: u32,
    axes: [Vec<[u32; 2]>; 2],
    coordinates: Vec<[i32; 2]>,
    row_offsets: Vec<u32>,
    spans: Vec<[u32; 2]>,
}

impl MaskDeformationRaster {
    fn identity(extent: EffectFrameExtent) -> Self {
        Self {
            extent,
            side: 0,
            offset_bits: 0,
            axes: [Vec::new(), Vec::new()],
            coordinates: Vec::new(),
            row_offsets: Vec::new(),
            spans: Vec::new(),
        }
    }

    /// Actual raster extent for this payload.
    pub const fn extent(&self) -> EffectFrameExtent {
        self.extent
    }

    /// Lattice side. A zero side means that no raster pixel can move.
    pub const fn lattice_side(&self) -> u32 {
        self.side
    }

    /// Number of fractional bits in each lattice displacement.
    pub const fn offset_bits(&self) -> u32 {
        self.offset_bits
    }

    /// Row-major inverse displacements in units of `1 / 2^offset_bits` texels.
    pub fn offsets(&self) -> &[[i32; 2]] {
        &self.coordinates
    }

    /// X and Y lookup tables of lattice cell indices and 16-bit interpolation
    /// weights. A weight of 65536 selects the next vertex exactly.
    pub fn axes(&self) -> [&[[u32; 2]]; 2] {
        [&self.axes[0], &self.axes[1]]
    }

    /// Offset table for half-open scanline span ranges, with an end sentinel.
    pub fn row_offsets(&self) -> &[u32] {
        &self.row_offsets
    }

    /// Contiguous integer span pairs.
    pub fn spans(&self) -> &[[u32; 2]] {
        &self.spans
    }

    /// Resample a full straight-alpha scene-linear Float32 frame. Exterior
    /// texels retain their original channels, including hidden RGB.
    pub fn apply_f32(
        &self,
        source: &[[f32; 4]],
        output: &mut [[f32; 4]],
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<(), MaskDeformationError> {
        let width = self.extent.width() as usize;
        let height = self.extent.height() as usize;
        if width.checked_mul(height) != Some(source.len()) || output.len() != source.len() {
            return Err(MaskDeformationError::BufferLength);
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        let size = [self.extent.width() as i32, self.extent.height() as i32];
        for (y, (source_row, output_row)) in
            source.chunks_exact(width).zip(output.chunks_exact_mut(width)).enumerate()
        {
            control.checkpoint()?;
            output_row.copy_from_slice(source_row);
            if self.side == 0 {
                continue;
            }
            for &[first, last] in self.scanline(y) {
                for (chunk, pixels) in
                    output_row[first as usize..last as usize].chunks_mut(4096).enumerate()
                {
                    control.checkpoint()?;
                    let x = first + (chunk * 4096) as u32;
                    for (index, pixel) in pixels.iter_mut().enumerate() {
                        let position = [x + index as u32, y as u32];
                        let offset = self.interior_offset(position);
                        if offset != [0; 2] {
                            *pixel = sample_clamped_f32(
                                source,
                                size,
                                position,
                                offset,
                                self.offset_bits,
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Compute a source sample in texel-index coordinates. `None` is a direct
    /// copy of the destination texel.
    pub fn source_texel(&self, x: u32, y: u32) -> Option<[f64; 2]> {
        if self.side == 0 || x >= self.extent.width() || y >= self.extent.height() {
            return None;
        }
        let spans = self.scanline(y as usize);
        let index = spans.partition_point(|span| span[1] <= x);
        if spans.get(index).is_none_or(|span| x < span[0]) {
            return None;
        }
        let offset = self.interior_offset([x, y]);
        let divisor = f64::from(1_u32 << self.offset_bits);
        (offset != [0; 2]).then(|| {
            [
                f64::from(x) + f64::from(offset[0]) / divisor,
                f64::from(y) + f64::from(offset[1]) / divisor,
            ]
        })
    }

    fn scanline(&self, y: usize) -> &[[u32; 2]] {
        &self.spans[self.row_offsets[y] as usize..self.row_offsets[y + 1] as usize]
    }

    #[inline]
    fn interior_offset(&self, [x, y]: [u32; 2]) -> [i32; 2] {
        let [cx, wx] = self.axes[0][x as usize];
        let [cy, wy] = self.axes[1][y as usize];
        let stride = self.side as usize;
        let index = cy as usize * stride + cx as usize;
        let [a, b, c, d] =
            [index, index + 1, index + stride, index + stride + 1].map(|i| self.coordinates[i]);
        std::array::from_fn(|axis| {
            let top = fixed_mix(a[axis], b[axis], wx);
            let bottom = fixed_mix(c[axis], d[axis], wx);
            fixed_mix(top, bottom, wy)
        })
    }
}

#[inline]
fn sample_clamped_f32(
    source: &[[f32; 4]],
    [width, height]: [i32; 2],
    [x, y]: [u32; 2],
    [dx, dy]: [i32; 2],
    offset_bits: u32,
) -> [f32; 4] {
    let unit = 1_i32 << offset_bits;
    let (x0, x1, tx) = clamped_taps(x, dx, width - 1, offset_bits, unit);
    let (y0, y1, ty) = clamped_taps(y, dy, height - 1, offset_bits, unit);
    let width = width as usize;
    let [a, b, c, d] = [
        source[y0 * width + x0],
        source[y0 * width + x1],
        source[y1 * width + x0],
        source[y1 * width + x1],
    ]
    .map(|pixel| {
        let alpha = pixel[3].clamp(0.0, 1.0);
        [pixel[0] * alpha, pixel[1] * alpha, pixel[2] * alpha, alpha]
    });
    // Nonnegative weights retain small positive coverage near integer texels.
    let associated = std::array::from_fn(|channel| {
        let top = a[channel] * (1.0 - tx) + b[channel] * tx;
        let bottom = c[channel] * (1.0 - tx) + d[channel] * tx;
        top * (1.0 - ty) + bottom * ty
    });
    crate::coverage::straight_rgba_from_premultiplied(associated)
}

#[inline]
fn clamped_taps(pixel: u32, offset: i32, end: i32, bits: u32, unit: i32) -> (usize, usize, f32) {
    let base = pixel as i32 + (offset >> bits);
    let low = base.clamp(0, end);
    let high = (low + 1).min(end);
    let fraction = if base < 0 || base >= end {
        0.0
    } else {
        (offset & (unit - 1)) as f32 / unit as f32
    };
    (low as usize, high as usize, fraction)
}

fn build_lattice_axis(
    extent: u32,
    origin: f64,
    size: f64,
    side: u32,
    control: &mut EffectProcessorControl<'_>,
) -> Result<Vec<[u32; 2]>, MaskDeformationError> {
    let mut axis = Vec::with_capacity(extent as usize);
    let max = f64::from(side - 1);
    for texel in 0..extent {
        if texel % 4096 == 0 {
            control.checkpoint()?;
        }
        let uv = (f64::from(texel) + 0.5) / f64::from(extent);
        let lattice = ((uv - origin) * (max / size)).clamp(0.0, max);
        let cell = (lattice as u32).min(side - 2);
        let weight = ((lattice - f64::from(cell)) * f64::from(LATTICE_WEIGHT_ONE)).round() as u32;
        axis.push([cell, weight]);
    }
    Ok(axis)
}

fn fixed_mix(a: i32, b: i32, weight: u32) -> i32 {
    let delta = b - a;
    // Split the signed product to stay within the shader's 32-bit integers.
    let low = (delta as u32 & (LATTICE_WEIGHT_ONE - 1)) * weight + LATTICE_WEIGHT_ONE / 2;
    a + (delta >> LATTICE_WEIGHT_BITS) * weight as i32 + (low >> LATTICE_WEIGHT_BITS) as i32
}

fn physical_extent(extent: EffectFrameExtent, sampling: ImageSampling) -> DVec2 {
    let par = sampling.sample_aspect_ratio();
    let [sx, sy] = sampling.render_scale();
    let width = f64::from(extent.width()) * f64::from(par.numerator())
        / f64::from(par.denominator())
        * sx.den as f64
        / sx.num as f64;
    let height = f64::from(extent.height()) * sy.den as f64 / sy.num as f64;
    DVec2::new(width, height) / width.max(height)
}

#[derive(Debug, PartialEq)]
struct Contour {
    points: Vec<DVec2>,
    crossing_index: Option<ContourCrossingIndex>,
    min: DVec2,
    max: DVec2,
    center: DVec2,
}

impl Contour {
    fn from_shape(shape: &MaskShape) -> Result<Self, MaskDeformationError> {
        let mut points: Vec<DVec2> = match shape {
            MaskShape::Rectangle { x, y, width, height, corner_radius }
                if *width > 0.0 && *height > 0.0 =>
            {
                let min = DVec2::new(f64::from(*x), f64::from(*y));
                let max = min + DVec2::new(f64::from(*width), f64::from(*height));
                let radius =
                    f64::from(*corner_radius).clamp(0.0, 0.5 * f64::from(width.min(*height)));
                if !corner_radius.is_finite() {
                    return Err(MaskDeformationError::InvalidContour(
                        "nonfinite corner radius",
                    ));
                }
                if radius == 0.0 {
                    vec![min, DVec2::new(max.x, min.y), max, DVec2::new(min.x, max.y)]
                } else {
                    let centers = [
                        DVec2::new(max.x - radius, min.y + radius),
                        max - radius,
                        DVec2::new(min.x + radius, max.y - radius),
                        min + radius,
                    ];
                    centers
                        .into_iter()
                        .enumerate()
                        .flat_map(|(corner, center)| {
                            (0..=CORNER_STEPS).map(move |step| {
                                let angle = (corner as f64 - 1.0
                                    + step as f64 / CORNER_STEPS as f64)
                                    * std::f64::consts::FRAC_PI_2;
                                center + DVec2::from_angle(angle) * radius
                            })
                        })
                        .collect()
                }
            }
            MaskShape::Ellipse { center, radii } if radii.min_element() > 0.0 => (0..ELLIPSE_STEPS)
                .map(|step| {
                    center.as_dvec2()
                        + DVec2::from_angle(
                            std::f64::consts::TAU * step as f64 / ELLIPSE_STEPS as f64,
                        ) * radii.as_dvec2()
                })
                .collect(),
            MaskShape::Path { points, .. }
                if (2..=MAX_MASK_PATH_POINTS).contains(&points.len()) =>
            {
                points
                    .iter()
                    .zip(points.iter().cycle().skip(1))
                    .flat_map(|(&a, &b)| {
                        (0..PATH_STEPS).map(move |step| {
                            cubic_bezier(a, b, step as f32 / PATH_STEPS as f32).as_dvec2()
                        })
                    })
                    .collect()
            }
            _ => {
                return Err(MaskDeformationError::InvalidContour(
                    "empty, oversized or degenerate shape",
                ))
            }
        };
        if points.iter().any(|p| !p.is_finite()) {
            return Err(MaskDeformationError::InvalidContour("nonfinite point"));
        }
        points.dedup();
        if points.len() > 1 && points.first() == points.last() {
            points.pop();
        }
        let min = points.iter().copied().reduce(DVec2::min).unwrap_or(DVec2::ZERO);
        let max = points.iter().copied().reduce(DVec2::max).unwrap_or(DVec2::ZERO);
        let mut area = 0.0;
        let mut weighted_center = DVec2::ZERO;
        for (a, b) in edges(&points) {
            let a = a - min;
            let b = b - min;
            let cross = a.perp_dot(b);
            area += cross;
            weighted_center += (a + b) * cross;
        }
        if !area.is_finite() || area.abs() <= 1e-12 {
            return Err(MaskDeformationError::InvalidContour("zero area"));
        }
        let center = min + weighted_center / (3.0 * area);
        let crossing_index =
            (points.len() > ELLIPSE_STEPS).then(|| ContourCrossingIndex::new(&points));
        Ok(Self { points, crossing_index, min, max, center })
    }

    fn contains(&self, p: DVec2) -> bool {
        if !p.is_finite() || p.cmple(self.min).any() || p.cmpge(self.max).any() {
            return false;
        }
        let mut inside = false;
        self.for_each_crossing(p.y, |x| inside ^= p.x < x);
        inside
    }

    fn for_each_crossing(&self, y: f64, mut visit: impl FnMut(f64)) {
        if let Some(index) = &self.crossing_index {
            index.visit(&self.points, y, 1, &mut visit);
        } else {
            edges(&self.points).filter_map(|(a, b)| crossing(a, b, y)).for_each(visit);
        }
    }

    fn uniform_landmarks(&self, count: usize, metric: DVec2) -> Vec<DVec2> {
        let perimeter: f64 = edges(&self.points).map(|(a, b)| ((a - b) * metric).length()).sum();
        let mut segments = edges(&self.points);
        let (mut a, mut b) = segments.next().expect("nonzero contour area");
        let mut base = 0.0;
        let mut length = ((a - b) * metric).length();
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let at = perimeter * i as f64 / count as f64;
            while at >= base + length {
                let Some((next_a, next_b)) = segments.next() else {
                    break;
                };
                base += length;
                a = next_a;
                b = next_b;
                length = ((a - b) * metric).length();
            }
            result.push(a.lerp(b, ((at - base) / length).clamp(0.0, 1.0)));
        }
        result
    }

    fn raster_spans(
        &self,
        extent: EffectFrameExtent,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<(Vec<u32>, Vec<[u32; 2]>), MaskDeformationError> {
        let height = extent.height() as usize;
        let row_bytes = height
            .checked_add(1)
            .and_then(|n| n.checked_mul(4))
            .and_then(|rows| {
                height
                    .checked_add(extent.width() as usize)
                    .and_then(|samples| samples.checked_mul(8))
                    .and_then(|axes| rows.checked_add(axes))
            })
            .filter(|&bytes| bytes < MAX_SPAN_BYTES)
            .ok_or(MaskDeformationError::SpanBudget)?;
        let span_limit = (MAX_SPAN_BYTES - row_bytes) / 8;
        let mut offsets = Vec::with_capacity(height + 1);
        let mut spans = Vec::new();
        let mut crossings = Vec::with_capacity(self.points.len());
        let width = f64::from(extent.width());
        for y in 0..extent.height() {
            control.checkpoint()?;
            offsets.push(spans.len() as u32);
            let py = (f64::from(y) + 0.5) / f64::from(extent.height());
            if py <= self.min.y || py >= self.max.y {
                continue;
            }
            crossings.clear();
            self.for_each_crossing(py, |x| crossings.push(x));
            crossings.sort_unstable_by(f64::total_cmp);
            for pair in crossings.chunks_exact(2) {
                let start = (pair[0] * width - 0.5).ceil().clamp(0.0, width) as u32;
                let end = (pair[1] * width - 0.5).ceil().clamp(0.0, width) as u32;
                if start < end {
                    if spans.len() >= span_limit {
                        return Err(MaskDeformationError::SpanBudget);
                    }
                    spans.push([start, end]);
                }
            }
        }
        offsets.push(spans.len() as u32);
        Ok((offsets, spans))
    }
}

#[derive(Debug, PartialEq)]
struct ContourCrossingIndex {
    edge_indices: Vec<u32>,
    y_bounds: Vec<[f64; 2]>,
    first_leaf: usize,
}

impl ContourCrossingIndex {
    fn new(points: &[DVec2]) -> Self {
        let endpoints = |i: u32| {
            let i = i as usize;
            [points[i].y, points[(i + 1) % points.len()].y]
        };
        let mut edge_indices = (0..points.len() as u32).collect::<Vec<_>>();
        edge_indices.sort_unstable_by(|&a, &b| {
            let [a0, a1] = endpoints(a);
            let [b0, b1] = endpoints(b);
            a0.min(a1).total_cmp(&b0.min(b1))
        });
        let first_leaf = edge_indices.len().div_ceil(CROSSING_LEAF_EDGES).next_power_of_two();
        let empty = [f64::INFINITY, f64::NEG_INFINITY];
        let mut y_bounds = vec![empty; first_leaf * 2];
        for (i, chunk) in edge_indices.chunks(CROSSING_LEAF_EDGES).enumerate() {
            y_bounds[first_leaf + i] = chunk.iter().fold(empty, |[min, max], &edge| {
                let [a, b] = endpoints(edge);
                [min.min(a.min(b)), max.max(a.max(b))]
            });
        }
        for i in (1..first_leaf).rev() {
            let [lo_min, lo_max] = y_bounds[i * 2];
            let [hi_min, hi_max] = y_bounds[i * 2 + 1];
            y_bounds[i] = [lo_min.min(hi_min), lo_max.max(hi_max)];
        }
        Self { edge_indices, y_bounds, first_leaf }
    }

    fn visit(&self, points: &[DVec2], y: f64, node: usize, visit: &mut impl FnMut(f64)) {
        let [min, max] = self.y_bounds[node];
        if y < min || y >= max {
            return;
        }
        if node < self.first_leaf {
            self.visit(points, y, node * 2, visit);
            self.visit(points, y, node * 2 + 1, visit);
        } else {
            let start = (node - self.first_leaf) * CROSSING_LEAF_EDGES;
            let end = (start + CROSSING_LEAF_EDGES).min(self.edge_indices.len());
            for &edge in &self.edge_indices[start..end] {
                let i = edge as usize;
                if let Some(x) = crossing(points[i], points[(i + 1) % points.len()], y) {
                    visit(x);
                }
            }
        }
    }
}

fn edges(points: &[DVec2]) -> impl Iterator<Item = (DVec2, DVec2)> + '_ {
    points.iter().copied().zip(points.iter().copied().cycle().skip(1))
}

fn crossing(a: DVec2, b: DVec2, y: f64) -> Option<f64> {
    ((a.y > y) != (b.y > y)).then(|| a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y))
}

struct Similarity {
    source_center: DVec2,
    target_center: DVec2,
    rotation: DMat2,
    scale: f64,
}

impl Similarity {
    fn forward(&self, p: DVec2) -> DVec2 {
        self.target_center + self.rotation * (p - self.source_center) * self.scale
    }

    fn inverse(&self, p: DVec2) -> DVec2 {
        self.source_center + self.rotation.transpose() * (p - self.target_center) / self.scale
    }
}

struct ThinPlate {
    points: Vec<DVec2>,
    radial: Vec<DVec2>,
    translation: DVec2,
    linear: DMat2,
}

impl ThinPlate {
    fn fit(
        pairs: &[(DVec2, DVec2)],
        regularization: f64,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<Self, MaskDeformationError> {
        let count = pairs.len();
        let rank = count + 3;
        let mut matrix = vec![vec![0.0; rank]; rank];
        let mut rhs = vec![DVec2::ZERO; rank];
        for (i, &(p, q)) in pairs.iter().enumerate() {
            for (j, &(other, _)) in pairs.iter().enumerate() {
                let r2 = p.distance_squared(other);
                matrix[i][j] = if r2 > 0.0 { r2 * r2.ln() } else { 0.0 };
            }
            matrix[i][i] += regularization;
            for (j, value) in [1.0, p.x, p.y].into_iter().enumerate() {
                matrix[i][count + j] = value;
                matrix[count + j][i] = value;
            }
            rhs[i] = q - p;
        }
        for column in 0..rank {
            control.checkpoint()?;
            let pivot = (column..rank)
                .max_by(|&a, &b| matrix[a][column].abs().total_cmp(&matrix[b][column].abs()))
                .expect("nonempty pivot column");
            matrix.swap(column, pivot);
            rhs.swap(column, pivot);
            let lead = matrix[column][column];
            if !lead.is_finite() || lead.abs() < 1e-12 {
                return Err(MaskDeformationError::SingularLandmarks);
            }
            let (processed, remaining) = matrix.split_at_mut(column + 1);
            let pivot_row = &processed[column];
            let pivot_rhs = rhs[column];
            for (row, rhs) in remaining.iter_mut().zip(&mut rhs[column + 1..]) {
                let factor = row[column] / lead;
                for (value, &pivot) in row[column + 1..].iter_mut().zip(&pivot_row[column + 1..]) {
                    *value -= factor * pivot;
                }
                *rhs -= factor * pivot_rhs;
            }
        }
        for row in (0..rank).rev() {
            let tail: DVec2 =
                matrix[row][row + 1..].iter().zip(&rhs[row + 1..]).map(|(&a, &b)| a * b).sum();
            rhs[row] = (rhs[row] - tail) / matrix[row][row];
        }
        if rhs.iter().any(|point| !point.is_finite()) {
            return Err(MaskDeformationError::SingularLandmarks);
        }
        let translation = rhs[count];
        let linear = DMat2::from_cols(rhs[count + 1], rhs[count + 2]);
        rhs.truncate(count);
        Ok(Self {
            points: pairs.iter().map(|&(p, _)| p).collect(),
            radial: rhs,
            translation,
            linear,
        })
    }

    fn evaluate(&self, p: DVec2) -> (DVec2, DMat2) {
        let mut mapped = p + self.translation + self.linear * p;
        let mut jacobian = DMat2::IDENTITY + self.linear;
        for (&position, &coefficient) in self.points.iter().zip(&self.radial) {
            let delta = p - position;
            let r2 = delta.length_squared();
            if r2 > 0.0 {
                let log = r2.ln();
                mapped += coefficient * (r2 * log);
                let gradient = delta * (2.0 * (1.0 + log));
                jacobian.x_axis += coefficient * gradient.x;
                jacobian.y_axis += coefficient * gradient.y;
            }
        }
        (mapped, jacobian)
    }

    fn invert(&self, target: DVec2, seed: DVec2) -> DVec2 {
        let mut current = seed;
        let mut value = self.evaluate(current);
        for _ in 0..INVERSE_ITERATIONS {
            let error = value.0 - target;
            let norm = error.length_squared();
            if !norm.is_finite() || value.1.determinant() <= MIN_DETERMINANT {
                break;
            }
            if norm <= INVERSE_TOLERANCE.powi(2) {
                return current;
            }
            let step = value.1.inverse() * error;
            let improved = [1.0, 0.5, 0.25, 0.125].into_iter().find_map(|gain| {
                let candidate = current - step * gain;
                let value = self.evaluate(candidate);
                (value.0.distance_squared(target) < norm).then_some((candidate, value))
            });
            let Some((candidate, evaluated)) = improved else {
                break;
            };
            current = candidate;
            value = evaluated;
        }
        // A folded forward map may have multiple roots. Use the direct inverse
        // landmark interpolation if the positive-Jacobian solve cannot prove a root.
        if value.1.determinant() > MIN_DETERMINANT
            && value.0.distance_squared(target) <= INVERSE_TOLERANCE.powi(2)
        {
            current
        } else {
            seed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::BezierPoint;
    use glam::Vec2;
    use mondrian_core::{Rational, SampleAspectRatio};

    fn rect(x: f32, y: f32, width: f32, height: f32) -> MaskShape {
        MaskShape::Rectangle { x, y, width, height, corner_radius: 0.0 }
    }

    fn controls(target: [f64; 2]) -> MaskDeformationControls {
        MaskDeformationControls {
            target,
            angle_degrees: 0.0,
            scale: 1.0,
            strength: 1.0,
            elasticity: 1,
            density: 2,
        }
    }

    fn effect_mask(shape: MaskShape) -> crate::MaskComponent {
        let mut mask = crate::MaskComponent::new(
            "Smear fixture".into(),
            crate::MaskEvaluation { shape, ..crate::MaskEvaluation::default() },
        );
        mask.participation = mondrian_core::mask_data::MaskParticipation::EffectOnly;
        mask
    }

    #[test]
    fn smear_author_graph_resolves_mask_ids_and_uses_actual_sampling() {
        use crate::{
            CompiledEffectGraph, EffectColorDomain, EffectFloatFrameView, EffectGpuSpatialPass,
            EffectGpuStage, EffectType, EffectValueFormat, EffectWorkingPrecision,
            PreparedEffectStack,
        };
        use mondrian_core::{
            automation::PropertyValue, ColorFrameAlpha, TimelineTime, WorkingColorSpace,
        };

        let source = effect_mask(rect(0.23, 0.25, 0.43, 0.40));
        let boundary = effect_mask(rect(0.05, 0.08, 0.9, 0.84));
        let kind = EffectType::Smear;
        let mut effect = crate::instantiate_effect_node(kind.clone()).unwrap();
        let compile = |effect: &crate::EffectNode,
                       masks: &[crate::MaskComponent]|
         -> Arc<CompiledEffectGraph> {
            let graph = PreparedEffectStack::prepare_with_masks(
                std::slice::from_ref(effect),
                masks,
                WorkingColorSpace::LinearRec709,
            )
            .unwrap()
            .evaluate_graph(TimelineTime::ZERO)
            .unwrap();
            crate::compile_reference_render_graph(graph).unwrap()
        };
        assert!(compile(&effect, &[]).graph().is_identity());
        for (key, value) in [
            ("source_mask", PropertyValue::MaskReference(Some(source.id))),
            (
                "boundary_mask",
                PropertyValue::MaskReference(Some(boundary.id)),
            ),
            ("mask_rotation", PropertyValue::Float(41.0)),
            ("percent", PropertyValue::Float(100.0)),
        ] {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let graph = compile(&effect, &[source.clone(), boundary.clone()]);
        let mut renamed = source;
        renamed.name = "Renamed source".into();
        let reordered = compile(&effect, &[boundary, renamed]);
        assert_eq!(graph.identity(), reordered.identity());
        let extent = EffectFrameExtent::new(73, 59);
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(2, 1).unwrap(),
            [Rational::new(1, 1); 2],
        )
        .unwrap();
        let pixels = (0..extent.width() * extent.height())
            .map(|i| {
                [
                    (i % 29) as f32 / 7.0,
                    (i % 17) as f32 / -5.0,
                    0.75,
                    (i % 5) as f32 / 4.0,
                ]
            })
            .collect::<Vec<_>>();
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let apply = |sampling| {
            crate::apply_compiled_effect_graph_rgba_f32(
                EffectFloatFrameView::new(extent, format, sampling, &pixels),
                &graph,
                0,
            )
            .unwrap()
        };
        let actual = apply(sampling);
        assert_eq!(actual.sampling(), sampling);
        assert_eq!(actual.pixels()[0], pixels[0]);
        assert_ne!(actual.pixels(), pixels);
        assert_ne!(actual.pixels(), apply(ImageSampling::UNIT_SQUARE).pixels());
        let pipeline = crate::lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        assert!(pipeline.requires_image_sampling());
        let [EffectGpuStage::Spatial {
            pass: EffectGpuSpatialPass::Smear { grade, sampling: None },
            ..
        }] = pipeline.stages()
        else {
            panic!("expected an unbound Smear pass");
        };
        let mut expected = vec![[0.0; 4]; pixels.len()];
        let control = &mut EffectProcessorControl::uncancelled();
        grade
            .prepare(extent, sampling, control)
            .unwrap()
            .apply_f32(&pixels, &mut expected, control)
            .unwrap();
        assert_eq!(actual.pixels(), expected);
        assert!(
            matches!(pipeline.with_image_sampling(sampling).stages(), [EffectGpuStage::Spatial { pass: EffectGpuSpatialPass::Smear { sampling: Some(value), .. }, .. }] if *value == sampling)
        );
    }

    #[test]
    fn smear_rejects_a_stale_bound_mask_during_author_evaluation() {
        use crate::{EffectType, PreparedEffectStack};
        use mondrian_core::{automation::PropertyValue, TimelineTime, WorkingColorSpace};

        let kind = EffectType::Smear;
        let mut effect = crate::instantiate_effect_node(kind.clone()).unwrap();
        let missing = effect_mask(rect(0.3, 0.3, 0.4, 0.4));
        let boundary = effect_mask(rect(0.05, 0.05, 0.9, 0.9));
        for (key, value) in [
            (
                "source_mask",
                PropertyValue::MaskReference(Some(missing.id)),
            ),
            (
                "boundary_mask",
                PropertyValue::MaskReference(Some(boundary.id)),
            ),
            ("percent", PropertyValue::Float(100.0)),
        ] {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let error = PreparedEffectStack::prepare_with_masks(
            &[effect],
            &[boundary],
            WorkingColorSpace::LinearRec709,
        )
        .and_then(|stack| stack.evaluate_graph(TimelineTime::ZERO))
        .unwrap_err();
        assert!(
            error.to_string().contains(&missing.id.to_string()),
            "{error}"
        );
    }

    #[test]
    fn dense_contour_crossing_index_matches_exact_linear_scan() {
        let points = (0..1024)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / 1024.0;
                let radius = 0.36 + 0.1 * (angle * 5.0).cos();
                BezierPoint::new(Vec2::splat(0.5) + Vec2::from_angle(angle) * radius)
            })
            .collect();
        let contour = Contour::from_shape(&MaskShape::Path { points, closed: true }).unwrap();
        assert!(contour.crossing_index.is_some());
        for y in (0..512)
            .map(|i| f64::from(i) / 511.0)
            .chain(contour.points.iter().step_by(73).map(|p| p.y))
        {
            let mut expected = edges(&contour.points)
                .filter_map(|(a, b)| crossing(a, b, y))
                .collect::<Vec<_>>();
            let mut actual = Vec::new();
            contour.for_each_crossing(y, |x| actual.push(x));
            expected.sort_unstable_by(f64::total_cmp);
            actual.sort_unstable_by(f64::total_cmp);
            assert_eq!(actual, expected);
        }
    }

    fn prepare(
        source: &MaskShape,
        boundary: &MaskShape,
        controls: MaskDeformationControls,
        extent: EffectFrameExtent,
    ) -> MaskDeformationRaster {
        MaskDeformation::new(source, boundary, controls)
            .unwrap()
            .prepare(
                extent,
                ImageSampling::UNIT_SQUARE,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap()
    }

    #[test]
    fn concave_open_boundary_closes_and_preserves_exterior_rgb_bit_for_bit() {
        let boundary = MaskShape::Path {
            points: [
                [0.1, 0.1],
                [0.9, 0.1],
                [0.9, 0.4],
                [0.45, 0.4],
                [0.45, 0.9],
                [0.1, 0.9],
            ]
            .into_iter()
            .map(|point| BezierPoint::new(Vec2::from_array(point)))
            .collect(),
            closed: false,
        };
        let extent = EffectFrameExtent::new(101, 73);
        let field = prepare(
            &rect(0.17, 0.2, 0.14, 0.12),
            &boundary,
            controls([0.35, 0.28]),
            extent,
        );
        let source: Vec<_> = (0..extent.height())
            .flat_map(|y| {
                (0..extent.width()).map(move |x| [x as f32 / 8.0, y as f32 / 8.0, 5.0, 1.0])
            })
            .collect();
        let mut output = vec![[0.0; 4]; source.len()];
        field
            .apply_f32(
                &source,
                &mut output,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        let notch = 55 * 101 + 70;
        assert_eq!(output[notch], source[notch]);
        let corner = 12;
        assert_eq!(output[corner], source[corner]);
        assert!(field.source_texel(36, 20).is_some());
        assert_ne!(output[20 * 101 + 36], source[20 * 101 + 36]);
    }

    #[test]
    fn density_changes_the_shared_grid_and_keeps_the_transformed_source_anchors() {
        let mut grade = controls([132.0 / 256.0, 120.0 / 256.0]);
        let source = rect(84.0 / 256.0, 96.0 / 256.0, 48.0 / 256.0, 48.0 / 256.0);
        let boundary = rect(24.0 / 256.0, 24.0 / 256.0, 208.0 / 256.0, 208.0 / 256.0);
        for (density, side) in [(1, 33), (2, 49), (3, 65)] {
            grade.density = density;
            let field = prepare(&source, &boundary, grade, EffectFrameExtent::new(256, 256));
            assert_eq!(field.lattice_side(), side);
            assert_eq!(field.offsets().len(), (side * side) as usize);
            let [sx, sy] = field.source_texel(130, 120).unwrap();
            assert!((sx - 106.0).abs() <= 1e-3 && (sy - 120.0).abs() <= 1e-3);
            assert_eq!(field.source_texel(23, 120), None);
            assert_eq!(field.source_texel(232, 120), None);
        }
    }

    #[test]
    fn rotation_uses_actual_sampling_distance_with_nonsquare_pixels() {
        let source = rect(0.3, 0.2, 0.4, 0.6);
        let boundary = rect(0.05, 0.05, 0.9, 0.9);
        let grade = MaskDeformation::new(
            &source,
            &boundary,
            MaskDeformationControls { angle_degrees: 90.0, ..controls([0.5, 0.5]) },
        )
        .unwrap();
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(2, 1).unwrap(),
            [Rational::new(1, 1); 2],
        )
        .unwrap();
        let field = grade
            .prepare(
                EffectFrameExtent::new(97, 65),
                sampling,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        let [x, y] = field.source_texel(52, 32).unwrap();
        assert!(
            (x - 48.0).abs() <= 1e-3 && (y - 24.0).abs() <= 1e-3,
            "{x}, {y}"
        );
    }

    #[test]
    fn identity_keeps_hidden_color_and_stop_is_authoritative() {
        let source = rect(0.3, 0.2, 0.4, 0.6);
        let boundary = rect(0.05, 0.05, 0.9, 0.9);
        let extent = EffectFrameExtent::new(5, 3);
        let field = prepare(
            &source,
            &boundary,
            MaskDeformationControls { strength: 0.0, ..controls([0.9, 0.9]) },
            extent,
        );
        assert_eq!(field.lattice_side(), 0);
        let pixels = [[5.0, -1.0, 3.0, 0.0]; 15];
        let mut output = [[0.0; 4]; 15];
        field
            .apply_f32(
                &pixels,
                &mut output,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert_eq!(output, pixels);
        let mut stop = || Some(EffectProcessorStopReason::DeadlineExpired);
        let result = MaskDeformation::new(&source, &boundary, controls([0.55, 0.5]))
            .unwrap()
            .prepare(
                extent,
                ImageSampling::UNIT_SQUARE,
                &mut EffectProcessorControl::new(&mut stop),
            );
        assert!(matches!(
            result,
            Err(MaskDeformationError::Stopped(
                EffectProcessorStopReason::DeadlineExpired
            ))
        ));
    }

    #[test]
    fn large_out_of_bounds_transform_keeps_a_finite_bounded_map() {
        let source = rect(0.3, 0.2, 0.4, 0.6);
        let boundary = rect(0.15, 0.15, 0.7, 0.7);
        let extent = EffectFrameExtent::new(81, 43);
        let field = prepare(
            &source,
            &boundary,
            MaskDeformationControls {
                angle_degrees: 155.0,
                scale: 1.9,
                ..controls([1.08, -0.25])
            },
            extent,
        );
        let max_offset = extent.width() as i32 * (1_i32 << field.offset_bits());
        assert!(field.offsets().iter().flatten().all(|v| v.abs() <= max_offset));
        assert_eq!(field.source_texel(0, 0), None);
        assert_eq!(field.source_texel(80, 42), None);
    }

    #[test]
    fn fixed_mix_matches_wide_integer_arithmetic_at_signed_limits() {
        for a in [-536_870_912_i32, -65_537, -1, 0, 65_535, 536_870_912] {
            for b in [-536_870_912_i32, -1, 0, 65_537, 536_870_912] {
                for weight in [0, 1, 32768, 65535, 65536] {
                    let delta = i64::from(b) - i64::from(a);
                    let expected = i64::from(a) + ((delta * i64::from(weight) + 32768) >> 16);
                    assert_eq!(i64::from(fixed_mix(a, b, weight)), expected);
                }
            }
        }
    }

    #[test]
    fn dci_4k_field_keeps_only_lattice_and_scanline_metadata() {
        let begin = std::time::Instant::now();
        let field = prepare(
            &rect(0.2, 0.22, 0.23, 0.25),
            &rect(0.05, 0.05, 0.9, 0.9),
            MaskDeformationControls {
                angle_degrees: 46.0,
                density: 3,
                ..controls([0.47, 0.39])
            },
            EffectFrameExtent::new(4096, 2160),
        );
        let [x, y] = field.axes();
        let bytes = 32
            + (field.offsets().len() + x.len() + y.len() + field.spans().len()) * 8
            + field.row_offsets().len() * 4;
        eprintln!(
            "DCI 4K deformation field: {bytes} bytes, prepared in {:?}",
            begin.elapsed()
        );
        assert!(bytes < 128 * 1024);
        assert_eq!(field.offset_bits(), 16);
    }
}
