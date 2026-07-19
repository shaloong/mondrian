//! Explicit standard-layout selection and FFmpeg channel-map lowering.

use crate::{
    info::{AudioStreamInfo, ChannelLayout},
    MediaFileFingerprint,
};
use mondrian_core::AudioChannelLayout;
use serde::{Deserialize, Serialize};

/// One explicitly selected physical audio stream and its probed semantic layout.
///
/// This is an Adapter value, not author state. Asset Component catalogs own
/// stable logical identity and produce a selection only after their persisted
/// stream signature matches current probe evidence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioSourceSelection {
    stream_index: u32,
    source_layout: ChannelLayout,
    source_fingerprint: MediaFileFingerprint,
}

impl AudioSourceSelection {
    /// Create a physical selection from explicit probe or fixture evidence.
    pub const fn new(
        stream_index: u32,
        source_layout: ChannelLayout,
        source_fingerprint: MediaFileFingerprint,
    ) -> Self {
        Self { stream_index, source_layout, source_fingerprint }
    }

    /// Capture an execution selection from one already-validated stream probe.
    pub fn from_stream(stream: &AudioStreamInfo, source_fingerprint: MediaFileFingerprint) -> Self {
        Self::new(
            stream.index,
            stream.channel_layout.clone(),
            source_fingerprint,
        )
    }

    /// Absolute container stream index used by FFmpeg `-map 0:<index>`.
    pub const fn stream_index(&self) -> u32 {
        self.stream_index
    }

    /// Exact native semantic layout observed during probe.
    pub const fn source_layout(&self) -> &ChannelLayout {
        &self.source_layout
    }

    /// File revision whose probe evidence authorized this selection.
    pub const fn source_fingerprint(&self) -> MediaFileFingerprint {
        self.source_fingerprint
    }
}

/// Resolve the deterministic standard mix matrix for one source/output pair.
///
/// Coefficients are linear-amplitude values. LFE is deliberately omitted from
/// standard downmixes; no hidden normalization, clipping, or limiter is added.
pub(super) fn standard_pan_filter(
    source: &ChannelLayout,
    output: AudioChannelLayout,
) -> Option<&'static str> {
    match (source, output) {
        (ChannelLayout::Mono | ChannelLayout::Unspecified(1), AudioChannelLayout::Mono) => {
            Some("pan=mono|c0=c0")
        }
        (ChannelLayout::Mono | ChannelLayout::Unspecified(1), AudioChannelLayout::Stereo) => {
            Some("pan=stereo|c0=c0|c1=c0")
        }
        (
            ChannelLayout::Mono | ChannelLayout::Unspecified(1),
            AudioChannelLayout::Surround51Side,
        ) => Some(
            "pan=5.1(side)|c0=0*c0|c1=0*c0|c2=c0|c3=0*c0|c4=0*c0|c5=0*c0",
        ),
        (ChannelLayout::Stereo | ChannelLayout::Unspecified(2), AudioChannelLayout::Mono) => {
            Some("pan=mono|c0=0.5*c0+0.5*c1")
        }
        (ChannelLayout::Stereo | ChannelLayout::Unspecified(2), AudioChannelLayout::Stereo) => {
            Some("pan=stereo|c0=c0|c1=c1")
        }
        (
            ChannelLayout::Stereo | ChannelLayout::Unspecified(2),
            AudioChannelLayout::Surround51Side,
        ) => Some(
            "pan=5.1(side)|c0=c0|c1=c1|c2=0*c0|c3=0*c0|c4=0*c0|c5=0*c0",
        ),
        (ChannelLayout::Surround51Side, AudioChannelLayout::Mono) => Some(
            "pan=mono|c0=0.5*c0+0.5*c1+0.7071067811865476*c2+0.3535533905932738*c4+0.3535533905932738*c5",
        ),
        (ChannelLayout::Surround51Side, AudioChannelLayout::Stereo) => Some(
            "pan=stereo|c0=c0+0.7071067811865476*c2+0.7071067811865476*c4|c1=c1+0.7071067811865476*c2+0.7071067811865476*c5",
        ),
        (ChannelLayout::Surround51Side, AudioChannelLayout::Surround51Side) => {
            Some("pan=5.1(side)|c0=c0|c1=c1|c2=c2|c3=c3|c4=c4|c5=c5")
        }
        (
            ChannelLayout::Unspecified(_)
            | ChannelLayout::Surround51Back
            | ChannelLayout::Surround71
            | ChannelLayout::Other(_),
            _,
        ) => None,
        (_, AudioChannelLayout::Speakers(_) | AudioChannelLayout::Discrete(_)) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_supported_standard_layout_pairs_have_an_explicit_matrix() {
        for source in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::Surround51Side,
        ] {
            for output in [
                AudioChannelLayout::Mono,
                AudioChannelLayout::Stereo,
                AudioChannelLayout::Surround51Side,
            ] {
                assert!(standard_pan_filter(&source, output).is_some());
            }
        }
    }

    #[test]
    fn unsupported_or_ambiguous_native_layouts_fail_closed() {
        for source in [
            ChannelLayout::Unspecified(3),
            ChannelLayout::Surround51Back,
            ChannelLayout::Surround71,
            ChannelLayout::Other(4),
        ] {
            assert_eq!(
                standard_pan_filter(&source, AudioChannelLayout::Stereo),
                None
            );
        }
        assert_eq!(
            standard_pan_filter(&ChannelLayout::Stereo, AudioChannelLayout::Surround51Back),
            None
        );
        assert_eq!(
            standard_pan_filter(
                &ChannelLayout::Stereo,
                AudioChannelLayout::discrete(2).expect("discrete layout")
            ),
            None
        );
    }

    #[test]
    fn unspecified_one_and_two_channel_sources_use_explicit_discrete_defaults() {
        assert_eq!(
            standard_pan_filter(&ChannelLayout::Unspecified(1), AudioChannelLayout::Stereo),
            Some("pan=stereo|c0=c0|c1=c0")
        );
        assert_eq!(
            standard_pan_filter(&ChannelLayout::Unspecified(2), AudioChannelLayout::Stereo),
            Some("pan=stereo|c0=c0|c1=c1")
        );
    }

    #[test]
    fn surround_downmix_omits_lfe_and_uses_semantic_side_channels() {
        let stereo =
            standard_pan_filter(&ChannelLayout::Surround51Side, AudioChannelLayout::Stereo)
                .expect("5.1 side to stereo");

        assert!(!stereo.contains("c3"));
        assert!(stereo.contains("c4"));
        assert!(stereo.contains("c5"));
    }
}
