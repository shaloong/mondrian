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
    mut canvas: mondrian_effects::EffectFloatFrame,
    input: &TimelineTransitionInput<'_>,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
    control: &mut mondrian_effects::EffectProcessorControl<'_>,
) -> Result<mondrian_effects::EffectFloatFrame, EffectFloatExecutionError> {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            let frame = layer.frame.rgba_f32();
            let effect_output;
            let input = working_float_view(frame);
            let source = if layer.effect_graph.graph().is_identity() {
                input
            } else {
                effect_output = apply_effect_graph_f32(
                    &mut scratch.effect_execution,
                    &mut scratch.color_execution,
                    input,
                    &layer.effect_graph,
                    layer.frame_seed,
                    runtime,
                    control,
                )?;
                effect_output.view()
            };
            canvas = alpha_blend_typed_float_layer(
                canvas,
                source,
                layer.opacity,
                layer.blend_mode,
                layer.transform,
                layer.frame_seed,
                &mut scratch.cpu_visual_execution,
                control,
            )?
            .0;
        }
        TimelineTransitionInput::SolidColor(layer) => {
            let extent = canvas.extent();
            let pixel = [layer.color.r, layer.color.g, layer.color.b, layer.color.a];
            scratch.solid_fill_f32.resize(canvas.len(), pixel);
            scratch.solid_fill_f32.fill(pixel);
            let input = mondrian_effects::EffectFloatFrameView::new(
                extent,
                scene_linear_float_format(ColorFrameAlpha::StraightCoverage),
                &scratch.solid_fill_f32,
            );
            let effect_output;
            let source = if layer.effect_graph.graph().is_identity() {
                input
            } else {
                effect_output = apply_effect_graph_f32(
                    &mut scratch.effect_execution,
                    &mut scratch.color_execution,
                    input,
                    &layer.effect_graph,
                    layer.frame_seed,
                    runtime,
                    control,
                )?;
                effect_output.view()
            };
            canvas = alpha_blend_typed_float_layer(
                canvas,
                source,
                layer.opacity,
                layer.blend_mode,
                layer.transform,
                layer.frame_seed,
                &mut scratch.cpu_visual_execution,
                control,
            )?
            .0;
        }
    }
    Ok(canvas)
}

#[cfg(test)]
pub(crate) fn cross_dissolve_straight_rgba_f32(
    output: &mut [[f32; 4]],
    left: &[[f32; 4]],
    right: &[[f32; 4]],
    progress: f32,
) {
    let progress = progress.clamp(0.0, 1.0);
    for ((output, left), right) in output.iter_mut().zip(left).zip(right) {
        *output = mondrian_effects::mix_straight_rgba(*left, *right, progress);
    }
}

pub(super) fn diagnose_transition_input(
    input: &TimelineTransitionInput<'_>,
    diagnostics: &mut TimelineCompositeDiagnostics,
    session: &mut EffectExecutionSession,
) {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            diagnostics.effect_gpu_blockers = diagnostics.effect_gpu_blockers.saturating_add(
                u64::from(session.get_or_lower_gpu_plan(&layer.effect_graph).is_err()),
            );
        }
        TimelineTransitionInput::SolidColor(layer) => {
            diagnostics.effect_gpu_blockers = diagnostics.effect_gpu_blockers.saturating_add(
                u64::from(session.get_or_lower_gpu_plan(&layer.effect_graph).is_err()),
            );
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
            let source = if layer.effect_graph.graph().is_identity() {
                scratch.media_source.as_slice()
            } else {
                scratch.media_effect = scratch.effect_execution.apply_compiled_rgba8(
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
            let source = if layer.effect_graph.graph().is_identity() {
                scratch.solid_fill.as_slice()
            } else {
                scratch.media_effect = scratch.effect_execution.apply_compiled_rgba8(
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
        if !mondrian_effects::has_positive_coverage(alpha) {
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
