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
    /// Native API or protocol that produced the result.
    pub backend: Option<DisplayProbeBackend>,
    /// OS display-device identifier used by the platform API, if known.
    pub display_device_name: Option<String>,
    /// Resolved ICC/ICM profile path, if the OS reported one.
    pub profile_path: Option<PathBuf>,
    /// ICC payload returned directly by APIs that do not expose a stable path.
    pub profile_bytes: Option<Vec<u8>>,
    /// Structured human-readable failure reason when discovery did not produce
    /// a usable profile path.
    pub error: Option<String>,
}

/// Native backend used to discover display color-management state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayProbeBackend {
    /// Windows Color System default-profile lookup.
    WindowsWcs,
    /// Windows DisplayConfig Advanced Color query.
    WindowsDisplayConfig,
    /// macOS CoreGraphics display color-space query.
    MacOsCoreGraphics,
    /// macOS AppKit Extended Dynamic Range query.
    MacOsAppKit,
    /// Wayland `color-management-v1` output image description.
    WaylandColorManagementV1,
    /// X11 root-window `_ICC_PROFILE` property.
    X11RootProperty,
    /// Linux DRM connector/EDID metadata.
    LinuxDrmSysfs,
}

impl DisplayProbeBackend {
    /// Stable backend label for diagnostics and cache evidence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsWcs => "windows-wcs",
            Self::WindowsDisplayConfig => "windows-display-config",
            Self::MacOsCoreGraphics => "macos-core-graphics",
            Self::MacOsAppKit => "macos-app-kit",
            Self::WaylandColorManagementV1 => "wayland-color-management-v1",
            Self::X11RootProperty => "x11-root-property",
            Self::LinuxDrmSysfs => "linux-drm-sysfs",
        }
    }
}

/// Platform-neutral HDR/EDR evidence reported by a native display API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayHdrProbeDetails {
    /// Whether the physical display and active link support HDR/EDR.
    pub hdr_supported: Option<bool>,
    /// Whether the desktop compositor currently exposes HDR/EDR output.
    pub hdr_enabled: Option<bool>,
    /// Whether the display or active link advertises a wider-than-sRGB gamut.
    pub wide_color_supported: Option<bool>,
    /// Whether the current desktop output encoding is wider than sRGB.
    pub wide_color_active: Option<bool>,
    /// Whether OS or driver policy explicitly disables HDR.
    pub force_disabled: Option<bool>,
    /// Reported output bits per color channel, if available.
    pub bits_per_color_channel: Option<u32>,
    /// Native display color encoding label, if available.
    pub color_encoding: Option<String>,
    /// Active output transfer function such as `PQ` or `HLG`, if reported.
    pub active_transfer_function: Option<String>,
    /// Transfer functions the display/link advertises independently of active mode.
    pub supported_transfer_functions: Vec<String>,
    /// SDR reference white in nits, if reported or normatively derived.
    pub sdr_reference_white_nits: Option<u32>,
    /// Minimum display luminance in milli-nits, if reported.
    pub min_luminance_millinits: Option<u32>,
    /// Maximum display luminance in nits, if reported.
    pub max_luminance_nits: Option<u32>,
    /// EDR headroom currently available, encoded as parts per million.
    pub current_headroom_ppm: Option<u32>,
    /// Maximum EDR headroom potentially available, in parts per million.
    pub potential_headroom_ppm: Option<u32>,
    /// Reference EDR headroom used by the platform, in parts per million.
    pub reference_headroom_ppm: Option<u32>,
}

/// OS HDR / EDR discovery result for a display target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayHdrProbeResult {
    /// Whether the current platform adapter has an OS HDR discovery mechanism.
    pub discovery_available: bool,
    /// Native API or protocol that produced the result.
    pub backend: Option<DisplayProbeBackend>,
    /// OS display-device identifier used by the platform API, if known.
    pub display_device_name: Option<String>,
    /// Platform-neutral HDR/EDR capability and active-state evidence.
    pub details: DisplayHdrProbeDetails,
    /// Structured human-readable failure reason when discovery did not produce
    /// a usable HDR/EDR result.
    pub error: Option<String>,
}

/// Native video texture handle family that a platform adapter may be able to
/// import into the renderer backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeVideoTextureHandleKind {
    /// Windows D3D12 `ID3D12Resource` decode surface.
    D3D12Resource,
    /// Windows D3D11 `ID3D11Texture2D` decode surface.
    D3D11Texture2D,
    /// Legacy Windows DXVA2 `IDirect3DSurface9` decode surface.
    Dxva2Surface,
    /// macOS/iOS `CVPixelBuffer`/IOSurface-backed decode surface.
    CVPixelBuffer,
    /// Linux DMABUF-exportable VA-API surface.
    DmaBuf,
    /// Legacy Linux VDPAU `VdpVideoSurface`.
    VdpauVideoSurface,
    /// CUDA/NVDEC device allocation.
    CudaDeviceMemory,
}

impl NativeVideoTextureHandleKind {
    /// Stable handle-kind name for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D12Resource => "D3D12Resource",
            Self::D3D11Texture2D => "D3D11Texture2D",
            Self::Dxva2Surface => "Dxva2Surface",
            Self::CVPixelBuffer => "CVPixelBuffer",
            Self::DmaBuf => "DmaBuf",
            Self::VdpauVideoSurface => "VdpauVideoSurface",
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
    /// Build a successful HDR / EDR probe result.
    pub fn found(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        details: DisplayHdrProbeDetails,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            display_device_name,
            details,
            error: None,
        }
    }

    /// Build a result for a supported probe that could not resolve this display.
    pub fn missing(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            display_device_name,
            details: DisplayHdrProbeDetails::default(),
            error: Some(reason.into()),
        }
    }

    /// Build a result for a supported probe that failed.
    pub fn failed(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::missing(backend, display_device_name, reason)
    }

    /// Build a result for a platform with no HDR / Advanced Color discovery adapter.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            backend: None,
            display_device_name: None,
            details: DisplayHdrProbeDetails::default(),
            error: Some(reason.into()),
        }
    }

    /// Produce stable, human-readable evidence for display-contract diagnostics.
    pub fn evidence(&self) -> String {
        let backend = self.backend.map(DisplayProbeBackend::as_str).unwrap_or("unavailable");
        let device = self.display_device_name.as_deref().unwrap_or("unknown-display");
        let details = &self.details;
        format!(
            "backend={backend} display={device} supported={:?} enabled={:?} force_disabled={:?} wide_supported={:?} wide_active={:?} active_transfer={:?} supported_transfers={:?} bpc={:?} sdr_white_nits={:?} min_millinits={:?} max_nits={:?} current_headroom_ppm={:?} potential_headroom_ppm={:?}",
            details.hdr_supported,
            details.hdr_enabled,
            details.force_disabled,
            details.wide_color_supported,
            details.wide_color_active,
            details.active_transfer_function,
            details.supported_transfer_functions,
            details.bits_per_color_channel,
            details.sdr_reference_white_nits,
            details.min_luminance_millinits,
            details.max_luminance_nits,
            details.current_headroom_ppm,
            details.potential_headroom_ppm,
        )
    }
}

impl DisplayIccProfileProbeResult {
    /// Build a successful path-backed ICC profile probe result.
    pub fn found_path(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        profile_path: PathBuf,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            display_device_name,
            profile_path: Some(profile_path),
            profile_bytes: None,
            error: None,
        }
    }

    /// Build a successful in-memory ICC profile probe result.
    pub fn found_bytes(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        profile_bytes: Vec<u8>,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            display_device_name,
            profile_path: None,
            profile_bytes: Some(profile_bytes),
            error: None,
        }
    }

    /// Build a result for a supported probe that found no default profile.
    pub fn missing(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            display_device_name,
            profile_path: None,
            profile_bytes: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a supported probe that failed.
    pub fn failed(
        backend: DisplayProbeBackend,
        display_device_name: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::missing(backend, display_device_name, reason)
    }

    /// Build a result for a platform with no ICC profile discovery adapter.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            backend: None,
            display_device_name: None,
            profile_path: None,
            profile_bytes: None,
            error: Some(reason.into()),
        }
    }

    /// Stable source reference for diagnostics when no filesystem path exists.
    pub fn source_reference(&self) -> Option<String> {
        if let Some(path) = &self.profile_path {
            return Some(path.display().to_string());
        }
        self.backend.map(|backend| {
            format!(
                "{}:{}",
                backend.as_str(),
                self.display_device_name.as_deref().unwrap_or("unknown-display")
            )
        })
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
        assert!(result.profile_bytes.is_none());
    }

    #[test]
    fn in_memory_icc_profile_keeps_backend_identity() {
        let result = DisplayIccProfileProbeResult::found_bytes(
            DisplayProbeBackend::MacOsCoreGraphics,
            Some("Studio Display".to_owned()),
            vec![1, 2, 3],
        );

        assert_eq!(result.profile_bytes.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(
            result.source_reference().as_deref(),
            Some("macos-core-graphics:Studio Display")
        );
    }

    #[test]
    fn hdr_evidence_preserves_unknown_active_state() {
        let result = DisplayHdrProbeResult::found(
            DisplayProbeBackend::LinuxDrmSysfs,
            Some("card0-HDMI-A-1".to_owned()),
            DisplayHdrProbeDetails {
                hdr_supported: Some(true),
                hdr_enabled: None,
                max_luminance_nits: Some(1000),
                ..DisplayHdrProbeDetails::default()
            },
        );

        assert_eq!(result.details.hdr_supported, Some(true));
        assert_eq!(result.details.hdr_enabled, None);
        assert!(result.evidence().contains("enabled=None"));
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
                NativeVideoTextureHandleKind::D3D12Resource,
                NativeVideoTextureHandleKind::D3D11Texture2D,
                NativeVideoTextureHandleKind::CVPixelBuffer,
            ],
            true,
            false,
        );

        assert!(result.discovery_available);
        assert!(result.supports(NativeVideoTextureHandleKind::D3D12Resource));
        assert!(result.supports(NativeVideoTextureHandleKind::D3D11Texture2D));
        assert!(result.supports(NativeVideoTextureHandleKind::CVPixelBuffer));
        assert!(!result.supports(NativeVideoTextureHandleKind::DmaBuf));
        assert!(!result.supports(NativeVideoTextureHandleKind::VdpauVideoSurface));
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
            NativeVideoTextureHandleKind::D3D12Resource.as_str(),
            "D3D12Resource"
        );
        assert_eq!(
            NativeVideoTextureHandleKind::D3D11Texture2D.as_str(),
            "D3D11Texture2D"
        );
        assert_eq!(
            NativeVideoTextureHandleKind::Dxva2Surface.as_str(),
            "Dxva2Surface"
        );
        assert_eq!(
            NativeVideoTextureHandleKind::CVPixelBuffer.as_str(),
            "CVPixelBuffer"
        );
        assert_eq!(NativeVideoTextureHandleKind::DmaBuf.as_str(), "DmaBuf");
        assert_eq!(
            NativeVideoTextureHandleKind::VdpauVideoSurface.as_str(),
            "VdpauVideoSurface"
        );
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
