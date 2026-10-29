//! A finite real Surge App/Media cold entry; software enqueue, not a device/RT claim.
use super::*;
use mondrian_audio::{
    AudioContinuityEvidenceVisitor, AudioExecutionError, AudioNativeProcessorAddress,
    AudioNativeProcessorOwnerLease, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioSessionCursorEvidence, AudioSessionCursorState,
};
use mondrian_core::{
    AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationClaim, AuthoringList,
    ExecutionCancellationToken, MusicalTimeReference, NativeAudioAuthorProposal,
};
use mondrian_media::{
    AudioBuffer, AudioOutputAdapter, AudioPcmNativeAuthorResumeKey,
    AudioPcmNativeAuthorTerminalBatchStatus as Status, AudioPcmRenderer,
};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorOutputPort, AudioRoute,
    AudioRouteDestination, AudioRouteSource,
};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const CLASS: &str = "ABCDEF019182FAEB566D624153675854";
const BLOCK: usize = 256;
const WINDOWS: usize = 24;
const START: i64 = 2_000;

// Record only successful real enqueue arguments. Device/clock/control semantics
// remain those of the existing no-device adapter; no test-generated PCM.
struct RecordingOutput {
    original: NativeHoldPlatformOutput,
    buffers: Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
}
impl AudioOutputAdapter for RecordingOutput {
    fn poll(&mut self) -> Option<mondrian_media::RealtimeAudioOutputEvent> {
        self.original.poll()
    }
    fn enqueue(
        &mut self,
        buffer: &AudioBuffer,
    ) -> Result<(), mondrian_media::RealtimeAudioOutputEnqueueError> {
        self.original.enqueue(buffer)?;
        let mut buffers = self.buffers.lock();
        assert!(buffers.len() < WINDOWS);
        assert_eq!(buffer.frame_count(), BLOCK);
        buffers.push(buffer.clone());
        Ok(())
    }
    fn clear(&mut self) {
        self.original.clear();
    }
    fn validate_deactivation(&self) -> Result<(), mondrian_media::RealtimeAudioOutputControlError> {
        self.original.validate_deactivation()
    }
    fn deactivate(
        &self,
    ) -> Result<
        Option<mondrian_media::RealtimeAudioOutputQuiescenceToken>,
        mondrian_media::RealtimeAudioOutputControlError,
    > {
        self.original.deactivate()
    }
    fn is_quiescent(
        &self,
        token: mondrian_media::RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, mondrian_media::RealtimeAudioOutputControlError> {
        self.original.is_quiescent(token)
    }
    fn activate_after_discard(
        &self,
        token: mondrian_media::RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), mondrian_media::RealtimeAudioOutputControlError> {
        self.original.activate_after_discard(token, frames)
    }
    fn buffered_frames(&self) -> usize {
        self.original.buffered_frames()
    }
    fn capacity_frames(&self) -> Option<usize> {
        self.original.capacity_frames()
    }
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        self.original.snapshot()
    }
}

#[derive(Default)]
struct Cursors {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
fn cursors(renderer: &TimelineAudioPcmRenderer) -> Cursors {
    renderer.with_delivery_observation_for_test(|delivery| {
        let mut facts = Cursors::default();
        delivery.visit_continuity_evidence(&mut facts).unwrap();
        assert_eq!((facts.sessions.len(), facts.processors.len()), (1, 1));
        assert!(delivery.requires_state_entry());
        assert_eq!(facts.sessions[0].public_output_lookahead_frames, 0);
        assert_eq!(delivery.resource_footprint().media_window_bytes, 0);
        facts
    })
}
fn owner(renderer: &TimelineAudioPcmRenderer) -> AudioNativeProcessorOwnerLease {
    let facts = cursors(renderer);
    let processor = facts.processors[0];
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        other => panic!("unexpected Surge cursor: {other:?}"),
    };
    renderer.with_delivery_observation_for_test(|delivery| {
        delivery
            .native_owner_lease(AudioNativeProcessorAddress {
                root: facts.sessions[0].identity,
                local: processor.identity,
                occurrence: processor.occurrence,
                epoch,
            })
            .unwrap()
    })
}

// Correlation evidence only. All preparation, canonical publication, Source CAS
// and native acknowledgements belong to the original App/Media dispatcher.
struct AuthorReceipt {
    proposal: NativeAudioAuthorProposal,
    key: Option<AudioPcmNativeAuthorResumeKey>,
    previous: AudioNativeProcessorOwnerLease,
    claim: AudioOwnerInstallationClaim,
    object: mondrian_audio::NativeAudioObjectObservation,
    acknowledged: bool,
}
#[derive(Default)]
struct Observations {
    renderer_id: Option<mondrian_core::AudioPcmRendererMaterializationId>,
    ready: bool,
    author: Vec<AuthorReceipt>,
}
impl Observations {
    fn settled(&self, state: &AppState) -> bool {
        self.ready
            && self.author.iter().all(|item| item.acknowledged)
            && !state.authoring.as_ref().unwrap().has_pending_native_authoring()
    }
    fn dispatch(
        &mut self,
        state: &mut AppState,
        renderer: &Arc<TimelineAudioPcmRenderer>,
        event: AudioPlaybackEvent,
        generation: u64,
        frontier: i64,
    ) {
        let (actual, observed) = match &event {
            AudioPlaybackEvent::NativeOwnerBindingsObserved { renderer_id, generation, .. }
            | AudioPlaybackEvent::NativeDiagnosticsObserved { renderer_id, generation, .. }
            | AudioPlaybackEvent::NativeParameterObservationsObserved {
                renderer_id,
                generation,
                ..
            }
            | AudioPlaybackEvent::NativeAuthorProposalObserved {
                renderer_id, generation, ..
            }
            | AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared {
                renderer_id,
                generation,
                ..
            }
            | AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                renderer_id,
                generation,
                ..
            } => (Some(*renderer_id), *generation),
            AudioPlaybackEvent::NativeAuthorPending { target, start_sample, generation } => {
                assert_eq!(*start_sample, frontier);
                assert_eq!(*target, owner(renderer).editor_target());
                (None, *generation)
            }
            other => panic!("unexpected device, replacement, recovery or render fault: {other:?}"),
        };
        assert_eq!(observed, generation);
        if let Some(actual) = actual {
            assert!(self.renderer_id.replace(actual).is_none_or(|prior| prior == actual));
        }
        match &event {
            AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } => {
                assert_eq!(
                    *status,
                    mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                );
                self.ready = true;
            }
            AudioPlaybackEvent::NativeDiagnosticsObserved { batch, .. } => {
                assert!(!batch.loss().incomplete());
            }
            AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                // This is a historical completion, not a current capability:
                // an earlier event may already have published a new binding.
                let historical = observation.target();
                let current = owner(renderer).editor_target();
                assert_eq!(
                    (
                        historical.instance,
                        historical.root_session,
                        historical.local_session
                    ),
                    (
                        current.instance,
                        current.root_session,
                        current.local_session
                    )
                );
                assert!(historical.continuity_epoch.is_none_or(|epoch| epoch == generation));
            }
            AudioPlaybackEvent::NativeAuthorProposalObserved { proposal, .. } => {
                assert_eq!(proposal.sequence(), state.active_sequence().unwrap().id);
                assert!(proposal.terminal(), "headless canonical terminal path");
                if !self.author.iter().any(|item| item.proposal.same_retained_proposal(proposal)) {
                    let previous = owner(renderer);
                    assert_eq!(proposal.target(), previous.editor_target());
                    let verifier = renderer.owner_installation_verifier().unwrap();
                    let claim = verifier.current_claim().unwrap();
                    verifier.validate(&claim).unwrap();
                    assert_eq!(proposal.target().installed_generation, claim.generation());
                    self.author.push(AuthorReceipt {
                        proposal: proposal.as_ref().clone(),
                        key: None,
                        previous,
                        claim,
                        object: renderer.observe_native_object_for_test(previous).unwrap(),
                        acknowledged: false,
                    });
                }
            }
            AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared { proposal, key, .. } => {
                let item = self
                    .author
                    .iter_mut()
                    .find(|item| item.proposal.same_retained_proposal(proposal))
                    .unwrap();
                assert!(!item.acknowledged);
                if let Some(prior) = &item.key {
                    assert!(key == prior || key.continues(prior));
                }
                item.key = Some(key.clone());
                assert!(state.authoring.as_ref().unwrap().has_pending_native_authoring());
            }
            AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                proposal, key, status, ..
            } => {
                let item = self
                    .author
                    .iter_mut()
                    .find(|item| item.proposal.same_retained_proposal(proposal))
                    .unwrap();
                assert_eq!(item.key.as_ref(), Some(key));
                let target = match status {
                    Status::Acknowledged { target }
                    | Status::InstalledAwaitingAcknowledgement { target } => *target,
                    Status::Refused(reason) => {
                        panic!("real author publication refused: {reason:?}")
                    }
                };
                let current = owner(renderer);
                assert_eq!(target, current.editor_target());
                assert_eq!(
                    renderer.observe_native_object_for_test(current).unwrap(),
                    item.object
                );
                let old = item.previous.editor_target();
                assert_eq!(
                    (
                        target.instance,
                        target.root_session,
                        target.local_session,
                        target.continuity_epoch
                    ),
                    (
                        old.instance,
                        old.root_session,
                        old.local_session,
                        old.continuity_epoch
                    )
                );
                let verifier = renderer.owner_installation_verifier().unwrap();
                let claim = verifier.current_claim().unwrap();
                verifier.validate(&claim).unwrap();
                assert_eq!(target.installed_generation, claim.generation());
                assert_ne!(claim.generation(), item.claim.generation());
                assert!(verifier.validate(&item.claim).is_err());
                assert!(renderer.observe_native_object_for_test(item.previous).is_err());
                if matches!(status, Status::Acknowledged { .. }) {
                    assert!(
                        !item.acknowledged,
                        "one complete acknowledgement per retained prefix"
                    );
                    item.acknowledged = true;
                }
            }
            AudioPlaybackEvent::NativeAuthorPending { .. } => {}
            _ => unreachable!(),
        }
        state.handle_audio_playback_event(event, Instant::now()).unwrap();
    }
}

// An observation can find callbacks/flush/Ack or a real Begin/End still pending.
// Drive only the original dispatcher until its original deadline, never synthesize
// a close. Permanent faults fail immediately; only the original validator's Ok
// proves closure. Retain the last typed cause for a useful deadline failure.
fn source_closed(
    renderer: &TimelineAudioPcmRenderer,
    epoch: Option<u64>,
    last_hold: &mut Option<mondrian_audio::NativeAudioAuthorHoldReason>,
) -> bool {
    use mondrian_audio::{
        NativeAudioAuthorAdmissionError as Error, NativeAudioAuthorHoldReason as Hold,
    };
    match renderer.observe_native_author_source_closed_for_test(&ExecutionCancellationToken::new())
    {
        None => false,
        Some(Ok(())) => {
            *last_hold = None;
            true
        }
        Some(Err(Error::Pending { target, reason }))
            if matches!(
                reason,
                Hold::PendingCallbacks
                    | Hold::PendingParameterObservations
                    | Hold::DspFeedbackPending
                    | Hold::OutstandingDelivery
                    | Hold::OpenBoundary
            ) =>
        {
            let current = owner(renderer).editor_target();
            assert_eq!(
                (
                    target.instance,
                    target.root_session,
                    target.local_session,
                    target.owner,
                    target.insertion
                ),
                (
                    current.instance,
                    current.root_session,
                    current.local_session,
                    current.owner,
                    current.insertion
                )
            );
            assert_eq!(target.continuity_epoch, epoch);
            *last_hold = Some(reason);
            false
        }
        Some(Err(error)) => panic!("actual SourceClosed validation failed: {error:?}"),
    }
}

fn assert_author(state: &AppState, original: &Sequence) {
    let mut current = state.active_sequence().unwrap().clone();
    let track = original.audio_tracks[0].id;
    let expected = &original.audio_program.track_channels[&track].strip.pre_fader.processors[0];
    let actual = &mut current
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(actual.parameters.len(), expected.parameters.len());
    assert_eq!(
        actual.parameters, expected.parameters,
        "no native rescan may invent host overrides"
    );
    assert!(actual.parameters.values().all(|parameter| parameter.automation.is_none()));
    assert!(actual
        .opaque_state
        .as_ref()
        .is_some_and(|state| !state.is_empty() && state.len() <= 128 * 1024));
    // A real coherent native capture may update only its opaque state here.
    actual.opaque_state = expected.opaque_state.clone();
    assert!(
        current.author_state_eq_ignoring_revision(original),
        "complete score/schema/author state"
    );
    assert!(state.native_audio_editor.is_none());
}

#[test]
#[ignore = "pinned Surge XT 1.3.4 and retained origin state; finite App Realtime software FIFO only"]
fn app_surge_xt_vst3_first_nonzero_note_playback_sustains_main() {
    let file = |name| {
        let path =
            std::path::PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let seed_path = file("MONDRIAN_SURGE_VST3_ORIGIN_STATE");
    let seed = std::fs::read(&seed_path).unwrap();
    assert_eq!(seed.len(), 51_928, "retained state, never recapture");
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    let descriptors = vst3.install_plugin(file("MONDRIAN_SURGE_XT_VST3_DLL")).unwrap();
    let matching: Vec<_> = descriptors.iter().filter(|item| item.class_id == CLASS).collect();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].name, "Surge XT");
    assert_eq!(matching[0].vendor.as_deref(), Some("Surge Synth Team"));
    assert_eq!(matching[0].version.as_deref(), Some("1.3.4"));
    let config = playback_config();
    let instance = vst3
        .create_instance(CLASS, render_contract(config), Some(seed.clone()))
        .expect("real Realtime B256 instance from the retained seed");
    assert_eq!(
        instance.opaque_state.as_ref().map(AsRef::<[u8]>::as_ref),
        Some(seed.as_slice())
    );
    assert_eq!(instance.parameters.len(), 2855);
    assert_eq!(
        instance.parameters.values().filter(|row| row.schema.is_animatable).count(),
        774
    );
    assert!(instance
        .parameters
        .iter()
        .all(|(id, row)| row.schema.parameter_id == *id && row.automation.is_none()));
    assert!(
        matches!(&instance.definition, mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 {
        class_id, vendor, binary_sha256: Some(_), schema_version: 1, ..
    } if class_id == CLASS && vendor.as_deref() == Some("Surge Synth Team"))
    );
    run_app_surge_playback(
        clap,
        vst3,
        note_sequence(instance, "vst3.note-input.0".into()),
        Some((&seed_path, &seed)),
        true,
    );
}

#[test]
#[ignore = "pinned Surge XT 1.3.4 CLAP; fresh in-memory capture and finite App Realtime software FIFO only"]
fn app_surge_xt_clap_first_nonzero_note_playback_sustains_main() {
    let (clap, vst3, sequence) = fresh_clap_surge_case();
    run_app_surge_playback(clap, vst3, sequence, None, false);
}

fn fresh_clap_surge_case() -> (
    Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    Sequence,
) {
    use sha2::{Digest, Sha256};
    const ID: &str = "org.surge-synth-team.surge-xt";
    let file = |name| {
        let path =
            std::path::PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_CLAP_TEST_HELPER");
    let plugin = file("MONDRIAN_SURGE_XT_CLAP_DLL");
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 = Arc::new(
        mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let descriptors = clap.install_library(plugin.clone()).unwrap();
    let matching: Vec<_> = descriptors.iter().filter(|item| item.plugin_id == ID).collect();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].name, "Surge XT");
    assert_eq!(matching[0].vendor.as_deref(), Some("Surge Synth Team"));
    assert_eq!(matching[0].version.as_deref(), Some("1.3.4"));
    let render = render_contract(playback_config());
    // One canonical instance: the public None path captures the actual default
    // state. Keep it only in memory; App's original factory restores this Some.
    let instance = clap
        .create_instance(ID, render, None)
        .expect("actual fresh CLAP catalog capture");
    let seed = instance.opaque_state.as_ref().map(AsRef::<[u8]>::as_ref).unwrap().to_vec();
    assert!(!seed.is_empty() && seed.len() <= 64 * 1024);
    let digest: [u8; 32] = Sha256::digest(&seed).into();
    // This bounded metadata child restores that same captured seed. It neither
    // supplies an execution owner nor manufactures native ports/Note dialect.
    let offer =
        mondrian_audio::probe_clap_note_processing_offer(&helper, &plugin, ID, render, Some(&seed))
            .expect("real same-seed NoteOffer5");
    assert_eq!(offer.schema_version, 6);
    assert_eq!(offer.notes.schema_version, 2); // response version, not port count
    assert_eq!(offer.notes.plugin_id, ID);
    assert_eq!(offer.notes.state_sha256, Some(digest));
    assert_eq!(offer.notes.sample_rate, 48_000);
    assert_eq!(offer.notes.channel_layout, AudioChannelLayout::Stereo);
    assert_eq!(offer.notes.max_block_frames, BLOCK);
    assert!(!offer.notes.offline);
    assert_eq!(
        offer.processing_mode(),
        mondrian_audio::AudioProcessingMode::Realtime
    );
    assert_eq!(offer.latency_frames(), 0);
    assert_eq!(
        offer.notes.contract.completion,
        mondrian_audio::AudioNoteCompletionPolicy::NativeEnd
    );
    assert!(offer.notes.contract.ports.iter().all(|port| port.channel_count == 16));
    let note_port = offer.notes.contract.ports.first().expect("actual negotiated CLAP Note input");
    assert_eq!(
        offer.main_audio_input(),
        mondrian_audio::ClapMainAudioInput::Absent
    );
    let auxiliary_outputs = offer.auxiliary_outputs();
    assert_eq!(
        auxiliary_outputs.buses.len(),
        2,
        "retain both native stereo auxiliary outputs"
    );
    assert!(auxiliary_outputs
        .buses
        .iter()
        .all(|bus| bus.channel_layout == AudioChannelLayout::Stereo));
    let auxiliary_inputs: Vec<_> = offer.auxiliary_inputs().collect();
    assert_eq!(
        auxiliary_inputs.len(),
        1,
        "retain the actual zero-filled non-Main input"
    );
    assert_eq!(auxiliary_inputs[0].1, AudioChannelLayout::Stereo);
    assert!(
        matches!(&instance.definition, mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
        plugin_id, schema_version: 1, binary_sha256: Some(hash),
    } if plugin_id == ID && *hash == offer.notes.binary_sha256)
    );
    let mut ids = std::collections::BTreeSet::new();
    for native in offer.parameters() {
        let id = native.parameter_id().unwrap();
        assert!(ids.insert(id.clone()), "complete unique native directory");
        if native.read_only {
            assert!(!instance.parameters.contains_key(&id));
        } else {
            let row = instance.parameters.get(&id).expect("every writable native parameter");
            assert_eq!(row.schema, native.authoring_schema().unwrap());
            assert!(row.automation.is_none(), "native state owns this value");
        }
    }
    assert!(!instance.parameters.is_empty());
    assert_eq!(
        instance.parameters.len(),
        offer.parameters().iter().filter(|row| !row.read_only).count()
    );
    assert_eq!(
        instance.opaque_state.as_ref().map(AsRef::<[u8]>::as_ref),
        Some(seed.as_slice())
    );
    (
        clap,
        vst3,
        note_sequence(instance, note_port.port_key.clone()),
    )
}

fn note_sequence(
    instance: mondrian_timeline::audio::AudioProcessorInstance,
    input_port_key: String,
) -> Sequence {
    let id = instance.id;
    let mut sequence = Sequence::new("real Surge first nonzero App Note playback");
    sequence.settings.frame_rate = mondrian_core::Rational::FPS_24;
    sequence.settings.audio_sample_rate = 48_000;
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color::from_rgba8(0, 0, 0, 255),
                TimelineTime::ZERO,
                TimelineTime::new(1, 1).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
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
    sequence.audio_program.musical_time_reference = Some(
        MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap()).unwrap(),
    );
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget { track_id: track, processor_id: id, input_port_key },
        notes: AuthoringList::from(vec![AudioNote {
            id: AudioNoteId::new(),
            start: TimelineTime::new(127, 48_000).unwrap(),
            end: TimelineTime::new(8193, 48_000).unwrap(),
            channel: 0,
            key: 60,
            on_velocity: 0.75,
            off_velocity: 0.25,
        }]),
    });
    sequence.audio_program.routes.clear();
    sequence.audio_program.routes.push(AudioRoute::new(
        AudioRouteSource::ProcessorOutput {
            processor_id: id,
            port: AudioProcessorOutputPort::Main,
        },
        AudioRouteDestination::Output(sequence.audio_program.outputs[0].id),
    ));
    assert!(sequence.audio_program.sidechain_routes.is_empty());
    assert!(sequence.audio_tracks.iter().all(|track| track.clips.is_empty()));
    sequence
}

fn playback_config() -> mondrian_media::AudioPlaybackConfig {
    mondrian_media::AudioPlaybackConfig {
        sample_rate: 48_000,
        channel_layout: crate::app::AUDIO_OUTPUT_LAYOUT,
        chunk_frames: BLOCK,
        preroll_frames: BLOCK,
        high_watermark_frames: BLOCK,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    }
}

fn render_contract(
    config: mondrian_media::AudioPlaybackConfig,
) -> mondrian_audio::AudioRenderContract {
    mondrian_audio::AudioRenderContract {
        sample_rate: config.sample_rate,
        channel_layout: config.channel_layout,
        max_block_frames: BLOCK,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}

fn install_surge_output(
    state: &mut AppState,
    buffers: Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
) -> Arc<parking_lot::Mutex<NativeHoldPlatformState>> {
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let config = playback_config();
    let mut snapshot = audio_snapshot();
    snapshot.active = false;
    snapshot.buffered_frames = 0;
    snapshot.callback_consumed_frames = 0;
    snapshot.active_callback_consumed_frames = 0;
    snapshot.active_duration = None;
    snapshot.callback_count = 0;
    snapshot.last_callback_frames = 0;
    snapshot.last_callback_playback_delay = Some(Duration::ZERO);
    snapshot.last_callback_playback_delay_uncertainty = Some(Duration::ZERO);
    snapshot.last_callback_age = Some(Duration::ZERO);
    let output = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
        snapshot,
        revision: 0,
        acknowledged: 0,
        auto_acknowledge: true,
        queued: 0,
    }));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            config,
            Box::new(RecordingOutput {
                original: NativeHoldPlatformOutput(output.clone()),
                buffers: buffers.clone(),
            }),
        )
        .unwrap(),
    ));
    output
}

fn prepare_idle_surge(
    state: &mut AppState,
    buffers: &Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
    weak: &mut std::sync::Weak<TimelineAudioPcmRenderer>,
    observed: &mut Observations,
    deadline: Option<Instant>,
) -> (u64, Instant) {
    state.seek(1).expect("public frame1 seek at24fps");
    assert_eq!(state.current_frame(), 1);
    assert_eq!(state.audio_playback_snapshot().next_start_sample, START);
    assert!(state.native_audio_renderer.is_none());
    let source = state.audio_playback_renderer("Surge first S2000 prepare").unwrap().unwrap();
    assert_eq!(
        source.renderer.delivery_evidence().program_layout,
        AudioChannelLayout::Stereo
    );
    assert_eq!(
        source.renderer.delivery_evidence().target_layout,
        crate::app::AUDIO_OUTPUT_LAYOUT
    );
    assert!(source.renderer.owner_installation_verifier().is_none());
    *weak = Arc::downgrade(&source.renderer);
    assert!(matches!(
        cursors(&source.renderer).processors[0].state,
        AudioProcessorCursorState::Unentered
    ));
    state
        .commit_audio_playback(
            "Surge first S2000 prepare",
            audio_anchor(START),
            Some(source),
        )
        .unwrap();
    let generation = state.audio_playback_snapshot().generation;
    let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
    let mut last_source_hold = None;
    // Initialization control callbacks close through the same canonical
    // dispatcher before the first positive signal window may enter.
    loop {
        let renderer = weak.upgrade().unwrap();
        let poll = state.audio_playback.poll(AudioPlaybackMode::Idle, audio_anchor(START)).unwrap();
        assert_eq!(poll.snapshot.generation, generation);
        assert_eq!(poll.snapshot.next_start_sample, START);
        assert!(buffers.lock().is_empty());
        for event in poll.events {
            observed.dispatch(state, &renderer, event, generation, START);
        }
        if observed.settled(state)
            && state.audio_playback_snapshot().in_flight == 0
            && source_closed(&renderer, None, &mut last_source_hold)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial real Source did not close: {last_source_hold:?}"
        );
        std::thread::yield_now();
    }
    (generation, deadline)
}

fn shutdown_surge(
    state: &mut AppState,
    weak: &std::sync::Weak<TimelineAudioPcmRenderer>,
    verifier: Option<&mondrian_core::AudioOwnerInstallationVerifier>,
    handle: Option<&OwnedHandle>,
) {
    let evidence = state.shutdown_audio_playback_and_wait();
    assert!(evidence.all_workers_terminated(), "{evidence:?}");
    assert!(evidence.shutdown_resource_facts_complete_at_deadline);
    assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
    assert_eq!(evidence.foreign_owner_panics, 0);
    assert_eq!(evidence.foreign_owner_abandonments, 0);
    assert!(weak.upgrade().is_none());
    if let Some(verifier) = verifier {
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
    }
    if let Some(handle) = handle {
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
    }
}

fn run_app_surge_playback(
    clap: Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    vst3: Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    sequence: Sequence,
    retained_seed: Option<(&std::path::Path, &[u8])>,
    require_author_receipt: bool,
) {
    let original = sequence.clone();
    let directory = tempfile::tempdir().unwrap();
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let revision = library.database_revision().unwrap();
    let mut state = AppState::with_native_audio_catalogs(clap, vst3);
    let buffers = Arc::new(parking_lot::Mutex::new(Vec::with_capacity(WINDOWS)));
    let output = install_surge_output(&mut state, buffers.clone());
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library.clone()));
    state.synchronize_playback_authoring_binding().unwrap();
    assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&original));
    assert_eq!(library.database_revision().unwrap(), revision);
    run_loaded_surge_playback(
        &mut state,
        output,
        buffers,
        retained_seed,
        require_author_receipt,
        |_| {},
        None,
    );
}

fn run_loaded_surge_playback(
    state: &mut AppState,
    output: Arc<parking_lot::Mutex<NativeHoldPlatformState>>,
    buffers: Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
    retained_seed: Option<(&std::path::Path, &[u8])>,
    require_author_receipt: bool,
    before_entry: impl FnOnce(&TimelineAudioPcmRenderer),
    deadline: Option<Instant>,
) {
    let original = state.active_sequence().unwrap().clone();
    let library = state
        .asset_library_handle()
        .expect("the actual installed Authoring Session library");
    let revision = library.database_revision().unwrap();
    let author_session = state.authoring_session_id().unwrap();
    let mut weak = std::sync::Weak::<TimelineAudioPcmRenderer>::new();
    let mut verifier = None;
    let mut handle: Option<OwnedHandle> = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut observed = Observations::default();
        let (generation, deadline) =
            prepare_idle_surge(state, &buffers, &mut weak, &mut observed, deadline);
        let mut last_source_hold = None;
        assert_author(state, &original);
        let renderer = weak.upgrade().unwrap();
        let cold = owner(&renderer);
        let anchor = cold.editor_target();
        assert!(anchor.continuity_epoch.is_none());
        assert!(matches!(
            cursors(&renderer).sessions[0].state,
            AudioSessionCursorState::Unentered
        ));
        let object = renderer.observe_native_object_for_test(cold).unwrap();
        assert_ne!(object.process_id().get(), std::process::id());
        // SAFETY: query/synchronize rights for this observed installed native PID.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
        assert!(!raw.is_null());
        handle = Some(unsafe { OwnedHandle::from_raw_handle(raw.cast()) });
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_ref().unwrap().as_raw_handle().cast(), 0) },
            258
        );
        verifier = Some(renderer.owner_installation_verifier().unwrap());
        before_entry(&renderer);
        drop(renderer);
        for window in 0..WINDOWS {
            let start = START + (window * BLOCK) as i64;
            let end = start + BLOCK as i64;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "finite Surge software windows did not settle: {last_source_hold:?}"
                );
                let renderer = weak.upgrade().unwrap();
                let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
                assert!(state.audio_playback.matches_source_weak(&erased));
                assert!(state
                    .native_audio_renderer
                    .as_ref()
                    .is_some_and(|current| std::sync::Weak::ptr_eq(current, &weak)));
                let poll = state
                    .audio_playback
                    .poll(AudioPlaybackMode::Preroll, audio_anchor(start))
                    .unwrap();
                assert_eq!(poll.snapshot.generation, generation);
                assert!(poll.snapshot.next_start_sample <= end);
                assert!(!poll.snapshot.output.unwrap().active);
                for event in poll.events {
                    observed.dispatch(state, &renderer, event, generation, start);
                }
                // A real NativeAuthorPending leaves this same frontier parked.
                // Natural dispatcher completion, never a manual reset/Enter,
                // permits the original request to enqueue on the next poll.
                if buffers.lock().len() == window + 1
                    && state.audio_playback_snapshot().in_flight == 0
                    && observed.settled(state)
                    && source_closed(&renderer, Some(generation), &mut last_source_hold)
                {
                    let current = owner(&renderer);
                    let target = current.editor_target();
                    assert_eq!(
                        (target.instance, target.root_session, target.local_session),
                        (anchor.instance, anchor.root_session, anchor.local_session)
                    );
                    assert_eq!(target.continuity_epoch, Some(generation));
                    assert_eq!(
                        renderer.observe_native_object_for_test(current).unwrap(),
                        object
                    );
                    assert!(renderer.observe_native_object_for_test(cold).is_err());
                    let installed = renderer.owner_installation_verifier().unwrap();
                    let claim = installed.current_claim().unwrap();
                    installed.validate(&claim).unwrap();
                    assert_eq!(target.installed_generation, claim.generation());
                    verifier = Some(installed);
                    let facts = cursors(&renderer);
                    assert!(matches!(facts.sessions[0].state,
                        AudioSessionCursorState::Active { epoch, next_public_sample, next_execution_sample, .. }
                        if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                            && next_public_sample.sample() == end && next_execution_sample.sample() == end));
                    assert!(matches!(facts.processors[0].state,
                        AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                        if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                            && next_input_signal_sample.sample() == end));
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(buffers.lock().len(), window + 1);
            assert_eq!(output.lock().queued, (window + 1) * BLOCK);
            assert_eq!(state.audio_playback_snapshot().next_start_sample, end);
            assert_eq!(state.authoring_session_id(), Some(author_session));
            assert_author(state, &original);
        }
        if require_author_receipt {
            assert!(
                !observed.author.is_empty(),
                "natural callbacks must complete canonical/CAS/Ack within the finite gate"
            );
        }
        assert!(observed.settled(state));
        let recorded = buffers.lock();
        assert_eq!(recorded.len(), WINDOWS);
        let mut sustain_energy = 0.0_f64;
        for (window, buffer) in recorded.iter().enumerate() {
            assert_eq!(buffer.sample_rate, 48_000);
            assert_eq!(buffer.channel_layout, AudioChannelLayout::Stereo);
            assert_eq!(buffer.samples.len(), BLOCK * 2);
            assert!(buffer.samples.iter().all(|sample| sample.is_finite()));
            // Fixed last16 blocks [4048,8144), not a searched loud interval.
            if window >= 8 {
                let energy =
                    buffer.samples.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>();
                assert!(energy > 0.0, "silent fixed sustain block {window}");
                sustain_energy += energy;
            }
        }
        assert!(sustain_energy > 1.0e-12, "{sustain_energy}");
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 8_144);
        assert_eq!(output.lock().queued, 6_144);
        assert_eq!(library.database_revision().unwrap(), revision);
        if let Some((path, seed)) = retained_seed {
            assert_eq!(std::fs::read(path).unwrap(), seed);
        }
        // This window ends before Off8193; no release, device or DSP-golden claim.
        (observed.author.len(), sustain_energy)
    }));
    // Consume the original Media/Runtime owners before propagating target panic.
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shutdown_surge(state, &weak, verifier.as_ref(), handle.as_ref());
    }));
    let (author_prefixes, sustain_energy) = match target {
        Ok(result) => result,
        Err(primary) => std::panic::resume_unwind(primary),
    };
    if let Err(error) = cleanup {
        std::panic::resume_unwind(error);
    }
    let processor = &original.audio_program.track_channels[&original.audio_tracks[0].id]
        .strip
        .pre_fader
        .processors[0];
    let format = match &processor.definition {
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 { .. } => "VST3",
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap { .. } => "CLAP",
        other => panic!("not a native Surge definition: {other:?}"),
    };
    println!("SURGE_APP_FIFO format={format} captured_state_bytes={} author_parameters={} author_prefixes={author_prefixes} windows={WINDOWS} sustain_energy={sustain_energy}",
        processor.opaque_state.as_ref().unwrap().len(), processor.parameters.len());
}

#[test]
#[ignore = "pinned Surge XT 1.3.4 CLAP; public Save As/Close/new App Open then first nonzero software FIFO, no cross-instance DSP equivalence"]
fn app_surge_xt_clap_reopened_project_first_nonzero_note_playback_sustains_main() {
    let (clap, vst3, sequence) = fresh_clap_surge_case();
    let captured = sequence.clone();
    let files = tempfile::tempdir().unwrap();
    let archive = files.path().join("surge-closed-author.mdp");
    let library = AssetLibrary::open(files.path().join("library")).unwrap();
    let mut state = AppState::with_native_audio_catalogs(clap.clone(), vst3.clone());
    let buffers = Arc::new(parking_lot::Mutex::new(Vec::with_capacity(WINDOWS)));
    let output = install_surge_output(&mut state, buffers.clone());
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));
    state.synchronize_playback_authoring_binding().unwrap();
    let mut weak = std::sync::Weak::<TimelineAudioPcmRenderer>::new();
    let mut verifier = None;
    let mut handle: Option<OwnedHandle> = None;
    let mut reopened: Option<AppState> = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut observed = Observations::default();
        // One original30s deadline starts at this Source commit and is retained
        // through durable Save/Close, the new App's Open, and all fresh PCM.
        let (_, deadline) =
            prepare_idle_surge(&mut state, &buffers, &mut weak, &mut observed, None);
        assert_author(&state, &captured);
        let renderer = weak.upgrade().unwrap();
        let old_owner = owner(&renderer);
        assert!(old_owner.editor_target().continuity_epoch.is_none());
        assert!(matches!(
            cursors(&renderer).sessions[0].state,
            AudioSessionCursorState::Unentered
        ));
        let old_object = renderer.observe_native_object_for_test(old_owner).unwrap();
        assert_ne!(old_object.process_id().get(), std::process::id());
        // SAFETY: query/synchronize only for this observed actual native PID.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, old_object.process_id().get()) };
        assert!(!raw.is_null());
        handle = Some(unsafe { OwnedHandle::from_raw_handle(raw.cast()) });
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_ref().unwrap().as_raw_handle().cast(), 0) },
            258
        );
        verifier = Some(renderer.owner_installation_verifier().unwrap());
        drop(renderer);
        assert_eq!(output.lock().queued, 0);
        assert!(buffers.lock().is_empty());
        assert!(Instant::now() < deadline);
        state
            .save_project_file_as(archive.clone())
            .expect("public durable Save As after real SourceClosed");
        assert!(Instant::now() < deadline);
        assert!(archive.is_file());
        assert_author(&state, &captured);
        // Snapshot the actual successfully saved canonical state. Initialization
        // may have legitimately captured a different coherent opaque state.
        let saved = state.active_sequence().unwrap().clone();
        let author = state.authoring.as_ref().unwrap();
        let project = author.project_id();
        let old_session = author.session_id();
        assert!(!author.has_pending_native_authoring());
        let renderer = weak.upgrade().unwrap();
        let old_owner = owner(&renderer);
        let old_target = old_owner.editor_target();
        let old_verifier = renderer.owner_installation_verifier().unwrap();
        let old_claim = old_verifier.current_claim().unwrap();
        old_verifier.validate(&old_claim).unwrap();
        assert_eq!(
            renderer.observe_native_object_for_test(old_owner).unwrap(),
            old_object
        );
        assert!(matches!(
            cursors(&renderer).processors[0].state,
            AudioProcessorCursorState::Unentered
        ));
        verifier = Some(old_verifier.clone());
        drop(renderer);
        state
            .close_project()
            .expect("public Close consumes the original no-signal Source");
        assert!(Instant::now() < deadline);
        shutdown_surge(&mut state, &weak, verifier.as_ref(), handle.as_ref());
        assert!(state.authoring.is_none() && state.native_audio_renderer.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert_eq!(output.lock().queued, 0);
        assert!(buffers.lock().is_empty());

        let mut fresh = AppState::with_native_audio_catalogs(clap, vst3);
        let fresh_buffers = Arc::new(parking_lot::Mutex::new(Vec::with_capacity(WINDOWS)));
        let fresh_output = install_surge_output(&mut fresh, fresh_buffers.clone());
        reopened = Some(fresh);
        let fresh = reopened.as_mut().unwrap();
        assert!(Instant::now() < deadline);
        fresh
            .open_project_file(archive.clone())
            .expect("public new App Open of the durable Project");
        assert!(Instant::now() < deadline);
        let author = fresh.authoring.as_ref().unwrap();
        assert_eq!(author.project_id(), project);
        assert_ne!(author.session_id(), old_session);
        // Sequence.revision is serialized author state (not an AuthorSession
        // identity or disk Document revision); public Open preserves it too.
        assert_eq!(
            fresh.active_sequence().unwrap(),
            &saved,
            "complete saved author/opaque/schema/None/Score and revision"
        );
        assert_eq!(author.history().diagnostics().undo_entries, 0);
        assert_eq!(author.history().diagnostics().redo_entries, 0);
        assert_eq!(author.history().diagnostics().pending_entries, 0);
        assert!(!fresh.can_undo_action() && !fresh.can_redo_action());
        assert!(fresh.native_audio_renderer.is_none());
        assert!(fresh.asset_library_handle().is_some());
        fresh.synchronize_playback_authoring_binding().unwrap();
        // This driver consumes the real loaded Session/library. No test setter
        // or cloned Sequence replaces public Open's canonical state.
        run_loaded_surge_playback(
            fresh,
            fresh_output,
            fresh_buffers,
            None,
            false,
            |renderer| {
                assert!(Instant::now() < deadline);
                assert!(!std::ptr::eq(weak.as_ptr(), renderer));
                let current = owner(renderer);
                let current_target = current.editor_target();
                assert_ne!(current_target.root_session, old_target.root_session);
                assert_ne!(current_target.local_session, old_target.local_session);
                assert_ne!(current_target.binding, old_target.binding);
                assert_eq!(current_target.instance, old_target.instance);
                assert!(current_target.continuity_epoch.is_none());
                let current_verifier = renderer.owner_installation_verifier().unwrap();
                let claim = current_verifier.current_claim().unwrap();
                assert!(!current_verifier.same_source(&old_verifier));
                assert!(!current_verifier.same_registration(&old_verifier));
                current_verifier.validate(&claim).unwrap();
                assert!(current_verifier.validate(&old_claim).is_err());
                assert!(old_verifier.validate(&claim).is_err());
                assert!(renderer.observe_native_object_for_test(old_owner).is_err());
                assert!(weak.upgrade().is_none());
                // Native PIDs can be reused and serials are process-local.
                // Fresh Session/Source/claim and the refused old lease are proof.
            },
            Some(deadline),
        );
        assert!(Instant::now() < deadline);
    }));
    // Always attempt both consuming cleanups, even if the first one panics.
    let old_cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shutdown_surge(&mut state, &weak, verifier.as_ref(), handle.as_ref());
    }));
    let fresh_cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(fresh) = &mut reopened {
            let fresh_weak = fresh.native_audio_renderer.clone().unwrap_or_default();
            shutdown_surge(fresh, &fresh_weak, None, None);
        }
    }));
    if let Err(primary) = target {
        std::panic::resume_unwind(primary);
    }
    if let Err(error) = old_cleanup {
        std::panic::resume_unwind(error);
    }
    if let Err(error) = fresh_cleanup {
        std::panic::resume_unwind(error);
    }
}
