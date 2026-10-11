//! Bounded local host allocation snapshots, separate from full native admission.

use crate::NativeHostBufferLayout;
use std::alloc::{alloc, alloc_zeroed, Layout};
use std::marker::PhantomData;
use std::ptr;

mod sealed {
    /// Only complete standard allocation owners may issue a local borrow.
    pub trait AllocationOwner {}
    impl<T> AllocationOwner for Vec<T> {}
    impl AllocationOwner for String {}
    impl<T: ?Sized> AllocationOwner for Box<T> {}
}

/// An actual full local allocation owner that can expose its requested layout.
///
/// Implementations are sealed: a borrowed subslice, an SDK handle or an Arc
/// payload is not evidence of a complete physical allocation. Nested element
/// allocations must be reported separately. The immutable borrow prevents
/// resizing or dropping this owner while its local ledger remains live.
pub trait NativeHostAllocationOwner: sealed::AllocationOwner {
    /// Borrow the actual allocation base and its complete requested layout.
    fn host_allocation(&self) -> Result<NativeHostAllocationBorrow<'_>, NativeHostLedgerError>;
}

/// One borrowed complete allocation in the current host address space.
///
/// Identity is the physical allocation base, not contents or author identity.
/// Zero-sized storage has no allocation and is ignored by the ledger. Foreign
/// worker receipts must use their own ledger and launch identity; equal virtual
/// addresses from different processes cannot be merged into this local union.
#[derive(Debug, Clone, Copy)]
pub struct NativeHostAllocationBorrow<'a> {
    address: usize,
    layout: Layout,
    _owner: PhantomData<&'a ()>,
}

impl NativeHostAllocationBorrow<'_> {
    /// Complete allocation identity in this address space, not dereference authority.
    pub const fn address(self) -> usize {
        self.address
    }
    /// Exact allocator-requested complete layout supplied by the owning container.
    pub const fn layout(self) -> Layout {
        self.layout
    }
}

impl<T> NativeHostAllocationOwner for Vec<T> {
    fn host_allocation(&self) -> Result<NativeHostAllocationBorrow<'_>, NativeHostLedgerError> {
        let layout = Layout::array::<T>(self.capacity())
            .map_err(|_| NativeHostLedgerError::ExtentOverflow)?;
        Ok(NativeHostAllocationBorrow {
            address: self.as_ptr().cast::<()>() as usize,
            layout,
            _owner: PhantomData,
        })
    }
}

impl NativeHostAllocationOwner for String {
    fn host_allocation(&self) -> Result<NativeHostAllocationBorrow<'_>, NativeHostLedgerError> {
        let layout = Layout::array::<u8>(self.capacity())
            .map_err(|_| NativeHostLedgerError::ExtentOverflow)?;
        Ok(NativeHostAllocationBorrow {
            address: self.as_ptr().cast::<()>() as usize,
            layout,
            _owner: PhantomData,
        })
    }
}

impl<T: ?Sized> NativeHostAllocationOwner for Box<T> {
    fn host_allocation(&self) -> Result<NativeHostAllocationBorrow<'_>, NativeHostLedgerError> {
        let layout = Layout::for_value(self.as_ref());
        let address = ptr::from_ref(self.as_ref()).cast::<()>() as usize;
        Ok(NativeHostAllocationBorrow { address, layout, _owner: PhantomData })
    }
}

/// Failure to construct or complete one exact bounded local allocation snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeHostLedgerError {
    /// The allocation table or byte sum exceeds a representable layout.
    #[error("native host allocation extent overflowed")]
    ExtentOverflow,
    /// The exact known live union exceeds its explicit byte grant.
    #[error("native host allocation union requires {required} bytes but was granted {granted}")]
    GrantExceeded {
        /// Complete currently observed known allocation bytes.
        required: usize,
        /// Explicit overlapping known-host grant.
        granted: usize,
    },
    /// No more distinct owners fit the preallocated receipt table.
    #[error("native host allocation receipt exceeds its {limit} owner limit")]
    OwnerLimitExceeded {
        /// Exact maximum number of external owners admitted by this table.
        limit: usize,
    },
    /// Identical allocation identity was reported with incompatible layouts.
    #[error("native host allocation identity has conflicting layouts")]
    ConflictingLayout,
    /// The exact already admitted bookkeeping allocation was unavailable.
    #[error("native host allocation ledger storage is unavailable")]
    AllocationUnavailable,
}

// Every bit pattern of these three usize fields is valid. Unused slots stay
// zeroed; only the initialized logical prefix is inspected as a receipt.
#[derive(Clone, Copy)]
struct AllocationSlot {
    address: usize,
    bytes: usize,
    alignment: usize,
}

/// Exact layouts of the ledger's own two allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHostLedgerStorage {
    /// Boxed ledger body, including its inline metadata.
    pub owner: NativeHostBufferLayout,
    /// Fixed complete receipt table; zero slots require no table allocation.
    pub table: NativeHostBufferLayout,
}

impl NativeHostLedgerStorage {
    /// Checked total requested bytes, without allocator bookkeeping or OS pages.
    pub fn bytes(self) -> Result<usize, NativeHostLedgerError> {
        self.owner
            .bytes
            .checked_add(self.table.bytes)
            .ok_or(NativeHostLedgerError::ExtentOverflow)
    }
}

/// One bounded non-RT union of borrowed actual local allocations.
///
/// The owner body and fixed table are themselves charged. Recording and sealing
/// allocate nothing and never grow the table. Any ignored recording error is
/// sticky and prevents a truncated receipt from being published. This ledger
/// proves only the reported subset, not complete SDK, process or candidate
/// admission. Missing host owner seams cannot be treated as zero.
pub struct NativeHostAllocationLedger<'a> {
    slots: Box<[AllocationSlot]>,
    used: usize,
    bytes: usize,
    grant: usize,
    failure: Option<NativeHostLedgerError>,
    storage: NativeHostLedgerStorage,
    _owners: PhantomData<&'a ()>,
}

impl<'a> NativeHostAllocationLedger<'a> {
    /// Exact allocator-requested bookkeeping layouts for this owner limit.
    pub fn storage_layout(limit: usize) -> Result<NativeHostLedgerStorage, NativeHostLedgerError> {
        let table = Layout::array::<AllocationSlot>(limit)
            .map_err(|_| NativeHostLedgerError::ExtentOverflow)?;
        let owner = Layout::new::<Self>();
        Ok(NativeHostLedgerStorage {
            owner: NativeHostBufferLayout { bytes: owner.size(), alignment: owner.align() },
            table: NativeHostBufferLayout { bytes: table.size(), alignment: table.align() },
        })
    }

    /// Allocate exact bounded bookkeeping after checking its incremental peak.
    ///
    /// The grant is a snapshot ceiling for the complete reported local union,
    /// including these allocations. It does not authorize unreported candidate
    /// construction or substitute for root old/new/temporary resource admission.
    pub fn new(limit: usize, grant: usize) -> Result<Box<Self>, NativeHostLedgerError> {
        let storage = Self::storage_layout(limit)?;
        let bytes = storage.bytes()?;
        if bytes > grant {
            return Err(NativeHostLedgerError::GrantExceeded { required: bytes, granted: grant });
        }
        let slots = if limit == 0 {
            Box::default()
        } else {
            let layout = Layout::array::<AllocationSlot>(limit)
                .map_err(|_| NativeHostLedgerError::ExtentOverflow)?;
            // SAFETY: layout describes exactly limit slots of three usize fields.
            // Zero bytes are valid initialized slots. Box owns the same complete
            // allocation and deallocates it with the identical slice layout.
            let raw = unsafe { alloc_zeroed(layout) }.cast::<AllocationSlot>();
            if raw.is_null() {
                return Err(NativeHostLedgerError::AllocationUnavailable);
            }
            // SAFETY: the nonnull zeroed allocation contains limit valid slots.
            unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(raw, limit)) }
        };
        let layout = Layout::new::<Self>();
        // SAFETY: this nonzero layout is the complete ledger body. A null result
        // is handled before initialization and the already owned table is dropped.
        let raw = unsafe { alloc(layout) }.cast::<Self>();
        if raw.is_null() {
            return Err(NativeHostLedgerError::AllocationUnavailable);
        }
        let value = Self {
            slots,
            used: 0,
            bytes,
            grant,
            failure: None,
            storage,
            _owners: PhantomData,
        };
        // SAFETY: raw is unique, aligned and sized for Self. The complete valid
        // value is initialized before Box assumes ownership; it drops once.
        unsafe {
            raw.write(value);
            Ok(Box::from_raw(raw))
        }
    }

    /// Record both actual SDK-owned CLAP port buffer allocations in this local union.
    ///
    /// The canonical SDK exposes complete borrowed capacity/layout facts from the
    /// owning implementation. It exposes no samples or uninitialized spare data.
    /// The inline port owner and any referenced PCM storage remain separate owners.
    pub fn record_clap_audio_ports(
        &mut self,
        ports: &'a clack_host::process::audio_buffers::AudioPorts,
    ) -> Result<(), NativeHostLedgerError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let storage = match ports.allocated_storage() {
            Ok(storage) => storage,
            Err(_) => {
                self.failure = Some(NativeHostLedgerError::ExtentOverflow);
                return Err(NativeHostLedgerError::ExtentOverflow);
            }
        };
        for allocation in storage {
            self.record(NativeHostAllocationBorrow {
                address: allocation.allocation_base() as usize,
                layout: allocation.allocation_layout(),
                _owner: PhantomData,
            })?;
        }
        Ok(())
    }

    /// Record one complete allocation; aliases of the same owner are charged once.
    ///
    /// The first rejection remains sticky. No caller can ignore one overflow and
    /// subsequently seal a smaller successful receipt from the same attempt.
    pub fn record(
        &mut self,
        allocation: NativeHostAllocationBorrow<'a>,
    ) -> Result<(), NativeHostLedgerError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.record_inner(allocation);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn record_inner(
        &mut self,
        allocation: NativeHostAllocationBorrow<'a>,
    ) -> Result<(), NativeHostLedgerError> {
        let bytes = allocation.layout.size();
        if bytes == 0 {
            return Ok(());
        }
        if let Some(existing) =
            self.slots[..self.used].iter().find(|slot| slot.address == allocation.address)
        {
            return if existing.bytes == bytes && existing.alignment == allocation.layout.align() {
                Ok(())
            } else {
                Err(NativeHostLedgerError::ConflictingLayout)
            };
        }
        let total = self.bytes.checked_add(bytes).ok_or(NativeHostLedgerError::ExtentOverflow)?;
        if total > self.grant {
            return Err(NativeHostLedgerError::GrantExceeded {
                required: total,
                granted: self.grant,
            });
        }
        if self.used == self.slots.len() {
            return Err(NativeHostLedgerError::OwnerLimitExceeded { limit: self.slots.len() });
        }
        self.slots[self.used] = AllocationSlot {
            address: allocation.address,
            bytes,
            alignment: allocation.layout.align(),
        };
        self.used += 1;
        self.bytes = total;
        Ok(())
    }

    /// Seal this observed subset and retire the receipt bookkeeping allocations.
    ///
    /// The returned bytes describe the peak while the full table and all borrowed
    /// owners overlapped, not a continuing lease. Root installation still needs
    /// complete owner coverage, exact generation/cohort validation and an atomic
    /// install claim before owners can be resized, released or transferred.
    pub fn finish(self) -> Result<NativeHostLocalPeakSnapshot, NativeHostLedgerError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        Ok(NativeHostLocalPeakSnapshot {
            allocation_bytes: self.bytes,
            allocations: self.used + 1 + usize::from(!self.slots.is_empty()),
            ledger_storage: self.storage,
            process_id: std::process::id(),
        })
    }
}

/// Known reported local allocation peak, with no complete-coverage assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHostLocalPeakSnapshot {
    allocation_bytes: usize,
    allocations: usize,
    ledger_storage: NativeHostLedgerStorage,
    process_id: u32,
}

impl NativeHostLocalPeakSnapshot {
    /// Requested bytes of this reported union, including the ledger at its peak.
    pub const fn allocation_bytes(self) -> usize {
        self.allocation_bytes
    }
    /// Distinct allocations, including bookkeeping; zero-sized owners are absent.
    pub const fn allocations(self) -> usize {
        self.allocations
    }
    /// Separately attributable exact ledger layouts that overlapped all owners.
    pub const fn ledger_storage(self) -> NativeHostLedgerStorage {
        self.ledger_storage
    }
    /// Address space observed here; cross-process physical identities remain separate.
    pub const fn process_id(self) -> u32 {
        self.process_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_host_ledger_conflicting_layout_is_sticky_without_reading_foreign_memory() {
        let vector = Vec::<u64>::with_capacity(3);
        let receipt = vector.host_allocation().expect("actual owner");
        let mut ledger = NativeHostAllocationLedger::new(1, usize::MAX).expect("table");
        ledger.record(receipt).expect("actual layout");
        // Negative private-protocol qualification: this deliberately malformed
        // duplicate is never dereferenced and cannot be issued by the sealed API.
        let malformed = NativeHostAllocationBorrow {
            address: receipt.address,
            layout: Layout::new::<u8>(),
            _owner: PhantomData,
        };
        assert_eq!(
            ledger.record(malformed),
            Err(NativeHostLedgerError::ConflictingLayout)
        );
        assert_eq!(
            ledger.record(receipt),
            Err(NativeHostLedgerError::ConflictingLayout)
        );
        assert_eq!(
            ledger.finish(),
            Err(NativeHostLedgerError::ConflictingLayout)
        );
    }

    #[test]
    fn native_host_ledger_checked_byte_sum_cannot_wrap_into_a_smaller_receipt() {
        let vector = vec![1u8; 3];
        let mut ledger = NativeHostAllocationLedger::new(1, usize::MAX).expect("table");
        // Arithmetic negative qualification only, not a claim that this
        // synthetic total was actually allocated in the current address space.
        ledger.bytes = usize::MAX;
        assert_eq!(
            ledger.record(vector.host_allocation().expect("live owner")),
            Err(NativeHostLedgerError::ExtentOverflow)
        );
        assert_eq!(ledger.finish(), Err(NativeHostLedgerError::ExtentOverflow));
    }
}
