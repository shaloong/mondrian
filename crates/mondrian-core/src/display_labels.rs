//! Human-readable display labels for Mondrian domain types.
//!
//! These functions convert enum values and domain data into user-facing strings
//! suitable for dropdown options, property inspector rows, and graph editor
//! overlays. No UI-framework types appear here — every function returns plain
//! `String` or `&'static str`.

use crate::timeline_data::{AlphaInterpretation, FieldOrder, PixelAspectRatio};
use crate::types::{ColorSpace, Rational};

/// Options for the blend-mode dropdown.
#[derive(Debug, Clone, Copy)]
pub struct BlendModeOption {
    pub label: &'static str,
    pub value: &'static str,
}

impl BlendModeOption {
    // Helpers removed — struct literals are const-compatible in array expressions.
}

/// All blend mode options in a canonical display order.
pub fn blend_mode_options() -> &'static [BlendModeOption] {
    &[
        BlendModeOption { label: "继承轨道", value: "inherit" },
        BlendModeOption { label: "正常", value: "Normal" },
        BlendModeOption { label: "溶解", value: "Dissolve" },
        BlendModeOption { label: "变暗", value: "Darken" },
        BlendModeOption { label: "正片叠底", value: "Multiply" },
        BlendModeOption { label: "颜色加深", value: "ColorBurn" },
        BlendModeOption { label: "线性加深", value: "LinearBurn" },
        BlendModeOption { label: "深色", value: "DarkerColor" },
        BlendModeOption { label: "变亮", value: "Lighten" },
        BlendModeOption { label: "滤色", value: "Screen" },
        BlendModeOption { label: "颜色减淡", value: "ColorDodge" },
        BlendModeOption {
            label: "线性减淡(添加)", value: "LinearDodge"
        },
        BlendModeOption { label: "浅色", value: "LighterColor" },
        BlendModeOption { label: "叠加", value: "Overlay" },
        BlendModeOption { label: "柔光", value: "SoftLight" },
        BlendModeOption { label: "强光", value: "HardLight" },
        BlendModeOption { label: "亮光", value: "VividLight" },
        BlendModeOption { label: "线性光", value: "LinearLight" },
        BlendModeOption { label: "点光", value: "PinLight" },
        BlendModeOption { label: "强混合", value: "HardMix" },
        BlendModeOption { label: "差值", value: "Difference" },
        BlendModeOption { label: "排除", value: "Exclusion" },
        BlendModeOption { label: "相减", value: "Subtract" },
        BlendModeOption { label: "相除", value: "Divide" },
        BlendModeOption { label: "色相", value: "Hue" },
        BlendModeOption { label: "饱和度", value: "Saturation" },
        BlendModeOption { label: "颜色", value: "Color" },
        BlendModeOption { label: "发光度", value: "Luminosity" },
    ]
}

/// Human-readable label for a blend mode value (including "inherit").
pub fn blend_mode_display_label(value: &str) -> String {
    match value {
        "inherit" => "继承轨道".to_string(),
        "Normal" => "正常".to_string(),
        "Dissolve" => "溶解".to_string(),
        "Multiply" => "正片叠底".to_string(),
        "Screen" => "滤色".to_string(),
        "Overlay" => "叠加".to_string(),
        "Darken" => "变暗".to_string(),
        "Lighten" => "变亮".to_string(),
        "ColorDodge" => "颜色减淡".to_string(),
        "ColorBurn" => "颜色加深".to_string(),
        "HardLight" => "强光".to_string(),
        "SoftLight" => "柔光".to_string(),
        "Difference" => "差值".to_string(),
        "Exclusion" => "排除".to_string(),
        "Subtract" => "相减".to_string(),
        "DarkerColor" => "深色".to_string(),
        "LighterColor" => "浅色".to_string(),
        "LinearBurn" => "线性加深".to_string(),
        "LinearDodge" => "线性减淡(添加)".to_string(),
        "VividLight" => "亮光".to_string(),
        "LinearLight" => "线性光".to_string(),
        "PinLight" => "点光".to_string(),
        "HardMix" => "强混合".to_string(),
        "Divide" => "相除".to_string(),
        "Hue" => "色相".to_string(),
        "Saturation" => "饱和度".to_string(),
        "Color" => "颜色".to_string(),
        "Luminosity" => "发光度".to_string(),
        other => other.to_string(),
    }
}

/// Mask operation dropdown options.
pub fn mask_op_options() -> &'static [(&'static str, &'static str)] {
    &[
        ("Add", "相加"),
        ("Subtract", "相减"),
        ("Intersect", "交集"),
        ("Difference", "差值"),
    ]
}

/// Human-readable label for a mask operation.
pub fn mask_op_display_label(value: &str) -> String {
    match value {
        "Add" => "相加".to_string(),
        "Subtract" => "相减".to_string(),
        "Intersect" => "交集".to_string(),
        "Difference" => "差值".to_string(),
        other => other.to_string(),
    }
}

/// Human-readable label for a color space.
pub fn color_space_label(value: ColorSpace) -> &'static str {
    match value {
        ColorSpace::Rec709 => "Rec. 709",
        ColorSpace::Rec601Pal => "Rec. 601 PAL",
        ColorSpace::Rec601Ntsc => "Rec. 601 NTSC",
        ColorSpace::Rec2100Hlg => "Rec. 2100 HLG",
        ColorSpace::Rec2100Pq => "Rec. 2100 PQ",
        ColorSpace::Srgb => "sRGB",
        ColorSpace::Rec2020 => "Rec. 2020",
        ColorSpace::DciP3 => "DCI-P3",
        ColorSpace::AppleLog => "Apple Log",
        ColorSpace::SLog3 => "S-Log3",
        ColorSpace::ArriLogC4 => "ARRI LogC4",
    }
}

/// Human-readable label for a pixel aspect ratio preset.
pub fn pixel_aspect_ratio_label(value: PixelAspectRatio) -> &'static str {
    match value {
        PixelAspectRatio::Square => "方形像素",
        PixelAspectRatio::D1DvNtsc => "D1/DV NTSC",
        PixelAspectRatio::D1DvNtscWidescreen => "D1/DV NTSC 16:9",
        PixelAspectRatio::D1DvPal => "D1/DV PAL",
        PixelAspectRatio::D1DvPalWidescreen => "D1/DV PAL 16:9",
        PixelAspectRatio::Anamorphic2x => "变形 2:1",
        PixelAspectRatio::HdAnamorphic1080 => "HD 变形 1080",
        PixelAspectRatio::DvcproHd => "DVCPRO HD",
        PixelAspectRatio::Unknown => "未知 PAR",
    }
}

/// Human-readable label for a field order.
pub fn field_order_label(value: FieldOrder) -> &'static str {
    match value {
        FieldOrder::Progressive => "逐行扫描",
        FieldOrder::UpperFirst => "高场优先",
        FieldOrder::LowerFirst => "低场优先",
    }
}

/// Human-readable label for an alpha interpretation mode.
pub fn alpha_interpretation_label(value: AlphaInterpretation) -> &'static str {
    match value {
        AlphaInterpretation::Straight => "直通 Alpha",
        AlphaInterpretation::Premultiplied => "预乘 Alpha",
        AlphaInterpretation::Ignore => "忽略 Alpha",
    }
}

/// Human-readable label for a frame rate rational value.
pub fn frame_rate_label(value: Rational) -> String {
    let fps = value.to_f64();
    if (fps.fract()).abs() < 0.001 {
        format!("{fps:.0} fps")
    } else if (fps * 10.0).fract().abs() < 0.001 {
        format!("{fps:.1} fps")
    } else {
        format!("{fps:.3} fps")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── BlendMode ────────────────────────────────────────────────────────

    #[test]
    fn blend_mode_options_count_matches_enum_variants_plus_inherit() {
        // All BlendMode variants + the special "inherit" option.
        let options = blend_mode_options();
        assert_eq!(options.len(), 28);
    }

    #[test]
    fn blend_mode_option_labels_unique() {
        let options = blend_mode_options();
        let labels: Vec<&str> = options.iter().map(|opt| opt.label).collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(labels.len(), unique.len(), "duplicate blend mode labels");
    }

    #[test]
    fn blend_mode_option_values_unique() {
        let options = blend_mode_options();
        let values: Vec<&str> = options.iter().map(|opt| opt.value).collect();
        let mut unique = values.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(values.len(), unique.len(), "duplicate blend mode values");
    }

    #[test]
    fn blend_mode_display_label_covers_all_option_values() {
        for option in blend_mode_options() {
            let label = blend_mode_display_label(option.value);
            assert!(!label.is_empty(), "missing label for {}", option.value);
            assert_eq!(label, option.label, "label mismatch for {}", option.value);
        }
    }

    #[test]
    fn blend_mode_display_label_for_unknown_is_raw_value() {
        let label = blend_mode_display_label("CustomFoo");
        assert_eq!(label, "CustomFoo");
    }

    // ── MaskOp ───────────────────────────────────────────────────────────

    #[test]
    fn mask_op_options_count_matches_enum() {
        let options = mask_op_options();
        assert_eq!(options.len(), 4);
    }

    #[test]
    fn mask_op_display_label_covers_all_options() {
        for (value, label) in mask_op_options() {
            assert_eq!(mask_op_display_label(value), *label);
        }
    }

    #[test]
    fn mask_op_display_label_for_unknown_is_raw_value() {
        let label = mask_op_display_label("UnknownOp");
        assert_eq!(label, "UnknownOp");
    }

    // ── ColorSpace ───────────────────────────────────────────────────────

    #[test]
    fn color_space_label_covers_all_variants() {
        let spaces = [
            ColorSpace::Rec709,
            ColorSpace::Rec2100Hlg,
            ColorSpace::Rec2100Pq,
            ColorSpace::Srgb,
            ColorSpace::Rec2020,
            ColorSpace::DciP3,
            ColorSpace::AppleLog,
            ColorSpace::SLog3,
            ColorSpace::ArriLogC4,
        ];
        for space in spaces {
            let label = color_space_label(space);
            assert!(!label.is_empty(), "missing label for {space:?}");
        }
    }

    #[test]
    fn color_space_labels_are_unique() {
        let all = [
            ColorSpace::Rec709,
            ColorSpace::Rec2100Hlg,
            ColorSpace::Rec2100Pq,
            ColorSpace::Srgb,
            ColorSpace::Rec2020,
            ColorSpace::DciP3,
            ColorSpace::AppleLog,
            ColorSpace::SLog3,
            ColorSpace::ArriLogC4,
        ];
        let labels: Vec<&str> = all.iter().map(|&s| color_space_label(s)).collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(labels.len(), unique.len(), "duplicate color space labels");
    }

    // ── PixelAspectRatio ─────────────────────────────────────────────────

    #[test]
    fn pixel_aspect_ratio_label_covers_all_variants() {
        let ratios = [
            PixelAspectRatio::Square,
            PixelAspectRatio::D1DvNtsc,
            PixelAspectRatio::D1DvNtscWidescreen,
            PixelAspectRatio::D1DvPal,
            PixelAspectRatio::D1DvPalWidescreen,
            PixelAspectRatio::Anamorphic2x,
            PixelAspectRatio::HdAnamorphic1080,
            PixelAspectRatio::DvcproHd,
            PixelAspectRatio::Unknown,
        ];
        for ratio in ratios {
            let label = pixel_aspect_ratio_label(ratio);
            assert!(!label.is_empty(), "missing label for {ratio:?}");
        }
    }

    #[test]
    fn pixel_aspect_ratio_labels_are_unique() {
        let all = [
            PixelAspectRatio::Square,
            PixelAspectRatio::D1DvNtsc,
            PixelAspectRatio::D1DvNtscWidescreen,
            PixelAspectRatio::D1DvPal,
            PixelAspectRatio::D1DvPalWidescreen,
            PixelAspectRatio::Anamorphic2x,
            PixelAspectRatio::HdAnamorphic1080,
            PixelAspectRatio::DvcproHd,
            PixelAspectRatio::Unknown,
        ];
        let labels: Vec<&str> = all.iter().map(|&r| pixel_aspect_ratio_label(r)).collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(labels.len(), unique.len(), "duplicate PAR labels");
    }

    // ── FieldOrder ───────────────────────────────────────────────────────

    #[test]
    fn field_order_label_covers_all_variants() {
        let orders = [
            FieldOrder::Progressive,
            FieldOrder::UpperFirst,
            FieldOrder::LowerFirst,
        ];
        for order in orders {
            let label = field_order_label(order);
            assert!(!label.is_empty(), "missing label for {order:?}");
        }
    }

    #[test]
    fn field_order_labels_are_unique() {
        let all = [
            FieldOrder::Progressive,
            FieldOrder::UpperFirst,
            FieldOrder::LowerFirst,
        ];
        let labels: Vec<&str> = all.iter().map(|&o| field_order_label(o)).collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(labels.len(), unique.len(), "duplicate field order labels");
    }

    // ── AlphaInterpretation ──────────────────────────────────────────────

    #[test]
    fn alpha_interpretation_label_covers_all_variants() {
        let modes = [
            AlphaInterpretation::Straight,
            AlphaInterpretation::Premultiplied,
            AlphaInterpretation::Ignore,
        ];
        for mode in modes {
            let label = alpha_interpretation_label(mode);
            assert!(!label.is_empty(), "missing label for {mode:?}");
        }
    }

    // ── Frame rate ───────────────────────────────────────────────────────

    #[test]
    fn frame_rate_label_integer_fps() {
        let label = frame_rate_label(Rational::FPS_24);
        assert_eq!(label, "24 fps");
    }

    #[test]
    fn frame_rate_label_ntsc_fps() {
        let label = frame_rate_label(Rational::FPS_23976);
        assert_eq!(label, "23.976 fps");
    }

    #[test]
    fn frame_rate_label_three_decimals() {
        // 25/1001 = ~0.024975... → 3 decimal places
        let label = frame_rate_label(Rational::new(25, 1001));
        assert!(label.contains("fps"), "unexpected format: {label}");
    }

    #[test]
    fn frame_rate_label_one_decimal() {
        // 12/5 = 2.4
        let label = frame_rate_label(Rational::new(12, 5));
        assert_eq!(label, "2.4 fps");
    }
}
