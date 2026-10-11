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

impl ControllerParameterRef<'_> {
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
