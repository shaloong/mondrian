//! Motion tile: repeats the incoming frame over a tiled field.
//!
//! The tile is the source frame scaled by the authored tile width and tile
//! height percentages, anchored at the authored tile centre. The tiled field
//! itself is scaled by the authored output width and height percentages, so a
//! value above 100 % pushes the tiling pattern beyond the frame. `mirror
//! edges` flips every other tile along each axis, `phase` shifts the tiling
//! origin by whole pixels and the horizontal phase shifts it by a fraction of
//! a tile. Tiles never sample outside the source frame: the resolved source
//! coordinate is clamped, which keeps the edge pixel replicated and avoids
//! inventing colour.
//!
//! The op is a coordinate remap, so it is O(1) per pixel and can run as a
//! fragment pass on the GPU with the same arithmetic.

use thiserror::Error;

/// Smallest accepted tile size in source pixels per axis.
const MIN_TILE_PIXELS: f32 = 1.0;

/// Largest accepted tile or output extent as an authored percentage.
pub const MOTION_TILE_MAX_PERCENT: f32 = 30_000.0;

/// Errors produced when the authored parameter set is not usable.
#[derive(Debug, Error, PartialEq)]
pub enum MotionTileError {
    #[error(
        "motion tile tile width must be finite and within [0, {MOTION_TILE_MAX_PERCENT}] percent"
    )]
    TileWidth,
    #[error(
        "motion tile tile height must be finite and within [0, {MOTION_TILE_MAX_PERCENT}] percent"
    )]
    TileHeight,
    #[error(
        "motion tile output width must be finite and within [0, {MOTION_TILE_MAX_PERCENT}] percent"
    )]
    OutputWidth,
    #[error("motion tile output height must be finite and within [0, {MOTION_TILE_MAX_PERCENT}] percent")]
    OutputHeight,
    #[error("motion tile centre must be finite")]
    Center,
    #[error("motion tile phase must be finite")]
    Phase,
    #[error("motion tile horizontal phase must be finite and within [0, 1]")]
    HorizontalPhase,
}

/// Authored motion tile parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionTileGrade {
    center: [f32; 2],
    tile_width_percent: f32,
    tile_height_percent: f32,
    output_width_percent: f32,
    output_height_percent: f32,
    mirror_edges: bool,
    phase: f32,
    horizontal_phase: f32,
}

impl MotionTileGrade {
    /// Builds a grade, rejecting values that cannot define a tile field.
    pub fn new(
        center: [f32; 2],
        tile_width_percent: f32,
        tile_height_percent: f32,
        output_width_percent: f32,
        output_height_percent: f32,
        mirror_edges: bool,
        phase: f32,
        horizontal_phase: f32,
    ) -> Result<Self, MotionTileError> {
        if !center[0].is_finite() || !center[1].is_finite() {
            return Err(MotionTileError::Center);
        }
        for (value, error) in [
            (tile_width_percent, MotionTileError::TileWidth),
            (tile_height_percent, MotionTileError::TileHeight),
            (output_width_percent, MotionTileError::OutputWidth),
            (output_height_percent, MotionTileError::OutputHeight),
        ] {
            if !value.is_finite() || !(0.0..=MOTION_TILE_MAX_PERCENT).contains(&value) {
                return Err(error);
            }
        }
        if !phase.is_finite() {
            return Err(MotionTileError::Phase);
        }
        if !horizontal_phase.is_finite() || !(0.0..=1.0).contains(&horizontal_phase) {
            return Err(MotionTileError::HorizontalPhase);
        }
        Ok(Self {
            center,
            tile_width_percent,
            tile_height_percent,
            output_width_percent,
            output_height_percent,
            mirror_edges,
            phase,
            horizontal_phase,
        })
    }

    /// Tile centre in frame pixels.
    pub fn center(&self) -> [f32; 2] {
        self.center
    }

    /// Tile width as a percentage of the frame width.
    pub fn tile_width_percent(&self) -> f32 {
        self.tile_width_percent
    }

    /// Tile height as a percentage of the frame height.
    pub fn tile_height_percent(&self) -> f32 {
        self.tile_height_percent
    }

    /// Output field width as a percentage of the frame width.
    pub fn output_width_percent(&self) -> f32 {
        self.output_width_percent
    }

    /// Output field height as a percentage of the frame height.
    pub fn output_height_percent(&self) -> f32 {
        self.output_height_percent
    }

    /// Whether alternating tiles are mirrored.
    pub fn mirror_edges(&self) -> bool {
        self.mirror_edges
    }

    /// Whole-pixel phase offset applied to both axes.
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// Fraction of a tile added to the horizontal phase.
    pub fn horizontal_phase(&self) -> f32 {
        self.horizontal_phase
    }

    /// A tile that leaves every pixel untouched: full-size tile, full-size
    /// output, no mirroring and no phase.
    pub fn is_identity(&self) -> bool {
        !self.mirror_edges
            && self.phase == 0.0
            && self.horizontal_phase == 0.0
            && (self.tile_width_percent - 100.0).abs() <= f32::EPSILON
            && (self.tile_height_percent - 100.0).abs() <= f32::EPSILON
            && (self.output_width_percent - 100.0).abs() <= f32::EPSILON
            && (self.output_height_percent - 100.0).abs() <= f32::EPSILON
    }

    /// Packed GPU controls: `[tile_w, tile_h, out_w, out_h, center_x,
    /// center_y, phase, horizontal_phase * tile_w]` plus a mirror flag the
    /// shader reads from `params.w`.
    pub fn gpu_controls(&self, width: u32, height: u32) -> [f32; 8] {
        let (tile_width, tile_height) = self.tile_pixels(width, height);
        let (output_width, output_height) = self.output_pixels(width, height);
        [
            tile_width,
            tile_height,
            output_width,
            output_height,
            self.center[0],
            self.center[1],
            self.phase,
            self.horizontal_phase * tile_width,
        ]
    }

    fn tile_pixels(&self, width: u32, height: u32) -> (f32, f32) {
        let tile_width =
            (width.max(1) as f32 * self.tile_width_percent / 100.0).max(MIN_TILE_PIXELS);
        let tile_height =
            (height.max(1) as f32 * self.tile_height_percent / 100.0).max(MIN_TILE_PIXELS);
        (tile_width, tile_height)
    }

    fn output_pixels(&self, width: u32, height: u32) -> (f32, f32) {
        let output_width =
            (width.max(1) as f32 * self.output_width_percent / 100.0).max(MIN_TILE_PIXELS);
        let output_height =
            (height.max(1) as f32 * self.output_height_percent / 100.0).max(MIN_TILE_PIXELS);
        (output_width, output_height)
    }

    /// Resolves the source coordinate for one destination pixel.
    pub fn source_pixel(&self, x: u32, y: u32, width: u32, height: u32) -> (usize, usize) {
        let (tile_width, tile_height) = self.tile_pixels(width, height);
        // Scale the destination back into the tile field: an output extent
        // above the frame spreads the same tiles over more pixels.
        let output_scale_x = (self.output_width_percent / 100.0).max(0.01);
        let output_scale_y = (self.output_height_percent / 100.0).max(0.01);
        let mut qx = (x as f32 + 0.5 - self.center[0]) / (tile_width * output_scale_x) + 0.5;
        let mut qy = (y as f32 + 0.5 - self.center[1]) / (tile_height * output_scale_y) + 0.5;
        qx -= self.phase / tile_width;
        qy -= self.phase / tile_height;
        qx -= self.horizontal_phase;
        (self.resolve_axis(qx, width), self.resolve_axis(qy, height))
    }

    /// Resolves a tile-unit coordinate into a source pixel index, mirroring
    /// odd tiles when the authored flag asks for it.
    fn resolve_axis(&self, q: f32, extent: u32) -> usize {
        let index = q.floor();
        let mut fraction = q - index;
        if self.mirror_edges && (index as i64).rem_euclid(2) == 1 {
            fraction = 1.0 - fraction;
        }
        let pixel = (fraction * extent.max(1) as f32).floor();
        (pixel.max(0.0) as usize).min(extent.max(1) as usize - 1)
    }
}

/// Applies motion tile in place; the source frame is snapshotted first so the
/// remap never reads pixels it has already written.
pub(crate) fn apply_motion_tile_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &MotionTileGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() != width.saturating_mul(height) {
        return Ok(false);
    }
    let width = width as u32;
    let height = height as u32;
    let source = pixels.to_vec();
    checkpoint()?;
    for y in 0..height {
        for x in 0..width {
            let (source_x, source_y) = grade.source_pixel(x, y, width, height);
            let source_x = source_x.min(width as usize - 1);
            let source_y = source_y.min(height as usize - 1);
            let index = source_y * width as usize + source_x;
            pixels[y as usize * width as usize + x as usize] = source[index];
        }
        if y % 512 == 0 {
            checkpoint()?;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Whole-frame helper used by tests and the renderer parity check.
pub fn motion_tile_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &MotionTileGrade,
) -> bool {
    match apply_motion_tile_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    }) {
        Ok(applied) => applied,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(tile_width: f32, tile_height: f32, mirror: bool, phase: f32) -> MotionTileGrade {
        MotionTileGrade::new(
            [2.0, 2.0],
            tile_width,
            tile_height,
            100.0,
            100.0,
            mirror,
            phase,
            0.0,
        )
        .expect("valid grade")
    }

    fn ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| [index as f32, (index * 2) as f32, 1.0, 1.0])
            .collect()
    }

    #[test]
    fn range_validation_rejects_impossible_parameters() {
        assert_eq!(
            MotionTileGrade::new([0.0, 0.0], -1.0, 100.0, 100.0, 100.0, false, 0.0, 0.0)
                .unwrap_err(),
            MotionTileError::TileWidth
        );
        assert_eq!(
            MotionTileGrade::new([0.0, 0.0], 100.0, 100.0, 100.0, 100.0, false, f32::NAN, 0.0)
                .unwrap_err(),
            MotionTileError::Phase
        );
        assert_eq!(
            MotionTileGrade::new([0.0, 0.0], 100.0, 100.0, 100.0, 100.0, false, 0.0, 1.5)
                .unwrap_err(),
            MotionTileError::HorizontalPhase
        );
        assert_eq!(
            MotionTileGrade::new(
                [f32::INFINITY, 0.0],
                100.0,
                100.0,
                100.0,
                100.0,
                false,
                0.0,
                0.0
            )
            .unwrap_err(),
            MotionTileError::Center
        );
    }

    #[test]
    fn identity_tile_copies_the_frame() {
        let grade = MotionTileGrade::new([1.5, 1.0], 100.0, 100.0, 100.0, 100.0, false, 0.0, 0.0)
            .expect("identity grade");
        let mut frame = ramp(3, 2);
        let original = frame.clone();
        assert!(motion_tile_frame_f32(&mut frame, 3, 2, &grade));
        assert_eq!(frame, original);
        assert!(grade.is_identity());
    }

    #[test]
    fn mirrored_edge_tiles_flip_the_neighbouring_tile() {
        let mut grade = grade(50.0, 100.0, true, 0.0);
        grade.center = [0.0, 0.5];
        // A 4 px tile on an 8 px frame: tile 1 covers x = 2..5 and mirrors.
        assert_eq!(grade.source_pixel(0, 0, 8, 1), (5, 0));
        assert_eq!(grade.source_pixel(2, 0, 8, 1), (7, 0));
        assert_eq!(grade.source_pixel(3, 0, 8, 1), (5, 0));
    }

    #[test]
    fn plain_tiles_repeat_the_source_without_mirroring() {
        let mut grade = grade(50.0, 100.0, false, 0.0);
        grade.center = [0.0, 0.5];
        assert_eq!(grade.source_pixel(0, 0, 8, 1), (5, 0));
        assert_eq!(grade.source_pixel(1, 0, 8, 1), (7, 0));
        assert_eq!(grade.source_pixel(2, 0, 8, 1), (1, 0));
        // The tiling repeats every tile width in destination pixels.
        assert_eq!(
            grade.source_pixel(4, 0, 8, 1),
            grade.source_pixel(0, 0, 8, 1)
        );
    }

    #[test]
    fn whole_pixel_phase_rotates_the_tiling_window() {
        let grade = MotionTileGrade::new([2.0, 0.5], 100.0, 100.0, 100.0, 100.0, false, 1.0, 0.0)
            .expect("phase grade");
        let mut frame: Vec<[f32; 4]> = vec![
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [2.0, 0.0, 0.0, 1.0],
            [3.0, 0.0, 0.0, 1.0],
        ];
        assert!(motion_tile_frame_f32(&mut frame, 4, 1, &grade));
        let shifted: Vec<f32> = frame.iter().map(|pixel| pixel[0]).collect();
        assert_eq!(shifted, vec![3.0, 0.0, 1.0, 2.0]);
    }

    #[test]
    fn transparent_pixels_stay_transparent_and_colour_is_not_clamped() {
        let grade = MotionTileGrade::new([1.0, 0.5], 100.0, 100.0, 100.0, 100.0, false, 0.0, 0.0)
            .expect("identity grade");
        let mut frame: Vec<[f32; 4]> = vec![[0.0, 0.0, 0.0, 0.0], [2.5, -0.5, 1.0, 1.0]];
        assert!(motion_tile_frame_f32(&mut frame, 2, 1, &grade));
        assert_eq!(frame[0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(frame[1], [2.5, -0.5, 1.0, 1.0]);
    }

    #[test]
    fn output_extent_above_the_frame_spreads_the_same_tiles() {
        let grade = MotionTileGrade::new([2.0, 2.0], 50.0, 50.0, 200.0, 200.0, false, 0.0, 0.0)
            .expect("output grade");
        assert_eq!(grade.source_pixel(0, 0, 4, 4), (0, 0));
        assert_eq!(grade.source_pixel(2, 2, 4, 4), (2, 2));
        assert_eq!(grade.source_pixel(4, 2, 4, 4), (0, 2));
    }

    #[test]
    fn gpu_controls_carry_the_resolved_tile_geometry() {
        let grade =
            MotionTileGrade::new([4.0, 3.0], 50.0, 25.0, 100.0, 200.0, true, 2.0, 0.5).unwrap();
        let controls = grade.gpu_controls(8, 8);
        assert_eq!(controls[0], 4.0);
        assert_eq!(controls[1], 2.0);
        assert_eq!(controls[2], 8.0);
        assert_eq!(controls[3], 16.0);
        assert_eq!(controls[4], 4.0);
        assert_eq!(controls[5], 3.0);
        assert_eq!(controls[6], 2.0);
        assert_eq!(controls[7], 2.0);
    }
}
