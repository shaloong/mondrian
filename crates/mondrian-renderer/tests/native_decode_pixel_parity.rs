//! Real decoder agreement through the production Viewer and isolated demux.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::{bail, Context, Result};
use mondrian_core::{
    BlendMode, ColorEngine, ColorSpace, SequenceId, SourceSampleTarget, TimelineTime,
    WorkingColorSpace,
};
use mondrian_media::preview::*;
use mondrian_media::{
    CpuYuvChromaPlanes, CpuYuvSampleFormat, DecodedGpuFrameHandleKind, DecodedVideoRange,
    DecodedVideoSurfaceFormat, MediaFileFingerprint,
};
use mondrian_renderer::color::ProgramOutputBoundary as RenderOutputColorBoundary;
use mondrian_renderer::*;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
#[path = "support/viewer_retirement.rs"]
mod retirement;

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires NVIDIA CUDA/Vulkan, explicit demux worker and 30 fps SDR fixtures"]
fn cpu_and_cuda_decoded_viewer_pixels_agree_across_gop_seeks() -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let fixtures =
        std::env::var_os("MONDRIAN_CUDA_NATIVE_FIXTURES").context("explicit fixtures")?;
    let context = pollster::block_on(GpuContext::new())?;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime
        .reconfigure_resource_grant(ViewerGpuExecutionResourceGrant::professional_realtime()));
    let result = (|| -> Result<()> {
        for path in std::env::split_paths(&fixtures) {
            let cpu = measure(&path, false, None, &context, &mut runtime)?;
            let native = measure(&path, true, None, &context, &mut runtime)?;
            anyhow::ensure!(cpu.len() == native.len());
            for (index, (cpu, native)) in cpu.iter().zip(&native).enumerate() {
                anyhow::ensure!(cpu.len() == native.len());
                let max =
                    cpu.iter().zip(native).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
                eprintln!(
                    "{} seek sample {index}: maximum float output difference {max}",
                    path.display()
                );
                // Same reconstruction semantics through the production half-float Viewer.
                // Permit one half-float ULP at unity for rounding, below an 8-bit code step.
                anyhow::ensure!(
                    max <= 1.0 / 1024.0,
                    "CPU/CUDA decoded pixel disagreement: {max}"
                );
            }
        }
        Ok(())
    })();
    let closure = retirement::retire_runtime(&context.device, runtime);
    result?;
    closure?;
    Ok(())
}
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker and a 30 fps Main10 PQ motion/B fixture"]
fn cpu_and_native_main10_motion_viewer_pixels_agree_across_gop_seeks() -> Result<()> {
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_MAIN10_MOTION_FIXTURE",
        [(DecodedVideoSurfaceFormat::P010, ColorSpace::Rec2100Pq)],
        1.0,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker and nine lossless CICP-tagged sixteen-bit FFV1 planar fixtures"]
fn sixteen_bit_planar_420_422_444_share_compact_viewer_color_path() -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let fixtures = std::env::var_os("MONDRIAN_PLANAR_16_FIXTURES")
        .context("explicit sixteen-bit planar fixtures")?;
    let paths: Vec<_> = std::env::split_paths(&fixtures).collect();
    anyhow::ensure!(
        paths.len() == 9,
        "expected three chroma grids in SDR, PQ and HLG"
    );
    let context = pollster::block_on(GpuContext::new())?;
    anyhow::ensure!(context.adapter.get_info().backend == wgpu::Backend::Metal);
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime
        .reconfigure_resource_grant(ViewerGpuExecutionResourceGrant::professional_realtime()));
    let result = (|| -> Result<()> {
        let mut reference = None;
        for (i, path) in paths.iter().enumerate() {
            let surface = match i % 3 {
                0 => DecodedVideoSurfaceFormat::Yuv420p16le,
                1 => DecodedVideoSurfaceFormat::Yuv422p16le,
                _ => DecodedVideoSurfaceFormat::Yuv444p16le,
            };
            let color = match i / 3 {
                0 => ColorSpace::Rec709,
                1 => ColorSpace::Rec2100Pq,
                _ => ColorSpace::Rec2100Hlg,
            };
            let pixels = measure_with_resource_observer(
                path,
                false,
                None,
                Some(surface),
                color,
                &context,
                &mut runtime,
                |_, _, _| {},
            )?;
            if i % 3 == 0 {
                reference = Some(pixels);
            } else {
                let expected = reference.as_ref().context("missing 4:2:0 reference")?;
                anyhow::ensure!(pixels.len() == expected.len());
                for (sample, (expected, actual)) in expected.iter().zip(&pixels).enumerate() {
                    anyhow::ensure!(actual.len() == expected.len());
                    let max = expected
                        .iter()
                        .zip(actual)
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0_f32, f32::max);
                    eprintln!(
                        "{color:?} {surface:?} sample {sample}: maximum pixel difference {max}"
                    );
                    anyhow::ensure!(max <= 1.0 / 1024.0, "planar grid color disagreement: {max}");
                }
            }
        }
        Ok(())
    })();
    let closure = retirement::retire_runtime(&context.device, runtime);
    result?;
    closure?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker and twelve ProRes422 profile/color motion fixtures"]
fn cpu_and_native_prores422_viewer_pixels_agree_across_seeks() -> Result<()> {
    let colors = [
        ColorSpace::Rec709,
        ColorSpace::Rec2100Pq,
        ColorSpace::Rec2100Hlg,
    ];
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_PRORES422_FIXTURES",
        std::array::from_fn::<_, 12, _>(|index| {
            (DecodedVideoSurfaceFormat::P210, colors[index % 3])
        }),
        1.0,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, a ProRes422 source and lossless planar capture of the same VideoToolbox x422 samples"]
fn prores422_matched_reconstruction_uses_the_same_viewer_color_path() -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let fixtures = std::env::var_os("MONDRIAN_MACOS_PRORES422_MATCHED_RECONSTRUCTION")
        .context("explicit ProRes422 source and matching VideoToolbox sample capture")?;
    let color = match std::env::var("MONDRIAN_MACOS_PRORES422_MATCHED_COLOR")?.as_str() {
        "sdr" => ColorSpace::Rec709,
        "pq" => ColorSpace::Rec2100Pq,
        "hlg" => ColorSpace::Rec2100Hlg,
        other => anyhow::bail!("unsupported matched ProRes422 color: {other}"),
    };
    let paths: Vec<_> = std::env::split_paths(&fixtures).collect();
    anyhow::ensure!(
        paths.len() == 2,
        "expected ProRes source and matched FFV1 capture"
    );
    let context = pollster::block_on(GpuContext::new())?;
    anyhow::ensure!(context.adapter.get_info().backend == wgpu::Backend::Metal);
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime
        .reconfigure_resource_grant(ViewerGpuExecutionResourceGrant::professional_realtime()));
    let result = (|| -> Result<()> {
        let native = measure_with_resource_observer(
            &paths[0],
            true,
            Some((
                DecodedVideoSurfaceFormat::P210,
                PreviewDecodePath::InProcessVideoToolboxNative,
            )),
            None,
            color,
            &context,
            &mut runtime,
            |_, _, _| {},
        )?;
        let captured = measure_with_resource_observer(
            &paths[1],
            false,
            None,
            None,
            color,
            &context,
            &mut runtime,
            |_, _, _| {},
        )?;
        anyhow::ensure!(native.len() == captured.len());
        for (index, (native, captured)) in native.iter().zip(captured).enumerate() {
            anyhow::ensure!(native.len() == captured.len());
            let max =
                native.iter().zip(captured).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
            eprintln!("matched ProRes422 sample {index}: maximum Viewer difference {max}");
            anyhow::ensure!(
                max <= 1.0 / 1024.0,
                "matched decoder sample changed in Viewer"
            );
        }
        Ok(())
    })();
    let closure = retirement::retire_runtime(&context.device, runtime);
    result?;
    closure?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker and six SDR/PQ/HLG HEVC 422/444 motion fixtures"]
fn cpu_and_native_hevc_range_extensions_viewer_pixels_agree_across_gop_seeks() -> Result<()> {
    let colors = [
        ColorSpace::Rec709,
        ColorSpace::Rec2100Pq,
        ColorSpace::Rec2100Hlg,
    ];
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_HEVC_REXT_FIXTURES",
        std::array::from_fn::<_, 6, _>(|index| {
            (
                if index < 3 {
                    DecodedVideoSurfaceFormat::P210
                } else {
                    DecodedVideoSurfaceFormat::P410
                },
                colors[index % 3],
            )
        }),
        1.0,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker, and tagged HEVC 4:2:0/4:2:2/4:4:4 fixtures"]
fn cpu_and_videotoolbox_decoded_viewer_pixels_agree_across_gop_seeks() -> Result<()> {
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_NATIVE_FIXTURES",
        [
            (DecodedVideoSurfaceFormat::P010, ColorSpace::Rec709),
            (DecodedVideoSurfaceFormat::P210, ColorSpace::Rec709),
            (DecodedVideoSurfaceFormat::P410, ColorSpace::Rec709),
        ],
        1.0,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker, and tagged 30 fps PQ/HLG HEVC fixtures"]
fn cpu_and_videotoolbox_hdr_viewer_pixels_agree_across_gop_seeks() -> Result<()> {
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_NATIVE_HDR_FIXTURES",
        [
            (DecodedVideoSurfaceFormat::P010, ColorSpace::Rec2100Pq),
            (DecodedVideoSurfaceFormat::P210, ColorSpace::Rec2100Pq),
            (DecodedVideoSurfaceFormat::P410, ColorSpace::Rec2100Hlg),
        ],
        1.0,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker and tagged 30 fps SDR/PQ/HLG ProRes 4444 alpha fixtures"]
fn cpu_and_prores4444_alpha_viewer_pixels_agree_across_seeks() -> Result<()> {
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_PRORES_ALPHA_FIXTURES",
        [
            (DecodedVideoSurfaceFormat::Yuva444sp16, ColorSpace::Rec709),
            (
                DecodedVideoSurfaceFormat::Yuva444sp16,
                ColorSpace::Rec2100Pq,
            ),
            (
                DecodedVideoSurfaceFormat::Yuva444sp16,
                ColorSpace::Rec2100Hlg,
            ),
        ],
        0.5,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires real Metal, explicit demux worker and tagged 30 fps SDR/PQ/HLG ProRes 4444 zero-alpha fixtures"]
fn transparent_prores4444_viewer_output_is_zero_across_seeks() -> Result<()> {
    compare_macos_videotoolbox_fixtures(
        "MONDRIAN_MACOS_PRORES_ZERO_ALPHA_FIXTURES",
        [
            (DecodedVideoSurfaceFormat::Yuva444sp16, ColorSpace::Rec709),
            (
                DecodedVideoSurfaceFormat::Yuva444sp16,
                ColorSpace::Rec2100Pq,
            ),
            (
                DecodedVideoSurfaceFormat::Yuva444sp16,
                ColorSpace::Rec2100Hlg,
            ),
        ],
        0.0,
        PreviewDecodePath::InProcessVideoToolboxNative,
    )
}

#[cfg(target_os = "macos")]
fn compare_macos_videotoolbox_fixtures<const N: usize>(
    fixture_env: &str,
    cases: [(DecodedVideoSurfaceFormat, ColorSpace); N],
    expected_alpha: f32,
    expected_native_decode_path: PreviewDecodePath,
) -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let fixtures = std::env::var_os(fixture_env).with_context(|| fixture_env.to_owned())?;
    let paths: Vec<_> = std::env::split_paths(&fixtures).collect();
    anyhow::ensure!(
        paths.len() == cases.len(),
        "fixture count differs from tagged cases"
    );
    let context = pollster::block_on(GpuContext::new())?;
    anyhow::ensure!(context.adapter.get_info().backend == wgpu::Backend::Metal);
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime
        .reconfigure_resource_grant(ViewerGpuExecutionResourceGrant::professional_realtime()));
    let result = (|| -> Result<()> {
        let mut pixel_failures = Vec::new();
        for (path, (surface, color)) in paths.into_iter().zip(cases) {
            let cpu = measure_with_resource_observer(
                &path,
                false,
                Some((surface, expected_native_decode_path)),
                None,
                color,
                &context,
                &mut runtime,
                |_, _, _| {},
            )?;
            let native = measure_with_resource_observer(
                &path,
                true,
                Some((surface, expected_native_decode_path)),
                None,
                color,
                &context,
                &mut runtime,
                |_, _, _| {},
            )?;
            for output in cpu.iter().chain(&native) {
                anyhow::ensure!(
                    output.iter().all(|value| value.is_finite()),
                    "Viewer output contains a non-finite component"
                );
                let tolerance = if expected_alpha == 0.5 { 0.01 } else { 1e-5 };
                anyhow::ensure!(output
                    .chunks_exact(4)
                    .all(|pixel| (pixel[3] - expected_alpha).abs() < tolerance));
                if expected_alpha == 0.0 {
                    anyhow::ensure!(
                        output.iter().all(|value| value.abs() < 1e-5),
                        "transparent source leaked hidden RGB into composite output"
                    );
                }
            }
            anyhow::ensure!(cpu.len() == native.len());
            for (index, (cpu, native)) in cpu.iter().zip(&native).enumerate() {
                anyhow::ensure!(cpu.len() == native.len());
                let (maximum_offset, max) = cpu
                    .iter()
                    .zip(native)
                    .enumerate()
                    .map(|(offset, (a, b))| (offset, (a - b).abs()))
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .context("empty Viewer output")?;
                let pixel_offset = maximum_offset / 4 * 4;
                eprintln!(
                    "{} {color:?} seek sample {index}: maximum float output difference {max}, rgba_offset={maximum_offset}, cpu={:?}, native={:?}",
                    path.display(), &cpu[pixel_offset..pixel_offset + 4], &native[pixel_offset..pixel_offset + 4]
                );
                if max > 1.0 / 1024.0 {
                    pixel_failures.push(format!("{} sample {index}: {max}", path.display()));
                }
            }
        }
        anyhow::ensure!(
            pixel_failures.is_empty(),
            "CPU/VideoToolbox Viewer pixel disagreement: {pixel_failures:?}"
        );
        Ok(())
    })();
    let closure = retirement::retire_runtime(&context.device, runtime);
    result?;
    closure?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan, explicit demux worker and equivalent tagged planar SDR fixtures"]
fn compact_planar_formats_agree_through_production_viewer() -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let fixtures = std::env::var_os("MONDRIAN_PLANAR_PARITY_FIXTURES")
        .context("explicit equivalent fixtures")?;
    let paths: Vec<_> = std::env::split_paths(&fixtures).collect();
    anyhow::ensure!(
        paths.len() == 4,
        "expected 422p, 444p, 444p10le and 444p12le equivalent fixtures"
    );
    let context = pollster::block_on(GpuContext::new())?;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime
        .reconfigure_resource_grant(ViewerGpuExecutionResourceGrant::professional_realtime()));
    let result = (|| -> Result<()> {
        let reference = measure(&paths[0], false, None, &context, &mut runtime)?;
        for path in &paths[1..] {
            let actual = measure(path, false, None, &context, &mut runtime)?;
            anyhow::ensure!(actual.len() == reference.len());
            for (expected, actual) in reference.iter().zip(actual) {
                anyhow::ensure!(expected.len() == actual.len());
                let max =
                    expected.iter().zip(actual).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
                eprintln!("{} planar Viewer maximum difference {max}", path.display());
                anyhow::ensure!(max <= 1.0 / 1024.0, "planar output disagreement: {max}");
            }
        }
        Ok(())
    })();
    let closure = retirement::retire_runtime(&context.device, runtime);
    result?;
    closure?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn measure(
    path: &Path,
    native: bool,
    expected_native_surface: Option<DecodedVideoSurfaceFormat>,
    context: &GpuContext,
    runtime: &mut ViewerGpuExecutionRuntime,
) -> Result<Vec<Vec<f32>>> {
    measure_with_resource_observer(
        path,
        native,
        expected_native_surface.map(|surface| (surface, PreviewDecodePath::InProcessFfmpegNative)),
        None,
        ColorSpace::Rec709,
        context,
        runtime,
        |_, _, _| {},
    )
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan, explicit demux worker and a 4K 30 fps tagged SDR planar fixture"]
fn compact_uhd_frames_reuse_the_standard_working_set() -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let path =
        std::env::var_os("MONDRIAN_UHD_PLANAR_FIXTURE").context("explicit UHD planar fixture")?;
    let context = pollster::block_on(GpuContext::new())?;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime.reconfigure_resource_grant(
        ViewerGpuExecutionResourceGrant::new(3, 256 * 1024 * 1024)
            .with_active_limits(2 * 1024 * 1024 * 1024, 96)
    ));
    let mut observations = Vec::new();
    let mut extents = Vec::new();
    let result = measure_with_resource_observer(
        Path::new(&path),
        false,
        None,
        None,
        ColorSpace::Rec709,
        &context,
        &mut runtime,
        |width, height, diagnostics| {
            extents.push((width, height));
            observations.push(diagnostics);
        },
    );
    let closure = retirement::retire_runtime(&context.device, runtime);
    result?;
    closure?;
    anyhow::ensure!(
        extents.iter().all(|extent| *extent == (3840, 2160)),
        "fixture must be UHD"
    );
    let first = observations.first().context("missing warmup allocation evidence")?;
    anyhow::ensure!(observations.len() == 6, "missing steady frame observations");
    anyhow::ensure!(
        observations.iter().skip(1).all(|sample| sample.misses == first.misses),
        "UHD working-set allocations continued after warmup: {observations:?}"
    );
    Ok(())
}

fn measure_with_resource_observer(
    path: &Path,
    native: bool,
    expected_native_surface: Option<(DecodedVideoSurfaceFormat, PreviewDecodePath)>,
    expected_cpu_surface: Option<DecodedVideoSurfaceFormat>,
    source_color: ColorSpace,
    context: &GpuContext,
    runtime: &mut ViewerGpuExecutionRuntime,
    mut observe: impl FnMut(u32, u32, GpuColorFrameWgpuResourcePoolDiagnostics),
) -> Result<Vec<Vec<f32>>> {
    let expected_decode_path = expected_native_surface.map(|(_, path)| path);
    let expected_native_surface = expected_native_surface.map(|(surface, _)| surface);
    let (bootstrap, observer) = PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(
        std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH")
            .context("explicit packaged demux worker")?
            .into(),
    );
    let mut decoder = bootstrap.build();
    let fingerprint = MediaFileFingerprint::capture(path);
    let selector = runtime.native_import_support().hardware_decode_device_selector;
    let graph = mondrian_effects::compile_reference_render_graph(
        mondrian_effects::EffectGraphBuilderState::new().finish(),
    )
    .context("effect graph")?;
    let effect = Arc::new(mondrian_effects::lower_effect_graph_to_gpu_plan(&graph)?);
    let input = RenderInputTransform::to_working_gpu(
        WorkingColorSpace::LinearRec2020,
        true,
        ColorEngine::mondrian_standard(),
    );
    let boundary =
        RenderOutputColorBoundary::display(source_color, false, ColorEngine::mondrian_standard());
    let monitor =
        RenderMonitorAdaptation::new(source_color, source_color, ColorEngine::mondrian_standard())?;
    let sequence = SequenceId::new();
    let mut samples = Vec::new();
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    runtime.install_cpu_yuv_upload_waker(move || {
        let _ = wake_tx.send(());
    });
    for index in [0, 17, 1, 29, 30, 5] {
        runtime.clear_frame_resources();
        let start = Instant::now();
        let deadline = start + Duration::from_secs(10);
        let mut request = PreviewDecodeRequest::new(
            path,
            SourceSampleTarget::covering(TimelineTime::new(index, 30)?),
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewSourceColorContract::automatic(source_color, DecodedVideoRange::Limited),
        )
        .with_fingerprint(fingerprint)
        .with_field_processing(PreviewSourceFieldProcessing::Progressive)
        .with_hardware_decode_request(if native {
            PreviewHardwareDecodeRequest::RequireGpuResident
        } else {
            PreviewHardwareDecodeRequest::Auto
        });
        request.representation = if native {
            PreviewDecodeRepresentation::NativeSurface
        } else {
            PreviewDecodeRepresentation::CompactCpuYuv
        };
        if native {
            if cfg!(target_os = "linux") {
                request.hardware_decode_device_selector =
                    Some(selector.context("exact CUDA device")?);
            } else {
                request.hardware_decode_device_selector = selector;
            }
        }
        let outcome = decoder.decode_cancellable(request, move || Instant::now() >= deadline)?;
        let (width, height, native_source, cpu_yuv_source) = match outcome {
            PreviewDecodeOutcome::NativeGpuFrame(f) if native => {
                assert!(f.diagnostics.native_decoder_active);
                assert!(!f.diagnostics.hardware_decode_cpu_transfer_observed);
                assert!(!f.diagnostics.temporal_approximation);
                assert_eq!(f.diagnostics.zero_copy_active, cfg!(target_os = "macos"));
                eprintln!(
                    "native sampling evidence: surface={:?}, path={:?}, observed_hardware={:?}",
                    f.surface_format, f.diagnostics.path, f.diagnostics.hardware_decode_observed
                );
                if let Some(expected) = expected_native_surface {
                    assert_eq!(f.handle_kind(), DecodedGpuFrameHandleKind::CVPixelBuffer);
                    assert_eq!(f.surface_format, expected);
                    assert_eq!(
                        Some(f.diagnostics.decoded_video_sampling.bit_depth),
                        expected.fixed_bit_depth()
                    );
                    assert_eq!(Some(f.diagnostics.path), expected_decode_path);
                    if expected_decode_path == Some(PreviewDecodePath::InProcessVideoToolboxNative)
                    {
                        assert_eq!(f.diagnostics.hardware_decode_observed, Some(true));
                    }
                }
                (
                    f.width,
                    f.height,
                    Some(ViewerGpuNativeSource {
                        source_color_space: source_color,
                        input_transform: input.clone(),
                        materialization_width: f.width,
                        materialization_height: f.height,
                        native_frame: Arc::new(f),
                    }),
                    None,
                )
            }
            PreviewDecodeOutcome::CpuYuvFrame(f) if !native => {
                assert!(
                    !f.diagnostics.native_decoder_active && !f.diagnostics.temporal_approximation
                );
                if let Some(expected) = expected_cpu_surface {
                    assert_eq!(f.surface_format(), expected);
                    assert_eq!(f.sample_format, CpuYuvSampleFormat::Unorm16);
                    assert_eq!(f.video_sampling.bit_depth, 16);
                    assert_eq!(f.diagnostics.decoded_video_sampling.bit_depth, 16);
                    let luma = f.luma_plane();
                    assert!(luma.data()[..f.width as usize * 2]
                        .chunks_exact(2)
                        .any(|bytes| bytes[0] != 0));
                    match f.chroma_planes() {
                        CpuYuvChromaPlanes::Planar { cb, cr } => {
                            assert!(cb.data()[0] != 0 && cr.data()[0] != 0);
                        }
                        CpuYuvChromaPlanes::Interleaved(_) => {
                            bail!("planar source became interleaved")
                        }
                    }
                }
                (
                    f.width,
                    f.height,
                    None,
                    Some(ViewerGpuCpuYuvSource {
                        materialization_width: f.width,
                        materialization_height: f.height,
                        frame: Arc::new(f),
                        input_transform: input.clone(),
                    }),
                )
            }
            other => bail!("unexpected decode {other:?}"),
        };
        let layers = [ViewerGpuExecutionLayer::Source(Box::new(
            ViewerGpuSourceLayer::Media {
                frame: None,
                is_data_texture: false,
                gpu_source: None,
                native_source,
                cpu_yuv_source,
                heterogeneous_input: None,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Arc::clone(&effect),
                frame_seed: index,
            },
        ))];
        runtime.prepare_native_video_imports(&layers)?;
        while !runtime.prewarm_cpu_yuv_uploads(&layers)? {
            context.device.poll(wgpu::PollType::Poll)?;
            if Instant::now() >= deadline {
                bail!("CPU upload timed out");
            }
            let _ = wake_rx.recv_timeout(Duration::from_millis(1));
        }
        let mut encoder = context.device.create_command_encoder(&Default::default());
        let mut record = runtime.record(
            &context.device,
            &context.queue,
            &mut encoder,
            ViewerGpuExecutionRequest {
                sequence_id: sequence,
                timeline_frame: index,
                width,
                height,
                working_color_space: WorkingColorSpace::LinearRec2020,
                layers: &layers,
                heterogeneous_inputs: Vec::new(),
                program_output_boundary: &boundary,
                monitor_adaptation: &monitor,
                source_rect: ViewerSourceRect::FULL,
                output_width: width,
                output_height: height,
                output_precision: ViewerGpuOutputPrecision::EncodedFloat16,
                display_calibration: None,
                program_scopes: None,
                signal_monitoring: None,
            },
        )?;
        if !record.fallback_reasons.is_empty() {
            bail!("unexpected fallback {:?}", record.fallback_reasons);
        }
        if expected_cpu_surface.is_some() {
            anyhow::ensure!(record.residency.cpu_upload_layers == 0);
            anyhow::ensure!(record.residency.gpu_input_layers == 1);
            anyhow::ensure!(record.residency.native_decoder_gpu_layers == 0);
            anyhow::ensure!(record.residency.gpu_input_failures == 0);
            anyhow::ensure!(record.stage_diagnostics.upload_stages == 1);
            anyhow::ensure!(record.stage_diagnostics.readback_stages == 0);
            anyhow::ensure!(record.residency.native_video_import.is_some_and(|facts| {
                facts.decoder_residency == mondrian_media::DecodedFrameResidency::CpuYuv
                    && facts.decoder_handle_kind.is_none()
            }));
        }
        if native {
            anyhow::ensure!(record.residency.native_decoder_gpu_layers == 1);
            anyhow::ensure!(record.residency.cpu_upload_layers == 0);
            anyhow::ensure!(record.residency.gpu_input_failures == 0);
            anyhow::ensure!(record.stage_diagnostics.upload_stages == 0);
            anyhow::ensure!(record.stage_diagnostics.readback_stages == 0);
        }
        if native
            && expected_native_surface == Some(DecodedVideoSurfaceFormat::Yuva444sp16)
            && index == 5
        {
            // Release the caller and decoder owners before the queue barrier.
            // The production import completion retain must cover final GPU use.
            drop(layers);
            decoder.clear();
            eprintln!(
                "ProRes native sources retained after decoder clear: {}",
                runtime.native_import_retained_source_count()
            );
        }
        let output = runtime.take_presentation_output(&mut record)?;
        let plan = GpuColorFrameReadbackPlan::encoded_rgba16float(output.handle().clone())
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let buffer = GpuColorFrameReadback::record_presentation_copy(
            &context.device,
            &mut encoder,
            &plan,
            &output,
        )
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let submitted = context.queue.submit([encoder.finish()]);
        assert!(record.assert_adapter_submission(submitted.clone()).is_empty());
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        context.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submitted),
            timeout: Some(Duration::from_secs(5)),
        })?;
        rx.recv_timeout(Duration::from_secs(5))??;
        let mapped = buffer.slice(..).get_mapped_range()?;
        let pixels = plan
            .unpack_mapped_rgba16float(&mapped)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        anyhow::ensure!(pixels.iter().all(|v| v.is_finite()));
        if expected_cpu_surface.is_some() {
            let (minimum, maximum) = pixels.chunks_exact(4).map(|pixel| pixel[0]).fold(
                (f32::INFINITY, f32::NEG_INFINITY),
                |(minimum, maximum), red| (minimum.min(red), maximum.max(red)),
            );
            anyhow::ensure!(
                maximum - minimum > 1.0 / 128.0,
                "sixteen-bit source collapsed to a constant Viewer output"
            );
        }
        if expected_native_surface != Some(DecodedVideoSurfaceFormat::Yuva444sp16) {
            anyhow::ensure!(pixels.chunks_exact(4).all(|p| (p[3] - 1.0).abs() < 1e-5));
        }
        drop(mapped);
        buffer.unmap();
        drop(output);
        if native
            && expected_native_surface == Some(DecodedVideoSurfaceFormat::Yuva444sp16)
            && index == 5
        {
            anyhow::ensure!(runtime.native_import_retained_source_count() == 0);
            anyhow::ensure!(decoder.native_outputs_released());
        }
        observe(width, height, runtime.resource_pool_diagnostics());
        samples.push(pixels);
    }
    runtime.clear_frame_resources();
    decoder.clear();
    assert!(decoder.native_outputs_released());
    let demux = observer.snapshot().isolated_demux;
    assert!(demux.completed_reads > 0 && demux.clean_closes > 0 && demux.active_sessions == 0);
    Ok(samples)
}
