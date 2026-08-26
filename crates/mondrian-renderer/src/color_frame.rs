use crate::color_transform::{RenderColorTransformBackend, RenderInputTransform};
use mondrian_core::{
    display_calibration::DisplayCalibrationKey, timeline_data::AlphaInterpretation,
    types::ColorSpace, ColorMatrixCoefficients, ColorTransferCharacteristic, WorkingColorSpace,
    WorkingRgbaF32Frame,
};
use mondrian_media::{
    DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat, PreviewNativeDecodedFrame,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Semantic role of a frame in the color-managed render graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFrameDomain {
    /// Decoded source pixels before timeline working-space conversion.
    Source,
    /// Timeline working-space pixels after input transforms and compositing.
    Working,
    /// Color-managed intermediate pixels prepared for an effect's declared domain.
    Effect,
    /// Non-color scalar alpha/mask values stored in the alpha channel.
    AlphaMask,
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
    /// Non-linear color-space-encoded RGBA stored as floating-point samples.
    ///
    /// Native YCbCr decoding produces this representation before the OCIO
    /// input transform. Display and export transforms also produce it when
    /// their destination transfer function is retained at float precision.
    /// It preserves signal precision without falsely labeling encoded values
    /// as linear light.
    EncodedFloat,
    /// Non-linear, destination-encoded RGBA bytes.
    EncodedRgba8,
    /// Monitor-device RGB values produced by an explicit calibration processor.
    DeviceFloat,
}

/// Memory residency for a render-graph frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFrameResidency {
    /// Pixels are resident in CPU memory.
    Cpu,
    /// Pixels are resident in GPU memory and represented by a renderer handle.
    Gpu,
}

/// Association of RGB samples with linear coverage alpha.
///
/// Alpha is never color-managed. This value is nevertheless part of the frame
/// contract because filtering and compositing must know whether RGB is stored
/// independently from coverage or already multiplied by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorFrameAlpha {
    /// RGB is independent of alpha and alpha carries coverage.
    StraightCoverage,
    /// RGB has been multiplied by coverage alpha.
    PremultipliedCoverage,
    /// Every pixel is guaranteed fully opaque.
    Opaque,
}

impl ColorFrameAlpha {
    /// Return whether RGB is stored premultiplied by coverage.
    pub const fn is_premultiplied(self) -> bool {
        matches!(self, Self::PremultipliedCoverage)
    }

    /// Return whether this contract can be consumed as straight RGB without conversion.
    pub const fn is_straight_compatible(self) -> bool {
        matches!(self, Self::StraightCoverage | Self::Opaque)
    }
}

/// Color identity carried by a renderer frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFrameSpace {
    /// External source, display, or delivery color identity.
    Color(ColorSpace),
    /// Linear-light effects/compositing samples.
    Working(WorkingColorSpace),
    /// Monitor-device RGB identity after ICC calibration.
    Device(DisplayCalibrationKey),
    /// Non-color data that must never enter a color transform.
    NonColorData,
}

impl ColorFrameSpace {
    /// Return the external color identity, if this is a boundary frame.
    pub const fn color(self) -> Option<ColorSpace> {
        match self {
            Self::Color(space) => Some(space),
            Self::Working(_) | Self::Device(_) | Self::NonColorData => None,
        }
    }

    /// Return the linear identity, if this is a working frame.
    pub const fn working(self) -> Option<WorkingColorSpace> {
        match self {
            Self::Color(_) | Self::Device(_) | Self::NonColorData => None,
            Self::Working(space) => Some(space),
        }
    }
}

impl From<ColorSpace> for ColorFrameSpace {
    fn from(value: ColorSpace) -> Self {
        Self::Color(value)
    }
}

impl From<WorkingColorSpace> for ColorFrameSpace {
    fn from(value: WorkingColorSpace) -> Self {
        Self::Working(value)
    }
}

/// Metadata that makes a frame's color contract explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorFrameDescriptor {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Color space currently represented by the pixels.
    pub color_space: ColorFrameSpace,
    /// Frame role in the render graph.
    pub domain: ColorFrameDomain,
    /// Pixel encoding.
    pub encoding: ColorFrameEncoding,
    /// CPU/GPU residency.
    pub residency: ColorFrameResidency,
    /// RGB/coverage association carried by the pixels.
    pub alpha: ColorFrameAlpha,
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

    /// Whether non-color space identity and non-color frame role agree.
    pub const fn has_coherent_space_domain(self) -> bool {
        matches!(
            (self.color_space, self.domain),
            (ColorFrameSpace::NonColorData, ColorFrameDomain::AlphaMask)
        ) || (!matches!(self.color_space, ColorFrameSpace::NonColorData)
            && !matches!(self.domain, ColorFrameDomain::AlphaMask))
    }
}

/// Renderer-owned identifier for a GPU color frame resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuColorFrameId {
    allocator_authority: u64,
    raw: u64,
}

impl GpuColorFrameId {
    /// Create a fixture identifier inside the renderer-owned crate boundary.
    #[cfg(test)]
    pub(crate) fn from_raw(raw: u64) -> Self {
        Self { allocator_authority: 0, raw }
    }

    fn from_allocator(allocator_authority: u64, raw: u64) -> Self {
        Self { allocator_authority, raw }
    }

    /// Return the allocator authority that owns this identity.
    pub fn allocator_authority(self) -> u64 {
        self.allocator_authority
    }

    /// Return the authority-local raw renderer resource sequence.
    pub fn raw(self) -> u64 {
        self.raw
    }
}

/// Monotonic allocator for renderer-owned GPU color frame ids.
///
/// Every allocator claims one process-unique non-zero authority. `u64::MAX` is
/// reserved as the terminal sequence exhaustion sentinel. Allocators are not
/// cloneable and never wrap, saturate, or reuse either identity component.
#[derive(Debug, PartialEq, Eq)]
pub struct GpuColorFrameIdAllocator {
    authority: u64,
    next: u64,
}

/// Failure returned when the GPU frame identity space is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GpuColorFrameIdAllocationError {
    /// No new process-unique allocator authority remains.
    #[error("renderer GPU color frame allocator authority space is exhausted")]
    AllocatorAuthorityExhausted,
    /// This allocator has consumed every permitted authority-local sequence.
    #[error("renderer GPU color frame sequence is exhausted for allocator authority {authority}")]
    SequenceExhausted {
        /// Exhausted process-unique allocator authority.
        authority: u64,
    },
}

static NEXT_GPU_COLOR_FRAME_ALLOCATOR_AUTHORITY: AtomicU64 = AtomicU64::new(1);

impl GpuColorFrameIdAllocator {
    /// Create an allocator with a unique authority and the provided first raw id.
    ///
    /// Passing `u64::MAX` creates an allocator whose sequence is explicitly
    /// exhausted. Process authority exhaustion rejects construction.
    pub fn new(first: u64) -> Result<Self, GpuColorFrameIdAllocationError> {
        let authority = claim_gpu_color_frame_allocator_authority()
            .ok_or(GpuColorFrameIdAllocationError::AllocatorAuthorityExhausted)?;
        Ok(Self { authority, next: first })
    }

    /// Allocate the next frame id, failing closed at identity exhaustion.
    pub fn allocate(&mut self) -> Result<GpuColorFrameId, GpuColorFrameIdAllocationError> {
        let authority = self.authority;
        let raw = self.next;
        let next = raw
            .checked_add(1)
            .ok_or(GpuColorFrameIdAllocationError::SequenceExhausted { authority })?;
        let id = GpuColorFrameId::from_allocator(authority, raw);
        self.next = next;
        Ok(id)
    }

    /// Return this allocator's process-unique authority.
    pub fn authority(&self) -> u64 {
        self.authority
    }

    /// Return the next raw id, or the reserved exhaustion sentinel.
    pub fn next_raw(&self) -> u64 {
        self.next
    }

    /// Whether no further unique identity can be allocated.
    pub fn is_exhausted(&self) -> bool {
        self.next == u64::MAX
    }
}

fn claim_gpu_color_frame_allocator_authority() -> Option<u64> {
    NEXT_GPU_COLOR_FRAME_ALLOCATOR_AUTHORITY
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .ok()
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
        if !descriptor.has_coherent_space_domain() {
            return Err(GpuColorFrameHandleError::IncoherentSpaceDomain {
                space: descriptor.color_space,
                domain: descriptor.domain,
            });
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
    /// Non-color space identity must pair exactly with a non-color frame role.
    #[error("GPU color frame has incoherent space {space:?} and domain {domain:?}")]
    IncoherentSpaceDomain {
        /// Rejected sample-space identity.
        space: ColorFrameSpace,
        /// Rejected render-graph role.
        domain: ColorFrameDomain,
    },
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

    /// Move the exact resource out of the table after validating its complete contract.
    ///
    /// A mismatched descriptor or texture format leaves the stored resource
    /// untouched. Presentation adapters use this transition to detach one
    /// output without exposing id-only removal as an ownership authority.
    pub fn take(
        &mut self,
        handle: &GpuColorFrameHandle,
    ) -> Result<GpuColorFrameResource<R>, GpuColorFrameResourceTableError> {
        self.get(handle)?;
        self.entries
            .remove(&handle.id())
            .ok_or(GpuColorFrameResourceTableError::MissingFrame { id: handle.id() })
    }

    /// Remove a resource entry by frame id.
    pub fn remove(&mut self, id: GpuColorFrameId) -> Option<GpuColorFrameResource<R>> {
        self.entries.remove(&id)
    }

    /// Remove every resource entry from the table.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drain every resource entry while retaining the table allocation.
    pub fn drain(&mut self) -> impl Iterator<Item = GpuColorFrameResource<R>> + '_ {
        self.entries.drain().map(|(_, resource)| resource)
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
    cached_bind_groups: Mutex<VecDeque<CachedGpuColorFrameBindGroup>>,
}

const MAX_CACHED_BIND_GROUPS_PER_GPU_COLOR_FRAME: usize = 8;
static NEXT_GPU_COLOR_FRAME_BIND_GROUP_CACHE_KEY: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GpuColorFrameBindGroupCacheKey(u64);

/// Failure returned when no unique GPU frame bind-group cache key remains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("renderer GPU color frame bind-group cache key space is exhausted")]
pub struct GpuColorFrameBindGroupCacheKeyAllocationError;

impl GpuColorFrameBindGroupCacheKey {
    pub(crate) fn allocate() -> Result<Self, GpuColorFrameBindGroupCacheKeyAllocationError> {
        allocate_gpu_color_frame_bind_group_cache_key(&NEXT_GPU_COLOR_FRAME_BIND_GROUP_CACHE_KEY)
    }
}

fn allocate_gpu_color_frame_bind_group_cache_key(
    sequence: &AtomicU64,
) -> Result<GpuColorFrameBindGroupCacheKey, GpuColorFrameBindGroupCacheKeyAllocationError> {
    sequence
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map(GpuColorFrameBindGroupCacheKey)
        .map_err(|_| GpuColorFrameBindGroupCacheKeyAllocationError)
}

struct CachedGpuColorFrameBindGroup {
    key: GpuColorFrameBindGroupCacheKey,
    bind_group: wgpu::BindGroup,
}

impl GpuColorFrameWgpuResource {
    fn new(
        texture: wgpu::Texture,
        texture_view: wgpu::TextureView,
        sampler: wgpu::Sampler,
    ) -> Self {
        Self {
            texture,
            texture_view,
            sampler,
            cached_bind_groups: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn cached_bind_group(
        &self,
        key: GpuColorFrameBindGroupCacheKey,
        create: impl FnOnce(&wgpu::TextureView) -> wgpu::BindGroup,
    ) -> (wgpu::BindGroup, bool) {
        let mut cache = self.cached_bind_groups.lock();
        if let Some(position) = cache.iter().position(|entry| entry.key == key)
            && let Some(entry) = cache.remove(position)
        {
            let bind_group = entry.bind_group.clone();
            cache.push_back(entry);
            return (bind_group, true);
        }
        let bind_group = create(&self.texture_view);
        if cache.len() >= MAX_CACHED_BIND_GROUPS_PER_GPU_COLOR_FRAME {
            cache.pop_front();
        }
        cache.push_back(CachedGpuColorFrameBindGroup { key, bind_group: bind_group.clone() });
        (bind_group, false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GpuColorFrameWgpuResourcePoolKey {
    width: u32,
    height: u32,
    texture_format: GpuColorFrameTextureFormat,
    usage_bits: u32,
}

impl GpuColorFrameWgpuResourcePoolKey {
    fn from_plan(plan: &GpuColorFrameAllocationPlan) -> Self {
        Self {
            width: plan.extent.width,
            height: plan.extent.height,
            texture_format: plan.texture_format,
            usage_bits: plan.usage.bits(),
        }
    }

    fn from_resource(resource: &GpuColorFrameResource<GpuColorFrameWgpuResource>) -> Self {
        let texture = &resource.resource().texture;
        Self {
            width: texture.width(),
            height: texture.height(),
            texture_format: resource.handle().texture_format(),
            usage_bits: texture.usage().bits(),
        }
    }

    fn logical_byte_len(self) -> u128 {
        u128::from(self.width)
            * u128::from(self.height)
            * u128::from(self.texture_format.bytes_per_pixel())
    }

    fn byte_len(self) -> u64 {
        saturating_u128_to_u64(self.logical_byte_len())
    }
}

/// Bounded reuse policy for renderer-owned color-frame textures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuColorFrameWgpuResourcePoolOptions {
    /// Maximum idle textures retained for one exact extent/format/usage contract.
    pub max_per_contract: usize,
    /// Maximum approximate idle texture bytes retained across all contracts.
    pub max_retained_bytes: u64,
}

impl Default for GpuColorFrameWgpuResourcePoolOptions {
    fn default() -> Self {
        Self {
            max_per_contract: 3,
            max_retained_bytes: 384 * 1024 * 1024,
        }
    }
}

/// Point-in-time evidence for renderer color-frame texture reuse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuColorFrameWgpuResourcePoolDiagnostics {
    /// Exact-contract acquisitions served from retained resources.
    pub hits: u64,
    /// Acquisitions that required a new GPU texture allocation.
    pub misses: u64,
    /// Resources returned after a submitted or abandoned frame candidate.
    pub releases: u64,
    /// Resources dropped to enforce per-contract or byte limits.
    pub evictions: u64,
    /// Device/runtime invalidations that revoked all previously issued return generations.
    pub invalidations: u64,
    /// Detached resources dropped instead of re-entering an invalidated pool generation.
    pub stale_generation_releases: u64,
    /// Current presentation textures detached into move-only external leases.
    pub detached_presentation_resources: u64,
    /// Current logical bytes owned by detached presentation leases.
    pub detached_presentation_bytes: u64,
    /// Highest simultaneous detached-presentation texture count.
    pub detached_presentation_high_water_resources: u64,
    /// Highest simultaneous detached-presentation logical byte ownership.
    pub detached_presentation_high_water_bytes: u64,
    /// Transitions into a demand that cannot be represented by the public `u64` budget model.
    pub detached_presentation_accounting_overflows: u64,
    /// Whether the current detached-presentation demand exceeds the public budget model.
    pub detached_presentation_accounting_overflowed: bool,
    /// Current number of idle retained resources.
    pub retained_resources: usize,
    /// Approximate bytes occupied by idle retained resources.
    pub retained_bytes: u64,
}

struct PooledGpuColorFrameWgpuResource {
    key: GpuColorFrameWgpuResourcePoolKey,
    payload: GpuColorFrameWgpuResource,
}

struct GpuColorFrameWgpuResourcePoolState {
    options: GpuColorFrameWgpuResourcePoolOptions,
    idle: VecDeque<PooledGpuColorFrameWgpuResource>,
    retained_bytes: u64,
    hits: u64,
    misses: u64,
    releases: u64,
    evictions: u64,
    generation: u64,
    accepts_generation_returns: bool,
    invalidations: u64,
    stale_generation_releases: u64,
    detached_presentation_resources: u128,
    detached_presentation_bytes: u128,
    detached_presentation_high_water_resources: u128,
    detached_presentation_high_water_bytes: u128,
    detached_presentation_accounting_overflows: u64,
    detached_presentation_accounting_irrecoverable: bool,
}

impl Default for GpuColorFrameWgpuResourcePoolState {
    fn default() -> Self {
        Self {
            options: GpuColorFrameWgpuResourcePoolOptions::default(),
            idle: VecDeque::new(),
            retained_bytes: 0,
            hits: 0,
            misses: 0,
            releases: 0,
            evictions: 0,
            generation: 1,
            accepts_generation_returns: true,
            invalidations: 0,
            stale_generation_releases: 0,
            detached_presentation_resources: 0,
            detached_presentation_bytes: 0,
            detached_presentation_high_water_resources: 0,
            detached_presentation_high_water_bytes: 0,
            detached_presentation_accounting_overflows: 0,
            detached_presentation_accounting_irrecoverable: false,
        }
    }
}

/// Opaque generation authorizing a detached resource to return to one pool epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuColorFrameWgpuResourcePoolGeneration(u64);

/// Device-scoped, byte-bounded LRU pool for typed color-frame textures.
///
/// Resources may be released only after their previous work was submitted to
/// the same ordered GPU queue or the candidate was abandoned before submit.
/// Reuse therefore adds no CPU completion wait and preserves queue ordering.
pub struct GpuColorFrameWgpuResourcePool {
    state: Mutex<GpuColorFrameWgpuResourcePoolState>,
}

impl GpuColorFrameWgpuResourcePool {
    /// Create a pool with an explicit per-contract and retained-byte policy.
    pub fn new(options: GpuColorFrameWgpuResourcePoolOptions) -> Self {
        Self {
            state: Mutex::new(GpuColorFrameWgpuResourcePoolState {
                options,
                ..GpuColorFrameWgpuResourcePoolState::default()
            }),
        }
    }

    /// Replace the idle-retention policy and synchronously enforce it.
    ///
    /// Resources already checked out remain valid. A later release observes
    /// the new policy, so shrinking a live execution owner cannot repopulate
    /// residency beyond the new grant.
    pub fn reconfigure(&self, options: GpuColorFrameWgpuResourcePoolOptions) {
        let mut state = self.state.lock();
        state.options = options;
        enforce_gpu_color_frame_pool_limits(&mut state);
    }

    /// Return the currently enforced idle-retention policy.
    pub fn options(&self) -> GpuColorFrameWgpuResourcePoolOptions {
        self.state.lock().options
    }

    /// Capture the current return generation for a move-only detached resource.
    #[cfg(test)]
    pub(crate) fn generation(&self) -> GpuColorFrameWgpuResourcePoolGeneration {
        GpuColorFrameWgpuResourcePoolGeneration(self.state.lock().generation)
    }

    /// Acquire an exact-contract texture or allocate one on a pool miss.
    pub fn acquire(
        &self,
        device: &wgpu::Device,
        plan: &GpuColorFrameAllocationPlan,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        let key = GpuColorFrameWgpuResourcePoolKey::from_plan(plan);
        let reused = {
            let mut state = self.state.lock();
            let position = state.idle.iter().position(|entry| entry.key == key);
            if let Some(position) = position {
                if let Some(entry) = state.idle.remove(position) {
                    state.retained_bytes = state.retained_bytes.saturating_sub(key.byte_len());
                    state.hits = state.hits.saturating_add(1);
                    Some(entry.payload)
                } else {
                    state.misses = state.misses.saturating_add(1);
                    None
                }
            } else {
                state.misses = state.misses.saturating_add(1);
                None
            }
        };
        match reused {
            Some(payload) => GpuColorFrameResource::new(plan.handle.clone(), payload),
            None => GpuColorFrameUploader::allocate(device, plan),
        }
    }

    /// Return one renderer-owned texture after its previous queue use was ordered.
    pub fn release(&self, resource: GpuColorFrameResource<GpuColorFrameWgpuResource>) {
        let mut state = self.state.lock();
        release_gpu_color_frame_resource(&mut state, resource);
    }

    /// Register one presentation allocation and atomically capture its return generation.
    fn register_detached_presentation(
        &self,
        byte_len: u128,
    ) -> GpuColorFrameWgpuResourcePoolGeneration {
        let mut state = self.state.lock();
        register_detached_presentation_demand(&mut state, byte_len);
        GpuColorFrameWgpuResourcePoolGeneration(state.generation)
    }

    /// Return a detached presentation resource and retire its active demand.
    ///
    /// A device/runtime reset invalidates earlier generations. Their late drops
    /// release backend handles directly instead of repopulating the idle pool.
    /// Demand retirement and either pool return or backend drop are atomic to
    /// active-working-set observers.
    fn release_detached_presentation(
        &self,
        generation: GpuColorFrameWgpuResourcePoolGeneration,
        byte_len: u128,
        resource: GpuColorFrameResource<GpuColorFrameWgpuResource>,
    ) -> bool {
        let mut state = self.state.lock();
        unregister_detached_presentation_demand(&mut state, byte_len);
        if !state.accepts_generation_returns || generation.0 != state.generation {
            state.stale_generation_releases = state.stale_generation_releases.saturating_add(1);
            drop(state);
            drop(resource);
            return false;
        }
        release_gpu_color_frame_resource(&mut state, resource);
        true
    }

    /// Return exact active demand from every live detached presentation lease.
    ///
    /// `None` is a conservative overflow signal: Viewer admission must reject
    /// instead of treating unrepresentable physical ownership as free.
    pub(crate) fn detached_presentation_demand(&self) -> Option<(u64, u64)> {
        let state = self.state.lock();
        if detached_presentation_demand_overflowed(&state) {
            return None;
        }
        Some((
            state.detached_presentation_resources as u64,
            state.detached_presentation_bytes as u64,
        ))
    }

    /// Revoke every outstanding return generation and drop all idle resources.
    ///
    /// Unlike [`Self::clear`], this is a device/runtime lifetime boundary.
    /// Detached presentation leases from an older generation remain valid
    /// owners, but their eventual drops cannot return resources to this pool.
    pub fn invalidate(&self) {
        let mut state = self.state.lock();
        state.invalidations = state.invalidations.saturating_add(1);
        match state.generation.checked_add(1) {
            Some(next) => state.generation = next,
            None => state.accepts_generation_returns = false,
        }
        state.evictions = state.evictions.saturating_add(state.idle.len() as u64);
        state.idle.clear();
        state.retained_bytes = 0;
    }

    /// Return point-in-time pool reuse and memory-retention evidence.
    pub fn diagnostics(&self) -> GpuColorFrameWgpuResourcePoolDiagnostics {
        let state = self.state.lock();
        GpuColorFrameWgpuResourcePoolDiagnostics {
            hits: state.hits,
            misses: state.misses,
            releases: state.releases,
            evictions: state.evictions,
            invalidations: state.invalidations,
            stale_generation_releases: state.stale_generation_releases,
            detached_presentation_resources: saturating_u128_to_u64(
                state.detached_presentation_resources,
            ),
            detached_presentation_bytes: saturating_u128_to_u64(state.detached_presentation_bytes),
            detached_presentation_high_water_resources: saturating_u128_to_u64(
                state.detached_presentation_high_water_resources,
            ),
            detached_presentation_high_water_bytes: saturating_u128_to_u64(
                state.detached_presentation_high_water_bytes,
            ),
            detached_presentation_accounting_overflows: state
                .detached_presentation_accounting_overflows,
            detached_presentation_accounting_overflowed: detached_presentation_demand_overflowed(
                &state,
            ),
            retained_resources: state.idle.len(),
            retained_bytes: state.retained_bytes,
        }
    }

    /// Drop all idle retained resources while preserving cumulative evidence.
    pub fn clear(&self) {
        let mut state = self.state.lock();
        state.evictions = state.evictions.saturating_add(state.idle.len() as u64);
        state.idle.clear();
        state.retained_bytes = 0;
    }
}

fn register_detached_presentation_demand(
    state: &mut GpuColorFrameWgpuResourcePoolState,
    byte_len: u128,
) {
    let was_overflowed = detached_presentation_demand_overflowed(state);
    let next_resources = state.detached_presentation_resources.checked_add(1);
    let next_bytes = state.detached_presentation_bytes.checked_add(byte_len);
    match (next_resources, next_bytes) {
        (Some(resources), Some(bytes)) => {
            state.detached_presentation_resources = resources;
            state.detached_presentation_bytes = bytes;
            state.detached_presentation_high_water_resources =
                state.detached_presentation_high_water_resources.max(resources);
            state.detached_presentation_high_water_bytes =
                state.detached_presentation_high_water_bytes.max(bytes);
        }
        _ => {
            // A physical process cannot own enough Rust allocations to overflow
            // this u128 ledger, but fail closed if that invariant ever changes.
            state.detached_presentation_accounting_irrecoverable = true;
        }
    }
    if !was_overflowed && detached_presentation_demand_overflowed(state) {
        state.detached_presentation_accounting_overflows =
            state.detached_presentation_accounting_overflows.saturating_add(1);
    }
}

fn unregister_detached_presentation_demand(
    state: &mut GpuColorFrameWgpuResourcePoolState,
    byte_len: u128,
) {
    if state.detached_presentation_accounting_irrecoverable {
        return;
    }
    let next_resources = state.detached_presentation_resources.checked_sub(1);
    let next_bytes = state.detached_presentation_bytes.checked_sub(byte_len);
    match (next_resources, next_bytes) {
        (Some(resources), Some(bytes)) => {
            state.detached_presentation_resources = resources;
            state.detached_presentation_bytes = bytes;
        }
        _ => {
            state.detached_presentation_accounting_irrecoverable = true;
            state.detached_presentation_accounting_overflows =
                state.detached_presentation_accounting_overflows.saturating_add(1);
        }
    }
}

fn detached_presentation_demand_overflowed(state: &GpuColorFrameWgpuResourcePoolState) -> bool {
    state.detached_presentation_accounting_irrecoverable
        || state.detached_presentation_resources > u128::from(u64::MAX)
        || state.detached_presentation_bytes > u128::from(u64::MAX)
}

fn saturating_u128_to_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn release_gpu_color_frame_resource(
    state: &mut GpuColorFrameWgpuResourcePoolState,
    resource: GpuColorFrameResource<GpuColorFrameWgpuResource>,
) {
    let key = GpuColorFrameWgpuResourcePoolKey::from_resource(&resource);
    let byte_len = key.byte_len();
    let (_, payload) = resource.into_parts();
    state.releases = state.releases.saturating_add(1);
    if state.options.max_per_contract == 0 || byte_len > state.options.max_retained_bytes {
        state.evictions = state.evictions.saturating_add(1);
        return;
    }
    while state.idle.iter().filter(|entry| entry.key == key).count()
        >= state.options.max_per_contract
    {
        let Some(position) = state.idle.iter().position(|entry| entry.key == key) else {
            break;
        };
        if let Some(evicted) = state.idle.remove(position) {
            state.retained_bytes = state.retained_bytes.saturating_sub(evicted.key.byte_len());
            state.evictions = state.evictions.saturating_add(1);
        }
    }
    state.idle.push_back(PooledGpuColorFrameWgpuResource { key, payload });
    state.retained_bytes = state.retained_bytes.saturating_add(byte_len);
    enforce_gpu_color_frame_pool_byte_limit(state);
}

fn enforce_gpu_color_frame_pool_limits(state: &mut GpuColorFrameWgpuResourcePoolState) {
    let mut retained_per_contract = HashMap::new();
    let mut index = state.idle.len();
    while index > 0 {
        index -= 1;
        let key = state.idle[index].key;
        let retained = retained_per_contract.entry(key).or_insert(0_usize);
        if *retained >= state.options.max_per_contract {
            if let Some(evicted) = state.idle.remove(index) {
                state.retained_bytes = state.retained_bytes.saturating_sub(evicted.key.byte_len());
                state.evictions = state.evictions.saturating_add(1);
            }
        } else {
            *retained = retained.saturating_add(1);
        }
    }
    enforce_gpu_color_frame_pool_byte_limit(state);
}

fn enforce_gpu_color_frame_pool_byte_limit(state: &mut GpuColorFrameWgpuResourcePoolState) {
    while state.retained_bytes > state.options.max_retained_bytes {
        let Some(evicted) = state.idle.pop_front() else {
            break;
        };
        state.retained_bytes = state.retained_bytes.saturating_sub(evicted.key.byte_len());
        state.evictions = state.evictions.saturating_add(1);
    }
}

impl Default for GpuColorFrameWgpuResourcePool {
    fn default() -> Self {
        Self::new(GpuColorFrameWgpuResourcePoolOptions::default())
    }
}

/// Move-only ownership of one Viewer presentation output detached from a runtime table.
///
/// The lease keeps the actual texture allocation out of the reusable pool for
/// as long as a presentation adapter advertises or samples it. Dropping the
/// lease returns the resource only when its captured pool generation remains
/// valid; device/runtime invalidation instead drops the backend resource.
pub struct ViewerGpuPresentationOutputLease {
    resource: Option<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    pool: Arc<GpuColorFrameWgpuResourcePool>,
    pool_generation: GpuColorFrameWgpuResourcePoolGeneration,
    byte_len: u128,
}

impl ViewerGpuPresentationOutputLease {
    /// Bind one detached resource to the pool generation that produced it.
    pub(crate) fn new(
        resource: GpuColorFrameResource<GpuColorFrameWgpuResource>,
        pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Self {
        let byte_len =
            GpuColorFrameWgpuResourcePoolKey::from_resource(&resource).logical_byte_len();
        let pool_generation = pool.register_detached_presentation(byte_len);
        Self {
            resource: Some(resource),
            pool,
            pool_generation,
            byte_len,
        }
    }

    /// Exact typed renderer handle owned by this presentation lease.
    pub fn handle(&self) -> &GpuColorFrameHandle {
        self.resource().handle()
    }

    /// Complete descriptor and texture-format contract of the detached output.
    pub fn contract(&self) -> GpuColorFrameContract {
        self.handle().contract()
    }

    /// Borrow the actual texture while this move-only lease remains alive.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.resource().resource().texture
    }

    /// Borrow the default presentation view while this move-only lease remains alive.
    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.resource().resource().texture_view
    }

    fn resource(&self) -> &GpuColorFrameResource<GpuColorFrameWgpuResource> {
        self.resource
            .as_ref()
            .expect("presentation output lease resource is present before Drop")
    }
}

impl std::fmt::Debug for ViewerGpuPresentationOutputLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ViewerGpuPresentationOutputLease")
            .field("handle", self.resource().handle())
            .field("pool_generation", &self.pool_generation)
            .finish_non_exhaustive()
    }
}

impl Drop for ViewerGpuPresentationOutputLease {
    fn drop(&mut self) {
        let Some(resource) = self.resource.take() else {
            return;
        };
        let _ =
            self.pool
                .release_detached_presentation(self.pool_generation, self.byte_len, resource);
    }
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
#[derive(Debug, Clone, PartialEq)]
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
    payload: GpuColorFrameUploadPayload,
}

#[derive(Debug, Clone, PartialEq)]
enum GpuColorFrameUploadPayload {
    Bytes(Arc<Vec<u8>>),
    Float32(Arc<Vec<f32>>),
    EncodedRgba32(Arc<EncodedRgbaF32Frame>),
    WorkingRgba32(Arc<WorkingRgbaF32Frame>),
    AlphaMaskRgba32(Arc<Vec<[f32; 4]>>),
}

impl GpuColorFrameUploadPayload {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes.as_slice(),
            Self::Float32(samples) => bytemuck::cast_slice(samples.as_slice()),
            Self::EncodedRgba32(frame) => bytemuck::cast_slice(frame.data.as_slice()),
            Self::WorkingRgba32(frame) => bytemuck::cast_slice(frame.data.as_slice()),
            Self::AlphaMaskRgba32(samples) => bytemuck::cast_slice(samples.as_slice()),
        }
    }
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
        Self::new(
            handle,
            GpuColorFrameUploadPayload::WorkingRgba32(frame.rgba_f32_shared()),
        )
    }

    /// Build an upload plan for one renderer-internal non-color alpha matte.
    pub(crate) fn from_cpu_alpha_mask_frame(
        id: GpuColorFrameId,
        frame: &CpuAlphaMaskFrame,
        label: impl Into<String>,
    ) -> Result<Self, GpuColorFrameUploadError> {
        let descriptor = frame.descriptor().with_residency(ColorFrameResidency::Gpu);
        validate_cpu_pixel_count(descriptor, frame.samples.len())?;
        let handle = GpuColorFrameHandle::new(
            id,
            descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            label,
        )
        .map_err(GpuColorFrameUploadError::Handle)?;
        Self::new(
            handle,
            GpuColorFrameUploadPayload::AlphaMaskRgba32(Arc::clone(&frame.samples)),
        )
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
        Self::new(
            handle,
            GpuColorFrameUploadPayload::Bytes(frame.rgba_shared()),
        )
    }

    /// Build an upload plan for a CPU encoded floating-point source frame.
    pub fn from_cpu_encoded_float_frame(
        id: GpuColorFrameId,
        frame: &CpuEncodedFloatColorFrame,
        label: impl Into<String>,
    ) -> Result<Self, GpuColorFrameUploadError> {
        let descriptor = frame.descriptor().with_residency(ColorFrameResidency::Gpu);
        validate_cpu_pixel_count(descriptor, frame.rgba_f32().data.len())?;
        let handle = GpuColorFrameHandle::new(
            id,
            descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            label,
        )
        .map_err(GpuColorFrameUploadError::Handle)?;
        Self::new(
            handle,
            GpuColorFrameUploadPayload::EncodedRgba32(frame.rgba_f32_shared()),
        )
    }

    /// Build an upload plan for a scene-linear floating-point source frame.
    pub fn from_linear_float_source(
        id: GpuColorFrameId,
        frame: &LinearFloatSource,
        label: impl Into<String>,
    ) -> Result<Self, GpuColorFrameUploadError> {
        let descriptor = frame.descriptor().with_residency(ColorFrameResidency::Gpu);
        let expected = descriptor
            .pixel_count()
            .checked_mul(4)
            .ok_or(GpuColorFrameUploadError::UploadLayoutOverflow)?;
        if frame.data().len() != expected {
            return Err(GpuColorFrameUploadError::FloatComponentCountMismatch {
                expected,
                actual: frame.data().len(),
            });
        }
        let handle = GpuColorFrameHandle::new(
            id,
            descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            label,
        )
        .map_err(GpuColorFrameUploadError::Handle)?;
        Self::new(
            handle,
            GpuColorFrameUploadPayload::Float32(frame.data_shared()),
        )
    }

    fn new(
        handle: GpuColorFrameHandle,
        payload: GpuColorFrameUploadPayload,
    ) -> Result<Self, GpuColorFrameUploadError> {
        let descriptor = handle.descriptor();
        let texture_format = handle.texture_format();
        let bytes_per_row = descriptor
            .width
            .checked_mul(texture_format.bytes_per_pixel())
            .ok_or(GpuColorFrameUploadError::UploadLayoutOverflow)?;
        let expected_len = bytes_per_row as usize * descriptor.height as usize;
        if payload.bytes().len() != expected_len {
            return Err(GpuColorFrameUploadError::ByteLengthMismatch {
                expected: expected_len,
                actual: payload.bytes().len(),
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
            payload,
        })
    }

    /// Borrow the packed bytes exactly as submitted to wgpu.
    pub fn bytes(&self) -> &[u8] {
        self.payload.bytes()
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
            GpuColorFrameWgpuResource::new(texture, texture_view, sampler),
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
            plan.bytes(),
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
            GpuColorFrameWgpuResource::new(texture, texture_view, sampler),
        )
    }

    /// Upload into an exact-contract pooled resource when one is available.
    pub fn upload_with_pool(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GpuColorFrameUploadPlan,
        pool: &GpuColorFrameWgpuResourcePool,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        let allocation = GpuColorFrameAllocationPlan::for_handle(plan.handle.clone());
        let resource = pool.acquire(device, &allocation);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &resource.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            plan.bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(plan.bytes_per_row),
                rows_per_image: Some(plan.rows_per_image),
            },
            plan.extent,
        );
        resource
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
    /// The CPU RGBA f32 component count does not match its descriptor.
    FloatComponentCountMismatch {
        /// Expected scalar component count.
        expected: usize,
        /// Actual scalar component count.
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
    /// 10-bit P010 two-plane YCbCr surface.
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

impl TryFrom<DecodedVideoSurfaceFormat> for GpuNativeDecodedFrameTextureFormat {
    type Error = GpuNativeDecodedFrameSourceFormatError;

    fn try_from(format: DecodedVideoSurfaceFormat) -> Result<Self, Self::Error> {
        match format {
            DecodedVideoSurfaceFormat::Nv12 => Ok(Self::Nv12),
            DecodedVideoSurfaceFormat::P010 => Ok(Self::P010),
            DecodedVideoSurfaceFormat::Rgba8 => Ok(Self::Rgba8Unorm),
            DecodedVideoSurfaceFormat::Bgra8 => Ok(Self::Bgra8Unorm),
            DecodedVideoSurfaceFormat::Unknown
            | DecodedVideoSurfaceFormat::Yuv420p
            | DecodedVideoSurfaceFormat::Yuv420p10le
            | DecodedVideoSurfaceFormat::Yuv422p
            | DecodedVideoSurfaceFormat::Yuv422p10le
            | DecodedVideoSurfaceFormat::Other => {
                Err(GpuNativeDecodedFrameSourceFormatError::Unsupported { format })
            }
        }
    }
}

/// Error returned when a media decoder surface has no native renderer format.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeDecodedFrameSourceFormatError {
    /// The decoder format is not a supported native GPU payload.
    #[error("decoded video surface format {format:?} cannot enter native renderer import")]
    Unsupported {
        /// Unsupported media decoder format.
        format: DecodedVideoSurfaceFormat,
    },
}

/// Encoded video quantization range carried by a native decoder surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuVideoRange {
    /// Studio/legal range YCbCr or RGB values.
    Limited,
    /// Full-range YCbCr or RGB values.
    Full,
}

/// Chroma siting used by a subsampled native decoder surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuVideoChromaLocation {
    /// Chroma location was not signaled. Native import must fail closed.
    Unspecified,
    /// MPEG-2 / H.264 / HEVC left chroma siting.
    Left,
    /// Centered chroma siting.
    Center,
    /// Top-left chroma siting.
    TopLeft,
}

/// GPU shader sampling contract for a native decoded video surface.
///
/// Platform adapters import OS decoder surfaces, but the renderer owns the
/// shader-visible interpretation: range expansion, YCbCr matrix conversion,
/// transfer semantics, chroma siting, and effective bit depth. Keeping this in
/// the renderer contract prevents D3D/VideoToolbox/VA-API adapters from baking
/// in divergent color assumptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuNativeDecodedFrameVideoSampling {
    /// Encoded quantization range.
    pub range: GpuVideoRange,
    /// Matrix used to convert sampled YCbCr into encoded RGB.
    pub matrix: ColorMatrixCoefficients,
    /// Transfer characteristic represented by the encoded RGB signal before
    /// OCIO input conversion.
    pub transfer: ColorTransferCharacteristic,
    /// Effective coded bit depth. NV12 must be 8; P010 must be 10.
    pub bit_depth: u8,
    /// Chroma sample location for subsampled YCbCr surfaces.
    pub chroma_location: GpuVideoChromaLocation,
}

impl GpuNativeDecodedFrameVideoSampling {
    /// Build a sampling contract from the source color space and explicit video
    /// container metadata.
    pub fn from_source_color_space(
        source_color_space: ColorSpace,
        range: GpuVideoRange,
        bit_depth: u8,
        chroma_location: GpuVideoChromaLocation,
    ) -> Self {
        let encoding = source_color_space.encoding();
        Self {
            range,
            matrix: encoding.matrix,
            transfer: encoding.transfer,
            bit_depth,
            chroma_location,
        }
    }

    fn validate_for(
        self,
        source_texture_format: GpuNativeDecodedFrameTextureFormat,
        source_color_space: ColorSpace,
    ) -> Result<(), GpuNativeDecodedFrameImportPlanError> {
        let source_encoding = source_color_space.encoding();
        if self.transfer != source_encoding.transfer {
            return Err(GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                source_texture_format,
                reason: format!(
                    "sampling transfer {:?} does not match source color space {:?} transfer {:?}",
                    self.transfer, source_color_space, source_encoding.transfer
                ),
            });
        }

        match source_texture_format {
            GpuNativeDecodedFrameTextureFormat::Nv12 => {
                self.validate_ycbcr(source_texture_format, 8)
            }
            GpuNativeDecodedFrameTextureFormat::P010 => {
                self.validate_ycbcr(source_texture_format, 10)
            }
            GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
            | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
                if self.bit_depth != 8 {
                    return Err(GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                        source_texture_format,
                        reason: format!(
                            "{} requires 8-bit sampling metadata, got {}",
                            source_texture_format.as_str(),
                            self.bit_depth
                        ),
                    });
                }
                if self.matrix != ColorMatrixCoefficients::Rgb {
                    return Err(GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                        source_texture_format,
                        reason: format!(
                            "{} is an RGB surface and requires an RGB matrix, got {:?}",
                            source_texture_format.as_str(),
                            self.matrix
                        ),
                    });
                }
                Ok(())
            }
        }
    }

    fn validate_ycbcr(
        self,
        source_texture_format: GpuNativeDecodedFrameTextureFormat,
        expected_bit_depth: u8,
    ) -> Result<(), GpuNativeDecodedFrameImportPlanError> {
        if self.bit_depth != expected_bit_depth {
            return Err(GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                source_texture_format,
                reason: format!(
                    "{} requires {}-bit sampling metadata, got {}",
                    source_texture_format.as_str(),
                    expected_bit_depth,
                    self.bit_depth
                ),
            });
        }
        if matches!(
            self.matrix,
            ColorMatrixCoefficients::Rgb | ColorMatrixCoefficients::Unspecified
        ) {
            return Err(GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                source_texture_format,
                reason: format!(
                    "{} requires a specified YCbCr matrix, got {:?}",
                    source_texture_format.as_str(),
                    self.matrix
                ),
            });
        }
        if self.chroma_location == GpuVideoChromaLocation::Unspecified {
            return Err(GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                source_texture_format,
                reason: format!(
                    "{} requires explicit chroma location metadata",
                    source_texture_format.as_str()
                ),
            });
        }
        Ok(())
    }
}

/// Physical transfer mode used before a native decoded surface is sampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum GpuNativeDecodedFrameImportMode {
    /// The active Renderer samples external decoder storage without copying its pixels.
    ZeroCopy,
    /// The active Renderer performs one GPU-local bridge copy before sampling.
    GpuBridgeCopy,
}

/// Renderer backend capability contract for importing native decoded frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuNativeDecodedFrameImportSupport {
    /// Whether the concrete renderer backend has connected native import code.
    pub renderer_backend_ready: bool,
    /// Renderer backend label observed by the app/runtime, when available.
    pub renderer_backend_label: Option<String>,
    /// Structured reason the backend is not ready or is only partially ready.
    pub unavailable_reason: Option<String>,
    /// Decoder handle families accepted by the backend.
    pub supported_handle_kinds: Vec<DecodedGpuFrameHandleKind>,
    /// Decoder source texture formats accepted by the backend.
    pub supported_source_texture_formats: Vec<GpuNativeDecodedFrameTextureFormat>,
    /// Physical transfer mode implemented by this exact backend/device binding.
    pub import_mode: Option<GpuNativeDecodedFrameImportMode>,
    /// Decoder device that produces resources on the renderer's physical adapter.
    pub hardware_decode_device_selector: Option<mondrian_media::HwAccelDeviceSelector>,
}

impl GpuNativeDecodedFrameImportSupport {
    /// Build a fail-closed support value for builds without native import.
    pub fn unavailable() -> Self {
        Self {
            renderer_backend_ready: false,
            renderer_backend_label: None,
            unavailable_reason: Some(
                "renderer backend native decoded-frame import is not connected".to_owned(),
            ),
            supported_handle_kinds: Vec::new(),
            supported_source_texture_formats: Vec::new(),
            import_mode: None,
            hardware_decode_device_selector: None,
        }
    }

    /// Build a fail-closed support value with concrete backend diagnostics.
    pub fn unavailable_with_reason(
        renderer_backend_label: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            renderer_backend_ready: false,
            renderer_backend_label: Some(renderer_backend_label.into()),
            unavailable_reason: Some(reason.into()),
            supported_handle_kinds: Vec::new(),
            supported_source_texture_formats: Vec::new(),
            import_mode: None,
            hardware_decode_device_selector: None,
        }
    }

    /// Build support for a backend that directly samples external decoder storage.
    pub fn ready_zero_copy(
        supported_handle_kinds: Vec<DecodedGpuFrameHandleKind>,
        supported_source_texture_formats: Vec<GpuNativeDecodedFrameTextureFormat>,
    ) -> Self {
        Self::ready_with_mode(
            supported_handle_kinds,
            supported_source_texture_formats,
            GpuNativeDecodedFrameImportMode::ZeroCopy,
        )
    }

    /// Build support for a backend that performs one GPU-local bridge copy.
    pub fn ready_gpu_bridge_copy(
        supported_handle_kinds: Vec<DecodedGpuFrameHandleKind>,
        supported_source_texture_formats: Vec<GpuNativeDecodedFrameTextureFormat>,
    ) -> Self {
        Self::ready_with_mode(
            supported_handle_kinds,
            supported_source_texture_formats,
            GpuNativeDecodedFrameImportMode::GpuBridgeCopy,
        )
    }

    fn ready_with_mode(
        supported_handle_kinds: Vec<DecodedGpuFrameHandleKind>,
        supported_source_texture_formats: Vec<GpuNativeDecodedFrameTextureFormat>,
        import_mode: GpuNativeDecodedFrameImportMode,
    ) -> Self {
        Self {
            renderer_backend_ready: true,
            renderer_backend_label: None,
            unavailable_reason: None,
            supported_handle_kinds,
            supported_source_texture_formats,
            import_mode: Some(import_mode),
            hardware_decode_device_selector: None,
        }
    }

    /// Attach a renderer backend label to this support contract.
    pub fn with_renderer_backend_label(
        mut self,
        renderer_backend_label: impl Into<String>,
    ) -> Self {
        self.renderer_backend_label = Some(renderer_backend_label.into());
        self
    }

    /// Attach the decoder device that matches the renderer adapter.
    pub fn with_hardware_decode_device_selector(
        mut self,
        selector: mondrian_media::HwAccelDeviceSelector,
    ) -> Self {
        self.hardware_decode_device_selector = Some(selector);
        self
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
    /// Renderer materialization width after Preview-scale sampling.
    pub output_width: u32,
    /// Renderer materialization height after Preview-scale sampling.
    pub output_height: u32,
    /// Color space represented by the decoded source surface.
    pub source_color_space: ColorSpace,
    /// Complete OCIO input-transform contract for source -> working conversion.
    pub input_transform: RenderInputTransform,
    /// Decoder handle family.
    pub handle_kind: DecodedGpuFrameHandleKind,
    /// Decoder source texture layout.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
    /// Shader-visible sampling contract for converting the decoded source
    /// surface into encoded RGB before OCIO input conversion.
    pub video_sampling: GpuNativeDecodedFrameVideoSampling,
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
    /// Visible source width carried by the decoder surface.
    pub source_width: u32,
    /// Visible source height carried by the decoder surface.
    pub source_height: u32,
    /// Decoder handle family consumed by the backend.
    pub handle_kind: DecodedGpuFrameHandleKind,
    /// Decoder source texture layout consumed by the backend.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
    /// Source color space represented by the decoder surface.
    pub source_color_space: ColorSpace,
    /// Validated OCIO GPU input transform applied after native surface sampling.
    pub input_transform: RenderInputTransform,
    /// Validated source video sampling contract.
    pub video_sampling: GpuNativeDecodedFrameVideoSampling,
    /// Renderer-owned encoded RGB frame produced by native surface sampling
    /// before the OCIO source-to-working transform.
    pub encoded_source_frame: GpuColorFrameHandle,
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
        if contract.output_width == 0 || contract.output_height == 0 {
            return Err(GpuNativeDecodedFrameImportPlanError::EmptyOutputExtent {
                width: contract.output_width,
                height: contract.output_height,
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
        contract
            .video_sampling
            .validate_for(contract.source_texture_format, contract.source_color_space)?;
        if contract.input_transform.backend != RenderColorTransformBackend::OcioGpuShaderPlan {
            return Err(
                GpuNativeDecodedFrameImportPlanError::UnsupportedInputTransformBackend {
                    backend: contract.input_transform.backend,
                },
            );
        }
        let encoded_source_descriptor = ColorFrameDescriptor {
            width: contract.output_width,
            height: contract.output_height,
            color_space: contract.source_color_space.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::Opaque,
        };
        let encoded_source_frame = GpuColorFrameHandle::new(
            ids.allocate()?,
            encoded_source_descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
            format!("{}.encoded-source", contract.label),
        )
        .map_err(GpuNativeDecodedFrameImportPlanError::EncodedSourceFrameHandle)?;
        let working_descriptor = ColorFrameDescriptor {
            width: contract.output_width,
            height: contract.output_height,
            color_space: contract.input_transform.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        };
        let working_frame = GpuColorFrameHandle::new(
            ids.allocate()?,
            working_descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            contract.label,
        )
        .map_err(GpuNativeDecodedFrameImportPlanError::WorkingFrameHandle)?;

        Ok(Self {
            source_width: contract.width,
            source_height: contract.height,
            handle_kind: contract.handle_kind,
            source_texture_format: contract.source_texture_format,
            source_color_space: contract.source_color_space,
            input_transform: contract.input_transform,
            video_sampling: contract.video_sampling,
            encoded_source_frame,
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
    /// Requested renderer materialization extent was empty.
    #[error("native decoded frame output extent must be non-empty, got {width}x{height}")]
    EmptyOutputExtent {
        /// Invalid output width.
        width: u32,
        /// Invalid output height.
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
    /// Native decoded frames must use the renderer OCIO GPU input path.
    #[error("unsupported native decoded frame input transform backend {backend:?}")]
    UnsupportedInputTransformBackend {
        /// Backend that would violate native GPU residency or OCIO execution.
        backend: RenderColorTransformBackend,
    },
    /// The video sampling metadata cannot be used for this decoded surface.
    #[error("invalid native decoded frame video sampling contract for {source_texture_format:?}: {reason}")]
    InvalidVideoSampling {
        /// Decoded source texture format being sampled.
        source_texture_format: GpuNativeDecodedFrameTextureFormat,
        /// Stable validation reason.
        reason: String,
    },
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// The renderer-owned encoded source frame handle could not be built.
    #[error("failed to create native decoded frame encoded source handle: {0}")]
    EncodedSourceFrameHandle(GpuColorFrameHandleError),
    /// The renderer-owned working frame handle could not be built.
    #[error("failed to create native decoded frame working handle: {0}")]
    WorkingFrameHandle(GpuColorFrameHandleError),
}

/// Imported native decoded-frame output produced by a renderer backend.
#[derive(Debug)]
pub struct GpuNativeDecodedFrameImportExecution<R> {
    /// Validated import plan used for this execution.
    pub plan: GpuNativeDecodedFrameImportPlan,
    /// Renderer-owned resource containing the linear working frame.
    pub resource: GpuColorFrameResource<R>,
}

/// Renderer-visible facts for a native decoded-frame payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuNativeDecodedFrameSourceDescriptor {
    /// Source frame width in pixels.
    pub width: u32,
    /// Source frame height in pixels.
    pub height: u32,
    /// Decoder handle family carried by the native payload.
    pub handle_kind: DecodedGpuFrameHandleKind,
    /// Decoder source texture layout carried by the native payload.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
}

impl GpuNativeDecodedFrameSourceDescriptor {
    fn from_contract(contract: &GpuNativeDecodedFrameImportContract) -> Self {
        Self {
            width: contract.width,
            height: contract.height,
            handle_kind: contract.handle_kind,
            source_texture_format: contract.source_texture_format,
        }
    }
}

/// Native decoded-frame payload that can expose renderer-visible import facts.
pub trait GpuNativeDecodedFrameImportSource {
    /// Return the source descriptor carried by this native payload.
    fn native_decoded_frame_source_descriptor(
        &self,
    ) -> Result<GpuNativeDecodedFrameSourceDescriptor, GpuNativeDecodedFrameSourceFormatError>;
}

impl GpuNativeDecodedFrameImportSource for PreviewNativeDecodedFrame {
    fn native_decoded_frame_source_descriptor(
        &self,
    ) -> Result<GpuNativeDecodedFrameSourceDescriptor, GpuNativeDecodedFrameSourceFormatError> {
        Ok(GpuNativeDecodedFrameSourceDescriptor {
            width: self.width,
            height: self.height,
            handle_kind: self.handle_kind(),
            source_texture_format: self.surface_format.try_into()?,
        })
    }
}

/// Backend hook that imports one native decoded frame into a renderer resource.
///
/// Platform-specific implementations own the concrete native-frame payload and
/// backend resource type. The renderer-owned helper validates support,
/// constructs the working-frame plan, asks the backend to import the native
/// surface, then verifies the returned resource matches the planned working
/// frame handle identity and contract.
pub trait GpuNativeDecodedFrameImportBackend {
    /// Native decoded-frame payload consumed by this backend.
    type NativeFrame: GpuNativeDecodedFrameImportSource;
    /// Concrete renderer resource produced by this backend.
    type Resource;

    /// Advertised native decoded-frame import support.
    fn support(&self) -> &GpuNativeDecodedFrameImportSupport;

    /// Import the native decoded frame according to the validated plan.
    fn import_native_decoded_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &Self::NativeFrame,
    ) -> Result<GpuColorFrameResource<Self::Resource>, GpuNativeDecodedFrameImportError>;
}

/// Execute a native decoded-frame import through a renderer backend.
pub fn execute_native_decoded_frame_import<B>(
    backend: &mut B,
    ids: &mut GpuColorFrameIdAllocator,
    contract: GpuNativeDecodedFrameImportContract,
    native_frame: &B::NativeFrame,
) -> Result<GpuNativeDecodedFrameImportExecution<B::Resource>, GpuNativeDecodedFrameImportError>
where
    B: GpuNativeDecodedFrameImportBackend,
{
    let expected_source = GpuNativeDecodedFrameSourceDescriptor::from_contract(&contract);
    let actual_source = native_frame.native_decoded_frame_source_descriptor()?;
    if actual_source != expected_source {
        return Err(
            GpuNativeDecodedFrameImportError::NativeFrameContractMismatch {
                expected: expected_source,
                actual: actual_source,
            },
        );
    }
    let plan = GpuNativeDecodedFrameImportPlan::from_contract(ids, contract, backend.support())?;
    let resource = backend.import_native_decoded_frame(&plan, native_frame)?;
    if resource.handle().contract() != plan.working_frame.contract() {
        return Err(GpuNativeDecodedFrameImportError::ResourceContractMismatch {
            expected: plan.working_frame.contract(),
            actual: resource.handle().contract(),
        });
    }
    if resource.handle().id() != plan.working_frame.id() {
        return Err(GpuNativeDecodedFrameImportError::ResourceHandleMismatch {
            expected: plan.working_frame.id(),
            actual: resource.handle().id(),
        });
    }
    Ok(GpuNativeDecodedFrameImportExecution { plan, resource })
}

/// Error returned while executing native decoded-frame import.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeDecodedFrameImportError {
    /// Planning failed before backend execution.
    #[error(transparent)]
    Plan(#[from] GpuNativeDecodedFrameImportPlanError),
    /// The media payload could not produce a renderer source descriptor.
    #[error(transparent)]
    SourceFormat(#[from] GpuNativeDecodedFrameSourceFormatError),
    /// Every bounded native-import resource for this contract is still in flight.
    ///
    /// This is transient queue backpressure, not a capability or correctness
    /// failure. Callers should retain the last presented frame and retry or
    /// discard this candidate according to their scheduling policy.
    #[error("native decoded frame import is backpressured: {reason}")]
    Backpressure {
        /// Stable backend detail for telemetry and diagnosis.
        reason: String,
    },
    /// Backend rejected the native decoded frame.
    #[error("native decoded frame import backend rejected the frame: {reason}")]
    BackendRejected {
        /// Stable backend rejection reason.
        reason: String,
    },
    /// The native decoder device reported physical removal while observing a
    /// copy-ready fence.
    ///
    /// The backend clears only the source protected by that removed device;
    /// this is typed retirement proof, not a generic backend rejection.
    #[error("native decoded frame device was removed: {reason}")]
    NativeDeviceRemoved {
        /// Stable native-device diagnostic.
        reason: String,
    },
    /// The native payload does not match the import contract.
    #[error("native decoded frame payload does not match the import contract")]
    NativeFrameContractMismatch {
        /// Source descriptor required by the import contract.
        expected: GpuNativeDecodedFrameSourceDescriptor,
        /// Source descriptor reported by the native payload.
        actual: GpuNativeDecodedFrameSourceDescriptor,
    },
    /// Backend produced a resource that does not match the validated plan.
    #[error("native decoded frame import backend returned mismatched working resource")]
    ResourceContractMismatch {
        /// Planned working-frame contract.
        expected: GpuColorFrameContract,
        /// Actual returned resource contract.
        actual: GpuColorFrameContract,
    },
    /// Backend returned the right contract under a different renderer resource identity.
    #[error(
        "native decoded frame import backend returned resource id {actual:?}, expected {expected:?}"
    )]
    ResourceHandleMismatch {
        /// Renderer-owned identity allocated by the import plan.
        expected: GpuColorFrameId,
        /// Identity attached to the backend-returned resource.
        actual: GpuColorFrameId,
    },
}

impl GpuNativeDecodedFrameImportError {
    /// Whether this failure is transient bounded-resource backpressure.
    pub fn is_backpressure(&self) -> bool {
        matches!(self, Self::Backpressure { .. })
    }

    /// Whether a native fence returned the D3D device-removed sentinel.
    pub fn is_native_device_removed(&self) -> bool {
        matches!(self, Self::NativeDeviceRemoved { .. })
    }
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

    /// Create a readback plan for a `Rgba32Float` GPU texture.
    pub fn encoded_rgba32float(
        handle: GpuColorFrameHandle,
    ) -> Result<Self, GpuColorFrameReadbackError> {
        if handle.texture_format() != GpuColorFrameTextureFormat::Rgba32Float {
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
            texture_format: GpuColorFrameTextureFormat::Rgba32Float,
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
        let color_space = self.output_descriptor.color_space.color().ok_or(
            GpuColorFrameReadbackError::UnsupportedColorIdentity {
                color_space: self.output_descriptor.color_space,
            },
        )?;
        Ok(CpuEncodedColorFrame::rgba8(
            self.output_descriptor.width,
            self.output_descriptor.height,
            color_space,
            self.output_descriptor.domain,
            rgba,
        ))
    }

    /// Unpack a padded mapped readback buffer from an `Rgba16Float` texture
    /// into f32 RGBA samples while preserving the descriptor's encoding.
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

    /// Unpack a padded `Rgba32Float` mapped buffer into native f32 RGBA samples.
    pub fn unpack_mapped_rgba32float(
        &self,
        mapped: &[u8],
    ) -> Result<Vec<f32>, GpuColorFrameReadbackError> {
        if self.texture_format != GpuColorFrameTextureFormat::Rgba32Float {
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
            for component in 0..self.extent.width as usize * 4 {
                let offset = src_start + component * size_of::<f32>();
                let bytes = [
                    mapped[offset],
                    mapped[offset + 1],
                    mapped[offset + 2],
                    mapped[offset + 3],
                ];
                data.push(f32::from_le_bytes(bytes));
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
        resource: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
    ) -> Result<wgpu::Buffer, GpuColorFrameReadbackError> {
        if resource.handle().contract() != plan.handle.contract() {
            return Err(GpuColorFrameReadbackError::ResourceContractMismatch {
                expected: plan.handle.contract(),
                actual: resource.handle().contract(),
            });
        }
        if resource.handle().id() != plan.handle.id() {
            return Err(GpuColorFrameReadbackError::ResourceHandleMismatch {
                expected: plan.handle.id(),
                actual: resource.handle().id(),
            });
        }
        Ok(Self::record_copy_unchecked(
            device,
            encoder,
            plan,
            resource.resource(),
        ))
    }

    fn record_copy_unchecked(
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
    /// Encoded readback was requested for a linear working identity.
    UnsupportedColorIdentity {
        /// Actual frame color identity.
        color_space: ColorFrameSpace,
    },
    /// Resolved resource metadata differs from the readback plan.
    ResourceContractMismatch {
        /// Contract captured by the readback plan.
        expected: GpuColorFrameContract,
        /// Contract carried by the supplied resource.
        actual: GpuColorFrameContract,
    },
    /// Resolved resource belongs to another strong renderer frame identity.
    ResourceHandleMismatch {
        /// Identity captured by the readback plan.
        expected: GpuColorFrameId,
        /// Identity carried by the supplied resource.
        actual: GpuColorFrameId,
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GpuColorFrameResourceTableError {
    /// No resource exists for the requested frame id.
    #[error("GPU color frame resource {id:?} is missing")]
    MissingFrame {
        /// Missing frame id.
        id: GpuColorFrameId,
    },
    /// A resource id exists but its descriptor or texture format no longer matches.
    #[error(
        "GPU color frame resource {id:?} contract mismatch: expected {expected:?}, actual {actual:?}"
    )]
    ContractMismatch {
        /// Resource id that mismatched.
        id: GpuColorFrameId,
        /// Contract expected by the caller.
        expected: GpuColorFrameContract,
        /// Contract stored in the resource table.
        actual: GpuColorFrameContract,
    },
}

/// Renderer-internal CPU alpha matte awaiting typed GPU upload.
///
/// This stays distinct from [`CpuColorFrame`]: its samples have no color-space
/// identity and may only become an `AlphaMask + NonColorData` resource.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CpuAlphaMaskFrame {
    width: u32,
    height: u32,
    samples: Arc<Vec<[f32; 4]>>,
}

impl CpuAlphaMaskFrame {
    pub(crate) fn new(width: u32, height: u32, samples: Vec<[f32; 4]>) -> Self {
        Self { width, height, samples: Arc::new(samples) }
    }

    const fn descriptor(&self) -> ColorFrameDescriptor {
        ColorFrameDescriptor {
            width: self.width,
            height: self.height,
            color_space: ColorFrameSpace::NonColorData,
            domain: ColorFrameDomain::AlphaMask,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        }
    }
}

/// CPU-resident linear floating-point frame with a typed color contract.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuColorFrame {
    descriptor: ColorFrameDescriptor,
    frame: Arc<WorkingRgbaF32Frame>,
}

impl CpuColorFrame {
    /// Wrap a linear-light frame as a working-space render-graph frame.
    pub fn working(frame: WorkingRgbaF32Frame) -> Self {
        let descriptor = ColorFrameDescriptor {
            width: frame.width,
            height: frame.height,
            color_space: ColorFrameSpace::Working(frame.color_space),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        };
        Self { descriptor, frame: Arc::new(frame) }
    }

    /// Return the frame metadata contract.
    pub fn descriptor(&self) -> ColorFrameDescriptor {
        self.descriptor
    }

    /// Borrow the underlying linear-light frame.
    pub fn rgba_f32(&self) -> &WorkingRgbaF32Frame {
        self.frame.as_ref()
    }

    /// Clone the shared immutable linear-light frame backing this wrapper.
    pub fn rgba_f32_shared(&self) -> Arc<WorkingRgbaF32Frame> {
        Arc::clone(&self.frame)
    }

    /// Return whether two typed frames share the same immutable pixel storage.
    ///
    /// This is execution evidence for zero-copy routing. Pixel equality alone
    /// cannot prove that a compositor or output adapter avoided allocating and
    /// copying a frame.
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frame, &other.frame)
    }

    /// Consume this wrapper and return the underlying linear-light frame.
    pub fn into_rgba_f32(self) -> WorkingRgbaF32Frame {
        Arc::try_unwrap(self.frame).unwrap_or_else(|frame| frame.as_ref().clone())
    }
}

/// CPU-resident color-space-encoded floating-point frame at a graph boundary.
///
/// This type is intentionally distinct from [`CpuColorFrame`]: its RGB samples
/// have already crossed an input, display, or export transfer function and
/// therefore must not participate in linear-light compositing.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuEncodedFloatColorFrame {
    descriptor: ColorFrameDescriptor,
    frame: Arc<EncodedRgbaF32Frame>,
}

/// Encoded RGBA f32 samples at a source, display, or export boundary.
///
/// Unlike [`WorkingRgbaF32Frame`], these RGB values are not linear light and cannot be
/// consumed by effects or compositing APIs.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedRgbaF32Frame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Transfer-encoded RGBA values. Alpha remains linear coverage.
    pub data: Vec<[f32; 4]>,
    /// Encoded source/display/delivery color-space identity.
    pub color_space: ColorSpace,
}

impl CpuEncodedFloatColorFrame {
    /// Wrap encoded float pixels at an explicit source/display/export boundary.
    pub fn new(frame: EncodedRgbaF32Frame, domain: ColorFrameDomain) -> Self {
        let descriptor = ColorFrameDescriptor {
            width: frame.width,
            height: frame.height,
            color_space: ColorFrameSpace::Color(frame.color_space),
            domain,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        };
        Self { descriptor, frame: Arc::new(frame) }
    }

    /// Create a source/import encoded-float frame from interleaved RGBA samples.
    pub fn source_flat_rgba_f32(
        width: u32,
        height: u32,
        color_space: ColorSpace,
        data: Vec<f32>,
    ) -> Self {
        assert_eq!(
            data.len(),
            width as usize * height as usize * 4,
            "encoded source data length must be width * height * 4"
        );
        // `[f32; 4]` has the same alignment as `f32`; the validated component
        // count makes this an allocation-preserving ownership cast rather than
        // a second full-frame copy at the 4K software-decode boundary.
        let data = bytemuck::allocation::cast_vec(data);
        Self::new(
            EncodedRgbaF32Frame { width, height, data, color_space },
            ColorFrameDomain::Source,
        )
    }

    /// Return the frame metadata contract.
    pub fn descriptor(&self) -> ColorFrameDescriptor {
        self.descriptor
    }

    /// Borrow the underlying encoded floating-point samples.
    pub fn rgba_f32(&self) -> &EncodedRgbaF32Frame {
        self.frame.as_ref()
    }

    /// Clone the shared immutable encoded floating-point frame backing this wrapper.
    pub fn rgba_f32_shared(&self) -> Arc<EncodedRgbaF32Frame> {
        Arc::clone(&self.frame)
    }

    /// Consume this wrapper and return the encoded floating-point samples.
    pub fn into_rgba_f32(self) -> EncodedRgbaF32Frame {
        Arc::try_unwrap(self.frame).unwrap_or_else(|frame| frame.as_ref().clone())
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
            color_space: color_space.into(),
            domain,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
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

    /// Return shared RGBA8 pixels.
    pub fn rgba_shared(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.rgba)
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
    data: Arc<Vec<f32>>,
}

impl LinearFloatSource {
    /// Create a linear float source frame.
    pub fn new(
        width: u32,
        height: u32,
        color_space: impl Into<ColorFrameSpace>,
        data: Vec<f32>,
    ) -> Self {
        Self::new_shared(width, height, color_space, Arc::new(data))
    }

    /// Create a linear float source frame from shared immutable samples.
    pub fn new_shared(
        width: u32,
        height: u32,
        color_space: impl Into<ColorFrameSpace>,
        data: Arc<Vec<f32>>,
    ) -> Self {
        assert_eq!(
            data.len(),
            width as usize * height as usize * 4,
            "LinearFloatSource data length must be width * height * 4"
        );
        let color_space = color_space.into();
        assert!(
            matches!(color_space, ColorFrameSpace::Working(_))
                || color_space.color().is_some_and(|space| {
                    space.encoding().kind == mondrian_core::ColorEncodingKind::SceneLinear
                }),
            "LinearFloatSource requires a scene-linear external or working identity"
        );
        let descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space,
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
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
        self.data.as_slice()
    }

    /// Return the shared immutable RGBA f32 samples.
    pub fn data_shared(&self) -> Arc<Vec<f32>> {
        Arc::clone(&self.data)
    }

    /// Consume this wrapper and return RGBA f32 pixels.
    pub fn into_data(self) -> Vec<f32> {
        Arc::try_unwrap(self.data).unwrap_or_else(|data| data.as_ref().clone())
    }

    /// Convert to a working-space [`CpuColorFrame`] without any u8
    /// quantization. The caller must ensure the data is already in the target
    /// working color space.
    pub fn to_working_frame(self, working_color_space: WorkingColorSpace) -> CpuColorFrame {
        let pixels: Vec<[f32; 4]> =
            self.data.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let frame = WorkingRgbaF32Frame {
            width: self.descriptor.width,
            height: self.descriptor.height,
            data: pixels,
            color_space: working_color_space,
        };
        CpuColorFrame::working(frame)
    }
}

/// CPU-decoded source frame accepted by the renderer input-color boundary.
#[derive(Debug, Clone)]
pub enum CpuSourceColorFrame {
    /// Transfer-encoded 8-bit RGBA samples.
    EncodedRgba8(CpuEncodedColorFrame),
    /// Transfer-encoded floating-point RGBA samples.
    EncodedFloat(CpuEncodedFloatColorFrame),
    /// Scene-linear floating-point RGBA samples.
    LinearFloat(LinearFloatSource),
}

/// Failure while normalizing decoded source alpha to straight coverage.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SourceAlphaInterpretationError {
    /// A floating-point decoder returned invalid coverage.
    #[error(
        "invalid decoded alpha {value} at pixel {pixel_index}; expected finite coverage in [0, 1]"
    )]
    InvalidFloatCoverage {
        /// Pixel containing invalid coverage.
        pixel_index: usize,
        /// Invalid decoded value.
        value: f32,
    },
}

impl CpuSourceColorFrame {
    /// Return the exact source-boundary descriptor.
    pub fn descriptor(&self) -> ColorFrameDescriptor {
        match self {
            Self::EncodedRgba8(frame) => frame.descriptor(),
            Self::EncodedFloat(frame) => frame.descriptor(),
            Self::LinearFloat(frame) => frame.descriptor(),
        }
    }

    /// Return CPU bytes retained by the decoded source payload.
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::EncodedRgba8(frame) => frame.rgba().len(),
            Self::EncodedFloat(frame) => {
                frame.rgba_f32().data.len().saturating_mul(std::mem::size_of::<[f32; 4]>())
            }
            Self::LinearFloat(frame) => {
                frame.data().len().saturating_mul(std::mem::size_of::<f32>())
            }
        }
    }

    /// Apply the user/source alpha interpretation before any RGB color transform.
    ///
    /// The renderer's working contract is straight coverage. Premultiplied
    /// stored RGB is therefore unassociated in its source encoding, while
    /// `Ignore` makes the source fully opaque. Straight sources retain their
    /// shared payload without a copy.
    pub fn normalize_alpha(
        self,
        interpretation: AlphaInterpretation,
    ) -> Result<Self, SourceAlphaInterpretationError> {
        match (self, interpretation) {
            (mut frame, AlphaInterpretation::Straight) => {
                frame.set_alpha_contract(ColorFrameAlpha::StraightCoverage);
                Ok(frame)
            }
            (Self::EncodedRgba8(frame), interpretation) => {
                let mut descriptor = frame.descriptor;
                descriptor.alpha = match interpretation {
                    AlphaInterpretation::Ignore => ColorFrameAlpha::Opaque,
                    AlphaInterpretation::Straight | AlphaInterpretation::Premultiplied => {
                        ColorFrameAlpha::StraightCoverage
                    }
                };
                let mut rgba = frame.into_rgba();
                normalize_rgba8_alpha(&mut rgba, interpretation);
                let frame = CpuEncodedColorFrame { descriptor, rgba: Arc::new(rgba) };
                Ok(Self::EncodedRgba8(frame))
            }
            (Self::EncodedFloat(frame), interpretation) => {
                let mut descriptor = frame.descriptor;
                descriptor.alpha = match interpretation {
                    AlphaInterpretation::Ignore => ColorFrameAlpha::Opaque,
                    AlphaInterpretation::Straight | AlphaInterpretation::Premultiplied => {
                        ColorFrameAlpha::StraightCoverage
                    }
                };
                let mut rgba = frame.into_rgba_f32();
                normalize_rgba_f32_alpha(rgba.data.as_flattened_mut(), interpretation)?;
                let frame = CpuEncodedFloatColorFrame { descriptor, frame: Arc::new(rgba) };
                Ok(Self::EncodedFloat(frame))
            }
            (Self::LinearFloat(frame), interpretation) => {
                let mut descriptor = frame.descriptor;
                descriptor.alpha = match interpretation {
                    AlphaInterpretation::Ignore => ColorFrameAlpha::Opaque,
                    AlphaInterpretation::Straight | AlphaInterpretation::Premultiplied => {
                        ColorFrameAlpha::StraightCoverage
                    }
                };
                let mut rgba = frame.into_data();
                normalize_rgba_f32_alpha(&mut rgba, interpretation)?;
                let frame = LinearFloatSource { descriptor, data: Arc::new(rgba) };
                Ok(Self::LinearFloat(frame))
            }
        }
    }

    fn set_alpha_contract(&mut self, alpha: ColorFrameAlpha) {
        match self {
            Self::EncodedRgba8(frame) => frame.descriptor.alpha = alpha,
            Self::EncodedFloat(frame) => frame.descriptor.alpha = alpha,
            Self::LinearFloat(frame) => frame.descriptor.alpha = alpha,
        }
    }
}

fn normalize_rgba8_alpha(rgba: &mut [u8], interpretation: AlphaInterpretation) {
    for pixel in rgba.chunks_exact_mut(4) {
        match interpretation {
            AlphaInterpretation::Straight => {}
            AlphaInterpretation::Ignore => pixel[3] = u8::MAX,
            AlphaInterpretation::Premultiplied => {
                let alpha = u32::from(pixel[3]);
                if alpha == 0 {
                    pixel[..3].fill(0);
                } else if alpha < u32::from(u8::MAX) {
                    for channel in &mut pixel[..3] {
                        let straight =
                            (u32::from(*channel) * u32::from(u8::MAX) + alpha / 2) / alpha;
                        *channel = straight.min(u32::from(u8::MAX)) as u8;
                    }
                }
            }
        }
    }
}

fn normalize_rgba_f32_alpha(
    rgba: &mut [f32],
    interpretation: AlphaInterpretation,
) -> Result<(), SourceAlphaInterpretationError> {
    for (pixel_index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        match interpretation {
            AlphaInterpretation::Straight => {}
            AlphaInterpretation::Ignore => pixel[3] = 1.0,
            AlphaInterpretation::Premultiplied => {
                let alpha = pixel[3];
                if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
                    return Err(SourceAlphaInterpretationError::InvalidFloatCoverage {
                        pixel_index,
                        value: alpha,
                    });
                }
                if alpha <= f32::EPSILON {
                    pixel[..3].fill(0.0);
                } else if alpha < 1.0 {
                    for channel in &mut pixel[..3] {
                        *channel /= alpha;
                    }
                }
            }
        }
    }
    Ok(())
}

impl From<CpuEncodedColorFrame> for CpuSourceColorFrame {
    fn from(frame: CpuEncodedColorFrame) -> Self {
        Self::EncodedRgba8(frame)
    }
}

impl From<CpuEncodedFloatColorFrame> for CpuSourceColorFrame {
    fn from(frame: CpuEncodedFloatColorFrame) -> Self {
        Self::EncodedFloat(frame)
    }
}

impl From<LinearFloatSource> for CpuSourceColorFrame {
    fn from(frame: LinearFloatSource) -> Self {
        Self::LinearFloat(frame)
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
    fn source_alpha_normalization_unassociates_encoded_premultiplied_rgb() {
        let source = CpuSourceColorFrame::from(CpuEncodedColorFrame::source_rgba8(
            1,
            1,
            ColorSpace::Rec709,
            vec![64, 32, 16, 128],
        ));

        let normalized = source
            .normalize_alpha(AlphaInterpretation::Premultiplied)
            .expect("valid premultiplied source");
        let CpuSourceColorFrame::EncodedRgba8(normalized) = normalized else {
            panic!("encoded source must stay encoded");
        };

        assert_eq!(normalized.rgba(), &[128, 64, 32, 128]);
        assert_eq!(
            normalized.descriptor().alpha,
            ColorFrameAlpha::StraightCoverage
        );
    }

    #[test]
    fn source_alpha_normalization_preserves_extended_float_rgb_and_coverage() {
        let source = CpuSourceColorFrame::from(LinearFloatSource::new(
            1,
            1,
            ColorSpace::LinearRec2020,
            vec![0.5, -0.125, 0.0625, 0.25],
        ));

        let normalized = source
            .normalize_alpha(AlphaInterpretation::Premultiplied)
            .expect("valid premultiplied float source");
        let CpuSourceColorFrame::LinearFloat(normalized) = normalized else {
            panic!("float source must stay float");
        };

        assert_eq!(normalized.data(), &[2.0, -0.5, 0.25, 0.25]);
        assert_eq!(
            normalized.descriptor().alpha,
            ColorFrameAlpha::StraightCoverage
        );
    }

    #[test]
    fn source_alpha_ignore_makes_coverage_opaque_without_changing_rgb() {
        let source = CpuSourceColorFrame::from(CpuEncodedColorFrame::source_rgba8(
            1,
            1,
            ColorSpace::Rec709,
            vec![12, 34, 56, 78],
        ));

        let normalized = source
            .normalize_alpha(AlphaInterpretation::Ignore)
            .expect("encoded coverage is always valid");
        let CpuSourceColorFrame::EncodedRgba8(normalized) = normalized else {
            panic!("encoded source must stay encoded");
        };

        assert_eq!(normalized.rgba(), &[12, 34, 56, 255]);
        assert_eq!(normalized.descriptor().alpha, ColorFrameAlpha::Opaque);
    }

    #[test]
    fn color_frame_space_keeps_full_device_identity_in_bounded_inline_storage() {
        assert_eq!(
            std::mem::size_of::<DisplayCalibrationKey>(),
            32,
            "device-frame equality must retain the complete calibration identity"
        );
        assert!(
            std::mem::size_of::<ColorFrameSpace>() <= 40,
            "the full calibration identity must remain inline and bounded"
        );
    }

    #[test]
    fn gpu_color_frame_handle_requires_gpu_residency() {
        let descriptor = ColorFrameDescriptor {
            width: 1920,
            height: 1080,
            color_space: ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
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
            color_space: ColorSpace::Rec2020.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
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
        let mut allocator = GpuColorFrameIdAllocator::new(40).expect("frame id allocator");

        assert_eq!(allocator.allocate().expect("frame 40").raw(), 40);
        assert_eq!(allocator.allocate().expect("frame 41").raw(), 41);
        assert_eq!(allocator.next_raw(), 42);
    }

    #[test]
    fn gpu_color_frame_id_includes_allocator_authority() {
        let mut left = GpuColorFrameIdAllocator::new(40).expect("left frame id allocator");
        let mut right = GpuColorFrameIdAllocator::new(40).expect("right frame id allocator");

        let left_id = left.allocate().expect("left frame id");
        let right_id = right.allocate().expect("right frame id");

        assert_eq!(left_id.raw(), right_id.raw());
        assert_ne!(
            left_id.allocator_authority(),
            right_id.allocator_authority()
        );
        assert_ne!(left_id, right_id);
    }

    #[test]
    fn gpu_color_frame_id_allocator_fails_closed_before_u64_reuse() {
        let mut allocator =
            GpuColorFrameIdAllocator::new(u64::MAX - 1).expect("boundary frame id allocator");
        let authority = allocator.authority();

        assert_eq!(
            allocator.allocate().expect("last unique frame id").raw(),
            u64::MAX - 1
        );
        assert_eq!(allocator.next_raw(), u64::MAX);
        assert!(allocator.is_exhausted());
        assert_eq!(
            allocator.allocate(),
            Err(GpuColorFrameIdAllocationError::SequenceExhausted { authority })
        );
        assert_eq!(
            allocator.allocate(),
            Err(GpuColorFrameIdAllocationError::SequenceExhausted { authority })
        );
        assert_eq!(allocator.next_raw(), u64::MAX);
    }

    #[test]
    fn gpu_color_frame_bind_group_cache_key_fails_closed_before_u64_reuse() {
        let sequence = AtomicU64::new(u64::MAX - 1);

        assert_eq!(
            allocate_gpu_color_frame_bind_group_cache_key(&sequence)
                .expect("last unique cache key")
                .0,
            u64::MAX - 1
        );
        assert_eq!(sequence.load(Ordering::Acquire), u64::MAX);
        assert_eq!(
            allocate_gpu_color_frame_bind_group_cache_key(&sequence),
            Err(GpuColorFrameBindGroupCacheKeyAllocationError)
        );
        assert_eq!(
            allocate_gpu_color_frame_bind_group_cache_key(&sequence),
            Err(GpuColorFrameBindGroupCacheKeyAllocationError)
        );
        assert_eq!(sequence.load(Ordering::Acquire), u64::MAX);
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
    fn native_decoded_frame_texture_format_maps_media_native_surfaces() {
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::try_from(DecodedVideoSurfaceFormat::Nv12),
            Ok(GpuNativeDecodedFrameTextureFormat::Nv12)
        );
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::try_from(DecodedVideoSurfaceFormat::P010),
            Ok(GpuNativeDecodedFrameTextureFormat::P010)
        );
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::try_from(DecodedVideoSurfaceFormat::Rgba8),
            Ok(GpuNativeDecodedFrameTextureFormat::Rgba8Unorm)
        );
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::try_from(DecodedVideoSurfaceFormat::Bgra8),
            Ok(GpuNativeDecodedFrameTextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            GpuNativeDecodedFrameTextureFormat::try_from(DecodedVideoSurfaceFormat::Yuv420p),
            Err(GpuNativeDecodedFrameSourceFormatError::Unsupported {
                format: DecodedVideoSurfaceFormat::Yuv420p,
            })
        );
    }

    #[test]
    fn native_decoded_frame_import_defaults_to_fail_closed() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
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
    fn native_decoded_frame_import_unavailable_support_preserves_backend_reason() {
        let support = GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
            "Dx12",
            "D3D11 shared texture import bridge is not connected",
        );

        assert!(!support.renderer_backend_ready);
        assert_eq!(support.renderer_backend_label.as_deref(), Some("Dx12"));
        assert_eq!(
            support.unavailable_reason.as_deref(),
            Some("D3D11 shared texture import bridge is not connected")
        );
        assert!(!support.supports_handle_kind(DecodedGpuFrameHandleKind::D3D11Texture2D));
    }

    #[test]
    fn native_decoded_frame_import_rejects_unsupported_handle_kind() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
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
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
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
    fn native_decoded_frame_import_requires_gpu_ocio_input_transform() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );
        let mut contract = native_import_contract();
        contract.input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec2020,
            true,
            mondrian_core::types::ColorEngine::mondrian_standard(),
        );

        let err = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect_err("native input must not route through a CPU OCIO boundary");

        assert_eq!(
            err,
            GpuNativeDecodedFrameImportPlanError::UnsupportedInputTransformBackend {
                backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
            }
        );
        assert_eq!(ids.next_raw(), 500);
    }

    #[test]
    fn native_decoded_frame_import_plan_produces_renderer_owned_working_frame() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
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
        assert_eq!((plan.source_width, plan.source_height), (3840, 2160));
        assert_eq!(
            plan.input_transform,
            RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec2020,
                true,
                mondrian_core::types::ColorEngine::mondrian_standard(),
            )
        );
        assert_eq!(
            plan.video_sampling,
            GpuNativeDecodedFrameVideoSampling {
                range: GpuVideoRange::Limited,
                matrix: ColorMatrixCoefficients::Bt2020NonConstant,
                transfer: ColorTransferCharacteristic::Pq,
                bit_depth: 8,
                chroma_location: GpuVideoChromaLocation::Left,
            }
        );
        assert_eq!(plan.encoded_source_frame.id().raw(), 500);
        assert_eq!(
            plan.encoded_source_frame.descriptor(),
            ColorFrameDescriptor {
                width: 960,
                height: 540,
                color_space: ColorSpace::Rec2100Pq.into(),
                domain: ColorFrameDomain::Source,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::Opaque,
            }
        );
        assert_eq!(
            plan.encoded_source_frame.texture_format(),
            GpuColorFrameTextureFormat::Rgba16Float
        );
        assert_eq!(plan.working_frame.id().raw(), 501);
        assert_eq!(
            plan.working_frame.descriptor(),
            ColorFrameDescriptor {
                width: 960,
                height: 540,
                color_space: WorkingColorSpace::LinearRec2020.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::StraightCoverage,
            }
        );
        assert_eq!(
            plan.working_frame.texture_format(),
            GpuColorFrameTextureFormat::Rgba32Float
        );
        assert_eq!(ids.next_raw(), 502);
    }

    #[test]
    fn native_decoded_frame_import_preserves_decoder_matrix_independent_of_rgb_space() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );
        let mut contract = native_import_contract();
        contract.video_sampling.matrix = ColorMatrixCoefficients::Bt709;

        let plan = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect("decoder matrix describes YCbCr sampling, not encoded RGB primaries");

        assert_eq!(plan.video_sampling.matrix, ColorMatrixCoefficients::Bt709);
        assert_eq!(ids.next_raw(), 502);
    }

    #[test]
    fn native_decoded_frame_import_rejects_sampling_transfer_color_space_mismatch() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );
        let mut contract = native_import_contract();
        contract.video_sampling.transfer = ColorTransferCharacteristic::Hlg;

        let err = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect_err("native sampling transfer must match the source color space");

        match err {
            GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling { reason, .. } => {
                assert!(reason.contains("sampling transfer"));
                assert!(reason.contains("Rec2100Pq"));
                assert!(reason.contains("Pq"));
            }
            other => panic!("expected invalid video sampling, got {other:?}"),
        }
        assert_eq!(ids.next_raw(), 500);
    }

    #[test]
    fn native_decoded_frame_import_rejects_p010_with_wrong_bit_depth() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::P010],
        );
        let mut contract = native_import_contract();
        contract.source_texture_format = GpuNativeDecodedFrameTextureFormat::P010;

        let err = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect_err("P010 must not inherit NV12 8-bit sampling metadata");

        match err {
            GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling {
                source_texture_format,
                reason,
            } => {
                assert_eq!(
                    source_texture_format,
                    GpuNativeDecodedFrameTextureFormat::P010
                );
                assert!(reason.contains("10-bit"));
                assert!(reason.contains("8"));
            }
            other => panic!("expected invalid video sampling, got {other:?}"),
        }
        assert_eq!(ids.next_raw(), 500);
    }

    #[test]
    fn native_decoded_frame_import_rejects_ycbcr_without_chroma_location() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        );
        let mut contract = native_import_contract();
        contract.video_sampling.chroma_location = GpuVideoChromaLocation::Unspecified;

        let err = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect_err("native YCbCr sampling must fail closed without chroma siting");

        match err {
            GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling { reason, .. } => {
                assert!(reason.contains("chroma location"));
            }
            other => panic!("expected invalid video sampling, got {other:?}"),
        }
    }

    #[test]
    fn native_decoded_frame_import_rejects_rgb_surface_with_ycbcr_matrix() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Rgba8Unorm],
        );
        let mut contract = native_import_contract();
        contract.source_texture_format = GpuNativeDecodedFrameTextureFormat::Rgba8Unorm;

        let err = GpuNativeDecodedFrameImportPlan::from_contract(&mut ids, contract, &support)
            .expect_err("RGB native surfaces must not use a YCbCr matrix");

        match err {
            GpuNativeDecodedFrameImportPlanError::InvalidVideoSampling { reason, .. } => {
                assert!(reason.contains("RGB matrix"));
            }
            other => panic!("expected invalid video sampling, got {other:?}"),
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeNativeDecodedFrame {
        width: u32,
        height: u32,
        handle_kind: DecodedGpuFrameHandleKind,
        source_texture_format: GpuNativeDecodedFrameTextureFormat,
    }

    impl FakeNativeDecodedFrame {
        fn matching_contract() -> Self {
            Self {
                width: 3840,
                height: 2160,
                handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
                source_texture_format: GpuNativeDecodedFrameTextureFormat::Nv12,
            }
        }

        fn mismatched_extent() -> Self {
            Self { width: 1920, ..Self::matching_contract() }
        }
    }

    impl GpuNativeDecodedFrameImportSource for FakeNativeDecodedFrame {
        fn native_decoded_frame_source_descriptor(
            &self,
        ) -> Result<GpuNativeDecodedFrameSourceDescriptor, GpuNativeDecodedFrameSourceFormatError>
        {
            Ok(GpuNativeDecodedFrameSourceDescriptor {
                width: self.width,
                height: self.height,
                handle_kind: self.handle_kind,
                source_texture_format: self.source_texture_format,
            })
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeImportedResource;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeNativeImportResult {
        Exact,
        MismatchedContract,
        MismatchedIdentity,
    }

    struct FakeNativeImportBackend {
        support: GpuNativeDecodedFrameImportSupport,
        result: FakeNativeImportResult,
    }

    impl FakeNativeImportBackend {
        fn ready() -> Self {
            Self {
                support: GpuNativeDecodedFrameImportSupport::ready_zero_copy(
                    vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
                    vec![GpuNativeDecodedFrameTextureFormat::Nv12],
                ),
                result: FakeNativeImportResult::Exact,
            }
        }
    }

    impl GpuNativeDecodedFrameImportBackend for FakeNativeImportBackend {
        type NativeFrame = FakeNativeDecodedFrame;
        type Resource = FakeImportedResource;

        fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
            &self.support
        }

        fn import_native_decoded_frame(
            &mut self,
            plan: &GpuNativeDecodedFrameImportPlan,
            _native_frame: &Self::NativeFrame,
        ) -> Result<GpuColorFrameResource<Self::Resource>, GpuNativeDecodedFrameImportError>
        {
            let handle = match self.result {
                FakeNativeImportResult::Exact => plan.working_frame.clone(),
                FakeNativeImportResult::MismatchedContract => gpu_handle(
                    999,
                    ColorFrameDescriptor {
                        width: 1280,
                        height: 720,
                        ..plan.working_frame.descriptor()
                    },
                    plan.working_frame.texture_format(),
                ),
                FakeNativeImportResult::MismatchedIdentity => gpu_handle(
                    999,
                    plan.working_frame.descriptor(),
                    plan.working_frame.texture_format(),
                ),
            };
            Ok(GpuColorFrameResource::new(handle, FakeImportedResource))
        }
    }

    #[test]
    fn native_decoded_frame_import_execution_fails_closed_without_backend_support() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let mut backend = FakeNativeImportBackend {
            support: GpuNativeDecodedFrameImportSupport::unavailable(),
            result: FakeNativeImportResult::Exact,
        };

        let err = execute_native_decoded_frame_import(
            &mut backend,
            &mut ids,
            native_import_contract(),
            &FakeNativeDecodedFrame::matching_contract(),
        )
        .expect_err("unavailable backend must fail before execution");

        assert_eq!(
            err,
            GpuNativeDecodedFrameImportError::Plan(
                GpuNativeDecodedFrameImportPlanError::RendererBackendUnavailable
            )
        );
        assert_eq!(ids.next_raw(), 500);
    }

    #[test]
    fn native_decoded_frame_import_execution_returns_validated_working_resource() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let mut backend = FakeNativeImportBackend::ready();

        let execution = execute_native_decoded_frame_import(
            &mut backend,
            &mut ids,
            native_import_contract(),
            &FakeNativeDecodedFrame::matching_contract(),
        )
        .expect("ready backend can import a native frame");

        assert_eq!(execution.plan.encoded_source_frame.id().raw(), 500);
        assert_eq!(execution.plan.working_frame.id().raw(), 501);
        assert_eq!(execution.resource.handle(), &execution.plan.working_frame);
        assert_eq!(execution.resource.resource(), &FakeImportedResource);
        assert_eq!(ids.next_raw(), 502);
    }

    #[test]
    fn native_decoded_frame_import_execution_rejects_mismatched_backend_resource() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let mut backend = FakeNativeImportBackend::ready();
        backend.result = FakeNativeImportResult::MismatchedContract;

        let err = execute_native_decoded_frame_import(
            &mut backend,
            &mut ids,
            native_import_contract(),
            &FakeNativeDecodedFrame::matching_contract(),
        )
        .expect_err("backend must return the planned working resource");

        match err {
            GpuNativeDecodedFrameImportError::ResourceContractMismatch { expected, actual } => {
                assert_eq!(expected.descriptor.width, 960);
                assert_eq!(actual.descriptor.width, 1280);
                assert_eq!(
                    expected.texture_format,
                    GpuColorFrameTextureFormat::Rgba32Float
                );
                assert_eq!(
                    actual.texture_format,
                    GpuColorFrameTextureFormat::Rgba32Float
                );
            }
            other => panic!("expected resource contract mismatch, got {other:?}"),
        }
    }

    #[test]
    fn native_decoded_frame_import_rejects_same_contract_under_another_resource_id() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let mut backend = FakeNativeImportBackend::ready();
        backend.result = FakeNativeImportResult::MismatchedIdentity;

        let err = execute_native_decoded_frame_import(
            &mut backend,
            &mut ids,
            native_import_contract(),
            &FakeNativeDecodedFrame::matching_contract(),
        )
        .expect_err("backend cannot substitute a same-contract resource identity");

        match err {
            GpuNativeDecodedFrameImportError::ResourceHandleMismatch { expected, actual } => {
                assert_eq!(expected.raw(), 501);
                assert_eq!(actual.raw(), 999);
                assert_ne!(expected, actual);
            }
            other => panic!("expected resource handle mismatch, got {other:?}"),
        }
    }

    #[test]
    fn native_decoded_frame_import_execution_rejects_mismatched_native_payload() {
        let mut ids = GpuColorFrameIdAllocator::new(500).expect("frame id allocator");
        let mut backend = FakeNativeImportBackend::ready();

        let err = execute_native_decoded_frame_import(
            &mut backend,
            &mut ids,
            native_import_contract(),
            &FakeNativeDecodedFrame::mismatched_extent(),
        )
        .expect_err("native payload must match import contract before backend execution");

        match err {
            GpuNativeDecodedFrameImportError::NativeFrameContractMismatch { expected, actual } => {
                assert_eq!(expected.width, 3840);
                assert_eq!(actual.width, 1920);
                assert_eq!(expected.height, actual.height);
                assert_eq!(expected.handle_kind, actual.handle_kind);
                assert_eq!(expected.source_texture_format, actual.source_texture_format);
            }
            other => panic!("expected native frame contract mismatch, got {other:?}"),
        }
        assert_eq!(ids.next_raw(), 500);
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
        stale_descriptor.color_space = ColorSpace::Rec2020.into();
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
    fn gpu_color_frame_resource_table_take_moves_only_an_exact_contract() {
        let handle = gpu_handle(
            106,
            working_descriptor(),
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let mut mismatched_descriptor = working_descriptor();
        mismatched_descriptor.color_space = ColorSpace::Rec2020.into();
        let mismatched_handle = gpu_handle(
            106,
            mismatched_descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(
                handle.clone(),
                "presentation-texture",
            ))
            .expect("insert presentation resource");

        let error = table
            .take(&mismatched_handle)
            .expect_err("a mismatched contract must not transfer ownership");
        assert_eq!(
            error,
            GpuColorFrameResourceTableError::ContractMismatch {
                id: handle.id(),
                expected: mismatched_handle.contract(),
                actual: handle.contract(),
            }
        );
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get(&handle).expect("failed take preserves the resource").resource(),
            &"presentation-texture"
        );

        let resource = table.take(&handle).expect("exact take succeeds");
        assert_eq!(resource.handle(), &handle);
        assert_eq!(resource.resource(), &"presentation-texture");
        assert!(table.is_empty());
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
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            color_space: WorkingColorSpace::LinearRec709,
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
        assert_eq!(plan.bytes().len(), 2 * 16);
        let floats: &[f32] = bytemuck::cast_slice(plan.bytes());
        assert_eq!(floats, &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 0.5]);
        let GpuColorFrameUploadPayload::WorkingRgba32(payload) = &plan.payload else {
            panic!("CPU working upload should retain the shared frame payload");
        };
        assert!(Arc::ptr_eq(payload, &frame.frame));
    }

    #[test]
    fn alpha_mask_upload_retains_non_color_domain_identity() {
        let frame =
            CpuAlphaMaskFrame::new(2, 1, vec![[0.0, 0.0, 0.0, 0.25], [0.0, 0.0, 0.0, 0.75]]);
        assert_eq!(frame.descriptor().domain, ColorFrameDomain::AlphaMask);
        assert_eq!(
            frame.descriptor().color_space,
            ColorFrameSpace::NonColorData
        );
        assert_eq!(frame.descriptor().color_space.color(), None);
        assert_eq!(frame.descriptor().color_space.working(), None);

        let plan = GpuColorFrameUploadPlan::from_cpu_alpha_mask_frame(
            GpuColorFrameId::from_raw(205),
            &frame,
            "alpha-mask-upload",
        )
        .expect("alpha-mask upload plan");
        assert_eq!(
            plan.handle.descriptor(),
            frame.descriptor().with_residency(ColorFrameResidency::Gpu)
        );

        for incoherent in [
            ColorFrameDescriptor {
                color_space: WorkingColorSpace::LinearRec709.into(),
                ..plan.handle.descriptor()
            },
            ColorFrameDescriptor {
                domain: ColorFrameDomain::Working,
                ..plan.handle.descriptor()
            },
        ] {
            assert!(matches!(
                GpuColorFrameHandle::new(
                    GpuColorFrameId::from_raw(206),
                    incoherent,
                    GpuColorFrameTextureFormat::Rgba32Float,
                    "incoherent-alpha-mask",
                ),
                Err(GpuColorFrameHandleError::IncoherentSpaceDomain { .. })
            ));
        }
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
    fn gpu_color_frame_pool_reconfigures_without_replacing_the_owner() {
        let pool = GpuColorFrameWgpuResourcePool::default();
        let reduced = GpuColorFrameWgpuResourcePoolOptions {
            max_per_contract: 1,
            max_retained_bytes: 32 * 1024 * 1024,
        };

        pool.reconfigure(reduced);

        assert_eq!(pool.options(), reduced);
        assert_eq!(pool.diagnostics().retained_resources, 0);
    }

    #[test]
    fn gpu_color_frame_pool_invalidation_advances_the_return_generation() {
        let pool = GpuColorFrameWgpuResourcePool::default();
        let original = pool.generation();

        pool.invalidate();

        assert_ne!(pool.generation(), original);
        assert_eq!(pool.diagnostics().invalidations, 1);
        assert_eq!(pool.diagnostics().retained_resources, 0);
    }

    #[test]
    fn detached_presentation_accounting_fails_closed_on_public_budget_overflow() {
        let mut state = GpuColorFrameWgpuResourcePoolState::default();

        register_detached_presentation_demand(&mut state, u128::from(u64::MAX));
        assert!(!detached_presentation_demand_overflowed(&state));
        register_detached_presentation_demand(&mut state, 1);
        assert!(detached_presentation_demand_overflowed(&state));
        assert_eq!(state.detached_presentation_accounting_overflows, 1);
        assert_eq!(
            state.detached_presentation_high_water_bytes,
            u128::from(u64::MAX) + 1
        );

        unregister_detached_presentation_demand(&mut state, 1);
        assert!(!detached_presentation_demand_overflowed(&state));
        assert_eq!(state.detached_presentation_accounting_overflows, 1);
        unregister_detached_presentation_demand(&mut state, u128::from(u64::MAX));
        assert_eq!(state.detached_presentation_resources, 0);
        assert_eq!(state.detached_presentation_bytes, 0);
    }

    #[tokio::test]
    async fn presentation_output_lease_returns_once_to_its_current_pool_generation() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping presentation lease test: no GPU adapter");
            return;
        };
        let pool = Arc::new(GpuColorFrameWgpuResourcePool::default());
        let handle = gpu_handle(
            107,
            ColorFrameDescriptor { width: 2, height: 2, ..working_descriptor() },
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let resource = pool.acquire(
            &context.device,
            &GpuColorFrameAllocationPlan::for_handle(handle.clone()),
        );

        {
            let lease = ViewerGpuPresentationOutputLease::new(resource, Arc::clone(&pool));
            assert_eq!(lease.handle(), &handle);
            let detached = pool.diagnostics();
            assert_eq!(detached.retained_resources, 0);
            assert_eq!(detached.detached_presentation_resources, 1);
            assert_eq!(detached.detached_presentation_bytes, 2 * 2 * 8);
            assert_eq!(detached.detached_presentation_high_water_resources, 1);
            assert_eq!(detached.detached_presentation_high_water_bytes, 2 * 2 * 8);
            assert_eq!(detached.detached_presentation_accounting_overflows, 0);
            assert!(!detached.detached_presentation_accounting_overflowed);
        }

        let released = pool.diagnostics();
        assert_eq!(released.releases, 1);
        assert_eq!(released.retained_resources, 1);
        assert_eq!(released.detached_presentation_resources, 0);
        assert_eq!(released.detached_presentation_bytes, 0);
        assert_eq!(released.detached_presentation_high_water_resources, 1);
        assert_eq!(released.detached_presentation_high_water_bytes, 2 * 2 * 8);
        let reused = pool.acquire(
            &context.device,
            &GpuColorFrameAllocationPlan::for_handle(handle),
        );
        assert_eq!(pool.diagnostics().hits, 1);
        drop(reused);
    }

    #[tokio::test]
    async fn presentation_output_lease_cannot_repopulate_an_invalidated_pool() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping stale presentation lease test: no GPU adapter");
            return;
        };
        let pool = Arc::new(GpuColorFrameWgpuResourcePool::default());
        let handle = gpu_handle(
            108,
            ColorFrameDescriptor { width: 2, height: 2, ..working_descriptor() },
            GpuColorFrameTextureFormat::Rgba16Float,
        );
        let resource = pool.acquire(
            &context.device,
            &GpuColorFrameAllocationPlan::for_handle(handle),
        );
        let lease = ViewerGpuPresentationOutputLease::new(resource, Arc::clone(&pool));

        pool.invalidate();
        let invalidated = pool.diagnostics();
        assert_eq!(invalidated.detached_presentation_resources, 1);
        assert_eq!(invalidated.detached_presentation_bytes, 2 * 2 * 8);
        drop(lease);

        let diagnostics = pool.diagnostics();
        assert_eq!(diagnostics.invalidations, 1);
        assert_eq!(diagnostics.stale_generation_releases, 1);
        assert_eq!(diagnostics.releases, 0);
        assert_eq!(diagnostics.detached_presentation_resources, 0);
        assert_eq!(diagnostics.detached_presentation_bytes, 0);
        assert_eq!(diagnostics.detached_presentation_high_water_resources, 1);
        assert_eq!(
            diagnostics.detached_presentation_high_water_bytes,
            2 * 2 * 8
        );
        assert_eq!(diagnostics.retained_resources, 0);
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
        assert_eq!(plan.bytes(), frame.rgba());
        let GpuColorFrameUploadPayload::Bytes(payload) = &plan.payload else {
            panic!("RGBA8 upload must retain byte payload");
        };
        assert!(Arc::ptr_eq(payload, &frame.rgba));
        let GpuColorFrameUploadPayload::Bytes(cloned_payload) = &plan.clone().payload else {
            panic!("cloned RGBA8 upload must retain byte payload");
        };
        assert!(Arc::ptr_eq(payload, cloned_payload));
    }

    #[test]
    fn linear_float_source_upload_preserves_extended_range_and_shares_payload() {
        let samples = Arc::new(vec![-0.25, 0.18, 2.0, 1.0, 4.0, -1.0, 0.5, 0.25]);
        let frame =
            LinearFloatSource::new_shared(2, 1, ColorSpace::LinearRec709, Arc::clone(&samples));

        let plan = GpuColorFrameUploadPlan::from_linear_float_source(
            GpuColorFrameId::from_raw(202),
            &frame,
            "linear-source-upload",
        )
        .expect("linear float upload plan");

        assert_eq!(plan.texture_format, GpuColorFrameTextureFormat::Rgba32Float);
        assert_eq!(
            plan.handle.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert_eq!(plan.bytes_per_row, 32);
        assert_eq!(
            bytemuck::cast_slice::<u8, f32>(plan.bytes()),
            samples.as_slice()
        );
        let GpuColorFrameUploadPayload::Float32(payload) = &plan.payload else {
            panic!("linear source upload must retain f32 payload");
        };
        assert!(Arc::ptr_eq(payload, &samples));
    }

    #[test]
    fn encoded_float_source_upload_preserves_source_encoding_and_precision() {
        let frame = CpuEncodedFloatColorFrame::source_flat_rgba_f32(
            1,
            1,
            ColorSpace::Rec709,
            vec![1.0 / 65_535.0, 0.5, 1.0, 1.0],
        );

        let plan = GpuColorFrameUploadPlan::from_cpu_encoded_float_frame(
            GpuColorFrameId::from_raw(203),
            &frame,
            "encoded-float-source-upload",
        )
        .expect("encoded float upload plan");

        assert_eq!(plan.texture_format, GpuColorFrameTextureFormat::Rgba32Float);
        assert_eq!(
            plan.handle.descriptor().encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(plan.handle.descriptor().domain, ColorFrameDomain::Source);
        assert_eq!(
            bytemuck::cast_slice::<u8, f32>(plan.bytes())[0],
            1.0 / 65_535.0
        );
        let GpuColorFrameUploadPayload::EncodedRgba32(payload) = &plan.payload else {
            panic!("encoded float upload must retain the shared typed payload");
        };
        assert!(Arc::ptr_eq(payload, &frame.frame));
    }

    #[test]
    fn cpu_color_frame_clone_shares_linear_payload() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.25, 0.5, 0.75, 1.0], [1.25, 1.5, 1.75, 0.5]],
        });

        let cloned = frame.clone();

        assert!(Arc::ptr_eq(&frame.frame, &cloned.frame));
        assert_eq!(cloned.rgba_f32().data, frame.rgba_f32().data);
        assert_eq!(cloned.descriptor(), frame.descriptor());
    }

    #[test]
    fn encoded_float_frame_has_a_distinct_non_linear_payload_type() {
        let frame = CpuEncodedFloatColorFrame::new(
            EncodedRgbaF32Frame {
                width: 1,
                height: 1,
                data: vec![[0.5, 0.25, 0.125, 1.0]],
                color_space: ColorSpace::Srgb,
            },
            ColorFrameDomain::Export,
        );

        assert_eq!(
            frame.descriptor().encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(frame.rgba_f32().color_space, ColorSpace::Srgb);
        assert_eq!(frame.rgba_f32().data[0], [0.5, 0.25, 0.125, 1.0]);
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
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            color_space: WorkingColorSpace::LinearRec709,
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
            color_space: ColorSpace::Srgb.into(),
            domain: ColorFrameDomain::Display,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
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
            color_space: ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Export,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
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
            color_space: ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        }
    }

    fn native_import_contract() -> GpuNativeDecodedFrameImportContract {
        GpuNativeDecodedFrameImportContract {
            width: 3840,
            height: 2160,
            output_width: 960,
            output_height: 540,
            source_color_space: ColorSpace::Rec2100Pq,
            input_transform: RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec2020,
                true,
                mondrian_core::types::ColorEngine::mondrian_standard(),
            ),
            handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
            source_texture_format: GpuNativeDecodedFrameTextureFormat::Nv12,
            video_sampling: GpuNativeDecodedFrameVideoSampling::from_source_color_space(
                ColorSpace::Rec2100Pq,
                GpuVideoRange::Limited,
                8,
                GpuVideoChromaLocation::Left,
            ),
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
