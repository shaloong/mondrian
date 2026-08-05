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

/// Explicit ownership scope for one native process-memory observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMemoryScope {
    /// Only the process that invoked the Adapter.
    CurrentProcess,
    /// The invoking Mondrian process and every descendant in one verified OS
    /// process-tree inventory.
    ProductProcessTree,
}

impl ProcessMemoryScope {
    /// Stable scope label for reports and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CurrentProcess => "current-process",
            Self::ProductProcessTree => "product-process-tree",
        }
    }
}

/// Native backend used to observe a process-memory footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMemoryProbeBackend {
    /// Windows Process Status API for exactly the calling process.
    WindowsCurrentProcessStatus,
    /// Windows Tool Help process-tree inventory plus Process Status queries for
    /// every verified member.
    WindowsToolhelpProcessTree,
    /// Linux `/proc/self/status` counters for exactly the calling process.
    LinuxCurrentProcessStatus,
    /// Linux `/proc` process inventory plus per-process status counters.
    LinuxProcfsProcessTree,
    /// macOS `proc_pid_rusage` counters for exactly the calling process.
    MacOsCurrentProcessRusage,
    /// macOS `libproc` inventory plus `proc_pid_rusage` counters.
    MacOsLibprocProcessTree,
}

impl ProcessMemoryProbeBackend {
    /// Stable backend label for structured diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsCurrentProcessStatus => "windows-current-process-status",
            Self::WindowsToolhelpProcessTree => "windows-toolhelp-process-tree-status",
            Self::LinuxCurrentProcessStatus => "linux-current-process-status",
            Self::LinuxProcfsProcessTree => "linux-procfs-process-tree-status",
            Self::MacOsCurrentProcessRusage => "macos-current-process-rusage",
            Self::MacOsLibprocProcessTree => "macos-libproc-process-tree-rusage",
        }
    }

    /// Ownership scope this backend can prove.
    pub fn scope(self) -> ProcessMemoryScope {
        match self {
            Self::WindowsCurrentProcessStatus => ProcessMemoryScope::CurrentProcess,
            Self::WindowsToolhelpProcessTree => ProcessMemoryScope::ProductProcessTree,
            Self::LinuxCurrentProcessStatus | Self::MacOsCurrentProcessRusage => {
                ProcessMemoryScope::CurrentProcess
            }
            Self::LinuxProcfsProcessTree | Self::MacOsLibprocProcessTree => {
                ProcessMemoryScope::ProductProcessTree
            }
        }
    }

    /// Platform-native private-footprint metric returned by this backend.
    pub fn private_memory_metric(self) -> ProcessPrivateMemoryMetric {
        match self {
            Self::WindowsCurrentProcessStatus | Self::WindowsToolhelpProcessTree => {
                ProcessPrivateMemoryMetric::WindowsPrivateCommit
            }
            Self::LinuxCurrentProcessStatus | Self::LinuxProcfsProcessTree => {
                ProcessPrivateMemoryMetric::LinuxAnonymousResident
            }
            Self::MacOsCurrentProcessRusage | Self::MacOsLibprocProcessTree => {
                ProcessPrivateMemoryMetric::MacOsPhysicalFootprint
            }
        }
    }
}

/// Meaning of the platform-native private-footprint byte counter.
///
/// These metrics are intentionally not numerically interchangeable. They are
/// suitable for same-platform plateau and budget evidence, while reports must
/// preserve the metric whenever samples cross a persistence or telemetry Seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPrivateMemoryMetric {
    /// Windows private committed virtual memory (`PrivateUsage`).
    WindowsPrivateCommit,
    /// Linux anonymous resident memory (`RssAnon`).
    LinuxAnonymousResident,
    /// macOS physical footprint reported by `proc_pid_rusage`.
    MacOsPhysicalFootprint,
}

impl ProcessPrivateMemoryMetric {
    /// Stable metric label for evidence and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsPrivateCommit => "windows-private-commit",
            Self::LinuxAnonymousResident => "linux-anonymous-resident",
            Self::MacOsPhysicalFootprint => "macos-physical-footprint",
        }
    }
}

/// Point-in-time scoped memory facts from a native operating-system API.
///
/// `private_memory_bytes` is the platform-native acceptance-grade
/// leak/plateau metric identified by `private_memory_metric`. Resident-set values remain diagnostic because the
/// operating system may reclaim shared or file-backed pages independently of
/// application lifetime. A professional whole-product gate must additionally
/// require `ProductProcessTree`, a complete inventory, and a non-zero observed
/// process count; a successful current-process sample is not interchangeable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMemoryProbeResult {
    /// Ownership scope requested from the Adapter.
    pub scope: ProcessMemoryScope,
    /// Whether this platform has a process-memory implementation.
    pub discovery_available: bool,
    /// Native API that produced the sample.
    pub backend: Option<ProcessMemoryProbeBackend>,
    /// Number of processes whose counters contributed to the aggregate.
    pub observed_process_count: u32,
    /// Number of bounded inventory attempts consumed by the Adapter.
    pub inventory_attempts: u32,
    /// Whether the Adapter proved a stable inventory and queried every member.
    pub inventory_complete: bool,
    /// Semantics of `private_memory_bytes`, when a complete sample is present.
    pub private_memory_metric: Option<ProcessPrivateMemoryMetric>,
    /// Aggregate platform-native private-footprint bytes in the declared scope.
    pub private_memory_bytes: Option<u64>,
    /// Aggregate current physical resident-set or working-set bytes.
    pub resident_bytes: Option<u64>,
    /// Checked sum of member peak resident-set or working-set bytes.
    pub peak_resident_bytes: Option<u64>,
    /// Structured failure reason when no complete sample was produced.
    pub error: Option<String>,
}

impl ProcessMemoryProbeResult {
    /// Build a complete native sample.
    pub fn observed(
        scope: ProcessMemoryScope,
        backend: ProcessMemoryProbeBackend,
        observed_process_count: u32,
        inventory_attempts: u32,
        private_memory_bytes: u64,
        resident_bytes: u64,
        peak_resident_bytes: u64,
    ) -> Self {
        Self::observed_with_optional_peak(
            scope,
            backend,
            observed_process_count,
            inventory_attempts,
            private_memory_bytes,
            resident_bytes,
            Some(peak_resident_bytes),
        )
    }

    /// Build a complete native sample when the platform exposes no truthful
    /// lifetime peak-resident counter for arbitrary process-tree members.
    pub fn observed_with_optional_peak(
        scope: ProcessMemoryScope,
        backend: ProcessMemoryProbeBackend,
        observed_process_count: u32,
        inventory_attempts: u32,
        private_memory_bytes: u64,
        resident_bytes: u64,
        peak_resident_bytes: Option<u64>,
    ) -> Self {
        Self {
            scope,
            discovery_available: true,
            backend: Some(backend),
            observed_process_count,
            inventory_attempts,
            inventory_complete: true,
            private_memory_metric: Some(backend.private_memory_metric()),
            private_memory_bytes: Some(private_memory_bytes),
            resident_bytes: Some(resident_bytes),
            peak_resident_bytes,
            error: None,
        }
    }

    /// Build a supported-backend query failure.
    pub fn failed(
        scope: ProcessMemoryScope,
        backend: ProcessMemoryProbeBackend,
        observed_process_count: u32,
        inventory_attempts: u32,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            scope,
            discovery_available: true,
            backend: Some(backend),
            observed_process_count,
            inventory_attempts,
            inventory_complete: false,
            private_memory_metric: None,
            private_memory_bytes: None,
            resident_bytes: None,
            peak_resident_bytes: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a platform without an implementation.
    pub fn unsupported(scope: ProcessMemoryScope, reason: impl Into<String>) -> Self {
        Self {
            scope,
            discovery_available: false,
            backend: None,
            observed_process_count: 0,
            inventory_attempts: 0,
            inventory_complete: false,
            private_memory_metric: None,
            private_memory_bytes: None,
            resident_bytes: None,
            peak_resident_bytes: None,
            error: Some(reason.into()),
        }
    }

    /// Whether this is one internally coherent, complete sample for `scope`.
    pub fn is_complete_for(&self, scope: ProcessMemoryScope) -> bool {
        self.scope == scope
            && self.discovery_available
            && self.inventory_complete
            && self.observed_process_count > 0
            && self.inventory_attempts > 0
            && self.backend.is_some_and(|backend| backend.scope() == scope)
            && self.private_memory_metric
                == self.backend.map(ProcessMemoryProbeBackend::private_memory_metric)
            && self.private_memory_bytes.is_some()
            && self.resident_bytes.is_some()
            && self.error.is_none()
    }
}

/// Interface for native scoped process-memory observation.
pub trait ProcessMemoryProbe: Send + Sync {
    /// Observe the requested ownership scope without mutating product policy.
    fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult;

    /// Observe only the calling process.
    fn current_process_memory(&self) -> ProcessMemoryProbeResult {
        self.process_memory(ProcessMemoryScope::CurrentProcess)
    }

    /// Observe the Mondrian process plus its complete descendant process tree.
    fn product_process_tree_memory(&self) -> ProcessMemoryProbeResult {
        self.process_memory(ProcessMemoryScope::ProductProcessTree)
    }
}

/// Native backend used to discover physically installed memory capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalMemoryCapacityProbeBackend {
    /// Windows `GetPhysicallyInstalledSystemMemory`.
    WindowsInstalledSystemMemory,
    /// Linux `/proc/meminfo` `MemTotal` capacity.
    LinuxProcfsMemTotal,
    /// macOS `hw.memsize` sysctl capacity.
    MacOsHwMemsizeSysctl,
}

impl PhysicalMemoryCapacityProbeBackend {
    /// Stable backend label for structured diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsInstalledSystemMemory => "windows-installed-system-memory",
            Self::LinuxProcfsMemTotal => "linux-procfs-mem-total",
            Self::MacOsHwMemsizeSysctl => "macos-hw-memsize-sysctl",
        }
    }
}

/// Stable machine-capacity evidence used to classify the 8/16/32 GiB tiers.
///
/// This is deliberately separate from OS-visible or currently available
/// memory. Firmware-reserved memory must not move a nominal 8 GiB machine
/// below the supported product floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalMemoryCapacityProbeResult {
    /// Whether this platform has an installed-capacity implementation.
    pub discovery_available: bool,
    /// Native API that produced the observation.
    pub backend: Option<PhysicalMemoryCapacityProbeBackend>,
    /// Physically installed bytes, when successfully observed.
    pub installed_physical_bytes: Option<u64>,
    /// Structured failure or unsupported reason.
    pub error: Option<String>,
}

impl PhysicalMemoryCapacityProbeResult {
    /// Build a complete native capacity observation.
    pub fn observed(
        backend: PhysicalMemoryCapacityProbeBackend,
        installed_physical_bytes: u64,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            installed_physical_bytes: Some(installed_physical_bytes),
            error: None,
        }
    }

    /// Build a supported-backend query failure.
    pub fn failed(backend: PhysicalMemoryCapacityProbeBackend, reason: impl Into<String>) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            installed_physical_bytes: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a platform without an implementation.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            backend: None,
            installed_physical_bytes: None,
            error: Some(reason.into()),
        }
    }
}

/// Interface for stable physically installed memory discovery.
pub trait PhysicalMemoryCapacityProbe: Send + Sync {
    /// Observe installed capacity without interpreting runtime pressure.
    fn physical_memory_capacity(&self) -> PhysicalMemoryCapacityProbeResult;
}

/// Native backend used to observe whole-system physical-memory pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMemoryProbeBackend {
    /// Windows `GlobalMemoryStatusEx`.
    WindowsGlobalMemoryStatus,
    /// Linux `/proc/meminfo` `MemTotal` and `MemAvailable`.
    LinuxProcfsMeminfo,
    /// macOS Mach host virtual-memory statistics plus `hw.memsize`.
    MacOsMachHostStatistics,
}

impl SystemMemoryProbeBackend {
    /// Stable backend label for structured diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsGlobalMemoryStatus => "windows-global-memory-status",
            Self::LinuxProcfsMeminfo => "linux-procfs-meminfo",
            Self::MacOsMachHostStatistics => "macos-mach-host-statistics",
        }
    }
}

/// Point-in-time whole-system physical-memory facts.
///
/// This observation includes memory consumed by decoder/encoder child
/// processes and unrelated applications. It complements, but never replaces,
/// current-process private-commit evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemMemoryProbeResult {
    /// Whether this platform has a whole-system memory implementation.
    pub discovery_available: bool,
    /// Native API that produced the sample.
    pub backend: Option<SystemMemoryProbeBackend>,
    /// Total physical memory visible to the operating system.
    pub total_physical_bytes: Option<u64>,
    /// Physical memory currently available without paging.
    pub available_physical_bytes: Option<u64>,
    /// Operating-system memory load in the inclusive range 0–100.
    pub memory_load_percent: Option<u32>,
    /// Structured failure reason when no complete sample was produced.
    pub error: Option<String>,
}

impl SystemMemoryProbeResult {
    /// Build a complete native sample.
    pub fn observed(
        backend: SystemMemoryProbeBackend,
        total_physical_bytes: u64,
        available_physical_bytes: u64,
        memory_load_percent: u32,
    ) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            total_physical_bytes: Some(total_physical_bytes),
            available_physical_bytes: Some(available_physical_bytes),
            memory_load_percent: Some(memory_load_percent.min(100)),
            error: None,
        }
    }

    /// Build a supported-backend query failure.
    pub fn failed(backend: SystemMemoryProbeBackend, reason: impl Into<String>) -> Self {
        Self {
            discovery_available: true,
            backend: Some(backend),
            total_physical_bytes: None,
            available_physical_bytes: None,
            memory_load_percent: None,
            error: Some(reason.into()),
        }
    }

    /// Build a result for a platform without an implementation.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            discovery_available: false,
            backend: None,
            total_physical_bytes: None,
            available_physical_bytes: None,
            memory_load_percent: None,
            error: Some(reason.into()),
        }
    }
}

/// Interface for native whole-system physical-memory observation.
pub trait SystemMemoryProbe: Send + Sync {
    /// Observe current system capacity without mutating application policy.
    fn current_system_memory(&self) -> SystemMemoryProbeResult;
}

/// Combined memory observation boundary consumed by product resource policy.
pub trait ExecutionMemoryProbe: ProcessMemoryProbe + SystemMemoryProbe {}

impl<T> ExecutionMemoryProbe for T where T: ProcessMemoryProbe + SystemMemoryProbe + ?Sized {}

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

impl ProcessMemoryProbe for NoopPlatformService {
    fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
        ProcessMemoryProbeResult::unsupported(scope, "process-memory discovery is not configured")
    }
}

impl PhysicalMemoryCapacityProbe for NoopPlatformService {
    fn physical_memory_capacity(&self) -> PhysicalMemoryCapacityProbeResult {
        PhysicalMemoryCapacityProbeResult::unsupported(
            "installed-memory discovery is not configured",
        )
    }
}

impl SystemMemoryProbe for NoopPlatformService {
    fn current_system_memory(&self) -> SystemMemoryProbeResult {
        SystemMemoryProbeResult::unsupported("system-memory discovery is not configured")
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
    fn process_memory_backends_preserve_scope_and_metric() {
        let cases = [
            (
                ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
                ProcessMemoryScope::CurrentProcess,
                ProcessPrivateMemoryMetric::WindowsPrivateCommit,
            ),
            (
                ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                ProcessMemoryScope::ProductProcessTree,
                ProcessPrivateMemoryMetric::WindowsPrivateCommit,
            ),
            (
                ProcessMemoryProbeBackend::LinuxCurrentProcessStatus,
                ProcessMemoryScope::CurrentProcess,
                ProcessPrivateMemoryMetric::LinuxAnonymousResident,
            ),
            (
                ProcessMemoryProbeBackend::LinuxProcfsProcessTree,
                ProcessMemoryScope::ProductProcessTree,
                ProcessPrivateMemoryMetric::LinuxAnonymousResident,
            ),
            (
                ProcessMemoryProbeBackend::MacOsCurrentProcessRusage,
                ProcessMemoryScope::CurrentProcess,
                ProcessPrivateMemoryMetric::MacOsPhysicalFootprint,
            ),
            (
                ProcessMemoryProbeBackend::MacOsLibprocProcessTree,
                ProcessMemoryScope::ProductProcessTree,
                ProcessPrivateMemoryMetric::MacOsPhysicalFootprint,
            ),
        ];

        for (backend, scope, metric) in cases {
            assert_eq!(backend.scope(), scope);
            assert_eq!(backend.private_memory_metric(), metric);
            let sample = ProcessMemoryProbeResult::observed_with_optional_peak(
                scope, backend, 1, 1, 1024, 2048, None,
            );
            assert!(sample.is_complete_for(scope));
            assert_eq!(sample.private_memory_metric, Some(metric));
        }
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
