//! Public Note refusal/continuation through canonical Runtime and actual owned B239 DSP.
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

fn accept_gain_proposal(
    runtime: &mut AudioProgramRuntime,
    sequence: &mut Sequence,
    source: &AudioOwnerInstallationSource,
    proposal: mondrian_core::NativeAudioAuthorProposal,
) -> usize {
    let cancellation = ExecutionCancellationToken::new();
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

#[derive(Debug, Default, PartialEq, Eq)]
struct Cursors {
    sessions: Vec<crate::AudioSessionCursorEvidence>,
    processors: Vec<crate::AudioProcessorCursorEvidence>,
}
impl crate::AudioContinuityEvidenceVisitor for Cursors {
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
fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).unwrap();
    cursors
}
fn lease(runtime: &AudioProgramRuntime) -> crate::AudioNativeProcessorOwnerLease {
    let observed = cursors(runtime);
    assert_eq!((observed.sessions.len(), observed.processors.len()), (1, 1));
    let processor = observed.processors[0];
    let crate::AudioProcessorCursorState::Active { epoch, .. } = processor.state else {
        panic!("actual Note processor must be entered");
    };
    runtime
        .native_owner_lease(crate::AudioNativeProcessorAddress {
            root: observed.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch: Some(epoch),
        })
        .unwrap()
}
fn frozen(
    runtime: &AudioProgramRuntime,
) -> (Cursors, String, crate::AudioRuntimeResourceFootprint) {
    (
        cursors(runtime),
        format!("{:?}", runtime.latest_meter_frame()),
        runtime.resource_footprint(),
    )
}

#[test]
#[ignore = "requires pinned B239 DLL and fresh normal Product; new public Note refusal contracts"]
fn installed_clap_public_note_refusals_preserve_owner_clock_pcm_and_original_off() {
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").unwrap());
    assert!(helper.is_absolute());
    assert!(std::fs::metadata(&helper).unwrap().is_file());
    let catalog =
        Arc::new(DiscoveredClapAudioProcessorSpecResolver::discover(helper, [fixture()]).unwrap());
    let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 14,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let mut sequence = authored(&catalog, render);
    let reads = Arc::new(AtomicUsize::new(0));
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &Media(reads.clone()),
        &processors,
        render,
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .expect("same canonical public preparation as the locked positive consumer");
    let cancellation = ExecutionCancellationToken::new();
    let before = frozen(&runtime);
    assert!(matches!(
        runtime.enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        }),
        Err(crate::AudioExecutionError::ProcessorHost(
            crate::AudioProcessorHostError::NoteExecutionRefused(
                "Note score execution requires the installed root owner"
            )
        ))
    ));
    assert_eq!(frozen(&runtime), before);
    let mut pcm = [91.0];
    assert!(matches!(
        runtime.render_into(AudioRenderRequest { start_sample: 0, frames: 1 }, &mut pcm),
        Err(crate::AudioExecutionError::StateEntryRequired)
    ));
    assert_eq!(pcm, [91.0]);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(frozen(&runtime), before);

    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime.install_native_owner_bindings(&claim, &cancellation).unwrap();
    let before = frozen(&runtime);
    // Nonzero entry is valid; an already canceled request must still refuse
    // before entering the fresh epoch or touching the installed native object.
    let canceled = ExecutionCancellationToken::new();
    canceled.cancel();
    assert_eq!(
        runtime.enter_state_cancellable(
            AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: 1,
            },
            &canceled,
        ),
        Err(crate::AudioNativeOwnerError::Cancelled.into())
    );
    assert_eq!(frozen(&runtime), before);
    assert_eq!(
        source.current_claim().unwrap().unwrap().generation(),
        claim.generation()
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();

    let entered_lease = lease(&runtime);
    let before = frozen(&runtime);
    // A valid zero-frame output slice has no writable samples. Keep adjacent
    // storage as a sentinel and check actual cursors/meter/reads/owner separately.
    let mut adjacent = [91.0];
    assert!(matches!(
        runtime.render_into(
            AudioRenderRequest { start_sample: 0, frames: 0 },
            &mut adjacent[..0]
        ),
        Err(crate::AudioExecutionError::ProcessorHost(
            crate::AudioProcessorHostError::NoteExecutionRefused(
                "Note score zero-frame processing is unsupported"
            )
        ))
    ));
    assert_eq!(adjacent, [91.0]);
    assert_eq!(frozen(&runtime), before);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(lease(&runtime), entered_lease);
    runtime.validate_native_owner(entered_lease).unwrap();

    let mut first = [91.0; 3];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 0, frames: 3 },
            &mut first,
        )
        .unwrap();
    assert_eq!(first, [0.0, 0.0, 5.0 / 64.0]);
    let before = frozen(&runtime);
    let before_reads = reads.load(Ordering::SeqCst);
    let old_lease = lease(&runtime);
    // A new epoch may now seek, but reusing the active epoch remains invalid
    // and must preserve the original held notes and their pending Offs.
    assert_eq!(
        runtime.enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 3,
        }),
        Err(crate::AudioExecutionError::ReusedContinuityEpoch(
            AudioContinuityEpoch::new(1)
        ))
    );
    assert_eq!(frozen(&runtime), before);
    pcm = [91.0];
    assert!(matches!(
        runtime.render_into(AudioRenderRequest { start_sample: 4, frames: 1 }, &mut pcm),
        Err(crate::AudioExecutionError::NonContiguousBlock {
            expected_start_sample: 3,
            actual_start_sample: 4,
            ..
        })
    ));
    assert_eq!(pcm, [91.0]);
    assert_eq!(frozen(&runtime), before);
    assert_eq!(reads.load(Ordering::SeqCst), before_reads);
    assert_eq!(
        source.current_claim().unwrap().unwrap().generation(),
        claim.generation()
    );
    assert_eq!(lease(&runtime), old_lease);
    runtime.validate_native_owner(old_lease).unwrap();

    let mut middle = [91.0; 6];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 3, frames: 6 },
            &mut middle,
        )
        .expect("refusals did not replay On or advance native signal history");
    assert_eq!(
        middle,
        [
            5.0 / 64.0,
            5.0 / 64.0,
            5.0 / 64.0,
            19.0 / 256.0,
            9.0 / 128.0,
            17.0 / 256.0
        ]
    );
    let before = frozen(&runtime);
    let before_reads = reads.load(Ordering::SeqCst);
    let before_lease = lease(&runtime);
    // The first real End has pending Gain callbacks. Zero-frame refusal must
    // win before native control can instead return NativeAuthorPending. The
    // following genuine poll must still deliver both original callbacks.
    assert!(matches!(
        runtime.render_into(
            AudioRenderRequest { start_sample: 9, frames: 0 },
            &mut adjacent[..0]
        ),
        Err(crate::AudioExecutionError::ProcessorHost(
            crate::AudioProcessorHostError::NoteExecutionRefused(
                "Note score zero-frame processing is unsupported"
            )
        ))
    ));
    assert_eq!(adjacent, [91.0]);
    assert_eq!(frozen(&runtime), before);
    assert_eq!(reads.load(Ordering::SeqCst), before_reads);
    assert_eq!(lease(&runtime), before_lease);
    let proposal = runtime
        .poll_native_observations(&cancellation)
        .unwrap()
        .unwrap()
        .author
        .expect("actual two Gain callbacks around the first native End");
    assert_eq!(proposal.callbacks().len(), 2);
    let before = frozen(&runtime);
    let before_reads = reads.load(Ordering::SeqCst);
    let old_lease = lease(&runtime);
    let old_claim = source.current_claim().unwrap().unwrap();
    let mut changed = sequence.clone();
    changed.audio_program.note_lanes[1].notes[0].end = time(10, render.sample_rate);
    let changed_programs = [NativeAudioAuthorProgram {
        sequence: changed.id,
        program: Arc::new(
            crate::compile_audio_program(
                &changed,
                crate::AudioCompileRequest::program(changed.audio_program.outputs[0].id),
            )
            .unwrap(),
        ),
    }];
    let (registration, destination) = source.register().unwrap();
    let reserved = source.reserve_install(&registration, Some(&old_claim)).unwrap();
    let authorization = source.authorize_runtime_installation(reserved).unwrap();
    let error = runtime
        .prepare_native_author_parameter_handoff(
            &changed_programs,
            &proposal,
            destination,
            &cancellation,
        )
        .err()
        .expect("changed immutable Score must not retire an active native owner");
    assert!(
        matches!(error, crate::NativeAudioParameterHandoffError::Compile(
        crate::AudioCompileError::InvalidPreparedGraph(ref reason)
    ) if reason == "native author handoff changed immutable Note score")
    );
    drop(authorization); // No Source install/CAS occurred for the refused plan.
    assert_eq!(frozen(&runtime), before);
    assert_eq!(reads.load(Ordering::SeqCst), before_reads);
    assert_eq!(
        source.current_claim().unwrap().unwrap().generation(),
        old_claim.generation()
    );
    assert_eq!(lease(&runtime), old_lease);
    runtime.validate_native_owner(old_lease).unwrap();
    let accepted = accept_gain_proposal(&mut runtime, &mut sequence, &source, proposal);
    assert_eq!(accepted, 2);
    let mut last = [91.0; 5];
    runtime
        .render_into(AudioRenderRequest { start_sample: 9, frames: 5 }, &mut last)
        .expect("same-score real handoff/Ack preserves the held second note's original Off");
    assert_eq!(last, [1.0 / 16.0, 3.0 / 64.0, 1.0 / 32.0, 1.0 / 64.0, 0.0]);
    let proposal = runtime
        .poll_native_observations(&cancellation)
        .unwrap()
        .unwrap()
        .author
        .expect("second actual End and its real Gain callbacks");
    assert_eq!(
        accept_gain_proposal(&mut runtime, &mut sequence, &source, proposal),
        2
    );
    let mut silent = [91.0];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 14, frames: 1 },
            &mut silent,
        )
        .unwrap();
    assert_eq!(silent, [0.0]);
    drop(runtime);
    fixture();
}
