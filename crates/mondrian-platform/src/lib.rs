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
