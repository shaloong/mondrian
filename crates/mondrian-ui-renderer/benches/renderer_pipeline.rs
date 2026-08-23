//! Repeatable CPU-side renderer pipeline benchmarks.
//!
//! These benchmarks avoid native surfaces and GPU devices so they stay stable
//! across developer machines while still covering renderer IR, batching,
//! atlas churn, raster payload command encoding, and retained command replay.

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::atlas::TextureAtlas;
use mondrian_ui_renderer::batch::build_batches;
use mondrian_ui_renderer::{CornerRadii, DrawCommand, DrawEncoder, RetainedDrawCommands};

const SCREEN: (u32, u32) = (1920, 1080);

type RasterPayload = (String, Rect, u32, u32, Arc<[u8]>);

fn color(index: usize) -> Color {
    Color::from_rgba8(
        ((index * 73) % 255) as u8,
        ((index * 131) % 255) as u8,
        ((index * 197) % 255) as u8,
        220,
    )
}

fn rect_command(index: usize) -> DrawCommand {
    let col = index % 56;
    let row = index / 56;
    DrawCommand::Rect {
        bounds: Rect::new(8.0 + col as f32 * 34.0, 8.0 + row as f32 * 22.0, 27.0, 15.0),
        color: color(index),
        corner_radii: CornerRadii::all((index % 8) as f32),
    }
}

fn dense_rect_commands(count: usize) -> Vec<DrawCommand> {
    (0..count).map(rect_command).collect()
}

fn timeline_style_commands(track_count: usize, clips_per_track: usize) -> Vec<DrawCommand> {
    let mut commands = Vec::with_capacity(track_count * clips_per_track * 5 + track_count * 2);
    commands.push(DrawCommand::PushClip {
        bounds: Rect::new(0.0, 0.0, SCREEN.0 as f32, SCREEN.1 as f32),
    });

    for track in 0..track_count {
        let y = 36.0 + track as f32 * 28.0;
        commands.push(DrawCommand::Rect {
            bounds: Rect::new(0.0, y, SCREEN.0 as f32, 26.0),
            color: if track % 2 == 0 {
                Color::from_rgba8(36, 39, 48, 255)
            } else {
                Color::from_rgba8(42, 45, 55, 255)
            },
            corner_radii: CornerRadii::all(0.0),
        });
        commands.push(DrawCommand::Line {
            start: Point::new(0.0, y + 26.0),
            end: Point::new(SCREEN.0 as f32, y + 26.0),
            width: 1.0,
            color: Color::from_rgba8(72, 76, 88, 255),
        });

        for clip in 0..clips_per_track {
            let x = 112.0 + clip as f32 * 74.0 + (track % 3) as f32 * 13.0;
            let w = 46.0 + ((clip + track) % 7) as f32 * 9.0;
            commands.push(DrawCommand::PushClip { bounds: Rect::new(x, y + 3.0, w, 20.0) });
            commands.push(DrawCommand::Rect {
                bounds: Rect::new(x, y + 3.0, w, 20.0),
                color: if track % 2 == 0 {
                    Color::from_rgba8(72, 132, 184, 255)
                } else {
                    Color::from_rgba8(75, 160, 148, 255)
                },
                corner_radii: CornerRadii::all(4.0),
            });
            commands.push(DrawCommand::RasterAtlasImage {
                bounds: Rect::new(x + 4.0, y + 6.0, 14.0, 14.0),
                uv_rect: Rect::new(0.0, 0.0, 0.125, 0.125),
                tint: Color::WHITE,
            });
            commands.push(DrawCommand::Line {
                start: Point::new(x + w - 1.0, y + 5.0),
                end: Point::new(x + w - 1.0, y + 21.0),
                width: 1.0,
                color: Color::from_rgba8(255, 255, 255, 92),
            });
            commands.push(DrawCommand::PopClip);
        }
    }

    commands.push(DrawCommand::PopClip);
    commands
}

fn raster_payloads(count: usize) -> Vec<RasterPayload> {
    (0..count)
        .map(|index| {
            let width = 16 + (index % 9) as u32 * 7;
            let height = 16 + (index % 7) as u32 * 6;
            let rgba = (0..width * height)
                .flat_map(|pixel| {
                    let seed = index as u32 * 17 + pixel;
                    [
                        (seed & 0xff) as u8,
                        ((seed >> 3) & 0xff) as u8,
                        ((seed >> 5) & 0xff) as u8,
                        255,
                    ]
                })
                .collect::<Vec<_>>();
            let col = index % 32;
            let row = index / 32;
            (
                format!("raster-{index}"),
                Rect::new(8.0 + col as f32 * 28.0, 8.0 + row as f32 * 22.0, 24.0, 18.0),
                width,
                height,
                Arc::from(rgba),
            )
        })
        .collect()
}

fn benchmark_batching(c: &mut Criterion) {
    let rects = dense_rect_commands(8_000);
    c.bench_function("ui_renderer/build_batches_dense_rects_8k", |b| {
        b.iter(|| build_batches(black_box(&rects), black_box(SCREEN)))
    });

    let timeline = timeline_style_commands(42, 52);
    c.bench_function("ui_renderer/build_batches_timeline_style_stream", |b| {
        b.iter(|| build_batches(black_box(&timeline), black_box(SCREEN)))
    });
}

fn benchmark_atlas(c: &mut Criterion) {
    c.bench_function("ui_renderer/texture_atlas_fragmentation_churn", |b| {
        b.iter_batched(
            || {
                (0..768)
                    .map(|index| {
                        let width = 8 + (index % 17) as u32 * 4;
                        let height = 8 + (index % 19) as u32 * 3;
                        (format!("atlas-item-{index}"), width, height)
                    })
                    .collect::<Vec<_>>()
            },
            |items| {
                let mut atlas = TextureAtlas::new(2048, 2048);
                for (key, width, height) in items {
                    black_box(atlas.allocate_pixels(&key, width, height));
                }
                black_box(atlas.stats())
            },
            BatchSize::SmallInput,
        )
    });
}

fn benchmark_raster_commands(c: &mut Criterion) {
    let payloads = raster_payloads(1_024);
    c.bench_function("ui_renderer/raster_image_command_payloads_1k", |b| {
        b.iter(|| {
            let mut encoder = DrawEncoder::new();
            for (key, bounds, width, height, rgba) in &payloads {
                encoder.draw_raster_image(
                    black_box(key),
                    black_box(*bounds),
                    black_box(*width),
                    black_box(*height),
                    black_box(mondrian_ui_core::RasterImageColorSpace::Srgb),
                    Arc::clone(rgba),
                    black_box(Color::WHITE),
                );
            }
            black_box(encoder.finish())
        })
    });
}

fn benchmark_retained_replay(c: &mut Criterion) {
    let retained = RetainedDrawCommands::new(timeline_style_commands(18, 36))
        .expect("benchmark command stream should be structurally valid");
    c.bench_function("ui_renderer/retained_timeline_command_replay", |b| {
        b.iter(|| {
            let mut encoder = DrawEncoder::new();
            retained.replay_into(black_box(&mut encoder));
            black_box(encoder.finish())
        })
    });
}

fn benchmarks(c: &mut Criterion) {
    benchmark_batching(c);
    benchmark_atlas(c);
    benchmark_raster_commands(c);
    benchmark_retained_replay(c);
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
