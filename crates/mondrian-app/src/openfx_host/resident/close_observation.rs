//! Default-off, fixed Windows close observations. Never an execution authority.
//!
//! The caller creates a private evidence directory before startup. Each session
//! creates one 4096-byte file; no file operation, allocation, flush or extra
//! waiter occurs in a close record. Read/decode only after the original owner
//! has settled. Zero slots are missing; status 2 is a failed clock observation.
//! QPC ticks use one Windows system clock/frequency, never process-local Instant.
//! Version 3 adds one parent-only query summary; all prior offsets stay fixed. UTC
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

#[derive(Clone, Debug, Default)]
pub(super) struct Trace {
    #[cfg(windows)]
    mapping: Option<std::sync::Arc<windows::Mapping>>,
}
impl Trace {
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
    const MAGIC: [u8; 8] = *b"OFXCLQ03";
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
        _reserved: [u8; 2816],
    }
    const _: () = {
        assert!(std::mem::size_of::<Header>() == 128);
        assert!(std::mem::size_of::<Record>() == 64);
        assert!(std::mem::size_of::<ExitObservation>() == 64);
        assert!(std::mem::offset_of!(Layout, records) == 128);
        assert!(std::mem::offset_of!(Layout, exit_observation) == 1152);
        assert!(std::mem::size_of::<ExitQueryObservation>() == 64);
        assert!(std::mem::offset_of!(Layout, exit_queries) == 1216);
        assert!(std::mem::offset_of!(Layout, _reserved) == 1280);
        assert!(std::mem::size_of::<Layout>() == BYTES);
    };
    impl Layout {
        fn new(owner: [u8; 16], nonce: [u8; 16], frequency: i64, parent_pid: u32) -> Self {
            Self {
                header: Header {
                    magic: MAGIC,
                    version: 3,
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
                _reserved: [0; 2816],
            }
        }
        fn valid(&self, owner: &[u8; 16], nonce: &[u8; 16], frequency: i64) -> bool {
            self.header.ready.load(Ordering::Acquire) == 1
                && self.header.magic == MAGIC
                && self.header.version == 3
                && self.header.bytes == BYTES as u32
                && frequency > 0
                && self.header.frequency == frequency
                && self.header.parent_pid != 0
                && self.header.owner == *owner
                && self.header.nonce == *nonce
        }
    }

    #[derive(Debug)]
    pub(super) struct Mapping {
        // Retained for every reference/Arc, including outer inner-return mark.
        _mapping: memmap2::MmapMut,
        _file: File,
        layout: NonNull<Layout>,
        pid: u32,
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
            Ok(Self { _mapping: mapping, _file: file, layout, pid })
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
            let mapping = Self::from_file(file)?;
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
            let mapping = Self::from_file(file)?;
            if !mapping.layout().valid(owner.0.as_bytes(), &nonce_bytes(nonce)?, frequency()?) {
                return Err(invalid(
                    "OpenFX close observation identity/layout/clock mismatch",
                ));
            }
            Ok(mapping)
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
            assert_eq!(std::mem::offset_of!(Layout, _reserved), 1280);
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
            layout.header.version = 3;
            layout.header.magic = *b"OFXCLQ01";
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
            layout.header.magic = MAGIC;
            layout.header.bytes = 4095;
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
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
                    "schema_version": 3,
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
