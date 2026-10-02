//! Bounded Inspector path checks outside the UI and authoring threads.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mondrian_core::{ResourcePathContract, ResourcePathKind, ResourcePathSelection};
use std::sync::mpsc::{sync_channel, Receiver};

/// Private product mode for an isolated, read-only filesystem inspection.
pub const RESOURCE_INSPECTION_WORKER_ARGUMENT: &str = "--internal-resource-inspection-worker";
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const INSPECTION_POLL: Duration = Duration::from_millis(5);
const MAX_RESPONSE_BYTES: u64 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InspectionTicket(uuid::Uuid);
impl InspectionTicket {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathInspectionOutcome {
    File,
    Directory,
    Other,
    Missing,
    Unavailable,
    TimedOut,
    Cancelled,
}
impl PathInspectionOutcome {
    pub(crate) fn validate(self, contract: ResourcePathContract) -> Result<(), &'static str> {
        match self {
            Self::File
                if matches!(
                    contract.kind,
                    ResourcePathKind::File | ResourcePathKind::FileOrDirectory
                ) =>
            {
                Ok(())
            }
            Self::Directory
                if matches!(
                    contract.kind,
                    ResourcePathKind::Directory | ResourcePathKind::FileOrDirectory
                ) =>
            {
                Ok(())
            }
            Self::Missing if contract.selection == ResourcePathSelection::ExistingOrNew => Ok(()),
            Self::File | Self::Directory | Self::Other => Err("resource-path-wrong-kind"),
            Self::Missing => Err("resource-path-missing"),
            Self::TimedOut => Err("resource-path-timeout"),
            Self::Unavailable | Self::Cancelled => Err("resource-path-unavailable"),
        }
    }
    fn wire(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Other => "other",
            Self::Missing => "missing",
            _ => "unavailable",
        }
    }
    fn parse(bytes: &[u8]) -> Self {
        match bytes {
            b"file" => Self::File,
            b"directory" => Self::Directory,
            b"other" => Self::Other,
            b"missing" => Self::Missing,
            _ => Self::Unavailable,
        }
    }
}

pub(crate) fn inspect_local_path(path: &Path) -> PathInspectionOutcome {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => PathInspectionOutcome::File,
        Ok(metadata) if metadata.is_dir() => PathInspectionOutcome::Directory,
        Ok(_) => PathInspectionOutcome::Other,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            PathInspectionOutcome::Missing
        }
        Err(_) => PathInspectionOutcome::Unavailable,
    }
}

/// Invalid invocation or failed publication of the isolated inspection response.
#[derive(Debug, thiserror::Error)]
pub enum ResourceInspectionWorkerError {
    /// The caller must supply exactly one native path argument.
    #[error("invalid resource inspection worker arguments")]
    Arguments,
    /// The bounded response could not be written.
    #[error("resource inspection response failed: {0}")]
    Output(#[from] std::io::Error),
}

/// Inspect one path in the dedicated product process without opening the UI.
pub fn run_resource_inspection_worker() -> Result<(), ResourceInspectionWorkerError> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref()
        != Some(std::ffi::OsStr::new(RESOURCE_INSPECTION_WORKER_ARGUMENT))
    {
        return Err(ResourceInspectionWorkerError::Arguments);
    }
    let path = arguments.next().ok_or(ResourceInspectionWorkerError::Arguments)?;
    if path.is_empty() || arguments.next().is_some() {
        return Err(ResourceInspectionWorkerError::Arguments);
    }
    let outcome = inspect_local_path(Path::new(&path));
    std::io::stdout().lock().write_all(outcome.wire().as_bytes())?;
    Ok(())
}

struct InspectionJob {
    ticket: InspectionTicket,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<PathInspectionOutcome>,
    thread: JoinHandle<()>,
    deadline: Instant,
    reported_timeout: bool,
}

/// One running process and at most one replacement request. No idle worker exists.
pub(crate) struct PathInspectionService {
    helper: PathBuf,
    arguments: Vec<OsString>,
    timeout: Duration,
    running: Option<InspectionJob>,
    replacement: Option<(InspectionTicket, PathBuf, Instant)>,
}
impl PathInspectionService {
    pub(crate) fn new(helper: PathBuf) -> Self {
        Self {
            helper,
            arguments: vec![RESOURCE_INSPECTION_WORKER_ARGUMENT.into()],
            timeout: INSPECTION_TIMEOUT,
            running: None,
            replacement: None,
        }
    }
    pub(crate) fn submit(&mut self, ticket: InspectionTicket, path: PathBuf) {
        if let Some(job) = &self.running {
            job.cancel.store(true, Ordering::Release);
        }
        self.replacement = Some((ticket, path, Instant::now() + self.timeout));
    }
    pub(crate) fn cancel_except(&mut self, ticket: Option<InspectionTicket>) {
        if let Some(job) = &self.running
            && Some(job.ticket) != ticket
        {
            job.cancel.store(true, Ordering::Release);
        }
        if self.replacement.as_ref().is_some_and(|(id, _, _)| Some(*id) != ticket) {
            self.replacement = None;
        }
    }
    pub(crate) fn has_work(&self) -> bool {
        self.running.is_some() || self.replacement.is_some()
    }
    pub(crate) fn poll(&mut self) -> Option<(InspectionTicket, PathInspectionOutcome)> {
        if self
            .replacement
            .as_ref()
            .is_some_and(|(_, _, deadline)| Instant::now() >= *deadline)
        {
            let (ticket, _, _) = self.replacement.take()?;
            return Some((ticket, PathInspectionOutcome::TimedOut));
        }
        if let Some(job) = self.running.as_mut()
            && !job.reported_timeout
            && Instant::now() >= job.deadline
        {
            job.reported_timeout = true;
            job.cancel.store(true, Ordering::Release);
            return Some((job.ticket, PathInspectionOutcome::TimedOut));
        }
        let completed = if self.running.as_ref().is_some_and(|job| job.thread.is_finished()) {
            let job = self.running.take()?;
            let outcome = job.receiver.try_recv().unwrap_or(PathInspectionOutcome::Unavailable);
            let outcome = if job.thread.join().is_ok() {
                outcome
            } else {
                PathInspectionOutcome::Unavailable
            };
            Some((job.ticket, outcome))
        } else {
            None
        };
        if self.running.is_none()
            && let Some((ticket, path, deadline)) = self.replacement.take()
        {
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancel);
            let helper = self.helper.clone();
            let arguments = self.arguments.clone();
            let timeout = self.timeout;
            let (sender, receiver) = sync_channel(1);
            match thread::Builder::new().name("resource-path-inspection".into()).spawn(move || {
                let outcome =
                    supervise_inspection(&helper, &arguments, &path, &worker_cancel, timeout);
                let _ = sender.try_send(outcome);
            }) {
                Ok(thread) => {
                    self.running = Some(InspectionJob {
                        ticket,
                        cancel,
                        receiver,
                        thread,
                        deadline,
                        reported_timeout: false,
                    })
                }
                Err(_) => return Some((ticket, PathInspectionOutcome::Unavailable)),
            }
        }
        completed
    }
}
impl Drop for PathInspectionService {
    fn drop(&mut self) {
        if let Some(job) = self.running.take() {
            job.cancel.store(true, Ordering::Release);
            // Never join a still-running OS operation on the UI thread. The
            // supervisor retains and terminates its child before it returns.
            if job.thread.is_finished() {
                let _ = job.thread.join();
            }
        }
    }
}

fn supervise_inspection(
    helper: &Path,
    arguments: &[OsString],
    path: &Path,
    cancel: &AtomicBool,
    timeout: Duration,
) -> PathInspectionOutcome {
    if cancel.load(Ordering::Acquire) {
        return PathInspectionOutcome::Cancelled;
    }
    let mut command = Command::new(helper);
    command
        .args(arguments)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let deadline = Instant::now() + timeout;
    let Ok(mut child) = command.spawn() else {
        return PathInspectionOutcome::Unavailable;
    };
    let disposition = loop {
        if cancel.load(Ordering::Acquire) {
            break PathInspectionOutcome::Cancelled;
        }
        if Instant::now() >= deadline {
            break PathInspectionOutcome::TimedOut;
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut bytes = Vec::new();
                return if child.stdout.take().is_some_and(|stdout| {
                    stdout.take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut bytes).is_ok()
                }) && bytes.len() as u64 <= MAX_RESPONSE_BYTES
                {
                    PathInspectionOutcome::parse(&bytes)
                } else {
                    PathInspectionOutcome::Unavailable
                };
            }
            Ok(Some(_)) | Err(_) => break PathInspectionOutcome::Unavailable,
            Ok(None) => thread::sleep(INSPECTION_POLL),
        }
    };
    let _ = child.kill();
    let _ = child.wait(); // Supervisor only; never on the UI thread.
    disposition
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_inspection_reports_shape_and_applies_selection_policy() {
        let temp = tempfile::tempdir().expect("paths");
        let file = temp.path().join("输入🙂.cube");
        std::fs::write(&file, b"input").expect("file");
        let missing = temp.path().join("new").join("输出.cube");
        assert_eq!(inspect_local_path(&file), PathInspectionOutcome::File);
        assert_eq!(
            inspect_local_path(temp.path()),
            PathInspectionOutcome::Directory
        );
        assert_eq!(inspect_local_path(&missing), PathInspectionOutcome::Missing);
        for kind in [
            ResourcePathKind::File,
            ResourcePathKind::Directory,
            ResourcePathKind::FileOrDirectory,
        ] {
            for selection in [
                ResourcePathSelection::Existing,
                ResourcePathSelection::ExistingOrNew,
            ] {
                let contract = ResourcePathContract { kind, selection, max_utf8_bytes: None };
                assert_eq!(
                    PathInspectionOutcome::Missing.validate(contract).is_ok(),
                    selection == ResourcePathSelection::ExistingOrNew
                );
                assert_eq!(
                    PathInspectionOutcome::File.validate(contract).is_ok(),
                    kind != ResourcePathKind::Directory
                );
                assert_eq!(
                    PathInspectionOutcome::Directory.validate(contract).is_ok(),
                    kind != ResourcePathKind::File
                );
                assert!(PathInspectionOutcome::Other.validate(contract).is_err());
                assert!(PathInspectionOutcome::TimedOut.validate(contract).is_err());
            }
        }
        assert!(!missing.parent().expect("parent").exists());
    }
    #[test]
    fn response_rejects_partial_extra_and_unknown_payloads() {
        assert_eq!(
            PathInspectionOutcome::parse(b"directory"),
            PathInspectionOutcome::Directory
        );
        for payload in [
            b"file\n".as_slice(),
            b"file junk",
            b"",
            b"timeout",
            b"files",
        ] {
            assert_eq!(
                PathInspectionOutcome::parse(payload),
                PathInspectionOutcome::Unavailable
            );
        }
    }
    #[test]
    fn replacement_and_cancellation_are_bounded_before_spawn() {
        let mut service = PathInspectionService::new(PathBuf::from("unused"));
        let first = InspectionTicket::new();
        let second = InspectionTicket::new();
        service.submit(first, "one".into());
        service.submit(second, "two".into());
        assert_eq!(service.replacement.as_ref().expect("replacement").0, second);
        service.cancel_except(Some(first));
        assert!(!service.has_work());
        assert!(service.poll().is_none());
    }

    fn python_service(script: &str) -> PathInspectionService {
        let helper = std::env::var_os("MONDRIAN_RESOURCE_INSPECTION_TEST_PYTHON")
            .expect("Python test helper");
        let mut service = PathInspectionService::new(helper.into());
        service.arguments = vec!["-c".into(), script.into()];
        service
    }
    fn wait_result(
        service: &mut PathInspectionService,
    ) -> (InspectionTicket, PathInspectionOutcome) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(outcome) = service.poll() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "inspection did not finish");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    #[ignore = "requires MONDRIAN_RESOURCE_INSPECTION_TEST_PYTHON"]
    fn supervised_process_cancellation_timeout_and_retry_reap_the_owned_child() {
        let temp = tempfile::tempdir().expect("markers");
        let script = "import sys,os,time;from pathlib import Path;Path(sys.argv[1]).write_text(str(os.getpid()));time.sleep(60)";
        for timeout in [false, true] {
            let marker = temp.path().join(if timeout { "timeout" } else { "cancel" });
            let mut service = python_service(script);
            let ticket = InspectionTicket::new();
            service.submit(ticket, marker.clone());
            assert!(service.poll().is_none());
            let deadline = Instant::now() + Duration::from_secs(4);
            while !marker.exists() {
                assert!(Instant::now() < deadline, "child did not start");
                thread::sleep(Duration::from_millis(5));
            }
            if timeout {
                service.running.as_mut().expect("running").deadline = Instant::now();
            } else {
                service.cancel_except(None);
            }
            let (id, outcome) = wait_result(&mut service);
            assert_eq!(id, ticket);
            assert_eq!(
                outcome,
                if timeout {
                    PathInspectionOutcome::TimedOut
                } else {
                    PathInspectionOutcome::Cancelled
                }
            );
            let deadline = Instant::now() + Duration::from_secs(3);
            while service.has_work() {
                service.poll();
                assert!(Instant::now() < deadline, "cancelled child was not reaped");
                thread::sleep(Duration::from_millis(5));
            }
            service.arguments = vec!["-c".into(), "import sys;sys.stdout.write('file')".into()];
            let next = InspectionTicket::new();
            service.submit(next, marker);
            assert_eq!(
                wait_result(&mut service),
                (next, PathInspectionOutcome::File)
            );
            assert!(!service.has_work());
        }
    }
    #[test]
    #[ignore = "requires MONDRIAN_RESOURCE_INSPECTION_TEST_PYTHON"]
    fn failed_helper_responses_are_bounded_and_leave_the_service_reusable() {
        let mut service = python_service("import sys;sys.stdout.write('file' * 1000)");
        let first = InspectionTicket::new();
        service.submit(first, "unused".into());
        assert_eq!(
            wait_result(&mut service),
            (first, PathInspectionOutcome::Unavailable)
        );
        service.arguments = vec![
            "-c".into(),
            "import sys;sys.stdout.write('directory')".into(),
        ];
        let next = InspectionTicket::new();
        service.submit(next, "unused".into());
        assert_eq!(
            wait_result(&mut service),
            (next, PathInspectionOutcome::Directory)
        );
    }

    #[test]
    #[ignore = "requires a rebuilt MONDRIAN_RESOURCE_INSPECTION_HELPER product executable"]
    fn actual_product_helper_preserves_native_paths_and_reports_local_objects() {
        let helper =
            std::env::var_os("MONDRIAN_RESOURCE_INSPECTION_HELPER").expect("product helper");
        let mut service = PathInspectionService::new(helper.into());
        let temp = tempfile::tempdir().expect("paths");
        let file = temp.path().join("已有🙂.cube");
        std::fs::write(&file, b"original").expect("input");
        let missing = temp.path().join("未创建").join("输出🙂.cube");
        for (path, expected) in [
            (file.clone(), PathInspectionOutcome::File),
            (temp.path().to_owned(), PathInspectionOutcome::Directory),
            (missing.clone(), PathInspectionOutcome::Missing),
        ] {
            let id = InspectionTicket::new();
            service.submit(id, path);
            assert_eq!(wait_result(&mut service), (id, expected));
        }
        assert_eq!(std::fs::read(&file).expect("input"), b"original");
        assert!(!missing.parent().expect("parent").exists());
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let path = temp.path().join(OsString::from_wide(&[0xd800]));
            let id = InspectionTicket::new();
            service.submit(id, path.clone());
            assert_eq!(wait_result(&mut service), (id, inspect_local_path(&path)));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let path = temp.path().join(OsString::from_vec(vec![0xff]));
            std::fs::write(&path, b"native").expect("native filename");
            let id = InspectionTicket::new();
            service.submit(id, path);
            assert_eq!(wait_result(&mut service), (id, PathInspectionOutcome::File));
        }
    }
}
