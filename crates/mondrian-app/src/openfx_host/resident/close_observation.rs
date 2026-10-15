//! Default-off, fixed Windows close observations. Never an execution authority.
//!
//! The caller creates a private evidence directory before startup. Each session
//! creates one 4096-byte file; no file operation, allocation, flush or extra
//! waiter occurs in a close record. Read/decode only after the original owner
//! has settled. Zero slots are missing; status 2 is a failed clock observation.
//! QPC ticks use one Windows system clock/frequency, never process-local Instant.
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

#[derive(Clone, Debug, Default)]
pub(super) struct Trace {
    #[cfg(windows)]
    mapping: Option<std::sync::Arc<windows::Mapping>>,
}
impl Trace {
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
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

    const BYTES: usize = 4096;
    const MAGIC: [u8; 8] = *b"OFXCLQ01";
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
        _reserved: [u8; 2944],
    }
    const _: () = assert!(std::mem::size_of::<Layout>() == BYTES);
    impl Layout {
        fn new(owner: [u8; 16], nonce: [u8; 16], frequency: i64, parent_pid: u32) -> Self {
            Self {
                header: Header {
                    magic: MAGIC,
                    version: 1,
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
                _reserved: [0; 2944],
            }
        }
        fn valid(&self, owner: &[u8; 16], nonce: &[u8; 16], frequency: i64) -> bool {
            self.header.ready.load(Ordering::Acquire) == 1
                && self.header.magic == MAGIC
                && self.header.version == 1
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
            if (layout.as_ptr() as usize) % std::mem::align_of::<Layout>() != 0 {
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
        pub(super) fn set_child(&self, pid: u32, handle: u64) {
            self.layout().header.process_handle.store(handle, Ordering::Relaxed);
            self.layout().header.child_pid.store(pid, Ordering::Release);
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
            assert_eq!(std::mem::size_of::<Layout>(), 4096);
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
            layout.header.ready.store(1, Ordering::Release);
            assert!(layout.valid(&[1; 16], &[2; 16], 10_000_000));
            assert!(!layout.valid(&[3; 16], &[2; 16], 10_000_000));
            assert!(!layout.valid(&[1; 16], &[3; 16], 10_000_000));
            assert!(!layout.valid(&[1; 16], &[2; 16], 1));
            assert!(!layout.valid(&[1; 16], &[2; 16], 0));
            layout.header.bytes = 4095;
            assert!(!layout.valid(&[1; 16], &[2; 16], 10_000_000));
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
