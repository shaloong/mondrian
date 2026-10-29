//! Actual host-owned storage observations, independent of plugin DSP state.
//!
//! A callback borrows each real owner while its layout is inspected. It must
//! not invoke plugins, resize storage, allocate or wait on another owner. This
//! is a local allocation inventory, never author permission or installation.

use std::alloc::Layout;
use std::ffi::CString;
use std::marker::PhantomData;
use std::sync::Arc;
use vst3::{Class, ComWeak, ComWrapper};

/// Whether a view covers an allocation or an Arc payload requiring std proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStorageKind {
    /// Complete Box/Vec/CString allocator-requested storage.
    CompleteAllocation,
    /// Arc payload only; std reference counters/padding require separate proof.
    ArcPayload,
    /// Platform encoded PathBuf storage; actual std source/allocator proof required.
    StdPlatformEncodedBuffer,
}

/// A short owning borrow of one host-created local storage extent.
#[derive(Debug, Clone, Copy)]
pub struct HostStorageRef<'a> {
    address: usize,
    layout: Layout,
    kind: HostStorageKind,
    owner: PhantomData<&'a ()>,
}

impl<'a> HostStorageRef<'a> {
    /// Actual complete-allocation or Arc-payload identity in this process.
    pub const fn address(self) -> usize {
        self.address
    }
    /// Exact allocator request (complete storage) or payload layout.
    pub const fn layout(self) -> Layout {
        self.layout
    }
    /// Coverage distinction; an Arc payload is not its entire allocation.
    pub const fn kind(self) -> HostStorageKind {
        self.kind
    }

    pub(crate) fn vector<T>(value: &'a Vec<T>) -> Result<Self, HostStorageError> {
        Ok(Self {
            address: value.as_ptr().cast::<()>() as usize,
            layout: Layout::array::<T>(value.capacity())
                .map_err(|_| HostStorageError::ExtentOverflow)?,
            kind: HostStorageKind::CompleteAllocation,
            owner: PhantomData,
        })
    }

    pub(crate) fn string(value: &'a String) -> Result<Self, HostStorageError> {
        Ok(Self {
            address: value.as_ptr().cast::<()>() as usize,
            layout: Layout::array::<u8>(value.capacity())
                .map_err(|_| HostStorageError::ExtentOverflow)?,
            kind: HostStorageKind::CompleteAllocation,
            owner: PhantomData,
        })
    }

    pub(crate) fn c_string(value: &'a CString) -> Result<Self, HostStorageError> {
        let bytes = value.as_bytes_with_nul();
        Ok(Self {
            address: bytes.as_ptr().cast::<()>() as usize,
            layout: Layout::for_value(bytes),
            kind: HostStorageKind::CompleteAllocation,
            owner: PhantomData,
        })
    }

    #[expect(
        clippy::borrowed_box,
        reason = "The Box borrow proves allocation ownership; &T could name stack or borrowed storage"
    )]
    pub(crate) fn boxed<T: ?Sized>(value: &'a Box<T>) -> Self {
        Self {
            address: std::ptr::from_ref(value.as_ref()).cast::<()>() as usize,
            layout: Layout::for_value(value.as_ref()),
            kind: HostStorageKind::CompleteAllocation,
            owner: PhantomData,
        }
    }

    pub(crate) fn arc<T: ?Sized>(value: &'a Arc<T>) -> Self {
        Self {
            address: Arc::as_ptr(value).cast::<()>() as usize,
            layout: Layout::for_value(value.as_ref()),
            kind: HostStorageKind::ArcPayload,
            owner: PhantomData,
        }
    }

    pub(crate) fn queue<T>(value: &'a crossbeam_queue::ArrayQueue<T>) -> Self {
        let storage = value.buffer_storage();
        Self {
            address: storage.address(),
            layout: storage.layout(),
            kind: HostStorageKind::CompleteAllocation,
            owner: PhantomData,
        }
    }

    pub(crate) fn owned_layout<T: ?Sized>(owner: &'a T, address: usize, layout: Layout) -> Self {
        let _ = owner;
        Self {
            address,
            layout,
            kind: HostStorageKind::CompleteAllocation,
            owner: PhantomData,
        }
    }

    pub(crate) fn path(value: &'a std::path::PathBuf) -> Result<Self, HostStorageError> {
        Ok(Self {
            address: value.as_os_str().as_encoded_bytes().as_ptr() as usize,
            layout: Layout::array::<u8>(value.capacity())
                .map_err(|_| HostStorageError::ExtentOverflow)?,
            kind: HostStorageKind::StdPlatformEncodedBuffer,
            owner: PhantomData,
        })
    }

    pub(crate) fn com<C: Class>(value: &'a ComWrapper<C>) -> Self {
        let payload = value.payload_storage();
        Self {
            address: payload.address(),
            layout: payload.layout(),
            kind: HostStorageKind::ArcPayload,
            owner: PhantomData,
        }
    }

    pub(crate) fn weak_com<C: Class>(value: &'a ComWeak<C>) -> Self {
        let payload = value.payload_storage();
        Self {
            address: payload.address(),
            layout: payload.layout(),
            kind: HostStorageKind::ArcPayload,
            owner: PhantomData,
        }
    }
}

/// Actual identity and requested stack of one host-owned standard thread.
///
/// These public facts do not expose std's private allocation addresses or
/// layouts. A complete observer must use an independently qualified std
/// thread profile for the exact compiler/target and account the System-owned
/// Thread allocation, spawn packet and live closure separately. The requested
/// stack reservation is distinct from committed pages and OS bookkeeping.
#[derive(Debug, Clone, Copy)]
pub struct HostThreadStorageRef<'a> {
    thread: &'a std::thread::Thread,
    requested_stack_size: usize,
    has_join_handle: bool,
    finished: bool,
}

impl<'a> HostThreadStorageRef<'a> {
    pub(crate) const fn new(
        thread: &'a std::thread::Thread,
        requested_stack_size: usize,
        has_join_handle: bool,
        finished: bool,
    ) -> Self {
        Self {
            thread,
            requested_stack_size,
            has_join_handle,
            finished,
        }
    }
    /// The actual standard thread identity, shared by all clones of this handle.
    pub fn thread_id(self) -> std::thread::ThreadId {
        self.thread.id()
    }
    /// Actual immutable UTF-8 thread name, borrowed while the handle is alive.
    pub fn name(self) -> Option<&'a str> {
        self.thread.name()
    }
    /// The stack size explicitly requested at the actual spawning boundary.
    pub const fn requested_stack_size(self) -> usize {
        self.requested_stack_size
    }
    /// Whether this owner still retains the actual standard JoinHandle packet.
    pub const fn has_join_handle(self) -> bool {
        self.has_join_handle
    }
    /// Actual JoinHandle completion observation; this does not join the thread.
    pub const fn is_finished(self) -> bool {
        self.finished
    }
}

/// Receiver of short owning borrows; it must not retain dereferenceable pointers.
pub trait HostStorageVisitor {
    /// Account this owner immediately, with no allocation or foreign calls.
    fn visit(&mut self, storage: HostStorageRef<'_>) -> Result<(), HostStorageError>;
    /// Account a real host-owned standard thread using its qualified profile.
    /// The default explicitly refuses this family instead of charging zero.
    fn visit_thread(&mut self, _thread: HostThreadStorageRef<'_>) -> Result<(), HostStorageError> {
        Err(HostStorageError::UnqualifiedStdThreadOwner)
    }
}

/// Explicit failure to finish one known host storage observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HostStorageError {
    /// A complete layout cannot be represented.
    #[error("host storage extent overflowed")]
    ExtentOverflow,
    /// A registry cannot retain another observed owner within its bound.
    #[error("host-created COM allocation registry is full")]
    RegistryFull,
    /// Fallible non-RT registry storage allocation failed.
    #[error("host allocation registry storage is unavailable")]
    AllocationUnavailable,
    /// A poisoned owner cannot supply a trustworthy inventory.
    #[error("host allocation owner is poisoned")]
    PoisonedOwner,
    /// The caller's inventory limit was exhausted; no truncated success.
    #[error("host allocation observation was refused")]
    ObserverRefused,
    /// This backend has no local owning proof for the requested family.
    #[error("host allocation owner is unavailable")]
    UnavailableOwner,
    /// A concurrent shared owner has no complete nested-storage observation.
    #[error("shared host allocation owner is not exclusively observable")]
    NonExclusiveSharedOwner,
    /// An arbitrary caller callback can contain unknown captured heap owners.
    #[error("host callback captured storage has no owning observation contract")]
    OpaqueCallbackOwner,
    /// A complete std-thread allocation profile has not been qualified here.
    #[error("standard thread allocation profile is unqualified")]
    UnqualifiedStdThreadOwner,
    /// A plugin-context observation must run on its component control thread.
    #[error("host allocation observation requires the component control thread")]
    WrongThread,
}
