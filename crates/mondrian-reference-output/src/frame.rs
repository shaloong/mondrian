use mondrian_core::{AudioChannelLayout, ColorMatrixCoefficients};
use sha2::{Digest, Sha256};

use crate::{
    ReferenceOutputPixelFormat, ReferenceOutputRange, ReferenceOutputSignal,
    ReferenceOutputSignalError,
};

/// One clean-feed Program Output video frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceVideoFrame {
    signal: ReferenceOutputSignal,
    frame_index: u64,
    row_bytes: u32,
    bytes: Vec<u8>,
}

impl ReferenceVideoFrame {
    /// Construct a frame only from the final Program Output boundary.
    pub fn from_program_output(
        signal: &ReferenceOutputSignal,
        frame_index: u64,
        row_bytes: u32,
        bytes: Vec<u8>,
    ) -> Result<Self, ReferenceOutputPayloadError> {
        signal.validate()?;
        let minimum_row_bytes = minimum_row_bytes(signal.width, signal.pixel_format)?;
        if row_bytes < minimum_row_bytes {
            return Err(ReferenceOutputPayloadError::VideoRowBytes {
                minimum: minimum_row_bytes,
                actual: row_bytes,
            });
        }
        let expected = usize::try_from(row_bytes)
            .ok()
            .and_then(|row| row.checked_mul(signal.height as usize))
            .ok_or(ReferenceOutputPayloadError::ExtentOverflow)?;
        if bytes.len() != expected {
            return Err(ReferenceOutputPayloadError::VideoByteLength {
                expected,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            signal: signal.clone(),
            frame_index,
            row_bytes,
            bytes,
        })
    }

    /// Zero-based scheduled frame coordinate.
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Exact signal used when the clean-feed payload was packed.
    pub const fn signal(&self) -> &ReferenceOutputSignal {
        &self.signal
    }

    /// Host row stride.
    pub const fn row_bytes(&self) -> u32 {
        self.row_bytes
    }

    /// Exact packed payload.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Deterministic payload digest for diagnostics and qualification.
    pub fn sha256(&self) -> [u8; 32] {
        Sha256::digest(&self.bytes).into()
    }
}

/// Signed 24-bit embedded-audio samples stored in interleaved i32 lanes.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceAudioFrame {
    signal: ReferenceOutputSignal,
    frame_index: u64,
    channel_layout: AudioChannelLayout,
    samples: Vec<i32>,
}

impl ReferenceAudioFrame {
    /// Construct exact 48 kHz audio paired with one video frame.
    pub fn new(
        signal: &ReferenceOutputSignal,
        frame_index: u64,
        samples: Vec<i32>,
    ) -> Result<Self, ReferenceOutputPayloadError> {
        signal.validate()?;
        let expected_frames = signal.audio_frames_for_video_frame(frame_index)? as usize;
        let expected_samples = expected_frames
            .checked_mul(signal.audio_layout.channel_count())
            .ok_or(ReferenceOutputPayloadError::ExtentOverflow)?;
        if samples.len() != expected_samples {
            return Err(ReferenceOutputPayloadError::AudioSampleLength {
                expected: expected_samples,
                actual: samples.len(),
            });
        }
        if let Some(sample) = samples
            .iter()
            .copied()
            .find(|sample| !(-8_388_608..=8_388_607).contains(sample))
        {
            return Err(ReferenceOutputPayloadError::AudioSampleOutOfRange { sample });
        }
        Ok(Self {
            signal: signal.clone(),
            frame_index,
            channel_layout: signal.audio_layout,
            samples,
        })
    }

    /// Zero-based video-frame coordinate owning this audio interval.
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Exact signal used to derive cadence and channel order.
    pub const fn signal(&self) -> &ReferenceOutputSignal {
        &self.signal
    }

    /// Semantic embedded-audio layout.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }

    /// Interleaved signed 24-bit values in i32 lanes.
    pub fn samples(&self) -> &[i32] {
        &self.samples
    }

    /// Number of interleaved sample frames.
    pub fn sample_frames(&self) -> usize {
        self.samples.len() / self.channel_layout.channel_count()
    }
}

/// Atomically scheduled clean-feed video plus its exact audio interval.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceOutputBundle {
    /// Final Program Output video payload.
    pub video: ReferenceVideoFrame,
    /// Embedded 48 kHz audio payload.
    pub audio: ReferenceAudioFrame,
}

impl ReferenceOutputBundle {
    /// Validate common time identity.
    pub fn validate(&self) -> Result<(), ReferenceOutputPayloadError> {
        if self.video.frame_index != self.audio.frame_index {
            return Err(ReferenceOutputPayloadError::BundleTimeMismatch {
                video: self.video.frame_index,
                audio: self.audio.frame_index,
            });
        }
        if self.video.signal != self.audio.signal {
            return Err(ReferenceOutputPayloadError::BundleSignalMismatch);
        }
        Ok(())
    }

    /// Shared zero-based schedule coordinate.
    pub const fn frame_index(&self) -> u64 {
        self.video.frame_index
    }
}

/// Pack encoded Program Output RGB floats as compact v210 rows.
///
/// The input is straight RGBA in the signal's already-encoded transfer. RGB is
/// converted to non-constant-luminance Y'CbCr, horizontal chroma is averaged
/// over each pair, and code values are rounded once into the requested range.
/// Alpha is deliberately absent from clean-feed SDI.
pub fn pack_encoded_rgb_to_v210(
    signal: &ReferenceOutputSignal,
    rgba: &[[f32; 4]],
) -> Result<(u32, Vec<u8>), ReferenceVideoPackingError> {
    signal.validate()?;
    if signal.pixel_format != ReferenceOutputPixelFormat::Yuv422TenV210 {
        return Err(ReferenceVideoPackingError::PixelFormatMismatch);
    }
    let expected = pixel_len(signal)?;
    if rgba.len() != expected {
        return Err(ReferenceVideoPackingError::RgbaLength { expected, actual: rgba.len() });
    }
    let (kr, kb) = matrix_luma_coefficients(signal.color_space.encoding().matrix)?;
    let row_bytes = minimum_row_bytes(signal.width, signal.pixel_format)?;
    let mut packed = vec![0_u8; row_bytes as usize * signal.height as usize];
    for y in 0..signal.height as usize {
        let source_row = &rgba[y * signal.width as usize..][..signal.width as usize];
        let output_row = &mut packed[y * row_bytes as usize..][..row_bytes as usize];
        for group in 0..signal.width.div_ceil(6) as usize {
            let mut luma = [legal_black(signal.range); 6];
            let mut cb = [chroma_neutral(signal.range); 3];
            let mut cr = [chroma_neutral(signal.range); 3];
            for pair in 0..3 {
                let pixel0 = group * 6 + pair * 2;
                if pixel0 >= signal.width as usize {
                    continue;
                }
                let pixel1 = (pixel0 + 1).min(signal.width as usize - 1);
                let first = rgb_to_ycbcr(
                    source_row[pixel0][0],
                    source_row[pixel0][1],
                    source_row[pixel0][2],
                    kr,
                    kb,
                )?;
                let second = rgb_to_ycbcr(
                    source_row[pixel1][0],
                    source_row[pixel1][1],
                    source_row[pixel1][2],
                    kr,
                    kb,
                )?;
                luma[pair * 2] = quantize_luma(first.0, signal.range);
                if pixel0 + 1 < signal.width as usize {
                    luma[pair * 2 + 1] = quantize_luma(second.0, signal.range);
                }
                cb[pair] = quantize_chroma((first.1 + second.1) * 0.5, signal.range);
                cr[pair] = quantize_chroma((first.2 + second.2) * 0.5, signal.range);
            }
            let words = [
                pack_three_10(cb[0], luma[0], cr[0]),
                pack_three_10(luma[1], cb[1], luma[2]),
                pack_three_10(cr[1], luma[3], cb[2]),
                pack_three_10(luma[4], cr[2], luma[5]),
            ];
            let byte_offset = group * 16;
            for (word, destination) in words
                .into_iter()
                .zip(output_row[byte_offset..byte_offset + 16].chunks_exact_mut(4))
            {
                destination.copy_from_slice(&word.to_le_bytes());
            }
        }
    }
    Ok((row_bytes, packed))
}

/// Pack encoded Program Output RGB floats as portable 12-bit RGB lanes.
pub fn pack_encoded_rgb_to_rgb12(
    signal: &ReferenceOutputSignal,
    rgba: &[[f32; 4]],
) -> Result<(u32, Vec<u8>), ReferenceVideoPackingError> {
    signal.validate()?;
    if signal.pixel_format != ReferenceOutputPixelFormat::Rgb444TwelveIn16Le {
        return Err(ReferenceVideoPackingError::PixelFormatMismatch);
    }
    let expected = pixel_len(signal)?;
    if rgba.len() != expected {
        return Err(ReferenceVideoPackingError::RgbaLength { expected, actual: rgba.len() });
    }
    let row_bytes =
        signal.width.checked_mul(6).ok_or(ReferenceVideoPackingError::ExtentOverflow)?;
    let mut packed = Vec::with_capacity(row_bytes as usize * signal.height as usize);
    for pixel in rgba {
        for component in &pixel[..3] {
            if !component.is_finite() {
                return Err(ReferenceVideoPackingError::NonFiniteComponent);
            }
            let code = (component.clamp(0.0, 1.0) * 4095.0).round() as u16;
            packed.extend_from_slice(&code.to_le_bytes());
        }
    }
    Ok((row_bytes, packed))
}

/// Quantize interleaved float Program Output audio into signed 24-bit lanes.
pub fn pack_f32_audio_to_s24(samples: &[f32]) -> Result<Vec<i32>, ReferenceAudioPackingError> {
    samples
        .iter()
        .map(|sample| {
            if !sample.is_finite() {
                return Err(ReferenceAudioPackingError::NonFiniteSample);
            }
            Ok(if *sample <= -1.0 {
                -8_388_608
            } else {
                (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32
            })
        })
        .collect()
}

fn minimum_row_bytes(
    width: u32,
    pixel_format: ReferenceOutputPixelFormat,
) -> Result<u32, ReferenceOutputPayloadError> {
    match pixel_format {
        ReferenceOutputPixelFormat::Yuv422TenV210 => width
            .div_ceil(6)
            .checked_mul(16)
            .ok_or(ReferenceOutputPayloadError::ExtentOverflow),
        ReferenceOutputPixelFormat::Rgb444TwelveIn16Le => {
            width.checked_mul(6).ok_or(ReferenceOutputPayloadError::ExtentOverflow)
        }
    }
}

fn pixel_len(signal: &ReferenceOutputSignal) -> Result<usize, ReferenceVideoPackingError> {
    usize::try_from(signal.width)
        .ok()
        .and_then(|width| width.checked_mul(signal.height as usize))
        .ok_or(ReferenceVideoPackingError::ExtentOverflow)
}

fn matrix_luma_coefficients(
    matrix: ColorMatrixCoefficients,
) -> Result<(f32, f32), ReferenceVideoPackingError> {
    match matrix {
        ColorMatrixCoefficients::Bt709 => Ok((0.2126, 0.0722)),
        ColorMatrixCoefficients::Bt2020NonConstant => Ok((0.2627, 0.0593)),
        _ => Err(ReferenceVideoPackingError::UnsupportedYuvMatrix { matrix }),
    }
}

fn rgb_to_ycbcr(
    r: f32,
    g: f32,
    b: f32,
    kr: f32,
    kb: f32,
) -> Result<(f32, f32, f32), ReferenceVideoPackingError> {
    if !r.is_finite() || !g.is_finite() || !b.is_finite() {
        return Err(ReferenceVideoPackingError::NonFiniteComponent);
    }
    let kg = 1.0 - kr - kb;
    let y = kr * r + kg * g + kb * b;
    let cb = (b - y) / (2.0 * (1.0 - kb)) + 0.5;
    let cr = (r - y) / (2.0 * (1.0 - kr)) + 0.5;
    Ok((y, cb, cr))
}

fn quantize_luma(value: f32, range: ReferenceOutputRange) -> u16 {
    match range {
        ReferenceOutputRange::Legal => (64.0 + value.clamp(0.0, 1.0) * 876.0).round() as u16,
        ReferenceOutputRange::Full => (value.clamp(0.0, 1.0) * 1023.0).round() as u16,
    }
}

fn quantize_chroma(value: f32, range: ReferenceOutputRange) -> u16 {
    match range {
        ReferenceOutputRange::Legal => (64.0 + value.clamp(0.0, 1.0) * 896.0).round() as u16,
        ReferenceOutputRange::Full => (value.clamp(0.0, 1.0) * 1023.0).round() as u16,
    }
}

const fn legal_black(range: ReferenceOutputRange) -> u16 {
    match range {
        ReferenceOutputRange::Legal => 64,
        ReferenceOutputRange::Full => 0,
    }
}

const fn chroma_neutral(_range: ReferenceOutputRange) -> u16 {
    512
}

const fn pack_three_10(a: u16, b: u16, c: u16) -> u32 {
    (a as u32 & 0x3ff) | ((b as u32 & 0x3ff) << 10) | ((c as u32 & 0x3ff) << 20)
}

/// Invalid clean-feed frame payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceOutputPayloadError {
    /// Signal contract is invalid.
    #[error(transparent)]
    Signal(#[from] ReferenceOutputSignalError),
    /// Audio cadence could not be represented.
    #[error(transparent)]
    AudioCadence(#[from] crate::ReferenceAudioCadenceError),
    /// Row stride is too short for the signal.
    #[error("reference output row stride {actual} is below minimum {minimum}")]
    VideoRowBytes { minimum: u32, actual: u32 },
    /// Video byte extent does not match raster and stride.
    #[error("reference output video has {actual} bytes, expected {expected}")]
    VideoByteLength { expected: usize, actual: usize },
    /// Audio extent does not match exact rational cadence.
    #[error("reference output audio has {actual} samples, expected {expected}")]
    AudioSampleLength { expected: usize, actual: usize },
    /// Signed sample exceeds 24-bit range.
    #[error("reference output audio sample {sample} exceeds signed 24-bit range")]
    AudioSampleOutOfRange { sample: i32 },
    /// Video and audio must share one frame coordinate.
    #[error("reference output bundle video frame {video} differs from audio frame {audio}")]
    BundleTimeMismatch { video: u64, audio: u64 },
    /// Video and audio were prepared against different device signals.
    #[error("reference output bundle video and audio signals differ")]
    BundleSignalMismatch,
    /// Extent arithmetic overflowed.
    #[error("reference output payload extent overflow")]
    ExtentOverflow,
}

/// Program Output video packing failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceVideoPackingError {
    /// Signal is invalid.
    #[error(transparent)]
    Signal(#[from] ReferenceOutputSignalError),
    /// The requested packer and signal disagree.
    #[error("reference output video packer does not match the signal pixel format")]
    PixelFormatMismatch,
    /// Float raster length is invalid.
    #[error("reference output RGBA input has {actual} components, expected {expected}")]
    RgbaLength { expected: usize, actual: usize },
    /// Float pixels must be finite before clamping/quantization.
    #[error("reference output RGB contains a non-finite component")]
    NonFiniteComponent,
    /// First matrix row supports the standardized HD/UHD matrices only.
    #[error("reference output v210 does not support YCbCr matrix {matrix:?}")]
    UnsupportedYuvMatrix { matrix: ColorMatrixCoefficients },
    /// Extent arithmetic overflowed.
    #[error("reference output video extent overflow")]
    ExtentOverflow,
    /// Shared payload calculation failed.
    #[error(transparent)]
    Payload(#[from] ReferenceOutputPayloadError),
}

/// Embedded-audio packing failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceAudioPackingError {
    /// Input PCM must be finite.
    #[error("reference output audio contains a non-finite sample")]
    NonFiniteSample,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReferenceHdrSignal, ReferenceOutputScan};
    use mondrian_core::{ColorSpace, Rational};

    fn v210_signal(range: ReferenceOutputRange) -> ReferenceOutputSignal {
        ReferenceOutputSignal {
            width: 6,
            height: 1,
            frame_rate: Rational::FPS_25,
            scan: ReferenceOutputScan::Progressive,
            pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
            color_space: ColorSpace::Rec709,
            range,
            hdr: None,
            audio_layout: AudioChannelLayout::Stereo,
        }
    }

    #[test]
    fn legal_black_v210_matches_reference_codes() {
        let signal = v210_signal(ReferenceOutputRange::Legal);
        let rgba = [[0.0, 0.0, 0.0, 1.0]; 6];
        let (row_bytes, packed) = pack_encoded_rgb_to_v210(&signal, &rgba).expect("v210");
        assert_eq!(row_bytes, 16);
        let words = packed
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("word")))
            .collect::<Vec<_>>();
        assert_eq!(words[0], pack_three_10(512, 64, 512));
        assert_eq!(words[1], pack_three_10(64, 512, 64));
        assert_eq!(words[2], pack_three_10(512, 64, 512));
        assert_eq!(words[3], pack_three_10(64, 512, 64));
    }

    #[test]
    fn rgb12_clamps_only_at_final_device_quantization() {
        let signal = ReferenceOutputSignal {
            width: 1,
            height: 1,
            frame_rate: Rational::FPS_25,
            scan: ReferenceOutputScan::Progressive,
            pixel_format: ReferenceOutputPixelFormat::Rgb444TwelveIn16Le,
            color_space: ColorSpace::Rec2100Pq,
            range: ReferenceOutputRange::Full,
            hdr: Some(ReferenceHdrSignal { mastering_display: None, content_light: None }),
            audio_layout: AudioChannelLayout::Stereo,
        };
        let (_, bytes) =
            pack_encoded_rgb_to_rgb12(&signal, &[[-0.5, 0.5, 1.5, 0.25]]).expect("rgb12");
        let codes = bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().expect("lane")))
            .collect::<Vec<_>>();
        assert_eq!(codes, vec![0, 2048, 4095]);
    }

    #[test]
    fn signed_24_audio_has_asymmetric_full_scale() {
        assert_eq!(
            pack_f32_audio_to_s24(&[-1.5, -1.0, 0.0, 1.0, 1.5]).expect("s24"),
            vec![-8_388_608, -8_388_608, 0, 8_388_607, 8_388_607]
        );
    }
}
