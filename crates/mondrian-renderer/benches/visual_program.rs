use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use mondrian_core::{AssetId, Color, FramePosition, TimelineTime};
use mondrian_renderer::{
    PreparedVisualProgram, TimelineCompositeScratch, TimelineEvaluationRequest,
};
use mondrian_timeline::{Clip, Sequence, Track};

fn timeline_time(frame: i64, sequence: &Sequence) -> TimelineTime {
    TimelineTime::from_frame_position(FramePosition::new(frame, sequence.time_base()))
        .expect("benchmark frame must be representable")
}

fn sparse_sequence(clip_count: usize) -> Sequence {
    let mut sequence = Sequence::new(format!("{clip_count} Clip visual benchmark"));
    sequence.video_tracks.clear();
    let mut track = Track::new_video("V1");
    for index in 0..clip_count {
        let frame = i64::try_from(index).expect("benchmark Clip count fits i64").saturating_mul(4);
        track
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    Color::BLACK,
                    timeline_time(frame, &sequence),
                    timeline_time(1, &sequence),
                )
                .expect("benchmark Clip"),
            )
            .expect("append non-overlapping benchmark Clip");
    }
    sequence.video_tracks.push(track);
    sequence
}

fn visual_program_benchmark(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("visual_program_frame_evaluation");
    for clip_count in [1_000_usize, 10_000] {
        let sequence = sparse_sequence(clip_count);
        let program =
            PreparedVisualProgram::prepare(&sequence).expect("prepare benchmark visual program");
        let frame = i64::try_from(clip_count - 1)
            .expect("benchmark Clip count fits i64")
            .saturating_mul(4);
        let request = TimelineEvaluationRequest::preview(
            FramePosition::new(frame, sequence.time_base()),
            1.0,
        );

        group.bench_with_input(
            BenchmarkId::new("cold_program_preparation", clip_count),
            &clip_count,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        PreparedVisualProgram::prepare(black_box(&sequence))
                            .expect("cold benchmark preparation"),
                    )
                });
            },
        );
        let mut scratch = TimelineCompositeScratch::default();
        group.bench_with_input(
            BenchmarkId::new("prepared_session_evaluation", clip_count),
            &clip_count,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        scratch
                            .evaluate_prepared_visual_program(
                                black_box(&program),
                                black_box(request),
                            )
                            .expect("prepared benchmark evaluation"),
                    )
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, visual_program_benchmark);
criterion_main!(benches);
