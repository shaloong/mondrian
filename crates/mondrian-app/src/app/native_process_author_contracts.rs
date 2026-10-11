// Actual installed Media -> native Process -> App Session -> live Plan/Ack.
// This test uses a controlled software output adapter, not physical DAC proof.
use super::*;

struct ProcessOutput {
    ordinary: NativeHoldPlatformOutput,
    received: Arc<parking_lot::Mutex<Vec<mondrian_media::AudioBuffer>>>,
}

impl mondrian_media::AudioOutputAdapter for ProcessOutput {
    fn poll(&mut self) -> Option<mondrian_media::RealtimeAudioOutputEvent> {
        self.ordinary.poll()
    }
    fn enqueue(&mut self, buffer: &mondrian_media::AudioBuffer)
        -> Result<(), mondrian_media::RealtimeAudioOutputEnqueueError> {
        self.ordinary.enqueue(buffer)?;
        // Test-only PCM evidence, outside all measured allocation/deadline gates.
        let mut received = self.received.lock();
        if received.is_empty() {
            eprintln!("[DEBUG-process-r499] first actual PCM prefix={:?}",
                &buffer.samples[..buffer.samples.len().min(12)]);
        }
        received.push(buffer.clone());
        Ok(())
    }
    fn clear(&mut self) { self.ordinary.clear(); }
    fn validate_deactivation(&self) -> Result<(), mondrian_media::RealtimeAudioOutputControlError> {
        self.ordinary.validate_deactivation()
    }
    fn deactivate(&self) -> Result<Option<mondrian_media::RealtimeAudioOutputQuiescenceToken>,
        mondrian_media::RealtimeAudioOutputControlError> { self.ordinary.deactivate() }
    fn is_quiescent(&self, token: mondrian_media::RealtimeAudioOutputQuiescenceToken)
        -> Result<bool, mondrian_media::RealtimeAudioOutputControlError> {
        self.ordinary.is_quiescent(token)
    }
    fn activate_after_discard(&self, token: mondrian_media::RealtimeAudioOutputQuiescenceToken,
        frames: usize) -> Result<(), mondrian_media::RealtimeAudioOutputControlError> {
        self.ordinary.activate_after_discard(token, frames)
    }
    fn buffered_frames(&self) -> usize { self.ordinary.buffered_frames() }
    fn capacity_frames(&self) -> Option<usize> { self.ordinary.capacity_frames() }
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> { self.ordinary.snapshot() }
}

fn source_fixture() -> (tempfile::TempDir, Arc<AssetLibrary>, Sequence) {
    use std::io::Write;
    let root = tempfile::Builder::new().prefix("native-process-author-音频🙂-")
        .tempdir().expect("source directory");
    let path = root.path().join("constant.wav");
    let frames = 48_000_usize;
    let data_bytes = u32::try_from(frames * 8).expect("bounded WAV");
    let mut wave = std::io::BufWriter::new(std::fs::File::create(&path).expect("create WAV"));
    for bytes in [b"RIFF".as_slice(), &(36 + data_bytes).to_le_bytes(), b"WAVEfmt ",
        &16_u32.to_le_bytes(), &3_u16.to_le_bytes(), &2_u16.to_le_bytes(),
        &48_000_u32.to_le_bytes(), &384_000_u32.to_le_bytes(),
        &8_u16.to_le_bytes(), &32_u16.to_le_bytes(), b"data", &data_bytes.to_le_bytes()] {
        wave.write_all(bytes).expect("WAV header");
    }
    for _ in 0..frames {
        wave.write_all(&0.25_f32.to_le_bytes()).expect("left");
        wave.write_all(&(-0.25_f32).to_le_bytes()).expect("right");
    }
    wave.flush().expect("complete WAV");drop(wave);
    let path = std::fs::canonicalize(path).expect("canonical source");
    let library = AssetLibrary::open(root.path().join("library")).expect("real SQLite library");
    let asset = library.commit_media_probe(
        mondrian_assets::AssetMediaProbeCandidate::new(path.clone(),
            mondrian_core::MediaFileFingerprint::capture(&path),
            mondrian_media::probe_media_info(&path).expect("real source probe"))
            .expect("probe candidate"), None).expect("register actual source");
    let mut sequence = Sequence::new("no editor Process author");
    let track = sequence.audio_tracks[0].id;
    sequence.add_media_audio_clip(track, Clip::new(asset, mondrian_core::TimelineTime::ZERO,
        mondrian_core::TimelineTime::ONE).expect("real one second clip"),
        mondrian_core::AudioSourceComponentId::primary()).expect("audio component");
    (root, library, sequence)
}

fn render_contract() -> mondrian_audio::AudioRenderContract {
    use mondrian_audio::AudioRenderContract as C;
    C {
        sample_rate: 48_000, channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: super::super::super::audio_rendering::MAX_AUDIO_RENDER_BLOCK_FRAMES,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes: C::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames: C::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes: C::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}

#[test]
#[ignore = "requires matching product and owned CLAP Process-author fixture"]
fn native_clap_process_without_editor_commits_exact_time_plan_ack_and_history() {
    use mondrian_audio::{DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver};
    use mondrian_core::{NativeAudioAuthorCallback as Callback, NativeAudioParameterCallbackPhase as Phase};
    use mondrian_media::AudioPcmNativeAuthorHandoffStatus as Status;
    use mondrian_timeline::{AudioChannelStripOwner, AudioChannelStripRack, AudioProcessorRackAddress};
    let helper = std::path::PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .expect("matching actual product"));
    let plugin = std::path::PathBuf::from(std::env::var_os("MONDRIAN_CLAP_PROCESS_AUTHOR_TEST_PLUGIN")
        .expect("owned native producer binary"));
    let installed = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("actual native discovery");
    let instance = installed.create_instance("org.mondrian.fixture.process-author", render_contract(), None)
        .expect("exact pinned native definition and schema");
    assert!(instance.opaque_state.is_none(), "fixture has no optional State extension");
    let id = instance.parameters.keys().next().cloned().expect("actual parameter schema");
    let instance_id = instance.id;
    let resolver = Arc::new(IsolatedAudioProcessorResolver::new(Arc::new(installed)));
    let (root, library, mut sequence) = source_fixture();
    let track_id = sequence.audio_tracks[0].id;
    let rack = AudioProcessorRackAddress::ChannelStrip { owner: AudioChannelStripOwner::Track { track_id },
        rack: AudioChannelStripRack::PreFader };
    sequence.audio_program.track_channels.get_mut(&track_id).expect("strip").strip.pre_fader
        .processors.push(instance);
    let original = sequence.clone();
    let mut state = AppState::with_audio_processor_resolver(resolver.clone());
    state.test_set_sequence(Some(sequence));state.test_set_asset_library(Some(library));
    state.shutdown_audio_playback_and_wait();
    let mut snapshot = audio_snapshot();snapshot.active=false;snapshot.buffered_frames=0;
    snapshot.callback_consumed_frames=0;snapshot.active_callback_consumed_frames=0;
    snapshot.active_duration=None;snapshot.last_callback_playback_delay=Some(Duration::ZERO);
    snapshot.last_callback_playback_delay_uncertainty=Some(Duration::ZERO);
    snapshot.last_callback_age=Some(Duration::ZERO);
    let platform = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
        snapshot, revision:0, acknowledged:0, auto_acknowledge:true, queued:0,
    }));
    let received = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let config = mondrian_media::AudioPlaybackConfig { chunk_frames:64,preroll_frames:64,
        high_watermark_frames:128,max_in_flight:1,..mondrian_media::AudioPlaybackConfig::product_default() };
    state.audio_playback=AppAudioPlayback::Available(Box::new(AudioPlayback::with_output(config,
        Box::new(ProcessOutput { ordinary:NativeHoldPlatformOutput(platform), received:Arc::clone(&received) }))
        .expect("actual serial Media worker/platform boundary")));
    let source=state.audio_playback_renderer("native_process_author_test").expect("canonical actual Runtime");
    state.commit_audio_playback("native_process_author_test",audio_anchor(257),source)
        .expect("real renderer slot/Core installation");
    assert!(state.native_audio_editor.is_none(), "test never opens or synthesizes an editor");
    let captured=state.native_audio_renderer.as_ref().and_then(std::sync::Weak::upgrade)
        .expect("actual current renderer");
    let deadline=Instant::now()+Duration::from_secs(10);
    let mut observed_target=None;let mut accepted_target=None;let mut observed_batches=0;
    let mut acknowledged_at=None;
    let mut diagnostic_events=0_u8;
    loop {
        let poll=state.audio_playback.poll(AudioPlaybackMode::Preroll,audio_anchor(257)).expect("actual render/poll");
        for event in poll.events {
            if diagnostic_events < 16 && matches!(&event,
                AudioPlaybackEvent::NativeOwnerBindingsObserved { .. }
                    | AudioPlaybackEvent::NativeAuthorPending { .. }
                    | AudioPlaybackEvent::NativeObservationPollRefused { .. }
                    | AudioPlaybackEvent::NativeParameterObservationsObserved { .. }) {
                eprintln!("[DEBUG-process-r495] {event:?}");
                diagnostic_events+=1;
            }
            if let AudioPlaybackEvent::NativeAuthorProposalObserved {proposal,..}=&event {
                if observed_target.is_none() {
                    assert_eq!(proposal.callbacks().len(),3);
                    assert!(proposal.state().is_none());assert!(proposal.terminal());
                    let positions=proposal.callbacks().iter().map(|callback| match callback {
                        Callback::ParameterAtExecution {parameter,phase,value_bits,position} => {
                            assert_eq!(parameter,&id);
                            if *phase==Phase::Value {assert_eq!(*value_bits,Some(0.625_f64.to_bits()));}
                            (*phase,position.time)
                        }
                        _=>panic!("actual Process origin was lost"),
                    }).collect::<Vec<_>>();
                    assert_eq!(positions,[(Phase::Begin,mondrian_core::TimelineTime::new(259,48_000).expect("begin")),
                        (Phase::Value,mondrian_core::TimelineTime::new(260,48_000).expect("value")),
                        (Phase::End,mondrian_core::TimelineTime::new(261,48_000).expect("end"))]);
                    observed_target=Some(proposal.target());observed_batches+=1;
                    eprintln!("PROCESS_AUTHOR_NATIVE_PRIMARY: actual bound Process prefix, no editor, exact sample times");
                } else {
                    assert_eq!(Some(proposal.target()),observed_target,"retained retry changed before install/Ack");
                }
                let result=state.handle_audio_playback_event(event,Instant::now());
                let processor=mondrian_timeline::audio_processor_rack(state.active_sequence().expect("Sequence"),&rack)
                    .expect("canonical rack").processors.iter().find(|p|p.id==instance_id).expect("processor");
                let curve=processor.parameters.get(&id).expect("schema").automation.as_ref().expect("recorded curve");
                assert_eq!(curve.keyframes.len(),1,"actual no-editor Process proposal must enter sole Session");
                assert_eq!(curve.keyframes[0].time,mondrian_core::TimelineTime::new(260,48_000).expect("exact key"));
                assert_eq!(curve.keyframes[0].value,0.625);
                result.expect("same Session/Plan author route");
                continue;
            }
            if let AudioPlaybackEvent::NativeAuthorHandoffObserved {status:Status::Acknowledged {target,..},..}=&event {
                accepted_target=Some(**target);acknowledged_at=Some(Instant::now());
            }
            state.handle_audio_playback_event(event,Instant::now()).expect("actual Media completion");
        }
        if acknowledged_at.is_some_and(|at|at.elapsed()>=Duration::from_millis(400))
            && received.lock().len()>=2 {break;}
        assert!(Instant::now()<deadline,
            "real producer/Session/Plan/Ack/resumed PCM did not settle; [DEBUG-process-r495] snapshot={:?} received={} observed={observed_batches}",
            poll.snapshot,received.lock().len());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(observed_batches,1,"same live Plan handoff must not reactivate/reemit native DSP");
    let before=observed_target.expect("actual initial target");let after=accepted_target.expect("actual Ack");
    assert_eq!(before.root_session,after.root_session);assert_eq!(before.local_session,after.local_session);
    assert_eq!(before.continuity_epoch,after.continuity_epoch);assert_ne!(before.binding,after.binding);
    assert!(Arc::ptr_eq(&captured,&state.native_audio_renderer.as_ref().and_then(std::sync::Weak::upgrade).expect("same renderer")));
    let pcm=received.lock();
    assert!(pcm.len()>=2);
    for (buffer_index,buffer) in pcm.iter().enumerate() {
        for (frame,pair) in buffer.samples.chunks_exact(2).enumerate() {
            let expected=if buffer_index==0 && frame<3 {0.25} else {0.15625};
            assert!((pair[0]-expected).abs()<1e-6 && (pair[1]+expected).abs()<1e-6,
                "native input/output and continued Plan PCM mismatch at {buffer_index}/{frame}: {pair:?}");
        }
    }
    drop(pcm);
    assert_eq!(state.authoring_history().expect("history").diagnostics().undo_entries,1);
    let edited=state.active_sequence().expect("edited").clone();
    state.shutdown_audio_playback_and_wait();drop(captured);
    assert!(state.undo_timeline().expect("one native Undo"));
    assert!(state.active_sequence().expect("Undo").author_state_eq_ignoring_revision(&original));
    assert!(state.redo_timeline().expect("one native Redo"));
    assert!(state.active_sequence().expect("Redo").author_state_eq_ignoring_revision(&edited));
    let path=root.path().join("原生Process作者🙂.mdp");state.save_project_file_as(path.clone()).expect("real SQLite/archive save");
    drop(state);
    let mut reopened=AppState::new();reopened.open_project_file(path).expect("fresh current-format reopen");
    assert!(reopened.active_sequence().expect("reopened").author_state_eq_ignoring_revision(&edited));
    assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
}
