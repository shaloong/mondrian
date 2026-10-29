//! Actual third-party PCM through the public Runtime/Graph and normal Product.
//! The corpus is explicit; no native implementation or synthesized opaque state.

use mondrian_audio::{
    probe_clap_plugin_registration, AudioContinuityEpoch, AudioExecutionError, AudioProcessingMode,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant,
    AudioStateEntry, DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExecutionCancellationToken, NativeChildSettlementRegistrar, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
use std::{path::PathBuf, sync::Arc};

#[path = "support/portable_native_runtime.rs"]
mod support;
use support::{sample, Census, Signal};

#[test]
#[ignore = "requires the matching normal Product, official Ring Out CLAP and its actual -6dB state in MONDRIAN_CLAP_TEST_HELPER / MONDRIAN_CLAP_TEST_PLUGIN / MONDRIAN_CLAP_TEST_STATE"]
fn ring_out_runtime_restores_state_preserves_pcm_across_seek_and_retires_children() {
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("normal Product"));
    let plugin = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN").expect("official Ring Out CLAP"),
    );
    let minus6 = std::fs::read(
        std::env::var_os("MONDRIAN_CLAP_TEST_STATE").expect("actual native Ring Out -6dB state"),
    )
    .unwrap();
    assert!(!minus6.is_empty());
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    const ID: &str = "com.duskaudio.ring-out";
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].plugin_id, ID);
    let census = Arc::new(Census::default());
    let registrar = NativeChildSettlementRegistrar::try_new(census.clone()).unwrap();
    let cancel = ExecutionCancellationToken::new();

    let layouts = [AudioChannelLayout::Mono, AudioChannelLayout::Stereo];
    let states = [(0.0_f32, None), (-6.0, Some(minus6.as_slice()))];
    let cases = layouts.map(|layout| states.map(|(gain, seed)| (layout, gain, seed)));
    for (layout, gain_db, seed) in cases.into_iter().flatten() {
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: layout,
            max_block_frames: 64,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let native_registration =
            probe_clap_plugin_registration(&helper, &plugin, ID, render, seed).unwrap();
        assert!(native_registration.auxiliary_inputs.buses.is_empty());
        assert!(native_registration.auxiliary_outputs.buses.is_empty());
        // Check actual native getters before using either independent PCM oracle.
        // A nonidentity gain makes a skipped native process call observable.
        for (id, value) in [(7, f64::from(gain_db)), (8, 0.0)] {
            assert_eq!(
                native_registration
                    .parameters
                    .iter()
                    .find(|p| p.id == id)
                    .unwrap()
                    .current_value,
                value
            );
        }
        let initial = catalog.create_instance(ID, render, seed.map(<[u8]>::to_vec)).unwrap();
        let state = initial.opaque_state.as_ref().expect("native state capture").to_vec();
        assert!(!state.is_empty());
        let restored = catalog.create_instance(ID, render, Some(state.clone())).unwrap();
        assert_eq!(
            restored.opaque_state.as_deref().map(Vec::as_slice),
            Some(state.as_slice())
        );
        assert!(restored.parameters.values().all(|p| p.automation.is_none()));
        let instance = restored.id;
        let asset = AssetId::new();
        let mut sequence = Sequence::new("Portable real CLAP scoped Runtime");
        sequence.settings.audio_channel_layout = layout;
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
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(restored);
        let saved = serde_json::to_vec(&sequence).unwrap();
        let sequence: Sequence = serde_json::from_slice(&saved).unwrap();
        let before = serde_json::to_value(&sequence).unwrap();
        let resolver = IsolatedAudioProcessorResolver::new(catalog.clone());
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &Signal { asset, layout, rate: render.sample_rate },
            &resolver,
            render,
            None,
            AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
        )
        .unwrap();
        runtime.bind_native_child_settlement(&registrar).unwrap();
        assert!(
            census.lock().children > 0,
            "actual original native child is observed"
        );
        let installation = AudioOwnerInstallationSource::new();
        let (owner_registration, verifier) = installation.register().unwrap();
        runtime.bind_native_owner_installation(verifier.clone()).unwrap();
        let mut claim = installation
            .reserve_install(&owner_registration, None)
            .and_then(|r| installation.begin_commit(r))
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(
            runtime.install_native_owner_bindings(&claim, &cancel).unwrap(),
            1
        );
        let observed = runtime
            .poll_native_observations(&cancel)
            .unwrap()
            .expect("native cold observations");
        assert!(
            observed.author.is_none(),
            "restoring completed state is not an author edit"
        );
        let parameters = observed.parameters.expect("actual native getters");
        assert_eq!(parameters.target().instance, instance);
        let object = (parameters.process_id(), parameters.object_serial());
        assert_ne!(object.0.get(), std::process::id());
        for descriptor in native_registration.parameters.iter().filter(|p| !p.read_only) {
            let id = descriptor.parameter_id().unwrap();
            let fact = parameters.facts().iter().find(|fact| fact.parameter == id).unwrap();
            assert_eq!(fact.value_bits, Some(descriptor.current_value.to_bits()));
            assert_eq!(
                fact.author_value_bits,
                Some(descriptor.current_value.to_bits())
            );
        }
        runtime.validate_native_author_source_closed(&cancel).unwrap();

        let mut epoch = 0;
        let mut installs = 0;
        for (index, (start, partitions)) in
            [(0, [64, 64]), (0, [17, 47]), (97, [47, 17])].into_iter().enumerate()
        {
            epoch += 1;
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: start,
                })
                .unwrap();
            let mut done = 0;
            let mut block = 0;
            while done < 257 {
                let frames = partitions[block % partitions.len()].min(257 - done);
                let mut output = vec![f32::NAN; frames * layout.channel_count()];
                let request = AudioRenderRequest {
                    transport_activity: None,
                    start_sample: start + done as i64,
                    frames,
                };
                match runtime.render_into(request, &mut output) {
                    Ok(()) => {}
                    Err(AudioExecutionError::ReprepareRequired(ticket)) => {
                        assert!(installs < 2, "unexpected recurring restart: {ticket:?}");
                        assert!(
                            output.iter().all(|v| v.to_bits() == f32::NAN.to_bits()),
                            "preflight HOLD preserves every output word"
                        );
                        let operation = runtime
                            .service_pending_native_reprepare(ticket.product_notice(), &cancel)
                            .expect("capture actual native restart");
                        runtime
                            .prepare_native_reprepare_plan(&operation, verifier.clone(), &cancel)
                            .expect("prepare within the original resource grant");
                        let reservation = installation
                            .reserve_install(&owner_registration, Some(&claim))
                            .unwrap();
                        let mut authorization =
                            installation.authorize_runtime_installation(reservation).unwrap();
                        let receipt = runtime
                            .install_native_reprepare_plan(&operation, &mut authorization, &cancel)
                            .expect("install retained native owner");
                        claim = receipt.claim().clone();
                        runtime.finish_native_reprepare_retirement(receipt.retire()).unwrap();
                        epoch += 1;
                        runtime
                            .enter_state(AudioStateEntry {
                                epoch: AudioContinuityEpoch::new(epoch),
                                start_sample: request.start_sample,
                            })
                            .expect("explicit Fresh Enter after installation");
                        installs += 1;
                        runtime
                            .render_into(request, &mut output)
                            .expect("Fresh produces the full unchanged PCM request");
                    }
                    Err(error) => panic!("native render failed: {error:?}"),
                }
                for (frame, pcm) in output.chunks_exact(layout.channel_count()).enumerate() {
                    for (channel, &actual) in pcm.iter().enumerate() {
                        let expected = sample(start + (done + frame) as i64, channel)
                            * 10.0_f32.powf(gain_db / 20.0);
                        assert!(actual.is_finite() && (actual - expected).abs() <= 2e-6,
                            "{layout:?} {gain_db}dB epoch{index} frame{} channel{channel}: {actual} vs {expected}", done + frame);
                    }
                }
                done += frames;
                block += 1;
            }
            if let Some(observed) = runtime.poll_native_observations(&cancel).unwrap() {
                assert!(observed.author.is_none());
                if let Some(parameters) = observed.parameters {
                    assert_eq!(
                        (parameters.process_id(), parameters.object_serial()),
                        object
                    );
                }
                if let Some(diagnostics) = observed.diagnostics {
                    assert!(!diagnostics.loss().incomplete());
                }
            }
            runtime.validate_native_author_source_closed(&cancel).unwrap();
            assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
        }
        assert!(
            installs > 0,
            "the corpus must exercise actual reset/restart installation"
        );
        drop(runtime);
        assert_eq!(census.lock().children, 0);
    }
    drop(registrar);
    let final_state = census.lock();
    assert!(!final_state.unavailable);
    assert_eq!((final_state.scopes, final_state.children), (0, 0));
    assert!(final_state.settled >= layouts.len() * states.len());
}
