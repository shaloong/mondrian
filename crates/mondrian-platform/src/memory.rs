//! Native whole-machine memory Adapter.
//!
//! Capacity classification and live pressure are separate observations. Each
//! platform Implementation preserves its native meaning and reports failure
//! instead of manufacturing a fallback number.

use mondrian_platform_core::{
    PhysicalMemoryCapacityProbeBackend, PhysicalMemoryCapacityProbeResult,
    SystemMemoryProbeBackend, SystemMemoryProbeResult,
};

pub(super) fn physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
    platform::physical_memory_capacity()
}

pub(super) fn system_memory() -> SystemMemoryProbeResult {
    platform::system_memory()
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    pub(super) fn physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
        let mut kib = 0_u64;
        // SAFETY: Windows writes one u64 to the valid pointer for the duration
        // of this call and retains no reference.
        let result = unsafe {
            windows_sys::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory(
                &mut kib,
            )
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

    pub(super) fn system_memory() -> SystemMemoryProbeResult {
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
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::BTreeMap;
    use std::fs;

    use super::*;

    pub(super) fn physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
        let backend = PhysicalMemoryCapacityProbeBackend::LinuxProcfsMemTotal;
        match meminfo().and_then(|values| required_bytes(&values, "MemTotal")) {
            Ok(bytes) => PhysicalMemoryCapacityProbeResult::observed(backend, bytes),
            Err(error) => PhysicalMemoryCapacityProbeResult::failed(backend, error),
        }
    }

    pub(super) fn system_memory() -> SystemMemoryProbeResult {
        let backend = SystemMemoryProbeBackend::LinuxProcfsMeminfo;
        let result = meminfo().and_then(|values| {
            let total = required_bytes(&values, "MemTotal")?;
            let available = required_bytes(&values, "MemAvailable")?;
            let used = total.saturating_sub(available);
            let load = if total == 0 {
                return Err(String::from("MemTotal must be non-zero"));
            } else {
                u32::try_from(used.saturating_mul(100) / total).unwrap_or(100)
            };
            Ok((total, available, load))
        });
        match result {
            Ok((total, available, load)) => {
                SystemMemoryProbeResult::observed(backend, total, available, load)
            }
            Err(error) => SystemMemoryProbeResult::failed(backend, error),
        }
    }

    fn meminfo() -> Result<BTreeMap<String, u64>, String> {
        let contents = fs::read_to_string("/proc/meminfo").map_err(|error| error.to_string())?;
        parse_meminfo(&contents)
    }

    fn parse_meminfo(contents: &str) -> Result<BTreeMap<String, u64>, String> {
        let mut values = BTreeMap::new();
        for line in contents.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let mut fields = value.split_whitespace();
            let Some(kib) = fields.next() else {
                continue;
            };
            let Ok(kib) = kib.parse::<u64>() else {
                continue;
            };
            if fields.next() != Some("kB") {
                continue;
            }
            let bytes =
                kib.checked_mul(1024).ok_or_else(|| format!("{name} byte count overflowed"))?;
            values.insert(name.to_owned(), bytes);
        }
        Ok(values)
    }

    fn required_bytes(values: &BTreeMap<String, u64>, name: &str) -> Result<u64, String> {
        values
            .get(name)
            .copied()
            .ok_or_else(|| format!("/proc/meminfo did not contain {name}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_required_meminfo_units() {
            let values = parse_meminfo("MemTotal: 8192 kB\nMemAvailable: 2048 kB\n")
                .expect("meminfo should parse");
            assert_eq!(
                required_bytes(&values, "MemTotal").expect("total"),
                8 * 1024 * 1024
            );
            assert_eq!(
                required_bytes(&values, "MemAvailable").expect("available"),
                2 * 1024 * 1024
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CStr;
    use std::mem::{self, MaybeUninit};

    use super::*;

    pub(super) fn physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
        let backend = PhysicalMemoryCapacityProbeBackend::MacOsHwMemsizeSysctl;
        match sysctl_u64(c"hw.memsize") {
            Ok(bytes) if bytes > 0 => PhysicalMemoryCapacityProbeResult::observed(backend, bytes),
            Ok(_) => PhysicalMemoryCapacityProbeResult::failed(backend, "hw.memsize was zero"),
            Err(error) => PhysicalMemoryCapacityProbeResult::failed(backend, error),
        }
    }

    pub(super) fn system_memory() -> SystemMemoryProbeResult {
        let backend = SystemMemoryProbeBackend::MacOsMachHostStatistics;
        match mach_memory() {
            Ok((total, available, load)) => {
                SystemMemoryProbeResult::observed(backend, total, available, load)
            }
            Err(error) => SystemMemoryProbeResult::failed(backend, error),
        }
    }

    fn mach_memory() -> Result<(u64, u64, u32), String> {
        let total = sysctl_u64(c"hw.memsize")?;
        let page_size = sysctl_u64(c"hw.pagesize")?;
        if total == 0 || page_size == 0 {
            return Err(String::from(
                "macOS reported zero total memory or page size",
            ));
        }
        let mut statistics = MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `statistics` is a correctly sized writable vm_statistics64
        // buffer and Mach retains no pointer after returning.
        let result = unsafe {
            libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_VM_INFO64,
                statistics.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if result != libc::KERN_SUCCESS {
            return Err(format!("host_statistics64 failed with Mach code {result}"));
        }
        if count < libc::HOST_VM_INFO64_COUNT {
            return Err(format!(
                "host_statistics64 returned {count} words, expected {}",
                libc::HOST_VM_INFO64_COUNT
            ));
        }
        // SAFETY: Mach reported a complete structure.
        let statistics = unsafe { statistics.assume_init() };
        let available_pages = u64::from(statistics.free_count)
            .checked_add(u64::from(statistics.inactive_count))
            .and_then(|value| value.checked_add(u64::from(statistics.speculative_count)))
            .ok_or_else(|| String::from("available-page count overflowed"))?;
        let available = available_pages
            .checked_mul(page_size)
            .ok_or_else(|| String::from("available-memory byte count overflowed"))?
            .min(total);
        let load = u32::try_from(total.saturating_sub(available).saturating_mul(100) / total)
            .unwrap_or(100);
        Ok((total, available, load))
    }

    fn sysctl_u64(name: &CStr) -> Result<u64, String> {
        let mut value = 0_u64;
        let mut size = mem::size_of::<u64>();
        // SAFETY: `value` and `size` describe a writable u64 buffer; sysctl
        // retains no pointer after returning.
        let result = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(format!(
                "sysctl {} failed: {}",
                name.to_string_lossy(),
                std::io::Error::last_os_error()
            ));
        }
        if size != mem::size_of::<u64>() {
            return Err(format!(
                "sysctl {} returned {size} bytes",
                name.to_string_lossy()
            ));
        }
        Ok(value)
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    pub(super) fn physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
        PhysicalMemoryCapacityProbeResult::unsupported(
            "installed physical memory discovery is not implemented for this platform",
        )
    }

    pub(super) fn system_memory() -> SystemMemoryProbeResult {
        SystemMemoryProbeResult::unsupported(
            "native whole-system memory discovery is not implemented for this platform",
        )
    }
}
