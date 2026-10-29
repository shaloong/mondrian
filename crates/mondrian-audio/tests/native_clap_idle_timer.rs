//! A real main-thread timer handshake while the parent sends no native IPC.
#![cfg(windows)]

use mondrian_audio::{
    AudioContinuityEvidenceVisitor, AudioExecutionError, AudioMediaResolver,
    AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease, AudioProcessingMode,
    AudioProcessorCursorEvidence, AudioProcessorCursorState, AudioProgramRuntime,
    AudioRenderContract, AudioRuntimeResourceGrant, AudioSessionCursorEvidence,
    DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExecutionCancellationToken, NativeAudioDiagnosticMessageFacts,
};
use mondrian_timeline::Sequence;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::Arc;
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

// The public compiler constructs the request and original isolated Factory.
// An empty media graph still owns its authored terminal processor; no PCM is run.
struct NoMedia;
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Err("idle qualification must not resolve media".into())
    }
}
#[derive(Default)]
struct Cursors {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, value: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.sessions.push(value);
        Ok(())
    }
    fn processor(
        &mut self,
        value: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.processors.push(value);
        Ok(())
    }
}
fn owner(runtime: &AudioProgramRuntime) -> AudioNativeProcessorOwnerLease {
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).unwrap();
    assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
    let processor = cursors.processors[0];
    assert_eq!(processor.state, AudioProcessorCursorState::Unentered);
    runtime
        .native_owner_lease(AudioNativeProcessorAddress {
            root: cursors.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch: None,
        })
        .expect("actual installed Unentered Runtime lease")
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
        // Original timer contract's finite PDC preparation allowances admit
        // the unchanged mode15 latency3; the test never enters or renders PCM.
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
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
    let instance_id = instance.id;
    let mut sequence = Sequence::new("IPC-free CLAP timer");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let resolver = IsolatedAudioProcessorResolver::new(Arc::new(catalog));
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &NoMedia,
        &resolver,
        render,
        None,
        AudioRuntimeResourceGrant::new(1, 32 * 1024 * 1024, 16 * 1024 * 1024),
    )
    .expect("public Runtime constructs the real prepare request and Factory");
    let installation = AudioOwnerInstallationSource::new();
    let (registration, verifier) = installation.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = installation
        .reserve_install(&registration, None)
        .and_then(|reservation| installation.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .unwrap();
    let lease = owner(&runtime);
    assert_eq!(lease.editor_target().instance, instance_id);
    // This is the LAST initial parent IPC. Events do not exist yet, so neither
    // metadata probes nor a callback inside that RPC can publish a witness.
    let object = runtime.observe_native_owner_object(lease).expect("actual native object");
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
        let batch = runtime
            .poll_native_observations(&ExecutionCancellationToken::new())
            .expect("original bounded post-witness Poll")
            .expect("actual diagnostic source");
        assert!(
            batch.author.is_none(),
            "no fabricated author acknowledgement"
        );
        let logs = batch.diagnostics.expect("the three real callback records");
        let source = logs.source();
        let target = lease.editor_target();
        assert_eq!(source.process_id, object.process_id());
        assert_eq!(source.instance, instance_id);
        assert_eq!(
            (source.root_plan, source.root_session),
            (target.root_plan, target.root_session)
        );
        assert_eq!(
            (source.local_plan, source.local_session),
            (target.local_plan, target.local_session)
        );
        assert!(!logs.loss().incomplete());
        let expected = [
            "idle:registered",
            "idle:disarmed:main=true:audio=false",
            "idle:armed:main=true:audio=false:unregistered",
        ];
        assert_eq!(logs.records().len(), expected.len());
        for (index, (record, expected)) in logs.records().iter().zip(expected).enumerate() {
            assert_eq!(record.sequence(), index as u64);
            assert_eq!(record.raw_severity(), 1);
            assert_eq!(record.bytes(), expected.as_bytes());
            assert_eq!(record.facts(), NativeAudioDiagnosticMessageFacts::default());
        }
        for _ in 0..2 {
            if let Some(after) =
                runtime.poll_native_observations(&ExecutionCancellationToken::new()).unwrap()
            {
                assert!(after.author.is_none());
                if let Some(logs) = after.diagnostics {
                    assert!(!logs.loss().incomplete());
                    assert!(
                        logs.records().is_empty(),
                        "wrong/repeated callback must fail"
                    );
                }
            }
        }
        assert_eq!(owner(&runtime), lease);
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
    }));
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(runtime);
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
