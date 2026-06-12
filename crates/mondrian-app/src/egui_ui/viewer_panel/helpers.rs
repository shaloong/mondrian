//! Decode pipeline, perf metrics, cache, and config helpers.
use super::*;

pub(crate) fn prefetch_budget(active_layer_count: usize, is_playing: bool) -> PrefetchBudget {
    if !is_playing {
        return if active_layer_count >= 8 {
            PrefetchBudget {
                frames_ahead: 8,
                max_in_flight: 8,
                max_spawn_per_tick: 3,
            }
        } else {
            PrefetchBudget {
                frames_ahead: 12,
                max_in_flight: 12,
                max_spawn_per_tick: 4,
            }
        };
    }

    if active_layer_count >= 10 {
        PrefetchBudget {
            frames_ahead: 2,
            max_in_flight: 6,
            max_spawn_per_tick: 3,
        }
    } else if active_layer_count >= 6 {
        PrefetchBudget {
            frames_ahead: 3,
            max_in_flight: 8,
            max_spawn_per_tick: 4,
        }
    } else {
        PrefetchBudget {
            frames_ahead: 4,
            max_in_flight: 10,
            max_spawn_per_tick: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CacheFileEntry {
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) modified: std::time::SystemTime,
}

pub(crate) fn list_cache_files(root: &Path) -> anyhow::Result<Vec<CacheFileEntry>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        let read_dir = std::fs::read_dir(&dir)?;
        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if metadata.is_dir() {
                stack.push(path);
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            files.push(CacheFileEntry {
                path,
                size: metadata.len(),
                modified: metadata.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            });
        }
    }

    Ok(files)
}

pub(crate) fn prefetch_offsets(
    frames_ahead: i64,
    direction: i64,
    is_playing: bool,
    chunk_len: i64,
) -> Vec<i64> {
    let chunk_len = chunk_len.max(1);

    if is_playing {
        let mut offsets = Vec::new();
        let mut offset = 1;
        while offset <= frames_ahead {
            offsets.push(offset * direction);
            offset += chunk_len;
        }
        return offsets;
    }

    let mut offsets = Vec::new();
    let mut offset = 1;
    while offset <= frames_ahead {
        offsets.push(offset);
        offsets.push(-offset);
        offset += chunk_len;
    }
    offsets
}

pub(crate) fn composite_signature_hash(signature: &CompositeFrameSignature) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    signature.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn render_element_signature(layer: &RenderElement) -> LayerSignature {
    match layer {
        RenderElement::Media(layer) => LayerSignature::Media {
            asset_id: layer.frame_key.0,
            source_frame: layer.frame_key.1,
            source_time_base: layer.source_time_base,
            opacity_u8: (layer.opacity * 255.0).round() as u8,
            blend_mode: layer.blend_mode,
            transform_key: quantize_transform_signature(layer.transform),
            frame_seed: layer.frame_seed,
            effect_hash: layer.effect_graph.signature_hash,
            input_color_space: layer.input_color_space,
            working_color_space: layer.working_color_space,
            tone_map: layer.tone_map,
        },
        RenderElement::Adjustment(layer) => LayerSignature::Adjustment {
            opacity_u8: (layer.opacity * 255.0).round() as u8,
            blend_mode: layer.blend_mode,
            frame_seed: layer.frame_seed,
            effect_hash: layer.effect_graph.signature_hash,
        },
        RenderElement::SolidColor(layer) => LayerSignature::SolidColor {
            color_bits: [
                layer.color.r.to_bits(),
                layer.color.g.to_bits(),
                layer.color.b.to_bits(),
                layer.color.a.to_bits(),
            ],
            opacity_u8: (layer.opacity * 255.0).round() as u8,
            blend_mode: layer.blend_mode,
            transform_key: quantize_transform_signature(layer.transform),
            frame_seed: layer.frame_seed,
            effect_hash: layer.effect_graph.signature_hash,
        },
        RenderElement::NestedSequence(layer) => {
            let child_hash = {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                for child in &layer.layers {
                    render_element_signature(child).hash(&mut hasher);
                }
                hasher.finish()
            };
            LayerSignature::NestedSequence {
                sequence_id: layer.sequence_id,
                source_frame: layer.source_frame,
                opacity_u8: (layer.opacity * 255.0).round() as u8,
                blend_mode: layer.blend_mode,
                transform_key: quantize_transform_signature(layer.transform),
                frame_seed: layer.frame_seed,
                effect_hash: layer.effect_graph.signature_hash,
                child_hash,
                nested_processing: layer.nested_processing,
                engine: layer.engine.clone(),
            }
        }
    }
}

pub(crate) fn decode_composited_rgba(
    request: &DecodeRequest,
) -> anyhow::Result<(RgbaFrame, Option<CompositedFrame>)> {
    let decode_started_at = Instant::now();
    if request.generation != request.latest_generation.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!("decode cancelled by newer generation"));
    }

    // Ensure the color engine is ready.
    if let Err(e) = request.engine.ensure_loaded() {
        tracing::warn!("色彩引擎加载失败: {}", e);
    }

    let width = request.target_width.max(1);
    let height = request.target_height.max(1);

    let mut decoded_layers = 0usize;
    let mut last_error: Option<anyhow::Error> = None;

    if request.layers.is_empty() {
        return Ok((
            RgbaFrame {
                width,
                height,
                data: vec![0u8; width as usize * height as usize * 4],
            },
            None,
        ));
    }

    let playback_mode = request.playback_mode;
    let layer_cache_enabled = request.layer_cache_enabled;
    let decode_generation = request.generation;
    let latest_generation = Arc::clone(&request.latest_generation);
    let layer_cache = Arc::clone(&request.layer_cache);
    let decoder_pool = Arc::clone(&request.decoder_pool);

    // Decode at native resolution — the transform maps canvas→media-native
    // coordinates, so the decoded frame must be at the source's native size.
    let decode_w = u32::MAX;
    let decode_h = u32::MAX;
    let layer_outputs = preview_decode_pool().install(|| {
        request
            .layers
            .par_iter()
            .cloned()
            .enumerate()
            .filter_map(|(index, layer)| {
                let RenderElement::Media(layer) = layer else {
                    return None;
                };
                if decode_generation != latest_generation.load(Ordering::Relaxed) {
                    return Some((
                        index,
                        Err("decode cancelled by newer generation".to_string()),
                    ));
                }

                let decoded = decode_layer_rgba(
                    &layer,
                    decode_w,
                    decode_h,
                    playback_mode,
                    layer_cache_enabled,
                    &layer_cache,
                    &decoder_pool,
                )
                .map_err(|e| e.to_string());
                Some((index, decoded))
            })
            .collect::<Vec<_>>()
    });

    let nested_outputs = request
        .layers
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(index, layer)| {
            let RenderElement::NestedSequence(layer) = layer else {
                return None;
            };
            if decode_generation != latest_generation.load(Ordering::Relaxed) {
                return Some((
                    index,
                    Err("decode cancelled by newer generation".to_string()),
                ));
            }
            let parent_context = ColorContext {
                working_color_space: request.working_color_space,
                output_color_space: request.output_color_space,
                tone_map: request.tone_map,
                workflow: ColorWorkflow::DisplayReferred,
                nested_processing: layer.nested_processing,
                engine: request.engine.clone(),
                missing_metadata_policy: MissingColorMetadataPolicy::default(),
                ocio_display: None,
                ocio_view: None,
            };
            let nested_context = match layer.nested_processing {
                NestedColorProcessing::PreserveChildWorkingSpace => ColorContext {
                    working_color_space: layer.working_color_space,
                    output_color_space: parent_context.working_color_space,
                    tone_map: layer.tone_map,
                    workflow: parent_context.workflow,
                    nested_processing: layer.nested_processing,
                    engine: layer.engine.clone(),
                    missing_metadata_policy: parent_context.missing_metadata_policy,
                    ocio_display: parent_context.ocio_display.clone(),
                    ocio_view: parent_context.ocio_view.clone(),
                },
                NestedColorProcessing::ForceParentWorkingSpace => ColorContext {
                    working_color_space: parent_context.working_color_space,
                    output_color_space: parent_context.working_color_space,
                    tone_map: parent_context.tone_map,
                    workflow: parent_context.workflow,
                    nested_processing: layer.nested_processing,
                    engine: parent_context.engine.clone(),
                    missing_metadata_policy: parent_context.missing_metadata_policy,
                    ocio_display: parent_context.ocio_display.clone(),
                    ocio_view: parent_context.ocio_view.clone(),
                },
                NestedColorProcessing::BakeChildOutputTransform => ColorContext {
                    working_color_space: layer.working_color_space,
                    output_color_space: parent_context.working_color_space,
                    tone_map: layer.tone_map || parent_context.tone_map,
                    workflow: parent_context.workflow,
                    nested_processing: layer.nested_processing,
                    engine: layer.engine.clone(),
                    missing_metadata_policy: parent_context.missing_metadata_policy,
                    ocio_display: parent_context.ocio_display.clone(),
                    ocio_view: parent_context.ocio_view.clone(),
                },
            };
            let signature = CompositeFrameSignature {
                width: layer.width,
                height: layer.height,
                working_color_space: nested_context.working_color_space,
                output_color_space: nested_context.output_color_space,
                display_profile_key: 0,
                tone_map: nested_context.tone_map,
                layers: layer.layers.iter().map(render_element_signature).collect(),
            };
            let nested_request = DecodeRequest {
                signature,
                layers: layer.layers.clone(),
                working_color_space: nested_context.working_color_space,
                output_color_space: nested_context.output_color_space,
                engine: nested_context.engine.clone(),
                display_profile: DisplayColorProfile::rec709_reference(),
                ocio_display: nested_context.ocio_display.clone(),
                ocio_view: nested_context.ocio_view.clone(),
                tone_map: nested_context.tone_map,
                playback_mode,
                target_width: layer.width,
                target_height: layer.height,
                seq_width: layer.width,
                seq_height: layer.height,
                layer_cache_enabled,
                layer_cache: Arc::clone(&layer_cache),
                decoder_pool: Arc::clone(&decoder_pool),
                generation: decode_generation,
                latest_generation: Arc::clone(&latest_generation),
            };
            Some((
                index,
                decode_composited_rgba(&nested_request)
                    .map(|(rgba, _gpu)| rgba)
                    .map_err(|e| e.to_string()),
            ))
        })
        .collect::<Vec<_>>();

    let mut layer_results: Vec<Option<Result<RgbaFrame, String>>> =
        vec![None; request.layers.len()];
    let mut decoded_media_frames =
        std::iter::repeat_with(|| None).take(request.layers.len()).collect::<Vec<_>>();
    let mut decoded_nested_frames =
        std::iter::repeat_with(|| None).take(request.layers.len()).collect::<Vec<_>>();
    for (index, decoded) in layer_outputs {
        if index < layer_results.len() {
            layer_results[index] = Some(decoded);
        }
    }

    for (index, decoded) in nested_outputs {
        let Some(RenderElement::NestedSequence(layer)) = request.layers.get(index) else {
            continue;
        };
        match decoded {
            Ok(frame) => {
                decoded_nested_frames[index] = Some((layer.clone(), frame));
                decoded_layers += 1;
            }
            Err(err) => {
                last_error = Some(anyhow::anyhow!(
                    "nested sequence {}@{} 解码失败: {}",
                    layer.sequence_id,
                    layer.source_frame,
                    err
                ));
            }
        }
    }

    for (index, layer) in request.layers.iter().enumerate() {
        let RenderElement::Media(layer) = layer else {
            continue;
        };
        match layer_results.get_mut(index).and_then(Option::take) {
            Some(Ok(frame)) => {
                decoded_media_frames[index] = Some((layer.clone(), frame));
                decoded_layers += 1;
            }
            Some(Err(err)) => {
                last_error = Some(anyhow::anyhow!(
                    "{}@{} 解码失败: {}",
                    layer.frame_key.0,
                    layer.frame_key.1,
                    err
                ));
            }
            None => {
                last_error = Some(anyhow::anyhow!(
                    "{}@{} 解码失败: worker 未返回结果",
                    layer.frame_key.0,
                    layer.frame_key.1
                ));
            }
        }
    }

    if decoded_layers == 0 {
        if let Some(err) = last_error {
            tracing::debug!("预览合成回退到透明帧：{}", err);
        }
        return Ok((
            RgbaFrame {
                width,
                height,
                data: vec![0u8; width as usize * height as usize * 4],
            },
            None,
        ));
    }

    let has_cpu_only_ops = request.layers.iter().any(|layer| match layer {
        RenderElement::Media(layer) => {
            !layer.effect_graph.graph.is_identity()
                || !is_identity_transform(layer.transform)
                || layer.blend_mode != BlendMode::Normal
        }
        RenderElement::Adjustment(_) => true,
        RenderElement::SolidColor(_) => true,
        RenderElement::NestedSequence(_) => true,
    });

    if !has_cpu_only_ops && decoded_layers == 1 {
        let only_layer =
            decoded_media_frames.iter().flatten().next().expect("single layer should exist");
        let (_, frame) = only_layer;
        if frame.width == width && frame.height == height {
            record_preview_perf_passthrough_frame();
            record_preview_perf_decode_total(decode_started_at.elapsed());
            let mut data = frame.data.clone();
            apply_preview_output_color(&mut data, request);
            return Ok((RgbaFrame { width, height, data }, None));
        }
    }

    if !has_cpu_only_ops {
        let rgba_layers_for_gpu: Vec<CpuRgbaLayer> = decoded_media_frames
            .iter()
            .flatten()
            .map(|(layer, frame)| CpuRgbaLayer {
                width: frame.width,
                height: frame.height,
                data: frame.data.clone(),
                opacity: layer.opacity,
            })
            .collect();

        // Zero-copy GPU path: composite to texture + GPU color conversion.
        // Check prerequisites before allocating GPU resources.
        if let (Some(device), Some(queue)) = (gpu_device(), gpu_queue()) {
            if let Some(params) = gpu_color_params(request) {
                if let Some(texture) =
                    try_gpu_composite_to_texture(width, height, &rgba_layers_for_gpu)
                {
                    record_preview_perf_decode_total(decode_started_at.elapsed());
                    let converted =
                        apply_gpu_color_conversion(&device, &queue, texture, width, height, params);
                    let gpu_frame = CompositedFrame::new(&device, converted, width, height);
                    return Ok((
                        RgbaFrame { width, height, data: Vec::new() },
                        Some(gpu_frame),
                    ));
                }
            }
        }

        // Standard GPU path with CPU readback for color conversion.
        if let Some(gpu_rgba) = try_gpu_composite_rgba_layers(width, height, &rgba_layers_for_gpu) {
            record_preview_perf_decode_total(decode_started_at.elapsed());
            let mut data = gpu_rgba;
            apply_preview_output_color(&mut data, request);
            return Ok((RgbaFrame { width, height, data }, None));
        }
    }

    let cpu_composite_started_at = Instant::now();
    // Convert transforms from sequence space to canvas space.
    // The compositor canvas may differ from the sequence resolution.
    let seq_to_canvas = (width as f32 / request.seq_width.max(1) as f32)
        .min(height as f32 / request.seq_height.max(1) as f32);
    let mut composite_elements = Vec::with_capacity(request.layers.len());
    for (index, layer) in request.layers.iter().enumerate() {
        match layer {
            RenderElement::Media(_) => {
                let Some((layer, frame)) = decoded_media_frames[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    rgba: &frame.data,
                    width: frame.width,
                    height: frame.height,
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: scale_affine(layer.transform, seq_to_canvas),
                    effect_graph: std::sync::Arc::clone(&layer.effect_graph),
                    frame_seed: layer.frame_seed,
                }));
            }
            RenderElement::Adjustment(adjustment) => {
                composite_elements.push(TimelineCompositeElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: std::sync::Arc::clone(&adjustment.effect_graph),
                        opacity: adjustment.opacity,
                        blend_mode: adjustment.blend_mode,
                        frame_seed: adjustment.frame_seed,
                    },
                ));
            }
            RenderElement::SolidColor(solid) => {
                composite_elements.push(TimelineCompositeElement::SolidColor(
                    TimelineSolidColorLayer {
                        color: solid.color,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform: scale_affine(solid.transform, seq_to_canvas),
                        effect_graph: std::sync::Arc::clone(&solid.effect_graph),
                        frame_seed: solid.frame_seed,
                    },
                ));
            }
            RenderElement::NestedSequence(_) => {
                let Some((layer, frame)) = decoded_nested_frames[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    rgba: &frame.data,
                    width: frame.width,
                    height: frame.height,
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: scale_affine(layer.transform, seq_to_canvas),
                    effect_graph: std::sync::Arc::clone(&layer.effect_graph),
                    frame_seed: layer.frame_seed,
                }));
            }
        }
    }

    let mut scratch = TimelineCompositeScratch::default();
    let mut canvas = composite_timeline_elements_float_linear(
        width,
        height,
        &composite_elements,
        TimelineCompositeOptions { empty_canvas_transparent: true },
        request.working_color_space,
        &mut scratch,
    );
    apply_preview_output_color(&mut canvas, request);

    record_preview_perf_composite_ns(cpu_composite_started_at.elapsed().as_nanos() as u64, false);
    record_preview_perf_decode_total(decode_started_at.elapsed());

    Ok((RgbaFrame { width, height, data: canvas }, None))
}

/// Build GPU color conversion parameters from the request's display profile.
/// Returns `None` when the conversion requires CPU-side processing
/// (OCIde display/view, ICC profile, HDR, or non-standard transfer functions).
pub(crate) fn gpu_color_params(
    request: &DecodeRequest,
) -> Option<crate::egui_ui::viewer::gpu_composite::GpuColorConversionParams> {
    use crate::egui_ui::viewer::gpu_composite::GpuColorConversionParams;
    // Only supported for OCIde-free, ICC-free workflows.
    if request.ocio_display.is_some() || request.ocio_view.is_some() {
        return None;
    }
    if request.display_profile.icc_bytes.is_some() {
        return None;
    }
    // HDR not supported on GPU path.
    if request.working_color_space.is_hdr() || request.output_color_space.is_hdr() {
        return None;
    }

    let decode_gamma = match request.working_color_space {
        ColorSpace::Srgb => 2.2,
        ColorSpace::Rec709 => 2.4,
        _ => return None, // unsupported transfer
    };
    let encode_gamma = match request.display_profile.color_space {
        ColorSpace::Srgb => 2.2,
        ColorSpace::Rec709 => 2.4,
        _ => return None,
    };

    Some(GpuColorConversionParams {
        decode_gamma,
        display_matrix: request.display_profile.linear_matrix,
        display_gamma: request.display_profile.gamma,
        encode_gamma,
    })
}

pub(crate) fn apply_preview_output_color(data: &mut [u8], request: &DecodeRequest) {
    convert_rgba8_in_place(
        data,
        ColorPipeline::new(
            request.working_color_space,
            request.working_color_space,
            request.output_color_space,
            request.tone_map,
        )
        .with_engine(request.engine.clone()),
    );

    // Use explicit display/view from viewer settings, or fall back to OCIO defaults.
    let ocio_defaults = mondrian_core::ocio_default_display_view();
    let display = request
        .ocio_display
        .as_deref()
        .or_else(|| ocio_defaults.as_ref().map(|(d, _)| d.as_str()));
    let view = request
        .ocio_view
        .as_deref()
        .or_else(|| ocio_defaults.as_ref().map(|(_, v)| v.as_str()));

    if let (Some(display), Some(view)) = (display, view) {
        if request
            .engine
            .display_transform(data, request.output_color_space, display, view)
            .is_ok()
        {
            return;
        }
    }

    if let Err(err) = apply_display_profile_rgba8_in_place(
        data,
        request.output_color_space,
        &request.display_profile,
        request.tone_map,
    ) {
        tracing::warn!("显示色彩配置无效，已跳过显示校准: {}", err);
    }
}

// GPU compositor functions extracted to crate::egui_ui::viewer::gpu_composite.

pub(crate) fn global_media_path_cache(cache_root: PathBuf) -> Arc<mondrian_media::MultiLevelCache> {
    static MEDIA_PATH_CACHE: OnceLock<
        Mutex<HashMap<PathBuf, Arc<mondrian_media::MultiLevelCache>>>,
    > = OnceLock::new();
    let cache_map = MEDIA_PATH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match cache_map.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    guard
        .entry(cache_root.clone())
        .or_insert_with(|| mondrian_media::MultiLevelCache::new(cache_root, 256))
        .clone()
}

pub(crate) fn decode_layer_rgba(
    layer: &LayerDecodeRequest,
    width: u32,
    height: u32,
    playback_mode: bool,
    layer_cache_enabled: bool,
    layer_cache: &SharedLayerFrameCache,
    decoder_pool: &Arc<DecoderPool>,
) -> anyhow::Result<RgbaFrame> {
    let started_at = Instant::now();
    let cache_key = LayerFrameCacheKey {
        asset_id: layer.frame_key.0,
        source_frame: layer.frame_key.1,
        source_time_base: layer.source_time_base,
        target_width: width,
        target_height: height,
        input_color_space: layer.input_color_space,
        working_color_space: layer.working_color_space,
        engine: layer.engine.clone(),
        tone_map: layer.tone_map,
    };

    if layer_cache_enabled {
        if let Some(frame) = layer_cache_get(layer_cache, &cache_key) {
            record_preview_perf_layer_decode(started_at.elapsed(), true);
            if preview_diag_enabled() {
                tracing::debug!(
                    "[preview-diag] layer cache hit asset={} frame={} size={}x{}",
                    layer.frame_key.0,
                    layer.frame_key.1,
                    width,
                    height
                );
            }
            return Ok(frame);
        }
    }

    if layer_cache_enabled && playback_mode {
        let tolerance = playback_layer_cache_tolerance_frames();
        if tolerance > 0 {
            if let Some(frame) = layer_cache_get_with_tolerance(layer_cache, &cache_key, tolerance)
            {
                layer_cache_put(layer_cache, cache_key.clone(), frame.clone());
                record_preview_perf_layer_decode(started_at.elapsed(), true);
                if preview_diag_enabled() {
                    tracing::debug!(
                        "[preview-diag] layer tolerance cache hit asset={} frame={} tol={} size={}x{}",
                        layer.frame_key.0,
                        layer.frame_key.1,
                        tolerance,
                        width,
                        height
                    );
                }
                return Ok(frame);
            }
        }
    }

    let async_result = with_layer_decode_runtime(|rt| {
        rt.block_on(decoder_pool.get_video_frame_rgba(
            layer.frame_key.0,
            layer.path.clone(),
            TimeCode::new(layer.frame_key.1, layer.source_time_base),
            width,
            height,
        ))
    });

    if let Some(Ok(frame)) = async_result {
        let mut frame = (*frame).clone();
        apply_layer_input_color(&mut frame.data, layer);
        if layer_cache_enabled {
            layer_cache_put(layer_cache, cache_key.clone(), frame.clone());
        }
        record_preview_perf_layer_decode(started_at.elapsed(), false);
        if preview_diag_enabled() {
            tracing::debug!(
                "[preview-diag] layer async decode ok asset={} frame={} elapsed={}ms",
                layer.frame_key.0,
                layer.frame_key.1,
                started_at.elapsed().as_millis() as u64
            );
        }
        return Ok(frame);
    }

    if let Some(Err(err)) = async_result {
        if preview_diag_enabled() {
            tracing::warn!(
                "[preview-diag] layer async decode failed asset={} frame={} err={}",
                layer.frame_key.0,
                layer.frame_key.1,
                err
            );
        }
    }

    if !preview_sync_fallback_enabled() {
        return Err(anyhow::anyhow!(
            "async layer decode failed and sync fallback disabled: asset={} frame={}",
            layer.frame_key.0,
            layer.frame_key.1
        ));
    }

    if preview_diag_enabled() {
        tracing::warn!(
            "[preview-diag] entering sync fallback decode asset={} frame={}",
            layer.frame_key.0,
            layer.frame_key.1
        );
    }

    Ok(mondrian_media::decode_video_frame_at_time_rgba_scaled(
        layer.path.as_path(),
        layer.source_secs,
        None,
        None,
    )
    .map(|mut frame| {
        apply_layer_input_color(&mut frame.data, layer);
        if layer_cache_enabled {
            layer_cache_put(layer_cache, cache_key, frame.clone());
        }
        record_preview_perf_layer_decode(started_at.elapsed(), false);
        frame
    })?)
}

pub(crate) fn apply_layer_input_color(data: &mut [u8], layer: &LayerDecodeRequest) {
    convert_rgba8_in_place(
        data,
        ColorPipeline::new(
            layer.input_color_space,
            layer.working_color_space,
            layer.working_color_space,
            layer.tone_map,
        )
        .with_engine(layer.engine.clone()),
    );
}

#[derive(Default)]
pub(crate) struct PreviewPerfStats {
    decode_frames: AtomicU64,
    committed_frames: AtomicU64,
    passthrough_frames: AtomicU64,
    layer_cache_hits: AtomicU64,
    layer_cache_misses: AtomicU64,
    decode_total_ns: AtomicU64,
    layer_decode_ns: AtomicU64,
    composite_cpu_ns: AtomicU64,
    composite_gpu_ns: AtomicU64,
    upload_ns: AtomicU64,
    last_report_ms: AtomicU64,
    last_avg_decode_x100: AtomicU64,
    last_avg_layer_x100: AtomicU64,
    last_avg_comp_x100: AtomicU64,
    last_avg_upload_x100: AtomicU64,
    last_layer_share_pct: AtomicU64,
    last_comp_share_pct: AtomicU64,
    last_upload_share_pct: AtomicU64,
    last_hit_rate_pct: AtomicU64,
    last_passthrough_rate_pct: AtomicU64,
    last_gpu_comp_on: AtomicBool,
    last_decode_frames: AtomicU64,
    last_commit_frames: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreviewPerfSnapshot {
    pub(crate) avg_decode_ms: f64,
    pub(crate) avg_layer_ms: f64,
    pub(crate) avg_comp_ms: f64,
    pub(crate) avg_upload_ms: f64,
    pub(crate) layer_share_pct: u64,
    pub(crate) comp_share_pct: u64,
    pub(crate) upload_share_pct: u64,
    pub(crate) hit_rate_pct: u64,
    pub(crate) passthrough_rate_pct: u64,
    pub(crate) gpu_comp_on: bool,
    pub(crate) decode_frames: u64,
    pub(crate) commit_frames: u64,
}

pub(crate) fn preview_perf_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_PERF")
            .map(|v| {
                let value = v.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

pub(crate) fn preview_perf_report_interval_ms() -> u64 {
    static INTERVAL_MS: OnceLock<u64> = OnceLock::new();
    *INTERVAL_MS.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_PERF_INTERVAL_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(1000)
    })
}

pub(crate) fn preview_perf_stats() -> &'static PreviewPerfStats {
    static STATS: OnceLock<PreviewPerfStats> = OnceLock::new();
    STATS.get_or_init(PreviewPerfStats::default)
}

pub(crate) fn preview_perf_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn preview_perf_snapshot() -> Option<PreviewPerfSnapshot> {
    let stats = preview_perf_stats();
    let decode_frames = stats.last_decode_frames.load(Ordering::Relaxed);
    let commit_frames = stats.last_commit_frames.load(Ordering::Relaxed);
    if decode_frames == 0 && commit_frames == 0 {
        return None;
    }

    Some(PreviewPerfSnapshot {
        avg_decode_ms: stats.last_avg_decode_x100.load(Ordering::Relaxed) as f64 / 100.0,
        avg_layer_ms: stats.last_avg_layer_x100.load(Ordering::Relaxed) as f64 / 100.0,
        avg_comp_ms: stats.last_avg_comp_x100.load(Ordering::Relaxed) as f64 / 100.0,
        avg_upload_ms: stats.last_avg_upload_x100.load(Ordering::Relaxed) as f64 / 100.0,
        layer_share_pct: stats.last_layer_share_pct.load(Ordering::Relaxed),
        comp_share_pct: stats.last_comp_share_pct.load(Ordering::Relaxed),
        upload_share_pct: stats.last_upload_share_pct.load(Ordering::Relaxed),
        hit_rate_pct: stats.last_hit_rate_pct.load(Ordering::Relaxed),
        passthrough_rate_pct: stats.last_passthrough_rate_pct.load(Ordering::Relaxed),
        gpu_comp_on: stats.last_gpu_comp_on.load(Ordering::Relaxed),
        decode_frames,
        commit_frames,
    })
}

pub(crate) fn record_preview_perf_passthrough_frame() {
    if !preview_perf_enabled() {
        return;
    }
    preview_perf_stats().passthrough_frames.fetch_add(1, Ordering::Relaxed);
    maybe_report_preview_perf();
}

pub(crate) fn record_preview_perf_layer_decode(elapsed: Duration, cache_hit: bool) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    stats.layer_decode_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    if cache_hit {
        stats.layer_cache_hits.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.layer_cache_misses.fetch_add(1, Ordering::Relaxed);
    }
    maybe_report_preview_perf();
}

pub(crate) fn record_preview_perf_composite_ns(elapsed_ns: u64, gpu: bool) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    if gpu {
        stats.composite_gpu_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
    } else {
        stats.composite_cpu_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
    }
    maybe_report_preview_perf();
}

pub(crate) fn record_preview_perf_decode_total(elapsed: Duration) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    stats.decode_frames.fetch_add(1, Ordering::Relaxed);
    stats.decode_total_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    maybe_report_preview_perf();
}

pub(crate) fn record_preview_perf_upload(elapsed: Duration) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    stats.committed_frames.fetch_add(1, Ordering::Relaxed);
    stats.upload_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    maybe_report_preview_perf();
}

pub(crate) fn maybe_report_preview_perf() {
    if !preview_perf_enabled() {
        return;
    }

    let stats = preview_perf_stats();
    let now_ms = preview_perf_now_ms();
    let interval = preview_perf_report_interval_ms();
    let last = stats.last_report_ms.load(Ordering::Relaxed);

    if now_ms < last.saturating_add(interval) {
        return;
    }

    if stats
        .last_report_ms
        .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    let decode_frames = stats.decode_frames.swap(0, Ordering::Relaxed);
    let committed_frames = stats.committed_frames.swap(0, Ordering::Relaxed);
    let passthrough_frames = stats.passthrough_frames.swap(0, Ordering::Relaxed);
    let cache_hits = stats.layer_cache_hits.swap(0, Ordering::Relaxed);
    let cache_misses = stats.layer_cache_misses.swap(0, Ordering::Relaxed);
    let decode_total_ns = stats.decode_total_ns.swap(0, Ordering::Relaxed);
    let layer_decode_ns = stats.layer_decode_ns.swap(0, Ordering::Relaxed);
    let composite_cpu_ns = stats.composite_cpu_ns.swap(0, Ordering::Relaxed);
    let composite_gpu_ns = stats.composite_gpu_ns.swap(0, Ordering::Relaxed);
    let upload_ns = stats.upload_ns.swap(0, Ordering::Relaxed);

    if decode_frames == 0 && committed_frames == 0 {
        return;
    }

    let avg_decode_ms = if decode_frames > 0 {
        decode_total_ns as f64 / decode_frames as f64 / 1_000_000.0
    } else {
        0.0
    };

    let layer_calls = cache_hits.saturating_add(cache_misses);
    let avg_layer_ms = if layer_calls > 0 {
        layer_decode_ns as f64 / layer_calls as f64 / 1_000_000.0
    } else {
        0.0
    };

    let composite_total_ns = composite_cpu_ns.saturating_add(composite_gpu_ns);
    let avg_composite_ms = if decode_frames > 0 {
        composite_total_ns as f64 / decode_frames as f64 / 1_000_000.0
    } else {
        0.0
    };

    let avg_upload_ms = if committed_frames > 0 {
        upload_ns as f64 / committed_frames as f64 / 1_000_000.0
    } else {
        0.0
    };

    let denominator_ns = layer_decode_ns
        .saturating_add(composite_total_ns)
        .saturating_add(upload_ns)
        .max(1);
    let layer_pct = layer_decode_ns as f64 * 100.0 / denominator_ns as f64;
    let composite_pct = composite_total_ns as f64 * 100.0 / denominator_ns as f64;
    let upload_pct = upload_ns as f64 * 100.0 / denominator_ns as f64;

    let hit_rate = if layer_calls > 0 {
        cache_hits as f64 * 100.0 / layer_calls as f64
    } else {
        0.0
    };
    let passthrough_rate = if decode_frames > 0 {
        passthrough_frames as f64 * 100.0 / decode_frames as f64
    } else {
        0.0
    };

    stats.last_avg_decode_x100.store(
        (avg_decode_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_avg_layer_x100.store(
        (avg_layer_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_avg_comp_x100.store(
        (avg_composite_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_avg_upload_x100.store(
        (avg_upload_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_layer_share_pct.store(
        layer_pct.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_comp_share_pct.store(
        composite_pct.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_upload_share_pct.store(
        upload_pct.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats
        .last_hit_rate_pct
        .store(hit_rate.round().clamp(0.0, 100.0) as u64, Ordering::Relaxed);
    stats.last_passthrough_rate_pct.store(
        passthrough_rate.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_gpu_comp_on.store(composite_gpu_ns > 0, Ordering::Relaxed);
    stats.last_decode_frames.store(decode_frames, Ordering::Relaxed);
    stats.last_commit_frames.store(committed_frames, Ordering::Relaxed);

    tracing::info!(
        "[preview-perf] frames(dec/commit)={}/{} avg_ms(dec/layer/comp/upload)={:.2}/{:.2}/{:.2}/{:.2} share(layer/comp/upload)={:.0}%/{:.0}%/{:.0}% layer_hit={:.0}% pass={:.0}% gpu_comp={}",
        decode_frames,
        committed_frames,
        avg_decode_ms,
        avg_layer_ms,
        avg_composite_ms,
        avg_upload_ms,
        layer_pct,
        composite_pct,
        upload_pct,
        hit_rate,
        passthrough_rate,
        if composite_gpu_ns > 0 { "on" } else { "off" }
    );
}

pub(crate) fn with_layer_decode_runtime<T>(
    f: impl FnOnce(&tokio::runtime::Runtime) -> T,
) -> Option<T> {
    thread_local! {
        static LAYER_DECODE_RUNTIME: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
    }

    LAYER_DECODE_RUNTIME.with(|slot| {
        let mut runtime = slot.borrow_mut();
        if runtime.is_none() {
            *runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
        }
        runtime.as_ref().map(f)
    })
}

pub(crate) fn layer_cache_get(
    layer_cache: &SharedLayerFrameCache,
    key: &LayerFrameCacheKey,
) -> Option<RgbaFrame> {
    let guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return None,
    };
    guard.entries.get(key).cloned()
}

pub(crate) fn layer_cache_get_with_tolerance(
    layer_cache: &SharedLayerFrameCache,
    key: &LayerFrameCacheKey,
    tolerance_frames: i64,
) -> Option<RgbaFrame> {
    let guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return None,
    };

    let mut best_key: Option<LayerFrameCacheKey> = None;
    let mut best_distance = i64::MAX;

    for entry_key in guard.entries.keys() {
        if entry_key.asset_id != key.asset_id
            || entry_key.target_width != key.target_width
            || entry_key.target_height != key.target_height
        {
            continue;
        }

        let distance = (entry_key.source_frame - key.source_frame).abs();
        if distance <= tolerance_frames && distance < best_distance {
            best_distance = distance;
            best_key = Some(entry_key.clone());
            if distance == 0 {
                break;
            }
        }
    }

    best_key.and_then(|entry_key| guard.entries.get(&entry_key).cloned())
}

pub(crate) fn layer_cache_contains(
    layer_cache: &SharedLayerFrameCache,
    key: &LayerFrameCacheKey,
) -> bool {
    let guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    guard.entries.contains_key(key)
}

pub(crate) fn layer_cache_put(
    layer_cache: &SharedLayerFrameCache,
    key: LayerFrameCacheKey,
    frame: RgbaFrame,
) {
    // 扩大图层帧缓存至 256 帧：
    // 预取 + scrub 场景下 96 帧容量不足，大 seek 后旧缓存无法复用，
    // 增大容量可显著减少 seek 后的重复解码次数。
    const LAYER_CACHE_CAPACITY: usize = 256;

    let mut guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    if !guard.entries.contains_key(&key) {
        guard.order.push_front(key.clone());
    }

    guard.entries.insert(key, frame);
    while guard.entries.len() > LAYER_CACHE_CAPACITY {
        if let Some(oldest) = guard.order.pop_back() {
            guard.entries.remove(&oldest);
        } else {
            break;
        }
    }
}

pub(crate) fn layer_cache_clear(layer_cache: &SharedLayerFrameCache) {
    if let Ok(mut guard) = layer_cache.lock() {
        guard.entries.clear();
        guard.order.clear();
    }
}

pub(crate) fn scaled_dimension(raw: f32, factor: f32) -> u32 {
    (raw.max(1.0) * factor.max(0.05)).round().max(1.0) as u32
}

pub(crate) fn sequence_preview_target_size(
    resolution: mondrian_core::types::Resolution,
    available_width: f32,
    available_height: f32,
    scale_factor: f32,
) -> (u32, u32) {
    let max_width = scaled_dimension(available_width, scale_factor);
    let max_height = scaled_dimension(available_height, scale_factor);

    let width_scale = max_width as f64 / resolution.width.max(1) as f64;
    let height_scale = max_height as f64 / resolution.height.max(1) as f64;
    let scale = width_scale.min(height_scale).max(0.0001);

    let mut target_width = (resolution.width.max(1) as f64 * scale).round().max(1.0) as u32;
    let mut target_height = ((target_width as f64 / resolution.width.max(1) as f64)
        * resolution.height.max(1) as f64)
        .round()
        .max(1.0) as u32;

    if target_height > max_height {
        target_height = max_height.max(1);
        target_width = ((target_height as f64 / resolution.height.max(1) as f64)
            * resolution.width.max(1) as f64)
            .round()
            .max(1.0) as u32;
    }
    if target_width > max_width {
        target_width = max_width.max(1);
        target_height = ((target_width as f64 / resolution.width.max(1) as f64)
            * resolution.height.max(1) as f64)
            .round()
            .max(1.0) as u32;
    }

    (target_width.max(1), target_height.max(1))
}

pub(crate) fn playback_adjusted_target_size(size: (u32, u32), is_playing: bool) -> (u32, u32) {
    let (width, height) = size;
    let width_f = width.max(1) as f64;
    let height_f = height.max(1) as f64;
    let mut out_w = width.max(1);
    let mut out_h = height.max(1);

    if is_playing {
        let max_dim = playback_preview_max_dimension();
        if max_dim > 0 {
            let current_max = width_f.max(height_f);
            if current_max > max_dim as f64 {
                let scale = max_dim as f64 / current_max;
                out_w = (width_f * scale).round().max(1.0) as u32;
                out_h = (height_f * scale).round().max(1.0) as u32;
            }
        }
    }

    if out_w % 2 == 1 {
        out_w = out_w.saturating_sub(1).max(1);
    }
    if out_h % 2 == 1 {
        out_h = out_h.saturating_sub(1).max(1);
    }

    let step = if is_playing { 16 } else { 8 };
    let out_w = quantize_dimension(out_w.max(1), step);
    let out_h = quantize_dimension(out_h.max(1), step);

    (out_w, out_h)
}

pub(crate) fn preview_diag_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_DIAG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(cfg!(debug_assertions))
    })
}

pub(crate) fn preview_decode_worker_cap() -> usize {
    static WORKER_CAP: OnceLock<usize> = OnceLock::new();
    *WORKER_CAP.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_DECODE_WORKERS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.clamp(1, 16))
            .unwrap_or(6)
    })
}

pub(crate) fn preview_decode_pool() -> &'static rayon::ThreadPool {
    static DECODE_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    DECODE_POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(preview_decode_worker_cap())
            .thread_name(|idx| format!("preview-decode-{}", idx))
            .build()
            .expect("failed to build preview decode rayon pool")
    })
}

pub(crate) fn preview_diag_slow_threshold_ms() -> u64 {
    static THRESHOLD: OnceLock<u64> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_DIAG_SLOW_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 5)
            .unwrap_or(40)
    })
}

pub(crate) fn preview_sync_fallback_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_SYNC_FALLBACK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

pub(crate) fn playback_preview_max_dimension() -> u32 {
    static MAX_DIM: OnceLock<u32> = OnceLock::new();
    *MAX_DIM.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREVIEW_MAX_DIM")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value >= 320)
            .unwrap_or(1280)
    })
}

pub(crate) fn decode_stall_timeout_ms() -> u64 {
    static TIMEOUT_MS: OnceLock<u64> = OnceLock::new();
    *TIMEOUT_MS.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_STALL_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                let budget = decode_timeout_budget_ms();
                let grace = std::env::var("MONDRIAN_DECODE_STALL_GRACE_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(300);
                Some(budget.saturating_add(grace))
            })
            .filter(|value| *value >= 150)
            .unwrap_or(1200)
    })
}

pub(crate) fn decode_timeout_budget_ms() -> u64 {
    static BUDGET_MS: OnceLock<u64> = OnceLock::new();
    *BUDGET_MS.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_TIMEOUT_BUDGET_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                std::env::var("MONDRIAN_PREVIEW_DECODE_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .filter(|value| *value >= 100)
            .unwrap_or(2500)
    })
}

pub(crate) fn decode_request_coalescing_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_REQUEST_COALESCING")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(true)
    })
}

pub(crate) fn playback_prefill_duration_ms() -> u64 {
    static PREFILL_MS: OnceLock<u64> = OnceLock::new();
    *PREFILL_MS.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREFILL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(0, 3000))
            .unwrap_or(600)
    })
}

pub(crate) fn playback_prefetch_target_frames(fps: f64) -> i64 {
    static TARGET: OnceLock<i64> = OnceLock::new();
    let configured = *TARGET.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREFETCH_TARGET_FRAMES")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(8, 96))
            .unwrap_or(-1)
    });

    if configured > 0 {
        configured
    } else {
        (fps.round() as i64).max(25).clamp(12, 64)
    }
}

pub(crate) fn playback_prefetch_hysteresis_frames(fps: f64) -> i64 {
    static HYSTERESIS: OnceLock<i64> = OnceLock::new();
    let configured = *HYSTERESIS.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREFETCH_HYSTERESIS_FRAMES")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(1, 48))
            .unwrap_or(-1)
    });

    if configured > 0 {
        configured
    } else {
        (fps / 4.0).round() as i64
    }
    .max(6)
    .clamp(2, 32)
}

pub(crate) fn playback_layer_cache_tolerance_frames() -> i64 {
    static TOLERANCE: OnceLock<i64> = OnceLock::new();
    *TOLERANCE.get_or_init(|| {
        std::env::var("MONDRIAN_LAYER_CACHE_TOLERANCE_PLAYBACK")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(0, 3))
            .unwrap_or(0)
    })
}

pub(crate) fn playback_seek_reset_threshold_frames(fps: f64) -> i64 {
    static CONFIGURED: OnceLock<i64> = OnceLock::new();
    let configured = *CONFIGURED.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_SEEK_RESET_FRAMES")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(8, 240))
            .unwrap_or(-1)
    });

    if configured > 0 {
        configured
    } else {
        (fps * 1.2).round() as i64
    }
    .max(16)
    .clamp(12, 120)
}
