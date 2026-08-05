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

/// Successful completion of one native file-dialog interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDialogOutcome<T> {
    /// The user selected one or more paths.
    Selected(T),
    /// The user dismissed the dialog without selecting a path.
    Cancelled,
}

impl<T> FileDialogOutcome<T> {
    /// Convert a successful dialog outcome to its optional selection.
    pub fn into_selection(self) -> Option<T> {
        match self {
            Self::Selected(selection) => Some(selection),
            Self::Cancelled => None,
        }
    }
}

/// Native file-dialog failure at the platform boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDialogError {
    /// No native dialog adapter is available in this execution context.
    Unavailable,
    /// The native backend failed before it could return a user outcome.
    BackendFailed {
        /// Platform-specific diagnostic that must not be interpreted as policy.
        reason: String,
    },
}

impl std::fmt::Display for FileDialogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("native file dialog is unavailable"),
            Self::BackendFailed { reason } => {
                write!(formatter, "native file dialog failed: {reason}")
            }
        }
    }
}

impl std::error::Error for FileDialogError {}

/// File-manager reveal failure at the platform boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRevealError {
    /// No file-manager adapter is available in this execution context.
    Unavailable,
    /// The platform file-manager process could not be launched.
    LaunchFailed {
        /// Platform-specific diagnostic that must not be interpreted as policy.
        reason: String,
    },
}

impl std::fmt::Display for FileRevealError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("file-manager reveal is unavailable"),
            Self::LaunchFailed { reason } => {
                write!(formatter, "file-manager reveal launch failed: {reason}")
            }
        }
    }
}

impl std::error::Error for FileRevealError {}

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
    fn open_file_dialog(
        &self,
        title: &str,
        filters: &[FileFilter],
    ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError>;

    /// Open a native save-file dialog.
    fn save_file_dialog(
        &self,
        title: &str,
        default_name: &str,
        filters: &[FileFilter],
    ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError>;

    /// Reveal a file or folder in the platform file manager.
    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), FileRevealError>;
}

impl PlatformService for NoopPlatformService {
    fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unavailable)
    }

    fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
        Err(ClipboardError::Unavailable)
    }

    fn open_file_dialog(
        &self,
        _title: &str,
        _filters: &[FileFilter],
    ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
        Err(FileDialogError::Unavailable)
    }

    fn save_file_dialog(
        &self,
        _title: &str,
        _default_name: &str,
        _filters: &[FileFilter],
    ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
        Err(FileDialogError::Unavailable)
    }

    fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
        Err(FileRevealError::Unavailable)
    }
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
        assert_eq!(
            service.open_file_dialog("Open", &[]),
            Err(FileDialogError::Unavailable)
        );
        assert_eq!(
            service.save_file_dialog("Save", "test.txt", &[]),
            Err(FileDialogError::Unavailable)
        );
        assert_eq!(
            service.reveal_in_file_manager(Path::new("/tmp/test.txt")),
            Err(FileRevealError::Unavailable)
        );

        let _boxed: Box<dyn PlatformService> = Box::new(NoopPlatformService);
    }

    #[test]
    fn cancelled_dialog_is_distinct_from_unavailable_adapter() {
        assert_eq!(
            FileDialogOutcome::<PathBuf>::Cancelled.into_selection(),
            None
        );
        assert_eq!(
            NoopPlatformService.open_file_dialog("Open", &[]),
            Err(FileDialogError::Unavailable)
        );
    }
}
