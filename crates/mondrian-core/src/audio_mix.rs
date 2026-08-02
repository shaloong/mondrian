//! Canonical semantic channel-mix matrices shared by authoring and execution.

use crate::AudioChannelLayout;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::hash::{Hash, Hasher};

/// Hard safety bound for one linear-amplitude channel-mix coefficient.
///
/// The value is approximately +24 dB. It prevents corrupt author data from
/// creating unbounded intermediate samples without imposing normalization,
/// clipping, or limiter policy on a valid matrix.
pub const MAX_AUDIO_CHANNEL_MIX_GAIN: f64 = 16.0;

/// Finite, canonical linear-amplitude coefficient used by a channel matrix.
#[derive(Debug, Clone, Copy)]
pub struct AudioChannelMixGain(f64);

impl AudioChannelMixGain {
    /// Construct one finite coefficient inside the hard safety interval.
    pub fn new(value: f64) -> Result<Self, AudioChannelMixMatrixError> {
        if !value.is_finite() || value.abs() > MAX_AUDIO_CHANNEL_MIX_GAIN {
            return Err(AudioChannelMixMatrixError::InvalidGain);
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }

    /// Return the exact linear-amplitude value.
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for AudioChannelMixGain {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for AudioChannelMixGain {}

impl Hash for AudioChannelMixGain {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Serialize for AudioChannelMixGain {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for AudioChannelMixGain {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// One non-zero edge in a sparse source-to-destination channel matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioChannelMixEntry {
    source_channel: u8,
    destination_channel: u8,
    gain: AudioChannelMixGain,
}

impl AudioChannelMixEntry {
    /// Construct one sparse matrix edge.
    ///
    /// Channel bounds are checked by [`AudioChannelMixMatrix::new`] because
    /// they depend on the matrix layouts.
    pub fn new(
        source_channel: u8,
        destination_channel: u8,
        gain: f64,
    ) -> Result<Self, AudioChannelMixMatrixError> {
        Ok(Self {
            source_channel,
            destination_channel,
            gain: AudioChannelMixGain::new(gain)?,
        })
    }

    /// Zero-based channel in the matrix source layout.
    pub const fn source_channel(self) -> u8 {
        self.source_channel
    }

    /// Zero-based channel in the matrix destination layout.
    pub const fn destination_channel(self) -> u8 {
        self.destination_channel
    }

    /// Exact linear-amplitude coefficient.
    pub const fn gain(self) -> AudioChannelMixGain {
        self.gain
    }
}

/// Canonical sparse linear mapping between two exact semantic signal layouts.
///
/// Entries are stored destination-major then source-major. Missing edges are
/// exact zero, duplicate edges are invalid, and zero-valued edges are removed.
/// The matrix performs no normalization, clipping, limiting, or metadata
/// reinterpretation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct AudioChannelMixMatrix {
    source_layout: AudioChannelLayout,
    destination_layout: AudioChannelLayout,
    entries: Vec<AudioChannelMixEntry>,
}

impl crate::AuthoringFootprint for AudioChannelMixEntry {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { source_channel: _, destination_channel: _, gain: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for AudioChannelMixMatrix {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { source_layout: _, destination_layout: _, entries } = self;
        collector.collect(entries)
    }
}

#[derive(Deserialize)]
struct SerializedAudioChannelMixMatrix {
    source_layout: AudioChannelLayout,
    destination_layout: AudioChannelLayout,
    entries: Vec<AudioChannelMixEntry>,
}

impl<'de> Deserialize<'de> for AudioChannelMixMatrix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = SerializedAudioChannelMixMatrix::deserialize(deserializer)?;
        Self::new(value.source_layout, value.destination_layout, value.entries)
            .map_err(serde::de::Error::custom)
    }
}

impl AudioChannelMixMatrix {
    /// Validate and canonicalize one sparse matrix.
    pub fn new(
        source_layout: AudioChannelLayout,
        destination_layout: AudioChannelLayout,
        entries: impl IntoIterator<Item = AudioChannelMixEntry>,
    ) -> Result<Self, AudioChannelMixMatrixError> {
        let source_channels = source_layout.channel_count_u8();
        let destination_channels = destination_layout.channel_count_u8();
        let mut entries = entries.into_iter().collect::<Vec<_>>();
        for entry in &entries {
            if entry.source_channel >= source_channels {
                return Err(AudioChannelMixMatrixError::SourceChannelOutOfRange {
                    channel: entry.source_channel,
                    channels: source_channels,
                });
            }
            if entry.destination_channel >= destination_channels {
                return Err(AudioChannelMixMatrixError::DestinationChannelOutOfRange {
                    channel: entry.destination_channel,
                    channels: destination_channels,
                });
            }
        }
        entries.sort_by_key(|entry| (entry.destination_channel, entry.source_channel));
        if entries.windows(2).any(|pair| {
            pair[0].source_channel == pair[1].source_channel
                && pair[0].destination_channel == pair[1].destination_channel
        }) {
            return Err(AudioChannelMixMatrixError::DuplicateEntry);
        }
        entries.retain(|entry| entry.gain.get() != 0.0);
        Ok(Self { source_layout, destination_layout, entries })
    }

    /// Construct an exact ordinal identity mapping for one layout.
    pub fn identity(layout: AudioChannelLayout) -> Self {
        let entries = (0..layout.channel_count_u8()).map(|channel| AudioChannelMixEntry {
            source_channel: channel,
            destination_channel: channel,
            gain: AudioChannelMixGain(1.0),
        });
        Self::new(layout, layout, entries).expect("identity matrix uses valid layout bounds")
    }

    /// Resolve Mondrian's versioned standard mapping for one exact layout pair.
    ///
    /// Equal layouts always preserve ordinal channels. Additional admitted
    /// conversions are deliberately restricted to mono, stereo, 5.1(side),
    /// and the explicit one/two-channel Discrete compatibility cases.
    pub fn standard(
        source_layout: AudioChannelLayout,
        destination_layout: AudioChannelLayout,
    ) -> Result<Self, AudioChannelMixMatrixError> {
        if source_layout == destination_layout {
            return Ok(Self::identity(source_layout));
        }

        let source_kind = StandardLayoutKind::from_layout(source_layout);
        let destination_kind = StandardLayoutKind::from_layout(destination_layout);
        let entries: &[(u8, u8, f64)] = match (source_kind, destination_kind) {
            (Some(StandardLayoutKind::One), Some(StandardLayoutKind::One)) => &[(0, 0, 1.0)],
            (Some(StandardLayoutKind::One), Some(StandardLayoutKind::Two)) => {
                &[(0, 0, 1.0), (0, 1, 1.0)]
            }
            (Some(StandardLayoutKind::One), Some(StandardLayoutKind::Surround51Side)) => {
                &[(0, 2, 1.0)]
            }
            (Some(StandardLayoutKind::Two), Some(StandardLayoutKind::One)) => {
                &[(0, 0, 0.5), (1, 0, 0.5)]
            }
            (Some(StandardLayoutKind::Two), Some(StandardLayoutKind::Two)) => {
                &[(0, 0, 1.0), (1, 1, 1.0)]
            }
            (Some(StandardLayoutKind::Two), Some(StandardLayoutKind::Surround51Side)) => {
                &[(0, 0, 1.0), (1, 1, 1.0)]
            }
            (Some(StandardLayoutKind::Surround51Side), Some(StandardLayoutKind::One)) => &[
                (0, 0, 0.5),
                (1, 0, 0.5),
                (2, 0, std::f64::consts::FRAC_1_SQRT_2),
                (4, 0, 0.353_553_390_593_273_8),
                (5, 0, 0.353_553_390_593_273_8),
            ],
            (Some(StandardLayoutKind::Surround51Side), Some(StandardLayoutKind::Two)) => &[
                (0, 0, 1.0),
                (2, 0, std::f64::consts::FRAC_1_SQRT_2),
                (4, 0, std::f64::consts::FRAC_1_SQRT_2),
                (1, 1, 1.0),
                (2, 1, std::f64::consts::FRAC_1_SQRT_2),
                (5, 1, std::f64::consts::FRAC_1_SQRT_2),
            ],
            _ => {
                return Err(AudioChannelMixMatrixError::NoStandardMapping {
                    source_layout,
                    destination_layout,
                });
            }
        };
        let entries = entries
            .iter()
            .map(|&(source, destination, gain)| {
                AudioChannelMixEntry::new(source, destination, gain)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(source_layout, destination_layout, entries)
    }

    /// Exact source signal layout.
    pub const fn source_layout(&self) -> AudioChannelLayout {
        self.source_layout
    }

    /// Exact destination signal layout.
    pub const fn destination_layout(&self) -> AudioChannelLayout {
        self.destination_layout
    }

    /// Canonically ordered non-zero matrix edges.
    pub fn entries(&self) -> &[AudioChannelMixEntry] {
        &self.entries
    }

    /// Whether this is an exact ordinal identity transform.
    pub fn is_identity(&self) -> bool {
        self.source_layout == self.destination_layout
            && self.entries.len() == self.source_layout.channel_count()
            && self.entries.iter().enumerate().all(|(channel, entry)| {
                usize::from(entry.source_channel) == channel
                    && usize::from(entry.destination_channel) == channel
                    && entry.gain.get() == 1.0
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandardLayoutKind {
    One,
    Two,
    Surround51Side,
}

impl StandardLayoutKind {
    fn from_layout(layout: AudioChannelLayout) -> Option<Self> {
        if layout == AudioChannelLayout::Mono
            || layout.discrete_channel_count().is_some_and(|count| count.get() == 1)
        {
            Some(Self::One)
        } else if layout == AudioChannelLayout::Stereo
            || layout.discrete_channel_count().is_some_and(|count| count.get() == 2)
        {
            Some(Self::Two)
        } else if layout == AudioChannelLayout::Surround51Side {
            Some(Self::Surround51Side)
        } else {
            None
        }
    }
}

/// Invalid, ambiguous, or unsupported channel-mix authoring.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioChannelMixMatrixError {
    /// Coefficients must be finite and inside the hard amplitude interval.
    #[error("audio channel-mix gain must be finite and within +/-{MAX_AUDIO_CHANNEL_MIX_GAIN}")]
    InvalidGain,
    /// One sparse source index exceeded the declared source layout.
    #[error("audio channel-mix source channel {channel} is outside 0..{channels}")]
    SourceChannelOutOfRange { channel: u8, channels: u8 },
    /// One sparse destination index exceeded the declared destination layout.
    #[error("audio channel-mix destination channel {channel} is outside 0..{channels}")]
    DestinationChannelOutOfRange { channel: u8, channels: u8 },
    /// A source/destination pair must have exactly one canonical coefficient.
    #[error("audio channel-mix matrix contains a duplicate source/destination entry")]
    DuplicateEntry,
    /// The standard policy deliberately has no mapping for this semantic pair.
    #[error("no standard audio channel mapping from {source_layout} to {destination_layout}")]
    NoStandardMapping {
        source_layout: AudioChannelLayout,
        destination_layout: AudioChannelLayout,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: u8, destination: u8, gain: f64) -> AudioChannelMixEntry {
        AudioChannelMixEntry::new(source, destination, gain).expect("valid matrix entry")
    }

    #[test]
    fn canonicalizes_sparse_order_and_omits_zero_edges() {
        let matrix = AudioChannelMixMatrix::new(
            AudioChannelLayout::Stereo,
            AudioChannelLayout::Stereo,
            [entry(1, 1, 1.0), entry(0, 1, 0.0), entry(0, 0, 1.0)],
        )
        .expect("valid matrix");

        assert!(matrix.is_identity());
        assert_eq!(matrix.entries(), &[entry(0, 0, 1.0), entry(1, 1, 1.0)]);
    }

    #[test]
    fn rejects_invalid_gain_bounds_indices_and_duplicates() {
        assert_eq!(
            AudioChannelMixEntry::new(0, 0, f64::NAN),
            Err(AudioChannelMixMatrixError::InvalidGain)
        );
        assert_eq!(
            AudioChannelMixEntry::new(0, 0, MAX_AUDIO_CHANNEL_MIX_GAIN + 0.1),
            Err(AudioChannelMixMatrixError::InvalidGain)
        );
        assert_eq!(
            AudioChannelMixMatrix::new(
                AudioChannelLayout::Mono,
                AudioChannelLayout::Mono,
                [entry(1, 0, 1.0)]
            ),
            Err(AudioChannelMixMatrixError::SourceChannelOutOfRange { channel: 1, channels: 1 })
        );
        assert_eq!(
            AudioChannelMixMatrix::new(
                AudioChannelLayout::Mono,
                AudioChannelLayout::Mono,
                [entry(0, 0, 1.0), entry(0, 0, 0.5)]
            ),
            Err(AudioChannelMixMatrixError::DuplicateEntry)
        );
    }

    #[test]
    fn standard_policy_is_explicit_and_fails_closed() {
        let stereo = AudioChannelMixMatrix::standard(
            AudioChannelLayout::Surround51Side,
            AudioChannelLayout::Stereo,
        )
        .expect("approved downmix");
        assert_eq!(stereo.entries().len(), 6);
        assert!(!stereo.entries().iter().any(|entry| entry.source_channel() == 3));

        assert!(matches!(
            AudioChannelMixMatrix::standard(
                AudioChannelLayout::Surround51Back,
                AudioChannelLayout::Stereo,
            ),
            Err(AudioChannelMixMatrixError::NoStandardMapping { .. })
        ));
        assert!(AudioChannelMixMatrix::standard(
            AudioChannelLayout::discrete(2).expect("discrete layout"),
            AudioChannelLayout::Stereo,
        )
        .is_ok());
    }

    #[test]
    fn serialization_round_trip_preserves_canonical_identity() {
        let matrix =
            AudioChannelMixMatrix::standard(AudioChannelLayout::Stereo, AudioChannelLayout::Mono)
                .expect("standard matrix");
        let json = serde_json::to_string(&matrix).expect("serialize matrix");
        let decoded: AudioChannelMixMatrix =
            serde_json::from_str(&json).expect("deserialize matrix");
        assert_eq!(decoded, matrix);
    }
}
