use mondrian_core::{types::ColorSpace, RgbaF32Frame};
use std::collections::HashMap;

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

    /// Return the descriptor/texture-format contract for this frame handle.
    pub fn contract(&self) -> GpuColorFrameContract {
        GpuColorFrameContract {
            descriptor: self.descriptor,
            texture_format: self.texture_format,
        }
    }

    /// Human-readable resource label for diagnostics/profiling.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Descriptor and texture-format contract for a GPU color frame resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuColorFrameContract {
    /// Frame metadata contract.
    pub descriptor: ColorFrameDescriptor,
    /// Backend texture format.
    pub texture_format: GpuColorFrameTextureFormat,
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

/// Resource-table entry for a GPU color frame handle and backend-owned payload.
#[derive(Debug)]
pub struct GpuColorFrameResource<R> {
    handle: GpuColorFrameHandle,
    resource: R,
}

impl<R> GpuColorFrameResource<R> {
    /// Create a resource-table entry for a typed GPU frame handle.
    pub fn new(handle: GpuColorFrameHandle, resource: R) -> Self {
        Self { handle, resource }
    }

    /// Return the typed GPU frame handle.
    pub fn handle(&self) -> &GpuColorFrameHandle {
        &self.handle
    }

    /// Borrow the backend resource payload.
    pub fn resource(&self) -> &R {
        &self.resource
    }

    /// Mutably borrow the backend resource payload.
    pub fn resource_mut(&mut self) -> &mut R {
        &mut self.resource
    }

    /// Consume this entry and return the handle and backend resource.
    pub fn into_parts(self) -> (GpuColorFrameHandle, R) {
        (self.handle, self.resource)
    }
}

/// Shared resource table for GPU-resident color frames.
///
/// The table is generic over the backend resource payload so contract behavior
/// can be tested without constructing real wgpu objects. Production renderer
/// paths use `GpuColorFrameWgpuResource` as the payload.
pub struct GpuColorFrameResourceTable<R> {
    entries: HashMap<GpuColorFrameId, GpuColorFrameResource<R>>,
}

impl<R> GpuColorFrameResourceTable<R> {
    /// Create an empty GPU color frame resource table.
    pub fn new() -> Self {
        Self { entries: HashMap::new() }
    }

    /// Insert a resource entry, replacing only an existing entry with the same contract.
    pub fn insert(
        &mut self,
        entry: GpuColorFrameResource<R>,
    ) -> Result<Option<GpuColorFrameResource<R>>, GpuColorFrameResourceTableError> {
        let id = entry.handle.id();
        if let Some(existing) = self.entries.get(&id) {
            validate_frame_contract(id, &entry.handle, &existing.handle)?;
        }
        Ok(self.entries.insert(id, entry))
    }

    /// Resolve a resource entry for the requested frame handle.
    pub fn get(
        &self,
        handle: &GpuColorFrameHandle,
    ) -> Result<&GpuColorFrameResource<R>, GpuColorFrameResourceTableError> {
        let entry = self
            .entries
            .get(&handle.id())
            .ok_or(GpuColorFrameResourceTableError::MissingFrame { id: handle.id() })?;
        validate_frame_contract(handle.id(), handle, &entry.handle)?;
        Ok(entry)
    }

    /// Mutably resolve a resource entry for the requested frame handle.
    pub fn get_mut(
        &mut self,
        handle: &GpuColorFrameHandle,
    ) -> Result<&mut GpuColorFrameResource<R>, GpuColorFrameResourceTableError> {
        let entry = self
            .entries
            .get_mut(&handle.id())
            .ok_or(GpuColorFrameResourceTableError::MissingFrame { id: handle.id() })?;
        validate_frame_contract(handle.id(), handle, &entry.handle)?;
        Ok(entry)
    }

    /// Remove a resource entry by frame id.
    pub fn remove(&mut self, id: GpuColorFrameId) -> Option<GpuColorFrameResource<R>> {
        self.entries.remove(&id)
    }

    /// Return the number of entries in the table.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the table has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<R> Default for GpuColorFrameResourceTable<R> {
    fn default() -> Self {
        Self::new()
    }
}

/// Concrete wgpu payload for a GPU color frame resource table entry.
pub struct GpuColorFrameWgpuResource {
    /// Backend texture owned by the renderer resource table.
    pub texture: wgpu::Texture,
    /// Default view used by color pass sampling or rendering.
    pub texture_view: wgpu::TextureView,
    /// Default sampler used when this frame is sampled by a fullscreen pass.
    pub sampler: wgpu::Sampler,
}

/// Error returned when resolving GPU color frame resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuColorFrameResourceTableError {
    /// No resource exists for the requested frame id.
    MissingFrame {
        /// Missing frame id.
        id: GpuColorFrameId,
    },
    /// A resource id exists but its descriptor or texture format no longer matches.
    ContractMismatch {
        /// Resource id that mismatched.
        id: GpuColorFrameId,
        /// Contract expected by the caller.
        expected: GpuColorFrameContract,
        /// Contract stored in the resource table.
        actual: GpuColorFrameContract,
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

fn validate_frame_contract(
    id: GpuColorFrameId,
    expected: &GpuColorFrameHandle,
    actual: &GpuColorFrameHandle,
) -> Result<(), GpuColorFrameResourceTableError> {
    if expected.contract() != actual.contract() {
        return Err(GpuColorFrameResourceTableError::ContractMismatch {
            id,
            expected: expected.contract(),
            actual: actual.contract(),
        });
    }
    Ok(())
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

    #[test]
    fn gpu_color_frame_resource_table_resolves_matching_contract() {
        let handle = gpu_handle(
            100,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let mut table = GpuColorFrameResourceTable::new();

        let previous = table
            .insert(GpuColorFrameResource::new(handle.clone(), "texture-a"))
            .expect("insert resource");

        assert!(previous.is_none());
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get(&handle).expect("resource").resource(),
            &"texture-a"
        );
    }

    #[test]
    fn gpu_color_frame_resource_table_rejects_missing_frame() {
        let handle = gpu_handle(
            101,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let table = GpuColorFrameResourceTable::<()>::new();

        let err = table.get(&handle).expect_err("missing frame must fail");

        assert_eq!(
            err,
            GpuColorFrameResourceTableError::MissingFrame { id: handle.id() }
        );
    }

    #[test]
    fn gpu_color_frame_resource_table_rejects_stale_descriptor_for_same_id() {
        let handle = gpu_handle(
            102,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let mut stale_descriptor = working_descriptor();
        stale_descriptor.color_space = ColorSpace::Rec2020;
        let stale_handle = gpu_handle(
            102,
            stale_descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(
                stale_handle.clone(),
                "stale-texture",
            ))
            .expect("insert stale resource");

        let err = table.get(&handle).expect_err("stale descriptor under same id must fail");

        assert_eq!(
            err,
            GpuColorFrameResourceTableError::ContractMismatch {
                id: handle.id(),
                expected: handle.contract(),
                actual: stale_handle.contract()
            }
        );
    }

    #[test]
    fn gpu_color_frame_resource_table_replaces_same_contract_only() {
        let handle = gpu_handle(
            103,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let wrong_format = gpu_handle(
            103,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba32Float,
        );
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(handle.clone(), "first"))
            .expect("first insert");

        let replaced = table
            .insert(GpuColorFrameResource::new(handle.clone(), "second"))
            .expect("same contract can replace");
        assert_eq!(replaced.expect("previous entry").resource(), &"first");
        assert_eq!(
            table.get(&handle).expect("second entry").resource(),
            &"second"
        );

        let err = table
            .insert(GpuColorFrameResource::new(
                wrong_format.clone(),
                "wrong-format",
            ))
            .expect_err("same id with different texture format must fail");
        assert_eq!(
            err,
            GpuColorFrameResourceTableError::ContractMismatch {
                id: handle.id(),
                expected: wrong_format.contract(),
                actual: handle.contract()
            }
        );
    }

    fn working_descriptor() -> ColorFrameDescriptor {
        ColorFrameDescriptor {
            width: 1920,
            height: 1080,
            color_space: ColorSpace::Rec709,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        }
    }

    fn gpu_handle(
        id: u64,
        descriptor: ColorFrameDescriptor,
        texture_format: GpuColorFrameTextureFormat,
    ) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(id),
            descriptor,
            texture_format,
            "test-frame",
        )
        .expect("GPU frame handle")
    }
}
