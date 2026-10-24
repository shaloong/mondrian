//! Canonical authored Score through public Runtime, installed owner and Product IPC.
//! Explicit owned PCM-input fixture; this does not qualify zero-input instruments.
#![cfg(windows)]

use super::*;
use crate::{
    AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime,
    AudioRuntimeResourceGrant, AudioStateEntry, NativeAudioAuthorProgram,
    NativeAudioParameterHandoffContinuity, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource, AudioSourceComponentId,
    AuthoringList, ExactAutomationCurve, ExecutionCancellationToken, NativeAudioAuthorCallback,
    NativeAudioParameterCallbackPhase, TimelineTime,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::{Clip, Sequence};
use std::sync::atomic::{AtomicUsize, Ordering};

const FIXTURE_PATH: &str = "E:/codex-artifact-storage/mondrian-3349-plugin-host-sprint/clap-note-gate-build-target-r2/debug/mondrian_clap_note_gate_fixture.dll";
const FIXTURE_SHA: &str = "B239C594F9547EE3A9F3FA107622A6FE663AAF73425E11248BDB1E0A0F32AF1D";

fn fixture() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_INTERNAL_NOTE_GATE_DLL")
            .expect("explicit original B239 owned DLL env"),
    );
    assert!(path.is_absolute());
    assert_eq!(
        std::fs::canonicalize(&path).unwrap(),
        std::fs::canonicalize(FIXTURE_PATH).unwrap()
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 214_528);
    let fingerprint = super::super::clap_discovery::fingerprint_clap_binary(&path).unwrap();
    let hash: String = fingerprint.iter().map(|byte| format!("{byte:02X}")).collect();
    assert_eq!(hash, FIXTURE_SHA);
    path
}

fn time(sample: i64, rate: u32) -> TimelineTime {
    TimelineTime::new(sample, i64::from(rate)).unwrap()
}

struct Ones(Arc<AtomicUsize>);
impl AudioDecodedSource for Ones {
    fn read_interleaved(
        &self,
        _: i64,
        frames: usize,
        out: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        assert!(!cancellation.is_canceled());
        assert_eq!(out.len(), frames);
        self.0.fetch_add(1, Ordering::SeqCst);
        out.fill(1.0);
        Ok(())
    }
}
struct Media(Arc<AtomicUsize>);
impl AudioMediaResolver for Media {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Mono,
            Arc::new(Ones(self.0.clone())),
        ))
    }
}

fn authored(
    catalog: &DiscoveredClapAudioProcessorSpecResolver,
    render: AudioRenderContract,
) -> Sequence {
    let mut state = vec![0_u8; 16];
    state[..4].copy_from_slice(b"MNG1");
    state[4..12].copy_from_slice(&0.5_f64.to_le_bytes());
    let instance = catalog
        .create_instance("org.mondrian.fixture.note-gate", render, Some(state))
        .expect("actual ordinary CLAP author snapshot, not a Note execution grant");
    let processor_id = instance.id;
    let mut sequence = Sequence::new("canonical public owned Note DSP");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track_id = sequence.audio_tracks[0].id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(instance);
    sequence
        .add_media_audio_clip(
            track_id,
            Clip::new(
                AssetId::new(),
                TimelineTime::ZERO,
                time(14, render.sample_rate),
            )
            .unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    for (port, end, on_velocity, off_velocity) in
        [(0_u32, 5, 0.0, 0.25), (0x8000_0017, 9, 1.0, 1.0)]
    {
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id,
                processor_id,
                input_port_key: format!("clap.note-input.{port}"),
            },
            notes: AuthoringList::from(vec![AudioNote {
                id: AudioNoteId::new(),
                start: time(2, render.sample_rate),
                end: time(end, render.sample_rate),
                channel: 0,
                key: 60,
                on_velocity,
                off_velocity,
            }]),
        });
    }
    sequence
}

fn accept_actual_gain_outputs(
    runtime: &mut AudioProgramRuntime,
    sequence: &mut Sequence,
    source: &AudioOwnerInstallationSource,
) -> usize {
    let cancellation = ExecutionCancellationToken::new();
    let batch = runtime
        .poll_native_observations(&cancellation)
        .expect("poll the sole actual native source without clearing it");
    let Some(proposal) = batch.and_then(|batch| batch.author) else {
        return 0;
    };
    let count = proposal.callbacks().len();
    assert!(count > 0);
    // B239 emits two real Gain Values around each NoteEnd. These already equal
    // the canonical constant; verify every callback rather than discarding one.
    for callback in proposal.callbacks() {
        match callback {
            NativeAudioAuthorCallback::ParameterAtExecution {
                parameter,
                phase,
                value_bits,
                ..
            }
            | NativeAudioAuthorCallback::Parameter { parameter, phase, value_bits } => {
                assert_eq!(*parameter, ParameterId::new_static("clap.param.17"));
                assert_eq!(*phase, NativeAudioParameterCallbackPhase::Value);
                assert_eq!(*value_bits, Some(0.5_f64.to_bits()));
            }
            other => panic!("unexpected native author output: {other:?}"),
        }
    }
    let track = sequence.audio_tracks[0].id;
    let processor = &mut sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors[0];
    if let Some(state) = proposal.state() {
        processor.opaque_state = Some(state.clone());
    }
    processor
        .set_parameter_automation(
            ExactAutomationCurve::new(ParameterId::new_static("clap.param.17"), 0.5).unwrap(),
        )
        .expect("schema-validated same constant in canonical author state");
    let programs = [NativeAudioAuthorProgram {
        sequence: sequence.id,
        program: Arc::new(
            crate::compile_audio_program(
                sequence,
                crate::AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
            )
            .unwrap(),
        ),
    }];
    let (registration, destination) = source.register().unwrap();
    let current = source.current_claim().unwrap().expect("actual installed root");
    let reservation = source.reserve_install(&registration, Some(&current)).unwrap();
    let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
    let mut prepared = runtime
        .prepare_native_author_parameter_handoff(&programs, &proposal, destination, &cancellation)
        .expect("same immutable Note score and original C+H+N fee admission");
    let receipt = runtime
        .install_native_author_parameter_handoff(&mut prepared, &mut authorization, &cancellation)
        .expect("actual Source CAS and continuing ledger rebind, no native reload");
    assert_eq!(
        receipt.continuity,
        NativeAudioParameterHandoffContinuity::Preserved
    );
    drop(prepared);
    runtime
        .acknowledge_native_author_parameter_handoff(&proposal, &cancellation)
        .expect("Ack only after the actual canonical Plan/Source handoff");
    count
}

#[test]
#[ignore = "requires pinned B239 DLL and fresh normal Product helper; canonical public Note gate"]
fn installed_clap_canonical_public_notes_produce_literal_pcm_across_partitions() {
    let helper = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .expect("fresh normal Product bound by external owner/producer gate"),
    );
    assert!(helper.is_absolute());
    assert!(std::fs::metadata(&helper).unwrap().is_file());
    let dll = fixture();
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper, [dll])
            .expect("real Product descriptor discovery"),
    );
    let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
    // Independent literal dyadics from native Gate: port0 weight1/32,
    // port1 weight1/8, gain1/2, input1, four-sample linear release.
    // Native IDs are intentionally absent from the oracle and input: canonical
    // author IDs are lowered/paired by the production ledger itself.
    let expected = [
        0.0_f32,
        0.0,
        5.0 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        19.0 / 256.0,
        9.0 / 128.0,
        17.0 / 256.0,
        1.0 / 16.0,
        3.0 / 64.0,
        1.0 / 32.0,
        1.0 / 64.0,
        0.0,
    ];
    for rate in [44_100, 48_000] {
        let render = AudioRenderContract {
            sample_rate: rate,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 14,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let original_sequence = authored(&catalog, render);
        for partition in [&[14_usize][..], &[3, 2, 4, 5], &[1; 14]] {
            let mut sequence = original_sequence.clone();
            let reads = Arc::new(AtomicUsize::new(0));
            // Full existing grant is explicit: preclone G alone is not a promise
            // to cover native factory/IPC/Hosted storage or later author fees.
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &Media(reads.clone()),
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .expect("canonical public Note preparation must reach real admitted CLAP DSP");
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = source
                .reserve_install(&registration, None)
                .and_then(|reservation| source.begin_commit(reservation))
                .unwrap()
                .commit()
                .unwrap();
            runtime
                .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
                .expect("same actual Hosted native objects acknowledge installed root");
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .expect("installed confirmed cold origin zero");
            let mut rendered = Vec::new();
            let mut start = 0_usize;
            let mut parameter_outputs = 0_usize;
            for &frames in partition {
                let mut pcm = vec![99.0; frames];
                runtime
                    .render_into(
                        AudioRenderRequest { transport_activity: None, start_sample: start as i64, frames },
                        &mut pcm,
                    )
                    .expect("public Runtime DAG through normal Product DSP IPC");
                rendered.extend(pcm);
                start += frames;
                parameter_outputs +=
                    accept_actual_gain_outputs(&mut runtime, &mut sequence, &source);
            }
            assert_eq!(start, expected.len());
            assert_eq!(rendered, expected, "rate={rate}, partition={partition:?}");
            assert_eq!(
                parameter_outputs, 4,
                "two genuine Gain outputs around each actual End"
            );
            assert!(reads.load(Ordering::SeqCst) > 0);
            drop(runtime);
            fixture(); // Recheck the original actual DLL after native retirement.
        }
    }
}

// No input media is authored for the fixed zero-input native variant. A hidden
// ones-source, dummy Clip or accidental resolver fallback must fail this test.
struct ZeroInputNoMedia(Arc<AtomicUsize>);
impl AudioMediaResolver for ZeroInputNoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("zero-input public Note case must not resolve any media".to_owned())
    }
}

fn zero_input_fixture() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_ZERO_INPUT_NOTE_GATE_DLL")
            .expect("separately retained zero-input fixture DLL"),
    );
    assert!(path.is_absolute());
    let expected = std::env::var("MONDRIAN_ZERO_INPUT_NOTE_GATE_DLL_SHA256")
        .expect("explicit separately produced fixture SHA256");
    assert!(expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let actual = super::super::clap_discovery::fingerprint_clap_binary(&path)
        .expect("current real zero-input fixture fingerprint");
    let actual: String = actual.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(actual, expected.to_ascii_lowercase());
    path
}

#[derive(Default)]
struct ZeroInputCursors {
    sessions: Vec<crate::AudioSessionCursorEvidence>,
    processors: Vec<crate::AudioProcessorCursorEvidence>,
}
impl crate::AudioContinuityEvidenceVisitor for ZeroInputCursors {
    fn session(
        &mut self,
        evidence: crate::AudioSessionCursorEvidence,
    ) -> Result<(), crate::AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: crate::AudioProcessorCursorEvidence,
    ) -> Result<(), crate::AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
fn zero_input_owner(runtime: &AudioProgramRuntime) -> crate::AudioNativeProcessorOwnerLease {
    let mut cursors = ZeroInputCursors::default();
    runtime.visit_continuity_evidence(&mut cursors).unwrap();
    assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
    let processor = cursors.processors[0];
    let epoch = match processor.state {
        crate::AudioProcessorCursorState::Unentered => None,
        crate::AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        _ => panic!("zero-input Note occurrence has no current native owner"),
    };
    runtime
        .native_owner_lease(crate::AudioNativeProcessorAddress {
            root: cursors.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch,
        })
        .expect("lease issued by this actual installed Runtime")
}

#[test]
#[ignore = "requires separately pinned zero-input NoteGate DLL and fresh normal Product helper"]
fn installed_clap_zero_input_public_notes_produce_literal_pcm_without_media() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    };

    let helper = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .expect("fresh normal Product bound by external producer/owner gate"),
    );
    assert!(helper.is_absolute() && std::fs::metadata(&helper).unwrap().is_file());
    let dll = zero_input_fixture();
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper, [dll.clone()])
            .expect("actual Product discovers the fixed zero-input native definition"),
    );
    let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
    // Same independent literal note weights/release as the original public
    // gate, now with the fixture's own carrier1 and NO media Clip/source.
    // Do not call Gate or calculate expected PCM from the native implementation.
    let expected = [
        0.0_f32,
        0.0,
        5.0 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        19.0 / 256.0,
        9.0 / 128.0,
        17.0 / 256.0,
        1.0 / 16.0,
        3.0 / 64.0,
        1.0 / 32.0,
        1.0 / 64.0,
        0.0,
    ];
    for rate in [44_100, 48_000] {
        let render = AudioRenderContract {
            sample_rate: rate,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 14,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let mut state = vec![0_u8; 16];
        state[..4].copy_from_slice(b"MNG1");
        state[4..12].copy_from_slice(&0.5_f64.to_le_bytes());
        let instance = catalog
            .create_instance(
                "org.mondrian.fixture.note-gate.zero-input",
                render,
                Some(state.clone()),
            )
            .expect("real zero-input native MNG1 restore and ordinary author snapshot");
        assert!(!instance.bypassed);
        assert_eq!(
            instance.opaque_state.as_deref().map(Vec::as_slice),
            Some(state.as_slice())
        );
        let processor_id = instance.id;
        let mut authored = Sequence::new("canonical zero-audio-input owned public Note DSP");
        authored.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let track_id = authored.audio_tracks[0].id;
        authored
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(instance);
        for (port, end, on_velocity, off_velocity) in
            [(0_u32, 5, 0.0, 0.25), (0x8000_0017, 9, 1.0, 1.0)]
        {
            authored.audio_program.note_lanes.push(AudioNoteLane {
                schema_version: 1,
                id: AudioNoteLaneId::new(),
                target: AudioNoteTarget {
                    track_id,
                    processor_id,
                    input_port_key: format!("clap.note-input.{port}"),
                },
                notes: AuthoringList::from(vec![AudioNote {
                    id: AudioNoteId::new(),
                    start: time(2, rate),
                    end: time(end, rate),
                    channel: 0,
                    key: 60,
                    on_velocity,
                    off_velocity,
                }]),
            });
        }
        assert!(authored.audio_tracks.iter().all(|track| track.clips.is_empty()));
        let serialized = serde_json::to_vec(&authored).expect("canonical Sequence serialization");
        assert!(serialized.len() <= 512 * 1024);
        let reopened: Sequence =
            serde_json::from_slice(&serialized).expect("canonical Sequence reopen");
        assert_eq!(
            serde_json::to_value(&reopened).unwrap(),
            serde_json::to_value(&authored).unwrap(),
            "canonical Sequence meaning survives serialization and reopen"
        );
        for partition in [&[14_usize][..], &[3, 2, 4, 5], &[1; 14]] {
            let mut sequence = reopened.clone();
            let resolves = Arc::new(AtomicUsize::new(0));
            // Preserve the original explicit public Note grant. This is NOT a
            // newly qualified finite Root ceiling or complete heap/RT claim.
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &ZeroInputNoMedia(resolves.clone()),
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .expect(
                    "NoteScore alone retains the native Track rack without a dummy media source",
                );
            assert_eq!(resolves.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = source
                .reserve_install(&registration, None)
                .and_then(|reservation| source.begin_commit(reservation))
                .unwrap()
                .commit()
                .unwrap();
            runtime
                .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
                .expect("actual zero-input child acknowledges the sole installed root");
            let cold_lease = zero_input_owner(&runtime);
            assert!(cold_lease.editor_target().continuity_epoch.is_none());
            assert_eq!(cold_lease.editor_target().instance, processor_id);
            let cold = runtime
                .observe_native_owner_object(cold_lease)
                .expect("actual cold object, independent of incremental parameter presentation");
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .expect("actual cold author Source is closed, not a fake Ack");
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .expect("installed zero-input native Session enters real origin zero");
            let entered_lease = zero_input_owner(&runtime);
            let entered = runtime.observe_native_owner_object(entered_lease).unwrap();
            assert_eq!(entered, cold);
            assert_eq!(entered_lease.editor_target().continuity_epoch, Some(1));
            let pid = entered.process_id().get();
            assert_ne!(pid, std::process::id());
            // SAFETY: only this current installed Runtime's authenticated native
            // object supplies PID. HANDLE has query/synchronize rights only and
            // stays live through real PCM, Source handoffs and Runtime Drop.
            let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
            assert!(!raw.is_null());
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
                258
            );
            let mut rendered = Vec::new();
            let mut start = 0;
            let mut parameter_outputs = 0;
            for &frames in partition {
                let mut pcm = vec![99.0; frames];
                runtime.render_into(AudioRenderRequest { transport_activity: None,start_sample:start as i64,frames},&mut pcm)
                    .expect("real NoteOn/Off/End through normal Product DSP IPC with zero native audio inputs");
                assert!(pcm.iter().all(|value| value.is_finite()));
                rendered.extend(pcm);
                start += frames;
                // Reuse the accepted canonical Plan/Source publication and real
                // Ack, validating each actual Gain callback around native End.
                parameter_outputs +=
                    accept_actual_gain_outputs(&mut runtime, &mut sequence, &source);
                let current = zero_input_owner(&runtime);
                assert_eq!(
                    runtime.observe_native_owner_object(current).unwrap(),
                    entered
                );
                assert_eq!(current.editor_target().continuity_epoch, Some(1));
                assert_eq!(resolves.load(Ordering::SeqCst), 0);
            }
            assert_eq!(start, expected.len());
            assert_eq!(rendered, expected, "rate={rate}, partition={partition:?}");
            assert_eq!(
                parameter_outputs, 4,
                "the same two actual Gain callbacks around each native End"
            );
            assert!(sequence.audio_tracks.iter().all(|track| track.clips.is_empty()));
            assert_eq!(resolves.load(Ordering::SeqCst), 0);
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .expect(
                    "all actual native author Source was canonically accepted and acknowledged",
                );
            let current = zero_input_owner(&runtime);
            assert_eq!(
                runtime.observe_native_owner_object(current).unwrap(),
                entered
            );
            let footprint = runtime.resource_footprint();
            assert_eq!(footprint.media_window_bytes, 0);
            let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
            drop(runtime);
            let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
            let mut exit_code = 0;
            let queried =
                unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
            let evidence=serde_json::to_string(&serde_json::json!({
                "case":"owned-fixed-zero-input-public-notes", "sample_rate":rate,
                "partitions":partition,"completed_frames":start,"media_resolve_calls":resolves.load(Ordering::SeqCst),
                "media_window_bytes":footprint.media_window_bytes,"parameter_callbacks_accepted":parameter_outputs,
                "native_pid":pid,"native_object_serial":entered.object_serial().get(),
                "same_object_through_pcm_and_source_handoffs":true,
                "alive_before_drop_wait_status":alive,"post_drop_wait_status":settled,
                "exit_query_succeeded":queried!=0,"exit_code":exit_code,
                "root_resource_footprint":format!("{footprint:?}"),
                "root_grant_is_original_explicit_unbounded":true,
                "fixture_enforces_native_audio_inputs_count_zero":true,
                "native_process_callback_count_instrumented":false,
                "third_party_instrument_qualified":false,"realtime_or_performance_qualified":false
            })).unwrap();
            assert!(evidence.len() <= 16 * 1024);
            println!("{evidence}");
            assert_eq!(alive, 258);
            assert_eq!(
                settled, 0,
                "existing Runtime Drop settles the exact native child"
            );
            assert_ne!(queried, 0);
            assert_eq!(exit_code, 0);
            assert_eq!(
                std::fs::canonicalize(zero_input_fixture()).unwrap(),
                std::fs::canonicalize(&dll).unwrap()
            );
        }
    }
}

#[test]
#[ignore = "requires existing pinned zero-input NoteGate DLL and fresh normal Product helper"]
fn installed_clap_nonzero_public_notes_chase_and_reset_same_object() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    };

    let helper = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("fresh normal Product helper"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    let dll = zero_input_fixture();
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper, [dll.clone()]).unwrap(),
    );
    let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 11,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        ..test_render_contract()
    };
    let mut state = vec![0_u8; 16];
    state[..4].copy_from_slice(b"MNG1");
    state[4..12].copy_from_slice(&0.5_f64.to_le_bytes());
    let mut instance = catalog
        .create_instance(
            "org.mondrian.fixture.note-gate.zero-input",
            render,
            Some(state),
        )
        .unwrap();
    // Native End emits the same constant Gain value. Start with that canonical
    // curve so accepting real callbacks does not change the authored meaning.
    instance
        .set_parameter_automation(
            ExactAutomationCurve::new(ParameterId::new_static("clap.param.17"), 0.5).unwrap(),
        )
        .unwrap();
    let id = instance.id;
    let mut sequence = Sequence::new("CLAP public cutoff and retained reset");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    sequence.settings.audio_sample_rate = render.sample_rate;
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
    // Public cutoff S=5: held, On==S, Off==S, and completely past. The last
    // two must not create release tails or unmatched Offs in this fresh epoch.
    for (port, key, on, off, velocity) in [
        (0_u32, 60, 2, 9, 0.0),
        (0x8000_0017, 61, 5, 11, 1.0),
        (0, 62, 1, 5, 1.0),
        (0x8000_0017, 63, 3, 4, 0.0),
    ] {
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id: id,
                input_port_key: format!("clap.note-input.{port}"),
            },
            notes: AuthoringList::from(vec![AudioNote {
                id: AudioNoteId::new(),
                start: time(on, render.sample_rate),
                end: time(off, render.sample_rate),
                channel: 0,
                key,
                on_velocity: velocity,
                off_velocity: 0.25,
            }]),
        });
    }
    let authored = serde_json::to_value(&sequence).unwrap();
    let resolves = Arc::new(AtomicUsize::new(0));
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &ZeroInputNoMedia(resolves.clone()),
        &processors,
        render,
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .unwrap();
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source.reserve_install(&registration, None).unwrap();
    let claim = source.begin_commit(claim).unwrap().commit().unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .unwrap();
    let cold = zero_input_owner(&runtime);
    let object = runtime.observe_native_owner_object(cold).unwrap();
    assert_eq!(cold.editor_target().instance, id);
    assert!(cold.editor_target().continuity_epoch.is_none());
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: the authenticated installed object supplies this PID. This handle
    // observes that exact process and never becomes another native lifetime owner.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let cursors = |runtime: &AudioProgramRuntime| {
        let mut result = ZeroInputCursors::default();
        runtime.visit_continuity_evidence(&mut result).unwrap();
        assert_eq!((result.sessions.len(), result.processors.len()), (1, 1));
        assert_eq!(result.sessions[0].public_output_lookahead_frames, 0);
        result
    };
    // Independent Main literals at samples5..16: native held weights1/32 and
    // 1/8, Gain1/2, Off9/11, four-sample releases. No Gate/host expected helper.
    let expected = [
        5.0_f32 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        5.0 / 64.0,
        19.0 / 256.0,
        9.0 / 128.0,
        13.0 / 256.0,
        1.0 / 32.0,
        1.0 / 64.0,
        0.0,
    ];
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Complete first nonzero entry, then leave both voices active before
        // another reset. The final window detects uncancelled old voices/Ends.
        for (epoch, partition) in [
            (1_u64, &[3_usize, 4, 4][..]),
            (2, &[2][..]),
            (3, &[1, 3, 2, 5][..]),
        ] {
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
            let before = cursors(&runtime);
            let root = runtime.resource_root_identity();
            let old = zero_input_owner(&runtime);
            let claim = source.current_claim().unwrap().unwrap();
            let canceled = ExecutionCancellationToken::new();
            canceled.cancel();
            assert_eq!(
                runtime.enter_state_cancellable(
                    AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(epoch),
                        start_sample: 5,
                    },
                    &canceled
                ),
                Err(crate::AudioNativeOwnerError::Cancelled.into())
            );
            let unchanged = cursors(&runtime);
            assert_eq!(unchanged.sessions, before.sessions);
            assert_eq!(unchanged.processors, before.processors);
            assert_eq!(zero_input_owner(&runtime), old);
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: 5,
                })
                .expect("public nonzero Note entry must chase at S and reset this CLAP object");
            let current = zero_input_owner(&runtime);
            assert_eq!(
                runtime.observe_native_owner_object(current).unwrap(),
                object
            );
            assert_eq!(current.address().root, old.address().root);
            assert_eq!(current.address().local, old.address().local);
            assert_eq!(current.address().occurrence, old.address().occurrence);
            assert_eq!(current.editor_target().continuity_epoch, Some(epoch));
            assert!(runtime.validate_native_owner(old).is_err());
            assert!(runtime.observe_native_owner_object(old).is_err());
            assert!(runtime.native_editor_target(old.address()).is_err());
            runtime.validate_native_owner(current).unwrap();
            assert_eq!(runtime.resource_root_identity(), root);
            assert_eq!(
                source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            runtime.native_owner_installation_verifier().unwrap().validate(&claim).unwrap();
            let entered = cursors(&runtime);
            assert_eq!(entered.sessions[0].identity, before.sessions[0].identity);
            assert_eq!(
                entered.processors[0].identity,
                before.processors[0].identity
            );
            assert!(matches!(entered.sessions[0].state,
                crate::AudioSessionCursorState::Active { epoch: actual, next_public_sample,
                    next_execution_sample, alignment_primed: true }
                if actual == AudioContinuityEpoch::new(epoch)
                    && next_public_sample.sample() == 5 && next_execution_sample.sample() == 5));
            assert_eq!(
                runtime.enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: 5,
                }),
                Err(crate::AudioExecutionError::ReusedContinuityEpoch(
                    AudioContinuityEpoch::new(epoch)
                ))
            );
            let refused = cursors(&runtime);
            assert_eq!(refused.sessions, entered.sessions);
            assert_eq!(refused.processors, entered.processors);
            let mut rendered = Vec::new();
            let mut callbacks = 0;
            for &frames in partition {
                let mut pcm = vec![99.0; frames];
                runtime
                    .render_into(
                        AudioRenderRequest { transport_activity: None, start_sample: 5 + rendered.len() as i64, frames },
                        &mut pcm,
                    )
                    .unwrap();
                rendered.extend(pcm);
                callbacks += accept_actual_gain_outputs(&mut runtime, &mut sequence, &source);
                let current = zero_input_owner(&runtime);
                assert_eq!(
                    runtime.observe_native_owner_object(current).unwrap(),
                    object
                );
                assert_eq!(current.editor_target().continuity_epoch, Some(epoch));
                let after = cursors(&runtime);
                let end = 5 + rendered.len() as i64;
                assert!(matches!(after.sessions[0].state,
                    crate::AudioSessionCursorState::Active { epoch: actual, next_public_sample,
                        next_execution_sample, alignment_primed: true }
                    if actual == AudioContinuityEpoch::new(epoch)
                        && next_public_sample.sample() == end && next_execution_sample.sample() == end));
                assert!(matches!(after.processors[0].state,
                    crate::AudioProcessorCursorState::Active { epoch: actual, next_input_signal_sample }
                    if actual == AudioContinuityEpoch::new(epoch) && next_input_signal_sample.sample() == end));
            }
            assert_eq!(
                rendered.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                expected[..rendered.len()].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "epoch={epoch}, partition={partition:?}"
            );
            assert_eq!(
                callbacks,
                if epoch == 2 { 0 } else { 4 },
                "two real Gain callbacks per actual native End; reset cancels old pending Ends"
            );
            assert_eq!(serde_json::to_value(&sequence).unwrap(), authored);
            assert_eq!(resolves.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
        }
    }));
    // Consume the original Runtime before resuming any target panic, including
    // baseline unsupported. The same HANDLE must already be settled after Drop.
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        drop(runtime);
        assert_eq!(alive, 258);
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
            0
        );
        let mut exit = u32::MAX;
        assert_ne!(
            unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
            0
        );
        assert_eq!(exit, 0);
        drop(source);
    }));
    if let Err(failure) = target {
        std::panic::resume_unwind(failure);
    }
    if let Err(failure) = cleanup {
        std::panic::resume_unwind(failure);
    }
    assert_eq!(
        std::fs::canonicalize(zero_input_fixture()).unwrap(),
        std::fs::canonicalize(dll).unwrap()
    );
}
