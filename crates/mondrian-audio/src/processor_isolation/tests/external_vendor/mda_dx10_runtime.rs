//! Official MIT SDK mda DX10: finite real-instrument Runtime qualification.
//! Metadata plus audible output does not independently observe native input
//! pointers; the separate owned MNE6 gate must verify that exact SDK boundary.
use super::{owner, Cursors};
use crate::processor_isolation::vst3_discovery::probe_vst3_plugin_registration_with_state;
use crate::processor_isolation::{
    scan_vst3_binary_descriptors, DiscoveredVst3AudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver, Vst3EventBusRole, Vst3MainAudioInput,
};
use crate::{
    AudioContinuityEpoch, AudioMediaResolver, AudioProcessingMode, AudioProcessorCursorState,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant,
    AudioStateEntry, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
    AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, MusicalTimeReference,
    TimelineTime,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::Sequence;
use serde_json::json;
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

const CLASS: &str = "5653544D4441786D6461206478313000";
const BLOCK: usize = 256;
const TOTAL: usize = 16_384;
const ON: i64 = 127;
const OFF: i64 = 8_193;

struct NoMedia(Arc<AtomicUsize>);
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("the authored mda instrument score contains no media".into())
    }
}

#[test]
#[ignore = "normal Product, explicit official SDK mda DLL and outer native Job"]
fn external_mda_dx10_vst3_captured_state_notes_without_media() {
    let mut in_job = 0;
    // SAFETY: the pseudo handle is valid and the writable BOOL is call-local.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "the caller must own the complete native tree");
    let file = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let library = file("MONDRIAN_MDA_VST3_PLUGIN");
    let descriptors = scan_vst3_binary_descriptors(&helper, &library).unwrap();
    let matches: Vec<_> = descriptors.iter().filter(|item| item.class_id == CLASS).collect();
    assert_eq!(matches.len(), 1);
    let descriptor = matches[0];
    assert_eq!(descriptor.name, "mda DX10");
    assert_eq!(descriptor.vendor.as_deref(), Some("mda"));
    assert_eq!(descriptor.version.as_deref(), Some("3.8.0.0"));

    for rate in [44_100, 48_000] {
        let render = AudioRenderContract {
            sample_rate: rate,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: BLOCK,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
            ..super::super::test_render_contract()
        };
        let captured =
            probe_vst3_plugin_registration_with_state(&helper, &library, CLASS, render, None, true)
                .expect("actual default-state capture with the strict current Probe");
        let state = captured.captured_state.as_ref().expect("real nonempty plugin state");
        assert!(!state.is_empty() && state.len() <= 64 * 1024);
        let restored = probe_vst3_plugin_registration_with_state(
            &helper,
            &library,
            CLASS,
            render,
            Some(state),
            false,
        )
        .expect("fresh actual native restoration, never a fabricated state payload");
        for probe in [&captured, &restored] {
            assert_eq!(
                probe.registration.main_audio_input,
                Vst3MainAudioInput::Absent
            );
            assert!(probe.registration.auxiliary_inputs.buses.is_empty());
            assert!(probe.input_names.is_empty());
            let events = &probe.registration.event_bus_contract;
            assert!(events.outputs.as_slice().is_empty());
            let rows = events.inputs.as_slice();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].native_index, 0);
            assert_eq!(rows[0].role, Vst3EventBusRole::Main);
            assert_eq!(rows[0].channel_count, 1);
            assert!(rows[0].host_active);
        }
        assert_eq!(
            captured.registration.parameters,
            restored.registration.parameters
        );
        assert_eq!(captured.current_values, restored.current_values);
        assert_eq!(
            captured.registration.event_bus_contract,
            restored.registration.event_bus_contract
        );
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [library.clone()])
                .unwrap(),
        );

        for partition in [&[BLOCK][..], &[31_usize, 225, 127, 129]] {
            let instance = catalog.create_instance(CLASS, render, Some(state.clone())).unwrap();
            assert_eq!(
                instance.opaque_state.as_deref().map(Vec::as_slice),
                Some(state.as_slice())
            );
            let processor_id = instance.id;
            let mut sequence = Sequence::new("official mda DX10 Note-only Runtime");
            sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
            sequence.audio_program.musical_time_reference = Some(
                MusicalTimeReference::new(
                    TimelineTime::new(0, 1).unwrap(),
                    TimelineTime::new(1, 2).unwrap(),
                )
                .unwrap(),
            );
            let track = sequence.audio_tracks[0].id;
            sequence
                .audio_program
                .track_channels
                .get_mut(&track)
                .unwrap()
                .strip
                .pre_fader
                .processors
                .push(instance);
            let time = |sample| TimelineTime::new(sample, i64::from(rate)).unwrap();
            sequence.audio_program.note_lanes.push(AudioNoteLane {
                schema_version: 1,
                id: AudioNoteLaneId::new(),
                target: AudioNoteTarget {
                    track_id: track,
                    processor_id,
                    input_port_key: "vst3.note-input.0".into(),
                },
                notes: AuthoringList::from(vec![AudioNote {
                    id: AudioNoteId::new(),
                    start: time(ON),
                    end: time(OFF),
                    channel: 0,
                    key: 60,
                    on_velocity: 0.75,
                    off_velocity: 0.25,
                }]),
            });
            let media = Arc::new(AtomicUsize::new(0));
            let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &NoMedia(media.clone()),
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .expect("real zero-input instrument through the sole compiled Runtime");
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = source.reserve_install(&registration, None).unwrap();
            let claim = source.begin_commit(claim).unwrap().commit().unwrap();
            let cancel = ExecutionCancellationToken::new();
            runtime.install_native_owner_bindings(&claim, &cancel).unwrap();
            runtime.validate_native_author_source_closed(&cancel).unwrap();
            let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
            let pid = object.process_id().get();
            assert_ne!(pid, std::process::id());
            // SAFETY: acquire only query/synchronize rights; adopt this returned
            // nonnull owned handle once and retain it through the original Drop.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            assert!(!raw.is_null());
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            assert_eq!(
                runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                object
            );
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
                WAIT_TIMEOUT
            );
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .unwrap();
            let mut start = 0;
            let mut blocks = 0;
            let mut energy = 0_f64;
            let mut peak = 0_f32;
            let mut quiet_frames = 0;
            for &size in partition.iter().cycle() {
                let frames = size.min(TOTAL - start);
                let mut pcm = vec![f32::NAN; frames * 2];
                runtime
                    .render_into(
                        AudioRenderRequest { start_sample: start as i64, frames },
                        &mut pcm,
                    )
                    .unwrap();
                runtime.validate_native_author_source_closed(&cancel).unwrap();
                assert!(pcm.iter().all(|value| value.is_finite()));
                for (offset, frame) in pcm.chunks_exact(2).enumerate() {
                    if start + offset < ON as usize {
                        assert!(
                            frame.iter().all(|value| *value == 0.0),
                            "silence before the authored On"
                        );
                        quiet_frames += 1;
                    }
                    for &value in frame {
                        energy += f64::from(value) * f64::from(value);
                        peak = peak.max(value.abs());
                    }
                }
                assert_eq!(
                    runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                    object
                );
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    claim.generation()
                );
                start += frames;
                blocks += 1;
                if start == TOTAL {
                    break;
                }
            }
            assert_eq!(quiet_frames, ON as usize);
            assert!(energy.is_finite() && energy > 0.0 && peak > 0.0);
            assert_eq!(media.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
            let mut cursors = Cursors::default();
            runtime.visit_continuity_evidence(&mut cursors).unwrap();
            assert_eq!(cursors.processors.len(), 1);
            assert!(matches!(cursors.processors[0].state,
                AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                if epoch == AudioContinuityEpoch::new(1) && next_input_signal_sample.sample() >= TOTAL as i64));
            drop(runtime);
            // SAFETY: the same acquired handle remains owned and live. This
            // observes Drop, without a new timeout, ACK claim or reopen-by-PID.
            let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
            let mut exit_code = 0;
            let queried =
                unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
            assert_eq!(settled, WAIT_OBJECT_0);
            assert_ne!(queried, 0);
            assert_eq!(exit_code, 0, "observed child exit code must be zero");
            println!(
                "{}",
                json!({"case":"mda-dx10-runtime", "stage":"completed", "version":descriptor.version,
                "class":CLASS,"rate":rate,"partition":partition,"block":BLOCK,"frames":start,"blocks":blocks,
                "state_bytes":state.len(),"main_input":"Absent","event_channels":1,"note_on":ON,"note_off":OFF,
                "pre_on_silent_frames":quiet_frames,"energy":energy,"peak":peak,"media_resolves":0,
                "native_pid":pid,"post_drop_wait_status":settled,"exit_code":exit_code,
                "explicit_unbounded_runtime_grant":true,"algorithm_or_terminal_off_qualified":false,
                "graceful_ack_qualified":false,"realtime_qualified":false})
            );
        }
    }
}
