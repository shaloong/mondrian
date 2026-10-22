//! Native editor control data for the same installed processor object.
//! Requests and child acknowledgements are not author or installation grants.

use crate::NativeAudioObjectBindRequest;

/// An owned request to attach the same bound native object's editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioEditorOpenRequest {
    binding: NativeAudioObjectBindRequest,
    title: mondrian_core::NativeAudioEditorTitle,
    controller_values: Vec<(u32, u64)>,
}

impl NativeAudioEditorOpenRequest {
    /// Carry a title already admitted by the sole Core control-data boundary.
    /// The Runtime separately validates the actual installed owner lease.
    pub fn new(
        binding: NativeAudioObjectBindRequest,
        title: mondrian_core::NativeAudioEditorTitle,
    ) -> Self {
        Self { binding, title, controller_values: Vec::new() }
    }
    /// One non-RT request owner, moved from the installed Plan's constant Some
    /// lanes. The child separately owns/charges its decoded copy while this Vec
    /// is live. This local frontier excludes Plan/native-private allocations and
    /// allocator bookkeeping; it does not alter the processor B*L budget.
    pub(crate) fn with_controller_values(
        mut self,
        values: Vec<(u32, u64)>,
    ) -> Result<Self, crate::AudioProcessorHostError> {
        if values.len() > NATIVE_EDITOR_CONTROLLER_VALUES
            || values.iter().enumerate().any(|(index, (id, bits))| {
                !f64::from_bits(*bits).is_finite()
                    || values[..index].iter().any(|(previous, _)| previous == id)
            })
        {
            return Err(crate::AudioProcessorHostError::InvalidContract(
                "invalid native editor controller value selection".to_owned(),
            ));
        }
        self.controller_values = values;
        self.owned_layout()?;
        Ok(self)
    }
    pub(crate) fn controller_values(&self) -> &[(u32, u64)] {
        &self.controller_values
    }
    pub(crate) fn owned_layout(
        &self,
    ) -> Result<crate::NativeHostBufferLayout, crate::AudioProcessorHostError> {
        controller_value_layout(self.title().len(), self.controller_values.capacity())
    }
    /// Exact Runtime-issued binding and observed native object.
    pub const fn binding(&self) -> NativeAudioObjectBindRequest {
        self.binding
    }
    /// Product title displayed by the real parent window.
    pub fn title(&self) -> &str {
        self.title.as_str()
    }
}

/// Typed refusal keeps a valid retained processor alive without opening a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorRefusal {
    /// This adapter has no actual native editor implementation.
    UnsupportedAdapter,
    /// No currently installed Runtime/Hosted lease authorizes this request.
    NotInstalled,
    /// Actual child object/load serial or its acknowledged binding changed.
    OwnerChanged,
    /// This actual component/controller does not supply an editor.
    NoEditor,
    /// Configuration/author state must be coherently completed first.
    ConfigurationPending,
    /// A title exceeded the exact transport extent or contains NUL.
    InvalidTitle,
}

/// Exact incomplete native detach step observed from the sole attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorCloseStage {
    /// The still-attached native view refused removal.
    RemoveView,
    /// Removal succeeded but the view still holds its host frame.
    ClearHostFrame,
}

/// One acknowledged same-object editor control result; no author history effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorOutcome {
    /// The actual retained controller's view is attached to its real parent.
    Opened {
        /// Exact child binding acknowledged by the request.
        binding: NativeAudioObjectBindRequest,
        /// Actual admitted native view dimensions.
        width: u32,
        /// Actual admitted native view dimensions.
        height: u32,
    },
    /// The same object's view/frame/parent are confirmed closed.
    Closed {
        /// Exact child binding acknowledged by the request.
        binding: NativeAudioObjectBindRequest,
    },
    /// Safe control refusal; the DSP object is not retired.
    Refused(NativeAudioEditorRefusal),
    /// A valid native object retains its actual unfinished attachment step.
    /// Removal failure requires isolated retirement without another removed
    /// callback; frame cleanup alone may retry. No successful close is forged.
    ClosePending {
        /// Exact child binding with the unresolved native attachment.
        binding: NativeAudioObjectBindRequest,
        /// The real failed lifecycle step.
        stage: NativeAudioEditorCloseStage,
    },
}

// An Open request has at most the already admitted full VST3 census. This is
// unrelated to selected Fact limits and is not a second parameter directory.
pub(crate) const NATIVE_EDITOR_CONTROLLER_VALUES: usize = 4096;
pub(crate) fn controller_value_layout(
    title_bytes: usize,
    capacity: usize,
) -> Result<crate::NativeHostBufferLayout, crate::AudioProcessorHostError> {
    use std::alloc::Layout;
    let invalid = || {
        crate::AudioProcessorHostError::InvalidContract(
            "native editor controller request exceeds its local storage frontier".to_owned(),
        )
    };
    let values = Layout::array::<(u32, u64)>(capacity).map_err(|_| invalid())?;
    let maximum = Layout::array::<(u32, u64)>(NATIVE_EDITOR_CONTROLLER_VALUES)
        .map_err(|_| invalid())?
        .size()
        .checked_add(mondrian_core::NATIVE_AUDIO_EDITOR_TITLE_BYTES)
        .ok_or_else(invalid)?;
    let bytes = values.size().checked_add(title_bytes).ok_or_else(invalid)?;
    if title_bytes > mondrian_core::NATIVE_AUDIO_EDITOR_TITLE_BYTES
        || capacity > NATIVE_EDITOR_CONTROLLER_VALUES
        || bytes > maximum
    {
        return Err(invalid());
    }
    Ok(crate::NativeHostBufferLayout {
        bytes,
        alignment: if capacity == 0 { 1 } else { values.align() },
    })
}
