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

/// Fixed, allocation-free refusals for the bounded native event API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeEventError {
    /// This backend does not implement this capability.
    UnsupportedBackend,
    /// A control operation was attempted away from the initializing thread.
    WrongThread,
    /// Metadata cannot be queried while processing.
    QueryWhileProcessing,
    /// A native direction reported an invalid or excessive count.
    BusCount {
        /// Native direction which reported this count.
        direction: crate::audio::BusDirection,
        /// Raw SDK count, preserved without a cast.
        count: i32,
    },
    /// The native metadata call or its returned scalar fields are invalid.
    BusMetadata {
        /// Direction of the requested native bus.
        direction: crate::audio::BusDirection,
        /// Direction-local native index.
        index: i32,
        /// Raw getBusInfo/activation result.
        result: i32,
    },
    /// Native topology no longer matches this owner's retained activation evidence.
    TopologyChanged {
        /// Direction whose retained declaration no longer matches.
        direction: crate::audio::BusDirection,
    },
    /// The index is outside the same count validated by the complete census.
    BusIndex {
        /// Direction-local requested index.
        direction: crate::audio::BusDirection,
        /// Original signed requested index.
        index: i32,
        /// Count of retained buses matched by that census.
        count: usize,
    },
    /// The component did not acknowledge its cold active-state transition.
    ComponentActivation {
        /// Requested component active state.
        active: bool,
        /// Raw SDK return code.
        result: i32,
    },
    /// A validated native event bus did not acknowledge activation.
    BusActivation {
        /// Native bus direction.
        direction: crate::audio::BusDirection,
        /// Validated native index.
        index: i32,
        /// Raw SDK return code.
        result: i32,
    },
    /// Bounded preparation storage could not be reserved.
    Allocation,
    /// No successful preparation/start has validated the current event topology.
    PreparationRequired,
    /// The caller's complete audio window has an invalid extent.
    InvalidFrames,
    /// The caller's rate differs from the prepared native rate.
    SampleRate,
    /// Audio bus/channel/activation metadata differs from the native buffers.
    AudioLayout,
    /// The indexed event is outside this explicitly supported Note subset.
    InvalidEvent {
        /// Position in the caller's original batch.
        index: usize,
    },
    /// An indexed event precedes an earlier event in the supplied batch.
    EventOrder {
        /// First batch position which precedes its predecessor.
        index: usize,
    },
    /// Existing input/chunk events belong to another pending producer.
    PendingEvents,
    /// The original preallocated input/chunk storage cannot hold the batch.
    Capacity,
    /// The original event-list mutex is poisoned.
    StoragePoisoned,
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

    /// Structured refusal from the bounded same-instance event API.
    #[error("native event operation failed: {0:?}")]
    NativeEvent(NativeEventError),

    /// Other errors
    #[error("{0}")]
    Other(String),
}

/// Convenient Result type alias
pub type Result<T> = std::result::Result<T, Error>;
