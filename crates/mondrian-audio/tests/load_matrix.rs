use mondrian_audio::{
    compile_audio_program, AudioCompileRequest, AudioExecutionError, AudioKernelBackend,
    AudioPcmSource, AudioProcessingMode, AudioRenderContract, AudioRenderRequest,
    AudioRenderSession, PreparedAudioPlan,
};
use mondrian_core::{
    AssetId, AudioComponentEditId, AudioRouteId, AudioSourceComponentId, MixBusId, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioChannelStrip, AudioChannelStripOutputPort, AudioMixBus, AudioRoute, AudioRouteDestination,
    AudioRouteSource,
};
use mondrian_timeline::{Clip, Sequence};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
// At least 200 observations are required for p99 to differ from the maximum.
// Keep absolute maxima and miss counts in the report so scheduler stalls remain
// visible even when they do not describe the steady DSP distribution.
const ITERATIONS: usize = 256;

struct DeterministicSource;

impl AudioPcmSource for DeterministicSource {
    fn read_indexed_interleaved(
        &mut self,
        _edit: AudioComponentEditId,
        source_frames: &[i64],
        channels: usize,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        for (frame, source_frame) in source_frames.iter().copied().enumerate() {
            let value = if source_frame < 0 {
                0.0
            } else {
                ((source_frame % 257) as f32 - 128.0) / 257.0
            };
            destination[frame * channels..(frame + 1) * channels].fill(value);
        }
        Ok(())
    }
}

#[test]
#[ignore = "fixed-reference-machine dense schedule scalar/SIMD multitrack load matrix"]
fn dense_schedule_multitrack_load_matrix() {
    for (track_count, bus_count) in [(1_usize, 0_usize), (8, 2), (32, 8), (64, 16)] {
        let sequence = multitrack_sequence(track_count, bus_count);
        for block_frames in [64_usize, 256, 1024] {
            let scalar = prepared(&sequence, block_frames, AudioKernelBackend::ScalarReference);
            let vectorized = prepared(
                &sequence,
                block_frames,
                AudioKernelBackend::RuntimeVectorized,
            );
            assert_eq!(scalar.schedule_summary(), vectorized.schedule_summary());
            assert_eq!(scalar.schedule_summary().track_count, track_count);
            assert_eq!(scalar.schedule_summary().bus_count, bus_count);
            let expected_routes = if bus_count == 0 {
                track_count
            } else {
                track_count + bus_count
            };
            assert_eq!(scalar.schedule_summary().route_count, expected_routes);
            let scratch_slot_count = scalar.schedule_summary().scratch_slot_count;

            let (scalar_pcm, scalar_us) = run_case(scalar, block_frames);
            let (vectorized_pcm, vectorized_us) = run_case(vectorized, block_frames);
            assert_eq!(scalar_pcm, vectorized_pcm, "backend PCM parity");

            let deadline_us =
                (block_frames as u64).saturating_mul(1_000_000).div_ceil(u64::from(SAMPLE_RATE));
            let scalar_p99 = percentile(&scalar_us, 99);
            let vectorized_p99 = percentile(&vectorized_us, 99);
            let scalar_deadline_misses = deadline_misses(&scalar_us, deadline_us);
            let vectorized_deadline_misses = deadline_misses(&vectorized_us, deadline_us);
            println!(
                "MONDRIAN_AUDIO_LOAD_MATRIX={{\"profile\":\"dense_schedule_v2\",\"tracks\":{track_count},\"buses\":{bus_count},\"routes\":{expected_routes},\"block_frames\":{block_frames},\"sample_rate\":{SAMPLE_RATE},\"channels\":{CHANNELS},\"iterations\":{ITERATIONS},\"deadline_us\":{deadline_us},\"scalar_p50_us\":{},\"scalar_p95_us\":{},\"scalar_p99_us\":{scalar_p99},\"scalar_max_us\":{},\"scalar_deadline_misses\":{scalar_deadline_misses},\"vectorized_p50_us\":{},\"vectorized_p95_us\":{},\"vectorized_p99_us\":{vectorized_p99},\"vectorized_max_us\":{},\"vectorized_deadline_misses\":{vectorized_deadline_misses},\"scratch_slots\":{}}}",
                percentile(&scalar_us, 50),
                percentile(&scalar_us, 95),
                scalar_us.last().copied().unwrap_or_default(),
                percentile(&vectorized_us, 50),
                percentile(&vectorized_us, 95),
                vectorized_us.last().copied().unwrap_or_default(),
                scratch_slot_count
            );
            assert!(
                vectorized_p99 <= deadline_us,
                "dense schedule missed realtime deadline: tracks={track_count}, block={block_frames}, p99={vectorized_p99} us, deadline={deadline_us} us"
            );
        }
    }
}

#[test]
fn dense_schedule_multibus_scalar_vectorized_pcm_parity() {
    let sequence = multitrack_sequence(8, 3);
    let block_frames = 257;
    let scalar = prepared(&sequence, block_frames, AudioKernelBackend::ScalarReference);
    let vectorized = prepared(
        &sequence,
        block_frames,
        AudioKernelBackend::RuntimeVectorized,
    );
    assert_eq!(scalar.schedule_summary(), vectorized.schedule_summary());
    assert_eq!(scalar.schedule_summary().bus_count, 3);
    assert_eq!(scalar.schedule_summary().route_count, 11);
    assert_eq!(
        render_one_block(scalar, block_frames),
        render_one_block(vectorized, block_frames)
    );
}

fn multitrack_sequence(track_count: usize, bus_count: usize) -> Sequence {
    let mut sequence = Sequence::new("load-matrix");
    while sequence.audio_tracks.len() > track_count {
        let track_id = sequence.audio_tracks.last().expect("audio Track").id;
        sequence.remove_audio_track(track_id).expect("remove default Track");
    }
    while sequence.audio_tracks.len() < track_count {
        sequence.add_audio_track();
    }
    let track_ids = sequence.audio_tracks.iter().map(|track| track.id).collect::<Vec<_>>();
    for track_id in track_ids {
        let clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(60, 1).expect("duration"),
        )
        .expect("Clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("audio Clip");
    }
    if bus_count > 0 {
        let output_id = sequence.audio_program.outputs[0].id;
        sequence.audio_program.routes.clear();
        let bus_ids = (0..bus_count)
            .map(|index| {
                let id = MixBusId::new();
                sequence.audio_program.buses.push(AudioMixBus {
                    id,
                    name: format!("Load Bus {index}"),
                    strip: AudioChannelStrip::default(),
                });
                id
            })
            .collect::<Vec<_>>();
        for (index, track_id) in sequence.audio_tracks.iter().map(|track| track.id).enumerate() {
            sequence.audio_program.routes.push(AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                destination: AudioRouteDestination::Bus(bus_ids[index % bus_ids.len()]),
            });
        }
        for bus_id in bus_ids {
            sequence.audio_program.routes.push(AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Bus {
                    bus_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                destination: AudioRouteDestination::Output(output_id),
            });
        }
    }
    sequence
}

fn prepared(
    sequence: &Sequence,
    max_block_frames: usize,
    backend: AudioKernelBackend,
) -> Arc<PreparedAudioPlan> {
    let output_id = sequence.audio_program.outputs[0].id;
    let program = compile_audio_program(sequence, AudioCompileRequest::program(output_id))
        .expect("semantic IR");
    Arc::new(
        PreparedAudioPlan::prepare_with_backend(
            Arc::new(program),
            AudioRenderContract {
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS,
                max_block_frames,
                processing_mode: AudioProcessingMode::Realtime,
            },
            backend,
        )
        .expect("dense schedule"),
    )
}

fn run_case(plan: Arc<PreparedAudioPlan>, block_frames: usize) -> (Vec<f32>, Vec<u64>) {
    let mut session = AudioRenderSession::new(plan).expect("Session");
    let mut source = DeterministicSource;
    let mut destination = vec![0.0; block_frames * CHANNELS];
    let request = AudioRenderRequest { start_sample: 0, frames: block_frames };
    session.render_into(&mut source, request, &mut destination).expect("warmup");
    let parity_pcm = destination.clone();
    let mut durations = Vec::with_capacity(ITERATIONS);
    for iteration in 0..ITERATIONS {
        let request = AudioRenderRequest {
            start_sample: i64::try_from(iteration.saturating_add(1).saturating_mul(block_frames))
                .expect("sample position"),
            frames: block_frames,
        };
        let started = Instant::now();
        session
            .render_into(&mut source, request, &mut destination)
            .expect("matrix render");
        black_box(&destination);
        durations.push(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
    }
    durations.sort_unstable();
    (parity_pcm, durations)
}

fn render_one_block(plan: Arc<PreparedAudioPlan>, block_frames: usize) -> Vec<f32> {
    let mut session = AudioRenderSession::new(plan).expect("Session");
    let mut source = DeterministicSource;
    let mut destination = vec![0.0; block_frames * CHANNELS];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { start_sample: 0, frames: block_frames },
            &mut destination,
        )
        .expect("parity render");
    destination
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    let index = rank.saturating_sub(1);
    sorted[index.min(sorted.len().saturating_sub(1))]
}

fn deadline_misses(sorted: &[u64], deadline_us: u64) -> usize {
    sorted.iter().filter(|duration| **duration > deadline_us).count()
}
