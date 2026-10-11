//! Exact-layout non-RT storage owned by the native host, never plugin-private heap.

use std::alloc::{alloc_zeroed, Layout};
use std::io;
use std::ptr;

/// Exact allocator-requested bytes for one fixed host buffer.
///
/// Allocator usable-size overhead, OS pages and plugin-private memory are not
/// part of this layout. The caller reserves the complete overlapping owner peak
/// before passing its explicit per-buffer grant to the allocation constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHostBufferLayout {
    /// Requested allocation size, including no hidden Vec spare capacity.
    pub bytes: usize,
    /// Actual allocation alignment.
    pub alignment: usize,
}

impl NativeHostBufferLayout {
    /// Describe actual reserved Vec element storage without using its length.
    ///
    /// Nested elements, the owning structure and any shared allocation must be
    /// reported separately. A zero-sized element has no element allocation.
    pub fn for_vector<T>(vector: &Vec<T>) -> Result<Self, NativeHostBufferError> {
        let layout = Layout::array::<T>(vector.capacity())
            .map_err(|_| NativeHostBufferError::ExtentOverflow)?;
        Ok(Self { bytes: layout.size(), alignment: layout.align() })
    }
}

/// Failure to reserve or write one exact bounded host allocation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NativeHostBufferError {
    /// A requested layout cannot be represented by the allocator.
    #[error("native host buffer extent exceeds the addressable allocation layout")]
    ExtentOverflow,
    /// This owner has no authority for the requested bytes.
    #[error("native host buffer requires {required} bytes but was granted {granted}")]
    GrantExceeded {
        /// Bytes requested by this exact allocation owner.
        required: usize,
        /// Incremental bytes explicitly granted to this allocation owner.
        granted: usize,
    },
    /// The exact already admitted allocation could not be obtained.
    #[error("native host could not allocate its admitted fixed buffer")]
    AllocationUnavailable,
}

/// One fixed host byte owner with a separately tracked initialized payload length.
///
/// Creation, clearing and destruction are non-RT operations. Writes can neither
/// grow the allocation nor exceed its authorized extent. It does not assert a
/// complete worker footprint: SDK objects, metadata, other containers and IPC
/// buffers still belong to the canonical owner ledger.
pub struct NativeHostFixedBytes {
    storage: Box<[u8]>,
    used: usize,
    writer_failed: bool,
}

impl NativeHostFixedBytes {
    /// Allocate exactly one authorized byte-slice layout before calling native code.
    ///
    /// A grant is incremental to this owner, not a standalone worker budget.
    /// The root planner must reserve retained + candidate + control overlap.
    pub fn new(length: usize, granted_bytes: usize) -> Result<Self, NativeHostBufferError> {
        if length > granted_bytes {
            return Err(NativeHostBufferError::GrantExceeded {
                required: length,
                granted: granted_bytes,
            });
        }
        let layout =
            Layout::array::<u8>(length).map_err(|_| NativeHostBufferError::ExtentOverflow)?;
        let storage = if length == 0 {
            Box::default()
        } else {
            // SAFETY: layout describes exactly length aligned u8 elements. The
            // global allocator returns a unique initialized zeroed allocation
            // or null. On success Box assumes that exact matching allocation;
            // its normal drop deallocates it once with the same slice layout.
            // No Vec conversion/shrink creates an unrecorded overlapping copy.
            let raw = unsafe { alloc_zeroed(layout) };
            if raw.is_null() {
                return Err(NativeHostBufferError::AllocationUnavailable);
            }
            // SAFETY: the nonnull allocation above contains length valid u8s.
            unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(raw, length)) }
        };
        Ok(Self { storage, used: 0, writer_failed: false })
    }

    /// Actual allocator-requested storage, independent of current payload length.
    pub fn allocation_layout(&self) -> NativeHostBufferLayout {
        NativeHostBufferLayout {
            bytes: self.storage.len(),
            alignment: std::mem::align_of::<u8>(),
        }
    }

    /// Current bytes written within the fixed allocation.
    pub fn payload(&self) -> &[u8] {
        &self.storage[..self.used]
    }

    /// Clear logical contents while preserving the exact allocation owner.
    ///
    /// This starts a new writer attempt; callers must never clear a failed native
    /// attempt merely to publish truncated output from that same attempt.
    pub fn begin_write(&mut self) {
        self.used = 0;
        self.writer_failed = false;
    }

    /// Independently inspect ignored native stream failures before publication.
    pub const fn writer_failed(&self) -> bool {
        self.writer_failed
    }

    /// Transfer the fixed owner and its exact used length without a clone/reallocation.
    ///
    /// The full storage layout remains charged after transfer. A short payload
    /// does not reduce peak residency to its logical length.
    pub fn into_storage(self) -> Result<(Box<[u8]>, usize), io::Error> {
        if self.writer_failed {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        Ok((self.storage, self.used))
    }

    /// Transfer to an existing Vec-based state codec without reallocating.
    ///
    /// Its capacity remains the full admitted storage length, even when the
    /// payload is shorter. The receiving owner must charge that actual capacity;
    /// it cannot count the payload length or shrink this transfer inside service.
    pub fn into_payload_vector(self) -> Result<Vec<u8>, io::Error> {
        let (storage, used) = self.into_storage()?;
        let mut bytes = storage.into_vec();
        bytes.truncate(used);
        Ok(bytes)
    }
}

impl io::Write for NativeHostFixedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.used.checked_add(bytes.len());
        if self.writer_failed || !end.is_some_and(|end| end <= self.storage.len()) {
            self.writer_failed = true;
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let end = end.ok_or_else(|| io::Error::from(io::ErrorKind::StorageFull))?;
        self.storage[self.used..end].copy_from_slice(bytes);
        self.used = end;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
