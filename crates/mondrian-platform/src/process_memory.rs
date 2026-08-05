//! Native scoped process-memory observation.
//!
//! Current-process and product-process-tree samples deliberately share one
//! Interface but never one claim. The Windows tree Adapter validates two
//! consecutive Tool Help inventories and retains every queried process handle
//! through the second inventory, so PID reuse, process exit, or a newly visible
//! descendant makes the bounded attempt fail instead of silently undercounting.

use mondrian_platform_core::{
    ProcessMemoryProbeBackend, ProcessMemoryProbeResult, ProcessMemoryScope,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub(super) fn system_process_memory(scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
    #[cfg(target_os = "windows")]
    {
        match scope {
            ProcessMemoryScope::CurrentProcess => windows_current_process_memory(),
            ProcessMemoryScope::ProductProcessTree => windows_product_process_tree_memory(),
        }
    }

    #[cfg(target_os = "linux")]
    {
        linux::process_memory(scope)
    }

    #[cfg(target_os = "macos")]
    {
        macos::process_memory(scope)
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        ProcessMemoryProbeResult::unsupported(
            scope,
            format!(
                "native {} memory discovery is not implemented for this platform",
                scope.as_str()
            ),
        )
    }
}

#[cfg(target_os = "windows")]
const PROCESS_TREE_MAX_ATTEMPTS: u32 = 4;

#[cfg(target_os = "windows")]
const SNAPSHOT_MAX_ATTEMPTS: u32 = 3;

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeProcessMemoryCounters {
    pid: u32,
    private_committed_bytes: u64,
    resident_bytes: u64,
    peak_resident_bytes: u64,
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct StableProductProcessTreeSample {
    inventory_attempts: u32,
    processes: Vec<NativeProcessMemoryCounters>,
    private_committed_bytes: u64,
    resident_bytes: u64,
    peak_resident_bytes: u64,
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct ProductProcessTreeProbeError {
    observed_process_count: usize,
    reason: String,
}

#[cfg(target_os = "windows")]
impl ProductProcessTreeProbeError {
    fn new(observed_process_count: usize, reason: impl Into<String>) -> Self {
        Self { observed_process_count, reason: reason.into() }
    }
}

#[cfg(target_os = "windows")]
fn windows_current_process_memory() -> ProcessMemoryProbeResult {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: GetCurrentProcess returns a process-local pseudo handle that is
    // always valid for the lifetime of the calling process and must not be
    // closed. The query retains no reference to it.
    match query_process_memory(unsafe { GetCurrentProcess() }, std::process::id()) {
        Ok(counters) => ProcessMemoryProbeResult::observed(
            ProcessMemoryScope::CurrentProcess,
            ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
            1,
            1,
            counters.private_committed_bytes,
            counters.resident_bytes,
            counters.peak_resident_bytes,
        ),
        Err(reason) => ProcessMemoryProbeResult::failed(
            ProcessMemoryScope::CurrentProcess,
            ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
            0,
            1,
            reason,
        ),
    }
}

#[cfg(target_os = "windows")]
fn windows_product_process_tree_memory() -> ProcessMemoryProbeResult {
    match stable_product_process_tree_sample() {
        Ok(sample) => {
            let process_count = match u32::try_from(sample.processes.len()) {
                Ok(process_count) => process_count,
                Err(_) => {
                    return ProcessMemoryProbeResult::failed(
                        ProcessMemoryScope::ProductProcessTree,
                        ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                        u32::MAX,
                        sample.inventory_attempts,
                        "product process tree contains more members than the evidence schema can represent",
                    );
                }
            };
            ProcessMemoryProbeResult::observed(
                ProcessMemoryScope::ProductProcessTree,
                ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                process_count,
                sample.inventory_attempts,
                sample.private_committed_bytes,
                sample.resident_bytes,
                sample.peak_resident_bytes,
            )
        }
        Err((attempts, error)) => ProcessMemoryProbeResult::failed(
            ProcessMemoryScope::ProductProcessTree,
            ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
            u32::try_from(error.observed_process_count).unwrap_or(u32::MAX),
            attempts,
            error.reason,
        ),
    }
}

#[cfg(target_os = "windows")]
fn stable_product_process_tree_sample(
) -> Result<StableProductProcessTreeSample, (u32, ProductProcessTreeProbeError)> {
    let root_pid = std::process::id();
    let mut last_error = ProductProcessTreeProbeError::new(0, "no inventory attempt completed");
    for attempt in 1..=PROCESS_TREE_MAX_ATTEMPTS {
        match sample_product_process_tree_once(root_pid, attempt) {
            Ok(sample) => return Ok(sample),
            Err(error) => last_error = error,
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Err((PROCESS_TREE_MAX_ATTEMPTS, last_error))
}

#[cfg(target_os = "windows")]
fn sample_product_process_tree_once(
    root_pid: u32,
    inventory_attempts: u32,
) -> Result<StableProductProcessTreeSample, ProductProcessTreeProbeError> {
    let before = process_tree_inventory(root_pid)?;
    let expected_count = before.len();
    let mut queried = Vec::with_capacity(expected_count);
    for &pid in before.keys() {
        let handle = open_process_for_memory(pid).map_err(|reason| {
            ProductProcessTreeProbeError::new(
                expected_count,
                format!("could not open product process {pid}: {reason}"),
            )
        })?;
        let counters = query_process_memory(handle.raw(), pid).map_err(|reason| {
            ProductProcessTreeProbeError::new(
                expected_count,
                format!("could not query product process {pid}: {reason}"),
            )
        })?;
        queried.push(QueriedProcessMemory { handle, counters });
    }

    let after = process_tree_inventory(root_pid)?;
    if before != after {
        return Err(ProductProcessTreeProbeError::new(
            after.len(),
            "product process-tree membership changed between bounded inventory passes",
        ));
    }
    for process in &queried {
        match process.handle.is_active() {
            Ok(true) => {}
            Ok(false) => {
                return Err(ProductProcessTreeProbeError::new(
                    after.len(),
                    format!(
                        "product process {} exited while its memory sample was being validated",
                        process.counters.pid
                    ),
                ));
            }
            Err(reason) => {
                return Err(ProductProcessTreeProbeError::new(
                    after.len(),
                    format!(
                        "could not validate liveness of product process {}: {reason}",
                        process.counters.pid
                    ),
                ));
            }
        }
    }

    let processes = queried.into_iter().map(|process| process.counters).collect::<Vec<_>>();
    let (private_committed_bytes, resident_bytes, peak_resident_bytes) =
        checked_aggregate_process_memory(&processes)
            .map_err(|reason| ProductProcessTreeProbeError::new(processes.len(), reason))?;
    Ok(StableProductProcessTreeSample {
        inventory_attempts,
        processes,
        private_committed_bytes,
        resident_bytes,
        peak_resident_bytes,
    })
}

#[cfg(target_os = "windows")]
fn checked_aggregate_process_memory(
    processes: &[NativeProcessMemoryCounters],
) -> Result<(u64, u64, u64), &'static str> {
    let mut private_committed_bytes = 0_u64;
    let mut resident_bytes = 0_u64;
    let mut peak_resident_bytes = 0_u64;
    for process in processes {
        private_committed_bytes = private_committed_bytes
            .checked_add(process.private_committed_bytes)
            .ok_or("product process-tree private-commit aggregation overflowed")?;
        resident_bytes = resident_bytes
            .checked_add(process.resident_bytes)
            .ok_or("product process-tree working-set aggregation overflowed")?;
        peak_resident_bytes = peak_resident_bytes
            .checked_add(process.peak_resident_bytes)
            .ok_or("product process-tree peak-working-set aggregation overflowed")?;
    }
    Ok((private_committed_bytes, resident_bytes, peak_resident_bytes))
}

#[cfg(target_os = "windows")]
fn process_tree_inventory(
    root_pid: u32,
) -> Result<std::collections::BTreeMap<u32, u32>, ProductProcessTreeProbeError> {
    let entries = snapshot_process_entries()
        .map_err(|reason| ProductProcessTreeProbeError::new(0, reason))?;
    descendant_inventory(root_pid, &entries)
}

#[cfg(target_os = "windows")]
fn descendant_inventory(
    root_pid: u32,
    entries: &[(u32, u32)],
) -> Result<std::collections::BTreeMap<u32, u32>, ProductProcessTreeProbeError> {
    use std::collections::{BTreeMap, BTreeSet};

    let all = entries.iter().copied().collect::<BTreeMap<_, _>>();
    let Some(&root_parent) = all.get(&root_pid) else {
        return Err(ProductProcessTreeProbeError::new(
            0,
            format!("current process {root_pid} was absent from the OS process snapshot"),
        ));
    };
    let mut members = BTreeSet::from([root_pid]);
    loop {
        let before = members.len();
        for (&pid, &parent_pid) in &all {
            if members.contains(&parent_pid) {
                members.insert(pid);
            }
        }
        if members.len() == before {
            break;
        }
    }
    let mut inventory = BTreeMap::new();
    inventory.insert(root_pid, root_parent);
    for pid in members {
        if pid != root_pid {
            let parent_pid = all.get(&pid).copied().ok_or_else(|| {
                ProductProcessTreeProbeError::new(
                    inventory.len(),
                    format!("product process {pid} disappeared from the captured inventory"),
                )
            })?;
            inventory.insert(pid, parent_pid);
        }
    }
    Ok(inventory)
}

#[cfg(target_os = "windows")]
fn snapshot_process_entries() -> Result<Vec<(u32, u32)>, String> {
    use std::mem;
    use windows_sys::Win32::Foundation::{ERROR_BAD_LENGTH, ERROR_NO_MORE_FILES};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = {
        let mut last_error = None;
        let mut opened = None;
        for _ in 0..SNAPSHOT_MAX_ATTEMPTS {
            // SAFETY: This call takes scalar inputs and returns an owned handle.
            let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if let Some(handle) = OwnedWindowsHandle::new(raw) {
                opened = Some(handle);
                break;
            }
            let error = std::io::Error::last_os_error();
            let retryable = error.raw_os_error() == Some(ERROR_BAD_LENGTH as i32);
            last_error = Some(error);
            if !retryable {
                break;
            }
            std::thread::yield_now();
        }
        opened.ok_or_else(|| {
            format!(
                "CreateToolhelp32Snapshot failed after {SNAPSHOT_MAX_ATTEMPTS} bounded attempts: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "unknown OS error".to_owned())
            )
        })?
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    // SAFETY: `entry` is correctly sized and writable for the duration of the
    // call; `snapshot` owns a live process snapshot handle.
    if unsafe { Process32FirstW(snapshot.raw(), &mut entry) } == 0 {
        return Err(format!(
            "Process32FirstW failed with OS error {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut entries = Vec::new();
    loop {
        entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: Same valid snapshot and writable entry contract as above.
        if unsafe { Process32NextW(snapshot.raw(), &mut entry) } != 0 {
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            break;
        }
        return Err(format!("Process32NextW failed with OS error {error}"));
    }
    Ok(entries)
}

#[cfg(target_os = "windows")]
fn open_process_for_memory(pid: u32) -> Result<OwnedWindowsHandle, String> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_VM_READ,
    };

    // SAFETY: OpenProcess takes scalar inputs and returns a separately owned
    // handle. Inheritance is disabled.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    OwnedWindowsHandle::new(raw).ok_or_else(|| {
        format!(
            "OpenProcess failed with OS error {}",
            std::io::Error::last_os_error()
        )
    })
}

#[cfg(target_os = "windows")]
fn query_process_memory(
    handle: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
) -> Result<NativeProcessMemoryCounters, String> {
    use std::mem;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };

    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    // SAFETY: The handle has process-query/read access, the output points to a
    // correctly sized writable structure, and Windows retains neither pointer.
    let result = unsafe {
        GetProcessMemoryInfo(
            handle,
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if result == 0 {
        return Err(format!(
            "GetProcessMemoryInfo failed with OS error {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(NativeProcessMemoryCounters {
        pid,
        private_committed_bytes: counters.PrivateUsage as u64,
        resident_bytes: counters.WorkingSetSize as u64,
        peak_resident_bytes: counters.PeakWorkingSetSize as u64,
    })
}

#[cfg(target_os = "windows")]
struct QueriedProcessMemory {
    handle: OwnedWindowsHandle,
    counters: NativeProcessMemoryCounters,
}

#[cfg(target_os = "windows")]
struct OwnedWindowsHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl OwnedWindowsHandle {
    fn new(raw: windows_sys::Win32::Foundation::HANDLE) -> Option<Self> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

        (!raw.is_null() && raw != INVALID_HANDLE_VALUE).then_some(Self(raw))
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0
    }

    fn is_active(&self) -> Result<bool, String> {
        use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        // SAFETY: The handle is owned by `self` and remains live throughout
        // this non-blocking status query.
        match unsafe { WaitForSingleObject(self.0, 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            WAIT_FAILED => Err(format!(
                "WaitForSingleObject failed with OS error {}",
                std::io::Error::last_os_error()
            )),
            status => Err(format!(
                "WaitForSingleObject returned unexpected status {status}"
            )),
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for OwnedWindowsHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is an owned, non-null, non-pseudo handle closed
        // exactly once here.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    const CHILD_MODE_ENV: &str = "MONDRIAN_PROCESS_MEMORY_CHILD_MODE";
    const CHILD_READY: &str = "MONDRIAN_PROCESS_MEMORY_CHILD_READY";
    const CHILD_PRIVATE_ALLOCATION_BYTES: usize = 32 * 1024 * 1024;

    #[test]
    fn synthetic_inventory_recursively_includes_only_root_descendants() {
        let inventory = descendant_inventory(
            10,
            &[(1, 0), (10, 1), (11, 10), (12, 11), (20, 1), (21, 20)],
        )
        .expect("synthetic process tree");

        assert_eq!(
            inventory.keys().copied().collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
        assert_eq!(inventory.get(&12), Some(&11));
    }

    #[test]
    fn synthetic_aggregate_fails_closed_on_any_counter_overflow() {
        let processes = [
            NativeProcessMemoryCounters {
                pid: 1,
                private_committed_bytes: u64::MAX,
                resident_bytes: 1,
                peak_resident_bytes: 1,
            },
            NativeProcessMemoryCounters {
                pid: 2,
                private_committed_bytes: 1,
                resident_bytes: 1,
                peak_resident_bytes: 1,
            },
        ];

        assert!(checked_aggregate_process_memory(&processes).is_err());
    }

    #[test]
    fn windows_memory_handle_supports_nonblocking_liveness_validation() {
        let handle =
            open_process_for_memory(std::process::id()).expect("open current process for memory");

        assert!(
            handle.is_active().expect("query current-process liveness"),
            "the running test process must remain active"
        );
    }

    #[test]
    fn windows_process_tree_probe_child_helper() {
        if std::env::var_os(CHILD_MODE_ENV).is_none() {
            return;
        }

        let mut allocation = vec![0_u8; CHILD_PRIVATE_ALLOCATION_BYTES];
        for page in allocation.chunks_mut(4096) {
            page[0] = 0x5a;
        }
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{CHILD_READY}").expect("announce child readiness");
        stdout.flush().expect("flush child readiness");
        drop(stdout);
        let mut release = [0_u8; 1];
        std::io::stdin().read_exact(&mut release).expect("parent releases child");
        std::hint::black_box(allocation);
    }

    #[test]
    fn windows_product_process_tree_includes_child_private_commit() {
        let mut guard = ChildGuard::spawn();
        guard.wait_until_ready();
        let child_pid = guard.pid();

        let sample = stable_product_process_tree_sample().expect("stable process-tree sample");
        let child = sample
            .processes
            .iter()
            .find(|process| process.pid == child_pid)
            .expect("spawned child must be present in the verified process tree");
        let recomputed = checked_aggregate_process_memory(&sample.processes)
            .expect("sample counters aggregate without overflow");
        assert_eq!(
            recomputed,
            (
                sample.private_committed_bytes,
                sample.resident_bytes,
                sample.peak_resident_bytes,
            )
        );
        assert!(
            child.private_committed_bytes >= CHILD_PRIVATE_ALLOCATION_BYTES as u64,
            "child allocation must be privately committed: {child:?}"
        );
        assert!(sample.processes.iter().any(|process| process.pid == std::process::id()));
        assert!(sample.processes.len() >= 2);

        let public = windows_product_process_tree_memory();
        assert!(
            public.is_complete_for(ProcessMemoryScope::ProductProcessTree),
            "{:?}",
            public.error
        );
        assert!(public.observed_process_count >= 2);
        assert_eq!(
            public.backend,
            Some(ProcessMemoryProbeBackend::WindowsToolhelpProcessTree)
        );
        guard.release_and_wait();
    }

    struct ChildGuard {
        child: Option<Child>,
        ready: mpsc::Receiver<()>,
    }

    impl ChildGuard {
        fn spawn() -> Self {
            let mut child = Command::new(std::env::current_exe().expect("current test executable"))
                .arg("windows_process_tree_probe_child_helper")
                .arg("--nocapture")
                .env(CHILD_MODE_ENV, "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn process-memory child probe");
            let stdout = child.stdout.take().expect("capture child stdout");
            let (ready_tx, ready) = mpsc::channel();
            std::thread::spawn(move || {
                let mut announced = false;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if !announced && line.contains(CHILD_READY) {
                        let _ = ready_tx.send(());
                        announced = true;
                    }
                }
            });
            Self { child: Some(child), ready }
        }

        fn wait_until_ready(&self) {
            self.ready
                .recv_timeout(Duration::from_secs(10))
                .expect("child allocation becomes ready");
        }

        fn pid(&self) -> u32 {
            self.child.as_ref().expect("live child").id()
        }

        fn release_and_wait(&mut self) {
            let mut child = self.child.take().expect("live child");
            child.stdin.take().expect("child stdin").write_all(&[1]).expect("release child");
            let status = child.wait().expect("wait for process-memory child");
            assert!(status.success(), "child helper failed: {status}");
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}
