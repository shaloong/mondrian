//! Shared frame rendering helpers for app UI winit windows.
//!
//! Product shells and developer galleries should share the same text-atlas
//! upload and surface-present path so renderer behavior does not drift between
//! test windows and the real app shell.

use mondrian_renderer::GpuNativeDecodedFrameImportSupport;
use mondrian_ui_renderer::{DrawCommand, ExternalTextureKey, GlyphUpload, UiRenderer};
use mondrian_ui_text::{resolve_text_commands, TextRenderer};
use std::time::Instant;

const SLOW_FRAME_CPU_MICROS: u64 = 16_000;
const HIGH_GPU_UPLOAD_BYTES: u64 = 4 * 1024 * 1024;
const HIGH_ATLAS_OCCUPANCY_BPS: u16 = 8_500;
const LOW_LARGEST_FREE_RECT_PIXELS: u64 = 64 * 64;

/// Resource diagnostics observed while rendering an app UI frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppUiFrameDiagnostics {
    /// Text glyphs that failed rasterization or atlas allocation.
    pub text_missing_glyphs: u32,
    /// Raster images that failed upload or image-atlas allocation.
    pub raster_image_failures: u32,
    /// External texture draw commands whose key was missing from the renderer registry.
    pub external_texture_failures: u32,
}

impl AppUiFrameDiagnostics {
    /// Whether the frame rendered with any missing UI resource.
    pub fn has_failures(self) -> bool {
        self.text_missing_glyphs > 0
            || self.raster_image_failures > 0
            || self.external_texture_failures > 0
    }
}

/// Structured frame-cost and resource-pressure metrics for a presented app UI frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppUiFrameMetrics {
    /// Draw commands after text resolution and raster-image fallback resolution.
    pub command_count: usize,
    /// Low-level render batches produced before render-pass filtering.
    pub batch_count: usize,
    /// Vertices produced before render-pass filtering.
    pub vertex_count: usize,
    /// CPU time spent in the full app UI frame path, including text resolution,
    /// glyph uploads, raster image resolution, batching, and surface present.
    pub frame_cpu_time_micros: u64,
    /// CPU time spent inside the low-level renderer entry point.
    pub renderer_cpu_time_micros: u64,
    /// Bytes uploaded to the glyph atlas this frame.
    pub glyph_upload_bytes: u64,
    /// Bytes uploaded to the renderer-owned raster-image atlas this frame.
    pub raster_image_upload_bytes: u64,
    /// Bytes uploaded by the low-level renderer entry point.
    pub renderer_upload_bytes: u64,
    /// Total UI upload bytes observed by this app UI frame path.
    pub total_upload_bytes: u64,
    /// Renderer-owned raster image atlas entries after this frame.
    pub image_atlas_entries: usize,
    /// Renderer-owned raster image atlas occupancy in basis points.
    pub image_atlas_occupancy_bps: u16,
    /// Largest reusable raster image atlas free rectangle area.
    pub image_atlas_largest_free_rect_pixels: u64,
    /// Raster image atlas page resets triggered by this frame.
    pub image_atlas_page_resets_this_frame: u32,
    /// Raster image atlas allocation failures accumulated by the renderer.
    pub image_atlas_failed_allocations: u64,
    /// External GPU textures registered with the UI renderer.
    pub external_texture_entries: usize,
    /// External texture draw commands whose key was missing from the renderer registry.
    pub external_texture_failures: u32,
    /// Render batches that sampled an external GPU texture.
    pub external_texture_batches: usize,
}

impl AppUiFrameMetrics {
    fn from_stats(
        frame_cpu_time_micros: u64,
        glyph_upload_bytes: u64,
        render_stats: mondrian_ui_renderer::UiRenderFrameStats,
    ) -> Self {
        let image_atlas_occupancy_bps = if render_stats.image_atlas_total_pixels == 0 {
            0
        } else {
            ((render_stats.image_atlas_used_pixels.saturating_mul(10_000)
                / render_stats.image_atlas_total_pixels)
                .min(10_000)) as u16
        };
        Self {
            command_count: render_stats.command_count,
            batch_count: render_stats.batch_count,
            vertex_count: render_stats.vertex_count,
            frame_cpu_time_micros,
            renderer_cpu_time_micros: render_stats.frame_cpu_time_micros,
            glyph_upload_bytes,
            raster_image_upload_bytes: render_stats.raster_image_upload_bytes,
            renderer_upload_bytes: render_stats.gpu_upload_bytes,
            total_upload_bytes: glyph_upload_bytes.saturating_add(render_stats.gpu_upload_bytes),
            image_atlas_entries: render_stats.image_atlas_entries,
            image_atlas_occupancy_bps,
            image_atlas_largest_free_rect_pixels: render_stats.image_atlas_largest_free_rect_pixels,
            image_atlas_page_resets_this_frame: render_stats.image_atlas_page_resets_this_frame,
            image_atlas_failed_allocations: render_stats.image_atlas_failed_allocations,
            external_texture_entries: render_stats.external_texture_entries,
            external_texture_failures: render_stats.failed_external_textures,
            external_texture_batches: render_stats.submitted_external_texture_batches,
        }
    }

    /// Whether this frame crossed a slow CPU-frame threshold.
    pub fn is_slow_frame(self) -> bool {
        self.frame_cpu_time_micros >= SLOW_FRAME_CPU_MICROS
    }

    /// Whether this frame uploaded enough data to be worth surfacing.
    pub fn has_high_upload_pressure(self) -> bool {
        self.total_upload_bytes >= HIGH_GPU_UPLOAD_BYTES
    }

    /// Whether the raster image atlas is near pressure or has churned.
    pub fn has_image_atlas_pressure(self) -> bool {
        self.image_atlas_page_resets_this_frame > 0
            || self.image_atlas_failed_allocations > 0
            || self.image_atlas_occupancy_bps >= HIGH_ATLAS_OCCUPANCY_BPS
            || (self.image_atlas_entries > 0
                && self.image_atlas_largest_free_rect_pixels <= LOW_LARGEST_FREE_RECT_PIXELS)
    }
}

/// App UI frame pressure event suitable for structured logs and future overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppUiFramePressure {
    /// Whether the full app UI frame crossed the slow CPU-frame threshold.
    pub slow_frame: bool,
    /// Whether the frame uploaded enough data to indicate resource churn.
    pub high_upload: bool,
    /// Whether the renderer-owned raster image atlas is under pressure.
    pub image_atlas_pressure: bool,
    /// Metrics that caused this pressure event.
    pub metrics: AppUiFrameMetrics,
}

impl AppUiFramePressure {
    fn from_metrics(metrics: AppUiFrameMetrics) -> Option<Self> {
        let pressure = Self {
            slow_frame: metrics.is_slow_frame(),
            high_upload: metrics.has_high_upload_pressure(),
            image_atlas_pressure: metrics.has_image_atlas_pressure(),
            metrics,
        };
        (pressure.slow_frame || pressure.high_upload || pressure.image_atlas_pressure)
            .then_some(pressure)
    }

    fn signature(self) -> AppUiFramePressureSignature {
        AppUiFramePressureSignature {
            slow_frame: self.slow_frame,
            high_upload: self.high_upload,
            image_atlas_pressure: self.image_atlas_pressure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppUiFramePressureSignature {
    slow_frame: bool,
    high_upload: bool,
    image_atlas_pressure: bool,
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
        /// Structured frame-cost and resource-pressure metrics.
        metrics: AppUiFrameMetrics,
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

    /// Structured metrics for presented frames.
    pub fn metrics(self) -> AppUiFrameMetrics {
        match self {
            AppUiFrameResult::Presented { metrics, .. } => metrics,
            AppUiFrameResult::Skipped { .. } | AppUiFrameResult::Reconfigured { .. } => {
                AppUiFrameMetrics::default()
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
    last_pressure: Option<AppUiFramePressureSignature>,
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

    /// Return frame pressure that should be logged for this frame, if any.
    pub fn changed_pressure(&mut self, result: AppUiFrameResult) -> Option<AppUiFramePressure> {
        let pressure = AppUiFramePressure::from_metrics(result.metrics());
        let Some(pressure) = pressure else {
            self.last_pressure = None;
            return None;
        };
        let signature = pressure.signature();
        if self.last_pressure == Some(signature) {
            return None;
        }
        self.last_pressure = Some(signature);
        Some(pressure)
    }
}

/// GPU frame renderer shared by app UI and demo windows.
pub struct AppUiFrameRenderer {
    ui_renderer: UiRenderer,
    text_renderer: TextRenderer,
    native_decoded_frame_import_support: GpuNativeDecodedFrameImportSupport,
}

impl AppUiFrameRenderer {
    /// Create a renderer for a configured wgpu surface format.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        Self {
            ui_renderer: UiRenderer::new(device, surface_format),
            text_renderer: TextRenderer::new(),
            native_decoded_frame_import_support: GpuNativeDecodedFrameImportSupport::unavailable(),
        }
    }

    /// Create a renderer and attach adapter-specific native import diagnostics.
    pub fn new_with_adapter_info(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        adapter_info: &wgpu::AdapterInfo,
    ) -> Self {
        let mut renderer = Self::new(device, surface_format);
        renderer.native_decoded_frame_import_support =
            native_decoded_frame_import_support_from_adapter(adapter_info);
        renderer
    }

    /// Register or replace a GPU texture view for viewer/UI external texture draws.
    ///
    /// Callers keep ownership of the texture allocation. The renderer owns only
    /// a bind-group reference keyed by a stable frame/content identifier.
    pub fn register_external_texture_view(
        &mut self,
        device: &wgpu::Device,
        key: ExternalTextureKey,
        texture_view: &wgpu::TextureView,
    ) {
        self.ui_renderer.register_external_texture_view(device, key, texture_view);
    }

    /// Remove a previously registered external GPU texture view.
    pub fn unregister_external_texture(&mut self, key: &ExternalTextureKey) -> bool {
        self.ui_renderer.unregister_external_texture(key)
    }

    /// Number of external GPU texture views currently registered with the UI renderer.
    pub fn external_texture_count(&self) -> usize {
        self.ui_renderer.external_texture_count()
    }

    /// Renderer backend support for importing native hardware-decoded video surfaces.
    ///
    /// This remains fail-closed until the concrete wgpu backend can import and
    /// sample an OS decoder surface into a renderer-owned float working frame.
    pub fn native_decoded_frame_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.native_decoded_frame_import_support.clone()
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
        let frame_started = Instant::now();
        let resolved_text = resolve_text_commands(commands, &mut self.text_renderer);
        let text_stats = resolved_text.stats;
        let commands = resolved_text.commands;
        let pending = renderer_glyph_uploads(self.text_renderer.take_pending_uploads());
        let uploaded_glyphs = !pending.is_empty();
        let glyph_upload_stats = if uploaded_glyphs {
            self.ui_renderer.upload_glyphs(queue, &pending)
        } else {
            mondrian_ui_renderer::GlyphUploadStats::default()
        };

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
                queue.present(output);
                presented_result(
                    frame_started,
                    glyph_upload_stats.upload_bytes,
                    uploaded_glyphs,
                    text_stats.missing_glyphs,
                    render_stats,
                    None,
                )
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
                queue.present(output);
                presented_result(
                    frame_started,
                    glyph_upload_stats.upload_bytes,
                    uploaded_glyphs,
                    text_stats.missing_glyphs,
                    render_stats,
                    Some(AppUiBackendEvent::SurfaceSuboptimal),
                )
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

fn native_decoded_frame_import_support_from_adapter(
    adapter_info: &wgpu::AdapterInfo,
) -> GpuNativeDecodedFrameImportSupport {
    let backend_label = format!("{:?}", adapter_info.backend);
    let reason = match adapter_info.backend {
        wgpu::Backend::Dx12 => {
            "wgpu Dx12 renderer has no D3D11 shared texture native video import bridge connected"
        }
        wgpu::Backend::Vulkan => {
            "wgpu Vulkan renderer has no external-memory native video import bridge connected"
        }
        wgpu::Backend::Metal => {
            "wgpu Metal renderer has no CVPixelBuffer/IOSurface native video import bridge connected"
        }
        wgpu::Backend::Gl => {
            "wgpu GL renderer has no native video texture import bridge connected"
        }
        wgpu::Backend::BrowserWebGpu => {
            "browser WebGPU renderer cannot import desktop native decoder surfaces"
        }
        wgpu::Backend::Noop => "noop renderer cannot import native decoder surfaces",
    };
    GpuNativeDecodedFrameImportSupport::unavailable_with_reason(backend_label, reason)
}

fn presented_result(
    frame_started: Instant,
    glyph_upload_bytes: u64,
    uploaded_glyphs: bool,
    text_missing_glyphs: u32,
    render_stats: mondrian_ui_renderer::UiRenderFrameStats,
    backend_event: Option<AppUiBackendEvent>,
) -> AppUiFrameResult {
    let frame_cpu_time_micros =
        frame_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    AppUiFrameResult::Presented {
        uploaded_resources: uploaded_glyphs || render_stats.uploaded_raster_images,
        diagnostics: AppUiFrameDiagnostics {
            text_missing_glyphs,
            raster_image_failures: render_stats.failed_raster_images,
            external_texture_failures: render_stats.failed_external_textures,
        },
        metrics: AppUiFrameMetrics::from_stats(
            frame_cpu_time_micros,
            glyph_upload_bytes,
            render_stats,
        ),
        backend_event,
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

    fn presented(
        uploaded_resources: bool,
        diagnostics: AppUiFrameDiagnostics,
        metrics: AppUiFrameMetrics,
        backend_event: Option<AppUiBackendEvent>,
    ) -> AppUiFrameResult {
        AppUiFrameResult::Presented {
            uploaded_resources,
            diagnostics,
            metrics,
            backend_event,
        }
    }

    fn failures(text_missing_glyphs: u32, raster_image_failures: u32) -> AppUiFrameDiagnostics {
        AppUiFrameDiagnostics {
            text_missing_glyphs,
            raster_image_failures,
            external_texture_failures: 0,
        }
    }

    #[test]
    fn frame_result_requests_follow_up_after_resource_upload_or_reconfigure() {
        assert!(
            !presented(false, failures(2, 1), AppUiFrameMetrics::default(), None,)
                .needs_follow_up_redraw()
        );
        assert!(presented(
            true,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics::default(),
            None,
        )
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
        let failed = presented(false, failures(2, 1), AppUiFrameMetrics::default(), None);
        let changed = presented(
            false,
            AppUiFrameDiagnostics {
                text_missing_glyphs: 3,
                raster_image_failures: 1,
                external_texture_failures: 1,
            },
            AppUiFrameMetrics::default(),
            None,
        );
        let healthy = presented(
            false,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics::default(),
            None,
        );

        assert_eq!(reporter.changed_failure(failed), Some(failures(2, 1)));
        assert_eq!(reporter.changed_failure(failed), None);
        assert_eq!(
            reporter.changed_failure(changed),
            Some(AppUiFrameDiagnostics {
                text_missing_glyphs: 3,
                raster_image_failures: 1,
                external_texture_failures: 1,
            })
        );
        assert_eq!(reporter.changed_failure(healthy), None);
        assert_eq!(reporter.changed_failure(failed), Some(failures(2, 1)));
    }

    #[test]
    fn render_diagnostic_reporter_only_reports_changed_backend_events() {
        let mut reporter = AppUiRenderDiagnosticReporter::default();
        let timeout =
            AppUiFrameResult::Skipped { backend_event: AppUiBackendEvent::SurfaceTimeout };
        let lost = AppUiFrameResult::Reconfigured { backend_event: AppUiBackendEvent::SurfaceLost };
        let healthy = presented(
            false,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics::default(),
            None,
        );

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
    fn native_decoded_frame_import_support_reports_wgpu_backend_blocker() {
        let adapter_info =
            wgpu::AdapterInfo::new(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Dx12);
        let support = native_decoded_frame_import_support_from_adapter(&adapter_info);

        assert!(!support.renderer_backend_ready);
        assert_eq!(support.renderer_backend_label.as_deref(), Some("Dx12"));
        assert!(support
            .unavailable_reason
            .as_deref()
            .unwrap_or_default()
            .contains("D3D11 shared texture"));
    }

    #[test]
    fn frame_metrics_detect_slow_upload_and_atlas_pressure() {
        let metrics = AppUiFrameMetrics {
            frame_cpu_time_micros: SLOW_FRAME_CPU_MICROS,
            total_upload_bytes: HIGH_GPU_UPLOAD_BYTES,
            image_atlas_entries: 4,
            image_atlas_occupancy_bps: HIGH_ATLAS_OCCUPANCY_BPS,
            image_atlas_largest_free_rect_pixels: LOW_LARGEST_FREE_RECT_PIXELS,
            ..AppUiFrameMetrics::default()
        };

        assert!(metrics.is_slow_frame());
        assert!(metrics.has_high_upload_pressure());
        assert!(metrics.has_image_atlas_pressure());
        let pressure = AppUiFramePressure::from_metrics(metrics).expect("pressure event");
        assert!(pressure.slow_frame);
        assert!(pressure.high_upload);
        assert!(pressure.image_atlas_pressure);
    }

    #[test]
    fn render_diagnostic_reporter_only_reports_changed_pressure_signatures() {
        let mut reporter = AppUiRenderDiagnosticReporter::default();
        let slow = presented(
            false,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics {
                frame_cpu_time_micros: SLOW_FRAME_CPU_MICROS,
                ..AppUiFrameMetrics::default()
            },
            None,
        );
        let same_slow = presented(
            false,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics {
                frame_cpu_time_micros: SLOW_FRAME_CPU_MICROS + 5_000,
                ..AppUiFrameMetrics::default()
            },
            None,
        );
        let upload_and_slow = presented(
            false,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics {
                frame_cpu_time_micros: SLOW_FRAME_CPU_MICROS + 1,
                total_upload_bytes: HIGH_GPU_UPLOAD_BYTES,
                ..AppUiFrameMetrics::default()
            },
            None,
        );
        let healthy = presented(
            false,
            AppUiFrameDiagnostics::default(),
            AppUiFrameMetrics::default(),
            None,
        );

        assert_eq!(
            reporter.changed_pressure(slow).map(|event| event.slow_frame),
            Some(true)
        );
        assert_eq!(reporter.changed_pressure(same_slow), None);
        let pressure = reporter.changed_pressure(upload_and_slow).expect("new pressure signature");
        assert!(pressure.slow_frame);
        assert!(pressure.high_upload);
        assert_eq!(reporter.changed_pressure(healthy), None);
        assert_eq!(
            reporter.changed_pressure(slow).map(|event| event.slow_frame),
            Some(true)
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
