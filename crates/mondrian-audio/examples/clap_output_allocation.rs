//! Qualification only: exact System allocator requests of the production SDK callback owner.
//! This does not measure an audio deadline, OS/third-party heaps or a complete root grant.
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::{ClapId, Pckn};
use mondrian_audio::clap_output_events::*;
use mondrian_audio::ClapParameterDescriptor;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct ProbeAllocator;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
// SAFETY: every operation forwards its unchanged Rust allocation contract to
// System; the independent counters neither change ownership nor inspect memory.
unsafe impl GlobalAlloc for ProbeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: forward the unchanged valid layout to the actual System allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: forward the unchanged valid layout to the actual System allocator.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        // SAFETY: preserve the allocation/layout/new-size arguments supplied by Rust.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: return the same System-owned allocation with its original layout.
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;

fn start() {
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
}
fn finish() -> (usize, usize) {
    ACTIVE.store(false, Ordering::Relaxed);
    (CALLS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed))
}
fn main() {
    let parameters = [ClapParameterDescriptor {
        id: 7,
        name: "Gain".to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 0.5,
        current_value: 0.5,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    }];
    let expected =
        ClapOutputEvents::reservation_bytes(parameters.len()).expect("exact requested owners");
    start();
    let mut owner = ClapOutputEvents::new(
        &parameters,
        std::sync::Arc::new(mondrian_audio::ClapParameterSource::new()),
    )
    .expect("preallocated production source");
    let preparation = finish();
    // Definition validation also has its existing BTreeSet owner; compare the
    // actual owner layouts separately instead of pretending all prepare bytes are events.
    assert_eq!(
        owner.allocated_bytes().expect("actual capacities"),
        expected
    );
    {
        let mut writer = owner
            .writer(ClapOutputOrigin::InactiveFlush)
            .expect("exclusive callback publication");
        writer
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(7),
                Pckn::match_all(),
                0.5,
            ))
            .expect("warm standard FFI callback");
    }
    start();
    for index in 1..CLAP_OUTPUT_EVENTS {
        let mut writer = owner
            .writer(ClapOutputOrigin::Process {
                start_sample: index as i64,
                sample_rate: 48_000,
                frames: 4,
            })
            .expect("exclusive callback publication");
        writer
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(7),
                Pckn::match_all(),
                0.5,
            ))
            .expect("full admitted source");
    }
    let full = {
        let mut writer = owner
            .writer(ClapOutputOrigin::ActiveFlush)
            .expect("exclusive callback publication");
        writer
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(7),
                Pckn::match_all(),
                0.75,
            ))
            .is_err()
    };
    let callback = finish();
    assert!(full);
    assert_eq!(owner.events().len(), CLAP_OUTPUT_EVENTS);
    assert_eq!(
        callback,
        (0, 0),
        "production OutputEvents callback must not allocate even at refusal"
    );
    println!("{{\"schema\":1,\"status\":\"source_owner_allocation_control\",\"owner_bytes\":{expected},\"prepare_allocator_calls\":{},\"prepare_requested_bytes\":{},\"callback_allocator_calls\":{},\"callback_requested_bytes\":{},\"accepted_events\":{}}}",preparation.0,preparation.1,callback.0,callback.1,owner.events().len());
}
