//! Native editor control data for the same installed processor object.
//! Requests and child acknowledgements are not author or installation grants.

use crate::NativeAudioObjectBindRequest;

/// An owned request to attach the same bound native object's editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioEditorOpenRequest {
    binding: NativeAudioObjectBindRequest,
    title: mondrian_core::NativeAudioEditorTitle,
}

impl NativeAudioEditorOpenRequest {
    /// Carry a title already admitted by the sole Core control-data boundary.
    /// The Runtime separately validates the actual installed owner lease.
    pub fn new(
        binding: NativeAudioObjectBindRequest,
        title: mondrian_core::NativeAudioEditorTitle,
    ) -> Self {
        Self { binding, title }
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
