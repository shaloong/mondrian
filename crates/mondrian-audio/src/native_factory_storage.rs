//! Actual immutable factory storage, independent of author/install authority.

use crate::{
    AudioProcessorHostError, NativeHostAllocationBorrow, NativeHostAllocationOwner,
    NativeHostLedgerError,
};
use std::alloc::Layout;
use std::ffi::OsString;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

/// Exact coverage of one observed immutable factory owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessorFactoryStorageKind {
    /// Complete allocator-requested Vec/String/Box storage.
    CompleteAllocation,
    /// Actual Arc payload; its std prefix/padding needs separately qualified proof.
    ArcPayload,
    /// Platform encoded PathBuf/OsString storage; exact std source/target proof is separate.
    StdPlatformEncodedBuffer,
}

/// A borrowed actual local owner, with coverage kept distinct from its bytes.
///
/// This metadata cannot mint a policy grant or installed/native author lease.
/// No native state, std private prefix or uninitialized capacity is read. The
/// owner borrow keeps each identity/layout live throughout the observation.
#[derive(Debug, Clone, Copy)]
pub struct AudioProcessorFactoryStorageBorrow<'a> {
    address: usize,
    layout: Layout,
    kind: AudioProcessorFactoryStorageKind,
    complete: Option<NativeHostAllocationBorrow<'a>>,
    owner: PhantomData<&'a ()>,
}

impl<'a> AudioProcessorFactoryStorageBorrow<'a> {
    /// Observe one complete allocation through the sealed owning container API.
    pub fn complete<T: NativeHostAllocationOwner + ?Sized>(
        value: &'a T,
    ) -> Result<Self, AudioProcessorFactoryStorageError> {
        let allocation = value.host_allocation()?;
        Ok(Self {
            address: allocation.address(),
            layout: allocation.layout(),
            kind: AudioProcessorFactoryStorageKind::CompleteAllocation,
            complete: Some(allocation),
            owner: PhantomData,
        })
    }

    /// Observe a real Arc payload without guessing or dereferencing its prefix.
    /// Aliases of the same Arc expose the same payload address and dynamic layout.
    pub fn arc_payload<T: ?Sized>(value: &'a Arc<T>) -> Self {
        Self {
            address: Arc::as_ptr(value).cast::<()>() as usize,
            layout: Layout::for_value(value.as_ref()),
            kind: AudioProcessorFactoryStorageKind::ArcPayload,
            complete: None,
            owner: PhantomData,
        }
    }

    /// Observe actual platform encoded capacity without claiming std qualification.
    pub fn path(value: &'a PathBuf) -> Result<Self, AudioProcessorFactoryStorageError> {
        Self::encoded(
            value.as_os_str().as_encoded_bytes().as_ptr() as usize,
            value.capacity(),
        )
    }

    /// Observe actual platform encoded capacity without interpreting native text.
    pub fn os_string(value: &'a OsString) -> Result<Self, AudioProcessorFactoryStorageError> {
        Self::encoded(
            value.as_os_str().as_encoded_bytes().as_ptr() as usize,
            value.capacity(),
        )
    }

    fn encoded(address: usize, capacity: usize) -> Result<Self, AudioProcessorFactoryStorageError> {
        let layout =
            Layout::array::<u8>(capacity).map_err(|_| NativeHostLedgerError::ExtentOverflow)?;
        Ok(Self {
            address,
            layout,
            kind: AudioProcessorFactoryStorageKind::StdPlatformEncodedBuffer,
            complete: None,
            owner: PhantomData,
        })
    }

    /// Actual allocation or payload identity in the current parent process only.
    pub const fn address(self) -> usize {
        self.address
    }
    /// Complete layout or payload/encoded extent, interpreted by [`Self::kind`].
    pub const fn layout(self) -> Layout {
        self.layout
    }
    /// Coverage that must accompany the observed address and layout.
    pub const fn kind(self) -> AudioProcessorFactoryStorageKind {
        self.kind
    }
    /// A complete borrow is available only for a complete known allocation.
    /// Arc payloads and encoded buffers cannot enter a complete ledger as free/zero.
    pub const fn complete_allocation(self) -> Option<NativeHostAllocationBorrow<'a>> {
        self.complete
    }
}

/// A failed complete immutable factory storage observation.
#[derive(Debug, thiserror::Error)]
pub enum AudioProcessorFactoryStorageError {
    /// This adapter has not implemented its actual nested owner inventory.
    #[error("audio processor factory storage inventory is unavailable")]
    Unavailable,
    /// Complete standard owner layout or local union validation failed.
    #[error(transparent)]
    Owner(#[from] NativeHostLedgerError),
    /// Existing adapter isolation rejected the control-side callback.
    #[error(transparent)]
    Adapter(#[from] AudioProcessorHostError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_factory_storage_complete_owner_keeps_spare_capacity_and_exact_ledger_aliases() {
        let mut value = String::with_capacity(67);
        value.push_str("gain");
        let storage = AudioProcessorFactoryStorageBorrow::complete(&value).expect("actual owner");
        assert_eq!(storage.layout().size(), value.capacity());
        assert!(storage.layout().size() > value.len());
        let mut ledger = crate::NativeHostAllocationLedger::new(1, usize::MAX).expect("ledger");
        let complete = storage.complete_allocation().expect("complete only");
        ledger.record(complete).expect("owner");
        ledger.record(complete).expect("same owner alias");
        let result = ledger.finish().expect("known subset");
        assert_eq!(
            result.allocation_bytes(),
            value.capacity()
                + crate::NativeHostAllocationLedger::storage_layout(1)
                    .expect("bookkeeping")
                    .bytes()
                    .expect("sum")
        );
    }

    #[test]
    fn native_factory_storage_dynamic_arc_alias_is_payload_never_fabricated_full_allocation() {
        trait Data: Send + Sync {}
        struct Owned([u8; 37]);
        impl Data for Owned {}
        let value = Owned([4; 37]);
        assert_eq!(value.0[0], 4);
        let value: Arc<dyn Data> = Arc::new(value);
        let alias = Arc::clone(&value);
        let first = AudioProcessorFactoryStorageBorrow::arc_payload(&value);
        let second = AudioProcessorFactoryStorageBorrow::arc_payload(&alias);
        assert_eq!(first.address(), second.address());
        assert_eq!(first.layout(), Layout::for_value(value.as_ref()));
        assert_eq!(first.layout().size(), 37);
        assert_eq!(first.kind(), AudioProcessorFactoryStorageKind::ArcPayload);
        assert!(first.complete_allocation().is_none());
        let zero = Arc::new(());
        let payload = AudioProcessorFactoryStorageBorrow::arc_payload(&zero);
        assert_eq!(payload.layout().size(), 0);
        assert_eq!(payload.kind(), AudioProcessorFactoryStorageKind::ArcPayload);
        assert!(
            payload.complete_allocation().is_none(),
            "zero payload does not prove a zero Arc allocation"
        );
    }

    #[test]
    fn native_factory_storage_encoded_unicode_owners_remain_distinct_unqualified_facts() {
        let path = PathBuf::from("子🎹/native plugin");
        let argument = OsString::from("独立🎛参数");
        let path_storage = AudioProcessorFactoryStorageBorrow::path(&path).expect("path fact");
        let argument_storage =
            AudioProcessorFactoryStorageBorrow::os_string(&argument).expect("argument fact");
        assert_eq!(path_storage.layout().size(), path.capacity());
        assert_eq!(argument_storage.layout().size(), argument.capacity());
        assert_eq!(
            path_storage.address(),
            path.as_os_str().as_encoded_bytes().as_ptr() as usize
        );
        assert_eq!(
            argument_storage.address(),
            argument.as_os_str().as_encoded_bytes().as_ptr() as usize
        );
        assert_eq!(
            path_storage.kind(),
            AudioProcessorFactoryStorageKind::StdPlatformEncodedBuffer
        );
        assert_eq!(
            argument_storage.kind(),
            AudioProcessorFactoryStorageKind::StdPlatformEncodedBuffer
        );
        assert!(path_storage.complete_allocation().is_none());
        assert!(argument_storage.complete_allocation().is_none());
    }
}
