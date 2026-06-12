//! Display labels and option lists.
use super::*;

pub(crate) fn blend_mode_options() -> &'static [BlendModeOption] {
    static OPTIONS: [BlendModeOption; 28] = [
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
    ];
    &OPTIONS
}

pub(crate) fn blend_mode_display_label(value: &str) -> String {
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
        _ => value.to_string(),
    }
}

pub(crate) fn mask_op_options() -> &'static [(&'static str, &'static str)] {
    &[
        ("Add", "相加"),
        ("Subtract", "相减"),
        ("Intersect", "交集"),
        ("Difference", "差值"),
    ]
}

pub(crate) fn mask_op_display_label(value: &str) -> String {
    match value {
        "Add" => "相加".to_string(),
        "Subtract" => "相减".to_string(),
        "Intersect" => "交集".to_string(),
        "Difference" => "差值".to_string(),
        _ => value.to_string(),
    }
}

#[allow(dead_code)]
pub(crate) fn color_space_options() -> [ColorSpace; 9] {
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

pub(crate) fn color_space_label(value: ColorSpace) -> &'static str {
    match value {
        ColorSpace::Rec709 => "Rec. 709",
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

pub(crate) fn pixel_aspect_ratio_label(value: PixelAspectRatio) -> &'static str {
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

pub(crate) fn field_order_label(value: FieldOrder) -> &'static str {
    match value {
        FieldOrder::Progressive => "逐行扫描",
        FieldOrder::UpperFirst => "高场优先",
        FieldOrder::LowerFirst => "低场优先",
    }
}

pub(crate) fn alpha_interpretation_label(value: AlphaInterpretation) -> &'static str {
    match value {
        AlphaInterpretation::Straight => "直通 Alpha",
        AlphaInterpretation::Premultiplied => "预乘 Alpha",
        AlphaInterpretation::Ignore => "忽略 Alpha",
    }
}

pub(crate) fn frame_rate_label(value: Rational) -> String {
    let fps = value.to_f64();
    if (fps.fract()).abs() < 0.001 {
        format!("{fps:.0} fps")
    } else if (fps * 10.0).fract().abs() < 0.001 {
        format!("{fps:.1} fps")
    } else {
        format!("{fps:.3} fps")
    }
}
