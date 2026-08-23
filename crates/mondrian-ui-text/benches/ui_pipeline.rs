//! Repeatable custom-UI pipeline benchmarks.
//!
//! These benches cover CPU-side renderer and text stages that are stable enough
//! to compare across UI infrastructure changes without requiring a GPU device.

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::atlas::TextureAtlas;
use mondrian_ui_renderer::batch::build_batches;
use mondrian_ui_renderer::{CornerRadii, DrawCommand};
use mondrian_ui_text::{resolve_text_commands, TextRenderer};
use mondrian_ui_theme::typography::{FontWeight, TextStyle};

const SCREEN: (u32, u32) = (1920, 1080);

fn style(font_size: f32) -> TextStyle {
    TextStyle {
        font_size,
        line_height: font_size * 1.3,
        font_weight: FontWeight::Regular,
        letter_spacing: 0.0,
    }
}

fn color(index: usize) -> Color {
    Color::from_rgba8(
        ((index * 73) % 255) as u8,
        ((index * 131) % 255) as u8,
        ((index * 197) % 255) as u8,
        220,
    )
}

fn rect_command(index: usize) -> DrawCommand {
    let col = index % 40;
    let row = index / 40;
    DrawCommand::Rect {
        bounds: Rect::new(8.0 + col as f32 * 46.0, 8.0 + row as f32 * 28.0, 38.0, 18.0),
        color: color(index),
        corner_radii: CornerRadii::all((index % 8) as f32),
    }
}

fn dense_rect_commands(count: usize) -> Vec<DrawCommand> {
    (0..count).map(rect_command).collect()
}

fn timeline_style_commands(track_count: usize, clips_per_track: usize) -> Vec<DrawCommand> {
    let mut commands = Vec::with_capacity(track_count * clips_per_track * 4 + track_count * 2);
    commands.push(DrawCommand::PushClip {
        bounds: Rect::new(0.0, 0.0, SCREEN.0 as f32, SCREEN.1 as f32),
    });

    for track in 0..track_count {
        let y = 42.0 + track as f32 * 28.0;
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
            let x = 120.0 + clip as f32 * 86.0 + (track % 3) as f32 * 13.0;
            let w = 52.0 + ((clip + track) % 5) as f32 * 11.0;
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
            commands.push(DrawCommand::PopClip);
        }
    }

    commands.push(DrawCommand::PopClip);
    commands
}

fn text_commands(rows: usize) -> Vec<DrawCommand> {
    (0..rows)
        .map(|index| DrawCommand::Text {
            text: format!("Clip {index:04} A你🙂B"),
            style: style(13.0),
            position: Point::new(12.0, 24.0 + index as f32 * 16.0),
            max_width: None,
            color: Color::WHITE,
        })
        .collect()
}

fn raster_upload_payloads(count: usize) -> Vec<(String, u32, u32, Arc<[u8]>)> {
    (0..count)
        .map(|index| {
            let width = 16 + (index % 5) as u32 * 8;
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
            (format!("raster-{index}"), width, height, Arc::from(rgba))
        })
        .collect()
}

fn benchmark_batching(c: &mut Criterion) {
    let rects = dense_rect_commands(4_000);
    c.bench_function("ui_renderer/build_batches_dense_rects_4k", |b| {
        b.iter(|| build_batches(black_box(&rects), black_box(SCREEN)))
    });

    let timeline = timeline_style_commands(36, 48);
    c.bench_function("ui_renderer/build_batches_timeline_style_stream", |b| {
        b.iter(|| build_batches(black_box(&timeline), black_box(SCREEN)))
    });
}

fn benchmark_atlas(c: &mut Criterion) {
    c.bench_function("ui_renderer/texture_atlas_mixed_size_churn", |b| {
        b.iter_batched(
            || {
                (0..384)
                    .map(|index| {
                        let width = 8 + (index % 11) as u32 * 5;
                        let height = 8 + (index % 13) as u32 * 4;
                        (format!("glyph-or-icon-{index}"), width, height)
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

    let payloads = raster_upload_payloads(512);
    c.bench_function("ui_renderer/raster_atlas_allocation_pressure", |b| {
        b.iter(|| {
            let mut atlas = TextureAtlas::new(2048, 2048);
            let mut byte_count = 0usize;
            for (key, width, height, rgba) in &payloads {
                byte_count = byte_count.saturating_add(rgba.len());
                black_box(atlas.allocate_pixels(key, *width, *height));
            }
            black_box((atlas.stats(), byte_count))
        })
    });
}

fn benchmark_text(c: &mut Criterion) {
    let commands = text_commands(32);
    let mut renderer = TextRenderer::new();
    let _ = resolve_text_commands(commands.clone(), &mut renderer);

    c.bench_function("ui_text/resolve_text_commands_mixed_scripts", |b| {
        b.iter(|| {
            let resolved =
                resolve_text_commands(black_box(commands.clone()), black_box(&mut renderer));
            black_box((resolved.stats, resolved.commands.len()))
        })
    });
}

fn benchmarks(c: &mut Criterion) {
    benchmark_batching(c);
    benchmark_atlas(c);
    benchmark_text(c);
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
