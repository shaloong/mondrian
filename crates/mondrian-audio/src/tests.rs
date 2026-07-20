use super::*;
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioChannelMixEntry, AudioChannelMixMatrix, AudioChannelPosition,
    AudioComponentEditId, AudioSourceComponentId, AutomationSegmentInterpolation,
    ExactAutomationCurve, ExactAutomationKeyframe, ExactBezierHandle, ExecutionCancellationToken,
    ParameterId, TimeScale, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioComponentChannelMapping, AudioMixBus, AudioProcessorInstance,
    AudioRoute, AudioRouteDestination, AudioRouteSource, BUILTIN_GAIN_DEFINITION_ID,
    BUILTIN_SAMPLE_DELAY_DEFINITION_ID, GAIN_DB_PARAMETER_ID, ROUTE_GAIN_DB_PARAMETER_ID,
    SAMPLE_DELAY_FRAMES_PARAMETER_ID,
};
use mondrian_timeline::{Clip, Sequence};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
struct RampSource {
    block_reads: usize,
}

impl AudioPcmSource for RampSource {
    fn read_indexed_interleaved(
        &mut self,
        _edit: AudioComponentEditId,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let channels = channel_layout.channel_count();
        self.block_reads = self.block_reads.saturating_add(1);
        for (frame, source_frame) in source_frames.iter().copied().enumerate() {
            let value = if source_frame < 0 {
                0.0
            } else {
                source_frame as f32 + 1.0
            };
            destination[frame * channels..(frame + 1) * channels].fill(value);
        }
        Ok(())
    }
}

struct FailingSource;

impl AudioPcmSource for FailingSource {
    fn read_indexed_interleaved(
        &mut self,
        _edit: AudioComponentEditId,
        _source_frames: &[i64],
        _channel_layout: AudioChannelLayout,
        _destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        Err(AudioExecutionError::SourceUnavailable(
            "injected source failure".to_owned(),
        ))
    }
}

struct TestDelayProcessorResolver {
    latency_frames: usize,
    realtime_capable: bool,
    fail_first_state_entry: bool,
}

impl AudioProcessorResolver for TestDelayProcessorResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if !matches!(
            request.definition(),
            mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. }
                if plugin_id == "test.mondrian.delay"
        ) {
            return Err(AudioProcessorHostError::Unavailable(
                "test resolver only realizes test.mondrian.delay".to_owned(),
            ));
        }
        if !matches!(
            request.occurrence(),
            AudioProcessorOccurrence {
                owner: AudioProcessorOccurrenceOwner::Bus(_),
                insertion: AudioProcessorInsertionPoint::PreFader,
                ..
            }
        ) {
            return Err(AudioProcessorHostError::InvalidContract(
                "test Delay must be realized on a Bus pre-fader rack".to_owned(),
            ));
        }
        if !request.parameters().is_empty() || request.opaque_state().is_some() {
            return Err(AudioProcessorHostError::InvalidContract(
                "test Delay requires no author parameters or state chunk".to_owned(),
            ));
        }
        let scratch_samples = self
            .latency_frames
            .checked_mul(request.render_contract().channel_count())
            .ok_or_else(|| {
                AudioProcessorHostError::InvalidContract(
                    "test Delay scratch capacity overflowed".to_owned(),
                )
            })?;
        let scratch_bytes =
            scratch_samples.checked_mul(std::mem::size_of::<f32>()).ok_or_else(|| {
                AudioProcessorHostError::InvalidContract(
                    "test Delay scratch bytes overflowed".to_owned(),
                )
            })?;
        let contract = AudioProcessorExecutionContract::new(
            self.latency_frames,
            AudioProcessorTail::None,
            self.latency_frames > 0,
            self.realtime_capable,
            true,
            scratch_bytes,
        )?;
        Ok(Arc::new(TestDelayProcessorFactory {
            contract,
            delay_samples: scratch_samples,
            fail_first_state_entry: self.fail_first_state_entry,
        }))
    }
}

struct TestDelayProcessorFactory {
    contract: AudioProcessorExecutionContract,
    delay_samples: usize,
    fail_first_state_entry: bool,
}

impl AudioProcessorFactory for TestDelayProcessorFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        Ok(Box::new(TestDelayProcessor {
            samples: vec![0.0; self.delay_samples],
            cursor: 0,
            fail_first_state_entry: self.fail_first_state_entry,
        }))
    }
}

struct TestDelayProcessor {
    samples: Vec<f32>,
    cursor: usize,
    fail_first_state_entry: bool,
}

impl AudioProcessor for TestDelayProcessor {
    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        if self.fail_first_state_entry {
            self.fail_first_state_entry = false;
            return Err(AudioProcessorHostError::StateEntry(
                "injected first-entry failure".to_owned(),
            ));
        }
        self.samples.fill(0.0);
        self.cursor = 0;
        Ok(())
    }

    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        _parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let expected = context
            .request()
            .frames
            .checked_mul(context.channel_layout().channel_count())
            .ok_or_else(|| {
                AudioProcessorHostError::Process("test Delay block overflowed".to_owned())
            })?;
        if audio.main_layout() != context.channel_layout()
            || audio.frames() != context.request().frames
            || audio.main_interleaved().len() != expected
        {
            return Err(AudioProcessorHostError::Process(
                "test Delay received a mismatched main bus".to_owned(),
            ));
        }
        if self.samples.is_empty() {
            return Ok(());
        }
        for sample in audio.main_interleaved() {
            std::mem::swap(sample, &mut self.samples[self.cursor]);
            self.cursor += 1;
            if self.cursor == self.samples.len() {
                self.cursor = 0;
            }
        }
        Ok(())
    }
}

struct RampDecodedSource;

impl AudioDecodedSource for RampDecodedSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        _cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        let channels = 1;
        if destination.len() != frames {
            return Err("invalid destination extent".to_owned());
        }
        for frame in 0..frames {
            let value = start_frame.saturating_add(frame as i64).max(0) as f32 + 1.0;
            destination[frame * channels..(frame + 1) * channels].fill(value);
        }
        Ok(())
    }
}

struct RampResolver;

impl AudioMediaResolver for RampResolver {
    fn resolve(
        &self,
        _asset_id: AssetId,
        _component_id: AudioSourceComponentId,
        _sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Mono,
            Arc::new(RampDecodedSource),
        ))
    }
}

struct StereoRampDecodedSource;

impl AudioDecodedSource for StereoRampDecodedSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        _cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if destination.len() != frames.saturating_mul(2) {
            return Err("invalid stereo destination extent".to_owned());
        }
        for frame in 0..frames {
            let value = start_frame.saturating_add(frame as i64).max(0) as f32 + 1.0;
            destination[frame * 2] = value;
            destination[frame * 2 + 1] = -value;
        }
        Ok(())
    }
}

struct StereoRampResolver;

impl AudioMediaResolver for StereoRampResolver {
    fn resolve(
        &self,
        _asset_id: AssetId,
        _component_id: AudioSourceComponentId,
        _sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(StereoRampDecodedSource),
        ))
    }
}

fn tt(numerator: i64, denominator: i64) -> TimelineTime {
    TimelineTime::new(numerator, denominator).expect("valid time")
}

fn gain_curve(start_db: f64, end_db: f64) -> ExactAutomationCurve {
    let parameter = ParameterId::new(GAIN_DB_PARAMETER_ID).expect("gain parameter");
    let mut curve = ExactAutomationCurve::new(parameter, start_db).expect("curve");
    curve
        .set_keyframe(ExactAutomationKeyframe::linear(
            TimelineTime::ZERO,
            start_db,
        ))
        .expect("first key");
    curve
        .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ONE, end_db))
        .expect("second key");
    curve
}

fn route_gain_curve(start_db: f64, end_db: f64) -> ExactAutomationCurve {
    let parameter = ParameterId::new(ROUTE_GAIN_DB_PARAMETER_ID).expect("Route gain parameter");
    let mut curve = ExactAutomationCurve::new(parameter, start_db).expect("curve");
    curve
        .set_keyframe(ExactAutomationKeyframe::linear(
            TimelineTime::ZERO,
            start_db,
        ))
        .expect("first key");
    curve
        .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ONE, end_db))
        .expect("second key");
    curve
}

fn hold_then_bezier_gain_curve() -> ExactAutomationCurve {
    let parameter = ParameterId::new(GAIN_DB_PARAMETER_ID).expect("gain parameter");
    let mut curve = ExactAutomationCurve::new(parameter, 0.0).expect("curve");
    let mut first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
    first.interpolation_to_next = AutomationSegmentInterpolation::Hold;
    let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 6.0);
    middle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
    middle.out_handle = Some(ExactBezierHandle { time_offset: tt(1, 4), value_offset: -1.5 });
    let mut last = ExactAutomationKeyframe::linear(tt(2, 1), 0.0);
    last.in_handle = Some(ExactBezierHandle { time_offset: tt(-1, 4), value_offset: 1.5 });
    for keyframe in [first, middle, last] {
        curve.set_keyframe(keyframe).expect("valid prepared event key");
    }
    curve
}

fn sequence_with_audio_clip() -> Sequence {
    let mut sequence = Sequence::new("audio");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track_id = sequence.audio_tracks[0].id;
    let clip =
        Clip::new(mondrian_core::AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("clip");
    sequence
        .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
        .expect("authored audio Clip");
    sequence
}

fn sequence_with_parallel_hosted_delay() -> Sequence {
    sequence_with_parallel_processor(AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
            plugin_id: "test.mondrian.delay".to_owned(),
            schema_version: 1,
        },
        bypassed: false,
        parameters: BTreeMap::new(),
        opaque_state: None,
    })
}

fn sequence_with_parallel_sample_delay(delay_frames: i64) -> Sequence {
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_SAMPLE_DELAY_DEFINITION_ID, 1);
    processor
        .set_parameter_automation(
            ExactAutomationCurve::new(
                ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID),
                delay_frames as f64,
            )
            .expect("exact sample delay"),
        )
        .expect("valid sample delay");
    sequence_with_parallel_processor(processor)
}

fn sequence_with_scope_sample_delay(
    position: TimelineTime,
    duration: TimelineTime,
    delay_frames: i64,
) -> Sequence {
    let mut sequence = Sequence::new("scope delay");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track_id = sequence.audio_tracks[0].id;
    let clip = Clip::new(AssetId::new(), position, duration).expect("Clip");
    sequence
        .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
        .expect("authored audio Clip");
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_SAMPLE_DELAY_DEFINITION_ID, 1);
    processor
        .set_parameter_automation(
            ExactAutomationCurve::new(
                ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID),
                delay_frames as f64,
            )
            .expect("exact sample delay"),
        )
        .expect("valid sample delay");
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("processing Scope")
        .processors
        .processors
        .push(processor);
    sequence
}

fn sequence_with_parallel_processor(processor: AudioProcessorInstance) -> Sequence {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let output_id = sequence.audio_program.outputs[0].id;
    let bus_id = mondrian_core::MixBusId::new();
    let mut strip = mondrian_timeline::AudioChannelStrip::default();
    strip.pre_fader.processors.push(processor);
    sequence.audio_program.buses.push(AudioMixBus {
        id: bus_id,
        name: "Delayed parallel".to_owned(),
        strip,
    });
    sequence.audio_program.routes.clear();
    sequence.audio_program.routes.extend([
        AudioRoute::new(
            AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        ),
        AudioRoute::new(
            AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(bus_id),
        ),
        AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        ),
    ]);
    sequence
}

fn prepared(sequence: &Sequence, max_frames: usize) -> Arc<PreparedAudioPlan> {
    prepared_with_backend(sequence, max_frames, AudioKernelBackend::RuntimeVectorized)
}

fn prepared_with_backend(
    sequence: &Sequence,
    max_frames: usize,
    backend: AudioKernelBackend,
) -> Arc<PreparedAudioPlan> {
    let output = sequence.audio_program.outputs[0].id;
    let compiled = compile_audio_program(sequence, AudioCompileRequest::program(output))
        .expect("compiled program");
    Arc::new(
        PreparedAudioPlan::prepare_with_backend(
            Arc::new(compiled),
            AudioRenderContract {
                sample_rate: 2,
                channel_layout: AudioChannelLayout::Mono,
                max_block_frames: max_frames,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            },
            backend,
        )
        .expect("prepared plan"),
    )
}

#[test]
fn compiler_derives_track_and_range_from_real_clip_placement() {
    let sequence = sequence_with_audio_clip();
    let clip = &sequence.audio_tracks[0].clips[0];
    let output = sequence.audio_program.outputs[0].id;
    let compiled =
        compile_audio_program(&sequence, AudioCompileRequest::program(output)).expect("compiled");
    assert_eq!(compiled.contributions().len(), 1);
    assert_eq!(compiled.contributions()[0].clip_id, clip.id);
    assert_eq!(
        compiled.contributions()[0].track_id,
        sequence.audio_tracks[0].id
    );
    assert_eq!(
        compiled.contributions()[0].sequence_range.start,
        clip.position
    );
    assert_eq!(
        compiled.contributions()[0].sequence_range.duration,
        clip.duration
    );
}

#[test]
fn source_adapter_is_crossed_once_per_contribution_block() {
    let sequence = sequence_with_audio_clip();
    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("block render");

    assert_eq!(source.block_reads, 1);
    assert_eq!(pcm, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn session_executes_preallocated_contribution_and_route_compensation_block_invariant() {
    fn plan_with_compensation(sequence: &Sequence) -> Arc<PreparedAudioPlan> {
        let mut plan = prepared(sequence, 12);
        let prepared = Arc::make_mut(&mut plan);
        let contribution_track_slot = prepared.schedule.contributions[0].track_slot;
        let contribution_route = prepared
            .schedule
            .routes
            .iter()
            .position(|route| route.source_slot == contribution_track_slot)
            .expect("Contribution Track route");
        prepared.schedule.contributions[0].compensation_delay_frames = 2;
        prepared.schedule.routes[contribution_route].compensation_delay_frames = 2;
        prepared.schedule.summary.maximum_compensation_frames = 2;
        prepared.schedule.summary.output_latency_frames = 4;
        prepared.schedule.summary.requires_state_entry = true;
        plan
    }

    let sequence = sequence_with_audio_clip();
    let whole_plan = plan_with_compensation(&sequence);
    let mut whole_session = AudioRenderSession::new(whole_plan).expect("whole Session");
    assert!(whole_session.requires_state_entry());
    assert_eq!(whole_session.capacity().compensation_delay_line_count, 2);
    assert_eq!(whole_session.capacity().compensation_delay_samples, 4);
    let mut whole_source = RampSource::default();
    let mut whole = vec![0.0; 12];
    let unentered = whole_session
        .render_into(
            &mut whole_source,
            AudioRenderRequest { start_sample: 0, frames: 12 },
            &mut whole,
        )
        .expect_err("stateful Session must reject unentered execution");
    assert_eq!(unentered, AudioExecutionError::StateEntryRequired);
    let whole_epoch = AudioContinuityEpoch::new(7);
    whole_session
        .enter_state(AudioStateEntry { epoch: whole_epoch, start_sample: 0 })
        .expect("whole state entry");
    assert!(matches!(
        whole_session.enter_state(AudioStateEntry { epoch: whole_epoch, start_sample: 0 }),
        Err(AudioExecutionError::ReusedContinuityEpoch(epoch)) if epoch == whole_epoch
    ));
    whole_session
        .render_into(
            &mut whole_source,
            AudioRenderRequest { start_sample: 0, frames: 12 },
            &mut whole,
        )
        .expect("whole render");

    let split_plan = plan_with_compensation(&sequence);
    let mut split_session = AudioRenderSession::new(split_plan).expect("split Session");
    let split_epoch = AudioContinuityEpoch::new(8);
    split_session
        .enter_state(AudioStateEntry { epoch: split_epoch, start_sample: 0 })
        .expect("split state entry");
    let mut split_source = RampSource::default();
    let mut split = Vec::new();
    for start_sample in [0, 2, 4, 6, 8, 10] {
        let mut block = vec![0.0; 2];
        split_session
            .render_into(
                &mut split_source,
                AudioRenderRequest { start_sample, frames: 2 },
                &mut block,
            )
            .expect("split render");
        split.extend(block);
    }

    assert_eq!(
        whole,
        [0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]
    );
    assert_eq!(split, whole);

    assert!(matches!(
        split_session.render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 14, frames: 2 },
            &mut [0.0; 2],
        ),
        Err(AudioExecutionError::NonContiguousBlock {
            epoch,
            expected_start_sample: 12,
            actual_start_sample: 14,
        }) if epoch == split_epoch
    ));
    split_session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(9),
            start_sample: 4,
        })
        .expect("fresh discontinuity entry");
    let mut reset_output = [1.0; 2];
    split_session
        .render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 4, frames: 2 },
            &mut reset_output,
        )
        .expect("reset render");
    assert_eq!(reset_output, [0.0, 0.0]);

    let poisoned_epoch = AudioContinuityEpoch::new(10);
    split_session
        .enter_state(AudioStateEntry { epoch: poisoned_epoch, start_sample: 6 })
        .expect("failure entry");
    assert!(matches!(
        split_session.render_into(
            &mut FailingSource,
            AudioRenderRequest { start_sample: 6, frames: 2 },
            &mut [0.0; 2],
        ),
        Err(AudioExecutionError::SourceUnavailable(_))
    ));
    assert_eq!(
        split_session
            .render_into(
                &mut split_source,
                AudioRenderRequest { start_sample: 6, frames: 2 },
                &mut [0.0; 2],
            )
            .expect_err("partially advanced epoch must stay poisoned"),
        AudioExecutionError::ContinuityPoisoned(poisoned_epoch)
    );
}

#[test]
fn prepared_source_schedule_preserves_fractional_forward_retime() {
    let mut sequence = sequence_with_audio_clip();
    let clip = &mut sequence.audio_tracks[0].clips[0];
    clip.source_in = tt(1, 4);
    clip.speed.set_scale(TimeScale::new(3, 2).expect("exact forward scale"));

    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("fractional forward render");

    assert_eq!(source.block_reads, 1);
    assert_eq!(pcm, vec![1.0, 3.0, 4.0, 6.0]);
}

#[test]
fn prepared_source_schedule_preserves_fractional_reverse_retime() {
    let mut sequence = sequence_with_audio_clip();
    let clip = &mut sequence.audio_tracks[0].clips[0];
    clip.source_in = tt(3, 1);
    clip.speed.set_scale(TimeScale::new(-1, 2).expect("exact reverse scale"));

    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("fractional reverse render");

    assert_eq!(source.block_reads, 1);
    assert_eq!(pcm, vec![7.0, 6.0, 6.0, 5.0]);
}

#[test]
fn clip_track_bus_output_math_is_unclipped_and_block_invariant() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let output_id = sequence.audio_program.outputs[0].id;
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let curve = gain_curve(0.0, 6.0);
    processor.set_parameter_automation(curve).expect("valid gain automation");
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("scope")
        .processors
        .processors
        .push(processor);

    sequence.audio_program.routes.clear();
    let bus_id = mondrian_core::MixBusId::new();
    sequence.audio_program.buses.push(AudioMixBus {
        id: bus_id,
        name: "Bus".to_owned(),
        strip: mondrian_timeline::AudioChannelStrip::default(),
    });
    sequence.audio_program.routes.extend([
        AudioRoute::new(
            AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(bus_id),
        ),
        AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        ),
    ]);

    let plan = prepared(&sequence, 8);
    assert_eq!(
        plan.schedule_summary(),
        PreparedAudioScheduleSummary {
            node_count: 3,
            track_count: 1,
            bus_count: 1,
            contribution_count: 1,
            maximum_source_channels: 1,
            channel_mix_coefficient_count: 1,
            route_count: 2,
            transition_binding_count: 0,
            automation_curve_count: 1,
            automation_event_span_count: 2,
            processor_occurrence_count: 1,
            processor_parameter_lane_count: 1,
            maximum_parameter_events_per_block: 8,
            processor_session_scratch_bytes: 96,
            scratch_slot_count: 2,
            output_latency_frames: 0,
            maximum_compensation_frames: 0,
            requires_state_entry: false,
        }
    );
    let mut source = RampSource::default();
    let whole = render_audio(
        Arc::clone(&plan),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("whole");
    let first = render_audio(
        Arc::clone(&plan),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 2 },
    )
    .expect("first");
    let second = render_audio(
        plan,
        &mut source,
        AudioRenderRequest { start_sample: 2, frames: 2 },
    )
    .expect("second");
    assert_eq!(whole, [first, second].concat());
    assert!(
        whole[2] > 3.0,
        "internal float PCM must not be clipped or tanh-shaped"
    );
}

#[test]
fn parallel_route_is_a_sample_accurate_block_invariant_send() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let output_id = sequence.audio_program.outputs[0].id;
    let bus_id = mondrian_core::MixBusId::new();
    sequence.audio_program.buses.push(AudioMixBus {
        id: bus_id,
        name: "Parallel".to_owned(),
        strip: mondrian_timeline::AudioChannelStrip::default(),
    });
    sequence.audio_program.routes.clear();
    let direct = AudioRoute::new(
        AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        AudioRouteDestination::Output(output_id),
    );
    let curve = route_gain_curve(0.0, -6.020_599_913_279_624);
    let mut send = AudioRoute::new(
        AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        AudioRouteDestination::Bus(bus_id),
    );
    send.gain_automation = Some(curve.clone());
    let return_route = AudioRoute::new(
        AudioRouteSource::Bus {
            bus_id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        AudioRouteDestination::Output(output_id),
    );
    sequence.audio_program.routes.extend([direct, send, return_route]);

    let runtime_plan = prepared_with_backend(&sequence, 4, AudioKernelBackend::RuntimeVectorized);
    assert_eq!(runtime_plan.schedule_summary().route_count, 3);
    assert_eq!(runtime_plan.schedule_summary().automation_curve_count, 1);
    assert_eq!(
        runtime_plan.schedule_summary().automation_event_span_count,
        2
    );
    let scalar_plan = prepared_with_backend(&sequence, 4, AudioKernelBackend::ScalarReference);

    let mut runtime_source = RampSource::default();
    let runtime = render_audio(
        runtime_plan,
        &mut runtime_source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("runtime send");
    let mut scalar_source = RampSource::default();
    let scalar = render_audio(
        scalar_plan,
        &mut scalar_source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("scalar send");

    for (sample, (runtime, scalar)) in runtime.iter().zip(&scalar).enumerate() {
        let time = tt(i64::try_from(sample).expect("sample index"), 2);
        let send_gain = dsp::db_to_linear(curve.evaluate(time).expect("Route automation"));
        let source = sample as f32 + 1.0;
        let expected = source * (1.0 + send_gain);
        assert!((runtime - expected).abs() <= 1.0e-6);
        assert!((scalar - expected).abs() <= 1.0e-6);
    }

    let split_plan = prepared(&sequence, 2);
    let mut split_source = RampSource::default();
    let first = render_audio(
        Arc::clone(&split_plan),
        &mut split_source,
        AudioRenderRequest { start_sample: 0, frames: 2 },
    )
    .expect("first partition");
    let second = render_audio(
        split_plan,
        &mut split_source,
        AudioRenderRequest { start_sample: 2, frames: 2 },
    )
    .expect("second partition");
    assert_eq!([first, second].concat(), runtime);
}

#[test]
fn disabled_route_is_absent_from_the_compiled_signal_closure() {
    let mut sequence = sequence_with_audio_clip();
    for route in &mut sequence.audio_program.routes {
        route.enabled = false;
    }
    let plan = prepared(&sequence, 4);

    assert_eq!(plan.schedule_summary().track_count, 0);
    assert_eq!(plan.schedule_summary().contribution_count, 0);
    assert_eq!(plan.schedule_summary().route_count, 0);
    let mut source = RampSource::default();
    let rendered = render_audio(
        plan,
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("disabled Route");
    assert_eq!(rendered, vec![0.0; 4]);
    assert_eq!(source.block_reads, 0);
}

#[test]
fn prepared_automation_event_spans_preserve_hold_bezier_and_block_partitioning() {
    let mut sequence = sequence_with_audio_clip();
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let curve = hold_then_bezier_gain_curve();
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    processor
        .set_parameter_automation(curve.clone())
        .expect("valid gain automation");
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("scope")
        .processors
        .processors
        .push(processor);

    let plan = prepared(&sequence, 6);
    let summary = plan.schedule_summary();
    assert_eq!(summary.automation_curve_count, 1);
    assert_eq!(summary.automation_event_span_count, 3);

    let mut source = RampSource::default();
    let whole = render_audio(
        Arc::clone(&plan),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 6 },
    )
    .expect("whole prepared automation render");
    let mut partitioned = Vec::new();
    for start_sample in [0, 2, 4] {
        partitioned.extend(
            render_audio(
                Arc::clone(&plan),
                &mut source,
                AudioRenderRequest { start_sample, frames: 2 },
            )
            .expect("partitioned prepared automation render"),
        );
    }
    assert_eq!(whole, partitioned);

    let expected = (0..6)
        .map(|sample| {
            let gain_db = curve.evaluate(tt(sample, 2)).expect("reference curve");
            (sample as f32 + 1.0) * (10.0_f64.powf(gain_db / 20.0) as f32)
        })
        .collect::<Vec<_>>();
    assert_eq!(whole, expected);

    let mut session = AudioRenderSession::new(plan).expect("preallocated realtime Session");
    let capacity = session.capacity();
    let mut destination = vec![0.0; capacity.max_block_frames * capacity.channels];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: capacity.max_block_frames },
            &mut destination,
        )
        .expect("render within fixed allocation envelope");
    assert_eq!(session.capacity(), capacity);
}

#[test]
fn processor_parameter_batches_are_sample_accurate_and_preallocated() {
    let mut sequence = sequence_with_audio_clip();
    let scope = sequence.audio_program.processing_scopes.first_mut().expect("scope");
    let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    processor
        .set_parameter_automation(gain_curve(0.0, 6.0))
        .expect("gain automation");
    scope.processors.processors.push(processor);

    let plan = prepared(&sequence, 8);
    assert_eq!(plan.schedule_summary().processor_occurrence_count, 1);
    assert_eq!(plan.schedule_summary().processor_parameter_lane_count, 1);
    assert_eq!(
        plan.schedule_summary().maximum_parameter_events_per_block,
        8
    );
    let mut session = AudioRenderSession::new(plan).expect("Session");
    assert_eq!(session.capacity().processor_occurrences, 1);
    assert_eq!(session.capacity().maximum_processor_parameter_lanes, 1);
    assert_eq!(session.capacity().parameter_event_capacity, 8);
    assert_eq!(session.capacity().processor_session_scratch_bytes, 96);
    assert_eq!(session.capacity().meter_channel_state_count, 1);

    let lanes = session
        .processor_parameter_events_for_test(0, AudioRenderRequest { start_sample: 0, frames: 4 })
        .expect("parameter batch");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].0.as_str(), GAIN_DB_PARAMETER_ID);
    assert_eq!(
        lanes[0].1.iter().map(|event| event.sample_offset).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        lanes[0].1.iter().map(|event| event.value).collect::<Vec<_>>(),
        vec![0.0, 3.0, 6.0, 6.0]
    );
}

#[test]
fn shared_scope_definition_materializes_independent_processor_occurrences() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let shared_scope_id = sequence.audio_program.processing_scopes[0].id;
    let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let processor_id = processor.id;
    sequence.audio_program.processing_scopes[0]
        .processors
        .processors
        .push(processor);

    let second_clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("second Clip");
    let second_clip_id = sequence
        .add_media_audio_clip(track_id, second_clip, AudioSourceComponentId::primary())
        .expect("second audio Clip");
    let second_edit_id = {
        let clip = sequence.audio_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == second_clip_id)
            .expect("second Clip");
        clip.audio_components[0].processing.scope_id = shared_scope_id;
        clip.audio_components[0].id
    };
    let first_edit_id = sequence.audio_tracks[0].clips[0].audio_components[0].id;
    sequence.audio_program.compact_for_tracks(&sequence.audio_tracks);

    let plan = prepared(&sequence, 8);
    assert_eq!(plan.schedule_summary().processor_occurrence_count, 2);
    assert_eq!(plan.schedule_summary().processor_parameter_lane_count, 2);
    assert_eq!(
        plan.schedule_summary().maximum_parameter_events_per_block,
        1
    );
    let occurrences = plan
        .schedule
        .processors
        .iter()
        .map(|processor| processor.occurrence)
        .collect::<Vec<_>>();
    assert!(occurrences.iter().all(|occurrence| occurrence.instance_id == processor_id));
    assert!(occurrences.iter().any(|occurrence| matches!(
        occurrence.owner,
        crate::AudioProcessorOccurrenceOwner::Contribution { edit_id, scope_id }
            if edit_id == first_edit_id && scope_id == shared_scope_id
    )));
    assert!(occurrences.iter().any(|occurrence| matches!(
        occurrence.owner,
        crate::AudioProcessorOccurrenceOwner::Contribution { edit_id, scope_id }
            if edit_id == second_edit_id && scope_id == shared_scope_id
    )));

    let session = AudioRenderSession::new(plan).expect("Session");
    assert_eq!(session.capacity().processor_occurrences, 2);
}

#[test]
fn scalar_reference_and_runtime_vectorized_schedule_are_pcm_equivalent() {
    let mut sequence = sequence_with_audio_clip();
    while sequence.audio_tracks.len() < 8 {
        sequence.add_audio_track();
    }
    let additional_tracks =
        sequence.audio_tracks[1..8].iter().map(|track| track.id).collect::<Vec<_>>();
    for track_id in additional_tracks {
        let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("authored audio Clip");
    }
    let scalar = prepared_with_backend(&sequence, 257, AudioKernelBackend::ScalarReference);
    let vectorized = prepared_with_backend(&sequence, 257, AudioKernelBackend::RuntimeVectorized);
    assert_eq!(scalar.schedule_summary(), vectorized.schedule_summary());
    assert_eq!(scalar.schedule_summary().track_count, 8);
    assert_eq!(scalar.schedule_summary().contribution_count, 8);
    assert_eq!(scalar.schedule_summary().route_count, 8);

    let mut scalar_source = RampSource::default();
    let mut vectorized_source = RampSource::default();
    let scalar_pcm = render_audio(
        scalar,
        &mut scalar_source,
        AudioRenderRequest { start_sample: 0, frames: 257 },
    )
    .expect("scalar render");
    let vectorized_pcm = render_audio(
        vectorized,
        &mut vectorized_source,
        AudioRenderRequest { start_sample: 0, frames: 257 },
    )
    .expect("vectorized render");
    assert_eq!(scalar_pcm, vectorized_pcm);
    assert_eq!(scalar_source.block_reads, 8);
    assert_eq!(vectorized_source.block_reads, 8);
}

#[test]
fn track_mute_zeros_post_mute_route_without_reinterpreting_the_graph() {
    let mut sequence = sequence_with_audio_clip();
    sequence.audio_tracks[0].is_muted = true;
    let mut source = RampSource::default();
    let pcm = render_audio(
        prepared(&sequence, 4),
        &mut source,
        AudioRenderRequest { start_sample: 0, frames: 4 },
    )
    .expect("muted render");
    assert_eq!(pcm, vec![0.0; 4]);
}

#[test]
fn unresolved_plugin_survives_semantic_ir_and_fails_at_preparation() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track_id)
        .expect("track channel")
        .strip
        .pre_fader
        .processors
        .push(AudioProcessorInstance {
            id: mondrian_core::AudioProcessorInstanceId::new(),
            definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
                plugin_id: "com.example.effect".to_owned(),
                schema_version: 1,
            },
            bypassed: false,
            parameters: BTreeMap::new(),
            opaque_state: Some(vec![1, 2, 3]),
        });
    let output = sequence.audio_program.outputs[0].id;
    let compiled = compile_audio_program(&sequence, AudioCompileRequest::program(output))
        .expect("semantic compilation preserves unresolved plugin intent");
    let processor = &compiled
        .track_channels
        .get(&track_id)
        .expect("compiled Track channel")
        .strip
        .pre_fader
        .processors[0];
    assert!(matches!(
        processor.definition,
        mondrian_timeline::AudioProcessorDefinitionRef::Clap { ref plugin_id, .. }
            if plugin_id == "com.example.effect"
    ));
    assert_eq!(
        processor.opaque_state.as_deref(),
        Some([1, 2, 3].as_slice())
    );

    let error = PreparedAudioPlan::prepare(
        Arc::new(compiled),
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 256,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        },
    )
    .expect_err("default resolver must fail closed");
    assert!(matches!(
        error,
        AudioCompileError::ProcessorPreparation(AudioProcessorHostError::Unavailable(_))
    ));
}

#[test]
fn built_in_sample_delay_preserves_audible_delay_state_and_partitioned_pcm() {
    let sequence = sequence_with_parallel_sample_delay(2);
    let output = sequence.audio_program.outputs[0].id;
    let compiled = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("semantic program"),
    );
    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 5,
        processing_mode: AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let plan = Arc::new(
        PreparedAudioPlan::prepare_with_backend(
            compiled,
            contract,
            AudioKernelBackend::RuntimeVectorized,
        )
        .expect("sample-delay plan"),
    );
    assert_eq!(plan.output_latency_frames(), 0);
    assert!(plan.requires_state_entry());
    assert_eq!(plan.schedule_summary().processor_occurrence_count, 1);
    assert_eq!(plan.schedule_summary().maximum_compensation_frames, 0);
    assert_eq!(plan.schedule_summary().processor_session_scratch_bytes, 8);

    let mut whole = AudioRenderSession::new(Arc::clone(&plan)).expect("whole Session");
    assert_eq!(whole.capacity().processor_session_scratch_bytes, 8);
    assert_eq!(whole.capacity().meter_channel_state_count, 1);
    whole
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .expect("state entry");
    let mut source = RampSource::default();
    let mut whole_pcm = vec![0.0; 5];
    whole
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: 5 },
            &mut whole_pcm,
        )
        .expect("whole hosted block");
    assert_eq!(whole_pcm, vec![1.0, 2.0, 4.0, 6.0, 8.0]);
    let meter = whole.latest_meter_frame();
    assert_eq!(meter.block_serial, 1);
    assert_eq!(meter.start_sample, 0);
    assert_eq!(meter.channels[0].sample_peak_linear, 8.0);
    assert_eq!(meter.channels[0].clipped_sample_count, 4);

    let mut split = AudioRenderSession::new(plan).expect("split Session");
    split
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(2),
            start_sample: 0,
        })
        .expect("split state entry");
    let mut split_source = RampSource::default();
    let mut first = vec![0.0; 2];
    let mut second = vec![0.0; 3];
    split
        .render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 0, frames: 2 },
            &mut first,
        )
        .expect("first partition");
    split
        .render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 2, frames: 3 },
            &mut second,
        )
        .expect("second partition");
    assert_eq!([first, second].concat(), whole_pcm);
    assert_eq!(split.latest_meter_frame().block_serial, 2);

    split
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(3),
            start_sample: 0,
        })
        .expect("fresh seek entry");
    let mut after_seek = vec![0.0; 2];
    split
        .render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 0, frames: 2 },
            &mut after_seek,
        )
        .expect("render after seek");
    assert_eq!(after_seek, vec![1.0, 2.0]);
}

#[test]
fn contribution_processor_activates_lazily_and_flushes_its_declared_tail() {
    let sequence = sequence_with_scope_sample_delay(tt(2, 1), tt(1, 1), 2);
    let plan = prepared(&sequence, 4);
    let contribution = &plan.schedule.contributions[0];
    assert_eq!(contribution.sequence_start_sample, 4);
    assert_eq!(contribution.sequence_end_sample, 6);
    assert_eq!(contribution.execution_end_sample, Some(8));
    assert_eq!(contribution.scope_rack.algorithmic_latency_frames, 0);
    assert_eq!(contribution.scope_rack.tail, AudioProcessorTail::Finite(2));

    let mut session = AudioRenderSession::new(Arc::clone(&plan)).expect("Session");
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(40),
            start_sample: 0,
        })
        .expect("root entry");
    let mut source = RampSource::default();
    for start_sample in [0, 2] {
        let mut silence = [1.0; 2];
        session
            .render_into(
                &mut source,
                AudioRenderRequest { start_sample, frames: 2 },
                &mut silence,
            )
            .expect("pre-Contribution block");
        assert_eq!(silence, [0.0, 0.0]);
    }
    assert_eq!(source.block_reads, 0);

    let mut active_and_tail = [0.0; 4];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 4, frames: 4 },
            &mut active_and_tail,
        )
        .expect("active interval and declared tail");
    assert_eq!(active_and_tail, [0.0, 0.0, 1.0, 2.0]);
    assert_eq!(source.block_reads, 1);

    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(41),
            start_sample: 5,
        })
        .expect("seek into Contribution");
    let mut seek_output = [0.0; 3];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 5, frames: 3 },
            &mut seek_output,
        )
        .expect("cold Contribution-local entry");
    assert_eq!(seek_output, [0.0, 0.0, 2.0]);

    let immediate = sequence_with_scope_sample_delay(TimelineTime::ZERO, tt(1, 1), 2);
    let immediate_plan = prepared(&immediate, 4);
    let mut whole = AudioRenderSession::new(Arc::clone(&immediate_plan)).expect("whole Session");
    whole
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(42),
            start_sample: 0,
        })
        .expect("whole entry");
    let mut whole_source = RampSource::default();
    let mut whole_pcm = [0.0; 4];
    whole
        .render_into(
            &mut whole_source,
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut whole_pcm,
        )
        .expect("whole interval");

    let mut split = AudioRenderSession::new(immediate_plan).expect("split Session");
    split
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(43),
            start_sample: 0,
        })
        .expect("split entry");
    let mut split_source = RampSource::default();
    let mut split_pcm = Vec::new();
    for start_sample in 0..4 {
        let mut sample = [0.0; 1];
        split
            .render_into(
                &mut split_source,
                AudioRenderRequest { start_sample, frames: 1 },
                &mut sample,
            )
            .expect("single-frame partition");
        split_pcm.push(sample[0]);
    }
    assert_eq!(whole_pcm, [0.0, 0.0, 1.0, 2.0]);
    assert_eq!(split_pcm, whole_pcm);
}

#[test]
fn hosted_algorithmic_latency_drives_pdc_and_partitioned_pcm() {
    let sequence = sequence_with_parallel_hosted_delay();
    let output = sequence.audio_program.outputs[0].id;
    let compiled = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("semantic program"),
    );
    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 5,
        processing_mode: AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let resolver = TestDelayProcessorResolver {
        latency_frames: 2,
        realtime_capable: true,
        fail_first_state_entry: false,
    };
    let plan = Arc::new(
        PreparedAudioPlan::prepare_with_processor_resolver(
            compiled,
            contract,
            AudioKernelBackend::RuntimeVectorized,
            &resolver,
        )
        .expect("hosted latency plan"),
    );
    assert_eq!(plan.output_latency_frames(), 2);
    assert_eq!(plan.schedule_summary().maximum_compensation_frames, 2);
    assert!(plan.requires_state_entry());

    let mut whole = AudioRenderSession::new(Arc::clone(&plan)).expect("whole Session");
    whole
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(20),
            start_sample: 0,
        })
        .expect("whole entry");
    let mut source = RampSource::default();
    let mut whole_pcm = vec![0.0; 5];
    whole
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: 5 },
            &mut whole_pcm,
        )
        .expect("whole block");
    assert_eq!(whole_pcm, vec![0.0, 0.0, 2.0, 4.0, 6.0]);

    let mut split = AudioRenderSession::new(plan).expect("split Session");
    split
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(21),
            start_sample: 0,
        })
        .expect("split entry");
    let mut split_source = RampSource::default();
    let mut first = vec![0.0; 2];
    let mut second = vec![0.0; 3];
    split
        .render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 0, frames: 2 },
            &mut first,
        )
        .expect("first partition");
    split
        .render_into(
            &mut split_source,
            AudioRenderRequest { start_sample: 2, frames: 3 },
            &mut second,
        )
        .expect("second partition");
    assert_eq!([first, second].concat(), whole_pcm);
}

#[test]
fn processor_session_scratch_budget_fails_closed_during_preparation() {
    let sequence = sequence_with_parallel_sample_delay(2);
    let output = sequence.audio_program.outputs[0].id;
    let compiled = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("semantic program"),
    );
    let error = PreparedAudioPlan::prepare(
        compiled,
        AudioRenderContract {
            sample_rate: 2,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 5,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 7,
        },
    )
    .expect_err("eight bytes cannot fit a seven-byte budget");
    assert_eq!(
        error,
        AudioCompileError::ProcessorScratchBudgetExceeded { required_bytes: 8, budget_bytes: 7 }
    );
}

#[test]
fn processor_mode_capability_is_enforced_during_preparation() {
    let sequence = sequence_with_parallel_hosted_delay();
    let output = sequence.audio_program.outputs[0].id;
    let compiled = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("semantic program"),
    );
    let resolver = TestDelayProcessorResolver {
        latency_frames: 2,
        realtime_capable: false,
        fail_first_state_entry: false,
    };
    let error = PreparedAudioPlan::prepare_with_processor_resolver(
        compiled,
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 256,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        },
        AudioKernelBackend::RuntimeVectorized,
        &resolver,
    )
    .expect_err("offline-only processor cannot enter realtime plan");
    assert!(matches!(
        error,
        AudioCompileError::ProcessorPreparation(AudioProcessorHostError::InvalidContract(_))
    ));
}

#[test]
fn failed_processor_state_entry_poisons_the_new_epoch_before_partial_reset() {
    let sequence = sequence_with_parallel_hosted_delay();
    let output = sequence.audio_program.outputs[0].id;
    let compiled = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("semantic program"),
    );
    let resolver = TestDelayProcessorResolver {
        latency_frames: 2,
        realtime_capable: true,
        fail_first_state_entry: true,
    };
    let plan = Arc::new(
        PreparedAudioPlan::prepare_with_processor_resolver(
            compiled,
            AudioRenderContract {
                sample_rate: 2,
                channel_layout: AudioChannelLayout::Mono,
                max_block_frames: 4,
                processing_mode: AudioProcessingMode::Realtime,
                processor_session_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            },
            AudioKernelBackend::RuntimeVectorized,
            &resolver,
        )
        .expect("hosted plan"),
    );
    let mut session = AudioRenderSession::new(plan).expect("Session");
    let first_epoch = AudioContinuityEpoch::new(10);
    assert!(matches!(
        session.enter_state(AudioStateEntry { epoch: first_epoch, start_sample: 0 }),
        Err(AudioExecutionError::ProcessorHost(
            AudioProcessorHostError::StateEntry(_)
        ))
    ));
    let mut source = RampSource::default();
    let mut pcm = vec![0.0; 4];
    assert_eq!(
        session.render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut pcm,
        ),
        Err(AudioExecutionError::ContinuityPoisoned(first_epoch))
    );
    assert_eq!(
        session.enter_state(AudioStateEntry { epoch: first_epoch, start_sample: 0 }),
        Err(AudioExecutionError::ReusedContinuityEpoch(first_epoch))
    );

    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(11),
            start_sample: 0,
        })
        .expect("fresh epoch recovers");
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut pcm,
        )
        .expect("recovered render");
    assert_eq!(pcm, vec![0.0, 0.0, 2.0, 4.0]);
}

#[test]
fn nested_public_output_uses_an_independent_recursive_session() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;

    let mut root = Sequence::new("root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("child".to_owned()),
    )
    .expect("nested clip");
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");

    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("recursive runtime");
    assert_eq!(runtime.output_latency_frames(), 0);
    let mut pcm = vec![0.0; 4];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 4 }, &mut pcm)
        .expect("nested render");
    assert_eq!(pcm, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn standard_component_mapping_converts_native_media_before_sequence_processing() {
    let mut sequence = sequence_with_audio_clip();
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&sequence, &[], &RampResolver, contract, None)
        .expect("mono source mapped to stereo Sequence");
    let mut pcm = vec![0.0; 6];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 3 }, &mut pcm)
        .expect("mapped media render");
    assert_eq!(pcm, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
}

#[test]
fn explicit_component_matrix_executes_in_the_shared_prepared_schedule() {
    let mut sequence = Sequence::new("explicit matrix");
    let track_id = sequence.audio_tracks[0].id;
    let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("clip");
    sequence
        .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
        .expect("audio Clip");
    sequence.audio_tracks[0].clips[0].audio_components[0].channel_mapping =
        AudioComponentChannelMapping::Explicit(
            AudioChannelMixMatrix::new(
                AudioChannelLayout::Stereo,
                AudioChannelLayout::Stereo,
                [
                    AudioChannelMixEntry::new(1, 0, 1.0).expect("right to left"),
                    AudioChannelMixEntry::new(0, 1, 1.0).expect("left to right"),
                ],
            )
            .expect("swap matrix"),
        );
    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime =
        AudioProgramRuntime::build(&sequence, &[], &StereoRampResolver, contract, None)
            .expect("explicit matrix runtime");
    let mut pcm = vec![0.0; 4];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 2 }, &mut pcm)
        .expect("explicit matrix render");
    assert_eq!(pcm, vec![-1.0, 1.0, -2.0, 2.0]);
}

#[test]
fn nested_output_uses_child_layout_then_parent_component_mapping() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("stereo parent");
    let track_id = root.audio_tracks[0].id;
    let nested = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("mono child".to_owned()),
    )
    .expect("nested Clip");
    root.add_nested_audio_clip(track_id, nested, child_output)
        .expect("nested audio");
    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("child output mapped to parent layout");
    let mut pcm = vec![0.0; 4];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 2 }, &mut pcm)
        .expect("nested mapped render");
    assert_eq!(pcm, vec![1.0, 1.0, 2.0, 2.0]);
}

#[test]
fn stateless_nested_runtime_preserves_fractional_reverse_mapping() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let mut nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("reversed child".to_owned()),
    )
    .expect("nested clip");
    nested_clip.source_in = tt(3, 1);
    nested_clip.speed.set_scale(TimeScale::new(-1, 2).expect("exact reverse scale"));
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");

    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("stateless recursive runtime");
    let mut pcm = vec![0.0; 4];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 4 }, &mut pcm)
        .expect("stateless nested reverse render");
    assert_eq!(pcm, vec![7.0, 6.0, 6.0, 5.0]);
}

#[test]
fn stateful_nested_runtime_replays_forward_mapping_across_child_blocks() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;

    let mut root = Sequence::new("root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let mut nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("retimed child".to_owned()),
    )
    .expect("nested clip");
    nested_clip.source_in = tt(1, 4);
    nested_clip.speed.set_scale(TimeScale::new(3, 2).expect("exact forward scale"));
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");

    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("recursive runtime");
    runtime.require_state_entry_recursively_for_test();
    assert!(runtime.requires_state_entry());
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(41),
            start_sample: 0,
        })
        .expect("recursive state entry");

    let mut first = vec![0.0; 2];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 0, frames: 2 },
            &mut first,
        )
        .expect("first stateful nested retime block");
    let mut second = vec![0.0; 2];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 2, frames: 2 },
            &mut second,
        )
        .expect("second stateful nested retime block");
    assert_eq!(first, vec![1.0, 3.0]);
    assert_eq!(second, vec![4.0, 6.0]);
}

#[test]
fn stateful_nested_runtime_rejects_reverse_state_evaluation() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let mut nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("reversed child".to_owned()),
    )
    .expect("nested clip");
    nested_clip.source_in = tt(3, 1);
    nested_clip.speed.set_scale(TimeScale::NEGATIVE_ONE);
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");

    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("recursive runtime");
    runtime.require_state_entry_recursively_for_test();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(42),
            start_sample: 0,
        })
        .expect("recursive state entry");

    let edit_id = root.audio_tracks[0].clips[0].audio_components[0].id;
    let error = runtime
        .render_into(
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut [0.0; 4],
        )
        .expect_err("generic stateful child must not pretend to execute backwards");
    assert_eq!(
        error,
        AudioExecutionError::UnsupportedNestedStateDirection(edit_id)
    );
}

#[test]
fn stateful_nested_runtime_reenters_after_root_discontinuity() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(8, 1),
        Some("child".to_owned()),
    )
    .expect("nested clip");
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");

    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 2,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("recursive runtime");
    runtime.require_state_entry_recursively_for_test();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(51),
            start_sample: 0,
        })
        .expect("first recursive state entry");
    let mut first = vec![0.0; 2];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 0, frames: 2 },
            &mut first,
        )
        .expect("first nested block");

    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(52),
            start_sample: 6,
        })
        .expect("seek recursive state entry");
    let mut after_seek = vec![0.0; 2];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 6, frames: 2 },
            &mut after_seek,
        )
        .expect("nested block after seek");

    assert_eq!(first, vec![1.0, 2.0]);
    assert_eq!(after_seek, vec![7.0, 8.0]);
}

#[test]
fn direct_plan_preparation_rejects_unprepared_nested_source_dependency() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("child".to_owned()),
    )
    .expect("nested clip");
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested audio authoring");
    let root_output = root.audio_program.outputs[0].id;
    let program = Arc::new(
        compile_audio_program(&root, AudioCompileRequest::program(root_output))
            .expect("semantic program"),
    );
    let error = PreparedAudioPlan::prepare(
        program,
        AudioRenderContract {
            sample_rate: 2,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 8,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        },
    )
    .expect_err("nested source facts must be supplied by recursive preparation");
    assert!(matches!(error, AudioCompileError::InvalidPreparedGraph(_)));
    assert!(error.to_string().contains("no prepared child-output dependency"));
}

#[test]
fn nested_runtime_rejects_depth_beyond_the_shared_sequence_contract() {
    let mut child = sequence_with_audio_clip();
    let mut children = Vec::new();
    for index in 0..=mondrian_timeline::sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        let child_output = child.audio_program.outputs[0].id;
        let mut parent = Sequence::new(format!("nested-{index}"));
        parent.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let parent_track = parent.audio_tracks[0].id;
        let nested_clip = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            tt(4, 1),
            Some(child.name.clone()),
        )
        .expect("nested clip");
        parent
            .add_nested_audio_clip(parent_track, nested_clip, child_output)
            .expect("nested audio authoring");
        children.push(child);
        child = parent;
    }

    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
    };
    let error = match AudioProgramRuntime::build(&child, &children, &RampResolver, contract, None) {
        Ok(_) => panic!("depth above contract must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        AudioRuntimeBuildError::NestedDepthExceeded { .. }
    ));
}

#[test]
fn clip_balance_targets_semantic_front_pair_without_touching_surround_channels() {
    let mut sequence = sequence_with_audio_clip();
    sequence.audio_tracks[0].clips[0].audio_components[0].pan = 1.0;
    let output = sequence.audio_program.outputs[0].id;
    let program = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("semantic program"),
    );

    for (layout, expected) in [
        (AudioChannelLayout::Mono, vec![1.0]),
        (AudioChannelLayout::Stereo, vec![0.0, 1.0]),
        (
            AudioChannelLayout::Surround51Side,
            vec![0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        ),
        (
            AudioChannelLayout::speakers([
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
                AudioChannelPosition::TopCenter,
            ])
            .expect("custom speaker layout"),
            vec![0.0, 1.0, 1.0],
        ),
        (
            AudioChannelLayout::discrete(4).expect("discrete layout"),
            vec![1.0, 1.0, 1.0, 1.0],
        ),
    ] {
        let plan = Arc::new(
            PreparedAudioPlan::prepare(
                Arc::clone(&program),
                AudioRenderContract {
                    sample_rate: 2,
                    channel_layout: layout,
                    max_block_frames: 1,
                    processing_mode: AudioProcessingMode::Offline,
                    processor_session_scratch_budget_bytes:
                        AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                },
            )
            .expect("layout-specific plan"),
        );
        let actual = render_audio(
            plan,
            &mut RampSource::default(),
            AudioRenderRequest { start_sample: 0, frames: 1 },
        )
        .expect("rendered layout");
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() <= 1.0e-6, "layout={layout:?}");
        }
    }
}
