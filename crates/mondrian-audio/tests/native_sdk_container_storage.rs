//! Actual allocation oracle for SDK buffer/payload owning seams.
//!
//! This standalone test allocator is System pass-through with a warmed fixed
//! thread-local journal. No test bookkeeping allocations occur in observations.
//! COM payload metadata is deliberately not a complete std Arc claim.

use crossbeam_queue::ArrayQueue;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vst3::com_scrape_types::{Header, MakeHeader};
use vst3::Steinberg::{kResultOk, tresult, FUnknown, IPluginBase, IPluginBaseTrait};
use vst3::{Class, ComWrapper};

#[derive(Clone, Copy, Default)]
struct Allocation {
    operation: u8,
    address: usize,
    bytes: usize,
    alignment: usize,
}

#[derive(Clone, Copy)]
struct Trace {
    enabled: bool,
    overflow: bool,
    used: usize,
    allocations: [Allocation; 16],
}

thread_local! {
    static TRACE: Cell<Trace> = const { Cell::new(Trace {
        enabled: false, overflow: false, used: 0,
        allocations: [Allocation { operation: 0, address: 0, bytes: 0, alignment: 0 }; 16],
    }) };
}

fn observe(operation: u8, pointer: *mut u8, layout: Layout) {
    let _ = TRACE.try_with(|cell| {
        let mut trace = cell.get();
        if !trace.enabled {
            return;
        }
        if trace.used == trace.allocations.len() {
            trace.overflow = true;
        } else {
            trace.allocations[trace.used] = Allocation {
                operation,
                address: pointer as usize,
                bytes: layout.size(),
                alignment: layout.align(),
            };
            trace.used += 1;
        }
        cell.set(trace);
    });
}

struct ObservedAllocator;
// SAFETY: every operation forwards the original pointer/layout to System. The
// fixed initialized thread-local journal does no allocation or dereference.
unsafe impl GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc's caller supplies the legal allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        observe(1, pointer, layout);
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded GlobalAlloc allocation contract.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        observe(1, pointer, layout);
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        observe(2, pointer, layout);
        // SAFETY: pointer/layout belong to this same System-backed owner.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        observe(3, pointer, layout);
        // SAFETY: unchanged GlobalAlloc reallocation preconditions.
        let result = unsafe { System.realloc(pointer, layout, bytes) };
        if let Ok(new_layout) = Layout::from_size_align(bytes, layout.align()) {
            observe(4, result, new_layout);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

fn capture<T>(operation: impl FnOnce() -> T) -> (T, Trace) {
    TRACE.with(|cell| {
        let mut value = cell.get();
        value.enabled = true;
        value.overflow = false;
        value.used = 0;
        cell.set(value);
    });
    let result = operation();
    let trace = TRACE.with(|cell| {
        let mut value = cell.get();
        value.enabled = false;
        cell.set(value);
        value
    });
    assert!(!trace.overflow, "bounded allocator observation overflowed");
    (result, trace)
}

#[repr(align(128))]
#[derive(Clone, Copy)]
struct Aligned([u8; 17]);

fn queue_owner<T>(capacity: usize) {
    let (queue, constructor) = capture(|| ArrayQueue::<T>::new(capacity));
    assert_eq!(
        constructor.used, 1,
        "the real fixed Box constructor must allocate once"
    );
    let allocation = constructor.allocations[0];
    assert_eq!(allocation.operation, 1);
    assert_ne!(allocation.address, 0);
    let (storage, observation) = capture(|| queue.buffer_storage());
    assert_eq!(observation.used, 0, "view construction allocates nothing");
    assert_eq!(storage.address(), allocation.address);
    assert_eq!(
        (storage.layout().size(), storage.layout().align()),
        (allocation.bytes, allocation.alignment),
        "real requested complete Slot slice layout"
    );
    assert_eq!(queue.capacity(), capacity);
    let (again, observation) = capture(|| queue.buffer_storage());
    assert_eq!(observation.used, 0);
    assert_eq!(again.address(), storage.address());
    assert_eq!(again.layout(), storage.layout());
    let (_, destruction) = capture(|| drop(queue));
    assert_eq!(destruction.used, 1);
    let released = destruction.allocations[0];
    assert_eq!(released.operation, 2);
    assert_eq!(
        (released.address, released.bytes, released.alignment),
        (allocation.address, allocation.bytes, allocation.alignment)
    );
}

#[test]
fn native_sdk_container_queue_exact_small_and_large_slot_layouts() {
    queue_owner::<u8>(1);
    queue_owner::<u64>(7);
    queue_owner::<[u8; 273]>(257);
}

#[test]
fn native_sdk_container_queue_zero_sized_values_still_own_real_atomic_slots() {
    queue_owner::<()>(31);
}

#[test]
fn native_sdk_container_queue_alignment_is_actual_internal_slot_alignment() {
    queue_owner::<Aligned>(5);
    let queue = ArrayQueue::new(2);
    queue.push(Aligned([3; 17])).ok().expect("slot");
    assert_eq!(queue.pop().expect("same payload").0, [3; 17]);
}

#[test]
fn native_sdk_container_queue_view_never_changes_full_empty_order_or_wrap_semantics() {
    let queue = ArrayQueue::new(3);
    let storage = queue.buffer_storage();
    for lap in 0..96u32 {
        let (_, trace) = capture(|| {
            for offset in 0..3 {
                assert!(queue.push(lap * 3 + offset).is_ok());
            }
            assert_eq!(queue.push(u32::MAX), Err(u32::MAX));
            assert_eq!(queue.buffer_storage().address(), storage.address());
            for offset in 0..3 {
                assert_eq!(queue.pop(), Some(lap * 3 + offset));
            }
            assert_eq!(queue.pop(), None);
        });
        assert_eq!(
            trace.used, 0,
            "buffer metadata and fixed values must allocate nothing"
        );
    }
}

struct ComEmpty;
struct ComBytes([u8; 11]);
#[repr(align(128))]
struct ComAligned([u8; 129]);

macro_rules! class {
    ($kind:ty) => {
        impl Class for $kind {
            type Interfaces = (IPluginBase,);
        }
        impl IPluginBaseTrait for $kind {
            unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
                kResultOk
            }
            unsafe fn terminate(&self) -> tresult {
                kResultOk
            }
        }
    };
}
class!(ComEmpty);
class!(ComBytes);
class!(ComAligned);

fn com_payload<C: Class + 'static>(value: C)
where
    C::Interfaces: MakeHeader<C, ComWrapper<C>>,
{
    let (owner, constructor) = capture(|| ComWrapper::new(value));
    assert_eq!(
        constructor.used, 1,
        "actual COM wrapper has one Arc allocation"
    );
    let allocated = constructor.allocations[0];
    assert_eq!(allocated.operation, 1);
    let (storage, observation) = capture(|| owner.payload_storage());
    assert_eq!(observation.used, 0);
    let expected = Layout::new::<Header<C>>()
        .extend(Layout::new::<C>())
        .expect("public generated header plus class layout")
        .0
        .pad_to_align();
    assert_eq!(
        storage.layout(),
        expected,
        "includes generated COM header and alignment, not class data alone"
    );
    assert_eq!(storage.address() % expected.align(), 0);
    // Independent allocator establishes bounds without touching std's private
    // header. This checks only the public payload; full Arc profile is separate.
    let offset = storage
        .address()
        .checked_sub(allocated.address)
        .expect("payload in its actual owner");
    assert!(offset > 0, "Arc prefix is not covered by this payload view");
    assert!(offset
        .checked_add(expected.size())
        .is_some_and(|bytes| bytes <= allocated.bytes));
    assert_eq!(
        owner.as_com_ref::<IPluginBase>().expect("real generated interface").as_ptr() as usize,
        storage.address(),
        "first actual COM header starts the payload"
    );
    let (alias, clone) = capture(|| owner.clone());
    assert_eq!(clone.used, 0);
    assert_eq!(alias.payload_storage().address(), storage.address());
    assert_eq!(alias.payload_storage().layout(), storage.layout());
    let (_, first_drop) = capture(|| drop(owner));
    assert_eq!(first_drop.used, 0, "alias still retains payload");
    let (_, last_drop) = capture(|| drop(alias));
    assert_eq!(last_drop.used, 1);
    let released = last_drop.allocations[0];
    assert_eq!(released.operation, 2);
    assert_eq!(
        (released.address, released.bytes, released.alignment),
        (allocated.address, allocated.bytes, allocated.alignment)
    );
}

#[test]
fn native_sdk_container_com_payload_exact_header_data_alignment_and_alias_lifetime() {
    com_payload(ComEmpty);
    let bytes = ComBytes([9; 11]);
    assert_eq!(bytes.0, [9; 11]);
    com_payload(bytes);
    let aligned = ComAligned([11; 129]);
    assert_eq!(aligned.0, [11; 129]);
    com_payload(aligned);
}
struct NativeWeakDrop;
static NATIVE_WEAK_CLASS_DROPS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
class!(NativeWeakDrop);
impl Drop for NativeWeakDrop {
    fn drop(&mut self) {
        NATIVE_WEAK_CLASS_DROPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn native_sdk_container_com_weak_actual_allocator_class_and_com_reference_lifetimes() {
    NATIVE_WEAK_CLASS_DROPS.store(0, std::sync::atomic::Ordering::SeqCst);
    let (owner, constructor) = capture(|| ComWrapper::new(NativeWeakDrop));
    assert_eq!(constructor.used, 1);
    let allocated = constructor.allocations[0];
    let payload = owner.payload_storage();
    let payload_address = payload.address();
    let payload_layout = payload.layout();
    let (weak, downgrade) = capture(|| owner.downgrade());
    assert_eq!(
        downgrade.used, 0,
        "same allocation, not a second COM runtime"
    );
    assert_eq!(weak.payload_storage().address(), payload_address);
    assert_eq!(weak.payload_storage().layout(), payload_layout);
    let (pointer, add_ref) =
        capture(|| owner.to_com_ptr::<IPluginBase>().expect("generated COM pointer"));
    assert_eq!(add_ref.used, 0);
    let (_, owner_drop) = capture(|| drop(owner));
    assert_eq!(
        owner_drop.used, 0,
        "actual COM reference keeps the Rust class alive"
    );
    assert_eq!(
        NATIVE_WEAK_CLASS_DROPS.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(weak.is_alive());
    // Exercise a real generated interface call while only the COM reference
    // keeps the class alive; no reconstructed COM vtable is involved.
    assert_eq!(
        unsafe { pointer.initialize(std::ptr::null_mut()) },
        kResultOk
    );
    let (_, final_strong_drop) = capture(|| drop(pointer));
    assert_eq!(
        final_strong_drop.used, 0,
        "weak retains allocation but not the class"
    );
    assert_eq!(
        NATIVE_WEAK_CLASS_DROPS.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let (resurrection, lookup) = capture(|| weak.upgrade());
    assert!(resurrection.is_none());
    assert_eq!(lookup.used, 0);
    assert!(!weak.is_alive());
    assert_eq!(weak.payload_storage().address(), payload_address);
    assert_eq!(weak.payload_storage().layout(), payload_layout);
    let (alias, clone) = capture(|| weak.clone());
    assert_eq!(clone.used, 0);
    let (_, first_weak_drop) = capture(|| drop(weak));
    assert_eq!(first_weak_drop.used, 0);
    let (_, final_weak_drop) = capture(|| drop(alias));
    assert_eq!(final_weak_drop.used, 1);
    let released = final_weak_drop.allocations[0];
    assert_eq!(released.operation, 2);
    assert_eq!(
        (released.address, released.bytes, released.alignment),
        (allocated.address, allocated.bytes, allocated.alignment)
    );
}

#[test]
fn native_sdk_container_exclusive_payload_real_allocator_fifo_and_refusal() {
    let (mut first, first_allocation) = capture(|| Vec::<u8>::with_capacity(257));
    first.resize(3, 7);
    let (mut second, second_allocation) = capture(|| Vec::<u8>::with_capacity(4097));
    second.resize(17, 9);
    assert_eq!((first_allocation.used, second_allocation.used), (1, 1));
    let mut queue = ArrayQueue::new(2);
    assert!(queue.push(first).is_ok());
    assert!(queue.push(second).is_ok());
    let head = queue.pop().expect("actual first payload");
    assert_eq!(
        head.as_ptr() as usize,
        first_allocation.allocations[0].address
    );
    assert!(queue.push(head).is_ok()); // full queue with wrapped head
    let mut seen = [(0_usize, 0_usize); 2];
    let mut count = 0;
    let (result, observation) = capture(|| {
        queue.try_for_each_occupied(|value| {
            seen[count] = (value.as_ptr() as usize, value.capacity());
            count += 1;
            Ok::<(), ()>(())
        })
    });
    assert!(result.is_ok());
    assert_eq!(
        observation.used, 0,
        "exclusive observation allocates nothing"
    );
    assert_eq!(count, 2);
    assert_eq!(
        seen,
        [
            (
                second_allocation.allocations[0].address,
                second_allocation.allocations[0].bytes
            ),
            (
                first_allocation.allocations[0].address,
                first_allocation.allocations[0].bytes
            ),
        ]
    );
    let (error, refused) = capture(|| queue.try_for_each_occupied(|_| Err::<(), _>(17_u32)));
    assert_eq!(error, Err(17));
    assert_eq!(refused.used, 0);
    assert_eq!(
        queue.len(),
        2,
        "refused observation does not consume a value"
    );
    let next = queue.pop().expect("same second payload after refusal");
    assert_eq!(
        next.as_ptr() as usize,
        second_allocation.allocations[0].address
    );
    assert_eq!(next, vec![9; 17]);
    let last = queue.pop().expect("same first payload after refusal");
    assert_eq!(
        last.as_ptr() as usize,
        first_allocation.allocations[0].address
    );
    assert_eq!(last, vec![7; 3]);
}

#[test]
fn native_sdk_container_exclusive_payload_empty_partial_wrap_and_panic_leave_queue() {
    let mut queue = ArrayQueue::new(3);
    let mut count = 0;
    queue
        .try_for_each_occupied(|_: &u64| {
            count += 1;
            Ok::<(), ()>(())
        })
        .expect("empty owner");
    assert_eq!(count, 0);
    for round in 0..64_u64 {
        assert!(queue.push(round * 3).is_ok());
        assert!(queue.push(round * 3 + 1).is_ok());
        assert_eq!(queue.pop(), Some(round * 3));
        assert!(queue.push(round * 3 + 2).is_ok());
        let mut actual = [0; 2];
        let mut index = 0;
        queue
            .try_for_each_occupied(|value| {
                actual[index] = *value;
                index += 1;
                Ok::<(), ()>(())
            })
            .expect("wrapped initialized payloads");
        assert_eq!(actual, [round * 3 + 1, round * 3 + 2]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            queue.try_for_each_occupied(|_| -> Result<(), ()> { panic!("local observer refusal") })
        }));
        assert!(result.is_err());
        assert_eq!(queue.pop(), Some(round * 3 + 1));
        assert_eq!(queue.pop(), Some(round * 3 + 2));
        assert!(queue.is_empty());
    }
}

#[test]
fn native_sdk_container_exclusive_payload_visitor_side_effects_are_not_rolled_back() {
    let mut queue = ArrayQueue::new(2);
    assert!(queue.push(std::cell::Cell::new(1_u32)).is_ok());
    assert!(queue.push(std::cell::Cell::new(2_u32)).is_ok());
    let result = queue.try_for_each_occupied(|value| {
        value.set(17);
        Err::<(), _>(23)
    });
    assert_eq!(result, Err(23));
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.pop().map(|value| value.get()), Some(17));
    assert_eq!(queue.pop().map(|value| value.get()), Some(2));
}
