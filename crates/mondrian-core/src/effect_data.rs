//! Effect data types — pure data, no evaluation logic.
//!
//! These types define the data model for effects. The evaluation logic lives in
//! `mondrian-effects`. This separation allows `mondrian-timeline` to depend on
//! effect data types without depending on the full effect evaluation engine.

use crate::automation::{
    ParameterEnumOption, ParameterResourceReference, PropertyBag, PropertyDescriptor, PropertyValue,
};
use crate::types::EffectId;
use crate::{ParameterId, ParameterIdError, TimelineTime};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Supported effect types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectType {
    BasicCorrection,
    WhiteBalance,
    Lut3D,
    ColorWheel,
    HdrGrading,
    AscCdl,
    Curves,
    GamutCompression,
    HighlightRecovery,
    Qualifier,
    HueSaturationLightness,
    Crop,
    GaussianBlur,
    Sharpen,
    Vignette,
    ChromaticAberration,
    Grain,
    ChromaKey,
    LumaKey,
    Invert,
    BoxBlur,
    UnsharpMask,
    HueSaturation,
    BlackAndWhite,
    ShiftChannels,
    DirectionalBlur,
    ChannelMixer,
    DropShadow,
    FindEdges,
    ColorEmboss,
    Threshold,
    Mosaic,
    MotionTile,
    Mirror,
    /// Polar coordinates: converts between rectangular and polar coordinates.
    PolarCoordinates,
    LensDistortion,
    Flip,
    /// Ripple: pushes the frame in and out along the radius from a centre.
    Ripple,
    /// Wave warp: bends the frame along one travelling regular wave.
    WaveWarp,
    LensFlare,
    /// Bulge: magnifies an elliptical region of the frame.
    Bulge,
    /// Split: slides the two halves of the frame apart along a line.
    Split,
    /// Linear colour key: removes a colour with a tolerance and softness ramp.
    LinearColorKey,
    Extract,
    ColorOffset,
    Kaleidoscope,
    Bevel,
    Scatter,
    Tint,
    Vibrance,
    Tritone,
    RadialBlur,
    Convolution,
    Offset,
    Twirl,
    Spherize,
    Noise,
    BrushStrokes,
    Posterize,
    Emboss,
    /// Brightness and contrast: one gain and one offset in the working domain.
    BrightnessContrast,
    /// Levels: per-channel input and output points around a gamma.
    Levels,
    /// Transform: moves, scales, skews and rotates the frame about its centre.
    Transform,
    /// Corner pin: pins the frame onto an arbitrary quadrilateral.
    CornerPin,
    /// Turbulent displace: warps the frame with a fractal value-noise field.
    TurbulentDisplace,
    /// Deband: smooths the eight-bit colour banding of gradients.
    Deband,
    /// Ramp: a two-colour linear or radial gradient generator.
    Ramp,
    /// Glow: blooms bright colors or coverage edges with a threshold mask.
    Glow,
    /// Fractal noise: fills the frame with a fractal value-noise field.
    FractalNoise,
    /// Keylight: pulls a matte from a screen colour and despills the spill.
    Keylight,
    /// Matte refine: softens, shifts, feathers, and cleans the carried matte.
    MatteRefine,
    /// Echo: composites the frame over its own earlier instants.
    Echo,
    /// Arithmetic: combines the RGB channels with a constant per channel.
    Arithmetic,
    /// Leave color: keeps one authored colour and decolours everything else.
    LeaveColor,
    /// Broadcast safe: pulls the signal back inside the authored IRE amplitude.
    BroadcastSafe,
    /// HDR tone mapping: compresses scene-linear values onto the display range.
    HdrToneMapping,
    /// Choker: erodes or dilates the frame alpha without touching the colour.
    Choker,
    /// Test signals: generates a calibration frame from pixel position alone.
    TestSignals,
    /// Four-color gradient: blends four authored colours by inverse distance.
    FourColorGradient,
    /// Color balance: shifts the shadow, midtone, and highlight colours apart.
    ColorBalance,
    /// Minimax: morphology over each RGB channel and the alpha channel.
    Minimax,
    /// Spill suppressor: neutralises the backdrop colour that bounced onto the subject.
    SpillSuppressor,
    /// Channel blur: blurs the red, green, blue and alpha channels independently.
    ChannelBlur,
    /// Dent: pulls pixels towards (or pushes them away from) a control point.
    Dent,
    /// Change to colour: moves one authored colour family towards another.
    ChangeToColor,
    /// Warp: bends the layer along an arc.
    Warp,
    /// Grid warp: displaces a checkerboard of cell corners along the cell diagonals.
    GridWarp,
    /// Bezier warp: remaps the layer through an authored Coons patch of cubic edges.
    BezierWarp,
    /// Wire removal: clones pixels from a parallel displacement across an authored line.
    WireRemoval,
    /// Median: replaces every pixel with the per-channel median of its window.
    Median,
    /// Selective colour: moves CMYK ink inside nine hue and tone ranges.
    SelectiveColor,
    /// Roughen edges: grows shape edges out along a fractal field.
    RoughenEdges,
    /// Eyedropper fill: floods the layer with a statistic of one sample window.
    EyedropperFill,
    /// Checkerboard: paints a pixel-local checker grid with feathered edges.
    Checkerboard,
    /// Cell pattern: paints a pixel-local field of Worley cells.
    CellPattern,
    /// Fractal: paints an escape-time set with oversampling and a palette.
    Fractal,
    /// Remove color matting: recovers a straight-alpha sample from the matte
    /// background it was composited over.
    RemoveColorMatting,
    /// Photo filter: tints the frame with a photographic gel.
    PhotoFilter,
    /// JPEG damage: block quantisation, seam darkening and mosaic noise.
    JpegDamage,
    /// Colour map: maps a phase derived from the pixel onto a cyclic palette.
    Colorama,
    /// Grid: paints pixel-local grid lines with feathered edges.
    Grid,
    /// Circle and ellipse: paints a pixel-local soft-edged circle. The ellipse
    /// arm is deferred (design D-1), but the name keeps room for it.
    CircleEllipse,
    /// Stroke: paints a closed band along the layer's alpha contour.
    Stroke,
    /// Full: paints the whole layer with one colour behind an inset feather ramp.
    Fill,
    /// Magnify: enlarges a circular or square lens region around a centre.
    Magnify,
    /// Dust and scratches: replaces a pixel with its window median when the
    /// centre deviates from that median by more than the threshold.
    DustScratches,
    /// VR rotate sphere: re-projects the layer through a virtual-reality
    /// equirectangular sphere rotated by the authored tilt, pan and roll.
    VrRotateSphere,
    /// VR sphere to plane: projects an equirectangular frame through a
    /// virtual camera with a horizontal field of view and projection tilt.
    VrSphereToPlane,
    /// VR project 2D: projects a flat card onto an equirectangular frame at an
    /// authored angular scale, with separate source and projection poses.
    VrProject2D,
    /// Key cleaner: erodes, softens and re-steepens the alpha edge of a keyed
    /// matte without touching the colour channels.
    KeyCleaner,
    /// Channel combiner: rewrites the RGBA slots from a template of source
    /// channels, with an optional uniform RGB scale or shift for the derived
    /// luminance and lightness targets.
    ChannelCombiner,
    DisplacementMap,
    /// Expand the layer's raster with transparent padding.
    GrowBounds,
    /// Smart blur: a separable range-domain mean that only averages neighbours
    /// whose luminance stays within the authored threshold.
    SmartBlur,
    /// Coverage-weighted bilateral blur.
    BilateralBlur,
    ChromaticAberrationRemoval,
    Basic3D,
    CameraView,
    BlankingFill,
    /// Externally registered effect definition, including native-format adapters.
    /// This is a domain definition reference, not an Extension package identity.
    /// The serde name preserves the existing project wire representation.
    #[serde(rename = "Plugin")]
    External(String),
}

impl EffectType {
    pub fn key(&self) -> String {
        match self {
            Self::BasicCorrection => "builtin.basic_correction".to_string(),
            Self::WhiteBalance => "builtin.white_balance".to_string(),
            Self::Lut3D => "builtin.lut_3d".to_string(),
            Self::ColorWheel => "builtin.color_wheel".to_string(),
            Self::HdrGrading => "builtin.hdr_grading".to_string(),
            Self::AscCdl => "builtin.asc_cdl".to_string(),
            Self::Curves => "builtin.curves".to_string(),
            Self::GamutCompression => "builtin.gamut_compression".to_string(),
            Self::HighlightRecovery => "builtin.highlight_recovery".to_string(),
            Self::Qualifier => "builtin.qualifier".to_string(),
            Self::HueSaturationLightness => "builtin.hue_saturation_lightness".to_string(),
            Self::Crop => "builtin.crop".to_string(),
            Self::GaussianBlur => "builtin.gaussian_blur".to_string(),
            Self::Sharpen => "builtin.sharpen".to_string(),
            Self::Vignette => "builtin.vignette".to_string(),
            Self::ChromaticAberration => "builtin.chromatic_aberration".to_string(),
            Self::Grain => "builtin.grain".to_string(),
            Self::ChromaKey => "builtin.chroma_key".to_string(),
            Self::LumaKey => "builtin.luma_key".to_string(),
            Self::Invert => "builtin.invert".to_string(),
            Self::BoxBlur => "builtin.box_blur".to_string(),
            Self::UnsharpMask => "builtin.unsharp_mask".to_string(),
            Self::HueSaturation => "builtin.hue_saturation".to_string(),
            Self::BlackAndWhite => "builtin.black_and_white".to_string(),
            Self::ShiftChannels => "builtin.shift_channels".to_string(),
            Self::DirectionalBlur => "builtin.directional_blur".to_string(),
            Self::ChannelMixer => "builtin.channel_mixer".to_string(),
            Self::DropShadow => "builtin.drop_shadow".to_string(),
            Self::FindEdges => "builtin.find_edges".to_string(),
            Self::Emboss => "builtin.emboss".to_string(),
            Self::ColorEmboss => "builtin.color_emboss".to_string(),
            Self::Threshold => "builtin.threshold".to_string(),
            Self::Mosaic => "builtin.mosaic".to_string(),
            Self::MotionTile => "builtin.motion_tile".to_string(),
            Self::Mirror => "builtin.mirror".to_string(),
            Self::PolarCoordinates => "builtin.polar_coordinates".to_string(),
            Self::LensDistortion => "builtin.lens_distortion".to_string(),
            Self::Flip => "builtin.flip".to_string(),
            Self::Ripple => "builtin.ripple".to_string(),
            Self::WaveWarp => "builtin.wave_warp".to_string(),
            Self::LensFlare => "builtin.lens_flare".to_string(),
            Self::Bulge => "builtin.bulge".to_string(),
            Self::Split => "builtin.split".to_string(),
            Self::LinearColorKey => "builtin.linear_color_key".to_string(),
            Self::Extract => "builtin.extract".to_string(),
            Self::ColorOffset => "builtin.color_offset".to_string(),
            Self::Kaleidoscope => "builtin.kaleidoscope".to_string(),
            Self::Bevel => "builtin.bevel".to_string(),
            Self::Tint => "builtin.tint".to_string(),
            Self::Vibrance => "builtin.vibrance".to_string(),
            Self::Tritone => "builtin.tritone".to_string(),
            Self::RadialBlur => "builtin.radial_blur".to_string(),
            Self::Convolution => "builtin.convolution".to_string(),
            Self::Offset => "builtin.offset".to_string(),
            Self::Twirl => "builtin.twirl".to_string(),
            Self::Spherize => "builtin.spherize".to_string(),
            Self::Noise => "builtin.noise".to_string(),
            Self::Scatter => "builtin.scatter".to_string(),
            Self::BrushStrokes => "builtin.brush_strokes".to_string(),
            Self::Posterize => "builtin.posterize".to_string(),
            Self::BrightnessContrast => "builtin.brightness_contrast".to_string(),
            Self::Levels => "builtin.levels".to_string(),
            Self::Transform => "builtin.transform".to_string(),
            Self::CornerPin => "builtin.corner_pin".to_string(),
            Self::TurbulentDisplace => "builtin.turbulent_displace".to_string(),
            Self::Deband => "builtin.deband".to_string(),
            Self::Ramp => "builtin.ramp".to_string(),
            Self::Glow => "builtin.glow".to_string(),
            Self::FractalNoise => "builtin.fractal_noise".to_string(),
            Self::Keylight => "builtin.keylight".to_string(),
            Self::MatteRefine => "builtin.matte_refine".to_string(),
            Self::Echo => "builtin.echo".to_string(),
            Self::Arithmetic => "builtin.arithmetic".to_string(),
            Self::LeaveColor => "builtin.leave_color".to_string(),
            Self::BroadcastSafe => "builtin.broadcast_safe".to_string(),
            Self::HdrToneMapping => "builtin.hdr_tone_mapping".to_string(),
            Self::Choker => "builtin.choker".to_string(),
            Self::TestSignals => "builtin.test_signals".to_string(),
            Self::FourColorGradient => "builtin.four_color_gradient".to_string(),
            Self::ColorBalance => "builtin.color_balance".to_string(),
            Self::Minimax => "builtin.minimax".to_string(),
            Self::SpillSuppressor => "builtin.spill_suppressor".to_string(),
            Self::ChannelBlur => "builtin.channel_blur".to_string(),
            Self::Dent => "builtin.dent".to_string(),
            Self::ChangeToColor => "builtin.change_to_color".to_string(),
            Self::Warp => "builtin.warp".to_string(),
            Self::GridWarp => "builtin.grid_warp".to_string(),
            Self::BezierWarp => "builtin.bezier_warp".to_string(),
            Self::WireRemoval => "builtin.wire_removal".to_string(),
            Self::Median => "builtin.median".to_string(),
            Self::SelectiveColor => "builtin.selective_color".to_string(),
            Self::RoughenEdges => "builtin.roughen_edges".to_string(),
            Self::EyedropperFill => "builtin.eyedropper_fill".to_string(),
            Self::Checkerboard => "builtin.checkerboard".to_string(),
            Self::CellPattern => "builtin.cell_pattern".to_string(),
            Self::Fractal => "builtin.fractal".to_string(),
            Self::RemoveColorMatting => "builtin.remove_color_matting".to_string(),
            Self::PhotoFilter => "builtin.photo_filter".to_string(),
            Self::JpegDamage => "builtin.jpeg_damage".to_string(),
            Self::Colorama => "builtin.colorama".to_string(),
            Self::Grid => "builtin.grid".to_string(),
            Self::CircleEllipse => "builtin.circle_ellipse".to_string(),
            Self::Stroke => "builtin.stroke".to_string(),
            Self::Fill => "builtin.fill".to_string(),
            Self::Magnify => "builtin.magnify".to_string(),
            Self::DustScratches => "builtin.dust_scratches".to_string(),
            Self::VrRotateSphere => "builtin.vr_rotate_sphere".to_string(),
            Self::VrSphereToPlane => "builtin.vr_sphere_to_plane".to_string(),
            Self::VrProject2D => "builtin.vr_project_2d".to_string(),
            Self::KeyCleaner => "builtin.key_cleaner".to_string(),
            Self::ChannelCombiner => "builtin.channel_combiner".to_string(),
            Self::DisplacementMap => "builtin.displacement_map".to_string(),
            Self::GrowBounds => "builtin.grow_bounds".to_string(),
            Self::SmartBlur => "builtin.smart_blur".to_string(),
            Self::BilateralBlur => "builtin.bilateral_blur".to_string(),
            Self::ChromaticAberrationRemoval => "builtin.chromatic_aberration_removal".to_string(),
            Self::Basic3D => "builtin.basic_3d".to_string(),
            Self::CameraView => "builtin.camera_view".to_string(),
            Self::BlankingFill => "builtin.blanking_fill".to_string(),
            Self::External(key) => key.clone(),
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "builtin.basic_correction" => Self::BasicCorrection,
            "builtin.white_balance" => Self::WhiteBalance,
            "builtin.lut_3d" => Self::Lut3D,
            "builtin.color_wheel" => Self::ColorWheel,
            "builtin.hdr_grading" => Self::HdrGrading,
            "builtin.asc_cdl" => Self::AscCdl,
            "builtin.curves" => Self::Curves,
            "builtin.gamut_compression" => Self::GamutCompression,
            "builtin.highlight_recovery" => Self::HighlightRecovery,
            "builtin.qualifier" => Self::Qualifier,
            "builtin.hue_saturation_lightness" => Self::HueSaturationLightness,
            "builtin.crop" => Self::Crop,
            "builtin.gaussian_blur" => Self::GaussianBlur,
            "builtin.sharpen" => Self::Sharpen,
            "builtin.vignette" => Self::Vignette,
            "builtin.chromatic_aberration" => Self::ChromaticAberration,
            "builtin.grain" => Self::Grain,
            "builtin.chroma_key" => Self::ChromaKey,
            "builtin.luma_key" => Self::LumaKey,
            "builtin.invert" => Self::Invert,
            "builtin.box_blur" => Self::BoxBlur,
            "builtin.unsharp_mask" => Self::UnsharpMask,
            "builtin.hue_saturation" => Self::HueSaturation,
            "builtin.black_and_white" => Self::BlackAndWhite,
            "builtin.shift_channels" => Self::ShiftChannels,
            "builtin.directional_blur" => Self::DirectionalBlur,
            "builtin.channel_mixer" => Self::ChannelMixer,
            "builtin.drop_shadow" => Self::DropShadow,
            "builtin.find_edges" => Self::FindEdges,
            "builtin.emboss" => Self::Emboss,
            "builtin.color_emboss" => Self::ColorEmboss,
            "builtin.threshold" => Self::Threshold,
            "builtin.mosaic" => Self::Mosaic,
            "builtin.motion_tile" => Self::MotionTile,
            "builtin.mirror" => Self::Mirror,
            "builtin.polar_coordinates" => Self::PolarCoordinates,
            "builtin.lens_distortion" => Self::LensDistortion,
            "builtin.flip" => Self::Flip,
            "builtin.ripple" => Self::Ripple,
            "builtin.wave_warp" => Self::WaveWarp,
            "builtin.lens_flare" => Self::LensFlare,
            "builtin.bulge" => Self::Bulge,
            "builtin.split" => Self::Split,
            "builtin.linear_color_key" => Self::LinearColorKey,
            "builtin.extract" => Self::Extract,
            "builtin.color_offset" => Self::ColorOffset,
            "builtin.kaleidoscope" => Self::Kaleidoscope,
            "builtin.bevel" => Self::Bevel,
            "builtin.tint" => Self::Tint,
            "builtin.vibrance" => Self::Vibrance,
            "builtin.tritone" => Self::Tritone,
            "builtin.radial_blur" => Self::RadialBlur,
            "builtin.convolution" => Self::Convolution,
            "builtin.offset" => Self::Offset,
            "builtin.twirl" => Self::Twirl,
            "builtin.spherize" => Self::Spherize,
            "builtin.noise" => Self::Noise,
            "builtin.scatter" => Self::Scatter,
            "builtin.brush_strokes" => Self::BrushStrokes,
            "builtin.posterize" => Self::Posterize,
            "builtin.brightness_contrast" => Self::BrightnessContrast,
            "builtin.levels" => Self::Levels,
            "builtin.transform" => Self::Transform,
            "builtin.corner_pin" => Self::CornerPin,
            "builtin.turbulent_displace" => Self::TurbulentDisplace,
            "builtin.deband" => Self::Deband,
            "builtin.ramp" => Self::Ramp,
            "builtin.glow" => Self::Glow,
            "builtin.fractal_noise" => Self::FractalNoise,
            "builtin.keylight" => Self::Keylight,
            "builtin.matte_refine" => Self::MatteRefine,
            "builtin.echo" => Self::Echo,
            "builtin.arithmetic" => Self::Arithmetic,
            "builtin.leave_color" => Self::LeaveColor,
            "builtin.broadcast_safe" => Self::BroadcastSafe,
            "builtin.hdr_tone_mapping" => Self::HdrToneMapping,
            "builtin.choker" => Self::Choker,
            "builtin.test_signals" => Self::TestSignals,
            "builtin.four_color_gradient" => Self::FourColorGradient,
            "builtin.color_balance" => Self::ColorBalance,
            "builtin.minimax" => Self::Minimax,
            "builtin.spill_suppressor" => Self::SpillSuppressor,
            "builtin.channel_blur" => Self::ChannelBlur,
            "builtin.dent" => Self::Dent,
            "builtin.change_to_color" => Self::ChangeToColor,
            "builtin.warp" => Self::Warp,
            "builtin.grid_warp" => Self::GridWarp,
            "builtin.bezier_warp" => Self::BezierWarp,
            "builtin.wire_removal" => Self::WireRemoval,
            "builtin.median" => Self::Median,
            "builtin.selective_color" => Self::SelectiveColor,
            "builtin.roughen_edges" => Self::RoughenEdges,
            "builtin.eyedropper_fill" => Self::EyedropperFill,
            "builtin.checkerboard" => Self::Checkerboard,
            "builtin.cell_pattern" => Self::CellPattern,
            "builtin.fractal" => Self::Fractal,
            "builtin.remove_color_matting" => Self::RemoveColorMatting,
            "builtin.photo_filter" => Self::PhotoFilter,
            "builtin.jpeg_damage" => Self::JpegDamage,
            "builtin.colorama" => Self::Colorama,
            "builtin.grid" => Self::Grid,
            "builtin.circle_ellipse" => Self::CircleEllipse,
            "builtin.stroke" => Self::Stroke,
            "builtin.fill" => Self::Fill,
            "builtin.magnify" => Self::Magnify,
            "builtin.dust_scratches" => Self::DustScratches,
            "builtin.vr_rotate_sphere" => Self::VrRotateSphere,
            "builtin.vr_sphere_to_plane" => Self::VrSphereToPlane,
            "builtin.vr_project_2d" => Self::VrProject2D,
            "builtin.key_cleaner" => Self::KeyCleaner,
            "builtin.channel_combiner" => Self::ChannelCombiner,
            "builtin.displacement_map" => Self::DisplacementMap,
            "builtin.grow_bounds" => Self::GrowBounds,
            "builtin.smart_blur" => Self::SmartBlur,
            "builtin.bilateral_blur" => Self::BilateralBlur,
            "builtin.chromatic_aberration_removal" => Self::ChromaticAberrationRemoval,
            "builtin.basic_3d" => Self::Basic3D,
            "builtin.camera_view" => Self::CameraView,
            "builtin.blanking_fill" => Self::BlankingFill,
            other => Self::External(other.to_string()),
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::BasicCorrection => "基础调色",
            Self::WhiteBalance => "白平衡",
            Self::Lut3D => "3D LUT",
            Self::ColorWheel => "Primaries",
            Self::HdrGrading => "HDR Grading",
            Self::AscCdl => "ASC CDL",
            Self::Curves => "曲线",
            Self::GamutCompression => "色域压缩",
            Self::HighlightRecovery => "高光恢复",
            Self::Qualifier => "Qualifier",
            Self::HueSaturationLightness => "色相/饱和度/亮度",
            Self::Crop => "裁切",
            Self::GaussianBlur => "高斯模糊",
            Self::Sharpen => "锐化",
            Self::Vignette => "暗角",
            Self::ChromaticAberration => "色差",
            Self::Grain => "颗粒",
            Self::ChromaKey => "色度抠像",
            Self::LumaKey => "亮度抠像",
            Self::Invert => "反转",
            Self::BoxBlur => "方框模糊",
            Self::UnsharpMask => "钝化蒙版",
            Self::HueSaturation => "色相饱和度",
            Self::BlackAndWhite => "黑白",
            Self::ShiftChannels => "转换通道",
            Self::DirectionalBlur => "方向模糊",
            Self::ChannelMixer => "通道混合器",
            Self::DropShadow => "投影",
            Self::FindEdges => "查找边缘",
            Self::Emboss => "浮雕",
            Self::ColorEmboss => "色彩浮雕",
            Self::Threshold => "阈值",
            Self::Mosaic => "马赛克",
            Self::MotionTile => "动态拼贴",
            Self::Mirror => "镜像",
            Self::PolarCoordinates => "极坐标",
            Self::LensDistortion => "镜头畸变",
            Self::Flip => "翻转",
            Self::Ripple => "波纹",
            Self::WaveWarp => "波形变形",
            Self::LensFlare => "镜头光晕",
            Self::Bulge => "凸出",
            Self::Split => "分离",
            Self::LinearColorKey => "线性颜色键",
            Self::Extract => "提取",
            Self::ColorOffset => "颜色偏移",
            Self::Kaleidoscope => "万花筒",
            Self::Bevel => "斜面与浮雕",
            Self::Scatter => "散射",
            Self::BrushStrokes => "画笔描边",
            Self::Posterize => "色调分离",
            Self::Tint => "色调",
            Self::Vibrance => "自然饱和度",
            Self::Tritone => "三色调",
            Self::RadialBlur => "径向模糊",
            Self::Convolution => "卷积核",
            Self::Offset => "偏移",
            Self::Twirl => "旋转扭曲",
            Self::Spherize => "球面化",
            Self::Noise => "噪点",
            Self::BrightnessContrast => "亮度对比度",
            Self::Levels => "色阶",
            Self::Transform => "变换",
            Self::CornerPin => "边角定位",
            Self::TurbulentDisplace => "湍流置换",
            Self::Deband => "去色带",
            Self::Ramp => "渐变",
            Self::Glow => "发光",
            Self::FractalNoise => "分形噪波",
            Self::Keylight => "专业抠像",
            Self::MatteRefine => "遮罩精修",
            Self::Echo => "残影",
            Self::Arithmetic => "算术",
            Self::LeaveColor => "保留颜色",
            Self::BroadcastSafe => "广播安全",
            Self::HdrToneMapping => "HDR 色调映射",
            Self::Choker => "阻塞收缩",
            Self::TestSignals => "彩条与测试信号",
            Self::FourColorGradient => "四色渐变",
            Self::ColorBalance => "颜色平衡",
            Self::Minimax => "最小/最大",
            Self::SpillSuppressor => "溢色抑制",
            Self::ChannelBlur => "通道模糊",
            Self::Dent => "凹痕",
            Self::ChangeToColor => "定向改色",
            Self::Warp => "弯曲",
            Self::GridWarp => "网格变形",
            Self::BezierWarp => "贝塞尔曲线变形",
            Self::WireRemoval => "祛线",
            Self::Median => "中间值",
            Self::SelectiveColor => "可选颜色",
            Self::RoughenEdges => "粗糙边缘",
            Self::EyedropperFill => "吸管填充",
            Self::Checkerboard => "棋盘",
            Self::CellPattern => "单元格图案",
            Self::Fractal => "分形",
            Self::RemoveColorMatting => "移除颜色遮罩",
            Self::PhotoFilter => "照片滤镜",
            Self::JpegDamage => "JPEG 损坏",
            Self::Colorama => "色光",
            Self::Grid => "网格",
            Self::CircleEllipse => "圆形与椭圆",
            Self::Stroke => "描边",
            Self::Fill => "填充",
            Self::Magnify => "放大",
            Self::DustScratches => "蒙尘与划痕",
            Self::VrRotateSphere => "VR 旋转球面",
            Self::VrSphereToPlane => "VR 球面到平面",
            Self::VrProject2D => "VR 平面到球面",
            Self::KeyCleaner => "键控清理",
            Self::ChannelCombiner => "通道合成器",
            Self::DisplacementMap => "置换图",
            Self::GrowBounds => "范围扩散",
            Self::SmartBlur => "智能模糊",
            Self::BilateralBlur => "双向模糊",
            Self::ChromaticAberrationRemoval => "色差校正",
            Self::Basic3D => "基本 3D",
            Self::CameraView => "摄像机视图",
            Self::BlankingFill => "遮幅填充",
            Self::External(name) => name,
        }
    }

    pub fn category_path(&self) -> Vec<&str> {
        match self {
            Self::BasicCorrection
            | Self::WhiteBalance
            | Self::Lut3D
            | Self::ColorWheel
            | Self::HdrGrading
            | Self::HdrToneMapping
            | Self::AscCdl
            | Self::Curves
            | Self::GamutCompression
            | Self::HighlightRecovery
            | Self::HueSaturationLightness
            | Self::HueSaturation
            | Self::BlackAndWhite
            | Self::ChannelMixer => vec!["颜色", "调色"],
            Self::ShiftChannels => vec!["通道"],
            Self::Crop => vec!["变换"],
            Self::GaussianBlur
            | Self::Sharpen
            | Self::BoxBlur
            | Self::UnsharpMask
            | Self::DirectionalBlur => {
                vec!["颜色", "模糊与锐化"]
            }
            Self::Vignette | Self::ChromaticAberration | Self::Grain => vec!["颜色", "风格化"],
            Self::Qualifier | Self::ChromaKey | Self::LumaKey => vec!["抠像"],
            Self::Invert => vec!["通道"],
            Self::DropShadow => vec!["光效"],
            Self::FindEdges => vec!["风格化"],
            Self::Emboss => vec!["风格化"],
            Self::ColorEmboss => vec!["风格化"],
            Self::Threshold => vec!["风格化"],
            Self::Mosaic => vec!["风格化"],
            Self::MotionTile => vec!["风格化"],
            Self::Mirror => vec!["变换"],
            Self::PolarCoordinates => vec!["变形"],
            Self::LensDistortion => vec!["变形"],
            Self::Flip => vec!["变换与尺寸"],
            Self::Ripple => vec!["变形"],
            Self::WaveWarp => vec!["变形"],
            Self::LensFlare => vec!["光效"],
            Self::Bulge => vec!["变形"],
            Self::Split => vec!["变形"],
            Self::LinearColorKey => vec!["抠像"],
            Self::Extract => vec!["抠像"],
            Self::ColorOffset => vec!["颜色", "调色"],
            Self::Kaleidoscope => vec!["风格化"],
            Self::Bevel => vec!["边缘效果"],
            Self::Scatter => vec!["风格化"],
            Self::BrushStrokes => vec!["风格化"],
            Self::Posterize => vec!["风格化"],
            Self::Tint => vec!["颜色/调色"],
            Self::Vibrance => vec!["颜色/调色"],
            Self::Tritone => vec!["颜色/调色"],
            Self::RadialBlur => vec!["颜色", "模糊与锐化"],
            Self::Convolution => vec!["颜色", "调色"],
            Self::Offset => vec!["变换与尺寸"],
            Self::Twirl => vec!["变形"],
            Self::Spherize => vec!["变形"],
            Self::Noise => vec!["胶片质感"],
            Self::BrightnessContrast => vec!["颜色", "调色"],
            Self::Levels => vec!["颜色", "调色"],
            Self::Transform => vec!["变换"],
            Self::CornerPin => vec!["变换"],
            Self::TurbulentDisplace => vec!["变形"],
            Self::Deband => vec!["修复"],
            Self::Ramp => vec!["颜色"],
            Self::Glow => vec!["光"],
            Self::FractalNoise => vec!["颜色"],
            Self::Keylight => vec!["抠像"],
            Self::MatteRefine => vec!["抠像"],
            Self::Echo => vec!["时间"],
            Self::Arithmetic => vec!["通道"],
            Self::LeaveColor => vec!["颜色"],
            Self::BroadcastSafe => vec!["颜色"],
            Self::Choker => vec!["抠像"],
            Self::TestSignals => vec!["生成器"],
            Self::FourColorGradient => vec!["生成器"],
            Self::ColorBalance => vec!["颜色", "调色"],
            Self::Minimax => vec!["通道"],
            Self::SpillSuppressor => vec!["抠像"],
            Self::ChannelBlur => vec!["颜色", "模糊与锐化"],
            Self::Dent => vec!["变形"],
            Self::ChangeToColor => vec!["颜色", "调色"],
            Self::Warp => vec!["变形"],
            Self::GridWarp => vec!["变形"],
            Self::BezierWarp => vec!["变形"],
            Self::WireRemoval => vec!["修复"],
            Self::Median => vec!["颜色", "模糊与锐化"],
            Self::SelectiveColor => vec!["颜色", "调色"],
            Self::RoughenEdges => vec!["风格化"],
            Self::EyedropperFill => vec!["生成器"],
            Self::Checkerboard => vec!["生成器"],
            Self::CellPattern => vec!["生成器"],
            Self::Fractal => vec!["生成器"],
            Self::RemoveColorMatting => vec!["通道"],
            Self::PhotoFilter => vec!["颜色"],
            Self::JpegDamage => vec!["胶片质感"],
            Self::Colorama => vec!["颜色", "调色"],
            Self::Grid => vec!["生成器"],
            Self::CircleEllipse => vec!["生成器"],
            Self::Stroke => vec!["生成器"],
            Self::Fill => vec!["生成器"],
            Self::Magnify => vec!["变形"],
            Self::DustScratches => vec!["修复"],
            Self::VrRotateSphere => vec!["变形"],
            Self::VrSphereToPlane => vec!["变形"],
            Self::VrProject2D => vec!["变形"],
            Self::KeyCleaner => vec!["边缘修补"],
            Self::ChannelCombiner => vec!["通道"],
            Self::DisplacementMap => vec!["变形"],
            Self::GrowBounds => vec!["通道"],
            Self::SmartBlur | Self::BilateralBlur => vec!["模糊与锐化"],
            Self::ChromaticAberrationRemoval => vec!["修复"],
            Self::Basic3D => vec!["变换"],
            Self::CameraView => vec!["变换"],
            Self::BlankingFill => vec!["变换"],
            Self::External(_) => vec!["外部效果"],
        }
    }

    pub fn property_namespace(&self) -> String {
        match self {
            Self::BasicCorrection => "basic_correction".to_string(),
            Self::WhiteBalance => "white_balance".to_string(),
            Self::Lut3D => "lut_3d".to_string(),
            Self::ColorWheel => "color_wheel".to_string(),
            Self::HdrGrading => "hdr_grading".to_string(),
            Self::AscCdl => "asc_cdl".to_string(),
            Self::Curves => "curves".to_string(),
            Self::GamutCompression => "gamut_compression".to_string(),
            Self::HighlightRecovery => "highlight_recovery".to_string(),
            Self::Qualifier => "qualifier".to_string(),
            Self::HueSaturationLightness => "hue_saturation_lightness".to_string(),
            Self::Crop => "crop".to_string(),
            Self::GaussianBlur => "gaussian_blur".to_string(),
            Self::Sharpen => "sharpen".to_string(),
            Self::Vignette => "vignette".to_string(),
            Self::ChromaticAberration => "chromatic_aberration".to_string(),
            Self::Grain => "grain".to_string(),
            Self::ChromaKey => "chroma_key".to_string(),
            Self::LumaKey => "luma_key".to_string(),
            Self::Invert => "invert".to_string(),
            Self::BoxBlur => "box_blur".to_string(),
            Self::UnsharpMask => "unsharp_mask".to_string(),
            Self::HueSaturation => "hue_saturation".to_string(),
            Self::BlackAndWhite => "black_and_white".to_string(),
            Self::ShiftChannels => "shift_channels".to_string(),
            Self::DirectionalBlur => "directional_blur".to_string(),
            Self::ChannelMixer => "channel_mixer".to_string(),
            Self::DropShadow => "drop_shadow".to_string(),
            Self::FindEdges => "find_edges".to_string(),
            Self::Emboss => "emboss".to_string(),
            Self::ColorEmboss => "color_emboss".to_string(),
            Self::Threshold => "threshold".to_string(),
            Self::Mosaic => "mosaic".to_string(),
            Self::MotionTile => "motion_tile".to_string(),
            Self::Mirror => "mirror".to_string(),
            Self::PolarCoordinates => "polar_coordinates".to_string(),
            Self::LensDistortion => "lens_distortion".to_string(),
            Self::Flip => "flip".to_string(),
            Self::Ripple => "ripple".to_string(),
            Self::WaveWarp => "wave_warp".to_string(),
            Self::LensFlare => "lens_flare".to_string(),
            Self::Bulge => "bulge".to_string(),
            Self::Split => "split".to_string(),
            Self::LinearColorKey => "linear_color_key".to_string(),
            Self::Extract => "extract".to_string(),
            Self::ColorOffset => "color_offset".to_string(),
            Self::Kaleidoscope => "kaleidoscope".to_string(),
            Self::Bevel => "bevel".to_string(),
            Self::Scatter => "scatter".to_string(),
            Self::BrushStrokes => "brush_strokes".to_string(),
            Self::Posterize => "posterize".to_string(),
            Self::Tint => "tint".to_string(),
            Self::Vibrance => "vibrance".to_string(),
            Self::Tritone => "tritone".to_string(),
            Self::RadialBlur => "radial_blur".to_string(),
            Self::Convolution => "convolution".to_string(),
            Self::Offset => "offset".to_string(),
            Self::Twirl => "twirl".to_string(),
            Self::Spherize => "spherize".to_string(),
            Self::Noise => "noise".to_string(),
            Self::BrightnessContrast => "brightness_contrast".to_string(),
            Self::Levels => "levels".to_string(),
            Self::Transform => "transform".to_string(),
            Self::CornerPin => "corner_pin".to_string(),
            Self::TurbulentDisplace => "turbulent_displace".to_string(),
            Self::Deband => "deband".to_string(),
            Self::Ramp => "ramp".to_string(),
            Self::Glow => "glow".to_string(),
            Self::FractalNoise => "fractal_noise".to_string(),
            Self::Keylight => "keylight".to_string(),
            Self::MatteRefine => "matte_refine".to_string(),
            Self::Echo => "echo".to_string(),
            Self::Arithmetic => "arithmetic".to_string(),
            Self::LeaveColor => "leave_color".to_string(),
            Self::BroadcastSafe => "broadcast_safe".to_string(),
            Self::HdrToneMapping => "hdr_tone_mapping".to_string(),
            Self::Choker => "choker".to_string(),
            Self::TestSignals => "test_signals".to_string(),
            Self::FourColorGradient => "four_color_gradient".to_string(),
            Self::ColorBalance => "color_balance".to_string(),
            Self::Minimax => "minimax".to_string(),
            Self::SpillSuppressor => "spill_suppressor".to_string(),
            Self::ChannelBlur => "channel_blur".to_string(),
            Self::Dent => "dent".to_string(),
            Self::ChangeToColor => "change_to_color".to_string(),
            Self::Warp => "warp".to_string(),
            Self::GridWarp => "grid_warp".to_string(),
            Self::BezierWarp => "bezier_warp".to_string(),
            Self::WireRemoval => "wire_removal".to_string(),
            Self::Median => "median".to_string(),
            Self::SelectiveColor => "selective_color".to_string(),
            Self::RoughenEdges => "roughen_edges".to_string(),
            Self::EyedropperFill => "eyedropper_fill".to_string(),
            Self::Checkerboard => "checkerboard".to_string(),
            Self::CellPattern => "cell_pattern".to_string(),
            Self::Fractal => "fractal".to_string(),
            Self::RemoveColorMatting => "remove_color_matting".to_string(),
            Self::PhotoFilter => "photo_filter".to_string(),
            Self::JpegDamage => "jpeg_damage".to_string(),
            Self::Colorama => "colorama".to_string(),
            Self::Grid => "grid".to_string(),
            Self::CircleEllipse => "circle_ellipse".to_string(),
            Self::Stroke => "stroke".to_string(),
            Self::Fill => "fill".to_string(),
            Self::Magnify => "magnify".to_string(),
            Self::DustScratches => "dust_scratches".to_string(),
            Self::VrRotateSphere => "vr_rotate_sphere".to_string(),
            Self::VrSphereToPlane => "vr_sphere_to_plane".to_string(),
            Self::VrProject2D => "vr_project_2d".to_string(),
            Self::KeyCleaner => "key_cleaner".to_string(),
            Self::ChannelCombiner => "channel_combiner".to_string(),
            Self::DisplacementMap => "displacement_map".to_string(),
            Self::GrowBounds => "grow_bounds".to_string(),
            Self::SmartBlur => "smart_blur".to_string(),
            Self::BilateralBlur => "bilateral_blur".to_string(),
            Self::ChromaticAberrationRemoval => "chromatic_aberration_removal".to_string(),
            Self::Basic3D => "basic_3d".to_string(),
            Self::CameraView => "camera_view".to_string(),
            Self::BlankingFill => "blanking_fill".to_string(),
            Self::External(key) => key.clone(),
        }
    }

    pub fn property_path(&self, parameter: &str) -> String {
        format!("effect.{}.{}", self.property_namespace(), parameter)
    }

    /// Build the stable parameter identity owned by this effect definition.
    pub fn parameter_id(&self, parameter: &str) -> Result<ParameterId, ParameterIdError> {
        ParameterId::new(format!("mondrian.effect.{}.{}", self.key(), parameter))
    }
}

/// A single effect instance on a clip.
///
/// The struct holds identity (id, type), animatable properties, and opaque
/// JSON params. Effect evaluation is performed by `mondrian-effects`, not by
/// methods on this struct.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EffectNode {
    pub id: EffectId,
    pub effect_type: EffectType,
    pub properties: PropertyBag,
    pub params: serde_json::Value,
    pub is_enabled: bool,
}

impl<'de> Deserialize<'de> for EffectNode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename = "EffectNode")]
        struct StoredEffectNode {
            #[serde(default)]
            id: EffectId,
            effect_type: EffectType,
            #[serde(default)]
            properties: PropertyBag,
            #[serde(default)]
            params: serde_json::Value,
            is_enabled: bool,
        }

        let StoredEffectNode { id, effect_type, properties, params, is_enabled } =
            StoredEffectNode::deserialize(deserializer)?;
        let mut effect = Self { id, effect_type, properties, params, is_enabled };
        let added_property = match effect.effect_type {
            EffectType::ChannelCombiner => Some((
                Self::channel_combiner_second_layer_source_descriptor(),
                "source_layer",
            )),
            EffectType::ChannelMixer => {
                Some((Self::channel_mixer_inverse_descriptor(), "monochrome"))
            }
            EffectType::Glow => Some((Self::glow_based_on_descriptor(), "threshold")),
            _ => None,
        };
        if let Some((mut descriptor, anchor)) = added_property {
            let legacy_path = effect.effect_type.property_path(anchor);
            let namespaced_legacy = namespaced_effect_path(id, &legacy_path);
            if effect.properties.property(&namespaced_legacy).is_some() {
                descriptor.path = namespaced_effect_path(id, &descriptor.path);
            }
            if (effect.properties.property(&legacy_path).is_some()
                || effect.properties.property(&namespaced_legacy).is_some())
                && !effect.properties.iter().any(|(_, property)| {
                    property.descriptor.parameter_id() == descriptor.parameter_id()
                })
            {
                effect.define_property(descriptor);
            }
        }
        Ok(effect)
    }
}

impl EffectNode {
    /// Glow input channel shared by the definition and legacy project loading.
    pub fn glow_based_on_descriptor() -> PropertyDescriptor {
        let effect_type = EffectType::Glow;
        let parameter = "based_on";
        let mut descriptor = PropertyDescriptor::new(
            effect_type.property_path(parameter),
            "发光基于",
            PropertyValue::Enum("color".to_owned()),
        )
        .with_parameter_id(effect_type.parameter_id(parameter).expect("built-in parameter id"))
        .with_enum_options(vec![
            ParameterEnumOption::new("color", "颜色通道", "mondrian.effect.glow.based_on.color"),
            ParameterEnumOption::new("alpha", "Alpha 通道", "mondrian.effect.glow.based_on.alpha"),
        ])
        .with_animatable(false);
        descriptor.ui_metadata.group_name = Some("发光".to_owned());
        descriptor.ui_metadata.display_order = Some(12);
        descriptor
    }

    /// Inverse affine transform control shared by the definition and project loading.
    pub fn channel_mixer_inverse_descriptor() -> PropertyDescriptor {
        let effect_type = EffectType::ChannelMixer;
        let mut descriptor = PropertyDescriptor::new(
            effect_type.property_path("inverse"),
            "反向",
            PropertyValue::Bool(false),
        )
        .with_parameter_id(effect_type.parameter_id("inverse").expect("built-in parameter id"));
        descriptor.ui_metadata.group_name = Some("通道混合器".to_owned());
        descriptor.ui_metadata.display_order = Some(13);
        descriptor
    }

    /// Static source field shared by the definition and legacy Channel Combiner loading.
    pub fn channel_combiner_second_layer_source_descriptor() -> PropertyDescriptor {
        let effect_type = EffectType::ChannelCombiner;
        let parameter = "second_layer_source";
        let mut descriptor = PropertyDescriptor::new(
            effect_type.property_path(parameter),
            "源图层",
            PropertyValue::VisualSource(crate::VisualSourceReference::Unbound),
        )
        .with_parameter_id(effect_type.parameter_id(parameter).expect("built-in parameter id"));
        descriptor.ui_metadata.group_name = Some("第二图层".to_owned());
        descriptor.ui_metadata.display_order = Some(6);
        descriptor
    }

    /// Create a new effect node with an empty property bag.
    ///
    /// Product authoring must use the fallible `instantiate_effect_node()`
    /// boundary from `mondrian-effects`; this empty constructor is for
    /// definition/runtime assembly where registry availability is handled by
    /// the caller.
    pub fn new(effect_type: EffectType) -> Self {
        Self {
            id: EffectId::new(),
            properties: PropertyBag::default(),
            effect_type,
            params: serde_json::json!({}),
            is_enabled: true,
        }
    }

    /// Validate one effect instance's parameter schemas and stable identities.
    pub fn validate_author_state(&self) -> crate::Result<()> {
        self.properties.validate()?;
        let mut parameter_ids = HashSet::new();
        for (_, property) in self.properties.iter() {
            let parameter_id = property.descriptor.parameter_id();
            if !parameter_ids.insert(parameter_id.clone()) {
                return Err(crate::MondrianError::WorkflowStepFailed {
                    step_id: "effect_parameter_identity_validation".to_owned(),
                    reason: format!(
                        "Effect {} contains duplicate parameter identity {}",
                        self.id, parameter_id
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn evaluate_property(&self, path: &str, time: TimelineTime) -> Option<PropertyValue> {
        self.properties.evaluate(path, time)
    }

    /// Evaluate one definition-stable parameter independent of instance address.
    pub fn evaluate_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<PropertyValue> {
        self.unique_parameter_property(parameter_id)
            .map(|(_, property)| property.evaluate(time))
    }

    pub fn define_property(&mut self, descriptor: PropertyDescriptor) {
        self.properties.define(descriptor);
    }

    /// Evaluate one floating parameter by stable schema identity.
    pub fn evaluate_f32_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
        fallback: f32,
    ) -> f32 {
        self.evaluate_parameter(parameter_id, time)
            .and_then(|value| value.as_f32())
            .unwrap_or(fallback)
    }

    /// Evaluate one three-component parameter by stable schema identity.
    pub fn evaluate_vec3_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
        fallback: glam::Vec3,
    ) -> glam::Vec3 {
        self.evaluate_parameter(parameter_id, time)
            .and_then(|value| value.as_vec3())
            .unwrap_or(fallback)
    }

    /// Evaluate one structured normalized curve by stable schema identity.
    pub fn evaluate_curve_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<crate::automation::NormalizedCurve> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Curve(curve) => Some(curve),
            _ => None,
        })
    }

    /// Evaluate a structured three-dimensional qualifier sample set by stable schema identity.
    pub fn evaluate_qualifier_samples_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<crate::automation::QualifierSampleSet> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::QualifierSamples(samples) => Some(samples),
            _ => None,
        })
    }

    /// Namespace all property paths with an effect instance prefix.
    ///
    /// Definition-owned Inspector groups are preserved. `fallback_group_name`
    /// is assigned only to older properties that do not define a group.
    /// Called when an effect is placed on a clip to avoid path collisions.
    ///
    /// Not idempotent: calling twice double-prefixes property paths.
    /// Callers must ensure this is called exactly once per clip placement.
    pub fn instantiate_for_clip(&mut self, fallback_group_name: String) {
        let mut namespaced = PropertyBag::default();
        for (_, property) in self.properties.iter() {
            let mut property = property.clone();
            property.descriptor.path = namespaced_effect_path(self.id, &property.descriptor.path);
            property
                .descriptor
                .ui_metadata
                .group_name
                .get_or_insert_with(|| fallback_group_name.clone());
            namespaced.upsert(property);
        }
        self.properties = namespaced;
    }

    /// Evaluate one non-empty text parameter by stable schema identity.
    pub fn evaluate_text_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<String> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Text(text) if !text.trim().is_empty() => Some(text),
            _ => None,
        })
    }

    /// Evaluate one stable enum key by definition-stable parameter identity.
    pub fn evaluate_enum_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<String> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Enum(key) => Some(key),
            _ => None,
        })
    }

    /// Evaluate a typed resource reference by stable schema identity.
    pub fn evaluate_resource_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<ParameterResourceReference> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Resource(reference) => Some(reference),
            _ => None,
        })
    }

    /// Read a static visual source by stable parameter identity.
    ///
    /// Source selection and its offset cannot be animated. Validate the property
    /// before reading so corrupt persisted channels cannot masquerade as Unbound.
    pub fn evaluate_visual_source_parameter(
        &self,
        parameter_id: &ParameterId,
    ) -> Option<crate::VisualSourceReference> {
        let (_, property) = self.unique_parameter_property(parameter_id)?;
        property.validate().ok()?;
        match property.static_value() {
            PropertyValue::VisualSource(reference) => Some(*reference),
            _ => None,
        }
    }

    /// Mutate one property selected by definition-stable parameter identity.
    pub fn set_static_value_by_parameter(
        &mut self,
        parameter_id: &ParameterId,
        value: PropertyValue,
    ) -> crate::Result<()> {
        let path = self
            .unique_parameter_property(parameter_id)
            .map(|(path, _)| path.to_string())
            .ok_or_else(|| crate::MondrianError::WorkflowStepFailed {
            step_id: "effect_set_static_value".to_string(),
            reason: format!("effect parameter does not exist: {parameter_id}"),
        })?;
        self.properties.set_static_value(&path, value)
    }

    fn unique_parameter_property(
        &self,
        parameter_id: &ParameterId,
    ) -> Option<(&str, &crate::automation::AnimatedProperty)> {
        let mut matches = self
            .properties
            .iter()
            .filter(|(_, property)| property.descriptor.parameter_id() == parameter_id);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }
}

impl crate::automation::PropertyHost for EffectNode {
    fn property_bag(&self) -> crate::Result<PropertyBag> {
        Ok(self.properties.clone())
    }

    fn apply_property_mutation(
        &mut self,
        mutation: crate::automation::PropertyMutation,
    ) -> crate::Result<()> {
        self.properties.apply_mutation(mutation)
    }
}

/// Build a namespaced property path scoped to a single effect instance.
pub fn namespaced_effect_path(effect_id: EffectId, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("effect.") {
        format!("effect.{effect_id}.{rest}")
    } else {
        format!("effect.{effect_id}.{path}")
    }
}

/// Parse an [`EffectId`] from a namespaced property path.
///
/// Effect property paths have the form `effect.<UUID>.<rest>`. This function
/// extracts the UUID segment and parses it into an `EffectId`.
///
/// Returns `None` if the path does not start with `effect.` or the second
/// segment is not a valid UUID.
pub fn parse_effect_id_from_property_path(path: &str) -> Option<EffectId> {
    let mut segments = path.split('.');
    if segments.next()? != "effect" {
        return None;
    }
    let id_raw = segments.next()?;
    uuid::Uuid::parse_str(id_raw).ok().map(EffectId)
}

impl crate::AuthoringFootprint for EffectType {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::External(key) => collector.collect(key),
            Self::BasicCorrection
            | Self::WhiteBalance
            | Self::Lut3D
            | Self::ColorWheel
            | Self::HdrGrading
            | Self::HdrToneMapping
            | Self::AscCdl
            | Self::Curves
            | Self::GamutCompression
            | Self::HighlightRecovery
            | Self::Qualifier
            | Self::HueSaturationLightness
            | Self::Crop
            | Self::GaussianBlur
            | Self::Sharpen
            | Self::Vignette
            | Self::ChromaticAberration
            | Self::Grain
            | Self::ChromaKey
            | Self::LumaKey
            | Self::Invert
            | Self::BoxBlur
            | Self::UnsharpMask
            | Self::HueSaturation
            | Self::BlackAndWhite => Ok(()),
            Self::ShiftChannels => Ok(()),
            Self::DirectionalBlur => Ok(()),
            Self::ChannelMixer => Ok(()),
            Self::DropShadow => Ok(()),
            Self::FindEdges => Ok(()),
            Self::Emboss => Ok(()),
            Self::ColorEmboss => Ok(()),
            Self::Threshold | Self::Mosaic | Self::MotionTile | Self::Kaleidoscope => Ok(()),
            Self::Mirror => Ok(()),
            Self::PolarCoordinates => Ok(()),
            Self::LensDistortion | Self::Flip | Self::Ripple | Self::WaveWarp => Ok(()),
            Self::LensFlare => Ok(()),
            Self::Bulge => Ok(()),
            Self::Split => Ok(()),
            Self::LinearColorKey | Self::Extract => Ok(()),
            Self::ColorOffset => Ok(()),
            Self::Posterize | Self::Bevel | Self::Scatter | Self::BrushStrokes => Ok(()),
            Self::Tint | Self::Vibrance => Ok(()),
            Self::Tritone => Ok(()),
            Self::RadialBlur => Ok(()),
            Self::Convolution => Ok(()),
            Self::Offset => Ok(()),
            Self::Twirl => Ok(()),
            Self::Noise => Ok(()),
            Self::Spherize => Ok(()),
            Self::BrightnessContrast => Ok(()),
            Self::Levels => Ok(()),
            Self::Transform => Ok(()),
            Self::CornerPin => Ok(()),
            Self::TurbulentDisplace => Ok(()),
            Self::Deband => Ok(()),
            Self::Ramp => Ok(()),
            Self::Glow => Ok(()),
            Self::FractalNoise => Ok(()),
            Self::Keylight => Ok(()),
            Self::MatteRefine => Ok(()),
            Self::Echo => Ok(()),
            Self::Arithmetic => Ok(()),
            Self::LeaveColor => Ok(()),
            Self::BroadcastSafe => Ok(()),
            Self::Choker => Ok(()),
            Self::TestSignals => Ok(()),
            Self::FourColorGradient => Ok(()),
            Self::ColorBalance => Ok(()),
            Self::Minimax => Ok(()),
            Self::SpillSuppressor => Ok(()),
            Self::ChannelBlur => Ok(()),
            Self::Dent => Ok(()),
            Self::ChangeToColor => Ok(()),
            Self::Warp => Ok(()),
            Self::GridWarp => Ok(()),
            Self::BezierWarp => Ok(()),
            Self::WireRemoval => Ok(()),
            Self::Median => Ok(()),
            Self::SelectiveColor => Ok(()),
            Self::RoughenEdges => Ok(()),
            Self::EyedropperFill => Ok(()),
            Self::Checkerboard => Ok(()),
            Self::CellPattern => Ok(()),
            Self::Fractal => Ok(()),
            Self::RemoveColorMatting => Ok(()),
            Self::PhotoFilter => Ok(()),
            Self::JpegDamage => Ok(()),
            Self::Colorama => Ok(()),
            Self::Grid => Ok(()),
            Self::CircleEllipse => Ok(()),
            Self::Stroke => Ok(()),
            Self::Fill => Ok(()),
            Self::Magnify => Ok(()),
            Self::DustScratches => Ok(()),
            Self::VrRotateSphere => Ok(()),
            Self::VrSphereToPlane => Ok(()),
            Self::VrProject2D => Ok(()),
            // Alpha-only cleanup: it can only remove coverage, never invent a
            // painted region, so it has no authoring footprint.
            Self::KeyCleaner => Ok(()),
            Self::ChannelCombiner => Ok(()),
            Self::DisplacementMap => Ok(()),
            Self::GrowBounds => Ok(()),
            // Range-domain blur: it samples and reweights existing pixels, so it
            // can never paint a region the source left empty.
            Self::SmartBlur
            | Self::BilateralBlur
            | Self::ChromaticAberrationRemoval
            | Self::Basic3D => Ok(()),
            Self::CameraView | Self::BlankingFill => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for EffectNode {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            id: _,
            effect_type,
            properties,
            params,
            is_enabled: _,
        } = self;
        collector.collect(effect_type)?;
        collector.collect(properties)?;
        collector.collect(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_effect_preserves_project_wire_and_definition_identity() {
        let wire = serde_json::json!({ "Plugin": "plugin.saved.effect" });
        let effect_type: EffectType = serde_json::from_value(wire.clone()).expect("read effect");
        assert_eq!(
            effect_type,
            EffectType::External("plugin.saved.effect".to_owned())
        );
        assert_eq!(effect_type.key(), "plugin.saved.effect");
        assert_eq!(
            serde_json::to_value(&effect_type).expect("write effect"),
            wire
        );
    }

    #[test]
    fn parse_effect_id_from_property_path_extracts_valid_uuid() {
        let id = EffectId::new();
        let path = format!("effect.{}.some.parameter", id);
        let parsed = parse_effect_id_from_property_path(&path);
        assert_eq!(parsed, Some(id));
    }

    #[test]
    fn parse_effect_id_from_property_path_rejects_non_effect_prefix() {
        assert_eq!(
            parse_effect_id_from_property_path("transform.position"),
            None
        );
    }

    #[test]
    fn parse_effect_id_from_property_path_rejects_invalid_uuid() {
        assert_eq!(
            parse_effect_id_from_property_path("effect.not-a-uuid.param"),
            None
        );
    }

    #[test]
    fn parse_effect_id_from_property_path_rejects_too_few_segments() {
        assert_eq!(parse_effect_id_from_property_path("effect"), None);
        assert_eq!(
            parse_effect_id_from_property_path("effect.a1b2c3d4e5f6"),
            None
        );
    }

    #[test]
    fn crop_effect_author_state_round_trips_without_legacy_translation() {
        let mut effect = EffectNode::new(EffectType::Crop);
        effect.define_property(
            PropertyDescriptor::new(
                EffectType::Crop.property_path("left"),
                "左侧",
                PropertyValue::Float(12.5),
            )
            .with_parameter_id(
                EffectType::Crop.parameter_id("left").expect("Crop parameter identity"),
            ),
        );

        let json = serde_json::to_string(&effect).expect("serialize Crop author state");
        let reopened: EffectNode =
            serde_json::from_str(&json).expect("deserialize Crop author state");

        assert_eq!(reopened, effect);
        assert_eq!(reopened.effect_type.key(), "builtin.crop");
    }

    #[test]
    fn clip_instantiation_preserves_definition_owned_property_groups() {
        let mut effect = EffectNode::new(EffectType::HdrGrading);
        let mut grouped = PropertyDescriptor::new(
            EffectType::HdrGrading.property_path("global_exposure"),
            "Exposure",
            PropertyValue::Float(0.0),
        );
        grouped.ui_metadata.group_name = Some("HDR · Global".to_owned());
        effect.define_property(grouped);
        effect.define_property(PropertyDescriptor::new(
            EffectType::HdrGrading.property_path("legacy"),
            "Legacy",
            PropertyValue::Float(0.0),
        ));

        effect.instantiate_for_clip("HDR Grading".to_owned());

        let groups = effect
            .properties
            .iter()
            .map(|(_, property)| property.descriptor.ui_metadata.group_name.as_deref())
            .collect::<Vec<_>>();
        assert!(groups.contains(&Some("HDR · Global")));
        assert!(groups.contains(&Some("HDR Grading")));
    }
}
