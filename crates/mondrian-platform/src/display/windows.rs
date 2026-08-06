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
    WcsGetDefaultColorProfile, WcsGetDefaultColorProfileSize, CPST_NONE, CPST_RGB_WORKING_SPACE,
    CPST_STANDARD_DISPLAY_COLOR_MODE, CPT_ICC, WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
    WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
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
    // SAFETY: `probe_monitor_device_name` passes a live, uniquely borrowed
    // `MonitorSearch` pointer to `EnumDisplayMonitors` for the synchronous
    // duration of this callback enumeration.
    let search = unsafe { &mut *(lparam as *mut MonitorSearch) };
    if search.matched_name.is_some() {
        return 0;
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
    center_x >= rect.left && center_x < rect.right && center_y >= rect.top && center_y < rect.bottom
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
    use super::sdr_white_level_to_nits;

    #[test]
    fn windows_sdr_white_level_uses_normative_80_nit_scale() {
        assert_eq!(sdr_white_level_to_nits(1_000), 80);
        assert_eq!(sdr_white_level_to_nits(2_000), 160);
        assert_eq!(sdr_white_level_to_nits(2_537), 203);
    }
}
