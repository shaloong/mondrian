//! 平台抽象层
//!
//! [`PlatformService`] 统一封装操作系统相关功能。
//! 业务代码通过此 trait 调用平台功能，禁止直接调用 OS API。
//!
//! ## 实现
//!
//! * `mondrian-app` 在启动时提供真实的平台实现（基于当前 OS）
//! * 测试中可以使用 mock 实现
//! * Stage A 提供空实现，后续 Stage 逐步添加真实实现

use std::path::{Path, PathBuf};

use mondrian_core::Color;

/// 文件过滤器（用于文件选择对话框）
#[derive(Debug, Clone)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(name: impl Into<String>, extensions: Vec<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            extensions: extensions.into_iter().map(|e| e.into()).collect(),
        }
    }
}

/// 平台服务 —— 所有 OS 交互的统一入口
///
/// ## 设计原则
///
/// * 业务代码通过此 trait 调用平台功能
/// * 不允许直接调用 `std::fs` 之外的 OS API
/// * 测试中可以注入 mock 实现
pub trait PlatformService: Send + Sync {
    // ── 剪贴板 ──────────────────────────────────────────────────────────────

    /// 复制文本到剪贴板
    fn clipboard_copy(&self, text: &str);

    /// 从剪贴板粘贴文本
    fn clipboard_paste(&self) -> Option<String>;

    // ── 文件对话框 ──────────────────────────────────────────────────────────

    /// 打开文件选择对话框
    fn open_file_dialog(&self, title: &str, filters: &[FileFilter]) -> Option<Vec<PathBuf>>;

    /// 保存文件对话框
    fn save_file_dialog(
        &self,
        title: &str,
        default_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf>;

    /// 打开文件夹选择对话框
    fn open_folder_dialog(&self, title: &str) -> Option<PathBuf>;

    // ── 系统交互 ────────────────────────────────────────────────────────────

    /// 在默认浏览器中打开 URL
    fn open_url(&self, url: &str);

    /// 在文件管理器中显示文件
    fn reveal_in_file_manager(&self, path: &Path);

    /// 发送系统通知
    fn send_notification(&self, title: &str, body: &str);
}

/// 空实现 —— Stage A 使用
///
/// 所有方法返回默认值（剪贴板为空，对话框为 None）。
/// 后续 Stage 替换为真实的 OS 实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopPlatformService;

impl PlatformService for NoopPlatformService {
    fn clipboard_copy(&self, _text: &str) {}

    fn clipboard_paste(&self) -> Option<String> {
        None
    }

    fn open_file_dialog(&self, _title: &str, _filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
        None
    }

    fn save_file_dialog(
        &self,
        _title: &str,
        _default_name: &str,
        _filters: &[FileFilter],
    ) -> Option<PathBuf> {
        None
    }

    fn open_folder_dialog(&self, _title: &str) -> Option<PathBuf> {
        None
    }

    fn open_url(&self, _url: &str) {}

    fn reveal_in_file_manager(&self, _path: &Path) {}

    fn send_notification(&self, _title: &str, _body: &str) {}
}

/// Default desktop platform implementation.
///
/// Stage 1 implements clipboard operations. File dialogs, URL opening, file
/// reveal, and notifications intentionally stay as no-ops until their app-shell
/// policies are defined.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPlatformService;

impl PlatformService for SystemPlatformService {
    fn clipboard_copy(&self, text: &str) {
        let _ = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
    }

    fn clipboard_paste(&self) -> Option<String> {
        arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()).ok()
    }

    fn open_file_dialog(&self, title: &str, filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
        configured_file_dialog(title, filters).pick_files()
    }

    fn save_file_dialog(
        &self,
        title: &str,
        default_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf> {
        configured_file_dialog(title, filters).set_file_name(default_name).save_file()
    }

    fn open_folder_dialog(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    fn open_url(&self, _url: &str) {}

    fn reveal_in_file_manager(&self, _path: &Path) {}

    fn send_notification(&self, _title: &str, _body: &str) {}
}

fn configured_file_dialog(title: &str, filters: &[FileFilter]) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    for filter in filters {
        if filter.extensions.is_empty() {
            continue;
        }
        let extensions = filter.extensions.iter().map(String::as_str).collect::<Vec<_>>();
        dialog = dialog.add_filter(&filter.name, &extensions);
    }
    dialog
}

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
#[derive(Debug, Clone)]
pub struct DesktopEyedropper {
    active: bool,
    snapshot: Option<ScreenSnapshot>,
    preview: Color,
    primary_button_down: bool,
}

impl Default for DesktopEyedropper {
    fn default() -> Self {
        Self {
            active: false,
            snapshot: None,
            preview: Color::BLACK,
            primary_button_down: false,
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
    pub fn preview_color(&self) -> Color {
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
        self.preview = self.sample(point).unwrap_or(Color::BLACK);
    }

    /// Begin sampling at the global cursor position, or use `fallback`.
    pub fn begin_at_cursor_or(&mut self, fallback: DesktopPoint) {
        self.begin(global_cursor_position().unwrap_or(fallback));
    }

    /// Cancel sampling and release cached screen data.
    pub fn cancel(&mut self) {
        self.active = false;
        self.snapshot = None;
        self.preview = Color::BLACK;
        self.primary_button_down = false;
    }

    /// Update the preview from a desktop-space point.
    pub fn update_preview(&mut self, point: DesktopPoint) -> Option<Color> {
        if !self.active {
            return None;
        }
        self.preview = self.sample(point).unwrap_or(Color::BLACK);
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
        let point = global_cursor_position()?;
        let _ = self.update_preview(point);
        Some(point)
    }

    /// Finish sampling at a desktop-space point and return the final color.
    pub fn finish_at(&mut self, point: DesktopPoint) -> Color {
        let color = self.sample(point).unwrap_or(Color::BLACK);
        self.cancel();
        color
    }

    /// Poll global pointer state and finish when the primary button is pressed.
    ///
    /// On platforms without a global-pointer adapter this still updates the
    /// internal button latch to `false` and returns `None`; callers should keep
    /// using normal window events as a fallback.
    pub fn poll_global_primary_press(&mut self) -> Option<(DesktopPoint, Color)> {
        if !self.active {
            self.primary_button_down = false;
            return None;
        }

        let Some(point) = global_cursor_position() else {
            self.primary_button_down = false;
            return None;
        };
        let _ = self.update_preview(point);
        let Some(is_down) = global_primary_button_down() else {
            self.primary_button_down = false;
            return None;
        };
        let pressed_now = is_down && !self.primary_button_down;
        self.primary_button_down = is_down;
        pressed_now.then(|| {
            let color = self.finish_at(point);
            (point, color)
        })
    }

    fn sample(&mut self, point: DesktopPoint) -> Option<Color> {
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

    fn sample(&self, point: DesktopPoint) -> Option<Color> {
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
        self.data.get(idx..idx + 4).map(|b| Color {
            r: b[0] as f32 / 255.0,
            g: b[1] as f32 / 255.0,
            b: b[2] as f32 / 255.0,
            a: 1.0,
        })
    }
}

#[cfg(target_os = "windows")]
fn global_cursor_position() -> Option<DesktopPoint> {
    use windows_sys::Win32::Foundation::POINT as WinPoint;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = WinPoint { x: 0, y: 0 };
    let ok = unsafe { GetCursorPos(&mut point) };
    (ok != 0).then_some(DesktopPoint::new(point.x, point.y))
}

#[cfg(not(target_os = "windows"))]
fn global_cursor_position() -> Option<DesktopPoint> {
    None
}

#[cfg(target_os = "windows")]
fn global_primary_button_down() -> Option<bool> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

    let state = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) };
    Some((state & 0x8000u16 as i16) != 0)
}

#[cfg(not(target_os = "windows"))]
fn global_primary_button_down() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════════
    // FileFilter
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn file_filter_new() {
        let filter = FileFilter::new("Video Files", vec!["mp4", "mov", "avi"]);
        assert_eq!(filter.name, "Video Files");
        assert_eq!(filter.extensions, vec!["mp4", "mov", "avi"]);
    }

    #[test]
    fn file_filter_from_string_types() {
        let filter = FileFilter::new(
            String::from("Images"),
            vec![String::from("png"), String::from("jpg")],
        );
        assert_eq!(filter.extensions, vec!["png", "jpg"]);
    }

    #[test]
    fn file_filter_empty_extensions() {
        let filter = FileFilter::new("All Files", Vec::<&str>::new());
        assert!(filter.extensions.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // NoopPlatformService
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn noop_clipboard_paste_returns_none() {
        let svc = NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), None);
    }

    #[test]
    fn noop_clipboard_copy_does_not_panic() {
        let svc = NoopPlatformService;
        svc.clipboard_copy("any text");
    }

    #[test]
    fn noop_open_file_dialog_returns_none() {
        let svc = NoopPlatformService;
        assert_eq!(svc.open_file_dialog("Open", &[]), None);
    }

    #[test]
    fn noop_save_file_dialog_returns_none() {
        let svc = NoopPlatformService;
        assert_eq!(svc.save_file_dialog("Save", "test.txt", &[]), None);
    }

    #[test]
    fn noop_open_folder_dialog_returns_none() {
        let svc = NoopPlatformService;
        assert_eq!(svc.open_folder_dialog("Select Folder"), None);
    }

    #[test]
    fn noop_system_methods_do_not_panic() {
        let svc = NoopPlatformService;
        svc.open_url("https://example.com");
        svc.reveal_in_file_manager(Path::new("/tmp/test.txt"));
        svc.send_notification("Title", "Body");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // trait object safety
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn platform_service_is_object_safe() {
        let svc: &dyn PlatformService = &NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), None);
    }

    #[test]
    fn platform_service_can_be_boxed() {
        let _boxed: Box<dyn PlatformService> = Box::new(NoopPlatformService);
    }

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

        assert_eq!(color, Color::WHITE);
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

    // ═══════════════════════════════════════════════════════════════════════════
    // mock for testing downstream consumers
    // ═══════════════════════════════════════════════════════════════════════════

    /// A test mock that returns controlled values.
    struct MockPlatformService {
        clipboard_content: Option<String>,
        file_dialog_result: Option<Vec<PathBuf>>,
        save_dialog_result: Option<PathBuf>,
        folder_dialog_result: Option<PathBuf>,
    }

    impl PlatformService for MockPlatformService {
        fn clipboard_copy(&self, _text: &str) {}

        fn clipboard_paste(&self) -> Option<String> {
            self.clipboard_content.clone()
        }

        fn open_file_dialog(&self, _title: &str, _filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
            self.file_dialog_result.clone()
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Option<PathBuf> {
            self.save_dialog_result.clone()
        }

        fn open_folder_dialog(&self, _title: &str) -> Option<PathBuf> {
            self.folder_dialog_result.clone()
        }

        fn open_url(&self, _url: &str) {}

        fn reveal_in_file_manager(&self, _path: &Path) {}

        fn send_notification(&self, _title: &str, _body: &str) {}
    }

    #[test]
    fn mock_platform_service_returns_configured_values() {
        let mock = MockPlatformService {
            clipboard_content: Some("copied text".into()),
            file_dialog_result: Some(vec![PathBuf::from("/test/file.mp4")]),
            save_dialog_result: Some(PathBuf::from("/test/output.mp4")),
            folder_dialog_result: Some(PathBuf::from("/test/folder")),
        };

        assert_eq!(mock.clipboard_paste(), Some("copied text".into()));
        assert_eq!(
            mock.open_file_dialog("", &[]),
            Some(vec![PathBuf::from("/test/file.mp4")])
        );
        assert_eq!(
            mock.save_file_dialog("", "", &[]),
            Some(PathBuf::from("/test/output.mp4"))
        );
        assert_eq!(
            mock.open_folder_dialog(""),
            Some(PathBuf::from("/test/folder")),
        );
    }
}
