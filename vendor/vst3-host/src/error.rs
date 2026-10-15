//! Error types for the vst3-host library

use thiserror::Error;

/// Native editor teardown step whose ABI callback failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorDetachStage {
    /// The attached view did not confirm removal from its platform parent.
    Removal,
    /// View removal completed, but clearing its host frame did not.
    FrameClear,
}

/// Main error type for vst3-host operations.
///
/// Marked `#[non_exhaustive]`: match with a wildcard arm, as new variants may be added in
/// future versions without it being a breaking change.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Plugin file not found
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),

    /// Failed to load plugin
    #[error("Failed to load plugin: {0}")]
    PluginLoadFailed(String),

    /// Plugin crashed during operation
    #[error("Plugin crashed")]
    PluginCrashed,

    /// Plugin operation timed out
    #[error("Plugin operation timed out")]
    PluginTimeout,

    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    /// Audio backend error
    #[error("Audio backend error: {0}")]
    AudioBackendError(String),

    /// MIDI error
    #[error("MIDI error: {0}")]
    MidiError(String),

    /// COM/VST3 interface error
    #[error("VST3 interface error: {0}")]
    InterfaceError(String),

    /// The embedding host did not declare this plug-in interface consumed.
    #[error("host does not consume plug-in interface {0:?}")]
    InterfaceNotConsumed(crate::PluginInterface),

    /// Process isolation error
    #[error("Process isolation error: {0}")]
    ProcessError(String),

    /// IO error
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    /// The plugin's `process()` returned a failure code. Carries the raw tresult rather than a
    /// formatted `String` so returning it from the audio callback allocates nothing.
    #[error("Plugin process() failed: {0:#x}")]
    ProcessFailed(i32),

    /// The plugin is not currently active/processing. A unit variant for the same reason as
    /// [`Self::ProcessFailed`] — this is rejected on the audio path once per block while stopped.
    #[error("Plugin is not processing")]
    NotProcessing,

    /// The actual controller does not provide a native editor view.
    #[error("native editor view is unavailable")]
    EditorUnavailable,

    /// The actual view does not support this host platform's parent type.
    #[error("native editor view does not support this platform")]
    EditorPlatformUnsupported,

    /// The actual view refused a required teardown step. Its native owner must
    /// remain alive; a later close cannot pretend this failure removed the view.
    #[error("native editor {stage:?} failed with {result:#x}")]
    EditorDetachFailed {
        /// Exact failed native lifecycle step.
        stage: EditorDetachStage,
        /// Actual native tresult returned by that callback.
        result: i32,
    },

    /// Failed teardown prohibits live UI actions or a fresh attachment.
    #[error("native editor lifecycle is unresolved")]
    EditorLifecycleBlocked,

    /// A native parent was given a different actual object attachment owner.
    #[error(
        "native editor parent and temporary controller role belong to different native objects"
    )]
    EditorAttachmentOwnerMismatch,

    /// A host-owned storage family could not be accounted or admitted.
    #[error(transparent)]
    HostStorage(#[from] crate::host_storage::HostStorageError),

    /// Existing host input must reach a real DSP call before state mutation/capture.
    #[error("native processor parameter input is still pending")]
    PendingParameterDelivery,

    /// Other errors
    #[error("{0}")]
    Other(String),
}

/// Convenient Result type alias
pub type Result<T> = std::result::Result<T, Error>;
