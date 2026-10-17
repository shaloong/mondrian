//! Real persistent-worker control oracle; candidate installation is a separate gate.
use super::*;
use std::num::NonZeroU64;

#[test]
#[ignore = "fresh production helper and owned VST3 modes5/6; typed control must produce a real retained candidate"]
fn installed_vst3_retained_configuration_candidate_uses_actual_component_profile() {
    let helper = native_path("MONDRIAN_VST3_TEST_HELPER");
    let binary = native_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN");
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary])
        .expect("profile catalog");
    let class = &catalog.descriptors()[0].class_id;
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
        let authored = catalog
            .create_instance(class, render, Some(vst3_sidechain_snapshot(0.5, 5)))
            .expect("native configuration profile");
        let ids: Vec<_> = authored.parameters.keys().cloned().collect();
        let spec = configuration_spec(&catalog, &authored, render);
        let keys = auxiliary_keys(&spec);
        assert_eq!(
            spec.plugin_execution_contract.algorithmic_latency_frames(),
            3
        );
        let mut processor = IsolatedAudioProcessorFactory::prepare(spec)
            .expect("profile factory")
            .create()
            .expect("retained profile child");
        let cohort = trigger_vst3_configuration(&mut *processor, &keys, &ids, mode);
        let token = NonZeroU64::new(17).expect("service token");
        // A controller mirror value cannot substitute for the actual successful
        // component DSP configuration. Wrong scalar profile leaves requests live.
        let refused = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[0.25],
                configuration_limit_bytes: 128 * 1024,
                control_buffer_grant_bytes: 1024 * 1024,
            })
            .expect("typed profile refusal");
        assert_eq!(
            refused,
            NativeConfigurationControlOutcome::CapabilityRefused(
                NativeConfigurationRefusal::UnsupportedConstantProfile
            )
        );
        assert_eq!(
            processor.native_configuration_cohort().expect("retained child"),
            Some(cohort)
        );
        let below_grant = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[0.5],
                configuration_limit_bytes: 128 * 1024,
                control_buffer_grant_bytes: 128 * 1024,
            })
            .expect("typed incremental byte-grant refusal");
        assert_eq!(
            below_grant,
            NativeConfigurationControlOutcome::CapabilityRefused(
                NativeConfigurationRefusal::BufferGrantExceeded
            )
        );
        assert_eq!(
            processor
                .native_configuration_cohort()
                .expect("retained pending after insufficient grant"),
            Some(cohort)
        );
        let ready = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[0.5],
                configuration_limit_bytes: 128 * 1024,
                control_buffer_grant_bytes: 1024 * 1024,
            })
            .expect("bounded actual retained service");
        let NativeConfigurationControlOutcome::ReadyForInstall(candidate) = ready else {
            panic!("actual component candidate expected: {ready:?}");
        };
        assert_candidate(
            &candidate,
            cohort,
            token,
            NativeConfigurationFormat::Vst3ComponentAndController,
        );
        assert_blocked_retained(&mut *processor, &keys, 8, mode);
        // Independently instantiate the published configuration twice. Neither
        // controller-only writes nor reused old DSP history can satisfy this PCM.
        for _ in 0..2 {
            let fresh = catalog
                .create_instance(class, render, Some(candidate.configuration.clone()))
                .expect("real restored seed probe");
            assert_fresh_candidate(&catalog, &fresh, render, candidate.observed_contract);
        }
    }
}

#[test]
#[ignore = "fresh production helper + owned CLAP mode7; typed retained control and independent restored seed PCM"]
fn installed_clap_retained_configuration_candidate_captures_actual_activated_profile() {
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(
        native_path("MONDRIAN_CLAP_TEST_HELPER"),
        [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
    )
    .expect("CLAP control catalog");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
        let authored = catalog
            .create_instance(
                "org.mondrian.fixture.sidechain",
                render,
                Some(clap_configuration(7, false)),
            )
            .expect("actual CLAP profile");
        let (mut processor, keys, cohort) = trigger_clap_configuration(&catalog, &authored, render);
        let token = NonZeroU64::new(19).expect("service token");
        let below_grant = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[],
                configuration_limit_bytes: 64 * 1024,
                control_buffer_grant_bytes: 1024,
            })
            .expect("CLAP bounded grant refusal");
        assert_eq!(
            below_grant,
            NativeConfigurationControlOutcome::CapabilityRefused(
                NativeConfigurationRefusal::BufferGrantExceeded
            )
        );
        assert_eq!(
            processor.native_configuration_cohort().expect("retained pending"),
            Some(cohort)
        );
        let ready = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[],
                configuration_limit_bytes: 64 * 1024,
                control_buffer_grant_bytes: 64 * 1024,
            })
            .expect("retained native CLAP service");
        let NativeConfigurationControlOutcome::ReadyForInstall(candidate) = ready else {
            panic!("real CLAP candidate expected: {ready:?}");
        };
        assert_candidate(
            &candidate,
            cohort,
            token,
            NativeConfigurationFormat::ClapState,
        );
        assert_eq!(candidate.configuration, clap_configuration(7, true));
        assert_blocked_retained(&mut *processor, &keys, 12, mode);
        let repeated = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &[],
                configuration_limit_bytes: 64 * 1024,
                control_buffer_grant_bytes: 128 * 1024,
            })
            .expect("candidate owner remains live and stopped");
        assert_eq!(
            repeated,
            NativeConfigurationControlOutcome::CapabilityRefused(
                NativeConfigurationRefusal::AwaitingInstallation
            )
        );
        for _ in 0..2 {
            let fresh = catalog
                .create_instance(
                    "org.mondrian.fixture.sidechain",
                    render,
                    Some(candidate.configuration.clone()),
                )
                .expect("actual fresh CLAP state");
            assert_fresh_candidate(&catalog, &fresh, render, candidate.observed_contract);
        }
    }
}

#[test]
#[ignore = "fresh production helper + owned CLAP state8; same-bit capture callback must remain pending"]
fn installed_clap_configuration_capture_preserves_new_same_bit_request_cohort() {
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(
        native_path("MONDRIAN_CLAP_TEST_HELPER"),
        [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
    )
    .expect("CLAP cohort catalog");
    let render = test_render_contract();
    let authored = catalog
        .create_instance(
            "org.mondrian.fixture.sidechain",
            render,
            Some(clap_configuration(8, false)),
        )
        .expect("capture-callback fixture");
    let (mut processor, keys, cohort) = trigger_clap_configuration(&catalog, &authored, render);
    let outcome = processor
        .service_native_configuration(NativeConfigurationControlRequest {
            expected_cohort: cohort,
            control_token: NonZeroU64::new(23).expect("token"),
            canonical_constant_values: &[],
            configuration_limit_bytes: 64 * 1024,
            control_buffer_grant_bytes: 128 * 1024,
        })
        .expect("capture callback is recoverable");
    let NativeConfigurationControlOutcome::StillPending(Some(newer)) = outcome else {
        panic!("new same-bit callback must prevent retirement: {outcome:?}");
    };
    assert_eq!(newer.request, cohort.request);
    assert_eq!(newer.serial, cohort.serial + 1);
    assert_eq!(
        processor.native_configuration_cohort().expect("no lost callback"),
        Some(newer)
    );
    assert_blocked_retained(&mut *processor, &keys, 12, render.processing_mode);
}

#[test]
#[ignore = "fresh production helper + owned CLAP state9; ignored stream overflow must be independently rejected"]
fn installed_clap_configuration_capture_refuses_ignored_writer_overflow_without_child_fault() {
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(
        native_path("MONDRIAN_CLAP_TEST_HELPER"),
        [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
    )
    .expect("CLAP bounded state catalog");
    let render = test_render_contract();
    let authored = catalog
        .create_instance(
            "org.mondrian.fixture.sidechain",
            render,
            Some(clap_configuration(9, false)),
        )
        .expect("ignored-overflow fixture");
    let (mut processor, keys, cohort) = trigger_clap_configuration(&catalog, &authored, render);
    let outcome = processor
        .service_native_configuration(NativeConfigurationControlRequest {
            expected_cohort: cohort,
            control_token: NonZeroU64::new(29).expect("token"),
            canonical_constant_values: &[],
            configuration_limit_bytes: 64 * 1024,
            control_buffer_grant_bytes: 128 * 1024,
        })
        .expect("bounded state refusal is recoverable");
    assert_eq!(
        outcome,
        NativeConfigurationControlOutcome::CapabilityRefused(
            NativeConfigurationRefusal::ConfigurationTooLarge
        )
    );
    assert_eq!(
        processor.native_configuration_cohort().expect("retained owner remains live"),
        Some(cohort)
    );
    assert_blocked_retained(&mut *processor, &keys, 12, render.processing_mode);
}

fn native_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .expect("current product/native fixture artifact")
}
fn clap_configuration(mode: u8, restarted: bool) -> Vec<u8> {
    let mut state = vec![mode];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    state.push(u8::from(restarted));
    state
}
fn configuration_spec(
    resolver: &impl IsolatedAudioProcessorSpecResolver,
    authored: &mondrian_timeline::audio::AudioProcessorInstance,
    render: AudioRenderContract,
) -> IsolatedAudioProcessorWorkerSpec {
    resolver
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: authored.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &authored.definition,
            &authored.parameters,
            authored.opaque_state.as_ref().map(AsRef::as_ref),
            render,
        ))
        .expect("actual configuration spec")
}
fn auxiliary_keys(spec: &IsolatedAudioProcessorWorkerSpec) -> Vec<String> {
    spec.auxiliary_inputs.buses.iter().map(|bus| bus.bus_key.clone()).collect()
}
fn configuration_audio(keys: &[String]) -> NativeSidechainAudioIo {
    NativeSidechainAudioIo {
        main: [1.0, -0.5].repeat(4),
        inputs: keys
            .iter()
            .zip([0.25, 0.5])
            .map(|(key, value)| (key.clone(), [value, -value * 0.5].repeat(4)))
            .collect(),
    }
}
fn configuration_context(
    start_sample: i64,
    mode: AudioProcessingMode,
) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { start_sample, frames: 4 },
        AudioSampleRate::new(48_000).expect("sample rate"),
        AudioChannelLayout::Stereo,
        mode,
        AudioKernelBackend::ScalarReference,
    )
}
fn assert_constant_delay(pcm: &[f32], processed: usize, delay: usize) {
    for (offset, frame) in pcm.chunks_exact(2).enumerate() {
        let expected = if processed + offset < delay {
            0.0
        } else {
            2.625
        };
        assert_eq!(
            frame,
            [expected, -expected * 0.5],
            "independent delay {delay}, actual input-frame {}",
            processed + offset
        );
    }
}

fn assert_configuration_audio_unchanged(
    audio: &NativeSidechainAudioIo,
    main: &[f32],
    inputs: &[(String, Vec<f32>)],
) {
    let bits = |samples: &[f32]| samples.iter().map(|sample| sample.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&audio.main), bits(main));
    assert_eq!(audio.inputs.len(), inputs.len());
    for ((key, actual), (expected_key, expected)) in audio.inputs.iter().zip(inputs) {
        assert_eq!(key, expected_key);
        assert_eq!(bits(actual), bits(expected));
    }
}

fn trigger_vst3_configuration(
    processor: &mut dyn AudioProcessor,
    keys: &[String],
    ids: &[ParameterId],
    mode: AudioProcessingMode,
) -> NativeRequestCohort {
    let restart = mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 };
    processor.enter_state(0).expect("old epoch");
    for start in [0, 4] {
        let mut audio = configuration_audio(keys);
        let original_main = audio.main.clone();
        let original_inputs = audio.inputs.clone();
        let result = processor.process(
            configuration_context(start, mode),
            &mut audio,
            AudioParameterEventBatch::new(
                start,
                4,
                ids,
                std::slice::from_ref(&(0..1)),
                &[AudioParameterEvent { sample_offset: 0, value: 0.5 }],
            ),
        );
        if start == 0 {
            result.expect("first admitted component DSP must complete");
            assert_constant_delay(&audio.main, 0, 3);
        } else {
            match result {
                Ok(()) => assert_constant_delay(&audio.main, start as usize, 3),
                Err(error) => {
                    assert!(
                        matches!(error, AudioProcessorHostError::ReprepareRequired(observed) if observed.request() == restart)
                    );
                    assert_configuration_audio_unchanged(&audio, &original_main, &original_inputs);
                }
            }
        }
    }
    // Main may consume the first scalar sync before or after the first DSP
    // changes profile 5 to 6. If the second block is admitted, its sync follows
    // that DSP; otherwise the restart is already observed. This control query
    // services the sync without submitting another block.
    let cohort = processor
        .native_configuration_cohort()
        .expect("live control query")
        .expect("actual component-requested cohort within the two-block bound");
    assert!(cohort.serial > 0);
    assert_eq!(cohort.request, restart);
    assert_eq!(
        processor.preflight_reprepare().expect("pending preflight"),
        Some(crate::NativeObservedRequest::Cohort(cohort))
    );
    let mut audio = configuration_audio(keys);
    let original_main = audio.main.clone();
    let original_inputs = audio.inputs.clone();
    assert_eq!(
        processor.process(
            configuration_context(8, mode),
            &mut audio,
            AudioParameterEventBatch::new(
                8,
                4,
                ids,
                std::slice::from_ref(&(0..1)),
                &[AudioParameterEvent { sample_offset: 0, value: 0.5 }],
            ),
        ),
        Err(AudioProcessorHostError::ReprepareRequired(
            crate::NativeObservedRequest::Cohort(cohort)
        ))
    );
    assert_configuration_audio_unchanged(&audio, &original_main, &original_inputs);
    assert_eq!(
        processor.native_configuration_cohort().expect("same pending cohort"),
        Some(cohort)
    );
    cohort
}
fn assert_candidate(
    candidate: &NativeConfigurationCandidate,
    cohort: NativeRequestCohort,
    token: NonZeroU64,
    format: NativeConfigurationFormat,
) {
    assert_eq!(candidate.serviced_cohort, cohort);
    assert_eq!(candidate.control_token, token);
    assert_eq!(candidate.format, format);
    assert_eq!(candidate.observed_contract.algorithmic_latency_frames(), 7);
    assert_eq!(
        candidate.observed_contract.tail(),
        AudioProcessorTail::Finite(7)
    );
    assert_eq!(
        candidate.parent_configuration_capacity_bytes,
        candidate.configuration.capacity()
    );
    assert!(candidate.configuration.len() <= candidate.buffers.configuration_capacity_bytes);
    assert!(candidate.buffers.peak_bytes().expect("checked byte buffers") <= 1024 * 1024);
}
fn assert_blocked_retained(
    processor: &mut dyn AudioProcessor,
    keys: &[String],
    start: i64,
    mode: AudioProcessingMode,
) {
    assert!(
        matches!(
            processor.enter_state(start),
            Err(crate::AudioProcessorHostError::ReprepareRequired(_))
        ),
        "old seed cannot enter after control changed activation"
    );
    let mut audio = configuration_audio(keys);
    let original = audio.main.clone();
    assert!(matches!(
        processor.process(
            configuration_context(start, mode),
            &mut audio,
            AudioParameterEventBatch::new(start, 4, &[], &[], &[])
        ),
        Err(crate::AudioProcessorHostError::ReprepareRequired(_))
    ));
    assert_eq!(
        audio.main, original,
        "blocked old-contract PCM is never copied/published"
    );
    assert!(processor.preflight_reprepare().expect("retained live child").is_some());
}
fn trigger_clap_configuration(
    catalog: &DiscoveredClapAudioProcessorSpecResolver,
    authored: &mondrian_timeline::audio::AudioProcessorInstance,
    render: AudioRenderContract,
) -> (Box<dyn AudioProcessor>, Vec<String>, NativeRequestCohort) {
    assert!(authored.parameters.is_empty());
    let spec = configuration_spec(catalog, authored, render);
    let keys = auxiliary_keys(&spec);
    let mut processor = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("CLAP profile factory")
        .create()
        .expect("retained CLAP child");
    processor.enter_state(0).expect("old epoch");
    for start in [0, 4, 8] {
        let mut audio = configuration_audio(&keys);
        processor
            .process(
                configuration_context(start, render.processing_mode),
                &mut audio,
                AudioParameterEventBatch::new(start, 4, &[], &[], &[]),
            )
            .expect("old trigger window completes");
        assert_constant_delay(&audio.main, start as usize, 3);
    }
    let cohort = processor
        .native_configuration_cohort()
        .expect("native cohort query")
        .expect("actual CLAP callback cohort");
    assert_eq!(
        cohort.request,
        mondrian_core::NativeAudioReprepareRequest::ClapRestart
    );
    (processor, keys, cohort)
}
fn assert_fresh_candidate(
    resolver: &impl IsolatedAudioProcessorSpecResolver,
    authored: &mondrian_timeline::audio::AudioProcessorInstance,
    render: AudioRenderContract,
    observed: AudioProcessorExecutionContract,
) {
    let spec = configuration_spec(resolver, authored, render);
    assert_eq!(spec.plugin_execution_contract, observed);
    let keys = auxiliary_keys(&spec);
    let ids: Vec<_> = authored.parameters.keys().cloned().collect();
    let mut processor = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("fresh observed factory")
        .create()
        .expect("fresh seed child");
    processor.enter_state(100).expect("fresh state entry");
    for (processed, start) in [100, 104, 108].into_iter().enumerate() {
        let mut audio = configuration_audio(&keys);
        let values = [AudioParameterEvent { sample_offset: 0, value: 0.5 }];
        let parameter_range = 0..1;
        let ranges = if ids.is_empty() {
            &[][..]
        } else {
            std::slice::from_ref(&parameter_range)
        };
        let events = if ids.is_empty() { &[][..] } else { &values[..] };
        processor
            .process(
                configuration_context(start, render.processing_mode),
                &mut audio,
                AudioParameterEventBatch::new(start, 4, &ids, ranges, events),
            )
            .expect("real independently restored seed PCM");
        assert_constant_delay(&audio.main, processed * 4, 7);
    }
    assert_eq!(
        processor.preflight_reprepare().expect("stable final profile"),
        None,
        "seed must not oscillate to an old profile"
    );
}

#[test]
#[ignore = "fresh product helper + existing owned VST3 constant-profile fixture; same-child seed then two fresh Enter PCM oracles"]
fn installed_vst3_configuration_seed_commit_preserves_new_profile_on_fresh_enter() {
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(
        native_path("MONDRIAN_VST3_TEST_HELPER"),
        [native_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")],
    )
    .expect("actual constant-profile catalog");
    let class = &catalog.descriptors()[0].class_id;
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
        let authored = catalog
            .create_instance(class, render, Some(vst3_sidechain_snapshot(0.5, 5)))
            .expect("independent old seed");
        let ids: Vec<_> = authored.parameters.keys().cloned().collect();
        let spec = configuration_spec(&catalog, &authored, render);
        let keys = auxiliary_keys(&spec);
        let mut processor = IsolatedAudioProcessorFactory::prepare(spec)
            .expect("factory")
            .create()
            .expect("same persistent child");
        let cohort = trigger_vst3_configuration(&mut *processor, &keys, &ids, mode);
        let result = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: NonZeroU64::new(71).expect("token"),
                canonical_constant_values: &[0.5],
                configuration_limit_bytes: 128 * 1024,
                control_buffer_grant_bytes: 1024 * 1024,
            })
            .expect("real retained activation/state control");
        let NativeConfigurationControlOutcome::ReadyForInstall(candidate) = result else {
            panic!("actual candidate: {result:?}");
        };
        assert_candidate(
            &candidate,
            cohort,
            NonZeroU64::new(71).expect("token"),
            NativeConfigurationFormat::Vst3ComponentAndController,
        );
        qualify_same_child_seed(&mut *processor, candidate, &keys, &ids, mode);
    }
}

#[test]
#[ignore = "fresh product helper + existing owned CLAP constant-profile fixture; blocked seed stage then two fresh reset PCM oracles"]
fn installed_clap_configuration_seed_commit_preserves_new_profile_on_fresh_enter() {
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(
        native_path("MONDRIAN_CLAP_TEST_HELPER"),
        [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
    )
    .expect("actual constant-profile catalog");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
        let authored = catalog
            .create_instance(
                "org.mondrian.fixture.sidechain",
                render,
                Some(clap_configuration(7, false)),
            )
            .expect("old independent state");
        let (mut processor, keys, cohort) = trigger_clap_configuration(&catalog, &authored, render);
        let result = processor
            .service_native_configuration(NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: NonZeroU64::new(73).expect("token"),
                canonical_constant_values: &[],
                configuration_limit_bytes: 64 * 1024,
                control_buffer_grant_bytes: 128 * 1024,
            })
            .expect("legal actual retained activation/state control");
        let NativeConfigurationControlOutcome::ReadyForInstall(candidate) = result else {
            panic!("actual candidate: {result:?}");
        };
        assert_eq!(candidate.configuration, clap_configuration(7, true));
        assert_candidate(
            &candidate,
            cohort,
            NonZeroU64::new(73).expect("token"),
            NativeConfigurationFormat::ClapState,
        );
        qualify_same_child_seed(&mut *processor, candidate, &keys, &[], mode);
    }
}

fn qualify_same_child_seed(
    processor: &mut dyn AudioProcessor,
    candidate: NativeConfigurationCandidate,
    keys: &[String],
    ids: &[ParameterId],
    mode: AudioProcessingMode,
) {
    let ticket = NativeConfigurationSeedTicket::from_candidate(&candidate);
    let old = candidate.serviced_cohort.request;
    assert_eq!(
        processor
            .commit_native_configuration_seed(ticket)
            .expect("no stage is safe refusal"),
        NativeConfigurationSeedOutcome::Refused(NativeConfigurationSeedRefusal::SeedNotStaged)
    );
    assert_eq!(
        processor
            .stage_native_configuration_seed(NativeConfigurationSeedRequest {
                ticket,
                configuration: &candidate.configuration,
                seed_capacity_grant_bytes: candidate.configuration.len().saturating_sub(1),
            })
            .expect("resource refusal preserves framed connection"),
        NativeConfigurationSeedOutcome::Refused(
            NativeConfigurationSeedRefusal::BufferGrantExceeded
        )
    );
    assert_blocked_retained(processor, keys, 100, mode);
    let mut stale = ticket;
    stale.control_token =
        NonZeroU64::new(ticket.control_token.get() + 1).expect("different service");
    assert_eq!(
        processor
            .stage_native_configuration_seed(NativeConfigurationSeedRequest {
                ticket: stale,
                configuration: &candidate.configuration,
                seed_capacity_grant_bytes: 128 * 1024,
            })
            .expect("stale same-child identity is typed refusal"),
        NativeConfigurationSeedOutcome::Refused(NativeConfigurationSeedRefusal::StaleTicket)
    );
    assert_eq!(
        processor.preflight_reprepare().expect("still retained"),
        Some(crate::NativeObservedRequest::RequestOnly(old))
    );
    let result = processor
        .stage_native_configuration_seed(NativeConfigurationSeedRequest {
            ticket,
            configuration: &candidate.configuration,
            seed_capacity_grant_bytes: 128 * 1024,
        })
        .expect("same actual child seed staging");
    let NativeConfigurationSeedOutcome::Accepted(staged) = result else {
        panic!("exact native seed required, got {result:?}");
    };
    assert_eq!(staged.ticket, ticket);
    assert!(staged.child_seed_capacity_bytes >= candidate.configuration.len());
    assert!(staged.child_seed_capacity_bytes <= 128 * 1024);
    assert_blocked_retained(processor, keys, 100, mode);
    assert_eq!(
        processor
            .commit_native_configuration_seed(stale)
            .expect("old ticket cannot consume another staged owner"),
        NativeConfigurationSeedOutcome::Refused(NativeConfigurationSeedRefusal::SeedNotStaged)
    );
    assert_eq!(
        processor
            .commit_native_configuration_seed(ticket)
            .expect("matching exact seed commit"),
        NativeConfigurationSeedOutcome::Accepted(staged)
    );
    assert_eq!(
        processor.preflight_reprepare().expect("no implicit restart loop"),
        None
    );
    assert_eq!(
        processor
            .commit_native_configuration_seed(ticket)
            .expect("duplicate commit is refused"),
        NativeConfigurationSeedOutcome::Refused(NativeConfigurationSeedRefusal::SeedNotStaged)
    );
    // This explicitly starts fresh DSP. It never resumes the old cursor/history.
    // VST3 actually destroys/reloads its component twice from the installed seed.
    for anchor in [100, -101] {
        processor.enter_state(anchor).expect("new explicit continuity generation");
        for index in 0..3 {
            let start = anchor + index * 4;
            let mut audio = configuration_audio(keys);
            let values = [AudioParameterEvent { sample_offset: 0, value: 0.5 }];
            let parameter_range = 0..1;
            let ranges = if ids.is_empty() {
                &[][..]
            } else {
                std::slice::from_ref(&parameter_range)
            };
            let events = if ids.is_empty() { &[][..] } else { &values[..] };
            processor
                .process(
                    configuration_context(start, mode),
                    &mut audio,
                    AudioParameterEventBatch::new(start, 4, ids, ranges, events),
                )
                .expect("same child new-profile PCM");
            assert_constant_delay(&audio.main, index as usize * 4, 7);
        }
        assert_eq!(
            processor.preflight_reprepare().expect("stable observed new contract"),
            None
        );
    }
}
