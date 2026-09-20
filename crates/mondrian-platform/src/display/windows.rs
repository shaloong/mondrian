//! Windows WCS and DisplayConfig adapters.

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
use windows_sys::Win32::Foundation::{
    LocalFree, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, LPARAM, LUID, RECT,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows_sys::Win32::Graphics::Gdi::{
    DISPLAYCONFIG_COLOR_ENCODING_INTENSITY, DISPLAYCONFIG_COLOR_ENCODING_RGB,
    DISPLAYCONFIG_COLOR_ENCODING_YCBCR420, DISPLAYCONFIG_COLOR_ENCODING_YCBCR422,
    DISPLAYCONFIG_COLOR_ENCODING_YCBCR444,
};
use windows_sys::Win32::UI::ColorSystem::{
    ColorProfileGetDisplayDefault, ColorProfileGetDisplayUserScope, WcsGetDefaultColorProfile,
    WcsGetDefaultColorProfileSize, CPST_EXTENDED_DISPLAY_COLOR_MODE, CPST_NONE,
    CPST_STANDARD_DISPLAY_COLOR_MODE, CPT_ICC, WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
    WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
};

use super::physical_rect_matches_target;

// Windows 11 exposes the exact active SDR/WCG/HDR mode through
// DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2. windows-sys 0.59 does not bind the
// structure yet, so keep this ABI-local definition aligned with wingdi.h.
const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: i32 = 15;
const DISPLAYCONFIG_ADVANCED_COLOR_MODE_SDR: i32 = 0;
const DISPLAYCONFIG_ADVANCED_COLOR_MODE_WCG: i32 = 1;
const DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR: i32 = 2;

#[repr(C)]
#[derive(Default)]
struct DisplayConfigGetAdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

pub fn display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    if !target.is_valid() {
        return DisplayIccProfileProbeResult::missing(
            DisplayProbeBackend::WindowsWcs,
            None,
            "display target has an empty physical extent",
        );
    }
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
        Ok((backend, path)) => DisplayIccProfileProbeResult::found_path(
            backend,
            Some(display_device_name.clone()),
            path,
        )
        .with_native_display_path_id(Some(display_device_name)),
        Err((backend, reason)) => DisplayIccProfileProbeResult::missing(
            backend,
            Some(display_device_name.clone()),
            reason,
        )
        .with_native_display_path_id(Some(display_device_name)),
    }
}

pub fn display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    if !target.is_valid() {
        return DisplayHdrProbeResult::missing(
            DisplayProbeBackend::WindowsDisplayConfig,
            None,
            "display target has an empty physical extent",
        );
    }
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
            Some(display_device_name.clone()),
            DisplayHdrProbeDetails {
                hdr_supported: Some(state.hdr_supported),
                hdr_enabled: Some(state.hdr_enabled),
                wide_color_supported: state.wide_color_supported,
                wide_color_active: Some(state.wide_color_active),
                force_disabled: Some(state.force_disabled),
                bits_per_color_channel: Some(state.bits_per_color_channel),
                color_encoding: state.color_encoding,
                sdr_reference_white_nits: state.sdr_white_level.map(sdr_white_level_to_nits),
                ..DisplayHdrProbeDetails::default()
            },
        )
        .with_native_display_path_id(Some(display_device_name)),
        Err(reason) => DisplayHdrProbeResult::failed(
            DisplayProbeBackend::WindowsDisplayConfig,
            Some(display_device_name.clone()),
            reason,
        )
        .with_native_display_path_id(Some(display_device_name)),
    }
}

fn display_device_name_for_target(
    target: DisplayProfileProbeTarget,
) -> Result<Option<String>, String> {
    let mut search = MonitorSearch {
        target,
        matched_name: None,
        monitor_info_failed: false,
    };
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
    if search.matched_name.is_none() && search.monitor_info_failed {
        return Err("GetMonitorInfoW failed while resolving the display target".to_owned());
    }
    Ok(search.matched_name)
}

struct MonitorSearch {
    target: DisplayProfileProbeTarget,
    matched_name: Option<String>,
    monitor_info_failed: bool,
}

unsafe extern "system" fn enum_monitor_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> windows_sys::core::BOOL {
    // SAFETY: `probe_monitor_device_name` passes a live, uniquely borrowed
    // `MonitorSearch` pointer to `EnumDisplayMonitors` for the synchronous
    // duration of this callback enumeration.
    let search = unsafe { &mut *(lparam as *mut MonitorSearch) };
    if search.matched_name.is_some() {
        return 1;
    }

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: Windows supplied `monitor`; `info` is initialized with the
    // required `cbSize` and remains writable for the duration of the call.
    let ok = unsafe {
        GetMonitorInfoW(
            monitor,
            &mut info as *mut MONITORINFOEXW as *mut windows_sys::Win32::Graphics::Gdi::MONITORINFO,
        )
    };
    if ok == 0 {
        search.monitor_info_failed = true;
        return 1;
    }

    if monitor_matches_target(&info.monitorInfo.rcMonitor, search.target) {
        search.matched_name = utf16z_to_string(&info.szDevice);
        return 1;
    }

    1
}

fn monitor_matches_target(rect: &RECT, target: DisplayProfileProbeTarget) -> bool {
    let width = rect.right.saturating_sub(rect.left) as u32;
    let height = rect.bottom.saturating_sub(rect.top) as u32;
    physical_rect_matches_target(rect.left, rect.top, width, height, target)
}

fn default_icc_profile_for_device(
    device_name: &str,
) -> Result<(DisplayProbeBackend, PathBuf), (DisplayProbeBackend, String)> {
    let mut failures = Vec::new();
    let path = active_display_path_for_device(device_name).map_err(|reason| {
        (
            DisplayProbeBackend::WindowsColorProfileDisplayDefault,
            reason,
        )
    })?;
    let advanced_color = advanced_color_state_for_path(&path).map_err(|reason| {
        (
            DisplayProbeBackend::WindowsColorProfileDisplayDefault,
            format!("could not determine the active display color mode: {reason}"),
        )
    })?;
    let scope = display_profile_management_scope_for_path(&path).map_err(|reason| {
        (
            DisplayProbeBackend::WindowsColorProfileDisplayDefault,
            reason,
        )
    })?;
    let subtype = advanced_color.profile_subtype;
    match display_default_profile_for_path(&path, scope, subtype) {
        Ok(profile) => {
            return Ok((
                DisplayProbeBackend::WindowsColorProfileDisplayDefault,
                resolve_color_profile_path(profile),
            ));
        }
        Err(reason) => failures.push(reason),
    }

    if subtype == CPST_EXTENDED_DISPLAY_COLOR_MODE {
        return Err((
            DisplayProbeBackend::WindowsColorProfileDisplayDefault,
            format!(
                "no default ICC profile is associated with the active HDR display mode ({})",
                failures.join("; ")
            ),
        ));
    }

    let device_name = wide_null(device_name);
    // WCS is the compatibility fallback for standard display mode. CPST_NONE
    // aliases the standard-display subtype; it cannot discover the active
    // Advanced Color association. Query only the association scope Windows
    // selected for this display; a stale profile in the inactive scope is not
    // the current display default.
    match default_icc_profile_for_scope(device_name.as_ptr(), scope, CPST_NONE) {
        Ok(path) => {
            return Ok((
                DisplayProbeBackend::WindowsWcs,
                resolve_color_profile_path(path),
            ));
        }
        Err(reason) => failures.push(reason),
    }

    Err((
        DisplayProbeBackend::WindowsWcs,
        format!(
            "WcsGetDefaultColorProfile did not return a standard-mode profile ({})",
            failures.join("; ")
        ),
    ))
}

fn display_profile_management_scope_for_path(
    path: &DISPLAYCONFIG_PATH_INFO,
) -> Result<i32, String> {
    let mut scope = WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE;
    let result = unsafe {
        ColorProfileGetDisplayUserScope(path.sourceInfo.adapterId, path.sourceInfo.id, &mut scope)
    };
    if result < 0 {
        return Err(format!(
            "ColorProfileGetDisplayUserScope failed with HRESULT=0x{:08x}",
            result as u32
        ));
    }
    match scope {
        WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER | WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE => {
            Ok(scope)
        }
        _ => Err(format!(
            "ColorProfileGetDisplayUserScope returned unknown scope {scope}"
        )),
    }
}

fn active_display_profile_subtype(active_color_mode: i32) -> Result<i32, String> {
    match active_color_mode {
        DISPLAYCONFIG_ADVANCED_COLOR_MODE_SDR | DISPLAYCONFIG_ADVANCED_COLOR_MODE_WCG => {
            Ok(CPST_STANDARD_DISPLAY_COLOR_MODE)
        }
        DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR => Ok(CPST_EXTENDED_DISPLAY_COLOR_MODE),
        _ => Err(format!(
            "DisplayConfig returned unknown active color mode {active_color_mode}"
        )),
    }
}

fn legacy_display_profile_subtype(advanced_color_enabled: bool) -> i32 {
    if advanced_color_enabled {
        CPST_EXTENDED_DISPLAY_COLOR_MODE
    } else {
        CPST_STANDARD_DISPLAY_COLOR_MODE
    }
}
struct WindowsAdvancedColorState {
    hdr_supported: bool,
    hdr_enabled: bool,
    wide_color_supported: Option<bool>,
    wide_color_active: bool,
    force_disabled: bool,
    bits_per_color_channel: u32,
    color_encoding: Option<String>,
    sdr_white_level: Option<u32>,
    profile_subtype: i32,
}

fn advanced_color_for_device(device_name: &str) -> Result<WindowsAdvancedColorState, String> {
    let path = active_display_path_for_device(device_name)?;
    advanced_color_state_for_path(&path)
}

fn advanced_color_state_for_path(
    path: &DISPLAYCONFIG_PATH_INFO,
) -> Result<WindowsAdvancedColorState, String> {
    let sdr_white_level = sdr_white_level(path).ok();
    match advanced_color_info_2(path) {
        Ok(advanced) => {
            let flags = advanced.flags;
            let active_mode = advanced.active_color_mode;
            let profile_subtype = active_display_profile_subtype(active_mode)?;
            let hdr_active = active_mode == DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR;
            Ok(WindowsAdvancedColorState {
                hdr_supported: bit(flags, 4),
                hdr_enabled: hdr_active,
                wide_color_supported: Some(bit(flags, 6)),
                wide_color_active: active_mode != DISPLAYCONFIG_ADVANCED_COLOR_MODE_SDR,
                force_disabled: bit(flags, 3),
                bits_per_color_channel: advanced.bits_per_color_channel,
                color_encoding: Some(color_encoding_name(advanced.color_encoding).to_owned()),
                sdr_white_level,
                profile_subtype,
            })
        }
        Err(code)
            if code == ERROR_INVALID_PARAMETER as i32 || code == ERROR_NOT_SUPPORTED as i32 =>
        {
            // Pre-Windows 11 systems do not expose the SDR/WCG/HDR enum. Preserve
            // their legacy Advanced Color interpretation; on those systems an
            // enabled Advanced Color desktop is the HDR mode.
            let advanced = advanced_color_info(path)?;
            let flags = unsafe { advanced.Anonymous.value };
            let advanced_color_enabled = bit(flags, 1);
            Ok(WindowsAdvancedColorState {
                hdr_supported: bit(flags, 0),
                hdr_enabled: advanced_color_enabled,
                wide_color_supported: None,
                wide_color_active: bit(flags, 2),
                force_disabled: bit(flags, 3),
                bits_per_color_channel: advanced.bitsPerColorChannel,
                color_encoding: Some(color_encoding_name(advanced.colorEncoding).to_owned()),
                sdr_white_level,
                profile_subtype: legacy_display_profile_subtype(advanced_color_enabled),
            })
        }
        Err(code) => Err(format!(
            "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO_2) failed with code {code}"
        )),
    }
}
fn active_display_path_for_device(device_name: &str) -> Result<DISPLAYCONFIG_PATH_INFO, String> {
    let mut matching_path = None;
    for path in active_display_paths()? {
        let source_name = source_gdi_device_name(&path)?;
        if source_name.eq_ignore_ascii_case(device_name) {
            if matching_path.is_some() {
                return Err(format!(
                    "DisplayConfig source device '{device_name}' maps to multiple active targets"
                ));
            }
            matching_path = Some(path);
        }
    }
    matching_path.ok_or_else(|| {
        format!("DisplayConfig active paths did not include source device '{device_name}'")
    })
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

fn advanced_color_info_2(
    path: &DISPLAYCONFIG_PATH_INFO,
) -> Result<DisplayConfigGetAdvancedColorInfo2, i32> {
    let mut packet = DisplayConfigGetAdvancedColorInfo2 {
        header: display_config_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
            mem::size_of::<DisplayConfigGetAdvancedColorInfo2>(),
            path.targetInfo.adapterId,
            path.targetInfo.id,
        ),
        ..DisplayConfigGetAdvancedColorInfo2::default()
    };
    let result = unsafe {
        DisplayConfigGetDeviceInfo(
            &mut packet as *mut DisplayConfigGetAdvancedColorInfo2
                as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
        )
    };
    if result != 0 {
        return Err(result);
    }
    Ok(packet)
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

fn display_default_profile_for_path(
    path: &DISPLAYCONFIG_PATH_INFO,
    scope: i32,
    subtype: i32,
) -> Result<PathBuf, String> {
    let mut profile_name = ptr::null_mut();
    let result = unsafe {
        ColorProfileGetDisplayDefault(
            scope,
            path.sourceInfo.adapterId,
            path.sourceInfo.id,
            CPT_ICC,
            subtype,
            &mut profile_name,
        )
    };
    if result < 0 || profile_name.is_null() {
        return Err(format!(
            "ColorProfileGetDisplayDefault failed for scope={scope} subtype={} with HRESULT=0x{:08x}",
            display_profile_subtype_name(subtype),
            result as u32
        ));
    }

    let profile = unsafe {
        let mut length = 0usize;
        while length < 32_768 && *profile_name.add(length) != 0 {
            length += 1;
        }
        let value = if length == 0 || length == 32_768 {
            None
        } else {
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                profile_name,
                length,
            )))
        };
        let _ = LocalFree(profile_name.cast());
        value
    }
    .ok_or_else(|| {
        format!(
            "ColorProfileGetDisplayDefault returned an invalid path for scope={scope} subtype={}",
            display_profile_subtype_name(subtype)
        )
    })?;
    Ok(PathBuf::from(profile))
}

fn display_profile_subtype_name(subtype: i32) -> &'static str {
    match subtype {
        CPST_EXTENDED_DISPLAY_COLOR_MODE => "extended-display-color-mode",
        CPST_STANDARD_DISPLAY_COLOR_MODE => "standard-display-color-mode",
        CPST_NONE => "none",
        _ => "unknown",
    }
}
fn resolve_color_profile_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }

    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("spool").join("drivers").join("color").join(&path))
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
    use super::{
        active_display_profile_subtype, sdr_white_level_to_nits,
        DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR, DISPLAYCONFIG_ADVANCED_COLOR_MODE_SDR,
        DISPLAYCONFIG_ADVANCED_COLOR_MODE_WCG,
    };
    use windows_sys::Win32::UI::ColorSystem::{
        CPST_EXTENDED_DISPLAY_COLOR_MODE, CPST_STANDARD_DISPLAY_COLOR_MODE,
    };

    #[test]
    fn windows_sdr_white_level_uses_normative_80_nit_scale() {
        assert_eq!(sdr_white_level_to_nits(1_000), 80);
        assert_eq!(sdr_white_level_to_nits(2_000), 160);
        assert_eq!(sdr_white_level_to_nits(2_537), 203);
    }

    #[test]
    fn windows_profile_subtype_uses_extended_only_for_hdr() {
        assert_eq!(
            active_display_profile_subtype(DISPLAYCONFIG_ADVANCED_COLOR_MODE_SDR),
            Ok(CPST_STANDARD_DISPLAY_COLOR_MODE)
        );
        assert_eq!(
            active_display_profile_subtype(DISPLAYCONFIG_ADVANCED_COLOR_MODE_WCG),
            Ok(CPST_STANDARD_DISPLAY_COLOR_MODE)
        );
        assert_eq!(
            active_display_profile_subtype(DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR),
            Ok(CPST_EXTENDED_DISPLAY_COLOR_MODE)
        );
        assert!(active_display_profile_subtype(99).is_err());
    }
}
