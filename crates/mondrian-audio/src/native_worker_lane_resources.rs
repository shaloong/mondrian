//! Known additional storage requests of the native main/reader/DSP lane.
//!
//! These requests are one input to the sole prepared Session admission. They
//! are neither a complete host-allocation receipt nor installed-owner authority.
//! Standard-library thread/TLS allocations, allocator metadata, OS/kernel
//! ownership and plugin-private memory require separate evidence.

use crate::NativeHostBufferLayout;
use std::alloc::Layout;

/// Requested reservation for the one scoped native command reader stack.
pub const SCOPED_NATIVE_READER_STACK_BYTES: usize = 2 * 1024 * 1024;
/// Requested reservation for the one scoped native DSP stack.
pub const SCOPED_NATIVE_DSP_STACK_BYTES: usize = 8 * 1024 * 1024;

/// A fixed known storage owner in one scoped native main/DSP implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWorkerLaneStorageKind {
    /// The complete retained concrete Adapter Worker Box allocation.
    AdapterResident,
    /// Main-owned command and reply acknowledgement slots.
    ReaderSlots,
    /// Main-owned job, completion and readiness slots.
    ProcessingSlots,
    /// Other explicitly measured inline main-role transport storage.
    MainInline,
    /// The complete concrete Audio main-role Box allocation.
    RuntimeMainRole,
    /// The complete concrete Audio DSP-role Box allocation.
    RuntimeDspRole,
    /// The complete concrete SDK editor-fields Box allocation.
    SdkMainRole,
    /// The complete concrete SDK process-fields Box allocation.
    SdkDspRole,
    /// Reserved element storage for the one decoded input message.
    DecodedInput,
    /// Reserved element storage for its simultaneously live response.
    ReplyStorage,
    /// Reserved element storage for an additional staged configuration seed.
    StagedSeed,
}

/// Actual requested layouts of the eight fixed known lane storage owners.
///
/// Construct these from the real owning types' `Layout::new`, or the SDK's
/// complete Box-owner observer. A trait-object pointer layout is insufficient.
/// The main inline storage is a separately identified logical stack obligation;
/// it is not an additional allocation inside the two requested worker stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeWorkerLaneOwnedLayouts {
    /// Complete retained Adapter Worker header, including its inline metadata.
    /// Nested allocations are separate owners and are not covered by this layout.
    pub adapter_resident: NativeHostBufferLayout,
    /// Combined requested layouts of the actual reader/ack slot values.
    pub reader_slots: NativeHostBufferLayout,
    /// Combined requested layouts of the actual job/completion/ready values.
    pub processing_slots: NativeHostBufferLayout,
    /// Additional main-stack values, excluding values counted in the slots.
    pub main_inline: NativeHostBufferLayout,
    /// Complete concrete runtime main-role allocation.
    pub runtime_main_role: NativeHostBufferLayout,
    /// Complete concrete runtime DSP-role allocation.
    pub runtime_dsp_role: NativeHostBufferLayout,
    /// Complete concrete SDK editor-fields allocation.
    pub sdk_main_role: NativeHostBufferLayout,
    /// Complete concrete SDK process-fields allocation.
    pub sdk_dsp_role: NativeHostBufferLayout,
}

/// Explicitly admitted element-storage limits for control overlap.
///
/// Only additional owners belong here; an unchanged retained seed must not be
/// charged again under a second identity. These are capacity ceilings, not
/// payload lengths or a claim that the maximum owners are always all live.
/// Each alignment is the maximum required alignment of its command union.
/// Legal observed alignments must divide that ceiling; all eight fixed owners
/// still require exact measured layouts. Actual capacity and alignment remain
/// the owning type's facts and must not be rewritten to fit the ceiling.
/// Descriptor strings, nested Vecs and error Strings need separate receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeWorkerControlOverlapLayout {
    /// One decoded command's capacity and maximum required alignment.
    /// A union of f64 parameter values and byte/string seed/title owners uses
    /// alignment 8 while preserving each observed owner's real alignment.
    pub decoded_input: NativeHostBufferLayout,
    /// The correlated response storage that can overlap the request.
    pub reply_storage: NativeHostBufferLayout,
    /// The additional preowned configuration seed waiting for commit.
    pub staged_seed: NativeHostBufferLayout,
}

/// Failure to form or observe one known scoped-lane resource request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeWorkerLaneResourceError {
    /// A reported complete owner cannot form a legal Rust allocation layout.
    #[error("native lane {owner:?} reports invalid layout {bytes}/{alignment}")]
    InvalidLayout {
        /// Identified known owner.
        owner: NativeWorkerLaneStorageKind,
        /// Requested bytes.
        bytes: usize,
        /// Requested alignment.
        alignment: usize,
    },
    /// The checked sum of known additional requests overflowed.
    #[error("native lane known resource request overflowed")]
    ExtentOverflow,
    /// An actual fixed owner does not match its measured preparation layout.
    #[error("native lane {owner:?} actual layout {observed:?} differs from prepared {prepared:?}")]
    FixedOwnerChanged {
        /// Identified fixed owner.
        owner: NativeWorkerLaneStorageKind,
        /// Preparation-time actual owning-type layout.
        prepared: NativeHostBufferLayout,
        /// Actual complete owner after creation.
        observed: NativeHostBufferLayout,
    },
    /// Actual element-storage capacity exceeds the admitted overlap ceiling.
    #[error(
        "native lane {owner:?} actual capacity layout {observed:?} exceeds prepared {prepared:?}"
    )]
    ControlCapacityExceeded {
        /// Identified control owner.
        owner: NativeWorkerLaneStorageKind,
        /// Admitted owner capacity ceiling.
        prepared: NativeHostBufferLayout,
        /// Actual complete element-storage capacity.
        observed: NativeHostBufferLayout,
    },
}

/// Checked known additional request for one scoped native main/DSP lane.
///
/// Private fields prevent mutation after validation. A decoded request must be
/// compared with the real same-binary adapter profile before loading or spawning.
/// This request has no authority to install a graph or exceed a root grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeWorkerLaneResourceProfile {
    owners: NativeWorkerLaneOwnedLayouts,
    control: NativeWorkerControlOverlapLayout,
    processor_input_vector_bytes: usize,
    processor_input_com_payload_bytes: usize,
    known_storage_limit_bytes: usize,
    additional_session_reservation_bytes: usize,
}

impl NativeWorkerLaneResourceProfile {
    /// Form the known request from actual owning-type layouts and codec ceilings.
    ///
    /// The caller feeds `additional_session_reservation_bytes()` through the
    /// existing processor execution contract and sole root admission. This does
    /// not authorize allocations before that admission.
    pub fn scoped_main_dsp(
        owners: NativeWorkerLaneOwnedLayouts,
        control: NativeWorkerControlOverlapLayout,
    ) -> Result<Self, NativeWorkerLaneResourceError> {
        let mut bytes = 0usize;
        for (kind, layout) in std::iter::once((
            NativeWorkerLaneStorageKind::AdapterResident,
            owners.adapter_resident,
        ))
        .chain(lane_layouts(owners))
        .chain(control_layouts(control))
        {
            validate_layout(kind, layout)?;
            bytes = bytes
                .checked_add(layout.bytes)
                .ok_or(NativeWorkerLaneResourceError::ExtentOverflow)?;
        }
        let additional_session_reservation_bytes = bytes
            .checked_add(SCOPED_NATIVE_READER_STACK_BYTES)
            .and_then(|value| value.checked_add(SCOPED_NATIVE_DSP_STACK_BYTES))
            .ok_or(NativeWorkerLaneResourceError::ExtentOverflow)?;
        Ok(Self {
            owners,
            control,
            processor_input_vector_bytes: 0,
            processor_input_com_payload_bytes: 0,
            known_storage_limit_bytes: bytes,
            additional_session_reservation_bytes,
        })
    }

    /// Add an independently identified persistent processor-input request.
    /// Vector element storage and COM payload coverage remain separate; the
    /// latter does not include the std Arc prefix. This is not control overlap.
    pub fn with_processor_input_storage(
        mut self,
        vector_bytes: usize,
        com_payload_bytes: usize,
    ) -> Result<Self, NativeWorkerLaneResourceError> {
        let old = self
            .processor_input_vector_bytes
            .checked_add(self.processor_input_com_payload_bytes)
            .ok_or(NativeWorkerLaneResourceError::ExtentOverflow)?;
        let new = vector_bytes
            .checked_add(com_payload_bytes)
            .ok_or(NativeWorkerLaneResourceError::ExtentOverflow)?;
        self.known_storage_limit_bytes = self
            .known_storage_limit_bytes
            .checked_sub(old)
            .and_then(|bytes| bytes.checked_add(new))
            .ok_or(NativeWorkerLaneResourceError::ExtentOverflow)?;
        self.additional_session_reservation_bytes = self
            .additional_session_reservation_bytes
            .checked_sub(old)
            .and_then(|bytes| bytes.checked_add(new))
            .ok_or(NativeWorkerLaneResourceError::ExtentOverflow)?;
        self.processor_input_vector_bytes = vector_bytes;
        self.processor_input_com_payload_bytes = com_payload_bytes;
        Ok(self)
    }

    /// Independent persistent input ceilings, not a complete Arc allocation receipt.
    pub const fn processor_input_storage(self) -> (usize, usize) {
        (
            self.processor_input_vector_bytes,
            self.processor_input_com_payload_bytes,
        )
    }

    /// Exact layouts used to construct this validated request.
    pub const fn owning_layouts(self) -> NativeWorkerLaneOwnedLayouts {
        self.owners
    }

    /// Explicit capacity limits for the identified additional control owners.
    pub const fn control_overlap(self) -> NativeWorkerControlOverlapLayout {
        self.control
    }

    /// Sum of the identified known host storage limits, excluding worker stacks.
    ///
    /// This is a reserved ceiling. An actual simultaneously-live allocation
    /// union still needs the canonical ledger and de-duplication evidence.
    pub const fn known_storage_limit_bytes(self) -> usize {
        self.known_storage_limit_bytes
    }

    /// Combined explicit requested reservation of the reader and DSP stacks.
    ///
    /// These are `Builder::stack_size` requests, not observed RSS, committed
    /// pages, kernel/thread bookkeeping or a portable guard-page measurement.
    pub const fn requested_stack_reservation_bytes(self) -> usize {
        SCOPED_NATIVE_READER_STACK_BYTES + SCOPED_NATIVE_DSP_STACK_BYTES
    }

    /// Additional requested ceiling for the existing Session scratch contract.
    ///
    /// This combines identified storage obligations and requested stack
    /// reservations. It is not a complete physical-allocation measurement.
    pub const fn additional_session_reservation_bytes(self) -> usize {
        self.additional_session_reservation_bytes
    }

    /// Check the seven scoped lane owners and actual control Vec capacities.
    ///
    /// Call before scoped thread entry/Ready or publication of a control result.
    /// Failure cannot be hidden by counting lengths or dropping the excess from
    /// a receipt. No allocation or locking occurs in this comparison.
    /// The retained Worker header is not borrowed by these scoped roles. Its
    /// `adapter_resident` field is ignored here, never reported as an actual
    /// observation; startup separately calls `admit_adapter_resident` on the
    /// same Box that remains owned throughout all subsequent role borrows.
    pub fn admit_observed_lane_owners(
        self,
        owners: NativeWorkerLaneOwnedLayouts,
        control: NativeWorkerControlOverlapLayout,
    ) -> Result<(), NativeWorkerLaneResourceError> {
        for ((kind, prepared), (_, observed)) in
            lane_layouts(self.owners).into_iter().zip(lane_layouts(owners))
        {
            validate_layout(kind, observed)?;
            if observed != prepared {
                return Err(NativeWorkerLaneResourceError::FixedOwnerChanged {
                    owner: kind,
                    prepared,
                    observed,
                });
            }
        }
        for ((kind, prepared), (_, observed)) in
            control_layouts(self.control).into_iter().zip(control_layouts(control))
        {
            validate_layout(kind, observed)?;
            if observed.bytes > prepared.bytes || prepared.alignment % observed.alignment != 0 {
                return Err(NativeWorkerLaneResourceError::ControlCapacityExceeded {
                    owner: kind,
                    prepared,
                    observed,
                });
            }
        }
        Ok(())
    }

    /// Verify the complete concrete retained Worker Box before Ready.
    ///
    /// Derive this layout from `Layout::for_value` on the actual boxed worker,
    /// not the trait-object pointer or a scoped role. This does not observe any
    /// nested heap allocation or any of the separately borrowed lane owners.
    pub fn admit_adapter_resident(
        self,
        observed: NativeHostBufferLayout,
    ) -> Result<(), NativeWorkerLaneResourceError> {
        let owner = NativeWorkerLaneStorageKind::AdapterResident;
        validate_layout(owner, observed)?;
        let prepared = self.owners.adapter_resident;
        if observed != prepared {
            return Err(NativeWorkerLaneResourceError::FixedOwnerChanged {
                owner,
                prepared,
                observed,
            });
        }
        Ok(())
    }
}

fn validate_layout(
    kind: NativeWorkerLaneStorageKind,
    layout: NativeHostBufferLayout,
) -> Result<(), NativeWorkerLaneResourceError> {
    Layout::from_size_align(layout.bytes, layout.alignment)
        .map(|_| ())
        .map_err(|_| NativeWorkerLaneResourceError::InvalidLayout {
            owner: kind,
            bytes: layout.bytes,
            alignment: layout.alignment,
        })
}

fn lane_layouts(
    owners: NativeWorkerLaneOwnedLayouts,
) -> [(NativeWorkerLaneStorageKind, NativeHostBufferLayout); 7] {
    use NativeWorkerLaneStorageKind::*;
    [
        (ReaderSlots, owners.reader_slots),
        (ProcessingSlots, owners.processing_slots),
        (MainInline, owners.main_inline),
        (RuntimeMainRole, owners.runtime_main_role),
        (RuntimeDspRole, owners.runtime_dsp_role),
        (SdkMainRole, owners.sdk_main_role),
        (SdkDspRole, owners.sdk_dsp_role),
    ]
}

fn control_layouts(
    control: NativeWorkerControlOverlapLayout,
) -> [(NativeWorkerLaneStorageKind, NativeHostBufferLayout); 3] {
    use NativeWorkerLaneStorageKind::*;
    [
        (DecodedInput, control.decoded_input),
        (ReplyStorage, control.reply_storage),
        (StagedSeed, control.staged_seed),
    ]
}
