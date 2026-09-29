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
    product_gpu_working_texture_format, ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, GpuColorFrameAllocationPlan, GpuColorFrameHandle,
    GpuNativeDecodedFrameTextureFormat, GpuNativeVideoExtent, GpuNativeYuvDecodePlan,
    GpuNativeYuvDecoder, GpuNativeYuvPlaneViews, GpuYuvChromaPlaneLayout, GpuYuvChromaSubsampling,
    GpuYuvCodeAlignment, RenderColorStageDiagnostics, RenderGpuOutputBoundaryRuntime,
    RenderInputTransform,
};

/// Viewer-owned compact-plane upload cache.
///
/// One slot is required for each distinct compact YUV frame recorded into the same
/// command buffer. Repeated layers share the immutable uploaded content of
/// their retained source frame. Slots survive candidate clears so playback
/// updates existing textures instead of allocating per layer and frame.
/// A bounded renderer worker copies retained media planes
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
    _recorded_uploads: Vec<RecordedCpuYuvUpload>,
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
    interest: Arc<Mutex<CpuYuvUploadInterest>>,
    pending: Vec<CpuYuvFrameUploadKey>,
    prepared: VecDeque<CpuYuvUploadWorkerResult>,
    result_receiver: mpsc::Receiver<CpuYuvUploadWorkerResult>,
    returned_sender: mpsc::Sender<wgpu::Buffer>,
    recorded_uploads: Vec<RecordedCpuYuvUpload>,
    completion_waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The single physical generation and current-input authority shared with the
/// upload worker. It contains no Timeline coordinates or publication state.
struct CpuYuvUploadInterest {
    generation: u64,
    candidate_inputs: Vec<(CpuYuvFrameUploadKey, Arc<CpuYuvFrame>)>,
    lookahead_inputs: Option<Vec<(CpuYuvFrameUploadKey, Arc<CpuYuvFrame>)>>,
}

impl Default for CpuYuvUploadInterest {
    fn default() -> Self {
        Self {
            generation: 1,
            candidate_inputs: Vec::new(),
            lookahead_inputs: None,
        }
    }
}

impl CpuYuvUploadInterest {
    fn is_candidate_input(&self, key: CpuYuvFrameUploadKey) -> bool {
        self.candidate_inputs.iter().any(|(candidate, _)| *candidate == key)
    }
}

impl CpuYuvUploadState {
    fn is_candidate_input(&self, key: CpuYuvFrameUploadKey) -> bool {
        self.interest.lock().is_candidate_input(key)
    }
}

const CPU_YUV_UPLOAD_WORKER_CAPACITY: usize = 4;
const CPU_YUV_UPLOAD_POOL_CAPACITY: usize = 4;
const CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY: usize = 4;

struct CpuYuvUploadSlot {
    key: CpuYuvUploadKey,
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    chroma_v: Option<wgpu::Texture>,
    alpha: Option<wgpu::Texture>,
    views: CpuYuvUploadedPlaneViews,
    prepared_pass: Option<Arc<crate::GpuNativeYuvPreparedPass>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuYuvUploadKey {
    width: u32,
    height: u32,
    chroma_width: u32,
    chroma_height: u32,
    sample_format: CpuYuvSampleFormat,
    chroma_plane_layout: CpuYuvChromaPlaneLayout,
    has_alpha: bool,
}

#[derive(Clone)]
pub(crate) struct CpuYuvUploadedPlaneViews {
    slot_index: usize,
    luma: wgpu::TextureView,
    chroma: wgpu::TextureView,
    chroma_v: wgpu::TextureView,
    alpha: Option<wgpu::TextureView>,
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
    alpha: Option<PreparedPlaneCopy>,
}

/// Immutable plane content shared only within one command-buffer candidate.
/// Retaining the prepared frame also prevents its pointer identity from being
/// recycled while another layer may still request these views.
struct RecordedCpuYuvUpload {
    key: CpuYuvFrameUploadKey,
    prepared: CpuYuvPreparedUpload,
    planes: CpuYuvUploadedPlaneViews,
}

pub(crate) struct CpuYuvMaterializationRecord {
    pub(crate) output: GpuColorFrameHandle,
    pub(crate) stage_diagnostics: RenderColorStageDiagnostics,
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
        let interest = Arc::new(Mutex::new(CpuYuvUploadInterest::default()));
        let worker_interest = Arc::clone(&interest);
        // Complete bounded owner allocation before starting physical execution.
        let state = Mutex::new(CpuYuvUploadState {
            slots: Vec::new(),
            next_slot: 0,
            interest,
            pending: Vec::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY),
            prepared: VecDeque::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY),
            result_receiver,
            returned_sender,
            recorded_uploads: Vec::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY),
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
                    worker_interest,
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
            interest: _,
            pending: _,
            prepared,
            result_receiver,
            returned_sender,
            recorded_uploads,
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
            _recorded_uploads: recorded_uploads,
        }
    }

    pub(crate) fn install_completion_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.state.lock().completion_waker = Some(Arc::new(waker));
    }

    /// Start one Viewer candidate while retaining device allocations.
    pub(crate) fn begin_frame(&self) {
        let mut state = self.state.lock();
        state.next_slot = 0;
        state.recorded_uploads.clear();
    }

    /// Bind every transfer buffer used by this candidate to asynchronous remap
    /// and worker-pool return after its command buffer completes.
    pub(crate) fn finish_candidate(&self, encoder: &dyn crate::gpu_command::GpuCommandEncoder) {
        let (uploads, returned_sender) = {
            let mut state = self.state.lock();
            (
                std::mem::take(&mut state.recorded_uploads),
                state.returned_sender.clone(),
            )
        };
        for upload in uploads {
            let buffer = upload.prepared.buffer;
            let returned_sender = returned_sender.clone();
            let callback_buffer = buffer.clone();
            encoder.map_buffer_on_submit(
                &buffer,
                wgpu::MapMode::Write,
                Box::new(move |result| {
                    if result.is_ok() {
                        let _ = returned_sender.send(callback_buffer);
                    }
                }),
            );
        }
    }

    /// Drop transfer buffers whose recorded copies will not be submitted.
    pub(crate) fn discard_candidate(&self) {
        self.state.lock().recorded_uploads.clear();
    }

    /// Retire every idle upload texture during critical trim or device reset.
    pub(crate) fn clear(&self) {
        let mut state = self.state.lock();
        state.slots.clear();
        state.next_slot = 0;
        {
            let mut interest = state.interest.lock();
            interest.generation = interest.generation.wrapping_add(1);
            interest.candidate_inputs.clear();
            interest.lookahead_inputs = None;
        }
        state.pending.clear();
        state.prepared.clear();
        state.recorded_uploads.clear();
        self.trim_requested.store(true, Ordering::Release);
        let _ = self.request_sender.try_send(CpuYuvUploadWorkerCommand::Trim);
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<(CpuYuvUploadedPlaneViews, bool), CpuYuvMaterializationError> {
        if !self.prepare(frame)? {
            return Err(CpuYuvMaterializationError::UploadPending);
        }
        let mut state = self.state.lock();
        let frame_key = Self::frame_key(&state, frame);
        if let Some(upload) = state.recorded_uploads.iter().find(|upload| upload.key == frame_key) {
            return Ok((upload.planes.clone(), false));
        }
        let index = state
            .prepared
            .iter()
            .position(|result| result.key == frame_key)
            .ok_or(CpuYuvMaterializationError::UploadPending)?;
        let result = state
            .prepared
            .remove(index)
            .ok_or(CpuYuvMaterializationError::UploadWorkerUnavailable)?;
        let prepared = result.outcome.map_err(CpuYuvMaterializationError::UploadPreparation)?;
        if !Arc::ptr_eq(&prepared.frame, frame) {
            return Err(CpuYuvMaterializationError::UploadIdentityMismatch);
        }
        let key = CpuYuvUploadKey {
            width: frame.width,
            height: frame.height,
            chroma_width: frame.chroma_width,
            chroma_height: frame.chroma_height,
            sample_format: frame.sample_format,
            chroma_plane_layout: frame.chroma_plane_layout(),
            has_alpha: frame.alpha_plane().is_some(),
        };
        let slot_index = state.next_slot;
        state.next_slot = state.next_slot.saturating_add(1);
        if state.slots.get(slot_index).is_none_or(|slot| slot.key != key) {
            let slot = create_upload_slot(device, slot_index, key);
            if slot_index == state.slots.len() {
                state.slots.push(slot);
            } else {
                state.slots[slot_index] = slot;
            }
        }
        let slot = &state.slots[slot_index];
        // CPU ownership transfers once. Repeated layers reuse these immutable
        // textures; finish_candidate installs one remap after GPU completion.
        prepared.buffer.unmap();
        record_prepared_plane_copy(encoder, &prepared.buffer, &slot.luma, prepared.luma);
        record_prepared_plane_copy(encoder, &prepared.buffer, &slot.chroma, prepared.chroma);
        if let Some(chroma_v_copy) = prepared.chroma_v {
            let chroma_v = slot
                .chroma_v
                .as_ref()
                .ok_or(CpuYuvMaterializationError::MissingPlanarChromaTexture)?;
            record_prepared_plane_copy(encoder, &prepared.buffer, chroma_v, chroma_v_copy);
        }
        if let Some(alpha_copy) = prepared.alpha {
            let alpha =
                slot.alpha.as_ref().ok_or(CpuYuvMaterializationError::MissingAlphaTexture)?;
            record_prepared_plane_copy(encoder, &prepared.buffer, alpha, alpha_copy);
        }
        // Views describe immutable texture extent/format, not frame content.
        // Retain them with the physical slot instead of entering the native
        // object allocator for every plane on every realtime candidate.
        let views = slot.views.clone();
        state.recorded_uploads.push(RecordedCpuYuvUpload {
            key: frame_key,
            prepared,
            planes: views.clone(),
        });
        Ok((views, true))
    }

    fn prepare_plane_pass(
        &self,
        decoder: &GpuNativeYuvDecoder,
        device: &wgpu::Device,
        plan: &GpuNativeYuvDecodePlan,
        planes: &CpuYuvUploadedPlaneViews,
    ) -> Result<Arc<crate::GpuNativeYuvPreparedPass>, CpuYuvMaterializationError> {
        let mut state = self.state.lock();
        let slot = state
            .slots
            .get_mut(planes.slot_index)
            .ok_or(CpuYuvMaterializationError::UploadSlotMismatch)?;
        prepare_upload_slot_pass(slot, decoder, device, plan, planes)
    }

    /// Protect one completely admitted candidate, independently of the worker's
    /// bounded transport and speculative result retention. Only current-frame
    /// admission calls this; prewarming cannot replace these physical inputs.
    pub(crate) fn prepare_candidate(
        &self,
        frames: &[Arc<CpuYuvFrame>],
    ) -> Result<bool, CpuYuvMaterializationError> {
        {
            let mut state = self.state.lock();
            let inputs = frames
                .iter()
                .map(|frame| (Self::frame_key(&state, frame), Arc::clone(frame)))
                .collect();
            state.interest.lock().candidate_inputs = inputs;
            Self::drain_worker_results(&mut state);
            Self::trim_speculative_results(&mut state);
        }
        for frame in frames {
            self.prepare(frame)?;
        }
        // Readiness comes from one owner snapshot after all admissions, not an
        // accumulation of per-input observations taken while results change.
        let mut state = self.state.lock();
        Self::drain_worker_results(&mut state);
        let ready = state
            .interest
            .lock()
            .candidate_inputs
            .iter()
            .all(|(key, _)| Self::input_is_ready(&state, *key));
        Ok(ready)
    }

    fn input_is_ready(state: &CpuYuvUploadState, key: CpuYuvFrameUploadKey) -> bool {
        state.prepared.iter().any(|result| result.key == key)
            || state.recorded_uploads.iter().any(|upload| upload.key == key)
    }

    /// Project an already ordered owner horizon into the existing bounded
    /// physical upload set. This does not infer Timeline order or admit output.
    pub(crate) fn prepare_horizon(
        &self,
        frames: &[Arc<CpuYuvFrame>],
    ) -> Result<(), CpuYuvMaterializationError> {
        let selected = {
            let mut state = self.state.lock();
            let mut selected = Vec::with_capacity(CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY);
            for frame in frames {
                let key = Self::frame_key(&state, frame);
                if selected.iter().any(|(existing, _)| *existing == key) {
                    continue;
                }
                selected.push((key, Arc::clone(frame)));
                if selected.len() == CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY {
                    break;
                }
            }
            state.interest.lock().lookahead_inputs = Some(selected.clone());
            Self::drain_worker_results(&mut state);
            Self::trim_speculative_results(&mut state);
            selected
        };
        for (_, frame) in selected {
            self.prepare(&frame)?;
        }
        Ok(())
    }

    fn trim_speculative_results(state: &mut CpuYuvUploadState) {
        // Retire obsolete physical speculation before allocating replacement
        // work. Current admission always retains its independent protection.
        loop {
            let obsolete = {
                let interest = state.interest.lock();
                interest.lookahead_inputs.as_ref().and_then(|inputs| {
                    state.prepared.iter().position(|result| {
                        !interest.is_candidate_input(result.key)
                            && !inputs.iter().any(|(key, _)| *key == result.key)
                    })
                })
            };
            let Some(index) = obsolete else {
                break;
            };
            Self::recycle_unsubmitted_result(state, index);
        }

        while state
            .prepared
            .iter()
            .filter(|result| !state.is_candidate_input(result.key))
            .count()
            > CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY
        {
            let Some(index) =
                state.prepared.iter().position(|result| !state.is_candidate_input(result.key))
            else {
                break;
            };
            Self::recycle_unsubmitted_result(state, index);
        }
    }

    fn recycle_unsubmitted_result(state: &mut CpuYuvUploadState, index: usize) {
        if let Some(result) = state.prepared.remove(index)
            && let Ok(upload) = result.outcome
        {
            // No command buffer has consumed this storage. Its CPU mapping
            // remains valid, so the existing worker pool can overwrite it
            // without a native allocation or an asynchronous GPU remap.
            let _ = state.returned_sender.send(upload.buffer);
        }
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
        if Self::input_is_ready(&state, key) {
            return Ok(true);
        }
        if !state.pending.contains(&key) {
            if self.worker.is_finished() {
                return Err(CpuYuvMaterializationError::UploadWorkerUnavailable);
            }
            if state.pending.len() >= CPU_YUV_UPLOAD_WORKER_CAPACITY {
                return Ok(false);
            }
            let current = state.is_candidate_input(key);
            if !current
                && state
                    .interest
                    .lock()
                    .lookahead_inputs
                    .as_ref()
                    .is_some_and(|inputs| !inputs.iter().any(|(wanted, _)| *wanted == key))
            {
                return Ok(false);
            }
            let speculative = state
                .pending
                .iter()
                .filter(|pending| !state.is_candidate_input(**pending))
                .count()
                + state
                    .prepared
                    .iter()
                    .filter(|result| !state.is_candidate_input(result.key))
                    .count();
            let required_bytes = cpu_yuv_upload_byte_count(frame)?;
            let reusable = state.prepared.iter().position(|result| {
                !state.is_candidate_input(result.key)
                    && result
                        .outcome
                        .as_ref()
                        .is_ok_and(|upload| upload.buffer.size() == required_bytes)
            });
            if current {
                // Current admission may reclaim speculative storage, but it
                // must never reclaim another input of its complete candidate.
                if let Some(index) = reusable {
                    Self::recycle_unsubmitted_result(&mut state, index);
                }
            } else if speculative >= CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY {
                if state.interest.lock().lookahead_inputs.is_some() {
                    return Ok(false);
                }
                let recyclable = reusable.or_else(|| {
                    state.prepared.iter().position(|result| !state.is_candidate_input(result.key))
                });
                let Some(index) = recyclable else {
                    // The bounded speculative capacity is still in flight.
                    // Do not allocate a fifth result before retiring one.
                    return Ok(false);
                };
                Self::recycle_unsubmitted_result(&mut state, index);
            }
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

    fn drain_worker_results(state: &mut CpuYuvUploadState) {
        while let Ok(result) = state.result_receiver.try_recv() {
            state.pending.retain(|pending| *pending != result.key);
            let generation = state.interest.lock().generation;
            if result.key.generation == generation {
                state.prepared.push_back(result);
                Self::trim_speculative_results(state);
            }
        }
    }

    fn frame_key(state: &CpuYuvUploadState, frame: &Arc<CpuYuvFrame>) -> CpuYuvFrameUploadKey {
        CpuYuvFrameUploadKey {
            generation: state.interest.lock().generation,
            frame_identity: Arc::as_ptr(frame) as usize,
        }
    }
}

fn create_upload_slot(
    device: &wgpu::Device,
    slot_index: usize,
    key: CpuYuvUploadKey,
) -> CpuYuvUploadSlot {
    let (luma_format, interleaved_chroma_format, planar_chroma_format) =
        plane_formats(key.sample_format);
    let luma = create_plane_texture(
        device,
        "mondrian.cpu-yuv.luma",
        key.width,
        key.height,
        luma_format,
    );
    let chroma = create_plane_texture(
        device,
        "mondrian.cpu-yuv.chroma",
        key.chroma_width,
        key.chroma_height,
        match key.chroma_plane_layout {
            CpuYuvChromaPlaneLayout::Interleaved => interleaved_chroma_format,
            CpuYuvChromaPlaneLayout::Planar => planar_chroma_format,
        },
    );
    let chroma_v = (key.chroma_plane_layout == CpuYuvChromaPlaneLayout::Planar).then(|| {
        create_plane_texture(
            device,
            "mondrian.cpu-yuv.chroma-v",
            key.chroma_width,
            key.chroma_height,
            planar_chroma_format,
        )
    });
    let alpha = key.has_alpha.then(|| {
        create_plane_texture(
            device,
            "mondrian.cpu-yuv.alpha",
            key.width,
            key.height,
            luma_format,
        )
    });
    let chroma_view = chroma.create_view(&wgpu::TextureViewDescriptor::default());
    let chroma_v_view = match &chroma_v {
        Some(texture) => texture.create_view(&wgpu::TextureViewDescriptor::default()),
        None => chroma_view.clone(),
    };
    let views = CpuYuvUploadedPlaneViews {
        slot_index,
        luma: luma.create_view(&wgpu::TextureViewDescriptor::default()),
        chroma: chroma_view,
        chroma_v: chroma_v_view,
        alpha: alpha
            .as_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default())),
    };
    CpuYuvUploadSlot {
        key,
        luma,
        chroma,
        chroma_v,
        alpha,
        views,
        prepared_pass: None,
    }
}

fn prepare_upload_slot_pass(
    slot: &mut CpuYuvUploadSlot,
    decoder: &GpuNativeYuvDecoder,
    device: &wgpu::Device,
    plan: &GpuNativeYuvDecodePlan,
    planes: &CpuYuvUploadedPlaneViews,
) -> Result<Arc<crate::GpuNativeYuvPreparedPass>, CpuYuvMaterializationError> {
    if slot.views.luma != planes.luma
        || slot.views.chroma != planes.chroma
        || slot.views.chroma_v != planes.chroma_v
        || slot.views.alpha != planes.alpha
    {
        return Err(CpuYuvMaterializationError::UploadSlotMismatch);
    }
    if let Some(prepared) = &slot.prepared_pass
        && prepared.matches_texture_plan(decoder, plan)
    {
        return Ok(Arc::clone(prepared));
    }
    let prepared = Arc::new(decoder.prepare_pass(
        device,
        plan,
        GpuNativeYuvPlaneViews {
            luma: &planes.luma,
            chroma: &planes.chroma,
            chroma_v: &planes.chroma_v,
            alpha: planes.alpha.as_ref(),
        },
    )?);
    slot.prepared_pass = Some(Arc::clone(&prepared));
    Ok(prepared)
}

/// Transport seam only: reconstruction, range, alpha and OCIO remain in
/// `record_cpu_yuv_frame` for both realtime worker and ordered Export queues.
pub(crate) trait CpuYuvPlaneUpload {
    fn upload_planes(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<(CpuYuvUploadedPlaneViews, bool), CpuYuvMaterializationError>;
    fn prepare_planes(
        &self,
        decoder: &GpuNativeYuvDecoder,
        device: &wgpu::Device,
        plan: &GpuNativeYuvDecodePlan,
        planes: &CpuYuvUploadedPlaneViews,
    ) -> Result<Arc<crate::GpuNativeYuvPreparedPass>, CpuYuvMaterializationError>;
}

impl CpuYuvPlaneUpload for CpuYuvUploadRuntime {
    fn upload_planes(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<(CpuYuvUploadedPlaneViews, bool), CpuYuvMaterializationError> {
        self.upload(device, encoder, frame)
    }
    fn prepare_planes(
        &self,
        decoder: &GpuNativeYuvDecoder,
        device: &wgpu::Device,
        plan: &GpuNativeYuvDecodePlan,
        planes: &CpuYuvUploadedPlaneViews,
    ) -> Result<Arc<crate::GpuNativeYuvPreparedPass>, CpuYuvMaterializationError> {
        self.prepare_plane_pass(decoder, device, plan, planes)
    }
}

/// CPU-addressable plane transfer for a non-realtime, ordered GPU visual owner.
/// Queue writes copy host bytes before returning and run before that queue's
/// next submission. A source retains its immutable slot through the whole
/// visual closure; no worker, playback clock or color interpreter is created.
#[derive(Default)]
pub(crate) struct QueueCpuYuvUploadRuntime {
    state: Mutex<QueueCpuYuvUploadState>,
}
#[derive(Default)]
struct QueueCpuYuvUploadState {
    slots: Vec<CpuYuvUploadSlot>,
    frames: Vec<Arc<CpuYuvFrame>>,
}
impl QueueCpuYuvUploadRuntime {
    pub(crate) fn clear_frame(&self) {
        let mut state = self.state.lock();
        let used = state.frames.len();
        if used > 0 {
            state.slots.truncate(used);
        }
        state.frames.clear();
    }
    pub(crate) fn retained_plane_demand(&self) -> Result<(u64, u64), CpuYuvMaterializationError> {
        let state = self.state.lock();
        let mut bytes = 0_u64;
        let mut textures = 0_u64;
        for (index, slot) in state.slots.iter().enumerate() {
            let demand = if let Some(frame) = state.frames.get(index) {
                queue_plane_demand(frame)?
            } else {
                // Cached allocations survive content invalidation. They remain
                // resident even when a later visual closure uses fewer sources.
                let mut bytes = 0_u64;
                let mut textures = 0_u64;
                for texture in [
                    Some(&slot.luma),
                    Some(&slot.chroma),
                    slot.chroma_v.as_ref(),
                    slot.alpha.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    let size = texture.size();
                    let block_bytes = texture
                        .format()
                        .block_copy_size(None)
                        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
                    let allocation = u64::from(size.width)
                        .checked_mul(u64::from(size.height))
                        .and_then(|pixels| pixels.checked_mul(u64::from(block_bytes)))
                        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
                    bytes = bytes
                        .checked_add(allocation)
                        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
                    textures += 1;
                }
                if slot.prepared_pass.is_some() {
                    bytes = bytes
                        .checked_add(crate::GpuNativeYuvPreparedPass::uniform_byte_count())
                        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
                }
                (bytes, textures)
            };
            bytes = bytes
                .checked_add(demand.0)
                .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
            textures = textures
                .checked_add(demand.1)
                .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
        }
        Ok((bytes, textures))
    }
}
/// Plane textures and conservatively retained queue transfer storage are
/// distinct allocations. Count each once, including odd extents/padded rows.
pub(crate) fn queue_plane_demand(
    frame: &CpuYuvFrame,
) -> Result<(u64, u64), CpuYuvMaterializationError> {
    let bytes = cpu_yuv_upload_byte_count(frame)?
        .checked_mul(2)
        .and_then(|value| value.checked_add(crate::GpuNativeYuvPreparedPass::uniform_byte_count()))
        .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
    let textures = match frame.chroma_plane_layout() {
        CpuYuvChromaPlaneLayout::Interleaved => 2,
        CpuYuvChromaPlaneLayout::Planar => 3,
    } + u64::from(frame.alpha_plane().is_some());
    Ok((bytes, textures))
}
impl CpuYuvPlaneUpload for QueueCpuYuvUploadRuntime {
    fn upload_planes(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        frame: &Arc<CpuYuvFrame>,
    ) -> Result<(CpuYuvUploadedPlaneViews, bool), CpuYuvMaterializationError> {
        let mut state = self.state.lock();
        if let Some(index) = state.frames.iter().position(|old| Arc::ptr_eq(old, frame)) {
            return Ok((state.slots[index].views.clone(), false));
        }
        let index = state.frames.len();
        let key = CpuYuvUploadKey {
            width: frame.width,
            height: frame.height,
            chroma_width: frame.chroma_width,
            chroma_height: frame.chroma_height,
            sample_format: frame.sample_format,
            chroma_plane_layout: frame.chroma_plane_layout(),
            has_alpha: frame.alpha_plane().is_some(),
        };
        if state.slots.get(index).is_none_or(|slot| slot.key != key) {
            let slot = create_upload_slot(device, index, key);
            if index == state.slots.len() {
                state.slots.push(slot);
            } else {
                state.slots[index] = slot;
            }
        }
        let slot = &state.slots[index];
        let component_bytes = match frame.sample_format {
            CpuYuvSampleFormat::Unorm8 => 1,
            _ => 2,
        };
        write_plane(
            queue,
            &slot.luma,
            frame.luma_plane(),
            frame.width,
            frame.height,
            component_bytes,
        )?;
        match frame.chroma_planes() {
            CpuYuvChromaPlanes::Interleaved(plane) => write_plane(
                queue,
                &slot.chroma,
                plane,
                frame.chroma_width,
                frame.chroma_height,
                component_bytes * 2,
            )?,
            CpuYuvChromaPlanes::Planar { cb, cr } => {
                write_plane(
                    queue,
                    &slot.chroma,
                    cb,
                    frame.chroma_width,
                    frame.chroma_height,
                    component_bytes,
                )?;
                write_plane(
                    queue,
                    slot.chroma_v
                        .as_ref()
                        .ok_or(CpuYuvMaterializationError::MissingPlanarChromaTexture)?,
                    cr,
                    frame.chroma_width,
                    frame.chroma_height,
                    component_bytes,
                )?;
            }
        }
        if let Some(alpha) = frame.alpha_plane() {
            write_plane(
                queue,
                slot.alpha.as_ref().ok_or(CpuYuvMaterializationError::MissingAlphaTexture)?,
                alpha,
                frame.width,
                frame.height,
                component_bytes,
            )?;
        }
        let views = slot.views.clone();
        state.frames.push(Arc::clone(frame));
        Ok((views, true))
    }
    fn prepare_planes(
        &self,
        decoder: &GpuNativeYuvDecoder,
        device: &wgpu::Device,
        plan: &GpuNativeYuvDecodePlan,
        planes: &CpuYuvUploadedPlaneViews,
    ) -> Result<Arc<crate::GpuNativeYuvPreparedPass>, CpuYuvMaterializationError> {
        let mut state = self.state.lock();
        let slot = state
            .slots
            .get_mut(planes.slot_index)
            .ok_or(CpuYuvMaterializationError::UploadSlotMismatch)?;
        prepare_upload_slot_pass(slot, decoder, device, plan, planes)
    }
}
fn write_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    plane: CpuYuvPlane<'_>,
    width: u32,
    height: u32,
    bytes_per_texel: u32,
) -> Result<(), CpuYuvMaterializationError> {
    validate_plane_bytes(width, height, bytes_per_texel, plane)?;
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
    Ok(())
}

pub(crate) fn record_cpu_yuv_frame(
    decoder: &GpuNativeYuvDecoder,
    uploads: &impl CpuYuvPlaneUpload,
    frame: &Arc<CpuYuvFrame>,
    input_transform: &RenderInputTransform,
    output_width: u32,
    output_height: u32,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
) -> Result<CpuYuvMaterializationRecord, CpuYuvMaterializationError> {
    let source_color_space = frame
        .source_color
        .color_space()
        .ok_or(CpuYuvMaterializationError::InvalidVideoSampling)?;
    let source_texture_format = match frame.sample_format {
        CpuYuvSampleFormat::Unorm8 => GpuNativeDecodedFrameTextureFormat::Nv12,
        CpuYuvSampleFormat::Unorm16Lsb10 | CpuYuvSampleFormat::Unorm16Msb10 => {
            GpuNativeDecodedFrameTextureFormat::P010
        }
        CpuYuvSampleFormat::Unorm16Lsb12 => GpuNativeDecodedFrameTextureFormat::P012,
        CpuYuvSampleFormat::Unorm16 => GpuNativeDecodedFrameTextureFormat::P016,
    };
    let video_sampling = crate::viewer_execution::decoded_video_sampling_for_surface(
        source_color_space,
        frame
            .surface_format()
            .descriptor()
            .ok_or(CpuYuvMaterializationError::InvalidVideoSampling)?,
        frame.video_sampling,
    )
    .ok_or(CpuYuvMaterializationError::InvalidVideoSampling)?;
    let alpha = if frame.alpha_plane().is_some() {
        ColorFrameAlpha::StraightCoverage
    } else {
        ColorFrameAlpha::Opaque
    };
    let encoded_source = GpuColorFrameHandle::new(
        runtime.frame_ids_mut().allocate()?,
        ColorFrameDescriptor {
            width: output_width,
            height: output_height,
            color_space: source_color_space.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha,
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
            alpha,
        },
        product_gpu_working_texture_format(),
        "viewer-cpu-yuv-working",
    )?;
    let chroma_subsampling = match frame.chroma_subsampling {
        CpuYuvChromaSubsampling::Cs420 => GpuYuvChromaSubsampling::Cs420,
        CpuYuvChromaSubsampling::Cs422 => GpuYuvChromaSubsampling::Cs422,
        CpuYuvChromaSubsampling::Cs444 => GpuYuvChromaSubsampling::Cs444,
    };
    let code_alignment = match frame.sample_format {
        CpuYuvSampleFormat::Unorm8 | CpuYuvSampleFormat::Unorm16Msb10 => {
            GpuYuvCodeAlignment::MostSignificant
        }
        CpuYuvSampleFormat::Unorm16Lsb10
        | CpuYuvSampleFormat::Unorm16Lsb12
        | CpuYuvSampleFormat::Unorm16 => GpuYuvCodeAlignment::LeastSignificant,
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
    let (planes, uploaded) = uploads.upload_planes(device, queue, encoder, frame)?;
    let prepared = uploads.prepare_planes(decoder, device, &plan, &planes)?;
    let backend = runtime
        .prepare_fused_yuv_input(
            input_transform,
            &encoded_source,
            &working,
            decoder.fused_texture_input(),
            device,
            queue,
        )
        .map_err(CpuYuvMaterializationError::ColorStage)?;
    let resource_pool = runtime.resource_pool();
    let working_resource = resource_pool.acquire(
        device,
        &GpuColorFrameAllocationPlan::for_handle(working.clone()),
    );
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.cpu-yuv.working-input"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &working_resource.resource().texture_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&backend.pipeline);
        pass.set_bind_group(0, &backend.objects.ocio_bind_group.bind_group, &[]);
        pass.set_bind_group(1, &prepared.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
    if let Some(previous) = runtime.frame_table_mut().insert(working_resource)? {
        if let Some(inserted) = runtime.frame_table_mut().remove(working.id()) {
            resource_pool.release(inserted);
        }
        let displaced = runtime.frame_table_mut().insert(previous)?;
        debug_assert!(displaced.is_none());
        return Err(CpuYuvMaterializationError::LiveResourceCollision);
    }
    Ok(CpuYuvMaterializationRecord {
        output: working,
        stage_diagnostics: RenderColorStageDiagnostics {
            // An upload stage denotes one complete source picture, including
            // every plane, rather than each buffer-to-texture copy command.
            total_stages: 1 + u64::from(uploaded),
            upload_stages: u64::from(uploaded),
            gpu_color_stages: 1,
            stage_pixels: u64::from(output_width)
                .saturating_mul(u64::from(output_height))
                .saturating_add(if uploaded {
                    u64::from(frame.width).saturating_mul(u64::from(frame.height))
                } else {
                    0
                }),
            ..RenderColorStageDiagnostics::default()
        },
    })
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
        CpuYuvSampleFormat::Unorm16Lsb10
        | CpuYuvSampleFormat::Unorm16Lsb12
        | CpuYuvSampleFormat::Unorm16
        | CpuYuvSampleFormat::Unorm16Msb10 => (
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

pub(crate) fn cpu_yuv_upload_byte_count(
    frame: &CpuYuvFrame,
) -> Result<u64, CpuYuvMaterializationError> {
    let component_bytes = frame.sample_format.bytes_per_component() as u32;
    let luma = plane_upload_layout(frame.width, frame.height, component_bytes)?.upload_byte_count;
    let (chroma_components, chroma_planes) = match frame.chroma_plane_layout() {
        CpuYuvChromaPlaneLayout::Interleaved => (2, 1),
        CpuYuvChromaPlaneLayout::Planar => (1, 2),
    };
    let chroma = plane_upload_layout(
        frame.chroma_width,
        frame.chroma_height,
        component_bytes * chroma_components,
    )?
    .upload_byte_count;
    luma.checked_add(
        chroma
            .checked_mul(chroma_planes)
            .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?,
    )
    .and_then(|bytes| {
        bytes.checked_add(if frame.alpha_plane().is_some() {
            luma
        } else {
            0
        })
    })
    .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)
}

fn run_cpu_yuv_upload_worker(
    device: wgpu::Device,
    request_receiver: mpsc::Receiver<CpuYuvUploadWorkerCommand>,
    result_sender: mpsc::SyncSender<CpuYuvUploadWorkerResult>,
    returned_receiver: mpsc::Receiver<wgpu::Buffer>,
    trim_requested: Arc<AtomicBool>,
    interest: Arc<Mutex<CpuYuvUploadInterest>>,
) {
    let mut pool = Vec::with_capacity(CPU_YUV_UPLOAD_POOL_CAPACITY);
    let mut commands = VecDeque::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY);
    let mut retired_commands = Vec::with_capacity(CPU_YUV_UPLOAD_WORKER_CAPACITY);
    'worker: loop {
        if commands.is_empty() {
            match request_receiver.recv() {
                Ok(command) => commands.push_back(command),
                Err(_) => break,
            }
        }
        // Receive a bounded batch from the existing transport. Pending
        // ownership includes this batch; it does not create extra admission.
        for _ in commands.len()..CPU_YUV_UPLOAD_WORKER_CAPACITY {
            match request_receiver.try_recv() {
                Ok(command) => commands.push_back(command),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'worker,
            }
        }
        let command = {
            let interest = interest.lock();
            for index in (0..commands.len()).rev() {
                if matches!(&commands[index], CpuYuvUploadWorkerCommand::Prepare { key, .. }
                    if key.generation != interest.generation)
                    && let Some(command) = commands.remove(index)
                {
                    retired_commands.push(command);
                }
            }
            let next = commands.iter().position(|command| matches!(command, CpuYuvUploadWorkerCommand::Trim))
                .or_else(|| commands.iter().position(|command| matches!(command,
                    CpuYuvUploadWorkerCommand::Prepare { key, .. } if interest.is_candidate_input(*key))))
                .unwrap_or(0);
            commands.remove(next)
        };
        // Payload destruction and all native calls stay outside the shared
        // interest lock, so allocation cannot lock out candidate admission.
        retired_commands.clear();
        let Some(command) = command else {
            continue;
        };
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
    let (alpha, alpha_layout, alpha_plane) = if let Some(plane) = frame.alpha_plane() {
        validate_plane_bytes(frame.width, frame.height, bytes_per_component, plane)?;
        let layout = plane_upload_layout(frame.width, frame.height, bytes_per_component)?;
        let offset = chroma_v_offset
            .checked_add(chroma_v_layout.map_or(0, |layout| layout.upload_byte_count))
            .ok_or(CpuYuvMaterializationError::PlaneExtentOverflow)?;
        (
            Some(PreparedPlaneCopy {
                offset,
                bytes_per_row: layout.upload_bytes_per_row,
                width: frame.width,
                height: frame.height,
            }),
            Some(layout),
            Some(plane),
        )
    } else {
        (None, None, None)
    };
    let total_bytes = cpu_yuv_upload_byte_count(&frame)?;
    let buffer = pool
        .iter()
        .position(|buffer| buffer.size() == total_bytes)
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
        if let (Some(copy), Some(layout), Some(plane)) = (alpha, alpha_layout, alpha_plane) {
            copy_plane_to_mapped(&mut mapped, copy.offset, layout, plane)?;
        }
    }
    // Keep unsubmitted storage mapped. Actual recording consumes that mapping;
    // speculative replacement can instead return it directly to the worker.
    Ok(CpuYuvPreparedUpload { frame, buffer, luma, chroma, chroma_v, alpha })
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
    encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
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
    #[error("compact CPU YUV plane views no longer belong to their upload slot")]
    UploadSlotMismatch,
    #[error("compact CPU YUV upload preparation failed: {0}")]
    UploadPreparation(String),
    #[error("compact CPU YUV plane has {actual} bytes; expected {expected}")]
    PlaneByteCount { expected: usize, actual: usize },
    #[error("compact CPU YUV plane row stride is {actual} bytes; expected at least {minimum}")]
    PlaneStride { minimum: u32, actual: u32 },
    #[error("compact planar CPU YUV upload slot is missing its Cr texture")]
    MissingPlanarChromaTexture,
    #[error("compact CPU YUVA upload slot is missing its alpha texture")]
    MissingAlphaTexture,
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
    ColorStage(crate::color_stage::RenderGpuFusedInputError),
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
    #[ignore = "requires a real GPU"]
    fn repeated_candidate_source_shares_upload_without_aliasing_distinct_frames() {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        mondrian_core::ensure_mondrian_default_ocio_loaded().expect("production OCIO");
        let context = pollster::block_on(crate::GpuContext::new()).expect("physical adapter");
        eprintln!("shared CPU YUV adapter: {:?}", context.adapter.get_info());
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let path = fixture.path().join("distinct.y4m");
        let codes = [64_u8, 192];
        let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C420jpeg\n".to_vec();
        for code in codes {
            bytes.extend_from_slice(b"FRAME\n");
            bytes.extend_from_slice(&[code; 8]);
            bytes.extend_from_slice(&[128; 4]);
        }
        std::fs::write(&path, bytes).expect("fixture");
        let mut decoder = PreviewDecodeSessionContext::new();
        let mut frames = Vec::new();
        for index in 0..2 {
            let mut request = PreviewDecodeRequest::new(
                &path,
                mondrian_core::SourceSampleTarget::covering(
                    mondrian_core::TimelineTime::new(index, 25).expect("sample time"),
                ),
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewSourceColorContract::automatic(
                    mondrian_core::ColorSpace::Rec709,
                    DecodedVideoRange::Limited,
                )
                .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
            );
            request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
            let PreviewDecodeOutcome::CpuYuvFrame(frame) =
                decoder.decode_cancellable(request, || false).expect("software frame")
            else {
                panic!("compact frame required")
            };
            frames.push(Arc::new(frame));
        }
        decoder.clear();
        drop(decoder);
        let uploads = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !uploads.prepare_candidate(&frames).expect("admit sources") {
            assert!(std::time::Instant::now() < deadline, "prepare deadline");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let yuv = GpuNativeYuvDecoder::new(&context.device);
        let mut color = RenderGpuOutputBoundaryRuntime::new().expect("color owner");
        let transform = RenderInputTransform::to_working_gpu(
            mondrian_core::WorkingColorSpace::LinearRec2020,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );
        let mut encoder = context.device.create_command_encoder(&Default::default());
        let mut reads = Vec::new();
        let mut stages = RenderColorStageDiagnostics::default();
        for (index, width, height, expected_uploads) in [(0, 4, 2, 1), (0, 2, 1, 0), (1, 4, 2, 1)] {
            let record = record_cpu_yuv_frame(
                &yuv,
                &uploads,
                &frames[index],
                &transform,
                width,
                height,
                &mut color,
                &context.device,
                &context.queue,
                &mut encoder,
            )
            .expect("production materialization");
            assert_eq!(record.stage_diagnostics.upload_stages, expected_uploads);
            assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
            stages.accumulate(record.stage_diagnostics);
            let plan = crate::GpuColorFrameReadbackPlan::encoded_rgba32float(record.output.clone())
                .expect("diagnostic readback");
            let output = color.frame_table().get(&record.output).expect("working output");
            let read = crate::GpuColorFrameReadback::record_copy(
                &context.device,
                &mut encoder,
                &plan,
                output,
            )
            .expect("copy working output");
            reads.push((index, plan, read));
        }
        assert_eq!(stages.total_stages, 5);
        assert_eq!(stages.upload_stages, 2);
        assert_eq!(stages.gpu_color_stages, 3);
        assert_eq!(stages.stage_pixels, 34);
        assert_eq!(uploads.state.lock().slots.len(), 2);
        uploads.finish_candidate(&encoder);
        context.queue.submit([encoder.finish()]);
        let mut pixels = Vec::new();
        for (index, plan, read) in reads {
            let (sender, receiver) = mpsc::channel();
            read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_secs(30)),
                })
                .expect("real completion");
            receiver
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("callback")
                .expect("map");
            let actual = plan
                .unpack_mapped_rgba32float(
                    &read.slice(..).get_mapped_range().expect("mapped bytes"),
                )
                .expect("working pixels");
            read.unmap();
            pixels.push((index, actual));
        }
        let mut retirement = uploads.into_retirement();
        while retirement.poll().is_none() {
            assert!(std::time::Instant::now() < deadline, "retirement deadline");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        drop(retirement);
        for (index, actual) in pixels {
            // Independent limited-range neutral source, followed by production CPU OCIO.
            let encoded = (f32::from(codes[index]) - 16.0) / 219.0;
            let mut expected = [encoded, encoded, encoded, 1.0];
            mondrian_core::ColorEngine::mondrian_standard()
                .convert_identity_float(
                    &mut expected,
                    mondrian_core::ColorSpace::Rec709.into(),
                    mondrian_core::WorkingColorSpace::LinearRec2020.into(),
                )
                .expect("CPU reference");
            for pixel in actual.chunks_exact(4) {
                for channel in 0..4 {
                    assert!(
                        pixel[channel].is_finite()
                            && (pixel[channel] - expected[channel]).abs() <= 1.0e-3,
                        "distinct source {index}: {pixel:?} vs {expected:?}"
                    );
                }
            }
        }
        eprintln!("shared CPU YUV: working_passes=3 physical_uploads=2 slots=2 owner=returned");
    }

    #[test]
    #[ignore = "requires a real GPU and MONDRIAN_YUVA12_DECODE_FIXTURE"]
    fn yuva12_cpu_export_and_fused_gpu_input_match_for_actual_decode() {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        let path = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_YUVA12_DECODE_FIXTURE").expect("explicit YUVA12 fixture"),
        );
        mondrian_core::ensure_mondrian_default_ocio_loaded().expect("production OCIO");
        let context =
            pollster::block_on(crate::GpuContext::new()).expect("required physical adapter");
        eprintln!("YUVA12 CPU/GPU adapter: {:?}", context.adapter.get_info());
        let mut decoder = PreviewDecodeSessionContext::new();
        let request = |representation| {
            let mut request = PreviewDecodeRequest::new(
                &path,
                mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO),
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewSourceColorContract::automatic(
                    mondrian_core::ColorSpace::Rec709,
                    DecodedVideoRange::Limited,
                )
                .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
            );
            request.representation = representation;
            request
        };
        let PreviewDecodeOutcome::FloatFrame(cpu) = decoder
            .decode_cancellable(request(PreviewDecodeRepresentation::NativeCpu), || false)
            .expect("Export CPU-addressable decode")
        else {
            panic!("unclamped CPU Float32 required")
        };
        let PreviewDecodeOutcome::CpuYuvFrame(compact) = decoder
            .decode_cancellable(request(PreviewDecodeRepresentation::CompactCpuYuv), || {
                false
            })
            .expect("Preview compact decode")
        else {
            panic!("compact YUVA required")
        };
        assert_eq!(
            compact.surface_format(),
            mondrian_media::DecodedVideoSurfaceFormat::Yuva444p12le
        );
        assert_eq!(
            compact.decode_execution,
            PreviewDecodeExecutionPath::SoftwareCpu
        );
        assert_eq!((cpu.width, cpu.height), (compact.width, compact.height));
        decoder.clear();
        drop(decoder);
        let mut expected = cpu.rgba().to_vec();
        mondrian_core::ColorEngine::mondrian_standard()
            .convert_identity_float(
                &mut expected,
                mondrian_core::ColorSpace::Rec709.into(),
                mondrian_core::WorkingColorSpace::LinearRec2020.into(),
            )
            .expect("shared CPU OCIO input");
        let frame = Arc::new(compact);
        let uploads = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !uploads.prepare_candidate(&[Arc::clone(&frame)]).expect("admission") {
            assert!(std::time::Instant::now() < deadline, "upload deadline");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let yuv_decoder = GpuNativeYuvDecoder::new(&context.device);
        let mut color = RenderGpuOutputBoundaryRuntime::new().expect("color owner");
        let transform = RenderInputTransform::to_working_gpu(
            mondrian_core::WorkingColorSpace::LinearRec2020,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );
        let mut encoder = context.device.create_command_encoder(&Default::default());
        let recorded = record_cpu_yuv_frame(
            &yuv_decoder,
            &uploads,
            &frame,
            &transform,
            frame.width,
            frame.height,
            &mut color,
            &context.device,
            &context.queue,
            &mut encoder,
        )
        .expect("production fused YUVA/OCIO");
        assert_eq!(recorded.stage_diagnostics.upload_stages, 1);
        assert_eq!(recorded.stage_diagnostics.gpu_color_stages, 1);
        let working = recorded.output;
        assert_eq!(
            working.descriptor().alpha,
            ColorFrameAlpha::StraightCoverage
        );
        let plan = crate::GpuColorFrameReadbackPlan::encoded_rgba32float(working.clone())
            .expect("diagnostic readback");
        let output = color.frame_table().get(&working).expect("working frame");
        let read =
            crate::GpuColorFrameReadback::record_copy(&context.device, &mut encoder, &plan, output)
                .expect("diagnostic copy");
        uploads.finish_candidate(&encoder);
        context.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .expect("GPU completion");
        receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("callback")
            .expect("map");
        let actual = plan
            .unpack_mapped_rgba32float(&read.slice(..).get_mapped_range().expect("mapped bytes"))
            .expect("float pixels");
        read.unmap();
        let max_delta =
            actual.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        let all_finite = actual.iter().chain(&expected).all(|value| value.is_finite());
        let mut retirement = uploads.into_retirement();
        while retirement.poll().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "upload shutdown deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        drop(retirement);
        eprintln!(
            "YUVA12 CPU/GPU pixels={} max_delta={max_delta} upload_owner=returned",
            actual.len() / 4
        );
        assert_eq!(actual.len(), expected.len());
        assert!(
            all_finite && max_delta <= 1.0e-3,
            "existing CPU/GPU working-space parity budget: {max_delta}"
        );
    }

    #[test]
    #[ignore = "requires an explicitly available GPU adapter"]
    fn persistent_upload_slot_reuses_native_plane_views() {
        exercise_persistent_upload_slot(false);
    }

    #[test]
    #[ignore = "requires an explicitly available GPU adapter"]
    fn persistent_upload_slot_reuses_native_yuv_bindings() {
        exercise_persistent_upload_slot(true);
    }

    #[test]
    #[ignore = "requires an explicitly available GPU adapter"]
    fn unsubmitted_prewarm_recycles_native_storage_without_growing_capacity() {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        let context = pollster::block_on(crate::GpuContext::new()).expect("required GPU");
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let path = fixture.path().join("unsubmitted.y4m");
        let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C420jpeg\n".to_vec();
        for value in 64..72 {
            bytes.extend_from_slice(b"FRAME\n");
            bytes.extend_from_slice(&[value; 12]);
        }
        std::fs::write(&path, bytes).expect("fixture");
        let mut decoder = PreviewDecodeSessionContext::new();
        let runtime = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut buffers = Vec::new();
        let mut samples = Vec::new();
        for index in 0..8 {
            let mut request = PreviewDecodeRequest::new(
                &path,
                mondrian_core::SourceSampleTarget::covering(
                    mondrian_core::TimelineTime::new(index, 25).expect("exact frame time"),
                ),
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewSourceColorContract::automatic(
                    mondrian_core::ColorSpace::Rec709,
                    DecodedVideoRange::Limited,
                )
                .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
            );
            request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
            let PreviewDecodeOutcome::CpuYuvFrame(frame) =
                decoder.decode_cancellable(request, || false).expect("decode")
            else {
                panic!("compact frame")
            };
            let frame = Arc::new(frame);
            while !runtime.prepare(&frame).expect("prepare") {
                assert!(std::time::Instant::now() < deadline, "prewarm deadline");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let state = runtime.state.lock();
            let key = CpuYuvUploadRuntime::frame_key(&state, &frame);
            let upload = state
                .prepared
                .iter()
                .find(|result| result.key == key)
                .expect("prepared input")
                .outcome
                .as_ref()
                .expect("prepared upload");
            samples.push(upload.buffer.slice(..).get_mapped_range().ok().map(|data| data[0]));
            if !buffers.contains(&upload.buffer) {
                buffers.push(upload.buffer.clone());
            }
        }
        decoder.clear();
        let mut retirement = runtime.into_retirement();
        while retirement.poll().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "upload owner closure deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        drop(retirement);
        assert!(
            buffers.len() <= CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY,
            "unsubmitted replacement allocated {} native buffers for a {}-entry retention grant",
            buffers.len(),
            CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY
        );
        assert_eq!(
            samples,
            (64..72).map(Some).collect::<Vec<_>>(),
            "unsubmitted buffers stay mapped and replacement copies the new source pixels"
        );
    }

    #[test]
    #[ignore = "requires an explicitly available GPU adapter"]
    fn admitted_candidate_retains_unscheduled_source_identity() {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        let context = pollster::block_on(crate::GpuContext::new()).expect("required GPU");
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let path = fixture.path().join("retained.y4m");
        let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C420jpeg\nFRAME\n".to_vec();
        bytes.extend_from_slice(&[128; 12]);
        std::fs::write(&path, bytes).expect("fixture");
        let mut decoder = PreviewDecodeSessionContext::new();
        let mut request = PreviewDecodeRequest::new(
            &path,
            mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO),
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewSourceColorContract::automatic(
                mondrian_core::ColorSpace::Rec709,
                DecodedVideoRange::Limited,
            )
            .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
        );
        request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
        let PreviewDecodeOutcome::CpuYuvFrame(frame) =
            decoder.decode_cancellable(request, || false).expect("decode")
        else {
            panic!("compact frame")
        };
        decoder.clear();
        let frames = (0..6).map(|_| Arc::new(frame.clone())).collect::<Vec<_>>();
        let weak = Arc::downgrade(&frames[5]);
        let runtime = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let (entered, entry) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let first = AtomicBool::new(true);
        runtime.install_completion_waker(move || {
            if first.swap(false, Ordering::AcqRel) {
                entered.send(()).expect("worker entered");
                released
                    .lock()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("release worker");
            }
        });
        assert!(!runtime.prepare(&frames[0]).expect("schedule first input"));
        entry.recv_timeout(std::time::Duration::from_secs(5)).expect("worker paused");
        assert!(!runtime.prepare_candidate(&frames).expect("admit complete candidate"));
        {
            let state = runtime.state.lock();
            assert!(
                !state.pending.contains(&CpuYuvUploadRuntime::frame_key(&state, &frames[5])),
                "last input must still be outside the bounded worker transport"
            );
        }
        drop(frames);
        let retained = weak.upgrade().is_some();
        release.send(()).expect("release worker");
        let mut retirement = runtime.into_retirement();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while retirement.poll().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "owner closure deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        drop(retirement);
        assert!(
            retained,
            "admitted input identity must retain its source even before worker scheduling"
        );
        assert!(
            weak.upgrade().is_none(),
            "retirement must release the admitted source"
        );
    }

    #[test]
    #[ignore = "requires an explicitly available GPU adapter"]
    fn farther_prewarm_does_not_evict_nearest_staged_input() {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        let context = pollster::block_on(crate::GpuContext::new()).expect("required GPU");
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let path = fixture.path().join("priority.y4m");
        let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C420jpeg\nFRAME\n".to_vec();
        bytes.extend_from_slice(&[128; 12]);
        std::fs::write(&path, bytes).expect("fixture");
        let mut decoder = PreviewDecodeSessionContext::new();
        let mut request = PreviewDecodeRequest::new(
            &path,
            mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO),
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewSourceColorContract::automatic(
                mondrian_core::ColorSpace::Rec709,
                DecodedVideoRange::Limited,
            )
            .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
        );
        request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
        let PreviewDecodeOutcome::CpuYuvFrame(frame) =
            decoder.decode_cancellable(request, || false).expect("decode")
        else {
            panic!("compact frame")
        };
        decoder.clear();
        let frames = (0..5).map(|_| Arc::new(frame.clone())).collect::<Vec<_>>();
        let runtime = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for count in 1..=frames.len() {
            loop {
                runtime.prepare_horizon(&frames[..count]).expect("ordered prewarm");
                let ready = {
                    let mut state = runtime.state.lock();
                    CpuYuvUploadRuntime::drain_worker_results(&mut state);
                    frames.iter().take(count.min(CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY)).all(
                        |frame| {
                            CpuYuvUploadRuntime::input_is_ready(
                                &state,
                                CpuYuvUploadRuntime::frame_key(&state, frame),
                            )
                        },
                    )
                };
                if ready {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "prewarm deadline");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let nearest_ready = {
            let mut state = runtime.state.lock();
            CpuYuvUploadRuntime::drain_worker_results(&mut state);
            CpuYuvUploadRuntime::input_is_ready(
                &state,
                CpuYuvUploadRuntime::frame_key(&state, &frames[0]),
            )
        };
        let initial_buffers = {
            let state = runtime.state.lock();
            assert!(!CpuYuvUploadRuntime::input_is_ready(
                &state,
                CpuYuvUploadRuntime::frame_key(&state, &frames[4])
            ));
            state
                .prepared
                .iter()
                .map(|result| result.outcome.as_ref().expect("prepared upload").buffer.clone())
                .collect::<Vec<_>>()
        };
        loop {
            runtime.prepare_horizon(&frames[1..]).expect("advance physical horizon");
            let ready = {
                let mut state = runtime.state.lock();
                CpuYuvUploadRuntime::drain_worker_results(&mut state);
                frames[1..].iter().all(|frame| {
                    CpuYuvUploadRuntime::input_is_ready(
                        &state,
                        CpuYuvUploadRuntime::frame_key(&state, frame),
                    )
                })
            };
            if ready {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "advanced horizon deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        {
            let state = runtime.state.lock();
            assert_eq!(
                state.prepared.len(),
                CPU_YUV_SPECULATIVE_PREPARATION_CAPACITY
            );
            assert!(
                state.prepared.iter().all(|result| initial_buffers
                    .contains(&result.outcome.as_ref().expect("prepared upload").buffer)),
                "horizon advancement must reuse retired mapped storage"
            );
            assert!(!CpuYuvUploadRuntime::input_is_ready(
                &state,
                CpuYuvUploadRuntime::frame_key(&state, &frames[0])
            ));
        }
        runtime.prepare_candidate(&[Arc::clone(&frames[1])]).expect("current admission");
        runtime.prepare_horizon(&[]).expect("retire speculative horizon");
        {
            let state = runtime.state.lock();
            assert_eq!(
                state.prepared.len(),
                1,
                "empty horizon preserves only the admitted current input"
            );
        }
        drop(initial_buffers);
        let mut retirement = runtime.into_retirement();
        while retirement.poll().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "owner closure deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        assert!(
            nearest_ready,
            "farther staged input evicted the nearest physical preparation"
        );
    }

    #[test]
    #[ignore = "requires an explicitly available GPU adapter"]
    fn queued_current_upload_precedes_speculative_uploads() {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        let context = pollster::block_on(crate::GpuContext::new()).expect("required GPU");
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let path = fixture.path().join("priority.y4m");
        let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C420jpeg\nFRAME\n".to_vec();
        bytes.extend_from_slice(&[128; 12]);
        std::fs::write(&path, bytes).expect("fixture");
        let mut decoder = PreviewDecodeSessionContext::new();
        let mut request = PreviewDecodeRequest::new(
            &path,
            mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO),
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewSourceColorContract::automatic(
                mondrian_core::ColorSpace::Rec709,
                DecodedVideoRange::Limited,
            )
            .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
        );
        request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
        let PreviewDecodeOutcome::CpuYuvFrame(frame) =
            decoder.decode_cancellable(request, || false).expect("decode")
        else {
            panic!("compact frame")
        };
        decoder.clear();
        let frames = (0..4).map(|_| Arc::new(frame.clone())).collect::<Vec<_>>();
        let runtime = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let (entered, entry) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        runtime.install_completion_waker(move || {
            entered.send(()).expect("first upload complete");
            released
                .lock()
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("release worker");
        });
        assert!(!runtime.prepare(&frames[0]).expect("first upload"));
        entry.recv_timeout(std::time::Duration::from_secs(5)).expect("worker paused");
        let (completed, completion) = mpsc::channel();
        for (index, frame) in frames.iter().enumerate().skip(1) {
            let completed = completed.clone();
            runtime.install_completion_waker(move || {
                let _ = completed.send(index);
            });
            assert!(!runtime.prepare(frame).expect("queued speculative input"));
        }
        assert!(!runtime
            .prepare_candidate(&[Arc::clone(&frames[3])])
            .expect("promote current input"));
        release.send(()).expect("release worker");
        let first = completion
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("next completion");
        let mut retirement = runtime.into_retirement();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while retirement.poll().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "owner closure deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        drop(retirement);
        assert_eq!(
            first, 3,
            "already queued current input must precede unstarted speculation"
        );
    }

    fn exercise_persistent_upload_slot(check_bindings: bool) {
        use mondrian_media::preview::*;
        use mondrian_media::{DecodedVideoMatrix, DecodedVideoRange};
        let context = pollster::block_on(crate::GpuContext::new()).expect("required GPU");
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let path = fixture.path().join("planes.y4m");
        let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C420jpeg\nFRAME\n".to_vec();
        bytes.extend_from_slice(&[128; 12]);
        std::fs::write(&path, bytes).expect("fixture");
        let mut decoder = PreviewDecodeSessionContext::new();
        let mut request = PreviewDecodeRequest::new(
            &path,
            mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO),
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewSourceColorContract::automatic(
                mondrian_core::ColorSpace::Rec709,
                DecodedVideoRange::Limited,
            )
            .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709),
        );
        request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
        let PreviewDecodeOutcome::CpuYuvFrame(frame) =
            decoder.decode_cancellable(request, || false).expect("software decode")
        else {
            panic!("compact frame")
        };
        decoder.clear();
        let frame = Arc::new(frame);
        let runtime = CpuYuvUploadRuntime::new(&context.device).expect("upload owner");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut views = Vec::new();
        let mut bindings = Vec::new();
        let yuv_decoder = GpuNativeYuvDecoder::new(&context.device);
        let sampling = crate::viewer_execution::decoded_video_sampling_for_surface(
            mondrian_core::ColorSpace::Rec709,
            frame.surface_format().descriptor().expect("physical contract"),
            frame.video_sampling,
        )
        .expect("sampling contract");
        let mut plan = GpuNativeYuvDecodePlan::new_with_plane_layout(
            GpuNativeDecodedFrameTextureFormat::Nv12,
            GpuYuvChromaSubsampling::Cs420,
            GpuYuvChromaPlaneLayout::Planar,
            GpuYuvCodeAlignment::MostSignificant,
            GpuNativeVideoExtent { width: 4, height: 2 },
            GpuNativeVideoExtent { width: 4, height: 2 },
            GpuNativeVideoExtent { width: 4, height: 2 },
            sampling,
            GpuColorFrameHandle::new(
                crate::GpuColorFrameId::from_raw(1),
                ColorFrameDescriptor {
                    width: 4,
                    height: 2,
                    color_space: mondrian_core::ColorSpace::Rec709.into(),
                    domain: ColorFrameDomain::Source,
                    encoding: ColorFrameEncoding::EncodedFloat,
                    residency: ColorFrameResidency::Gpu,
                    alpha: ColorFrameAlpha::Opaque,
                },
                product_gpu_working_texture_format(),
                "binding-regression",
            )
            .expect("encoded source"),
        )
        .expect("YUV plan");
        for index in 0..5 {
            if index == 3 {
                plan.video_sampling.range = crate::GpuVideoRange::Full;
            }
            if index == 2 {
                runtime.clear();
            }
            runtime.begin_frame();
            while !runtime.prepare(&frame).expect("prepare") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "upload preparation deadline"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let mut encoder = context.device.create_command_encoder(&Default::default());
            let (planes, uploaded) =
                runtime.upload(&context.device, &mut encoder, &frame).expect("record upload");
            assert!(
                uploaded,
                "each new candidate must record its own physical upload"
            );
            let (repeated, uploaded) =
                runtime.upload(&context.device, &mut encoder, &frame).expect("reuse planes");
            assert!(
                !uploaded,
                "repeated layer must share the candidate's plane content"
            );
            assert_eq!(planes.luma, repeated.luma);
            assert_eq!(planes.chroma, repeated.chroma);
            assert_eq!(planes.chroma_v, repeated.chroma_v);
            assert_eq!(planes.alpha, repeated.alpha);
            assert_eq!(runtime.state.lock().slots.len(), 1);
            views.push(planes);
            bindings.push(
                runtime
                    .prepare_plane_pass(
                        &yuv_decoder,
                        &context.device,
                        &plan,
                        views.last().expect("uploaded planes"),
                    )
                    .expect("prepare production YUV binding"),
            );
            drop(encoder);
            runtime.discard_candidate();
        }
        let mut retirement = runtime.into_retirement();
        while retirement.poll().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "upload owner closure deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            retirement.poll(),
            Some(crate::ViewerCpuYuvUploadWorkerExit::Returned)
        );
        drop(retirement);
        if check_bindings {
            assert_eq!(
                bindings[0].bind_group, bindings[1].bind_group,
                "unchanged physical planes and sampling must retain immutable bindings"
            );
            assert_ne!(
                bindings[1].bind_group, bindings[2].bind_group,
                "generation clear must retire immutable bindings"
            );
            assert_ne!(
                bindings[2].bind_group, bindings[3].bind_group,
                "range change must rebuild immutable bindings"
            );
            assert_eq!(
                bindings[3].bind_group, bindings[4].bind_group,
                "new sampling contract must become reusable"
            );
        }
        assert_eq!(
            views[0].luma, views[1].luma,
            "one physical slot must retain its luma view"
        );
        assert_eq!(
            views[0].chroma, views[1].chroma,
            "one physical slot must retain its Cb view"
        );
        assert_eq!(
            views[0].chroma_v, views[1].chroma_v,
            "one physical slot must retain its Cr view"
        );
        assert_ne!(
            views[0].luma, views[2].luma,
            "retired slot must not reuse an old view"
        );
        assert_ne!(
            views[0].chroma, views[2].chroma,
            "retired slot must not reuse an old chroma view"
        );
    }

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
