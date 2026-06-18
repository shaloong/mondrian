//! Shared frame rendering helpers for self-hosted winit windows.
//!
//! Product shells and developer galleries should share the same text-atlas
//! upload and surface-present path so renderer behavior does not drift between
//! test windows and the real app shell.

use mondrian_ui_renderer::{DrawCommand, GlyphUpload, UiRenderer};
use mondrian_ui_text::{resolve_text_commands, TextRenderer};

/// Result of submitting one self-hosted UI frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfHostedFrameResult {
    /// The frame rendered and was presented to the surface.
    Presented,
    /// The surface was temporarily unavailable and the frame was skipped.
    Skipped,
    /// The surface was lost/outdated and was reconfigured for the next frame.
    Reconfigured,
}

impl SelfHostedFrameResult {
    /// Whether the window should request another redraw immediately.
    pub fn needs_follow_up_redraw(self) -> bool {
        matches!(self, SelfHostedFrameResult::Reconfigured)
    }
}

/// GPU frame renderer shared by self-hosted app and demo windows.
pub struct SelfHostedFrameRenderer {
    ui_renderer: UiRenderer,
    text_renderer: TextRenderer,
}

impl SelfHostedFrameRenderer {
    /// Create a renderer for a configured wgpu surface format.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        Self {
            ui_renderer: UiRenderer::new(device, surface_format),
            text_renderer: TextRenderer::new(),
        }
    }

    /// Resolve text draw commands, upload pending glyphs, and present a frame.
    pub fn render_draw_commands(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: &wgpu::Surface<'_>,
        config: &wgpu::SurfaceConfiguration,
        screen_size: (u32, u32),
        commands: Vec<DrawCommand>,
    ) -> SelfHostedFrameResult {
        let commands = resolve_text_commands(commands, &mut self.text_renderer);
        let pending = renderer_glyph_uploads(self.text_renderer.take_pending_uploads());
        if !pending.is_empty() {
            self.ui_renderer.upload_glyphs(queue, &pending);
        }

        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                let view = output.texture.create_view(&Default::default());
                self.ui_renderer.render_resolved_commands(
                    device,
                    queue,
                    &view,
                    &commands,
                    screen_size,
                );
                output.present();
                SelfHostedFrameResult::Presented
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                SelfHostedFrameResult::Skipped
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(device, config);
                SelfHostedFrameResult::Reconfigured
            }
            _ => SelfHostedFrameResult::Skipped,
        }
    }
}

fn renderer_glyph_uploads(uploads: Vec<mondrian_ui_text::atlas::GlyphUpload>) -> Vec<GlyphUpload> {
    uploads
        .into_iter()
        .map(|upload| GlyphUpload {
            x: upload.x,
            y: upload.y,
            width: upload.width,
            height: upload.height,
            data: upload.data,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_result_requests_follow_up_only_after_reconfigure() {
        assert!(!SelfHostedFrameResult::Presented.needs_follow_up_redraw());
        assert!(SelfHostedFrameResult::Reconfigured.needs_follow_up_redraw());
        assert!(!SelfHostedFrameResult::Skipped.needs_follow_up_redraw());
    }

    #[test]
    fn renderer_glyph_uploads_preserve_atlas_coordinates_and_alpha_data() {
        let uploads = renderer_glyph_uploads(vec![mondrian_ui_text::atlas::GlyphUpload {
            x: 11,
            y: 17,
            width: 3,
            height: 2,
            data: vec![0, 64, 128, 192, 255, 32],
        }]);

        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].x, 11);
        assert_eq!(uploads[0].y, 17);
        assert_eq!(uploads[0].width, 3);
        assert_eq!(uploads[0].height, 2);
        assert_eq!(uploads[0].data, vec![0, 64, 128, 192, 255, 32]);
    }
}
