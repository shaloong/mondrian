//! Desktop capture and global-pointer coordination for the eyedropper.
//!
//! This module owns composed-desktop sampling and session state. It does not
//! interpret sampled display code values as Project or working-space color.

/// Desktop-space pixel coordinate.
///
/// This uses the operating system's virtual desktop coordinate space, not a
/// window-local coordinate space. Multi-monitor setups may produce negative
/// coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPoint {
    /// Horizontal desktop coordinate in physical pixels.
    pub x: i32,
    /// Vertical desktop coordinate in physical pixels.
    pub y: i32,
}

/// One byte-exact color sampled from the composed desktop image.
///
/// This is display-referred platform evidence, not a Project or working-space
/// color. The App Adapter decides how to present or author it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopRgba8 {
    /// Red display code value.
    pub r: u8,
    /// Green display code value.
    pub g: u8,
    /// Blue display code value.
    pub b: u8,
    /// Alpha code value supplied by the capture Adapter.
    pub a: u8,
}

impl DesktopRgba8 {
    /// Opaque black used when desktop capture cannot provide a sample.
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0, a: u8::MAX };
    /// Opaque white.
    #[cfg(test)]
    const WHITE: Self = Self { r: u8::MAX, g: u8::MAX, b: u8::MAX, a: u8::MAX };
}

impl DesktopPoint {
    /// Create a desktop-space point.
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Platform eyedropper session for screen color sampling.
///
/// The UI layer owns widget state and sends an eyedropper request. The app shell
/// owns window-local coordinate conversion. This module owns the platform work:
/// screen capture, desktop-coordinate sampling, and best-effort global pointer
/// polling.
#[derive(Debug)]
pub struct DesktopEyedropper {
    active: bool,
    snapshot: Option<ScreenSnapshot>,
    preview: DesktopRgba8,
    primary_button_down: bool,
    global_pointer: crate::global_pointer::GlobalPointerSession,
}

impl Default for DesktopEyedropper {
    fn default() -> Self {
        Self {
            active: false,
            snapshot: None,
            preview: DesktopRgba8::BLACK,
            primary_button_down: false,
            global_pointer: crate::global_pointer::GlobalPointerSession::default(),
        }
    }
}

impl DesktopEyedropper {
    /// Create an inactive eyedropper session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a desktop sampling session is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Last sampled preview color.
    pub fn preview_color(&self) -> DesktopRgba8 {
        self.preview
    }

    /// Begin sampling at a desktop-space point.
    ///
    /// The session captures the monitor containing `point` and samples from the
    /// cached image until the pointer moves outside that monitor, at which point
    /// it captures the new monitor.
    pub fn begin(&mut self, point: DesktopPoint) {
        self.active = true;
        self.primary_button_down = false;
        self.snapshot = ScreenSnapshot::capture_at(point);
        self.preview = self.sample(point).unwrap_or(DesktopRgba8::BLACK);
    }

    /// Begin sampling at the global cursor position, or use `fallback`.
    pub fn begin_at_cursor_or(&mut self, fallback: DesktopPoint) {
        self.begin(
            self.global_pointer
                .snapshot()
                .map(|snapshot| snapshot.position)
                .unwrap_or(fallback),
        );
    }

    /// Cancel sampling and release cached screen data.
    pub fn cancel(&mut self) {
        self.active = false;
        self.snapshot = None;
        self.preview = DesktopRgba8::BLACK;
        self.primary_button_down = false;
    }

    /// Update the preview from a desktop-space point.
    pub fn update_preview(&mut self, point: DesktopPoint) -> Option<DesktopRgba8> {
        if !self.active {
            return None;
        }
        self.preview = self.sample(point).unwrap_or(DesktopRgba8::BLACK);
        Some(self.preview)
    }

    /// Poll the global cursor and update the preview.
    ///
    /// Returns `None` when inactive or when the current platform adapter cannot
    /// read the global cursor position.
    pub fn poll_global_cursor(&mut self) -> Option<DesktopPoint> {
        if !self.active {
            return None;
        }
        let point = self.global_pointer.snapshot()?.position;
        let _ = self.update_preview(point);
        Some(point)
    }

    /// Finish sampling at a desktop-space point and return the final color.
    pub fn finish_at(&mut self, point: DesktopPoint) -> DesktopRgba8 {
        let color = self.sample(point).unwrap_or(DesktopRgba8::BLACK);
        self.cancel();
        color
    }

    /// Poll global pointer state and finish when the primary button is pressed.
    ///
    /// On platforms without a global-pointer adapter this still updates the
    /// internal button latch to `false` and returns `None`; callers should keep
    /// using normal window events as a fallback.
    pub fn poll_global_primary_press(&mut self) -> Option<(DesktopPoint, DesktopRgba8)> {
        if !self.active {
            self.primary_button_down = false;
            return None;
        }

        let Some(snapshot) = self.global_pointer.snapshot() else {
            self.primary_button_down = false;
            return None;
        };
        let point = snapshot.position;
        let _ = self.update_preview(point);
        let is_down = snapshot.primary_button_down;
        let pressed_now = is_down && !self.primary_button_down;
        self.primary_button_down = is_down;
        pressed_now.then(|| {
            let color = self.finish_at(point);
            (point, color)
        })
    }

    fn sample(&mut self, point: DesktopPoint) -> Option<DesktopRgba8> {
        if let Some(color) = self.snapshot.as_ref().and_then(|snapshot| snapshot.sample(point)) {
            return Some(color);
        }
        self.snapshot = ScreenSnapshot::capture_at(point);
        self.snapshot.as_ref().and_then(|snapshot| snapshot.sample(point))
    }
}

#[derive(Debug, Clone)]
struct ScreenSnapshot {
    data: Vec<u8>,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
}

impl ScreenSnapshot {
    fn capture_at(point: DesktopPoint) -> Option<Self> {
        let monitor = xcap::Monitor::from_point(point.x, point.y).ok()?;
        let img = monitor.capture_image().ok()?;
        Some(Self {
            width: img.width(),
            height: img.height(),
            data: img.into_raw(),
            x: monitor.x(),
            y: monitor.y(),
        })
    }

    fn sample(&self, point: DesktopPoint) -> Option<DesktopRgba8> {
        let ux = point.x.checked_sub(self.x)?;
        let uy = point.y.checked_sub(self.y)?;
        if ux < 0 || uy < 0 {
            return None;
        }
        let ux = ux as u32;
        let uy = uy as u32;
        if ux >= self.width || uy >= self.height {
            return None;
        }
        let idx = (uy as usize * self.width as usize + ux as usize) * 4;
        self.data
            .get(idx..idx + 4)
            .map(|b| DesktopRgba8 { r: b[0], g: b[1], b: b[2], a: b[3] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_snapshot_samples_desktop_coordinates() {
        let snapshot = ScreenSnapshot {
            width: 2,
            height: 2,
            x: -10,
            y: 20,
            data: vec![
                255, 0, 0, 255, // (-10, 20)
                0, 255, 0, 255, // (-9, 20)
                0, 0, 255, 255, // (-10, 21)
                255, 255, 255, 255, // (-9, 21)
            ],
        };

        let color = snapshot
            .sample(DesktopPoint::new(-9, 21))
            .expect("point should be inside snapshot");

        assert_eq!(color, DesktopRgba8::WHITE);
    }

    #[test]
    fn screen_snapshot_rejects_points_outside_bounds() {
        let snapshot = ScreenSnapshot {
            width: 2,
            height: 2,
            x: -10,
            y: 20,
            data: vec![0; 16],
        };

        assert_eq!(snapshot.sample(DesktopPoint::new(-11, 20)), None);
        assert_eq!(snapshot.sample(DesktopPoint::new(-8, 20)), None);
        assert_eq!(snapshot.sample(DesktopPoint::new(-10, 19)), None);
        assert_eq!(snapshot.sample(DesktopPoint::new(-10, 22)), None);
    }
}
