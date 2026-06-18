//! Shared frame rendering helpers for self-hosted winit windows.
//!
//! Product shells and developer galleries should share the same text-atlas
//! upload and surface-present path so renderer behavior does not drift between
//! test windows and the real app shell.

use mondrian_ui_renderer::{DrawCommand, GlyphUpload, UiRenderer};
use mondrian_ui_text::{resolve_text_commands, TextRenderer};

/// Resource diagnostics observed while rendering a self-hosted UI frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelfHostedFrameDiagnostics {
    /// Text glyphs that failed rasterization or atlas allocation.
    pub text_missing_glyphs: u32,
    /// Raster images that failed upload or image-atlas allocation.
    pub raster_image_failures: u32,
}

impl SelfHostedFrameDiagnostics {
    /// Whether the frame rendered with any missing UI resource.
    pub fn has_failures(self) -> bool {
        self.text_missing_glyphs > 0 || self.raster_image_failures > 0
    }
}

/// Result of submitting one self-hosted UI frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfHostedFrameResult {
    /// The frame rendered and was presented to the surface.
    Presented {
        /// The frame uploaded atlas resources that should be visible on a
        /// deterministic follow-up frame across all backends.
        uploaded_resources: bool,
        /// Resource diagnostics for this frame.
        diagnostics: SelfHostedFrameDiagnostics,
    },
    /// The surface was temporarily unavailable and the frame was skipped.
    Skipped,
    /// The surface was lost/outdated and was reconfigured for the next frame.
    Reconfigured,
}

impl SelfHostedFrameResult {
    /// Whether the window should request another redraw immediately.
    pub fn needs_follow_up_redraw(self) -> bool {
        match self {
            SelfHostedFrameResult::Presented { uploaded_resources, .. } => uploaded_resources,
            SelfHostedFrameResult::Reconfigured => true,
            SelfHostedFrameResult::Skipped => false,
        }
    }

    /// Resource diagnostics for presented frames.
    pub fn diagnostics(self) -> SelfHostedFrameDiagnostics {
        match self {
            SelfHostedFrameResult::Presented { diagnostics, .. } => diagnostics,
            SelfHostedFrameResult::Skipped | SelfHostedFrameResult::Reconfigured => {
                SelfHostedFrameDiagnostics::default()
            }
        }
    }
}

/// Emits render resource diagnostics once per changed failure count.
#[derive(Debug, Default)]
pub struct SelfHostedRenderDiagnosticReporter {
    last_reported: Option<SelfHostedFrameDiagnostics>,
}

impl SelfHostedRenderDiagnosticReporter {
    /// Return diagnostics that should be logged for this frame, if any.
    pub fn changed_failure(
        &mut self,
        result: SelfHostedFrameResult,
    ) -> Option<SelfHostedFrameDiagnostics> {
        let diagnostics = result.diagnostics();
        if !diagnostics.has_failures() {
            self.last_reported = None;
            return None;
        }
        if self.last_reported == Some(diagnostics) {
            return None;
        }
        self.last_reported = Some(diagnostics);
        Some(diagnostics)
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
        let resolved_text = resolve_text_commands(commands, &mut self.text_renderer);
        let text_stats = resolved_text.stats;
        let commands = resolved_text.commands;
        let pending = renderer_glyph_uploads(self.text_renderer.take_pending_uploads());
        let uploaded_glyphs = !pending.is_empty();
        if uploaded_glyphs {
            self.ui_renderer.upload_glyphs(queue, &pending);
        }

        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                let view = output.texture.create_view(&Default::default());
                let render_stats = self.ui_renderer.render_resolved_commands(
                    device,
                    queue,
                    &view,
                    &commands,
                    screen_size,
                );
                output.present();
                SelfHostedFrameResult::Presented {
                    uploaded_resources: uploaded_glyphs || render_stats.uploaded_raster_images,
                    diagnostics: SelfHostedFrameDiagnostics {
                        text_missing_glyphs: text_stats.missing_glyphs,
                        raster_image_failures: render_stats.failed_raster_images,
                    },
                }
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
    fn frame_result_requests_follow_up_after_resource_upload_or_reconfigure() {
        assert!(!SelfHostedFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: SelfHostedFrameDiagnostics {
                text_missing_glyphs: 2,
                raster_image_failures: 1,
            },
        }
        .needs_follow_up_redraw());
        assert!(SelfHostedFrameResult::Presented {
            uploaded_resources: true,
            diagnostics: SelfHostedFrameDiagnostics::default(),
        }
        .needs_follow_up_redraw());
        assert!(SelfHostedFrameResult::Reconfigured.needs_follow_up_redraw());
        assert!(!SelfHostedFrameResult::Skipped.needs_follow_up_redraw());
    }

    #[test]
    fn render_diagnostic_reporter_only_reports_changed_failures() {
        let mut reporter = SelfHostedRenderDiagnosticReporter::default();
        let failed = SelfHostedFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: SelfHostedFrameDiagnostics {
                text_missing_glyphs: 2,
                raster_image_failures: 1,
            },
        };
        let changed = SelfHostedFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: SelfHostedFrameDiagnostics {
                text_missing_glyphs: 3,
                raster_image_failures: 1,
            },
        };
        let healthy = SelfHostedFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: SelfHostedFrameDiagnostics::default(),
        };

        assert_eq!(
            reporter.changed_failure(failed),
            Some(SelfHostedFrameDiagnostics { text_missing_glyphs: 2, raster_image_failures: 1 })
        );
        assert_eq!(reporter.changed_failure(failed), None);
        assert_eq!(
            reporter.changed_failure(changed),
            Some(SelfHostedFrameDiagnostics { text_missing_glyphs: 3, raster_image_failures: 1 })
        );
        assert_eq!(reporter.changed_failure(healthy), None);
        assert_eq!(
            reporter.changed_failure(failed),
            Some(SelfHostedFrameDiagnostics { text_missing_glyphs: 2, raster_image_failures: 1 })
        );
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
