//! Real decoder agreement through the production Viewer and isolated demux.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::{bail, Context, Result};
use mondrian_core::{
    BlendMode, ColorEngine, ColorSpace, SequenceId, SourceSampleTarget, TimelineTime,
    WorkingColorSpace,
};
use mondrian_media::preview::*;
use mondrian_media::{
    DecodedGpuFrameHandleKind, DecodedVideoRange, DecodedVideoSurfaceFormat, MediaFileFingerprint,
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
    )
}

#[cfg(target_os = "macos")]
fn compare_macos_videotoolbox_fixtures(
    fixture_env: &str,
    cases: [(DecodedVideoSurfaceFormat, ColorSpace); 3],
    expected_alpha: f32,
) -> Result<()> {
    mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let fixtures = std::env::var_os(fixture_env).with_context(|| fixture_env.to_owned())?;
    let paths: Vec<_> = std::env::split_paths(&fixtures).collect();
    anyhow::ensure!(paths.len() == cases.len(), "expected three tagged fixtures");
    let context = pollster::block_on(GpuContext::new())?;
    anyhow::ensure!(context.adapter.get_info().backend == wgpu::Backend::Metal);
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    anyhow::ensure!(runtime
        .reconfigure_resource_grant(ViewerGpuExecutionResourceGrant::professional_realtime()));
    let result = (|| -> Result<()> {
        for (path, (surface, color)) in paths.into_iter().zip(cases) {
            let cpu = measure_with_resource_observer(
                &path,
                false,
                Some(surface),
                color,
                &context,
                &mut runtime,
                |_, _, _| {},
            )?;
            let native = measure_with_resource_observer(
                &path,
                true,
                Some(surface),
                color,
                &context,
                &mut runtime,
                |_, _, _| {},
            )?;
            for output in cpu.iter().chain(&native) {
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
                let max =
                    cpu.iter().zip(native).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
                eprintln!(
                    "{} {color:?} seek sample {index}: maximum float output difference {max}",
                    path.display()
                );
                anyhow::ensure!(
                    max <= 1.0 / 1024.0,
                    "CPU/VideoToolbox Viewer pixel disagreement: {max}"
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
        expected_native_surface,
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
    expected_native_surface: Option<DecodedVideoSurfaceFormat>,
    source_color: ColorSpace,
    context: &GpuContext,
    runtime: &mut ViewerGpuExecutionRuntime,
    mut observe: impl FnMut(u32, u32, GpuColorFrameWgpuResourcePoolDiagnostics),
) -> Result<Vec<Vec<f32>>> {
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
                if let Some(expected) = expected_native_surface {
                    assert_eq!(f.handle_kind(), DecodedGpuFrameHandleKind::CVPixelBuffer);
                    assert_eq!(f.surface_format, expected);
                    assert_eq!(
                        Some(f.diagnostics.decoded_video_sampling.bit_depth),
                        expected.fixed_bit_depth()
                    );
                    if expected == DecodedVideoSurfaceFormat::Yuva444sp16 {
                        assert_eq!(
                            f.diagnostics.path,
                            PreviewDecodePath::InProcessVideoToolboxNative
                        );
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
