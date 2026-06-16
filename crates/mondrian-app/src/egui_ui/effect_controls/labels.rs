//! Display labels and option lists — re-exports from `mondrian-core`.
//!
//! TODO: delete this file once egui panels are fully replaced.
//! Callers should use the `mondrian-core::display_labels` equivalents directly.

pub(crate) use mondrian_core::display_labels::{
    alpha_interpretation_label, blend_mode_display_label, blend_mode_options, color_space_label,
    field_order_label, frame_rate_label, mask_op_display_label, mask_op_options,
    pixel_aspect_ratio_label,
};

// color_space_options returns the set of color spaces as an array for iteration
// in dropdown menus. The display_labels module owns the labels; this helper
// returns the concrete enum values for egui `for` loops.
pub(crate) fn color_space_options() -> [mondrian_core::ColorSpace; 9] {
    use mondrian_core::ColorSpace;
    [
        ColorSpace::Rec709,
        ColorSpace::Rec2100Hlg,
        ColorSpace::Rec2100Pq,
        ColorSpace::Srgb,
        ColorSpace::Rec2020,
        ColorSpace::DciP3,
        ColorSpace::AppleLog,
        ColorSpace::SLog3,
        ColorSpace::ArriLogC4,
    ]
}
