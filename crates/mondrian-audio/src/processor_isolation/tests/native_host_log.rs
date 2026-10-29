use super::*;

fn log_fixture_native_spec(
    mode: AudioProcessingMode,
) -> (IsolatedAudioProcessorWorkerSpec, AudioRenderContract) {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current product helper");
    let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("rebuilt owned CLAP producer");
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("native descriptor discovery");
    let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
    let mut state = vec![10];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    let instance = catalog
        .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
        .expect("actual legal host log producer activation");
    let request = AudioProcessorPrepareRequest::new(
        AudioProcessorOccurrence {
            instance_id: instance.id,
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
        &instance.definition,
        &instance.parameters,
        instance.opaque_state.as_ref().map(AsRef::as_ref),
        render,
    );
    (
        catalog.resolve(request).expect("actual current native spec"),
        render,
    )
}

fn log_fixture_native_pcm(
    processor: &mut dyn AudioProcessor,
    render: AudioRenderContract,
    start: i64,
) {
    let mut io = NativeSidechainAudioIo {
        main: vec![1.0; 8],
        inputs: vec![
            ("clap.audio-input.11".into(), vec![2.0; 8]),
            ("clap.audio-input.7".into(), vec![3.0; 8]),
        ],
    };
    let context = AudioProcessorProcessContext::new(
        AudioRenderRequest {
            transport_activity: None,
            start_sample: start,
            frames: 4,
        },
        AudioSampleRate::new(48_000).expect("rate"),
        AudioChannelLayout::Stereo,
        render.processing_mode,
        AudioKernelBackend::ScalarReference,
    );
    processor.enter_state(start).expect("fresh known continuity");
    processor
        .process(
            context,
            &mut io,
            AudioParameterEventBatch::new(start, 4, &[], &[], &[]),
        )
        .expect("actual logged native process");
    assert_eq!(
        io.main,
        &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 11.5, 11.5],
        "independent gain*(2*main+3*input11+5*input7), latency3; logs cannot alter PCM"
    );
}

#[test]
#[ignore = "current product helper and owned CLAP host-log mode10 fixture; existing native APIs only"]
fn installed_clap_host_log_requires_real_activation_and_unchanged_native_pcm() {
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let (spec, render) = log_fixture_native_spec(mode);
        let factory =
            IsolatedAudioProcessorFactory::prepare(spec).expect("actual host-log native factory");
        let mut processor = factory.create(None).expect("exclusive actual native child");
        log_fixture_native_pcm(&mut *processor, render, -101);
    }
}

#[test]
#[ignore = "matching current V6 helper and rebuilt owned CLAP log producer"]
fn installed_clap_host_log_delivers_main_background_audio_records_on_one_authenticated_poll() {
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let (spec, render) = log_fixture_native_spec(mode);
        let factory = IsolatedAudioProcessorFactory::prepare(spec).expect("native log factory");
        assert_eq!(factory.execution_contract().algorithmic_latency_frames(), 3);
        let mut processor = factory.create(None).expect("same exclusive child");
        let object = processor
            .native_audio_object_observation()
            .expect("CLAP capability does not consume logs")
            .expect("current CLAP adapter exposes its actual retained native object");
        assert_ne!(object.process_id().get(), std::process::id());
        assert_eq!(
            object.object_serial().get(),
            1,
            "one initial load in this fresh exclusive child"
        );
        let activation = processor
            .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
            .expect("same canonical Poll9 reply");
        let pid = activation.owner_process_id().expect("actual child producer PID");
        assert_ne!(pid.get(), std::process::id());
        assert_eq!(
            pid,
            object.process_id(),
            "native load and logs belong to the authenticated child"
        );
        assert_eq!(activation.object, Some(object));
        assert_eq!(activation.logs.records().len(), 14);
        assert!(!activation.logs.incomplete());
        for (index, record) in activation.logs.records().iter().enumerate() {
            assert_eq!(record.raw_severity(), (index % 7) as i32);
            assert_eq!(record.sequence(), index as u64);
            assert_eq!(
                record.text(),
                Ok(if index < 7 {
                    "log:activate"
                } else {
                    "log:background"
                })
            );
        }
        for (index, anchor) in [0, -101, 73].into_iter().enumerate() {
            log_fixture_native_pcm(&mut *processor, render, anchor);
            let preserved = processor
                .poll_native_observations(crate::NativeAudioObservationConsumption::Preserve)
                .expect("nonconsuming capability facts");
            assert_eq!(preserved.owner_process_id(), Some(pid));
            assert_eq!(
                preserved.object,
                Some(object),
                "same native load after CLAP reset/seek"
            );
            assert!(preserved.logs.records().is_empty());
            let audio = processor
                .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
                .expect("same child bounded records");
            assert_eq!(audio.owner_process_id(), Some(pid));
            assert_eq!(audio.object, Some(object));
            assert_eq!(audio.logs.records().len(), 7);
            assert!(!audio.logs.incomplete());
            for (severity, record) in audio.logs.records().iter().enumerate() {
                assert_eq!(record.raw_severity(), severity as i32);
                assert_eq!(record.sequence(), (14 + index * 7 + severity) as u64);
                assert_eq!(record.text(), Ok("log:process"));
                assert!(!record.truncated() && !record.invalid_utf8());
            }
            let drained = processor
                .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
                .expect("same connection after drain");
            assert_eq!(drained.owner_process_id(), Some(pid));
            assert_eq!(drained.object, Some(object));
            assert!(drained.logs.records().is_empty());
        }
    }
}

struct HostLogConstantResolver;
impl crate::AudioMediaResolver for HostLogConstantResolver {
    fn resolve(
        &self,
        _asset: mondrian_core::AssetId,
        _component: mondrian_core::AudioSourceComponentId,
        _rate: u32,
    ) -> Result<crate::ResolvedAudioSource, String> {
        Ok(crate::ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(HostLogConstantSource),
        ))
    }
}
struct HostLogConstantSource;
impl crate::AudioDecodedSource for HostLogConstantSource {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        pcm: &mut [f32],
        _cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), String> {
        if pcm.len() != frames * 2 {
            return Err("independent constant source extent".to_owned());
        }
        for (offset, pair) in pcm.chunks_exact_mut(2).enumerate() {
            let sample = if start + (offset as i64) < 0 {
                0.0
            } else {
                1.0
            };
            pair.copy_from_slice(&[sample, sample]);
        }
        Ok(())
    }
}

#[test]
#[ignore = "matching actual V6 product helper + owned CLAP log mode10 fixture; whole canonical root/PDC/control"]
fn installed_clap_host_log_whole_root_projects_exact_source_and_original_pcm_on_one_lane() {
    use crate::{AudioContinuityEpoch, AudioProgramRuntime, AudioStateEntry};
    use mondrian_core::{
        AssetId, AudioSourceComponentId, ExecutionCancellationToken, TimelineTime,
    };
    use mondrian_timeline::{Clip, Sequence};
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current product helper");
    let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("rebuilt owned CLAP log producer");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let catalog =
            DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
                .expect("actual native discovery");
        let render = AudioRenderContract {
            max_block_frames: 16,
            processing_mode: mode,
            ..test_render_contract()
        };
        let mut state = vec![10];
        state.extend_from_slice(&0.5_f32.to_le_bytes());
        let native = catalog
            .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
            .expect("actual authored native instance");
        let instance_id = native.id;
        let mut sequence = Sequence::new("host-log canonical public root");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        sequence
            .add_media_audio_clip(
                sequence.audio_tracks[0].id,
                Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE)
                    .expect("exact public clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("canonical clip authoring");
        let output = sequence.audio_program.outputs[0].id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(native);
        let resolver = IsolatedAudioProcessorResolver::new(Arc::new(catalog));
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
            &sequence,
            &[],
            &HostLogConstantResolver,
            &resolver,
            render,
            None,
        )
        .expect("sole canonical compiler/prepared PDC/actual native Session");
        assert!(runtime.native_observations_supported());
        let cancel = ExecutionCancellationToken::new();
        let activation = runtime
            .poll_native_observations(&cancel)
            .expect("one exact occurrence on same retained child")
            .expect("main/background activation logs")
            .diagnostics
            .expect("actual diagnostic projection");
        assert_eq!(activation.records().len(), 14);
        assert!(!activation.loss().incomplete());
        let source = activation.source();
        assert_eq!(source.instance, instance_id);
        assert_ne!(source.process_id.get(), std::process::id());
        assert_eq!(source.root_plan, source.local_plan);
        assert_eq!(source.root_session, source.local_session);
        assert_eq!(
            source.location,
            mondrian_core::NativeAudioDiagnosticLocation::Output(output)
        );
        assert_eq!(
            source.insertion,
            mondrian_core::NativeAudioDiagnosticInsertion::PreFader
        );
        for (index, record) in activation.records().iter().enumerate() {
            assert_eq!(record.raw_severity(), (index % 7) as i32);
            assert_eq!(record.sequence(), index as u64);
        }
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(41),
                start_sample: 0,
            })
            .expect("actual generation entry");
        let mut pcm = [99.0; 16];
        runtime
            .render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 8,
                },
                &mut pcm,
            )
            .expect("whole actual root PCM");
        // Sole native main coefficient2 * authored gain0.5; unrouted aux zero.
        // Canonical PDC removes the real three-sample native latency.
        assert_eq!(
            pcm, [1.0; 16],
            "independent constant main input, native equation, public latency-aligned PCM"
        );
        let audio = runtime
            .poll_native_observations(&cancel)
            .expect("same single occurrence after native DSP")
            .expect("actual audio-role logs")
            .diagnostics
            .expect("actual diagnostic projection");
        assert_eq!(audio.source(), source);
        assert!(!audio.loss().incomplete());
        assert!(!audio.records().is_empty() && audio.records().len().is_multiple_of(7));
        for (index, record) in audio.records().iter().enumerate() {
            assert_eq!(record.raw_severity(), (index % 7) as i32);
            assert_eq!(record.sequence(), 14 + index as u64);
            assert_eq!(record.bytes(), b"log:process");
        }
        assert!(runtime
            .poll_native_observations(&cancel)
            .expect("no second queue/store")
            .is_none());
    }
}
