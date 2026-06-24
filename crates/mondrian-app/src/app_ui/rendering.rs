//! Shared frame rendering helpers for app UI winit windows.
//!
//! Product shells and developer galleries should share the same text-atlas
//! upload and surface-present path so renderer behavior does not drift between
//! test windows and the real app shell.

use mondrian_ui_renderer::{DrawCommand, GlyphUpload, UiRenderer};
use mondrian_ui_text::{resolve_text_commands, TextRenderer};

/// Resource diagnostics observed while rendering an app UI frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppUiFrameDiagnostics {
    /// Text glyphs that failed rasterization or atlas allocation.
    pub text_missing_glyphs: u32,
    /// Raster images that failed upload or image-atlas allocation.
    pub raster_image_failures: u32,
}

impl AppUiFrameDiagnostics {
    /// Whether the frame rendered with any missing UI resource.
    pub fn has_failures(self) -> bool {
        self.text_missing_glyphs > 0 || self.raster_image_failures > 0
    }
}

/// Backend-level surface event observed while acquiring or presenting a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiBackendEvent {
    /// The surface produced a suboptimal texture that can be presented, but the
    /// backend recommends reconfiguration soon.
    SurfaceSuboptimal,
    /// The backend timed out while acquiring the next surface texture.
    SurfaceTimeout,
    /// The surface is occluded and skipped this frame.
    SurfaceOccluded,
    /// The surface became outdated and was reconfigured.
    SurfaceOutdated,
    /// The surface was lost and was reconfigured.
    SurfaceLost,
    /// A non-exhaustive backend surface state was reported by wgpu.
    SurfaceUnavailable,
}

/// Result of submitting one app UI frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiFrameResult {
    /// The frame rendered and was presented to the surface.
    Presented {
        /// The frame uploaded atlas resources that should be visible on a
        /// deterministic follow-up frame across all backends.
        uploaded_resources: bool,
        /// Resource diagnostics for this frame.
        diagnostics: AppUiFrameDiagnostics,
        /// Optional backend event for presented frames.
        backend_event: Option<AppUiBackendEvent>,
    },
    /// The surface was temporarily unavailable and the frame was skipped.
    Skipped {
        /// Backend event that caused the skip.
        backend_event: AppUiBackendEvent,
    },
    /// The surface was lost/outdated and was reconfigured for the next frame.
    Reconfigured {
        /// Backend event that caused the reconfigure.
        backend_event: AppUiBackendEvent,
    },
}

impl AppUiFrameResult {
    /// Whether the window should request another redraw immediately.
    pub fn needs_follow_up_redraw(self) -> bool {
        match self {
            AppUiFrameResult::Presented { uploaded_resources, .. } => uploaded_resources,
            AppUiFrameResult::Reconfigured { .. } => true,
            AppUiFrameResult::Skipped { .. } => false,
        }
    }

    /// Resource diagnostics for presented frames.
    pub fn diagnostics(self) -> AppUiFrameDiagnostics {
        match self {
            AppUiFrameResult::Presented { diagnostics, .. } => diagnostics,
            AppUiFrameResult::Skipped { .. } | AppUiFrameResult::Reconfigured { .. } => {
                AppUiFrameDiagnostics::default()
            }
        }
    }

    /// Backend surface event associated with this frame, if any.
    pub fn backend_event(self) -> Option<AppUiBackendEvent> {
        match self {
            AppUiFrameResult::Presented { backend_event, .. } => backend_event,
            AppUiFrameResult::Skipped { backend_event }
            | AppUiFrameResult::Reconfigured { backend_event } => Some(backend_event),
        }
    }
}

/// Emits render resource diagnostics once per changed failure count.
#[derive(Debug, Default)]
pub struct AppUiRenderDiagnosticReporter {
    last_reported: Option<AppUiFrameDiagnostics>,
    last_backend_event: Option<AppUiBackendEvent>,
}

impl AppUiRenderDiagnosticReporter {
    /// Return diagnostics that should be logged for this frame, if any.
    pub fn changed_failure(&mut self, result: AppUiFrameResult) -> Option<AppUiFrameDiagnostics> {
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

    /// Return backend diagnostics that should be logged for this frame, if any.
    pub fn changed_backend_event(&mut self, result: AppUiFrameResult) -> Option<AppUiBackendEvent> {
        let event = result.backend_event();
        if event.is_none() {
            self.last_backend_event = None;
            return None;
        }
        if self.last_backend_event == event {
            return None;
        }
        self.last_backend_event = event;
        event
    }
}

/// GPU frame renderer shared by app UI and demo windows.
pub struct AppUiFrameRenderer {
    ui_renderer: UiRenderer,
    text_renderer: TextRenderer,
}

impl AppUiFrameRenderer {
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
    ) -> AppUiFrameResult {
        let resolved_text = resolve_text_commands(commands, &mut self.text_renderer);
        let text_stats = resolved_text.stats;
        let commands = resolved_text.commands;
        let pending = renderer_glyph_uploads(self.text_renderer.take_pending_uploads());
        let uploaded_glyphs = !pending.is_empty();
        if uploaded_glyphs {
            self.ui_renderer.upload_glyphs(queue, &pending);
        }

        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output) => {
                let view = output.texture.create_view(&Default::default());
                let render_stats = self.ui_renderer.render_resolved_commands(
                    device,
                    queue,
                    &view,
                    &commands,
                    screen_size,
                );
                output.present();
                AppUiFrameResult::Presented {
                    uploaded_resources: uploaded_glyphs || render_stats.uploaded_raster_images,
                    diagnostics: AppUiFrameDiagnostics {
                        text_missing_glyphs: text_stats.missing_glyphs,
                        raster_image_failures: render_stats.failed_raster_images,
                    },
                    backend_event: None,
                }
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                let view = output.texture.create_view(&Default::default());
                let render_stats = self.ui_renderer.render_resolved_commands(
                    device,
                    queue,
                    &view,
                    &commands,
                    screen_size,
                );
                output.present();
                AppUiFrameResult::Presented {
                    uploaded_resources: uploaded_glyphs || render_stats.uploaded_raster_images,
                    diagnostics: AppUiFrameDiagnostics {
                        text_missing_glyphs: text_stats.missing_glyphs,
                        raster_image_failures: render_stats.failed_raster_images,
                    },
                    backend_event: Some(AppUiBackendEvent::SurfaceSuboptimal),
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                AppUiFrameResult::Skipped { backend_event: AppUiBackendEvent::SurfaceTimeout }
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                AppUiFrameResult::Skipped { backend_event: AppUiBackendEvent::SurfaceOccluded }
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                surface.configure(device, config);
                AppUiFrameResult::Reconfigured { backend_event: AppUiBackendEvent::SurfaceOutdated }
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(device, config);
                AppUiFrameResult::Reconfigured { backend_event: AppUiBackendEvent::SurfaceLost }
            }
            _ => AppUiFrameResult::Skipped {
                backend_event: AppUiBackendEvent::SurfaceUnavailable,
            },
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
        assert!(!AppUiFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: AppUiFrameDiagnostics { text_missing_glyphs: 2, raster_image_failures: 1 },
            backend_event: None,
        }
        .needs_follow_up_redraw());
        assert!(AppUiFrameResult::Presented {
            uploaded_resources: true,
            diagnostics: AppUiFrameDiagnostics::default(),
            backend_event: None,
        }
        .needs_follow_up_redraw());
        assert!(
            AppUiFrameResult::Reconfigured { backend_event: AppUiBackendEvent::SurfaceLost }
                .needs_follow_up_redraw()
        );
        assert!(
            !AppUiFrameResult::Skipped { backend_event: AppUiBackendEvent::SurfaceTimeout }
                .needs_follow_up_redraw()
        );
    }

    #[test]
    fn render_diagnostic_reporter_only_reports_changed_failures() {
        let mut reporter = AppUiRenderDiagnosticReporter::default();
        let failed = AppUiFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: AppUiFrameDiagnostics { text_missing_glyphs: 2, raster_image_failures: 1 },
            backend_event: None,
        };
        let changed = AppUiFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: AppUiFrameDiagnostics { text_missing_glyphs: 3, raster_image_failures: 1 },
            backend_event: None,
        };
        let healthy = AppUiFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: AppUiFrameDiagnostics::default(),
            backend_event: None,
        };

        assert_eq!(
            reporter.changed_failure(failed),
            Some(AppUiFrameDiagnostics { text_missing_glyphs: 2, raster_image_failures: 1 })
        );
        assert_eq!(reporter.changed_failure(failed), None);
        assert_eq!(
            reporter.changed_failure(changed),
            Some(AppUiFrameDiagnostics { text_missing_glyphs: 3, raster_image_failures: 1 })
        );
        assert_eq!(reporter.changed_failure(healthy), None);
        assert_eq!(
            reporter.changed_failure(failed),
            Some(AppUiFrameDiagnostics { text_missing_glyphs: 2, raster_image_failures: 1 })
        );
    }

    #[test]
    fn render_diagnostic_reporter_only_reports_changed_backend_events() {
        let mut reporter = AppUiRenderDiagnosticReporter::default();
        let timeout =
            AppUiFrameResult::Skipped { backend_event: AppUiBackendEvent::SurfaceTimeout };
        let lost = AppUiFrameResult::Reconfigured { backend_event: AppUiBackendEvent::SurfaceLost };
        let healthy = AppUiFrameResult::Presented {
            uploaded_resources: false,
            diagnostics: AppUiFrameDiagnostics::default(),
            backend_event: None,
        };

        assert_eq!(
            reporter.changed_backend_event(timeout),
            Some(AppUiBackendEvent::SurfaceTimeout)
        );
        assert_eq!(reporter.changed_backend_event(timeout), None);
        assert_eq!(
            reporter.changed_backend_event(lost),
            Some(AppUiBackendEvent::SurfaceLost)
        );
        assert_eq!(reporter.changed_backend_event(healthy), None);
        assert_eq!(
            reporter.changed_backend_event(timeout),
            Some(AppUiBackendEvent::SurfaceTimeout)
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
