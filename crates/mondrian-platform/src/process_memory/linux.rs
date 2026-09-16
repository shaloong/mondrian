//! Linux `/proc` process-memory Adapter.
//!
//! A product-tree sample is accepted only when two full PID/start-time/group
//! inventories of live address spaces match. This rejects PID reuse, live-member
//! exit, and newly visible descendants instead of silently undercounting them.
//! Confirmed zombie/dead tasks have released their user address space; they stay
//! in ancestry discovery but cannot supply RSS counters. This says nothing about
//! whether the separate process owner has consumed their wait status.

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
    process_group: u32,
    start_ticks: u64,
    address_space_exited: bool,
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
        if let Some(identity) = inventory_identity(read_process_identity(pid))
            .map_err(|error| format!("could not inventory process {pid}: {error}"))?
        {
            all.insert(pid, identity);
        }
    }

    let root = all
        .get(&root_pid)
        .copied()
        .ok_or_else(|| format!("root process {root_pid} was absent from /proc inventory"))?;
    let mut members = BTreeSet::from([root_pid]);
    let mut owned_groups = BTreeSet::new();
    loop {
        let previous = (members.len(), owned_groups.len());
        for (&pid, identity) in &all {
            if members.contains(&identity.parent_pid)
                || owned_groups.contains(&identity.process_group)
            {
                members.insert(pid);
            }
            // Native helper owners retain their group leader until the group
            // exits. Its ancestry anchors live members even after reparenting.
            // The root may share a terminal/job group with unrelated peers;
            // only a descendant leader establishes this ownership boundary.
            if pid != root_pid && members.contains(&pid) && identity.process_group == pid {
                owned_groups.insert(pid);
            }
        }
        if (members.len(), owned_groups.len()) == previous {
            break;
        }
    }

    let mut result = BTreeMap::new();
    result.insert(root_pid, root);
    for pid in members {
        if let Some(identity) = all.get(&pid)
            && !identity.address_space_exited
        {
            result.insert(pid, *identity);
        }
    }
    Ok(result)
}

#[derive(Debug, thiserror::Error)]
enum ProcessIdentityReadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid Linux process identity record: {0}")]
    Record(String),
}

fn inventory_identity(
    result: Result<ProcessIdentity, ProcessIdentityReadError>,
) -> Result<Option<ProcessIdentity>, ProcessIdentityReadError> {
    match result {
        Ok(identity) => Ok(Some(identity)),
        Err(ProcessIdentityReadError::Io(error))
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn read_process_identity(pid: u32) -> Result<ProcessIdentity, ProcessIdentityReadError> {
    let path = PathBuf::from("/proc").join(pid.to_string()).join("stat");
    let contents = fs::read(&path)?;
    parse_stat_identity(&contents).map_err(ProcessIdentityReadError::Record)
}

fn parse_stat_identity(contents: &[u8]) -> Result<ProcessIdentity, String> {
    // comm is an opaque kernel byte string, not a UTF-8 path or identifier.
    // Only the numeric/state suffix participates in the identity contract.
    let closing = contents
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or_else(|| String::from("/proc stat command name is unterminated"))?;
    let suffix = std::str::from_utf8(&contents[closing + 1..])
        .map_err(|error| format!("invalid process identity suffix: {error}"))?;
    let fields: Vec<&str> = suffix.split_whitespace().collect();
    let parent_pid = fields
        .get(1)
        .ok_or_else(|| String::from("/proc stat has no parent PID"))?
        .parse::<u32>()
        .map_err(|error| format!("invalid parent PID: {error}"))?;
    let process_group = fields
        .get(2)
        .ok_or_else(|| String::from("/proc stat has no process group"))?
        .parse::<u32>()
        .map_err(|error| format!("invalid process group: {error}"))?;
    let start_ticks = fields
        .get(19)
        .ok_or_else(|| String::from("/proc stat has no start time"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid process start time: {error}"))?;
    let state = fields.first().ok_or_else(|| String::from("/proc stat has no state"))?;
    let threads = fields
        .get(17)
        .ok_or_else(|| String::from("/proc stat has no thread count"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid thread count: {error}"))?;
    Ok(ProcessIdentity {
        parent_pid,
        process_group,
        start_ticks,
        // A zombie group leader may still have live sibling threads. Missing
        // counters in that case remain unknown, never an invented zero sample.
        address_space_exited: matches!(*state, "Z" | "X" | "x") && threads == 1,
    })
}

fn read_status(path: &Path) -> io::Result<ProcessCounters> {
    let contents = fs::read(path)?;
    let private_bytes = status_kib(&contents, "RssAnon:")?;
    let resident_bytes = status_kib(&contents, "VmRSS:")?;
    let peak_resident_bytes = status_kib(&contents, "VmHWM:")?;
    Ok(ProcessCounters { private_bytes, resident_bytes, peak_resident_bytes })
}

fn status_kib(contents: &[u8], key: &str) -> io::Result<u64> {
    let line = contents
        .split(|byte| *byte == b'\n')
        .find(|line| line.starts_with(key.as_bytes()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {key}")))?;
    let value = std::str::from_utf8(&line[key.len()..])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut fields = value.split_whitespace();
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
    // These fixtures share the test process as their inventory root. Their
    // ownership transitions must not invalidate another fixture's stable scan.
    static NATIVE_PROCESS_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn root_group_does_not_claim_an_unrelated_peer() {
        let _fixture = NATIVE_PROCESS_FIXTURE.lock().expect("exclusive native fixture ownership");
        use std::os::unix::process::CommandExt;
        use std::process::Command;
        let mut root = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("native root fixture");
        let peer = Command::new("/bin/sleep").arg("30").process_group(root.id() as i32).spawn();
        let inventory = product_tree_inventory(root.id());
        let mut peer = match peer {
            Ok(peer) => peer,
            Err(error) => {
                root.kill().expect("terminate root after failed peer startup");
                root.wait().expect("reap root after failed peer startup");
                panic!("native peer fixture: {error}");
            }
        };
        root.kill().expect("terminate owned root fixture");
        peer.kill().expect("terminate owned peer fixture");
        root.wait().expect("reap root fixture");
        peer.wait().expect("reap peer fixture");
        let inventory = inventory.expect("native root inventory");
        assert_eq!(
            inventory.len(),
            1,
            "same terminal group does not prove product ancestry"
        );
        assert!(inventory.contains_key(&root.id()));
        assert!(!inventory.contains_key(&peer.id()));
    }

    #[test]
    fn exited_owned_group_leader_does_not_hide_live_descendant_memory() {
        let _fixture = NATIVE_PROCESS_FIXTURE.lock().expect("exclusive native fixture ownership");
        use std::io::{BufRead, BufReader};
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let mut leader = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & printf '%s\\n' $!; exit 0"])
            .process_group(0)
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .expect("owned launcher fixture");
        let mut line = String::new();
        let readiness =
            BufReader::new(leader.stdout.take().expect("descendant PID pipe")).read_line(&mut line);
        let descendant = line.trim().parse::<u32>();
        let deadline = Instant::now() + Duration::from_secs(2);
        let exited = loop {
            if read_process_identity(leader.id()).is_ok_and(|record| record.address_space_exited) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let inventory = product_tree_inventory(std::process::id());
        // The unreaped leader still pins this fixture's numeric group; never
        // signal another process or let the launcher release the identity first.
        let killed = unsafe { libc::kill(-(leader.id() as libc::pid_t), libc::SIGKILL) };
        let reaped = leader.wait();
        readiness.expect("read descendant identity");
        assert_eq!(killed, 0, "terminate owned descendant group");
        reaped.expect("reap owned launcher");
        assert!(exited, "fixture must reach exited/unreaped launcher state");
        let descendant = descendant.expect("native descendant PID");
        assert!(
            inventory.expect("product inventory").contains_key(&descendant),
            "a live member of an owned process group must remain counted after launcher exit"
        );
    }

    #[test]
    fn opaque_status_name_does_not_relax_numeric_counter_validation() {
        let status = b"Name:\tworker\xff\nRssAnon:\t10 kB\n";
        assert_eq!(
            status_kib(status, "RssAnon:").expect("numeric counter"),
            10 * 1024
        );
        let malformed = status_kib(b"RssAnon:\t1\xff kB\n", "RssAnon:");
        assert_eq!(
            malformed.expect_err("malformed numeric field").kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn only_disappeared_inventory_records_can_be_omitted() {
        for code in [libc::ENOENT, libc::ESRCH] {
            assert!(matches!(
                inventory_identity(Err(ProcessIdentityReadError::Io(
                    io::Error::from_raw_os_error(code)
                ))),
                Ok(None)
            ));
        }
        assert!(inventory_identity(Err(ProcessIdentityReadError::Record(
            "malformed stat".to_owned()
        )))
        .is_err());
    }

    #[test]
    fn unreadable_inventory_record_cannot_be_silently_omitted() {
        for code in [libc::EACCES, libc::EPERM, libc::EIO] {
            let observed = inventory_identity(Err(ProcessIdentityReadError::Io(
                io::Error::from_raw_os_error(code),
            )));
            assert!(
                observed.is_err(),
                "inventory omitted an unreadable record for OS error {code}"
            );
        }
    }

    #[test]
    fn non_utf8_child_name_remains_in_inventory_and_memory_sample() {
        let _fixture = NATIVE_PROCESS_FIXTURE.lock().expect("exclusive native fixture ownership");
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "printf '\\377owned' > /proc/$$/comm; printf 'ready\\n'; read -r line",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("owned native child");
        let pid = child.id();
        let mut ready = String::new();
        let readiness =
            BufReader::new(child.stdout.take().expect("readiness pipe")).read_line(&mut ready);
        let name = fs::read(format!("/proc/{pid}/comm"));
        let inventory = product_tree_inventory(std::process::id());
        let sample = sample_tree_once(pid);
        child.kill().expect("terminate native fixture");
        child.wait().expect("reap native fixture before assertions");
        readiness.expect("read readiness");
        assert_eq!(ready, "ready\n");
        assert!(
            name.expect("native task name").contains(&0xff),
            "fixture must use a real non-UTF8 task name"
        );
        assert!(
            inventory.expect("product inventory").contains_key(&pid),
            "a legal native task name must not hide a live product child"
        );
        let (count, private, resident, _) = sample.expect("native memory counters remain readable");
        assert_eq!(count, 1);
        assert!(resident >= private);
    }

    #[test]
    fn exited_unreaped_child_does_not_invalidate_live_memory_inventory() {
        let _fixture = NATIVE_PROCESS_FIXTURE.lock().expect("exclusive native fixture ownership");
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("owned disposable child");
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(2);
        let observed_zombie = loop {
            let status =
                fs::read_to_string(format!("/proc/{pid}/status")).expect("owned child status");
            if status.lines().any(|line| {
                line.starts_with("State:") && line.split_whitespace().nth(1) == Some("Z")
            }) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let inventory = product_tree_inventory(std::process::id());
        let sample = sample_tree_once(std::process::id());
        child.wait().expect("reap child even if assertion fails");
        assert!(
            observed_zombie,
            "fixture must really reach exited/unreaped state"
        );
        assert!(
            sample.is_ok(),
            "a confirmed exited address space has no RSS fields: {sample:?}"
        );
        assert!(
            !inventory.expect("inventory").contains_key(&pid),
            "memory inventory must count live address spaces, not unreaped PID ownership"
        );
    }

    #[test]
    fn zombie_group_leader_cannot_hide_live_thread_memory() {
        let mut fields = ["0"; 20];
        fields[0] = "Z";
        fields[1] = "12";
        fields[17] = "2"; // num_threads: the leader exited but another thread lives.
        fields[19] = "991";
        let identity = parse_stat_identity(format!("42 (worker) {}", fields.join(" ")).as_bytes())
            .expect("zombie leader stat");
        assert!(
            !identity.address_space_exited,
            "task state alone cannot prove process memory exited"
        );
    }

    #[test]
    fn stat_parser_survives_spaces_and_parentheses_in_command_name() {
        let mut suffix = vec!["S", "12"];
        suffix.extend(std::iter::repeat_n("0", 17));
        suffix.push("991");
        let stat = format!("42 (render worker (copy)) {}", suffix.join(" "));
        assert_eq!(
            parse_stat_identity(stat.as_bytes()).expect("stat should parse"),
            ProcessIdentity {
                parent_pid: 12,
                process_group: 0,
                start_ticks: 991,
                address_space_exited: false
            }
        );
    }

    #[test]
    fn status_parser_preserves_metric_meaning() {
        let status = "VmHWM:\t30 kB\nVmRSS:\t20 kB\nRssAnon:\t10 kB\n";
        let path = Path::new("unused");
        assert_eq!(
            status_kib(status.as_bytes(), "RssAnon:").expect("value"),
            10 * 1024
        );
        let _ = path;
    }
}
