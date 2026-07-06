//! Platform boundary interfaces shared by UI and desktop adapters.
//!
//! This crate intentionally contains no operating-system implementation. UI
//! crates depend on this interface crate, while desktop shells provide concrete
//! adapters from `mondrian-platform`.

use std::path::{Path, PathBuf};

/// File filter used by native file dialogs.
#[derive(Debug, Clone)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(name: impl Into<String>, extensions: Vec<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            extensions: extensions.into_iter().map(|extension| extension.into()).collect(),
        }
    }
}

/// Display target used for OS display-profile probing.
///
/// Coordinates and size are in the operating system's virtual desktop physical
/// pixel space. Multi-monitor setups may report negative coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayProfileProbeTarget {
    /// Left edge in virtual desktop physical pixels.
    pub x: i32,
    /// Top edge in virtual desktop physical pixels.
    pub y: i32,
    /// Physical monitor width in pixels.
    pub width: u32,
    /// Physical monitor height in pixels.
    pub height: u32,
}

impl DisplayProfileProbeTarget {
    /// Create a display-profile probe target from a monitor rectangle.
    pub fn new(position: (i32, i32), physical_size: (u32, u32)) -> Self {
        Self {
            x: position.0,
            y: position.1,
            width: physical_size.0,
            height: physical_size.1,
        }
    }
}

/// OS ICC profile discovery result for a display target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayIccProfileProbeResult {
    /// Whether the current platform adapter has an OS ICC discovery mechanism.
    pub discovery_available: bool,
    /// OS display-device identifier used by the platform API, if known.
    pub display_device_name: Option<String>,
    /// Resolved ICC/ICM profile path, if the OS reported one.
    pub profile_path: Option<PathBuf>,
    /// Structured human-readable failure reason when discovery did not produce
    /// a usable profile path.
    pub error: Option<String>,
}

impl DisplayIccProfileProbeResult {
    /// Build a successful ICC profile probe result.
    pub fn found(display_device_name: Option<String>, profile_path: PathBuf) -> Self {
        Self {
            discovery_available: true,
            display_device_name,
            profile_path: Some(profile_path),
            error: None,
        }
    }

    /// Build a result for a supported probe that found no default profile.
    pub fn missing(display_device_name: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            discovery_available: true,
            display_device_name,
            profile_path: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a supported probe that failed.
    pub fn failed(display_device_name: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            discovery_available: true,
            display_device_name,
            profile_path: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a platform with no ICC profile discovery adapter.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            display_device_name: None,
            profile_path: None,
            error: Some(reason.into()),
        }
    }
}

/// Interface for OS-backed display profile probing.
pub trait DisplayProfileProbe: Send + Sync {
    /// Resolve the current display's default ICC profile, when the platform can
    /// provide one.
    fn display_icc_profile(
        &self,
        target: DisplayProfileProbeTarget,
    ) -> DisplayIccProfileProbeResult;
}

/// Clipboard operation failure reported by the platform boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardError {
    /// The current platform or process context cannot access a clipboard.
    Unavailable,
    /// Text could not be written to the clipboard.
    WriteFailed,
    /// Text could not be read from the clipboard.
    ReadFailed,
}

/// Unified interface for OS-backed platform services.
///
/// UI and business logic depend on this trait instead of directly calling
/// desktop APIs. Runtime crates inject concrete implementations.
pub trait PlatformService: Send + Sync {
    /// Copy text to the platform clipboard.
    fn clipboard_copy(&self, text: &str) -> Result<(), ClipboardError>;

    /// Read text from the platform clipboard.
    ///
    /// `Ok(None)` means the clipboard is accessible but currently has no text.
    /// `Err` means platform access itself failed.
    fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError>;

    /// Open a native file picker.
    fn open_file_dialog(&self, title: &str, filters: &[FileFilter]) -> Option<Vec<PathBuf>>;

    /// Open a native save-file dialog.
    fn save_file_dialog(
        &self,
        title: &str,
        default_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf>;

    /// Open a native folder picker.
    fn open_folder_dialog(&self, title: &str) -> Option<PathBuf>;

    /// Open a URL in the default browser.
    fn open_url(&self, url: &str);

    /// Reveal a file or folder in the platform file manager.
    fn reveal_in_file_manager(&self, path: &Path);

    /// Send a platform notification.
    fn send_notification(&self, title: &str, body: &str);
}

/// Empty platform implementation for tests and headless UI execution.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopPlatformService;

impl PlatformService for NoopPlatformService {
    fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unavailable)
    }

    fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
        Err(ClipboardError::Unavailable)
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

impl DisplayProfileProbe for NoopPlatformService {
    fn display_icc_profile(
        &self,
        _target: DisplayProfileProbeTarget,
    ) -> DisplayIccProfileProbeResult {
        DisplayIccProfileProbeResult::unsupported(
            "OS ICC profile discovery unavailable in noop platform adapter",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn noop_clipboard_paste_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), Err(ClipboardError::Unavailable));
    }

    #[test]
    fn noop_clipboard_copy_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(
            svc.clipboard_copy("any text"),
            Err(ClipboardError::Unavailable)
        );
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
    fn noop_display_profile_probe_reports_unsupported() {
        let svc = NoopPlatformService;
        let result = svc.display_icc_profile(DisplayProfileProbeTarget::new((0, 0), (1920, 1080)));
        assert!(!result.discovery_available);
        assert!(result.profile_path.is_none());
    }

    #[test]
    fn noop_system_methods_do_not_panic() {
        let svc = NoopPlatformService;
        svc.open_url("https://example.com");
        svc.reveal_in_file_manager(Path::new("/tmp/test.txt"));
        svc.send_notification("Title", "Body");
    }

    #[test]
    fn platform_service_is_object_safe() {
        let svc: &dyn PlatformService = &NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), Err(ClipboardError::Unavailable));
    }

    #[test]
    fn platform_service_can_be_boxed() {
        let _boxed: Box<dyn PlatformService> = Box::new(NoopPlatformService);
    }
}
