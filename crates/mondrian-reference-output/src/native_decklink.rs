//! Windows COM adapter for the independently implemented DeckLink bridge.
//!
//! Fixed BMD API 12.0 IDL comes from OBS commit
//! 671fb57daf4972fcd506689a48a474dd4eda9e66, preserving each BMD license.
//! Native MTA workers own scheduled output and independent raw VANC capture;
//! Rust retains exact admission, immutable image leases and consuming receipts.
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::*;
use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
#[path = "native_decklink_wire.rs"]
mod wire;
pub use wire::DeckLinkWireReadbackConfiguration;
use wire::{CaptureOpenFn, CapturePollFn, CapturePreflightFn, WireSession};

const NATIVE_DLL: &str = "mondrian_reference_decklink.dll";
const ERROR_BYTES: usize = 512;
const MODES: [(u32, u32, i64, i64); 11] = [
    (1920, 1080, 24000, 1001),
    (1920, 1080, 24, 1),
    (1920, 1080, 25, 1),
    (1920, 1080, 30000, 1001),
    (1920, 1080, 30, 1),
    (1920, 1080, 50, 1),
    (1920, 1080, 60000, 1001),
    (1920, 1080, 60, 1),
    (1280, 720, 50, 1),
    (1280, 720, 60000, 1001),
    (1280, 720, 60, 1),
];
#[repr(C)]
#[derive(Clone, Copy)]
struct NativeDevice {
    serial: u64,
    generation: u64,
    physical_group: u64,
    mode_mask: u32,
    input_mode_mask: u32,
    flags: u32,
    maximum_audio_channels: u32,
    driver: [u8; 96],
    name: [u8; 128],
}
#[repr(C)]
struct NativeRequest {
    mode: u32,
    channels: u32,
    ancillary: u32,
    require_reference: u32,
    preroll: u32,
    max_frames: u32,
}
#[repr(C)]
#[derive(Default)]
struct NativeEvent {
    kind: u32,
    reserved: u32,
    frame: u64,
    ticks: u64,
}
#[repr(C)]
struct NativeShutdown {
    requested: u32,
    stopped: u32,
    worker_joined: u32,
    released: u32,
    outstanding_frames: u64,
    outstanding_resources: u64,
    error: [u8; 512],
}

type AbiFn = unsafe extern "C" fn() -> u32;
type VersionFn = unsafe extern "C" fn(*mut u8, u32) -> i32;
type DiscoverFn = unsafe extern "C" fn(*mut NativeDevice, u32, *mut u32, *mut u8, u32) -> i32;
type ReferenceFn = unsafe extern "C" fn(u64, u64, *mut i32, *mut u8, u32) -> i32;
type OpenFn =
    unsafe extern "C" fn(u64, u64, *const NativeRequest, *mut *mut c_void, *mut u8, u32) -> i32;
type StartFn = unsafe extern "C" fn(*mut c_void, *mut u8, u32) -> i32;
type PollFn = unsafe extern "C" fn(*mut c_void, *mut NativeEvent, *mut u8, u32) -> i32;
type StopFn = unsafe extern "C" fn(*mut c_void) -> i32;
type ShutdownFn = unsafe extern "C" fn(*mut c_void, *mut NativeShutdown) -> i32;

struct NativeRuntime {
    module: usize,
    _image_lease: File,
    sha256: String,
    version: String,
    discover: DiscoverFn,
    reference: ReferenceFn,
    open: OpenFn,
    start: StartFn,
    poll: PollFn,
    stop: StopFn,
    shutdown: ShutdownFn,
    schedule_vanc: wire::ScheduleVancFn,
    capture_preflight: CapturePreflightFn,
    capture_open: CaptureOpenFn,
    capture_poll: CapturePollFn,
    capture_start: StartFn,
    capture_shutdown: ShutdownFn,
}
impl Drop for NativeRuntime {
    fn drop(&mut self) {
        // All native Sessions retain this Arc until their worker and device
        // have been consumed. No function pointer survives this library owner.
        unsafe {
            FreeLibrary(self.module as *mut c_void);
        }
    }
}
fn native_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|byte| *byte == 0).next().unwrap_or_default()).into_owned()
}
fn vendor(operation: &'static str, detail: impl Into<String>) -> ReferenceOutputAdapterError {
    ReferenceOutputAdapterError::Vendor { operation, detail: detail.into() }
}
fn native_failure(
    operation: &'static str,
    status: i32,
    detail: String,
) -> ReferenceOutputAdapterError {
    match status {
        -2 => ReferenceOutputAdapterError::DriverMissing { detail },
        -3 => ReferenceOutputAdapterError::DeviceUnavailable,
        -4 => ReferenceOutputAdapterError::VersionMismatch { detail },
        _ => vendor(operation, detail),
    }
}
impl NativeRuntime {
    fn load(path: &Path) -> Result<Arc<Self>, ReferenceOutputAdapterError> {
        if !path.is_absolute() || path.file_name().is_none_or(|name| name != NATIVE_DLL) {
            return Err(vendor(
                "load",
                "native DeckLink image must use its exact absolute DLL name",
            ));
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            ReferenceOutputAdapterError::DriverMissing { detail: error.to_string() }
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 128 * 1024 * 1024
        {
            return Err(vendor(
                "load",
                "native DeckLink image must be a bounded regular file",
            ));
        }
        let mut image_lease = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(path)
            .map_err(|error| vendor("image-lease", error.to_string()))?;
        let mut digest = Sha256::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let count = image_lease
                .read(&mut chunk)
                .map_err(|error| vendor("image-hash", error.to_string()))?;
            if count == 0 {
                break;
            }
            digest.update(&chunk[..count]);
        }
        let sha256 = format!("{:x}", digest.finalize());
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if wide[..wide.len() - 1].contains(&0) {
            return Err(vendor("load", "embedded NUL in DLL path"));
        }
        let module = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(ReferenceOutputAdapterError::DriverMissing {
                detail: format!(
                    "load pinned DeckLink image: {}",
                    std::io::Error::last_os_error()
                ),
            });
        }
        struct LoadGuard(usize);
        impl Drop for LoadGuard {
            fn drop(&mut self) {
                if self.0 != 0 {
                    unsafe {
                        FreeLibrary(self.0 as *mut c_void);
                    }
                }
            }
        }
        let mut guard = LoadGuard(module as usize);
        macro_rules! symbol {
            ($name:literal,$ty:ty) => {{
                let address = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) }
                    .ok_or_else(|| vendor("symbol", concat!("missing DeckLink symbol ", $name)))?;
                // Pinned bridge.h fixes each exported calling convention and ABI.
                unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(address) }
            }};
        }
        let abi = symbol!("md_decklink_abi_version", AbiFn);
        if unsafe { abi() } != 1 {
            return Err(ReferenceOutputAdapterError::VersionMismatch {
                detail: "native DeckLink C ABI must be version 1".to_owned(),
            });
        }
        let version = symbol!("md_decklink_sdk_version", VersionFn);
        let mut version_bytes = [0u8; 96];
        if unsafe { version(version_bytes.as_mut_ptr(), 96) } != 0 {
            return Err(vendor("sdk-version", native_text(&version_bytes)));
        }
        let version = native_text(&version_bytes);
        if version != "12.0" {
            return Err(ReferenceOutputAdapterError::VersionMismatch {
                detail: format!("native DeckLink SDK {version} is outside pinned API 12.0"),
            });
        }
        let runtime = Self {
            module: module as usize,
            _image_lease: image_lease,
            sha256,
            version,
            discover: symbol!("md_decklink_discover", DiscoverFn),
            reference: symbol!("md_decklink_reference", ReferenceFn),
            open: symbol!("md_decklink_open", OpenFn),
            start: symbol!("md_decklink_start", StartFn),
            poll: symbol!("md_decklink_poll", PollFn),
            stop: symbol!("md_decklink_request_stop", StopFn),
            shutdown: symbol!("md_decklink_shutdown", ShutdownFn),
            schedule_vanc: symbol!("md_decklink_schedule_vanc", wire::ScheduleVancFn),
            capture_preflight: symbol!("md_decklink_capture_preflight", CapturePreflightFn),
            capture_open: symbol!("md_decklink_capture_open", CaptureOpenFn),
            capture_poll: symbol!("md_decklink_capture_poll", CapturePollFn),
            capture_start: symbol!("md_decklink_capture_start", StartFn),
            capture_shutdown: symbol!("md_decklink_capture_shutdown", ShutdownFn),
        };
        guard.0 = 0;
        Ok(Arc::new(runtime))
    }
}

/// Physical DeckLink SDI adapter backed by the native COM implementation.
///
/// The optional DLL is loaded by exact absolute path with system-only dependent
/// DLL search. Missing SDK bridge, driver, card, mode or reference remains a
/// structured unavailable outcome. No simulated provider is substituted.
pub struct DeckLinkReferenceOutputAdapter {
    runtime: Arc<NativeRuntime>,
    evidence: ReferenceOutputProviderEvidence,
    devices: HashMap<ReferenceOutputDeviceId, NativeDevice>,
    wire: Option<DeckLinkWireReadbackConfiguration>,
}
impl DeckLinkReferenceOutputAdapter {
    /// Load the separately built native bridge from a fixed absolute DLL path.
    pub fn load(path: &Path) -> Result<Self, ReferenceOutputAdapterError> {
        let runtime = NativeRuntime::load(path)?;
        let evidence = ReferenceOutputProviderEvidence {
            provider: ReferenceOutputProvider::DeckLink,
            adapter_version: format!("mondrian-decklink-abi1/{}", runtime.sha256),
            sdk_version: Some(runtime.version.clone()),
            driver_version: None,
            hardware_backed: false,
            availability: ReferenceOutputRuntimeAvailability::NoDevices,
        };
        Ok(Self {
            runtime,
            evidence,
            devices: HashMap::new(),
            wire: None,
        })
    }
    /// Bind an explicitly configured independent SDI receiver and canonical
    /// validation-only marker owner before discovery.
    pub fn with_wire_readback(
        mut self,
        configuration: DeckLinkWireReadbackConfiguration,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        configuration.validate()?;
        self.wire = Some(configuration);
        Ok(self)
    }
    /// Load the application-packaged image beside the current executable.
    pub fn load_packaged() -> Result<Self, ReferenceOutputAdapterError> {
        let executable =
            std::env::current_exe().map_err(|error| vendor("executable", error.to_string()))?;
        let directory = executable
            .parent()
            .ok_or_else(|| vendor("executable", "missing executable directory"))?;
        Self::load(&directory.join(NATIVE_DLL))
    }
    fn native_device(
        &self,
        device: &ReferenceOutputDeviceDescriptor,
    ) -> Result<NativeDevice, ReferenceOutputAdapterError> {
        let native = self
            .devices
            .get(&device.id)
            .ok_or(ReferenceOutputAdapterError::DeviceUnavailable)?;
        if device.provider != ReferenceOutputProvider::DeckLink
            || device.generation != native.generation
        {
            return Err(ReferenceOutputAdapterError::DeviceUnavailable);
        }
        Ok(*native)
    }
}
fn descriptor(
    native: &NativeDevice,
) -> Result<ReferenceOutputDeviceDescriptor, ReferenceOutputAdapterError> {
    if native.serial == 0
        || native.generation == 0
        || native.physical_group == 0
        || native.input_mode_mask & !0x7ff != 0
        || native.maximum_audio_channels > 64
        || native.mode_mask & !0x7ff != 0
        || native.flags & !3 != 0
    {
        return Err(vendor(
            "discovery",
            "native descriptor violates the fixed capability ABI",
        ));
    }
    let mut modes = Vec::new();
    let layouts = [AudioChannelLayout::Stereo, AudioChannelLayout::Surround71];
    for (index, (width, height, num, den)) in MODES.iter().copied().enumerate() {
        if native.mode_mask & (1 << index) == 0 {
            continue;
        }
        for layout in layouts {
            if layout.channel_count() as u32 > native.maximum_audio_channels {
                continue;
            }
            modes.push(ReferenceOutputMode {
                signal: ReferenceOutputSignal {
                    width,
                    height,
                    frame_rate: Rational::new(num, den),
                    scan: ReferenceOutputScan::Progressive,
                    pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
                    color_space: ColorSpace::Rec709,
                    range: ReferenceOutputRange::Legal,
                    hdr: None,
                    audio_layout: layout,
                },
                supports_hdr_signal: false,
                supports_static_hdr_metadata: false,
                supports_reference_status: native.flags & 2 != 0,
                // Raw VANC API availability alone cannot prove independent wire
                // reception. A bound receiver/nonce is required below.
                supports_ancillary: false,
                supports_ancillary_readback: false,
            });
        }
    }
    Ok(ReferenceOutputDeviceDescriptor {
        id: ReferenceOutputDeviceId::new(format!("decklink:{:016x}:sdi", native.serial))?,
        provider: ReferenceOutputProvider::DeckLink,
        display_name: native_text(&native.name),
        generation: native.generation,
        modes,
    })
}
impl ReferenceOutputAdapter for DeckLinkReferenceOutputAdapter {
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        &self.evidence
    }
    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
        self.devices.clear();
        self.evidence.hardware_backed = false;
        self.evidence.availability = ReferenceOutputRuntimeAvailability::NoDevices;
        let empty = NativeDevice {
            serial: 0,
            generation: 0,
            physical_group: 0,
            mode_mask: 0,
            input_mode_mask: 0,
            flags: 0,
            maximum_audio_channels: 0,
            driver: [0; 96],
            name: [0; 128],
        };
        let mut native = [empty; 64];
        let mut count = 0;
        let mut error = [0u8; ERROR_BYTES];
        let status = unsafe {
            (self.runtime.discover)(
                native.as_mut_ptr(),
                64,
                &mut count,
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        };
        if status != 0 {
            let detail = native_text(&error);
            self.evidence.availability = match status {
                -2 => ReferenceOutputRuntimeAvailability::DriverMissing { detail: detail.clone() },
                -4 => {
                    ReferenceOutputRuntimeAvailability::VersionMismatch { detail: detail.clone() }
                }
                _ => ReferenceOutputRuntimeAvailability::NoDevices,
            };
            return Err(native_failure("discovery", status, detail));
        }
        if count > 64 {
            return Err(vendor("discovery", "native device count overflow"));
        }
        if count == 0 {
            return Err(ReferenceOutputAdapterError::NoDevices);
        }
        let mut result = Vec::new();
        let mut driver = None;
        for item in &native[..count as usize] {
            let current = native_text(&item.driver);
            if current.trim().is_empty()
                || driver.as_ref().is_some_and(|previous| previous != &current)
            {
                return Err(vendor(
                    "discovery",
                    "DeckLink driver inventory has missing or inconsistent versions",
                ));
            }
            driver = Some(current);
            let device = descriptor(item)?;
            if self.devices.insert(device.id.clone(), *item).is_some() {
                return Err(vendor(
                    "discovery",
                    "duplicate DeckLink stable device identity",
                ));
            }
            result.push(device);
        }
        if let Some(wire) = &self.wire {
            for device in &mut result {
                let Some(native) = self.devices.get(&device.id) else {
                    continue;
                };
                if wire.preflight(&self.runtime, &self.devices, native.serial).is_ok() {
                    for mode in &mut device.modes {
                        if mode.signal == wire.signal {
                            mode.supports_ancillary = true;
                            mode.supports_ancillary_readback = true;
                        }
                    }
                }
            }
        }
        self.evidence.driver_version = driver;
        self.evidence.hardware_backed = true;
        self.evidence.availability = ReferenceOutputRuntimeAvailability::Available;
        Ok(result)
    }
    fn preflight_reference_lock(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
    ) -> Result<Option<bool>, ReferenceOutputAdapterError> {
        let native = self.native_device(device)?;
        if let Some(wire) = &self.wire {
            wire.preflight(&self.runtime, &self.devices, native.serial)?;
        }
        let mut locked = -1;
        let mut error = [0u8; ERROR_BYTES];
        if unsafe {
            (self.runtime.reference)(
                native.serial,
                native.generation,
                &mut locked,
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        } != 0
        {
            return Err(vendor("reference-preflight", native_text(&error)));
        }
        match locked {
            -1 => Ok(None),
            0 => Ok(Some(false)),
            1 => Ok(Some(true)),
            _ => Err(vendor("reference-preflight", "invalid native lock value")),
        }
    }
    fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
        let native = self.native_device(device)?;
        let mut admitted = descriptor(&native)?;
        if let Some(wire) = &self.wire {
            wire.preflight(&self.runtime, &self.devices, native.serial)?;
            for mode in &mut admitted.modes {
                if mode.signal == wire.signal {
                    mode.supports_ancillary = true;
                    mode.supports_ancillary_readback = true;
                }
            }
        }
        admitted.admit(request)?;
        if request.max_scheduled_frames > 64
            || request.preroll_frames < 2
            || request.preroll_frames >= request.max_scheduled_frames
        {
            return Err(ReferenceOutputAdapterError::ModeUnsupported);
        }
        let mode = MODES
            .iter()
            .position(|(w, h, n, d)| {
                *w == request.signal.width
                    && *h == request.signal.height
                    && Rational::new(*n, *d) == request.signal.frame_rate
            })
            .ok_or(ReferenceOutputAdapterError::ModeUnsupported)?;
        let native_request = NativeRequest {
            mode: mode as u32,
            channels: request.signal.audio_layout.channel_count() as u32,
            ancillary: u32::from(request.ancillary_policy.is_required()),
            require_reference: u32::from(
                request.reference_policy == ReferenceOutputReferencePolicy::RequireExternalLock,
            ),
            preroll: request.preroll_frames,
            max_frames: request.max_scheduled_frames,
        };
        let mut raw = std::ptr::null_mut();
        let mut error = [0u8; ERROR_BYTES];
        let result = unsafe {
            (self.runtime.open)(
                native.serial,
                native.generation,
                &native_request,
                &mut raw,
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        };
        if raw.is_null() {
            return Err(vendor("open", native_text(&error)));
        }
        let mut session = Box::new(DeckLinkSession {
            runtime: Arc::clone(&self.runtime),
            raw: Some(raw as usize),
            evidence: self.evidence.clone(),
            request: request.clone(),
            generation: native.generation,
            wire: None,
        });
        if result != 0 {
            let shutdown = crate::module::retire_rejected_reference_session(
                session,
                self.evidence.clone(),
                Instant::now() + Duration::from_secs(5),
            );
            return Err(ReferenceOutputAdapterError::VendorOpenRejected {
                detail: native_text(&error),
                shutdown: Box::new(shutdown),
            });
        }
        if request.ancillary_policy.is_required() {
            let opened = self
                .wire
                .as_ref()
                .ok_or(ReferenceOutputAdapterError::AncillaryNotEnabled)
                .and_then(|configuration| {
                    WireSession::open(
                        Arc::clone(&self.runtime),
                        configuration.clone(),
                        &self.devices,
                        native.serial,
                        mode as u32,
                        request.max_scheduled_frames,
                    )
                });
            match opened {
                Ok(wire) => {
                    let failure = wire.open_error().map(str::to_owned);
                    session.wire = Some(wire);
                    if let Some(detail) = failure {
                        let shutdown = crate::module::retire_rejected_reference_session(
                            session,
                            self.evidence.clone(),
                            Instant::now() + Duration::from_secs(5),
                        );
                        return Err(ReferenceOutputAdapterError::VendorOpenRejected {
                            detail,
                            shutdown: Box::new(shutdown),
                        });
                    }
                }
                Err(error) => {
                    let shutdown = crate::module::retire_rejected_reference_session(
                        session,
                        self.evidence.clone(),
                        Instant::now() + Duration::from_secs(5),
                    );
                    return Err(ReferenceOutputAdapterError::VendorOpenRejected {
                        detail: error.to_string(),
                        shutdown: Box::new(shutdown),
                    });
                }
            }
        }
        Ok(session)
    }
}
struct DeckLinkSession {
    runtime: Arc<NativeRuntime>,
    raw: Option<usize>,
    evidence: ReferenceOutputProviderEvidence,
    request: ReferenceOutputOpenRequest,
    generation: u64,
    wire: Option<WireSession>,
}
impl DeckLinkSession {
    fn raw(&self) -> Result<*mut c_void, ReferenceOutputAdapterError> {
        self.raw
            .map(|raw| raw as *mut c_void)
            .ok_or(ReferenceOutputAdapterError::SessionStopped)
    }
    fn consume(&mut self) -> ReferenceOutputSessionShutdownReceipt {
        let Some(raw) = self.raw.take() else {
            return ReferenceOutputSessionShutdownReceipt::never_opened();
        };
        let mut facts = NativeShutdown {
            requested: 0,
            stopped: 0,
            worker_joined: 0,
            released: 0,
            outstanding_frames: 0,
            outstanding_resources: 0,
            error: [0; 512],
        };
        let result = unsafe { (self.runtime.shutdown)(raw as *mut c_void, &mut facts) };
        if let Some(mut wire) = self.wire.take() {
            wire.consume_into(&mut facts);
        }
        if facts.worker_joined != 1 || facts.released != 1 || facts.outstanding_resources != 0 {
            std::mem::forget(Arc::clone(&self.runtime));
        }
        let error = native_text(&facts.error);
        ReferenceOutputSessionShutdownReceipt {
            schema_version: 2,
            session_present: true,
            shutdown_request_completed: facts.requested == 1,
            playback_stopped: facts.stopped == 1,
            callback_execution_terminated: facts.worker_joined == 1,
            device_released: facts.released == 1,
            outstanding_frames: facts.outstanding_frames,
            outstanding_resources: facts.outstanding_resources.max(u64::from(result != 0)),
            provider_failure: if result != 0 || !error.is_empty() {
                Some(ReferenceOutputProviderShutdownFailure::new(
                    "native-decklink-shutdown",
                    error,
                ))
            } else {
                None
            },
            coordinator: ReferenceOutputShutdownCoordinatorFacts::not_required(),
        }
    }
}
impl ReferenceOutputAdapterSession for DeckLinkSession {
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        &self.evidence
    }
    fn request(&self) -> &ReferenceOutputOpenRequest {
        &self.request
    }
    fn device_generation(&self) -> u64 {
        self.generation
    }
    fn schedule(
        &mut self,
        bundle: ReferenceOutputBundle,
    ) -> Result<(), ReferenceOutputAdapterError> {
        bundle.validate()?;
        if bundle.video.signal() != &self.request.signal {
            return Err(ReferenceOutputAdapterError::PayloadSignalMismatch);
        }
        if self.wire.is_none() && !bundle.ancillary.packets().is_empty() {
            return Err(ReferenceOutputAdapterError::AncillaryNotEnabled);
        }
        let video_bytes = u32::try_from(bundle.video.bytes().len())
            .map_err(|_| vendor("schedule", "video extent overflow"))?;
        let audio_samples = u32::try_from(bundle.audio.samples().len())
            .map_err(|_| vendor("schedule", "Audio extent overflow"))?;
        let mut error = [0u8; ERROR_BYTES];
        let packets = if let Some(wire) = &self.wire {
            wire.prepare(&bundle.ancillary)?
        } else {
            Vec::new()
        };
        let result = unsafe {
            (self.runtime.schedule_vanc)(
                self.raw()?,
                bundle.frame_index(),
                bundle.video.bytes().as_ptr(),
                video_bytes,
                bundle.video.row_bytes(),
                bundle.audio.samples().as_ptr(),
                audio_samples,
                packets.as_ptr(),
                packets.len() as u32,
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        };
        match result {
            0 => {
                if let Some(wire) = &mut self.wire {
                    wire.scheduled(bundle.ancillary)?;
                }
                Ok(())
            }
            1 => Err(ReferenceOutputAdapterError::Backpressure),
            _ => Err(vendor("schedule", native_text(&error))),
        }
    }
    fn start(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        if let Some(wire) = &mut self.wire {
            wire.start()?;
        }
        let mut error = [0u8; ERROR_BYTES];
        if unsafe { (self.runtime.start)(self.raw()?, error.as_mut_ptr(), ERROR_BYTES as u32) } != 0
        {
            return Err(vendor("start", native_text(&error)));
        }
        Ok(())
    }
    fn poll(&mut self) -> Result<Option<ReferenceOutputAdapterEvent>, ReferenceOutputAdapterError> {
        if let Some(wire) = &mut self.wire {
            wire.poll_capture()?;
            if let Some(event) = wire.ready()? {
                return Ok(Some(event));
            }
        }
        let mut event = NativeEvent::default();
        let mut error = [0u8; ERROR_BYTES];
        let result = unsafe {
            (self.runtime.poll)(
                self.raw()?,
                &mut event,
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        };
        if result == 0 {
            return Ok(None);
        }
        if result != 1 {
            return Err(vendor("poll", native_text(&error)));
        }
        let event = match event.kind {
            1 if event.ticks > 0 => ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: event.frame,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: event.ticks,
                    ticks_per_second: 48_000,
                }),
                ancillary_readback_sha256: None,
            },
            3 => ReferenceOutputAdapterEvent::FrameDropped { frame_index: event.frame },
            5 => ReferenceOutputAdapterEvent::ReferenceLockChanged { locked: true },
            6 => ReferenceOutputAdapterEvent::ReferenceLockChanged { locked: false },
            7 => ReferenceOutputAdapterEvent::DeviceLost,
            _ => {
                return Err(vendor(
                    "poll",
                    "invalid native event or missing hardware clock",
                ))
            }
        };
        if let Some(wire) = &mut self.wire
            && matches!(event, ReferenceOutputAdapterEvent::FrameCompleted { .. })
        {
            wire.completed(event)?;
            return wire.ready();
        }
        Ok(Some(event))
    }
    fn begin_shutdown(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        if unsafe { (self.runtime.stop)(self.raw()?) } != 0 {
            return Err(vendor("request-stop", "native stop request rejected"));
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        self.begin_shutdown()
    }
    fn shutdown(mut self: Box<Self>) -> ReferenceOutputSessionShutdownReceipt {
        self.consume()
    }
}
impl Drop for DeckLinkSession {
    fn drop(&mut self) {
        let Some(raw) = self.raw.take() else {
            return;
        };
        // Defensive abandonment never blocks the caller or unloads code still
        // used by a native callback worker. Normal product closure is explicit.
        let runtime = Arc::clone(&self.runtime);
        let abandoned = Box::new((runtime, raw));
        let pointer = Box::into_raw(abandoned) as usize;
        let spawn = std::thread::Builder::new()
            .name("decklink-abandoned-session".to_owned())
            .spawn(move || {
                let owner = unsafe { Box::from_raw(pointer as *mut (Arc<NativeRuntime>, usize)) };
                let mut facts = NativeShutdown {
                    requested: 0,
                    stopped: 0,
                    worker_joined: 0,
                    released: 0,
                    outstanding_frames: 0,
                    outstanding_resources: 0,
                    error: [0; 512],
                };
                unsafe {
                    (owner.0.stop)(owner.1 as *mut c_void);
                    (owner.0.shutdown)(owner.1 as *mut c_void, &mut facts);
                }
                if facts.worker_joined != 1
                    || facts.released != 1
                    || facts.outstanding_resources != 0
                {
                    std::mem::forget(Arc::clone(&owner.0));
                }
            });
        // On spawn failure the allocation intentionally retains both the native
        // owner and its DLL; a failed lifetime is never published as clean.
        drop(spawn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_decklink_c_abi_extents_match_the_checked_header() {
        assert_eq!(std::mem::size_of::<NativeDevice>(), 264);
        assert_eq!(std::mem::size_of::<NativeRequest>(), 24);
        assert_eq!(std::mem::size_of::<wire::NativeWirePacket>(), 540);
        assert_eq!(std::mem::size_of::<wire::NativeWireFrame>(), 34_576);
        assert_eq!(std::mem::size_of::<NativeEvent>(), 24);
        assert_eq!(std::mem::size_of::<NativeShutdown>(), 544);
    }
    #[test]
    fn native_decklink_never_advertises_unimplemented_carriers_or_wire_readback() {
        let native = NativeDevice {
            serial: 1,
            generation: 2,
            physical_group: 3,
            mode_mask: 1,
            input_mode_mask: 1,
            flags: 3,
            maximum_audio_channels: 8,
            driver: [0; 96],
            name: [0; 128],
        };
        let device = descriptor(&native).expect("native descriptor");
        assert_eq!(device.modes.len(), 2);
        assert!(device.modes.iter().all(|mode| !mode.supports_hdr_signal
            && !mode.supports_static_hdr_metadata
            && !mode.supports_ancillary
            && !mode.supports_ancillary_readback));
        assert!(descriptor(&NativeDevice { mode_mask: u32::MAX, ..native }).is_err());
        assert!(descriptor(&NativeDevice { physical_group: 0, ..native }).is_err());
        let input_only =
            descriptor(&NativeDevice { mode_mask: 0, ..native }).expect("input-only inventory");
        assert!(input_only.modes.is_empty());
        let stereo_only = descriptor(&NativeDevice { maximum_audio_channels: 2, ..native })
            .expect("two channels");
        assert_eq!(stereo_only.modes.len(), 1);
    }
    #[test]
    fn native_decklink_missing_driver_and_interface_remain_typed() {
        assert!(matches!(
            native_failure("discover", -2, "COM not registered".into()),
            ReferenceOutputAdapterError::DriverMissing { .. }
        ));
        assert!(matches!(
            native_failure("discover", -4, "API IID absent".into()),
            ReferenceOutputAdapterError::VersionMismatch { .. }
        ));
        assert!(matches!(
            native_failure("open", -3, "device gone".into()),
            ReferenceOutputAdapterError::DeviceUnavailable
        ));
    }
}
