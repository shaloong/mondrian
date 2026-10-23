//! First nonzero authored Note window through App preparation and Media enqueue.
//! Software queue PCM only: no physical device, reprime, loop or Export claim.
use super::*;
use mondrian_audio::{
    AudioContinuityEvidenceVisitor, AudioExecutionError, AudioNativeProcessorAddress,
    AudioNativeProcessorOwnerLease, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioSessionCursorEvidence, AudioSessionCursorState,
};
use mondrian_core::{
    AudioNoteId, AudioNoteLaneId, AuthoringList, ExecutionCancellationToken, MusicalTimeReference,
};
use mondrian_media::{
    AudioBuffer, AudioOutputAdapter, AudioPcmContinuity, AudioPcmContinuityModel,
    AudioPcmRenderGeneration, AudioPcmRenderRequest, AudioPcmRenderer,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";

// This is the existing software adapter, with only a bounded copy of its two
// successful enqueue arguments. All clock/control/queue/error behavior delegates.
struct RecordingOutput {
    original: NativeHoldPlatformOutput,
    buffers: Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
    window_limit: usize,
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
        assert!(
            buffers.len() < self.window_limit,
            "only the declared finite four-frame windows may enqueue"
        );
        assert_eq!(buffer.frame_count(), 4);
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

#[derive(Default, Debug, PartialEq, Eq)]
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
        other => panic!("no exact native Note owner: {other:?}"),
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
fn state_bytes() -> Vec<u8> {
    let mut component = b"MNE4".to_vec();
    component.extend_from_slice(&4_u32.to_le_bytes());
    component.extend_from_slice(&0_u32.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 12, 12] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}

#[test]
#[ignore = "current normal Product and existing owned MNE4 DLL; App seek/Media software enqueue, no physical device or recovery qualification"]
fn app_owned_vst3_first_nonzero_note_playback_enqueues_literal_pcm() {
    let material = |name| {
        let path = std::path::PathBuf::from(
            std::env::var_os(name).expect("explicit current native material"),
        );
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = material("MONDRIAN_VST3_TEST_HELPER");
    let plugin = material("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN");
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    assert!(vst3
        .install_plugin(plugin)
        .unwrap()
        .iter()
        .any(|p| { p.class_id == CLASS && p.name == "Mondrian VST3 Note Event Oracle" }));
    let render = mondrian_audio::AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let seed = state_bytes();
    let instance = vst3.create_instance(CLASS, render, Some(seed.clone())).unwrap();
    assert_eq!(
        instance.opaque_state.as_ref().map(AsRef::<[u8]>::as_ref),
        Some(seed.as_slice())
    );
    assert!(instance.parameters.is_empty());
    let id = instance.id;
    let mut sequence = Sequence::new("first nonzero native Note playback");
    sequence.settings.frame_rate = mondrian_core::Rational::FPS_24;
    sequence.settings.audio_sample_rate = 48_000;
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    // A media-free solid only gives the public transport a one-second range.
    // This gate does not execute video or add any audio Clip/carrier source.
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
    // S2000: held, On==S, Off==S and completely past. Velocities are independent
    // dyadics; native Main produces L=v/2 and R=-L/2 with exact paired Offs.
    for (key, on, off, velocity) in [
        (60, 1_990, 2_005, 0.5),
        (61, 2_000, 2_003, 0.25),
        (62, 1_980, 2_000, 0.75),
        (63, 1_995, 1_998, 0.875),
    ] {
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id: id,
                input_port_key: "vst3.note-input.0".into(),
            },
            notes: AuthoringList::from(vec![AudioNote {
                id: AudioNoteId::new(),
                start: TimelineTime::new(on, 48_000).unwrap(),
                end: TimelineTime::new(off, 48_000).unwrap(),
                channel: 3,
                key,
                on_velocity: velocity,
                off_velocity: 0.125,
            }]),
        });
    }
    // Independent complete interleaved Main literals: held .25 plus OnS
    // .125 through2002, then held alone2003/4, exact Off2005 and silence.
    let expected = [
        0.375_f32, -0.1875, 0.375, -0.1875, 0.375, -0.1875, 0.25, -0.125, 0.25, -0.125, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0,
    ];
    run_app_nonzero_note_playback(clap, vst3, sequence, &expected);
}

#[test]
#[ignore = "current normal Product and pinned zero-input CLAP NoteGate DLL; App first-entry software FIFO only, no NativeEnd or recovery qualification"]
fn app_owned_clap_first_nonzero_note_playback_enqueues_literal_pcm() {
    let (clap, vst3, sequence, expected) = clap_note_playback_case();
    run_app_nonzero_note_playback(clap, vst3, sequence, &expected);
}

#[test]
#[ignore = "current normal Product and pinned zero-input CLAP NoteGate DLL; finite same-FIFO NativeEnd author continuation, no device/recovery qualification"]
fn app_owned_clap_native_end_commits_author_and_continues_same_fifo() {
    let (clap, vst3, sequence, expected) = clap_note_playback_case();
    run_app_nonzero_note_playback_with_continuation(
        clap,
        vst3,
        sequence,
        &expected,
        4,
        continue_clap_native_end,
    );
}

fn clap_note_playback_case() -> (
    Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    Sequence,
    [f32; 16],
) {
    const PLUGIN: &str = "org.mondrian.fixture.note-gate.zero-input";
    let material = |name| {
        let path = std::path::PathBuf::from(
            std::env::var_os(name).expect("explicit current native material"),
        );
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = material("MONDRIAN_CLAP_TEST_HELPER");
    let plugin = material("MONDRIAN_ZERO_INPUT_NOTE_GATE_DLL");
    let expected_hash = std::env::var("MONDRIAN_ZERO_INPUT_NOTE_GATE_DLL_SHA256")
        .expect("explicit existing zero-input fixture SHA256");
    assert!(expected_hash.len() == 64 && expected_hash.bytes().all(|b| b.is_ascii_hexdigit()));
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    assert!(clap.install_library(plugin).unwrap().iter().any(|p| {
        p.plugin_id == PLUGIN && p.name == "Mondrian owned zero-input NoteGate oracle"
    }));
    let render = mondrian_audio::AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 4,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut seed = b"MNG1".to_vec();
    seed.extend_from_slice(&0.5_f64.to_le_bytes());
    seed.extend_from_slice(&0_u32.to_le_bytes());
    let mut instance = clap.create_instance(PLUGIN, render, Some(seed.clone())).unwrap();
    let mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
        plugin_id,
        binary_sha256: Some(hash),
        ..
    } = &instance.definition
    else {
        panic!("the selected real CLAP definition must retain its binary identity");
    };
    assert_eq!(plugin_id, PLUGIN);
    let actual_hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(actual_hash, expected_hash.to_ascii_lowercase());
    assert_eq!(
        instance.opaque_state.as_ref().map(AsRef::<[u8]>::as_ref),
        Some(seed.as_slice())
    );
    // The native catalog also has read-only Exact Offs23; it is telemetry,
    // not an author parameter lane. Only editable Gain17 and Scenario19 persist.
    assert_eq!(instance.parameters.len(), 2);
    for id in ["clap.param.17", "clap.param.19"] {
        assert!(instance.parameters.contains_key(&mondrian_core::ParameterId::new_static(id)));
    }
    assert!(!instance
        .parameters
        .contains_key(&mondrian_core::ParameterId::new_static("clap.param.23")));
    let gain = mondrian_core::ParameterId::new_static("clap.param.17");
    instance
        .set_parameter_automation(
            mondrian_core::ExactAutomationCurve::new(gain.clone(), 0.5).unwrap(),
        )
        .unwrap();
    let curve = instance.parameters[&gain].automation.as_ref().unwrap();
    assert_eq!(curve.default_value.to_bits(), 0.5_f64.to_bits());
    assert!(curve.keyframes.is_empty());
    assert!(instance
        .parameters
        .iter()
        .all(|(id, row)| id == &gain || row.automation.is_none()));
    let id = instance.id;
    let mut sequence = Sequence::new("first nonzero zero-input CLAP Note playback");
    sequence.settings.frame_rate = mondrian_core::Rational::FPS_24;
    sequence.settings.audio_sample_rate = 48_000;
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
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
    // The second CLAP port's native ID is 0x80000017, not ordinal1's ID1.
    // With S2000: held, On==S, Off==S and completely past. Zero velocity is
    // a real held voice in this fixed independent native oracle.
    for (port, key, on, off, velocity) in [
        (0_u32, 60, 1_997, 2_005, 0.0),
        (0x8000_0017, 61, 2_000, 2_007, 1.0),
        (0, 62, 1_996, 2_000, 1.0),
        (0x8000_0017, 63, 1_998, 1_999, 0.0),
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
                start: TimelineTime::new(on, 48_000).unwrap(),
                end: TimelineTime::new(off, 48_000).unwrap(),
                channel: 0,
                key,
                on_velocity: velocity,
                off_velocity: 0.25,
            }]),
        });
    }
    // App delivery uses the standard unity Mono-to-Stereo matrix: complete
    // interleaved literals are 5/64 for2000..2005, then19/256 and9/128 on L/R.
    // The first NativeEnd is at2008, outside these two software windows.
    let expected = [
        0.078125_f32,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.078125,
        0.07421875,
        0.07421875,
        0.0703125,
        0.0703125,
    ];
    (clap, vst3, sequence, expected)
}

fn run_app_nonzero_note_playback(
    clap: Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    vst3: Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    sequence: Sequence,
    expected: &[f32],
) {
    run_app_nonzero_note_playback_with_continuation(
        clap,
        vst3,
        sequence,
        expected,
        2,
        |_, _, _, _, _, _| {},
    );
}

fn run_app_nonzero_note_playback_with_continuation(
    clap: Arc<mondrian_audio::InstalledClapAudioProcessorSpecResolver>,
    vst3: Arc<mondrian_audio::InstalledVst3AudioProcessorSpecResolver>,
    sequence: Sequence,
    expected: &[f32],
    window_limit: usize,
    continuation: impl FnOnce(
        &mut AppState,
        &Arc<TimelineAudioPcmRenderer>,
        &Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
        &Arc<parking_lot::Mutex<NativeHoldPlatformState>>,
        mondrian_audio::NativeAudioObjectObservation,
        Instant,
    ),
) {
    assert!(matches!(window_limit, 2 | 4));
    let program_layout = sequence.settings.audio_channel_layout;
    // Program/native layout is independent of App's fixed monitoring target.
    let channel_layout = crate::app::AUDIO_OUTPUT_LAYOUT;
    assert_eq!(sequence.settings.audio_sample_rate, 48_000);
    assert_eq!(
        sequence.settings.frame_rate,
        mondrian_core::Rational::FPS_24
    );
    assert_eq!(expected.len(), 8 * channel_layout.channel_count());
    let config = mondrian_media::AudioPlaybackConfig {
        sample_rate: 48_000,
        channel_layout,
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 4,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    assert!(sequence.audio_tracks.iter().all(|track| track.clips.is_empty()));
    let directory = tempfile::tempdir().unwrap();
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let mut state = AppState::with_native_audio_catalogs(clap, vst3);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let mut snapshot = audio_snapshot();
    snapshot.contract.channel_layout = channel_layout;
    snapshot.contract.channel_semantics = match channel_layout {
        AudioChannelLayout::Mono => mondrian_media::RealtimeAudioChannelSemantics::MonoConvention,
        AudioChannelLayout::Stereo => {
            mondrian_media::RealtimeAudioChannelSemantics::StereoConvention
        }
        other => panic!("undeclared software Note layout: {other:?}"),
    };
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
    let buffers = Arc::new(parking_lot::Mutex::new(Vec::with_capacity(window_limit)));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            config,
            Box::new(RecordingOutput {
                original: NativeHoldPlatformOutput(output.clone()),
                buffers: buffers.clone(),
                window_limit,
            }),
        )
        .unwrap(),
    ));
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library.clone()));
    state.synchronize_playback_authoring_binding().unwrap();
    assert!(state.authoring_session_id().is_some());
    let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    let revision = library.database_revision().unwrap();
    let mut weak = std::sync::Weak::<TimelineAudioPcmRenderer>::new();
    let mut verifier = None;
    let mut handle: Option<OwnedHandle> = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.seek(1).expect("public frame1 seek at24fps");
        assert_eq!(state.current_frame(), 1);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 2_000);
        assert!(state.native_audio_renderer.is_none());
        let source = state.audio_playback_renderer("nonzero Note prepare").unwrap().unwrap();
        let delivery = source.renderer.delivery_evidence();
        assert_eq!(delivery.program_layout, program_layout);
        assert_eq!(delivery.target_layout, channel_layout);
        weak = Arc::downgrade(&source.renderer);
        assert!(source.renderer.owner_installation_verifier().is_none());
        let before = cursors(&source.renderer);
        assert_eq!(before.sessions[0].state, AudioSessionCursorState::Unentered);
        assert_eq!(
            before.processors[0].state,
            AudioProcessorCursorState::Unentered
        );
        // Negative only: direct adapter entry cannot bypass the real Media slot.
        let error = source
            .renderer
            .render(
                AudioPcmRenderRequest {
                    start_sample: 2_000,
                    frame_count: 4,
                    sample_rate: 48_000,
                    channel_layout,
                    continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
                },
                &ExecutionCancellationToken::new(),
            )
            .unwrap_err();
        let mondrian_core::MondrianError::WorkflowStepFailed { step_id, reason } = error else {
            panic!("missing Source must preserve the original typed adapter failure");
        };
        assert_eq!(step_id, "timeline_audio_state_entry");
        assert_eq!(
            reason,
            AudioExecutionError::ProcessorHost(
                mondrian_audio::AudioProcessorHostError::NoteExecutionRefused(
                    "Note score execution requires the installed root owner"
                )
            )
            .to_string()
        );
        assert_eq!(cursors(&source.renderer), before);
        assert!(buffers.lock().is_empty());
        // Baseline must fail here with the original FreshEntryOriginMismatch,
        // not at a metadata assertion or a fabricated alternate preparation.
        state
            .commit_audio_playback(
                "first nonzero Note Media prepare",
                audio_anchor(2_000),
                Some(source),
            )
            .expect("FIRST_NONZERO_NOTE_MEDIA_PREPARE: original Media prepare must admit S2000");
        assert_eq!(
            weak.upgrade().unwrap().continuity_model(),
            AudioPcmContinuityModel::GenerationState
        );
        let generation = state.audio_playback_snapshot().generation;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        while !ready {
            let poll =
                state.audio_playback.poll(AudioPlaybackMode::Idle, audio_anchor(2_000)).unwrap();
            assert_eq!(poll.snapshot.generation, generation);
            assert_eq!(poll.snapshot.in_flight, 0);
            assert_eq!(poll.snapshot.next_start_sample, 2_000);
            for event in poll.events {
                match &event {
                    AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } => {
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                        );
                        ready = true;
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                    | AudioPlaybackEvent::NativeParameterObservationsObserved { .. } => {}
                    other => panic!("unexpected pre-Enter work: {other:?}"),
                }
                state.handle_audio_playback_event(event, Instant::now()).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "original Media native binding did not settle"
            );
            std::thread::yield_now();
        }
        let renderer = weak.upgrade().unwrap();
        let installed = renderer.owner_installation_verifier().unwrap();
        let claim = installed.current_claim().unwrap();
        installed.validate(&claim).unwrap();
        verifier = Some(installed.clone());
        renderer
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        let cold = owner(&renderer);
        assert!(cold.editor_target().continuity_epoch.is_none());
        let object = renderer.observe_native_object_for_test(cold).unwrap();
        let pid = object.process_id().get();
        assert_ne!(pid, std::process::id());
        // SAFETY: this exact installed object supplied PID; query/synchronize only.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        assert!(!raw.is_null());
        handle = Some(unsafe { OwnedHandle::from_raw_handle(raw.cast()) });
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_ref().unwrap().as_raw_handle().cast(), 0) },
            258
        );
        assert_eq!(cursors(&renderer), before);
        assert_eq!(
            installed.current_claim().unwrap().generation(),
            claim.generation()
        );
        installed.validate(&claim).unwrap();
        drop(renderer);
        // Two original Media Preroll frontiers; no callback consumption, second
        // Enter, Idle/Suspend or direct positive renderer call between windows.
        for (frontier, anchor) in [2_000, 2_004].into_iter().enumerate() {
            while buffers.lock().len() < frontier + 1
                || state.audio_playback_snapshot().in_flight != 0
            {
                let poll = state
                    .audio_playback
                    .poll(AudioPlaybackMode::Preroll, audio_anchor(anchor))
                    .unwrap();
                assert_eq!(poll.snapshot.generation, generation);
                assert!(poll.snapshot.next_start_sample <= anchor + 4);
                assert!(!poll.snapshot.output.unwrap().active);
                for event in poll.events {
                    assert!(
                        matches!(
                            &event,
                            AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                                | AudioPlaybackEvent::NativeParameterObservationsObserved { .. }
                                | AudioPlaybackEvent::NativeOwnerBindingsObserved { .. }
                        ),
                        "unexpected PCM/author/device event: {event:?}"
                    );
                    state.handle_audio_playback_event(event, Instant::now()).unwrap();
                }
                assert!(
                    Instant::now() < deadline,
                    "two real PCM windows did not enqueue"
                );
                std::thread::yield_now();
            }
            assert_eq!(buffers.lock().len(), frontier + 1);
            assert_eq!(output.lock().queued, (frontier + 1) * 4);
            let renderer = weak.upgrade().unwrap();
            let current = owner(&renderer);
            assert_eq!(current.editor_target().continuity_epoch, Some(generation));
            assert_ne!(current.binding(), cold.binding());
            assert_eq!(
                renderer.observe_native_object_for_test(current).unwrap(),
                object
            );
            assert!(renderer.observe_native_object_for_test(cold).is_err());
            assert_eq!(
                installed.current_claim().unwrap().generation(),
                claim.generation()
            );
            installed.validate(&claim).unwrap();
            let facts = cursors(&renderer);
            assert!(matches!(facts.sessions[0].state,
                AudioSessionCursorState::Active { epoch, next_public_sample, next_execution_sample, .. }
                if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                    && next_public_sample.sample() == anchor + 4 && next_execution_sample.sample() == anchor + 4));
            assert!(matches!(facts.processors[0].state,
                AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                if epoch == mondrian_audio::AudioContinuityEpoch::new(generation) && next_input_signal_sample.sample() == anchor + 4));
        }
        let renderer = weak.upgrade().unwrap();
        let before = cursors(&renderer);
        let error = renderer
            .render(
                AudioPcmRenderRequest {
                    start_sample: 2_008,
                    frame_count: 4,
                    sample_rate: 48_000,
                    channel_layout,
                    continuity: AudioPcmContinuity::Continue(AudioPcmRenderGeneration::new(
                        generation + 1,
                    )),
                },
                &ExecutionCancellationToken::new(),
            )
            .unwrap_err();
        let mondrian_core::MondrianError::WorkflowStepFailed { step_id, reason } = error else {
            panic!("wrong generation must preserve the exact adapter failure");
        };
        assert_eq!(step_id, "timeline_audio_continuity");
        assert_eq!(
            reason,
            format!(
                "render generation {} continued without a matching entry",
                generation + 1
            )
        );
        assert_eq!(cursors(&renderer), before);
        assert_eq!(
            installed.current_claim().unwrap().generation(),
            claim.generation()
        );
        installed.validate(&claim).unwrap();
        renderer
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        drop(renderer);
        let recorded = buffers.lock();
        assert_eq!(recorded.len(), 2);
        for buffer in recorded.iter() {
            assert_eq!(buffer.sample_rate, 48_000);
            assert_eq!(buffer.channel_layout, channel_layout);
            assert!(buffer.samples.iter().all(|value| value.is_finite()));
        }
        assert_eq!(
            recorded
                .iter()
                .flat_map(|buffer| buffer.samples.iter())
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            expected.iter().copied().map(f32::to_bits).collect::<Vec<_>>()
        );
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 2_008);
        assert_eq!(library.database_revision().unwrap(), revision);
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert!(!state.authoring.as_ref().unwrap().has_pending_native_authoring());
        assert!(state.native_audio_editor.is_none());
        drop(recorded);
        let renderer = weak.upgrade().unwrap();
        // The old two-window oracle and every refusal above must pass first.
        continuation(&mut state, &renderer, &buffers, &output, object, deadline);
        assert_eq!(library.database_revision().unwrap(), revision);
    }));
    // Original Media consuming shutdown releases this sole renderer/Runtime,
    // including a baseline prepare refusal. Do this before restoring target panic.
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
    }));
    if let Err(primary) = target {
        std::panic::resume_unwind(primary);
    }
    if let Err(error) = cleanup {
        std::panic::resume_unwind(error);
    }
}

fn continue_clap_native_end(
    state: &mut AppState,
    renderer: &Arc<TimelineAudioPcmRenderer>,
    buffers: &Arc<parking_lot::Mutex<Vec<AudioBuffer>>>,
    output: &Arc<parking_lot::Mutex<NativeHoldPlatformState>>,
    object: mondrian_audio::NativeAudioObjectObservation,
    deadline: Instant,
) {
    use mondrian_core::{
        AuthoringTimeDomain, NativeAudioAuthorCallback as Callback, NativeAudioAuthorProposal,
        NativeAudioParameterCallbackPhase as Phase, NativeAudioParameterObservationOrigin,
        ParameterId,
    };
    use mondrian_media::AudioPcmNativeAuthorTerminalBatchStatus as Status;

    let original = state.active_sequence().unwrap().clone();
    let sequence_id = original.id;
    let track = original.audio_tracks[0].id;
    let original_processor =
        &original.audio_program.track_channels[&track].strip.pre_fader.processors[0];
    let gain = ParameterId::new_static("clap.param.17");
    let offs = ParameterId::new_static("clap.param.23");
    let author = state.authoring.as_ref().unwrap();
    let author_session = author.session_id();
    let author_generation = author.author_generation();
    let history = author.history().diagnostics();
    let generation = state.audio_playback_snapshot().generation;
    let entered = owner(renderer);
    let initial_target = entered.editor_target();
    assert_eq!(initial_target.continuity_epoch, Some(generation));
    let installed = renderer.owner_installation_verifier().unwrap();
    let initial_claim = installed.current_claim().unwrap();
    installed.validate(&initial_claim).unwrap();
    let weak = Arc::downgrade(renderer);
    let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
    let first_pcm: Vec<_> = buffers
        .lock()
        .iter()
        .flat_map(|buffer| buffer.samples.iter().map(|value| value.to_bits()))
        .collect();
    assert_eq!(first_pcm.len(), 16);

    let mut renderer_id = None;
    let mut pending: Option<NativeAudioAuthorProposal> = None;
    let mut prepared_key = None;
    let mut prepared = 0_usize;
    let mut acknowledged = 0_usize;
    let expected: [[u32; 8]; 2] = [
        [
            0x3d50_0000,
            0x3d50_0000,
            0x3d00_0000,
            0x3d00_0000,
            0x3c80_0000,
            0x3c80_0000,
            0,
            0,
        ],
        [0; 8],
    ];
    // New binding forces one post-Ack MainQuery. Unchanged2012 may deduplicate
    // that query; retain its authenticated target and require it still current.
    let mut observed_current_offs = None;
    // Do not submit2012 until2008's real author/CAS/Ack/SourceClosed completes.
    for (window, anchor) in [2_008, 2_012].into_iter().enumerate() {
        loop {
            assert!(
                Instant::now() < deadline,
                "native End author continuation did not close"
            );
            assert!(state.audio_playback.matches_source_weak(&erased));
            assert!(state
                .native_audio_renderer
                .as_ref()
                .is_some_and(|current| std::sync::Weak::ptr_eq(current, &weak)));
            assert!(state.native_audio_editor.is_none());
            let poll = state
                .audio_playback
                .poll(AudioPlaybackMode::Preroll, audio_anchor(anchor))
                .unwrap();
            assert_eq!(poll.snapshot.generation, generation);
            assert!(poll.snapshot.next_start_sample <= anchor + 4);
            assert!(!poll.snapshot.output.unwrap().active);
            for event in poll.events {
                let (actual, observed) = match &event {
                    AudioPlaybackEvent::NativeDiagnosticsObserved {
                        renderer_id,
                        generation,
                        ..
                    }
                    | AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id,
                        generation,
                        ..
                    }
                    | AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        renderer_id,
                        generation,
                        ..
                    }
                    | AudioPlaybackEvent::NativeAuthorProposalObserved {
                        renderer_id,
                        generation,
                        ..
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
                    } => (*renderer_id, *generation),
                    other => panic!("unexpected continuation event: {other:?}"),
                };
                assert_eq!(observed, generation);
                assert!(renderer_id.replace(actual).is_none_or(|prior| prior == actual));
                match &event {
                    AudioPlaybackEvent::NativeDiagnosticsObserved { batch, .. } => {
                        assert!(!batch.loss().incomplete());
                    }
                    AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } => {
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                        );
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        observation, ..
                    } => {
                        assert_eq!(observation.process_id(), object.process_id());
                        assert_eq!(observation.object_serial(), object.object_serial());
                        let target = observation.target();
                        assert_eq!(target.instance, initial_target.instance);
                        assert_eq!(target.root_session, initial_target.root_session);
                        assert_eq!(target.local_session, initial_target.local_session);
                        assert_eq!(target.continuity_epoch, Some(generation));
                        for fact in observation.facts().iter().filter(|fact| fact.parameter == offs)
                        {
                            // Native read-only23 remains a fact, never an author lane.
                            assert!(fact.author_value_bits.is_none());
                            if fact.value_bits == Some(2.0_f64.to_bits())
                                && matches!(
                                    fact.origin,
                                    NativeAudioParameterObservationOrigin::MainQuery { .. }
                                )
                            {
                                observed_current_offs = Some(target);
                            }
                        }
                    }
                    AudioPlaybackEvent::NativeAuthorProposalObserved { proposal, .. } => {
                        assert_eq!(proposal.sequence(), sequence_id);
                        assert_eq!(proposal.target(), initial_target);
                        assert!(proposal.terminal());
                        if let Some(previous) = &pending {
                            assert!(previous.same_retained_proposal(proposal));
                        } else {
                            assert_eq!(prepared, 0);
                            assert_eq!(proposal.callbacks().len(), 4);
                            // NOTE_END header time is ignored; only these actual
                            // parameter execution positions are author clocks.
                            for (callback, sample) in
                                proposal.callbacks().iter().zip([2_008, 2_008, 2_010, 2_010])
                            {
                                let Callback::ParameterAtExecution {
                                    parameter,
                                    phase,
                                    value_bits,
                                    position,
                                } = callback
                                else {
                                    panic!("NativeEnd must retain the actual timed Gain outputs");
                                };
                                assert_eq!(*parameter, gain);
                                assert_eq!(*phase, Phase::Value);
                                assert_eq!(*value_bits, Some(0.5_f64.to_bits()));
                                assert_eq!(
                                    position.domain,
                                    AuthoringTimeDomain::Sequence(sequence_id)
                                );
                                assert_eq!(
                                    position.time,
                                    TimelineTime::new(sample, 48_000).unwrap()
                                );
                            }
                            assert_eq!(
                                proposal.state().map(AsRef::<[u8]>::as_ref),
                                original_processor.opaque_state.as_ref().map(AsRef::<[u8]>::as_ref)
                            );
                            pending = Some(proposal.as_ref().clone());
                        }
                    }
                    AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared {
                        proposal, key, ..
                    } => {
                        assert!(pending.as_ref().unwrap().same_retained_proposal(proposal));
                        assert!(state.authoring.as_ref().unwrap().has_pending_native_authoring());
                        prepared += 1;
                        assert_eq!(prepared, 1);
                        assert!(prepared_key.replace(key.clone()).is_none());
                    }
                    AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                        proposal,
                        key,
                        status,
                        ..
                    } => {
                        assert_eq!(prepared, 1);
                        assert!(pending.as_ref().unwrap().same_retained_proposal(proposal));
                        assert_eq!(Some(key), prepared_key.as_ref());
                        match status {
                            Status::Acknowledged { target }
                            | Status::InstalledAwaitingAcknowledgement { target } => {
                                let current = owner(renderer);
                                assert_eq!(current.editor_target(), *target);
                                assert_eq!(target.instance, initial_target.instance);
                                assert_eq!(target.root_session, initial_target.root_session);
                                assert_eq!(target.local_session, initial_target.local_session);
                                assert_eq!(target.continuity_epoch, Some(generation));
                                assert_ne!(target.binding, initial_target.binding);
                                assert_ne!(target.root_plan, initial_target.root_plan);
                                let current_verifier =
                                    renderer.owner_installation_verifier().unwrap();
                                let claim = current_verifier.current_claim().unwrap();
                                current_verifier.validate(&claim).unwrap();
                                assert_ne!(claim.generation(), initial_claim.generation());
                                assert_eq!(target.installed_generation, claim.generation());
                                assert!(installed.validate(&initial_claim).is_err());
                                if matches!(status, Status::Acknowledged { .. }) {
                                    acknowledged += 1;
                                    assert_eq!(acknowledged, 1, "one real complete native Ack");
                                }
                            }
                            Status::Refused(reason) => {
                                panic!("actual terminal author refused: {reason:?}")
                            }
                        }
                    }
                    _ => unreachable!(),
                }
                // Only the original App dispatcher publishes canonical state,
                // queues Source CAS and accepts the producer's actual Ack.
                state.handle_audio_playback_event(event, Instant::now()).unwrap();
                let author = state.authoring.as_ref().unwrap();
                assert_eq!(author.session_id(), author_session);
                if prepared != 0 {
                    assert_eq!(
                        author.author_generation().get(),
                        author_generation.get() + 1
                    );
                } else {
                    assert_eq!(author.author_generation(), author_generation);
                }
                if acknowledged != 0 {
                    assert!(!author.has_pending_native_authoring());
                }
            }
            assert!(buffers.lock().len() <= window + 3);
            if buffers.lock().len() == window + 3
                && state.audio_playback_snapshot().in_flight == 0
                && acknowledged == 1
            {
                let current = owner(renderer);
                if observed_current_offs == Some(current.editor_target()) {
                    assert!(renderer.observe_native_object_for_test(entered).is_err());
                    renderer
                        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                        .unwrap();
                    break;
                }
            }
            std::thread::yield_now();
        }
        assert_eq!(
            output.lock().queued,
            (window + 3) * 4,
            "original FIFO must retain every prior frame"
        );
        let recorded = buffers.lock();
        let buffer = &recorded[window + 2];
        assert_eq!(buffer.sample_rate, 48_000);
        assert_eq!(buffer.channel_layout, crate::app::AUDIO_OUTPUT_LAYOUT);
        assert_eq!(buffer.frame_count(), 4);
        assert_eq!(
            buffer.samples.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            expected[window]
        );
        assert_eq!(
            recorded[..2]
                .iter()
                .flat_map(|buffer| buffer.samples.iter().map(|value| value.to_bits()))
                .collect::<Vec<_>>(),
            first_pcm
        );
        drop(recorded);
        let facts = cursors(renderer);
        assert_eq!(
            facts.sessions[0].identity.session_id,
            initial_target.root_session
        );
        assert_eq!(
            facts.processors[0].identity.session_id,
            initial_target.local_session
        );
        assert!(matches!(facts.sessions[0].state,
            AudioSessionCursorState::Active { epoch, next_public_sample, next_execution_sample, .. }
            if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                && next_public_sample.sample() == anchor + 4 && next_execution_sample.sample() == anchor + 4));
        assert!(matches!(facts.processors[0].state,
            AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
            if epoch == mondrian_audio::AudioContinuityEpoch::new(generation)
                && next_input_signal_sample.sample() == anchor + 4));
    }
    assert!(pending.is_some());
    assert_eq!((prepared, acknowledged), (1, 1));
    assert_eq!(buffers.lock().len(), 4);
    assert_eq!(state.audio_playback_snapshot().next_start_sample, 2_016);
    let author = state.authoring.as_ref().unwrap();
    assert!(!author.has_pending_native_authoring());
    let after = author.history().diagnostics();
    assert_eq!(after.undo_entries, history.undo_entries + 1);
    assert_eq!(after.redo_entries, 0);
    assert_eq!(after.pending_entries, 0);
    assert_eq!(after.budget_evicted_entries, history.budget_evicted_entries);
    assert_eq!(
        after.barrier_discarded_entries,
        history.barrier_discarded_entries
    );
    assert_eq!(
        after.retention_disabled_entries,
        history.retention_disabled_entries
    );
    assert_eq!(
        after.oversize_dropped_entries,
        history.oversize_dropped_entries
    );
    let mut restored = state.active_sequence().unwrap().clone();
    // The one canonical publication must advance Session-owned revision once.
    assert_eq!(restored.revision, original.revision.checked_next().unwrap());
    let processor = &mut restored
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(processor.parameters.len(), 2);
    assert!(!processor.parameters.contains_key(&offs));
    let curve = processor.parameters[&gain].automation.as_ref().unwrap();
    assert_eq!(curve.default_value.to_bits(), 0.5_f64.to_bits());
    assert_eq!(
        curve
            .keyframes
            .iter()
            .map(|key| (key.time, key.value.to_bits()))
            .collect::<Vec<_>>(),
        [
            (TimelineTime::new(2_008, 48_000).unwrap(), 0.5_f64.to_bits()),
            (TimelineTime::new(2_010, 48_000).unwrap(), 0.5_f64.to_bits()),
        ]
    );
    processor.parameters.get_mut(&gain).unwrap().automation =
        original_processor.parameters[&gain].automation.clone();
    assert!(restored.author_state_eq_ignoring_revision(&original),
        "apart from the checked revision increment, only the two actual timed Gain keys may change canonical author state; the complete score and IDs stay intact");
}
