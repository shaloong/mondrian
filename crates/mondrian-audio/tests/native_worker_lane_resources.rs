//! New-API request qualification; actual native lane/Ready gates are separate.

use mondrian_audio::{
    NativeHostBufferLayout, NativeWorkerControlOverlapLayout, NativeWorkerLaneOwnedLayouts,
    NativeWorkerLaneResourceError, NativeWorkerLaneResourceProfile, NativeWorkerLaneStorageKind,
    SCOPED_NATIVE_DSP_STACK_BYTES, SCOPED_NATIVE_READER_STACK_BYTES,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct ObservedAllocator;
thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: the wrapper preserves System's pointer, layout and allocator contract.
// Observation uses preinitialized inline thread-local cells and never allocates.
unsafe impl GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ENABLED.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwarded unchanged from GlobalAlloc's caller.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout belong to this same forwarding allocator.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ENABLED.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwarded unchanged from GlobalAlloc's caller.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, length: usize) -> *mut u8 {
        ENABLED.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: pointer/layout/new size retain GlobalAlloc's preconditions.
        unsafe { System.realloc(pointer, layout, length) }
    }
}

#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

fn observed<T: ?Sized>(owner: &T) -> NativeHostBufferLayout {
    let layout = Layout::for_value(owner);
    NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() }
}

fn empty() -> NativeHostBufferLayout {
    NativeHostBufferLayout { bytes: 0, alignment: 1 }
}

fn owners(body: NativeHostBufferLayout) -> NativeWorkerLaneOwnedLayouts {
    NativeWorkerLaneOwnedLayouts {
        reader_slots: empty(),
        processing_slots: empty(),
        main_inline: empty(),
        runtime_main_role: body,
        runtime_dsp_role: empty(),
        sdk_main_role: empty(),
        sdk_dsp_role: empty(),
    }
}

fn control(decoded: NativeHostBufferLayout) -> NativeWorkerControlOverlapLayout {
    NativeWorkerControlOverlapLayout {
        decoded_input: decoded,
        reply_storage: empty(),
        staged_seed: empty(),
    }
}

#[test]
fn native_worker_lane_profile_complete_box_and_actual_vec_capacity() {
    let role: Box<dyn std::fmt::Debug> = Box::new([23_u64; 513]);
    let mut payload = Vec::<u8>::with_capacity(1057);
    payload.extend_from_slice(b"short");
    let role_layout = observed(role.as_ref());
    let payload_layout =
        NativeHostBufferLayout::for_vector(&payload).expect("complete Vec owner layout");
    assert!(role_layout.bytes > std::mem::size_of_val(&role));
    assert!(payload_layout.bytes > payload.len());
    let profile = NativeWorkerLaneResourceProfile::scoped_main_dsp(
        owners(role_layout),
        control(payload_layout),
    )
    .expect("known request");
    assert_eq!(
        profile.known_storage_limit_bytes(),
        role_layout.bytes + payload.capacity()
    );
    assert_eq!(
        profile.requested_stack_reservation_bytes(),
        SCOPED_NATIVE_READER_STACK_BYTES + SCOPED_NATIVE_DSP_STACK_BYTES
    );
    assert_eq!(
        profile.additional_session_reservation_bytes(),
        profile.known_storage_limit_bytes() + profile.requested_stack_reservation_bytes()
    );
    profile
        .admit_observed_owners(owners(role_layout), control(payload_layout))
        .expect("same actual owners");
}

#[test]
fn native_worker_lane_profile_actual_capacity_refuses_before_publication() {
    let mut payload = Vec::<u8>::with_capacity(37);
    payload.extend_from_slice(b"same logical length");
    let admitted = NativeHostBufferLayout::for_vector(&payload).expect("initial allocation");
    let profile =
        NativeWorkerLaneResourceProfile::scoped_main_dsp(owners(empty()), control(admitted))
            .expect("known request");
    payload
        .try_reserve_exact(admitted.bytes + 127)
        .expect("independent larger allocation");
    let actual = NativeHostBufferLayout::for_vector(&payload).expect("grown actual capacity");
    assert!(actual.bytes > admitted.bytes);
    assert!(
        matches!(profile.admit_observed_owners(owners(empty()), control(actual)),
        Err(NativeWorkerLaneResourceError::ControlCapacityExceeded { owner: NativeWorkerLaneStorageKind::DecodedInput, prepared, observed })
        if prepared == admitted && observed == actual)
    );
}

#[test]
fn native_worker_lane_profile_changed_fixed_owner_and_invalid_layout_refuse() {
    let body = Box::new([3_u64; 15]);
    let profile = NativeWorkerLaneResourceProfile::scoped_main_dsp(
        owners(observed(body.as_ref())),
        control(empty()),
    )
    .expect("fixed actual owner");
    let changed = Box::new([3_u64; 17]);
    assert!(matches!(
        profile.admit_observed_owners(owners(observed(changed.as_ref())), control(empty())),
        Err(NativeWorkerLaneResourceError::FixedOwnerChanged {
            owner: NativeWorkerLaneStorageKind::RuntimeMainRole,
            ..
        })
    ));
    assert!(matches!(
        NativeWorkerLaneResourceProfile::scoped_main_dsp(
            owners(NativeHostBufferLayout { bytes: 5, alignment: 3 }),
            control(empty())
        ),
        Err(NativeWorkerLaneResourceError::InvalidLayout {
            owner: NativeWorkerLaneStorageKind::RuntimeMainRole,
            ..
        })
    ));
}

#[test]
fn native_worker_lane_profile_warmed_observation_allocates_nothing() {
    let body = Box::new([3_u64; 131]);
    let payload = Vec::<u8>::with_capacity(512);
    let fixed = owners(observed(body.as_ref()));
    let control = control(NativeHostBufferLayout::for_vector(&payload).expect("actual capacity"));
    let profile =
        NativeWorkerLaneResourceProfile::scoped_main_dsp(fixed, control).expect("known request");
    ENABLED.with(|value| value.set(false));
    ALLOCATIONS.with(|value| value.set(0));
    // Warm only the test's own observation cells; all owners already exist.
    ENABLED.with(|value| value.set(true));
    let result = (0..1024).try_for_each(|_| profile.admit_observed_owners(fixed, control));
    ENABLED.with(|value| value.set(false));
    let count = ALLOCATIONS.with(Cell::get);
    result.expect("same owners remain admitted");
    assert_eq!(count, 0, "profile comparison has no allocator operation");
}

#[test]
fn native_worker_lane_profile_control_union_accepts_real_typed_alignments() {
    let values = Vec::<f64>::with_capacity(16);
    let seed = Vec::<u8>::with_capacity(128);
    let title: Box<str> = "真实窗口标题".into();
    let actual_values = NativeHostBufferLayout::for_vector(&values).expect("actual f64 capacity");
    let actual_seed = NativeHostBufferLayout::for_vector(&seed).expect("actual byte capacity");
    let actual_title = observed(title.as_ref());
    assert_eq!(actual_values.alignment, std::mem::align_of::<f64>());
    assert_eq!(actual_seed.alignment, std::mem::align_of::<u8>());
    assert_eq!(actual_title.alignment, std::mem::align_of::<u8>());
    assert_eq!(actual_title.bytes, title.len());
    let ceiling = NativeHostBufferLayout { bytes: 256, alignment: std::mem::align_of::<f64>() };
    let profile =
        NativeWorkerLaneResourceProfile::scoped_main_dsp(owners(empty()), control(ceiling))
            .expect("typed union ceiling");
    for (kind, actual) in [
        ("parameters", actual_values),
        ("seed", actual_seed),
        ("title", actual_title),
    ] {
        profile
            .admit_observed_owners(owners(empty()), control(actual))
            .unwrap_or_else(|error| {
                panic!("legal typed {kind} owner is below the same canonical ceiling: {error}")
            });
    }
    assert_eq!(profile.control_overlap().decoded_input, ceiling);
    assert_eq!(
        actual_seed.bytes,
        seed.capacity(),
        "observation was not a logical payload length"
    );
}

#[test]
fn native_worker_lane_profile_control_union_keeps_capacity_and_fixed_layout_refusals() {
    let fixed: Box<u64> = Box::new(42);
    let fixed_layout = observed(fixed.as_ref());
    let ceiling = NativeHostBufferLayout { bytes: 128, alignment: 8 };
    let profile =
        NativeWorkerLaneResourceProfile::scoped_main_dsp(owners(fixed_layout), control(ceiling))
            .expect("valid canonical profile");
    let higher_alignment = Vec::<u128>::with_capacity(1);
    let observed_higher =
        NativeHostBufferLayout::for_vector(&higher_alignment).expect("actual u128 capacity");
    assert!(observed_higher.bytes <= ceiling.bytes);
    assert!(observed_higher.alignment > ceiling.alignment);
    assert!(
        matches!(profile.admit_observed_owners(owners(fixed_layout), control(observed_higher)),
        Err(NativeWorkerLaneResourceError::ControlCapacityExceeded { owner: NativeWorkerLaneStorageKind::DecodedInput, observed, .. }) if observed == observed_higher)
    );
    let larger = Vec::<u8>::with_capacity(129);
    let observed_larger =
        NativeHostBufferLayout::for_vector(&larger).expect("actual oversized byte capacity");
    assert!(
        matches!(profile.admit_observed_owners(owners(fixed_layout), control(observed_larger)),
        Err(NativeWorkerLaneResourceError::ControlCapacityExceeded { owner: NativeWorkerLaneStorageKind::DecodedInput, observed, .. }) if observed == observed_larger)
    );
    let changed_fixed: Box<[u8; 8]> = Box::new([0; 8]);
    let changed_layout = observed(changed_fixed.as_ref());
    assert_eq!(changed_layout.bytes, fixed_layout.bytes);
    assert!(matches!(
        profile.admit_observed_owners(owners(changed_layout), control(empty())),
        Err(NativeWorkerLaneResourceError::FixedOwnerChanged {
            owner: NativeWorkerLaneStorageKind::RuntimeMainRole,
            ..
        })
    ));
    assert!(matches!(
        profile.admit_observed_owners(
            owners(fixed_layout),
            control(NativeHostBufferLayout { bytes: 5, alignment: 3 })
        ),
        Err(NativeWorkerLaneResourceError::InvalidLayout {
            owner: NativeWorkerLaneStorageKind::DecodedInput,
            ..
        })
    ));
}

#[test]
fn native_worker_lane_persistent_input_request_is_independent_and_counted_once() {
    let base = NativeWorkerLaneResourceProfile::scoped_main_dsp(owners(empty()), control(empty()))
        .unwrap();
    let request = base.with_processor_input_storage(24, 64).unwrap();
    assert_eq!(request.processor_input_storage(), (24, 64));
    assert_eq!(
        request.known_storage_limit_bytes(),
        base.known_storage_limit_bytes() + 88
    );
    assert_eq!(
        request.additional_session_reservation_bytes(),
        base.additional_session_reservation_bytes() + 88
    );
    assert_eq!(request.control_overlap(), base.control_overlap());
    assert_eq!(
        request
            .with_processor_input_storage(48, 128)
            .unwrap()
            .known_storage_limit_bytes(),
        base.known_storage_limit_bytes() + 176
    );
    assert_eq!(
        request.with_processor_input_storage(usize::MAX, 1),
        Err(NativeWorkerLaneResourceError::ExtentOverflow)
    );
    assert_eq!(base.processor_input_storage(), (0, 0));
}
