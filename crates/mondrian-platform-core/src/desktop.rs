//! Platform-neutral desktop shell service contracts.

use std::path::{Path, PathBuf};

use crate::NoopPlatformService;

/// File filter used by native file dialogs.
#[derive(Debug, Clone)]
pub struct FileFilter {
    /// User-facing filter name.
    pub name: String,
    /// Extensions admitted by the filter, without leading dots.
    pub extensions: Vec<String>,
}

impl FileFilter {
    /// Create one native file-dialog filter.
    pub fn new(name: impl Into<String>, extensions: Vec<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            extensions: extensions.into_iter().map(Into::into).collect(),
        }
    }
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

/// Unified interface for OS-backed desktop shell services.
///
/// UI and business logic depend on this trait instead of directly calling
/// desktop APIs. Runtime crates inject concrete implementations. Display and
/// memory discovery intentionally use independent, narrower probe traits.
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

    /// Reveal a file or folder in the platform file manager.
    fn reveal_in_file_manager(&self, path: &Path);
}

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

    fn reveal_in_file_manager(&self, _path: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_filter_construction_preserves_values() {
        let filter = FileFilter::new("Video Files", vec!["mp4", "mov", "avi"]);
        assert_eq!(filter.name, "Video Files");
        assert_eq!(filter.extensions, vec!["mp4", "mov", "avi"]);

        let owned = FileFilter::new(
            String::from("Images"),
            vec![String::from("png"), String::from("jpg")],
        );
        assert_eq!(owned.extensions, vec!["png", "jpg"]);

        let empty = FileFilter::new("All Files", Vec::<&str>::new());
        assert!(empty.extensions.is_empty());
    }

    #[test]
    fn noop_desktop_service_is_explicit_and_object_safe() {
        let service: &dyn PlatformService = &NoopPlatformService;
        assert_eq!(service.clipboard_paste(), Err(ClipboardError::Unavailable));
        assert_eq!(
            service.clipboard_copy("any text"),
            Err(ClipboardError::Unavailable)
        );
        assert_eq!(service.open_file_dialog("Open", &[]), None);
        assert_eq!(service.save_file_dialog("Save", "test.txt", &[]), None);
        service.reveal_in_file_manager(Path::new("/tmp/test.txt"));

        let _boxed: Box<dyn PlatformService> = Box::new(NoopPlatformService);
    }
}
