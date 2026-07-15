//! Stable app/UI registry identities for GPU Program Output scopes.

use mondrian_ui_widgets::VideoScopesTextureSet;

pub(crate) const HISTOGRAM_TEXTURE_KEY: &str = "mondrian.scopes.program.histogram";
pub(crate) const WAVEFORM_TEXTURE_KEY: &str = "mondrian.scopes.program.waveform";
pub(crate) const VECTORSCOPE_TEXTURE_KEY: &str = "mondrian.scopes.program.vectorscope";

pub(crate) fn texture_set() -> VideoScopesTextureSet {
    VideoScopesTextureSet {
        histogram: HISTOGRAM_TEXTURE_KEY.to_owned(),
        waveform: WAVEFORM_TEXTURE_KEY.to_owned(),
        vectorscope: VECTORSCOPE_TEXTURE_KEY.to_owned(),
    }
}
