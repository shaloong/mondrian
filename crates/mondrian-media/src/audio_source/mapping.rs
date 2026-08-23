//! Explicit standard-layout selection and FFmpeg channel-map lowering.

use mondrian_core::AudioChannelLayout;
pub use mondrian_core::AudioSourceSelection;

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
