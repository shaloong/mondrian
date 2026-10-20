//! Actual Ring Out startup callbacks must enter App authoring without a GUI.
//! This control stops before signal entry; no PCM or Project persistence oracle.
use super::*;
use mondrian_audio::{
    AudioContinuityEvidenceVisitor, AudioExecutionError, AudioProcessorCursorEvidence,
    AudioProcessorCursorState, AudioSessionCursorEvidence, AudioSessionCursorState,
};
use mondrian_core::{ExecutionCancellationToken, NativeAudioAuthorProposal, ParameterId};
use mondrian_media::{
    AudioPcmNativeAuthorHandoffContinuity as Continuity,
    AudioPcmNativeAuthorHandoffStatus as Status, AudioPcmRenderer,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

const CASE_ENV: &str = "MONDRIAN_RING_OUT_HEADLESS_AUTHOR_CASE";
const CASE_HASH_ENV: &str = "MONDRIAN_RING_OUT_HEADLESS_AUTHOR_CASE_SHA256";
const PLUGIN: &str = "com.duskaudio.ring-out";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    schema: u32,
    helper: Binding,
    plugin: Binding,
    opaque_state: Binding,
}

// The same finite read-only material rules as the existing external controls.
// FILE_SHARE_READ retains the actual input bytes through all selected owners.
struct Lease {
    file: File,
    path: PathBuf,
    binding: Binding,
}
impl Lease {
    fn open(binding: Binding, maximum: u64) -> Self {
        assert!(binding.path.is_absolute());
        assert!(binding.bytes > 0 && binding.bytes <= maximum);
        assert!(
            binding.sha256.len() == 64
                && binding.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        for ancestor in binding.path.ancestors() {
            assert_eq!(
                std::fs::symlink_metadata(ancestor)
                    .expect("existing material ancestor")
                    .file_attributes()
                    & 0x400,
                0,
                "no material reparse ancestor"
            );
        }
        let path = std::fs::canonicalize(&binding.path).expect("canonical material");
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .expect("actual read-only material lease");
        assert!(file.metadata().unwrap().is_file());
        assert_eq!(file.metadata().unwrap().len(), binding.bytes);
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut bytes = 0_u64;
        loop {
            let count = file.read(&mut buffer).expect("hash leased material");
            if count == 0 {
                break;
            }
            bytes = bytes.checked_add(count as u64).unwrap();
            assert!(bytes <= binding.bytes);
            hash.update(&buffer[..count]);
        }
        assert_eq!(bytes, binding.bytes);
        assert_eq!(format!("{:x}", hash.finalize()), binding.sha256);
        assert_eq!(std::fs::canonicalize(&binding.path).unwrap(), path);
        Self { file, path, binding }
    }

    fn read(&mut self) -> Vec<u8> {
        self.file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::with_capacity(self.binding.bytes as usize);
        Read::by_ref(&mut self.file)
            .take(self.binding.bytes + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes.len() as u64, self.binding.bytes);
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), self.binding.sha256);
        bytes
    }
}

#[derive(Default)]
struct Unentered {
    session: Option<AudioSessionCursorEvidence>,
    processor: Option<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Unentered {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        assert_eq!(evidence.state, AudioSessionCursorState::Unentered);
        assert!(
            self.session.replace(evidence).is_none(),
            "one actual root Session"
        );
        Ok(())
    }

    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        assert_eq!(evidence.state, AudioProcessorCursorState::Unentered);
        assert!(
            self.processor.replace(evidence).is_none(),
            "one actual Hosted occurrence"
        );
        Ok(())
    }
}
fn unentered(
    renderer: &TimelineAudioPcmRenderer,
    instance: mondrian_core::AudioProcessorInstanceId,
) -> Unentered {
    let mut facts = Unentered::default();
    renderer
        .visit_unentered_continuity_for_test(&mut facts)
        .expect("actual cold owner cursors");
    let session = facts.session.expect("actual Session");
    let processor = facts.processor.expect("actual Hosted occurrence");
    assert_eq!(session.identity, processor.identity);
    assert_eq!(processor.occurrence.instance_id, instance);
    facts
}

#[test]
#[ignore = "current normal Product and leased Ring Out DLL/185-byte state; actual NoGUI App startup author transaction before first DSP"]
fn app_ring_out_headless_startup_author_handoff_closes_before_first_dsp() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let case_path =
        PathBuf::from(std::env::var_os(CASE_ENV).expect("explicit headless author case"));
    let mut case_lease = Lease::open(
        Binding {
            bytes: std::fs::metadata(&case_path).expect("actual case").len(),
            path: case_path,
            sha256: std::env::var(CASE_HASH_ENV).expect("exact case SHA256"),
        },
        16 * 1024,
    );
    // Direct typed decoding rejects unknown and duplicate known fields.
    let case: Case =
        serde_json::from_slice(&case_lease.read()).expect("strict headless author case");
    assert_eq!(case.schema, 1);
    assert_eq!(
        case.plugin.sha256,
        "df86e10f2b0aada4a3eb5f4f0bda315a941960b587b4b9513cd7ba8a9f8bd99b"
    );
    assert_eq!(
        case.opaque_state.bytes, 185,
        "new nondefault capture, not the old 184-byte default"
    );
    assert_eq!(
        case.opaque_state.sha256,
        "615db8dad30407a90391e94845c08b67df5be399481cb91a1855547f9f6ed33a"
    );
    let helper = Lease::open(case.helper, 256 * 1024 * 1024);
    let plugin = Lease::open(case.plugin, 64 * 1024 * 1024);
    let mut seed = Lease::open(case.opaque_state, 64 * 1024);
    let state_bytes = seed.read();
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.path.clone()).unwrap(),
    );
    let vst3 = Arc::new(
        mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper.path.clone()).unwrap(),
    );
    let descriptors = clap
        .install_library(plugin.path.clone())
        .expect("actual installed CLAP catalog");
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].plugin_id, PLUGIN);
    let config = mondrian_media::AudioPlaybackConfig {
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let instance = clap.create_instance(
        PLUGIN,
        mondrian_audio::AudioRenderContract {
            sample_rate: config.sample_rate,
            channel_layout: config.channel_layout,
            max_block_frames: config.chunk_frames,
            processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames: mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes: mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        },
        Some(state_bytes.clone()),
    ).expect("original catalog restores the actual nondefault state and admits its schemas");
    assert_eq!(
        instance.parameters.len(),
        9,
        "eleven native parameters include two read-only meters"
    );
    for id in 0..11 {
        assert_eq!(
            instance
                .parameters
                .contains_key(&ParameterId::new(format!("clap.param.{id}")).unwrap()),
            id < 9
        );
    }
    let gain = ParameterId::new("clap.param.7").unwrap();
    let gain_curve = instance.parameters[&gain].automation.as_ref().unwrap();
    assert_eq!(gain_curve.default_value.to_bits(), (-6.0_f64).to_bits());
    assert!(gain_curve.keyframes.is_empty());
    assert_eq!(
        instance.opaque_state.as_ref().unwrap().as_slice(),
        state_bytes.as_slice()
    );
    assert!(matches!(&instance.definition,
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
            plugin_id, binary_sha256: Some(_), ..
        } if plugin_id == PLUGIN));
    let instance_id = instance.id;
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control_with_instance(clap, vst3, config, instance, |_, _, _| {});
    let mut installed_verifier = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(state.native_audio_editor.is_none());
        assert!(state.active_sequence().unwrap().audio_program.note_lanes.is_empty());
        let renderer = weak.upgrade().expect("actual installed Timeline owner");
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        assert!(state.audio_playback.matches_source_weak(&erased));
        let verifier = renderer.owner_installation_verifier().unwrap();
        installed_verifier = Some(verifier.clone());
        let author_session = state.authoring.as_ref().unwrap().session_id();
        let initial_claim = verifier.current_claim().unwrap();
        assert!(verifier.validate(&initial_claim).is_ok());
        let prepared = unentered(&renderer, instance_id);
        let root_session = prepared.session.unwrap().identity.session_id;
        let local_session = prepared.processor.unwrap().identity.session_id;
        let generation = state.audio_playback_snapshot().generation;
        let sequence = state.active_sequence_id().unwrap();
        let mut renderer_id = None;
        let mut native_object = None;
        let mut parameter_target = None;
        let mut pending: Option<NativeAudioAuthorProposal> = None;
        let mut proposals = 0_usize;
        let mut acknowledgements = 0_usize;
        loop {
            assert!(
                Instant::now() < deadline,
                "actual startup author path did not settle within 30s"
            );
            assert!(
                state.native_audio_editor.is_none(),
                "startup authoring must not open an editor"
            );
            assert!(state.audio_playback.matches_source_weak(&erased));
            let current = unentered(&renderer, instance_id);
            assert_eq!(current.session.unwrap().identity.session_id, root_session);
            assert_eq!(
                current.processor.unwrap().identity.session_id,
                local_session
            );
            let poll = state
                .audio_playback
                .poll(AudioPlaybackMode::Idle, audio_anchor(0))
                .expect("original Media queue advances controls without warmup/PCM");
            assert_eq!(poll.snapshot.generation, generation);
            assert_eq!(poll.snapshot.in_flight, 0);
            assert_eq!(poll.snapshot.next_start_sample, 0);
            assert_eq!(poll.snapshot.output.unwrap().buffered_frames, 0);
            assert_eq!(output.lock().queued, 0);
            for event in poll.events {
                let mut delivered = None;
                match &event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        renderer_id: actual,
                        generation: observed,
                        status,
                    } => {
                        assert_eq!(*observed, generation);
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                        );
                        assert!(renderer_id.is_none_or(|prior| prior == *actual));
                        renderer_id = Some(*actual);
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id: actual,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        let target = observation.target();
                        assert_eq!(target.instance, instance_id);
                        assert_eq!(target.root_session, root_session);
                        assert_eq!(target.local_session, local_session);
                        assert!(target.continuity_epoch.is_none());
                        let claim = verifier.current_claim().unwrap();
                        assert!(verifier.validate(&claim).is_ok());
                        assert_eq!(target.installed_generation, claim.generation());
                        let object = (observation.process_id(), observation.object_serial());
                        assert!(native_object.is_none_or(|prior| prior == object));
                        native_object = Some(object);
                        let queried: std::collections::BTreeSet<_> = observation.facts().iter()
                            .filter(|fact| matches!(fact.origin, mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { .. }))
                            .map(|fact| fact.parameter.clone()).collect();
                        assert_eq!(
                            queried,
                            (0..11)
                                .map(|id| ParameterId::new(format!("clap.param.{id}")).unwrap())
                                .collect()
                        );
                        let value = observation
                            .facts()
                            .iter()
                            .rev()
                            .find(|fact| fact.parameter == gain)
                            .unwrap();
                        assert_eq!(value.value_bits, Some((-6.0_f64).to_bits()));
                        assert_eq!(value.name.as_deref(), Some("Gain Out"));
                        parameter_target = Some(target);
                    }
                    AudioPlaybackEvent::NativeAuthorProposalObserved {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert_eq!(proposal.sequence(), sequence);
                        assert_eq!(Some(proposal.target()), parameter_target);
                        assert!(
                            native_object.is_some(),
                            "actual parameter DTO authenticates the native incarnation"
                        );
                        let claim = verifier.current_claim().unwrap();
                        assert!(verifier.validate(&claim).is_ok());
                        assert_eq!(proposal.target().installed_generation, claim.generation());
                        assert!(proposal.target().continuity_epoch.is_none());
                        if let Some(prior) = &pending {
                            assert!(prior.same_retained_proposal(proposal));
                        } else {
                            proposals = proposals.checked_add(1).unwrap();
                            pending = Some(proposal.as_ref().clone());
                        }
                        delivered = Some(proposal.as_ref().clone());
                    }
                    AudioPlaybackEvent::NativeAuthorHandoffObserved {
                        renderer_id: actual,
                        generation: observed,
                        proposal,
                        status,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert!(pending
                            .as_ref()
                            .expect("actual proposal precedes handoff")
                            .same_retained_proposal(proposal));
                        match status {
                            Status::Acknowledged { target, continuity } => {
                                assert_eq!(*continuity, Continuity::Preserved);
                                assert_eq!(target.instance, instance_id);
                                assert_eq!(target.root_session, root_session);
                                assert_eq!(target.local_session, local_session);
                                assert!(target.continuity_epoch.is_none());
                                let claim = verifier.current_claim().unwrap();
                                assert!(verifier.validate(&claim).is_ok());
                                assert_eq!(target.installed_generation, claim.generation());
                                acknowledgements = acknowledgements.checked_add(1).unwrap();
                                pending = None;
                            }
                            Status::InstalledAwaitingAcknowledgement { continuity, .. } => {
                                assert_eq!(*continuity, Continuity::Preserved)
                            }
                            Status::Refused(reason) => {
                                panic!("actual headless author handoff refused: {reason:?}")
                            }
                        }
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        renderer_id: actual,
                        generation: observed,
                        batch,
                    } => {
                        assert_eq!(Some(*actual), renderer_id);
                        assert_eq!(*observed, generation);
                        assert!(!batch.loss().incomplete(), "actual native diagnostic loss");
                    }
                    other => panic!("NoGUI pre-DSP control admitted unexpected event: {other:?}"),
                }
                let before = state.authoring.as_ref().unwrap().author_generation();
                state
                    .handle_audio_playback_event(event, Instant::now())
                    .expect("original App event dispatcher");
                assert!(
                    state.native_audio_editor.is_none(),
                    "headless author work cannot create a GUI context"
                );
                if let Some(proposal) = delivered {
                    assert!(state.audio_playback.matches_source_weak(&erased));
                    let current = unentered(&renderer, instance_id);
                    assert_eq!(current.session.unwrap().identity.session_id, root_session);
                    assert_eq!(
                        current.processor.unwrap().identity.session_id,
                        local_session
                    );
                    assert_eq!(output.lock().queued, 0);
                    let author = state.authoring.as_ref().unwrap();
                    assert_eq!(author.session_id(), author_session);
                    let accepted = author.has_pending_native_authoring()
                        || author.author_generation() != before;
                    let closed = renderer
                        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                        .map_err(|error| error.to_string());
                    assert!(Instant::now() < deadline);
                    println!(
                        "HEADLESS_NATIVE_AUTHOR_DISPATCH {}",
                        serde_json::json!({
                            "proposal_revision": proposal.revision(), "callbacks": proposal.callbacks().len(),
                            "state_bytes": proposal.state().map(|bytes| bytes.len()),
                            "terminal": proposal.terminal(), "app_accepted": accepted,
                            "pending_native_authoring": author.has_pending_native_authoring(),
                            "source_closed": closed.is_ok(), "source_error": closed.as_ref().err(),
                            "process_id": native_object.unwrap().0.get(),
                            "object_serial": native_object.unwrap().1.get(),
                            "editor_absent": true, "unentered": true, "pcm_qualified": false,
                        })
                    );
                    assert!(accepted,
                        "HEADLESS_NATIVE_AUTHOR_PROPOSAL_NOT_CONSUMED: real proposal revision={} callbacks={} state_bytes={:?} reached the original App dispatcher, but no author transaction was accepted; SourceClosed={closed:?}",
                        proposal.revision(), proposal.callbacks().len(), proposal.state().map(|bytes| bytes.len()));
                }
            }
            assert!(state.native_audio_editor.is_none());
            assert!(state.audio_playback.matches_source_weak(&erased));
            let current = unentered(&renderer, instance_id);
            assert_eq!(current.session.unwrap().identity.session_id, root_session);
            assert_eq!(
                current.processor.unwrap().identity.session_id,
                local_session
            );
            if acknowledgements > 0 && pending.is_none() {
                assert_eq!(
                    acknowledgements, proposals,
                    "every actual retained proposal receives its real Ack"
                );
                assert!(!state.authoring.as_ref().unwrap().has_pending_native_authoring());
                renderer.validate_native_author_source_closed(&ExecutionCancellationToken::new())
                    .expect("actual SourceClosed after original Authoring/install/native acknowledgement");
                let claim = verifier.current_claim().unwrap();
                assert!(verifier.validate(&claim).is_ok());
                assert!(Instant::now() < deadline);
                break;
            }
            std::thread::yield_now();
        }
    }));
    // Release all test observation aliases before the original consuming close.
    // Cleanup cannot turn the frozen author acceptance/SourceClosed result green.
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    let clean = cleanup.all_resources_released()
        && weak.upgrade().is_none()
        && installed_verifier.as_ref().is_none_or(|verifier| {
            matches!(
                verifier.current_claim(),
                Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
            )
        });
    if let Err(payload) = target {
        if !clean {
            eprintln!("headless author consuming cleanup incomplete: {cleanup:?}");
        }
        std::panic::resume_unwind(payload);
    }
    assert!(clean, "HEADLESS_NATIVE_AUTHOR_CLEANUP_HOLD: {cleanup:?}");
}
