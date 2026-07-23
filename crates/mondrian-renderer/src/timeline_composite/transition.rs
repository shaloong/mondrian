//! Two-input visual-Transition lowering for the shared CPU compositor.

use super::*;

/// One already color-adapted input to a two-input visual Transition.
#[derive(Debug, Clone)]
pub enum TimelineTransitionInput<'a> {
    /// Disabled endpoint or explicit absence of coverage.
    Transparent,
    /// File-backed or nested media after source adaptation.
    Media(TimelineMediaLayer<'a>),
    /// Generated solid after Clip-local planning.
    SolidColor(TimelineSolidColorLayer),
}

/// Cross Dissolve occupying one position in the ordered Track stack.
#[derive(Debug, Clone)]
pub struct TimelineCrossDissolveLayer<'a> {
    /// Earlier edit endpoint.
    pub left: TimelineTransitionInput<'a>,
    /// Later edit endpoint.
    pub right: TimelineTransitionInput<'a>,
    /// Normalized interpolation coefficient in `0..=1`.
    pub progress: f32,
}

pub(super) fn composite_transition_input_f32(
    canvas: &mut [[f32; 4]],
    width: u32,
    height: u32,
    input: &TimelineTransitionInput<'_>,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> Result<(), EffectFloatExecutionError> {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            let frame = layer.frame.rgba_f32();
            let effect_output;
            let source = if layer.effect_graph.graph.is_identity() {
                frame.data.as_slice()
            } else {
                effect_output = apply_effect_graph_f32(
                    &frame.data,
                    frame.width,
                    frame.height,
                    &layer.effect_graph,
                    layer.frame_seed,
                    runtime,
                )?;
                effect_output.as_slice()
            };
            alpha_blend_f32_layer(
                canvas,
                width as usize,
                height as usize,
                source,
                frame.width as usize,
                frame.height as usize,
                layer.opacity,
                layer.blend_mode,
                layer.transform,
                layer.frame_seed,
            );
        }
        TimelineTransitionInput::SolidColor(layer) => {
            let pixel_count = width as usize * height as usize;
            let color = [layer.color.r, layer.color.g, layer.color.b, layer.color.a];
            if layer.effect_graph.graph.is_identity() && is_identity_transform(layer.transform) {
                alpha_blend_f32_solid(
                    canvas,
                    color,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                );
            } else {
                scratch.solid_fill_f32.resize(pixel_count, color);
                scratch.solid_fill_f32.fill(color);
                let source = if layer.effect_graph.graph.is_identity() {
                    scratch.solid_fill_f32.as_slice()
                } else {
                    scratch.solid_effect_f32 = apply_effect_graph_f32(
                        &scratch.solid_fill_f32,
                        width,
                        height,
                        &layer.effect_graph,
                        layer.frame_seed,
                        runtime,
                    )?;
                    scratch.solid_effect_f32.as_slice()
                };
                alpha_blend_f32_layer(
                    canvas,
                    width as usize,
                    height as usize,
                    source,
                    width as usize,
                    height as usize,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                    layer.frame_seed,
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn cross_dissolve_straight_rgba_f32(
    output: &mut [[f32; 4]],
    left: &[[f32; 4]],
    right: &[[f32; 4]],
    progress: f32,
) {
    let progress = progress.clamp(0.0, 1.0);
    let inverse = 1.0 - progress;
    for ((output, left), right) in output.iter_mut().zip(left).zip(right) {
        let alpha = left[3] * inverse + right[3] * progress;
        if alpha <= f32::EPSILON {
            *output = [0.0, 0.0, 0.0, 0.0];
            continue;
        }
        output[0] = (left[0] * left[3] * inverse + right[0] * right[3] * progress) / alpha;
        output[1] = (left[1] * left[3] * inverse + right[1] * right[3] * progress) / alpha;
        output[2] = (left[2] * left[3] * inverse + right[2] * right[3] * progress) / alpha;
        output[3] = alpha;
    }
}

pub(super) fn diagnose_transition_input(
    input: &TimelineTransitionInput<'_>,
    diagnostics: &mut TimelineCompositeDiagnostics,
) {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            diagnostics.effect_gpu_blockers =
                diagnostics.effect_gpu_blockers.saturating_add(u64::from(
                    mondrian_effects::get_or_lower_effect_graph_to_gpu_plan(&layer.effect_graph)
                        .is_err(),
                ));
            if effect_domain_is_blocked(&layer.effect_graph) {
                diagnostics.blocked_media_effect_domain =
                    diagnostics.blocked_media_effect_domain.saturating_add(1);
            } else if !compiled_effect_graph_supports_rgba_f32_with_domain_processor(
                &layer.effect_graph,
            ) {
                diagnostics.legacy_media_effect = diagnostics.legacy_media_effect.saturating_add(1);
            }
        }
        TimelineTransitionInput::SolidColor(layer) => {
            diagnostics.effect_gpu_blockers =
                diagnostics.effect_gpu_blockers.saturating_add(u64::from(
                    mondrian_effects::get_or_lower_effect_graph_to_gpu_plan(&layer.effect_graph)
                        .is_err(),
                ));
            if effect_domain_is_blocked(&layer.effect_graph) {
                diagnostics.blocked_solid_effect_domain =
                    diagnostics.blocked_solid_effect_domain.saturating_add(1);
            } else if !compiled_effect_graph_supports_rgba_f32_with_domain_processor(
                &layer.effect_graph,
            ) {
                diagnostics.legacy_solid_effect = diagnostics.legacy_solid_effect.saturating_add(1);
            }
        }
    }
}

pub(super) fn composite_transition_input_rgba8(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    input: &TimelineTransitionInput<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> Result<(), mondrian_effects::EffectExecutionError> {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            let descriptor = layer.frame.descriptor();
            scratch.media_source = layer
                .frame
                .rgba_f32()
                .data
                .iter()
                .flat_map(|pixel| {
                    pixel.iter().map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
                })
                .collect();
            let source = if layer.effect_graph.graph.is_identity() {
                scratch.media_source.as_slice()
            } else {
                scratch.media_effect = apply_compiled_effect_graph(
                    &scratch.media_source,
                    descriptor.width,
                    descriptor.height,
                    &layer.effect_graph,
                    layer.frame_seed,
                )?;
                scratch.media_effect.as_slice()
            };
            alpha_blend_layer(
                canvas,
                width,
                height,
                source,
                descriptor.width,
                descriptor.height,
                layer.opacity,
                layer.blend_mode,
                layer.transform,
            );
        }
        TimelineTransitionInput::SolidColor(layer) => {
            fill_solid_rgba(
                &mut scratch.solid_fill,
                width as usize,
                height as usize,
                layer.color,
            );
            let source = if layer.effect_graph.graph.is_identity() {
                scratch.solid_fill.as_slice()
            } else {
                scratch.media_effect = apply_compiled_effect_graph(
                    &scratch.solid_fill,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.frame_seed,
                )?;
                scratch.media_effect.as_slice()
            };
            alpha_blend_layer(
                canvas,
                width,
                height,
                source,
                width,
                height,
                layer.opacity,
                layer.blend_mode,
                layer.transform,
            );
        }
    }
    Ok(())
}

pub(super) fn cross_dissolve_straight_rgba8(
    output: &mut [u8],
    left: &[u8],
    right: &[u8],
    progress: f32,
) {
    let progress = progress.clamp(0.0, 1.0);
    let inverse = 1.0 - progress;
    for ((output, left), right) in
        output.chunks_exact_mut(4).zip(left.chunks_exact(4)).zip(right.chunks_exact(4))
    {
        let left_alpha = left[3] as f32 / 255.0;
        let right_alpha = right[3] as f32 / 255.0;
        let alpha = left_alpha * inverse + right_alpha * progress;
        if alpha <= f32::EPSILON {
            output.copy_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        for channel in 0..3 {
            let value = ((left[channel] as f32 / 255.0) * left_alpha * inverse
                + (right[channel] as f32 / 255.0) * right_alpha * progress)
                / alpha;
            output[channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        output[3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}
