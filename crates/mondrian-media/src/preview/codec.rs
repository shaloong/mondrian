//! Codec ownership behind the shared Preview packet/candidate state machine.

use ffmpeg_next as ffmpeg;

#[cfg(target_os = "macos")]
use super::videotoolbox::hevc::{HevcConfiguration, HevcContractError, HevcPacketFacts};
#[cfg(target_os = "macos")]
use std::collections::BTreeMap;

pub(super) enum PreviewCodec {
    Ffmpeg(ffmpeg::decoder::Video),
    #[cfg(target_os = "macos")]
    VideoToolbox(VideoToolboxCodec),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PreviewCodecError {
    #[error(transparent)]
    Ffmpeg(#[from] ffmpeg::Error),
    #[cfg(target_os = "macos")]
    #[error(transparent)]
    VideoToolbox(#[from] super::videotoolbox::VideoToolboxError),
    #[cfg(target_os = "macos")]
    #[error("invalid native packet contract: {0}")]
    Packet(&'static str),
    #[cfg(target_os = "macos")]
    #[error(transparent)]
    Hevc(#[from] HevcContractError),
}

#[cfg(target_os = "macos")]
pub(super) struct VideoToolboxCodec {
    decoder: Option<super::videotoolbox::VideoToolboxDecoder>,
    contract: super::videotoolbox::VideoToolboxContract,
    time_base: ffmpeg::Rational,
    chroma_location: ffmpeg::ffi::AVChromaLocation,
    pending: BTreeMap<i64, NativePendingFrame>,
    hevc: Option<HevcConfiguration>,
    last_emitted_pts: Option<i64>,
    failure: Option<super::videotoolbox::VideoToolboxError>,
    eof: bool,
    discard: ffmpeg::codec::discard::Discard,
}

#[cfg(target_os = "macos")]
struct NativePendingFrame {
    frame: ffmpeg::util::frame::video::Video,
    facts: HevcPacketFacts,
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxCodec {
    fn drop(&mut self) {
        self.pending.clear();
        if let Some(decoder) = self.decoder.take()
            && let Err(error) = decoder.close()
        {
            tracing::warn!(%error, "VideoToolbox codec consuming close failed");
        }
    }
}

impl PreviewCodec {
    #[cfg(target_os = "macos")]
    pub(super) fn videotoolbox(
        parameters: &ffmpeg::codec::Parameters,
        time_base: ffmpeg::Rational,
        range: crate::DecodedVideoRange,
    ) -> Result<Self, PreviewCodecError> {
        use super::videotoolbox::{VideoToolboxContract, VideoToolboxDecoder};
        // SAFETY: Parameters owns this immutable codec metadata for the borrow.
        let raw = unsafe { &*parameters.as_ptr() };
        if raw.field_order != ffmpeg::ffi::AVFieldOrder::AV_FIELD_PROGRESSIVE {
            return Err(PreviewCodecError::Packet(
                "source must prove progressive scan",
            ));
        }
        if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
            return Err(PreviewCodecError::Packet(
                "source time base must be positive",
            ));
        }
        let is_hevc = parameters.id() == ffmpeg::codec::Id::HEVC;
        let codec_tag = raw.codec_tag.swap_bytes();
        let prores_chroma = if parameters.id() == ffmpeg::codec::Id::PRORES {
            super::videotoolbox::prores_chroma_format_for_tag(codec_tag)
        } else {
            None
        };
        if !is_hevc && prores_chroma.is_none() {
            return Err(PreviewCodecError::Packet("unsupported native codec tag"));
        }
        let hevc = if is_hevc {
            let source_chroma =
                super::videotoolbox::hevc::source_chroma_format(raw.profile, raw.format).ok_or(
                    PreviewCodecError::Hevc(HevcContractError::UnsupportedSampling),
                )?;
            if raw.color_range != ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG
                || (source_chroma == 1
                    && raw.chroma_location
                        == ffmpeg::ffi::AVChromaLocation::AVCHROMA_LOC_UNSPECIFIED)
                || raw.extradata.is_null()
                || raw.extradata_size <= 0
            {
                return Err(PreviewCodecError::Hevc(
                    HevcContractError::UnsupportedSampling,
                ));
            }
            // SAFETY: Source Parameters owns the validated positive byte range.
            let bytes =
                unsafe { std::slice::from_raw_parts(raw.extradata, raw.extradata_size as usize) };
            let configuration =
                HevcConfiguration::parse(bytes, raw.width as u32, raw.height as u32)?;
            if configuration.chroma_format_idc != source_chroma {
                return Err(PreviewCodecError::Hevc(
                    HevcContractError::UnsupportedSampling,
                ));
            }
            Some(configuration)
        } else {
            None
        };
        let contract = VideoToolboxContract {
            width: raw.width.try_into().map_err(|_| PreviewCodecError::Packet("invalid width"))?,
            height: raw
                .height
                .try_into()
                .map_err(|_| PreviewCodecError::Packet("invalid height"))?,
            codec: if is_hevc {
                u32::from_be_bytes(*b"hvc1")
            } else {
                codec_tag
            },
            primaries: raw.color_primaries as u32,
            transfer: raw.color_trc as u32,
            matrix: raw.color_space as u32,
            chroma_format_idc: if let Some(configuration) = &hevc {
                configuration.chroma_format_idc
            } else {
                prores_chroma.expect("validated ProRes tag")
            },
        };
        Ok(Self::VideoToolbox(VideoToolboxCodec {
            decoder: Some(match &hevc {
                Some(configuration) => {
                    VideoToolboxDecoder::new_hevc(contract, range, configuration)?
                }
                None => VideoToolboxDecoder::new(contract, range)?,
            }),
            contract,
            time_base,
            chroma_location: raw.chroma_location,
            pending: BTreeMap::new(),
            hevc,
            last_emitted_pts: None,
            failure: None,
            eof: false,
            discard: ffmpeg::codec::discard::Discard::Default,
        }))
    }

    pub(super) fn send_packet(&mut self, packet: &ffmpeg::Packet) -> Result<(), PreviewCodecError> {
        match self {
            Self::Ffmpeg(decoder) => Ok(decoder.send_packet(packet)?),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => {
                if let Some(error) = codec.failure {
                    return Err(error.into());
                }
                if codec.eof {
                    return Err(ffmpeg::Error::Eof.into());
                }
                let reorder_depth = codec.hevc.as_ref().map_or(0, |c| c.reorder_depth);
                if codec.pending.len() > reorder_depth {
                    return Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }.into());
                }
                let pts = packet.pts().ok_or(PreviewCodecError::Packet("missing PTS"))?;
                let native_pts = pts
                    .checked_mul(i64::from(codec.time_base.numerator()))
                    .ok_or(PreviewCodecError::Packet("PTS exceeds exact CMTime range"))?;
                if packet.duration() < 0 {
                    return Err(PreviewCodecError::Packet("negative duration"));
                }
                let data = packet
                    .data()
                    .filter(|data| !data.is_empty())
                    .ok_or(PreviewCodecError::Packet("empty packet"))?;
                let facts = match &codec.hevc {
                    Some(configuration) => {
                        let changed = packet.side_data().any(|side| {
                            side.kind() == ffmpeg::codec::packet::side_data::Type::NewExtraData
                        });
                        let result = if changed {
                            Err(HevcContractError::ParameterChanged)
                        } else {
                            configuration.validate_packet(data)
                        };
                        match result {
                            Ok(facts) => facts,
                            Err(error) => {
                                codec.failure = Some(super::videotoolbox::VideoToolboxError {
                                    status: -70014,
                                    stage: 11,
                                });
                                return Err(error.into());
                            }
                        }
                    }
                    None => HevcPacketFacts { key: true, reference: true },
                };
                if codec.last_emitted_pts.is_some_and(|last| pts <= last)
                    || codec.pending.contains_key(&pts)
                {
                    codec.failure =
                        Some(super::videotoolbox::VideoToolboxError { status: -70016, stage: 8 });
                    return Err(PreviewCodecError::Packet("duplicate or late native PTS"));
                }
                let output = codec
                    .decoder
                    .as_mut()
                    .expect("codec remains owned until consuming drop")
                    .decode_packet(data, native_pts, codec.time_base.denominator())
                    .inspect_err(|error| {
                        codec.failure = Some(*error);
                    })?;
                let mut frame = output.into_av_frame(
                    pts,
                    packet.duration(),
                    codec.time_base,
                    ffmpeg::ffi::AVFieldOrder::AV_FIELD_PROGRESSIVE,
                )?;
                // SAFETY: Sole AVFrame owner. This is the frozen source fact,
                // not a platform-created chroma interpretation. HEVC parameter
                // changes were rejected before decode.
                unsafe {
                    (*frame.as_mut_ptr()).chroma_location = codec.chroma_location;
                }
                codec.pending.insert(pts, NativePendingFrame { frame, facts });
                Ok(())
            }
        }
    }

    pub(super) fn receive_frame(
        &mut self,
        frame: &mut ffmpeg::util::frame::video::Video,
    ) -> Result<(), ffmpeg::Error> {
        match self {
            Self::Ffmpeg(decoder) => decoder.receive_frame(frame),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => {
                if codec.failure.is_some() {
                    return Err(ffmpeg::Error::InvalidData);
                }
                let reorder_depth = codec.hevc.as_ref().map_or(0, |c| c.reorder_depth);
                loop {
                    if codec.pending.len() <= reorder_depth && !codec.eof {
                        return Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN });
                    }
                    let Some((pts, output)) = codec.pending.pop_first() else {
                        return Err(if codec.eof {
                            ffmpeg::Error::Eof
                        } else {
                            ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }
                        });
                    };
                    codec.last_emitted_pts = Some(pts);
                    use ffmpeg::codec::discard::Discard;
                    let suppressed = matches!(codec.discard, Discard::All)
                        || (codec.discard == Discard::NonKey && !output.facts.key)
                        || (codec.discard == Discard::NonReference && !output.facts.reference);
                    if !suppressed {
                        *frame = output.frame;
                        return Ok(());
                    }
                }
            }
        }
    }

    pub(super) fn send_eof(&mut self) -> Result<(), ffmpeg::Error> {
        match self {
            Self::Ffmpeg(decoder) => decoder.send_eof(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => {
                if codec.eof {
                    return Err(ffmpeg::Error::Eof);
                }
                if codec.failure.is_some() {
                    return Err(ffmpeg::Error::InvalidData);
                }
                if let Err(error) = codec.decoder.as_mut().unwrap().finish() {
                    codec.failure = Some(error);
                    return Err(ffmpeg::Error::External);
                }
                codec.eof = true;
                Ok(())
            }
        }
    }

    pub(super) fn flush(&mut self) {
        match self {
            Self::Ffmpeg(decoder) => decoder.flush(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => {
                codec.pending.clear();
                codec.last_emitted_pts = None;
                codec.eof = false;
                if codec.failure.is_none()
                    && let Err(error) = codec.decoder.as_mut().unwrap().reset()
                {
                    codec.failure = Some(error);
                }
            }
        }
    }

    pub(super) fn skip_frame(&mut self, discard: ffmpeg::codec::discard::Discard) {
        match self {
            Self::Ffmpeg(decoder) => decoder.skip_frame(discard),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => codec.discard = discard,
        }
    }

    pub(super) fn width(&self) -> u32 {
        match self {
            Self::Ffmpeg(decoder) => decoder.width(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => codec.contract.width,
        }
    }

    pub(super) fn height(&self) -> u32 {
        match self {
            Self::Ffmpeg(decoder) => decoder.height(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => codec.contract.height,
        }
    }

    pub(super) fn format(&self) -> ffmpeg::util::format::pixel::Pixel {
        match self {
            Self::Ffmpeg(decoder) => decoder.format(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(_) => ffmpeg::util::format::pixel::Pixel::VIDEOTOOLBOX,
        }
    }

    pub(super) fn color_space(&self) -> ffmpeg::util::color::Space {
        match self {
            Self::Ffmpeg(decoder) => decoder.color_space(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => match codec.contract.matrix {
                9 => ffmpeg::util::color::Space::BT2020NCL,
                _ => ffmpeg::util::color::Space::BT709,
            },
        }
    }

    pub(super) fn color_range(&self) -> ffmpeg::util::color::Range {
        match self {
            Self::Ffmpeg(decoder) => decoder.color_range(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(_) => ffmpeg::util::color::Range::MPEG,
        }
    }

    pub(super) fn threading(&self) -> ffmpeg::codec::threading::Config {
        match self {
            Self::Ffmpeg(decoder) => decoder.threading(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(_) => ffmpeg::codec::threading::Config {
                kind: ffmpeg::codec::threading::Type::None,
                count: 0,
            },
        }
    }

    pub(super) fn native_decode_path(&self) -> super::PreviewDecodePath {
        match self {
            Self::Ffmpeg(_) => super::PreviewDecodePath::InProcessFfmpegNative,
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(_) => super::PreviewDecodePath::InProcessVideoToolboxNative,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod native_codec_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::ffi::c_void;

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *mut c_void, plane: usize) -> *const u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *mut c_void, plane: usize) -> usize;
    }

    fn fixture() -> (
        ffmpeg::codec::Parameters,
        ffmpeg::Rational,
        Vec<ffmpeg::Packet>,
    ) {
        ffmpeg::init().unwrap();
        let path = std::env::var("MONDRIAN_HEVC_VT_TEST_MEDIA_PATH")
            .expect("explicit sixty-frame progressive Main10 motion/B-frame fixture");
        let mut input = ffmpeg::format::input(&path).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        let parameters = stream.parameters();
        let time_base = stream.time_base();
        let index = stream.index();
        let packets = input.packets().filter(|(s, _)| s.index() == index).map(|(_, p)| p).collect();
        (parameters, time_base, packets)
    }

    #[test]
    #[ignore = "requires actual VideoToolbox and a tagged progressive ProRes422 fixture"]
    fn prores422_native_packet_metadata_conflicts_poison_decoder() {
        ffmpeg::init().unwrap();
        let path = std::env::var("MONDRIAN_PRORES422_VT_TEST_MEDIA_PATH")
            .expect("explicit tagged progressive ProRes422 fixture");
        let mut input = ffmpeg::format::input(&path).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        let parameters = stream.parameters();
        let time_base = stream.time_base();
        let index = stream.index();
        let packet = input.packets().find(|(s, _)| s.index() == index).unwrap().1;
        let original = packet.data().unwrap();
        assert_eq!(&original[4..8], b"icpf");
        for (header_offset, altered, expected_status) in [
            (12, (original[20] & 0x3f) | 0xc0, -70012),
            (17, original[25] | 1, -70015),
            (14, if original[22] == 1 { 9 } else { 1 }, -70013),
        ] {
            let mut bytes = original.to_vec();
            bytes[8 + header_offset] = altered;
            let mut conflict = ffmpeg::Packet::copy(&bytes);
            conflict.set_pts(packet.pts());
            conflict.set_duration(packet.duration());
            let mut codec = PreviewCodec::videotoolbox(
                &parameters,
                time_base,
                crate::DecodedVideoRange::Limited,
            )
            .unwrap();
            assert!(matches!(
                codec.send_packet(&conflict),
                Err(PreviewCodecError::VideoToolbox(
                    super::super::videotoolbox::VideoToolboxError { status, stage: 11 }
                )) if status == expected_status
            ));
            codec.flush();
            assert!(codec.send_packet(&packet).is_err(), "poison survives reset");
        }
    }

    fn hash_pixels(frame: &ffmpeg::util::frame::video::Video) -> [u8; 32] {
        let native = frame.format() == ffmpeg::util::format::pixel::Pixel::VIDEOTOOLBOX;
        // SAFETY: This test exclusively borrows an owning immutable AVFrame;
        // native data[3] is its retained public CVPixelBuffer carrier.
        let buffer: *mut c_void = if native {
            unsafe { (*frame.as_ptr()).data[3].cast() }
        } else {
            std::ptr::null_mut()
        };
        if native {
            // Test oracle only: production never locks or reads these pixels.
            assert!(!buffer.is_null());
            // SAFETY: Live retained buffer; matching read-only unlock below.
            assert_eq!(unsafe { CVPixelBufferLockBaseAddress(buffer, 1) }, 0);
        } else {
            assert_eq!(
                frame.format(),
                ffmpeg::util::format::pixel::Pixel::YUV420P10LE
            );
        }
        let mut hash = Sha256::new();
        for plane in 0..3 {
            let width = frame.width() as usize / if plane == 0 { 1 } else { 2 };
            let height = frame.height() as usize / if plane == 0 { 1 } else { 2 };
            let mut row = vec![0; width * 2];
            let (base, stride) = if native {
                // SAFETY: Locked buffer plane addresses remain live until unlock.
                unsafe {
                    (
                        CVPixelBufferGetBaseAddressOfPlane(buffer, usize::from(plane != 0)),
                        CVPixelBufferGetBytesPerRowOfPlane(buffer, usize::from(plane != 0)),
                    )
                }
            } else {
                (frame.data(plane).as_ptr(), frame.stride(plane))
            };
            assert!(!base.is_null());
            for y in 0..height {
                for x in 0..width {
                    let offset = y * stride
                        + x * if native && plane != 0 { 4 } else { 2 }
                        + if native && plane == 2 { 2 } else { 0 };
                    // SAFETY: Loop bounds address only actual Y/Cb/Cr pixels.
                    let value =
                        unsafe { u16::from_le_bytes([*base.add(offset), *base.add(offset + 1)]) };
                    let value = if native { value >> 6 } else { value };
                    row[x * 2..x * 2 + 2].copy_from_slice(&value.to_le_bytes());
                }
                hash.update(&row);
            }
        }
        if native {
            // SAFETY: Balances the successful lock of this retained buffer.
            assert_eq!(unsafe { CVPixelBufferUnlockBaseAddress(buffer, 1) }, 0);
        }
        hash.finalize().into()
    }

    fn receive_all(codec: &mut PreviewCodec, output: &mut Vec<ffmpeg::util::frame::video::Video>) {
        loop {
            let mut frame = ffmpeg::util::frame::video::Video::empty();
            match codec.receive_frame(&mut frame) {
                Ok(()) => output.push(frame),
                Err(ffmpeg::Error::Eof) => break,
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => break,
                Err(error) => panic!("receive failed: {error}"),
            }
        }
    }

    #[test]
    #[ignore = "requires actual VideoToolbox and the explicit sixty-frame motion Main10/B fixture"]
    fn hevc_real_codec_reorders_pixels_drains_eof_and_resets_temporal_owner() {
        let (parameters, tb, packets) = fixture();
        assert_eq!(packets.len(), 60);
        assert!(
            packets.windows(2).any(|p| p[0].pts() > p[1].pts()),
            "real B-frame reorder"
        );
        let mut software = PreviewCodec::Ffmpeg(
            ffmpeg::codec::context::Context::from_parameters(parameters.clone())
                .unwrap()
                .decoder()
                .video()
                .unwrap(),
        );
        let mut reference = Vec::new();
        for packet in &packets {
            software.send_packet(packet).unwrap();
            receive_all(&mut software, &mut reference);
        }
        software.send_eof().unwrap();
        receive_all(&mut software, &mut reference);
        let oracle: BTreeMap<_, _> = reference
            .iter()
            .map(|frame| (frame.pts().unwrap(), hash_pixels(frame)))
            .collect();
        assert_eq!(oracle.len(), 60);
        assert_eq!(
            oracle.values().collect::<std::collections::HashSet<_>>().len(),
            60,
            "every motion frame must have distinct pixels"
        );
        drop(reference);
        drop(software);
        let mut native =
            PreviewCodec::videotoolbox(&parameters, tb, crate::DecodedVideoRange::Limited).unwrap();
        let PreviewCodec::VideoToolbox(codec) = &native else {
            unreachable!()
        };
        assert_eq!(
            codec.hevc.as_ref().unwrap().reorder_depth,
            2,
            "actual coded SPS bound"
        );
        let mut frames = Vec::new();
        for packet in &packets {
            native.send_packet(packet).unwrap();
            if let PreviewCodec::VideoToolbox(codec) = &native {
                assert!(
                    codec.pending.len() <= 3,
                    "bounded coded reorder plus current output"
                );
            }
            receive_all(&mut native, &mut frames);
        }
        assert_eq!(frames.len(), 58, "EOF must release two withheld pictures");
        native.send_eof().unwrap();
        receive_all(&mut native, &mut frames);
        assert_eq!(frames.len(), 60);
        assert!(frames.windows(2).all(|f| f[0].pts() < f[1].pts()));
        for frame in &frames {
            assert_eq!(
                hash_pixels(frame),
                oracle[&frame.pts().unwrap()],
                "entire raster at exact PTS"
            );
            assert_ne!(
                frame.chroma_location(),
                ffmpeg::util::chroma::Location::Unspecified
            );
        }
        assert!(matches!(native.send_eof(), Err(ffmpeg::Error::Eof)));
        // Real seek reset consumes old temporal state, then restarts at an IDR.
        native.flush();
        let mut restarted = Vec::new();
        for packet in &packets[..8] {
            native.send_packet(packet).unwrap();
            receive_all(&mut native, &mut restarted);
        }
        native.send_eof().unwrap();
        receive_all(&mut native, &mut restarted);
        assert_eq!(restarted.len(), 8);
        assert_eq!(restarted[0].pts(), Some(0));
        for frame in &restarted {
            assert_eq!(hash_pixels(frame), oracle[&frame.pts().unwrap()]);
        }
        drop(native); // actual consuming native close; retained frames still own their pixels
        for frame in frames.iter().take(5) {
            assert_eq!(hash_pixels(frame), oracle[&frame.pts().unwrap()]);
        }
        eprintln!("hardware-only native codec: 60 exact ordered frames, 2 EOF frames, reset=8, retained-after-close=5");
    }

    #[test]
    #[ignore = "requires actual VideoToolbox and an explicit Main10/B fixture"]
    fn hevc_parameter_change_and_color_conflict_poison_native_codec() {
        let (parameters, tb, packets) = fixture();
        let mut codec =
            PreviewCodec::videotoolbox(&parameters, tb, crate::DecodedVideoRange::Limited).unwrap();
        let PreviewCodec::VideoToolbox(state) = &codec else {
            unreachable!()
        };
        let configuration = state.hevc.as_ref().unwrap();
        let mut sps =
            configuration.parameter_sets.iter().find(|s| s[0] >> 1 == 33).unwrap().clone();
        *sps.last_mut().unwrap() ^= 1;
        let mut payload = (sps.len() as u32).to_be_bytes().to_vec();
        payload.extend(sps);
        payload.extend_from_slice(packets[0].data().unwrap());
        let mut changed = ffmpeg::Packet::copy(&payload);
        changed.set_pts(packets[0].pts());
        changed.set_duration(packets[0].duration());
        assert!(matches!(
            codec.send_packet(&changed),
            Err(PreviewCodecError::Hevc(HevcContractError::ParameterChanged))
        ));
        codec.flush();
        assert!(
            codec.send_packet(&packets[0]).is_err(),
            "poison survives reset"
        );
        drop(codec);
        let mut conflict = parameters.clone();
        // SAFETY: Sole owned metadata; intentionally conflicts with coded PQ VUI.
        unsafe {
            (*conflict.as_mut_ptr()).color_trc =
                ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67;
        }
        let mut codec =
            PreviewCodec::videotoolbox(&conflict, tb, crate::DecodedVideoRange::Limited).unwrap();
        assert!(matches!(
            codec.send_packet(&packets[0]),
            Err(PreviewCodecError::VideoToolbox(
                super::super::videotoolbox::VideoToolboxError { stage: 11, .. }
            ))
        ));
        assert!(codec.send_packet(&packets[0]).is_err());
    }
}
