//! Typed negotiation of one concrete realtime audio output contract.

use cpal::traits::{DeviceTrait, HostTrait};
use mondrian_core::AudioChannelLayout;

/// Scalar sample representation selected for one concrete output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RealtimeAudioSampleFormat {
    /// Signed 8-bit integer PCM.
    I8,
    /// Signed 16-bit integer PCM.
    I16,
    /// Signed 32-bit integer PCM.
    I32,
    /// Signed 64-bit integer PCM.
    I64,
    /// Unsigned 8-bit integer PCM.
    U8,
    /// Unsigned 16-bit integer PCM.
    U16,
    /// Unsigned 32-bit integer PCM.
    U32,
    /// Unsigned 64-bit integer PCM.
    U64,
    /// 32-bit floating-point PCM.
    F32,
    /// 64-bit floating-point PCM.
    F64,
}

impl RealtimeAudioSampleFormat {
    fn from_cpal(value: cpal::SampleFormat) -> Option<Self> {
        match value {
            cpal::SampleFormat::I8 => Some(Self::I8),
            cpal::SampleFormat::I16 => Some(Self::I16),
            cpal::SampleFormat::I32 => Some(Self::I32),
            cpal::SampleFormat::I64 => Some(Self::I64),
            cpal::SampleFormat::U8 => Some(Self::U8),
            cpal::SampleFormat::U16 => Some(Self::U16),
            cpal::SampleFormat::U32 => Some(Self::U32),
            cpal::SampleFormat::U64 => Some(Self::U64),
            cpal::SampleFormat::F32 => Some(Self::F32),
            cpal::SampleFormat::F64 => Some(Self::F64),
            _ => None,
        }
    }

    const fn preference(self) -> u8 {
        match self {
            Self::F32 => 0,
            Self::F64 => 1,
            Self::I32 => 2,
            Self::I16 => 3,
            Self::I64 => 4,
            Self::U32 => 5,
            Self::U16 => 6,
            Self::U64 => 7,
            Self::I8 => 8,
            Self::U8 => 9,
        }
    }
}

/// Why CPAL's channel-count-only contract is sufficient for one target layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RealtimeAudioChannelSemantics {
    /// A one-channel device uses Mondrian's versioned layout-independent Mono convention.
    MonoConvention,
    /// A two-channel device uses Mondrian's versioned front-left/front-right convention.
    StereoConvention,
    /// The target is explicitly ordinal and claims no speaker positions.
    OrdinalDiscrete,
}

/// Buffer extent advertised by the selected CPAL configuration range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RealtimeAudioSupportedBufferSize {
    /// The backend cannot expose a supported range before stream creation.
    Unknown,
    /// Inclusive frame-count range advertised by the backend.
    Range { min_frames: u32, max_frames: u32 },
}

/// Counts proving how one selected candidate was narrowed from device evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RealtimeAudioCandidateCounts {
    /// All output configuration ranges returned by the device.
    pub enumerated: u32,
    /// Ranges with the exact requested channel count.
    pub matching_channels: u32,
    /// Channel-compatible ranges containing the exact requested sample rate.
    pub matching_sample_rate: u32,
    /// Rate-compatible ranges whose scalar format Mondrian can execute.
    pub executable: u32,
}

/// Immutable physical output contract selected before stream creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RealtimeAudioOutputContract {
    /// Exact requested and selected sample rate.
    pub sample_rate: u32,
    /// Exact semantic target layout. Channel count is derived from this value.
    pub channel_layout: AudioChannelLayout,
    /// Concrete callback scalar representation.
    pub sample_format: RealtimeAudioSampleFormat,
    /// Proof used to interpret CPAL's channel-count-only candidate.
    pub channel_semantics: RealtimeAudioChannelSemantics,
    /// Supported buffer range behind the selected default-buffer stream.
    pub supported_buffer_size: RealtimeAudioSupportedBufferSize,
    /// Candidate narrowing evidence from the same device enumeration.
    pub candidates: RealtimeAudioCandidateCounts,
}

impl RealtimeAudioOutputContract {
    /// Exact interleaved channel extent derived from the semantic layout.
    pub const fn channels(self) -> u8 {
        self.channel_layout.channel_count_u8()
    }
}

/// Low-frequency identity and configuration evidence for one selected device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeAudioOutputDeviceEvidence {
    /// CPAL host Adapter selected on this platform.
    pub host_name: String,
    /// Human-readable default-device name, when the backend could provide it.
    pub device_name: Option<String>,
    /// Name-query failure retained without blocking otherwise valid playback.
    pub device_name_error: Option<String>,
    /// Exact selected stream contract.
    pub contract: RealtimeAudioOutputContract,
}

/// Stable rejection taxonomy for default-device open and negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RealtimeAudioOutputOpenFailureCode {
    /// The selected host currently exposes no default output device.
    NoDefaultDevice,
    /// The backend could not enumerate supported output configurations.
    ConfigurationEnumerationFailed,
    /// Channel count alone cannot prove the requested semantic layout.
    ChannelSemanticsUnproven,
    /// No configuration exposes the requested channel extent.
    ChannelCountUnsupported,
    /// No channel-compatible configuration contains the requested rate.
    SampleRateUnsupported,
    /// No exact channel/rate candidate uses an executable scalar format.
    SampleFormatUnsupported,
    /// The bounded realtime PCM queue extent cannot be represented.
    QueueCapacityOverflow,
    /// Every non-zero physical stream-generation identity was issued.
    StreamGenerationExhausted,
    /// The backend rejected creation of the selected stream contract.
    StreamBuildFailed,
    /// The backend created but could not start the selected stream.
    StreamStartFailed,
}

/// Structured failure for one concrete default-output open attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("realtime audio output open failed ({code:?}): {detail}")]
pub struct RealtimeAudioOutputOpenFailure {
    /// Stable machine-readable rejection code.
    pub code: RealtimeAudioOutputOpenFailureCode,
    /// Exact requested sample rate.
    pub requested_sample_rate: u32,
    /// Exact requested semantic target layout.
    pub requested_layout: AudioChannelLayout,
    /// Candidate counts available when failure occurred.
    pub candidates: RealtimeAudioCandidateCounts,
    /// Selected contract when discovery succeeded but stream creation failed.
    pub selected_contract: Option<RealtimeAudioOutputContract>,
    /// Human-readable backend or policy detail.
    pub detail: String,
}

impl RealtimeAudioOutputOpenFailure {
    pub(crate) fn before_selection(
        code: RealtimeAudioOutputOpenFailureCode,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        candidates: RealtimeAudioCandidateCounts,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            requested_sample_rate: sample_rate,
            requested_layout: channel_layout,
            candidates,
            selected_contract: None,
            detail: detail.into(),
        }
    }

    pub(crate) fn after_selection(
        code: RealtimeAudioOutputOpenFailureCode,
        contract: RealtimeAudioOutputContract,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            requested_sample_rate: contract.sample_rate,
            requested_layout: contract.channel_layout,
            candidates: contract.candidates,
            selected_contract: Some(contract),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct AudioOutputCandidate {
    index: usize,
    channels: u16,
    min_sample_rate: u32,
    max_sample_rate: u32,
    sample_format: Option<RealtimeAudioSampleFormat>,
    supported_buffer_size: RealtimeAudioSupportedBufferSize,
}

#[derive(Debug)]
struct SelectedAudioOutputCandidate {
    index: usize,
    contract: RealtimeAudioOutputContract,
}

pub(crate) struct PreparedRealtimeAudioOutputDevice {
    pub(crate) device: cpal::Device,
    pub(crate) config: cpal::StreamConfig,
    pub(crate) sample_format: cpal::SampleFormat,
    pub(crate) evidence: RealtimeAudioOutputDeviceEvidence,
}

/// Discover the current default device and select one exact executable contract.
pub(crate) fn prepare_default_realtime_audio_output(
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
) -> Result<PreparedRealtimeAudioOutputDevice, RealtimeAudioOutputOpenFailure> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_owned();
    let device = host.default_output_device().ok_or_else(|| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::NoDefaultDevice,
            sample_rate,
            channel_layout,
            RealtimeAudioCandidateCounts::default(),
            "the selected CPAL host reported no default output device",
        )
    })?;
    let (device_name, device_name_error) = match device.name() {
        Ok(name) => (Some(name), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let ranges = device.supported_output_configs().map_err(|error| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::ConfigurationEnumerationFailed,
            sample_rate,
            channel_layout,
            RealtimeAudioCandidateCounts::default(),
            error.to_string(),
        )
    })?;
    let ranges = ranges.collect::<Vec<_>>();
    let candidates = ranges
        .iter()
        .enumerate()
        .map(|(index, range)| AudioOutputCandidate {
            index,
            channels: range.channels(),
            min_sample_rate: range.min_sample_rate().0,
            max_sample_rate: range.max_sample_rate().0,
            sample_format: RealtimeAudioSampleFormat::from_cpal(range.sample_format()),
            supported_buffer_size: supported_buffer_size(*range.buffer_size()),
        })
        .collect::<Vec<_>>();
    let selected = select_audio_output_candidate(sample_rate, channel_layout, &candidates)?;
    let range = ranges.get(selected.index).ok_or_else(|| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::ConfigurationEnumerationFailed,
            sample_rate,
            channel_layout,
            selected.contract.candidates,
            "selected CPAL output candidate disappeared before stream configuration",
        )
    })?;
    let supported = range.try_with_sample_rate(cpal::SampleRate(sample_rate)).ok_or_else(|| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::SampleRateUnsupported,
            sample_rate,
            channel_layout,
            selected.contract.candidates,
            "selected CPAL range no longer contains the requested sample rate",
        )
    })?;
    Ok(PreparedRealtimeAudioOutputDevice {
        device,
        config: supported.config(),
        sample_format: supported.sample_format(),
        evidence: RealtimeAudioOutputDeviceEvidence {
            host_name,
            device_name,
            device_name_error,
            contract: selected.contract,
        },
    })
}

fn select_audio_output_candidate(
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    candidates: &[AudioOutputCandidate],
) -> Result<SelectedAudioOutputCandidate, RealtimeAudioOutputOpenFailure> {
    let mut counts = RealtimeAudioCandidateCounts {
        enumerated: bounded_count(candidates.len()),
        ..RealtimeAudioCandidateCounts::default()
    };
    let semantics = prove_channel_semantics(channel_layout).map_err(|detail| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::ChannelSemanticsUnproven,
            sample_rate,
            channel_layout,
            counts,
            detail,
        )
    })?;
    let requested_channels = u16::from(channel_layout.channel_count_u8());
    let mut executable = Vec::new();
    for candidate in candidates {
        if candidate.channels != requested_channels {
            continue;
        }
        counts.matching_channels = counts.matching_channels.saturating_add(1);
        if sample_rate < candidate.min_sample_rate || sample_rate > candidate.max_sample_rate {
            continue;
        }
        counts.matching_sample_rate = counts.matching_sample_rate.saturating_add(1);
        let Some(sample_format) = candidate.sample_format else {
            continue;
        };
        counts.executable = counts.executable.saturating_add(1);
        executable.push((*candidate, sample_format));
    }
    if counts.matching_channels == 0 {
        return Err(RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::ChannelCountUnsupported,
            sample_rate,
            channel_layout,
            counts,
            format!("device exposes no {requested_channels}-channel output configuration"),
        ));
    }
    if counts.matching_sample_rate == 0 {
        return Err(RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::SampleRateUnsupported,
            sample_rate,
            channel_layout,
            counts,
            format!("no {requested_channels}-channel range contains {sample_rate} Hz"),
        ));
    }
    let Some((candidate, sample_format)) = executable
        .into_iter()
        .min_by_key(|(candidate, format)| (format.preference(), candidate.index))
    else {
        return Err(RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::SampleFormatUnsupported,
            sample_rate,
            channel_layout,
            counts,
            "all exact channel/rate candidates use unsupported scalar sample formats",
        ));
    };
    Ok(SelectedAudioOutputCandidate {
        index: candidate.index,
        contract: RealtimeAudioOutputContract {
            sample_rate,
            channel_layout,
            sample_format,
            channel_semantics: semantics,
            supported_buffer_size: candidate.supported_buffer_size,
            candidates: counts,
        },
    })
}

fn prove_channel_semantics(
    channel_layout: AudioChannelLayout,
) -> Result<RealtimeAudioChannelSemantics, &'static str> {
    match channel_layout {
        AudioChannelLayout::Mono => Ok(RealtimeAudioChannelSemantics::MonoConvention),
        AudioChannelLayout::Stereo => Ok(RealtimeAudioChannelSemantics::StereoConvention),
        AudioChannelLayout::Discrete(_) => Ok(RealtimeAudioChannelSemantics::OrdinalDiscrete),
        AudioChannelLayout::Speakers(_) => Err(
            "CPAL reports only channel count; this named speaker layout needs a platform Adapter that proves channel positions",
        ),
    }
}

const fn supported_buffer_size(
    value: cpal::SupportedBufferSize,
) -> RealtimeAudioSupportedBufferSize {
    match value {
        cpal::SupportedBufferSize::Unknown => RealtimeAudioSupportedBufferSize::Unknown,
        cpal::SupportedBufferSize::Range { min, max } => {
            RealtimeAudioSupportedBufferSize::Range { min_frames: min, max_frames: max }
        }
    }
}

fn bounded_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        index: usize,
        channels: u16,
        min_sample_rate: u32,
        max_sample_rate: u32,
        sample_format: Option<RealtimeAudioSampleFormat>,
    ) -> AudioOutputCandidate {
        AudioOutputCandidate {
            index,
            channels,
            min_sample_rate,
            max_sample_rate,
            sample_format,
            supported_buffer_size: RealtimeAudioSupportedBufferSize::Range {
                min_frames: 64,
                max_frames: 2_048,
            },
        }
    }

    #[test]
    fn exact_candidate_selection_prefers_float_and_retains_narrowing_evidence() {
        let candidates = [
            candidate(0, 2, 44_100, 48_000, Some(RealtimeAudioSampleFormat::I16)),
            candidate(1, 1, 48_000, 48_000, Some(RealtimeAudioSampleFormat::F32)),
            candidate(2, 2, 48_000, 96_000, Some(RealtimeAudioSampleFormat::F32)),
        ];
        let selected =
            select_audio_output_candidate(48_000, AudioChannelLayout::Stereo, &candidates)
                .expect("exact stereo candidate");
        assert_eq!(selected.index, 2);
        assert_eq!(
            selected.contract.sample_format,
            RealtimeAudioSampleFormat::F32
        );
        assert_eq!(
            selected.contract.candidates,
            RealtimeAudioCandidateCounts {
                enumerated: 3,
                matching_channels: 2,
                matching_sample_rate: 2,
                executable: 2,
            }
        );
    }

    #[test]
    fn selection_distinguishes_channel_rate_and_format_rejections() {
        let wrong_channels = [candidate(
            0,
            1,
            48_000,
            48_000,
            Some(RealtimeAudioSampleFormat::F32),
        )];
        let error =
            select_audio_output_candidate(48_000, AudioChannelLayout::Stereo, &wrong_channels)
                .expect_err("channel count must fail");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::ChannelCountUnsupported
        );

        let wrong_rate = [candidate(
            0,
            2,
            44_100,
            44_100,
            Some(RealtimeAudioSampleFormat::F32),
        )];
        let error = select_audio_output_candidate(48_000, AudioChannelLayout::Stereo, &wrong_rate)
            .expect_err("sample rate must fail");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::SampleRateUnsupported
        );

        let unknown_format = [candidate(0, 2, 48_000, 48_000, None)];
        let error =
            select_audio_output_candidate(48_000, AudioChannelLayout::Stereo, &unknown_format)
                .expect_err("sample format must fail");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::SampleFormatUnsupported
        );
    }

    #[test]
    fn named_surround_is_rejected_when_only_channel_count_is_known() {
        let candidates = [candidate(
            0,
            6,
            48_000,
            48_000,
            Some(RealtimeAudioSampleFormat::F32),
        )];
        let error =
            select_audio_output_candidate(48_000, AudioChannelLayout::Surround51Side, &candidates)
                .expect_err("CPAL cannot prove speaker positions");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::ChannelSemanticsUnproven
        );
    }

    #[test]
    fn discrete_layout_preserves_explicit_ordinal_semantics() {
        let layout = AudioChannelLayout::discrete(8).expect("discrete layout");
        let candidates = [candidate(
            0,
            8,
            48_000,
            48_000,
            Some(RealtimeAudioSampleFormat::I32),
        )];
        let selected = select_audio_output_candidate(48_000, layout, &candidates)
            .expect("ordinal device contract");
        assert_eq!(
            selected.contract.channel_semantics,
            RealtimeAudioChannelSemantics::OrdinalDiscrete
        );
    }
}
