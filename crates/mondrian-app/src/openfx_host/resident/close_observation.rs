//! Default-off, fixed Windows close observations. Never an execution authority.
//!
//! The caller creates a private evidence directory before startup. Each session
//! creates one 4096-byte file; no file operation, allocation, flush or extra
//! waiter occurs in a close record. Read/decode only after the original owner
//! has settled. Zero slots are missing; status 2 is a failed clock observation.
//! QPC ticks use one Windows system clock/frequency, never process-local Instant.
//! Version 5 adds the final child Mapping Drop entry; it precedes unmap/file Drop
//! and establishes no completion or OS exit. All prior offsets stay fixed. UTC
//! FILETIMEs are diagnostic raw facts, never deadlines or interval authority.
use super::*;

const DIRECTORY_ENV: &str = "MONDRIAN_OPENFX_CLOSE_OBSERVATION_DIRECTORY";
const CHILD_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_CLOSE_OBSERVATION";

#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum Stage {
    ParentBegin,
    ParentSendBegin,
    ParentSendCommittedObserved,
    ParentClosedObserved,
    ParentOsExitObserved,
    ParentGracefulEnd,
    ParentSettlementBegin,
    ParentSettlementEnd,
    NativeDropBegin,
    NativeDropEnd,
    ClosedCommitted,
    AckReturned,
    PumpStopJoinBegin,
    PumpStopJoinEnd,
    ResidentInnerReturned,
    ChildAttached,
}

// Cold stack-only statistics. A failed observation cannot replace the query result.
#[derive(Debug, Default)]
pub(super) struct ExitQueryProbe {
    enabled: bool,
    unavailable: bool,
    last_result: u32, // 0 no query, 1 pending, 2 exited, 3 IO error
    query_count: u64,
    none_count: u64,
    last_none_qpc: i64,
    last_start_qpc: i64,
    last_end_qpc: i64,
    max_query_ticks: u64,
}
impl ExitQueryProbe {
    fn new(enabled: bool) -> Self {
        Self { enabled, ..Self::default() }
    }
    pub(super) fn observe<T>(
        &mut self,
        query: impl FnOnce() -> io::Result<Option<T>>,
    ) -> io::Result<Option<T>> {
        self.observe_with(query, || {
            #[cfg(windows)]
            {
                windows::query_counter()
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
    }
    fn observe_with<T>(
        &mut self,
        query: impl FnOnce() -> io::Result<Option<T>>,
        mut clock: impl FnMut() -> Option<i64>,
    ) -> io::Result<Option<T>> {
        if !self.enabled || self.unavailable {
            return query();
        }
        let start = clock();
        let result = query();
        let end = clock();
        let count = self.query_count.checked_add(1);
        let none_count = self.none_count.checked_add(u64::from(matches!(&result, Ok(None))));
        if let (Some(start), Some(end), Some(count), Some(none_count)) =
            (start, end, count, none_count)
            && start >= 0
            && end >= start
            && (self.query_count == 0 || start >= self.last_end_qpc)
        {
            self.query_count = count;
            self.none_count = none_count;
            self.last_result = match &result {
                Ok(None) => 1,
                Ok(Some(_)) => 2,
                Err(_) => 3,
            };
            if self.last_result == 1 {
                self.last_none_qpc = end;
            }
            self.last_start_qpc = start;
            self.last_end_qpc = end;
            self.max_query_ticks = self.max_query_ticks.max((end - start) as u64);
        } else {
            self.unavailable = true;
        }
        result
    }
}

// The first original cleanup attempt, not a second observer or an exit authority.
// Operation slots: initial try_wait, kill, post-refused-kill try_wait, wait.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum CleanupStep {
    InitialObservation,
    Kill,
    FinalObservation,
    Wait,
}

#[derive(Clone, Copy, Debug, Default)]
struct CleanupOperation {
    result: u32, // 0 not called, 1 pending, 2 exited, 3 kill Ok, 4 IO error
    flags: u32,  // 1 usable QPC, 2 exit code present, 4 successful exit, 8 raw OS error present
    value: i32,  // original exit code or raw OS error; flags distinguish absent from zero
    start_qpc: i64,
    end_qpc: i64,
}

#[derive(Debug)]
pub(super) struct CleanupProbe {
    enabled: bool,
    already_settled: bool,
    operations: [CleanupOperation; 4],
}
impl CleanupProbe {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            already_settled: false,
            operations: [CleanupOperation::default(); 4],
        }
    }
    pub(super) fn already_settled(&mut self) {
        self.already_settled = true;
    }
    pub(super) fn observe(
        &mut self,
        step: CleanupStep,
        query: impl FnOnce() -> io::Result<Option<std::process::ExitStatus>>,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        self.capture(step, query, |status| match status {
            Some(status) => Self::exit_status(status),
            None => CleanupOperation { result: 1, ..CleanupOperation::default() },
        })
    }
    pub(super) fn kill(&mut self, kill: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        self.capture(CleanupStep::Kill, kill, |_| CleanupOperation {
            result: 3,
            ..CleanupOperation::default()
        })
    }
    pub(super) fn wait(
        &mut self,
        wait: impl FnOnce() -> io::Result<std::process::ExitStatus>,
    ) -> io::Result<std::process::ExitStatus> {
        self.capture(CleanupStep::Wait, wait, Self::exit_status)
    }
    fn exit_status(status: &std::process::ExitStatus) -> CleanupOperation {
        CleanupOperation {
            result: 2,
            flags: u32::from(status.code().is_some()) * 2 + u32::from(status.success()) * 4,
            value: status.code().unwrap_or(0),
            ..CleanupOperation::default()
        }
    }
    fn capture<T>(
        &mut self,
        step: CleanupStep,
        operation: impl FnOnce() -> io::Result<T>,
        classify: impl FnOnce(&T) -> CleanupOperation,
    ) -> io::Result<T> {
        self.capture_with(step, operation, classify, || {
            #[cfg(windows)]
            {
                windows::query_counter()
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
    }
    fn capture_with<T>(
        &mut self,
        step: CleanupStep,
        operation: impl FnOnce() -> io::Result<T>,
        classify: impl FnOnce(&T) -> CleanupOperation,
        mut clock: impl FnMut() -> Option<i64>,
    ) -> io::Result<T> {
        if !self.enabled {
            return operation();
        }
        let start = clock();
        let result = operation();
        let end = clock();
        let mut record = match &result {
            Ok(value) => classify(value),
            Err(error) => CleanupOperation {
                result: 4,
                flags: u32::from(error.raw_os_error().is_some()) * 8,
                value: error.raw_os_error().unwrap_or(0),
                ..CleanupOperation::default()
            },
        };
        if let (Some(start), Some(end)) = (start, end)
            && start >= 0
            && end >= start
            && self.operations[..step as usize]
                .iter()
                .all(|previous| previous.flags & 1 == 0 || start >= previous.end_qpc)
        {
            record.flags |= 1;
            record.start_qpc = start;
            record.end_qpc = end;
        }
        self.operations[step as usize] = record;
        result
    }
    #[cfg(test)]
    pub(super) fn operation(&self, step: CleanupStep) -> (u32, u32, i32) {
        let operation = self.operations[step as usize];
        (operation.result, operation.flags, operation.value)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Trace {
    #[cfg(windows)]
    mapping: Option<std::sync::Arc<windows::Mapping>>,
}
impl Trace {
    pub(super) fn cleanup_probe(&self) -> CleanupProbe {
        #[cfg(windows)]
        let enabled = self.mapping.as_ref().is_some_and(|mapping| mapping.cleanup_pending());
        #[cfg(not(windows))]
        let enabled = false;
        CleanupProbe::new(enabled)
    }
    pub(super) fn publish_cleanup(&self, probe: CleanupProbe) {
        #[cfg(windows)]
        if let Some(mapping) = &self.mapping {
            mapping.publish_cleanup(&probe);
        }
        let _ = probe;
    }
    pub(super) fn exit_query_probe(&self) -> ExitQueryProbe {
        #[cfg(windows)]
        let enabled = self.mapping.is_some();
        #[cfg(not(windows))]
        let enabled = false;
        ExitQueryProbe::new(enabled)
    }
    pub(super) fn publish_exit_queries(&self, probe: ExitQueryProbe) {
        #[cfg(windows)]
        if let Some(mapping) = &self.mapping {
            mapping.publish_exit_queries(&probe);
        }
        let _ = probe;
    }
    pub(super) fn parent(
        command: &mut Command,
        owner: mondrian_core::EffectProcessorOwnerId,
        nonce: &str,
    ) -> Result<Self, OpenFxHostError> {
        // A foreign inherited child key cannot enable tracing accidentally.
        command.env_remove(CHILD_ENV);
        let Some(directory) = std::env::var_os(DIRECTORY_ENV) else {
            return Ok(Self::default());
        };
        #[cfg(windows)]
        {
            let (mapping, path) = windows::Mapping::create(directory.into(), owner, nonce)?;
            command.env(CHILD_ENV, path);
            Ok(Self { mapping: Some(std::sync::Arc::new(mapping)) })
        }
        #[cfg(not(windows))]
        {
            let _ = (directory, owner, nonce);
            Err(unavailable("OpenFX close observation requires Windows"))
        }
    }

    pub(super) fn child(
        owner: mondrian_core::EffectProcessorOwnerId,
        nonce: &str,
    ) -> Result<Self, OpenFxHostError> {
        let Some(path) = std::env::var_os(CHILD_ENV) else {
            return Ok(Self::default());
        };
        #[cfg(windows)]
        {
            let trace = Self {
                mapping: Some(std::sync::Arc::new(windows::Mapping::open(
                    path.into(),
                    owner,
                    nonce,
                )?)),
            };
            trace.record(Stage::ChildAttached, 0);
            Ok(trace)
        }
        #[cfg(not(windows))]
        {
            let _ = (path, owner, nonce);
            Err(unavailable("OpenFX close observation requires Windows"))
        }
    }

    pub(super) fn set_child(&self, child: &Child) {
        #[cfg(windows)]
        if let Some(mapping) = &self.mapping {
            use std::os::windows::io::AsRawHandle;
            mapping.set_child(child.id(), child.as_raw_handle() as usize as u64);
        }
        let _ = child;
    }

    /// Called only after the sole ProcessOwner has confirmed this Child exited.
    pub(super) fn record_process_exit(&self, child: &Child) {
        #[cfg(windows)]
        if let Some(mapping) = &self.mapping {
            mapping.record_process_exit(child);
        }
        let _ = child;
    }

    pub(super) fn record(&self, stage: Stage, value: u32) {
        #[cfg(windows)]
        if let Some(mapping) = &self.mapping {
            mapping.record(stage, value);
        }
        let _ = (stage, value);
    }
}

pub(super) fn result_code(result: &Result<(), OpenFxHostError>) -> u32 {
    match result {
        Ok(()) => 0,
        Err(OpenFxHostError::DeadlineExceeded) => 1,
        Err(_) => 2,
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
    use windows_sys::Win32::Foundation::{GetLastError, FILETIME};
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetCurrentThreadId, GetProcessTimes,
    };

    const BYTES: usize = 4096;
    const MAGIC: [u8; 8] = *b"OFXCLQ05";
    const SLOTS: usize = 16;

    // Binary little-endian x64 layout, independently readable after settlement.
    // status: 0 missing, 1 committed QPC, 2 committed clock failure. All slots
    // have one writer: parent 0..7, resident 8,9,11..15, sole pump 10.
    #[repr(C, align(64))]
    struct Record {
        status: AtomicU32,
        pid: AtomicU32,
        tid: AtomicU32,
        value: AtomicU32,
        qpc: AtomicI64,
        _reserved: [u8; 40],
    }
    impl Record {
        fn new() -> Self {
            Self {
                status: AtomicU32::new(0),
                pid: AtomicU32::new(0),
                tid: AtomicU32::new(0),
                value: AtomicU32::new(0),
                qpc: AtomicI64::new(0),
                _reserved: [0; 40],
            }
        }
        fn commit(&self, pid: u32, tid: u32, qpc: Option<i64>, value: u32) {
            // One-shot, single-writer slots also survive repeated shutdown/Drop.
            if self.status.load(Ordering::Relaxed) != 0 {
                return;
            }
            self.pid.store(pid, Ordering::Relaxed);
            self.tid.store(tid, Ordering::Relaxed);
            self.value.store(value, Ordering::Relaxed);
            self.qpc.store(qpc.unwrap_or(0), Ordering::Relaxed);
            self.status.store(if qpc.is_some() { 1 } else { 2 }, Ordering::Release);
        }
    }
    // Parent-only, single-writer publication after authoritative Child exit.
    // status: 0 missing, 1 usable UTC facts, 2 GetProcessTimes failed,
    // 3 a zero timestamp, 4 creation/exit/observation UTC ordering invalid.
    // Non-1 states retain raw diagnostics but never establish a usable gap.
    #[repr(C, align(64))]
    struct ExitObservation {
        status: AtomicU32,
        pid: AtomicU32,
        tid: AtomicU32,
        raw_os_error: AtomicU32,
        observed_utc_filetime: AtomicU64,
        creation_utc_filetime: AtomicU64,
        exit_utc_filetime: AtomicU64,
        _reserved: [u8; 24],
    }
    // Single publication after the original graceful attempt, never live counters.
    #[repr(C, align(64))]
    struct ExitQueryObservation {
        status: AtomicU32, // 0 missing, 1 usable, 2 unavailable (zero metric payload)
        pid: AtomicU32,
        tid: AtomicU32,
        last_result: AtomicU32,
        query_count: AtomicU64,
        none_count: AtomicU64,
        last_none_qpc: AtomicI64,
        last_start_qpc: AtomicI64,
        last_end_qpc: AtomicI64,
        max_query_ticks: AtomicU64,
    }
    impl ExitQueryObservation {
        fn new() -> Self {
            Self {
                status: AtomicU32::new(0),
                pid: AtomicU32::new(0),
                tid: AtomicU32::new(0),
                last_result: AtomicU32::new(0),
                query_count: AtomicU64::new(0),
                none_count: AtomicU64::new(0),
                last_none_qpc: AtomicI64::new(0),
                last_start_qpc: AtomicI64::new(0),
                last_end_qpc: AtomicI64::new(0),
                max_query_ticks: AtomicU64::new(0),
            }
        }
        fn commit(&self, pid: u32, tid: u32, probe: &ExitQueryProbe) {
            if !probe.enabled || self.status.load(Ordering::Relaxed) != 0 {
                return;
            }
            self.pid.store(pid, Ordering::Relaxed);
            self.tid.store(tid, Ordering::Relaxed);
            if !probe.unavailable {
                self.last_result.store(probe.last_result, Ordering::Relaxed);
                self.query_count.store(probe.query_count, Ordering::Relaxed);
                self.none_count.store(probe.none_count, Ordering::Relaxed);
                self.last_none_qpc.store(probe.last_none_qpc, Ordering::Relaxed);
                self.last_start_qpc.store(probe.last_start_qpc, Ordering::Relaxed);
                self.last_end_qpc.store(probe.last_end_qpc, Ordering::Relaxed);
                self.max_query_ticks.store(probe.max_query_ticks, Ordering::Relaxed);
            }
            self.status.store(if probe.unavailable { 2 } else { 1 }, Ordering::Release);
        }
    }
    // One parent-only, one-shot publication after the first original cleanup.
    // All existing schema3 offsets remain fixed; no live reader is supported.
    #[repr(C)]
    struct CleanupOperationRecord {
        result: AtomicU32,
        flags: AtomicU32,
        value: std::sync::atomic::AtomicI32,
        _reserved: u32,
        start_qpc: AtomicI64,
        end_qpc: AtomicI64,
    }
    impl CleanupOperationRecord {
        fn new() -> Self {
            Self {
                result: AtomicU32::new(0),
                flags: AtomicU32::new(0),
                value: std::sync::atomic::AtomicI32::new(0),
                _reserved: 0,
                start_qpc: AtomicI64::new(0),
                end_qpc: AtomicI64::new(0),
            }
        }
        fn store(&self, operation: &CleanupOperation) {
            self.result.store(operation.result, Ordering::Relaxed);
            self.flags.store(operation.flags, Ordering::Relaxed);
            self.value.store(operation.value, Ordering::Relaxed);
            self.start_qpc.store(operation.start_qpc, Ordering::Relaxed);
            self.end_qpc.store(operation.end_qpc, Ordering::Relaxed);
        }
    }
    #[repr(C, align(64))]
    struct CleanupObservation {
        status: AtomicU32, // 0 missing/partial, 1 summary published (per-operation QPC may be unavailable)
        pid: AtomicU32,
        tid: AtomicU32,
        already_settled: AtomicU32,
        _reserved: [u8; 48],
        operations: [CleanupOperationRecord; 4],
    }
    impl CleanupObservation {
        fn new() -> Self {
            Self {
                status: AtomicU32::new(0),
                pid: AtomicU32::new(0),
                tid: AtomicU32::new(0),
                already_settled: AtomicU32::new(0),
                _reserved: [0; 48],
                operations: std::array::from_fn(|_| CleanupOperationRecord::new()),
            }
        }
        fn commit(&self, pid: u32, tid: u32, probe: &CleanupProbe) {
            if !probe.enabled || self.status.load(Ordering::Relaxed) != 0 {
                return;
            }
            self.pid.store(pid, Ordering::Relaxed);
            self.tid.store(tid, Ordering::Relaxed);
            self.already_settled.store(u32::from(probe.already_settled), Ordering::Relaxed);
            for (record, operation) in self.operations.iter().zip(&probe.operations) {
                record.store(operation);
            }
            self.status.store(1, Ordering::Release);
        }
    }
    struct ProcessTimes {
        creation: u64,
        exit: u64,
    }
    impl ExitObservation {
        fn new() -> Self {
            Self {
                status: AtomicU32::new(0),
                pid: AtomicU32::new(0),
                tid: AtomicU32::new(0),
                raw_os_error: AtomicU32::new(0),
                observed_utc_filetime: AtomicU64::new(0),
                creation_utc_filetime: AtomicU64::new(0),
                exit_utc_filetime: AtomicU64::new(0),
                _reserved: [0; 24],
            }
        }
        fn commit(&self, pid: u32, tid: u32, observed: u64, times: Result<ProcessTimes, u32>) {
            if self.status.load(Ordering::Relaxed) != 0 {
                return;
            }
            self.pid.store(pid, Ordering::Relaxed);
            self.tid.store(tid, Ordering::Relaxed);
            self.observed_utc_filetime.store(observed, Ordering::Relaxed);
            let status = match times {
                Err(error) => {
                    self.raw_os_error.store(error, Ordering::Relaxed);
                    2
                }
                Ok(times) => {
                    self.creation_utc_filetime.store(times.creation, Ordering::Relaxed);
                    self.exit_utc_filetime.store(times.exit, Ordering::Relaxed);
                    if observed == 0 || times.creation == 0 || times.exit == 0 {
                        3
                    } else if times.creation > times.exit || times.exit > observed {
                        4
                    } else {
                        1
                    }
                }
            };
            self.status.store(status, Ordering::Release);
        }
        #[cfg(test)]
        fn observed_gap_100ns(&self) -> Option<u64> {
            if self.status.load(Ordering::Acquire) != 1 {
                return None;
            }
            self.observed_utc_filetime
                .load(Ordering::Relaxed)
                .checked_sub(self.exit_utc_filetime.load(Ordering::Relaxed))
        }
    }
    fn filetime_ticks(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }
    #[repr(C, align(64))]
    struct Header {
        magic: [u8; 8],
        version: u32,
        bytes: u32,
        frequency: i64,
        owner: [u8; 16],
        nonce: [u8; 16],
        parent_pid: u32,
        child_pid: AtomicU32,
        process_handle: AtomicU64,
        ready: AtomicU32,
        _reserved: [u8; 52],
    }
    #[repr(C, align(64))]
    struct Layout {
        header: Header,
        records: [Record; SLOTS],
        exit_observation: ExitObservation,
        exit_queries: ExitQueryObservation,
        cleanup: CleanupObservation,
        final_child_mapping_drop_entry: Record,
        _reserved: [u8; 2560],
    }
    const _: () = {
        assert!(std::mem::size_of::<Header>() == 128);
        assert!(std::mem::size_of::<Record>() == 64);
        assert!(std::mem::size_of::<ExitObservation>() == 64);
        assert!(std::mem::offset_of!(Layout, records) == 128);
        assert!(std::mem::offset_of!(Layout, exit_observation) == 1152);
        assert!(std::mem::size_of::<ExitQueryObservation>() == 64);
        assert!(std::mem::offset_of!(Layout, exit_queries) == 1216);
        assert!(std::mem::size_of::<CleanupOperationRecord>() == 32);
        assert!(std::mem::size_of::<CleanupObservation>() == 192);
        assert!(std::mem::offset_of!(Layout, cleanup) == 1280);
        assert!(std::mem::offset_of!(Layout, final_child_mapping_drop_entry) == 1472);
        assert!(std::mem::offset_of!(Layout, _reserved) == 1536);
        assert!(std::mem::size_of::<Layout>() == BYTES);
    };
    impl Layout {
        fn new(owner: [u8; 16], nonce: [u8; 16], frequency: i64, parent_pid: u32) -> Self {
            Self {
                header: Header {
                    magic: MAGIC,
                    version: 5,
                    bytes: BYTES as u32,
                    frequency,
                    owner,
                    nonce,
                    parent_pid,
                    child_pid: AtomicU32::new(0),
                    process_handle: AtomicU64::new(0),
                    ready: AtomicU32::new(0),
                    _reserved: [0; 52],
                },
                records: std::array::from_fn(|_| Record::new()),
                exit_observation: ExitObservation::new(),
                exit_queries: ExitQueryObservation::new(),
                cleanup: CleanupObservation::new(),
                final_child_mapping_drop_entry: Record::new(),
                _reserved: [0; 2560],
            }
        }
        fn valid(&self, owner: &[u8; 16], nonce: &[u8; 16], frequency: i64) -> bool {
            self.header.ready.load(Ordering::Acquire) == 1
                && self.header.magic == MAGIC
                && self.header.version == 5
                && self.header.bytes == BYTES as u32
                && frequency > 0
                && self.header.frequency == frequency
                && self.header.parent_pid != 0
                && self.header.owner == *owner
                && self.header.nonce == *nonce
        }
    }

    // Diagnostic writer role only, set after the corresponding setup succeeds.
    // An invalid/unattached view must not publish from its destructor.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MappingRole {
        Unattached,
        Parent,
        Child,
    }

    #[derive(Debug)]
    pub(super) struct Mapping {
        // Retained for every reference/Arc, including outer inner-return mark.
        _mapping: memmap2::MmapMut,
        _file: File,
        layout: NonNull<Layout>,
        pid: u32,
        role: MappingRole,
    }
    // SAFETY: fixed aligned layout, initialized before release-ready publication.
    // Immutable header fields never change thereafter. Every mutable field is
    // atomic; each record has one writer and commit is last. There are no &mut
    // references after initialization. The mmap and file outlive every Arc.
    unsafe impl Send for Mapping {}
    unsafe impl Sync for Mapping {}

    pub(super) fn query_counter() -> Option<i64> {
        let mut tick = 0;
        // SAFETY: optional observation only, one live aligned output, no wait or owner operation.
        (unsafe { QueryPerformanceCounter(&mut tick) } != 0).then_some(tick)
    }
    fn setup_error(error: io::Error) -> OpenFxHostError {
        unavailable(format!("OpenFX close observation setup failed: {error}"))
    }
    fn nonce_bytes(nonce: &str) -> Result<[u8; 16], OpenFxHostError> {
        uuid::Uuid::parse_str(nonce)
            .map(|id| *id.as_bytes())
            .map_err(|_| invalid("OpenFX close observation nonce is invalid"))
    }
    fn frequency() -> Result<i64, OpenFxHostError> {
        let mut value = 0_i64;
        // SAFETY: API writes exactly one live, aligned i64; setup only.
        if unsafe { QueryPerformanceFrequency(&mut value) } == 0 || value <= 0 {
            return Err(unavailable(
                "OpenFX close observation QPC frequency unavailable",
            ));
        }
        Ok(value)
    }
    impl Mapping {
        fn from_file(file: File) -> Result<Self, OpenFxHostError> {
            if file.metadata().map_err(setup_error)?.len() != BYTES as u64 {
                return Err(invalid("OpenFX close observation file extent mismatch"));
            }
            // SAFETY: exact retained file and fixed size; no resize/delete is
            // authorized while mapped. Only the declared atomic fields mutate.
            let mut mapping =
                unsafe { MmapOptions::new().len(BYTES).map_mut(&file) }.map_err(setup_error)?;
            let layout = NonNull::new(mapping.as_mut_ptr().cast::<Layout>())
                .ok_or_else(|| unavailable("OpenFX close observation mapping is null"))?;
            if !(layout.as_ptr() as usize).is_multiple_of(std::mem::align_of::<Layout>()) {
                return Err(invalid(
                    "OpenFX close observation mapping alignment mismatch",
                ));
            }
            // SAFETY: no arguments/pointers; cached once outside close timing.
            let pid = unsafe { GetCurrentProcessId() };
            Ok(Self {
                _mapping: mapping,
                _file: file,
                layout,
                pid,
                role: MappingRole::Unattached,
            })
        }
        fn layout(&self) -> &Layout {
            // SAFETY: above mapping lifetime/alignment/atomic invariants. Layout
            // consists solely of integers/bytes with all bit patterns valid.
            unsafe { self.layout.as_ref() }
        }
        pub(super) fn create(
            directory: PathBuf,
            owner: mondrian_core::EffectProcessorOwnerId,
            nonce: &str,
        ) -> Result<(Self, PathBuf), OpenFxHostError> {
            let metadata = std::fs::symlink_metadata(&directory).map_err(setup_error)?;
            if !directory.is_absolute()
                || !metadata.is_dir()
                || metadata.file_attributes() & 0x400 != 0
            {
                return Err(invalid(
                    "OpenFX close observation requires an existing nonreparse directory",
                ));
            }
            let nonce_id = nonce_bytes(nonce)?;
            let frequency = frequency()?;
            let path = directory.join(format!("ofx-close-{nonce}.bin"));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .share_mode(1 | 2)
                .open(&path)
                .map_err(setup_error)?;
            file.set_len(BYTES as u64).map_err(setup_error)?;
            let mut mapping = Self::from_file(file)?;
            // SAFETY: parent has the sole view/reference before spawn. Full
            // initialization touches the entire fixed map before close timing.
            unsafe {
                mapping.layout.as_ptr().write(Layout::new(
                    *owner.0.as_bytes(),
                    nonce_id,
                    frequency,
                    mapping.pid,
                ))
            };
            mapping.layout().header.ready.store(1, Ordering::Release);
            mapping.role = MappingRole::Parent;
            Ok((mapping, path))
        }
        pub(super) fn open(
            path: PathBuf,
            owner: mondrian_core::EffectProcessorOwnerId,
            nonce: &str,
        ) -> Result<Self, OpenFxHostError> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(1 | 2)
                .open(path)
                .map_err(setup_error)?;
            let mut mapping = Self::from_file(file)?;
            if !mapping.layout().valid(owner.0.as_bytes(), &nonce_bytes(nonce)?, frequency()?) {
                return Err(invalid(
                    "OpenFX close observation identity/layout/clock mismatch",
                ));
            }
            mapping.role = MappingRole::Child;
            Ok(mapping)
        }
        pub(super) fn cleanup_pending(&self) -> bool {
            self.pid == self.layout().header.parent_pid
                && self.layout().cleanup.status.load(Ordering::Relaxed) == 0
        }
        pub(super) fn publish_cleanup(&self, probe: &CleanupProbe) {
            if !probe.enabled || self.pid != self.layout().header.parent_pid {
                return;
            }
            // SAFETY: only this existing writer identity is read, never another Child.
            let tid = unsafe { GetCurrentThreadId() };
            self.layout().cleanup.commit(self.pid, tid, probe);
        }
        pub(super) fn publish_exit_queries(&self, probe: &ExitQueryProbe) {
            if self.pid != self.layout().header.parent_pid {
                return;
            }
            // SAFETY: only obtains this existing writer's identity; never observes a Child.
            let tid = unsafe { GetCurrentThreadId() };
            self.layout().exit_queries.commit(self.pid, tid, probe);
        }
        pub(super) fn set_child(&self, pid: u32, handle: u64) {
            self.layout().header.process_handle.store(handle, Ordering::Relaxed);
            self.layout().header.child_pid.store(pid, Ordering::Release);
        }
        pub(super) fn record_process_exit(&self, child: &Child) {
            use std::os::windows::io::AsRawHandle;
            let record = &self.layout().exit_observation;
            if record.status.load(Ordering::Relaxed) != 0 {
                return;
            }
            let mut observed: FILETIME = unsafe { std::mem::zeroed() };
            let mut creation: FILETIME = unsafe { std::mem::zeroed() };
            let mut exit: FILETIME = unsafe { std::mem::zeroed() };
            let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
            let mut user: FILETIME = unsafe { std::mem::zeroed() };
            // SAFETY: the sole owner already confirmed exit and retains this
            // exact Child handle throughout the calls. Every output points to
            // one live FILETIME. No handle is opened, duplicated, waited or closed.
            // UTC is sampled at this observation, never used for a deadline.
            unsafe { GetSystemTimePreciseAsFileTime(&mut observed) };
            let ok = unsafe {
                GetProcessTimes(
                    child.as_raw_handle(),
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            };
            // Preserve the failed call's error before any other Windows API.
            let times = if ok == 0 {
                Err(unsafe { GetLastError() })
            } else {
                Ok(ProcessTimes {
                    creation: filetime_ticks(creation),
                    exit: filetime_ticks(exit),
                })
            };
            let tid = unsafe { GetCurrentThreadId() };
            record.commit(self.pid, tid, filetime_ticks(observed), times);
        }
        pub(super) fn record(&self, stage: Stage, value: u32) {
            let record = &self.layout().records[stage as usize];
            if record.status.load(Ordering::Relaxed) != 0 {
                return;
            }
            let mut tick = 0_i64;
            // SAFETY: only enabled trace calls these APIs; live aligned i64 and
            // no retained foreign pointer. No wait, allocation or file I/O.
            let clock_ok = unsafe { QueryPerformanceCounter(&mut tick) } != 0;
            let tid = unsafe { GetCurrentThreadId() };
            record.commit(self.pid, tid, clock_ok.then_some(tick), value);
        }
    }

    // This is only the final child Arc's destructor entry. The existing fields
    // still drop in declaration order afterwards; no clone keeps the view alive.
    impl Drop for Mapping {
        fn drop(&mut self) {
            self.record_drop_entry_with(query_counter, || {
                // SAFETY: no retained handle/output; identifies this sole writer.
                unsafe { GetCurrentThreadId() }
            });
        }
    }
    impl Mapping {
        fn record_drop_entry_with(
            &self,
            clock: impl FnOnce() -> Option<i64>,
            thread_id: impl FnOnce() -> u32,
        ) {
            let layout = (self.role == MappingRole::Child).then(|| self.layout());
            record_final_mapping_drop_entry_with(layout, self.pid, clock, thread_id);
        }
    }
    fn record_final_mapping_drop_entry_with(
        layout: Option<&Layout>,
        pid: u32,
        clock: impl FnOnce() -> Option<i64>,
        thread_id: impl FnOnce() -> u32,
    ) {
        let Some(layout) = layout else { return };
        let record = &layout.final_child_mapping_drop_entry;
        if record.status.load(Ordering::Relaxed) != 0 {
            return;
        }
        let tick = clock().filter(|tick| *tick >= 0);
        // value 1 is the authenticated child-view role, not a completion result.
        record.commit(pid, thread_id(), tick, 1);
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn fixed_layout_rejects_wrong_identity_clock_and_unpublished_headers() {
            let mut layout = Layout::new([1; 16], [2; 16], 10_000_000, 42);
            assert_eq!(std::mem::size_of::<Header>(), 128);
            assert_eq!(std::mem::size_of::<Record>(), 64);
            assert_eq!(std::mem::size_of::<ExitObservation>(), 64);
            assert_eq!(std::mem::offset_of!(Layout, records), 128);
            assert_eq!(std::mem::offset_of!(Layout, exit_observation), 1152);
            assert_eq!(std::mem::size_of::<ExitQueryObservation>(), 64);
            assert_eq!(std::mem::offset_of!(Layout, exit_queries), 1216);
            assert_eq!(std::mem::size_of::<CleanupOperationRecord>(), 32);
            assert_eq!(std::mem::size_of::<CleanupObservation>(), 192);
            assert_eq!(std::mem::offset_of!(Layout, cleanup), 1280);
            assert_eq!(
                std::mem::offset_of!(Layout, final_child_mapping_drop_entry),
                1472
            );
            assert_eq!(std::mem::offset_of!(Layout, _reserved), 1536);
            assert_eq!(std::mem::size_of::<Layout>(), 4096);
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
            layout.header.ready.store(1, Ordering::Release);
            assert!(layout.valid(&[1; 16], &[2; 16], 10_000_000));
            assert!(!layout.valid(&[3; 16], &[2; 16], 10_000_000));
            assert!(!layout.valid(&[1; 16], &[3; 16], 10_000_000));
            assert!(!layout.valid(&[1; 16], &[2; 16], 1));
            assert!(!layout.valid(&[1; 16], &[2; 16], 0));
            layout.header.version = 1;
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
            layout.header.version = 5;
            layout.header.magic = *b"OFXCLQ01";
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
            layout.header.magic = MAGIC;
            layout.header.bytes = 4095;
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
        }
        #[test]
        fn final_child_mapping_drop_is_published_only_by_the_last_arc() {
            let directory = tempfile::tempdir().expect("private trace directory");
            let owner = mondrian_core::EffectProcessorOwnerId::new();
            let nonce = uuid::Uuid::new_v4().to_string();
            let (parent, path) = Mapping::create(directory.path().to_path_buf(), owner, &nonce)
                .expect("actual fixed parent mapping");
            let child = std::sync::Arc::new(
                Mapping::open(path, owner, &nonce).expect("authenticated separate child view"),
            );
            let last = std::sync::Arc::clone(&child);
            let record = &parent.layout().final_child_mapping_drop_entry;
            assert_eq!(record.status.load(Ordering::Acquire), 0);
            drop(child);
            assert_eq!(record.status.load(Ordering::Acquire), 0);
            drop(last);
            assert_eq!(record.status.load(Ordering::Acquire), 1);
            assert_eq!(record.pid.load(Ordering::Relaxed), parent.pid);
            // This ordinary control uses two views in one process; it does not
            // claim a native child or OS exit. Production uses the real child PID.
            assert_ne!(record.tid.load(Ordering::Relaxed), 0);
            assert_eq!(record.value.load(Ordering::Relaxed), 1);
            assert!(record.qpc.load(Ordering::Relaxed) >= 0);
        }

        #[test]
        fn parent_mapping_drop_never_writes_or_reads_the_clock() {
            let directory = tempfile::tempdir().expect("private trace directory");
            let owner = mondrian_core::EffectProcessorOwnerId::new();
            let nonce = uuid::Uuid::new_v4().to_string();
            let (parent, path) = Mapping::create(directory.path().to_path_buf(), owner, &nonce)
                .expect("actual fixed parent mapping");
            let child = Mapping::open(path, owner, &nonce).expect("separate retained view");
            parent.record_drop_entry_with(
                || panic!("parent must not read QPC for the child record"),
                || panic!("parent must not read the child writer TID"),
            );
            drop(parent);
            assert_eq!(
                child.layout().final_child_mapping_drop_entry.status.load(Ordering::Acquire),
                0
            );
        }

        #[test]
        fn disabled_final_mapping_drop_observation_never_reads_identity_or_clock() {
            let trace = Trace::default();
            assert!(trace.mapping.is_none());
            record_final_mapping_drop_entry_with(
                None,
                0,
                || panic!("disabled observation must not read QPC"),
                || panic!("disabled observation must not read TID"),
            );
            drop(trace);
        }

        #[test]
        fn rejected_child_mapping_setup_never_publishes_a_drop_entry() {
            let directory = tempfile::tempdir().expect("private trace directory");
            let owner = mondrian_core::EffectProcessorOwnerId::new();
            let nonce = uuid::Uuid::new_v4().to_string();
            let (parent, path) = Mapping::create(directory.path().to_path_buf(), owner, &nonce)
                .expect("actual fixed parent mapping");
            assert!(Mapping::open(
                path.clone(),
                mondrian_core::EffectProcessorOwnerId::new(),
                &nonce
            )
            .is_err());
            assert!(Mapping::open(path, owner, &uuid::Uuid::new_v4().to_string()).is_err());
            assert_eq!(
                parent.layout().final_child_mapping_drop_entry.status.load(Ordering::Acquire),
                0
            );
        }

        #[test]
        fn final_mapping_drop_clock_failure_is_one_shot_and_not_completion() {
            for tick in [None, Some(-1)] {
                let layout = Layout::new([1; 16], [2; 16], 10_000_000, 11);
                record_final_mapping_drop_entry_with(Some(&layout), 22, || tick, || 33);
                let record = &layout.final_child_mapping_drop_entry;
                assert_eq!(record.status.load(Ordering::Acquire), 2);
                assert_eq!(record.pid.load(Ordering::Relaxed), 22);
                assert_eq!(record.tid.load(Ordering::Relaxed), 33);
                assert_eq!(record.value.load(Ordering::Relaxed), 1);
                assert_eq!(record.qpc.load(Ordering::Relaxed), 0);
                record_final_mapping_drop_entry_with(
                    Some(&layout),
                    99,
                    || panic!("a second publication must not read QPC"),
                    || panic!("a second publication must not read TID"),
                );
                assert_eq!(record.pid.load(Ordering::Relaxed), 22);
                assert_eq!(record.tid.load(Ordering::Relaxed), 33);
            }
        }

        #[test]
        fn cleanup_summary_is_one_shot_and_preserves_results_without_clock() {
            let record = CleanupObservation::new();
            record.commit(11, 22, &CleanupProbe::new(false));
            assert_eq!(record.status.load(Ordering::Acquire), 0);
            assert_eq!(record.pid.load(Ordering::Relaxed), 0);
            assert_eq!(record.tid.load(Ordering::Relaxed), 0);
            assert_eq!(record.already_settled.load(Ordering::Relaxed), 0);
            assert!(record.operations.iter().all(|operation| {
                operation.result.load(Ordering::Relaxed) == 0
                    && operation.flags.load(Ordering::Relaxed) == 0
                    && operation.value.load(Ordering::Relaxed) == 0
                    && operation.start_qpc.load(Ordering::Relaxed) == 0
                    && operation.end_qpc.load(Ordering::Relaxed) == 0
            }));
            let mut probe = CleanupProbe::new(true);
            let error = probe
                .capture_with::<()>(
                    CleanupStep::InitialObservation,
                    || Err(io::Error::from_raw_os_error(87)),
                    |_| panic!("error must not classify success"),
                    || None,
                )
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(87));
            record.commit(11, 22, &probe);
            assert_eq!(record.status.load(Ordering::Acquire), 1);
            assert_eq!(record.pid.load(Ordering::Relaxed), 11);
            assert_eq!(record.tid.load(Ordering::Relaxed), 22);
            let initial = &record.operations[0];
            assert_eq!(initial.result.load(Ordering::Relaxed), 4);
            assert_eq!(initial.flags.load(Ordering::Relaxed), 8);
            assert_eq!(initial.value.load(Ordering::Relaxed), 87);
            assert_eq!(initial.start_qpc.load(Ordering::Relaxed), 0);
            assert_eq!(initial.end_qpc.load(Ordering::Relaxed), 0);
            let mut retry = CleanupProbe::new(true);
            retry.already_settled();
            record.commit(99, 99, &retry);
            assert_eq!(record.pid.load(Ordering::Relaxed), 11);
            assert_eq!(record.already_settled.load(Ordering::Relaxed), 0);
            assert_eq!(initial.result.load(Ordering::Relaxed), 4);
            for operation in &record.operations[1..] {
                assert_eq!(operation.result.load(Ordering::Relaxed), 0);
                assert_eq!(operation.flags.load(Ordering::Relaxed), 0);
            }
            let settled = CleanupObservation::new();
            settled.commit(11, 33, &retry);
            assert_eq!(settled.status.load(Ordering::Acquire), 1);
            assert_eq!(settled.already_settled.load(Ordering::Relaxed), 1);
            assert!(settled
                .operations
                .iter()
                .all(|operation| operation.result.load(Ordering::Relaxed) == 0));
        }

        #[test]
        fn exit_query_summary_is_one_shot_and_unavailable_is_not_zero_queries() {
            let record = ExitQueryObservation::new();
            let mut probe = ExitQueryProbe::new(true);
            record.commit(11, 22, &probe);
            assert_eq!(record.status.load(Ordering::Acquire), 1);
            assert_eq!(record.query_count.load(Ordering::Relaxed), 0);
            probe.observe_with::<u8>(|| Ok(None), || Some(10)).unwrap();
            record.commit(99, 99, &probe);
            assert_eq!(record.pid.load(Ordering::Relaxed), 11);
            assert_eq!(record.query_count.load(Ordering::Relaxed), 0);
            let failed = ExitQueryObservation::new();
            probe.observe_with::<u8>(|| Ok(None), || None).unwrap();
            failed.commit(11, 22, &probe);
            assert_eq!(failed.status.load(Ordering::Acquire), 2);
            assert_eq!(failed.last_result.load(Ordering::Relaxed), 0);
            assert_eq!(failed.query_count.load(Ordering::Relaxed), 0);
            assert_eq!(failed.last_none_qpc.load(Ordering::Relaxed), 0);
            let measured = ExitQueryObservation::new();
            let mut probe = ExitQueryProbe::new(true);
            let mut ticks = [10, 14].into_iter();
            probe.observe_with::<u8>(|| Ok(None), || ticks.next()).unwrap();
            measured.commit(11, 22, &probe);
            assert_eq!(measured.query_count.load(Ordering::Relaxed), 1);
            assert_eq!(measured.none_count.load(Ordering::Relaxed), 1);
            assert_eq!(measured.last_none_qpc.load(Ordering::Relaxed), 14);
            assert_eq!(measured.last_start_qpc.load(Ordering::Relaxed), 10);
            assert_eq!(measured.last_end_qpc.load(Ordering::Relaxed), 14);
            assert_eq!(measured.max_query_ticks.load(Ordering::Relaxed), 4);
        }
        #[test]
        fn committed_slots_are_one_shot_and_clock_failure_is_not_a_timestamp() {
            let record = Record::new();
            assert_eq!(record.status.load(Ordering::Acquire), 0);
            record.commit(11, 22, Some(333), 44);
            record.commit(99, 99, None, 99);
            assert_eq!(record.status.load(Ordering::Acquire), 1);
            assert_eq!(record.pid.load(Ordering::Relaxed), 11);
            assert_eq!(record.tid.load(Ordering::Relaxed), 22);
            assert_eq!(record.qpc.load(Ordering::Relaxed), 333);
            assert_eq!(record.value.load(Ordering::Relaxed), 44);
            let failed = Record::new();
            failed.commit(11, 22, None, 5);
            assert_eq!(failed.status.load(Ordering::Acquire), 2);
            assert_eq!(failed.qpc.load(Ordering::Relaxed), 0);
            assert_eq!(failed.value.load(Ordering::Relaxed), 5);
        }
        #[test]
        fn exit_observation_keeps_query_failure_zero_and_utc_order_unknown() {
            let failed = ExitObservation::new();
            failed.commit(11, 22, 50, Err(5));
            assert_eq!(failed.status.load(Ordering::Acquire), 2);
            assert_eq!(failed.raw_os_error.load(Ordering::Relaxed), 5);
            assert_eq!(failed.observed_gap_100ns(), None);
            assert_eq!(failed.exit_utc_filetime.load(Ordering::Relaxed), 0);
            // A later success cannot rewrite the first immutable failure.
            failed.commit(99, 99, 500, Ok(ProcessTimes { creation: 1, exit: 2 }));
            assert_eq!(failed.raw_os_error.load(Ordering::Relaxed), 5);
            assert_eq!(failed.pid.load(Ordering::Relaxed), 11);
            assert_eq!(failed.observed_gap_100ns(), None);
            for (observed, creation, exit, status) in [
                (0, 10, 20, 3),
                (30, 0, 20, 3),
                (30, 10, 0, 3),
                (30, 21, 20, 4),
                (19, 10, 20, 4),
            ] {
                let record = ExitObservation::new();
                record.commit(11, 22, observed, Ok(ProcessTimes { creation, exit }));
                assert_eq!(record.status.load(Ordering::Acquire), status);
                assert_eq!(record.observed_gap_100ns(), None);
            }
            let record = ExitObservation::new();
            assert_eq!(record.observed_gap_100ns(), None);
            record.commit(11, 22, 50, Ok(ProcessTimes { creation: 10, exit: 20 }));
            assert_eq!(record.status.load(Ordering::Acquire), 1);
            assert_eq!(record.observed_gap_100ns(), Some(30));
            record.commit(99, 99, 500, Err(6));
            assert_eq!(record.observed_gap_100ns(), Some(30));
            assert_eq!(record.raw_os_error.load(Ordering::Relaxed), 0);
            assert_eq!(
                filetime_ticks(FILETIME {
                    dwLowDateTime: 0x89ab_cdef,
                    dwHighDateTime: 0x1234_5678
                }),
                0x1234_5678_89ab_cdef,
            );
        }

        #[test]
        #[ignore = "requires actual outer owner Job; naturally exited same-handle child with delayed observation"]
        fn owned_natural_exit_timestamp_distinguishes_delayed_parent_observation() {
            use crate::openfx_host::resident::process_retirement_tests::{
                assert_settled_idempotent, request_natural_exit, sleeping_owner,
            };
            use std::os::windows::io::AsRawHandle;
            let (mut owner, child_directory) = sleeping_owner();
            let trace_directory = tempfile::tempdir().expect("private exit observation directory");
            let processor_owner = mondrian_core::EffectProcessorOwnerId::new();
            let nonce = uuid::Uuid::new_v4().to_string();
            let (mapping, _path) = Mapping::create(
                trace_directory.path().to_path_buf(),
                processor_owner,
                &nonce,
            )
            .expect("actual fixed observation mapping");
            let mapping = std::sync::Arc::new(mapping);
            owner.close_trace = Trace { mapping: Some(mapping.clone()) };
            owner.close_trace.set_child(&owner.child);
            let pid = owner.child.id();
            let handle = owner.child.as_raw_handle() as usize as u64;
            request_natural_exit(&child_directory);
            // No Child exit query occurs between this request and the delayed
            // observation below. The OS exit FILETIME, not this sleep, proves
            // how long the already-exited child preceded parent observation.
            let delay = Instant::now();
            thread::sleep(Duration::from_millis(300));
            let delay_elapsed = delay.elapsed();
            owner
                .terminate_with(
                    |child| {
                        let status = child.try_wait()?;
                        assert_eq!(
                            status.and_then(|status| status.code()),
                            Some(7),
                            "must confirm natural fixture exit before recording"
                        );
                        Ok(status)
                    },
                    |_| panic!("natural-exit control must never kill"),
                    |_| panic!("natural-exit control must never wait"),
                )
                .expect("same owner confirms the naturally exited child");
            let record = &mapping.layout().exit_observation;
            assert_eq!(record.status.load(Ordering::Acquire), 1);
            let gap = record.observed_gap_100ns().expect("usable same-domain raw UTC facts");
            assert!(
                gap >= 1_500_000,
                "actual exit-to-observation gap below 150ms: {gap} ticks"
            );
            assert_eq!(
                mapping.layout().header.child_pid.load(Ordering::Acquire),
                pid
            );
            assert_eq!(
                mapping.layout().header.process_handle.load(Ordering::Relaxed),
                handle
            );
            assert_settled_idempotent(&mut owner);
            assert_eq!(record.observed_gap_100ns(), Some(gap));
            println!(
                "{}",
                serde_json::json!({
                    "control": "owned_natural_exit_timestamp_distinguishes_delayed_parent_observation",
                    "schema_version": 5,
                    "child_pid": pid,
                    "retained_child_handle": handle,
                    "natural_exit_code": 7,
                    "observation_status": record.status.load(Ordering::Acquire),
                    "raw_os_error": record.raw_os_error.load(Ordering::Relaxed),
                    "creation_utc_filetime": record.creation_utc_filetime.load(Ordering::Relaxed),
                    "exit_utc_filetime": record.exit_utc_filetime.load(Ordering::Relaxed),
                    "parent_observed_utc_filetime": record.observed_utc_filetime.load(Ordering::Relaxed),
                    "exit_to_observation_100ns": gap,
                    "deliberate_parent_delay_ms": delay_elapsed.as_millis() as u64,
                })
            );
        }

        #[test]
        fn separately_mapped_views_keep_identity_one_shot_and_create_new_ownership() {
            let directory = tempfile::tempdir().expect("private trace directory");
            let owner = mondrian_core::EffectProcessorOwnerId::new();
            let nonce = uuid::Uuid::new_v4().to_string();
            let (parent, path) = Mapping::create(directory.path().to_path_buf(), owner, &nonce)
                .expect("actual fixed parent mapping");
            let child = Mapping::open(path.clone(), owner, &nonce).expect("separate child view");
            assert!(Mapping::create(directory.path().to_path_buf(), owner, &nonce).is_err());
            assert!(Mapping::open(
                path.clone(),
                mondrian_core::EffectProcessorOwnerId::new(),
                &nonce
            )
            .is_err());
            assert!(Mapping::open(path, owner, &uuid::Uuid::new_v4().to_string()).is_err());
            parent.record(Stage::ParentBegin, 17);
            let seen = &child.layout().records[Stage::ParentBegin as usize];
            assert_eq!(seen.status.load(Ordering::Acquire), 1);
            assert_eq!(seen.pid.load(Ordering::Relaxed), parent.pid);
            assert_ne!(seen.tid.load(Ordering::Relaxed), 0);
            assert_eq!(seen.value.load(Ordering::Relaxed), 17);
            let first = seen.qpc.load(Ordering::Relaxed);
            parent.record(Stage::ParentBegin, 99);
            assert_eq!(seen.qpc.load(Ordering::Relaxed), first);
            assert_eq!(seen.value.load(Ordering::Relaxed), 17);
            assert_eq!(
                child.layout().records[Stage::ClosedCommitted as usize]
                    .status
                    .load(Ordering::Acquire),
                0
            );
        }
        #[test]
        fn late_parent_pid_publication_is_metadata_not_a_child_wait_condition() {
            let layout = Layout::new([1; 16], [2; 16], 10_000_000, 42);
            layout.header.ready.store(1, Ordering::Release);
            assert_eq!(layout.header.child_pid.load(Ordering::Acquire), 0);
            assert!(layout.valid(&[1; 16], &[2; 16], 10_000_000));
            layout.records[Stage::ChildAttached as usize].commit(77, 78, Some(123), 0);
            layout.header.process_handle.store(55, Ordering::Relaxed);
            layout.header.child_pid.store(77, Ordering::Release);
            assert_eq!(layout.header.child_pid.load(Ordering::Acquire), 77);
            assert_eq!(layout.header.process_handle.load(Ordering::Relaxed), 55);
            assert_eq!(
                layout.records[Stage::ChildAttached as usize].pid.load(Ordering::Relaxed),
                77
            );
            assert_eq!(
                layout.records[Stage::ClosedCommitted as usize].status.load(Ordering::Acquire),
                0
            );
        }
    }
}

#[cfg(test)]
mod query_probe_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn disabled_exit_query_probe_calls_once_without_clock() {
        let calls = Cell::new(0);
        let mut probe = ExitQueryProbe::new(false);
        let result = probe.observe_with(
            || {
                calls.set(calls.get() + 1);
                Ok(Some(7))
            },
            || panic!("disabled observation must not read QPC"),
        );
        assert_eq!(result.unwrap(), Some(7));
        assert_eq!(calls.get(), 1);
        assert_eq!(probe.query_count, 0);
    }

    #[test]
    fn exit_query_probe_preserves_each_result_and_pending_timestamp() {
        let mut probe = ExitQueryProbe::new(true);
        let mut ticks = [10, 13, 20, 29, 30, 31].into_iter();
        let calls = Cell::new(0);
        let mut clock = || ticks.next();
        assert!(probe
            .observe_with::<u8>(
                || {
                    calls.set(calls.get() + 1);
                    Ok(None)
                },
                &mut clock
            )
            .unwrap()
            .is_none());
        let error = probe
            .observe_with::<u8>(
                || {
                    calls.set(calls.get() + 1);
                    Err(io::Error::from_raw_os_error(87))
                },
                &mut clock,
            )
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(87));
        assert_eq!(
            probe
                .observe_with(
                    || {
                        calls.set(calls.get() + 1);
                        Ok(Some(42))
                    },
                    &mut clock
                )
                .unwrap(),
            Some(42)
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(
            (probe.query_count, probe.none_count, probe.last_result),
            (3, 1, 2)
        );
        assert_eq!(
            (
                probe.last_none_qpc,
                probe.last_start_qpc,
                probe.last_end_qpc,
                probe.max_query_ticks
            ),
            (13, 30, 31, 9)
        );
        assert!(!probe.unavailable);
        assert!(ticks.next().is_none());
    }

    #[test]
    fn exit_query_probe_clock_or_counter_failure_is_only_unavailable() {
        for ticks in [[None, Some(2)], [Some(2), None], [Some(2), Some(1)]] {
            let mut probe = ExitQueryProbe::new(true);
            let mut ticks = ticks.into_iter();
            let error = probe
                .observe_with::<u8>(
                    || Err(io::Error::from_raw_os_error(5)),
                    || ticks.next().unwrap(),
                )
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(5));
            assert!(probe.unavailable);
            assert_eq!(
                probe
                    .observe_with(
                        || Ok(Some(9)),
                        || panic!("invalid statistics stay unavailable")
                    )
                    .unwrap(),
                Some(9)
            );
        }
        let mut probe = ExitQueryProbe::new(true);
        probe.observe_with::<u8>(|| Ok(None), || Some(10)).unwrap();
        let mut regressed = [9, 12].into_iter();
        assert_eq!(
            probe.observe_with(|| Ok(Some(4)), || regressed.next()).unwrap(),
            Some(4)
        );
        assert!(probe.unavailable);
        let mut probe = ExitQueryProbe::new(true);
        probe.query_count = u64::MAX;
        assert_eq!(
            probe.observe_with(|| Ok(Some(3)), || Some(1)).unwrap(),
            Some(3)
        );
        assert!(probe.unavailable);
    }
}

#[cfg(test)]
mod cleanup_probe_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn disabled_cleanup_probe_calls_original_once_without_clock_or_payload_read() {
        let calls = Cell::new(0);
        let mut probe = CleanupProbe::new(false);
        let original = io::Error::from_raw_os_error(87);
        let error = probe
            .capture_with::<u8>(
                CleanupStep::InitialObservation,
                || {
                    calls.set(calls.get() + 1);
                    Err(original)
                },
                |_| panic!("disabled observation must not inspect payload"),
                || panic!("disabled observation must not read QPC"),
            )
            .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(error.raw_os_error(), Some(87));
        assert_eq!(probe.operation(CleanupStep::InitialObservation), (0, 0, 0));
    }

    #[test]
    fn cleanup_probe_clock_failure_never_changes_original_results() {
        for ticks in [
            [None, Some(2)],
            [Some(2), None],
            [Some(2), Some(1)],
            [Some(-1), Some(2)],
        ] {
            let mut probe = CleanupProbe::new(true);
            let mut ticks = ticks.into_iter();
            let calls = Cell::new(0);
            let error = probe
                .capture_with::<u8>(
                    CleanupStep::FinalObservation,
                    || {
                        calls.set(calls.get() + 1);
                        Err(io::Error::from_raw_os_error(6))
                    },
                    |_| panic!("error must not classify success"),
                    || ticks.next().unwrap(),
                )
                .unwrap_err();
            assert_eq!(calls.get(), 1);
            assert_eq!(error.raw_os_error(), Some(6));
            let operation = probe.operations[CleanupStep::FinalObservation as usize];
            assert_eq!(
                (operation.result, operation.flags, operation.value),
                (4, 8, 6)
            );
            assert_eq!((operation.start_qpc, operation.end_qpc), (0, 0));
        }
        let mut probe = CleanupProbe::new(true);
        let mut ticks = [10, 20, 19, 21].into_iter();
        probe
            .capture_with(
                CleanupStep::InitialObservation,
                || Ok(()),
                |_| CleanupOperation { result: 1, ..CleanupOperation::default() },
                || ticks.next(),
            )
            .unwrap();
        probe
            .capture_with(
                CleanupStep::Kill,
                || Ok(()),
                |_| CleanupOperation { result: 3, ..CleanupOperation::default() },
                || ticks.next(),
            )
            .unwrap();
        assert_eq!(probe.operation(CleanupStep::InitialObservation), (1, 1, 0));
        assert_eq!(probe.operation(CleanupStep::Kill), (3, 0, 0));
        assert_eq!(
            (probe.operations[1].start_qpc, probe.operations[1].end_qpc),
            (0, 0)
        );
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn cleanup_probe_preserves_exit_status_and_distinguishes_kill_from_wait() {
        fn status(code: u32) -> std::process::ExitStatus {
            #[cfg(windows)]
            {
                use std::os::windows::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(code)
            }
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                std::process::ExitStatus::from_raw((code as i32) << 8)
            }
        }
        let mut probe = CleanupProbe::new(true);
        let calls = Cell::new(0);
        let original = status(7);
        assert_eq!(
            probe
                .observe(CleanupStep::InitialObservation, || {
                    calls.set(calls.get() + 1);
                    Ok(Some(original))
                })
                .unwrap(),
            Some(original)
        );
        probe
            .kill(|| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap();
        let success = status(0);
        assert_eq!(
            probe
                .wait(|| {
                    calls.set(calls.get() + 1);
                    Ok(success)
                })
                .unwrap(),
            success
        );
        assert_eq!(calls.get(), 3);
        let (result, flags, value) = probe.operation(CleanupStep::InitialObservation);
        assert_eq!((result, flags & !1, value), (2, 2, 7));
        let (result, flags, value) = probe.operation(CleanupStep::Kill);
        assert_eq!((result, flags & !1, value), (3, 0, 0));
        let (result, flags, value) = probe.operation(CleanupStep::Wait);
        assert_eq!((result, flags & !1, value), (2, 6, 0));
        assert_eq!(probe.operation(CleanupStep::FinalObservation), (0, 0, 0));
    }

    #[test]
    fn cleanup_probe_records_only_invoked_operations_and_original_error_availability() {
        let mut probe = CleanupProbe::new(true);
        let mut ticks = [10, 13, 20, 29].into_iter();
        let calls = Cell::new(0);
        let value = probe
            .capture_with(
                CleanupStep::InitialObservation,
                || {
                    calls.set(calls.get() + 1);
                    Ok(17_u8)
                },
                |_| CleanupOperation { result: 1, ..CleanupOperation::default() },
                || ticks.next(),
            )
            .unwrap();
        assert_eq!(value, 17);
        let error = probe
            .capture_with::<()>(
                CleanupStep::Kill,
                || {
                    calls.set(calls.get() + 1);
                    Err(io::Error::other("original non-OS refusal"))
                },
                |_| panic!("error must not classify success"),
                || ticks.next(),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "original non-OS refusal");
        assert_eq!(calls.get(), 2);
        assert_eq!(probe.operation(CleanupStep::InitialObservation), (1, 1, 0));
        assert_eq!(probe.operation(CleanupStep::Kill), (4, 1, 0));
        assert_eq!(probe.operation(CleanupStep::FinalObservation), (0, 0, 0));
        assert_eq!(probe.operation(CleanupStep::Wait), (0, 0, 0));
        assert_eq!(
            (probe.operations[0].start_qpc, probe.operations[0].end_qpc),
            (10, 13)
        );
        assert_eq!(
            (probe.operations[1].start_qpc, probe.operations[1].end_qpc),
            (20, 29)
        );
    }
}
