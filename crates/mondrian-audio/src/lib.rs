//! Author-model validation, compilation, and deterministic audio DSP execution.
//!
//! `mondrian-timeline` owns persistent audio intent. This crate is the single
//! lowering boundary shared by preview, playback, audition, analysis, and export.

use mondrian_core::{
    AudioContributionId, AudioSamplePosition, AudioSampleRate, AudioSampleRounding,
    ExactAutomationCurve, MixBusId, ProgramOutputId, TimelineTime, TimelineTimeError, TrackId,
};
use mondrian_timeline::audio::{
    AudioChannelStrip, AudioContribution, AudioProcessorDefinitionRef, AudioProcessorRack,
    AudioProgramOutput, AudioRoute, AudioRouteDestination, AudioRouteSource,
    ProgramOutputMainSource, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
};
use mondrian_timeline::{AudioAuthoringError, Sequence};
use std::collections::{BTreeMap, BTreeSet};

/// Consumer purpose. It affects admission/scheduling, never signal semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioConsumerPurpose {
    /// Interactive timeline monitoring.
    Preview,
    /// Offline file rendering.
    Export,
    /// Isolated source or effect audition.
    Audition,
    /// Signal analysis such as loudness or waveform generation.
    Analysis,
}

/// Immutable compiled signal closure for one public Program Output.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAudioPlan {
    output_id: ProgramOutputId,
    contributions: Vec<CompiledContribution>,
    track_channels: BTreeMap<TrackId, CompiledChannelStrip>,
    buses: BTreeMap<MixBusId, CompiledChannelStrip>,
    bus_order: Vec<MixBusId>,
    output: CompiledChannelStrip,
    routes: Vec<AudioRoute>,
}

impl CompiledAudioPlan {
    /// Public output produced by this closure.
    pub fn output_id(&self) -> ProgramOutputId {
        self.output_id
    }

    /// Contribution identities required by this closure.
    pub fn contribution_ids(&self) -> impl Iterator<Item = AudioContributionId> + '_ {
        self.contributions.iter().map(|contribution| contribution.id)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CompiledContribution {
    id: AudioContributionId,
    track_id: TrackId,
    sequence_range: mondrian_core::TimelineTimeRange,
    processors: CompiledRack,
    gain_db: f64,
    gain_automation: Option<ExactAutomationCurve>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct CompiledRack {
    gains: Vec<Option<ExactAutomationCurve>>,
}

#[derive(Debug, Clone, PartialEq)]
struct CompiledChannelStrip {
    input_trim_db: f64,
    pre_fader: CompiledRack,
    fader_db: f64,
    fader_automation: Option<ExactAutomationCurve>,
    post_fader: CompiledRack,
}

/// Compile one public output using the same semantic path for every consumer.
///
/// `purpose` is accepted for admission telemetry and future scheduling policy;
/// it is deliberately excluded from the returned signal closure.
pub fn compile_audio_program(
    sequence: &Sequence,
    output_id: ProgramOutputId,
    _purpose: AudioConsumerPurpose,
) -> Result<CompiledAudioPlan, AudioCompileError> {
    let track_ids = sequence.audio_tracks.iter().map(|track| track.id).collect::<Vec<_>>();
    sequence.audio_program.validate(&track_ids, &sequence.audio_roles)?;
    if !sequence.audio_program.transitions.is_empty() {
        return Err(AudioCompileError::TransitionsNotExecutableYet);
    }
    let output = sequence
        .audio_program
        .outputs
        .iter()
        .find(|output| output.id == output_id)
        .ok_or(AudioCompileError::OutputNotFound(output_id))?;
    if !matches!(output.main_source, ProgramOutputMainSource::RoutedInputs) {
        return Err(AudioCompileError::SemanticProjectionNotExecutableYet);
    }

    let (routes, required_tracks, required_buses) =
        resolve_signal_closure(&sequence.audio_program.routes, output_id);
    let mut track_channels = BTreeMap::new();
    for track_id in &required_tracks {
        let channel = sequence
            .audio_program
            .track_channels
            .get(track_id)
            .ok_or(AudioCompileError::MissingTrackChannel(*track_id))?;
        track_channels.insert(*track_id, compile_strip(&channel.strip)?);
    }

    let mut buses = BTreeMap::new();
    for bus in &sequence.audio_program.buses {
        if required_buses.contains(&bus.id) {
            buses.insert(bus.id, compile_strip(&bus.strip)?);
        }
    }
    let bus_order = topological_bus_order(&routes, &required_buses)?;

    let contributions = sequence
        .audio_program
        .contributions
        .iter()
        .filter(|contribution| required_tracks.contains(&contribution.track_id))
        .map(compile_contribution)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CompiledAudioPlan {
        output_id,
        contributions,
        track_channels,
        buses,
        bus_order,
        output: compile_output(output)?,
        routes,
    })
}

fn compile_contribution(
    contribution: &AudioContribution,
) -> Result<CompiledContribution, AudioCompileError> {
    Ok(CompiledContribution {
        id: contribution.id,
        track_id: contribution.track_id,
        sequence_range: contribution.sequence_range,
        processors: compile_rack(&contribution.processors)?,
        gain_db: contribution.gain_db,
        gain_automation: contribution.gain_automation.clone(),
    })
}

fn compile_output(output: &AudioProgramOutput) -> Result<CompiledChannelStrip, AudioCompileError> {
    compile_strip(&output.strip)
}

fn compile_strip(strip: &AudioChannelStrip) -> Result<CompiledChannelStrip, AudioCompileError> {
    Ok(CompiledChannelStrip {
        input_trim_db: strip.input_trim_db,
        pre_fader: compile_rack(&strip.pre_fader)?,
        fader_db: strip.fader_db,
        fader_automation: strip.fader_automation.clone(),
        post_fader: compile_rack(&strip.post_fader)?,
    })
}

fn compile_rack(rack: &AudioProcessorRack) -> Result<CompiledRack, AudioCompileError> {
    let mut gains = Vec::new();
    for processor in &rack.processors {
        if processor.bypassed {
            continue;
        }
        match &processor.definition {
            AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version }
                if definition_id == BUILTIN_GAIN_DEFINITION_ID && *schema_version == 1 =>
            {
                let curve = processor.parameters.iter().find_map(|(id, curve)| {
                    (id.as_str() == GAIN_DB_PARAMETER_ID).then(|| curve.clone())
                });
                if processor.parameters.keys().any(|id| id.as_str() != GAIN_DB_PARAMETER_ID) {
                    return Err(AudioCompileError::UnsupportedBuiltInParameter);
                }
                gains.push(curve);
            }
            AudioProcessorDefinitionRef::BuiltIn { definition_id, .. } => {
                return Err(AudioCompileError::UnsupportedBuiltIn(definition_id.clone()))
            }
            AudioProcessorDefinitionRef::Vst3 { class_id, .. } => {
                return Err(AudioCompileError::UnresolvedPlugin(format!(
                    "VST3:{class_id}"
                )))
            }
            AudioProcessorDefinitionRef::Clap { plugin_id, .. } => {
                return Err(AudioCompileError::UnresolvedPlugin(format!(
                    "CLAP:{plugin_id}"
                )))
            }
        }
    }
    Ok(CompiledRack { gains })
}

fn resolve_signal_closure(
    routes: &[AudioRoute],
    output_id: ProgramOutputId,
) -> (Vec<AudioRoute>, BTreeSet<TrackId>, BTreeSet<MixBusId>) {
    let mut required_buses = BTreeSet::new();
    let mut required_tracks = BTreeSet::new();
    let mut selected = Vec::new();
    let mut pending_destinations = vec![AudioRouteDestination::Output(output_id)];
    while let Some(destination) = pending_destinations.pop() {
        for route in routes.iter().filter(|route| route.destination == destination) {
            if selected.iter().any(|selected: &AudioRoute| selected.id == route.id) {
                continue;
            }
            selected.push(route.clone());
            match route.source {
                AudioRouteSource::Track(track_id) => {
                    required_tracks.insert(track_id);
                }
                AudioRouteSource::Bus(bus_id) => {
                    if required_buses.insert(bus_id) {
                        pending_destinations.push(AudioRouteDestination::Bus(bus_id));
                    }
                }
            }
        }
    }
    selected.sort_by_key(|route| route.id);
    (selected, required_tracks, required_buses)
}

fn topological_bus_order(
    routes: &[AudioRoute],
    buses: &BTreeSet<MixBusId>,
) -> Result<Vec<MixBusId>, AudioCompileError> {
    let mut indegree = buses.iter().map(|id| (*id, 0_usize)).collect::<BTreeMap<_, _>>();
    for route in routes {
        if let (AudioRouteSource::Bus(source), AudioRouteDestination::Bus(destination)) =
            (route.source, route.destination)
        {
            if buses.contains(&source) && buses.contains(&destination) {
                if let Some(value) = indegree.get_mut(&destination) {
                    *value += 1;
                }
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_first() {
        order.push(id);
        for route in routes {
            if route.source == AudioRouteSource::Bus(id) {
                if let AudioRouteDestination::Bus(destination) = route.destination {
                    if let Some(value) = indegree.get_mut(&destination) {
                        *value -= 1;
                        if *value == 0 {
                            ready.insert(destination);
                        }
                    }
                }
            }
        }
    }
    if order.len() != buses.len() {
        return Err(AudioCompileError::RouteCycle);
    }
    Ok(order)
}

/// Pull-style PCM source for compiled contribution identities.
pub trait AudioPcmSource {
    /// Return one normalized floating sample. Values outside `[-1, 1]` are legal.
    fn sample(&self, contribution: AudioContributionId, frame_offset: usize, channel: usize)
        -> f32;
}

/// Exact render request. Block partitioning must not affect its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRenderRequest {
    /// Sequence-domain start time.
    pub start: TimelineTime,
    /// Number of output sample frames.
    pub frames: usize,
    /// Explicit output sample rate.
    pub sample_rate: u32,
    /// Interleaved output channel count.
    pub channels: usize,
}

/// Execute one immutable plan into interleaved `f32` PCM.
pub fn render_audio(
    plan: &CompiledAudioPlan,
    source: &impl AudioPcmSource,
    request: AudioRenderRequest,
) -> Result<Vec<f32>, AudioExecutionError> {
    if request.sample_rate == 0 || request.channels == 0 {
        return Err(AudioExecutionError::InvalidFormat);
    }
    let sample_count = request
        .frames
        .checked_mul(request.channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let mut output = vec![0.0_f32; sample_count];

    for frame_offset in 0..request.frames {
        let offset = TimelineTime::new(
            i64::try_from(frame_offset).map_err(|_| AudioExecutionError::BufferTooLarge)?,
            i64::from(request.sample_rate),
        )?;
        let sequence_time = request.start.checked_add(offset)?;
        let mut tracks = plan
            .track_channels
            .keys()
            .map(|id| (*id, vec![0.0_f32; request.channels]))
            .collect::<BTreeMap<_, _>>();

        for contribution in &plan.contributions {
            if !contribution.sequence_range.contains(sequence_time)? {
                continue;
            }
            let local_time = sequence_time.checked_sub(contribution.sequence_range.start)?;
            let source_frame = AudioSamplePosition::from_timeline_time(
                local_time,
                AudioSampleRate::new(request.sample_rate)?,
                AudioSampleRounding::Floor,
            )?;
            let source_frame = usize::try_from(source_frame.sample())
                .map_err(|_| AudioExecutionError::BufferTooLarge)?;
            let gain_db = contribution
                .gain_automation
                .as_ref()
                .map_or(Ok(contribution.gain_db), |curve| {
                    curve.evaluate(local_time).map_err(AudioExecutionError::from)
                })?
                + contribution.processors.gain_db(local_time)?;
            let gain = db_to_linear(gain_db);
            if let Some(track) = tracks.get_mut(&contribution.track_id) {
                for (channel, value) in track.iter_mut().enumerate() {
                    *value += source.sample(contribution.id, source_frame, channel) * gain;
                }
            }
        }

        for (track_id, samples) in &mut tracks {
            if let Some(strip) = plan.track_channels.get(track_id) {
                strip.process(samples, sequence_time)?;
            }
        }

        let mut buses = BTreeMap::<MixBusId, Vec<f32>>::new();
        for bus_id in &plan.bus_order {
            let mut samples = vec![0.0_f32; request.channels];
            sum_incoming(
                &mut samples,
                AudioRouteDestination::Bus(*bus_id),
                &plan.routes,
                &tracks,
                &buses,
            );
            if let Some(strip) = plan.buses.get(bus_id) {
                strip.process(&mut samples, sequence_time)?;
            }
            buses.insert(*bus_id, samples);
        }

        let frame =
            &mut output[frame_offset * request.channels..(frame_offset + 1) * request.channels];
        sum_incoming(
            frame,
            AudioRouteDestination::Output(plan.output_id),
            &plan.routes,
            &tracks,
            &buses,
        );
        plan.output.process(frame, sequence_time)?;
    }
    Ok(output)
}

fn sum_incoming(
    destination_samples: &mut [f32],
    destination: AudioRouteDestination,
    routes: &[AudioRoute],
    tracks: &BTreeMap<TrackId, Vec<f32>>,
    buses: &BTreeMap<MixBusId, Vec<f32>>,
) {
    for route in routes.iter().filter(|route| route.destination == destination) {
        let source = match route.source {
            AudioRouteSource::Track(id) => tracks.get(&id),
            AudioRouteSource::Bus(id) => buses.get(&id),
        };
        if let Some(source) = source {
            for (destination, source) in destination_samples.iter_mut().zip(source) {
                *destination += *source;
            }
        }
    }
}

impl CompiledRack {
    fn gain_db(&self, time: TimelineTime) -> Result<f64, AudioExecutionError> {
        self.gains.iter().try_fold(0.0, |sum, curve| {
            Ok(sum + curve.as_ref().map_or(Ok(0.0), |curve| curve.evaluate(time))?)
        })
    }
}

impl CompiledChannelStrip {
    fn process(&self, samples: &mut [f32], time: TimelineTime) -> Result<(), AudioExecutionError> {
        let fader = self
            .fader_automation
            .as_ref()
            .map_or(Ok(self.fader_db), |curve| curve.evaluate(time))?;
        let gain_db = self.input_trim_db
            + self.pre_fader.gain_db(time)?
            + fader
            + self.post_fader.gain_db(time)?;
        let gain = db_to_linear(gain_db);
        for sample in samples {
            *sample *= gain;
        }
        Ok(())
    }
}

fn db_to_linear(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
}

/// Author validation or lowering failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AudioCompileError {
    /// Invalid persistent author state.
    #[error(transparent)]
    InvalidAuthoring(#[from] AudioAuthoringError),
    /// Requested public output is absent.
    #[error("audio Program Output {0} does not exist")]
    OutputNotFound(ProgramOutputId),
    /// Required Track mixer state is absent.
    #[error("audio Track mixer channel {0} is absent")]
    MissingTrackChannel(TrackId),
    /// Built-in processor is not implemented by this executor.
    #[error("unsupported built-in audio processor {0}")]
    UnsupportedBuiltIn(String),
    /// Gain processor contains a parameter outside its versioned schema.
    #[error("unsupported built-in Gain parameter")]
    UnsupportedBuiltInParameter,
    /// Plugin author intent is valid but the runtime dependency is unresolved.
    #[error("audio plugin dependency is unresolved: {0}")]
    UnresolvedPlugin(String),
    /// Semantic projection authoring is preserved but not executable yet.
    #[error("semantic audio output projection is not executable yet")]
    SemanticProjectionNotExecutableYet,
    /// Transition authoring is preserved but not executable yet.
    #[error("audio Transitions are not executable yet")]
    TransitionsNotExecutableYet,
    /// Instantaneous route cycle reached compilation.
    #[error("audio routing contains a cycle")]
    RouteCycle,
}

/// Deterministic DSP execution failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AudioExecutionError {
    /// Sample rate and channel count must be positive.
    #[error("invalid audio render format")]
    InvalidFormat,
    /// Requested allocation or index cannot be represented.
    #[error("audio render buffer is too large")]
    BufferTooLarge,
    /// Exact timeline arithmetic failed.
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
    /// Exact author-time to sample-grid conversion failed.
    #[error(transparent)]
    AudioTime(#[from] mondrian_core::AudioTimeError),
    /// Automation authoring/evaluation failed.
    #[error("invalid audio automation: {0}")]
    Automation(String),
}

impl From<mondrian_core::AutomationError> for AudioExecutionError {
    fn from(error: mondrian_core::AutomationError) -> Self {
        Self::Automation(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        AudioRouteId, AudioSourceComponentId, ClipId, ExactAutomationKeyframe, ParameterId,
        TimelineTimeRange,
    };
    use mondrian_timeline::audio::{AudioContributionSource, AudioMixBus, AudioProcessorInstance};

    struct ConstantSource;

    impl AudioPcmSource for ConstantSource {
        fn sample(
            &self,
            _contribution: AudioContributionId,
            _frame_offset: usize,
            _channel: usize,
        ) -> f32 {
            1.0
        }
    }

    struct RampSource;

    impl AudioPcmSource for RampSource {
        fn sample(
            &self,
            _contribution: AudioContributionId,
            frame_offset: usize,
            _channel: usize,
        ) -> f32 {
            frame_offset as f32 + 1.0
        }
    }

    fn exact(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).expect("valid test time")
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

    #[test]
    fn preview_and_export_compile_the_same_signal_closure() {
        let sequence = Sequence::new("audio");
        let output = sequence.audio_program.outputs[0].id;
        let preview = compile_audio_program(&sequence, output, AudioConsumerPurpose::Preview)
            .expect("preview plan");
        let export = compile_audio_program(&sequence, output, AudioConsumerPurpose::Export)
            .expect("export plan");
        assert_eq!(preview, export);
    }

    #[test]
    fn contribution_track_bus_and_output_gain_are_sample_accurate_and_unclipped() {
        let mut sequence = Sequence::new("audio");
        let track_id = sequence.audio_tracks[0].id;
        let output_id = sequence.audio_program.outputs[0].id;
        sequence
            .audio_program
            .routes
            .retain(|route| route.source != AudioRouteSource::Track(track_id));

        let bus_id = MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus_id,
            name: "Dialog stem".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        sequence.audio_program.routes.extend([
            AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Track(track_id),
                destination: AudioRouteDestination::Bus(bus_id),
            },
            AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Bus(bus_id),
                destination: AudioRouteDestination::Output(output_id),
            },
        ]);

        let contribution_id = AudioContributionId::new();
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let curve = gain_curve(0.0, 6.0);
        processor.parameters.insert(curve.parameter_id.clone(), curve);
        sequence.audio_program.contributions.push(AudioContribution {
            id: contribution_id,
            source: AudioContributionSource::ClipComponent {
                clip_id: ClipId::new(),
                component_id: AudioSourceComponentId::new(),
            },
            track_id,
            role_id: None,
            sequence_range: TimelineTimeRange::new(TimelineTime::ZERO, exact(2, 1)).expect("range"),
            processors: AudioProcessorRack { processors: vec![processor] },
            gain_db: 0.0,
            gain_automation: None,
        });

        let plan = compile_audio_program(&sequence, output_id, AudioConsumerPurpose::Preview)
            .expect("plan");
        let pcm = render_audio(
            &plan,
            &ConstantSource,
            AudioRenderRequest {
                start: TimelineTime::ZERO,
                frames: 3,
                sample_rate: 2,
                channels: 1,
            },
        )
        .expect("pcm");

        assert!((pcm[0] - 1.0).abs() < 1.0e-6);
        assert!((pcm[1] - 10.0_f32.powf(3.0 / 20.0)).abs() < 1.0e-5);
        assert!(pcm[2] > 1.9, "internal PCM must not be silently clipped");
        assert_eq!(
            plan.contribution_ids().collect::<Vec<_>>(),
            vec![contribution_id]
        );
    }

    #[test]
    fn unresolved_plugin_blocks_compilation_without_destroying_author_intent() {
        let mut sequence = Sequence::new("audio");
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(mondrian_timeline::AudioProcessorInstance {
                id: mondrian_core::AudioProcessorInstanceId::new(),
                definition: AudioProcessorDefinitionRef::Clap {
                    plugin_id: "com.example.effect".to_owned(),
                    schema_version: 1,
                },
                bypassed: false,
                parameters: BTreeMap::new(),
                opaque_state: Some(vec![1, 2, 3]),
            });
        let output = sequence.audio_program.outputs[0].id;
        let error = compile_audio_program(&sequence, output, AudioConsumerPurpose::Export)
            .expect_err("unresolved plugin");
        assert!(matches!(error, AudioCompileError::UnresolvedPlugin(_)));
    }

    #[test]
    fn render_result_is_independent_of_block_partitioning() {
        let mut sequence = Sequence::new("audio");
        let track_id = sequence.audio_tracks[0].id;
        let output_id = sequence.audio_program.outputs[0].id;
        sequence.audio_program.contributions.push(AudioContribution {
            id: AudioContributionId::new(),
            source: AudioContributionSource::ClipComponent {
                clip_id: ClipId::new(),
                component_id: AudioSourceComponentId::new(),
            },
            track_id,
            role_id: None,
            sequence_range: TimelineTimeRange::new(TimelineTime::ZERO, exact(4, 1)).expect("range"),
            processors: AudioProcessorRack::default(),
            gain_db: 0.0,
            gain_automation: Some(gain_curve(0.0, 6.0)),
        });
        let plan = compile_audio_program(&sequence, output_id, AudioConsumerPurpose::Export)
            .expect("plan");
        let whole = render_audio(
            &plan,
            &RampSource,
            AudioRenderRequest {
                start: TimelineTime::ZERO,
                frames: 4,
                sample_rate: 2,
                channels: 1,
            },
        )
        .expect("whole");
        let first = render_audio(
            &plan,
            &RampSource,
            AudioRenderRequest {
                start: TimelineTime::ZERO,
                frames: 2,
                sample_rate: 2,
                channels: 1,
            },
        )
        .expect("first");
        let second = render_audio(
            &plan,
            &RampSource,
            AudioRenderRequest {
                start: TimelineTime::ONE,
                frames: 2,
                sample_rate: 2,
                channels: 1,
            },
        )
        .expect("second");
        assert_eq!(whole, [first, second].concat());
    }
}
