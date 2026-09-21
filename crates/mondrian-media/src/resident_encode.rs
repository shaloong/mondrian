//! In-process hardware encoder surfaces that remain resident on the renderer device.
//!
//! This module owns FFmpeg codec/frame/mux lifetimes. Renderer platform
//! Adapters may borrow the typed native surface and fence, but cannot reinterpret
//! the codec Session or replace its device root.

use std::path::PathBuf;

use crate::{HwAccelBackend, RendererHwAccelDeviceContext};

/// Native surface precision admitted by the resident HEVC encoder path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentEncodeBitDepth {
    /// NV12 4:2:0 surface and HEVC Main profile.
    Eight,
    /// P010 4:2:0 surface and HEVC Main 10 profile.
    Ten,
}

/// Encoded signal identity used by the hardware video processor and bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentEncodeColorimetry {
    /// BT.709 primaries, transfer, and non-constant-luminance matrix.
    Rec709,
    /// BT.2020 primaries, PQ transfer, and BT.2020 non-constant-luminance matrix.
    Rec2100Pq,
    /// BT.2020 primaries, HLG transfer, and BT.2020 non-constant-luminance matrix.
    Rec2100Hlg,
}

/// Spatial location authored for each 4:2:0 chroma sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentEncodeChromaLocation {
    /// Chroma is horizontally co-sited with the left luma sample and vertically centered.
    Left,
    /// Chroma is centered across each 2x2 luma block.
    Center,
}

/// Immutable contract for one video-only resident HEVC elementary encode/mux Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentHevcEncoderConfig {
    /// Video-only temporary artifact. Export may later stream-copy it while muxing audio.
    pub output_path: PathBuf,
    /// Coded width; must be non-zero and even for 4:2:0.
    pub width: u32,
    /// Coded height; must be non-zero and even for 4:2:0.
    pub height: u32,
    /// Frame-rate numerator.
    pub frame_rate_num: u32,
    /// Frame-rate denominator.
    pub frame_rate_den: u32,
    /// Sample-aspect-ratio numerator written into the coded stream.
    pub sample_aspect_ratio_num: u32,
    /// Sample-aspect-ratio denominator written into the coded stream.
    pub sample_aspect_ratio_den: u32,
    /// Hardware surface and profile precision.
    pub bit_depth: ResidentEncodeBitDepth,
    /// Encoded signal identity.
    pub colorimetry: ResidentEncodeColorimetry,
    /// Whether YCbCr code values use the full range.
    pub full_range: bool,
    /// Exact 4:2:0 chroma location produced by the RGB conversion kernel.
    pub chroma_location: ResidentEncodeChromaLocation,
    /// Closed-GOP keyframe interval in frames.
    pub keyframe_interval_frames: u32,
    /// Maximum B-frame count.
    pub max_b_frames: u32,
    /// Constant-quantizer quality value accepted by the D3D12VA encoder.
    pub quantizer: u8,
    /// Maximum codec-owned input surfaces.
    pub surface_pool_size: u32,
}

impl ResidentHevcEncoderConfig {
    fn validate(&self) -> Result<(), ResidentEncodeError> {
        if self.width == 0
            || self.height == 0
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err(ResidentEncodeError::InvalidDimensions {
                width: self.width,
                height: self.height,
            });
        }
        if self.frame_rate_num == 0 || self.frame_rate_den == 0 {
            return Err(ResidentEncodeError::InvalidFrameRate {
                numerator: self.frame_rate_num,
                denominator: self.frame_rate_den,
            });
        }
        if self.sample_aspect_ratio_num == 0 || self.sample_aspect_ratio_den == 0 {
            return Err(ResidentEncodeError::InvalidSampleAspectRatio {
                numerator: self.sample_aspect_ratio_num,
                denominator: self.sample_aspect_ratio_den,
            });
        }
        if self.keyframe_interval_frames == 0 {
            return Err(ResidentEncodeError::InvalidKeyframeInterval);
        }
        if self.surface_pool_size < 2 {
            return Err(ResidentEncodeError::InvalidSurfacePoolSize {
                value: self.surface_pool_size,
            });
        }
        if !(1..=51).contains(&self.quantizer) {
            return Err(ResidentEncodeError::InvalidQuantizer { value: self.quantizer });
        }
        Ok(())
    }
}

/// Stable failure from resident hardware-frame creation, encoding, or muxing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResidentEncodeError {
    /// The Session received a renderer root for a different hardware backend.
    #[error("resident HEVC encode requires a renderer-qualified device root for its backend")]
    WrongDeviceBackend,
    /// This build lacks FFmpeg 7.1's D3D12 HEVC encoder surface.
    #[error("linked FFmpeg does not expose the 7.1 D3D12 HEVC encoder surface")]
    FfmpegD3D12EncodeUnavailable,
    /// 4:2:0 hardware surfaces require non-zero even dimensions.
    #[error("resident HEVC encode requires non-zero even dimensions, got {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    /// Frame cadence must be a positive rational.
    #[error("resident HEVC encode has invalid frame rate {numerator}/{denominator}")]
    InvalidFrameRate { numerator: u32, denominator: u32 },
    /// Sample aspect ratio must be a positive rational.
    #[error("resident HEVC encode has invalid sample aspect ratio {numerator}/{denominator}")]
    InvalidSampleAspectRatio { numerator: u32, denominator: u32 },
    /// GOP construction requires a positive interval.
    #[error("resident HEVC encode requires a positive keyframe interval")]
    InvalidKeyframeInterval,
    /// At least two surfaces are required to separate producer and encoder ownership.
    #[error("resident HEVC encode surface pool size {value} is below two")]
    InvalidSurfacePoolSize { value: u32 },
    /// HEVC CQP accepts the normative 1..=51 interval.
    #[error("resident HEVC quantizer {value} is outside 1..=51")]
    InvalidQuantizer { value: u8 },
    /// A frame index could not be represented as an FFmpeg timestamp.
    #[error("resident HEVC frame index {frame_index} exceeds FFmpeg timestamp range")]
    FrameIndexOverflow { frame_index: u64 },
    /// Surface readiness fence values must remain strictly monotonic.
    #[error("resident HEVC input-surface fence value is exhausted")]
    FenceValueExhausted,
    /// FFmpeg returned an incomplete object or rejected the exact contract.
    #[error("resident HEVC FFmpeg stage {stage} failed: {reason}")]
    Ffmpeg { stage: &'static str, reason: String },
    /// The D3D12 frame ABI did not carry a valid resource/fence pair.
    #[error("resident HEVC FFmpeg surface is missing {object}")]
    MissingNativeSurface { object: &'static str },
    /// No further frames may be submitted after finalization.
    #[error("resident HEVC encoder Session is already finalized")]
    AlreadyFinished,
    /// Error cleanup could not prove the producer finished with the surface.
    #[error("resident HEVC producer fence did not complete during bounded error cleanup")]
    ProducerFenceTimeout,
}

#[cfg(target_os = "linux")]
mod linux_cuda_impl {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use ffmpeg::{codec, encoder, format, Dictionary, Packet, Rational};
    use ffmpeg_next as ffmpeg;

    use super::*;

    #[repr(C)]
    struct CudaContextPrefix {
        context: *mut c_void,
        stream: *mut c_void,
    }

    pub(super) struct SessionInner {
        encoder: encoder::Video,
        output: format::context::Output,
        frames_context: FramesContextRef,
        input_time_base: Rational,
        output_time_base: Rational,
        stream_index: usize,
        width: u32,
        height: u32,
        bit_depth: ResidentEncodeBitDepth,
        chroma_location: ResidentEncodeChromaLocation,
    }

    impl SessionInner {
        pub(super) fn open(
            device_root: &RendererHwAccelDeviceContext,
            config: ResidentHevcEncoderConfig,
        ) -> Result<Self, ResidentEncodeError> {
            ffmpeg::init().map_err(|error| ffmpeg_error("initialize", error))?;
            let codec =
                encoder::find_by_name("hevc_nvenc").ok_or_else(|| ResidentEncodeError::Ffmpeg {
                    stage: "find_encoder",
                    reason: "hevc_nvenc is not registered".to_owned(),
                })?;
            let device_ref = device_root.retain_ffmpeg_device_ref().map_err(|error| {
                ResidentEncodeError::Ffmpeg {
                    stage: "retain_device_root",
                    reason: error.to_string(),
                }
            })?;
            // Keep the frame-pool root under RAII before any later fallible
            // output/codec setup. A partial open must release the CUDA pool and
            // its retained device reference immediately.
            let frames_context = create_frames_context(device_ref, &config)?;
            let mut output = format::output(&config.output_path)
                .map_err(|error| ffmpeg_error("open_output", error))?;
            let global_header = output.format().flags().contains(format::Flags::GLOBAL_HEADER);
            let mut video = codec::context::Context::new_with_codec(codec)
                .encoder()
                .video()
                .map_err(|error| ffmpeg_error("create_encoder", error))?;
            let input_time_base = Rational(
                i32::try_from(config.frame_rate_den).map_err(|_| {
                    ResidentEncodeError::InvalidFrameRate {
                        numerator: config.frame_rate_num,
                        denominator: config.frame_rate_den,
                    }
                })?,
                i32::try_from(config.frame_rate_num).map_err(|_| {
                    ResidentEncodeError::InvalidFrameRate {
                        numerator: config.frame_rate_num,
                        denominator: config.frame_rate_den,
                    }
                })?,
            );
            video.set_width(config.width);
            video.set_height(config.height);
            video.set_time_base(input_time_base);
            video.set_frame_rate(Some(Rational(
                input_time_base.denominator(),
                input_time_base.numerator(),
            )));
            video.set_aspect_ratio(Rational(
                i32::try_from(config.sample_aspect_ratio_num).map_err(|_| {
                    ResidentEncodeError::InvalidSampleAspectRatio {
                        numerator: config.sample_aspect_ratio_num,
                        denominator: config.sample_aspect_ratio_den,
                    }
                })?,
                i32::try_from(config.sample_aspect_ratio_den).map_err(|_| {
                    ResidentEncodeError::InvalidSampleAspectRatio {
                        numerator: config.sample_aspect_ratio_num,
                        denominator: config.sample_aspect_ratio_den,
                    }
                })?,
            ));
            video.set_format(ffmpeg::format::Pixel::CUDA);
            video.set_gop(config.keyframe_interval_frames);
            video.set_max_b_frames(config.max_b_frames as usize);
            apply_signal(&mut video, config.colorimetry, config.full_range);
            apply_chroma_location(&mut video, config.chroma_location);
            let mut codec_flags = codec::Flags::CLOSED_GOP;
            if global_header {
                codec_flags |= codec::Flags::GLOBAL_HEADER;
            }
            video.set_flags(codec_flags);
            unsafe {
                (*video.as_mut_ptr()).hw_frames_ctx =
                    ffmpeg::ffi::av_buffer_ref(frames_context.as_ptr());
                if (*video.as_ptr()).hw_frames_ctx.is_null() {
                    return Err(ResidentEncodeError::Ffmpeg {
                        stage: "attach_frames_context",
                        reason: "av_buffer_ref returned null".to_owned(),
                    });
                }
            }
            let mut options = Dictionary::new();
            options.set("rc", "constqp");
            options.set("qp", &config.quantizer.to_string());
            options.set(
                "profile",
                match config.bit_depth {
                    ResidentEncodeBitDepth::Eight => "main",
                    ResidentEncodeBitDepth::Ten => "main10",
                },
            );
            options.set("forced-idr", "1");
            options.set("no-scenecut", "1");
            let opened = {
                let mut stream =
                    output.add_stream(codec).map_err(|error| ffmpeg_error("add_stream", error))?;
                stream.set_time_base(input_time_base);
                stream.set_parameters(&video);
                let opened = video
                    .open_as_with(codec, options)
                    .map_err(|error| ffmpeg_error("open_encoder", error))?;
                stream.set_parameters(&opened);
                opened
            };
            output.write_header().map_err(|error| ffmpeg_error("write_header", error))?;
            let output_time_base = output
                .stream(0)
                .ok_or_else(|| ResidentEncodeError::Ffmpeg {
                    stage: "resolve_output_stream",
                    reason: "output stream 0 is missing".to_owned(),
                })?
                .time_base();
            Ok(Self {
                encoder: opened,
                output,
                frames_context,
                input_time_base,
                output_time_base,
                stream_index: 0,
                width: config.width,
                height: config.height,
                bit_depth: config.bit_depth,
                chroma_location: config.chroma_location,
            })
        }

        pub(super) fn acquire_input_frame(
            &mut self,
        ) -> Result<CudaResidentEncodeInputFrame, ResidentEncodeError> {
            let mut frame = ffmpeg::frame::Video::empty();
            let result = unsafe {
                ffmpeg::ffi::av_hwframe_get_buffer(
                    self.frames_context.as_ptr(),
                    frame.as_mut_ptr(),
                    0,
                )
            };
            if result < 0 {
                return Err(ffmpeg_code_error("acquire_input_surface", result));
            }
            let (context, stream) = cuda_context(self.frames_context.as_ptr())?;
            let component_bytes = match self.bit_depth {
                ResidentEncodeBitDepth::Eight => 1usize,
                ResidentEncodeBitDepth::Ten => 2usize,
            };
            let raw = unsafe { &*frame.as_ptr() };
            unsafe {
                (*frame.as_mut_ptr()).chroma_location =
                    ffmpeg_chroma_location(self.chroma_location);
            }
            let mut planes = [(0usize, 0usize); 2];
            for (index, plane) in planes.iter_mut().enumerate() {
                let rows = if index == 0 {
                    self.height
                } else {
                    self.height / 2
                };
                let row_bytes = usize::try_from(self.width)
                    .ok()
                    .and_then(|width| width.checked_mul(component_bytes))
                    .ok_or(ResidentEncodeError::InvalidDimensions {
                        width: self.width,
                        height: self.height,
                    })?;
                let pitch = usize::try_from(raw.linesize[index]).map_err(|_| {
                    ResidentEncodeError::MissingNativeSurface {
                        object: "positive CUDA plane pitch",
                    }
                })?;
                let address = raw.data[index] as usize;
                if address == 0 || pitch < row_bytes || rows == 0 {
                    return Err(ResidentEncodeError::MissingNativeSurface {
                        object: "CUDA plane address/pitch",
                    });
                }
                address
                    .checked_add(
                        pitch
                            .checked_mul(rows as usize - 1)
                            .and_then(|value| value.checked_add(row_bytes))
                            .ok_or(ResidentEncodeError::MissingNativeSurface {
                                object: "bounded CUDA plane extent",
                            })?,
                    )
                    .ok_or(ResidentEncodeError::MissingNativeSurface {
                        object: "bounded CUDA plane address",
                    })?;
                *plane = (address, pitch);
            }
            Ok(CudaResidentEncodeInputFrame {
                frame,
                context,
                stream,
                planes,
                width: self.width,
                height: self.height,
                bit_depth: self.bit_depth,
            })
        }

        pub(super) fn submit_input_frame(
            &mut self,
            mut input: CudaResidentEncodeInputFrame,
            frame_index: u64,
        ) -> Result<u64, ResidentEncodeError> {
            let pts = i64::try_from(frame_index)
                .map_err(|_| ResidentEncodeError::FrameIndexOverflow { frame_index })?;
            input.frame.set_pts(Some(pts));
            self.encoder
                .send_frame(&input.frame)
                .map_err(|error| ffmpeg_error("send_frame", error))?;
            self.drain_packets(false)
        }

        pub(super) fn finish(&mut self) -> Result<u64, ResidentEncodeError> {
            self.encoder.send_eof().map_err(|error| ffmpeg_error("send_eof", error))?;
            let written = self.drain_packets(true)?;
            self.output
                .write_trailer()
                .map_err(|error| ffmpeg_error("write_trailer", error))?;
            Ok(written)
        }

        fn drain_packets(&mut self, flushing: bool) -> Result<u64, ResidentEncodeError> {
            let mut written = 0_u64;
            let mut packet = Packet::empty();
            loop {
                match self.encoder.receive_packet(&mut packet) {
                    Ok(()) => {
                        packet.set_stream(self.stream_index);
                        packet.rescale_ts(self.input_time_base, self.output_time_base);
                        packet
                            .write_interleaved(&mut self.output)
                            .map_err(|error| ffmpeg_error("write_packet", error))?;
                        written = written.saturating_add(1);
                    }
                    Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                        if flushing {
                            return Err(ResidentEncodeError::Ffmpeg {
                                stage: "receive_packet_after_eof",
                                reason: "encoder returned EAGAIN while flushing".to_owned(),
                            });
                        }
                        break;
                    }
                    Err(ffmpeg::Error::Eof) => break,
                    Err(error) => return Err(ffmpeg_error("receive_packet", error)),
                }
            }
            Ok(written)
        }
    }

    struct FramesContextRef(NonNull<ffmpeg::ffi::AVBufferRef>);

    impl FramesContextRef {
        fn as_ptr(&self) -> *mut ffmpeg::ffi::AVBufferRef {
            self.0.as_ptr()
        }
    }

    impl Drop for FramesContextRef {
        fn drop(&mut self) {
            let mut frames = self.0.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut frames) };
        }
    }

    fn create_frames_context(
        device_ref: NonNull<ffmpeg::ffi::AVBufferRef>,
        config: &ResidentHevcEncoderConfig,
    ) -> Result<FramesContextRef, ResidentEncodeError> {
        let raw = unsafe { ffmpeg::ffi::av_hwframe_ctx_alloc(device_ref.as_ptr()) };
        let Some(frames_ref) = NonNull::new(raw) else {
            let mut device = device_ref.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut device) };
            return Err(ResidentEncodeError::Ffmpeg {
                stage: "allocate_frames_context",
                reason: "av_hwframe_ctx_alloc returned null".to_owned(),
            });
        };
        let configured = (|| {
            let generic = unsafe {
                NonNull::new((*frames_ref.as_ptr()).data.cast::<ffmpeg::ffi::AVHWFramesContext>())
            }
            .ok_or(ResidentEncodeError::Ffmpeg {
                stage: "configure_frames_context",
                reason: "AVHWFramesContext payload is null".to_owned(),
            })?;
            let generic =
                unsafe { generic.as_ptr().as_mut() }.ok_or(ResidentEncodeError::Ffmpeg {
                    stage: "configure_frames_context",
                    reason: "AVHWFramesContext payload is not mutable".to_owned(),
                })?;
            generic.format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA;
            generic.sw_format = match config.bit_depth {
                ResidentEncodeBitDepth::Eight => ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12,
                ResidentEncodeBitDepth::Ten => ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE,
            };
            generic.width = i32::try_from(config.width).map_err(|_| {
                ResidentEncodeError::InvalidDimensions {
                    width: config.width,
                    height: config.height,
                }
            })?;
            generic.height = i32::try_from(config.height).map_err(|_| {
                ResidentEncodeError::InvalidDimensions {
                    width: config.width,
                    height: config.height,
                }
            })?;
            generic.initial_pool_size = i32::try_from(config.surface_pool_size).map_err(|_| {
                ResidentEncodeError::InvalidSurfacePoolSize { value: config.surface_pool_size }
            })?;
            let result = unsafe { ffmpeg::ffi::av_hwframe_ctx_init(frames_ref.as_ptr()) };
            if result < 0 {
                return Err(ffmpeg_code_error("initialize_frames_context", result));
            }
            Ok(())
        })();
        let mut device = device_ref.as_ptr();
        unsafe { ffmpeg::ffi::av_buffer_unref(&mut device) };
        if let Err(error) = configured {
            let mut frames = frames_ref.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut frames) };
            return Err(error);
        }
        Ok(FramesContextRef(frames_ref))
    }

    fn cuda_context(
        frames_ref: *mut ffmpeg::ffi::AVBufferRef,
    ) -> Result<(*mut c_void, *mut c_void), ResidentEncodeError> {
        let frames =
            unsafe { NonNull::new((*frames_ref).data.cast::<ffmpeg::ffi::AVHWFramesContext>()) }
                .ok_or(ResidentEncodeError::MissingNativeSurface {
                    object: "CUDA frames context",
                })?;
        let device_ref = unsafe { NonNull::new((*frames.as_ptr()).device_ref) }
            .ok_or(ResidentEncodeError::MissingNativeSurface { object: "CUDA device reference" })?;
        let device = unsafe {
            NonNull::new((*device_ref.as_ptr()).data.cast::<ffmpeg::ffi::AVHWDeviceContext>())
        }
        .ok_or(ResidentEncodeError::MissingNativeSurface { object: "CUDA device context" })?;
        if unsafe { (*device.as_ptr()).type_ } != ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA
        {
            return Err(ResidentEncodeError::WrongDeviceBackend);
        }
        let cuda = unsafe { NonNull::new((*device.as_ptr()).hwctx.cast::<CudaContextPrefix>()) }
            .ok_or(ResidentEncodeError::MissingNativeSurface { object: "CUDA native context" })?;
        let context = unsafe { (*cuda.as_ptr()).context };
        let stream = unsafe { (*cuda.as_ptr()).stream };
        if context.is_null() {
            return Err(ResidentEncodeError::MissingNativeSurface {
                object: "CUDA context handle",
            });
        }
        Ok((context, stream))
    }

    fn apply_signal(
        video: &mut ffmpeg::codec::encoder::video::Video,
        colorimetry: ResidentEncodeColorimetry,
        full_range: bool,
    ) {
        let raw = unsafe { &mut *video.as_mut_ptr() };
        raw.color_range = if full_range {
            ffmpeg::ffi::AVColorRange::AVCOL_RANGE_JPEG
        } else {
            ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG
        };
        let (primaries, transfer, matrix) = match colorimetry {
            ResidentEncodeColorimetry::Rec709 => (
                ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT709,
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709,
                ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT709,
            ),
            ResidentEncodeColorimetry::Rec2100Pq => (
                ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084,
                ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL,
            ),
            ResidentEncodeColorimetry::Rec2100Hlg => (
                ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67,
                ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL,
            ),
        };
        raw.color_primaries = primaries;
        raw.color_trc = transfer;
        raw.colorspace = matrix;
    }

    fn apply_chroma_location(
        video: &mut ffmpeg::codec::encoder::video::Video,
        location: ResidentEncodeChromaLocation,
    ) {
        unsafe {
            (*video.as_mut_ptr()).chroma_sample_location = ffmpeg_chroma_location(location);
        }
    }

    const fn ffmpeg_chroma_location(
        location: ResidentEncodeChromaLocation,
    ) -> ffmpeg::ffi::AVChromaLocation {
        match location {
            ResidentEncodeChromaLocation::Left => ffmpeg::ffi::AVChromaLocation::AVCHROMA_LOC_LEFT,
            ResidentEncodeChromaLocation::Center => {
                ffmpeg::ffi::AVChromaLocation::AVCHROMA_LOC_CENTER
            }
        }
    }

    fn ffmpeg_error(stage: &'static str, error: ffmpeg::Error) -> ResidentEncodeError {
        ResidentEncodeError::Ffmpeg { stage, reason: error.to_string() }
    }

    fn ffmpeg_code_error(stage: &'static str, code: i32) -> ResidentEncodeError {
        ffmpeg_error(stage, ffmpeg::Error::from(code))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn partial_open_owner_releases_frames_context_reference() {
            let raw = unsafe { ffmpeg::ffi::av_buffer_alloc(1) };
            let raw = NonNull::new(raw).expect("allocate FFmpeg regression buffer");
            let mut witness = unsafe { ffmpeg::ffi::av_buffer_ref(raw.as_ptr()) };
            assert!(!witness.is_null());
            let owner = FramesContextRef(raw);
            assert_eq!(unsafe { ffmpeg::ffi::av_buffer_get_ref_count(witness) }, 2);

            drop(owner);

            assert_eq!(unsafe { ffmpeg::ffi::av_buffer_get_ref_count(witness) }, 1);
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut witness) };
        }
    }
}

/// Independently inspectable no-readback/no-upload evidence for one Session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct D3D12ResidentHevcEncoderSessionDiagnostics {
    /// FFmpeg hardware input surfaces acquired.
    pub surfaces_acquired: u64,
    /// Frames accepted by the in-process hardware encoder.
    pub frames_submitted: u64,
    /// Encoded packets written to the video-only artifact.
    pub packets_written: u64,
    /// CPU pixel readbacks. A qualified resident Session keeps this at zero.
    pub cpu_pixel_readbacks: u64,
    /// Rawvideo bytes written. A qualified resident Session keeps this at zero.
    pub rawvideo_pipe_bytes: u64,
    /// CPU-to-encoder pixel uploads. A qualified resident Session keeps this at zero.
    pub cpu_pixel_uploads: u64,
}

/// Independently inspectable no-readback/no-host-upload evidence for one CUDA Session.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CudaResidentHevcEncoderSessionDiagnostics {
    /// FFmpeg CUDA input surfaces acquired.
    pub surfaces_acquired: u64,
    /// Frames accepted by the in-process NVENC encoder.
    pub frames_submitted: u64,
    /// Encoded packets written to the video-only artifact.
    pub packets_written: u64,
    /// CPU pixel readbacks. A qualified resident Session keeps this at zero.
    pub cpu_pixel_readbacks: u64,
    /// Rawvideo bytes written. A qualified resident Session keeps this at zero.
    pub rawvideo_pipe_bytes: u64,
    /// Host-to-device pixel uploads. A qualified resident Session keeps this at zero.
    pub cpu_pixel_uploads: u64,
}

/// FFmpeg-owned D3D12 input surface awaiting one renderer video-process write.
#[cfg(target_os = "windows")]
pub struct D3D12ResidentEncodeInputFrame {
    #[cfg(mondrian_ffmpeg_7_1)]
    frame: ffmpeg_next::frame::Video,
    resource: windows::Win32::Graphics::Direct3D12::ID3D12Resource,
    fence: windows::Win32::Graphics::Direct3D12::ID3D12Fence,
    fence_value: u64,
    bit_depth: ResidentEncodeBitDepth,
}

/// FFmpeg-owned input whose producer signal has been enqueued on the native queue.
///
/// Construction is restricted to a renderer platform Adapter that owns the
/// resource-state transitions and exact fence signal. Media accepts only this
/// state, so an acquired but unwritten surface cannot enter the encoder.
#[cfg(target_os = "windows")]
pub struct D3D12ResidentEncodeReadyFrame {
    input: D3D12ResidentEncodeInputFrame,
}

#[cfg(not(target_os = "windows"))]
pub struct D3D12ResidentEncodeReadyFrame {
    _private: (),
}

#[cfg(not(target_os = "windows"))]
pub struct D3D12ResidentEncodeInputFrame {
    _private: (),
}

/// Borrowed CUDA destination surface owned by an FFmpeg NVENC frame.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
pub struct CudaResidentEncodeSurfaceView<'a> {
    /// CUDA context owning the destination addresses.
    pub context: *mut std::ffi::c_void,
    /// FFmpeg CUDA stream associated with the hardware frame pool.
    pub stream: *mut std::ffi::c_void,
    /// Luma and interleaved chroma `(device_address, pitch_bytes)` pairs.
    pub planes: [(usize, usize); 2],
    /// Visible coded width.
    pub width: u32,
    /// Visible coded height.
    pub height: u32,
    /// Exact NV12 or P010 precision.
    pub bit_depth: ResidentEncodeBitDepth,
    _owner: std::marker::PhantomData<&'a CudaResidentEncodeInputFrame>,
}

/// FFmpeg-owned CUDA input surface awaiting the renderer's device-to-device write.
#[cfg(target_os = "linux")]
pub struct CudaResidentEncodeInputFrame {
    frame: ffmpeg_next::frame::Video,
    context: *mut std::ffi::c_void,
    stream: *mut std::ffi::c_void,
    planes: [(usize, usize); 2],
    width: u32,
    height: u32,
    bit_depth: ResidentEncodeBitDepth,
}

// SAFETY: CUDA device addresses are never CPU-dereferenced. The move-only
// frame remains owned by one Export worker and native use is ordered by the
// renderer Adapter before the frame is promoted to Ready.
#[cfg(target_os = "linux")]
unsafe impl Send for CudaResidentEncodeInputFrame {}

/// FFmpeg-owned CUDA frame after the renderer completed its native producer write.
#[cfg(target_os = "linux")]
pub struct CudaResidentEncodeReadyFrame {
    input: CudaResidentEncodeInputFrame,
}

#[cfg(target_os = "linux")]
impl CudaResidentEncodeInputFrame {
    /// Borrow the exact CUDA destination surface while retaining its FFmpeg owner.
    pub fn surface(&self) -> CudaResidentEncodeSurfaceView<'_> {
        CudaResidentEncodeSurfaceView {
            context: self.context,
            stream: self.stream,
            planes: self.planes,
            width: self.width,
            height: self.height,
            bit_depth: self.bit_depth,
            _owner: std::marker::PhantomData,
        }
    }

    /// Promote a surface after its producer copy has completed successfully.
    ///
    /// # Safety
    ///
    /// The caller must prove all CUDA writes to both planes completed before
    /// calling this function. The Vulkan/CUDA renderer Adapter is the production caller.
    pub unsafe fn assume_producer_copy_complete(self) -> CudaResidentEncodeReadyFrame {
        CudaResidentEncodeReadyFrame { input: self }
    }
}

#[cfg(target_os = "windows")]
impl D3D12ResidentEncodeInputFrame {
    /// Borrow the exact FFmpeg-owned destination resource.
    pub fn resource(&self) -> &windows::Win32::Graphics::Direct3D12::ID3D12Resource {
        &self.resource
    }

    /// Borrow the fence that the renderer producer must signal.
    pub fn fence(&self) -> &windows::Win32::Graphics::Direct3D12::ID3D12Fence {
        &self.fence
    }

    /// Fence value reserved for this producer write.
    pub fn fence_value(&self) -> u64 {
        self.fence_value
    }

    /// Exact native surface precision.
    pub fn bit_depth(&self) -> ResidentEncodeBitDepth {
        self.bit_depth
    }

    /// Promote the surface after the exact resource transitions, producer
    /// write, and reserved fence signal have all been enqueued.
    ///
    /// # Safety
    ///
    /// The caller must have restored the destination to `COMMON` and enqueued
    /// `Signal(self.fence(), self.fence_value())` after the final write on the
    /// same queue. The renderer's native Adapter is the production caller.
    pub unsafe fn assume_producer_signal_enqueued(self) -> D3D12ResidentEncodeReadyFrame {
        D3D12ResidentEncodeReadyFrame { input: self }
    }
}

/// Media-owned in-process D3D12VA HEVC encoder and video-only mux Session.
pub struct D3D12ResidentHevcEncoderSession {
    #[cfg(all(target_os = "windows", mondrian_ffmpeg_7_1))]
    inner: Option<windows_impl::SessionInner>,
    diagnostics: D3D12ResidentHevcEncoderSessionDiagnostics,
}

impl D3D12ResidentHevcEncoderSession {
    /// Open an exact-device D3D12VA HEVC Session.
    pub fn open(
        device_root: &RendererHwAccelDeviceContext,
        config: ResidentHevcEncoderConfig,
    ) -> Result<Self, ResidentEncodeError> {
        config.validate()?;
        if device_root.backend() != HwAccelBackend::D3D12VA {
            return Err(ResidentEncodeError::WrongDeviceBackend);
        }
        #[cfg(all(target_os = "windows", mondrian_ffmpeg_7_1))]
        {
            Ok(Self {
                inner: Some(windows_impl::SessionInner::open(device_root, config)?),
                diagnostics: D3D12ResidentHevcEncoderSessionDiagnostics::default(),
            })
        }
        #[cfg(not(all(target_os = "windows", mondrian_ffmpeg_7_1)))]
        {
            let _ = config;
            Err(ResidentEncodeError::FfmpegD3D12EncodeUnavailable)
        }
    }

    /// Acquire one FFmpeg-owned native input surface and reserve its producer fence value.
    #[cfg(target_os = "windows")]
    pub fn acquire_input_frame(
        &mut self,
    ) -> Result<D3D12ResidentEncodeInputFrame, ResidentEncodeError> {
        #[cfg(mondrian_ffmpeg_7_1)]
        {
            let inner = self.inner.as_mut().ok_or(ResidentEncodeError::AlreadyFinished)?;
            let frame = inner.acquire_input_frame()?;
            self.diagnostics.surfaces_acquired =
                self.diagnostics.surfaces_acquired.saturating_add(1);
            Ok(frame)
        }
        #[cfg(not(mondrian_ffmpeg_7_1))]
        Err(ResidentEncodeError::FfmpegD3D12EncodeUnavailable)
    }

    /// Transfer one producer-signaled surface to FFmpeg and drain available packets.
    #[cfg(target_os = "windows")]
    pub fn submit_input_frame(
        &mut self,
        frame: D3D12ResidentEncodeReadyFrame,
        frame_index: u64,
    ) -> Result<(), ResidentEncodeError> {
        #[cfg(mondrian_ffmpeg_7_1)]
        {
            let inner = self.inner.as_mut().ok_or(ResidentEncodeError::AlreadyFinished)?;
            let written = inner.submit_input_frame(frame.input, frame_index)?;
            self.diagnostics.frames_submitted = self.diagnostics.frames_submitted.saturating_add(1);
            self.diagnostics.packets_written =
                self.diagnostics.packets_written.saturating_add(written);
            Ok(())
        }
        #[cfg(not(mondrian_ffmpeg_7_1))]
        {
            let _ = (frame, frame_index);
            Err(ResidentEncodeError::FfmpegD3D12EncodeUnavailable)
        }
    }

    /// Flush delayed frames, publish the video-only trailer, and release codec surfaces.
    pub fn finish(&mut self) -> Result<(), ResidentEncodeError> {
        #[cfg(all(target_os = "windows", mondrian_ffmpeg_7_1))]
        {
            let mut inner = self.inner.take().ok_or(ResidentEncodeError::AlreadyFinished)?;
            let written = inner.finish()?;
            self.diagnostics.packets_written =
                self.diagnostics.packets_written.saturating_add(written);
            Ok(())
        }
        #[cfg(not(all(target_os = "windows", mondrian_ffmpeg_7_1)))]
        Err(ResidentEncodeError::FfmpegD3D12EncodeUnavailable)
    }

    /// Return cumulative Session evidence.
    pub fn diagnostics(&self) -> D3D12ResidentHevcEncoderSessionDiagnostics {
        self.diagnostics
    }
}

/// FFmpeg-owned CUDA HEVC encoder Session using same-device NVENC surfaces.
#[cfg(target_os = "linux")]
pub struct CudaResidentHevcEncoderSession {
    inner: Option<linux_cuda_impl::SessionInner>,
    diagnostics: CudaResidentHevcEncoderSessionDiagnostics,
}

#[cfg(target_os = "linux")]
impl CudaResidentHevcEncoderSession {
    /// Open an exact-device CUDA/NVENC HEVC Session.
    pub fn open(
        device_root: &RendererHwAccelDeviceContext,
        config: ResidentHevcEncoderConfig,
    ) -> Result<Self, ResidentEncodeError> {
        config.validate()?;
        if device_root.backend() != HwAccelBackend::Cuda {
            return Err(ResidentEncodeError::WrongDeviceBackend);
        }
        Ok(Self {
            inner: Some(linux_cuda_impl::SessionInner::open(device_root, config)?),
            diagnostics: CudaResidentHevcEncoderSessionDiagnostics::default(),
        })
    }

    /// Acquire one FFmpeg-owned CUDA input surface.
    pub fn acquire_input_frame(
        &mut self,
    ) -> Result<CudaResidentEncodeInputFrame, ResidentEncodeError> {
        let inner = self.inner.as_mut().ok_or(ResidentEncodeError::AlreadyFinished)?;
        let frame = inner.acquire_input_frame()?;
        self.diagnostics.surfaces_acquired = self.diagnostics.surfaces_acquired.saturating_add(1);
        Ok(frame)
    }

    /// Transfer one completed CUDA surface to NVENC and drain available packets.
    pub fn submit_input_frame(
        &mut self,
        frame: CudaResidentEncodeReadyFrame,
        frame_index: u64,
    ) -> Result<(), ResidentEncodeError> {
        let inner = self.inner.as_mut().ok_or(ResidentEncodeError::AlreadyFinished)?;
        let written = inner.submit_input_frame(frame.input, frame_index)?;
        self.diagnostics.frames_submitted = self.diagnostics.frames_submitted.saturating_add(1);
        self.diagnostics.packets_written = self.diagnostics.packets_written.saturating_add(written);
        Ok(())
    }

    /// Flush delayed NVENC frames and publish the video-only trailer.
    pub fn finish(&mut self) -> Result<(), ResidentEncodeError> {
        let mut inner = self.inner.take().ok_or(ResidentEncodeError::AlreadyFinished)?;
        let written = inner.finish()?;
        self.diagnostics.packets_written = self.diagnostics.packets_written.saturating_add(written);
        Ok(())
    }

    /// Return cumulative CUDA resident-encode evidence.
    pub fn diagnostics(&self) -> CudaResidentHevcEncoderSessionDiagnostics {
        self.diagnostics
    }
}

#[cfg(all(target_os = "windows", mondrian_ffmpeg_7_1))]
mod windows_impl {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use ffmpeg::{codec, encoder, format, Dictionary, Packet, Rational};
    use ffmpeg_next as ffmpeg;
    use windows::core::Interface;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Graphics::Direct3D12::{ID3D12Fence, ID3D12Resource};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    use super::*;

    #[repr(C)]
    struct AvD3D12VaSyncContext {
        fence: *mut c_void,
        event: *mut c_void,
        fence_value: u64,
    }

    #[repr(C)]
    struct AvD3D12VaFrame {
        texture: *mut c_void,
        #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
        subresource_index: i32,
        sync_ctx: AvD3D12VaSyncContext,
        #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
        flags: i32,
    }

    // Public ABI from libavutil/hwcontext_d3d12va.h. FFmpeg added texture-array
    // fields after 8.0, so build.rs derives the layout from the installed header
    // rather than guessing from the libavcodec major version.
    #[cfg(target_pointer_width = "64")]
    const _: () = {
        assert!(std::mem::size_of::<AvD3D12VaSyncContext>() == 24);
        assert!(std::mem::offset_of!(AvD3D12VaSyncContext, fence_value) == 16);
        #[cfg(not(mondrian_ffmpeg_d3d12va_frame_v2))]
        {
            assert!(std::mem::size_of::<AvD3D12VaFrame>() == 32);
            assert!(std::mem::offset_of!(AvD3D12VaFrame, sync_ctx) == 8);
        }
        #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
        {
            assert!(std::mem::size_of::<AvD3D12VaFrame>() == 48);
            assert!(std::mem::offset_of!(AvD3D12VaFrame, sync_ctx) == 16);
            assert!(std::mem::offset_of!(AvD3D12VaFrame, flags) == 40);
        }
    };

    pub(super) struct SessionInner {
        encoder: encoder::Video,
        output: format::context::Output,
        frames_context: FramesContextRef,
        input_time_base: Rational,
        output_time_base: Rational,
        stream_index: usize,
        bit_depth: ResidentEncodeBitDepth,
        chroma_location: ResidentEncodeChromaLocation,
    }

    impl SessionInner {
        pub(super) fn open(
            device_root: &RendererHwAccelDeviceContext,
            config: ResidentHevcEncoderConfig,
        ) -> Result<Self, ResidentEncodeError> {
            ffmpeg::init().map_err(|error| ffmpeg_error("initialize", error))?;
            let codec = encoder::find_by_name("hevc_d3d12va").ok_or_else(|| {
                ResidentEncodeError::Ffmpeg {
                    stage: "find_encoder",
                    reason: "hevc_d3d12va is not registered".to_owned(),
                }
            })?;
            let device_ref = device_root.retain_ffmpeg_device_ref().map_err(|error| {
                ResidentEncodeError::Ffmpeg {
                    stage: "retain_device_root",
                    reason: error.to_string(),
                }
            })?;
            let frames_context = create_frames_context(device_ref, &config)?;
            let mut output = format::output(&config.output_path)
                .map_err(|error| ffmpeg_error("open_output", error))?;
            let global_header = output.format().flags().contains(format::Flags::GLOBAL_HEADER);
            let mut video = codec::context::Context::new_with_codec(codec)
                .encoder()
                .video()
                .map_err(|error| ffmpeg_error("create_encoder", error))?;
            let input_time_base = Rational(
                i32::try_from(config.frame_rate_den).map_err(|_| {
                    ResidentEncodeError::InvalidFrameRate {
                        numerator: config.frame_rate_num,
                        denominator: config.frame_rate_den,
                    }
                })?,
                i32::try_from(config.frame_rate_num).map_err(|_| {
                    ResidentEncodeError::InvalidFrameRate {
                        numerator: config.frame_rate_num,
                        denominator: config.frame_rate_den,
                    }
                })?,
            );
            video.set_width(config.width);
            video.set_height(config.height);
            video.set_time_base(input_time_base);
            video.set_frame_rate(Some(Rational(
                input_time_base.denominator(),
                input_time_base.numerator(),
            )));
            video.set_aspect_ratio(Rational(
                i32::try_from(config.sample_aspect_ratio_num).map_err(|_| {
                    ResidentEncodeError::InvalidSampleAspectRatio {
                        numerator: config.sample_aspect_ratio_num,
                        denominator: config.sample_aspect_ratio_den,
                    }
                })?,
                i32::try_from(config.sample_aspect_ratio_den).map_err(|_| {
                    ResidentEncodeError::InvalidSampleAspectRatio {
                        numerator: config.sample_aspect_ratio_num,
                        denominator: config.sample_aspect_ratio_den,
                    }
                })?,
            ));
            video.set_format(ffmpeg::format::Pixel::D3D12);
            video.set_gop(config.keyframe_interval_frames);
            video.set_max_b_frames(config.max_b_frames as usize);
            apply_signal(&mut video, config.colorimetry, config.full_range);
            apply_chroma_location(&mut video, config.chroma_location);
            let mut codec_flags = codec::Flags::CLOSED_GOP;
            if global_header {
                codec_flags |= codec::Flags::GLOBAL_HEADER;
            }
            video.set_flags(codec_flags);
            unsafe {
                (*video.as_mut_ptr()).hw_frames_ctx =
                    ffmpeg::ffi::av_buffer_ref(frames_context.as_ptr());
                if (*video.as_ptr()).hw_frames_ctx.is_null() {
                    return Err(ResidentEncodeError::Ffmpeg {
                        stage: "attach_frames_context",
                        reason: "av_buffer_ref returned null".to_owned(),
                    });
                }
            }
            let mut options = Dictionary::new();
            options.set("async_depth", "1");
            options.set("rc_mode", "CQP");
            options.set("qp", &config.quantizer.to_string());
            let opened = {
                let mut stream =
                    output.add_stream(codec).map_err(|error| ffmpeg_error("add_stream", error))?;
                stream.set_time_base(input_time_base);
                stream.set_parameters(&video);
                let opened = video
                    .open_as_with(codec, options)
                    .map_err(|error| ffmpeg_error("open_encoder", error))?;
                stream.set_parameters(&opened);
                opened
            };
            output.write_header().map_err(|error| ffmpeg_error("write_header", error))?;
            let output_time_base = output
                .stream(0)
                .ok_or_else(|| ResidentEncodeError::Ffmpeg {
                    stage: "resolve_output_stream",
                    reason: "output stream 0 is missing".to_owned(),
                })?
                .time_base();
            Ok(Self {
                encoder: opened,
                output,
                frames_context,
                input_time_base,
                output_time_base,
                stream_index: 0,
                bit_depth: config.bit_depth,
                chroma_location: config.chroma_location,
            })
        }

        pub(super) fn acquire_input_frame(
            &mut self,
        ) -> Result<D3D12ResidentEncodeInputFrame, ResidentEncodeError> {
            let mut frame = ffmpeg::frame::Video::empty();
            let result = unsafe {
                ffmpeg::ffi::av_hwframe_get_buffer(
                    self.frames_context.as_ptr(),
                    frame.as_mut_ptr(),
                    0,
                )
            };
            if result < 0 {
                return Err(ffmpeg_code_error("acquire_input_surface", result));
            }
            unsafe {
                (*frame.as_mut_ptr()).chroma_location =
                    ffmpeg_chroma_location(self.chroma_location);
            }
            let native = unsafe { (*frame.as_mut_ptr()).data[0].cast::<AvD3D12VaFrame>().as_mut() }
                .ok_or(ResidentEncodeError::MissingNativeSurface { object: "frame descriptor" })?;
            let next = native
                .sync_ctx
                .fence_value
                .checked_add(1)
                .ok_or(ResidentEncodeError::FenceValueExhausted)?;
            native.sync_ctx.fence_value = next;
            let resource = clone_com::<ID3D12Resource>(native.texture, "ID3D12Resource")?;
            let fence = clone_com::<ID3D12Fence>(native.sync_ctx.fence, "ID3D12Fence")?;
            Ok(D3D12ResidentEncodeInputFrame {
                frame,
                resource,
                fence,
                fence_value: next,
                bit_depth: self.bit_depth,
            })
        }

        pub(super) fn submit_input_frame(
            &mut self,
            mut input: D3D12ResidentEncodeInputFrame,
            frame_index: u64,
        ) -> Result<u64, ResidentEncodeError> {
            let pts = i64::try_from(frame_index)
                .map_err(|_| ResidentEncodeError::FrameIndexOverflow { frame_index })?;
            input.frame.set_pts(Some(pts));
            if let Err(error) = self.encoder.send_frame(&input.frame) {
                wait_for_producer_on_error(&input)?;
                return Err(ffmpeg_error("send_frame", error));
            }
            self.drain_packets(false)
        }

        pub(super) fn finish(&mut self) -> Result<u64, ResidentEncodeError> {
            self.encoder.send_eof().map_err(|error| ffmpeg_error("send_eof", error))?;
            let written = self.drain_packets(true)?;
            self.output
                .write_trailer()
                .map_err(|error| ffmpeg_error("write_trailer", error))?;
            Ok(written)
        }

        fn drain_packets(&mut self, flushing: bool) -> Result<u64, ResidentEncodeError> {
            let mut written = 0_u64;
            let mut packet = Packet::empty();
            loop {
                match self.encoder.receive_packet(&mut packet) {
                    Ok(()) => {
                        packet.set_stream(self.stream_index);
                        packet.rescale_ts(self.input_time_base, self.output_time_base);
                        packet
                            .write_interleaved(&mut self.output)
                            .map_err(|error| ffmpeg_error("write_packet", error))?;
                        written = written.saturating_add(1);
                    }
                    Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                        if flushing {
                            return Err(ResidentEncodeError::Ffmpeg {
                                stage: "receive_packet_after_eof",
                                reason: "encoder returned EAGAIN while flushing".to_owned(),
                            });
                        }
                        break;
                    }
                    Err(ffmpeg::Error::Eof) => break,
                    Err(error) => return Err(ffmpeg_error("receive_packet", error)),
                }
            }
            Ok(written)
        }
    }

    struct FramesContextRef(NonNull<ffmpeg::ffi::AVBufferRef>);

    impl FramesContextRef {
        fn as_ptr(&self) -> *mut ffmpeg::ffi::AVBufferRef {
            self.0.as_ptr()
        }
    }

    impl Drop for FramesContextRef {
        fn drop(&mut self) {
            let mut frames = self.0.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut frames) };
        }
    }

    fn create_frames_context(
        device_ref: NonNull<ffmpeg::ffi::AVBufferRef>,
        config: &ResidentHevcEncoderConfig,
    ) -> Result<FramesContextRef, ResidentEncodeError> {
        let raw = unsafe { ffmpeg::ffi::av_hwframe_ctx_alloc(device_ref.as_ptr()) };
        let Some(frames_ref) = NonNull::new(raw) else {
            let mut retained_device = device_ref.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut retained_device) };
            return Err(ResidentEncodeError::Ffmpeg {
                stage: "allocate_frames_context",
                reason: "av_hwframe_ctx_alloc returned null".to_owned(),
            });
        };
        let configured = (|| {
            let generic = unsafe {
                NonNull::new((*frames_ref.as_ptr()).data.cast::<ffmpeg::ffi::AVHWFramesContext>())
            }
            .ok_or(ResidentEncodeError::Ffmpeg {
                stage: "configure_frames_context",
                reason: "AVHWFramesContext payload is null".to_owned(),
            })?;
            let generic =
                unsafe { generic.as_ptr().as_mut() }.ok_or(ResidentEncodeError::Ffmpeg {
                    stage: "configure_frames_context",
                    reason: "AVHWFramesContext payload is not mutable".to_owned(),
                })?;
            generic.format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12;
            generic.sw_format = match config.bit_depth {
                ResidentEncodeBitDepth::Eight => ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12,
                ResidentEncodeBitDepth::Ten => ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE,
            };
            generic.width = i32::try_from(config.width).map_err(|_| {
                ResidentEncodeError::InvalidDimensions {
                    width: config.width,
                    height: config.height,
                }
            })?;
            generic.height = i32::try_from(config.height).map_err(|_| {
                ResidentEncodeError::InvalidDimensions {
                    width: config.width,
                    height: config.height,
                }
            })?;
            generic.initial_pool_size = i32::try_from(config.surface_pool_size).map_err(|_| {
                ResidentEncodeError::InvalidSurfacePoolSize { value: config.surface_pool_size }
            })?;
            let result = unsafe { ffmpeg::ffi::av_hwframe_ctx_init(frames_ref.as_ptr()) };
            if result < 0 {
                return Err(ffmpeg_code_error("initialize_frames_context", result));
            }
            Ok(())
        })();
        let mut retained_device = device_ref.as_ptr();
        unsafe { ffmpeg::ffi::av_buffer_unref(&mut retained_device) };
        if let Err(error) = configured {
            let mut raw = frames_ref.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut raw) };
            return Err(error);
        }
        Ok(FramesContextRef(frames_ref))
    }

    fn apply_signal(
        video: &mut ffmpeg::codec::encoder::video::Video,
        colorimetry: ResidentEncodeColorimetry,
        full_range: bool,
    ) {
        let raw = unsafe { &mut *video.as_mut_ptr() };
        raw.color_range = if full_range {
            ffmpeg::ffi::AVColorRange::AVCOL_RANGE_JPEG
        } else {
            ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG
        };
        let (primaries, transfer, matrix) = match colorimetry {
            ResidentEncodeColorimetry::Rec709 => (
                ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT709,
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709,
                ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT709,
            ),
            ResidentEncodeColorimetry::Rec2100Pq => (
                ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084,
                ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL,
            ),
            ResidentEncodeColorimetry::Rec2100Hlg => (
                ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67,
                ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL,
            ),
        };
        raw.color_primaries = primaries;
        raw.color_trc = transfer;
        raw.colorspace = matrix;
    }

    fn apply_chroma_location(
        video: &mut ffmpeg::codec::encoder::video::Video,
        location: ResidentEncodeChromaLocation,
    ) {
        unsafe {
            (*video.as_mut_ptr()).chroma_sample_location = ffmpeg_chroma_location(location);
        }
    }

    const fn ffmpeg_chroma_location(
        location: ResidentEncodeChromaLocation,
    ) -> ffmpeg::ffi::AVChromaLocation {
        match location {
            ResidentEncodeChromaLocation::Left => ffmpeg::ffi::AVChromaLocation::AVCHROMA_LOC_LEFT,
            ResidentEncodeChromaLocation::Center => {
                ffmpeg::ffi::AVChromaLocation::AVCHROMA_LOC_CENTER
            }
        }
    }

    fn clone_com<T: Interface>(
        raw: *mut c_void,
        object: &'static str,
    ) -> Result<T, ResidentEncodeError> {
        let borrowed = unsafe { T::from_raw_borrowed(&raw) }
            .ok_or(ResidentEncodeError::MissingNativeSurface { object })?;
        Ok(borrowed.to_owned())
    }

    fn ffmpeg_error(stage: &'static str, error: ffmpeg::Error) -> ResidentEncodeError {
        ResidentEncodeError::Ffmpeg { stage, reason: error.to_string() }
    }

    fn ffmpeg_code_error(stage: &'static str, code: i32) -> ResidentEncodeError {
        ffmpeg_error(stage, ffmpeg::Error::from(code))
    }

    fn wait_for_producer_on_error(
        input: &D3D12ResidentEncodeInputFrame,
    ) -> Result<(), ResidentEncodeError> {
        if unsafe { input.fence.GetCompletedValue() } >= input.fence_value {
            return Ok(());
        }
        let event = unsafe { CreateEventW(None, false, false, None) }.map_err(|error| {
            ResidentEncodeError::Ffmpeg {
                stage: "producer_error_cleanup",
                reason: error.to_string(),
            }
        })?;
        let event = EventHandle(event);
        unsafe { input.fence.SetEventOnCompletion(input.fence_value, event.0) }.map_err(
            |error| ResidentEncodeError::Ffmpeg {
                stage: "producer_error_cleanup",
                reason: error.to_string(),
            },
        )?;
        if unsafe { WaitForSingleObject(event.0, 30_000) } != WAIT_OBJECT_0 {
            return Err(ResidentEncodeError::ProducerFenceTimeout);
        }
        Ok(())
    }

    struct EventHandle(HANDLE);

    impl Drop for EventHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_contract_rejects_non_420_dimensions_and_unbounded_surface_pool() {
        let base = ResidentHevcEncoderConfig {
            output_path: PathBuf::from("resident.mkv"),
            width: 1920,
            height: 1080,
            frame_rate_num: 24,
            frame_rate_den: 1,
            sample_aspect_ratio_num: 1,
            sample_aspect_ratio_den: 1,
            bit_depth: ResidentEncodeBitDepth::Ten,
            colorimetry: ResidentEncodeColorimetry::Rec2100Pq,
            full_range: false,
            chroma_location: ResidentEncodeChromaLocation::Center,
            keyframe_interval_frames: 48,
            max_b_frames: 2,
            quantizer: 18,
            surface_pool_size: 8,
        };
        let mut odd = base.clone();
        odd.width = 1919;
        assert_eq!(
            odd.validate(),
            Err(ResidentEncodeError::InvalidDimensions { width: 1919, height: 1080 })
        );
        let mut invalid_sar = base.clone();
        invalid_sar.sample_aspect_ratio_den = 0;
        assert_eq!(
            invalid_sar.validate(),
            Err(ResidentEncodeError::InvalidSampleAspectRatio { numerator: 1, denominator: 0 })
        );
        let mut shallow = base;
        shallow.surface_pool_size = 1;
        assert_eq!(
            shallow.validate(),
            Err(ResidentEncodeError::InvalidSurfacePoolSize { value: 1 })
        );
    }
}
