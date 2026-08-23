//! Stable semantic channel layouts shared by authoring and execution.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Maximum number of interleaved channels admitted by the common signal contract.
///
/// This is an allocation and plugin-boundary safety limit, not a claim that every
/// Adapter can render every admitted layout. Concrete media, device, and export
/// Adapters still negotiate or reject the layout explicitly.
pub const MAX_AUDIO_CHANNELS: u8 = 64;

/// Semantic position of one interleaved PCM speaker channel.
///
/// This is signal meaning, not a device-channel index. Speaker layouts use the
/// declaration order below as their canonical interleaving order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AudioChannelPosition {
    /// A layout-independent mono program channel.
    Mono,
    /// Front left.
    FrontLeft,
    /// Front right.
    FrontRight,
    /// Front center.
    FrontCenter,
    /// Low-frequency effects.
    LowFrequencyEffects,
    /// Back left.
    BackLeft,
    /// Back right.
    BackRight,
    /// Front left-of-center.
    FrontLeftOfCenter,
    /// Front right-of-center.
    FrontRightOfCenter,
    /// Back center.
    BackCenter,
    /// Left side surround.
    SideLeft,
    /// Right side surround.
    SideRight,
    /// Top center.
    TopCenter,
    /// Top-front left.
    TopFrontLeft,
    /// Top-front center.
    TopFrontCenter,
    /// Top-front right.
    TopFrontRight,
    /// Top-back left.
    TopBackLeft,
    /// Top-back center.
    TopBackCenter,
    /// Top-back right.
    TopBackRight,
    /// Left wide/front-wide.
    WideLeft,
    /// Right wide/front-wide.
    WideRight,
    /// Left top-side.
    TopSideLeft,
    /// Right top-side.
    TopSideRight,
    /// Second low-frequency-effects channel.
    LowFrequencyEffects2,
}

const SPEAKER_POSITIONS: [AudioChannelPosition; 23] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::LowFrequencyEffects,
    AudioChannelPosition::BackLeft,
    AudioChannelPosition::BackRight,
    AudioChannelPosition::FrontLeftOfCenter,
    AudioChannelPosition::FrontRightOfCenter,
    AudioChannelPosition::BackCenter,
    AudioChannelPosition::SideLeft,
    AudioChannelPosition::SideRight,
    AudioChannelPosition::TopCenter,
    AudioChannelPosition::TopFrontLeft,
    AudioChannelPosition::TopFrontCenter,
    AudioChannelPosition::TopFrontRight,
    AudioChannelPosition::TopBackLeft,
    AudioChannelPosition::TopBackCenter,
    AudioChannelPosition::TopBackRight,
    AudioChannelPosition::WideLeft,
    AudioChannelPosition::WideRight,
    AudioChannelPosition::TopSideLeft,
    AudioChannelPosition::TopSideRight,
    AudioChannelPosition::LowFrequencyEffects2,
];

impl AudioChannelPosition {
    const fn speaker_bit(self) -> Option<u64> {
        match self {
            Self::Mono => None,
            Self::FrontLeft => Some(1 << 0),
            Self::FrontRight => Some(1 << 1),
            Self::FrontCenter => Some(1 << 2),
            Self::LowFrequencyEffects => Some(1 << 3),
            Self::BackLeft => Some(1 << 4),
            Self::BackRight => Some(1 << 5),
            Self::FrontLeftOfCenter => Some(1 << 6),
            Self::FrontRightOfCenter => Some(1 << 7),
            Self::BackCenter => Some(1 << 8),
            Self::SideLeft => Some(1 << 9),
            Self::SideRight => Some(1 << 10),
            Self::TopCenter => Some(1 << 11),
            Self::TopFrontLeft => Some(1 << 12),
            Self::TopFrontCenter => Some(1 << 13),
            Self::TopFrontRight => Some(1 << 14),
            Self::TopBackLeft => Some(1 << 15),
            Self::TopBackCenter => Some(1 << 16),
            Self::TopBackRight => Some(1 << 17),
            Self::WideLeft => Some(1 << 18),
            Self::WideRight => Some(1 << 19),
            Self::TopSideLeft => Some(1 << 20),
            Self::TopSideRight => Some(1 << 21),
            Self::LowFrequencyEffects2 => Some(1 << 22),
        }
    }
}

impl fmt::Display for AudioChannelPosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mono => "Mono",
            Self::FrontLeft => "FrontLeft",
            Self::FrontRight => "FrontRight",
            Self::FrontCenter => "FrontCenter",
            Self::LowFrequencyEffects => "LFE",
            Self::BackLeft => "BackLeft",
            Self::BackRight => "BackRight",
            Self::FrontLeftOfCenter => "FrontLeftOfCenter",
            Self::FrontRightOfCenter => "FrontRightOfCenter",
            Self::BackCenter => "BackCenter",
            Self::SideLeft => "SideLeft",
            Self::SideRight => "SideRight",
            Self::TopCenter => "TopCenter",
            Self::TopFrontLeft => "TopFrontLeft",
            Self::TopFrontCenter => "TopFrontCenter",
            Self::TopFrontRight => "TopFrontRight",
            Self::TopBackLeft => "TopBackLeft",
            Self::TopBackCenter => "TopBackCenter",
            Self::TopBackRight => "TopBackRight",
            Self::WideLeft => "WideLeft",
            Self::WideRight => "WideRight",
            Self::TopSideLeft => "TopSideLeft",
            Self::TopSideRight => "TopSideRight",
            Self::LowFrequencyEffects2 => "LFE2",
        })
    }
}

/// Validated set of named speaker positions.
///
/// The bit set is order-independent; iteration always follows Mondrian's
/// canonical speaker order. This keeps cache and plan identity stable when two
/// hosts describe the same speaker set in different discovery orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioSpeakerSet(u64);

impl AudioSpeakerSet {
    const STEREO_BITS: u64 = (1 << 0) | (1 << 1);
    const SURROUND_5_1_SIDE_BITS: u64 =
        Self::STEREO_BITS | (1 << 2) | (1 << 3) | (1 << 9) | (1 << 10);
    const SURROUND_5_1_BACK_BITS: u64 =
        Self::STEREO_BITS | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
    const SURROUND_7_1_BITS: u64 = Self::SURROUND_5_1_SIDE_BITS | (1 << 4) | (1 << 5);

    /// Build a non-empty named speaker set.
    pub fn new(
        positions: impl IntoIterator<Item = AudioChannelPosition>,
    ) -> Result<Self, AudioChannelLayoutError> {
        let mut bits = 0_u64;
        for position in positions {
            let bit =
                position.speaker_bit().ok_or(AudioChannelLayoutError::MonoInsideSpeakerSet)?;
            if bits & bit != 0 {
                return Err(AudioChannelLayoutError::DuplicateSpeakerPosition(position));
            }
            bits |= bit;
        }
        if bits == 0 {
            return Err(AudioChannelLayoutError::EmptySpeakerSet);
        }
        Ok(Self(bits))
    }

    /// Number of canonical interleaved speaker channels.
    pub const fn channel_count(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether this set contains one semantic speaker position.
    pub const fn contains(self, position: AudioChannelPosition) -> bool {
        match position.speaker_bit() {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }

    /// Iterate positions in the canonical interleaving order.
    pub fn positions(self) -> impl Iterator<Item = AudioChannelPosition> {
        SPEAKER_POSITIONS.into_iter().filter(move |position| self.contains(*position))
    }

    const fn from_valid_bits(bits: u64) -> Self {
        Self(bits)
    }
}

impl Serialize for AudioSpeakerSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.positions().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AudioSpeakerSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let positions = Vec::<AudioChannelPosition>::deserialize(deserializer)?;
        Self::new(positions).map_err(serde::de::Error::custom)
    }
}

/// Validated number of layout-independent discrete channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct AudioDiscreteChannelCount(u8);

impl AudioDiscreteChannelCount {
    /// Construct a bounded non-zero discrete channel count.
    pub const fn new(channels: u8) -> Result<Self, AudioChannelLayoutError> {
        if channels == 0 || channels > MAX_AUDIO_CHANNELS {
            return Err(AudioChannelLayoutError::InvalidDiscreteChannelCount(
                channels,
            ));
        }
        Ok(Self(channels))
    }

    /// Return the concrete channel count.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for AudioDiscreteChannelCount {
    type Error = AudioChannelLayoutError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AudioDiscreteChannelCount> for u8 {
    fn from(value: AudioDiscreteChannelCount) -> Self {
        value.get()
    }
}

/// Version-stable semantic layout of one interleaved audio signal.
///
/// Named speaker sets have one canonical interleaving order. Discrete layouts
/// intentionally carry no speaker meaning and therefore cannot be silently
/// lowered through speaker mix matrices. A concrete Adapter must explicitly
/// negotiate or reject every non-standard layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AudioChannelLayout {
    /// One layout-independent mono program channel.
    Mono,
    /// A non-empty canonical set of named speaker positions.
    Speakers(AudioSpeakerSet),
    /// Layout-independent channels addressed only by stable ordinal.
    Discrete(AudioDiscreteChannelCount),
}

impl Default for AudioChannelLayout {
    fn default() -> Self {
        Self::Stereo
    }
}

#[allow(non_upper_case_globals)]
impl AudioChannelLayout {
    /// Front-left and front-right.
    pub const Stereo: Self = Self::Speakers(AudioSpeakerSet::from_valid_bits(
        AudioSpeakerSet::STEREO_BITS,
    ));
    /// Left, right, center, LFE, side-left, side-right.
    pub const Surround51Side: Self = Self::Speakers(AudioSpeakerSet::from_valid_bits(
        AudioSpeakerSet::SURROUND_5_1_SIDE_BITS,
    ));
    /// Left, right, center, LFE, back-left, back-right.
    pub const Surround51Back: Self = Self::Speakers(AudioSpeakerSet::from_valid_bits(
        AudioSpeakerSet::SURROUND_5_1_BACK_BITS,
    ));
    /// 5.1(side) plus back-left and back-right.
    pub const Surround71: Self = Self::Speakers(AudioSpeakerSet::from_valid_bits(
        AudioSpeakerSet::SURROUND_7_1_BITS,
    ));

    /// Construct a validated custom named-speaker layout.
    pub fn speakers(
        positions: impl IntoIterator<Item = AudioChannelPosition>,
    ) -> Result<Self, AudioChannelLayoutError> {
        Ok(Self::Speakers(AudioSpeakerSet::new(positions)?))
    }

    /// Construct a bounded layout-independent discrete bus.
    pub const fn discrete(channels: u8) -> Result<Self, AudioChannelLayoutError> {
        match AudioDiscreteChannelCount::new(channels) {
            Ok(channels) => Ok(Self::Discrete(channels)),
            Err(error) => Err(error),
        }
    }

    /// Number of channels implied by the sole layout authority.
    pub const fn channel_count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Speakers(speakers) => speakers.channel_count(),
            Self::Discrete(channels) => channels.get() as usize,
        }
    }

    /// Compact channel count for platform and container Adapters.
    pub const fn channel_count_u8(self) -> u8 {
        self.channel_count() as u8
    }

    /// Semantic position at one canonical interleaved channel index.
    ///
    /// Discrete channels return `None`; their ordinal is identity and must not
    /// be guessed as a speaker position.
    pub fn channel_position(self, index: usize) -> Option<AudioChannelPosition> {
        match self {
            Self::Mono => (index == 0).then_some(AudioChannelPosition::Mono),
            Self::Speakers(speakers) => speakers.positions().nth(index),
            Self::Discrete(_) => None,
        }
    }

    /// Named speaker set when this is a speaker layout.
    pub const fn speaker_set(self) -> Option<AudioSpeakerSet> {
        match self {
            Self::Speakers(speakers) => Some(speakers),
            Self::Mono | Self::Discrete(_) => None,
        }
    }

    /// Discrete channel count when speaker meaning is deliberately absent.
    pub const fn discrete_channel_count(self) -> Option<AudioDiscreteChannelCount> {
        match self {
            Self::Discrete(channels) => Some(channels),
            Self::Mono | Self::Speakers(_) => None,
        }
    }
}

impl fmt::Display for AudioChannelLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::Mono {
            return formatter.write_str("Mono");
        }
        if *self == Self::Stereo {
            return formatter.write_str("Stereo");
        }
        if *self == Self::Surround51Side {
            return formatter.write_str("5.1(side)");
        }
        if *self == Self::Surround51Back {
            return formatter.write_str("5.1(back)");
        }
        if *self == Self::Surround71 {
            return formatter.write_str("7.1");
        }
        match self {
            Self::Speakers(speakers) => {
                formatter.write_str("Speakers(")?;
                for (index, position) in speakers.positions().enumerate() {
                    if index > 0 {
                        formatter.write_str(",")?;
                    }
                    write!(formatter, "{position}")?;
                }
                formatter.write_str(")")
            }
            Self::Discrete(channels) => write!(formatter, "Discrete({})", channels.get()),
            Self::Mono => formatter.write_str("Mono"),
        }
    }
}

/// Invalid or ambiguous audio signal layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioChannelLayoutError {
    /// A named speaker set cannot be empty.
    EmptySpeakerSet,
    /// The independent mono program semantic cannot be mixed into a speaker set.
    MonoInsideSpeakerSet,
    /// A speaker position appeared more than once.
    DuplicateSpeakerPosition(AudioChannelPosition),
    /// Discrete buses must contain between one and [`MAX_AUDIO_CHANNELS`] channels.
    InvalidDiscreteChannelCount(u8),
}

impl fmt::Display for AudioChannelLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySpeakerSet => formatter.write_str("audio speaker layout cannot be empty"),
            Self::MonoInsideSpeakerSet => formatter.write_str(
                "layout-independent mono cannot be combined with named speaker positions",
            ),
            Self::DuplicateSpeakerPosition(position) => {
                write!(formatter, "duplicate audio speaker position {position:?}")
            }
            Self::InvalidDiscreteChannelCount(channels) => write!(
                formatter,
                "discrete audio channel count {channels} is outside 1..={MAX_AUDIO_CHANNELS}"
            ),
        }
    }
}

impl std::error::Error for AudioChannelLayoutError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_layouts_have_stable_semantic_orders() {
        assert_eq!(
            AudioChannelLayout::Mono.channel_position(0),
            Some(AudioChannelPosition::Mono)
        );
        assert_eq!(
            (0..AudioChannelLayout::Stereo.channel_count())
                .filter_map(|index| AudioChannelLayout::Stereo.channel_position(index))
                .collect::<Vec<_>>(),
            [
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight
            ]
        );
        assert_eq!(AudioChannelLayout::Surround51Side.channel_count(), 6);
        assert_eq!(
            AudioChannelLayout::Surround51Side.channel_position(4),
            Some(AudioChannelPosition::SideLeft)
        );
        assert_eq!(
            AudioChannelLayout::Surround51Back.channel_position(4),
            Some(AudioChannelPosition::BackLeft)
        );
        assert_ne!(
            AudioChannelLayout::Surround51Side,
            AudioChannelLayout::Surround51Back
        );
    }

    #[test]
    fn custom_speaker_sets_are_canonical_and_serde_stable() {
        let left_right_center = AudioChannelLayout::speakers([
            AudioChannelPosition::FrontCenter,
            AudioChannelPosition::FrontRight,
            AudioChannelPosition::FrontLeft,
        ])
        .expect("speaker layout");
        assert_eq!(
            (0..left_right_center.channel_count())
                .filter_map(|index| left_right_center.channel_position(index))
                .collect::<Vec<_>>(),
            [
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
                AudioChannelPosition::FrontCenter,
            ]
        );

        let encoded = serde_json::to_string(&left_right_center).expect("serialize layout");
        let decoded: AudioChannelLayout = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, left_right_center);
        assert_eq!(
            serde_json::to_value(AudioChannelLayout::Stereo).expect("serialize stereo"),
            serde_json::json!({"Speakers": ["FrontLeft", "FrontRight"]})
        );
    }

    #[test]
    fn invalid_or_ambiguous_layouts_fail_closed() {
        assert_eq!(
            AudioChannelLayout::speakers([]),
            Err(AudioChannelLayoutError::EmptySpeakerSet)
        );
        assert_eq!(
            AudioChannelLayout::speakers([AudioChannelPosition::Mono]),
            Err(AudioChannelLayoutError::MonoInsideSpeakerSet)
        );
        assert!(AudioChannelLayout::discrete(0).is_err());
        assert!(AudioChannelLayout::discrete(MAX_AUDIO_CHANNELS).is_ok());
        assert!(AudioChannelLayout::discrete(MAX_AUDIO_CHANNELS + 1).is_err());
        assert!(
            serde_json::from_value::<AudioChannelLayout>(serde_json::json!({"Speakers": []}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<AudioChannelLayout>(serde_json::json!({
                "Speakers": ["FrontLeft", "FrontLeft"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AudioChannelLayout>(serde_json::json!({"Discrete": 0}))
                .is_err()
        );
    }
}
