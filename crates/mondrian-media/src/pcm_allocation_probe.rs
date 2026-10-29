//! Test-only actual allocator observation; every allocation delegates to System.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

struct ProbeAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

static TARGET: AtomicUsize = AtomicUsize::new(0);
static TARGET_DROPPED: AtomicUsize = AtomicUsize::new(0);
static TARGET_SERIAL: Mutex<()> = Mutex::new(());

#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;

fn note_allocation() {
    let tracking = TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false);
    if tracking {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: The allocator delegates the unmodified pointer/Layout contracts to
// System; probes use only fixed atomics and const thread-local scalar storage.
unsafe impl GlobalAlloc for ProbeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: The caller supplies the GlobalAlloc Layout contract unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: The caller supplies the GlobalAlloc Layout contract unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: The caller supplies the original System owner and valid size.
        unsafe { System.realloc(pointer, layout, new_size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if TARGET
            .compare_exchange(pointer as usize, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            TARGET_DROPPED.fetch_add(1, Ordering::Release);
        }
        // SAFETY: The exact allocation pointer/Layout is returned to System.
        unsafe { System.dealloc(pointer, layout) }
    }
}

pub(crate) struct AllocationScope;

impl AllocationScope {
    /// Begin an actual allocator interval on this caller thread after setup.
    pub(crate) fn enter() -> Self {
        // Initialize/read the scalar TLS outside the tracked interval.
        TRACK_ALLOCATIONS.with(|tracking| assert!(!tracking.get(), "nested allocation oracle"));
        ALLOCATIONS.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|tracking| tracking.set(true));
        Self
    }

    /// Actual allocation/reallocation attempts on the current tracked thread.
    pub(crate) fn count(&self) -> usize {
        ALLOCATIONS.with(Cell::get)
    }
}

impl Drop for AllocationScope {
    fn drop(&mut self) {
        TRACK_ALLOCATIONS.with(|tracking| tracking.set(false));
    }
}

pub(crate) struct BackingOwnerProbe {
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl BackingOwnerProbe {
    /// Track only the actual original f32 backing, across its worker transfer.
    pub(crate) fn track(pointer: *const f32) -> Self {
        // A behavioral RED may unwind while holding this serialization guard;
        // subsequent independent cases still need their actual pointer oracle.
        let serial = TARGET_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        TARGET_DROPPED.store(0, Ordering::Release);
        assert_ne!(pointer as usize, 0);
        TARGET.store(pointer as usize, Ordering::Release);
        Self { _serial: serial }
    }

    /// Whether System received the tracked original backing's first deallocation.
    pub(crate) fn dropped(&self) -> usize {
        TARGET_DROPPED.load(Ordering::Acquire)
    }
}

impl Drop for BackingOwnerProbe {
    fn drop(&mut self) {
        TARGET.store(0, Ordering::Release);
    }
}

#[test]
fn actual_pcm_allocator_control_detects_backing_drop_and_excludes_setup() {
    let mut samples = Vec::with_capacity(8);
    samples.extend_from_slice(&[1.0_f32, 2.0, 3.0, 4.0]);
    let backing = BackingOwnerProbe::track(samples.as_ptr());
    assert_eq!(backing.dropped(), 0);
    drop(samples);
    assert_eq!(backing.dropped(), 1);
    let allocations = AllocationScope::enter();
    let owned = Box::new(std::hint::black_box([0_u8; 64]));
    std::hint::black_box(&owned);
    let count = allocations.count();
    drop(allocations);
    assert!(count >= 1, "actual allocating control must be observed");
    drop(owned);
}
