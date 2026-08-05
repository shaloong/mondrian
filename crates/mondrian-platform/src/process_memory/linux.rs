//! Linux `/proc` process-memory Adapter.
//!
//! A product-tree sample is accepted only when two full PID/start-time
//! inventories match. This rejects PID reuse, member exit, and newly visible
//! descendants instead of silently undercounting them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mondrian_platform_core::{
    ProcessMemoryProbeBackend, ProcessMemoryProbeResult, ProcessMemoryScope,
};

const PROCESS_TREE_MAX_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    parent_pid: u32,
    start_ticks: u64,
}

#[derive(Debug, Clone, Copy)]
struct ProcessCounters {
    private_bytes: u64,
    resident_bytes: u64,
    peak_resident_bytes: u64,
}

pub(super) fn process_memory(scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
    match scope {
        ProcessMemoryScope::CurrentProcess => current_process_memory(),
        ProcessMemoryScope::ProductProcessTree => product_process_tree_memory(),
    }
}

fn current_process_memory() -> ProcessMemoryProbeResult {
    let backend = ProcessMemoryProbeBackend::LinuxCurrentProcessStatus;
    match read_status(Path::new("/proc/self/status")) {
        Ok(sample) => ProcessMemoryProbeResult::observed(
            ProcessMemoryScope::CurrentProcess,
            backend,
            1,
            1,
            sample.private_bytes,
            sample.resident_bytes,
            sample.peak_resident_bytes,
        ),
        Err(error) => ProcessMemoryProbeResult::failed(
            ProcessMemoryScope::CurrentProcess,
            backend,
            0,
            1,
            format!("could not read /proc/self/status: {error}"),
        ),
    }
}

fn product_process_tree_memory() -> ProcessMemoryProbeResult {
    let backend = ProcessMemoryProbeBackend::LinuxProcfsProcessTree;
    let root_pid = std::process::id();
    let mut last_error = String::from("no inventory attempt completed");
    let mut last_count = 0_u32;

    for attempt in 1..=PROCESS_TREE_MAX_ATTEMPTS {
        match sample_tree_once(root_pid) {
            Ok((count, private_bytes, resident_bytes, peak_resident_bytes)) => {
                return ProcessMemoryProbeResult::observed(
                    ProcessMemoryScope::ProductProcessTree,
                    backend,
                    count,
                    attempt,
                    private_bytes,
                    resident_bytes,
                    peak_resident_bytes,
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

fn sample_tree_once(root_pid: u32) -> Result<(u32, u64, u64, u64), (u32, String)> {
    let before = product_tree_inventory(root_pid).map_err(|error| (0, error))?;
    let count = u32::try_from(before.len()).unwrap_or(u32::MAX);
    let mut private_bytes = 0_u64;
    let mut resident_bytes = 0_u64;
    let mut peak_resident_bytes = 0_u64;

    for (pid, identity) in &before {
        let stat = read_process_identity(*pid).map_err(|error| {
            (
                count,
                format!("could not revalidate product process {pid}: {error}"),
            )
        })?;
        if stat != *identity {
            return Err((count, format!("product process {pid} changed identity")));
        }
        let status_path = PathBuf::from("/proc").join(pid.to_string()).join("status");
        let counters = read_status(&status_path).map_err(|error| {
            (
                count,
                format!("could not read product process {pid} counters: {error}"),
            )
        })?;
        private_bytes = private_bytes.checked_add(counters.private_bytes).ok_or_else(|| {
            (
                count,
                String::from("product private-memory aggregate overflowed"),
            )
        })?;
        resident_bytes = resident_bytes.checked_add(counters.resident_bytes).ok_or_else(|| {
            (
                count,
                String::from("product resident-memory aggregate overflowed"),
            )
        })?;
        peak_resident_bytes =
            peak_resident_bytes.checked_add(counters.peak_resident_bytes).ok_or_else(|| {
                (
                    count,
                    String::from("product peak-resident aggregate overflowed"),
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

    Ok((count, private_bytes, resident_bytes, peak_resident_bytes))
}

fn product_tree_inventory(root_pid: u32) -> Result<BTreeMap<u32, ProcessIdentity>, String> {
    let mut all = BTreeMap::new();
    for entry in fs::read_dir("/proc").map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(identity) = read_process_identity(pid) {
            all.insert(pid, identity);
        }
    }

    let root = all
        .get(&root_pid)
        .copied()
        .ok_or_else(|| format!("root process {root_pid} was absent from /proc inventory"))?;
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

fn read_process_identity(pid: u32) -> Result<ProcessIdentity, String> {
    let path = PathBuf::from("/proc").join(pid.to_string()).join("stat");
    let contents = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    parse_stat_identity(&contents)
}

fn parse_stat_identity(contents: &str) -> Result<ProcessIdentity, String> {
    let closing = contents
        .rfind(')')
        .ok_or_else(|| String::from("/proc stat command name is unterminated"))?;
    let fields: Vec<&str> = contents[closing + 1..].split_whitespace().collect();
    let parent_pid = fields
        .get(1)
        .ok_or_else(|| String::from("/proc stat has no parent PID"))?
        .parse::<u32>()
        .map_err(|error| format!("invalid parent PID: {error}"))?;
    let start_ticks = fields
        .get(19)
        .ok_or_else(|| String::from("/proc stat has no start time"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid process start time: {error}"))?;
    Ok(ProcessIdentity { parent_pid, start_ticks })
}

fn read_status(path: &Path) -> io::Result<ProcessCounters> {
    let contents = fs::read_to_string(path)?;
    let private_bytes = status_kib(&contents, "RssAnon:")?;
    let resident_bytes = status_kib(&contents, "VmRSS:")?;
    let peak_resident_bytes = status_kib(&contents, "VmHWM:")?;
    Ok(ProcessCounters { private_bytes, resident_bytes, peak_resident_bytes })
}

fn status_kib(contents: &str, key: &str) -> io::Result<u64> {
    let line = contents
        .lines()
        .find(|line| line.starts_with(key))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {key}")))?;
    let mut fields = line[key.len()..].split_whitespace();
    let kib = fields
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("empty {key}")))?
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if fields.next() != Some("kB") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{key} is not expressed in kB"),
        ));
    }
    kib.checked_mul(1024).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{key} byte count overflowed"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parser_survives_spaces_and_parentheses_in_command_name() {
        let mut suffix = vec!["S", "12"];
        suffix.extend(std::iter::repeat_n("0", 17));
        suffix.push("991");
        let stat = format!("42 (render worker (copy)) {}", suffix.join(" "));
        assert_eq!(
            parse_stat_identity(&stat).expect("stat should parse"),
            ProcessIdentity { parent_pid: 12, start_ticks: 991 }
        );
    }

    #[test]
    fn status_parser_preserves_metric_meaning() {
        let status = "VmHWM:\t30 kB\nVmRSS:\t20 kB\nRssAnon:\t10 kB\n";
        let path = Path::new("unused");
        assert_eq!(status_kib(status, "RssAnon:").expect("value"), 10 * 1024);
        let _ = path;
    }
}
