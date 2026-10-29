//! Borrowed storage owned by SDK containers, without private allocator casts.
//!
//! Complete Box/Vec allocations and Arc payloads are deliberately distinct.
//! A payload view does not prove Arc's refcount prefix, its full allocation
//! layout, allocator metadata, OS resources or nested handler/provider storage.

use std::alloc::Layout;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::ptr;
use std::sync::Arc;

/// What the borrowed address and layout actually describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStorageKind {
    /// The base and complete allocator-requested layout of one Box or Vec.
    CompleteAllocation,
    /// Only an Arc's public payload address and payload layout.
    /// The allocation prefix must be qualified separately for the toolchain.
    ArcPayload,
}

/// A live, immutable view of one storage owner or public Arc payload.
///
/// This view never dereferences its address. Its lifetime prevents the source
/// owner from resizing or dropping while the view is borrowed. Nested element
/// allocations require separate views. Equal live Arc payload identities are
/// aliases; they are not additional physical allocations.
#[derive(Debug, Clone, Copy)]
pub struct HostStorageRef<'a> {
    address: usize,
    layout: Layout,
    kind: HostStorageKind,
    _owner: PhantomData<&'a ()>,
}

impl<'a> HostStorageRef<'a> {
    /// Borrow a complete Vec allocation using its actual capacity.
    /// Zero-sized elements and zero capacity allocate no storage.
    pub fn from_vec<T>(owner: &'a Vec<T>) -> Result<Self, HostStorageError> {
        let layout =
            Layout::array::<T>(owner.capacity()).map_err(|_| HostStorageError::ExtentOverflow)?;
        Ok(Self {
            address: owner.as_ptr().cast::<()>() as usize,
            layout,
            kind: HostStorageKind::CompleteAllocation,
            _owner: PhantomData,
        })
    }

    /// Borrow a complete Box allocation, including a DST's actual extent.
    #[expect(
        clippy::borrowed_box,
        reason = "Box ownership proves a complete allocation; a generic payload reference does not"
    )]
    pub fn from_box<T: ?Sized>(owner: &'a Box<T>) -> Self {
        Self::boxed_value(owner.as_ref())
    }

    /// Borrow an Arc's public payload; this is not a complete allocation view.
    pub fn from_arc<T: ?Sized>(owner: &'a Arc<T>) -> Self {
        Self::arc_payload(owner.as_ref())
    }

    pub(crate) fn boxed_value<T: ?Sized>(owner: &'a T) -> Self {
        Self {
            address: ptr::from_ref(owner).cast::<()>() as usize,
            layout: Layout::for_value(owner),
            kind: HostStorageKind::CompleteAllocation,
            _owner: PhantomData,
        }
    }

    pub(crate) fn arc_payload<T: ?Sized>(owner: &'a T) -> Self {
        Self {
            address: ptr::from_ref(owner).cast::<()>() as usize,
            layout: Layout::for_value(owner),
            kind: HostStorageKind::ArcPayload,
            _owner: PhantomData,
        }
    }

    /// The complete allocation base or public payload identity, per `kind()`.
    pub const fn address(self) -> usize {
        self.address
    }
    /// The complete allocation layout or payload layout, per `kind()`.
    pub const fn layout(self) -> Layout {
        self.layout
    }
    /// Whether the view covers the complete allocation or only an Arc payload.
    pub const fn kind(self) -> HostStorageKind {
        self.kind
    }
}

/// An explicit inability to prove all requested SDK/provider storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStorageError {
    /// An actual capacity cannot be represented as an allocation layout.
    ExtentOverflow,
    /// A custom entry provider has not supplied an owning storage proof.
    ProviderStorageUnavailable,
    /// An in-process Clack plugin entry owns arbitrary plugin-defined storage.
    InProcessPluginStorageUnavailable,
}

impl Display for HostStorageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ExtentOverflow => "SDK storage extent overflowed",
            Self::ProviderStorageUnavailable => "entry provider owning storage is unavailable",
            Self::InProcessPluginStorageUnavailable => {
                "in-process plugin entry owning storage is unavailable"
            }
        })
    }
}

impl Error for HostStorageError {}
