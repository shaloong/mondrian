//! CPU compositor benchmarks using criterion.
//!
//! Measures typed color-frame compositor performance
//! at common resolutions with varying layer counts.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mondrian_core::types::{BlendMode, ColorEngine, ColorSpace};
use mondrian_renderer::{
    composite_timeline_elements_color_frame, CpuColorFrame, CpuColorTransformExecutor,
    CpuEncodedColorFrame, RenderColorTransform, RenderInputTransform, TimelineCompositeElement,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineMediaLayer,
};
use std::sync::Arc;

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

fn solid_rgba(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    std::iter::repeat_n([r, g, b, 255u8], w as usize * h as usize)
        .flatten()
        .collect()
}

fn identity_graph() -> Arc<mondrian_effects::CompiledEffectGraph> {
    mondrian_effects::get_or_compile_scheduled_render_graph(
        mondrian_effects::EffectRenderGraph::identity(),
    )
    .expect("compile identity graph")
}

fn working_frame(w: u32, h: u32, rgba: Vec<u8>) -> CpuColorFrame {
    let source = CpuEncodedColorFrame::source_rgba8(w, h, ColorSpace::Rec709, rgba);
    CpuColorTransformExecutor::input_to_working(
        &source,
        &RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart),
    )
    .expect("benchmark input transform")
}

fn bench_layers(c: &mut Criterion, name: &str, w: u32, h: u32, n: usize) {
    let layers: Vec<(CpuColorFrame, f32)> = (0..n)
        .map(|i| {
            let r = ((i * 67) % 256) as u8;
            let g = ((i * 133) % 256) as u8;
            let b = ((i * 197) % 256) as u8;
            (
                working_frame(w, h, solid_rgba(w, h, r, g, b)),
                0.5 + (i as f32 * 0.1).min(0.5),
            )
        })
        .collect();

    let elements: Vec<TimelineCompositeElement> = layers
        .iter()
        .map(|(frame, opacity)| {
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame,
                opacity: *opacity,
                blend_mode: BlendMode::Normal,
                transform: IDENTITY,
                effect_graph: identity_graph(),
                frame_seed: 0,
            })
        })
        .collect();

    let mut scratch = TimelineCompositeScratch::default();
    c.bench_function(name, |b| {
        b.iter(|| {
            let frame = composite_timeline_elements_color_frame(
                black_box(w),
                black_box(h),
                black_box(&elements),
                TimelineCompositeOptions { empty_canvas_transparent: true },
                ColorSpace::Rec709,
                &mut scratch,
            );
            CpuColorTransformExecutor::transform(
                &frame,
                &RenderColorTransform::display(
                    ColorSpace::Rec709,
                    false,
                    ColorEngine::MondrianSmart,
                ),
            )
            .expect("benchmark color transform")
            .into_rgba()
        })
    });
}

fn benchmarks(c: &mut Criterion) {
    bench_layers(c, "composite/1080p_1_layer", 1920, 1080, 1);
    bench_layers(c, "composite/1080p_4_layers", 1920, 1080, 4);
    bench_layers(c, "composite/1080p_8_layers", 1920, 1080, 8);
    bench_layers(c, "composite/4k_1_layer", 3840, 2160, 1);
    bench_layers(c, "composite/4k_8_layers", 3840, 2160, 8);
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
