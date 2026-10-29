use clack_common::entry::EntryDescriptor;
use std::ptr::NonNull;
use std::sync::Arc;

/// A type which can provide an entry descriptor pointer for Clack hosts to load.
///
/// # Safety
///
/// Implementors *must* ensure the [`EntryDescriptor`] returned by the [`entry_pointer`](Self::entry_pointer) method
/// remains valid for the entire lifetime of this type. That means all the function pointers it contains
/// must remain valid to call.
///
/// Moreover, the returned [`EntryDescriptor`] must have a stable address. It must not move for the
/// lifetime of this type.
pub unsafe trait EntryProvider: Send + Sync + 'static {
    /// Returns a pointer to the [`EntryDescriptor`] this provides.
    ///
    /// This pointer always valid for reads, as long as this type is alive.
    /// This pointer is also stable, i.e. its address will never change for the entire duration of
    /// this type's lifetime.
    fn entry_pointer(&self) -> NonNull<EntryDescriptor>;

    /// Visit allocations owned inside this provider, excluding its inline body.
    ///
    /// The inline provider body is already included in the loaded-entry Arc
    /// payload. Unknown custom providers return an explicit incomplete proof;
    /// a missing implementation never means zero bytes. Implementations must
    /// report each nested Box/Vec/Arc owner using its real owning container.
    /// The callback is control-time metadata work and must not load/unload
    /// entries or invoke native plugin callbacks while the entry-cache lock is held.
    fn visit_owned_storage<'a>(
        &'a self,
        _visitor: &mut dyn FnMut(crate::storage::HostStorageRef<'a>),
    ) -> Result<(), crate::storage::HostStorageError> {
        Err(crate::storage::HostStorageError::ProviderStorageUnavailable)
    }
}

// SAFETY: The pointer is always valid since it comes from a 'static reference
unsafe impl EntryProvider for &'static EntryDescriptor {
    #[inline]
    fn entry_pointer(&self) -> NonNull<EntryDescriptor> {
        (*self).into()
    }

    fn visit_owned_storage<'a>(
        &'a self,
        _visitor: &mut dyn FnMut(crate::storage::HostStorageRef<'a>),
    ) -> Result<(), crate::storage::HostStorageError> {
        Ok(())
    }
}

// SAFETY: The pointer comes from this Arc, it is always valid as long as the Arc is alive
unsafe impl EntryProvider for Arc<EntryDescriptor> {
    #[inline]
    fn entry_pointer(&self) -> NonNull<EntryDescriptor> {
        self.as_ref().into()
    }

    fn visit_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(crate::storage::HostStorageRef<'a>),
    ) -> Result<(), crate::storage::HostStorageError> {
        visitor(crate::storage::HostStorageRef::from_arc(self));
        Ok(())
    }
}

// SAFETY: The pointer comes from this Box, it is always valid as long as the Box is alive
unsafe impl EntryProvider for Box<EntryDescriptor> {
    #[inline]
    fn entry_pointer(&self) -> NonNull<EntryDescriptor> {
        self.as_ref().into()
    }

    fn visit_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(crate::storage::HostStorageRef<'a>),
    ) -> Result<(), crate::storage::HostStorageError> {
        visitor(crate::storage::HostStorageRef::from_box(self));
        Ok(())
    }
}
