//! macOS `libproc` process-memory Adapter.
//!
//! `ri_phys_footprint` is preserved as a macOS-specific metric. It is not
//! relabelled as Windows private commit. Product-tree membership is accepted
//! only after two matching PID/start-time inventories.

use std::collections::{BTreeMap, BTreeSet};
use std::mem::{self, MaybeUninit};

use mondrian_platform_core::{
    ProcessMemoryProbeBackend, ProcessMemoryProbeResult, ProcessMemoryScope,
};

const PROC_ALL_PIDS: u32 = 1;
const PROCESS_TREE_MAX_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    parent_pid: u32,
    start_seconds: u64,
    start_microseconds: u64,
}

#[derive(Debug, Clone, Copy)]
struct ProcessCounters {
    private_bytes: u64,
    resident_bytes: u64,
}

pub(super) fn process_memory(scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
    match scope {
        ProcessMemoryScope::CurrentProcess => current_process_memory(),
        ProcessMemoryScope::ProductProcessTree => product_process_tree_memory(),
    }
}

fn current_process_memory() -> ProcessMemoryProbeResult {
    let backend = ProcessMemoryProbeBackend::MacOsCurrentProcessRusage;
    let pid = std::process::id();
    match query_counters(pid) {
        Ok(sample) => ProcessMemoryProbeResult::observed_with_optional_peak(
            ProcessMemoryScope::CurrentProcess,
            backend,
            1,
            1,
            sample.private_bytes,
            sample.resident_bytes,
            None,
        ),
        Err(error) => ProcessMemoryProbeResult::failed(
            ProcessMemoryScope::CurrentProcess,
            backend,
            0,
            1,
            error,
        ),
    }
}

fn product_process_tree_memory() -> ProcessMemoryProbeResult {
    let backend = ProcessMemoryProbeBackend::MacOsLibprocProcessTree;
    let root_pid = std::process::id();
    let mut last_error = String::from("no inventory attempt completed");
    let mut last_count = 0_u32;
    for attempt in 1..=PROCESS_TREE_MAX_ATTEMPTS {
        match sample_tree_once(root_pid) {
            Ok((count, private_bytes, resident_bytes)) => {
                return ProcessMemoryProbeResult::observed_with_optional_peak(
                    ProcessMemoryScope::ProductProcessTree,
                    backend,
                    count,
                    attempt,
                    private_bytes,
                    resident_bytes,
                    None,
                );
            }
            Err((count, error)) => {
                last_count = count;
                last_error = error;
            }
        }
        std::thread::yield_now();
    }
    ProcessMemoryProbeResult::failed(
        ProcessMemoryScope::ProductProcessTree,
        backend,
        last_count,
        PROCESS_TREE_MAX_ATTEMPTS,
        last_error,
    )
}

fn sample_tree_once(root_pid: u32) -> Result<(u32, u64, u64), (u32, String)> {
    let before = product_tree_inventory(root_pid).map_err(|error| (0, error))?;
    let count = u32::try_from(before.len()).unwrap_or(u32::MAX);
    let mut private_bytes = 0_u64;
    let mut resident_bytes = 0_u64;
    for (&pid, identity) in &before {
        if query_identity(pid).map_err(|error| (count, error))? != *identity {
            return Err((count, format!("product process {pid} changed identity")));
        }
        let counters = query_counters(pid).map_err(|error| (count, error))?;
        private_bytes = private_bytes.checked_add(counters.private_bytes).ok_or_else(|| {
            (
                count,
                String::from("product physical-footprint aggregate overflowed"),
            )
        })?;
        resident_bytes = resident_bytes.checked_add(counters.resident_bytes).ok_or_else(|| {
            (
                count,
                String::from("product resident-memory aggregate overflowed"),
            )
        })?;
    }
    let after = product_tree_inventory(root_pid).map_err(|error| (count, error))?;
    if before != after {
        return Err((
            u32::try_from(after.len()).unwrap_or(u32::MAX),
            String::from("product process-tree membership changed between inventory passes"),
        ));
    }
    Ok((count, private_bytes, resident_bytes))
}

fn product_tree_inventory(root_pid: u32) -> Result<BTreeMap<u32, ProcessIdentity>, String> {
    let all_pids = list_all_pids()?;
    let mut all = BTreeMap::new();
    for pid in all_pids {
        if let Ok(identity) = query_identity(pid) {
            all.insert(pid, identity);
        }
    }
    let root = all
        .get(&root_pid)
        .copied()
        .ok_or_else(|| format!("root process {root_pid} was absent from libproc inventory"))?;
    let mut members = BTreeSet::from([root_pid]);
    loop {
        let previous_len = members.len();
        for (&pid, identity) in &all {
            if members.contains(&identity.parent_pid) {
                members.insert(pid);
            }
        }
        if members.len() == previous_len {
            break;
        }
    }
    let mut result = BTreeMap::new();
    result.insert(root_pid, root);
    for pid in members {
        if let Some(identity) = all.get(&pid) {
            result.insert(pid, *identity);
        }
    }
    Ok(result)
}

fn list_all_pids() -> Result<Vec<u32>, String> {
    // SAFETY: A null buffer asks libproc for the required byte capacity.
    let required = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if required <= 0 {
        return Err(format!(
            "proc_listpids capacity query failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let element_size = mem::size_of::<libc::pid_t>();
    let capacity = (usize::try_from(required).map_err(|error| error.to_string())? / element_size)
        .saturating_add(64);
    let mut pids = vec![0 as libc::pid_t; capacity];
    let buffer_bytes = pids
        .len()
        .checked_mul(element_size)
        .and_then(|bytes| i32::try_from(bytes).ok())
        .ok_or_else(|| String::from("libproc PID buffer exceeds supported size"))?;
    // SAFETY: The vector exposes `buffer_bytes` writable bytes and libproc
    // retains no pointer after returning.
    let written =
        unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, pids.as_mut_ptr().cast(), buffer_bytes) };
    if written < 0 {
        return Err(format!(
            "proc_listpids failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let count = usize::try_from(written).map_err(|error| error.to_string())? / element_size;
    pids.truncate(count);
    Ok(pids
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .collect())
}

fn query_identity(pid: u32) -> Result<ProcessIdentity, String> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size =
        i32::try_from(mem::size_of::<libc::proc_bsdinfo>()).map_err(|error| error.to_string())?;
    // SAFETY: `info` is a correctly sized writable proc_bsdinfo buffer.
    let written = unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).map_err(|error| error.to_string())?,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return Err(format!(
            "proc_pidinfo({pid}) returned {written} bytes, expected {size}"
        ));
    }
    // SAFETY: libproc reported that the complete structure was initialized.
    let info = unsafe { info.assume_init() };
    Ok(ProcessIdentity {
        parent_pid: info.pbi_ppid,
        start_seconds: info.pbi_start_tvsec,
        start_microseconds: info.pbi_start_tvusec,
    })
}

fn query_counters(pid: u32) -> Result<ProcessCounters, String> {
    let mut usage = MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: The pointer addresses a writable rusage_info_v2 buffer and
    // proc_pid_rusage retains no reference after returning.
    let result = unsafe {
        libc::proc_pid_rusage(
            i32::try_from(pid).map_err(|error| error.to_string())?,
            libc::RUSAGE_INFO_V2,
            usage.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        return Err(format!(
            "proc_pid_rusage({pid}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: A zero result initializes the requested rusage structure.
    let usage = unsafe { usage.assume_init() };
    Ok(ProcessCounters {
        private_bytes: usage.ri_phys_footprint,
        resident_bytes: usage.ri_resident_size,
    })
}
