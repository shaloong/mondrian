//! Owned public VideoToolbox session; compressed-only FFmpeg muxing.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use super::{
    ResidentEncodeBitDepth, ResidentEncodeChromaLocation, ResidentEncodeColorimetry,
    ResidentEncodeError, ResidentHevcEncoderConfig,
};
use ffmpeg_next as ffmpeg;

const NATIVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Identity of the actual unified-memory Metal device producing encoder input.
#[derive(Clone)]
pub struct MetalResidentEncodeDeviceRoot {
    registry_id: u64,
}
impl MetalResidentEncodeDeviceRoot {
    /// Bind the active renderer's physical Metal device.
    ///
    /// # Safety
    /// The caller must obtain this ID from its actual Metal device, verify
    /// `hasUnifiedMemory`, and keep that device alive through all GPU writes.
    pub unsafe fn from_unified_metal_device(registry_id: u64) -> Result<Self, ResidentEncodeError> {
        if registry_id == 0 {
            return Err(native_error("Metal device", "missing registry identity"));
        }
        Ok(Self { registry_id })
    }
    /// Actual renderer device registry identity.
    pub fn registry_id(&self) -> u64 {
        self.registry_id
    }
}

#[repr(C)]
struct NativeContract {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    sar_num: u32,
    sar_den: u32,
    ten_bit: u32,
    full_range: u32,
    primaries: u32,
    transfer: u32,
    matrix: u32,
    centered_chroma: u32,
    gop: u32,
    quantizer: u32,
    pool_size: u32,
    metal_registry_id: u64,
}
#[repr(C)]
#[derive(Default)]
struct NativeError {
    status: i32,
    stage: u32,
}
#[repr(C)]
struct NativePacket {
    bytes: *const u8,
    size: usize,
    pts: i64,
    key: u32,
    hardware_observed: u32,
}
unsafe extern "C" {
    fn mdn_vt_encoder_create(
        contract: *const NativeContract,
        error: *mut NativeError,
    ) -> *mut c_void;
    fn mdn_vt_encoder_acquire(session: *mut c_void, error: *mut NativeError) -> *mut c_void;
    fn mdn_vt_encoder_submit(
        session: *mut c_void,
        buffer: *mut c_void,
        index: u64,
        packet: *mut NativePacket,
        error: *mut NativeError,
    ) -> i32;
    fn mdn_vt_encoder_close(session: *mut c_void);
    fn mdn_vt_surface_release(surface: *mut c_void);
}

/// Borrowed CVPixelBuffer retained by one move-only codec input.
#[derive(Clone, Copy)]
pub struct MetalResidentEncodeSurfaceView<'a> {
    /// Native CVPixelBuffer; never exposes CPU pixel addresses.
    pub pixel_buffer: *mut c_void,
    /// Coded extent.
    pub width: u32,
    /// Coded extent.
    pub height: u32,
    /// NV12/P010 precision.
    pub bit_depth: ResidentEncodeBitDepth,
    /// Actual unified Metal device identity.
    pub registry_id: u64,
    _owner: std::marker::PhantomData<&'a MetalResidentEncodeInputFrame>,
}
/// Codec-owned IOSurface awaiting the renderer's completed GPU writes.
pub struct MetalResidentEncodeInputFrame {
    buffer: NonNull<c_void>,
    owner: Arc<()>,
    config: Arc<ResidentHevcEncoderConfig>,
    registry_id: u64,
}
// SAFETY: each +1 CoreVideo reference moves exclusively between the codec
// worker and producer. CoreVideo ref-counting is thread-safe; writes are never concurrent.
unsafe impl Send for MetalResidentEncodeInputFrame {}
impl Drop for MetalResidentEncodeInputFrame {
    fn drop(&mut self) {
        unsafe { mdn_vt_surface_release(self.buffer.as_ptr()) };
    }
}
impl MetalResidentEncodeInputFrame {
    /// Borrow the exact owned destination.
    pub fn surface(&self) -> MetalResidentEncodeSurfaceView<'_> {
        MetalResidentEncodeSurfaceView {
            pixel_buffer: self.buffer.as_ptr(),
            width: self.config.width,
            height: self.config.height,
            bit_depth: self.config.bit_depth,
            registry_id: self.registry_id,
            _owner: std::marker::PhantomData,
        }
    }
    /// Promote only after both GPU plane writes completed successfully.
    ///
    /// # Safety
    /// The producer must prove completion on the actual owning Metal device,
    /// including all reads of its renderer lease, before calling this function.
    pub unsafe fn assume_producer_copy_complete(self) -> MetalResidentEncodeReadyFrame {
        MetalResidentEncodeReadyFrame { input: self }
    }
}
/// Move-only input whose native GPU writes have completed.
pub struct MetalResidentEncodeReadyFrame {
    input: MetalResidentEncodeInputFrame,
}

/// Cumulative codec evidence from actual submissions and consuming shutdown.
#[derive(Clone, Copy, Default, Debug)]
pub struct VideoToolboxResidentHevcEncoderSessionDiagnostics {
    /// Acquired native surfaces.
    pub surfaces_acquired: u64,
    /// Encoded frames.
    pub frames_submitted: u64,
    /// Compressed packets muxed.
    pub packets_written: u64,
    /// Native hardware property observed after actual output.
    pub hardware_observed: bool,
    /// Pixel readbacks (this adapter has no such API).
    pub cpu_pixel_readbacks: u64,
    /// Rawvideo transport (compressed packets only).
    pub rawvideo_pipe_bytes: u64,
    /// CPU pixel uploads (native surfaces only).
    pub cpu_pixel_uploads: u64,
    /// Actual codec-worker exit confirmed.
    pub consuming_shutdown: bool,
}

type Work = Box<dyn FnOnce(&mut Option<NativeSession>) + Send>;
/// One exclusive platform worker; no queue, scheduling or Export authority.
pub struct VideoToolboxResidentHevcEncoderSession {
    sender: Option<mpsc::SyncSender<Work>>,
    worker: Option<JoinHandle<()>>,
    closed: mpsc::Receiver<()>,
    diagnostics: VideoToolboxResidentHevcEncoderSessionDiagnostics,
}
impl VideoToolboxResidentHevcEncoderSession {
    /// Open a bounded native codec/mux owner under the renderer device identity.
    pub fn open(
        root: &MetalResidentEncodeDeviceRoot,
        config: ResidentHevcEncoderConfig,
    ) -> Result<Self, ResidentEncodeError> {
        config.validate()?;
        let (sender, receiver) = mpsc::sync_channel::<Work>(1);
        let (opened_tx, opened) = mpsc::sync_channel(1);
        let (closed_tx, closed) = mpsc::sync_channel(1);
        let root = root.clone();
        let worker = std::thread::Builder::new()
            .name("mondrian-vt-encode".into())
            .spawn(move || {
                let mut state = match NativeSession::open(root, config) {
                    Ok(state) => {
                        let _ = opened_tx.send(Ok(()));
                        Some(state)
                    }
                    Err(error) => {
                        let _ = opened_tx.send(Err(error));
                        None
                    }
                };
                while state.is_some() {
                    match receiver.recv() {
                        Ok(work) => work(&mut state),
                        Err(_) => break,
                    }
                }
                // Invalidation and callback storage release precede the exit receipt.
                drop(state);
                let _ = closed_tx.send(());
            })
            .map_err(|e| native_error("worker spawn", e))?;
        let mut result = Self {
            sender: Some(sender),
            worker: Some(worker),
            closed,
            diagnostics: Default::default(),
        };
        opened
            .recv_timeout(NATIVE_TIMEOUT)
            .map_err(|e| native_error("worker open", e))??;
        result.diagnostics = Default::default();
        Ok(result)
    }
    fn request<T: Send + 'static>(
        &mut self,
        operation: impl FnOnce(&mut Option<NativeSession>) -> Result<T, ResidentEncodeError>
            + Send
            + 'static,
    ) -> Result<T, ResidentEncodeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(ResidentEncodeError::AlreadyFinished)?
            .send(Box::new(move |state| {
                let result = operation(state);
                if result.is_err() {
                    drop(state.take());
                }
                let _ = tx.send(result);
            }))
            .map_err(|e| native_error("worker admission", e))?;
        match rx.recv_timeout(NATIVE_TIMEOUT) {
            Ok(result) => result,
            Err(error) => {
                // The worker retains every native parent and input until the
                // actual native operation returns. Never release on a timeout.
                self.sender.take();
                Err(native_error("worker completion", error))
            }
        }
    }
    /// Acquire an allocation-threshold-bounded pool surface.
    pub fn acquire_input_frame(
        &mut self,
    ) -> Result<MetalResidentEncodeInputFrame, ResidentEncodeError> {
        let input = self.request(|state| {
            state.as_mut().ok_or(ResidentEncodeError::AlreadyFinished)?.acquire()
        })?;
        self.diagnostics.surfaces_acquired += 1;
        Ok(input)
    }
    /// Encode one completed GPU surface and mux its compressed packet.
    pub fn submit_input_frame(
        &mut self,
        frame: MetalResidentEncodeReadyFrame,
        index: u64,
    ) -> Result<(), ResidentEncodeError> {
        self.request(move |state| {
            state.as_mut().ok_or(ResidentEncodeError::AlreadyFinished)?.submit(frame, index)
        })?;
        self.diagnostics.frames_submitted += 1;
        self.diagnostics.packets_written += 1;
        self.diagnostics.hardware_observed = true;
        Ok(())
    }
    /// Consume native callback ownership and finalize the video-only mux.
    pub fn finish(&mut self) -> Result<(), ResidentEncodeError> {
        self.request(|state| state.take().ok_or(ResidentEncodeError::AlreadyFinished)?.finish())?;
        self.sender.take();
        self.wait_closed()?;
        self.diagnostics.consuming_shutdown = true;
        Ok(())
    }
    /// Abort without accepting a partial artifact, and prove native owner exit.
    pub fn shutdown(&mut self) -> Result<(), ResidentEncodeError> {
        self.sender.take();
        self.wait_closed()?;
        self.diagnostics.consuming_shutdown = true;
        Ok(())
    }
    fn wait_closed(&mut self) -> Result<(), ResidentEncodeError> {
        if self.worker.is_none() {
            return Ok(());
        }
        self.closed
            .recv_timeout(NATIVE_TIMEOUT)
            .map_err(|e| native_error("consuming shutdown", e))?;
        self.worker
            .take()
            .expect("live worker")
            .join()
            .map_err(|_| native_error("worker join", "codec worker panicked"))?;
        Ok(())
    }
    /// Return only facts from actual completed operations.
    pub fn diagnostics(&self) -> VideoToolboxResidentHevcEncoderSessionDiagnostics {
        self.diagnostics
    }
}
impl Drop for VideoToolboxResidentHevcEncoderSession {
    fn drop(&mut self) {
        self.sender.take();
        if let Err(error) = self.wait_closed() {
            // A detached worker continues to own its live codec/input; it is
            // neither an exit receipt nor a resource release proof.
            tracing::error!(%error,"VideoToolbox encoder consuming shutdown unproven");
        }
    }
}

struct NativeSession {
    native: Option<NonNull<c_void>>,
    output: ffmpeg::format::context::Output,
    input_time_base: ffmpeg::Rational,
    output_time_base: ffmpeg::Rational,
    config: Arc<ResidentHevcEncoderConfig>,
    owner: Arc<()>,
    registry_id: u64,
    next_index: u64,
}
impl Drop for NativeSession {
    fn drop(&mut self) {
        if let Some(native) = self.native.take() {
            unsafe { mdn_vt_encoder_close(native.as_ptr()) };
        }
    }
}
impl NativeSession {
    fn open(
        root: MetalResidentEncodeDeviceRoot,
        config: ResidentHevcEncoderConfig,
    ) -> Result<Self, ResidentEncodeError> {
        ffmpeg::init().map_err(|e| native_error("FFmpeg initialize", e))?;
        let num = i32::try_from(config.frame_rate_num).map_err(|e| native_error("cadence", e))?;
        let den = i32::try_from(config.frame_rate_den).map_err(|e| native_error("cadence", e))?;
        let (primaries, transfer, matrix) = match config.colorimetry {
            ResidentEncodeColorimetry::Rec709 => (1, 1, 1),
            ResidentEncodeColorimetry::Rec2100Pq => (9, 16, 9),
            ResidentEncodeColorimetry::Rec2100Hlg => (9, 18, 9),
        };
        let contract = NativeContract {
            width: config.width,
            height: config.height,
            fps_num: config.frame_rate_num,
            fps_den: config.frame_rate_den,
            sar_num: config.sample_aspect_ratio_num,
            sar_den: config.sample_aspect_ratio_den,
            ten_bit: u32::from(config.bit_depth == ResidentEncodeBitDepth::Ten),
            full_range: u32::from(config.full_range),
            primaries,
            transfer,
            matrix,
            centered_chroma: u32::from(
                config.chroma_location == ResidentEncodeChromaLocation::Center,
            ),
            gop: config.keyframe_interval_frames,
            quantizer: u32::from(config.quantizer),
            pool_size: config.surface_pool_size,
            metal_registry_id: root.registry_id,
        };
        let mut error = NativeError::default();
        let native = NonNull::new(unsafe { mdn_vt_encoder_create(&contract, &mut error) })
            .ok_or_else(|| native_failure("create", error))?;
        // Establish RAII immediately, before fallible mux setup.
        struct Guard(Option<NonNull<c_void>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                if let Some(p) = self.0.take() {
                    unsafe { mdn_vt_encoder_close(p.as_ptr()) };
                }
            }
        }
        let mut guard = Guard(Some(native));
        let mut output = ffmpeg::format::output_as(&config.output_path, "mpegts")
            .map_err(|e| native_error("open mux", e))?;
        let mut parameters = ffmpeg::codec::Parameters::new();
        // SAFETY: newly allocated parameters are exclusively owned; CICP values
        // come from the sealed signal enum, not demux defaults or native inference.
        unsafe {
            let p = &mut *parameters.as_mut_ptr();
            p.codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            p.codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_HEVC;
            p.width = i32::try_from(config.width).map_err(|e| native_error("coded width", e))?;
            p.height = i32::try_from(config.height).map_err(|e| native_error("coded height", e))?;
            p.format = if config.bit_depth == ResidentEncodeBitDepth::Ten {
                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32
            } else {
                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32
            };
            p.profile = if config.bit_depth == ResidentEncodeBitDepth::Ten {
                2
            } else {
                1
            };
            p.sample_aspect_ratio = ffmpeg::ffi::AVRational {
                num: i32::try_from(config.sample_aspect_ratio_num)
                    .map_err(|e| native_error("SAR", e))?,
                den: i32::try_from(config.sample_aspect_ratio_den)
                    .map_err(|e| native_error("SAR", e))?,
            };
            p.color_range = if config.full_range {
                ffmpeg::ffi::AVColorRange::AVCOL_RANGE_JPEG
            } else {
                ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG
            };
        }
        let input_time_base = ffmpeg::Rational(den, num);
        {
            let mut stream = output
                .add_stream(ffmpeg::codec::Id::HEVC)
                .map_err(|e| native_error("add stream", e))?;
            stream.set_time_base(input_time_base);
            stream.set_parameters(parameters);
        }
        output.write_header().map_err(|e| native_error("mux header", e))?;
        let output_time_base = output
            .stream(0)
            .ok_or_else(|| native_error("mux stream", "missing stream"))?
            .time_base();
        guard.0.take();
        Ok(Self {
            native: Some(native),
            output,
            input_time_base,
            output_time_base,
            config: Arc::new(config),
            owner: Arc::new(()),
            registry_id: root.registry_id,
            next_index: 0,
        })
    }
    fn acquire(&mut self) -> Result<MetalResidentEncodeInputFrame, ResidentEncodeError> {
        let mut error = NativeError::default();
        let buffer = NonNull::new(unsafe {
            mdn_vt_encoder_acquire(self.native.expect("live codec").as_ptr(), &mut error)
        })
        .ok_or_else(|| native_failure("acquire", error))?;
        Ok(MetalResidentEncodeInputFrame {
            buffer,
            owner: self.owner.clone(),
            config: self.config.clone(),
            registry_id: self.registry_id,
        })
    }
    fn submit(
        &mut self,
        frame: MetalResidentEncodeReadyFrame,
        index: u64,
    ) -> Result<(), ResidentEncodeError> {
        if !Arc::ptr_eq(&frame.input.owner, &self.owner) || index != self.next_index {
            return Err(native_error(
                "input identity",
                "wrong codec owner or frame sequence",
            ));
        }
        let mut packet = NativePacket {
            bytes: std::ptr::null(),
            size: 0,
            pts: 0,
            key: 0,
            hardware_observed: 0,
        };
        let mut error = NativeError::default();
        let ok = unsafe {
            mdn_vt_encoder_submit(
                self.native.expect("live codec").as_ptr(),
                frame.input.buffer.as_ptr(),
                index,
                &mut packet,
                &mut error,
            )
        };
        if ok == 0 {
            return Err(native_failure("submit", error));
        }
        if packet.hardware_observed != 1 || packet.bytes.is_null() || packet.size == 0 {
            return Err(native_error(
                "native output",
                "missing compressed/hardware proof",
            ));
        }
        // SAFETY: the worker exclusively owns the live native session; compressed
        // bytes remain valid through this copy, before any subsequent submission.
        let mut output =
            ffmpeg::Packet::copy(unsafe { std::slice::from_raw_parts(packet.bytes, packet.size) });
        output.set_pts(Some(packet.pts));
        output.set_dts(Some(packet.pts));
        output.set_duration(1);
        output.set_stream(0);
        if packet.key != 0 {
            output.set_flags(ffmpeg::codec::packet::Flags::KEY);
        }
        output.rescale_ts(self.input_time_base, self.output_time_base);
        output
            .write_interleaved(&mut self.output)
            .map_err(|e| native_error("mux packet", e))?;
        self.next_index = self
            .next_index
            .checked_add(1)
            .ok_or(ResidentEncodeError::FrameIndexOverflow { frame_index: index })?;
        Ok(())
    }
    fn finish(mut self) -> Result<(), ResidentEncodeError> {
        if let Some(native) = self.native.take() {
            unsafe { mdn_vt_encoder_close(native.as_ptr()) };
        }
        self.output.write_trailer().map_err(|e| native_error("mux trailer", e))
    }
}
fn native_error(stage: &'static str, reason: impl ToString) -> ResidentEncodeError {
    ResidentEncodeError::Native { stage, reason: reason.to_string() }
}
fn native_failure(stage: &'static str, error: NativeError) -> ResidentEncodeError {
    native_error(
        stage,
        format!("VideoToolbox stage {} status {}", error.stage, error.status),
    )
}
