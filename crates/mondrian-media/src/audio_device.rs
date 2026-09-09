//! Typed negotiation of one concrete realtime audio output contract.

use cpal::traits::{DeviceTrait, HostTrait};
use mondrian_core::AudioChannelLayout;
#[cfg(target_os = "windows")]
use mondrian_core::AudioChannelPosition;
use serde::{Deserialize, Deserializer, Serialize};
use std::str::FromStr;

const MAX_SERIALIZED_AUDIO_DEVICE_ID_BYTES: usize = 4_096;

/// Stable cross-process identity of one physical or virtual audio device.
///
/// The value is an opaque CPAL `host:backend-id` string. It belongs to user
/// preferences and runtime selection, never to Project or Sequence authoring.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RealtimeAudioOutputDeviceId(String);

impl RealtimeAudioOutputDeviceId {
    /// Validate one serialized CPAL device identity without requiring that the
    /// originating host or device be present on this machine.
    pub fn new(value: impl Into<String>) -> Result<Self, RealtimeAudioOutputDeviceIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RealtimeAudioOutputDeviceIdError::Empty);
        }
        if value.len() > MAX_SERIALIZED_AUDIO_DEVICE_ID_BYTES {
            return Err(RealtimeAudioOutputDeviceIdError::TooLong);
        }
        let Some((host, device)) = value.split_once(':') else {
            return Err(RealtimeAudioOutputDeviceIdError::MissingHostSeparator);
        };
        if host.is_empty() || device.is_empty() {
            return Err(RealtimeAudioOutputDeviceIdError::EmptyComponent);
        }
        if value.chars().any(char::is_control) {
            return Err(RealtimeAudioOutputDeviceIdError::ControlCharacter);
        }
        Ok(Self(value))
    }

    /// Borrow the opaque serialized identity.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    fn from_cpal(value: cpal::DeviceId) -> Result<Self, RealtimeAudioOutputDeviceIdError> {
        Self::new(value.to_string())
    }

    fn to_cpal(&self) -> Result<cpal::DeviceId, cpal::Error> {
        cpal::DeviceId::from_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RealtimeAudioOutputDeviceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Invalid serialized audio-device identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RealtimeAudioOutputDeviceIdError {
    /// The serialized value is empty.
    #[error("audio output device identity is empty")]
    Empty,
    /// The value exceeds the bounded preference payload.
    #[error("audio output device identity exceeds the supported length")]
    TooLong,
    /// The stable identity must contain CPAL's host separator.
    #[error("audio output device identity has no host separator")]
    MissingHostSeparator,
    /// Either the host or backend-specific identity is empty.
    #[error("audio output device identity contains an empty component")]
    EmptyComponent,
    /// Control characters are never valid preference payload.
    #[error("audio output device identity contains a control character")]
    ControlCharacter,
}

/// User/runtime intent for selecting a realtime output device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RealtimeAudioOutputDeviceSelection {
    /// Follow the operating system's current default output device.
    #[default]
    SystemDefault,
    /// Reopen only the exact stable device identity; never silently fall back.
    Specific {
        device_id: RealtimeAudioOutputDeviceId,
    },
}

/// One output device visible to the current default CPAL host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeAudioOutputDeviceDescriptor {
    /// Stable selectable identity, absent only when the backend identity query failed.
    pub device_id: Option<RealtimeAudioOutputDeviceId>,
    /// User-facing backend description.
    pub display_name: String,
    /// Whether this exact device is the current system default.
    pub is_system_default: bool,
    /// Identity-query failure retained for an unselectable catalog row.
    pub device_id_error: Option<String>,
    /// Structured-description failure; `display_name` still contains CPAL's fallback text.
    pub description_error: Option<String>,
}

/// Immutable output-device catalog from one concrete CPAL host observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeAudioOutputDeviceCatalog {
    /// CPAL host Adapter used for enumeration.
    pub host_name: String,
    /// Output-capable devices in backend enumeration order.
    pub devices: Vec<RealtimeAudioOutputDeviceDescriptor>,
}

/// Failure to enumerate the current host's output-device catalog.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("realtime audio output discovery failed on {host_name}: {detail}")]
pub struct RealtimeAudioOutputDiscoveryFailure {
    /// CPAL host Adapter used for this attempt.
    pub host_name: String,
    /// Backend failure detail.
    pub detail: String,
}

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

/// Proof that gives one physical channel extent its exact signal semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RealtimeAudioChannelSemantics {
    /// A one-channel device uses Mondrian's versioned layout-independent Mono convention.
    MonoConvention,
    /// A two-channel device uses Mondrian's versioned front-left/front-right convention.
    StereoConvention,
    /// The target is explicitly ordinal and claims no speaker positions.
    OrdinalDiscrete,
    /// Windows WASAPI initialized the stream with the exact mask derived from
    /// the contract's named [`AudioChannelLayout`].
    WindowsWasapiSpeakerMask,
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
    /// Proof used to interpret the selected physical stream's channels.
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
    /// Stable identity used to reopen this exact device.
    pub device_id: RealtimeAudioOutputDeviceId,
    /// Selection intent resolved by this open attempt.
    pub selection: RealtimeAudioOutputDeviceSelection,
    /// Whether the selected device was the system default at open time.
    pub was_system_default: bool,
    /// Human-readable device name, when the backend could provide it.
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
    /// The requested stable identity is malformed or belongs to another unavailable host.
    InvalidDeviceId,
    /// A specific device identity is well formed but not currently available.
    RequestedDeviceUnavailable,
    /// The selected device could not provide the stable identity required for safe reopen.
    DeviceIdentityUnavailable,
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

pub(crate) enum PreparedRealtimeAudioOutputBackend {
    Cpal {
        device: cpal::Device,
        config: cpal::StreamConfig,
        sample_format: cpal::SampleFormat,
    },
    #[cfg(target_os = "windows")]
    WindowsWasapiNamed {
        endpoint_id: String,
        channel_mask: u32,
    },
}

pub(crate) struct PreparedRealtimeAudioOutputDevice {
    pub(crate) backend: PreparedRealtimeAudioOutputBackend,
    pub(crate) evidence: RealtimeAudioOutputDeviceEvidence,
}

/// Enumerate output-capable devices on the current default host.
///
/// This may call platform audio APIs and should run on a domain-owned worker,
/// never inside the realtime callback or UI event handler.
pub fn discover_realtime_audio_output_devices(
) -> Result<RealtimeAudioOutputDeviceCatalog, RealtimeAudioOutputDiscoveryFailure> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_owned();
    let default = host.default_output_device();
    let devices = host
        .output_devices()
        .map_err(|error| RealtimeAudioOutputDiscoveryFailure {
            host_name: host_name.clone(),
            detail: error.to_string(),
        })?
        .map(|device| {
            let is_system_default = default.as_ref().is_some_and(|default| default == &device);
            let fallback_name = device.to_string();
            let (display_name, description_error) = match device.description() {
                Ok(description) => (description.name().to_owned(), None),
                Err(error) => (fallback_name, Some(error.to_string())),
            };
            let (device_id, device_id_error) = match device.id() {
                Ok(device_id) => match RealtimeAudioOutputDeviceId::from_cpal(device_id) {
                    Ok(device_id) => (Some(device_id), None),
                    Err(error) => (None, Some(error.to_string())),
                },
                Err(error) => (None, Some(error.to_string())),
            };
            RealtimeAudioOutputDeviceDescriptor {
                device_id,
                display_name,
                is_system_default,
                device_id_error,
                description_error,
            }
        })
        .collect();
    Ok(RealtimeAudioOutputDeviceCatalog { host_name, devices })
}

pub(crate) fn current_default_realtime_audio_output_device_id(
) -> Result<Option<RealtimeAudioOutputDeviceId>, String> {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        return Ok(None);
    };
    let id = device.id().map_err(|error| error.to_string())?;
    RealtimeAudioOutputDeviceId::from_cpal(id)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Resolve one device intent and select an exact executable stream contract.
/// Inspect the exact production output negotiation without creating a stream.
///
/// This uses the same device selection, rate, scalar-format and channel-semantics
/// authority as Playback. The returned evidence is a preflight observation;
/// stream creation must negotiate again and prove the live device identity.
pub fn probe_realtime_audio_output_contract(
    selection: &RealtimeAudioOutputDeviceSelection,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
) -> Result<RealtimeAudioOutputDeviceEvidence, RealtimeAudioOutputOpenFailure> {
    let prepared = prepare_realtime_audio_output(selection, sample_rate, channel_layout)?;
    Ok(prepared.evidence)
}

pub(crate) fn prepare_realtime_audio_output(
    selection: &RealtimeAudioOutputDeviceSelection,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
) -> Result<PreparedRealtimeAudioOutputDevice, RealtimeAudioOutputOpenFailure> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_owned();
    let default = host.default_output_device();
    let device = match selection {
        RealtimeAudioOutputDeviceSelection::SystemDefault => default.clone().ok_or_else(|| {
            RealtimeAudioOutputOpenFailure::before_selection(
                RealtimeAudioOutputOpenFailureCode::NoDefaultDevice,
                sample_rate,
                channel_layout,
                RealtimeAudioCandidateCounts::default(),
                "the selected CPAL host reported no default output device",
            )
        })?,
        RealtimeAudioOutputDeviceSelection::Specific { device_id } => {
            let parsed = device_id.to_cpal().map_err(|error| {
                RealtimeAudioOutputOpenFailure::before_selection(
                    RealtimeAudioOutputOpenFailureCode::InvalidDeviceId,
                    sample_rate,
                    channel_layout,
                    RealtimeAudioCandidateCounts::default(),
                    error.to_string(),
                )
            })?;
            host.device_by_id(&parsed).ok_or_else(|| {
                RealtimeAudioOutputOpenFailure::before_selection(
                    RealtimeAudioOutputOpenFailureCode::RequestedDeviceUnavailable,
                    sample_rate,
                    channel_layout,
                    RealtimeAudioCandidateCounts::default(),
                    format!(
                        "requested audio output device {} is unavailable",
                        device_id.as_str()
                    ),
                )
            })?
        }
    };
    let was_system_default = default.as_ref().is_some_and(|default| default == &device);
    let cpal_device_id = device.id().map_err(|error| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::DeviceIdentityUnavailable,
            sample_rate,
            channel_layout,
            RealtimeAudioCandidateCounts::default(),
            error.to_string(),
        )
    })?;
    #[cfg(target_os = "windows")]
    let backend_device_id = cpal_device_id.id().to_owned();
    let device_id = RealtimeAudioOutputDeviceId::from_cpal(cpal_device_id).map_err(|error| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::DeviceIdentityUnavailable,
            sample_rate,
            channel_layout,
            RealtimeAudioCandidateCounts::default(),
            error.to_string(),
        )
    })?;
    let (device_name, device_name_error) = match device.description() {
        Ok(description) => (Some(description.name().to_owned()), None),
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
            min_sample_rate: range.min_sample_rate(),
            max_sample_rate: range.max_sample_rate(),
            sample_format: RealtimeAudioSampleFormat::from_cpal(range.sample_format()),
            supported_buffer_size: supported_buffer_size(*range.buffer_size()),
        })
        .collect::<Vec<_>>();
    let channel_semantics =
        prove_channel_semantics(channel_layout, host.id()).map_err(|detail| {
            RealtimeAudioOutputOpenFailure::before_selection(
                RealtimeAudioOutputOpenFailureCode::ChannelSemanticsUnproven,
                sample_rate,
                channel_layout,
                RealtimeAudioCandidateCounts::default(),
                detail,
            )
        })?;
    let selected =
        select_audio_output_candidate(sample_rate, channel_layout, channel_semantics, &candidates)?;
    let range = ranges.get(selected.index).ok_or_else(|| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::ConfigurationEnumerationFailed,
            sample_rate,
            channel_layout,
            selected.contract.candidates,
            "selected CPAL output candidate disappeared before stream configuration",
        )
    })?;
    let supported = range.try_with_sample_rate(sample_rate).ok_or_else(|| {
        RealtimeAudioOutputOpenFailure::before_selection(
            RealtimeAudioOutputOpenFailureCode::SampleRateUnsupported,
            sample_rate,
            channel_layout,
            selected.contract.candidates,
            "selected CPAL range no longer contains the requested sample rate",
        )
    })?;
    let backend = match channel_semantics {
        #[cfg(target_os = "windows")]
        RealtimeAudioChannelSemantics::WindowsWasapiSpeakerMask => {
            let channel_mask = windows_speaker_mask(channel_layout).ok_or_else(|| {
                RealtimeAudioOutputOpenFailure::before_selection(
                    RealtimeAudioOutputOpenFailureCode::ChannelSemanticsUnproven,
                    sample_rate,
                    channel_layout,
                    selected.contract.candidates,
                    "the proven Windows speaker mask disappeared before stream preparation",
                )
            })?;
            PreparedRealtimeAudioOutputBackend::WindowsWasapiNamed {
                endpoint_id: backend_device_id,
                channel_mask,
            }
        }
        _ => PreparedRealtimeAudioOutputBackend::Cpal {
            device,
            config: supported.config(),
            sample_format: supported.sample_format(),
        },
    };
    Ok(PreparedRealtimeAudioOutputDevice {
        backend,
        evidence: RealtimeAudioOutputDeviceEvidence {
            host_name,
            device_id,
            selection: selection.clone(),
            was_system_default,
            device_name,
            device_name_error,
            contract: selected.contract,
        },
    })
}

fn select_audio_output_candidate(
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    channel_semantics: RealtimeAudioChannelSemantics,
    candidates: &[AudioOutputCandidate],
) -> Result<SelectedAudioOutputCandidate, RealtimeAudioOutputOpenFailure> {
    let mut counts = RealtimeAudioCandidateCounts {
        enumerated: bounded_count(candidates.len()),
        ..RealtimeAudioCandidateCounts::default()
    };
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
        if matches!(
            channel_semantics,
            RealtimeAudioChannelSemantics::WindowsWasapiSpeakerMask
        ) && sample_format != RealtimeAudioSampleFormat::F32
        {
            continue;
        }
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
            channel_semantics,
            supported_buffer_size: candidate.supported_buffer_size,
            candidates: counts,
        },
    })
}

fn prove_channel_semantics(
    channel_layout: AudioChannelLayout,
    host_id: cpal::HostId,
) -> Result<RealtimeAudioChannelSemantics, &'static str> {
    match channel_layout {
        AudioChannelLayout::Mono => Ok(RealtimeAudioChannelSemantics::MonoConvention),
        AudioChannelLayout::Stereo => Ok(RealtimeAudioChannelSemantics::StereoConvention),
        AudioChannelLayout::Discrete(_) => Ok(RealtimeAudioChannelSemantics::OrdinalDiscrete),
        AudioChannelLayout::Speakers(_) => named_speaker_semantics(channel_layout, host_id),
    }
}

#[cfg(target_os = "windows")]
fn named_speaker_semantics(
    channel_layout: AudioChannelLayout,
    host_id: cpal::HostId,
) -> Result<RealtimeAudioChannelSemantics, &'static str> {
    if host_id != cpal::HostId::Wasapi {
        return Err("named speaker output requires the Windows WASAPI Adapter");
    }
    if windows_speaker_mask(channel_layout).is_none() {
        return Err(
            "the named layout contains positions that WAVEFORMATEXTENSIBLE cannot represent",
        );
    }
    Ok(RealtimeAudioChannelSemantics::WindowsWasapiSpeakerMask)
}

#[cfg(not(target_os = "windows"))]
fn named_speaker_semantics(
    _channel_layout: AudioChannelLayout,
    _host_id: cpal::HostId,
) -> Result<RealtimeAudioChannelSemantics, &'static str> {
    Err(
        "CPAL reports only channel count; this named speaker layout needs a platform Adapter that proves channel positions",
    )
}

#[cfg(target_os = "windows")]
fn windows_speaker_mask(channel_layout: AudioChannelLayout) -> Option<u32> {
    let speakers = channel_layout.speaker_set()?;
    speakers.positions().try_fold(0_u32, |mask, position| {
        windows_speaker_position_bit(position).map(|bit| mask | bit)
    })
}

#[cfg(target_os = "windows")]
const fn windows_speaker_position_bit(position: AudioChannelPosition) -> Option<u32> {
    match position {
        AudioChannelPosition::FrontLeft => Some(1 << 0),
        AudioChannelPosition::FrontRight => Some(1 << 1),
        AudioChannelPosition::FrontCenter => Some(1 << 2),
        AudioChannelPosition::LowFrequencyEffects => Some(1 << 3),
        AudioChannelPosition::BackLeft => Some(1 << 4),
        AudioChannelPosition::BackRight => Some(1 << 5),
        AudioChannelPosition::FrontLeftOfCenter => Some(1 << 6),
        AudioChannelPosition::FrontRightOfCenter => Some(1 << 7),
        AudioChannelPosition::BackCenter => Some(1 << 8),
        AudioChannelPosition::SideLeft => Some(1 << 9),
        AudioChannelPosition::SideRight => Some(1 << 10),
        AudioChannelPosition::TopCenter => Some(1 << 11),
        AudioChannelPosition::TopFrontLeft => Some(1 << 12),
        AudioChannelPosition::TopFrontCenter => Some(1 << 13),
        AudioChannelPosition::TopFrontRight => Some(1 << 14),
        AudioChannelPosition::TopBackLeft => Some(1 << 15),
        AudioChannelPosition::TopBackCenter => Some(1 << 16),
        AudioChannelPosition::TopBackRight => Some(1 << 17),
        AudioChannelPosition::Mono
        | AudioChannelPosition::WideLeft
        | AudioChannelPosition::WideRight
        | AudioChannelPosition::TopSideLeft
        | AudioChannelPosition::TopSideRight
        | AudioChannelPosition::LowFrequencyEffects2 => None,
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

    #[test]
    fn stable_device_identity_roundtrips_without_requiring_local_hardware() {
        let id = RealtimeAudioOutputDeviceId::new("wasapi:{device-guid}")
            .expect("well-formed opaque identity");
        let encoded = serde_json::to_string(&id).expect("serialize identity");
        let decoded: RealtimeAudioOutputDeviceId =
            serde_json::from_str(&encoded).expect("deserialize identity");
        assert_eq!(decoded, id);
        assert_eq!(decoded.as_str(), "wasapi:{device-guid}");
    }

    #[test]
    fn device_identity_rejects_ambiguous_or_unbounded_preference_payloads() {
        assert_eq!(
            RealtimeAudioOutputDeviceId::new("device-without-host").expect_err("missing host"),
            RealtimeAudioOutputDeviceIdError::MissingHostSeparator
        );
        assert_eq!(
            RealtimeAudioOutputDeviceId::new("wasapi:").expect_err("missing backend id"),
            RealtimeAudioOutputDeviceIdError::EmptyComponent
        );
        assert_eq!(
            RealtimeAudioOutputDeviceId::new("wasapi:device\nname").expect_err("control character"),
            RealtimeAudioOutputDeviceIdError::ControlCharacter
        );
    }

    #[test]
    fn device_selection_serialization_preserves_default_and_specific_intent() {
        let selections = [
            RealtimeAudioOutputDeviceSelection::SystemDefault,
            RealtimeAudioOutputDeviceSelection::Specific {
                device_id: RealtimeAudioOutputDeviceId::new("coreaudio:42")
                    .expect("specific identity"),
            },
        ];
        for selection in selections {
            let encoded = serde_json::to_vec(&selection).expect("serialize selection");
            let decoded: RealtimeAudioOutputDeviceSelection =
                serde_json::from_slice(&encoded).expect("deserialize selection");
            assert_eq!(decoded, selection);
        }
    }

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
        let selected = select_audio_output_candidate(
            48_000,
            AudioChannelLayout::Stereo,
            RealtimeAudioChannelSemantics::StereoConvention,
            &candidates,
        )
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
        let error = select_audio_output_candidate(
            48_000,
            AudioChannelLayout::Stereo,
            RealtimeAudioChannelSemantics::StereoConvention,
            &wrong_channels,
        )
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
        let error = select_audio_output_candidate(
            48_000,
            AudioChannelLayout::Stereo,
            RealtimeAudioChannelSemantics::StereoConvention,
            &wrong_rate,
        )
        .expect_err("sample rate must fail");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::SampleRateUnsupported
        );

        let unknown_format = [candidate(0, 2, 48_000, 48_000, None)];
        let error = select_audio_output_candidate(
            48_000,
            AudioChannelLayout::Stereo,
            RealtimeAudioChannelSemantics::StereoConvention,
            &unknown_format,
        )
        .expect_err("sample format must fail");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::SampleFormatUnsupported
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn named_surround_requires_explicit_semantics_and_float_execution() {
        let candidates = [candidate(
            0,
            6,
            48_000,
            48_000,
            Some(RealtimeAudioSampleFormat::F32),
        )];
        let semantics = RealtimeAudioChannelSemantics::WindowsWasapiSpeakerMask;
        let selected = select_audio_output_candidate(
            48_000,
            AudioChannelLayout::Surround51Side,
            semantics,
            &candidates,
        )
        .expect("exact named output candidate");
        assert_eq!(selected.contract.channel_semantics, semantics);

        let integer_only = [candidate(
            0,
            6,
            48_000,
            48_000,
            Some(RealtimeAudioSampleFormat::I32),
        )];
        let error = select_audio_output_candidate(
            48_000,
            AudioChannelLayout::Surround51Side,
            semantics,
            &integer_only,
        )
        .expect_err("the exact-mask Adapter executes only f32");
        assert_eq!(
            error.code,
            RealtimeAudioOutputOpenFailureCode::SampleFormatUnsupported
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_masks_preserve_canonical_named_speaker_order() {
        assert_eq!(windows_speaker_mask(AudioChannelLayout::Stereo), Some(0x3));
        assert_eq!(
            windows_speaker_mask(AudioChannelLayout::Surround51Side),
            Some(0x60f)
        );
        assert_eq!(
            windows_speaker_mask(AudioChannelLayout::Surround51Back),
            Some(0x3f)
        );
        assert_eq!(
            windows_speaker_mask(AudioChannelLayout::Surround71),
            Some(0x63f)
        );
        let unsupported = AudioChannelLayout::speakers([
            AudioChannelPosition::FrontLeft,
            AudioChannelPosition::WideLeft,
        ])
        .expect("valid core speaker layout");
        assert_eq!(windows_speaker_mask(unsupported), None);
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
        let selected = select_audio_output_candidate(
            48_000,
            layout,
            RealtimeAudioChannelSemantics::OrdinalDiscrete,
            &candidates,
        )
        .expect("ordinal device contract");
        assert_eq!(
            selected.contract.channel_semantics,
            RealtimeAudioChannelSemantics::OrdinalDiscrete
        );
    }
}
