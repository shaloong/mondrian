//! Official AGain GUI through the public installed Runtime/Graph and normal Product.
//! X11 observations additionally prove a real foreign child and nonuniform painting.

#[cfg(target_os = "linux")]
#[path = "support/native_gui_x11.rs"]
mod native_window;
#[path = "support/portable_native_runtime.rs"]
mod support;

use mondrian_audio::{
    probe_vst3_plugin_registration, AudioContinuityEpoch, AudioNativeProcessorAddress,
    AudioProcessingMode, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
    AudioProcessorOccurrenceOwner, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    AudioRuntimeResourceGrant, AudioSessionCursorIdentity, AudioStateEntry,
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExactAutomationCurve, ExecutionCancellationToken, NativeAudioEditorControlIntent as Intent,
    NativeAudioEditorControlOutcome as Outcome, NativeAudioEditorInsertion,
    NativeAudioEditorLocator, NativeAudioEditorOccurrenceOwner, NativeAudioEditorTarget,
    NativeAudioEditorTitle, NativeChildSettlementRegistrar, Rational, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
use std::{path::PathBuf, sync::Arc};
use support::{sample, Census, Signal};

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

#[test]
#[ignore = "requires matching normal Product and unmodified SDK3.8.0 AGain GUI; Linux requires authenticated X11 display"]
fn again_gui_retains_installed_object_pcm_and_native_parent_through_repeated_close() {
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_VST3_TEST_HELPER").expect("normal Product"));
    let plugin = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_GUI_TEST_PLUGIN").expect("official GUI reference"),
    );
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    // Explicit official reference class, independently of discovery ordering or captions.
    let class = &catalog
        .descriptors()
        .iter()
        .find(|d| d.class_id.eq_ignore_ascii_case("84E8DE5F92554F5396FAE4133C935A18"))
        .expect("official non-sidechain AGain class")
        .class_id;
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let registration =
        probe_vst3_plugin_registration(&helper, &plugin, class, render, None).unwrap();
    assert!(registration.auxiliary_inputs.buses.is_empty());
    let mut author = catalog.create_instance(class, render, None).unwrap();
    assert!(author.opaque_state.as_ref().is_some_and(|state| !state.is_empty()));
    // Explicit gain and bypass stimuli, not assumptions about plugin defaults.
    let gain = 0.5_f32;
    for (id, value) in [(0, f64::from(gain)), (2, 0.0)] {
        let parameter = registration.parameters.iter().find(|p| p.id == id).unwrap();
        assert!(parameter.can_automate && !parameter.read_only);
        author
            .set_parameter_automation(
                ExactAutomationCurve::new(parameter.parameter_id().unwrap(), value).unwrap(),
            )
            .unwrap();
    }
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
    let before = serde_json::to_value(&sequence).unwrap();
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &Signal {
            asset,
            layout: render.channel_layout,
            rate: render.sample_rate,
        },
        &resolver,
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
    let locator = NativeAudioEditorLocator {
        sequence: sequence.id,
        instance,
        owner: NativeAudioEditorOccurrenceOwner::Output(output),
        insertion: NativeAudioEditorInsertion::PreFader,
    };
    #[cfg(target_os = "linux")]
    let probe = native_window::Probe::connect();
    let mut same_object = None;
    for cycle in 0..3 {
        let title = format!("Mondrian GUI reference {instance} {cycle}");
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
                    "real foreign child must paint through native main service"
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
            // AGain declares a fixed viewport. A native host resize must return
            // to the actual accepted bounds without replacing or breaking DSP.
            probe.request_resize(window, width + 17, height + 9);
            let deadline = Instant::now() + Duration::from_secs(3);
            while probe.size(window) != (width, height) {
                runtime.poll_native_observations(&cancel).unwrap();
                assert!(
                    Instant::now() < deadline,
                    "fixed native view bounds must be honored"
                );
            }
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
        assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
    }
    drop(runtime);
    drop(registrar);
    let counts = census.lock();
    assert!(!counts.unavailable);
    assert_eq!((counts.scopes, counts.children), (0, 0));
    assert!(counts.settled >= 1);
}
