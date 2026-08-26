//! Compact CPU YUV plane upload and shared GPU video/color materialization.
//!
//! Software decoding remains media-owned. This renderer Module prevents that
//! boundary from expanding subsampled high-bit video into CPU RGBA: it uploads
//! compact normalized planes, reuses the native-video YUV shader, and then
//! enters the sole OCIO GPU input-stage contract.

use mondrian_media::{
    CpuYuvChromaPlaneLayout, CpuYuvChromaPlanes, CpuYuvChromaSubsampling, CpuYuvFrame, CpuYuvPlane,
    CpuYuvSampleFormat,
};
use parking_lot::Mutex;

use crate::{
    native_video_sampling_from_decoded, ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, GpuColorFrameAllocationPlan, GpuColorFrameHandle,
    GpuColorFrameTextureFormat, GpuColorFrameUploader, GpuNativeDecodedFrameTextureFormat,
    GpuNativeVideoExtent, GpuNativeYuvDecodePlan, GpuNativeYuvDecoder, GpuNativeYuvPlaneViews,
    GpuYuvChromaPlaneLayout, GpuYuvChromaSubsampling, GpuYuvCodeAlignment,
    RenderColorTransformGpuOptions, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, RenderInputTransform,
};

/// Viewer-owned compact-plane upload cache.
///
/// One slot is required for each compact YUV layer recorded into the same
/// command buffer. Slots survive candidate clears so steady-state playback
/// updates their existing textures instead of allocating two device textures
/// per layer and frame. Viewer execution submits a recorded candidate before
/// beginning the next one; queue writes and command-buffer submissions are
/// therefore ordered even when the next candidate reuses the same slot while
/// the previous submission is still completing.
pub(crate) struct CpuYuvUploadRuntime {
    state: Mutex<CpuYuvUploadState>,
}

#[derive(Default)]
struct CpuYuvUploadState {
    slots: Vec<CpuYuvUploadSlot>,
    next_slot: usize,
}

struct CpuYuvUploadSlot {
    key: CpuYuvUploadKey,
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    chroma_v: Option<wgpu::Texture>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuYuvUploadKey {
    width: u32,
    height: u32,
    chroma_width: u32,
    chroma_height: u32,
    sample_format: CpuYuvSampleFormat,
    chroma_plane_layout: CpuYuvChromaPlaneLayout,
}

struct CpuYuvUploadedPlaneViews {
    luma: wgpu::TextureView,
    chroma: wgpu::TextureView,
    chroma_v: wgpu::TextureView,
}

impl CpuYuvUploadRuntime {
    pub(crate) fn new() -> Self {
        Self { state: Mutex::new(CpuYuvUploadState::default()) }
    }

    /// Start one Viewer candidate while retaining device allocations.
    pub(crate) fn begin_frame(&self) {
        self.state.lock().next_slot = 0;
    }

    /// Retire every idle upload texture during critical trim or device reset.
    pub(crate) fn clear(&self) {
        let mut state = self.state.lock();
        state.slots.clear();
        state.next_slot = 0;
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &CpuYuvFrame,
    ) -> Result<CpuYuvUploadedPlaneViews, CpuYuvMaterializationError> {
        let key = CpuYuvUploadKey {
            width: frame.width,
            height: frame.height,
            chroma_width: frame.chroma_width,
            chroma_height: frame.chroma_height,
            sample_format: frame.sample_format,
            chroma_plane_layout: frame.chroma_plane_layout(),
        };
        let (luma_format, interleaved_chroma_format, planar_chroma_format) =
            plane_formats(frame.sample_format);
        let luma = frame.luma_plane();
        validate_plane_bytes(
            frame.width,
            frame.height,
            frame.sample_format.bytes_per_component() as u32,
            luma,
        )?;
        let chroma = frame.chroma_planes();
        match chroma {
            CpuYuvChromaPlanes::Interleaved(chroma) => validate_plane_bytes(
                frame.chroma_width,
                frame.chroma_height,
                (frame.sample_format.bytes_per_component() * 2) as u32,
                chroma,
            )?,
            CpuYuvChromaPlanes::Planar { cb, cr } => {
                validate_plane_bytes(
                    frame.chroma_width,
                    frame.chroma_height,
                    frame.sample_format.bytes_per_component() as u32,
                    cb,
                )?;
                validate_plane_bytes(
                    frame.chroma_width,
                    frame.chroma_height,
                    frame.sample_format.bytes_per_component() as u32,
                    cr,
                )?;
            }
        }

        let mut state = self.state.lock();
        let slot_index = state.next_slot;
        state.next_slot = state.next_slot.saturating_add(1);
        if state.slots.get(slot_index).is_none_or(|slot| slot.key != key) {
            let slot = CpuYuvUploadSlot {
                key,
                luma: create_plane_texture(
                    device,
                    "mondrian.cpu-yuv.luma",
                    frame.width,
                    frame.height,
                    luma_format,
                ),
                chroma: create_plane_texture(
                    device,
                    "mondrian.cpu-yuv.chroma",
                    frame.chroma_width,
                    frame.chroma_height,
                    match key.chroma_plane_layout {
                        CpuYuvChromaPlaneLayout::Interleaved => interleaved_chroma_format,
                        CpuYuvChromaPlaneLayout::Planar => planar_chroma_format,
                    },
                ),
                chroma_v: (key.chroma_plane_layout == CpuYuvChromaPlaneLayout::Planar).then(|| {
                    create_plane_texture(
                        device,
                        "mondrian.cpu-yuv.chroma-v",
                        frame.chroma_width,
                        frame.chroma_height,
                        planar_chroma_format,
                    )
                }),
            };
            if slot_index == state.slots.len() {
                state.slots.push(slot);
            } else {
                state.slots[slot_index] = slot;
            }
        }
        let slot = &state.slots[slot_index];
        write_plane(queue, &slot.luma, frame.width, frame.height, luma);
        match chroma {
            CpuYuvChromaPlanes::Interleaved(chroma) => {
                write_plane(
                    queue,
                    &slot.chroma,
                    frame.chroma_width,
                    frame.chroma_height,
                    chroma,
                );
            }
            CpuYuvChromaPlanes::Planar { cb, cr } => {
                write_plane(
                    queue,
                    &slot.chroma,
                    frame.chroma_width,
                    frame.chroma_height,
                    cb,
                );
                let chroma_v = slot
                    .chroma_v
                    .as_ref()
                    .ok_or(CpuYuvMaterializationError::MissingPlanarChromaTexture)?;
                write_plane(queue, chroma_v, frame.chroma_width, frame.chroma_height, cr);
            }
        }
        let chroma_v = slot.chroma_v.as_ref().unwrap_or(&slot.chroma);
        Ok(CpuYuvUploadedPlaneViews {
            luma: slot.luma.create_view(&wgpu::TextureViewDescriptor::default()),
            chroma: slot.chroma.create_view(&wgpu::TextureViewDescriptor::default()),
            chroma_v: chroma_v.create_view(&wgpu::TextureViewDescriptor::default()),
        })
    }
}

pub(crate) fn record_cpu_yuv_frame(
    decoder: &GpuNativeYuvDecoder,
    uploads: &CpuYuvUploadRuntime,
    frame: &CpuYuvFrame,
    input_transform: &RenderInputTransform,
    output_width: u32,
    output_height: u32,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<GpuColorFrameHandle, CpuYuvMaterializationError> {
    let source_texture_format = match frame.sample_format {
        CpuYuvSampleFormat::Unorm8 => GpuNativeDecodedFrameTextureFormat::Nv12,
        CpuYuvSampleFormat::Unorm16Lsb10 => GpuNativeDecodedFrameTextureFormat::P010,
    };
    let video_sampling = native_video_sampling_from_decoded(
        frame.source_color.color_space,
        source_texture_format,
        frame.video_sampling,
    )
    .ok_or(CpuYuvMaterializationError::InvalidVideoSampling)?;
    let encoded_source = GpuColorFrameHandle::new(
        runtime.frame_ids_mut().allocate()?,
        ColorFrameDescriptor {
            width: output_width,
            height: output_height,
            color_space: frame.source_color.color_space.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::Opaque,
        },
        GpuColorFrameTextureFormat::Rgba16Float,
        "viewer-cpu-yuv-encoded-source",
    )?;
    let working = GpuColorFrameHandle::new(
        runtime.frame_ids_mut().allocate()?,
        ColorFrameDescriptor {
            width: output_width,
            height: output_height,
            color_space: input_transform.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        },
        GpuColorFrameTextureFormat::Rgba32Float,
        "viewer-cpu-yuv-working",
    )?;
    let chroma_subsampling = match frame.chroma_subsampling {
        CpuYuvChromaSubsampling::Cs420 => GpuYuvChromaSubsampling::Cs420,
        CpuYuvChromaSubsampling::Cs422 => GpuYuvChromaSubsampling::Cs422,
    };
    let code_alignment = match frame.sample_format {
        CpuYuvSampleFormat::Unorm8 => GpuYuvCodeAlignment::MostSignificant,
        CpuYuvSampleFormat::Unorm16Lsb10 => GpuYuvCodeAlignment::LeastSignificant,
    };
    let chroma_plane_layout = match frame.chroma_plane_layout() {
        CpuYuvChromaPlaneLayout::Interleaved => GpuYuvChromaPlaneLayout::Interleaved,
        CpuYuvChromaPlaneLayout::Planar => GpuYuvChromaPlaneLayout::Planar,
    };
    let plan = GpuNativeYuvDecodePlan::new_with_plane_layout(
        source_texture_format,
        chroma_subsampling,
        chroma_plane_layout,
        code_alignment,
        GpuNativeVideoExtent { width: frame.width, height: frame.height },
        GpuNativeVideoExtent { width: output_width, height: output_height },
        GpuNativeVideoExtent { width: frame.width, height: frame.height },
        video_sampling,
        encoded_source.clone(),
    )?;
    let planes = uploads.upload(device, queue, frame)?;
    let prepared = decoder.prepare_pass(
        device,
        &plan,
        GpuNativeYuvPlaneViews {
            luma: &planes.luma,
            chroma: &planes.chroma,
            chroma_v: &planes.chroma_v,
        },
    );
    let encoded_resource = GpuColorFrameUploader::allocate(
        device,
        &GpuColorFrameAllocationPlan::for_handle(encoded_source.clone()),
    );
    decoder.record(encoder, &plan, &prepared, &encoded_resource)?;
    if let Some(previous) = runtime.frame_table_mut().insert(encoded_resource)? {
        runtime.frame_table_mut().remove(encoded_source.id());
        let displaced = runtime.frame_table_mut().insert(previous)?;
        debug_assert!(displaced.is_none());
        return Err(CpuYuvMaterializationError::LiveResourceCollision);
    }
    let record_result = runtime.record_wgpu_input_stage_gpu_frame_owned_backend(
        input_transform,
        &encoded_source,
        &working,
        RenderColorTransformGpuOptions::default(),
        RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
            device,
            queue,
            encoder,
            load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
        },
    );
    runtime.frame_table_mut().remove(encoded_source.id());
    if let Err(error) = record_result {
        runtime.frame_table_mut().remove(working.id());
        return Err(CpuYuvMaterializationError::ColorStage(format!("{error:?}")));
    }
    Ok(working)
}

fn plane_formats(
    sample_format: CpuYuvSampleFormat,
) -> (
    wgpu::TextureFormat,
    wgpu::TextureFormat,
    wgpu::TextureFormat,
) {
    match sample_format {
        CpuYuvSampleFormat::Unorm8 => (
            wgpu::TextureFormat::R8Unorm,
            wgpu::TextureFormat::Rg8Unorm,
            wgpu::TextureFormat::R8Unorm,
        ),
        CpuYuvSampleFormat::Unorm16Lsb10 => (
            wgpu::TextureFormat::R16Unorm,
            wgpu::TextureFormat::Rg16Unorm,
            wgpu::TextureFormat::R16Unorm,
        ),
    }
}

fn validate_plane_bytes(
    width: u32,
    height: u32,
    bytes_per_texel: u32,
    plane: CpuYuvPlane<'_>,
) -> Result<(), CpuYuvMaterializationError> {
    let visible_bytes_per_row = width
        .checked_mul(bytes_per_texel)
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    if plane.bytes_per_row() < visible_bytes_per_row {
        return Err(CpuYuvMaterializationError::PlaneStride {
            minimum: visible_bytes_per_row,
            actual: plane.bytes_per_row(),
        });
    }
    let expected = (plane.bytes_per_row() as usize)
        .checked_mul(height.saturating_sub(1) as usize)
        .and_then(|prefix| prefix.checked_add(visible_bytes_per_row as usize))
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    if plane.data().len() < expected {
        return Err(CpuYuvMaterializationError::PlaneByteCount {
            expected,
            actual: plane.data().len(),
        });
    }
    Ok(())
}

fn create_plane_texture(
    device: &wgpu::Device,
    label: &'static str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    plane: CpuYuvPlane<'_>,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        plane.data(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(plane.bytes_per_row()),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CpuYuvMaterializationError {
    #[error("compact CPU YUV sampling metadata is incomplete or inconsistent")]
    InvalidVideoSampling,
    #[error("compact CPU YUV plane extent overflowed")]
    PlaneExtentOverflow,
    #[error("compact CPU YUV plane has {actual} bytes; expected {expected}")]
    PlaneByteCount { expected: usize, actual: usize },
    #[error("compact CPU YUV plane row stride is {actual} bytes; expected at least {minimum}")]
    PlaneStride { minimum: u32, actual: u32 },
    #[error("compact planar CPU YUV upload slot is missing its Cr texture")]
    MissingPlanarChromaTexture,
    #[error("compact CPU YUV materialization collided with a live renderer resource")]
    LiveResourceCollision,
    #[error(transparent)]
    FrameId(#[from] crate::GpuColorFrameIdAllocationError),
    #[error(transparent)]
    FrameHandle(#[from] crate::GpuColorFrameHandleError),
    #[error(transparent)]
    DecodePlan(#[from] crate::GpuNativeYuvDecodePlanError),
    #[error(transparent)]
    DecodeRecord(#[from] crate::GpuNativeYuvDecodeRecordError),
    #[error(transparent)]
    ResourceTable(#[from] crate::GpuColorFrameResourceTableError),
    #[error("compact CPU YUV color stage failed: {0}")]
    ColorStage(String),
}
