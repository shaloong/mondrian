//! Desktop platform adapters.
//!
//! Pure platform interfaces live in `mondrian-platform-core`. This crate keeps
//! OS-backed desktop implementations behind those interfaces.

use std::path::{Path, PathBuf};
use std::process::Command;

mod display;
mod global_pointer;
mod memory;
mod playback_scheduling;
mod process_memory;
pub use mondrian_platform_core::{
    ClipboardError, DisplayHdrProbe, DisplayHdrProbeDetails, DisplayHdrProbeResult,
    DisplayIccProfileProbeResult, DisplayProbeBackend, DisplayProfileProbe,
    DisplayProfileProbeTarget, ExecutionMemoryProbe, FileFilter, NoopPlatformService,
    PhysicalMemoryCapacityProbe, PhysicalMemoryCapacityProbeBackend,
    PhysicalMemoryCapacityProbeResult, PlatformService, ProcessMemoryProbe,
    ProcessMemoryProbeBackend, ProcessMemoryProbeResult, ProcessMemoryScope,
    ProcessPrivateMemoryMetric, SystemMemoryProbe, SystemMemoryProbeBackend,
    SystemMemoryProbeResult,
};

/// Default desktop platform implementation.
///
/// Clipboard, native dialogs, and file reveal use cross-platform desktop
/// adapters. Speculative operations are not part of the platform-neutral
/// Interface until a product caller and a typed failure contract exist.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPlatformService;

pub use playback_scheduling::{
    PlaybackThreadScheduling, PlaybackThreadSchedulingError, PlaybackThreadSchedulingStatus,
};

impl PlatformService for SystemPlatformService {
    fn clipboard_copy(&self, text: &str) -> Result<(), ClipboardError> {
        let mut clipboard = arboard::Clipboard::new().map_err(|_| ClipboardError::Unavailable)?;
        clipboard.set_text(text).map_err(|_| ClipboardError::WriteFailed)
    }

    fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
        let mut clipboard = arboard::Clipboard::new().map_err(|_| ClipboardError::Unavailable)?;
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(_) => Err(ClipboardError::ReadFailed),
        }
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

    fn reveal_in_file_manager(&self, path: &Path) {
        reveal_path_in_file_manager(path);
    }
}

impl DisplayProfileProbe for SystemPlatformService {
    fn display_icc_profile(
        &self,
        target: DisplayProfileProbeTarget,
    ) -> DisplayIccProfileProbeResult {
        system_display_icc_profile(target)
    }
}

impl DisplayHdrProbe for SystemPlatformService {
    fn display_hdr_state(&self, target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
        system_display_hdr_state(target)
    }
}

impl ProcessMemoryProbe for SystemPlatformService {
    fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
        process_memory::system_process_memory(scope)
    }
}

impl PhysicalMemoryCapacityProbe for SystemPlatformService {
    fn physical_memory_capacity(&self) -> PhysicalMemoryCapacityProbeResult {
        system_physical_memory_capacity()
    }
}

impl SystemMemoryProbe for SystemPlatformService {
    fn current_system_memory(&self) -> SystemMemoryProbeResult {
        system_memory()
    }
}

fn system_physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
    memory::physical_memory_capacity()
}

fn system_memory() -> SystemMemoryProbeResult {
    memory::system_memory()
}

#[cfg(target_os = "windows")]
fn system_display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    windows_display_profile::display_icc_profile(target)
}

#[cfg(target_os = "macos")]
fn system_display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    display::macos::display_icc_profile(target)
}

#[cfg(target_os = "linux")]
fn system_display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    display::linux::display_icc_profile(target)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn system_display_icc_profile(_target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    DisplayIccProfileProbeResult::unsupported(
        "OS ICC profile discovery is not implemented for this platform",
    )
}

#[cfg(target_os = "windows")]
fn system_display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    windows_display_profile::display_hdr_state(target)
}

#[cfg(target_os = "macos")]
fn system_display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    display::macos::display_hdr_state(target)
}

#[cfg(target_os = "linux")]
fn system_display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    display::linux::display_hdr_state(target)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn system_display_hdr_state(_target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    DisplayHdrProbeResult::unsupported(
        "OS HDR / Advanced Color discovery is not implemented for this platform",
    )
}

fn reveal_path_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let target = if path.exists() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = Command::new("explorer").arg(format!("/select,{}", target.display())).spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let target = if path.exists() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = Command::new("open").arg("-R").arg(target).spawn();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
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

#[cfg(target_os = "windows")]
mod windows_display_profile {
    use std::mem;
    use std::path::PathBuf;
    use std::ptr;

    use mondrian_platform_core::{
        DisplayHdrProbeDetails, DisplayHdrProbeResult, DisplayIccProfileProbeResult,
        DisplayProbeBackend, DisplayProfileProbeTarget,
    };
    use windows_sys::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows_sys::Win32::Foundation::{LPARAM, LUID, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        DISPLAYCONFIG_COLOR_ENCODING_INTENSITY, DISPLAYCONFIG_COLOR_ENCODING_RGB,
        DISPLAYCONFIG_COLOR_ENCODING_YCBCR420, DISPLAYCONFIG_COLOR_ENCODING_YCBCR422,
        DISPLAYCONFIG_COLOR_ENCODING_YCBCR444,
    };
    use windows_sys::Win32::UI::ColorSystem::{
        WcsGetDefaultColorProfile, WcsGetDefaultColorProfileSize, CPST_NONE,
        CPST_RGB_WORKING_SPACE, CPST_STANDARD_DISPLAY_COLOR_MODE, CPT_ICC,
        WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER, WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
    };

    pub fn display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
        let display_device_name = match display_device_name_for_target(target) {
            Ok(Some(name)) => name,
            Ok(None) => {
                return DisplayIccProfileProbeResult::missing(
                    DisplayProbeBackend::WindowsWcs,
                    None,
                    "no Windows monitor matched the winit display rectangle",
                );
            }
            Err(reason) => {
                return DisplayIccProfileProbeResult::failed(
                    DisplayProbeBackend::WindowsWcs,
                    None,
                    reason,
                );
            }
        };

        match default_icc_profile_for_device(&display_device_name) {
            Ok(path) => DisplayIccProfileProbeResult::found_path(
                DisplayProbeBackend::WindowsWcs,
                Some(display_device_name),
                path,
            ),
            Err(reason) => DisplayIccProfileProbeResult::missing(
                DisplayProbeBackend::WindowsWcs,
                Some(display_device_name),
                reason,
            ),
        }
    }

    pub fn display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
        let display_device_name = match display_device_name_for_target(target) {
            Ok(Some(name)) => name,
            Ok(None) => {
                return DisplayHdrProbeResult::missing(
                    DisplayProbeBackend::WindowsDisplayConfig,
                    None,
                    "no Windows monitor matched the winit display rectangle",
                );
            }
            Err(reason) => {
                return DisplayHdrProbeResult::failed(
                    DisplayProbeBackend::WindowsDisplayConfig,
                    None,
                    reason,
                );
            }
        };

        match advanced_color_for_device(&display_device_name) {
            Ok(state) => DisplayHdrProbeResult::found(
                DisplayProbeBackend::WindowsDisplayConfig,
                Some(display_device_name),
                DisplayHdrProbeDetails {
                    hdr_supported: Some(state.advanced_color_supported),
                    hdr_enabled: Some(state.advanced_color_enabled),
                    wide_color_active: Some(state.wide_color_enforced),
                    force_disabled: Some(state.advanced_color_force_disabled),
                    bits_per_color_channel: Some(state.bits_per_color_channel),
                    color_encoding: state.color_encoding,
                    sdr_reference_white_nits: state.sdr_white_level.map(sdr_white_level_to_nits),
                    ..DisplayHdrProbeDetails::default()
                },
            ),
            Err(reason) => DisplayHdrProbeResult::failed(
                DisplayProbeBackend::WindowsDisplayConfig,
                Some(display_device_name),
                reason,
            ),
        }
    }

    fn display_device_name_for_target(
        target: DisplayProfileProbeTarget,
    ) -> Result<Option<String>, String> {
        let mut search = MonitorSearch { target, matched_name: None };
        let ok = unsafe {
            EnumDisplayMonitors(
                ptr::null_mut(),
                ptr::null(),
                Some(enum_monitor_proc),
                &mut search as *mut MonitorSearch as LPARAM,
            )
        };
        if ok == 0 {
            return Err("EnumDisplayMonitors failed".to_owned());
        }
        Ok(search.matched_name)
    }

    struct MonitorSearch {
        target: DisplayProfileProbeTarget,
        matched_name: Option<String>,
    }

    unsafe extern "system" fn enum_monitor_proc(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> windows_sys::core::BOOL {
        let search = &mut *(lparam as *mut MonitorSearch);
        if search.matched_name.is_some() {
            return 0;
        }

        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let ok = GetMonitorInfoW(
            monitor,
            &mut info as *mut MONITORINFOEXW as *mut windows_sys::Win32::Graphics::Gdi::MONITORINFO,
        );
        if ok == 0 {
            return 1;
        }

        if monitor_matches_target(&info.monitorInfo.rcMonitor, search.target) {
            search.matched_name = utf16z_to_string(&info.szDevice);
            return 0;
        }

        1
    }

    fn monitor_matches_target(rect: &RECT, target: DisplayProfileProbeTarget) -> bool {
        let width = rect.right.saturating_sub(rect.left) as u32;
        let height = rect.bottom.saturating_sub(rect.top) as u32;
        let exact = rect.left == target.x
            && rect.top == target.y
            && width == target.width
            && height == target.height;
        if exact {
            return true;
        }

        let center_x = target.x.saturating_add((target.width / 2) as i32);
        let center_y = target.y.saturating_add((target.height / 2) as i32);
        center_x >= rect.left
            && center_x < rect.right
            && center_y >= rect.top
            && center_y < rect.bottom
    }

    fn default_icc_profile_for_device(device_name: &str) -> Result<PathBuf, String> {
        let device_name = wide_null(device_name);
        let scopes = [
            WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
            WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
        ];
        let subtypes = [
            CPST_RGB_WORKING_SPACE,
            CPST_STANDARD_DISPLAY_COLOR_MODE,
            CPST_NONE,
        ];

        let mut failures = Vec::new();
        for scope in scopes {
            for subtype in subtypes {
                match default_icc_profile_for_scope(device_name.as_ptr(), scope, subtype) {
                    Ok(path) => return Ok(resolve_color_profile_path(path)),
                    Err(reason) => failures.push(reason),
                }
            }
        }

        Err(format!(
            "WcsGetDefaultColorProfile did not return a profile ({})",
            failures.join("; ")
        ))
    }

    struct WindowsAdvancedColorState {
        advanced_color_supported: bool,
        advanced_color_enabled: bool,
        wide_color_enforced: bool,
        advanced_color_force_disabled: bool,
        bits_per_color_channel: u32,
        color_encoding: Option<String>,
        sdr_white_level: Option<u32>,
    }

    fn advanced_color_for_device(device_name: &str) -> Result<WindowsAdvancedColorState, String> {
        let paths = active_display_paths()?;
        for path in paths {
            let source_name = source_gdi_device_name(&path)?;
            if !source_name.eq_ignore_ascii_case(device_name) {
                continue;
            }

            let advanced = advanced_color_info(&path)?;
            let advanced_flags = unsafe { advanced.Anonymous.value };
            let sdr_white_level = sdr_white_level(&path).ok();
            return Ok(WindowsAdvancedColorState {
                advanced_color_supported: bit(advanced_flags, 0),
                advanced_color_enabled: bit(advanced_flags, 1),
                wide_color_enforced: bit(advanced_flags, 2),
                advanced_color_force_disabled: bit(advanced_flags, 3),
                bits_per_color_channel: advanced.bitsPerColorChannel,
                color_encoding: Some(color_encoding_name(advanced.colorEncoding).to_owned()),
                sdr_white_level,
            });
        }

        Err(format!(
            "DisplayConfig active paths did not include source device '{device_name}'"
        ))
    }

    fn active_display_paths() -> Result<Vec<DISPLAYCONFIG_PATH_INFO>, String> {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let size_result = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if size_result != 0 {
            return Err(format!(
                "GetDisplayConfigBufferSizes failed with code {size_result}"
            ));
        }
        if path_count == 0 {
            return Err("QueryDisplayConfig reported no active display paths".to_owned());
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let query_result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        if query_result != 0 {
            return Err(format!(
                "QueryDisplayConfig failed with code {query_result}"
            ));
        }
        paths.truncate(path_count as usize);
        Ok(paths)
    }

    fn source_gdi_device_name(path: &DISPLAYCONFIG_PATH_INFO) -> Result<String, String> {
        let mut packet = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: display_config_header(
                DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                path.sourceInfo.adapterId,
                path.sourceInfo.id,
            ),
            ..DISPLAYCONFIG_SOURCE_DEVICE_NAME::default()
        };
        let result = unsafe {
            DisplayConfigGetDeviceInfo(
                &mut packet as *mut DISPLAYCONFIG_SOURCE_DEVICE_NAME
                    as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if result != 0 {
            return Err(format!(
                "DisplayConfigGetDeviceInfo(GET_SOURCE_NAME) failed with code {result}"
            ));
        }
        utf16z_to_string(&packet.viewGdiDeviceName)
            .ok_or_else(|| "DisplayConfig source returned an empty GDI device name".to_owned())
    }

    fn advanced_color_info(
        path: &DISPLAYCONFIG_PATH_INFO,
    ) -> Result<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, String> {
        let mut packet = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: display_config_header(
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
                path.targetInfo.adapterId,
                path.targetInfo.id,
            ),
            ..DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default()
        };
        let result = unsafe {
            DisplayConfigGetDeviceInfo(
                &mut packet as *mut DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO
                    as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if result != 0 {
            return Err(format!(
                "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO) failed with code {result}"
            ));
        }
        Ok(packet)
    }

    fn sdr_white_level(path: &DISPLAYCONFIG_PATH_INFO) -> Result<u32, String> {
        let mut packet = DISPLAYCONFIG_SDR_WHITE_LEVEL {
            header: display_config_header(
                DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
                path.targetInfo.adapterId,
                path.targetInfo.id,
            ),
            ..DISPLAYCONFIG_SDR_WHITE_LEVEL::default()
        };
        let result = unsafe {
            DisplayConfigGetDeviceInfo(
                &mut packet as *mut DISPLAYCONFIG_SDR_WHITE_LEVEL
                    as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if result != 0 {
            return Err(format!(
                "DisplayConfigGetDeviceInfo(GET_SDR_WHITE_LEVEL) failed with code {result}"
            ));
        }
        Ok(packet.SDRWhiteLevel)
    }

    fn sdr_white_level_to_nits(raw_level: u32) -> u32 {
        // DISPLAYCONFIG_SDR_WHITE_LEVEL is an 80-nit multiplier scaled by 1000.
        raw_level.saturating_mul(80).saturating_add(500) / 1000
    }

    fn display_config_header(
        packet_type: i32,
        packet_size: usize,
        adapter_id: LUID,
        id: u32,
    ) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
        DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: packet_type,
            size: packet_size as u32,
            adapterId: adapter_id,
            id,
        }
    }

    fn bit(value: u32, bit_index: u32) -> bool {
        value & (1 << bit_index) != 0
    }

    fn color_encoding_name(encoding: i32) -> &'static str {
        match encoding {
            DISPLAYCONFIG_COLOR_ENCODING_RGB => "Rgb",
            DISPLAYCONFIG_COLOR_ENCODING_YCBCR444 => "YCbCr444",
            DISPLAYCONFIG_COLOR_ENCODING_YCBCR422 => "YCbCr422",
            DISPLAYCONFIG_COLOR_ENCODING_YCBCR420 => "YCbCr420",
            DISPLAYCONFIG_COLOR_ENCODING_INTENSITY => "Intensity",
            _ => "Unknown",
        }
    }

    fn default_icc_profile_for_scope(
        device_name: *const u16,
        scope: i32,
        subtype: i32,
    ) -> Result<PathBuf, String> {
        let mut size = 0u32;
        let size_ok = unsafe {
            WcsGetDefaultColorProfileSize(scope, device_name, CPT_ICC, subtype, 0, &mut size)
        };
        if size_ok == 0 || size == 0 {
            return Err(format!(
                "size query failed for scope={scope} subtype={subtype}"
            ));
        }

        let mut buffer = vec![0u16; size as usize];
        let profile_ok = unsafe {
            WcsGetDefaultColorProfile(
                scope,
                device_name,
                CPT_ICC,
                subtype,
                0,
                size,
                buffer.as_mut_ptr(),
            )
        };
        if profile_ok == 0 {
            return Err(format!(
                "profile query failed for scope={scope} subtype={subtype}"
            ));
        }

        let profile = utf16z_to_string(&buffer)
            .ok_or_else(|| format!("empty profile path for scope={scope} subtype={subtype}"))?;
        Ok(PathBuf::from(profile))
    }

    fn resolve_color_profile_path(path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            return path;
        }

        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .map(|root| {
                root.join("System32").join("spool").join("drivers").join("color").join(&path)
            })
            .unwrap_or(path)
    }

    fn utf16z_to_string(slice: &[u16]) -> Option<String> {
        let len = slice.iter().position(|ch| *ch == 0).unwrap_or(slice.len());
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&slice[..len]))
    }

    fn wide_null(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::sdr_white_level_to_nits;

        #[test]
        fn windows_sdr_white_level_uses_normative_80_nit_scale() {
            assert_eq!(sdr_white_level_to_nits(1_000), 80);
            assert_eq!(sdr_white_level_to_nits(2_000), 160);
            assert_eq!(sdr_white_level_to_nits(2_537), 203);
        }
    }
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
    global_pointer: global_pointer::GlobalPointerSession,
}

impl Default for DesktopEyedropper {
    fn default() -> Self {
        Self {
            active: false,
            snapshot: None,
            preview: DesktopRgba8::BLACK,
            primary_button_down: false,
            global_pointer: global_pointer::GlobalPointerSession::default(),
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
    fn noop_file_reveal_does_not_panic() {
        let svc = NoopPlatformService;
        svc.reveal_in_file_manager(Path::new("/tmp/test.txt"));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // trait object safety
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn platform_service_is_object_safe() {
        let svc: &dyn PlatformService = &NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), Err(ClipboardError::Unavailable));
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_installed_memory_probe_reports_physical_capacity() {
        let result = SystemPlatformService.physical_memory_capacity();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(
            result.backend,
            Some(PhysicalMemoryCapacityProbeBackend::WindowsInstalledSystemMemory)
        );
        assert!(result.installed_physical_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.error.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_process_memory_probe_reports_private_commit() {
        let result = SystemPlatformService.current_process_memory();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(result.scope, ProcessMemoryScope::CurrentProcess);
        assert_eq!(
            result.backend,
            Some(ProcessMemoryProbeBackend::WindowsCurrentProcessStatus)
        );
        assert_eq!(result.observed_process_count, 1);
        assert!(result.inventory_complete);
        assert!(result.private_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.peak_resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.error.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_product_process_tree_probe_reports_complete_inventory() {
        let result = SystemPlatformService.product_process_tree_memory();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(result.scope, ProcessMemoryScope::ProductProcessTree);
        assert_eq!(
            result.backend,
            Some(ProcessMemoryProbeBackend::WindowsToolhelpProcessTree)
        );
        assert!(result.observed_process_count >= 1);
        assert!(result.inventory_complete);
        assert!(result.inventory_attempts >= 1);
        assert!(result.private_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.peak_resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.error.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_system_memory_probe_reports_available_capacity() {
        let result = SystemPlatformService.current_system_memory();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(
            result.backend,
            Some(SystemMemoryProbeBackend::WindowsGlobalMemoryStatus)
        );
        let total = result.total_physical_bytes.expect("Windows reports total physical memory");
        let available = result
            .available_physical_bytes
            .expect("Windows reports available physical memory");
        assert!(total > 0);
        assert!(available <= total);
        assert!(result.memory_load_percent.is_some_and(|load| load <= 100));
        assert!(result.error.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // mock for testing downstream consumers
    // ═══════════════════════════════════════════════════════════════════════════

    /// A test mock that returns controlled values.
    struct MockPlatformService {
        clipboard_content: Result<Option<String>, ClipboardError>,
        file_dialog_result: Option<Vec<PathBuf>>,
        save_dialog_result: Option<PathBuf>,
    }

    impl PlatformService for MockPlatformService {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Ok(())
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
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

        fn reveal_in_file_manager(&self, _path: &Path) {}
    }

    #[test]
    fn mock_platform_service_returns_configured_values() {
        let mock = MockPlatformService {
            clipboard_content: Ok(Some("copied text".into())),
            file_dialog_result: Some(vec![PathBuf::from("/test/file.mp4")]),
            save_dialog_result: Some(PathBuf::from("/test/output.mp4")),
        };

        assert_eq!(mock.clipboard_paste(), Ok(Some("copied text".into())));
        assert_eq!(
            mock.open_file_dialog("", &[]),
            Some(vec![PathBuf::from("/test/file.mp4")])
        );
        assert_eq!(
            mock.save_file_dialog("", "", &[]),
            Some(PathBuf::from("/test/output.mp4"))
        );
    }
}
