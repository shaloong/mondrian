use super::*;

#[test]
#[ignore = "rebuilt mode7 owned CLAP fixture and production isolated worker"]
fn installed_clap_restart_activation_profile_pcm() {
    let catalog = restart_catalog();
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        for restart_seen in [false, true] {
            let (spec, delay) = restart_spec(&catalog, mode, restart_seen);
            let keys: Vec<_> =
                spec.auxiliary_inputs.buses.iter().map(|bus| bus.bus_key.clone()).collect();
            assert_eq!(delay, if restart_seen { 7 } else { 3 });
            let factory =
                IsolatedAudioProcessorFactory::prepare(spec).expect("native profile factory");
            let mut processor = factory.create(None).expect("native activated profile");
            // A false restart profile stays below the trigger; a true profile
            // records an already acknowledged restart and never requests again.
            processor.enter_state(-16).expect("fresh reference history");
            for start in [-16, -12, -8] {
                let mut audio = restart_audio(&keys);
                processor
                    .process(
                        restart_context(start, mode),
                        &mut audio,
                        AudioParameterEventBatch::new(start, 4, &[], &[], &[]),
                    )
                    .expect("native reference profile DSP");
                for (offset, frame) in audio.main.chunks_exact(2).enumerate() {
                    let index = (start + 16) as usize + offset;
                    let expected = if index < delay { 0.0 } else { 2.625 };
                    assert_eq!(
                        frame,
                        [expected, -expected * 0.5],
                        "independent native delay {delay}, frame{index}, {mode:?}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "rebuilt mode7 owned CLAP fixture + current worker; existing API behavior RED"]
fn installed_clap_process_restart_preserves_completed_trigger_window() {
    let catalog = restart_catalog();
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let (spec, delay) = restart_spec(&catalog, mode, false);
        assert_eq!(delay, 3);
        let keys: Vec<_> =
            spec.auxiliary_inputs.buses.iter().map(|bus| bus.bus_key.clone()).collect();
        let factory = IsolatedAudioProcessorFactory::prepare(spec).expect("native restart factory");
        let mut processor = factory.create(None).expect("native restart child");
        processor.enter_state(0).expect("native restart epoch");
        for start in [0, 4, 8] {
            let mut audio = restart_audio(&keys);
            processor.process(restart_context(start,mode), &mut audio,
                AudioParameterEventBatch::new(start,4,&[],&[],&[]))
                .expect("complete trigger block uses valid old active contract even after core request_restart");
            for (offset, frame) in audio.main.chunks_exact(2).enumerate() {
                let index = start as usize + offset;
                let expected = if index < 3 { 0.0 } else { 2.625 };
                assert_eq!(
                    frame,
                    [expected, -expected * 0.5],
                    "trigger PCM frame {index}, {mode:?}"
                );
            }
        }
        let mut unpublished = restart_audio(&keys);
        let original = unpublished.main.clone();
        let outcome = processor.process(
            restart_context(12, mode),
            &mut unpublished,
            AudioParameterEventBatch::new(12, 4, &[], &[], &[]),
        );
        let Err(crate::AudioProcessorHostError::ReprepareRequired(request)) = outcome else {
            panic!(
                "expected typed retained CLAP pending, not worker failure: {mode:?}, {outcome:?}"
            );
        };
        assert_eq!(
            request.request(),
            mondrian_core::NativeAudioReprepareRequest::ClapRestart
        );
        assert_eq!(
            processor
                .preflight_reprepare()
                .expect("retained live CLAP child replies to actual pending IPC"),
            Some(request)
        );
        assert_eq!(
            unpublished.main, original,
            "pending request must preserve caller-owned next PCM"
        );
    }
}

fn restart_catalog() -> DiscoveredClapAudioProcessorSpecResolver {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current CLAP worker");
    let binary = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("rebuilt CLAP fixture");
    DiscoveredClapAudioProcessorSpecResolver::discover(helper, [binary])
        .expect("native restart discovery")
}

fn restart_spec(
    catalog: &DiscoveredClapAudioProcessorSpecResolver,
    mode: AudioProcessingMode,
    seen: bool,
) -> (IsolatedAudioProcessorWorkerSpec, usize) {
    let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
    let mut state = vec![7];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    state.push(u8::from(seen));
    let instance = catalog
        .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
        .expect("actual state/mode activation profile");
    assert!(instance.parameters.is_empty());
    let spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: instance.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            render,
        ))
        .expect("actual restart profile binding");
    let delay = spec.plugin_execution_contract.algorithmic_latency_frames();
    assert_eq!(
        spec.plugin_execution_contract.tail(),
        AudioProcessorTail::Finite(delay)
    );
    (spec, delay)
}

fn restart_context(start: i64, mode: AudioProcessingMode) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { transport_activity: None, start_sample: start, frames: 4 },
        AudioSampleRate::new(48_000).expect("reference sample rate"),
        AudioChannelLayout::Stereo,
        mode,
        AudioKernelBackend::ScalarReference,
    )
}

fn restart_audio(keys: &[String]) -> NativeSidechainAudioIo {
    NativeSidechainAudioIo {
        main: [1.0, -0.5].repeat(4),
        inputs: keys
            .iter()
            .zip([0.25, 0.5])
            .map(|(key, sample)| (key.clone(), [sample, -sample * 0.5].repeat(4)))
            .collect(),
    }
}
