use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mondrian_core::timeline_data::RenderPlanSource;
use mondrian_core::{AssetId, Color, TimelineTime};
use mondrian_timeline::{Clip, PreparedVisualSchedule, Sequence, Track};

const TRACKS: i64 = 8;
const CLIPS_PER_TRACK: i64 = 1_250;

fn time(frame: i64) -> TimelineTime {
    TimelineTime::new(frame, 25).expect("benchmark time must be valid")
}

fn sparse_sequence() -> Sequence {
    let mut sequence = Sequence::new("visual schedule benchmark");
    sequence.video_tracks.clear();
    for track_index in 0..TRACKS {
        let mut track = Track::new_video(format!("V{}", track_index + 1));
        for clip_index in 0..CLIPS_PER_TRACK {
            let position = clip_index * 10 + track_index;
            track
                .add_clip(
                    Clip::new_solid_color(AssetId::new(), Color::BLACK, time(position), time(2))
                        .expect("benchmark Clip must be valid"),
                )
                .expect("sparse benchmark Clips must not overlap");
        }
        sequence.video_tracks.push(track);
    }
    sequence
}

fn benchmark_visual_schedule(c: &mut Criterion) {
    let sequence = sparse_sequence();
    let schedule =
        PreparedVisualSchedule::compile(&sequence).expect("benchmark schedule must prepare");
    let query_time = time((CLIPS_PER_TRACK / 2) * 10);
    let mut group = c.benchmark_group("visual_schedule_10000_sparse_clips");

    group.bench_function("direct_sequence_reference", |b| {
        b.iter(|| {
            RenderPlanSource::flat_visual_items_at(&sequence, black_box(query_time))
                .expect("reference evaluation must succeed")
        });
    });
    group.bench_function("prepared_interval_index", |b| {
        b.iter(|| {
            RenderPlanSource::flat_visual_items_at(&schedule, black_box(query_time))
                .expect("prepared evaluation must succeed")
        });
    });
    group.finish();
}

criterion_group!(benches, benchmark_visual_schedule);
criterion_main!(benches);
