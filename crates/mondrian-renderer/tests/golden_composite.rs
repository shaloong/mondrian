//! Golden image tests — pixel-level comparison to catch visual regressions.
//!
//! Each test renders a known scene and compares the output against a
//! reference PNG stored in `tests/golden/`. Tolerance: ±1 per channel.
//!
//! To regenerate golden images (after intentional visual changes):
//!   `env MONDRIAN_UPDATE_GOLDEN=1 cargo test -p mondrian-renderer --test golden_composite`

use std::path::PathBuf;
use std::sync::Arc;

use mondrian_core::types::{BlendMode, ColorEngine, ColorSpace};
use mondrian_renderer::{
    composite_timeline_elements_color_frame, CpuColorFrame, CpuColorTransformExecutor,
    RenderColorTransform, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineMediaLayer,
};

const IDENTITY_TRANSFORM: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

// ── Helpers ───────────────────────────────────────────────────────────

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn golden_dir() -> PathBuf {
    project_dir().join("tests/golden")
}

fn should_update() -> bool {
    std::env::var("MONDRIAN_UPDATE_GOLDEN").is_ok_and(|v| v == "1")
}

fn solid_rgba(w: u32, h: u32, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    std::iter::repeat_n([r, g, b, a], w as usize * h as usize).flatten().collect()
}

fn identity_graph() -> Arc<mondrian_effects::CompiledEffectGraph> {
    let g = mondrian_effects::EffectRenderGraph::identity();
    mondrian_effects::get_or_compile_scheduled_render_graph(g).expect("compile identity graph")
}

fn encode_rec709(frame: &CpuColorFrame) -> Vec<u8> {
    CpuColorTransformExecutor::transform(
        frame,
        &RenderColorTransform::display(ColorSpace::Rec709, false, ColorEngine::MondrianSmart),
    )
    .expect("encode golden frame")
    .into_rgba()
}

fn composite_single_layer(w: u32, h: u32, rgba: &[u8], opacity: f32, blend: BlendMode) -> Vec<u8> {
    let elements = vec![TimelineCompositeElement::Media(TimelineMediaLayer {
        rgba,
        width: w,
        height: h,
        opacity,
        blend_mode: blend,
        transform: IDENTITY_TRANSFORM,
        effect_graph: identity_graph(),
        frame_seed: 0,
    })];
    let mut scratch = TimelineCompositeScratch::default();
    let frame = composite_timeline_elements_color_frame(
        w,
        h,
        &elements,
        TimelineCompositeOptions { empty_canvas_transparent: true },
        ColorSpace::Rec709,
        &mut scratch,
    );
    encode_rec709(&frame)
}

fn assert_rgba8_equal(actual: &[u8], expected: &[u8], w: u32, _h: u32, name: &str) {
    assert_eq!(actual.len(), expected.len(), "{name}: size mismatch");
    let mut failures = 0u32;
    for i in (0..actual.len()).step_by(4) {
        let ar = actual[i] as i32;
        let ag = actual[i + 1] as i32;
        let ab = actual[i + 2] as i32;
        let aa = actual[i + 3] as i32;
        let er = expected[i] as i32;
        let eg = expected[i + 1] as i32;
        let eb = expected[i + 2] as i32;
        let ea = expected[i + 3] as i32;
        if (ar - er).abs() > 1 || (ag - eg).abs() > 1 || (ab - eb).abs() > 1 || (aa - ea).abs() > 1
        {
            if failures < 5 {
                let px = (i / 4) as u32 % w;
                let py = (i / 4) as u32 / w;
                eprintln!("{name}: pixel ({px},{py}) actual=[{ar},{ag},{ab},{aa}] expected=[{er},{eg},{eb},{ea}]");
            }
            failures += 1;
        }
    }
    assert_eq!(
        failures, 0,
        "{name}: {failures} pixels differ beyond ±1 tolerance"
    );
}

fn check_golden(name: &str, w: u32, h: u32, actual: &[u8]) {
    let path = golden_dir().join(name);
    if should_update() || !path.exists() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        image::save_buffer(&path, actual, w, h, image::ColorType::Rgba8)
            .unwrap_or_else(|e| panic!("{name}: failed to save golden: {e}"));
        if should_update() {
            return; // Updated, don't compare
        }
        panic!("{name}: golden image saved at {path:?} — re-run without MONDRIAN_UPDATE_GOLDEN to verify");
    }
    let expected = image::open(&path)
        .unwrap_or_else(|e| panic!("{name}: failed to open golden: {e}"))
        .into_rgba8()
        .into_raw();
    assert_rgba8_equal(actual, &expected, w, h, name);
}

// ── Tests ────────────────────────────────────────────────────────────

#[test]
fn golden_transparent_canvas() {
    let w = 64;
    let h = 64;
    let elements: Vec<TimelineCompositeElement> = vec![];
    let mut scratch = TimelineCompositeScratch::default();
    let frame = composite_timeline_elements_color_frame(
        w,
        h,
        &elements,
        TimelineCompositeOptions { empty_canvas_transparent: true },
        ColorSpace::Rec709,
        &mut scratch,
    );
    let result = encode_rec709(&frame);
    check_golden("transparent_canvas_64x64.png", w, h, &result);
}

#[test]
fn golden_opaque_white() {
    let w = 64;
    let h = 64;
    let data = solid_rgba(w, h, 255, 255, 255, 255);
    let result = composite_single_layer(w, h, &data, 1.0, BlendMode::Normal);
    check_golden("opaque_white_64x64.png", w, h, &result);
}

#[test]
fn golden_opaque_red() {
    let w = 64;
    let h = 64;
    let data = solid_rgba(w, h, 255, 0, 0, 255);
    let result = composite_single_layer(w, h, &data, 1.0, BlendMode::Normal);
    check_golden("opaque_red_64x64.png", w, h, &result);
}

#[test]
fn golden_half_opacity_red_over_black() {
    let w = 64;
    let h = 64;
    let data = solid_rgba(w, h, 255, 0, 0, 128);
    let result = composite_single_layer(w, h, &data, 1.0, BlendMode::Normal);
    check_golden("half_red_64x64.png", w, h, &result);
}

#[test]
fn golden_two_layers_normal() {
    let w = 64;
    let h = 64;
    let bg = solid_rgba(w, h, 255, 0, 0, 255);
    let fg = solid_rgba(w, h, 0, 255, 0, 128);
    let elements = vec![
        TimelineCompositeElement::Media(TimelineMediaLayer {
            rgba: &bg,
            width: w,
            height: h,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: IDENTITY_TRANSFORM,
            effect_graph: identity_graph(),
            frame_seed: 0,
        }),
        TimelineCompositeElement::Media(TimelineMediaLayer {
            rgba: &fg,
            width: w,
            height: h,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: IDENTITY_TRANSFORM,
            effect_graph: identity_graph(),
            frame_seed: 0,
        }),
    ];
    let mut scratch = TimelineCompositeScratch::default();
    let frame = composite_timeline_elements_color_frame(
        w,
        h,
        &elements,
        TimelineCompositeOptions { empty_canvas_transparent: true },
        ColorSpace::Rec709,
        &mut scratch,
    );
    let result = encode_rec709(&frame);
    check_golden("two_layers_normal_64x64.png", w, h, &result);
}
