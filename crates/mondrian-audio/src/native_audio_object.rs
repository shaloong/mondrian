//! Native object facts from the sole authenticated child, distinct from author permission.

use mondrian_core::AudioProcessorBindingId;
use std::num::{NonZeroU32, NonZeroU64};

/// Actual native object load observed inside one exclusive child process.
///
/// PID alone, class ID, COM address and authored processor ID do not identify a
/// load. The parent additionally retains the exact launched process/control
/// handle and checks PID against that handle. This observation cannot install a
/// root, mint an editor lease, authorize an author write or preserve private DSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioObjectObservation {
    process_id: NonZeroU32,
    object_serial: NonZeroU64,
}

impl NativeAudioObjectObservation {
    /// Describe bounded actual child facts; this constructor grants no permission.
    pub const fn new(process_id: NonZeroU32, object_serial: NonZeroU64) -> Self {
        Self { process_id, object_serial }
    }
    /// Process that owns the actual loaded object, checked against the supervised child.
    pub const fn process_id(self) -> NonZeroU32 {
        self.process_id
    }
    /// Non-wrapping process-local load serial emitted by the actual native adapter.
    pub const fn object_serial(self) -> NonZeroU64 {
        self.object_serial
    }
}

/// Runtime-issued exact binding request sent on the existing child owner stream.
///
/// The child checks the expected native object before recording the binding. This
/// request has no root/author installation authority on its own. The Runtime
/// publishes a lease only after current-install verification and every required
/// root/nested binding acknowledgement succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioObjectBindRequest {
    /// The binding incarnation issued by the sole current Runtime.
    pub binding: AudioProcessorBindingId,
    /// The actual prepared/entered object expected on that retained child.
    pub expected_object: NativeAudioObjectObservation,
}

/// Safe native bind refusal; a valid stopped child remains retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioObjectBindRefusal {
    /// The concrete native adapter does not expose an editor object capability.
    UnsupportedAdapter,
    /// An intervening actual load or object retirement changed the expected owner.
    ObjectChanged,
    /// An unresolved view or incomplete author stream must settle before rebinding.
    EditorStatePending,
}

/// Exact successful child acknowledgement, not a root installation or author lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioObjectBindReceipt {
    binding: AudioProcessorBindingId,
    object: NativeAudioObjectObservation,
}

impl NativeAudioObjectBindReceipt {
    /// Exact Runtime request acknowledged by the same actual child object.
    pub const fn binding(self) -> AudioProcessorBindingId {
        self.binding
    }
    /// Actual native object retained by the matching supervised owner.
    pub const fn object(self) -> NativeAudioObjectObservation {
        self.object
    }

    pub(crate) fn acknowledge(
        request: NativeAudioObjectBindRequest,
        actual: NativeAudioObjectObservation,
    ) -> Result<Self, NativeAudioObjectBindRefusal> {
        if actual != request.expected_object {
            return Err(NativeAudioObjectBindRefusal::ObjectChanged);
        }
        Ok(Self { binding: request.binding, object: actual })
    }
}

/// Bounded outcome of binding the one actual native object on its owner thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioObjectBindOutcome {
    /// The child acknowledged the exact object and Runtime-issued binding.
    Bound(NativeAudioObjectBindReceipt),
    /// No root/editor permission was published; the valid child remains retained.
    Refused(NativeAudioObjectBindRefusal),
}
