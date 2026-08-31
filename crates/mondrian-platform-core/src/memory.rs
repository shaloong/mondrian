//! Platform-neutral process and system memory observation contracts.

use crate::NoopPlatformService;
use serde::{Deserialize, Serialize};

/// Explicit ownership scope for one native process-memory observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
/// `private_memory_bytes` is the platform-native acceptance-grade leak/plateau
/// metric identified by `private_memory_metric`. Resident-set values remain
/// diagnostic because the operating system may reclaim shared or file-backed
/// pages independently of application lifetime. A professional whole-product
/// gate must additionally require `ProductProcessTree`, a complete inventory,
/// and a non-zero observed process count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
}
