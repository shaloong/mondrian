//! Actual allocator oracle for the proposed exact-layout owner.
//!
//! Intended as a separate integration binary with its own global allocator.
//! No native fixture or product correctness is inferred from this storage test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;

// Root places the source into the canonical crate/module before running this
// test. This path is explicit; no private SDK representation is recreated here.
use mondrian_audio::{NativeHostBufferError, NativeHostBufferLayout, NativeHostFixedBytes};

struct AuditAllocator;

thread_local! { static AUDIT: Cell<AuditState> = const { Cell::new(AuditState {
    active: false, counts: Allocations { calls: 0, bytes: 0, reallocations: 0, drops: 0, drop_bytes: 0 }
}) }; }

#[derive(Clone, Copy)]
struct AuditState {
    active: bool,
    counts: Allocations,
}

fn record(operation: u8, bytes: usize) {
    // Thread-local counters exclude concurrently running tests and allocator
    // bookkeeping itself. TLS destruction is explicitly harmless here.
    let _ = AUDIT.try_with(|audit| {
        let mut state = audit.get();
        if state.active {
            match operation {
                0 => {
                    state.counts.calls += 1;
                    state.counts.bytes += bytes;
                }
                1 => state.counts.reallocations += 1,
                2 => {
                    state.counts.drops += 1;
                    state.counts.drop_bytes += bytes;
                }
                _ => return,
            }
            audit.set(state);
        }
    });
}

unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: transparently forward the allocator contract to System.
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            record(0, layout.size());
        }
        result
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: transparently forward the allocator contract to System.
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() {
            record(0, layout.size());
        }
        result
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(1, size);
        // SAFETY: transparently forward the allocator contract to System.
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record(2, layout.size());
        // SAFETY: transparently forward the allocator contract to System.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Allocations {
    calls: usize,
    bytes: usize,
    reallocations: usize,
    drops: usize,
    drop_bytes: usize,
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, Allocations) {
    // Initialize thread-local state and all test bookkeeping before admission.
    AUDIT.with(|audit| {
        assert!(!audit.get().active);
        audit.set(AuditState {
            active: true,
            counts: Allocations {
                calls: 0,
                bytes: 0,
                reallocations: 0,
                drops: 0,
                drop_bytes: 0,
            },
        });
    });
    struct EndAudit;
    impl Drop for EndAudit {
        fn drop(&mut self) {
            AUDIT.with(|audit| {
                let mut state = audit.get();
                state.active = false;
                audit.set(state);
            });
        }
    }
    let guard = EndAudit;
    let result = operation();
    drop(guard);
    (result, AUDIT.with(|audit| audit.get().counts))
}

#[test]
fn native_host_fixed_bytes_allocator_matches_actual_layout_without_vec_shrink() {
    for size in [0, 1, 3, 255, 65_535, 65_536, 131_071] {
        let (buffer, allocations) =
            measure(|| NativeHostFixedBytes::new(size, size).expect("exact grant"));
        assert_eq!(allocations.calls, usize::from(size != 0));
        assert_eq!(allocations.bytes, size);
        assert_eq!(allocations.reallocations, 0);
        assert_eq!(allocations.drops, 0);
        assert_eq!(
            buffer.allocation_layout(),
            NativeHostBufferLayout { bytes: size, alignment: 1 }
        );
        let (_, released) = measure(|| drop(buffer));
        assert_eq!(released.calls, 0);
        assert_eq!(released.reallocations, 0);
        assert_eq!(released.drops, usize::from(size != 0));
        assert_eq!(released.drop_bytes, size);
    }
}

#[test]
fn native_host_fixed_bytes_below_grant_allocates_nothing() {
    let (result, allocations) = measure(|| NativeHostFixedBytes::new(65_536, 65_535));
    assert!(matches!(
        result,
        Err(NativeHostBufferError::GrantExceeded { required: 65_536, granted: 65_535 })
    ));
    assert_eq!(
        allocations,
        Allocations {
            calls: 0,
            bytes: 0,
            reallocations: 0,
            drops: 0,
            drop_bytes: 0
        }
    );
}

#[test]
fn native_host_fixed_bytes_full_payload_and_transfer_have_zero_allocation() {
    let mut buffer = NativeHostFixedBytes::new(65_536, 65_536).expect("fixed allocation");
    let full = [0x5A; 65_536];
    let pointer = buffer.payload().as_ptr();
    let ((storage, used), allocations) = measure(|| {
        buffer.write_all(&full).expect("full payload");
        buffer.into_storage().expect("move owner")
    });
    assert_eq!(
        allocations,
        Allocations {
            calls: 0,
            bytes: 0,
            reallocations: 0,
            drops: 0,
            drop_bytes: 0
        }
    );
    assert_eq!(used, full.len());
    assert_eq!(storage.as_ptr(), pointer);
    assert_eq!(&*storage, &full);
}

#[test]
fn native_host_fixed_bytes_short_payload_vector_transfer_preserves_full_owner_capacity() {
    let mut buffer = NativeHostFixedBytes::new(65_536, 65_536).expect("fixed allocation");
    let pointer = buffer.payload().as_ptr();
    let (state, allocations) = measure(|| {
        buffer.write_all(&[0x37, 0x19, 0xA5]).expect("short state");
        buffer.into_payload_vector().expect("existing codec transfer")
    });
    assert_eq!(
        allocations,
        Allocations {
            calls: 0,
            bytes: 0,
            reallocations: 0,
            drops: 0,
            drop_bytes: 0
        }
    );
    assert_eq!(state, [0x37, 0x19, 0xA5]);
    assert_eq!(state.as_ptr(), pointer);
    assert_eq!(state.capacity(), 65_536);
    assert_eq!(
        NativeHostBufferLayout::for_vector(&state)
            .expect("retained state capacity")
            .bytes,
        65_536
    );
    let (_, released) = measure(|| drop(state));
    assert_eq!(released.calls, 0);
    assert_eq!(released.reallocations, 0);
    assert_eq!(released.drops, 1);
    assert_eq!(released.drop_bytes, 65_536);
}

#[test]
fn native_host_vector_layout_counts_spare_capacity_and_zero_sized_elements() {
    let vector = Vec::<u64>::with_capacity(31);
    assert!(vector.is_empty());
    let actual = NativeHostBufferLayout::for_vector(&vector).expect("owned vector layout");
    assert_eq!(actual.bytes, vector.capacity() * std::mem::size_of::<u64>());
    assert_eq!(actual.alignment, std::mem::align_of::<u64>());
    let zst = vec![(); 17];
    assert_eq!(
        NativeHostBufferLayout::for_vector(&zst).expect("ZST").bytes,
        0
    );
}

#[test]
fn native_host_fixed_bytes_ignored_overflow_never_publishes_truncated_configuration() {
    let mut buffer = NativeHostFixedBytes::new(3, 3).expect("fixed buffer");
    buffer.write_all(&[1, 2, 3]).expect("at limit");
    let (overflow, allocations) = measure(|| buffer.write_all(&[4]));
    assert!(overflow.is_err());
    assert_eq!(
        allocations,
        Allocations {
            calls: 0,
            bytes: 0,
            reallocations: 0,
            drops: 0,
            drop_bytes: 0
        }
    );
    assert!(buffer.writer_failed());
    assert_eq!(buffer.payload(), [1, 2, 3]);
    assert!(buffer.into_storage().is_err());
}

#[test]
fn native_host_fixed_bytes_new_attempt_resets_sticky_failure_without_allocation() {
    let mut buffer = NativeHostFixedBytes::new(3, 3).expect("fixed buffer");
    assert!(buffer.write_all(&[1, 2, 3, 4]).is_err());
    let ((), allocations) = measure(|| {
        buffer.begin_write();
        buffer.write_all(&[7, 8]).expect("distinct new state attempt");
    });
    assert_eq!(
        allocations,
        Allocations {
            calls: 0,
            bytes: 0,
            reallocations: 0,
            drops: 0,
            drop_bytes: 0
        }
    );
    assert!(!buffer.writer_failed());
    assert_eq!(buffer.payload(), [7, 8]);
    assert_eq!(buffer.allocation_layout().bytes, 3);
}
