//! Edge detection for scene-linear frames.
//!
//! The kernel runs on premultiplied colour so transparent neighbours cannot
//! leak colour into an edge, and writes straight alpha back out. Gradients are
//! measured per colour channel and combined as a Euclidean norm of all six
//! Sobel taps, which keeps the result independent of the project working space.

use crate::coverage::has_positive_coverage;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Radius of the Sobel neighbourhood, in pixels.
pub const FIND_EDGES_HALO_PIXELS: u32 = 1;

/// Largest halo this kernel asks the executor for.
pub const FIND_EDGES_MAX_HALO_PIXELS: u32 = FIND_EDGES_HALO_PIXELS;

/// Authored state for the find edges effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FindEdgesGrade {
    blend_with_original: f32,
    invert: bool,
}

/// Invalid authored find edges state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FindEdgesError {
    #[error("find edges blend with original must be finite and within [0, 1]")]
    BlendWithOriginal,
}

impl FindEdgesGrade {
    /// Builds a validated grade.
    pub fn new(blend_with_original: f32, invert: bool) -> Result<Self, FindEdgesError> {
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(FindEdgesError::BlendWithOriginal);
        }
        Ok(Self { blend_with_original, invert })
    }

    /// Fraction of the original frame kept in the mix.
    pub const fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// Whether edges are drawn as bright lines instead of dark ones.
    pub const fn invert(self) -> bool {
        self.invert
    }

    /// A mix that keeps the whole original frame is the identity.
    pub fn is_identity(self) -> bool {
        self.blend_with_original >= 1.0
    }

    /// Pixel halo the executor has to supply around the requested region.
    pub const fn input_halo(self) -> (u32, u32) {
        (FIND_EDGES_HALO_PIXELS, FIND_EDGES_HALO_PIXELS)
    }

    /// One Sobel response, expressed as a straight-alpha edge pixel.
    fn edge_pixel(self, magnitude: f32) -> [f32; 3] {
        if self.invert {
            [magnitude; 3]
        } else {
            [1.0 - magnitude; 3]
        }
    }
}

/// Applies the effect to one frame region.
///
/// `working` already holds the region and its halo; `width`/`height` are the
/// padded region dimensions, so edges at the region border see the halo rows
/// instead of being clamped.
pub(crate) fn apply_find_edges_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: FindEdgesGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width < 3 || height < 3 || working.len() < pixel_count {
        return Ok(false);
    }
    premultiply_rgba_controlled(&mut working[..pixel_count], checkpoint)?;
    let source = working[..pixel_count].to_vec();
    checkpoint()?;
    crate::parallel::for_each_row_band(working, width, 0..height, |first_row, band| {
        for (offset, row) in band.chunks_mut(width).enumerate() {
            let y = first_row + offset;
            for (x, pixel) in row.iter_mut().enumerate() {
                let original = source[y * width + x];
                if !has_positive_coverage(original[3].clamp(0.0, 1.0)) {
                    *pixel = [0.0; 4];
                    continue;
                }
                let magnitude = if x == 0 || y == 0 || x + 1 >= width || y + 1 >= height {
                    0.0
                } else {
                    sobel_magnitude(&source, width, x, y)
                };
                let edge = grade.edge_pixel(magnitude);
                let mix = grade.blend_with_original;
                let mixed = [
                    edge[0] * (1.0 - mix) + original[0] * mix,
                    edge[1] * (1.0 - mix) + original[1] * mix,
                    edge[2] * (1.0 - mix) + original[2] * mix,
                ];
                *pixel = [mixed[0], mixed[1], mixed[2], original[3].clamp(0.0, 1.0)];
            }
        }
    });
    checkpoint()?;
    unpremultiply_rgba_controlled(&mut working[..pixel_count], checkpoint)?;
    Ok(true)
}

fn sobel_magnitude(source: &[[f32; 4]], width: usize, x: usize, y: usize) -> f32 {
    let at = |dx: usize, dy: usize| source[(y + dy - 1) * width + (x + dx - 1)];
    let tl = at(0, 0);
    let tc = at(1, 0);
    let tr = at(2, 0);
    let ml = at(0, 1);
    let mr = at(2, 1);
    let bl = at(0, 2);
    let bc = at(1, 2);
    let br = at(2, 2);
    let mut magnitude = 0.0_f32;
    for channel in 0..3 {
        let gx = (tr[channel] + 2.0 * mr[channel] + br[channel])
            - (tl[channel] + 2.0 * ml[channel] + bl[channel]);
        let gy = (bl[channel] + 2.0 * bc[channel] + br[channel])
            - (tl[channel] + 2.0 * tc[channel] + tr[channel]);
        magnitude += gx * gx + gy * gy;
    }
    magnitude.sqrt()
}

/// Applies the effect to a whole straight-alpha frame in place.
///
/// The frame owns its own border, so the outermost pixel ring reports no edge
/// (the Sobel neighbourhood does not exist there). Executors that supply an ROI
/// halo call the region kernel instead and get real edges on that ring.
pub fn find_edges_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: FindEdgesGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    matches!(
        apply_find_edges_f32_controlled(pixels, width, height, grade, &mut checkpoint),
        Ok(true)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pixels: &mut [[f32; 4]], width: usize, height: usize, grade: FindEdgesGrade) {
        let mut checkpoint = || Ok::<(), ()>(());
        assert!(
            apply_find_edges_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap()
        );
    }

    fn step_frame() -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 25];
        for y in 0..5 {
            for x in 2..5 {
                pixels[y * 5 + x] = [1.0, 1.0, 1.0, 1.0];
            }
        }
        pixels
    }

    #[test]
    fn grade_rejects_out_of_range_blend() {
        assert_eq!(
            FindEdgesGrade::new(-0.1, false),
            Err(FindEdgesError::BlendWithOriginal)
        );
        assert_eq!(
            FindEdgesGrade::new(1.5, false),
            Err(FindEdgesError::BlendWithOriginal)
        );
        assert_eq!(
            FindEdgesGrade::new(f32::NAN, false),
            Err(FindEdgesError::BlendWithOriginal)
        );
        assert!(FindEdgesGrade::new(0.5, true).is_ok());
    }

    #[test]
    fn full_mix_is_the_identity() {
        assert!(FindEdgesGrade::new(1.0, false).unwrap().is_identity());
        assert!(!FindEdgesGrade::new(0.999, false).unwrap().is_identity());
    }

    #[test]
    fn a_flat_frame_has_no_edges() {
        let mut pixels = vec![[0.25_f32, 0.25, 0.25, 1.0]; 25];
        run(&mut pixels, 5, 5, FindEdgesGrade::new(0.0, false).unwrap());
        for pixel in &pixels {
            assert!((pixel[0] - 1.0).abs() < 1.0e-6, "{pixel:?}");
            assert!((pixel[3] - 1.0).abs() < 1.0e-6);
        }
    }

    #[test]
    fn a_step_edge_draws_a_dark_line_and_invert_flips_it() {
        let mut pixels = step_frame();
        run(&mut pixels, 5, 5, FindEdgesGrade::new(0.0, false).unwrap());
        let edge = pixels[2 * 5 + 2];
        assert!(edge[0] < 0.5, "dark edge expected, got {edge:?}");
        assert!((edge[3] - 1.0).abs() < 1.0e-6);

        let mut inverted = step_frame();
        run(&mut inverted, 5, 5, FindEdgesGrade::new(0.0, true).unwrap());
        let bright = inverted[2 * 5 + 2];
        assert!(bright[0] > 0.5, "bright edge expected, got {bright:?}");
    }

    #[test]
    fn the_mix_keeps_the_requested_fraction_of_the_frame() {
        let mut pixels = step_frame();
        run(&mut pixels, 5, 5, FindEdgesGrade::new(1.0, false).unwrap());
        assert_eq!(pixels, step_frame());

        let mut half = step_frame();
        run(&mut half, 5, 5, FindEdgesGrade::new(0.5, false).unwrap());
        let original = step_frame()[2 * 5 + 2];
        let edge_only = {
            let mut edge_frame = step_frame();
            run(
                &mut edge_frame,
                5,
                5,
                FindEdgesGrade::new(0.0, false).unwrap(),
            );
            edge_frame[2 * 5 + 2]
        };
        let expected = 0.5 * original[0] + 0.5 * edge_only[0];
        assert!((half[2 * 5 + 2][0] - expected).abs() < 1.0e-6);
    }

    #[test]
    fn transparent_pixels_stay_empty() {
        let mut pixels = step_frame();
        pixels[2 * 5 + 2][3] = 0.0;
        run(&mut pixels, 5, 5, FindEdgesGrade::new(0.0, false).unwrap());
        assert_eq!(pixels[2 * 5 + 2], [0.0; 4]);
    }

    #[test]
    fn hdr_edges_are_not_clamped() {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 25];
        for y in 0..5 {
            for x in 2..5 {
                pixels[y * 5 + x] = [4.0, 4.0, 4.0, 1.0];
            }
        }
        run(&mut pixels, 5, 5, FindEdgesGrade::new(0.0, true).unwrap());
        assert!(
            pixels[2 * 5 + 2][0] > 1.0,
            "unclamped edge expected, got {:?}",
            pixels[2 * 5 + 2]
        );
    }
}
