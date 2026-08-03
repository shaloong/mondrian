use super::*;
use mondrian_timeline::audio::{
    BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID,
    LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID, LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID,
};

#[derive(Clone)]
struct PatternSource {
    channel_layout: AudioChannelLayout,
    interleaved: Vec<f32>,
}

impl PatternSource {
    fn mono(samples: impl IntoIterator<Item = f32>) -> Self {
        Self {
            channel_layout: AudioChannelLayout::Mono,
            interleaved: samples.into_iter().collect(),
        }
    }

    fn stereo(frames: impl IntoIterator<Item = [f32; 2]>) -> Self {
        Self {
            channel_layout: AudioChannelLayout::Stereo,
            interleaved: frames.into_iter().flatten().collect(),
        }
    }
}

impl AudioPcmSource for PatternSource {
    fn read_indexed_interleaved(
        &mut self,
        _edit: AudioComponentEditId,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let channels = channel_layout.channel_count();
        if channel_layout != self.channel_layout
            || destination.len()
                != source_frames
                    .len()
                    .checked_mul(channels)
                    .ok_or(AudioExecutionError::BufferTooLarge)?
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        for (output_frame, source_frame) in source_frames.iter().copied().enumerate() {
            let output = &mut destination[output_frame * channels..(output_frame + 1) * channels];
            let Ok(source_frame) = usize::try_from(source_frame) else {
                output.fill(0.0);
                continue;
            };
            let Some(source_start) = source_frame.checked_mul(channels) else {
                return Err(AudioExecutionError::BufferTooLarge);
            };
            let Some(source_end) = source_start.checked_add(channels) else {
                return Err(AudioExecutionError::BufferTooLarge);
            };
            let Some(source) = self.interleaved.get(source_start..source_end) else {
                output.fill(0.0);
                continue;
            };
            output.copy_from_slice(source);
        }
        Ok(())
    }
}

fn sequence_with_audio_clip_layout(channel_layout: AudioChannelLayout) -> Sequence {
    let mut sequence = Sequence::new("audio layout");
    sequence.settings.audio_channel_layout = channel_layout;
    let track_id = sequence.audio_tracks[0].id;
    let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("clip");
    sequence
        .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
        .expect("authored audio Clip");
    sequence
}

fn lookahead_limiter(
    ceiling_db: f64,
    lookahead_ms: f64,
    release_ms: f64,
) -> AudioProcessorInstance {
    let mut processor =
        AudioProcessorInstance::built_in(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, 1);
    for (parameter_id, value) in [
        (LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID, ceiling_db),
        (LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID, lookahead_ms),
        (LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID, release_ms),
    ] {
        processor
            .set_parameter_automation(
                ExactAutomationCurve::new(ParameterId::new_static(parameter_id), value)
                    .expect("limiter curve"),
            )
            .expect("valid limiter parameter");
    }
    processor
}

fn sequence_with_output_limiter(
    channel_layout: AudioChannelLayout,
    processor: AudioProcessorInstance,
) -> Sequence {
    let mut sequence = sequence_with_audio_clip_layout(channel_layout);
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
    sequence
}

fn prepared_at(
    sequence: &Sequence,
    sample_rate: u32,
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
                sample_rate,
                channel_layout: sequence.settings.audio_channel_layout,
                max_block_frames: max_frames,
                processing_mode: AudioProcessingMode::Realtime,
                processor_session_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                public_output_lookahead_budget_frames:
                    AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
                compensation_delay_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
            },
            backend,
        )
        .expect("prepared plan"),
    )
}

#[allow(clippy::too_many_arguments)]
fn reference_linked_sample_peak_limiter(
    source: &[f32],
    channels: usize,
    start_frame: usize,
    frames: usize,
    lookahead_frames: usize,
    ceiling_db: f64,
    release_ms: f64,
    sample_rate: u32,
) -> Vec<f32> {
    let ceiling = 10.0_f32.powf(ceiling_db as f32 / 20.0);
    let release_samples = release_ms * f64::from(sample_rate) / 1_000.0;
    let release_coefficient = (-1.0 / release_samples).exp() as f32;
    let mut gain = 1.0_f32;
    let mut output = vec![0.0; frames * channels];
    for local_frame in 0..frames {
        let signal_frame = start_frame + local_frame;
        let mut peak = 0.0_f32;
        for future in 0..=lookahead_frames {
            let frame = signal_frame + future;
            let Some(frame_start) = frame.checked_mul(channels) else {
                continue;
            };
            let Some(frame_end) = frame_start.checked_add(channels) else {
                continue;
            };
            if let Some(samples) = source.get(frame_start..frame_end) {
                peak = samples.iter().fold(peak, |maximum, sample| maximum.max(sample.abs()));
            }
        }
        let target = if peak > ceiling { ceiling / peak } else { 1.0 };
        let released = 1.0 - (1.0 - gain) * release_coefficient;
        gain = if target < gain {
            target
        } else {
            released.min(target)
        };
        let output_start = local_frame * channels;
        let source_start = signal_frame * channels;
        for channel in 0..channels {
            output[output_start + channel] =
                (source.get(source_start + channel).copied().unwrap_or(0.0) * gain)
                    .clamp(-ceiling, ceiling);
        }
    }
    output
}

#[test]
fn built_in_lookahead_limiter_is_block_invariant_and_bounds_sample_peaks() {
    let sequence = sequence_with_output_limiter(
        AudioChannelLayout::Mono,
        lookahead_limiter(0.0, 2.0, 1_000.0),
    );
    let scalar_plan = prepared_at(&sequence, 1_000, 8, AudioKernelBackend::ScalarReference);
    let vector_plan = prepared_at(&sequence, 1_000, 8, AudioKernelBackend::RuntimeVectorized);
    assert_eq!(scalar_plan.public_output_lookahead_frames(), 2);
    assert_eq!(scalar_plan.schedule_summary().processor_occurrence_count, 1);
    assert_eq!(
        scalar_plan.schedule_summary().processor_session_scratch_bytes,
        236
    );
    assert!(scalar_plan.requires_state_entry());

    let pattern = [0.25, 0.5, 2.0, 0.25, 0.25, 0.25, 0.25, 0.25];
    let mut scalar = AudioRenderSession::new(scalar_plan).expect("scalar Session");
    scalar
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(100),
            start_sample: 0,
        })
        .expect("scalar state entry");
    let mut scalar_source = PatternSource::mono(pattern);
    let mut scalar_pcm = [0.0; 6];
    scalar
        .render_into(
            &mut scalar_source,
            AudioRenderRequest { start_sample: 0, frames: scalar_pcm.len() },
            &mut scalar_pcm,
        )
        .expect("scalar limiter render");
    assert_eq!(&scalar_pcm[..3], &[0.125, 0.25, 1.0]);
    assert!(scalar_pcm.iter().all(|sample| sample.is_finite() && sample.abs() <= 1.0));
    assert_eq!(
        scalar.latest_meter_frame().channels[0].clipped_sample_count,
        0
    );

    let mut vector = AudioRenderSession::new(Arc::clone(&vector_plan)).expect("vector Session");
    vector
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(101),
            start_sample: 0,
        })
        .expect("vector state entry");
    let mut vector_source = PatternSource::mono(pattern);
    let mut first = [0.0; 2];
    let mut second = [0.0; 4];
    vector
        .render_into(
            &mut vector_source,
            AudioRenderRequest { start_sample: 0, frames: first.len() },
            &mut first,
        )
        .expect("first vector partition");
    vector
        .render_into(
            &mut vector_source,
            AudioRenderRequest { start_sample: 2, frames: second.len() },
            &mut second,
        )
        .expect("second vector partition");
    assert_eq!([first.as_slice(), second.as_slice()].concat(), scalar_pcm);
    assert_eq!(vector.capacity().processor_session_scratch_bytes, 236);
    assert_eq!(vector.capacity().public_output_lookahead_frames, 2);
}

#[test]
fn limiter_monotonic_window_matches_independent_scalar_reference_and_seek_entry() {
    const SAMPLE_RATE: u32 = 1_000;
    const LOOKAHEAD: usize = 5;
    const CEILING_DB: f64 = -3.0;
    const RELEASE_MS: f64 = 50.0;
    const FRAMES: usize = 97;
    let sequence = sequence_with_output_limiter(
        AudioChannelLayout::Stereo,
        lookahead_limiter(CEILING_DB, LOOKAHEAD as f64, RELEASE_MS),
    );
    let scalar_plan = prepared_at(
        &sequence,
        SAMPLE_RATE,
        FRAMES,
        AudioKernelBackend::ScalarReference,
    );
    let vector_plan = prepared_at(
        &sequence,
        SAMPLE_RATE,
        17,
        AudioKernelBackend::RuntimeVectorized,
    );

    let mut state = 0x1234_5678_u32;
    let mut source_pcm = Vec::with_capacity((FRAMES + LOOKAHEAD + 32) * 2);
    for _ in 0..FRAMES + LOOKAHEAD + 32 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let left = ((state >> 8) as f32 / 16_777_215.0 * 2.0 - 1.0) * 1.5;
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let right = ((state >> 8) as f32 / 16_777_215.0 * 2.0 - 1.0) * 1.5;
        source_pcm.extend([left, right]);
    }
    let expected = reference_linked_sample_peak_limiter(
        &source_pcm,
        2,
        0,
        FRAMES,
        LOOKAHEAD,
        CEILING_DB,
        RELEASE_MS,
        SAMPLE_RATE,
    );

    let mut scalar = AudioRenderSession::new(scalar_plan).expect("scalar reference Session");
    scalar
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(107),
            start_sample: 0,
        })
        .expect("scalar reference entry");
    let mut scalar_source = PatternSource {
        channel_layout: AudioChannelLayout::Stereo,
        interleaved: source_pcm.clone(),
    };
    let mut scalar_pcm = vec![0.0; FRAMES * 2];
    scalar
        .render_into(
            &mut scalar_source,
            AudioRenderRequest { start_sample: 0, frames: FRAMES },
            &mut scalar_pcm,
        )
        .expect("scalar whole block");

    let mut vector = AudioRenderSession::new(Arc::clone(&vector_plan)).expect("vector Session");
    vector
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(108),
            start_sample: 0,
        })
        .expect("vector entry");
    let mut vector_source = PatternSource {
        channel_layout: AudioChannelLayout::Stereo,
        interleaved: source_pcm.clone(),
    };
    let partitions = [1_usize, 7, 3, 16, 2, 11, 17, 5, 13, 9, 13];
    assert_eq!(partitions.iter().sum::<usize>(), FRAMES);
    let mut vector_pcm = Vec::with_capacity(FRAMES * 2);
    let mut start_sample = 0_i64;
    for frames in partitions {
        let mut block = vec![0.0; frames * 2];
        vector
            .render_into(
                &mut vector_source,
                AudioRenderRequest { start_sample, frames },
                &mut block,
            )
            .expect("partitioned vector block");
        vector_pcm.extend(block);
        start_sample += i64::try_from(frames).expect("small test partition");
    }
    for (index, ((scalar, vector), expected)) in
        scalar_pcm.iter().zip(&vector_pcm).zip(&expected).enumerate()
    {
        assert!(
            (scalar - expected).abs() <= 2.0e-6 && (vector - expected).abs() <= 2.0e-6,
            "sample {index}: scalar={scalar} vector={vector} expected={expected}"
        );
    }

    const SEEK_START: usize = 37;
    const SEEK_FRAMES: usize = 17;
    vector
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(109),
            start_sample: SEEK_START as i64,
        })
        .expect("seek entry");
    let mut seek_pcm = vec![0.0; SEEK_FRAMES * 2];
    vector
        .render_into(
            &mut vector_source,
            AudioRenderRequest {
                start_sample: SEEK_START as i64,
                frames: SEEK_FRAMES,
            },
            &mut seek_pcm,
        )
        .expect("seek render");
    let seek_expected = reference_linked_sample_peak_limiter(
        &source_pcm,
        2,
        SEEK_START,
        SEEK_FRAMES,
        LOOKAHEAD,
        CEILING_DB,
        RELEASE_MS,
        SAMPLE_RATE,
    );
    for (index, (actual, expected)) in seek_pcm.iter().zip(&seek_expected).enumerate() {
        assert!(
            (actual - expected).abs() <= 2.0e-6,
            "seek sample {index}: actual={actual} expected={expected}"
        );
    }
}

#[test]
fn built_in_lookahead_limiter_drives_real_parallel_pdc() {
    let sequence = sequence_with_parallel_processor(lookahead_limiter(0.0, 2.0, 100.0));
    let plan = prepared_at(&sequence, 1_000, 5, AudioKernelBackend::RuntimeVectorized);
    assert_eq!(plan.public_output_lookahead_frames(), 2);
    assert_eq!(plan.schedule_summary().maximum_compensation_frames, 2);
    assert!(plan.schedule_summary().compensation_delay_samples >= 2);

    let mut session = AudioRenderSession::new(plan).expect("PDC Session");
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(102),
            start_sample: 0,
        })
        .expect("PDC state entry");
    let mut source = PatternSource::mono([0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]);
    let mut pcm = [0.0; 5];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: pcm.len() },
            &mut pcm,
        )
        .expect("parallel limited render");
    for (actual, expected) in pcm.iter().zip([0.2_f32, 0.4, 0.6, 0.8, 1.0]) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "actual={actual} expected={expected}"
        );
    }
}

#[test]
fn built_in_lookahead_limiter_links_channels_without_collapsing_image() {
    let sequence = sequence_with_output_limiter(
        AudioChannelLayout::Stereo,
        lookahead_limiter(0.0, 0.0, 100.0),
    );
    let plan = prepared_at(&sequence, 1_000, 2, AudioKernelBackend::RuntimeVectorized);
    assert_eq!(plan.public_output_lookahead_frames(), 0);
    assert!(plan.requires_state_entry());
    let mut session = AudioRenderSession::new(plan).expect("stereo Session");
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(103),
            start_sample: 0,
        })
        .expect("stereo state entry");
    let mut source = PatternSource::stereo([[2.0, 0.5], [0.25, 0.125]]);
    let mut pcm = [0.0; 4];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: 2 },
            &mut pcm,
        )
        .expect("linked limiter render");
    assert_eq!(&pcm[..2], &[1.0, 0.25]);
    assert!((pcm[2] / pcm[3] - 2.0).abs() <= 1.0e-6);
}

#[test]
fn limiter_ceiling_automation_stays_on_signal_time_across_lookahead() {
    let mut limiter = lookahead_limiter(0.0, 2.0, 100.0);
    let ceiling_id = ParameterId::new_static(LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID);
    let mut ceiling = ExactAutomationCurve::new(ceiling_id, 0.0).expect("ceiling curve");
    let mut initial = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
    initial.interpolation_to_next = AutomationSegmentInterpolation::Hold;
    ceiling.set_keyframe(initial).expect("initial ceiling");
    ceiling
        .set_keyframe(ExactAutomationKeyframe::linear(
            tt(2, 1_000),
            -6.020_599_913_279_624,
        ))
        .expect("lower ceiling");
    limiter.set_parameter_automation(ceiling).expect("automated ceiling");
    let sequence = sequence_with_output_limiter(AudioChannelLayout::Mono, limiter);
    let plan = prepared_at(&sequence, 1_000, 5, AudioKernelBackend::RuntimeVectorized);
    let mut session = AudioRenderSession::new(plan).expect("automated Session");
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(104),
            start_sample: 0,
        })
        .expect("automated state entry");
    let mut source = PatternSource::mono([1.0; 7]);
    let mut pcm = [0.0; 5];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: pcm.len() },
            &mut pcm,
        )
        .expect("automated limiter render");
    assert_eq!(&pcm[..2], &[1.0, 1.0]);
    assert!(
        (pcm[2] - 0.5).abs() <= 1.0e-6,
        "ceiling moved at wrong signal sample: {pcm:?}"
    );
    assert!(pcm[3..].iter().all(|sample| *sample <= 0.5 + 1.0e-6));
}

#[test]
fn limiter_resource_and_non_finite_failures_close_before_publication() {
    let sequence =
        sequence_with_output_limiter(AudioChannelLayout::Mono, lookahead_limiter(0.0, 2.0, 100.0));
    let output = sequence.audio_program.outputs[0].id;
    let compiled = Arc::new(
        compile_audio_program(&sequence, AudioCompileRequest::program(output))
            .expect("limiter program"),
    );
    let contract = AudioRenderContract {
        sample_rate: 1_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes: 235,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    assert_eq!(
        PreparedAudioPlan::prepare(Arc::clone(&compiled), contract)
            .expect_err("one byte below the exact limiter scratch demand"),
        AudioCompileError::ProcessorScratchBudgetExceeded {
            required_bytes: 236,
            budget_bytes: 235,
        }
    );

    let plan = Arc::new(
        PreparedAudioPlan::prepare(
            compiled,
            AudioRenderContract {
                processor_session_scratch_budget_bytes: 236,
                ..contract
            },
        )
        .expect("exact scratch budget"),
    );
    let mut session = AudioRenderSession::new(plan).expect("finite-check Session");
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(105),
            start_sample: 0,
        })
        .expect("finite-check state entry");
    let mut invalid = PatternSource::mono([f32::NAN, 0.25, 0.25, 0.25]);
    let mut destination = [7.0; 1];
    assert!(matches!(
        session.render_into(
            &mut invalid,
            AudioRenderRequest { start_sample: 0, frames: 1 },
            &mut destination,
        ),
        Err(AudioExecutionError::ProcessorHost(
            AudioProcessorHostError::Process(_)
        ))
    ));
    assert_eq!(destination, [0.0]);
    assert_eq!(
        session.render_into(
            &mut PatternSource::mono([0.25; 4]),
            AudioRenderRequest { start_sample: 0, frames: 1 },
            &mut destination,
        ),
        Err(AudioExecutionError::ContinuityPoisoned(
            AudioContinuityEpoch::new(105)
        ))
    );
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(106),
            start_sample: 0,
        })
        .expect("fresh state entry");
    session
        .render_into(
            &mut PatternSource::mono([0.25; 4]),
            AudioRenderRequest { start_sample: 0, frames: 1 },
            &mut destination,
        )
        .expect("fresh finite render");
    assert_eq!(destination, [0.25]);
}
