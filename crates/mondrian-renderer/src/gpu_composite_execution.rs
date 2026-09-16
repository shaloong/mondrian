//! Checked spatial execution planning for GPU working-space compositing.
//!
//! The planner is deliberately backend-object free. It turns ordered layer
//! footprints into conservative damage rectangles, preserved-copy regions,
//! and bounded tile draws. The wgpu recorder consumes this plan without
//! reinterpreting spatial semantics.

use serde::{Deserialize, Serialize};

/// Default maximum width or height of one compositor tile draw.
///
/// 4K work remains a single draw while 8K and larger work is bounded without
/// introducing another render pass.
pub const DEFAULT_GPU_COMPOSITE_TILE_DIMENSION: u32 = 4096;
/// Maximum tile draws admitted for one compositor Layer pass.
pub const MAX_GPU_COMPOSITE_TILES: u64 = 4096;

/// One integer pixel rectangle in output-texture coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositeRect {
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl GpuCompositeRect {
    /// Return the number of pixels in the rectangle.
    pub fn pixel_count(self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }

    fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }
}

/// Normalized source crop used to tighten a transformed layer footprint.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuCompositeSourceCrop {
    /// Fraction removed from the left edge.
    pub left: f32,
    /// Fraction removed from the top edge.
    pub top: f32,
    /// Fraction removed from the right edge.
    pub right: f32,
    /// Fraction removed from the bottom edge.
    pub bottom: f32,
}

impl GpuCompositeSourceCrop {
    /// Intersect this crop with another source-relative crop.
    pub fn intersect(self, other: Self) -> Self {
        Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    fn clamped(self) -> Self {
        Self {
            left: finite_clamp(self.left),
            top: finite_clamp(self.top),
            right: finite_clamp(self.right),
            bottom: finite_clamp(self.bottom),
        }
    }
}

/// Spatial and fusion evidence supplied for one ordered composite layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GpuCompositeLayerFootprint {
    /// The layer cannot change any output pixel.
    NoContribution,
    /// The layer can change the complete output canvas.
    FullCanvas {
        /// Point-effect operations fused with transform and blend.
        fused_point_operations: u32,
    },
    /// A source-domain rectangle transformed into the output canvas.
    TransformedSource {
        /// Source width in pixels.
        source_width: u32,
        /// Source height in pixels.
        source_height: u32,
        /// Source-to-output affine transform `[a, c, tx, b, d, ty]`.
        transform: [f32; 6],
        /// Conservative source-relative crop intersection.
        crop: GpuCompositeSourceCrop,
        /// Point-effect operations fused with transform and blend.
        fused_point_operations: u32,
    },
}

/// Policy controlling bounded compositor tile draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuCompositeExecutionPolicy {
    /// Maximum width or height of one tile draw.
    pub max_tile_dimension: u32,
}

impl Default for GpuCompositeExecutionPolicy {
    fn default() -> Self {
        Self {
            max_tile_dimension: DEFAULT_GPU_COMPOSITE_TILE_DIMENSION,
        }
    }
}

/// One contributing layer's checked recording schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuCompositeLayerExecution {
    /// Index into the original ordered layer request.
    pub input_index: usize,
    /// Conservative output region that the layer may change.
    pub damage: GpuCompositeRect,
    /// Non-overlapping regions copied from the previous accumulator.
    pub preserved_regions: Vec<GpuCompositeRect>,
    /// Non-overlapping scissor rectangles drawn in one render pass.
    pub tiles: Vec<GpuCompositeRect>,
    /// Whether the destination begins from transparent instead of prior pixels.
    pub initializes_accumulator: bool,
    /// Number of point operations fused into this layer pass.
    pub fused_point_operations: u32,
}

/// Measured work derived from a complete composite execution plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuCompositeExecutionDiagnostics {
    /// Layers rejected before recording because they cannot contribute.
    pub eliminated_layers: u64,
    /// Render passes required by contributing layers.
    pub render_passes: u64,
    /// Tile draws recorded inside those render passes.
    pub tile_draws: u64,
    /// Point operations fused with transform and blend.
    pub fused_point_operations: u64,
    /// Layer passes that fused one or more point operations with transform and blend.
    pub fused_layer_passes: u64,
    /// Pixels that would have been shaded by full-frame layer passes.
    pub logical_full_frame_shader_pixels: u64,
    /// Pixels actually shaded by planned tile draws.
    pub shaded_pixels: u64,
    /// Full-frame shader pixels eliminated by spatial planning.
    pub avoided_shader_pixels: u64,
    /// Pixels preserved with texture copies instead of layer shader execution.
    pub preserved_copy_pixels: u64,
    /// Non-overlapping texture-copy commands used to preserve prior pixels.
    pub preserved_copy_regions: u64,
}

impl GpuCompositeExecutionDiagnostics {
    /// Accumulate another execution snapshot using saturating counters.
    pub fn accumulate(&mut self, other: Self) {
        self.eliminated_layers = self.eliminated_layers.saturating_add(other.eliminated_layers);
        self.render_passes = self.render_passes.saturating_add(other.render_passes);
        self.tile_draws = self.tile_draws.saturating_add(other.tile_draws);
        self.fused_point_operations =
            self.fused_point_operations.saturating_add(other.fused_point_operations);
        self.fused_layer_passes = self.fused_layer_passes.saturating_add(other.fused_layer_passes);
        self.logical_full_frame_shader_pixels = self
            .logical_full_frame_shader_pixels
            .saturating_add(other.logical_full_frame_shader_pixels);
        self.shaded_pixels = self.shaded_pixels.saturating_add(other.shaded_pixels);
        self.avoided_shader_pixels =
            self.avoided_shader_pixels.saturating_add(other.avoided_shader_pixels);
        self.preserved_copy_pixels =
            self.preserved_copy_pixels.saturating_add(other.preserved_copy_pixels);
        self.preserved_copy_regions =
            self.preserved_copy_regions.saturating_add(other.preserved_copy_regions);
    }
}

/// Complete checked spatial execution plan for one ordered composite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuCompositeExecutionPlan {
    /// Per-contributing-layer schedules in input order.
    pub layers: Vec<GpuCompositeLayerExecution>,
    /// Aggregate work evidence.
    pub diagnostics: GpuCompositeExecutionDiagnostics,
}

/// Invalid input rejected by the spatial execution planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GpuCompositeExecutionPlanError {
    /// Output dimensions must be non-zero.
    #[error("GPU composite execution output dimensions must be non-zero")]
    EmptyCanvas,
    /// Tile dimensions must be non-zero.
    #[error("GPU composite execution tile dimension must be non-zero")]
    EmptyTile,
    /// A Layer damage rectangle would require an unbounded draw schedule.
    #[error(
        "GPU composite execution requires {required_tiles} tiles, exceeding the {maximum_tiles} tile limit"
    )]
    TileLimitExceeded {
        /// Tiles required by the requested extent and policy.
        required_tiles: u64,
        /// Hard planner limit.
        maximum_tiles: u64,
    },
}

/// Backend-neutral planner for composite ROI, damage, preservation, and tiles.
pub struct GpuCompositeExecutionPlanner;

impl GpuCompositeExecutionPlanner {
    /// Build a checked plan for ordered layer footprints.
    pub fn plan(
        width: u32,
        height: u32,
        footprints: &[GpuCompositeLayerFootprint],
        policy: GpuCompositeExecutionPolicy,
    ) -> Result<GpuCompositeExecutionPlan, GpuCompositeExecutionPlanError> {
        if width == 0 || height == 0 {
            return Err(GpuCompositeExecutionPlanError::EmptyCanvas);
        }
        if policy.max_tile_dimension == 0 {
            return Err(GpuCompositeExecutionPlanError::EmptyTile);
        }

        let canvas = GpuCompositeRect { x: 0, y: 0, width, height };
        let canvas_pixels = canvas.pixel_count();
        let mut layers = Vec::new();
        let mut diagnostics = GpuCompositeExecutionDiagnostics::default();

        for (input_index, footprint) in footprints.iter().copied().enumerate() {
            let (damage, fused_point_operations) = match footprint {
                GpuCompositeLayerFootprint::NoContribution => {
                    diagnostics.eliminated_layers = diagnostics.eliminated_layers.saturating_add(1);
                    continue;
                }
                GpuCompositeLayerFootprint::FullCanvas { fused_point_operations } => {
                    (canvas, fused_point_operations)
                }
                GpuCompositeLayerFootprint::TransformedSource {
                    source_width,
                    source_height,
                    transform,
                    crop,
                    fused_point_operations,
                } => {
                    let Some(damage) = transformed_source_damage(
                        canvas,
                        source_width,
                        source_height,
                        transform,
                        crop,
                    ) else {
                        diagnostics.eliminated_layers =
                            diagnostics.eliminated_layers.saturating_add(1);
                        continue;
                    };
                    (damage, fused_point_operations)
                }
            };

            let initializes_accumulator = layers.is_empty();
            let preserved_regions = if initializes_accumulator || damage == canvas {
                Vec::new()
            } else {
                complement(canvas, damage)
            };
            let tiles = split_tiles(damage, policy.max_tile_dimension)?;
            let shaded_pixels = tiles.iter().fold(0_u64, |total, tile| {
                total.saturating_add(tile.pixel_count())
            });
            let preserved_pixels = preserved_regions.iter().fold(0_u64, |total, region| {
                total.saturating_add(region.pixel_count())
            });

            diagnostics.render_passes = diagnostics.render_passes.saturating_add(1);
            diagnostics.tile_draws = diagnostics
                .tile_draws
                .saturating_add(u64::try_from(tiles.len()).unwrap_or(u64::MAX));
            diagnostics.fused_point_operations = diagnostics
                .fused_point_operations
                .saturating_add(u64::from(fused_point_operations));
            if fused_point_operations > 0 {
                diagnostics.fused_layer_passes = diagnostics.fused_layer_passes.saturating_add(1);
            }
            diagnostics.logical_full_frame_shader_pixels =
                diagnostics.logical_full_frame_shader_pixels.saturating_add(canvas_pixels);
            diagnostics.shaded_pixels = diagnostics.shaded_pixels.saturating_add(shaded_pixels);
            diagnostics.preserved_copy_pixels =
                diagnostics.preserved_copy_pixels.saturating_add(preserved_pixels);
            diagnostics.preserved_copy_regions = diagnostics
                .preserved_copy_regions
                .saturating_add(u64::try_from(preserved_regions.len()).unwrap_or(u64::MAX));
            layers.push(GpuCompositeLayerExecution {
                input_index,
                damage,
                preserved_regions,
                tiles,
                initializes_accumulator,
                fused_point_operations,
            });
        }

        diagnostics.avoided_shader_pixels = diagnostics
            .logical_full_frame_shader_pixels
            .saturating_sub(diagnostics.shaded_pixels);
        Ok(GpuCompositeExecutionPlan { layers, diagnostics })
    }
}

fn transformed_source_damage(
    canvas: GpuCompositeRect,
    source_width: u32,
    source_height: u32,
    transform: [f32; 6],
    crop: GpuCompositeSourceCrop,
) -> Option<GpuCompositeRect> {
    if source_width == 0 || source_height == 0 || transform.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let crop = crop.clamped();
    let source_width = f64::from(source_width);
    let source_height = f64::from(source_height);
    let left = 0.5_f64.max(f64::from(crop.left) * source_width);
    let top = 0.5_f64.max(f64::from(crop.top) * source_height);
    let right = (source_width + 0.5).min((1.0 - f64::from(crop.right)) * source_width);
    let bottom = (source_height + 0.5).min((1.0 - f64::from(crop.bottom)) * source_height);
    if right <= left || bottom <= top {
        return None;
    }

    let points = [(left, top), (right, top), (left, bottom), (right, bottom)];
    let mut minimum_x = f64::INFINITY;
    let mut minimum_y = f64::INFINITY;
    let mut maximum_x = f64::NEG_INFINITY;
    let mut maximum_y = f64::NEG_INFINITY;
    for (x, y) in points {
        let output_x =
            f64::from(transform[0]) * x + f64::from(transform[1]) * y + f64::from(transform[2]);
        let output_y =
            f64::from(transform[3]) * x + f64::from(transform[4]) * y + f64::from(transform[5]);
        minimum_x = minimum_x.min(output_x);
        minimum_y = minimum_y.min(output_y);
        maximum_x = maximum_x.max(output_x);
        maximum_y = maximum_y.max(output_y);
    }

    // Pixel centers are x + 0.5/y + 0.5. Floor the lower edge to remain
    // conservative under affine floating-point roundoff; the half-open upper
    // edge can use ceil directly.
    let start_x = clamp_floor(minimum_x - 0.5, canvas.width);
    let start_y = clamp_floor(minimum_y - 0.5, canvas.height);
    let end_x = clamp_ceil(maximum_x - 0.5, canvas.width);
    let end_y = clamp_ceil(maximum_y - 0.5, canvas.height);
    (end_x > start_x && end_y > start_y).then_some(GpuCompositeRect {
        x: start_x,
        y: start_y,
        width: end_x - start_x,
        height: end_y - start_y,
    })
}

fn clamp_floor(value: f64, maximum: u32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value >= f64::from(maximum) {
        return maximum;
    }
    value.floor() as u32
}

fn clamp_ceil(value: f64, maximum: u32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value >= f64::from(maximum) {
        return maximum;
    }
    value.ceil() as u32
}

fn finite_clamp(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn complement(canvas: GpuCompositeRect, damage: GpuCompositeRect) -> Vec<GpuCompositeRect> {
    let mut regions = Vec::with_capacity(4);
    push_non_empty(&mut regions, 0, 0, canvas.width, damage.y);
    push_non_empty(
        &mut regions,
        0,
        damage.bottom(),
        canvas.width,
        canvas.height.saturating_sub(damage.bottom()),
    );
    push_non_empty(&mut regions, 0, damage.y, damage.x, damage.height);
    push_non_empty(
        &mut regions,
        damage.right(),
        damage.y,
        canvas.width.saturating_sub(damage.right()),
        damage.height,
    );
    regions
}

fn push_non_empty(regions: &mut Vec<GpuCompositeRect>, x: u32, y: u32, width: u32, height: u32) {
    if width > 0 && height > 0 {
        regions.push(GpuCompositeRect { x, y, width, height });
    }
}

fn split_tiles(
    rectangle: GpuCompositeRect,
    maximum: u32,
) -> Result<Vec<GpuCompositeRect>, GpuCompositeExecutionPlanError> {
    let columns = rectangle.width.div_ceil(maximum);
    let rows = rectangle.height.div_ceil(maximum);
    let required_tiles = u64::from(columns).saturating_mul(u64::from(rows));
    if required_tiles > MAX_GPU_COMPOSITE_TILES {
        return Err(GpuCompositeExecutionPlanError::TileLimitExceeded {
            required_tiles,
            maximum_tiles: MAX_GPU_COMPOSITE_TILES,
        });
    }
    let capacity = usize::try_from(required_tiles).unwrap_or(usize::MAX);
    let mut tiles = Vec::with_capacity(capacity);
    let mut y = rectangle.y;
    while y < rectangle.bottom() {
        let height = maximum.min(rectangle.bottom() - y);
        let mut x = rectangle.x;
        while x < rectangle.right() {
            let width = maximum.min(rectangle.right() - x);
            tiles.push(GpuCompositeRect { x, y, width, height });
            x = x.saturating_add(width);
        }
        y = y.saturating_add(height);
    }
    Ok(tiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

    #[test]
    fn transformed_source_tightens_shader_work_and_preserves_prior_pixels() {
        let plan = GpuCompositeExecutionPlanner::plan(
            1920,
            1080,
            &[
                GpuCompositeLayerFootprint::FullCanvas { fused_point_operations: 0 },
                GpuCompositeLayerFootprint::TransformedSource {
                    source_width: 320,
                    source_height: 180,
                    transform: [1.0, 0.0, 100.0, 0.0, 1.0, 50.0],
                    crop: GpuCompositeSourceCrop::default(),
                    fused_point_operations: 3,
                },
            ],
            GpuCompositeExecutionPolicy::default(),
        )
        .expect("plan");

        assert_eq!(plan.layers.len(), 2);
        assert_eq!(
            plan.layers[1].damage,
            GpuCompositeRect { x: 100, y: 50, width: 320, height: 180 }
        );
        assert_eq!(plan.layers[1].preserved_regions.len(), 4);
        assert_eq!(plan.diagnostics.fused_point_operations, 3);
        assert_eq!(plan.diagnostics.fused_layer_passes, 1);
        assert_eq!(plan.diagnostics.shaded_pixels, 1920 * 1080 + 320 * 180);
        assert_eq!(
            plan.diagnostics.avoided_shader_pixels,
            1920 * 1080 - 320 * 180
        );
        assert_eq!(
            plan.diagnostics.preserved_copy_pixels,
            1920 * 1080 - 320 * 180
        );
    }

    #[test]
    fn off_canvas_and_zero_crop_layers_are_eliminated() {
        let plan = GpuCompositeExecutionPlanner::plan(
            1920,
            1080,
            &[
                GpuCompositeLayerFootprint::NoContribution,
                GpuCompositeLayerFootprint::TransformedSource {
                    source_width: 100,
                    source_height: 100,
                    transform: [1.0, 0.0, 3000.0, 0.0, 1.0, 0.0],
                    crop: GpuCompositeSourceCrop::default(),
                    fused_point_operations: 0,
                },
                GpuCompositeLayerFootprint::TransformedSource {
                    source_width: 100,
                    source_height: 100,
                    transform: IDENTITY,
                    crop: GpuCompositeSourceCrop {
                        left: 0.5,
                        right: 0.5,
                        ..GpuCompositeSourceCrop::default()
                    },
                    fused_point_operations: 0,
                },
            ],
            GpuCompositeExecutionPolicy::default(),
        )
        .expect("plan");

        assert!(plan.layers.is_empty());
        assert_eq!(plan.diagnostics.eliminated_layers, 3);
    }

    #[test]
    fn eight_k_canvas_is_tiled_inside_one_render_pass() {
        let plan = GpuCompositeExecutionPlanner::plan(
            7680,
            4320,
            &[GpuCompositeLayerFootprint::FullCanvas { fused_point_operations: 12 }],
            GpuCompositeExecutionPolicy::default(),
        )
        .expect("plan");

        assert_eq!(plan.diagnostics.render_passes, 1);
        assert_eq!(plan.diagnostics.tile_draws, 4);
        assert_eq!(plan.layers[0].tiles.len(), 4);
        assert_eq!(plan.diagnostics.shaded_pixels, 7680 * 4320);
    }

    #[test]
    fn adversarial_extent_fails_before_building_an_unbounded_tile_schedule() {
        let error = GpuCompositeExecutionPlanner::plan(
            u32::MAX,
            u32::MAX,
            &[GpuCompositeLayerFootprint::FullCanvas { fused_point_operations: 0 }],
            GpuCompositeExecutionPolicy::default(),
        )
        .expect_err("unbounded tile schedule");

        assert!(matches!(
            error,
            GpuCompositeExecutionPlanError::TileLimitExceeded {
                maximum_tiles: MAX_GPU_COMPOSITE_TILES,
                ..
            }
        ));
    }

    #[test]
    fn crop_intersection_tightens_transformed_source_damage() {
        let crop = GpuCompositeSourceCrop { left: 0.25, top: 0.25, right: 0.0, bottom: 0.0 }
            .intersect(GpuCompositeSourceCrop { left: 0.0, top: 0.0, right: 0.25, bottom: 0.25 });
        let plan = GpuCompositeExecutionPlanner::plan(
            100,
            100,
            &[GpuCompositeLayerFootprint::TransformedSource {
                source_width: 100,
                source_height: 100,
                transform: IDENTITY,
                crop,
                fused_point_operations: 1,
            }],
            GpuCompositeExecutionPolicy::default(),
        )
        .expect("plan");

        assert_eq!(
            plan.layers[0].damage,
            GpuCompositeRect { x: 24, y: 24, width: 51, height: 51 }
        );
    }
}
