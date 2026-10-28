//! Compare the actual public owned/borrowed admission APIs on the same bytes.
//! Allocation, destruction and plugin work are outside these timers. This is
//! an attribution experiment, never a production frame-budget qualification.
use mondrian_app::openfx_host::{
    diagnose_openfx_output_materialization, OpenFxHostError, OpenFxRenderDiagnostic,
    OpenFxRenderPhase,
};
use mondrian_core::{ColorFrameAlpha, ImageSampling};
use mondrian_effects::{
    EffectColorDomain, EffectFloatFrame, EffectFloatFrameView, EffectFrameExtent,
    EffectValueFormat, EffectWorkingPrecision,
};
use std::{hint::black_box, time::Instant};

type Result<T> = std::result::Result<T, OpenFxHostError>;

#[inline(never)]
fn borrowed<C: FnMut() -> Result<()> + ?Sized>(
    view: EffectFloatFrameView<'_>,
    checkpoint: &mut C,
) -> Result<()> {
    view.validate_controlled(&mut || checkpoint())
}

#[inline(never)]
fn owned<C: FnMut() -> Result<()> + ?Sized>(
    extent: EffectFrameExtent,
    format: EffectValueFormat,
    pixels: Vec<[f32; 4]>,
    checkpoint: &mut C,
) -> Result<EffectFloatFrame> {
    EffectFloatFrame::from_pixels_controlled(
        extent,
        format,
        ImageSampling::UNIT_SQUARE,
        pixels,
        &mut || checkpoint(),
    )
}

fn pair<C: FnMut() -> Result<()> + ?Sized>(
    extent: EffectFrameExtent,
    format: EffectValueFormat,
    pixels: Vec<[f32; 4]>,
    view_first: bool,
    checkpoint: &mut C,
) -> Result<(u64, u64)> {
    let expected_first = pixels[0].map(f32::to_bits);
    let (view_ns, owned_ns, frame) = if view_first {
        let view = EffectFloatFrameView::new(extent, format, ImageSampling::UNIT_SQUARE, &pixels);
        let start = Instant::now();
        let result = borrowed(black_box(view), checkpoint);
        let view_ns = start.elapsed().as_nanos() as u64;
        result?;
        let start = Instant::now();
        let result = owned(extent, format, pixels, checkpoint);
        let owned_ns = start.elapsed().as_nanos() as u64;
        (view_ns, owned_ns, result?)
    } else {
        let start = Instant::now();
        let result = owned(extent, format, pixels, checkpoint);
        let owned_ns = start.elapsed().as_nanos() as u64;
        let frame = result?;
        let start = Instant::now();
        let result = borrowed(black_box(frame.view()), checkpoint);
        let view_ns = start.elapsed().as_nanos() as u64;
        result?;
        (view_ns, owned_ns, frame)
    };
    assert_eq!(frame.pixels()[0].map(f32::to_bits), expected_first);
    black_box(frame); // Destruction stays outside both measured calls.
    Ok((view_ns, owned_ns))
}

fn stats(samples: &[u64]) -> serde_json::Value {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    serde_json::json!({
        "mean_ns": samples.iter().copied().sum::<u64>() / samples.len() as u64,
        "p50_ns": sorted[(sorted.len() * 50).div_ceil(100) - 1],
        "p95_ns": sorted[(sorted.len() * 95).div_ceil(100) - 1],
        "max_ns": sorted.last(),
    })
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        return Err("use the release profile".into());
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let width: u32 = args.first().map_or(Ok(1920), |s| s.parse())?;
    let height: u32 = args.get(1).map_or(Ok(1080), |s| s.parse())?;
    let count: usize = args.get(2).map_or(Ok(32), |s| s.parse())?;
    let alpha = ColorFrameAlpha::from_identity_code(args.get(3).map_or(Ok(2), |s| s.parse())?)
        .expect("alpha identity0..4");
    let transfer = args.get(4).map(String::as_str).unwrap_or("bulk");
    assert!(matches!(transfer, "bulk" | "blocks" | "transport"));
    let pixels = u64::from(width) * u64::from(height);
    assert!(width > 0 && height > 0 && pixels <= 8_294_400 && (8..=256).contains(&count));
    let extent = EffectFrameExtent::new(width, height);
    let format = EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        alpha,
    );
    let seed = (0..pixels)
        .map(|i| {
            let x = (i % u64::from(width)) as f32 / width as f32;
            [
                -0.25 + x,
                0.5 + x,
                2.0 - x,
                if alpha == ColorFrameAlpha::Opaque {
                    1.0
                } else {
                    0.5
                },
            ]
        })
        .collect::<Vec<_>>();
    if transfer == "transport" {
        let source: &[u8] = bytemuck::cast_slice(&seed);
        let mut copies = Vec::with_capacity(count);
        let mut admissions = Vec::with_capacity(count);
        let mut repeated_views = Vec::with_capacity(count);
        for sample in 0..(count + 4) {
            let mut report = OpenFxRenderDiagnostic::default();
            let mut calls = 0usize;
            let mut checkpoint = || {
                calls += 1;
                Ok(())
            };
            let frame = diagnose_openfx_output_materialization(
                black_box(extent),
                black_box(format),
                ImageSampling::UNIT_SQUARE,
                black_box(source),
                &mut checkpoint,
                &mut report,
            )?;
            assert!(report.owner.is_none() && report.child.is_none() && !report.accepted_output);
            assert_eq!(report.output_copy_bytes, source.len());
            let start = Instant::now();
            let result = borrowed(black_box(frame.view()), &mut checkpoint);
            let view_ns = start.elapsed().as_nanos() as u64;
            result?;
            assert!(calls > 0);
            assert_eq!(frame.pixels(), seed.as_slice());
            if sample >= 4 {
                copies.push(
                    report.elapsed(OpenFxRenderPhase::OutputCopy).expect("copy").as_nanos() as u64,
                );
                admissions.push(
                    report
                        .elapsed(OpenFxRenderPhase::OutputAdmission)
                        .expect("admission")
                        .as_nanos() as u64,
                );
                repeated_views.push(view_ns);
            }
        }
        println!(
            "{}",
            serde_json::json!({
                "diagnostic_only": true, "width": width, "height": height,
                "alpha": alpha, "samples": count, "warmup_excluded": 4,
                "pre_timer_transfer": transfer,
                "actual_output_copy": stats(&copies),
                "actual_output_admission": stats(&admissions),
                "same_output_borrowed_recheck": stats(&repeated_views),
                "conditions": "Actual private materializer with actual App error and fixed report, then a borrowed recheck on the same output. Counter-only checkpoints, owned seed bytes rather than worker mapping. No plugin/native invocation or publication identity is fabricated; this diagnoses memory/codegen only, never production performance.",
            })
        );
        return Ok(());
    }
    let mut rows = Vec::new();
    for dynamic in [false, true] {
        for view_first in [false, true] {
            let mut views = Vec::with_capacity(count);
            let mut owners = Vec::with_capacity(count);
            for sample in 0..(count + 4) {
                let pixels = black_box(if transfer == "blocks" {
                    let mut copy = Vec::with_capacity(seed.len());
                    // Match the transport's1MiB granularity without admitting
                    // another value or moving allocation into either timer.
                    for block in seed.chunks(65_536) {
                        copy.extend_from_slice(block);
                    }
                    copy
                } else {
                    seed.clone()
                });
                let mut calls = 0usize;
                let mut checkpoint = || {
                    calls += 1;
                    Ok(())
                };
                let (view, owner) = if dynamic {
                    let callback: &mut dyn FnMut() -> Result<()> = black_box(&mut checkpoint);
                    pair(
                        black_box(extent),
                        black_box(format),
                        pixels,
                        view_first,
                        callback,
                    )?
                } else {
                    pair(
                        black_box(extent),
                        black_box(format),
                        pixels,
                        view_first,
                        &mut checkpoint,
                    )?
                };
                assert_eq!(calls, 2 * ((pixels_count(extent)).div_ceil(65_536) + 1));
                if sample >= 4 {
                    views.push(view);
                    owners.push(owner);
                }
            }
            rows.push(serde_json::json!({
                "dynamic_callback": dynamic, "borrowed_first": view_first,
                "borrowed": stats(&views), "owned": stats(&owners),
            }));
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "diagnostic_only": true, "width": width, "height": height,
            "alpha": alpha, "samples_per_order_and_callback": count,
            "warmup_excluded": 4, "rows": rows,
            "error_type": std::any::type_name::<OpenFxHostError>(),
            "error_bytes": std::mem::size_of::<OpenFxHostError>(),
            "pre_timer_transfer": transfer,
            "conditions": "Same allocation and same complete validation rules in each pair, order reversed. Concrete/dynamic callback type is controlled; every checkpoint is counted. Timers exclude seed cloning/allocation/destruction/plugin work. Actual App error type, but a different call site/inlining context and counter-only callbacks. This cannot prove App attribution or qualify production performance.",
        })
    );
    Ok(())
}

fn pixels_count(extent: EffectFrameExtent) -> usize {
    extent.width() as usize * extent.height() as usize
}
