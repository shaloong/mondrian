//! Exclusive packet decoder and immutable surface owners for public VideoToolbox.
//! Demux, candidate ordering, clocks and color interpretation stay with Preview.

use crate::decoder::DecodedVideoRange;
use std::ffi::c_void;
use std::ptr::NonNull;

pub(super) mod hevc;

/// Explicit source contract for limited-range ProRes 4444/XQ or HEVC Main10.
/// CICP values must come from the resolved source contract, never defaults.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct VideoToolboxContract {
    /// Decoded visible width, before presentation orientation.
    pub width: u32,
    /// Decoded visible height, before presentation orientation.
    pub height: u32,
    /// CoreMedia big-endian FourCC (`ap4h`, `ap4x` or `hvc1`).
    pub codec: u32,
    /// CICP color primaries (1 or 9).
    pub primaries: u32,
    /// CICP transfer characteristic (1, 16 or 18).
    pub transfer: u32,
    /// CICP matrix coefficients (1 or 9).
    pub matrix: u32,
}

/// Native stage and OSStatus are retained for structured rejection diagnostics.
#[derive(Debug, Clone, Copy, Default, thiserror::Error)]
#[error("VideoToolbox stage {stage} failed with status {status}")]
#[repr(C)]
pub(super) struct VideoToolboxError {
    /// Apple OSStatus, or adapter rejection code.
    pub status: i32,
    /// Adapter stage from the native codec boundary.
    pub stage: u32,
}

#[derive(Debug, Default)]
#[repr(C)]
struct NativeOutput {
    pixel_buffer: *mut c_void,
    allocation_bytes: usize,
    width: u32,
    height: u32,
    pixel_format: u32,
    callback_count: u32,
    hardware_observed: u32,
}

impl Drop for NativeOutput {
    fn drop(&mut self) {
        // SAFETY: The adapter transfers one retained reference, including on
        // any path rejected by Rust. Release accepts null and consumes it once.
        unsafe { mdn_vt_surface_release(self.pixel_buffer) };
    }
}

#[repr(C)]
struct NativeParameterSet {
    bytes: *const u8,
    size: usize,
}

unsafe extern "C" {
    fn mdn_vt_create(
        contract: *const VideoToolboxContract,
        sets: *const NativeParameterSet,
        count: usize,
        nal_length: i32,
        error: *mut VideoToolboxError,
    ) -> *mut c_void;
    fn mdn_vt_decode(
        decoder: *mut c_void,
        data: *const u8,
        size: usize,
        pts_value: i64,
        pts_scale: i32,
        output: *mut NativeOutput,
        error: *mut VideoToolboxError,
    ) -> i32;
    fn mdn_vt_finish(decoder: *mut c_void, error: *mut VideoToolboxError) -> i32;
    fn mdn_vt_reset(decoder: *mut c_void, error: *mut VideoToolboxError) -> i32;
    fn mdn_vt_close(decoder: *mut c_void, error: *mut VideoToolboxError) -> i32;
    fn mdn_vt_surface_release(surface: *mut c_void);
}

/// One exclusively owned decoder. Successful packets return independent owners;
/// consuming shutdown therefore cannot invalidate outstanding frame leases.
#[derive(Debug)]
pub(super) struct VideoToolboxDecoder {
    pointer: Option<NonNull<c_void>>,
    contract: VideoToolboxContract,
}

// SAFETY: Moving exclusive ownership does not overlap calls. VideoToolbox does
// not require the session's creation thread; callbacks synchronize in C. The
// pointer deliberately remains !Sync and decoding requires an exclusive borrow.
unsafe impl Send for VideoToolboxDecoder {}

impl VideoToolboxDecoder {
    /// Require hardware decoding with an explicit limited-range source contract.
    pub(super) fn new(
        contract: VideoToolboxContract,
        range: DecodedVideoRange,
    ) -> Result<Self, VideoToolboxError> {
        Self::create(contract, range, None)
    }

    pub(super) fn new_hevc(
        contract: VideoToolboxContract,
        range: DecodedVideoRange,
        configuration: &hevc::HevcConfiguration,
    ) -> Result<Self, VideoToolboxError> {
        Self::create(contract, range, Some(configuration))
    }

    fn create(
        contract: VideoToolboxContract,
        range: DecodedVideoRange,
        configuration: Option<&hevc::HevcConfiguration>,
    ) -> Result<Self, VideoToolboxError> {
        if range != DecodedVideoRange::Limited {
            return Err(VideoToolboxError { status: -70001, stage: 11 });
        }
        let sets: Vec<_> = configuration
            .into_iter()
            .flat_map(|configuration| &configuration.parameter_sets)
            .map(|bytes| NativeParameterSet { bytes: bytes.as_ptr(), size: bytes.len() })
            .collect();
        let mut error = VideoToolboxError::default();
        // SAFETY: Descriptors and their bytes live through creation. CoreMedia
        // constructs and owns the format description before returning.
        let pointer = unsafe {
            mdn_vt_create(
                &contract,
                sets.as_ptr(),
                sets.len(),
                configuration.map_or(0, |c| c.length_size as i32),
                &mut error,
            )
        };
        let pointer = NonNull::new(pointer).ok_or(error)?;
        Ok(Self { pointer: Some(pointer), contract })
    }

    pub(super) fn finish(&mut self) -> Result<(), VideoToolboxError> {
        let mut error = VideoToolboxError::default();
        // SAFETY: Exclusive live session; the adapter joins every callback.
        let success = unsafe { mdn_vt_finish(self.pointer.unwrap().as_ptr(), &mut error) };
        if success == 0 {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(super) fn reset(&mut self) -> Result<(), VideoToolboxError> {
        let mut error = VideoToolboxError::default();
        // SAFETY: Exclusive live session. Old temporal work is consumed before
        // new session creation; retained output allocations remain independent.
        let success = unsafe { mdn_vt_reset(self.pointer.unwrap().as_ptr(), &mut error) };
        if success == 0 {
            Err(error)
        } else {
            Ok(())
        }
    }

    /// Decode one complete packet at an exact rational presentation time.
    /// Native completion is awaited before returning. A codec failure
    /// poisons the session; the caller must consume it rather than retry it.
    pub(super) fn decode_packet(
        &mut self,
        packet: &[u8],
        pts_value: i64,
        pts_scale: i32,
    ) -> Result<VideoToolboxFrame, VideoToolboxError> {
        let mut output = NativeOutput::default();
        let mut error = VideoToolboxError::default();
        let pointer = self.pointer.expect("live decoder before consuming close");
        // SAFETY: Exclusive decoder borrow, live packet, initialized out fields.
        // Native code copies compressed bytes and joins all callbacks on return.
        let success = unsafe {
            mdn_vt_decode(
                pointer.as_ptr(),
                packet.as_ptr(),
                packet.len(),
                pts_value,
                pts_scale,
                &mut output,
                &mut error,
            )
        };
        if success == 0 {
            return Err(error);
        }
        if output.pixel_buffer.is_null()
            || output.allocation_bytes == 0
            || output.width != self.contract.width
            || output.height != self.contract.height
            || output.pixel_format
                != if self.contract.codec == u32::from_be_bytes(*b"hvc1") {
                    u32::from_be_bytes(*b"x420")
                } else {
                    u32::from_be_bytes(*b"s4as")
                }
            || output.callback_count != 1
            || output.hardware_observed != 1
        {
            return Err(VideoToolboxError { status: -70003, stage: 9 });
        }
        Ok(VideoToolboxFrame { output, contract: self.contract })
    }

    /// Consume the session and join callbacks even when shutdown reports error.
    pub(super) fn close(mut self) -> Result<(), VideoToolboxError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), VideoToolboxError> {
        let Some(pointer) = self.pointer.take() else {
            return Ok(());
        };
        let mut error = VideoToolboxError::default();
        // SAFETY: Taking the sole pointer makes native consuming close exactly
        // once, including failure and the subsequent Rust destructor.
        if unsafe { mdn_vt_close(pointer.as_ptr(), &mut error) } == 0 {
            Err(error)
        } else {
            Ok(())
        }
    }
}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        if let Err(error) = self.close_inner() {
            tracing::warn!(%error, "VideoToolbox decoder drop failed");
        }
    }
}

/// Independently retained immutable native surface, with observed hardware decode.
#[derive(Debug)]
pub(super) struct VideoToolboxFrame {
    output: NativeOutput,
    contract: VideoToolboxContract,
}

impl VideoToolboxFrame {
    /// Move the retained surface into the shared AVFrame candidate carrier.
    /// This does not invoke an FFmpeg decoder, invent a software pixel layout,
    /// or create an AVHWFramesContext. Only proven progressive sources may use
    /// this adapter; field processing must reject other source scan contracts.
    pub(super) fn into_av_frame(
        self,
        pts: i64,
        duration: i64,
        time_base: ffmpeg_next::Rational,
        field_order: ffmpeg_next::ffi::AVFieldOrder,
    ) -> Result<ffmpeg_next::util::frame::video::Video, VideoToolboxError> {
        use ffmpeg_next::ffi;
        if pts == ffi::AV_NOPTS_VALUE
            || duration < 0
            || time_base.numerator() <= 0
            || time_base.denominator() <= 0
            || field_order != ffi::AVFieldOrder::AV_FIELD_PROGRESSIVE
        {
            return Err(VideoToolboxError { status: -70001, stage: 11 });
        }
        // SAFETY: Fresh AVFrame allocation; null is checked before dereference.
        let raw = unsafe { ffi::av_frame_alloc() };
        let Some(raw) = NonNull::new(raw) else {
            return Err(VideoToolboxError { status: -70002, stage: 9 });
        };
        // SAFETY: Video owns this allocation from here, including error returns.
        let mut frame = unsafe { ffmpeg_next::util::frame::video::Video::wrap(raw.as_ptr()) };
        let contract = self.contract;
        let surface = self.output.pixel_buffer;
        let owner = Box::into_raw(Box::new(self));
        // SAFETY: AVBuffer owns the boxed surface owner only on success. Its
        // zero-sized data is an opaque resource, never a CPU pixel allocation.
        let buffer = unsafe {
            ffi::av_buffer_create(
                surface.cast(),
                0,
                Some(release_av_frame_surface),
                owner.cast(),
                ffi::AV_BUFFER_FLAG_READONLY,
            )
        };
        if buffer.is_null() {
            // SAFETY: av_buffer_create leaves ownership with its caller on error.
            drop(unsafe { Box::from_raw(owner) });
            return Err(VideoToolboxError { status: -70002, stage: 9 });
        }
        // SAFETY: Sole mutable AVFrame owner; buffer now owns the +1 surface.
        unsafe {
            let raw = &mut *frame.as_mut_ptr();
            raw.buf[0] = buffer;
            raw.data[3] = surface.cast();
            raw.format = ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
            raw.width = contract.width as i32;
            raw.height = contract.height as i32;
            raw.pts = pts;
            raw.best_effort_timestamp = pts;
            raw.time_base = time_base.into();
            #[cfg(mondrian_ffmpeg_7_0)]
            {
                raw.duration = duration;
            }
            #[cfg(not(mondrian_ffmpeg_7_0))]
            {
                raw.pkt_duration = duration;
            }
            raw.color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
            raw.color_primaries = match contract.primaries {
                9 => ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
                _ => ffi::AVColorPrimaries::AVCOL_PRI_BT709,
            };
            raw.color_trc = match contract.transfer {
                16 => ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084,
                18 => ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67,
                _ => ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709,
            };
            raw.colorspace = match contract.matrix {
                9 => ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL,
                _ => ffi::AVColorSpace::AVCOL_SPC_BT709,
            };
        }
        Ok(frame)
    }
}

unsafe extern "C" fn release_av_frame_surface(opaque: *mut c_void, _data: *mut u8) {
    // SAFETY: Only a successful av_buffer_create installs this callback. The
    // final AVBuffer unref consumes its unique box once; frame clones share it.
    // The callback may run on another thread: output pixels are immutable and
    // CoreVideo release is thread safe; no thread-affine decoder is in the box.
    drop(unsafe { Box::from_raw(opaque.cast::<VideoToolboxFrame>()) });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_packet() -> (
        VideoToolboxContract,
        ffmpeg_next::ffi::AVFieldOrder,
        Vec<u8>,
    ) {
        use ffmpeg_next as ffmpeg;
        ffmpeg::init().unwrap();
        let path = std::env::var("MONDRIAN_PRORES_VT_TEST_MEDIA_PATH")
            .expect("explicit ProRes fixture is required");
        let mut input = ffmpeg::format::input(&path).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        let index = stream.index();
        let parameters = stream.parameters();
        // SAFETY: Owned parameters remain live for this immutable metadata read.
        let raw = unsafe { &*parameters.as_ptr() };
        let contract = VideoToolboxContract {
            width: raw.width.try_into().unwrap(),
            height: raw.height.try_into().unwrap(),
            codec: raw.codec_tag.swap_bytes(),
            primaries: raw.color_primaries as u32,
            transfer: raw.color_trc as u32,
            matrix: raw.color_space as u32,
        };
        assert_eq!(raw.color_range, ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG);
        let packet = input.packets().find(|(s, _)| s.index() == index).unwrap().1;
        (contract, raw.field_order, packet.data().unwrap().to_vec())
    }

    #[test]
    #[ignore = "requires real VideoToolbox and an explicit tagged ProRes fixture"]
    fn native_packet_rejects_frame_contract_changes_before_output() {
        let (contract, _, packet) = fixture_packet();
        assert!(packet.len() >= 28);
        for (label, offset, value) in [
            ("primaries", 22, 9),
            ("transfer", 23, 16),
            ("missing transfer", 23, 2),
            ("alpha", 25, (packet[25] & 0xf0) | 3),
            ("matrix", 24, 9),
            ("interlaced", 20, packet[20] | 4),
            ("chroma", 20, (packet[20] & 0x3f) | 0x80),
            ("width", 17, packet[17] ^ 1),
        ] {
            assert_ne!(
                packet[offset], value,
                "fixture must exercise {label} conflict"
            );
            let mut changed = packet.clone();
            changed[offset] = value;
            let mut decoder =
                VideoToolboxDecoder::new(contract, DecodedVideoRange::Limited).unwrap();
            let result = decoder.decode_packet(&changed, 0, 30);
            assert!(
                result.is_err(),
                "native decoder accepted conflicting {label}"
            );
            let error = result.unwrap_err();
            assert_eq!(error.stage, 11, "{label} must fail at contract admission");
            assert!(decoder.decode_packet(&packet, 0, 30).is_err());
            decoder.close().unwrap();
        }
    }

    #[test]
    fn missing_color_metadata_is_rejected_before_session_creation() {
        let contract = VideoToolboxContract {
            width: 1920,
            height: 1080,
            codec: u32::from_be_bytes(*b"ap4h"),
            primaries: 1,
            transfer: 0,
            matrix: 1,
        };
        let error = VideoToolboxDecoder::new(contract, DecodedVideoRange::Limited).unwrap_err();
        assert_eq!(error.stage, 11);
        assert_ne!(error.status, 0);
        for range in [DecodedVideoRange::Full, DecodedVideoRange::Unknown] {
            assert!(VideoToolboxDecoder::new(contract, range).is_err());
        }
    }

    #[test]
    #[ignore = "requires a real Metal/VideoToolbox session and explicit ProRes fixture"]
    fn native_packet_output_survives_consuming_close_and_failed_session_is_rejected() {
        use ffmpeg_next as ffmpeg;
        let (contract, field_order, packet) = fixture_packet();
        let data = packet.as_slice();
        let mut decoder = VideoToolboxDecoder::new(contract, DecodedVideoRange::Limited).unwrap();
        let first = decoder.decode_packet(data, 0, 30).unwrap();
        let second = decoder.decode_packet(data, 1, 30).unwrap();
        assert_ne!(first.output.pixel_buffer, second.output.pixel_buffer);
        assert_eq!(
            (first.output.width, first.output.height),
            (contract.width, contract.height)
        );
        assert!(
            first.output.allocation_bytes >= contract.width as usize * contract.height as usize * 8
        );
        drop(second);
        decoder.close().unwrap();
        unsafe extern "C" {
            fn CVPixelBufferGetWidth(buffer: *const c_void) -> usize;
        }
        // SAFETY: Independently retained first frame is alive after decoder close.
        assert_eq!(
            unsafe { CVPixelBufferGetWidth(first.output.pixel_buffer) },
            contract.width as usize
        );
        let allocation_bytes = first.output.allocation_bytes;
        let frame = first.into_av_frame(0, 1, (1, 30).into(), field_order).unwrap();
        assert_eq!(frame.pts(), Some(0));
        assert_eq!(frame.packet().duration, 1);
        // SAFETY: Immutable metadata read on an owned frame.
        assert!(unsafe { (*frame.as_ptr()).hw_frames_ctx.is_null() });
        let mut field_processor =
            super::super::field_processing::PreviewFieldProcessingSession::new(
                super::super::PreviewSourceFieldProcessing::Progressive,
                (1, 30).into(),
                std::path::Path::new("native-prores-fixture"),
            );
        let processed = field_processor.push(0, &frame).unwrap();
        drop(frame);
        assert_eq!(processed.len(), 1);
        let retained = &processed[0];
        assert_eq!(retained.pts(), Some(0));
        assert_eq!(
            super::super::native_frame::ffmpeg_cv_pixel_buffer_allocation_bytes(retained),
            Some(allocation_bytes)
        );
        let family = super::super::native_frame::PreviewNativeOutputTracker::default();
        let session = super::super::native_frame::PreviewNativeOutputTracker::default();
        let lease =
            super::super::native_frame::PreviewDecodeSessionOutputLease::acquire(&family, &session)
                .unwrap();
        let native = super::super::frame_materialization::materialize_native_decoded_frame(
            retained,
            super::super::PreviewSourceColorContract::new(
                mondrian_core::ColorSpace::Rec709,
                crate::DecodedVideoRangeContract::Automatic {
                    probed_range: DecodedVideoRange::Limited,
                },
            ),
            ffmpeg::codec::Id::PRORES,
            Some(lease),
            super::super::PreviewDecodePath::InProcessVideoToolboxNative,
        )
        .unwrap();
        drop(processed);
        assert_eq!(
            native.surface_format,
            crate::DecodedVideoSurfaceFormat::Yuva444sp16
        );
        assert_eq!(
            native.diagnostics.path,
            super::super::PreviewDecodePath::InProcessVideoToolboxNative
        );
        assert_eq!(native.diagnostics.decoded_video_sampling.bit_depth, 16);
        assert_eq!(
            native.diagnostics.decoded_video_sampling.range,
            DecodedVideoRange::Limited
        );
        assert_eq!(family.outstanding(), 1);
        assert_eq!(session.outstanding(), 1);
        let clone = native.clone();
        drop(native);
        let borrowed = clone.handle.cv_pixel_buffer().unwrap();
        // SAFETY: The final payload clone still owns its AVBuffer and CVPixelBuffer.
        assert_eq!(
            unsafe { CVPixelBufferGetWidth(borrowed.as_ptr().as_ptr()) },
            contract.width as usize
        );
        drop(clone);
        assert_eq!(family.outstanding(), 0);
        assert_eq!(session.outstanding(), 0);
        let mut decoder = VideoToolboxDecoder::new(contract, DecodedVideoRange::Limited).unwrap();
        assert!(decoder.decode_packet(&[0, 1, 2, 3], 0, 30).is_err());
        assert!(decoder.decode_packet(data, 0, 30).is_err());
        decoder.close().unwrap();
    }
}
