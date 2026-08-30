use mondrian_core::{
    AudioChannelLayout, ColorMatrixCoefficients, ColorSpace, Rational, VideoContentLightMetadata,
    VideoMasteringDisplayMetadata,
};
use serde::{Deserialize, Serialize};

/// Physical sample layout accepted by a Reference Output Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceOutputPixelFormat {
    /// SMPTE/QuickTime v210: packed little-endian 10-bit Y'CbCr 4:2:2.
    Yuv422TenV210,
    /// Portable RGB 4:4:4 12-bit values in little-endian 16-bit lanes.
    ///
    /// A vendor bridge lowers these lanes into its exact device-specific
    /// packed 12-bit representation after exact mode admission.
    Rgb444TwelveIn16Le,
}

impl ReferenceOutputPixelFormat {
    /// Effective component precision.
    pub const fn bit_depth(self) -> u8 {
        match self {
            Self::Yuv422TenV210 => 10,
            Self::Rgb444TwelveIn16Le => 12,
        }
    }

    /// Sampling matrix required at the Adapter boundary.
    pub fn matrix(self, color_space: ColorSpace) -> ColorMatrixCoefficients {
        match self {
            Self::Yuv422TenV210 => color_space.encoding().matrix,
            Self::Rgb444TwelveIn16Le => ColorMatrixCoefficients::Rgb,
        }
    }
}

/// Code-value range carried by the physical output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceOutputRange {
    /// SMPTE legal/narrow-range video codes.
    Legal,
    /// Full component code range.
    Full,
}

/// Exact picture sampling cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceOutputScan {
    /// One complete progressive picture per frame instant.
    Progressive,
    /// Two fields per picture, upper field first.
    InterlacedUpperFirst,
}

/// HDR signal metadata that a device must prove it can emit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceHdrSignal {
    /// Authored mastering-display metadata, when the provider can carry it.
    pub mastering_display: Option<VideoMasteringDisplayMetadata>,
    /// Authored MaxCLL/MaxFALL metadata, when the provider can carry it.
    pub content_light: Option<VideoContentLightMetadata>,
}

/// Closed video + embedded-audio signal contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputSignal {
    /// Active raster width.
    pub width: u32,
    /// Active raster height.
    pub height: u32,
    /// Picture cadence in frames per second.
    pub frame_rate: Rational,
    /// Progressive or qualified interlaced sampling.
    pub scan: ReferenceOutputScan,
    /// Host payload layout and physical precision request.
    pub pixel_format: ReferenceOutputPixelFormat,
    /// Program Output encoded color identity.
    pub color_space: ColorSpace,
    /// Physical RGB/YCbCr code range.
    pub range: ReferenceOutputRange,
    /// Optional HDR signalling and static metadata.
    pub hdr: Option<ReferenceHdrSignal>,
    /// Embedded audio is always 48 kHz; this declares channel semantics.
    pub audio_layout: AudioChannelLayout,
}

impl ReferenceOutputSignal {
    /// Validate provider-independent signal invariants.
    pub fn validate(&self) -> Result<(), ReferenceOutputSignalError> {
        if self.width == 0 || self.height == 0 {
            return Err(ReferenceOutputSignalError::EmptyRaster);
        }
        if self.frame_rate.num <= 0 || self.frame_rate.den <= 0 {
            return Err(ReferenceOutputSignalError::InvalidCadence);
        }
        if !self.color_space.is_display_referred() {
            return Err(ReferenceOutputSignalError::NonDisplayReferredColor {
                color_space: self.color_space,
            });
        }
        if self.pixel_format == ReferenceOutputPixelFormat::Yuv422TenV210
            && !self.width.is_multiple_of(2)
        {
            return Err(ReferenceOutputSignalError::OddYuv422Width { width: self.width });
        }
        if self.pixel_format == ReferenceOutputPixelFormat::Yuv422TenV210
            && self.color_space.encoding().matrix == ColorMatrixCoefficients::Rgb
        {
            return Err(ReferenceOutputSignalError::YuvMatrixUnavailable {
                color_space: self.color_space,
            });
        }
        if self.pixel_format == ReferenceOutputPixelFormat::Rgb444TwelveIn16Le
            && self.range != ReferenceOutputRange::Full
        {
            return Err(ReferenceOutputSignalError::Rgb12RequiresFullRange);
        }
        if self.hdr.is_some() && !self.color_space.is_hdr() {
            return Err(ReferenceOutputSignalError::HdrMetadataOnSdr {
                color_space: self.color_space,
            });
        }
        if self.color_space.is_hdr() && self.hdr.is_none() {
            return Err(ReferenceOutputSignalError::HdrSignalMetadataMissing {
                color_space: self.color_space,
            });
        }
        if self.audio_layout.channel_count() > 16 {
            return Err(ReferenceOutputSignalError::TooManyEmbeddedAudioChannels {
                channels: self.audio_layout.channel_count(),
            });
        }
        Ok(())
    }

    /// Expected embedded-audio sample frames for one video frame index.
    pub fn audio_frames_for_video_frame(
        &self,
        frame_index: u64,
    ) -> Result<u32, ReferenceAudioCadenceError> {
        ReferenceAudioCadence::new(self.frame_rate, 48_000)?
            .sample_frames_for_video_frame(frame_index)
    }
}

/// Provider-independent signal validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceOutputSignalError {
    /// Raster dimensions must be non-zero.
    #[error("reference output raster must be non-zero")]
    EmptyRaster,
    /// Cadence numerator and denominator must be positive.
    #[error("reference output cadence must be positive")]
    InvalidCadence,
    /// Program Output cannot be a working/log acquisition space.
    #[error("reference output color space {color_space:?} is not display-referred")]
    NonDisplayReferredColor { color_space: ColorSpace },
    /// 4:2:2 requires pairs of pixels.
    #[error("v210 output width must be even, got {width}")]
    OddYuv422Width { width: u32 },
    /// YCbCr output requires known matrix coefficients.
    #[error("reference output {color_space:?} has no YCbCr matrix")]
    YuvMatrixUnavailable { color_space: ColorSpace },
    /// The portable 12-bit RGB carrier has full-range semantics.
    #[error("12-bit RGB reference output requires full range")]
    Rgb12RequiresFullRange,
    /// HDR metadata cannot be attached to SDR colorimetry.
    #[error("HDR metadata cannot be attached to SDR {color_space:?}")]
    HdrMetadataOnSdr { color_space: ColorSpace },
    /// HDR transfer identity requires an explicit signalling record even when
    /// no static mastering fields are authored.
    #[error("HDR reference output {color_space:?} requires an explicit HDR signal record")]
    HdrSignalMetadataMissing { color_space: ColorSpace },
    /// The first product matrix is bounded to one 16-channel SDI group set.
    #[error("embedded reference audio supports at most 16 channels, got {channels}")]
    TooManyEmbeddedAudioChannels { channels: usize },
}

/// External reference-lock policy requested from a device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceOutputReferencePolicy {
    /// Device may use its internal reference.
    #[default]
    FreeRunAllowed,
    /// Start and continue only while the provider proves external lock.
    RequireExternalLock,
}

/// Ancillary-data capability required for one open Session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceOutputAncillaryPolicy {
    /// The Session must reject non-empty ancillary inventories.
    #[default]
    Disabled,
    /// Provider must prove atomic ancillary scheduling with video/audio.
    Required,
    /// Provider must additionally prove packet readback/capture evidence.
    RequiredWithReadback,
}

impl ReferenceOutputAncillaryPolicy {
    /// Whether ancillary scheduling capability is required at open.
    pub const fn is_required(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Whether the provider must expose independent packet readback evidence.
    pub const fn requires_readback(self) -> bool {
        matches!(self, Self::RequiredWithReadback)
    }
}

/// Exact mode advertised by one physical or simulated device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputMode {
    /// Closed signal accepted without implicit conversion.
    pub signal: ReferenceOutputSignal,
    /// Provider can carry an HDR transfer/colorimetry signal such as ST 352 VPID.
    pub supports_hdr_signal: bool,
    /// Provider can carry mastering-display and content-light fields.
    pub supports_static_hdr_metadata: bool,
    /// Provider can continuously report external reference lock.
    pub supports_reference_status: bool,
    /// Provider can schedule a canonical ANC inventory atomically with picture/audio.
    pub supports_ancillary: bool,
    /// Provider can read back or capture the actual scheduled packet inventory.
    pub supports_ancillary_readback: bool,
}

impl ReferenceOutputMode {
    /// Prove an exact request without provider-side format conversion.
    pub fn admits(
        &self,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<(), ReferenceOutputModeError> {
        request.signal.validate()?;
        if self.signal != request.signal {
            return Err(ReferenceOutputModeError::SignalMismatch);
        }
        if request.signal.color_space.is_hdr() && !self.supports_hdr_signal {
            return Err(ReferenceOutputModeError::HdrSignalUnsupported);
        }
        if request
            .signal
            .hdr
            .as_ref()
            .is_some_and(|hdr| hdr.mastering_display.is_some() || hdr.content_light.is_some())
            && !self.supports_static_hdr_metadata
        {
            return Err(ReferenceOutputModeError::StaticHdrMetadataUnsupported);
        }
        if request.reference_policy == ReferenceOutputReferencePolicy::RequireExternalLock
            && !self.supports_reference_status
        {
            return Err(ReferenceOutputModeError::ReferenceStatusUnsupported);
        }
        if request.ancillary_policy.is_required() && !self.supports_ancillary {
            return Err(ReferenceOutputModeError::AncillaryUnsupported);
        }
        if request.ancillary_policy.requires_readback() && !self.supports_ancillary_readback {
            return Err(ReferenceOutputModeError::AncillaryReadbackUnsupported);
        }
        Ok(())
    }
}

/// Request that becomes immutable for one open device Session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputOpenRequest {
    /// Exact signal requested from the selected device.
    pub signal: ReferenceOutputSignal,
    /// External reference-lock policy.
    pub reference_policy: ReferenceOutputReferencePolicy,
    /// Required ANC scheduling/readback evidence.
    #[serde(default)]
    pub ancillary_policy: ReferenceOutputAncillaryPolicy,
    /// Minimum video-frame preroll before playback starts.
    pub preroll_frames: u32,
    /// Bounded outstanding scheduled-frame limit.
    pub max_scheduled_frames: u32,
}

impl ReferenceOutputOpenRequest {
    /// Validate queue and signal bounds.
    pub fn validate(&self) -> Result<(), ReferenceOutputModeError> {
        self.signal.validate()?;
        if self.preroll_frames == 0 {
            return Err(ReferenceOutputModeError::ZeroPreroll);
        }
        if self.max_scheduled_frames < self.preroll_frames {
            return Err(ReferenceOutputModeError::QueueSmallerThanPreroll {
                queue: self.max_scheduled_frames,
                preroll: self.preroll_frames,
            });
        }
        if self.max_scheduled_frames > 64 {
            return Err(ReferenceOutputModeError::QueueTooLarge {
                queue: self.max_scheduled_frames,
            });
        }
        Ok(())
    }
}

/// Exact-mode admission failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceOutputModeError {
    /// Signal itself is invalid.
    #[error(transparent)]
    Signal(#[from] ReferenceOutputSignalError),
    /// Advertised mode is not byte-for-byte/field-for-field identical.
    #[error("reference output device mode does not exactly match the requested signal")]
    SignalMismatch,
    /// HDR transfer/colorimetry signalling is absent.
    #[error("reference output device cannot prove HDR signal support")]
    HdrSignalUnsupported,
    /// Complete static HDR metadata cannot be carried.
    #[error("reference output device cannot carry complete static HDR metadata")]
    StaticHdrMetadataUnsupported,
    /// External lock cannot be observed continuously.
    #[error("reference output device cannot report reference lock")]
    ReferenceStatusUnsupported,
    /// Atomic video/audio/ANC scheduling is unavailable.
    #[error("reference output device cannot schedule ancillary packets")]
    AncillaryUnsupported,
    /// Actual packet inventory cannot be independently read back or captured.
    #[error("reference output device cannot prove ancillary packet readback")]
    AncillaryReadbackUnsupported,
    /// Scheduled output requires preroll.
    #[error("reference output preroll must be greater than zero")]
    ZeroPreroll,
    /// Queue must retain the whole preroll.
    #[error("reference output queue {queue} is smaller than preroll {preroll}")]
    QueueSmallerThanPreroll { queue: u32, preroll: u32 },
    /// Product queue bound protects latency and memory.
    #[error("reference output queue {queue} exceeds the 64-frame product bound")]
    QueueTooLarge { queue: u32 },
}

/// Exact rational mapping from video frames to embedded audio sample frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceAudioCadence {
    frame_rate: Rational,
    sample_rate: u32,
}

impl ReferenceAudioCadence {
    /// Create a positive cadence.
    pub fn new(frame_rate: Rational, sample_rate: u32) -> Result<Self, ReferenceAudioCadenceError> {
        if frame_rate.num <= 0 || frame_rate.den <= 0 || sample_rate == 0 {
            return Err(ReferenceAudioCadenceError::InvalidRate);
        }
        Ok(Self { frame_rate, sample_rate })
    }

    /// Number of sample frames paired with one zero-based video frame.
    pub fn sample_frames_for_video_frame(
        self,
        frame_index: u64,
    ) -> Result<u32, ReferenceAudioCadenceError> {
        let start = self.sample_position(frame_index)?;
        let end = self.sample_position(
            frame_index.checked_add(1).ok_or(ReferenceAudioCadenceError::Overflow)?,
        )?;
        u32::try_from(end - start).map_err(|_| ReferenceAudioCadenceError::Overflow)
    }

    /// Exact floor-mapped sample position for one video-frame boundary.
    pub fn sample_position(self, frame_index: u64) -> Result<u64, ReferenceAudioCadenceError> {
        let numerator = u128::from(frame_index)
            .checked_mul(u128::from(self.sample_rate))
            .and_then(|value| value.checked_mul(self.frame_rate.den as u128))
            .ok_or(ReferenceAudioCadenceError::Overflow)?;
        let position = numerator / self.frame_rate.num as u128;
        u64::try_from(position).map_err(|_| ReferenceAudioCadenceError::Overflow)
    }
}

/// Embedded-audio cadence failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceAudioCadenceError {
    /// Rates must be positive.
    #[error("reference output audio cadence rates must be positive")]
    InvalidRate,
    /// Frame/sample coordinate exceeded representable bounds.
    #[error("reference output audio cadence overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntsc_fractional_audio_cadence_never_rounds_each_frame() {
        let cadence = ReferenceAudioCadence::new(Rational::FPS_2997, 48_000).expect("cadence");
        let counts = (0..5)
            .map(|frame| cadence.sample_frames_for_video_frame(frame).expect("count"))
            .collect::<Vec<_>>();
        assert_eq!(counts, vec![1601, 1602, 1601, 1602, 1602]);
        assert_eq!(cadence.sample_position(5).expect("position"), 8008);
    }
}
