use super::*;
use crate::{
    AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime,
    AudioStateEntry, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioSourceComponentId, AutomationSegmentInterpolation, ExactAutomationCurve,
    ExactAutomationKeyframe, ExecutionCancellationToken, TimeScale, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioProcessorInstance, BUILTIN_SAMPLE_DELAY_DEFINITION_ID, SAMPLE_DELAY_FRAMES_PARAMETER_ID,
};
use mondrian_timeline::{Clip, Sequence};
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use vst3_host::audio::{BusDirection, MediaType};
use vst3_host::plugin::ProcessMode;
use vst3_host::Vst3Host;

#[test]
#[ignore = "rebuilt owned VST3 fixture state 4; independent control-thread request oracle"]
fn installed_vst3_legal_latency_restart_reference() {
    let (helper, binary) = restart_paths();
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary.clone()])
        .expect("discover independent latency fixture");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        assert_restart_reference(&binary, &catalog.descriptors()[0].class_id, mode);
    }
}

#[test]
#[ignore = "current product worker + rebuilt owned VST3 fixture state 4; existing API behavior RED"]
fn installed_vst3_latency_restart_invalidates_prepared_contract() {
    let (helper, binary) = restart_paths();
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary.clone()])
        .expect("discover latency fixture");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        // Independently prove this exact binary sends an acknowledged main-thread
        // restart and changes actual PCM only after deactivate/reactivate.
        assert_restart_reference(&binary, &catalog.descriptors()[0].class_id, mode);
        let render = AudioRenderContract { processing_mode: mode, ..test_render_contract() };
        let instance = catalog
            .create_instance(
                &catalog.descriptors()[0].class_id,
                render,
                Some(vst3_sidechain_snapshot(0.5, 4)),
            )
            .expect("restore dynamic latency fixture");
        let ids: Vec<_> = instance.parameters.keys().cloned().collect();
        assert_eq!(ids.len(), 1);
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
            .expect("resolve initial dynamic occurrence");
        assert_eq!(
            spec.plugin_execution_contract.algorithmic_latency_frames(),
            3
        );
        assert_eq!(
            spec.plugin_execution_contract.tail(),
            AudioProcessorTail::Finite(3)
        );
        let keys: Vec<_> =
            spec.auxiliary_inputs.buses.iter().map(|bus| bus.bus_key.clone()).collect();
        assert_eq!(keys, ["vst3.audio-input.1", "vst3.audio-input.2"]);
        let factory = IsolatedAudioProcessorFactory::prepare(spec).expect("prepared child factory");
        let mut processor = factory.create().expect("isolated dynamic occurrence");
        processor.enter_state(73).expect("enter dynamic occurrence");
        let mut initial = restart_io(&keys);
        processor
            .process(
                restart_context(73, mode),
                &mut initial,
                AudioParameterEventBatch::new(73, 4, &ids, std::slice::from_ref(&(0..0)), &[]),
            )
            .expect("unchanged old contract processes normally");
        assert_eq!(initial.main, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.625, -1.3125]);
        let mut changed = restart_io(&keys);
        let outcome = processor.process(
            restart_context(77, mode),
            &mut changed,
            AudioParameterEventBatch::new(
                77,
                4,
                &ids,
                std::slice::from_ref(&(0..1)),
                &[AudioParameterEvent { sample_offset: 0, value: 0.75 }],
            ),
        );
        outcome.expect("the admitted trigger window remains valid under the active old contract");
        assert_eq!(
            changed.main,
            [2.625, -1.3125, 2.625, -1.3125, 2.625, -1.3125, 3.9375, -1.96875]
        );
        let mut next = restart_io(&keys);
        let unpublished = next.main.clone();
        let outcome = processor.process(
            restart_context(81, mode),
            &mut next,
            AudioParameterEventBatch::new(81, 4, &ids, std::slice::from_ref(&(0..0)), &[]),
        );
        let Err(crate::AudioProcessorHostError::ReprepareRequired(request)) = outcome else {
            panic!("expected retained typed native pending, not crash/string failure: {mode:?}, {outcome:?}");
        };
        assert_eq!(
            request.request(),
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
        );
        assert_eq!(
            processor
                .preflight_reprepare()
                .expect("live child replies to actual pure pending IPC"),
            Some(request)
        );
        assert_eq!(
            next.main, unpublished,
            "next-window preflight must not touch unpublished PCM"
        );
        // This RED intentionally uses existing APIs. The production fix must add a
        // recoverable typed outcome and non-RT reprepare, then extend this oracle
        // to retained state, fresh latency/PDC and child liveness. A fatal error
        // alone does not complete the dynamic lifecycle feature.
    }
}

fn restart_paths() -> (PathBuf, PathBuf) {
    (
        std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(PathBuf::from)
            .expect("current production worker helper"),
        std::env::var_os("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("rebuilt owned sidechain fixture"),
    )
}

fn restart_context(start_sample: i64, mode: AudioProcessingMode) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { start_sample, frames: 4 },
        AudioSampleRate::new(48_000).expect("reference sample grid"),
        AudioChannelLayout::Stereo,
        mode,
        AudioKernelBackend::ScalarReference,
    )
}

fn restart_io(keys: &[String]) -> NativeSidechainAudioIo {
    NativeSidechainAudioIo {
        main: [1.0, -0.5].repeat(4),
        inputs: keys
            .iter()
            .zip([0.25, 0.5])
            .map(|(key, sample)| (key.clone(), [sample, -sample * 0.5].repeat(4)))
            .collect(),
    }
}

fn assert_restart_reference(binary: &Path, class: &str, mode: AudioProcessingMode) {
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(16)
        .input_channels(2)
        .output_channels(2)
        .build()
        .expect("owned native reference host");
    let mut plugin = host.load_plugin_class(binary, class).expect("owned native reference class");
    plugin
        .load_state(&vst3_sidechain_snapshot(0.5, 4))
        .expect("restore independent quality fixture");
    plugin
        .set_process_mode(match mode {
            AudioProcessingMode::Realtime => ProcessMode::Realtime,
            AudioProcessingMode::Offline => ProcessMode::Offline,
        })
        .expect("reference render mode");
    for index in 0..3 {
        plugin
            .set_bus_active(MediaType::Audio, BusDirection::Input, index, true)
            .expect("activate reference native input");
    }
    plugin
        .set_bus_active(MediaType::Audio, BusDirection::Output, 0, true)
        .expect("activate reference output");
    plugin.start_processing().expect("start reference DSP");
    assert_eq!(plugin.latency_samples(), 3);
    let mut buffers = plugin.create_bus_audio_buffers(16).expect("reference buffers");
    fill_reference_inputs(&mut buffers);
    plugin.process_bus_audio(&mut buffers).expect("initial old DSP");
    assert_reference_pcm(&buffers.outputs[0].channels, 3, 2.625);
    plugin.set_parameter_at(0, 0.75, 0).expect("main-thread quality control");
    assert_eq!(
        plugin.latency_samples(),
        3,
        "latency changes after legal lifecycle, never during active processing"
    );
    fill_reference_inputs(&mut buffers);
    plugin
        .process_bus_audio(&mut buffers)
        .expect("the active old contract remains valid before lifecycle");
    for (frame, (&left, &right)) in buffers.outputs[0].channels[0]
        .iter()
        .zip(&buffers.outputs[0].channels[1])
        .enumerate()
    {
        let expected = if frame < 3 { 2.625 } else { 3.9375 };
        assert_eq!(
            [left, right],
            [expected, -expected * 0.5],
            "old active latency remains exactly three samples"
        );
    }
    let flags = plugin.service_host_requests().expect("service acknowledged legal restart");
    assert!(
        flags.latency_changed(),
        "fixture must actually send IComponentHandler::restartComponent(kLatencyChanged)"
    );
    assert_eq!(plugin.latency_samples(), 7);
    assert_eq!(plugin.tail_samples(), 7);
    fill_reference_inputs(&mut buffers);
    plugin.process_bus_audio(&mut buffers).expect("new reference DSP");
    assert_reference_pcm(&buffers.outputs[0].channels, 7, 3.9375);
    assert!(
        !plugin.take_restart_flags().latency_changed(),
        "serviced request is consumed once"
    );
    plugin.stop_processing().expect("reference teardown");
}

fn fill_reference_inputs(buffers: &mut vst3_host::BusAudioBuffers) {
    for (bus, sample) in buffers.inputs.iter_mut().zip([1.0, 0.25, 0.5]) {
        bus.channels[0].fill(sample);
        bus.channels[1].fill(-sample * 0.5);
    }
}

fn assert_reference_pcm(channels: &[Vec<f32>], delay: usize, signal: f32) {
    for (frame, (&left, &right)) in channels[0].iter().zip(&channels[1]).enumerate() {
        let expected = if frame < delay { 0.0 } else { signal };
        assert_eq!(
            [left, right],
            [expected, -expected * 0.5],
            "independent reference delay {delay}, frame{frame}"
        );
    }
}

#[test]
#[ignore = "current native worker + rebuilt state4 fixture; nested root-window behavior RED"]
fn installed_vst3_latency_restart_finishes_nested_root_window_before_reprepare() {
    let (helper, binary) = restart_paths();
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let catalog =
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [binary.clone()])
                .expect("discover dynamic nested fixture");
        let render = AudioRenderContract {
            max_block_frames: 8,
            processing_mode: mode,
            ..test_render_contract()
        };
        let mut native = catalog
            .create_instance(
                &catalog.descriptors()[0].class_id,
                render,
                Some(vst3_sidechain_snapshot(0.5, 4)),
            )
            .expect("native child authored state");
        let id = native.parameters.keys().next().expect("fixture gain id").clone();
        let mut curve = ExactAutomationCurve::new(id, 0.5).expect("exact gain curve");
        let mut initial = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.5);
        initial.interpolation_to_next = AutomationSegmentInterpolation::Hold;
        curve.set_keyframe(initial).expect("initial gain");
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(9, 48_000).expect("exact child sample nine"),
                0.75,
            ))
            .expect("quality request inside middle child fill");
        native.set_parameter_automation(curve).expect("author gain automation");

        let mut child = Sequence::new("retimed dynamic native child");
        child.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        let child_asset = AssetId::new();
        child
            .add_media_audio_clip(
                child.audio_tracks[0].id,
                Clip::new(child_asset, TimelineTime::ZERO, TimelineTime::ONE).expect("child clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("child media authoring");
        let pending_instance_id = native.id;
        child.audio_program.outputs[0].strip.pre_fader.processors.push(native);
        let child_output = child.audio_program.outputs[0].id;
        let mut root = Sequence::new("old contract root window");
        root.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        root.add_media_audio_clip(
            root.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("sibling clip"),
            AudioSourceComponentId::primary(),
        )
        .expect("sibling media authoring");
        let scope = root.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        let mut delay = AudioProcessorInstance::built_in(BUILTIN_SAMPLE_DELAY_DEFINITION_ID, 1);
        delay
            .set_parameter_automation(
                ExactAutomationCurve::new(
                    ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID),
                    2.0,
                )
                .expect("sibling delay"),
            )
            .expect("stateful sibling authoring");
        root.audio_program
            .processing_scopes
            .iter_mut()
            .find(|candidate| candidate.id == scope)
            .expect("sibling scope")
            .processors
            .processors
            .push(delay);
        let nested_track = root.add_audio_track();
        let mut nested = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            TimelineTime::ONE,
            Some("three-times child demand".to_owned()),
        )
        .expect("nested clip");
        nested
            .set_constant_source_time_map(
                TimelineTime::ZERO,
                TimeScale::new(3, 1).expect("exact source rate"),
            )
            .expect("retimed child map");
        root.add_nested_audio_clip(nested_track, nested, child_output)
            .expect("nested authoring");
        let reads = Arc::new(AtomicUsize::new(0));
        let child_reads = Arc::new(AtomicUsize::new(0));
        let source = RestartRampResolver {
            reads: reads.clone(),
            child_reads: child_reads.clone(),
            child_asset,
        };
        let resolver = IsolatedAudioProcessorResolver::new(Arc::new(catalog));
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
            &root,
            &[child],
            &source,
            &resolver,
            render,
            None,
        )
        .expect("sole recursively compiled native runtime");
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: 0,
            })
            .expect("root epoch");
        let mut admitted = [99.0; 16];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: 0, frames: 8 },
                &mut admitted,
            )
            .expect("complete trigger root window, including later child fills, remains valid");
        for (sample, pair) in admitted.chunks_exact(2).enumerate() {
            let child_sample = sample * 3;
            let gain = if child_sample < 9 { 0.5 } else { 0.75 };
            // The fixture's native main input coefficient is exactly two.
            let expected = (sample + 1) as f32 + (child_sample + 1) as f32 * 2.0 * gain;
            assert_eq!(
                pair,
                [expected, -expected * 0.5],
                "independent complete root PCM frame {sample}, {mode:?}"
            );
        }
        let reads_before_refusal = reads.load(Ordering::Relaxed);
        assert!(
            child_reads.load(Ordering::Relaxed) >= 3,
            "retiming must demand at least three actual child fills"
        );
        // The preflight must find pending requests throughout the nested closure
        // before any next root DSP/source read. No post-DSP same-window retry.
        let mut retained_ticket = None;
        for _ in 0..2 {
            let mut unpublished = [42.0; 16];
            let outcome = runtime.render_into(
                AudioRenderRequest { start_sample: 8, frames: 8 },
                &mut unpublished,
            );
            let Err(crate::AudioExecutionError::ReprepareRequired(ticket)) = outcome else {
                panic!("expected exact retained native occurrence ticket: {mode:?}, {outcome:?}");
            };
            assert_eq!(ticket.occurrence.instance_id, pending_instance_id);
            assert_eq!(
                ticket.occurrence.owner,
                crate::AudioProcessorOccurrenceOwner::Output(child_output)
            );
            assert_eq!(
                ticket.request.request(),
                mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
            );
            assert_eq!(ticket.continuity_epoch, Some(AudioContinuityEpoch::new(1)));
            if let Some(previous) = retained_ticket.replace(ticket) {
                assert_eq!(
                    previous, ticket,
                    "same exact plan/Session/occurrence retained, not regenerated by preflight"
                );
            }
            assert_eq!(
                unpublished, [42.0; 16],
                "preflight preserves caller-owned PCM"
            );
            assert_eq!(
                reads.load(Ordering::Relaxed),
                reads_before_refusal,
                "neither sibling nor nested sources advance"
            );
        }
    }
}

struct RestartRampResolver {
    reads: Arc<AtomicUsize>,
    child_reads: Arc<AtomicUsize>,
    child_asset: AssetId,
}

impl AudioMediaResolver for RestartRampResolver {
    fn resolve(
        &self,
        asset: AssetId,
        _component: AudioSourceComponentId,
        _sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(RestartRamp {
                reads: self.reads.clone(),
                child_reads: (asset == self.child_asset).then(|| self.child_reads.clone()),
            }),
        ))
    }
}

struct RestartRamp {
    reads: Arc<AtomicUsize>,
    child_reads: Option<Arc<AtomicUsize>>,
}

impl AudioDecodedSource for RestartRamp {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        pcm: &mut [f32],
        _cancel: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if pcm.len() != frames * 2 {
            return Err("reference source extent".to_owned());
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        if let Some(reads) = &self.child_reads {
            reads.fetch_add(1, Ordering::Relaxed);
        }
        for (offset, pair) in pcm.chunks_exact_mut(2).enumerate() {
            let sample = start + offset as i64;
            let value = if sample < 0 { 0.0 } else { (sample + 1) as f32 };
            pair.copy_from_slice(&[value, -value * 0.5]);
        }
        Ok(())
    }
}
