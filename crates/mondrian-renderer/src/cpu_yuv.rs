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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use crate::{
    native_video_sampling_from_decoded, product_gpu_working_texture_format, ColorFrameAlpha,
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency,
    GpuColorFrameAllocationPlan, GpuColorFrameHandle, GpuNativeDecodedFrameTextureFormat,
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
/// per layer and frame. A bounded renderer worker copies retained media planes
/// into reusable mapped transfer buffers before the realtime caller records a
/// candidate. Viewer execution then records only buffer-to-texture commands;
/// successful submissions remap the transfer buffer asynchronously and return
/// it to the worker pool.
pub(crate) struct CpuYuvUploadRuntime {
    request_sender: mpsc::SyncSender<CpuYuvUploadWorkerCommand>,
    trim_requested: Arc<AtomicBool>,
    state: Mutex<CpuYuvUploadState>,
    worker: std::thread::JoinHandle<()>,
}

/// Consumed upload owner: no scheduling endpoints survive this transition.
pub(crate) struct CpuYuvUploadRetirement {
    worker: Option<std::thread::JoinHandle<()>>,
    outcome: Option<crate::ViewerCpuYuvUploadWorkerExit>,
    _slots: Vec<CpuYuvUploadSlot>,
    _prepared: VecDeque<CpuYuvUploadWorkerResult>,
    _used_buffers: Vec<wgpu::Buffer>,
}

impl CpuYuvUploadRetirement {
    pub(crate) fn poll(&mut self) -> Option<crate::ViewerCpuYuvUploadWorkerExit> {
        if let Some(outcome) = self.outcome {
            return Some(outcome);
        }
        if !self.worker.as_ref()?.is_finished() {
            return None;
        }
        let worker = self.worker.take()?;
        let outcome = if worker.join().is_ok() {
            crate::ViewerCpuYuvUploadWorkerExit::Returned
        } else {
            crate::ViewerCpuYuvUploadWorkerExit::Panicked
        };
        self.outcome = Some(outcome);
        Some(outcome)
    }
}

struct CpuYuvUploadState {
    slots: Vec<CpuYuvUploadSlot>,
    next_slot: usize,
    generation: u64,
    pending: Vec<CpuYuvFrameUploadKey>,
    prepared: VecDeque<CpuYuvUploadWorkerResult>,
    result_receiver: mpsc::Receiver<CpuYuvUploadWorkerResult>,
    returned_sender: mpsc::Sender<wgpu::Buffer>,
    used_buffers: Vec<wgpu::Buffer>,
    completion_waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

const CPU_YUV_UPLOAD_WORKER_CAPACITY: usize = 4;
const CPU_YUV_UPLOAD_POOL_CAPACITY: usize = 4;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuYuvFrameUploadKey {
    generation: u64,
    frame_identity: usize,
}

struct CpuYuvPreparedUpload {
    frame: Arc<CpuYuvFrame>,
    buffer: wgpu::Buffer,
    luma: PreparedPlaneCopy,
    chroma: PreparedPlaneCopy,
    chroma_v: Option<PreparedPlaneCopy>,
}

#[derive(Debug, Clone, Copy)]
struct PreparedPlaneCopy {
    offset: u64,
    bytes_per_row: u32,
    width: u32,
    height: u32,
}

struct CpuYuvUploadWorkerResult {
    key: CpuYuvFrameUploadKey,
    outcome: Result<CpuYuvPreparedUpload, String>,
}

enum CpuYuvUploadWorkerCommand {
    Prepare {
        key: CpuYuvFrameUploadKey,
        frame: Arc<CpuYuvFrame>,
        waker: Option<Arc<dyn Fn() + Send + Sync>>,
    },
    Trim,
}

impl CpuYuvUploadRuntime {
    pub(crate) fn new(device: &wgpu::Device) -> Result<Self, CpuYuvUploadWorkerStartError> {
        let (request_sender, request_receiver) = mpsc::sync_channel(CPU_YUV_UPLOAD_WORKER_CAPACITY);
        let (result_sender, result_receiver) = mpsc::sync_channel(CPU_YUV_UPLOAD_WORKER_CAPACITY);
        let (returned_sender, returned_receiver) = mpsc::channel();
        let worker_device = device.clone();
        let trim_requested = Arc::new(AtomicBool::new(false));
        let worker_trim_requested = Arc::clone(&trim_requested);
        // Complete bounded owner allocation before starting physical execution.
        let state = Mutex::new(CpuYuvUploadState {
            slots: Vec::new(),
            next_slot: 0,
            generation: 1,
            pending: Vec::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY),
            prepared: VecDeque::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY),
            result_receiver,
            returned_sender,
            used_buffers: Vec::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY),
            completion_waker: None,
        });
        let worker = std::thread::Builder::new()
            .name("mondrian-viewer-yuv-upload".to_owned())
            .spawn(move || {
                run_cpu_yuv_upload_worker(
                    worker_device,
                    request_receiver,
                    result_sender,
                    returned_receiver,
                    worker_trim_requested,
                );
            })
            .map_err(|_| CpuYuvUploadWorkerStartError)?;
        Ok(Self { request_sender, trim_requested, state, worker })
    }

    pub(crate) fn into_retirement(self) -> CpuYuvUploadRetirement {
        let Self { request_sender, trim_requested: _, state, worker } = self;
        let CpuYuvUploadState {
            slots,
            next_slot: _,
            generation: _,
            pending: _,
            prepared,
            result_receiver,
            returned_sender,
            used_buffers,
            completion_waker: _,
        } = state.into_inner();
        drop(request_sender);
        // A worker may be blocked publishing to a full bounded result queue.
        // Closing admission alone cannot release that send.
        drop(result_receiver);
        // Late GPU map callbacks may retain other return-sender clones. Worker
        // exit never depends on the return channel becoming disconnected.
        drop(returned_sender);
        CpuYuvUploadRetirement {
            worker: Some(worker),
            outcome: None,
            _slots: slots,
            _prepared: prepared,
            _used_buffers: used_buffers,
        }
    }

    pub(crate) fn install_completion_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.state.lock().completion_waker = Some(Arc::new(waker));
    }

    /// Start one Viewer candidate while retaining device allocations.
    pub(crate) fn begin_frame(&self) {
        self.state.lock().next_slot = 0;
    }

    /// Bind every transfer buffer used by this candidate to asynchronous remap
    /// and worker-pool return after its command buffer completes.
    pub(crate) fn finish_candidate(&self, encoder: &wgpu::CommandEncoder) {
        let (buffers, returned_sender) = {
            let mut state = self.state.lock();
            (
                std::mem::take(&mut state.used_buffers),
                state.returned_sender.clone(),
            )
        };
        for buffer in buffers {
            let returned_sender = returned_sender.clone();
            let callback_buffer = buffer.clone();
            encoder.map_buffer_on_submit(&buffer, wgpu::MapMode::Write, .., move |result| {
                if result.is_ok() {
                    let _ = returned_sender.send(callback_buffer);
                }
            });
        }
    }

    /// Drop transfer buffers whose recorded copies will not be submitted.
    pub(crate) fn discard_candidate(&self) {
        self.state.lock().used_buffers.clear();
    }

    /// Retire every idle upload texture during critical trim or device reset.
    pub(crate) fn clear(&self) {
        let mut state = self.state.lock();
        state.slots.clear();
        state.next_slot = 0;
        state.generation = state.generation.wrapping_add(1);
        state.pending.clear();
        state.prepared.clear();
        state.used_buffers.clear();
        self.trim_requested.store(true, Ordering::Release);
        let _ = self.request_sender.try_send(CpuYuvUploadWorkerCommand::Trim);
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<CpuYuvUploadedPlaneViews, CpuYuvMaterializationError> {
        let prepared = self.take_or_schedule_prepared(frame)?;
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
        record_prepared_plane_copy(encoder, &prepared.buffer, &slot.luma, prepared.luma);
        record_prepared_plane_copy(encoder, &prepared.buffer, &slot.chroma, prepared.chroma);
        if let Some(chroma_v_copy) = prepared.chroma_v {
            let chroma_v = slot
                .chroma_v
                .as_ref()
                .ok_or(CpuYuvMaterializationError::MissingPlanarChromaTexture)?;
            record_prepared_plane_copy(encoder, &prepared.buffer, chroma_v, chroma_v_copy);
        }
        let chroma_v = slot.chroma_v.as_ref().unwrap_or(&slot.chroma);
        let views = CpuYuvUploadedPlaneViews {
            luma: slot.luma.create_view(&wgpu::TextureViewDescriptor::default()),
            chroma: slot.chroma.create_view(&wgpu::TextureViewDescriptor::default()),
            chroma_v: chroma_v.create_view(&wgpu::TextureViewDescriptor::default()),
        };
        state.used_buffers.push(prepared.buffer);
        Ok(views)
    }

    /// Ensure one retained media frame has a worker-owned mapped upload ready.
    ///
    /// This operation records no GPU commands and does not consume the
    /// prepared result. Viewer lookahead can therefore start the large plane
    /// copy several frame intervals before the exact candidate acquires a GPU
    /// submission slot.
    pub(crate) fn prepare(
        &self,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<bool, CpuYuvMaterializationError> {
        let mut state = self.state.lock();
        Self::drain_worker_results(&mut state);
        let key = Self::frame_key(&state, frame);
        if state.prepared.iter().any(|result| result.key == key) {
            return Ok(true);
        }
        if !state.pending.contains(&key) {
            let command = CpuYuvUploadWorkerCommand::Prepare {
                key,
                frame: Arc::clone(frame),
                waker: state.completion_waker.clone(),
            };
            match self.request_sender.try_send(command) {
                Ok(()) => state.pending.push(key),
                Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(CpuYuvMaterializationError::UploadWorkerUnavailable);
                }
            }
        }
        Ok(false)
    }

    fn take_or_schedule_prepared(
        &self,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<CpuYuvPreparedUpload, CpuYuvMaterializationError> {
        if !self.prepare(frame)? {
            return Err(CpuYuvMaterializationError::UploadPending);
        }
        let mut state = self.state.lock();
        Self::drain_worker_results(&mut state);
        let key = Self::frame_key(&state, frame);
        let index = state
            .prepared
            .iter()
            .position(|result| result.key == key)
            .ok_or(CpuYuvMaterializationError::UploadPending)?;
        let result = state
            .prepared
            .remove(index)
            .ok_or(CpuYuvMaterializationError::UploadWorkerUnavailable)?;
        let prepared = result.outcome.map_err(CpuYuvMaterializationError::UploadPreparation)?;
        if !Arc::ptr_eq(&prepared.frame, frame) {
            return Err(CpuYuvMaterializationError::UploadIdentityMismatch);
        }
        Ok(prepared)
    }

    fn drain_worker_results(state: &mut CpuYuvUploadState) {
        while let Ok(result) = state.result_receiver.try_recv() {
            state.pending.retain(|pending| *pending != result.key);
            if result.key.generation == state.generation {
                if state.prepared.len() >= CPU_YUV_UPLOAD_WORKER_CAPACITY {
                    state.prepared.pop_front();
                }
                state.prepared.push_back(result);
            }
        }
    }

    fn frame_key(state: &CpuYuvUploadState, frame: &Arc<CpuYuvFrame>) -> CpuYuvFrameUploadKey {
        CpuYuvFrameUploadKey {
            generation: state.generation,
            frame_identity: Arc::as_ptr(frame) as usize,
        }
    }
}

pub(crate) fn record_cpu_yuv_frame(
    decoder: &GpuNativeYuvDecoder,
    uploads: &CpuYuvUploadRuntime,
    frame: &Arc<CpuYuvFrame>,
    input_transform: &RenderInputTransform,
    output_width: u32,
    output_height: u32,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<GpuColorFrameHandle, CpuYuvMaterializationError> {
    let source_color_space = frame
        .source_color
        .color_space()
        .ok_or(CpuYuvMaterializationError::InvalidVideoSampling)?;
    let source_texture_format = match frame.sample_format {
        CpuYuvSampleFormat::Unorm8 => GpuNativeDecodedFrameTextureFormat::Nv12,
        CpuYuvSampleFormat::Unorm16Lsb10 => GpuNativeDecodedFrameTextureFormat::P010,
    };
    let video_sampling = native_video_sampling_from_decoded(
        source_color_space,
        source_texture_format,
        frame.video_sampling,
    )
    .ok_or(CpuYuvMaterializationError::InvalidVideoSampling)?;
    let encoded_source = GpuColorFrameHandle::new(
        runtime.frame_ids_mut().allocate()?,
        ColorFrameDescriptor {
            width: output_width,
            height: output_height,
            color_space: source_color_space.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::Opaque,
        },
        product_gpu_working_texture_format(),
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
        product_gpu_working_texture_format(),
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
    let planes = uploads.upload(device, encoder, frame)?;
    let prepared = decoder.prepare_pass(
        device,
        &plan,
        GpuNativeYuvPlaneViews {
            luma: &planes.luma,
            chroma: &planes.chroma,
            chroma_v: &planes.chroma_v,
        },
    );
    // The encoded RGB intermediate is frame-local, but its physical texture is
    // part of the steady Viewer working set. Acquiring it from the same
    // exact-contract pool as the working/output stages prevents a fresh UHD
    // allocation on every software-decoded frame. Direct allocation appears
    // cheap until the backend allocator grows, at which point it can stall a
    // realtime successor record for an entire frame interval.
    let resource_pool = runtime.resource_pool();
    let encoded_resource = resource_pool.acquire(
        device,
        &GpuColorFrameAllocationPlan::for_handle(encoded_source.clone()),
    );
    decoder.record(encoder, &plan, &prepared, &encoded_resource)?;
    if let Some(previous) = runtime.frame_table_mut().insert(encoded_resource)? {
        if let Some(inserted) = runtime.frame_table_mut().remove(encoded_source.id()) {
            resource_pool.release(inserted);
        }
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
    if let Err(error) = record_result {
        for id in [encoded_source.id(), working.id()] {
            if let Some(resource) = runtime.frame_table_mut().remove(id) {
                resource_pool.release(resource);
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaneUploadLayout {
    visible_bytes_per_row: u32,
    upload_bytes_per_row: u32,
    upload_byte_count: u64,
}

fn plane_upload_layout(
    width: u32,
    height: u32,
    bytes_per_texel: u32,
) -> Result<PlaneUploadLayout, CpuYuvMaterializationError> {
    let visible_bytes_per_row = width
        .checked_mul(bytes_per_texel)
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let upload_bytes_per_row = visible_bytes_per_row
        .checked_add(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        .map(|bytes| bytes / wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .and_then(|rows| rows.checked_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let upload_byte_count = u64::from(upload_bytes_per_row)
        .checked_mul(u64::from(height))
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    if upload_byte_count == 0 {
        return Err(CpuYuvMaterializationError::EmptyPlane);
    }
    Ok(PlaneUploadLayout {
        visible_bytes_per_row,
        upload_bytes_per_row,
        upload_byte_count,
    })
}

fn run_cpu_yuv_upload_worker(
    device: wgpu::Device,
    request_receiver: mpsc::Receiver<CpuYuvUploadWorkerCommand>,
    result_sender: mpsc::SyncSender<CpuYuvUploadWorkerResult>,
    returned_receiver: mpsc::Receiver<wgpu::Buffer>,
    trim_requested: Arc<AtomicBool>,
) {
    let mut pool = Vec::with_capacity(CPU_YUV_UPLOAD_POOL_CAPACITY);
    while let Ok(command) = request_receiver.recv() {
        if trim_requested.swap(false, Ordering::AcqRel) {
            pool.clear();
        }
        while let Ok(buffer) = returned_receiver.try_recv() {
            if pool.len() < CPU_YUV_UPLOAD_POOL_CAPACITY {
                pool.push(buffer);
            }
        }
        match command {
            CpuYuvUploadWorkerCommand::Prepare { key, frame, waker } => {
                let outcome = prepare_cpu_yuv_upload(&device, &mut pool, frame)
                    .map_err(|error| error.to_string());
                if result_sender.send(CpuYuvUploadWorkerResult { key, outcome }).is_err() {
                    break;
                }
                if let Some(waker) = waker {
                    waker();
                }
            }
            CpuYuvUploadWorkerCommand::Trim => pool.clear(),
        }
    }
}

fn prepare_cpu_yuv_upload(
    device: &wgpu::Device,
    pool: &mut Vec<wgpu::Buffer>,
    frame: Arc<CpuYuvFrame>,
) -> Result<CpuYuvPreparedUpload, CpuYuvMaterializationError> {
    let bytes_per_component = frame.sample_format.bytes_per_component() as u32;
    let luma_plane = frame.luma_plane();
    validate_plane_bytes(frame.width, frame.height, bytes_per_component, luma_plane)?;
    let luma_layout = plane_upload_layout(frame.width, frame.height, bytes_per_component)?;
    let luma = PreparedPlaneCopy {
        offset: 0,
        bytes_per_row: luma_layout.upload_bytes_per_row,
        width: frame.width,
        height: frame.height,
    };
    let chroma_offset = luma_layout.upload_byte_count;
    let (chroma_plane, chroma_v_plane, chroma_bytes_per_texel) = match frame.chroma_planes() {
        CpuYuvChromaPlanes::Interleaved(chroma) => (chroma, None, bytes_per_component * 2),
        CpuYuvChromaPlanes::Planar { cb, cr } => (cb, Some(cr), bytes_per_component),
    };
    validate_plane_bytes(
        frame.chroma_width,
        frame.chroma_height,
        chroma_bytes_per_texel,
        chroma_plane,
    )?;
    let chroma_layout = plane_upload_layout(
        frame.chroma_width,
        frame.chroma_height,
        chroma_bytes_per_texel,
    )?;
    let chroma = PreparedPlaneCopy {
        offset: chroma_offset,
        bytes_per_row: chroma_layout.upload_bytes_per_row,
        width: frame.chroma_width,
        height: frame.chroma_height,
    };
    let chroma_v_offset = chroma_offset
        .checked_add(chroma_layout.upload_byte_count)
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let (chroma_v, chroma_v_layout) = if let Some(chroma_v_plane) = chroma_v_plane {
        validate_plane_bytes(
            frame.chroma_width,
            frame.chroma_height,
            bytes_per_component,
            chroma_v_plane,
        )?;
        let layout =
            plane_upload_layout(frame.chroma_width, frame.chroma_height, bytes_per_component)?;
        (
            Some(PreparedPlaneCopy {
                offset: chroma_v_offset,
                bytes_per_row: layout.upload_bytes_per_row,
                width: frame.chroma_width,
                height: frame.chroma_height,
            }),
            Some(layout),
        )
    } else {
        (None, None)
    };
    let total_bytes = chroma_v_layout
        .map_or_else(
            || chroma_offset.checked_add(chroma_layout.upload_byte_count),
            |layout| chroma_v_offset.checked_add(layout.upload_byte_count),
        )
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let buffer = pool
        .iter()
        .position(|buffer| buffer.size() >= total_bytes)
        .map(|index| pool.swap_remove(index))
        .unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mondrian.cpu-yuv.upload-worker"),
                size: total_bytes,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            })
        });
    {
        let mut mapped = buffer
            .slice(..total_bytes)
            .get_mapped_range_mut()
            .map_err(|_| CpuYuvMaterializationError::MappedUploadUnavailable)?;
        copy_plane_to_mapped(&mut mapped, luma.offset, luma_layout, luma_plane)?;
        copy_plane_to_mapped(&mut mapped, chroma.offset, chroma_layout, chroma_plane)?;
        if let (Some(copy), Some(layout), Some(plane)) = (chroma_v, chroma_v_layout, chroma_v_plane)
        {
            copy_plane_to_mapped(&mut mapped, copy.offset, layout, plane)?;
        }
    }
    buffer.unmap();
    Ok(CpuYuvPreparedUpload { frame, buffer, luma, chroma, chroma_v })
}

fn copy_plane_to_mapped(
    mapped: &mut wgpu::BufferViewMut,
    target_offset: u64,
    layout: PlaneUploadLayout,
    plane: CpuYuvPlane<'_>,
) -> Result<(), CpuYuvMaterializationError> {
    let target_offset = usize::try_from(target_offset)
        .map_err(|_| CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let source_stride = plane.bytes_per_row() as usize;
    let target_stride = layout.upload_bytes_per_row as usize;
    let visible_row_bytes = layout.visible_bytes_per_row as usize;
    let upload_byte_count = usize::try_from(layout.upload_byte_count)
        .map_err(|_| CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let target_end = target_offset
        .checked_add(upload_byte_count)
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    if source_stride == target_stride && plane.data().len() >= upload_byte_count {
        mapped
            .slice(target_offset..target_end)
            .copy_from_slice(&plane.data()[..upload_byte_count]);
        return Ok(());
    }
    let height = upload_byte_count / target_stride;
    for row in 0..height {
        let source_start = row
            .checked_mul(source_stride)
            .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
        let source_end = source_start
            .checked_add(visible_row_bytes)
            .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
        let target_start = target_offset
            .checked_add(
                row.checked_mul(target_stride)
                    .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?,
            )
            .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
        let target_end = target_start
            .checked_add(visible_row_bytes)
            .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
        mapped
            .slice(target_start..target_end)
            .copy_from_slice(&plane.data()[source_start..source_end]);
    }
    Ok(())
}

fn record_prepared_plane_copy(
    encoder: &mut wgpu::CommandEncoder,
    buffer: &wgpu::Buffer,
    texture: &wgpu::Texture,
    plane: PreparedPlaneCopy,
) {
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: plane.offset,
                bytes_per_row: Some(plane.bytes_per_row),
                rows_per_image: Some(plane.height),
            },
        },
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: plane.width,
            height: plane.height,
            depth_or_array_layers: 1,
        },
    );
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CpuYuvMaterializationError {
    #[error("compact CPU YUV sampling metadata is incomplete or inconsistent")]
    InvalidVideoSampling,
    #[error("compact CPU YUV plane extent overflowed")]
    PlaneExtentOverflow,
    #[error("compact CPU YUV plane is empty")]
    EmptyPlane,
    #[error("compact CPU YUV staging allocation is not mapped for writing")]
    MappedUploadUnavailable,
    #[error("compact CPU YUV upload preparation is still running")]
    UploadPending,
    #[error("compact CPU YUV upload worker is unavailable")]
    UploadWorkerUnavailable,
    #[error("compact CPU YUV upload result did not retain the requested frame identity")]
    UploadIdentityMismatch,
    #[error("compact CPU YUV upload preparation failed: {0}")]
    UploadPreparation(String),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("failed to start compact CPU YUV upload worker")]
pub(crate) struct CpuYuvUploadWorkerStartError;

#[cfg(test)]
#[path = "cpu_yuv/retirement_tests.rs"]
mod retirement_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_upload_layout_preserves_aligned_uhd_ten_bit_rows() {
        let layout = plane_upload_layout(3840, 2160, 2).expect("valid UHD luma layout");

        assert_eq!(layout.visible_bytes_per_row, 7680);
        assert_eq!(layout.upload_bytes_per_row, 7680);
        assert_eq!(layout.upload_byte_count, 16_588_800);
    }

    #[test]
    fn plane_upload_layout_pads_transfer_stride_without_changing_visible_width() {
        let layout = plane_upload_layout(1919, 1080, 2).expect("valid padded luma layout");

        assert_eq!(layout.visible_bytes_per_row, 3838);
        assert_eq!(layout.upload_bytes_per_row, 3840);
        assert_eq!(layout.upload_byte_count, 4_147_200);
    }

    #[test]
    fn plane_upload_layout_rejects_empty_and_overflowing_planes() {
        assert!(matches!(
            plane_upload_layout(0, 1080, 2),
            Err(CpuYuvMaterializationError::EmptyPlane)
        ));
        assert!(matches!(
            plane_upload_layout(u32::MAX, 1, 2),
            Err(CpuYuvMaterializationError::PlaneExtentOverflow)
        ));
    }
}
