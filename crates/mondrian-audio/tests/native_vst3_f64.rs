//! Owned Float64-only negotiation through the real Product and public Runtime.
//! This is a Float32 publication/conversion gate, not a realtime or vendor claim.
#![cfg(windows)]

use mondrian_audio::{
    AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioDecodedSource, AudioExecutionError,
    AudioMediaResolver, AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease,
    AudioProcessingMode, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant,
    AudioSessionCursorEvidence, AudioStateEntry, DiscoveredVst3AudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExecutionCancellationToken, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

const CLASS: &str = "B64C8B1910234F819CA26D315F8702A1";
// Eight stereo frames. These are independent source words, not SDK output.
const INPUT: [u32; 16] = [
    0x3f800000, 0xbf800000, 0x3f800001, 0xbf800001, 0x3e800000, 0xbe800000, 0x3fc00000, 0xbfc00000,
    0x40000000, 0xc0000000, 0x40000001, 0xc0000001, 0x3f000000, 0xbf000000, 0x3f400000, 0xbf400000,
];
// Realtime f64 adds signed 2^-24 at gain one. At +/-1 the midpoint
// rounds to even mantissa zero; the next input rounds to even mantissa two.
// The .25/.5/.75 rows also detect unchanged PCM.
const EXPECTED_REALTIME: [u32; 16] = [
    0x3f800000, 0xbf800000, 0x3f800002, 0xbf800002, 0x3e800002, 0xbe800002, 0x3fc00000, 0xbfc00000,
    0x40000000, 0xc0000000, 0x40000001, 0xc0000001, 0x3f000001, 0xbf000001, 0x3f400001, 0xbf400001,
];

// Offline uses signed 3*2^-24. These different, independent words also
// require the actual native process mode, not just a successful setter call.
const EXPECTED_OFFLINE: [u32; 16] = [
    0x3f800002, 0xbf800002, 0x3f800002, 0xbf800002, 0x3e800006, 0xbe800006, 0x3fc00002, 0xbfc00002,
    0x40000001, 0xc0000001, 0x40000002, 0xc0000002, 0x3f000003, 0xbf000003, 0x3f400003, 0xbf400003,
];

fn input_file(name: &str) -> (PathBuf, File) {
    let path = PathBuf::from(std::env::var_os(name).expect("explicit Product/fixture input"));
    assert!(path.is_absolute() && path.is_file(), "{name}");
    let lease = OpenOptions::new()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ: stable bytes through every child Drop.
        .open(&path)
        .expect("retained native input lease");
    (path, lease)
}

#[derive(Clone)]
struct Media(Arc<AtomicUsize>);
impl AudioMediaResolver for Media {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        if rate != 48_000 {
            return Err("fixed source rate".into());
        }
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(self.clone()),
        ))
    }
}
impl AudioDecodedSource for Media {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        output: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if cancellation.is_canceled() || start < 0 || output.len() != frames * 2 {
            return Err("source cancellation/extent".into());
        }
        let start = usize::try_from(start).map_err(|_| "source index")?;
        let end = start.checked_add(frames).ok_or("source overflow")?;
        if end > INPUT.len() / 2 {
            return Err("source outside eight-frame qualification".into());
        }
        self.0.fetch_add(1, Ordering::Relaxed);
        for (out, bits) in output.iter_mut().zip(&INPUT[start * 2..end * 2]) {
            *out = f32::from_bits(*bits);
        }
        Ok(())
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
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        other => panic!("unexpected actual processor cursor: {other:?}"),
    };
    runtime
        .native_owner_lease(AudioNativeProcessorAddress {
            root: cursors.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch,
        })
        .expect("actual installed Runtime lease")
}
fn settle_author(runtime: &mut AudioProgramRuntime) {
    if let Some(batch) = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("original owner Poll")
    {
        assert!(batch.author.is_none(), "no fabricated author Ack");
        if let Some(logs) = batch.diagnostics {
            assert!(!logs.loss().incomplete());
            assert!(logs.records().is_empty(), "no unexpected native diagnostic");
        }
    }
    runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .expect("actual closed author Source");
}

#[test]
#[ignore = "current normal Product, rebuilt owned Float64-only class, original outer native Job"]
fn installed_vst3_float64_only_publishes_literal_f32_pcm() {
    let mut in_job = 0;
    // SAFETY: current process pseudo handle and call-local BOOL are valid.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "original outer Job owns every native descendant");
    let (helper, _helper_lease) = input_file("MONDRIAN_VST3_TEST_HELPER");
    let (plugin, _plugin_lease) = input_file("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN");
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("real Product discovery"),
    );
    assert_eq!(
        catalog.descriptors().iter().filter(|d| d.class_id == CLASS).count(),
        1
    );
    let mut observations = Vec::new();
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: mode,
            processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
            public_output_lookahead_budget_frames: 0,
            compensation_delay_scratch_budget_bytes: 0,
        };
        let instance = catalog
            .create_instance(CLASS, render, None)
            .expect("actual Float64-only capture and same-mode probe");
        let instance_id = instance.id;
        assert!(!instance.bypassed && instance.opaque_state.is_some());
        assert_eq!(instance.parameters.len(), 1);
        assert!(instance.parameters.values().all(|p| p.automation.is_none()));
        let mut sequence = Sequence::new("Float64-only publication");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        sequence
            .add_media_audio_clip(
                sequence.audio_tracks[0].id,
                Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
                AudioSourceComponentId::primary(),
            )
            .unwrap();
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        let reads = Arc::new(AtomicUsize::new(0));
        let resolver = IsolatedAudioProcessorResolver::new(catalog.clone());
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &Media(reads.clone()),
            &resolver,
            render,
            None,
            AudioRuntimeResourceGrant::new(1, 32 * 1024 * 1024, 16 * 1024 * 1024),
        )
        .expect("public Runtime uses original Factory and isolated Product");
        assert_eq!(runtime.resource_footprint().runtime_occurrences, 1);
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
        let cold = owner(&runtime);
        assert_eq!(cold.editor_target().instance, instance_id);
        assert_eq!(cold.editor_target().continuity_epoch, None);
        let object = runtime.observe_native_owner_object(cold).expect("authenticated cold object");
        assert_ne!(object.process_id().get(), std::process::id());
        // SAFETY: authentic current owner PID; query/synchronize only. Runtime
        // retains all shutdown authority and its original graceful deadline.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                object.process_id().get(),
            )
        };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut child_in_job = 0;
            // SAFETY: query-only handle to the exact authenticated child.
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
            assert_ne!(
                child_in_job, 0,
                "original outer Job retains the native child"
            );
            settle_author(&mut runtime);
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .expect("real Runtime Enter0");
            let lease = owner(&runtime);
            assert_eq!(lease.editor_target().instance, instance_id);
            assert_eq!(lease.editor_target().continuity_epoch, Some(1));
            assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
            let mut pcm = Vec::new();
            for start in [0, 4] {
                let mut window = [f32::NAN; 8];
                runtime
                    .render_into(
                        AudioRenderRequest { start_sample: start, frames: 4 },
                        &mut window,
                    )
                    .expect("same-owner public Enter/Continue PCM window");
                assert_eq!(owner(&runtime), lease);
                assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
                pcm.extend(window.map(f32::to_bits));
            }
            assert!(
                reads.load(Ordering::Relaxed) > 0,
                "real decoded source was consumed"
            );
            settle_author(&mut runtime);
            pcm
        }));
        let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        drop(runtime);
        let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        let mut exit = u32::MAX;
        let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
        // Failure cleanup never replaces the original target panic.
        let pcm = match target {
            Ok(pcm) => pcm,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        assert_eq!(alive, WAIT_TIMEOUT);
        assert_eq!(
            settled, WAIT_OBJECT_0,
            "original Runtime Drop reaped this exact child"
        );
        assert_ne!(queried, 0);
        assert_eq!(exit, 0);
        observations.push((
            mode,
            object.process_id().get(),
            object.object_serial().get(),
            pcm,
        ));
    }
    // Keep both complete PCM observations and retire both native owners before
    // the independent literal oracle can fail. No partial/suffix PASS is given.
    println!(
        "MONDRIAN_VST3_F64_PCM={}",
        serde_json::json!({
            "class_id": CLASS, "input_bits": INPUT,
            "expected_realtime_bits": EXPECTED_REALTIME, "expected_offline_bits": EXPECTED_OFFLINE,
            "observations": observations.iter().map(|(mode, pid, serial, pcm)| serde_json::json!({
                "mode": format!("{mode:?}"), "pid": pid, "object_serial": serial, "actual_bits": pcm,
                "public_windows": [[0,4],[4,4]], "original_drop_exit": 0,
            })).collect::<Vec<_>>(),
            "realtime_performance_or_third_party_qualified": false,
        })
    );
    for (mode, _, _, pcm) in observations {
        let expected = match mode {
            AudioProcessingMode::Realtime => EXPECTED_REALTIME,
            AudioProcessingMode::Offline => EXPECTED_OFFLINE,
        };
        assert_eq!(pcm.len(), expected.len());
        for (sample, (actual, expected)) in pcm.iter().zip(expected).enumerate() {
            assert_eq!(
                *actual, expected,
                "{mode:?} literal interleaved sample {sample}"
            );
        }
    }
}
