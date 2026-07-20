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

/// Lower an exact native layout to an ordinal FFmpeg identity `pan` filter.
///
/// Channel conversion belongs to the prepared audio graph. The media Adapter
/// only proves native interleaving and never bakes Clip-specific downmix policy
/// into decoded-window cache identity.
pub(super) fn identity_pan_filter(layout: AudioChannelLayout) -> String {
    let mut filter = format!("pan={}c", layout.channel_count());
    for channel in 0..layout.channel_count() {
        filter.push_str(&format!("|c{channel}=c{channel}"));
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_filter_preserves_every_ordinal_without_conversion() {
        assert_eq!(
            identity_pan_filter(AudioChannelLayout::Mono),
            "pan=1c|c0=c0"
        );
        assert_eq!(
            identity_pan_filter(AudioChannelLayout::Surround51Side),
            "pan=6c|c0=c0|c1=c1|c2=c2|c3=c3|c4=c4|c5=c5"
        );
    }
}
