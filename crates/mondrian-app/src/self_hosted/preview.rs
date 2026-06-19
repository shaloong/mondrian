//! Viewer preview service for the self-hosted UI host.
//!
//! The service owns render-plan interpretation and preview-frame cache keys.
//! Panels stay read-only and only consume `ViewerFrameImage` payloads.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mondrian_renderer::{
    build_timeline_render_plan, composite_timeline_elements, TimelineCompositeElement,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineRenderPlanElement,
    TimelineSolidColorLayer,
};
use mondrian_ui_widgets::ViewerFrameImage;

use crate::app::AppState;
use crate::self_hosted::panels::ViewerPreviewSource;

/// Host-owned preview renderer used by the self-hosted viewer panel.
///
/// This first path renders solid-color render-plan elements through the shared
/// renderer compositor. Media and nested-sequence decode can attach here without
/// changing panel models or widget APIs.
#[derive(Default)]
pub struct SelfHostedPreviewService {
    scratch: RefCell<TimelineCompositeScratch>,
}

impl SelfHostedPreviewService {
    /// Create an empty preview service.
    pub fn new() -> Self {
        Self::default()
    }

    fn render_solid_preview(&self, state: &AppState) -> Option<ViewerFrameImage> {
        let sequence = state.sequence.as_ref()?;
        let frame = state.current_frame().max(0);
        let plan = build_timeline_render_plan(sequence, frame);
        if plan.is_empty() {
            return None;
        }

        let mut elements = Vec::with_capacity(plan.len());
        for element in plan {
            match element {
                TimelineRenderPlanElement::SolidColor(solid) => {
                    elements.push(TimelineCompositeElement::SolidColor(
                        TimelineSolidColorLayer {
                            color: solid.color,
                            opacity: solid.opacity,
                            blend_mode: solid.blend_mode,
                            transform: solid.transform,
                            effect_graph: solid.effect_graph,
                            frame_seed: solid.frame_seed,
                        },
                    ));
                }
                TimelineRenderPlanElement::Media(_)
                | TimelineRenderPlanElement::Adjustment(_)
                | TimelineRenderPlanElement::NestedSequence(_) => return None,
            }
        }

        let (width, height) = preview_dimensions(state)?;
        let rgba = composite_timeline_elements(
            width,
            height,
            &elements,
            TimelineCompositeOptions::default(),
            &mut self.scratch.borrow_mut(),
        );
        let key = preview_cache_key(frame, width, height, &rgba);
        ViewerFrameImage::new(key, width, height, rgba)
    }
}

impl ViewerPreviewSource for SelfHostedPreviewService {
    fn viewer_frame_for_state(&self, state: &AppState) -> Option<ViewerFrameImage> {
        self.render_solid_preview(state)
    }
}

fn preview_dimensions(state: &AppState) -> Option<(u32, u32)> {
    let sequence = state.sequence.as_ref()?;
    let resolution = sequence.settings.resolution;
    let scale = sequence.settings.preview.resolution_scale.clamp(0.125, 1.0);
    let width = ((resolution.width as f32 * scale).round() as u32).max(1);
    let height = ((resolution.height as f32 * scale).round() as u32).max(1);
    Some((width, height))
}

fn preview_cache_key(frame: i64, width: u32, height: u32, rgba: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    rgba.hash(&mut hasher);
    format!(
        "self-hosted-viewer:{width}x{height}:f{frame}:p{:016x}",
        hasher.finish()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_core::Color;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;

    fn state_with_solid_color_clip(color: Color) -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("preview");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                color,
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("solid clip should be insertable");
        state.sequence = Some(sequence);
        state.seek(4);
        state
    }

    #[test]
    fn solid_color_sequence_returns_preview_frame_at_preview_scale() {
        let service = SelfHostedPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let frame = service
            .viewer_frame_for_state(&state)
            .expect("solid-color timeline should preview");

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.rgba.len(), 960 * 540 * 4);
        assert!(frame.key.contains("self-hosted-viewer:960x540:f4:"));
    }

    #[test]
    fn unsupported_media_plan_returns_no_partial_preview() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("media");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("media clip should be insertable");
        state.sequence = Some(sequence);

        let service = SelfHostedPreviewService::new();

        assert!(service.viewer_frame_for_state(&state).is_none());
    }

    #[test]
    fn preview_cache_key_changes_when_pixels_change() {
        let service = SelfHostedPreviewService::new();
        let first = service
            .viewer_frame_for_state(&state_with_solid_color_clip(Color::from_rgba8(
                255, 0, 0, 255,
            )))
            .expect("first preview");
        let second = service
            .viewer_frame_for_state(&state_with_solid_color_clip(Color::from_rgba8(
                0, 0, 255, 255,
            )))
            .expect("second preview");

        assert_ne!(first.key, second.key);
    }
}
