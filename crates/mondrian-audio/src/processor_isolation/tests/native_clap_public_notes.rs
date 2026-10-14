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
                        AudioRenderRequest { start_sample: start as i64, frames },
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
