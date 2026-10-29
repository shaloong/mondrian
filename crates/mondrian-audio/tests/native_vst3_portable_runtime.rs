//! Official ADelay through the public Runtime/Graph and matching normal Product.
//! The oracle follows SDK3.8.0's ring buffer, including its one-sample minimum.

#[path = "support/portable_native_runtime.rs"]
mod support;

use mondrian_audio::{
    probe_vst3_plugin_registration, AudioContinuityEpoch, AudioProcessingMode, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant, AudioStateEntry,
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    AutomationSegmentInterpolation, ExactAutomationCurve, ExactAutomationKeyframe,
    ExecutionCancellationToken, NativeChildSettlementRegistrar, Rational, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
use std::{path::PathBuf, sync::Arc};
use support::{sample, Census, Signal};

#[test]
#[ignore = "requires matching Product and official SDK3.8.0 ADelay in MONDRIAN_VST3_TEST_HELPER / MONDRIAN_VST3_TEST_PLUGIN"]
fn adelay_runtime_restores_state_processes_delayed_pcm_across_seek_and_retires_children() {
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_VST3_TEST_HELPER").expect("normal Product"));
    let plugin =
        PathBuf::from(std::env::var_os("MONDRIAN_VST3_TEST_PLUGIN").expect("official ADelay"));
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .expect("actual native class discovery"),
    );
    let descriptors = catalog.descriptors();
    assert_eq!(descriptors.len(), 1, "one reference effect class");
    let class_id = descriptors[0].class_id.clone();
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let registration = probe_vst3_plugin_registration(&helper, &plugin, &class_id, render, None)
        .expect("actual native processing metadata");
    assert!(registration.auxiliary_inputs.buses.is_empty());
    let delay = registration
        .parameters
        .iter()
        .find(|p| p.name == "Delay")
        .expect("ADelay parameter");
    assert!(delay.can_automate && !delay.read_only);
    assert_eq!(delay.step_count, 0);
    let delay_id = delay.parameter_id().unwrap();
    let initial = catalog.create_instance(&class_id, render, None).unwrap();
    let state = initial.opaque_state.expect("actual native state capture").to_vec();
    assert!(!state.is_empty());
    let census = Arc::new(Census::default());
    let registrar = NativeChildSettlementRegistrar::try_new(census.clone()).unwrap();
    let cancel = ExecutionCancellationToken::new();
    // Explicit stimuli, independent of plugin/project defaults. A nonzero delay
    // and nonzero source make bypass, silence and skipped native DSP observable.
    let delays = [0.0, 1.0 / 64.0];
    for normalized in delays {
        let mut author = catalog.create_instance(&class_id, render, Some(state.clone())).unwrap();
        assert_eq!(
            author.opaque_state.as_deref().map(Vec::as_slice),
            Some(state.as_slice())
        );
        assert!(author.parameters.values().all(|p| p.automation.is_none()));
        assert!(!author.bypassed);
        let mut curve = ExactAutomationCurve::new(delay_id.clone(), normalized).unwrap();
        let mut key = ExactAutomationKeyframe::linear(TimelineTime::ZERO, normalized);
        key.interpolation_to_next = AutomationSegmentInterpolation::Hold;
        curve.set_keyframe(key).unwrap();
        author.set_parameter_automation(curve).unwrap();
        let instance = author.id;
        let asset = AssetId::new();
        let mut sequence = Sequence::new("Portable official ADelay Runtime");
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
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(author);
        let saved = serde_json::to_vec(&sequence).unwrap();
        let sequence: Sequence = serde_json::from_slice(&saved).unwrap();
        let before = serde_json::to_value(&sequence).unwrap();
        let resolver = IsolatedAudioProcessorResolver::new(catalog.clone());
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
            AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
        )
        .unwrap();
        runtime.bind_native_child_settlement(&registrar).unwrap();
        assert!(census.lock().children > 0);
        let installation = AudioOwnerInstallationSource::new();
        let (owner_registration, verifier) = installation.register().unwrap();
        runtime.bind_native_owner_installation(verifier).unwrap();
        let claim = installation
            .reserve_install(&owner_registration, None)
            .and_then(|r| installation.begin_commit(r))
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(
            runtime.install_native_owner_bindings(&claim, &cancel).unwrap(),
            1
        );
        let cold = runtime.poll_native_observations(&cancel).unwrap().expect("native observations");
        assert!(cold.author.is_none());
        let facts = cold.parameters.expect("actual native getters");
        assert_eq!(facts.target().instance, instance);
        assert_ne!(facts.process_id().get(), std::process::id());

        let delay_frames = (normalized * f64::from(render.sample_rate)) as usize;
        let delay_frames = delay_frames.max(1);
        let frames_per_epoch = delay_frames + 3 * render.max_block_frames + 1;
        let short = render.max_block_frames / 4 + 1;
        let partitions = [
            render.max_block_frames,
            short,
            render.max_block_frames - short,
        ];
        for (epoch, start) in
            [0, 0, (2 * render.max_block_frames + 7) as i64].into_iter().enumerate()
        {
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch as u64 + 1),
                    start_sample: start,
                })
                .unwrap();
            let mut done = 0;
            let mut block = 0;
            let mut audible = false;
            while done < frames_per_epoch {
                let frames = partitions[block % partitions.len()].min(frames_per_epoch - done);
                let mut output = vec![f32::NAN; frames * render.channel_layout.channel_count()];
                runtime
                    .render_into(
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start + done as i64,
                            frames,
                        },
                        &mut output,
                    )
                    .expect("complete native delayed PCM request");
                for (frame, pcm) in
                    output.chunks_exact(render.channel_layout.channel_count()).enumerate()
                {
                    let relative = done + frame;
                    for (channel, &actual) in pcm.iter().enumerate() {
                        let expected = if relative < delay_frames {
                            0.0
                        } else {
                            sample(start + (relative - delay_frames) as i64, channel)
                        };
                        audible |= expected != 0.0;
                        assert!(actual.is_finite() && (actual - expected).abs() <= 2e-6,
                            "delay{normalized} epoch{epoch} frame{relative} channel{channel}: {actual} vs {expected}");
                    }
                }
                done += frames;
                block += 1;
            }
            assert!(
                audible,
                "each epoch must expose actual nonidentity DSP output"
            );
            runtime.validate_native_author_source_closed(&cancel).unwrap();
            assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
        }
        drop(runtime);
        assert_eq!(census.lock().children, 0);
    }
    drop(registrar);
    let final_state = census.lock();
    assert!(!final_state.unavailable);
    assert_eq!((final_state.scopes, final_state.children), (0, 0));
    assert!(final_state.settled >= delays.len());
}
