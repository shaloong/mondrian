//! Bounded component/controller configuration capture, distinct from DSP history.

/// The native state writer whose result could not be captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationStatePart {
    /// IComponent configuration.
    Component,
    /// Separate IEditController configuration.
    Controller,
}

/// Native lifecycle operation whose actual result is retained on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationLifecyclePhase {
    /// Optional IAudioProcessor::setProcessing(false) notification.
    StopProcessing,
    /// IComponent::setActive(false).
    Deactivate,
    /// IComponent::setActive(true).
    Activate,
}

/// Structured refusal or failure from the dedicated bounded capture operation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationCaptureError {
    /// This adapter or native component provides no reusable configuration state.
    #[error("bounded native configuration state is unavailable")]
    Unavailable,
    /// The caller's state limit cannot hold the canonical state envelope.
    #[error("invalid bounded configuration limit: {limit} bytes")]
    InvalidLimit {
        /// Requested state envelope limit.
        limit: usize,
    },
    /// The additional capture buffers exceed the caller's admitted buffer grant.
    #[error("configuration capture needs {required} buffer bytes, grant is {granted}")]
    BufferGrantExceeded {
        /// Required overlapping capture/envelope buffer bytes.
        required: usize,
        /// Incremental buffer grant, excluding the retained worker baseline.
        granted: usize,
    },
    /// A native writer exceeded its fixed write or seek extent.
    #[error("native configuration exceeds its {limit}-byte state extent")]
    StateLimitExceeded {
        /// Extent of the refused state stream.
        limit: usize,
    },
    /// Host allocation failed before invoking the native state writer.
    #[error("configuration capture buffer allocation failed")]
    AllocationFailed,
    /// An allocation returned a greater Rust-visible capacity than admitted.
    #[error("configuration allocation requested {requested} bytes but owns {actual}")]
    AllocationExtent {
        /// Requested fixed buffer bytes.
        requested: usize,
        /// Actual Rust-visible buffer capacity.
        actual: usize,
    },
    /// Buffer arithmetic cannot be represented by the host.
    #[error("configuration capture extent overflow")]
    ExtentOverflow,
    /// A poisoned host buffer cannot safely supply state bytes.
    #[error("configuration capture host buffer is poisoned")]
    PoisonedBuffer,
    /// Bytes were not successfully sealed before envelope publication.
    #[error("configuration capture is not a completed sealed state")]
    InvalidCaptureState,
    /// Native capture is only legal on the owning control thread.
    #[error("configuration capture must run on the component control thread")]
    WrongThread,
    /// Processing must have stopped before this bounded control operation.
    #[error("configuration capture requires a stopped native processor")]
    StillProcessing,
    /// The caller changed setup while preparing an unchanged-profile service.
    #[error("configuration service requires the exact already applied ProcessSetup")]
    ChangedSetup,
    /// A lifecycle call failed; the old DSP contract cannot be resumed safely.
    #[error("native configuration {phase:?} failed: {result:#x}")]
    LifecycleFailure {
        /// Exact SDK lifecycle operation.
        phase: ConfigurationLifecyclePhase,
        /// Actual native SDK result.
        result: i32,
    },
    /// The native state method failed with its actual result code.
    #[error("native {part:?} configuration getState failed: {result:#x}")]
    NativeFailure {
        /// Component or separate controller writer.
        part: ConfigurationStatePart,
        /// Actual SDK tresult.
        result: i32,
    },
    /// The canonical SDK runtime could not create the capture stream interface.
    #[error("native configuration stream interface is unavailable")]
    StreamInterfaceUnavailable,
    /// Host-created stream ownership could not be registered before native use.
    #[error(transparent)]
    HostStorage(#[from] crate::host_storage::HostStorageError),
}

/// Additional host-owned byte buffers that overlap during configuration capture.
///
/// This is a buffer receipt. It does not replace complete retained plugin bridge,
/// descriptor/payload/PCM/COM object or candidate-plan ownership accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigurationCaptureBuffers {
    /// Fixed component and separate controller capture payload allocations.
    pub stream_payload_bytes: usize,
    /// Actual Vec capacity of the resulting canonical state envelope.
    pub envelope_capacity_bytes: usize,
}

impl ConfigurationCaptureBuffers {
    /// Checked peak of these overlapping capture and envelope byte buffers.
    pub fn peak_buffer_bytes(self) -> Option<usize> {
        self.stream_payload_bytes
            .checked_add(self.envelope_capacity_bytes)
    }
}

/// Completed bounded native configuration with independent buffer evidence.
///
/// Standard state contains configuration. This type makes no DSP checkpoint,
/// sample-history or exact offline continuation promise.
pub struct BoundedConfigurationState {
    pub(crate) bytes: Vec<u8>,
    pub(crate) buffers: ConfigurationCaptureBuffers,
}

impl BoundedConfigurationState {
    /// Canonical component/controller state envelope bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Actual byte buffer receipt from this capture, distinct from used length.
    pub const fn buffer_receipt(&self) -> ConfigurationCaptureBuffers {
        self.buffers
    }

    /// Transfer the state buffer without cloning its configuration bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
