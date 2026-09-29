//! Codec ownership behind the shared Preview packet/candidate state machine.

use ffmpeg_next as ffmpeg;

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
    VideoToolbox(#[from] super::videotoolbox_prores::VideoToolboxProResError),
    #[cfg(target_os = "macos")]
    #[error("invalid native ProRes packet contract: {0}")]
    Packet(&'static str),
}

#[cfg(target_os = "macos")]
pub(super) struct VideoToolboxCodec {
    decoder: Option<super::videotoolbox_prores::VideoToolboxProResDecoder>,
    contract: super::videotoolbox_prores::VideoToolboxProResContract,
    time_base: ffmpeg::Rational,
    pending: Option<ffmpeg::util::frame::video::Video>,
    eof: bool,
    discard: ffmpeg::codec::discard::Discard,
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxCodec {
    fn drop(&mut self) {
        self.pending = None;
        if let Some(decoder) = self.decoder.take()
            && let Err(error) = decoder.close()
        {
            tracing::warn!(%error, "VideoToolbox codec consuming close failed");
        }
    }
}

impl PreviewCodec {
    #[cfg(target_os = "macos")]
    pub(super) fn videotoolbox_prores(
        parameters: &ffmpeg::codec::Parameters,
        time_base: ffmpeg::Rational,
        range: crate::DecodedVideoRange,
    ) -> Result<Self, PreviewCodecError> {
        use super::videotoolbox_prores::{VideoToolboxProResContract, VideoToolboxProResDecoder};
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
        let contract = VideoToolboxProResContract {
            width: raw.width.try_into().map_err(|_| PreviewCodecError::Packet("invalid width"))?,
            height: raw
                .height
                .try_into()
                .map_err(|_| PreviewCodecError::Packet("invalid height"))?,
            codec: raw.codec_tag.swap_bytes(),
            primaries: raw.color_primaries as u32,
            transfer: raw.color_trc as u32,
            matrix: raw.color_space as u32,
        };
        Ok(Self::VideoToolbox(VideoToolboxCodec {
            decoder: Some(VideoToolboxProResDecoder::new(contract, range)?),
            contract,
            time_base,
            pending: None,
            eof: false,
            discard: ffmpeg::codec::discard::Discard::Default,
        }))
    }

    pub(super) fn send_packet(&mut self, packet: &ffmpeg::Packet) -> Result<(), PreviewCodecError> {
        match self {
            Self::Ffmpeg(decoder) => Ok(decoder.send_packet(packet)?),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(codec) => {
                if codec.eof {
                    return Err(ffmpeg::Error::Eof.into());
                }
                if codec.pending.is_some() {
                    return Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }.into());
                }
                // ProRes has only reference intra pictures, so the other
                // non-reference/non-key discard modes retain every picture.
                if codec.discard == ffmpeg::codec::discard::Discard::All {
                    return Ok(());
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
                let frame = codec
                    .decoder
                    .as_mut()
                    .expect("codec remains owned until consuming drop")
                    .decode_packet(data, native_pts, codec.time_base.denominator())?;
                codec.pending = Some(frame.into_av_frame(
                    pts,
                    packet.duration(),
                    codec.time_base,
                    ffmpeg::ffi::AVFieldOrder::AV_FIELD_PROGRESSIVE,
                )?);
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
            Self::VideoToolbox(codec) => match codec.pending.take() {
                Some(output) => {
                    *frame = output;
                    Ok(())
                }
                None if codec.eof => Err(ffmpeg::Error::Eof),
                None => Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }),
            },
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
                // Each intra decode already joins all callbacks. There is no
                // temporal reference state to rebuild; discard only pending
                // publication. A poisoned native decoder remains poisoned.
                codec.pending = None;
                codec.eof = false;
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
