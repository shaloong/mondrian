use mondrian_core::{types::ColorSpace, RgbaF32Frame};
use mondrian_media::DecodedGpuFrameHandleKind;
use std::collections::HashMap;
use std::sync::Arc;

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

/// Monotonic allocator for renderer-owned GPU color frame ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuColorFrameIdAllocator {
    next: u64,
}

impl GpuColorFrameIdAllocator {
    /// Create an allocator starting at the provided raw id.
    pub fn new(first: u64) -> Self {
        Self { next: first }
    }

    /// Allocate the next frame id.
    pub fn allocate(&mut self) -> GpuColorFrameId {
        let id = GpuColorFrameId::from_raw(self.next);
        self.next = self.next.saturating_add(1);
        id
    }

    /// Return the next raw id that will be allocated.
    pub fn next_raw(&self) -> u64 {
        self.next
    }
}

impl Default for GpuColorFrameIdAllocator {
    fn default() -> Self {
        Self::new(1)
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

impl GpuColorFrameTextureFormat {
    /// Return the byte stride for one pixel in this texture format.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8Unorm => 4,
            Self::Rgba16Float => 8,
            Self::Rgba32Float => 16,
        }
    }

    /// Return the wgpu texture format represented by this renderer format.
    pub fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
            Self::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
        }
    }
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
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
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

    /// Remove every resource entry from the table.
    pub fn clear(&mut self) {
        self.entries.clear();
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

/// GPU texture allocation plan for a color frame resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuColorFrameAllocationPlan {
    /// GPU frame handle produced by this allocation.
    pub handle: GpuColorFrameHandle,
    /// Texture format to create.
    pub texture_format: GpuColorFrameTextureFormat,
    /// Texture extent.
    pub extent: wgpu::Extent3d,
    /// Texture usages required by color upload, sampling, rendering, and readback.
    pub usage: wgpu::TextureUsages,
}

impl GpuColorFrameAllocationPlan {
    /// Build an allocation plan for a validated GPU frame handle.
    pub fn for_handle(handle: GpuColorFrameHandle) -> Self {
        let descriptor = handle.descriptor();
        Self {
            texture_format: handle.texture_format(),
            extent: wgpu::Extent3d {
                width: descriptor.width,
                height: descriptor.height,
                depth_or_array_layers: 1,
            },
            usage: default_color_frame_texture_usage(),
            handle,
        }
    }
}

/// CPU-to-GPU upload plan for one color frame resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuColorFrameUploadPlan {
    /// GPU frame handle produced by this upload.
    pub handle: GpuColorFrameHandle,
    /// Texture format to create.
    pub texture_format: GpuColorFrameTextureFormat,
    /// Texture extent.
    pub extent: wgpu::Extent3d,
    /// Bytes per texture row.
    pub bytes_per_row: u32,
    /// Rows per uploaded image.
    pub rows_per_image: u32,
    /// Packed upload bytes.
    pub bytes: Vec<u8>,
}

impl GpuColorFrameUploadPlan {
    /// Build an upload plan for a CPU linear floating-point frame.
    pub fn from_cpu_color_frame(
        id: GpuColorFrameId,
        frame: &CpuColorFrame,
        texture_format: GpuColorFrameTextureFormat,
        label: impl Into<String>,
    ) -> Result<Self, GpuColorFrameUploadError> {
        if texture_format != GpuColorFrameTextureFormat::Rgba32Float {
            return Err(GpuColorFrameUploadError::UnsupportedCpuFloatTextureFormat {
                texture_format,
            });
        }
        let descriptor = frame.descriptor().with_residency(ColorFrameResidency::Gpu);
        validate_cpu_pixel_count(descriptor, frame.rgba_f32().data.len())?;
        let handle = GpuColorFrameHandle::new(id, descriptor, texture_format, label)
            .map_err(GpuColorFrameUploadError::Handle)?;
        let bytes = bytemuck::cast_slice(&frame.rgba_f32().data).to_vec();
        Self::new(handle, bytes)
    }

    /// Build an upload plan for a CPU encoded RGBA8 boundary frame.
    pub fn from_cpu_encoded_frame(
        id: GpuColorFrameId,
        frame: &CpuEncodedColorFrame,
        texture_format: GpuColorFrameTextureFormat,
        label: impl Into<String>,
    ) -> Result<Self, GpuColorFrameUploadError> {
        if texture_format != GpuColorFrameTextureFormat::Rgba8Unorm {
            return Err(
                GpuColorFrameUploadError::UnsupportedCpuEncodedTextureFormat { texture_format },
            );
        }
        let descriptor = frame.descriptor().with_residency(ColorFrameResidency::Gpu);
        validate_cpu_byte_count(descriptor, frame.rgba().len())?;
        let handle = GpuColorFrameHandle::new(id, descriptor, texture_format, label)
            .map_err(GpuColorFrameUploadError::Handle)?;
        Self::new(handle, frame.rgba().to_vec())
    }

    fn new(handle: GpuColorFrameHandle, bytes: Vec<u8>) -> Result<Self, GpuColorFrameUploadError> {
        let descriptor = handle.descriptor();
        let texture_format = handle.texture_format();
        let bytes_per_row = descriptor
            .width
            .checked_mul(texture_format.bytes_per_pixel())
            .ok_or(GpuColorFrameUploadError::UploadLayoutOverflow)?;
        let expected_len = bytes_per_row as usize * descriptor.height as usize;
        if bytes.len() != expected_len {
            return Err(GpuColorFrameUploadError::ByteLengthMismatch {
                expected: expected_len,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            handle,
            texture_format,
            extent: wgpu::Extent3d {
                width: descriptor.width,
                height: descriptor.height,
                depth_or_array_layers: 1,
            },
            bytes_per_row,
            rows_per_image: descriptor.height,
            bytes,
        })
    }
}

/// Uploads validated color frame upload plans into wgpu resources.
pub struct GpuColorFrameUploader;

impl GpuColorFrameUploader {
    /// Allocate an empty renderer-owned wgpu color frame resource.
    pub fn allocate(
        device: &wgpu::Device,
        plan: &GpuColorFrameAllocationPlan,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        let texture = create_color_frame_texture(
            device,
            plan.handle.label(),
            plan.extent,
            plan.texture_format,
            plan.usage,
        );
        let (texture_view, sampler) = create_color_frame_view_and_sampler(device, &texture);
        GpuColorFrameResource::new(
            plan.handle.clone(),
            GpuColorFrameWgpuResource { texture, texture_view, sampler },
        )
    }

    /// Upload a validated color frame plan into a renderer-owned wgpu resource.
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GpuColorFrameUploadPlan,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        let allocation = GpuColorFrameAllocationPlan::for_handle(plan.handle.clone());
        let texture = create_color_frame_texture(
            device,
            plan.handle.label(),
            plan.extent,
            plan.texture_format,
            allocation.usage,
        );
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &plan.bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(plan.bytes_per_row),
                rows_per_image: Some(plan.rows_per_image),
            },
            plan.extent,
        );
        let (texture_view, sampler) = create_color_frame_view_and_sampler(device, &texture);
        GpuColorFrameResource::new(
            plan.handle.clone(),
            GpuColorFrameWgpuResource { texture, texture_view, sampler },
        )
    }
}

/// Error returned when a CPU color frame cannot be packed for GPU upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuColorFrameUploadError {
    /// The requested texture format cannot represent a CPU linear float frame yet.
    UnsupportedCpuFloatTextureFormat {
        /// Requested texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
    /// The requested texture format cannot represent a CPU encoded RGBA8 frame.
    UnsupportedCpuEncodedTextureFormat {
        /// Requested texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
    /// The GPU frame handle could not be created.
    Handle(GpuColorFrameHandleError),
    /// The CPU frame pixel count does not match its descriptor.
    PixelCountMismatch {
        /// Expected pixel count.
        expected: usize,
        /// Actual pixel count.
        actual: usize,
    },
    /// The CPU RGBA8 byte count does not match its descriptor.
    ByteCountMismatch {
        /// Expected byte count.
        expected: usize,
        /// Actual byte count.
        actual: usize,
    },
    /// Row-stride calculation overflowed.
    UploadLayoutOverflow,
    /// Packed upload bytes do not match the texture extent/format.
    ByteLengthMismatch {
        /// Expected upload byte length.
        expected: usize,
        /// Actual upload byte length.
        actual: usize,
    },
}

/// Source texture layout produced by a native hardware decoder.
///
/// This is the imported decoder-surface format, not the working-frame texture
/// format that Mondrian composites after input color conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuNativeDecodedFrameTextureFormat {
    /// 8-bit NV12 two-plane YCbCr surface.
    Nv12,
    /// 10/12-bit P010 two-plane YCbCr surface.
    P010,
    /// Single-plane 8-bit normalized RGBA surface.
    Rgba8Unorm,
    /// Single-plane 8-bit normalized BGRA surface.
    Bgra8Unorm,
}

impl GpuNativeDecodedFrameTextureFormat {
    /// Stable texture-format name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nv12 => "Nv12",
            Self::P010 => "P010",
            Self::Rgba8Unorm => "Rgba8Unorm",
            Self::Bgra8Unorm => "Bgra8Unorm",
        }
    }
}

/// Renderer backend capability contract for importing native decoded frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuNativeDecodedFrameImportSupport {
    /// Whether the concrete renderer backend has connected native import code.
    pub renderer_backend_ready: bool,
    /// Decoder handle families accepted by the backend.
    pub supported_handle_kinds: Vec<DecodedGpuFrameHandleKind>,
    /// Decoder source texture formats accepted by the backend.
    pub supported_source_texture_formats: Vec<GpuNativeDecodedFrameTextureFormat>,
}

impl GpuNativeDecodedFrameImportSupport {
    /// Build a fail-closed support value for builds without native import.
    pub fn unavailable() -> Self {
        Self {
            renderer_backend_ready: false,
            supported_handle_kinds: Vec::new(),
            supported_source_texture_formats: Vec::new(),
        }
    }

    /// Build an explicit support value for a renderer backend implementation.
    pub fn ready(
        supported_handle_kinds: Vec<DecodedGpuFrameHandleKind>,
        supported_source_texture_formats: Vec<GpuNativeDecodedFrameTextureFormat>,
    ) -> Self {
        Self {
            renderer_backend_ready: true,
            supported_handle_kinds,
            supported_source_texture_formats,
        }
    }

    /// Whether the backend reports support for a decoder handle family.
    pub fn supports_handle_kind(&self, handle_kind: DecodedGpuFrameHandleKind) -> bool {
        self.supported_handle_kinds.contains(&handle_kind)
    }

    /// Whether the backend reports support for a decoded source texture format.
    pub fn supports_source_texture_format(
        &self,
        texture_format: GpuNativeDecodedFrameTextureFormat,
    ) -> bool {
        self.supported_source_texture_formats.contains(&texture_format)
    }
}

impl Default for GpuNativeDecodedFrameImportSupport {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// Request to import a hardware-decoded native frame into the renderer graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuNativeDecodedFrameImportContract {
    /// Source frame width in pixels.
    pub width: u32,
    /// Source frame height in pixels.
    pub height: u32,
    /// Color space represented by the decoded source surface.
    pub source_color_space: ColorSpace,
    /// Timeline working color space to produce after input conversion.
    pub working_color_space: ColorSpace,
    /// Decoder handle family.
    pub handle_kind: DecodedGpuFrameHandleKind,
    /// Decoder source texture layout.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
    /// Renderer-owned working texture format to produce.
    pub working_texture_format: GpuColorFrameTextureFormat,
    /// Human-readable label for diagnostics/profiling.
    pub label: String,
}

/// Renderer-owned plan for importing native decoded frames.
///
/// The imported decoder surface is not represented as a `GpuColorFrameHandle`
/// because it may be multi-plane YCbCr. The handle in this plan is the
/// renderer-owned linear working frame produced after native surface sampling
/// and the OCIO input transform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuNativeDecodedFrameImportPlan {
    /// Decoder handle family consumed by the backend.
    pub handle_kind: DecodedGpuFrameHandleKind,
    /// Decoder source texture layout consumed by the backend.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
    /// Source color space represented by the decoder surface.
    pub source_color_space: ColorSpace,
    /// Renderer-owned output working frame.
    pub working_frame: GpuColorFrameHandle,
}

impl GpuNativeDecodedFrameImportPlan {
    /// Build a native decoded-frame import plan from a validated backend
    /// support contract.
    pub fn from_contract(
        ids: &mut GpuColorFrameIdAllocator,
        contract: GpuNativeDecodedFrameImportContract,
        support: &GpuNativeDecodedFrameImportSupport,
    ) -> Result<Self, GpuNativeDecodedFrameImportPlanError> {
        if contract.width == 0 || contract.height == 0 {
            return Err(GpuNativeDecodedFrameImportPlanError::EmptyExtent {
                width: contract.width,
                height: contract.height,
            });
        }
        if !support.renderer_backend_ready {
            return Err(GpuNativeDecodedFrameImportPlanError::RendererBackendUnavailable);
        }
        if !support.supports_handle_kind(contract.handle_kind) {
            return Err(
                GpuNativeDecodedFrameImportPlanError::UnsupportedHandleKind {
                    handle_kind: contract.handle_kind,
                },
            );
        }
        if !support.supports_source_texture_format(contract.source_texture_format) {
            return Err(
                GpuNativeDecodedFrameImportPlanError::UnsupportedSourceTextureFormat {
                    source_texture_format: contract.source_texture_format,
                },
            );
        }
        if !matches!(
            contract.working_texture_format,
            GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float
        ) {
            return Err(
                GpuNativeDecodedFrameImportPlanError::UnsupportedWorkingTextureFormat {
                    working_texture_format: contract.working_texture_format,
                },
            );
        }

        let working_descriptor = ColorFrameDescriptor {
            width: contract.width,
            height: contract.height,
            color_space: contract.working_color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let working_frame = GpuColorFrameHandle::new(
            ids.allocate(),
            working_descriptor,
            contract.working_texture_format,
            contract.label,
        )
        .map_err(GpuNativeDecodedFrameImportPlanError::WorkingFrameHandle)?;

        Ok(Self {
            handle_kind: contract.handle_kind,
            source_texture_format: contract.source_texture_format,
            source_color_space: contract.source_color_space,
            working_frame,
        })
    }
}

/// Error returned when native decoded-frame import cannot be planned.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeDecodedFrameImportPlanError {
    /// The source frame extent is empty.
    #[error("native decoded frame import requires a non-empty extent, got {width}x{height}")]
    EmptyExtent {
        /// Source width.
        width: u32,
        /// Source height.
        height: u32,
    },
    /// No concrete renderer backend has connected native import code.
    #[error("renderer backend does not support native decoded frame import")]
    RendererBackendUnavailable,
    /// The decoder handle family is not supported by the renderer backend.
    #[error("unsupported native decoded frame handle kind {handle_kind:?}")]
    UnsupportedHandleKind {
        /// Unsupported handle family.
        handle_kind: DecodedGpuFrameHandleKind,
    },
    /// The decoder source texture format is not supported by the renderer backend.
    #[error("unsupported native decoded frame source texture format {source_texture_format:?}")]
    UnsupportedSourceTextureFormat {
        /// Unsupported decoder source texture format.
        source_texture_format: GpuNativeDecodedFrameTextureFormat,
    },
    /// The requested working texture format cannot carry linear working pixels.
    #[error("unsupported native decoded frame working texture format {working_texture_format:?}")]
    UnsupportedWorkingTextureFormat {
        /// Unsupported working texture format.
        working_texture_format: GpuColorFrameTextureFormat,
    },
    /// The renderer-owned working frame handle could not be built.
    #[error("failed to create native decoded frame working handle: {0}")]
    WorkingFrameHandle(GpuColorFrameHandleError),
}

/// GPU-to-CPU readback plan for one encoded color frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuColorFrameReadbackPlan {
    /// GPU frame handle read by this plan.
    pub handle: GpuColorFrameHandle,
    /// CPU descriptor produced after unpacking.
    pub output_descriptor: ColorFrameDescriptor,
    /// Texture format copied from the GPU resource.
    pub texture_format: GpuColorFrameTextureFormat,
    /// Texture copy extent.
    pub extent: wgpu::Extent3d,
    /// Unpadded bytes in one logical image row.
    pub unpadded_bytes_per_row: u32,
    /// wgpu-aligned bytes in one copied buffer row.
    pub padded_bytes_per_row: u32,
    /// Readback buffer size in bytes.
    pub buffer_size: u64,
}

impl GpuColorFrameReadbackPlan {
    /// Build a readback plan for an encoded RGBA8 GPU frame.
    pub fn encoded_rgba8(handle: GpuColorFrameHandle) -> Result<Self, GpuColorFrameReadbackError> {
        if handle.texture_format() != GpuColorFrameTextureFormat::Rgba8Unorm {
            return Err(GpuColorFrameReadbackError::UnsupportedTextureFormat {
                texture_format: handle.texture_format(),
            });
        }
        let descriptor = handle.descriptor();
        if descriptor.encoding != ColorFrameEncoding::EncodedRgba8 {
            return Err(GpuColorFrameReadbackError::UnsupportedEncoding {
                encoding: descriptor.encoding,
            });
        }
        let unpadded_bytes_per_row = descriptor
            .width
            .checked_mul(handle.texture_format().bytes_per_pixel())
            .ok_or(GpuColorFrameReadbackError::ReadbackLayoutOverflow)?;
        let padded_bytes_per_row = align_copy_bytes_per_row(unpadded_bytes_per_row)?;
        let buffer_size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(descriptor.height))
            .ok_or(GpuColorFrameReadbackError::ReadbackLayoutOverflow)?;
        Ok(Self {
            handle,
            output_descriptor: descriptor.with_residency(ColorFrameResidency::Cpu),
            texture_format: GpuColorFrameTextureFormat::Rgba8Unorm,
            extent: wgpu::Extent3d {
                width: descriptor.width,
                height: descriptor.height,
                depth_or_array_layers: 1,
            },
            unpadded_bytes_per_row,
            padded_bytes_per_row,
            buffer_size,
        })
    }

    /// Create a readback plan for a `Rgba16Float` GPU texture.
    ///
    /// This is the precision-preserving alternative to [`Self::encoded_rgba8`]
    /// for export paths that need higher-than-8-bit precision.
    pub fn encoded_rgba16float(
        handle: GpuColorFrameHandle,
    ) -> Result<Self, GpuColorFrameReadbackError> {
        if handle.texture_format() != GpuColorFrameTextureFormat::Rgba16Float {
            return Err(GpuColorFrameReadbackError::UnsupportedTextureFormat {
                texture_format: handle.texture_format(),
            });
        }
        let descriptor = handle.descriptor();
        let unpadded_bytes_per_row = descriptor
            .width
            .checked_mul(handle.texture_format().bytes_per_pixel())
            .ok_or(GpuColorFrameReadbackError::ReadbackLayoutOverflow)?;
        let padded_bytes_per_row = align_copy_bytes_per_row(unpadded_bytes_per_row)?;
        let buffer_size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(descriptor.height))
            .ok_or(GpuColorFrameReadbackError::ReadbackLayoutOverflow)?;
        Ok(Self {
            handle,
            output_descriptor: descriptor.with_residency(ColorFrameResidency::Cpu),
            texture_format: GpuColorFrameTextureFormat::Rgba16Float,
            extent: wgpu::Extent3d {
                width: descriptor.width,
                height: descriptor.height,
                depth_or_array_layers: 1,
            },
            unpadded_bytes_per_row,
            padded_bytes_per_row,
            buffer_size,
        })
    }

    /// Unpack a padded mapped readback buffer into a typed CPU encoded frame.
    pub fn unpack_mapped_rgba8(
        &self,
        mapped: &[u8],
    ) -> Result<CpuEncodedColorFrame, GpuColorFrameReadbackError> {
        let expected = self.buffer_size as usize;
        if mapped.len() < expected {
            return Err(GpuColorFrameReadbackError::MappedBufferTooSmall {
                expected,
                actual: mapped.len(),
            });
        }
        let mut rgba = vec![0; self.unpadded_bytes_per_row as usize * self.extent.height as usize];
        for row in 0..self.extent.height as usize {
            let src_start = row * self.padded_bytes_per_row as usize;
            let src_end = src_start + self.unpadded_bytes_per_row as usize;
            let dst_start = row * self.unpadded_bytes_per_row as usize;
            let dst_end = dst_start + self.unpadded_bytes_per_row as usize;
            rgba[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
        }
        Ok(CpuEncodedColorFrame::rgba8(
            self.output_descriptor.width,
            self.output_descriptor.height,
            self.output_descriptor.color_space,
            self.output_descriptor.domain,
            rgba,
        ))
    }

    /// Unpack a padded mapped readback buffer from an `Rgba16Float` texture
    /// into linear f32 RGBA pixels.
    ///
    /// Returns the pixel data as `Vec<f32>` (4 floats per pixel, little-endian
    /// half-float unpacked to f32). The caller can use this for higher-bit-depth
    /// export paths without RGBA8 quantization.
    pub fn unpack_mapped_rgba16float(
        &self,
        mapped: &[u8],
    ) -> Result<Vec<f32>, GpuColorFrameReadbackError> {
        if self.texture_format != GpuColorFrameTextureFormat::Rgba16Float {
            return Err(GpuColorFrameReadbackError::UnsupportedTextureFormat {
                texture_format: self.texture_format,
            });
        }
        let expected = self.buffer_size as usize;
        if mapped.len() < expected {
            return Err(GpuColorFrameReadbackError::MappedBufferTooSmall {
                expected,
                actual: mapped.len(),
            });
        }
        let pixel_count = self.extent.width as usize * self.extent.height as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for row in 0..self.extent.height as usize {
            let src_start = row * self.padded_bytes_per_row as usize;
            for px in 0..self.extent.width as usize {
                let offset = src_start + px * 8; // 8 bytes per pixel (4 × f16)
                if offset + 8 > mapped.len() {
                    break;
                }
                // Unpack f16 to f32.
                for c in 0..4 {
                    let half_bits =
                        u16::from_le_bytes([mapped[offset + c * 2], mapped[offset + c * 2 + 1]]);
                    data.push(f16_to_f32(half_bits));
                }
            }
        }
        Ok(data)
    }
}

/// Records and completes GPU color frame readback copies.
pub struct GpuColorFrameReadback;

impl GpuColorFrameReadback {
    /// Create a MAP_READ buffer and record a texture-to-buffer copy into the encoder.
    pub fn record_copy(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        plan: &GpuColorFrameReadbackPlan,
        resource: &GpuColorFrameWgpuResource,
    ) -> wgpu::Buffer {
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_color_frame_readback"),
            size: plan.buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resource.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(plan.padded_bytes_per_row),
                    rows_per_image: Some(plan.extent.height),
                },
            },
            plan.extent,
        );
        readback
    }
}

/// Error returned when a GPU color frame cannot be read back as a CPU frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuColorFrameReadbackError {
    /// Only RGBA8 readback is currently defined for encoded CPU boundaries.
    UnsupportedTextureFormat {
        /// Actual GPU texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
    /// Only encoded RGBA8 descriptors can use this readback path.
    UnsupportedEncoding {
        /// Actual GPU frame encoding.
        encoding: ColorFrameEncoding,
    },
    /// Row layout calculation overflowed.
    ReadbackLayoutOverflow,
    /// The mapped readback buffer is smaller than the plan requires.
    MappedBufferTooSmall {
        /// Expected mapped byte length.
        expected: usize,
        /// Actual mapped byte length.
        actual: usize,
    },
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
        Self::linear(frame, ColorFrameDomain::Working)
    }

    /// Wrap a linear-light frame with an explicit render-graph domain.
    pub fn linear(frame: RgbaF32Frame, domain: ColorFrameDomain) -> Self {
        let descriptor = ColorFrameDescriptor {
            width: frame.width,
            height: frame.height,
            color_space: frame.color_space,
            domain,
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
    rgba: Arc<Vec<u8>>,
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
        Self::rgba8_shared(width, height, color_space, domain, Arc::new(rgba))
    }

    /// Create a CPU RGBA8 boundary frame from shared immutable pixels.
    pub fn rgba8_shared(
        width: u32,
        height: u32,
        color_space: ColorSpace,
        domain: ColorFrameDomain,
        rgba: Arc<Vec<u8>>,
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

    /// Create a source/import RGBA8 boundary frame from shared immutable pixels.
    pub fn source_rgba8_shared(
        width: u32,
        height: u32,
        color_space: ColorSpace,
        rgba: Arc<Vec<u8>>,
    ) -> Self {
        Self::rgba8_shared(width, height, color_space, ColorFrameDomain::Source, rgba)
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
        self.rgba.as_slice()
    }

    /// Consume this wrapper and return RGBA8 pixels.
    pub fn into_rgba(self) -> Vec<u8> {
        Arc::try_unwrap(self.rgba).unwrap_or_else(|rgba| rgba.as_ref().clone())
    }
}

/// A CPU-resident linear-light f32 source frame.
///
/// This is the precision-preserving alternative to [`CpuEncodedColorFrame`] for
/// sources that are already in linear float (e.g., synthetic test data, float
/// decode output, or effect graph intermediates). It bypasses the RGBA8
/// quantization path entirely.
///
/// The frame stores RGBA f32 pixels (4 floats per pixel) in linear light,
/// matching the `CpuColorFrame` working-space contract but at the source
/// domain boundary.
#[derive(Debug, Clone)]
pub struct LinearFloatSource {
    descriptor: ColorFrameDescriptor,
    data: Vec<f32>,
}

impl LinearFloatSource {
    /// Create a linear float source frame.
    pub fn new(width: u32, height: u32, color_space: ColorSpace, data: Vec<f32>) -> Self {
        assert_eq!(
            data.len(),
            width as usize * height as usize * 4,
            "LinearFloatSource data length must be width * height * 4"
        );
        let descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space,
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };
        Self { descriptor, data }
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

    /// Borrow RGBA f32 pixels.
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Consume this wrapper and return RGBA f32 pixels.
    pub fn into_data(self) -> Vec<f32> {
        self.data
    }

    /// Convert to a working-space [`CpuColorFrame`] without any u8
    /// quantization. The caller must ensure the data is already in the target
    /// working color space.
    pub fn to_working_frame(self, working_color_space: ColorSpace) -> CpuColorFrame {
        let pixels: Vec<[f32; 4]> =
            self.data.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let frame = RgbaF32Frame {
            width: self.descriptor.width,
            height: self.descriptor.height,
            data: pixels,
            color_space: working_color_space,
        };
        CpuColorFrame::working(frame)
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

fn validate_cpu_pixel_count(
    descriptor: ColorFrameDescriptor,
    actual: usize,
) -> Result<(), GpuColorFrameUploadError> {
    let expected = descriptor.pixel_count();
    if actual != expected {
        return Err(GpuColorFrameUploadError::PixelCountMismatch { expected, actual });
    }
    Ok(())
}

fn validate_cpu_byte_count(
    descriptor: ColorFrameDescriptor,
    actual: usize,
) -> Result<(), GpuColorFrameUploadError> {
    let expected = descriptor.pixel_count() * 4;
    if actual != expected {
        return Err(GpuColorFrameUploadError::ByteCountMismatch { expected, actual });
    }
    Ok(())
}

fn align_copy_bytes_per_row(bytes_per_row: u32) -> Result<u32, GpuColorFrameReadbackError> {
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes_per_row
        .checked_add(alignment - 1)
        .map(|value| (value / alignment) * alignment)
        .ok_or(GpuColorFrameReadbackError::ReadbackLayoutOverflow)
}

/// Convert IEEE 754 half-precision (f16) bits to f32.
fn f16_to_f32(half: u16) -> f32 {
    let sign = (half >> 15) as u32;
    let exponent = ((half >> 10) & 0x1F) as u32;
    let mantissa = (half & 0x3FF) as u32;

    if exponent == 0 {
        if mantissa == 0 {
            // Zero.
            f32::from_bits(sign << 31)
        } else {
            // Denormalized.
            let value = (mantissa as f32) / 1024.0 * f32::from_bits(0x38800000); // 2^-14
            f32::from_bits((sign << 31) | value.to_bits())
        }
    } else if exponent == 31 {
        // Infinity or NaN.
        f32::from_bits((sign << 31) | 0x7F800000 | (mantissa << 13))
    } else {
        // Normalized.
        let biased_exponent = exponent as i32 - 15 + 127;
        let bits = (sign << 31) | ((biased_exponent as u32) << 23) | (mantissa << 13);
        f32::from_bits(bits)
    }
}

fn default_color_frame_texture_usage() -> wgpu::TextureUsages {
    wgpu::TextureUsages::COPY_DST
        | wgpu::TextureUsages::COPY_SRC
        | wgpu::TextureUsages::TEXTURE_BINDING
        | wgpu::TextureUsages::RENDER_ATTACHMENT
}

fn create_color_frame_texture(
    device: &wgpu::Device,
    label: &str,
    extent: wgpu::Extent3d,
    texture_format: GpuColorFrameTextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: texture_format.to_wgpu(),
        usage,
        view_formats: &[],
    })
}

fn create_color_frame_view_and_sampler(
    device: &wgpu::Device,
    texture: &wgpu::Texture,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gpu_color_frame_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
    });
    (texture_view, sampler)
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
    fn gpu_color_frame_id_allocator_is_monotonic() {
        let mut allocator = GpuColorFrameIdAllocator::new(40);

        assert_eq!(allocator.allocate().raw(), 40);
        assert_eq!(allocator.allocate().raw(), 41);
        assert_eq!(allocator.next_raw(), 42);
    }

    #[test]
    fn native_decoded_frame_texture_format_names_are_stable() {
        assert_eq!(GpuNativeDecodedFrameTextureFormat::Nv12.as_str(), "Nv12");
        assert_eq!(GpuNativeDecodedFrameTextureFormat::P010.as_str(), "P010");
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::Rgba8Unorm.as_str(),
            "Rgba8Unorm"
        );
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::Bgra8Unorm.as_str(),
            "Bgra8Unorm"
        );
    }

    #[test]
    fn native_decoded_frame_import_defaults_to_fail_closed() {
        let mut ids = GpuColorFrameIdAllocator::new(500);
        let err = GpuNativeDecodedFrameImportPlan::from_contract(
            &mut ids,
            native_import_contract(),
            &GpuNativeDecodedFrameImportSupport::unavailable(),
        )
        .expect_err("native import must fail until a renderer backend is connected");

        assert_eq!(
            err,
            GpuNativeDecodedFrameImportPlanError::RendererBackendUnavailable
        );
        assert_eq!(ids.next_raw(), 500);
    }

    #[test]
    fn native_decoded_frame_import_rejects_unsupported_handle_kind() {
        let mut ids = GpuColorFrameIdAllocator::new(500);
        let support = GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::CVPixelBuffer],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );

        let err = GpuNativeDecodedFrameImportPlan::from_contract(
            &mut ids,
            native_import_contract(),
            &support,
        )
        .expect_err("unsupported decoder handle kind must fail");

        assert_eq!(
            err,
            GpuNativeDecodedFrameImportPlanError::UnsupportedHandleKind {
                handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D
            }
        );
    }

    #[test]
    fn native_decoded_frame_import_rejects_unsupported_source_format() {
        let mut ids = GpuColorFrameIdAllocator::new(500);
        let support = GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::P010],
        );

        let err = GpuNativeDecodedFrameImportPlan::from_contract(
            &mut ids,
            native_import_contract(),
            &support,
        )
        .expect_err("unsupported source texture format must fail");

        assert_eq!(
            err,
            GpuNativeDecodedFrameImportPlanError::UnsupportedSourceTextureFormat {
                source_texture_format: GpuNativeDecodedFrameTextureFormat::Nv12
            }
        );
    }

    #[test]
    fn native_decoded_frame_import_requires_float_working_texture() {
        let mut ids = GpuColorFrameIdAllocator::new(500);
        let support = GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );
        let mut contract = native_import_contract();
        contract.working_texture_format = GpuColorFrameTextureFormat::Rgba8Unorm;

        let err = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect_err("native input must not produce RGBA8 working frames");

        assert_eq!(
            err,
            GpuNativeDecodedFrameImportPlanError::UnsupportedWorkingTextureFormat {
                working_texture_format: GpuColorFrameTextureFormat::Rgba8Unorm
            }
        );
    }

    #[test]
    fn native_decoded_frame_import_plan_produces_renderer_owned_working_frame() {
        let mut ids = GpuColorFrameIdAllocator::new(500);
        let support = GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );

        let plan = GpuNativeDecodedFrameImportPlan::from_contract(
            &mut ids,
            native_import_contract(),
            &support,
        )
        .expect("ready backend can produce an import plan");

        assert_eq!(plan.handle_kind, DecodedGpuFrameHandleKind::D3D11Texture2D);
        assert_eq!(
            plan.source_texture_format,
            GpuNativeDecodedFrameTextureFormat::Nv12
        );
        assert_eq!(plan.source_color_space, ColorSpace::Rec2100Pq);
        assert_eq!(plan.working_frame.id().raw(), 500);
        assert_eq!(
            plan.working_frame.descriptor(),
            ColorFrameDescriptor {
                width: 3840,
                height: 2160,
                color_space: ColorSpace::Rec2020,
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
            }
        );
        assert_eq!(
            plan.working_frame.texture_format(),
            GpuColorFrameTextureFormat::Rgba16Float
        );
        assert_eq!(ids.next_raw(), 501);
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

    #[test]
    fn gpu_color_frame_resource_table_clear_removes_all_entries() {
        let first = gpu_handle(
            104,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let second = gpu_handle(
            105,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let mut table = GpuColorFrameResourceTable::new();
        table.insert(GpuColorFrameResource::new(first, "first")).expect("insert first");
        table
            .insert(GpuColorFrameResource::new(second, "second"))
            .expect("insert second");

        table.clear();

        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn gpu_color_frame_upload_plan_packs_cpu_linear_float_as_rgba32float() {
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: 2,
            height: 1,
            color_space: ColorSpace::Rec709,
            data: vec![[0.25, 0.5, 0.75, 1.0], [1.25, 1.5, 1.75, 0.5]],
        });

        let plan = GpuColorFrameUploadPlan::from_cpu_color_frame(
            GpuColorFrameId::from_raw(200),
            &frame,
            GpuColorFrameTextureFormat::Rgba32Float,
            "working-upload",
        )
        .expect("float upload plan");

        assert_eq!(plan.handle.id().raw(), 200);
        assert_eq!(
            plan.handle.descriptor(),
            frame.descriptor().with_residency(ColorFrameResidency::Gpu)
        );
        assert_eq!(plan.texture_format, GpuColorFrameTextureFormat::Rgba32Float);
        assert_eq!(plan.bytes_per_row, 2 * 16);
        assert_eq!(plan.rows_per_image, 1);
        assert_eq!(plan.bytes.len(), 2 * 16);
        let floats: &[f32] = bytemuck::cast_slice(&plan.bytes);
        assert_eq!(floats, &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 0.5]);
    }

    #[test]
    fn gpu_color_frame_allocation_plan_preserves_handle_contract_and_usage() {
        let handle = gpu_handle(
            199,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );

        let plan = GpuColorFrameAllocationPlan::for_handle(handle.clone());

        assert_eq!(plan.handle, handle);
        assert_eq!(plan.texture_format, GpuColorFrameTextureFormat::Rgba16Float);
        assert_eq!(plan.extent.width, 1920);
        assert_eq!(plan.extent.height, 1080);
        assert_eq!(plan.extent.depth_or_array_layers, 1);
        assert!(plan.usage.contains(wgpu::TextureUsages::COPY_DST));
        assert!(plan.usage.contains(wgpu::TextureUsages::COPY_SRC));
        assert!(plan.usage.contains(wgpu::TextureUsages::TEXTURE_BINDING));
        assert!(plan.usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
    }

    #[test]
    fn gpu_color_frame_upload_plan_packs_cpu_encoded_rgba8() {
        let frame = CpuEncodedColorFrame::source_rgba8(
            2,
            1,
            ColorSpace::Srgb,
            vec![0, 64, 128, 255, 255, 128, 64, 32],
        );

        let plan = GpuColorFrameUploadPlan::from_cpu_encoded_frame(
            GpuColorFrameId::from_raw(201),
            &frame,
            GpuColorFrameTextureFormat::Rgba8Unorm,
            "source-upload",
        )
        .expect("encoded upload plan");

        assert_eq!(
            plan.handle.descriptor(),
            frame.descriptor().with_residency(ColorFrameResidency::Gpu)
        );
        assert_eq!(plan.texture_format, GpuColorFrameTextureFormat::Rgba8Unorm);
        assert_eq!(plan.bytes_per_row, 2 * 4);
        assert_eq!(plan.rows_per_image, 1);
        assert_eq!(plan.bytes, frame.rgba());
    }

    #[test]
    fn cpu_encoded_color_frame_clone_shares_rgba_payload() {
        let frame = CpuEncodedColorFrame::source_rgba8(
            2,
            1,
            ColorSpace::Srgb,
            vec![0, 64, 128, 255, 255, 128, 64, 32],
        );

        let cloned = frame.clone();

        assert!(Arc::ptr_eq(&frame.rgba, &cloned.rgba));
        assert_eq!(cloned.rgba(), frame.rgba());
        assert_eq!(cloned.descriptor(), frame.descriptor());
    }

    #[test]
    fn cpu_encoded_color_frame_shared_constructor_preserves_payload() {
        let payload = Arc::new(vec![0, 64, 128, 255, 255, 128, 64, 32]);
        let frame =
            CpuEncodedColorFrame::source_rgba8_shared(2, 1, ColorSpace::Srgb, Arc::clone(&payload));

        assert!(Arc::ptr_eq(&payload, &frame.rgba));
        assert_eq!(frame.rgba(), payload.as_slice());
    }

    #[test]
    fn gpu_color_frame_upload_plan_rejects_unsupported_cpu_formats() {
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: 1,
            height: 1,
            color_space: ColorSpace::Rec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]],
        });
        let encoded =
            CpuEncodedColorFrame::source_rgba8(1, 1, ColorSpace::Rec709, vec![0, 0, 0, 255]);

        let float_err = GpuColorFrameUploadPlan::from_cpu_color_frame(
            GpuColorFrameId::from_raw(202),
            &frame,
            GpuColorFrameTextureFormat::Rgba16Float,
            "float-rgba16",
        )
        .expect_err("rgba16 float upload is intentionally not implicit");
        assert_eq!(
            float_err,
            GpuColorFrameUploadError::UnsupportedCpuFloatTextureFormat {
                texture_format: GpuColorFrameTextureFormat::Rgba16Float
            }
        );

        let encoded_err = GpuColorFrameUploadPlan::from_cpu_encoded_frame(
            GpuColorFrameId::from_raw(203),
            &encoded,
            GpuColorFrameTextureFormat::Rgba32Float,
            "encoded-rgba32",
        )
        .expect_err("encoded upload must stay rgba8");
        assert_eq!(
            encoded_err,
            GpuColorFrameUploadError::UnsupportedCpuEncodedTextureFormat {
                texture_format: GpuColorFrameTextureFormat::Rgba32Float
            }
        );
    }

    #[test]
    fn gpu_color_frame_readback_plan_aligns_rows_and_unpacks_rgba8() {
        let descriptor = ColorFrameDescriptor {
            width: 3,
            height: 2,
            color_space: ColorSpace::Srgb,
            domain: ColorFrameDomain::Display,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Gpu,
        };
        let handle = gpu_handle(300, descriptor, GpuColorFrameTextureFormat::Rgba8Unorm);

        let plan = GpuColorFrameReadbackPlan::encoded_rgba8(handle.clone()).expect("readback plan");

        assert_eq!(plan.handle, handle);
        assert_eq!(
            plan.output_descriptor,
            descriptor.with_residency(ColorFrameResidency::Cpu)
        );
        assert_eq!(plan.unpadded_bytes_per_row, 12);
        assert_eq!(
            plan.padded_bytes_per_row,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
        );
        assert_eq!(
            plan.buffer_size,
            u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * 2
        );

        let row0 = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let row1 = [13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24];
        let mut mapped = vec![0u8; plan.buffer_size as usize];
        mapped[0..12].copy_from_slice(&row0);
        let row1_start = plan.padded_bytes_per_row as usize;
        mapped[row1_start..row1_start + 12].copy_from_slice(&row1);

        let frame = plan.unpack_mapped_rgba8(&mapped).expect("unpack readback");

        assert_eq!(frame.descriptor(), plan.output_descriptor);
        assert_eq!(frame.rgba(), [&row0[..], &row1[..]].concat().as_slice());
    }

    #[test]
    fn gpu_color_frame_readback_plan_rejects_unsupported_contracts() {
        let mut descriptor = working_descriptor();
        descriptor.encoding = ColorFrameEncoding::EncodedRgba8;
        let rgba16 = gpu_handle(301, descriptor, GpuColorFrameTextureFormat::Rgba16Float);
        let err = GpuColorFrameReadbackPlan::encoded_rgba8(rgba16)
            .expect_err("rgba16 readback must be explicit");
        assert_eq!(
            err,
            GpuColorFrameReadbackError::UnsupportedTextureFormat {
                texture_format: GpuColorFrameTextureFormat::Rgba16Float
            }
        );

        let linear = gpu_handle(
            302,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba8Unorm,
        );
        let err = GpuColorFrameReadbackPlan::encoded_rgba8(linear)
            .expect_err("linear frame cannot use encoded readback");
        assert_eq!(
            err,
            GpuColorFrameReadbackError::UnsupportedEncoding {
                encoding: ColorFrameEncoding::LinearFloat
            }
        );
    }

    #[test]
    fn gpu_color_frame_readback_unpack_rejects_short_mapped_buffer() {
        let descriptor = ColorFrameDescriptor {
            width: 2,
            height: 1,
            color_space: ColorSpace::Rec709,
            domain: ColorFrameDomain::Export,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Gpu,
        };
        let handle = gpu_handle(303, descriptor, GpuColorFrameTextureFormat::Rgba8Unorm);
        let plan = GpuColorFrameReadbackPlan::encoded_rgba8(handle).expect("readback plan");

        let err = plan.unpack_mapped_rgba8(&[0; 8]).expect_err("short mapped buffer must fail");

        assert_eq!(
            err,
            GpuColorFrameReadbackError::MappedBufferTooSmall {
                expected: plan.buffer_size as usize,
                actual: 8
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

    fn native_import_contract() -> GpuNativeDecodedFrameImportContract {
        GpuNativeDecodedFrameImportContract {
            width: 3840,
            height: 2160,
            source_color_space: ColorSpace::Rec2100Pq,
            working_color_space: ColorSpace::Rec2020,
            handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
            source_texture_format: GpuNativeDecodedFrameTextureFormat::Nv12,
            working_texture_format: GpuColorFrameTextureFormat::Rgba16Float,
            label: "native-decoded-working".to_owned(),
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
