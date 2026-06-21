//! Product-shell asset decoding helpers.
//!
//! These helpers keep Mondrian-branded bundled resources in the app layer
//! instead of leaking product assets into reusable widget crates.

use mondrian_ui_widgets::RasterImage;

/// Rasterize an SVG asset into a renderer-ready RGBA image.
pub(crate) fn rasterize_svg_asset(
    key: impl Into<String>,
    svg: &str,
    width: u32,
    height: u32,
) -> Option<RasterImage> {
    RasterImage::new(key, width, height, rasterize_svg_rgba(svg, width, height)?)
}

/// Rasterize an SVG into straight-alpha RGBA pixels.
pub(crate) fn rasterize_svg_rgba(svg: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }

    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default()).ok()?;
    let source_size = tree.size();
    let scale_x = width as f32 / source_size.width().max(1.0);
    let scale_y = height as f32 / source_size.height().max(1.0);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale_x, scale_y),
        &mut pixmap.as_mut(),
    );

    Some(unpremultiply_rgba(pixmap.data()))
}

fn unpremultiply_rgba(pixels: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(pixels.len());
    for pixel in pixels.chunks_exact(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }

        let red = (u32::from(pixel[0]) * 255 / alpha).min(255) as u8;
        let green = (u32::from(pixel[1]) * 255 / alpha).min(255) as u8;
        let blue = (u32::from(pixel[2]) * 255 / alpha).min(255) as u8;
        rgba.extend_from_slice(&[red, green, blue, pixel[3]]);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterize_svg_rgba_rejects_empty_dimensions() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>"#;

        assert!(rasterize_svg_rgba(svg, 0, 16).is_none());
        assert!(rasterize_svg_rgba(svg, 16, 0).is_none());
    }

    #[test]
    fn rasterize_svg_rgba_produces_straight_alpha_pixels() {
        let svg = r##"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2">
                <rect width="2" height="2" fill="#ff0000" fill-opacity="0.5"/>
            </svg>
        "##;

        let rgba = rasterize_svg_rgba(svg, 2, 2).expect("test svg should rasterize");

        assert_eq!(rgba.len(), 2 * 2 * 4);
        assert!(rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
        assert!(rgba.chunks_exact(4).any(|pixel| pixel[0] >= pixel[3]));
    }
}
