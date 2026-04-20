use super::*;
use mondrian_core::types::Resolution;

fn pixel_at(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    let idx = (y * width + x) * 4;
    [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
}

#[test]
fn quantize_transform_signature_should_include_translation() {
    let identity = quantize_transform_signature([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    let translated = quantize_transform_signature([1.0, 0.0, 0.5, 0.0, 1.0, 0.0]);

    assert_ne!(identity, translated);
    assert_eq!(translated[2], 512);
}

#[test]
fn alpha_blend_layer_should_apply_translation_transform() {
    let dst_w = 5u32;
    let dst_h = 5u32;
    let src_w = 3u32;
    let src_h = 3u32;

    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    initialize_canvas_alpha_opaque(&mut dst);

    let mut src = vec![0u8; (src_w * src_h * 4) as usize];
    let src_center = ((src_w as usize) + 1) * 4;
    src[src_center] = 255;
    src[src_center + 1] = 0;
    src[src_center + 2] = 0;
    src[src_center + 3] = 255;

    alpha_blend_layer(
        &mut dst,
        dst_w,
        dst_h,
        &src,
        src_w,
        src_h,
        1.0,
        [1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
    );

    let moved = pixel_at(&dst, dst_w as usize, 2, 1);
    let original = pixel_at(&dst, dst_w as usize, 1, 1);

    assert_eq!(moved, [255, 0, 0, 255]);
    assert_eq!(original, [0, 0, 0, 255]);
}

#[test]
fn alpha_blend_layer_should_skip_non_invertible_transform() {
    let dst_w = 4u32;
    let dst_h = 4u32;
    let src_w = 2u32;
    let src_h = 2u32;

    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    initialize_canvas_alpha_opaque(&mut dst);
    let before = dst.clone();

    let src = vec![255u8; (src_w * src_h * 4) as usize];

    alpha_blend_layer(
        &mut dst,
        dst_w,
        dst_h,
        &src,
        src_w,
        src_h,
        1.0,
        [1.0, 2.0, 0.0, 2.0, 4.0, 0.0],
    );

    assert_eq!(dst, before);
}

#[test]
fn sequence_preview_target_size_preserves_sequence_aspect_ratio() {
    let size = sequence_preview_target_size(Resolution::DCI4K, 800.0, 600.0, 1.0);

    assert_eq!(size, (800, 422));
}

#[test]
fn fit_aspect_keeps_canvas_centered_with_sequence_ratio() {
    let outer = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(900.0, 700.0));
    let fitted = fit_aspect(outer, Resolution::FHD.aspect_ratio());

    assert!((fitted.center().x - outer.center().x).abs() < 0.001);
    assert!((fitted.center().y - outer.center().y).abs() < 0.001);
    assert!((fitted.width() / fitted.height() - Resolution::FHD.aspect_ratio()).abs() < 0.01);
}
