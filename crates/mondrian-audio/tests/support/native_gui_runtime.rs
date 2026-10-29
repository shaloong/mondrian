//! Shared public Runtime/Graph GUI acceptance; format policy stays in each case.
#[cfg(target_os = "linux")]
#[path = "native_gui_x11.rs"]
mod native_window;
use super::support::{sample, Census, Signal};
use mondrian_audio::{
    AudioContinuityEpoch, AudioNativeProcessorAddress, AudioProcessorInsertionPoint,
    AudioProcessorOccurrence, AudioProcessorOccurrenceOwner, AudioProcessorResolver,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant,
    AudioSessionCursorIdentity, AudioStateEntry,
};
use mondrian_core::{
    AssetId, AudioOwnerInstallationSource, AudioSourceComponentId, ExecutionCancellationToken,
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    NativeAudioEditorInsertion, NativeAudioEditorLocator, NativeAudioEditorOccurrenceOwner,
    NativeAudioEditorTarget, NativeAudioEditorTitle, NativeChildSettlementRegistrar, Rational,
    TimelineTime,
};
use mondrian_timeline::{audio::AudioProcessorInstance, Clip, Sequence};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

fn address(target: NativeAudioEditorTarget) -> AudioNativeProcessorAddress {
    let NativeAudioEditorOccurrenceOwner::Output(output) = target.owner else {
        panic!("selected output occurrence");
    };
    assert_eq!(target.insertion, NativeAudioEditorInsertion::PreFader);
    AudioNativeProcessorAddress {
        root: AudioSessionCursorIdentity {
            prepared_plan_id: target.root_plan,
            session_id: target.root_session,
        },
        local: AudioSessionCursorIdentity {
            prepared_plan_id: target.local_plan,
            session_id: target.local_session,
        },
        occurrence: AudioProcessorOccurrence {
            instance_id: target.instance,
            owner: AudioProcessorOccurrenceOwner::Output(output),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
        epoch: target.continuity_epoch.map(AudioContinuityEpoch::new),
    }
}

pub fn effect_sequence(
    author: AudioProcessorInstance,
    render: AudioRenderContract,
) -> (Sequence, AssetId, NativeAudioEditorLocator) {
    let instance = author.id;
    let asset = AssetId::new();
    let mut sequence = Sequence::new("Official GUI Runtime");
    sequence.settings.frame_rate = Rational::FPS_24;
    sequence.settings.audio_channel_layout = render.channel_layout;
    sequence.settings.audio_sample_rate = render.sample_rate;
    for channel in sequence.audio_program.track_channels.values_mut() {
        channel.strip.input_trim_db = 0.0;
        channel.strip.fader_db = 0.0;
        channel.strip.fader_automation = None;
    }
    for output in sequence.audio_program.outputs.iter_mut() {
        output.strip.input_trim_db = 0.0;
        output.strip.fader_db = 0.0;
        output.strip.fader_automation = None;
    }
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(asset, TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    let output = sequence.audio_program.outputs[0].id;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(author);
    let serialized = serde_json::to_vec(&sequence).unwrap();
    let sequence: Sequence = serde_json::from_slice(&serialized).unwrap();
    let locator = NativeAudioEditorLocator {
        sequence: sequence.id,
        instance,
        owner: NativeAudioEditorOccurrenceOwner::Output(output),
        insertion: NativeAudioEditorInsertion::PreFader,
    };
    (sequence, asset, locator)
}

pub fn exercise_gui_runtime(
    sequence: &Sequence,
    asset: AssetId,
    resolver: &dyn AudioProcessorResolver,
    render: AudioRenderContract,
    locator: NativeAudioEditorLocator,
    gain: f32,
    resizable: bool,
) {
    #[cfg(not(target_os = "linux"))]
    let _ = resizable;
    let before = serde_json::to_value(sequence).unwrap();
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        sequence,
        &[],
        &Signal {
            asset,
            layout: render.channel_layout,
            rate: render.sample_rate,
        },
        resolver,
        render,
        None,
        AudioRuntimeResourceGrant::new(1, 96 * 1024 * 1024, 48 * 1024 * 1024),
    )
    .unwrap();
    let census = Arc::new(Census::default());
    let registrar = NativeChildSettlementRegistrar::try_new(census.clone()).unwrap();
    runtime.bind_native_child_settlement(&registrar).unwrap();
    let installation = AudioOwnerInstallationSource::new();
    let (owner, verifier) = installation.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = installation
        .reserve_install(&owner, None)
        .and_then(|r| installation.begin_commit(r))
        .unwrap()
        .commit()
        .unwrap();
    let cancel = ExecutionCancellationToken::new();
    assert_eq!(
        runtime.install_native_owner_bindings(&claim, &cancel).unwrap(),
        1
    );
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let mut position = 0;
    let mut check_pcm = |runtime: &mut AudioProgramRuntime, frames| {
        let mut pcm = vec![f32::NAN; frames * render.channel_layout.channel_count()];
        runtime
            .render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: position,
                    frames,
                },
                &mut pcm,
            )
            .unwrap();
        for (frame, values) in pcm.chunks_exact(render.channel_layout.channel_count()).enumerate() {
            for (channel, actual) in values.iter().enumerate() {
                let expected = sample(position + frame as i64, channel) * gain;
                assert!(
                    actual.is_finite() && (*actual - expected).abs() <= 2e-6,
                    "frame{frame} channel{channel}: {actual} vs {expected}"
                );
            }
        }
        position += frames as i64;
    };
    check_pcm(&mut runtime, 17);
    #[cfg(target_os = "linux")]
    let probe = native_window::Probe::connect();
    let mut same_object = None;
    for cycle in 0..3 {
        let title = format!("Mondrian GUI reference {} {cycle}", locator.instance);
        let outcome = runtime
            .control_native_editor(
                Intent::ResolveAndOpen {
                    locator,
                    title: NativeAudioEditorTitle::new(title.clone()).unwrap(),
                },
                &cancel,
            )
            .unwrap();
        let Outcome::Opened { target, width, height } = outcome else {
            panic!("actual foreign view must open: {outcome:?}")
        };
        assert!(width > 0 && height > 0);
        let lease = runtime.native_owner_lease(address(target)).unwrap();
        let actual = runtime.observe_native_owner_object(lease).unwrap();
        assert_ne!(actual.process_id().get(), std::process::id());
        assert_eq!(*same_object.get_or_insert(actual), actual);
        #[cfg(target_os = "linux")]
        let window = {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                runtime.poll_native_observations(&cancel).unwrap();
                if let Some(window) = probe.find(&title) {
                    assert_eq!(probe.size(window), (width, height));
                    if probe.foreign_view_is_painted(window) {
                        break window;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "real foreign child must paint through native main service: {:?}",
                    probe.describe_windows()
                );
                std::thread::yield_now();
            }
        };
        for frames in [31, 64, 23] {
            check_pcm(&mut runtime, frames);
        }
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), actual);
        #[cfg(target_os = "linux")]
        {
            // Stimulus derives from actual opened bounds, not plugin defaults.
            probe.request_resize(window, width + 17, height + 9);
            let deadline = Instant::now() + Duration::from_secs(3);
            if resizable {
                loop {
                    runtime.poll_native_observations(&cancel).unwrap();
                    let outcome = runtime
                        .control_native_editor(
                            Intent::ResolveAndOpen {
                                locator,
                                title: NativeAudioEditorTitle::new(title.clone()).unwrap(),
                            },
                            &cancel,
                        )
                        .unwrap();
                    let Outcome::Opened {
                        target: retained,
                        width: accepted_width,
                        height: accepted_height,
                    } = outcome
                    else {
                        panic!("existing resized editor must remain open: {outcome:?}")
                    };
                    assert_eq!(retained, target);
                    let accepted = (accepted_width, accepted_height);
                    if accepted != (width, height)
                        && probe.size(window) == accepted
                        && probe.foreign_view_fills_parent(window)
                        && probe.foreign_view_is_painted(window)
                    {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "negotiated size must reach the actual native child"
                    );
                    std::thread::yield_now();
                }
                check_pcm(&mut runtime, 11);
            } else {
                while probe.size(window) != (width, height) {
                    runtime.poll_native_observations(&cancel).unwrap();
                    assert!(
                        Instant::now() < deadline,
                        "fixed native view bounds must be honored"
                    );
                }
            }
            assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), actual);
            if cycle == 2 {
                probe.request_close(window);
                let deadline = Instant::now() + Duration::from_secs(3);
                while probe.find(&title).is_some() {
                    runtime.poll_native_observations(&cancel).unwrap();
                    assert!(
                        Instant::now() < deadline,
                        "native user close must finish actual view removal"
                    );
                }
            }
        }
        assert!(
            matches!(runtime.control_native_editor(Intent::Close { target }, &cancel).unwrap(), Outcome::Closed { target: closed } if closed == target)
        );
        #[cfg(target_os = "linux")]
        assert!(
            probe.find(&title).is_none(),
            "confirmed close retires its actual parent"
        );
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), actual);
        check_pcm(&mut runtime, 19);
        runtime.validate_native_author_source_closed(&cancel).unwrap();
        assert_eq!(serde_json::to_value(sequence).unwrap(), before);
    }
    drop(runtime);
    drop(registrar);
    let counts = census.lock();
    assert!(!counts.unavailable);
    assert_eq!((counts.scopes, counts.children), (0, 0));
    assert!(counts.settled >= 1);
}
