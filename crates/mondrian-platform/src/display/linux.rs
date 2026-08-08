//! Linux display probing through Wayland color-management, X11 ICC properties,
//! and DRM connector EDID fallback.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mondrian_platform_core::{
    DisplayHdrProbeDetails, DisplayHdrProbeResult, DisplayIccProfileProbeResult,
    DisplayProbeBackend, DisplayProfileProbeTarget,
};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::WpColorManagementOutputV1,
    wp_color_manager_v1::WpColorManagerV1,
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use x11rb::connection::Connection as _;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

use super::edid::parse_hdr_capabilities;

const MAX_ICC_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn display_icc_profile(
    target: DisplayProfileProbeTarget,
) -> DisplayIccProfileProbeResult {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        match cached_wayland_evidence(target) {
            Ok(evidence) => {
                return match evidence.icc_bytes {
                    Some(bytes) => DisplayIccProfileProbeResult::found_bytes(
                        DisplayProbeBackend::WaylandColorManagementV1,
                        evidence.display_name,
                        bytes,
                    ),
                    None => DisplayIccProfileProbeResult::missing(
                        DisplayProbeBackend::WaylandColorManagementV1,
                        evidence.display_name,
                        "the active Wayland output image description is parametric and did not expose an ICC payload",
                    ),
                };
            }
            Err(wayland_error) if std::env::var_os("DISPLAY").is_none() => {
                return DisplayIccProfileProbeResult::missing(
                    DisplayProbeBackend::WaylandColorManagementV1,
                    None,
                    wayland_error,
                );
            }
            Err(_) => {}
        }
    }

    match x11_icc_profile(target) {
        Ok((name, bytes)) => DisplayIccProfileProbeResult::found_bytes(
            DisplayProbeBackend::X11RootProperty,
            name,
            bytes,
        ),
        Err(reason) => DisplayIccProfileProbeResult::missing(
            DisplayProbeBackend::X11RootProperty,
            None,
            reason,
        ),
    }
}

pub(crate) fn display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && let Ok(evidence) = cached_wayland_evidence(target)
    {
        let active_hdr =
            evidence.transfer_function.as_deref().is_some_and(is_hdr_transfer_function);
        return DisplayHdrProbeResult::found(
            DisplayProbeBackend::WaylandColorManagementV1,
            evidence.display_name,
            DisplayHdrProbeDetails {
                hdr_supported: active_hdr.then_some(true),
                hdr_enabled: Some(active_hdr),
                wide_color_active: evidence.wide_color_active,
                force_disabled: Some(false),
                active_transfer_function: evidence.transfer_function,
                sdr_reference_white_nits: evidence.reference_white_nits,
                min_luminance_millinits: evidence.min_luminance_millinits,
                max_luminance_nits: evidence.max_luminance_nits,
                ..DisplayHdrProbeDetails::default()
            },
        );
    }

    match drm_hdr_state(target) {
        Ok((display_name, details)) => DisplayHdrProbeResult::found(
            DisplayProbeBackend::LinuxDrmSysfs,
            Some(display_name),
            details,
        ),
        Err(reason) => {
            DisplayHdrProbeResult::missing(DisplayProbeBackend::LinuxDrmSysfs, None, reason)
        }
    }
}

fn x11_icc_profile(target: DisplayProfileProbeTarget) -> Result<(Option<String>, Vec<u8>), String> {
    let (connection, screen_index) =
        x11rb::connect(None).map_err(|error| format!("X11 connection failed: {error}"))?;
    let screen = connection
        .setup()
        .roots
        .get(screen_index)
        .ok_or_else(|| "X11 default screen is missing".to_owned())?;
    let monitors = connection
        .randr_get_monitors(screen.root, true)
        .map_err(|error| format!("RandR GetMonitors request failed: {error}"))?
        .reply()
        .map_err(|error| format!("RandR GetMonitors reply failed: {error}"))?;
    let monitor_index = monitors
        .monitors
        .iter()
        .position(|monitor| {
            rectangle_matches_target(
                i32::from(monitor.x),
                i32::from(monitor.y),
                u32::from(monitor.width),
                u32::from(monitor.height),
                target,
            )
        })
        .ok_or_else(|| "no RandR monitor matched the winit display rectangle".to_owned())?;
    let monitor = &monitors.monitors[monitor_index];
    let monitor_name = connection
        .get_atom_name(monitor.name)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .and_then(|reply| String::from_utf8(reply.name).ok());

    let mut property_names = Vec::with_capacity(3);
    if monitor_index == 0 {
        property_names.push("_ICC_PROFILE".to_owned());
        property_names.push("_ICC_PROFILE_0".to_owned());
    } else {
        property_names.push(format!("_ICC_PROFILE_{monitor_index}"));
        if monitor.primary {
            property_names.push("_ICC_PROFILE".to_owned());
        }
    }

    for property_name in property_names {
        let atom = connection
            .intern_atom(true, property_name.as_bytes())
            .map_err(|error| format!("X11 intern_atom failed: {error}"))?
            .reply()
            .map_err(|error| format!("X11 intern_atom reply failed: {error}"))?
            .atom;
        if atom == 0 {
            continue;
        }
        let property = connection
            .get_property(
                false,
                screen.root,
                atom,
                AtomEnum::ANY,
                0,
                (MAX_ICC_BYTES / 4) as u32,
            )
            .map_err(|error| format!("X11 ICC property request failed: {error}"))?
            .reply()
            .map_err(|error| format!("X11 ICC property reply failed: {error}"))?;
        if property.format == 8 && !property.value.is_empty() {
            if property.value.len() > MAX_ICC_BYTES {
                return Err("X11 ICC profile exceeds the 32 MiB safety limit".to_owned());
            }
            return Ok((monitor_name, property.value));
        }
    }

    Err("the matching RandR monitor has no _ICC_PROFILE property".to_owned())
}

fn drm_hdr_state(
    target: DisplayProfileProbeTarget,
) -> Result<(String, DisplayHdrProbeDetails), String> {
    let drm_root = Path::new("/sys/class/drm");
    let entries = fs::read_dir(drm_root)
        .map_err(|error| format!("failed to enumerate /sys/class/drm: {error}"))?;
    let mut connected = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::read_to_string(path.join("status"))
            .ok()
            .is_some_and(|status| status.trim() == "connected")
        {
            connected.push(path);
        }
    }
    if connected.is_empty() {
        return Err("DRM sysfs reported no connected display connectors".to_owned());
    }

    let mode = format!("{}x{}", target.width, target.height);
    let matching = connected
        .iter()
        .filter(|path| {
            fs::read_to_string(path.join("modes"))
                .ok()
                .is_some_and(|modes| modes.lines().any(|candidate| candidate.trim() == mode))
        })
        .cloned()
        .collect::<Vec<_>>();
    let connector = if matching.len() == 1 {
        matching[0].clone()
    } else if connected.len() == 1 {
        connected[0].clone()
    } else {
        return Err(format!(
            "DRM connector matching is ambiguous ({} connected, {} advertise {mode})",
            connected.len(),
            matching.len()
        ));
    };

    let edid = fs::read(connector.join("edid"))
        .map_err(|error| format!("failed to read DRM connector EDID: {error}"))?;
    let hdr = parse_hdr_capabilities(&edid)?;
    let display_name = connector
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-drm-connector")
        .to_owned();
    let transfer = match (hdr.pq, hdr.hlg) {
        (true, true) => Some("PQ+HLG".to_owned()),
        (true, false) => Some("PQ".to_owned()),
        (false, true) => Some("HLG".to_owned()),
        (false, false) => None,
    };
    Ok((
        display_name,
        DisplayHdrProbeDetails {
            hdr_supported: Some(hdr.pq || hdr.hlg),
            // Connector enabled does not prove that the compositor uses HDR.
            hdr_enabled: None,
            wide_color_supported: (hdr.pq || hdr.hlg).then_some(true),
            supported_transfer_functions: transfer.into_iter().collect(),
            min_luminance_millinits: hdr.min_luminance_millinits,
            max_luminance_nits: hdr.max_luminance_nits,
            ..DisplayHdrProbeDetails::default()
        },
    ))
}

fn rectangle_matches_target(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    target: DisplayProfileProbeTarget,
) -> bool {
    if x == target.x && y == target.y && width == target.width && height == target.height {
        return true;
    }
    let center_x = target.x.saturating_add((target.width / 2) as i32);
    let center_y = target.y.saturating_add((target.height / 2) as i32);
    center_x >= x
        && center_y >= y
        && center_x < x.saturating_add(width as i32)
        && center_y < y.saturating_add(height as i32)
}

#[derive(Debug, Clone, Default)]
struct WaylandDisplayEvidence {
    display_name: Option<String>,
    icc_bytes: Option<Vec<u8>>,
    transfer_function: Option<String>,
    wide_color_active: Option<bool>,
    min_luminance_millinits: Option<u32>,
    max_luminance_nits: Option<u32>,
    reference_white_nits: Option<u32>,
}

type WaylandCacheEntry = (
    DisplayProfileProbeTarget,
    Instant,
    Result<WaylandDisplayEvidence, String>,
);

fn cached_wayland_evidence(
    target: DisplayProfileProbeTarget,
) -> Result<WaylandDisplayEvidence, String> {
    static CACHE: OnceLock<Mutex<Option<WaylandCacheEntry>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((cached_target, timestamp, result)) = guard.as_ref()
        && *cached_target == target
        && timestamp.elapsed() < Duration::from_millis(500)
    {
        return result.clone();
    }

    let result = wayland_evidence(target);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((target, Instant::now(), result.clone()));
    }
    result
}

#[derive(Debug, Default)]
struct WaylandOutputState {
    position: Option<(i32, i32)>,
    size: Option<(u32, u32)>,
    scale: i32,
    name: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Default)]
struct WaylandProbeState {
    outputs: HashMap<u32, WaylandOutputState>,
    evidence: WaylandDisplayEvidence,
    error: Option<String>,
}

fn wayland_evidence(target: DisplayProfileProbeTarget) -> Result<WaylandDisplayEvidence, String> {
    let connection = Connection::connect_to_env()
        .map_err(|error| format!("Wayland connection failed: {error}"))?;
    let (globals, mut queue) = registry_queue_init::<WaylandProbeState>(&connection)
        .map_err(|error| format!("Wayland registry initialization failed: {error}"))?;
    let qh = queue.handle();
    let manager: WpColorManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|error| format!("Wayland color-management-v1 is unavailable: {error}"))?;

    let mut output_proxies = Vec::new();
    for global in globals.contents().clone_list() {
        if global.interface == wl_output::WlOutput::interface().name {
            let version = global.version.min(wl_output::WlOutput::interface().version);
            let output = globals.registry().bind::<wl_output::WlOutput, _, _>(
                global.name,
                version,
                &qh,
                global.name,
            );
            output_proxies.push((global.name, output));
        }
    }
    if output_proxies.is_empty() {
        return Err("Wayland registry reported no wl_output globals".to_owned());
    }

    let xdg_output_manager = globals.bind::<ZxdgOutputManagerV1, _, _>(&qh, 1..=3, ()).ok();
    let _xdg_outputs = xdg_output_manager
        .as_ref()
        .map(|manager| {
            output_proxies
                .iter()
                .map(|(id, output)| manager.get_xdg_output(output, &qh, *id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut state = WaylandProbeState::default();
    queue
        .roundtrip(&mut state)
        .map_err(|error| format!("Wayland output query failed: {error}"))?;
    let matching_ids = state
        .outputs
        .iter()
        .filter_map(|(id, output)| {
            let (x, y) = output.position?;
            let (width, height) = output.size?;
            let scale = output.scale.max(1);
            rectangle_matches_target(
                x.saturating_mul(scale),
                y.saturating_mul(scale),
                width,
                height,
                target,
            )
            .then_some(*id)
        })
        .collect::<Vec<_>>();
    let selected_id = if matching_ids.len() == 1 {
        matching_ids[0]
    } else if output_proxies.len() == 1 {
        output_proxies[0].0
    } else {
        return Err(format!(
            "Wayland output matching is ambiguous ({} outputs, {} rectangle matches)",
            output_proxies.len(),
            matching_ids.len()
        ));
    };
    let output = output_proxies
        .iter()
        .find(|(id, _)| *id == selected_id)
        .map(|(_, output)| output)
        .ok_or_else(|| "matching Wayland output disappeared".to_owned())?;
    let output_state = state.outputs.get(&selected_id);
    state.evidence.display_name =
        output_state.and_then(|output| output.name.clone().or_else(|| output.description.clone()));

    let color_output = manager.get_output(output, &qh, ());
    let _image_description = color_output.get_image_description(&qh, selected_id);
    queue
        .roundtrip(&mut state)
        .map_err(|error| format!("Wayland image-description query failed: {error}"))?;
    queue
        .roundtrip(&mut state)
        .map_err(|error| format!("Wayland image-description information failed: {error}"))?;
    if let Some(error) = state.error {
        return Err(error);
    }
    Ok(state.evidence)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandProbeState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, u32> for WaylandProbeState {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let output = state.outputs.entry(*id).or_default();
        match event {
            wl_output::Event::Geometry { x, y, .. } => output.position = Some((x, y)),
            wl_output::Event::Mode { flags: WEnum::Value(flags), width, height, .. }
                if flags.contains(wl_output::Mode::Current) && width > 0 && height > 0 =>
            {
                output.size = Some((width as u32, height as u32));
            }
            wl_output::Event::Scale { factor } => output.scale = factor.max(1),
            wl_output::Event::Name { name } => output.name = Some(name),
            wl_output::Event::Description { description } => {
                output.description = Some(description);
            }
            _ => {}
        }
    }
}

delegate_noop!(WaylandProbeState: ignore WpColorManagerV1);
delegate_noop!(WaylandProbeState: ignore WpColorManagementOutputV1);
delegate_noop!(WaylandProbeState: ignore ZxdgOutputManagerV1);

impl Dispatch<ZxdgOutputV1, u32> for WaylandProbeState {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        id: &u32,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let output = state.outputs.entry(*id).or_default();
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => output.position = Some((x, y)),
            zxdg_output_v1::Event::Name { name } => output.name = Some(name),
            zxdg_output_v1::Event::Description { description } => {
                output.description = Some(description);
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, u32> for WaylandProbeState {
    fn event(
        state: &mut Self,
        proxy: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        id: &u32,
        _connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. } => {
                proxy.get_information(qh, *id);
            }
            wp_image_description_v1::Event::Failed { msg, .. } => {
                state.error = Some(format!("Wayland output image description failed: {msg}"));
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, u32> for WaylandProbeState {
    fn event(
        state: &mut Self,
        _proxy: &WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        _id: &u32,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_info_v1::Event::IccFile { icc, icc_size } => {
                if icc_size as usize > MAX_ICC_BYTES {
                    state.error =
                        Some("Wayland ICC profile exceeds the 32 MiB safety limit".to_owned());
                    return;
                }
                let file = fs::File::from(icc);
                let mut bytes = Vec::with_capacity(icc_size as usize);
                match file.take(u64::from(icc_size)).read_to_end(&mut bytes) {
                    Ok(_) if bytes.len() == icc_size as usize => {
                        state.evidence.icc_bytes = Some(bytes)
                    }
                    Ok(_) => {
                        state.error =
                            Some("Wayland ICC file descriptor ended before icc_size".to_owned())
                    }
                    Err(error) => {
                        state.error = Some(format!(
                            "failed to read Wayland ICC file descriptor: {error}"
                        ))
                    }
                }
            }
            wp_image_description_info_v1::Event::TfNamed { tf } => {
                state.evidence.transfer_function = Some(wayland_enum_name(tf));
            }
            wp_image_description_info_v1::Event::PrimariesNamed { primaries } => {
                let name = wayland_enum_name(primaries);
                state.evidence.wide_color_active = Some(
                    name.contains("Bt2020")
                        || name.contains("DisplayP3")
                        || name.contains("DciP3")
                        || name.contains("AdobeRgb"),
                );
            }
            wp_image_description_info_v1::Event::Luminances { min_lum, max_lum, reference_lum } => {
                state.evidence.min_luminance_millinits = Some(min_lum / 10);
                state.evidence.max_luminance_nits = Some(max_lum);
                state.evidence.reference_white_nits = Some(reference_lum);
            }
            wp_image_description_info_v1::Event::TargetLuminance { min_lum, max_lum } => {
                state.evidence.min_luminance_millinits = Some(min_lum / 10);
                state.evidence.max_luminance_nits = Some(max_lum);
            }
            _ => {}
        }
    }
}

fn wayland_enum_name<T: std::fmt::Debug>(value: WEnum<T>) -> String {
    match value {
        WEnum::Value(value) => format!("{value:?}"),
        WEnum::Unknown(value) => format!("Unknown({value})"),
    }
}

fn is_hdr_transfer_function(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized.contains("st2084") || normalized.contains("pq") || normalized.contains("hlg")
}
