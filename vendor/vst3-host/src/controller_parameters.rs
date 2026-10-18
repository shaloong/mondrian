//! Bounded borrowed observations of the native edit controller, on its owner thread.
//!
//! This API does not drain deferred DSP feedback, mutate the controller, cache values,
//! publish author edits, or retire restart callbacks. The caller owns those decisions.

use vst3::Steinberg::Vst::ParameterInfo;

/// One reusable inline SDK output slot; no plugin-sized allocation is involved.
pub struct ControllerParameterQueryFrame {
    pub(crate) info: ParameterInfo,
    pub(crate) value: f64,
}

impl Default for ControllerParameterQueryFrame {
    fn default() -> Self {
        // SAFETY: ParameterInfo contains only integers, floating point and fixed
        // integer arrays. Zero is valid for every field; no pointers or references.
        Self {
            info: unsafe { std::mem::zeroed() },
            value: 0.0,
        }
    }
}

impl ControllerParameterQueryFrame {
    pub(crate) fn observation(&self) -> ControllerParameterRef<'_> {
        ControllerParameterRef { frame: self }
    }
}

/// Borrow of one completed native metadata/value query, valid only during a visit.
#[derive(Clone, Copy)]
pub struct ControllerParameterRef<'a> {
    frame: &'a ControllerParameterQueryFrame,
}

impl<'a> ControllerParameterRef<'a> {
    /// Native definition-local ID.
    pub fn id(self) -> u32 {
        self.frame.info.id
    }
    /// Native descriptor default, not the current controller value.
    pub fn default_normalized(self) -> f64 {
        self.frame.info.defaultNormalizedValue
    }
    /// Native number of discrete gaps.
    pub fn step_count(self) -> i32 {
        self.frame.info.stepCount
    }
    /// All native descriptor flags, including unknown bits.
    pub fn flags(self) -> u32 {
        self.frame.info.flags as u32
    }
    /// Current controller value; the consumer must validate finiteness and range.
    pub fn normalized(self) -> f64 {
        self.frame.value
    }
    /// Exact fixed SDK title, including trailing UTF-16 code units.
    pub fn title_utf16(self) -> &'a [u16; 128] {
        &self.frame.info.title
    }
    /// Exact fixed SDK short title, without lossy text conversion.
    pub fn short_title_utf16(self) -> &'a [u16; 128] {
        &self.frame.info.shortTitle
    }
    /// Exact fixed SDK units, without scanning beyond the query frame.
    pub fn units_utf16(self) -> &'a [u16; 128] {
        &self.frame.info.units
    }
    /// Native unit identity belonging to this parameter information query.
    pub fn unit_id(self) -> i32 {
        self.frame.info.unitId
    }
    /// Compare the fixed native title with the existing lossy UTF-16 discovery name.
    /// Never allocates or scans beyond the SDK's fixed title extent.
    pub fn title_matches(self, expected: &str) -> bool {
        let native = self
            .frame
            .info
            .title
            .iter()
            .take_while(|code| **code != 0)
            .copied();
        char::decode_utf16(native)
            .map(|code| code.unwrap_or(char::REPLACEMENT_CHARACTER))
            .eq(expected.chars())
    }
}

/// Separate fixed scratch for optional presentation queries. The original rescan
/// query frame and its metadata/value-only call sequence remain unchanged.
pub struct ControllerParameterPresentationFrame {
    pub(crate) parameter: ControllerParameterQueryFrame,
    pub(crate) plain: f64,
    pub(crate) display: [u16; 128],
    pub(crate) display_available: bool,
}

impl Default for ControllerParameterPresentationFrame {
    fn default() -> Self {
        Self {
            parameter: ControllerParameterQueryFrame::default(),
            plain: f64::NAN,
            display: [0; 128],
            display_available: false,
        }
    }
}

impl ControllerParameterPresentationFrame {
    pub(crate) fn observation(&self) -> ControllerParameterPresentationRef<'_> {
        ControllerParameterPresentationRef { frame: self }
    }
}

/// Borrowed outputs of actual native plain conversion and optional formatting.
/// These facts are presentation only, never an author scalar or a DSP delivery.
#[derive(Clone, Copy)]
pub struct ControllerParameterPresentationRef<'a> {
    frame: &'a ControllerParameterPresentationFrame,
}

impl<'a> ControllerParameterPresentationRef<'a> {
    /// The actual metadata and normalized current-value observation.
    pub fn parameter(self) -> ControllerParameterRef<'a> {
        self.frame.parameter.observation()
    }
    /// Actual normalizedParamToPlain output, if finite; no default or clamp.
    pub fn plain(self) -> Option<f64> {
        self.frame.plain.is_finite().then_some(self.frame.plain)
    }
    /// Actual VST3 visibility flag, with no CLAP flag interpretation.
    pub fn hidden(self) -> bool {
        self.frame.parameter.info.flags
            & vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::kIsHidden
            != 0
    }
    /// Successful getParamStringByValue output, without lossy text conversion.
    /// The consumer must validate bounded UTF-16 and its own text/storage limits.
    pub fn display_utf16(self) -> Option<&'a [u16; 128]> {
        self.frame.display_available.then_some(&self.frame.display)
    }
}
