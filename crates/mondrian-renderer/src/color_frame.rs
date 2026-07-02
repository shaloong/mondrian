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

impl ColorFrameDescriptor {
    /// Number of pixels described by this frame.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Return this descriptor with a different memory residency.
    pub fn with_residency(mut self, residency: ColorFrameResidency) -> Self {
        self.residency = residency;
        self
    }
}

/// Renderer-owned identifier for a GPU color frame resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuColorFrameId(u64);

impl GpuColorFrameId {
    /// Create an identifier from a renderer resource table key.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Return the raw renderer resource table key.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Texture format used by a GPU-resident color frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuColorFrameTextureFormat {
    /// 8-bit normalized RGBA texture.
    Rgba8Unorm,
    /// 16-bit floating-point RGBA texture.
    Rgba16Float,
    /// 32-bit floating-point RGBA texture.
    Rgba32Float,
}

/// GPU-resident color frame handle.
///
/// This is a typed renderer resource handle, not a CPU pixel container. Native
/// backends own the actual texture and use the id to resolve it from their
/// resource tables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GpuColorFrameHandle {
    id: GpuColorFrameId,
    descriptor: ColorFrameDescriptor,
    texture_format: GpuColorFrameTextureFormat,
    label: String,
}

impl GpuColorFrameHandle {
    /// Create a GPU frame handle with a validated descriptor.
    pub fn new(
        id: GpuColorFrameId,
        descriptor: ColorFrameDescriptor,
        texture_format: GpuColorFrameTextureFormat,
        label: impl Into<String>,
    ) -> Result<Self, GpuColorFrameHandleError> {
        if descriptor.residency != ColorFrameResidency::Gpu {
            return Err(GpuColorFrameHandleError::CpuResidentDescriptor);
        }
        if descriptor.width == 0 || descriptor.height == 0 {
            return Err(GpuColorFrameHandleError::EmptyExtent {
                width: descriptor.width,
                height: descriptor.height,
            });
        }

        Ok(Self {
            id,
            descriptor,
            texture_format,
            label: label.into(),
        })
    }

    /// Return the renderer resource id.
    pub fn id(&self) -> GpuColorFrameId {
        self.id
    }

    /// Return the frame metadata contract.
    pub fn descriptor(&self) -> ColorFrameDescriptor {
        self.descriptor
    }

    /// Return the backend texture format.
    pub fn texture_format(&self) -> GpuColorFrameTextureFormat {
        self.texture_format
    }

    /// Human-readable resource label for diagnostics/profiling.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Error returned when constructing a GPU color frame handle.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GpuColorFrameHandleError {
    /// The descriptor does not describe a GPU-resident frame.
    #[error("GPU color frame handle requires a GPU-resident descriptor")]
    CpuResidentDescriptor,
    /// The descriptor has an empty pixel extent.
    #[error("GPU color frame handle requires a non-empty extent, got {width}x{height}")]
    EmptyExtent {
        /// Descriptor width.
        width: u32,
        /// Descriptor height.
        height: u32,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_color_frame_handle_requires_gpu_residency() {
        let descriptor = ColorFrameDescriptor {
            width: 1920,
            height: 1080,
            color_space: ColorSpace::Rec709,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };

        let err = GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(7),
            descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
            "working-frame",
        )
        .expect_err("CPU descriptor must be rejected");

        assert_eq!(err, GpuColorFrameHandleError::CpuResidentDescriptor);
    }

    #[test]
    fn gpu_color_frame_handle_carries_descriptor_and_resource_id() {
        let descriptor = ColorFrameDescriptor {
            width: 3840,
            height: 2160,
            color_space: ColorSpace::Rec2020,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };

        let handle = GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(42),
            descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
            "timeline-working",
        )
        .expect("GPU descriptor");

        assert_eq!(handle.id().raw(), 42);
        assert_eq!(handle.descriptor(), descriptor);
        assert_eq!(
            handle.texture_format(),
            GpuColorFrameTextureFormat::Rgba16Float
        );
        assert_eq!(handle.label(), "timeline-working");
        assert_eq!(handle.descriptor().pixel_count(), 3840 * 2160);
    }
}
