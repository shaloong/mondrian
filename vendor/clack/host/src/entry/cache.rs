use crate::entry::PluginEntry;
use crate::entry::PluginEntryError;
use crate::entry::entry_provider::EntryProvider;
use crate::entry::loaded_entry::{LoadedEntry, LoadedEntryDyn};
use crate::storage::{HostStorageError, HostStorageRef};
use clack_common::entry::EntryDescriptor;
use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Eq, PartialEq)]
struct EntryPointer(NonNull<EntryDescriptor>);

// SAFETY: addresses are keys only; this type never dereferences them.
unsafe impl Send for EntryPointer {}
// SAFETY: addresses are keys only; this type never dereferences them.
unsafe impl Sync for EntryPointer {}

type EntryCache = Vec<(EntryPointer, Arc<dyn LoadedEntryDyn>)>;

// Entry loading/unloading is control-time work. A dense owner exposes its
// actual complete allocation layout instead of guessing HashMap bucket bytes.
// Repeated load/refcount/deinit behavior remains keyed by the same native entry.
static ENTRY_CACHE: Mutex<EntryCache> = Mutex::new(Vec::new());

pub(crate) fn get_or_init<E: EntryProvider>(
    entry_provider: E,
    init_bundle_path: &CStr,
) -> Result<CachedEntry, PluginEntryError> {
    let mut cache = ENTRY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let entry_pointer = EntryPointer(entry_provider.entry_pointer());
    let source = if let Some((_, existing)) = cache.iter().find(|(key, _)| *key == entry_pointer) {
        Arc::clone(existing)
    } else {
        let source: Arc<dyn LoadedEntryDyn> =
            Arc::new(LoadedEntry::load(entry_provider, init_bundle_path)?);
        cache.push((entry_pointer, Arc::clone(&source)));
        source
    };
    Ok(CachedEntry(Some(source)))
}

/// An immutable, locked snapshot of the SDK's global loaded-entry owners.
///
/// While this guard is held, entry loading/unloading waits: do not invoke native
/// callbacks, drop entry handles, or recursively acquire it. The guard performs
/// no loading, native calls or allocation. Storage views cannot outlive it.
/// Its cache allocation and all cached loaded-entry Arc payloads are one global
/// union, not independent per-instance charges. The cache may retain its Vec
/// capacity after entries unload; that live allocation is still reported.
pub struct EntryCacheStorageGuard<'a> {
    cache: MutexGuard<'static, EntryCache>,
    _entry: &'a PluginEntry,
}

impl<'a> EntryCacheStorageGuard<'a> {
    pub(crate) fn new(entry: &'a PluginEntry) -> Self {
        Self {
            cache: ENTRY_CACHE
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            _entry: entry,
        }
    }

    /// Visit the complete cache Vec, each loaded-entry Arc payload, and each
    /// provider's known nested storage. Unknown providers explicitly fail.
    ///
    /// A failure invalidates the entire requested snapshot; do not publish the
    /// already visited prefix as a complete receipt. This callback must only
    /// record metadata and must not execute native or entry-lifecycle work.
    pub fn visit_owned_storage<'s>(
        &'s self,
        visitor: &mut dyn FnMut(HostStorageRef<'s>),
    ) -> Result<(), HostStorageError> {
        visitor(HostStorageRef::from_vec(&self.cache)?);
        for (_, entry) in self.cache.iter() {
            visitor(HostStorageRef::from_arc(entry));
            entry.visit_owned_storage(visitor)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct CachedEntry(Option<Arc<dyn LoadedEntryDyn>>);

impl CachedEntry {
    #[inline]
    pub(crate) fn raw_entry(&self) -> NonNull<EntryDescriptor> {
        let Some(entry) = &self.0 else {
            unreachable!("Unloaded state only exists during CachedEntry's Drop implementation")
        };
        entry.entry_pointer()
    }

    #[inline]
    pub fn as_ref(&self) -> &EntryDescriptor {
        // SAFETY: the entry stays live while this cached handle is alive.
        unsafe { self.raw_entry().as_ref() }
    }

    #[inline]
    pub fn get(&self) -> EntryDescriptor {
        // SAFETY: the entry stays live while this cached handle is alive.
        unsafe { self.raw_entry().read() }
    }
}

impl Drop for CachedEntry {
    fn drop(&mut self) {
        let pointer = EntryPointer(self.raw_entry());
        // Drop this handle before testing whether the cache has the only Arc.
        self.0 = None;
        let mut cache = ENTRY_CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = cache.iter().position(|(key, _)| *key == pointer) {
            if Arc::get_mut(&mut cache[index].1).is_some() {
                // No observable entry iteration order exists outside this
                // metadata guard. Removing the last alias calls deinit once.
                cache.swap_remove(index);
            }
        }
    }
}
