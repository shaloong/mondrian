use mondrian_core::{types::ColorSpace, RgbaF32Frame};

/// Semantic role of a frame in the color-managed render graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFrameDomain {
    /// Decoded source pixels before timeline working-space conversion.
    Source,
    /// Timeline working-space pixels after input transforms and compositing.
    Working,
    /// Presentation pixels after a display/view transform.
    Display,
    /// Delivery pixels after export/output transforms.
    Export,
}

/// Pixel encoding carried by a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFrameEncoding {
    /// Linear-light floating-point RGBA.
    LinearFloat,
    /// Non-linear, destination-encoded RGBA bytes.
    EncodedRgba8,
}

/// Memory residency for a render-graph frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFrameResidency {
    /// Pixels are resident in CPU memory.
    Cpu,
    /// Pixels are resident in GPU memory and represented by a renderer handle.
    Gpu,
}

/// Metadata that makes a frame's color contract explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorFrameDescriptor {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Color space currently represented by the pixels.
    pub color_space: ColorSpace,
    /// Frame role in the render graph.
    pub domain: ColorFrameDomain,
    /// Pixel encoding.
    pub encoding: ColorFrameEncoding,
    /// CPU/GPU residency.
    pub residency: ColorFrameResidency,
}

/// CPU-resident linear floating-point frame with a typed color contract.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuColorFrame {
    descriptor: ColorFrameDescriptor,
    frame: RgbaF32Frame,
}

impl CpuColorFrame {
    /// Wrap a linear-light frame as a working-space render-graph frame.
    pub fn working(frame: RgbaF32Frame) -> Self {
        let descriptor = ColorFrameDescriptor {
            width: frame.width,
            height: frame.height,
            color_space: frame.color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };
        Self { descriptor, frame }
    }

    /// Return the frame metadata contract.
    pub fn descriptor(&self) -> ColorFrameDescriptor {
        self.descriptor
    }

    /// Borrow the underlying linear-light frame.
    pub fn rgba_f32(&self) -> &RgbaF32Frame {
        &self.frame
    }

    /// Consume this wrapper and return the underlying linear-light frame.
    pub fn into_rgba_f32(self) -> RgbaF32Frame {
        self.frame
    }

    /// Encode this frame to RGBA8 for a specific output color space.
    pub(crate) fn to_output_rgba8(&self, output: ColorSpace, tone_map: bool) -> Vec<u8> {
        self.frame.to_rgba8(output, tone_map)
    }
}

/// CPU-resident encoded RGBA8 frame at an explicit graph boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuEncodedColorFrame {
    descriptor: ColorFrameDescriptor,
    rgba: Vec<u8>,
}

impl CpuEncodedColorFrame {
    /// Create a CPU RGBA8 boundary frame.
    pub fn rgba8(
        width: u32,
        height: u32,
        color_space: ColorSpace,
        domain: ColorFrameDomain,
        rgba: Vec<u8>,
    ) -> Self {
        let descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space,
            domain,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Cpu,
        };
        Self { descriptor, rgba }
    }

    /// Create a source/import RGBA8 boundary frame.
    pub fn source_rgba8(width: u32, height: u32, color_space: ColorSpace, rgba: Vec<u8>) -> Self {
        Self::rgba8(width, height, color_space, ColorFrameDomain::Source, rgba)
    }

    /// Return frame width.
    pub fn width(&self) -> u32 {
        self.descriptor.width
    }

    /// Return frame height.
    pub fn height(&self) -> u32 {
        self.descriptor.height
    }

    /// Return the frame metadata contract.
    pub fn descriptor(&self) -> ColorFrameDescriptor {
        self.descriptor
    }

    /// Borrow RGBA8 pixels.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Consume this wrapper and return RGBA8 pixels.
    pub fn into_rgba(self) -> Vec<u8> {
        self.rgba
    }
}
