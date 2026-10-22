//! A real main-thread timer handshake while the parent sends no native IPC.
#![cfg(windows)]

use mondrian_audio::{
    AudioProcessingMode, AudioProcessorFactory, AudioProcessorInsertionPoint,
    AudioProcessorOccurrence, AudioProcessorOccurrenceOwner, AudioProcessorPrepareRequest,
    AudioRenderContract, DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorFactory,
    IsolatedAudioProcessorSpecResolver, NativeAudioObservationConsumption,
};
use mondrian_core::{AudioChannelLayout, ProgramOutputId};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{GetLastError, SetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, SetEvent, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

// Null security attributes avoid adding Win32_Security to the workspace feature
// union. The event remains non-inheritable; both calls request minimal access.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventExW(
        attributes: *const c_void,
        name: *const u16,
        flags: u32,
        access: u32,
    ) -> *mut c_void;
}
fn event(pid: u32, nonce: [u8; 16], kind: &str, manual: bool, access: u32) -> OwnedHandle {
    let name: Vec<u16> = format!(
        "Local\\MondrianClapIdle-{pid}-{:032x}-{kind}",
        u128::from_le_bytes(nonce)
    )
    .encode_utf16()
    .chain([0])
    .collect();
    // SAFETY: null attributes, live NUL-terminated UTF16 name, and valid event
    // flags/access. A successful call transfers exactly one owned kernel handle.
    unsafe { SetLastError(0) };
    let raw = unsafe { CreateEventExW(std::ptr::null(), name.as_ptr(), u32::from(manual), access) };
    let error = unsafe { GetLastError() };
    assert!(!raw.is_null(), "create {kind} event: {error}");
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    assert_ne!(
        error, 183,
        "witness event must not reuse an existing object"
    );
    handle
}
fn input(name: &str) -> (PathBuf, File) {
    let path = PathBuf::from(std::env::var_os(name).expect("explicit native gate input"));
    assert!(path.is_absolute() && path.is_file());
    let lease = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .expect("retain stable Product/fixture bytes through original Drop");
    (path, lease)
}

#[test]
#[ignore = "current Product, rebuilt owned CLAP mode15, original outer native Job"]
fn installed_clap_timer_wakes_main_without_ipc() {
    let mut in_job = 0;
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "the original Job must own native descendants");
    let (helper, _helper_lease) = input("MONDRIAN_CLAP_TEST_HELPER");
    let (plugin, _plugin_lease) = input("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN");
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("real Product discovery");
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        public_output_lookahead_budget_frames: 0,
        compensation_delay_scratch_budget_bytes: 0,
    };
    let nonce = *uuid::Uuid::new_v4().as_bytes();
    let mut state = vec![15];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    state.extend_from_slice(&nonce);
    let instance = catalog
        .create_instance(
            "org.mondrian.fixture.sidechain",
            render,
            Some(state.clone()),
        )
        .expect("original inactive state/render/activation probe");
    assert!(instance.parameters.is_empty());
    assert_eq!(
        instance.opaque_state.as_ref().map(AsRef::as_ref),
        Some(state.as_slice())
    );
    let spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: instance.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            render,
        ))
        .expect("original native Factory metadata");
    let factory = IsolatedAudioProcessorFactory::prepare(spec).expect("original isolated Factory");
    let mut processor = factory.create(None).expect("one retained native owner");
    // This is the LAST initial parent IPC. Events do not exist yet, so neither
    // metadata probes nor a callback inside that RPC can publish a witness.
    let object = processor
        .native_audio_object_observation()
        .unwrap()
        .expect("actual native object");
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    assert_eq!(object.object_serial().get(), 1);
    // Query/synchronize only; no kill/shutdown authority leaves the original owner.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut child_in_job = 0;
        assert_ne!(
            unsafe {
                IsProcessInJob(
                    handle.as_raw_handle().cast(),
                    std::ptr::null_mut(),
                    &mut child_in_job,
                )
            },
            0
        );
        assert_ne!(child_in_job, 0);
        // NO PARENT IPC from the first event creation through the second wait.
        // Arm: manual-reset, EVENT_MODIFY_STATE only. Witness: auto-reset,
        // SYNCHRONIZE only; this parent cannot SetEvent on its Witness handle.
        let arm = event(pid, nonce, "Arm", true, 2);
        let witness = event(pid, nonce, "Witness", false, 0x0010_0000);
        assert_eq!(
            unsafe { WaitForSingleObject(witness.as_raw_handle().cast(), 5_000) },
            WAIT_OBJECT_0,
            "first real callback must observe Arm=false on plugin main"
        );
        assert_eq!(
            unsafe { WaitForSingleObject(witness.as_raw_handle().cast(), 0) },
            WAIT_TIMEOUT,
            "first callback cannot publish a second witness"
        );
        assert_ne!(unsafe { SetEvent(arm.as_raw_handle().cast()) }, 0);
        assert_eq!(
            unsafe { WaitForSingleObject(witness.as_raw_handle().cast(), 5_000) },
            WAIT_OBJECT_0,
            "a subsequent IPC-free timer tick must see Arm=true and unregister"
        );
        // END of the no-IPC interval. Only now may ordinary Poll collect the
        // original worker's logs; a Poll-triggered callback cannot rescue a wait.
        let batch = processor
            .poll_native_observations(NativeAudioObservationConsumption::Drain)
            .expect("original bounded post-witness Poll");
        assert_eq!(batch.object, Some(object));
        assert_eq!(batch.owner_process_id(), Some(object.process_id()));
        assert!(!batch.logs.incomplete());
        let expected = [
            "idle:registered",
            "idle:disarmed:main=true:audio=false",
            "idle:armed:main=true:audio=false:unregistered",
        ];
        assert_eq!(batch.logs.records().len(), expected.len());
        for (index, (record, expected)) in batch.logs.records().iter().zip(expected).enumerate() {
            assert_eq!(record.sequence(), index as u64);
            assert_eq!(record.raw_severity(), 1);
            assert_eq!(record.text(), Ok(expected));
            assert!(!record.truncated() && !record.invalid_utf8());
        }
        for _ in 0..2 {
            let after = processor
                .poll_native_observations(NativeAudioObservationConsumption::Drain)
                .unwrap();
            assert_eq!(after.object, Some(object));
            assert!(!after.logs.incomplete());
            assert!(
                after.logs.records().is_empty(),
                "wrong/repeated callback must fail"
            );
        }
    }));
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(processor);
    drop(factory);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = u32::MAX;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    // Every wait/target failure reaches original Drop before propagating. An
    // observer timeout changes neither the native 100ms policy nor Job budgets.
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(alive, WAIT_TIMEOUT);
    assert_eq!(settled, WAIT_OBJECT_0);
    assert_ne!(queried, 0);
    assert_eq!(exit, 0);
    println!(
        "MONDRIAN_CLAP_IDLE_TIMER_WITNESS={}",
        serde_json::json!({
            "mode": "Offline", "pid": pid, "object_serial": object.object_serial().get(),
            "no_ipc_callback_witnesses": 2, "original_drop_exit": exit,
            "pcm_gui_cadence_realtime_or_100ms_qualified": false,
        })
    );
}
