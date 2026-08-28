//! Prepared, region-aware Mask rasterization.
//!
//! One immutable preparation validates parameters and flattens Path geometry
//! once. Full-frame and tiled consumers then share the same normalized canvas
//! coordinates, cancellation cadence, numerical rules, and spatial index.

mod path;

use self::path::PreparedPath;
use super::mask::{MaskShape, MAX_MASK_PATH_POINTS};
use crate::{
    EffectFrameExtent, EffectGraphNodeId, EffectGraphNodeKind, EffectPixelRoi, EffectRenderGraph,
};
use glam::Vec2;
use mondrian_core::ExecutionCancellationToken;
use std::{collections::HashMap, sync::Arc};

const MASK_RASTER_CHECKPOINT_PIXELS: usize = 4_096;

/// Typed failure from Mask geometry preparation or raster execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaskRasterError {
    /// The generation or owner canceled preparation/raster work.
    #[error("mask rasterization was canceled")]
    Canceled,
    /// Authored or plugin-emitted geometry is not finite or structurally valid.
    #[error("mask geometry is invalid: {reason}")]
    InvalidGeometry {
        /// Stable failure reason.
        reason: &'static str,
    },
    /// A Path exceeded the deterministic preparation limit.
    #[error("mask Path contains {actual} points, exceeding the {limit}-point limit")]
    PathPointLimitExceeded {
        /// Authored point count.
        actual: usize,
        /// Maximum admitted point count.
        limit: usize,
    },
    /// A checked geometry or allocation-size derivation overflowed.
    #[error("mask geometry size overflowed: {reason}")]
    GeometrySizeOverflow {
        /// Stable overflow context.
        reason: &'static str,
    },
    /// The requested region is outside the immutable full-frame extent.
    #[error("mask raster region is outside its prepared frame extent")]
    RegionOutsideExtent,
}

/// Internal split between Mask geometry/raster failures and the exact stop
/// selected by the execution attempt that owns a cooperative checkpoint.
///
/// Keeping the checkpoint error generic lets temporal token-backed execution
/// and heterogeneous deadline-backed execution share one raster
/// Implementation without flattening scheduler evidence into `Canceled`.
#[derive(Debug)]
pub(crate) enum ControlledMaskRasterError<E> {
    Raster(MaskRasterError),
    Checkpoint(E),
}

impl<E> From<MaskRasterError> for ControlledMaskRasterError<E> {
    fn from(error: MaskRasterError) -> Self {
        Self::Raster(error)
    }
}

fn flatten_token_control_error(
    error: ControlledMaskRasterError<MaskRasterError>,
) -> MaskRasterError {
    match error {
        ControlledMaskRasterError::Raster(error) | ControlledMaskRasterError::Checkpoint(error) => {
            error
        }
    }
}

fn controlled_checkpoint<E>(
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ControlledMaskRasterError<E>> {
    checkpoint().map_err(ControlledMaskRasterError::Checkpoint)
}

/// Immutable Mask geometry and pixel contract prepared for one full-frame
/// extent.
///
/// The prepared value may rasterize any contained region. It owns Path
/// flattening and its nearest-segment acceleration, so tile execution never
/// repeats topology construction or allocates per pixel.
#[derive(Debug)]
pub struct PreparedMaskRaster {
    extent: EffectFrameExtent,
    geometry: PreparedMaskGeometry,
    expansion_scale: f32,
    feather_scale: f32,
    opacity: f32,
    retained_bytes: usize,
}

/// All reachable MaskSource rasters prepared once for one graph evaluation
/// extent and shared by direct or tiled scalar execution.
#[derive(Debug)]
pub(crate) struct PreparedMaskRasterSet {
    extent: EffectFrameExtent,
    rasters: HashMap<EffectGraphNodeId, Arc<PreparedMaskRaster>>,
    retained_bytes: usize,
}

impl PreparedMaskRasterSet {
    pub(crate) fn required_retained_bytes(
        graph: &EffectRenderGraph,
    ) -> Result<usize, MaskRasterError> {
        let mask_count = graph
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, EffectGraphNodeKind::MaskSource { .. }))
            .count();
        if mask_count == 0 {
            return Ok(0);
        }
        let mut retained_bytes = mask_set_base_retained_bytes(mask_count)?;
        for node in &graph.nodes {
            let EffectGraphNodeKind::MaskSource { shape, .. } = &node.kind else {
                continue;
            };
            retained_bytes = retained_bytes
                .checked_add(std::mem::size_of::<PreparedMaskRaster>())
                .ok_or(MaskRasterError::GeometrySizeOverflow {
                    reason: "Mask raster-set retained byte count overflowed",
                })?;
            if let MaskShape::Path { points, closed } = shape {
                if points.len() > MAX_MASK_PATH_POINTS {
                    return Err(MaskRasterError::PathPointLimitExceeded {
                        actual: points.len(),
                        limit: MAX_MASK_PATH_POINTS,
                    });
                }
                retained_bytes = retained_bytes
                    .checked_add(PreparedPath::required_retained_bytes(
                        points.len(),
                        *closed,
                    )?)
                    .ok_or(MaskRasterError::GeometrySizeOverflow {
                        reason: "Mask Path retained byte count overflowed",
                    })?;
            }
        }
        Ok(retained_bytes)
    }

    pub(crate) fn required_max_scratch_bytes(
        graph: &EffectRenderGraph,
    ) -> Result<usize, MaskRasterError> {
        let mut max_scratch_bytes = 0;
        for node in &graph.nodes {
            let EffectGraphNodeKind::MaskSource { shape, .. } = &node.kind else {
                continue;
            };
            if let MaskShape::Path { points, closed } = shape {
                if points.len() > MAX_MASK_PATH_POINTS {
                    return Err(MaskRasterError::PathPointLimitExceeded {
                        actual: points.len(),
                        limit: MAX_MASK_PATH_POINTS,
                    });
                }
                max_scratch_bytes = max_scratch_bytes.max(
                    PreparedPath::required_max_row_scratch_bytes(points.len(), *closed)?,
                );
            }
        }
        Ok(max_scratch_bytes)
    }

    pub(crate) fn prepare(
        graph: &EffectRenderGraph,
        extent: EffectFrameExtent,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Self, MaskRasterError> {
        let mut checkpoint = || mask_raster_checkpoint(cancellation);
        Self::prepare_controlled(graph, extent, &mut checkpoint)
            .map_err(flatten_token_control_error)
    }

    pub(crate) fn prepare_controlled<E>(
        graph: &EffectRenderGraph,
        extent: EffectFrameExtent,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, ControlledMaskRasterError<E>> {
        let mask_count = graph
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, EffectGraphNodeKind::MaskSource { .. }))
            .count();
        let mut rasters = HashMap::with_capacity(mask_count);
        let mut retained_bytes = if mask_count == 0 {
            0
        } else {
            mask_set_base_retained_bytes(mask_count)?
        };
        for node in &graph.nodes {
            controlled_checkpoint(checkpoint)?;
            let EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity, .. } =
                &node.kind
            else {
                continue;
            };
            let raster = Arc::new(PreparedMaskRaster::prepare_controlled(
                shape, extent, *feather, *expansion, *opacity, checkpoint,
            )?);
            retained_bytes = retained_bytes.checked_add(raster.retained_bytes()).ok_or(
                MaskRasterError::GeometrySizeOverflow {
                    reason: "Mask raster-set retained byte count overflowed",
                },
            )?;
            if rasters.insert(node.id, raster).is_some() {
                return Err(MaskRasterError::InvalidGeometry {
                    reason: "Effect graph contains duplicate MaskSource identity",
                }
                .into());
            }
        }
        Ok(Self { extent, rasters, retained_bytes })
    }

    pub(crate) const fn extent(&self) -> EffectFrameExtent {
        self.extent
    }

    pub(crate) fn get(&self, node_id: EffectGraphNodeId) -> Option<&PreparedMaskRaster> {
        self.rasters.get(&node_id).map(Arc::as_ref)
    }

    pub(crate) const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn max_scratch_bytes(&self) -> usize {
        self.rasters
            .values()
            .map(|raster| raster.max_scratch_bytes())
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rasters.is_empty()
    }
}

fn mask_set_base_retained_bytes(mask_count: usize) -> Result<usize, MaskRasterError> {
    let per_entry = std::mem::size_of::<EffectGraphNodeId>()
        .checked_add(std::mem::size_of::<Arc<PreparedMaskRaster>>())
        .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<usize>()))
        .ok_or(MaskRasterError::GeometrySizeOverflow {
            reason: "Mask raster-set entry size overflowed",
        })?;
    mask_count
        .checked_mul(per_entry)
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<PreparedMaskRasterSet>()))
        .ok_or(MaskRasterError::GeometrySizeOverflow {
            reason: "Mask raster-set retained byte count overflowed",
        })
}

impl PreparedMaskRaster {
    /// Validate and prepare one Mask for an immutable output extent.
    pub fn prepare(
        shape: &MaskShape,
        extent: EffectFrameExtent,
        feather: f32,
        expansion: f32,
        opacity: f32,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Self, MaskRasterError> {
        let mut checkpoint = || mask_raster_checkpoint(cancellation);
        Self::prepare_controlled(shape, extent, feather, expansion, opacity, &mut checkpoint)
            .map_err(flatten_token_control_error)
    }

    pub(crate) fn prepare_controlled<E>(
        shape: &MaskShape,
        extent: EffectFrameExtent,
        feather: f32,
        expansion: f32,
        opacity: f32,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, ControlledMaskRasterError<E>> {
        controlled_checkpoint(checkpoint)?;
        if !feather.is_finite() || !expansion.is_finite() || !opacity.is_finite() {
            return Err(MaskRasterError::InvalidGeometry {
                reason: "feather, expansion, and opacity must be finite",
            }
            .into());
        }
        let geometry = PreparedMaskGeometry::prepare_controlled(shape, checkpoint)?;
        let inverse_width = if extent.width() == 0 {
            0.0
        } else {
            1.0 / extent.width() as f32
        };
        let inverse_height = if extent.height() == 0 {
            0.0
        } else {
            1.0 / extent.height() as f32
        };
        let pixel_scale = inverse_width.max(inverse_height).max(f32::MIN_POSITIVE);
        let retained_bytes =
            std::mem::size_of::<Self>().saturating_add(geometry.dynamic_retained_bytes());
        Ok(Self {
            extent,
            geometry,
            expansion_scale: expansion * pixel_scale,
            feather_scale: feather.max(0.0) * 0.5 * pixel_scale,
            opacity: opacity.clamp(0.0, 1.0),
            retained_bytes,
        })
    }

    /// Immutable full-frame coordinate extent.
    pub const fn extent(&self) -> EffectFrameExtent {
        self.extent
    }

    /// Conservative logical bytes retained by prepared geometry and its
    /// spatial index.
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Maximum temporary row-crossing bytes needed by one raster call.
    pub fn max_scratch_bytes(&self) -> usize {
        self.geometry.max_scratch_bytes()
    }

    /// Rasterize normalized Float32 alpha over one exact contained region.
    pub fn rasterize_alpha_f32(
        &self,
        region: EffectPixelRoi,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Vec<f32>, MaskRasterError> {
        let mut checkpoint = || mask_raster_checkpoint(cancellation);
        self.rasterize_region_controlled(region, &mut checkpoint, |alpha| alpha)
            .map_err(flatten_token_control_error)
    }

    /// Rasterize RGBA Float32 mask pixels over one exact contained region.
    pub(crate) fn rasterize_rgba_f32(
        &self,
        region: EffectPixelRoi,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Vec<[f32; 4]>, MaskRasterError> {
        let mut checkpoint = || mask_raster_checkpoint(cancellation);
        self.rasterize_rgba_f32_controlled(region, &mut checkpoint)
            .map_err(flatten_token_control_error)
    }

    pub(crate) fn rasterize_rgba_f32_controlled<E>(
        &self,
        region: EffectPixelRoi,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Vec<[f32; 4]>, ControlledMaskRasterError<E>> {
        self.rasterize_region_controlled(region, checkpoint, |alpha| [1.0, 1.0, 1.0, alpha])
    }

    /// Rasterize RGBA8 Mask pixels for the legacy encoded reference path.
    pub(crate) fn rasterize_rgba_u8(
        &self,
        region: EffectPixelRoi,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Vec<u8>, MaskRasterError> {
        let pixel_count = usize::try_from(region.width())
            .ok()
            .and_then(|width| {
                usize::try_from(region.height())
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(MaskRasterError::GeometrySizeOverflow {
                reason: "encoded Mask raster byte count exceeds addressable memory",
            })?;
        let mut checkpoint = || mask_raster_checkpoint(cancellation);
        let alpha = self
            .rasterize_region_controlled(region, &mut checkpoint, |alpha| {
                (alpha * 255.0).round() as u8
            })
            .map_err(flatten_token_control_error)?;
        let mut rgba = Vec::with_capacity(pixel_count);
        for value in alpha {
            rgba.extend_from_slice(&[255, 255, 255, value]);
        }
        Ok(rgba)
    }

    fn rasterize_region_controlled<T, E>(
        &self,
        region: EffectPixelRoi,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
        mut map: impl FnMut(f32) -> T,
    ) -> Result<Vec<T>, ControlledMaskRasterError<E>> {
        validate_region(self.extent, region)?;
        controlled_checkpoint(checkpoint)?;
        let pixel_count = usize::try_from(region.width())
            .ok()
            .and_then(|width| {
                usize::try_from(region.height())
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(MaskRasterError::GeometrySizeOverflow {
                reason: "Mask raster pixel count exceeds addressable memory",
            })?;
        let mut output = Vec::with_capacity(pixel_count);
        if pixel_count == 0 {
            return Ok(output);
        }

        let inverse_width = 1.0 / self.extent.width() as f32;
        let inverse_height = 1.0 / self.extent.height() as f32;
        let mut row_crossings = Vec::with_capacity(self.geometry.max_row_crossings());
        let mut since_checkpoint = 0usize;
        let x_end = region.x().checked_add(region.width()).ok_or(
            MaskRasterError::GeometrySizeOverflow { reason: "Mask raster x range overflowed" },
        )?;
        let y_end = region.y().checked_add(region.height()).ok_or(
            MaskRasterError::GeometrySizeOverflow { reason: "Mask raster y range overflowed" },
        )?;
        for y in region.y()..y_end {
            controlled_checkpoint(checkpoint)?;
            let py = (y as f32 + 0.5) * inverse_height;
            self.geometry.row_crossings(py, &mut row_crossings);
            for x in region.x()..x_end {
                if since_checkpoint >= MASK_RASTER_CHECKPOINT_PIXELS {
                    controlled_checkpoint(checkpoint)?;
                    since_checkpoint = 0;
                }
                let px = (x as f32 + 0.5) * inverse_width;
                let distance = self.geometry.signed_distance(Vec2::new(px, py), &row_crossings);
                let expanded = distance - self.expansion_scale;
                let coverage = if self.feather_scale <= 1.0e-8 {
                    if expanded <= 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    1.0 - smoothstep(-self.feather_scale, self.feather_scale, expanded)
                };
                output.push(map((coverage * self.opacity).clamp(0.0, 1.0)));
                since_checkpoint += 1;
            }
        }
        controlled_checkpoint(checkpoint)?;
        if output.len() != pixel_count {
            return Err(MaskRasterError::GeometrySizeOverflow {
                reason: "Mask raster output did not match its checked region",
            }
            .into());
        }
        Ok(output)
    }

    #[cfg(test)]
    fn path_segment_count(&self) -> usize {
        self.geometry.path_segment_count()
    }
}

/// Rasterize one complete Mask into encoded alpha bytes.
pub fn rasterize_mask_shape(
    shape: &MaskShape,
    width: u32,
    height: u32,
    feather: f32,
    expansion: f32,
    opacity: f32,
) -> Result<Vec<u8>, MaskRasterError> {
    let cancellation = ExecutionCancellationToken::new();
    let extent = EffectFrameExtent::new(width, height);
    let raster =
        PreparedMaskRaster::prepare(shape, extent, feather, expansion, opacity, &cancellation)?;
    let mut checkpoint = || mask_raster_checkpoint(&cancellation);
    raster
        .rasterize_region_controlled(extent.full_frame_roi(), &mut checkpoint, |alpha| {
            (alpha * 255.0).round() as u8
        })
        .map_err(flatten_token_control_error)
}

/// Rasterize one complete Mask into normalized Float32 alpha.
#[cfg(test)]
pub(crate) fn rasterize_mask_shape_f32(
    shape: &MaskShape,
    width: u32,
    height: u32,
    feather: f32,
    expansion: f32,
    opacity: f32,
) -> Result<Vec<f32>, MaskRasterError> {
    let cancellation = ExecutionCancellationToken::new();
    let extent = EffectFrameExtent::new(width, height);
    PreparedMaskRaster::prepare(shape, extent, feather, expansion, opacity, &cancellation)?
        .rasterize_alpha_f32(extent.full_frame_roi(), &cancellation)
}

#[derive(Debug)]
enum PreparedMaskGeometry {
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        corner_radius: f32,
    },
    Ellipse {
        center: Vec2,
        radii: Vec2,
    },
    Path(PreparedPath),
}

impl PreparedMaskGeometry {
    fn prepare_controlled<E>(
        shape: &MaskShape,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, ControlledMaskRasterError<E>> {
        match shape {
            MaskShape::Rectangle { x, y, width, height, corner_radius } => {
                if ![*x, *y, *width, *height, *corner_radius].into_iter().all(f32::is_finite)
                    || *width < 0.0
                    || *height < 0.0
                    || *corner_radius < 0.0
                {
                    return Err(MaskRasterError::InvalidGeometry {
                        reason: "Rectangle values must be finite and sizes non-negative",
                    }
                    .into());
                }
                Ok(Self::Rectangle {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    corner_radius: *corner_radius,
                })
            }
            MaskShape::Ellipse { center, radii } => {
                if !center.is_finite() || !radii.is_finite() || radii.x < 0.0 || radii.y < 0.0 {
                    return Err(MaskRasterError::InvalidGeometry {
                        reason: "Ellipse values must be finite and radii non-negative",
                    }
                    .into());
                }
                Ok(Self::Ellipse { center: *center, radii: *radii })
            }
            MaskShape::Path { points, closed } => {
                if points.len() > MAX_MASK_PATH_POINTS {
                    return Err(MaskRasterError::PathPointLimitExceeded {
                        actual: points.len(),
                        limit: MAX_MASK_PATH_POINTS,
                    }
                    .into());
                }
                if !points.iter().all(|point| {
                    point.position.is_finite()
                        && point.control_in.is_finite()
                        && point.control_out.is_finite()
                }) {
                    return Err(MaskRasterError::InvalidGeometry {
                        reason: "Path control points must be finite",
                    }
                    .into());
                }
                Ok(Self::Path(PreparedPath::prepare_controlled(
                    points, *closed, checkpoint,
                )?))
            }
        }
    }

    fn signed_distance(&self, point: Vec2, row_crossings: &[f32]) -> f32 {
        match self {
            Self::Rectangle { x, y, width, height, corner_radius } => {
                rect_sdf(point.x, point.y, *x, *y, *width, *height, *corner_radius)
            }
            Self::Ellipse { center, radii } => {
                ellipse_sdf(point.x, point.y, center.x, center.y, radii.x, radii.y)
            }
            Self::Path(path) => path.signed_distance(point, row_crossings),
        }
    }

    fn row_crossings(&self, y: f32, output: &mut Vec<f32>) {
        match self {
            Self::Path(path) => path.row_crossings(y, output),
            Self::Rectangle { .. } | Self::Ellipse { .. } => output.clear(),
        }
    }

    fn max_row_crossings(&self) -> usize {
        match self {
            Self::Path(path) => path.max_row_scratch_bytes() / std::mem::size_of::<f32>(),
            Self::Rectangle { .. } | Self::Ellipse { .. } => 0,
        }
    }

    fn max_scratch_bytes(&self) -> usize {
        match self {
            Self::Path(path) => path.max_row_scratch_bytes(),
            Self::Rectangle { .. } | Self::Ellipse { .. } => 0,
        }
    }

    fn dynamic_retained_bytes(&self) -> usize {
        match self {
            Self::Path(path) => path.retained_bytes(),
            Self::Rectangle { .. } | Self::Ellipse { .. } => 0,
        }
    }

    #[cfg(test)]
    fn path_segment_count(&self) -> usize {
        match self {
            Self::Path(path) => path.segment_count(),
            Self::Rectangle { .. } | Self::Ellipse { .. } => 0,
        }
    }
}

fn validate_region(
    extent: EffectFrameExtent,
    region: EffectPixelRoi,
) -> Result<(), MaskRasterError> {
    let right = u64::from(region.x()) + u64::from(region.width());
    let bottom = u64::from(region.y()) + u64::from(region.height());
    if right > u64::from(extent.width()) || bottom > u64::from(extent.height()) {
        return Err(MaskRasterError::RegionOutsideExtent);
    }
    Ok(())
}

fn rect_sdf(px: f32, py: f32, x: f32, y: f32, width: f32, height: f32, radius: f32) -> f32 {
    let center_x = x + width * 0.5;
    let center_y = y + height * 0.5;
    let half_width = width * 0.5;
    let half_height = height * 0.5;
    let dx = (px - center_x).abs() - half_width + radius;
    let dy = (py - center_y).abs() - half_height + radius;
    Vec2::new(dx.max(0.0), dy.max(0.0)).length() + dx.max(dy).min(0.0) - radius
}

fn ellipse_sdf(px: f32, py: f32, center_x: f32, center_y: f32, rx: f32, ry: f32) -> f32 {
    let rx = rx.max(1.0e-8);
    let ry = ry.max(1.0e-8);
    let dx = (px - center_x) / rx;
    let dy = (py - center_y) / ry;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 1.0e-10 {
        -1.0
    } else {
        (length - 1.0) * rx.min(ry)
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub(crate) fn mask_raster_checkpoint(
    cancellation: &ExecutionCancellationToken,
) -> Result<(), MaskRasterError> {
    if cancellation.is_canceled() {
        Err(MaskRasterError::Canceled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::BezierPoint;

    fn render(
        shape: &MaskShape,
        width: u32,
        height: u32,
        feather: f32,
        expansion: f32,
    ) -> Vec<f32> {
        rasterize_mask_shape_f32(shape, width, height, feather, expansion, 1.0)
            .expect("Mask raster")
    }

    fn crop(full: &[f32], width: usize, roi: EffectPixelRoi) -> Vec<f32> {
        let mut output = Vec::new();
        for y in roi.y() as usize..(roi.y() + roi.height()) as usize {
            let start = y * width + roi.x() as usize;
            output.extend_from_slice(&full[start..start + roi.width() as usize]);
        }
        output
    }

    #[test]
    fn rectangle_feather_expansion_and_opacity_remain_stable() {
        let shape = MaskShape::Rectangle {
            x: 0.25,
            y: 0.25,
            width: 0.5,
            height: 0.5,
            corner_radius: 0.0,
        };
        let hard = render(&shape, 64, 64, 0.0, 0.0);
        let soft = render(&shape, 64, 64, 10.0, 0.0);
        let expanded = render(&shape, 64, 64, 0.0, 10.0);
        assert!(hard[32 * 64 + 32] > 0.99);
        assert!(hard[0] < 0.01);
        assert!(soft.iter().filter(|alpha| **alpha > 0.1 && **alpha < 0.9).count() > 0);
        assert!(
            expanded.iter().filter(|alpha| **alpha > 0.5).count()
                > hard.iter().filter(|alpha| **alpha > 0.5).count()
        );

        let half =
            rasterize_mask_shape_f32(&shape, 64, 64, 0.0, 0.0, 0.5).expect("half-opacity Mask");
        assert!((half[32 * 64 + 32] - 0.5).abs() <= f32::EPSILON);
    }

    #[test]
    fn every_shape_region_is_bit_exact_with_its_full_frame_crop() {
        let shapes = [
            MaskShape::Rectangle {
                x: 0.18,
                y: 0.22,
                width: 0.61,
                height: 0.47,
                corner_radius: 0.07,
            },
            MaskShape::Ellipse {
                center: Vec2::new(0.51, 0.46),
                radii: Vec2::new(0.28, 0.31),
            },
            MaskShape::Path {
                points: vec![
                    BezierPoint::new(Vec2::new(0.12, 0.18)),
                    BezierPoint::new(Vec2::new(0.82, 0.25)),
                    BezierPoint::new(Vec2::new(0.68, 0.84)),
                    BezierPoint::new(Vec2::new(0.2, 0.72)),
                ],
                closed: true,
            },
        ];
        let extent = EffectFrameExtent::new(97, 61);
        let roi = EffectPixelRoi::new(37, 19, 23, 17);
        for shape in shapes {
            let cancellation = ExecutionCancellationToken::new();
            let prepared =
                PreparedMaskRaster::prepare(&shape, extent, 7.5, -1.25, 0.73, &cancellation)
                    .expect("prepared Mask");
            let full = prepared
                .rasterize_alpha_f32(extent.full_frame_roi(), &cancellation)
                .expect("full Mask");
            let tile = prepared.rasterize_alpha_f32(roi, &cancellation).expect("tile Mask");
            assert_eq!(tile, crop(&full, extent.width() as usize, roi));
        }
    }

    #[test]
    fn path_is_flattened_once_and_reused_across_regions() {
        let shape = MaskShape::Path {
            points: vec![
                BezierPoint::new(Vec2::new(0.1, 0.1)),
                BezierPoint::new(Vec2::new(0.9, 0.1)),
                BezierPoint::new(Vec2::new(0.9, 0.9)),
                BezierPoint::new(Vec2::new(0.1, 0.9)),
            ],
            closed: true,
        };
        let cancellation = ExecutionCancellationToken::new();
        let prepared = PreparedMaskRaster::prepare(
            &shape,
            EffectFrameExtent::new(64, 64),
            0.0,
            0.0,
            1.0,
            &cancellation,
        )
        .expect("prepared Path");
        assert_eq!(prepared.path_segment_count(), 32);
        let retained = prepared.retained_bytes();
        prepared
            .rasterize_alpha_f32(EffectPixelRoi::new(0, 0, 32, 64), &cancellation)
            .expect("left tile");
        prepared
            .rasterize_alpha_f32(EffectPixelRoi::new(32, 0, 32, 64), &cancellation)
            .expect("right tile");
        assert_eq!(prepared.path_segment_count(), 32);
        assert_eq!(prepared.retained_bytes(), retained);
    }

    #[test]
    fn raster_set_preflight_matches_prepared_logical_bytes() {
        let graph = EffectRenderGraph {
            nodes: vec![crate::EffectGraphNode {
                id: EffectGraphNodeId(11),
                kind: EffectGraphNodeKind::MaskSource {
                    shape: MaskShape::Path {
                        points: vec![
                            BezierPoint::new(Vec2::new(0.1, 0.1)),
                            BezierPoint::new(Vec2::new(0.9, 0.1)),
                            BezierPoint::new(Vec2::new(0.9, 0.9)),
                            BezierPoint::new(Vec2::new(0.1, 0.9)),
                        ],
                        closed: true,
                    },
                    feather: 4.0,
                    expansion: 2.0,
                    opacity: 0.8,
                    invert: false,
                },
            }],
            output: Some(EffectGraphNodeId(11)),
        };
        let required =
            PreparedMaskRasterSet::required_retained_bytes(&graph).expect("preflight bytes");
        let prepared = PreparedMaskRasterSet::prepare(
            &graph,
            EffectFrameExtent::new(1920, 1080),
            &ExecutionCancellationToken::new(),
        )
        .expect("prepared Mask set");

        assert_eq!(prepared.retained_bytes(), required);
    }

    #[test]
    fn cancellation_and_invalid_regions_fail_without_partial_success() {
        let shape = MaskShape::default();
        let cancellation = ExecutionCancellationToken::new();
        let prepared = PreparedMaskRaster::prepare(
            &shape,
            EffectFrameExtent::new(64, 64),
            0.0,
            0.0,
            1.0,
            &cancellation,
        )
        .expect("prepared Mask");
        cancellation.cancel();
        assert_eq!(
            prepared
                .rasterize_alpha_f32(EffectPixelRoi::new(0, 0, 64, 64), &cancellation)
                .expect_err("canceled raster"),
            MaskRasterError::Canceled
        );
        let active = ExecutionCancellationToken::new();
        assert_eq!(
            prepared
                .rasterize_alpha_f32(EffectPixelRoi::new(63, 63, 2, 2), &active)
                .expect_err("out-of-bounds raster"),
            MaskRasterError::RegionOutsideExtent
        );
    }

    #[test]
    fn empty_extent_is_empty_and_path_limit_fails_closed() {
        let cancellation = ExecutionCancellationToken::new();
        let prepared = PreparedMaskRaster::prepare(
            &MaskShape::default(),
            EffectFrameExtent::new(0, 0),
            0.0,
            0.0,
            1.0,
            &cancellation,
        )
        .expect("empty Mask");
        assert!(prepared
            .rasterize_alpha_f32(EffectPixelRoi::new(0, 0, 0, 0), &cancellation)
            .expect("empty raster")
            .is_empty());

        let points = vec![BezierPoint::new(Vec2::ZERO); MAX_MASK_PATH_POINTS + 1];
        assert_eq!(
            PreparedMaskRaster::prepare(
                &MaskShape::Path { points, closed: true },
                EffectFrameExtent::new(16, 16),
                0.0,
                0.0,
                1.0,
                &cancellation,
            )
            .expect_err("oversized Path"),
            MaskRasterError::PathPointLimitExceeded {
                actual: MAX_MASK_PATH_POINTS + 1,
                limit: MAX_MASK_PATH_POINTS,
            }
        );
    }
}
