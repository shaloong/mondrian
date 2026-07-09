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

/// OS HDR / Advanced Color discovery result for a display target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayHdrProbeResult {
    /// Whether the current platform adapter has an OS HDR discovery mechanism.
    pub discovery_available: bool,
    /// OS display-device identifier used by the platform API, if known.
    pub display_device_name: Option<String>,
    /// Whether the OS reports HDR / Advanced Color support for this display.
    pub advanced_color_supported: Option<bool>,
    /// Whether the OS currently has HDR / Advanced Color enabled for this display.
    pub advanced_color_enabled: Option<bool>,
    /// Whether wide color is enforced by the OS.
    pub wide_color_enforced: Option<bool>,
    /// Whether Advanced Color is force-disabled by the OS or driver policy.
    pub advanced_color_force_disabled: Option<bool>,
    /// Reported output bits per color channel, if available.
    pub bits_per_color_channel: Option<u32>,
    /// OS display color encoding label, if available.
    pub color_encoding: Option<String>,
    /// Windows SDR white level raw value reported by DisplayConfig, if available.
    pub sdr_white_level: Option<u32>,
    /// Structured human-readable failure reason when discovery did not produce
    /// a usable Advanced Color result.
    pub error: Option<String>,
}

/// Native video texture handle family that a platform adapter may be able to
/// import into the renderer backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeVideoTextureHandleKind {
    /// Windows D3D11 `ID3D11Texture2D` decode surface.
    D3D11Texture2D,
    /// macOS/iOS `CVPixelBuffer`/IOSurface-backed decode surface.
    CVPixelBuffer,
    /// Linux DMABUF-exportable VA-API surface.
    DmaBuf,
    /// CUDA/NVDEC device allocation.
    CudaDeviceMemory,
}

impl NativeVideoTextureHandleKind {
    /// Stable handle-kind name for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D11Texture2D => "D3D11Texture2D",
            Self::CVPixelBuffer => "CVPixelBuffer",
            Self::DmaBuf => "DmaBuf",
            Self::CudaDeviceMemory => "CudaDeviceMemory",
        }
    }
}

/// OS/platform capability probe for native video texture import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVideoTextureImportProbeResult {
    /// Whether this platform adapter has a native texture import probe.
    pub discovery_available: bool,
    /// Native handle families the platform adapter can currently import.
    pub supported_handle_kinds: Vec<NativeVideoTextureHandleKind>,
    /// Whether imported textures can remain GPU-resident through renderer use.
    pub zero_copy_supported: bool,
    /// Whether an unsupported path can still use a low-copy staging upload.
    pub low_copy_fallback_supported: bool,
    /// Structured diagnostic reason when import is unavailable or partial.
    pub error: Option<String>,
}

impl NativeVideoTextureImportProbeResult {
    /// Build a successful native texture import probe result.
    pub fn found(
        supported_handle_kinds: Vec<NativeVideoTextureHandleKind>,
        zero_copy_supported: bool,
        low_copy_fallback_supported: bool,
    ) -> Self {
        Self {
            discovery_available: true,
            supported_handle_kinds,
            zero_copy_supported,
            low_copy_fallback_supported,
            error: None,
        }
    }

    /// Build a native texture import probe result with partial readiness.
    ///
    /// Use this when OS/device discovery succeeded but some higher-level piece,
    /// such as zero-copy renderer import, is still unavailable.
    pub fn found_partial(
        supported_handle_kinds: Vec<NativeVideoTextureHandleKind>,
        zero_copy_supported: bool,
        low_copy_fallback_supported: bool,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            discovery_available: true,
            supported_handle_kinds,
            zero_copy_supported,
            low_copy_fallback_supported,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a platform adapter that is present but not ready.
    pub fn missing(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: true,
            supported_handle_kinds: Vec::new(),
            zero_copy_supported: false,
            low_copy_fallback_supported: false,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a platform with no native import adapter.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            supported_handle_kinds: Vec::new(),
            zero_copy_supported: false,
            low_copy_fallback_supported: false,
            error: Some(reason.into()),
        }
    }

    /// Whether the probe reports support for a handle family.
    pub fn supports(&self, handle_kind: NativeVideoTextureHandleKind) -> bool {
        self.supported_handle_kinds.contains(&handle_kind)
    }
}

impl DisplayHdrProbeResult {
    /// Build a successful HDR / Advanced Color probe result.
    #[allow(clippy::too_many_arguments)]
    pub fn found(
        display_device_name: Option<String>,
        advanced_color_supported: bool,
        advanced_color_enabled: bool,
        wide_color_enforced: bool,
        advanced_color_force_disabled: bool,
        bits_per_color_channel: u32,
        color_encoding: Option<String>,
        sdr_white_level: Option<u32>,
    ) -> Self {
        Self {
            discovery_available: true,
            display_device_name,
            advanced_color_supported: Some(advanced_color_supported),
            advanced_color_enabled: Some(advanced_color_enabled),
            wide_color_enforced: Some(wide_color_enforced),
            advanced_color_force_disabled: Some(advanced_color_force_disabled),
            bits_per_color_channel: Some(bits_per_color_channel),
            color_encoding,
            sdr_white_level,
            error: None,
        }
    }

    /// Build a result for a supported probe that could not resolve this display.
    pub fn missing(display_device_name: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            discovery_available: true,
            display_device_name,
            advanced_color_supported: None,
            advanced_color_enabled: None,
            wide_color_enforced: None,
            advanced_color_force_disabled: None,
            bits_per_color_channel: None,
            color_encoding: None,
            sdr_white_level: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a supported probe that failed.
    pub fn failed(display_device_name: Option<String>, reason: impl Into<String>) -> Self {
        Self::missing(display_device_name, reason)
    }

    /// Build a result for a platform with no HDR / Advanced Color discovery adapter.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            display_device_name: None,
            advanced_color_supported: None,
            advanced_color_enabled: None,
            wide_color_enforced: None,
            advanced_color_force_disabled: None,
            bits_per_color_channel: None,
            color_encoding: None,
            sdr_white_level: None,
            error: Some(reason.into()),
        }
    }
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

/// Interface for OS-backed display HDR / Advanced Color probing.
pub trait DisplayHdrProbe: Send + Sync {
    /// Resolve the current display's HDR / Advanced Color state, when the
    /// platform can provide it.
    fn display_hdr_state(&self, target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult;
}

/// Interface for OS-backed native video texture import capability probing.
pub trait NativeVideoTextureImportProbe: Send + Sync {
    /// Resolve whether this platform/backend can import decoder-owned native
    /// video textures into renderer-owned GPU resources.
    fn native_video_texture_import(&self) -> NativeVideoTextureImportProbeResult;
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

impl DisplayHdrProbe for NoopPlatformService {
    fn display_hdr_state(&self, _target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
        DisplayHdrProbeResult::unsupported(
            "OS HDR / Advanced Color discovery unavailable in noop platform adapter",
        )
    }
}

impl NativeVideoTextureImportProbe for NoopPlatformService {
    fn native_video_texture_import(&self) -> NativeVideoTextureImportProbeResult {
        NativeVideoTextureImportProbeResult::unsupported(
            "native video texture import unavailable in noop platform adapter",
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
    fn noop_native_video_texture_import_probe_reports_unsupported() {
        let svc = NoopPlatformService;
        let result = svc.native_video_texture_import();

        assert!(!result.discovery_available);
        assert!(result.supported_handle_kinds.is_empty());
        assert!(!result.zero_copy_supported);
        assert!(!result.low_copy_fallback_supported);
        assert!(result.error.as_deref().unwrap_or_default().contains("noop"));
    }

    #[test]
    fn native_video_texture_import_probe_supports_handle_kinds() {
        let result = NativeVideoTextureImportProbeResult::found(
            vec![
                NativeVideoTextureHandleKind::D3D11Texture2D,
                NativeVideoTextureHandleKind::CVPixelBuffer,
            ],
            true,
            false,
        );

        assert!(result.discovery_available);
        assert!(result.supports(NativeVideoTextureHandleKind::D3D11Texture2D));
        assert!(result.supports(NativeVideoTextureHandleKind::CVPixelBuffer));
        assert!(!result.supports(NativeVideoTextureHandleKind::DmaBuf));
        assert!(result.zero_copy_supported);
        assert!(!result.low_copy_fallback_supported);
        assert_eq!(result.error, None);
    }

    #[test]
    fn native_video_texture_import_probe_preserves_partial_readiness_reason() {
        let result = NativeVideoTextureImportProbeResult::found_partial(
            vec![NativeVideoTextureHandleKind::D3D11Texture2D],
            false,
            true,
            "renderer import not connected",
        );

        assert!(result.discovery_available);
        assert!(result.supports(NativeVideoTextureHandleKind::D3D11Texture2D));
        assert!(!result.zero_copy_supported);
        assert!(result.low_copy_fallback_supported);
        assert_eq!(
            result.error.as_deref(),
            Some("renderer import not connected")
        );
    }

    #[test]
    fn native_video_texture_handle_kind_names_are_stable() {
        assert_eq!(
            NativeVideoTextureHandleKind::D3D11Texture2D.as_str(),
            "D3D11Texture2D"
        );
        assert_eq!(
            NativeVideoTextureHandleKind::CVPixelBuffer.as_str(),
            "CVPixelBuffer"
        );
        assert_eq!(NativeVideoTextureHandleKind::DmaBuf.as_str(), "DmaBuf");
        assert_eq!(
            NativeVideoTextureHandleKind::CudaDeviceMemory.as_str(),
            "CudaDeviceMemory"
        );
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
