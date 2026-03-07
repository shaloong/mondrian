//! Shader 源码存放模块（通过 include_str! 引用 .wgsl 文件）

pub const YUV_TO_RGB: &str = include_str!("../shaders/yuv_to_rgb.wgsl");
pub const COMPOSITE: &str = include_str!("../shaders/composite.wgsl");
pub const LUT3D: &str = include_str!("../shaders/lut3d.wgsl");
pub const GAUSSIAN_BLUR: &str = include_str!("../shaders/blur_gaussian.wgsl");
