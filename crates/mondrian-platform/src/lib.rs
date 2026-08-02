//! Desktop platform adapters.
//!
//! Pure platform interfaces live in `mondrian-platform-core`. This crate keeps
//! OS-backed desktop implementations behind those interfaces.

use std::path::{Path, PathBuf};
use std::process::Command;

use mondrian_core::Color;
mod display;
mod process_memory;
pub use mondrian_platform_core::{
    ClipboardError, DisplayHdrProbe, DisplayHdrProbeDetails, DisplayHdrProbeResult,
    DisplayIccProfileProbeResult, DisplayProbeBackend, DisplayProfileProbe,
    DisplayProfileProbeTarget, ExecutionMemoryProbe, FileFilter, NativeVideoTextureHandleKind,
    NativeVideoTextureImportProbe, NativeVideoTextureImportProbeResult, NoopPlatformService,
    PhysicalMemoryCapacityProbe, PhysicalMemoryCapacityProbeBackend,
    PhysicalMemoryCapacityProbeResult, PlatformService, ProcessMemoryProbe,
    ProcessMemoryProbeBackend, ProcessMemoryProbeResult, ProcessMemoryScope, SystemMemoryProbe,
    SystemMemoryProbeBackend, SystemMemoryProbeResult,
};

/// Default desktop platform implementation.
///
/// Stage 1 implements clipboard operations. File dialogs, URL opening, file
/// reveal, and notifications intentionally stay as no-ops until their app-shell
/// policies are defined.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPlatformService;

/// Observable state of the current thread's product playback scheduling class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackThreadSchedulingStatus {
    /// No playback scheduling class is active.
    Inactive,
    /// The native multimedia playback class is active on this thread.
    Active,
    /// This platform has no native implementation yet; playback remains valid
    /// under the portable scheduler contract.
    Unsupported,
}

/// Failure to enter the native multimedia playback scheduling class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackThreadSchedulingError {
    operation: &'static str,
    detail: String,
}

impl PlaybackThreadSchedulingError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self { operation, detail: detail.into() }
    }
}

impl std::fmt::Display for PlaybackThreadSchedulingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.detail)
    }
}

impl std::error::Error for PlaybackThreadSchedulingError {}

/// Thread-affine native scheduling state for the product playback coordinator.
///
/// The owner synchronizes this object with transport residency. On Windows it
/// joins the MMCSS `Playback` task at critical relative priority and reverts that
/// registration when playback stops or the owner is dropped. It deliberately
/// does not change process-wide timer resolution or worker-pool policy.
#[derive(Debug, Default)]
pub struct PlaybackThreadScheduling {
    native: Option<NativePlaybackThreadScheduling>,
    activation_attempted: bool,
}

impl PlaybackThreadScheduling {
    /// Enter or leave the native scheduling class to match playback residency.
    ///
    /// A failed activation is reported once for the current active residency;
    /// calling with `active = false` resets that attempt for a later playback
    /// session. Unsupported platforms return an explicit portable status.
    pub fn synchronize(
        &mut self,
        active: bool,
    ) -> Result<PlaybackThreadSchedulingStatus, PlaybackThreadSchedulingError> {
        if !active {
            self.native = None;
            self.activation_attempted = false;
            return Ok(PlaybackThreadSchedulingStatus::Inactive);
        }
        if self.native.is_some() {
            return Ok(PlaybackThreadSchedulingStatus::Active);
        }
        if self.activation_attempted {
            return Ok(native_playback_scheduling_absent_status());
        }
        self.activation_attempted = true;
        match NativePlaybackThreadScheduling::enter()? {
            Some(native) => {
                self.native = Some(native);
                Ok(PlaybackThreadSchedulingStatus::Active)
            }
            None => Ok(PlaybackThreadSchedulingStatus::Unsupported),
        }
    }
}

#[cfg(target_os = "windows")]
fn native_playback_scheduling_absent_status() -> PlaybackThreadSchedulingStatus {
    PlaybackThreadSchedulingStatus::Inactive
}

#[cfg(not(target_os = "windows"))]
fn native_playback_scheduling_absent_status() -> PlaybackThreadSchedulingStatus {
    PlaybackThreadSchedulingStatus::Unsupported
}

#[cfg(target_os = "windows")]
struct NativePlaybackThreadScheduling {
    handle: windows_sys::Win32::Foundation::HANDLE,
    _thread_affine: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(target_os = "windows")]
impl std::fmt::Debug for NativePlaybackThreadScheduling {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NativePlaybackThreadScheduling").finish_non_exhaustive()
    }
}

#[cfg(target_os = "windows")]
impl NativePlaybackThreadScheduling {
    fn enter() -> Result<Option<Self>, PlaybackThreadSchedulingError> {
        use windows_sys::Win32::System::Threading::{
            AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
            AVRT_PRIORITY_CRITICAL,
        };

        const PLAYBACK_TASK: &[u16] = &[
            b'P' as u16,
            b'l' as u16,
            b'a' as u16,
            b'y' as u16,
            b'b' as u16,
            b'a' as u16,
            b'c' as u16,
            b'k' as u16,
            0,
        ];
        let mut task_index = 0_u32;
        // SAFETY: PLAYBACK_TASK is a static NUL-terminated UTF-16 string and
        // task_index remains valid for the duration of the call.
        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(PLAYBACK_TASK.as_ptr(), &mut task_index) };
        if handle.is_null() {
            return Err(PlaybackThreadSchedulingError::new(
                "AvSetMmThreadCharacteristicsW",
                std::io::Error::last_os_error().to_string(),
            ));
        }
        // SAFETY: handle is the live registration returned above and is owned
        // by this current thread until reverted below or in Drop.
        if unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) } == 0 {
            let error = std::io::Error::last_os_error().to_string();
            // SAFETY: the handle is still live and owned by this thread.
            unsafe { AvRevertMmThreadCharacteristics(handle) };
            return Err(PlaybackThreadSchedulingError::new(
                "AvSetMmThreadPriority",
                error,
            ));
        }
        Ok(Some(Self {
            handle,
            _thread_affine: std::marker::PhantomData,
        }))
    }
}

#[cfg(target_os = "windows")]
impl Drop for NativePlaybackThreadScheduling {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics;

        // SAFETY: this non-Send guard can only be dropped on the thread that
        // owns its still-live MMCSS registration.
        unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

#[cfg(not(target_os = "windows"))]
#[derive(Debug)]
struct NativePlaybackThreadScheduling;

#[cfg(not(target_os = "windows"))]
impl NativePlaybackThreadScheduling {
    fn enter() -> Result<Option<Self>, PlaybackThreadSchedulingError> {
        Ok(None)
    }
}

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

    fn open_folder_dialog(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    fn open_url(&self, _url: &str) {}

    fn reveal_in_file_manager(&self, path: &Path) {
        reveal_path_in_file_manager(path);
    }

    fn send_notification(&self, _title: &str, _body: &str) {}
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

impl NativeVideoTextureImportProbe for SystemPlatformService {
    fn native_video_texture_import(&self) -> NativeVideoTextureImportProbeResult {
        system_native_video_texture_import()
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

#[cfg(target_os = "windows")]
fn system_physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
    let mut kib = 0_u64;
    // SAFETY: Windows writes one u64 to the valid pointer for the duration of
    // this call and retains no reference.
    let result = unsafe {
        windows_sys::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory(&mut kib)
    };
    if result == 0 {
        return PhysicalMemoryCapacityProbeResult::failed(
            PhysicalMemoryCapacityProbeBackend::WindowsInstalledSystemMemory,
            format!(
                "GetPhysicallyInstalledSystemMemory failed with OS error {}",
                std::io::Error::last_os_error()
            ),
        );
    }
    match kib.checked_mul(1024) {
        Some(bytes) => PhysicalMemoryCapacityProbeResult::observed(
            PhysicalMemoryCapacityProbeBackend::WindowsInstalledSystemMemory,
            bytes,
        ),
        None => PhysicalMemoryCapacityProbeResult::failed(
            PhysicalMemoryCapacityProbeBackend::WindowsInstalledSystemMemory,
            "installed physical memory exceeds the supported byte range",
        ),
    }
}

#[cfg(not(target_os = "windows"))]
fn system_physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
    PhysicalMemoryCapacityProbeResult::unsupported(
        "installed physical memory discovery is not implemented for this platform",
    )
}

#[cfg(target_os = "windows")]
fn system_memory() -> SystemMemoryProbeResult {
    use std::mem;
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..MEMORYSTATUSEX::default()
    };
    // SAFETY: Windows writes one complete MEMORYSTATUSEX to the valid,
    // correctly sized pointer and retains no reference.
    let result = unsafe { GlobalMemoryStatusEx(&mut status) };
    if result == 0 {
        return SystemMemoryProbeResult::failed(
            SystemMemoryProbeBackend::WindowsGlobalMemoryStatus,
            format!(
                "GlobalMemoryStatusEx failed with OS error {}",
                std::io::Error::last_os_error()
            ),
        );
    }

    SystemMemoryProbeResult::observed(
        SystemMemoryProbeBackend::WindowsGlobalMemoryStatus,
        status.ullTotalPhys,
        status.ullAvailPhys,
        status.dwMemoryLoad,
    )
}

#[cfg(not(target_os = "windows"))]
fn system_memory() -> SystemMemoryProbeResult {
    SystemMemoryProbeResult::unsupported(
        "native whole-system memory discovery is not implemented for this platform",
    )
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

fn system_native_video_texture_import() -> NativeVideoTextureImportProbeResult {
    #[cfg(target_os = "windows")]
    {
        windows_native_video_texture_import::probe()
    }
    #[cfg(target_os = "macos")]
    {
        NativeVideoTextureImportProbeResult::missing(
            "VideoToolbox CVPixelBuffer/IOSurface import is not connected to the wgpu renderer",
        )
    }
    #[cfg(target_os = "linux")]
    {
        NativeVideoTextureImportProbeResult::missing(
            "VA-API/DMABUF texture import is not connected to the wgpu renderer",
        )
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        NativeVideoTextureImportProbeResult::unsupported(
            "native video texture import is not implemented for this platform",
        )
    }
}

#[cfg(target_os = "windows")]
mod windows_native_video_texture_import {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use mondrian_platform_core::{
        NativeVideoTextureHandleKind, NativeVideoTextureImportProbeResult,
    };
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    const D3D_DRIVER_TYPE_HARDWARE: u32 = 1;
    const D3D11_CREATE_DEVICE_BGRA_SUPPORT: u32 = 0x20;
    const D3D11_SDK_VERSION: u32 = 7;
    const D3D_FEATURE_LEVEL_11_1: u32 = 0xb100;
    const D3D_FEATURE_LEVEL_11_0: u32 = 0xb000;
    const D3D_FEATURE_LEVEL_10_1: u32 = 0xa100;
    const IID_ID3D12_DEVICE: Guid = Guid {
        data1: 0x189819f1,
        data2: 0x1db6,
        data3: 0x4b57,
        data4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7],
    };

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    type D3D11CreateDeviceFn = unsafe extern "system" fn(
        padapter: *mut c_void,
        drivertype: u32,
        software: *mut c_void,
        flags: u32,
        pfeaturelevels: *const u32,
        featurelevels: u32,
        sdkversion: u32,
        ppdevice: *mut *mut c_void,
        pfeaturelevel: *mut u32,
        ppimmediatecontext: *mut *mut c_void,
    ) -> i32;

    type D3D12CreateDeviceFn = unsafe extern "system" fn(
        padapter: *mut c_void,
        minimum_feature_level: u32,
        riid: *const Guid,
        ppdevice: *mut *mut c_void,
    ) -> i32;

    #[repr(C)]
    struct IUnknownVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    /// Probe Windows D3D12/D3D11 native video texture staging capability.
    pub fn probe() -> NativeVideoTextureImportProbeResult {
        let mut supported_handle_kinds = Vec::new();
        let mut reasons = Vec::new();

        match probe_d3d12_device() {
            Ok(feature_level) => {
                supported_handle_kinds.push(NativeVideoTextureHandleKind::D3D12Resource);
                reasons.push(format!(
                    "D3D12 device probe succeeded at minimum feature level 0x{feature_level:x}"
                ));
            }
            Err(reason) => reasons.push(reason),
        }
        match probe_d3d11_device() {
            Ok(feature_level) => {
                supported_handle_kinds.push(NativeVideoTextureHandleKind::D3D11Texture2D);
                reasons.push(format!(
                    "D3D11 device probe succeeded at feature level 0x{feature_level:x}"
                ));
            }
            Err(reason) => reasons.push(reason),
        }

        if supported_handle_kinds.is_empty() {
            NativeVideoTextureImportProbeResult::missing(reasons.join("; "))
        } else {
            reasons.push(
                "native zero-copy renderer import is still gated by renderer backend support"
                    .to_owned(),
            );
            NativeVideoTextureImportProbeResult::found_partial(
                supported_handle_kinds,
                false,
                true,
                reasons.join("; "),
            )
        }
    }

    fn probe_d3d12_device() -> Result<u32, String> {
        let library = unsafe { LoadLibraryW(wide_null("d3d12.dll").as_ptr()) };
        if library.is_null() {
            return Err(
                "d3d12.dll is unavailable; D3D12 native video texture probe failed".to_owned(),
            );
        }

        let result = unsafe { probe_d3d12_device_with_library(library) };
        unsafe {
            FreeLibrary(library);
        }
        result
    }

    unsafe fn probe_d3d12_device_with_library(library: *mut c_void) -> Result<u32, String> {
        let symbol = GetProcAddress(library, c"D3D12CreateDevice".as_ptr().cast::<u8>());
        let Some(symbol) = symbol else {
            return Err("d3d12.dll does not export D3D12CreateDevice".to_owned());
        };
        let create_device: D3D12CreateDeviceFn = std::mem::transmute(symbol);
        let mut device: *mut c_void = ptr::null_mut();
        let hr = create_device(
            ptr::null_mut(),
            D3D_FEATURE_LEVEL_11_0,
            &IID_ID3D12_DEVICE,
            &mut device,
        );

        release_unknown(device);

        if hr < 0 {
            return Err(format!(
                "D3D12CreateDevice failed with HRESULT 0x{:08x}",
                hr as u32
            ));
        }
        Ok(D3D_FEATURE_LEVEL_11_0)
    }

    fn probe_d3d11_device() -> Result<u32, String> {
        let library = unsafe { LoadLibraryW(wide_null("d3d11.dll").as_ptr()) };
        if library.is_null() {
            return Err(
                "d3d11.dll is unavailable; D3D11 native video texture probe failed".to_owned(),
            );
        }

        let result = unsafe { probe_d3d11_device_with_library(library) };
        unsafe {
            FreeLibrary(library);
        }
        result
    }

    unsafe fn probe_d3d11_device_with_library(library: *mut c_void) -> Result<u32, String> {
        let symbol = GetProcAddress(library, c"D3D11CreateDevice".as_ptr().cast::<u8>());
        let Some(symbol) = symbol else {
            return Err("d3d11.dll does not export D3D11CreateDevice".to_owned());
        };
        let create_device: D3D11CreateDeviceFn = std::mem::transmute(symbol);
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];
        let mut device: *mut c_void = ptr::null_mut();
        let mut context: *mut c_void = ptr::null_mut();
        let mut resolved_feature_level = 0;
        let hr = create_device(
            ptr::null_mut(),
            D3D_DRIVER_TYPE_HARDWARE,
            ptr::null_mut(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            feature_levels.as_ptr(),
            feature_levels.len() as u32,
            D3D11_SDK_VERSION,
            &mut device,
            &mut resolved_feature_level,
            &mut context,
        );

        release_unknown(context);
        release_unknown(device);

        if hr < 0 {
            return Err(format!(
                "D3D11CreateDevice failed with HRESULT 0x{:08x}",
                hr as u32
            ));
        }
        Ok(resolved_feature_level)
    }

    unsafe fn release_unknown(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let vtbl = *(ptr as *mut *mut IUnknownVtbl);
        ((*vtbl).release)(ptr);
    }

    fn wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_native_video_texture_probe_reports_direct3d_without_zero_copy_claim() {
        let result = system_native_video_texture_import();

        assert!(result.discovery_available);
        assert!(!result.zero_copy_supported);
        if result.supports(NativeVideoTextureHandleKind::D3D12Resource)
            || result.supports(NativeVideoTextureHandleKind::D3D11Texture2D)
        {
            assert!(result.low_copy_fallback_supported);
            assert!(result.error.as_deref().unwrap_or_default().contains("D3D"));
            assert!(result.error.as_deref().unwrap_or_default().contains("zero-copy"));
        } else {
            assert!(!result.low_copy_fallback_supported);
            assert!(result.error.is_some());
        }
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
        assert!(result.private_committed_bytes.is_some_and(|bytes| bytes > 0));
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
        assert!(result.private_committed_bytes.is_some_and(|bytes| bytes > 0));
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
        folder_dialog_result: Option<PathBuf>,
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
            clipboard_content: Ok(Some("copied text".into())),
            file_dialog_result: Some(vec![PathBuf::from("/test/file.mp4")]),
            save_dialog_result: Some(PathBuf::from("/test/output.mp4")),
            folder_dialog_result: Some(PathBuf::from("/test/folder")),
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
        assert_eq!(
            mock.open_folder_dialog(""),
            Some(PathBuf::from("/test/folder")),
        );
    }
}
