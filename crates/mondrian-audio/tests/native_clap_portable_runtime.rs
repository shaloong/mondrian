//! Actual third-party PCM through the public Runtime/Graph and normal Product.
//! The corpus is explicit; no native implementation or synthesized opaque state.

use mondrian_audio::{
    probe_clap_plugin_registration, AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver,
    AudioProcessingMode, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    AudioRuntimeResourceGrant, AudioStateEntry, DiscoveredClapAudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExecutionCancellationToken, NativeChildSettlement, NativeChildSettlementError,
    NativeChildSettlementRegistrar, NativeChildSettlementReporter, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Signal {
    asset: AssetId,
    layout: AudioChannelLayout,
    rate: u32,
}

fn sample(frame: i64, channel: usize) -> f32 {
    if frame < 0 {
        return 0.0;
    }
    (((frame as usize * 29 + channel * 11) % 113) as f32 - 56.0) / 256.0
}

impl AudioMediaResolver for Signal {
    fn resolve(
        &self,
        asset: AssetId,
        component: AudioSourceComponentId,
        rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        assert_eq!(
            (asset, component, rate),
            (self.asset, AudioSourceComponentId::primary(), self.rate)
        );
        Ok(ResolvedAudioSource::new(
            self.layout,
            Arc::new(self.clone()),
        ))
    }
}

impl AudioDecodedSource for Signal {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        output: &mut [f32],
        cancel: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        assert!(!cancel.is_canceled());
        let channels = self.layout.channel_count();
        assert_eq!(output.len(), frames * channels);
        for (index, frame) in output.chunks_exact_mut(channels).enumerate() {
            for (channel, value) in frame.iter_mut().enumerate() {
                *value = sample(start + index as i64, channel);
            }
        }
        Ok(())
    }
}

// Observe the original cross-platform child owners. No PID polling or second
// process owner; this evidence does not certify graceful-shutdown timing.
#[derive(Default)]
struct Counts {
    scopes: usize,
    children: usize,
    settled: usize,
    unavailable: bool,
}
#[derive(Default)]
struct Census(Mutex<Counts>);
impl Census {
    fn lock(&self) -> std::sync::MutexGuard<'_, Counts> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.unavailable = true;
                state
            }
        }
    }
    fn begin(&self, child: bool) -> Result<(), NativeChildSettlementError> {
        let mut state = self.lock();
        if state.unavailable {
            return Err(NativeChildSettlementError::Unavailable);
        }
        let count = if child {
            &mut state.children
        } else {
            &mut state.scopes
        };
        match count.checked_add(1) {
            Some(next) => {
                *count = next;
                Ok(())
            }
            None => {
                state.unavailable = true;
                Err(NativeChildSettlementError::Overflow)
            }
        }
    }
    fn end(&self, child: bool) {
        let mut state = self.lock();
        let count = if child {
            &mut state.children
        } else {
            &mut state.scopes
        };
        match count.checked_sub(1) {
            Some(next) => *count = next,
            None => state.unavailable = true,
        }
    }
}
impl NativeChildSettlementReporter for Census {
    fn begin_scope(&self) -> Result<(), NativeChildSettlementError> {
        self.begin(false)
    }
    fn end_scope(&self) {
        self.end(false);
    }
    fn begin_child(&self) -> Result<(), NativeChildSettlementError> {
        self.begin(true)
    }
    fn end_child(&self, outcome: NativeChildSettlement) {
        self.end(true);
        let mut state = self.lock();
        match (outcome, state.settled.checked_add(1)) {
            (NativeChildSettlement::Settled, Some(next)) => state.settled = next,
            _ => state.unavailable = true,
        }
    }
    fn unavailable(&self) {
        self.lock().unavailable = true;
    }
}

#[test]
#[ignore = "requires the matching normal Product and official Ring Out CLAP in MONDRIAN_CLAP_TEST_HELPER / MONDRIAN_CLAP_TEST_PLUGIN"]
fn ring_out_runtime_restores_state_preserves_pcm_across_seek_and_retires_children() {
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("normal Product"));
    let plugin = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN").expect("official Ring Out CLAP"),
    );
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

    for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
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
            probe_clap_plugin_registration(&helper, &plugin, ID, render, None).unwrap();
        assert!(native_registration.auxiliary_inputs.buses.is_empty());
        assert!(native_registration.auxiliary_outputs.buses.is_empty());
        // This release's flat captured preset is the input to the unity oracle.
        // Fail a changed corpus condition instead of silently changing the oracle.
        for id in [7, 8] {
            assert_eq!(
                native_registration
                    .parameters
                    .iter()
                    .find(|p| p.id == id)
                    .unwrap()
                    .current_value,
                0.0
            );
        }
        let initial = catalog.create_instance(ID, render, None).unwrap();
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

        for (index, (start, partitions)) in
            [(0, [64, 64]), (0, [17, 47]), (97, [47, 17])].into_iter().enumerate()
        {
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(index as u64 + 1),
                    start_sample: start,
                })
                .unwrap();
            let mut done = 0;
            let mut block = 0;
            while done < 257 {
                let frames = partitions[block % partitions.len()].min(257 - done);
                let mut output = vec![f32::NAN; frames * layout.channel_count()];
                runtime
                    .render_into(
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start + done as i64,
                            frames,
                        },
                        &mut output,
                    )
                    .unwrap();
                for (frame, pcm) in output.chunks_exact(layout.channel_count()).enumerate() {
                    for (channel, &actual) in pcm.iter().enumerate() {
                        let expected = sample(start + (done + frame) as i64, channel);
                        assert!(actual.is_finite() && (actual - expected).abs() <= 2e-6,
                            "{layout:?} epoch{index} frame{} channel{channel}: {actual} vs {expected}", done + frame);
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
        drop(runtime);
        assert_eq!(census.lock().children, 0);
    }
    drop(registrar);
    let final_state = census.lock();
    assert!(!final_state.unavailable);
    assert_eq!((final_state.scopes, final_state.children), (0, 0));
    assert!(final_state.settled >= 2);
}
