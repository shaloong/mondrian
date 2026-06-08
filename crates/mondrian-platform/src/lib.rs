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
