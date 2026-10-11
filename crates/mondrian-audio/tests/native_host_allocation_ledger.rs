//! Actual requested-allocation oracle for the bounded local host union snapshot.

use mondrian_audio::{
    NativeHostAllocationLedger, NativeHostAllocationOwner, NativeHostLedgerError,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct AuditAllocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    attempts: usize,
    calls: usize,
    bytes: usize,
    reallocations: usize,
    drops: usize,
    drop_bytes: usize,
    max_alignment: usize,
}

impl Counts {
    const ZERO: Self = Self {
        attempts: 0,
        calls: 0,
        bytes: 0,
        reallocations: 0,
        drops: 0,
        drop_bytes: 0,
        max_alignment: 0,
    };
}

#[derive(Clone, Copy)]
struct AuditState {
    active: bool,
    fail_at: usize,
    counts: Counts,
}

thread_local! {
    static AUDIT: Cell<AuditState> = const { Cell::new(AuditState {
        active: false, fail_at: 0, counts: Counts::ZERO,
    }) };
}

fn allocation_is_refused() -> bool {
    AUDIT
        .try_with(|audit| {
            let mut state = audit.get();
            if !state.active {
                return false;
            }
            state.counts.attempts += 1;
            let refused = state.fail_at != 0 && state.counts.attempts == state.fail_at;
            audit.set(state);
            refused
        })
        .unwrap_or(false)
}

fn record(operation: u8, layout: Layout) {
    let _ = AUDIT.try_with(|audit| {
        let mut state = audit.get();
        if !state.active {
            return;
        }
        match operation {
            0 => {
                state.counts.calls += 1;
                state.counts.bytes += layout.size();
                state.counts.max_alignment = state.counts.max_alignment.max(layout.align());
            }
            1 => state.counts.reallocations += 1,
            2 => {
                state.counts.drops += 1;
                state.counts.drop_bytes += layout.size();
            }
            _ => return,
        }
        audit.set(state);
    });
}

unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if allocation_is_refused() {
            return std::ptr::null_mut();
        }
        // SAFETY: forward the unchanged allocator contract to System.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(0, layout);
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if allocation_is_refused() {
            return std::ptr::null_mut();
        }
        // SAFETY: forward the unchanged allocator contract to System.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record(0, layout);
        }
        pointer
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(1, layout);
        // SAFETY: forward the unchanged allocator contract to System.
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record(2, layout);
        // SAFETY: forward the unchanged allocator contract to System.
        unsafe {
            System.dealloc(pointer, layout);
        }
    }
}

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

fn measure<T>(fail_at: usize, operation: impl FnOnce() -> T) -> (T, Counts) {
    // Warm fixed TLS bookkeeping before the first observed allocation. Counters
    // are thread-local and do not count allocations on other test threads.
    AUDIT.with(|audit| {
        assert!(!audit.get().active);
        audit.set(AuditState { active: true, fail_at, counts: Counts::ZERO });
    });
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            AUDIT.with(|audit| {
                let mut state = audit.get();
                state.active = false;
                audit.set(state);
            });
        }
    }
    let stop = Stop;
    let result = operation();
    drop(stop);
    (result, AUDIT.with(|audit| audit.get().counts))
}

#[test]
fn native_host_ledger_exact_bookkeeping_layout_and_drop_match_actual_allocator() {
    for limit in [0, 1, 3, 17, 1024] {
        let layout =
            NativeHostAllocationLedger::storage_layout(limit).expect("representable table");
        let expected = layout.bytes().expect("complete bookkeeping");
        let (ledger, observed) = measure(0, || {
            NativeHostAllocationLedger::new(limit, expected).expect("exact grant")
        });
        assert_eq!(observed.attempts, 1 + usize::from(limit != 0));
        assert_eq!(observed.calls, observed.attempts);
        assert_eq!(observed.bytes, expected);
        assert_eq!(observed.reallocations, 0);
        assert_eq!(observed.drops, 0);
        let (peak, released) = measure(0, || ledger.finish().expect("complete observed subset"));
        assert_eq!(peak.allocation_bytes(), expected);
        assert_eq!(peak.allocations(), observed.calls);
        assert_eq!(peak.ledger_storage(), layout);
        assert_eq!(peak.process_id(), std::process::id());
        assert_eq!(released.calls, 0);
        assert_eq!(released.reallocations, 0);
        assert_eq!(released.drops, observed.calls);
        assert_eq!(released.drop_bytes, observed.bytes);
    }
}

#[test]
fn native_host_ledger_below_bookkeeping_grant_and_extent_overflow_allocate_nothing() {
    let required = NativeHostAllocationLedger::storage_layout(3)
        .expect("table")
        .bytes()
        .expect("sum");
    let (result, observed) = measure(0, || NativeHostAllocationLedger::new(3, required - 1));
    assert!(
        matches!(result, Err(NativeHostLedgerError::GrantExceeded { required: actual, granted }) if actual == required && granted == required - 1)
    );
    assert_eq!(observed, Counts::ZERO);
    let (result, observed) = measure(0, || {
        NativeHostAllocationLedger::new(usize::MAX, usize::MAX)
    });
    assert!(matches!(result, Err(NativeHostLedgerError::ExtentOverflow)));
    assert_eq!(observed, Counts::ZERO);
}

#[test]
fn native_host_ledger_partial_allocation_failure_releases_exact_initialized_owner() {
    let layout = NativeHostAllocationLedger::storage_layout(17).expect("table");
    let (first, first_counts) = measure(1, || NativeHostAllocationLedger::new(17, usize::MAX));
    assert!(matches!(
        first,
        Err(NativeHostLedgerError::AllocationUnavailable)
    ));
    assert_eq!(first_counts.attempts, 1);
    assert_eq!(first_counts.calls, 0);
    assert_eq!(first_counts.drops, 0);
    let (second, second_counts) = measure(2, || NativeHostAllocationLedger::new(17, usize::MAX));
    assert!(matches!(
        second,
        Err(NativeHostLedgerError::AllocationUnavailable)
    ));
    assert_eq!(second_counts.attempts, 2);
    assert_eq!(second_counts.calls, 1);
    assert_eq!(second_counts.bytes, layout.table.bytes);
    assert_eq!(second_counts.drops, 1);
    assert_eq!(second_counts.drop_bytes, layout.table.bytes);
    assert_eq!(second_counts.reallocations, 0);
}

#[test]
fn native_host_ledger_union_counts_spare_capacity_and_actual_box_string_owners() {
    let ((vector, text, boxed, peak), observed) = measure(0, || {
        let vector = Vec::<u64>::with_capacity(17);
        let text = String::with_capacity(31);
        let boxed = Box::new([0u8; 33]);
        let mut ledger = NativeHostAllocationLedger::new(3, usize::MAX).expect("bounded table");
        ledger.record(vector.host_allocation().expect("vector")).expect("record vector");
        ledger.record(text.host_allocation().expect("string")).expect("record string");
        ledger.record(boxed.host_allocation().expect("box")).expect("record box");
        ledger
            .record(vector.host_allocation().expect("same owner"))
            .expect("shared alias is not charged twice");
        let peak = ledger.finish().expect("all recorded owners");
        (vector, text, boxed, peak)
    });
    assert!(vector.is_empty() && text.is_empty());
    assert_eq!(
        peak.allocation_bytes(),
        observed.bytes,
        "independent GlobalAlloc requested layouts include every owner and ledger storage"
    );
    assert_eq!(peak.allocations(), observed.calls);
    assert_eq!(observed.calls, 5);
    assert_eq!(observed.reallocations, 0);
    let storage_bytes = peak.ledger_storage().bytes().expect("ledger");
    assert_eq!(
        peak.allocation_bytes(),
        storage_bytes + vector.capacity() * 8 + text.capacity() + 33
    );
    assert_eq!(
        observed.drop_bytes, storage_bytes,
        "sealing retires only the two bookkeeping owners"
    );
    let (_, released) = measure(0, || drop((vector, text, boxed)));
    assert_eq!(released.calls, 0);
    assert_eq!(released.drops, 3);
    assert_eq!(released.drop_bytes + observed.drop_bytes, observed.bytes);
}

#[test]
fn native_host_ledger_nested_box_and_vec_storage_are_distinct_without_alias_growth() {
    let ((boxed, peak), observed) = measure(0, || {
        let boxed = Box::new(Vec::<u32>::with_capacity(47));
        let mut ledger = NativeHostAllocationLedger::new(2, usize::MAX).expect("two owners");
        ledger.record(boxed.host_allocation().expect("Box body")).expect("outer owner");
        ledger
            .record(boxed.as_ref().host_allocation().expect("Vec storage"))
            .expect("nested owner");
        for _ in 0..512 {
            ledger
                .record(boxed.host_allocation().expect("same body"))
                .expect("deduplicate body");
            ledger
                .record(boxed.as_ref().host_allocation().expect("same storage"))
                .expect("deduplicate storage");
        }
        let peak = ledger.finish().expect("union");
        (boxed, peak)
    });
    assert_eq!(observed.calls, 4);
    assert_eq!(observed.reallocations, 0);
    assert_eq!(peak.allocations(), observed.calls);
    assert_eq!(peak.allocation_bytes(), observed.bytes);
    let (_, released) = measure(0, || drop(boxed));
    assert_eq!(released.drops, 2);
    assert_eq!(released.drop_bytes + observed.drop_bytes, observed.bytes);
}

#[repr(align(128))]
struct Aligned([u8; 3]);

#[test]
fn native_host_ledger_dynamic_box_layout_preserves_requested_alignment() {
    let ((boxed, peak), observed) = measure(0, || {
        let boxed: Box<[u64]> = Box::new([1, 2, 3]);
        let aligned = Box::new(Aligned([1, 2, 3]));
        let mut ledger = NativeHostAllocationLedger::new(2, usize::MAX).expect("two layouts");
        ledger
            .record(boxed.host_allocation().expect("unsized box"))
            .expect("slice owner");
        ledger
            .record(aligned.host_allocation().expect("aligned box"))
            .expect("aligned owner");
        let peak = ledger.finish().expect("union");
        ((boxed, aligned), peak)
    });
    assert_eq!(boxed.0.as_ref(), [1, 2, 3]);
    assert_eq!(boxed.1 .0, [1, 2, 3]);
    assert_eq!(observed.max_alignment, 128);
    assert_eq!(observed.calls, 4);
    assert_eq!(observed.reallocations, 0);
    assert_eq!(peak.allocation_bytes(), observed.bytes);
}

#[test]
fn native_host_ledger_empty_and_zero_sized_owners_do_not_consume_slots() {
    let ((owners, peak), observed) = measure(0, || {
        let vector = vec![(); 17];
        let string = String::new();
        let boxed = Box::new(());
        let mut ledger = NativeHostAllocationLedger::new(0, usize::MAX).expect("no table slots");
        ledger
            .record(vector.host_allocation().expect("ZST vector"))
            .expect("no allocation");
        ledger
            .record(string.host_allocation().expect("empty string"))
            .expect("no allocation");
        ledger.record(boxed.host_allocation().expect("ZST Box")).expect("no allocation");
        let peak = ledger.finish().expect("empty subset");
        ((vector, string, boxed), peak)
    });
    assert_eq!(owners.0.len(), 17);
    assert_eq!(observed.calls, 1, "only boxed ledger body allocates");
    assert_eq!(peak.allocation_bytes(), observed.bytes);
    assert_eq!(peak.allocations(), 1);
}

#[test]
fn native_host_ledger_ignored_owner_limit_failure_cannot_publish_a_truncated_subset() {
    let first = vec![1u8; 3];
    let second = vec![2u8; 5];
    let mut ledger = NativeHostAllocationLedger::new(1, usize::MAX).expect("one slot");
    let (_, observed) = measure(0, || {
        ledger.record(first.host_allocation().expect("first")).expect("one owner");
        assert_eq!(
            ledger.record(second.host_allocation().expect("second")),
            Err(NativeHostLedgerError::OwnerLimitExceeded { limit: 1 })
        );
        assert_eq!(
            ledger.record(first.host_allocation().expect("same first")),
            Err(NativeHostLedgerError::OwnerLimitExceeded { limit: 1 })
        );
    });
    assert_eq!(observed, Counts::ZERO);
    assert_eq!(
        ledger.finish(),
        Err(NativeHostLedgerError::OwnerLimitExceeded { limit: 1 })
    );
}

#[test]
fn native_host_ledger_union_grant_failure_is_sticky_and_recording_allocates_nothing() {
    let vector = Vec::<u64>::with_capacity(17);
    let storage = NativeHostAllocationLedger::storage_layout(1)
        .expect("table")
        .bytes()
        .expect("sum");
    let required = storage + vector.capacity() * 8;
    let mut ledger = NativeHostAllocationLedger::new(1, required - 1).expect("bookkeeping fits");
    let (result, observed) = measure(0, || {
        ledger.record(vector.host_allocation().expect("actual capacity"))
    });
    let expected = NativeHostLedgerError::GrantExceeded { required, granted: required - 1 };
    assert_eq!(result, Err(expected));
    assert_eq!(observed, Counts::ZERO);
    assert_eq!(ledger.finish(), Err(expected));
}

#[test]
fn native_host_ledger_clap_port_capacity_matches_actual_sdk_allocations() {
    use clack_host::process::audio_buffers::AudioPorts;
    for (channels, ports) in [(0, 0), (1, 1), (3, 2), (17, 3), (511, 17)] {
        let (owner, observed) = measure(0, || AudioPorts::with_capacity(channels, ports));
        let (storage, queried) = measure(0, || {
            owner.allocated_storage().expect("complete SDK capacity")
        });
        assert_eq!(
            queried,
            Counts::ZERO,
            "resource observation must not allocate"
        );
        let actual = storage
            .iter()
            .map(|allocation| allocation.allocation_layout().size())
            .sum::<usize>();
        assert_eq!(
            actual, observed.bytes,
            "independent allocator requested layouts, not public initialized port length"
        );
        assert_eq!(
            observed.calls,
            usize::from(channels != 0) + usize::from(ports != 0)
        );
        assert_eq!(observed.reallocations, 0);
        assert_eq!(
            storage[0].allocation_layout().align(),
            std::mem::align_of::<*mut f32>()
        );
        assert_eq!(
            storage[1].allocation_layout().align(),
            std::mem::align_of::<clap_sys::audio_buffer::clap_audio_buffer>()
        );
        let ledger_bytes = NativeHostAllocationLedger::storage_layout(2)
            .expect("table")
            .bytes()
            .expect("sum");
        let mut ledger =
            NativeHostAllocationLedger::new(2, ledger_bytes + actual).expect("exact known grant");
        let (_, repeated) = measure(0, || {
            for _ in 0..512 {
                ledger.record_clap_audio_ports(&owner).expect("unchanged two SDK owners");
            }
        });
        assert_eq!(repeated, Counts::ZERO);
        let peak = ledger.finish().expect("known union");
        assert_eq!(peak.allocation_bytes(), ledger_bytes + observed.bytes);
        assert_eq!(peak.allocations(), 2 + observed.calls);
        let (_, released) = measure(0, || drop(owner));
        assert_eq!(released.calls, 0);
        assert_eq!(released.drops, observed.calls);
        assert_eq!(released.drop_bytes, observed.bytes);
    }
}

#[test]
fn native_host_ledger_clap_port_views_preserve_capacity_and_allocate_nothing() {
    use clack_host::process::audio_buffers::{
        AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
    };
    let mut input = AudioPorts::with_capacity(3, 2);
    let mut output = AudioPorts::with_capacity(3, 2);
    let initial_input = input.allocated_storage().expect("input storage").map(|allocation| {
        (
            allocation.allocation_base() as usize,
            allocation.allocation_layout(),
        )
    });
    let initial_output = output.allocated_storage().expect("output storage").map(|allocation| {
        (
            allocation.allocation_base() as usize,
            allocation.allocation_layout(),
        )
    });
    let mut input_0 = [1.0f32; 17];
    let mut input_1 = [2.0f32; 17];
    let mut input_2 = [3.0f32; 17];
    let mut input_unused = [0.0f32; 17];
    let mut output_0 = [0.0f32; 17];
    let mut output_1 = [0.0f32; 17];
    let mut output_2 = [0.0f32; 17];
    let mut output_unused = [0.0f32; 17];
    let (_, observed) = measure(0, || {
        for iteration in 0..512 {
            let active_ports = iteration % 3;
            let input_buffers = input.with_input_buffers(
                [
                    AudioPortBuffer {
                        channels: AudioPortBufferType::f32_input_only(
                            [
                                InputChannel::variable(&mut input_0),
                                InputChannel::variable(&mut input_1),
                            ]
                            .into_iter()
                            .take(2),
                        ),
                        latency: 0,
                    },
                    AudioPortBuffer {
                        channels: AudioPortBufferType::f32_input_only(
                            [
                                InputChannel::variable(&mut input_2),
                                InputChannel::variable(&mut input_unused),
                            ]
                            .into_iter()
                            .take(1),
                        ),
                        latency: 0,
                    },
                ]
                .into_iter()
                .take(active_ports),
            );
            let mut output_buffers = output.with_output_buffers(
                [
                    AudioPortBuffer {
                        channels: AudioPortBufferType::f32_output_only(
                            [&mut output_0[..], &mut output_1[..]].into_iter().take(2),
                        ),
                        latency: 0,
                    },
                    AudioPortBuffer {
                        channels: AudioPortBufferType::f32_output_only(
                            [&mut output_2[..], &mut output_unused[..]].into_iter().take(1),
                        ),
                        latency: 0,
                    },
                ]
                .into_iter()
                .take(active_ports),
            );
            assert_eq!(input_buffers.port_count() as usize, active_ports);
            assert_eq!(output_buffers.port_count() as usize, active_ports);
            let expected_frames = (active_ports != 0).then_some(17);
            assert_eq!(input_buffers.frames_count(), expected_frames);
            assert_eq!(output_buffers.frames_count(), expected_frames);
            if active_ports != 0 {
                assert_eq!(input_buffers.as_raw_buffers()[0].channel_count, 2);
                assert_eq!(output_buffers.as_raw_buffers()[0].channel_count, 2);
            }
            if active_ports == 2 {
                assert_eq!(input_buffers.as_raw_buffers()[1].channel_count, 1);
                assert_eq!(output_buffers.as_raw_buffers()[1].channel_count, 1);
            }
        }
    });
    assert_eq!(observed, Counts::ZERO);
    assert_eq!(
        input.allocated_storage().expect("retained input storage").map(|allocation| {
            (
                allocation.allocation_base() as usize,
                allocation.allocation_layout(),
            )
        }),
        initial_input
    );
    assert_eq!(
        output.allocated_storage().expect("retained output storage").map(|allocation| {
            (
                allocation.allocation_base() as usize,
                allocation.allocation_layout(),
            )
        }),
        initial_output
    );
    assert_eq!(input_0, [1.0; 17]);
    assert_eq!(input_1, [2.0; 17]);
    assert_eq!(input_2, [3.0; 17]);
}
