//! Stable semantic channel layouts shared by authoring and execution.

use serde::{Deserialize, Serialize};

/// Semantic position of one interleaved PCM channel.
///
/// This is signal meaning, not a device-channel index. The order of positions
/// in [`AudioChannelLayout::ordered_channels`] is the canonical interleaving
/// order used at Mondrian's audio Interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// Left side surround.
    SideLeft,
    /// Right side surround.
    SideRight,
}

/// Version-stable semantic layout of one interleaved audio signal.
///
/// A layout is the sole authority for both channel count and interleaving
/// order. Device channel counts and decoder stream metadata are Adapter facts;
/// they must be mapped explicitly before entering this contract.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub enum AudioChannelLayout {
    /// One mono channel.
    Mono,
    /// Left, right.
    #[default]
    Stereo,
    /// Left, right, center, LFE, left surround, right surround.
    Surround51,
}

impl AudioChannelLayout {
    /// Canonical semantic channel order for this layout.
    pub const fn ordered_channels(self) -> &'static [AudioChannelPosition] {
        use AudioChannelPosition::{
            FrontCenter, FrontLeft, FrontRight, LowFrequencyEffects, Mono, SideLeft, SideRight,
        };

        match self {
            Self::Mono => &[Mono],
            Self::Stereo => &[FrontLeft, FrontRight],
            Self::Surround51 => &[
                FrontLeft,
                FrontRight,
                FrontCenter,
                LowFrequencyEffects,
                SideLeft,
                SideRight,
            ],
        }
    }

    /// Number of channels implied by the canonical layout.
    pub const fn channel_count(self) -> usize {
        self.ordered_channels().len()
    }

    /// Compact channel count for platform and container Adapters.
    pub const fn channel_count_u8(self) -> u8 {
        self.channel_count() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_layouts_have_stable_semantic_orders() {
        assert_eq!(
            AudioChannelLayout::Mono.ordered_channels(),
            &[AudioChannelPosition::Mono]
        );
        assert_eq!(
            AudioChannelLayout::Stereo.ordered_channels(),
            &[
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight
            ]
        );
        assert_eq!(AudioChannelLayout::Surround51.channel_count(), 6);
        assert_eq!(
            AudioChannelLayout::Surround51.ordered_channels()[4..],
            [
                AudioChannelPosition::SideLeft,
                AudioChannelPosition::SideRight
            ]
        );
    }
}
