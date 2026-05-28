//! Canvas coordinate system — unified sequence-space to screen-space mapping.
//!
//! All canvas elements (preview image, mask overlays, transform handles,
//! safe margins) use `CanvasTransform` for coordinate conversion.

use egui::{Pos2, Rect, Vec2};

/// The zoom mode for the viewer canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasZoomMode {
    /// Fit the entire sequence frame within the canvas, centered (letterboxed).
    Fit,
    /// Fixed zoom level: 1.0 = 1:1 pixels, 2.0 = 200%, etc.
    Fixed(f32),
}

impl CanvasZoomMode {
    pub fn effective_zoom(&self, seq_size: (u32, u32), canvas_rect: Rect) -> f32 {
        match self {
            CanvasZoomMode::Fit => {
                let cw = canvas_rect.width();
                let ch = canvas_rect.height();
                if cw <= 0.0 || ch <= 0.0 || seq_size.0 == 0 || seq_size.1 == 0 {
                    return 1.0;
                }
                let sx = cw / seq_size.0 as f32;
                let sy = ch / seq_size.1 as f32;
                sx.min(sy)
            }
            CanvasZoomMode::Fixed(zoom) => zoom.clamp(0.01, 32.0),
        }
    }

    pub fn display_label(&self) -> String {
        match self {
            CanvasZoomMode::Fit => "Fit".to_string(),
            CanvasZoomMode::Fixed(z) => format!("{:.0}%", z * 100.0),
        }
    }
}

/// Unified coordinate transform between sequence pixel space and screen space.
///
/// Sequence space: origin at top-left, x right, y down, units = pixels
/// Screen space: egui canvas coordinates on screen
#[derive(Debug, Clone)]
pub struct CanvasTransform {
    /// Sequence resolution (constant for a given sequence).
    pub seq_size: (u32, u32),
    /// The canvas rect on screen.
    pub canvas_rect: Rect,
    /// Current zoom mode.
    pub zoom_mode: CanvasZoomMode,
    /// Pan offset in sequence-pixel space, relative to centered frame.
    pub pan: Vec2,
    /// Background color shown in letterbox/pillarbox areas.
    pub background: egui::Color32,
}

impl CanvasTransform {
    /// Create a new transform in Fit mode with a default background.
    pub fn fit(seq_size: (u32, u32), canvas_rect: Rect) -> Self {
        Self {
            seq_size,
            canvas_rect,
            zoom_mode: CanvasZoomMode::Fit,
            pan: Vec2::ZERO,
            background: egui::Color32::from_rgb(0x2a, 0x2a, 0x2a),
        }
    }

    /// The effective zoom factor (sequence pixels → screen pixels).
    pub fn zoom(&self) -> f32 {
        self.zoom_mode.effective_zoom(self.seq_size, self.canvas_rect)
    }

    /// The rendered content rect within canvas_rect (excludes letterbox/pillarbox).
    pub fn content_rect(&self) -> Rect {
        let zoom = self.zoom();
        let cw = self.seq_size.0 as f32 * zoom;
        let ch = self.seq_size.1 as f32 * zoom;
        let cx = self.canvas_rect.center().x + self.pan.x * zoom;
        let cy = self.canvas_rect.center().y + self.pan.y * zoom;
        let left = cx - cw * 0.5;
        let top = cy - ch * 0.5;
        Rect::from_min_size(Pos2::new(left, top), Vec2::new(cw, ch))
    }

    /// Convert sequence pixel coordinates to screen coordinates.
    pub fn seq_to_screen(&self, x: f32, y: f32) -> Pos2 {
        let zoom = self.zoom();
        let content = self.content_rect();
        Pos2::new(content.left() + x * zoom, content.top() + y * zoom)
    }

    /// Convert screen coordinates to sequence pixel coordinates.
    /// Returns None if the point is outside the content rect.
    pub fn screen_to_seq(&self, pos: Pos2) -> Option<(f32, f32)> {
        let content = self.content_rect();
        let zoom = self.zoom();
        if zoom <= 0.0 {
            return None;
        }
        let sx = (pos.x - content.left()) / zoom;
        let sy = (pos.y - content.top()) / zoom;
        Some((sx, sy))
    }

    /// Check whether a screen point lies within the content area.
    pub fn contains_screen_point(&self, pos: Pos2) -> bool {
        self.content_rect().contains(pos)
    }

    /// Pan by a screen-space delta.
    pub fn pan_by_screen(&mut self, delta: Vec2) {
        let zoom = self.zoom();
        if zoom > 0.0 {
            self.pan += delta / zoom;
        }
        self.clamp_pan();
    }

    /// Zoom by a factor, centered on a screen-space point.
    /// The seq-space point under the cursor stays fixed on screen.
    pub fn zoom_at_screen(&mut self, center: Pos2, factor: f32) {
        let seq_under_center = self.screen_to_seq(center);
        let old_zoom = self.zoom();
        let new_zoom = (old_zoom * factor).clamp(0.01, 32.0);
        self.zoom_mode = CanvasZoomMode::Fixed(new_zoom);

        let canvas_cx = self.canvas_rect.center().x;
        let canvas_cy = self.canvas_rect.center().y;
        if let Some((sx, sy)) = seq_under_center {
            self.pan.x = (center.x - canvas_cx) / new_zoom + self.seq_size.0 as f32 / 2.0 - sx;
            self.pan.y = (center.y - canvas_cy) / new_zoom + self.seq_size.1 as f32 / 2.0 - sy;
        }
        self.clamp_pan();
    }

    /// Set the zoom mode and recenter.
    pub fn set_zoom_mode(&mut self, mode: CanvasZoomMode) {
        self.zoom_mode = mode;
        self.pan = Vec2::ZERO;
    }

    /// Clamp pan so at least 25% of the content remains visible.
    fn clamp_pan(&mut self) {
        let zoom = self.zoom();
        if zoom <= 0.0 {
            return;
        }
        let cw = self.canvas_rect.width();
        let ch = self.canvas_rect.height();
        let content_w = self.seq_size.0 as f32 * zoom;
        let content_h = self.seq_size.1 as f32 * zoom;
        // Maximum pan before content is completely off-screen.
        let max_px = (content_w * 0.75 + cw * 0.5) / zoom;
        let max_py = (content_h * 0.75 + ch * 0.5) / zoom;
        self.pan.x = self.pan.x.clamp(-max_px, max_px);
        self.pan.y = self.pan.y.clamp(-max_py, max_py);
    }

    /// Update the canvas rect (e.g. on window resize) and re-apply current zoom mode.
    pub fn update_canvas_rect(&mut self, new_rect: Rect) {
        self.canvas_rect = new_rect;
    }

    /// Is the viewer in Fit mode?
    pub fn is_fit(&self) -> bool {
        matches!(self.zoom_mode, CanvasZoomMode::Fit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ct() -> CanvasTransform {
        CanvasTransform::fit(
            (1920, 1080),
            Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(960.0, 540.0)),
        )
    }

    #[test]
    fn fit_zoom_is_correct() {
        let ct = make_ct();
        // 960/1920 = 0.5, 540/1080 = 0.5 → fit = 0.5
        assert!((ct.zoom() - 0.5).abs() < 1e-5);
    }

    #[test]
    fn content_rect_centered_in_fit() {
        let ct = make_ct();
        let content = ct.content_rect();
        // 1920 * 0.5 = 960, 1080 * 0.5 = 540
        assert!((content.width() - 960.0).abs() < 1e-5);
        assert!((content.height() - 540.0).abs() < 1e-5);
        // Top-left should be at canvas origin since canvas is exactly content-sized.
        assert!((content.left() - 0.0).abs() < 1e-5);
        assert!((content.top() - 0.0).abs() < 1e-5);
    }

    #[test]
    fn seq_to_screen_maps_origin() {
        let ct = make_ct();
        let p = ct.seq_to_screen(0.0, 0.0);
        assert!(p.x.abs() < 1e-5);
        assert!(p.y.abs() < 1e-5);
    }

    #[test]
    fn seq_to_screen_maps_center() {
        let ct = make_ct();
        let p = ct.seq_to_screen(960.0, 540.0);
        assert!((p.x - 480.0).abs() < 1e-5, "got x={}", p.x);
        assert!((p.y - 270.0).abs() < 1e-5, "got y={}", p.y);
    }

    #[test]
    fn screen_to_seq_roundtrip() {
        let ct = make_ct();
        let (sx, sy) = ct.screen_to_seq(Pos2::new(240.0, 135.0)).unwrap();
        assert!((sx - 480.0).abs() < 1e-5);
        assert!((sy - 270.0).abs() < 1e-5);
    }

    #[test]
    fn zoom_at_preserves_center_point() {
        let mut ct = make_ct();
        let center = ct.canvas_rect.center();
        let (sx0, sy0) = ct.screen_to_seq(center).unwrap();
        assert!((sx0 - 960.0).abs() < 1.0);
        assert!((sy0 - 540.0).abs() < 1.0);

        ct.zoom_at_screen(center, 2.0);
        let (sx1, sy1) = ct.screen_to_seq(center).unwrap();
        // The seq point under the cursor should stay the same.
        assert!((sx1 - sx0).abs() < 1.0, "sx shifted from {sx0} to {sx1}");
        assert!((sy1 - sy0).abs() < 1.0, "sy shifted from {sy0} to {sy1}");
    }

    #[test]
    fn fixed_50pct_same_as_fit_for_full_hd_in_hd_slot() {
        // 1920x1080 seq in 960x540 slot: fit = Fixed(0.5) = 0.5 zoom
        let slot = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(960.0, 540.0));
        let mut ct = CanvasTransform::fit((1920, 1080), slot);
        let fit_rect = ct.content_rect();

        ct.set_zoom_mode(CanvasZoomMode::Fixed(0.5));
        let fixed_rect = ct.content_rect();

        assert!(
            (fit_rect.left() - fixed_rect.left()).abs() < 1.0,
            "fit left={}, fixed left={}",
            fit_rect.left(),
            fixed_rect.left()
        );
        assert!(
            (fit_rect.top() - fixed_rect.top()).abs() < 1.0,
            "fit top={}, fixed top={}",
            fit_rect.top(),
            fixed_rect.top()
        );
        assert!((fit_rect.width() - fixed_rect.width()).abs() < 1.0);
        assert!((fit_rect.height() - fixed_rect.height()).abs() < 1.0);
    }

    #[test]
    fn zoom_10pct_centered_in_slot() {
        let slot = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(960.0, 540.0));
        let mut ct = CanvasTransform::fit((1920, 1080), slot);
        ct.set_zoom_mode(CanvasZoomMode::Fixed(0.1));
        let r = ct.content_rect();
        let slot_c = slot.center();
        assert!(
            (r.center().x - slot_c.x).abs() < 1.0,
            "10% content not horizontally centered: center.x={}",
            r.center().x
        );
        assert!(
            (r.center().y - slot_c.y).abs() < 1.0,
            "10% content not vertically centered: center.y={}",
            r.center().y
        );
    }

    #[test]
    fn pan_by_screen_shifts_view() {
        let mut ct = make_ct();
        let before = ct.seq_to_screen(0.0, 0.0);
        ct.pan_by_screen(Vec2::new(100.0, 50.0));
        let after = ct.seq_to_screen(0.0, 0.0);
        // Pan moves view: origin should shift on screen.
        assert!((after.x - before.x - 100.0).abs() < 1.0);
        assert!((after.y - before.y - 50.0).abs() < 1.0);
    }
}
