//! Straight-alpha helpers shared by the spatial effect kernels.
//!
//! Public working-frame seams are straight/opaque: colour channels are not multiplied by alpha and
//! colour is only meaningful where coverage is positive. Spatial kernels must filter coverage and
//! colour together, so they premultiply the frame, run the weighted average, and unpremultiply
//! before handing the frame back. Filtering straight colour directly would let transparent black
//! neighbours darken an opaque pixel.

/// Multiplies the colour channels by the clamped alpha of every pixel.
pub(crate) fn premultiply_rgba_controlled<E>(
    pixels: &mut [[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for chunk in pixels.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            let alpha = pixel[3].clamp(0.0, 1.0);
            pixel[0] *= alpha;
            pixel[1] *= alpha;
            pixel[2] *= alpha;
            pixel[3] = alpha;
        }
    }
    checkpoint()
}

/// Divides the premultiplied colour channels by their coverage and restores straight alpha.
pub(crate) fn unpremultiply_rgba_controlled<E>(
    pixels: &mut [[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for chunk in pixels.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            *pixel = crate::coverage::straight_rgba_from_premultiplied(*pixel);
        }
    }
    checkpoint()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_scales_colour_by_clamped_alpha() {
        let mut pixels = vec![[0.5, 0.25, 1.0, 0.5], [1.0, 2.0, -0.5, 2.0]];
        premultiply_rgba_controlled(&mut pixels, &mut || Ok::<(), ()>(())).expect("no failure");
        assert_eq!(pixels[0], [0.25, 0.125, 0.5, 0.5]);
        assert_eq!(pixels[1], [1.0, 2.0, -0.5, 1.0]);
    }

    #[test]
    fn unpremultiply_restores_straight_colour() {
        let mut pixels = vec![[0.25, 0.125, 0.5, 0.5], [0.0, 0.0, 0.0, 0.0]];
        unpremultiply_rgba_controlled(&mut pixels, &mut || Ok::<(), ()>(())).expect("no failure");
        assert_eq!(pixels[0], [0.5, 0.25, 1.0, 0.5]);
        assert_eq!(pixels[1], [0.0, 0.0, 0.0, 0.0]);
    }
}
