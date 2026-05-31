//! Shader 源码存放模块（通过 include_str! 引用 .wgsl 文件）

pub const YUV_TO_RGB: &str = include_str!("../shaders/yuv_to_rgb.wgsl");
pub const COMPOSITE: &str = include_str!("../shaders/composite.wgsl");
pub const LUT3D: &str = include_str!("../shaders/lut3d.wgsl");
pub const GAUSSIAN_BLUR: &str = include_str!("../shaders/blur_gaussian.wgsl");

// Compute shaders (Phase 3: GPU effects)
pub const LUT3D_COMPUTE: &str = include_str!("../shaders/lut3d_compute.wgsl");
pub const BLUR_GAUSSIAN_COMPUTE: &str = include_str!("../shaders/blur_gaussian_compute.wgsl");
pub const COLOR_ADJUST_COMPUTE: &str = include_str!("../shaders/color_adjust_compute.wgsl");
