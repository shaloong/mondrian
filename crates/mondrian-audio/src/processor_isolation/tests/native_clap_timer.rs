use super::*;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Instant;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, WaitForSingleObject,
};

#[test]
#[ignore = "current Product, rebuilt owned CLAP timer mode14, and original outer native Job"]
fn installed_clap_timers_run_on_main_and_unregister_without_repeated_callbacks() {
    let mut in_job = 0;
    // SAFETY: current-process pseudo handle and the call-local BOOL are valid.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(
        in_job, 0,
        "the original caller must own the complete native tree"
    );
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current normal Product helper");
    let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("rebuilt owned CLAP timer fixture");
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("original native descriptor discovery");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
        let mut state = vec![14];
        state.extend_from_slice(&0.5_f32.to_le_bytes());
        let instance = catalog
            .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
            .expect("original state/render/activation probe");
        assert!(instance.parameters.is_empty());
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
        let factory =
            IsolatedAudioProcessorFactory::prepare(spec).expect("same owned native Factory");
        let mut processor = factory.create(None).expect("one retained native timer instance");
        let object = processor
            .native_audio_object_observation()
            .expect("actual native observation without draining logs")
            .expect("real retained native object");
        let pid = object.process_id().get();
        assert_ne!(pid, std::process::id());
        assert_eq!(
            object.object_serial().get(),
            1,
            "one native load in this fresh child"
        );
        // SAFETY: the actual native object supplies this PID. Query/synchronize
        // access only observes the same child; its original owner retains all
        // shutdown/kill/reap authority, including its unchanged 100ms deadline.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut child_in_job = 0;
            // SAFETY: the observation handle is live; no new process owner.
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
            // Duplicate unregister follows the pinned SDK's normal Host Log
            // error path too. Preserve those actual error records, rather than
            // hiding them or confusing an expected refusal with evidence loss.
            let expected = [
                (1, "timer:registered:A"),
                (1, "timer:registered:B-distinct"),
                (1, "timer:both-due-before-main-return"),
                (1, "timer:A:main=true:audio=false"),
                (1, "timer:unregistered:B"),
                (1, "timer:unregistered:A"),
                (3, "CLAP timer ID is not registered"),
                (1, "timer:duplicate-unregister:B:refused"),
                (3, "CLAP timer ID is not registered"),
                (1, "timer:duplicate-unregister:A:refused"),
            ];
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut observed = 0;
            while observed < expected.len() {
                let batch = processor
                    .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
                    .expect("original bounded native Poll, not a second collector");
                assert_eq!(
                    batch.object,
                    Some(object),
                    "same retained child/native load"
                );
                assert_eq!(batch.owner_process_id(), Some(object.process_id()));
                assert!(
                    !batch.logs.incomplete(),
                    "timer evidence must not lose records"
                );
                for record in batch.logs.records() {
                    let wanted = expected
                        .get(observed)
                        .expect("B or repeated callback must not add a record");
                    assert_eq!(record.sequence(), observed as u64);
                    assert_eq!(record.raw_severity(), wanted.0);
                    assert_eq!(
                        record.text(),
                        Ok(wanted.1),
                        "{mode:?}, timer record {observed}"
                    );
                    assert!(!record.truncated() && !record.invalid_utf8());
                    observed += 1;
                }
                assert!(
                    Instant::now() < deadline,
                    "bounded timer lifecycle: {mode:?}, records={observed}"
                );
            }
            // The fixture's main callback establishes both deadlines before the
            // host samples one due snapshot. A's callback removes both live
            // slots, so B must be skipped in THAT snapshot. These subsequent
            // actual Poll turns also cross their original one-ms periods and
            // must not deliver either timer again. This does not qualify timer
            // cadence, realtime behavior or independent no-IPC idle triggering.
            let after_period = Instant::now() + Duration::from_millis(2);
            let mut turns = 0;
            loop {
                let batch = processor
                    .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
                    .expect("same actual native main service after unregister");
                assert_eq!(batch.object, Some(object));
                assert_eq!(batch.owner_process_id(), Some(object.process_id()));
                assert!(!batch.logs.incomplete());
                assert!(
                    batch.logs.records().is_empty(),
                    "no B or repeated timer: {batch:?}"
                );
                turns += 1;
                let now = Instant::now();
                assert!(now < deadline, "post-unregister main turns remain bounded");
                if turns >= 2 && now >= after_period {
                    break;
                }
            }
            assert_eq!(
                processor.native_audio_object_observation().unwrap(),
                Some(object)
            );
        }));
        // Always retire the original sole owner before propagating a target
        // panic. The HANDLE only observes Drop; it never kills or extends the
        // original graceful deadline, nor proves that deadline independently.
        let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        drop(processor);
        drop(factory);
        let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        let mut exit = u32::MAX;
        let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
        if let Err(panic) = target {
            std::panic::resume_unwind(panic);
        }
        assert_eq!(
            alive, WAIT_TIMEOUT,
            "same actual child was live before owner Drop"
        );
        assert_eq!(
            settled, WAIT_OBJECT_0,
            "original native owner reaped the child"
        );
        assert_ne!(queried, 0);
        assert_eq!(exit, 0, "same child exits naturally; no manual kill");
    }
}
